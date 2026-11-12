import { describe, expect, it } from "vitest";

import { sha256Hex } from "../src/engine/hash";
import { Sha256 } from "../src/engine/sha256";

const encoder = new TextEncoder();

function bytes(n: number, seed = 7): Uint8Array {
  const data = new Uint8Array(n);
  let x = seed;
  for (let i = 0; i < n; i++) {
    x = (x * 1103515245 + 12345) >>> 0;
    data[i] = x >>> 24;
  }
  return data;
}

describe("Sha256", () => {
  it("matches known digests", () => {
    expect(new Sha256().hex()).toBe(
      "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
    );
    expect(new Sha256().update(encoder.encode("abc")).hex()).toBe(
      "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
    );
  });

  it("matches Web Crypto for every length around block edges", async () => {
    for (const n of [1, 55, 56, 63, 64, 65, 119, 120, 127, 128, 129, 1000, 70_001]) {
      const data = bytes(n, n);
      expect(new Sha256().update(data).hex(), `length ${n}`).toBe(await sha256Hex(data));
    }
  });

  it("gives the same result however the input is split", async () => {
    const data = bytes(10_000);
    const expected = await sha256Hex(data);
    for (const piece of [1, 7, 63, 64, 65, 4096]) {
      const hash = new Sha256();
      for (let i = 0; i < data.length; i += piece) hash.update(data.subarray(i, i + piece));
      expect(hash.hex(), `pieces of ${piece}`).toBe(expected);
    }
  });

  it("continues after hex(), save() and restore()", async () => {
    const data = bytes(5000);
    const hash = new Sha256().update(data.subarray(0, 1234));
    hash.hex();
    const saved = JSON.parse(JSON.stringify(hash.save()));
    const resumed = Sha256.restore(saved).update(data.subarray(1234));
    expect(resumed.hex()).toBe(await sha256Hex(data));
  });

  it("rejects a damaged saved state", () => {
    expect(() => Sha256.restore({ h: [1, 2], pending: "", length: 0 })).toThrow();
    expect(() => Sha256.restore({ h: new Array(8).fill(0), pending: "abc", length: 0 })).toThrow();
  });
});
