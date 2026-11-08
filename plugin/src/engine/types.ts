// Interfaces between the sync engine and the outside world. The engine never
// imports Obsidian, so it can be tested with in-memory implementations.

export interface FileStat {
  path: string;
  size: number;
  mtime: number;
}

/** File access for one vault. Paths are relative and use "/" separators. */
export interface VaultFs {
  /** Every file in the vault, including hidden folders (the engine filters them). */
  list(): Promise<FileStat[]>;
  stat(path: string): Promise<FileStat | null>;
  read(path: string): Promise<Uint8Array>;
  /** Replaces the file's contents, creating it and its parent folders if needed. */
  write(path: string, data: Uint8Array): Promise<void>;
  /** Moves the file to the vault's trash instead of deleting it permanently. */
  trash(path: string): Promise<void>;
  rename(from: string, to: string): Promise<void>;
}

// Server API shapes (see docs/design.md).

export type ChangeOp = "upsert" | "delete";

export interface RemoteChange {
  seq: number;
  path: string;
  op: ChangeOp;
  hash: string | null;
  size: number | null;
  created_at: number;
}

export interface ChangesPage {
  changes: RemoteChange[];
  head_seq: number;
  more: boolean;
}

export interface RemoteFile {
  path: string;
  hash: string;
  size: number;
  seq: number;
}

export interface RemoteState {
  head_seq: number;
  files: RemoteFile[];
}

export interface CommitChange {
  path: string;
  op: ChangeOp;
  hash?: string;
  /** The version of the path this device last saw; null means it shouldn't exist yet. */
  expect_seq: number | null;
}

export interface CommitResult {
  head_seq: number;
  applied: { path: string; seq: number }[];
}

export interface PathConflict {
  path: string;
  current_hash: string | null;
  current_seq: number | null;
}

export interface ServerApi {
  changes(vaultId: number, since: number, limit?: number): Promise<ChangesPage>;
  state(vaultId: number): Promise<RemoteState>;
  missing(vaultId: number, hashes: string[]): Promise<string[]>;
  upload(vaultId: number, hash: string, data: Uint8Array): Promise<void>;
  download(vaultId: number, hash: string): Promise<Uint8Array>;
  commit(vaultId: number, changes: CommitChange[]): Promise<CommitResult>;
}

// What this device remembers between syncs.

export interface SyncedFile {
  hash: string;
  /** Server version of the path at the last sync. */
  seq: number;
  /** Local mtime and size when the file matched `hash`; -1 means "unknown, re-hash". */
  mtime: number;
  size: number;
}

export interface SyncState {
  vaultId: number;
  /** Every server change up to this number has been applied locally. */
  lastSeq: number;
  files: Record<string, SyncedFile>;
}

export interface StateStore {
  load(): Promise<SyncState | null>;
  /** Must replace the saved state atomically. */
  save(state: SyncState): Promise<void>;
}

export interface SyncReport {
  downloaded: string[];
  uploaded: string[];
  deletedLocally: string[];
  deletedRemotely: string[];
  renamedLocally: { from: string; to: string }[];
  merged: string[];
  conflictCopies: string[];
  skipped: { path: string; reason: string }[];
}

export function emptyReport(): SyncReport {
  return {
    downloaded: [],
    uploaded: [],
    deletedLocally: [],
    deletedRemotely: [],
    renamedLocally: [],
    merged: [],
    conflictCopies: [],
    skipped: [],
  };
}
