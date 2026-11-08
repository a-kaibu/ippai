// Browser-local account vault. The key is non-exportable; same-origin scripts can
// still use it. Never include this database in game saves, exports or telemetry.
export interface AccountSettings {
  email: string;
  password: string;
  rememberPassword: boolean;
}
interface StoredAccount {
  email: string;
  key?: CryptoKey;
  iv?: Uint8Array<ArrayBuffer>;
  ciphertext?: ArrayBuffer;
}
// Unsaved passwords last only for this page visit. Never serialize this cache.
let sessionAccount: AccountSettings | undefined;
function database(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const request = indexedDB.open("ippai-account", 1);
    request.onupgradeneeded = () => request.result.createObjectStore("settings");
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error("設定用のストレージを開けませんでした。"));
  });
}
async function read(): Promise<StoredAccount | undefined> {
  const db = await database();
  try {
    return await new Promise((resolve, reject) => {
      const request = db.transaction("settings").objectStore("settings").get("dmm");
      request.onsuccess = () => resolve(request.result as StoredAccount | undefined);
      request.onerror = () => reject(new Error("設定を読み込めませんでした。"));
    });
  } finally {
    db.close();
  }
}
async function write(value?: StoredAccount): Promise<void> {
  const db = await database();
  try {
    await new Promise<void>((resolve, reject) => {
      const tx = db.transaction("settings", "readwrite");
      const store = tx.objectStore("settings");
      if (value) store.put(value, "dmm");
      else store.delete("dmm");
      tx.oncomplete = () => resolve();
      tx.onabort = tx.onerror = () => reject(new Error("設定を保存できませんでした。"));
    });
  } finally {
    db.close();
  }
}
export async function loadAccountSettings(): Promise<AccountSettings> {
  if (sessionAccount) return { ...sessionAccount };
  const saved = await read();
  let password = "";
  if (saved?.key && saved.iv && saved.ciphertext) {
    const bytes = await crypto.subtle.decrypt({ name: "AES-GCM", iv: saved.iv }, saved.key, saved.ciphertext);
    password = new TextDecoder().decode(bytes);
  }
  return { email: saved?.email ?? "", password, rememberPassword: Boolean(saved?.ciphertext) };
}
export async function saveAccountSettings(settings: AccountSettings): Promise<void> {
  const saved: StoredAccount = { email: settings.email.trim() };
  if (settings.rememberPassword && settings.password) {
    saved.key = await crypto.subtle.generateKey({ name: "AES-GCM", length: 256 }, false, [
      "encrypt",
      "decrypt",
    ]);
    saved.iv = crypto.getRandomValues(new Uint8Array(12));
    saved.ciphertext = await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: saved.iv },
      saved.key,
      new TextEncoder().encode(settings.password),
    );
  }
  await write(saved);
  sessionAccount = { ...settings, email: saved.email };
}
export async function clearAccountSettings(): Promise<void> {
  await write();
  sessionAccount = undefined;
}

/** One use only, consumed by the verified original SDRT dialog, never the relay. */
export async function accountInput(): Promise<SharedArrayBuffer | null> {
  const account = await loadAccountSettings();
  if (!account.email || !account.password) return null;
  if (
    account.email.length > 320 ||
    account.password.length > 1024 ||
    account.email.includes("\0") ||
    account.password.includes("\0")
  ) {
    throw new Error("DMM アカウントの入力が長すぎるか、使用できない文字を含んでいます。");
  }
  const shared = new SharedArrayBuffer(8 + 2 * (320 + 1024));
  const words = new Uint16Array(shared, 4);
  words[0] = account.email.length;
  words[1] = account.password.length;
  for (let i = 0; i < account.email.length; i++) words[2 + i] = account.email.charCodeAt(i);
  for (let i = 0; i < account.password.length; i++) words[2 + 320 + i] = account.password.charCodeAt(i);
  Atomics.store(new Int32Array(shared, 0, 1), 0, 1);
  return shared;
}
