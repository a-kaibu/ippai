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
//   &mount=/ippai/game=tree          also mount a store tree (repeatable; Z:\ippai\game\...)
//   &env=NAME=value                 extra environment variable (repeatable, e.g. SAKE_TRACE_FILES=1)
//   &nosave                         no saved games (ippai serve's pre-run: its x86 regions only)
//   &headless                       don't draw to the canvas (tests/play.mjs: headless Chromium
//                                   destroys the device then, and Direct3D games need it)
//   &bench                          bench.html: the program's output as "vnvm-print" events (all its threads')
//   &warm=0                         don't fetch the rest of the game's files onto the device while it plays (warm.ts)

import { chunkCacheMb } from "./chunkcache";
import { keepSaving, loadSaves, type SavesModule } from "./saves";
import { memGate } from "./memgate";
import { startWebGame } from "./html";
import { linuxKeycode } from "./keymap";
import { AutoAdvance, DEFAULT_SETTINGS, parseSettings, type AutoSettings } from "./autoadvance";
import { AudioOut } from "./audio";
import { Video } from "./video";
import { AudioDecode } from "./adec";
import { ImageDecode } from "./idec";
import {
  applyGame,
  coverUrl,
  fetchGames,
  listUrl,
  needsListBelow,
  nextRunUrl,
  type GameEntry,
} from "./launcher";
import { getUi, setStart, setUi } from "./ui/store";
import { renderShell } from "./ui/Shell";
import { showLibrary } from "./ui/Library";
import { FlightRecorder, sendLeftovers } from "./crashlog";
import {
  fetchRetry,
  getChunk,
  chunkCache,
  HttpError,
  treeChunkSizes,
  trimChunks,
  watchRetries,
  withRetries,
} from "./net";
import {
  fileVersion,
  fnv,
  jitBase as jitBaseOf,
  gameChunksName,
  traceName,
  sakeBase,
  sakeBuild,
  type SakeBuild,
  translatorVersion,
  versioned,
} from "./runtime";
import { checkOnline, startApp } from "./pwa";
import { Budget, pool } from "./ahead";
import { startWarm, warmAllowed, warmBusyWidth, type Warm } from "./warm";
import { savedChunks } from "./offline";
import { sakeWasmBytes, wasmModuleArg } from "./wasmsource";
import { deviceChunks } from "./chunkstore";
import { netBudget, setGameBusy } from "./budget";
import {
  builds,
  getState,
  keptApps,
  listStates,
  loadable,
  putState,
  SLOTS,
  snapping,
  stateHeader,
  stateMemory,
  Stirred,
  takeState,
  type SnapModule,
  type StateInfo,
} from "./savestate";
import { toast } from "sonner";
import { lostNotice } from "./gpulost";
import { accountInput } from "./account-settings";
import { keepInstallation, loadInstallation } from "./installation";
import {
  joinRecords,
  legacyNames,
  portablePath,
  renameRecords,
  selectRecords,
  withRegistry,
} from "./file-records";

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
  /** render/mod.rs: the drawing's lost devices (web/src/gpulost.ts) */
  _vnhost_render_lost_state?(): number;
  _vnhost_net_busy?(): number;
  _vnhost_jit_late?(wasm: number, wasmLen: number, idx: number, idxLen: number): number;
  /** host/fetch-bridge.js: the game's chunks fetched onto the device (warm.ts) */
  ippaiWarm?: Warm;
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
  _vnhost_video_chunk(id: number, t: number, n: number, size: number): number;
  _vnhost_video_begin(id: number, n: number, size: number): number;
  _vnhost_video_ready(id: number, buf: number, layout: number, ok: number): void;
  _vnhost_video_fail(id: number): void;
  _free(ptr: number): void;
  _vnhost_adec_doorbell(): number;
  _vnhost_adec_list(out: number, max: number): number;
  _vnhost_adec_chunk(id: number, gen: number, size: number, seq: number): number;
  _vnhost_adec_pcm(id: number, gen: number, pcm: number, frames: number, channels: number): void;
  _vnhost_adec_done(id: number, gen: number, seq: number): void;
  _vnhost_adec_fail(id: number, gen: number): void;
  _vnhost_idec_doorbell(): number;
  _vnhost_idec_list(out: number, max: number): number;
  _vnhost_idec_begin(id: number, w: number, h: number, layout: number): number;
  _vnhost_idec_done(id: number, ok: number): void;
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

/** The saves of `name`; with none there, those of `old` (the name before the rename, Z:\vnvm),
 * their paths moved to the new ones (the next save goes under `name`). */
async function loadSavesOrOld(
  name: string,
  old: string | null,
  notice: (m: string) => void,
): Promise<Uint8Array | null> {
  const saves = await loadSaves(name, notice);
  if (saves?.length || !old) return saves;
  const moved = renameRecords(await loadSaves(old, notice));
  if (moved?.length) log(`以前のセーブを引き継ぎました (${old})`);
  return moved ?? saves;
}

async function boot(audio: Promise<AudioOut>): Promise<void> {
  const run = params.get("run") ?? "C:\\wintest.exe";
  // saved games: per program and mounts; what it writes in its folders and the user folders
  // (&saves: the launcher's, kept by the program it started: BISHOP's startup.exe writes the
  // bsxx.ini its game reads)
  const mounts = params.getAll("mount");
  const saveNameOf = (r: string, m: string[]) =>
    `${r.replace(/[^A-Za-z0-9_.-]/g, "_")}-${fnv(new TextEncoder().encode(m.join(":")))}`;
  const saveName = params.get("saves") ?? saveNameOf(run, mounts);
  // (the saves of before the rename, Z:\vnvm: read when there are none under the new name)
  const legacy = params.has("saves") ? null : legacyNames(run, mounts);
  // (or those made under the launcher the server now starts the engine of, launcher.ts)
  const launcher = params.has("saves") ? null : params.get("launcher");
  const oldSaves = legacy
    ? saveNameOf(legacy.run, legacy.mounts)
    : launcher
      ? saveNameOf(launcher, mounts)
      : null;
  // (the programs this one starts in its place keep them: closeGame)
  params.set("saves", saveName);
  const nosave = params.has("nosave");
  // a web game (NW.js): its own page, no sake
  if (await startWebGame(run, mounts, nosave ? null : saveName, log, oldSaves)) {
    if (!nosave && params.get("game")) stopPlay = trackPlay(params.get("game")!);
    return;
  }
  log("sake を読み込み中");
  const BASE = sakeBase();
  // a save state to go on from (&state=<slot>: the states dialog starts the page again so), in
  // the build it is of
  const slot = params.get("state");
  const stateFile =
    slot !== null && !nosave
      ? await (async () => {
          log("ステートを読み込み中");
          const info = (await listStates(saveName)).find((s) => s.slot === Number(slot));
          if (!info) throw new Error("ステートが見つかりません");
          // (of another build: in the web app of its time, which the server keeps)
          if (!loadable(await builds(), info)) {
            const app = (await keptApps()).get(info.build);
            if (app && !location.href.startsWith(app)) {
              log("このステートを保存したバージョンで開いています");
              location.replace(`${app}play.html${location.search}`);
              return new Promise<never>(() => {});
            }
          }
          return getState(saveName, info);
        })()
      : undefined;
  const stateWasm = stateFile && stateHeader(stateFile).wasm;
  const build = sakeBuild(stateWasm === "sake-grow" ? "grow" : params.get("memory"));
  const WASM = `${build.name}.wasm`;
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
  const jitVersion = translatorVersion(wasmVersion).then((v) => v + build.jit);
  const jitName = jitBase === "0" ? undefined : jitVersion.then((v) => `${jitBase}-${v}`);
  // (the first part before the start; the others once the program runs: lateJit)
  const loaded = jitName ? jitName.then(loadJit) : Promise.resolve(undefined);
  const jit = loaded.then((l) => (l?.parts.length ? { parts: l.parts } : undefined));
  // (the trees' chunk lengths, once their lists are here: the read-ahead's reservations)
  let treesRead!: (sizes: Map<string, number>) => void;
  const chunkSizes = new Promise<Map<string, number>>((r) => (treesRead = r));
  // the store chunks earlier runs of this game read, in order (kura's vfs::fetched): the first
  // fetched into the module's memory ahead (readAhead), the rest onto the device (warm.ts)
  const game = params.get("game");
  const conn = (navigator as { connection?: { saveData?: boolean; type?: string; downlink?: number } })
    .connection;
  const busyWidth = warmBusyWidth(conn);
  const listNames = jitName ? jitName.then((n) => [...(game ? [gameChunksName(game)] : []), n]) : undefined;
  const listed = listNames
    ? listNames.then(chunksList).catch(() => [] as string[])
    : Promise.resolve([] as string[]);
  // (what followed each chunk in earlier runs, every branch: warm.ts nextPairs)
  const edges = listNames
    ? listNames.then(chunksNext).catch(() => new Uint8Array(0))
    : Promise.resolve(new Uint8Array(0));
  const ahead = jitName ? readAhead(listed, jit, chunkSizes, busyWidth) : undefined;
  // versioned too: the game's threads past the pool each start a worker from it, and a URL the
  // browser must revalidate fails while the link is down (the game then hangs)
  const scriptUrl = versioned(`${BASE}${build.name}.js`, await fileVersion(`${BASE}${build.name}.js`));
  const factory = (await import(/* @vite-ignore */ scriptUrl)).default as (m: object) => Promise<SakeModule>;
  const version = await wasmVersion;
  // sake's wasm from the bucket when the browser lacks the server's (wasmsource.ts): a far or
  // slow server held the start for minutes; &wasm=server: always the server's
  const wasmUrl = new URL(versioned(`${BASE}${WASM}`, version), location.href).href;
  const wasmBytes =
    params.get("wasm") === "server"
      ? Promise.resolve({ bytes: null, from: "server" as const })
      : sakeWasmBytes(WASM, wasmUrl, new URL("store/", location.href).href, {
          device: deviceChunks,
          note: (l) => recorder?.note(l),
        });
  recorder.note(
    `build: ${WASM} ${version}, translator ${await jitVersion}, memory ${build.memory ? "4 GB fixed" : "growing from 2.06 GB (4 GB refused)"}`,
  );
  const saveRoots = [...mounts.map((m) => m.slice(0, m.indexOf("="))), "/ippai/prefix/drive_c/users"];
  const { files: installation, registry: legacyRegistry } = await loadInstallation();
  const saved = nosave
    ? null
    : withRegistry(
        selectRecords(
          await loadSavesOrOld(saveName, oldSaves, (m) => (log(m), toast.warning(m))),
          portablePath,
        ),
        legacyRegistry,
      );
  let stateMem: WebAssembly.Memory | undefined;
  if (stateFile) {
    stateMem = await stateMemory(stateFile, build.memory);
    // (a reload starts the game as usual)
    params.delete("state");
    history.replaceState(history.state, "", `?${params}`);
  }
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
  ).catch((e: unknown) => {
    treesRead(new Map());
    throw e;
  });
  const sizes = new Map<string, number>();
  for (const b of Object.values(vnvmFiles)) treeChunkSizes(new TextDecoder().decode(b), sizes);
  treesRead(sizes);
  // Credentials stay out of vnvmEnv, URLs and log metadata. Only the verified original
  // dialog takes them (crates/sake/src/sdrt_login.rs); the other games never read them.
  const vnvmAccount = await accountInput().catch(() => {
    toast.error("DMM アカウントの設定を読み込めませんでした。ゲーム内で入力してください。");
    return null;
  });
  // Set by the guest once it holds secrets: the account input taken, or a persisted CryptoAPI
  // key container opened (host/account-bridge.js). Guest memory may then keep plaintext, so
  // from that moment the run exports no crash logs and no memory states.
  const vnvmSensitive = new SharedArrayBuffer(4);
  let sensitiveRun = false;
  // (memory states hold one process: none once a second process runs)
  let statesOff = false;
  const noStates = () => {
    if (statesOff) return;
    statesOff = true;
    setUi({ states: null });
  };
  const markSensitive = () => {
    if (sensitiveRun) return;
    sensitiveRun = true;
    recorder?.suppressSensitiveRun();
    noStates();
  };
  const sensitiveTimer = setInterval(() => {
    if (Atomics.load(new Int32Array(vnvmSensitive), 0)) {
      markSensitive();
      clearInterval(sensitiveTimer);
    }
  }, 200);
  let activeModule: SakeModule | undefined;
  let rootModule: SakeModule | undefined;
  let rootExited = false;
  let pendingChildren = 0;
  const moduleArg = {
    vnvmAccount,
    vnvmSensitive,
    // SCM/COM local servers are actual guest processes, sharing the kernel and
    // the one-shot account input, but each having its own address space.
    vnvmSpawn: spawn,
    vnvmFiles,
    // (the bucket's budget of requests: the network worker and the page's save take from one, budget.ts)
    ippaiNetBudget: netBudget(),
    // (sake takes the host path: Z:\\x\\y.exe is /x/y.exe)
    arguments: [
      run.replace(/^z:/i, "").replaceAll("\\", "/"),
      ...(params.get("args")?.split(",").filter(Boolean) ?? []),
    ],
    mainScriptUrlOrBlob: scriptUrl,
    // (sake.wasm: the 4 GB the page could reserve, see sakeBuild; or the memory a state filled,
    // the module starting its threads where they stood)
    ...(build.memory || stateMem ? { wasmMemory: stateMem ?? build.memory } : {}),
    locateFile: (path: string) => (path === WASM ? versioned(`${BASE}${path}`, version) : `${BASE}${path}`),
    // (the bytes from the device or the bucket, compiled once for this process and its children)
    ...(await wasmModuleArg(wasmBytes, (l) => recorder?.note(l))),
    vnvmEnv: {
      // "/=": an empty tree at the root, so every path is kura's (no emscripten file system)
      IPPAI_MOUNTS: ["/=", "/ippai/prefix=prefix32", ...params.getAll("mount")].join(":"),
      LANG: "ja_JP.UTF-8",
      // sake's C:\ (the prefix's fonts)
      SAKE_C: "/ippai/prefix/drive_c",
      ...(jitName ? { IPPAI_JIT_URL: new URL(`jit-save/${await jitName}`, location.href).href } : {}),
      // (the chunks list: the game's own, see gameChunksName)
      ...(jitName && game
        ? { IPPAI_CHUNKS_URL: new URL(`jit-save/${gameChunksName(game)}.chunks`, location.href).href }
        : {}),
      // (this run's read trace, beside it: what it read when, what was foreseen; kura vfs/trace.rs)
      ...(jitName && game
        ? { IPPAI_TRACE_URL: new URL(`jit-save/${traceName(game, Date.now())}`, location.href).href }
        : {}),
      // count the threads' CPU time (vnhost_load_busy: the run log's perf lines, game.html);
      // &noload: off
      ...(params.has("noload") ? {} : { IPPAI_LOAD: "1" }),
      // the page decodes sound with WebCodecs (web/src/adec.ts: from its first frame on)
      ...(typeof AudioDecoder !== "undefined" ? { IPPAI_ADEC: "1" } : {}),
      // (headless Chromium destroys a device that draws to a canvas: tests take shots instead)
      ...(params.has("headless") ? { IPPAI_HEADLESS: "1" } : {}),
      ...(params.has("d3dstats") ? { IPPAI_D3DSTATS: "1" } : {}),
      // (a small chunk cache on a device with little memory, chunkcache.ts; &env= overrides it)
      ...((mb) => (mb ? { IPPAI_CHUNK_CACHE_MB: String(mb) } : {}))(
        chunkCacheMb((navigator as Navigator & { deviceMemory?: number }).deviceMemory),
      ),
      // the game starts in its own folder (as from Explorer): many open their data relatively
      ...(/^z:\\/i.test(run)
        ? { IPPAI_CWD: run.slice(2, run.lastIndexOf("\\")).replaceAll("\\", "/") || "/" }
        : {}),
      ...Object.fromEntries(
        params.getAll("env").map((e) => [e.slice(0, e.indexOf("=")), e.slice(e.indexOf("=") + 1)]),
      ),
    },
    // read by kura's file system as "@saves" (host/vnfs.js); a state has its files already
    vnvmSaves: stateMem ? null : joinRecords(saved, installation),
    // the canvas: a thread of the module draws into it (crates/kura/src/render, host/vngpu.js)
    vnvmCanvas: canvas.transferControlToOffscreen(),
    vnvmJit: await jit,
    print: (line: string) => {
      console.log(line);
      // bench.html shows the program's output
      dispatchEvent(new CustomEvent("vnvm-print", { detail: line }));
    },
    // (&bench: and its threads' lines, host/pre.js)
    ...(params.has("bench") ? { vnvmPrint: PRINT_CHANNEL } : {}),
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
      rootExited = true;
      exited = childProcesses.size === 0 && pendingChildren === 0;
    },
    printErr: (line: string) => {
      console.warn(line);
      recorder?.printErr(line);
      if (params.has("bench")) dispatchEvent(new CustomEvent("vnvm-print", { detail: line }));
      // the program started another of the game's programs in its place (sake's k32::exec)
      const exec = /^sake: exec (.+)\t(.*)$/.exec(line);
      if (exec) nextRun = { run: exec[1], args: exec[2] };
      if (line.startsWith("page stats")) statsLine(0, line);
      if (line.startsWith("d3d9 stats")) statsLine(1, line);
    },
  };
  const childProcesses = new Set<SakeModule>();
  function spawn(
    request: { pid: number; exe: string },
    kernel: SharedArrayBuffer,
    sharedFs: SharedArrayBuffer,
  ): void {
    noStates();
    pendingChildren++;
    const surface = document.createElement("canvas");
    surface.className = "process-screen";
    surface.dataset.pid = String(request.pid);
    surface.style.display = "none";
    surface.style.pointerEvents = "none";
    canvas.after(surface);
    let child: SakeModule | undefined;
    let finished = false;
    let settled = false;
    let rect = 0;
    let observer: ResizeObserver | undefined;
    let timer: ReturnType<typeof setInterval> | undefined;
    let stopWatch: (() => void) | undefined;
    const settle = () => {
      if (!settled) {
        settled = true;
        pendingChildren--;
      }
    };
    const finish = () => {
      if (finished) return;
      finished = true;
      clearInterval(timer);
      observer?.disconnect();
      stopWatch?.();
      // onExit/onAbort run after Emscripten has stopped guest calls. Calling
      // _free here would abort again and recursively enter this callback.
      rect = 0;
      surface.remove();
      if (child) childProcesses.delete(child);
      if (activeModule === child && rootModule && !rootExited) {
        activeModule = rootModule;
        (window as unknown as { vnvmSake: SakeModule }).vnvmSake = rootModule;
        void audio.then((out) => out.attach(rootModule!));
      }
      if (rootExited && !childProcesses.size && !pendingChildren) exited = true;
    };
    const failed = () => {
      settle();
      finish();
      toast.error("プログラムを起動できませんでした。");
    };
    // (a second or third 4 GB memory is what a phone refuses: then the module makes its own, or
    // its start fails through the catch below; a throw here used to escape spawn with
    // pendingChildren up for good, and the page never went back to the list)
    let memory: WebAssembly.Memory | undefined;
    if (build.memory)
      try {
        memory = new WebAssembly.Memory({ initial: 65535, maximum: 65535, shared: true });
      } catch (e) {
        recorder?.note(`spawn: no 4 GB memory for the child (${String(e)})`);
      }
    let offscreen: OffscreenCanvas;
    try {
      offscreen = surface.transferControlToOffscreen();
    } catch {
      return failed();
    }
    void factory({
      ...moduleArg,
      arguments: [request.exe],
      vnvmKernel: kernel,
      vnvmFs: sharedFs,
      vnvmCanvas: offscreen,
      wasmMemory: memory,
      vnvmEnv: { ...moduleArg.vnvmEnv, SAKE_PID: String(request.pid) },
      onExit: finish,
      onAbort: () => {
        finish();
        toast.error("起動したプログラムが停止しました。");
      },
      quit: () => {},
    })
      .then((module) => {
        settle();
        child = module;
        if (finished) {
          if (rootExited && !childProcesses.size && !pendingChildren) exited = true;
          return;
        }
        childProcesses.add(module);
        module._vnhost_render_resize(
          Math.floor(canvas.clientWidth * devicePixelRatio),
          Math.floor(canvas.clientHeight * devicePixelRatio),
        );
        module._vnhost_render_start();
        rect = module._malloc(16);
        timer = setInterval(() => {
          const windows = module._vnhost_display_windows?.(rect, 1) ?? 0;
          surface.dataset.windows = String(windows);
          if (!windows) return;
          clearInterval(timer);
          module._free(rect);
          rect = 0;
          surface.style.display = "block";
          activeModule = module;
          (window as unknown as { vnvmSake: SakeModule }).vnvmSake = module;
          const resize = () =>
            module._vnhost_render_resize(
              Math.floor(canvas.clientWidth * devicePixelRatio),
              Math.floor(canvas.clientHeight * devicePixelRatio),
            );
          resize();
          observer = new ResizeObserver(resize);
          observer.observe(canvas);
          module._vnhost_render_start();
          stopWatch = watch(
            module,
            () => activeModule === module,
            () => !finished,
          );
          void audio.then((out) => out.attach(module));
        }, 100);
      })
      .catch(failed);
  }
  log(`${run} を起動中`);
  const sake = await factory(moduleArg);
  rootModule = sake;
  activeModule ??= sake;
  keepInstallation(sake as unknown as SavesModule, installation, () =>
    toast.error("インストール状態をこの端末に保存できませんでした。"),
  );
  if (vnvmAccount) {
    const state = new Int32Array(vnvmAccount, 0, 1);
    const until = Date.now() + 180_000;
    const timer = setInterval(() => {
      const result = Atomics.load(state, 0);
      if (result < 0) {
        toast.error(`ソフト電池の認証に失敗しました（公式ランタイムのエラーコード ${-result - 1}）。`);
        clearInterval(timer);
      } else if (result === 4) {
        toast.warning(
          "ソフト電池のランタイムが確認済みの版と違うため、アカウントを自動入力しませんでした。ゲーム内で入力してください。",
        );
        clearInterval(timer);
        new Uint8Array(vnvmAccount, 4).fill(0);
      } else if (Date.now() >= until || exited) {
        clearInterval(timer);
        new Uint8Array(vnvmAccount, 4).fill(0);
      }
    }, 500);
  }
  if (!nosave && params.get("game")) stopPlay = trackPlay(params.get("game")!);
  recorder.attach(sake);
  if (!nosave) {
    // (the game quits: the upload tried now, even in the pause after a failed one)
    const save = keepSaving(sake as unknown as SavesModule, saveName, saveRoots, saved);
    saveNow = () => save(true);
  }
  // for tests/play.mjs (headless Chromium can't present WebGPU canvases)
  (window as unknown as { vnvmSake: SakeModule }).vnvmSake = activeModule;
  // (tests: &grow=<MiB> takes that much of sake's memory at once while the game runs, so that
  // sake-grow.wasm's memory grows mid-run; tests/bench/suite.mjs "memory-grow")
  const grow = Number(params.get("grow"));
  if (grow > 0) {
    const before = sake.wasmMemory.buffer.byteLength;
    if (!sake._malloc(grow * 2 ** 20)) console.log(`sake memory: ${grow} MiB refused`);
    const mib = (n: number) => (n / 2 ** 20).toFixed(0);
    console.log(`sake memory: ${mib(before)} -> ${mib(sake.wasmMemory.buffer.byteLength)} MiB`);
  }
  ahead?.(sake);
  // (a restored state has the late parts it had in its memory already)
  if (!stateMem) void loaded.then((l) => l && handLate(sake, l.late));
  // (a save to the device going on meanwhile gives way while the game waits: offline.ts)
  setGameBusy(() => !!sake._vnhost_net_busy?.());
  // the rest of the game's files onto the device while it plays: what earlier runs read first,
  // in that order, then the trees' others; fewer at once while it waits for a read (warm.ts)
  if (sake.ippaiWarm && warmAllowed(conn) && params.get("warm") !== "0")
    void Promise.all([chunkSizes, listed, edges]).then(([sizes, first, next]) => {
      const warm = sake.ippaiWarm!;
      const note = (l: string) => (console.log(l), recorder?.note(l));
      note(
        `fetching ahead: ${first.length} chunks earlier runs read first, then the trees' (${sizes.size}); ${busyWidth} at once while the game waits`,
      );
      // (at once: the network worker serves the game's reads first, host/fetch-bridge.js)
      startWarm(
        warm,
        new URL("store/", location.href).href,
        sizes,
        () => !!sake._vnhost_net_busy?.(),
        note,
        first,
        busyWidth,
        next,
      );
    });
  attachInput(() => activeModule ?? sake);
  attachAuto(() => activeModule ?? sake);
  const resize = () => {
    const dpr = window.devicePixelRatio || 1;
    memGate.run(
      () =>
        sake._vnhost_render_resize(
          Math.floor(canvas.clientWidth * dpr),
          Math.floor(canvas.clientHeight * dpr),
        ),
      resize,
    );
  };
  resize();
  new ResizeObserver(resize).observe(canvas);
  sake._vnhost_render_start();
  watch(
    sake,
    () => activeModule === sake,
    () => !rootExited,
  );
  const out = await audio;
  out.attach(sake);
  recorder.audio = () => out.describe();
  if (!nosave && !statesOff)
    attachStates(
      sake as unknown as SnapModule,
      saveName,
      build.name,
      () => statesOff || sensitive(vnvmSensitive),
    );
}

/**
 * Saved region parts downloaded and compiled at once (see loadJit). A/B, headless Chromium, CPU
 * throttled 4x, 8 parts of the KiriKiri bench game's 6.4 MiB module (median of 3): 1 at a time
 * 980 ms, 2 896 ms, 4 886 ms, 8 940-960 ms; the processes' memory grew ~205 MiB at every width
 * (V8 compiles lazily).
 */
const JIT_LOADS = 4;

/** A saved part fetched after the start: its module's bytes and its index (lateJit). */
type LatePart = { wasm: Uint8Array; idx: Uint8Array };

/**
 * The saved regions `name`: the parts the start waits for, compiled while they download (the
 * browser caches each part's code: a run's save adds a part, the others stay the same), and the
 * rest, fetched once those are here. The first part is the regions runs saved first (their
 * start: ippai serve keeps it apart when it merges parts); the others are what later play
 * reached, which a thread takes in when it first misses one of their regions (kura jit.rs
 * vnhost_jit_late; meanwhile it translates as without them). #123: a game whose parts are big
 * started only once all were downloaded and compiled.
 */
async function loadJit(
  name: string,
): Promise<
  { parts: { module: WebAssembly.Module; idx: Uint8Array }[]; late: Promise<LatePart[]> } | undefined
> {
  const ok = (r: Response) => (r.ok ? r : Promise.reject(new HttpError(r.status)));
  try {
    // "<module> <index> <version>" per part
    const list = new TextDecoder()
      .decode(await fetchRetry(`jit/${name}.parts`, {}, { giveUpMs: 60000 }))
      .split("\n")
      .filter(Boolean)
      .map((line) => line.split(" ") as [string, string, string]);
    const [first, rest] = splitParts(list);
    const parts = await pool(
      JIT_LOADS,
      first.map(([wasm, idx, v]) => async () => {
        const [module, bytes] = await Promise.all([
          // (streamed: the browser keeps the compiled code of a ?v= URL; tried again as a whole)
          withRetries(() => WebAssembly.compileStreaming(fetch(`jit/${wasm}?v=${v}`).then(ok))),
          fetchRetry(`jit/${idx}?v=${v}`, {}, { giveUpMs: 60000 }),
        ]);
        return { module, idx: bytes };
      }),
    );
    // (a part that fails is left out: its regions get translated)
    const late = pool(
      2,
      rest.map(
        ([wasm, idx, v]) =>
          () =>
            Promise.all([
              fetchRetry(`jit/${wasm}?v=${v}`, {}, { giveUpMs: 60000 }),
              fetchRetry(`jit/${idx}?v=${v}`, {}, { giveUpMs: 60000 }),
            ]).then(
              ([w, i]) => (WebAssembly.validate(w) ? { wasm: w, idx: i } : null),
              () => null,
            ),
      ),
    ).then((l) => l.filter((x) => x !== null) as LatePart[]);
    return parts.length || rest.length ? { parts, late } : undefined;
  } catch (e) {
    console.log(`no saved regions for ${name}: ${String(e)}`);
    return undefined;
  }
}

/** The parts (`.parts` lines, oldest first) the start waits for, and the rest: the first
 * (&jitall: all of them, as before #123). */
function splitParts<T>(list: T[]): [T[], T[]] {
  const n = params.has("jitall") ? list.length : 1;
  return [list.slice(0, n), list.slice(n)];
}

/** Hands the late parts to the running module (kura's vnhost_jit_late keeps the memory). */
async function handLate(sake: SakeModule, late: Promise<LatePart[]>): Promise<void> {
  const parts = await late;
  for (const p of parts) {
    // (a save state is being taken: the memory stays as it is)
    while (snapping()) await new Promise((r) => setTimeout(r, 50));
    const w = sake._malloc(p.wasm.length) >>> 0;
    const i = sake._malloc(Math.max(p.idx.length, 1)) >>> 0;
    if (!w || !i) return;
    // (a fresh view: the memory may have grown)
    const heap = new Uint8Array(sake.wasmMemory.buffer);
    heap.set(p.wasm, w);
    heap.set(p.idx, i);
    if (sake._vnhost_jit_late?.(w, p.wasm.length, i, p.idx.length) !== 0) return;
  }
  if (parts.length) console.log(`saved regions: ${parts.length} more parts after the start`);
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

/** The states dialog (ui/States.tsx) for the running game `game`. */
function sensitive(flag: SharedArrayBuffer): boolean {
  return Atomics.load(new Int32Array(flag), 0) !== 0;
}

function attachStates(sake: SnapModule, game: string, wasm: SakeBuild["name"], off: () => boolean): void {
  let slots: StateInfo[] = [];
  let busy = "";
  let ids = new Map<string, string>();
  let kept = new Map<string, string>();
  const show = () =>
    setUi({
      states: {
        slots: Array.from({ length: SLOTS }, (_, i) => {
          const s = slots.find((x) => x.slot === i);
          return s
            ? {
                created: s.created,
                thumb: s.thumb,
                size: s.size,
                old: !loadable(ids, s) && !kept.has(s.build),
              }
            : null;
        }),
        busy,
        open: () => void refresh(),
        save: (n) => void save(n),
        load,
      },
    });
  const refresh = async () => {
    ids = await builds();
    kept = await keptApps();
    slots = await listStates(game).catch(() => slots);
    show();
  };
  const save = async (n: number) => {
    if (busy || snapping()) return;
    // (the run came to hold secrets, or a second process, since the menu was drawn)
    if (off()) return void toast.error("この実行ではメモリステートを保存できません。");
    busy = "セーブ中…";
    show();
    try {
      // (once more when a thread ran while it was copied: savestate.ts Stirred)
      const { file, info } = await takeState(sake, wasm).catch((e: unknown) =>
        e instanceof Stirred ? takeState(sake, wasm) : Promise.reject(e),
      );
      const s = { ...info, slot: n };
      const { uploaded } = await putState(game, s, file);
      slots = [...slots.filter((x) => x.slot !== n), s];
      toast(`スロット ${n + 1} にセーブしました`);
      busy = "サーバーに送信中…";
      show();
      await uploaded.catch(() => toast.warning("サーバーには送れませんでした（この端末には保存済みです）"));
    } catch (e) {
      toast.error(`セーブできませんでした: ${e instanceof Error ? e.message : String(e)}`);
    } finally {
      busy = "";
      show();
    }
  };
  const load = (n: number) => {
    // (a new page: the module starts from the state)
    params.set("state", String(n));
    location.search = `?${params}`;
  };
  void refresh();
}

/** The chunks list earlier runs left (`names` in turn: the game's, its program's), as hex; for
 * one with none yet (a game's first run), the one several programs read (fonts and the like). */
async function chunksList(names: string[]): Promise<string[]> {
  const get = (u: string) =>
    fetchRetry(u, {}, { giveUpMs: 60000 }).then(
      (b) => new Uint8Array(b.buffer),
      () => new Uint8Array(0),
    );
  let l = new Uint8Array(0);
  for (const n of [...names, "_shared"]) {
    l = await get(`jit/${n}.chunks`);
    if (l.length >= 16) break;
  }
  const out: string[] = [];
  const seen = new Set<string>();
  for (let i = 0; i + 16 <= l.length; i += 16) {
    const hex = Array.from(l.subarray(i, i + 16), (b) => b.toString(16).padStart(2, "0")).join("");
    if (!seen.has(hex)) {
      seen.add(hex);
      out.push(hex);
    }
  }
  return out;
}

/** What followed each chunk in earlier runs (`names` in turn: the first with any; ippai serve's
 * `<name>.next`, pairs of hashes). */
async function chunksNext(names: string[]): Promise<Uint8Array> {
  for (const n of names) {
    const b = await fetchRetry(`jit/${n}.next`, {}, { giveUpMs: 60000 }).then(
      (b) => new Uint8Array(b.buffer),
      () => new Uint8Array(0),
    );
    if (b.length >= 32) return b;
  }
  return new Uint8Array(0);
}

function readAhead(
  listed: Promise<string[]>,
  after: Promise<unknown>,
  sizes: Promise<Map<string, number>>,
  busyWidth: number,
): (sake: SakeModule) => void {
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
    const list = await listed;
    await after.catch(() => undefined);
    if (!list.length) return;
    const cache = await chunkCache();
    // (the chunks' lengths, from the trees' lists: each fetch reserves its bytes first, so the
    // 4 side by side never pass the budget; a chunk of no listed length is left to the game)
    const length = await sizes.catch(() => new Map<string, number>());
    let next = 0;
    const one = async (i: number) => {
      while (next < list.length && !budget.full()) {
        const hex = list[next++]!;
        const hash = Uint8Array.from(hex.match(/../g)!, (x) => parseInt(x, 16));
        const n = length.get(hex);
        if (!n || !(await budget.reserve(n))) continue;
        // (the game's requests first: while it waits, only `busyWidth` of these go on, as the
        // fetches ahead do, warm.ts WARM_BUSY_WIDTH)
        while (i >= busyWidth && sake?._vnhost_net_busy?.()) await new Promise((r) => setTimeout(r, 25));
        // (kept on the device from an earlier play: no network; a chunk that fails for good is
        // left to the game's own read)
        const c = await getChunk(store, hex, { cache, priority: "low" }).catch(() => null);
        if (!c) {
          budget.release(n);
          continue;
        }
        if (c.data.length !== n) budget.release(n - c.data.length);
        // (a save state is being taken: the memory stays as it is)
        while (snapping()) await new Promise((r) => setTimeout(r, 50));
        if (sake) put(sake, hash, c.data);
        else got.push([hash, c.data]);
      }
    };
    await Promise.all(Array.from({ length: 4 }, (_, i) => one(i)));
  })().catch((e) => console.log(`read-ahead: ${String(e)}`));
  return (w) => {
    sake = w;
    for (const [hash, data] of got.splice(0)) put(w, hash, data);
    budget.start();
  };
}

/** With the module drawing: the page's events for its frames (vnvm-frame), and the exit.
 * Returns what stops its decoders (a child process that ended: play.ts spawn's finish). */
function watch(
  sake: SakeModule,
  active: () => boolean = () => true,
  live: () => boolean = () => true,
): () => void {
  const info = sake._malloc(32) >>> 0;
  let serial = 0;
  const note = (l: string) => (console.log(l), recorder?.note(l));
  // Siglus movies: decoded here for the guest (web/src/video.ts)
  const video = typeof VideoDecoder !== "undefined" ? new Video(sake, note) : null;
  // sound in codecs sake has no decoder of its own for (MP3 through msacm32: web/src/adec.ts)
  const adec = typeof AudioDecoder !== "undefined" ? new AudioDecode(sake, note) : null;
  // JPEG pictures of WIC / GDI+ / D3DX / OLE: the browser's decoder (web/src/idec.ts)
  const idec =
    typeof ImageDecoder !== "undefined" && globalThis.crypto?.subtle ? new ImageDecode(sake, note) : null;
  let videoErrors = 0;
  // the drawing's WebGPU device lost and made again, or not (host/vngpu.js)
  let lost = 0;
  const frame = () => {
    if (exited) return closeGame();
    if (!live()) return;
    // (a state is being taken: the memory stays as it is)
    if (memGate.closed) return void requestAnimationFrame(frame);
    const l = sake._vnhost_render_lost_state?.() ?? 0;
    if (l !== lost) {
      const n = lostNotice(lost, l);
      lost = l;
      if (n) {
        console.log(`webgpu: ${n.text}`);
        recorder?.note(`webgpu: device lost state ${l}`);
        if (n.kind === "error")
          toast.error(n.text, {
            duration: Infinity,
            action: { label: "再読み込み", onClick: () => location.reload() },
          });
        else toast(n.text);
      }
    }
    // (a throw here used to end this frame loop for good: no more frames, the exit unseen)
    try {
      video?.tick();
    } catch (e) {
      if (++videoErrors <= 5) note(`video: ${String(e)}`);
    }
    sake._vnhost_render_info(info);
    const [s, bits, width, height] = Array.from(new Uint32Array(sake.wasmMemory.buffer, info, 4));
    if (active() && s !== serial && width && height) {
      serial = s!;
      // game.html looks at the frames
      dispatchEvent(new CustomEvent("vnvm-frame", { detail: { heap: sake.HEAPU8, bits, width, height } }));
      if (getUi().status) log("");
    }
    requestAnimationFrame(frame);
  };
  requestAnimationFrame(frame);
  return () => {
    video?.stop();
    adec?.stop();
    idec?.stop();
  };
}

/** Where the pointer last was on the canvas (canvas pixels): 自動送り's clicks go there. */
let pointerAt: [number, number] | null = null;
/** 自動送り (web/src/autoadvance.ts) of the running game: stopped when the game ends. */
let auto: AutoAdvance | null = null;

const AUTO_KEY = "ippai.auto";

function attachAuto(current: () => SakeModule): void {
  let settings: AutoSettings = DEFAULT_SETTINGS;
  try {
    settings = parseSettings(JSON.parse(localStorage.getItem(AUTO_KEY) ?? "null"));
  } catch {
    // (no storage here: the default)
  }
  const a = new AutoAdvance({
    push: (kind, x, y, data) => {
      // (the game is gone: closeGame stops this)
      if (exited) return;
      memGate.run(() => void current()._vnhost_render_input(kind, x, y, data));
    },
    closed: () => memGate.closed,
    // (no pointer yet: the middle of the screen, low, where the text usually is)
    at: () => pointerAt ?? [Math.round(canvas.width / 2), Math.round(canvas.height * 0.85)],
  });
  auto = a;
  const show = () =>
    setUi({
      auto: {
        ...settings,
        running: a.running,
        set: (mode, interval) => {
          settings = { mode, interval };
          try {
            localStorage.setItem(AUTO_KEY, JSON.stringify(settings));
          } catch {
            // (kept for this run only)
          }
          if (a.running) a.start(mode, interval);
          show();
        },
        start: () => {
          a.start(settings.mode, settings.interval);
          recorder?.note(`auto: ${settings.mode} every ${settings.interval} ms`);
          show();
        },
        stop: () => {
          a.stop();
          recorder?.note("auto: stopped");
          show();
        },
      },
    });
  // (a page out of sight: nothing held down while the player is elsewhere)
  document.addEventListener("visibilitychange", () => {
    if (document.hidden && a.running) {
      a.stop();
      show();
    }
  });
  show();
}

/** bench.html: the program's threads' stdout lines (host/pre.js), as "vnvm-print" events. */
const PRINT_CHANNEL = "ippai-print";
if (params.has("bench")) {
  new BroadcastChannel(PRINT_CHANNEL).onmessage = (e: MessageEvent<string>) =>
    dispatchEvent(new CustomEvent("vnvm-print", { detail: e.data }));
}

function attachInput(current: () => SakeModule): void {
  // (a key pressed while a state is taken: given to the game after, memgate.ts)
  const push = (kind: number, x: number, y: number, data: number) =>
    memGate.run(() => void current()._vnhost_render_input(kind, x, y, data));
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
    pointerAt = toFrame(e);
  });
  canvas.addEventListener("pointerdown", (e) => {
    pointerAt = toFrame(e);
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
  if (library) {
    addEventListener("popstate", backToList);
    // (a page the browser kept frozen in its back/forward cache, a game maybe: a fresh one)
    addEventListener("pageshow", (e) => e.persisted && location.reload());
  }
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
      // a history entry of its own: the browser's back button comes back to the list (backToList)
      history.pushState({ game: g.id }, "", url);
      applyGame(params, g);
      splash(g);
      fullscreen();
      start();
    });
    return;
  }
  const id = params.get("game");
  if (library && id && needsListBelow(history.state, id)) {
    if (params.has("run")) {
      // the program a game started in its place (closeGame): over the game's entry, the list's
      // still under it
      history.replaceState({ game: id }, "", location.href);
    } else {
      // opened at the game's own address: the list goes under it, for the back button
      const here = location.href;
      history.replaceState(null, "", listUrl(here));
      history.pushState({ game: id }, "", here);
    }
  }
  if (id && !params.has("run")) {
    const g = (await fetchGames()).find((x) => x.id === id);
    if (!g) throw new Error(`ゲーム ${id} が一覧にありません`);
    applyGame(params, g);
    document.title = `${g.title} - ippai`;
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

let closing = false;
function closeGame(): void {
  // (once: every process's frame loop still scheduled calls it, and a second wrapUp and leave
  // went back twice, out of the app)
  if (closing) return;
  closing = true;
  auto?.stop();
  setUi({ auto: null });
  if (nextRun) {
    const { run, args } = nextRun;
    log(`${run} に切り替えます`);
    // (from params, not the address bar: a game picked from the list has only ?game= there,
    // and the next page would start without its mounts)
    const url = nextRunUrl(location.href, params, run, args);
    void saveNow().finally(() => location.replace(url));
    return;
  }
  // (bench.html: it goes on to its next step, or shows its results: not back to the list)
  if (params.has("bench")) {
    dispatchEvent(new CustomEvent("vnvm-exit"));
    stopPlay?.();
    return;
  }
  log("ゲームが終了しました。一覧に戻ります");
  void wrapUp().finally(leave);
}

/** The game's files saved and its log sent (at most 4 s for the log). */
function wrapUp(why?: string): Promise<unknown> {
  stopPlay?.();
  const ended = recorder
    ? Promise.race([recorder.end(why), new Promise((r) => setTimeout(r, 4000))])
    : Promise.resolve();
  return Promise.all([saveNow(), ended]);
}

/** The browser's back (or forward) button: the address is already the list's (or the game's); the
 * game running is saved and the page starts again there. */
let leaving = false;
function backToList(): void {
  if (leaving) return;
  leaving = true;
  log("一覧に戻ります");
  void wrapUp("the player went back to the list (the browser's back button)").finally(() =>
    location.reload(),
  );
}

function leave(): void {
  stopPlay?.();
  const back = () => {
    // the list's entry is under this one: back to it (no second list entry over the first)
    if (library && history.state?.game) {
      leaving = true;
      addEventListener("popstate", () => location.reload(), { once: true });
      history.back();
      return;
    }
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
