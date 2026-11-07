// The app installed and offline (web/src/sw.ts, pwa.ts, manifest.webmanifest): starts ippai
// serve on a small store and checks in Chromium that the page is installable, that after one
// online visit it opens with no network (still cross-origin isolated, the game list as last
// read, "オフラインです") and that a file never fetched answers at once as offline (no retries);
// then the same with the network up but the server gone. Two games sharing chunks are saved to
// the device and one removed: the other stays saved.
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
const work = mkdtempSync(join(tmpdir(), "ippai-offline-"));
// (a copy of web/dist: without a sake build, stand-ins for its files, which 端末に保存 fetches)
const web = join(work, "web");
cpSync(resolve("web/dist"), web, { recursive: true });
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const exe = process.env.EXE && resolve(process.env.EXE);
const full = !!(exe && process.env.FONTS && existsSync(join(web, "sake/sake.wasm")));
if (!existsSync(join(web, "sake/sake.wasm"))) {
  mkdirSync(join(web, "sake"), { recursive: true });
  writeFileSync(join(web, "sake/sake.js"), "export default () => {};\n");
  writeFileSync(join(web, "sake/sake.wasm"), new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]));
  writeFileSync(join(web, "sake/jit-version.txt"), "00000000\n");
}

const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
if (full) cpSync(resolve(process.env.FONTS), join(prefix, "Fonts"), { recursive: true });
else writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(game);
if (full) cpSync(exe, join(game, basename(exe)));
else writeFileSync(join(game, "game.exe"), "MZ not really");
// (a file both games have: one chunk they share, besides the prefix's)
const shared = new Uint8Array(300 << 10).map((_, i) => (i * 7) & 255);
writeFileSync(join(game, "shared.dat"), shared);
run("pack", game, "g");
run("add-game", "g", "--title", "オフラインのゲーム", "--tree", "g", "--exe", full ? basename(exe) : "game.exe", "--no-meta");
const other = join(work, "other");
mkdirSync(other);
writeFileSync(join(other, "other.exe"), "MZ another");
writeFileSync(join(other, "shared.dat"), shared);
run("pack", other, "a");
run("add-game", "a", "--title", "もうひとつのゲーム", "--tree", "a", "--exe", "other.exe", "--no-meta");

const port = 8700 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", web, "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun", "--trust-loopback"],
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
// what reached the network for the file never saved (the page's own and the service worker's
// requests: Chromium reports both to the context)
const seen = [];
const watch = (what) => (r) => r.url().includes("never-saved") && seen.push(`${what} ${r.serviceWorker() ? "sw" : "page"} ${r.failure()?.errorText ?? ""}`.trim());
browser.on("request", watch("request"));
browser.on("requestfailed", watch("failed"));
browser.on("requestfinished", watch("finished"));
let page;
/** A file never fetched answers at once as offline (503 + x-ippai-offline); on a miss, what the
 * page, its service worker and the network saw, to tell the worker's fault from the harness's. */
async function neverFetched(phase) {
  seen.length = 0;
  const missed = await page.evaluate(async () => {
    const t = performance.now();
    const r = await fetch(`jit/never-saved.parts?${Math.random()}`);
    const headers = Object.fromEntries(r.headers);
    const ms = performance.now() - t;
    const sw = navigator.serviceWorker.controller;
    const state = await new Promise((resolve) => {
      if (!sw) return resolve(null);
      const ch = new MessageChannel();
      ch.port1.onmessage = (e) => resolve(e.data);
      setTimeout(() => resolve("no answer"), 1000);
      sw.postMessage("state", [ch.port2]);
    });
    return { status: r.status, offline: r.headers.get("x-ippai-offline"), ms, headers, onLine: navigator.onLine, controlled: !!sw, swState: sw?.state, state };
  });
  const diag = `${phase}: ${JSON.stringify({ ...missed, network: seen })}`;
  assert.equal(missed.status, 503, diag);
  assert.equal(missed.offline, "1", diag);
  assert.ok(missed.ms < 2000, `miss took ${missed.ms} ms: ${diag}`);
}
try {
  page = browser.pages()[0] ?? (await browser.newPage());
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

  // both games saved to the device, then the other one removed: this one's chunks (the prefix's
  // and the file they share) must stay
  const dialog = async (title, f) => {
    await page.getByRole("button", { name: `${title} の情報` }).click();
    await f(page.getByRole("dialog"));
    await page.keyboard.press("Escape");
    await page.getByRole("dialog").waitFor({ state: "detached" });
  };
  for (const title of ["オフラインのゲーム", "もうひとつのゲーム"])
    await dialog(title, async (d) => {
      await d.getByRole("button", { name: "端末に保存" }).click();
      await d.getByText(/端末に保存済み/).waitFor({ timeout: 120000 });
    });
  await dialog("もうひとつのゲーム", async (d) => {
    await d.getByRole("button", { name: "端末から削除" }).click();
    // (what it shares with the saved game stays)
    await d.getByText(/一部を端末に保存済み/).waitFor();
  });
  await dialog("オフラインのゲーム", (d) => d.getByText(/端末に保存済み/).waitFor({ timeout: 10000 }));

  await browser.setOffline(true);
  // (Playwright 1.56 left the service worker online here: its fetches reached the server, which
  // answered 404; 1.63 takes the worker offline with the page, CI's browser job pins it)
  await page.reload();
  assert.equal(await page.evaluate(() => crossOriginIsolated), true, "isolated offline");
  await page.getByText("オフラインのゲーム").first().waitFor({ timeout: 10000 });

  await page.getByText("オフラインです").waitFor({ timeout: 5000 });
  // never fetched: offline at once (503, x-ippai-offline), not a network error to retry
  await neverFetched("offline");

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
  await neverFetched("server gone");
  console.log(`offline-browser: ok${full ? " (game run offline)" : ""}`);
} catch (e) {
  console.log(lines.slice(-40).join("\n"));
  throw e;
} finally {
  await browser.close();
  server.kill();
}
