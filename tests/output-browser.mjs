// The program's output in Chromium (crates/kura/src/wrap.rs, host/pre.js, #221): the game's
// threads queue their stdout / stderr and the page's thread prints it when it gets to it. Before,
// each write of a thread waited for the page's thread (emscripten proxies it there), and a game
// stopped for good inside one, rarely. host/tests/outflood.c: four threads print numbered lines
// with GetTickCount while the page's thread is kept busy for 3 s. Every line arrives once, whole,
// in each thread's order, and no thread's lines show a gap the size of the busy spell (the
// writes did not wait for the page).
//
//   node tests/output-browser.mjs [path to masu]
//   (web/dist built with sake in it; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/masu");
const work = mkdtempSync(join(tmpdir(), "masu-output-"));
assert.ok(existsSync("web/dist/sake/sake.wasm"), "web/dist has no sake build");
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(game, { recursive: true });
execFileSync("i686-w64-mingw32-gcc", ["-O2", "host/tests/outflood.c", "-o", join(game, "flood.exe"), "-luser32"]);
run("pack", game, "flood");

const port = 9300 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit" },
);
process.on("exit", () => server.kill());
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}

const lines = [];
async function until(re, ms = 120000) {
  const t = Date.now();
  while (!lines.some((l) => re.test(l))) {
    if (Date.now() - t > ms) throw new Error(`no line matching ${re}; last: ${lines.slice(-12).join(" | ")}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

const browser = await chromium.launch({ args: ["--enable-unsafe-webgpu"] });
let failed = false;
try {
  const page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
  page.on("console", (m) => {
    for (const l of m.text().split("\n")) lines.push(l);
    if (process.env.VERBOSE) console.log(m.text());
  });
  page.on("pageerror", (e) => lines.push(`pageerror: ${e.message}`));
  await page.goto(`${base}play.html?autostart&headless&mount=/masu/game=flood&run=${encodeURIComponent("Z:\\masu\\game\\flood.exe")}`);
  await until(/^flood t0 40 /);
  // the page's thread busy for 3 s, as on a slow phone or behind a long task
  const busy = await page.evaluate(() => {
    const t = Date.now();
    while (Date.now() - t < 3000) {}
    return Date.now() - t;
  });
  assert.ok(busy >= 3000);
  await until(/^flood end$/);
  const got = lines.filter((l) => l.startsWith("flood t"));
  for (const l of got) assert.match(l, /^flood t[0-3] \d+ \d+$/, `a torn line: ${JSON.stringify(l)}`);
  let worst = 0;
  for (let t = 0; t < 4; t++) {
    const mine = got.filter((l) => l.startsWith(`flood t${t} `)).map((l) => l.split(" ").slice(2).map(Number));
    assert.deepEqual(mine.map((m) => m[0]), [...Array(400).keys()], `thread ${t}: every line once, in order`);
    for (let i = 1; i < mine.length; i++) worst = Math.max(worst, mine[i][1] - mine[i - 1][1]);
  }
  // (10 ms apart; a write that waited for the page's thread would leave ~3000)
  assert.ok(worst < 1500, `a thread's writes waited for the page's thread: ${worst} ms between two lines`);
  assert.ok(!lines.some((l) => /pageerror|panicked/.test(l)), lines.filter((l) => /pageerror|panicked/.test(l)).join("\n"));
  console.log(`ok   the threads' output arrives whole and in order without waiting for the page (longest gap ${worst} ms)`);
} catch (e) {
  failed = true;
  console.error(`FAIL ${e.stack ?? e}`);
  console.error(lines.slice(-30).join("\n"));
} finally {
  await browser.close();
  server.kill();
}
process.exit(failed ? 1 : 0);
