// 自動送り: the page itself pressing on through a game's text, as a player would by holding
// Ctrl or clicking again and again (for games with no skip of their own, or one that stops at
// unread text). Turned on from the どこでもセーブ dialog (ui/States.tsx); the input goes the way
// the player's own does (play.ts attachInput: vnhost_render_input, through memgate.ts).
//
//   click  a left click at the pointer's last place on the screen, `interval` ms apart
//   enter  Enter pressed and let go, `interval` ms apart
//   ctrl   Ctrl held down, repeating as a held key does (WM_KEYDOWN with the repeat bit, and
//          GetKeyState/GetAsyncKeyState down all along), let go when stopped
//
// A click or a press is its down and its up half an interval apart (a game that looks at the
// button's state between its frames sees it down). What is down is always let go: on stop, on a
// change of mode, and before the page is hidden. While a save state is taken (the gate shut) no
// new press starts; a let-go still goes (memgate.ts runs it once the gate opens).

export type AutoMode = "click" | "enter" | "ctrl";

export const AUTO_MODES: AutoMode[] = ["click", "enter", "ctrl"];
/** ms from one click / press to the next */
export const AUTO_INTERVALS = [50, 100, 200, 500];
/** a held key's repeat (Windows' fastest keyboard repeat rate: about 30 a second) */
export const CTRL_REPEAT_MS = 33;

// input record kinds (crates/kura/src/display.rs), as play.ts
export const MOUSE_MOVE = 1,
  MOUSE_DOWN = 2,
  MOUSE_UP = 3,
  KEY_DOWN = 5,
  KEY_UP = 6;
/** vk | scan << 16, as play.ts sends a real key (ControlLeft: VK_LCONTROL, scan 29) */
export const ENTER_KEY = 0x0d | (28 << 16);
export const CTRL_KEY = 0xa2 | (29 << 16);

export interface AutoInput {
  /** one input record (play.ts: through the gate) */
  push(kind: number, x: number, y: number, data: number): void;
  /** a save state is being taken */
  closed(): boolean;
  /** where a click goes (canvas pixels, as the player's) */
  at(): [number, number];
}

export interface Timers {
  every(f: () => void, ms: number): unknown;
  cancel(t: unknown): void;
}

const realTimers: Timers = {
  every: (f, ms) => setInterval(f, ms),
  cancel: (t) => clearInterval(t as ReturnType<typeof setInterval>),
};

export interface AutoSettings {
  mode: AutoMode;
  interval: number;
}

export const DEFAULT_SETTINGS: AutoSettings = { mode: "click", interval: 100 };

/** Settings read back (from the page's storage): anything else is the default's. */
export function parseSettings(s: unknown): AutoSettings {
  const o = (typeof s === "object" && s) || {};
  const mode = (o as { mode?: unknown }).mode;
  const interval = (o as { interval?: unknown }).interval;
  return {
    mode: AUTO_MODES.includes(mode as AutoMode) ? (mode as AutoMode) : DEFAULT_SETTINGS.mode,
    interval: AUTO_INTERVALS.includes(interval as number) ? (interval as number) : DEFAULT_SETTINGS.interval,
  };
}

export class AutoAdvance {
  private timer: unknown = null;
  private mode: AutoMode = DEFAULT_SETTINGS.mode;
  /** what is down now: a button / key the next tick (or stop) lets go */
  private down: { kind: "click" | "enter" } | null = null;
  private ctrlDown = false;
  private at: [number, number] = [0, 0];

  constructor(
    private input: AutoInput,
    private timers: Timers = realTimers,
  ) {}

  get running(): boolean {
    return this.timer !== null;
  }

  /** Starts (or goes on in) `mode`, a click / press every `interval` ms. */
  start(mode: AutoMode, interval: number): void {
    this.stop();
    this.mode = mode;
    if (mode === "ctrl") {
      this.ctrlDown = true;
      this.press(KEY_DOWN, CTRL_KEY);
      this.timer = this.timers.every(() => this.tick(), CTRL_REPEAT_MS);
    } else {
      this.timer = this.timers.every(() => this.tick(), Math.max(1, Math.round(interval / 2)));
      this.tick();
    }
  }

  /** Stops, letting go of what is down. */
  stop(): void {
    if (this.timer !== null) this.timers.cancel(this.timer);
    this.timer = null;
    this.release();
  }

  private release(): void {
    if (this.down?.kind === "click") this.input.push(MOUSE_UP, ...this.at, 0);
    if (this.down?.kind === "enter") this.input.push(KEY_UP, 0, 0, ENTER_KEY);
    this.down = null;
    if (this.ctrlDown) this.input.push(KEY_UP, 0, 0, CTRL_KEY);
    this.ctrlDown = false;
  }

  private press(kind: number, data: number): void {
    this.input.push(kind, 0, 0, data);
  }

  private tick(): void {
    if (this.mode === "ctrl") {
      // (the key stays down across a state: only its repeats wait)
      if (!this.input.closed()) this.press(KEY_DOWN, CTRL_KEY);
      return;
    }
    if (this.down) return this.release();
    if (this.input.closed()) return;
    if (this.mode === "click") {
      this.at = this.input.at();
      this.input.push(MOUSE_MOVE, ...this.at, 0);
      this.input.push(MOUSE_DOWN, ...this.at, 0);
      this.down = { kind: "click" };
    } else {
      this.press(KEY_DOWN, ENTER_KEY);
      this.down = { kind: "enter" };
    }
  }
}
