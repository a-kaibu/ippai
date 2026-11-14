// Long-run memory check of a game in web/play.html (headless Chromium): every STEP seconds it
// logs the host heap (crates/kura/src/wrap.rs vnhost_mem_info: sbrk top and malloc's bytes in
// use; the 4 GB wasm memory is reserved at the start, so its size says nothing), the JS heap,
// the page's visibility and the Direct3D frames drawn. HIDE="a,b;c,d" puts the game's tab in
// the background from a to b seconds, as a phone's or a desktop browser's hidden tab: the
// animation frames stop (headless Chromium keeps every tab visible, so its workers' and the
// page's requestAnimationFrame hold their callbacks) and the page is told it is hidden
// (visibilityState, visibilitychange); the game's threads go on.
//
// At the end it compares the host heap in use at the last sample of each phase with the one at
// WARM seconds (past the start's loading): GROW_MB (default 256) is the most it may have grown,
// else it exits 1. 金恋's hidden tab grew ~280 MB a minute before kura/src/d3d9/gpu.rs present()
// waited for the page (2026-11-14 crash logs: OOM 6-7 min after the tab was hidden).
//
//   node tests/memlong-browser.mjs URL OUT_DIR [SECONDS] [STEP]
// (PLAYWRIGHT as elsewhere; WARM default 120; CLICK="x,y,ms" clicks there every ms while shown,
// to page through a story; KEYS too: "Enter,ms")
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const [url, out, seconds = "1800", step = "10"] = process.argv.slice(2);
if (!url || !out) {
  console.error("usage: node tests/memlong-browser.mjs URL OUT_DIR [SECONDS] [STEP]");
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const warm = Number(process.env.WARM ?? 120);
const growMb = Number(process.env.GROW_MB ?? 256);
const hide = (process.env.HIDE ?? "")
  .split(";")
  .filter(Boolean)
  .map((s) => s.split(",").map(Number));
const hiddenAt = (t) => hide.some(([a, b]) => t >= a && t < b);

const browser = await chromium.launch({ args: ["--enable-unsafe-webgpu", ...(process.env.CHROME_ARGS ?? "").split(" ").filter(Boolean)] });
const ctx = await browser.newContext({ viewport: { width: 1280, height: 720 } });
const page = await ctx.newPage();
const log = `${out}/console.log`;
const t0 = Date.now();
const secs = () => (Date.now() - t0) / 1000;
page.on("console", (m) => appendFileSync(log, `${secs().toFixed(1)} [${m.type()}] ${m.text()}\n`));
page.on("pageerror", (e) => appendFileSync(log, `${secs().toFixed(1)} [pageerror] ${e.message}\n`));
page.on("crash", () => appendFileSync(log, `${secs().toFixed(1)} [crash]\n`));
await page.goto(url);
let hidden = false;

// (in the page and every worker: requestAnimationFrame callbacks held while __hide is set)
const gate = `(() => {
  if (self.__gate) return;
  self.__gate = true;
  const raf = self.requestAnimationFrame?.bind(self);
  if (!raf) return;
  let held = [];
  self.__setHide = (on) => {
    self.__hide = on;
    if (!on) for (const f of held.splice(0)) raf(f);
  };
  self.requestAnimationFrame = (f) => (self.__hide ? (held.push(f), 0) : raf(f));
})()`;
const setHidden = async (on) => {
  // (a guest thread's worker runs wasm without going back to its event loop: it never answers,
  // and has no animation frames to hold)
  const late = () => new Promise((r) => setTimeout(r, 2000));
  await Promise.all(page.workers().map((w) => Promise.race([w.evaluate(`${gate}; self.__setHide?.(${on})`).catch(() => {}), late()])));
  await page.evaluate(
    ([g, on]) => {
      (0, eval)(g);
      self.__setHide?.(on);
      Object.defineProperty(document, "visibilityState", { configurable: true, get: () => (on ? "hidden" : "visible") });
      Object.defineProperty(document, "hidden", { configurable: true, get: () => on });
      document.dispatchEvent(new Event("visibilitychange"));
    },
    [gate, on],
  );
};

const rows = [];
const sample = () =>
  page.evaluate(() => {
    const m = window.vnvmSake;
    const pm = performance.memory;
    const r = { vis: document.visibilityState, js: pm ? pm.usedJSHeapSize / 2 ** 20 : 0, top: 0, used: 0, frames: 0 };
    if (m?._vnhost_mem_info && m._malloc) {
      const p = m._malloc(32) >>> 0;
      m._vnhost_mem_info(p);
      const f = new Float64Array(m.wasmMemory.buffer, p, 2);
      r.top = f[0] / 2 ** 20;
      r.used = f[1] / 2 ** 20;
      if (m._vnhost_render_info) {
        m._vnhost_render_info(p);
        r.frames = new Uint32Array(m.wasmMemory.buffer, p, 8)[4];
      }
      m._free(p);
    }
    return r;
  });

const click = process.env.CLICK?.split(",").map(Number);
if (click)
  setInterval(() => !hidden && page.mouse.click(click[0], click[1]).catch(() => {}), click[2]);
const key = process.env.KEYS?.split(",");
if (key) setInterval(() => !hidden && page.keyboard.press(key[0]).catch(() => {}), Number(key[1]));
const end = Number(seconds);
while (secs() < end) {
  const t = secs();
  const want = hiddenAt(t);
  if (want !== hidden) {
    hidden = want;
    await setHidden(want);
  }
  let r;
  try {
    r = await sample();
  } catch (e) {
    r = { error: String(e) };
  }
  const row = { t: Math.round(t), hidden: want, ...r };
  rows.push(row);
  appendFileSync(`${out}/mem.tsv`, `${row.t}\t${want ? "hidden" : "shown"}\t${r.vis}\t${r.top?.toFixed(0)}\t${r.used?.toFixed(0)}\t${r.js?.toFixed(0)}\t${r.frames}\n`);
  await new Promise((res) => setTimeout(res, Number(step) * 1000));
}
await browser.close();
writeFileSync(`${out}/mem.json`, JSON.stringify(rows));

const base = rows.find((r) => r.t >= warm && r.used);
let fail = false;
if (!base) {
  console.log("no sample with the host heap past WARM: did the game start?");
  fail = true;
} else {
  for (const r of rows.filter((r) => r.t >= warm)) {
    const grew = r.used - base.used;
    if (!(grew <= growMb)) {
      console.log(`at ${r.t} s (${r.hidden ? "hidden" : "shown"}): host heap in use ${r.used?.toFixed(0)} MB, ${grew.toFixed(0)} MB over ${base.used.toFixed(0)} MB at ${base.t} s (limit ${growMb})`);
      fail = true;
      break;
    }
  }
  // (a hidden tab draws nothing; shown again, the game goes on drawing)
  for (const [a, b] of hide) {
    const during = rows.filter((r) => r.t >= a + 2 * Number(step) && r.t < b && r.frames !== undefined);
    const after = rows.filter((r) => r.t >= b + 2 * Number(step) && r.frames !== undefined);
    if (during.length > 1 && during.at(-1).frames - during[0].frames > 60) {
      console.log(`hidden from ${a} to ${b} s: ${during.at(-1).frames - during[0].frames} frames drawn`);
      fail = true;
    }
    if (after.length > 1 && !(after.at(-1).frames > after[0].frames)) {
      console.log(`shown again at ${b} s: no frames drawn after`);
      fail = true;
    }
  }
  const last = rows.at(-1);
  console.log(`host heap in use: ${base.used.toFixed(0)} MB at ${base.t} s -> ${last.used?.toFixed(0)} MB at ${last.t} s (max ${Math.max(...rows.map((r) => r.used || 0)).toFixed(0)}), frames ${last.frames}`);
}
process.exit(fail ? 1 : 0);
