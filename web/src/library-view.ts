// What the game list (ui/Library.tsx) and its statistics show, without React: the order, the
// search and the filters, and the play time per game from the server's sessions.

import type { GameEntry } from "./launcher";
import type { Session } from "./library-data";

export type Filter = "all" | "favorites";

/** Last played first (this browser), then by title. */
export function sortGames(games: GameEntry[], last: Record<string, number>): GameEntry[] {
  return [...games].sort(
    (a, b) => (last[b.id] ?? 0) - (last[a.id] ?? 0) || a.title.localeCompare(b.title, "ja"),
  );
}

/** Every word of `query` in the title, reading, brand, series, engine, id or a tag (any case). */
export function matches(g: GameEntry, query: string): boolean {
  const words = query.trim().toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const m = g.meta;
  const key = [g.title, g.engine, g.id, m?.title, m?.furigana, m?.brand, m?.series, ...(m?.tags ?? [])]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return words.every((w) => key.includes(w));
}

export function visibleGames(
  sorted: GameEntry[],
  query: string,
  filter: Filter,
  favorites: readonly string[],
): GameEntry[] {
  return sorted.filter((g) => matches(g, query) && (filter === "all" || favorites.includes(g.id)));
}

export interface PlayTotal {
  game: string;
  ms: number;
  sessions: number;
  /** when the newest session started */
  last: number;
}

/** Play time per game, most played first; sessions without time are left out. */
export function playTotals(sessions: readonly Session[]): PlayTotal[] {
  const by = new Map<string, PlayTotal>();
  for (const s of sessions) {
    if (!(s.elapsed_ms > 0)) continue;
    const t = by.get(s.game) ?? { game: s.game, ms: 0, sessions: 0, last: 0 };
    t.ms += s.elapsed_ms;
    t.sessions++;
    t.last = Math.max(t.last, s.started_ms);
    by.set(s.game, t);
  }
  return [...by.values()].sort((a, b) => b.ms - a.ms || b.last - a.last || a.game.localeCompare(b.game));
}

/** "3時間 5分", "12分", "1分未満" */
export function duration(ms: number): string {
  const min = Math.floor(Math.max(0, ms) / 60_000);
  if (!min) return "1分未満";
  const h = Math.floor(min / 60);
  return h ? `${h}時間 ${min % 60}分` : `${min}分`;
}

/** "11/7 09:05" */
export function when(t: number): string {
  const d = new Date(t);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getMonth() + 1}/${d.getDate()} ${p(d.getHours())}:${p(d.getMinutes())}`;
}
