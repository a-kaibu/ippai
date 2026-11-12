// Save states in the page (web/src/savestate.ts, ui/States.tsx, docs/savestate.md): starts ippai
// serve on a small store with host/tests/snaptest.c as a game and, in Chromium, takes a state
// from the dialog while it runs, then loads it: the page starts again from the state and the
// program prints the rest of its lines, the same as a whole run (its steps and checksums go on
// from where the state was). Then a browser with nothing of its own (another device) goes on
// from the server's copy of the state. Then again on sake-grow.wasm (the build of a device that
// can't reserve 4 GB) with its memory grown before the state: loaded without asking for that
// build, the page takes the state's. And host/tests/snapmsaa.c: Direct3D 11 4x targets drawn
// once, read back sample by sample every step: the same bits after the state.
//
//   node tests/savestate-browser.mjs [path to ippai] [snaptest.exe]
//   (web/dist built with sake in it: crates/sake/build.sh, then pnpm build; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-state-"));
// Optional real upgrade: take states in OLD_WEB, serve NEW_WEB on the same store,
// then open each state from the new root page (also from a fresh browser profile).
const oldWeb = resolve(process.env.OLD_WEB ?? "web/dist");
const newWeb = resolve(process.env.NEW_WEB ?? oldWeb);
const upgrade = oldWeb !== newWeb;
for (const web of [oldWeb, newWeb]) assert.ok(existsSync(join(web, "sake/sake.wasm")), `${web} has no sake build`);
if (upgrade) {
  for (const runtime of ["sake", "sake-grow"]) {
    const id = (web) => readFileSync(join(web, "sake/build-id.txt"), "utf8").split("\n").find((l) => l.startsWith(`${runtime} `));
    assert.ok(id(oldWeb) && id(newWeb) && id(oldWeb) !== id(newWeb), `${runtime}: need two distinct real builds`);
    assert.notDeepEqual(readFileSync(join(oldWeb, `sake/${runtime}.wasm`)), readFileSync(join(newWeb, `sake/${runtime}.wasm`)), `${runtime}: identical Wasm`);
  }
}
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
const msaa = join(work, "msaa");
mkdirSync(msaa);
execFileSync("i686-w64-mingw32-gcc", ["-O2", "host/tests/snapmsaa.c", "-o", join(msaa, "snapmsaa.exe"), "-ld3d11", "-ld3dcompiler_47"]);
run("pack", msaa, "m");
run("add-game", "m", "--title", "ステートの試験 4x", "--tree", "m", "--exe", "snapmsaa.exe", "--no-meta");

const port = 8900 + Math.floor(Math.random() * 200) * 2;
const base = `http://127.0.0.1:${port}/`;
let server;
async function stopServer() {
  if (!server || server.exitCode !== null) return;
  const stopped = new Promise((r) => server.once("exit", r));
  server.kill();
  await stopped;
}
async function serve(web) {
  await stopServer();
  server = spawn(bin, ["serve", "--foreground", "--web", web, "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: "inherit" });
  for (let i = 0; i < 100; i++) {
    if (server.exitCode !== null) throw new Error(`server exited: ${server.exitCode}`);
    if (await fetch(`${base}apps/builds`).then((r) => r.ok, () => false)) return;
    await new Promise((r) => setTimeout(r, 300));
  }
  throw new Error("server did not become ready");
}
const contexts = new Set();
const bytes = (dir) => readdirSync(dir, { withFileTypes: true }).reduce((n, e) => n + (e.isDirectory() ? bytes(join(dir, e.name)) : statSync(join(dir, e.name)).size), 0);

/** A browser of its own (a device): its page and the program's lines. */
async function device(name) {
  const ctx = await chromium.launchPersistentContext(join(work, name), { args: ["--enable-unsafe-webgpu"], viewport: { width: 1280, height: 720 } });
  contexts.add(ctx);
  await ctx.addInitScript(() => { globalThis.__ippaiStateTestDocument = crypto.randomUUID(); });
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  if (process.env.VERBOSE) {
    page.on("framenavigated", (frame) => { if (frame === page.mainFrame()) console.log(`[${name}] navigation ${frame.url()}`); });
    page.on("response", (r) => { if (/build-id|apps\/builds/.test(r.url())) console.log(`[${name}] response ${r.status()} ${r.url()}`); });
  }
  const lines = [];
  page.on("console", (m) => {
    lines.push(m.text());
    if (process.env.VERBOSE) console.log(`[${name}] ${m.text()}`);
  });
  page.on("pageerror", (e) => lines.push(`pageerror: ${e.message}`));
  // (the slot may be the server's already: written over)
  page.on("dialog", (d) => d.accept());
  return { ctx, page, lines };
}
const program = (lines) => lines.filter((l) => /^(step \d+|late thread|clocks|sleeper|changed at|samples differ|end)/.test(l));
async function until(lines, re, ms = 120000) {
  const t = Date.now();
  while (!lines.some((l) => re.test(l))) {
    if (Date.now() - t > ms) throw new Error(`no line matching ${re}; last: ${lines.slice(-8).join(" | ")}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}
/** The whole round: `start`, the page's extra parameters for the run the state is taken in;
 * `whole` checks the whole run's lines. */
async function round(tag, start, game = "g", wholeOk = (w) => w.includes("clocks: ok")) {
  await serve(oldWeb);
  const play = `${base}play.html?game=${game}&headless&autostart`;
  // the whole run, a state taken in the middle (from the dialog)
  const a = await device(`a${tag}`);
  await a.page.goto(`${play}${start}`);
  await until(a.lines, /^step 15:/);
  const runDocument = await a.page.evaluate(() => globalThis.__ippaiStateTestDocument);
  await a.page.getByRole("button", { name: "どこでもセーブ" }).click();
  await a.page.getByRole("dialog").getByRole("button", { name: "セーブ" }).first().click();
  await until(a.lines, /^state: .* stood still \d+ ms/, 60000);
  await a.page.getByText("スロット 1 にセーブしました").waitFor({ timeout: 60000 });
  await until(a.lines, /^end$/);
  const whole = program(a.lines);
  assert.equal(whole.at(-1), "end");
  assert.ok(wholeOk(whole), whole.join("\n"));
  // (and on the server)
  await a.page.waitForFunction(() => !document.body.textContent?.includes("サーバーに送信中"), null, { timeout: 120000 });

  // `end` is guest output, before exit's asynchronous save and navigation finish.
  // Do not let the previous run's leave() interrupt the restoration navigation.
  // history.back changes the URL before leave() reloads the page. Wait for a
  // genuinely new document, or that pending reload can interrupt our goto below.
  await a.page.waitForFunction((old) => globalThis.__ippaiStateTestDocument !== old && !new URL(location.href).searchParams.has("game"), runDocument, { timeout: 120000 });
  await a.page.waitForLoadState("load");
  if (upgrade) await serve(newWeb);
  const builds = await (await fetch(`${base}apps/builds`)).json();
  const runtime = start.includes("memory=grow") ? "sake-grow" : "sake";
  const oldId = readFileSync(join(oldWeb, "sake/build-id.txt"), "utf8").split("\n").find((l) => l.startsWith(`${runtime} `)).trim().split(/\s+/)[1];
  const oldPath = `/apps/${builds[oldId]}/`;
  assert.ok(builds[oldId], `old build ${oldId} was not kept`);
  const watch = (page) => {
    const requests = [];
    page.on("request", (r) => requests.push(new URL(r.url()).pathname));
    return requests;
  };
  const checkApp = async (page, requests) => {
    if (!upgrade) return;
    assert.ok(requests.includes(`${oldPath}sake/${runtime}.wasm`), `did not execute retained ${runtime}: ${requests.filter((p) => p.endsWith(".wasm"))}`);
    await page.waitForURL((u) => u.pathname === "/play.html" && !u.searchParams.has("game"), { timeout: 30000 });
    assert.equal(new URL(page.url()).searchParams.has("state"), false);
  };

  // loaded: the page starts again from it, the rest of the lines follow
  const check = (lines, who) => {
    const got = program(lines);
    assert.ok(got.length > 0 && got.length < whole.length, `${who}: ${got.length} lines of ${whole.length}`);
    assert.deepEqual(got, whole.slice(whole.length - got.length), `${who}: not the rest of the whole run`);
    assert.ok(!lines.some((l) => /pageerror|panicked|unreachable/.test(l)), `${who}: ${lines.filter((l) => /pageerror|panicked|unreachable/.test(l)).join("\n")}`);
  };
  const ar = watch(a.page);
  a.lines.length = 0;
  // (the program ended and the page went back to the list: as the dialog's ロード does)
  await a.page.goto(`${play}&state=0`);
  await until(a.lines, /^end$/);
  check(a.lines, "loaded");
  await checkApp(a.page, ar);

  // another device: nothing of its own, the server's state
  const b = await device(`b${tag}`);
  const br = watch(b.page);
  await b.page.goto(`${play}&state=0`);
  await until(b.lines, /^end$/);
  check(b.lines, "another device");
  await checkApp(b.page, br);
  console.log(`ok   save states in the page${tag}: taken, loaded, loaded on another device`);
  await a.ctx.close();
  await b.ctx.close();
  contexts.delete(a.ctx);
  contexts.delete(b.ctx);
  if (upgrade) console.log(`kept apps: ${bytes(join(store, "apps"))} bytes (${tag || "fixed"})`);
}

let failed = false;
try {
  await round("", "");
  // (&grow: 512 MiB of sake's memory taken at once, so that it grows mid-run)
  await round(" (sake-grow, grown)", "&memory=grow&grow=512");
  // (4x targets: every sample of each, bit for bit, and the samples of edge pixels differ)
  await round(" (Direct3D 11 4x targets)", "", "m", (w) => w.includes("samples differ: all") && !w.some((l) => l.startsWith("changed at")));
} catch (e) {
  failed = true;
  console.error(`FAIL ${e.stack ?? e}`);
} finally {
  await Promise.all([...contexts].map((ctx) => ctx.close()));
  await stopServer();
}
process.exit(failed ? 1 : 0);
