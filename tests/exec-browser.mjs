// A launcher's game, picked from the list, in Chromium against a real ippai serve: the page that
// takes over when the program starts another in its place (sake's k32::exec, play.ts closeGame)
// keeps the game's mounts, environment and saves, also at the second step. host/tests/execchain.c
// built as Key's chain (anemoi: Start.exe -> StartData\StartMenu.exe /from="autorun" ->
// GameData\SiglusEngine.exe /window, each by ShellExecuteExW). The list puts only ?game= in the
// address bar; the next page was made from that and so started without the game's files
// ("StartMenu.exe: No such file or directory").
//
//   node tests/exec-browser.mjs [path to ippai]
//   (web/dist built with sake in it: crates/sake/build.sh, then pnpm build; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-exec-"));
assert.ok(existsSync("web/dist/sake/sake.wasm"), "web/dist has no sake build");
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(join(game, "StartData/GameData"), { recursive: true });
for (const [stage, out] of [
  [0, "Start.exe"],
  [1, "StartData/StartMenu.exe"],
  [2, "StartData/GameData/SiglusEngine.exe"],
]) {
  execFileSync("i686-w64-mingw32-gcc", ["-O2", `-DSTAGE=${stage}`, "host/tests/execchain.c", "-o", join(game, out), "-lshell32", "-lgdi32"]);
}
writeFileSync(join(game, "StartData/Start.ini"), '#GAME_TITLE = "chain"\r\n');
// anemoi's Start.bmp, small: 24 bits, bottom-up, biSizeImage 2 bytes more than the bits
{
  const [w, h] = [40, 30];
  const bmp = Buffer.alloc(54 + w * 3 * h);
  bmp.write("BM", 0, "latin1");
  bmp.writeUInt32LE(bmp.length, 2);
  bmp.writeUInt32LE(54, 10);
  bmp.writeUInt32LE(40, 14);
  bmp.writeInt32LE(w, 18);
  bmp.writeInt32LE(h, 22);
  bmp.writeUInt16LE(1, 26);
  bmp.writeUInt16LE(24, 28);
  bmp.writeUInt32LE(w * 3 * h + 2, 34);
  writeFileSync(join(game, "StartData/Start.bmp"), bmp);
}
writeFileSync(join(game, "StartData/GameData/Gameexe.dat"), "data");
run("pack", game, "chain");
run("add-game", "chain", "--title", "ランチャーの試験", "--tree", "chain", "--exe", "Start.exe", "--no-meta");

const port = 8900 + Math.floor(Math.random() * 200) * 2;
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
    lines.push(m.text());
    if (process.env.VERBOSE) console.log(m.text());
  });
  page.on("pageerror", (e) => lines.push(`pageerror: ${e.message}`));
  const urls = [];
  page.on("framenavigated", (f) => f === page.mainFrame() && urls.push(f.url()));
  // the list, as a player opens it: the game from its card
  await page.goto(`${base}play.html?headless`);
  await page.locator('[data-game="chain"]').getByRole("button").first().click();
  await until(/^chain 0: start$/);
  // each program after a launcher starts by itself: the player already pressed 起動 once (a
  // phone user who saw the button again thought the game never started)
  for (const step of [1, 2]) {
    await page.waitForURL((u) => u.searchParams.get("run")?.endsWith(step === 1 ? "StartMenu.exe" : "SiglusEngine.exe"), { timeout: 60000 });
    const u = new URL(page.url());
    assert.deepEqual(u.searchParams.getAll("mount"), ["/ippai/game=chain"], `step ${step}: ${u}`);
    assert.equal(u.searchParams.get("game"), "chain");
    assert.ok(u.searchParams.get("saves"), `step ${step}: the launcher's saves`);
    assert.ok(u.searchParams.has("autostart"), `step ${step}: starts without a click: ${u}`);
    await until(new RegExp(`^chain ${step}:`));
  }
  await until(/^end$/);
  const got = lines.filter((l) => /^chain|^end$/.test(l));
  assert.deepEqual(got, [
    "chain 0: start",
    'chain 1: args [/from="autorun"] ini [#GAME_TITLE = "chain"] bmp 40x30 24 bits',
    "chain 2: args [/window] gamedata yes",
    "end",
  ]);
  assert.ok(!lines.some((l) => /pageerror|No such file|panicked/.test(l)), lines.filter((l) => /pageerror|No such file|panicked/.test(l)).join("\n"));
  console.log(`ok   a launcher's chain from the list keeps its game's files (${urls.length} pages)`);
} catch (e) {
  failed = true;
  console.error(`FAIL ${e.stack ?? e}`);
  console.error(lines.slice(-30).join("\n"));
} finally {
  await browser.close();
  server.kill();
}
process.exit(failed ? 1 : 0);
