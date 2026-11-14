// Plays what kura's sound card mixed (crates/kura/src/audio.rs: 44.1 kHz stereo f32).
// The AudioWorklet reads the card's ring straight from the shared wasm memory (no copies, and
// nothing on the page's own thread, where a long frame would make the sound stutter). It keeps
// its lag short: kura's wall clock and the audio clock drift a little apart.

const RATE = 44100;
/** frames the worklet may lag behind before it skips ahead */
const MAX_QUEUE = RATE / 5;
/** how far behind it starts, and lands when it skips */
const TARGET = RATE / 20;

/** Frames of a fade in or out where the sound stops or jumps (an underrun, a skip ahead): a
 * cut there was a click, made periodic by the clocks' drift. */
const FADE = 32;

/** The worklet's state across blocks. */
export interface SinkState {
  /** the ring's frame read next (-1: not started) */
  read: number;
  /** the next block starts with a fade in (after silence or a jump) */
  fadeIn: boolean;
}

/**
 * One block of the sink: reads what the ring has from `read` into `l` and `r`, skipping ahead
 * when it lags more than `max` frames (to `target` behind), a fade out before an underrun's
 * silence and a fade in after it or a skip. Plain JS on purpose: the worklet's own code is this
 * function's text (sinkStep.toString()), so the tests run what plays.
 */
export function sinkStep(
  st: SinkState,
  head: number,
  d: Float32Array,
  mask: number,
  l: Float32Array,
  r: Float32Array,
  max: number,
  target: number,
  fade: number,
): void {
  if (st.read < 0) st.read = (head - target) >>> 0;
  let avail = (head - st.read) >>> 0;
  if (avail > max) {
    st.read = (head - target) >>> 0;
    avail = target;
    st.fadeIn = true;
  }
  const n = Math.min(avail, l.length);
  for (let i = 0; i < n; i++) {
    const at = ((st.read + i) & mask) * 2;
    l[i] = d[at];
    r[i] = d[at + 1];
  }
  if (st.fadeIn && n) {
    const k = Math.min(fade, n);
    for (let i = 0; i < k; i++) {
      const g = (i + 1) / (k + 1);
      l[i] *= g;
      r[i] *= g;
    }
    st.fadeIn = false;
  }
  if (n < l.length) {
    // (an underrun: what there is fades out, the rest is silence, the next sound fades in)
    const k = Math.min(fade, n);
    for (let i = 0; i < k; i++) {
      const g = (k - i) / (k + 1);
      l[n - k + i] *= g;
      r[n - k + i] *= g;
    }
    for (let i = n; i < l.length; i++) {
      l[i] = 0;
      r[i] = 0;
    }
    st.fadeIn = true;
  }
  st.read = (st.read + n) >>> 0;
}

/** The worklet module (exported for tests/audio-sink-browser.mjs). */
export const worklet = `
const sinkStep = ${sinkStep.toString()};
class Sink extends AudioWorkletProcessor {
  constructor() {
    super();
    this.st = { read: -1, fadeIn: true };
    this.port.onmessage = (e) => {
      const { mem, ptr, frames } = e.data;
      this.head = new Uint32Array(mem, ptr, 1);
      this.data = new Float32Array(mem, ptr + 8, frames * 2);
      this.mask = frames - 1;
    };
  }
  process(_inputs, outputs) {
    const [l, r] = outputs[0];
    if (!this.head) return true;
    sinkStep(this.st, Atomics.load(this.head, 0), this.data, this.mask, l, r, ${MAX_QUEUE}, ${TARGET}, ${FADE});
    return true;
  }
}
registerProcessor("vnvm-sink", Sink);
`;

export interface AudioSource {
  HEAPU32: Uint32Array;
  wasmMemory: WebAssembly.Memory;
  _malloc(size: number): number;
  _vnhost_audio_ring(frames: number): number;
}

/** Created from a user gesture (autoplay rules); `attach` starts playing a running game. */
export class AudioOut {
  private constructor(
    private ctx: AudioContext,
    private node: AudioWorkletNode,
  ) {}

  static async create(): Promise<AudioOut> {
    const ctx = new AudioContext({ sampleRate: RATE, latencyHint: "interactive" });
    const url = URL.createObjectURL(new Blob([worklet], { type: "text/javascript" }));
    await ctx.audioWorklet.addModule(url);
    URL.revokeObjectURL(url);
    const node = new AudioWorkletNode(ctx, "vnvm-sink", { numberOfInputs: 0, outputChannelCount: [2] });
    node.connect(ctx.destination);
    return new AudioOut(ctx, node);
  }

  /** For the run's log: the context's state and rate, and how many frames the game's sound
   * card mixed into the ring since the last call (0 while the game plays nothing). */
  describe(): string {
    const head = this.head ? Atomics.load(this.head, 0) : 0;
    const mixed = this.head ? (head - this.lastHead) >>> 0 : 0;
    this.lastHead = head;
    return `audio: ${this.ctx.state} at ${this.ctx.sampleRate} Hz, ${this.head ? `${mixed} frames mixed since` : "no game attached"}`;
  }

  private head: Uint32Array | null = null;
  private lastHead = 0;

  private listening = false;

  attach(sake: AudioSource): void {
    // autoplay: the context can only start on a user gesture; on a phone it is suspended (or
    // "interrupted") again by a call, a route change or the background, and stayed silent
    // until the next tap: resumed when the page is shown again and on any touch too
    if (!this.listening) {
      this.listening = true;
      const resume = () =>
        void (
          this.ctx.state !== "running" &&
          this.ctx.state !== "closed" &&
          this.ctx.resume().catch(() => {})
        );
      for (const ev of ["pointerdown", "keydown", "touchend"]) addEventListener(ev, resume, true);
      addEventListener("visibilitychange", () => document.visibilityState === "visible" && resume());
      this.ctx.addEventListener("statechange", () => document.visibilityState === "visible" && resume());
    }
    const p = sake._malloc(4) >>> 0;
    if (!p) return;
    const ptr = sake._vnhost_audio_ring(p) >>> 0;
    const frames = new Uint32Array(sake.wasmMemory.buffer, p, 1)[0]!;
    (sake as AudioSource & { _free?(p: number): void })._free?.(p);
    this.head = new Uint32Array(sake.wasmMemory.buffer, ptr, 1);
    this.lastHead = Atomics.load(this.head, 0);
    this.node.port.postMessage({ mem: sake.wasmMemory.buffer, ptr, frames });
  }
}
