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
  StagingFs,
  StateStore,
  SyncState,
  UploadProgress,
  VaultFs,
} from "../../src/engine/types";

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export class MemoryFs implements VaultFs {
  private readonly files = new Map<string, { data: Uint8Array; mtime: number }>();
  private clock = 1000;
  readonly trashed: string[] = [];
  readonly renames: { from: string; to: string }[] = [];
  /** Scratch files, by name. */
  readonly scratch = new Map<string, Uint8Array>();
  /** Largest single write or append, to check that big files go in pieces. */
  largestWrite = 0;

  readonly staging: StagingFs = {
    size: async (name) => this.scratch.get(name)?.length ?? null,
    append: async (name, data) => {
      this.largestWrite = Math.max(this.largestWrite, data.length);
      const old = this.scratch.get(name) ?? new Uint8Array();
      const joined = new Uint8Array(old.length + data.length);
      joined.set(old);
      joined.set(data, old.length);
      this.scratch.set(name, joined);
    },
    readText: async (name) => {
      const data = this.scratch.get(name);
      return data ? decoder.decode(data) : null;
    },
    writeText: async (name, text) => {
      this.scratch.set(name, encoder.encode(text));
    },
    remove: async (name) => {
      this.scratch.delete(name);
    },
    list: async () => [...this.scratch.keys()],
    moveIntoVault: async (name, path) => {
      const data = this.scratch.get(name);
      if (!data) throw new Error(`no such scratch file: ${name}`);
      this.scratch.delete(name);
      this.files.set(path, { data, mtime: ++this.clock });
    },
  };

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

  /** Acts like Windows: names that differ only by letter case are the same file. */
  caseInsensitive = false;

  private onDisk(path: string): string | undefined {
    if (this.files.has(path)) return path;
    if (!this.caseInsensitive) return undefined;
    return [...this.files.keys()].find((p) => pathKey(p) === pathKey(path));
  }

  async stat(path: string): Promise<FileStat | null> {
    const actual = this.onDisk(path);
    const file = actual === undefined ? undefined : this.files.get(actual);
    return file ? { path, size: file.data.length, mtime: file.mtime } : null;
  }

  async read(path: string): Promise<Uint8Array> {
    const file = this.files.get(path);
    if (!file) throw new Error(`no such file: ${path}`);
    return file.data.slice();
  }

  async write(path: string, data: Uint8Array): Promise<void> {
    this.largestWrite = Math.max(this.largestWrite, data.length);
    this.setBytes(path, data);
  }

  async trash(path: string): Promise<void> {
    if (!this.files.delete(path)) throw new Error(`no such file: ${path}`);
    this.trashed.push(path);
  }

  async rename(from: string, to: string): Promise<void> {
    const existing = this.onDisk(to);
    if (existing !== undefined && existing !== from) throw new Error(`already exists: ${to}`);
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
  /** Unfinished uploads sent in pieces. */
  private readonly parts = new Map<string, Uint8Array>();
  /** Acts like a server before 0.5.0: no ranges, no uploads in pieces. */
  legacy = false;
  /** Largest single request or response body, to check that big files go in pieces. */
  largestTransfer = 0;
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
    this.largestTransfer = Math.max(this.largestTransfer, data.length);
    this.blobs.set(hash, data.slice());
  }

  async download(_vaultId: number, hash: string): Promise<Uint8Array> {
    const data = this.blobs.get(hash);
    if (!data) throw new ServerError(404, "not_found", "not found");
    this.largestTransfer = Math.max(this.largestTransfer, data.length);
    return data.slice();
  }

  async downloadRange(
    vaultId: number,
    hash: string,
    offset: number,
    length: number,
  ): Promise<{ data: Uint8Array; whole: boolean }> {
    if (this.legacy) return { data: await this.download(vaultId, hash), whole: true };
    const data = this.blobs.get(hash);
    if (!data) throw new ServerError(404, "not_found", "not found");
    if (offset >= data.length) throw new ServerError(416, "http_416", "range not satisfiable");
    const piece = data.slice(offset, offset + length);
    this.largestTransfer = Math.max(this.largestTransfer, piece.length);
    return { data: piece, whole: false };
  }

  async uploadProgress(_vaultId: number, hash: string): Promise<UploadProgress | null> {
    if (this.legacy) return null;
    const stored = this.blobs.get(hash);
    if (stored) return { received: stored.length, stored: true };
    return { received: this.parts.get(hash)?.length ?? 0, stored: false };
  }

  async uploadPart(
    _vaultId: number,
    hash: string,
    offset: number,
    total: number,
    data: Uint8Array,
  ): Promise<UploadProgress> {
    if (this.legacy) throw new ServerError(404, "http_404", "The server returned HTTP 404.");
    if (this.blobs.has(hash)) return { received: total, stored: true };
    this.largestTransfer = Math.max(this.largestTransfer, data.length);
    const old = this.parts.get(hash) ?? new Uint8Array();
    if (offset !== old.length) {
      throw new ServerError(409, "wrong_offset", `the upload has ${old.length} bytes so far`);
    }
    const joined = new Uint8Array(old.length + data.length);
    joined.set(old);
    joined.set(data, old.length);
    if (joined.length < total) {
      this.parts.set(hash, joined);
      return { received: joined.length, stored: false };
    }
    this.parts.delete(hash);
    if ((await sha256Hex(joined)) !== hash) {
      throw new ServerError(400, "hash_mismatch", "content does not match its hash");
    }
    this.blobs.set(hash, joined);
    return { received: joined.length, stored: true };
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
