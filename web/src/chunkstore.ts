// Where store chunks are kept on the device: the origin private file system (OPFS), one file a
// chunk (or a block of one) in the folder CHUNK_DIR, named as the last part of its URL (its hex
// name; a block's "<hex>?b=<k>&s=<block>"). Cache Storage before (net.ts CHUNKS) is still read
// and its entries go when they are put or deleted again, so a game saved to the device before
// stays saved. The network worker has its own copy (host/fetch-bridge.js masuOpfsStore), which
// reads with synchronous access handles: 4-5x quicker a read than Cache Storage there
// (docs/s3-reads.md), and Cache Storage sometimes did not answer at all.
//
// A whole chunk's file is kept only once checked against its name (as every put of one is),
// so it reads back as checked; a block's is not. A file's writes land whole at close
// (createWritable writes a copy), but the file exists empty from its creation on: an empty one
// reads as not there (no chunk or block is empty).

/** What net.ts, offline.ts and play.ts use of a chunk cache (Cache Storage has it too). */
export interface ChunkStore {
  match(request: Key): Promise<Response | undefined>;
  put(request: Key, response: Response): Promise<void>;
  delete(request: Key): Promise<boolean>;
  keys(): Promise<readonly { url: string }[]>;
  /** What is kept, with each one's bytes and when it was written (ms; 0: not known), none empty
   * (chunkstore's own; a plain Cache has none: listChunks measures it). */
  list?(): Promise<Kept[]>;
}
/** A kept chunk or block: its key, bytes, when written (ms since 1970; 0: not known). */
export interface Kept {
  url: string;
  size: number;
  time: number;
}
/** How many files are opened at once to list a store (not all of them together: tens of
 * thousands of File objects on a phone's main thread). */
const LIST_AT_ONCE = 16;

/** Runs `f` over `xs`, `width` at a time, in order of the results. */
async function batched<T, R>(xs: readonly T[], width: number, f: (x: T) => Promise<R>): Promise<R[]> {
  const out: R[] = Array.from({ length: xs.length });
  let next = 0;
  await Promise.all(
    Array.from({ length: Math.min(width, xs.length) }, async () => {
      while (next < xs.length) {
        const i = next++;
        out[i] = await f(xs[i]!);
      }
    }),
  );
  return out;
}

/** The old Cache Storage's entries with their sizes (x-ippai-size, else measured), not `skip`'s. */
async function listOld(old: Cache | null, skip: (k: Key) => boolean = () => false): Promise<Kept[]> {
  const keys = ((await old?.keys().catch(() => [])) ?? []).filter((k) => !skip(k));
  const sized = await batched(keys, LIST_AT_ONCE, async (k): Promise<Kept> => {
    const r = await old?.match(k.url).catch(() => undefined);
    const n = Number(r?.headers.get(SIZE));
    const size = n > 0 ? n : ((await r?.blob().catch(() => null))?.size ?? 0);
    return { url: k.url, size, time: 0 };
  });
  return sized.filter((k) => k.size > 0);
}

/** What `store` keeps with sizes and times (its list(), else its keys measured a few at a time). */
export function listChunks(store: ChunkStore): Promise<Kept[]> {
  return store.list ? store.list() : listOld(store as unknown as Cache);
}

/** Whether a store's put failed because the file is open elsewhere for a read (the network
 * worker's synchronous handle): then it is there already. */
const busy = (e: unknown) => e instanceof DOMException && e.name === "NoModificationAllowedError";

/** Empty files (writes cut short by a killed tab) older than this go at a listing. */
export const EMPTY_STALE_MS = 10 * 60 * 1000;
/** A URL, or what keys() gave. */
export type Key = string | URL | { url: string };

export const CHUNK_DIR = "ippai-chunks";
const CHECKED = "x-ippai-checked";
const SIZE = "x-ippai-size";

const urlOf = (r: Key) => (typeof r === "string" ? r : r instanceof URL ? r.href : r.url);
/** What Cache Storage takes of a key (none for this store's own keys()). */
const oldKey = (r: Key) => (/^https?:/.test(urlOf(r)) ? urlOf(r) : undefined);

/** A key's file name: its URL's last part ("" for none: then not kept here). */
export function chunkFileName(request: Key): string {
  const u = urlOf(request);
  // (chunks/<ab>/<hex>, its folder its first two digits; this store's own keys opfs:/<dir>/<name>)
  const m =
    /\/([0-9a-f]{2}|ippai-chunks)\/([0-9a-f]{32}(\?b=\d+&s=\d+)?)$/.exec(u) ??
    /^()([0-9a-f]{32}(\?b=\d+&s=\d+)?)$/.exec(u);
  return m && (m[1] === "" || m[1] === "ippai-chunks" || m[2]!.startsWith(m[1]!)) ? m[2]! : "";
}

const notFound = (e: unknown) =>
  e instanceof DOMException && (e.name === "NotFoundError" || e.name === "TypeMismatchError");

/** The store in folder `dir`, Cache Storage's `old` read too (and emptied as entries are kept again). */
export function opfsStore(dir: FileSystemDirectoryHandle, old: Cache | null): ChunkStore {
  return {
    async match(request) {
      const name = chunkFileName(request);
      if (name) {
        try {
          const file = await (await dir.getFileHandle(name)).getFile();
          if (file.size > 0) {
            const headers: Record<string, string> = { [SIZE]: String(file.size) };
            if (!name.includes("?")) headers[CHECKED] = "1";
            return new Response(file, { headers });
          }
        } catch (e) {
          if (!notFound(e)) throw e;
        }
      }
      const k = oldKey(request);
      return k ? ((await old?.match(k).catch(() => undefined)) ?? undefined) : undefined;
    },
    async put(request, response) {
      const name = chunkFileName(request);
      const k = oldKey(request);
      // (not a chunk's, or a whole chunk not checked: kept as before)
      if (!name || (!name.includes("?") && response.headers.get(CHECKED) !== "1")) {
        if (k) await old?.put(k, response);
        return;
      }
      // (the body as it comes, not read whole first and copied again)
      if (response.headers.get(SIZE) === "0" || !response.body) return;
      let w: FileSystemWritableFileStream;
      try {
        w = await (await dir.getFileHandle(name, { create: true })).createWritable();
      } catch (e) {
        // (open for a read in the network worker: kept there already, as checked as this)
        if (busy(e)) return;
        throw e;
      }
      try {
        // (QuotaExceededError goes to the caller: net.ts storeChunk, isFull)
        const r = response.body.getReader();
        let n = 0;
        for (;;) {
          const { done, value } = await r.read();
          if (done) break;
          n += value.length;
          await w.write(value);
        }
        if (n) await w.close();
        else await w.abort();
      } catch (e) {
        await w.abort().catch(() => undefined);
        throw e;
      }
      if (k) await old?.delete(k).catch(() => false);
    },
    async delete(request) {
      const name = chunkFileName(request);
      let gone = false;
      if (name)
        try {
          await dir.removeEntry(name);
          gone = true;
        } catch {
          // (gone already, or open in the network worker for a read: stays, trimmed another time)
        }
      const k = oldKey(request);
      const was = (k && (await old?.delete(k).catch(() => false))) || false;
      return gone || was;
    },
    async keys() {
      const out: { url: string }[] = [];
      const seen = new Set<string>();
      for await (const name of (dir as unknown as { keys(): AsyncIterable<string> }).keys()) {
        if (!chunkFileName(name)) continue;
        seen.add(name);
        out.push({ url: `opfs:/${CHUNK_DIR}/${name}` });
      }
      for (const k of (await old?.keys().catch(() => [])) ?? []) if (!seen.has(chunkFileName(k))) out.push(k);
      return out;
    },
    async list() {
      const handles: FileSystemFileHandle[] = [];
      for await (const h of (dir as unknown as { values(): AsyncIterable<FileSystemHandle> }).values())
        if (h.kind === "file" && chunkFileName(h.name)) handles.push(h as FileSystemFileHandle);
      const now = Date.now();
      const files = await batched(handles, LIST_AT_ONCE, async (h) => {
        try {
          const f = await h.getFile();
          // (an empty one: being written now, or a write a killed tab cut short; those go)
          if (!f.size && now - f.lastModified > EMPTY_STALE_MS)
            await dir.removeEntry(h.name).catch(() => undefined);
          return { url: `opfs:/${CHUNK_DIR}/${h.name}`, size: f.size, time: f.lastModified };
        } catch {
          return { url: "", size: 0, time: 0 };
        }
      });
      const out = files.filter((f) => f.size > 0);
      const seen = new Set(handles.map((h) => h.name));
      return out.concat(await listOld(old, (k) => seen.has(chunkFileName(k))));
    },
  };
}

let opened: Promise<FileSystemDirectoryHandle | null> | undefined;

// (Cache Storage's chunk cache opened once, for the CacheStorage it came from)
let oldOpened: { of: CacheStorage; p: Promise<Cache | null> } | undefined;

/** The device's chunk store: OPFS (with Cache Storage's old entries), else Cache Storage alone, else none. */
export async function deviceChunks(): Promise<ChunkStore | null> {
  let old: Cache | null = null;
  if (typeof caches !== "undefined") {
    if (oldOpened?.of !== caches)
      oldOpened = { of: caches, p: caches.open("ippai-chunks-v1").catch(() => null) };
    old = await oldOpened.p;
    if (!old) oldOpened = undefined;
  }
  // (the folder opened once; none: Cache Storage alone, asked again next time)
  const dir = await (opened ??= (async () => {
    try {
      return await (await navigator.storage.getDirectory()).getDirectoryHandle(CHUNK_DIR, { create: true });
    } catch {
      return null;
    }
  })());
  if (!dir) opened = undefined;
  return dir ? opfsStore(dir, old) : old;
}
