// Sound decoded with WebCodecs' AudioDecoder for the guest (crates/kura/src/adec.rs): codecs sake
// has no decoder of its own for (MP3 through msacm32). The guest pushes chunks and waits until
// every chunk it pushed is decoded; this feeds them to an AudioDecoder per stream and hands back
// each output's samples (interleaved f32).
//
// When is a chunk done? AudioDecoder.flush() ends the stream (the next chunk decodes without
// the bit reservoir / overlap of the ones before: tests/audiodec), so outputs are counted
// instead: MP3 and Opus give one per chunk, Vorbis none for its first; a chunk that gives none
// (a bad frame) is let go once the decoder has nothing queued and nothing came for QUIET ms.

import { memGate, type Gate } from "./memgate";

export interface AdecWasm {
  wasmMemory: WebAssembly.Memory;
  _malloc(size: number): number;
  _free(ptr: number): void;
  _vnhost_adec_doorbell(): number;
  _vnhost_adec_list(out: number, max: number): number;
  _vnhost_adec_chunk(id: number, gen: number, size: number, seq: number): number;
  _vnhost_adec_pcm(id: number, gen: number, pcm: number, frames: number, channels: number): void;
  _vnhost_adec_done(id: number, gen: number, seq: number): void;
  _vnhost_adec_fail(id: number, gen: number): void;
}

/** The part of AudioDecoder this uses (tests give their own). */
export interface Decoder {
  readonly state: string;
  readonly decodeQueueSize: number;
  configure(c: AudioDecoderConfig): void;
  decode(c: EncodedAudioChunk | { type: "key"; timestamp: number; data: Uint8Array }): void;
  close(): void;
}
export interface DecoderKind {
  new (init: { output: (d: AudioData) => void; error: (e: unknown) => void }): Decoder;
  isConfigSupported(c: AudioDecoderConfig): Promise<{ supported?: boolean }>;
}
type Chunk = (init: {
  type: "key";
  timestamp: number;
  data: Uint8Array;
}) => EncodedAudioChunk | { type: "key"; timestamp: number; data: Uint8Array };

type WaitAsync = (
  a: Int32Array,
  i: number,
  v: number,
  t: number,
) => { async: boolean; value: Promise<string> | string };

const MAX = 32;
const ROW = 10;
/** ms with nothing queued and no output before the chunks fed count as done */
export const QUIET = 20;

interface Stream {
  id: number;
  gen: number;
  dec: Decoder | null;
  /** waiting for isConfigSupported */
  pending: boolean;
  broken: boolean;
  channels: number;
  /** chunks expected to give no output (Vorbis: its first) */
  silent: number;
  /** last chunk fed, outputs had */
  fed: number;
  outs: number;
  said: number;
  quiet: ReturnType<typeof setTimeout> | null;
}

export class AudioDecode {
  private streams = new Map<number, Stream>();
  private io: number;
  private sizes: number;
  private scratch = 0;
  private scratchLen = 0;
  private doorbell: number;
  /** outputs that came while a state was taken (memgate.ts), written after in order */
  private held: [Stream, AudioData][] = [];
  private heldKey = {};

  constructor(
    private wasm: AdecWasm,
    private note: (line: string) => void,
    private kind: DecoderKind = AudioDecoder as unknown as DecoderKind,
    private chunk: Chunk = (i) => new EncodedAudioChunk(i),
    watch = true,
    private gate: Gate = memGate,
  ) {
    this.io = wasm._malloc(4 * ROW * MAX) >>> 0;
    this.sizes = wasm._malloc(8) >>> 0;
    this.doorbell = wasm._vnhost_adec_doorbell() >>> 0;
    if (watch) void this.watch();
  }

  private async watch(): Promise<void> {
    const i32 = () => new Int32Array(this.wasm.wasmMemory.buffer);
    let seen = Atomics.load(i32(), this.doorbell >>> 2);
    for (;;) {
      const w = (Atomics as unknown as { waitAsync: WaitAsync }).waitAsync(
        i32(),
        this.doorbell >>> 2,
        seen,
        1000,
      );
      if (w.async) await w.value;
      seen = Atomics.load(i32(), this.doorbell >>> 2);
      this.tick();
    }
  }

  /** New and closed streams, and chunks to feed (on each doorbell). */
  tick(): void {
    // (a save state is being taken: the memory stays as it is; the next doorbell looks again)
    if (this.gate.closed) return;
    const n = this.wasm._vnhost_adec_list(this.io, MAX);
    const u32 = new Uint32Array(this.wasm.wasmMemory.buffer, this.io, n * ROW);
    const live = new Set<number>();
    for (let k = 0; k < n; k++) {
      const r = u32.subarray(k * ROW, k * ROW + ROW);
      const [id, gen] = [r[0]!, r[1]!];
      live.add(id);
      let s = this.streams.get(id);
      if (s && s.gen !== gen) {
        this.drop(s);
        s = undefined;
      }
      if (!s) {
        s = this.open(id, gen, r);
        this.streams.set(id, s);
      }
      this.feed(s);
    }
    for (const [id, s] of this.streams)
      if (!live.has(id)) {
        this.drop(s);
        this.streams.delete(id);
      }
  }

  private open(id: number, gen: number, r: Uint32Array): Stream {
    const mem = this.wasm.wasmMemory.buffer;
    const codec = new TextDecoder().decode(new Uint8Array(mem, r[2]!, r[3]!).slice());
    const desc = r[7]! ? new Uint8Array(mem, r[6]!, r[7]!).slice() : null;
    const config: AudioDecoderConfig = { codec, sampleRate: r[4]!, numberOfChannels: r[5]! };
    if (desc) config.description = desc;
    const s: Stream = {
      id,
      gen,
      dec: null,
      pending: true,
      broken: false,
      channels: r[5]!,
      silent: codec === "vorbis" ? 1 : 0,
      fed: 0,
      outs: 0,
      said: 0,
      quiet: null,
    };
    void this.configure(s, config);
    return s;
  }

  private async configure(s: Stream, config: AudioDecoderConfig): Promise<void> {
    let ok = false;
    try {
      ok = !!(await this.kind.isConfigSupported(config)).supported;
    } catch {
      ok = false;
    }
    if (this.streams.get(s.id) !== s) return;
    s.pending = false;
    if (!ok) {
      this.note(
        `audio ${s.id}: ${config.codec} ${config.sampleRate} Hz ${config.numberOfChannels} ch not supported`,
      );
      return this.gate.run(() => this.fail(s));
    }
    s.dec = new this.kind({
      output: (d) => this.output(s, d),
      error: (e) => {
        this.note(`audio ${s.id}: ${String(e)}`);
        this.gate.run(() => this.fail(s));
      },
    });
    s.dec.configure(config);
    this.note(`audio ${s.id}: ${config.codec} ${config.sampleRate} Hz ${config.numberOfChannels} ch`);
    this.gate.run(() => this.feed(s));
  }

  private feed(s: Stream): void {
    if (this.gate.closed) return this.gate.run(() => this.feed(s));
    if (s.pending || s.broken || !s.dec || s.dec.state !== "configured") return;
    const u32 = () => new Uint32Array(this.wasm.wasmMemory.buffer, this.sizes, 2);
    for (;;) {
      const p = this.wasm._vnhost_adec_chunk(s.id, s.gen, this.sizes, this.sizes + 4) >>> 0;
      if (!p) break;
      const [size, seq] = [u32()[0]!, u32()[1]!];
      const data = new Uint8Array(this.wasm.wasmMemory.buffer, p, size).slice();
      // (timestamps only order the chunks: sake keeps its own time)
      s.dec.decode(this.chunk({ type: "key", timestamp: seq * 1000, data }));
      s.fed = seq;
    }
    this.check(s);
  }

  private output(s: Stream, d: AudioData): void {
    // (a state is being taken: kept, written once it is done, after any kept before)
    if (this.gate.closed || this.held.length) {
      this.held.push([s, d]);
      this.gate.run(() => {
        for (const [hs, hd] of this.held.splice(0)) this.write(hs, hd);
      }, this.heldKey);
      return;
    }
    this.write(s, d);
  }

  private write(s: Stream, d: AudioData): void {
    try {
      if (this.streams.get(s.id) !== s || s.broken) return;
      const ch = d.numberOfChannels;
      const frames = d.numberOfFrames;
      const bytes = frames * ch * 4;
      if (bytes > this.scratchLen) {
        if (this.scratch) this.wasm._free(this.scratch);
        this.scratchLen = Math.max(bytes, 2 * this.scratchLen, 1 << 16);
        this.scratch = this.wasm._malloc(this.scratchLen) >>> 0;
      }
      const out = new Float32Array(this.wasm.wasmMemory.buffer, this.scratch, frames * ch);
      if (d.format === "f32") d.copyTo(out, { planeIndex: 0, format: "f32" });
      else {
        const plane = new Float32Array(frames);
        for (let c = 0; c < ch; c++) {
          d.copyTo(plane, { planeIndex: c, format: "f32-planar" });
          for (let i = 0; i < frames; i++) out[i * ch + c] = plane[i]!;
        }
      }
      this.wasm._vnhost_adec_pcm(s.id, s.gen, this.scratch, frames, ch);
      s.outs++;
      this.check(s);
    } finally {
      d.close();
    }
  }

  /** Says the chunks fed are done when their outputs are all here, or once nothing more comes. */
  private check(s: Stream): void {
    if (s.said >= s.fed || s.broken) return;
    if (s.outs >= s.fed - s.silent) return this.done(s);
    if (s.quiet) clearTimeout(s.quiet);
    s.quiet = setTimeout(() => {
      s.quiet = null;
      if (this.streams.get(s.id) !== s || s.broken) return;
      this.gate.run(() => {
        if (s.dec && s.dec.decodeQueueSize === 0) this.done(s);
        else this.check(s);
      });
    }, QUIET);
  }

  private done(s: Stream): void {
    if (s.quiet) clearTimeout(s.quiet);
    s.quiet = null;
    s.said = s.fed;
    this.wasm._vnhost_adec_done(s.id, s.gen, s.fed);
  }

  private fail(s: Stream): void {
    s.broken = true;
    this.wasm._vnhost_adec_fail(s.id, s.gen);
  }

  private drop(s: Stream): void {
    if (s.quiet) clearTimeout(s.quiet);
    s.broken = true;
    if (s.dec && s.dec.state !== "closed") s.dec.close();
  }
}
