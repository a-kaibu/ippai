import { expect, test } from "vite-plus/test";
import {
  AUTO_INTERVALS,
  AUTO_MODES,
  AutoAdvance,
  CTRL_KEY,
  CTRL_REPEAT_MS,
  DEFAULT_SETTINGS,
  ENTER_KEY,
  KEY_DOWN,
  KEY_UP,
  MOUSE_DOWN,
  MOUSE_MOVE,
  MOUSE_UP,
  parseSettings,
  type AutoMode,
  type Timers,
} from "./autoadvance";

/** A clock the test moves on by hand. */
class Clock implements Timers {
  now = 0;
  private next = 1;
  private live = new Map<number, { f: () => void; ms: number; at: number }>();
  every(f: () => void, ms: number) {
    const id = this.next++;
    this.live.set(id, { f, ms, at: this.now + ms });
    return id;
  }
  cancel(t: unknown) {
    this.live.delete(t as number);
  }
  get count() {
    return this.live.size;
  }
  advance(ms: number) {
    const end = this.now + ms;
    for (;;) {
      let first: [number, { f: () => void; ms: number; at: number }] | undefined;
      for (const e of this.live) if (!first || e[1].at < first[1].at) first = e;
      if (!first || first[1].at > end) break;
      this.now = first[1].at;
      first[1].at += first[1].ms;
      first[1].f();
    }
    this.now = end;
  }
}

type Rec = [number, number, number, number, number]; // time, kind, x, y, data
function rig() {
  const clock = new Clock();
  const recs: Rec[] = [];
  const st = { shut: false, at: [640, 600] as [number, number] };
  const auto = new AutoAdvance(
    {
      push: (k, x, y, d) => recs.push([clock.now, k, x, y, d]),
      closed: () => st.shut,
      at: () => st.at,
    },
    clock,
  );
  return { clock, recs, st, auto };
}
const kinds = (r: Rec[]) => r.map((x) => x[1]);

test("click: move, down, up half an interval later, at the pointer's place, an interval apart", () => {
  const { clock, recs, st, auto } = rig();
  auto.start("click", 100);
  expect(recs).toEqual([
    [0, MOUSE_MOVE, 640, 600, 0],
    [0, MOUSE_DOWN, 640, 600, 0],
  ]);
  clock.advance(50);
  expect(recs.at(-1)).toEqual([50, MOUSE_UP, 640, 600, 0]);
  st.at = [10, 20];
  clock.advance(50);
  expect(recs.slice(-2)).toEqual([
    [100, MOUSE_MOVE, 10, 20, 0],
    [100, MOUSE_DOWN, 10, 20, 0],
  ]);
  // (the up goes where its down went, though the pointer moved)
  st.at = [99, 99];
  clock.advance(50);
  expect(recs.at(-1)).toEqual([150, MOUSE_UP, 10, 20, 0]);
  clock.advance(1000);
  const downs = recs.filter((r) => r[1] === MOUSE_DOWN).map((r) => r[0]);
  expect(downs).toEqual([0, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 1100]);
  auto.stop();
  expect(auto.running).toBe(false);
  expect(clock.count).toBe(0);
});

test("click: stopped while down lets go at once; nothing after", () => {
  const { clock, recs, auto } = rig();
  auto.start("click", 200);
  clock.advance(10);
  auto.stop();
  expect(kinds(recs)).toEqual([MOUSE_MOVE, MOUSE_DOWN, MOUSE_UP]);
  clock.advance(5000);
  expect(recs).toHaveLength(3);
  auto.stop();
  expect(recs).toHaveLength(3);
});

test("enter: down and up half an interval apart, Enter's vk and scan code", () => {
  const { clock, recs, auto } = rig();
  auto.start("enter", 500);
  clock.advance(1000);
  expect(recs).toEqual([
    [0, KEY_DOWN, 0, 0, ENTER_KEY],
    [250, KEY_UP, 0, 0, ENTER_KEY],
    [500, KEY_DOWN, 0, 0, ENTER_KEY],
    [750, KEY_UP, 0, 0, ENTER_KEY],
    [1000, KEY_DOWN, 0, 0, ENTER_KEY],
  ]);
  expect(ENTER_KEY & 0xff).toBe(0x0d);
  expect(ENTER_KEY >>> 16).toBe(28);
});

test("ctrl: held, repeating about 30 a second, let go on stop", () => {
  const { clock, recs, auto } = rig();
  auto.start("ctrl", 500);
  expect(recs).toEqual([[0, KEY_DOWN, 0, 0, CTRL_KEY]]);
  clock.advance(1000);
  expect(recs.every((r) => r[1] === KEY_DOWN && r[4] === CTRL_KEY)).toBe(true);
  expect(recs.length).toBe(1 + Math.floor(1000 / CTRL_REPEAT_MS));
  auto.stop();
  expect(recs.at(-1)).toEqual([1000, KEY_UP, 0, 0, CTRL_KEY]);
  expect(CTRL_KEY & 0xff).toBe(0xa2);
  expect(CTRL_KEY >>> 16).toBe(29);
});

test("a change of mode lets go of what the last one held", () => {
  const { clock, recs, auto } = rig();
  auto.start("ctrl", 100);
  clock.advance(40);
  auto.start("click", 100);
  const up = recs.findIndex((r) => r[1] === KEY_UP);
  const down = recs.findIndex((r) => r[1] === MOUSE_DOWN);
  expect(up).toBeGreaterThan(0);
  expect(down).toBeGreaterThan(up);
  clock.advance(20);
  auto.start("enter", 100);
  expect(recs.slice(-2).map((r) => r[1])).toEqual([MOUSE_UP, KEY_DOWN]);
});

test("while a state is taken: no new press, a let-go still goes, ctrl stays down", () => {
  const { clock, recs, st, auto } = rig();
  auto.start("click", 100);
  st.shut = true;
  clock.advance(50);
  expect(recs.at(-1)![1]).toBe(MOUSE_UP);
  const n = recs.length;
  clock.advance(500);
  expect(recs).toHaveLength(n);
  st.shut = false;
  clock.advance(50);
  expect(recs.at(-1)![1]).toBe(MOUSE_DOWN);
  auto.stop();

  recs.length = 0;
  auto.start("ctrl", 100);
  st.shut = true;
  clock.advance(500);
  expect(recs).toEqual([[clock.now - 500, KEY_DOWN, 0, 0, CTRL_KEY]]);
  auto.stop();
  expect(recs.at(-1)![1]).toBe(KEY_UP);
});

test("settings read back: only known modes and intervals", () => {
  expect(parseSettings({ mode: "ctrl", interval: 50 })).toEqual({ mode: "ctrl", interval: 50 });
  expect(parseSettings({ mode: "skip", interval: 50 })).toEqual({
    mode: DEFAULT_SETTINGS.mode,
    interval: 50,
  });
  expect(parseSettings({ mode: "enter", interval: 7 })).toEqual({
    mode: "enter",
    interval: DEFAULT_SETTINGS.interval,
  });
  for (const v of [null, undefined, 3, "x", [], { interval: "100" }])
    expect(parseSettings(v)).toEqual(DEFAULT_SETTINGS);
});

/** xorshift32: the same run from the same seed */
function rng(seed: number) {
  let s = seed >>> 0 || 1;
  return () => {
    s ^= s << 13;
    s >>>= 0;
    s ^= s >>> 17;
    s ^= s << 5;
    s >>>= 0;
    return s / 2 ** 32;
  };
}

test("property: any run of starts, stops, mode changes, states and time keeps the input balanced", () => {
  for (let seed = 1; seed <= 400; seed++) {
    const r = rng(seed);
    const { clock, recs, st, auto } = rig();
    const pick = <T>(a: T[]) => a[Math.floor(r() * a.length)]!;
    let active = false;
    let mode: AutoMode = "click";
    let interval = 100;
    for (let step = 0; step < 60; step++) {
      const op = r();
      const before = recs.length;
      if (op < 0.25) {
        mode = pick(AUTO_MODES);
        interval = pick(AUTO_INTERVALS);
        auto.start(mode, interval);
        active = true;
      } else if (op < 0.35) {
        auto.stop();
        active = false;
      } else if (op < 0.5) {
        st.shut = !st.shut;
      } else if (op < 0.6) {
        st.at = [Math.floor(r() * 1920), Math.floor(r() * 1080)];
      } else {
        const ms = Math.floor(r() * 400);
        clock.advance(ms);
        // (stopped: nothing at all)
        if (!active) expect(recs.length, `seed ${seed}`).toBe(before);
      }
      expect(auto.running).toBe(active);
      expect(clock.count, `seed ${seed}: one timer at most`).toBe(active ? 1 : 0);
    }
    auto.stop();
    // every down has its up, in order, the up at the down's place; no up without a down
    let mouse: Rec | null = null;
    let enter = false;
    let ctrl = false;
    for (const x of recs) {
      const [, k, , , d] = x;
      if (k === MOUSE_DOWN) {
        expect(mouse, `seed ${seed}: a second down`).toBe(null);
        mouse = x;
      } else if (k === MOUSE_UP) {
        expect(mouse, `seed ${seed}: an up with no down`).not.toBe(null);
        expect([x[2], x[3]]).toEqual([mouse![2], mouse![3]]);
        mouse = null;
      } else if (k === KEY_DOWN && d === ENTER_KEY) {
        expect(enter).toBe(false);
        enter = true;
      } else if (k === KEY_UP && d === ENTER_KEY) {
        expect(enter).toBe(true);
        enter = false;
      } else if (k === KEY_DOWN && d === CTRL_KEY) {
        ctrl = true;
      } else if (k === KEY_UP && d === CTRL_KEY) {
        expect(ctrl, `seed ${seed}: ctrl let go twice`).toBe(true);
        ctrl = false;
      }
      // one thing held at a time (a mode at a time)
      expect([mouse !== null, enter, ctrl].filter(Boolean).length).toBeLessThanOrEqual(1);
    }
    expect([mouse, enter, ctrl], `seed ${seed}: something left down`).toEqual([null, false, false]);
  }
});

test("property: presses start only while the gate is open", () => {
  for (let seed = 1; seed <= 200; seed++) {
    const r = rng(seed * 7919);
    const clock = new Clock();
    const st = { shut: false };
    const bad: number[] = [];
    const auto = new AutoAdvance(
      {
        push: (k, _x, _y, d) => {
          const press = k === MOUSE_DOWN || (k === KEY_DOWN && d === ENTER_KEY);
          if (press && st.shut) bad.push(k);
        },
        closed: () => st.shut,
        at: () => [1, 1],
      },
      clock,
    );
    auto.start(r() < 0.5 ? "click" : "enter", AUTO_INTERVALS[Math.floor(r() * 4)]!);
    for (let i = 0; i < 80; i++) {
      if (r() < 0.3) st.shut = !st.shut;
      clock.advance(Math.floor(r() * 300));
    }
    auto.stop();
    expect(bad, `seed ${seed}`).toEqual([]);
  }
});
