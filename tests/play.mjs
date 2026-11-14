// Headless test of web/play.html (sake in wasm): logs the console with timestamps and saves the
// screen kura composes (read from the wasm heap: headless Chromium can't show WebGPU canvases;
// Direct3D windows' frames are read back from the GPU)
// as a PNG every INTERVAL seconds. After the first shot, CLICKS="x,y;x,y" clicks there (page
// pixels; each held 200 ms) and KEYS="KeyA Enter" presses those keys; CLICK_EVERY="x,y" clicks there after every
// shot (to page through a game); CLICK_SEQ="x,y;;x,y" clicks its n-th entry (if any) after shot n
// instead, then CLICK_EVERY takes over (SHOT_COORDS=1: click coordinates are screenshot pixels). CHROME_ARGS adds browser flags; USER_DATA_DIR keeps the
// browser profile (its HTTP and code caches) there across runs. NET="kbit/s,ms" throttles the
// network; NET_DROP="at,seconds" cuts it off at `at` s for `seconds` (repeatable: "a,s;a,s"; a
// phone out of coverage). (The network numbers are in the run log's perf lines.)
//
//   node tests/play.mjs URL OUT_DIR [SECONDS] [INTERVAL]
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
// VIEWPORT="WxH": the page's size (default 1280x720; with &env=MASU_SCREEN%3DWxH a bigger screen)
const [vw, vh] = (process.env.VIEWPORT ?? "1280x720").split("x").map(Number);
const viewport = { width: vw, height: vh };
const browser = process.env.USER_DATA_DIR
  ? await chromium.launchPersistentContext(process.env.USER_DATA_DIR, { args, viewport })
  : await chromium.launch({ args });
const page = await browser.newPage(process.env.USER_DATA_DIR ? undefined : { viewport });
// MASH="x,y,ms,n,regex": from shot n on, clicks x,y every ms until a console line matches regex
const mash = process.env.MASH?.split(",");
let mashing = !!mash;
page.on("console", (m) => {
  appendFileSync(logFile, `${stamp()} [${m.type()}] ${m.text()}\n`);
  if (mashing && mash[4] && new RegExp(mash[4]).test(m.text())) mashing = false;
});
page.on("pageerror", (e) => appendFileSync(logFile, `${stamp()} [pageerror] ${e.message}\n`));
page.on("crash", () => appendFileSync(logFile, `${stamp()} [crash] the page crashed\n`));
if (process.env.THROTTLE) {
  // Android-like CPU: Chromium's CPU throttling (DevTools), rate = slowdown factor
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Emulation.setCPUThrottlingRate', { rate: Number(process.env.THROTTLE) });
}
// a slow connection: NET="kbit/s,latency ms" (DevTools network throttling; the page's workers'
// requests go through it too); NET_DROP's cuts through the same session (a context's setOffline
// would be undone by the throttling)
let netCdp;
const net = async (offline) => {
  const [kbps, latency] = (process.env.NET ?? "0,0").split(",").map(Number);
  netCdp ??= await page.context().newCDPSession(page);
  await netCdp.send("Network.enable");
  await netCdp.send("Network.emulateNetworkConditions", {
    offline,
    latency,
    downloadThroughput: kbps ? (kbps * 1000) / 8 : -1,
    uploadThroughput: kbps ? (kbps * 1000) / 8 : -1,
  });
};
if (process.env.NET) await net(false);
for (const drop of (process.env.NET_DROP ?? "").split(";").filter(Boolean)) {
  const [at, secs] = drop.split(",").map(Number);
  setTimeout(async () => {
    appendFileSync(logFile, `${stamp()} [net] offline for ${secs} s\n`);
    await net(true);
    setTimeout(async () => {
      await net(false);
      appendFileSync(logFile, `${stamp()} [net] online again\n`);
    }, secs * 1000);
  }, at * 1000);
}
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
    const m = window.vnvmSake;
    if (!m) return null;
    const enc = (out, w, h) => {
      let s = "";
      for (let i = 0; i < out.length; i += 0x8000) s += String.fromCharCode(...out.subarray(i, i + 0x8000));
      return { w, h, data: btoa(s) };
    };
    // what the page shows, at the screen's pixels (crates/kura/src/render)
    const p = m._malloc(12) >>> 0;
    const first = m._vnhost_render_shot(p);
    for (let k = 0; k < 100; k++) {
      await new Promise((r) => setTimeout(r, 20));
      if (m._vnhost_render_shot(p) !== first) break;
    }
    const [ptr, w, h] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3));
    m._free(p);
    if (!ptr || !w || !h) return null;
    return enc(new Uint8Array(m.wasmMemory.buffer, ptr, w * h * 4).slice(), w, h);
  });
  if (f) writeFileSync(`${out}/screen-${String(n).padStart(3, "0")}.png`, png(f.w, f.h, Buffer.from(f.data, "base64")));
}

// (hovered first: many games only take a click on a button they saw the cursor over)
async function click(x, y) {
  // SHOT_COORDS=1: x,y are pixels of the screenshots (the game window's client area), not of the page
  if (process.env.SHOT_COORDS)
    [x, y] = await page.evaluate(([sx, sy]) => {
      const m = window.vnvmSake;
      const o = m._malloc(40) >>> 0;
      m._vnhost_render_layout(o);
      const [rx, ry, rw, rh, l, t, vr, vb] = Array.from(new Int32Array(m.wasmMemory.buffer, o, 10));
      m._free(o);
      const c = document.querySelector("#screen"), b = c.getBoundingClientRect(), dpr = window.devicePixelRatio || 1;
      const fx = l + sx + 0.5, fy = t + sy + 0.5;
      return [b.left + (rx + ((fx - l) * rw) / (vr - l)) / dpr, b.top + (ry + ((fy - t) * rh) / (vb - t)) / dpr];
    }, [x, y]);
  appendFileSync(logFile, `${stamp()} [click] page ${x.toFixed(1)},${y.toFixed(1)}\n`);
  await page.mouse.move(x, y);
  await page.waitForTimeout(300);
  await page.mouse.click(x, y, { delay: 200 }); // (held: games that poll GetAsyncKeyState per frame)
}

const end = t0 + Number(seconds) * 1000;
let n = 0;
while (Date.now() < end) {
  const next = Date.now() + Number(interval) * 1000;
  if (mashing && n >= Number(mash[3]))
    while (mashing && Date.now() < next) await click(Number(mash[0]), Number(mash[1])), await page.waitForTimeout(Number(mash[2]));
  await page.waitForTimeout(Math.max(0, next - Date.now()));
  await shot(++n);
  if (n === 1) {
    for (const xy of process.env.CLICKS?.split(";") ?? []) {
      const [x, y] = xy.split(",").map(Number);
      await click(x, y);
    }
    for (const key of process.env.KEYS?.split(" ").filter(Boolean) ?? []) await page.keyboard.press(key);
  }
  // PROFILE_AT=n: a CPU profile of the page's main thread from shot n for PROFILE_SECS (10) s
  if (process.env.PROFILE_AT && n === Number(process.env.PROFILE_AT)) {
    // (and the workers': attached through the page's session, PROFILE_WORKERS=1)
    const cdp = await page.context().newCDPSession(page);
    const workers = [];
    if (process.env.PROFILE_WORKERS) {
      let id = 0;
      const pending = new Map();
      cdp.on("Target.attachedToTarget", (e) => workers.push({ session: e.sessionId, title: e.targetInfo.title || e.targetInfo.url }));
      cdp.on("Target.receivedMessageFromTarget", (e) => {
        const m = JSON.parse(e.message);
        if (m.id && pending.has(m.id)) pending.get(m.id)(m.result), pending.delete(m.id);
      });
      const send = (w, method, params = {}) =>
        new Promise((res) => {
          pending.set(++id, res);
          cdp.send("Target.sendMessageToTarget", { sessionId: w.session, message: JSON.stringify({ id, method, params }) });
        });
      await cdp.send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: false, flatten: false });
      await page.waitForTimeout(500);
      for (const w of workers) (w.send = (m, p) => send(w, m, p)), await w.send("Profiler.enable"), await w.send("Profiler.start");
    }
    await cdp.send("Profiler.enable");
    await cdp.send("Profiler.start");
    await page.waitForTimeout(Number(process.env.PROFILE_SECS ?? 10) * 1000);
    const { profile } = await cdp.send("Profiler.stop");
    writeFileSync(`${out}/page.cpuprofile`, JSON.stringify(profile));
    for (const [i, w] of workers.entries()) {
      const r = await w.send("Profiler.stop");
      if (r?.profile) writeFileSync(`${out}/worker-${i}.cpuprofile`, JSON.stringify(r.profile));
      appendFileSync(logFile, `${stamp()} [profile] worker-${i}: ${w.title}\n`);
    }
  }
  // HOLD_KEY=Control HOLD_AT=n: holds the key down from shot n on (e.g. a game's Ctrl skip)
  if (process.env.HOLD_KEY && n === Number(process.env.HOLD_AT ?? 1)) await page.keyboard.down(process.env.HOLD_KEY);
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
