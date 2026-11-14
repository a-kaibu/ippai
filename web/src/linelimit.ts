// How many of the guest's printed lines reach the console (play.ts print/printErr): a game or a
// sake trace printing per frame or per API call (unimplemented-call warnings) made a
// console.log, its formatting and its memory per line on the page's thread for the whole
// session. BURST lines at once, then RATE a second; the lines left out are counted and told in
// one line when lines are shown again.

export class LineLimit {
  private tokens: number;
  private last: number;
  private dropped = 0;
  constructor(
    readonly rate = 50,
    readonly burst = 500,
    now = Date.now(),
  ) {
    this.tokens = burst;
    this.last = now;
  }
  /** Whether a line may be shown now; `told` gets the count of the lines left out before it. */
  allow(now = Date.now(), told?: (dropped: number) => void): boolean {
    this.tokens = Math.min(this.burst, this.tokens + ((now - this.last) / 1000) * this.rate);
    this.last = Math.max(this.last, now);
    if (this.tokens < 1) {
      this.dropped++;
      return false;
    }
    this.tokens--;
    if (this.dropped) {
      told?.(this.dropped);
      this.dropped = 0;
    }
    return true;
  }
}
