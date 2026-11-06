// Checkpoint pictures of the scenario benchmarks: when the game says "BENCH-SHOT <name>" (the
// screen holds still for a moment, testdata/kirikiri/scenario/first.ks), the harness takes the
// picture shown until two in a row are alike and compares a coarse picture of it (GRID cells of
// the mean colour) with the one expected (EXPECTED, committed): a few cells may differ (text
// antialiasing, a moved pixel), not a part of the screen. A picture that is not what it should be
// (or all one colour, or never still) fails the play.
// BENCH_SHOTS=DIR writes the pictures there (PNG); BENCH_SHOTS_UPDATE=1 writes the expected
// pictures from this play (look at the PNGs first: they become what's right).
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync } from "node:zlib";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "../..");
export const EXPECTED = {
  kirikiri: join(ROOT, "testdata/kirikiri/expected.json"),
  // (the same game on 吉里吉里2: its own pictures, text and transitions drawn by another engine)
  krkr2: join(ROOT, "testdata/kirikiri/expected-krkr2.json"),
};
const GRID = [32, 18];
// (a cell "differs" when a channel's mean is off by more than CELL; at most CELLS such cells and a
// mean difference of MEAN over the whole grid)
const CELL = 24, CELLS = 6, MEAN = 3;

// the picture shown, at the screen's pixels (crates/kura/src/render: vnhost_render_shot asks the
// drawing thread for the next frame's pixels; a page screenshot sees no WebGPU canvas headless)
async function grab(page, full) {
  const r = await page.evaluate(async ([full, [gw, gh]]) => {
    const m = window.vnvmSake;
    if (!m?._vnhost_render_shot) return null;
    const p = m._malloc(16) >>> 0;
    const first = m._vnhost_render_shot(p);
    for (let k = 0; k < 100 && m._vnhost_render_shot(p) === first; k++) await new Promise((r) => setTimeout(r, 20));
    const [ptr, w, h] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3));
    m._free(p);
    if (!ptr || !w || !h) return null;
    const px = new Uint8Array(m.wasmMemory.buffer, ptr, w * h * 4);
    // (the coarse picture here: the whole one crosses only when asked for)
    const grid = [];
    for (let gy = 0; gy < gh; gy++)
      for (let gx = 0; gx < gw; gx++) {
        const x0 = Math.floor((gx * w) / gw), x1 = Math.floor(((gx + 1) * w) / gw);
        const y0 = Math.floor((gy * h) / gh), y1 = Math.floor(((gy + 1) * h) / gh);
        let r = 0, g = 0, b = 0;
        for (let y = y0; y < y1; y++)
          for (let x = x0, i = (y * w + x0) * 4; x < x1; x++, i += 4) (b += px[i]), (g += px[i + 1]), (r += px[i + 2]);
        const n = (x1 - x0) * (y1 - y0);
        grid.push(Math.round(r / n), Math.round(g / n), Math.round(b / n));
      }
    let data = "";
    if (full) for (let i = 0; i < px.length; i += 0x8000) data += String.fromCharCode(...px.subarray(i, i + 0x8000));
    return { w, h, grid, data: full ? btoa(data) : "" };
  }, [full, GRID]);
  if (!r) throw new Error("no picture (vnhost_render_shot)");
  let rgb;
  if (full) {
    const bgra = Buffer.from(r.data, "base64");
    rgb = Buffer.alloc(r.w * r.h * 3);
    for (let i = 0, j = 0; i < bgra.length; i += 4, j += 3) (rgb[j] = bgra[i + 2]), (rgb[j + 1] = bgra[i + 1]), (rgb[j + 2] = bgra[i]);
  }
  return { w: r.w, h: r.h, rgb, grid: Buffer.from(r.grid) };
}

/** {w, h, rgb} -> PNG (to look at) */
export function encodePng({ w, h, rgb }) {
  const crcTable = Array.from({ length: 256 }, (_, n) => {
    for (let k = 0; k < 8; k++) n = n & 1 ? 0xedb88320 ^ (n >>> 1) : n >>> 1;
    return n >>> 0;
  });
  const crc = (b) => {
    let c = 0xffffffff;
    for (const x of b) c = crcTable[(c ^ x) & 255] ^ (c >>> 8);
    return (c ^ 0xffffffff) >>> 0;
  };
  const chunk = (t, d) => {
    const head = Buffer.alloc(8), tail = Buffer.alloc(4);
    head.writeUInt32BE(d.length), head.write(t, 4, "latin1");
    tail.writeUInt32BE(crc(Buffer.concat([head.subarray(4), d])));
    return Buffer.concat([head, d, tail]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w), ihdr.writeUInt32BE(h, 4), ihdr.set([8, 2, 0, 0, 0], 8);
  const raw = Buffer.alloc((w * 3 + 1) * h);
  for (let y = 0; y < h; y++) rgb.copy(raw, y * (w * 3 + 1) + 1, y * w * 3, (y + 1) * w * 3);
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

/** why picture a (coarse) is not picture b, or undefined */
export function differs(a, b) {
  if (a.length !== b.length) return `${a.length} != ${b.length} bytes`;
  let cells = 0, sum = 0;
  for (let i = 0; i < a.length; i += 3) {
    let m = 0;
    for (let k = 0; k < 3; k++) (m = Math.max(m, Math.abs(a[i + k] - b[i + k]))), (sum += Math.abs(a[i + k] - b[i + k]));
    if (m > CELL) cells++;
  }
  const mean = sum / a.length;
  if (cells > CELLS || mean > MEAN) return `${cells} of ${a.length / 3} cells differ, mean difference ${mean.toFixed(1)}`;
}

/** where two coarse pictures differ: "cells x0,y0-x1,y1" */
function moved(a, b) {
  if (a.w !== b.w || a.h !== b.h) return `${a.w}x${a.h} -> ${b.w}x${b.h}`;
  const [gw] = GRID;
  let x0 = 99, y0 = 99, x1 = -1, y1 = -1;
  for (let i = 0; i < a.grid.length; i += 3) {
    if (Math.max(...[0, 1, 2].map((k) => Math.abs(a.grid[i + k] - b.grid[i + k]))) <= CELL) continue;
    const x = (i / 3) % gw, y = Math.floor(i / 3 / gw);
    (x0 = Math.min(x0, x)), (y0 = Math.min(y0, y)), (x1 = Math.max(x1, x)), (y1 = Math.max(y1, y));
  }
  return `cells ${x0},${y0}-${x1},${y1} of ${GRID.join("x")}`;
}

/** (a picture all one colour: nothing drawn) */
const flat = (c) => c.every((v, i) => Math.abs(v - c[i % 3]) <= 2);

/** The shots of one game: take(page, name) while it plays, then finish() -> error or undefined */
export function shots(game) {
  const file = EXPECTED[game];
  const expected = file && existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : {};
  const dir = process.env.BENCH_SHOTS, got = {}, errors = [], pending = [];
  if (dir) mkdirSync(dir, { recursive: true });
  const take = (page, name) => {
    pending.push((async () => {
      // (two in a row alike: the frame of the state, not one on the way; alike, not the same: the
      // drawing of a still screen is not always exact to the bit from frame to frame)
      let prev, pic;
      const t0 = Date.now();
      for (let i = 0; ; i++) {
        pic = await grab(page, !!dir);
        if (prev && prev.w === pic.w && prev.h === pic.h && !differs(prev.grid, pic.grid)) break;
        if (i === 5) {
          if (dir) writeFileSync(join(dir, `${game}-${name}-moving.png`), encodePng(pic));
          return errors.push(`shot ${name}: the screen did not hold still (${moved(prev, pic)}, ${Date.now() - t0} ms)`);
        }
        prev = pic;
        await page.waitForTimeout(100);
      }
      if (dir) writeFileSync(join(dir, `${game}-${name}.png`), encodePng(pic));
      const c = pic.grid;
      got[name] = c.toString("base64");
      if (flat(c)) return errors.push(`shot ${name}: the screen is one colour`);
      if (!expected[name]) return errors.push(`shot ${name}: nothing expected (${file})`);
      const why = differs(c, Buffer.from(expected[name], "base64"));
      if (why) errors.push(`shot ${name}: not the picture expected: ${why}`);
    })().catch((e) => errors.push(`shot ${name}: ${e.message ?? e}`)));
  };
  const finish = async () => {
    await Promise.all(pending);
    if (process.env.BENCH_SHOTS_UPDATE && file) {
      const bad = errors.filter((e) => !/not the picture expected|nothing expected/.test(e));
      if (bad.length) return bad.join("; ");
      const all = { ...expected, ...got };
      writeFileSync(file, JSON.stringify(Object.fromEntries(Object.keys(all).sort().map((k) => [k, all[k]])), null, 1) + "\n");
      return;
    }
    const missing = Object.keys(expected).filter((k) => !(k in got));
    return [...errors, ...(missing.length && !errors.length ? [`shots not taken: ${missing.join(", ")}`] : [])].join("; ") || undefined;
  };
  return { take, finish, settle: () => Promise.all(pending), expects: () => Object.keys(expected).length > 0 || !!process.env.BENCH_SHOTS_UPDATE };
}
