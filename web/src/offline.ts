// A game saved to the device beforehand (on Wi-Fi, before a trip on a mobile line): every store
// chunk of its trees (and of the prefix: fonts, DLLs) goes into the chunk cache (net.ts CHUNKS),
// where the game's reads (host/fetch-bridge.js) and the page's read-ahead look first. sake
// itself, the game's saved x86 regions and lists are fetched with them (saveApp), and the
// service worker (sw.ts) keeps those and the app's pages: the game then starts with no connection.

import { coverUrl, type GameEntry } from "./launcher";
import { chunkCache, chunkPath, fetchRetry, getChunk, HttpError, treeChunks } from "./net";
import {
  fileVersion,
  gameChunksName,
  jitBase,
  lastSakeBuild,
  sakeBase,
  translatorVersion,
  versioned,
} from "./runtime";

const store = () => new URL("store/", location.href).href;

/** The trees `g` runs with: the prefix and its mounts (/path=tree). */
export function gameTrees(g: GameEntry): string[] {
  return [...new Set(["prefix32", ...g.mounts.map((m) => m.slice(m.indexOf("=") + 1))])].filter(Boolean);
}

/**
 * Where saving a game to the device got to: "saving" (started and not finished: on a page closed
 * meanwhile it stays so, and reads as "stopped"), "done" (the app's files and every chunk
 * fetched), "stopped" (中止), "failed" (an error). "none": never saved (chunks the game read
 * while playing may still be here).
 */
export type SaveStatus = "none" | "saving" | "done" | "stopped" | "failed";

export interface OfflineState {
  /** chunks of the game, and of them on the device */
  total: number;
  have: number;
  /** about the bytes of all (decoded) */
  bytes: number;
  status: SaveStatus;
  /** whether the game starts with no connection: saved to the end, and every chunk still here */
  complete: boolean;
}

// The games saved to the device (by id: their trees and how far the save got), in this browser:
// chunks one of them needs stay when another is removed, and when the cache is trimmed (net.ts
// trimChunks), a save stopped half-way too (until removed).
const SAVED = "ippai-offline";

export interface Saved {
  trees: string[];
  status: Exclude<SaveStatus, "none">;
}

function savedGames(): Record<string, Saved> {
  try {
    const all = JSON.parse(localStorage.getItem(SAVED) ?? "{}") as Record<string, Saved | string[]>;
    // (before the status was kept: a list of trees, the save then finished or not; the chunks
    // tell, see stateOf)
    return Object.fromEntries(
      Object.entries(all).map(([id, v]) => [id, Array.isArray(v) ? { trees: v, status: "done" } : v]),
    );
  } catch {
    return {};
  }
}

function setSaved(id: string, saved: Saved | null): void {
  const all = savedGames();
  if (saved) all[id] = saved;
  else delete all[id];
  try {
    localStorage.setItem(SAVED, JSON.stringify(all));
  } catch {
    // (storage blocked: removing a game then keeps nothing for the others)
  }
}

/**
 * What `saved` (the game's record, if any) and the chunks on the device (`have` of `total`) say.
 * A save cut short with the page closed ("saving" not running here) is a stopped one; a finished
 * one whose chunks were since removed (the browser's storage cleared in part) is not complete.
 */
export function stateOf(
  saved: Saved | undefined,
  running: boolean,
  have: number,
  total: number,
): Pick<OfflineState, "status" | "complete"> {
  const status = !saved ? "none" : saved.status === "saving" && !running ? "stopped" : saved.status;
  return { status, complete: status === "done" && have >= total };
}

// the games being saved by this page now
const running = new Set<string>();

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
  for (const [id, { trees }] of Object.entries(savedGames()))
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
  const [{ hashes, chunk }, on] = await Promise.all([chunksOf(g), onDevice()]);
  const have = hashes.filter((h) => on.has(h)).length;
  return {
    total: hashes.length,
    have,
    bytes: hashes.length * chunk,
    ...stateOf(savedGames()[g.id], running.has(g.id), have, hashes.length),
  };
}

/**
 * Fetches what play.ts loads for `g` besides the chunks, at the URLs it uses: sake.js and
 * sake.wasm (or the -grow ones this device ran last, runtime.ts sakeBuild), the saved regions and the lists (sw.ts keeps them). Nothing when there is no
 * service worker (dev, or a browser without one): the browser's HTTP cache only.
 */
async function saveApp(g: GameEntry, signal: AbortSignal): Promise<void> {
  if (!navigator.serviceWorker?.controller) return;
  const base = sakeBase();
  const build = lastSakeBuild();
  const wasm = fileVersion(`${base}${build.name}.wasm`);
  const get = (u: string) => fetchRetry(u, {}, { signal, giveUpMs: 60000 });
  // (no saved regions yet, and the like: not needed to start)
  const maybe = (u: string) =>
    get(u).catch((e: unknown) => {
      if (!(e instanceof HttpError)) throw e;
      return new Uint8Array();
    });
  const name = `${jitBase(g.run)}-${await translatorVersion(wasm)}${build.jit}`;
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
    get(versioned(`${base}${build.name}.js`, await fileVersion(`${base}${build.name}.js`))),
    get(versioned(`${base}${build.name}.wasm`, await wasm)),
    maybe(`jit/${gameChunksName(g.id)}.chunks`),
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
  const trees = gameTrees(g);
  // (first: its chunks are kept from here on, and a page closed meanwhile shows a stopped save)
  setSaved(g.id, { trees, status: "saving" });
  running.add(g.id);
  try {
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
    if (signal.aborted) throw signal.reason;
    // done only when every chunk is on the device now (the browser may have dropped some)
    const now = await onDevice();
    const lost = hashes.filter((h) => !now.has(h)).length;
    if (lost) throw new Error(`${lost} 個のチャンクが端末に残りませんでした（空き容量を確認してください）`);
    setSaved(g.id, { trees, status: "done" });
  } catch (e) {
    // (unless removed meanwhile)
    if (savedGames()[g.id]) setSaved(g.id, { trees, status: signal.aborted ? "stopped" : "failed" });
    throw e;
  } finally {
    running.delete(g.id);
  }
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
