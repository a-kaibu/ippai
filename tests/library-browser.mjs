// The game list (web/src/ui/Library.tsx) in Chromium, at a desktop and a phone size, on ippai
// serve with a small store: the app's name and 🍶 icons everywhere (no "vnvm" left on screen,
// favicon/manifest icons are PNGs that load), no debug switch and no log page, 統計 at the top
// right (and its dialog adds up the play time the server recorded), the game last played offered
// to continue, search, and a favorite set from a game's dialog showing on its card and under the
// お気に入り tab after a reload. SHOTS=<dir> keeps screenshots.
//
//   node tests/library-browser.mjs [path to ippai]   (web/dist built; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-library-"));
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const shots = process.env.SHOTS && resolve(process.env.SHOTS);
if (shots) mkdirSync(shots, { recursive: true });

const TITLES = { sakura: "さくらの詩", tsuki: "月の光", hoshi: "星のかけら" };
for (const [id, title] of Object.entries(TITLES)) {
  const dir = join(work, id);
  mkdirSync(dir);
  writeFileSync(join(dir, "game.exe"), `MZ ${id}`);
  run("pack", dir, id);
  run("add-game", id, "--title", title, "--tree", id, "--exe", "game.exe", "--no-meta");
}

const port = 8900 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit" },
);
// (an assertion failing before the browser's try still stops the server)
process.on("exit", () => server.kill());
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(base).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}
// play time the server recorded: 90 min of 月の光 in two sessions, 5 min of さくらの詩
const put = (s) =>
  fetch(`${base}api/sessions`, { method: "PUT", headers: { "content-type": "application/json" }, body: JSON.stringify(s) }).then((r) =>
    assert.ok(r.ok, `PUT session: HTTP ${r.status}`),
  );
await put({ id: crypto.randomUUID(), game: "tsuki", started_ms: Date.parse("2026-11-01T10:00:00Z"), elapsed_ms: 60 * 60_000 });
await put({ id: crypto.randomUUID(), game: "tsuki", started_ms: Date.parse("2026-11-02T10:00:00Z"), elapsed_ms: 30 * 60_000 });
await put({ id: crypto.randomUUID(), game: "sakura", started_ms: Date.parse("2026-11-03T10:00:00Z"), elapsed_ms: 5 * 60_000 });

// the files the pages name: the icons are PNGs and load; the log page is gone
const html = await (await fetch(base)).text();
assert.match(html, /<title>ippai<\/title>/);
assert.doesNotMatch(html, /vnvm|icon\.svg/i);
const manifest = await (await fetch(`${base}manifest.webmanifest`)).json();
assert.equal(manifest.name, "ippai");
const icons = [...html.matchAll(/<link rel="(?:icon|apple-touch-icon)" href="\.\/([^"]+)"/g)].map((m) => m[1]);
assert.ok(icons.includes("icons/favicon-48.png"), `favicon: ${icons}`);
for (const src of [...icons, ...manifest.icons.map((i) => i.src)]) {
  const r = await fetch(new URL(src, base));
  assert.equal(r.status, 200, src);
  const png = new Uint8Array(await r.arrayBuffer());
  assert.deepEqual([...png.slice(1, 4)], [0x50, 0x4e, 0x47], `${src} is a PNG`);
  // (the size the manifest says: IHDR width)
  const size = manifest.icons.find((i) => i.src === src)?.sizes;
  if (size) assert.equal(`${new DataView(png.buffer).getUint32(16)}x${new DataView(png.buffer).getUint32(20)}`, size, src);
}
assert.ok(manifest.icons.some((i) => i.purpose === "maskable"));
assert.equal((await fetch(`${base}logs.html`)).status, 404, "logs.html is gone");

const browser = await chromium.launch();
const failures = [];
try {
  for (const view of [
    { name: "desktop", width: 1280, height: 800 },
    { name: "phone", width: 390, height: 844, isMobile: true, hasTouch: true },
  ]) {
    const ctx = await browser.newContext({ viewport: { width: view.width, height: view.height }, isMobile: view.isMobile, hasTouch: view.hasTouch });
    const page = await ctx.newPage();
    page.on("pageerror", (e) => failures.push(`${view.name}: ${e.message}`));
    // 星のかけら last played in this browser
    await page.addInitScript(() => localStorage.setItem("vnvm-played", JSON.stringify({ hoshi: Date.parse("2026-11-06T21:30:00") })));
    await page.goto(base);
    await page.getByRole("heading", { name: "ippai", level: 1 }).waitFor();
    const shot = (what) => shots && page.screenshot({ path: join(shots, `${view.name}-${what}.png`) });
    await page.locator("[data-game]").first().waitFor();
    await shot("list");

    // the name and the mark
    assert.equal(await page.title(), "ippai");
    const text = await page.locator("body").innerText();
    assert.doesNotMatch(text, /vnvm/i, `${view.name}: "vnvm" on screen`);
    const logo = page.locator("header img").first();
    assert.equal(await logo.evaluate((i) => i.complete && i.naturalWidth), 192, "the header's 🍶 icon loads");

    // no debug switch, no log page link
    assert.equal(await page.getByRole("switch").count(), 0, "debug switch");
    assert.doesNotMatch(text, /デバッグ/);
    assert.equal(await page.locator('a[href*="logs"]').count(), 0, "log page link");
    assert.equal(await page.getByRole("link", { name: /ログ/ }).count(), 0);

    // 統計 at the top right: the last control in the header, by the right edge
    const stats = page.getByRole("button", { name: "統計" });
    const box = await stats.boundingBox();
    assert.ok(box, "統計 button shown");
    assert.ok(box.y + box.height <= 64, `${view.name}: 統計 at the top (y ${box.y})`);
    assert.ok(view.width - (box.x + box.width) <= 24, `${view.name}: 統計 at the right (x ${box.x}+${box.width} of ${view.width})`);
    const rightmost = await page.locator("header button, header a, header input").evaluateAll((els) =>
      els.map((e) => ({ label: e.getAttribute("aria-label") ?? e.textContent?.trim(), right: e.getBoundingClientRect().right })).sort((a, b) => b.right - a.right)[0].label,
    );
    assert.equal(rightmost, "統計");
    // no horizontal scrolling
    assert.ok(await page.evaluate(() => document.querySelector("main").scrollWidth <= innerWidth), `${view.name}: scrolls sideways`);

    // the statistics: total, games, sessions, most played first
    await stats.click();
    const dialog = page.getByRole("dialog", { name: "プレイ統計" });
    await dialog.getByRole("list", { name: "ゲーム別のプレイ時間" }).waitFor();
    const dtext = await dialog.innerText();
    assert.match(dtext, /1時間 35分/, dtext);
    assert.match(dtext, /2 本/);
    assert.match(dtext, /3 回/);
    const rows = await dialog.getByRole("listitem").allInnerTexts();
    assert.match(rows[0], /月の光[\s\S]*1時間 30分/);
    assert.match(rows[1], /さくらの詩[\s\S]*5分/);
    await shot("stats");
    await page.keyboard.press("Escape");
    await dialog.waitFor({ state: "detached" });

    // the game last played, to continue
    const cont = page.getByRole("region", { name: "続きから遊ぶ" });
    assert.match(await cont.innerText(), /星のかけら/);
    assert.equal(await cont.getByRole("button", { name: /つづきから遊ぶ/ }).count(), 1);

    // search: words narrow the list; a miss offers to clear it
    const search = page.getByRole("searchbox");
    await search.fill("月");
    assert.deepEqual(await page.locator("[data-game]").evaluateAll((l) => l.map((e) => e.dataset.game)), ["tsuki"]);
    assert.equal(await cont.count(), 0, "no continue banner while searching");
    await search.fill("ありえない");
    await page.getByRole("button", { name: /検索をクリア/ }).click();
    assert.equal(await page.locator("[data-game]").count(), 3);

    if (view.name === "desktop") {
      // a favorite from the game's dialog: on its card at once, under お気に入り, and after a reload
      await page.getByRole("button", { name: "月の光 の情報" }).click();
      const info = page.getByRole("dialog", { name: "月の光" });
      const fav = info.getByRole("button", { name: /お気に入りに追加/ });
      await fav.click();
      await info.locator('button[aria-pressed="true"]').waitFor();
      await shot("info");
      await page.keyboard.press("Escape");
      await page.locator('[data-game="tsuki"] [title="お気に入り"]').waitFor();
      await page.reload();
      await page.getByRole("tab", { name: /お気に入り/ }).click();
      await page.waitForFunction(() => document.querySelectorAll("[data-game]").length === 1);
      assert.deepEqual(await page.locator("[data-game]").evaluateAll((l) => l.map((e) => e.dataset.game)), ["tsuki"]);
      assert.match(await page.getByRole("tab", { name: /お気に入り/ }).innerText(), /1/);
      // the note: saved on 保存 only
      await page.getByRole("button", { name: "月の光 の情報" }).click();
      const note = page.getByRole("dialog", { name: "月の光" }).getByRole("textbox", { name: "メモ" });
      const save = page.getByRole("button", { name: /メモを保存/ });
      assert.ok(await save.isDisabled(), "nothing to save yet");
      await note.fill("第2章まで");
      await save.click();
      await page.getByText("メモを保存しました").waitFor();
      const saved = await (await fetch(`${base}api/library/tsuki`)).json();
      assert.deepEqual(saved, { favorite: true, note: "第2章まで" });
    }
    await ctx.close();
  }
  assert.deepEqual(failures, []);
  console.log("library-browser: ok");
} finally {
  await browser.close();
  server.kill();
}
