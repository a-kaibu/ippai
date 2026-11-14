import { describe, expect, it, vi } from "vite-plus/test";
import {
  bgraToRgba,
  grabScreen,
  grabStats,
  SCREEN_EVERY,
  SCREEN_FIRST,
  screenDue,
  screenLine,
  screenStats,
} from "./screen-stats";

const image = (w: number, h: number, f: (x: number, y: number) => [number, number, number]) => {
  const px = new Uint8Array(w * h * 4);
  for (let y = 0; y < h; y++)
    for (let x = 0; x < w; x++) {
      const [r, g, b] = f(x, y);
      px.set([b, g, r, 255], (y * w + x) * 4);
    }
  return px;
};

describe("screenStats", () => {
  it("tells a black screen", () => {
    const s = screenStats(
      image(960, 540, () => [0, 0, 0]),
      960,
      540,
    )!;
    expect(s).toEqual({ w: 960, h: 540, lit: 0, mean: [0, 0, 0], colors: 1 });
    expect(screenLine(s)).toBe("screen 960x540: 0% lit, mean 0,0,0, 1 color (black)");
  });
  it("tells a picture: the lit share, the mean, the colors (BGRA read as RGB)", () => {
    // left half red, right half black
    const s = screenStats(
      image(640, 360, (x) => (x < 320 ? [200, 0, 0] : [0, 0, 0])),
      640,
      360,
    )!;
    expect(s.lit).toBe(0.5);
    expect(s.mean).toEqual([100, 0, 0]);
    expect(s.colors).toBe(2);
    expect(screenLine(s)).toBe("screen 640x360: 50% lit, mean 100,0,0, 2 colors");
  });
  it("counts near-black as dark", () => {
    expect(
      screenStats(
        image(8, 8, () => [24, 24, 24]),
        8,
        8,
      )!.lit,
    ).toBe(0);
    expect(
      screenStats(
        image(8, 8, () => [25, 0, 0]),
        8,
        8,
      )!.lit,
    ).toBe(1);
  });
  it("refuses what is not an image", () => {
    expect(screenStats(new Uint8Array(10), 4, 4)).toBeNull();
    expect(screenStats(new Uint8Array(0), 0, 0)).toBeNull();
    expect(screenLine(null)).toBe("screen: none");
  });
  // seeded random images: within bounds, and a flat image is one color, lit or not by its value
  it("holds for any image", () => {
    let seed = 7;
    const r = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
    for (let k = 0; k < 200; k++) {
      const w = 1 + Math.floor(r() * 200),
        h = 1 + Math.floor(r() * 120);
      const flat = r() < 0.5;
      const c: [number, number, number] = [
        Math.floor(r() * 256),
        Math.floor(r() * 256),
        Math.floor(r() * 256),
      ];
      const px = image(w, h, () =>
        flat ? c : [Math.floor(r() * 256), Math.floor(r() * 256), Math.floor(r() * 256)],
      );
      const s = screenStats(px, w, h)!;
      expect(s.lit).toBeGreaterThanOrEqual(0);
      expect(s.lit).toBeLessThanOrEqual(1);
      for (const m of s.mean) expect(m >= 0 && m <= 255).toBe(true);
      expect(s.colors).toBeGreaterThanOrEqual(1);
      if (flat) {
        expect(s.colors).toBe(1);
        expect(s.mean).toEqual(c);
        expect(s.lit).toBe(c.some((v) => v > 24) ? 1 : 0);
      }
    }
  });
});

/** A module whose drawing thread finishes a shot after `after` asks (0: never a new one). */
function shots(after: number, start = 1) {
  const memory = new WebAssembly.Memory({ initial: 1 });
  const heap = new Uint8Array(memory.buffer);
  let asks = 0;
  let serial = start;
  // the picture: 2x1 pixels at 1024 (the first, black, then a lit one)
  heap.set([0, 0, 0, 255, 0, 0, 0, 255], 1024);
  return {
    wasmMemory: memory,
    _malloc: () => 2048,
    _free: () => {},
    _vnhost_render_shot(out: number) {
      asks++;
      if (after && asks === after) {
        serial++;
        heap.set([10, 200, 30, 255, 10, 200, 30, 255], 1024);
      }
      new Uint32Array(memory.buffer, out, 3).set([1024, 2, 1]);
      return serial;
    },
  };
}

describe("grabScreen", () => {
  it("waits for the drawing thread's new picture (a slow phone's), not the older black one", async () => {
    const got = await grabScreen(shots(30));
    expect(got && Array.from(got.px.slice(0, 3))).toEqual([10, 200, 30]);
  });
  it("gives nothing when no new picture comes (rather than the old one)", async () => {
    expect(await grabScreen(shots(0))).toBeNull();
  }, 10_000);
  it("takes the first picture when there was none before", async () => {
    const got = await grabScreen(shots(2, 0));
    expect(got?.w).toBe(2);
  });
});

/** A module whose drawing thread hands a new picture after `after` looks (never: Infinity). */
function shotModule(w: number, h: number, px: Uint8Array, after: number) {
  const mem = new WebAssembly.Memory({ initial: Math.ceil((w * h * 4 + 4096) / 65536) + 1 });
  const at = 1024;
  new Uint8Array(mem.buffer).set(px, at);
  let looks = 0;
  return {
    looks: () => looks,
    m: {
      wasmMemory: mem,
      _malloc: () => 16,
      _free: () => {},
      _vnhost_render_shot: (out: number) => {
        looks++;
        new Uint32Array(mem.buffer, out, 3).set([at, w, h]);
        return looks > after ? 2 : 1;
      },
    },
  };
}

describe("shots", () => {
  it("the stats read in place are those of the copied frame (differential, seeded)", async () => {
    let seed = 3;
    const r = (n: number) => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) % n;
    for (let run = 0; run < 20; run++) {
      const w = 1 + r(200),
        h = 1 + r(120);
      const px = Uint8Array.from({ length: w * h * 4 }, () => r(256));
      const a = shotModule(w, h, px, 1);
      const b = shotModule(w, h, px, 1);
      const shot = await grabScreen(a.m);
      expect(await grabStats(b.m)).toEqual(screenStats(shot!.px, shot!.w, shot!.h));
    }
  });
  it("a screen that does not change is polled at most 82 times in its 4 s, then none", async () => {
    vi.useFakeTimers();
    try {
      const s = shotModule(4, 4, new Uint8Array(64), Infinity);
      const got = grabStats(s.m);
      await vi.advanceTimersByTimeAsync(5000);
      expect(await got).toBeNull();
      expect(s.looks()).toBeLessThanOrEqual(82);
    } finally {
      vi.useRealTimers();
    }
  });
  it("a frame said to lie past the heap is not read", async () => {
    const s = shotModule(4, 4, new Uint8Array(64), 1);
    s.m._vnhost_render_shot = (out: number) => (
      new Uint32Array(s.m.wasmMemory.buffer, out, 3).set([1 << 30, 4, 4]),
      2
    );
    expect(await grabStats(s.m)).toBeNull();
  });
  it("BGRA to RGBA four bytes at a time is the byte swap (property, seeded)", () => {
    let seed = 11;
    const r = (n: number) => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) % n;
    for (let run = 0; run < 200; run++) {
      const px = Uint8Array.from({ length: 4 * r(50) }, () => r(256));
      const want = Uint8ClampedArray.from(px);
      for (let i = 0; i < want.length; i += 4) [want[i], want[i + 2]] = [want[i + 2]!, want[i]!];
      expect(bgraToRgba(px)).toEqual(want);
    }
  });
  it("screen lines: the first few, then one a minute, none hidden", () => {
    const at: number[] = [];
    let n = 0,
      last = 0;
    for (let t = 10_000; t <= 600_000; t += 10_000)
      if (screenDue(n, last, t, false)) {
        at.push(t);
        n++;
        last = t;
      }
    expect(at.slice(0, SCREEN_FIRST)).toEqual([10_000, 20_000, 30_000]);
    for (let i = SCREEN_FIRST; i < at.length; i++)
      expect(at[i]! - at[i - 1]!).toBeGreaterThanOrEqual(SCREEN_EVERY - 1000);
    expect(at.length).toBeLessThanOrEqual(SCREEN_FIRST + 600_000 / SCREEN_EVERY);
    expect(screenDue(0, 0, 1, true)).toBe(false);
  });
});
