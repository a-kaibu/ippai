import { expect, test } from "vite-plus/test";
import {
  folderDirs,
  HELLO_AGAIN,
  HELLO_WAIT_MS,
  normPath,
  SAVE_KEYS_MAX,
  SAVE_SOON_MS,
  SaveItems,
  SAVES_MAX,
} from "./webstore";

let x = 23;
const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;

test("folders of a tree's files", () => {
  expect([...folderDirs(["a/b/c.png", "a/d.txt", "e.js"])].sort()).toEqual(["", "a", "a/b"]);
  expect(folderDirs([]).size).toBe(0);
});

test("existsSync of a folder: the same answers as the old scan of every name (differential, seeded)", () => {
  const part = () => ["img", "js", "data", "a", "ab", "日本"][rnd(6)]!;
  for (let run = 0; run < 400; run++) {
    const names = new Set<string>();
    for (let k = rnd(30); k > 0; k--)
      names.add(Array.from({ length: 1 + rnd(4) }, part).join("/") + (rnd(2) ? ".png" : ""));
    const dirs = folderDirs(names);
    for (let q = 0; q < 30; q++) {
      const raw =
        [...Array.from({ length: rnd(4) }, part), ...(rnd(3) ? [] : [".", "..", ""])].join(
          rnd(2) ? "/" : "\\",
        ) + (rnd(4) ? "" : "/");
      const p = normPath(raw);
      // (the top, ".": there now when the tree has files; the old scan said no)
      const old =
        p === ""
          ? names.size > 0
          : names.has(p) || [...names].some((n) => n.startsWith(p.replace(/\/$/, "") + "/"));
      expect(names.has(p) || dirs.has(p), `${raw} in ${[...names].join(",")}`).toBe(old);
    }
  }
});

test("normPath: no empty, '.' or '..' parts; the same again (property)", () => {
  for (let run = 0; run < 2000; run++) {
    const raw = Array.from({ length: rnd(6) }, () => ["a", "..", ".", "", "b c", "x\\y"][rnd(6)]).join("/");
    const p = normPath(raw);
    expect(normPath(p)).toBe(p);
    if (p) for (const s of p.split("/")) expect(["", ".", ".."]).not.toContain(s);
  }
});

test("SaveItems: total is what the map holds, never past the limits; a refused set changes nothing (property)", () => {
  const s = new SaveItems();
  const big = "x".repeat(SAVES_MAX / 4);
  for (let step = 0; step < 3000; step++) {
    const key = `k${rnd(12)}`;
    const value = rnd(5) === 0 ? null : rnd(20) === 0 ? big + rnd(9) : "v".repeat(rnd(50));
    const before = [...s.map];
    const ok = s.set(key, value);
    if (!ok) expect([...s.map]).toEqual(before);
    let sum = 0;
    for (const [k, v] of s.map) sum += k.length + v.length;
    expect(s.total).toBe(sum);
    expect(s.total).toBeLessThanOrEqual(SAVES_MAX);
  }
  // (past the keys allowed: a new key refused, an old one still changed)
  const t = new SaveItems();
  for (let i = 0; i < SAVE_KEYS_MAX; i++) expect(t.set(`k${i}`, "")).toBe(true);
  expect(t.set("one more", "")).toBe(false);
  expect(t.set("k0", "changed")).toBe(true);
  expect(t.set("k1", null)).toBe(true);
  expect(t.set("one more", "")).toBe(true);
});

test("the frame asks again while the app is silent, then goes on; the app stores soon after a change", () => {
  expect(HELLO_AGAIN.every((ms, i) => ms < HELLO_WAIT_MS && (i === 0 || ms > HELLO_AGAIN[i - 1]!))).toBe(
    true,
  );
  expect(SAVE_SOON_MS).toBeLessThan(1000);
});
