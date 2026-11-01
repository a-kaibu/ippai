// One corpus title in headless Chromium (wine.html, &load): finds the title screen (first frame
// whose middle band has many colours), runs the title's steps, then clicks through the story for
// STORY seconds while measuring Wine's CPU (vnhost_load_busy, as game.html). Writes OUT/result.json,
// console.log and PNGs (title, story-N, last).
//
//   node tests/corpus.mjs URL OUT [STEPS]
//
// STEPS (client-area pixels of the game window, run once the title shows):
//   "click:x,y wait:3 key:Return wait:5" ; default: nothing (story clicks start right away)
// env: PRE=x,y / PRE_KEYS=0 (see below), STORY=60 (story seconds), TITLE_MAX=120 (give up on the title), STORY_AT=x,y (story clicks,
// default centre of the bottom fifth), CHROME_ARGS, PLAYWRIGHT
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
import { mkdirSync, appendFileSync, writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const [url, out, steps = ""] = process.argv.slice(2);
const STORY = Number(process.env.STORY ?? 60);
const TITLE_MAX = Number(process.env.TITLE_MAX ?? 120);
mkdirSync(out, { recursive: true });
const t0 = Date.now();
const now = () => (Date.now() - t0) / 1000;
const logFile = `${out}/console.log`;
writeFileSync(logFile, "");
const res = { url, steps, title: null, errors: [], shots: [] };
const save = () => writeFileSync(`${out}/result.json`, JSON.stringify(res, null, 1));
const args = ["--enable-unsafe-webgpu", "--use-webgpu-adapter=swiftshader", "--enable-features=Vulkan",
  ...(process.env.CHROME_ARGS ?? "").split(" ").filter(Boolean)];
const browser = await chromium.launch({ args });
// Japanese games check the time zone (Siglus: "Japan only")
const page = await browser.newPage({ viewport: { width: 1280, height: 720 }, timezoneId: process.env.TZ_ID ?? "Asia/Tokyo", locale: "ja-JP" });
let crashed = false, exited = false;
page.on("console", (m) => {
  const t = m.text();
  appendFileSync(logFile, `${now().toFixed(1).padStart(7)} [${m.type()}] ${t}\n`);
  if (/Unhandled exception|unimplemented function|page fault|panicked|RuntimeError|wine: |abort/i.test(t) && res.errors.length < 40)
    res.errors.push(t.slice(0, 300));
  if (/quit|exited/i.test(t) && /game|root/i.test(t)) exited = true;
});
page.on("pageerror", (e) => { appendFileSync(logFile, `[pageerror] ${e.message}\n`); res.errors.push("pageerror: " + e.message.slice(0, 200)); });
page.on("crash", () => { crashed = true; res.errors.push("page crashed"); });
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
      raw[d] = bgra[s + 2]; raw[d + 1] = bgra[s + 1]; raw[d + 2] = bgra[s];
    }
  const chunk = (type, data) => {
    const b = Buffer.alloc(12 + data.length);
    b.writeUInt32BE(data.length, 0); b.write(type, 4, "ascii"); data.copy(b, 8);
    b.writeUInt32BE(crc32(b.subarray(4, 8 + data.length)), 8 + data.length);
    return b;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr.set([8, 2, 0, 0, 0], 8);
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

// the game window's client area as the page shows it (+ Direct3D overlay); with full=false only stats
async function frame(full) {
  return page.evaluate(async (full) => {
    const m = window.vnvmWine;
    if (!m || !m._vnhost_display_frame) return null;
    const p = m._malloc(20) >>> 0;
    m._vnhost_display_size(p, p + 4);
    m.HEAPU32[(p + 8) >>> 2] = -1;
    const w = m.HEAPU32[p >>> 2], h = m.HEAPU32[(p + 4) >>> 2];
    const bits = m._vnhost_display_frame(p + 8) >>> 0;
    m._free(p);
    const px = m.HEAPU8.slice(bits, bits + w * h * 4);
    const d3d = window.vnvmD3d;
    const o = d3d?.overlay;
    const cap = o ? await d3d.capture() : null;
    if (cap) {
      const [l, t] = o.rect;
      const cw = Math.min(cap.w, o.rect[2] - l), ch = Math.min(cap.h, o.rect[3] - t);
      for (let y = 0; y < ch; y++) {
        if (t + y < 0 || t + y >= h) continue;
        for (let x = 0; x < cw; x++) {
          if (l + x < 0 || l + x >= w) continue;
          px.set(cap.data.subarray((y * cap.w + x) * 4, (y * cap.w + x) * 4 + 4), ((t + y) * w + l + x) * 4);
        }
      }
    }
    let v = window.vnvmPresenter?.view;
    if (!(v && v[2] > v[0] && v[3] > v[1])) v = [0, 0, w, h];
    const ow = v[2] - v[0], oh = v[3] - v[1];
    const outp = new Uint8Array(ow * oh * 4);
    for (let y = 0; y < oh; y++) outp.set(px.subarray(((v[1] + y) * w + v[0]) * 4, ((v[1] + y) * w + v[2]) * 4), y * ow * 4);
    const colors = new Set();
    // ~3000 samples of the middle third, whatever the window size
    const step = Math.max(1, Math.floor((ow * oh) / 3 / 3000)) * 4 + 4;
    for (let i = Math.floor(oh / 3) * ow * 4; i < Math.floor((2 * oh) / 3) * ow * 4; i += step)
      colors.add(outp[i] | (outp[i + 1] << 8) | (outp[i + 2] << 16));
    let hsh = 0;
    for (let i = 0; i < outp.length; i += 4093) hsh = (hsh * 31 + outp[i]) | 0;
    const r = { w: ow, h: oh, view: v, colors: colors.size, hash: hsh, d3d: !!o, busy: m._vnhost_load_busy?.() ?? 0 };
    if (full) {
      let s = "";
      for (let i = 0; i < outp.length; i += 0x8000) s += String.fromCharCode(...outp.subarray(i, i + 0x8000));
      r.data = btoa(s);
    }
    return r;
  }, full).catch((e) => ({ err: String(e) }));
}
async function shot(name) {
  const f = await frame(true);
  if (!f || f.err || !f.data) return f;
  writeFileSync(`${out}/${name}.png`, png(f.w, f.h, Buffer.from(f.data, "base64")));
  res.shots.push(name);
  delete f.data;
  return f;
}
const sleep = (s) => page.waitForTimeout(s * 1000);
async function input(kind, x, y, d) {
  await page.evaluate(([k, x, y, d]) => window.vnvmWine?._vnhost_input_push(k, x, y, d), [kind, x, y, d]).catch(() => {});
}
let view = [0, 0, 640, 480];
async function click(x, y) {
  const X = view[0] + x, Y = view[1] + y;
  await input(1, X, Y, 0); await input(2, X, Y, 0); await input(3, X, Y, 0);
}
const VK = { Return: [0x0d, 28], Space: [0x20, 57], Escape: [0x1b, 1], Down: [0x28, 0], Up: [0x26, 0], Ctrl: [0x11, 29], z: [0x5a, 44] };
async function key(name) {
  const [vk, sc] = VK[name] ?? [name.charCodeAt(0), 0];
  await input(5, 0, 0, vk | (sc << 16)); await sleep(0.05); await input(6, 0, 0, vk | (sc << 16));
}

// 1. the title
// before the title: a launcher or settings dialog (PRE=x,y clicks there every 3 s; otherwise
// Return every 6 s from 12 s on, which takes a dialog's default button)
let last = null, firstFrame = null, lastPre = 0;
const PRE = process.env.PRE?.split(",").map(Number);
while (now() < TITLE_MAX && !crashed) {
  await sleep(1);
  const f = await frame(false);
  if (!f || f.err) continue;
  last = f;
  view = f.view;
  if (PRE?.length === 2 && now() - lastPre >= 3) { lastPre = now(); await click(PRE[0], PRE[1]); }
  else if (!PRE && process.env.PRE_KEYS !== "0" && now() > 12 && now() - lastPre >= 6 && f.colors <= 300) { lastPre = now(); await key("Return"); }
  if (firstFrame === null && f.w > 32) firstFrame = now();
  if (f.colors > 300) { res.title = now(); break; }
}
// the title is drawn: wait until it holds still (menus fade in), at most 25 s
if (res.title !== null) {
  let prev = null, still = 0;
  for (const t = now(); now() - t < 25 && !crashed; ) {
    await sleep(1);
    const f = await frame(false);
    if (!f || f.err) continue;
    still = f.hash === prev ? still + 1 : 0;
    prev = f.hash;
    if (still >= 2) break;
  }
  res.titleStill = now();
}
res.firstWindow = firstFrame;
if (last) res.size = [last.w, last.h];
if (res.title === null) {
  await shot("notitle");
  res.d3d = last?.d3d ?? false;
  save();
  await browser.close();
  process.exit(0);
}
const tf = await shot("title");
if (tf?.view) view = tf.view;
res.d3d = tf?.d3d ?? false;
save();

// 2. the title's steps
for (const s of steps.split(/[ ;]+/).filter(Boolean)) {
  const [op, a = ""] = s.split(":");
  if (op === "click") { const [x, y] = a.split(",").map(Number); await click(x, y); }
  else if (op === "wait") await sleep(Number(a));
  else if (op === "key") await key(a);
  else if (op === "shot") await shot(a);
}

// 3. the story: clicks every 2 s, CPU over STORY seconds
const [sx, sy] = (process.env.STORY_AT ?? "").split(",").map(Number);
const at = sx ? [sx, sy] : [Math.round(view[2] - view[0]) / 2, Math.round((view[3] - view[1]) * 0.85)];
const b0 = (await frame(false))?.busy ?? 0, s0 = now();
const hashes = new Set();
for (let s = 0; s < STORY && !crashed; s += 2) {
  await click(at[0], at[1]);
  await sleep(2);
  if (s % 20 === 18) { const f = await shot(`story-${s + 2}`); if (f?.hash !== undefined) hashes.add(f.hash); }
}
const f1 = await frame(false);
res.storyCores = f1 && !f1.err ? (f1.busy - b0) / 1000 / (now() - s0) : null;
res.storyFrames = hashes.size;
res.crashed = crashed;
res.end = now();
save();
await browser.close();
