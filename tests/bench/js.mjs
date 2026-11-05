// The same game-like kernels compiled straight to wasm (no x86, no sake: tests/bench/js.sh
// builds them), run in JavaScript engines' shells: V8 (node) and JavaScriptCore (Safari's
// engine). Not optimized for either; it shows how the engines compare on this kind of wasm, and
// against the cpu-game numbers it shows what the x86 translation costs.
//
//   node tests/bench/js.mjs OUT.json DIR ENGINE=command [ENGINE=command ...]
//   (DIR has cpu.js and bench.js; ROUNDS measured rounds, default 3, after one warm-up)
import { execFileSync } from "node:child_process";
import { writeFileSync } from "node:fs";

const [out, dir, ...engineArgs] = process.argv.slice(2);
const engines = engineArgs.map((e) => [e.slice(0, e.indexOf("=")), e.slice(e.indexOf("=") + 1)]);
const ROUNDS = Number(process.env.ROUNDS ?? 3);
const CASES = ["script", "huffman", "unfilter", "yuv", "mix", "compose", "compose_sse", "transform"];

function run(cmd, file, args) {
  // (node takes the program's arguments after the file, JSC's shell after "--")
  const argv = cmd === "node" ? [file, ...args] : [file, "--", ...args];
  return execFileSync(cmd, argv, { encoding: "utf8", timeout: 600000, maxBuffer: 1 << 24 });
}

const results = { builds: ["new"], rounds: ROUNDS, runs: { new: {} }, checks: { new: {} }, errors: { new: {} } };
for (let round = 0; round <= ROUNDS; round++) {
  for (const [engine, cmd] of engines) {
    const jobs = [
      ...CASES.map((c) => ({ file: `${dir}/cpu.js`, args: [c, "1"], prefix: "cpu" })),
      { file: `${dir}/bench.js`, args: ["2"], prefix: "bench" },
    ];
    for (const j of jobs) {
      const key = `js/${engine}.${j.prefix}${j.args[0] && j.prefix === "cpu" ? `:${j.args[0]}` : ""}`;
      let text;
      try {
        text = run(cmd, j.file, j.args);
      } catch (e) {
        console.log(`${engine} ${j.prefix} ${j.args.join(" ")}: FAILED ${e.message.split("\n")[0]}`);
        results.errors.new[key] = { error: `${e.message}`.slice(0, 2000) };
        continue;
      }
      const metrics = {};
      for (const line of text.split("\n")) {
        let r;
        if ((r = /^BENCH (\S+) ([-\d.]+)$/.exec(line))) metrics[`${j.prefix}.${r[1]}`] = Number(r[2]);
        else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) results.checks.new[`js/${engine}.${j.prefix}.${r[1]}`] = r[2];
        else if ((r = /^(\w+)\s+([\d.]+) ms\s+([0-9a-f]{8})$/.exec(line.trim()))) {
          metrics[`${j.prefix}.${r[1]}`] = Number(r[2]);
          results.checks.new[`js/${engine}.${j.prefix}.${r[1]}`] = r[3];
        }
      }
      console.log(`${round ? `round ${round}` : "warm-up"} ${engine}: ${Object.entries(metrics).map(([k, v]) => `${k} ${v.toFixed(2)}`).join(", ")}`);
      if (!round) continue;
      for (const [k, v] of Object.entries(metrics)) (results.runs.new[`js/${engine}.${k}`] ??= []).push(v);
    }
  }
}
writeFileSync(out, JSON.stringify(results, null, 1));
console.log(`wrote ${out}`);
