// The browser's back button on masu's pages (web/src/play.ts backToList): a game picked from the
// list, or opened at its own address (a bookmark, a link), goes back to the game list, and the
// list is under the game only once (one more back leaves masu). The list's 一覧に戻る button
// (after a game that could not start: the test's games are not real programs) goes back to the
// same entry instead of adding a second list over it. Forward comes to the game again.
//
//   node tests/back-browser.mjs [path to masu]   (web/dist built; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/masu");
const work = mkdtempSync(join(tmpdir(), "masu-back-"));
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const TITLES = { sakura: "さくらの詩", tsuki: "月の光" };
for (const [id, title] of Object.entries(TITLES)) {
  const dir = join(work, id);
  mkdirSync(dir);
  writeFileSync(join(dir, "game.exe"), `MZ ${id}`);
  run("pack", dir, id);
  run("add-game", id, "--title", title, "--tree", id, "--exe", "game.exe", "--no-meta");
}

const port = 9300 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit" },
);
process.on("exit", () => server.kill());
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(base).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}
// a page before masu's: one back too many lands here
const before = `${base}manifest.webmanifest`;

const browser = await chromium.launch();
try {
  const page = await browser.newPage();
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  const game = (u) => new URL(u).searchParams.get("game");
  const atList = async () => {
    await page.waitForURL((u) => !u.searchParams.has("game"));
    await page.locator("[data-game]").first().waitFor();
    assert.equal(await page.locator("[data-game]").count(), 2, "the list's games");
  };
  const pick = async (title) => {
    await page.getByRole("button", { name: `${title} を起動` }).click();
    // (a game not saved to the device: the dialog that saves it first, web/src/ui/SaveFirst.tsx;
    // these games have no prefix tree to save, tests/save-first-browser.mjs saves real ones)
    const first = page.getByTestId("save-first");
    await Promise.race([first.waitFor(), page.waitForURL((u) => u.searchParams.get("game") !== null)]);
    if (await first.count()) await first.getByRole("button", { name: "保存せずに遊ぶ" }).click();
    await page.waitForURL((u) => u.searchParams.get("game") !== null);
  };

  // 1. picked from the list: back → the list; once more → the page before
  await page.goto(before);
  await page.goto(base);
  await atList();
  await pick("月の光");
  assert.equal(game(page.url()), "tsuki");
  await page.goBack();
  await atList();
  // forward: the game's start screen again
  await page.goForward();
  await page.waitForURL((u) => u.searchParams.get("game") === "tsuki");
  await page.getByRole("button", { name: "月の光 を起動" }).waitFor();
  await page.goBack();
  await atList();
  await page.goBack();
  await page.waitForURL(before);

  // 2. opened at its own address: back → the list (put under it), then the page before
  await page.goto(before);
  await page.goto(`${base}?game=sakura`);
  await page.getByRole("button", { name: "さくらの詩 を起動" }).waitFor();
  // a reload keeps one list under it, not two
  await page.reload();
  await page.getByRole("button", { name: "さくらの詩 を起動" }).waitFor();
  await page.goBack();
  await atList();
  await page.goBack();
  await page.waitForURL(before);

  // 3. 一覧に戻る after a game that did not start: the list's own entry, no second one
  await page.goto(before);
  await page.goto(base);
  await atList();
  await pick("さくらの詩");
  const backButton = page.getByRole("button", { name: /一覧に戻る/ });
  await backButton.waitFor({ timeout: 60_000 });
  await backButton.click();
  await atList();
  await page.goBack();
  await page.waitForURL(before);

  // 4. back while the game is still loading (no 一覧に戻る yet)
  await page.goto(before);
  await page.goto(base);
  await atList();
  await pick("月の光");
  await page.goBack();
  await atList();
  assert.equal(await page.getByRole("button", { name: /一覧に戻る/ }).count(), 0);

  assert.deepEqual(errors, [], "page errors");
  console.log("back-browser: ok");
} finally {
  await browser.close();
  server.kill();
}
