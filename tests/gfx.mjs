// Graphics API tests: runs each check of host/tests (built by build-gfx.sh, packed as store
// tree "gfxtest") with "still" in headless Chromium through tests/play.mjs, then compares pixels
// of the screen against what the check draws, and its log against what it prints. Needs
// vnvm serve on BASE (default http://127.0.0.1:8090); exits 1 when anything fails.
//
//   node tests/gfx.mjs ["name" ...]    (default: all; OUT=dir keeps the screens and logs;
//                                       ENGINE is ignored (the page runs sake only);
//                                       ENV="A=1 B=2" sets environment variables)
import { execFileSync } from "node:child_process";
import { readFileSync, existsSync } from "node:fs";
import { inflateSync } from "node:zlib";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const base = process.env.BASE ?? "http://127.0.0.1:8090";
const outDir = process.env.OUT ?? "/tmp/vnvm-gfx";

// [x, y, r, g, b] on the screen play.mjs saves (the window, its client area at 4,1); triangle
// pixels are interpolated colors, so all compare within 24
const TESTS = {
  ddrawtest: {
    // color bars (Lock), GDI text on the surface (GetDC), the gradient, Blt through a clipper
    pixels: [[40, 150, 255, 0, 0], [120, 150, 0, 255, 0], [200, 150, 0, 0, 255], [280, 150, 255, 255, 0],
      [360, 150, 0, 255, 255], [440, 150, 255, 0, 255], [10, 470, 0, 7, 247], [630, 470, 0, 247, 247]],
    log: [/IDirectDraw7_CreateSurface/, /frame 60 /],
  },
  "ddrawtest full": {
    // the same in exclusive 640x480x32 with a flip chain (BltFast to the back buffer, Flip)
    pixels: [[40, 150, 255, 0, 0], [120, 150, 0, 255, 0], [200, 150, 0, 0, 255], [10, 470, 0, 7, 247], [630, 470, 0, 247, 247]],
    log: [/frame 60 /],
  },
  d3d8test: {
    // Clear, fixed-function textured quad (checker), vs.1.1/ps.1.1 triangle
    pixels: [[10, 10, 32, 32, 64], [600, 450, 32, 32, 64], [45, 45, 0, 0, 255],
      [454, 190, 236, 10, 9], [330, 374, 5, 9, 241], [580, 374, 5, 243, 6], [450, 320, 73, 88, 94]],
    log: [/frame 60 /],
  },
  d3d11test: {
    // ClearRenderTargetView, textured blended quad, triangle with a constant buffer
    pixels: [[10, 10, 26, 77, 26], [630, 470, 26, 77, 26], [60, 40, 13, 38, 77], [140, 40, 255, 255, 0],
      [320, 90, 239, 9, 8], [150, 352, 7, 12, 237], [490, 352, 7, 237, 11], [320, 280, 70, 93, 92]],
    log: [/frame 60\b/],
  },
  d2dtest: {
    // Direct2D on the D3D11 swap chain: clear, filled rectangle, ellipse outline, gradient bar,
    // DirectWrite text (its metrics in the log)
    pixels: [[10, 10, 26, 77, 26], [100, 100, 255, 0, 0], [341, 100, 0, 0, 255], [460, 100, 26, 77, 26],
      [45, 420, 252, 255, 3], [320, 420, 127, 255, 128], [595, 420, 2, 255, 253]],
    log: [/text metrics \d/, /EndDraw -> 0\b/, /frame 60\b/],
  },
  gltest: {
    // glClear, glBegin quad into an FBO, its texture drawn with client arrays, GLSL triangle
    pixels: [[10, 10, 26, 77, 26], [60, 60, 102, 26, 26], [100, 90, 255, 255, 0], [170, 90, 51, 13, 77],
      [15, 475, 255, 0, 0], [320, 115, 240, 8, 7], [175, 335, 6, 9, 240], [465, 335, 6, 241, 9], [320, 280, 64, 96, 95]],
    log: [/ARB functions ok/, /framebuffer status 0x8cd5/, /frame 60, glGetError 0\b/],
  },
  // (not graphics: the JIT's dispatch must fault on a call through a null pointer)
  nullcall: { pixels: [], log: [/nullcall: ok/] },
};

function readPng(file) {
  const b = readFileSync(file);
  let w = 0, h = 0, i = 8;
  const idat = [];
  while (i < b.length) {
    const n = b.readUInt32BE(i), type = b.toString("ascii", i + 4, i + 8);
    if (type === "IHDR") [w, h] = [b.readUInt32BE(i + 8), b.readUInt32BE(i + 12)];
    if (type === "IDAT") idat.push(b.subarray(i + 8, i + 8 + n));
    i += 12 + n;
  }
  // play.mjs writes RGB rows with filter 0
  const raw = inflateSync(Buffer.concat(idat));
  return { w, h, at: (x, y) => [...raw.subarray(y * (w * 3 + 1) + 1 + x * 3, y * (w * 3 + 1) + 4 + x * 3)] };
}

const names = process.argv.slice(2).length ? process.argv.slice(2) : Object.keys(TESTS);
let failed = 0;
for (const name of names) {
  const t = TESTS[name];
  const [exe, ...args] = name.split(" ");
  const out = `${outDir}/${name.replace(" ", "-")}`;
  const engine =
    (process.env.ENGINE ? `engine=${process.env.ENGINE}&` : "") +
    (process.env.ENV ?? "").split(" ").filter(Boolean).map((e) => `env=${encodeURIComponent(e)}&`).join("");
  const url = `${base}/play.html?${engine}autostart&headless&mount=/vnvm/game=gfxtest&args=${["still", ...args]}&run=Z:\\vnvm\\game\\${exe}.exe`;
  execFileSync("rm", ["-rf", out]);
  try {
    execFileSync("node", [`${here}/play.mjs`, url, out, "22", "20"], { stdio: "ignore", timeout: 90000 });
  } catch {}
  const errors = [];
  const log = existsSync(`${out}/console.log`) ? readFileSync(`${out}/console.log`, "utf8") : "";
  for (const re of t.log) if (!re.test(log)) errors.push(`log lacks ${re}`);
  for (const bad of [/Unhandled exception/, /\[pageerror\]/, /\[crash\]/]) if (bad.test(log)) errors.push(`log has ${bad}`);
  if (!t.pixels.length) {
  } else if (existsSync(`${out}/screen-001.png`)) {
    const png = readPng(`${out}/screen-001.png`);
    for (const [x, y, ...want] of t.pixels) {
      const got = png.at(x, y);
      if (got.some((v, k) => Math.abs(v - want[k]) > 24)) errors.push(`(${x},${y}) is ${got}, want ${want}`);
    }
  } else errors.push("no screen");
  console.log(`${errors.length ? "FAIL" : "ok  "} ${name}${errors.map((e) => `\n     ${e}`).join("")}`);
  if (errors.length) failed++;
}
console.log(`${names.length - failed}/${names.length} passed (screens and logs in ${outDir})`);
process.exit(failed ? 1 : 0);
