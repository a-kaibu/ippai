// Save states ("save anywhere", docs/savestate.md): the whole running program at a moment, kept
// in slots per game, back where it was in a later page. The module stops every thread where
// it can go on from (crates/kura/src/snap.rs, crates/sake/src/snap.rs), the drawing thread reads
// the GPU into memory, and the page copies the memory: its blocks compressed with zstd in a
// worker (snapz-worker.ts). A state starts a new page (`&state=<slot>`): the memory filled
// before the module runs, which then starts its threads where they stood.
//
// A state is kept in this browser (OPFS: states/<game>/<hash>.snap and index.json) and on the
// server (ippai serve /saves/state.<game>.<hash>, its S3 bucket), so another device goes on
// from it too. The file is host/snap.js's: "IPPAISNP", u32 version, u32 header length, the
// header (JSON), then blocks (u32 address, u32 length, u32 compressed length, a zstd frame),
// ended by the address 0xffffffff.

import type { Reply, Request } from "./snapz-worker";
import { memGate } from "./memgate";
import { sakeBase, type SakeBuild } from "./runtime";
import { bgraToRgba } from "./screen-stats";

type SakeName = SakeBuild["name"];

export interface SnapModule {
  wasmMemory: WebAssembly.Memory;
  _malloc(size: number): number;
  _free(p: number): void;
  _vnhost_snap_ctl(): number;
  _vnhost_render_shot(out: number): number;
}

/** A slot's state, as the index lists it. */
export interface StateInfo {
  slot: number;
  /** ISO time it was taken */
  created: string;
  /** bytes of the file */
  size: number;
  /** the build of sake it is of (build-id.txt) */
  build: string;
  /** which of the page's two (runtime.ts sakeBuild; "sake" when not said) */
  wasm?: SakeName;
  /** what the screen showed (a JPEG data URL), "" without */
  thumb: string;
  /** the file's name by its content (stateHash); none in states from before */
  hash?: string;
}

export const SLOTS = 4;
const MAGIC = "IPPAISNP";
const BLOCK = 1 << 20;
// Ctl's offsets (crates/kura/src/snap.rs)
const CMD = 8,
  STATE = 12,
  RESTORING = 24,
  RANGES = 28,
  NRANGES = 32,
  MSG = 36,
  MSG_LEN = 40,
  STIRRED = 88;
const ST_RUNNING = 0,
  ST_READY = 2,
  ST_FAILED = 3;

/** A state is being taken: nothing of the page may change the module's memory (memgate.ts). */
export const snapping = () => memGate.closed;

let worker: Worker | undefined;
let nextId = 1;
const waiting = new Map<number, { resolve: (r: Reply) => void; reject: (e: Error) => void }>();
/** How long a request to the worker may take: one that never answers (the worker died without
 * an error event, an OOM) left the game stopped and the gate shut for good. */
const WORK_MS = 120_000;

type Body<T> = T extends unknown ? Omit<T, "id"> : never;

/** A request to the zstd worker (snapz-worker.ts), its answer matched by id (two at once were
 * each answered by the last one's handler). */
function work(r: Body<Request>, transfer: Transferable[] = [], ms = WORK_MS): Promise<Reply> {
  if (!worker) {
    const w = (worker = new Worker(new URL("./snapz-worker.ts", import.meta.url), { type: "module" }));
    w.onmessage = (e: MessageEvent<Reply>) => {
      const p = waiting.get(e.data.id);
      waiting.delete(e.data.id);
      if (!p) return;
      if (e.data.ok) p.resolve(e.data);
      else p.reject(new Error(e.data.error));
    };
    w.onerror = (e) => fail(w, new Error(e.message));
  }
  const id = nextId++;
  const w = worker;
  return new Promise<Reply>((resolve, reject) => {
    const timer = setTimeout(() => fail(w, new Error("圧縮の処理が応答しません")), ms);
    waiting.set(id, {
      resolve: (x) => (clearTimeout(timer), resolve(x)),
      reject: (e) => (clearTimeout(timer), reject(e)),
    });
    w.postMessage({ ...r, id }, transfer);
  });
}
/** The worker is given up: every request waiting fails, the next one starts a new worker. */
function fail(w: Worker, e: Error): void {
  if (worker !== w) return;
  worker = undefined;
  w.terminate();
  for (const p of waiting.values()) p.reject(e);
  waiting.clear();
}
const snapzUrl = () => `${sakeBase()}snapz.mjs`;

let buildIds: Promise<Map<string, string>> | undefined;
/** The ids of this page's sake builds, by name (crates/sake/build.sh writes build-id.txt:
 * "<name> <id>" lines). */
export function builds(): Promise<Map<string, string>> {
  buildIds ??= fetch(`${sakeBase()}build-id.txt`, { cache: "no-cache" }).then(
    async (r) =>
      new Map(
        r.ok
          ? (await r.text())
              .split("\n")
              .map((l) => l.trim().split(/\s+/))
              .filter((w) => w.length === 2)
              .map((w) => [w[0]!, w[1]!] as [string, string])
          : [],
      ),
    () => new Map(),
  );
  return buildIds;
}
/** Whether this page can load a state of `s` (its build is one of the page's). */
export const loadable = (ids: Map<string, string>, s: { build: string; wasm?: string }) =>
  ids.size === 0 || ids.get(s.wasm ?? "sake") === s.build;

/** What the page shows now, small (a JPEG data URL; "" when there is none). */
async function thumbnail(m: SnapModule): Promise<string> {
  try {
    const p = m._malloc(12) >>> 0;
    if (!p) return "";
    const first = m._vnhost_render_shot(p);
    for (let k = 0; k < 25 && m._vnhost_render_shot(p) === first; k++)
      await new Promise((r) => setTimeout(r, 20));
    const [ptr, w, h] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3)) as [number, number, number];
    m._free(p);
    if (!ptr || !w || !h) return "";
    // (BGRA rows)
    const px = bgraToRgba(new Uint8Array(m.wasmMemory.buffer, ptr, w * h * 4));
    const full = new OffscreenCanvas(w, h);
    full.getContext("2d")!.putImageData(new ImageData(px, w, h), 0, 0);
    const tw = 240,
      th = Math.max(1, Math.round((h * tw) / w));
    const small = new OffscreenCanvas(tw, th);
    small.getContext("2d")!.drawImage(full, 0, 0, tw, th);
    const blob = await small.convertToBlob({ type: "image/jpeg", quality: 0.7 });
    return await new Promise<string>((resolve) => {
      const r = new FileReader();
      r.onload = () => resolve(typeof r.result === "string" ? r.result : "");
      r.onerror = () => resolve("");
      r.readAsDataURL(blob);
    });
  } catch {
    return "";
  }
}

/** A thread of the module ran while the page copied: the state is not kept (taking it again
 * is fine). */
export class Stirred extends Error {
  constructor() {
    super("セーブ中に動いたスレッドがありました");
  }
}

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Takes a state of the running module: the file and its index entry (slot set by the caller). */
export async function takeState(
  m: SnapModule,
  wasm: SakeName,
): Promise<{ file: Uint8Array<ArrayBuffer>; info: Omit<StateInfo, "slot"> }> {
  const thumb = await thumbnail(m);
  const id = (await builds()).get(wasm) ?? "";
  // (the worker and its zstd ready before the program is stopped: a slow fetch of snapz.mjs used
  // to happen with every guest thread parked)
  await work({ op: "warm", url: snapzUrl() });
  const ctl = m._vnhost_snap_ctl() >>> 0;
  const i32 = new Int32Array(m.wasmMemory.buffer);
  const word = (off: number) => (ctl + off) >>> 2;
  // (the page's own writes first: WebCodecs' copies under way end, later ones wait)
  await memGate.close();
  let parts: { a: number; n: number; frame: Uint8Array }[];
  let raw: number;
  const t0 = performance.now();
  try {
    Atomics.store(i32, word(CMD), 1);
    Atomics.notify(i32, word(CMD));
    let st;
    while ((st = Atomics.load(i32, word(STATE))) !== ST_READY && st !== ST_FAILED) {
      if (performance.now() - t0 > 30000) throw new Error("止まりませんでした");
      await wait(5);
    }
    if (st === ST_FAILED) {
      const p = i32[word(MSG)]! >>> 0,
        n = i32[word(MSG_LEN)]!;
      throw new Error(new TextDecoder().decode(new Uint8Array(m.wasmMemory.buffer, p, n).slice()));
    }
    const rp = i32[word(RANGES)]! >>> 0,
      rn = i32[word(NRANGES)]!;
    const ranges = Array.from(new Uint32Array(m.wasmMemory.buffer, rp, rn * 2));
    const r = await work({
      op: "pack",
      url: snapzUrl(),
      memory: m.wasmMemory.buffer as unknown as SharedArrayBuffer,
      ranges,
      block: BLOCK,
    }).finally(() => {
      // the program goes on (also when packing failed)
      Atomics.store(i32, word(CMD), 2);
      Atomics.notify(i32, word(CMD));
    });
    if (!("parts" in r)) throw new Error("packing");
    ({ parts, raw } = r);
    // (a thread that ran while copied may have left a lock held in the copy: not kept)
    const t1 = performance.now();
    while (Atomics.load(i32, word(STATE)) !== ST_RUNNING && performance.now() - t1 < 5000) await wait(2);
    if (Atomics.load(i32, word(STIRRED))) throw new Stirred();
  } finally {
    memGate.open();
  }
  const took = Math.round(performance.now() - t0);
  const created = new Date().toISOString();
  // (pages: sake-grow's memory may have grown; a state needs as much)
  const pages = m.wasmMemory.buffer.byteLength / 65536;
  const head = new TextEncoder().encode(
    JSON.stringify({ format: 1, build: id, wasm, pages, ctl, raw, created, took }),
  );
  let size = 16 + head.length + 4;
  for (const p of parts) size += 12 + p.frame.length;
  const file = new Uint8Array(size);
  const v = new DataView(file.buffer);
  for (let i = 0; i < 8; i++) file[i] = MAGIC.charCodeAt(i);
  v.setUint32(8, 1, true);
  v.setUint32(12, head.length, true);
  file.set(head, 16);
  let at = 16 + head.length;
  for (const p of parts) {
    v.setUint32(at, p.a, true);
    v.setUint32(at + 4, p.n, true);
    v.setUint32(at + 8, p.frame.length, true);
    file.set(p.frame, at + 12);
    at += 12 + p.frame.length;
    // (each frame let go once copied: the state is not held twice over while the file fills)
    p.frame = EMPTY;
  }
  v.setUint32(at, 0xffffffff, true);
  console.log(
    `state: ${(raw / 1048576).toFixed(1)} MiB in ${(size / 1048576).toFixed(1)} MiB, the program stood still ${took} ms`,
  );
  return { file, info: { created, size, build: id, wasm, thumb } };
}

/** The header of a state file (throws when it is none). */
export function stateHeader(file: Uint8Array): {
  build: string;
  wasm: SakeName;
  pages: number;
  ctl: number;
  at: number;
} {
  const v = new DataView(file.buffer, file.byteOffset, file.byteLength);
  if (String.fromCharCode(...file.subarray(0, 8)) !== MAGIC || v.getUint32(8, true) !== 1)
    throw new Error("ステートのファイルではありません");
  const hl = v.getUint32(12, true);
  const head = JSON.parse(new TextDecoder().decode(file.subarray(16, 16 + hl))) as {
    build: string;
    wasm?: SakeName;
    pages?: number;
    ctl: number;
  };
  return {
    build: head.build,
    wasm: head.wasm ?? "sake",
    pages: head.pages ?? 65535,
    ctl: head.ctl,
    at: 16 + hl,
  };
}

const EMPTY = new Uint8Array(0);

/** A memory with the state in it, for the module to start from (its `wasmMemory`): `fixed`,
 * the 4 GB one the page reserved for sake.wasm, or a new one for sake-grow.wasm. `own`: the
 * caller is done with `file`, whose buffer then goes to the worker (detached here), not a copy. */
export async function stateMemory(
  file: Uint8Array<ArrayBuffer>,
  fixed?: WebAssembly.Memory,
  own = false,
): Promise<WebAssembly.Memory> {
  const h = stateHeader(file);
  if (!loadable(await builds(), h))
    throw new Error("ippai の更新前のステートです（このバージョンでは読み込めません）");
  // (sake's memory: crates/sake/build.sh FIXED / GROW, as large as it was)
  const mem =
    h.wasm === "sake" ? fixed : new WebAssembly.Memory({ initial: h.pages, maximum: 65535, shared: true });
  if (!mem) throw new Error("この端末ではこのステートを読み込めません（4 GB のメモリを確保できません）");
  const whole = file.byteOffset === 0 && file.byteLength === file.buffer.byteLength;
  const copy = whole && own ? file.buffer : whole ? file.buffer.slice(0) : file.slice().buffer;
  await work(
    {
      op: "unpack",
      url: snapzUrl(),
      memory: mem.buffer as unknown as SharedArrayBuffer,
      file: copy,
      at: h.at,
    },
    [copy],
  );
  new Uint32Array(mem.buffer)[(h.ctl + RESTORING) >>> 2] = 1;
  return mem;
}

// ---- older builds' states: their web apps, kept by the server (crates/ippai/src/apps.rs) ----

/** The server's root (this page may be a kept app's, at apps/<key>/). */
export const appRoot = () => new URL(".", location.href).href.replace(/apps\/[0-9a-f]{16}\/$/, "");

/** The sake builds whose web apps the server keeps: build id -> the app's URL. */
export async function keptApps(): Promise<Map<string, string>> {
  try {
    const r = await fetch(new URL("apps/builds", appRoot()).href, { cache: "no-store" });
    if (!r.ok) return new Map();
    const v = (await r.json()) as Record<string, string>;
    return new Map(Object.entries(v).map(([b, k]) => [b, new URL(`apps/${k}/`, appRoot()).href]));
  } catch {
    return new Map();
  }
}

// ---- slots: this browser's (OPFS) and the server's ----
//
// A state's file is named by its content (`hash`, SHA-256): `states/<game>/<hash>.snap` here,
// `state.<game>.<hash>` on the server, written once and never over. The index (index.json,
// `state.<game>.index`) lists the slots and the hash each holds; it is the only thing that
// changes, with If-Match, merged again with the server's when another device wrote it since
// (the newer entry of each slot wins). So a slot saved from two devices at once never leaves an
// index pointing at a file that was replaced: each file stays as it was. States from before
// (no hash) are still read from their old names (`<slot>.snap`, `state.<game>.<slot>`).

const serverUrl = (name: string) => new URL(`saves/${encodeURIComponent(name)}`, location.href).href;
const oldFileName = (game: string, slot: number) => `state.${game}.${slot}`;
const fileName = (game: string, hash: string) => `state.${game}.${hash}`;
const indexName = (game: string) => `state.${game}.index`;
const localName = (info: Pick<StateInfo, "slot" | "hash">) =>
  info.hash ? `${info.hash}.snap` : `${info.slot}.snap`;

/** A state file's name by its content (hex, 128 bits of SHA-256). */
export async function stateHash(file: Uint8Array<ArrayBuffer>): Promise<string> {
  const d = new Uint8Array(await crypto.subtle.digest("SHA-256", file));
  return Array.from(d.subarray(0, 16), (b) => b.toString(16).padStart(2, "0")).join("");
}

async function dir(game: string, create: boolean): Promise<FileSystemDirectoryHandle | null> {
  try {
    const root = await navigator.storage.getDirectory();
    const states = await root.getDirectoryHandle("states", { create });
    return await states.getDirectoryHandle(game, { create });
  } catch {
    return null;
  }
}
async function readLocal(game: string, name: string): Promise<Uint8Array<ArrayBuffer> | null> {
  try {
    const f = await (await dir(game, false))?.getFileHandle(name);
    return f ? new Uint8Array(await (await f.getFile()).arrayBuffer()) : null;
  } catch {
    return null;
  }
}
async function writeLocal(game: string, name: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
  const d = await dir(game, true);
  if (!d) throw new Error("この端末に保存できません");
  const w = await (await d.getFileHandle(name, { create: true })).createWritable();
  await w.write(data);
  await w.close();
}
/** Removes this browser's state files no entry of `index` names. */
async function pruneLocal(game: string, index: StateInfo[]): Promise<void> {
  const d = await dir(game, false);
  if (!d) return;
  const keep = new Set(index.map(localName));
  const gone: string[] = [];
  for await (const name of (d as unknown as { keys(): AsyncIterable<string> }).keys())
    if (name.endsWith(".snap") && !keep.has(name)) gone.push(name);
  for (const name of gone) await d.removeEntry(name).catch(() => {});
}

/** A request's time limit: STATE_WAIT_MS, and a second per SLOW_LINK bytes of an upload (a
 * stalled link held the states dialog and the start of a game on a state as long as the socket). */
const STATE_WAIT_MS = 30_000;
const SLOW_LINK = 64 * 1024;
const limit = (bytes = 0) => AbortSignal.timeout(STATE_WAIT_MS + Math.ceil((bytes / SLOW_LINK) * 1000));

async function serverGet(name: string): Promise<{ data: Uint8Array<ArrayBuffer> | null; revision: string }> {
  // (a download is given as long as its body takes, at least a slow link's pace: the limit is
  // on the answer coming, then on each 64 KiB)
  const r = await fetch(serverUrl(name), { cache: "no-store", signal: limit() });
  if (r.status === 404) return { data: null, revision: '"0"' };
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return { data: new Uint8Array(await r.arrayBuffer()), revision: r.headers.get("etag") ?? '"0"' };
}
/** Writes `data` as `name` if the server's revision is still `revision`: false when it moved. */
async function serverPut(name: string, data: Uint8Array<ArrayBuffer>, revision: string): Promise<boolean> {
  const r = await fetch(serverUrl(name), {
    method: "PUT",
    headers: { "if-match": revision },
    body: data,
    signal: limit(data.length),
  });
  if (r.status === 412) return false;
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return true;
}

const parseIndex = (b: Uint8Array | null): StateInfo[] => {
  try {
    return b ? (JSON.parse(new TextDecoder().decode(b)) as { states: StateInfo[] }).states : [];
  } catch {
    return [];
  }
};
const encodeIndex = (v: StateInfo[]) => new TextEncoder().encode(JSON.stringify({ states: v }));
/** The newer of each slot's two entries. */
export function merge(a: StateInfo[], b: StateInfo[]): StateInfo[] {
  const m = new Map<number, StateInfo>();
  for (const s of [...a, ...b]) if (!m.has(s.slot) || m.get(s.slot)!.created < s.created) m.set(s.slot, s);
  return [...m.values()].sort((x, y) => x.slot - y.slot);
}

/** `game`'s states (this browser's and the server's, the newer of each slot). */
export async function listStates(game: string): Promise<StateInfo[]> {
  const local = parseIndex(await readLocal(game, "index.json"));
  const remote = await serverGet(indexName(game)).then(
    (r) => parseIndex(r.data),
    () => [],
  );
  return merge(local, remote);
}

/** Puts `info` into the server's index of `game`: merged with what is there, again while another
 * device writes it meanwhile. */
async function serverIndex(game: string, info: StateInfo): Promise<void> {
  for (let attempt = 0; attempt < 8; attempt++) {
    const { data, revision } = await serverGet(indexName(game));
    if (await serverPut(indexName(game), encodeIndex(merge(parseIndex(data), [info])), revision)) return;
  }
  throw new Error("サーバーのステート一覧が更新され続けています");
}

/** Keeps `file` as `game`'s state in `slot`: here at once, on the server after (the promise
 * `uploaded` says when; it rejects when the server could not be reached). */
export async function putState(
  game: string,
  info: StateInfo,
  file: Uint8Array<ArrayBuffer>,
): Promise<{ uploaded: Promise<void> }> {
  info = { ...info, hash: await stateHash(file) };
  await writeLocal(game, localName(info), file);
  const local = merge(
    parseIndex(await readLocal(game, "index.json")).filter((s) => s.slot !== info.slot),
    [info],
  );
  await writeLocal(game, "index.json", encodeIndex(local));
  await pruneLocal(game, local).catch(() => {});
  const uploaded = (async () => {
    // (the file once: already there is the same bytes)
    const name = fileName(game, info.hash!);
    if (!(await serverPut(name, file, '"0"'))) {
      const there = await serverGet(name);
      if (!there.data || (await stateHash(there.data)) !== info.hash)
        throw new Error("サーバーのステートが壊れています");
    }
    await serverIndex(game, info);
  })();
  uploaded.catch((e: unknown) => console.warn(`state: not on the server: ${String(e)}`));
  return { uploaded };
}

/** `game`'s state in `slot` as `info` lists it: this browser's copy when it is that one, else the server's. */
export async function getState(game: string, info: StateInfo): Promise<Uint8Array<ArrayBuffer>> {
  const local = await readLocal(game, localName(info));
  if (info.hash) {
    if (local && (await stateHash(local)) === info.hash) return local;
  } else {
    const localInfo = parseIndex(await readLocal(game, "index.json")).find((s) => s.slot === info.slot);
    if (local && localInfo?.created === info.created) return local;
  }
  const remote = (await serverGet(info.hash ? fileName(game, info.hash) : oldFileName(game, info.slot))).data;
  if (!remote) {
    if (local && !info.hash) return local;
    throw new Error("ステートが見つかりません");
  }
  if (info.hash && (await stateHash(remote)) !== info.hash)
    throw new Error("サーバーのステートが壊れています");
  // (kept here too, for the next time)
  await writeLocal(game, localName(info), remote).catch(() => {});
  const idx = merge(
    parseIndex(await readLocal(game, "index.json")).filter((s) => s.slot !== info.slot),
    [info],
  );
  await writeLocal(game, "index.json", encodeIndex(idx)).catch(() => {});
  await pruneLocal(game, idx).catch(() => {});
  return remote;
}
