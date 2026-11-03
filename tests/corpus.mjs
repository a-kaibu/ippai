// One corpus title in headless Chromium (play.html, &load): finds the title screen (first frame
// whose middle band has many colours), runs the title's steps, then clicks through the story for
// STORY seconds while measuring the program's CPU (vnhost_load_busy, as game.html). Writes OUT/result.json,
// console.log and PNGs (title, story-N, last).
//
//   node tests/corpus.mjs URL OUT [STEPS]
//
// STEPS (client-area pixels of the game window, run once the title shows):
//   "click:x,y wait:3 key:Return wait:5" (move:x,y: the mouse only) ; default: nothing (story clicks start right away)
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

// a web game (NW.js: web/src/html.ts) runs in an iframe, without sake: shots of the page, and
// Playwright's mouse and keyboard
const isWeb = () => page.$("#frame").then((e) => !!e, () => false);
let web = false;
async function webFrame(full) {
  const b64 = (await page.screenshot().catch(() => null))?.toString("base64");
  if (!b64) return null;
  return page.evaluate(async ([b64, full]) => {
    const img = await createImageBitmap(await (await fetch(`data:image/png;base64,${b64}`)).blob());
    const c = new OffscreenCanvas(img.width, img.height).getContext("2d");
    c.drawImage(img, 0, 0);
    const d = c.getImageData(0, 0, img.width, img.height).data;
    const [ow, oh] = [img.width, img.height];
    const colors = new Set();
    for (let i = Math.floor(oh / 3) * ow * 4; i < Math.floor((2 * oh) / 3) * ow * 4; i += 4 * 97)
      colors.add(d[i] | (d[i + 1] << 8) | (d[i + 2] << 16));
    let hsh = 0;
    for (let i = 0; i < d.length; i += 4093) hsh = (hsh * 31 + d[i]) | 0;
    const r = { w: ow, h: oh, view: [0, 0, ow, oh], colors: colors.size, hash: hsh, d3d: false, busy: 0 };
    if (full) {
      // (BGRA, as sake's shots)
      for (let i = 0; i < d.length; i += 4) [d[i], d[i + 2]] = [d[i + 2], d[i]];
      let s = "";
      for (let i = 0; i < d.length; i += 0x8000) s += String.fromCharCode(...d.subarray(i, i + 0x8000));
      r.data = btoa(s);
    }
    return r;
  }, [b64, full]).catch((e) => ({ err: String(e) }));
}

// the game window's client area as the page shows it (+ Direct3D overlay); with full=false only stats
async function frame(full) {
  if (web || (web = await isWeb())) return webFrame(full);
  return page.evaluate(async (full) => {
    const m = window.vnvmSake;
    if (!m || !m._vnhost_render_shot) return null;
    // what the page shows (the game window's client area, Direct3D frames included), at the
    // screen's pixels: crates/kura/src/render
    const p = m._malloc(40) >>> 0;
    const first = m._vnhost_render_shot(p);
    for (let k = 0; k < 100 && m._vnhost_render_shot(p) === first; k++) await new Promise((r) => setTimeout(r, 20));
    const [ptr, ow, oh] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3));
    m._vnhost_display_view(p);
    const v = Array.from(new Int32Array(m.wasmMemory.buffer, p, 4));
    m._vnhost_render_info(p);
    const o = new Uint32Array(m.wasmMemory.buffer, p, 8)[4] > 0;
    m._free(p);
    if (!ptr || !ow || !oh) return null;
    const outp = new Uint8Array(m.wasmMemory.buffer, ptr, ow * oh * 4).slice();
    const colors = new Set();
    // ~3000 samples of the middle third, whatever the window size
    const step = Math.max(1, Math.floor((ow * oh) / 3 / 3000)) * 4 + 4;
    for (let i = Math.floor(oh / 3) * ow * 4; i < Math.floor((2 * oh) / 3) * ow * 4; i += step)
      colors.add(outp[i] | (outp[i + 1] << 8) | (outp[i + 2] << 16));
    // a grey picture has at most 256 colours: count it by its greys (KAG3's demo)
    let greys = 0;
    for (const c of colors) if (Math.abs((c & 255) - ((c >> 8) & 255)) < 3 && Math.abs((c & 255) - (c >> 16)) < 3) greys++;
    const ncolors = greys > 100 ? Math.max(colors.size, 301) : colors.size;
    let hsh = 0;
    for (let i = 0; i < outp.length; i += 4093) hsh = (hsh * 31 + outp[i]) | 0;
    const r = { w: ow, h: oh, view: v, colors: ncolors, hash: hsh, d3d: !!o, busy: m._vnhost_load_busy?.() ?? 0 };
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
const WEB_KEYS = { 0x0d: "Enter", 0x20: " ", 0x1b: "Escape", 0x28: "ArrowDown", 0x26: "ArrowUp", 0x11: "Control" };
async function input(kind, x, y, d) {
  if (web) {
    const k = WEB_KEYS[d & 0xffff] ?? String.fromCharCode(d & 0xffff).toLowerCase();
    if (kind === 1) await page.mouse.move(x, y);
    else if (kind === 2) await page.mouse.down();
    else if (kind === 3) await page.mouse.up();
    else if (kind === 5) await page.keyboard.down(k);
    else if (kind === 6) await page.keyboard.up(k);
    return;
  }
  await page.evaluate(([k, x, y, d]) => window.vnvmSake?._vnhost_input_push(k, x, y, d), [kind, x, y, d]).catch(() => {});
}
let view = [0, 0, 640, 480];
// x, y in client pixels, or below 1: a fraction of the client size (dialogs whose size varies)
async function click(x, y) {
  if (x < 1) x = Math.round(x * (view[2] - view[0]));
  if (y < 1) y = Math.round(y * (view[3] - view[1]));
  const X = view[0] + x, Y = view[1] + y;
  // (held 0.1 s like a finger: games polling the button state at a few frames a second miss
  // an instant press)
  await input(1, X, Y, 0); await input(2, X, Y, 0); await sleep(0.1); await input(3, X, Y, 0);
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
res.titleHash = tf?.hash ?? null;
save();

// 2. the title's steps
for (const s of steps.split(/[ ;]+/).filter(Boolean)) {
  const [op, a = ""] = s.split(":");
  if (op === "click") { const [x, y] = a.split(",").map(Number); await click(x, y); }
  else if (op === "move") { const [x, y] = a.split(",").map(Number); await input(1, view[0] + x, view[1] + y, 0); }
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
// the story screens left the title (the steps started the game)
res.leftTitle = [...hashes].some((h) => h !== res.titleHash);
res.crashed = crashed;
res.end = now();
save();
await browser.close();
