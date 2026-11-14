/**
 * The drawing thread's WebGPU device lost (host/vngpu.js `device.lost`): render/mod.rs makes
 * everything again on a new device and says so in `vnhost_render_lost_state` (losses come back
 * from << 2, | 2 when no new device was had, | 1 while the drawing is stopped). What the page
 * tells the player when that changes.
 */

export const LOST_STOPPED = 1;
export const LOST_FOR_GOOD = 2;

export interface LostNotice {
  kind: "error" | "info";
  text: string;
  /** a reload button */
  reload: boolean;
}

/** What to say when the state went from `prev` to `now` (null: nothing). */
export function lostNotice(prev: number, now: number): LostNotice | null {
  if (now & LOST_FOR_GOOD && !(prev & LOST_FOR_GOOD))
    return {
      kind: "error",
      text: "GPU が使えなくなり、描画を再開できませんでした。ページを再読み込みしてください（ゲーム内でセーブしてから）。",
      reload: true,
    };
  if (now >>> 2 > prev >>> 2)
    return {
      kind: "info",
      text: "GPU がリセットされたため描画を作り直しました。次に描かれるまで一部が黒く見えることがあります。",
      reload: false,
    };
  return null;
}
