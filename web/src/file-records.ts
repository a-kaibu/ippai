// VFS records are path length, UTF-8 path, mtime, data length, data (little endian).
export function selectRecords(
  data: Uint8Array | null,
  keep: (path: string) => boolean,
): Uint8Array<ArrayBuffer> | null {
  if (!data) return null;
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const parts: Uint8Array[] = [];
  let size = 0;
  for (let at = 0; at < data.length;) {
    if (at + 16 > data.length) throw new Error("ファイル保存データが破損しています。");
    const n = view.getUint32(at, true);
    if (at + 16 + n > data.length) throw new Error("ファイル保存データが破損しています。");
    const len = view.getUint32(at + 12 + n, true);
    const end = at + 16 + n + len;
    if (end > data.length) throw new Error("ファイル保存データが破損しています。");
    const path = new TextDecoder("utf-8", { fatal: true }).decode(data.subarray(at + 4, at + 4 + n));
    if (keep(path)) {
      const part = data.subarray(at, end);
      parts.push(part);
      size += part.length;
    }
    at = end;
  }
  const out = new Uint8Array(size);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}
const prefix = "/ippai/prefix/drive_c/";
const users = `${prefix}users/`;
export const registryPath = `${users}user/sake-registry.tsv`;
const secret = (p: string) => p.includes("/microsoft/crypto/");
const binary = (p: string) => /\.(exe|dll|ocx)$/i.test(p);
/**
 * This browser's machine: what the prefix keeps outside the user folders (C:\ProgramData,
 * C:\windows, C:\Program Files: installed runtimes' data) and the CryptoAPI key containers,
 * which never leave the device. The user folders (game saves, the registry) are each game's
 * own saves; the two never hold the same path, so neither overrides the other.
 */
export function installationPath(path: string): boolean {
  const p = path.toLowerCase();
  return (
    p.startsWith(prefix) &&
    !p.split("/").includes("..") &&
    !binary(p) &&
    !p.includes("/temp/") &&
    (!p.startsWith(users) || secret(p))
  );
}
/** A game's saves: everything it wrote but executables and key containers. */
export function portablePath(path: string): boolean {
  const p = path.toLowerCase();
  return !binary(p) && !secret(p);
}
/** The registry of an older installation record (8ab1b50 kept it there, not in the saves). */
export function legacyRegistry(path: string): boolean {
  return path.toLowerCase() === registryPath;
}
/** The saves, with `registry` first when they have none of their own. */
export function withRegistry(
  saved: Uint8Array<ArrayBuffer> | null,
  registry: Uint8Array<ArrayBuffer> | null,
): Uint8Array<ArrayBuffer> | null {
  if (!registry?.length || selectRecords(saved, legacyRegistry)?.length) return saved;
  return joinRecords(registry, saved);
}
export function joinRecords(
  a: Uint8Array<ArrayBuffer> | null,
  b: Uint8Array<ArrayBuffer> | null,
): Uint8Array<ArrayBuffer> | null {
  if (!a?.length) return b;
  if (!b?.length) return a;
  const out = new Uint8Array(a.length + b.length);
  out.set(a);
  out.set(b, a.length);
  return out;
}
/** Paths of before the rename (/vnvm/game, /vnvm/prefix) as they are now (/ippai/...). */
export function movedPath(path: string): string {
  return path.replace(/^\/vnvm\/(game|prefix)(?=\/|$)/, "/ippai/$1");
}
/**
 * The records with their paths moved by `move` (the rest of each record as it was); null for
 * null. Saves and installations written before the rename read as written after it.
 */
export function renameRecords(
  data: Uint8Array | null,
  move: (path: string) => string = movedPath,
): Uint8Array<ArrayBuffer> | null {
  if (!data) return null;
  const view = new DataView(data.buffer, data.byteOffset, data.byteLength);
  const parts: Uint8Array[] = [];
  let size = 0;
  for (let at = 0; at < data.length;) {
    if (at + 16 > data.length) throw new Error("ファイル保存データが破損しています。");
    const n = view.getUint32(at, true);
    if (at + 16 + n > data.length) throw new Error("ファイル保存データが破損しています。");
    const len = view.getUint32(at + 12 + n, true);
    const end = at + 16 + n + len;
    if (end > data.length) throw new Error("ファイル保存データが破損しています。");
    const path = new TextDecoder("utf-8", { fatal: true }).decode(data.subarray(at + 4, at + 4 + n));
    const to = move(path);
    if (to === path) {
      parts.push(data.subarray(at, end));
    } else {
      const p = new TextEncoder().encode(to);
      const part = new Uint8Array(16 + p.length + len);
      new DataView(part.buffer).setUint32(0, p.length, true);
      part.set(p, 4);
      part.set(data.subarray(at + 4 + n, end), 4 + p.length);
      parts.push(part);
    }
    size += parts[parts.length - 1]!.length;
    at = end;
  }
  const out = new Uint8Array(size);
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}
/** The saves' name of before the rename for a program and its mounts (play.ts saveName). */
export function legacyNames(run: string, mounts: string[]): { run: string; mounts: string[] } | null {
  const r = run.replace(/^Z:\\ippai\\/i, "Z:\\vnvm\\");
  const m = mounts.map((x) => x.replace(/^\/ippai\/(game|prefix)(?=[/=]|$)/, "/vnvm/$1"));
  return r === run && m.every((x, i) => x === mounts[i]) ? null : { run: r, mounts: m };
}
