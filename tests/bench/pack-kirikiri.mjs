// The KiriKiri benchmark's game (suite.mjs's "kirikiri" group, played by scenario.mjs):
// testdata/kirikiri (a small KAG3 game, see its scenario/first.ks) packed into data.xp3 as games
// ship, next to 吉里吉里Z's tvpwin32.exe and the plugins it links; or, the same game for the
// "krkr2" group, next to 吉里吉里2's krkr.eXe (its scripts in UTF-16: 吉里吉里2 reads text without
// a BOM as Shift_JIS).
//
//   node tests/bench/pack-kirikiri.mjs OUT_DIR KRKRZ_DIR   (writes OUT_DIR/kirikiri/)
//     KRKRZ_DIR: the krkrz release (tests/bench/fetch-krkrz.sh)
//   node tests/bench/pack-kirikiri.mjs OUT_DIR KRKR2_DIR   (writes OUT_DIR/krkr2/)
//     KRKR2_DIR: 吉里吉里2 2.32r2's kirikiri2/ (tests/bench/fetch-krkr2.sh)
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { deflateSync } from "node:zlib";

const [out, krkrz] = process.argv.slice(2);
if (!krkrz) throw new Error("usage: pack-kirikiri.mjs OUT_DIR KRKRZ_DIR");
const krkr2 = existsSync(join(krkrz, "krkr.eXe"));
const game = join(dirname(fileURLToPath(import.meta.url)), "../../testdata/kirikiri");

const adler32 = (b) => {
  let a = 1, s = 0;
  for (let i = 0; i < b.length; i += 4096) {
    for (let j = i; j < Math.min(i + 4096, b.length); j++) (a += b[j]), (s += a);
    (a %= 65521), (s %= 65521);
  }
  return ((s << 16) | a) >>> 0;
};

// the files, scripts compressed (pictures and sounds are already)
const files = [];
const walk = (dir, at) => {
  for (const f of readdirSync(dir).sort()) {
    const p = join(dir, f), name = at ? `${at}/${f}` : f;
    if (statSync(p).isDirectory()) walk(p, name);
    else {
      const text = /\.(tjs|ks|txt)$/.test(f);
      let data = readFileSync(p);
      if (text && krkr2) data = Buffer.concat([Buffer.from([0xff, 0xfe]), Buffer.from(data.toString("utf8"), "utf16le")]);
      files.push([name, data, text]);
    }
  }
};
walk(game, "");
// (in no useful order, as games' archives are)
let rng = 777;
const rnd = () => ((rng = (Math.imul(rng, 1103515245) + 12345) >>> 0) >>> 8);
for (let i = files.length - 1; i > 0; i--) {
  const j = rnd() % (i + 1);
  [files[i], files[j]] = [files[j], files[i]];
}

// data.xp3: "XP3\r\n \n\x1a\x8b\x67\x01", u64 index offset; the files; the index (zlib): per
// file a "File" chunk of "info" (flags, size, size in the archive, name), "segm" (one segment)
// and "adlr" (krkrz's base/XP3Archive.cpp reads it)
const u32 = (v) => { const b = Buffer.alloc(4); b.writeUInt32LE(v); return b; };
const u64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); return b; };
const chunk = (name, body) => Buffer.concat([Buffer.from(name, "latin1"), u64(body.length), body]);
const parts = [Buffer.from([0x58, 0x50, 0x33, 0x0d, 0x0a, 0x20, 0x0a, 0x1a, 0x8b, 0x67, 0x01]), null];
let at = 19;
const index = [];
for (const [name, data, compress] of files) {
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
const archive = Buffer.concat(parts);

const dir = join(out, krkr2 ? "krkr2" : "kirikiri");
mkdirSync(join(dir, "plugin"), { recursive: true });
writeFileSync(join(dir, "data.xp3"), archive);
if (krkr2) {
  // (KAGParser and menu are built into 吉里吉里2)
  copyFileSync(join(krkrz, "krkr.eXe"), join(dir, "krkr.eXe"));
  copyFileSync(join(krkrz, "plugin", "extrans.dll"), join(dir, "plugin", "extrans.dll"));
  copyFileSync(join(krkrz, "..", "license.txt"), join(dir, "license.txt"));
} else {
  copyFileSync(join(krkrz, "tvpwin32.exe"), join(dir, "tvpwin32.exe"));
  for (const p of ["KAGParser.dll", "menu.dll", "extrans.dll"]) copyFileSync(join(krkrz, "plugin", p), join(dir, "plugin", p));
  // (krkrz's license asks for its notice next to the binaries)
  copyFileSync(join(krkrz, "license.txt"), join(dir, "license.txt"));
}
console.log(`${krkr2 ? "krkr2" : "kirikiri"}/data.xp3: ${files.length} files, ${(archive.length / 1048576).toFixed(1)} MiB`);
