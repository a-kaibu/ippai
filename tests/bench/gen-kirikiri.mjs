// The KiriKiri benchmark's game (suite.mjs's "kirikiri" group, played by scenario.mjs): a small
// visual novel written for 吉里吉里Z (krkrz) with KAG3, its pictures and sounds made here
// (procedural, with noise so they don't compress to nothing), packed in data.xp3 as games ship.
// It plays as scenario.exe does: a title, lines with voices, scene changes (crossfades and a rule
// transition), a choice, a save; started again with -mode=load it restores the save. KAG's
// Debug.message lines (krkrz prints them to stdout) are the harness's BENCH-STATE / BENCH-WAIT.
//
//   node tests/bench/gen-kirikiri.mjs OUT_DIR KRKRZ_DIR KAG3_DIR
//     KRKRZ_DIR: the krkrz release (tvpwin32.exe, plugin/), KAG3_DIR: krkrz/kag3's data/
//     (tests/bench/fetch-krkrz.sh fetches both); writes OUT_DIR/kirikiri/
import { copyFileSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const [out, krkrz, kag3] = process.argv.slice(2);
if (!kag3) throw new Error("usage: gen-kirikiri.mjs OUT_DIR KRKRZ_DIR KAG3_DIR");
const W = 1280, H = 720;

let rng = 777;
const rnd = () => ((rng = (Math.imul(rng, 1103515245) + 12345) >>> 0) >>> 8);

// ---- files ----
const crcTable = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (b) => {
  let c = 0xffffffff;
  for (const x of b) c = crcTable[(c ^ x) & 255] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const adler32 = (b) => {
  let a = 1, s = 0;
  for (let i = 0; i < b.length; i += 4096) {
    for (let j = i; j < Math.min(i + 4096, b.length); j++) (a += b[j]), (s += a);
    (a %= 65521), (s %= 65521);
  }
  return ((s << 16) | a) >>> 0;
};

/** w x h pixels (r, g, b, a bytes), rows filtered "Sub" */
function png(w, h, rgba, alpha) {
  const bpp = alpha ? 4 : 3, raw = Buffer.alloc(h * (w * bpp + 1));
  for (let y = 0, o = 0; y < h; y++) {
    raw[o++] = 1;
    for (let x = 0; x < w; x++)
      for (let c = 0; c < bpp; c++, o++) raw[o] = (rgba[(y * w + x) * 4 + c] - (x ? rgba[(y * w + x - 1) * 4 + c] : 0)) & 255;
  }
  const chunk = (type, data) => {
    const b = Buffer.alloc(12 + data.length);
    b.writeUInt32BE(data.length, 0);
    b.write(type, 4, "latin1");
    data.copy(b, 8);
    b.writeUInt32BE(crc32(b.subarray(4, 8 + data.length)), 8 + data.length);
    return b;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0), ihdr.writeUInt32BE(h, 4);
  ihdr.set([8, alpha ? 6 : 2, 0, 0, 0], 8);
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(raw, { level: 6 })), chunk("IEND", Buffer.alloc(0))]);
}

function wav(seconds, rate, channels, f0) {
  const n = Math.round(seconds * rate), b = Buffer.alloc(44 + n * channels * 2);
  b.write("RIFF", 0, "latin1"), b.writeUInt32LE(36 + n * channels * 2, 4), b.write("WAVEfmt ", 8, "latin1");
  b.writeUInt32LE(16, 16), b.writeUInt16LE(1, 20), b.writeUInt16LE(channels, 22), b.writeUInt32LE(rate, 24);
  b.writeUInt32LE(rate * channels * 2, 28), b.writeUInt16LE(channels * 2, 32), b.writeUInt16LE(16, 34);
  b.write("data", 36, "latin1"), b.writeUInt32LE(n * channels * 2, 40);
  for (let i = 0; i < n; i++) {
    const t = i / rate, env = Math.min(1, t * 20, (seconds - t) * 20);
    const v = (Math.sin(2 * Math.PI * f0 * t) * 0.5 + Math.sin(2 * Math.PI * f0 * 1.5 * t) * 0.2 + ((rnd() & 255) / 255 - 0.5) * 0.05) * env;
    for (let c = 0; c < channels; c++) b.writeInt16LE(Math.round(v * 12000), 44 + (i * channels + c) * 2);
  }
  return b;
}

// ---- pictures ----
function background(seed) {
  const p = Buffer.alloc(W * H * 4);
  for (let y = 0; y < H; y++)
    for (let x = 0; x < W; x++) {
      const hill = ((H * 2) / 3 + (60 * Math.sin(x / (90 + seed * 13) + seed))) | 0, n = rnd() & 15;
      const sky = y < hill, o = (y * W + x) * 4;
      p[o] = sky ? 40 + (y * 120) / H + ((seed * 37) % 80) + (n >> 2) : 30 + n + ((seed * 20) % 60);
      p[o + 1] = sky ? 70 + (y * 100) / H + (n >> 2) : 110 + ((x + seed * 50) % 40) + n;
      p[o + 2] = sky ? 210 - (y * 60) / H - ((seed * 23) % 50) : 40 + n;
      p[o + 3] = 255;
    }
  return png(W, H, p, false);
}
function character(w, h, hue, face) {
  const p = Buffer.alloc(w * h * 4);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const dx = (x - w / 2) / (w * 0.42), dy = (y - h * 0.55) / (h * 0.47);
      let d = dx * dx + dy * dy;
      const hd = (x - w / 2) ** 2 / (w * 0.2) ** 2 + (y - h * 0.18) ** 2 / (h * 0.12) ** 2;
      if (hd < d) d = hd;
      const mouth = Math.abs(y - h * (0.22 + face * 0.01)) < 3 && Math.abs(x - w / 2) < 10 + face * 4, o = (y * w + x) * 4;
      p[o] = mouth ? 160 : (128 + 100 * (hue & 1) + (y * 27) / h + (rnd() & 3)) & 255;
      p[o + 1] = mouth ? 40 : (128 + 100 * ((hue >> 1) & 1) - (x * 30) / w) & 255;
      p[o + 2] = mouth ? 60 : (128 + 100 * ((hue >> 2) & 1)) & 255;
      p[o + 3] = d < 0.9 ? 255 : d < 1 ? ((1 - d) * 2550) | 0 : 0;
    }
  return png(w, h, p, true);
}
// a universal transition's rule: what turns first is dark (a diagonal wipe with noise)
function rule() {
  const p = Buffer.alloc(W * H * 4);
  for (let y = 0; y < H; y++)
    for (let x = 0; x < W; x++) {
      const v = Math.min(255, Math.max(0, ((x + y) * 255) / (W + H) + (rnd() & 15) - 8)) | 0, o = (y * W + x) * 4;
      p[o] = p[o + 1] = p[o + 2] = v;
      p[o + 3] = 255;
    }
  return png(W, H, p, false);
}

const files = new Map(); // name in the archive -> [bytes, compress]
const text = (name, s) => files.set(name, [Buffer.from(s, "utf8"), true]);
files.set("bgimage/title.png", [background(0), false]);
for (let i = 1; i <= 6; i++) files.set(`bgimage/bg${i}.png`, [background(i), false]);
for (let c = 0; c < 3; c++) for (let f = 0; f < 3; f++) files.set(`fgimage/ch${c}_${f}.png`, [character(400, 640, c + 1, f), false]);
files.set("rule/wipe.png", [rule(), false]);
for (let i = 0; i < 24; i++) files.set(`sound/voice${i}.wav`, [wav(1.2 + (i % 3) * 0.4, 22050, 1, 220 + i * 15), false]);
for (let i = 0; i < 2; i++) files.set(`bgm/bgm${i}.wav`, [wav(12, 44100, 2, 110 + i * 55), false]);

// ---- KAG3, its config for this game ----
const walk = (dir, at = "") => {
  for (const f of readdirSync(dir)) {
    const p = `${dir}/${f}`, name = at ? `${at}/${f}` : f;
    if (statSync(p).isDirectory()) walk(p, name);
    else if (/\.(tjs|png|asd)$/.test(f)) files.set(name, [readFileSync(p), /\.tjs$/.test(f)]);
  }
};
walk(`${kag3}/system`, "system");
files.set("startup.tjs", [readFileSync(`${kag3}/startup.tjs`), true]);
let config = readFileSync(`${kag3}/system/Config.tjs`, "utf8");
const set = (key, v) => {
  const re = new RegExp(`^;${key.replace(".", "\\.")} = .*;`, "m");
  if (!re.test(config)) throw new Error(`Config.tjs: no ${key}`);
  config = config.replace(re, `;${key} = ${v};`);
};
set("scWidth", W), set("scHeight", H), set("menu.visible", "false"), set("defaultFontSize", 28);
set("scPositionX.left", 320), set("scPositionX.left_center", 480), set("scPositionX.center", 640);
set("scPositionX.right_center", 800), set("scPositionX.right", 960);
text("system/Config.tjs", config);

// ---- the scenario ----
// (the harness's lines: "BENCH-STATE what step" when the game shows something, "BENCH-WAIT step
// what" when it waits for a click; `step` counts both)
const ks = [];
const say = (...l) => ks.push(...l);
say(
  "[macro name=st][eval exp=\"Debug.message('BENCH-STATE ' + mp.w + ' ' + (++tf.step))\"][endmacro]",
  "[macro name=bw][eval exp=\"Debug.message('BENCH-WAIT ' + (++tf.step) + (mp.w === void ? '' : ' ' + mp.w))\"][endmacro]",
  "[eval exp=\"tf.step = 0\"]",
  "[position layer=message0 page=fore left=40 top=520 width=1200 height=180 marginl=24 margint=20 opacity=160]",
  "[position layer=message0 page=back left=40 top=520 width=1200 height=180 marginl=24 margint=20 opacity=160]",
  "[if exp=\"System.getArgument('-mode') == 'load'\"][eval exp=\"tf.loaded = true\"][load place=0][s][endif]",
  "*title|タイトル",
  "[image storage=title layer=base page=fore]",
  "[nowait]吉里吉里ベンチ　―　クリックではじめる[endnowait]",
  "[st w=title][bw][waitclick]",
  "[playbgm storage=bgm0.wav loop=true]",
);
let voice = 0, line = 0;
const lines = (n, ch) => {
  for (let i = 0; i < n; i++) {
    // (a different face every other line)
    const face = ch !== undefined && i % 2 ? `[image storage=ch${ch}_${(i >> 1) % 3} layer=0 page=fore visible=true left=440 top=80]` : "";
    say(`[cm]${face}[playse storage=voice${voice++ % 24}.wav buf=0][nowait]「これは${++line}行目の台詞です。吉里吉里Zで書いた小さなノベルゲーム。」[endnowait]`);
    say(i === 2 ? "[st w=line][bw w=idle][p]" : "[st w=line][bw][p]");
  }
};
const scene = (bg, ch, how) => {
  say(`[cm][backlay][image storage=bg${bg} layer=base page=back]`);
  say(ch === undefined ? "[freeimage layer=0 page=back]" : `[image storage=ch${ch}_0 layer=0 page=back visible=true left=440 top=80]`);
  say(how === "rule" ? "[trans method=universal rule=wipe vague=64 time=600][wt]" : "[trans method=crossfade time=500][wt]");
  say("[st w=scene][bw][waitclick]");
};
scene(1, 0), lines(5, 0);
scene(2, 1, "rule"), lines(4, 1);
scene(3, 2), lines(4, 2);
say("[playbgm storage=bgm1.wav loop=true]");
// the choice: two links on a message layer over the whole screen (the first under the harness's
// click, 640,430)
say(
  "[cm][current layer=message1 page=fore][position layer=message1 page=fore left=0 top=0 width=1280 height=720 frame=\"\" opacity=0 visible=true]",
  "[locate x=400 y=400][font size=48][link target=*left]　左の道を行く　[endlink][locate x=400 y=520][link target=*right]　右の道を行く　[endlink][resetfont]",
  "[bw w=choice][s]",
  "*left",
  "[er][layopt layer=message1 visible=false][current layer=message0]",
);
scene(4, 0, "rule");
say("[jump target=*after]", "*right", "[er][layopt layer=message1 visible=false][current layer=message0]");
scene(5, 1);
say("*after");
lines(3, 0);
// the save: KAG saves at the last label and a load starts from it again
say(
  "*saved|保存した場面",
  "[if exp=\"tf.loaded\"][nowait]（ロードしました）[endnowait][st w=restored][eval exp=\"Debug.message('BENCH-END')\"][s][endif]",
  "[save place=0][st w=saved][bw][p]",
);
scene(6, 2), lines(4, 2);
say("[stopbgm][st w=end][eval exp=\"Debug.message('BENCH-END')\"][s]");
text("scenario/first.ks", ks.join("\n") + "\n");

// ---- data.xp3 ----
// "XP3\r\n \n\x1a\x8b\x67\x01", u64 index offset; the files; the index (zlib): per file a "File"
// chunk of "info" (flags, size, size in the archive, name), "segm" (one segment) and "adlr"
function xp3(entries) {
  const parts = [Buffer.from([0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 0x01]), Buffer.alloc(8)];
  let at = 19;
  const index = [];
  const u32 = (v) => { const b = Buffer.alloc(4); b.writeUInt32LE(v); return b; };
  const u64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
  const chunk = (name, body) => Buffer.concat([Buffer.from(name, "latin1"), u64(body.length), body]);
  for (const [name, [data, compress]] of entries) {
    const stored = compress ? deflateSync(data) : data;
    parts.push(stored);
    const n = Buffer.from(name, "utf16le"), nl = Buffer.alloc(2);
    nl.writeUInt16LE(name.length);
    index.push(chunk("File", Buffer.concat([
      chunk("info", Buffer.concat([u32(0), u64(data.length), u64(stored.length), nl, n])),
      chunk("segm", Buffer.concat([u32(compress ? 1 : 0), u64(at), u64(data.length), u64(stored.length)])),
      chunk("adlr", u32(adler32(data))),
    ])));
    at += stored.length;
  }
  const raw = Buffer.concat(index), packed = deflateSync(raw);
  parts[1] = u64(at);
  parts.push(Buffer.from([1]), u64(packed.length), u64(raw.length), packed);
  return Buffer.concat(parts);
}
// (in no useful order, as games' archives are)
const entries = [...files];
for (let i = entries.length - 1; i > 0; i--) {
  const j = rnd() % (i + 1);
  [entries[i], entries[j]] = [entries[j], entries[i]];
}
const game = `${out}/kirikiri`;
mkdirSync(`${game}/plugin`, { recursive: true });
const archive = xp3(entries);
writeFileSync(`${game}/data.xp3`, archive);
copyFileSync(`${krkrz}/tvpwin32.exe`, `${game}/tvpwin32.exe`);
for (const p of ["KAGParser.dll", "menu.dll"]) copyFileSync(`${krkrz}/plugin/${p}`, `${game}/plugin/${p}`);
copyFileSync(`${krkrz}/license.txt`, `${game}/license.txt`);
console.log(`kirikiri/data.xp3: ${entries.length} files, ${(archive.length / 1048576).toFixed(1)} MiB`);
