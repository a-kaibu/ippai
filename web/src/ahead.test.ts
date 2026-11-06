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

test("budget: held bytes wait for start, the total ends it", async () => {
  const b = new Budget(100, 250);
  let fetched = 0;
  const worker = async () => {
    while (await b.room()) {
      await tick(1);
      b.add(30);
      fetched++;
    }
  };
  const done = Promise.all([worker(), worker(), worker()]);
  await tick(30);
  // (each worker checks before it fetches: 3 may be past the line by one chunk each)
  expect(b.spent).toBeGreaterThanOrEqual(100);
  expect(b.spent).toBeLessThan(100 + 3 * 30);
  const held = fetched;
  await tick(20);
  expect(fetched).toBe(held);
  b.start();
  await done;
  expect(b.spent).toBeGreaterThanOrEqual(250);
  expect(b.spent).toBeLessThan(250 + 3 * 30);
});
