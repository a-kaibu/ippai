// The page's share of the bucket's budget of requests (host/fetch-bridge.js has the network
// worker's copy, which the page gives this one's buffer: one budget for the game's reads, its
// fetches ahead and a save to the device; docs/s3-reads.md). The bucket lets one address make
// 3000 requests in 5 minutes: requests refill the budget at RATE a second up to BURST, and each
// class leaves RESERVE[class] to the classes above it (the game's may go into debt). The address
// is the line's, not the tab's: tabs of this origin on the device split the budget between them
// (shareAcrossTabs: `share` of it each) and tell each other the bucket's 429s.
// Layout: an Int32 lock word (0 free, else its holder's tag), then [tokens, last refill, blocked
// until (Date.now()), share (0, 1]] as Float64: BYTES in all.
// The lock is only ever freed by its holder (or, once the page has terminated the worker that
// held it, by the page: reclaim). A taker that cannot have it soon gives up without touching the
// budget and tries again later (formal/tla/BudgetLock.tla).

/** Request classes, most urgent first (as fetch-bridge.js MASU_CLASS). */
export const CLASS = { game: 0, ahead: 1, hint: 2, warm: 3, save: 4 } as const;
export const RATE = 9;
export const BURST = 40;
export const RESERVE = [-RATE * 5, 0, 8, 16, 16];
export const BYTES = 40;
/** The page's tag in the lock word (the network workers it starts get 2, 3, ...). */
export const PAGE_TAG = 1;

let shared: SharedArrayBuffer | ArrayBuffer | undefined;

/** Fresh values for a budget made at `now`. */
export function fresh(buf: SharedArrayBuffer | ArrayBuffer, now = Date.now()): void {
  new Float64Array(buf, 8, 4).set([BURST, now, 0, 1]);
}

/** The page's budget (made once): handed to the network worker (play.ts, Module.masuNetBudget). */
export function netBudget(): SharedArrayBuffer | ArrayBuffer {
  if (!shared) {
    shared = typeof SharedArrayBuffer === "undefined" ? new ArrayBuffer(BYTES) : new SharedArrayBuffer(BYTES);
    fresh(shared);
  }
  return shared;
}

/** Tries for the lock before a taker gives up for now: a holder is inside for a few
 * instructions, so this many only fail while it is not scheduled (or the page has not yet seen
 * that it was terminated). Giving up leaves the budget as it was. */
export const SPIN_MAX = 1 << 14;

/** `f(values)` with the lock held as `tag`; `undefined` (and `f` not run) when the lock could not
 * be had within SPIN_MAX tries. */
function withBudget<T>(f: (v: Float64Array) => T, buf = netBudget(), tag = PAGE_TAG): { v: T } | undefined {
  const lock = new Int32Array(buf, 0, 1);
  for (let n = 0; Atomics.compareExchange(lock, 0, 0, tag) !== 0; n++) if (n >= SPIN_MAX) return undefined;
  try {
    return { v: f(new Float64Array(buf, 8, 4)) };
  } finally {
    // (only ours to free: a stale holder never frees a later one's)
    Atomics.compareExchange(lock, 0, tag, 0);
  }
}

/** The budget's share, read as a number in (0, 1] (a buffer from before shares: 1). */
function shareOf(v: Float64Array): number {
  const s = v[3]!;
  return s > 0 && s <= 1 ? s : 1;
}

/** Takes one request of class `cls` (refilled to `now`): whether it may go. None while the
 * bucket asked to wait (drain), nor while the lock is held elsewhere (gate tries again). */
export function take(cls: number, now = Date.now(), buf?: SharedArrayBuffer | ArrayBuffer): boolean {
  return (
    withBudget((v) => {
      if (now < v[2]!) return false;
      const s = shareOf(v);
      const dt = Math.max(0, now - v[1]!) / 1000;
      v[0] = Math.min(BURST * s, v[0]! + dt * RATE * s);
      v[1] = Math.max(v[1]!, now);
      if (v[0] - 1 < (RESERVE[cls] ?? 0) * s) return false;
      v[0] -= 1;
      return true;
    }, buf)?.v ?? false
  );
}

/** Tabs told of this page's drains at once (shareAcrossTabs). */
let told: ((until: number) => void) | undefined;

/** The bucket said 429: no class goes for `ms`, then the budget refills from none. While the lock
 * is held elsewhere, it lands as soon as the lock is free (tried again after a few ms). */
export function drain(ms: number, now = Date.now(), buf?: SharedArrayBuffer | ArrayBuffer): void {
  const until = now + ms;
  const done = withBudget((v) => {
    v[2] = Math.max(v[2]!, until);
    v[0] = Math.min(v[0]!, 0);
    v[1] = Math.max(v[1]!, until);
  }, buf);
  if (!done) setTimeout(() => drain(ms, now, buf), 5);
  else if (!buf || buf === shared) told?.(until);
}

/** How long until a request of class `cls` may go (ms; 0: now), as the budget stands at `now`
 * (a few ms while the lock is held elsewhere). */
export function waitFor(cls: number, now = Date.now(), buf?: SharedArrayBuffer | ArrayBuffer): number {
  return (
    withBudget((v) => {
      // (none goes while the bucket asked to wait; then the tokens refill from the last refill)
      const from = Math.max(now, v[2]!);
      const s = shareOf(v);
      const want = (RESERVE[cls] ?? 0) * s + 1;
      if (Math.min(BURST * s, v[0]! + (Math.max(0, from - v[1]!) / 1000) * RATE * s) >= want)
        return from - now;
      return Math.ceil(v[1]! + ((want - v[0]!) / (RATE * s)) * 1000 - now);
    }, buf)?.v ?? 10
  );
}

/** After the page terminated the worker whose tag is `dead` (host/fetch-bridge.js
 * masuFetchDispose): a lock it still held is freed, and the budget, which it may have left half
 * written, starts again from none (refilling from `now`; a 429's wait is kept). Nothing while
 * the lock is free or held by someone alive. Returns whether it reclaimed. */
export function reclaim(dead: number, now = Date.now(), buf = netBudget()): boolean {
  const lock = new Int32Array(buf, 0, 1);
  if (dead === 0 || Atomics.compareExchange(lock, 0, dead, PAGE_TAG) !== dead) return false;
  const v = new Float64Array(buf, 8, 4);
  const until = Number.isFinite(v[2]) ? v[2]! : 0;
  v.set([0, Math.max(now, until), until, shareOf(v)]);
  Atomics.compareExchange(lock, 0, PAGE_TAG, 0);
  return true;
}

/** Waits until a request of class `cls` may go (or `signal` aborts): a timer for when it may,
 * not a look every 50 ms per waiter (many waiters during a 429's pause each kept a timer). */
export async function gate(cls: number, signal?: AbortSignal): Promise<void> {
  while (!take(cls)) {
    signal?.throwIfAborted();
    // (others take tokens too: a look again by then, at most a second away)
    const ms = Math.min(1000, Math.max(10, waitFor(cls)));
    await new Promise((r) => setTimeout(r, ms));
  }
}

/** What tabs say to each other on the channel "masu-budget". */
export interface TabNote {
  id: string;
  /** the 429's wait it knows of (Date.now()), 0: none */
  until: number;
  bye?: boolean;
}
/** How often a tab says it is there, and how long a tab not heard from counts. */
export const TAB_MS = 1000;
export const TAB_GONE_MS = 3500;

/** One step of the tabs' split (pure, for shareAcrossTabs and its tests): `peers` (id -> last
 * heard) without the ones gone by `now`, and this tab's share. */
export function tabShare(peers: Map<string, number>, now: number): number {
  for (const [id, t] of peers) if (now - t > TAB_GONE_MS) peers.delete(id);
  return 1 / (1 + peers.size);
}

/** Splits the budget in `buf` with the other tabs of this origin (on this device: other devices
 * on the line are not seen; docs/s3-reads.md) and passes 429s between them: each says it is
 * there every TAB_MS, and takes 1 / (tabs) of the rate, burst and reserves. Returns a stop. */
export function shareAcrossTabs(
  buf = netBudget(),
  opt: { channel?: () => BroadcastChannel | undefined; now?: () => number; id?: string } = {},
): () => void {
  const now = opt.now ?? (() => Date.now());
  const ch =
    opt.channel?.() ??
    (typeof BroadcastChannel === "undefined" ? undefined : new BroadcastChannel("masu-budget"));
  if (!ch) return () => {};
  const id = opt.id ?? Math.random().toString(36).slice(2);
  const peers = new Map<string, number>();
  const blocked = () => withBudget((v) => v[2]!, buf)?.v ?? 0;
  const say = (n: Partial<TabNote> = {}) => ch.postMessage({ id, until: blocked(), ...n } satisfies TabNote);
  const step = () => {
    const s = tabShare(peers, now());
    // (busy: the next step sets it)
    withBudget((v) => void (v[3] = s), buf);
  };
  ch.onmessage = (e: MessageEvent<TabNote>) => {
    const m = e.data;
    if (!m || typeof m.id !== "string" || m.id === id) return;
    if (m.bye) peers.delete(m.id);
    else {
      // (a tab new to us hears of us at once: both split from the next step)
      if (!peers.has(m.id)) say();
      peers.set(m.id, now());
    }
    const t = now();
    if (Number(m.until) > t && Number(m.until) > blocked()) drain(Number(m.until) - t, t, buf);
    step();
  };
  if (buf === shared) told = (until) => ch.postMessage({ id, until } satisfies TabNote);
  const bye = () => ch.postMessage({ id, until: 0, bye: true } satisfies TabNote);
  globalThis.addEventListener?.("pagehide", bye);
  say();
  const timer = setInterval(() => {
    say();
    step();
  }, TAB_MS);
  return () => {
    clearInterval(timer);
    globalThis.removeEventListener?.("pagehide", bye);
    if (buf === shared) told = undefined;
    bye();
    ch.close();
  };
}

/** Whether the game waits for a read of its own now (play.ts sets it: the module's count). */
let busy: () => boolean = () => false;
export function setGameBusy(f: () => boolean): void {
  busy = f;
}
export function gameBusy(): boolean {
  return busy();
}
