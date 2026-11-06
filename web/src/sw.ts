// The service worker (built as dist/sw.js by vite.config.ts, registered by pwa.ts): the app's
// own files stay on the device, so that the page opens with no connection (or with the server
// out of reach) and the games saved to the device (offline.ts: their store chunks, which net.ts
// keeps in its own cache and this leaves alone) play.
//
// - the app shell (pages, assets/, the manifest and icons) and the game list are fetched when a
//   build installs
// - versioned URLs (?v=: sake.js, sake.wasm, saved x86 regions) and assets/ never change: from
//   here first; a new version of a path drops the older ones
// - the rest (pages, /games, tree lists, jit lists, jit-version.txt) comes from the network, and
//   the copy here answers when the network fails, or does not answer within a few seconds
//   while there is a copy (a weak mobile link); covers: the copy at once, refreshed behind
// - HEAD (play.ts asks sake.js's and sake.wasm's version that way) answers from the headers last
//   seen, which are kept for it
// - a file that is not here while offline: 503 with x-ippai-offline, which net.ts does not retry
//
// Updates: a new build's sw.js installs on the next load online, takes over at once (skipWaiting:
// the old pages' files stay here until it activates and prunes assets/ to its own) and pwa.ts
// offers the library a reload. Online, pages and lists come from the server anyway, so the
// newest build is what an online load gets.

// (the parts of the worker types used here: the page's lib is DOM, which has no worker scope)
interface ExtendableEvent extends Event {
  waitUntil(p: Promise<unknown>): void;
}
interface FetchEvent extends ExtendableEvent {
  request: Request;
  respondWith(r: Response | Promise<Response>): void;
}
interface WorkerScope {
  registration: { scope: string };
  clients: { claim(): Promise<void> };
  skipWaiting(): Promise<void>;
  addEventListener(type: "install" | "activate", f: (e: ExtendableEvent) => void): void;
  addEventListener(type: "fetch", f: (e: FetchEvent) => void): void;
  addEventListener(type: "message", f: (e: MessageEvent) => void): void;
}
const sw = self as unknown as WorkerScope;

/** the build's id and shell files (vite.config.ts, relative to the scope) */
declare const IPPAI_BUILD: string;
declare const IPPAI_SHELL: string[];

const CACHE = "ippai-app-v1";
/** how long a request may take while there is a copy (ms) */
const PATIENCE = 4000;
/** after a failed request, misses count as offline this long (the server out of reach) */
const UNREACHABLE_MS = 30000;
/** never handled here: the chunks (net.ts's cache), saves, writes, logs and the web games' files */
const PASS = /^(store\/chunks\/|saves\/|api\/|jit-save\/|log-|logs\/|bench-log|tree\/|game-origin)/;

const scope = () => new URL(sw.registration.scope);
const at = (path: string) => new URL(path, scope()).href;
let failedAt = 0;

sw.addEventListener("install", (e) => {
  void sw.skipWaiting();
  e.waitUntil(
    caches.open(CACHE).then((c) =>
      // (each on its own: one missing file must not keep the build from installing)
      // (and the game list: the library's first load came before this worker)
      Promise.all(
        [...IPPAI_SHELL, "games"].map((p) =>
          fetch(at(p), { cache: "reload" })
            .then((r) => (r.ok ? c.put(key(at(p)), r) : undefined))
            .catch(() => undefined),
        ),
      ),
    ),
  );
});

sw.addEventListener("activate", (e) => {
  e.waitUntil(
    (async () => {
      await sw.clients.claim();
      // assets/ of older builds (content-hashed, so a page of this build never asks for them)
      const c = await caches.open(CACHE);
      const keep = new Set(IPPAI_SHELL.map(at));
      for (const r of await c.keys())
        if (new URL(r.url).pathname.startsWith(`${scope().pathname}assets/`) && !keep.has(r.url))
          await c.delete(r);
    })(),
  );
});

// pwa.ts asks whether the server was out of reach (the page then says it is offline)
sw.addEventListener("message", (e) => {
  if (e.data === "state")
    e.ports[0]?.postMessage({ build: IPPAI_BUILD, unreachable: Date.now() - failedAt < UNREACHABLE_MS });
});

sw.addEventListener("fetch", (e) => {
  const req = e.request;
  const url = new URL(req.url);
  const base = scope();
  if (url.origin !== base.origin || !url.pathname.startsWith(base.pathname)) return;
  if ((req.method !== "GET" && req.method !== "HEAD") || req.headers.has("range")) return;
  const path = url.pathname.slice(base.pathname.length);
  if (PASS.test(path) || path === "sw.js") return;
  const done = (p: Promise<unknown>) => e.waitUntil(p.catch(() => undefined));
  if (req.method === "HEAD") e.respondWith(head(req, done));
  else if (url.searchParams.has("v") || path.startsWith("assets/")) e.respondWith(kept(req, path));
  else if (path.startsWith("store/games/")) e.respondWith(stale(req, done));
  else e.respondWith(fresh(req, req.mode === "navigate", done));
});

/** The key a response is kept under: pages without their parameters (play.html?game=...). */
function key(href: string, page = false): string {
  const u = new URL(href);
  if (page) u.search = "";
  if (u.pathname === scope().pathname) u.pathname += "index.html";
  return u.href;
}

function offline(): Response {
  return new Response("offline: not on this device", {
    status: 503,
    headers: { "x-ippai-offline": "1", "content-type": "text/plain; charset=utf-8" },
  });
}

/** A miss with the network down: offline when the browser says so or the server was out of reach. */
function miss(error: unknown): Response {
  if (!navigator.onLine || Date.now() - failedAt < UNREACHABLE_MS) return offline();
  // (a dropped request on a working link: the page's retries take it from here)
  throw error;
}

async function network(req: Request): Promise<Response> {
  try {
    const r = await fetch(req);
    failedAt = 0;
    return r;
  } catch (e) {
    failedAt = Date.now();
    throw e;
  }
}

/** Keeps `r` (a full, same-origin 200) under `k`; for a versioned URL, drops its other versions. */
async function keep(k: string, r: Response): Promise<void> {
  if (r.status !== 200 || r.type !== "basic") return;
  const c = await caches.open(CACHE);
  await c.put(k, r);
  const u = new URL(k);
  if (!u.searchParams.has("v")) return;
  for (const old of await c.keys()) {
    const o = new URL(old.url);
    if (o.pathname === u.pathname && o.href !== u.href && o.searchParams.has("v")) await c.delete(old);
  }
}

/** Never changes: the copy, else the network (then kept). */
async function kept(req: Request, path: string): Promise<Response> {
  const hit = await caches.match(req.url, { cacheName: CACHE });
  if (hit) return hit;
  try {
    const r = await network(req);
    await keep(req.url, r.clone()).catch(() => undefined);
    return r;
  } catch (e) {
    // sake.js?v=...: pthread workers of a page that started before an update ask the old one
    if (path.startsWith("sake/")) {
      const any = await caches.match(req.url, { cacheName: CACHE, ignoreSearch: true });
      if (any) return any;
    }
    return miss(e);
  }
}

/** The network, or the copy when it fails or keeps us waiting (the network then still refreshes it). */
async function fresh(req: Request, page: boolean, done: (p: Promise<unknown>) => void): Promise<Response> {
  const k = key(req.url, page);
  const copy = async () =>
    (await caches.match(k, { cacheName: CACHE })) ??
    (page ? await caches.match(at("index.html"), { cacheName: CACHE }) : undefined);
  const got = network(req).then(async (r) => {
    await keep(k, r.clone()).catch(() => undefined);
    return r;
  });
  done(got);
  const have = await copy();
  if (!have) return got.catch(miss);
  let timer = 0;
  const late = new Promise<Response>((resolve) => (timer = setTimeout(() => resolve(have), PATIENCE)));
  try {
    return await Promise.race([got, late]);
  } catch {
    return have;
  } finally {
    clearTimeout(timer);
  }
}

/** The copy at once, refreshed from the network behind (covers). */
async function stale(req: Request, done: (p: Promise<unknown>) => void): Promise<Response> {
  const got = network(req).then(async (r) => {
    await keep(req.url, r.clone()).catch(() => undefined);
    return r;
  });
  done(got);
  return (await caches.match(req.url, { cacheName: CACHE })) ?? got.catch(miss);
}

/** HEAD: the network (its headers kept under `?ippai-head`), else the headers last seen. */
async function head(req: Request, done: (p: Promise<unknown>) => void): Promise<Response> {
  const u = new URL(req.url);
  u.searchParams.set("ippai-head", "1");
  const k = u.href;
  const got = network(req).then(async (r) => {
    if (r.ok) {
      const c = await caches.open(CACHE);
      await c.put(k, new Response(null, { headers: r.headers })).catch(() => undefined);
    }
    return r;
  });
  done(got);
  const have = await caches.match(k, { cacheName: CACHE });
  if (!have) return got.catch(miss);
  const copy = () => new Response(null, { status: 200, headers: have.headers });
  let timer = 0;
  const late = new Promise<Response>((resolve) => (timer = setTimeout(() => resolve(copy()), PATIENCE)));
  try {
    return await Promise.race([got, late]);
  } catch {
    return copy();
  } finally {
    clearTimeout(timer);
  }
}
