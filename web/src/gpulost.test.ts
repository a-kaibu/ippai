import { expect, test } from "vite-plus/test";
import { LOST_FOR_GOOD, LOST_STOPPED, lostNotice } from "./gpulost";

const back = (n: number) => n << 2;

test("nothing while nothing was lost, or while a new device is being had", () => {
  expect(lostNotice(0, 0)).toBeNull();
  expect(lostNotice(0, LOST_STOPPED)).toBeNull();
  expect(lostNotice(back(1), back(1) | LOST_STOPPED)).toBeNull();
});

test("each loss come back from says so once", () => {
  expect(lostNotice(LOST_STOPPED, back(1))?.kind).toBe("info");
  expect(lostNotice(0, back(1))?.reload).toBe(false);
  expect(lostNotice(back(1), back(1))).toBeNull();
  expect(lostNotice(back(1), back(2))?.kind).toBe("info");
  // (more than one between two looks: one notice)
  expect(lostNotice(back(1), back(4))).not.toBeNull();
});

test("no new device: an error with a reload, once", () => {
  const n = lostNotice(LOST_STOPPED, LOST_STOPPED | LOST_FOR_GOOD);
  expect(n?.kind).toBe("error");
  expect(n?.reload).toBe(true);
  expect(lostNotice(back(2) | 3, back(2) | 3)).toBeNull();
});

test("every pair of states: an error only when it became for good, info only when one more came back", () => {
  for (let prev = 0; prev < 32; prev++)
    for (let now = 0; now < 32; now++) {
      const n = lostNotice(prev, now);
      const good = !!(now & LOST_FOR_GOOD) && !(prev & LOST_FOR_GOOD);
      const more = now >>> 2 > prev >>> 2;
      expect(n?.kind === "error").toBe(good);
      expect(n?.kind === "info").toBe(!good && more);
      expect(n === null).toBe(!good && !more);
    }
});
