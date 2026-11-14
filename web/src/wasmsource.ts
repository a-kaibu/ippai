// Where the page gets sake's wasm (~23 MB, ~5 MB as zstd). Normally from the server
// (sake/<name>.wasm?v=...), which the browser keeps with its compiled code. But a server far
// away or on a slow line (worker2 gave ~100 KB/s at times) held the start for minutes before
// the first instruction ran, the page saying only "起動中". ippai serve puts its wasm in the
// bucket the game's chunks come from (crates/ippai/src/store_api.rs publish_wasm) and
// /sake-direct/<name> says how to fetch it there, as /store-direct does for a chunk. So:
//
// 1. the browser has the server's file already (HTTP cache): that, as before (the compiled
//    code comes with it);
// 2. the device's chunk store (OPFS) has it from an earlier start: that;
// 3. the bucket: fetched, opened, zstd-decoded, checked against its name, kept on the device;
// 4. none of these (an older server, no S3, the bucket failing): the server's file, as before.
//
// 1 and 4 are `null` (emscripten loads the URL itself); 2 and 3 the bytes, which play.ts hands
// to sake.js through Module.instantiateWasm.

import { CLASS } from "./budget";
import { directFetcher, isDirectInfo, type Unzstd } from "./direct";
import type { ChunkStore } from "./chunkstore";
import { keepChunk } from "./net";

export interface WasmSourceDeps {
  request?: typeof fetch;
  /** the device's chunk store (none: not kept) */
  device?: () => Promise<ChunkStore | null>;
  /** what fetches and checks a chunk from the bucket given /sake-direct's answer */
  fetchChunk?: (store: string, info: unknown, hex: string) => Promise<Uint8Array | null>;
  /** zstd frame -> bytes (default: sake's snapz) */
  unzstd?: Unzstd;
  /** how long the server may take to answer /sake-direct */
  askMs?: number;
  note?: (line: string) => void;
}

/** What loaded it, for the run log. */
export type WasmFrom = "http-cache" | "device" | "bucket" | "server";

/**
 * sake's wasm `name` (`sake.wasm`) as bytes from the device or the bucket, or null for the
 * server's `url` (the versioned URL emscripten would load).
 */
export async function sakeWasmBytes(
  name: string,
  url: string,
  store: string,
  deps: WasmSourceDeps = {},
): Promise<{ bytes: Uint8Array<ArrayBuffer> | null; from: WasmFrom }> {
  const request = deps.request ?? ((i, n) => fetch(i, n));
  const note = deps.note ?? (() => {});
  // 1. kept by the browser: emscripten's own load is quickest (and keeps the compiled code)
  try {
    const r = await request(url, { cache: "only-if-cached", mode: "same-origin" });
    if (r.ok) {
      await r.body?.cancel().catch(() => {});
      return { bytes: null, from: "http-cache" };
    }
  } catch {
    // (not kept, or a browser without only-if-cached)
  }
  // which chunk it is (and how to fetch it from the bucket)
  let info: unknown;
  try {
    const r = await request(new URL(`../sake-direct/${encodeURIComponent(name)}`, store).href, {
      cache: "no-store",
      signal: AbortSignal.timeout(deps.askMs ?? 10000),
    });
    if (!r.ok) return { bytes: null, from: "server" };
    info = await r.json();
  } catch {
    return { bytes: null, from: "server" };
  }
  const hex = (info as { chunk?: unknown } | null)?.chunk;
  if (typeof hex !== "string" || !/^[0-9a-f]{32}$/.test(hex) || !isDirectInfo(info))
    return { bytes: null, from: "server" };
  const key = `${store}chunks/${hex.slice(0, 2)}/${hex}`;
  // 2. on the device from an earlier start (kept only once checked, chunkstore.ts)
  const device = await (deps.device?.() ?? Promise.resolve(null)).catch(() => null);
  try {
    const kept = await device?.match(key);
    if (kept) {
      const bytes = new Uint8Array(await kept.arrayBuffer());
      if (bytes.length) return { bytes, from: "device" };
    }
  } catch {
    // (a store that fails to read: the bucket)
  }
  // 3. the bucket
  const t0 = Date.now();
  const fetchChunk =
    deps.fetchChunk ?? ((s: string, i: unknown, h: string) => bucketChunk(s, i, h, request, deps.unzstd));
  const bytes = await fetchChunk(store, info, hex).catch(() => null);
  if (!bytes) return { bytes: null, from: "server" };
  note(
    `${name}: ${(bytes.length / 2 ** 20).toFixed(1)} MiB from the bucket in ${((Date.now() - t0) / 1000).toFixed(1)} s`,
  );
  // (as every checked chunk is kept: marked so, it goes to OPFS, not the old Cache Storage)
  if (device) void keepChunk(device, key, new Uint8Array(bytes));
  return { bytes: new Uint8Array(bytes), from: "bucket" };
}

/** The chunk `hex` that /sake-direct's `info` says how to fetch, opened and checked (direct.ts). */
async function bucketChunk(
  store: string,
  info: unknown,
  hex: string,
  request: typeof fetch,
  unzstd?: Unzstd,
): Promise<Uint8Array | null> {
  // (the fetcher asks /store-direct for the chunk unless told: it is told what /sake-direct said)
  const answered = (async (input: RequestInfo | URL, init?: RequestInit) => {
    const u = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    if (u.includes("/store-direct/")) return Response.json(info);
    return request(input, init);
  }) as typeof fetch;
  return directFetcher(store, { request: answered, cls: CLASS.game, ...(unzstd ? { unzstd } : {}) }).get(
    hex,
    {
      giveUpMs: 120000,
    },
  );
}

/**
 * What sake.js's module object gets for `got`'s bytes: Module.instantiateWasm, with the module
 * compiled once (the children a process starts are given the same object); nothing for none
 * (emscripten loads its URL). A module that fails to compile is none too.
 */
export async function wasmModuleArg(
  got: Promise<{ bytes: Uint8Array<ArrayBuffer> | null; from: WasmFrom }>,
  note: (line: string) => void = () => {},
): Promise<{
  instantiateWasm?: (
    imports: WebAssembly.Imports,
    done: (instance: WebAssembly.Instance, module: WebAssembly.Module) => void,
  ) => object;
}> {
  const { bytes, from } = await got;
  note(`sake wasm: ${from}`);
  if (!bytes) return {};
  let module: WebAssembly.Module;
  try {
    module = await WebAssembly.compile(bytes);
  } catch (e) {
    note(`sake wasm from the ${from} did not compile (${String(e)}): the server's`);
    return {};
  }
  return {
    instantiateWasm(imports, done) {
      void WebAssembly.instantiate(module, imports).then((instance) => done(instance, module));
      // (emscripten: an empty object while it instantiates asynchronously)
      return {};
    },
  };
}
