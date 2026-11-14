import { expect, test } from "vite-plus/test";
import { fetchForesee, FORESEE_TRIES, handForesee, mountedTrees } from "./foresee";

test("the trees the mounts name, each once, not paths or odd names", () => {
  expect(
    mountedTrees(["/=", "/ippai/prefix=prefix32", "/ippai/game=kinkoi", "/x=kinkoi", "/y=../z", "nomount"]),
  ).toEqual(["prefix32", "kinkoi"]);
});

test("a table goes into the module's memory and to kura, and is freed", () => {
  const memory = new WebAssembly.Memory({ initial: 1 });
  const calls: string[] = [];
  const m = {
    wasmMemory: memory,
    _malloc: (n: number) => (calls.push(`malloc ${n}`), 64),
    _free: (p: number) => void calls.push(`free ${p}`),
    _vnhost_vfs_foresee: (p: number, n: number) =>
      void calls.push(`foresee ${new TextDecoder().decode(new Uint8Array(memory.buffer, p, n))}`),
  };
  expect(handForesee(m, new TextEncoder().encode("ippai-foresee 1\na.ks\tb"))).toBe(true);
  expect(calls).toEqual(["malloc 22", "foresee ippai-foresee 1\na.ks\tb", "free 64"]);
  // (an older sake without it, an empty table: nothing)
  expect(handForesee({ ...m, _vnhost_vfs_foresee: undefined }, new Uint8Array(3))).toBe(false);
  expect(handForesee(m, new Uint8Array(0))).toBe(false);
});

test("asked again while the server reads it, then handed; a 404 or an older server stops", async () => {
  const asked: string[] = [];
  let left = 2;
  const fetch = (async (u: string) => {
    asked.push(u);
    if (u.endsWith("/a"))
      return left-- > 0
        ? { status: 202 }
        : { status: 200, arrayBuffer: async () => new Uint8Array([1, 2]).buffer };
    return { status: 404 };
  }) as unknown as typeof globalThis.fetch;
  const got: [string, number[]][] = [];
  const waits: number[] = [];
  await fetchForesee(["a", "b"], (t, x) => got.push([t, [...x]]), {
    base: "https://h/",
    fetch,
    wait: async (ms) => void waits.push(ms),
  });
  expect(got).toEqual([["a", [1, 2]]]);
  expect(asked.filter((u) => u === "https://h/foresee/a")).toHaveLength(3);
  expect(asked.filter((u) => u === "https://h/foresee/b")).toHaveLength(1);
  expect(waits).toHaveLength(2);
});

test("a table never ready: given up after the tries, or when told to stop", async () => {
  let n = 0;
  const fetch = (async () => (n++, { status: 202 })) as unknown as typeof globalThis.fetch;
  await fetchForesee(["a"], () => {}, { base: "https://h/", fetch, wait: async () => {} });
  expect(n).toBe(FORESEE_TRIES);
  n = 0;
  await fetchForesee(["a"], () => {}, {
    base: "https://h/",
    fetch,
    wait: async () => {},
    still: () => n < 3,
  });
  expect(n).toBe(3);
});
