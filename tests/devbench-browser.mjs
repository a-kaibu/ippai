// The device benchmark (web/bench.html, web/src/bench.ts) end to end in headless Chromium: a
// profile from the start screen's button to the comparison table, every step's numbers in the
// run ippai serve kept (GET /bench-results), the program steps' checksums agreeing (the JIT's
// first and second bench.exe, the second from the saved translations), and a second run compared
// with the first. Needs a built page with benchwasm (host/tests/bench-wasm.sh, tools/ci/build.sh)
// and the programs (PROGRAMS_ONLY=1 tests/bench/build.sh DIR); packs them as the tree "bench"
// of a new store (C:\ with FONTS's fonts, else none).
//
//   node tests/devbench-browser.mjs IPPAI PROGRAMS [profile]
//   env: ANDROID=1 (V8 as on Android: every wasm access checked), PLAYWRIGHT, SHOTS=dir, FONTS=dir
import { execFileSync, spawn } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import assert from "node:assert/strict";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
const [bin, programs, profile = "quick"] = process.argv.slice(2);
const work = mkdtempSync(join(tmpdir(), "ippai-devbench-"));
const store = join(work, "store");
mkdirSync(join(work, "prefix/drive_c/windows/temp"), { recursive: true });
writeFileSync(join(work, "prefix/drive_c/windows/win.ini"), "");
if (process.env.FONTS) cpSync(process.env.FONTS, join(work, "prefix/drive_c/windows/Fonts"), { recursive: true });
execFileSync(resolve(bin), ["--store", store, "pack", join(work, "prefix"), "prefix32"], { stdio: "pipe" });
execFileSync(resolve(bin), ["--store", store, "pack", resolve(programs), "bench"], { stdio: "pipe" });
const port = 9450 + Math.floor(Math.random() * 40) * 2;
const server = spawn(resolve(bin), ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: ["ignore", "ignore", "inherit"] });
const base = `http://127.0.0.1:${port}/`;
for (let k = 0; k < 100 && !(await fetch(`${base}bench.html`).then((r) => r.ok, () => false)); k++) await new Promise((r) => setTimeout(r, 300));
const browser = await chromium.launch({
  args: ["--enable-unsafe-webgpu", "--no-proxy-server", "--disable-background-timer-throttling", ...(process.env.ANDROID ? ["--js-flags=--wasm-enforce-bounds-checks"] : [])],
});
const runs = async () => (await (await fetch(`${base}bench-results`)).text()).split("\n").filter(Boolean).map((l) => JSON.parse(l));
// (a phone: its viewport and pixel ratio, so the WebGPU step draws at device pixels)
const ctx = await browser.newContext({ viewport: { width: 914, height: 411 }, deviceScaleFactor: 2.625 });
let failed = false;
try {
  for (let round = 1; round <= 2; round++) {
    const page = await ctx.newPage();
    page.on("console", (m) => process.env.VERBOSE && console.log(`  ${m.text()}`));
    page.on("pageerror", (e) => console.log(`  page error: ${e.message}`));
    // (&headless passes through to the program steps: headless Chromium can't present their canvas)
    await page.goto(`${base}bench.html?headless`);
    const label = { quick: "クイック", standard: "標準", full: "すべて" }[profile];
    await page.getByRole("button", { name: new RegExp(`^${label}`) }).waitFor({ timeout: 30000 });
    if (process.env.SHOTS) await page.screenshot({ path: join(process.env.SHOTS, `devbench-start-${round}.png`) });
    await page.getByRole("button", { name: new RegExp(`^${label}`) }).click({ timeout: 30000 });
    const t0 = Date.now();
    await page.waitForFunction(() => /計測完了/.test(document.body.innerText), null, { timeout: 1800000, polling: 1000 });
    console.log(`round ${round}: ${((Date.now() - t0) / 1000).toFixed(0)} s`);
    if (process.env.SHOTS) await page.screenshot({ path: join(process.env.SHOTS, `devbench-${round}.png`), fullPage: true });
    const text = await page.evaluate(() => document.body.innerText);
    assert.match(text, /サーバーに保存しました/);
    assert.match(text, /この端末/);
    const all = await runs();
    assert.equal(all.length, round);
    const run = all.at(-1);
    assert.equal(run.profile, profile);
    assert.equal(run.device.dpr, 2.625);
    assert.ok(run.device.cores > 0 && run.device.ua.includes("Chrome"));
    for (const [id, s] of Object.entries(run.steps)) {
      const keys = Object.keys(s.metrics);
      console.log(`  ${id}: ${s.error ? `ERROR ${s.error} ` : ""}${keys.slice(0, 6).map((k) => `${k}=${s.metrics[k].toFixed(1)}`).join(" ")} (${(s.wall_ms / 1000).toFixed(1)} s)`);
      if (s.error) failed = true;
      else assert.ok(keys.length, id);
    }
    // the JIT computed what it did the first time, from saved translations too; wasm the same as x86
    for (const k of ["crc32", "lz", "blend", "memcpy", "sort", "calls"]) {
      assert.equal(run.steps.jit1.checks[k], run.steps.jit2.checks[k], k);
      assert.equal(run.steps.wasm.checks[k], run.steps.jit2.checks[k], `wasm ${k}`);
    }
    assert.ok(run.steps.gpu.metrics["fill.mpix_s"] > 0);
    assert.ok(run.steps["d3d9.vn"].metrics.vn > 0);
    if (round === 2) assert.ok(!/まだ結果はありません/.test(text));
    await page.close();
  }
} finally {
  await browser.close();
  server.kill();
}
if (failed) {
  console.log("some steps failed");
  process.exit(1);
}
console.log("ok");
