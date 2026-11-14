import { expect, test } from "vite-plus/test";
import { ReportLimit } from "./reportlimit";

// crashlog.ts fatal's reports: a recurring error makes a few, not one per occurrence.

test("the first few at once, then one a minute, and a cap", () => {
  const l = new ReportLimit(3, 60_000, 5, 10 * 60_000);
  const at = (t: number, i: number) => l.allow(`TypeError: error ${i}\n    at x (a.js:1:2)`, t);
  expect([0, 1, 2].map((i) => at(i, i))).toEqual([true, true, true]);
  expect(at(10, 3)).toBe(false);
  expect(at(60_002, 4)).toBe(true);
  expect(at(60_003, 5)).toBe(false);
  expect(at(120_002, 6)).toBe(true);
  // (five in all)
  expect(at(10 ** 9, 7)).toBe(false);
});

test("the same error again within the window makes none", () => {
  const l = new ReportLimit(3, 60_000, 20, 10 * 60_000);
  expect(l.allow("unhandled rejection: Error: x\n at a", 0)).toBe(true);
  expect(l.allow("unhandled rejection: Error: x\n at b", 1000)).toBe(false);
  expect(l.allow("unhandled rejection: Error: y", 2000)).toBe(true);
  expect(l.allow("unhandled rejection: Error: x", 11 * 60_000)).toBe(true);
});

test("the browser's ResizeObserver notice is not an error", () => {
  const l = new ReportLimit();
  expect(l.allow("JS error: ResizeObserver loop completed with undelivered notifications. (:0:0)", 0)).toBe(
    false,
  );
  expect(l.allow("JS error: ResizeObserver loop limit exceeded (:0:0)", 0)).toBe(false);
});

test("property: never more than allowed in any stretch of time", () => {
  for (let run = 0; run < 200; run++) {
    const free = 1 + (run % 4);
    const every = 1000 * (1 + (run % 7));
    const max = 3 + (run % 10);
    const l = new ReportLimit(free, every, max, 5000);
    let t = 0;
    const times: number[] = [];
    for (let i = 0; i < 300; i++) {
      t += Math.floor(Math.random() * 700);
      if (l.allow(`E${Math.floor(Math.random() * 30)}`, t)) times.push(t);
    }
    expect(times.length).toBeLessThanOrEqual(max);
    // (after the free ones, any two are `every` apart)
    for (let i = free; i < times.length; i++) expect(times[i]! - times[i - 1]!).toBeGreaterThanOrEqual(every);
  }
});
