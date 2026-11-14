// Real Chromium: where the page gets sake's wasm (web/src/wasmsource.ts). The page is cross-origin
// isolated like ippai's; the bucket is another origin answering with CORS, its object sealed as
// crates/ippai/src/envelope.rs seals it. Checked, in order:
// 1. nothing kept: /sake-direct answers, the wasm comes from the bucket (not the server's line),
//    compiles, and instantiateWasm hands emscripten an instance; it is kept in OPFS;
// 2. a new page: the wasm read from OPFS, the bucket not asked again;
// 3. the server's file fetched once (immutable, as ippai serve sends a ?v= file): the next page
//    leaves it to the browser's HTTP cache (only-if-cached), neither /sake-direct nor the bucket asked;
// 4. a damaged bucket object or a server without S3: null (emscripten loads the server's file).
//   node tests/wasmsource-browser.mjs        (web's pnpm install done; PLAYWRIGHT as elsewhere)
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { createCipheriv, randomBytes, randomUUID } from 'node:crypto';
import { createRequire } from 'node:module';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { pathToFileURL } from 'node:url';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const root = new URL('..', import.meta.url);
const vp = await import(pathToFileURL(createRequire(new URL('web/package.json', root)).resolve('vite-plus')).href);
const build = vp.build ?? vp.default.build;

// the page's module: wasmsource.ts and what it imports, in one file
const src = (f) => new URL(`web/src/${f}`, root).pathname;
const entry = join(mkdtempSync(join(tmpdir(), 'wasmsource-')), 'entry.ts');
writeFileSync(entry, `export * from ${JSON.stringify(src('wasmsource.ts'))};
export { deviceChunks } from ${JSON.stringify(src('chunkstore.ts'))};
export { blake3Hex } from ${JSON.stringify(src('blake3.ts'))};
`);
const out = await build({
  configFile: false,
  logLevel: 'silent',
  build: { write: false, minify: false, lib: { entry, formats: ['es'], fileName: 'wasmsource' } },
});
const bundle = (Array.isArray(out) ? out[0] : out).output[0].code;

// a wasm module of ~6 MB (a custom section), named by its BLAKE3 as a chunk
const leb = (v) => {
  const o = [];
  do {
    let b = v & 0x7f;
    v >>>= 7;
    if (v) b |= 0x80;
    o.push(b);
  } while (v);
  return o;
};
const BODY = 6 << 20;
const wasm = Buffer.concat([
  Buffer.from([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0, 0, ...leb(2 + BODY), 1, 0x78]),
  Buffer.from(Uint8Array.from({ length: BODY }, (_, i) => (i * 2654435761) >>> 27)),
]);
const key = randomBytes(32);
const object = randomUUID();
const seal = (plain) => {
  const nonce = randomBytes(12);
  const c = createCipheriv('aes-256-gcm', key, nonce);
  c.setAAD(Buffer.from(object.replace(/-/g, ''), 'hex'));
  return Buffer.concat([nonce, c.update(plain), c.final(), c.getAuthTag()]);
};
// ("zstd": the test hands wasmsource an identity unzstd; snapz is direct-browser.mjs's)
let sealed = seal(wasm);

const counts = { file: 0, direct: 0, bucket: 0 };
let s3 = true;
let hex = '';
const app = createServer((req, res) => {
  const h = {
    'cross-origin-opener-policy': 'same-origin',
    'cross-origin-embedder-policy': 'require-corp',
    'cross-origin-resource-policy': 'same-origin',
  };
  const u = new URL(req.url, 'http://x');
  if (u.pathname === '/') {
    res.writeHead(200, { ...h, 'content-type': 'text/html' });
    return res.end('<!doctype html><title>wasm</title>');
  }
  if (u.pathname === '/wasmsource.js') {
    res.writeHead(200, { ...h, 'content-type': 'text/javascript', 'cache-control': 'no-cache' });
    return res.end(bundle);
  }
  if (u.pathname === '/sake/sake.wasm') {
    counts.file++;
    res.writeHead(200, { ...h, 'content-type': 'application/wasm', 'cache-control': 'public, max-age=31536000, immutable' });
    return res.end(wasm);
  }
  if (u.pathname === '/sake-direct/sake.wasm') {
    counts.direct++;
    if (!s3) {
      res.writeHead(501, h);
      return res.end();
    }
    res.writeHead(200, { ...h, 'content-type': 'application/json', 'cache-control': 'no-store' });
    return res.end(JSON.stringify({ url: `http://localhost:${bucketPort}/o?sig=1`, key: key.toString('hex'), object, chunk: hex }));
  }
  res.writeHead(404, h);
  res.end();
}).listen(0, '127.0.0.1');
const bucket = createServer((req, res) => {
  counts.bucket++;
  res.writeHead(200, { 'access-control-allow-origin': '*', 'content-type': 'application/octet-stream' });
  res.end(sealed);
}).listen(0, '127.0.0.1');
await Promise.all([app, bucket].map((s) => new Promise((r) => s.once('listening', r))));
const bucketPort = bucket.address().port;
const origin = `http://127.0.0.1:${app.address().port}`;

const browser = await chromium.launch();
try {
  const context = await browser.newContext();
  const run = async () => {
    const page = await context.newPage();
    await page.goto(`${origin}/`);
    const r = await page.evaluate(async (origin) => {
      const m = await import(`${origin}/wasmsource.js`);
      const { deviceChunks } = m;
      const url = `${origin}/sake/sake.wasm?v=1`;
      const got = await m.sakeWasmBytes('sake.wasm', url, `${origin}/store/`, {
        device: deviceChunks,
        unzstd: async (b) => b,
      });
      const lines = [];
      const arg = await m.wasmModuleArg(Promise.resolve(got), (l) => lines.push(l));
      let section = -1;
      if (arg.instantiateWasm) {
        const [inst, mod] = await new Promise((ok) => arg.instantiateWasm({}, (i, mo) => ok([i, mo])));
        section = inst instanceof WebAssembly.Instance ? WebAssembly.Module.customSections(mod, 'x')[0].byteLength : -2;
      }
      // (the device's copy once it is written)
      await new Promise((r) => setTimeout(r, 300));
      return { from: got.from, bytes: got.bytes?.length ?? 0, section, lines, isolated: crossOriginIsolated };
    }, origin);
    await page.close();
    return r;
  };
  // the chunk's name: the page's own BLAKE3 of the bytes
  {
    const page = await context.newPage();
    await page.goto(`${origin}/`);
    hex = await page.evaluate(async ({ origin, b64 }) => {
      const m = await import(`${origin}/wasmsource.js`);
      return m.blake3Hex(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)));
    }, { origin, b64: wasm.toString('base64') });
    await page.close();
  }
  assert.match(hex, /^[0-9a-f]{32}$/);

  const first = await run();
  assert.ok(first.isolated, 'the page is cross-origin isolated');
  assert.equal(first.from, 'bucket', JSON.stringify(first));
  assert.equal(first.bytes, wasm.length);
  assert.equal(first.section, BODY + 0, 'instantiated from the bytes');
  assert.deepEqual(counts, { file: 0, direct: 1, bucket: 1 });

  const second = await run();
  assert.equal(second.from, 'device', JSON.stringify(second));
  assert.equal(second.section, BODY);
  assert.equal(counts.bucket, 1, 'the bucket not asked again');

  // 3. kept by the browser itself: left to emscripten
  {
    const page = await context.newPage();
    await page.goto(`${origin}/`);
    await page.evaluate(async (u) => (await fetch(u)).arrayBuffer(), `${origin}/sake/sake.wasm?v=1`);
    await page.close();
  }
  const before = { ...counts };
  const third = await run();
  assert.equal(third.from, 'http-cache', JSON.stringify(third));
  assert.equal(third.section, -1, 'emscripten loads it itself');
  assert.deepEqual(counts, before, 'neither the server nor the bucket asked');

  // 4. a new device, a damaged object; then a server without S3
  const context2 = await browser.newContext();
  const run2 = async () => {
    const page = await context2.newPage();
    await page.goto(`${origin}/`);
    const r = await page.evaluate(async (origin) => {
      const m = await import(`${origin}/wasmsource.js`);
      const got = await m.sakeWasmBytes('sake.wasm', `${origin}/sake/sake.wasm?v=2`, `${origin}/store/`, {
        device: m.deviceChunks,
        unzstd: async (b) => b,
      });
      return { from: got.from, bytes: got.bytes };
    }, origin);
    await page.close();
    return r;
  };
  sealed = Buffer.from(sealed);
  sealed[100] ^= 1;
  assert.deepEqual(await run2(), { from: 'server', bytes: null });
  s3 = false;
  sealed = seal(wasm);
  assert.deepEqual(await run2(), { from: 'server', bytes: null });
  await context2.close();
  console.log('wasmsource-browser: ok', counts);
} finally {
  await browser.close();
  app.close();
  bucket.close();
}
