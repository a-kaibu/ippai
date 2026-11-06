/**
 * Limits for work the page does ahead of the game: how many at once, and how many bytes.
 */

/** `jobs`, `width` at a time (the next starts as one ends); their results in order. */
export async function pool<T>(width: number, jobs: (() => Promise<T>)[]): Promise<T[]> {
  const out: T[] = [];
  let next = 0;
  const one = async () => {
    while (next < jobs.length) {
      const i = next++;
      out[i] = await jobs[i]!();
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(width, jobs.length)) }, one));
  return out;
}

/**
 * Bytes fetched ahead: at most `before` until `start()` (they are held until then), `total` in
 * all.
 */
export class Budget {
  spent = 0;
  private started = false;
  private wake = () => {};
  private woken = new Promise<void>((r) => (this.wake = r));
  constructor(
    readonly before: number,
    readonly total: number,
  ) {}
  /** Waits while held bytes are at the limit; false once the total is spent. */
  async room(): Promise<boolean> {
    if (!this.started && this.spent >= this.before) await this.woken;
    return this.spent < this.total;
  }
  add(n: number): void {
    this.spent += n;
  }
  start(): void {
    this.started = true;
    this.wake();
  }
}
