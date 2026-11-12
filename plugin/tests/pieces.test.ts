// Large files go a piece at a time, and resume after an interruption.

import { beforeEach, describe, expect, it } from "vitest";

import { PIECE_BYTES } from "../src/engine/engine";
import type { ServerApi } from "../src/engine/types";
import { makeDevice, type Device } from "./support/devices";
import { MemoryServer } from "./support/memory";

const VAULT = 1;
/** Two and a half pieces. */
const BIG = PIECE_BYTES * 2 + PIECE_BYTES / 2;

function bytes(n: number, seed: number): Uint8Array {
  const data = new Uint8Array(n);
  let x = seed;
  for (let i = 0; i < n; i += 4096) {
    x = (x * 1103515245 + 12345) >>> 0;
    data.fill(x >>> 24, i, i + 4096);
  }
  return data;
}

function equal(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((v, i) => v === b[i]);
}

/** Counts calls to a server method and can fail one of them. */
function counting(target: ServerApi, method: "downloadRange" | "uploadPart", failOn?: number) {
  let calls = 0;
  const api = Object.create(target) as ServerApi;
  const original = target[method].bind(target) as (...a: unknown[]) => Promise<unknown>;
  (api as unknown as Record<string, unknown>)[method] = async (...a: unknown[]) => {
    calls++;
    if (calls === failOn) throw new Error("connection lost");
    return original(...a);
  };
  return { api, calls: () => calls };
}

let server: MemoryServer;
let pc: Device;
let phone: Device;

beforeEach(() => {
  server = new MemoryServer();
  pc = makeDevice(server, VAULT, "PC");
  phone = makeDevice(server, VAULT, "Phone");
});

describe("large files", () => {
  it("are uploaded and downloaded in pieces", async () => {
    const video = bytes(BIG, 1);
    pc.fs.setBytes("Media/video.mp4", video);
    pc.fs.setText("Note.md", "small files work as before");
    await pc.engine.sync();
    expect(server.largestTransfer).toBeLessThanOrEqual(PIECE_BYTES);

    const report = await phone.engine.sync();
    expect(report.downloaded.sort()).toEqual(["Media/video.mp4", "Note.md"]);
    expect(equal(await phone.fs.read("Media/video.mp4"), video)).toBe(true);
    expect(phone.fs.largestWrite).toBeLessThanOrEqual(PIECE_BYTES);
    expect(server.largestTransfer).toBeLessThanOrEqual(PIECE_BYTES);
    expect(phone.fs.scratch.size).toBe(0);

    // Nothing left to do afterwards.
    const again = await phone.engine.sync();
    expect(again).toMatchObject({ uploaded: [], downloaded: [] });
  });

  it("resume a download that was cut off", async () => {
    const video = bytes(BIG, 2);
    pc.fs.setBytes("video.mp4", video);
    await pc.engine.sync();

    const flaky = counting(server, "downloadRange", 2);
    const cut = makeDevice(flaky.api, VAULT, "Phone");
    await expect(cut.engine.sync()).rejects.toThrow("connection lost");
    expect(cut.fs.has("video.mp4")).toBe(false);
    expect(cut.fs.scratch.get(`${[...cut.fs.scratch.keys()][0]}`)).toBeDefined();

    // The next sync asks only for the pieces it doesn't have.
    const counted = counting(server, "downloadRange");
    const resumed = makeDevice(counted.api, VAULT, "Phone", {}, cut);
    await resumed.engine.sync();
    expect(counted.calls()).toBe(2);
    expect(equal(await resumed.fs.read("video.mp4"), video)).toBe(true);
    expect(resumed.fs.scratch.size).toBe(0);
  });

  it("start a download again if the staged copy is out of step", async () => {
    const video = bytes(BIG, 3);
    pc.fs.setBytes("video.mp4", video);
    await pc.engine.sync();

    const flaky = counting(server, "downloadRange", 3);
    const cut = makeDevice(flaky.api, VAULT, "Phone");
    await expect(cut.engine.sync()).rejects.toThrow("connection lost");
    // As if the app stopped after adding a piece but before noting it.
    const part = [...cut.fs.scratch.keys()].find((n) => n.endsWith(".part"))!;
    await cut.fs.staging.append(part, new Uint8Array(10));

    const resumed = makeDevice(server, VAULT, "Phone", {}, cut);
    await resumed.engine.sync();
    expect(equal(await resumed.fs.read("video.mp4"), video)).toBe(true);
  });

  it("resume an upload that was cut off", async () => {
    const video = bytes(BIG, 4);
    const flaky = counting(server, "uploadPart", 2);
    const cut = makeDevice(flaky.api, VAULT, "PC");
    cut.fs.setBytes("video.mp4", video);
    await expect(cut.engine.sync()).rejects.toThrow("connection lost");

    const counted = counting(server, "uploadPart");
    const resumed = makeDevice(counted.api, VAULT, "PC", {}, cut);
    await resumed.engine.sync();
    expect(counted.calls()).toBe(2);

    await phone.engine.sync();
    expect(equal(await phone.fs.read("video.mp4"), video)).toBe(true);
  });

  it("work with a server older than 0.5.0", async () => {
    server.legacy = true;
    const video = bytes(BIG, 5);
    pc.fs.setBytes("video.mp4", video);
    await pc.engine.sync();
    await phone.engine.sync();
    expect(equal(await phone.fs.read("video.mp4"), video)).toBe(true);
    expect(phone.fs.scratch.size).toBe(0);
  });

  it("keep both versions when both devices changed one", async () => {
    pc.fs.setBytes("video.mp4", bytes(BIG, 6));
    await pc.engine.sync();
    await phone.engine.sync();

    const fromPc = bytes(BIG, 7);
    const fromPhone = bytes(BIG + 100, 8);
    pc.fs.setBytes("video.mp4", fromPc);
    await pc.engine.sync();
    phone.fs.setBytes("video.mp4", fromPhone);
    const report = await phone.engine.sync();

    const copy = "video (conflict 2026-11-06 1432 Phone).mp4";
    expect(report.conflictCopies).toEqual([copy]);
    expect(equal(await phone.fs.read("video.mp4"), fromPc)).toBe(true);
    expect(equal(await phone.fs.read(copy), fromPhone)).toBe(true);
    expect(phone.fs.largestWrite).toBeLessThanOrEqual(PIECE_BYTES);

    await pc.engine.sync();
    expect(equal(await pc.fs.read(copy), fromPhone)).toBe(true);
  });
});
