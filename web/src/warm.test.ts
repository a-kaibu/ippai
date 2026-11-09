import { expect, test } from "vite-plus/test";
import { startWarm, warmAllowed, warmList, type WarmReport } from "./warm";

test("the trees' chunks in order, up to the bytes allowed", () => {
  const sizes = new Map([
    ["a".repeat(32), 4],
    ["b".repeat(32), 4],
    ["c".repeat(32), 4],
  ]);
  expect(warmList(sizes, 8)).toEqual(["a".repeat(32), "b".repeat(32)]);
  expect(warmList(sizes, 7)).toEqual(["a".repeat(32)]);
  expect(warmList(sizes, 100)).toHaveLength(3);
  expect(warmList(new Map(), 100)).toEqual([]);
});

test("random lists: never past the bytes allowed, always a prefix, as long as fits", () => {
  let seed = 7;
  const rnd = (n: number) => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) % n);
  for (let i = 0; i < 500; i++) {
    const sizes = new Map<string, number>();
    for (let k = 0, n = rnd(40); k < n; k++) sizes.set(k.toString(16).padStart(32, "0"), 1 + rnd(100));
    const max = rnd(3000);
    const got = warmList(sizes, max);
    const all = [...sizes.keys()];
    expect(got).toEqual(all.slice(0, got.length));
    const sum = got.reduce((a, h) => a + sizes.get(h)!, 0);
    expect(sum).toBeLessThanOrEqual(max);
    if (got.length < all.length) expect(sum + sizes.get(all[got.length]!)!).toBeGreaterThan(max);
  }
});

test("not on a mobile network or with data saving", () => {
  expect(warmAllowed(undefined)).toBe(true);
  expect(warmAllowed({ type: "wifi" })).toBe(true);
  expect(warmAllowed({ type: "cellular" })).toBe(false);
  expect(warmAllowed({ saveData: true })).toBe(false);
});

test("chunk URLs under the store, the game's waits passed on, and a stop", async () => {
  let urls: string[] = [];
  let flag!: Int32Array;
  let report!: (r: WarmReport) => void;
  let busy = true;
  const lines: string[] = [];
  const stop = startWarm(
    (u, f, width, r) => {
      urls = u;
      flag = new Int32Array(f);
      report = r;
      expect(width).toBeGreaterThan(1);
      return {} as Worker;
    },
    "https://h/store/",
    new Map([["ab" + "0".repeat(30), 10]]),
    () => busy,
    (l) => lines.push(l),
  );
  expect(urls).toEqual([`https://h/store/chunks/ab/ab${"0".repeat(30)}`]);
  await new Promise((r) => setTimeout(r, 120));
  expect(Atomics.load(flag, 0)).toBe(1);
  busy = false;
  await new Promise((r) => setTimeout(r, 120));
  expect(Atomics.load(flag, 0)).toBe(0);
  report({ done: 1, fetched: 1, bytes: 10, failed: 0, end: true });
  expect(lines.at(-1)).toMatch(/^fetched ahead: 1 of 1 chunks looked at, 1 fetched .* done$/);
  stop();
  expect(Atomics.load(flag, 1)).toBe(1);
});
