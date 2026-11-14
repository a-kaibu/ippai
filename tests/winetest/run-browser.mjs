// Runs Wine's test programs in headless Chromium (WebGPU) through play.html, for the DLLs that
// need a GPU (Direct3D, DXGI, Direct2D, DirectWrite, DirectDraw, OpenGL): sake's node build has
// none. tests/winetest/run.py --browser calls it with a job file and parses what it writes, as it
// does for node runs. Packs the test programs into a scratch store and serves it with masu
// (the way tests/winref/run-browser.mjs does).
//
//   node tests/winetest/run-browser.mjs <bin dir> <jobs file> <out dir> <timeout s> [pages]
//   jobs file: "<dll> <exe name> <test>" lines; out: <dll>:<test>.txt (every console line of the
//   program, then "exit code N" or "timeout")
//   MASU=path (default target/release/masu), WEB=dir (web app instead of the built-in one),
//   PLAYWRIGHT=module path, FONTS=dir (drive_c\windows\Fonts), PORT (default 8791),
//   GUEST_ENV="NAME=value ..." (added to SAKE_NO_EXEC=1 SAKE_PROC_STUBS=1)
import { execFileSync, spawn } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const [binDir, jobsFile, outDir, timeoutS = "120", pages = "2"] = process.argv.slice(2);
const masu = process.env.MASU ?? join(root, "target/release/masu");
const port = Number(process.env.PORT ?? 8791);
const jobs = readFileSync(jobsFile, "utf8").split("\n").filter(Boolean).map((l) => l.split(" "));
mkdirSync(outDir, { recursive: true });

const work = mkdtempSync(join(tmpdir(), "winetest-browser-"));
const store = join(work, "store");
for (const d of ["windows/temp", "windows/system32", "users/user/desktop"]) mkdirSync(join(work, "prefix/drive_c", d), { recursive: true });
if (process.env.FONTS) cpSync(process.env.FONTS, join(work, "prefix/drive_c/windows/Fonts"), { recursive: true });
mkdirSync(join(work, "bin"));
for (const exe of new Set(jobs.map((j) => j[1]))) cpSync(join(binDir, exe), join(work, "bin", exe));
execFileSync(masu, ["--store", store, "pack", join(work, "prefix"), "prefix32"], { stdio: "ignore" });
execFileSync(masu, ["--store", store, "pack", join(work, "bin"), "winetest"], { stdio: "ignore" });
const serveArgs = ["--store", store, "serve", "--foreground", "--listen", `127.0.0.1:${port}`, "--logs", join(work, "logs")];
if (process.env.WEB) serveArgs.push("--web", process.env.WEB);
const server = spawn(masu, serveArgs, { stdio: "ignore" });
const base = `http://127.0.0.1:${port}`;
for (let i = 0; ; i++) {
  try {
    if ((await fetch(`${base}/play.html`)).ok) break;
  } catch {}
  if (i > 100) throw new Error("masu serve did not start");
  await new Promise((r) => setTimeout(r, 100));
}

const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;
const browser = await chromium.launch({ args: ["--enable-unsafe-webgpu"] });
const guestEnv = ["SAKE_NO_EXEC=1", "SAKE_PROC_STUBS=1", ...(process.env.GUEST_ENV ?? "").split(/\s+/).filter(Boolean)];
const env = guestEnv.map((e) => `&env=${encodeURIComponent(e)}`).join("");

async function runOne([dll, exe, test]) {
  // a context per test: nothing (saves, storage) carries over
  const ctx = await browser.newContext();
  const page = await ctx.newPage();
  const lines = [];
  let end = "timeout";
  const done = new Promise((ok) => {
    page.on("console", (m) => {
      const t = m.text();
      if (m.type() === "log" && /^exit code (-?\d+)$/.test(t)) {
        end = t;
        ok();
      } else if (m.type() === "warning" || m.type() === "error") lines.push(t);
    });
    page.on("pageerror", (e) => lines.push(`RuntimeError (page): ${e.message}`));
    page.on("crash", () => {
      lines.push("RuntimeError: the page crashed");
      end = "exit code crashed";
      ok();
    });
  });
  const t0 = Date.now();
  const args = encodeURIComponent(test);
  await page.goto(`${base}/play.html?autostart&headless&nosave${env}&mount=/masu/game=winetest&args=${args}&run=Z:\\masu\\game\\${exe}`);
  await Promise.race([done, new Promise((r) => setTimeout(r, Number(timeoutS) * 1000))]);
  await ctx.close();
  writeFileSync(join(outDir, `${dll}:${test}.txt`), lines.map((l) => l + "\n").join("") + end + "\n");
  console.log(`${dll}:${test} ${end} ${((Date.now() - t0) / 1000).toFixed(0)}s ${lines.length} lines`);
}

const queue = [...jobs];
await Promise.all(Array.from({ length: Number(pages) }, async () => {
  while (queue.length) await runOne(queue.shift());
}));
await browser.close();
server.kill();
rmSync(work, { recursive: true, force: true });
