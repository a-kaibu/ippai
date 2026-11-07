import { expect, test } from "vite-plus/test";
import { blake3Hex } from "./blake3";
// chunk names from the blake3 crate (crates/ippai/src/pack.rs makes the file)
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
