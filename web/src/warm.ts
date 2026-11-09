// The game's files fetched onto the device while it plays (host/fetch-bridge.js ippaiWarm). A
// store chunk the game reads first waits 1-5 s for the bucket (worker2's games from a Mac:
// 俺の姿が､透明に!'s CG mode and mode changes waited that long for each file read first); the
// chunks fetched ahead, in the trees' order and only while the game waits for nothing, are on
// the device by the time it reads them.

import { chunkPath } from "./net";

/** Bytes fetched ahead at most for one run: under the chunk cache's 2 GB (net.ts trimChunks), so
 * what is fetched first does not go again before the game reads it. */
export const WARM_MAX = 1536 * 2 ** 20;
/** Chunks fetched side by side (the bucket answers each after a while, whatever its size). */
export const WARM_WIDTH = 6;

/** The chunks to fetch ahead: the trees' chunks (`sizes`, hex -> bytes, in the lists' order),
 * the first ones up to `max` bytes in all. */
export function warmList(sizes: Map<string, number>, max = WARM_MAX): string[] {
  const out: string[] = [];
  let total = 0;
  for (const [hex, n] of sizes) {
    if (total + n > max) break;
    total += n;
    out.push(hex);
  }
  return out;
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
}

export type Warm = (urls: string[], flag: SharedArrayBuffer, width: number, report: (r: WarmReport) => void) => Worker;

/**
 * Starts fetching ahead (`warm`: the module's ippaiWarm) the chunks of `sizes` under `store`;
 * `busy()` says while the game waits for a read of its own (none starts then). Returns a stop.
 */
export function startWarm(
  warm: Warm,
  store: string,
  sizes: Map<string, number>,
  busy: () => boolean,
  log: (line: string) => void,
): () => void {
  const list = warmList(sizes);
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
      const s = ((performance.now() - t) / 1000).toFixed(0);
      if (r.end || r.done % 128 === 0)
        log(`fetched ahead: ${r.done} of ${list.length} chunks looked at, ${r.fetched} fetched (${mb(r.bytes)} MB), ${r.failed} failed, in ${s} s${r.end ? ", done" : ""}`);
      if (r.end) clearInterval(timer);
    },
  );
  return () => {
    Atomics.store(f, 1, 1);
    clearInterval(timer);
  };
}
