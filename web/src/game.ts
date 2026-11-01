// A real game as the benchmark: Reunion (KiriKiri, the trial in the store as tree "reunion")
// started in wine.html, its story clicked through for a minute, measuring what playing costs:
//
//   - seconds from start to the title screen (the first frame with a busy middle band)
//   - CPU per second during the story: every Wine thread's time outside its waits, summed
//     (vnhost_load_busy), in cores
//   - frames per second the game draws, and the longest gap between animation frames (stalls)
//
// Native Wine on the x86-64 dev machine for comparison: title 5.9 s, story 0.18 cores (9 s of
// CPU in 50 s). The result goes to vnvm-server's `PUT /bench-log` (bench-results.txt).
//
//   /game.html?secs=60    story seconds to measure (default 60)

const NATIVE = { title: 5.9, cores: 0.18 };
const CLICK = [380, 622]; // the text box, in frame pixels: advances the story (and leaves the title)

const params = new URLSearchParams(location.search);
const secs = Math.max(10, Number(params.get("secs") ?? 60) || 60);
if (!params.has("run")) {
  params.set("run", "Z:\\vnvm\\game\\reunion.exe");
  params.set("mount", "/vnvm/game=reunion");
  params.set("autostart", "");
  params.set("load", "");
  history.replaceState(null, "", `?${params}`);
}

const table = document.querySelector<HTMLTableElement>("#result")!;
const env = document.querySelector<HTMLDivElement>("#env")!;
const phase = document.querySelector<HTMLDivElement>("#phase")!;
const device = `${navigator.userAgent} / ${navigator.hardwareConcurrency} cores`;
env.textContent = device;
const t0 = performance.now();
const now = () => (performance.now() - t0) / 1000;

function row(name: string, value: string, native = ""): void {
  const r = table.insertRow();
  for (const t of [name, value, native]) r.insertCell().textContent = t;
}
table.innerHTML = "<tr><th></th><th>vnvm</th><th>native</th></tr>";

interface Wine {
  _vnhost_input_push(k: number, x: number, y: number, d: number): number;
  _vnhost_load_busy(): number;
}
const wine = () => (window as unknown as { vnvmWine?: Wine }).vnvmWine;
// CPU ms all Wine threads used so far
const totalBusy = () => wine()?._vnhost_load_busy() ?? 0;

// frames the game drew, and the title
let frames = 0;
let title = 0;
let lastLook = 0;
addEventListener("wine-frame", (e) => {
  frames++;
  if (title || performance.now() - lastLook < 250) return;
  lastLook = performance.now();
  const { heap, bits, width, height } = (e as CustomEvent<{ heap: Uint8Array; bits: number; width: number; height: number }>).detail;
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

function click(): void {
  const w = wine();
  if (!w) return;
  w._vnhost_input_push(1, CLICK[0]!, CLICK[1]!, 0);
  w._vnhost_input_push(2, CLICK[0]!, CLICK[1]!, 0);
  w._vnhost_input_push(3, CLICK[0]!, CLICK[1]!, 0);
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

async function story(): Promise<void> {
  row("タイトルまで (秒)", title.toFixed(1), String(NATIVE.title));
  // the title takes a moment to take clicks
  await sleep(3000);
  const perSec: { cpu: number; fps: number; gap: number }[] = [];
  let b = totalBusy();
  let f = frames;
  let t = performance.now();
  for (let s = 0; s < secs; s++) {
    if (s % 2 === 0) click();
    phase.textContent = `計測中… 本編 ${s} / ${secs} 秒。閉じずにお待ちください`;
    worstGap = 0;
    await sleep(1000);
    const nb = totalBusy();
    const nt = performance.now();
    perSec.push({ cpu: (nb - b) / (nt - t), fps: ((frames - f) * 1000) / (nt - t), gap: worstGap });
    b = nb;
    f = frames;
    t = nt;
  }
  const avg = (k: "cpu" | "fps") => perSec.reduce((a, p) => a + p[k], 0) / perSec.length;
  const gaps = perSec.map((p) => p.gap).sort((x, y) => x - y);
  const cpu = avg("cpu");
  row("本編の CPU (コア)", cpu.toFixed(2), String(NATIVE.cores));
  row("本編の CPU (ネイティブ比)", `${(cpu / NATIVE.cores).toFixed(2)} 倍`);
  row("描画 fps (平均)", avg("fps").toFixed(1));
  row("コマ落ち: 最長の空き (ms)", gaps[gaps.length - 1]!.toFixed(0));
  row("コマ落ち: 95% の秒の空き (ms)", gaps[Math.floor(gaps.length * 0.95)]!.toFixed(0));
  phase.textContent = "✓ 計測完了。結果はサーバに保存しました";
  phase.className = "done";
  const lines = [
    `game Reunion (story ${secs} s)`,
    `title ${title.toFixed(1)} s`,
    `cpu ${cpu.toFixed(3)} cores (native ${NATIVE.cores})`,
    `fps ${avg("fps").toFixed(1)}`,
    `gap max ${gaps[gaps.length - 1]!.toFixed(0)} ms, p95 ${gaps[Math.floor(gaps.length * 0.95)]!.toFixed(0)} ms`,
    `per second (cpu cores / fps / gap ms): ${perSec.map((p) => `${p.cpu.toFixed(2)}/${p.fps.toFixed(0)}/${p.gap.toFixed(0)}`).join(" ")}`,
    device,
  ];
  void fetch("bench-log", { method: "PUT", body: lines.join("\n") }).catch(() => undefined);
}

phase.textContent = "計測中… 起動してタイトル画面を待っています";
setInterval(() => {
  if (!title) phase.textContent = `計測中… 起動してタイトル画面を待っています (${now().toFixed(0)} 秒)`;
}, 1000);

await import("./wine");

export {};
