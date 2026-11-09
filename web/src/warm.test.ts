import { expect, test } from "vite-plus/test";
import {
  startWarm,
  warmAllowed,
  warmBusyWidth,
  warmList,
  WARM_BUSY_WIDTH,
  WARM_WIDTH,
  type WarmReport,
} from "./warm";
import { gameChunksName } from "./runtime";

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
  const rnd = (n: number) => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) % n;
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

test("what earlier runs read first, in their order, then the trees' others; each once", () => {
  const h = (c: string) => c.repeat(32);
  const sizes = new Map([
    [h("a"), 4],
    [h("b"), 4],
    [h("c"), 4],
    [h("d"), 4],
  ]);
  // (one the trees lack: another game's, or one gone from the store; skipped)
  expect(warmList(sizes, 100, [h("c"), h("9"), h("a"), h("c")])).toEqual([h("c"), h("a"), h("b"), h("d")]);
  expect(warmList(sizes, 8, [h("d")])).toEqual([h("d"), h("a")]);
  expect(warmList(sizes, 100, [])).toEqual(warmList(sizes, 100));
});

test("random lists with a first part: a permutation's prefix, first ones first, within the bytes", () => {
  let seed = 19;
  const rnd = (n: number) => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) % n;
  for (let i = 0; i < 500; i++) {
    const sizes = new Map<string, number>();
    for (let k = 0, n = rnd(40); k < n; k++) sizes.set(k.toString(16).padStart(32, "0"), 1 + rnd(100));
    const first = Array.from({ length: rnd(20) }, () => rnd(60).toString(16).padStart(32, "0"));
    const max = rnd(3000);
    const got = warmList(sizes, max, first);
    expect(new Set(got).size).toBe(got.length);
    for (const x of got) expect(sizes.has(x)).toBe(true);
    const want = [...new Set([...first.filter((x) => sizes.has(x)), ...sizes.keys()])];
    expect(got).toEqual(want.slice(0, got.length));
    const sum = got.reduce((a, x) => a + sizes.get(x)!, 0);
    expect(sum).toBeLessThanOrEqual(max);
    if (got.length < want.length) expect(sum + sizes.get(want[got.length]!)!).toBeGreaterThan(max);
  }
});

test("while the game waits: some go on, none on a slow line", () => {
  expect(warmBusyWidth(undefined)).toBe(WARM_BUSY_WIDTH);
  expect(warmBusyWidth({})).toBe(WARM_BUSY_WIDTH);
  expect(warmBusyWidth({ downlink: 10 })).toBe(WARM_BUSY_WIDTH);
  expect(warmBusyWidth({ downlink: 4.9 })).toBe(0);
  expect(WARM_BUSY_WIDTH).toBeGreaterThan(0);
  expect(WARM_BUSY_WIDTH).toBeLessThan(WARM_WIDTH);
});

test("the first ones and the width while the game waits reach the worker", () => {
  let urls: string[] = [];
  let busyWidth: number | undefined;
  const h = (c: string) => c.repeat(32);
  const stop = startWarm(
    (u, _f, _w, _r, b) => {
      urls = u;
      busyWidth = b;
      return {} as Worker;
    },
    "https://h/store/",
    new Map([
      [h("a"), 1],
      [h("b"), 1],
    ]),
    () => false,
    () => {},
    [h("b")],
    1,
  );
  expect(urls).toEqual([`https://h/store/chunks/bb/${h("b")}`, `https://h/store/chunks/aa/${h("a")}`]);
  expect(busyWidth).toBe(1);
  stop();
});

test("a game's chunks list goes by the game, not its program", () => {
  expect(gameChunksName("saga_planets-2")).toBe("game.saga_planets-2");
  expect(gameChunksName("a/b c")).toBe("game.a_b_c");
  // (what ippai serve's /jit-save takes: letters, digits, _ - .)
  for (const id of ["x", "日本語", "../x", "a.b"])
    expect(gameChunksName(id)).toMatch(/^game\.[A-Za-z0-9_-]+$/);
});
