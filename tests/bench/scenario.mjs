// The scenario benchmark's player (tests/bench/src/scenario.c, run.mjs runs it for cases with
// `scenario`): plays the game as a player would, in a fresh browser context (cold: no HTTP
// cache, no saved games), clicking whenever the game waits, then starts it again in the same
// context to load the save (warm: what the browser kept). The server's saved translations are
// warm in both (run.mjs's warm-up round made them).
//
// Metrics (ms unless named otherwise; lower is better):
//   cold.startup          page opened -> the title shown
//   line_p50, line_p95    click -> the next line's first frame (text advance)
//   scene_p50, scene_max  click -> a scene transition done (pictures read and decoded, crossfade)
//   choice                click -> the choice's next scene done
//   idle_cpu              the browser's CPU per second while the game waits for 3 s (ms/s)
//   gpu_busy              the GPU's time in render passes per second of play (ms/s; timestamp-query)
//   peak_rss_mb           the browser's processes' memory at most (MB)
//   chunks, chunk_mb      the store's chunks the page fetched while playing (count, MB)
//   warm.restore          page opened again -> the save restored and shown
// plus the game's own numbers (boot_to_title, frame_p50/p95/p99, fade_ms, movie_p95, movie_late).
import { readdirSync, readFileSync } from "node:fs";

const linux = process.platform === "linux";
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
const cpuMs = () =>
  browserProcs().reduce((s, pid) => {
    try {
      const f = readFileSync(`/proc/${pid}/stat`, "latin1").split(") ")[1].split(" ");
      return s + (Number(f[11]) + Number(f[12])) * 10; // (utime + stime, at 100 ticks/s)
    } catch {
      return s;
    }
  }, 0);
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
  const url = (args) =>
    `${build.url}/play.html?autostart&headless&mount=/vnvm/game=bench&args=${encodeURIComponent(args)}` +
    `&run=${encodeURIComponent(`Z:\\vnvm\\game\\${c.exe}`)}`;
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
      for (const line of m.text().split("\n")) {
        if (log.length < 400) log.push(line);
        let r;
        if ((r = /^BENCH-STATE (\S+) (\d+)$/.exec(line))) events.push({ kind: "state", what: r[1], step: +r[2], t: Date.now() });
        else if ((r = /^BENCH-WAIT (\d+)(?: (\S+))?$/.exec(line))) events.push({ kind: "wait", what: r[2] ?? "", step: +r[1], t: Date.now() });
        else if ((r = /^BENCH (\S+) ([-\d.]+)$/.exec(line))) metrics[r[1]] = Number(r[2]);
        else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) checks[r[1]] = r[2];
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
    await page.goto(url("play"));
    const title = await next((e) => e.kind === "state" && e.what === "title", "the title");
    metrics["cold.startup"] = title.t - t0;
    const gpuTotal = () => page.evaluate(() => window.vnvmSake?._vnhost_gpu_total?.() ?? -1).catch(() => -1);
    const gpu0 = await gpuTotal(), play0 = Date.now();
    const lines = [], scenes = [];
    for (;;) {
      const w = await next((e) => e.kind === "wait" || (e.kind === "state" && e.what === "end"), "the game to wait");
      if (w.kind === "state") break;
      if (w.what === "idle") {
        const c0 = cpuMs(), i0 = Date.now();
        await page.waitForTimeout(3000);
        if (linux) metrics.idle_cpu = ((cpuMs() - c0) / (Date.now() - i0)) * 1000;
      } else await page.waitForTimeout(100); // (a player's click is not instant either)
      const at = await click(640, w.what === "choice" ? 430 : 600);
      const r = await next((e) => e.kind === "state" && e.step > w.step, "the click's answer");
      if (w.what === "choice") metrics.choice = r.t - at;
      else if (r.what === "line") lines.push(r.t - at);
      else if (r.what === "scene") scenes.push(r.t - at);
      // (after the movie the game goes on by itself: the next wait comes after its scene)
      if (r.what === "saved") {
        // the save must reach the browser's storage before the page goes (web/src/saves.ts)
        const until = Date.now() + 30000;
        while (!(await page.evaluate(async () => {
          try {
            const d = await (await navigator.storage.getDirectory()).getDirectoryHandle("saves");
            for await (const _ of d.values()) return true;
          } catch {}
          return false;
        })) && Date.now() < until) await page.waitForTimeout(250);
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
    await page.close();

    // ---- warm: the save loaded in a new page of the same context ----
    error = undefined;
    deadline = Date.now() + timeout;
    const page2 = await context.newPage();
    let restored;
    const got = new Promise((resolve) => {
      page2.on("console", (m) => {
        for (const line of m.text().split("\n")) {
          let r;
          if (log.length < 600) log.push(`load: ${line}`);
          if (/^BENCH-STATE restored /.test(line)) (restored = Date.now()), resolve();
          else if (/ゲームが終了しました|[Uu]nhandled exception|\[crash\]|sake: call to .*which we neither implement/.test(line) && !restored)
            (error ??= `load: ended early: ${line.trim()}`), resolve();
          else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) checks[r[1]] = r[2];
          else if ((r = /^BENCH-FAIL (.*)$/.exec(line))) (error ??= `load: ${r[1]}`), resolve();
        }
      });
      page2.on("crash", () => ((error ??= "load: the page crashed"), resolve()));
      setTimeout(resolve, Math.max(1000, deadline - Date.now()));
    });
    const t1 = Date.now();
    await page2.goto(url("load"));
    await got;
    if (!restored) error ??= `load: the save was not restored (${Math.round((Date.now() - t1) / 1000)} s)`;
    else metrics["warm.restore"] = restored - t1;
  } catch (e) {
    error ??= `${e.message ?? e}`;
  }
  await context.close().catch(() => {});
  for (const k of Object.keys(metrics)) if (metrics[k] === undefined || Number.isNaN(metrics[k])) delete metrics[k];
  return { metrics, checks, error, log: error ? log.slice(-40) : undefined };
}
