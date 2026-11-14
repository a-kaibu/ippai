// What the page showed, in a line of the run's log (crashlog.ts): a black screen with the game
// running (AMBITIOUS MISSION AE1, 2026-11-09) is told from a picture without a screenshot.

/** BGRA pixels (rows of `w`), sampled on a grid of at most 64x36 points. */
export interface ScreenStats {
  w: number;
  h: number;
  /** the share of points that are not near black (any channel above 24), 0..1 */
  lit: number;
  /** the points' mean color, RGB */
  mean: [number, number, number];
  /** distinct colors among the points (1: one flat color) */
  colors: number;
}

export function screenStats(px: Uint8Array, w: number, h: number): ScreenStats | null {
  if (w <= 0 || h <= 0 || px.length < w * h * 4) return null;
  const gx = Math.min(64, w),
    gy = Math.min(36, h);
  let lit = 0;
  const sum = [0, 0, 0];
  const seen = new Set<number>();
  for (let j = 0; j < gy; j++)
    for (let i = 0; i < gx; i++) {
      const x = Math.floor(((i + 0.5) * w) / gx),
        y = Math.floor(((j + 0.5) * h) / gy);
      const o = (y * w + x) * 4;
      const [b, g, r] = [px[o]!, px[o + 1]!, px[o + 2]!];
      if (r > 24 || g > 24 || b > 24) lit++;
      sum[0] += r;
      sum[1] += g;
      sum[2] += b;
      if (seen.size < 1000) seen.add((r << 16) | (g << 8) | b);
    }
  const n = gx * gy;
  return {
    w,
    h,
    lit: lit / n,
    mean: [sum[0]! / n, sum[1]! / n, sum[2]! / n].map(Math.round) as [number, number, number],
    colors: seen.size,
  };
}

/** The log line: "screen 960x540: 87% lit, mean 120,98,77, 412 colors". */
export function screenLine(s: ScreenStats | null): string {
  if (!s) return "screen: none";
  return `screen ${s.w}x${s.h}: ${Math.round(s.lit * 100)}% lit, mean ${s.mean.join(",")}, ${s.colors} color${s.colors === 1 ? "" : "s"}${s.lit === 0 ? " (black)" : ""}`;
}

export interface ShotModule {
  wasmMemory: WebAssembly.Memory;
  _malloc?(n: number): number;
  _free?(p: number): void;
  _vnhost_render_shot?(out: number): number;
}

/** Waits for a new picture: SHOT_WAITS of SHOT_WAIT_MS, up to 4 s (fewer wasm calls than 20 ms
 * ones: a game waiting for a click draws nothing, and the page polled 50 times a second). */
const SHOT_WAITS = 80;
const SHOT_WAIT_MS = 50;

/** A new picture of what the page shows (kura's composed screen, read back:
 * crates/kura/src/render), handed to `use` as BGRA pixels in the wasm heap (valid only during
 * the call); null when there is none, no new one came in time (not the older one: that was taken
 * for the black screen of a game showing a picture) or this build cannot. */
export async function withShot<T>(
  m: ShotModule,
  use: (px: Uint8Array, w: number, h: number) => T,
): Promise<T | null> {
  if (!m._vnhost_render_shot || !m._malloc || !m._free) return null;
  const p = m._malloc(12) >>> 0;
  if (!p) return null;
  try {
    // (a new one: the drawing thread takes it at its next frame and reads it back, which on a
    // busy phone took longer than half a second; the last one before it was the black first)
    const first = m._vnhost_render_shot(p);
    for (let k = 0; k < SHOT_WAITS && m._vnhost_render_shot(p) === first; k++)
      await new Promise((r) => setTimeout(r, SHOT_WAIT_MS));
    if (m._vnhost_render_shot(p) === first && first !== 0) return null;
    const [ptr, w, h] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3)) as [number, number, number];
    if (!ptr || !w || !h || ptr + w * h * 4 > m.wasmMemory.buffer.byteLength) return null;
    return use(new Uint8Array(m.wasmMemory.buffer, ptr, w * h * 4), w, h);
  } finally {
    m._free(p);
  }
}

/** What the page shows now as BGRA pixels copied out of the wasm heap (withShot). */
export function grabScreen(m: ShotModule): Promise<{ px: Uint8Array; w: number; h: number } | null> {
  return withShot(m, (px, w, h) => ({ px: px.slice(), w, h }));
}

/** The statistics of what the page shows now, read in place: not the whole frame copied out
 * (8 MB at 1080p) for 64x36 points of it. */
export async function grabStats(m: ShotModule): Promise<ScreenStats | null> {
  return (await withShot(m, screenStats)) ?? null;
}

/** BGRA pixels as RGBA (a copy), four bytes at a time. */
export function bgraToRgba(px: Uint8Array): Uint8ClampedArray<ArrayBuffer> {
  const out = new Uint8ClampedArray(px.length);
  out.set(px);
  const n = px.length >>> 2;
  const v = new Uint32Array(out.buffer, 0, n);
  // (little endian: B G R A -> R G B A, the first and third bytes swapped)
  for (let i = 0; i < n; i++) {
    const x = v[i]!;
    v[i] = (x & 0xff00ff00) | ((x & 0xff) << 16) | ((x >>> 16) & 0xff);
  }
  return out;
}

/** A small JPEG of BGRA pixels, as a data URL ("" where the browser cannot). */
export async function smallJpeg(px: Uint8Array, w: number, h: number, width = 160): Promise<string> {
  try {
    const rgba = bgraToRgba(px);
    const full = new OffscreenCanvas(w, h);
    full.getContext("2d")!.putImageData(new ImageData(rgba, w, h), 0, 0);
    const th = Math.max(1, Math.round((h * width) / w));
    const small = new OffscreenCanvas(width, th);
    small.getContext("2d")!.drawImage(full, 0, 0, width, th);
    const blob = await small.convertToBlob({ type: "image/jpeg", quality: 0.6 });
    const b = new Uint8Array(await blob.arrayBuffer());
    let s = "";
    for (let i = 0; i < b.length; i += 0x8000) s += String.fromCharCode(...b.subarray(i, i + 0x8000));
    return `data:image/jpeg;base64,${btoa(s)}`;
  } catch {
    return "";
  }
}

/** Screen lines every 10 s at first (crashlog's screen()). */
export const SCREEN_FIRST = 3;
/** Then one every SCREEN_EVERY. */
export const SCREEN_EVERY = 60_000;

/** Whether the log's next screen line is due: `n` taken so far, the last at `last`. */
export function screenDue(n: number, last: number, now: number, hidden: boolean): boolean {
  if (hidden) return false;
  return n < SCREEN_FIRST || now - last >= SCREEN_EVERY - 1000;
}
