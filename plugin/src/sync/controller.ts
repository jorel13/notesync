// Decides when to sync and turns results and errors into status and messages.
// No Obsidian imports, so it can be tested on its own.

import type { SyncEngine } from "../engine/engine";
import { MassDeleteError, ServerError } from "../engine/errors";
import type { SyncReport } from "../engine/types";

export type SyncTrigger = "manual" | "startup" | "edit" | "timer" | "resume";

export type SyncStatus =
  | { kind: "not-configured" }
  | { kind: "idle"; lastSync: number | null }
  | { kind: "syncing" }
  | { kind: "error"; message: string; lastSync: number | null };

export interface SyncControllerDeps {
  /** The engine for the current settings, or null if sign-in or vault choice is missing. */
  engine(): SyncEngine | null;
  /** True if a file event was caused by the sync itself. */
  isOwnChange(path: string): boolean;
  /** Asks the user whether a large delete may go ahead. */
  confirmMassDelete(error: MassDeleteError): Promise<boolean>;
  notify(message: string): void;
  onStatus(status: SyncStatus): void;
  onReport?(report: SyncReport, trigger: SyncTrigger): void;
  now?: () => number;
}

export interface SyncSchedule {
  /** Sync this long after the last edit; null turns edit syncing off. */
  editDelayMs: number | null;
  /** Sync on a timer this often; null turns the timer off. */
  intervalMs: number | null;
}

/** Coming back to the app syncs only if the last sync is older than this. */
export const RESUME_MIN_GAP_MS = 60_000;

export class SyncController {
  private schedule: SyncSchedule = { editDelayMs: null, intervalMs: null };
  private editTimer: ReturnType<typeof setTimeout> | null = null;
  private intervalTimer: ReturnType<typeof setInterval> | null = null;
  private running = 0;
  private confirming = false;
  private lastSync: number | null = null;
  private lastAttempt: number | null = null;
  private lastError: string | null = null;
  private status: SyncStatus = { kind: "idle", lastSync: null };
  private readonly now: () => number;

  constructor(private readonly deps: SyncControllerDeps) {
    this.now = deps.now ?? Date.now;
  }

  get currentStatus(): SyncStatus {
    return this.status;
  }

  /** Applies new timer settings. Call again whenever the settings change. */
  start(schedule: SyncSchedule): void {
    this.stop();
    this.schedule = schedule;
    if (schedule.intervalMs !== null && schedule.intervalMs > 0) {
      this.intervalTimer = setInterval(() => void this.run("timer"), schedule.intervalMs);
    }
  }

  stop(): void {
    if (this.editTimer !== null) clearTimeout(this.editTimer);
    if (this.intervalTimer !== null) clearInterval(this.intervalTimer);
    this.editTimer = null;
    this.intervalTimer = null;
  }

  /** Called for every file event in the vault. */
  fileChanged(path: string): void {
    const delay = this.schedule.editDelayMs;
    if (delay === null || this.deps.isOwnChange(path)) return;
    if (this.editTimer !== null) clearTimeout(this.editTimer);
    this.editTimer = setTimeout(() => {
      this.editTimer = null;
      void this.run("edit");
    }, delay);
  }

  /** Called when the app comes back to the foreground. */
  resumed(): void {
    const last = this.lastAttempt;
    if (last === null || this.now() - last >= RESUME_MIN_GAP_MS) void this.run("resume");
  }

  /** Runs a sync now. Never throws; problems end up in the status and notices. */
  async run(trigger: SyncTrigger, allowMassDelete = false): Promise<void> {
    const engine = this.deps.engine();
    if (!engine) {
      this.setStatus({ kind: "not-configured" });
      if (trigger === "manual") {
        this.deps.notify("notesync: sign in and choose a vault in the plugin settings first.");
      }
      return;
    }

    this.lastAttempt = this.now();
    this.running++;
    this.setStatus({ kind: "syncing" });
    try {
      const report = await engine.sync({ allowMassDelete });
      this.lastSync = this.now();
      this.lastError = null;
      this.finish({ kind: "idle", lastSync: this.lastSync });
      this.deps.onReport?.(report, trigger);
      this.announce(report, trigger);
    } catch (err) {
      if (err instanceof MassDeleteError) {
        this.finish({ kind: "idle", lastSync: this.lastSync });
        await this.askAboutMassDelete(err, trigger);
        return;
      }
      const message = describeError(err);
      this.finish({ kind: "error", message, lastSync: this.lastSync });
      // Automatic syncs only speak up when the problem is new, so being
      // offline doesn't produce a notice every few minutes.
      if (trigger === "manual" || message !== this.lastError) {
        this.deps.notify(`notesync: ${message}`);
      }
      this.lastError = message;
    }
  }

  private finish(status: SyncStatus): void {
    this.running = Math.max(0, this.running - 1);
    // While another run is still going, keep showing "syncing".
    if (this.running === 0) this.setStatus(status);
  }

  private setStatus(status: SyncStatus): void {
    this.status = status;
    this.deps.onStatus(status);
  }

  private async askAboutMassDelete(err: MassDeleteError, trigger: SyncTrigger): Promise<void> {
    if (this.confirming) return;
    this.confirming = true;
    let allowed: boolean;
    try {
      allowed = await this.deps.confirmMassDelete(err);
    } finally {
      this.confirming = false;
    }
    if (allowed) {
      await this.run(trigger, true);
    } else {
      const message = "Sync paused: a large delete is waiting for confirmation.";
      this.setStatus({ kind: "error", message, lastSync: this.lastSync });
      this.lastError = message;
    }
  }

  private announce(report: SyncReport, trigger: SyncTrigger): void {
    if (report.conflictCopies.length > 0) {
      this.deps.notify(
        `notesync: conflicting edits were kept as copies:\n${report.conflictCopies.join("\n")}`,
      );
    }
    if (trigger === "manual") this.deps.notify(`notesync: ${summarize(report)}`);
  }
}

/** Short status text for the status bar, ribbon tooltip and settings. */
export function statusText(status: SyncStatus): string {
  switch (status.kind) {
    case "not-configured":
      return "not set up";
    case "syncing":
      return "syncing…";
    case "idle":
      return status.lastSync === null
        ? "ready"
        : `synced ${new Date(status.lastSync).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`;
    case "error":
      return status.message;
  }
}

/** One line describing what a sync did. */
export function summarize(report: SyncReport): string {
  const parts: string[] = [];
  const count = (n: number, what: string) => {
    if (n > 0) parts.push(`${n} ${what}`);
  };
  count(report.downloaded.length, "downloaded");
  count(report.uploaded.length, "uploaded");
  count(report.renamedLocally.length, "renamed");
  count(report.merged.length, "merged");
  count(report.deletedLocally.length + report.deletedRemotely.length, "deleted");
  count(report.conflictCopies.length, "conflict copies");
  count(report.skipped.length, "skipped");
  return parts.length === 0 ? "Everything is up to date." : `Synced: ${parts.join(", ")}.`;
}

/** A short, user-facing explanation of a sync failure. */
export function describeError(err: unknown): string {
  if (err instanceof ServerError) {
    switch (err.code) {
      case "unauthorized":
        return "Signed out by the server. Sign in again in the plugin settings.";
      case "client_too_old":
        return "The server needs a newer version of this plugin. Update the plugin.";
      case "not_found":
        return "The server vault wasn't found. Choose a vault again in the plugin settings.";
      case "rate_limited":
        return "Too many attempts. Wait a few minutes and try again.";
      default:
        return err.message;
    }
  }
  const message = err instanceof Error ? err.message : String(err);
  if (/net::|ERR_|ENOTFOUND|ECONNREFUSED|ETIMEDOUT|Failed to fetch|timed? ?out/i.test(message)) {
    return "Can't reach the server. Check that Tailscale is connected.";
  }
  return `Sync failed: ${message}`;
}
