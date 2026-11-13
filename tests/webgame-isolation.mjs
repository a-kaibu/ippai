// Web games (NW.js pages, web/src/html.ts + frame.ts) run on ippai serve's game listener: another
// origin than the app. Packs a fake NW.js game whose page tries to reach the app, starts the
// server and checks in Chromium that the game gets its saves, writes them back, and reaches
// nothing else: not the page, not the server's other routes.
// node tests/webgame-isolation.mjs [path to ippai] (PLAYWRIGHT, CHROME as in fetch-browser.mjs)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = process.argv[2] ?? "target/debug/ippai";
const work = mkdtempSync(join(tmpdir(), "ippai-webgame-"));
const game = join(work, "game");
mkdirSync(join(game, "www"), { recursive: true });
writeFileSync(join(game, "Game.exe"), "MZ not really");
writeFileSync(join(game, "package.json"), JSON.stringify({ main: "www/index.html" }));
writeFileSync(join(game, "www", "data.txt"), "from the game's folder");
// every byte value, read back through the synchronous fs (web/src/sync-read.ts)
writeFileSync(join(game, "www", "bytes.bin"), Uint8Array.from({ length: 512 }, (_, i) => (i * 7) & 255));
// a big file the game never reads with fs: it must not be sent to the frame ahead (a 950 MB
// RPG Maker MV folder sent ahead took the tab past 8 GB)
mkdirSync(join(game, "www", "img"));
writeFileSync(join(game, "www", "img", "big.png"), new Uint8Array(32 << 20).fill(0x5a));
writeFileSync(
  join(game, "www", "index.html"),
  `<!doctype html><html><head><title>fake game</title><script>
  const out = {};
  try { out.parent = String(parent.document.title); } catch (e) { out.parent = "blocked"; }
  out.origin = location.origin;
  out.nodeBefore = typeof require;
  window.addEventListener("load", async () => {
    out.saved = localStorage.getItem("progress");
    out.fs = require("fs").readFileSync("www/data.txt", "utf8");
    const b = require("fs").readFileSync("www/bytes.bin");
    out.bytes = b.length + ":" + Array.from(b).every((x, i) => x === ((i * 7) & 255));
    out.exists = [require("fs").existsSync("www/img/big.png"), require("fs").existsSync("www/img"), require("fs").existsSync("www/none.txt")].join();
    try { require("fs").readFileSync("www/none.txt"); out.missing = "read"; } catch (e) { out.missing = e.code; }
    localStorage.setItem("progress", String(Number(out.saved ?? 0) + 1));
    for (const path of ["/saves/x", "/api/favorites", "/store/trees/sample.tsv", "/game-origin"]) {
      try { out[path] = (await fetch(path)).status; } catch { out[path] = "blocked"; }
      try { await fetch(new URL(path, document.referrer || "http://127.0.0.1/").href, { mode: "cors" }); out["app " + path] = "read"; }
      catch { out["app " + path] = "blocked"; }
    }
    try { await fetch(new URL("/saves/x", document.referrer).href, { method: "PUT", body: "x" }); out.put = "sent"; }
    catch { out.put = "blocked"; }
    document.title = "done " + JSON.stringify(out);
  });
  </script></head><body>game</body></html>`,
);
const store = join(work, "store");
execFileSync(bin, ["--store", store, "add", game, "--exe", "Game.exe", "--id", "sample", "--no-meta"], { stdio: "inherit" });

// Electron games: the page is resources/app/index.html. An Electron 7 one with
// nodeIntegration (TyranoScript v5, configSave=file) saves through require("fs") next to
// the exe; a newer one (contextIsolation by default) has no Node in its page.
const electron = (id, version) => {
  const dir = join(work, id);
  const app = join(dir, "resources", "app");
  mkdirSync(app, { recursive: true });
  writeFileSync(join(dir, "game.exe"), "MZ not really");
  writeFileSync(join(dir, "version"), version);
  writeFileSync(join(app, "package.json"), JSON.stringify({ name: id, main: "main.js" }));
  writeFileSync(
    join(app, "main.js"),
    `const { app, BrowserWindow } = require("electron");
app.on("ready", () => {
  const w = new BrowserWindow({ width: 1280, height: 720, webPreferences: { nodeIntegration: true } });
  w.loadURL("file://" + __dirname + "/index.html");
});`,
  );
  writeFileSync(
    join(app, "index.html"),
    `<!doctype html><html><head><title>fake electron game</title><script>
  const out = { require: typeof require, process: typeof process, studio: typeof studio_api };
  window.addEventListener("load", () => {
    if (out.require === "function") {
      const fs = require("fs");
      const dir = require("electron").remote.app.getAppPath().replace("\\\\resources\\\\app", "");
      const file = dir + "/" + "tyrano_sf.sav";
      out.saved = fs.existsSync(file) ? unescape(fs.readFileSync(file)) : null;
      fs.writeFileSync(file, escape(JSON.stringify({ n: Number(JSON.parse(out.saved ?? '{"n":0}').n) + 1, s: "セーブ" })));
      out.platform = process.platform;
    }
    document.title = "done " + JSON.stringify(out);
  });
  </script></head><body>game</body></html>`,
  );
  execFileSync(bin, ["--store", store, "add", dir, "--exe", "game.exe", "--id", id, "--no-meta"], { stdio: "inherit" });
};
electron("tyrano", "7.1.2");
electron("isolated", "22.3.27");

const port = 18000 + Math.floor(Math.random() * 1000) * 2;
const server = spawn(
  bin,
  ["--store", store, "serve", "--foreground", "--web", "web/dist", "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit", env: { ...process.env, XDG_RUNTIME_DIR: work } },
);
const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROME, chromiumSandbox: false });
try {
  for (let i = 0; ; i++) {
    try {
      await fetch(`http://127.0.0.1:${port}/game-origin`);
      break;
    } catch (e) {
      if (i > 100) throw e;
      await new Promise((r) => setTimeout(r, 100));
    }
  }
  const page = await browser.newPage();
  page.on("console", (m) => console.log("page:", m.text()));
  page.on("pageerror", (e) => console.log("page error:", e.message));
  const big = [];
  page.on("request", (r) => r.url().includes("big.png") && big.push(r.url()));
  const playOf = (id, exe) =>
    `http://127.0.0.1:${port}/play.html?autostart&game=${id}&mount=/ippai/game=${id}&run=${encodeURIComponent(`Z:\\ippai\\game\\${exe}`)}`;
  const result = async (play = playOf("sample", "Game.exe"), id = "sample") => {
    await page.goto(play);
    await page.waitForSelector("#frame", { state: "attached", timeout: 20000 });
    let f;
    for (let i = 0; i < 100 && !f; i++) {
      f = page.frames().find((x) => x.url().includes(`/tree/${id}/`));
      if (!f) await page.waitForTimeout(100);
    }
    assert.ok(f, "the game's frame");
    await f.waitForFunction(() => document.title.startsWith("done "), null, { timeout: 20000 });
    return JSON.parse((await f.title()).slice(5));
  };
  const first = await result();
  console.log(first);
  assert.equal(first.parent, "blocked", "the game reaches the app's page");
  assert.equal(first.origin, `http://127.0.0.1:${port + 1}`);
  assert.equal(first.nodeBefore, "function", "require is there before the game's scripts");
  assert.equal(first.saved, null);
  assert.equal(first.fs, "from the game's folder");
  assert.equal(first.bytes, "512:true", "binary files read through the synchronous fs");
  assert.equal(first.exists, "true,true,false", "existsSync of a file, a folder, nothing");
  assert.equal(first.missing, "ENOENT");
  assert.deepEqual(big, [], "a file the game does not read is not fetched");
  for (const path of ["/saves/x", "/api/favorites", "/store/trees/sample.tsv", "/game-origin"]) {
    assert.equal(first[path], 404, `${path} on the game listener`);
    assert.equal(first[`app ${path}`], "blocked", `${path} on the app read from the game`);
  }
  assert.equal(first.put, "blocked", "a write to the app from the game");
  // the save made it to the app's page and survives a reload
  await page.waitForTimeout(3500);
  const second = await result();
  assert.equal(second.saved, "1", "the game's save after a reload");

  const tyrano = () => result(playOf("tyrano", "game.exe"), "tyrano");
  const t1 = await tyrano();
  console.log(t1);
  assert.deepEqual(
    [t1.require, t1.process, t1.studio, t1.saved, t1.platform],
    ["function", "function", "undefined", null, "win32"],
    "an Electron 7 page with nodeIntegration gets require and process",
  );
  await page.waitForTimeout(3500);
  const t2 = await tyrano();
  assert.equal(t2.saved, JSON.stringify({ n: 1, s: "セーブ" }), "its fs save after a reload");
  const iso = await result(playOf("isolated", "game.exe"), "isolated");
  console.log(iso);
  assert.deepEqual(
    [iso.require, iso.process, iso.studio],
    ["undefined", "undefined", "object"],
    "a newer Electron page (contextIsolation) runs without Node",
  );
  console.log("web game isolation: passed");
} finally {
  await browser.close();
  server.kill();
}
