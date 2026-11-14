import { expect, test } from "vite-plus/test";
import { PACKAGE_MAX, restoreProblem, restoreTimeout } from "./ui/Personal";

test("a restore's file: refused at once when empty or past the server's limit", () => {
  expect(restoreProblem({ size: 0 })).toMatch(/空/);
  expect(restoreProblem({ size: 1 })).toBe("");
  expect(restoreProblem({ size: PACKAGE_MAX })).toBe("");
  expect(restoreProblem({ size: PACKAGE_MAX + 1 })).toMatch(/大きすぎ/);
  expect(restoreProblem({ size: 4 * 2 ** 30 })).toMatch(/4096 MiB/);
});

test("PACKAGE_MAX is what masu serve takes for /api/packages (crates/masu/src/portable.rs)", async () => {
  // (node's fs, out of the web types' sight)
  const { readFileSync } = (await import(/* @vite-ignore */ ["node", "fs"].join(":"))) as {
    readFileSync: (u: URL, e: string) => string;
  };
  const rs = readFileSync(new URL("../../crates/masu/src/portable.rs", import.meta.url), "utf8");
  const m = /DefaultBodyLimit::max\((\d+) << (\d+)\)/.exec(rs);
  expect(m).not.toBeNull();
  expect(Number(m![1]) * 2 ** Number(m![2])).toBe(PACKAGE_MAX);
});

test("restoreTimeout: at least a minute, growing with the size, never below a slow line's need (property)", () => {
  let x = 3;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  let last = 0;
  for (const b of Array.from({ length: 2000 }, () => rnd(PACKAGE_MAX + 1)).sort((p, q) => p - q)) {
    const t = restoreTimeout(b);
    expect(t).toBeGreaterThanOrEqual(60_000);
    expect(t).toBeGreaterThanOrEqual(last);
    // (100 KB/s still fits)
    expect(t).toBeGreaterThanOrEqual((b / 100_000) * 1000);
    last = t;
  }
});
