// Where play.ts loads sake and a program's saved x86 regions from, and their versions: shared
// with offline.ts, which fetches the same URLs ahead so that a game saved to the device starts
// offline (sw.ts keeps them).

/** sake's files (crates/sake/build.sh → public/sake/) */
export const sakeBase = () => new URL("sake/", location.href).href;

/** `url`'s version as ippai serve tells it (Last-Modified and length), "" when out of reach. */
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

/** The name `run`'s saved regions go by (ippai serve /jit), before the translator's version. */
export const jitBase = (run: string) => `sake-${run.replace(/[^A-Za-z0-9_.-]/g, "_")}`;

/** FNV-1a of `bytes`, in hex. */
export function fnv(bytes: Uint8Array): string {
  let h = 0x811c9dc5;
  for (const b of bytes) h = Math.imul(h ^ b, 0x01000193);
  return (h >>> 0).toString(16);
}
