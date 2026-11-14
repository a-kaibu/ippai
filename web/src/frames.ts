// One requestAnimationFrame loop for the page's own per-frame work (crashlog.ts's page fps and
// janks, meter.ts's frame count, play.ts's watch of each module's frames): each loop of its
// own was a callback more per frame on the page's thread, three at 60 Hz while a game ran.
// The loop runs only while someone listens.

type Tick = (t: number) => void;

export interface FrameSource {
  request(cb: (t: number) => void): unknown;
}

/** A frame ticker on `src` (the browser's requestAnimationFrame; tests pass their own). */
export function frameTicker(src: FrameSource) {
  const ticks = new Set<Tick>();
  let looping = false;
  const loop = (t: number) => {
    if (!ticks.size) {
      looping = false;
      return;
    }
    // (a listener added during this frame runs from the next; one that throws does not stop
    // the others or the loop)
    for (const f of Array.from(ticks)) {
      if (!ticks.has(f)) continue;
      try {
        f(t);
      } catch (e) {
        console.log(`frame: ${String(e)}`);
      }
    }
    // (the last listener gone during this frame: no more frames asked for)
    if (ticks.size) src.request(loop);
    else looping = false;
  };
  return {
    /** Calls `f` every frame until the function it returns is called. */
    each(f: Tick): () => void {
      ticks.add(f);
      if (!looping) {
        looping = true;
        src.request(loop);
      }
      return () => void ticks.delete(f);
    },
    /** listeners now (tests) */
    get size() {
      return ticks.size;
    },
  };
}

// (no frames where there is no requestAnimationFrame: node's tests)
const page = frameTicker({
  request: (cb) => (typeof requestAnimationFrame === "undefined" ? undefined : requestAnimationFrame(cb)),
});

/** The page's ticker: `f` every animation frame until the returned function is called. */
export const eachFrame = (f: Tick) => page.each(f);
