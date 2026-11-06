// The app installed and offline (web/src/sw.ts, pwa.ts, manifest.webmanifest): starts ippai
// serve on a small store and checks in Chromium that the page is installable, that after one
// online visit it opens with no network (still cross-origin isolated, the game list as last
// read, "オフラインです") and that a file never fetched answers at once as offline (no retries);
// then the same with the network up but the server gone.
// With EXE (a Windows console program) and FONTS (a drive_c/windows/Fonts folder) and sake built
// into web/dist/sake: the game is saved to the device from its dialog, then started offline,
// and must print a line matching EXPECT (default: one starting with the program's name).
//
//   node tests/offline-browser.mjs [path to ippai]   (web/dist built; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const web = resolve("web/dist");
const work = mkdtempSync(join(tmpdir(), "ippai-offline-"));
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const exe = process.env.EXE && resolve(process.env.EXE);
const full = !!(exe && process.env.FONTS && existsSync(join(web, "sake/sake.wasm")));

const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
if (full) cpSync(resolve(process.env.FONTS), join(prefix, "Fonts"), { recursive: true });
else writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(game);
if (full) cpSync(exe, join(game, basename(exe)));
else writeFileSync(join(game, "game.exe"), "MZ not really");
run("pack", game, "g");
run("add-game", "g", "--title", "オフラインのゲーム", "--tree", "g", "--exe", full ? basename(exe) : "game.exe", "--no-meta");

const port = 8700 + Math.floor(Math.random() * 200) * 2;
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
  viewport: { width: 1280, height: 720 },
});
const lines = [];
try {
  const page = browser.pages()[0] ?? (await browser.newPage());
  page.on("console", (m) => lines.push(m.text()));

  // installable: Chrome reads the manifest (icons, start_url, display)
  await page.goto(`${base}play.html`);
  const cdp = await browser.newCDPSession(page);
  const { errors = [], url: manifestUrl } = await cdp.send("Page.getAppManifest");
  assert.ok(manifestUrl.endsWith("manifest.webmanifest"), `manifest: ${manifestUrl}`);
  assert.deepEqual(errors, [], "manifest errors");
  const { installabilityErrors } = await cdp.send("Page.getInstallabilityErrors");
  assert.deepEqual(installabilityErrors, [], "installability");

  // the service worker controls the page once it is ready (it claims it)
  await page.waitForFunction(() => !!navigator.serviceWorker.controller, null, { timeout: 30000 });
  await page.getByText("オフラインのゲーム").first().waitFor();

  if (full) {
    await page.getByRole("button", { name: "オフラインのゲーム の情報" }).click();
    await page.getByRole("button", { name: "端末に保存" }).click();
    await page.getByText(/端末に保存済み/).waitFor({ timeout: 120000 });
  }

  await browser.setOffline(true);
  await page.reload();
  assert.equal(await page.evaluate(() => crossOriginIsolated), true, "isolated offline");
  await page.getByText("オフラインのゲーム").first().waitFor({ timeout: 10000 });

  await page.getByText("オフラインです").waitFor({ timeout: 5000 });
  // never fetched: offline at once (503, x-ippai-offline), not a network error to retry
  const missed = await page.evaluate(async () => {
    const t = performance.now();
    const r = await fetch("jit/never-saved.parts");
    return { status: r.status, offline: r.headers.get("x-ippai-offline"), ms: performance.now() - t };
  });
  assert.equal(missed.status, 503);
  assert.equal(missed.offline, "1");
  assert.ok(missed.ms < 2000, `miss took ${missed.ms} ms`);

  if (full) {
    const t = Date.now();
    // the program's own output (EXPECT: a regular expression, by default any line starting with
    // the program's name; the page goes back to the list when the program exits)
    const expect = new RegExp(process.env.EXPECT ?? `^${basename(exe, ".exe")}`, "i");
    const out = new Promise((resolve) => page.on("console", (m) => expect.test(m.text()) && resolve(m.text())));
    await page.goto(`${base}play.html?game=g&autostart`);
    const first = await Promise.race([out, new Promise((r) => setTimeout(() => r(null), 120000))]);
    assert.ok(first, "no output from the program offline");
    console.log(`offline: start to the program's output ${((Date.now() - t) / 1000).toFixed(1)} s: ${first}`);
  }
  // a network but the server out of reach (a phone away from home): the same, from the device
  await browser.setOffline(false);
  server.kill();
  await new Promise((r) => setTimeout(r, 500));
  await page.goto(`${base}play.html`);
  await page.getByText("オフラインのゲーム").first().waitFor({ timeout: 15000 });
  await page.getByText("オフラインです").waitFor({ timeout: 5000 });
  console.log(`offline-browser: ok${full ? " (game run offline)" : ""}`);
} catch (e) {
  console.log(lines.slice(-40).join("\n"));
  throw e;
} finally {
  await browser.close();
  server.kill();
}
