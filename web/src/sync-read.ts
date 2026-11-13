// NW.js games read their own files synchronously (Node's fs.readFileSync) from the page. The
// frame reads them when asked, with a synchronous XMLHttpRequest on its own origin (the game
// listener serves the tree), instead of the app sending the whole folder ahead: a 950 MB RPG
// Maker MV folder sent ahead (fetched, kept, structured-cloned into the frame) took the tab
// past 8 GB and the browser killed it.

/** The bytes of a response read as text in "x-user-defined" (each byte one char, 0x80-0xFF as U+F780-U+F7FF). */
export function bytesOf(text: string): Uint8Array {
  const b = new Uint8Array(text.length);
  for (let i = 0; i < text.length; i++) b[i] = text.charCodeAt(i) & 0xff;
  return b;
}

/** What readSync needs of an XMLHttpRequest. */
export interface SyncRequest {
  open(method: string, url: string, async: boolean): void;
  overrideMimeType(mime: string): void;
  send(): void;
  readonly status: number;
  readonly responseText: string;
}

/** The file at `url`, read synchronously; null when the server has none (or it cannot be read). */
export function readSync(
  url: string,
  make: () => SyncRequest = () => new XMLHttpRequest(),
): Uint8Array | null {
  const r = make();
  try {
    r.open("GET", url, false);
    // (bytes as they are: a synchronous request cannot take responseType "arraybuffer")
    r.overrideMimeType("text/plain; charset=x-user-defined");
    r.send();
  } catch {
    return null;
  }
  return r.status === 200 ? bytesOf(r.responseText) : null;
}

/** The URL of `path` (in the game's folder, "/"-separated) under the folder's URL `base`. */
export const fileUrl = (base: string, path: string) =>
  base + path.split("/").map(encodeURIComponent).join("/");
