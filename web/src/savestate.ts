// Save states ("save anywhere", docs/savestate.md): the whole running program at a moment, kept
// in slots per game, back where it was in a later page. The module stops every thread where
// it can go on from (crates/kura/src/snap.rs, crates/sake/src/snap.rs), the drawing thread reads
// the GPU into memory, and the page copies the memory: its blocks compressed with zstd in a
// worker (snapz-worker.ts). A state starts a new page (`&state=<slot>`): the memory filled
// before the module runs, which then starts its threads where they stood.
//
// A state is kept in this browser (OPFS: states/<game>/<slot>.snap and index.json) and on the
// server (ippai serve /saves/state.<game>.<slot>, its S3 bucket), so another device goes on
// from it too. The file is host/snap.js's: "IPPAISNP", u32 version, u32 header length, the
// header (JSON), then blocks (u32 address, u32 length, u32 compressed length, a zstd frame),
// ended by the address 0xffffffff.

import type { Reply, Request } from "./snapz-worker";
import { sakeBase, type SakeBuild } from "./runtime";

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

let busy = false;
/** A state is being taken: nothing of the page may change the module's memory. */
export const snapping = () => busy;

let worker: Worker | undefined;
function work(r: Request, transfer: Transferable[] = []): Promise<Reply> {
  worker ??= new Worker(new URL("./snapz-worker.ts", import.meta.url), { type: "module" });
  const w = worker;
  return new Promise((resolve, reject) => {
    w.onmessage = (e: MessageEvent<Reply>) => (e.data.ok ? resolve(e.data) : reject(new Error(e.data.error)));
    w.onerror = (e) => reject(new Error(e.message));
    w.postMessage(r, transfer);
  });
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
    const first = m._vnhost_render_shot(p);
    for (let k = 0; k < 25 && m._vnhost_render_shot(p) === first; k++)
      await new Promise((r) => setTimeout(r, 20));
    const [ptr, w, h] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3)) as [number, number, number];
    m._free(p);
    if (!ptr || !w || !h) return "";
    // (BGRA rows)
    const px = new Uint8ClampedArray(new Uint8Array(m.wasmMemory.buffer, ptr, w * h * 4));
    for (let i = 0; i < px.length; i += 4) [px[i], px[i + 2]] = [px[i + 2]!, px[i]!];
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
  const ctl = m._vnhost_snap_ctl() >>> 0;
  const i32 = new Int32Array(m.wasmMemory.buffer);
  const word = (off: number) => (ctl + off) >>> 2;
  busy = true;
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
    busy = false;
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

/** A memory with the state in it, for the module to start from (its `wasmMemory`): `fixed`,
 * the 4 GB one the page reserved for sake.wasm, or a new one for sake-grow.wasm. */
export async function stateMemory(
  file: Uint8Array<ArrayBuffer>,
  fixed?: WebAssembly.Memory,
): Promise<WebAssembly.Memory> {
  const h = stateHeader(file);
  if (!loadable(await builds(), h))
    throw new Error("ippai の更新前のステートです（このバージョンでは読み込めません）");
  // (sake's memory: crates/sake/build.sh FIXED / GROW, as large as it was)
  const mem =
    h.wasm === "sake" ? fixed : new WebAssembly.Memory({ initial: h.pages, maximum: 65535, shared: true });
  if (!mem) throw new Error("この端末ではこのステートを読み込めません（4 GB のメモリを確保できません）");
  const copy =
    file.byteOffset === 0 && file.byteLength === file.buffer.byteLength
      ? file.buffer.slice(0)
      : file.slice().buffer;
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

// ---- slots: this browser's (OPFS) and the server's ----

const serverUrl = (name: string) => new URL(`saves/${encodeURIComponent(name)}`, location.href).href;
const fileName = (game: string, slot: number) => `state.${game}.${slot}`;
const indexName = (game: string) => `state.${game}.index`;

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

// The server's /saves keeps revisions (If-Match): a slot is written over whatever is there.
const revisions = new Map<string, string>();
async function serverGet(name: string): Promise<Uint8Array<ArrayBuffer> | null> {
  const r = await fetch(serverUrl(name), { cache: "no-store" });
  if (r.status === 404) {
    revisions.set(name, '"0"');
    return null;
  }
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  revisions.set(name, r.headers.get("etag") ?? '"0"');
  return new Uint8Array(await r.arrayBuffer());
}
async function revisionOf(name: string): Promise<string> {
  const r = await fetch(serverUrl(name), { method: "HEAD", cache: "no-store" });
  if (r.status === 404) return '"0"';
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.headers.get("etag") ?? '"0"';
}
async function serverPut(name: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
  for (let attempt = 0; attempt < 2; attempt++) {
    const rev = revisions.get(name) ?? (await revisionOf(name));
    const r = await fetch(serverUrl(name), { method: "PUT", headers: { "if-match": rev }, body: data });
    if (r.status === 412) {
      // (another page wrote it since: the newer state is this one)
      revisions.delete(name);
      continue;
    }
    if (!r.ok) throw new Error(`HTTP ${r.status}`);
    revisions.set(name, r.headers.get("etag") ?? rev);
    return;
  }
  throw new Error("サーバーのステートが更新され続けています");
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
function merge(a: StateInfo[], b: StateInfo[]): StateInfo[] {
  const m = new Map<number, StateInfo>();
  for (const s of [...a, ...b]) if (!m.has(s.slot) || m.get(s.slot)!.created < s.created) m.set(s.slot, s);
  return [...m.values()].sort((x, y) => x.slot - y.slot);
}

/** `game`'s states (this browser's and the server's, the newer of each slot). */
export async function listStates(game: string): Promise<StateInfo[]> {
  const local = parseIndex(await readLocal(game, "index.json"));
  const remote = await serverGet(indexName(game)).then(parseIndex, () => []);
  return merge(local, remote);
}

/** Keeps `file` as `game`'s state in `slot`: here at once, on the server after (the promise
 * `uploaded` says when; it rejects when the server could not be reached). */
export async function putState(
  game: string,
  info: StateInfo,
  file: Uint8Array<ArrayBuffer>,
): Promise<{ uploaded: Promise<void> }> {
  await writeLocal(game, `${info.slot}.snap`, file);
  const local = merge(
    parseIndex(await readLocal(game, "index.json")).filter((s) => s.slot !== info.slot),
    [info],
  );
  await writeLocal(game, "index.json", encodeIndex(local));
  const uploaded = (async () => {
    await serverPut(fileName(game, info.slot), file);
    // (the server's index: what it had, this slot replaced)
    const remote = parseIndex(await serverGet(indexName(game)).catch(() => null)).filter(
      (s) => s.slot !== info.slot,
    );
    await serverPut(indexName(game), encodeIndex(merge(remote, [info])));
  })();
  uploaded.catch((e: unknown) => console.warn(`state: not on the server: ${String(e)}`));
  return { uploaded };
}

/** `game`'s state in `slot` as `info` lists it: this browser's copy when it is that one, else the server's. */
export async function getState(game: string, info: StateInfo): Promise<Uint8Array<ArrayBuffer>> {
  const local = await readLocal(game, `${info.slot}.snap`);
  const localInfo = parseIndex(await readLocal(game, "index.json")).find((s) => s.slot === info.slot);
  if (local && localInfo?.created === info.created) return local;
  const remote = await serverGet(fileName(game, info.slot));
  if (!remote) {
    if (local) return local;
    throw new Error("ステートが見つかりません");
  }
  // (kept here too, for the next time)
  await writeLocal(game, `${info.slot}.snap`, remote).catch(() => {});
  const idx = merge(
    parseIndex(await readLocal(game, "index.json")).filter((s) => s.slot !== info.slot),
    [info],
  );
  await writeLocal(game, "index.json", encodeIndex(idx)).catch(() => {});
  return remote;
}
