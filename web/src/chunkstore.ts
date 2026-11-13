// Where store chunks are kept on the device: the origin private file system (OPFS), one file a
// chunk (or a block of one) in the folder CHUNK_DIR, named as the last part of its URL (its hex
// name; a block's "<hex>?b=<k>&s=<block>"). Cache Storage before (net.ts CHUNKS) is still read
// and its entries go when they are put or deleted again, so a game saved to the device before
// stays saved. The network worker has its own copy (host/fetch-bridge.js ippaiOpfsStore), which
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
}
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
      const data = new Uint8Array(await response.arrayBuffer());
      if (!data.length) return;
      const w = await (await dir.getFileHandle(name, { create: true })).createWritable();
      try {
        await w.write(data);
        await w.close();
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
  };
}

let opened: Promise<FileSystemDirectoryHandle | null> | undefined;

/** The device's chunk store: OPFS (with Cache Storage's old entries), else Cache Storage alone, else none. */
export async function deviceChunks(): Promise<ChunkStore | null> {
  const old = typeof caches === "undefined" ? null : await caches.open("ippai-chunks-v1").catch(() => null);
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
