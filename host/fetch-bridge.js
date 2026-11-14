// The page owns a network worker and gives each game pthread a MessagePort.
// The network worker never relies on a blocked pthread's event loop. A bounded shared buffer transfers one response, with explicit acknowledgement.
//
// Mobile links drop, stall and come back: a request is not given up on the first failure. The
// worker retries network errors, stalled bodies (no byte for MASU_STALL_MS) and 5xx/408/429
// with backoff, waits while the browser says it is offline, and only gives up after
// MASU_GIVE_UP_MS (a 404 fails at once). While it retries it wakes the waiting pthread every
// few seconds, so the pthread's own timeout only catches a dead worker. Store chunks are named by
// their content: they are kept in Cache Storage (MASU_CHUNKS, web/src/net.ts uses the same one),
// so a second play, or a game saved to the device beforehand, needs no network for them. A chunk
// is checked against its name (BLAKE3, masuBlake3) before it is used or kept: one that does not
// match (cut or damaged on the way, a wrong answer) is fetched again, at most MASU_BAD_CHUNK
// times. Kept ones carry MASU_CHECKED; one kept before that (or by an older page) is checked
// once when read, then kept again with it.
// Retries are announced on the BroadcastChannel "masu-net" (the page shows that it waits).
var MASU_CHUNKS = 'masu-chunks-v1';
var MASU_STALL_MS = 20000;
var MASU_GIVE_UP_MS = 15 * 60000;
var MASU_RETRY_STATUS = [0, 408, 425, 429, 500, 502, 503, 504];
var MASU_BAD_CHUNK = 3;
var MASU_CHECKED = 'x-masu-checked';

/** A store chunk: content-addressed, the same bytes forever. */
function masuIsChunk(url) {
  return /\/chunks\/[0-9a-f]{2}\/[0-9a-f]{32}$/.test(url);
}

/**
 * The first 16 bytes of `data`'s BLAKE3 hash, as hex: a store chunk's name (crates/masu/src/pack.rs).
 * One-shot (a chunk at once: 4 MiB on average, 64 MiB at most), all of it in this function: the
 * network worker gets it as source.
 */
function masuBlake3(data) {
  // (wasm SIMD where there is: ~6-10x masuBlake3Js's speed; that was the network worker's
  // busiest code while a game loads)
  const fast = data.length > 1024 && masuBlake3Simd();
  return fast ? fast(data) : masuBlake3Js(data);
}

/** masuBlake3 in plain JavaScript (where there is no wasm SIMD, and small inputs). */
function masuBlake3Js(data) {
  const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const PERM = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
  const S = [];
  let s = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
  for (let r = 0; r < 7; r++) {
    S.push(s);
    s = PERM.map((p) => s[p]);
  }
  const bytes = new Uint8Array(64);
  const m = new Uint32Array(bytes.buffer);
  const out = new Uint32Array(16);
  // the compression function: out = the 16 words for chaining value `h` (8 words at `ho`), block m
  const compress = (h, ho, counter, len, flags) => {
    let v0 = h[ho], v1 = h[ho + 1], v2 = h[ho + 2], v3 = h[ho + 3], v4 = h[ho + 4], v5 = h[ho + 5], v6 = h[ho + 6], v7 = h[ho + 7];
    let v8 = IV[0], v9 = IV[1], v10 = IV[2], v11 = IV[3], v12 = counter | 0, v13 = (counter / 0x100000000) | 0, v14 = len, v15 = flags;
    for (let r = 0; r < 7; r++) {
      const q = S[r];
      // columns
      v0 = (v0 + v4 + m[q[0]]) | 0; v12 ^= v0; v12 = (v12 >>> 16) | (v12 << 16);
      v8 = (v8 + v12) | 0; v4 ^= v8; v4 = (v4 >>> 12) | (v4 << 20);
      v0 = (v0 + v4 + m[q[1]]) | 0; v12 ^= v0; v12 = (v12 >>> 8) | (v12 << 24);
      v8 = (v8 + v12) | 0; v4 ^= v8; v4 = (v4 >>> 7) | (v4 << 25);
      v1 = (v1 + v5 + m[q[2]]) | 0; v13 ^= v1; v13 = (v13 >>> 16) | (v13 << 16);
      v9 = (v9 + v13) | 0; v5 ^= v9; v5 = (v5 >>> 12) | (v5 << 20);
      v1 = (v1 + v5 + m[q[3]]) | 0; v13 ^= v1; v13 = (v13 >>> 8) | (v13 << 24);
      v9 = (v9 + v13) | 0; v5 ^= v9; v5 = (v5 >>> 7) | (v5 << 25);
      v2 = (v2 + v6 + m[q[4]]) | 0; v14 ^= v2; v14 = (v14 >>> 16) | (v14 << 16);
      v10 = (v10 + v14) | 0; v6 ^= v10; v6 = (v6 >>> 12) | (v6 << 20);
      v2 = (v2 + v6 + m[q[5]]) | 0; v14 ^= v2; v14 = (v14 >>> 8) | (v14 << 24);
      v10 = (v10 + v14) | 0; v6 ^= v10; v6 = (v6 >>> 7) | (v6 << 25);
      v3 = (v3 + v7 + m[q[6]]) | 0; v15 ^= v3; v15 = (v15 >>> 16) | (v15 << 16);
      v11 = (v11 + v15) | 0; v7 ^= v11; v7 = (v7 >>> 12) | (v7 << 20);
      v3 = (v3 + v7 + m[q[7]]) | 0; v15 ^= v3; v15 = (v15 >>> 8) | (v15 << 24);
      v11 = (v11 + v15) | 0; v7 ^= v11; v7 = (v7 >>> 7) | (v7 << 25);
      // diagonals
      v0 = (v0 + v5 + m[q[8]]) | 0; v15 ^= v0; v15 = (v15 >>> 16) | (v15 << 16);
      v10 = (v10 + v15) | 0; v5 ^= v10; v5 = (v5 >>> 12) | (v5 << 20);
      v0 = (v0 + v5 + m[q[9]]) | 0; v15 ^= v0; v15 = (v15 >>> 8) | (v15 << 24);
      v10 = (v10 + v15) | 0; v5 ^= v10; v5 = (v5 >>> 7) | (v5 << 25);
      v1 = (v1 + v6 + m[q[10]]) | 0; v12 ^= v1; v12 = (v12 >>> 16) | (v12 << 16);
      v11 = (v11 + v12) | 0; v6 ^= v11; v6 = (v6 >>> 12) | (v6 << 20);
      v1 = (v1 + v6 + m[q[11]]) | 0; v12 ^= v1; v12 = (v12 >>> 8) | (v12 << 24);
      v11 = (v11 + v12) | 0; v6 ^= v11; v6 = (v6 >>> 7) | (v6 << 25);
      v2 = (v2 + v7 + m[q[12]]) | 0; v13 ^= v2; v13 = (v13 >>> 16) | (v13 << 16);
      v8 = (v8 + v13) | 0; v7 ^= v8; v7 = (v7 >>> 12) | (v7 << 20);
      v2 = (v2 + v7 + m[q[13]]) | 0; v13 ^= v2; v13 = (v13 >>> 8) | (v13 << 24);
      v8 = (v8 + v13) | 0; v7 ^= v8; v7 = (v7 >>> 7) | (v7 << 25);
      v3 = (v3 + v4 + m[q[14]]) | 0; v14 ^= v3; v14 = (v14 >>> 16) | (v14 << 16);
      v9 = (v9 + v14) | 0; v4 ^= v9; v4 = (v4 >>> 12) | (v4 << 20);
      v3 = (v3 + v4 + m[q[15]]) | 0; v14 ^= v3; v14 = (v14 >>> 8) | (v14 << 24);
      v9 = (v9 + v14) | 0; v4 ^= v9; v4 = (v4 >>> 7) | (v4 << 25);
    }
    out[0] = v0 ^ v8; out[1] = v1 ^ v9; out[2] = v2 ^ v10; out[3] = v3 ^ v11;
    out[4] = v4 ^ v12; out[5] = v5 ^ v13; out[6] = v6 ^ v14; out[7] = v7 ^ v15;
  };
  // (words are little-endian: m is a Uint32Array over the block's bytes, as every browser is)
  const chunk = (i, root) => {
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
  const parent = (l, r, root) => {
    m.set(l);
    m.set(r, 8);
    compress(IV, 0, 0, 64, 4 | (root ? 8 : 0));
    return out.slice(0, 8);
  };
  const n = Math.max(1, Math.ceil(data.length / 1024));
  let cv;
  if (n === 1) cv = chunk(0, true);
  else {
    const stack = [];
    for (let i = 0; i < n - 1; i++) {
      let c = chunk(i, false);
      for (let t = i + 1; (t & 1) === 0; t >>= 1) c = parent(stack.pop(), c, false);
      stack.push(c);
    }
    cv = chunk(n - 1, false);
    while (stack.length > 1) cv = parent(stack.pop(), cv, false);
    cv = parent(stack.pop(), cv, true);
  }
  let hex = '';
  for (let k = 0; k < 16; k++) hex += ((cv[k >> 2] >>> ((k & 3) * 8)) & 255).toString(16).padStart(2, '0');
  return hex;
}

/**
 * BLAKE3 with wasm SIMD (masuBlake3's fast path): a module made here, byte by byte, whose
 * `h(in, stride, n, blocks, counter, step, flags, start, end, lastLen, out)` hashes `n` (1-4)
 * inputs at once, one per i32x4 lane (input l at in + l*stride: `blocks` 64-byte blocks, the
 * last `lastLen` bytes long, zero-padded; counter + l*step; `flags` on every block, `start` on
 * the first, `end` on the last), and stores each one's chaining value (8 words) at out + l*32.
 * Whole chunks go four at a time, then the tree's parents four at a time, level by level.
 * Returns hash(data) -> the first 16 bytes as hex, or null where wasm SIMD is missing.
 */
function masuBlake3Simd() {
  if (masuBlake3Simd.made) return masuBlake3Simd.hash;
  masuBlake3Simd.made = true;
  const IV = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
  const PERM = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
  const leb = (v) => {
    const o = [];
    for (;;) {
      const b = v & 0x7f;
      v >>= 7;
      if ((v === 0 && !(b & 0x40)) || (v === -1 && b & 0x40)) return o.push(b), o;
      o.push(b | 0x80);
    }
  };
  const uleb = (v) => {
    const o = [];
    do {
      let b = v & 0x7f;
      v >>>= 7;
      if (v) b |= 0x80;
      o.push(b);
    } while (v);
    return o;
  };
  const c = [];
  // params 0-10 (i32): in stride n blocks counter step flags start end lastLen out
  const [IN, STRIDE, N, BLOCKS, COUNTER, STEP, FLAGS, START, END, LASTLEN, OUT] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
  // i32 locals 11-16: a0-a3 (each lane's address), b (block), f (this block's flags)
  const A = 11, B = 15, F = 16;
  // v128 locals from 17: h0-7, v0-15, m0-15, ctr
  const H = 17, V = 25, M = 41, CTR = 57;
  const lget = (i) => c.push(0x20, ...uleb(i));
  const lset = (i) => c.push(0x21, ...uleb(i));
  const i32 = (v) => c.push(0x41, ...leb(v | 0));
  const simd = (op, ...imm) => c.push(0xfd, ...uleb(op), ...imm);
  const splatConst = (v) => (i32(v), simd(0x11));
  const add = () => simd(0xae), xor = () => simd(0x51), or = () => simd(0x50);
  const shuffle = (lanes) => simd(0x0d, ...lanes);
  const ROT16 = [], ROT8 = [];
  for (let i = 0; i < 4; i++) ROT16.push(4 * i + 2, 4 * i + 3, 4 * i, 4 * i + 1), ROT8.push(4 * i + 1, 4 * i + 2, 4 * i + 3, 4 * i);
  // x = rotr(x, r) on the stack
  const rot = (r, tmp) => {
    if (r === 16 || r === 8) {
      lset(tmp);
      lget(tmp);
      lget(tmp);
      shuffle(r === 16 ? ROT16 : ROT8);
      return;
    }
    lset(tmp);
    lget(tmp);
    i32(r);
    simd(0xad); // shr_u
    lget(tmp);
    i32(32 - r);
    simd(0xab); // shl
    or();
  };
  const T = CTR + 1; // a scratch v128
  const g = (a, b, cc, d, x, y) => {
    const v = (k) => V + k;
    // a = a + b + x; d = rotr(d ^ a, 16); c = c + d; b = rotr(b ^ c, 12)
    lget(v(a)), lget(v(b)), add(), lget(M + x), add(), lset(v(a));
    lget(v(d)), lget(v(a)), xor(), rot(16, T), lset(v(d));
    lget(v(cc)), lget(v(d)), add(), lset(v(cc));
    lget(v(b)), lget(v(cc)), xor(), rot(12, T), lset(v(b));
    lget(v(a)), lget(v(b)), add(), lget(M + y), add(), lset(v(a));
    lget(v(d)), lget(v(a)), xor(), rot(8, T), lset(v(d));
    lget(v(cc)), lget(v(d)), add(), lset(v(cc));
    lget(v(b)), lget(v(cc)), xor(), rot(7, T), lset(v(b));
  };
  // lane addresses: in + min(l, n - 1) * stride
  for (let l = 0; l < 4; l++) {
    lget(IN);
    i32(l);
    lget(N);
    i32(1);
    c.push(0x6b); // i32.sub
    i32(l);
    lget(N);
    i32(1);
    c.push(0x6b);
    c.push(0x48); // i32.lt_s: l < n - 1 ? l : n - 1
    c.push(0x1b); // select
    lget(STRIDE);
    c.push(0x6c); // i32.mul
    c.push(0x6a); // i32.add
    lset(A + l);
  }
  // counters: counter + l * step
  lget(COUNTER), simd(0x11);
  lget(STEP), simd(0x11);
  simd(0x0c, ...[0, 1, 2, 3].flatMap((k) => [k, 0, 0, 0]));
  simd(0xb5); // i32x4.mul
  add();
  lset(CTR);
  for (let k = 0; k < 8; k++) splatConst(IV[k]), lset(H + k);
  i32(0), lset(B);
  // loop over the blocks
  c.push(0x02, 0x40, 0x03, 0x40); // block, loop
  {
    // f = flags | (b == 0 ? start : 0) | (b == blocks - 1 ? end : 0)
    lget(FLAGS);
    lget(START), i32(0), lget(B), c.push(0x45, 0x1b); // b == 0 (i32.eqz) ? start : 0
    c.push(0x72); // i32.or
    lget(END), i32(0), lget(B), lget(BLOCKS), i32(1), c.push(0x6b, 0x46, 0x1b); // b == blocks-1
    c.push(0x72);
    lset(F);
    // m_w: word w of each lane's block (all loaded before anything is stored)
    for (let w = 0; w < 16; w++) {
      simd(0x0c, ...new Array(16).fill(0));
      for (let l = 0; l < 4; l++) {
        lset(T);
        lget(A + l);
        lget(B), i32(6), c.push(0x74), c.push(0x6a); // + (b << 6)
        lget(T);
        simd(0x56, 2, ...uleb(4 * w), l); // v128.load32_lane align=2 offset=4w lane l
      }
      lset(M + w);
    }
    for (let k = 0; k < 8; k++) lget(H + k), lset(V + k);
    for (let k = 0; k < 4; k++) splatConst(IV[k]), lset(V + 8 + k);
    lget(CTR), lset(V + 12);
    simd(0x0c, ...new Array(16).fill(0)), lset(V + 13);
    // len: lastLen on the last block, else 64
    lget(LASTLEN), i32(64), lget(B), lget(BLOCKS), i32(1), c.push(0x6b, 0x46, 0x1b);
    simd(0x11), lset(V + 14);
    lget(F), simd(0x11), lset(V + 15);
    let s = [...Array(16).keys()];
    for (let r = 0; r < 7; r++) {
      g(0, 4, 8, 12, s[0], s[1]);
      g(1, 5, 9, 13, s[2], s[3]);
      g(2, 6, 10, 14, s[4], s[5]);
      g(3, 7, 11, 15, s[6], s[7]);
      g(0, 5, 10, 15, s[8], s[9]);
      g(1, 6, 11, 12, s[10], s[11]);
      g(2, 7, 8, 13, s[12], s[13]);
      g(3, 4, 9, 14, s[14], s[15]);
      s = PERM.map((p) => s[p]);
    }
    for (let k = 0; k < 8; k++) lget(V + k), lget(V + 8 + k), xor(), lset(H + k);
    // b++, again while b < blocks
    lget(B), i32(1), c.push(0x6a), lset(B);
    lget(B), lget(BLOCKS), c.push(0x48, 0x0d, 0x00); // i32.lt_s, br_if 0 (the loop)
  }
  c.push(0x0b, 0x0b); // end loop, end block
  // stores: lane l (l < n) to out + l*32
  for (let l = 0; l < 4; l++) {
    if (l) lget(N), i32(l), c.push(0x4a, 0x04, 0x40); // n > l: if
    for (let k = 0; k < 8; k++) {
      lget(OUT);
      lget(H + k);
      simd(0x5a, 2, ...uleb(l * 32 + 4 * k), l); // v128.store32_lane
    }
    if (l) c.push(0x0b);
  }
  c.push(0x0b); // end function
  const locals = [0x02, 6, 0x7f, 42, 0x7b]; // 6 i32, 42 v128 (h, v, m, ctr, scratch)
  const body = [...locals, ...c];
  const sec = (id, bytes) => [id, ...uleb(bytes.length), ...bytes];
  const name = (s) => [s.length, ...[...s].map((ch) => ch.charCodeAt(0))];
  const bytes = new Uint8Array([
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00,
    ...sec(1, [1, 0x60, 11, ...new Array(11).fill(0x7f), 0]),
    ...sec(3, [1, 0]),
    ...sec(5, [1, 0, 1]),
    ...sec(7, [2, ...name('h'), 0, 0, ...name('m'), 2, 0]),
    ...sec(10, [1, ...uleb(body.length), ...body]),
  ]);
  let inst;
  try {
    inst = new WebAssembly.Instance(new WebAssembly.Module(bytes));
  } catch (_) {
    return (masuBlake3Simd.hash = null);
  }
  const h = inst.exports.h, mem = inst.exports.m;
  const hash = (data) => {
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
      for (let i = 0; i < k; i += 4) h(cvBytes + i * 1024, 1024, Math.min(4, k - i), 16, w + i, 1, 0, 1, 2, 64, (w + i) * 32);
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
    let hex = '';
    for (let k = 0; k < 16; k++) hex += u8[k].toString(16).padStart(2, '0');
    return hex;
  };
  return (masuBlake3Simd.hash = hash);
}

/** Whether `data` is store chunk `url`'s content (true for anything not a chunk). */
function masuChunkOk(url, data) {
  return !masuIsChunk(url) || masuBlake3(data) === url.slice(url.lastIndexOf('/') + 1);
}

/** The delay before retry `attempt` (from 0): exponential with jitter, at most 8 s. */
function masuBackoff(attempt) {
  return Math.min(8000, 300 * 2 ** attempt) * (0.5 + Math.random() * 0.5);
}

/** Waits `ms`; while the browser reports no network, until it reports one again (at most 30 s). */
async function masuPause(ms) {
  await new Promise((r) => setTimeout(r, ms));
  // (polled: a worker's 'online' event is not dependable)
  for (let i = 0; i < 60 && globalThis.navigator?.onLine === false; i++) await new Promise((r) => setTimeout(r, 500));
}

/** One try: the body read in pieces; aborted when no byte arrives for `stallMs`. */
async function masuFetchOnce(url, stallMs, reload, headers) {
  const abort = new AbortController();
  let last = Date.now();
  const watch = setInterval(() => {
    if (Date.now() - last > stallMs) abort.abort(new Error('stalled'));
  }, Math.min(1000, stallMs / 4));
  try {
    // A range never goes through the browser's HTTP cache: the same URL's whole chunk (200,
    // zstd-coded) is kept there, and Chromium answers a range from that entry with the coded
    // bytes (a wrong Content-Range, then "Failed to fetch"): reads retried for good while the
    // page's read-ahead had fetched the chunk first, and the game stopped at its start.
    // Under CPU load the bodies of several 206s of one chunk at once also waited on each other
    // there for 20 s and more (2026-11-13, #205).
    const cache = headers?.range ? 'no-store' : reload ? 'reload' : undefined;
    const init = { signal: abort.signal, ...(cache ? { cache } : {}), ...(headers ? { headers } : {}) };
    const response = await fetch(url, init);
    last = Date.now();
    if (!response.ok) {
      response.body?.cancel?.().catch?.(() => {});
      return { response };
    }
    if (!response.body?.getReader) return { response, data: new Uint8Array(await response.arrayBuffer()) };
    const reader = response.body.getReader();
    const parts = [];
    let length = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      last = Date.now();
      parts.push(value);
      length += value.length;
    }
    const data = new Uint8Array(length);
    let at = 0;
    for (const p of parts) {
      data.set(p, at);
      at += p.length;
    }
    return { response, data };
  } finally {
    clearInterval(watch);
  }
}

var masuNet = { retrying: new Map(), channel: undefined, id: Math.random().toString(36).slice(2) };
/** Tells the page how many requests are being retried now (and the last error). */
function masuNetReport(error) {
  try {
    masuNet.channel ??= new BroadcastChannel('masu-net');
    masuNet.channel.unref?.();
    masuNet.channel.postMessage({ id: masuNet.id, retrying: masuNet.retrying.size, error: String(error ?? '') });
  } catch (_) {}
}

/**
 * How long one step of a read of Cache Storage (open, match, a body) may take: past it, the
 * chunk is as if not there and comes from the network. Chromium left a cached chunk's body read
 * pending for good under load (2026-11-13, 白濁の少女 and 夏神家に起きたこと+ about 1 start in 4
 * with the warm run beside the first reads: a read ahead waited in Blob.arrayBuffer, the game's
 * read for its blocks behind it, no window ever); a read of a 4 MiB chunk takes milliseconds.
 */
var MASU_CACHE_MS = 5000;
/** `p` (a Cache Storage step), or a rejection once it took MASU_CACHE_MS. */
function masuCacheStep(p) {
  let timer;
  const late = new Promise((_, no) => (timer = setTimeout(() => no(new Error('Cache Storage did not answer')), MASU_CACHE_MS)));
  return Promise.race([p, late]).finally(() => clearTimeout(timer));
}

/**
 * The device's chunk store (opened once: every read of a block asks for it): OPFS, with Cache
 * Storage's entries from before read too (web/src/chunkstore.ts has the page's copy, tested
 * against this one on one folder); Cache Storage alone where there is no OPFS.
 */
var masuCacheOpen;
var MASU_CHUNK_DIR = 'masu-chunks';
function masuCache() {
  return (masuCacheOpen ??= (async () => {
    let old = null;
    try {
      old = typeof caches === 'undefined' ? null : await masuCacheStep(caches.open(MASU_CHUNKS));
    } catch (_) {}
    try {
      const root = await navigator.storage.getDirectory();
      return masuOpfsStore(await root.getDirectoryHandle(MASU_CHUNK_DIR, { create: true }), old);
    } catch (_) {
      return old;
    }
  })());
}

/** A key's file name in the chunk folder (chunkstore.ts chunkFileName). */
function masuChunkFile(u) {
  u = typeof u === 'string' ? u : u.url;
  const m = /\/([0-9a-f]{2}|masu-chunks)\/([0-9a-f]{32}(\?b=\d+&s=\d+)?)$/.exec(u) ?? /^()([0-9a-f]{32}(\?b=\d+&s=\d+)?)$/.exec(u);
  return m && (m[1] === '' || m[1] === 'masu-chunks' || m[2].startsWith(m[1])) ? m[2] : '';
}

/**
 * Chunks in OPFS folder `dir` (one file each, chunkstore.ts says how), Cache Storage's `old`
 * read too, with the Cache interface the rest of this file uses, and `read(url, from, len)`: a
 * part through a read-only synchronous access handle (a dedicated worker's), kept open
 * MASU_HANDLE_MS for the next reads of the same chunk, MASU_HANDLES at most; elsewhere the
 * file's slice. null when it is not here.
 */
var MASU_HANDLE_MS = 5000;
var MASU_HANDLES = 32;
function masuOpfsStore(dir, old) {
  const handles = new Map();
  const oldKey = (k) => {
    const u = typeof k === 'string' ? k : k.url;
    return /^https?:/.test(u) ? u : undefined;
  };
  const notFound = (e) => e?.name === 'NotFoundError' || e?.name === 'TypeMismatchError';
  const file = async (name) => {
    try {
      const f = await (await dir.getFileHandle(name)).getFile();
      return f.size > 0 ? f : null;
    } catch (e) {
      if (notFound(e)) return null;
      throw e;
    }
  };
  const close = (name) => {
    const h = handles.get(name);
    if (!h) return;
    handles.delete(name);
    clearTimeout(h.timer);
    try {
      h.h.close();
    } catch (_) {}
  };
  const handle = async (name) => {
    let h = handles.get(name);
    if (!h) {
      let fh;
      try {
        fh = await dir.getFileHandle(name);
      } catch (e) {
        if (notFound(e)) return null;
        throw e;
      }
      if (typeof fh.createSyncAccessHandle !== 'function') return undefined;
      let sync;
      try {
        sync = await fh.createSyncAccessHandle({ mode: 'read-only' });
      } catch (_) {
        return undefined;
      }
      // (opened meanwhile by another read: that one)
      if (handles.has(name)) {
        try {
          sync.close();
        } catch (_) {}
        h = handles.get(name);
      } else {
        while (handles.size >= MASU_HANDLES) close(handles.keys().next().value);
        h = { h: sync };
        handles.set(name, h);
      }
    } else {
      // (the newest last: the oldest goes first)
      handles.delete(name);
      handles.set(name, h);
    }
    clearTimeout(h.timer);
    h.timer = setTimeout(() => close(name), MASU_HANDLE_MS);
    return h.h;
  };
  return {
    async match(k) {
      const name = masuChunkFile(k);
      const f = name ? await file(name) : null;
      if (f) {
        const headers = { 'x-masu-size': String(f.size) };
        if (!name.includes('?')) headers[MASU_CHECKED] = '1';
        return new Response(f, { headers });
      }
      const o = oldKey(k);
      return o ? ((await old?.match(o).catch(() => undefined)) ?? undefined) : undefined;
    },
    async put(k, response) {
      const name = masuChunkFile(k);
      const o = oldKey(k);
      if (!name || (!name.includes('?') && response.headers.get(MASU_CHECKED) !== '1')) {
        if (o) await old?.put(o, response);
        return;
      }
      const data = new Uint8Array(await response.arrayBuffer());
      if (!data.length) return;
      // (this worker's own read handle let go; another's (the page's save, another tab): kept already)
      close(name);
      let w;
      try {
        w = await (await dir.getFileHandle(name, { create: true })).createWritable();
      } catch (e) {
        if (e?.name === 'NoModificationAllowedError') return;
        throw e;
      }
      try {
        await w.write(data);
        await w.close();
      } catch (e) {
        await w.abort().catch(() => {});
        throw e;
      }
      if (o) await old?.delete(o).catch(() => false);
    },
    async delete(k) {
      const name = masuChunkFile(k);
      let gone = false;
      if (name) {
        close(name);
        try {
          await dir.removeEntry(name);
          gone = true;
        } catch (_) {}
      }
      const o = oldKey(k);
      const was = (o && (await old?.delete(o).catch(() => false))) || false;
      return gone || was;
    },
    async keys() {
      const out = [];
      const seen = new Set();
      for await (const name of dir.keys()) {
        if (!masuChunkFile(name)) continue;
        seen.add(name);
        out.push({ url: `opfs:/${MASU_CHUNK_DIR}/${name}` });
      }
      for (const k of (await old?.keys().catch(() => [])) ?? []) if (!seen.has(masuChunkFile(k))) out.push(k);
      return out;
    },
    async read(k, from, len) {
      const name = masuChunkFile(k);
      if (!name) return null;
      const h = await handle(name);
      if (h === null) return null;
      if (h) {
        const size = h.getSize();
        if (!size) return null;
        const buf = new Uint8Array(Math.max(0, Math.min(len, size - from)));
        const n = h.read(buf, { at: from });
        return n === buf.length ? buf : buf.subarray(0, n);
      }
      const f = await file(name);
      return f ? new Uint8Array(await f.slice(from, from + len).arrayBuffer()) : null;
    },
    closeAll() {
      for (const name of [...handles.keys()]) close(name);
    },
  };
}

/**
 * "<store path>#<from>+<len>/<block>[!a]" (vfs.rs asks for blocks of a chunk so, `block` bytes
 * each; "!a": by its fetcher threads, ahead of the game's reads): [path, from, len, block,
 * class (MASU_CLASS)]; a plain path: [path].
 */
function masuRange(name) {
  const m = /^(.*)#(\d+)\+(\d+)(?:\/(\d+))?(!a)?$/.exec(name);
  return m ? [m[1], +m[2], +m[3], m[4] === undefined ? undefined : +m[4], m[5] ? MASU_CLASS.ahead : MASU_CLASS.game] : [name];
}

/**
 * Whole chunks this worker fetched lately, newest last (MASU_RECENT_BYTES, and the newest one
 * whatever its size): a read asks for a chunk's blocks one range after another, and the first
 * fetched it whole (the network has no ranges of a zstd-coded chunk). They are answered from
 * here until Cache Storage has the chunk (its put is not awaited), or where there is none.
 */
var MASU_RECENT_BYTES = 32 << 20;
var masuRecent = new Map();
function masuKeepRecent(url, data) {
  masuRecent.delete(url);
  masuRecent.set(url, data);
  let bytes = 0;
  for (const d of masuRecent.values()) bytes += d.length;
  for (const [k, d] of masuRecent) {
    if (bytes <= MASU_RECENT_BYTES || masuRecent.size === 1) break;
    masuRecent.delete(k);
    bytes -= d.length;
  }
}
/**
 * Whole chunks checked and on their way onto the device, their put not done yet (a pack's
 * neighbours, a chunk past MASU_RECENT_BYTES): answered from here meanwhile, so a read of one
 * in that time does not fetch it again. MASU_SAVING_BYTES of them at most before the fetches
 * past the game's wait for the device to catch up (masuGate).
 */
var MASU_SAVING_BYTES = 64 << 20;
var masuSaving = new Map();
var masuSavingBytes = 0;
/** How long a chunk waits here for its put (Cache Storage can leave one pending for good under
 * load: its bytes stayed here, and past MASU_SAVING_BYTES every fetch ahead stopped). */
var MASU_PUT_MS = 30000;
function masuKeep(cache, url, data) {
  if (!cache || masuSaving.has(url)) return;
  masuSaving.set(url, data);
  masuSavingBytes += data.length;
  let timer;
  const late = new Promise((r) => (timer = setTimeout(r, MASU_PUT_MS)));
  Promise.race([cache.put(url, new Response(data, { headers: { [MASU_CHECKED]: '1', 'x-masu-size': String(data.length) } })), late])
    .catch(() => {})
    .finally(() => {
      clearTimeout(timer);
      if (masuSaving.get(url) !== data) return;
      masuSaving.delete(url);
      masuSavingBytes -= data.length;
    });
}
/** Chunk `url` whole if this worker holds it: fetched lately, or on its way onto the device. */
function masuHeld(url) {
  return masuRecent.get(url) ?? masuSaving.get(url);
}
/** Cache Storage's key for block `k` (of `block` bytes) of chunk `url`, kept alone. */
function masuBlockKey(url, k, block) {
  return `${url}?b=${k}&s=${block}`;
}

// ---------------------------------------------------------------------------------------------
// One scheduler for every fetch from the bucket (docs/s3-reads.md): the game's reads, the blocks
// vfs.rs fetches ahead, the files it says the game will read soon (hints), the chunks fetched
// onto the device while it plays (warm) and a game saved whole (save) all go through this
// worker. The bucket (Hugging Face Storage Buckets) answers each object after 0.4-2 s whatever
// its size and lets one address make 3000 requests in 5 minutes ("resolvers", fixed window): a
// budget of requests is shared, each class leaving a reserve to the ones above it, and a chunk
// asked for twice is fetched once (in this worker: the same promise; across the page's other
// fetchers, its save: a Web Lock per chunk).

/** Request classes, most urgent first. */
var MASU_CLASS = { game: 0, ahead: 1, hint: 2, warm: 3, save: 4 };
/** Requests per second the budget refills by, and holds at most. */
var MASU_RATE = 9;
var MASU_BURST = 40;
/** Requests each class leaves to the classes above it; the game's may run the budget into debt
 * (to -MASU_RATE * 5: a game waits for no fetch ahead's budget, and its bursts are short). */
var MASU_RESERVE = [-MASU_RATE * 5, 0, 8, 16, 16];

/**
 * The shared budget: an Int32 lock word (0 free, else its holder's tag), then [tokens, last
 * refill (Date.now()), blocked until (Date.now()), share of the line's budget (0, 1], the
 * page's other tabs having the rest)] as Float64, over a SharedArrayBuffer the page makes
 * (web/src/budget.ts netBudget: its own save uses it too) or this worker's own. The lock is only
 * freed by its holder: a taker that cannot have it gives up for now without touching the budget
 * (masuTake: no; masuDrain: again in a few ms); after the page terminated a worker, the page
 * frees the lock that worker held (masuBudgetReclaim). formal/tla/BudgetLock.tla.
 */
var MASU_BUDGET_BYTES = 40;
var MASU_BUDGET_SPIN = 1 << 14;
var masuBudgetBuf;
/** This context's tag in the lock word: the page's network worker gets its own from the page. */
var masuBudgetTag = 1;
function masuBudgetInit(buf, tag) {
  masuBudgetBuf = buf ?? masuBudgetBuf ?? (() => {
    const b = new (typeof SharedArrayBuffer === 'undefined' ? ArrayBuffer : SharedArrayBuffer)(MASU_BUDGET_BYTES);
    new Float64Array(b, 8, 4).set([MASU_BURST, Date.now(), 0, 1]);
    return b;
  })();
  if (tag) masuBudgetTag = tag;
  return masuBudgetBuf;
}
/** `{ v: f(values) }` with the budget's lock held (a few instructions: spun for, a while);
 * undefined, `f` not run, when it could not be had (its holder not scheduled now). */
function masuBudgetWith(f) {
  const buf = masuBudgetInit();
  const lock = new Int32Array(buf, 0, 1);
  const tag = masuBudgetTag;
  for (let n = 0; Atomics.compareExchange(lock, 0, 0, tag) !== 0; n++) if (n >= MASU_BUDGET_SPIN) return undefined;
  try {
    return { v: f(new Float64Array(buf, 8, 4)) };
  } finally {
    Atomics.compareExchange(lock, 0, tag, 0);
  }
}
/** The budget's share as a number in (0, 1]. */
function masuBudgetShare(v) {
  return v[3] > 0 && v[3] <= 1 ? v[3] : 1;
}
/** Takes one request of class `cls` from the budget (refilled to `now`): whether it may go.
 * While the bucket asked to wait (a 429), no class goes, the game's neither; nor while the
 * lock is held elsewhere (masuGate tries again). */
function masuTake(cls, now = Date.now()) {
  return masuBudgetWith((v) => {
    if (now < v[2]) return false;
    const s = masuBudgetShare(v);
    const dt = Math.max(0, now - v[1]) / 1000;
    v[0] = Math.min(MASU_BURST * s, v[0] + dt * MASU_RATE * s);
    v[1] = Math.max(v[1], now);
    if (v[0] - 1 < (MASU_RESERVE[cls] ?? 0) * s) return false;
    v[0] -= 1;
    return true;
  })?.v ?? false;
}
/** The bucket said 429: nobody goes for `ms`, and the budget refills only from then on, from
 * none (not a burst of every class at once when the wait ends). Lands once the lock is free. */
function masuDrain(ms, now = Date.now()) {
  const until = now + ms;
  const done = masuBudgetWith((v) => {
    v[2] = Math.max(v[2], until);
    v[0] = Math.min(v[0], 0);
    v[1] = Math.max(v[1], until);
  });
  if (!done) setTimeout(() => masuDrain(ms, now), 5);
}
/** On the page, after it terminated the worker tagged `dead` (masuFetchDispose): a lock that
 * worker still held is freed and the budget starts again from none (its 429 wait kept), as
 * budget.ts reclaim. Nothing while the lock is free or another's. */
function masuBudgetReclaim(buf, dead, now = Date.now()) {
  const lock = new Int32Array(buf, 0, 1);
  if (!dead || Atomics.compareExchange(lock, 0, dead, 1) !== dead) return false;
  const v = new Float64Array(buf, 8, 4);
  const until = Number.isFinite(v[2]) ? v[2] : 0;
  v.set([0, Math.max(now, until), until, masuBudgetShare(v)]);
  Atomics.compareExchange(lock, 0, 1, 0);
  return true;
}
/** Waits until `job` (its class read again each time) may make a request of the bucket; past
 * the game's, also until the chunks on their way onto the device are under MASU_SAVING_BYTES. */
async function masuGate(job) {
  while ((job.cls > 0 && masuSavingBytes > MASU_SAVING_BYTES) || !masuTake(job.cls)) await new Promise((r) => setTimeout(r, 50));
}

/** The game's reads waiting in this worker now (fetches ahead give way while there are any). */
var masuGameWaiting = 0;

/** Chunks being fetched whole now: url -> { promise, job }. */
var masuInflight = new Map();

/**
 * Chunk `url` whole, fetched once whoever asks: from this worker's memory, Cache Storage, a
 * fetch of it already on its way (made `cls`'s if more urgent), else from the bucket (or the
 * server). `want`: the data is needed (not only kept on the device). `size`: its decoded length
 * when known. Classes past the game's hold the chunk's Web Lock while they fetch, so the page's
 * own save does not fetch it again meanwhile (the game's do not wait for a lock).
 */
var MASU_LOCK_WAIT_MS = 1000;
function masuWhole(url, { cls = 0, size, want = true, directOnly = false } = {}) {
  const mem = masuHeld(url);
  if (mem) return Promise.resolve({ data: mem, wire: 0, cached: 1 });
  // (null: not from the bucket; the server then, as the asker wants it: once for every waiter
  // of the same chunk, not a download each)
  const rest = (r) => r ?? (directOnly ? null : masuRobustOnce(url, cls ? { giveUpMs: 60000 } : {}));
  const again = (r) => (r && want && !r.data ? masuWhole(url, { cls, size, want, directOnly }) : rest(r));
  const going = masuInflight.get(url);
  if (going) {
    going.job.cls = Math.min(going.job.cls, cls);
    // (a fetch ahead still waiting for the chunk's lock, held by another fetcher of it, the
    // page's save most likely: the game waits for no lock. A little for that fetch, then its own.)
    if (cls === 0 && going.locking) {
      let late;
      const timer = new Promise((r) => (late = setTimeout(() => r(masuWhole.LATE), MASU_LOCK_WAIT_MS)));
      return Promise.race([going.promise, timer]).then((r) => {
        clearTimeout(late);
        if (r !== masuWhole.LATE) return again(r);
        const now = masuInflight.get(url);
        if (now && !now.locking) return now.promise.then(again);
        return masuStartWhole(url, cls, size, want, false).then(again);
      });
    }
    return going.promise.then(again);
  }
  return masuStartWhole(url, cls, size, want, cls > 0).then(rest);
}
masuWhole.LATE = {};

/** masuFetchRobust of `url` shared by everyone asking while it runs. */
var masuRobustInflight = new Map();
function masuRobustOnce(url, opt) {
  let p = masuRobustInflight.get(url);
  if (!p) {
    p = masuFetchRobust(url, opt).finally(() => {
      if (masuRobustInflight.get(url) === p) masuRobustInflight.delete(url);
    });
    masuRobustInflight.set(url, p);
  }
  return p;
}

/** masuWhole's fetch of `url`, registered in masuInflight; `lock`: under the chunk's Web Lock. */
function masuStartWhole(url, cls, size, want, lock) {
  const job = { cls };
  const entry = { promise: undefined, job, locking: false };
  const run = async () => {
    entry.locking = false;
    // (fetched meanwhile, by the game's own fetch while this one waited for the lock)
    const mem = masuHeld(url);
    if (mem) return { data: want ? mem : null, wire: 0, cached: 1 };
    const cache = await masuCache();
    const hit = cache && (await masuCacheStep(cache.match(url)).catch(() => undefined));
    if (hit && hit.headers.get(MASU_CHECKED) === '1') {
      if (!want) return { data: null, wire: 0, cached: 1 };
      try {
        return { data: new Uint8Array(await masuCacheStep(hit.arrayBuffer())), wire: 0, cached: 1 };
      } catch (_) {}
    }
    return masuDirect(url, job, size);
  };
  const locks = lock ? globalThis.navigator?.locks : undefined;
  entry.locking = !!locks;
  const promise = (locks ? locks.request(`masu-chunk:${url.slice(-32)}`, run) : run()).finally(() => {
    if (masuInflight.get(url) === entry) masuInflight.delete(url);
  });
  entry.promise = promise;
  masuInflight.set(url, entry);
  return promise;
}

/**
 * Chunks a read says the game reads soon (vfs.rs `hint`: the next files of a folder, the next
 * chunks of an archive): fetched onto the device, MASU_HINT_WIDTH at a time, newest hints first
 * (a scene that moved on leaves the old ones behind). The budget's reserve keeps them from the
 * game's requests; the line is not what limits (16 at once from the bucket: 45 MB/s), the
 * bucket's wait for each object is.
 */
var MASU_HINT_WIDTH = 6;
var MASU_HINT_MAX = 256;
var masuHints = [];
var masuHintsSeen = new Set();
var masuHintsGoing = 0;
function masuHint(urls) {
  for (const u of urls.slice(0, MASU_HINT_MAX).reverse()) {
    if (masuHintsSeen.has(u) || masuInflight.has(u) || masuHeld(u)) continue;
    masuHintsSeen.add(u);
    masuHints.push(u);
    masuReads.hinted++;
  }
  if (masuHintsSeen.size > 1 << 16) masuHintsSeen.clear();
  // (the oldest dropped)
  if (masuHints.length > MASU_HINT_MAX) masuHints.splice(0, masuHints.length - MASU_HINT_MAX);
  void masuDirectMany(masuHints.slice(-MASU_DIRECT_MANY));
  masuHintPump();
}
function masuHintPump() {
  while (masuHintsGoing < MASU_HINT_WIDTH && masuHints.length) {
    const u = masuHints.pop();
    masuHintsGoing++;
    masuWhole(u, { cls: MASU_CLASS.hint, want: false })
      .catch(() => {})
      .finally(() => {
        masuHintsGoing--;
        // (not this worker's memory: the game's reads find it on the device)
        if (!masuInflight.has(u)) masuRecent.delete(u);
        masuHintPump();
      });
  }
}

/**
 * What came next after each chunk in earlier runs of the game (masu serve's `<list>.next`:
 * every branch, the newest runs' first, and the list's own order after them): hex -> hexes.
 * When the game reads a chunk for the first time, the chunks that followed it are fetched ahead
 * as hints (masuFollow), whichever way the game goes from there: one list in one order is the
 * way one run went (title, then the story), the game may go another (title, then the gallery).
 */
var MASU_FOLLOW = 12;
var masuNext = new Map();
/** `bytes`: pairs of 16-byte hashes, the chunk then one that followed it. */
function masuNextAdd(bytes) {
  const hex = (o) => Array.from(bytes.subarray(o, o + 16), (b) => b.toString(16).padStart(2, '0')).join('');
  for (let o = 0; o + 32 <= bytes.length; o += 32) {
    const a = hex(o), b = hex(o + 16);
    if (a === b) continue;
    let l = masuNext.get(a);
    if (!l) masuNext.set(a, (l = []));
    if (!l.includes(b)) l.push(b);
  }
}
/** The game read chunk `url` for the first time: what followed it, breadth first (the nearest
 * of every branch first), MASU_FOLLOW at most, fetched ahead as hints. Returns their URLs. */
function masuFollow(url) {
  const base = url.slice(0, -35);
  const out = [];
  const seen = new Set([url.slice(-32)]);
  let level = [url.slice(-32)];
  while (level.length && out.length < MASU_FOLLOW) {
    const n = [];
    for (const a of level) for (const b of masuNext.get(a) ?? []) if (!seen.has(b)) seen.add(b), n.push(b);
    out.push(...n);
    level = n;
  }
  const urls = out.slice(0, MASU_FOLLOW).map((h) => `${base}${h.slice(0, 2)}/${h}`);
  if (urls.length) masuHint(urls);
  return urls;
}

/**
 * Where each chunk the game read was when it read it first (counts and milliseconds waited):
 * `device` (kept already), `saving` (checked, on its way into the cache), `coming` (a fetch
 * ahead was on its way), `none` (the game's read went to the network itself). The misses that
 * count are `coming` and `none`: what to fetch ahead earlier. `followed`: chunks queued from
 * what came next in earlier runs (masuFollow); `hinted`: all queued as hints (those, and the
 * reads' own: a folder's next files, the files a script names, vfs/predict.rs).
 */
var masuReads = { device: [0, 0], saving: [0, 0], coming: [0, 0], none: [0, 0], followed: 0, hinted: 0 };
var masuReadSeen = new Set();
/** Before the game's read of `url`: whether it is the chunk's first, and where it is then
 * (undefined: on the device or nowhere, as the read finds out). Its followers go ahead. */
function masuReadStart(url) {
  if (!masuIsChunk(url) || masuReadSeen.has(url)) return null;
  if (masuReadSeen.size > 1 << 16) masuReadSeen.clear();
  masuReadSeen.add(url);
  const where = masuRecent.has(url) ? 'device' : masuSaving.has(url) ? 'saving' : masuInflight.has(url) ? 'coming' : undefined;
  masuReads.followed += masuFollow(url).length;
  return { where, t: Date.now() };
}
function masuReadEnd(start, cached) {
  if (!start) return;
  const r = masuReads[start.where ?? (cached ? 'device' : 'none')];
  r[0]++;
  r[1] += Date.now() - start.t;
}

/**
 * A store chunk straight from the bucket (S3) when the server gives a way (masu serve's
 * /store-direct: a presigned URL, the object's AES-256-GCM key and UUID): fetched, decrypted
 * (nonce first, the UUID as associated data), zstd-decoded (sake/snapz.mjs) and checked
 * against its name, then kept as one from the server would be. The bucket answers every object
 * after ~2 s whatever its size, so many at once go much faster than through the server's line
 * (worker2: 4 at once 3 MB/s; this page's own line has more). null: no way (an older server, a
 * store without S3, a chunk only on the server, an expired URL that failed): the server, as before.
 */
var MASU_DIRECT_KEEP_MS = 3600000;
/** How long the server may take to say how to fetch a chunk from the bucket (a stuck server, as
 * worker2's at 19:58 JST 2026-11-09, held a read 80 s): past it, the server's own /store. */
var MASU_DIRECT_ASK_MS = 10000;
var masuDirectInfo = new Map();
var masuDirectOff = false;
var masuZstd;
/** Whether `info` (from /store-direct) still works. */
function masuDirectFresh(info) {
  return !!info && Date.now() - info.at <= MASU_DIRECT_KEEP_MS;
}

/** A well-formed answer of /store-direct for one chunk. */
function masuDirectValid(info) {
  return !!info && typeof info.url === 'string' && /^[0-9a-f]{64}$/.test(info.key) && /^[0-9a-f-]{36}$/.test(info.object);
}

/**
 * Asks the server how to fetch many chunks from the bucket at once (`POST store-direct`, a
 * list of chunk paths: crates/masu/src/store_api.rs `direct_many`), MASU_DIRECT_MANY at a
 * time, and keeps the answers: a game's read of one of them then goes to the bucket at once,
 * without first a round trip to the server (which a read of every file the game had not read
 * yet paid before the bucket's own wait). Those the server does not name are only on the server.
 * An older server without the route: asked one by one, as before (masuDirect).
 */
var MASU_DIRECT_MANY = 512;
var masuDirectAsking = new Map();
var masuDirectManyOff = false;
async function masuDirectMany(urls) {
  const want = urls.filter((u) => masuIsChunk(u) && u.includes('/store/chunks/') && !masuDirectFresh(masuDirectInfo.get(u)) && !masuDirectAsking.has(u));
  if (masuDirectOff || masuDirectManyOff || !want.length || typeof fetch === 'undefined') return;
  const runs = [];
  for (let i = 0; i < want.length; i += MASU_DIRECT_MANY) {
    const part = want.slice(i, i + MASU_DIRECT_MANY);
    const at = part[0].lastIndexOf('/store/chunks/');
    const base = part[0].slice(0, at + 1);
    const run = (async () => {
      let r;
      try {
        r = await fetch(`${base}store-direct`, {
          method: 'POST',
          cache: 'no-store',
          headers: { 'content-type': 'application/json' },
          body: JSON.stringify(part.map((u) => u.slice(u.lastIndexOf('/store/chunks/') + 7))),
          signal: AbortSignal.timeout(MASU_DIRECT_ASK_MS * 3),
        });
      } catch (_) {
        return;
      }
      if (r.status === 501) masuDirectOff = true;
      // (a server without the route)
      if (r.status === 404 || r.status === 405) masuDirectManyOff = true;
      if (!r.ok) return;
      let all;
      try {
        all = await r.json();
      } catch (_) {
        return;
      }
      const now = Date.now();
      if (masuDirectInfo.size > 20000) masuDirectInfo.clear();
      for (const u of part) {
        const info = all?.[u.slice(u.lastIndexOf('/store/chunks/') + 7)];
        // (not named: only on the server, which then answers it as before)
        const ok = masuDirectValid(info);
        masuDirectInfo.set(u, ok ? { url: info.url, key: info.key, object: info.object, pack: masuPackValid(info.pack) ? info.pack : undefined, at: now } : { none: true, at: now });
        if (ok) masuPackNote(u, masuDirectInfo.get(u));
      }
    })();
    for (const u of part) masuDirectAsking.set(u, run);
    runs.push(run.finally(() => part.forEach((u) => masuDirectAsking.get(u) === run && masuDirectAsking.delete(u))));
  }
  await Promise.all(runs);
}

/**
 * Chunk `url` from the bucket itself (as above). `job.cls` is the request's class
 * (MASU_CLASS; read again while it waits for the budget: a game read that joins a fetch ahead
 * of the same chunk makes it the game's), `size` its decoded length when known; `alone`: not
 * with its delivery pack's neighbours.
 */
async function masuDirect(url, job = { cls: 0 }, size, alone = false) {
  const at = url.lastIndexOf('/store/chunks/');
  if (masuDirectOff || at < 0 || !masuIsChunk(url) || !globalThis.crypto?.subtle || typeof fetch === 'undefined') return null;
  const base = url.slice(0, at + 1);
  let info = masuDirectInfo.get(url);
  // (being asked for with others: that answer)
  if (!masuDirectFresh(info) && masuDirectAsking.has(url)) {
    await masuDirectAsking.get(url);
    info = masuDirectInfo.get(url);
  }
  if (!masuDirectFresh(info)) {
    let r;
    try {
      r = await fetch(`${base}store-direct/${url.slice(at + 7)}`, { cache: 'no-store', signal: AbortSignal.timeout(MASU_DIRECT_ASK_MS) });
    } catch (_) {
      return null;
    }
    // (501: a store without S3; 404: this chunk is only on the server)
    if (r.status === 501) masuDirectOff = true;
    if (!r.ok) return null;
    try {
      info = { ...(await r.json()), at: Date.now() };
    } catch (e) {
      // (not JSON: a server without /store-direct answered with something else; cut short by
      // the time limit: only this once)
      if (e?.name !== 'TimeoutError' && e?.name !== 'AbortError') masuDirectOff = true;
      return null;
    }
    if (!masuDirectValid(info)) return null;
    if (!masuPackValid(info.pack)) delete info.pack;
    if (masuDirectInfo.size > 20000) masuDirectInfo.clear();
    masuDirectInfo.set(url, info);
    masuPackNote(url, info);
  }
  if (info.none) return null;
  // (with the chunks after it in its delivery pack, one GET for them all; failing that, alone)
  if (info.pack && !alone) {
    const r = await masuPackRun(url, info, job, size, base);
    if (r) return r;
  }
  for (let bad = 0; bad < MASU_BAD_CHUNK; bad++) {
    let got;
    try {
      got = await masuFetchRobust(info.url, { giveUpMs: 60000, job });
    } catch (_) {
      masuDirectInfo.delete(url);
      return null;
    }
    const plain = await masuOpen(info, got.data).catch(() => null);
    const data = plain && (await masuUnzstd(base, plain, size).catch(() => null));
    if (data && masuChunkOk(url, data)) {
      masuKeepRecent(url, data);
      masuKeep(await masuCache(), url, data);
      return { data, wire: got.wire, cached: got.cached };
    }
  }
  return null;
}

/**
 * Delivery packs (docs/s3-reads.md; crates/masu/src/storage.rs pack_objects): a game's chunk
 * objects, as the bucket has them, side by side in big objects in the order the game reads
 * them. A /store-direct answer names where its chunk's object is in one (`pack`: id, URL,
 * offset, length); a read of a chunk then takes the chunks after it in the pack with the same
 * ranged GET (those not on the device nor coming already), up to MASU_PACK_SPAN bytes for its
 * class: the bucket's wait for an object (0.4-2 s) and one request of its budget paid once for
 * them all. A game's read takes fewer (it waits for the whole range); a save to the device many.
 */
var MASU_PACK_SPAN = [4 << 20, 8 << 20, 16 << 20, 32 << 20, 32 << 20];
var MASU_PACK_MAX = 64;
/** pack id -> Map(offset -> chunk URL), from the answers. */
var masuPacks = new Map();
function masuPackValid(p) {
  return !!p && typeof p.url === 'string' && /^[0-9a-f-]{36}$/.test(p.id) && Number.isSafeInteger(p.offset) && p.offset >= 0 && Number.isSafeInteger(p.len) && p.len >= 28;
}
function masuPackNote(url, info) {
  if (!info?.pack) return;
  if (masuPacks.size > 4096) masuPacks.clear();
  let m = masuPacks.get(info.pack.id);
  if (!m) masuPacks.set(info.pack.id, (m = new Map()));
  m.set(info.pack.offset, url);
}

/** Chunk `url` with the chunks after it in its pack (as above): its result, or null (then alone). */
async function masuPackRun(url, info, job, size, base) {
  const p = info.pack;
  const at = masuPacks.get(p.id);
  const run = [{ url, info }];
  const span = MASU_PACK_SPAN[job.cls] ?? MASU_PACK_SPAN.at(-1);
  const cache = await masuCache();
  let end = p.offset + p.len;
  let last = end;
  while (at && run.length < MASU_PACK_MAX) {
    const u = at.get(end);
    const n = u && masuDirectInfo.get(u);
    if (!n || !masuDirectFresh(n) || n.pack?.id !== p.id || end + n.pack.len - p.offset > span) break;
    end += n.pack.len;
    // (here or coming already: its bytes only if a later one is taken)
    if (masuInflight.has(u) || masuHeld(u)) continue;
    const hit = cache && (await masuCacheStep(cache.match(u)).catch(() => undefined));
    if (hit && hit.headers.get(MASU_CHECKED) === '1') continue;
    run.push({ url: u, info: n });
    last = end;
  }
  // (the neighbours are coming: a read of one of them joins this GET)
  let done;
  const all = new Promise((r) => (done = r));
  // (one job for the GET and every chunk of it: a game read joining a neighbour makes the GET
  // the game's; one that failed then comes alone, as urgent)
  for (const n of run.slice(1)) {
    const entry = { promise: undefined, job, locking: false };
    entry.promise = all
      .then((m) => m.get(n.url) ?? masuDirect(n.url, job, undefined, true))
      .finally(() => {
        if (masuInflight.get(n.url) === entry) masuInflight.delete(n.url);
      });
    masuInflight.set(n.url, entry);
  }
  const out = new Map();
  try {
    const got = await masuFetchRobust(p.url, { giveUpMs: 60000, range: [p.offset, last - p.offset], job });
    // (a bucket that sends the whole pack for a range: the range from it)
    const bytes = got.partial ? got.data : got.data.subarray(p.offset, last);
    if (bytes.length === last - p.offset)
      for (const n of run) {
        const off = n.info.pack.offset - p.offset;
        const plain = await masuOpen(n.info, bytes.subarray(off, off + n.info.pack.len)).catch(() => null);
        const data = plain && (await masuUnzstd(base, plain, n.url === url ? size : undefined).catch(() => null));
        if (!data || !masuChunkOk(n.url, data)) continue;
        out.set(n.url, { data, wire: n.url === url ? got.wire : 0, cached: got.cached });
        // (held until it is on the device: a read of it meanwhile does not fetch it again)
        masuKeep(cache, n.url, data);
      }
  } catch (_) {}
  done(out);
  const r = out.get(url);
  if (r) masuKeepRecent(url, r.data);
  return r ?? null;
}

/** Hex `h` as bytes. */
function masuHex(h) {
  return Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));
}

/** Object `cipher` (nonce, ciphertext and tag, as crates/masu/src/envelope.rs seals it) opened with `info`'s key. */
/** The AES keys imported so far, by their hex (a game's chunks share one: not imported per chunk). */
var masuKeys = new Map();
async function masuOpen(info, cipher) {
  if (cipher.length < 28) throw new Error('short');
  let key = masuKeys.get(info.key);
  if (!key) {
    key = crypto.subtle.importKey('raw', masuHex(info.key), 'AES-GCM', false, ['decrypt']);
    if (masuKeys.size >= 64) masuKeys.clear();
    masuKeys.set(info.key, key);
    key.catch(() => masuKeys.get(info.key) === key && masuKeys.delete(info.key));
  }
  // (views, not copies, of the IV and the ciphertext)
  const plain = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: cipher.subarray(0, 12), additionalData: masuHex(info.object.replace(/-/g, '')), tagLength: 128 }, await key, cipher.subarray(12));
  return new Uint8Array(plain);
}

/** A zstd frame's decoded size from its header (Frame_Content_Size, RFC 8878 3.1.1.1), 0 when
 * the header does not say (or is not a zstd frame's). */
function masuZstdSize(f) {
  if (!f || f.length < 6 || f[0] !== 0x28 || f[1] !== 0xb5 || f[2] !== 0x2f || f[3] !== 0xfd) return 0;
  const fhd = f[4];
  const single = (fhd >> 5) & 1;
  const at = 5 + (single ? 0 : 1) + [0, 1, 2, 4][fhd & 3];
  const n = [single ? 1 : 0, 2, 4, 8][fhd >> 6];
  if (n === 0 || at + n > f.length) return 0;
  let v = 0;
  for (let i = n - 1; i >= 0; i--) v = v * 256 + f[at + i];
  return n === 2 ? v + 256 : v;
}

/** zstd frame `frame` decoded by sake's snapz (sake/snapz.mjs beside the store): `size` bytes when known, else a chunk is 16 MiB at most (4x FastCDC's 4 MiB average), 64 MiB tried once more. */
async function masuUnzstd(base, frame, size) {
  masuZstd ??= import(`${base}sake/snapz.mjs`).then((m) => m.default());
  const z = await masuZstd;
  // (the length when known, from the trees' lists: no 16 MiB buffer for every chunk at once,
  // which several side by side on a phone add up to)
  // (else the frame's own header's size when it gives one, 64 MiB at most: a right-sized
  // buffer, not 16 MiB then 64 MiB that the zstd heap keeps committed for the session)
  const said = masuZstdSize(frame);
  for (const cap of size > 0 ? [size] : said > 0 && said <= 64 << 20 ? [said, 16 << 20, 64 << 20] : [16 << 20, 64 << 20]) {
    const src = z._malloc(frame.length) >>> 0;
    const dst = z._malloc(cap) >>> 0;
    try {
      if (!src || !dst) throw new Error('zstd: no memory');
      z.HEAPU8.set(frame, src);
      const n = z._snapz_decompress(dst, cap, src, frame.length) >>> 0;
      if (n !== 0xffffffff) return z.HEAPU8.slice(dst, dst + n);
    } finally {
      z._free?.(src);
      z._free?.(dst);
    }
  }
  throw new Error('zstd: not a frame, or too big');
}

/**
 * Bytes `from..from + len` of `url`'s (whole when `from` is undefined), as masuFetchRobust's
 * result. Of a chunk, from the first place that has them: this worker's memory (a chunk it
 * fetched whole lately), Cache Storage's copy of the whole chunk (checked when it was kept;
 * Blob.slice reads only those bytes, whatever the chunk's size), its blocks kept alone (`block`
 * bytes each, as vfs.rs asks for them: `from` is a block's start); else a range from the server
 * (only what is read crosses the network, kept as blocks); a server without ranges sends the
 * whole chunk, then checked and kept whole (and in this worker's memory for the next blocks).
 */
async function masuFetchPart(url, from, len, block, cls = 0) {
  if (from === undefined) return masuIsChunk(url) ? masuWhole(url, { cls }) : masuFetchRobust(url);
  const part = (d) => d.subarray(Math.min(from, d.length), Math.min(from + len, d.length));
  const mem = masuHeld(url);
  if (mem) return { data: part(mem), wire: 0, cached: 1 };
  if (!masuIsChunk(url)) return masuFetchPart(url).then((r) => ({ ...r, data: part(r.data) }));
  const cache = await masuCache();
  block ||= len || 1;
  const first = Math.floor(from / block);
  const last = Math.max(first, Math.ceil((from + len) / block) - 1);
  if (cache) {
    // (the whole chunk in OPFS: those bytes alone, through a synchronous handle)
    const quick = cache.read && (await masuCacheStep(cache.read(url, from, len)).catch(() => null));
    if (quick) return { data: quick, wire: 0, cached: 1 };
    const hit = await masuCacheStep(cache.match(url)).catch(() => undefined);
    if (hit && hit.headers.get(MASU_CHECKED) === '1') {
      try {
        const blob = await masuCacheStep(hit.blob());
        return { data: new Uint8Array(await masuCacheStep(blob.slice(from, from + len).arrayBuffer())), wire: 0, cached: 1 };
      } catch (_) {}
    }
    // (blocks kept alone: all of them, or the network; a block that does not read is not there,
    // not a short last one)
    const parts = [];
    for (let k = first; k <= last; k++) {
      const b = await masuCacheStep(cache.match(masuBlockKey(url, k, block))).catch(() => undefined);
      const bytes = b && (await masuCacheStep(b.arrayBuffer()).catch(() => undefined));
      if (!bytes) break;
      parts.push(new Uint8Array(bytes));
      // (a chunk's last block: shorter, nothing after it)
      if (parts.at(-1).length < block) break;
    }
    if (parts.length === last + 1 - first || (parts.length && parts.at(-1).length < block)) {
      const all = new Uint8Array(parts.reduce((a, p) => a + p.length, 0));
      let at = 0;
      for (const p of parts) all.set(p, (at += p.length) - p.length);
      return { data: all.subarray(from - first * block, from - first * block + len), wire: 0, cached: 1 };
    }
  }
  // (from the bucket itself: the whole chunk, kept; the next blocks come from memory; one
  // fetch of it whoever else asks for it now)
  const direct = await masuWhole(url, { cls, directOnly: true });
  if (direct?.data) return { ...direct, data: part(direct.data) };
  // (the whole blocks asked for: what is kept)
  const at = first * block;
  const r = await masuFetchRobust(url, { range: [at, (last + 1) * block - at] });
  if (!r.partial) return { ...r, data: part(r.data) };
  if (cache) {
    for (let k = first; k <= last; k++) {
      const d = r.data.subarray((k - first) * block, (k + 1 - first) * block);
      // (a short block only at the chunk's end)
      if (d.length === block || (r.total !== undefined && k * block + d.length === r.total))
        cache.put(masuBlockKey(url, k, block), new Response(d.slice(), { headers: { 'x-masu-size': String(d.length) } })).catch(() => {});
    }
  }
  return { ...r, data: r.data.subarray(from - at, from - at + len) };
}

/**
 * `url`'s bytes, retried as above: { data, wire (bytes over the network), cached (1: no
 * network) }. Throws for a final failure.
 */
async function masuFetchRobust(url, opt = {}) {
  const stallMs = opt.stallMs ?? MASU_STALL_MS;
  const giveUpMs = opt.giveUpMs ?? MASU_GIVE_UP_MS;
  const chunk = masuIsChunk(url);
  const cache = chunk ? await masuCache() : null;
  // opt.range [from, len]: those bytes of the decoded chunk (masuFetchPart looked on the device
  // already); the answer has `partial` (a 206 of them, masu serve's store_api.rs) or is the
  // whole chunk (a server without ranges), checked and kept as any
  const range = opt.range;
  const hit = range || !cache ? undefined : await masuCacheStep(cache.match(url)).catch(() => undefined);
  if (hit) {
    try {
      const data = new Uint8Array(await masuCacheStep(hit.arrayBuffer()));
      if (hit.headers.get(MASU_CHECKED) === '1') return { data, wire: 0, cached: 1 };
      if (masuChunkOk(url, data)) {
        masuKeep(cache, url, data);
        return { data, wire: 0, cached: 1 };
      }
      await cache.delete(url).catch(() => {});
    } catch (_) {}
  }
  const start = Date.now();
  const key = {};
  let after = 0;
  let bad = 0;
  try {
    for (let attempt = 0; ; attempt++) {
      let error;
      try {
        // (the bucket's budget, for each request made: a retry is one more; opt.job's class
        // read again each time)
        if (opt.job) await masuGate(opt.job);
        // (after a chunk that did not match, past the browser's HTTP cache: it keeps chunks for good)
        const { response, data } = await masuFetchOnce(url, stallMs, bad > 0, range && { range: `bytes=${range[0]}-${range[0] + range[1] - 1}` });
        if (data && range && response.status === 206) {
          // (a range: its place and length checked; a block has no name of its own to check)
          const m = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers?.get?.('content-range') ?? '');
          if (m && +m[1] === range[0] && data.length === +m[2] - +m[1] + 1 && (data.length === range[1] || +m[2] === +m[3] - 1))
            return { data, wire: data.length, cached: 0, partial: true, total: +m[3] };
          if (++bad >= MASU_BAD_CHUNK) throw Object.assign(new Error(`range is not what was asked ${bad} times`), { final: true });
          throw new Error('range is not what was asked');
        }
        if (range && response.status === 416) return { data: new Uint8Array(0), wire: 0, cached: 0, partial: true };
        if (data && !masuChunkOk(url, data)) {
          if (++bad >= MASU_BAD_CHUNK) throw Object.assign(new Error(`chunk does not match its name ${bad} times`), { final: true });
          throw new Error('chunk does not match its name');
        }
        if (data) {
          if (chunk) masuKeepRecent(url, data);
          // (another origin's timing without Timing-Allow-Origin, as the bucket's: all zeros,
          // which said 0 bytes crossed the network for every chunk from the bucket)
          const t = typeof performance !== 'undefined' && performance.getEntriesByName?.(url).at(-1);
          const timing = t && (t.transferSize > 0 || t.decodedBodySize > 0) ? t : undefined;
          const wire = timing ? timing.transferSize : +(response.headers?.get?.('content-length') || data.length);
          const cached = timing && timing.transferSize === 0 && timing.decodedBodySize > 0 ? 1 : 0;
          masuKeep(cache, url, data);
          return { data, wire, cached };
        }
        if (!MASU_RETRY_STATUS.includes(response.status)) throw Object.assign(new Error(`HTTP ${response.status}`), { final: true });
        error = `HTTP ${response.status}`;
        // (seconds; a date form counts as none)
        after = Math.min((+(response.headers?.get?.('retry-after')) || 0) * 1000, 30000);
        // (too many requests: every class waits, not only this one)
        if (response.status === 429) masuDrain(after || 5000);
      } catch (e) {
        if (e.final) throw e;
        error = e?.message ?? e;
      }
      if (Date.now() - start > giveUpMs) throw new Error(`${error} (gave up)`);
      masuNet.retrying.set(key, attempt);
      masuNetReport(error);
      await masuPause(after || masuBackoff(attempt));
      after = 0;
    }
  } finally {
    if (masuNet.retrying.delete(key)) masuNetReport('');
  }
}

/**
 * The transfer buffer a synchronous read and the network worker share: a header (status,
 * part length, wire bytes, from the HTTP cache, body length) and MASU_BRIDGE_BYTES of body
 * at a time, one per thread. A/B (node workers, 400 reads, median of 5): a 256 KiB chunk took
 * 0.369 ms with 64 KiB parts copied twice (the old protocol), 0.342 ms copied once, 0.214 ms
 * in one 256 KiB part, 0.216 ms with 1 MiB; a 64 KiB read 0.135-0.139 ms with any. A read
 * asks for 256 KiB blocks (crates/kura/src/vfs.rs), 1 MiB at most at a time; a whole chunk
 * (the page's) goes in several parts.
 */
var MASU_BRIDGE_HEADER = 32;
var MASU_BRIDGE_BYTES = 262144;

function masuFetchWorker(event) {
  const m = event.data;
  // (not a read: chunks the game reads soon, the budget the page shares, a warm or save run)
  if (m.hint) return masuHint(m.hint);
  if (m.budget) return void masuBudgetInit(m.budget, m.tag);
  if (m.warm) return void masuWarmRun(m.warm, (r) => event.target?.postMessage?.(r));
  const { url, shared, from, len, block } = m;
  const cls = m.cls ?? MASU_CLASS.game;
  const state = new Int32Array(shared, 0, 8);
  const output = new Uint8Array(shared, MASU_BRIDGE_HEADER);
  const publish = (status) => {
    Atomics.store(state, 0, status);
    Atomics.notify(state, 0);
  };
  // (the pthread waits with a timeout: a wake-up without news tells it this worker is alive)
  const alive = setInterval(() => Atomics.notify(state, 0), 5000);
  if (cls === MASU_CLASS.game) masuGameWaiting++;
  const first = cls === MASU_CLASS.game ? masuReadStart(url) : null;
  (async () => {
    try {
      const { data, wire, cached } = await masuFetchPart(url, from, len, block, cls).finally(() => {
        if (cls === MASU_CLASS.game) masuGameWaiting--;
      });
      masuReadEnd(first, cached);
      Atomics.store(state, 2, wire);
      Atomics.store(state, 3, cached);
      Atomics.store(state, 4, data.length);
      for (let offset = 0; offset < data.length; offset += output.length) {
        const part = data.subarray(offset, offset + output.length);
        output.set(part);
        Atomics.store(state, 1, part.length);
        publish(1);
        while (Atomics.load(state, 0) === 1) {
          const wait = Atomics.waitAsync(state, 0, 1, 65000);
          if ((wait.async ? await wait.value : wait.value) === 'timed-out') return;
        }
        if (Atomics.load(state, 0) === -1) return;
      }
      publish(2);
    } catch (_) {
      publish(-1);
    } finally {
      clearInterval(alive);
    }
  })();
}

/** The network worker's script: the functions above (and `more`), `main` its message handler. */
function masuFetchWorkerSource(main, more = []) {
  const vars = { MASU_CHUNKS, MASU_STALL_MS, MASU_GIVE_UP_MS, MASU_RETRY_STATUS, MASU_BAD_CHUNK, MASU_CHECKED, MASU_CACHE_MS, MASU_BRIDGE_HEADER, MASU_BRIDGE_BYTES, MASU_RECENT_BYTES, MASU_DIRECT_KEEP_MS, MASU_DIRECT_ASK_MS, MASU_DIRECT_MANY, MASU_CLASS, MASU_RATE, MASU_BURST, MASU_RESERVE, MASU_HINT_WIDTH, MASU_HINT_MAX, MASU_PACK_SPAN, MASU_PACK_MAX, MASU_CHUNK_DIR, MASU_HANDLE_MS, MASU_HANDLES, MASU_BUDGET_BYTES, MASU_BUDGET_SPIN, MASU_SAVING_BYTES, MASU_LOCK_WAIT_MS, MASU_FOLLOW, MASU_PUT_MS };
  const fns = [masuIsChunk, masuBlake3, masuBlake3Js, masuBlake3Simd, masuChunkOk, masuBackoff, masuPause, masuFetchOnce, masuNetReport, masuCacheStep, masuCache, masuChunkFile, masuOpfsStore, masuRange, masuKeepRecent, masuKeep, masuHeld, masuBlockKey, masuBudgetInit, masuBudgetWith, masuBudgetShare, masuTake, masuDrain, masuGate, masuWhole, masuRobustOnce, masuStartWhole, masuHint, masuHintPump, masuNextAdd, masuFollow, masuReadStart, masuReadEnd, masuDirectFresh, masuDirectValid, masuDirectMany, masuPackValid, masuPackNote, masuPackRun, masuDirect, masuHex, masuOpen, masuZstdSize, masuUnzstd, masuFetchPart, masuFetchRobust, masuWarmRun, masuFetchWorker];
  return [
    ...Object.entries(vars).map(([k, v]) => `var ${k} = ${JSON.stringify(v)};`),
    'var masuNet = { retrying: new Map(), channel: undefined, id: Math.random().toString(36).slice(2) };',
    'var masuCacheOpen, masuRecent = new Map(), masuDirectInfo = new Map(), masuDirectOff = false, masuZstd;',
    'var masuDirectAsking = new Map(), masuDirectManyOff = false, masuBudgetBuf, masuBudgetTag = 1, masuGameWaiting = 0, masuInflight = new Map(), masuRobustInflight = new Map(), masuKeys = new Map();',
    'var masuHints = [], masuHintsSeen = new Set(), masuHintsGoing = 0, masuPacks = new Map(), masuSaving = new Map(), masuSavingBytes = 0;',
    "var masuNext = new Map(), masuReadSeen = new Set(), masuReads = { device: [0, 0], saving: [0, 0], coming: [0, 0], none: [0, 0], followed: 0, hinted: 0 };",
    'masuWhole.LATE = {};',
    ...[...fns, ...more].map(String),
    main,
  ].join('\n');
}

/**
 * The game's store chunks fetched onto the device while it plays (masuWarm): `urls` in order,
 * `width` at a time, each the device lacks fetched as the game's reads would (from the bucket
 * itself when the server gives a way, else the server) and kept in Cache Storage, where the
 * game's later reads find it (masuFetchPart). The bucket answers each object after 1-5 s from
 * far away (worker2 to the Mac: 俺の姿が､透明に!'s CG mode and mode changes waited that long for
 * every file read first), so a file read later is on the device by then. `flag` (Int32Array
 * over a SharedArrayBuffer): [0] != 0 while the game waits for a read of its own (then only
 * `msg.busyWidth` of the `width` start fetches: a game that reads one file after another waits
 * nearly all the time), [1] != 0 to stop. Reports { done, fetched, bytes } now and then and
 * { end: true, ... }.
 */
async function masuWarmRun(msg, report) {
  const flag = new Int32Array(msg.flag);
  const cls = msg.cls ?? MASU_CLASS.warm;
  if (msg.next) masuNextAdd(msg.next);
  // (where the game's reads found their chunks, now and then while it plays: masuReads)
  const copy = () => JSON.parse(JSON.stringify(masuReads));
  const reads = msg.readsMs && setInterval(() => (Atomics.load(flag, 1) ? clearInterval(reads) : report({ reads: copy() })), msg.readsMs);
  const cache = await masuCache();
  let next = 0, done = 0, fetched = 0, bytes = 0, failed = 0;
  const stop = () => Atomics.load(flag, 1) !== 0;
  // (the game waits for a read of its own: in this worker, or as the page says)
  const busy = () => masuGameWaiting > 0 || Atomics.load(flag, 0) !== 0;
  const one = async (i) => {
    while (next < msg.urls.length && !stop()) {
      while (i >= (msg.busyWidth ?? 0) && busy() && !stop() && next < msg.urls.length) await new Promise((r) => setTimeout(r, 50));
      if (next >= msg.urls.length || stop()) break;
      const k = next++;
      // (how to fetch the next ones from the bucket: asked for MASU_DIRECT_MANY at a time,
      // a window ahead, so no fetch waits for the server's answer of its own)
      if (k % (MASU_DIRECT_MANY / 2) === 0) void masuDirectMany(msg.urls.slice(k, k + MASU_DIRECT_MANY));
      const url = msg.urls[k];
      try {
        const d = await masuWhole(url, { cls, size: msg.sizes?.[k], want: false });
        if (!d.cached) {
          fetched++;
          bytes += d.data?.length ?? 0;
        }
        // (not this worker's memory: the game's reads look on the device)
        if (!masuInflight.has(url)) masuRecent.delete(url);
      } catch (_) {
        failed++;
      }
      if (++done % 16 === 0) report({ done, fetched, bytes, failed });
    }
  };
  if (cache) await Promise.all(Array.from({ length: Math.max(1, msg.width) }, (_, i) => one(i)));
  report({ end: true, done, fetched, bytes, failed });
}

/**
 * Starts masuWarmRun in the network worker, beside the game's reads (one scheduler: a chunk the
 * game reads while it is being fetched ahead is fetched once, and the game's go first);
 * `report` gets its reports. `opt.cls`: MASU_CLASS.save for a game saved whole, `opt.sizes`
 * the chunks' lengths, `opt.next` what followed each chunk (masuNextAdd's pairs), `opt.readsMs`
 * how often to report { reads } (masuReads) while the game plays. Returns the port the reports come on.
 */
function masuWarm(urls, flag, width, report, busyWidth = 0, opt = {}) {
  const port = masuFetchConnect();
  port.onmessage = (e) => report(e.data);
  port.postMessage({ warm: { urls, flag, width, busyWidth, cls: opt.cls, sizes: opt.sizes, next: opt.next, readsMs: opt.readsMs } });
  return port;
}
if (typeof Module !== 'undefined') Module.masuWarm = masuWarm;

var masuFetchWorkerInstance;
var masuFetchPort;
function masuFetchConnect() {
  if (!masuFetchWorkerInstance) {
    const blob = new Blob([masuFetchWorkerSource('self.onmessage = e => { if (e.data.port) e.data.port.onmessage = masuFetchWorker; else masuFetchWorker(e); };')], {type:'text/javascript'});
    const url = URL.createObjectURL(blob);
    masuFetchWorkerInstance = new Worker(url);
    URL.revokeObjectURL(url);
    // (the page's budget of bucket requests: its own save takes from it too, web/src/net.ts)
    // (each worker its own tag in the budget's lock word: the page frees only a terminated one's)
    const budget = typeof Module !== 'undefined' ? Module.masuNetBudget : undefined;
    if (budget) masuFetchWorkerInstance.postMessage({ budget, tag: (masuFetchWorkerTag = masuFetchTagNext++) });
  }
  const channel = new MessageChannel();
  masuFetchWorkerInstance.postMessage({port:channel.port1}, [channel.port1]);
  return channel.port2;
}
var masuFetchTagNext = 2, masuFetchWorkerTag = 0;
function masuFetchDispose() {
  masuFetchWorkerInstance?.terminate();
  masuFetchWorkerInstance = undefined;
  // (terminated, so it holds the budget's lock no longer: freed once it surely stopped running)
  const budget = typeof Module !== 'undefined' ? Module.masuNetBudget : undefined;
  const dead = masuFetchWorkerTag;
  masuFetchWorkerTag = 0;
  if (budget && dead) setTimeout(() => masuBudgetReclaim(budget, dead), 1000);
}

var masuBridgeBuffer;
/** `url`'s bytes (`from`, `len`: only those; masuFetchPart), on a thread that may block. */
function masuFetchSync(url, from, len, block, cls) {
  if (typeof document !== 'undefined') throw new Error('synchronous reads on the page must be preloaded');
  if (!masuFetchPort && !masuFetchWorkerInstance) {
    const blob = new Blob([masuFetchWorkerSource('self.onmessage = masuFetchWorker;')], {type: 'text/javascript'});
    const location = URL.createObjectURL(blob);
    masuFetchWorkerInstance = new Worker(location);
    URL.revokeObjectURL(location);
  }
  // (this thread's buffer, kept: one read at a time per thread; a new one after a worker
  // that timed out, which may still write into the old)
  const shared = (masuBridgeBuffer ??= new SharedArrayBuffer(MASU_BRIDGE_HEADER + MASU_BRIDGE_BYTES));
  const state = new Int32Array(shared, 0, 8);
  const bytes = new Uint8Array(shared, MASU_BRIDGE_HEADER);
  Atomics.store(state, 0, 0);
  let result;
  let length = 0;
  (masuFetchPort || masuFetchWorkerInstance).postMessage({url, shared, from, len, block, cls});
  for (;;) {
    while (Atomics.load(state, 0) === 0) {
      if (Atomics.wait(state, 0, 0, 65000) === 'timed-out') {
        Atomics.store(state, 0, -1);
        Atomics.notify(state, 0);
        masuBridgeBuffer = undefined;
        masuFetchDispose();
        throw new Error('fetch worker timed out');
      }
    }
    const status = Atomics.load(state, 0);
    if (status === -1) throw new Error(`fetch failed: ${url}`);
    if (status === 2) break;
    // (copied once, into the body's place: the length came first)
    result ??= new Uint8Array(Atomics.load(state, 4));
    const n = Atomics.load(state, 1);
    result.set(bytes.subarray(0, n), length);
    length += n;
    Atomics.store(state, 0, 0);
    Atomics.notify(state, 0);
  }
  result ??= new Uint8Array(0);
  result.wire = Atomics.load(state, 2) >>> 0;
  result.cached = Atomics.load(state, 3);
  return result;
}

/** Chunks (`urls`) a read says the game reads soon: to the network worker (masuHint), not waited for. */
function masuFetchHint(urls) {
  if (!urls.length) return;
  const to = masuFetchPort || masuFetchWorkerInstance;
  to?.postMessage({ hint: urls });
}
