import { normalizePath, TFile, type Vault } from "obsidian";
import type { FileStat, VaultFs } from "../engine/types";

/**
 * The engine's file access, through Obsidian's vault API so open notes, links
 * and the file explorer stay up to date.
 */
export class ObsidianVaultFs implements VaultFs {
  constructor(private readonly vault: Vault) {}

  async list(): Promise<FileStat[]> {
    // Obsidian doesn't index hidden files and folders, so `.obsidian/` and
    // other dot-folders never appear here.
    return this.vault.getFiles().map((file) => ({
      path: file.path,
      size: file.stat.size,
      mtime: file.stat.mtime,
    }));
  }

  async stat(path: string): Promise<FileStat | null> {
    // Asks the disk directly rather than Obsidian's cache, because the engine
    // uses this to check that a file hasn't changed before overwriting it.
    const stat = await this.vault.adapter.stat(normalizePath(path));
    if (!stat || stat.type !== "file") return null;
    return { path, size: stat.size, mtime: stat.mtime };
  }

  async read(path: string): Promise<Uint8Array> {
    const file = this.vault.getFileByPath(path);
    const data = file
      ? await this.vault.readBinary(file)
      : await this.vault.adapter.readBinary(normalizePath(path));
    return new Uint8Array(data);
  }

  async write(path: string, data: Uint8Array): Promise<void> {
    const buffer = toArrayBuffer(data);
    const file = this.vault.getFileByPath(path);
    if (file) {
      await this.vault.modifyBinary(file, buffer);
      return;
    }
    await this.ensureParentFolder(path);
    try {
      await this.vault.createBinary(path, buffer);
    } catch (err) {
      // The file exists on disk but Obsidian hasn't indexed it yet.
      if (!(await this.vault.adapter.exists(normalizePath(path)))) throw err;
      await this.vault.adapter.writeBinary(normalizePath(path), buffer);
    }
  }

  async trash(path: string): Promise<void> {
    const file = this.vault.getFileByPath(path);
    // Always the vault's own .trash folder, whatever the "deleted files"
    // setting says, so a synced delete can be undone on every device.
    if (file) await this.vault.trash(file, false);
    else if (await this.vault.adapter.exists(normalizePath(path))) {
      await this.vault.adapter.trashLocal(normalizePath(path));
    }
  }

  async rename(from: string, to: string): Promise<void> {
    const file = this.vault.getFileByPath(from);
    if (!(file instanceof TFile)) throw new Error(`Can't rename "${from}": file not found.`);
    await this.ensureParentFolder(to);
    // vault.rename, not fileManager.renameFile: the device that renamed the
    // file already updated links, and those edits arrive as their own changes.
    if (from !== to && from.toLowerCase() === to.toLowerCase()) {
      // A case-only rename goes through a temporary name, so it works the same
      // on file systems that ignore letter case.
      await this.vault.rename(file, `${to}.notesync-rename`);
    }
    await this.vault.rename(file, to);
  }

  private async ensureParentFolder(path: string): Promise<void> {
    const slash = path.lastIndexOf("/");
    if (slash <= 0) return;
    const folder = path.slice(0, slash);
    if (this.vault.getFolderByPath(folder)) return;
    try {
      await this.vault.createFolder(folder);
    } catch (err) {
      if (!(await this.vault.adapter.exists(normalizePath(folder)))) throw err;
    }
  }
}

function toArrayBuffer(data: Uint8Array): ArrayBuffer {
  return data.buffer.slice(data.byteOffset, data.byteOffset + data.byteLength) as ArrayBuffer;
}
