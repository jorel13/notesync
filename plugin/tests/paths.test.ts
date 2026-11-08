import { describe, expect, it } from "vitest";

import { conflictCopyPath, extension, isExcluded, pathProblem } from "../src/engine/paths";

describe("pathProblem", () => {
  it("accepts normal vault paths", () => {
    for (const path of ["Note.md", "Daily/2026-11-06.md", "Attachments/photo 1.png", "Ideas/Café ☕.md", "Aux notes.md"]) {
      expect(pathProblem(path), path).toBeNull();
    }
  });

  it("rejects paths the server or Windows can't store", () => {
    for (const path of [
      "",
      "/abs.md",
      "a//b.md",
      "../x.md",
      "what?.md",
      "colon:.md",
      "back\\slash.md",
      "trailing.",
      "trailing /x.md",
      "con.md",
      "folder/LPT1.txt",
      "Café.md",
      "a".repeat(1025),
    ]) {
      expect(pathProblem(path), JSON.stringify(path)).not.toBeNull();
    }
  });
});

describe("isExcluded", () => {
  const prefixes = [".obsidian/", "Private/", "secret.md"];

  it("matches folders and exact files", () => {
    expect(isExcluded(".obsidian/app.json", prefixes)).toBe(true);
    expect(isExcluded(".obsidian", prefixes)).toBe(true);
    expect(isExcluded("Private/diary.md", prefixes)).toBe(true);
    expect(isExcluded("secret.md", prefixes)).toBe(true);
  });

  it("doesn't match similar names", () => {
    expect(isExcluded("Private notes.md", prefixes)).toBe(false);
    expect(isExcluded("Folder/secret.md", prefixes)).toBe(false);
    expect(isExcluded(".obsidian-backup/x.md", prefixes)).toBe(false);
  });
});

describe("extension", () => {
  it("returns the lower-cased extension", () => {
    expect(extension("Folder/Note.MD")).toBe("md");
    expect(extension("archive.tar.gz")).toBe("gz");
    expect(extension("Makefile")).toBe("");
    expect(extension(".hidden")).toBe("");
    expect(extension("dir.v2/file")).toBe("");
  });
});

describe("conflictCopyPath", () => {
  const when = new Date(2026, 10, 6, 14, 32);

  it("adds the date and device before the extension", () => {
    expect(conflictCopyPath("Folder/Note.md", "Pixel", when, () => false)).toBe(
      "Folder/Note (conflict 2026-11-06 1432 Pixel).md",
    );
    expect(conflictCopyPath("README", "PC", when, () => false)).toBe(
      "README (conflict 2026-11-06 1432 PC)",
    );
  });

  it("numbers copies when the name is taken", () => {
    const taken = new Set(["Note (conflict 2026-11-06 1432 PC).md"]);
    expect(conflictCopyPath("Note.md", "PC", when, (p) => taken.has(p))).toBe(
      "Note (conflict 2026-11-06 1432 PC 2).md",
    );
  });

  it("makes device names safe for file names", () => {
    const path = conflictCopyPath("Note.md", "Jo's PC: work/home", when, () => false);
    expect(pathProblem(path)).toBeNull();
  });
});
