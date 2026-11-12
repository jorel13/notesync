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
  /** Scratch space for downloads too large to hold in memory. */
  readonly staging: StagingFs;
}

/**
 * Scratch files outside the vault (never synced or shown), for building a
 * large download a piece at a time. Names are chosen by the engine.
 */
export interface StagingFs {
  /** Size in bytes, or null if there's no such file. */
  size(name: string): Promise<number | null>;
  /** Adds bytes to the end, creating the file if needed. */
  append(name: string, data: Uint8Array): Promise<void>;
  readText(name: string): Promise<string | null>;
  writeText(name: string, text: string): Promise<void>;
  /** Does nothing if there's no such file. */
  remove(name: string): Promise<void>;
  list(): Promise<string[]>;
  /** Moves a scratch file to `path` in the vault, replacing any file there
   * and creating parent folders. */
  moveIntoVault(name: string, path: string): Promise<void>;
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

/** Where an upload sent in pieces stands on the server. */
export interface UploadProgress {
  /** Bytes the server has so far. */
  received: number;
  /** True once the content is stored; no more pieces are needed. */
  stored: boolean;
}

export interface ServerApi {
  changes(vaultId: number, since: number, limit?: number): Promise<ChangesPage>;
  state(vaultId: number): Promise<RemoteState>;
  missing(vaultId: number, hashes: string[]): Promise<string[]>;
  upload(vaultId: number, hash: string, data: Uint8Array): Promise<void>;
  download(vaultId: number, hash: string): Promise<Uint8Array>;
  /**
   * Up to `length` bytes starting at `offset`. `whole` is true when the
   * server ignored the range and sent the entire file (servers before 0.5.0).
   */
  downloadRange(
    vaultId: number,
    hash: string,
    offset: number,
    length: number,
  ): Promise<{ data: Uint8Array; whole: boolean }>;
  /** Null if the server can't take uploads in pieces (before 0.5.0). */
  uploadProgress(vaultId: number, hash: string): Promise<UploadProgress | null>;
  /** Adds the piece that starts at `offset` of a `total`-byte file. An empty
   * piece at the end finishes an upload that already has every byte. */
  uploadPart(
    vaultId: number,
    hash: string,
    offset: number,
    total: number,
    data: Uint8Array,
  ): Promise<UploadProgress>;
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
  /** Server files not downloaded because they're over this device's size
   * limit. Kept so the path isn't overwritten from here, and so the file is
   * downloaded if the limit is raised. */
  tooLarge?: Record<string, RemoteFile>;
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
