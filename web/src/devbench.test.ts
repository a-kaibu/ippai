import { expect, test } from "vite-plus/test";
import {
  NATIVE,
  OutputParser,
  PROFILES,
  SUITE,
  VERSION,
  compareRows,
  deviceName,
  formatScore,
  geomean,
  median,
  parseRun,
  plainScale,
  scores,
  stepsOf,
  throttle,
  type BenchRun,
} from "./devbench";
import { fillSizes } from "./devbench-gpu";
// (the programs the suite names, as tests/bench/suite.mjs knows them)
import { GROUPS } from "../../tests/bench/suite.mjs";

const rng = (seed: number) => () => (seed = (seed * 1103515245 + 12345) >>> 0) / 2 ** 32;

test("profiles: each longer one holds the shorter, ids are unique, every program case exists", () => {
  const ids = SUITE.map((s) => s.id);
  expect(new Set(ids).size).toBe(ids.length);
  const [q, s, f] = PROFILES.map((p) => stepsOf(p.id).map((x) => x.id));
  expect(q!.every((id) => s!.includes(id)) && s!.every((id) => f!.includes(id))).toBe(true);
  expect(f!.length).toBe(SUITE.length);
  expect(q![0]).toBe("info");
  const known = new Set(
    Object.values(GROUPS)
      .flat()
      .map((c) => `${c.exe}:${c.args}`),
  );
  for (const st of SUITE.filter((x) => x.kind === "exe" && !x.plain)) {
    // (tests/bench/run.mjs adds the scale after the case: "case,1")
    expect(known.has(`${st.exe}:${st.args!.split(",")[0]}`), st.id).toBe(true);
  }
  for (const st of SUITE) {
    if (st.kind === "exe") expect(st.exe, st.id).toBeTruthy();
    if (st.kind === "wasm" || st.kind === "sustain") expect(st.wasm && st.args, st.id).toBeTruthy();
  }
});

test("bench.exe's lines: per repeat, the x5 repeats kept apart, ends at total", () => {
  const p = new OutputParser(true, undefined, plainScale("2"));
  p.line("crc32      106.0 ms  1a2b3c4d");
  p.line("lz          60.0 ms  0000beef\nsort       240.0 ms  00000001");
  expect(p.done).toBe(false);
  p.line("total      826.0 ms  deadbeef");
  expect(p.done).toBe(true);
  expect(p.metrics).toEqual({ crc32: 53, lz: 30, sort: 120, total: 413 });
  expect(p.checks.crc32).toBe("1a2b3c4d");
  expect(p.error).toBeUndefined();
  // (after the end nothing more counts)
  p.line("crc32 1.0 ms 0");
  expect(p.metrics.crc32).toBe(53);
  const x5 = new OutputParser(true, undefined, 1);
  for (const ms of [10, 9, 8, 8, 8]) x5.line(`lz ${ms}.0 ms 0`);
  x5.line("total 43.0 ms 0");
  expect(x5.metrics).toEqual({ lz: 10, "lz#2": 9, "lz#3": 8, "lz#4": 8, "lz#5": 8, total: 43 });
  expect(plainScale("1,lz")).toBe(1);
  expect(plainScale("")).toBe(1);
  expect(plainScale("x")).toBe(1);
});

test("BENCH lines as run.mjs reads them, the GPU time between MARK and FRAMES per frame", () => {
  let gpu = 100;
  const p = new OutputParser(false, () => gpu);
  p.line("BENCH-HOST nopace");
  p.line("BENCH-MARK vn");
  gpu = 160;
  p.line("BENCH-FRAMES vn 30");
  p.line("BENCH vn 4.25");
  p.line("BENCH-CHECK vn.frame 0badf00d");
  expect(p.done).toBe(false);
  p.line("BENCH-END");
  expect(p.done).toBe(true);
  expect(p.metrics).toEqual({ "vn.gpu": 2, vn: 4.25 });
  expect(p.checks).toEqual({ "vn.frame": "0badf00d" });
  // no timestamp-query: no .gpu
  const n = new OutputParser(false, () => -1);
  n.line("BENCH-MARK a\nBENCH-FRAMES a 10\nBENCH a 1\nBENCH-END");
  expect(n.metrics).toEqual({ a: 1 });
  // (not plain: bench.exe's format is just a line)
  n.line("crc32 1.0 ms 0");
  expect(n.metrics).toEqual({ a: 1 });
});

test("failures: BENCH-FAIL, a crash, an unimplemented call, the program ending without results", () => {
  const f = new OutputParser(false);
  f.line("BENCH a 1");
  f.line("BENCH-FAIL frame of one color");
  expect([f.done, f.error]).toEqual([true, "frame of one color"]);
  const c = new OutputParser(false);
  c.line("Unhandled exception c0000005 at 00401000");
  expect(c.done && c.error?.startsWith("ended early")).toBe(true);
  const u = new OutputParser(false);
  u.line("sake: call to d3d9.Foo which we neither implement nor forward");
  u.line("BENCH a 1");
  u.line("BENCH-END");
  expect(u.done && u.error?.includes("neither implement")).toBe(true);
  const e = new OutputParser(false);
  e.end();
  expect(e.error).toBe("no results");
  const t = new OutputParser(false);
  t.line("BENCH a 2");
  t.end("300 秒で終わりませんでした");
  expect([t.metrics.a, t.error]).toEqual([2, "300 秒で終わりませんでした"]);
});

test("property: how the output is cut into print calls changes nothing", () => {
  const lines = [
    "BENCH-MARK x",
    "BENCH x 1.5",
    "BENCH-FRAMES x 3",
    "BENCH y 2",
    "BENCH-CHECK x.frame ab",
    "noise line",
    "BENCH-END",
  ];
  const want = new OutputParser(false, () => 7);
  for (const l of lines) want.line(l);
  const r = rng(7);
  for (let k = 0; k < 300; k++) {
    const p = new OutputParser(false, () => 7);
    let i = 0;
    while (i < lines.length) {
      const n = 1 + Math.floor(r() * 3);
      p.line(lines.slice(i, i + n).join("\n"));
      i += n;
    }
    expect([p.metrics, p.checks, p.done, p.error]).toEqual([
      want.metrics,
      want.checks,
      want.done,
      want.error,
    ]);
  }
});

test("median, geomean, throttle", () => {
  expect(median([3, 1, 2])).toBe(2);
  expect(median([4, 1, 3, 2])).toBe(2.5);
  expect(median([])).toBeNaN();
  expect(geomean([1, 4])).toBeCloseTo(2);
  expect(geomean([NaN, 0, 9])).toBeCloseTo(9);
  expect(throttle([10, 10, 10, 10, 10, 10, 10, 10, 15, 15])).toBeCloseTo(1.5);
  expect(throttle([10, 10, 10])).toBeNaN();
  // property: a constant series never throttles; scaling the whole series changes nothing
  const r = rng(3);
  for (let k = 0; k < 200; k++) {
    const n = 4 + Math.floor(r() * 60);
    const xs = Array.from({ length: n }, () => 1 + r() * 100);
    const c = 1 + r() * 10;
    expect(throttle(xs.map(() => c))).toBe(1);
    expect(throttle(xs.map((x) => x * c))).toBeCloseTo(throttle(xs), 9);
  }
});

const device = (model: string) => ({
  ua: "Mozilla/5.0 (Linux; Android 14)",
  model,
  cores: 8,
  screen: "1080x2400",
  dpr: 2.625,
  viewport: "411x914",
  crossOriginIsolated: true,
});

function runOf(started: number, k: number, model = "Pixel 8"): BenchRun {
  const kernels = Object.fromEntries(Object.entries(NATIVE).map(([n, v]) => [n, v * k]));
  return {
    version: VERSION,
    profile: "quick",
    started,
    build: "",
    device: device(model),
    steps: {
      wasm: { metrics: kernels, wall_ms: 1 },
      jit2: {
        metrics: Object.fromEntries(Object.entries(kernels).map(([n, v]) => [n, v * 1.25])),
        wall_ms: 1,
      },
      "d3d9.vn": { metrics: { vn: 4, "vn.gpu": 1 }, wall_ms: 1 },
      "gdi.vn": { metrics: { vn: 9 }, wall_ms: 1 },
      gpu: { metrics: { "fill.mpix_s": 900 }, wall_ms: 1 },
      "cpu.script": { metrics: {}, error: "300 秒で終わりませんでした", wall_ms: 300000 },
    },
  };
}

test("scores: against native, the JIT's cost, frames as a geometric mean, missing steps as –", () => {
  const s = Object.fromEntries(scores(runOf(1, 2)).map((x) => [x.label, x]));
  expect(s["CPU 素の速さ (wasm)"]!.value).toBeCloseTo(2);
  expect(s["x86 実行 (JIT)"]!.value).toBeCloseTo(2.5);
  expect(s["JIT ÷ wasm"]!.value).toBeCloseTo(1.25);
  expect(s["描画 1フレーム"]!.value).toBeCloseTo(6);
  expect(s["WebGPU 塗り"]!.value).toBe(900);
  expect(formatScore(s["発熱による低下"]!)).toBe("–");
  expect(formatScore(s["x86 実行 (JIT)"]!)).toBe("2.50x");
});

test("comparison: this run first, then the newest others of the same version", () => {
  const me = runOf(5000, 2, "Pixel 8");
  const old = { ...runOf(1000, 4, "Galaxy A23"), version: VERSION - 1 };
  const history = [runOf(2000, 3, "Galaxy A54"), runOf(3000, 1.5, "Xperia 1"), old, me];
  const rows = compareRows(me, history);
  expect(rows[0]!.length).toBe(4);
  expect(rows[0]![1]).toBe("この端末");
  expect(rows[0]![2]).toContain("Xperia 1");
  expect(rows[0]![3]).toContain("Galaxy A54");
  expect(rows.flat().join(" ")).not.toContain("Galaxy A23");
  const jit = rows.find((r) => r[0] === "x86 実行 (JIT)")!;
  expect(jit.slice(1)).toEqual(["2.50x", "1.88x", "3.75x"]);
  expect(rows.some((r) => r[0] === "D3D9 vn vn" && r[1] === "4.0")).toBe(true);
  expect(rows.some((r) => r[0] === "CPU script" && r[1]!.startsWith("失敗"))).toBe(true);
  expect(compareRows(me, [], 4)[0]).toEqual(["", "この端末"]);
});

test("device names and stored lines", () => {
  expect(deviceName(device("SM-S918B"))).toBe("SM-S918B (8コア)");
  expect(
    deviceName({
      ...device(""),
      gpu: {
        vendor: "qualcomm",
        architecture: "adreno-7xx",
        device: "",
        description: "",
        features: [],
        maxTexture: 8192,
      },
    }),
  ).toBe("Android (8コア, qualcomm)");
  const r = runOf(1, 1);
  expect(parseRun(JSON.stringify({ ...r, received: 5 }))?.started).toBe(1);
  expect(parseRun("{}")).toBeNull();
  expect(parseRun("not json")).toBeNull();
  expect(parseRun('{"i":1}')).toBeNull();
});

test("fill sizes: 720p, 1080p and the canvas's device pixels (landscape), no repeats, within the GPU's limit", () => {
  expect(fillSizes(411, 914, 2.625, 8192)).toEqual([
    [1280, 720],
    [1920, 1080],
    [2399, 1079],
  ]);
  expect(fillSizes(1280, 720, 1, 8192)).toEqual([
    [1280, 720],
    [1920, 1080],
  ]);
  expect(fillSizes(3000, 2000, 3, 4096)).toEqual([
    [1280, 720],
    [1920, 1080],
    [4096, 4096],
  ]);
});
