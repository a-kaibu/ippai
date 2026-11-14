// The drawing surface's size in device pixels for a canvas of `w` x `h` CSS pixels (play.ts:
// kura's _vnhost_render_resize): at least 1 x 1 (a canvas hidden a moment by CSS, a layout pass
// in a fullscreen or orientation change, measured 0 x 0) and at most `max` on a side (a 5K or 8K
// display at dpr 2 against a phone-class GPU's 8192), scaled down keeping its shape; under
// panic=abort a surface WebGPU refuses ended the game.

/** The largest side a surface is given (WebGPU's guaranteed maxTextureDimension2D). */
export const SURFACE_MAX = 8192;

export function surfaceSize(w: number, h: number, dpr: number, max = SURFACE_MAX): [number, number] {
  let x = Math.floor((Number.isFinite(w) ? w : 0) * (dpr > 0 && Number.isFinite(dpr) ? dpr : 1));
  let y = Math.floor((Number.isFinite(h) ? h : 0) * (dpr > 0 && Number.isFinite(dpr) ? dpr : 1));
  const big = Math.max(x, y);
  if (big > max) {
    x = Math.floor((x * max) / big);
    y = Math.floor((y * max) / big);
  }
  return [Math.min(max, Math.max(1, x)), Math.min(max, Math.max(1, y))];
}
