// Store chunks straight from the bucket (S3) for the page, as host/fetch-bridge.js masuDirect
// does it for the game (that runs as a plain script inside sake.js, so it has its own copy):
// masu serve's /store-direct gives a presigned URL, the object's AES-256-GCM key and UUID; the
// object is fetched, opened (nonce first, the UUID as associated data, crates/masu/src/envelope.rs),
// zstd-decoded by sake's snapz and checked against its name. The bucket answers each object
// after ~2 s whatever its size but takes many at once, so saving a whole game to the device
// (offline.ts) goes many at a time this way instead of through the server's own line.

import { blake3Hex } from "./blake3";
import { CLASS } from "./budget";
import { fetchRetry, HttpError, type RetryOptions } from "./net";
import { sakeBase } from "./runtime";

export interface DirectInfo {
  url: string;
  key: string;
  object: string;
  /** Where a delivery pack holds the same object (crates/masu/src/storage.rs pack_objects). */
  pack?: PackSpan;
}

/** A delivery pack's place for one object: read with Range: bytes=offset-(offset+len-1). */
export interface PackSpan {
  id: string;
  url: string;
  offset: number;
  len: number;
}

/** Whether `v` is a well-formed pack place (host/fetch-bridge.js masuPackValid). */
export function isPackSpan(v: unknown): v is PackSpan {
  const p = v as Partial<PackSpan> | null;
  return (
    !!p &&
    typeof p.url === "string" &&
    typeof p.id === "string" &&
    /^[0-9a-f-]{36}$/.test(p.id) &&
    Number.isSafeInteger(p.offset) &&
    p.offset! >= 0 &&
    Number.isSafeInteger(p.len) &&
    p.len! >= 28
  );
}

/** A save's one ranged GET of a pack at most: bytes, chunks (host/fetch-bridge.js MASU_PACK_SPAN). */
export const PACK_RUN_BYTES = 32 << 20;
export const PACK_RUN_MAX = 64;

/** How long the server may take to say how to fetch a chunk from the bucket (then: the server's /store). */
export const DIRECT_ASK_MS = 10000;
/** Fetches of one object that may not open or match its name before the server is asked instead. */
const BAD = 3;

/** zstd frame -> bytes. */
export type Unzstd = (frame: Uint8Array) => Promise<Uint8Array>;

interface Snapz {
  HEAPU8: Uint8Array;
  _malloc(n: number): number;
  _free?(p: number): void;
  _snapz_decompress(dst: number, cap: number, src: number, n: number): number;
}

let snapz: Promise<Snapz> | undefined;

/** A zstd frame's content size from its header (RFC 8878 3.1.1.1), null when it does not say
 * (or `frame` is not one). */
export function zstdContentSize(frame: Uint8Array): number | null {
  if (frame.length < 6 || frame[0] !== 0x28 || frame[1] !== 0xb5 || frame[2] !== 0x2f || frame[3] !== 0xfd)
    return null;
  const fhd = frame[4]!;
  const single = (fhd >> 5) & 1;
  const fcs = [single, 2, 4, 8][fhd >> 6]!;
  if (!fcs) return null;
  let at = 5 + (single ? 0 : 1) + [0, 1, 2, 4][fhd & 3]!;
  if (at + fcs > frame.length) return null;
  let v = 0;
  for (let i = fcs - 1; i >= 0; i--) v = v * 256 + frame[at + i]!;
  at += fcs;
  return fcs === 2 ? v + 256 : v;
}

/** The largest chunk a frame may decode to here (FastCDC's 64 MiB at most). */
const UNZSTD_MAX = 64 << 20;

/** sake's snapz (sake/snapz.mjs): the destination as large as the frame says (else 16 MiB, then
 * 64 MiB once more: each try's 16 or 64 MiB grew snapz's memory for good, and with no _free each
 * decode leaked them). */
export const snapzUnzstd: Unzstd = async (frame) => {
  snapz ??= import(/* @vite-ignore */ `${sakeBase()}snapz.mjs`).then((m: { default: () => Promise<Snapz> }) =>
    m.default(),
  );
  const z = await snapz.catch((e: unknown) => {
    snapz = undefined;
    throw e;
  });
  if (!z._free) throw new Error("zstd: this snapz cannot free");
  const said = zstdContentSize(frame);
  if (said !== null && said > UNZSTD_MAX) throw new Error(`zstd: a frame of ${said} bytes`);
  for (const cap of said !== null ? [Math.max(1, said)] : [16 << 20, UNZSTD_MAX]) {
    const src = z._malloc(frame.length) >>> 0;
    const dst = z._malloc(cap) >>> 0;
    try {
      if (!src || !dst) throw new Error("zstd: no memory");
      z.HEAPU8.set(frame, src);
      const n = z._snapz_decompress(dst, cap, src, frame.length) >>> 0;
      if (n !== 0xffffffff) return z.HEAPU8.slice(dst, dst + n);
    } finally {
      z._free?.(src);
      z._free?.(dst);
    }
  }
  throw new Error("zstd: not a frame, or too big");
};

const hexBytes = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));

// (the keys imported once each: a game's objects share one key, imported again per chunk before)
const keys = new Map<string, Promise<CryptoKey>>();
const KEYS_MAX = 64;
function keyOf(hex: string): Promise<CryptoKey> {
  let k = keys.get(hex);
  if (!k) {
    if (keys.size >= KEYS_MAX) keys.delete(keys.keys().next().value!);
    k = crypto.subtle.importKey("raw", hexBytes(hex), "AES-GCM", false, ["decrypt"]);
    keys.set(hex, k);
    k.catch(() => keys.delete(hex));
  }
  return k;
}

/** Object `cipher` (nonce, ciphertext and tag) opened with `info`'s key. */
export async function openSealed(info: DirectInfo, cipher: Uint8Array): Promise<Uint8Array> {
  if (cipher.length < 28) throw new Error("short");
  // (views of the object, not copies of it)
  const plain = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: cipher.subarray(0, 12) as Uint8Array<ArrayBuffer>,
      additionalData: hexBytes(info.object.replace(/-/g, "")),
      tagLength: 128,
    },
    await keyOf(info.key),
    cipher.subarray(12) as Uint8Array<ArrayBuffer>,
  );
  return new Uint8Array(plain);
}

/** Whether `v` is what /store-direct answers. */
export function isDirectInfo(v: unknown): v is DirectInfo {
  const i = v as Partial<DirectInfo> | null;
  return (
    !!i &&
    typeof i.url === "string" &&
    typeof i.key === "string" &&
    /^[0-9a-f]{64}$/.test(i.key) &&
    typeof i.object === "string" &&
    /^[0-9a-f-]{36}$/.test(i.object)
  );
}

/**
 * Fetches chunks from the bucket for one store (its /store/ URL). `get(hex)`: the decoded,
 * checked chunk, or null when there is no way (a store without S3 or an older server: then
 * never again for this fetcher; a chunk only on the server, a stuck server, an object that
 * would not open): the caller asks the server.
 */
/** Chunks one POST /store-direct asks for (host/fetch-bridge.js MASU_DIRECT_MANY). */
export const DIRECT_MANY = 512;

export function directFetcher(
  store: string,
  opt: {
    request?: typeof fetch;
    unzstd?: Unzstd;
    askMs?: number;
    cls?: number;
    /** a run's caps [PACK_RUN_BYTES, PACK_RUN_MAX] */
    packRun?: [number, number];
  } = {},
): {
  get(hex: string, retry?: RetryOptions): Promise<Uint8Array<ArrayBuffer> | null>;
  prime(hexes: string[], signal?: AbortSignal): Promise<void>;
  /** How many of `hexes` from `from` on one ranged GET of a pack fetches (1: alone). */
  span(hexes: string[], from: number): number;
  /** `hexes` (of one pack, as span gave them) with one ranged GET: those that came, checked. */
  getRun(hexes: string[], retry?: RetryOptions): Promise<Map<string, Uint8Array<ArrayBuffer>>>;
  /** (get's work with what is known of `hex`: EXPIRED for a URL the bucket refused) */
  getWith(
    hex: string,
    info: unknown,
    retry?: RetryOptions,
  ): Promise<Uint8Array<ArrayBuffer> | null | typeof EXPIRED>;
  off(): boolean;
} {
  const request = opt.request ?? ((i, n) => fetch(i, n));
  const unzstd = opt.unzstd ?? snapzUnzstd;
  const base = new URL("../store-direct/", store).href;
  let off = typeof crypto === "undefined" || !crypto.subtle;
  // what POST /store-direct said (null: only on the server), and whether the server has it
  const known = new Map<string, DirectInfo | null>();
  let many = true;
  return {
    off: () => off,
    /** Asks the server how to fetch `hexes` at once, DIRECT_MANY at a time (store_api.rs
     * direct_many): their gets then go to the bucket without a round trip to the server each. */
    async prime(hexes, signal) {
      const want = hexes.filter((h) => !known.has(h));
      for (let i = 0; many && !off && i < want.length; i += DIRECT_MANY) {
        const part = want.slice(i, i + DIRECT_MANY);
        let r: Response;
        try {
          r = await request(base.slice(0, -1), {
            method: "POST",
            cache: "no-store",
            headers: { "content-type": "application/json" },
            body: JSON.stringify(part.map((h) => `chunks/${h.slice(0, 2)}/${h}`)),
            signal: signal
              ? AbortSignal.any([signal, AbortSignal.timeout(DIRECT_ASK_MS * 3)])
              : AbortSignal.timeout(DIRECT_ASK_MS * 3),
          });
        } catch {
          signal?.throwIfAborted();
          return;
        }
        if (r.status === 501) off = true;
        // (an older server: one by one)
        if (r.status === 404 || r.status === 405) many = false;
        if (!r.ok) return;
        const all = (await r.json().catch(() => null)) as Record<string, unknown> | null;
        if (!all) return;
        for (const h of part) {
          const v = all[`chunks/${h.slice(0, 2)}/${h}`];
          known.set(h, isDirectInfo(v) ? { ...v, pack: isPackSpan(v.pack) ? v.pack : undefined } : null);
        }
      }
    },
    span(hexes, from) {
      const [maxBytes, maxRun] = opt.packRun ?? [PACK_RUN_BYTES, PACK_RUN_MAX];
      const first = known.get(hexes[from]!)?.pack;
      if (!first || off) return 1;
      let end = first.offset + first.len;
      let n = 1;
      while (from + n < hexes.length && n < maxRun) {
        const p = known.get(hexes[from + n]!)?.pack;
        if (!p || p.id !== first.id || p.offset !== end || end + p.len - first.offset > maxBytes) break;
        end += p.len;
        n++;
      }
      return n;
    },
    async getRun(hexes, retry = {}) {
      const out = new Map<string, Uint8Array<ArrayBuffer>>();
      const all = hexes
        .map((h) => [h, known.get(h)] as const)
        .filter((x): x is [string, DirectInfo & { pack: PackSpan }] => !!x[1]?.pack);
      if (!all.length || off) return out;
      // (the first one's pack: others are left out)
      const pack = all[0]![1].pack;
      const infos = all.filter(([, i]) => i.pack.id === pack.id);
      const from = Math.min(...infos.map(([, i]) => i.pack.offset));
      const to = Math.max(...infos.map(([, i]) => i.pack.offset + i.pack.len));
      // (what the server said is used once: kept for a whole save it held tens of MB of URLs,
      // and a presigned URL kept past its life answered 403 for a whole window)
      for (const [h] of infos) known.delete(h);
      let bytes: Uint8Array;
      try {
        bytes = await fetchRetry(
          pack.url,
          { cache: "no-store", headers: { range: `bytes=${from}-${to - 1}` } },
          { giveUpMs: 60000, ...retry, request, budget: opt.cls ?? CLASS.save },
        );
      } catch (e) {
        if (retry.signal?.aborted) throw e;
        return out;
      }
      // (a bucket that sends the whole pack for a range: the range from it)
      if (bytes.length !== to - from)
        bytes = bytes.length >= to ? bytes.subarray(from, to) : new Uint8Array(0);
      if (bytes.length !== to - from) return out;
      for (const [h, i] of infos) {
        const at = i.pack.offset - from;
        const plain = await openSealed(i, bytes.subarray(at, at + i.pack.len)).catch(() => null);
        const data = plain && (await unzstd(plain).catch(() => null));
        if (data && blake3Hex(data) === h) out.set(h, new Uint8Array(data));
      }
      return out;
    },
    async get(hex, retry = {}) {
      retry.signal?.throwIfAborted();
      if (off) return null;
      let info: unknown = known.get(hex);
      // (used once: see getRun)
      known.delete(hex);
      if (info === null) return null;
      for (let asked = 0; ; asked++) {
        const got = await this.getWith(hex, info, retry);
        // (a URL primed earlier that expired since: asked for again once)
        if (got !== EXPIRED || asked || info === undefined) return got === EXPIRED ? null : got;
        info = undefined;
      }
    },
    async getWith(
      hex: string,
      info: unknown,
      retry: RetryOptions = {},
    ): Promise<Uint8Array<ArrayBuffer> | null | typeof EXPIRED> {
      if (info === undefined)
        try {
          const signal = retry.signal
            ? AbortSignal.any([retry.signal, AbortSignal.timeout(opt.askMs ?? DIRECT_ASK_MS)])
            : AbortSignal.timeout(opt.askMs ?? DIRECT_ASK_MS);
          const r = await request(`${base}chunks/${hex.slice(0, 2)}/${hex}`, { cache: "no-store", signal });
          // (501: a store without S3; 404: this chunk is only on the server)
          if (r.status === 501) off = true;
          if (!r.ok) return null;
          try {
            info = await r.json();
          } catch (e) {
            // (not JSON: a server without /store-direct answered with something else)
            if (!(e instanceof DOMException && (e.name === "TimeoutError" || e.name === "AbortError")))
              off = true;
            return null;
          }
        } catch {
          if (retry.signal?.aborted) throw retry.signal.reason;
          return null;
        }
      if (!isDirectInfo(info)) return null;
      for (let bad = 0; bad < BAD; bad++) {
        let cipher: Uint8Array;
        try {
          // (the bucket's budget of requests, shared with the game's: budget.ts; each try)
          cipher = await fetchRetry(
            info.url,
            { cache: "no-store" },
            { giveUpMs: 60000, ...retry, request, budget: opt.cls ?? CLASS.save },
          );
        } catch (e) {
          if (retry.signal?.aborted) throw e;
          // (an expired URL: asked for again; others: the server)
          if (e instanceof HttpError && (e.status === 401 || e.status === 403)) return EXPIRED;
          if (e instanceof HttpError || bad + 1 >= BAD) return null;
          continue;
        }
        const plain = await openSealed(info, cipher).catch(() => null);
        const data = plain && (await unzstd(plain).catch(() => null));
        if (data && blake3Hex(data) === hex) return new Uint8Array(data);
      }
      return null;
    },
  };
}

/** What getWith answers for a URL the bucket refused as expired. */
const EXPIRED = Symbol("expired");

/** Runs at most `n` of the functions given at once. */
export function limit(n: number): <T>(f: () => Promise<T>) => Promise<T> {
  let active = 0;
  const waiting: (() => void)[] = [];
  return async (f) => {
    while (active >= n) await new Promise<void>((r) => waiting.push(r));
    active++;
    try {
      return await f();
    } finally {
      active--;
      waiting.shift()?.();
    }
  };
}
