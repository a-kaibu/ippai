// BLAKE3 for store chunk names: the same function as host/fetch-bridge.js"s ippaiBlake3 (that
// one runs as a plain script in the network worker, so it has its own copy); both are checked
// against the blake3 crate"s answers (testdata/blake3.tsv).

/**
 * The first 16 bytes of `data`"s BLAKE3 hash, as hex: a store chunk"s name (crates/ippai/src/pack.rs).
 * One-shot (chunks are 256 KiB at most). ~230 MB/s in desktop Chromium.
 */
export function blake3Hex(data: Uint8Array): string {
  const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const PERM = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
  const S: number[][] = [];
  let s = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
  for (let r = 0; r < 7; r++) {
    S.push(s);
    const prev = s;
    s = PERM.map((p) => prev[p]!);
  }
  const bytes = new Uint8Array(64);
  const m = new Uint32Array(bytes.buffer);
  const out = new Uint32Array(16);
  // the compression function: out = the 16 words for chaining value `h` (8 words at `ho`), block m
  const compress = (h: ArrayLike<number>, ho: number, counter: number, len: number, flags: number) => {
    let v0 = h[ho],
      v1 = h[ho + 1],
      v2 = h[ho + 2],
      v3 = h[ho + 3],
      v4 = h[ho + 4],
      v5 = h[ho + 5],
      v6 = h[ho + 6],
      v7 = h[ho + 7];
    let v8 = IV[0],
      v9 = IV[1],
      v10 = IV[2],
      v11 = IV[3],
      v12 = counter | 0,
      v13 = (counter / 0x100000000) | 0,
      v14 = len,
      v15 = flags;
    for (let r = 0; r < 7; r++) {
      const q = S[r];
      // columns
      v0 = (v0 + v4 + m[q[0]]) | 0;
      v12 ^= v0;
      v12 = (v12 >>> 16) | (v12 << 16);
      v8 = (v8 + v12) | 0;
      v4 ^= v8;
      v4 = (v4 >>> 12) | (v4 << 20);
      v0 = (v0 + v4 + m[q[1]]) | 0;
      v12 ^= v0;
      v12 = (v12 >>> 8) | (v12 << 24);
      v8 = (v8 + v12) | 0;
      v4 ^= v8;
      v4 = (v4 >>> 7) | (v4 << 25);
      v1 = (v1 + v5 + m[q[2]]) | 0;
      v13 ^= v1;
      v13 = (v13 >>> 16) | (v13 << 16);
      v9 = (v9 + v13) | 0;
      v5 ^= v9;
      v5 = (v5 >>> 12) | (v5 << 20);
      v1 = (v1 + v5 + m[q[3]]) | 0;
      v13 ^= v1;
      v13 = (v13 >>> 8) | (v13 << 24);
      v9 = (v9 + v13) | 0;
      v5 ^= v9;
      v5 = (v5 >>> 7) | (v5 << 25);
      v2 = (v2 + v6 + m[q[4]]) | 0;
      v14 ^= v2;
      v14 = (v14 >>> 16) | (v14 << 16);
      v10 = (v10 + v14) | 0;
      v6 ^= v10;
      v6 = (v6 >>> 12) | (v6 << 20);
      v2 = (v2 + v6 + m[q[5]]) | 0;
      v14 ^= v2;
      v14 = (v14 >>> 8) | (v14 << 24);
      v10 = (v10 + v14) | 0;
      v6 ^= v10;
      v6 = (v6 >>> 7) | (v6 << 25);
      v3 = (v3 + v7 + m[q[6]]) | 0;
      v15 ^= v3;
      v15 = (v15 >>> 16) | (v15 << 16);
      v11 = (v11 + v15) | 0;
      v7 ^= v11;
      v7 = (v7 >>> 12) | (v7 << 20);
      v3 = (v3 + v7 + m[q[7]]) | 0;
      v15 ^= v3;
      v15 = (v15 >>> 8) | (v15 << 24);
      v11 = (v11 + v15) | 0;
      v7 ^= v11;
      v7 = (v7 >>> 7) | (v7 << 25);
      // diagonals
      v0 = (v0 + v5 + m[q[8]]) | 0;
      v15 ^= v0;
      v15 = (v15 >>> 16) | (v15 << 16);
      v10 = (v10 + v15) | 0;
      v5 ^= v10;
      v5 = (v5 >>> 12) | (v5 << 20);
      v0 = (v0 + v5 + m[q[9]]) | 0;
      v15 ^= v0;
      v15 = (v15 >>> 8) | (v15 << 24);
      v10 = (v10 + v15) | 0;
      v5 ^= v10;
      v5 = (v5 >>> 7) | (v5 << 25);
      v1 = (v1 + v6 + m[q[10]]) | 0;
      v12 ^= v1;
      v12 = (v12 >>> 16) | (v12 << 16);
      v11 = (v11 + v12) | 0;
      v6 ^= v11;
      v6 = (v6 >>> 12) | (v6 << 20);
      v1 = (v1 + v6 + m[q[11]]) | 0;
      v12 ^= v1;
      v12 = (v12 >>> 8) | (v12 << 24);
      v11 = (v11 + v12) | 0;
      v6 ^= v11;
      v6 = (v6 >>> 7) | (v6 << 25);
      v2 = (v2 + v7 + m[q[12]]) | 0;
      v13 ^= v2;
      v13 = (v13 >>> 16) | (v13 << 16);
      v8 = (v8 + v13) | 0;
      v7 ^= v8;
      v7 = (v7 >>> 12) | (v7 << 20);
      v2 = (v2 + v7 + m[q[13]]) | 0;
      v13 ^= v2;
      v13 = (v13 >>> 8) | (v13 << 24);
      v8 = (v8 + v13) | 0;
      v7 ^= v8;
      v7 = (v7 >>> 7) | (v7 << 25);
      v3 = (v3 + v4 + m[q[14]]) | 0;
      v14 ^= v3;
      v14 = (v14 >>> 16) | (v14 << 16);
      v9 = (v9 + v14) | 0;
      v4 ^= v9;
      v4 = (v4 >>> 12) | (v4 << 20);
      v3 = (v3 + v4 + m[q[15]]) | 0;
      v14 ^= v3;
      v14 = (v14 >>> 8) | (v14 << 24);
      v9 = (v9 + v14) | 0;
      v4 ^= v9;
      v4 = (v4 >>> 7) | (v4 << 25);
    }
    out[0] = v0 ^ v8;
    out[1] = v1 ^ v9;
    out[2] = v2 ^ v10;
    out[3] = v3 ^ v11;
    out[4] = v4 ^ v12;
    out[5] = v5 ^ v13;
    out[6] = v6 ^ v14;
    out[7] = v7 ^ v15;
  };
  // (words are little-endian: m is a Uint32Array over the block"s bytes, as every browser is)
  const chunk = (i: number, root: boolean) => {
    const at = i * 1024;
    const end = Math.min(data.length, at + 1024);
    const blocks = Math.max(1, Math.ceil((end - at) / 64));
    const cv = new Uint32Array(IV);
    for (let b = 0; b < blocks; b++) {
      const from = at + b * 64;
      const n = Math.min(64, end - from);
      if (n === 64) bytes.set(data.subarray(from, from + 64));
      else {
        bytes.fill(0);
        if (n > 0) bytes.set(data.subarray(from, end));
      }
      const last = b === blocks - 1;
      compress(cv, 0, i, Math.max(n, 0), (b === 0 ? 1 : 0) | (last ? 2 : 0) | (last && root ? 8 : 0));
      cv.set(out.subarray(0, 8));
    }
    return cv;
  };
  const parent = (l: Uint32Array, r: Uint32Array, root: boolean) => {
    m.set(l);
    m.set(r, 8);
    compress(IV, 0, 0, 64, 4 | (root ? 8 : 0));
    return out.slice(0, 8);
  };
  const n = Math.max(1, Math.ceil(data.length / 1024));
  let cv: Uint32Array;
  if (n === 1) cv = chunk(0, true);
  else {
    const stack: Uint32Array[] = [];
    for (let i = 0; i < n - 1; i++) {
      let c = chunk(i, false);
      for (let t = i + 1; (t & 1) === 0; t >>= 1) c = parent(stack.pop()!, c, false);
      stack.push(c);
    }
    cv = chunk(n - 1, false);
    while (stack.length > 1) cv = parent(stack.pop()!, cv, false);
    cv = parent(stack.pop()!, cv, true);
  }
  let hex = "";
  for (let k = 0; k < 16; k++) hex += ((cv[k >> 2]! >>> ((k & 3) * 8)) & 255).toString(16).padStart(2, "0");
  return hex;
}
