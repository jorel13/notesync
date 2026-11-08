import { normalizePath, type DataAdapter } from "obsidian";
import type { StateStore, SyncState } from "../engine/types";

/**
 * Saves the sync state as `sync-state.json` in the plugin's folder (inside
 * `.obsidian/`, which is never synced). Writes go to a temporary file first,
 * so an interrupted save leaves the previous state readable.
 */
export class FileStateStore implements StateStore {
  private readonly path: string;
  private readonly tempPath: string;

  constructor(
    private readonly adapter: DataAdapter,
    pluginDir: string,
  ) {
    this.path = normalizePath(`${pluginDir}/sync-state.json`);
    this.tempPath = normalizePath(`${pluginDir}/sync-state.json.tmp`);
  }

  async load(): Promise<SyncState | null> {
    for (const path of [this.path, this.tempPath]) {
      if (!(await this.adapter.exists(path))) continue;
      try {
        const state = JSON.parse(await this.adapter.read(path)) as SyncState;
        if (typeof state.vaultId === "number" && typeof state.lastSeq === "number") return state;
      } catch {
        // Unreadable: try the other file, or start fresh.
      }
    }
    return null;
  }

  async save(state: SyncState): Promise<void> {
    await this.adapter.write(this.tempPath, JSON.stringify(state));
    if (await this.adapter.exists(this.path)) await this.adapter.remove(this.path);
    await this.adapter.rename(this.tempPath, this.path);
  }

  /** Forgets everything, e.g. after switching to another server vault. */
  async clear(): Promise<void> {
    for (const path of [this.path, this.tempPath]) {
      if (await this.adapter.exists(path)) await this.adapter.remove(path);
    }
  }
}
