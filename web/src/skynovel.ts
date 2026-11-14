// SKYNovel's Electron games in a web game's frame (frame.ts): the `to_app` its preload script
// gives the page, answered without a main process.
import { FILE } from "./frame-protocol";

type Changed = (key: string) => void;

const norm = (p: string) => {
  const out: string[] = [];
  for (const s of String(p).replaceAll("\\", "/").split("/")) {
    if (s === "..") out.pop();
    else if (s && s !== ".") out.push(s);
  }
  return out.join("/");
};

/**
 * SKYNovel's Electron build (あにまるめいど!, #234) reaches its main process through the
 * `to_app` its preload script exposes (@famibee/skynovel/dist/preload.js; ipcMain handlers in
 * appMain.js): the app's path (where it reads its project, as URLs here: this frame's folder up
 * to resources/app), text files (readFileSync, utf8), its electron-store data (Store,
 * Store_isEmpty, Store_get, flush: kept with the saves) and files it writes (screenshots, kept
 * with the saves); window and dialog calls do nothing (a message box answers "Cancel").
 */
export function toApp(items: Map<string, string>, changed: Changed, page: string) {
  const at = /^(.*?\/resources\/app)(?:\/|$)/i.exec(page);
  const appPath = at ? at[1]! : new URL(".", page).href.replace(/\/$/, "");
  const userData = "C:/Users/user/AppData/Roaming/skynovel";
  let store = "";
  const storeKey = () => `${FILE}skynovel/store/${store}`;
  const fileKey = (p: string) => `${FILE}skynovel/${norm(p)}`;
  const ours = (p: string) => /^https?:/i.test(String(p));
  const bytes = (data: unknown): string => {
    const b =
      typeof data === "string"
        ? new TextEncoder().encode(data)
        : data instanceof ArrayBuffer
          ? new Uint8Array(data)
          : ArrayBuffer.isView(data)
            ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
            : new Uint8Array();
    let s = "";
    for (const c of b) s += String.fromCharCode(c);
    return btoa(s);
  };
  const saved = (p: string) => items.get(fileKey(p));
  const text = (b64: string) => new TextDecoder().decode(Uint8Array.from(atob(b64), (c) => c.charCodeAt(0)));
  const put = (key: string, value: string) => (items.set(key, value), changed(key));
  const fetchText = async (p: string) => {
    const r = await fetch(p);
    if (!r.ok) throw new Error(`ENOENT: no such file or directory, open '${p}'`);
    return r.text();
  };
  const nothing = async (..._args: unknown[]) => undefined;
  return {
    getInfo: async () => ({
      getAppPath: appPath,
      isPackaged: true,
      downloads: "C:/Users/user/Downloads",
      userData,
      getVersion: "",
      env: {},
      platform: "win32",
      arch: "x64",
    }),
    existsSync: async (p: string) =>
      ours(p)
        ? (await fetch(p, { method: "HEAD" }).catch(() => null))?.ok === true
        : saved(p) !== undefined ||
          [...items.keys()].some((k) => k.startsWith(fileKey(p).replace(/\/?$/, "/"))),
    copySync: async (from: string, to: string) => {
      const [f, t] = [fileKey(from).replace(/\/?$/, "/"), fileKey(to).replace(/\/?$/, "/")];
      if (saved(from) !== undefined) put(fileKey(to), saved(from)!);
      // (the copies land in `items`: the keys to copy are taken first)
      const from_ = Array.from(items).filter(([k]) => k.startsWith(f));
      for (const [k, v] of from_) put(t + k.slice(f.length), v);
    },
    removeSync: async (p: string) => {
      const dir = fileKey(p).replace(/\/?$/, "/");
      const gone = Array.from(items.keys()).filter((k) => k === fileKey(p) || k.startsWith(dir));
      for (const k of gone) {
        items.delete(k);
        changed(k);
      }
    },
    ensureFileSync: async (p: string) => void (saved(p) === undefined && !ours(p) && put(fileKey(p), "")),
    readFileSync: async (p: string) => (ours(p) ? fetchText(p) : text(saved(p) ?? "")),
    readFile: async (p: string) =>
      ours(p)
        ? new Uint8Array(await (await fetch(p)).arrayBuffer())
        : Uint8Array.from(atob(saved(p) ?? ""), (c) => c.charCodeAt(0)),
    writeFileSync: async (p: string, data: unknown) => void (!ours(p) && put(fileKey(p), bytes(data))),
    appendFile: async (p: string, data: unknown) =>
      void (
        !ours(p) &&
        put(
          fileKey(p),
          bytes(
            text(saved(p) ?? "") +
              (typeof data === "string" ? data : new TextDecoder().decode(data as ArrayBuffer)),
          ),
        )
      ),
    Store: async (opt: { name?: string } | undefined) => void (store = String(opt?.name ?? "config")),
    Store_isEmpty: async () => {
      const v = items.get(storeKey());
      return !v || v === "{}";
    },
    Store_get: async () => JSON.parse(items.get(storeKey()) ?? "{}") as unknown,
    flush: async (data: unknown) => put(storeKey(), JSON.stringify(data ?? {})),
    zip: nothing,
    unzip: nothing,
    win_close: nothing,
    win_setTitle: async (t: string) => void (document.title = String(t)),
    showMessageBox: async (o: { cancelId?: number } | undefined) => ({
      response: o?.cancelId ?? 1,
      checkboxChecked: false,
    }),
    capturePage: nothing,
    navigate_to: async (u: string) => void open(String(u)),
    openDevTools: nothing,
    win_ev_devtools_opened: nothing,
    isSimpleFullScreen: async () => false,
    setSimpleFullScreen: nothing,
    window: nothing,
    on: (_channel: string, _listener: unknown) => undefined,
  };
}
