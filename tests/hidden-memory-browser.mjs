// A game drawing in a tab that went to the background (tests/memlong-browser.mjs HIDE: the
// animation frames stop, the game's threads go on) against host/tests/presentloop.c, a Direct3D
// 9 game rewriting a 1 MiB texture every frame and presenting as fast as it may: the host heap
// stays within GROW_MB (64) of what it was before the tab was hidden, the page draws nothing
// while hidden and goes on drawing when shown again. Before kura/src/d3d9/gpu.rs present() held
// the game while the page was away, the frames piled up in memory (2026-11-14: 金恋 out of
// memory 6-7 minutes after its tab was hidden).
//
//   node tests/hidden-memory-browser.mjs [path to ippai] [presentloop.exe]
//   (web/dist, or WEB, built with sake in it; PLAYWRIGHT as elsewhere; SECONDS default 180)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-hidden-"));
const web = resolve(process.env.WEB ?? "web/dist");
assert.ok(existsSync(join(web, "sake/sake.wasm")), `${web} has no sake build`);
let exe = process.argv[3];
if (!exe) {
  exe = join(work, "presentloop.exe");
  execFileSync("i686-w64-mingw32-gcc", ["-O2", "host/tests/presentloop.c", "-o", exe, "-mwindows", "-ld3d9"]);
}
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(game);
cpSync(exe, join(game, "presentloop.exe"));
run("pack", game, "g");
run("add-game", "g", "--title", "裏タブの試験", "--tree", "g", "--exe", "presentloop.exe", "--no-meta");

const port = 9500 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", web, "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit" },
);
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}
const secs = Number(process.env.SECONDS ?? 180);
const out = process.env.OUT ? resolve(process.env.OUT) : join(work, "out");
let code = 1;
try {
  code = await new Promise((done) => {
    const p = spawn(process.execPath, [resolve("tests/memlong-browser.mjs"), `${base}play.html?game=g&headless&autostart`, out, String(secs), "5"], {
      stdio: "inherit",
      env: { ...process.env, WARM: "40", HIDE: `60,${secs - 60}`, GROW_MB: process.env.GROW_MB ?? "64" },
    });
    p.on("exit", (c) => done(c ?? 1));
  });
} finally {
  server.kill();
}
console.log(code === 0 ? "ok: a hidden tab held the game, not its frames" : `FAILED (the samples: ${out}/mem.tsv)`);
process.exit(code);
