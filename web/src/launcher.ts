// The game list's data (wine.html without ?run=; shown by ui/Library.tsx): the games vnvm
// add-game put in the store, served as one list by vnvm serve (GET /games). Picking one fills
// wine.html's parameters (run, mount, args, env) and starts it in the same page, from the same
// click (the audio and fullscreen need a user gesture).

/** From ErogameScape (批評空間) / VNDB: `vnvm meta` (crates/vnvm/src/meta.rs, Info) */
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

/** store/games/<id>.json (crates/vnvm/src/pack.rs, GameEntry) */
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
  /** "x86_64": a 64-bit program */
  arch?: string;
  meta?: GameMeta;
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

/** Sets wine.html's parameters for `g` (other parameters, e.g. &debug, stay). */
export function applyGame(params: URLSearchParams, g: GameEntry): void {
  params.set("run", g.run);
  params.delete("mount");
  for (const m of g.mounts) params.append("mount", m);
  if (g.args.length) params.set("args", g.args.join(","));
  else params.delete("args");
  for (const e of g.env) params.append("env", e);
  if (g.arch) params.set("arch", g.arch);
  else params.delete("arch");
  markPlayed(g.id);
}
