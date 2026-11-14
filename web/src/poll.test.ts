import { afterEach, expect, test, vi } from "vite-plus/test";
import { every, shared, throttled } from "./poll";

afterEach(() => void vi.useRealTimers());

/**
 * every: random run lengths against the pause; never two runs at once, each next run starts
 * no sooner than `ms` after the last ended, a throwing run does not stop it, stop stops it
 * (property, seeded, fake clock).
 */
test("every: runs never overlap and keep their pause, throws included (property, seeded)", async () => {
  vi.useFakeTimers();
  let x = 3;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 30; run++) {
    const ms = 1 + rnd(50);
    let active = 0;
    let ended = -Infinity;
    const starts: number[] = [];
    let failed = 0;
    const stop = every(
      ms,
      async () => {
        expect(active).toBe(0);
        expect(Date.now() - ended).toBeGreaterThanOrEqual(starts.length ? ms : -Infinity);
        starts.push(Date.now());
        active++;
        await new Promise((r) => setTimeout(r, rnd(200)));
        active--;
        ended = Date.now();
        if (!rnd(4)) throw new Error("run failed");
      },
      () => failed++,
    );
    await vi.advanceTimersByTimeAsync(2000);
    stop();
    const n = starts.length;
    await vi.advanceTimersByTimeAsync(2000);
    // (a run under way when stopped ends; none starts after)
    expect(starts.length).toBe(n);
    expect(n).toBeGreaterThan(2000 / (ms + 200) - 1);
    expect(failed).toBeLessThanOrEqual(n);
  }
});

test("throttled: at most once per pause, and the last wish is kept (property, seeded)", async () => {
  vi.useFakeTimers();
  let x = 5;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 30; run++) {
    const calls: number[] = [];
    const f = throttled(250, () => calls.push(Date.now()));
    let lastWish = 0;
    for (let i = 0; i < 100; i++) {
      await vi.advanceTimersByTimeAsync(rnd(100));
      f();
      lastWish = Date.now();
    }
    await vi.advanceTimersByTimeAsync(1000);
    for (let i = 1; i < calls.length; i++) expect(calls[i]! - calls[i - 1]!).toBeGreaterThanOrEqual(250);
    // the last call came at or after the last wish
    expect(calls.at(-1)!).toBeGreaterThanOrEqual(lastWish);
    f.cancel();
  }
});

test("shared: callers within the time get one answer; fresh asks anew; a failure is not kept", async () => {
  vi.useFakeTimers();
  let n = 0;
  let fail = false;
  const get = shared(2000, async () => {
    await new Promise((r) => setTimeout(r, 100));
    if (fail) throw new Error("no");
    return ++n;
  });
  const [a, b] = await Promise.all([
    (async () => {
      const p = get();
      await vi.advanceTimersByTimeAsync(100);
      return p;
    })(),
    get(),
  ]);
  expect([a, b]).toEqual([1, 1]);
  expect(await get()).toBe(1);
  const f = get(true);
  await vi.advanceTimersByTimeAsync(100);
  expect(await f).toBe(2);
  await vi.advanceTimersByTimeAsync(2001);
  fail = true;
  const e = get();
  const caught = e.catch((x: Error) => x.message);
  await vi.advanceTimersByTimeAsync(100);
  expect(await caught).toBe("no");
  fail = false;
  const g = get();
  await vi.advanceTimersByTimeAsync(100);
  expect(await g).toBe(3);
});
