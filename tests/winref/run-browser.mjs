// Runs the winref probes that need a GPU (build.sh lists them in <exe dir>/browser.txt) on sake
// in headless Chromium (WebGPU), as run.sh does under node for the rest: <out dir>/<probe>/
// stdout.txt (the program's console lines) and exit. Packs the exes into a scratch store and
// serves it with ippai.
//
//   node tests/winref/run-browser.mjs <exe dir> <out dir> [name-filter]
//   IPPAI=path (default target/release/ippai), WEB=dir (web app instead of the built-in one),
//   PLAYWRIGHT=module path, FONTS=dir (drive_c\windows\Fonts), IPPAI_JIT=0, PORT (default 8790),
//   GUEST_ENV="NAME=value ..." (the program's environment), DEBUG_CONSOLE=1 (every console line
//   of the page into stderr.txt), BROWSER64=1
import { execFileSync, spawn } from "node:child_process";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync, cpSync, readdirSync, copyFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const [exeDir, outDir, filter = ""] = process.argv.slice(2);
const ippai = process.env.IPPAI ?? join(root, "target/release/ippai");
const port = Number(process.env.PORT ?? 8790);
const list = join(exeDir, "browser.txt");
// (x64 probes need the x64 CRT in the page's prefix, not there yet: BROWSER64=1 runs them)
const names = existsSync(list) ? readFileSync(list, "utf8").split(/\s+/).filter((n) => n && n.includes(filter) && (process.env.BROWSER64 || !n.endsWith("-64"))) : [];
if (!names.length) process.exit(0);

const work = mkdtempSync(join(tmpdir(), "winref-"));
const store = join(work, "store");
mkdirSync(join(work, "prefix/drive_c/windows/temp"), { recursive: true });
if (process.env.FONTS) cpSync(process.env.FONTS, join(work, "prefix/drive_c/windows/Fonts"), { recursive: true });
mkdirSync(join(work, "probes"));
for (const n of names) copyFileSync(join(exeDir, `${n}.exe`), join(work, "probes", `${n}.exe`));
execFileSync(ippai, ["--store", store, "pack", join(work, "prefix"), "prefix32"], { stdio: "ignore" });
execFileSync(ippai, ["--store", store, "pack", join(work, "probes"), "winref"], { stdio: "ignore" });
const serveArgs = ["--store", store, "serve", "--foreground", "--listen", `127.0.0.1:${port}`, "--logs", join(work, "logs")];
if (process.env.WEB) serveArgs.push("--web", process.env.WEB);
const server = spawn(ippai, serveArgs, { stdio: "ignore" });
const base = `http://127.0.0.1:${port}`;
for (let i = 0; ; i++) {
  try {
    if ((await fetch(`${base}/play.html`)).ok) break;
  } catch {}
  if (i > 100) throw new Error("ippai serve did not start");
  await new Promise((r) => setTimeout(r, 100));
}

const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
const browser = await chromium.launch({ args: ["--enable-unsafe-webgpu"] });
// page warnings that are not the program's (console.warn of the page itself)
const pageNoise = /^(crashlog:|plugin error|package\.json:|saves:|sake:)/;
let failed = 0;
for (const n of names) {
  const out = join(outDir, n);
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const page = await browser.newPage();
  const lines = [], sakeLines = [];
  let code = "?";
  const done = new Promise((ok) => {
    page.on("console", (m) => {
      const t = m.text();
      if (process.env.DEBUG_CONSOLE) sakeLines.push(`[${m.type()}] ${t}`);
      if (m.type() === "warning") {
        if (pageNoise.test(t)) sakeLines.push(t);
        else lines.push(t);
      } else if (m.type() === "log" && /^exit code (-?\d+)$/.test(t)) {
        code = t.match(/(-?\d+)$/)[1];
        ok();
      }
    });
    page.on("crash", ok);
  });
  const env = [...(process.env.IPPAI_JIT === "0" ? ["IPPAI_JIT=0"] : []), ...(process.env.GUEST_ENV ?? "").split(/\s+/).filter(Boolean)].map((e) => `&env=${encodeURIComponent(e)}`).join("");
  const t0 = Date.now();
  await page.goto(`${base}/play.html?autostart&headless${env}&mount=/vnvm/game=winref&run=Z:\\vnvm\\game\\${n}.exe`);
  await Promise.race([done, new Promise((r) => setTimeout(r, 180000))]);
  await page.close();
  writeFileSync(join(out, "stdout.txt"), lines.map((l) => l + "\n").join(""));
  writeFileSync(join(out, "stderr.txt"), sakeLines.map((l) => l + "\n").join(""));
  writeFileSync(join(out, "exit"), `${code}\n`);
  if (code === "?") failed++;
  console.log(`${n} exit ${code} ${((Date.now() - t0) / 1000).toFixed(0)}s ${lines.length} lines`);
}
await browser.close();
server.kill();
rmSync(work, { recursive: true, force: true });
process.exit(failed ? 1 : 0);
