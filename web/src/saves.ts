import { SaveTransport } from "./save-transport";
import { withRetries } from "./net";
import { snapping } from "./savestate";
// Saved games: what the game wrote (in kura's in-memory file system) survives the page.
// The files written under the game's folders and the prefix's user folders go to the browser's
// origin private file system (OPFS) as one blob per game, whenever they change (checked every
// few seconds, and when the page is hidden); the next start puts them back before the program runs (IPPAI_SAVES, crates/kura/src/vfs.rs).
// Each new blob is also backed up through ippai serve to S3 (`/saves/<name>`, when the server
// has a bucket set up); a browser without a blob of its own starts from that backup.

export interface SavesModule {
  wasmMemory: WebAssembly.Memory;
  _malloc(size: number): number;
  _vnhost_free(p: number): void;
  _vnhost_vfs_export(roots: number, len: number): number;
  /** changes whenever the file system may have changed */
  _vnhost_vfs_changes?(): number;
}

/** Where this browser keeps the blobs (OPFS "saves/"; tests give their own). */
export interface SaveFiles {
  read(file: string): Promise<Uint8Array<ArrayBuffer> | null>;
  write(file: string, data: Uint8Array): Promise<void>;
}

const opfs: SaveFiles = {
  async read(file) {
    try {
      const root = await navigator.storage.getDirectory();
      const dir = await root.getDirectoryHandle("saves");
      const f = await dir.getFileHandle(file);
      return new Uint8Array(await (await f.getFile()).arrayBuffer());
    } catch (e) {
      console.warn(`saves: read ${file}: ${String(e)}`);
      return null;
    }
  },
  async write(file, data) {
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle("saves", { create: true });
    const w = await (await dir.getFileHandle(file, { create: true })).createWritable();
    await w.write(data as Uint8Array<ArrayBuffer>);
    await w.close();
  },
};

let files: SaveFiles = opfs;
let transport = new SaveTransport();
/** Tests: another place for the blobs and another fetch (a fresh transport: a new page). */
export function savesFor(f: SaveFiles, request?: typeof fetch): void {
  files = f;
  transport = new SaveTransport(request);
  locks.clear();
}

const backup = (name: string) => new URL(`saves/${encodeURIComponent(name)}`, location.href).href;

// Beside each blob (<name>.bin), what it is to the server (<name>.sync.json): `base`, the
// server's revision it came from or was last sent as, and `dirty`, written here and not yet
// acknowledged by the server. A save made offline (or whose upload the page did not live to see)
// stays dirty across pages, so the next start that reaches the server sends it instead of
// starting from the server's older copy. Written dirty before the blob and clean only after the
// server's acknowledgement: a page that dies in between leaves it dirty (sent again, harmless).
interface Sync {
  base: string | null;
  dirty: boolean;
}
async function readSync(name: string): Promise<Sync | null> {
  const b = await files.read(`${name}.sync.json`).catch(() => null);
  if (!b) return null;
  try {
    const v = JSON.parse(new TextDecoder().decode(b)) as Partial<Sync>;
    return { base: typeof v.base === "string" ? v.base : null, dirty: v.dirty !== false };
  } catch {
    // (unreadable: as if not sent, the safe side)
    return { base: null, dirty: true };
  }
}
const writeSync = (name: string, s: Sync) =>
  files.write(`${name}.sync.json`, new TextEncoder().encode(JSON.stringify(s)));
/** Keeps `data` beside the blob (`what`: whose copy), when one of two copies must give way. */
async function stash(name: string, what: string, data: Uint8Array): Promise<void> {
  await files
    .write(`${name}.${what}-${Date.now()}.bin`, data)
    .catch((e: unknown) => console.warn("saves: stash", e));
}

// (one save of a name at a time: a later one's "dirty" never lands under an earlier one's "clean")
const locks = new Map<string, Promise<unknown>>();
function locked<T>(name: string, f: () => Promise<T>): Promise<T> {
  const run = (locks.get(name) ?? Promise.resolve()).catch(() => {}).then(f);
  locks.set(name, run);
  return run;
}

/**
 * The blob an earlier session saved for `name` (null: none). With the server in reach: the
 * server's, unless this browser's copy was saved after it was last sent (offline, or the page
 * closed before the upload): then this browser's, sent now; when the server's copy moved too
 * (another device), this browser's still wins and the server's is kept beside it (and in the
 * server's revisions), `notice` told. Out of reach: this browser's copy, sent later.
 */
export async function loadSaves(
  name: string,
  notice?: (message: string) => void,
): Promise<Uint8Array | null> {
  const url = backup(name);
  const sync = await readSync(name);
  const local = await files.read(`${name}.bin`).catch(() => null);
  let remote: Uint8Array | null;
  try {
    // (offline: no tries at all; started from the app kept on the device, sw.ts)
    if (!navigator.onLine) throw new Error("offline");
    remote = await withRetries(() => transport.load(url), 3);
  } catch (e) {
    console.warn(`saves: server out of reach, starting from this browser's copy: ${String(e)}`);
    transport.startedOffline(url, local, sync?.base);
    return local;
  }
  const revision = transport.revision(url) ?? '"0"';
  const send = async (data: Uint8Array) => {
    // (not sent now: still dirty, sent with the next save or the next start)
    await storeSaves(name, data as Uint8Array<ArrayBuffer>).catch((e: unknown) =>
      console.warn(`saves: not sent: ${String(e)}`),
    );
  };
  if (local && sync?.dirty) {
    if (remote && sync.base !== revision && !same(remote, local)) {
      await stash(name, "server", remote);
      console.warn(`saves: ${name}: saved here and on another device since ${sync.base}; this device's kept`);
      notice?.(
        "別の端末のセーブより、この端末で後から保存したセーブを使います（別の端末のものも残してあります）",
      );
    }
    await send(local);
    return local;
  }
  if (remote) {
    // (an older page's copy, not known to be sent: kept aside rather than lost)
    if (local && !sync && !same(local, remote)) await stash(name, "local", local);
    await locked(name, async () => {
      await writeSync(name, { base: revision, dirty: true });
      await files.write(`${name}.bin`, remote);
      await writeSync(name, { base: revision, dirty: false });
    }).catch((e: unknown) => console.warn("saves: local cache", e));
    return remote;
  }
  if (local?.length) await send(local);
  return local;
}

function exportSaves(m: SavesModule, roots: string[]): Uint8Array<ArrayBuffer> {
  const s = new TextEncoder().encode(`${roots.join(":")}\0`);
  const sp = m._malloc(s.length + 8) >>> 0;
  new Uint8Array(m.wasmMemory.buffer).set(s, sp);
  const lenp = sp + Math.ceil(s.length / 4) * 4;
  const p = m._vnhost_vfs_export(sp, lenp) >>> 0;
  const len = new Uint32Array(m.wasmMemory.buffer, lenp, 1)[0]!;
  const out = new Uint8Array(len);
  out.set(new Uint8Array(m.wasmMemory.buffer, p, len));
  m._vnhost_free(p);
  m._vnhost_free(sp);
  return out;
}

function same(a: Uint8Array | null, b: Uint8Array): boolean {
  if (!a || a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** Writes `data` as `name`'s blob: this browser's copy, then the server's (rejects when the
 * server's failed; this browser's copy is kept, marked as not sent). */
export function storeSaves(name: string, data: Uint8Array<ArrayBuffer>): Promise<boolean> {
  return locked(name, async () => {
    const url = backup(name);
    // This browser's copy first: with the link down the save still survives the page (the
    // server's copy is tried again on the next change check, or the next start). A denied OPFS
    // quota must not prevent server persistence.
    const base = transport.revision(url) ?? (await readSync(name))?.base ?? null;
    try {
      await writeSync(name, { base, dirty: true });
      await files.write(`${name}.bin`, data);
    } catch (e) {
      console.warn("saves: local cache", e);
    }
    await transport.store(url, data);
    await writeSync(name, { base: transport.revision(url) ?? null, dirty: false }).catch((e: unknown) =>
      console.warn("saves: local cache", e),
    );
    return true;
  });
}

/**
 * Keeps saving what the game writes under `roots` for `name`. Returns a function that saves now
 * (when the game quits).
 */
export function keepSaving(
  m: SavesModule,
  name: string,
  roots: string[],
  last: Uint8Array | null,
): () => Promise<void> {
  let pending: Promise<void> | undefined;
  let seen = -1;
  const perform = async () => {
    // (a save state is being taken: the memory stays as it is; the next look saves)
    if (snapping()) return;
    // (nothing written since the last look: no export)
    const changes = m._vnhost_vfs_changes?.() ?? -2;
    if (changes === seen && changes !== -2) return;
    try {
      const data = exportSaves(m, roots);
      if (same(last, data)) {
        seen = changes;
        return;
      }
      if (!(await storeSaves(name, data))) return;
      last = data;
      seen = changes;
    } catch (e) {
      console.warn(`saves: ${String(e)}`);
    }
  };
  const save = (): Promise<void> => {
    if (pending) return pending.then(save);
    pending = perform().finally(() => {
      pending = undefined;
    });
    return pending;
  };
  setInterval(save, 5000);
  addEventListener("visibilitychange", () => document.visibilityState === "hidden" && void save());
  addEventListener("pagehide", () => void save());
  return save;
}
