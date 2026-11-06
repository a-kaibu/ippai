// A game saved to the device beforehand (on Wi-Fi, before a trip on a mobile line): every store
// chunk of its trees (and of the prefix: fonts, DLLs) goes into the chunk cache (net.ts CHUNKS),
// where the game's reads (host/fetch-bridge.js) and the page's read-ahead look first. The list
// and the program itself still come from the server; only the game data stays here.

import { type GameEntry } from "./launcher";
import { chunkCache, chunkPath, fetchRetry, getChunk, treeChunks } from "./net";

const store = () => new URL("store/", location.href).href;

/** The trees `g` runs with: the prefix and its mounts (/path=tree). */
export function gameTrees(g: GameEntry): string[] {
  return [...new Set(["prefix32", ...g.mounts.map((m) => m.slice(m.indexOf("=") + 1))])].filter(Boolean);
}

export interface OfflineState {
  /** chunks of the game, and of them on the device */
  total: number;
  have: number;
  /** about the bytes of all (decoded) */
  bytes: number;
}

async function chunksOf(g: GameEntry): Promise<{ hashes: string[]; chunk: number }> {
  const seen = new Set<string>();
  let chunk = 1 << 18;
  for (const t of gameTrees(g)) {
    const tsv = new TextDecoder().decode(
      await fetchRetry(new URL(`trees/${encodeURIComponent(t)}.tsv`, store())),
    );
    chunk = Number(tsv.slice(0, tsv.indexOf("\n")).split("\t")[2]) || chunk;
    for (const h of treeChunks(tsv)) seen.add(h);
  }
  return { hashes: [...seen], chunk };
}

async function onDevice(): Promise<Set<string>> {
  const cache = await chunkCache();
  const keys = (await cache?.keys()) ?? [];
  return new Set(keys.map((r) => r.url.slice(r.url.lastIndexOf("/") + 1)));
}

/** How much of `g` is on the device (null: no chunk cache here). */
export async function offlineState(g: GameEntry): Promise<OfflineState | null> {
  if (!(await chunkCache())) return null;
  const [{ hashes, chunk }, have] = await Promise.all([chunksOf(g), onDevice()]);
  return {
    total: hashes.length,
    have: hashes.filter((h) => have.has(h)).length,
    bytes: hashes.length * chunk,
  };
}

/** Fetches `g`'s chunks the device lacks (4 at a time, retried as net.ts does); `progress(have, total)`. */
export async function saveOffline(
  g: GameEntry,
  progress: (have: number, total: number) => void,
  signal: AbortSignal,
): Promise<void> {
  void navigator.storage?.persist?.().catch(() => false);
  const cache = await chunkCache();
  if (!cache) throw new Error("この環境では端末に保存できません");
  const [{ hashes }, have] = await Promise.all([chunksOf(g), onDevice()]);
  const missing = hashes.filter((h) => !have.has(h));
  let done = hashes.length - missing.length;
  progress(done, hashes.length);
  let next = 0;
  const one = async () => {
    while (next < missing.length && !signal.aborted) {
      await getChunk(store(), missing[next++]!, { cache, priority: "low", signal });
      progress(++done, hashes.length);
    }
  };
  await Promise.all(Array.from({ length: 4 }, one));
}

/** Removes `g`'s chunks from the device (those other games share go too: they come back on use). */
export async function dropOffline(g: GameEntry): Promise<void> {
  const cache = await chunkCache();
  if (!cache) return;
  const { hashes } = await chunksOf(g);
  await Promise.all(hashes.map((h) => cache.delete(new URL(chunkPath(h), store()).href)));
}
