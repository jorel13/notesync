import type { FileStat, VaultFs } from "../engine/types";

/** How long after the sync touches a file its change events are ignored. */
export const SELF_WRITE_WINDOW_MS = 5_000;

/**
 * Wraps a VaultFs and remembers which paths the sync itself changed, so the
 * file events those changes cause don't schedule another sync.
 */
export class TrackingFs implements VaultFs {
  private readonly touched = new Map<string, number>();

  constructor(
    private readonly inner: VaultFs,
    private readonly now: () => number = Date.now,
  ) {}

  /** True if the sync changed this path in the last few seconds. */
  isOwnChange(path: string): boolean {
    const at = this.touched.get(path);
    if (at === undefined) return false;
    if (this.now() - at <= SELF_WRITE_WINDOW_MS) return true;
    this.touched.delete(path);
    return false;
  }

  private mark(...paths: string[]): void {
    const at = this.now();
    for (const path of paths) this.touched.set(path, at);
  }

  list(): Promise<FileStat[]> {
    return this.inner.list();
  }

  stat(path: string): Promise<FileStat | null> {
    return this.inner.stat(path);
  }

  read(path: string): Promise<Uint8Array> {
    return this.inner.read(path);
  }

  async write(path: string, data: Uint8Array): Promise<void> {
    // Marked before and after: Obsidian may report the event during the write
    // or just after it finishes.
    this.mark(path);
    await this.inner.write(path, data);
    this.mark(path);
  }

  async trash(path: string): Promise<void> {
    this.mark(path);
    await this.inner.trash(path);
    this.mark(path);
  }

  async rename(from: string, to: string): Promise<void> {
    this.mark(from, to);
    await this.inner.rename(from, to);
    this.mark(from, to);
  }
}
