// Real browser: store chunks fetched from the bucket itself (host/fetch-bridge.js masuDirect,
// masu serve's /store-direct). The page is cross-origin isolated like masu's; the bucket is
// another origin that redirects (s3.hf.co to its CDN) and answers with CORS; objects are sealed
// as crates/masu/src/envelope.rs seals them and zstd-coded by sake's own snapz. Every way back
// to the server is checked too: a wrong key, a chunk the bucket lacks, a damaged body, a server
// without S3.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createCipheriv, randomBytes, randomUUID } from 'node:crypto';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const root = new URL('..', import.meta.url);
// (the server's answer to /store-direct waited for 1 s, not 10: the test of a stuck server)
const source = (await readFile(new URL('host/fetch-bridge.js', root), 'utf8')).replace('var MASU_DIRECT_ASK_MS = 10000;', 'var MASU_DIRECT_ASK_MS = 1000;');
assert.ok(source.includes('MASU_DIRECT_ASK_MS = 1000;'));
const sakeDir = new URL(process.env.SAKE_WEB ?? 'web/public/sake/', root);
const blake3 = new Function(`${source}; return masuBlake3;`)();
const z = await (await import(new URL('snapz.mjs', sakeDir))).default();

/** `n` bytes from an LCG seeded with `seed` (with runs, so zstd has something to do). */
const bytes = (n, seed) => {
  let x = seed >>> 0;
  return Uint8Array.from({ length: n }, (_, i) => {
    if (i % 1024 < 300) return i & 7;
    x = (Math.imul(x, 1103515245) + 12345) >>> 0;
    return (x >>> 16) & 255;
  });
};
const zstd = (data) => {
  const cap = z._snapz_bound(data.length) >>> 0;
  const src = z._malloc(data.length) >>> 0;
  const dst = z._malloc(cap) >>> 0;
  z.HEAPU8.set(data, src);
  const n = z._snapz_compress(dst, cap, src, data.length, 3) >>> 0;
  const out = z.HEAPU8.slice(dst, dst + n);
  z._free(src);
  z._free(dst);
  return out;
};
/** envelope.rs: nonce (12) + AES-256-GCM ciphertext + tag, the object's UUID bytes as associated data. */
const seal = (key, object, plain) => {
  const nonce = randomBytes(12);
  const c = createCipheriv('aes-256-gcm', key, nonce);
  c.setAAD(Buffer.from(object.replace(/-/g, ''), 'hex'));
  return Buffer.concat([nonce, c.update(plain), c.final(), c.getAuthTag()]);
};

// chunks: what the server has decoded, and what the bucket has sealed
const chunks = new Map();
const make = (label, n, seed, how = {}) => {
  const data = bytes(n, seed);
  const hash = blake3(data);
  const key = randomBytes(32);
  const object = randomUUID();
  chunks.set(hash, { label, data, key, object, sealed: seal(key, object, zstd(data)), how, gets: 0 });
  return `/store/chunks/${hash.slice(0, 2)}/${hash}`;
};
const whole = make('whole', 300000, 1);
const ranged = make('ranged', 1200000, 2);
const wrongKey = make('wrong key', 70000, 3, { wrongKey: true });
const missing = make('missing', 90000, 4, { missing: true });
const damaged = make('damaged', 110000, 5, { damagedOnce: true });
const big = make('big', 6 << 20, 6);

const hits = { store: 0, direct: 0, bucket: 0, redirect: 0 };
let noS3 = false;
let bucketOrigin;
const app = createServer(async (req, res) => {
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  const url = new URL(req.url, 'http://x');
  if (url.pathname === '/') return res.setHeader('Content-Type', 'text/html'), res.end('<!doctype html><title>direct</title>');
  if (url.pathname.startsWith('/sake/snapz.')) {
    res.setHeader('Content-Type', url.pathname.endsWith('.wasm') ? 'application/wasm' : 'text/javascript');
    return res.end(await readFile(new URL(url.pathname.slice(6), sakeDir)));
  }
  const m = /^\/store(-direct)?\/chunks\/[0-9a-f]{2}\/([0-9a-f]{32})$/.exec(url.pathname);
  const c = m && chunks.get(m[2]);
  if (!c) return res.writeHead(404), res.end();
  if (m[1]) {
    hits.direct++;
    if (noS3) return res.writeHead(501), res.end();
    if (c.how.missing) return res.writeHead(404), res.end();
    // (a stuck server: never answers)
    if (c.how.stuck) return;
    const key = c.how.wrongKey ? randomBytes(32) : c.key;
    res.setHeader('Cache-Control', 'no-store');
    res.setHeader('Content-Type', 'application/json');
    return res.end(JSON.stringify({ url: `${bucketOrigin}/presigned/${m[2]}?X-Amz-Signature=x`, key: key.toString('hex'), object: c.object }));
  }
  hits.store++;
  const range = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range ?? '');
  if (range) {
    const a = +range[1];
    const b = Math.min(+range[2], c.data.length - 1);
    res.writeHead(206, { 'Content-Range': `bytes ${a}-${b}/${c.data.length}` });
    return res.end(c.data.subarray(a, b + 1));
  }
  res.end(c.data);
});
const bucket = createServer((req, res) => {
  res.setHeader('Access-Control-Allow-Origin', '*');
  res.setHeader('Access-Control-Allow-Headers', '*');
  if (req.method === 'OPTIONS') return res.writeHead(204), res.end();
  const m = /^\/(presigned|cdn)\/([0-9a-f]{32})/.exec(req.url);
  const c = m && chunks.get(m[2]);
  if (!c) return res.writeHead(404), res.end();
  // (s3.hf.co answers a GET with a redirect to its CDN)
  if (m[1] === 'presigned') return hits.redirect++, res.writeHead(307, { Location: `/cdn/${m[2]}` }), res.end();
  hits.bucket++;
  c.gets++;
  const body = Buffer.from(c.sealed);
  if (c.how.damagedOnce && c.gets === 1) body[40] ^= 1;
  res.end(body);
});
await new Promise((r) => app.listen(0, '127.0.0.1', r));
await new Promise((r) => bucket.listen(0, 'localhost', r));
bucketOrigin = `http://localhost:${bucket.address().port}`;

const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROME, chromiumSandbox: false });
try {
  const open = async () => {
    const page = await browser.newPage();
    page.on('console', (m) => process.env.VERBOSE && console.log('page:', m.text()));
    await page.goto(`http://127.0.0.1:${app.address().port}/`);
    assert.ok(await page.evaluate(() => crossOriginIsolated), 'the page is cross-origin isolated');
    return page;
  };
  /** Reads `paths` (`#from+len/block` for a range, as crates/kura/src/vfs.rs asks) one after another, each from a fresh thread. */
  const read = (page, paths) =>
    page.evaluate(
      async ([source, paths]) => {
        const connect = new Function(source + '; return masuFetchConnect;')();
        const one = (path) =>
          new Promise((resolve, reject) => {
            const port = connect();
            const script = source + `\nself.onmessage=e=>{masuFetchPort=e.data.port;try{const d=masuFetchSync(...masuRange(e.data.url));self.postMessage({length:d.length,hash:masuBlake3(d)});}catch(e){self.postMessage({error:e.message});}};`;
            const blob = URL.createObjectURL(new Blob([script], { type: 'text/javascript' }));
            const worker = new Worker(blob);
            const timer = setTimeout(() => (worker.terminate(), reject(new Error(`worker stalled: ${path}`))), 60000);
            worker.onerror = (e) => reject(new Error(e.message));
            worker.onmessage = (e) => (clearTimeout(timer), worker.terminate(), URL.revokeObjectURL(blob), resolve(e.data));
            worker.postMessage({ port, url: location.origin + path }, [port]);
          });
        const out = [];
        for (const p of paths) out.push(await one(p));
        return out;
      },
      [source, paths],
    );
  const expect = (path, from, len) => {
    const c = chunks.get(path.slice(-32));
    const d = from === undefined ? c.data : c.data.subarray(from, from + len);
    return { length: d.length, hash: blake3(d) };
  };
  const since = (f) => {
    const before = { ...hits };
    return f().then((r) => [r, Object.fromEntries(Object.keys(hits).map((k) => [k, hits[k] - before[k]]))]);
  };

  const page = await open();
  // a whole chunk: from the bucket (through its redirect), never from the server; then kept
  let [r, d] = await since(() => read(page, [whole]));
  assert.deepEqual(r, [expect(whole)]);
  assert.deepEqual(d, { store: 0, direct: 1, bucket: 1, redirect: 1 }, 'from the bucket');
  [r, d] = await since(() => read(page, [whole, whole]));
  assert.deepEqual(r, [expect(whole), expect(whole)]);
  assert.deepEqual(d, { store: 0, direct: 0, bucket: 0, redirect: 0 }, 'kept');
  // blocks of a chunk: the chunk from the bucket once, every block from it, none from the server
  const blocks = [[0, 262144], [262144, 262144], [1000000, 200000], [5, 7]];
  [r, d] = await since(() => read(page, blocks.map(([a, n]) => `${ranged}#${a}+${n}/262144`)));
  assert.deepEqual(r, blocks.map(([a, n]) => expect(ranged, a, n)));
  assert.deepEqual(d, { store: 0, direct: 1, bucket: 1, redirect: 1 }, 'blocks from one fetch');
  // 6 MiB: more than one transfer buffer, and its last bytes
  [r] = await since(() => read(page, [big, `${big}#${(6 << 20) - 10}+10/262144`]));
  assert.deepEqual(r, [expect(big), expect(big, (6 << 20) - 10, 10)]);
  // a key that does not open it, a chunk the bucket lacks: the server's, right
  for (const p of [wrongKey, missing]) {
    [r, d] = await since(() => read(page, [p]));
    assert.deepEqual(r, [expect(p)], p);
    assert.equal(d.store, 1, `${chunks.get(p.slice(-32)).label}: from the server`);
  }
  // a body damaged on the way: fetched again from the bucket
  [r, d] = await since(() => read(page, [damaged]));
  assert.deepEqual(r, [expect(damaged)]);
  assert.deepEqual([d.store, d.bucket], [0, 2], 'damaged once: the bucket again');

  // a server without S3: asked once, then never again
  noS3 = true;
  for (const c of chunks.values()) c.gets = 0;
  const fresh = [make('no s3 a', 50000, 7), make('no s3 b', 60000, 8), make('no s3 c', 70000, 9)];
  const page2 = await open();
  await page2.evaluate(async () => { await caches.delete('ippai-chunks-v1'); await (await navigator.storage.getDirectory()).removeEntry('ippai-chunks', { recursive: true }).catch(() => {}); });
  [r, d] = await since(() => read(page2, fresh));
  assert.deepEqual(r, fresh.map((p) => expect(p)));
  assert.deepEqual(d, { store: 3, direct: 1, bucket: 0, redirect: 0 }, 'no S3: asked once');

  // a server stuck on /store-direct (worker2 at 19:58 JST held a read 80 s): the server's
  // /store after the time limit, and the bucket again for the next chunk
  noS3 = false;
  const stuck = make('stuck ask', 60000, 14, { stuck: true });
  const after = make('after stuck', 50000, 15);
  const page4 = await open();
  const t4 = Date.now();
  [r, d] = await since(() => read(page4, [stuck, after]));
  assert.deepEqual(r, [expect(stuck), expect(after)]);
  assert.ok(Date.now() - t4 < 8000, `not held: ${Date.now() - t4} ms`);
  assert.deepEqual([d.store, d.bucket], [1, 1], 'the stuck one from /store, the next from the bucket');

  // the game's chunks fetched onto the device while it plays (masuWarm, web/src/warm.ts): none
  // while the game waits for a read of its own, then all of them, from the bucket (the server for
  // one it lacks), kept where the game's reads look; one already on the device not again
  noS3 = false;
  const warmed = [make('warm a', 80000, 10), make('warm b', 1500000, 11), make('warm c', 90000, 12, { missing: true }), make('warm d', 70000, 13)];
  const page3 = await open();
  await page3.evaluate(async () => { await caches.delete('ippai-chunks-v1'); await (await navigator.storage.getDirectory()).removeEntry('ippai-chunks', { recursive: true }).catch(() => {}); });
  await read(page3, [warmed[3]]);
  const warm = (paths, { busyFirst, stopFirst, busyWidth } = {}) =>
    page3.evaluate(
      async ([source, paths, busyFirst, stopFirst, busyWidth]) => {
        const masuWarm = new Function(source + '; return masuWarm;')();
        const flag = new SharedArrayBuffer(8);
        const f = new Int32Array(flag);
        if (busyFirst || busyWidth) Atomics.store(f, 0, 1);
        if (stopFirst) Atomics.store(f, 1, 1);
        const reports = [];
        const end = new Promise((resolve) => masuWarm(paths.map((p) => location.origin + p), flag, 3, (r) => (reports.push(r), r.end && resolve()), busyWidth));
        let early = 0;
        if (busyFirst) {
          await new Promise((r) => setTimeout(r, 400));
          early = reports.length;
          Atomics.store(f, 0, 0);
        }
        // (one that waits for the game for good: a failure, not a hang)
        await Promise.race([end, new Promise((_, no) => setTimeout(() => no(new Error('fetching ahead stalled')), 30000))]);
        const cache = await (async () => {
      // (the device's chunk store: OPFS, web/src/chunkstore.ts; Cache Storage before)
      const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle('ippai-chunks', { create: true });
      const old = await caches.open('ippai-chunks-v1');
      return {
        async match(u) {
          const name = u.slice(u.lastIndexOf('/') + 1);
          try {
            const f = await (await dir.getFileHandle(name)).getFile();
            if (f.size) return new Response(f, { headers: { 'x-ippai-size': String(f.size) } });
          } catch {}
          return old.match(u);
        },
      };
    })();
        // (a chunk's file is written after it is fetched: a moment later)
        let kept = [];
        for (let t = 0; t < 40; t++) {
          kept = [];
          for (const p of paths) {
            const r = await cache.match(location.origin + p);
            kept.push(r ? new Uint8Array(await r.arrayBuffer()).length : -1);
          }
          if (!kept.includes(-1) || stopFirst) break;
          await new Promise((r) => setTimeout(r, 50));
        }
        return { last: reports.at(-1), early, kept };
      },
      [source, paths, !!busyFirst, !!stopFirst, busyWidth ?? 0],
    );
  let w;
  [w, d] = await since(() => warm(warmed, { stopFirst: true }));
  assert.equal(w.last.done, 0, 'stopped before it starts: nothing');
  assert.deepEqual(d, { store: 0, direct: 0, bucket: 0, redirect: 0 });
  [w, d] = await since(() => warm(warmed, { busyFirst: true }));
  assert.equal(w.early, 0, 'nothing while the game waits');
  assert.deepEqual(w.last, { end: true, done: 4, fetched: 3, bytes: 80000 + 1500000 + 90000, failed: 0 });
  assert.deepEqual(w.kept, warmed.map((p) => chunks.get(p.slice(-32)).data.length), 'all on the device');
  assert.deepEqual([d.bucket, d.store], [2, 1], 'from the bucket, the server for the one it lacks, the kept one not again');
  // what the game reads next is on the device: no network
  [r, d] = await since(() => read(page3, [warmed[1], `${warmed[0]}#100+50/262144`]));
  assert.deepEqual(r, [expect(warmed[1]), expect(warmed[0], 100, 50)]);
  assert.deepEqual(d, { store: 0, direct: 0, bucket: 0, redirect: 0 }, 'read from the device');
  // the bytes over the network of a chunk from the bucket: another origin, which gives no
  // timings (no Timing-Allow-Origin, as s3.hf.co): counted all the same (the run logs said
  // "0.01 MB over the network" for minutes of 金恋's reads from the bucket)
  const counted = make('counted', 120000, 19);
  const got = await page3.evaluate(
    async ([source, url]) => {
      const masuDirect = new Function(source + '; return masuDirect;')();
      const d = await masuDirect(url);
      return { wire: d.wire, cached: d.cached, length: d.data.length };
    },
    [source, `http://127.0.0.1:${app.address().port}${counted}`],
  );
  assert.equal(got.length, 120000);
  assert.equal(got.cached, 0);
  assert.equal(got.wire, chunks.get(counted.slice(-32)).sealed.length, 'the sealed object crossed the network');
  // a game that waits for a read the whole time (金恋: one file after another, ~1.2 s each):
  // busyWidth of them go on all the same, all fetched; none of them twice
  const busy = [make('busy a', 60000, 14), make('busy b', 70000, 15), make('busy c', 50000, 16, { missing: true }), make('busy d', 40000, 17), make('busy e', 30000, 18)];
  [w, d] = await since(() => warm(busy, { busyWidth: 1 }));
  assert.deepEqual(w.last, { end: true, done: 5, fetched: 5, bytes: 60000 + 70000 + 50000 + 40000 + 30000, failed: 0 }, 'all while the game waits');
  assert.deepEqual(w.kept, busy.map((p) => chunks.get(p.slice(-32)).data.length));
  assert.deepEqual([d.bucket, d.store], [4, 1], 'each once');
  console.log('direct browser: bucket fetch, redirect, decrypt, zstd, blocks, cache and every fallback passed; fetched ahead');
} finally {
  await browser.close();
  app.close();
  bucket.close();
}
