import { expect, test } from "vite-plus/test";
import { zero } from "./zeroscan";

// Differential against a byte loop over random parts of a shared memory: any start, any length,
// one non-zero byte anywhere (the tail and an odd start included: the BigUint64Array scan threw
// at an unaligned start and never looked at the last n & 7 bytes).
test("zero: the same answer as a byte loop at any start and length (property, seeded)", () => {
  let x = 21;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  const mem = new Uint8Array(new SharedArrayBuffer(4096));
  for (let run = 0; run < 5000; run++) {
    mem.fill(0);
    const at = rnd(2048),
      n = rnd(1024);
    if (rnd(3)) mem[at + rnd(n + 16) - 8 < 0 ? 0 : at + rnd(n + 16) - 8] = 1 + rnd(255);
    let want = true;
    for (let i = at; i < at + n; i++) if (mem[i]) want = false;
    expect(zero(mem, at, n), `run ${run} at ${at} n ${n}`).toBe(want);
  }
  // (the bug's case: the only non-zero byte in the last 7)
  mem.fill(0);
  mem[8 + 13] = 5;
  expect(zero(mem, 8, 14)).toBe(false);
  expect(zero(mem, 3, 10)).toBe(true);
});
