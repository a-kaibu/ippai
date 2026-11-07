// Network and memory of the running game, polled once a second: the store's requests (counted
// in crates/kura/src/vfs.rs, vnhost_net_stats: the game's threads read files chunk by chunk
// with synchronous requests), the page's own fetches (read-ahead chunks, saved regions), wasm
// heap, JS heap and fps. The debug overlay (ui/Debug.tsx) shows the last seconds, the run log's
// perf lines (crashlog.ts) the last 10 s.

import { useSyncExternalStore } from "react";

export interface MeterWasm {
  wasmMemory: WebAssembly.Memory;
  HEAPF64?: Float64Array;
  HEAPU32?: Uint32Array;
  _malloc?(n: number): number;
  _vnhost_net_stats?(out: number): void;
  _vnhost_mem_info?(out: number): void;
  _vnhost_render_info?(out: number): void;
  _vnhost_load_busy?(): number;
  _vnhost_gpu_pipelines?(out: number): void;
}

/** Running totals at one moment. */
interface Totals {
  t: number;
  requests: number;
  cached: number;
  failed: number;
  /** bytes over the network (compressed), after decoding */
  wire: number;
  bytes: number;
  /** request time, all threads (ms) */
  reqMs: number;
  /** the game's threads waiting for file data (ms) */
  stallMs: number;
  /** the page's fetches of the store and saved regions */
  pageReqs: number;
  pageWire: number;
  frames: number;
  raf: number;
  busy: number;
  pipeHits: number;
  pipeMade: number;
}

/** What one window (the last n seconds) looked like. */
export interface MeterView {
  secs: number;
  /** network bytes per second (store + page), and the totals since the start */
  wirePerSec: number;
  wireTotal: number;
  bytesTotal: number;
  requests: number;
  cached: number;
  failed: number;
  requestsTotal: number;
  /** requests on their way now */
  inflight: number;
  /** mean time of one request (ms; 0 without requests) */
  reqAvgMs: number;
  /** the game's threads waiting for data: ms per second, the longest single wait (ms) */
  stallPerSec: number;
  stallMaxMs: number;
  reqMaxMs: number;
  /** chunks kept in memory (bytes); chunks a read fetched again after the cache dropped them */
  cacheBytes: number;
  refetched: number;
  /** malloc's high-water mark and what it has handed out (bytes; -1 unknown) */
  heapTop: number;
  heapUsed: number;
  jsHeap: number;
  jsHeapLimit: number;
  gameFps: number;
  pageFps: number;
  /** guest CPU in cores */
  cpu: number;
  /** pipelines: the APIs' cache hits and pipelines created, in this window and in all; the
   * creation time of the recent ones (ms, p50/p95) and the longest; -1 hits: unknown */
  pipeHits: number;
  pipeMade: number;
  pipeTotal: number;
  pipeP50: number;
  pipeP95: number;
  pipeMax: number;
}

const HISTORY = 61;

class Meter {
  private wasm: MeterWasm | null = null;
  private buf = 0;
  private hist: (Totals & { stallMax: number; reqMax: number })[] = [];
  private now = {
    inflight: 0,
    cacheBytes: 0,
    refetched: 0,
    heapTop: -1,
    heapUsed: -1,
    pipeP50: 0,
    pipeP95: 0,
    pipeMax: 0,
  };
  private frames = 0;
  private raf = 0;
  private pageReqs = 0;
  private pageWire = 0;
  private view: MeterView | null = null;
  private listeners = new Set<() => void>();
  private timer = 0;

  constructor() {
    addEventListener("vnvm-frame", () => this.frames++);
    const loop = () => {
      this.raf++;
      requestAnimationFrame(loop);
    };
    requestAnimationFrame(loop);
    // the page's own fetches (the game's synchronous requests are counted by the module)
    try {
      new PerformanceObserver((l) => {
        for (const e of l.getEntries() as PerformanceResourceTiming[]) {
          if (e.initiatorType !== "fetch" || !/\/(store|jit)\//.test(e.name)) continue;
          this.pageReqs++;
          this.pageWire += e.transferSize;
        }
      }).observe({ type: "resource", buffered: true });
    } catch {
      // (no resource timing)
    }
  }

  attach(wasm: MeterWasm): void {
    this.wasm = wasm;
    this.buf = 0;
    if (!this.timer) this.timer = window.setInterval(() => this.poll(), 1000);
    this.poll();
  }

  detach(): void {
    this.wasm = null;
  }

  private poll(): void {
    const w = this.wasm;
    const v = Array.from({ length: 12 }, () => 0);
    let d3dFrames = 0;
    let busy = 0;
    let pipeHits = -1;
    let pipeMade = 0;
    try {
      if (w?._malloc && w.HEAPF64) {
        this.buf ||= w._malloc(128) >>> 0;
        const f64 = () => new Float64Array(w.wasmMemory.buffer, this.buf, 12);
        if (w._vnhost_net_stats) {
          w._vnhost_net_stats(this.buf);
          v.splice(0, 12, ...f64());
        }
        if (w._vnhost_mem_info) {
          w._vnhost_mem_info(this.buf);
          const m = f64();
          this.now.heapTop = m[0]!;
          this.now.heapUsed = m[1]!;
        }
        if (w._vnhost_render_info) {
          w._vnhost_render_info(this.buf);
          d3dFrames = new Uint32Array(w.wasmMemory.buffer, this.buf, 8)[4]!;
        }
        busy = w._vnhost_load_busy?.() ?? 0;
        if (w._vnhost_gpu_pipelines) {
          w._vnhost_gpu_pipelines(this.buf);
          const p = f64();
          [pipeHits, pipeMade] = [p[0]!, p[1]!];
          [this.now.pipeMax, this.now.pipeP50, this.now.pipeP95] = [p[3]!, p[4]!, p[5]!];
        }
      }
    } catch {
      // (the runtime is gone: after an abort or exit)
    }
    this.now.inflight = v[6]!;
    this.now.cacheBytes = v[10]!;
    this.now.refetched = v[11]!;
    this.hist.push({
      t: performance.now(),
      requests: v[0]!,
      cached: v[1]!,
      failed: v[2]!,
      wire: v[3]!,
      bytes: v[4]!,
      reqMs: v[5]!,
      stallMs: v[7]!,
      stallMax: v[8]!,
      reqMax: v[9]!,
      pageReqs: this.pageReqs,
      pageWire: this.pageWire,
      frames: this.frames + d3dFrames,
      raf: this.raf,
      busy,
      pipeHits,
      pipeMade,
    });
    if (this.hist.length > HISTORY) this.hist.shift();
    this.view = this.window(2);
    for (const l of this.listeners) l();
  }

  /** The last `secs` seconds (as many as there are). */
  window(secs: number): MeterView | null {
    const h = this.hist;
    if (h.length < 2) return null;
    const b = h[h.length - 1]!;
    const a = h[Math.max(0, h.length - 1 - secs)]!;
    const s = Math.max((b.t - a.t) / 1000, 0.001);
    const span = h.slice(Math.max(1, h.length - secs));
    const reqs = b.requests - a.requests;
    const pm = (performance as Performance & { memory?: { usedJSHeapSize: number; jsHeapSizeLimit: number } })
      .memory;
    return {
      secs: s,
      wirePerSec: (b.wire - a.wire + b.pageWire - a.pageWire) / s,
      wireTotal: b.wire + b.pageWire,
      bytesTotal: b.bytes,
      requests: reqs + b.pageReqs - a.pageReqs,
      cached: b.cached - a.cached,
      failed: b.failed - a.failed,
      requestsTotal: b.requests + b.pageReqs,
      inflight: this.now.inflight,
      reqAvgMs: reqs ? (b.reqMs - a.reqMs) / reqs : 0,
      stallPerSec: (b.stallMs - a.stallMs) / s,
      stallMaxMs: Math.max(0, ...span.map((x) => x.stallMax)),
      reqMaxMs: Math.max(0, ...span.map((x) => x.reqMax)),
      cacheBytes: this.now.cacheBytes,
      refetched: this.now.refetched,
      heapTop: this.now.heapTop,
      heapUsed: this.now.heapUsed,
      jsHeap: pm?.usedJSHeapSize ?? -1,
      jsHeapLimit: pm?.jsHeapSizeLimit ?? -1,
      gameFps: (b.frames - a.frames) / s,
      pageFps: (b.raf - a.raf) / s,
      cpu: (b.busy - a.busy) / 1000 / s,
      pipeHits: b.pipeHits < 0 ? -1 : b.pipeHits - a.pipeHits,
      pipeMade: b.pipeMade - a.pipeMade,
      pipeTotal: b.pipeMade,
      pipeP50: this.now.pipeP50,
      pipeP95: this.now.pipeP95,
      pipeMax: this.now.pipeMax,
    };
  }

  /** Network bytes per second, one per second of the last minute (oldest first). */
  series(): number[] {
    const h = this.hist;
    return h.slice(1).map((b, i) => {
      const a = h[i]!;
      return (b.wire - a.wire + b.pageWire - a.pageWire) / Math.max((b.t - a.t) / 1000, 0.001);
    });
  }

  subscribe = (l: () => void) => {
    this.listeners.add(l);
    return () => void this.listeners.delete(l);
  };

  current = () => this.view;
}

export const meter = new Meter();

export function useMeter(): MeterView | null {
  return useSyncExternalStore(meter.subscribe, meter.current);
}

const mb = (n: number) => (n / 2 ** 20).toFixed(n < 10 * 2 ** 20 ? 2 : 1);

/** The run log's network and memory part of a perf line. */
export function meterLine(v: MeterView | null): string {
  if (!v) return "";
  return (
    `net ${mb(v.wirePerSec)} MB/s (${v.requests} requests, ${v.cached} from cache, ${v.failed} failed, ` +
    `avg ${v.reqAvgMs.toFixed(0)} ms, longest ${v.reqMaxMs.toFixed(0)} ms), total ${mb(v.wireTotal)} MB over the network ` +
    `(${mb(v.bytesTotal)} MB decoded, ${v.requestsTotal} requests), waiting for files ${v.stallPerSec.toFixed(0)} ms/s ` +
    `(longest ${v.stallMaxMs.toFixed(0)} ms), in flight ${v.inflight}, chunk cache ${mb(v.cacheBytes)} MB (${v.refetched} fetched again)` +
    (v.heapTop >= 0 ? `, heap ${mb(v.heapUsed)} MB in use (top ${mb(v.heapTop)} MB)` : "") +
    (v.pipeHits >= 0
      ? `, pipelines ${v.pipeHits} hits ${v.pipeMade} made (${v.pipeTotal} in all; ` +
        `p50 ${v.pipeP50.toFixed(1)} p95 ${v.pipeP95.toFixed(1)} max ${v.pipeMax.toFixed(1)} ms)`
      : "")
  );
}

export { mb };
