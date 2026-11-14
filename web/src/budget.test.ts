import { expect, test } from "vite-plus/test";
import source from "../../host/fetch-bridge.js?raw";
import { BURST, BYTES, CLASS, RATE, RESERVE, drain, take, waitFor } from "./budget";

// host/fetch-bridge.js's copy, run on the same buffer: the page and the network worker share it
interface Bridge {
  IPPAI_CLASS: typeof CLASS;
  IPPAI_RATE: number;
  IPPAI_BURST: number;
  IPPAI_RESERVE: number[];
  ippaiBudgetInit(b?: SharedArrayBuffer): SharedArrayBuffer;
  ippaiTake(cls: number, now: number): boolean;
  ippaiDrain(ms: number, now: number): void;
  IPPAI_BUDGET_BYTES: number;
}
const bridge = (): Bridge =>
  // oxlint-disable-next-line typescript/no-implied-eval -- (the worker's script, as the worker runs it)
  new Function(
    `${source}; return { IPPAI_CLASS, IPPAI_RATE, IPPAI_BURST, IPPAI_RESERVE, IPPAI_BUDGET_BYTES, ippaiBudgetInit, ippaiTake, ippaiDrain };`,
  )() as Bridge;

test("the page's budget is the network worker's: same constants", () => {
  const b = bridge();
  expect({ ...b.IPPAI_CLASS }).toEqual(CLASS);
  expect([b.IPPAI_RATE, b.IPPAI_BURST, [...b.IPPAI_RESERVE], b.IPPAI_BUDGET_BYTES]).toEqual([
    RATE,
    BURST,
    RESERVE,
    BYTES,
  ]);
});

test("the page and the worker taking from one buffer agree with one taker (differential)", () => {
  let x = 99;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 100; run++) {
    const t0 = 1_000_000;
    const mk = () => {
      const buf = new SharedArrayBuffer(BYTES);
      new Float64Array(buf, 8, 3).set([BURST, t0, 0]);
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
      // (now and then a 429, from either side)
      if (rnd(40) === 0) {
        const ms = rnd(6000);
        if (rnd(2)) w.ippaiDrain(ms, now);
        else drain(ms, now, both);
        drain(ms, now, alone);
        continue;
      }
      const cls = rnd(5);
      const a = rnd(2) ? w.ippaiTake(cls, now) : take(cls, now, both);
      const b = take(cls, now, alone);
      expect(a, `run ${run} step ${i}`).toBe(b);
    }
  }
});

test("a class leaves its reserve to those above it", () => {
  const buf = new SharedArrayBuffer(BYTES);
  const now = 5_000_000;
  const set = (n: number) => new Float64Array(buf, 8, 3).set([n, now, 0]);
  set(RESERVE[CLASS.save]! + 0.5);
  expect(take(CLASS.save, now, buf)).toBe(false);
  expect(take(CLASS.hint, now, buf)).toBe(true);
  set(0.5);
  expect(take(CLASS.ahead, now, buf)).toBe(false);
  expect(take(CLASS.game, now, buf)).toBe(true);
});

test("a 429 stops every class, the game's too, for its whole wait (112 ms after it the game went before)", () => {
  const buf = new SharedArrayBuffer(BYTES);
  const now = 9_000_000;
  new Float64Array(buf, 8, 3).set([BURST, now, 0]);
  drain(5000, now, buf);
  for (const dt of [0, 112, 4999])
    for (const cls of Object.values(CLASS)) expect(take(cls, now + dt, buf)).toBe(false);
  expect(take(CLASS.game, now + 5000, buf)).toBe(true);
  expect(take(CLASS.save, now + 5000, buf)).toBe(false);
});

test("a lock word its holder never gave back (a worker that died inside) is taken over, not spun on forever", () => {
  const buf = new SharedArrayBuffer(BYTES);
  new Float64Array(buf, 8, 3).set([BURST, 0, 0]);
  new Int32Array(buf, 0, 1)[0] = 1;
  expect(take(CLASS.game, 0, buf)).toBe(true);
  // (and freed after: the next one takes it at once)
  expect(new Int32Array(buf, 0, 1)[0]).toBe(0);
  const b = bridge();
  const wb = b.ippaiBudgetInit(buf);
  new Int32Array(wb, 0, 1)[0] = 1;
  expect(b.ippaiTake(CLASS.game, 0)).toBe(true);
});

/** waitFor against take over random budgets: when it says 0 a take goes now; when it says t, no
 * take goes before t and one goes at t (property, seeded). */
test("waitFor says when take will go (property, seeded)", () => {
  let x = 8;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 2000; run++) {
    const buf = new SharedArrayBuffer(BYTES);
    const now = 1_000_000;
    new Float64Array(buf, 8, 3).set([
      rnd(BURST * 2) - BURST,
      now - 3000 + rnd(6000),
      rnd(3) ? 0 : now + rnd(5000),
    ]);
    const cls = rnd(5);
    const w = waitFor(cls, now, buf);
    const copy = () => {
      const b = new SharedArrayBuffer(BYTES);
      new Uint8Array(b).set(new Uint8Array(buf));
      return b;
    };
    if (w === 0) expect(take(cls, now, copy()), `run ${run}`).toBe(true);
    else {
      expect(take(cls, now + w - 2, copy()), `run ${run}: not before ${w}`).toBe(false);
      expect(take(cls, now + w + 1, copy()), `run ${run}: at ${w}`).toBe(true);
    }
  }
});
