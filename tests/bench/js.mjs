// The same game-like kernels compiled straight to wasm (no x86, no sake: tests/bench/js.sh
// builds them), run in JavaScript engines' shells: V8 (node) and JavaScriptCore (Safari's
// engine). Not optimized for either; it shows how the engines compare on this kind of wasm, and
// against the cpu-game numbers it shows what the x86 translation costs.
//
//   node tests/bench/js.mjs OUT.json DIR ENGINE=command [ENGINE=command ...]
//   (DIR has cpu.js, bench.js and dyn.js; ROUNDS measured rounds, default 3, after one warm-up;
//    VERSIONS="name=version ..." is recorded with the results)
// Exits 1 when a case fails or the engines computed different checksums (after writing OUT).
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { VERSION } from "./suite.mjs";

const [out, dir, ...engineArgs] = process.argv.slice(2);
const engines = engineArgs.map((e) => [e.slice(0, e.indexOf("=")), e.slice(e.indexOf("=") + 1)]);
const ROUNDS = Number(process.env.ROUNDS || 3);
const CASES = ["script", "huffman", "unfilter", "yuv", "mix", "compose", "compose_sse", "transform"];

function run(cmd, file, args) {
  // (node takes the program's arguments after the file, JSC's shell after "--")
  const argv = cmd === "node" ? [file, ...args] : [file, "--", ...args];
  return execFileSync(cmd, argv, { encoding: "utf8", timeout: 600000, maxBuffer: 1 << 24 });
}

const jobs = [
  ...CASES.map((c) => ({ file: "cpu.js", args: [c, "1"], name: `cpu:${c}` })),
  { file: "bench.js", args: ["2"], name: "bench" },
  // (the shape of what koji makes: modules compiled at run time, tables of functions)
  { file: "dyn.js", args: [], name: "dyn" },
];
const results = {
  version: VERSION,
  builds: ["new"],
  rounds: ROUNDS,
  cases: engines.flatMap(([engine]) => jobs.map((j) => `js/${engine}.${j.name}`)),
  produced: { new: {} },
  env: { node: process.version, ...Object.fromEntries((process.env.VERSIONS ?? "").split(/\s+/).filter(Boolean).map((v) => v.split("="))) },
  runs: { new: {} },
  checks: { new: {} },
  errors: { new: {} },
};
for (let round = 0; round <= ROUNDS; round++) {
  for (const [engine, cmd] of engines) {
    for (const j of jobs) {
      const key = `js/${engine}.${j.name}`;
      if (results.errors.new[key]) continue;
      const prefix = j.name.split(":")[0];
      let text;
      try {
        text = run(cmd, `${dir}/${j.file}`, j.args);
      } catch (e) {
        console.log(`${key}: FAILED ${e.message.split("\n")[0]}`);
        results.errors.new[key] = { error: `${e.message}`.slice(0, 2000) };
        continue;
      }
      const metrics = {}, checks = {};
      let ended = false;
      for (const line of text.split("\n")) {
        let r;
        if ((r = /^BENCH (\S+) ([-\d.]+)$/.exec(line))) metrics[`${prefix}.${r[1]}`] = Number(r[2]);
        else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) checks[`${prefix}.${r[1]}`] = r[2];
        else if (line === "BENCH-END") ended = true;
        else if ((r = /^(\w+)\s+([\d.]+) ms\s+([0-9a-f]{8})$/.exec(line.trim()))) {
          metrics[`${prefix}.${r[1]}`] = Number(r[2]);
          checks[`${prefix}.${r[1]}`] = r[3];
          if (r[1] === "total") ended = true;
        }
      }
      if (!ended || !Object.keys(metrics).length) {
        console.log(`${key}: FAILED no results\n${text.slice(-2000)}`);
        results.errors.new[key] = { error: `no results: ${text.slice(-500)}` };
        continue;
      }
      console.log(`${round ? `round ${round}` : "warm-up"} ${key}: ${Object.entries(metrics).map(([k, v]) => `${k} ${v.toFixed(2)}`).join(", ")}`);
      for (const [k, v] of Object.entries(checks)) results.checks.new[`js/${engine}.${k}`] = v;
      if (!round) continue;
      results.produced.new[key] = Object.keys(metrics).map((k) => `js/${engine}.${k}`);
      for (const [k, v] of Object.entries(metrics)) (results.runs.new[`js/${engine}.${k}`] ??= []).push(v);
    }
  }
}
// the engines ran the same wasm: they must have computed the same
const byName = {};
for (const [k, v] of Object.entries(results.checks.new)) (byName[k.slice(k.indexOf(".") + 1)] ??= {})[k.slice(3, k.indexOf("."))] = v;
for (const [name, per] of Object.entries(byName)) {
  if (new Set(Object.values(per)).size > 1) results.errors.new[`js/checks:${name}`] = { error: `the engines disagree: ${JSON.stringify(per)}` };
}
writeFileSync(out, JSON.stringify(results, null, 1));
console.log(`wrote ${out}`);
const failed = Object.keys(results.errors.new);
if (failed.length) {
  console.error(`failed: ${failed.join(" ")}`);
  process.exit(1);
}
