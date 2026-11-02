// Runs sake (crates/sake, the from-scratch Win32 layer, built by crates/sake/build.sh → public/sake/)
// in the page: the prefix (its C:\ fonts) and the games are streamed from the store, windows are
// composed by vnhost (crates/vnhost/src/display.rs) and shown with WebGPU. (The Wine path that this
// page also ran until 2026-11-02 is in a-kaibu/vnvm-wine.)
//
//   /wine.html                      the game list (web/src/launcher.ts, vnvm-pack add-game)
//   /wine.html?game=id              a game from the list (its run, mount, args and env)
//   /wine.html?run=C:\path\to.exe   program to start
//   &args=a,b                       its arguments
//   &debug=<flags>[:channel,...]    SAKE_WINEDEBUG (crates/sake/src/k32.rs)
//   &mount=/vnvm/game=tree          also mount a store tree (repeatable; Z:\vnvm\game\...)
//   &env=NAME=value                 extra environment variable (repeatable, e.g. VNVM_TRACE_PROC=1)
//   &nosave                         no saved games (vnvm-server's pre-run: its x86 regions only)
//   &headless                       don't draw to the canvas (tests/wine.mjs: headless Chromium
//                                   destroys the device then, and Direct3D games need it)

import { Presenter } from "./present";
import { keepSaving, loadSaves, type SavesModule } from "./saves";
import { linuxKeycode } from "./keymap";
import { AudioOut } from "./wine-audio";
import { D3D9 } from "./d3d9";
import { applyGame, coverUrl, fetchGames, showLibrary, type GameEntry } from "./launcher";
import { FlightRecorder, sendLeftovers } from "./crashlog";

const params = new URLSearchParams(location.search);
const tz = Intl.DateTimeFormat().resolvedOptions().timeZone || "Asia/Tokyo";
const status = document.querySelector<HTMLDivElement>("#status")!;
const canvas = document.querySelector<HTMLCanvasElement>("#screen")!;
const startButton = document.querySelector<HTMLButtonElement>("#start")!;


interface WineModule {
  FS: {
    mkdirTree(path: string): void;
    mkdir(path: string): void;
    writeFile(path: string, data: Uint8Array | string): void;
  };
  HEAPU8: Uint8Array;
  HEAPU32: Uint32Array;
  HEAPF32: Float32Array;
  _malloc(size: number): number;
  _vnhost_display_size(w: number, h: number): void;
  _vnhost_display_view(out: number): void;
  _vnhost_display_windows?(out: number, max: number): number;
  _vnhost_root_exited(): number;
  _vnhost_display_frame(io: number): number;
  _vnhost_input_push(kind: number, x: number, y: number, data: number): number;
  _vnhost_audio_take(buf: number, frames: number): number;
  _vnhost_vfs_put_chunk(hash: number, data: number, len: number): void;
  _vnhost_d3d9_poll(out: number): number;
  _vnhost_d3d9_readback_done(id: number): void;
  _vnhost_d3d9_readback_begin(id: number): number;
  _vnhost_d3d9_done(): void;
  _vnhost_crashlog_info?(): number;
  _vnhost_crashlog_snapshot?(): number;
  wasmMemory: WebAssembly.Memory;
}

/** The run's detailed log (web/src/crashlog.ts), from the start of boot(). */
let recorder: FlightRecorder | null = null;

// input record kinds (crates/vnhost/src/display.rs)
const MOUSE_MOVE = 1,
  MOUSE_DOWN = 2,
  MOUSE_UP = 3,
  WHEEL = 4,
  KEY_DOWN = 5,
  KEY_UP = 6;

function log(line: string): void {
  console.log(line);
  if (line) recorder?.note(`status: ${line}`);
  status.textContent = line;
}

/** KeyboardEvent.code -> Windows virtual-key code. */
function virtualKey(code: string): number {
  let m;
  if ((m = /^Key([A-Z])$/.exec(code))) return m[1]!.charCodeAt(0);
  if ((m = /^Digit(\d)$/.exec(code))) return 0x30 + Number(m[1]);
  if ((m = /^Numpad(\d)$/.exec(code))) return 0x60 + Number(m[1]);
  if ((m = /^F(\d+)$/.exec(code))) return 0x6f + Number(m[1]);
  const keys: Record<string, number> = {
    Backspace: 0x08, Tab: 0x09, Enter: 0x0d, NumpadEnter: 0x0d, ShiftLeft: 0xa0, ShiftRight: 0xa1,
    ControlLeft: 0xa2, ControlRight: 0xa3, AltLeft: 0xa4, AltRight: 0xa5, Pause: 0x13, CapsLock: 0x14,
    Escape: 0x1b, Space: 0x20, PageUp: 0x21, PageDown: 0x22, End: 0x23, Home: 0x24, ArrowLeft: 0x25,
    ArrowUp: 0x26, ArrowRight: 0x27, ArrowDown: 0x28, PrintScreen: 0x2c, Insert: 0x2d, Delete: 0x2e,
    MetaLeft: 0x5b, MetaRight: 0x5c, ContextMenu: 0x5d, NumpadMultiply: 0x6a, NumpadAdd: 0x6b,
    NumpadSubtract: 0x6d, NumpadDecimal: 0x6e, NumpadDivide: 0x6f, NumLock: 0x90, ScrollLock: 0x91,
    Semicolon: 0xba, Equal: 0xbb, Comma: 0xbc, Minus: 0xbd, Period: 0xbe, Slash: 0xbf, Backquote: 0xc0,
    BracketLeft: 0xdb, Backslash: 0xdc, BracketRight: 0xdd, Quote: 0xde, IntlBackslash: 0xe2,
    IntlRo: 0xc1, IntlYen: 0xdc, Convert: 0x1c, NonConvert: 0x1d, KanaMode: 0x15,
  };
  return keys[code] ?? 0;
}

async function boot(audio: Promise<AudioOut>): Promise<void> {
  const presenter = await Presenter.create(canvas);
  presenter.headless = params.has("headless");
  // Keep the GPU device reachable: Chromium destroys a collected one.
  (window as unknown as { vnvmPresenter: Presenter }).vnvmPresenter = presenter;
  log("sake を読み込み中");
  const WINE = new URL("sake/", location.href).href;
  const WASM = "sake.wasm";
  const run = params.get("run") ?? "C:\\wintest.exe";
  recorder = new FlightRecorder(params.get("game") ?? run.slice(run.lastIndexOf("\\") + 1), {
    game: params.get("game") ?? "",
    run,
    args: params.get("args") ?? "",
    mounts: params.getAll("mount").join(" "),
    env: params.getAll("env").join(" "),
    engine: "sake",
    arch: "x86",
  });
  // the browser keeps a versioned wine.wasm with its compiled code (see vnvm-server)
  const wasmVersion = fetch(`${WINE}${WASM}`, { method: "HEAD" }).then(
    (r) => `${r.headers.get("last-modified") ?? ""}-${r.headers.get("content-length") ?? ""}`,
    () => "",
  );
  // x86 regions earlier runs of this program saved (vnvm-server /jit), per build of wine.wasm
  // (which has the translator); &jit=0 for none
  const jitBase = params.get("jit") ?? `sake-${run.replace(/[^A-Za-z0-9_.-]/g, "_")}`;
  // (jit-version.txt: a hash of the translator's sources, which vnvm-wine hangover/build.sh writes; saved
  // regions stay good across builds that change anything else)
  const jitVersion = fetch(`${WINE}jit-version.txt`, { cache: "no-cache" })
    .then((r) => (r.ok ? r.text() : Promise.reject(new Error(`HTTP ${r.status}`))))
    .then((t) => (/^[0-9a-f]{8}$/.test(t.trim()) ? t.trim() : Promise.reject(new Error("bad version"))))
    .catch(() => wasmVersion.then((v) => fnv(new TextEncoder().encode(v))));
  const jitName = jitBase === "0" ? undefined : jitVersion.then((v) => `${jitBase}-${v}`);
  const jit = jitName
    ? jitName.then(async (name) => {
        const parts = (await loadJit(name))?.parts ?? [];
        return parts.length ? { parts } : undefined;
      })
    : Promise.resolve(undefined);
  const ahead = jitName ? readAhead(jitName) : undefined;
  const scriptUrl = `${WINE}sake.js`;
  const factory = (await import(/* @vite-ignore */ scriptUrl)).default as (m: object) => Promise<WineModule>;
  const version = await wasmVersion;
  recorder.note(`build: ${WASM} ${version}, translator ${await jitVersion}`);
  // saved games: per program and mounts; what it writes in its folders and the user folders
  const mounts = params.getAll("mount");
  const saveName = `${run.replace(/[^A-Za-z0-9_.-]/g, "_")}-${fnv(new TextEncoder().encode(mounts.join(":")))}`;
  const saveRoots = [...mounts.map((m) => m.slice(0, m.indexOf("="))), "/vnvm/prefix/drive_c/users", "/vnvm/prefix/user.reg"];
  const nosave = params.has("nosave");
  const saved = nosave ? null : await loadSaves(saveName).catch(() => null);
  const moduleArg = {
    // (sake takes the host path: Z:\\x\\y.exe is /x/y.exe)
    arguments: [
      run.replace(/^z:/i, "").replaceAll("\\", "/"),
      ...(params.get("args")?.split(",").filter(Boolean) ?? []),
    ],
    mainScriptUrlOrBlob: scriptUrl,
    locateFile: (path: string) =>
      path === WASM && version ? `${WINE}${path}?v=${encodeURIComponent(version)}` : `${WINE}${path}`,
    vnvmEnv: {
      VNVM_MOUNTS: [
        "/vnvm/prefix=prefix32",
        ...params.getAll("mount"),
      ].join(":"),
      LANG: "ja_JP.UTF-8",
      // sake's C:\ (the prefix's fonts)
      SAKE_C: "/vnvm/prefix/drive_c",
      SAKE_WINEDEBUG: params.get("debug") ?? "",
      ...(jitName ? { VNVM_JIT_URL: new URL(`jit-save/${await jitName}`, location.href).href } : {}),
      // game.html: count the threads' waits (vnhost_load_busy)
      ...(params.has("load") ? { VNVM_LOAD: "1" } : {}),
      ...(saved ? { VNVM_SAVES: "/vnvm/saves.bin" } : {}),
      HOME: "/home/web",
      USER: "web",
      // Wine names the zone from TZ (needs a file under TZDIR): by offset alone Tokyo matches Korea first,
      // and Siglus etc. refuse to run outside Japan
      TZ: tz,
      TZDIR: "/vnvm/tz",
      // the game starts in its own folder (as from Explorer): many open their data relatively
      ...(/^z:\\/i.test(run) ? { VNVM_CWD: run.slice(2, run.lastIndexOf("\\")).replaceAll("\\", "/") || "/" } : {}),
      ...Object.fromEntries(params.getAll("env").map((e) => [e.slice(0, e.indexOf("=")), e.slice(e.indexOf("=") + 1)])),
    },
    preRun: [
      (m: WineModule) => {
        m.FS.mkdir("/vnvm");
        m.FS.mkdir("/home/web");
        m.FS.mkdirTree(`/vnvm/tz/${tz.slice(0, tz.lastIndexOf("/") + 1)}`);
        m.FS.writeFile(`/vnvm/tz/${tz}`, "");
        if (saved) m.FS.writeFile("/vnvm/saves.bin", saved);
      },
    ],
    vnvmJit: await jit,
    print: (line: string) => {
      console.log(line);
      // bench.html shows the program's output
      dispatchEvent(new CustomEvent("wine-print", { detail: line }));
    },
    onAbort: (what: unknown) => recorder?.abort(what),
    printErr: (line: string) => {
      console.warn(line);
      recorder?.printErr(line);
      if (line.startsWith("d3d9 stats")) statsLine(1, line);
    },
  };
  log(`${run} を起動中`);
  const wine = await factory(moduleArg);
  recorder.attach(wine);
  if (!nosave) saveNow = keepSaving(wine as unknown as SavesModule, saveName, saveRoots, saved);
  // for tests/wine.mjs (headless Chromium can't present WebGPU canvases)
  (window as unknown as { vnvmWine: WineModule }).vnvmWine = wine;
  ahead?.(wine);
  attachInput(wine, presenter);
  present(wine, presenter);
  (await audio).attach(wine);
}

/** FNV-1a of `bytes`, in hex. */
function fnv(bytes: Uint8Array): string {
  let h = 0x811c9dc5;
  for (const b of bytes) h = Math.imul(h ^ b, 0x01000193);
  return (h >>> 0).toString(16);
}

/**
 * The saved regions `name`, its parts compiled while they download (the browser caches each
 * part's code: a run's save adds a part, the others stay the same), if any.
 */
async function loadJit(name: string): Promise<{ parts: { module: WebAssembly.Module; idx: Uint8Array }[] } | undefined> {
  const ok = (r: Response) => (r.ok ? r : Promise.reject(new Error(`HTTP ${r.status}`)));
  try {
    // "<module> <index> <version>" per part
    const list = (await (await fetch(`jit/${name}.parts`).then(ok)).text()).split("\n").filter(Boolean);
    const parts = await Promise.all(
      list.map(async (line) => {
        const [wasm, idx, v] = line.split(" ");
        const [module, bytes] = await Promise.all([
          WebAssembly.compileStreaming(fetch(`jit/${wasm}?v=${v}`).then(ok)),
          fetch(`jit/${idx}?v=${v}`).then(ok).then((r) => r.arrayBuffer()),
        ]);
        return { module, idx: new Uint8Array(bytes) };
      }),
    );
    return parts.length ? { parts } : undefined;
  } catch (e) {
    console.log(`no saved regions for ${name}: ${e}`);
    return undefined;
  }
}

/**
 * Fetches the store chunks the last run of `name` read first (saved with its regions), several
 * at a time: Wine's threads read the store one synchronous request at a time, which took about
 * 2 s of the start (fonts are read whole). Returns the function that hands them to Wine.
 */
function readAhead(name: Promise<string>): (wine: WineModule) => void {
  const store = new URL("store/", location.href).href;
  const got: [Uint8Array, Uint8Array][] = [];
  let wine: WineModule | undefined;
  let p = 0;
  const put = (w: WineModule, hash: Uint8Array, data: Uint8Array) => {
    p ||= w._malloc(16) >>> 0;
    const d = w._malloc(Math.max(data.length, 1)) >>> 0;
    // (a fresh view: the memory may have grown, and HEAPU8 is this thread's old one)
    const heap = new Uint8Array(w.wasmMemory.buffer);
    heap.set(hash, p);
    heap.set(data, d);
    w._vnhost_vfs_put_chunk(p, d, data.length);
  };
  (async () => {
    // this program's, then those several programs read (Wine's files: a game's first run too)
    const lists = await Promise.all(
      [`jit/${await name}.chunks`, "jit/_shared.chunks"].map((u) =>
        fetch(u).then((r) => (r.ok ? r.arrayBuffer() : new ArrayBuffer(0)), () => new ArrayBuffer(0)),
      ),
    );
    const seen = new Set<string>();
    const all: number[] = [];
    for (const l of lists.map((b) => new Uint8Array(b))) {
      for (let i = 0; i + 16 <= l.length; i += 16) {
        const k = l.subarray(i, i + 16).join(",");
        if (!seen.has(k)) {
          seen.add(k);
          all.push(...l.subarray(i, i + 16));
        }
      }
    }
    const list = new Uint8Array(all);
    if (!list.length) return;
    let next = 0;
    const one = async () => {
      while (next * 16 < list.length) {
        const hash = list.subarray(next * 16, ++next * 16);
        const hex = Array.from(hash, (b) => b.toString(16).padStart(2, "0")).join("");
        const c = await fetch(`${store}chunks/${hex.slice(0, 2)}/${hex}`);
        if (!c.ok) continue;
        const data = new Uint8Array(await c.arrayBuffer());
        if (wine) put(wine, hash, data);
        else got.push([hash, data]);
      }
    };
    await Promise.all(Array.from({ length: 6 }, one));
  })().catch((e) => console.log(`read-ahead: ${e}`));
  return (w) => {
    wine = w;
    for (const [hash, data] of got.splice(0)) put(w, hash, data);
  };
}

/** Shows the screen whenever it changes. */
function present(wine: WineModule, presenter: Presenter): void {
  const scratch = wine._malloc(36) >>> 0;
  const winBuf = wine._malloc(128) >>> 0;
  let aboveKey = "";
  wine._vnhost_display_size(scratch, scratch + 4);
  let width = wine.HEAPU32[scratch >>> 2]!;
  let height = wine.HEAPU32[(scratch + 4) >>> 2]!;
  let shown = -1;
  const io = (scratch + 8) >>> 2;
  // Direct3D 9 games: their commands run here, the presented frame goes over the screen
  const d3d = new D3D9(presenter.device, wine);
  (window as unknown as { vnvmD3d: D3D9 }).vnvmD3d = d3d;
  let d3dFrames = 0;
  const view = (scratch + 20) >>> 2;
  // &d3dstats: every 5 s, page frames, slow ones (>34 ms apart), Direct3D frames and batch time
  const stats = params.has("d3dstats") ? { t0: performance.now(), last: 0, frames: 0, slow: 0, d3d: 0 } : null;
  const frame = () => {
    if (stats) {
      const t = performance.now();
      if (stats.last && t - stats.last > 34) stats.slow++;
      stats.last = t;
      stats.frames++;
      if (t - stats.t0 >= 5000) {
        const s = (t - stats.t0) / 1000;
        const line = `page stats/${s.toFixed(1)}s: rAF ${(stats.frames / s).toFixed(1)}/s, slow ${stats.slow}, d3d frames ${((d3d.frames - stats.d3d) / s).toFixed(1)}/s, batches ${d3d.runMs.toFixed(0)}ms, gpu ${d3d.stat.gpuMs.toFixed(0)}ms (max ${d3d.stat.gpuMax.toFixed(0)}), passes ${d3d.stat.passes}, submits ${d3d.stat.submits}, textures ${d3d.stat.created}, upload ${(d3d.stat.uploaded / 1e6).toFixed(1)}MB`;
        console.log(line);
        statsLine(0, line);
        d3d.stat = { passes: 0, submits: 0, created: 0, uploaded: 0, gpuMs: 0, gpuMax: 0 };
        Object.assign(stats, { t0: t, frames: 0, slow: 0, d3d: d3d.frames });
        d3d.runMs = 0;
      }
    }
    // (a game may change the display mode: the screen's size)
    wine._vnhost_display_size(scratch, scratch + 4);
    if (wine.HEAPU32[scratch >>> 2] !== width || wine.HEAPU32[(scratch + 4) >>> 2] !== height) {
      width = wine.HEAPU32[scratch >>> 2]!;
      height = wine.HEAPU32[(scratch + 4) >>> 2]!;
      shown = -1;
    }
    // only the game window's client area is shown
    wine._vnhost_display_view(view * 4);
    const v = [0, 1, 2, 3].map((i) => wine.HEAPU32[view + i]! | 0) as [number, number, number, number];
    if (!presenter.view || v.some((x, i) => x !== presenter.view![i])) {
      presenter.view = v;
      shown = -1;
    }
    d3d.poll();
    if (d3d.frames !== d3dFrames) {
      d3dFrames = d3d.frames;
      presenter.overlay = d3d.overlay;
      if (shown !== -1) presenter.draw();
    }
    // windows over the Direct3D one (a dialog the game opens) are drawn from the frame on top
    const o = d3d.overlay;
    let above: [number, number, number, number][] = [];
    if (o && wine._vnhost_display_windows) {
      const n = wine._vnhost_display_windows(winBuf, 8);
      const rs = Array.from({ length: n }, (_, k) => [0, 1, 2, 3].map((j) => wine.HEAPU32[(winBuf >>> 2) + 4 * k + j]! | 0) as [number, number, number, number]);
      const [ol, ot, or, ob] = o.rect;
      const i = rs.findIndex(([l, t, r, b]) => l <= ol && t <= ot && r >= or && b >= ob);
      if (i > 0) above = rs.slice(0, i);
    }
    const key = above.join(";");
    if (key !== aboveKey) {
      aboveKey = key;
      presenter.above = above;
      if (shown !== -1) presenter.draw();
    }
    wine.HEAPU32[io] = shown;
    const bits = wine._vnhost_display_frame(scratch + 8) >>> 0;
    const serial = wine.HEAPU32[io]!;
    if (serial !== shown) {
      shown = serial;
      presenter.update(wine.HEAPU8, bits, width, height, width * 4, wine.HEAPU32[io + 1]!, wine.HEAPU32[io + 2]!);
      // game.html looks at the frames
      dispatchEvent(new CustomEvent("wine-frame", { detail: { heap: wine.HEAPU8, bits, width, height } }));
      if (status.textContent) log("");
    }
    // the game quit: so does the page
    if (wine._vnhost_root_exited()) return closeGame();
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
}

function attachInput(wine: WineModule, presenter: Presenter): void {
  const push = (kind: number, x: number, y: number, data: number) => wine._vnhost_input_push(kind, x, y, data);
  // DOM button -> 0 left, 1 right, 2 middle
  const button = (b: number) => [0, 2, 1][b] ?? -1;
  canvas.addEventListener("pointermove", (e) => {
    for (const ev of e.getCoalescedEvents?.() ?? [e]) push(MOUSE_MOVE, ...presenter.toFrame(ev), 0);
  });
  canvas.addEventListener("pointerdown", (e) => {
    canvas.setPointerCapture(e.pointerId);
    if (button(e.button) >= 0) push(MOUSE_DOWN, ...presenter.toFrame(e), button(e.button));
    e.preventDefault();
  });
  canvas.addEventListener("pointerup", (e) => {
    if (button(e.button) >= 0) push(MOUSE_UP, ...presenter.toFrame(e), button(e.button));
  });
  canvas.addEventListener("contextmenu", (e) => e.preventDefault());
  canvas.addEventListener(
    "wheel",
    (e) => {
      push(WHEEL, ...presenter.toFrame(e), (e.deltaY < 0 ? 120 : -120) >>> 0);
      e.preventDefault();
    },
    { passive: false },
  );
  const key = (kind: number) => (e: KeyboardEvent) => {
    const vk = virtualKey(e.code);
    if (!vk) return;
    // PC scan code set 1 matches the Linux keycodes below 89
    const scan = linuxKeycode(e.code);
    push(kind, 0, 0, vk | ((scan < 89 ? scan : 0) << 16));
    e.preventDefault();
  };
  window.addEventListener("keydown", key(KEY_DOWN));
  window.addEventListener("keyup", key(KEY_UP));
}

function checkSupport(): string | null {
  if (!crossOriginIsolated) return "SharedArrayBuffer が使えません (COOP/COEP ヘッダが必要)";
  if (!("gpu" in navigator)) return "WebGPU に対応していないブラウザです";
  return null;
}

// (bench.html and game.html have none)
const library = document.querySelector<HTMLElement>("#library");

const start = () => {
  startButton.hidden = true;
  if (library) library.hidden = true;
  // the AudioContext comes from this click (autoplay rules)
  boot(AudioOut.create()).catch((e) => {
    recorder?.note(`boot failed: ${e instanceof Error ? (e.stack ?? e.message) : e}`);
    log(`エラー: ${e instanceof Error ? e.message : e}`);
  });
};

/** The game's cover behind the loading messages, until Wine draws. */
function splash(g: GameEntry): void {
  const url = coverUrl(g);
  if (!url) return;
  const div = document.createElement("div");
  div.id = "splash";
  div.style.backgroundImage = `url("${url}")`;
  document.body.append(div);
  addEventListener("wine-frame", () => div.remove(), { once: true });
}

async function main(): Promise<void> {
  // logs an earlier page left (its tab was killed mid-game) go to the server now
  void sendLeftovers().catch(() => {});
  const problem = checkSupport();
  if (problem) {
    log(problem);
    startButton.disabled = true;
    return;
  }
  // the list: picking a game starts it from that click
  if (library && !params.has("run") && !params.has("game")) {
    startButton.hidden = true;
    log("");
    const games = await fetchGames();
    showLibrary(library, games, (g) => {
      const url = new URL(location.href);
      url.searchParams.set("game", g.id);
      history.replaceState(null, "", url);
      applyGame(params, g);
      splash(g);
      fullscreen();
      start();
    });
    return;
  }
  const id = params.get("game");
  if (id && !params.has("run")) {
    const g = (await fetchGames()).find((x) => x.id === id);
    if (!g) throw new Error(`ゲーム ${id} が一覧にありません`);
    applyGame(params, g);
    document.title = `${g.title} - vnvm`;
    startButton.textContent = `${g.title} を起動`;
    splash(g);
  }
  log("");
  startButton.hidden = false;
  startButton.addEventListener("click", () => {
    fullscreen();
    start();
  });
  if (params.has("autostart")) {
    start();
    // (fullscreen needs a user gesture: a phone's first touch)
    addEventListener("pointerdown", (e) => e.pointerType === "touch" && fullscreen(), { once: true });
  }
}

main().catch((e) => log(`エラー: ${e instanceof Error ? e.message : e}`));

/** After the game quit: leaves fullscreen and goes back to the start screen (a fresh page). */
let saveNow: () => Promise<void> = async () => {};

function closeGame(): void {
  log("ゲームが終了しました");
  const ended = recorder ? Promise.race([recorder.end(), new Promise((r) => setTimeout(r, 4000))]) : Promise.resolve();
  void Promise.all([saveNow(), ended]).finally(leave);
}

function leave(): void {
  const back = () => {
    const url = new URL(location.href);
    url.searchParams.delete("autostart");
    // a game from the list: back to the list
    url.searchParams.delete("game");
    location.replace(url);
  };
  if (document.fullscreenElement) document.exitFullscreen().then(back, back);
  else back();
}

/** &d3dstats: the latest stats lines over the screen (to read them on a phone) */
function statsLine(n: number, line: string): void {
  let box = document.querySelector<HTMLPreElement>("#stats");
  if (!box) {
    box = document.createElement("pre");
    box.id = "stats";
    box.style.cssText =
      "position:fixed;left:4px;top:4px;margin:0;padding:4px 6px;background:rgb(0 0 0/70%);color:#fff;font:11px/1.3 monospace;white-space:pre-wrap;max-width:calc(100% - 16px);pointer-events:none;z-index:2";
    document.body.append(box);
  }
  const lines = (box.textContent ?? "").split("\n");
  lines[n] = line;
  box.textContent = lines.join("\n");
}

/** The whole display for the game (landscape on phones); refusals are fine. */
function fullscreen(): void {
  if (document.fullscreenElement) return;
  document.documentElement
    .requestFullscreen?.({ navigationUI: "hide" })
    .then(() => (screen.orientation as unknown as { lock?(o: string): Promise<void> }).lock?.("landscape"))
    .catch(() => {});
}
