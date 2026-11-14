// The game's files fetched onto the device while it plays (host/fetch-bridge.js ippaiWarm). A
// store chunk the game reads first waits 1-5 s for the bucket (worker2's games from a Mac:
// 俺の姿が､透明に!'s CG mode and mode changes waited that long for each file read first); the
// chunks fetched ahead (what earlier runs of the game read first, in that order, then the trees'
// others; fewer at once while the game waits for a read) are on the device by the time it reads
// them.

import { chunkPath } from "./net";

/** Bytes fetched ahead at most for one run: under the chunk cache's 2 GB (net.ts trimChunks), so
 * what is fetched first does not go again before the game reads it. */
export const WARM_MAX = 1536 * 2 ** 20;
/** Chunks fetched side by side (the bucket answers each after a while, whatever its size). */
export const WARM_WIDTH = 6;
/** Of those, how many go on while the game waits for a read of its own. The bucket takes 1-3 s
 * to answer each request whatever its size, so a game that reads one file after another (金恋:
 * SiglusEngine reads its pictures and sounds one at a time, ~1.2 s each, "waiting for files"
 * ~970 ms/s for minutes) waits nearly all the time: with none then, nothing was ever fetched
 * ahead. On a slow line (warmBusyWidth) none, as before: there the game's read needs the line. */
export const WARM_BUSY_WIDTH = 2;

/** The chunks to fetch ahead: those of `first` the trees have (the chunks earlier runs of the
 * game read, in the order they read them), then the trees' others (`sizes`, hex -> bytes, in the
 * lists' order); each once, up to `max` bytes in all. */
export function warmList(sizes: Map<string, number>, max = WARM_MAX, first: string[] = []): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  let total = 0;
  for (const hex of [...first, ...sizes.keys()]) {
    const n = sizes.get(hex);
    if (n === undefined || seen.has(hex)) continue;
    if (total + n > max) break;
    seen.add(hex);
    total += n;
    out.push(hex);
  }
  return out;
}

/** How many fetches ahead go on while the game waits for a read (WARM_BUSY_WIDTH): none on a
 * line the browser says is slow (under 5 Mbit/s: there the game's own read is slowed by them). */
export function warmBusyWidth(c: { downlink?: number } | undefined): number {
  return c?.downlink !== undefined && c.downlink < 5 ? 0 : WARM_BUSY_WIDTH;
}

interface Connection {
  saveData?: boolean;
  type?: string;
}

/** Whether to fetch ahead on this link: not where the player asked to save data, or on a
 * mobile network (its data is paid for). */
export function warmAllowed(c: Connection | undefined): boolean {
  return !c?.saveData && c?.type !== "cellular";
}

export interface WarmReport {
  done: number;
  fetched: number;
  bytes: number;
  failed: number;
  end?: boolean;
  /** Where the game's reads found their chunks (host/fetch-bridge.js ippaiReads): [count, ms]. */
  reads?: Record<"device" | "saving" | "coming" | "none", [number, number]> & {
    followed: number;
    hinted?: number;
  };
}

/** How often the reads are reported while the game plays. */
export const READS_MS = 60000;

const unhex = (h: string) => Uint8Array.from(h.match(/../g)!, (x) => parseInt(x, 16));

/** What followed each chunk (host/fetch-bridge.js ippaiNextAdd's pairs): ippai serve's `.next`
 * (`edges`: every branch earlier runs took), then the list's own order (`first`: the way the
 * newest run went, for a list from before there were edges). */
export function nextPairs(edges: Uint8Array, first: string[]): Uint8Array {
  const n = Math.floor(edges.length / 32) * 32;
  const out = new Uint8Array(n + Math.max(0, first.length - 1) * 32);
  out.set(edges.subarray(0, n));
  for (let i = 0; i + 1 < first.length; i++) {
    out.set(unhex(first[i]!), n + i * 32);
    out.set(unhex(first[i + 1]!), n + i * 32 + 16);
  }
  return out;
}

/** The run log's line of where the game's reads found their chunks (WarmReport.reads). */
export function readsLine(r: NonNullable<WarmReport["reads"]>): string {
  const all = r.device[0] + r.saving[0] + r.coming[0] + r.none[0];
  const p = (k: "device" | "saving" | "coming" | "none") =>
    `${k} ${r[k][0]} (${all ? Math.round((100 * r[k][0]) / all) : 0}%, ${r[k][0] ? Math.round(r[k][1] / r[k][0]) : 0} ms)`;
  return `game reads, first of each chunk: ${all}; ${p("device")}, ${p("saving")}, ${p("coming")}, ${p("none")}; fetched ahead as hints ${r.hinted ?? 0}, of them ${r.followed} from what came next in earlier runs`;
}

export type Warm = (
  urls: string[],
  flag: SharedArrayBuffer,
  width: number,
  report: (r: WarmReport) => void,
  busyWidth?: number,
  opt?: { cls?: number; sizes?: number[]; next?: Uint8Array; readsMs?: number },
) => unknown;

/**
 * Starts fetching ahead (`warm`: the module's ippaiWarm) the chunks of `sizes` under `store`,
 * those of `first` first; `busy()` says while the game waits for a read of its own (then only
 * `busyWidth` go on). Returns a stop.
 */
export function startWarm(
  warm: Warm,
  store: string,
  sizes: Map<string, number>,
  busy: () => boolean,
  log: (line: string) => void,
  first: string[] = [],
  busyWidth = WARM_BUSY_WIDTH,
  edges: Uint8Array = new Uint8Array(0),
): () => void {
  const list = warmList(sizes, WARM_MAX, first);
  const flag = new SharedArrayBuffer(8);
  const f = new Int32Array(flag);
  const t = performance.now();
  const timer = setInterval(() => Atomics.store(f, 0, busy() ? 1 : 0), 50);
  const mb = (n: number) => (n / 2 ** 20).toFixed(0);
  warm(
    list.map((h) => new URL(chunkPath(h), store).href),
    flag,
    WARM_WIDTH,
    (r) => {
      if (r.reads) return log(readsLine(r.reads));
      const s = ((performance.now() - t) / 1000).toFixed(0);
      if (r.end || r.done % 128 === 0)
        log(
          `fetched ahead: ${r.done} of ${list.length} chunks looked at, ${r.fetched} fetched (${mb(r.bytes)} MB), ${r.failed} failed, in ${s} s${r.end ? ", done" : ""}`,
        );
      if (r.end) clearInterval(timer);
    },
    busyWidth,
    // (the lengths: each decoded into a buffer of its size, not 16 MiB)
    { sizes: list.map((h) => sizes.get(h) ?? 0), next: nextPairs(edges, first), readsMs: READS_MS },
  );
  return () => {
    Atomics.store(f, 1, 1);
    clearInterval(timer);
  };
}
