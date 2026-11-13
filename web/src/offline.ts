// A game saved to the device before it is played (ui/SaveFirst.tsx: when it is picked the first
// time; or from its 詳細, on Wi-Fi before a trip on a mobile line): every store
// chunk of its trees (and of the prefix: fonts, DLLs) goes into the chunk cache (net.ts CHUNKS),
// where the game's reads (host/fetch-bridge.js) and the page's read-ahead look first. sake
// itself, the game's saved x86 regions and lists are fetched with them (saveApp), and the
// service worker (sw.ts) keeps those and the app's pages: the game then starts with no connection.

import { coverUrl, type GameEntry } from "./launcher";
import { gameBusy } from "./budget";
import { directFetcher, limit, type Unzstd } from "./direct";
import {
  chunkCache,
  chunkPath,
  fetchRetry,
  getChunk,
  HttpError,
  keepChunk,
  treeChunks,
  treeChunkSizes,
} from "./net";
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

/** The file lists (trees/<name>.tsv) of `trees`. */
async function treeLists(trees: string[]): Promise<{ name: string; tsv: string }[]> {
  return Promise.all(
    trees.map(async (name) => ({
      name,
      tsv: new TextDecoder().decode(
        await fetchRetry(new URL(`trees/${encodeURIComponent(name)}.tsv`, store())),
      ),
    })),
  );
}

async function chunksOfTrees(
  trees: string[],
): Promise<{ hashes: string[]; chunk: number; sizes: Map<string, number> }> {
  const seen = new Set<string>();
  const sizes = new Map<string, number>();
  let chunk = 1 << 18;
  for (const { tsv } of await treeLists(trees)) {
    chunk = Number(tsv.slice(0, tsv.indexOf("\n")).split("\t")[2]) || chunk;
    for (const h of treeChunks(tsv)) seen.add(h);
    treeChunkSizes(tsv, sizes);
  }
  return { hashes: [...seen], chunk, sizes };
}

/** The bytes of `hashes` (their lengths from the trees' lists; `chunk` for one of no length). */
const bytesOf = (hashes: Iterable<string>, sizes: Map<string, number>, chunk: number) => {
  let n = 0;
  for (const h of hashes) n += sizes.get(h) ?? chunk;
  return n;
};

/** One part of a game on the device: a top folder of its tree, its files in it, its chunks' bytes, of them on the device. */
export interface PartInfo {
  name: string;
  files: number;
  bytes: number;
  have: number;
}

/** The prefix tree (sake's C:\: fonts, DLLs) as the parts list shows it. */
export const PREFIX_PART = "システム（フォントなど）";
/** A tree's files not in a folder. */
export const TOP_PART = "（フォルダの外のファイル）";

/**
 * What a game's trees hold, part by part (the prefix as one, each top folder of the others,
 * their files outside folders as one; another mounted tree's parts named "<tree>: <folder>"),
 * the biggest first: files, the bytes of their chunks (a chunk two parts share counted in the
 * first only, so the parts add up to the whole), and of them on the device (`on`).
 */
export function breakdown(trees: { name: string; tsv: string }[], on: Set<string>): PartInfo[] {
  const parts = new Map<string, PartInfo>();
  const seen = new Set<string>();
  const games = trees.filter((t) => t.name !== "prefix32");
  for (const { name, tsv } of trees) {
    for (const line of tsv.split("\n").slice(1)) {
      const f = line.split("\t");
      if (f[0] !== "f") continue;
      const path = f[3] ?? "";
      const slash = path.indexOf("/");
      const folder = slash > 0 ? path.slice(0, slash) : TOP_PART;
      const key = name === "prefix32" ? PREFIX_PART : games.length > 1 ? `${name}: ${folder}` : folder;
      let p = parts.get(key);
      if (!p) parts.set(key, (p = { name: key, files: 0, bytes: 0, have: 0 }));
      p.files++;
      for (const c of (f[5] ?? "").split(",")) {
        const [h, n] = c.split(":");
        if (!h || !/^[0-9a-f]{32}$/.test(h) || seen.has(h)) continue;
        seen.add(h);
        p.bytes += Number(n) || 0;
        if (on.has(h)) p.have += Number(n) || 0;
      }
    }
  }
  return [...parts.values()].sort((a, b) => b.bytes - a.bytes || a.name.localeCompare(b.name));
}

/** The first file (path) of each chunk in the trees' lists: what a chunk being saved belongs to. */
export function chunkFiles(trees: { name: string; tsv: string }[]): Map<string, string> {
  const out = new Map<string, string>();
  for (const { name, tsv } of trees)
    for (const line of tsv.split("\n").slice(1)) {
      const f = line.split("\t");
      if (f[0] !== "f" || !f[5]) continue;
      for (const c of f[5].split(",")) {
        const h = c.split(":")[0]!;
        if (/^[0-9a-f]{32}$/.test(h) && !out.has(h)) out.set(h, name === "prefix32" ? `C:\\${f[3]}` : f[3]!);
      }
    }
  return out;
}

/** `g`'s parts (breakdown) with what is on the device now (null: no chunk cache here). */
export async function offlineParts(g: GameEntry): Promise<PartInfo[] | null> {
  if (!(await chunkCache())) return null;
  const [lists, on] = await Promise.all([treeLists(gameTrees(g)), onDevice()]);
  return breakdown(lists, on);
}

/** A save to the device going on in this page: chunks and bytes done of all, the file of the
 * chunk fetched last, when it started and the bytes done then (for the rate). */
export interface Progress {
  have: number;
  total: number;
  haveBytes: number;
  totalBytes: number;
  now: string;
  started: number;
  startBytes: number;
}

const progress = new Map<string, Progress>();
const watchers = new Set<() => void>();
const tell = () => watchers.forEach((w) => w());

/** The saves going on in this page now, by game id. */
export function savesRunning(): ReadonlyMap<string, Progress> {
  return progress;
}

/** Calls `on` whenever a save's progress changes; returns the unsubscribe. */
export function watchSaves(on: () => void): () => void {
  watchers.add(on);
  return () => void watchers.delete(on);
}

/** The ids of the games saved to the device (finished or not) in this browser. */
export function savedIds(): string[] {
  return Object.keys(savedGames());
}

/** How much the origin may store and does (navigator.storage), and the chunk cache's bytes in all. */
export async function storageUse(): Promise<{
  quota?: number;
  usage?: number;
  persisted?: boolean;
  chunks: number;
  count: number;
} | null> {
  const cache = await chunkCache();
  if (!cache) return null;
  const est = await navigator.storage?.estimate?.().catch(() => undefined);
  const persisted = await navigator.storage?.persisted?.().catch(() => undefined);
  let chunks = 0;
  const keys = await cache.keys();
  for (const k of keys) {
    const r = await cache.match(k).catch(() => undefined);
    const n = Number(r?.headers.get("x-ippai-size"));
    chunks += n > 0 ? n : ((await r?.blob().catch(() => null))?.size ?? 0);
  }
  return { quota: est?.quota, usage: est?.usage, persisted, chunks, count: keys.length };
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
  const [{ hashes, chunk, sizes }, on] = await Promise.all([chunksOf(g), onDevice()]);
  const have = hashes.filter((h) => on.has(h)).length;
  return {
    total: hashes.length,
    have,
    bytes: bytesOf(hashes, sizes, chunk),
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

/** Chunks fetched at once from the bucket itself (direct.ts: ~2 s each whatever the size). */
export const DIRECT_AT_ONCE = 16;
/** Chunks fetched at once through the server (/store: a store without S3, a chunk only there). */
export const SERVER_AT_ONCE = 4;

/** Of DIRECT_AT_ONCE, how many go on while a game playing waits for a read of its own. */
export const SAVE_BUSY_WIDTH = 2;

/** `hashes` with those of `order` first, in its order (the rest as they were). */
export function byOrder(hashes: string[], order: string[]): string[] {
  const want = new Set(hashes);
  const first = [...new Set(order.filter((h) => want.has(h)))];
  const seen = new Set(first);
  return [...first, ...hashes.filter((h) => !seen.has(h))];
}

/** The chunks earlier runs of game `id` read first, in order (jit/game.<id>.chunks: 16 bytes
 * each; none yet: empty). */
async function readOrder(id: string, request?: typeof fetch): Promise<string[]> {
  const b = await fetchRetry(
    new URL(`jit/${gameChunksName(id)}.chunks`, location.href).href,
    {},
    { giveUpMs: 20000, request },
  ).catch(() => new Uint8Array(0));
  const out: string[] = [];
  for (let i = 0; i + 16 <= b.length; i += 16)
    out.push(Array.from(b.subarray(i, i + 16), (x) => x.toString(16).padStart(2, "0")).join(""));
  return out;
}

/** `f()` holding chunk `hex`'s Web Lock (the network worker's fetches ahead hold it too), or just `f()`. */
/** Chunks a save asks the server about at once (direct.ts prime), two windows ahead. */
export const PRIME_WINDOW = 256;

function withChunkLock(hex: string, f: () => Promise<void>): Promise<void> {
  const locks = (navigator as { locks?: LockManager }).locks;
  return locks ? locks.request(`ippai-chunk:${hex}`, f) : f();
}

/** What saveOffline may be given in place of the real ones (tests). */
export interface SaveOptions {
  request?: typeof fetch;
  unzstd?: Unzstd;
}

/**
 * Whether `free` bytes hold `need` more (with a margin: the browser counts what it keeps a
 * little above the bytes). undefined: the browser does not say.
 */
export function roomFor(need: number, quota?: number, usage?: number): boolean {
  if (!quota) return true;
  return need * 1.05 <= quota * 0.9 - (usage ?? 0);
}

/** The free room a save needs, said as people read it. */
export class NoRoom extends Error {
  constructor(
    readonly need: number,
    readonly free: number,
  ) {
    super(
      `端末の空き容量が足りません（あと約 ${Math.ceil(need / 2 ** 20)} MB 必要、空き約 ${Math.floor(Math.max(0, free) / 2 ** 20)} MB）`,
    );
  }
}

/** Fetches `g`'s chunks the device lacks (from the bucket DIRECT_AT_ONCE at a time, else through
 * the server SERVER_AT_ONCE at a time, retried as net.ts does); `report(have, total)`, and the
 * whole progress in savesRunning (watchSaves). Throws NoRoom first when the device has no room for them. */
export async function saveOffline(
  g: GameEntry,
  report: (have: number, total: number) => void,
  signal: AbortSignal,
  opt: SaveOptions = {},
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
    const [lists, have] = await Promise.all([treeLists(trees), onDevice()]);
    const sizes = new Map<string, number>();
    const all = new Set<string>();
    let chunk = 1 << 18;
    for (const { tsv } of lists) {
      chunk = Number(tsv.slice(0, tsv.indexOf("\n")).split("\t")[2]) || chunk;
      treeChunkSizes(tsv, sizes);
      for (const h of treeChunks(tsv)) all.add(h);
    }
    const hashes = [...all];
    const files = chunkFiles(lists);
    // (what earlier runs of the game read first, first: a game started while it is being saved
    // finds its start on the device sooner)
    const order = await readOrder(g.id, opt.request);
    const missing = byOrder(
      hashes.filter((h) => !have.has(h)),
      order,
    );
    let done = hashes.length - missing.length;
    const p: Progress = {
      have: done,
      total: hashes.length,
      haveBytes: bytesOf(
        hashes.filter((h) => have.has(h)),
        sizes,
        chunk,
      ),
      totalBytes: bytesOf(hashes, sizes, chunk),
      now: "",
      started: Date.now(),
      startBytes: 0,
    };
    p.startBytes = p.haveBytes;
    const est = await navigator.storage?.estimate?.().catch(() => undefined);
    if (!roomFor(p.totalBytes - p.haveBytes, est?.quota, est?.usage))
      throw new NoRoom(p.totalBytes - p.haveBytes, (est?.quota ?? 0) * 0.9 - (est?.usage ?? 0));
    progress.set(g.id, p);
    tell();
    report(done, hashes.length);
    const base = store();
    const direct = directFetcher(base, opt);
    const server = limit(SERVER_AT_ONCE);
    let next = 0;
    const primed = new Map<number, Promise<void>>();
    const one = async (i: number) => {
      while (next < missing.length && !signal.aborted) {
        // (a game playing meanwhile waits for a read of its own: SAVE_BUSY_WIDTH go on)
        while (i >= SAVE_BUSY_WIDTH && gameBusy() && !signal.aborted)
          await new Promise((r) => setTimeout(r, 50));
        if (next >= missing.length || signal.aborted) break;
        const k = next++;
        const h = missing[k]!;
        // (how to fetch the next ones from the bucket, asked for at once a window ahead; every
        // fetcher waits for its window's answer, not only the one that asked)
        const w = Math.floor(k / PRIME_WINDOW);
        if (!primed.has(w))
          primed.set(
            w,
            direct
              .prime(missing.slice(w * PRIME_WINDOW, (w + 2) * PRIME_WINDOW), signal)
              .catch(() => undefined),
          );
        await primed.get(w);
        p.now = files.get(h) ?? "";
        tell();
        const url = new URL(chunkPath(h), base).href;
        // (one fetch of a chunk the game's network worker fetches too: its Web Lock, host/fetch-bridge.js ippaiWhole)
        await withChunkLock(h, async () => {
          if (await cache.match(url).catch(() => undefined)) return;
          const data = await direct.get(h, { signal, request: opt.request });
          if (data) await keepChunk(cache, url, data);
          else
            await server(() => getChunk(base, h, { cache, priority: "low", signal, request: opt.request }));
        });
        p.have = ++done;
        p.haveBytes += sizes.get(h) ?? chunk;
        tell();
        report(done, hashes.length);
      }
    };
    await Promise.all(Array.from({ length: DIRECT_AT_ONCE }, (_, i) => one(i)));
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
    progress.delete(g.id);
    tell();
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

/** Whether `id` was saved to the device to the end (as this browser remembers: read at once,
 * in the click that starts a game; chunks the browser dropped since are fetched as it plays). */
export function savedDone(id: string): boolean {
  return savedGames()[id]?.status === "done";
}

// the saves this page started (stopSave stops them), and how each will end
const saves = new Map<string, { stop: AbortController; done: Promise<void> }>();

/**
 * Starts saving `g` to the device, or gives the save of it going on now: `done` settles when it
 * ends (rejected when it failed or was stopped).
 */
export function startSave(
  g: GameEntry,
  opt: SaveOptions = {},
): { stop: AbortController; done: Promise<void> } {
  const on = saves.get(g.id);
  if (on) return on;
  const stop = new AbortController();
  const done = saveOffline(g, () => {}, stop.signal, opt).finally(() => saves.delete(g.id));
  // (the callers' own handlers report it)
  done.catch(() => {});
  const s = { stop, done };
  saves.set(g.id, s);
  return s;
}

/** The save of `id` this page started, going on now. */
export const saveOf = (id: string) => saves.get(id);
