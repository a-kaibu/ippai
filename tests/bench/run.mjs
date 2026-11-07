// Runs benchmark groups (tests/bench/suite.mjs) on one or more builds of ippai in headless
// Chromium and writes what they measured as JSON. With two builds (this commit and the one before
// it) the runs alternate between them, round after round, on the same machine: a slow runner
// slows both the same, so their ratio says whether the commit made things faster or slower.
//
//   node tests/bench/run.mjs OUT.json GROUP [GROUP ...]
//   env: BUILDS="new=http://127.0.0.1:8091 old=http://127.0.0.1:8092" (name=ippai serve URL;
//          default "new=http://127.0.0.1:8090"), ROUNDS (measured rounds, default 3 or the
//          group's MIN_ROUNDS; one more
//          before them warms the JIT's saved translations and is not counted), SCALE (work per
//          case, default 1), ONLY="exe:case ..." (just those), TIMEOUT (s per run, default 240),
//          PLAYWRIGHT (the playwright module), CHROME_ARGS (more browser flags)
//
// Pages: play.html?autostart&headless with the bench tree mounted, IPPAI_NOPACE (frames as fast as
// the guest makes them) and Chromium's frame rate unlimited (no vsync), so frame times are what
// the build can do rather than 16.7 ms.
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
import { writeFileSync } from "node:fs";
import { GROUPS, MIN_ROUNDS, VERSION } from "./suite.mjs";
import { runScenario } from "./scenario.mjs";

const [out, ...groups] = process.argv.slice(2);
if (!out || !groups.length) {
  console.error(`usage: run.mjs OUT.json GROUP...  (groups: ${Object.keys(GROUPS).join(" ")})`);
  process.exit(2);
}
const builds = (process.env.BUILDS ?? "new=http://127.0.0.1:8090").split(/\s+/).filter(Boolean).map((b) => {
  const i = b.indexOf("=");
  return { name: b.slice(0, i), url: b.slice(i + 1) };
});
const ROUNDS = Number(process.env.ROUNDS || Math.max(3, ...groups.map((g) => MIN_ROUNDS[g] ?? 0)));
const SCALE = process.env.SCALE ?? "1";
const TIMEOUT = Number(process.env.TIMEOUT ?? 240) * 1000;
const only = (process.env.ONLY ?? "").split(/\s+/).filter(Boolean);

const todo = groups.flatMap((g) => {
  if (!GROUPS[g]) throw new Error(`no group ${g}`);
  return GROUPS[g].map((c) => ({ ...c, group: g }));
}).filter((c) => !only.length || only.includes(`${c.exe}:${c.args}`) || only.includes(c.exe));

// (the scenario plays paced, as players play: a browser with its display's rate; the rest
// unlimited)
const browserArgs = (unpaced) => [
  "--enable-unsafe-webgpu",
  // (timestamp-query's times as they are, not rounded to 100 µs)
  "--enable-webgpu-developer-features",
  ...(unpaced ? ["--disable-frame-rate-limit", "--disable-gpu-vsync"] : []),
  // (a page in the background would get its timers and frames throttled)
  "--disable-background-timer-throttling",
  "--disable-renderer-backgrounding",
  "--disable-backgrounding-occluded-windows",
  // (the scenario plays sound; clicks the harness sends are no user gesture)
  "--autoplay-policy=no-user-gesture-required",
  ...(process.env.CHROME_ARGS ?? "").split(" ").filter(Boolean),
];
const unpacedCases = todo.some((c) => !c.scenario), pacedCases = todo.some((c) => c.scenario);
const browser = unpacedCases ? await chromium.launch({ args: browserArgs(true) }) : null;
const pacedBrowser = pacedCases ? await chromium.launch({ args: browserArgs(false) }) : null;

/** One run of one case on one build: { metrics: {name: ms}, checks: {name: hex}, error? } */
async function runOnce(build, c) {
  const scale = c.format === "plain" ? "" : SCALE;
  const args = [c.args, c.args && scale].filter(Boolean).join(",");
  const url =
    `${build.url}/play.html?autostart&headless&env=IPPAI_NOPACE%3D1&mount=/vnvm/game=bench${c.page ? `&${c.page}` : ""}` +
    `${args ? `&args=${encodeURIComponent(args)}` : ""}&run=${encodeURIComponent(`Z:\\vnvm\\game\\${c.exe}`)}`;
  const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  const page = await context.newPage();
  const metrics = {}, checks = {}, log = [];
  const gpuAt = {}, pending = [];
  const gpuTotal = () => page.evaluate(() => window.vnvmSake?._vnhost_gpu_total?.() ?? -1).catch(() => -1);
  let nopace = false;
  let grew = false;
  let error, ended = false, resolve;
  const finished = new Promise((r) => (resolve = r));
  // (after the program's last line, the page's own "the game ended" and the like don't count)
  const done = () => ((ended = true), resolve());
  const t0 = Date.now();
  page.on("console", (m) => {
    const text = m.text();
    if (ended) return;
    if (log.length < 400) log.push(text);
    for (const line of text.split("\n")) {
      let r;
      if (line === "BENCH-HOST nopace") nopace = true;
      // (the GPU time of the timed frames, where the page has timestamp-query: "<case>.gpu", ms
      // per frame)
      else if ((r = /^BENCH-MARK (\S+)$/.exec(line))) gpuAt[r[1]] = gpuTotal();
      else if ((r = /^BENCH-FRAMES (\S+) (\d+)$/.exec(line))) {
        const [name, frames, at] = [r[1], +r[2], gpuAt[r[1]]];
        pending.push(gpuTotal().then(async (t) => {
          const t0 = await at;
          if (t0 >= 0 && t > t0) metrics[`${name}.gpu`] = (t - t0) / frames;
        }));
      }
      else if ((r = /^BENCH (\S+) ([-\d.]+)$/.exec(line))) metrics[r[1]] = Number(r[2]);
      else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) checks[r[1]] = r[2];
      else if ((r = /^BENCH-FAIL (.*)$/.exec(line))) return (error = r[1]), done();
      else if (line === "BENCH-END") return done();
      // (c.grows: the page's &grow= must have grown the memory, web/src/play.ts)
      else if ((r = /^sake memory: (\d+) -> (\d+) MiB$/.exec(line))) grew = +r[2] > +r[1];
      else if (c.format === "plain" && (r = /^(\w+)\s+([\d.]+) ms\s+([0-9a-f]{8})$/.exec(line.trim()))) {
        metrics[r[1]] = Number(r[2]);
        checks[r[1]] = r[3];
        if (r[1] === "total") return done();
      } else if (/sake: call to .*which we neither implement/.test(line)) error ??= line.trim();
      else if (/ゲームが終了しました|[Uu]nhandled exception|\[crash\]/.test(line)) return (error ??= `ended early: ${line.trim()}`), done();
    }
  });
  page.on("pageerror", (e) => ended || ((error ??= `page error: ${e.message}`), done()));
  page.on("crash", () => ended || ((error ??= "the page crashed"), done()));
  const timer = setTimeout(() => ((error ??= `timed out after ${TIMEOUT / 1000} s`), done()), TIMEOUT);
  try {
    await page.goto(url);
    await finished;
    await Promise.all(pending);
  } catch (e) {
    error ??= `${e}`;
  }
  clearTimeout(timer);
  if (c.wall && !error) metrics[c.wall] = Date.now() - t0;
  await context.close().catch(() => {});
  // (a program that ended without its results is a failure too)
  if (!error && !Object.keys(metrics).length) error = "no results";
  if (!error && c.grows && !grew) error = "the memory did not grow (sake-grow.wasm)";
  return { metrics, checks, error, nopace, log: error ? log.slice(-40) : undefined };
}

const results = {
  version: VERSION,
  builds: builds.map((b) => b.name),
  rounds: ROUNDS,
  scale: SCALE,
  // (what ran: the report checks that every one of them has its numbers)
  cases: todo.map((c) => `${c.group}/${c.exe}:${c.args}`),
  env: { chromium: (browser ?? pacedBrowser).version(), node: process.version, runner: process.env.RUNNER_NAME ?? "" },
  runs: {},
  checks: {},
  errors: {},
  // builds left out of the comparison, and why
  incompatible: {},
  // checks that came out different from one run to the next of the same build
  unstable: {},
  // the metrics each case made (the report checks they have all their rounds)
  produced: {},
};
for (const b of builds) (results.runs[b.name] = {}), (results.checks[b.name] = {}), (results.errors[b.name] = {});
// (graphics groups: a build that keeps its 60 fps pacing measures the vsync, not the build)
const paced = todo.some((c) => !c.group.startsWith("cpu-") && !c.scenario);
const saidNopace = {};
let active = builds;
for (let round = 0; round <= ROUNDS; round++) {
  for (const c of todo) {
    const key = `${c.group}/${c.exe}:${c.args}`;
    // (alternating which build goes first: neither always gets the warmer machine)
    const order = round % 2 ? [...active].reverse() : active;
    for (const b of order) {
      if (results.errors[b.name][key]?.fatal) continue;
      const r = c.scenario ? await runScenario(pacedBrowser, b, c, TIMEOUT) : await runOnce(b, c);
      if (r.nopace) saidNopace[b.name] = true;
      const tag = `${round ? `round ${round}` : "warm-up"} ${b.name} ${key}`;
      if (r.error) {
        console.log(`${tag}: FAILED ${r.error}`);
        // (one failure is a hiccup; the second in a row means this build can't run the case)
        const prev = results.errors[b.name][key];
        results.errors[b.name][key] = { error: r.error, log: r.log, fatal: !!prev };
        continue;
      }
      // (a case that failed once and then ran was a hiccup)
      delete results.errors[b.name][key];
      console.log(`${tag}: ${Object.entries(r.metrics).map(([k, v]) => `${k} ${v.toFixed(2)}`).join(", ")}`);
      for (const [k, v] of Object.entries(r.checks)) {
        const name = `${c.group}/${k}`, had = results.checks[b.name][name];
        if (had !== undefined && had !== v) (results.unstable[b.name] ??= {})[name] = [...new Set([...(results.unstable[b.name][name] ?? [had]), v])];
        results.checks[b.name][name] = v;
      }
      if (!round) continue;
      for (const [k, v] of Object.entries(r.metrics)) {
        const name = `${c.group}/${c.name ?? c.exe.replace(/\.exe$/, "")}.${k}`;
        (results.runs[b.name][name] ??= []).push(v);
        ((results.produced[b.name] ??= {})[key] ??= []).includes(name) || results.produced[b.name][key].push(name);
      }
    }
  }
  if (round === 0 && paced) {
    // After the warm-up: every build must have said it runs unpaced (IPPAI_NOPACE). This one must;
    // a previous build that doesn't (from before the flag, or with another name) can't be
    // compared with it, so it is left out and the report compares with the history instead.
    for (const b of builds) {
      if (saidNopace[b.name]) continue;
      const why = "this build does not run frames unpaced (IPPAI_NOPACE): its frame times are its 60 fps pacing";
      console.log(`${b.name}: ${why}`);
      if (b === builds[0]) results.errors[b.name][`${groups.join("+")}/nopace`] = { error: why, fatal: true };
      else results.incompatible[b.name] = why;
    }
    active = builds.filter((b) => b === builds[0] || !results.incompatible[b.name]);
    for (const name of Object.keys(results.incompatible)) (results.runs[name] = {}), (results.checks[name] = {}), (results.errors[name] = {});
  }
}
await browser?.close();
await pacedBrowser?.close();
writeFileSync(out, JSON.stringify(results, null, 1));
console.log(`wrote ${out}`);
