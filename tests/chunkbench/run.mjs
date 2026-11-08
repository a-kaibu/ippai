// Chunk size vs a game's reads on an Android-like machine (577a146 review §2): the same game
// (chunkread.exe and a 192 MiB incompressible data.bin, as packed archives are) packed with
// FastCDC at each average size, served by ippai serve from this machine (a local server: no
// network throttling), run in Chromium. Per size: a first start (nothing on the device), a
// second (the chunks in Cache Storage), and one offline (the same reads, the device's copy only).
// Prints, per run: the page's start to the program's first line, its scattered reads (64 KiB at
// random places: p50/p95/max), a 32 MiB sequential read, the run's total, the bytes fetched, and
// the browser's peak RSS (all its processes). Run it under tests/android-like.sh for a phone's CPU
// (and THROTTLE=4 slows the page's main thread too).
//
//   tests/android-like.sh node tests/chunkbench/run.mjs [ippai] [sizes in KiB, default 1024,4096,16384]
//   (web/dist built with sake; PLAYWRIGHT as elsewhere; EXE: chunkread.exe, else built with mingw)
import { execFileSync, spawn } from "node:child_process";
import { cpSync, existsSync, mkdtempSync, mkdirSync, openSync, writeSync, closeSync, writeFileSync, readFileSync } from "node:fs";
import { randomBytes } from "node:crypto";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const sizes = (process.argv[3] ?? "1024,4096,16384").split(",").map(Number);
const work = mkdtempSync(join(tmpdir(), "ippai-chunkbench-"));
let exe = process.env.EXE;
if (!exe) {
  exe = join(work, "chunkread.exe");
  execFileSync("i686-w64-mingw32-gcc", ["-O2", resolve("tests/chunkbench/chunkread.c"), "-o", exe]);
}
const game = join(work, "game");
mkdirSync(game);
cpSync(exe, join(game, "chunkread.exe"));
const fd = openSync(join(game, "data.bin"), "w");
for (let i = 0; i < 192; i++) writeSync(fd, randomBytes(1 << 20));
closeSync(fd);
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
if (process.env.FONTS) cpSync(resolve(process.env.FONTS), join(prefix, "Fonts"), { recursive: true });

/** Chromium's processes' resident memory now (bytes). */
function rss() {
  let n = 0;
  try {
    for (const l of execFileSync("ps", ["-eo", "rss=,comm="], { encoding: "utf8" }).split("\n"))
      if (/chrom|headless_shell/.test(l)) n += Number(l.trim().split(/\s+/)[0]) * 1024;
  } catch {}
  return n;
}

const results = [];
for (const [i, kib] of sizes.entries()) {
  const store = join(work, `store-${kib}`);
  const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
  run("pack", join(work, "prefix"), "prefix32", "--chunk-kib", String(kib));
  run("pack", game, "g", "--chunk-kib", String(kib));
  run("add-game", "g", "--title", "chunks", "--tree", "g", "--exe", "chunkread.exe", "--no-meta");
  const chunks = readFileSync(join(store, "trees/g.tsv"), "utf8").split("\n").filter((l) => l.startsWith("f\t")).reduce((a, l) => a + (l.split("\t")[5]?.split(",").length ?? 0), 0);
  const port = 9300 + i * 10;
  const server = spawn(bin, ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, `jit-${kib}`), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: ["ignore", "ignore", "inherit"] });
  const base = `http://127.0.0.1:${port}/`;
  for (let k = 0; k < 100; k++) {
    if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
    await new Promise((r) => setTimeout(r, 300));
  }
  const ctx = await chromium.launchPersistentContext(join(work, `profile-${kib}`), { args: ["--enable-unsafe-webgpu", "--no-proxy-server"] });
  for (const phase of ["first", "second", "offline"]) {
    const page = await ctx.newPage();
    const cdp = await ctx.newCDPSession(page);
    if (process.env.THROTTLE) await cdp.send("Emulation.setCPUThrottlingRate", { rate: Number(process.env.THROTTLE) });
    // (store chunks fetched: the game's reads go through the network worker, reported with the page)
    let bytes = 0;
    const count = async (req) => {
      if (!req.url().includes("/store/chunks/")) return;
      const s = await req.sizes().catch(() => null);
      bytes += s?.responseBodySize ?? 0;
    };
    ctx.on("requestfinished", count);
    const lines = {};
    let peak = 0;
    const sampler = setInterval(() => (peak = Math.max(peak, rss())), 250);
    const t0 = Date.now();
    let first = 0;
    const done = new Promise((resolve) =>
      page.on("console", (m) => {
        const t = m.text();
        if (process.env.VERBOSE) console.log(`  [${phase}] ${t}`);
        const c = t.match(/CHUNKREAD (.*)/);
        if (!c) return;
        first ||= Date.now() - t0;
        for (const kv of c[1].split(" ")) {
          const [k, v] = kv.split("=");
          if (v !== undefined) lines[k] = Number(v);
        }
        if (c[1].startsWith("done") || c[1].startsWith("error")) resolve();
      }),
    );
    if (phase === "offline") await ctx.setOffline(true);
    await page.goto(`${base}play.html?game=g&autostart`);
    await Promise.race([done, new Promise((r) => setTimeout(r, Number(process.env.LIMIT_S ?? 600) * 1000))]);
    clearInterval(sampler);
    ctx.off("requestfinished", count);
    if (phase === "offline") await ctx.setOffline(false);
    const r = { kib, phase, chunks, to_first_line_ms: first, ...lines, fetched_mib: +(bytes / 1048576).toFixed(1), peak_rss_mib: Math.round(peak / 1048576) };
    console.log(JSON.stringify(r));
    results.push(r);
    await page.close();
  }
  await ctx.close();
  server.kill();
}
const cols = ["kib", "phase", "chunks", "to_first_line_ms", "start_ms", "p50", "p95", "max", "random_total_ms", "seq_ms", "total_ms", "fetched_mib", "peak_rss_mib"];
console.log(["", cols.join(" | "), ""].join("|"));
for (const r of results) console.log(["", cols.map((c) => r[c] ?? "").join(" | "), ""].join("|"));
