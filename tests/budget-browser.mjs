// Real Chromium: the bucket's request budget shared by the page and network workers
// (web/src/budget.ts, host/fetch-bridge.js masuBudgetWith), and split between tabs.
//  1. A worker holding the lock word and not letting go (as one the scheduler paused inside):
//     the page's takes give up, leave the budget as it was and never free the word (the
//     review of 2026-11-14: they used to write it anyway and free the holder's word).
//  2. The page and four workers taking at once for a while: no more requests go than the
//     budget holds, and the word is free at the end.
//  3. Two tabs of the origin: each takes half of the budget, a 429 in one stops the other,
//     and when one closes the other has the whole again.
//   node tests/budget-browser.mjs          (PLAYWRIGHT as elsewhere)
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import * as nodeModule from 'node:module';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const bridge = await readFile(new URL('../host/fetch-bridge.js', import.meta.url), 'utf8');
// (budget.ts as the page runs it: node's own type stripping, else web/'s TypeScript)
const ts = await readFile(new URL('../web/src/budget.ts', import.meta.url), 'utf8');
const budget = nodeModule.stripTypeScriptTypes
  ? nodeModule.stripTypeScriptTypes(ts)
  : nodeModule.createRequire(new URL('../web/package.json', import.meta.url))('typescript').transpileModule(ts, { compilerOptions: { target: 99, module: 99 } }).outputText;

const server = createServer((req, res) => {
  // (SharedArrayBuffer: cross-origin isolated, as masu serve's pages)
  const h = { 'cross-origin-opener-policy': 'same-origin', 'cross-origin-embedder-policy': 'require-corp' };
  if (req.url === '/budget.js') res.writeHead(200, { ...h, 'content-type': 'text/javascript' }).end(budget);
  else if (req.url === '/bridge.js') res.writeHead(200, { ...h, 'content-type': 'text/javascript' }).end(bridge);
  else res.writeHead(200, { ...h, 'content-type': 'text/html' }).end('<!doctype html><title>budget</title>');
}).listen(0, '127.0.0.1');
await new Promise((r) => server.once('listening', r));
const origin = `http://127.0.0.1:${server.address().port}`;
const browser = await chromium.launch();
try {
  const context = await browser.newContext();
  const page = await context.newPage();
  await page.goto(`${origin}/`);
  assert.equal(await page.evaluate(() => crossOriginIsolated), true);

  const r = await page.evaluate(async () => {
    const b = await import('/budget.js');
    const src = await (await fetch('/bridge.js')).text();
    const worker = (main) => new Worker(URL.createObjectURL(new Blob([src, '\n', main], { type: 'text/javascript' })));
    const out = {};
    // 1. a holder that does not let go for 400 ms
    {
      const buf = new SharedArrayBuffer(b.BYTES);
      b.fresh(buf, Date.now());
      const w = worker(`self.onmessage = (e) => {
        masuBudgetInit(e.data.buf, 2);
        masuBudgetWith(() => { postMessage('in'); const end = Date.now() + 400; while (Date.now() < end); });
        postMessage('out');
      };`);
      const said = [];
      const inside = new Promise((res) => (w.onmessage = (e) => (said.push(e.data), e.data === 'in' && res())));
      w.postMessage({ buf });
      await inside;
      const lock = new Int32Array(buf, 0, 1);
      const v = new Float64Array(buf, 8, 4);
      const before = [...v];
      let taken = 0;
      for (let i = 0; i < 20; i++) if (b.take(0, Date.now(), buf)) taken++;
      b.drain(1000, Date.now(), buf);
      out.held = { taken, lock: lock[0], same: JSON.stringify([...v]) === JSON.stringify(before) };
      await new Promise((res) => setTimeout(res, 600));
      out.after = { said, lock: lock[0], blocked: v[2] > Date.now(), take: b.take(0, Date.now() + 2000, buf) };
      w.terminate();
    }
    // 2. five takers at once
    {
      const buf = new SharedArrayBuffer(b.BYTES);
      const t0 = Date.now();
      b.fresh(buf, t0);
      const MS = 800;
      const ws = Array.from({ length: 4 }, (_, i) => worker(`self.onmessage = (e) => {
        masuBudgetInit(e.data.buf, ${i + 2});
        const end = Date.now() + e.data.ms;
        let n = 0;
        while (Date.now() < end) if (masuTake(${i % 2})) n++;
        postMessage(n);
      };`));
      const counts = ws.map((w) => new Promise((res) => (w.onmessage = (e) => res(e.data))));
      ws.forEach((w) => w.postMessage({ buf, ms: MS }));
      let mine = 0;
      // (the page between tasks too, as it is: a while of takes, then a turn of its event loop)
      const end = Date.now() + MS;
      while (Date.now() < end) {
        for (let i = 0; i < 200; i++) if (b.take(0, Date.now(), buf)) mine++;
        await new Promise((res) => setTimeout(res, 0));
      }
      const all = mine + (await Promise.all(counts)).reduce((a, n) => a + n, 0);
      const secs = (Date.now() - t0) / 1000;
      out.many = { all, max: Math.ceil(b.BURST + b.RATE * secs - b.RESERVE[0]) + 1, lock: new Int32Array(buf, 0, 1)[0] };
      ws.forEach((w) => w.terminate());
    }
    return out;
  });
  console.log('held:', JSON.stringify(r.held), 'after:', JSON.stringify(r.after), 'many:', JSON.stringify(r.many));
  assert.deepEqual(r.held, { taken: 0, lock: 2, same: true });
  assert.deepEqual(r.after.said, ['in', 'out']);
  assert.equal(r.after.lock, 0);
  assert.equal(r.after.blocked, true, 'the 429 told while the word was held landed after');
  assert.equal(r.after.take, true);
  assert.ok(r.many.all > 0 && r.many.all <= r.many.max, `${r.many.all} requests taken, at most ${r.many.max}`);
  assert.equal(r.many.lock, 0);

  // 3. two tabs
  const start = (p) =>
    p.evaluate(async () => {
      const b = await import('/budget.js');
      globalThis.stopShare = b.shareAcrossTabs();
      globalThis.b = b;
    });
  const share = (p) => p.evaluate(() => new Float64Array(globalThis.b.netBudget(), 8, 4)[3]);
  const other = await context.newPage();
  await other.goto(`${origin}/`);
  await start(page);
  await start(other);
  await page.waitForTimeout(2500);
  assert.deepEqual([await share(page), await share(other)], [0.5, 0.5]);
  await page.evaluate(() => globalThis.b.drain(5000));
  await other.waitForTimeout(300);
  const stopped = await other.evaluate(() => ({ take: globalThis.b.take(0), blocked: new Float64Array(globalThis.b.netBudget(), 8, 4)[2] - Date.now() }));
  console.log('other tab after a 429 in the first:', JSON.stringify(stopped));
  assert.equal(stopped.take, false);
  assert.ok(stopped.blocked > 3000);
  await page.close({ runBeforeUnload: true });
  await other.waitForTimeout(1500);
  assert.equal(await share(other), 1, 'the tab left: its share comes back');
  console.log('ok: budget lock and tabs');
} finally {
  await browser.close();
  server.close();
}
