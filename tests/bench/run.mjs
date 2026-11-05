// Runs benchmark groups (tests/bench/suite.mjs) on one or more builds of ippai in headless
// Chromium and writes what they measured as JSON. With two builds (this commit and the one before
// it) the runs alternate between them, round after round, on the same machine: a slow runner
// slows both the same, so their ratio says whether the commit made things faster or slower.
//
//   node tests/bench/run.mjs OUT.json GROUP [GROUP ...]
//   env: BUILDS="new=http://127.0.0.1:8091 old=http://127.0.0.1:8092" (name=ippai serve URL;
//          default "new=http://127.0.0.1:8090"), ROUNDS (measured rounds, default 3; one more
//          before them warms the JIT's saved translations and is not counted), SCALE (work per
//          case, default 1), ONLY="exe:case ..." (just those), TIMEOUT (s per run, default 240),
//          PLAYWRIGHT (the playwright module), CHROME_ARGS (more browser flags)
//
// Pages: play.html?autostart&headless with the bench tree mounted, VNVM_NOPACE (frames as fast as
// the guest makes them) and Chromium's frame rate unlimited (no vsync), so frame times are what
// the build can do rather than 16.7 ms.
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
import { writeFileSync } from "node:fs";
import { GROUPS } from "./suite.mjs";

const [out, ...groups] = process.argv.slice(2);
if (!out || !groups.length) {
  console.error(`usage: run.mjs OUT.json GROUP...  (groups: ${Object.keys(GROUPS).join(" ")})`);
  process.exit(2);
}
const builds = (process.env.BUILDS ?? "new=http://127.0.0.1:8090").split(/\s+/).filter(Boolean).map((b) => {
  const i = b.indexOf("=");
  return { name: b.slice(0, i), url: b.slice(i + 1) };
});
const ROUNDS = Number(process.env.ROUNDS ?? 3);
const SCALE = process.env.SCALE ?? "1";
const TIMEOUT = Number(process.env.TIMEOUT ?? 240) * 1000;
const only = (process.env.ONLY ?? "").split(/\s+/).filter(Boolean);

const todo = groups.flatMap((g) => {
  if (!GROUPS[g]) throw new Error(`no group ${g}`);
  return GROUPS[g].map((c) => ({ ...c, group: g }));
}).filter((c) => !only.length || only.includes(`${c.exe}:${c.args}`) || only.includes(c.exe));

const browser = await chromium.launch({
  args: [
    "--enable-unsafe-webgpu",
    "--disable-frame-rate-limit",
    "--disable-gpu-vsync",
    // (a page in the background would get its timers and frames throttled)
    "--disable-background-timer-throttling",
    "--disable-renderer-backgrounding",
    "--disable-backgrounding-occluded-windows",
    ...(process.env.CHROME_ARGS ?? "").split(" ").filter(Boolean),
  ],
});

/** One run of one case on one build: { metrics: {name: ms}, checks: {name: hex}, error? } */
async function runOnce(build, c) {
  const scale = c.format === "plain" ? "" : SCALE;
  const args = [c.args, c.args && scale].filter(Boolean).join(",");
  const url =
    `${build.url}/play.html?autostart&headless&env=VNVM_NOPACE%3D1&mount=/vnvm/game=bench` +
    `${args ? `&args=${encodeURIComponent(args)}` : ""}&run=${encodeURIComponent(`Z:\\vnvm\\game\\${c.exe}`)}`;
  const context = await browser.newContext({ viewport: { width: 1280, height: 720 } });
  const page = await context.newPage();
  const metrics = {}, checks = {}, log = [];
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
      if ((r = /^BENCH (\S+) ([-\d.]+)$/.exec(line))) metrics[r[1]] = Number(r[2]);
      else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) checks[r[1]] = r[2];
      else if ((r = /^BENCH-FAIL (.*)$/.exec(line))) return (error = r[1]), done();
      else if (line === "BENCH-END") return done();
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
  } catch (e) {
    error ??= `${e}`;
  }
  clearTimeout(timer);
  if (c.wall && !error) metrics[c.wall] = Date.now() - t0;
  await context.close().catch(() => {});
  // (a program that ended without its results is a failure too)
  if (!error && !Object.keys(metrics).length) error = "no results";
  return { metrics, checks, error, log: error ? log.slice(-40) : undefined };
}

const results = { builds: builds.map((b) => b.name), rounds: ROUNDS, scale: SCALE, runs: {}, checks: {}, errors: {} };
for (const b of builds) (results.runs[b.name] = {}), (results.checks[b.name] = {}), (results.errors[b.name] = {});
for (let round = 0; round <= ROUNDS; round++) {
  for (const c of todo) {
    const key = `${c.group}/${c.exe}:${c.args}`;
    // (alternating which build goes first: neither always gets the warmer machine)
    const order = round % 2 ? [...builds].reverse() : builds;
    for (const b of order) {
      if (results.errors[b.name][key]?.fatal) continue;
      const r = await runOnce(b, c);
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
      Object.assign(results.checks[b.name], Object.fromEntries(Object.entries(r.checks).map(([k, v]) => [`${c.group}/${k}`, v])));
      if (!round) continue;
      for (const [k, v] of Object.entries(r.metrics)) {
        const name = `${c.group}/${c.exe.replace(/\.exe$/, "")}.${k}`;
        (results.runs[b.name][name] ??= []).push(v);
      }
    }
  }
}
await browser.close();
writeFileSync(out, JSON.stringify(results, null, 1));
console.log(`wrote ${out}`);
