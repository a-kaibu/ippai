// How many crash reports the page's own errors make (crashlog.ts fatal). Each report grabs the
// screen, writes an OPFS file and uploads it: one recurring error (a rejected promise in a
// polling loop, the browser's benign ResizeObserver notice) used to make one per occurrence,
// which on a phone is itself enough to get the tab killed.

/** Window errors the browser raises that say nothing is wrong. */
const BENIGN = /ResizeObserver loop (completed with undelivered notifications|limit exceeded)/;

export class ReportLimit {
  private sent = 0;
  private last = -Infinity;
  private seen = new Map<string, number>();

  /**
   * `free` reports at once, then one per `every` ms, `max` in all; the same first line again
   * within `again` ms makes none.
   */
  constructor(
    private free = 3,
    private every = 60_000,
    private max = 20,
    private again = 10 * 60_000,
  ) {}

  /** Whether an error with `reason` gets a report now. */
  allow(reason: string, now = Date.now()): boolean {
    if (BENIGN.test(reason)) return false;
    const key = reason.split("\n", 1)[0]!.slice(0, 300);
    const prev = this.seen.get(key);
    if (prev !== undefined && now - prev < this.again) return false;
    if (this.sent >= this.max) return false;
    if (this.sent >= this.free && now - this.last < this.every) return false;
    this.seen.set(key, now);
    this.sent++;
    this.last = now;
    return true;
  }
}
