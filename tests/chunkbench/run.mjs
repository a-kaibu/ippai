// Chunk size vs a game's reads on an Android-like machine (577a146 review §2): the same game
// (chunkread.exe and a 192 MiB incompressible data.bin, as packed archives are) packed with
// FastCDC at each average size, served by masu serve from this machine (a local server: no
// network throttling), run in Chromium. Per size: a first start (nothing on the device), a
// second (the chunks in Cache Storage), and one offline (the same reads, the device's copy only).
// Prints, per run: the page's start to the program's first line, its scattered reads (64 KiB at
// random places: p50/p95/max), a 32 MiB sequential read, the run's total, the bytes fetched, and
// the browser's peak RSS (all its processes). Run it under tests/android-like.sh for a phone's CPU
// (and THROTTLE=4 slows the page's main thread too).
//
// NET="<kbit/s>,<rtt ms>" adds a first start over a link that slow (CDP network emulation; the
// game's reads go through the network worker, which it covers), in a profile of its own, of the
// same data read NET_READS="<reads> <bytes each>" (default "50 131072": a scene's voices and
// pictures, scattered) instead: what a chunk size costs a first play on a phone's line.
//
//   tests/android-like.sh node tests/chunkbench/run.mjs [masu] [sizes in KiB, default 1024,4096,16384]
//   (web/dist built with sake; PLAYWRIGHT as elsewhere; EXE: chunkread.exe, else built with mingw;
//   WEB: the built app, default web/dist)
import { execFileSync, spawn } from "node:child_process";
import { cpSync, rmSync, existsSync, mkdtempSync, mkdirSync, openSync, writeSync, closeSync, writeFileSync, readFileSync } from "node:fs";
import { createCipheriv } from "node:crypto";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/masu");
const sizes = (process.argv[3] ?? "1024,4096,16384").split(",").map(Number);
const work = mkdtempSync(join(tmpdir(), "masu-chunkbench-"));
let exe = process.env.EXE;
if (!exe) {
  exe = join(work, "chunkread.exe");
  execFileSync("i686-w64-mingw32-gcc", ["-O2", resolve("tests/chunkbench/chunkread.c"), "-o", exe]);
}
const game = join(work, "game");
mkdirSync(game);
cpSync(exe, join(game, "chunkread.exe"));
// (the same bytes every run, from SEED: runs of two builds read the same data, and the
// program's sum of what it read must be the same)
const fd = openSync(join(game, "data.bin"), "w");
const stream = createCipheriv("aes-128-ctr", Buffer.alloc(16, Number(process.env.SEED ?? 1)), Buffer.alloc(16));
for (let i = 0; i < 192; i++) writeSync(fd, stream.update(Buffer.alloc(1 << 20)));
closeSync(fd);
const net = process.env.NET?.split(",").map(Number);
if (net) {
  const g = join(work, "gnet");
  mkdirSync(g);
  cpSync(exe, join(g, "chunkread.exe"));
  cpSync(join(game, "data.bin"), join(g, "data.bin"));
  writeFileSync(join(g, "n.txt"), process.env.NET_READS ?? "50 131072");
}
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
  if (net) {
    run("pack", join(work, "gnet"), "gnet", "--chunk-kib", String(kib));
    run("add-game", "gnet", "--title", "chunks (net)", "--tree", "gnet", "--exe", "chunkread.exe", "--no-meta");
  }
  const chunks = readFileSync(join(store, "trees/g.tsv"), "utf8").split("\n").filter((l) => l.startsWith("f\t")).reduce((a, l) => a + (l.split("\t")[5]?.split(",").length ?? 0), 0);
  const port = 9300 + i * 10;
  const server = spawn(bin, ["serve", "--foreground", "--web", resolve(process.env.WEB ?? "web/dist"), "--store", store, "--jit", join(work, `jit-${kib}`), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: ["ignore", "ignore", "inherit"] });
  const base = `http://127.0.0.1:${port}/`;
  for (let k = 0; k < 100; k++) {
    if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
    await new Promise((r) => setTimeout(r, 300));
  }
  const args = { args: ["--enable-unsafe-webgpu", "--no-proxy-server"] };
  const netCtx = net && (await chromium.launchPersistentContext(join(work, `profile-net-${kib}`), args));
  const ctx = await chromium.launchPersistentContext(join(work, `profile-${kib}`), args);
  // (PHASES: some of first,second,offline)
  const phases = (process.env.PHASES ?? "first,second,offline").split(",").filter(Boolean);
  for (const phase of [...(net ? ["net-first"] : []), ...phases]) {
    const c = phase === "net-first" ? netCtx : ctx;
    const page = await c.newPage();
    const cdp = await c.newCDPSession(page);
    if (process.env.THROTTLE) await cdp.send("Emulation.setCPUThrottlingRate", { rate: Number(process.env.THROTTLE) });
    if (phase === "net-first") {
      await cdp.send("Network.enable");
      await cdp.send("Network.emulateNetworkConditions", { offline: false, latency: net[1], downloadThroughput: (net[0] * 1000) / 8, uploadThroughput: (net[0] * 1000) / 8 });
    }
    // (store chunks fetched: the game's reads go through the network worker, reported with the page)
    let bytes = 0;
    const count = async (req) => {
      if (!req.url().includes("/store/chunks/")) return;
      const s = await req.sizes().catch(() => null);
      bytes += s?.responseBodySize ?? 0;
    };
    c.on("requestfinished", count);
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
    await page.goto(`${base}play.html?game=${phase === "net-first" ? "gnet" : "g"}&autostart`);
    await Promise.race([done, new Promise((r) => setTimeout(r, Number(process.env.LIMIT_S ?? 600) * 1000))]);
    clearInterval(sampler);
    c.off("requestfinished", count);
    if (phase === "offline") await ctx.setOffline(false);
    const r = { kib, phase, chunks, to_first_line_ms: first, ...lines, fetched_mib: +(bytes / 1048576).toFixed(1), peak_rss_mib: Math.round(peak / 1048576) };
    console.log(JSON.stringify(r));
    results.push(r);
    await page.close();
  }
  await ctx.close();
  if (netCtx) await netCtx.close();
  server.kill();
  // (each size's store and profiles are GBs: gone before the next)
  for (const d of [store, join(work, `jit-${kib}`), join(work, `profile-${kib}`), join(work, `profile-net-${kib}`)]) rmSync(d, { recursive: true, force: true });
}
const cols = ["kib", "phase", "chunks", "to_first_line_ms", "start_ms", "p50", "p95", "max", "random_total_ms", "seq_ms", "total_ms", "fetched_mib", "peak_rss_mib"];
console.log(["", cols.join(" | "), ""].join("|"));
for (const r of results) console.log(["", cols.map((c) => r[c] ?? "").join(" | "), ""].join("|"));
rmSync(work, { recursive: true, force: true });
