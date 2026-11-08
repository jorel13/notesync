import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SyncEngine } from "../src/engine/engine";
import { MassDeleteError, ServerError } from "../src/engine/errors";
import { emptyReport, type SyncReport } from "../src/engine/types";
import {
  describeError,
  RESUME_MIN_GAP_MS,
  summarize,
  SyncController,
  type SyncControllerDeps,
  type SyncStatus,
} from "../src/sync/controller";

type SyncFn = (run: { allowMassDelete?: boolean }) => Promise<SyncReport>;

function setup(options: { sync?: SyncFn; configured?: boolean; confirm?: boolean } = {}) {
  const sync = vi.fn<SyncFn>(options.sync ?? (async () => emptyReport()));
  const engine = { sync } as unknown as SyncEngine;
  const notices: string[] = [];
  const statuses: SyncStatus[] = [];
  const ownChanges = new Set<string>();
  const confirmMassDelete = vi.fn(async () => options.confirm ?? false);
  const deps: SyncControllerDeps = {
    engine: () => (options.configured === false ? null : engine),
    isOwnChange: (path) => ownChanges.has(path),
    confirmMassDelete,
    notify: (message) => notices.push(message),
    onStatus: (status) => statuses.push(status),
  };
  const controller = new SyncController(deps);
  return { controller, sync, notices, statuses, ownChanges, confirmMassDelete };
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("SyncController", () => {
  it("reports not-configured and explains for a manual sync", async () => {
    const { controller, notices, statuses, sync } = setup({ configured: false });
    await controller.run("manual");
    expect(sync).not.toHaveBeenCalled();
    expect(statuses.at(-1)).toEqual({ kind: "not-configured" });
    expect(notices).toHaveLength(1);

    await controller.run("timer");
    expect(notices).toHaveLength(1);
  });

  it("goes syncing then idle, and summarizes manual syncs", async () => {
    const report = emptyReport();
    report.downloaded.push("a.md");
    const { controller, statuses, notices } = setup({ sync: async () => report });
    await controller.run("manual");
    expect(statuses.map((s) => s.kind)).toEqual(["syncing", "idle"]);
    expect(notices).toEqual(["notesync: Synced: 1 downloaded."]);
  });

  it("stays quiet after automatic syncs unless conflict copies were made", async () => {
    const report = emptyReport();
    const { controller, notices } = setup({ sync: async () => report });
    await controller.run("timer");
    expect(notices).toEqual([]);

    report.conflictCopies.push("Note (conflict 2026-11-07 1200 PC).md");
    await controller.run("edit");
    expect(notices).toHaveLength(1);
    expect(notices[0]).toContain("Note (conflict");
  });

  it("notifies an automatic error once until it changes", async () => {
    const { controller, notices, statuses } = setup({
      sync: async () => {
        throw new Error("net::ERR_NAME_NOT_RESOLVED");
      },
    });
    await controller.run("timer");
    await controller.run("timer");
    expect(notices).toEqual(["notesync: Can't reach the server. Check that Tailscale is connected."]);
    expect(statuses.at(-1)?.kind).toBe("error");

    await controller.run("manual");
    expect(notices).toHaveLength(2);
  });

  it("asks before a mass delete and retries with permission", async () => {
    const { controller, sync, confirmMassDelete } = setup({
      confirm: true,
      sync: async (run) => {
        if (!run.allowMassDelete) throw new MassDeleteError(40, "server");
        return emptyReport();
      },
    });
    await controller.run("timer");
    expect(confirmMassDelete).toHaveBeenCalledOnce();
    expect(sync).toHaveBeenCalledTimes(2);
    expect(sync.mock.calls[1][0]).toEqual({ allowMassDelete: true });
    expect(controller.currentStatus.kind).toBe("idle");
  });

  it("pauses when a mass delete is declined", async () => {
    const { controller, sync } = setup({
      confirm: false,
      sync: async () => {
        throw new MassDeleteError(40, "local");
      },
    });
    await controller.run("manual");
    expect(sync).toHaveBeenCalledOnce();
    expect(controller.currentStatus).toMatchObject({ kind: "error" });
  });

  it("syncs once after a burst of edits", async () => {
    const { controller, sync } = setup();
    controller.start({ editDelayMs: 10_000, intervalMs: null });
    controller.fileChanged("a.md");
    await vi.advanceTimersByTimeAsync(5_000);
    controller.fileChanged("a.md");
    await vi.advanceTimersByTimeAsync(9_000);
    expect(sync).not.toHaveBeenCalled();
    await vi.advanceTimersByTimeAsync(1_000);
    expect(sync).toHaveBeenCalledOnce();
    controller.stop();
  });

  it("ignores file events caused by the sync itself", async () => {
    const { controller, sync, ownChanges } = setup();
    controller.start({ editDelayMs: 1_000, intervalMs: null });
    ownChanges.add("downloaded.md");
    controller.fileChanged("downloaded.md");
    await vi.advanceTimersByTimeAsync(5_000);
    expect(sync).not.toHaveBeenCalled();
    controller.stop();
  });

  it("ignores edits when edit syncing is off", async () => {
    const { controller, sync } = setup();
    controller.start({ editDelayMs: null, intervalMs: null });
    controller.fileChanged("a.md");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(sync).not.toHaveBeenCalled();
  });

  it("syncs on the timer until stopped", async () => {
    const { controller, sync } = setup();
    controller.start({ editDelayMs: null, intervalMs: 60_000 });
    await vi.advanceTimersByTimeAsync(180_000);
    expect(sync).toHaveBeenCalledTimes(3);
    controller.stop();
    await vi.advanceTimersByTimeAsync(180_000);
    expect(sync).toHaveBeenCalledTimes(3);
  });

  it("syncs on resume only if the last attempt is old enough", async () => {
    const { controller, sync } = setup();
    controller.resumed();
    await vi.advanceTimersByTimeAsync(0);
    expect(sync).toHaveBeenCalledTimes(1);

    controller.resumed();
    await vi.advanceTimersByTimeAsync(0);
    expect(sync).toHaveBeenCalledTimes(1);

    await vi.advanceTimersByTimeAsync(RESUME_MIN_GAP_MS);
    controller.resumed();
    await vi.advanceTimersByTimeAsync(0);
    expect(sync).toHaveBeenCalledTimes(2);
  });
});

describe("describeError", () => {
  it("explains server error codes", () => {
    expect(describeError(new ServerError(401, "unauthorized", "x"))).toMatch(/Sign in again/);
    expect(describeError(new ServerError(426, "client_too_old", "x"))).toMatch(/Update the plugin/);
    expect(describeError(new ServerError(404, "not_found", "x"))).toMatch(/Choose a vault/);
    expect(describeError(new ServerError(400, "invalid_path", "Bad path"))).toBe("Bad path");
  });

  it("recognizes network failures", () => {
    expect(describeError(new Error("net::ERR_CONNECTION_REFUSED"))).toMatch(/Can't reach/);
    expect(describeError(new Error("Something else"))).toBe("Sync failed: Something else");
  });
});

describe("summarize", () => {
  it("says when nothing changed", () => {
    expect(summarize(emptyReport())).toBe("Everything is up to date.");
  });

  it("lists what happened", () => {
    const report = emptyReport();
    report.uploaded.push("a.md", "b.md");
    report.deletedLocally.push("c.md");
    report.deletedRemotely.push("d.md");
    expect(summarize(report)).toBe("Synced: 2 uploaded, 2 deleted.");
  });
});
