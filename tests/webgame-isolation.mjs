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
  const play = `http://127.0.0.1:${port}/play.html?autostart&game=sample&mount=/ippai/game=sample&run=${encodeURIComponent("Z:\\ippai\\game\\Game.exe")}`;
  const result = async () => {
    await page.goto(play);
    await page.waitForSelector("#frame", { state: "attached", timeout: 20000 });
    let f;
    for (let i = 0; i < 100 && !f; i++) {
      f = page.frames().find((x) => x.url().includes("/tree/sample/"));
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
  for (const path of ["/saves/x", "/api/favorites", "/store/trees/sample.tsv", "/game-origin"]) {
    assert.equal(first[path], 404, `${path} on the game listener`);
    assert.equal(first[`app ${path}`], "blocked", `${path} on the app read from the game`);
  }
  assert.equal(first.put, "blocked", "a write to the app from the game");
  // the save made it to the app's page and survives a reload
  await page.waitForTimeout(3500);
  const second = await result();
  assert.equal(second.saved, "1", "the game's save after a reload");
  console.log("web game isolation: passed");
} finally {
  await browser.close();
  server.kill();
}
