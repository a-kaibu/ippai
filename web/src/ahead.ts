/**
 * Limits for work the page does ahead of the game: how many at once, and how many bytes.
 */

/** `jobs`, `width` at a time (the next starts as one ends); their results in order. The first
 * failure rejects it and no job starts after (before, the other runners went on and their later
 * failures went unobserved: an unhandled rejection, a crash report, each). */
export async function pool<T>(width: number, jobs: (() => Promise<T>)[]): Promise<T[]> {
  const out: T[] = [];
  let next = 0;
  let failed = false;
  const one = async () => {
    while (next < jobs.length && !failed) {
      const i = next++;
      try {
        out[i] = await jobs[i]!();
      } catch (e) {
        failed = true;
        throw e;
      }
    }
  };
  await Promise.all(Array.from({ length: Math.max(1, Math.min(width, jobs.length)) }, one));
  return out;
}

/**
 * Bytes fetched ahead: at most `before` until `start()` (they are held until then), `total` in
 * all. A fetch reserves its bytes before it starts (its length known from the tree lists), so
 * fetches side by side never take more between them.
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
  /** Reserves `n` bytes, waiting while they would pass what may be held before the start;
   * false (nothing reserved) when they would pass the total. */
  async reserve(n: number): Promise<boolean> {
    for (;;) {
      if (this.spent + n > this.total) return false;
      if (this.started || this.spent + n <= this.before) {
        this.spent += n;
        return true;
      }
      // (more than may ever be held: after the start)
      await this.woken;
    }
  }
  /** Gives back a reservation (the fetch failed), or the part of it not used. */
  release(n: number): void {
    this.spent -= n;
  }
  /** No reservation of `n` bytes can be had any more. */
  full(n = 1): boolean {
    return this.spent + n > this.total;
  }
  start(): void {
    this.started = true;
    this.wake();
  }
}
