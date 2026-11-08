import { describe, expect, it } from "vitest";

import { decodeText, mergeText } from "../src/engine/merge";

const base = ["# Plan", "", "- groceries", "- laundry", "- call mum", ""].join("\n");

describe("mergeText", () => {
  it("combines edits to different lines", () => {
    const local = base.replace("- groceries", "- groceries (milk, eggs)");
    const remote = base.replace("- call mum", "- call mum and dad");
    expect(mergeText(base, local, remote)).toEqual({
      clean: true,
      text: ["# Plan", "", "- groceries (milk, eggs)", "- laundry", "- call mum and dad", ""].join("\n"),
    });
  });

  it("combines additions at different ends", () => {
    const local = `Intro line\n${base}`;
    const remote = `${base}Footer line\n`;
    const result = mergeText(base, local, remote);
    expect(result.clean).toBe(true);
    if (result.clean) {
      expect(result.text.startsWith("Intro line\n# Plan")).toBe(true);
      expect(result.text.endsWith("Footer line\n")).toBe(true);
    }
  });

  it("accepts the same edit on both sides", () => {
    const edited = base.replace("- laundry", "- laundry (done)");
    expect(mergeText(base, edited, edited)).toEqual({ clean: true, text: edited });
  });

  it("reports overlapping edits as a conflict", () => {
    const local = base.replace("- laundry", "- laundry today");
    const remote = base.replace("- laundry", "- laundry tomorrow");
    expect(mergeText(base, local, remote)).toEqual({ clean: false });
  });

  it("handles an empty base", () => {
    expect(mergeText("", "local text", "remote text")).toEqual({ clean: false });
    expect(mergeText("", "same", "same")).toEqual({ clean: true, text: "same" });
  });

  it("keeps Windows line endings when the local copy uses them", () => {
    const crlfBase = base.replace(/\n/g, "\r\n");
    const local = crlfBase.replace("- groceries", "- groceries!");
    const remote = base.replace("- call mum", "- call mum!");
    const result = mergeText(crlfBase, local, remote);
    expect(result.clean).toBe(true);
    if (result.clean) {
      expect(result.text).toContain("- groceries!\r\n");
      expect(result.text).toContain("- call mum!\r\n");
      expect(result.text.replace(/\r\n/g, "")).not.toContain("\n");
    }
  });
});

describe("decodeText", () => {
  it("decodes UTF-8 and rejects binary", () => {
    expect(decodeText(new TextEncoder().encode("Café ☕"))).toBe("Café ☕");
    expect(decodeText(new Uint8Array([0xff, 0xfe, 0x00, 0x80]))).toBeNull();
  });
});
