// A game saved to the device beforehand (on Wi-Fi, before a trip on a mobile line): every store
// chunk of its trees (and of the prefix: fonts, DLLs) goes into the chunk cache (net.ts CHUNKS),
// where the game's reads (host/fetch-bridge.js) and the page's read-ahead look first. sake
// itself, the game's saved x86 regions and lists are fetched with them (saveApp), and the
// service worker (sw.ts) keeps those and the app's pages: the game then starts with no connection.

import { coverUrl, type GameEntry } from "./launcher";
import { chunkCache, chunkPath, fetchRetry, getChunk, HttpError, treeChunks } from "./net";
import { fileVersion, jitBase, sakeBase, translatorVersion, versioned } from "./runtime";

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

// The games saved to the device (by id: their trees), in this browser: chunks one of them needs
// stay when another is removed, and when the cache is trimmed (net.ts trimChunks).
const SAVED = "ippai-offline";

function savedGames(): Record<string, string[]> {
  try {
    return JSON.parse(localStorage.getItem(SAVED) ?? "{}") as Record<string, string[]>;
  } catch {
    return {};
  }
}

function setSaved(id: string, trees: string[] | null): void {
  const all = savedGames();
  if (trees) all[id] = trees;
  else delete all[id];
  try {
    localStorage.setItem(SAVED, JSON.stringify(all));
  } catch {
    // (storage blocked: removing a game then keeps nothing for the others)
  }
}

async function chunksOfTrees(trees: string[]): Promise<{ hashes: string[]; chunk: number }> {
  const seen = new Set<string>();
  let chunk = 1 << 18;
  for (const t of trees) {
    const tsv = new TextDecoder().decode(
      await fetchRetry(new URL(`trees/${encodeURIComponent(t)}.tsv`, store())),
    );
    chunk = Number(tsv.slice(0, tsv.indexOf("\n")).split("\t")[2]) || chunk;
    for (const h of treeChunks(tsv)) seen.add(h);
  }
  return { hashes: [...seen], chunk };
}

const chunksOf = (g: GameEntry) => chunksOfTrees(gameTrees(g));

/** The chunks of the games saved to the device except `but` (their tree lists: sw.ts has them offline). */
export async function savedChunks(but?: string): Promise<Set<string>> {
  const keep = new Set<string>();
  for (const [id, trees] of Object.entries(savedGames()))
    if (id !== but) for (const h of (await chunksOfTrees(trees)).hashes) keep.add(h);
  return keep;
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

/**
 * Fetches what play.ts loads for `g` besides the chunks, at the URLs it uses: sake.js and
 * sake.wasm, the saved regions and the lists (sw.ts keeps them). Nothing when there is no
 * service worker (dev, or a browser without one): the browser's HTTP cache only.
 */
async function saveApp(g: GameEntry, signal: AbortSignal): Promise<void> {
  if (!navigator.serviceWorker?.controller) return;
  const base = sakeBase();
  const wasm = fileVersion(`${base}sake.wasm`);
  const get = (u: string) => fetchRetry(u, {}, { signal, giveUpMs: 60000 });
  // (no saved regions yet, and the like: not needed to start)
  const maybe = (u: string) =>
    get(u).catch((e: unknown) => {
      if (!(e instanceof HttpError)) throw e;
      return new Uint8Array();
    });
  const name = `${jitBase(g.run)}-${await translatorVersion(wasm)}`;
  const parts = new TextDecoder()
    .decode(await maybe(`jit/${name}.parts`))
    .split("\n")
    .filter(Boolean)
    .flatMap((l) => {
      const [module, idx, v] = l.split(" ");
      return [`jit/${module}?v=${v}`, `jit/${idx}?v=${v}`];
    });
  const cover = coverUrl(g);
  await Promise.all([
    get(versioned(`${base}sake.js`, await fileVersion(`${base}sake.js`))),
    get(versioned(`${base}sake.wasm`, await wasm)),
    maybe(`jit/${name}.chunks`),
    maybe("jit/_shared.chunks"),
    ...(cover ? [maybe(cover)] : []),
    ...parts.map(maybe),
  ]);
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
  setSaved(g.id, gameTrees(g));
  await saveApp(g, signal);
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

/**
 * Removes `g`'s chunks from the device, but not those another saved game needs (the prefix's
 * fonts and DLLs, files the games share): that one must still start offline.
 */
export async function dropOffline(g: GameEntry): Promise<void> {
  const cache = await chunkCache();
  if (!cache) return;
  const [{ hashes }, keep] = await Promise.all([chunksOf(g), savedChunks(g.id)]);
  setSaved(g.id, null);
  await Promise.all(
    hashes.filter((h) => !keep.has(h)).map((h) => cache.delete(new URL(chunkPath(h), store()).href)),
  );
}
