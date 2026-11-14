import { expect, test } from "vite-plus/test";
import { LineLimit } from "./linelimit";
import { SURFACE_MAX, surfaceSize } from "./surface";

test("surfaceSize: within 1..max on each side, the shape kept when scaled down (property, seeded)", () => {
  let x = 23;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  const odd = [0, -1, NaN, Infinity, 0.3];
  for (let run = 0; run < 5000; run++) {
    const w = rnd(8) ? rnd(9000) + rnd(100) / 100 : odd[rnd(odd.length)]!;
    const h = rnd(8) ? rnd(6000) : odd[rnd(odd.length)]!;
    const dpr = rnd(6) ? [1, 1.5, 2, 2.625, 3, 4][rnd(6)]! : odd[rnd(odd.length)]!;
    const [a, b] = surfaceSize(w, h, dpr);
    expect(Number.isInteger(a) && Number.isInteger(b), `run ${run}`).toBe(true);
    for (const v of [a, b])
      expect(v >= 1 && v <= SURFACE_MAX, `run ${run}: ${w}x${h}@${dpr} -> ${a}x${b}`).toBe(true);
    const d = dpr > 0 && Number.isFinite(dpr) ? dpr : 1;
    const fx = Math.floor((Number.isFinite(w) ? w : 0) * d),
      fy = Math.floor((Number.isFinite(h) ? h : 0) * d);
    // (within the limit: as before, but never 0)
    if (Math.max(fx, fy) <= SURFACE_MAX) expect([a, b]).toEqual([Math.max(1, fx), Math.max(1, fy)]);
    else if (fx > 0 && fy > 0)
      expect(Math.abs(a / b - fx / fy) / (fx / fy)).toBeLessThan(0.01 + 2 / Math.min(a, b));
  }
});

test("LineLimit: a burst, then the rate; what was left out is told once (property, seeded)", () => {
  let x = 29;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 100; run++) {
    const l = new LineLimit(50, 500, 0);
    let t = 0,
      shownN = 0,
      dropped = 0,
      told = 0;
    for (let i = 0; i < 5000; i++) {
      t += rnd(10) ? 0 : rnd(200);
      if (l.allow(t, (n) => (told += n))) shownN++;
      else dropped++;
    }
    // (shown at most the burst and the rate over the time)
    expect(shownN).toBeLessThanOrEqual(500 + (t / 1000) * 50 + 1);
    expect(shownN + dropped).toBe(5000);
    // (every line left out is told, but the last run of them, not yet)
    expect(told).toBeLessThanOrEqual(dropped);
    l.allow(t + 1e6, (n) => (told += n));
    expect(told).toBe(dropped);
  }
});
