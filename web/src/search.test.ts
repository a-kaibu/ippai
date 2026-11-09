import { describe, expect, it } from "vite-plus/test";
import {
  allowedTypos,
  normalize,
  queryWords,
  searchScore,
  subsequence,
  substringDistance,
  wordScore,
  type Searchable,
} from "./search";

function rng(seed: number) {
  return () => (seed = (seed * 1103515245 + 12345) >>> 0) / 2 ** 32;
}
const toKata = (s: string) => s.replace(/[ぁ-ゖ]/g, (c) => String.fromCharCode(c.charCodeAt(0) + 0x60));

const anemoi: Searchable = { names: ["アネモイ", "あねもい"], other: ["Key", "純愛", "KiriKiri"] };
const sakura: Searchable = {
  names: ["サクラノ詩 －櫻の森の上を舞う－", "さくらのうた"],
  other: ["枕", "感動"],
};
const reunion: Searchable = { names: ["Reunion"], other: ["大人の道楽"] };

describe("normalize", () => {
  it("folds width, case, kana and punctuation", () => {
    expect(normalize("ＡＢＣ１２３")).toBe("abc123");
    expect(normalize("ｱﾈﾓｲ")).toBe("あねもい");
    expect(normalize("アネモイ")).toBe("あねもい");
    expect(normalize("ヴァ")).toBe("ゔぁ");
    expect(normalize("サクラノ詩 －櫻の森の上を舞う－")).toBe("さくらの詩櫻の森の上を舞う");
    expect(normalize("Fate/stay night")).toBe("fatestaynight");
    expect(normalize("ゆーす")).toBe("ゆーす");
  });
  it("is idempotent and blind to kana kind (random text)", () => {
    const r = rng(2);
    const pool = Array.from("あいうえおかがアイウエオカガｱｲｳｴｵｶﾞabcABCＡＢＣ123１２３ 　・ー－!?漢字");
    for (let n = 0; n < 1000; n++) {
      const t = Array.from({ length: Math.floor(r() * 12) }, () => pool[Math.floor(r() * pool.length)]).join(
        "",
      );
      expect(normalize(normalize(t))).toBe(normalize(t));
      expect(normalize(toKata(t))).toBe(normalize(t));
      expect(normalize(t)).not.toMatch(/[\sァ-ヶA-Z]/);
    }
  });
});

describe("queryWords", () => {
  it("splits on ASCII and full-width spaces", () => {
    expect(queryWords(" アネモイ　Key  ")).toEqual(["あねもい", "key"]);
    expect(queryWords(" ・ ")).toEqual([]);
  });
});

describe("substringDistance", () => {
  it("counts the edits to the best substring", () => {
    expect(substringDistance("あねもい", "xxあねもいxx")).toBe(0);
    expect(substringDistance("あねもえ", "あねもい")).toBe(1);
    expect(substringDistance("あもい", "あねもい")).toBe(1);
    expect(substringDistance("abc", "")).toBe(3);
    expect(substringDistance("reunoin", "reunion")).toBe(1);
    expect(substringDistance("あもねい", "あねもい")).toBe(1);
    expect(substringDistance("", "abc")).toBe(0);
  });
  it("is 0 exactly for substrings and at most the word's length (random cases)", () => {
    const r = rng(8);
    const abc = Array.from("abcあい");
    const str = (n: number) => Array.from({ length: n }, () => abc[Math.floor(r() * abc.length)]).join("");
    for (let n = 0; n < 2000; n++) {
      const w = str(Math.floor(r() * 6));
      const t = str(Math.floor(r() * 10));
      const d = substringDistance(w, t);
      expect(d === 0).toBe(t.includes(w));
      expect(d).toBeLessThanOrEqual(Array.from(w).length);
      // one change makes it at most one more
      if (w.length) {
        const i = Math.floor(r() * w.length);
        const w2 = w.slice(0, i) + "z" + w.slice(i + 1);
        expect(substringDistance(w2, t)).toBeLessThanOrEqual(d + 1);
      }
    }
  });
});

// the plain definition: the optimal-string-alignment distance to every substring, the least
function osa(a: string[], b: string[]): number {
  const d = Array.from({ length: a.length + 1 }, (_, i) =>
    Array.from({ length: b.length + 1 }, (_, j) => (i ? (j ? 0 : i) : j)),
  );
  for (let i = 1; i <= a.length; i++)
    for (let j = 1; j <= b.length; j++) {
      d[i][j] = Math.min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + (a[i - 1] === b[j - 1] ? 0 : 1));
      if (i > 1 && j > 1 && a[i - 1] === b[j - 2] && a[i - 2] === b[j - 1])
        d[i][j] = Math.min(d[i][j], d[i - 2][j - 2] + 1);
    }
  return d[a.length][b.length];
}
describe("substringDistance against the definition", () => {
  it("is the least distance to any substring (random cases)", () => {
    const r = rng(77);
    const abc = Array.from("abあい");
    const str = (n: number) => Array.from({ length: n }, () => abc[Math.floor(r() * abc.length)]);
    for (let n = 0; n < 3000; n++) {
      const w = str(Math.floor(r() * 6));
      const t = str(Math.floor(r() * 8));
      let best = w.length;
      for (let i = 0; i <= t.length; i++)
        for (let j = i; j <= t.length; j++) best = Math.min(best, osa(w, t.slice(i, j)));
      expect(substringDistance(w.join(""), t.join("")), `${w.join("")} in ${t.join("")}`).toBe(best);
    }
  });
});

describe("subsequence", () => {
  it("takes characters in order, within a short span", () => {
    expect(subsequence("あもい", "あねもい")).toBe(true);
    expect(subsequence("いあ", "あねもい")).toBe(false);
    expect(subsequence("さう", "さくらのうた")).toBe(true);
    expect(subsequence("ab", "a" + "x".repeat(10) + "b")).toBe(false);
  });
});

describe("wordScore / searchScore", () => {
  it("finds Japanese titles however they are typed", () => {
    for (const q of ["アネモイ", "あねもい", "ｱﾈﾓｲ", "あね", "ネモ", "ＫＥＹ", "key", "純愛", "kirikiri"])
      expect(searchScore(q, anemoi), q).toBeGreaterThan(0);
    for (const q of ["さくらの", "櫻の森", "さくらのうた", "サクラノウタ", "サクラノ詩", "枕"])
      expect(searchScore(q, sakura), q).toBeGreaterThan(0);
    expect(searchScore("REUNION", reunion)).toBeGreaterThan(0);
  });
  it("forgives a typo or gaps, and ranks exact above fuzzy", () => {
    expect(searchScore("あねもえ", anemoi)).toBeGreaterThan(0);
    expect(searchScore("あもい", anemoi)).toBeGreaterThan(0);
    expect(searchScore("reunoin", reunion)).toBeGreaterThan(0);
    const exact = searchScore("あねもい", anemoi);
    expect(exact).toBeGreaterThan(searchScore("あもい", anemoi));
    expect(searchScore("あもい", anemoi)).toBeGreaterThan(searchScore("あねもえ", anemoi));
    expect(searchScore("あね", anemoi)).toBeGreaterThan(searchScore("純愛", anemoi));
  });
  it("does not find what is not there", () => {
    for (const q of ["x", "月", "reunion", "アネモイ 月", "かえで", "えもい"])
      expect(searchScore(q, anemoi), q).toBe(0);
    expect(searchScore("ab", reunion)).toBe(0);
    expect(searchScore("", anemoi)).toBe(1);
  });
  it("any substring of a name is found, best kind, in either kana (random cases)", () => {
    const r = rng(17);
    for (let n = 0; n < 500; n++) {
      const s = [anemoi, sakura, reunion][Math.floor(r() * 3)];
      const name = normalize(s.names[Math.floor(r() * s.names.length)]);
      const chars = Array.from(name);
      const i = Math.floor(r() * chars.length);
      const w = chars.slice(i, i + 1 + Math.floor(r() * 4)).join("");
      expect(wordScore(w, s)).toBeGreaterThanOrEqual(90);
      expect(searchScore(toKata(w), s)).toBe(searchScore(w, s));
    }
  });
  it("a typo within the allowance still finds the name (random cases)", () => {
    const r = rng(29);
    const long: Searchable = { names: ["ぎんいろはるかのものがたり"], other: [] };
    const name = Array.from(normalize(long.names[0]));
    for (let n = 0; n < 300; n++) {
      const len = 4 + Math.floor(r() * 6);
      const i = Math.floor(r() * (name.length - len));
      const w = name.slice(i, i + len);
      const j = Math.floor(r() * len);
      w[j] = "ん" === w[j] ? "ぬ" : "ん";
      expect(allowedTypos(len)).toBeGreaterThanOrEqual(1);
      expect(searchScore(w.join(""), long), w.join("")).toBeGreaterThan(0);
    }
  });
  it("ranks a name's start above its middle, and fewer typos above more", () => {
    expect(wordScore("あね", anemoi)).toBeGreaterThan(wordScore("もい", anemoi));
    const long: Searchable = { names: ["ぎんいろはるかのものがたり"], other: [] };
    expect(wordScore("ぎんいろはるかの", long)).toBeGreaterThan(wordScore("ぎんいろはるかぬ", long));
    expect(wordScore("ぎんいろはるかぬ", long)).toBeGreaterThan(wordScore("ぎぬいろはるかぬ", long));
    expect(wordScore("ぎぬいろはるかぬ", long)).toBeGreaterThan(0);
  });
  it("needs every word", () => {
    expect(searchScore("あねもい key", anemoi)).toBeGreaterThan(0);
    expect(searchScore("あねもい 枕", anemoi)).toBe(0);
  });
});
