// The pieces of a web game's saves and folder (html.ts on the app's page, frame.ts in the game's
// frame) that are kept apart to be tested.

/** A path as the game's fs takes it: "/"-separated, without ".", "..", or empty parts. */
export function normPath(p: string): string {
  const out: string[] = [];
  for (const s of String(p).replaceAll("\\", "/").split("/")) {
    if (s === "..") out.pop();
    else if (s && s !== ".") out.push(s);
  }
  return out.join("/");
}

/** Every folder that holds one of `names` (files' paths), "" for the top when there are any:
 * existsSync of a folder looks here, not through the whole list each time (RPG Maker MV/MZ
 * trees hold 10-30k files and plugins ask for each asset). */
export function folderDirs(names: Iterable<string>): Set<string> {
  const dirs = new Set<string>();
  for (const n of names) {
    dirs.add("");
    for (let i = n.indexOf("/"); i >= 0; i = n.indexOf("/", i + 1)) {
      const d = n.slice(0, i);
      if (d) dirs.add(d);
    }
  }
  return dirs;
}

/** The characters a web game's saves may hold in all (keys and values): a game or a plugin that
 * writes in a loop is refused past this, not kept in the app page's memory and the save. */
export const SAVES_MAX = 32 << 20;
/** As many keys at most */
export const SAVE_KEYS_MAX = 50_000;

/** The saves as the app's page keeps them: a Map, and what they hold in all. */
export class SaveItems {
  readonly map = new Map<string, string>();
  /** characters of every key and value */
  total = 0;

  /** Sets (or with null removes) one; false when that would go past the limits (nothing changed). */
  set(key: string, value: string | null): boolean {
    const old = this.map.get(key);
    const was = old === undefined ? 0 : key.length + old.length;
    if (value === null) {
      if (old === undefined) return true;
      this.map.delete(key);
      this.total -= was;
      return true;
    }
    const now = key.length + value.length;
    if (this.total - was + now > SAVES_MAX || (old === undefined && this.map.size >= SAVE_KEYS_MAX))
      return false;
    this.map.set(key, value);
    this.total += now - was;
    return true;
  }
}

/** How long after a change the saves are stored: soon, as a page put away on a phone may be
 * frozen before an asynchronous store lands (before: every 3 s). */
export const SAVE_SOON_MS = 300;
/** How long the frame waits for the app's saves, asking again meanwhile, before its page goes on
 * without them (then nothing it writes is kept: the saves of before stay as they are). */
export const HELLO_WAIT_MS = 15_000;
/** When the frame asks again for the saves, ms after its first ask */
export const HELLO_AGAIN = [500, 1000, 2000, 4000, 8000];
