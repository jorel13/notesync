import { beforeEach, describe, expect, it } from "vitest";

import { MassDeleteError, ServerError } from "../src/engine/errors";
import type { CommitChange, ServerApi } from "../src/engine/types";
import { expectSameFiles, makeDevice, syncAll, type Device } from "./support/devices";
import { MemoryServer } from "./support/memory";

const VAULT = 1;

let server: MemoryServer;
let pc: Device;
let phone: Device;

beforeEach(() => {
  server = new MemoryServer();
  pc = makeDevice(server, VAULT, "PC");
  phone = makeDevice(server, VAULT, "Phone");
});

describe("basic sync", () => {
  it("uploads new files and downloads them on another device", async () => {
    pc.fs.setText("Note.md", "hello");
    pc.fs.setText("Folder/Other.md", "world");
    pc.fs.setBytes("Attachments/pic.png", new Uint8Array([1, 2, 3, 255]));

    const up = await pc.engine.sync();
    expect(up.uploaded.sort()).toEqual(["Attachments/pic.png", "Folder/Other.md", "Note.md"]);
    expect(server.paths()).toEqual(["Attachments/pic.png", "Folder/Other.md", "Note.md"]);

    const down = await phone.engine.sync();
    expect(down.downloaded.sort()).toEqual(["Attachments/pic.png", "Folder/Other.md", "Note.md"]);
    expectSameFiles(pc, phone);
  });

  it("does nothing when nothing changed", async () => {
    pc.fs.setText("Note.md", "hello");
    await syncAll(pc, phone);
    const report = await pc.engine.sync();
    expect(report).toMatchObject({ uploaded: [], downloaded: [], deletedLocally: [], deletedRemotely: [] });
  });

  it("propagates edits and deletes", async () => {
    pc.fs.setText("Keep.md", "v1");
    pc.fs.setText("Remove.md", "bye");
    await syncAll(pc, phone);

    phone.fs.setText("Keep.md", "v2");
    phone.fs.remove("Remove.md");
    const report = await phone.engine.sync();
    expect(report.uploaded).toEqual(["Keep.md"]);
    expect(report.deletedRemotely).toEqual(["Remove.md"]);

    const pcReport = await pc.engine.sync();
    expect(pcReport.downloaded).toEqual(["Keep.md"]);
    expect(pcReport.deletedLocally).toEqual(["Remove.md"]);
    expect(pc.fs.trashed).toEqual(["Remove.md"]);
    expectSameFiles(pc, phone);
  });

  it("combines two vaults on first sync without losing anything", async () => {
    pc.fs.setText("Only PC.md", "pc");
    pc.fs.setText("Same.md", "identical");
    pc.fs.setText("Differs.md", "pc version");
    await pc.engine.sync();

    phone.fs.setText("Only phone.md", "phone");
    phone.fs.setText("Same.md", "identical");
    phone.fs.setText("Differs.md", "phone version");
    const report = await phone.engine.sync();
    expect(report.conflictCopies).toEqual(["Differs (conflict 2026-11-06 1432 Phone).md"]);

    await syncAll(pc, phone);
    expectSameFiles(pc, phone);
    expect(pc.fs.snapshot()).toEqual({
      "Differs (conflict 2026-11-06 1432 Phone).md": "phone version",
      "Differs.md": "pc version",
      "Only PC.md": "pc",
      "Only phone.md": "phone",
      "Same.md": "identical",
    });
  });
});

describe("concurrent edits", () => {
  beforeEach(async () => {
    pc.fs.setText("Plan.md", "# Plan\n\n- groceries\n- laundry\n- call mum\n");
    await syncAll(pc, phone);
  });

  it("merges edits to different parts of a note", async () => {
    pc.fs.setText("Plan.md", "# Plan\n\n- groceries (milk)\n- laundry\n- call mum\n");
    phone.fs.setText("Plan.md", "# Plan\n\n- groceries\n- laundry\n- call mum and dad\n");
    await pc.engine.sync();
    const report = await phone.engine.sync();
    expect(report.merged).toEqual(["Plan.md"]);
    await pc.engine.sync();

    expectSameFiles(pc, phone);
    expect(pc.fs.text("Plan.md")).toBe("# Plan\n\n- groceries (milk)\n- laundry\n- call mum and dad\n");
  });

  it("keeps a conflict copy when edits overlap", async () => {
    pc.fs.setText("Plan.md", "# Plan\n\n- groceries\n- laundry today\n- call mum\n");
    phone.fs.setText("Plan.md", "# Plan\n\n- groceries\n- laundry tomorrow\n- call mum\n");
    await pc.engine.sync();
    const report = await phone.engine.sync();
    const copy = "Plan (conflict 2026-11-06 1432 Phone).md";
    expect(report.conflictCopies).toEqual([copy]);
    await pc.engine.sync();

    expectSameFiles(pc, phone);
    expect(pc.fs.text("Plan.md")).toContain("laundry today");
    expect(pc.fs.text(copy)).toContain("laundry tomorrow");
  });

  it("keeps a conflict copy for binary files", async () => {
    pc.fs.setBytes("pic.png", new Uint8Array([1]));
    await syncAll(pc, phone);
    pc.fs.setBytes("pic.png", new Uint8Array([2]));
    phone.fs.setBytes("pic.png", new Uint8Array([3]));
    await pc.engine.sync();
    const report = await phone.engine.sync();
    expect(report.conflictCopies).toEqual(["pic (conflict 2026-11-06 1432 Phone).png"]);
    await pc.engine.sync();
    expectSameFiles(pc, phone);
  });

  it("keeps an edit when the other device deleted the file", async () => {
    pc.fs.remove("Plan.md");
    phone.fs.setText("Plan.md", "edited on phone");
    await pc.engine.sync();
    await phone.engine.sync();
    await pc.engine.sync();
    expectSameFiles(pc, phone);
    expect(pc.fs.text("Plan.md")).toBe("edited on phone");
  });

  it("keeps an edit when the other device deleted the file first", async () => {
    phone.fs.setText("Plan.md", "edited on phone");
    pc.fs.remove("Plan.md");
    await phone.engine.sync();
    await pc.engine.sync();
    await phone.engine.sync();
    expectSameFiles(pc, phone);
    expect(pc.fs.text("Plan.md")).toBe("edited on phone");
  });

  it("merges edits to different notes made offline", async () => {
    for (let i = 0; i < 10; i++) {
      pc.fs.setText(`PC ${i}.md`, `pc ${i}`);
      phone.fs.setText(`Phone ${i}.md`, `phone ${i}`);
    }
    await syncAll(pc, phone);
    expectSameFiles(pc, phone);
    expect(pc.fs.paths()).toHaveLength(21);
  });
});

describe("renames", () => {
  it("applies a rename as a rename on the other device", async () => {
    pc.fs.setText("Draft.md", "content");
    await syncAll(pc, phone);

    pc.fs.move("Draft.md", "Final.md");
    const report = await pc.engine.sync();
    expect(report.uploaded).toEqual(["Final.md"]);
    expect(report.deletedRemotely).toEqual(["Draft.md"]);

    const phoneReport = await phone.engine.sync();
    expect(phoneReport.renamedLocally).toEqual([{ from: "Draft.md", to: "Final.md" }]);
    expect(phone.fs.renames).toEqual([{ from: "Draft.md", to: "Final.md" }]);
    expect(phone.fs.trashed).toEqual([]);
    expectSameFiles(pc, phone);
  });

  it("keeps both when one device renames and the other edits", async () => {
    pc.fs.setText("Draft.md", "original");
    await syncAll(pc, phone);

    pc.fs.move("Draft.md", "Final.md");
    phone.fs.setText("Draft.md", "edited");
    await syncAll(pc, phone);
    expectSameFiles(pc, phone);
    expect(pc.fs.snapshot()).toEqual({ "Draft.md": "edited", "Final.md": "original" });
  });

  it("renames that only change letter case", async () => {
    pc.fs.setText("note.md", "text");
    await syncAll(pc, phone);
    pc.fs.move("note.md", "Note.md");
    await syncAll(pc, phone);
    expect(phone.fs.paths()).toEqual(["Note.md"]);
  });
});

describe("what gets synced", () => {
  it("never syncs settings, trash or git folders", async () => {
    pc.fs.setText(".obsidian/app.json", "{}");
    pc.fs.setText(".trash/old.md", "old");
    pc.fs.setText(".git/HEAD", "ref");
    pc.fs.setText("Note.md", "note");
    await pc.engine.sync();
    expect(server.paths()).toEqual(["Note.md"]);
  });

  it("honours extra excluded paths", async () => {
    const custom = makeDevice(server, VAULT, "Custom", { excludedPaths: ["Private/"] });
    custom.fs.setText("Private/diary.md", "secret");
    custom.fs.setText("Public.md", "hello");
    await custom.engine.sync();
    expect(server.paths()).toEqual(["Public.md"]);
  });

  it("skips files it can't sync, and never deletes them from the server", async () => {
    const small = makeDevice(server, VAULT, "Small", { maxFileBytes: 10 });
    small.fs.setText("ok.md", "short");
    small.fs.setText("big.md", "this is far too long");
    small.fs.setText("what?.md", "bad name");
    const report = await small.engine.sync();
    expect(server.paths()).toEqual(["ok.md"]);
    expect(report.skipped.map((s) => s.path).sort()).toEqual(["big.md", "what?.md"]);

    // A synced file that grows past the limit is left alone, not deleted.
    small.fs.setText("ok.md", "now much longer than ten bytes");
    const again = await small.engine.sync();
    expect(again.deletedRemotely).toEqual([]);
    expect(server.paths()).toEqual(["ok.md"]);
  });

  it("skips a new file whose name differs only by letter case", async () => {
    pc.fs.setText("Note.md", "one");
    await pc.engine.sync();
    pc.fs.setText("note.md", "two");
    const report = await pc.engine.sync();
    expect(report.skipped).toEqual([
      { path: "note.md", reason: 'differs only by letter case from "Note.md"' },
    ]);
    expect(server.paths()).toEqual(["Note.md"]);
  });
});

describe("safety", () => {
  beforeEach(async () => {
    for (let i = 0; i < 40; i++) pc.fs.setText(`Note ${i}.md`, `note ${i}`);
    await syncAll(pc, phone);
  });

  it("pauses before deleting many files from the server", async () => {
    for (let i = 0; i < 30; i++) pc.fs.remove(`Note ${i}.md`);
    await expect(pc.engine.sync()).rejects.toMatchObject({ count: 30, where: "server" });
    expect(server.paths()).toHaveLength(40);

    const report = await pc.engine.sync({ allowMassDelete: true });
    expect(report.deletedRemotely).toHaveLength(30);
  });

  it("pauses before trashing many files on this device", async () => {
    for (let i = 0; i < 30; i++) pc.fs.remove(`Note ${i}.md`);
    await pc.engine.sync({ allowMassDelete: true });

    const error = await phone.engine.sync().catch((e: unknown) => e);
    expect(error).toBeInstanceOf(MassDeleteError);
    expect(phone.fs.paths()).toHaveLength(40);

    await phone.engine.sync({ allowMassDelete: true });
    expect(phone.fs.trashed).toHaveLength(30);
    expectSameFiles(pc, phone);
  });

  it("allows a few deletes without asking", async () => {
    for (let i = 0; i < 3; i++) pc.fs.remove(`Note ${i}.md`);
    await syncAll(pc, phone);
    expect(phone.fs.paths()).toHaveLength(37);
  });
});

/** Wraps the server so a test can run code at specific points. */
function hooked(target: ServerApi, hooks: Partial<Record<keyof ServerApi, () => Promise<void>>>): ServerApi {
  return {
    changes: async (...a) => (await hooks.changes?.(), target.changes(...a)),
    state: async (...a) => (await hooks.state?.(), target.state(...a)),
    missing: async (...a) => (await hooks.missing?.(), target.missing(...a)),
    upload: async (...a) => (await hooks.upload?.(), target.upload(...a)),
    download: async (...a) => (await hooks.download?.(), target.download(...a)),
    commit: async (...a: [number, CommitChange[]]) => (await hooks.commit?.(), target.commit(...a)),
  };
}

describe("races and interruptions", () => {
  it("retries when another device commits during a sync", async () => {
    pc.fs.setText("Shared.md", "line 1\nline 2\nline 3\n");
    await syncAll(pc, phone);

    let interfered = false;
    const racy = makeDevice(
      hooked(server, {
        commit: async () => {
          if (interfered) return;
          interfered = true;
          phone.fs.setText("Shared.md", "line 1\nline 2\nline 3 from phone\n");
          await phone.engine.sync();
        },
      }),
      VAULT,
      "Racy",
    );
    // Give the racy device the same starting point as the PC.
    racy.fs.setText("Shared.md", "line 1\nline 2\nline 3\n");
    await racy.engine.sync();
    racy.fs.setText("Shared.md", "line 1 from racy\nline 2\nline 3\n");

    const report = await racy.engine.sync();
    expect(interfered).toBe(true);
    expect(report.merged).toEqual(["Shared.md"]);
    await syncAll(racy, phone);
    expectSameFiles(racy, phone);
    expect(phone.fs.text("Shared.md")).toBe("line 1 from racy\nline 2\nline 3 from phone\n");
  });

  it("never overwrites a file edited while it was downloading", async () => {
    pc.fs.setText("Note.md", "v1");
    await syncAll(pc, phone);
    pc.fs.setText("Note.md", "v2 from pc");
    await pc.engine.sync();

    let typed = false;
    const slowPhone = makeDevice(
      hooked(server, {
        download: async () => {
          if (typed) return;
          typed = true;
          phone.fs.setText("Note.md", "typed during sync");
        },
      }),
      VAULT,
      "Phone",
      {},
      phone,
    );

    await slowPhone.engine.sync();
    expect(typed).toBe(true);
    // The typed text was kept; it conflicts with the PC's edit, so both survive.
    await syncAll(pc, slowPhone);
    const all = Object.values(pc.fs.snapshot());
    expect(all).toContain("typed during sync");
    expect(all).toContain("v2 from pc");
    expectSameFiles(pc, slowPhone);
  });

  it("recovers from a sync interrupted before its commit", async () => {
    let failNext = true;
    const flaky = makeDevice(
      hooked(server, {
        commit: async () => {
          if (failNext) {
            failNext = false;
            throw new Error("network down");
          }
        },
      }),
      VAULT,
      "Flaky",
    );
    flaky.fs.setText("A.md", "a");
    flaky.fs.setText("B.md", "b");
    await expect(flaky.engine.sync()).rejects.toThrow("network down");
    expect(server.paths()).toEqual([]);

    const report = await flaky.engine.sync();
    expect(report.uploaded.sort()).toEqual(["A.md", "B.md"]);
    await phone.engine.sync();
    expectSameFiles(flaky, phone);
  });

  it("runs overlapping sync calls one after another", async () => {
    pc.fs.setText("A.md", "a");
    const [first, second, third] = await Promise.all([
      pc.engine.sync(),
      pc.engine.sync(),
      pc.engine.sync(),
    ]);
    expect(first.uploaded).toEqual(["A.md"]);
    expect(second).toBe(third);
    expect(second.uploaded).toEqual([]);
  });

  it("surfaces server errors it can't retry", async () => {
    const broken = makeDevice(
      hooked(server, {
        changes: async () => {
          throw new ServerError(401, "unauthorized", "authentication required");
        },
        state: async () => {
          throw new ServerError(401, "unauthorized", "authentication required");
        },
      }),
      VAULT,
      "Broken",
    );
    await expect(broken.engine.sync()).rejects.toMatchObject({ code: "unauthorized" });
  });
});
