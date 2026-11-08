// Movies decoded with WebCodecs for the guest (crates/kura/src/video.rs): each stream is a
// `.vnv` (VP9 tracks: color, and alpha when the movie has one; crates/ippai/src/movie.rs): its
// header and frame tables in wasm memory, each frame's bytes had from the guest when it has read
// them (a movie file is read a few seconds ahead of the frame shown). The guest asks for frame n; this decodes ahead of the newest request (hardware
// first) and copies whole frames into the stream's slots, where the guest takes them.

import { memGate, type Gate } from "./memgate";

export interface VideoWasm {
  wasmMemory: WebAssembly.Memory;
  _malloc(size: number): number;
  _vnhost_video_doorbell(): number;
  _vnhost_video_list(out: number, max: number): number;
  _vnhost_video_has(id: number, n: number): number;
  _vnhost_video_chunk(id: number, t: number, n: number, size: number): number;
  _vnhost_video_begin(id: number, n: number, size: number): number;
  _vnhost_video_ready(id: number, buf: number, layout: number, ok: number): void;
  _vnhost_video_fail(id: number): void;
}

type WaitAsync = (
  a: Int32Array,
  i: number,
  v: number,
  t: number,
) => { async: boolean; value: Promise<string> | string };

/** frames decoded ahead of the newest request */
const AHEAD = 3;
const FORMATS: Record<string, number> = { I420: 0, NV12: 1 };

interface Track {
  codec: string;
  /** per frame: offset, size, key */
  frames: Uint32Array;
  dec: VideoDecoder | null;
  /** the configuration chosen (hardware when the browser has it) */
  config: VideoDecoderConfig | null;
  /** the next frame fed to the decoder, and the first fed since the last reset */
  fed: number;
  from: number;
  /** decoded and not yet copied, by frame */
  out: Map<number, VideoFrame>;
}

interface Stream {
  id: number;
  w: number;
  h: number;
  frames: number;
  tracks: Track[];
  want: number;
  copying: Set<number>;
  broken: boolean;
  /** keys of its work held while a state is taken (memgate.ts run) */
  held: { serve: object; copy: object };
}

export class Video {
  private streams = new Map<number, Stream>();
  private io: number;
  private layoutBuf: number;
  private doorbell: number;

  /** `note`: a line for the run's log (the decoder chosen, errors) */
  constructor(
    private wasm: VideoWasm,
    private note: (line: string) => void,
    private gate: Gate = memGate,
    watch = true,
  ) {
    this.io = wasm._malloc(4 * 6 * 16 + 4) >>> 0;
    this.layoutBuf = wasm._malloc(14 * 4) >>> 0;
    this.doorbell = wasm._vnhost_video_doorbell() >>> 0;
    if (watch) void this.watch();
  }

  /** wakes at once when the guest asks for a frame (Atomics.waitAsync on the doorbell) */
  private async watch(): Promise<void> {
    const i32 = () => new Int32Array(this.wasm.wasmMemory.buffer);
    let seen = Atomics.load(i32(), this.doorbell >>> 2);
    for (;;) {
      // (Atomics.waitAsync: ES2024, in Chrome since 87)
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

  /** Called every frame (and on each request): new and closed streams, decoding ahead. */
  tick(): void {
    // (a save state is being taken: the memory stays as it is; the next tick looks again)
    if (this.gate.closed) return;
    const n = this.wasm._vnhost_video_list(this.io, 16);
    const u32 = new Uint32Array(this.wasm.wasmMemory.buffer, this.io, n * 6);
    const live = new Set<number>();
    for (let k = 0; k < n; k++) {
      const [id, ptr, len, want] = [u32[k * 6]!, u32[k * 6 + 1]!, u32[k * 6 + 2]!, u32[k * 6 + 3]! | 0];
      live.add(id);
      let s = this.streams.get(id);
      if (!s) {
        s = this.open(id, ptr, len);
        this.streams.set(id, s);
      }
      if (!s.broken && want >= 0) this.serve(s, want);
    }
    for (const [id, s] of this.streams) {
      if (!live.has(id)) {
        this.close(s);
        this.streams.delete(id);
      }
    }
  }

  private open(id: number, ptr: number, len: number): Stream {
    const b = new Uint8Array(this.wasm.wasmMemory.buffer, ptr, len);
    const dv = new DataView(b.buffer, ptr, len);
    const u = (o: number) => dv.getUint32(o, true);
    const [w, h, nt] = [u(4), u(8), u(12)];
    let o = 16;
    const tracks: Track[] = [];
    let frames = 0;
    for (let t = 0; t < nt; t++) {
      const cl = u(o);
      const codec = new TextDecoder().decode(b.slice(o + 4, o + 4 + cl));
      o += 4 + cl;
      frames = u(o);
      o += 4;
      const table = new Uint32Array(frames * 3);
      for (let i = 0; i < frames * 3; i++) table[i] = u(o + 4 * i);
      o += 12 * frames;
      tracks.push({
        codec,
        frames: table,
        dec: null,
        config: null,
        fed: 0,
        from: 0,
        out: new Map(),
      });
    }
    const s: Stream = {
      id,
      w,
      h,
      frames,
      tracks,
      want: -1,
      copying: new Set(),
      broken: false,
      held: { serve: {}, copy: {} },
    };
    void this.configure(s, ptr);
    return s;
  }

  private async configure(s: Stream, ptr: number): Promise<void> {
    for (const t of s.tracks) {
      // (no-preference: the hardware decoder when there is one free, else software; Android has
      // few hardware decoders and Siglus plays several movies at once)
      const config: VideoDecoderConfig = {
        codec: t.codec,
        codedWidth: s.w,
        codedHeight: s.h,
        optimizeForLatency: true,
      };
      if (!(await VideoDecoder.isConfigSupported(config)).supported) {
        this.note(`video ${s.id}: ${t.codec} not supported`);
        this.gate.run(() => this.fail(s));
        return;
      }
      t.dec = new VideoDecoder({
        output: (f) => {
          const n = Math.round(f.timestamp / 1000);
          if (n < s.want || t.out.has(n)) f.close();
          else t.out.set(n, f);
          // (held while a state is taken: copied after)
          this.gate.run(() => this.copyReady(s), s.held.copy);
        },
        error: (e) => {
          this.note(`video ${s.id}: ${String(e)}`);
          this.gate.run(() => this.fail(s));
        },
      });
      t.config = config;
      t.dec.configure(config);
      this.note(`video ${s.id}: ${t.codec} ${s.w}x${s.h}`);
    }
    void ptr;
    this.gate.run(() => s.want >= 0 && this.serve(s, s.want), s.held.serve);
  }

  /** Frame n of track t, or null while the guest has not read its bytes yet. */
  private chunk(s: Stream, t: number, n: number): EncodedVideoChunk | null {
    const key = s.tracks[t]!.frames[n * 3 + 2]!;
    const ptr = this.wasm._vnhost_video_chunk(s.id, t, n, this.layoutBuf) >>> 0;
    if (!ptr) return null;
    const size = new Uint32Array(this.wasm.wasmMemory.buffer, this.layoutBuf, 1)[0]!;
    return new EncodedVideoChunk({
      type: key ? "key" : "delta",
      timestamp: n * 1000,
      data: new Uint8Array(this.wasm.wasmMemory.buffer, ptr, size).slice(),
    });
  }

  private serve(s: Stream, want: number): void {
    if (this.gate.closed) return this.gate.run(() => this.serve(s, s.want), s.held.serve);
    want = Math.min(want, s.frames - 1);
    s.want = want;
    if (s.tracks.some((t) => !t.dec || t.dec.state !== "configured")) return;
    const end = Math.min(s.frames, want + AHEAD);
    for (const [ti, t] of s.tracks.entries()) {
      // a jump back, or past a key frame not fed yet: from the track's key frame before it
      // (resetting a hardware decoder costs; the tracks' key frames need not line up)
      let k = want;
      while (k > 0 && !t.frames[k * 3 + 2]) k--;
      if (want < t.from || k > t.fed) {
        t.dec!.reset();
        t.dec!.configure(t.config!);
        for (const f of t.out.values()) f.close();
        t.out.clear();
        t.fed = t.from = k;
      }
      for (const [n, f] of t.out)
        if (n < want) {
          f.close();
          t.out.delete(n);
        }
      while (t.fed < end && t.dec!.decodeQueueSize < 4) {
        // (not read yet: on a later frame)
        const c = this.chunk(s, ti, t.fed);
        if (!c) break;
        t.dec!.decode(c);
        t.fed++;
      }
    }
    this.copyReady(s);
  }

  /** Copies decoded frames (every track's) the guest has no slot of yet, oldest first. */
  private copyReady(s: Stream): void {
    if (this.gate.closed) return this.gate.run(() => this.copyReady(s), s.held.copy);
    for (let n = s.want; n < s.want + AHEAD; n++) {
      if (n < 0 || s.copying.has(n)) continue;
      const fs = s.tracks.map((t) => t.out.get(n));
      if (fs.some((f) => !f)) continue;
      if (this.wasm._vnhost_video_has(s.id, n)) continue;
      // per track: the format, and how it copies (RGBA, converted back, when the frame is not
      // readable as I420/NV12: an opaque or other format)
      const how = (fs as VideoFrame[]).map((f) => {
        const fmt = f.format ? FORMATS[f.format] : undefined;
        const opts = (fmt === undefined ? { format: "RGBA" } : {}) as VideoFrameCopyToOptions;
        return { fmt: fmt ?? 2, opts, need: f.allocationSize(opts) };
      });
      const sizeAt = this.layoutBuf;
      new Uint32Array(this.wasm.wasmMemory.buffer, sizeAt, 1)[0] = how.reduce((a, h) => a + h.need, 0);
      const buf = this.wasm._vnhost_video_begin(s.id, n, sizeAt) >>> 0;
      if (!buf) return;
      s.copying.add(n);
      // (the copy writes into the memory when it ends: a state waits for it, memgate.ts)
      this.gate.enter();
      void this.copy(s, n, fs as VideoFrame[], how, buf).finally(() => this.gate.leave());
    }
  }

  private async copy(
    s: Stream,
    n: number,
    fs: VideoFrame[],
    how: { fmt: number; opts: VideoFrameCopyToOptions; need: number }[],
    buf: number,
  ): Promise<void> {
    const layout = new Uint32Array(14);
    let ok = 1;
    try {
      let at = 0;
      for (let t = 0; t < fs.length; t++) {
        const { fmt, opts, need } = how[t]!;
        const planes = await fs[t]!.copyTo(new Uint8Array(this.wasm.wasmMemory.buffer, buf + at, need), opts);
        layout[t * 7] = fmt;
        planes.forEach((p, i) => {
          if (i < 3) {
            layout[t * 7 + 1 + 2 * i] = at + p.offset;
            layout[t * 7 + 2 + 2 * i] = p.stride;
          }
        });
        at += need;
      }
    } catch (e) {
      this.note(`video ${s.id}: copy of frame ${n}: ${String(e)}`);
      ok = 0;
    }
    new Uint32Array(this.wasm.wasmMemory.buffer, this.layoutBuf, 14).set(layout);
    this.wasm._vnhost_video_ready(s.id, buf, this.layoutBuf, ok);
    s.copying.delete(n);
    for (const [t, f] of fs.entries()) {
      f.close();
      s.tracks[t]!.out.delete(n);
    }
    // more to decode and copy (the guest took the slot it waited for); after the state when one
    // is waiting for this copy
    if (this.streams.has(s.id)) this.gate.run(() => this.serve(s, s.want), s.held.serve);
  }

  /** The guest stops waiting for this stream's frames (it keeps its old pixels). */
  private fail(s: Stream): void {
    s.broken = true;
    this.wasm._vnhost_video_fail(s.id);
  }

  private close(s: Stream): void {
    for (const t of s.tracks) {
      for (const f of t.out.values()) f.close();
      if (t.dec && t.dec.state !== "closed") t.dec.close();
    }
  }
}
