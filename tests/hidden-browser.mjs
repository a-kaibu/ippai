// A game out of sight (web/src/hidden.ts): in a real Chromium window (Xvfb), the game's window
// is put behind another tab, so that it is really hidden (no animation frames: the state's GPU
// read back must come without them, host/vngpu.js). host/tests/snaptest.c as the game:
//   1. hidden: a state is kept in the auto slot, and the program stands: no step line while
//      the tab stays behind (4 s), and its clocks do not move ("clocks: ok" at its end: a gap of
//      more than 2 s between two steps would be "jumped");
//   2. seen again: it goes on, to its end, the lines the same as a whole run's;
//   3. hidden again, then the tab discarded (as Chrome does: no pagehide, then the page loaded
//      again with document.wasDiscarded): the page starts from that state, and the program
//      prints the rest of its lines from where it stood.
// And with D3D 11 4x targets (host/tests/snapmsaa.c): the GPU read back in a hidden tab, the
// samples the same after the discard.
//
//   node tests/hidden-browser.mjs [path to masu]   (HEADFUL=1: a window, under xvfb-run;
//   ONLY="m emulated": that one; TAB=1: also in a real tab of the full Chromium, where its GPU
//   process keeps its device: passed on 8ee46ef, see `tab` below)
//   (web/dist built with sake in it: crates/sake/build.sh, then pnpm build; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { cpSync, mkdtempSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/masu");
const web = resolve(process.env.WEB ?? "web/dist");
const work = mkdtempSync(join(tmpdir(), "masu-hidden-"));
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "temp"), { recursive: true });
writeFileSync(join(prefix, "win.ini"), "");
run("pack", join(work, "prefix"), "prefix32");
for (const [tree, src, libs] of [
  ["g", "host/tests/snaptest.c", []],
  ["m", "host/tests/snapmsaa.c", ["-ld3d11", "-ld3dcompiler_47"]],
]) {
  const dir = join(work, tree);
  mkdirSync(dir);
  const exe = src.replace(/^.*\/(.*)\.c$/, "$1.exe");
  execFileSync("i686-w64-mingw32-gcc", ["-O2", src, "-o", join(dir, exe), ...libs]);
  run("pack", dir, tree);
  run("add-game", tree, "--title", `隠れたタブの試験 ${tree}`, "--tree", tree, "--exe", exe, "--no-meta");
}

const port = 9300 + Math.floor(Math.random() * 200) * 2;
const base = `http://127.0.0.1:${port}/`;
const server = spawn(bin, ["serve", "--foreground", "--web", web, "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"], { stdio: "inherit" });
for (let i = 0; ; i++) {
  if (server.exitCode !== null) throw new Error(`server exited: ${server.exitCode}`);
  if (await fetch(`${base}apps/builds`).then((r) => r.ok, () => false)) break;
  // (a debug masu checks its RTP downloads first: a while)
  if (i > 600) throw new Error("server did not become ready");
  await new Promise((r) => setTimeout(r, 300));
}

const program = (lines) => lines.filter((l) => /^(step \d+|late thread|clocks|sleeper|changed at|samples differ|end)/.test(l));
async function until(lines, re, ms = 120000) {
  const t = Date.now();
  while (!lines.some((l) => re.test(l))) {
    if (Date.now() - t > ms) throw new Error(`no line matching ${re}; last: ${lines.slice(-8).join(" | ")}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}
// (Chromium on SwiftShader here: getCompilationInfo of a shader module may reject at the start,
// "Instance dropped", with or without this change and the tab in front: not this test's; the
// worker's error then comes again as a page error event right after it)
const bad = (lines) => {
  const skip = new Set();
  lines.forEach((l, i) => {
    if (!/Instance dropped error in getCompilationInfo/.test(l)) return;
    skip.add(i);
    for (let k = i + 1; k < Math.min(lines.length, i + 4); k++) if (/^pageerror: ErrorEvent$/.test(lines[k])) skip.add(k);
  });
  return lines.filter((l, k) => !skip.has(k) && /pageerror|panicked|unreachable/.test(l));
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
// `tab`: the full Chromium of its own, driven over CDP (a page Playwright attaches to stays
// "visible" behind another tab), its tab really behind another one; its WebGPU here (SwiftShader)
// drops its device on Direct3D 11's read back, also with the tab in front, so only GDI there.
// `emulated`: Playwright's headless shell, where no tab is ever hidden: the page told it is
// (visibilityState, visibilitychange) and every worker's animation frames held (the drawing
// thread's loop, host/vngpu.js, then gets none, as in a hidden tab).
// (Playwright's own flags but the three that keep a page in the background "visible": the GPU
// process then keeps its device here, as in Playwright's runs)
const PW_FLAGS = ["--allow-pre-commit-input", "--disable-back-forward-cache", "--disable-background-networking", "--disable-breakpad", "--disable-client-side-phishing-detection", "--disable-component-extensions-with-background-pages", "--disable-component-update", "--disable-default-apps", "--disable-dev-shm-usage", "--disable-extensions", "--disable-field-trial-config", "--disable-hang-monitor", "--disable-infobars", "--disable-ipc-flooding-protection", "--disable-popup-blocking", "--disable-prompt-on-repost", "--disable-search-engine-choice-screen", "--disable-sync", "--enable-unsafe-swiftshader", "--force-color-profile=srgb", "--hide-scrollbars", "--metrics-recording-only", "--mute-audio", "--no-service-autorun", "--password-store=basic", "--use-mock-keychain"];
async function openBrowser(mode) {
  const cdpPort = 9700 + Math.floor(Math.random() * 200);
  if (mode !== "tab") return emulatedBrowser();
  const proc = spawn(
      chromium.executablePath(),
      [`--remote-debugging-port=${cdpPort}`, `--user-data-dir=${join(work, `profile-${mode}`)}`, "--no-first-run", "--no-default-browser-check", "--no-sandbox", ...(process.env.HEADFUL ? [] : ["--headless=new"]), "--enable-unsafe-webgpu", ...PW_FLAGS, "--window-size=1280,800", "about:blank"],
      { stdio: "ignore" },
    );
  const devtools = `http://127.0.0.1:${cdpPort}/json`;
  let targets;
  for (let i = 0; ; i++) {
    targets = await fetch(devtools).then((r) => r.json(), () => null);
    if (targets?.some((t) => t.type === "page")) break;
    if (i > 100) throw new Error("no browser");
    await sleep(200);
  }
  const tab = targets.find((t) => t.type === "page");
  const ws = new WebSocket(tab.webSocketDebuggerUrl);
  await new Promise((r, j) => ((ws.onopen = r), (ws.onerror = j)));
  let nextId = 0;
  const replies = new Map();
  const lines = [];
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id) return replies.get(d.id)?.(d);
    if (d.method === "Runtime.consoleAPICalled") {
      const l = d.params.args.map((a) => a.value ?? a.description ?? "").join(" ");
      lines.push(l);
      if (process.env.VERBOSE) console.log(`[${mode} ${(performance.now() / 1000).toFixed(2)}] ${l}`);
    } else if (d.method === "Runtime.exceptionThrown") lines.push(`pageerror: ${d.params.exceptionDetails.exception?.description ?? d.params.exceptionDetails.text}`);
  };
  const send = (method, params = {}) =>
    new Promise((resolve, reject) => {
      const id = ++nextId;
      replies.set(id, (d) => (replies.delete(id), d.error ? reject(new Error(`${method}: ${d.error.message}`)) : resolve(d.result)));
      ws.send(JSON.stringify({ id, method, params }));
    });
  const evaluate = async (expression) => (await send("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true })).result.value;
  await send("Runtime.enable");
  await send("Page.enable");
  // a discard, as Chrome's: the page loaded again with document.wasDiscarded, no pagehide before
  await send("Page.addScriptToEvaluateOnNewDocument", { source: DISCARDED });
  const visibility = async (want) => {
    for (let i = 0; i < 100; i++) {
      if ((await evaluate("document.visibilityState")) === want) return;
      await sleep(100);
    }
    throw new Error(`the page is not ${want}`);
  };
  /** Out of sight (no animation frames); the way back. */
  async function hide() {
    const other = await fetch(`${devtools}/new?about:blank`, { method: "PUT" }).then((r) => r.json());
    await visibility("hidden");
    return async () => {
      await fetch(`${devtools}/activate/${tab.id}`);
      await fetch(`${devtools}/close/${other.id}`);
      await visibility("visible");
    };
  }
  return {
    lines,
    hide,
    goto: (url) => send("Page.navigate", { url }),
    reload: () => send("Page.reload"),
    discard: () => evaluate(DISCARD),
    close: () => (ws.close(), proc.kill()),
  };
}

// a discard, as Chrome's: the page loaded again with document.wasDiscarded, no pagehide before
const DISCARDED = `if (sessionStorage.getItem("test.discard") === "1") {
  sessionStorage.removeItem("test.discard");
  Object.defineProperty(Document.prototype, "wasDiscarded", { get: () => true, configurable: true });
}`;
// (the page's pagehide must see nothing: a discard has none; its listeners are the page's own,
// so what they do to the tab's storage is undone)
const DISCARD = `sessionStorage.setItem("test.discard", "1"); Storage.prototype.removeItem = () => {};`;

/** Playwright's headless shell (its flags give WebGPU a device that reads Direct3D 11 back, as
 * in tests/savestate-browser.mjs): the page told it is hidden, every worker's animation frames
 * held. */
async function emulatedBrowser() {
  const ctx = await chromium.launchPersistentContext(join(work, "profile-emulated"), { args: ["--enable-unsafe-webgpu"], viewport: { width: 1280, height: 720 } });
  await ctx.addInitScript(DISCARDED);
  const page = ctx.pages()[0] ?? (await ctx.newPage());
  const lines = [];
  page.on("console", (m) => {
    lines.push(m.text());
    if (process.env.VERBOSE) console.log(`[emulated ${(performance.now() / 1000).toFixed(2)}] ${m.text()}`);
  });
  page.on("pageerror", (e) => lines.push(`pageerror: ${e.message}`));
  // (a worker blocked in Atomics.wait never answers: the guest's threads; the drawing thread's
  // worker does, between frames)
  const inWorkers = (f) => Promise.all(page.workers().map((w) => Promise.race([w.evaluate(f).catch(() => undefined), sleep(300)])));
  const setHidden = (h) =>
    page.evaluate((h) => {
      if (h) {
        Object.defineProperty(document, "visibilityState", { get: () => "hidden", configurable: true });
        Object.defineProperty(document, "hidden", { get: () => true, configurable: true });
      } else {
        delete document.visibilityState;
        delete document.hidden;
      }
      document.dispatchEvent(new Event("visibilitychange"));
    }, h);
  async function hide() {
    await inWorkers(() => {
      if (self.requestAnimationFrame && !self.__rafOrig) {
        self.__rafOrig = self.requestAnimationFrame;
        self.requestAnimationFrame = (cb) => ((self.__rafHeld = cb), 0);
      }
    });
    await setHidden(true);
    // (the drawing thread's loop came to ask for a frame and got none)
    await sleep(300);
    assert.ok((await inWorkers(() => !!self.__rafHeld)).some(Boolean), "no worker's animation frames held");
    return async () => {
      await inWorkers(() => {
        if (!self.__rafOrig) return;
        self.requestAnimationFrame = self.__rafOrig;
        delete self.__rafOrig;
        const cb = self.__rafHeld;
        self.__rafHeld = null;
        if (cb) requestAnimationFrame(cb);
      });
      await setHidden(false);
    };
  }
  return {
    lines,
    hide,
    goto: (url) => page.goto(url),
    reload: () => page.reload(),
    discard: () => page.evaluate(DISCARD),
    close: () => void ctx.close(),
  };
}
const browsers = [];

let failed = false;
try {
  for (const [game, mode, wholeOk] of (process.env.ONLY ? (a) => a.filter(([g, m]) => `${g} ${m}` === process.env.ONLY) : (a) => a)([
    // (a real tab: where the full Chromium's GPU process keeps its device; in the CI and cloud
    // containers it crashes at the start, "Instance dropped", and the drawing stops for good)
    ...(process.env.TAB ? [["g", "tab", (w) => w.includes("clocks: ok")]] : []),
    ["g", "emulated", (w) => w.includes("clocks: ok")],
    ["m", "emulated", (w) => w.includes("samples differ: all") && !w.some((l) => l.startsWith("changed at"))],
  ])) {
    const b = await openBrowser(mode);
    browsers.push(b);
    const { lines, hide, goto } = b;
    // (emulated: &headless, as tests/savestate-browser.mjs: the headless shell's canvas can't be
    // presented, and its WebGPU then drops its device on a read back)
    const play = `${base}play.html?game=${game}${mode === "emulated" ? "&headless" : ""}&autostart`;

    // 1-2: away and back, the whole run
    await goto(play);
    await until(lines, game === "g" ? /^step 10:/ : /^step 15:/);
    let show = await hide();
    await until(lines, /^hidden: state kept/, 60000);
    // (the program's lines come from its threads' worker: those printed before it stood may
    // still be on their way)
    await sleep(1000);
    const at = program(lines).length;
    await sleep(4000);
    assert.equal(program(lines).length, at, `ran while hidden: ${program(lines).slice(at).join(" | ")}`);
    await show();
    await until(lines, /^end$/);
    const whole = program(lines);
    assert.ok(wholeOk(whole), whole.join("\n"));
    assert.deepEqual(bad(lines), []);
    console.log(`ok   ${game} (${mode}): stood still while hidden (state kept), went on when seen`);

    // 3: hidden, discarded, loaded again: from the state
    lines.length = 0;
    await goto(play);
    await until(lines, game === "g" ? /^step 20:/ : /^step 15:/);
    show = await hide();
    await until(lines, /^hidden: state kept/, 60000);
    const before = program(lines);
    await b.discard();
    // (still out of sight: it stands, nothing more of it comes)
    await sleep(500);
    lines.length = 0;
    await b.reload();
    await show();
    await until(lines, /^end$/);
    // (the new page's own: a line of the old one may still come in late, a busy machine)
    const rest = program(lines.slice(lines.indexOf("sake を読み込み中")));
    assert.ok(rest.length > 0 && rest.length < whole.length, `${rest.length} lines of ${whole.length}`);
    assert.deepEqual(rest, whole.slice(whole.length - rest.length), "not the rest of the whole run");
    // (each step the same as the whole run's: a line the page got in two pieces, its threads'
    // output and stderr woven, "step snap: …", is not one)
    const steps = (l) => new Map(l.flatMap((x) => /^step (\d+): ([0-9a-f]{8} [0-9a-f]{8})$/.exec(x)?.slice(1, 3) ?? []).reduce((a, x, i, v) => (i % 2 ? a : [...a, [v[i], v[i + 1]]]), []));
    for (const [k, v] of steps(before)) if (steps(whole).has(k)) assert.equal(v, steps(whole).get(k), `step ${k} before the state`);
    assert.ok(rest[0] === whole[before.length] || before.length + rest.length >= whole.length, `went on from ${rest[0]}, not from where it stood (${before.at(-1)})`);
    assert.deepEqual(bad(lines), []);
    console.log(`ok   ${game} (${mode}): a discarded tab went on from where it was hidden`);
    b.close();
  }
} catch (e) {
  failed = true;
  console.error(`FAIL ${e.stack ?? e}`);
} finally {
  browsers.forEach((b) => b.close());
  server.kill();
  await sleep(500);
  rmSync(work, { recursive: true, force: true });
}
process.exit(failed ? 1 : 0);
