// Messages between the app's page (web/src/html.ts) and a web game's frame (web/src/frame.ts),
// which runs on another origin (ippai serve's game listener) and reaches nothing of the app's
// but through these.

/** frame → page */
export type FromFrame =
  /** the frame's script runs: send the saves */
  | { ippai: "hello" }
  /** one item of the saves changed (null: removed) */
  | { ippai: "set"; key: string; value: string | null };

/** page → frame */
export interface Init {
  ippai: "init";
  /** the saves: localStorage, and files the game wrote (FILE + path, base64) */
  items: [string, string][];
  /** NW.js: the game folder's URL on the frame's own origin, where its synchronous fs reads
   * (sync-read.ts), and the paths of the folder's files ("/"-separated) */
  base: string;
  names: string[];
}

/** Keys of the saves that hold files the game wrote with Node's fs (base64). */
export const FILE = "\0file:";

/** Whether `m` is a well-formed message from the frame (anything else is ignored). */
export function fromFrame(m: unknown): FromFrame | null {
  if (typeof m !== "object" || m === null) return null;
  const o = m as Record<string, unknown>;
  if (o.ippai === "hello") return { ippai: "hello" };
  if (o.ippai === "set" && typeof o.key === "string" && (typeof o.value === "string" || o.value === null))
    return { ippai: "set", key: o.key, value: o.value };
  return null;
}
