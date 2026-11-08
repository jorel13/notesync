import { isRetryable, MassDeleteError, ServerError } from "./errors";
import { sha256Hex } from "./hash";
import { decodeText, encodeText, mergeText } from "./merge";
import {
  ALWAYS_EXCLUDED,
  conflictCopyPath,
  extension,
  isExcluded,
  pathKey,
  pathProblem,
} from "./paths";
import {
  emptyReport,
  type ChangeOp,
  type ChangesPage,
  type CommitChange,
  type FileStat,
  type ServerApi,
  type StateStore,
  type SyncedFile,
  type SyncReport,
  type SyncState,
  type VaultFs,
} from "./types";

/** Pause when one sync would delete more than `maxCount` files, or more than
 * `maxFraction` of the vault once at least `minCount` files are involved. */
export interface MassDeleteLimit {
  maxCount: number;
  maxFraction: number;
  minCount: number;
}

export interface SyncOptions {
  vaultId: number;
  /** Used in conflict copy names. */
  deviceName: string;
  /** Extra paths to leave out: folders end with "/", anything else is an exact file. */
  excludedPaths?: string[];
  maxFileBytes?: number;
  /** Extensions merged line by line; everything else gets conflict copies. */
  textExtensions?: string[];
  massDelete?: MassDeleteLimit;
  now?: () => Date;
}

export interface SyncRunOptions {
  /** Set after the user confirms a large delete. */
  allowMassDelete?: boolean;
}

export const DEFAULT_MAX_FILE_BYTES = 100 * 1024 * 1024;
export const DEFAULT_TEXT_EXTENSIONS = ["md", "txt"];
export const DEFAULT_MASS_DELETE: MassDeleteLimit = { maxCount: 25, maxFraction: 0.1, minCount: 5 };

const MAX_ATTEMPTS = 3;
const PAGE_SIZE = 500;
const COMMIT_CHUNK = 500;
const HASH_CHUNK = 1000;

interface LocalFile {
  hash: string;
  size: number;
  mtime: number;
}

interface Scan {
  files: Map<string, LocalFile>;
  /** Files that can't be synced (bad name, too large). Never uploaded, deleted or overwritten. */
  skipped: Set<string>;
}

interface RemoteEntry {
  op: ChangeOp;
  hash: string | null;
  seq: number;
}

interface Pulled {
  head: number;
  /** Latest server change for each path changed since the last sync. */
  entries: Map<string, RemoteEntry>;
}

type Action =
  | { kind: "forget"; path: string }
  | { kind: "adopt"; path: string; hash: string; seq: number }
  | { kind: "download"; path: string; hash: string; seq: number }
  | { kind: "trash"; path: string }
  | { kind: "rename"; from: string; to: string; hash: string; seq: number }
  | { kind: "keepLocal"; path: string }
  | {
      kind: "merge";
      path: string;
      baseHash: string | null;
      remoteHash: string;
      seq: number;
    };

/**
 * Syncs one local vault with one server vault.
 *
 * Each run pulls server changes and applies them locally (merging where both
 * sides changed), then pushes local changes in all-or-nothing commits. If the
 * server reports that something changed in between, the run starts over.
 */
export class SyncEngine {
  private readonly excluded: string[];
  private readonly maxFileBytes: number;
  private readonly textExtensions: Set<string>;
  private readonly massDelete: MassDeleteLimit;
  private readonly now: () => Date;
  private current: Promise<SyncReport> | null = null;
  private queued: Promise<SyncReport> | null = null;

  constructor(
    private readonly fs: VaultFs,
    private readonly api: ServerApi,
    private readonly store: StateStore,
    private readonly options: SyncOptions,
  ) {
    this.excluded = [...ALWAYS_EXCLUDED, ...(options.excludedPaths ?? [])];
    this.maxFileBytes = options.maxFileBytes ?? DEFAULT_MAX_FILE_BYTES;
    this.textExtensions = new Set(options.textExtensions ?? DEFAULT_TEXT_EXTENSIONS);
    this.massDelete = options.massDelete ?? DEFAULT_MASS_DELETE;
    this.now = options.now ?? (() => new Date());
  }

  /**
   * Runs a sync. Calls made while one is running share a single follow-up
   * run, so syncs never overlap.
   */
  sync(run: SyncRunOptions = {}): Promise<SyncReport> {
    if (!this.current) {
      this.current = this.runWithRetries(run).finally(() => {
        this.current = null;
      });
      return this.current;
    }
    if (!this.queued) {
      this.queued = this.current
        .catch(() => undefined)
        .then(() => {
          this.queued = null;
          return this.sync(run);
        });
    }
    return this.queued;
  }

  private async runWithRetries(run: SyncRunOptions): Promise<SyncReport> {
    const report = emptyReport();
    for (let attempt = 1; ; attempt++) {
      try {
        await this.attempt(report, run);
        return report;
      } catch (err) {
        if (attempt >= MAX_ATTEMPTS || !isRetryable(err)) throw err;
      }
    }
  }

  private async attempt(report: SyncReport, run: SyncRunOptions): Promise<void> {
    const state = (await this.store.load()) ?? {
      vaultId: this.options.vaultId,
      lastSeq: 0,
      files: {},
    };
    if (state.vaultId !== this.options.vaultId) {
      throw new Error("The saved sync state belongs to a different server vault.");
    }

    // Phase 1: bring server changes to this device.
    const pulled = await this.pull(state.lastSeq);
    let scan = await this.scan(state, report);
    const actions = this.plan(state, scan, pulled);
    const trashCount = actions.filter((a) => a.kind === "trash").length;
    this.checkMassDelete(trashCount, "local", scan.files.size, run);
    const complete = await this.applyRemote(actions, state, scan, report);
    // Only skip past changes that were fully applied; anything left over is
    // pulled again next time.
    if (complete) state.lastSeq = Math.max(state.lastSeq, pulled.head);
    await this.store.save(state);

    // Phase 2: send this device's changes.
    scan = await this.scan(state, report);
    await this.push(state, scan, report, run);
  }

  // ---- Pull ----

  private async pull(since: number): Promise<Pulled> {
    const { vaultId } = this.options;
    const entries = new Map<string, RemoteEntry>();
    if (since === 0) {
      // First sync: the current state is all that matters, not the history.
      const remote = await this.api.state(vaultId);
      for (const file of remote.files) {
        entries.set(file.path, { op: "upsert", hash: file.hash, seq: file.seq });
      }
      return { head: remote.head_seq, entries };
    }
    let cursor = since;
    let page: ChangesPage;
    do {
      page = await this.api.changes(vaultId, cursor, PAGE_SIZE);
      for (const change of page.changes) {
        entries.set(change.path, { op: change.op, hash: change.hash, seq: change.seq });
        cursor = change.seq;
      }
    } while (page.more);
    return { head: page.head_seq, entries };
  }

  // ---- Local scan ----

  private async scan(state: SyncState, report: SyncReport): Promise<Scan> {
    const files = new Map<string, LocalFile>();
    const skipped = new Set<string>();
    for (const stat of await this.fs.list()) {
      const { path } = stat;
      if (isExcluded(path, this.excluded)) continue;
      const problem =
        pathProblem(path) ??
        (stat.size > this.maxFileBytes
          ? `larger than the ${Math.round(this.maxFileBytes / (1024 * 1024))} MB limit`
          : null);
      if (problem) {
        skipped.add(path);
        addSkip(report, path, problem);
        continue;
      }
      files.set(path, {
        hash: await this.hashOf(stat, state.files[path]),
        size: stat.size,
        mtime: stat.mtime,
      });
    }
    return { files, skipped };
  }

  /** Reuses the saved hash when size and modified time are unchanged. */
  private async hashOf(stat: FileStat, known: SyncedFile | undefined): Promise<string> {
    if (known && known.mtime === stat.mtime && known.size === stat.size) return known.hash;
    return sha256Hex(await this.fs.read(stat.path));
  }

  private ignored(path: string, scan: Scan): boolean {
    return isExcluded(path, this.excluded) || scan.skipped.has(path);
  }

  // ---- Plan and apply server changes ----

  private plan(state: SyncState, scan: Scan, pulled: Pulled): Action[] {
    const actions: Action[] = [];
    for (const [path, remote] of pulled.entries) {
      if (this.ignored(path, scan)) continue;
      const base = state.files[path];
      // Already known, e.g. this device's own commit coming back.
      if (base && base.seq === remote.seq) continue;

      const localHash = scan.files.get(path)?.hash ?? null;
      const baseHash = base?.hash ?? null;
      const remoteHash = remote.op === "upsert" ? remote.hash : null;

      if (localHash === remoteHash) {
        actions.push(
          remoteHash === null
            ? { kind: "forget", path }
            : { kind: "adopt", path, hash: remoteHash, seq: remote.seq },
        );
      } else if (localHash === baseHash) {
        // Only the server changed.
        actions.push(
          remoteHash === null
            ? { kind: "trash", path }
            : { kind: "download", path, hash: remoteHash, seq: remote.seq },
        );
      } else if (localHash === null) {
        // Deleted here, edited on the server: the edit wins.
        actions.push({ kind: "download", path, hash: remoteHash as string, seq: remote.seq });
      } else if (remoteHash === null) {
        // Edited here, deleted on the server: the edit wins and is uploaded as a new file.
        actions.push({ kind: "keepLocal", path });
      } else {
        actions.push({ kind: "merge", path, baseHash, remoteHash, seq: remote.seq });
      }
    }
    return this.detectRenames(actions, scan);
  }

  /** A trash plus a download of the same content becomes a local rename. */
  private detectRenames(actions: Action[], scan: Scan): Action[] {
    const trashedByHash = new Map<string, string>();
    for (const action of actions) {
      if (action.kind !== "trash") continue;
      const hash = scan.files.get(action.path)?.hash;
      if (hash && !trashedByHash.has(hash)) trashedByHash.set(hash, action.path);
    }
    if (trashedByHash.size === 0) return actions;

    const renamedFrom = new Set<string>();
    const result: Action[] = [];
    for (const action of actions) {
      if (action.kind === "download" && !scan.files.has(action.path)) {
        const from = trashedByHash.get(action.hash);
        if (from !== undefined && !renamedFrom.has(from)) {
          renamedFrom.add(from);
          result.push({ kind: "rename", from, to: action.path, hash: action.hash, seq: action.seq });
          continue;
        }
      }
      result.push(action);
    }
    return result.filter((a) => !(a.kind === "trash" && renamedFrom.has(a.path)));
  }

  /** Returns false if some change was skipped because the file was edited mid-sync. */
  private async applyRemote(
    actions: Action[],
    state: SyncState,
    scan: Scan,
    report: SyncReport,
  ): Promise<boolean> {
    let complete = true;
    for (const action of actions) {
      if (!(await this.applyAction(action, state, scan, report))) complete = false;
    }
    return complete;
  }

  private async applyAction(
    action: Action,
    state: SyncState,
    scan: Scan,
    report: SyncReport,
  ): Promise<boolean> {
    switch (action.kind) {
      case "forget":
      case "keepLocal":
        delete state.files[action.path];
        return true;

      case "adopt": {
        const local = scan.files.get(action.path) as LocalFile;
        state.files[action.path] = {
          hash: action.hash,
          seq: action.seq,
          mtime: local.mtime,
          size: local.size,
        };
        return true;
      }

      case "download": {
        const data = await this.fetchBlob(action.hash);
        if (!(await this.unchangedSinceScan(action.path, scan))) return false;
        await this.fs.write(action.path, data);
        await this.record(state, action.path, action.hash, action.seq, action.hash);
        add(report.downloaded, action.path);
        return true;
      }

      case "trash": {
        if (!(await this.unchangedSinceScan(action.path, scan))) return false;
        await this.fs.trash(action.path);
        delete state.files[action.path];
        add(report.deletedLocally, action.path);
        return true;
      }

      case "rename": {
        if (!(await this.unchangedSinceScan(action.from, scan))) return false;
        if (await this.fs.stat(action.to)) return false;
        await this.fs.rename(action.from, action.to);
        delete state.files[action.from];
        await this.record(state, action.to, action.hash, action.seq, action.hash);
        report.renamedLocally.push({ from: action.from, to: action.to });
        return true;
      }

      case "merge":
        return this.merge(action, state, scan, report);
    }
  }

  private async merge(
    action: Extract<Action, { kind: "merge" }>,
    state: SyncState,
    scan: Scan,
    report: SyncReport,
  ): Promise<boolean> {
    const { path } = action;
    const remoteData = await this.fetchBlob(action.remoteHash);
    if (!(await this.unchangedSinceScan(path, scan))) return false;
    const localData = await this.fs.read(path);

    if (this.isText(path) && action.baseHash) {
      const baseData = await this.tryFetchBlob(action.baseHash);
      const base = baseData && decodeText(baseData);
      const local = decodeText(localData);
      const remote = decodeText(remoteData);
      if (base != null && local != null && remote != null) {
        const outcome = mergeText(base, local, remote);
        if (outcome.clean) {
          const merged = encodeText(outcome.text);
          await this.fs.write(path, merged);
          // Recorded as the server version; if the merge added local edits,
          // the push phase sees the difference and uploads it.
          await this.record(state, path, action.remoteHash, action.seq, await sha256Hex(merged));
          add(report.merged, path);
          return true;
        }
      }
    }

    // Can't merge: keep the server version in place and this device's version as a copy.
    const copy = conflictCopyPath(
      path,
      this.options.deviceName,
      this.now(),
      (candidate) => scan.files.has(candidate) || candidate in state.files,
    );
    await this.fs.write(copy, localData);
    await this.fs.write(path, remoteData);
    await this.record(state, path, action.remoteHash, action.seq, action.remoteHash);
    scan.files.set(copy, { hash: "", size: localData.length, mtime: -1 });
    add(report.conflictCopies, copy);
    return true;
  }

  /** True if the file is still exactly as the scan saw it (or still absent). */
  private async unchangedSinceScan(path: string, scan: Scan): Promise<boolean> {
    const expected = scan.files.get(path);
    const stat = await this.fs.stat(path);
    if (!expected || !stat) return !expected && !stat;
    if (stat.mtime === expected.mtime && stat.size === expected.size) return true;
    return (await sha256Hex(await this.fs.read(path))) === expected.hash;
  }

  /**
   * Saves the server version of a path. If the local content differs from it
   * (`contentHash` !== `hash`), the timestamps are left unknown so the next
   * scan re-hashes the file and notices the difference.
   */
  private async record(
    state: SyncState,
    path: string,
    hash: string,
    seq: number,
    contentHash: string,
  ): Promise<void> {
    const stat = await this.fs.stat(path);
    const known = stat !== null && contentHash === hash;
    state.files[path] = {
      hash,
      seq,
      mtime: known ? stat.mtime : -1,
      size: known ? stat.size : -1,
    };
  }

  private async fetchBlob(hash: string): Promise<Uint8Array> {
    const data = await this.api.download(this.options.vaultId, hash);
    if ((await sha256Hex(data)) !== hash) {
      throw new Error("Downloaded content doesn't match its hash.");
    }
    return data;
  }

  /** Like fetchBlob, but returns null if the server no longer has the content. */
  private async tryFetchBlob(hash: string): Promise<Uint8Array | null> {
    try {
      return await this.fetchBlob(hash);
    } catch (err) {
      if (err instanceof ServerError && err.status === 404) return null;
      throw err;
    }
  }

  private isText(path: string): boolean {
    return this.textExtensions.has(extension(path));
  }

  private checkMassDelete(
    count: number,
    where: "local" | "server",
    total: number,
    run: SyncRunOptions,
  ): void {
    if (run.allowMassDelete || count === 0) return;
    const { maxCount, maxFraction, minCount } = this.massDelete;
    if (count > maxCount || (count >= minCount && count > total * maxFraction)) {
      throw new MassDeleteError(count, where);
    }
  }

  // ---- Push local changes ----

  private async push(
    state: SyncState,
    scan: Scan,
    report: SyncReport,
    run: SyncRunOptions,
  ): Promise<void> {
    const upserts: CommitChange[] = [];
    const deletes: CommitChange[] = [];
    for (const [path, local] of scan.files) {
      const base = state.files[path];
      if (base?.hash === local.hash) {
        // Same content with new timestamps: remember them to avoid re-hashing.
        if (base.mtime !== local.mtime || base.size !== local.size) {
          state.files[path] = { ...base, mtime: local.mtime, size: local.size };
        }
        continue;
      }
      upserts.push({ path, op: "upsert", hash: local.hash, expect_seq: base?.seq ?? null });
    }
    for (const [path, base] of Object.entries(state.files)) {
      if (!scan.files.has(path) && !this.ignored(path, scan)) {
        deletes.push({ path, op: "delete", expect_seq: base.seq });
      }
    }
    if (upserts.length === 0 && deletes.length === 0) {
      await this.store.save(state);
      return;
    }
    this.checkMassDelete(deletes.length, "server", Object.keys(state.files).length, run);

    const allowed = this.dropCaseCollisions(upserts, deletes, state, report);
    const unstable = await this.uploadMissing(allowed);

    const changes = [...deletes, ...allowed];
    for (let i = 0; i < changes.length; i += COMMIT_CHUNK) {
      const chunk = changes.slice(i, i + COMMIT_CHUNK);
      const applied = await this.commitChanges(chunk, report);
      for (const change of chunk) {
        const seq = applied.get(change.path);
        if (seq === undefined) continue;
        if (change.op === "delete") {
          delete state.files[change.path];
          add(report.deletedRemotely, change.path);
        } else {
          const local = scan.files.get(change.path) as LocalFile;
          const stable = !unstable.has(change.path);
          state.files[change.path] = {
            hash: change.hash as string,
            seq,
            mtime: stable ? local.mtime : -1,
            size: stable ? local.size : -1,
          };
          add(report.uploaded, change.path);
        }
      }
      await this.store.save(state);
    }
  }

  /** Skips new paths that differ only by letter case from another synced path. */
  private dropCaseCollisions(
    upserts: CommitChange[],
    deletes: CommitChange[],
    state: SyncState,
    report: SyncReport,
  ): CommitChange[] {
    const deleting = new Set(deletes.map((d) => d.path));
    const taken = new Map<string, string>();
    for (const path of Object.keys(state.files)) {
      if (!deleting.has(path)) taken.set(pathKey(path), path);
    }
    const allowed: CommitChange[] = [];
    for (const change of upserts) {
      const key = pathKey(change.path);
      const other = taken.get(key);
      if (other !== undefined && other !== change.path) {
        addSkip(report, change.path, `differs only by letter case from "${other}"`);
        continue;
      }
      taken.set(key, change.path);
      allowed.push(change);
    }
    return allowed;
  }

  /**
   * Uploads content the server doesn't have yet. Returns paths whose content
   * changed since the scan (their commit uses the new content's hash).
   */
  private async uploadMissing(changes: CommitChange[]): Promise<Set<string>> {
    const { vaultId } = this.options;
    const unstable = new Set<string>();
    const hashes = [...new Set(changes.map((c) => c.hash as string))];
    const missing = new Set<string>();
    for (let i = 0; i < hashes.length; i += HASH_CHUNK) {
      for (const hash of await this.api.missing(vaultId, hashes.slice(i, i + HASH_CHUNK))) {
        missing.add(hash);
      }
    }
    for (const change of changes) {
      if (!missing.has(change.hash as string)) continue;
      const data = await this.fs.read(change.path);
      const hash = await sha256Hex(data);
      if (hash !== change.hash) {
        change.hash = hash;
        unstable.add(change.path);
      } else {
        missing.delete(hash);
      }
      await this.api.upload(vaultId, hash, data);
    }
    return unstable;
  }

  /**
   * Commits a batch. If the server rejects it for a letter-case collision,
   * commits the changes one at a time and skips the ones that collide.
   */
  private async commitChanges(
    changes: CommitChange[],
    report: SyncReport,
  ): Promise<Map<string, number>> {
    try {
      const result = await this.api.commit(this.options.vaultId, changes);
      return new Map(result.applied.map((a) => [a.path, a.seq]));
    } catch (err) {
      if (!(err instanceof ServerError && err.code === "case_conflict")) throw err;
      if (changes.length === 1) {
        addSkip(report, changes[0].path, err.message);
        return new Map();
      }
      const applied = new Map<string, number>();
      for (const change of changes) {
        for (const [path, seq] of await this.commitChanges([change], report)) {
          applied.set(path, seq);
        }
      }
      return applied;
    }
  }
}

function add(list: string[], item: string): void {
  if (!list.includes(item)) list.push(item);
}

function addSkip(report: SyncReport, path: string, reason: string): void {
  if (!report.skipped.some((s) => s.path === path)) report.skipped.push({ path, reason });
}
