// The scenario benchmark's player (tests/bench/src/scenario.c, run.mjs runs it for cases with
// `scenario`): plays the game as a player would, in a fresh browser context (no HTTP cache, no
// chunks kept, no saved games), clicking whenever the game waits; starts it again with what the
// browser kept, then loads the save. The server's saved translations are there for these (run.mjs's
// warm-up round made them); a last start in a new context goes without them. Where the game says
// so, pictures of the screen are checked (shots.mjs).
//
// Metrics (ms unless named otherwise; lower is better):
// Starts (page opened -> the title shown), by what is already there: the server's saved
// translations (JIT), the browser's HTTP cache (the page, sake.wasm, /jit/), the store's chunks in
// Cache Storage (the game's and the prefix's files):
//   start.nothing         none of them: the first start ever (&jit=0, a new context)
//   cold.startup          the translations only (a new context: the first start on a device)
//   start.chunks_kept     the translations and the chunks (HTTP cache emptied)
//   start.warm            all three (the same context again)
//   line_p50, line_p95    click -> the next line's first frame (text advance)
//   scene_p50, scene_max  click -> a scene transition done (pictures read and decoded, crossfade)
//   choice                click -> the choice's next scene done
//   fx_*                  click -> an effect done (the KiriKiri game: testdata/kirikiri)
//   idle_cpu              the browser's CPU per second while the game waits for 3 s (ms/s), and
//   idle_cpu.<part>       the same by Chromium process and thread: workers (the page's workers:
//                         the game), renderer_main, renderer_other, gpu (the GPU process: Dawn,
//                         SwiftShader), browser, other (utility: audio, network); the busiest
//                         threads go to the log
//   gpu_busy              the GPU's time in render passes per second of play (ms/s; timestamp-query)
//   peak_rss_mb           the browser's processes' memory at most (MB)
//   chunks, chunk_mb      the store's chunks the page fetched while playing (count, MB)
//   warm.restore          page opened again -> the save restored and shown
// plus the game's own numbers (boot_to_title, frame_p50/p95/p99, fade_ms, movie_p95, movie_late).
import { readdirSync, readFileSync } from "node:fs";
import { shots } from "./shots.mjs";

const linux = process.platform === "linux";
// (BENCH_VERBOSE=1: the page's lines as they come; BENCH_URL_EXTRA="&d3dstats" and the like: more
// for the page's URL, to look into something)
const verbose = !!process.env.BENCH_VERBOSE;
// the browser's processes (this job runs one browser)
function browserProcs() {
  if (!linux) return [];
  const out = [];
  for (const pid of readdirSync("/proc")) {
    if (!/^\d+$/.test(pid)) continue;
    try {
      const cmd = readFileSync(`/proc/${pid}/cmdline`, "latin1");
      if (/chrom|headless_shell/i.test(cmd.split("\0")[0])) out.push(pid);
    } catch {}
  }
  return out;
}
// (utime + stime of a /proc stat file, ms at 100 ticks/s)
const statMs = (f) => {
  try {
    const v = readFileSync(f, "latin1").split(") ")[1].split(" ");
    return (Number(v[11]) + Number(v[12])) * 10;
  } catch {
    return 0;
  }
};
const cpuMs = () => browserProcs().reduce((s, pid) => s + statMs(`/proc/${pid}/stat`), 0);
// Chromium's process of a pid (--type=renderer, gpu-process, utility; none: the browser) and the
// kind of each of its threads by name: where the CPU goes, not only how much
const procType = (pid) => {
  try {
    return /--type=([\w-]+)/.exec(readFileSync(`/proc/${pid}/cmdline`, "latin1"))?.[1] ?? "browser";
  } catch {
    return "gone";
  }
};
const threadKind = (comm) =>
  /^DedicatedWorker|^Worker/.test(comm) ? "workers"
  : /^CrRendererMain/.test(comm) ? "main"
  : /^CrGpuMain/.test(comm) ? "main"
  : /^CrBrowserMain/.test(comm) ? "main"
  : /^swiftshader|^vk|^SwiftShader|^Task/i.test(comm) ? "swiftshader"
  : /^VizCompositor|^Compositor/.test(comm) ? "compositor"
  : /^AudioOutput|^Audio|^Media/.test(comm) ? "audio"
  : "other";
/** {"<process>.<thread kind>": ms, "thread:<process>/<name>": ms} */
const cpuThreads = () => {
  const out = {};
  for (const pid of browserProcs()) {
    const t = procType(pid);
    let tids = [];
    try {
      tids = readdirSync(`/proc/${pid}/task`);
    } catch {}
    for (const tid of tids) {
      let comm = "?";
      try {
        comm = readFileSync(`/proc/${pid}/task/${tid}/comm`, "latin1").trim();
      } catch {}
      const ms = statMs(`/proc/${pid}/task/${tid}/stat`);
      const k = `${t}.${threadKind(comm)}`, n = `thread:${t}/${comm.replace(/\d+$/, "#")}`;
      (out[k] = (out[k] ?? 0) + ms), (out[n] = (out[n] ?? 0) + ms);
    }
  }
  return out;
};
const rssMb = () =>
  browserProcs().reduce((s, pid) => {
    try {
      return s + (Number(readFileSync(`/proc/${pid}/statm`, "latin1").split(" ")[1]) * 4096) / 1048576;
    } catch {
      return s;
    }
  }, 0);

// (linear between the two nearest: the median of two is their mean)
const pct = (v, p) => {
  const s = [...v].sort((a, b) => a - b);
  if (!s.length) return undefined;
  const i = p * (s.length - 1), lo = Math.floor(i);
  return s[lo] + (s[Math.min(lo + 1, s.length - 1)] - s[lo]) * (i - lo);
};

/** One play of the scenario on one build: { metrics, checks, error, log } */
export async function runScenario(browser, build, c, timeout) {
  const metrics = {}, checks = {}, log = [];
  let error;
  const shot = shots(c.name ?? c.exe.replace(/\.exe$/, ""));
  const url = (args, extra = "") =>
    `${build.url}/play.html?autostart&headless&mount=/ippai/game=bench&args=${encodeURIComponent(args)}` +
    `&run=${encodeURIComponent(`Z:\\ippai\\game\\${c.exe}`)}${extra}${process.env.BENCH_URL_EXTRA ?? ""}` +
    (c.env ?? []).map((e) => `&env=${encodeURIComponent(e)}`).join("");
  // a start up to the title in its own page of `ctx`: ms, or throws
  const toTitle = async (ctx, extra, what) => {
    const p = await ctx.newPage();
    let fail;
    const t = Date.now();
    try {
      const shown = await new Promise((resolve) => {
        p.on("console", (m) => {
          for (const line of m.text().split("\n").map((l) => l.replace(/^\d\d:\d\d:\d\d /, ""))) {
            if (/^BENCH-STATE title /.test(line)) resolve(Date.now());
            else if (/^BENCH-FAIL |ゲームが終了しました|[Uu]nhandled exception|\[crash\]|sake: call to .*which we neither implement/.test(line))
              (fail ??= line.trim()), resolve();
          }
        });
        p.on("crash", () => ((fail ??= "the page crashed"), resolve()));
        setTimeout(resolve, timeout);
        p.goto(url(c.args, extra)).catch((e) => ((fail ??= `${e.message ?? e}`), resolve()));
      });
      if (!shown) throw new Error(`${what}: ${fail ?? `no title in ${Math.round(timeout / 1000)} s`}`);
      return shown - t;
    } finally {
      await p.close().catch(() => {});
    }
  };
  const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  let chunks = 0, chunkBytes = 0;
  context.on("requestfinished", async (r) => {
    if (!/\/store\/chunks\//.test(r.url())) return;
    chunks++;
    chunkBytes += (await r.sizes().catch(() => null))?.responseBodySize ?? 0;
  });
  // (the first play also translates the game's code: twice the time of a case; the load its own)
  let deadline = Date.now() + 2 * timeout;
  try {
    // ---- cold: the whole play ----
    const page = await context.newPage();
    const events = [];
    let wake = () => {};
    const ended = { v: false };
    page.on("console", (m) => {
      // (krkrz's log lines start with the time: "12:34:56 ")
      for (const line of m.text().split("\n").map((l) => l.replace(/^\d\d:\d\d:\d\d /, ""))) {
        if (log.length < 400) log.push(line);
        if (verbose) console.log(`  ${((Date.now() - t0) / 1000).toFixed(1)} ${line.slice(0, 1000)}`);
        let r;
        if ((r = /^BENCH-STATE (\S+) (\d+)$/.exec(line))) events.push({ kind: "state", what: r[1], step: +r[2], t: Date.now() });
        else if ((r = /^BENCH-WAIT (\d+)(?: (\S+))?$/.exec(line))) events.push({ kind: "wait", what: r[2] ?? "", step: +r[1], t: Date.now() });
        else if ((r = /^BENCH (\S+) ([-\d.]+)$/.exec(line))) metrics[r[1]] = Number(r[2]);
        else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) checks[r[1]] = r[2];
        else if ((r = /^BENCH-SHOT (\S+)$/.exec(line))) shot.take(page, r[1]);
        else if ((r = /^BENCH-FAIL (.*)$/.exec(line))) error ??= r[1];
        else if (line === "BENCH-END") ended.v = true;
        else if (/sake: call to .*which we neither implement/.test(line)) error ??= line.trim();
        else if (!ended.v && /ゲームが終了しました|[Uu]nhandled exception|\[crash\]/.test(line)) error ??= `ended early: ${line.trim()}`;
        else continue;
        wake();
      }
    });
    page.on("pageerror", (e) => ((error ??= `page error: ${e.message}`), wake()));
    page.on("crash", () => ((error ??= "the page crashed"), wake()));
    // the next event that matches
    let seen = 0;
    const next = async (ok, what) => {
      for (;;) {
        while (seen < events.length) {
          const e = events[seen++];
          if (ok(e)) return e;
        }
        if (error) throw new Error(error);
        if (Date.now() > deadline) throw new Error(`timed out waiting for ${what}`);
        await new Promise((r) => ((wake = r), setTimeout(r, 500)));
      }
    };
    const click = async (x, y) => {
      await page.evaluate(([x, y]) => {
        const m = window.vnvmSake, p = m._malloc(16) >>> 0;
        m._vnhost_display_view(p);
        const v = Array.from(new Int32Array(m.wasmMemory.buffer, p, 4));
        m._free(p);
        const X = v[0] + x, Y = v[1] + y;
        m._vnhost_input_push(1, X, Y, 0), m._vnhost_input_push(2, X, Y, 0);
        setTimeout(() => m._vnhost_input_push(3, X, Y, 0), 50);
      }, [x, y]);
      return Date.now();
    };
    let peak = rssMb();
    const sampler = setInterval(() => (peak = Math.max(peak, rssMb())), 250);
    const t0 = Date.now();
    await page.goto(url(c.args));
    const title = await next((e) => e.kind === "state" && e.what === "title", "the title");
    metrics["cold.startup"] = title.t - t0;
    const gpuTotal = () => page.evaluate(() => window.vnvmSake?._vnhost_gpu_total?.() ?? -1).catch(() => -1);
    const gpu0 = await gpuTotal(), play0 = Date.now();
    const lines = [], scenes = [];
    for (;;) {
      const w = await next((e) => e.kind === "wait" || (e.kind === "state" && e.what === "end"), "the game to wait");
      if (w.kind === "state") break;
      if (w.what === "idle") {
        const c0 = cpuMs(), th0 = cpuThreads(), i0 = Date.now();
        await page.waitForTimeout(3000);
        if (linux) {
          const dt = Date.now() - i0, th1 = cpuThreads(), per = (v) => (v / dt) * 1000;
          metrics.idle_cpu = per(cpuMs() - c0);
          // (by process and thread kind: idle_cpu.renderer.workers is the game's own threads,
          // gpu-process.swiftshader the software GPU drawing what the game presents)
          const d = Object.fromEntries(Object.keys(th1).map((k) => [k, th1[k] - (th0[k] ?? 0)]));
          // (always the same names, 0 when there was none: every round must have each metric)
          const sum = (re) => per(Object.entries(d).filter(([k]) => !k.startsWith("thread:") && re.test(k)).reduce((s, [, v]) => s + v, 0));
          metrics["idle_cpu.workers"] = sum(/^renderer\.workers$/);
          metrics["idle_cpu.renderer_main"] = sum(/^renderer\.main$/);
          metrics["idle_cpu.renderer_other"] = sum(/^renderer\.(?!workers$|main$)/);
          metrics["idle_cpu.gpu"] = sum(/^gpu-process\./);
          metrics["idle_cpu.browser"] = sum(/^browser\./);
          metrics["idle_cpu.other"] = sum(/^(?!renderer\.|gpu-process\.|browser\.)/);
          const top = Object.entries(d).filter(([k, v]) => k.startsWith("thread:") && v > 0).sort((a, b) => b[1] - a[1]).slice(0, 12);
          log.push(`idle cpu by thread (ms/s): ${top.map(([k, v]) => `${k.slice(7)} ${per(v).toFixed(0)}`).join(", ")}`);
          console.log(`  ${c.name ?? c.exe} ${build.name}: ${log.at(-1)}`);
        }
      } else await page.waitForTimeout(100); // (a player's click is not instant either)
      const at = await click(640, w.what === "choice" ? 430 : 600);
      const r = await next((e) => e.kind === "state" && e.step > w.step, "the click's answer");
      if (r.what === "end") break;
      if (w.what === "choice") metrics.choice = r.t - at;
      else if (r.what === "line") lines.push(r.t - at);
      else if (r.what === "scene") scenes.push(r.t - at);
      // (the KiriKiri game's heavy effects, each once: click -> the effect done)
      else if (r.what.startsWith("fx_")) metrics[r.what] = Math.max(metrics[r.what] ?? 0, r.t - at);
      // (after the movie the game goes on by itself: the next wait comes after its scene)
      if (r.what === "saved") {
        // the save must reach the browser's storage before the page goes (web/src/saves.ts
        // stores every 5 s what changed): what is stored changes after the save, or a store came
        // in between (a store found from before, of the game's persistent data, is not enough)
        const stored = () =>
          page.evaluate(async () => {
            try {
              const d = await (await navigator.storage.getDirectory()).getDirectoryHandle("saves");
              const out = [];
              for await (const [n, h] of d.entries()) {
                const f = h.kind === "file" ? await h.getFile() : null;
                out.push(`${n}:${f?.size}:${f?.lastModified}`);
              }
              return out.sort().join(",");
            } catch {}
            return "";
          });
        const before = await stored();
        const until = Date.now() + 30000, settled = Date.now() + 7000;
        for (;;) {
          const now = await stored();
          if ((now && now !== before) || (now && Date.now() > settled) || Date.now() > until) break;
          await page.waitForTimeout(250);
        }
      }
    }
    clearInterval(sampler);
    const gpu1 = await gpuTotal();
    if (gpu0 >= 0 && gpu1 > gpu0) metrics.gpu_busy = ((gpu1 - gpu0) / (Date.now() - play0)) * 1000;
    metrics.line_p50 = pct(lines, 0.5);
    metrics.line_p95 = pct(lines, 0.95);
    metrics.scene_p50 = pct(scenes, 0.5);
    metrics.scene_max = Math.max(...scenes);
    if (linux) metrics.peak_rss_mb = peak;
    metrics.chunks = chunks;
    metrics.chunk_mb = chunkBytes / 1048576;
    if (!ended.v) await next(() => false, "the end").catch(() => {});
    await shot.settle();
    await page.close();

    // ---- warm: the save loaded in a new page of the same context ----
    error = undefined;
    deadline = Date.now() + timeout;
    const page2 = await context.newPage();
    let restored, end2 = false;
    const got = new Promise((resolve) => {
      page2.on("console", (m) => {
        for (const line of m.text().split("\n").map((l) => l.replace(/^\d\d:\d\d:\d\d /, ""))) {
          let r;
          if (log.length < 600) log.push(`load: ${line}`);
          if (/^BENCH-STATE restored /.test(line)) (restored = Date.now()), resolve();
          else if (line === "BENCH-END") end2 = true;
          else if (/ゲームが終了しました|[Uu]nhandled exception|\[crash\]|sake: call to .*which we neither implement/.test(line) && !restored)
            (error ??= `load: ended early: ${line.trim()}`), resolve();
          else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) checks[r[1]] = r[2];
          else if ((r = /^BENCH-SHOT (\S+)$/.exec(line))) shot.take(page2, r[1]);
          else if ((r = /^BENCH-FAIL (.*)$/.exec(line))) (error ??= `load: ${r[1]}`), resolve();
        }
      });
      page2.on("crash", () => ((error ??= "load: the page crashed"), resolve()));
      setTimeout(resolve, Math.max(1000, deadline - Date.now()));
    });
    const t1 = Date.now();
    await page2.goto(url(c.load ?? "load"));
    await got;
    if (!restored) error ??= `load: the save was not restored (${Math.round((Date.now() - t1) / 1000)} s)`;
    else metrics["warm.restore"] = restored - t1;
    // (the restored screen's picture: the game ends after it)
    if (restored && shot.expects()) for (const until = Date.now() + 15000; !end2 && Date.now() < until; ) await page2.waitForTimeout(100);
    const why = await shot.finish();
    if (why) error ??= why;
    await page2.close();

    // ---- starts with what the browser kept: everything (HTTP cache and the store's chunks in
    // Cache Storage), then the chunks only (HTTP cache emptied: the page, sake.wasm, /jit/); after
    // the load, so that they can't touch the save it loads ----
    if (!error) {
      metrics["start.warm"] = await toTitle(context, "", "start.warm");
      const cdp = await context.newCDPSession(await context.newPage());
      await cdp.send("Network.clearBrowserCache");
      metrics["start.chunks_kept"] = await toTitle(context, "", "start.chunks_kept");
    }

    // ---- the first start of all: a new context (nothing kept) and no saved translations (&jit=0:
    // the game translates all its code as it runs, as on a server that never ran it) ----
    if (!error) {
      const fresh = await browser.newContext({ viewport: { width: 1280, height: 720 } });
      try {
        metrics["start.nothing"] = await toTitle(fresh, "&jit=0", "start.nothing");
      } finally {
        await fresh.close().catch(() => {});
      }
    }
  } catch (e) {
    error ??= `${e.message ?? e}`;
  }
  await context.close().catch(() => {});
  for (const k of Object.keys(metrics)) if (metrics[k] === undefined || Number.isNaN(metrics[k])) delete metrics[k];
  return { metrics, checks, error, log: error ? log.slice(-40) : undefined };
}
