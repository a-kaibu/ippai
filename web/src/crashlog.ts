// The flight recorder: every run keeps a detailed log, so that a crash can be diagnosed
// afterwards (crates/vnhost/src/crashlog.rs has the wasm side).
//
// What a report holds: the run (game, exe, build, browser, device), vnhost's events (each guest
// exception with registers, code, stack, frames resolved to module!export+off, the thread's last
// syscalls / API calls and JIT state; processes; panics), every guest thread's state at the time
// of the report, all of the program's stdout/stderr lately, and the page's own log (JS errors, Emscripten
// aborts, worker errors, memory, visibility) with a performance line every 10 s (see Perf) and
// a summary of them.
//
// When it is written:
// - every 10 s, to OPFS (vnvm-logs/session-<id>.txt): if the tab dies (Android kills it when
//   memory runs out) the next page load uploads it as "<id>-unclean"
// - at a crash (a sake fatal error, a panic, a wasm trap, an abort,
//   an uncaught JS error): "<id>-crashN", to OPFS and to the server (PUT /log-save/<name>, kept in
//   vnvm serve's --logs directory)
// - when the page goes to the background (the way out of a hung game on a phone): "<id>-hidden"
// - Ctrl+Shift+L or a three-finger tap: "<id>-manual"
// - when the game ends: "<id>-end"
//
// logs.html lists the server's and this browser's logs.

import { toast as sonner } from "sonner";
const DIR = "vnvm-logs";
/** OPFS files kept (the oldest go). */
const KEEP = 40;

interface Wasm {
  wasmMemory: WebAssembly.Memory;
  HEAPF64?: Float64Array;
  _malloc?(n: number): number;
  _vnhost_crashlog_info?(): number;
  _vnhost_crashlog_snapshot?(): number;
  _vnhost_load_busy?(): number;
  _vnhost_load_slot?(i: number, out: number): number;
  _vnhost_render_info?(out: number): void;
}

/** One 10 s window of the run. */
interface PerfSample {
  /** seconds since the start */
  t: number;
  /** frames the game drew per second (Direct3D presents, or GDI frames: vnvm-frame) */
  fps: number;
  /** Direct3D: batch time on the page (ms per second), GPU passes and texture uploads (MB) per window */
  d3dBatchMs: number;
  passes: number;
  uploadMb: number;
  /** the page's own frames per second (requestAnimationFrame) and its longest gap */
  rafFps: number;
  longestFrame: number;
  /** page frames over 50 ms */
  janks: number;
  /** main thread tasks over 50 ms (PerformanceObserver longtask) and their total */
  longTasks: number;
  longTaskMs: number;
  /** guest CPU, in cores (vnhost_load_busy), and the busiest threads */
  cpu: number;
  threads: number[];
  /** wasm and JS heap, MB */
  wasmMb: number;
  heapMb: number;
}

/** Performance of the running game, sampled every 10 s: drawn fps, the page's frame pacing,
 *  main-thread long tasks, the guest's CPU (whole and per thread) and memory. */
class Perf {
  readonly samples: PerfSample[] = [];
  private t0 = performance.now();
  private last = performance.now();
  private frames = 0;
  private raf = 0;
  private rafLast = 0;
  private longest = 0;
  private janks = 0;
  private longTasks = 0;
  private longTaskMs = 0;
  private busy = -1;
  private slots: number[] = [];
  private slotBuf = 0;
  private infoBuf = 0;
  private d3d = { frames: 0, runMs: 0, passes: 0, uploaded: 0 };

  constructor() {
    addEventListener("vnvm-frame", () => this.frames++);
    const loop = (t: number) => {
      if (this.rafLast) {
        const d = t - this.rafLast;
        this.longest = Math.max(this.longest, d);
        if (d > 50) this.janks++;
      }
      this.rafLast = t;
      this.raf++;
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
    try {
      new PerformanceObserver((l) => {
        for (const e of l.getEntries()) {
          this.longTasks++;
          this.longTaskMs += e.duration;
        }
      }).observe({ type: "longtask", buffered: false });
    } catch {
      // (no longtask entries in this browser)
    }
    // a page in the background has no frames: don't count its gap as a stall
    document.addEventListener("visibilitychange", () => (this.rafLast = 0));
  }

  /** The window since the last call. */
  sample(wasm: Wasm | null): PerfSample {
    const now = performance.now();
    const secs = (now - this.last) / 1000;
    let cpu = 0;
    let threads: number[] = [];
    try {
      const b = wasm?._vnhost_load_busy?.();
      if (b !== undefined) {
        if (this.busy >= 0) cpu = (b - this.busy) / 1000 / secs;
        this.busy = b;
      }
      if (wasm?._vnhost_load_slot && wasm._malloc && wasm.HEAPF64) {
        this.slotBuf ||= wasm._malloc(32) >>> 0;
        const cur: number[] = [];
        for (let i = 0; wasm._vnhost_load_slot(i, this.slotBuf); i++)
          cur.push(wasm.HEAPF64[this.slotBuf >>> 3]!);
        threads = cur
          .map((v, i) => (v - (this.slots[i] ?? 0)) / 1000 / secs)
          .sort((a, b) => b - a)
          .slice(0, 4);
        this.slots = cur;
      }
    } catch {
      // (the runtime is gone: after an abort or exit)
    }
    const pm = (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory;
    // Direct3D games present through crates/vnhost/src/render, not vnvm-frame: its counters
    let cur = this.d3d;
    try {
      if (wasm?._vnhost_render_info && wasm._malloc) {
        this.infoBuf ||= wasm._malloc(32) >>> 0;
        wasm._vnhost_render_info(this.infoBuf);
        const i = new Uint32Array(wasm.wasmMemory.buffer, this.infoBuf, 8);
        cur = { frames: i[4]!, runMs: i[5]!, passes: i[6]!, uploaded: i[7]! * 2 ** 20 };
      }
    } catch {
      // (the runtime is gone)
    }
    const d3dFrames = cur.frames - this.d3d.frames;
    const s: PerfSample = {
      t: (now - this.t0) / 1000,
      fps: (this.frames + d3dFrames) / secs,
      d3dBatchMs: (cur.runMs - this.d3d.runMs) / secs,
      passes: cur.passes - this.d3d.passes,
      uploadMb: (cur.uploaded - this.d3d.uploaded) / 1e6,
      rafFps: this.raf / secs,
      longestFrame: this.longest,
      janks: this.janks,
      longTasks: this.longTasks,
      longTaskMs: this.longTaskMs,
      cpu,
      threads,
      wasmMb: wasm ? wasm.wasmMemory.buffer.byteLength / 2 ** 20 : 0,
      heapMb: pm ? pm.usedJSHeapSize / 2 ** 20 : 0,
    };
    this.d3d = cur;
    this.last = now;
    this.frames = this.raf = this.janks = this.longTasks = this.longTaskMs = this.longest = 0;
    this.samples.push(s);
    if (this.samples.length > 2000) this.samples.splice(0, 500);
    return s;
  }

  static line(s: PerfSample): string {
    const f = (n: number, d = 1) => n.toFixed(d);
    return (
      `perf: game ${f(s.fps)} fps${s.passes ? ` (d3d batches ${f(s.d3dBatchMs, 0)} ms/s, passes ${s.passes}, upload ${f(s.uploadMb)} MB)` : ""}, page ${f(s.rafFps)} fps (longest frame ${f(s.longestFrame, 0)} ms, ` +
      `${s.janks} over 50 ms), long tasks ${s.longTasks} (${f(s.longTaskMs, 0)} ms), ` +
      `guest CPU ${f(s.cpu, 2)} cores${s.threads.length ? ` [${s.threads.map((v) => f(v, 2)).join(" ")}]` : ""}, ` +
      `wasm ${f(s.wasmMb, 0)} MB${s.heapMb ? `, js heap ${f(s.heapMb, 0)} MB` : ""}`
    );
  }

  /** Over the whole run: averages and the worst windows. */
  summary(): string {
    const all = this.samples.filter((s) => s.t > 15); // (past the start)
    if (!all.length) return "(less than 15 s of samples)";
    const avg = (k: (s: PerfSample) => number) => all.reduce((a, s) => a + k(s), 0) / all.length;
    const fpsSorted = all.map((s) => s.fps).sort((a, b) => a - b);
    const worst = [...all].sort((a, b) => a.fps - b.fps).slice(0, 5);
    return [
      `windows: ${all.length} x 10 s`,
      `game fps: avg ${avg((s) => s.fps).toFixed(1)}, min ${fpsSorted[0]!.toFixed(1)}, ` +
        `p10 ${fpsSorted[Math.floor(fpsSorted.length / 10)]!.toFixed(1)}`,
      `page frames over 50 ms: ${all.reduce((a, s) => a + s.janks, 0)}, longest ${Math.max(...all.map((s) => s.longestFrame)).toFixed(0)} ms`,
      `main thread long tasks: ${all.reduce((a, s) => a + s.longTasks, 0)} (${all.reduce((a, s) => a + s.longTaskMs, 0).toFixed(0)} ms)`,
      `guest CPU: avg ${avg((s) => s.cpu).toFixed(2)} cores, max ${Math.max(...all.map((s) => s.cpu)).toFixed(2)}`,
      `texture upload: total ${all.reduce((a, s) => a + s.uploadMb, 0).toFixed(0)} MB, max ${Math.max(...all.map((s) => s.uploadMb)).toFixed(0)} MB / 10 s`,
      `wasm memory: max ${Math.max(...all.map((s) => s.wasmMb)).toFixed(0)} MB`,
      "lowest-fps windows:",
      ...worst.map((s) => `  at ${s.t.toFixed(0)} s: ${Perf.line(s).slice(6)}`),
    ].join("\n");
  }
}

function two(n: number, w = 2): string {
  return String(n).padStart(w, "0");
}

/** hh:mm:ss.mmm in UTC, as vnhost's records. */
function clock(t = Date.now()): string {
  const d = new Date(t);
  return `${two(d.getUTCHours())}:${two(d.getUTCMinutes())}:${two(d.getUTCSeconds())}.${two(d.getUTCMilliseconds(), 3)}`;
}

function fileStamp(t = Date.now()): string {
  const d = new Date(t);
  return `${d.getFullYear()}${two(d.getMonth() + 1)}${two(d.getDate())}-${two(d.getHours())}${two(d.getMinutes())}${two(d.getSeconds())}`;
}

async function dir(): Promise<FileSystemDirectoryHandle | null> {
  try {
    return await (await navigator.storage.getDirectory()).getDirectoryHandle(DIR, { create: true });
  } catch {
    return null;
  }
}

async function writeFile(name: string, text: string): Promise<void> {
  const d = await dir();
  if (!d) return;
  try {
    const w = await (await d.getFileHandle(name, { create: true })).createWritable();
    await w.write(text);
    await w.close();
  } catch (e) {
    console.warn(`crashlog: OPFS ${name}: ${e}`);
  }
}

/** This browser's saved logs, newest first. */
export async function listLocal(): Promise<{ name: string; size: number; time: number }[]> {
  const d = await dir();
  if (!d) return [];
  const out = [];
  for await (const [name, h] of d as unknown as AsyncIterable<[string, FileSystemHandle]>) {
    if (h.kind !== "file") continue;
    const f = await (h as FileSystemFileHandle).getFile();
    out.push({ name, size: f.size, time: f.lastModified });
  }
  return out.sort((a, b) => b.time - a.time);
}

export async function readLocal(name: string): Promise<string> {
  const d = await dir();
  if (!d) throw new Error("OPFS がありません");
  return (await (await d.getFileHandle(name)).getFile()).text();
}

async function removeLocal(name: string): Promise<void> {
  await (await dir())?.removeEntry(name).catch(() => {});
}

/** Sends a log to vnvm serve (its --logs directory); true if it took it. */
export async function upload(name: string, text: string): Promise<boolean> {
  try {
    const r = await fetch(new URL(`log-save/${encodeURIComponent(name)}`, location.href), {
      method: "PUT",
      body: text,
    });
    return r.ok;
  } catch {
    return false;
  }
}

/**
 * Sessions an earlier page left behind (the tab was killed or closed while a game ran): uploads
 * each as "<id>-unclean" and keeps it under that name. Old files beyond KEEP go.
 */
export async function sendLeftovers(): Promise<void> {
  const files = await listLocal();
  // a running page holds its session's lock (Web Locks go when the tab does); without the API,
  // a session file is left over once it stops being rewritten (every 10 s)
  const held = navigator.locks
    ? new Set(((await navigator.locks.query()).held ?? []).map((l) => l.name ?? ""))
    : null;
  for (const f of files) {
    const m = /^session-(.+)\.txt$/.exec(f.name);
    if (!m || (held ? held.has(`vnvm-session-${m[1]}`) : Date.now() - f.time < 30_000)) continue;
    const text = await readLocal(f.name).catch(() => "");
    if (!text) continue;
    const name = `${m[1]}-unclean`;
    const note = "reason: the page went away while the game ran (tab killed, crashed or closed)\n";
    const body = note + text;
    await writeFile(`${name}.txt`, body);
    await removeLocal(f.name);
    if (await upload(name, body)) console.log(`crashlog: sent ${name}`);
  }
  for (const f of (await listLocal()).slice(KEEP)) await removeLocal(f.name);
}

export class FlightRecorder {
  readonly id: string;
  private meta: [string, string][];
  private page: string[] = [];
  private wasm: Wasm | null = null;
  private info: Uint32Array | null = null;
  private crashes = 0;
  private reports = 0;
  private lastHidden = 0;
  private lastHeads = "";
  private dirty = true;
  private ended = false;
  private aborted = false;
  private t0 = Date.now();
  private perf = new Perf();

  constructor(game: string, meta: Record<string, string>) {
    const rand = Math.random().toString(36).slice(2, 6);
    this.id = `${fileStamp()}-${game.replace(/[^A-Za-z0-9_.-]/g, "_").slice(0, 60)}-${rand}`;
    const nav = navigator as Navigator & { deviceMemory?: number };
    this.meta = Object.entries({
      ...meta,
      url: location.href,
      started: new Date(this.t0).toISOString(),
      browser: navigator.userAgent,
      cores: String(navigator.hardwareConcurrency ?? "?"),
      deviceMemory: `${nav.deviceMemory ?? "?"} GB`,
      screen: `${screen.width}x${screen.height} @${devicePixelRatio}`,
    });
    // (released when the tab goes, however it goes: see sendLeftovers)
    void navigator.locks
      ?.request(`vnvm-session-${this.id}`, () => new Promise<void>(() => {}))
      .catch(() => {});
    addEventListener("error", (e) =>
      this.fatal(`JS error: ${e.message} (${e.filename}:${e.lineno}:${e.colno})\n${e.error?.stack ?? ""}`),
    );
    addEventListener("unhandledrejection", (e) => {
      const r = e.reason as { stack?: string } | undefined;
      this.fatal(`unhandled rejection: ${r?.stack ?? String(e.reason)}`);
    });
    document.addEventListener("visibilitychange", () => {
      this.note(`visibility ${document.visibilityState}`);
      // (once a minute at most: switching apps back and forth)
      if (
        document.visibilityState === "hidden" &&
        this.wasm &&
        !this.ended &&
        Date.now() - this.lastHidden > 60_000
      ) {
        this.lastHidden = Date.now();
        void this.save("hidden", "the page went to the background");
      }
    });
    addEventListener("keydown", (e) => {
      if (e.ctrlKey && e.shiftKey && e.code === "KeyL") {
        e.preventDefault();
        void this.save("manual", "saved by hand (Ctrl+Shift+L)").then((n) =>
          toast(`ログを保存しました: ${n}`),
        );
      }
    });
    addEventListener("touchstart", (e) => {
      if (e.touches.length === 3)
        void this.save("manual", "saved by hand (three-finger tap)").then((n) =>
          toast(`ログを保存しました: ${n}`),
        );
    });
    setInterval(() => this.tick(), 500);
    setInterval(() => void this.persist(), 10_000);
    setInterval(() => this.memory(), 30_000);
    setInterval(() => {
      if (this.wasm && !this.ended) this.note(Perf.line(this.perf.sample(this.wasm)));
    }, 10_000);
    this.note("page started");
  }

  /** A line in the page's log. */
  note(line: string): void {
    this.page.push(`${clock()} ${line}`);
    if (this.page.length > 2000) this.page.splice(0, 500);
    this.dirty = true;
  }

  /** Emscripten's stderr on the page's thread (aborts, worker errors land here). */
  printErr(line: string): void {
    if (
      /Aborted\(|RuntimeError|worker sent an error|Uncaught|abort\(|out of memory|Cannot enlarge memory/i.test(
        line,
      )
    )
      this.fatal(line);
  }

  /** Emscripten's onAbort. */
  abort(what: unknown): void {
    this.aborted = true;
    this.fatal(`wasm abort: ${what instanceof Error ? (what.stack ?? what.message) : String(what)}`);
  }

  attach(wasm: Wasm): void {
    this.wasm = wasm;
    const p = wasm._vnhost_crashlog_info?.();
    if (p) {
      const i = new Uint32Array(wasm.wasmMemory.buffer, p >>> 0, 16).slice();
      if (i[0] === 0x4c434e56) this.info = i;
    }
    this.note(this.info ? "crash log attached" : "this build has no crash log (old build)");
    this.memory();
  }

  /** The game ended normally. */
  async end(): Promise<void> {
    if (this.ended) return;
    this.note("the game ended");
    await this.save("end", "the game ended");
    this.ended = true;
    await removeLocal(`session-${this.id}.txt`);
  }

  private fatal(reason: string): void {
    this.note(`FATAL ${reason}`);
    if (this.ended) return;
    void this.save(`crash${++this.crashes}`, reason).then((n) =>
      toast(`エラーが起きました。ログを保存しました: ${n}`),
    );
  }

  private memory(): void {
    const pm = (performance as Performance & { memory?: { usedJSHeapSize: number; jsHeapSizeLimit: number } })
      .memory;
    const wasm = this.wasm ? `${(this.wasm.wasmMemory.buffer.byteLength / 2 ** 20).toFixed(0)} MB` : "-";
    this.note(
      `memory: wasm ${wasm}${pm ? `, js heap ${(pm.usedJSHeapSize / 2 ** 20).toFixed(0)}/${(pm.jsHeapSizeLimit / 2 ** 20).toFixed(0)} MB` : ""}`,
    );
  }

  /** Watches vnhost's crash counter. */
  private tick(): void {
    const i = this.info;
    if (!i || !this.wasm) return;
    const n = Atomics.load(new Int32Array(this.wasm.wasmMemory.buffer, i[11]! >>> 0, 1), 0);
    if (n > this.crashes) {
      this.crashes = n;
      const reason = this.cString(i[12]!, i[13]!);
      // the faulting thread's state now; the report after the program has printed the rest
      const snap = this.snapshot();
      setTimeout(() => {
        void this.save(`crash${n}`, reason, snap).then((name) =>
          toast(`ゲームがクラッシュしました。ログを保存しました: ${name}`),
        );
      }, 1500);
    }
  }

  private cString(addr: number, max: number): string {
    const b = new Uint8Array(this.wasm!.wasmMemory.buffer, addr >>> 0, max).slice();
    const end = b.indexOf(0);
    return new TextDecoder().decode(b.subarray(0, end < 0 ? max : end));
  }

  private ring(buf: number, size: number, headAt: number): string {
    const mem = this.wasm!.wasmMemory.buffer;
    const head = Atomics.load(new Int32Array(mem, headAt >>> 0, 1), 0) >>> 0;
    const u8 = new Uint8Array(mem);
    buf >>>= 0;
    let bytes: Uint8Array;
    if (head <= size) bytes = u8.slice(buf, buf + head);
    else {
      const at = head % size;
      bytes = new Uint8Array(size);
      bytes.set(u8.subarray(buf + at, buf + size));
      bytes.set(u8.subarray(buf, buf + at), size - at);
    }
    let text = new TextDecoder().decode(bytes).replaceAll("\0", "");
    // (the oldest line was partly overwritten)
    if (head > size) text = text.slice(text.indexOf("\n") + 1);
    return text;
  }

  /** Every guest thread's state, from vnhost (not after an abort: the instance is gone). */
  private snapshot(): string {
    const i = this.info;
    if (!i || !this.wasm?._vnhost_crashlog_snapshot || this.aborted) return "";
    try {
      const n = this.wasm._vnhost_crashlog_snapshot() >>> 0;
      if (!n) return "(busy)";
      return new TextDecoder().decode(new Uint8Array(this.wasm.wasmMemory.buffer, i[8]! >>> 0, n).slice());
    } catch (e) {
      return `(snapshot failed: ${e})`;
    }
  }

  /** The whole log as text. */
  report(kind: string, reason: string, snap?: string): string {
    const i = this.info;
    const up = ((Date.now() - this.t0) / 1000).toFixed(1);
    const parts = [
      `=== vnvm log ${this.id} (${kind}) ===`,
      `reason: ${reason}`,
      `state: ${this.ended ? "ended" : "running"}`,
      `written: ${new Date().toISOString()} (${up} s after the start; times below are UTC)`,
      ...this.meta.map(([k, v]) => `${k}: ${v}`),
    ];
    if (snap !== undefined)
      parts.push("", "--- guest threads at the time of the report ---", snap || "(none)");
    if (i && this.wasm) {
      parts.push(
        "",
        "--- vnhost events (guest exceptions, processes, panics) ---",
        this.ring(i[2]!, i[3]!, i[4]!),
      );
      parts.push("", "--- stdout/stderr ---", this.ring(i[5]!, i[6]!, i[7]!));
    }
    parts.push("", "--- performance (every 10 s in the page log as perf:) ---", this.perf.summary());
    parts.push("", "--- page ---", ...this.page, "");
    return parts.join("\n");
  }

  /** Writes a report to OPFS and the server; returns its name. */
  async save(kind: string, reason: string, snap?: string): Promise<string> {
    const name = `${this.id}-${kind}`;
    this.reports++;
    this.note(`report ${name}: ${reason.split("\n")[0]}`);
    const text = this.report(kind, reason, snap ?? this.snapshot());
    await writeFile(`${name}.txt`, text);
    const sent = await upload(name, text);
    this.note(
      sent
        ? `report ${name} sent to the server`
        : `report ${name}: the server did not take it (kept in this browser)`,
    );
    return name;
  }

  /** The session file (what the next page load sends if this one dies). */
  private async persist(): Promise<void> {
    if (this.ended) return;
    const i = this.info;
    const heads =
      i && this.wasm
        ? [4, 7]
            .map((k) => Atomics.load(new Int32Array(this.wasm!.wasmMemory.buffer, i[k]! >>> 0, 1), 0))
            .join()
        : "";
    if (!this.dirty && heads === this.lastHeads) return;
    this.dirty = false;
    this.lastHeads = heads;
    await writeFile(`session-${this.id}.txt`, this.report("session", "the latest state of a running game"));
  }
}

function toast(text: string): void {
  // (shown by the page's <Toaster>: ui/Shell.tsx)
  sonner(text, { duration: 8000 });
}
