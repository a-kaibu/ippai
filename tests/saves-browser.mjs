// Saved games across pages, in real Chromium against a real ippai serve (web/src/saves.ts, the
// server's /saves in crates/ippai/src/saves.rs): a save made offline, the browser closed, and
// the next start online must start from that save and send it, not from the server's older
// copy. Also with the network up but the server gone, with a page closed while its upload was
// on the way, and with another device (another browser profile) saving meanwhile: this
// device's later save wins, the other's is kept in this browser's OPFS beside it.
//
//   node tests/saves-browser.mjs [path to ippai]   (web's node_modules installed; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-saves-"));

// saves.ts as a script for the page (window.ippaiSaves), built with the web app's Vite
const vp = await import(createRequire(resolve("web/package.json")).resolve("vite-plus"));
const build = vp.build ?? vp.default.build;
const built = await build({
  configFile: false,
  logLevel: "silent",
  build: { write: false, minify: false, lib: { entry: resolve("web/src/saves.ts"), formats: ["iife"], name: "ippaiSaves" } },
});
const script = (Array.isArray(built) ? built[0] : built).output[0].code;

const port = 9100 + Math.floor(Math.random() * 200) * 2;
const base = `http://127.0.0.1:${port}/`;
const store = join(work, "store");
// (no web app needed: a page of the origin to run saves.ts in)
const web = join(work, "web");
mkdirSync(web);
writeFileSync(join(web, "index.html"), "<!doctype html><title>saves</title>");
let server;
async function startServer() {
  server = spawn(bin, ["serve", "--foreground", "--web", web, "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: "inherit" });
  for (let i = 0; i < 100; i++) {
    if (await fetch(`${base}saves/x`).then((r) => r.status === 404, () => false)) return;
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error("ippai serve did not start");
}
const serverSave = async () => {
  const r = await fetch(`${base}saves/game`, { cache: "no-store" });
  return r.ok ? new TextDecoder().decode(await r.arrayBuffer()) : null;
};

/** A browser of its own (a device), its page with saves.ts in it. */
async function device(name) {
  const ctx = await chromium.launchPersistentContext(join(work, name), { args: ["--no-proxy-server"], viewport: { width: 400, height: 300 } });
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  page.on("console", (m) => process.env.VERBOSE && console.log(`[${name}] ${m.text()}`));
  // (any page of the origin: OPFS and /saves are the origin's)
  await page.goto(base);
  await page.addScriptTag({ content: script });
  return { ctx, page };
}
const load = (page) =>
  page.evaluate(async () => {
    const notices = [];
    const b = await window.ippaiSaves.loadSaves("game", (m) => notices.push(m));
    return { text: b ? new TextDecoder().decode(b) : null, notices };
  });
const store_ = (page, text) =>
  page.evaluate(
    (t) => window.ippaiSaves.storeSaves("game", new TextEncoder().encode(t)).then(() => true, () => false),
    text,
  );
/** This device's OPFS saves folder: names and texts. */
const opfs = (page) =>
  page.evaluate(async () => {
    const out = {};
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("saves").catch(() => null);
    if (dir) for await (const [n, h] of dir.entries()) out[n] = await h.getFile().then((f) => f.text(), () => "(gone)");
    return out;
  });

const step = (n) => process.env.VERBOSE && console.log(`step ${n}`);
await startServer();
try {
  step(1);
  // 1. online: save A
  let { ctx, page } = await device("phone");
  assert.equal((await load(page)).text, null);
  assert.equal(await store_(page, "A"), true);
  assert.equal(await serverSave(), "A");
  await ctx.close();

  step(2);
  // 2. offline: start from this device's copy, save B (not sent), the browser closes
  ({ ctx, page } = await device("phone"));
  await ctx.setOffline(true);
  assert.equal((await load(page)).text, "A");
  assert.equal(await store_(page, "B"), false);
  await ctx.close();
  assert.equal(await serverSave(), "A");

  step(3);
  // 3. a later start, online: B, and the server has it
  ({ ctx, page } = await device("phone"));
  assert.equal((await load(page)).text, "B", "the offline save was lost to the server's older copy");
  assert.equal(await serverSave(), "B");
  await ctx.close();

  step(4);
  // 4. the network up but the server gone (a phone away from home): save C, closed; back home
  // (the page opened first: the app's own start offline is offline-browser.mjs's)
  ({ ctx, page } = await device("phone"));
  server.kill();
  await new Promise((r) => server.once("exit", r));
  assert.equal((await load(page)).text, "B");
  assert.equal(await store_(page, "C"), false);
  await ctx.close();
  await startServer();
  ({ ctx, page } = await device("phone"));
  assert.equal((await load(page)).text, "C");
  assert.equal(await serverSave(), "C");

  step(5);
  // 5. the page closes while its upload is on the way (the PUT never answered)
  await page.route("**/saves/game", (route) => (route.request().method() === "PUT" ? undefined : route.continue()));
  const pending = store_(page, "D").catch(() => "closed");
  await page.waitForFunction(async () => {
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("saves");
    return (await (await (await dir.getFileHandle("game.bin")).getFile()).text()) === "D";
  });
  // (Chromium commits an OPFS write to disk a moment after the page sees it: a browser killed
  // at once keeps the file as it was, the sync state already "dirty", which then sends the older
  // blob, as it should; this case is about the upload, so the write is let land)
  await new Promise((r) => setTimeout(r, 1000));
  await ctx.close();
  assert.equal(await pending, "closed");
  assert.equal(await serverSave(), "C");
  ({ ctx, page } = await device("phone"));
  assert.equal((await load(page)).text, "D");
  assert.equal(await serverSave(), "D");
  await ctx.close();

  step(6);
  // 6. saved offline here (E) while another device saved online (F): this device's later save
  // wins, the other's is kept here, and the server's revisions still have it
  ({ ctx, page } = await device("phone"));
  await ctx.setOffline(true);
  await load(page);
  assert.equal(await store_(page, "E"), false);
  await ctx.close();
  const pc = await device("pc");
  assert.equal((await load(pc.page)).text, "D");
  assert.equal(await store_(pc.page, "F"), true);
  await pc.ctx.close();
  ({ ctx, page } = await device("phone"));
  const back = await load(page);
  assert.equal(back.text, "E");
  assert.equal(back.notices.length, 1, "the user is told");
  assert.equal(await serverSave(), "E");
  const kept = await opfs(page);
  assert.ok(Object.entries(kept).some(([n, t]) => n.startsWith("game.server-") && t === "F"), JSON.stringify(kept));
  await ctx.close();

  step(7);
  // 7. and the other device's next start is this one's save
  const pc2 = await device("pc");
  assert.equal((await load(pc2.page)).text, "E");
  await pc2.ctx.close();
  console.log("saves-browser: ok");
} finally {
  server.kill();
}
