import { normalizePath, TFile, type DataAdapter, type Vault } from "obsidian";
import type { FileStat, StagingFs, VaultFs } from "../engine/types";
import { toArrayBuffer } from "./transport";

/**
 * The engine's file access, through Obsidian's vault API so open notes, links
 * and the file explorer stay up to date.
 */
export class ObsidianVaultFs implements VaultFs {
  readonly staging: StagingFs;
  /**
   * Files written straight to disk (bypassing the vault API) that Obsidian may
   * not have indexed yet. `list` reports them anyway, so a sync running before
   * the index catches up never mistakes them for deleted files.
   */
  private readonly placed = new Set<string>();

  /** `stagingDir` is a folder inside the settings folder, which is never synced. */
  constructor(
    private readonly vault: Vault,
    stagingDir: string,
  ) {
    this.staging = new ObsidianStaging(vault.adapter, normalizePath(stagingDir), this);
  }

  async list(): Promise<FileStat[]> {
    // Obsidian doesn't index hidden files and folders, so `.obsidian/` and
    // other dot-folders never appear here.
    const files = this.vault.getFiles().map((file) => ({
      path: file.path,
      size: file.stat.size,
      mtime: file.stat.mtime,
    }));
    for (const path of [...this.placed]) {
      if (this.vault.getFileByPath(path)) {
        this.placed.delete(path);
        continue;
      }
      const stat = await this.stat(path);
      if (stat) files.push(stat);
      else this.placed.delete(path);
    }
    return files;
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
      this.placed.add(path);
    }
  }

  async trash(path: string): Promise<void> {
    const file = this.vault.getFileByPath(path);
    this.placed.delete(path);
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

  /** Records a file put in place on disk directly, until Obsidian indexes it. */
  notePlaced(path: string): void {
    this.placed.add(path);
  }

  async ensureParentFolder(path: string): Promise<void> {
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

/** Scratch files in a folder inside the settings folder. */
class ObsidianStaging implements StagingFs {
  constructor(
    private readonly adapter: DataAdapter,
    private readonly dir: string,
    private readonly fs: ObsidianVaultFs,
  ) {}

  private path(name: string): string {
    return normalizePath(`${this.dir}/${name}`);
  }

  private async ensureDir(): Promise<void> {
    if (!(await this.adapter.exists(this.dir))) await this.adapter.mkdir(this.dir);
  }

  async size(name: string): Promise<number | null> {
    const stat = await this.adapter.stat(this.path(name));
    return stat && stat.type === "file" ? stat.size : null;
  }

  async append(name: string, data: Uint8Array): Promise<void> {
    const path = this.path(name);
    if (await this.adapter.exists(path)) {
      await this.adapter.appendBinary(path, toArrayBuffer(data));
      return;
    }
    await this.ensureDir();
    await this.adapter.writeBinary(path, toArrayBuffer(data));
  }

  async readText(name: string): Promise<string | null> {
    const path = this.path(name);
    return (await this.adapter.exists(path)) ? this.adapter.read(path) : null;
  }

  async writeText(name: string, text: string): Promise<void> {
    await this.ensureDir();
    await this.adapter.write(this.path(name), text);
  }

  async remove(name: string): Promise<void> {
    const path = this.path(name);
    if (await this.adapter.exists(path)) await this.adapter.remove(path);
  }

  async list(): Promise<string[]> {
    if (!(await this.adapter.exists(this.dir))) return [];
    const { files } = await this.adapter.list(this.dir);
    return files.map((file) => file.slice(file.lastIndexOf("/") + 1));
  }

  async moveIntoVault(name: string, path: string): Promise<void> {
    const target = normalizePath(path);
    await this.fs.ensureParentFolder(path);
    // The adapter won't rename over an existing file. The old version was
    // already checked as unchanged since the scan, and the server has it.
    if (await this.adapter.exists(target)) await this.adapter.remove(target);
    await this.adapter.rename(this.path(name), target);
    this.fs.notePlaced(path);
  }
}
