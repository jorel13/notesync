// SHA-256 that takes its input a piece at a time, so a large download can be
// checked without holding it all in memory. Web Crypto can only hash a whole
// buffer at once. The state can be saved and restored, so an interrupted
// download picks up where it stopped.

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
  0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
  0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
  0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
  0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
]);

const INITIAL = [
  0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/** What `save` returns: plain JSON. */
export interface Sha256State {
  h: number[];
  /** Bytes not yet hashed (fewer than 64), as hex. */
  pending: string;
  length: number;
}

export class Sha256 {
  private readonly h = new Uint32Array(INITIAL);
  private readonly block = new Uint8Array(64);
  private blockLength = 0;
  private length = 0;
  private readonly w = new Uint32Array(64);

  update(data: Uint8Array): this {
    let i = 0;
    this.length += data.length;
    if (this.blockLength > 0) {
      const take = Math.min(64 - this.blockLength, data.length);
      this.block.set(data.subarray(0, take), this.blockLength);
      this.blockLength += take;
      i = take;
      if (this.blockLength < 64) return this;
      this.compress(this.block, 0);
      this.blockLength = 0;
    }
    for (; i + 64 <= data.length; i += 64) this.compress(data, i);
    if (i < data.length) {
      this.block.set(data.subarray(i));
      this.blockLength = data.length - i;
    }
    return this;
  }

  /** The digest of everything so far, as lower-case hex. Hashing can continue after. */
  hex(): string {
    const copy = Sha256.restore(this.save());
    const bits = copy.length * 8;
    const padding = new Uint8Array(((copy.blockLength < 56 ? 56 : 120) - copy.blockLength) + 8);
    padding[0] = 0x80;
    const view = new DataView(padding.buffer);
    view.setUint32(padding.length - 8, Math.floor(bits / 0x100000000));
    view.setUint32(padding.length - 4, bits >>> 0);
    copy.update(padding);
    return Array.from(copy.h, (word) => word.toString(16).padStart(8, "0")).join("");
  }

  save(): Sha256State {
    return {
      h: Array.from(this.h),
      pending: Array.from(this.block.subarray(0, this.blockLength), (b) =>
        b.toString(16).padStart(2, "0"),
      ).join(""),
      length: this.length,
    };
  }

  static restore(state: Sha256State): Sha256 {
    const hash = new Sha256();
    if (
      !Array.isArray(state.h) ||
      state.h.length !== 8 ||
      typeof state.pending !== "string" ||
      state.pending.length >= 128 ||
      state.pending.length % 2 !== 0 ||
      !Number.isSafeInteger(state.length)
    ) {
      throw new Error("Invalid saved hash state.");
    }
    hash.h.set(state.h);
    for (let i = 0; i < state.pending.length; i += 2) {
      hash.block[i / 2] = parseInt(state.pending.slice(i, i + 2), 16);
    }
    hash.blockLength = state.pending.length / 2;
    hash.length = state.length;
    return hash;
  }

  private compress(data: Uint8Array, offset: number): void {
    const w = this.w;
    for (let t = 0; t < 16; t++) {
      const j = offset + t * 4;
      w[t] = (data[j] << 24) | (data[j + 1] << 16) | (data[j + 2] << 8) | data[j + 3];
    }
    for (let t = 16; t < 64; t++) {
      const a = w[t - 15];
      const b = w[t - 2];
      const s0 = ((a >>> 7) | (a << 25)) ^ ((a >>> 18) | (a << 14)) ^ (a >>> 3);
      const s1 = ((b >>> 17) | (b << 15)) ^ ((b >>> 19) | (b << 13)) ^ (b >>> 10);
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) | 0;
    }
    let [a, b, c, d, e, f, g, h] = this.h;
    for (let t = 0; t < 64; t++) {
      const S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21)) ^ ((e >>> 25) | (e << 7));
      const ch = (e & f) ^ (~e & g);
      const t1 = (h + S1 + ch + K[t] + w[t]) | 0;
      const S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19)) ^ ((a >>> 22) | (a << 10));
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) | 0;
      h = g;
      g = f;
      f = e;
      e = (d + t1) | 0;
      d = c;
      c = b;
      b = a;
      a = (t1 + t2) | 0;
    }
    const s = this.h;
    s[0] += a;
    s[1] += b;
    s[2] += c;
    s[3] += d;
    s[4] += e;
    s[5] += f;
    s[6] += g;
    s[7] += h;
  }
}
