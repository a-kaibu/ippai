// What the page shows of a game's window, and where clicks land, in Chromium against a real
// ippai serve (host/tests/screenfit.c): only the game window's client area, whole, also what of
// it is off sake's screen (AMBITIOUS MISSION after episode 1's window at x 850 of 1366 was cut
// to 516 of its 976 columns), a Direct3D 9 Present's source rectangle (俺の姿が､透明に! shows
// (0,0)-(1920,1080) of a back buffer the desktop's size), at a phone's portrait and landscape
// viewports and a desktop's; a click at a place of the page reaches the game at that place.
//
//   node tests/screen-browser.mjs [path to ippai]
//   (web/dist built with sake in it: crates/sake/build.sh, then pnpm build; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, mkdirSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-screen-"));
assert.ok(existsSync("web/dist/sake/sake.wasm"), "web/dist has no sake build");
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
const game = join(work, "game");
mkdirSync(game, { recursive: true });
execFileSync("i686-w64-mingw32-gcc", ["-O2", "host/tests/screenfit.c", "-o", join(game, "screenfit.exe"), "-lgdi32", "-ld3d9"]);
run("pack", game, "screenfit");

const port = 9300 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit" },
);
process.on("exit", () => server.kill());
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}

const RED = [255, 0, 0], BLUE = [0, 0, 255], GREEN = [0, 255, 0];
const near = (p, c) => p.every((v, i) => Math.abs(v - c[i]) <= 8);

const browser = await chromium.launch({ args: ["--enable-unsafe-webgpu"] });
let failed = false;

/** One run of the probe in `mode` at viewport `vp` (with `mobile`: a phone's). */
async function check(mode, vp, mobile, want) {
  const lines = [];
  const until = async (re, ms = 120000) => {
    const t = Date.now();
    while (!lines.some((l) => re.test(l))) {
      if (Date.now() - t > ms) throw new Error(`no line matching ${re}; last: ${lines.slice(-12).join(" | ")}`);
      await new Promise((r) => setTimeout(r, 100));
    }
    return lines.find((l) => re.test(l));
  };
  const ctx = await browser.newContext({ viewport: vp, ...(mobile ? { isMobile: true, hasTouch: true, deviceScaleFactor: 3 } : {}) });
  const page = await ctx.newPage();
  page.on("console", (m) => {
    lines.push(m.text());
    if (process.env.VERBOSE) console.log(m.text());
  });
  page.on("pageerror", (e) => lines.push(`pageerror: ${e.message}`));
  const q = new URLSearchParams({ run: "Z:\\ippai\\game\\screenfit.exe", mount: "/ippai/game=screenfit", args: mode, nosave: "" });
  await page.goto(`${base}play.html?headless&autostart&${q}`);
  await until(/^ready$/);
  // the picture shown, at the screen's pixels, once it is the window's
  const shot = async () =>
    page.evaluate(async () => {
      const m = window.vnvmSake;
      const p = m._malloc(12) >>> 0;
      const first = m._vnhost_render_shot(p);
      for (let k = 0; k < 200; k++) {
        await new Promise((r) => setTimeout(r, 20));
        if (m._vnhost_render_shot(p) !== first) break;
      }
      const [ptr, w, h] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3));
      m._free(p);
      const px = (x, y) => {
        const o = ptr + (y * w + x) * 4;
        const b = new Uint8Array(m.wasmMemory.buffer, o, 4);
        return [b[2], b[1], b[0]];
      };
      return { w, h, at: [px(2, 2), px(Math.floor(w / 2) + 4, Math.floor(h / 2)), px(w - 3, h - 3), px(Math.floor(w / 4), h - 3)] };
    });
  let s = await shot();
  for (let k = 0; k < 20 && !(s.w === 640 && s.h === 360 && near(s.at[0], RED)); k++) s = await shot();
  assert.deepEqual([s.w, s.h], [640, 360], `${mode} ${vp.width}x${vp.height}: only the game window's client area, whole`);
  for (const [i, c] of want.entries()) assert.ok(near(s.at[i], c), `${mode} ${vp.width}x${vp.height}: point ${i} is ${s.at[i]}, not ${c}`);
  // a click at client (600, 300), from where the page shows it
  const at = await page.evaluate(([cx, cy]) => {
    const m = window.vnvmSake;
    const o = m._malloc(40) >>> 0;
    m._vnhost_render_layout(o);
    const [rx, ry, rw, rh, l, t, r, b] = Array.from(new Int32Array(m.wasmMemory.buffer, o, 10));
    m._free(o);
    const c = document.querySelector("#screen").getBoundingClientRect(), dpr = window.devicePixelRatio || 1;
    // (the view is the client area: l, t is its corner on sake's screen)
    return [c.left + (rx + ((cx + 0.5) * rw) / (r - l)) / dpr, c.top + (ry + ((cy + 0.5) * rh) / (b - t)) / dpr, rw / rh];
  }, [600, 300]);
  // (the letterbox keeps the window's shape)
  assert.ok(Math.abs(at[2] - 640 / 360) < 0.02, `${mode}: shape ${at[2]}`);
  await page.mouse.click(at[0], at[1]);
  const c = await until(/^click /, 20000);
  const [x, y] = c.slice(6).split(",").map(Number);
  assert.ok(Math.abs(x - 600) <= 2 && Math.abs(y - 300) <= 2, `${mode} ${vp.width}x${vp.height}: the click reached ${c}`);
  assert.ok(!lines.some((l) => /pageerror|panicked/.test(l)), lines.filter((l) => /pageerror|panicked/.test(l)).join("\n"));
  await ctx.close();
}

try {
  for (const [vp, mobile] of [
    [{ width: 1280, height: 720 }, false],
    [{ width: 360, height: 800 }, true],
    [{ width: 800, height: 360 }, true],
  ]) {
    // the GDI window off the screen: red left, blue right, green at its right edge (off the screen)
    await check("gdi", vp, mobile, [RED, BLUE, GREEN, RED]);
    // Direct3D 9: the source rectangle (all red); the same partly off the screen
    await check("d3d9", vp, mobile, [RED, RED, RED, RED]);
    await check("d3d9off", vp, mobile, [RED, RED, RED, RED]);
    console.log(`ok   the game window only, whole, and its clicks at ${vp.width}x${vp.height}${mobile ? " (phone)" : ""}`);
  }
} catch (e) {
  failed = true;
  console.error(`FAIL ${e.stack ?? e}`);
} finally {
  await browser.close();
  server.kill();
}
process.exit(failed ? 1 : 0);
