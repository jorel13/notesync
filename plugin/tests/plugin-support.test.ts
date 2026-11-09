import { describe, expect, it } from "vitest";
import manifest from "../manifest.json";
import { apiBase, CLIENT_VERSION, serverUrlProblem } from "../src/api/http";
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

describe("server address", () => {
  it("requires https except on this computer", () => {
    expect(serverUrlProblem("https://notes.example.ts.net")).toBeNull();
    expect(serverUrlProblem("http://127.0.0.1:8080")).toBeNull();
    expect(serverUrlProblem("http://localhost:8080")).toBeNull();
    expect(serverUrlProblem("http://[::1]:8080")).toBeNull();
    expect(serverUrlProblem("http://notes.example.ts.net")).toMatch(/https/);
    expect(serverUrlProblem("http://192.168.1.10:30080")).toMatch(/https/);
    expect(serverUrlProblem("http://localhost.evil.com")).toMatch(/https/);
    expect(serverUrlProblem("notes.example.ts.net")).toMatch(/valid URL/);
  });

  it("refuses to build requests to an insecure address", () => {
    expect(() => apiBase("http://192.168.1.10")).toThrow(/https/);
    expect(apiBase("https://notes.example.ts.net/")).toBe("https://notes.example.ts.net/api/v1");
  });

  it("reports the plugin's real version to the server", () => {
    expect(CLIENT_VERSION).toBe(manifest.version);
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
