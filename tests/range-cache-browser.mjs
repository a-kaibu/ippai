// Real browser regression: masu serve answers a chunk's URL whole (200, Content-Encoding:
// zstd, kept for good) and in ranges of its decoded bytes (206). After the page's read-ahead
// fetched a chunk whole, Chromium answered a game read's range from that cache entry with the
// coded bytes (a wrong Content-Range, then "Failed to fetch"): the read was retried for good
// and the game stopped at its start (mirror trials, part 3). host/fetch-bridge.js keeps ranges
// out of the HTTP cache. Checks it through the real worker protocol (masuFetchSync).
// node tests/range-cache-browser.mjs (PLAYWRIGHT, CHROME as in fetch-browser.mjs)
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import zlib from 'node:zlib';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const source = await readFile(new URL('../host/fetch-bridge.js', import.meta.url), 'utf8');
// (a chunk of 3 MB of bytes that do not compress, as media: the coded entry as long as the
// decoded chunk; a read past its first MiB, where ranges are asked for one after another. A
// range is of a block, which has no name of its own to check: the name is any)
const N = 3_000_000;
const name = '5a'.repeat(16);
let x = N;
const data = Buffer.from(Uint8Array.from({length: N}, () => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 16) & 255));
// (as the store keeps it; gzip where this node has no zstd: the cache behaves the same)
const [coding, coded] = zlib.zstdCompressSync ? ['zstd', zlib.zstdCompressSync(data)] : ['gzip', zlib.gzipSync(data)];
const path = `/store/chunks/${name.slice(0, 2)}/${name}`;
let ranges = 0;
const server = createServer((req, res) => {
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  if (req.url === '/') { res.setHeader('Content-Type', 'text/html'); res.end('<!doctype html><title>range cache test</title>'); return; }
  if (req.url !== path) { res.writeHead(404); res.end(); return; }
  // (crates/masu/src/serve.rs and store_api.rs)
  res.setHeader('Cache-Control', 'public, max-age=31536000, immutable');
  res.setHeader('Content-Type', 'application/octet-stream');
  const m = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range ?? '');
  if (!m) { res.setHeader('Content-Encoding', coding); res.end(coded); return; }
  ranges++;
  const a = +m[1], b = Math.min(m[2] ? +m[2] : N - 1, N - 1);
  if (a >= N) { res.writeHead(416, { 'Content-Range': `bytes */${N}` }); res.end(); return; }
  res.writeHead(206, { 'Content-Range': `bytes ${a}-${b}/${N}` });
  res.end(data.subarray(a, b + 1));
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROME, chromiumSandbox: false });
try {
  const page = await browser.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  const B = 262144;
  const asks = [[1_500_000, 100], [2_100_000, 300_000], [N - 5, 5], [65536, B], [N + 10, 5], [1_234_567, 1]];
  const results = await page.evaluate(async ([source, url, asks, B]) => {
    // the page's read-ahead: the whole chunk into the HTTP cache
    const whole = new Uint8Array(await (await fetch(url)).arrayBuffer());
    const connect = new Function(source + '; return masuFetchConnect;')();
    const port = connect();
    const script = source + `\nMASU_GIVE_UP_MS = 3000;\nself.onmessage=e=>{masuFetchPort=e.data.port;const out=[];for(const n of e.data.names){try{out.push(Array.from(masuFetchSync(...masuRange(n))));}catch(err){out.push(err.message);}}self.postMessage(out);};`;
    const blob = URL.createObjectURL(new Blob([script], { type: 'text/javascript' }));
    const worker = new Worker(blob);
    const out = await new Promise((resolve, reject) => {
      const timer = setTimeout(() => { worker.terminate(); reject(new Error('worker stalled')); }, 30000);
      worker.onerror = (e) => reject(new Error(e.message));
      worker.onmessage = (e) => { clearTimeout(timer); resolve(e.data); };
      worker.postMessage({ port, names: asks.map(([from, len]) => `${url}#${from}+${len}/${B}`) }, [port]);
    });
    worker.terminate();
    return { whole: whole.length, out };
  }, [source, `http://127.0.0.1:${server.address().port}${path}`, asks, B]);
  assert.equal(results.whole, N, 'the whole chunk, decoded');
  asks.forEach(([from, len], i) => {
    const got = results.out[i];
    assert.ok(Array.isArray(got), `${from}+${len}: ${got}`);
    const end = Math.min(from + len, N);
    assert.equal(got.length, Math.max(0, end - from), `${from}+${len}`);
    assert.ok(got.every((v, j) => v === data[from + j]), `${from}+${len}: bytes`);
  });
  assert.ok(ranges > 0, 'ranges went to the server');
  console.log(`range after a whole chunk in the HTTP cache (${coding}): passed`);
} finally {
  await browser.close();
  await new Promise((resolve) => server.close(resolve));
}
