// Web apps kept for older builds' save states (crates/ippai/src/apps.rs, web/src/savestate.ts
// keptApps/appRoot), in real Chromium against real ippai serves: a server with web app A, then
// the same store served with app B (an update). A's files stay at /apps/<A>/, cross-origin
// isolated as the root's; its relative URLs reach the same server's saves; /apps/builds names
// both builds; the page code finds the app of a state's build from either app.
//
//   node tests/apps-browser.mjs [path to ippai]   (web's node_modules installed; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-apps-"));
const vp = await import(createRequire(resolve("web/package.json")).resolve("vite-plus"));
const build = vp.build ?? vp.default.build;
const built = await build({
  configFile: false,
  logLevel: "silent",
  build: { write: false, minify: false, lib: { entry: resolve("web/src/savestate.ts"), formats: ["iife"], name: "ippaiStates" } },
});
const script = (Array.isArray(built) ? built[0] : built).output[0].code;

const app = (name, id) => {
  const w = join(work, name);
  mkdirSync(join(w, "sake"), { recursive: true });
  writeFileSync(join(w, "sake/build-id.txt"), `sake ${id}\nsake-grow ${id.replace(/./, "f")}\n`);
  writeFileSync(join(w, "sake/sake.wasm"), name);
  writeFileSync(join(w, "play.html"), `<!doctype html><title>${name}</title><p id=v>${name}</p>`);
  writeFileSync(join(w, "index.html"), `<!doctype html><title>${name}</title>`);
  return w;
};
const A = app("A", "000000000000000a");
const B = app("B", "000000000000000b");
const port = 9700 + Math.floor(Math.random() * 100) * 2;
const base = `http://127.0.0.1:${port}/`;
const store = join(work, "store");
async function serve(web) {
  const s = spawn(bin, ["serve", "--foreground", "--web", web, "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: "inherit" });
  for (let i = 0; i < 100; i++) {
    if (await fetch(`${base}apps/builds`).then((r) => r.ok, () => false)) return s;
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error("ippai serve did not start");
}
let server = await serve(A);
server.kill();
await new Promise((r) => server.once("exit", r));
server = await serve(B);
const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  const builds = await (await fetch(`${base}apps/builds`)).json();
  const keyA = builds["000000000000000a"];
  assert.ok(keyA && builds["f00000000000000a"] === keyA, JSON.stringify(builds));
  assert.ok(builds["000000000000000b"] && builds["000000000000000b"] !== keyA);
  await fetch(`${base}saves/game`, { method: "PUT", headers: { "if-match": '"0"' }, body: "the save" });
  const page = await browser.newPage();
  await page.goto(`${base}apps/${keyA}/play.html?game=g&state=0`);
  const r = await page.evaluate(async () => ({
    v: document.getElementById("v").textContent,
    isolated: crossOriginIsolated,
    save: await (await fetch("saves/game")).text(),
    wasm: await (await fetch("sake/sake.wasm")).text(),
  }));
  assert.deepEqual(r, { v: "A", isolated: true, save: "the save", wasm: "A" });
  // the page code: from app A, the root is the server's and B's build is B's app; from the root too
  await page.addScriptTag({ content: script });
  const found = await page.evaluate(async () => ({
    root: window.ippaiStates.appRoot(),
    b: (await window.ippaiStates.keptApps()).get("000000000000000b"),
    a: (await window.ippaiStates.keptApps()).get("000000000000000a"),
  }));
  assert.equal(found.root, base);
  assert.equal(found.a, `${base}apps/${keyA}/`);
  assert.equal(found.b, `${base}apps/${builds["000000000000000b"]}/`);
  // a write from a kept app's page goes through as from the root's (same origin)
  const put = await page.evaluate(async () => (await fetch("saves/game", { method: "PUT", headers: { "if-match": '"1"' }, body: "from A" })).status);
  assert.equal(put, 204);
  assert.equal(await (await fetch(`${base}saves/game`)).text(), "from A");
  console.log("apps-browser: ok");
} finally {
  await browser.close();
  server.kill();
}
