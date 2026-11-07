import { describe, expect, it } from "vite-plus/test";
import type { GameEntry } from "./launcher";
import type { Session } from "./library-data";
import { duration, matches, playTotals, sortGames, visibleGames } from "./library-view";

const game = (id: string, title: string, meta?: GameEntry["meta"]): GameEntry => ({
  id,
  title,
  run: "C:\\game.exe",
  args: [],
  mounts: [],
  env: [],
  meta,
});

const a = game("a", "アネモイ", { source: "egs", title: "アネモイ", brand: "ensemble", tags: ["純愛"] });
const b = game("b", "Reunion", { source: "vndb", title: "Reunion", furigana: "りゆにおん", series: "R" });
const c = game("c", "かえで", undefined);

// a small seeded generator (the same cases every run)
function rng(seed: number) {
  return () => (seed = (seed * 1103515245 + 12345) >>> 0) / 2 ** 32;
}

describe("sortGames", () => {
  it("puts the last played first, then orders by title", () => {
    expect(sortGames([a, b, c], {}).map((g) => g.id)).toEqual(
      [a, b, c].sort((x, y) => x.title.localeCompare(y.title, "ja")).map((g) => g.id),
    );
    expect(sortGames([a, b, c], { c: 5, b: 9 }).map((g) => g.id)).toEqual(["b", "c", "a"]);
  });
  it("is a permutation that keeps the input as it was (random lists)", () => {
    const r = rng(7);
    for (let n = 0; n < 200; n++) {
      const games = Array.from({ length: Math.floor(r() * 12) }, (_, i) =>
        game(`g${i}`, `t${Math.floor(r() * 5)}`),
      );
      const last = Object.fromEntries(games.filter(() => r() < 0.5).map((g) => [g.id, Math.floor(r() * 4)]));
      const before = games.map((g) => g.id);
      const out = sortGames(games, last);
      expect(games.map((g) => g.id)).toEqual(before);
      expect(out.map((g) => g.id).sort()).toEqual([...before].sort());
      for (let i = 1; i < out.length; i++)
        expect(last[out[i - 1].id] ?? 0).toBeGreaterThanOrEqual(last[out[i].id] ?? 0);
    }
  });
});

describe("matches", () => {
  it("finds a game by title, reading, brand, series, tag or id, in any case", () => {
    for (const q of ["アネモイ", "ENSEMBLE", "純愛", "a"]) expect(matches(a, q)).toBe(true);
    for (const q of ["りゆ", "reunion", "r"]) expect(matches(b, q)).toBe(true);
    expect(matches(c, "ensemble")).toBe(false);
  });
  it("needs every word, and an empty query matches everything", () => {
    expect(matches(a, "  ensemble 純愛 ")).toBe(true);
    expect(matches(a, "ensemble reunion")).toBe(false);
    expect(matches(c, "   ")).toBe(true);
  });
});

describe("visibleGames", () => {
  it("keeps only favorites with the favorites filter, in the given order", () => {
    expect(visibleGames([c, b, a], "", "favorites", ["a", "c"]).map((g) => g.id)).toEqual(["c", "a"]);
    expect(visibleGames([c, b, a], "", "all", []).map((g) => g.id)).toEqual(["c", "b", "a"]);
    expect(visibleGames([c, b, a], "ensemble", "favorites", ["c"])).toEqual([]);
  });
  it("is a subset of the list for any query and filter (random cases)", () => {
    const r = rng(11);
    const all = [a, b, c];
    const words = ["", "a", "ensemble", "りゆ", "x", "R"];
    for (let n = 0; n < 200; n++) {
      const favs = all.filter(() => r() < 0.5).map((g) => g.id);
      const q = words[Math.floor(r() * words.length)];
      const f = r() < 0.5 ? "all" : "favorites";
      const out = visibleGames(all, q, f, favs);
      for (const g of out) {
        expect(matches(g, q)).toBe(true);
        if (f === "favorites") expect(favs).toContain(g.id);
      }
      expect(out.length).toBe(
        all.filter((g) => matches(g, q) && (f === "all" || favs.includes(g.id))).length,
      );
    }
  });
});

describe("playTotals", () => {
  const s = (game: string, started: number, ms: number): Session => ({
    id: `${game}-${started}`,
    game,
    started_ms: started,
    elapsed_ms: ms,
  });
  it("adds up each game's sessions, most played first, and drops empty ones", () => {
    expect(playTotals([s("a", 1, 60_000), s("b", 2, 30_000), s("a", 5, 30_000), s("c", 9, 0)])).toEqual([
      { game: "a", ms: 90_000, sessions: 2, last: 5 },
      { game: "b", ms: 30_000, sessions: 1, last: 2 },
    ]);
  });
  it("keeps the total of all sessions (random cases)", () => {
    const r = rng(3);
    for (let n = 0; n < 200; n++) {
      const list = Array.from({ length: Math.floor(r() * 20) }, (_, i) =>
        s(`g${Math.floor(r() * 4)}`, i, Math.floor(r() * 3) * 1000),
      );
      const totals = playTotals(list);
      expect(totals.reduce((x, t) => x + t.ms, 0)).toBe(list.reduce((x, t) => x + t.elapsed_ms, 0));
      expect(new Set(totals.map((t) => t.game)).size).toBe(totals.length);
      for (let i = 1; i < totals.length; i++) expect(totals[i - 1].ms).toBeGreaterThanOrEqual(totals[i].ms);
    }
  });
});

describe("duration", () => {
  it("reads as hours and minutes", () => {
    expect(duration(0)).toBe("1分未満");
    expect(duration(59_999)).toBe("1分未満");
    expect(duration(60_000)).toBe("1分");
    expect(duration(59 * 60_000)).toBe("59分");
    expect(duration(3_600_000)).toBe("1時間 0分");
    expect(duration(3 * 3_600_000 + 5 * 60_000 + 59_000)).toBe("3時間 5分");
    expect(duration(-5)).toBe("1分未満");
  });
});
