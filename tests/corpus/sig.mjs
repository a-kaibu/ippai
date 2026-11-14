// A screen's signature, as tests/corpus/shots.py sig (same integers: shots_test.py checks both on
// random pictures): SIG_X x SIG_Y cells, each the mean of SIG_S x SIG_S points, 4 bits a
// channel, as hex. corpus.mjs runs screenSig inside the page (its source: screenSig.toString()).
export const SIG_X = 8, SIG_Y = 6, SIG_S = 8, SIG_MEAN = 1.0, SIG_CELL = 3, SIG_BAD = 4;

/** The signature of w*h pixels of `bpp` bytes each: rgb first, or with `bgr` blue first (sake's
 * BGRA frames). */
export function screenSig(w, h, px, bpp, bgr) {
  const X = 8, Y = 6, S = 8;
  let out = "";
  for (let cy = 0; cy < Y; cy++)
    for (let cx = 0; cx < X; cx++) {
      let r = 0, g = 0, b = 0;
      for (let sy = 0; sy < S; sy++) {
        const y = Math.floor(((cy * S + sy) * h) / (Y * S)) + Math.floor(h / (Y * S * 2));
        for (let sx = 0; sx < S; sx++) {
          const x = Math.floor(((cx * S + sx) * w) / (X * S)) + Math.floor(w / (X * S * 2));
          const i = (y * w + x) * bpp;
          r += px[bgr ? i + 2 : i]; g += px[i + 1]; b += px[bgr ? i : i + 2];
        }
      }
      for (const a of [r, g, b]) out += (Math.floor(a / (S * S)) >> 4).toString(16);
    }
  return out;
}

/** Whether signatures a and b are the same screen (shots.py sig_same). */
export function sigSame(a, b) {
  if (!a || !b || a.length !== b.length || a.length !== SIG_X * SIG_Y * 3) return false;
  let sum = 0, bad = 0;
  for (let i = 0; i < a.length; i += 3) {
    let m = 0;
    for (let k = i; k < i + 3; k++) {
      const d = Math.abs(parseInt(a[k], 16) - parseInt(b[k], 16));
      sum += d;
      m = Math.max(m, d);
    }
    if (m > SIG_CELL) bad++;
  }
  return sum / a.length <= SIG_MEAN && bad <= SIG_BAD;
}
