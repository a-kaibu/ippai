// What the game list (ui/Library.tsx) and its statistics show, without React: the order, the
// search and the filters, and the play time per game from the server's sessions.

import type { GameEntry } from "./launcher";
import type { Session } from "./library-data";

export type Filter = "all" | "favorites";

/** How the list is ordered: last played here, title (by its reading), brand, release (newest
 * first) or score (highest first). */
export type SortKey = "recent" | "title" | "brand" | "release" | "score";
/** How the list is split into sections: not at all, by brand or by release year. */
export type GroupKey = "none" | "brand" | "year";

export const SORTS: readonly { key: SortKey; label: string }[] = [
  { key: "recent", label: "最近遊んだ順" },
  { key: "title", label: "タイトル順" },
  { key: "brand", label: "ブランド順" },
  { key: "release", label: "発売日が新しい順" },
  { key: "score", label: "スコアが高い順" },
];
export const GROUPS: readonly { key: GroupKey; label: string }[] = [
  { key: "none", label: "区分けなし" },
  { key: "brand", label: "ブランドごと" },
  { key: "year", label: "発売年ごと" },
];

export interface ListView {
  sort: SortKey;
  group: GroupKey;
}
export const DEFAULT_VIEW: ListView = { sort: "recent", group: "none" };

/** A kept view (localStorage), anything unknown read as the default. */
export function parseView(text: string | null): ListView {
  let v: unknown;
  try {
    v = JSON.parse(text ?? "null");
  } catch {
    return DEFAULT_VIEW;
  }
  const o = (typeof v === "object" && v ? v : {}) as Partial<Record<keyof ListView, unknown>>;
  return {
    sort: SORTS.some((s) => s.key === o.sort) ? (o.sort as SortKey) : DEFAULT_VIEW.sort,
    group: GROUPS.some((g) => g.key === o.group) ? (o.group as GroupKey) : DEFAULT_VIEW.group,
  };
}

const ja = (a: string, b: string) => a.localeCompare(b, "ja");
const cmp = (a: string, b: string) => (a < b ? -1 : a > b ? 1 : 0);
const reading = (g: GameEntry) => g.meta?.furigana || g.meta?.title || g.title;
/** the game's brand as the list groups and filters by it ("" for none) */
export const brandOf = (g: GameEntry): string => g.meta?.brand?.trim() || "";
const brand = brandOf;
/** yyyy-mm-dd, or "" when there is no usable date */
const release = (g: GameEntry) => /^\d{4}(-\d{2}(-\d{2})?)?/.exec(g.meta?.release ?? "")?.[0] ?? "";
const score = (g: GameEntry) => (typeof g.meta?.score === "number" && g.meta.score > 0 ? g.meta.score : -1);
/** the release year, or "" */
export const year = (g: GameEntry): string => release(g).slice(0, 4);

/** Games without the sort's value go last; ties by title, then id (one order for any input). */
export function orderGames(games: GameEntry[], key: SortKey, last: Record<string, number>): GameEntry[] {
  const title = (a: GameEntry, b: GameEntry) =>
    ja(reading(a), reading(b)) || ja(a.title, b.title) || cmp(a.id, b.id);
  const missingLast = (x: string, y: string) => (!x === !y ? 0 : x ? -1 : 1);
  const by: Record<SortKey, (a: GameEntry, b: GameEntry) => number> = {
    recent: (a, b) => (last[b.id] ?? 0) - (last[a.id] ?? 0),
    title: () => 0,
    brand: (a, b) => missingLast(brand(a), brand(b)) || ja(brand(a), brand(b)) || cmp(release(a), release(b)),
    release: (a, b) => missingLast(release(a), release(b)) || cmp(release(b), release(a)),
    score: (a, b) => score(b) - score(a) || (b.meta?.votes ?? 0) - (a.meta?.votes ?? 0),
  };
  return [...games].sort((a, b) => by[key](a, b) || title(a, b));
}

export interface GameGroup {
  /** "" for the games without the value (they come last) */
  key: string;
  label: string;
  games: GameEntry[];
}

/** The ordered list in sections: brands in name order, years newest first, the games without a
 * brand or year last; each section keeps the list's order. `none` is one section of all. */
export function groupGames(ordered: GameEntry[], group: GroupKey): GameGroup[] {
  if (group === "none") return ordered.length ? [{ key: "", label: "", games: ordered }] : [];
  const keyOf = group === "brand" ? brand : year;
  const by = new Map<string, GameEntry[]>();
  for (const g of ordered) {
    const k = keyOf(g);
    const list = by.get(k);
    if (list) list.push(g);
    else by.set(k, [g]);
  }
  const keys = [...by.keys()].sort((a, b) =>
    !a !== !b ? (a ? -1 : 1) : group === "brand" ? ja(a, b) : cmp(b, a),
  );
  return keys.map((k) => ({
    key: k,
    label: k ? (group === "year" ? `${k}年` : k) : group === "brand" ? "ブランド不明" : "発売年不明",
    games: by.get(k)!,
  }));
}

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

/** The games shown: matching the search, a favorite under お気に入り, and of `brand` when one
 * was picked (a brand's name clicked: brandOf's value, exactly). */
export function visibleGames(
  sorted: GameEntry[],
  query: string,
  filter: Filter,
  favorites: readonly string[],
  brand: string | null = null,
): GameEntry[] {
  return sorted.filter(
    (g) =>
      matches(g, query) &&
      (filter === "all" || favorites.includes(g.id)) &&
      (brand === null || brandOf(g) === brand),
  );
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
