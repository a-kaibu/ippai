// BENCH_PROFILE=DIR (tests/bench/scenario.mjs): a Chromium trace of the cold play with V8's CPU
// sampler on every thread of the renderer (the page, the game's pthread workers, the network
// worker, the audio worklet), written as DIR/trace.json (open in Perfetto / DevTools) and
// DIR/summary.txt: per thread, its busy time and the functions and scripts it spent it in (self
// time), so that the host's JavaScript (host/*.js inside sake.js, the page's assets) can be told
// from the guest's wasm.
import { mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";

// CPU time (ms) of every thread of every process now, by "pid:tid" (Linux: the trace's pid and
// tid are the system's): a sampled profile counts a thread blocked in Atomics.wait inside wasm
// as busy there, so what it really ran comes from here
function threadCpu() {
  const out = new Map();
  if (process.platform !== "linux") return out;
  for (const pid of readdirSync("/proc").filter((p) => /^\d+$/.test(p))) {
    let tids = [];
    try {
      tids = readdirSync(`/proc/${pid}/task`);
    } catch {
      continue;
    }
    for (const tid of tids) {
      try {
        const f = readFileSync(`/proc/${pid}/task/${tid}/stat`, "latin1");
        const r = f.slice(f.lastIndexOf(")") + 2).split(" ");
        out.set(`${pid}:${tid}`, (Number(r[11]) + Number(r[12])) * 10);
      } catch {}
    }
  }
  return out;
}

const CATEGORIES = ["devtools.timeline", "v8.execute", "disabled-by-default-v8.cpu_profiler", "disabled-by-default-devtools.timeline"];

/** Starts tracing (call before the page's goto); returns stop(): writes DIR. */
export async function profile(browser, page, dir) {
  mkdirSync(dir, { recursive: true });
  await browser.startTracing(page, { path: `${dir}/trace.json`, categories: CATEGORIES });
  const cpu0 = threadCpu();
  return async () => {
    const cpu1 = threadCpu();
    await browser.stopTracing();
    const cpu = new Map([...cpu1].map(([k, v]) => [k, v - (cpu0.get(k) ?? 0)]));
    writeFileSync(`${dir}/summary.txt`, summary(JSON.parse(readFileSync(`${dir}/trace.json`, "utf8")), cpu).join("\n") + "\n");
  };
}

/** Per thread: self time by script and by function, from the trace's Profile/ProfileChunk events. */
export function summary(trace, cpu = new Map()) {
  const events = trace.traceEvents ?? trace;
  const names = new Map();
  for (const e of events) if (e.ph === "M" && e.name === "thread_name") names.set(`${e.pid}:${e.tid}`, e.args.name);
  // a profile per id: its nodes (id -> callFrame) and samples with time deltas
  const profiles = new Map();
  for (const e of events) {
    if (e.name !== "Profile" && e.name !== "ProfileChunk") continue;
    const p = profiles.get(e.id) ?? { thread: `${e.pid}:${e.tid}`, nodes: new Map(), samples: [], deltas: [] };
    profiles.set(e.id, p);
    const d = e.args?.data ?? {};
    for (const n of d.cpuProfile?.nodes ?? []) p.nodes.set(n.id, n.callFrame);
    p.samples.push(...(d.cpuProfile?.samples ?? []));
    p.deltas.push(...(d.timeDeltas ?? []));
  }
  const out = [];
  const rows = [];
  for (const p of profiles.values()) {
    const fn = new Map(), file = new Map();
    let busy = 0, total = 0;
    for (let i = 0; i < p.samples.length; i++) {
      const us = Math.max(0, p.deltas[i + 1] ?? 0);
      total += us;
      const f = p.nodes.get(p.samples[i]);
      if (!f || f.functionName === "(idle)") continue;
      busy += us;
      const script = f.url ? f.url.replace(/^.*\//, "").replace(/\?.*$/, "") : f.functionName.startsWith("(") ? f.functionName : "(native)";
      const key = `${f.functionName || "(anonymous)"} ${script}:${(f.lineNumber ?? -1) + 1}`;
      fn.set(key, (fn.get(key) ?? 0) + us);
      file.set(script, (file.get(script) ?? 0) + us);
    }
    if (busy > 0) rows.push({ name: names.get(p.thread) ?? p.thread, cpu: cpu.get(p.thread), busy, total, fn, file });
  }
  rows.sort((a, b) => (b.cpu ?? b.busy / 1000) - (a.cpu ?? a.busy / 1000));
  const ms = (us) => (us / 1000).toFixed(0).padStart(7);
  const top = (m, n) => [...m].sort((a, b) => b[1] - a[1]).slice(0, n);
  const cpuText = (r) => (r.cpu === undefined ? "" : `, ${r.cpu} ms on the CPU`);
  out.push(`threads (ms on the CPU, else sampled busy): ${rows.map((r) => `${r.name} ${r.cpu ?? (r.busy / 1000).toFixed(0)}`).join(", ")}`);
  for (const r of rows) {
    out.push(`== ${r.name}: ${ms(r.busy)} ms sampled busy of ${ms(r.total)}${cpuText(r)}`);
    out.push(...top(r.file, 8).map(([k, v]) => `  script ${ms(v)} ${k}`));
    out.push(...top(r.fn, 25).map(([k, v]) => `  fn     ${ms(v)} ${k}`));
  }
  return out;
}

// node tests/bench/profile.mjs trace.json: the summary of a trace taken before
if (import.meta.url === `file://${process.argv[1]}`) console.log(summary(JSON.parse(readFileSync(process.argv[2], "utf8"))).join("\n"));
