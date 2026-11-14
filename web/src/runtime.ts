// Where play.ts loads sake and a program's saved x86 regions from, and their versions: shared
// with offline.ts, which fetches the same URLs ahead so that a game saved to the device starts
// offline (sw.ts keeps them).

/** sake's files (crates/sake/build.sh → public/sake/) */
export const sakeBase = () => new URL("sake/", location.href).href;

/**
 * The sake build this device runs. sake.wasm has the whole 4 GB memory from the start and never
 * grows it (V8 then checks each access against a size it loads once per function, which matters
 * on Android: no trap handler there, docs/translator.md); sake-grow.wasm starts at 2.06 GB and
 * grows (crates/sake/build.sh), for a device that refuses 4 GB at once. The page tries the 4 GB
 * memory itself and hands it to sake.js (`wasmMemory`), so it is allocated once. `jit`: what
 * the translator's version gets for saved regions (the modules declare the memory they link
 * to, koji::jit::wasm::memory_min_pages). `&memory=grow` forces the second.
 */
export interface SakeBuild {
  name: "sake" | "sake-grow";
  memory?: WebAssembly.Memory;
  jit: "" | "g";
}

const BUILD_KEY = "masu-sake-build";
/** When sake.wasm last failed to start on this device with its 4 GB memory (Date.now()). */
const FAILED_KEY = "masu-sake-4g-failed";
/** How long a failed start keeps this device on sake-grow (a browser update may fix it). */
export const FAILED_KEEP_MS = 30 * 24 * 3600_000;

/** When 4 GB last failed here (0: never, or no storage to say). */
function failedAt(): number {
  try {
    return typeof localStorage === "undefined" ? 0 : Number(localStorage.getItem(FAILED_KEY)) || 0;
  } catch {
    return 0;
  }
}

/** `&memory=`: "grow" forces sake-grow, "full" tries the 4 GB memory even after it failed here. */
export function sakeBuild(force?: string | null, now = Date.now()): SakeBuild {
  let build: SakeBuild = { name: "sake-grow", jit: "g" };
  const failed = failedAt();
  const stay = force !== "full" && failed > 0 && now - failed < FAILED_KEEP_MS;
  if (force !== "grow" && !stay) {
    try {
      const memory = new WebAssembly.Memory({ initial: 65535, maximum: 65535, shared: true });
      build = { name: "sake", memory, jit: "" };
    } catch {
      // (RangeError: the browser could not reserve it)
    }
  }
  try {
    localStorage.setItem(BUILD_KEY, build.name);
  } catch {
    // (no storage: offline.ts saves sake.wasm then)
  }
  return build;
}

/**
 * The memory could be had, and yet sake.wasm did not start with it: the instance could not be
 * made (a phone that reserved 4 GB but cannot commit what it needs, an import the browser
 * refuses), or sake aborted before its main ran. For a fresh start of sake.wasm with the
 * page's 4 GB memory (`build.memory`), not one already on sake-grow, not a saved state (its
 * memory is the state's, laid out for the build it was taken with: savestate.ts), the page
 * starts again on sake-grow: the address to go to, with `&memory=grow` (a reload throws the
 * 4 GB away with everything else of this start), and the device remembers it for
 * FAILED_KEEP_MS. Otherwise null: the page says it could not start, as before.
 */
export function growRetry(
  href: string,
  o: { build: SakeBuild; fromState: boolean; started: boolean },
  now = Date.now(),
): string | null {
  if (o.build.name !== "sake" || !o.build.memory || o.fromState || o.started) return null;
  const url = new URL(href);
  if (url.searchParams.get("memory") === "grow") return null;
  url.searchParams.set("memory", "grow");
  try {
    localStorage.setItem(FAILED_KEY, String(now));
  } catch {
    // (no storage: the next start tries 4 GB again)
  }
  return url.href;
}

/** The build this device ran last (offline.ts saves that one), without trying a memory. */
export function lastSakeBuild(): SakeBuild {
  let name: string | null = null;
  try {
    name = localStorage.getItem(BUILD_KEY);
  } catch {
    // (as below)
  }
  return name === "sake-grow" ? { name, jit: "g" } : { name: "sake", jit: "" };
}

/** `url`'s version as masu serve tells it (Last-Modified and length), "" when out of reach. */
export function fileVersion(url: string): Promise<string> {
  return fetch(url, { method: "HEAD" }).then(
    (r) => `${r.headers.get("last-modified") ?? ""}-${r.headers.get("content-length") ?? ""}`,
    () => "",
  );
}

/** `url` with its version: the browser keeps it for good (and sw.ts on the device). */
export const versioned = (url: string, version: string) =>
  version ? `${url}?v=${encodeURIComponent(version)}` : url;

/**
 * The translator's version: jit-version.txt (a hash of its sources, which crates/sake/build.sh
 * writes; saved regions stay good across builds that change anything else), else one from
 * sake.wasm's version.
 */
export function translatorVersion(wasmVersion: Promise<string>): Promise<string> {
  return fetch(`${sakeBase()}jit-version.txt`, { cache: "no-cache" })
    .then((r) => (r.ok ? r.text() : Promise.reject(new Error(`HTTP ${r.status}`))))
    .then((t) => (/^[0-9a-f]{8}$/.test(t.trim()) ? t.trim() : Promise.reject(new Error("bad version"))))
    .catch(() => wasmVersion.then((v) => fnv(new TextEncoder().encode(v))));
}

/** The name `run`'s saved regions go by (masu serve /jit), before the translator's version. */
export const jitBase = (run: string) => `sake-${run.replace(/[^A-Za-z0-9_.-]/g, "_")}`;

/** The name of game `game`'s list of the store chunks its runs read, in order (masu serve
 * /jit/<name>.chunks; the page fetches them ahead). A game's own, not its program's: games start
 * the same program (every SiglusEngine.exe at Z:\masu\game) and read other chunks. Not per
 * build either: a new sake does not change what the game reads. */
export const gameChunksName = (game: string) => `game.${game.replace(/[^A-Za-z0-9_-]/g, "_")}`;

/** A run's read trace's name (masu serve keeps them in jit/traces/: kura vfs/trace.rs): the
 * game's, and the run's start (ms, base 36). */
export const traceName = (game: string, start: number) =>
  `trace.${gameChunksName(game)}.${start.toString(36)}`;

/** FNV-1a of `bytes`, in hex. */
export function fnv(bytes: Uint8Array): string {
  let h = 0x811c9dc5;
  for (const b of bytes) h = Math.imul(h ^ b, 0x01000193);
  return (h >>> 0).toString(16);
}
