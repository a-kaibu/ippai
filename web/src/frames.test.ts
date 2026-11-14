import { expect, test } from "vite-plus/test";
import { frameTicker } from "./frames";

/** A frame source the test steps by hand: how many frames are asked for at once. */
function source() {
  const want: ((t: number) => void)[] = [];
  return {
    want,
    request: (cb: (t: number) => void) => void want.push(cb),
    step(t: number) {
      const cbs = want.splice(0);
      for (const cb of cbs) cb(t);
      return cbs.length;
    },
  };
}

// property (seeded, against a model): over any order of listeners added, removed, removing
// themselves and throwing, every frame calls exactly the listeners there when it began (less
// those removed during it), at most one frame is ever asked for, and none once nobody listens
test("one loop for every listener, as the model says (seeded)", () => {
  let x = 7;
  const rnd = (n: number) => ((x = (Math.imul(x, 1664525) + 1013904223) >>> 0), x % n);
  for (let run = 0; run < 200; run++) {
    const src = source();
    const ticker = frameTicker(src);
    const live = new Map<number, () => void>();
    const calls: number[] = [];
    const selfStop = new Set<number>();
    const throwing = new Set<number>();
    let id = 0;
    for (let step = 0; step < 60; step++) {
      const op = rnd(4);
      if (op === 0 || live.size === 0) {
        const me = id++;
        if (rnd(5) === 0) selfStop.add(me);
        if (rnd(7) === 0) throwing.add(me);
        const stop = ticker.each(() => {
          calls.push(me);
          if (selfStop.has(me)) {
            live.get(me)?.();
            live.delete(me);
          }
          if (throwing.has(me)) throw new Error("listener failed");
        });
        live.set(me, stop);
      } else if (op === 1) {
        const k = [...live.keys()][rnd(live.size)]!;
        live.get(k)!();
        live.delete(k);
      } else {
        const before = [...live.keys()];
        calls.length = 0;
        const asked = src.step(step);
        expect(asked).toBeLessThanOrEqual(1);
        // (a frame asked for while someone listened calls each of them once, in order; one
        // after which nobody listens asks for no more)
        if (asked) expect(calls).toEqual(before);
        if (!live.size) expect(src.want.length).toBe(0);
      }
      expect(src.want.length).toBeLessThanOrEqual(1);
      expect(ticker.size).toBe(live.size);
    }
    for (const stop of live.values()) stop();
    src.step(1e9);
    // (nobody left: the loop ends, no frame asked for)
    expect(src.want.length).toBe(0);
  }
});

test("a listener added again after the loop ended starts it again", () => {
  const src = source();
  const ticker = frameTicker(src);
  let n = 0;
  const stop = ticker.each(() => n++);
  src.step(1);
  stop();
  src.step(2);
  expect(src.want.length).toBe(0);
  ticker.each(() => n++);
  expect(src.want.length).toBe(1);
  src.step(3);
  expect(n).toBe(2);
});
