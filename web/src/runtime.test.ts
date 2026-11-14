import { afterEach, beforeEach, expect, test } from "vite-plus/test";
import { FAILED_KEEP_MS, type SakeBuild, growRetry, lastSakeBuild, sakeBuild } from "./runtime";

// (a device's localStorage: node has none)
let kept: Map<string, string>;
beforeEach(() => {
  kept = new Map();
  (globalThis as { localStorage?: unknown }).localStorage = {
    getItem: (k: string) => kept.get(k) ?? null,
    setItem: (k: string, v: string) => void kept.set(k, String(v)),
    removeItem: (k: string) => void kept.delete(k),
  };
});
afterEach(() => {
  delete (globalThis as { localStorage?: unknown }).localStorage;
});

// (a stand-in for the 4 GB memory: growRetry only looks whether there is one)
const full: SakeBuild = { name: "sake", memory: {} as WebAssembly.Memory, jit: "" };
const grow: SakeBuild = { name: "sake-grow", jit: "g" };

/** Over random starts (seeded): a retry on sake-grow exactly for a fresh start of sake.wasm
 * with the page's 4 GB memory that failed before it ran, never for a saved state's (its memory
 * is the state's), never twice (the address it gives is already on sake-grow), and the rest of
 * the address kept. */
test("a start that failed with 4 GB starts again on sake-grow, only when that is safe (property, seeded)", () => {
  let x = 5;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  const builds = [full, grow, { name: "sake", jit: "" } as SakeBuild];
  for (let run = 0; run < 500; run++) {
    kept.clear();
    const build = builds[rnd(3)]!;
    const fromState = rnd(3) === 0;
    const started = rnd(3) === 0;
    const memory = ["", "&memory=grow", "&memory=full"][rnd(3)]!;
    const href = `https://w2.example/play.html?game=g${run}&run=Z:%5Cgame%5Ca.exe${memory}${rnd(2) ? "&jit=0" : ""}`;
    const to = growRetry(href, { build, fromState, started }, 1000);
    const want = build === full && !fromState && !started && memory !== "&memory=grow";
    expect(!!to, `run ${run}: ${JSON.stringify({ name: build.name, fromState, started, memory })}`).toBe(
      want,
    );
    if (!to) {
      expect(kept.size, `run ${run}`).toBe(0);
      continue;
    }
    const a = new URL(href);
    const b = new URL(to);
    expect(b.searchParams.get("memory")).toBe("grow");
    for (const [k, v] of a.searchParams) if (k !== "memory") expect(b.searchParams.get(k), k).toBe(v);
    expect(b.pathname).toBe(a.pathname);
    // (the start on sake-grow failing too: no third)
    expect(growRetry(to, { build: grow, fromState: false, started: false })).toBeNull();
    expect(growRetry(to, { build, fromState, started })).toBeNull();
  }
});

test("a device where 4 GB failed stays on sake-grow for a while (&memory=full tries it anyway)", () => {
  const t = 1_800_000_000_000;
  expect(sakeBuild(null, t).name).toBe("sake");
  expect(
    growRetry("https://w2.example/play.html?game=a", { build: full, fromState: false, started: false }, t),
  ).toBe("https://w2.example/play.html?game=a&memory=grow");
  for (const dt of [0, 1, FAILED_KEEP_MS - 1]) {
    const b = sakeBuild(null, t + dt);
    expect([b.name, b.memory, b.jit], `after ${dt} ms`).toEqual(["sake-grow", undefined, "g"]);
  }
  expect(lastSakeBuild().name).toBe("sake-grow");
  expect(sakeBuild("full", t + 1).name).toBe("sake");
  expect(sakeBuild(null, t + FAILED_KEEP_MS).name).toBe("sake");
  expect(sakeBuild("grow", t + FAILED_KEEP_MS).name).toBe("sake-grow");
});

test("no storage (private mode, blocked): the retry still goes, the next start tries 4 GB again", () => {
  (globalThis as { localStorage?: unknown }).localStorage = {
    getItem: () => {
      throw new Error("denied");
    },
    setItem: () => {
      throw new Error("denied");
    },
  };
  expect(growRetry("https://w2.example/play.html", { build: full, fromState: false, started: false })).toBe(
    "https://w2.example/play.html?memory=grow",
  );
  expect(sakeBuild(null).name).toBe("sake");
});
