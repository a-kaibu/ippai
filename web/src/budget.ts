// The page's share of the bucket's budget of requests (host/fetch-bridge.js has the network
// worker's copy, which the page gives this one's buffer: one budget for the game's reads, its
// fetches ahead and a save to the device; docs/s3-reads.md). The bucket lets one address make
// 3000 requests in 5 minutes: requests refill the budget at RATE a second up to BURST, and each
// class leaves RESERVE[class] to the classes above it (the game's may go into debt).
// Layout: an Int32 lock word, then [tokens, last refill, blocked until (Date.now())] as Float64:
// BYTES in all.

/** Request classes, most urgent first (as fetch-bridge.js IPPAI_CLASS). */
export const CLASS = { game: 0, ahead: 1, hint: 2, warm: 3, save: 4 } as const;
export const RATE = 9;
export const BURST = 40;
export const RESERVE = [-RATE * 5, 0, 8, 16, 16];
export const BYTES = 32;

let shared: SharedArrayBuffer | ArrayBuffer | undefined;

/** The page's budget (made once): handed to the network worker (play.ts, Module.ippaiNetBudget). */
export function netBudget(): SharedArrayBuffer | ArrayBuffer {
  if (!shared) {
    shared = typeof SharedArrayBuffer === "undefined" ? new ArrayBuffer(BYTES) : new SharedArrayBuffer(BYTES);
    new Float64Array(shared, 8, 3).set([BURST, Date.now(), 0]);
  }
  return shared;
}

function withBudget<T>(f: (v: Float64Array) => T, buf = netBudget()): T {
  const lock = new Int32Array(buf, 0, 1);
  const v = new Float64Array(buf, 8, 3);
  while (Atomics.compareExchange(lock, 0, 0, 1) !== 0);
  try {
    return f(v);
  } finally {
    Atomics.store(lock, 0, 0);
  }
}

/** Takes one request of class `cls` (refilled to `now`): whether it may go. None while the
 * bucket asked to wait (drain). */
export function take(cls: number, now = Date.now(), buf?: SharedArrayBuffer | ArrayBuffer): boolean {
  return withBudget((v) => {
    if (now < v[2]!) return false;
    const dt = Math.max(0, now - v[1]!) / 1000;
    v[0] = Math.min(BURST, v[0]! + dt * RATE);
    v[1] = Math.max(v[1]!, now);
    if (v[0] - 1 < (RESERVE[cls] ?? 0)) return false;
    v[0] -= 1;
    return true;
  }, buf);
}

/** The bucket said 429: no class goes for `ms`, then the budget refills from none. */
export function drain(ms: number, now = Date.now(), buf?: SharedArrayBuffer | ArrayBuffer): void {
  withBudget((v) => {
    const until = now + ms;
    v[2] = Math.max(v[2]!, until);
    v[0] = Math.min(v[0]!, 0);
    v[1] = Math.max(v[1]!, until);
  }, buf);
}

/** Waits until a request of class `cls` may go (or `signal` aborts). */
export async function gate(cls: number, signal?: AbortSignal): Promise<void> {
  while (!take(cls)) {
    signal?.throwIfAborted();
    await new Promise((r) => setTimeout(r, 50));
  }
}

/** Whether the game waits for a read of its own now (play.ts sets it: the module's count). */
let busy: () => boolean = () => false;
export function setGameBusy(f: () => boolean): void {
  busy = f;
}
export function gameBusy(): boolean {
  return busy();
}
