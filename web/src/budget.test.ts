import { expect, test } from "vite-plus/test";
import source from "../../host/fetch-bridge.js?raw";
import { BURST, CLASS, RATE, RESERVE, take } from "./budget";

// host/fetch-bridge.js's copy, run on the same buffer: the page and the network worker share it
interface Bridge {
  IPPAI_CLASS: typeof CLASS;
  IPPAI_RATE: number;
  IPPAI_BURST: number;
  IPPAI_RESERVE: number[];
  ippaiBudgetInit(b?: SharedArrayBuffer): SharedArrayBuffer;
  ippaiTake(cls: number, now: number): boolean;
}
const bridge = (): Bridge =>
  // oxlint-disable-next-line typescript/no-implied-eval -- (the worker's script, as the worker runs it)
  new Function(
    `${source}; return { IPPAI_CLASS, IPPAI_RATE, IPPAI_BURST, IPPAI_RESERVE, ippaiBudgetInit, ippaiTake };`,
  )() as Bridge;

test("the page's budget is the network worker's: same constants", () => {
  const b = bridge();
  expect({ ...b.IPPAI_CLASS }).toEqual(CLASS);
  expect([b.IPPAI_RATE, b.IPPAI_BURST, [...b.IPPAI_RESERVE]]).toEqual([RATE, BURST, RESERVE]);
});

test("the page and the worker taking from one buffer agree with one taker (differential)", () => {
  let x = 99;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 100; run++) {
    const t0 = 1_000_000;
    const mk = () => {
      const buf = new SharedArrayBuffer(24);
      new Float64Array(buf, 8, 2).set([BURST, t0]);
      return buf;
    };
    // one buffer shared by both takers; one more taken only by the page's copy
    const both = mk();
    const alone = mk();
    const w = bridge();
    w.ippaiBudgetInit(both);
    let now = t0;
    for (let i = 0; i < 300; i++) {
      now += rnd(150);
      const cls = rnd(5);
      const a = rnd(2) ? w.ippaiTake(cls, now) : take(cls, now, both);
      const b = take(cls, now, alone);
      expect(a, `run ${run} step ${i}`).toBe(b);
    }
  }
});

test("a class leaves its reserve to those above it", () => {
  const buf = new SharedArrayBuffer(24);
  const now = 5_000_000;
  const set = (n: number) => new Float64Array(buf, 8, 2).set([n, now]);
  set(RESERVE[CLASS.save]! + 0.5);
  expect(take(CLASS.save, now, buf)).toBe(false);
  expect(take(CLASS.hint, now, buf)).toBe(true);
  set(0.5);
  expect(take(CLASS.ahead, now, buf)).toBe(false);
  expect(take(CLASS.game, now, buf)).toBe(true);
});
