import { expect, test } from "vite-plus/test";
import { duration, percent, rate, size } from "./Storage";

test("sizes as people read them", () => {
  expect(size(0)).toBe("0 KB");
  expect(size(1)).toBe("1 KB");
  expect(size(1536)).toBe("2 KB");
  expect(size(5 * 2 ** 20)).toBe("5 MB");
  expect(size(1.5 * 2 ** 30)).toBe("1.5 GB");
  expect(size(6.67 * 2 ** 30)).toBe("6.7 GB");
  expect(size(631 * 2 ** 30)).toBe("631 GB");
});

test("percent: whole, never past 100 for a part of the whole, 0 of 0 done", () => {
  expect(percent(0, 0)).toBe(100);
  expect(percent(1, 3)).toBe(33);
  expect(percent(999, 1000)).toBe(99);
  for (let all = 1; all < 200; all += 7)
    for (let p = 0; p <= all; p += 3) expect(percent(p, all)).toBeLessThanOrEqual(100);
});

test("a save's rate and time left", () => {
  const p = {
    have: 2,
    total: 4,
    haveBytes: 30 << 20,
    totalBytes: 50 << 20,
    now: "",
    started: 0,
    startBytes: 10 << 20,
  };
  expect(rate(p, 500)).toEqual({ perSecond: 0, left: undefined });
  const r = rate(p, 10_000);
  expect(r.perSecond).toBe(2 << 20);
  expect(r.left).toBe(10);
  expect(duration(10)).toBe("10 秒");
  expect(duration(150)).toBe("3 分");
  expect(duration(3 * 3600 + 20 * 60)).toBe("3 時間 20 分");
});
