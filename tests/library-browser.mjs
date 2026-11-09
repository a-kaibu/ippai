// The game list (web/src/ui/Library.tsx) in Chromium, at a desktop and a phone size, on ippai
// serve with a small store: the app's name and 🍶 icons everywhere (no "vnvm" left on screen,
// favicon/manifest icons are PNGs that load), no debug switch and no log page, the sidebar's button at the top
// right opening the sidebar (not modal: the list beside it stays in use; tabs 詳細 / 統計 / 設定,
// a game's (i) opening 詳細) with the server's version
// and its update (a stand-in release page: 9.9.9 offered; a broken download says why; the
// update itself with its restart stood in by the page's routes), the app install, the play
// statistics the server recorded (reset after a confirmation) and the DMM account; the list in
// order of score / release and in sections by brand / year (kept across a reload), narrowed to a
// brand by clicking its name, the game last
// played offered to continue, search, and a favorite set from a game's dialog showing on its
// card and under the お気に入り tab after a reload. SHOTS=<dir> keeps screenshots.
//
//   node tests/library-browser.mjs [path to ippai]   (web/dist built; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
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
// brand, release and score as `ippai meta` would put them
const META = {
  sakura: { brand: "Key", release: "2004-06-25", score: 80 },
  tsuki: { brand: "Key", release: "2011-09-30", score: 90 },
  hoshi: { brand: "august", release: "2008-03-28" },
};
for (const [id, m] of Object.entries(META)) {
  const file = join(store, "games", `${id}.json`);
  const entry = JSON.parse(readFileSync(file, "utf8"));
  writeFileSync(file, JSON.stringify({ ...entry, meta: { source: "egs", title: TITLES[id], ...m } }));
}

// a stand-in for GitHub's releases page: the latest is 9.9.9, its download is not an archive
const releases = createServer((req, res) => {
  if (req.url === "/r/latest") res.writeHead(302, { location: "/r/tag/v9.9.9" }).end();
  else if (req.url === "/r/tag/v9.9.9") res.end("release page");
  else res.writeHead(req.url?.startsWith("/r/download/") ? 200 : 404).end("not a tarball");
});
await new Promise((r) => releases.listen(0, "127.0.0.1", r));
process.on("exit", () => releases.close());

const port = 8900 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit", env: { ...process.env, IPPAI_UPDATE_URL: `http://127.0.0.1:${releases.address().port}/r`, IPPAI_UPDATE_API: `http://127.0.0.1:${releases.address().port}/api` } },
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
    // (the phone's update stands in the server's answers by the page's routes, which do not see
    // what the service worker fetches: no worker there)
    const ctx = await browser.newContext({ viewport: { width: view.width, height: view.height }, isMobile: view.isMobile, hasTouch: view.hasTouch, serviceWorkers: view.name === "phone" ? "block" : "allow" });
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

    // 設定 at the top right, the last control in the header; 統計 and アプリにする moved into it
    // (the sidebar's button: opens it on the tab last shown, 設定 at first; closes it)
    const settingsButton = page.getByRole("button", { name: "サイドバー", exact: true });
    const box = await settingsButton.boundingBox();
    assert.ok(box, "sidebar button shown");
    assert.ok(box.y + box.height <= 64, `${view.name}: the sidebar button at the top (y ${box.y})`);
    assert.ok(view.width - (box.x + box.width) <= 24, `${view.name}: the sidebar button at the right (x ${box.x}+${box.width} of ${view.width})`);
    const rightmost = await page.locator("header button, header a, header input").evaluateAll((els) =>
      els.map((e) => ({ label: e.getAttribute("aria-label") ?? e.textContent?.trim(), right: e.getBoundingClientRect().right })).sort((a, b) => b.right - a.right)[0].label,
    );
    assert.equal(rightmost, "サイドバー");
    assert.equal(await settingsButton.getAttribute("aria-expanded"), "false");
    assert.equal(await page.locator("header").getByRole("button", { name: /統計|アプリにする/ }).count(), 0, "統計 / アプリにする left the header");
    // no horizontal scrolling
    assert.ok(await page.evaluate(() => document.querySelector("main").scrollWidth <= innerWidth), `${view.name}: scrolls sideways`);

    // the sidebar: along the right edge, full height, not modal (no overlay; on a wide screen the
    // list moves aside and stays in use)
    await settingsButton.click();
    const side = page.getByRole("dialog", { name: "設定" });
    await side.waitFor();
    const sbox = await side.boundingBox();
    assert.ok(Math.abs(sbox.x + sbox.width - view.width) <= 1 && sbox.y <= 1 && sbox.height >= view.height - 1, `${view.name}: sidebar at the right edge ${JSON.stringify(sbox)}`);
    if (view.name === "desktop") {
      assert.ok(sbox.width <= 400, `sidebar width ${sbox.width}`);
      const right = await page.locator("[data-game]").evaluateAll((l) => Math.max(...l.map((e) => e.getBoundingClientRect().right)));
      assert.ok(right <= sbox.x, `the cards stay beside the sidebar (${right} > ${sbox.x})`);
      await page.getByRole("searchbox").fill("星");
      await page.waitForFunction(() => document.querySelectorAll("[data-game]").length === 1);
      await page.getByRole("searchbox").fill("");
      assert.equal(await side.isVisible(), true, "typing in the list keeps the sidebar");
    } else assert.ok(sbox.width >= view.width - 1, `phone: the sidebar is the whole width (${sbox.width})`);
    assert.equal(await page.locator("[data-slot=dialog-overlay]").count(), 0, "no overlay");
    // its tabs: 詳細 (no game picked yet: off), 統計, 設定 (open)
    const sideTabs = side.getByRole("tablist", { name: "サイドバーの表示" }).getByRole("tab");
    assert.deepEqual(await sideTabs.allInnerTexts(), ["詳細", "統計", "設定"]);
    assert.ok(await sideTabs.nth(0).isDisabled(), "詳細 waits for a game");
    assert.equal(await sideTabs.nth(2).getAttribute("aria-selected"), "true");
    const sections = await side.getByRole("region").evaluateAll((l) => l.map((e) => e.getAttribute("aria-label")));
    assert.deepEqual(sections, ["サーバー", "アプリ", "DMM アカウント"]);

    // サーバー: the running version, the stand-in's 9.9.9 offered
    const upd = side.getByTestId("server-update");
    await upd.getByRole("button", { name: "v9.9.9 に更新" }).waitFor();
    assert.match(await upd.innerText(), /動作中のサーバー: v\d+\.\d+\.\d+/);
    assert.match(await upd.innerText(), /新しいバージョン v9\.9\.9 があります/);

    // 統計 (the tab): total, games, sessions, most played first
    await sideTabs.nth(1).click();
    const stats = page.getByRole("dialog", { name: "統計" }).getByRole("region", { name: "プレイ統計" });
    await stats.getByRole("list", { name: "ゲーム別のプレイ時間" }).waitFor();
    const dtext = await stats.innerText();
    assert.match(dtext, /1時間 35分/, dtext);
    assert.match(dtext, /2 本/);
    assert.match(dtext, /3 回/);
    const rows = await stats.getByRole("list", { name: "ゲーム別のプレイ時間" }).getByRole("listitem").allInnerTexts();
    assert.match(rows[0], /月の光[\s\S]*1時間 30分/);
    assert.match(rows[1], /さくらの詩[\s\S]*5分/);
    // the reset asks first; やめる keeps everything
    await stats.getByRole("button", { name: "統計をリセット" }).click();
    await stats.getByRole("alertdialog", { name: "統計のリセットの確認" }).getByRole("button", { name: "やめる" }).click();
    assert.equal((await (await fetch(`${base}api/sessions`)).json()).length, 3, "やめる keeps the sessions");
    await shot("settings");
    await page.keyboard.press("Escape");
    await page.locator("[data-slot=sheet-content]").waitFor({ state: "detached" });
    await page.waitForFunction(() => document.activeElement?.getAttribute("aria-label") === "サイドバー");
    // the button again: the tab last shown (統計); once more: closed
    await settingsButton.click();
    await page.getByRole("dialog", { name: "統計" }).waitFor();
    assert.equal(await settingsButton.getAttribute("aria-expanded"), "true");
    // (on a phone the sidebar covers the whole page: its × closes it)
    if (view.name === "desktop") await settingsButton.click();
    else await page.getByRole("dialog", { name: "統計" }).getByRole("button", { name: "閉じる" }).click();
    await page.locator("[data-slot=sheet-content]").waitFor({ state: "detached" });
    assert.equal(await settingsButton.getAttribute("aria-expanded"), "false");

    // the game last played, to continue
    const cont = page.getByRole("region", { name: "続きから遊ぶ" });
    assert.match(await cont.innerText(), /星のかけら/);
    assert.equal(await cont.getByRole("button", { name: /つづきから遊ぶ/ }).count(), 1);

    // search: words narrow the list; a miss offers to clear it
    const search = page.getByRole("searchbox");
    await search.fill("月");
    assert.deepEqual(await page.locator("[data-game]").evaluateAll((l) => l.map((e) => e.dataset.game)), ["tsuki"]);
    assert.equal(await cont.count(), 0, "no continue banner while searching");
    // Japanese as typed: katakana / half-width for hiragana, full-width letters, a typo
    const shown = () => page.locator("[data-game]").evaluateAll((l) => l.map((e) => e.dataset.game).sort());
    for (const q of ["サクラ", "ｻｸﾗ", "さくらの話"]) {
      await search.fill(q);
      assert.deepEqual(await shown(), ["sakura"], q);
    }
    await search.fill("ＫＥＹ");
    assert.deepEqual(await shown(), ["sakura", "tsuki"]);
    await search.fill("ありえない");
    await page.getByRole("button", { name: /検索をクリア/ }).click();
    assert.equal(await page.locator("[data-game]").count(), 3);

    if (view.name === "desktop") {
      // a favorite from the game's dialog: on its card at once, under お気に入り, and after a reload
      await page.getByRole("button", { name: "月の光 の情報" }).click();
      const info = page.getByRole("dialog", { name: "詳細" });
      await info.getByRole("heading", { name: "月の光" }).waitFor();
      // the same sidebar on its 詳細 tab, beside the list; the tabs switch, the game is kept
      const ibox = await info.boundingBox();
      assert.ok(Math.abs(ibox.x + ibox.width - view.width) <= 1 && ibox.y <= 1 && ibox.height >= view.height - 1 && ibox.width <= 400, `info sidebar ${JSON.stringify(ibox)}`);
      const cardsRight = await page.locator("[data-game]").evaluateAll((l) => Math.max(...l.map((e) => e.getBoundingClientRect().right)));
      assert.ok(cardsRight <= ibox.x, `the cards stay beside the information (${cardsRight} > ${ibox.x})`);
      assert.equal(await page.locator("[data-slot=dialog-overlay]").count(), 0, "no overlay");
      await info.getByRole("tab", { name: "設定" }).click();
      await page.getByRole("dialog", { name: "設定" }).waitFor();
      assert.equal(await info.count(), 0, "設定 took the sidebar");
      await page.getByRole("dialog", { name: "設定" }).getByRole("tab", { name: "統計" }).click();
      await page.getByRole("dialog", { name: "統計" }).getByRole("region", { name: "プレイ統計" }).waitFor();
      await page.getByRole("dialog", { name: "統計" }).getByRole("tab", { name: "詳細" }).click();
      await info.getByRole("heading", { name: "月の光" }).waitFor();
      await page.getByRole("button", { name: "さくらの詩 の情報" }).click();
      await info.getByRole("heading", { name: "さくらの詩" }).waitFor();
      await page.getByRole("button", { name: "月の光 の情報" }).click();
      await info.getByRole("heading", { name: "月の光" }).waitFor();
      assert.equal(await page.getByRole("dialog").count(), 1, "one sidebar");
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
      const note = page.getByRole("dialog", { name: "詳細" }).getByRole("textbox", { name: "メモ" });
      const save = page.getByRole("button", { name: /メモを保存/ });
      assert.ok(await save.isDisabled(), "nothing to save yet");
      await note.fill("第2章まで");
      await save.click();
      await page.getByText("メモを保存しました").waitFor();
      const saved = await (await fetch(`${base}api/library/tsuki`)).json();
      assert.deepEqual(saved, { favorite: true, note: "第2章まで" });
      await page.keyboard.press("Escape");

      // order and sections: score (unscored last), release, brand sections, year sections, kept
      const order = () => page.locator("[data-game]").evaluateAll((l) => l.map((e) => e.dataset.game));
      const groupsShown = () => page.locator("[data-group]").evaluateAll((l) => l.map((e) => [e.querySelector("h3")?.firstChild?.textContent ?? "", [...e.querySelectorAll("[data-game]")].map((g) => g.dataset.game)]));
      await page.getByRole("tab", { name: /すべて/ }).click();
      await page.getByLabel("並び順").selectOption({ label: "スコアが高い順" });
      assert.deepEqual(await order(), ["tsuki", "sakura", "hoshi"]);
      await page.getByLabel("並び順").selectOption({ label: "発売日が新しい順" });
      assert.deepEqual(await order(), ["tsuki", "hoshi", "sakura"]);
      await page.getByLabel("区分け").selectOption({ label: "ブランドごと" });
      assert.deepEqual(await groupsShown(), [["Key", ["tsuki", "sakura"]], ["august", ["hoshi"]]].sort((a, b) => a[0].localeCompare(b[0], "ja")));
      await page.getByLabel("区分け").selectOption({ label: "発売年ごと" });
      assert.deepEqual(await groupsShown(), [["2011年", ["tsuki"]], ["2008年", ["hoshi"]], ["2004年", ["sakura"]]]);
      await shot("groups");
      await page.reload();
      await page.locator("[data-group]").first().waitFor();
      assert.equal(await page.getByLabel("並び順").inputValue(), "release");
      assert.equal(await page.getByLabel("区分け").inputValue(), "year");
      assert.deepEqual(await groupsShown(), [["2011年", ["tsuki"]], ["2008年", ["hoshi"]], ["2004年", ["sakura"]]]);
      // searching inside sections drops the empty ones
      await page.getByRole("searchbox").fill("星");
      assert.deepEqual(await groupsShown(), [["2008年", ["hoshi"]]]);
      await page.getByRole("searchbox").fill("");
      await page.getByLabel("区分け").selectOption({ label: "区分けなし" });
      await page.getByLabel("並び順").selectOption({ label: "最近遊んだ順" });
      assert.equal(await page.locator("[data-group] h3").count(), 0);

      // a brand's name, on a card or in 詳細, narrows the list to its games; × clears it
      await page.locator('[data-game="tsuki"] [data-brand="Key"]').click();
      assert.deepEqual((await order()).sort(), ["sakura", "tsuki"]);
      assert.match(await page.getByTestId("brand-filter").innerText(), /ブランド:\s*Key/);
      assert.equal(await page.getByRole("region", { name: "続きから遊ぶ" }).count(), 0, "no continue banner under a brand");
      await page.getByRole("searchbox").fill("星");
      await page.getByText("「星」・ブランド「Key」に合うゲームはありません").waitFor();
      await page.getByRole("searchbox").fill("");
      await page.getByRole("button", { name: "ブランドの絞り込みを解除" }).click();
      assert.equal(await page.locator("[data-game]").count(), 3);
      await page.getByRole("button", { name: "星のかけら の情報" }).click();
      await page.getByRole("dialog", { name: "詳細" }).locator('[data-brand="august"]').click();
      assert.deepEqual(await order(), ["hoshi"]);
      assert.match(await page.getByTestId("brand-filter").innerText(), /ブランド:\s*august/);
      await shot("brand");
      await page.getByRole("button", { name: "ブランドの絞り込みを解除" }).click();
      assert.equal(await page.locator("[data-game]").count(), 3);
      assert.equal(await page.getByTestId("brand-filter").count(), 0);
    } else {
      // the update, on the real server: the stand-in's download is no archive, and it says so
      await page.getByRole("button", { name: "サイドバー", exact: true }).click();
      await page.getByRole("dialog", { name: "統計" }).getByRole("tab", { name: "設定" }).click();
      const side = page.getByRole("dialog", { name: "設定" });
      const upd = side.getByTestId("server-update");
      await upd.getByRole("button", { name: "v9.9.9 に更新" }).click();
      await upd.getByRole("alert").filter({ hasText: "更新できませんでした" }).waitFor({ timeout: 60_000 });
      assert.equal(await upd.getByRole("button", { name: "もう一度確認" }).count(), 1);
      // the update going through (the server's answers stood in: installed, the old server still
      // answering once, the restart, the new one) reloads the page
      let version = 0;
      await page.route("**/api/update", (r) => (r.request().method() === "POST" ? r.fulfill({ json: { installed: "9.9.9" } }) : r.fallback()));
      await page.route("**/version", (r) => {
        version++;
        return version === 1 ? r.fulfill({ json: { version: "0.0.1", commit: "1234567" } }) : version === 2 ? r.abort() : r.fulfill({ json: { version: "9.9.9", commit: "89abcde" } });
      });
      const reloaded = page.waitForEvent("load", { timeout: 60_000 });
      await upd.getByRole("button", { name: "v9.9.9 に更新" }).click();
      await upd.getByRole("status").filter({ hasText: "v9.9.9 で再起動しています" }).waitFor();
      await reloaded;
      assert.ok(version >= 3, `polled /version ${version} times`);
      await page.unrouteAll();
      // the reset, confirmed: the server forgets every session and the statistics are empty
      await page.getByRole("heading", { name: "ippai", level: 1 }).waitFor();
      await page.getByRole("button", { name: "サイドバー", exact: true }).click();
      await page.getByRole("dialog", { name: "設定" }).getByRole("tab", { name: "統計" }).click();
      const stats = page.getByRole("dialog", { name: "統計" }).getByRole("region", { name: "プレイ統計" });
      await stats.getByRole("button", { name: "統計をリセット" }).click();
      await stats.getByRole("button", { name: "リセットする" }).click();
      await stats.getByText("プレイ記録はまだありません").waitFor();
      await page.getByText("プレイ統計をリセットしました").waitFor();
      assert.deepEqual(await (await fetch(`${base}api/sessions`)).json(), []);
      assert.equal(await stats.getByRole("button", { name: "統計をリセット" }).count(), 0, "nothing left to reset");
    }
    await ctx.close();
  }
  assert.deepEqual(failures, []);
  console.log("library-browser: ok");
} finally {
  await browser.close();
  server.kill();
  releases.close();
}
