// The device benchmark's suite and arithmetic (web/bench.html runs it, web/src/bench.ts):
// which steps a profile runs, how a program's output becomes numbers, the summary scores and the
// comparison with other devices' results. Pure: no DOM, no network (devbench.test.ts).
//
// Android phones differ a lot (cores, big.LITTLE, GPU, memory, heat), so a run records what the
// device is (DeviceInfo) next to every number, and the steps are chosen so a slow phone still
// finishes: a step that takes too long is recorded as such and the suite goes on.

/** What a device run measured, as `PUT /bench-log` stores it (one JSON line per run). */
export interface BenchRun {
  /** VERSION below: runs of different versions aren't compared */
  version: number;
  profile: Profile;
  /** ms since 1970 when the run started */
  started: number;
  /** the build of the page (web/src/runtime.ts's sake version, when known) */
  build: string;
  device: DeviceInfo;
  /** step id -> what it gave */
  steps: Record<string, StepResult>;
}

export interface StepResult {
  /** metric -> ms (lower is better), or another unit the metric's name says (`.mpix_s`, `.mb_s`) */
  metrics: Record<string, number>;
  /** metric -> checksum the program printed */
  checks?: Record<string, string>;
  error?: string;
  /** the step's whole time, page load to its last number */
  wall_ms: number;
}

export interface DeviceInfo {
  ua: string;
  /** navigator.userAgentData's high-entropy values (Chrome: model, platformVersion, architecture) */
  model?: string;
  platform?: string;
  platformVersion?: string;
  arch?: string;
  browser?: string;
  cores: number;
  /** navigator.deviceMemory (GB, rounded by the browser; Chrome caps it at 8) */
  memoryGb?: number;
  /** the JS heap limit (performance.memory, Chrome), MB */
  heapLimitMb?: number;
  screen: string;
  dpr: number;
  /** the page's size in CSS pixels */
  viewport: string;
  gpu?: {
    vendor: string;
    architecture: string;
    device: string;
    description: string;
    features: string[];
    maxTexture: number;
  };
  /** a 4 GB shared wasm memory could be made (sake.wasm; else sake-grow.wasm runs) */
  mem4g?: boolean;
  /** navigator.storage.estimate(), MB */
  storageQuotaMb?: number;
  connection?: string;
  battery?: { level: number; charging: boolean };
  crossOriginIsolated: boolean;
}

export type Profile = "quick" | "standard" | "full";

export interface Step {
  id: string;
  label: string;
  /** the shortest profile it is in */
  profile: Profile;
  kind: "info" | "gpu" | "storage" | "wasm" | "exe" | "micro" | "sustain";
  /** kind exe: the program in the bench tree, its arguments, `plain` = bench.exe's own format */
  exe?: string;
  args?: string;
  plain?: boolean;
  /** more play.html parameters (memory=grow) */
  page?: string;
  /** extra environment variable for sake */
  env?: string;
  /** kind wasm/sustain: benchwasm/<wasm>.js and bench.c's arguments */
  wasm?: string;
  /** kind micro: the export of MICRO */
  micro?: string;
  /** seconds before the step counts as too slow (default STEP_TIMEOUT) */
  timeout?: number;
}

/** Bump when the steps or what they measure change so that older numbers mean something else. */
export const VERSION = 1;

export const STEP_TIMEOUT = 300;

/** bench.exe 1 natively (Wine on the x86-64 dev machine, vnvm-wine docs/hangover.md), ms. */
export const NATIVE: Record<string, number> = {
  crc32: 53,
  lz: 30,
  blend: 6,
  memcpy: 155,
  sort: 123,
  float: 7,
  calls: 39,
  total: 413,
};

const exe = (profile: Profile, file: string, cases: string[], label: string): Step[] =>
  cases.map((c) => ({
    id: `${file.replace(/\.exe$/, "")}.${c}`,
    label: `${label} ${c}`,
    profile,
    kind: "exe",
    exe: file,
    args: `${c},1`,
  }));

/** The suite in order; a profile runs its own steps and those of the shorter ones. */
export const SUITE: Step[] = [
  { id: "info", label: "端末情報", profile: "quick", kind: "info" },
  { id: "gpu", label: "WebGPU 描画", profile: "quick", kind: "gpu" },
  { id: "wasm", label: "wasm 直接 (bench.c)", profile: "quick", kind: "wasm", wasm: "bench", args: "2" },
  {
    id: "jit1",
    label: "JIT bench.exe 1回目",
    profile: "quick",
    kind: "exe",
    exe: "bench.exe",
    args: "2",
    plain: true,
  },
  {
    id: "jit2",
    label: "JIT bench.exe 2回目",
    profile: "quick",
    kind: "exe",
    exe: "bench.exe",
    args: "2",
    plain: true,
  },
  ...exe("quick", "cpu.exe", ["script", "yuv", "compose"], "CPU"),
  ...exe("quick", "cxx.exe", ["map", "virtual"], "C++"),
  ...exe("quick", "gdi.exe", ["vn"], "GDI"),
  ...exe("quick", "d3d9.exe", ["vn", "sprites_batched"], "D3D9"),
  ...exe("quick", "d3d11.exe", ["vn"], "D3D11"),
  { id: "storage", label: "端末への保存 (OPFS)", profile: "standard", kind: "storage" },
  ...exe(
    "standard",
    "cpu.exe",
    [
      "huffman",
      "unfilter",
      "mix",
      "compose_sse",
      "transform",
      "heap",
      "strings",
      "api",
      "sync",
      "threads",
      "file",
    ],
    "CPU",
  ),
  ...exe("standard", "cxx.exe", ["string", "vector", "exceptions", "unordered"], "C++"),
  ...exe("standard", "gdi.exe", ["alphablend", "stretch", "text"], "GDI"),
  ...exe("standard", "ddraw.exe", ["vn", "lock"], "DirectDraw"),
  ...exe("standard", "d3d8.exe", ["vn"], "D3D8"),
  ...exe("standard", "d3d9.exe", ["fade", "rt", "upload"], "D3D9"),
  ...exe("standard", "d3d11.exe", ["fade", "sprites_batched"], "D3D11"),
  ...exe("standard", "gl.exe", ["vn", "fade"], "OpenGL"),
  ...exe("standard", "d2d.exe", ["vn", "text"], "Direct2D"),
  {
    id: "sustain",
    label: "連続負荷 60秒 (発熱)",
    profile: "standard",
    kind: "sustain",
    wasm: "bench",
    args: "1,lz",
    timeout: 200,
  },
  {
    id: "wasm_shared",
    label: "wasm 直接 共有メモリ",
    profile: "full",
    kind: "wasm",
    wasm: "shared",
    args: "2",
  },
  { id: "wasm_big", label: "wasm 直接 共有 2.1 GB", profile: "full", kind: "wasm", wasm: "big", args: "2" },
  {
    id: "jit_grow",
    label: "JIT bench.exe 伸びるメモリ",
    profile: "full",
    kind: "exe",
    exe: "bench.exe",
    args: "2",
    plain: true,
    page: "memory=grow",
  },
  ...["sort", "lz", "float", "memcpy"].map((k): Step => ({
    id: `jit_${k}_x5`,
    label: `JIT ${k} x5`,
    profile: "full",
    kind: "exe",
    exe: "bench.exe",
    args: `1,${k}`,
    plain: true,
  })),
  { id: "micro_copy", label: "memory.copy x5", profile: "full", kind: "micro", micro: "copy" },
  { id: "micro_loop", label: "i64 ループ x5", profile: "full", kind: "micro", micro: "loop" },
  ...exe("full", "cpu.exe", ["huffman"], "CPU").map((s) => ({
    ...s,
    id: "cpu.huffman_slots",
    label: "CPU huffman (stack slots)",
    env: "MASU_JIT_SLOTS=4",
  })),
];

const RANK: Record<Profile, number> = { quick: 0, standard: 1, full: 2 };

export const PROFILES: { id: Profile; label: string; detail: string }[] = [
  { id: "quick", label: "クイック", detail: "約3〜5分。CPU・JIT・描画の主な項目" },
  { id: "standard", label: "標準", detail: "約10〜20分。全ケースと発熱・保存の速さ" },
  { id: "full", label: "すべて", detail: "約20〜30分。開発用の細かい比較も" },
];

export function stepsOf(profile: Profile): Step[] {
  return SUITE.filter((s) => RANK[s.profile] <= RANK[profile]);
}

export function isProfile(p: unknown): p is Profile {
  return p === "quick" || p === "standard" || p === "full";
}

/**
 * Reads a program's output line by line the way tests/bench/run.mjs does: `BENCH <metric> <ms>`,
 * `BENCH-CHECK <metric> <hex>`, `BENCH-MARK`/`BENCH-FRAMES` (the GPU time between them),
 * `BENCH-FAIL`, `BENCH-END`, and bench.exe's own `<kernel> <ms> ms <crc>` (ending at `total`).
 */
export class OutputParser {
  metrics: Record<string, number> = {};
  checks: Record<string, string> = {};
  error?: string;
  done = false;
  /** BENCH-MARK name -> the GPU total then (gpu() at that line) */
  private marks: Record<string, number> = {};
  constructor(
    private readonly plain: boolean,
    /** the GPU time so far, ms (kura's vnhost_gpu_total), or < 0 where there is none */
    private readonly gpu: () => number = () => -1,
    /** bench.exe's arguments' scale: its kernels are reported per repeat */
    private readonly scale = 1,
  ) {}

  line(text: string): void {
    for (const raw of text.split("\n")) {
      if (this.done) return;
      const line = raw.trim();
      let r: RegExpExecArray | null;
      if ((r = /^BENCH-MARK (\S+)$/.exec(line))) this.marks[r[1]!] = this.gpu();
      else if ((r = /^BENCH-FRAMES (\S+) (\d+)$/.exec(line))) {
        const at = this.marks[r[1]!];
        const now = this.gpu();
        const frames = Number(r[2]);
        if (at !== undefined && at >= 0 && now > at && frames > 0)
          this.metrics[`${r[1]}.gpu`] = (now - at) / frames;
      } else if ((r = /^BENCH (\S+) ([-\d.]+)$/.exec(line))) this.metrics[r[1]!] = Number(r[2]);
      else if ((r = /^BENCH-CHECK (\S+) (\S+)$/.exec(line))) this.checks[r[1]!] = r[2]!;
      else if ((r = /^BENCH-FAIL (.*)$/.exec(line))) this.end(r[1]);
      else if (line === "BENCH-END") this.end();
      else if (this.plain && (r = /^(\w+)\s+([\d.]+) ms\s+([0-9a-f]{1,8})$/.exec(line))) {
        const name = r[1]!;
        const ms = Number(r[2]) / this.scale;
        // (the x5 steps print the same kernel 5 times: kept apart)
        let key = name;
        for (let k = 2; key in this.metrics; k++) key = `${name}#${k}`;
        this.metrics[key] = ms;
        this.checks[key] = r[3]!;
        if (name === "total") this.end();
      } else if (/sake: call to .*which we neither implement/.test(line)) this.error ??= line;
      else if (/[Uu]nhandled exception|\[crash\]/.test(line)) this.end(`ended early: ${line}`);
    }
  }

  /** The program ended (or the page gave up) without its last line. */
  end(error?: string): void {
    if (this.done) return;
    if (error) this.error ??= error;
    if (!error && !Object.keys(this.metrics).length) this.error ??= "no results";
    this.done = true;
  }
}

/** bench.exe's scale: its first argument (bench.c: the repeat count). */
export function plainScale(args: string | undefined): number {
  const n = Number((args ?? "").split(",")[0]);
  return n > 0 ? n : 1;
}

export function median(xs: number[]): number {
  const s = xs.filter((x) => Number.isFinite(x)).sort((a, b) => a - b);
  if (!s.length) return NaN;
  const m = s.length >> 1;
  return s.length % 2 ? s[m]! : (s[m - 1]! + s[m]!) / 2;
}

export function geomean(xs: number[]): number {
  const s = xs.filter((x) => Number.isFinite(x) && x > 0);
  if (!s.length) return NaN;
  return Math.exp(s.reduce((a, x) => a + Math.log(x), 0) / s.length);
}

/**
 * The sustained step's series (ms per round of the same work, in order): how much slower the
 * last rounds are than the first, as a ratio (1 = no slowdown; a hot phone lowers its clocks).
 */
export function throttle(series: number[]): number {
  if (series.length < 4) return NaN;
  const n = Math.max(2, Math.floor(series.length / 5));
  return median(series.slice(-n)) / median(series.slice(0, n));
}

export interface Score {
  label: string;
  value: number;
  /** how to show it */
  unit: "x" | "ms" | "mpix" | "mb" | "ratio";
  /** what it means, one line */
  note: string;
}

const KERNELS = ["crc32", "lz", "blend", "memcpy", "sort", "float", "calls"];

/** The few numbers a device is compared by (NaN where the run lacks the step). */
export function scores(run: BenchRun): Score[] {
  const m = (id: string) => run.steps[id]?.metrics ?? {};
  const vsNative = (id: string) => geomean(KERNELS.map((k) => (m(id)[k] ?? NaN) / NATIVE[k]!));
  const wasm = vsNative("wasm");
  const jit = vsNative("jit2");
  const frame = (ids: string[]) =>
    geomean(
      ids.flatMap((id) =>
        Object.entries(m(id))
          .filter(([k]) => !k.includes(".") && !k.includes("#"))
          .map(([, v]) => v),
      ),
    );
  const gpu = m("gpu");
  return [
    {
      label: "CPU 素の速さ (wasm)",
      value: wasm,
      unit: "x",
      note: "bench.c を直接 wasm で。開発機のネイティブ x86 の何倍の時間か",
    },
    {
      label: "x86 実行 (JIT)",
      value: jit,
      unit: "x",
      note: "bench.exe 2回目。ネイティブ x86 の何倍の時間か（目標 1.2〜1.3）",
    },
    { label: "JIT ÷ wasm", value: jit / wasm, unit: "ratio", note: "翻訳のコスト。端末の速さに依らないはず" },
    {
      label: "ゲームらしい CPU",
      value: frame(["cpu.script", "cpu.yuv", "cpu.compose", "cxx.map", "cxx.virtual"]),
      unit: "ms",
      note: "スクリプト VM・YUV・合成・C++ の幾何平均",
    },
    {
      label: "描画 1フレーム",
      value: frame(["gdi.vn", "d3d9.vn", "d3d9.sprites_batched", "d3d11.vn"]),
      unit: "ms",
      note: "GDI・D3D9・D3D11 の VN 画面の幾何平均",
    },
    {
      label: "WebGPU 塗り",
      value: gpu["fill.mpix_s"] ?? NaN,
      unit: "mpix",
      note: "1秒に塗れる画素（百万）。高いほど良い",
    },
    {
      label: "発熱による低下",
      value: m("sustain")["throttle"] ?? NaN,
      unit: "ratio",
      note: "60秒連続で最後の速さ ÷ 最初（1 = 低下なし）",
    },
    {
      label: "OPFS 読み",
      value: m("storage")["opfs_read.mb_s"] ?? NaN,
      unit: "mb",
      note: "端末に保存したゲームのランダム読み MB/s",
    },
  ];
}

export function formatScore(s: Score): string {
  if (!Number.isFinite(s.value)) return "–";
  switch (s.unit) {
    case "x":
      return `${s.value.toFixed(2)}x`;
    case "ratio":
      return s.value.toFixed(2);
    case "ms":
      return `${s.value.toFixed(1)} ms`;
    case "mpix":
      return `${s.value.toFixed(0)} M/s`;
    case "mb":
      return `${s.value.toFixed(0)} MB/s`;
  }
}

/** A short name for a device's column: its model, else its platform and core count. */
export function deviceName(d: DeviceInfo): string {
  const base =
    d.model ||
    d.platform ||
    (/Android/.test(d.ua) ? "Android" : /Mac/.test(d.ua) ? "Mac" : /Windows/.test(d.ua) ? "Windows" : "端末");
  return `${base} (${d.cores}コア${d.gpu?.vendor ? `, ${d.gpu.vendor}` : ""})`;
}

/**
 * The comparison table: one row per score and then per metric this run has, one column for this
 * run and one per other run (the latest `others` of the same VERSION, newest first).
 */
export function compareRows(run: BenchRun, history: BenchRun[], others = 4): string[][] {
  const past = history
    .filter((h) => h.version === run.version && h.started !== run.started)
    .sort((a, b) => b.started - a.started)
    .slice(0, others);
  const cols = [run, ...past];
  const head = [
    "",
    "この端末",
    ...past.map((h) => `${deviceName(h.device)} ${new Date(h.started).toISOString().slice(5, 10)}`),
  ];
  const rows: string[][] = [head];
  const all = cols.map(scores);
  all[0]!.forEach((s, i) => rows.push([s.label, ...all.map((c) => formatScore(c[i]!))]));
  for (const step of SUITE) {
    const mine = run.steps[step.id];
    if (!mine) continue;
    if (mine.error) rows.push([step.label, `失敗: ${mine.error}`.slice(0, 60), ...past.map(() => "")]);
    for (const k of Object.keys(mine.metrics)) {
      if (k.includes("#")) continue;
      const fmt = (r: BenchRun) => {
        const v = r.steps[step.id]?.metrics[k];
        return v === undefined ? "" : v >= 100 ? v.toFixed(0) : v.toFixed(1);
      };
      rows.push([`${step.label} ${k}`, ...cols.map(fmt)]);
    }
  }
  return rows;
}

/** One line of the server's bench-results.jsonl, if it is a device run. */
export function parseRun(line: string): BenchRun | null {
  try {
    const r = JSON.parse(line) as BenchRun;
    return r && typeof r === "object" && typeof r.version === "number" && r.device && r.steps ? r : null;
  } catch {
    return null;
  }
}
