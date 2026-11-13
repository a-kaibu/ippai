// The network worker's scheduler (host/fetch-bridge.js, docs/s3-reads.md): one fetch of a chunk
// whoever asks for it, the game's reads first, the bucket's budget of requests shared by every
// class with each leaving a reserve to the classes above it, the bucket's addresses asked for
// many at once, hints and warm runs in the same worker. The bridge's own source runs in a vm
// context with a stand-in server and bucket (objects sealed as crates/ippai/src/envelope.rs
// seals them: AES-256-GCM, nonce first, the object's UUID as associated data).
//   node --test tests/sched.test.mjs
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import vm from 'node:vm';

const source = await readFile(new URL('../host/fetch-bridge.js', import.meta.url), 'utf8');
const vectors = (await readFile(new URL('../testdata/blake3.tsv', import.meta.url), 'utf8')).trim().split('\n').slice(1).map((l) => l.split('\t'));
const input = (kind, n) => {
  let x = n >>> 0;
  return Uint8Array.from({ length: n }, (_, i) => {
    if (kind === 'p') return i % 251;
    x = (Math.imul(x, 1103515245) + 12345) >>> 0;
    return (x >>> 16) & 255;
  });
};
// chunks with real names: hash -> bytes
const CHUNKS = new Map(vectors.filter((v) => +v[1] > 0).map(([k, n, h]) => [h, input(k, +n)]));
const HASHES = [...CHUNKS.keys()];
const BASE = 'http://srv/';
const url = (h) => `${BASE}store/chunks/${h.slice(0, 2)}/${h}`;

/** A fresh bridge with a stand-in server (`/store-direct`, `/store`) and bucket. */
async function bridge(opt = {}) {
  const key = crypto.getRandomValues(new Uint8Array(32));
  const hex = (b) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');
  const k = await crypto.subtle.importKey('raw', key, 'AES-GCM', false, ['encrypt']);
  const sealed = new Map();
  for (const [h, data] of CHUNKS) {
    const object = crypto.randomUUID();
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ad = Uint8Array.from(object.replace(/-/g, '').match(/../g), (x) => parseInt(x, 16));
    const c = new Uint8Array(await crypto.subtle.encrypt({ name: 'AES-GCM', iv, additionalData: ad }, k, data));
    const all = new Uint8Array(12 + c.length);
    all.set(iv);
    all.set(c, 12);
    sealed.set(h, { object, all });
  }
  const log = { bucket: [], one: 0, many: 0, store: 0 };
  let fail429 = opt.fail429 ?? 0;
  const delay = opt.delay ?? 5;
  const info = (h) => ({ url: `http://bucket/${sealed.get(h).object}`, key: hex(key), object: sealed.get(h).object });
  const fetch = async (u, init = {}) => {
    await new Promise((r) => setTimeout(r, delay));
    if (u === `${BASE}store-direct` && init.method === 'POST') {
      log.many++;
      const out = {};
      for (const p of JSON.parse(init.body)) {
        const h = p.slice(p.lastIndexOf('/') + 1);
        if (sealed.has(h) && !(opt.onlyServer ?? []).includes(h)) out[p] = info(h);
      }
      return { ok: true, status: 200, json: async () => out };
    }
    if (u.startsWith(`${BASE}store-direct/`)) {
      log.one++;
      const h = u.slice(u.lastIndexOf('/') + 1);
      if (!sealed.has(h) || (opt.onlyServer ?? []).includes(h)) return { ok: false, status: 404 };
      return { ok: true, status: 200, json: async () => info(h) };
    }
    if (u.startsWith('http://bucket/')) {
      log.bucket.push(u);
      if (fail429 > 0) {
        fail429--;
        return { ok: false, status: 429, headers: { get: (n) => (n === 'retry-after' ? '1' : null) }, body: null };
      }
      const s = [...sealed.values()].find((x) => u.endsWith(x.object));
      const body = s.all;
      return { ok: true, status: 200, headers: { get: () => null }, arrayBuffer: async () => body.slice().buffer };
    }
    if (u.startsWith(`${BASE}store/chunks/`)) {
      log.store++;
      const h = u.slice(u.lastIndexOf('/') + 1);
      return { ok: true, status: 200, headers: { get: () => null }, arrayBuffer: async () => CHUNKS.get(h).slice().buffer };
    }
    return { ok: false, status: 404 };
  };
  const kept = new Map();
  const caches = {
    open: async () => ({
      match: async (u) => {
        const d = kept.get(u);
        return d && { headers: { get: (n) => d.h[n] ?? null }, arrayBuffer: async () => d.b.slice().buffer };
      },
      put: async (u, r) => void kept.set(u, { b: new Uint8Array(await r.arrayBuffer()), h: Object.fromEntries(r.headers) }),
    }),
  };
  const ctx = vm.createContext({
    fetch, caches, crypto, Response, Blob, AbortSignal, AbortController, setTimeout, clearTimeout, setInterval, clearInterval,
    SharedArrayBuffer, Atomics, Int32Array, Float64Array, Uint8Array, ArrayBuffer, Map, Set, Promise, Math, Date, JSON, Object, Array, String, Number, Error,
    navigator: { onLine: true, locks: opt.locks },
    console,
  });
  vm.runInContext(source, ctx);
  // (the stand-in objects are not zstd frames: the plain chunk)
  vm.runInContext('ippaiUnzstd = async (base, frame) => frame; IPPAI_BAD_CHUNK = 1;', ctx);
  return { ctx, log, kept };
}

test('one fetch of a chunk however many ask for it at once, of any class', async () => {
  const { ctx, log } = await bridge();
  const h = HASHES[5];
  const all = await Promise.all([0, 1, 2, 3, 3, 4, 0].map((cls) => ctx.ippaiWhole(url(h), { cls })));
  assert.equal(log.bucket.length, 1);
  for (const r of all) assert.deepEqual(r.data, CHUNKS.get(h));
  // (kept: again, nothing over the network)
  ctx.ippaiRecent.clear();
  const again = await ctx.ippaiWhole(url(h), { cls: 3 });
  assert.equal(log.bucket.length, 1);
  assert.deepEqual(again.data, CHUNKS.get(h));
});

test("a fetch ahead holds the lock the page's save holds for the chunk (web/src/offline.ts withChunkLock); the game's does not wait for one", async () => {
  const names = [];
  const locks = { request: async (name, f) => (names.push(name), f()) };
  const { ctx } = await bridge({ locks });
  const [a, b] = HASHES;
  await ctx.ippaiWhole(url(a), { cls: 4 });
  await ctx.ippaiWhole(url(b), { cls: 0 });
  assert.deepEqual(names, [`ippai-chunk:${a}`]);
});

test('a game read joining a fetch ahead that waits for the budget makes it the game\'s', async () => {
  const { ctx, log } = await bridge();
  // (the budget spent: a warm fetch waits at its reserve, the game's goes into debt)
  ctx.ippaiBudgetWith((v) => void (v[0] = 0, v[1] = Date.now()));
  const h = HASHES[7];
  const warm = ctx.ippaiWhole(url(h), { cls: ctx.IPPAI_CLASS.warm, want: false });
  await new Promise((r) => setTimeout(r, 120));
  assert.equal(log.bucket.length, 0, 'warm waits for the budget');
  const t = Date.now();
  const game = await ctx.ippaiWhole(url(h), { cls: ctx.IPPAI_CLASS.game });
  assert.ok(Date.now() - t < 1000, `the game's read waited ${Date.now() - t} ms`);
  assert.deepEqual(game.data, CHUNKS.get(h));
  await warm;
  assert.equal(log.bucket.length, 1);
});

test('budget: no class past the game takes more than the burst and the rate allow, each leaves its reserve', () => {
  // random runs of takes at random times (a seeded LCG: the same runs every time)
  let x = 12345;
  const rnd = (n) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 200; run++) {
    const ctx = vm.createContext({ SharedArrayBuffer, Atomics, Int32Array, Float64Array, Math, Date });
    vm.runInContext(source, ctx);
    const t0 = 1_000_000;
    ctx.ippaiBudgetWith((v) => void (v[0] = ctx.IPPAI_BURST, v[1] = t0));
    let now = t0;
    const took = [0, 0, 0, 0, 0];
    for (let i = 0; i < 400; i++) {
      now += rnd(200);
      const cls = rnd(5);
      if (ctx.ippaiTake(cls, now)) took[cls]++;
      const tokens = ctx.ippaiBudgetWith((v) => v[0]);
      assert.ok(tokens <= ctx.IPPAI_BURST + 1e-9);
      // (never below the game's floor)
      assert.ok(tokens >= ctx.IPPAI_RESERVE[0] - 1e-9);
    }
    const secs = (now - t0) / 1000;
    const low = took.slice(1).reduce((a, b) => a + b, 0);
    assert.ok(low <= ctx.IPPAI_BURST + ctx.IPPAI_RATE * secs + 1e-6, `run ${run}: ${low} in ${secs} s`);
    // (the game's never refused while the debt allows)
    const all = took.reduce((a, b) => a + b, 0);
    assert.ok(all <= ctx.IPPAI_BURST + ctx.IPPAI_RATE * secs - ctx.IPPAI_RESERVE[0] + 1e-6);
  }
});

test('a class leaves its reserve: warm stops where hints still go, hints where the game still goes', () => {
  const ctx = vm.createContext({ SharedArrayBuffer, Atomics, Int32Array, Float64Array, Math, Date });
  vm.runInContext(source, ctx);
  const now = 5_000_000;
  const set = (n) => ctx.ippaiBudgetWith((v) => void (v[0] = n, v[1] = now));
  const C = ctx.IPPAI_CLASS;
  set(ctx.IPPAI_RESERVE[C.warm] + 0.5);
  assert.equal(ctx.ippaiTake(C.warm, now), false);
  assert.equal(ctx.ippaiTake(C.hint, now), true);
  set(ctx.IPPAI_RESERVE[C.hint] + 0.5);
  assert.equal(ctx.ippaiTake(C.hint, now), false);
  assert.equal(ctx.ippaiTake(C.ahead, now), true);
  set(0.5);
  assert.equal(ctx.ippaiTake(C.ahead, now), false);
  assert.equal(ctx.ippaiTake(C.game, now), true);
  // (the game's debt has a floor)
  set(ctx.IPPAI_RESERVE[C.game] + 0.5);
  assert.equal(ctx.ippaiTake(C.game, now), false);
  // (429: everybody waits)
  set(ctx.IPPAI_BURST);
  ctx.ippaiDrain(2000);
  assert.equal(ctx.ippaiTake(C.ahead, now), false);
});

test('the bucket\'s addresses asked for many at once: one POST per 512, none one by one after', async () => {
  const { ctx, log } = await bridge();
  // (the same few chunks under many names would not be chunks: use the real ones, many times over)
  const urls = HASHES.map(url);
  await ctx.ippaiDirectMany(urls);
  assert.equal(log.many, 1);
  // (asked again: fresh, no POST)
  await ctx.ippaiDirectMany(urls);
  assert.equal(log.many, 1);
  for (const u of urls.slice(0, 6)) await ctx.ippaiWhole(u, { cls: 0 });
  assert.equal(log.one, 0, 'no single ask after the batch');
  assert.equal(log.bucket.length, 6);
});

test('a chunk only on the server: the batch leaves it out, the read goes to /store without a single ask', async () => {
  const h = HASHES[3];
  const { ctx, log } = await bridge({ onlyServer: [h] });
  await ctx.ippaiDirectMany([url(h), url(HASHES[4])]);
  const r = await ctx.ippaiWhole(url(h), { cls: 0 });
  assert.deepEqual(r.data, CHUNKS.get(h));
  assert.equal(log.one, 0);
  assert.equal(log.store, 1);
  assert.equal(log.bucket.length, 0);
});

test('a warm run fetches each chunk once, asks for addresses in batches, and a game read in it joins', async () => {
  const { ctx, log, kept } = await bridge();
  const urls = HASHES.map(url);
  const flag = new SharedArrayBuffer(8);
  const reports = [];
  const run = ctx.ippaiWarmRun({ urls, flag, width: 6, busyWidth: 2, cls: ctx.IPPAI_CLASS.save }, (r) => reports.push(r));
  // (the game reads one of them meanwhile)
  const g = await ctx.ippaiWhole(urls[10], { cls: 0 });
  assert.deepEqual(g.data, CHUNKS.get(HASHES[10]));
  await run;
  const end = reports.at(-1);
  assert.equal(end.end, true);
  assert.equal(end.done, urls.length);
  assert.equal(end.failed, 0);
  assert.equal(new Set(log.bucket).size, log.bucket.length, 'no chunk fetched twice');
  assert.equal(log.bucket.length, urls.length);
  assert.equal(log.one, 0);
  for (const u of urls) assert.ok(kept.has(u), `kept ${u}`);
  // (again: all on the device, nothing fetched)
  const again = [];
  await ctx.ippaiWarmRun({ urls, flag, width: 6 }, (r) => again.push(r));
  assert.equal(again.at(-1).fetched, 0);
  assert.equal(log.bucket.length, urls.length);
});

test('a warm run stops when told', async () => {
  const { ctx, log } = await bridge({ delay: 20 });
  const flag = new SharedArrayBuffer(8);
  const f = new Int32Array(flag);
  const reports = [];
  const run = ctx.ippaiWarmRun({ urls: HASHES.map(url), flag, width: 2 }, (r) => reports.push(r));
  setTimeout(() => Atomics.store(f, 1, 1), 30);
  await run;
  assert.ok(reports.at(-1).end);
  assert.ok(log.bucket.length < HASHES.length, `stopped after ${log.bucket.length}`);
});

test('hints: newest first, each once, a few at a time, kept on the device and not in memory', async () => {
  const { ctx, log, kept } = await bridge({ delay: 15 });
  const urls = HASHES.slice(0, 20).map(url);
  ctx.ippaiHint(urls.slice(0, 10));
  ctx.ippaiHint(urls.slice(5, 20));
  assert.ok(ctx.ippaiHintsGoing <= ctx.IPPAI_HINT_WIDTH);
  // (the ones asked for twice queued once: no slot spent on a chunk already coming)
  assert.equal(ctx.ippaiHintsGoing + ctx.ippaiHints.length, 20);
  for (let i = 0; i < 200 && (ctx.ippaiHintsGoing || ctx.ippaiHints.length); i++) await new Promise((r) => setTimeout(r, 10));
  assert.equal(log.bucket.length, 20, 'each once');
  for (const u of urls) assert.ok(kept.has(u));
  assert.equal(ctx.ippaiRecent.size, 0);
  // (hinted again: nothing)
  ctx.ippaiHint(urls);
  await new Promise((r) => setTimeout(r, 50));
  assert.equal(log.bucket.length, 20);
});

test('a 429 from the bucket: retried after its wait, and the budget drained for every class', async () => {
  const { ctx, log } = await bridge({ fail429: 1 });
  const r = await ctx.ippaiWhole(url(HASHES[2]), { cls: 0 });
  assert.deepEqual(r.data, CHUNKS.get(HASHES[2]));
  assert.equal(log.bucket.length, 2);
  assert.equal(ctx.ippaiTake(ctx.IPPAI_CLASS.warm), false);
});

test('ranges name the fetcher threads\' reads', () => {
  const ctx = vm.createContext({ SharedArrayBuffer, Atomics, Int32Array, Float64Array, Math, Date });
  vm.runInContext(source, ctx);
  assert.deepEqual([...ctx.ippaiRange('chunks/ab/x#0+10/256')], ['chunks/ab/x', 0, 10, 256, 0]);
  assert.deepEqual([...ctx.ippaiRange('chunks/ab/x#262144+4/262144!a')], ['chunks/ab/x', 262144, 4, 262144, 1]);
});
