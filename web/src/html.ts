// Games that are web pages already (NW.js or Electron: TyranoScript, RPG Maker MV/MZ): when the
// program's folder (or its resources/app) has a package.json (ippai pack unpacks one appended to
// the exe: crates/ippai/src/nwjs.rs), the page opens the game's own start page in an iframe
// instead of running the exe. The frame is on another origin (ippai serve's game listener, see
// `GET /game-origin`) and sandboxed, so the game's scripts cannot reach this page, the saves of
// other games or the server's other routes: its pages start with frame.ts (frame.js), which
// asks this page for the game's saves and sends back what the game writes, and nothing else
// (frame-protocol.ts).
import { electronNode, electronPages } from "./electron";
import { fromFrame, type Init } from "./frame-protocol";
import { loadSaves, storeSaves } from "./saves";
import { SAVE_SOON_MS, SaveItems } from "./webstore";

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

/** Where the server's game listener is (crates/ippai/src/serve.rs). */
async function gameOrigin(): Promise<string> {
  const r = await fetch(new URL("game-origin", location.href));
  if (!r.ok) throw new Error(`game-origin: HTTP ${r.status}`);
  const { port, url } = (await r.json()) as { port: number; url: string | null };
  return new URL(url ?? `${location.protocol}//${location.hostname}:${port}`).origin;
}

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
  // (Electron: the main script names the page it opens, SKYNovel's doc/app/index.htm, #234)
  const named: string[] = [];
  if (/\.[cm]?js$/i.test(main)) {
    const code = await fetch(base + enc(main))
      .then((r) => (r.ok ? r.text() : ""))
      .catch(() => "");
    named.push(...electronPages(main, code));
  }
  for (const page of [main, ...named, "www/index.html", "index.html"]) {
    if (!/\.html?$/i.test(page)) continue;
    const h = await fetch(base + enc(page), { method: "HEAD" }).catch(() => null);
    if (h?.ok) return base + enc(page);
  }
  return null;
}

/** Whether the Electron game in `dir` (with resources/app/) runs its page with Node. */
async function electronPageNode(dir: string): Promise<boolean> {
  const text = async (url: string) => {
    const r = await fetch(url).catch(() => null);
    return r?.ok ? r.text() : null;
  };
  const app = `${dir}resources/app/`;
  let main = "index.js";
  try {
    const m = (JSON.parse((await text(`${app}package.json`)) ?? "{}") as { main?: unknown }).main;
    if (typeof m === "string" && m) main = m.replace(/^\.\//, "");
  } catch (e) {
    console.warn("package.json:", e);
  }
  if (!/\.[cm]?js$/i.test(main)) main += ".js";
  const code = await text(app + enc(main));
  return code !== null && electronNode(code, await text(`${dir}version`));
}

/**
 * Opens the program as a web game when it is one; false when it is not (sake runs it).
 * `saveName` names its saves (as sake's), `oldSaves` their name before the rename (Z:\vnvm).
 */
export async function startWebGame(
  run: string,
  mounts: string[],
  saveName: string | null,
  log: (line: string) => void,
  oldSaves: string | null = null,
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
  // (older Electron games, as TyranoScript v5's, have Node in the page: saves through fs)
  const electronWithNode = !node && (await electronPageNode(root + enc(at.dir)));
  log("Web ゲームとして開いています");
  const origin = await gameOrigin();

  // saves: the page's localStorage (and fs writes), as one JSON blob
  const saves = new SaveItems();
  const items = saves.map;
  // (none yet: those of before the rename, Z:\vnvm, if any; the next save goes under the new name)
  let saved = saveName ? await loadSaves(`${saveName}-web`) : null;
  if (saveName && !saved?.length && oldSaves) saved = await loadSaves(`${oldSaves}-web`);
  if (saved?.length) {
    try {
      for (const [k, v] of Object.entries(
        JSON.parse(new TextDecoder().decode(saved)) as Record<string, string>,
      ))
        if (typeof v === "string") saves.set(k, v);
    } catch (e) {
      console.warn("saves:", e);
    }
  }
  let generation = 0;
  let savedGeneration = 0;
  let pending: Promise<void> | undefined;
  const save = (): Promise<void> => {
    if (pending) return pending.then(save);
    if (generation === savedGeneration || !saveName) return Promise.resolve();
    const current = generation;
    pending = storeSaves(
      `${saveName}-web`,
      new TextEncoder().encode(JSON.stringify(Object.fromEntries(items))),
    )
      .then(() => {
        savedGeneration = current;
      })
      .catch((e: unknown) => {
        log(`セーブを保存できません: ${String(e)}`);
      })
      .finally(() => {
        pending = undefined;
      });
    return pending;
  };
  setInterval(() => void save(), 3000);
  // (soon after a change too: a phone's page put away may be frozen before the next interval)
  let soon: ReturnType<typeof setTimeout> | undefined;
  let refused = false;
  addEventListener("visibilitychange", () => document.visibilityState === "hidden" && void save());
  addEventListener("pagehide", () => void save());

  // NW.js games read their files synchronously (Node's fs): the frame reads them from the
  // folder when asked (frame.ts, sync-read.ts); it gets the folder's URL and file names
  const base = node ? `${origin}/tree/${encodeURIComponent(at.tree)}/${enc(at.dir)}` : "";
  const names: string[] = [];
  if (node) {
    const response = await fetch(new URL(`store/trees/${encodeURIComponent(at.tree)}.tsv`, location.href));
    if (!response.ok) throw new Error(`ゲーム一覧の取得: HTTP ${response.status}`);
    const paths = (await response.text())
      .split("\n")
      .filter((line) => line.startsWith("f\t"))
      .map((line) => line.split("\t")[3]!)
      .filter((path) => path.startsWith(at.dir));
    for (const path of paths) names.push(path.slice(at.dir.length));
  }

  const frame = document.createElement("iframe");
  frame.id = "frame";
  frame.allow = "autoplay; fullscreen";
  // (another origin already: the sandbox keeps it from navigating this page and the like)
  frame.sandbox.add(
    "allow-scripts",
    "allow-same-origin",
    "allow-forms",
    "allow-pointer-lock",
    "allow-popups",
    "allow-modals",
  );
  frame.src = `${origin}/tree/${encodeURIComponent(at.tree)}/${page.slice(root.length)}${electronWithNode ? "#ippai-node" : ""}`;
  addEventListener("message", (e: MessageEvent) => {
    if (e.source !== frame.contentWindow || e.origin !== origin) return;
    const m = fromFrame(e.data);
    if (m?.ippai === "hello") {
      // (each page of the game asks: a reload gets the saves as they are now)
      const init: Init = { ippai: "init", items: [...items], base, names };
      frame.contentWindow!.postMessage(init, origin);
    } else if (m?.ippai === "set") {
      if (!saves.set(m.key, m.value)) {
        if (!refused) log("ゲームのセーブが大きすぎるため、これ以上は保存しません");
        refused = true;
        return;
      }
      generation++;
      soon ??= setTimeout(() => {
        soon = undefined;
        void save();
      }, SAVE_SOON_MS);
    }
  });
  document.querySelector<HTMLCanvasElement>("#screen")!.hidden = true;
  document.body.append(frame);
  frame.addEventListener("load", () => {
    log("");
    frame.focus();
    dispatchEvent(new Event("vnvm-frame"));
  });
  return true;
}
