// In-memory stand-ins for the vault, the saved sync state and the server.

import { ServerError } from "../../src/engine/errors";
import { sha256Hex } from "../../src/engine/hash";
import { pathKey } from "../../src/engine/paths";
import type {
  ChangesPage,
  CommitChange,
  CommitResult,
  FileStat,
  PathConflict,
  RemoteChange,
  RemoteState,
  ServerApi,
  StateStore,
  SyncState,
  VaultFs,
} from "../../src/engine/types";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export class MemoryFs implements VaultFs {
  private readonly files = new Map<string, { data: Uint8Array; mtime: number }>();
  private clock = 1000;
  readonly trashed: string[] = [];
  readonly renames: { from: string; to: string }[] = [];

  // Helpers that act like the user editing the vault.

  setText(path: string, text: string): void {
    this.setBytes(path, encoder.encode(text));
  }

  setBytes(path: string, data: Uint8Array): void {
    this.files.set(path, { data: data.slice(), mtime: ++this.clock });
  }

  text(path: string): string | undefined {
    const file = this.files.get(path);
    return file && decoder.decode(file.data);
  }

  has(path: string): boolean {
    return this.files.has(path);
  }

  remove(path: string): void {
    this.files.delete(path);
  }

  move(from: string, to: string): void {
    const file = this.files.get(from);
    if (!file) throw new Error(`no such file: ${from}`);
    this.files.delete(from);
    this.files.set(to, file);
  }

  paths(): string[] {
    return [...this.files.keys()].sort();
  }

  /** Every file's content, for comparing devices. */
  snapshot(): Record<string, string> {
    const result: Record<string, string> = {};
    for (const path of this.paths()) result[path] = decoder.decode(this.files.get(path)!.data);
    return result;
  }

  // VaultFs

  async list(): Promise<FileStat[]> {
    return [...this.files].map(([path, f]) => ({ path, size: f.data.length, mtime: f.mtime }));
  }

  async stat(path: string): Promise<FileStat | null> {
    const file = this.files.get(path);
    return file ? { path, size: file.data.length, mtime: file.mtime } : null;
  }

  async read(path: string): Promise<Uint8Array> {
    const file = this.files.get(path);
    if (!file) throw new Error(`no such file: ${path}`);
    return file.data.slice();
  }

  async write(path: string, data: Uint8Array): Promise<void> {
    this.setBytes(path, data);
  }

  async trash(path: string): Promise<void> {
    if (!this.files.delete(path)) throw new Error(`no such file: ${path}`);
    this.trashed.push(path);
  }

  async rename(from: string, to: string): Promise<void> {
    if (this.files.has(to)) throw new Error(`already exists: ${to}`);
    this.move(from, to);
    this.renames.push({ from, to });
  }
}

export class MemoryStateStore implements StateStore {
  saved: SyncState | null = null;

  async load(): Promise<SyncState | null> {
    return this.saved && structuredClone(this.saved);
  }

  async save(state: SyncState): Promise<void> {
    this.saved = structuredClone(state);
  }
}

/** A single-vault server with the same commit rules as the real one. */
export class MemoryServer implements ServerApi {
  private readonly blobs = new Map<string, Uint8Array>();
  private readonly log: RemoteChange[] = [];
  private readonly current = new Map<string, { hash: string; size: number; seq: number }>();
  private head = 0;

  async changes(_vaultId: number, since: number, limit = 500): Promise<ChangesPage> {
    const after = this.log.filter((c) => c.seq > since);
    return { changes: after.slice(0, limit), head_seq: this.head, more: after.length > limit };
  }

  async state(): Promise<RemoteState> {
    return {
      head_seq: this.head,
      files: [...this.current].map(([path, f]) => ({ path, ...f })),
    };
  }

  async missing(_vaultId: number, hashes: string[]): Promise<string[]> {
    return [...new Set(hashes)].filter((h) => !this.blobs.has(h));
  }

  async upload(_vaultId: number, hash: string, data: Uint8Array): Promise<void> {
    if ((await sha256Hex(data)) !== hash) {
      throw new ServerError(400, "hash_mismatch", "content does not match its hash");
    }
    this.blobs.set(hash, data.slice());
  }

  async download(_vaultId: number, hash: string): Promise<Uint8Array> {
    const data = this.blobs.get(hash);
    if (!data) throw new ServerError(404, "not_found", "not found");
    return data.slice();
  }

  async commit(_vaultId: number, changes: CommitChange[]): Promise<CommitResult> {
    const ordered = [...changes].sort((a, b) => Number(a.op !== "delete") - Number(b.op !== "delete"));
    const conflicts: PathConflict[] = [];
    for (const change of ordered) {
      if (change.op === "upsert" && !this.blobs.has(change.hash!)) {
        throw new ServerError(400, "missing_blob", `content ${change.hash} has not been uploaded`);
      }
      const now = this.current.get(change.path);
      const upToDate = now ? now.seq === change.expect_seq : change.expect_seq === null;
      if (!upToDate) {
        const last = [...this.log].reverse().find((c) => c.path === change.path);
        conflicts.push({ path: change.path, current_hash: now?.hash ?? null, current_seq: last?.seq ?? null });
        continue;
      }
      if (change.op === "upsert" && !now) {
        const other = [...this.current.keys()].find(
          (p) => p !== change.path && pathKey(p) === pathKey(change.path),
        );
        if (other && !ordered.some((c) => c.op === "delete" && c.path === other)) {
          throw new ServerError(409, "case_conflict", `path differs only by letter case from ${other}`);
        }
      }
    }
    if (conflicts.length > 0) {
      throw new ServerError(409, "conflict", "some paths changed on the server", conflicts);
    }
    const applied = ordered.map((change) => {
      const seq = ++this.head;
      if (change.op === "delete") {
        this.current.delete(change.path);
        this.log.push({ seq, path: change.path, op: "delete", hash: null, size: null, created_at: 0 });
      } else {
        const size = this.blobs.get(change.hash!)!.length;
        this.current.set(change.path, { hash: change.hash!, size, seq });
        this.log.push({ seq, path: change.path, op: "upsert", hash: change.hash!, size, created_at: 0 });
      }
      return { path: change.path, seq };
    });
    return { head_seq: this.head, applied };
  }

  /** Current server paths, for assertions. */
  paths(): string[] {
    return [...this.current.keys()].sort();
  }
}
