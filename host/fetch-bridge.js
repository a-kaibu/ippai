// The page owns a network worker and gives each game pthread a MessagePort.
// The network worker never relies on a blocked pthread's event loop. A bounded shared buffer transfers one response, with explicit acknowledgement.
//
// Mobile links drop, stall and come back: a request is not given up on the first failure. The
// worker retries network errors, stalled bodies (no byte for IPPAI_STALL_MS) and 5xx/408/429
// with backoff, waits while the browser says it is offline, and only gives up after
// IPPAI_GIVE_UP_MS (a 404 fails at once). While it retries it wakes the waiting pthread every
// few seconds, so the pthread's own timeout only catches a dead worker. Store chunks are named by
// their content: they are kept in Cache Storage (IPPAI_CHUNKS, web/src/net.ts uses the same one),
// so a second play, or a game saved to the device beforehand, needs no network for them. A chunk
// is checked against its name (BLAKE3, ippaiBlake3) before it is used or kept: one that does not
// match (cut or damaged on the way, a wrong answer) is fetched again, at most IPPAI_BAD_CHUNK
// times. Kept ones carry IPPAI_CHECKED; one kept before that (or by an older page) is checked
// once when read, then kept again with it.
// Retries are announced on the BroadcastChannel "ippai-net" (the page shows that it waits).
var IPPAI_CHUNKS = 'ippai-chunks-v1';
var IPPAI_STALL_MS = 20000;
var IPPAI_GIVE_UP_MS = 15 * 60000;
var IPPAI_RETRY_STATUS = [0, 408, 425, 429, 500, 502, 503, 504];
var IPPAI_BAD_CHUNK = 3;
var IPPAI_CHECKED = 'x-ippai-checked';

/** A store chunk: content-addressed, the same bytes forever. */
function ippaiIsChunk(url) {
  return /\/chunks\/[0-9a-f]{2}\/[0-9a-f]{32}$/.test(url);
}

/**
 * The first 16 bytes of `data`'s BLAKE3 hash, as hex: a store chunk's name (crates/ippai/src/pack.rs).
 * One-shot (a chunk at once: 4 MiB on average, 64 MiB at most), all of it in this function: the
 * network worker gets it as source.
 */
function ippaiBlake3(data) {
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

/** Whether `data` is store chunk `url`'s content (true for anything not a chunk). */
function ippaiChunkOk(url, data) {
  return !ippaiIsChunk(url) || ippaiBlake3(data) === url.slice(url.lastIndexOf('/') + 1);
}

/** The delay before retry `attempt` (from 0): exponential with jitter, at most 8 s. */
function ippaiBackoff(attempt) {
  return Math.min(8000, 300 * 2 ** attempt) * (0.5 + Math.random() * 0.5);
}

/** Waits `ms`; while the browser reports no network, until it reports one again (at most 30 s). */
async function ippaiPause(ms) {
  await new Promise((r) => setTimeout(r, ms));
  // (polled: a worker's 'online' event is not dependable)
  for (let i = 0; i < 60 && globalThis.navigator?.onLine === false; i++) await new Promise((r) => setTimeout(r, 500));
}

/** One try: the body read in pieces; aborted when no byte arrives for `stallMs`. */
async function ippaiFetchOnce(url, stallMs, reload, headers) {
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

var ippaiNet = { retrying: new Map(), channel: undefined, id: Math.random().toString(36).slice(2) };
/** Tells the page how many requests are being retried now (and the last error). */
function ippaiNetReport(error) {
  try {
    ippaiNet.channel ??= new BroadcastChannel('ippai-net');
    ippaiNet.channel.unref?.();
    ippaiNet.channel.postMessage({ id: ippaiNet.id, retrying: ippaiNet.retrying.size, error: String(error ?? '') });
  } catch (_) {}
}

/** Cache Storage for chunks (opened once: every read of a block asks for it). */
var ippaiCacheOpen;
function ippaiCache() {
  return (ippaiCacheOpen ??= (async () => {
    try {
      return typeof caches === 'undefined' ? null : await caches.open(IPPAI_CHUNKS);
    } catch (_) {
      return null;
    }
  })());
}

/**
 * "<store path>#<from>+<len>/<block>[!a]" (vfs.rs asks for blocks of a chunk so, `block` bytes
 * each; "!a": by its fetcher threads, ahead of the game's reads): [path, from, len, block,
 * class (IPPAI_CLASS)]; a plain path: [path].
 */
function ippaiRange(name) {
  const m = /^(.*)#(\d+)\+(\d+)(?:\/(\d+))?(!a)?$/.exec(name);
  return m ? [m[1], +m[2], +m[3], m[4] === undefined ? undefined : +m[4], m[5] ? IPPAI_CLASS.ahead : IPPAI_CLASS.game] : [name];
}

/**
 * Whole chunks this worker fetched lately, newest last (IPPAI_RECENT_BYTES, and the newest one
 * whatever its size): a read asks for a chunk's blocks one range after another, and the first
 * fetched it whole (the network has no ranges of a zstd-coded chunk). They are answered from
 * here until Cache Storage has the chunk (its put is not awaited), or where there is none.
 */
var IPPAI_RECENT_BYTES = 32 << 20;
var ippaiRecent = new Map();
function ippaiKeepRecent(url, data) {
  ippaiRecent.delete(url);
  ippaiRecent.set(url, data);
  let bytes = 0;
  for (const d of ippaiRecent.values()) bytes += d.length;
  for (const [k, d] of ippaiRecent) {
    if (bytes <= IPPAI_RECENT_BYTES || ippaiRecent.size === 1) break;
    ippaiRecent.delete(k);
    bytes -= d.length;
  }
}
/** Cache Storage's key for block `k` (of `block` bytes) of chunk `url`, kept alone. */
function ippaiBlockKey(url, k, block) {
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
var IPPAI_CLASS = { game: 0, ahead: 1, hint: 2, warm: 3, save: 4 };
/** Requests per second the budget refills by, and holds at most. */
var IPPAI_RATE = 9;
var IPPAI_BURST = 40;
/** Requests each class leaves to the classes above it; the game's may run the budget into debt
 * (to -IPPAI_RATE * 5: a game waits for no fetch ahead's budget, and its bursts are short). */
var IPPAI_RESERVE = [-IPPAI_RATE * 5, 0, 8, 16, 16];

/**
 * The shared budget: an Int32 lock word, then [tokens, last refill (Date.now())] as Float64,
 * over a SharedArrayBuffer the page makes (web/src/net.ts netBudget: its own save uses it too)
 * or this worker's own.
 */
var ippaiBudgetBuf;
function ippaiBudgetInit(buf) {
  ippaiBudgetBuf = buf ?? ippaiBudgetBuf ?? (() => {
    const b = new (typeof SharedArrayBuffer === 'undefined' ? ArrayBuffer : SharedArrayBuffer)(24);
    new Float64Array(b, 8, 2).set([IPPAI_BURST, Date.now()]);
    return b;
  })();
  return ippaiBudgetBuf;
}
/** `f(values)` with the budget's lock held (a few instructions: spun for). */
function ippaiBudgetWith(f) {
  const buf = ippaiBudgetInit();
  const lock = new Int32Array(buf, 0, 1);
  const v = new Float64Array(buf, 8, 2);
  while (Atomics.compareExchange(lock, 0, 0, 1) !== 0);
  try {
    return f(v);
  } finally {
    Atomics.store(lock, 0, 0);
  }
}
/** Takes one request of class `cls` from the budget (refilled to `now`): whether it may go. */
function ippaiTake(cls, now = Date.now()) {
  return ippaiBudgetWith((v) => {
    const dt = Math.max(0, now - v[1]) / 1000;
    v[0] = Math.min(IPPAI_BURST, v[0] + dt * IPPAI_RATE);
    v[1] = Math.max(v[1], now);
    if (v[0] - 1 < (IPPAI_RESERVE[cls] ?? 0)) return false;
    v[0] -= 1;
    return true;
  });
}
/** The bucket said 429: nobody goes for `ms` (the budget into debt by that much). */
function ippaiDrain(ms) {
  ippaiBudgetWith((v) => {
    v[0] = Math.min(v[0], -(IPPAI_RATE * ms) / 1000);
  });
}
/** Waits until `job` (its class read again each time) may make a request of the bucket. */
async function ippaiGate(job) {
  while (!ippaiTake(job.cls)) await new Promise((r) => setTimeout(r, 50));
}

/** The game's reads waiting in this worker now (fetches ahead give way while there are any). */
var ippaiGameWaiting = 0;

/** Chunks being fetched whole now: url -> { promise, job }. */
var ippaiInflight = new Map();

/**
 * Chunk `url` whole, fetched once whoever asks: from this worker's memory, Cache Storage, a
 * fetch of it already on its way (made `cls`'s if more urgent), else from the bucket (or the
 * server). `want`: the data is needed (not only kept on the device). `size`: its decoded length
 * when known. Classes past the game's hold the chunk's Web Lock while they fetch, so the page's
 * own save does not fetch it again meanwhile (the game's do not wait for a lock).
 */
function ippaiWhole(url, { cls = 0, size, want = true, directOnly = false } = {}) {
  const mem = ippaiRecent.get(url);
  if (mem) return Promise.resolve({ data: mem, wire: 0, cached: 1 });
  // (null: not from the bucket; the server then, as the asker wants it)
  const rest = (r) => r ?? (directOnly ? null : ippaiFetchRobust(url, cls ? { giveUpMs: 60000 } : {}));
  const going = ippaiInflight.get(url);
  if (going) {
    going.job.cls = Math.min(going.job.cls, cls);
    return going.promise.then((r) => (r && want && !r.data ? ippaiWhole(url, { cls, size, want, directOnly }) : rest(r)));
  }
  const job = { cls };
  const run = async () => {
    const cache = await ippaiCache();
    const hit = await cache?.match(url).catch(() => undefined);
    if (hit && hit.headers.get(IPPAI_CHECKED) === '1') {
      if (!want) return { data: null, wire: 0, cached: 1 };
      try {
        return { data: new Uint8Array(await hit.arrayBuffer()), wire: 0, cached: 1 };
      } catch (_) {}
    }
    return ippaiDirect(url, job, size);
  };
  const locks = cls > 0 ? globalThis.navigator?.locks : undefined;
  const promise = (locks ? locks.request(`ippai-chunk:${url.slice(-32)}`, run) : run()).finally(() => {
    if (ippaiInflight.get(url)?.promise === promise) ippaiInflight.delete(url);
  });
  ippaiInflight.set(url, { promise, job });
  return promise.then(rest);
}

/**
 * Chunks a read says the game reads soon (vfs.rs `hint`: the next files of a folder, the next
 * chunks of an archive): fetched onto the device, IPPAI_HINT_WIDTH at a time, newest hints first
 * (a scene that moved on leaves the old ones behind). The budget's reserve keeps them from the
 * game's requests; the line is not what limits (16 at once from the bucket: 45 MB/s), the
 * bucket's wait for each object is.
 */
var IPPAI_HINT_WIDTH = 6;
var IPPAI_HINT_MAX = 256;
var ippaiHints = [];
var ippaiHintsSeen = new Set();
var ippaiHintsGoing = 0;
function ippaiHint(urls) {
  for (const u of urls.slice(0, IPPAI_HINT_MAX).reverse()) {
    if (ippaiHintsSeen.has(u) || ippaiInflight.has(u) || ippaiRecent.has(u)) continue;
    ippaiHintsSeen.add(u);
    ippaiHints.push(u);
  }
  if (ippaiHintsSeen.size > 1 << 16) ippaiHintsSeen.clear();
  // (the oldest dropped)
  if (ippaiHints.length > IPPAI_HINT_MAX) ippaiHints.splice(0, ippaiHints.length - IPPAI_HINT_MAX);
  void ippaiDirectMany(ippaiHints.slice(-IPPAI_DIRECT_MANY));
  ippaiHintPump();
}
function ippaiHintPump() {
  while (ippaiHintsGoing < IPPAI_HINT_WIDTH && ippaiHints.length) {
    const u = ippaiHints.pop();
    ippaiHintsGoing++;
    ippaiWhole(u, { cls: IPPAI_CLASS.hint, want: false })
      .catch(() => {})
      .finally(() => {
        ippaiHintsGoing--;
        // (not this worker's memory: the game's reads find it on the device)
        if (!ippaiInflight.has(u)) ippaiRecent.delete(u);
        ippaiHintPump();
      });
  }
}

/**
 * A store chunk straight from the bucket (S3) when the server gives a way (ippai serve's
 * /store-direct: a presigned URL, the object's AES-256-GCM key and UUID): fetched, decrypted
 * (nonce first, the UUID as associated data), zstd-decoded (sake/snapz.mjs) and checked
 * against its name, then kept as one from the server would be. The bucket answers every object
 * after ~2 s whatever its size, so many at once go much faster than through the server's line
 * (worker2: 4 at once 3 MB/s; this page's own line has more). null: no way (an older server, a
 * store without S3, a chunk only on the server, an expired URL that failed): the server, as before.
 */
var IPPAI_DIRECT_KEEP_MS = 3600000;
/** How long the server may take to say how to fetch a chunk from the bucket (a stuck server, as
 * worker2's at 19:58 JST 2026-11-09, held a read 80 s): past it, the server's own /store. */
var IPPAI_DIRECT_ASK_MS = 10000;
var ippaiDirectInfo = new Map();
var ippaiDirectOff = false;
var ippaiZstd;
/** Whether `info` (from /store-direct) still works. */
function ippaiDirectFresh(info) {
  return !!info && Date.now() - info.at <= IPPAI_DIRECT_KEEP_MS;
}

/** A well-formed answer of /store-direct for one chunk. */
function ippaiDirectValid(info) {
  return !!info && typeof info.url === 'string' && /^[0-9a-f]{64}$/.test(info.key) && /^[0-9a-f-]{36}$/.test(info.object);
}

/**
 * Asks the server how to fetch many chunks from the bucket at once (`POST store-direct`, a
 * list of chunk paths: crates/ippai/src/store_api.rs `direct_many`), IPPAI_DIRECT_MANY at a
 * time, and keeps the answers: a game's read of one of them then goes to the bucket at once,
 * without first a round trip to the server (which a read of every file the game had not read
 * yet paid before the bucket's own wait). Those the server does not name are only on the server.
 * An older server without the route: asked one by one, as before (ippaiDirect).
 */
var IPPAI_DIRECT_MANY = 512;
var ippaiDirectAsking = new Map();
var ippaiDirectManyOff = false;
async function ippaiDirectMany(urls) {
  const want = urls.filter((u) => ippaiIsChunk(u) && u.includes('/store/chunks/') && !ippaiDirectFresh(ippaiDirectInfo.get(u)) && !ippaiDirectAsking.has(u));
  if (ippaiDirectOff || ippaiDirectManyOff || !want.length || typeof fetch === 'undefined') return;
  const runs = [];
  for (let i = 0; i < want.length; i += IPPAI_DIRECT_MANY) {
    const part = want.slice(i, i + IPPAI_DIRECT_MANY);
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
          signal: AbortSignal.timeout(IPPAI_DIRECT_ASK_MS * 3),
        });
      } catch (_) {
        return;
      }
      if (r.status === 501) ippaiDirectOff = true;
      // (a server without the route)
      if (r.status === 404 || r.status === 405) ippaiDirectManyOff = true;
      if (!r.ok) return;
      let all;
      try {
        all = await r.json();
      } catch (_) {
        return;
      }
      const now = Date.now();
      if (ippaiDirectInfo.size > 20000) ippaiDirectInfo.clear();
      for (const u of part) {
        const info = all?.[u.slice(u.lastIndexOf('/store/chunks/') + 7)];
        // (not named: only on the server, which then answers it as before)
        const ok = ippaiDirectValid(info);
        ippaiDirectInfo.set(u, ok ? { url: info.url, key: info.key, object: info.object, pack: ippaiPackValid(info.pack) ? info.pack : undefined, at: now } : { none: true, at: now });
        if (ok) ippaiPackNote(u, ippaiDirectInfo.get(u));
      }
    })();
    for (const u of part) ippaiDirectAsking.set(u, run);
    runs.push(run.finally(() => part.forEach((u) => ippaiDirectAsking.get(u) === run && ippaiDirectAsking.delete(u))));
  }
  await Promise.all(runs);
}

/**
 * Chunk `url` from the bucket itself (as above). `job.cls` is the request's class
 * (IPPAI_CLASS; read again while it waits for the budget: a game read that joins a fetch ahead
 * of the same chunk makes it the game's), `size` its decoded length when known; `alone`: not
 * with its delivery pack's neighbours.
 */
async function ippaiDirect(url, job = { cls: 0 }, size, alone = false) {
  const at = url.lastIndexOf('/store/chunks/');
  if (ippaiDirectOff || at < 0 || !ippaiIsChunk(url) || !globalThis.crypto?.subtle || typeof fetch === 'undefined') return null;
  const base = url.slice(0, at + 1);
  let info = ippaiDirectInfo.get(url);
  // (being asked for with others: that answer)
  if (!ippaiDirectFresh(info) && ippaiDirectAsking.has(url)) {
    await ippaiDirectAsking.get(url);
    info = ippaiDirectInfo.get(url);
  }
  if (!ippaiDirectFresh(info)) {
    let r;
    try {
      r = await fetch(`${base}store-direct/${url.slice(at + 7)}`, { cache: 'no-store', signal: AbortSignal.timeout(IPPAI_DIRECT_ASK_MS) });
    } catch (_) {
      return null;
    }
    // (501: a store without S3; 404: this chunk is only on the server)
    if (r.status === 501) ippaiDirectOff = true;
    if (!r.ok) return null;
    try {
      info = { ...(await r.json()), at: Date.now() };
    } catch (e) {
      // (not JSON: a server without /store-direct answered with something else; cut short by
      // the time limit: only this once)
      if (e?.name !== 'TimeoutError' && e?.name !== 'AbortError') ippaiDirectOff = true;
      return null;
    }
    if (!ippaiDirectValid(info)) return null;
    if (!ippaiPackValid(info.pack)) delete info.pack;
    if (ippaiDirectInfo.size > 20000) ippaiDirectInfo.clear();
    ippaiDirectInfo.set(url, info);
    ippaiPackNote(url, info);
  }
  if (info.none) return null;
  // (with the chunks after it in its delivery pack, one GET for them all; failing that, alone)
  if (info.pack && !alone) {
    const r = await ippaiPackRun(url, info, job, size, base);
    if (r) return r;
  }
  for (let bad = 0; bad < IPPAI_BAD_CHUNK; bad++) {
    let got;
    try {
      await ippaiGate(job);
      got = await ippaiFetchRobust(info.url, { giveUpMs: 60000 });
    } catch (_) {
      ippaiDirectInfo.delete(url);
      return null;
    }
    const plain = await ippaiOpen(info, got.data).catch(() => null);
    const data = plain && (await ippaiUnzstd(base, plain, size).catch(() => null));
    if (data && ippaiChunkOk(url, data)) {
      ippaiKeepRecent(url, data);
      const cache = await ippaiCache();
      if (cache) cache.put(url, new Response(data, { headers: { [IPPAI_CHECKED]: '1', 'x-ippai-size': String(data.length) } })).catch(() => {});
      return { data, wire: got.wire, cached: got.cached };
    }
  }
  return null;
}

/**
 * Delivery packs (docs/s3-reads.md; crates/ippai/src/storage.rs pack_objects): a game's chunk
 * objects, as the bucket has them, side by side in big objects in the order the game reads
 * them. A /store-direct answer names where its chunk's object is in one (`pack`: id, URL,
 * offset, length); a read of a chunk then takes the chunks after it in the pack with the same
 * ranged GET (those not on the device nor coming already), up to IPPAI_PACK_SPAN bytes for its
 * class: the bucket's wait for an object (0.4-2 s) and one request of its budget paid once for
 * them all. A game's read takes fewer (it waits for the whole range); a save to the device many.
 */
var IPPAI_PACK_SPAN = [4 << 20, 8 << 20, 16 << 20, 32 << 20, 32 << 20];
var IPPAI_PACK_MAX = 64;
/** pack id -> Map(offset -> chunk URL), from the answers. */
var ippaiPacks = new Map();
function ippaiPackValid(p) {
  return !!p && typeof p.url === 'string' && /^[0-9a-f-]{36}$/.test(p.id) && Number.isSafeInteger(p.offset) && p.offset >= 0 && Number.isSafeInteger(p.len) && p.len >= 28;
}
function ippaiPackNote(url, info) {
  if (!info?.pack) return;
  if (ippaiPacks.size > 4096) ippaiPacks.clear();
  let m = ippaiPacks.get(info.pack.id);
  if (!m) ippaiPacks.set(info.pack.id, (m = new Map()));
  m.set(info.pack.offset, url);
}

/** Chunk `url` with the chunks after it in its pack (as above): its result, or null (then alone). */
async function ippaiPackRun(url, info, job, size, base) {
  const p = info.pack;
  const at = ippaiPacks.get(p.id);
  const run = [{ url, info }];
  const span = IPPAI_PACK_SPAN[job.cls] ?? IPPAI_PACK_SPAN.at(-1);
  const cache = await ippaiCache();
  let end = p.offset + p.len;
  let last = end;
  while (at && run.length < IPPAI_PACK_MAX) {
    const u = at.get(end);
    const n = u && ippaiDirectInfo.get(u);
    if (!n || !ippaiDirectFresh(n) || n.pack?.id !== p.id || end + n.pack.len - p.offset > span) break;
    end += n.pack.len;
    // (here or coming already: its bytes only if a later one is taken)
    if (ippaiInflight.has(u) || ippaiRecent.has(u)) continue;
    const hit = await cache?.match(u).catch(() => undefined);
    if (hit && hit.headers.get(IPPAI_CHECKED) === '1') continue;
    run.push({ url: u, info: n });
    last = end;
  }
  // (the neighbours are coming: a read of one of them joins this GET)
  let done;
  const all = new Promise((r) => (done = r));
  for (const n of run.slice(1)) {
    const job2 = { cls: job.cls };
    const promise = all
      .then((m) => m.get(n.url) ?? ippaiDirect(n.url, job2, undefined, true))
      .finally(() => {
        if (ippaiInflight.get(n.url)?.promise === promise) ippaiInflight.delete(n.url);
      });
    ippaiInflight.set(n.url, { promise, job: job2 });
  }
  const out = new Map();
  try {
    await ippaiGate(job);
    const got = await ippaiFetchRobust(p.url, { giveUpMs: 60000, range: [p.offset, last - p.offset] });
    // (a bucket that sends the whole pack for a range: the range from it)
    const bytes = got.partial ? got.data : got.data.subarray(p.offset, last);
    if (bytes.length === last - p.offset)
      for (const n of run) {
        const off = n.info.pack.offset - p.offset;
        const plain = await ippaiOpen(n.info, bytes.subarray(off, off + n.info.pack.len)).catch(() => null);
        const data = plain && (await ippaiUnzstd(base, plain, n.url === url ? size : undefined).catch(() => null));
        if (!data || !ippaiChunkOk(n.url, data)) continue;
        out.set(n.url, { data, wire: n.url === url ? got.wire : 0, cached: got.cached });
        if (cache) cache.put(n.url, new Response(data, { headers: { [IPPAI_CHECKED]: '1', 'x-ippai-size': String(data.length) } })).catch(() => {});
      }
  } catch (_) {}
  done(out);
  const r = out.get(url);
  if (r) ippaiKeepRecent(url, r.data);
  return r ?? null;
}

/** Hex `h` as bytes. */
function ippaiHex(h) {
  return Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));
}

/** Object `cipher` (nonce, ciphertext and tag, as crates/ippai/src/envelope.rs seals it) opened with `info`'s key. */
async function ippaiOpen(info, cipher) {
  if (cipher.length < 28) throw new Error('short');
  const key = await crypto.subtle.importKey('raw', ippaiHex(info.key), 'AES-GCM', false, ['decrypt']);
  const plain = await crypto.subtle.decrypt({ name: 'AES-GCM', iv: cipher.slice(0, 12), additionalData: ippaiHex(info.object.replace(/-/g, '')), tagLength: 128 }, key, cipher.slice(12));
  return new Uint8Array(plain);
}

/** zstd frame `frame` decoded by sake's snapz (sake/snapz.mjs beside the store): `size` bytes when known, else a chunk is 16 MiB at most (4x FastCDC's 4 MiB average), 64 MiB tried once more. */
async function ippaiUnzstd(base, frame, size) {
  ippaiZstd ??= import(`${base}sake/snapz.mjs`).then((m) => m.default());
  const z = await ippaiZstd;
  // (the length when known, from the trees' lists: no 16 MiB buffer for every chunk at once,
  // which several side by side on a phone add up to)
  for (const cap of size > 0 ? [size] : [16 << 20, 64 << 20]) {
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
 * Bytes `from..from + len` of `url`'s (whole when `from` is undefined), as ippaiFetchRobust's
 * result. Of a chunk, from the first place that has them: this worker's memory (a chunk it
 * fetched whole lately), Cache Storage's copy of the whole chunk (checked when it was kept;
 * Blob.slice reads only those bytes, whatever the chunk's size), its blocks kept alone (`block`
 * bytes each, as vfs.rs asks for them: `from` is a block's start); else a range from the server
 * (only what is read crosses the network, kept as blocks); a server without ranges sends the
 * whole chunk, then checked and kept whole (and in this worker's memory for the next blocks).
 */
async function ippaiFetchPart(url, from, len, block, cls = 0) {
  if (from === undefined) return ippaiIsChunk(url) ? ippaiWhole(url, { cls }) : ippaiFetchRobust(url);
  const part = (d) => d.subarray(Math.min(from, d.length), Math.min(from + len, d.length));
  const mem = ippaiRecent.get(url);
  if (mem) return { data: part(mem), wire: 0, cached: 1 };
  if (!ippaiIsChunk(url)) return ippaiFetchPart(url).then((r) => ({ ...r, data: part(r.data) }));
  const cache = await ippaiCache();
  block ||= len || 1;
  const first = Math.floor(from / block);
  const last = Math.max(first, Math.ceil((from + len) / block) - 1);
  if (cache) {
    const hit = await cache.match(url).catch(() => undefined);
    if (hit && hit.headers.get(IPPAI_CHECKED) === '1') {
      try {
        const blob = await hit.blob();
        return { data: new Uint8Array(await blob.slice(from, from + len).arrayBuffer()), wire: 0, cached: 1 };
      } catch (_) {}
    }
    // (blocks kept alone: all of them, or the network)
    const parts = [];
    for (let k = first; k <= last; k++) {
      const b = await cache.match(ippaiBlockKey(url, k, block)).catch(() => undefined);
      if (!b) break;
      parts.push(new Uint8Array(await b.arrayBuffer().catch(() => new ArrayBuffer(0))));
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
  const direct = await ippaiWhole(url, { cls, directOnly: true });
  if (direct?.data) return { ...direct, data: part(direct.data) };
  // (the whole blocks asked for: what is kept)
  const at = first * block;
  const r = await ippaiFetchRobust(url, { range: [at, (last + 1) * block - at] });
  if (!r.partial) return { ...r, data: part(r.data) };
  if (cache) {
    for (let k = first; k <= last; k++) {
      const d = r.data.subarray((k - first) * block, (k + 1 - first) * block);
      // (a short block only at the chunk's end)
      if (d.length === block || (r.total !== undefined && k * block + d.length === r.total))
        cache.put(ippaiBlockKey(url, k, block), new Response(d.slice(), { headers: { 'x-ippai-size': String(d.length) } })).catch(() => {});
    }
  }
  return { ...r, data: r.data.subarray(from - at, from - at + len) };
}

/**
 * `url`'s bytes, retried as above: { data, wire (bytes over the network), cached (1: no
 * network) }. Throws for a final failure.
 */
async function ippaiFetchRobust(url, opt = {}) {
  const stallMs = opt.stallMs ?? IPPAI_STALL_MS;
  const giveUpMs = opt.giveUpMs ?? IPPAI_GIVE_UP_MS;
  const chunk = ippaiIsChunk(url);
  const cache = chunk ? await ippaiCache() : null;
  // opt.range [from, len]: those bytes of the decoded chunk (ippaiFetchPart looked on the device
  // already); the answer has `partial` (a 206 of them, ippai serve's store_api.rs) or is the
  // whole chunk (a server without ranges), checked and kept as any
  const range = opt.range;
  const hit = range ? undefined : await cache?.match(url).catch(() => undefined);
  if (hit) {
    try {
      const data = new Uint8Array(await hit.arrayBuffer());
      if (hit.headers.get(IPPAI_CHECKED) === '1') return { data, wire: 0, cached: 1 };
      if (ippaiChunkOk(url, data)) {
        cache.put(url, new Response(data, { headers: { [IPPAI_CHECKED]: '1', 'x-ippai-size': String(data.length) } })).catch(() => {});
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
        // (after a chunk that did not match, past the browser's HTTP cache: it keeps chunks for good)
        const { response, data } = await ippaiFetchOnce(url, stallMs, bad > 0, range && { range: `bytes=${range[0]}-${range[0] + range[1] - 1}` });
        if (data && range && response.status === 206) {
          // (a range: its place and length checked; a block has no name of its own to check)
          const m = /^bytes (\d+)-(\d+)\/(\d+)$/.exec(response.headers?.get?.('content-range') ?? '');
          if (m && +m[1] === range[0] && data.length === +m[2] - +m[1] + 1 && (data.length === range[1] || +m[2] === +m[3] - 1))
            return { data, wire: data.length, cached: 0, partial: true, total: +m[3] };
          if (++bad >= IPPAI_BAD_CHUNK) throw Object.assign(new Error(`range is not what was asked ${bad} times`), { final: true });
          throw new Error('range is not what was asked');
        }
        if (range && response.status === 416) return { data: new Uint8Array(0), wire: 0, cached: 0, partial: true };
        if (data && !ippaiChunkOk(url, data)) {
          if (++bad >= IPPAI_BAD_CHUNK) throw Object.assign(new Error(`chunk does not match its name ${bad} times`), { final: true });
          throw new Error('chunk does not match its name');
        }
        if (data) {
          if (chunk) ippaiKeepRecent(url, data);
          // (another origin's timing without Timing-Allow-Origin, as the bucket's: all zeros,
          // which said 0 bytes crossed the network for every chunk from the bucket)
          const t = typeof performance !== 'undefined' && performance.getEntriesByName?.(url).at(-1);
          const timing = t && (t.transferSize > 0 || t.decodedBodySize > 0) ? t : undefined;
          const wire = timing ? timing.transferSize : +(response.headers?.get?.('content-length') || data.length);
          const cached = timing && timing.transferSize === 0 && timing.decodedBodySize > 0 ? 1 : 0;
          if (cache) cache.put(url, new Response(data, { headers: { [IPPAI_CHECKED]: '1', 'x-ippai-size': String(data.length) } })).catch(() => {});
          return { data, wire, cached };
        }
        if (!IPPAI_RETRY_STATUS.includes(response.status)) throw Object.assign(new Error(`HTTP ${response.status}`), { final: true });
        error = `HTTP ${response.status}`;
        // (seconds; a date form counts as none)
        after = Math.min((+(response.headers?.get?.('retry-after')) || 0) * 1000, 30000);
        // (too many requests: every class waits, not only this one)
        if (response.status === 429) ippaiDrain(after || 5000);
      } catch (e) {
        if (e.final) throw e;
        error = e?.message ?? e;
      }
      if (Date.now() - start > giveUpMs) throw new Error(`${error} (gave up)`);
      ippaiNet.retrying.set(key, attempt);
      ippaiNetReport(error);
      await ippaiPause(after || ippaiBackoff(attempt));
      after = 0;
    }
  } finally {
    if (ippaiNet.retrying.delete(key)) ippaiNetReport('');
  }
}

/**
 * The transfer buffer a synchronous read and the network worker share: a header (status,
 * part length, wire bytes, from the HTTP cache, body length) and IPPAI_BRIDGE_BYTES of body
 * at a time, one per thread. A/B (node workers, 400 reads, median of 5): a 256 KiB chunk took
 * 0.369 ms with 64 KiB parts copied twice (the old protocol), 0.342 ms copied once, 0.214 ms
 * in one 256 KiB part, 0.216 ms with 1 MiB; a 64 KiB read 0.135-0.139 ms with any. A read
 * asks for 256 KiB blocks (crates/kura/src/vfs.rs), 1 MiB at most at a time; a whole chunk
 * (the page's) goes in several parts.
 */
var IPPAI_BRIDGE_HEADER = 32;
var IPPAI_BRIDGE_BYTES = 262144;

function ippaiFetchWorker(event) {
  const m = event.data;
  // (not a read: chunks the game reads soon, the budget the page shares, a warm or save run)
  if (m.hint) return ippaiHint(m.hint);
  if (m.budget) return void ippaiBudgetInit(m.budget);
  if (m.warm) return void ippaiWarmRun(m.warm, (r) => event.target?.postMessage?.(r));
  const { url, shared, from, len, block } = m;
  const cls = m.cls ?? IPPAI_CLASS.game;
  const state = new Int32Array(shared, 0, 8);
  const output = new Uint8Array(shared, IPPAI_BRIDGE_HEADER);
  const publish = (status) => {
    Atomics.store(state, 0, status);
    Atomics.notify(state, 0);
  };
  // (the pthread waits with a timeout: a wake-up without news tells it this worker is alive)
  const alive = setInterval(() => Atomics.notify(state, 0), 5000);
  if (cls === IPPAI_CLASS.game) ippaiGameWaiting++;
  (async () => {
    try {
      const { data, wire, cached } = await ippaiFetchPart(url, from, len, block, cls).finally(() => {
        if (cls === IPPAI_CLASS.game) ippaiGameWaiting--;
      });
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
function ippaiFetchWorkerSource(main, more = []) {
  const vars = { IPPAI_CHUNKS, IPPAI_STALL_MS, IPPAI_GIVE_UP_MS, IPPAI_RETRY_STATUS, IPPAI_BAD_CHUNK, IPPAI_CHECKED, IPPAI_BRIDGE_HEADER, IPPAI_BRIDGE_BYTES, IPPAI_RECENT_BYTES, IPPAI_DIRECT_KEEP_MS, IPPAI_DIRECT_ASK_MS, IPPAI_DIRECT_MANY, IPPAI_CLASS, IPPAI_RATE, IPPAI_BURST, IPPAI_RESERVE, IPPAI_HINT_WIDTH, IPPAI_HINT_MAX, IPPAI_PACK_SPAN, IPPAI_PACK_MAX };
  const fns = [ippaiIsChunk, ippaiBlake3, ippaiChunkOk, ippaiBackoff, ippaiPause, ippaiFetchOnce, ippaiNetReport, ippaiCache, ippaiRange, ippaiKeepRecent, ippaiBlockKey, ippaiBudgetInit, ippaiBudgetWith, ippaiTake, ippaiDrain, ippaiGate, ippaiWhole, ippaiHint, ippaiHintPump, ippaiDirectFresh, ippaiDirectValid, ippaiDirectMany, ippaiPackValid, ippaiPackNote, ippaiPackRun, ippaiDirect, ippaiHex, ippaiOpen, ippaiUnzstd, ippaiFetchPart, ippaiFetchRobust, ippaiWarmRun, ippaiFetchWorker];
  return [
    ...Object.entries(vars).map(([k, v]) => `var ${k} = ${JSON.stringify(v)};`),
    'var ippaiNet = { retrying: new Map(), channel: undefined, id: Math.random().toString(36).slice(2) };',
    'var ippaiCacheOpen, ippaiRecent = new Map(), ippaiDirectInfo = new Map(), ippaiDirectOff = false, ippaiZstd;',
    'var ippaiDirectAsking = new Map(), ippaiDirectManyOff = false, ippaiBudgetBuf, ippaiGameWaiting = 0, ippaiInflight = new Map();',
    'var ippaiHints = [], ippaiHintsSeen = new Set(), ippaiHintsGoing = 0, ippaiPacks = new Map();',
    ...[...fns, ...more].map(String),
    main,
  ].join('\n');
}

/**
 * The game's store chunks fetched onto the device while it plays (ippaiWarm): `urls` in order,
 * `width` at a time, each the device lacks fetched as the game's reads would (from the bucket
 * itself when the server gives a way, else the server) and kept in Cache Storage, where the
 * game's later reads find it (ippaiFetchPart). The bucket answers each object after 1-5 s from
 * far away (worker2 to the Mac: 俺の姿が､透明に!'s CG mode and mode changes waited that long for
 * every file read first), so a file read later is on the device by then. `flag` (Int32Array
 * over a SharedArrayBuffer): [0] != 0 while the game waits for a read of its own (then only
 * `msg.busyWidth` of the `width` start fetches: a game that reads one file after another waits
 * nearly all the time), [1] != 0 to stop. Reports { done, fetched, bytes } now and then and
 * { end: true, ... }.
 */
async function ippaiWarmRun(msg, report) {
  const flag = new Int32Array(msg.flag);
  const cls = msg.cls ?? IPPAI_CLASS.warm;
  const cache = await ippaiCache();
  let next = 0, done = 0, fetched = 0, bytes = 0, failed = 0;
  const stop = () => Atomics.load(flag, 1) !== 0;
  // (the game waits for a read of its own: in this worker, or as the page says)
  const busy = () => ippaiGameWaiting > 0 || Atomics.load(flag, 0) !== 0;
  const one = async (i) => {
    while (next < msg.urls.length && !stop()) {
      while (i >= (msg.busyWidth ?? 0) && busy() && !stop() && next < msg.urls.length) await new Promise((r) => setTimeout(r, 50));
      if (next >= msg.urls.length || stop()) break;
      const k = next++;
      // (how to fetch the next ones from the bucket: asked for IPPAI_DIRECT_MANY at a time,
      // a window ahead, so no fetch waits for the server's answer of its own)
      if (k % (IPPAI_DIRECT_MANY / 2) === 0) void ippaiDirectMany(msg.urls.slice(k, k + IPPAI_DIRECT_MANY));
      const url = msg.urls[k];
      try {
        const d = await ippaiWhole(url, { cls, size: msg.sizes?.[k], want: false });
        if (!d.cached) {
          fetched++;
          bytes += d.data?.length ?? 0;
        }
        // (not this worker's memory: the game's reads look on the device)
        if (!ippaiInflight.has(url)) ippaiRecent.delete(url);
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
 * Starts ippaiWarmRun in the network worker, beside the game's reads (one scheduler: a chunk the
 * game reads while it is being fetched ahead is fetched once, and the game's go first);
 * `report` gets its reports. `opt.cls`: IPPAI_CLASS.save for a game saved whole, `opt.sizes`
 * the chunks' lengths. Returns the port the reports come on.
 */
function ippaiWarm(urls, flag, width, report, busyWidth = 0, opt = {}) {
  const port = ippaiFetchConnect();
  port.onmessage = (e) => report(e.data);
  port.postMessage({ warm: { urls, flag, width, busyWidth, cls: opt.cls, sizes: opt.sizes } });
  return port;
}
if (typeof Module !== 'undefined') Module.ippaiWarm = ippaiWarm;

var ippaiFetchWorkerInstance;
var ippaiFetchPort;
function ippaiFetchConnect() {
  if (!ippaiFetchWorkerInstance) {
    const blob = new Blob([ippaiFetchWorkerSource('self.onmessage = e => { if (e.data.port) e.data.port.onmessage = ippaiFetchWorker; else ippaiFetchWorker(e); };')], {type:'text/javascript'});
    const url = URL.createObjectURL(blob);
    ippaiFetchWorkerInstance = new Worker(url);
    URL.revokeObjectURL(url);
    // (the page's budget of bucket requests: its own save takes from it too, web/src/net.ts)
    const budget = typeof Module !== 'undefined' ? Module.ippaiNetBudget : undefined;
    if (budget) ippaiFetchWorkerInstance.postMessage({ budget });
  }
  const channel = new MessageChannel();
  ippaiFetchWorkerInstance.postMessage({port:channel.port1}, [channel.port1]);
  return channel.port2;
}
function ippaiFetchDispose() {
  ippaiFetchWorkerInstance?.terminate();
  ippaiFetchWorkerInstance = undefined;
}

var ippaiBridgeBuffer;
/** `url`'s bytes (`from`, `len`: only those; ippaiFetchPart), on a thread that may block. */
function ippaiFetchSync(url, from, len, block, cls) {
  if (typeof document !== 'undefined') throw new Error('synchronous reads on the page must be preloaded');
  if (!ippaiFetchPort && !ippaiFetchWorkerInstance) {
    const blob = new Blob([ippaiFetchWorkerSource('self.onmessage = ippaiFetchWorker;')], {type: 'text/javascript'});
    const location = URL.createObjectURL(blob);
    ippaiFetchWorkerInstance = new Worker(location);
    URL.revokeObjectURL(location);
  }
  // (this thread's buffer, kept: one read at a time per thread; a new one after a worker
  // that timed out, which may still write into the old)
  const shared = (ippaiBridgeBuffer ??= new SharedArrayBuffer(IPPAI_BRIDGE_HEADER + IPPAI_BRIDGE_BYTES));
  const state = new Int32Array(shared, 0, 8);
  const bytes = new Uint8Array(shared, IPPAI_BRIDGE_HEADER);
  Atomics.store(state, 0, 0);
  let result;
  let length = 0;
  (ippaiFetchPort || ippaiFetchWorkerInstance).postMessage({url, shared, from, len, block, cls});
  for (;;) {
    while (Atomics.load(state, 0) === 0) {
      if (Atomics.wait(state, 0, 0, 65000) === 'timed-out') {
        Atomics.store(state, 0, -1);
        Atomics.notify(state, 0);
        ippaiBridgeBuffer = undefined;
        ippaiFetchDispose();
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

/** Chunks (`urls`) a read says the game reads soon: to the network worker (ippaiHint), not waited for. */
function ippaiFetchHint(urls) {
  if (!urls.length) return;
  const to = ippaiFetchPort || ippaiFetchWorkerInstance;
  to?.postMessage({ hint: urls });
}
