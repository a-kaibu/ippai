import { expect, test } from "vite-plus/test";
import { Budget, pool } from "./ahead";

const tick = (ms: number) => new Promise((r) => setTimeout(r, ms));

test("pool: never more than width at once, results in order, every job once", async () => {
  for (const width of [1, 2, 4, 8]) {
    let now = 0;
    let most = 0;
    const ran: number[] = [];
    const jobs = Array.from({ length: 23 }, (_, i) => async () => {
      now++;
      most = Math.max(most, now);
      await tick((i * 7) % 5);
      ran.push(i);
      now--;
      return i * 10;
    });
    expect(await pool(width, jobs)).toEqual(jobs.map((_, i) => i * 10));
    expect(most).toBe(width);
    expect(ran.sort((a, b) => a - b)).toEqual(jobs.map((_, i) => i));
  }
  expect(await pool(4, [])).toEqual([]);
  await expect(pool(2, [async () => 1, async () => Promise.reject(new Error("x"))])).rejects.toThrow("x");
});

/** `workers` side by side over `sizes` as play.ts readAhead: reserve, fetch (a random wait),
 * hold (before the start) or hand over; the most ever reserved before and after the start. */
async function readAhead(
  b: Budget,
  sizes: number[],
  workers: number,
  rnd: (n: number) => number,
  startAt: number,
) {
  let next = 0;
  let most = 0;
  let mostBefore = 0;
  let started = false;
  const got: number[] = [];
  const one = async () => {
    while (next < sizes.length && !b.full()) {
      const n = sizes[next++]!;
      if (!(await b.reserve(n))) continue;
      most = Math.max(most, b.spent);
      if (!started) mostBefore = Math.max(mostBefore, b.spent);
      await tick(rnd(3));
      // (a failed fetch gives its bytes back)
      if (rnd(10) === 0) {
        b.release(n);
        continue;
      }
      got.push(n);
    }
  };
  const run = Promise.all(Array.from({ length: workers }, one));
  await tick(startAt);
  started = true;
  b.start();
  await run;
  return { most, mostBefore, got };
}

test("budget: 4 fetches of 64 MiB chunks side by side never hold more than the limit (577a146 review)", async () => {
  const MiB = 1 << 20;
  const b = new Budget(32 * MiB, 64 * MiB);
  const { most, mostBefore, got } = await readAhead(b, Array(8).fill(64 * MiB), 4, () => 1, 20);
  // (a 64 MiB chunk fits the total once started, never what may be held before)
  expect(mostBefore).toBe(0);
  expect(most).toBeLessThanOrEqual(64 * MiB);
  expect(got.length).toBeLessThanOrEqual(1);
  const c = new Budget(32 * MiB, 64 * MiB);
  const r = await readAhead(c, Array(40).fill(16 * MiB), 4, () => 1, 20);
  expect(r.mostBefore).toBe(32 * MiB);
  expect(r.most).toBe(64 * MiB);
});

test("property: reservations never pass the limits, and the total is used when the chunks allow", async () => {
  let seed = 3;
  const rnd = (n: number) => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff), seed % n);
  for (let run = 0; run < 200; run++) {
    const before = 1 + rnd(100);
    const total = before + rnd(200);
    const sizes = Array.from({ length: rnd(40) }, () => 1 + rnd(80));
    const b = new Budget(before, total);
    const { most, mostBefore, got } = await readAhead(b, sizes, 1 + rnd(6), rnd, rnd(10));
    expect(mostBefore, `run ${run}`).toBeLessThanOrEqual(before);
    expect(most, `run ${run}`).toBeLessThanOrEqual(total);
    expect(b.spent, `run ${run}`).toBe(got.reduce((a, n) => a + n, 0));
  }
});

test("pool: the first failure rejects it and no job starts after; every failure is observed", async () => {
  let x = 31;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 100; run++) {
    const n = 1 + rnd(20);
    const bad = rnd(n + 3);
    const started: number[] = [];
    let atFailure = -1;
    const jobs = Array.from({ length: n }, (_, i) => async () => {
      started.push(i);
      await new Promise((r) => setTimeout(r, rnd(3)));
      if (i === bad) {
        atFailure = started.length;
        throw new Error(`job ${i}`);
      }
      return i;
    });
    const width = 1 + rnd(4);
    const r = await pool(width, jobs).then(
      (v) => ({ ok: true as const, v }),
      (e: Error) => ({ ok: false as const, e }),
    );
    if (bad < n) {
      expect(r.ok, `run ${run}`).toBe(false);
      // (started after the failure: none, also once those under way at it have ended)
      await new Promise((r) => setTimeout(r, 10));
      expect(started.length).toBe(atFailure);
    } else {
      expect(r).toEqual({ ok: true, v: Array.from({ length: n }, (_, i) => i) });
    }
  }
});
