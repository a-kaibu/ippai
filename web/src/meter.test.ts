import { expect, test } from "vite-plus/test";

// A page that imports the meter (the game list does) runs no frame loop; one starts with a game
// attached, and ends at the first frame after it is detached; attaching the same module again
// keeps its buffer.
test("the frame loop runs only while a game is attached; one buffer per module", async () => {
  const g = globalThis as unknown as {
    requestAnimationFrame?: (f: () => void) => number;
    addEventListener?: unknown;
    window?: unknown;
    PerformanceObserver?: unknown;
  };
  const had = { raf: g.requestAnimationFrame, ael: g.addEventListener, w: g.window };
  let frames: (() => void)[] = [];
  g.requestAnimationFrame = (f) => (frames.push(f), frames.length);
  g.addEventListener ??= () => {};
  g.window ??= globalThis;
  try {
    const { Meter } = await import("./meter");
    const m = new Meter();
    expect(frames.length).toBe(0);
    let mallocs = 0;
    const wasm = {
      wasmMemory: new WebAssembly.Memory({ initial: 1 }),
      HEAPF64: new Float64Array(8),
      _malloc: () => (++mallocs, 1024),
    };
    m.attach(wasm as never);
    expect(frames.length).toBe(1);
    for (let i = 0; i < 3; i++) {
      const f = frames;
      frames = [];
      f.forEach((x) => x());
    }
    expect(frames.length).toBe(1);
    m.attach(wasm as never);
    expect(mallocs).toBe(1);
    // (attached again while looping: still one loop)
    expect(frames.length).toBe(1);
    m.detach();
    const f = frames;
    frames = [];
    f.forEach((x) => x());
    expect(frames.length).toBe(0);
    clearInterval((m as unknown as { timer: number }).timer);
  } finally {
    g.requestAnimationFrame = had.raf;
    if (!had.ael) delete g.addEventListener;
    if (!had.w) delete g.window;
  }
});
