// A real game as the benchmark: Reunion (KiriKiri, the trial in the store as tree "reunion")
// started in wine.html, its story clicked through for a minute, measuring what playing costs:
//
//   - seconds from start to the title screen (the first frame with a busy middle band)
//   - CPU per second during the story: every Wine thread's time outside its waits (and its
//     synchronous XHRs), summed; not the page's own thread or the browser's (drawing, audio)
//     (vnhost_load_busy), in cores
//   - frames per second the game draws, and the longest gap between animation frames (stalls)
//
// Native Wine on the x86-64 dev machine for comparison: title 5.9 s, story 0.18 cores (9 s of
// CPU in 50 s). The result goes to vnvm serve's `PUT /bench-log` (bench-results.txt).
//
// It runs twice (the page reloads itself, Wine can't start twice in a page): the first run
// translates what isn't saved yet (after a new build, all of it), the second starts from the
// regions the first saved. The table shows both.
//
//   /game.html?secs=60    story seconds to measure (default 60)
//   /game.html?runs=1     one run only
//   &titleafter=N         the title counts as shown after N s (Direct3D games)
//   &pre=x,y              clicked every 3 s before the title (a first-start dialog; client pixels)
//   &env=VNVM_JIT_PROFILE=1  print the executed instruction counts at the end (console)

import { setBench } from "./ui/store";
import { renderShell } from "./ui/Shell";
const NATIVE = { title: 5.9, cores: 0.18 };
// in the game window's client pixels (the main window's, which the screen shows): on the
// title a stroke of "Start" (its button takes only clicks on the letters), then the text box,
// which advances the story; &click=x,y for another game
const CLICK = [142, 545];

const params = new URLSearchParams(location.search);
const secs = Math.max(10, Number(params.get("secs") ?? 60) || 60);
const runs = Math.min(5, Math.max(1, Number(params.get("runs") ?? 2) || 2));
const round = Math.min(runs, Math.max(1, Number(params.get("round") ?? 1) || 1));
const STORE = "vnvm-game";
for (const [i, v] of (params.get("click") ?? "").split(",").filter(Boolean).entries())
  CLICK[i] = Number(v) || 0;
type Rows = [string, string, string][];
/** rows of the earlier runs */
const earlier: Rows[] = (() => {
  try {
    return round === 1
      ? []
      : (JSON.parse(sessionStorage.getItem(STORE) ?? "[]") as Rows[]).slice(0, round - 1);
  } catch {
    return [];
  }
})();
if (!params.has("run")) {
  params.set("run", "Z:\\vnvm\\game\\reunion.exe");
  params.set("mount", "/vnvm/game=reunion");
  params.set("autostart", "");
  params.set("load", "");
  // the screen the numbers were taken on: on vnhost's 1366x768 default Reunion's title ignores
  // clicks (under Wine and sake alike)
  params.append("env", "VNVM_SCREEN=1280x720");
  history.replaceState(null, "", `?${params}`);
}

const device = `${navigator.userAgent} / ${navigator.hardwareConcurrency} cores`;
renderShell();
setBench({ env: device });
const t0 = performance.now();
const now = () => (performance.now() - t0) / 1000;

/** this run's rows; the table has a column per run */
const mine: Rows = [];
function render(): void {
  const rows = [["", ...Array.from({ length: runs }, (_, i) => `${i + 1} 回目`), "native"]];
  const all = [...earlier, mine];
  const names: string[] = [];
  for (const r of all) for (const [n] of r) if (!names.includes(n)) names.push(n);
  for (const n of names) {
    rows.push([
      n,
      ...Array.from({ length: runs }, (_, i) => all[i]?.find((x) => x[0] === n)?.[1] ?? ""),
      all.flat().find((x) => x[0] === n && x[2])?.[2] ?? "",
    ]);
  }
  setBench({ rows });
}
function row(name: string, value: string, native = ""): void {
  mine.push([name, value, native]);
  render();
}
render();
const runLabel = runs > 1 ? `${round} / ${runs} 回目: ` : "";

interface Wine {
  _vnhost_input_push(k: number, x: number, y: number, d: number): number;
  _vnhost_load_busy(): number;
  _vnhost_load_slot(i: number, out: number): number;
  _vnhost_jit_profile?(): void;
  _vnhost_display_view(out: number): void;
  wasmMemory: WebAssembly.Memory;
  _malloc(n: number): number;
  HEAPF64: Float64Array;
}
const wine = () => (window as unknown as { vnvmWine?: Wine }).vnvmWine;
// CPU ms all the program's threads used so far
const totalBusy = () => wine()?._vnhost_load_busy() ?? 0;

/** per thread: CPU ms and waits */
function slots(): { busy: number; waits: number }[] {
  const w = wine();
  if (!w) return [];
  const out = w._malloc(16) >>> 0;
  const list = [];
  for (let i = 0; w._vnhost_load_slot(i, out); i++) {
    const v = w.HEAPF64.subarray(out >>> 3, (out >>> 3) + 2);
    list.push({ busy: v[0]!, waits: v[1]! });
  }
  return list;
}

// frames the game drew, and the title
let frames = 0;
let title = 0;
let lastLook = 0;
addEventListener("wine-frame", (e) => {
  frames++;
  if (title || performance.now() - lastLook < 250) return;
  lastLook = performance.now();
  const { heap, bits, width, height } = (
    e as CustomEvent<{ heap: Uint8Array; bits: number; width: number; height: number }>
  ).detail;
  const colors = new Set<number>();
  const from = bits + Math.floor(height / 3) * width * 4;
  const to = bits + Math.floor((2 * height) / 3) * width * 4;
  for (let p = from; p < to; p += 97 * 4) colors.add(heap[p]! | (heap[p + 1]! << 8) | (heap[p + 2]! << 16));
  if (colors.size > 300) {
    title = now();
    void story();
  }
});

// main-thread stalls: gaps between animation frames
let lastRaf = 0;
let worstGap = 0;
const raf = (t: number) => {
  if (lastRaf) worstGap = Math.max(worstGap, t - lastRaf);
  lastRaf = t;
  requestAnimationFrame(raf);
};
requestAnimationFrame(raf);

/** clicks at `at` in the game window's client area (where the screen shows it) */
let viewBuf = 0;
function click(at = CLICK): void {
  const w = wine();
  if (!w) return;
  viewBuf ||= w._malloc(16) >>> 0;
  w._vnhost_display_view(viewBuf);
  const v = new Int32Array(w.wasmMemory.buffer, viewBuf, 4);
  const [x, y] = [v[0]! + at[0]!, v[1]! + at[1]!];
  w._vnhost_input_push(1, x, y, 0);
  w._vnhost_input_push(2, x, y, 0);
  w._vnhost_input_push(3, x, y, 0);
}
// &titleafter=N: the title counts as shown after N s (a Direct3D game: its frames don't come
// through wine-frame)
const TITLE_AFTER = Number(params.get("titleafter") ?? 0);
if (TITLE_AFTER > 0) {
  setTimeout(() => {
    if (!title) {
      title = now();
      void story();
    }
  }, TITLE_AFTER * 1000);
}
// &pre=x,y: clicked every 3 s until the title shows (a first-start dialog's OK button)
const PRE = params.get("pre")?.split(",").map(Number);
if (PRE?.length === 2) {
  const t = setInterval(() => (title ? clearInterval(t) : click(PRE)), 3000);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function story(): Promise<void> {
  row("タイトルまで (秒)", title.toFixed(1), String(NATIVE.title));
  // the title takes a moment to take clicks
  await sleep(3000);
  const perSec: { cpu: number; fps: number; gap: number }[] = [];
  const s0 = slots();
  const st0 = performance.now();
  let b = totalBusy();
  let f = frames;
  let t = performance.now();
  for (let s = 0; s < secs; s++) {
    if (s % 2 === 0) click();
    setBench({ phase: `計測中… ${runLabel}本編 ${s} / ${secs} 秒。閉じずにお待ちください` });
    worstGap = 0;
    await sleep(1000);
    const nb = totalBusy();
    const nt = performance.now();
    perSec.push({ cpu: (nb - b) / (nt - t), fps: ((frames - f) * 1000) / (nt - t), gap: worstGap });
    b = nb;
    f = frames;
    t = nt;
  }
  // the busiest threads over the story: cores, waits per second
  const s1 = slots();
  const span = performance.now() - st0;
  const threads = s1
    .map((x, i) => ({
      i,
      cores: (x.busy - (s0[i]?.busy ?? 0)) / span,
      waits: ((x.waits - (s0[i]?.waits ?? 0)) * 1000) / span,
    }))
    .sort((x, y) => y.cores - x.cores)
    .slice(0, 8);
  const avg = (k: "cpu" | "fps") => perSec.reduce((a, p) => a + p[k], 0) / perSec.length;
  const gaps = perSec.map((p) => p.gap).sort((x, y) => x - y);
  const cpu = avg("cpu");
  row("本編の CPU (コア)", cpu.toFixed(2), String(NATIVE.cores));
  row("本編の CPU (ネイティブ比)", `${(cpu / NATIVE.cores).toFixed(2)} 倍`);
  row("描画 fps (平均)", avg("fps").toFixed(1));
  row("コマ落ち: 最長の空き (ms)", gaps[gaps.length - 1]!.toFixed(0));
  row("コマ落ち: 95% の秒の空き (ms)", gaps[Math.floor(gaps.length * 0.95)]!.toFixed(0));
  threads.forEach((t, k) =>
    row(`忙しいスレッド ${k + 1} 位`, `${t.cores.toFixed(2)} コア, 待ち ${t.waits.toFixed(0)}/秒 (#${t.i})`),
  );
  const lines = [
    `game Reunion on sake (story ${secs} s, run ${round} of ${runs})`,
    `title ${title.toFixed(1)} s`,
    `cpu ${cpu.toFixed(3)} cores (native ${NATIVE.cores})`,
    `fps ${avg("fps").toFixed(1)}`,
    `gap max ${gaps[gaps.length - 1]!.toFixed(0)} ms, p95 ${gaps[Math.floor(gaps.length * 0.95)]!.toFixed(0)} ms`,
    `threads (slot cores waits/s): ${threads.map((t) => `${t.i} ${t.cores.toFixed(2)} ${t.waits.toFixed(0)}`).join(", ")}`,
    `per second (cpu cores / fps / gap ms): ${perSec.map((p) => `${p.cpu.toFixed(2)}/${p.fps.toFixed(0)}/${p.gap.toFixed(0)}`).join(" ")}`,
    device,
  ];
  // &env=VNVM_JIT_PROFILE=1: the executed instruction counts (to stderr, the console)
  if (params.getAll("env").some((e) => e.startsWith("VNVM_JIT_PROFILE="))) wine()?._vnhost_jit_profile?.();
  await fetch("bench-log", { method: "PUT", body: lines.join("\n") }).catch(() => undefined);
  if (round < runs) {
    try {
      sessionStorage.setItem(STORE, JSON.stringify([...earlier, mine]));
    } catch {
      // the next run's table then lacks this one
    }
    setBench({ phase: `計測中… ${round} 回目が終わりました。${round + 1} 回目を始めます` });
    // (the regions this run translated go to the server every 30 s; the last ones with this)
    await sleep(1000);
    params.set("round", String(round + 1));
    location.search = `?${params}`;
    return;
  }
  setBench({ phase: "✓ 計測完了。結果はサーバに保存しました", done: true });
}

setBench({ phase: `計測中… ${runLabel}起動してタイトル画面を待っています` });
setInterval(() => {
  if (!title)
    setBench({ phase: `計測中… ${runLabel}起動してタイトル画面を待っています (${now().toFixed(0)} 秒)` });
}, 1000);

await import("./wine");
