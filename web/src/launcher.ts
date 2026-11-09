// The game list's data (play.html without ?run=; shown by ui/Library.tsx): the games `ippai add` (or
// add-game) put in the store, served as one list by ippai serve (GET /games). Picking one fills
// play.html's parameters (run, mount, args, env) and starts it in the same page, from the same
// click (the audio and fullscreen need a user gesture).

/** From ErogameScape (批評空間) / VNDB: `ippai meta` (crates/ippai/src/meta.rs, Info) */
export interface GameMeta {
  source: string;
  egs_id?: number;
  vndb_id?: string;
  title: string;
  furigana?: string;
  brand?: string;
  /** yyyy-mm-dd */
  release?: string;
  /** 0-100 */
  score?: number;
  votes?: number;
  tags?: string[];
  series?: string;
  description?: string;
  links?: { label: string; url: string }[];
}

/** store/games/<id>.json (crates/ippai/src/pack.rs, GameEntry) */
export interface GameEntry {
  id: string;
  title: string;
  engine?: string;
  /** image file in store/games/ */
  cover?: string;
  run: string;
  args: string[];
  /** /path=tree */
  mounts: string[];
  /** NAME=value */
  env: string[];
  meta?: GameMeta;
  /** what did not unpack of the download it was added from (`ippai add` of an archive a tool
   * failed on in part: crates/ippai/src/unpack.rs) */
  unpack_issues?: string[];
}

const PLAYED = "vnvm-played";

/** When each game was last started here, by id (this browser only). */
export function played(): Record<string, number> {
  try {
    return JSON.parse(localStorage.getItem(PLAYED) ?? "{}") as Record<string, number>;
  } catch {
    return {};
  }
}

function markPlayed(id: string): void {
  try {
    localStorage.setItem(PLAYED, JSON.stringify({ ...played(), [id]: Date.now() }));
  } catch {
    // (storage blocked: the order just doesn't remember)
  }
}

export async function fetchGames(): Promise<GameEntry[]> {
  const r = await fetch(new URL("games", location.href));
  if (!r.ok) throw new Error(`games: HTTP ${r.status}`);
  return (await r.json()) as GameEntry[];
}

export function coverUrl(g: GameEntry): string | null {
  return g.cover ? new URL(`store/games/${encodeURIComponent(g.cover)}`, location.href).href : null;
}

/** Sets play.html's parameters for `g` (other parameters, e.g. &debug, stay). */
export function applyGame(params: URLSearchParams, g: GameEntry): void {
  params.set("game", g.id);
  params.set("run", g.run);
  params.delete("mount");
  for (const m of g.mounts) params.append("mount", m);
  if (g.args.length) params.set("args", g.args.join(","));
  else params.delete("args");
  params.delete("env");
  for (const e of g.env) params.append("env", e);
  markPlayed(g.id);
}

/** The page for the program a game started in its own place (a launcher's game: sake's
 * k32::exec): the running page's parameters as they are now (`params`, which applyGame filled
 * for a game picked from the list: its mounts, env, ...; the address bar may hold only ?game=),
 * with that program and its arguments. A memory state stays with the program it was taken of. */
export function nextRunUrl(href: string, params: URLSearchParams, run: string, args: string): URL {
  const url = new URL(href);
  const next = new URLSearchParams(params);
  next.set("run", run);
  const list = args.split(/\s+/).filter(Boolean);
  if (list.length) next.set("args", list.join(","));
  else next.delete("args");
  next.delete("state");
  url.search = `?${next}`;
  return url;
}
