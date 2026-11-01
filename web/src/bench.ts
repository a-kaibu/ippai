// bench.exe (hangover/tests/bench.c) on a device, with everything needed to tell where the time
// goes, from one URL: /bench.html runs the steps of SUITE one after another (the page reloads
// itself between them; Wine can't start twice in a page) and ends with a table of all of them.
//
//   - bench.c compiled straight to wasm (hangover/tests/bench-wasm.sh): the device's own speed
//     with the same kernels, no x86 or Wine; also with shared memory, and shared and 2.1 GB
//     like wine.wasm
//   - bench.exe through Wine and the JIT, twice (the 2nd starts from the regions the 1st saved)
//   - single kernels 5 times in a row in one run: does the JIT's code get faster as V8 tiers up?
//   - the memcpy kernel's copies (4 MB there and back, 200 times) as wasm memory.copy and as an
//     i64 load/store loop, on shared memory: what the JIT's memcpy (memory.copy) can do
//
//   /bench.html?step=N     start at step N (later steps follow)
//   (other wine.html parameters pass through)
//
// Each step's result also goes to vnvm-server's `PUT /bench-log`, which prints it and appends it
// to bench-results.txt.

// native Wine on the x86-64 dev machine, bench.exe 1 (docs/hangover.md)
const NATIVE: Record<string, number> = {
  crc32: 53,
  lz: 30,
  blend: 6,
  memcpy: 155,
  sort: 123,
  float: 7,
  calls: 39,
  total: 413,
};
const KERNELS = Object.keys(NATIVE);

interface Step {
  label: string;
  /** benchwasm/<wasm>.js instead of Wine */
  wasm?: string;
  scale: number;
  only?: string;
  /** an export of MICRO instead (shown as memcpy) */
  micro?: string;
}

// (memory (import "e" "m") 256 256 shared), copy/loop(dst, src, n, repeats): memcpy(d, s, n),
// memmove(s + 1, d, n - 1) per repeat, by memory.copy / by i64 loads and stores
const MICRO = "AGFzbQEAAAABCAFgBH9/f38AAgsBAWUBbQIDgAKAAgMDAgAABw8CBGNvcHkAAARsb29wAAEKjwECKgADQCAAIAEgAvwKAAAgAUEBaiAAIAJBAWv8CgAAIANBAWshAyADDQALC2IBAX8DQEEAIQQDQCAAIARqIAEgBGopAwA3AwAgBEEIaiEEIAQgAkkNAAtBACEEA0AgAUEBaiAEaiAAIARqKQMANwMAIARBCGohBCAEIAJBCGtJDQALIANBAWshAyADDQALCw==";

const SUITE: Step[] = [
  { label: "wasm", wasm: "bench", scale: 3 },
  { label: "wasm shared", wasm: "shared", scale: 3 },
  { label: "wasm big", wasm: "big", scale: 3 },
  { label: "JIT 1", scale: 3 },
  { label: "JIT 2", scale: 3 },
  ...["sort", "lz", "float", "memcpy"].map((k) => ({ label: `JIT ${k} x5`, scale: 1, only: k })),
  { label: "memory.copy x5", scale: 1, micro: "copy" },
  { label: "i64 loop x5", scale: 1, micro: "loop" },
];

const params = new URLSearchParams(location.search);
const index = Math.min(SUITE.length - 1, Math.max(0, Number(params.get("step") ?? 0) || 0));
const step = SUITE[index]!;
const STORE = "vnvm-bench";

/** step label -> kernel -> ms per repeat (several for the x5 steps) */
type Results = Record<string, Record<string, number[]>>;
function loadResults(): Results {
  try {
    return index === 0 ? {} : (JSON.parse(sessionStorage.getItem(STORE) ?? "{}") as Results);
  } catch {
    return {};
  }
}
const results = loadResults();
const mine: Record<string, number[]> = (results[step.label] = {});

if (!step.wasm && !step.micro) {
  params.set("run", "C:\\bench.exe");
  params.set("args", step.only ? `${step.scale},${step.only}` : String(step.scale));
  params.set("autostart", "");
  history.replaceState(null, "", `?${params}`);
}

const table = document.querySelector<HTMLTableElement>("#result")!;
const env = document.querySelector<HTMLDivElement>("#env")!;
const copy = document.querySelector<HTMLButtonElement>("#copy")!;
const device = `${navigator.userAgent} / ${navigator.hardwareConcurrency} cores`;
const lines: string[] = [];
const t0 = performance.now();

/** All steps so far: one row per step, one column per kernel (ms per repeat). */
function render(): void {
  table.innerHTML = "";
  const head = table.insertRow();
  for (const k of ["", ...KERNELS]) head.insertCell().textContent = k;
  const native = table.insertRow();
  for (const v of ["native x86", ...KERNELS.map((k) => String(NATIVE[k]))]) native.insertCell().textContent = v;
  for (const s of SUITE) {
    const r = results[s.label];
    if (!r) continue;
    const row = table.insertRow();
    row.insertCell().textContent = s.label;
    for (const k of KERNELS) row.insertCell().textContent = r[k]?.map((v) => v.toFixed(0)).join(" / ") ?? "";
  }
  env.textContent = `${device} / step ${index + 1} of ${SUITE.length}: ${step.label}`;
}
render();

function onLine(line: string): void {
  const m = /^(\w+)\s+([\d.]+) ms\s+([0-9a-f]+)/.exec(line);
  if (!m) return;
  const [, name, ms, crc] = m as unknown as [string, string, string, string];
  const per = Number(ms) / step.scale;
  (mine[name] ??= []).push(per);
  lines.push(`${name} ${per.toFixed(1)} ${crc}`);
  render();
  if (name !== "total") return;
  lines.unshift(`${step.label} (step ${index + 1} of ${SUITE.length}, scale ${step.scale})`);
  lines.push(`(start to end ${((performance.now() - t0) / 1000).toFixed(1)} s)`, device);
  try {
    sessionStorage.setItem(STORE, JSON.stringify(results));
  } catch {
    // the table then shows this step only
  }
  void fetch("bench-log", { method: "PUT", body: lines.join("\n") })
    .catch(() => undefined)
    .then(() => {
      if (index + 1 >= SUITE.length) {
        env.textContent = `${device} / 完了`;
        copy.hidden = false;
        return;
      }
      const next = new URLSearchParams();
      next.set("step", String(index + 1));
      location.search = `?${next}`;
    });
}

addEventListener("wine-print", (e) => onLine((e as CustomEvent<string>).detail));
copy.addEventListener("click", () => {
  const text = [...table.rows].map((r) => [...r.cells].map((c) => c.textContent).join("\t")).join("\n");
  void navigator.clipboard.writeText(`${text}\n${device}`).catch(() => undefined);
});

if (step.micro) {
  document.querySelector<HTMLButtonElement>("#start")!.hidden = true;
  const memory = new WebAssembly.Memory({ initial: 256, maximum: 256, shared: true });
  const bytes = Uint8Array.from(atob(MICRO), (c) => c.charCodeAt(0));
  const run = (await WebAssembly.instantiate(bytes, { e: { m: memory } })).instance.exports[step.micro] as (
    d: number,
    s: number,
    n: number,
    r: number,
  ) => void;
  setTimeout(() => {
    let total = 0;
    for (let i = 0; i < 5; i++) {
      const t = performance.now();
      run(0, 6 << 20, 4 << 20, 200);
      const ms = performance.now() - t;
      total += ms;
      onLine(`memcpy ${ms.toFixed(1)} ms 0`);
    }
    onLine(`total ${total.toFixed(1)} ms 0`);
  }, 100);
} else if (step.wasm) {
  document.querySelector<HTMLButtonElement>("#start")!.hidden = true;
  const url = new URL(`benchwasm/${step.wasm}.js`, location.href).href;
  const factory = (await import(/* @vite-ignore */ url)).default as (m: object) => Promise<{
    callMain(args: string[]): void;
  }>;
  const m = await factory({ print: onLine, printErr: (l: string) => console.warn(l) });
  // after the page has painted, so the table is there while it runs
  setTimeout(() => m.callMain([String(step.scale)]), 100);
} else await import("./wine");

export {};
