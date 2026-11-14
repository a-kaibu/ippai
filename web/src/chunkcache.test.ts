import { expect, test } from "vite-plus/test";
import { chunkCacheMb } from "./chunkcache";

test("small devices get a small chunk cache, others kura's default", () => {
  // (Chrome's buckets: 0.25 0.5 1 2 4 8)
  for (const gb of [0.25, 0.5, 1, 2, 4, 6, 7.9]) expect(chunkCacheMb(gb)).toBe(32);
  for (const gb of [8, 16, 64]) expect(chunkCacheMb(gb)).toBeUndefined();
  // (not said: Firefox, Safari; a nonsense value)
  for (const gb of [undefined, 0, -1, NaN]) expect(chunkCacheMb(gb)).toBeUndefined();
});
