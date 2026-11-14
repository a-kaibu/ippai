import { expect, test } from "vite-plus/test";
import { HideKeeper, MIN_RUN_MS, type HideDeps } from "./hidden";
import type { Stopped } from "./savestate";

// The hidden-tab keeper (hidden.ts) against a model of the module: a program that runs or
// stands, stops that take a while and may fail, saves that take a while. Random sequences of
// the page's events (hidden, seen, pagehide, freeze, time passing), each awaited or not; after
// each the invariants, and once the events settle the program stands exactly when the page is
// out of sight.

class Rng {
  constructor(private s: number) {}
  next(): number {
    this.s = (this.s * 1103515245 + 12345) & 0x7fffffff;
    return this.s / 0x7fffffff;
  }
  pick<T>(a: T[]): T {
    return a[Math.floor(this.next() * a.length)]!;
  }
}

const tick = () => new Promise<void>((r) => queueMicrotask(r));

class World {
  hidden = false;
  time = 0;
  standing = 0;
  /** the program's own clock: ms it ran */
  ran = 0;
  states: number[] = [];
  flushes = 0;
  /** flushes when each stop began */
  flushedAt: number[] = [];
  failTake = false;
  failPause = false;
  stir = false;
  canState = true;
  log: string[] = [];
  violations: string[] = [];

  stopped(): Stopped {
    this.standing++;
    if (this.standing > 1) this.violations.push("stopped twice at once");
    let done = false;
    return {
      took: 1,
      stirred: () => false,
      settle: async () => {},
      pack: () => Promise.reject(new Error("no")),
      resume: async () => {
        await tick();
        if (done) this.violations.push("resumed twice");
        done = true;
        this.standing--;
      },
    };
  }

  deps(): HideDeps {
    return {
      hidden: () => this.hidden,
      now: () => this.time,
      log: (l) => this.log.push(l),
      canState: () => this.canState,
      flushSaves: async () => {
        await tick();
        this.flushes++;
      },
      take: async () => {
        this.flushedAt.push(this.flushes);
        await tick();
        if (this.failTake) throw new Error("still running: guest");
        const s = this.stopped();
        if (!this.stir) this.states.push(this.ran);
        return { stopped: s, kept: !this.stir };
      },
      pause: async () => {
        this.flushedAt.push(this.flushes);
        await tick();
        if (this.failPause) throw new Error("still running: guest");
        return this.stopped();
      },
    };
  }

  pass(ms: number): void {
    this.time += ms;
    if (!this.standing) this.ran += ms;
  }
}

async function settle(): Promise<void> {
  for (let i = 0; i < 60; i++) await tick();
}

test("the program stands exactly while out of sight, saves put away first", async () => {
  for (let seed = 1; seed <= 300; seed++) {
    const r = new Rng(seed);
    const w = new World();
    w.failTake = r.next() < 0.15;
    w.stir = r.next() < 0.15;
    w.canState = r.next() < 0.85;
    const k = new HideKeeper(w.deps());
    let leaving = false;
    const pending: Promise<void>[] = [];
    for (let step = 0; step < 30; step++) {
      const ev = r.pick(["hide", "show", "time", "time", "freeze", "pagehide", "await"]);
      if (ev === "hide") {
        w.hidden = true;
        pending.push(k.hide());
      } else if (ev === "show") {
        w.hidden = false;
        leaving = false;
        pending.push(k.show());
      } else if (ev === "freeze") {
        if (w.hidden) pending.push(k.hide());
      } else if (ev === "pagehide") {
        leaving = true;
        k.leave();
      } else if (ev === "time") {
        w.pass(Math.floor(r.next() * 8000));
      } else {
        await settle();
      }
      expect(w.violations, `seed ${seed}`).toEqual([]);
    }
    await Promise.all(pending);
    await settle();
    expect(w.violations, `seed ${seed}`).toEqual([]);
    // (settled: standing only out of sight; out of sight standing unless the stop failed)
    if (!w.hidden) expect(w.standing, `seed ${seed}: seen but standing`).toBe(0);
    else if (!leaving && !w.failPause) expect(w.standing, `seed ${seed}: hidden but running`).toBe(1);
    // every stop after a save of the game's files
    for (let i = 0; i < w.flushedAt.length; i++)
      expect(w.flushedAt[i], `seed ${seed}: stop ${i} before its save`).toBeGreaterThan(0);
    // a state again only after the program ran MIN_RUN_MS since the last one
    for (let i = 1; i < w.states.length; i++)
      expect(w.states[i]! - w.states[i - 1]!, `seed ${seed}: states too close`).toBeGreaterThanOrEqual(
        MIN_RUN_MS,
      );
    if (!w.canState) expect(w.states).toEqual([]);
    expect(k.kept).toBe(w.states.length);
  }
});

test("the first look away keeps a state; back and away at once only pauses", async () => {
  const w = new World();
  const k = new HideKeeper(w.deps());
  w.hidden = true;
  await k.hide();
  expect(w.standing).toBe(1);
  expect(w.states.length).toBe(1);
  w.hidden = false;
  await k.show();
  expect(w.standing).toBe(0);
  w.pass(1000);
  w.hidden = true;
  await k.hide();
  expect(w.standing).toBe(1);
  expect(w.states.length).toBe(1);
  w.hidden = false;
  await k.show();
  w.pass(MIN_RUN_MS);
  w.hidden = true;
  await k.hide();
  expect(w.states.length).toBe(2);
});

test("a state that can't be taken: the program still stands", async () => {
  const w = new World();
  w.failTake = true;
  const k = new HideKeeper(w.deps());
  w.hidden = true;
  await k.hide();
  expect(w.standing).toBe(1);
  expect(w.states).toEqual([]);
  expect(w.log.some((l) => l.includes("no state"))).toBe(true);
});

test("a stirred copy is not counted: the next look away takes one again", async () => {
  const w = new World();
  w.stir = true;
  const k = new HideKeeper(w.deps());
  w.hidden = true;
  await k.hide();
  expect(k.kept).toBe(0);
  w.hidden = false;
  await k.show();
  w.stir = false;
  w.pass(10);
  w.hidden = true;
  await k.hide();
  expect(k.kept).toBe(1);
});

test("a page going away keeps no state (its saves only)", async () => {
  const w = new World();
  const k = new HideKeeper(w.deps());
  k.leave();
  w.hidden = true;
  await k.hide();
  expect(w.flushes).toBe(1);
  expect(w.standing).toBe(0);
  expect(w.states).toEqual([]);
});

test("seen again before the stop ended: it goes on once the stop has", async () => {
  const w = new World();
  const k = new HideKeeper(w.deps());
  w.hidden = true;
  const h = k.hide();
  w.hidden = false;
  const s = k.show();
  await Promise.all([h, s]);
  await settle();
  expect(w.standing).toBe(0);
  expect(w.violations).toEqual([]);
});

test("away, back and away again before it went on: it stays standing (one stop)", async () => {
  const w = new World();
  const k = new HideKeeper(w.deps());
  w.hidden = true;
  const a = k.hide();
  w.hidden = false;
  const b = k.show();
  w.hidden = true;
  const c = k.hide();
  await Promise.all([a, b, c]);
  await settle();
  expect(w.standing).toBe(1);
  expect(k.paused).toBe(1);
});
