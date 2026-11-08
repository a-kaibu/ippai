// The chunk cache in real Chromium (web/src/net.ts trimChunks): FastCDC chunks of MiBs in Cache
// Storage, the origin's quota made small (estimate() made to say so) and other data in
// OPFS (saves, states): after a trim the chunks are within half the quota and, with the rest,
// the origin within 80% of it, counted by the chunks' real sizes (also those kept without
// x-ippai-size, by an older page), the oldest gone first.
//
//   node tests/chunk-trim-browser.mjs   (web's node_modules installed; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const vp = await import(createRequire(resolve("web/package.json")).resolve("vite-plus"));
const build = vp.build ?? vp.default.build;
const built = await build({
  configFile: false,
  logLevel: "silent",
  build: { write: false, minify: false, lib: { entry: resolve("web/src/net.ts"), formats: ["iife"], name: "ippaiNet" } },
});
const script = (Array.isArray(built) ? built[0] : built).output[0].code;

const MiB = 1 << 20;
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  const page = await browser.newPage();
  const origin = "https://ippai.test";
  await page.route(`${origin}/**`, (r) => r.fulfill({ contentType: "text/html", body: "<!doctype html><title>chunks</title>" }));
  await page.goto(`${origin}/`);
  // (the quota made small: CDP's Storage.overrideQuotaForOrigin is not honored by headless
  // Chromium 1243, so estimate() says 200 MiB; the usage is the browser's own measure)
  await page.evaluate((q) => {
    const real = navigator.storage.estimate.bind(navigator.storage);
    navigator.storage.estimate = async () => ({ ...(await real()), quota: q });
  }, 200 * MiB);
  await page.addScriptTag({ content: script });
  const r = await page.evaluate(async (MiB) => {
    const cache = await caches.open("ippai-chunks-v1");
    // 40 chunks of 4 MiB (160 MiB), every 5th one kept without its size
    for (let i = 0; i < 40; i++) {
      const hex = i.toString(16).padStart(32, "0");
      const headers = i % 5 ? { "x-ippai-size": String(4 * MiB) } : {};
      await cache.put(`${location.origin}/store/chunks/00/${hex}`, new Response(new Uint8Array(4 * MiB).fill(i), { headers }));
    }
    // 30 MiB of states in OPFS
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("states", { create: true });
    const w = await (await dir.getFileHandle("s.snap", { create: true })).createWritable();
    await w.write(new Uint8Array(30 * MiB).fill(7));
    await w.close();
    const before = await navigator.storage.estimate();
    await window.ippaiNet.trimChunks();
    const keys = await cache.keys();
    let bytes = 0;
    for (const k of keys) bytes += (await (await cache.match(k)).arrayBuffer()).byteLength;
    return { before, left: keys.map((k) => parseInt(k.url.slice(-32), 16)), bytes };
  }, MiB);
  // half of 200 MiB is 100; 80% of it less the 30 MiB of states (and the little else) ~130:
  // 100 MiB of chunks at most, 25 of the 40
  assert.equal(r.before.quota, 200 * MiB, "quota override");
  assert.ok(r.bytes <= 100 * MiB, `chunks left ${r.bytes / MiB} MiB`);
  assert.ok(r.bytes > 90 * MiB, `too many went: ${r.bytes / MiB} MiB`);
  assert.deepEqual(r.left, r.left.map((_, i) => 40 - r.left.length + i), "the oldest go first");

  // with the states larger (120 MiB more), the whole origin must fit in 160 MiB: chunks give way
  const r2 = await page.evaluate(async (MiB) => {
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("states");
    const w = await (await dir.getFileHandle("t.snap", { create: true })).createWritable();
    await w.write(new Uint8Array(120 * MiB).fill(8));
    await w.close();
    await window.ippaiNet.trimChunks();
    const cache = await caches.open("ippai-chunks-v1");
    let bytes = 0;
    for (const k of await cache.keys()) bytes += (await (await cache.match(k)).arrayBuffer()).byteLength;
    return { bytes, est: await navigator.storage.estimate() };
  }, MiB);
  // 160 - 150 of states: about 10 MiB of chunks left at most (the browser's usage lags the
  // deletes: fewer may stay)
  assert.ok(r2.bytes <= 12 * MiB, `chunks left beside the states: ${r2.bytes / MiB} MiB`);
  console.log(`chunk-trim-browser: ok (${r.bytes / MiB} MiB of 160 kept, then ${r2.bytes / MiB} MiB beside 150 MiB of states)`);
} finally {
  await browser.close();
}
