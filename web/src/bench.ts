// The device benchmark (/bench.html): measures a phone (or any browser) the way masu uses it,
// and keeps every device's results on the server so devices can be compared
// (web/src/devbench.ts has the suite and the arithmetic).
//
//   /bench.html                  the start screen: pick a profile, past results of all devices
//   /bench.html?profile=quick    start that profile at once (quick, standard, full)
//   ?step=N                      (the suite itself: the page reloads for every step, since sake
//                                can start only once in a page; the run so far is in sessionStorage)
//   (other play.html parameters pass through to the program steps, e.g. &headless for tests)
//
// Program steps run the bench tree's programs (tests/bench/src and bench.exe, packed as the store
// tree "bench": `masu bench install`) through sake and the JIT. Each run's results go to
// `PUT /bench-log` (masu serve keeps them in <jit>/bench-results.jsonl) when it ends, and
// `GET /bench-results` gives them all back for the comparison.

import { getUi, setBench, setStart, setUi } from "./ui/store";
import { renderShell } from "./ui/Shell";
import {
  OutputParser,
  PROFILES,
  STEP_TIMEOUT,
  VERSION,
  compareRows,
  deviceName,
  formatScore,
  isProfile,
  parseRun,
  plainScale,
  scores,
  stepsOf,
  throttle,
  type BenchRun,
  type DeviceInfo,
  type Profile,
  type Step,
  type StepResult,
} from "./devbench";

// (memory (import "e" "m") 256 256 shared), copy/loop(dst, src, n, repeats): memcpy(d, s, n),
// memmove(s + 1, d, n - 1) per repeat, by memory.copy / by i64 loads and stores
const MICRO =
  "AGFzbQEAAAABCAFgBH9/f38AAgsBAWUBbQIDgAKAAgMDAgAABw8CBGNvcHkAAARsb29wAAEKjwECKgADQCAAIAEgAvwKAAAgAUEBaiAAIAJBAWv8CgAAIANBAWshAyADDQALC2IBAX8DQEEAIQQDQCAAIARqIAEgBGopAwA3AwAgBEEIaiEEIAQgAkkNAAtBACEEA0AgAUEBaiAAIARqKQMANwMAIARBCGohBCAEIAJBCGtJDQALIANBAWshAyADDQALCw==";

const STORE = "masu-devbench";
const SUSTAIN_S = 60;

const params = new URLSearchParams(location.search);

function loadRun(): BenchRun | null {
  try {
    return JSON.parse(sessionStorage.getItem(STORE) ?? "null") as BenchRun | null;
  } catch {
    return null;
  }
}
function saveRun(run: BenchRun): void {
  try {
    sessionStorage.setItem(STORE, JSON.stringify(run));
  } catch {
    // (the run then ends at this step)
  }
}

/** The page's own parameters left out of the next step's address; the rest pass through. */
const OWN = ["step", "profile", "run", "args", "mount", "env", "autostart", "jit", "memory"];
function stepUrl(i: number): string {
  const next = new URLSearchParams();
  for (const [k, v] of params) if (!OWN.includes(k)) next.append(k, v);
  next.set("step", String(i));
  return `?${next}`;
}

async function deviceInfo(): Promise<DeviceInfo> {
  const nav = navigator as Navigator & {
    deviceMemory?: number;
    connection?: { effectiveType?: string; downlink?: number; type?: string };
    getBattery?: () => Promise<{ level: number; charging: boolean }>;
    userAgentData?: {
      getHighEntropyValues(
        h: string[],
      ): Promise<Record<string, string | undefined | { brand: string; version: string }[]>>;
    };
  };
  const d: DeviceInfo = {
    ua: navigator.userAgent,
    cores: navigator.hardwareConcurrency || 0,
    memoryGb: nav.deviceMemory,
    screen: `${screen.width}x${screen.height}`,
    dpr: devicePixelRatio || 1,
    viewport: `${innerWidth}x${innerHeight}`,
    crossOriginIsolated,
  };
  try {
    const h = await nav.userAgentData?.getHighEntropyValues([
      "model",
      "platformVersion",
      "architecture",
      "bitness",
      "fullVersionList",
    ]);
    if (h) {
      d.model = (h.model as string) || undefined;
      d.platform = h.platform as string;
      d.platformVersion = h.platformVersion as string;
      const [arch, bits] = [h.architecture, h.bitness].map((x) => (typeof x === "string" ? x : ""));
      d.arch = `${arch}${bits ? `/${bits}` : ""}` || undefined;
      const list = (h.fullVersionList as { brand: string; version: string }[] | undefined) ?? [];
      const b = list.find((x) => !/Not.A.Brand|Chromium/i.test(x.brand)) ?? list[0];
      if (b) d.browser = `${b.brand} ${b.version}`;
    }
  } catch {
    // (no client hints: Firefox, Safari)
  }
  const mem = (performance as Performance & { memory?: { jsHeapSizeLimit: number } }).memory;
  if (mem) d.heapLimitMb = Math.round(mem.jsHeapSizeLimit / 2 ** 20);
  try {
    const a = await navigator.gpu?.requestAdapter();
    if (a) {
      const i = a.info;
      d.gpu = {
        vendor: i.vendor,
        architecture: i.architecture,
        device: i.device,
        description: i.description,
        features: [...a.features].sort(),
        maxTexture: a.limits.maxTextureDimension2D,
      };
    }
  } catch {
    // (none)
  }
  try {
    // (as web/src/runtime.ts sakeBuild: the 4 GB memory sake.wasm needs, else sake-grow.wasm)
    new WebAssembly.Memory({ initial: 65535, maximum: 65535, shared: true });
    d.mem4g = true;
  } catch {
    d.mem4g = false;
  }
  try {
    const e = await navigator.storage?.estimate();
    if (e?.quota) d.storageQuotaMb = Math.round(e.quota / 2 ** 20);
  } catch {
    // (none)
  }
  if (nav.connection)
    d.connection = [
      nav.connection.type,
      nav.connection.effectiveType,
      nav.connection.downlink && `${nav.connection.downlink} Mbps`,
    ]
      .filter(Boolean)
      .join(" ");
  try {
    const b = await nav.getBattery?.();
    if (b) d.battery = { level: b.level, charging: b.charging };
  } catch {
    // (none)
  }
  return d;
}

async function pastRuns(): Promise<BenchRun[]> {
  try {
    const r = await fetch("bench-results");
    if (!r.ok) return [];
    return (await r.text())
      .split("\n")
      .map(parseRun)
      .filter((x): x is BenchRun => !!x);
  } catch {
    return [];
  }
}

/** The start screen: the profiles, and the past runs of every device. */
async function startScreen(): Promise<void> {
  setStart({ shown: false });
  setUi({ status: "" });
  const past = await pastRuns();
  const rows: string[][] = [
    [
      "",
      ...scores({
        version: VERSION,
        profile: "quick",
        started: 0,
        build: "",
        device: {} as DeviceInfo,
        steps: {},
      }).map((s) => s.label),
    ],
  ];
  for (const h of past
    .filter((h) => h.version === VERSION)
    .sort((a, b) => b.started - a.started)
    .slice(0, 20)) {
    rows.push([
      `${deviceName(h.device)} ${new Date(h.started).toISOString().slice(0, 10)}`,
      ...scores(h).map(formatScore),
    ]);
  }
  setBench({
    phase:
      "端末ベンチ：計測する範囲を選んでください（計測中は画面を閉じず、充電器を外した状態の方が実際に近くなります）",
    done: true,
    rows: rows.length > 1 ? rows : [],
    env: rows.length > 1 ? "これまでの結果（新しい順）" : "まだ結果はありません",
    choices: PROFILES.map((p) => ({ label: p.label, detail: p.detail, onClick: () => void begin(p.id) })),
  });
}

async function begin(profile: Profile): Promise<void> {
  setBench({ phase: "端末の情報を集めています…", done: false, choices: undefined, rows: [] });
  const run: BenchRun = {
    version: VERSION,
    profile,
    started: Date.now(),
    build: document.querySelector('meta[name="masu-version"]')?.getAttribute("content") ?? "",
    device: await deviceInfo(),
    steps: {},
  };
  saveRun(run);
  location.href = stepUrl(0);
}

function progressRows(run: BenchRun, steps: Step[]): string[][] {
  const rows = [["項目", "結果"]];
  for (const s of steps) {
    const r = run.steps[s.id];
    if (!r) continue;
    const v = r.error
      ? `失敗: ${r.error}`
      : Object.entries(r.metrics)
          .filter(([k]) => !k.includes("#"))
          .slice(0, 4)
          .map(([k, v]) => `${k} ${v >= 100 ? v.toFixed(0) : v.toFixed(1)}`)
          .join(", ");
    rows.push([s.label, v.slice(0, 120)]);
  }
  return rows;
}

/** Runs step `index` of the run's profile, records it, and goes on to the next one. */
async function runStep(run: BenchRun, index: number): Promise<void> {
  const steps = stepsOf(run.profile);
  const step = steps[index]!;
  const env = `${deviceName(run.device)} / ${run.profile} / ${index + 1} / ${steps.length}`;
  setBench({
    phase: `計測中… ${index + 1} / ${steps.length}（${step.label}）。閉じずにお待ちください`,
    done: false,
    rows: progressRows(run, steps),
    env,
  });
  const t0 = performance.now();
  const metrics: Record<string, number> = {};
  const metric = (k: string, v: number) => {
    metrics[k] = v;
    setBench({
      rows: [
        ...progressRows(run, steps),
        [
          step.label,
          Object.entries(metrics)
            .map(([a, b]) => `${a} ${b.toFixed(1)}`)
            .join(", ")
            .slice(0, 120),
        ],
      ],
    });
  };
  let finished = false;
  const finish = (r: Omit<StepResult, "wall_ms">) => {
    if (finished) return;
    finished = true;
    run.steps[step.id] = { ...r, wall_ms: Math.round(performance.now() - t0) };
    saveRun(run);
    if (index + 1 < steps.length) location.replace(stepUrl(index + 1));
    else void end(run);
  };
  const limit = step.timeout ?? STEP_TIMEOUT;
  const timer = setTimeout(() => {
    const why = `${limit} 秒で終わりませんでした`;
    if (parser) parser.end(why);
    else finish({ metrics, error: why });
  }, limit * 1000);
  let parser: OutputParser | undefined;
  try {
    switch (step.kind) {
      case "info":
        // (collected at the start already; here how long the page took to load)
        metric("page_load.ms", performance.now());
        break;
      case "gpu":
        await (await import("./devbench-gpu")).gpuBench(document.querySelector("#screen")!, metric);
        break;
      case "storage":
        await (await import("./devbench-io")).storageBench(metric);
        break;
      case "micro":
        await micro(step.micro!, metric);
        break;
      case "sustain":
        await sustain(step, metric);
        break;
      case "wasm": {
        parser = new OutputParser(true, undefined, plainScale(step.args));
        await new Promise<void>((resolve) => {
          const p = parser!;
          const done = p.end.bind(p);
          p.end = (e?: string) => (done(e), resolve());
          void wasmMain(step.wasm!, step.args!.split(","), (l) => {
            p.line(l);
            if (p.done) resolve();
          });
        });
        clearTimeout(timer);
        return finish({ metrics: parser.metrics, checks: parser.checks, error: parser.error });
      }
      case "exe":
        return exeStep(step, (p) => {
          parser = p;
          p.end = ((done) => (e?: string) => {
            done(e);
            clearTimeout(timer);
            finish({ metrics: p.metrics, checks: p.checks, error: p.error });
          })(p.end.bind(p));
        });
    }
    clearTimeout(timer);
    finish({ metrics });
  } catch (e) {
    clearTimeout(timer);
    finish({ metrics, error: String((e as Error)?.message ?? e) });
  }
}

/** A program of the bench tree through sake (play.ts), its output read by an OutputParser. */
async function exeStep(step: Step, made: (p: OutputParser) => void): Promise<void> {
  const gpu = () =>
    (
      window as unknown as { vnvmSake?: { _vnhost_gpu_total?: () => number } }
    ).vnvmSake?._vnhost_gpu_total?.() ?? -1;
  const p = new OutputParser(!!step.plain, gpu, step.plain ? plainScale(step.args) : 1);
  made(p);
  addEventListener("vnvm-print", (e) => p.line((e as CustomEvent<string>).detail));
  // (the program ended before its last line: play.ts says so before it leaves for the list)
  addEventListener("vnvm-exit", () => p.end());
  params.set("run", `Z:\\ippai\\game\\${step.exe}`);
  params.set("mount", "/ippai/game=bench");
  params.set("args", step.args ?? "");
  params.append("env", "MASU_NOPACE=1");
  if (step.env) {
    params.append("env", step.env);
    // (its own saved translations: they depend on it)
    params.set("jit", `bench-${step.env.replace(/[^A-Za-z0-9]/g, "_")}`);
  }
  for (const [k, v] of new URLSearchParams(step.page ?? "")) params.set(k, v);
  params.set("autostart", "");
  params.set("bench", "");
  history.replaceState(null, "", `?${params}`);
  await import("./play");
}

type Factory = (m: object) => Promise<{ callMain(args: string[]): void }>;
async function wasmModule(name: string, print: (l: string) => void) {
  const url = new URL(`benchwasm/${name}.js`, location.href).href;
  const factory = (await import(/* @vite-ignore */ url)).default as Factory;
  return factory({ print, printErr: (l: string) => console.warn(l) });
}

async function wasmMain(name: string, args: string[], print: (l: string) => void): Promise<void> {
  const m = await wasmModule(name, print);
  // (after the page has painted, so the table is there while it runs)
  await new Promise((r) => setTimeout(r, 100));
  m.callMain(args);
}

/** The same kernel again and again for SUSTAIN_S seconds: does the phone slow down when hot? */
async function sustain(step: Step, metric: (k: string, v: number) => void): Promise<void> {
  let total = NaN;
  const m = await wasmModule(step.wasm!, (l) => {
    const r = /^total\s+([\d.]+) ms/.exec(l.trim());
    if (r) total = Number(r[1]);
  });
  const series: number[] = [];
  const t0 = performance.now();
  while (performance.now() - t0 < SUSTAIN_S * 1000) {
    m.callMain(step.args!.split(","));
    series.push(total);
    // (the page stays responsive, and its timers run)
    await new Promise((r) => setTimeout(r, 0));
  }
  const n = Math.max(2, Math.floor(series.length / 5));
  const sorted = (xs: number[]) => [...xs].sort((a, b) => a - b);
  metric("first.ms", sorted(series.slice(0, n))[n >> 1]!);
  metric("last.ms", sorted(series.slice(-n))[n >> 1]!);
  metric("throttle", throttle(series));
  metric("rounds", series.length);
}

async function micro(name: string, metric: (k: string, v: number) => void): Promise<void> {
  const memory = new WebAssembly.Memory({ initial: 256, maximum: 256, shared: true });
  const bytes = Uint8Array.from(atob(MICRO), (c) => c.charCodeAt(0));
  const run = (await WebAssembly.instantiate(bytes, { e: { m: memory } })).instance.exports[name] as (
    d: number,
    s: number,
    n: number,
    r: number,
  ) => void;
  await new Promise((r) => setTimeout(r, 100));
  const times: number[] = [];
  for (let i = 0; i < 5; i++) {
    const t = performance.now();
    run(0, 6 << 20, 4 << 20, 200);
    times.push(performance.now() - t);
  }
  times.forEach((t, i) => metric(i ? `memcpy#${i + 1}` : "memcpy", t));
  metric(
    "total",
    times.reduce((a, b) => a + b, 0),
  );
}

async function end(run: BenchRun): Promise<void> {
  setBench({ phase: "結果を保存しています…", done: false });
  let saved = false;
  try {
    saved = (
      await fetch("bench-log", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(run),
      })
    ).ok;
  } catch {
    // (shown below)
  }
  try {
    sessionStorage.removeItem(STORE);
  } catch {
    // (none)
  }
  const past = await pastRuns();
  const rows = compareRows(run, past);
  setUi({ status: "" });
  const text = JSON.stringify(run);
  setBench({
    phase: saved
      ? "✓ 計測完了。結果はサーバーに保存しました"
      : "✓ 計測完了（サーバーに保存できませんでした。コピーして送ってください）",
    done: true,
    rows,
    env: `${run.device.ua} / ${run.device.cores} コア / ${run.device.gpu?.description || run.device.gpu?.vendor || ""}`,
    copy: () => {
      const t = getUi()
        .bench!.rows.map((r) => r.join("\t"))
        .join("\n");
      void navigator.clipboard.writeText(`${t}\n\n${text}`).catch(() => undefined);
    },
    choices: [{ label: "最初の画面へ", detail: "", onClick: () => location.assign(location.pathname) }],
  });
}

renderShell();
const run = loadRun();
const stepParam = params.get("step");
if (stepParam !== null && run) {
  const steps = stepsOf(run.profile);
  const i = Math.max(0, Math.min(steps.length - 1, Number(stepParam) || 0));
  void runStep(run, i);
} else if (isProfile(params.get("profile"))) {
  void begin(params.get("profile") as Profile);
} else {
  void startScreen();
}
