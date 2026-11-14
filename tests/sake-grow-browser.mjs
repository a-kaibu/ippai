// Real Chromium: sake.wasm's 4 GB memory, and the start again on sake-grow (web/src/runtime.ts
// sakeBuild, growRetry; play.ts startOnGrow) when sake.wasm does not start with it although the
// memory could be had (the review of 2026-11-14: only a failed reservation went to sake-grow).
// A module that imports the 4 GB shared memory as sake.wasm does, and then cannot be made:
//  1. its import refused (LinkError), 2. its start traps (as sake aborting before its main).
// For each: the page goes to &memory=grow, and that device's next start (a fresh page) picks
// sake-grow without trying 4 GB again; &memory=full tries it anyway. A state's start is not
// retried. 3. A browser that cannot reserve 4 GB at all (V8 limited to fewer pages, as a
// phone): sake-grow from the start.
//   node tests/sake-grow-browser.mjs          (PLAYWRIGHT as elsewhere)
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import * as nodeModule from 'node:module';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const ts = await readFile(new URL('../web/src/runtime.ts', import.meta.url), 'utf8');
const runtime = nodeModule.stripTypeScriptTypes
  ? nodeModule.stripTypeScriptTypes(ts)
  : nodeModule.createRequire(new URL('../web/package.json', import.meta.url))('typescript').transpileModule(ts, { compilerOptions: { target: 99, module: 99 } }).outputText;

const server = createServer((req, res) => {
  const h = { 'cross-origin-opener-policy': 'same-origin', 'cross-origin-embedder-policy': 'require-corp' };
  if (req.url === '/runtime.js') res.writeHead(200, { ...h, 'content-type': 'text/javascript' }).end(runtime);
  else res.writeHead(200, { ...h, 'content-type': 'text/html' }).end('<!doctype html><title>grow</title>');
}).listen(0, '127.0.0.1');
await new Promise((r) => server.once('listening', r));
const origin = `http://127.0.0.1:${server.address().port}`;

/** What a page start does: the build, then (for sake) a module like sake.wasm made with it. */
const start = (page, how) =>
  page.evaluate(async (how) => {
    const r = await import('/runtime.js');
    const params = new URLSearchParams(location.search);
    const build = r.sakeBuild(params.get('memory'));
    const out = { build: build.name, bytes: build.memory?.buffer.byteLength ?? 0, to: null, error: '' };
    if (build.name !== 'sake') return out;
    // (imports env.memory shared 65535 pages as sake.wasm, env.f; start: f, or a trap)
    const s = (t) => [t.length, ...new TextEncoder().encode(t)];
    const sec = (id, b) => [id, b.length, ...b];
    const bytes = new Uint8Array([
      0, 97, 115, 109, 1, 0, 0, 0,
      ...sec(1, [1, 0x60, 0, 0]),
      ...sec(2, [2, ...s('env'), ...s('memory'), 2, 3, 0xff, 0xff, 3, 0xff, 0xff, 3, ...s('env'), ...s('f'), 0, 0]),
      ...sec(3, [1, 0]),
      ...sec(8, [1]),
      ...sec(10, [1, 3, 0, 0, 0x0b]),
    ]);
    const module = await WebAssembly.compile(bytes);
    const imports = { env: { memory: build.memory, ...(how === 'link' ? {} : { f: () => {} }) } };
    try {
      await WebAssembly.instantiate(module, imports);
      out.error = 'started';
    } catch (e) {
      out.error = e.constructor.name;
      out.to = r.growRetry(location.href, { build, fromState: how === 'state', started: false });
    }
    return out;
  }, how);

const browser = await chromium.launch();
try {
  for (const how of ['link', 'trap']) {
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.goto(`${origin}/play.html?game=a&run=Z:%5Ca.exe`);
    const first = await start(page, how);
    console.log(how, 'first start:', JSON.stringify(first));
    assert.equal(first.build, 'sake');
    assert.equal(first.bytes, 65535 * 65536, 'the 4 GB memory was had');
    assert.equal(first.error, how === 'link' ? 'LinkError' : 'RuntimeError');
    assert.equal(first.to, `${origin}/play.html?game=a&run=Z%3A%5Ca.exe&memory=grow`);
    await page.goto(first.to);
    const again = await start(page, how);
    assert.deepEqual([again.build, again.to], ['sake-grow', null]);
    // (the device remembers: a later start without the parameter, a fresh page)
    const later = await context.newPage();
    await later.goto(`${origin}/play.html?game=b`);
    assert.equal((await start(later, how)).build, 'sake-grow');
    await later.goto(`${origin}/play.html?game=b&memory=full`);
    assert.equal((await start(later, how)).build, 'sake');
    await context.close();
  }
  {
    // a state's start: its memory is the state's, so no retry on another build
    const context = await browser.newContext();
    const page = await context.newPage();
    await page.goto(`${origin}/play.html?game=a&state=1`);
    const r = await start(page, 'state');
    assert.deepEqual([r.build, r.to], ['sake', null]);
    await context.close();
  }
} finally {
  await browser.close();
}
// 3. no 4 GB at all (V8 allowed fewer pages: as a phone that refuses the reservation)
const small = await chromium.launch({ args: ['--js-flags=--wasm-max-mem-pages=40000'] });
try {
  const page = await small.newPage();
  await page.goto(`${origin}/play.html?game=a`);
  const r = await start(page, 'link');
  console.log('no 4 GB:', JSON.stringify(r));
  assert.equal(r.build, 'sake-grow');
} finally {
  await small.close();
  server.close();
}
console.log('ok: sake-grow after a failed start with 4 GB');
