// Saved games: what the game wrote (in Wine's in-memory file system) survives the page.
// The files written under the game's folders and the prefix's user folders go to the browser's
// origin private file system (OPFS) as one blob per game, whenever they change (checked every
// few seconds, and when the page is hidden); the next start puts them back before Wine runs (VNVM_SAVES, crates/vnhost/src/vfs.rs).
// Each new blob is also backed up through vnvm serve to S3 (`/saves/<name>`, when the server
// has a bucket set up); a browser without a blob of its own starts from that backup.

export interface SavesModule {
  wasmMemory: WebAssembly.Memory;
  _malloc(size: number): number;
  _vnhost_free(p: number): void;
  _vnhost_vfs_export(roots: number, len: number): number;
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
  const f = await file(name, false);
  const data = f ? new Uint8Array(await (await f.getFile()).arrayBuffer()) : null;
  if (data?.length) return data;
  try {
    const r = await fetch(backup(name));
    if (!r.ok) return null;
    const b = new Uint8Array(await r.arrayBuffer());
    console.log(`saves: ${name} from the backup (${b.length} bytes)`);
    return b.length ? b : null;
  } catch {
    return null;
  }
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
  let busy = false;
  const save = async () => {
    if (busy) return;
    busy = true;
    try {
      const data = exportSaves(m, roots);
      if (same(last, data)) return;
      const f = await file(name, true);
      if (!f) return;
      const w = await f.createWritable();
      await w.write(data);
      await w.close();
      last = data;
      console.log(`saves: ${name} ${data.length} bytes`);
      // (keepalive: it still goes out when the page is closing)
      void fetch(backup(name), { method: "PUT", body: data, keepalive: data.length < 60000 })
        .then((r) => r.ok || r.status === 404 || console.warn(`saves: backup: HTTP ${r.status}`))
        .catch((e) => console.warn(`saves: backup: ${e}`));
    } catch (e) {
      console.warn(`saves: ${e}`);
    } finally {
      busy = false;
    }
  };
  setInterval(save, 5000);
  addEventListener("visibilitychange", () => document.visibilityState === "hidden" && void save());
  addEventListener("pagehide", () => void save());
  return save;
}
