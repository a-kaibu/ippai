// The game data of the scenario benchmark (tests/bench/src/scenario.c): one archive, scenario.dat,
// laid out as games' archives are: many entries in no useful order, an index at the end. Its
// pictures are procedural (bench.h's, with noise, so they don't compress to nothing) and stored
// "Sub"-filtered as PNG rows are (the game unfilters them); its sounds are 16-bit PCM.
//
//   node tests/bench/gen-scenario.mjs OUT_DIR       (writes OUT_DIR/scenario.dat, ~70 MB)
//
// Format (little-endian): "VNBA", u32 count, u32 index offset; data; the index: per entry
// name[24], u32 offset, u32 size, u32 kind (1 picture, 2 sound), u32 w (pictures; sounds: rate),
// u32 h (pictures; sounds: channels).
import { writeFileSync } from "node:fs";

const out = process.argv[2];
if (!out) throw new Error("usage: gen-scenario.mjs OUT_DIR");

let rng = 12345;
const rnd = () => ((rng = (Math.imul(rng, 1103515245) + 12345) >>> 0) >>> 8);

function background(w, h, seed) {
  const p = new Uint32Array(w * h);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const hill = ((h * 2) / 3 + (40 * (((x * (seed + 3) / 97) % 64) - 32)) / 32) | 0;
      const n = rnd() & 15;
      let r, g, b;
      if (y < hill) (r = 40 + ((y * 120) / h) + ((seed * 30) % 60) + (n >> 2)), (g = 80 + (y * 100) / h + (n >> 2)), (b = 200 - (y * 60) / h);
      else (r = 30 + n), (g = 120 + ((x + seed * 50) % 40) + n), (b = 40 + n);
      p[y * w + x] = (0xff000000 | ((r & 255) << 16) | ((g & 255) << 8) | (b & 255)) >>> 0;
    }
  return p;
}

function character(w, h, hue, face) {
  const p = new Uint32Array(w * h);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const dx = (x - w / 2) / (w * 0.42), dy = (y - h * 0.55) / (h * 0.47);
      let d = dx * dx + dy * dy;
      const hd = ((x - w / 2) ** 2) / (w * 0.2) ** 2 + ((y - h * 0.18) ** 2) / (h * 0.12) ** 2;
      if (hd < d) d = hd;
      const a = d < 0.9 ? 255 : d < 1 ? ((1 - d) * 2550) | 0 : 0;
      // (the face: a different mouth per expression)
      const mouth = Math.abs(y - h * (0.22 + face * 0.01)) < 3 && Math.abs(x - w / 2) < 10 + face * 4;
      const r = mouth ? 160 : (128 + 100 * (hue & 1) + ((y * 27) / h) + (rnd() & 3)) & 255;
      const g = mouth ? 40 : (128 + 100 * ((hue >> 1) & 1) - ((x * 30) / w)) & 255;
      const b = mouth ? 60 : (128 + 100 * ((hue >> 2) & 1)) & 255;
      p[y * w + x] = ((a << 24) | (r << 16) | (g << 8) | b) >>> 0;
    }
  return p;
}

function movieFrame(w, h, t) {
  const p = new Uint32Array(w * h);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const v = Math.sin(x * 0.02 + t * 0.2) + Math.cos(y * 0.03 - t * 0.15);
      p[y * w + x] = (0xff000000 | ((((v + 2) * 60) & 255) << 16) | ((((x + t * 4) ^ y) & 255) << 8) | ((rnd() & 31) + 100)) >>> 0;
    }
  return p;
}

// PNG "Sub": each byte minus the byte 4 to its left (the game adds them back)
function sub(p, w, h) {
  const src = new Uint8Array(p.buffer), b = new Uint8Array(src.length);
  for (let y = 0; y < h; y++) {
    const o = y * w * 4;
    for (let i = 0; i < w * 4; i++) b[o + i] = (src[o + i] - (i >= 4 ? src[o + i - 4] : 0)) & 255;
  }
  return b;
}

function sound(seconds, rate, channels, f0) {
  const n = Math.round(seconds * rate), b = new Int16Array(n * channels);
  for (let i = 0; i < n; i++) {
    const t = i / rate, env = Math.min(1, t * 20, (seconds - t) * 20);
    const v = (Math.sin(2 * Math.PI * f0 * t) * 0.5 + Math.sin(2 * Math.PI * f0 * 1.5 * t) * 0.2 + ((rnd() & 255) / 255 - 0.5) * 0.05) * env;
    for (let c = 0; c < channels; c++) b[i * channels + c] = Math.round(v * 12000);
  }
  return new Uint8Array(b.buffer);
}

const entries = [];
const pic = (name, w, h, p) => entries.push({ name, kind: 1, w, h, data: sub(p, w, h) });
for (let i = 0; i < 8; i++) pic(`bg${i}`, 1280, 720, background(1280, 720, i + 1));
for (let c = 0; c < 3; c++) for (let f = 0; f < 4; f++) pic(`ch${c}_${f}`, 400, 640, character(400, 640, c + 1, f));
for (let i = 0; i < 24; i++) entries.push({ name: `voice${i}`, kind: 2, w: 22050, h: 1, data: sound(1.5 + (i % 3) * 0.5, 22050, 1, 220 + i * 15) });
for (let i = 0; i < 2; i++) entries.push({ name: `bgm${i}`, kind: 2, w: 44100, h: 2, data: sound(12, 44100, 2, 110 + i * 55) });
// (the movie: its frames one after the other, as a movie file is read)
const movie = [];
for (let t = 0; t < 48; t++) movie.push({ name: `mv${String(t).padStart(2, "0")}`, kind: 1, w: 480, h: 270, data: sub(movieFrame(480, 270, t), 480, 270) });

// shuffled, the movie kept in one piece somewhere in the middle
for (let i = entries.length - 1; i > 0; i--) {
  const j = rnd() % (i + 1);
  [entries[i], entries[j]] = [entries[j], entries[i]];
}
entries.splice(entries.length >> 1, 0, ...movie);

const size = 12 + entries.reduce((s, e) => s + e.data.length, 0) + entries.length * 44;
const file = new Uint8Array(size), dv = new DataView(file.buffer);
let at = 12;
for (const e of entries) {
  e.offset = at;
  file.set(e.data, at);
  at += e.data.length;
}
file.set([0x56, 0x4e, 0x42, 0x41]);
dv.setUint32(4, entries.length, true);
dv.setUint32(8, at, true);
for (const e of entries) {
  for (let i = 0; i < 24; i++) file[at + i] = i < e.name.length ? e.name.charCodeAt(i) : 0;
  [e.offset, e.data.length, e.kind, e.w, e.h].forEach((v, i) => dv.setUint32(at + 24 + i * 4, v, true));
  at += 44;
}
writeFileSync(`${out}/scenario.dat`, file);
console.log(`scenario.dat: ${entries.length} entries, ${(size / 1048576).toFixed(1)} MiB`);
