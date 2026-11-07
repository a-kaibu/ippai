// Save states in the page (web/src/savestate.ts, ui/States.tsx, docs/savestate.md): starts ippai
// serve on a small store with host/tests/snaptest.c as a game and, in Chromium, takes a state
// from the dialog while it runs, then loads it: the page starts again from the state and the
// program prints the rest of its lines, the same as a whole run (its steps and checksums go on
// from where the state was). Then a browser with nothing of its own (another device) goes on
// from the server's copy of the state.
//
//   node tests/savestate-browser.mjs [path to ippai] [snaptest.exe]
//   (web/dist built with sake in it: crates/sake/build.sh, then pnpm build; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-state-"));
assert.ok(existsSync("web/dist/sake/sake.wasm"), "web/dist has no sake build");
let exe = process.argv[3];
if (!exe) {
  exe = join(work, "snaptest.exe");
  execFileSync("i686-w64-mingw32-gcc", ["-O2", "host/tests/snaptest.c", "-o", exe]);
}
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(game);
cpSync(exe, join(game, "snaptest.exe"));
run("pack", game, "g");
run("add-game", "g", "--title", "ステートの試験", "--tree", "g", "--exe", "snaptest.exe", "--no-meta");

const port = 8900 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit" },
);
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}

/** A browser of its own (a device): its page and the program's lines. */
async function device(name) {
  const ctx = await chromium.launchPersistentContext(join(work, name), { args: ["--enable-unsafe-webgpu"], viewport: { width: 1280, height: 720 } });
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  const lines = [];
  page.on("console", (m) => {
    lines.push(m.text());
    if (process.env.VERBOSE) console.log(`[${name}] ${m.text()}`);
  });
  page.on("pageerror", (e) => lines.push(`pageerror: ${e.message}`));
  return { ctx, page, lines };
}
const program = (lines) => lines.filter((l) => /^(step \d+|late thread|clocks|sleeper|end)/.test(l));
async function until(lines, re, ms = 120000) {
  const t = Date.now();
  while (!lines.some((l) => re.test(l))) {
    if (Date.now() - t > ms) throw new Error(`no line matching ${re}; last: ${lines.slice(-8).join(" | ")}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}
const play = `${base}play.html?game=g&headless&autostart`;

let failed = false;
try {
  // the whole run, a state taken in the middle (from the dialog)
  const a = await device("a");
  await a.page.goto(play);
  await until(a.lines, /^step 15:/);
  await a.page.getByRole("button", { name: "どこでもセーブ" }).click();
  await a.page.getByRole("dialog").getByRole("button", { name: "セーブ" }).first().click();
  await until(a.lines, /^state: .* stood still \d+ ms/, 60000);
  await a.page.getByText("スロット 1 にセーブしました").waitFor({ timeout: 60000 });
  await until(a.lines, /^end$/);
  const whole = program(a.lines);
  assert.equal(whole.at(-1), "end");
  assert.ok(whole.includes("clocks: ok"), whole.join("\n"));
  // (and on the server)
  await a.page.waitForFunction(() => !document.body.textContent?.includes("サーバーに送信中"), null, { timeout: 120000 });

  // loaded: the page starts again from it, the rest of the lines follow
  const check = (lines, who) => {
    const got = program(lines);
    assert.ok(got.length > 0 && got.length < whole.length, `${who}: ${got.length} lines of ${whole.length}`);
    assert.deepEqual(got, whole.slice(whole.length - got.length), `${who}: not the rest of the whole run`);
    assert.ok(!lines.some((l) => /pageerror|panicked|unreachable/.test(l)), `${who}: ${lines.filter((l) => /pageerror|panicked|unreachable/.test(l)).join("\n")}`);
  };
  a.lines.length = 0;
  // (the program ended and the page went back to the list: as the dialog's ロード does)
  await a.page.goto(`${play}&state=0`);
  await until(a.lines, /^end$/);
  check(a.lines, "loaded");

  // another device: nothing of its own, the server's state
  const b = await device("b");
  await b.page.goto(`${play}&state=0`);
  await until(b.lines, /^end$/);
  check(b.lines, "another device");
  console.log("ok   save states in the page: taken, loaded, loaded on another device");
  await a.ctx.close();
  await b.ctx.close();
} catch (e) {
  failed = true;
  console.error(`FAIL ${e.stack ?? e}`);
} finally {
  server.kill();
}
process.exit(failed ? 1 : 0);
