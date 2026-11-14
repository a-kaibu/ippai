import { SaveConflict, SaveTransport } from "./save-transport";
import { withRetries } from "./net";
import { snapping } from "./savestate";
import { portablePath, same } from "./file-records";
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
  _vnhost_vfs_list(roots: number, len: number): number;
  /** changes whenever the file system may have changed */
  _vnhost_vfs_changes?(): number;
}

/** Where this browser keeps the blobs (OPFS "saves/"; tests give their own). */
export interface SaveFiles {
  read(file: string): Promise<Uint8Array<ArrayBuffer> | null>;
  write(file: string, data: Uint8Array): Promise<void>;
  /** the files kept, and a removal (stash keeps the newest few copies set aside) */
  list?(): Promise<string[]>;
  remove?(file: string): Promise<void>;
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
  async list() {
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("saves", { create: true });
    const out: string[] = [];
    for await (const name of (dir as unknown as { keys(): AsyncIterable<string> }).keys()) out.push(name);
    return out;
  },
  async remove(file) {
    const dir = await (await navigator.storage.getDirectory()).getDirectoryHandle("saves");
    await dir.removeEntry(file);
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
  // (the newest STASH_KEEP of the name's copies stay: before, every conflict left a whole copy
  // for good, counted against the quota the chunk cache is trimmed by)
  const all = await files.list?.().catch(() => []);
  if (!all || !files.remove) return;
  for (const f of stashesToDrop(name, all)) await files.remove(f).catch(() => undefined);
}

/** Copies set aside kept per game (stash). */
export const STASH_KEEP = 3;

/** Of the files `all`, `name`'s copies set aside beyond the newest STASH_KEEP (by their time). */
export function stashesToDrop(name: string, all: readonly string[]): string[] {
  const pre = `${name}.`;
  const mine = all
    .filter((f) => f.startsWith(pre) && /^(server|local)-\d+\.bin$/.test(f.slice(pre.length)))
    .map((f) => [f, Number(/-(\d+)\.bin$/.exec(f)![1])] as const)
    .sort((a, b) => b[1] - a[1]);
  return mine.slice(STASH_KEEP).map(([f]) => f);
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

function call(m: SavesModule, f: (s: number, len: number) => number, arg: string): Uint8Array<ArrayBuffer> {
  const s = new TextEncoder().encode(`${arg}\0`);
  const sp = m._malloc(s.length + 8) >>> 0;
  // (no room in the host heap: not the roots written over the guest's memory at address 0)
  if (!sp) throw new Error("saves: no memory for the export");
  try {
    new Uint8Array(m.wasmMemory.buffer).set(s, sp);
    const lenp = sp + Math.ceil(s.length / 4) * 4;
    const p = f(sp, lenp) >>> 0;
    if (!p) throw new Error("saves: no memory for the export");
    const len = new Uint32Array(m.wasmMemory.buffer, lenp, 1)[0]!;
    const out = new Uint8Array(len);
    out.set(new Uint8Array(m.wasmMemory.buffer, p, len));
    m._vnhost_free(p);
    return out;
  } finally {
    m._vnhost_free(sp);
  }
}

/** The paths of the files written under `roots` (vnhost_vfs_list: path length, path, size). */
export function listFiles(m: SavesModule, roots: string[]): string[] {
  const b = call(m, (s, l) => m._vnhost_vfs_list(s, l), roots.join(":"));
  const view = new DataView(b.buffer);
  const out: string[] = [];
  for (let at = 0; at + 4 <= b.length;) {
    const n = view.getUint32(at, true);
    out.push(new TextDecoder().decode(b.subarray(at + 4, at + 4 + n)));
    at += 12 + n;
  }
  return out;
}

/**
 * The records of the files written under `roots`; with `keep`, only those it keeps, picked
 * from their list first so the others (an installer's temp files: hundreds of MB) are never
 * copied out of the module (#234).
 */
export function exportFiles(
  m: SavesModule,
  roots: string[],
  keep?: (path: string) => boolean,
): Uint8Array<ArrayBuffer> {
  if (keep) {
    roots = listFiles(m, roots).filter(keep);
    if (!roots.length) return new Uint8Array(0);
  }
  return call(m, (s, l) => m._vnhost_vfs_export(s, l), roots.join(":"));
}

function exportSaves(m: SavesModule, roots: string[]): Uint8Array<ArrayBuffer> {
  return exportFiles(m, roots, portablePath);
}

/** Writes `data` as `name`'s blob: this browser's copy, then the server's (rejects when the
 * server's failed; this browser's copy is kept, marked as not sent). `local` false: this
 * browser's copy already holds these bytes (marked dirty), only the server's is written;
 * `upload` false: this browser's copy only (marked not sent: sent later, or at the next start). */
export function storeSaves(
  name: string,
  data: Uint8Array<ArrayBuffer>,
  local = true,
  upload = true,
): Promise<boolean> {
  return locked(name, async () => {
    const url = backup(name);
    // This browser's copy first: with the link down the save still survives the page (the
    // server's copy is tried again on the next change check, or the next start). A denied OPFS
    // quota must not prevent server persistence.
    if (local) {
      const base = transport.revision(url) ?? (await readSync(name))?.base ?? null;
      try {
        await writeSync(name, { base, dirty: true });
        await files.write(`${name}.bin`, data);
      } catch (e) {
        console.warn("saves: local cache", e);
      }
    }
    if (!upload) return false;
    await transport.store(url, data);
    await writeSync(name, { base: transport.revision(url) ?? null, dirty: false }).catch((e: unknown) =>
      console.warn("saves: local cache", e),
    );
    return true;
  });
}

/** The least time between two uploads of a game's saves while it plays (keepSaving; one forced
 * by the page being hidden or the game quitting goes at once). */
export const UPLOAD_EVERY = 60_000;

/**
 * Keeps saving what the game writes under `roots` for `name`. Returns a function that saves now
 * (`force`: the upload too, even in the pause after a failed one: when the game quits or the
 * page is hidden).
 */
export function keepSaving(
  m: SavesModule,
  name: string,
  roots: string[],
  last: Uint8Array | null,
  // (tests: the clock and the export; `notice`: what the player is told, once, when the server's
  // copy stopped taking this page's saves: another device saved since)
  opt: { now?: () => number; read?: () => Uint8Array<ArrayBuffer>; notice?: (message: string) => void } = {},
): (force?: boolean) => Promise<void> {
  const now = opt.now ?? Date.now;
  const read = opt.read ?? (() => exportSaves(m, roots));
  let pending: Promise<void> | undefined;
  let queued: Promise<void> | undefined;
  let seen = -1;
  // (`last`: what the server has; `unsent`: what this browser's copy has that the server does not
  // yet. With the server out of reach the blob is written here once, not again every look, and
  // the upload is tried again after a growing pause: before, every 5 s exported the whole set,
  // rewrote it in OPFS and failed the upload, for the rest of the session)
  let unsent: Uint8Array<ArrayBuffer> | null = null;
  let failures = 0;
  let retryAt = 0;
  let told = false;
  const perform = async (force: boolean) => {
    // (a save state is being taken: the memory stays as it is; the next look saves)
    if (snapping()) return;
    // (nothing written since the last look: no export)
    const changes = m._vnhost_vfs_changes?.() ?? -2;
    const fresh = changes !== seen || changes === -2;
    if (!fresh && (!unsent || (!force && now() < retryAt))) return;
    try {
      const data = fresh ? read() : unsent!;
      seen = changes;
      if (same(last, data)) {
        unsent = null;
        return;
      }
      // (the same bytes as this browser's copy already: only the upload again, and only when due)
      const local = !same(unsent, data);
      const due = force || now() >= retryAt;
      if (!local && !due) return;
      unsent = data;
      // (between uploads, a change is written to this browser's copy only: an engine that
      // rewrites a system file every few lines made each 5 s look upload the whole set again;
      // the upload follows UPLOAD_EVERY after the last, at once when the page is hidden or the
      // game quits, and a page killed meanwhile sends this copy at its next start)
      await storeSaves(name, data, local, due);
      if (!due) return;
      last = data;
      unsent = null;
      failures = 0;
      retryAt = now() + UPLOAD_EVERY;
    } catch (e) {
      if (e instanceof SaveConflict && !told) {
        told = true;
        opt.notice?.(
          "別の端末でこのゲームのセーブが更新されたため、サーバーへの保存を止めました（この端末には保存しています）。再読み込みすると別の端末のセーブを使います",
        );
      }
      failures++;
      // (a forced one that failed does not bring the next plain upload forward)
      retryAt = Math.max(retryAt, now() + Math.min(5000 * 2 ** failures, 5 * 60_000));
      console.warn(`saves: ${String(e)}`);
    }
  };
  // (one look at a time, and at most one more waiting: a stuck upload used to have each 5 s tick
  // chain another look behind it)
  const save = (force = false): Promise<void> => {
    if (!pending) {
      pending = perform(force).finally(() => {
        pending = undefined;
      });
      return pending;
    }
    return (queued ??= pending.then(() => {
      queued = undefined;
      return save(force);
    }));
  };
  setInterval(() => void save(), 5000);
  addEventListener("visibilitychange", () => document.visibilityState === "hidden" && void save(true));
  addEventListener("pagehide", () => void save(true));
  return save;
}
