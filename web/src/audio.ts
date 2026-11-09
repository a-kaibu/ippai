// Plays what kura's sound card mixed (crates/kura/src/audio.rs: 44.1 kHz stereo f32).
// The AudioWorklet reads the card's ring straight from the shared wasm memory (no copies, and
// nothing on the page's own thread, where a long frame would make the sound stutter). It keeps
// its lag short: kura's wall clock and the audio clock drift a little apart.

const RATE = 44100;
/** frames the worklet may lag behind before it skips ahead */
const MAX_QUEUE = RATE / 5;
/** how far behind it starts, and lands when it skips */
const TARGET = RATE / 20;

const worklet = `
class Sink extends AudioWorkletProcessor {
  constructor() {
    super();
    this.read = -1;
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
    const head = Atomics.load(this.head, 0);
    if (this.read < 0) this.read = (head - ${TARGET}) >>> 0;
    let avail = (head - this.read) >>> 0;
    if (avail > ${MAX_QUEUE}) {
      this.read = (head - ${TARGET}) >>> 0;
      avail = ${TARGET};
    }
    const n = Math.min(avail, l.length);
    const d = this.data;
    for (let i = 0; i < n; i++) {
      const at = ((this.read + i) & this.mask) * 2;
      l[i] = d[at];
      r[i] = d[at + 1];
    }
    this.read = (this.read + n) >>> 0;
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

  attach(sake: AudioSource): void {
    // autoplay: the context can only start on a user gesture
    for (const ev of ["pointerdown", "keydown"])
      addEventListener(ev, () => void (this.ctx.state === "suspended" && this.ctx.resume()), true);
    const p = sake._malloc(4) >>> 0;
    const ptr = sake._vnhost_audio_ring(p) >>> 0;
    const frames = new Uint32Array(sake.wasmMemory.buffer, p, 1)[0]!;
    this.head = new Uint32Array(sake.wasmMemory.buffer, ptr, 1);
    this.lastHead = Atomics.load(this.head, 0);
    this.node.port.postMessage({ mem: sake.wasmMemory.buffer, ptr, frames });
  }
}
