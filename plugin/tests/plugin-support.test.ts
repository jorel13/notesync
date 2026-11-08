import { describe, expect, it } from "vitest";
import { DEFAULT_SETTINGS, loadSettings, parseExcludedPaths } from "../src/obsidian/settings";
import { SELF_WRITE_WINDOW_MS, TrackingFs } from "../src/sync/tracking-fs";
import { MemoryFs } from "./support/memory";

describe("TrackingFs", () => {
  it("remembers paths the sync changed for a short time", async () => {
    let now = 1_000;
    const fs = new TrackingFs(new MemoryFs(), () => now);
    await fs.write("a.md", new TextEncoder().encode("hi"));
    await fs.rename("a.md", "b.md");
    expect(fs.isOwnChange("a.md")).toBe(true);
    expect(fs.isOwnChange("b.md")).toBe(true);
    expect(fs.isOwnChange("other.md")).toBe(false);

    now += SELF_WRITE_WINDOW_MS + 1;
    expect(fs.isOwnChange("b.md")).toBe(false);
  });

  it("passes reads and writes through", async () => {
    const inner = new MemoryFs();
    const fs = new TrackingFs(inner);
    await fs.write("x/y.md", new TextEncoder().encode("hello"));
    expect(new TextDecoder().decode(await fs.read("x/y.md"))).toBe("hello");
    expect((await fs.list()).map((f) => f.path)).toEqual(["x/y.md"]);
    await fs.trash("x/y.md");
    expect(await fs.stat("x/y.md")).toBeNull();
  });
});

describe("settings", () => {
  it("parses excluded paths", () => {
    expect(parseExcludedPaths("Private/\n\n  /Drafts/ \n# comment\nsecret.md\r\n")).toEqual([
      "Private/",
      "Drafts/",
      "secret.md",
    ]);
  });

  it("fills in defaults and ignores bad values", () => {
    expect(loadSettings(null)).toEqual(DEFAULT_SETTINGS);
    const loaded = loadSettings({
      serverUrl: "https://notes.example.ts.net",
      vaultId: 3,
      intervalMinutes: "ten",
      unknown: true,
    });
    expect(loaded.serverUrl).toBe("https://notes.example.ts.net");
    expect(loaded.vaultId).toBe(3);
    expect(loaded.intervalMinutes).toBe(DEFAULT_SETTINGS.intervalMinutes);
    expect("unknown" in loaded).toBe(false);
  });
});
