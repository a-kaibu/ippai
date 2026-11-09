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

/** What the page shows now (kura's composed screen, read back: crates/kura/src/render), as BGRA
 * pixels copied out of the wasm heap; null when there is none or this build cannot. */
export async function grabScreen(m: ShotModule): Promise<{ px: Uint8Array; w: number; h: number } | null> {
  if (!m._vnhost_render_shot || !m._malloc || !m._free) return null;
  const p = m._malloc(12) >>> 0;
  try {
    const first = m._vnhost_render_shot(p);
    for (let k = 0; k < 25 && m._vnhost_render_shot(p) === first; k++)
      await new Promise((r) => setTimeout(r, 20));
    const [ptr, w, h] = Array.from(new Uint32Array(m.wasmMemory.buffer, p, 3)) as [number, number, number];
    if (!ptr || !w || !h) return null;
    return { px: new Uint8Array(m.wasmMemory.buffer, ptr, w * h * 4).slice(), w, h };
  } finally {
    m._free(p);
  }
}

/** A small JPEG of BGRA pixels, as a data URL ("" where the browser cannot). */
export async function smallJpeg(px: Uint8Array, w: number, h: number, width = 160): Promise<string> {
  try {
    const rgba = new Uint8ClampedArray(px);
    for (let i = 0; i < rgba.length; i += 4) [rgba[i], rgba[i + 2]] = [rgba[i + 2]!, rgba[i]!];
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
