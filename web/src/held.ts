// What of the mouse and keyboard the game was told is held down (play.ts attachInput), so that
// it can be told they were let go when the page stops seeing them: a touch a system gesture took
// over ends with pointercancel (no pointerup), and a key held as the window loses focus (Ctrl
// for skip, then an app switch) never gets its keyup; the game kept the button down or kept
// skipping text until the key was pressed again.

export class Held {
  private buttons = new Set<number>();
  private keys = new Map<string, number>();

  button(b: number, down: boolean): void {
    if (down) this.buttons.add(b);
    else this.buttons.delete(b);
  }
  /** a key (by its code) down with what the game was given for it, or up */
  key(code: string, down: boolean, data = 0): void {
    if (down) this.keys.set(code, data);
    else this.keys.delete(code);
  }
  /** Lets everything go: the buttons and the keys' data to send releases for, none held after. */
  release(): { buttons: number[]; keys: number[] } {
    const out = { buttons: [...this.buttons], keys: [...this.keys.values()] };
    this.buttons.clear();
    this.keys.clear();
    return out;
  }
  /** Lets the buttons go only (a pointer cancelled: the keys are still held). */
  releaseButtons(): number[] {
    const out = [...this.buttons];
    this.buttons.clear();
    return out;
  }
  get size(): number {
    return this.buttons.size + this.keys.size;
  }
}
