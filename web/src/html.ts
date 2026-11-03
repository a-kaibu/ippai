// Games that are web pages already (NW.js or Electron: TyranoScript, RPG Maker MV/MZ): when the
// program's folder (or its resources/app) has a package.json (ippai pack unpacks one appended to the exe: crates/ippai/src/nwjs.rs),
// the page opens the game's own start page in an iframe instead of running the exe. Its files
// come from `/tree/<tree>/<path>` (crates/ippai/src/treefiles.rs), which starts every page with
// a call to parent.vnvmFrame(window): before any script of the game, that gives the page a
// localStorage kept with vnvm's saved games and the Node stand-ins NW.js games reach for.
import { loadSaves, storeSaves } from "./saves";

/** The program's tree and folder in it, from &run and the &mount that holds it. */
function where(run: string, mounts: string[]): { tree: string; dir: string } | null {
  const host = run.replace(/^z:/i, "").replaceAll("\\", "/");
  for (const m of mounts) {
    const at = m.indexOf("=");
    const [prefix, tree] = [m.slice(0, at).replace(/\/$/, ""), m.slice(at + 1)];
    if (!host.toLowerCase().startsWith(`${prefix.toLowerCase()}/`)) continue;
    const rel = host.slice(prefix.length + 1);
    return { tree, dir: rel.includes("/") ? rel.slice(0, rel.lastIndexOf("/") + 1) : "" };
  }
  return null;
}

const enc = (path: string) => path.split("/").map(encodeURIComponent).join("/");

/** The start page's URL (package.json's main), or null when the program is no web game. */
async function startPage(base: string): Promise<string | null> {
  const r = await fetch(`${base}package.json`).catch(() => null);
  if (!r?.ok) return null;
  let main = "";
  try {
    const m = ((await r.json()) as { main?: unknown }).main;
    if (typeof m === "string") main = m.replace(/^app:\/\/[^/]*\//, "").replace(/^\.\//, "");
  } catch (e) {
    console.warn("package.json:", e);
  }
  for (const page of [main, "www/index.html", "index.html"]) {
    if (!/\.html?$/i.test(page)) continue;
    const h = await fetch(base + enc(page), { method: "HEAD" }).catch(() => null);
    if (h?.ok) return base + enc(page);
  }
  return null;
}

/** A Storage over `items` (string keys and values); `changed` after each write. */
function storage(items: Map<string, string>, changed: () => void): Storage {
  const keys = () => [...items.keys()].filter((k) => !k.startsWith(FILE));
  const api: Storage = {
    get length() {
      return keys().length;
    },
    key: (i: number) => keys()[i] ?? null,
    getItem: (key: string) => items.get(String(key)) ?? null,
    setItem: (key: string, value: string) => (items.set(String(key), String(value)), changed()),
    removeItem: (key: string) => void (items.delete(String(key)) && changed()),
    clear: () => (keys().forEach((k) => items.delete(k)), changed()),
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

/** Files the game writes with Node's fs: kept with the saves under this key prefix (base64). */
const FILE = "\0file:";

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
 * Node's require for NW.js games that call it from the page: synchronous fs (reads from the
 * game's tree, writes as saves) and path; nw.gui and the like do nothing.
 */
function nodeRequire(root: string, items: Map<string, string>, changed: () => void) {
  const read = (p: string): Uint8Array | null => {
    const saved = items.get(FILE + norm(p));
    if (saved !== undefined) return Uint8Array.from(atob(saved), (c) => c.charCodeAt(0));
    // (synchronous, as Node's: the page waits for the server)
    const x = new XMLHttpRequest();
    x.open("GET", root + enc(norm(p)), false);
    x.overrideMimeType("text/plain; charset=x-user-defined");
    x.send();
    if (x.status !== 200) return null;
    return Uint8Array.from(x.responseText, (c) => c.charCodeAt(0) & 0xff);
  };
  const exists = (p: string) => {
    if (items.has(FILE + norm(p))) return true;
    const x = new XMLHttpRequest();
    x.open("HEAD", root + enc(norm(p)), false);
    x.send();
    // (folders: no extension)
    return x.status === 200 || !/\.[^./\\]*$/.test(String(p));
  };
  const encoding = (opt: unknown) =>
    typeof opt === "string" ? opt : (opt as { encoding?: string } | undefined)?.encoding;
  const write = (p: string, data: unknown) => {
    const b = typeof data === "string" ? new TextEncoder().encode(data) : new Uint8Array(data as ArrayBuffer);
    let s = "";
    for (const c of b) s += String.fromCharCode(c);
    items.set(FILE + norm(p), btoa(s));
    changed();
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
    unlinkSync: (p: string) => void (items.delete(FILE + norm(p)) && changed()),
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
function studioApi(items: Map<string, string>, changed: () => void) {
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
      writeFileSync: (p: string, data: string) => (items.set(key(p), String(data)), changed()),
      unlinkSync: (p: string) => void (items.delete(key(p)) && changed()),
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

/**
 * Opens the program as a web game when it is one; false when it is not (sake runs it).
 * `saveName` names its saves (as sake's).
 */
export async function startWebGame(
  run: string,
  mounts: string[],
  saveName: string | null,
  log: (line: string) => void,
): Promise<boolean> {
  const at = where(run, mounts);
  if (!at) return false;
  const root = new URL(`tree/${encodeURIComponent(at.tree)}/`, location.href).href;
  // NW.js: package.json beside the exe; Electron: resources/app (its pages run without Node:
  // contextIsolation, and a global `process` stops a PixiJS game's title buttons)
  let page = await startPage(root + enc(at.dir));
  const node = !!page;
  page ??= await startPage(root + enc(`${at.dir}resources/app/`));
  if (!page) return false;
  log("Web ゲームとして開いています");

  // saves: the page's localStorage (and fs writes), as one JSON blob
  const items = new Map<string, string>();
  const saved = saveName ? await loadSaves(`${saveName}-web`).catch(() => null) : null;
  if (saved) {
    try {
      for (const [k, v] of Object.entries(
        JSON.parse(new TextDecoder().decode(saved)) as Record<string, string>,
      ))
        items.set(k, v);
    } catch (e) {
      console.warn("saves:", e);
    }
  }
  let dirty = false;
  const save = () => {
    if (!dirty || !saveName) return;
    dirty = false;
    void storeSaves(`${saveName}-web`, new TextEncoder().encode(JSON.stringify(Object.fromEntries(items))));
  };
  const changed = () => (dirty = true);
  setInterval(save, 3000);
  addEventListener("visibilitychange", () => document.visibilityState === "hidden" && save());
  addEventListener("pagehide", save);

  const localStorage = storage(items, changed);
  const studio = studioApi(items, changed);
  const require = nodeRequire(root + enc(at.dir), items, changed);
  Object.assign(window, {
    vnvmFrame(win: Window) {
      Object.defineProperty(win, "localStorage", { value: localStorage });
      if (node) Object.assign(win, { require, process: nodeProcess() });
      else Object.assign(win, { studio_api: studio });
      // a plugin that throws while it loads makes RPG Maker show the error instead of the
      // game, which the release runs past: until the page has loaded, those only go to the console
      const plugin = (e: ErrorEvent) => {
        if (!/\/js\/plugins\//.test(`${e.filename} ${(e.error as Error | undefined)?.stack}`)) return;
        console.warn("plugin error while loading:", e.error ?? e.message);
        e.stopImmediatePropagation();
        e.preventDefault();
      };
      win.addEventListener("error", plugin, true);
      win.addEventListener("load", () => win.removeEventListener("error", plugin, true));
      // RPG Maker fits the screen to the window only in NW.js: a 1920x1080 game would overflow
      win.addEventListener("load", () => {
        const g = (win as { Graphics?: { _defaultStretchMode?: () => boolean } }).Graphics;
        if (g?._defaultStretchMode) g._defaultStretchMode = () => true;
      });
    },
  });

  const frame = document.createElement("iframe");
  frame.id = "frame";
  frame.allow = "autoplay; fullscreen";
  frame.src = page;
  document.querySelector<HTMLCanvasElement>("#screen")!.hidden = true;
  document.body.append(frame);
  frame.addEventListener("load", () => {
    log("");
    frame.focus();
    dispatchEvent(new Event("vnvm-frame"));
  });
  return true;
}
