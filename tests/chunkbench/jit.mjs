// Start and run time with and without the regions earlier runs saved (577a146 review §2 JIT):
// flatbench.exe (Lua and SQLite, /mnt/project-files/sake/ab) through masu serve, a first run
// (nothing saved: every region translated as it runs), then RUNS more (the server's saved
// regions loaded at the start). Prints per run: page start to the program's line, flatbench's
// first and last round (ms), its average, and the page's whole time. Under tests/android-like.sh for a phone.
// ANDROID=1: V8 as on Android Chrome (no trap handler: every wasm load and store checked,
// docs/translator.md "Android"). ENV=K=V,... adds variables for the program (&env=).
//
//   tests/android-like.sh node tests/chunkbench/jit.mjs [masu] [flatbench.exe]
import { execFileSync, spawn } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
const bin = resolve(process.argv[2] ?? "target/debug/masu");
const exe = resolve(process.argv[3] ?? "/mnt/project-files/sake/ab/flatbench.exe");
const work = mkdtempSync(join(tmpdir(), "masu-jitbench-"));
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
mkdirSync(join(work, "prefix/drive_c/windows/temp"), { recursive: true });
writeFileSync(join(work, "prefix/drive_c/windows/win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
mkdirSync(join(work, "game"));
cpSync(exe, join(work, "game/flatbench.exe"));
run("pack", join(work, "game"), "f");
// (REPS rounds: kura saves regions after the program ran 20 s; each round's time printed)
run("add-game", "f", "--title", "flatbench", "--tree", "f", "--exe", "flatbench.exe", "--no-meta", "--arg", process.env.REPS ?? "12", "--arg", "all", "--arg", "x");
const port = 9400;
const server = spawn(bin, ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: ["ignore", "ignore", "inherit"] });
const base = `http://127.0.0.1:${port}/`;
for (let k = 0; k < 100; k++) {
  if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}
const ctx = await chromium.launchPersistentContext(join(work, "profile"), {
  args: ["--enable-unsafe-webgpu", "--no-proxy-server", ...(process.env.ANDROID ? ["--js-flags=--wasm-enforce-bounds-checks"] : [])],
});
const runs = 1 + Number(process.env.RUNS ?? 2);
try {
  for (let i = 0; i < runs; i++) {
    const page = await ctx.newPage();
    const cdp = await ctx.newCDPSession(page);
    if (process.env.THROTTLE) await cdp.send("Emulation.setCPUThrottlingRate", { rate: Number(process.env.THROTTLE) });
    const t0 = Date.now();
    let parts = "";  // (empty: the saved regions loaded)
    const reps = [];
    const line = new Promise((resolve) =>
      page.on("console", (m) => {
        const t = m.text();
        if (process.env.VERBOSE) console.log(`  ${t}`);
        if (/saved regions/.test(t) && !parts) parts = t.slice(0, 100);
        const r = t.match(/^rep (\d+) ([\d.]+)/);
        if (r) reps.push(Number(r[2]));
        if (t.startsWith("flatbench ")) resolve(t);
      }),
    );
    const env = (process.env.ENV ?? "").split(",").filter(Boolean).map((e) => `&env=${encodeURIComponent(e)}`).join("");
    await page.goto(`${base}play.html?game=f&autostart${env}`);
    const out = await Promise.race([line, new Promise((r) => setTimeout(() => r(null), 600000))]);
    const total = out?.match(/total=([\d.]+)/)?.[1];
    console.log(JSON.stringify({ run: i ? "saved" : "first", page_ms: Date.now() - t0, flatbench_avg_ms: Number(total), first_rep_ms: reps[0], last_rep_ms: reps.at(-1), parts: parts || "saved regions loaded" }));
    // (the run's new regions go to the server as it exits)
    await new Promise((r) => setTimeout(r, 8000));
    await page.close();
  }
} finally {
  await ctx.close();
  server.kill();
}
