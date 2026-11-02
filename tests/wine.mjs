// Headless test of web/wine.html (Wine in wasm): logs the console with timestamps and saves the
// screen vnhost composes (read from the wasm heap: headless Chromium can't show WebGPU canvases;
// Direct3D windows' frames are read back from the GPU)
// as a PNG every INTERVAL seconds. After the first shot, CLICKS="x,y;x,y" clicks there (page
// pixels; each held 200 ms) and KEYS="KeyA Enter" presses those keys; CLICK_EVERY="x,y" clicks there after every
// shot (to page through a game); CLICK_SEQ="x,y;;x,y" clicks its n-th entry (if any) after shot n
// instead, then CLICK_EVERY takes over (SHOT_COORDS=1: click coordinates are screenshot pixels). CHROME_ARGS adds browser flags; USER_DATA_DIR keeps the
// browser profile (its HTTP and code caches) there across runs.
//
//   node tests/wine.mjs URL OUT_DIR [SECONDS] [INTERVAL]
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
import { mkdirSync, appendFileSync, writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const [url, out, seconds = "120", interval = "15"] = process.argv.slice(2);
mkdirSync(out, { recursive: true });
const t0 = Date.now();
const stamp = () => ((Date.now() - t0) / 1000).toFixed(1).padStart(7);
const logFile = `${out}/console.log`;
const args = ["--enable-unsafe-webgpu", ...(process.env.CHROME_ARGS ?? "").split(" ").filter(Boolean)];
const viewport = { width: 1280, height: 720 };
const browser = process.env.USER_DATA_DIR
  ? await chromium.launchPersistentContext(process.env.USER_DATA_DIR, { args, viewport })
  : await chromium.launch({ args });
const page = await browser.newPage(process.env.USER_DATA_DIR ? undefined : { viewport });
page.on("console", (m) => appendFileSync(logFile, `${stamp()} [${m.type()}] ${m.text()}\n`));
page.on("pageerror", (e) => appendFileSync(logFile, `${stamp()} [pageerror] ${e.message}\n`));
page.on("crash", () => appendFileSync(logFile, `${stamp()} [crash] the page crashed\n`));
await page.goto(url);

const CRC = new Int32Array(256).map((_, n) => {
  for (let k = 0; k < 8; k++) n = n & 1 ? 0xedb88320 ^ (n >>> 1) : n >>> 1;
  return n;
});
function crc32(b) {
  let c = -1;
  for (const x of b) c = CRC[(c ^ x) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}
function png(w, h, bgra) {
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const s = (y * w + x) * 4, d = y * (w * 3 + 1) + 1 + x * 3;
      raw[d] = bgra[s + 2];
      raw[d + 1] = bgra[s + 1];
      raw[d + 2] = bgra[s];
    }
  const chunk = (type, data) => {
    const b = Buffer.alloc(12 + data.length);
    b.writeUInt32BE(data.length, 0);
    b.write(type, 4, "ascii");
    data.copy(b, 8);
    b.writeUInt32BE(crc32(b.subarray(4, 8 + data.length)), 8 + data.length);
    return b;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

async function shot(n) {
  const f = await page.evaluate(async () => {
    const m = window.vnvmWine;
    if (!m) return null;
    const p = m._malloc(20) >>> 0;
    m._vnhost_display_size(p, p + 4);
    // (views over the memory as it is now: another thread may have grown it)
    const u32 = () => new Uint32Array(m.wasmMemory.buffer);
    u32()[(p + 8) >>> 2] = -1;
    const w = u32()[p >>> 2], h = u32()[(p + 4) >>> 2];
    const bits = m._vnhost_display_frame(p + 8) >>> 0;
    // (a frame buffer shorter than the screen size, as while a mode changes, stays black)
    const px = new Uint8Array(w * h * 4);
    px.set(new Uint8Array(m.wasmMemory.buffer).subarray(bits, bits + w * h * 4).subarray(0, px.length));
    // a Direct3D window's last frame goes over its client area
    const d3d = window.vnvmD3d;
    const o = d3d?.overlay;
    const cap = o ? await d3d.capture() : null;
    if (cap) {
      const above = window.vnvmPresenter?.above ?? [];
      const l = Math.round(Number(o.rect[0])), t = Math.round(Number(o.rect[1]));
      const cw = Math.min(cap.w, Math.round(Number(o.rect[2])) - l), ch = Math.min(cap.h, Math.round(Number(o.rect[3])) - t);
      for (let y = 0; y < ch; y++) {
        if (t + y < 0 || t + y >= h) continue;
        for (let x = 0; x < cw; x++) {
          if (l + x < 0 || l + x >= w || (y * cap.w + x) * 4 + 4 > cap.data.length) continue;
          // (windows above the Direct3D one stay on top)
          if (above.some(([al, at, ar, ab]) => l + x >= al && l + x < ar && t + y >= at && t + y < ab)) continue;
          px.set(cap.data.subarray((y * cap.w + x) * 4, (y * cap.w + x) * 4 + 4), ((t + y) * w + l + x) * 4);
        }
      }
    }
    // what the page shows: the game window's client area
    let out = px, ow = w, oh = h;
    const v0 = window.vnvmPresenter?.view;
    // (clamped to the screen: a window may reach past it)
    const v = v0 && [Math.max(0, v0[0]), Math.max(0, v0[1]), Math.min(w, v0[2]), Math.min(h, v0[3])];
    if (v && v[2] > v[0] && v[3] > v[1]) {
      ow = v[2] - v[0];
      oh = v[3] - v[1];
      out = new Uint8Array(ow * oh * 4);
      for (let y = 0; y < oh; y++) out.set(px.subarray(((v[1] + y) * w + v[0]) * 4, ((v[1] + y) * w + v[2]) * 4), y * ow * 4);
    }
    let s = "";
    for (let i = 0; i < out.length; i += 0x8000) s += String.fromCharCode(...out.subarray(i, i + 0x8000));
    return { w: ow, h: oh, data: btoa(s) };
  });
  if (f) writeFileSync(`${out}/screen-${String(n).padStart(3, "0")}.png`, png(f.w, f.h, Buffer.from(f.data, "base64")));
}

// (hovered first: many games only take a click on a button they saw the cursor over)
async function click(x, y) {
  // SHOT_COORDS=1: x,y are pixels of the screenshots (the game window's client area), not of the page
  if (process.env.SHOT_COORDS)
    [x, y] = await page.evaluate(([sx, sy]) => {
      const p = window.vnvmPresenter;
      const b = p.canvas.getBoundingClientRect(), dpr = p.canvas.width / Math.max(1, b.width);
      const [l, t, vr, vb] = p.shown, r = p.rect;
      const fx = Math.max(0, p.view?.[0] ?? 0) + sx + 0.5, fy = Math.max(0, p.view?.[1] ?? 0) + sy + 0.5;
      return [b.left + (r.x + ((fx - l) * r.w) / (vr - l)) / dpr, b.top + (r.y + ((fy - t) * r.h) / (vb - t)) / dpr];
    }, [x, y]);
  appendFileSync(logFile, `${stamp()} [click] page ${x.toFixed(1)},${y.toFixed(1)}\n`);
  await page.mouse.move(x, y);
  await page.waitForTimeout(300);
  await page.mouse.click(x, y, { delay: 200 }); // (held: games that poll GetAsyncKeyState per frame)
}

const end = t0 + Number(seconds) * 1000;
let n = 0;
while (Date.now() < end) {
  await page.waitForTimeout(Number(interval) * 1000);
  await shot(++n);
  if (n === 1) {
    for (const xy of process.env.CLICKS?.split(";") ?? []) {
      const [x, y] = xy.split(",").map(Number);
      await click(x, y);
    }
    for (const key of process.env.KEYS?.split(" ").filter(Boolean) ?? []) await page.keyboard.press(key);
  }
  const seq = process.env.CLICK_SEQ?.split(";");
  if (seq && n <= seq.length) {
    if (seq[n - 1]) {
      const [x, y] = seq[n - 1].split(",").map(Number);
      await click(x, y);
    }
  } else if (process.env.CLICK_EVERY) {
    const [x, y] = process.env.CLICK_EVERY.split(",").map(Number);
    await click(x, y);
  }
}
await browser.close();
