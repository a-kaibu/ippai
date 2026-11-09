import { describe, expect, it } from "vite-plus/test";
import { grabScreen, screenLine, screenStats } from "./screen-stats";

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
