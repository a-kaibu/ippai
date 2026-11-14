// What is on the device and what is being saved there (web/src/ui/Storage.tsx), in Chromium on
// masu serve with a small store: a game with folders (bgm, g00, koe and files outside them) is
// saved from its 詳細 (端末に保存) with every chunk held back a while (the bucket's 1-2 s, cut
// short): while it goes, 詳細 shows the bytes done of all and the file being fetched, the sidebar's
// 保存 tab lists the game with the same progress; its 内訳 table has a row per folder (and the
// prefix) with the folder's size from the trees' lists; at the end both say saved, every row at
// 100%, and the 保存 tab's game data is the game's size at least. SHOTS=<dir> keeps screenshots.
//
//   node tests/storage-browser.mjs [path to masu]   (web/dist built; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { cpSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/masu");
const work = mkdtempSync(join(tmpdir(), "masu-storage-"));
const web = join(work, "web");
cpSync(resolve("web/dist"), web, { recursive: true });
if (!existsSync(join(web, "sake/sake.wasm"))) {
  mkdirSync(join(web, "sake"), { recursive: true });
  writeFileSync(join(web, "sake/sake.js"), "export default () => {};\n");
  writeFileSync(join(web, "sake/sake.wasm"), new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]));
  writeFileSync(join(web, "sake/jit-version.txt"), "00000000\n");
}
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const shots = process.env.SHOTS && resolve(process.env.SHOTS);
if (shots) mkdirSync(shots, { recursive: true });

const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "Fonts"), { recursive: true });
writeFileSync(join(prefix, "Fonts/font.ttf"), noise(200 << 10, 1));
run("pack", join(work, "prefix"), "prefix32");

/** `n` bytes that do not compress (an LCG from `seed`). */
function noise(n, seed) {
  let x = seed >>> 0;
  return Uint8Array.from({ length: n }, () => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 16) & 255);
}
// folders of different sizes (KiB): what the 内訳 lists, the biggest first
const game = join(work, "game");
const FOLDERS = { koe: [900, 700, 500], g00: [300, 200, 100, 50], bgm: [400] };
let seed = 10;
for (const [d, files] of Object.entries(FOLDERS)) {
  mkdirSync(join(game, d), { recursive: true });
  files.forEach((k, i) => writeFileSync(join(game, d, `${d}${i}.bin`), noise(k << 10, seed++)));
}
writeFileSync(join(game, "game.exe"), "MZ not really");
writeFileSync(join(game, "Gameexe.dat"), noise(20 << 10, 99));
// (small chunks: many requests, each held back below)
run("pack", game, "g", "--chunk-kib", "64");
run("add-game", "g", "--title", "保存のゲーム", "--tree", "g", "--exe", "game.exe", "--no-meta");

const port = 8900 + Math.floor(Math.random() * 200) * 2;
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
const browser = await chromium.launchPersistentContext(join(work, "profile"), {
  args: ["--enable-unsafe-webgpu"],
  viewport: { width: 1280, height: 800 },
  serviceWorkers: "block",
});
try {
  const page = browser.pages()[0] ?? (await browser.newPage());
  // every chunk held back 300 ms (the bucket's wait, shortened): the save is seen going
  await page.route(/\/store\/chunks\//, async (route) => {
    await new Promise((r) => setTimeout(r, 300));
    await route.continue();
  });
  await page.goto(`${base}play.html`);
  await page.getByText("保存のゲーム").first().waitFor();
  await page.getByRole("button", { name: "保存のゲーム の情報" }).click();
  const side = page.getByRole("dialog");
  await side.getByRole("button", { name: "端末に保存" }).click();
  // while it goes: bytes done of all, the file being fetched (one of the game's)
  await side.getByText(/^取得中: /).waitFor({ timeout: 30000 });
  const now = await side.getByText(/^取得中: /).first().textContent();
  assert.match(now, /^取得中: (koe|g00|bgm)\/\w+\.bin$|^取得中: (Gameexe\.dat|game\.exe)$|^取得中: C:\\windows\/Fonts\/font\.ttf$/, now);
  assert.match(await side.getByRole("progressbar", { name: "保存の進み" }).getAttribute("aria-valuenow"), /^\d+$/);
  if (shots) await page.screenshot({ path: join(shots, "info-saving.png") });
  // the 保存 tab: the game with its progress, and its parts
  await side.getByRole("tab", { name: "保存" }).click();
  const list = side.getByRole("list", { name: "端末に保存した作品" });
  await list.getByText("保存のゲーム").waitFor();
  await list.getByRole("progressbar", { name: "保存の進み" }).waitFor();
  await list.getByText("内訳", { exact: false }).click();
  const rows = list.getByRole("table", { name: "保存するデータの内訳" }).locator("tbody tr");
  await rows.first().waitFor();
  const names = await rows.locator("td:first-child").allTextContents();
  assert.deepEqual(names.slice(0, 3), ["koe", "g00", "bgm"], `the biggest first: ${names}`);
  assert.ok(names.includes("システム（フォントなど）") && names.includes("（フォルダの外のファイル）"), String(names));
  const koe = (await rows.nth(0).locator("td").allTextContents()).map((t) => t.trim());
  assert.equal(koe[1], "3", "koe's files");
  // (its size: 2100 KiB of data, in chunks of its own)
  assert.equal(koe[2], "2 MB");
  if (shots) await page.screenshot({ path: join(shots, "storage-saving.png") });
  // the end: saved, every part on the device
  await list.getByText(/（保存済み）/).waitFor({ timeout: 120000 });
  await list.getByRole("progressbar", { name: "保存の進み" }).waitFor({ state: "detached" });
  const done = await list.getByRole("table", { name: "保存するデータの内訳" }).locator("tbody tr td:last-child").allTextContents();
  assert.ok(done.length >= 5 && done.every((t) => t.trim() === "100%"), `parts not all on the device: ${done}`);
  const data = await side.getByText(/^ゲームのデータ: /).textContent();
  const mbOf = Number(/ゲームのデータ: ([\d.]+) MB/.exec(data)?.[1]);
  assert.ok(mbOf >= 3, data);
  if (shots) await page.screenshot({ path: join(shots, "storage-done.png") });
  await side.getByRole("tab", { name: "詳細" }).click();
  await side.getByText(/端末に保存済み/).waitFor();
  console.log("storage browser: progress, file being fetched, parts and totals passed");
} finally {
  await browser.close();
  server.kill();
  rmSync(work, { recursive: true, force: true });
}
