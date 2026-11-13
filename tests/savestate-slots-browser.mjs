// Save state slots from two devices at once (web/src/savestate.ts putState/getState), in real
// Chromium against a real ippai serve: the same slot saved from a PC and a phone, their uploads
// interleaved in every order (one device's requests held back by a route). Whatever the order,
// the slot ends with the newer state, the server's index names a file that is that state's bytes
// (files are named by their content and never written over), and a third device loads exactly
// those bytes. Also a state of the old layout (no hash, `state.<game>.<slot>`) still loads.
// Then the server starts again with every save a week and more old: the files no index names
// are gone (Saves::prune_states, issue #90), the ones it names and the old layout's still load.
//
//   node tests/savestate-slots-browser.mjs [path to ippai]   (web's node_modules installed; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-slots-"));
const vp = await import(createRequire(resolve("web/package.json")).resolve("vite-plus"));
const build = vp.build ?? vp.default.build;
const built = await build({
  configFile: false,
  logLevel: "silent",
  build: { write: false, minify: false, lib: { entry: resolve("web/src/savestate.ts"), formats: ["iife"], name: "ippaiStates" } },
});
const script = (Array.isArray(built) ? built[0] : built).output[0].code;

const web = join(work, "web");
mkdirSync(web);
writeFileSync(join(web, "index.html"), "<!doctype html><title>states</title>");
const port = 9500 + Math.floor(Math.random() * 200) * 2;
const base = `http://127.0.0.1:${port}/`;
async function start() {
  const s = spawn(bin, ["serve", "--foreground", "--web", web, "--store", join(work, "store"), "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: "inherit" });
  for (let i = 0; i < 100; i++) {
    if (await fetch(`${base}saves/x`).then((r) => r.status === 404, () => false)) break;
    await new Promise((r) => setTimeout(r, 300));
  }
  return s;
}
let server = await start();

async function device(name) {
  const ctx = await chromium.launchPersistentContext(join(work, name), { args: ["--no-proxy-server"] });
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  page.on("console", (m) => process.env.VERBOSE && console.log(`[${name}] ${m.text()}`));
  await page.goto(base);
  await page.addScriptTag({ content: script });
  return { ctx, page };
}
/** Holds back `page`'s PUTs whose name matches `re` by `ms`. */
const hold = (page, re, ms) =>
  page.route(/\/saves\/state\./, async (route) => {
    const req = route.request();
    if (req.method() === "PUT" && re.test(decodeURIComponent(req.url()))) await new Promise((r) => setTimeout(r, ms));
    await route.continue();
  });
const put = (page, slot, text, created) =>
  page.evaluate(
    async ([slot, text, created]) => {
      const file = new TextEncoder().encode(text);
      const info = { slot, created, size: file.length, build: "b", thumb: "" };
      const { uploaded } = await window.ippaiStates.putState("g", info, file);
      await uploaded;
    },
    [slot, text, created],
  );
const load = (page, slot) =>
  page.evaluate(async (slot) => {
    const info = (await window.ippaiStates.listStates("g")).find((s) => s.slot === slot);
    return info ? { info, text: new TextDecoder().decode(await window.ippaiStates.getState("g", info)) } : null;
  }, slot);

try {
  const pc = await device("pc");
  const phone = await device("phone");
  // the older state's file upload lands last; then its index write; then both at once
  const orders = [
    [/\.[0-9a-f]{32}$/, 0],
    [/\.index$/, 0],
    [/\.[0-9a-f]{32}$/, 1],
    [/\.index$/, 1],
    [/./, 0],
  ];
  for (const [round, [re, who]] of orders.entries()) {
    await pc.page.unrouteAll();
    await phone.page.unrouteAll();
    await hold(who ? phone.page : pc.page, re, 400);
    const t = Date.UTC(2026, 10, 8, 0, round);
    const older = new Date(t).toISOString();
    const newer = new Date(t + 1000).toISOString();
    // (pc's is older; both go at once)
    await Promise.all([put(pc.page, 0, `pc ${round} `.repeat(1000), older), put(phone.page, 0, `phone ${round} `.repeat(1000), newer)]);
    const third = await device(`third${round}`);
    const got = await load(third.page, 0);
    assert.equal(got?.info.created, newer, `round ${round}: the newer state is the slot's`);
    assert.equal(got.text, `phone ${round} `.repeat(1000), `round ${round}: the slot's file is not its state`);
    // and the two devices see the same
    for (const d of [pc, phone]) assert.equal((await load(d.page, 0)).text, got.text, `round ${round}`);
    await third.ctx.close();
  }
  // a state of the old layout (before files were named by their content) loads
  await fetch(`${base}saves/state.g.3`, { method: "PUT", headers: { "if-match": '"0"' }, body: "old state" });
  const idx = await (await fetch(`${base}saves/state.g.index`)).json();
  const rev = (await fetch(`${base}saves/state.g.index`, { method: "HEAD" })).headers.get("etag");
  idx.states.push({ slot: 3, created: "2026-01-01T00:00:00.000Z", size: 9, build: "b", thumb: "" });
  assert.equal((await fetch(`${base}saves/state.g.index`, { method: "PUT", headers: { "if-match": rev }, body: JSON.stringify(idx) })).status, 204);
  const old = await device("old");
  assert.equal((await load(old.page, 3)).text, "old state");
  await old.ctx.close();
  await pc.ctx.close();
  await phone.ctx.close();

  // a week later: the server starts and drops the files no index names
  const hash = (text) => createHash("sha256").update(text).digest().subarray(0, 16).toString("hex");
  const file = (text) => `${base}saves/state.g.${hash(text)}`;
  const named = `phone ${orders.length - 1} `.repeat(1000);
  const unnamed = [...orders.keys()].flatMap((r) => [`pc ${r} `.repeat(1000), `phone ${r} `.repeat(1000)]).filter((t) => t !== named);
  for (const t of [named, ...unnamed]) assert.equal((await fetch(file(t))).status, 200, "every state's file is there before");
  server.kill();
  await new Promise((r) => server.once("exit", r));
  const aged = spawnSync("python3", ["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute('UPDATE saves SET written_ms=written_ms-8*86400000'); c.commit()", join(work, "store", "library.sqlite3")], { stdio: "inherit" });
  assert.equal(aged.status, 0);
  server = await start();
  for (let i = 0; i < 100 && (await fetch(file(unnamed[0]))).status !== 404; i++) await new Promise((r) => setTimeout(r, 100));
  for (const t of unnamed) assert.equal((await fetch(file(t))).status, 404, `a file no index names stays: ${t.slice(0, 12)}`);
  const later = await device("later");
  assert.equal((await load(later.page, 0)).text, named);
  assert.equal((await load(later.page, 3)).text, "old state");
  await later.ctx.close();
  console.log(`savestate-slots-browser: ok (${orders.length} orders, ${unnamed.length} unnamed files pruned)`);
} finally {
  server.kill();
}
