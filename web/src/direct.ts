// Store chunks straight from the bucket (S3) for the page, as host/fetch-bridge.js ippaiDirect
// does it for the game (that runs as a plain script inside sake.js, so it has its own copy):
// ippai serve's /store-direct gives a presigned URL, the object's AES-256-GCM key and UUID; the
// object is fetched, opened (nonce first, the UUID as associated data, crates/ippai/src/envelope.rs),
// zstd-decoded by sake's snapz and checked against its name. The bucket answers each object
// after ~2 s whatever its size but takes many at once, so saving a whole game to the device
// (offline.ts) goes many at a time this way instead of through the server's own line.

import { blake3Hex } from "./blake3";
import { fetchRetry, HttpError, type RetryOptions } from "./net";
import { sakeBase } from "./runtime";

export interface DirectInfo {
  url: string;
  key: string;
  object: string;
}

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

/** sake's snapz (sake/snapz.mjs): a chunk is 16 MiB at most (4x FastCDC's 4 MiB average), 64 MiB tried once more. */
export const snapzUnzstd: Unzstd = async (frame) => {
  snapz ??= import(/* @vite-ignore */ `${sakeBase()}snapz.mjs`).then((m: { default: () => Promise<Snapz> }) =>
    m.default(),
  );
  const z = await snapz.catch((e: unknown) => {
    snapz = undefined;
    throw e;
  });
  for (const cap of [16 << 20, 64 << 20]) {
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

/** Object `cipher` (nonce, ciphertext and tag) opened with `info`'s key. */
export async function openSealed(info: DirectInfo, cipher: Uint8Array): Promise<Uint8Array> {
  if (cipher.length < 28) throw new Error("short");
  const key = await crypto.subtle.importKey("raw", hexBytes(info.key), "AES-GCM", false, ["decrypt"]);
  const plain = await crypto.subtle.decrypt(
    {
      name: "AES-GCM",
      iv: cipher.slice(0, 12),
      additionalData: hexBytes(info.object.replace(/-/g, "")),
      tagLength: 128,
    },
    key,
    cipher.slice(12),
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
export function directFetcher(
  store: string,
  opt: { request?: typeof fetch; unzstd?: Unzstd; askMs?: number } = {},
): { get(hex: string, retry?: RetryOptions): Promise<Uint8Array<ArrayBuffer> | null>; off(): boolean } {
  const request = opt.request ?? ((i, n) => fetch(i, n));
  const unzstd = opt.unzstd ?? snapzUnzstd;
  const base = new URL("../store-direct/", store).href;
  let off = typeof crypto === "undefined" || !crypto.subtle;
  return {
    off: () => off,
    async get(hex, retry = {}) {
      retry.signal?.throwIfAborted();
      if (off) return null;
      let info: unknown;
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
          cipher = await fetchRetry(info.url, { cache: "no-store" }, { giveUpMs: 60000, ...retry, request });
        } catch (e) {
          if (retry.signal?.aborted) throw e;
          // (an expired URL and the like: the server)
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
