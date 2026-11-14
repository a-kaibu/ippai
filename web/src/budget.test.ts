import { expect, test, vi } from "vite-plus/test";
import source from "../../host/fetch-bridge.js?raw";
import {
  BURST,
  BYTES,
  CLASS,
  PAGE_TAG,
  RATE,
  RESERVE,
  TAB_GONE_MS,
  TAB_MS,
  drain,
  reclaim,
  shareAcrossTabs,
  tabShare,
  take,
  waitFor,
} from "./budget";

// host/fetch-bridge.js's copy, run on the same buffer: the page and the network worker share it
interface Bridge {
  MASU_CLASS: typeof CLASS;
  MASU_RATE: number;
  MASU_BURST: number;
  MASU_RESERVE: number[];
  masuBudgetInit(b?: SharedArrayBuffer, tag?: number): SharedArrayBuffer;
  masuTake(cls: number, now: number): boolean;
  masuDrain(ms: number, now: number): void;
  masuBudgetReclaim(b: SharedArrayBuffer, dead: number, now: number): boolean;
  MASU_BUDGET_BYTES: number;
}
const bridge = (): Bridge =>
  // oxlint-disable-next-line typescript/no-implied-eval -- (the worker's script, as the worker runs it)
  new Function(
    `${source}; return { MASU_CLASS, MASU_RATE, MASU_BURST, MASU_RESERVE, MASU_BUDGET_BYTES, masuBudgetInit, masuTake, masuDrain, masuBudgetReclaim };`,
  )() as Bridge;

test("the page's budget is the network worker's: same constants", () => {
  const b = bridge();
  expect({ ...b.MASU_CLASS }).toEqual(CLASS);
  expect([b.MASU_RATE, b.MASU_BURST, [...b.MASU_RESERVE], b.MASU_BUDGET_BYTES]).toEqual([
    RATE,
    BURST,
    RESERVE,
    BYTES,
  ]);
});

test("the page and the worker taking from one buffer agree with one taker (differential)", () => {
  let x = 99;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 100; run++) {
    const t0 = 1_000_000;
    // (split with other tabs or not)
    const share = rnd(3) ? 1 : 1 / (2 + rnd(3));
    const mk = () => {
      const buf = new SharedArrayBuffer(BYTES);
      new Float64Array(buf, 8, 4).set([BURST, t0, 0, share]);
      return buf;
    };
    // one buffer shared by both takers; one more taken only by the page's copy
    const both = mk();
    const alone = mk();
    const w = bridge();
    w.masuBudgetInit(both);
    let now = t0;
    for (let i = 0; i < 300; i++) {
      now += rnd(150);
      // (now and then a 429, from either side)
      if (rnd(40) === 0) {
        const ms = rnd(6000);
        if (rnd(2)) w.masuDrain(ms, now);
        else drain(ms, now, both);
        drain(ms, now, alone);
        continue;
      }
      const cls = rnd(5);
      const a = rnd(2) ? w.masuTake(cls, now) : take(cls, now, both);
      const b = take(cls, now, alone);
      expect(a, `run ${run} step ${i}`).toBe(b);
    }
  }
});

test("a class leaves its reserve to those above it", () => {
  const buf = new SharedArrayBuffer(BYTES);
  const now = 5_000_000;
  const set = (n: number) => new Float64Array(buf, 8, 4).set([n, now, 0, 1]);
  set(RESERVE[CLASS.save]! + 0.5);
  expect(take(CLASS.save, now, buf)).toBe(false);
  expect(take(CLASS.hint, now, buf)).toBe(true);
  set(0.5);
  expect(take(CLASS.ahead, now, buf)).toBe(false);
  expect(take(CLASS.game, now, buf)).toBe(true);
});

test("a 429 stops every class, the game's too, for its whole wait (112 ms after it the game went before)", () => {
  const buf = new SharedArrayBuffer(BYTES);
  const now = 9_000_000;
  new Float64Array(buf, 8, 4).set([BURST, now, 0, 1]);
  drain(5000, now, buf);
  for (const dt of [0, 112, 4999])
    for (const cls of Object.values(CLASS)) expect(take(cls, now + dt, buf)).toBe(false);
  expect(take(CLASS.game, now + 5000, buf)).toBe(true);
  expect(take(CLASS.save, now + 5000, buf)).toBe(false);
});

// (the review of 2026-11-14: a taker that ran out of tries used to write the budget anyway and
// free the lock its holder still had; a holder that is only not scheduled looks the same as a dead one)
test("a lock held elsewhere is never taken over: the budget stays as it was and the lock its holder's", () => {
  const b = bridge();
  for (const holder of [2, 7]) {
    const buf = new SharedArrayBuffer(BYTES);
    const v = new Float64Array(buf, 8, 4);
    v.set([BURST, 0, 0, 1]);
    const lock = new Int32Array(buf, 0, 1);
    lock[0] = holder;
    const before = [...v];
    expect(take(CLASS.game, 1000, buf)).toBe(false);
    expect(waitFor(CLASS.game, 1000, buf)).toBeGreaterThan(0);
    // (the worker's copy, as the page's own tag: the same)
    b.masuBudgetInit(buf, PAGE_TAG);
    expect(b.masuTake(CLASS.game, 1000)).toBe(false);
    expect([...v]).toEqual(before);
    expect(lock[0]).toBe(holder);
    // (freed by its holder: the next take goes)
    lock[0] = 0;
    expect(take(CLASS.game, 1000, buf)).toBe(true);
    expect(lock[0]).toBe(0);
  }
});

test("a 429 told while the lock is held elsewhere lands once it is free (page and worker)", async () => {
  const b = bridge();
  const buf = new SharedArrayBuffer(BYTES);
  const v = new Float64Array(buf, 8, 4);
  v.set([BURST, 0, 0, 1]);
  const lock = new Int32Array(buf, 0, 1);
  lock[0] = 3;
  drain(5000, 100, buf);
  b.masuBudgetInit(buf, 2);
  b.masuDrain(7000, 100);
  await new Promise((r) => setTimeout(r, 30));
  expect(v[2]).toBe(0);
  lock[0] = 0;
  await new Promise((r) => setTimeout(r, 30));
  expect([v[0], v[2]]).toEqual([0, 7100]);
  expect(take(CLASS.game, 7099, buf)).toBe(false);
});

test("reclaim frees only the terminated worker's lock, and the budget starts from none", () => {
  const b = bridge();
  for (const [name, rec] of [
    ["page", (buf: SharedArrayBuffer, dead: number, now: number) => reclaim(dead, now, buf)],
    ["worker", (buf: SharedArrayBuffer, dead: number, now: number) => b.masuBudgetReclaim(buf, dead, now)],
  ] as const) {
    const buf = new SharedArrayBuffer(BYTES);
    const v = new Float64Array(buf, 8, 4);
    // (a half-written budget: the dead one stopped inside)
    v.set([Number.NaN, 0, 9000, 0.5]);
    const lock = new Int32Array(buf, 0, 1);
    lock[0] = 2;
    // (another's or a free lock: left alone)
    expect(rec(buf, 3, 5000), name).toBe(false);
    expect(rec(buf, 0, 5000), name).toBe(false);
    expect(lock[0], name).toBe(2);
    expect(rec(buf, 2, 5000), name).toBe(true);
    expect(lock[0], name).toBe(0);
    expect([...v], name).toEqual([0, 9000, 9000, 0.5]);
    lock[0] = 0;
    expect(rec(buf, 2, 5000), name).toBe(false);
    // (the 429's wait still holds; after it, the budget refills at the tab's share)
    expect(take(CLASS.game, 8999, buf), name).toBe(false);
    expect(take(CLASS.game, 9001, buf), name).toBe(true);
  }
});

/** Real threads (node workers running the network worker's copy, the page's copy on this
 * thread), one buffer: the lock keeps the budget whole. Over a run of T ms no more requests go
 * than the budget holds (burst + rate * T, the game's debt) at its share, and none are lost:
 * the tokens left are what the takes leave. */
test("threads taking at once never take more than the budget (real threads, its share)", async () => {
  // (node's threads: no @types/node here)
  const threads = "node:worker_threads";
  const { Worker } = (await import(/* @vite-ignore */ threads)) as {
    Worker: new (
      code: string,
      o: { eval: boolean; workerData: unknown },
    ) => { once(ev: string, f: (x: never) => void): void };
  };
  for (const share of [1, 0.5]) {
    const buf = new SharedArrayBuffer(BYTES);
    const t0 = Date.now();
    new Float64Array(buf, 8, 4).set([BURST * share, t0, 0, share]);
    const N = 4;
    const MS = 300;
    const code = `
const { parentPort, workerData } = require('node:worker_threads');
${source}
masuBudgetInit(workerData.buf, workerData.tag);
const end = Date.now() + workerData.ms;
let n = 0, tries = 0;
while (Date.now() < end) { tries++; if (masuTake(workerData.tag % 2 ? 0 : 1)) n++; }
parentPort.postMessage({ n, tries });`;
    const runs = Array.from(
      { length: N },
      (_, i) =>
        new Promise<{ n: number; tries: number }>((res, rej) => {
          const w = new Worker(code, { eval: true, workerData: { buf, tag: i + 2, ms: MS } });
          w.once("message", res);
          w.once("error", rej);
        }),
    );
    let mine = 0;
    const end = Date.now() + MS;
    while (Date.now() < end) if (take(CLASS.game, Date.now(), buf)) mine++;
    const got = await Promise.all(runs);
    const elapsed = (Date.now() - t0) / 1000;
    const total = mine + got.reduce((a, r) => a + r.n, 0);
    expect(new Int32Array(buf, 0, 1)[0]).toBe(0);
    expect(total, `share ${share}`).toBeLessThanOrEqual(
      Math.ceil((BURST + RATE * elapsed - RESERVE[0]!) * share) + 1,
    );
    // (and the tokens left are what the refill gave less what was taken: nothing lost to a race)
    const v = new Float64Array(buf, 8, 4);
    const refill = (v[1]! - t0) / 1000;
    expect(v[0]!).toBeCloseTo(Math.min(BURST * share, BURST * share + refill * RATE * share) - total, -1);
    expect(got.every((r) => r.tries > 0)).toBe(true);
  }
});

/** tabShare and shareAcrossTabs over a channel in memory and a clock of its own: k tabs settle
 * on 1/k each, a tab that leaves (or goes quiet) gives its share back, and a 429 in one tab
 * stops the others too (property over random runs, seeded). */
test("tabs split the budget and pass 429s on (property, seeded)", () => {
  vi.useFakeTimers();
  try {
    let x = 21;
    const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
    for (let run = 0; run < 30; run++) {
      const t0 = 50_000_000;
      vi.setSystemTime(t0);
      const subs = new Set<{ onmessage: ((e: MessageEvent) => void) | null; closed: boolean }>();
      let last: { closed: boolean } | undefined;
      const channel = () => {
        const me = { onmessage: null as ((e: MessageEvent) => void) | null, closed: false };
        subs.add(me);
        last = me;
        return {
          set onmessage(f: (e: MessageEvent) => void) {
            me.onmessage = f;
          },
          postMessage(data: unknown) {
            // (as BroadcastChannel: to every other, a task later)
            if (me.closed) return;
            for (const s of subs)
              if (s !== me && !s.closed) setTimeout(() => s.onmessage?.({ data } as MessageEvent), 1);
          },
          close() {
            me.closed = true;
            subs.delete(me);
          },
        } as unknown as BroadcastChannel;
      };
      const k = 2 + rnd(4);
      const tabs = Array.from({ length: k }, (_, i) => {
        const buf = new SharedArrayBuffer(BYTES);
        new Float64Array(buf, 8, 4).set([BURST, t0, 0, 1]);
        const stop = shareAcrossTabs(buf, { channel, id: `t${i}` });
        const me = last!;
        return { buf, stop, mute: () => void (me.closed = true) };
      });
      const share = (t: (typeof tabs)[number]) => new Float64Array(t.buf, 8, 4)[3]!;
      vi.advanceTimersByTime(TAB_MS * 2 + 10);
      for (const t of tabs) expect(share(t), `run ${run}`).toBeCloseTo(1 / k);
      // (the shares never add up past the whole)
      expect(tabs.reduce((a, t) => a + share(t), 0)).toBeLessThanOrEqual(1 + 1e-9);
      // a 429 in one tab: the others stop too
      const at = Date.now();
      drain(4000, at, tabs[0]!.buf);
      vi.advanceTimersByTime(TAB_MS + 10);
      for (const t of tabs.slice(1)) {
        expect(take(CLASS.game, at + 3000, t.buf), `run ${run}`).toBe(false);
        expect(new Float64Array(t.buf, 8, 4)[2], `run ${run}`).toBe(at + 4000);
      }
      // one leaves (or goes quiet: killed, its words no longer arrive): the rest split it
      const gone = tabs.pop()!;
      const quiet = rnd(2) === 0;
      if (quiet) gone.mute();
      else gone.stop();
      vi.advanceTimersByTime(TAB_GONE_MS + TAB_MS * 2);
      for (const t of tabs) expect(share(t), `run ${run} quiet ${quiet}`).toBeCloseTo(1 / (k - 1));
      for (const t of tabs) t.stop();
      gone.stop();
    }
  } finally {
    vi.useRealTimers();
  }
  const peers = new Map([
    ["a", 0],
    ["b", 1000],
  ]);
  expect(tabShare(peers, 1000 + TAB_GONE_MS - 1)).toBeCloseTo(1 / 2);
  expect([...peers.keys()]).toEqual(["b"]);
});

/** waitFor against take over random budgets: when it says 0 a take goes now; when it says t, no
 * take goes before t and one goes at t (property, seeded). */
test("waitFor says when take will go (property, seeded)", () => {
  let x = 8;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 2000; run++) {
    const buf = new SharedArrayBuffer(BYTES);
    const now = 1_000_000;
    new Float64Array(buf, 8, 4).set([
      rnd(BURST * 2) - BURST,
      now - 3000 + rnd(6000),
      rnd(3) ? 0 : now + rnd(5000),
      rnd(2) ? 1 : 1 / (1 + rnd(4)),
    ]);
    const cls = rnd(5);
    const w = waitFor(cls, now, buf);
    const copy = () => {
      const b = new SharedArrayBuffer(BYTES);
      new Uint8Array(b).set(new Uint8Array(buf));
      return b;
    };
    if (w === 0) expect(take(cls, now, copy()), `run ${run}`).toBe(true);
    else {
      expect(take(cls, now + w - 2, copy()), `run ${run}: not before ${w}`).toBe(false);
      expect(take(cls, now + w + 1, copy()), `run ${run}: at ${w}`).toBe(true);
    }
  }
});
