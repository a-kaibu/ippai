// The game list's search (library-view.ts): Japanese-aware and forgiving. Text and query are
// both normalized (NFKC: full-width ＡＢＣ/１２３ and half-width ｶﾅ fold; lower case; katakana
// as hiragana, so アネモイ, あねもい and ｱﾈﾓｲ are one; spaces and punctuation dropped), then each
// word of the query must be found in a game's title, reading, brand, series, engine, id or tags:
// as it is (best), as the start of a title, in order with gaps in a title or its reading
// ("あもい" for アネモイ), or with a typo or two (one per 4 characters; a swap of two counts as one: "あねもえ", "reunoin").

/** One form for comparing: NFKC, lower case, katakana as hiragana, no spaces or punctuation. */
export function normalize(s: string): string {
  return s
    .normalize("NFKC")
    .toLowerCase()
    .replace(/[ァ-ヶ]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0x60))
    .replace(/[^\p{L}\p{N}]/gu, "");
}

/** The query's words, normalized (empty ones dropped). */
export function queryWords(query: string): string[] {
  return query
    .split(/[\s　]+/)
    .map(normalize)
    .filter(Boolean);
}

/** Fewest edits (insert, delete, change, swapping two neighbours) turning `w` into some
 * substring of `text` (Sellers' search with Damerau's swap). */
export function substringDistance(w: string, text: string): number {
  const a = Array.from(w);
  const b = Array.from(text);
  // col[i]: edits for a[0..i] ending at the current position of b (a start anywhere is free)
  let prev: number[] = [];
  let col = Array.from({ length: a.length + 1 }, (_, i) => i);
  let best = col[a.length];
  for (let j = 0; j < b.length; j++) {
    const next = [0];
    for (let i = 1; i <= a.length; i++) {
      next[i] = Math.min(col[i] + 1, next[i - 1] + 1, col[i - 1] + (a[i - 1] === b[j] ? 0 : 1));
      if (i > 1 && j > 0 && a[i - 1] === b[j - 1] && a[i - 2] === b[j])
        next[i] = Math.min(next[i], prev[i - 2] + 1);
    }
    prev = col;
    col = next;
    best = Math.min(best, col[a.length]);
  }
  return best;
}

/** `w`'s characters in order in `text`, the span they cover at most 3 times `w`'s length. */
export function subsequence(w: string, text: string): boolean {
  const a = Array.from(w);
  const b = Array.from(text);
  for (let start = 0; start < b.length; start++) {
    if (b[start] !== a[0]) continue;
    let i = 1;
    let j = start + 1;
    for (; i < a.length && j < b.length && j - start < a.length * 3; j++) if (b[j] === a[i]) i++;
    if (i === a.length) return true;
  }
  return false;
}

/** How many typos a word of `n` characters may have. */
export const allowedTypos = (n: number): number => Math.floor(n / 4);

export interface Searchable {
  /** title and reading: where gaps and prefixes count */
  names: string[];
  /** everything else that is searched */
  other: string[];
}

/** How well one normalized word fits (0: not at all; 100 exact in a name, ...). */
export function wordScore(w: string, s: Searchable): number {
  const names = s.names.map(normalize).filter(Boolean);
  const all = [...names, ...s.other.map(normalize).filter(Boolean)];
  if (names.some((n) => n.startsWith(w))) return 100;
  if (names.some((n) => n.includes(w))) return 90;
  if (all.some((t) => t.includes(w))) return 80;
  if (Array.from(w).length >= 2 && names.some((n) => subsequence(w, n))) return 50;
  const typos = allowedTypos(Array.from(w).length);
  if (typos) {
    const d = Math.min(...all.map((t) => substringDistance(w, t)));
    if (d <= typos) return 40 - 10 * d;
  }
  return 0;
}

/** How well the query fits (0: some word fits nowhere; an empty query fits everything: 1). */
export function searchScore(query: string, s: Searchable): number {
  const words = queryWords(query);
  if (!words.length) return 1;
  let total = 0;
  for (const w of words) {
    const sc = wordScore(w, s);
    if (!sc) return 0;
    total += sc;
  }
  return total / words.length;
}
