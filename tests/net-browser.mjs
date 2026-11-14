// The game's reads on an unstable link, in real Chromium: a connection that drops, a body that
// stops halfway, the browser going offline mid-game and coming back, and a store chunk read
// again while offline (from Cache Storage). host/fetch-bridge.js, as fetch-browser.mjs runs it.
// node tests/net-browser.mjs (PLAYWRIGHT, CHROME as in fetch-browser.mjs)
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const source = await readFile(new URL('../host/fetch-bridge.js', import.meta.url), 'utf8');
const body = Buffer.from(Array.from({ length: 300000 }, (_, i) => i % 251));
// (a store chunk is named by its content: see the chunk steps below)
const name = new Function(`${source}; return masuBlake3;`)()(body);
const hits = new Map();
const server = createServer((req, res) => {
  const n = (hits.get(req.url) ?? 0) + 1;
  hits.set(req.url, n);
  res.setHeader('Cross-Origin-Opener-Policy', 'same-origin');
  res.setHeader('Cross-Origin-Embedder-Policy', 'require-corp');
  if (req.url === '/') {
    res.setHeader('Content-Type', 'text/html');
    res.end('<!doctype html><title>net test</title>');
    return;
  }
  // the link drops before an answer
  if (req.url === '/drop' && n <= 2) return req.socket.destroy();
  // a server restarting behind its proxy
  if (req.url === '/restart' && n <= 2) {
    res.writeHead(502);
    res.end();
    return;
  }
  // half the body, then nothing (a phone moving out of coverage)
  if (req.url === '/stall' && n === 1) {
    res.writeHead(200, { 'Content-Length': body.length, 'Cache-Control': 'no-store' });
    res.write(body.subarray(0, 100000));
    return;
  }
  // a chunk damaged on the way the first time, kept by the browser's HTTP cache as chunks are
  if (req.url.startsWith('/store/chunks/cd/') && n === 1) {
    const bad = Buffer.from(body);
    bad[5000] ^= 1;
    res.writeHead(200, { 'Cache-Control': 'public, max-age=31536000, immutable', 'Content-Length': bad.length });
    res.end(bad);
    return;
  }
  res.writeHead(200, { 'Cache-Control': 'no-store', 'Content-Length': body.length });
  res.end(body);
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROME, chromiumSandbox: false });
const t0 = Date.now();
try {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`http://127.0.0.1:${server.address().port}`);
  await page.evaluate((source) => {
    const connect = new Function(`${source}; MASU_STALL_MS = 1000; return masuFetchConnect;`)();
    window.retries = [];
    new BroadcastChannel('masu-net').onmessage = (e) => window.retries.push(e.data.retrying);
    window.request = (path) =>
      new Promise((resolve, reject) => {
        const port = connect();
        const script = `${source}\nself.onmessage=e=>{masuFetchPort=e.data.port;try{const b=masuFetchSync(e.data.url);self.postMessage({length:b.length,sum:b.reduce((a,x)=>(a*31+x)>>>0,0),cached:b.cached});}catch(e){self.postMessage({error:e.message});}};`;
        const blob = URL.createObjectURL(new Blob([script], { type: 'text/javascript' }));
        const worker = new Worker(blob);
        const timer = setTimeout(() => (worker.terminate(), reject(new Error('worker stalled'))), 60000);
        worker.onerror = reject;
        worker.onmessage = (e) => {
          clearTimeout(timer);
          worker.terminate();
          resolve(e.data);
        };
        worker.postMessage({ port, url: location.origin + path }, [port]);
      });
  }, source);
  const sum = body.reduce((a, x) => (a * 31 + x) >>> 0, 0);
  const want = { length: body.length, sum };
  const got = (r) => ({ length: r.length, sum: r.sum });

  for (const path of ['/drop', '/restart', '/stall']) {
    const r = await page.evaluate((p) => window.request(p), path);
    assert.deepEqual(got(r), want, `${path}: ${JSON.stringify(r)}`);
  }
  assert.equal(hits.get('/drop'), 3);
  assert.equal(hits.get('/stall'), 2);
  const retries = await page.evaluate(() => window.retries);
  assert.ok(retries.includes(1) && retries.at(-1) === 0, `retries announced: ${retries}`);
  console.log('net: dropped connection, 502s and a stalled body retried');

  // offline in the middle of play: the read waits and completes when the link is back
  await context.setOffline(true);
  const later = page.evaluate(() => window.request('/later'));
  await new Promise((r) => setTimeout(r, 2500));
  assert.equal(hits.get('/later'), undefined);
  await context.setOffline(false);
  assert.deepEqual(got(await later), want);
  console.log('net: a read made offline completed once back online');

  // a store chunk: kept on the device, read again offline without the network
  const chunk = `/store/chunks/ab/${name}`;
  assert.deepEqual(got(await page.evaluate((p) => window.request(p), chunk)), want);
  await new Promise((r) => setTimeout(r, 300));
  await context.setOffline(true);
  const again = await page.evaluate((p) => window.request(p), chunk);
  assert.deepEqual(got(again), want);
  assert.equal(again.cached, 1);
  assert.equal(hits.get(chunk), 1);
  await context.setOffline(false);
  console.log(`net: a chunk read again offline from Cache Storage (${((Date.now() - t0) / 1000).toFixed(1)} s)`);

  // a chunk that does not match its name: fetched again (past the HTTP cache), the good one used
  const damaged = `/store/chunks/cd/${name}`;
  assert.deepEqual(got(await page.evaluate((p) => window.request(p), damaged)), want);
  assert.equal(hits.get(damaged), 2);
  console.log('net: a damaged chunk was fetched again past the HTTP cache');
} finally {
  await browser.close();
  await new Promise((resolve) => server.close(resolve));
}
