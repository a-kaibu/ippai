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
  for (const [h, real] of CHUNKS) {
    // (opt.wrong: sealed as the bucket has it, but not the chunk its name says)
    const data = h === opt.wrong ? real.map((b) => b ^ 1) : real;
    const object = crypto.randomUUID();
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const ad = Uint8Array.from(object.replace(/-/g, '').match(/../g), (x) => parseInt(x, 16));
    const c = new Uint8Array(await crypto.subtle.encrypt({ name: 'AES-GCM', iv, additionalData: ad }, k, data));
    const all = new Uint8Array(12 + c.length);
    all.set(iv);
    all.set(c, 12);
    sealed.set(h, { object, all });
  }
  // (a delivery pack of the chunks `opt.packs` names, in that order: their objects side by side)
  const pack = { at: new Map(), body: new Uint8Array(0), ranges: [] };
  if (opt.packs) {
    const parts = opt.packs.map((h) => sealed.get(h).all);
    pack.body = new Uint8Array(parts.reduce((n, p) => n + p.length, 0));
    let off = 0;
    opt.packs.forEach((h, i) => {
      pack.at.set(h, { offset: off, len: parts[i].length });
      pack.body.set(parts[i], off);
      off += parts[i].length;
    });
    if (opt.damage) pack.body[pack.at.get(opt.damage).offset + 20] ^= 1;
  }
  const PACK = '0f0e0d0c-0b0a-4908-8706-050403020100';
  const log = { bucket: [], one: 0, many: 0, store: 0 };
  let fail429 = opt.fail429 ?? 0;
  const delay = opt.delay ?? 5;
  const info = (h) => ({
    url: `http://bucket/${sealed.get(h).object}`,
    key: hex(key),
    object: sealed.get(h).object,
    ...(pack.at.has(h) ? { pack: { id: PACK, url: `http://bucket/pack/${PACK}`, ...pack.at.get(h) } } : {}),
  });
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
    if (u === `http://bucket/pack/${PACK}`) {
      log.bucket.push(u);
      const range = init.headers?.range;
      pack.ranges.push(range);
      // (a range kept out of the HTTP cache: #213, a cached whole object answered ranges wrongly)
      pack.caches = [...(pack.caches ?? []), init.cache];
      if (opt.packGone) return { ok: false, status: 404, headers: { get: () => null } };
      const m = /^bytes=(\d+)-(\d+)$/.exec(range ?? '');
      // (a bucket that sends the whole object for a range)
      if (!m || opt.noRange) return { ok: true, status: 200, headers: { get: () => null }, arrayBuffer: async () => pack.body.slice().buffer };
      const body = pack.body.slice(+m[1], +m[2] + 1);
      const cr = `bytes ${m[1]}-${+m[1] + body.length - 1}/${pack.body.length}`;
      return { ok: true, status: 206, headers: { get: (n) => (n === 'content-range' ? cr : null) }, arrayBuffer: async () => body.slice().buffer };
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
      put: async (u, r) => {
        const b = new Uint8Array(await r.arrayBuffer());
        // (opt.putGate: the device's write held until it resolves)
        if (opt.putGate) await opt.putGate;
        kept.set(u, { b, h: Object.fromEntries(r.headers) });
      },
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
  return { ctx, log, kept, pack };
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

// ---- delivery packs (crates/ippai/src/storage.rs pack_objects)
const settle = async (ctx) => {
  for (let i = 0; i < 100 && ctx.ippaiInflight.size; i++) await new Promise((r) => setTimeout(r, 10));
};

test('a read takes the chunks after it in its pack with one ranged GET; reads of them then fetch nothing', async () => {
  const order = HASHES.slice(0, 10);
  const { ctx, log, pack } = await bridge({ packs: order });
  await ctx.ippaiDirectMany(order.map(url));
  const r = await ctx.ippaiWhole(url(order[0]), { cls: 2 });
  assert.deepEqual(r.data, CHUNKS.get(order[0]));
  await settle(ctx);
  assert.equal(log.bucket.length, 1);
  const end = pack.at.get(order[9]).offset + pack.at.get(order[9]).len;
  assert.deepEqual(pack.ranges, [`bytes=0-${end - 1}`]);
  assert.deepEqual(pack.caches, ['no-store'], 'a pack range past the HTTP cache (#213)');
  ctx.ippaiRecent.clear();
  for (const h of order) assert.deepEqual((await ctx.ippaiWhole(url(h), { cls: 0 })).data, CHUNKS.get(h));
  assert.equal(log.bucket.length, 1, 'all from the one GET');
});

test('a read in the middle of a pack: from it on; a game read takes fewer bytes than a save', async () => {
  const order = HASHES.slice(0, 12);
  const { ctx, pack } = await bridge({ packs: order });
  await ctx.ippaiDirectMany(order.map(url));
  const at = (h) => pack.at.get(h);
  // (the span: 3 chunks for the game's class, all for a save's)
  ctx.IPPAI_PACK_SPAN[0] = at(order[6]).offset + at(order[6]).len - at(order[4]).offset;
  await ctx.ippaiWhole(url(order[4]), { cls: 0 });
  await settle(ctx);
  assert.deepEqual(pack.ranges, [`bytes=${at(order[4]).offset}-${at(order[6]).offset + at(order[6]).len - 1}`]);
  await ctx.ippaiWhole(url(order[7]), { cls: 4 });
  await settle(ctx);
  assert.equal(pack.ranges.length, 2);
  assert.equal(pack.ranges[1], `bytes=${at(order[7]).offset}-${at(order[11]).offset + at(order[11]).len - 1}`);
});

test('neighbours on the device or coming already: left out, the range ends at the last one taken', async () => {
  const order = HASHES.slice(0, 8);
  const { ctx, log, pack, kept } = await bridge({ packs: order });
  await ctx.ippaiDirectMany(order.map(url));
  // (2 on the device, 7 on the device: the range ends with 6)
  for (const i of [2, 7]) {
    // (alone: not with its own neighbours)
    await ctx.ippaiDirect(url(order[i]), { cls: 0 }, undefined, true);
    await new Promise((r) => setTimeout(r, 5));
    assert.ok(kept.has(url(order[i])));
  }
  const before = log.bucket.length;
  pack.ranges.length = 0;
  ctx.ippaiRecent.clear();
  await ctx.ippaiWhole(url(order[0]), { cls: 3 });
  await settle(ctx);
  const at = (h) => pack.at.get(h);
  assert.deepEqual(pack.ranges, [`bytes=0-${at(order[6]).offset + at(order[6]).len - 1}`]);
  assert.equal(log.bucket.length, before + 1);
  for (const h of order) assert.ok(kept.has(url(h)), 'every one kept');
});

test('a neighbour in this worker\'s memory is left out too', async () => {
  const order = HASHES.slice(0, 6);
  const { ctx, pack } = await bridge({ packs: order });
  await ctx.ippaiDirectMany(order.map(url));
  ctx.ippaiRecent.set(url(order[3]), CHUNKS.get(order[3]));
  await ctx.ippaiWhole(url(order[0]), { cls: 3 });
  await settle(ctx);
  // (3's bytes only because 4 and 5 come after it)
  const at = (h) => pack.at.get(h);
  assert.deepEqual(pack.ranges, [`bytes=0-${at(order[5]).offset + at(order[5]).len - 1}`]);
  assert.equal(ctx.ippaiInflight.size, 0);
});

test('a span that opens but is not the chunk its name says: not kept, the chunk from the server', async () => {
  const order = HASHES.slice(0, 5);
  const { ctx, log, kept } = await bridge({ packs: order, wrong: order[2] });
  await ctx.ippaiDirectMany(order.map(url));
  await ctx.ippaiWhole(url(order[0]), { cls: 3 });
  await settle(ctx);
  assert.ok(!kept.has(url(order[2])), 'not kept');
  for (const i of [1, 3, 4]) assert.ok(kept.has(url(order[i])));
  // (read: from the server, once)
  assert.deepEqual((await ctx.ippaiWhole(url(order[2]), { cls: 0 })).data, CHUNKS.get(order[2]));
  assert.equal(log.store, 1);
});

test('a game read of a neighbour while the GET is going joins it', async () => {
  const order = HASHES.slice(0, 6);
  const { ctx, log } = await bridge({ packs: order, delay: 30 });
  await ctx.ippaiDirectMany(order.map(url));
  const a = ctx.ippaiWhole(url(order[0]), { cls: 3 });
  await new Promise((r) => setTimeout(r, 5));
  const b = ctx.ippaiWhole(url(order[3]), { cls: 0 });
  assert.deepEqual((await b).data, CHUNKS.get(order[3]));
  assert.deepEqual((await a).data, CHUNKS.get(order[0]));
  assert.equal(log.bucket.length, 1);
});

test('packs gone wrong: a damaged span alone again, a bucket without ranges sliced, a pack gone alone', async () => {
  const order = HASHES.slice(0, 6);
  {
    const { ctx, log } = await bridge({ packs: order, damage: order[2] });
    await ctx.ippaiDirectMany(order.map(url));
    await ctx.ippaiWhole(url(order[0]), { cls: 3 });
    await settle(ctx);
    ctx.ippaiRecent.clear();
    for (const h of order) assert.deepEqual((await ctx.ippaiWhole(url(h), { cls: 0 })).data, CHUNKS.get(h));
    // (the pack, then the damaged one's own object)
    assert.deepEqual(log.bucket.map((u) => u.includes('/pack/')), [true, false]);
  }
  {
    const { ctx, log } = await bridge({ packs: order, noRange: true });
    await ctx.ippaiDirectMany(order.map(url));
    await ctx.ippaiWhole(url(order[1]), { cls: 3 });
    await settle(ctx);
    ctx.ippaiRecent.clear();
    for (const h of order.slice(1)) assert.deepEqual((await ctx.ippaiWhole(url(h), { cls: 0 })).data, CHUNKS.get(h));
    assert.equal(log.bucket.length, 1);
  }
  {
    const { ctx, log } = await bridge({ packs: order, packGone: true });
    await ctx.ippaiDirectMany(order.map(url));
    const r = await ctx.ippaiWhole(url(order[0]), { cls: 3 });
    assert.deepEqual(r.data, CHUNKS.get(order[0]));
    await settle(ctx);
    ctx.ippaiRecent.clear();
    for (const h of order) assert.deepEqual((await ctx.ippaiWhole(url(h), { cls: 0 })).data, CHUNKS.get(h));
    // (the pack once, then each alone)
    assert.equal(log.bucket.filter((u) => u.includes('/pack/')).length, 1);
    assert.equal(log.bucket.length, 1 + order.length);
  }
});

test('a malformed pack in an answer is no pack', async () => {
  const { ctx } = await bridge();
  for (const bad of [{}, { id: 'x', url: 'u', offset: 0, len: 100 }, { id: PACK_ID, url: 1, offset: 0, len: 100 }, { id: PACK_ID, url: 'u', offset: -1, len: 100 }, { id: PACK_ID, url: 'u', offset: 0, len: 27 }, { id: PACK_ID, url: 'u', offset: 0.5, len: 100 }])
    assert.equal(ctx.ippaiPackValid(bad), false, JSON.stringify(bad));
  assert.equal(ctx.ippaiPackValid({ id: PACK_ID, url: 'u', offset: 0, len: 28 }), true);
});
const PACK_ID = '0f0e0d0c-0b0a-4908-8706-050403020100';

test('ranges name the fetcher threads\' reads', () => {
  const ctx = vm.createContext({ SharedArrayBuffer, Atomics, Int32Array, Float64Array, Math, Date });
  vm.runInContext(source, ctx);
  assert.deepEqual([...ctx.ippaiRange('chunks/ab/x#0+10/256')], ['chunks/ab/x', 0, 10, 256, 0]);
  assert.deepEqual([...ctx.ippaiRange('chunks/ab/x#262144+4/262144!a')], ['chunks/ab/x', 262144, 4, 262144, 1]);
});

// ---------------------------------------------------------------------------------------------
// Cache Storage that does not answer (Chromium left a cached chunk's body read pending for good
// under load: a read ahead waited on it, the game's read behind it, no window ever): each step of
// a read of it given IPPAI_CACHE_MS, then the network.

/** Cache Storage over `kept` (url -> { b, h }) whose steps (open, match, arrayBuffer, blob,
 * slice) never settle when `hang(step)` says so, and reject when `fail(step)` does. */
function stalling(kept, hang, fail = () => false) {
  const step = (name, f) => (hang(name) ? new Promise(() => {}) : fail(name) ? Promise.reject(new Error(name)) : f());
  const body = (b) => ({
    arrayBuffer: () => step('arrayBuffer', async () => b.slice().buffer),
    blob: () => step('blob', async () => ({ slice: (x, y) => ({ arrayBuffer: () => step('slice', async () => b.slice(x, y).buffer) }) })),
  });
  return {
    open: () =>
      step('open', async () => ({
        match: (u) =>
          step('match', async () => {
            const d = kept.get(u);
            return d && { headers: { get: (n) => d.h[n] ?? null }, ...body(d.b) };
          }),
        put: async (u, r) => void kept.set(u, { b: new Uint8Array(await r.arrayBuffer()), h: Object.fromEntries(r.headers) }),
      })),
  };
}

/** A seeded generator (xorshift32). */
const rng = (seed) => () => {
  seed ^= seed << 13;
  seed ^= seed >>> 17;
  seed ^= seed << 5;
  return (seed >>> 0) / 2 ** 32;
};

test('Cache Storage that never answers, at any step of any read: every read still has the right bytes, from the network, in time', async () => {
  const big = HASHES.filter((h) => CHUNKS.get(h).length > 3000);
  for (let seed = 1; seed <= 150; seed++) {
    const r = rng(seed * 2654435761);
    const { ctx, kept, log } = await bridge({ delay: 1 });
    const h = big[Math.floor(r() * big.length)];
    const data = CHUNKS.get(h);
    const block = 1024;
    // (on the device as a whole chunk, as blocks kept alone, or not at all)
    const where = Math.floor(r() * 3);
    if (where === 0) kept.set(url(h), { b: data, h: { 'x-ippai-checked': '1' } });
    if (where === 1) for (let k = 0; k * block < data.length; k++) kept.set(ctx.ippaiBlockKey(url(h), k, block), { b: data.slice(k * block, (k + 1) * block), h: {} });
    const p = r();
    const hung = new Set();
    ctx.caches = stalling(kept, (s) => r() < p && (hung.add(s), true));
    ctx.ippaiCacheOpen = undefined;
    ctx.IPPAI_CACHE_MS = 15;
    const whole = r() < 0.3;
    const from = Math.floor((r() * data.length) / block) * block;
    const len = 1 + Math.floor(r() * Math.min(3 * block, data.length - from));
    const cls = Math.floor(r() * 2);
    const t = Date.now();
    const got = whole ? await ctx.ippaiWhole(url(h), { cls }) : await ctx.ippaiFetchPart(url(h), from, len, block, cls);
    const want = whole ? data : data.subarray(from, from + len);
    assert.deepEqual(got.data, want, `seed ${seed}: where ${where}, hung ${[...hung]}, ${whole ? 'whole' : `${from}+${len}`}`);
    assert.ok(Date.now() - t < 2000, `seed ${seed}: ${Date.now() - t} ms`);
    // (an answering cache: from it, nothing over the network)
    if (!hung.size && where === 0) assert.equal(log.bucket.length + log.store, 0, `seed ${seed}`);
  }
});

test("a block kept alone that does not read is not there: not taken for the chunk's short last block", async () => {
  const h = HASHES.find((x) => CHUNKS.get(x).length > 4096);
  const data = CHUNKS.get(h);
  const block = 1024;
  for (const mode of ['reject', 'hang']) {
    const { ctx, kept, log } = await bridge({ delay: 1 });
    for (let k = 0; k * block < data.length; k++) kept.set(ctx.ippaiBlockKey(url(h), k, block), { b: data.slice(k * block, (k + 1) * block), h: {} });
    // (the second block's body does not read)
    let n = 0;
    const second = (s) => s === 'arrayBuffer' && ++n === 2;
    ctx.caches = mode === 'hang' ? stalling(kept, second) : stalling(kept, () => false, second);
    ctx.ippaiCacheOpen = undefined;
    ctx.IPPAI_CACHE_MS = 15;
    const got = await ctx.ippaiFetchPart(url(h), 0, 3 * block, block, 0);
    assert.deepEqual(got.data, data.subarray(0, 3 * block), mode);
    assert.ok(log.bucket.length + log.store > 0, `${mode}: from the network`);
  }
});

test("every range goes past the browser's HTTP cache (its bodies of one chunk waited on each other there), whole chunks not", async () => {
  const big = HASHES.filter((h) => CHUNKS.get(h).length > 3000);
  for (let seed = 1; seed <= 60; seed++) {
    const r = rng(seed * 40503);
    const { ctx } = await bridge({ delay: 1 });
    const seen = [];
    const inner = ctx.fetch;
    ctx.fetch = (u, init = {}) => {
      seen.push({ u, range: init.headers?.range, cache: init.cache });
      return inner(u, init);
    };
    // (a server without the bucket: reads of blocks are ranges of /store)
    ctx.ippaiDirectOff = r() < 0.5;
    const h = big[Math.floor(r() * big.length)];
    const data = CHUNKS.get(h);
    const block = 1024;
    const reads = Array.from({ length: 1 + Math.floor(r() * 4) }, () => {
      const from = Math.floor((r() * data.length) / block) * block;
      return [from, 1 + Math.floor(r() * Math.min(2 * block, data.length - from))];
    });
    const got = await Promise.all(reads.map(([from, len]) => ctx.ippaiFetchPart(url(h), from, len, block, Math.floor(r() * 2))));
    got.forEach((g, i) => assert.deepEqual(g.data, data.subarray(reads[i][0], reads[i][0] + reads[i][1]), `seed ${seed}`));
    const store = seen.filter((s) => s.u.includes('/store/chunks/'));
    for (const s of store) assert.equal(s.cache === 'no-store', !!s.range, `seed ${seed}: ${JSON.stringify(s)}`);
    if (ctx.ippaiDirectOff) assert.ok(store.some((s) => s.range), `seed ${seed}: ranges asked for`);
  }
});

// ---- the 2026-11-14 review of fe191e3c
const until = async (f, ms = 3000) => {
  for (const t0 = Date.now(); !f() && Date.now() - t0 < ms; ) await new Promise((r) => setTimeout(r, 5));
  return f();
};

test("a game read of a pack neighbour makes the pack's GET the game's (one job, not a copy)", async () => {
  const order = HASHES.slice(0, 6);
  const { ctx, log } = await bridge({ packs: order });
  await ctx.ippaiDirectMany(order.map(url));
  // (the budget spent: the warm read of the first waits at its reserve, its neighbours with it)
  ctx.ippaiBudgetWith((v) => void ((v[0] = 0), (v[1] = Date.now() + 60000)));
  const warm = ctx.ippaiWhole(url(order[0]), { cls: ctx.IPPAI_CLASS.warm });
  await new Promise((r) => setTimeout(r, 30));
  assert.equal(log.bucket.length, 0, 'warm waits for the budget');
  assert.ok(ctx.ippaiInflight.has(url(order[3])), 'the neighbour is coming with it');
  const t0 = Date.now();
  const game = await Promise.race([ctx.ippaiWhole(url(order[3]), { cls: 0 }), new Promise((r) => setTimeout(() => r({ late: true }), 2000))]);
  assert.ok(!game.late, `the game waited ${Date.now() - t0} ms behind the warm class's budget`);
  assert.deepEqual(game.data, CHUNKS.get(order[3]));
  assert.equal(log.bucket.length, 1, 'the one GET, gone at the game\'s class');
  assert.deepEqual((await warm).data, CHUNKS.get(order[0]));
});

test("a game read joining a fetch ahead that waits for the chunk's lock fetches it itself after a while", async () => {
  // (another fetcher holds the chunk's lock: the page's save, slow)
  let release;
  const held = new Promise((r) => (release = r));
  const locks = { request: async (name, f) => (await held, f()) };
  const { ctx, log } = await bridge({ locks });
  ctx.IPPAI_LOCK_WAIT_MS = 50;
  const h = HASHES[4];
  const ahead = ctx.ippaiWhole(url(h), { cls: ctx.IPPAI_CLASS.hint });
  const t0 = Date.now();
  const game = await Promise.race([ctx.ippaiWhole(url(h), { cls: 0 }), new Promise((r) => setTimeout(() => r({ late: true }), 2000))]);
  assert.ok(!game.late, 'the game waited for the lock');
  assert.deepEqual(game.data, CHUNKS.get(h));
  assert.ok(Date.now() - t0 < 1000, `the game waited ${Date.now() - t0} ms for a lock`);
  assert.equal(log.bucket.length, 1);
  // (the lock let go: the fetch ahead finds the game's, nothing fetched again)
  release();
  assert.deepEqual((await ahead).data, CHUNKS.get(h));
  assert.equal(log.bucket.length, 1);
});

test('a pack neighbour checked but not yet on the device is read from memory, not fetched again', async () => {
  const order = HASHES.slice(0, 6);
  let write;
  const putGate = new Promise((r) => (write = r));
  const { ctx, log, kept } = await bridge({ packs: order, putGate });
  await ctx.ippaiDirectMany(order.map(url));
  await ctx.ippaiWhole(url(order[0]), { cls: 0 });
  await settle(ctx);
  assert.equal(log.bucket.length, 1);
  ctx.ippaiRecent.clear();
  assert.equal(kept.size, 0, 'the device has none of them yet');
  for (const h of order.slice(1)) assert.deepEqual((await ctx.ippaiWhole(url(h), { cls: 0 })).data, CHUNKS.get(h));
  assert.equal(log.bucket.length, 1, 'none fetched again while being written');
  assert.ok(ctx.ippaiSavingBytes > 0);
  write();
  assert.ok(await until(() => ctx.ippaiSaving.size === 0 && kept.size >= order.length), 'written, then let go');
  assert.equal(ctx.ippaiSavingBytes, 0);
});

test('chunks waiting to be written past the cap hold fetches ahead, not the game', async () => {
  let write;
  const putGate = new Promise((r) => (write = r));
  const { ctx, log } = await bridge({ putGate });
  ctx.IPPAI_SAVING_BYTES = 1;
  ctx.IPPAI_RECENT_BYTES = 0;
  await ctx.ippaiWhole(url(HASHES[1]), { cls: 0 });
  assert.ok(ctx.ippaiSavingBytes > 1);
  const warm = ctx.ippaiWhole(url(HASHES[2]), { cls: ctx.IPPAI_CLASS.warm });
  await new Promise((r) => setTimeout(r, 120));
  assert.equal(log.bucket.length, 1, 'warm waits for the device to catch up');
  await ctx.ippaiWhole(url(HASHES[3]), { cls: 0 });
  assert.equal(log.bucket.length, 2, 'the game does not');
  write();
  await warm;
  assert.equal(log.bucket.length, 3);
});

test('each request to the bucket takes from the budget, retries too', async () => {
  const { ctx, log } = await bridge({ fail429: 2 });
  vm.runInContext('ippaiPause = async () => {}; ippaiDrain = () => {};', ctx);
  let takes = 0;
  const take = ctx.ippaiTake;
  ctx.ippaiTake = (cls, now) => (takes++, take(cls, now));
  await ctx.ippaiWhole(url(HASHES[6]), { cls: 0 });
  assert.equal(log.bucket.length, 3);
  assert.equal(takes, 3, 'one per request made');
});

test('a 429 stops every class for its whole wait, the game\'s too, then the budget refills from none', () => {
  const ctx = vm.createContext({ SharedArrayBuffer, Atomics, Int32Array, Float64Array, Math, Date });
  vm.runInContext(source, ctx);
  const C = ctx.IPPAI_CLASS;
  const now = 7_000_000;
  ctx.ippaiBudgetWith((v) => void ((v[0] = ctx.IPPAI_BURST), (v[1] = now), (v[2] = 0)));
  ctx.ippaiDrain(5000, now);
  for (const dt of [0, 112, 1000, 4999]) for (const cls of Object.values(C)) assert.equal(ctx.ippaiTake(cls, now + dt), false, `class ${cls} at +${dt} ms`);
  // (after it: the game first; the others when the budget is back over their reserve)
  assert.equal(ctx.ippaiTake(C.ahead, now + 5000), false);
  assert.equal(ctx.ippaiTake(C.game, now + 5000), true);
  assert.equal(ctx.ippaiTake(C.warm, now + 5000 + 1000), false);
  assert.equal(ctx.ippaiTake(C.warm, now + 5000 + 4000), true);
  // (a shorter 429 meanwhile does not shorten it)
  ctx.ippaiDrain(10000, now + 6000);
  ctx.ippaiDrain(1000, now + 6000);
  assert.equal(ctx.ippaiTake(C.game, now + 15000), false);
  assert.equal(ctx.ippaiTake(C.game, now + 16000), true);
});
