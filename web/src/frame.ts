// A web game's frame (NW.js or Electron pages: TyranoScript, RPG Maker MV/MZ), built as
// frame.js and loaded first by each of its pages (crates/ippai/src/treefiles.rs adds the
// <script>). The frame is on another origin than the app (ippai serve's game listener): its
// scripts reach the app only through the messages of frame-protocol.ts, which the app's page
// (html.ts) checks. Before any script of the game this gives the page a localStorage kept with
// ippai's saved games and the Node stand-ins NW.js games reach for; the saves come from the
// app's page, and until they do the page's DOMContentLoaded and load wait.
import { FILE, type FromFrame, type Init } from "./frame-protocol";
import { fileUrl, readSync } from "./sync-read";

type Changed = (key: string) => void;

/** A Storage over `items` (string keys and values); `changed(key)` after each write. */
function storage(items: Map<string, string>, changed: Changed): Storage {
  const keys = () => [...items.keys()].filter((k) => !k.startsWith(FILE));
  const api: Storage = {
    get length() {
      return keys().length;
    },
    key: (i: number) => keys()[i] ?? null,
    getItem: (key: string) => items.get(String(key)) ?? null,
    setItem: (key: string, value: string) => (items.set(String(key), String(value)), changed(String(key))),
    removeItem: (key: string) => void (items.delete(String(key)) && changed(String(key))),
    clear: () => keys().forEach((k) => (items.delete(k), changed(k))),
  };
  // (pages also use storage["key"] and Object.keys(storage))
  return new Proxy(api, {
    get: (t, k) => (typeof k === "symbol" || k in t ? Reflect.get(t, k) : (t.getItem(k) ?? undefined)),
    set: (t, k, v) => (typeof k === "symbol" || k in t ? false : (t.setItem(k, v), true)),
    deleteProperty: (t, k) => (typeof k === "string" && t.removeItem(k), true),
    has: (t, k) => typeof k === "string" && (k in t || t.getItem(k) !== null),
    ownKeys: () => keys(),
    getOwnPropertyDescriptor: (t, k) =>
      typeof k === "string" && t.getItem(k) !== null
        ? { value: t.getItem(k), writable: true, enumerable: true, configurable: true }
        : undefined,
  });
}

const norm = (p: string) => {
  const out: string[] = [];
  for (const s of String(p).replaceAll("\\", "/").split("/")) {
    if (s === "..") out.pop();
    else if (s && s !== ".") out.push(s);
  }
  return out.join("/");
};

const nodePath = {
  sep: "/",
  join: (...p: string[]) => norm(p.join("/")),
  resolve: (...p: string[]) => norm(p.join("/")),
  dirname: (p: string) => norm(p).split("/").slice(0, -1).join("/") || ".",
  basename: (p: string, ext = "") => {
    const b = norm(p).split("/").pop() ?? "";
    return ext && b.endsWith(ext) ? b.slice(0, -ext.length) : b;
  },
  extname: (p: string) => /\.[^./]*$/.exec(norm(p).split("/").pop() ?? "")?.[0] ?? "",
};

// A value whose properties and calls all give itself back, and which turns into "" or 0.
const stub: unknown = new Proxy(function () {}, {
  get: (_, key) => (key === Symbol.toPrimitive ? () => "" : key === "then" ? undefined : stub),
  apply: () => stub,
  construct: () => stub as object,
});

/**
 * Node's require for NW.js games that call it from the page: synchronous fs (reads the game's
 * files from its folder when asked, sync-read.ts; writes as saves) and path; nw.gui and the like
 * do nothing.
 */
function nodeRequire(folder: Folder, items: Map<string, string>, changed: Changed) {
  const read = (p: string): Uint8Array | null => {
    const saved = items.get(FILE + norm(p));
    if (saved !== undefined) return Uint8Array.from(atob(saved), (c) => c.charCodeAt(0));
    return folder.names.has(norm(p)) ? readSync(fileUrl(folder.base, norm(p))) : null;
  };
  const exists = (p: string) =>
    items.has(FILE + norm(p)) ||
    folder.names.has(norm(p)) ||
    [...folder.names].some((name) => name.startsWith(norm(p).replace(/\/$/, "") + "/"));
  const encoding = (opt: unknown) =>
    typeof opt === "string" ? opt : (opt as { encoding?: string } | undefined)?.encoding;
  const write = (p: string, data: unknown) => {
    const b = typeof data === "string" ? new TextEncoder().encode(data) : new Uint8Array(data as ArrayBuffer);
    let s = "";
    for (const c of b) s += String.fromCharCode(c);
    items.set(FILE + norm(p), btoa(s));
    changed(FILE + norm(p));
  };
  const fs = {
    existsSync: exists,
    readFileSync(p: string, opt?: unknown) {
      const data = read(p);
      if (!data)
        throw Object.assign(new Error(`ENOENT: no such file or directory, open '${p}'`), { code: "ENOENT" });
      const text = () => new TextDecoder().decode(data);
      // (a Buffer turns into its text, as with unescape(fs.readFileSync(p)))
      return encoding(opt) ? text() : Object.assign(data, { toString: text });
    },
    writeFileSync: write,
    unlinkSync: (p: string) => void (items.delete(FILE + norm(p)) && changed(FILE + norm(p))),
    readdirSync: () => [],
    mkdirSync: () => undefined,
    statSync: (p: string) => ({ isDirectory: () => !/\.[^./\\]*$/.test(String(p)), isFile: () => exists(p) }),
  };
  const electron = {
    remote: { app: { getAppPath: () => "." } },
    shell: { openExternal: (u: string) => open(u) },
  };
  const modules: Record<string, unknown> = { fs, "fs-extra": fs, path: nodePath, electron };
  return (name: string) => {
    if (name in modules) return modules[name];
    if (["nw.gui", "child_process", "os", "http", "https", "crypto"].includes(name)) return stub;
    throw new Error(`Cannot find module '${name}'`);
  };
}

/**
 * TyranoScript's Electron build (configSave=file) keeps its saves as files next to the exe
 * through the `studio_api` its preload script exposes (fs-extra, path, and an ipcRenderer that
 * answers getAppPath and getProcess): here they are kept with the saves, by file name.
 */
function studioApi(items: Map<string, string>, changed: Changed) {
  const key = (p: string) => `${FILE}studio/${String(p).split(/[\\/]/).pop() ?? ""}`;
  const process = {
    platform: "win32",
    execPath: "C:\\game\\game.exe",
    env: {},
    __dirname: "C:\\game\\resources\\app",
  };
  return {
    fs: {
      // folders exist; of the files, only the saves
      existsSync: (p: string) => (/\.sav$/i.test(p) ? items.has(key(p)) : !/\.[^./\\]*$/.test(String(p))),
      mkdirSync: () => undefined,
      readFileSync: (p: string) => items.get(key(p)) ?? "",
      writeFileSync: (p: string, data: string) => (items.set(key(p), String(data)), changed(key(p))),
      unlinkSync: (p: string) => void (items.delete(key(p)) && changed(key(p))),
    },
    path: nodePath,
    ipcRenderer: {
      sendSync: (channel: string) =>
        channel === "getAppPath" ? process.__dirname : channel === "getProcess" ? process : undefined,
    },
  };
}

// Plugins that see `require` read process.mainModule.filename for the game folder. This
// `process` is a function, so the RPG Maker cores' NW.js check (typeof process === "object")
// fails and they keep to web storage.
const nodeProcess = () =>
  Object.assign(function process() {}, {
    mainModule: { filename: "index.html" },
    execPath: "game.exe",
    platform: "win32",
    env: {},
    argv: [],
    versions: {},
    cwd: () => ".",
    exit: () => undefined,
    on: () => undefined,
  });

/** The game's folder: its URL on this origin and its files' paths (from the app's init). */
interface Folder {
  base: string;
  names: Set<string>;
}

const items = new Map<string, string>();
const folder: Folder = { base: "", names: new Set() };
let app: string | null = null;
const post = (m: FromFrame) => parent.postMessage(m, app ?? "*");
const changed: Changed = (key) => post({ ippai: "set", key, value: items.get(key) ?? null });

// Electron games' pages are under resources/app/ (they run without Node: contextIsolation,
// and a global `process` stops a PixiJS game's title buttons), unless the game asks for Node
// (nodeIntegration, as older Electron did by default: #ippai-node); NW.js ones beside the exe
const node =
  !/\/resources\/app\//i.test(decodeURIComponent(location.pathname)) || location.hash === "#ippai-node";
Object.defineProperty(window, "localStorage", { value: storage(items, changed) });
if (node) Object.assign(window, { require: nodeRequire(folder, items, changed), process: nodeProcess() });
else Object.assign(window, { studio_api: studioApi(items, changed) });

// DOMContentLoaded and load wait for the saves (the game reads them from its handlers)
let ready = false;
const held: Event[] = [];
const hold = (e: Event) => {
  if (ready) return;
  e.stopImmediatePropagation();
  held.push(e);
};
addEventListener("DOMContentLoaded", hold, true);
addEventListener("load", hold, true);
addEventListener("message", (e: MessageEvent) => {
  const m = e.data as Init | undefined;
  if (e.source !== parent || m?.ippai !== "init" || ready) return;
  app = e.origin;
  for (const [k, v] of m.items) items.set(k, v);
  // (only a folder on this frame's own origin: the reads go nowhere else)
  if (m.base && new URL(m.base, location.href).origin === location.origin) {
    folder.base = m.base;
    for (const n of m.names) folder.names.add(n);
  }
  ready = true;
  for (const ev of held)
    if (ev.type === "load") dispatchEvent(new Event("load"));
    else document.dispatchEvent(new Event("DOMContentLoaded", { bubbles: true }));
});
post({ ippai: "hello" });

// a plugin that throws while it loads makes RPG Maker show the error instead of the game,
// which the release runs past: until the page has loaded, those only go to the console
const plugin = (e: ErrorEvent) => {
  if (!/\/js\/plugins\//.test(`${e.filename} ${(e.error as Error | undefined)?.stack}`)) return;
  console.warn("plugin error while loading:", e.error ?? e.message);
  e.stopImmediatePropagation();
  e.preventDefault();
};
addEventListener("error", plugin, true);
addEventListener("load", () => removeEventListener("error", plugin, true));
// RPG Maker fits the screen to the window only in NW.js: a 1920x1080 game would overflow
addEventListener("load", () => {
  const g = (window as { Graphics?: { _defaultStretchMode?: () => boolean } }).Graphics;
  if (g?._defaultStretchMode) g._defaultStretchMode = () => true;
});
