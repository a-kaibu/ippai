import { expect, test } from "vite-plus/test";
import { blake3Hex, blake3Scalar, blake3Simd, simdMemoryBytes } from "./blake3";
// chunk names from the blake3 crate (crates/masu/src/pack.rs makes the file)
import tsv from "../../testdata/blake3.tsv?raw";

/** The inputs testdata/blake3.tsv names: `p` i % 251, `r` an LCG seeded with the length. */
export function input(kind: string, n: number): Uint8Array {
  let x = n >>> 0;
  return Uint8Array.from({ length: n }, (_, i) => {
    if (kind === "p") return i % 251;
    x = (Math.imul(x, 1103515245) + 12345) >>> 0;
    return (x >>> 16) & 255;
  });
}

const vectors = tsv
  .trim()
  .split("\n")
  .slice(1)
  .map((l) => l.split("\t") as [string, string, string]);

test("chunk names as the blake3 crate makes them", () => {
  expect(vectors.length).toBeGreaterThan(60);
  for (const [kind, n, h] of vectors) expect(blake3Hex(input(kind, +n)), `${kind} ${n}`).toBe(h);
});

test("a view into a larger buffer (fetches hand those out)", () => {
  const [, n, h] = vectors.find((v) => v[0] === "p" && v[1] === "4097")!;
  const big = new Uint8Array(10000);
  big.set(input("p", +n), 13);
  expect(blake3Hex(big.subarray(13, 13 + +n))).toBe(h);
});

test("both ways (wasm SIMD and plain) give the blake3 crate's names", () => {
  const simd = blake3Simd();
  expect(simd, "node has wasm SIMD").toBeTruthy();
  for (const [kind, n, h] of vectors) {
    const d = input(kind, +n);
    expect(blake3Scalar(d), `scalar ${kind} ${n}`).toBe(h);
    expect(simd!(d), `simd ${kind} ${n}`).toBe(h);
  }
});

// property: for any input, the SIMD tree (chunks four at a time, through a 256-chunk window,
// parents level by level) names it as the plain one does: random lengths around the
// boundaries that matter (1 KiB chunks, groups of 4, the 256 KiB window, odd levels)
test("wasm SIMD = plain BLAKE3 for random inputs (seeded)", () => {
  const simd = blake3Simd()!;
  let x = 0x9e3779b9;
  const rnd = () => ((x = (Math.imul(x, 1664525) + 1013904223) >>> 0), x);
  const edges = [1024, 4096, 256 * 1024, 257 * 1024, 1 << 20];
  for (let k = 0; k < 120; k++) {
    const e = edges[rnd() % edges.length]!;
    const n = k < 60 ? rnd() % 20000 : Math.max(0, e + (rnd() % 4097) - 2048);
    const d = Uint8Array.from({ length: n }, () => rnd() & 255);
    expect(simd(d), `length ${n}`).toBe(blake3Scalar(d));
  }
});

test("a 64 MiB chunk (the store's largest) hashes the same, and the module's memory stays small", () => {
  const simd = blake3Simd()!;
  const d = new Uint8Array(64 << 20);
  for (let i = 0; i < d.length; i += 4093) d[i] = i & 255;
  expect(simd(d)).toBe(blake3Scalar(d));
  // (the input goes through a window; only the chunks' chaining values stay: 2 MiB + 256 KiB)
  expect(simdMemoryBytes()).toBeLessThan(3 << 20);
});
