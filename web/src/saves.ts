import { SaveTransport } from "./save-transport";
const transport = new SaveTransport();
// Saved games: what the game wrote (in kura's in-memory file system) survives the page.
// The files written under the game's folders and the prefix's user folders go to the browser's
// origin private file system (OPFS) as one blob per game, whenever they change (checked every
// few seconds, and when the page is hidden); the next start puts them back before the program runs (VNVM_SAVES, crates/kura/src/vfs.rs).
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

async function file(name: string, create: boolean): Promise<FileSystemFileHandle | null> {
  try {
    const root = await navigator.storage.getDirectory();
    const dir = await root.getDirectoryHandle("saves", { create });
    return await dir.getFileHandle(`${name}.bin`, { create });
  } catch {
    return null;
  }
}

const backup = (name: string) => new URL(`saves/${encodeURIComponent(name)}`, location.href).href;

/** The blob an earlier session saved for `name`: this browser's, else the server's backup (null: none). */
export async function loadSaves(name: string): Promise<Uint8Array | null> {
  // The server is authoritative across devices; OPFS only seeds legacy saves when absent.
  const remote = await transport.load(backup(name));
  if (remote) return remote;
  const f = await file(name, false);
  const local = f ? new Uint8Array(await (await f.getFile()).arrayBuffer()) : null;
  if (local?.length) await transport.store(backup(name), local);
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

/** Writes `data` as `name`'s blob (OPFS, and the server's backup); false when OPFS fails. */
export async function storeSaves(name: string, data: Uint8Array<ArrayBuffer>): Promise<boolean> {
  await transport.store(backup(name), data);
  // A denied OPFS quota must not prevent server persistence.
  try {
    const f = await file(name, true);
    if (f) {
      const w = await f.createWritable();
      await w.write(data);
      await w.close();
    }
  } catch (e) {
    console.warn("saves: local cache", e);
  }
  return true;
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
