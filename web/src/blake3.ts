// BLAKE3 for store chunk names: the same function as host/fetch-bridge.js"s masuBlake3 (that
// one runs as a plain script in the network worker, so it has its own copy); both are checked
// against the blake3 crate"s answers (testdata/blake3.tsv).

/**
 * The first 16 bytes of `data`"s BLAKE3 hash, as hex: a store chunk"s name (crates/masu/src/pack.rs).
 * One-shot (chunks are 256 KiB at most). ~230 MB/s in desktop Chromium.
 */
export function blake3Hex(data: Uint8Array): string {
  // (wasm SIMD where there is: ~6-10x blake3Scalar's speed, which ran on the page's thread for
  // every chunk the read-ahead and the offline download fetched)
  const fast = data.length > 1024 && blake3Simd();
  return fast ? fast(data) : blake3Scalar(data);
}

/** blake3Hex in plain JavaScript (where there is no wasm SIMD, and small inputs). */
export function blake3Scalar(data: Uint8Array): string {
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

let simdMade = false;
let simdHash: ((data: Uint8Array) => string) | null = null;
let simdMemory: WebAssembly.Memory | null = null;

/** Tests: the SIMD module's memory, in bytes (0 before it is made). */
export const simdMemoryBytes = () => simdMemory?.buffer.byteLength ?? 0;

const uleb = (v: number): number[] => {
  const o: number[] = [];
  do {
    let b = v & 0x7f;
    v >>>= 7;
    if (v) b |= 0x80;
    o.push(b);
  } while (v);
  return o;
};
const sleb = (v: number): number[] => {
  const o: number[] = [];
  for (;;) {
    const b = v & 0x7f;
    v >>= 7;
    if ((v === 0 && !(b & 0x40)) || (v === -1 && b & 0x40)) {
      o.push(b);
      return o;
    }
    o.push(b | 0x80);
  }
};

/**
 * BLAKE3 with wasm SIMD (blake3Hex's fast path; host/fetch-bridge.js masuBlake3Simd makes the
 * same module): `h(in, stride, n, blocks, counter, step, flags, start, end, lastLen, out)`
 * hashes `n` (1-4) inputs at once, one per i32x4 lane (input l at in + l*stride: `blocks`
 * 64-byte blocks, the last `lastLen` bytes long, zero-padded; counter + l*step; `flags` on every
 * block, `start` on the first, `end` on the last) and stores each one's chaining value (8 words)
 * at out + l*32. Whole chunks go four at a time, then the tree's parents four at a time, level
 * by level. Returns hash(data) -> the first 16 bytes as hex, or null without wasm SIMD.
 */
export function blake3Simd(): ((data: Uint8Array) => string) | null {
  if (simdMade) return simdHash;
  simdMade = true;
  const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const PERM = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
  // params 0-10 (i32): in stride n blocks counter step flags start end lastLen out; i32 locals
  // 11-16: a0-a3 (each lane's address), b (the block), f (its flags); v128 locals from 17: h0-7,
  // v0-15, m0-15, ctr, a scratch
  const [IN, STRIDE, N, BLOCKS, COUNTER, STEP, FLAGS, START, END, LASTLEN, OUT] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10,
  ];
  const A = 11,
    B = 15,
    F = 16,
    H = 17,
    V = 25,
    M = 41,
    CTR = 57,
    T = 58;
  const get = (i: number) => [0x20, ...uleb(i)];
  const set = (i: number) => [0x21, ...uleb(i)];
  const i32 = (v: number) => [0x41, ...sleb(v | 0)];
  const simd = (op: number, ...imm: number[]) => [0xfd, ...uleb(op), ...imm];
  const ZERO = simd(0x0c, ...Array.from({ length: 16 }, () => 0));
  const ADD = simd(0xae),
    XOR = simd(0x51),
    OR = simd(0x50),
    SPLAT = simd(0x11);
  const SUB = [0x6b],
    EQ = [0x46],
    SELECT = [0x1b];
  const ROT16: number[] = [];
  const ROT8: number[] = [];
  for (let i = 0; i < 4; i++) {
    ROT16.push(4 * i + 2, 4 * i + 3, 4 * i, 4 * i + 1);
    ROT8.push(4 * i + 1, 4 * i + 2, 4 * i + 3, 4 * i);
  }
  // the value on the stack rotated right by r
  const rot = (r: number) =>
    r === 16 || r === 8
      ? [...set(T), ...get(T), ...get(T), ...simd(0x0d, ...(r === 16 ? ROT16 : ROT8))]
      : [...set(T), ...get(T), ...i32(r), ...simd(0xad), ...get(T), ...i32(32 - r), ...simd(0xab), ...OR];
  // x = x + y + m; then d = rotr(d ^ x, r1), c = c + d, y = rotr(y ^ c, r2)
  const half = (a: number, b: number, c: number, d: number, m: number, r1: number, r2: number) => [
    ...get(V + a),
    ...get(V + b),
    ...ADD,
    ...get(M + m),
    ...ADD,
    ...set(V + a),
    ...get(V + d),
    ...get(V + a),
    ...XOR,
    ...rot(r1),
    ...set(V + d),
    ...get(V + c),
    ...get(V + d),
    ...ADD,
    ...set(V + c),
    ...get(V + b),
    ...get(V + c),
    ...XOR,
    ...rot(r2),
    ...set(V + b),
  ];
  const g = (a: number, b: number, c: number, d: number, x: number, y: number) => [
    ...half(a, b, c, d, x, 16, 12),
    ...half(a, b, c, d, y, 8, 7),
  ];
  // (on the stack: x if b == blocks - 1, else y)
  const ifLast = (x: number[], y: number[]) => [
    ...x,
    ...y,
    ...get(B),
    ...get(BLOCKS),
    ...i32(1),
    ...SUB,
    ...EQ,
    ...SELECT,
  ];
  const c: number[] = [];
  // lane addresses: in + min(l, n - 1) * stride
  for (let l = 0; l < 4; l++) {
    const last = [...get(N), ...i32(1), ...SUB];
    c.push(
      ...get(IN),
      ...i32(l),
      ...last,
      ...i32(l),
      ...last,
      0x48,
      ...SELECT,
      ...get(STRIDE),
      0x6c,
      0x6a,
      ...set(A + l),
    );
  }
  // counters: counter + l * step
  c.push(...get(COUNTER), ...SPLAT, ...get(STEP), ...SPLAT);
  c.push(...simd(0x0c, ...[0, 1, 2, 3].flatMap((k) => [k, 0, 0, 0])), ...simd(0xb5), ...ADD, ...set(CTR));
  for (let k = 0; k < 8; k++) c.push(...i32(IV[k]!), ...SPLAT, ...set(H + k));
  c.push(...i32(0), ...set(B));
  c.push(0x02, 0x40, 0x03, 0x40); // block, loop: one block of every lane
  // f = flags | (b == 0 ? start : 0) | (b == blocks - 1 ? end : 0)
  c.push(...get(FLAGS), ...get(START), ...i32(0), ...get(B), 0x45, ...SELECT, 0x72);
  c.push(...ifLast(get(END), i32(0)), 0x72, ...set(F));
  // m_w: word w of each lane's block (all loaded before anything is stored)
  for (let w = 0; w < 16; w++) {
    c.push(...ZERO);
    for (let l = 0; l < 4; l++)
      c.push(
        ...set(T),
        ...get(A + l),
        ...get(B),
        ...i32(6),
        0x74,
        0x6a,
        ...get(T),
        ...simd(0x56, 2, ...uleb(4 * w), l),
      );
    c.push(...set(M + w));
  }
  for (let k = 0; k < 8; k++) c.push(...get(H + k), ...set(V + k));
  for (let k = 0; k < 4; k++) c.push(...i32(IV[k]!), ...SPLAT, ...set(V + 8 + k));
  c.push(...get(CTR), ...set(V + 12), ...ZERO, ...set(V + 13));
  c.push(...ifLast(get(LASTLEN), i32(64)), ...SPLAT, ...set(V + 14), ...get(F), ...SPLAT, ...set(V + 15));
  let s = [...Array(16).keys()];
  for (let r = 0; r < 7; r++) {
    const m = (k: number) => s[k]!;
    c.push(
      ...g(0, 4, 8, 12, m(0), m(1)),
      ...g(1, 5, 9, 13, m(2), m(3)),
      ...g(2, 6, 10, 14, m(4), m(5)),
      ...g(3, 7, 11, 15, m(6), m(7)),
    );
    c.push(
      ...g(0, 5, 10, 15, m(8), m(9)),
      ...g(1, 6, 11, 12, m(10), m(11)),
      ...g(2, 7, 8, 13, m(12), m(13)),
      ...g(3, 4, 9, 14, m(14), m(15)),
    );
    const prev = s;
    s = PERM.map((p) => prev[p]!);
  }
  for (let k = 0; k < 8; k++) c.push(...get(V + k), ...get(V + 8 + k), ...XOR, ...set(H + k));
  // b++, again while b < blocks
  c.push(...get(B), ...i32(1), 0x6a, ...set(B), ...get(B), ...get(BLOCKS), 0x48, 0x0d, 0x00);
  c.push(0x0b, 0x0b);
  // lane l's chaining value (l < n) to out + l*32
  for (let l = 0; l < 4; l++) {
    if (l) c.push(...get(N), ...i32(l), 0x4a, 0x04, 0x40);
    for (let k = 0; k < 8; k++)
      c.push(...get(OUT), ...get(H + k), ...simd(0x5a, 2, ...uleb(l * 32 + 4 * k), l));
    if (l) c.push(0x0b);
  }
  c.push(0x0b);
  const body = [0x02, 6, 0x7f, 42, 0x7b, ...c];
  const sec = (id: number, bytes: number[]) => [id, ...uleb(bytes.length), ...bytes];
  const name = (t: string) => [t.length, ...Array.from(t, (ch) => ch.charCodeAt(0))];
  const bytes = new Uint8Array([
    0x00,
    0x61,
    0x73,
    0x6d,
    0x01,
    0x00,
    0x00,
    0x00,
    ...sec(1, [1, 0x60, 11, ...Array.from({ length: 11 }, () => 0x7f), 0]),
    ...sec(3, [1, 0]),
    ...sec(5, [1, 0, 1]),
    ...sec(7, [2, ...name("h"), 0, 0, ...name("m"), 2, 0]),
    ...sec(10, [1, ...uleb(body.length), ...body]),
  ]);
  let inst: WebAssembly.Instance;
  try {
    inst = new WebAssembly.Instance(new WebAssembly.Module(bytes));
  } catch {
    return (simdHash = null);
  }
  const h = inst.exports.h as (...a: number[]) => void;
  const mem = inst.exports.m as WebAssembly.Memory;
  simdMemory = mem;
  simdHash = (data) => {
    const len = data.length;
    const n = Math.max(1, Math.ceil(len / 1024));
    // memory: the chunks' chaining values (2 MiB for a 64 MiB chunk), then a window of the
    // input that whole chunks are copied into 256 at a time (not all of it: the memory stays)
    const cvBytes = Math.ceil((n * 32) / 64) * 64;
    const WIN = 256;
    const need = cvBytes + WIN * 1024 + 64;
    if (mem.buffer.byteLength < need) mem.grow(Math.ceil((need - mem.buffer.byteLength) / 65536));
    const u8 = new Uint8Array(mem.buffer);
    const full = n - 1;
    for (let w = 0; w < full; w += WIN) {
      const k = Math.min(WIN, full - w);
      u8.set(data.subarray(w * 1024, (w + k) * 1024), cvBytes);
      for (let i = 0; i < k; i += 4)
        h(cvBytes + i * 1024, 1024, Math.min(4, k - i), 16, w + i, 1, 0, 1, 2, 64, (w + i) * 32);
    }
    // the last chunk (the root, when it is the only one), zero-padded
    const rest = len - full * 1024;
    const blocks = Math.max(1, Math.ceil(rest / 64));
    u8.set(data.subarray(full * 1024), cvBytes);
    u8.fill(0, cvBytes + rest, cvBytes + blocks * 64);
    h(cvBytes, 1024, 1, blocks, full, 0, 0, 1, n === 1 ? 10 : 2, rest - (blocks - 1) * 64, full * 32);
    // parents, level by level (an odd one out goes up as it is: BLAKE3's tree)
    let count = n;
    while (count > 2) {
      const pairs = count >> 1;
      for (let k = 0; k < pairs; k += 4) h(k * 64, 64, Math.min(4, pairs - k), 1, 0, 0, 4, 0, 0, 64, k * 32);
      if (count & 1) u8.copyWithin(pairs * 32, (count - 1) * 32, count * 32);
      count = pairs + (count & 1);
    }
    if (count === 2) h(0, 64, 1, 1, 0, 0, 12, 0, 0, 64, 0);
    let hex = "";
    for (let k = 0; k < 16; k++) hex += u8[k]!.toString(16).padStart(2, "0");
    return hex;
  };
  return simdHash;
}
