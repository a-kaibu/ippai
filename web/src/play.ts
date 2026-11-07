import { trackPlay } from "./library-data";
let stopPlay: (() => void) | undefined;
// Runs sake (crates/sake, the from-scratch Win32 layer, built by crates/sake/build.sh → public/sake/)
// in the page: the prefix (its C:\ fonts) and the games are streamed from the store, windows are
// composed by kura (crates/kura/src/display.rs) and shown with WebGPU. (The page ran Wine too
// until 2026-11-02: a-kaibu/vnvm-wine.)
//
//   /play.html                      the game list (web/src/launcher.ts, ippai add-game)
//   /play.html?game=id              a game from the list (its run, mount, args and env)
//   /play.html?run=C:\path\to.exe   program to start
//   &args=a,b                       its arguments
//   &mount=/vnvm/game=tree          also mount a store tree (repeatable; Z:\vnvm\game\...)
//   &env=NAME=value                 extra environment variable (repeatable, e.g. SAKE_TRACE_FILES=1)
//   &nosave                         no saved games (ippai serve's pre-run: its x86 regions only)
//   &headless                       don't draw to the canvas (tests/play.mjs: headless Chromium
//                                   destroys the device then, and Direct3D games need it)

import { keepSaving, loadSaves, type SavesModule } from "./saves";
import { startWebGame } from "./html";
import { linuxKeycode } from "./keymap";
import { AudioOut } from "./audio";
import { Video } from "./video";
import { applyGame, coverUrl, fetchGames, type GameEntry } from "./launcher";
import { getUi, setDebug, setStart, setUi } from "./ui/store";
import { renderShell } from "./ui/Shell";
import { showLibrary } from "./ui/Library";
import { FlightRecorder, sendLeftovers } from "./crashlog";
import { fetchRetry, getChunk, chunkCache, HttpError, trimChunks, watchRetries, withRetries } from "./net";
import { fileVersion, fnv, jitBase as jitBaseOf, sakeBase, translatorVersion, versioned } from "./runtime";
import { checkOnline, startApp } from "./pwa";
import { Budget, pool } from "./ahead";
import { savedChunks } from "./offline";

const params = new URLSearchParams(location.search);
const canvas = document.querySelector<HTMLCanvasElement>("#screen")!;
// the status line, start button, cover and stats over the screen (React: ui/Shell.tsx)
renderShell();

interface SakeModule {
  HEAPU8: Uint8Array;
  HEAPU32: Uint32Array;
  HEAPF32: Float32Array;
  _malloc(size: number): number;
  _vnhost_display_size(w: number, h: number): void;
  _vnhost_render_start(): void;
  _vnhost_render_resize(w: number, h: number): void;
  _vnhost_render_input(kind: number, x: number, y: number, data: number): number;
  _vnhost_render_info(out: number): void;
  _vnhost_net_busy?(): number;
  _vnhost_display_view(out: number): void;
  _vnhost_display_windows?(out: number, max: number): number;
  _vnhost_display_frame(io: number): number;
  _vnhost_input_push(kind: number, x: number, y: number, data: number): number;
  _vnhost_audio_ring(frames: number): number;
  _vnhost_vfs_changes(): number;
  _vnhost_vfs_put_chunk(hash: number, data: number, len: number): void;
  _vnhost_video_doorbell(): number;
  _vnhost_video_list(out: number, max: number): number;
  _vnhost_video_has(id: number, n: number): number;
  _vnhost_video_begin(id: number, n: number, size: number): number;
  _vnhost_video_ready(id: number, buf: number, layout: number, ok: number): void;
  _vnhost_video_fail(id: number): void;
  _vnhost_crashlog_info?(): number;
  _vnhost_crashlog_snapshot?(): number;
  wasmMemory: WebAssembly.Memory;
}

/** The run's detailed log (web/src/crashlog.ts), from the start of boot(). */
let recorder: FlightRecorder | null = null;

// input record kinds (crates/kura/src/display.rs)
const MOUSE_MOVE = 1,
  MOUSE_DOWN = 2,
  MOUSE_UP = 3,
  WHEEL = 4,
  KEY_DOWN = 5,
  KEY_UP = 6;

function log(line: string): void {
  console.log(line);
  if (line) recorder?.note(`status: ${line}`);
  setUi({ status: line });
}

/** KeyboardEvent.code -> Windows virtual-key code. */
function virtualKey(code: string): number {
  let m;
  if ((m = /^Key([A-Z])$/.exec(code))) return m[1]!.charCodeAt(0);
  if ((m = /^Digit(\d)$/.exec(code))) return 0x30 + Number(m[1]);
  if ((m = /^Numpad(\d)$/.exec(code))) return 0x60 + Number(m[1]);
  if ((m = /^F(\d+)$/.exec(code))) return 0x6f + Number(m[1]);
  const keys: Record<string, number> = {
    Backspace: 0x08,
    Tab: 0x09,
    Enter: 0x0d,
    NumpadEnter: 0x0d,
    ShiftLeft: 0xa0,
    ShiftRight: 0xa1,
    ControlLeft: 0xa2,
    ControlRight: 0xa3,
    AltLeft: 0xa4,
    AltRight: 0xa5,
    Pause: 0x13,
    CapsLock: 0x14,
    Escape: 0x1b,
    Space: 0x20,
    PageUp: 0x21,
    PageDown: 0x22,
    End: 0x23,
    Home: 0x24,
    ArrowLeft: 0x25,
    ArrowUp: 0x26,
    ArrowRight: 0x27,
    ArrowDown: 0x28,
    PrintScreen: 0x2c,
    Insert: 0x2d,
    Delete: 0x2e,
    MetaLeft: 0x5b,
    MetaRight: 0x5c,
    ContextMenu: 0x5d,
    NumpadMultiply: 0x6a,
    NumpadAdd: 0x6b,
    NumpadSubtract: 0x6d,
    NumpadDecimal: 0x6e,
    NumpadDivide: 0x6f,
    NumLock: 0x90,
    ScrollLock: 0x91,
    Semicolon: 0xba,
    Equal: 0xbb,
    Comma: 0xbc,
    Minus: 0xbd,
    Period: 0xbe,
    Slash: 0xbf,
    Backquote: 0xc0,
    BracketLeft: 0xdb,
    Backslash: 0xdc,
    BracketRight: 0xdd,
    Quote: 0xde,
    IntlBackslash: 0xe2,
    IntlRo: 0xc1,
    IntlYen: 0xdc,
    Convert: 0x1c,
    NonConvert: 0x1d,
    KanaMode: 0x15,
  };
  return keys[code] ?? 0;
}

async function boot(audio: Promise<AudioOut>): Promise<void> {
  // a three-finger tap over the game: debug mode on/off (Ctrl+Shift+D on a phone; four fingers
  // save the log, crashlog.ts). On the last finger up: a four-finger tap has three down first.
  let fingers = 0;
  addEventListener("touchstart", (e) => (fingers = Math.max(fingers, e.touches.length)));
  addEventListener("touchend", (e) => {
    if (e.touches.length) return;
    if (fingers === 3) setDebug(!getUi().debug);
    fingers = 0;
  });
  addEventListener("touchcancel", (e) => e.touches.length || (fingers = 0));
  const run = params.get("run") ?? "C:\\wintest.exe";
  // saved games: per program and mounts; what it writes in its folders and the user folders
  // (&saves: the launcher's, kept by the program it started: BISHOP's startup.exe writes the
  // bsxx.ini its game reads)
  const mounts = params.getAll("mount");
  const saveName =
    params.get("saves") ??
    `${run.replace(/[^A-Za-z0-9_.-]/g, "_")}-${fnv(new TextEncoder().encode(mounts.join(":")))}`;
  // (the programs this one starts in its place keep them: closeGame)
  params.set("saves", saveName);
  const nosave = params.has("nosave");
  // a web game (NW.js): its own page, no sake
  if (await startWebGame(run, mounts, nosave ? null : saveName, log)) {
    if (!nosave && params.get("game")) stopPlay = trackPlay(params.get("game")!);
    return;
  }
  log("sake を読み込み中");
  const BASE = sakeBase();
  const WASM = "sake.wasm";
  recorder = new FlightRecorder(params.get("game") ?? run.slice(run.lastIndexOf("\\") + 1), {
    game: params.get("game") ?? "",
    run,
    args: params.get("args") ?? "",
    mounts: params.getAll("mount").join(" "),
    env: params.getAll("env").join(" "),
  });
  // the browser keeps a versioned sake.wasm with its compiled code (see ippai serve)
  const wasmVersion = fileVersion(`${BASE}${WASM}`);
  // x86 regions earlier runs of this program saved (ippai serve /jit), per build of sake.wasm
  // (which has the translator); &jit=0 for none
  const jitBase = params.get("jit") ?? jitBaseOf(run);
  const jitVersion = translatorVersion(wasmVersion);
  const jitName = jitBase === "0" ? undefined : jitVersion.then((v) => `${jitBase}-${v}`);
  const jit = jitName
    ? jitName.then(async (name) => {
        const parts = (await loadJit(name))?.parts ?? [];
        return parts.length ? { parts } : undefined;
      })
    : Promise.resolve(undefined);
  const ahead = jitName ? readAhead(jitName, jit) : undefined;
  // versioned too: the game's threads past the pool each start a worker from it, and a URL the
  // browser must revalidate fails while the link is down (the game then hangs)
  const scriptUrl = versioned(`${BASE}sake.js`, await fileVersion(`${BASE}sake.js`));
  const factory = (await import(/* @vite-ignore */ scriptUrl)).default as (m: object) => Promise<SakeModule>;
  const version = await wasmVersion;
  recorder.note(`build: ${WASM} ${version}, translator ${await jitVersion}`);
  const saveRoots = [...mounts.map((m) => m.slice(0, m.indexOf("="))), "/vnvm/prefix/drive_c/users"];
  const saved = nosave ? null : await loadSaves(saveName);
  // Preload tree manifests before the factory: initialization may read them on the page.
  const vnvmFiles: Record<string, Uint8Array> = {};
  await Promise.all(
    [...new Set(["prefix32", ...mounts.map((m) => m.slice(m.indexOf("=") + 1))])]
      .filter(Boolean)
      .map(async (tree) => {
        const name = `trees/${tree}.tsv`;
        vnvmFiles[name] = await fetchRetry(new URL(`store/${name}`, location.href)).catch((e: unknown) => {
          throw new Error(`mount ${tree}: ${e instanceof Error ? e.message : String(e)}`);
        });
      }),
  );
  const moduleArg = {
    vnvmFiles,
    // (sake takes the host path: Z:\\x\\y.exe is /x/y.exe)
    arguments: [
      run.replace(/^z:/i, "").replaceAll("\\", "/"),
      ...(params.get("args")?.split(",").filter(Boolean) ?? []),
    ],
    mainScriptUrlOrBlob: scriptUrl,
    locateFile: (path: string) => (path === WASM ? versioned(`${BASE}${path}`, version) : `${BASE}${path}`),
    vnvmEnv: {
      // "/=": an empty tree at the root, so every path is kura's (no emscripten file system)
      IPPAI_MOUNTS: ["/=", "/vnvm/prefix=prefix32", ...params.getAll("mount")].join(":"),
      LANG: "ja_JP.UTF-8",
      // sake's C:\ (the prefix's fonts)
      SAKE_C: "/vnvm/prefix/drive_c",
      ...(jitName ? { IPPAI_JIT_URL: new URL(`jit-save/${await jitName}`, location.href).href } : {}),
      // count the threads' CPU time (vnhost_load_busy: the run log's perf lines, game.html);
      // &noload: off
      ...(params.has("noload") ? {} : { IPPAI_LOAD: "1" }),
      // (headless Chromium destroys a device that draws to a canvas: tests take shots instead)
      ...(params.has("headless") ? { IPPAI_HEADLESS: "1" } : {}),
      ...(params.has("d3dstats") ? { IPPAI_D3DSTATS: "1" } : {}),
      // the game starts in its own folder (as from Explorer): many open their data relatively
      ...(/^z:\\/i.test(run)
        ? { IPPAI_CWD: run.slice(2, run.lastIndexOf("\\")).replaceAll("\\", "/") || "/" }
        : {}),
      ...Object.fromEntries(
        params.getAll("env").map((e) => [e.slice(0, e.indexOf("=")), e.slice(e.indexOf("=") + 1)]),
      ),
    },
    // read by kura's file system as "@saves" (host/vnfs.js)
    vnvmSaves: saved,
    // the canvas: a thread of the module draws into it (crates/kura/src/render, host/vngpu.js)
    vnvmCanvas: canvas.transferControlToOffscreen(),
    vnvmJit: await jit,
    print: (line: string) => {
      console.log(line);
      // bench.html shows the program's output
      dispatchEvent(new CustomEvent("vnvm-print", { detail: line }));
    },
    onAbort: (what: unknown) => {
      recorder?.abort(what);
      // a crash: the way back to the list
      setUi({ back: leave });
    },
    // sake's ExitProcess / exit() end the runtime itself (it is the root process): the
    // frame loop then goes back to the list (closeGame)
    onExit: (code: number) => {
      console.log(`exit code ${code}`);
      stopPlay?.();
      exited = true;
    },
    printErr: (line: string) => {
      console.warn(line);
      recorder?.printErr(line);
      // the program started another of the game's programs in its place (sake's k32::exec)
      const exec = /^sake: exec (.+)\t(.*)$/.exec(line);
      if (exec) nextRun = { run: exec[1], args: exec[2] };
      if (line.startsWith("page stats")) statsLine(0, line);
      if (line.startsWith("d3d9 stats")) statsLine(1, line);
    },
  };
  log(`${run} を起動中`);
  const sake = await factory(moduleArg);
  if (!nosave && params.get("game")) stopPlay = trackPlay(params.get("game")!);
  recorder.attach(sake);
  if (!nosave) saveNow = keepSaving(sake as unknown as SavesModule, saveName, saveRoots, saved);
  // for tests/play.mjs (headless Chromium can't present WebGPU canvases)
  (window as unknown as { vnvmSake: SakeModule }).vnvmSake = sake;
  ahead?.(sake);
  attachInput(sake);
  const resize = () => {
    const dpr = window.devicePixelRatio || 1;
    sake._vnhost_render_resize(Math.floor(canvas.clientWidth * dpr), Math.floor(canvas.clientHeight * dpr));
  };
  resize();
  new ResizeObserver(resize).observe(canvas);
  sake._vnhost_render_start();
  watch(sake);
  (await audio).attach(sake);
}

/**
 * Saved region parts downloaded and compiled at once (see loadJit). A/B, headless Chromium, CPU
 * throttled 4x, 8 parts of the KiriKiri bench game's 6.4 MiB module (median of 3): 1 at a time
 * 980 ms, 2 896 ms, 4 886 ms, 8 940-960 ms; the processes' memory grew ~205 MiB at every width
 * (V8 compiles lazily).
 */
const JIT_LOADS = 4;

/**
 * The saved regions `name`, its parts compiled while they download (the browser caches each
 * part's code: a run's save adds a part, the others stay the same), if any.
 */
async function loadJit(
  name: string,
): Promise<{ parts: { module: WebAssembly.Module; idx: Uint8Array }[] } | undefined> {
  const ok = (r: Response) => (r.ok ? r : Promise.reject(new HttpError(r.status)));
  try {
    // "<module> <index> <version>" per part
    const list = new TextDecoder()
      .decode(await fetchRetry(`jit/${name}.parts`, {}, { giveUpMs: 60000 }))
      .split("\n")
      .filter(Boolean);
    const parts = await pool(
      JIT_LOADS,
      list.map((line) => async () => {
        const [wasm, idx, v] = line.split(" ");
        const [module, bytes] = await Promise.all([
          // (streamed: the browser keeps the compiled code of a ?v= URL; tried again as a whole)
          withRetries(() => WebAssembly.compileStreaming(fetch(`jit/${wasm}?v=${v}`).then(ok))),
          fetchRetry(`jit/${idx}?v=${v}`, {}, { giveUpMs: 60000 }),
        ]);
        return { module, idx: bytes };
      }),
    );
    return parts.length ? { parts } : undefined;
  } catch (e) {
    console.log(`no saved regions for ${name}: ${String(e)}`);
    return undefined;
  }
}

/**
 * Fetches the store chunks the last run of `name` read first (saved with its regions), several
 * at a time: the program's threads read the store one synchronous request at a time, which took about
 * 2 s of the start (fonts are read whole). Returns the function that hands them to the module.
 *
 * On a slow connection these compete with what the game waits for, so they come second: they
 * start once the saved regions are here (`after`), go at low priority, and none starts while
 * one of the game's own requests is on its way (vnhost_net_busy).
 */
/**
 * Bytes fetched ahead at most: before the module is there (held here) and in all (the module's
 * chunk cache is 128 MiB, oldest out first: more would push out what was fetched ahead before
 * the game reads it, after taking the memory and the network). The list has up to 8192 chunks
 * of up to 256 KiB: 2 GiB.
 */
const AHEAD_BEFORE_START = 32 << 20;
const AHEAD_TOTAL = 64 << 20;

function readAhead(name: Promise<string>, after: Promise<unknown>): (sake: SakeModule) => void {
  const store = new URL("store/", location.href).href;
  const got: [Uint8Array, Uint8Array][] = [];
  let sake: SakeModule | undefined;
  const budget = new Budget(AHEAD_BEFORE_START, AHEAD_TOTAL);
  let p = 0;
  const put = (w: SakeModule, hash: Uint8Array, data: Uint8Array) => {
    p ||= w._malloc(16) >>> 0;
    const d = w._malloc(Math.max(data.length, 1)) >>> 0;
    // (a fresh view: the memory may have grown, and HEAPU8 is this thread's old one)
    const heap = new Uint8Array(w.wasmMemory.buffer);
    heap.set(hash, p);
    heap.set(data, d);
    w._vnhost_vfs_put_chunk(p, d, data.length);
  };
  (async () => {
    // this program's list; for a program with none yet (a game's first run), those several
    // programs read (fonts and the like)
    const get = (u: string) =>
      fetchRetry(u, {}, { giveUpMs: 60000 }).then(
        (b) => b.buffer,
        () => new ArrayBuffer(0),
      );
    let lists = [await get(`jit/${await name}.chunks`)];
    if (!lists[0]!.byteLength) lists = [await get("jit/_shared.chunks")];
    await after.catch(() => undefined);
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
    const cache = await chunkCache();
    let next = 0;
    const one = async () => {
      while (next * 16 < list.length && (await budget.room())) {
        // (the game's requests first)
        while (sake?._vnhost_net_busy?.()) await new Promise((r) => setTimeout(r, 25));
        const hash = list.subarray(next * 16, ++next * 16);
        const hex = Array.from(hash, (b) => b.toString(16).padStart(2, "0")).join("");
        // (kept on the device from an earlier play: no network; a chunk that fails for good is
        // left to the game's own read)
        const c = await getChunk(store, hex, { cache, priority: "low" }).catch(() => null);
        if (!c) continue;
        budget.add(c.data.length);
        if (sake) put(sake, hash, c.data);
        else got.push([hash, c.data]);
      }
    };
    await Promise.all(Array.from({ length: 4 }, one));
  })().catch((e) => console.log(`read-ahead: ${String(e)}`));
  return (w) => {
    sake = w;
    for (const [hash, data] of got.splice(0)) put(w, hash, data);
    budget.start();
  };
}

/** With the module drawing: the page's events for its frames (vnvm-frame), and the exit. */
function watch(sake: SakeModule): void {
  const info = sake._malloc(32) >>> 0;
  let serial = 0;
  // Siglus movies: decoded here for the guest (web/src/video.ts)
  const video =
    typeof VideoDecoder !== "undefined" ? new Video(sake, (l) => (console.log(l), recorder?.note(l))) : null;
  const frame = () => {
    if (exited) return closeGame();
    video?.tick();
    sake._vnhost_render_info(info);
    const [s, bits, width, height] = Array.from(new Uint32Array(sake.wasmMemory.buffer, info, 4));
    if (s !== serial && width && height) {
      serial = s!;
      // game.html looks at the frames
      dispatchEvent(new CustomEvent("vnvm-frame", { detail: { heap: sake.HEAPU8, bits, width, height } }));
      if (getUi().status) log("");
    }
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
}

function attachInput(sake: SakeModule): void {
  const push = (kind: number, x: number, y: number, data: number) =>
    sake._vnhost_render_input(kind, x, y, data);
  // canvas pixels: the module maps them to the screen
  const toFrame = (e: { clientX: number; clientY: number }): [number, number] => {
    const b = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;
    return [Math.round((e.clientX - b.left) * dpr), Math.round((e.clientY - b.top) * dpr)];
  };
  // DOM button -> 0 left, 1 right, 2 middle
  const button = (b: number) => [0, 2, 1][b] ?? -1;
  canvas.addEventListener("pointermove", (e) => {
    for (const ev of e.getCoalescedEvents?.() ?? [e]) push(MOUSE_MOVE, ...toFrame(ev), 0);
  });
  canvas.addEventListener("pointerdown", (e) => {
    canvas.setPointerCapture(e.pointerId);
    if (button(e.button) >= 0) push(MOUSE_DOWN, ...toFrame(e), button(e.button));
    e.preventDefault();
  });
  canvas.addEventListener("pointerup", (e) => {
    if (button(e.button) >= 0) push(MOUSE_UP, ...toFrame(e), button(e.button));
  });
  canvas.addEventListener("contextmenu", (e) => e.preventDefault());
  canvas.addEventListener(
    "wheel",
    (e) => {
      push(WHEEL, ...toFrame(e), (e.deltaY < 0 ? 120 : -120) >>> 0);
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

// the game list: play.html / index.html only (bench.html and game.html have none)
const library = !!document.querySelector("#ui[data-library]");

const start = () => {
  setStart({ shown: false });
  if (library) renderShell();
  // the AudioContext comes from this click (autoplay rules)
  boot(AudioOut.create()).catch((e) => {
    recorder?.note(`boot failed: ${e instanceof Error ? (e.stack ?? e.message) : e}`);
    log(`エラー: ${e instanceof Error ? e.message : e}`);
    setUi({ back: leave });
  });
};

/** The game's cover behind the loading messages, until the game draws. */
function splash(g: GameEntry): void {
  const url = coverUrl(g);
  if (!url) return;
  setUi({ splash: url });
  addEventListener("vnvm-frame", () => setUi({ splash: null }), { once: true });
}

async function main(): Promise<void> {
  // installable, and opens offline (sw.ts)
  startApp();
  // logs an earlier page left (its tab was killed mid-game) go to the server now
  void sendLeftovers().catch(() => {});
  // a weak link: say that the game waits for the network (the requests are being retried)
  watchRetries((n) => setUi({ net: n > 0 }));
  void trimChunks(savedChunks).catch(() => undefined);
  const problem = checkSupport();
  if (problem) {
    log(problem);
    setStart({ disabled: true });
    return;
  }
  // the list: picking a game starts it from that click
  if (library && !params.has("run") && !params.has("game")) {
    setStart({ shown: false });
    log("");
    const games = await fetchGames();
    void checkOnline();
    showLibrary(games, (g) => {
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
    setStart({ label: `${g.title} を起動` });
    splash(g);
  }
  log("");
  setStart({
    shown: true,
    onClick: () => {
      fullscreen();
      start();
    },
  });
  if (params.has("autostart")) {
    start();
    // (fullscreen needs a user gesture: a phone's first touch)
    addEventListener("pointerdown", (e) => e.pointerType === "touch" && fullscreen(), { once: true });
  }
}

main().catch((e) => {
  log(`エラー: ${e instanceof Error ? e.message : e}`);
  setUi({ back: leave });
});

/** After the game quit: leaves fullscreen and goes back to the start screen (a fresh page). */
let saveNow: () => Promise<void> = async () => {};
/** The program ended the runtime (Module.onExit). */
let exited = false;

/** The program the game started in its own place (a launcher's game), run when it exits. */
let nextRun: { run: string; args: string } | undefined;

function closeGame(): void {
  if (nextRun) {
    const { run, args } = nextRun;
    log(`${run} に切り替えます`);
    const url = new URL(location.href);
    url.searchParams.set("saves", params.get("saves")!);
    url.searchParams.set("run", run);
    url.searchParams.set("args", args.split(/\s+/).filter(Boolean).join(","));
    if (!args.trim()) url.searchParams.delete("args");
    void saveNow().finally(() => location.replace(url));
    return;
  }
  log("ゲームが終了しました。一覧に戻ります");
  const ended = recorder
    ? Promise.race([recorder.end(), new Promise((r) => setTimeout(r, 4000))])
    : Promise.resolve();
  void Promise.all([saveNow(), ended]).finally(leave);
}

function leave(): void {
  stopPlay?.();
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
  setUi((s) => {
    const stats = [...s.stats];
    stats[n] = line;
    return { stats: Array.from(stats, (l) => l ?? "") };
  });
}

/** The whole display for the game (landscape on phones); refusals are fine. */
function fullscreen(): void {
  if (document.fullscreenElement) return;
  document.documentElement
    .requestFullscreen?.({ navigationUI: "hide" })
    .then(() => (screen.orientation as unknown as { lock?(o: string): Promise<void> }).lock?.("landscape"))
    .catch(() => {});
}
