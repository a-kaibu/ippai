// One browser installation: never uploaded with game saves or library exports.
import { exportFiles, type SavesModule } from "./saves";
import { installationPath, selectRecords } from "./file-records";
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
async function read(): Promise<State | undefined> {
  const db = await database();
  try {
    return await new Promise((resolve, reject) => {
      const r = db.transaction("state").objectStore("state").get("prefix32");
      r.onsuccess = () => resolve(r.result as State | undefined);
      r.onerror = () => reject(r.error);
    });
  } finally {
    db.close();
  }
}
async function write(state: State): Promise<void> {
  const db = await database();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction("state", "readwrite");
      tx.objectStore("state").put(state, "prefix32");
      tx.oncomplete = () => resolve();
      tx.onerror = () => reject(tx.error);
      tx.onabort = () => reject(tx.error);
    });
  } finally {
    db.close();
  }
}
let key: CryptoKey | undefined;
let lease: Promise<void> | undefined;
function ownInstallation(): Promise<void> {
  return (lease ??= new Promise((resolve, reject) => {
    if (!navigator.locks) {
      reject(new Error("このブラウザではインストール状態を安全に共有できません。"));
      return;
    }
    void navigator.locks
      .request("ippai-installation", { ifAvailable: true }, async (lock) => {
        if (!lock) {
          reject(new Error("別のタブでゲームが動いています。先にそのタブを閉じてください。"));
          return;
        }
        resolve();
        // Browsing-context destruction releases the installation lease. Keeping
        // it for the launch prevents independent VMs overwriting one key store.
        await new Promise<void>(() => {});
      })
      .catch(reject);
  }));
}
export async function loadInstallation(): Promise<Uint8Array<ArrayBuffer> | null> {
  await ownInstallation();
  const state = await read();
  if (!state) return null;
  key = state.key;
  const plain = await crypto.subtle.decrypt({ name: "AES-GCM", iv: state.iv }, key, state.encrypted);
  return selectRecords(new Uint8Array(plain), installationPath);
}
export function keepInstallation(
  module: SavesModule,
  initial: Uint8Array<ArrayBuffer> | null,
  failed: () => void,
): void {
  let previous = initial;
  let revision = -1;
  let pending = false;
  let reported = false;
  const save = async () => {
    if (pending) return;
    const current = module._vnhost_vfs_changes?.() ?? Date.now();
    if (current === revision) return;
    pending = true;
    try {
      const data = selectRecords(exportFiles(module, ["/vnvm/prefix/drive_c"]), installationPath)!;
      revision = current;
      if (!data.length) return;
      if (previous?.length === data.length && previous.every((b, i) => data[i] === b)) return;
      key ??= await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
        "encrypt",
        "decrypt",
      ]);
      const iv = crypto.getRandomValues(new Uint8Array(12));
      const encrypted = await crypto.subtle.encrypt({ name: "AES-GCM", iv }, key, data);
      await write({ key, iv, encrypted });
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
  setInterval(() => void save(), 1000);
  addEventListener("pagehide", () => void save());
}
