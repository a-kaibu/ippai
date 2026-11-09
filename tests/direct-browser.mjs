// Real browser: store chunks fetched from the bucket itself (host/fetch-bridge.js ippaiDirect,
// ippai serve's /store-direct). The page is cross-origin isolated like ippai's; the bucket is
// another origin that redirects (s3.hf.co to its CDN) and answers with CORS; objects are sealed
// as crates/ippai/src/envelope.rs seals them and zstd-coded by sake's own snapz. Every way back
// to the server is checked too: a wrong key, a chunk the bucket lacks, a damaged body, a server
// without S3.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createCipheriv, randomBytes, randomUUID } from 'node:crypto';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const root = new URL('..', import.meta.url);
const source = await readFile(new URL('host/fetch-bridge.js', root), 'utf8');
const sakeDir = new URL(process.env.SAKE_WEB ?? 'web/public/sake/', root);
const blake3 = new Function(`${source}; return ippaiBlake3;`)();
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
        const connect = new Function(source + '; return ippaiFetchConnect;')();
        const one = (path) =>
          new Promise((resolve, reject) => {
            const port = connect();
            const script = source + `\nself.onmessage=e=>{ippaiFetchPort=e.data.port;try{const d=ippaiFetchSync(...ippaiRange(e.data.url));self.postMessage({length:d.length,hash:ippaiBlake3(d)});}catch(e){self.postMessage({error:e.message});}};`;
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
  await page2.evaluate(() => caches.delete('ippai-chunks-v1'));
  [r, d] = await since(() => read(page2, fresh));
  assert.deepEqual(r, fresh.map((p) => expect(p)));
  assert.deepEqual(d, { store: 3, direct: 1, bucket: 0, redirect: 0 }, 'no S3: asked once');
  console.log('direct browser: bucket fetch, redirect, decrypt, zstd, blocks, cache and every fallback passed');
} finally {
  await browser.close();
  app.close();
  bucket.close();
}
