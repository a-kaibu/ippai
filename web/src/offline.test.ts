import { describe, expect, it } from "vite-plus/test";
import { breakdown, chunkFiles, PREFIX_PART, stateOf, TOP_PART, type Saved } from "./offline";
import { treeChunks } from "./net";

const rec = (status: Saved["status"]): Saved => ({ trees: ["prefix32", "g"], status });

describe("stateOf", () => {
  it("is complete only when saved to the end and every chunk is still here", () => {
    expect(stateOf(rec("done"), false, 10, 10)).toEqual({ status: "done", complete: true });
    // (the browser's storage cleared in part)
    expect(stateOf(rec("done"), false, 9, 10)).toEqual({ status: "done", complete: false });
    // every chunk read while playing, never saved: the app's files may be missing
    expect(stateOf(undefined, false, 10, 10)).toEqual({ status: "none", complete: false });
  });
  it("tells a stopped or failed save from a finished one", () => {
    expect(stateOf(rec("stopped"), false, 10, 10).complete).toBe(false);
    expect(stateOf(rec("failed"), false, 3, 10)).toEqual({ status: "failed", complete: false });
  });
  it("reads a save left running by a closed page as stopped", () => {
    expect(stateOf(rec("saving"), true, 3, 10)).toEqual({ status: "saving", complete: false });
    expect(stateOf(rec("saving"), false, 3, 10)).toEqual({ status: "stopped", complete: false });
    expect(stateOf(rec("saving"), false, 10, 10).complete).toBe(false);
  });
});

const h = (c: string) => c.repeat(32);
const tsv = (lines: string[]) => ["vnvm-tree\t2\t4194304", ...lines].join("\n");
const file = (path: string, chunks: [string, number][]) =>
  `f\t644\t1\t${path}\t${chunks.reduce((a, c) => a + c[1], 0)}\t${chunks.map(([x, n]) => `${x}:${n}`).join(",")}`;

describe("breakdown", () => {
  const game = {
    name: "kinkoi",
    tsv: tsv([
      file("Gameexe.dat", [[h("a"), 10]]),
      "d\t755\t1\tg00",
      file("g00/a.g00", [[h("b"), 100]]),
      file("g00/b.g00", [[h("c"), 300]]),
      file("koe/z.ovk", [
        [h("d"), 1000],
        [h("e"), 1000],
      ]),
      // (all zero: no chunk) and one the game shares with the prefix
      file("koe/empty", [["", 50]]),
      file("dat/font.ttf", [[h("f"), 40]]),
    ]),
  };
  const prefix = { name: "prefix32", tsv: tsv([file("windows/Fonts/msgothic.ttf", [[h("f"), 40]])]) };

  it("splits a game by its top folders, the prefix as one, the biggest first", () => {
    const parts = breakdown([prefix, game], new Set([h("b"), h("d"), h("f")]));
    expect(parts).toEqual([
      { name: "koe", files: 2, bytes: 2000, have: 1000 },
      { name: "g00", files: 2, bytes: 400, have: 100 },
      { name: PREFIX_PART, files: 1, bytes: 40, have: 40 },
      { name: TOP_PART, files: 1, bytes: 10, have: 0 },
      // (its chunk counted in the prefix already)
      { name: "dat", files: 1, bytes: 0, have: 0 },
    ]);
  });

  it("names another mounted tree's parts by the tree", () => {
    const dlc = { name: "dlc", tsv: tsv([file("g00/x.g00", [[h("9"), 5]])]) };
    expect(breakdown([game, dlc], new Set()).map((p) => p.name)).toContain("dlc: g00");
    expect(breakdown([game, dlc], new Set()).map((p) => p.name)).toContain("kinkoi: koe");
  });

  it("random trees: the parts add up to the trees' chunks, each once, never more on the device than in all", () => {
    let seed = 5;
    const rnd = (n: number) => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) % n;
    for (let i = 0; i < 300; i++) {
      const lens = new Map<string, number>();
      const trees = ["prefix32", "g", ...(rnd(3) ? [] : ["m"])].map((name) => ({
        name,
        tsv: tsv(
          Array.from({ length: rnd(12) }, (_, k) =>
            file(
              `${["a", "b", "c"][rnd(3)]}${rnd(2) ? "/" : "_"}f${k}`,
              Array.from({ length: 1 + rnd(3) }, () => {
                const x = rnd(20).toString(16).padStart(32, "0");
                if (!lens.has(x)) lens.set(x, 1 + rnd(1000));
                return [x, lens.get(x)!] as [string, number];
              }),
            ),
          ),
        ),
      }));
      const on = new Set([...lens.keys()].filter(() => rnd(2) === 0));
      const parts = breakdown(trees, on);
      const used = new Set(trees.flatMap((t) => treeChunks(t.tsv)));
      expect(parts.reduce((a, p) => a + p.bytes, 0)).toBe([...used].reduce((a, x) => a + lens.get(x)!, 0));
      expect(parts.reduce((a, p) => a + p.have, 0)).toBe(
        [...used].filter((x) => on.has(x)).reduce((a, x) => a + lens.get(x)!, 0),
      );
      for (const p of parts) expect(p.have).toBeLessThanOrEqual(p.bytes);
      for (let k = 1; k < parts.length; k++)
        expect(parts[k - 1]!.bytes).toBeGreaterThanOrEqual(parts[k]!.bytes);
    }
  });
});

describe("chunkFiles", () => {
  it("gives each chunk its first file, the prefix's as C:\\", () => {
    const m = chunkFiles([
      { name: "prefix32", tsv: tsv([file("windows/Fonts/a.ttf", [[h("1"), 1]])]) },
      {
        name: "g",
        tsv: tsv([
          file("g00/a.g00", [[h("2"), 1]]),
          file("g00/b.g00", [
            [h("2"), 1],
            [h("3"), 1],
          ]),
        ]),
      },
    ]);
    expect(m.get(h("1"))).toBe("C:\\windows/Fonts/a.ttf");
    expect(m.get(h("2"))).toBe("g00/a.g00");
    expect(m.get(h("3"))).toBe("g00/b.g00");
    expect(m.size).toBe(3);
  });
});
