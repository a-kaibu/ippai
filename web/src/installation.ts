// One browser installation: never uploaded with game saves or library exports.
import { toast } from "sonner";
import { exportFiles, type SavesModule } from "./saves";
import { installationPath, legacyRegistry, renameRecords, same, selectRecords } from "./file-records";
interface State {
  key: CryptoKey;
  iv: Uint8Array<ArrayBuffer>;
  encrypted: ArrayBuffer;
}
function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open("ippai-installation", 1);
    request.onupgradeneeded = () => request.result.createObjectStore("state");
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error("インストール状態の保存先を開けませんでした。"));
  });
}
async function read(name = "prefix32"): Promise<State | undefined> {
  const db = await database();
  try {
    return await new Promise((resolve, reject) => {
      const r = db.transaction("state").objectStore("state").get(name);
      r.onsuccess = () => resolve(r.result as State | undefined);
      r.onerror = () => reject(r.error);
    });
  } finally {
    db.close();
  }
}
async function write(state: State, name = "prefix32"): Promise<void> {
  const db = await database();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction("state", "readwrite");
      tx.objectStore("state").put(state, name);
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}
let key: CryptoKey | undefined;
let lease: Promise<boolean> | undefined;
/**
 * The installation's writer: one tab at a time, taken when this tab first has something to
 * write (most games never write outside the user folders, so any number of them run at once).
 */
function ownInstallation(): Promise<boolean> {
  // (not had: asked again at the next write, so a tab outliving the owner takes over; before,
  // its changes were dropped for the rest of its life)
  const p = (lease ??= new Promise((resolve) => {
    if (!navigator.locks) return resolve(false);
    void navigator.locks
      .request("ippai-installation", { ifAvailable: true }, async (lock) => {
        resolve(!!lock);
        // Browsing-context destruction releases the lease. Keeping it for the
        // page prevents another tab overwriting this one's provider keys.
        if (lock) await new Promise<void>(() => {});
      })
      .catch(() => resolve(false));
  }));
  void p.then((had) => {
    if (!had && lease === p) lease = undefined;
  });
  return p;
}
async function decrypt(state: State | undefined): Promise<Uint8Array<ArrayBuffer> | null> {
  if (!state) return null;
  key ??= state.key;
  return new Uint8Array(
    await crypto.subtle.decrypt({ name: "AES-GCM", iv: state.iv }, state.key, state.encrypted),
  );
}
async function encrypt(data: Uint8Array<ArrayBuffer>): Promise<State> {
  key ??= await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, ["encrypt", "decrypt"]);
  const iv = crypto.getRandomValues(new Uint8Array(12));
  return { key, iv, encrypted: await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, data) };
}
/**
 * This device's installation (file-records.ts installationPath), and the registry an older
 * record kept for every game (8ab1b50-83ea93a: the saves had none), for saves still without one.
 */
export async function loadInstallation(): Promise<{
  files: Uint8Array<ArrayBuffer> | null;
  registry: Uint8Array<ArrayBuffer> | null;
}> {
  // (written before the rename: /vnvm/prefix paths read as /ippai/prefix ones)
  const plain = renameRecords(await decrypt(await read()));
  let registry = selectRecords(plain, legacyRegistry);
  if (registry?.length) {
    // (kept aside once: the record is rewritten without it at this tab's first write)
    if (!(await read("legacy-registry")) && (await ownInstallation()))
      await write(await encrypt(registry), "legacy-registry");
  } else {
    registry = renameRecords(await decrypt(await read("legacy-registry")));
  }
  return { files: selectRecords(plain, installationPath), registry };
}
/** How often the installation is looked at (installers and runtimes write it, seldom; the
 * page being hidden or left looks at once). Each look with the file system changed exports the
 * whole C: drive: every 5 s, as the saves, was a periodic hitch for the whole session. */
export const INSTALL_LOOK_MS = 30_000;

/** Keeps this device's installation (installationPath) written whenever it changes; returns a
 * look now (tests; the page's own timer and events call it too). */
export function keepInstallation(
  module: SavesModule,
  initial: Uint8Array<ArrayBuffer> | null,
  failed: () => void,
  // (tests: no timer and no page events)
  opt: { timers?: boolean } = {},
): () => Promise<void> {
  let previous = initial;
  let revision = -1;
  let pending = false;
  let reported = false;
  let busy = false;
  const save = async () => {
    if (pending) return;
    const current = module._vnhost_vfs_changes?.() ?? Date.now();
    if (current === revision) return;
    pending = true;
    try {
      const data = selectRecords(exportFiles(module, ["/ippai/prefix/drive_c"]), installationPath)!;
      revision = current;
      if (!data.length) return;
      if (same(previous, data)) return;
      if (!(await ownInstallation())) {
        // (another tab writes the installation: this one's changes stay in its memory)
        if (!busy) {
          busy = true;
          toast.warning("別のタブがインストール状態を保存中のため、このタブの変更は保存しません。");
        }
        return;
      }
      await write(await encrypt(data));
      previous = data;
    } catch {
      revision = -1;
      if (!reported) {
        reported = true;
        failed();
      }
    } finally {
      pending = false;
    }
  };
  if (opt.timers !== false) {
    setInterval(() => void save(), INSTALL_LOOK_MS);
    addEventListener("visibilitychange", () => document.visibilityState === "hidden" && void save());
    addEventListener("pagehide", () => void save());
  }
  return save;
}
