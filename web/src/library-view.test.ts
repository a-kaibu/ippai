import { describe, expect, it } from "vite-plus/test";
import type { GameEntry } from "./launcher";
import type { Session } from "./library-data";
import {
  duration,
  groupGames,
  GROUPS,
  matches,
  orderGames,
  parseView,
  playTotals,
  SORTS,
  sortGames,
  visibleGames,
  year,
  type GroupKey,
  type SortKey,
} from "./library-view";

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

// games with and without brand, release, score and reading, for the sort and group tests
const m = (title: string, o: Partial<NonNullable<GameEntry["meta"]>>) => ({ source: "egs", title, ...o });
const shelf = [
  game("k1", "きみの声", m("きみの声", { brand: "Key", release: "2004-06-25", score: 88, votes: 900 })),
  game("k2", "あの空", m("あの空", { brand: "Key", release: "2011-09-30", score: 92, votes: 500 })),
  game("y1", "夜明け", m("夜明け", { brand: "Yuzusoft", release: "2011-02-25", score: 92, votes: 1200 })),
  game("n1", "無名", undefined),
  game("a1", "Zeta", m("Zeta", { brand: "august", furigana: "わ", release: "2008" })),
  game("s0", "空白", m("空白", { brand: "  ", release: "unknown", score: 0 })),
];
const ids = (l: GameEntry[]) => l.map((g) => g.id);

function shuffle<T>(list: T[], r: () => number): T[] {
  const out = [...list];
  for (let i = out.length - 1; i > 0; i--) {
    const j = Math.floor(r() * (i + 1));
    [out[i], out[j]] = [out[j], out[i]];
  }
  return out;
}

describe("orderGames", () => {
  it("orders by score, highest first, more votes first on a tie, unscored last", () => {
    const unscored = shelf.filter((g) => !g.meta?.score);
    expect(ids(orderGames(shelf, "score", {}))).toEqual([
      "y1",
      "k2",
      "k1",
      ...ids(orderGames(unscored, "title", {})),
    ]);
  });
  it("orders by release, newest first, undated last", () => {
    expect(ids(orderGames(shelf, "release", {})).slice(0, 4)).toEqual(["k2", "y1", "a1", "k1"]);
    expect(
      ids(orderGames(shelf, "release", {}))
        .slice(4)
        .sort(),
    ).toEqual(["n1", "s0"]);
  });
  it("orders by brand (blank brands last), oldest first within a brand", () => {
    const out = ids(orderGames(shelf, "brand", {}));
    expect(out.indexOf("k1")).toBe(out.indexOf("k2") - 1);
    expect(out.slice(-2).sort()).toEqual(["n1", "s0"]);
    expect(out.slice(0, 4)).toEqual(
      ["a1", "k1", "k2", "y1"].sort(
        (x, y) =>
          shelf
            .find((g) => g.id === x)!
            .meta!.brand!.localeCompare(shelf.find((g) => g.id === y)!.meta!.brand!, "ja") ||
          x.localeCompare(y),
      ),
    );
  });
  it("orders by the reading when there is one", () => {
    // Zeta reads わ: after あの空 and きみの声 (by its title it would come first)
    const out = ids(orderGames(shelf, "title", {}));
    expect(out.indexOf("a1")).toBeGreaterThan(out.indexOf("k1"));
    expect(out.indexOf("k1")).toBeGreaterThan(out.indexOf("k2"));
  });
  it("recent puts the last played first, as sortGames does", () => {
    const last = { n1: 9, k1: 3 };
    expect(ids(orderGames(shelf, "recent", last)).slice(0, 2)).toEqual(["n1", "k1"]);
    expect(ids(sortGames(shelf, last)).slice(0, 2)).toEqual(["n1", "k1"]);
  });
  it("is one order whatever the input's order, and keeps the input as it was (random cases)", () => {
    const r = rng(23);
    const brands = ["Key", "Yuzusoft", "august", "", undefined];
    for (let n = 0; n < 300; n++) {
      const games = Array.from({ length: Math.floor(r() * 14) }, (_, i) =>
        game(
          `g${i}`,
          `t${Math.floor(r() * 4)}`,
          r() < 0.2
            ? undefined
            : m(`t${i}`, {
                brand: brands[Math.floor(r() * brands.length)],
                release:
                  r() < 0.7 ? `20${10 + Math.floor(r() * 5)}-0${1 + Math.floor(r() * 9)}-15` : undefined,
                score: r() < 0.7 ? Math.floor(r() * 4) * 10 : undefined,
                votes: Math.floor(r() * 3),
              }),
        ),
      );
      const last = Object.fromEntries(games.filter(() => r() < 0.4).map((g) => [g.id, Math.floor(r() * 3)]));
      for (const { key } of SORTS) {
        const before = ids(games);
        const out = orderGames(games, key, last);
        expect(ids(games)).toEqual(before);
        expect(ids(out).sort()).toEqual([...before].sort());
        expect(ids(orderGames(shuffle(games, r), key, last))).toEqual(ids(out));
        for (let i = 1; i < out.length; i++) {
          const [p, q] = [out[i - 1], out[i]];
          if (key === "score") {
            const s = (g: GameEntry) => ((g.meta?.score ?? 0) > 0 ? g.meta!.score! : -1);
            expect(s(p)).toBeGreaterThanOrEqual(s(q));
          }
          if (key === "release") {
            const d = (g: GameEntry) => g.meta?.release ?? "";
            if (d(q)) expect(d(p) >= d(q) && !!d(p)).toBe(true);
          }
          if (key === "brand" && p.meta?.brand?.trim() === "") expect(q.meta?.brand?.trim() || "").toBe("");
          if (key === "recent") expect(last[p.id] ?? 0).toBeGreaterThanOrEqual(last[q.id] ?? 0);
        }
      }
    }
  });
});

describe("groupGames", () => {
  it("splits by brand in name order, the brandless last", () => {
    const groups = groupGames(orderGames(shelf, "score", {}), "brand");
    expect(groups.map((g) => g.label)).toEqual([
      ...["Key", "Yuzusoft", "august"].sort((a, b) => a.localeCompare(b, "ja")),
      "ブランド不明",
    ]);
    expect(ids(groups.find((g) => g.key === "Key")!.games)).toEqual(["k2", "k1"]);
  });
  it("splits by year, newest first, the undated last", () => {
    const groups = groupGames(orderGames(shelf, "title", {}), "year");
    expect(groups.map((g) => g.label)).toEqual(["2011年", "2008年", "2004年", "発売年不明"]);
    expect(ids(groups[0].games).sort()).toEqual(["k2", "y1"]);
    expect(year(shelf[4])).toBe("2008");
    expect(year(shelf[5])).toBe("");
  });
  it("none is one section, and an empty list has none", () => {
    expect(groupGames(shelf, "none")).toEqual([{ key: "", label: "", games: shelf }]);
    for (const { key } of GROUPS) expect(groupGames([], key)).toEqual([]);
  });
  it("partitions the list, keeping its order in each section (random cases)", () => {
    const r = rng(31);
    for (let n = 0; n < 300; n++) {
      const list = shuffle(shelf, r).filter(() => r() < 0.8);
      for (const { key } of GROUPS) {
        const groups = groupGames(list, key as GroupKey);
        expect(ids(groups.flatMap((g) => g.games)).sort()).toEqual(ids(list).sort());
        expect(new Set(groups.map((g) => g.key)).size).toBe(groups.length);
        for (const g of groups) {
          expect(g.games.length).toBeGreaterThan(0);
          const pos = g.games.map((x) => list.indexOf(x));
          expect(pos).toEqual([...pos].sort((a, b) => a - b));
        }
        const blank = groups.findIndex((g) => !g.key);
        if (key !== "none" && blank >= 0) expect(blank).toBe(groups.length - 1);
      }
    }
  });
});

describe("parseView", () => {
  it("reads a kept view and falls back on anything else", () => {
    expect(parseView('{"sort":"score","group":"brand"}')).toEqual({ sort: "score", group: "brand" });
    expect(parseView('{"sort":"score","group":"planet"}')).toEqual({ sort: "score", group: "none" });
    for (const t of [null, "", "{", "null", "[]", "1", '"score"', '{"sort":1}'])
      expect(parseView(t)).toEqual({ sort: "recent", group: "none" });
  });
  it("always gives a known view (random text)", () => {
    const r = rng(5);
    const parts = [
      '{"sort":',
      '"score"',
      '"brand"',
      ',"group":',
      '"year"',
      "}",
      "[",
      "null",
      "__proto__",
      '"',
    ];
    for (let n = 0; n < 1000; n++) {
      const t = Array.from({ length: Math.floor(r() * 6) }, () => parts[Math.floor(r() * parts.length)]).join(
        "",
      );
      const v = parseView(t);
      expect(SORTS.map((s) => s.key)).toContain(v.sort as SortKey);
      expect(GROUPS.map((g) => g.key)).toContain(v.group);
    }
  });
});
