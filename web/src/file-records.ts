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
const prefix = "/vnvm/prefix/drive_c/";
export function installationPath(path: string): boolean {
  const p = path.toLowerCase();
  return (
    p.startsWith(prefix) &&
    !p.split("/").includes("..") &&
    !/\.(exe|dll|ocx)$/i.test(p) &&
    !p.includes("/temp/")
  );
}
export function portablePath(path: string): boolean {
  const p = path.toLowerCase();
  return (
    !/\.(exe|dll|ocx)$/i.test(p) && !p.includes("/microsoft/crypto/") && !p.endsWith("/sake-registry.tsv")
  );
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
