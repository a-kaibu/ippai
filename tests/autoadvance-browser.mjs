// 自動送り (web/src/autoadvance.ts, its panel in ui/States.tsx' どこでもセーブ dialog) in the page,
// on sake, against host/tests/autoadvance.c (a window over the screen logging its clicks and
// keys): a real Ctrl from the keyboard arrives as Windows gives it (WM_KEYDOWN VK_CONTROL, not
// VK_LCONTROL; GetKeyState(VK_CONTROL) down); クリック連打 at 20 a second clicks where the pointer
// was, every down with its up, and stops at once from the button over the game (nothing left
// down); Ctrl押しっぱなし holds Ctrl, repeating (lParam bit 30) with GetAsyncKeyState down all
// along, let go when stopped; Enter連打 presses Enter; a state taken while it runs; the mode and
// the speed kept across a reload. SHOTS=<dir> keeps screenshots.
//
//   node tests/autoadvance-browser.mjs [path to ippai] [autoadvance.exe]
//   (web/dist built with sake in it: crates/sake/build.sh, then pnpm build; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-auto-"));
assert.ok(existsSync("web/dist/sake/sake.wasm"), "web/dist has no sake build");
const shots = process.env.SHOTS && resolve(process.env.SHOTS);
if (shots) mkdirSync(shots, { recursive: true });
let exe = process.argv[3];
if (!exe) {
  exe = join(work, "autoadvance.exe");
  execFileSync("i686-w64-mingw32-gcc", ["-O2", "host/tests/autoadvance.c", "-o", exe, "-mwindows"]);
}
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(game);
cpSync(exe, join(game, "autoadvance.exe"));
run("pack", game, "g");
run("add-game", "g", "--title", "自動送りの試験", "--tree", "g", "--exe", "autoadvance.exe", "--no-meta");

const port = 9100 + Math.floor(Math.random() * 200) * 2;
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

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(lines, re, ms = 120000) {
  const t = Date.now();
  while (!lines.some((l) => re.test(l))) {
    if (Date.now() - t > ms) throw new Error(`no line matching ${re}; last: ${lines.slice(-8).join(" | ")}`);
    await sleep(100);
  }
}
const prog = (lines) => lines.filter((l) => l.startsWith("auto "));
const count = (lines, re) => prog(lines).filter((l) => re.test(l)).length;

let failed = false;
try {
  const ctx = await chromium.launchPersistentContext(join(work, "pc"), { args: ["--enable-unsafe-webgpu"], viewport: { width: 1280, height: 720 } });
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  const lines = [];
  page.on("console", (m) => {
    lines.push(m.text());
    if (process.env.VERBOSE) console.log(m.text());
  });
  page.on("pageerror", (e) => lines.push(`pageerror: ${e.message}`));
  page.on("dialog", (d) => d.accept());
  await page.goto(`${base}play.html?game=g&headless&autostart`);
  await until(lines, /^auto ready$/);
  await sleep(500);

  // a real Ctrl: VK_CONTROL, both GetKeyState(VK_CONTROL) and (VK_LCONTROL) down; held, a repeat
  await page.keyboard.down("Control");
  await until(lines, /^auto key 17 /, 10000);
  await page.keyboard.up("Control");
  await until(lines, /^auto keyup 17 ctrl 0$/, 10000);
  assert.ok(prog(lines).includes("auto key 17 rep 0 ext 0 ctrl 1 lctrl 1 async 1"), prog(lines).join("\n"));
  assert.ok(!prog(lines).some((l) => /^auto key(up)? 16[2-5] /.test(l)), "a message named VK_LCONTROL");
  console.log("ok   a real Ctrl: WM_KEYDOWN VK_CONTROL, GetKeyState(VK_CONTROL) down");

  // the pointer on the game: クリック連打 clicks there
  await page.mouse.move(400, 300);
  await sleep(300);
  const dialog = page.getByRole("dialog");
  await page.getByRole("button", { name: "どこでもセーブ" }).click();
  const panel = dialog.getByRole("region", { name: "自動送り" });
  await panel.waitFor();
  await panel.getByRole("button", { name: "クリック連打" }).click();
  await panel.getByRole("button", { name: "1秒に20回" }).click();
  assert.equal(await panel.getByRole("button", { name: "1秒に20回" }).getAttribute("aria-pressed"), "true");
  if (shots) await page.screenshot({ path: join(shots, "dialog.png") });
  lines.length = 0;
  await panel.getByRole("button", { name: "自動送りを始める" }).click();
  await dialog.waitFor({ state: "hidden" });
  const stop = page.getByRole("button", { name: "自動送りを止める" });
  await stop.waitFor();
  if (shots) await page.screenshot({ path: join(shots, "running.png") });
  await sleep(2000);
  await stop.click();
  await stop.waitFor({ state: "hidden" });
  await sleep(300);
  const downs = count(lines, /^auto down /);
  const ups = count(lines, /^auto up /);
  assert.ok(downs >= 20 && downs <= 60, `${downs} clicks in 2 s at 20 a second`);
  assert.equal(ups, downs, "every down has its up");
  const places = new Set(prog(lines).filter((l) => /^auto (down|up) /.test(l)).map((l) => l.split(" ")[2]));
  assert.equal(places.size, 1, `the clicks went to ${[...places].join(" ")}`);
  const [x, y] = [...places][0].split(",").map(Number);
  assert.ok(x > 0 && y > 0, `clicked at ${x},${y}`);
  await sleep(1000);
  assert.equal(count(lines, /^auto down /), downs, "clicks after the stop");
  console.log(`ok   クリック連打: ${downs} clicks in 2 s at ${x},${y}, each let go, none after the stop`);

  // Ctrl押しっぱなし: held, repeating, polled down; let go when stopped
  await page.getByRole("button", { name: "どこでもセーブ" }).click();
  await panel.getByRole("button", { name: "Ctrl押しっぱなし" }).click();
  lines.length = 0;
  await panel.getByRole("button", { name: "自動送りを始める" }).click();
  await sleep(1500);
  const held = prog(lines);
  await stop.click();
  await until(lines, /^auto keyup 17 ctrl 0$/, 10000);
  await sleep(1200);
  assert.ok(held.includes("auto key 17 rep 0 ext 0 ctrl 1 lctrl 1 async 1"), held.join("\n"));
  const reps = held.filter((l) => l === "auto key 17 rep 1 ext 0 ctrl 1 lctrl 1 async 1").length;
  assert.ok(reps >= 10, `${reps} repeats in 1.5 s`);
  assert.ok(held.includes("auto poll ctrl 1"), "GetAsyncKeyState(VK_CONTROL) not down while held");
  const after = prog(lines).slice(prog(lines).indexOf("auto keyup 17 ctrl 0"));
  assert.ok(after.includes("auto poll ctrl 0") && !after.includes("auto poll ctrl 1"), after.join("\n"));
  assert.equal(count(lines, /^auto keyup 17 /), 1);
  console.log(`ok   Ctrl押しっぱなし: held (${reps} repeats in 1.5 s, polled down), let go when stopped`);

  // Enter連打, with a state taken while it runs
  await page.getByRole("button", { name: "どこでもセーブ" }).click();
  await panel.getByRole("button", { name: "Enter連打" }).click();
  await panel.getByRole("button", { name: "1秒に5回" }).click();
  lines.length = 0;
  await panel.getByRole("button", { name: "自動送りを始める" }).click();
  await sleep(800);
  await page.getByRole("button", { name: "どこでもセーブ" }).click();
  await dialog.getByRole("button", { name: "セーブ" }).first().click();
  await page.getByText("スロット 1 にセーブしました").waitFor({ timeout: 60000 });
  await sleep(1500);
  await stop.click();
  await sleep(500);
  const enters = count(lines, /^auto key 13 rep 0 /);
  assert.ok(enters >= 5, `${enters} Enters`);
  assert.equal(count(lines, /^auto keyup 13 /), enters);
  assert.ok(!lines.some((l) => /pageerror|panicked|unreachable/.test(l)), lines.filter((l) => /pageerror|panicked|unreachable/.test(l)).join("\n"));
  console.log(`ok   Enter連打: ${enters} presses, a state taken meanwhile`);

  // kept: the mode and the speed after a reload
  await page.reload();
  await until(lines, /^auto ready$/);
  await page.getByRole("button", { name: "どこでもセーブ" }).click();
  await panel.waitFor();
  assert.equal(await panel.getByRole("button", { name: "Enter連打" }).getAttribute("aria-pressed"), "true");
  assert.equal(await panel.getByRole("button", { name: "1秒に5回" }).getAttribute("aria-pressed"), "true");
  assert.equal(await page.getByRole("button", { name: "自動送りを止める" }).count(), 0, "running after a reload");
  console.log("ok   the mode and the speed kept across a reload, not running");
  await ctx.close();
} catch (e) {
  failed = true;
  console.error(`FAIL ${e.stack ?? e}`);
} finally {
  server.kill();
}
process.exit(failed ? 1 : 0);
