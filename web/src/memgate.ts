// The page's own writes into the module's memory (WebCodecs' decoded frames and samples, input,
// chunks read ahead), and a save state's copy of that memory (savestate.ts): while a state is
// taken the gate is shut, and nothing of the page may write. A write that goes on across awaits
// (VideoFrame.copyTo straight into the memory) holds the gate open (enter/leave), and shutting
// waits for those under way; what comes while it is shut (a decoder's output, a key pressed) is
// done once it opens again (run), in the order it came.

export class Gate {
  private shut = false;
  private active = 0;
  private idle: (() => void)[] = [];
  private later: (() => void)[] = [];
  private keys = new Set<unknown>();

  /** Shut: a state is being taken. */
  get closed(): boolean {
    return this.shut;
  }

  /** `f` now when open, else once open again (with `key`: once, however often asked). */
  run(f: () => void, key?: unknown): void {
    if (!this.shut) return f();
    if (key !== undefined) {
      if (this.keys.has(key)) return;
      this.keys.add(key);
    }
    this.later.push(() => {
      if (key !== undefined) this.keys.delete(key);
      f();
    });
  }

  /** Starts a write that goes on across awaits: false (nothing started) when shut. */
  enter(): boolean {
    if (this.shut) return false;
    this.active++;
    return true;
  }
  leave(): void {
    if (--this.active === 0) for (const r of this.idle.splice(0)) r();
  }

  /** Shuts the gate and waits until the writes under way are done (rejects after `ms`, open
   * again). */
  async close(ms = 10000): Promise<void> {
    this.shut = true;
    if (!this.active) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      await Promise.race([
        new Promise<void>((r) => this.idle.push(r)),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("動画・音声のデコード結果の書き込みが終わりません")), ms);
        }),
      ]);
    } catch (e) {
      this.open();
      throw e;
    } finally {
      clearTimeout(timer);
    }
  }

  /** Opens again: what came meanwhile runs now, in order. */
  open(): void {
    this.shut = false;
    // (one run may shut it again: the rest wait for that one)
    while (!this.shut && this.later.length) this.later.shift()!();
  }

  /** Writes under way (tests). */
  get busy(): number {
    return this.active;
  }
}

/** The one gate of the page's module. */
export const memGate = new Gate();
