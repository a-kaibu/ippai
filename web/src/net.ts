// The page's requests on a weak or unstable link (mobile): the same policy as the game's own
// reads (host/fetch-bridge.js, the network worker; that runs as a plain script inside sake.js, so
// it has its own copy): retry dropped connections, stalled bodies and 5xx/408/429 with backoff,
// wait while the browser is offline, fail at once for other statuses. Store chunks are kept in
// Cache Storage (CHUNKS, shared with the worker): content-addressed, so never stale. A chunk is
// checked against its name (BLAKE3) before it is used or kept, as the worker does: kept ones
// carry CHECKED, one kept without it (an older page's) is checked once when read.

import { blake3Hex } from "./blake3";

export const CHUNKS = "ippai-chunks-v1";
const RETRY = new Set([0, 408, 425, 429, 500, 502, 503, 504]);
const CHECKED = "x-ippai-checked";
/** A kept chunk's length (trimChunks; one kept without it, an older page's, is measured). */
const SIZE = "x-ippai-size";
/** Fetches of one chunk that may not match its name before it fails. */
const BAD_CHUNK = 3;

export interface RetryOptions {
  /** no byte for this long: abort and try again (ms) */
  stallMs?: number;
  /** give up after this long (ms) */
  giveUpMs?: number;
  /** stop retrying (the request is aborted too) */
  signal?: AbortSignal;
  request?: typeof fetch;
}

/** Waits `ms`; while the browser reports no network, until it reports one again (at most 30 s). */
async function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  const wait = (t: number) =>
    new Promise<void>((resolve) => {
      const done = () => {
        clearTimeout(timer);
        signal?.removeEventListener("abort", done);
        resolve();
      };
      const timer = setTimeout(done, t);
      signal?.addEventListener("abort", done);
    });
  await wait(ms);
  const offline = () => typeof navigator !== "undefined" && navigator.onLine === false;
  for (let i = 0; i < 60 && offline() && !signal?.aborted; i++) await wait(500);
}

/** The delay before retry `attempt` (from 0): exponential with jitter, at most 8 s. */
export function backoff(attempt: number): number {
  return Math.min(8000, 300 * 2 ** attempt) * (0.5 + Math.random() * 0.5);
}

/** One try with the whole body read; aborted when no byte arrives for `stallMs`. */
async function once(
  url: string,
  init: RequestInit,
  stallMs: number,
  request: typeof fetch,
): Promise<{ response: Response; data?: Uint8Array<ArrayBuffer> }> {
  const abort = new AbortController();
  const outer = init.signal;
  const forward = () => abort.abort(outer?.reason);
  outer?.addEventListener("abort", forward);
  let last = Date.now();
  const watch = setInterval(
    () => Date.now() - last > stallMs && abort.abort(new Error("stalled")),
    Math.min(1000, stallMs / 4),
  );
  try {
    const response = await request(url, { ...init, signal: abort.signal });
    last = Date.now();
    if (!response.ok) {
      void response.body?.cancel().catch(() => undefined);
      return { response };
    }
    if (!response.body) return { response, data: new Uint8Array(await response.arrayBuffer()) };
    const reader = response.body.getReader();
    const parts: Uint8Array[] = [];
    let length = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      last = Date.now();
      parts.push(value);
      length += value.length;
    }
    const data = new Uint8Array(length);
    let at = 0;
    for (const p of parts) {
      data.set(p, at);
      at += p.length;
    }
    return { response, data };
  } finally {
    clearInterval(watch);
    outer?.removeEventListener("abort", forward);
  }
}

/**
 * `f()`, tried again (with backoff) when it fails other than by an HttpError: for requests that
 * must stay a plain fetch (WebAssembly.compileStreaming keeps the compiled code of its URL).
 */
export async function withRetries<T>(f: () => Promise<T>, tries = 5): Promise<T> {
  for (let attempt = 0; ; attempt++) {
    try {
      return await f();
    } catch (e) {
      if (e instanceof HttpError || attempt + 1 >= tries) throw e;
      await sleep(backoff(attempt));
    }
  }
}

/** HTTP failure that retrying will not fix (404 and the like). */
export class HttpError extends Error {
  constructor(
    readonly status: number,
    /** offline, and the file is not on this device (sw.ts) */
    readonly offline = false,
  ) {
    super(offline ? "オフラインで、端末に保存されていません" : `HTTP ${status}`);
  }
}

/** `url`'s body, retried as above. Throws HttpError, or the last error after `giveUpMs`. */
export async function fetchRetry(
  url: string | URL,
  init: RequestInit = {},
  opt: RetryOptions = {},
): Promise<Uint8Array<ArrayBuffer>> {
  const request = opt.request ?? ((i, n) => fetch(i, n));
  const stallMs = opt.stallMs ?? 20000;
  const giveUpMs = opt.giveUpMs ?? 15 * 60000;
  const start = Date.now();
  let retrying = false;
  try {
    for (let attempt = 0; ; attempt++) {
      let error: unknown;
      let after = 0;
      try {
        const { response, data } = await once(String(url), { ...init, signal: opt.signal }, stallMs, request);
        if (data) return data;
        // (sw.ts: offline, and not kept on the device)
        if (response.headers.has("x-ippai-offline")) throw new HttpError(response.status, true);
        if (!RETRY.has(response.status)) throw new HttpError(response.status);
        error = new Error(`HTTP ${response.status}`);
        after = Math.min((Number(response.headers.get("retry-after")) || 0) * 1000, 30000);
      } catch (e) {
        if (e instanceof HttpError || opt.signal?.aborted) throw e;
        error = e;
      }
      if (Date.now() - start > giveUpMs) throw error;
      if (!retrying) {
        retrying = true;
        page.retrying++;
      }
      page.report(String(error instanceof Error ? error.message : error));
      await sleep(after || backoff(attempt), opt.signal);
      if (opt.signal?.aborted) throw opt.signal.reason;
    }
  } finally {
    if (retrying) {
      page.retrying--;
      page.report("");
    }
  }
}

/** The page's own requests being retried, for watchRetries. */
const page = {
  retrying: 0,
  listeners: new Set<(error: string) => void>(),
  report(error: string) {
    for (const l of this.listeners) l(error);
  },
};

/** Cache Storage for chunks (null where there is none: http:// other than localhost). */
export async function chunkCache(): Promise<Cache | null> {
  try {
    return typeof caches === "undefined" ? null : await caches.open(CHUNKS);
  } catch {
    return null;
  }
}

export const chunkPath = (hex: string) => `chunks/${hex.slice(0, 2)}/${hex}`;

/**
 * Store chunk `hex` (decoded): from Cache Storage, else fetched (and kept there). `cached`:
 * whether it came without the network.
 */
export async function getChunk(
  store: string,
  hex: string,
  opt: RetryOptions & { cache?: Cache | null; priority?: RequestPriority } = {},
): Promise<{ data: Uint8Array<ArrayBuffer>; cached: boolean }> {
  const url = new URL(chunkPath(hex), store).href;
  const cache = opt.cache === undefined ? await chunkCache() : opt.cache;
  const keep = (data: Uint8Array<ArrayBuffer>) => (cache ? keepChunk(cache, url, data) : undefined);
  const hit = await cache?.match(url).catch(() => undefined);
  if (hit) {
    const data = new Uint8Array(await hit.arrayBuffer());
    if (hit.headers.get(CHECKED) === "1") return { data, cached: true };
    if (blake3Hex(data) === hex) {
      void keep(data);
      return { data, cached: true };
    }
    await cache?.delete(url).catch(() => undefined);
  }
  for (let bad = 1; ; bad++) {
    // (again past the browser's HTTP cache, which keeps chunks for good)
    const init: RequestInit = {
      priority: opt.priority ?? "auto",
      ...(bad > 1 ? { cache: "reload" as const } : {}),
    };
    const data = await fetchRetry(url, init, opt);
    if (blake3Hex(data) === hex) {
      await keep(data);
      return { data, cached: false };
    }
    if (bad >= BAD_CHUNK) throw new Error(`chunk ${hex} does not match its name (${bad} times)`);
    await new Promise((r) => setTimeout(r, backoff(bad - 1)));
  }
}

/** Keeps chunk `data` (checked against its name) at `url` in `cache`, as getChunk does. */
export function keepChunk(cache: Cache, url: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
  return cache
    .put(url, new Response(data, { headers: { [CHECKED]: "1", [SIZE]: String(data.length) } }))
    .catch(() => undefined);
}

/** Bytes of chunks kept at most: the newest are kept. */
// (2 << 30 is negative in JS: until 2026-11-07 every trim emptied the cache of all but the
// saved games' chunks)
const KEEP_BYTES = 2 * 1024 ** 3;

/**
 * Keeps the chunk cache within its share of the origin's storage: half the quota and 2 GB, and
 * with what else the origin keeps (saves and states in OPFS, the app) the whole under SHARE of the
 * quota, those never going. The oldest chunks go first, but not those `saved()` lists (games saved
 * to the device: offline.ts savedChunks; only asked when something must go). Sizes are the
 * chunks' own (x-ippai-size; FastCDC chunks are 4 MiB on average, 64 MiB at most). Also asks the
 * browser to keep this origin's data under storage pressure.
 */
export async function trimChunks(saved: () => Promise<Set<string>> = async () => new Set()): Promise<void> {
  void navigator.storage?.persist?.().catch(() => false);
  const cache = await chunkCache();
  if (!cache) return;
  const { quota = 0, usage = 0 } = (await navigator.storage?.estimate?.().catch(() => undefined)) ?? {};
  const keys = await cache.keys();
  const sizes = await Promise.all(
    keys.map(async (k) => {
      const r = await cache.match(k).catch(() => undefined);
      if (!r) return 0;
      const n = Number(r.headers.get(SIZE));
      return n > 0 ? n : ((await r.blob().catch(() => null))?.size ?? 0);
    }),
  );
  const chunks = sizes.reduce((a, b) => a + b, 0);
  let limit = Math.min(KEEP_BYTES, quota ? quota * 0.5 : KEEP_BYTES);
  // (usage counts the chunks too, as the browser last measured)
  if (quota && usage) limit = Math.min(limit, quota * SHARE - Math.max(0, usage - chunks));
  let over = chunks - limit;
  if (over <= 0) return;
  const keep = await saved().catch(() => null);
  // (the saved games' lists out of reach: nothing goes rather than their chunks)
  if (!keep) return;
  for (const [i, k] of keys.entries()) {
    if (over <= 0) break;
    if (keep.has(k.url.slice(k.url.lastIndexOf("/") + 1))) continue;
    await cache.delete(k);
    over -= sizes[i]!;
  }
}
/** Of the quota, what the origin may fill in all before chunks go (trimChunks). */
const SHARE = 0.8;

/** The store chunk hashes (hex) of a tree's file list (trees/<name>.tsv, crates/ippai/src/pack.rs). */
export function treeChunks(tsv: string): string[] {
  const out: string[] = [];
  for (const line of tsv.split("\n").slice(1)) {
    const f = line.split("\t");
    if (f[0] !== "f" || !f[5]) continue;
    // (<hash>:<length>, ":<length>" all zero)
    for (const c of f[5].split(",")) {
      const h = c.split(":")[0]!;
      if (/^[0-9a-f]{32}$/.test(h)) out.push(h);
    }
  }
  return out;
}

/** The store chunks' lengths (hex -> bytes) of a tree's file list (as treeChunks). */
export function treeChunkSizes(tsv: string, into = new Map<string, number>()): Map<string, number> {
  for (const line of tsv.split("\n").slice(1)) {
    const f = line.split("\t");
    if (f[0] !== "f" || !f[5]) continue;
    for (const c of f[5].split(",")) {
      const [h, n] = c.split(":");
      if (h && /^[0-9a-f]{32}$/.test(h) && Number(n) > 0) into.set(h, Number(n));
    }
  }
  return into;
}

/** How many of the page's and the game's requests are being retried now (BroadcastChannel). */
export function watchRetries(on: (retrying: number, error: string) => void): () => void {
  const by = new Map<string, number>();
  const tell = (error: string) => on(page.retrying + [...by.values()].reduce((a, b) => a + b, 0), error);
  page.listeners.add(tell);
  const ch = typeof BroadcastChannel === "undefined" ? null : new BroadcastChannel("ippai-net");
  if (ch)
    ch.onmessage = (e: MessageEvent<{ id: string; retrying: number; error: string }>) => {
      by.set(e.data.id, e.data.retrying);
      tell(e.data.error);
    };
  return () => {
    page.listeners.delete(tell);
    ch?.close();
  };
}
