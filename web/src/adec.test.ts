import { expect, test } from "vite-plus/test";
import { AudioDecode, FIRST_QUIET, QUIET, type AdecWasm, type DecoderKind } from "./adec";

const tick = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** kura's side of crates/kura/src/adec.rs, in memory: streams, their chunks, what came back. */
class FakeKura implements AdecWasm {
  wasmMemory = new WebAssembly.Memory({ initial: 4 });
  private top = 1024;
  streams = new Map<
    number,
    { gen: number; codec: string; rate: number; ch: number; desc: number[]; queue: number[][]; taken: number }
  >();
  pcm: { id: number; gen: number; frames: number; ch: number; data: number[] }[] = [];
  done: [number, number, number][] = [];
  failed: [number, number][] = [];
  _malloc(n: number): number {
    const p = this.top;
    this.top += (n + 15) & ~15;
    return p;
  }
  _free(): void {}
  _vnhost_adec_doorbell(): number {
    return 512;
  }
  _vnhost_adec_list(out: number, max: number): number {
    const u32 = new Uint32Array(this.wasmMemory.buffer);
    const u8 = new Uint8Array(this.wasmMemory.buffer);
    let k = 0;
    for (const [id, s] of this.streams) {
      if (k === max) break;
      const cp = this._malloc(s.codec.length);
      u8.set(new TextEncoder().encode(s.codec), cp);
      const dp = this._malloc(s.desc.length);
      u8.set(s.desc, dp);
      u32.set(
        [id, s.gen, cp, s.codec.length, s.rate, s.ch, dp, s.desc.length, s.queue.length, 0],
        out / 4 + k * 10,
      );
      k++;
    }
    return k;
  }
  _vnhost_adec_chunk(id: number, gen: number, size: number, seq: number): number {
    const s = this.streams.get(id);
    if (!s || s.gen !== gen || !s.queue.length) return 0;
    const b = s.queue.shift()!;
    s.taken++;
    const p = this._malloc(b.length + 1);
    new Uint8Array(this.wasmMemory.buffer).set(b, p);
    const u32 = new Uint32Array(this.wasmMemory.buffer);
    u32[size / 4] = b.length;
    u32[seq / 4] = s.taken;
    return p;
  }
  _vnhost_adec_pcm(id: number, gen: number, pcm: number, frames: number, ch: number): void {
    this.pcm.push({
      id,
      gen,
      frames,
      ch,
      data: Array.from(new Float32Array(this.wasmMemory.buffer, pcm, frames * ch)),
    });
  }
  _vnhost_adec_done(id: number, gen: number, seq: number): void {
    this.done.push([id, gen, seq]);
  }
  _vnhost_adec_fail(id: number, gen: number): void {
    this.failed.push([id, gen]);
  }
}

/**
 * An AudioDecoder whose chunk k (first byte v) gives one planar output of 2 frames holding v and
 * v + 0.5 per channel, a tick later; `silent(bytes)` chunks give none; codecs in `refused` are
 * not supported.
 */
function fakeDecoder(
  opts: { silent?: (b: Uint8Array) => boolean; refused?: string[]; failOn?: number; firstLate?: number } = {},
) {
  const made: { config: AudioDecoderConfig | null; closed: boolean; fed: number[][] }[] = [];
  const Kind = class {
    state = "unconfigured";
    decodeQueueSize = 0;
    rec = { config: null as AudioDecoderConfig | null, closed: false, fed: [] as number[][] };
    constructor(private init: { output: (d: AudioData) => void; error: (e: unknown) => void }) {
      made.push(this.rec);
    }
    static async isConfigSupported(c: AudioDecoderConfig) {
      return { supported: !(opts.refused ?? []).includes(c.codec) };
    }
    configure(c: AudioDecoderConfig) {
      this.rec.config = c;
      this.state = "configured";
    }
    decode(c: { data: Uint8Array }) {
      const b = c.data;
      this.rec.fed.push(Array.from(b));
      this.decodeQueueSize++;
      // (firstLate: as Chrome's, the first chunk leaves the queue at once and its output comes
      // that many ms later, while the decoder starts)
      const late = opts.firstLate !== undefined && this.rec.fed.length === 1 ? opts.firstLate : 0;
      if (late) setTimeout(() => this.decodeQueueSize--, 0);
      setTimeout(() => {
        if (!late) this.decodeQueueSize--;
        if (this.state === "closed") return;
        if (opts.failOn !== undefined && b[0] === opts.failOn) return this.init.error(new Error("bad"));
        if (opts.silent?.(b)) return;
        const ch = this.rec.config!.numberOfChannels;
        const planes = Array.from({ length: ch }, (_, c) => [b[0]! + c, b[0]! + c + 0.5]);
        this.init.output({
          numberOfChannels: ch,
          numberOfFrames: 2,
          format: "f32-planar",
          copyTo: (dst: Float32Array, o: { planeIndex: number }) => dst.set(planes[o.planeIndex]!),
          close: () => {},
        } as unknown as AudioData);
      }, 1 + late);
    }
    close() {
      this.state = "closed";
      this.rec.closed = true;
    }
  };
  return { Kind: Kind as unknown as DecoderKind, made };
}

const chunk = (i: { type: "key"; timestamp: number; data: Uint8Array }) => i;

test("chunks fed in order, samples interleaved, done once all outputs are back", async () => {
  const k = new FakeKura();
  const { Kind, made } = fakeDecoder();
  const a = new AudioDecode(k, () => {}, Kind, chunk, false);
  k.streams.set(7, {
    gen: 0,
    codec: "mp3",
    rate: 44100,
    ch: 2,
    desc: [],
    queue: [[10], [20, 1], [30]],
    taken: 0,
  });
  a.tick();
  await tick(5);
  expect(made[0]!.config).toEqual({ codec: "mp3", sampleRate: 44100, numberOfChannels: 2 });
  expect(made[0]!.fed).toEqual([[10], [20, 1], [30]]);
  expect(k.pcm.map((p) => p.data)).toEqual([
    [10, 11, 10.5, 11.5],
    [20, 21, 20.5, 21.5],
    [30, 31, 30.5, 31.5],
  ]);
  expect(k.done).toEqual([[7, 0, 3]]);
  // more later: done again for them
  k.streams.get(7)!.queue.push([40]);
  a.tick();
  await tick(5);
  expect(k.done.at(-1)).toEqual([7, 0, 4]);
});

test("a description is passed on; Vorbis's first packet gives nothing and is not waited for", async () => {
  const k = new FakeKura();
  const { Kind, made } = fakeDecoder({ silent: (b) => b[0] === 1 });
  const a = new AudioDecode(k, () => {}, Kind, chunk, false);
  k.streams.set(1, {
    gen: 0,
    codec: "vorbis",
    rate: 22050,
    ch: 1,
    desc: [2, 3, 4],
    queue: [[1], [5]],
    taken: 0,
  });
  a.tick();
  await tick(5);
  expect(Array.from(made[0]!.config!.description as Uint8Array)).toEqual([2, 3, 4]);
  expect(k.done).toEqual([[1, 0, 2]]);
});

test("a chunk that gives nothing is let go once the decoder is idle", async () => {
  const k = new FakeKura();
  const { Kind } = fakeDecoder({ silent: (b) => b[0] === 99 });
  const a = new AudioDecode(k, () => {}, Kind, chunk, false);
  k.streams.set(2, { gen: 0, codec: "mp3", rate: 44100, ch: 1, desc: [], queue: [[3], [99], [4]], taken: 0 });
  a.tick();
  await tick(5);
  expect(k.done).toEqual([]);
  await tick(QUIET + 10);
  expect(k.done).toEqual([[2, 0, 3]]);
  expect(k.pcm.length).toBe(2);
});

test("an unsupported codec or a decoder error fails the stream", async () => {
  const k = new FakeKura();
  const lines: string[] = [];
  const { Kind } = fakeDecoder({ refused: ["wma"], failOn: 66 });
  const a = new AudioDecode(k, (l) => lines.push(l), Kind, chunk, false);
  k.streams.set(3, { gen: 0, codec: "wma", rate: 44100, ch: 2, desc: [], queue: [[1]], taken: 0 });
  k.streams.set(4, { gen: 5, codec: "mp3", rate: 44100, ch: 2, desc: [], queue: [[66]], taken: 0 });
  a.tick();
  await tick(5);
  expect(k.failed).toEqual([
    [3, 0],
    [4, 5],
  ]);
  expect(lines.some((l) => l.includes("wma") && l.includes("not supported"))).toBe(true);
});

test("a new generation gets a new decoder; a stream gone closes its decoder", async () => {
  const k = new FakeKura();
  const { Kind, made } = fakeDecoder();
  const a = new AudioDecode(k, () => {}, Kind, chunk, false);
  k.streams.set(9, { gen: 0, codec: "mp3", rate: 44100, ch: 1, desc: [], queue: [[1]], taken: 0 });
  a.tick();
  await tick(5);
  const s = k.streams.get(9)!;
  s.gen = 1;
  s.taken = 0;
  s.queue.push([2]);
  a.tick();
  await tick(5);
  expect(made.length).toBe(2);
  expect(made[0]!.closed).toBe(true);
  expect(made[1]!.fed).toEqual([[2]]);
  expect(k.done.at(-1)).toEqual([9, 1, 1]);
  k.streams.delete(9);
  a.tick();
  expect(made[1]!.closed).toBe(true);
});

test("stop(): the doorbell no longer looked at, the decoders closed (a child process ended)", async () => {
  const k = new FakeKura();
  k.wasmMemory = new WebAssembly.Memory({ initial: 4, maximum: 4, shared: true });
  let lists = 0;
  const list = k._vnhost_adec_list.bind(k);
  k._vnhost_adec_list = (out, max) => (lists++, list(out, max));
  const { Kind, made } = fakeDecoder();
  const a = new AudioDecode(k, () => {}, Kind, chunk, true);
  const ring = () => {
    const i32 = new Int32Array(k.wasmMemory.buffer);
    Atomics.add(i32, 512 / 4, 1);
    Atomics.notify(i32, 512 / 4);
  };
  k.streams.set(3, { gen: 0, codec: "mp3", rate: 44100, ch: 1, desc: [], queue: [[10]], taken: 0 });
  ring();
  await tick(20);
  expect(lists).toBeGreaterThan(0);
  expect(made.length).toBe(1);
  a.stop();
  expect(made[0]!.closed).toBe(true);
  const seen = lists;
  ring();
  await tick(20);
  ring();
  await tick(20);
  expect(lists).toBe(seen);
});

test("a tick that throws does not end the watching", async () => {
  const k = new FakeKura();
  k.wasmMemory = new WebAssembly.Memory({ initial: 4, maximum: 4, shared: true });
  let lists = 0;
  const list = k._vnhost_adec_list.bind(k);
  k._vnhost_adec_list = (out, max) => {
    if (++lists === 1) throw new RangeError("offset is out of bounds");
    return list(out, max);
  };
  const notes: string[] = [];
  const { Kind, made } = fakeDecoder();
  const a = new AudioDecode(k, (l) => notes.push(l), Kind, chunk, true);
  const ring = () => {
    const i32 = new Int32Array(k.wasmMemory.buffer);
    Atomics.add(i32, 512 / 4, 1);
    Atomics.notify(i32, 512 / 4);
  };
  ring();
  await tick(20);
  k.streams.set(3, { gen: 0, codec: "mp3", rate: 44100, ch: 1, desc: [], queue: [[10]], taken: 0 });
  ring();
  await tick(20);
  expect(notes.some((l) => l.includes("RangeError"))).toBe(true);
  expect(made.length).toBe(1);
  a.stop();
});

test("after a chunk that gave nothing, later chunks are done as their outputs come, not QUIET each", async () => {
  const k = new FakeKura();
  const { Kind } = fakeDecoder({ silent: (b) => b[0] === 99 });
  const a = new AudioDecode(k, () => {}, Kind, chunk, false);
  k.streams.set(2, { gen: 0, codec: "mp3", rate: 44100, ch: 1, desc: [], queue: [[3], [99], [4]], taken: 0 });
  a.tick();
  await tick(QUIET + 15);
  expect(k.done).toEqual([[2, 0, 3]]);
  for (let i = 0; i < 5; i++) {
    k.streams.get(2)!.queue.push([10 + i]);
    a.tick();
    await tick(5);
    expect(k.done.at(-1), `chunk ${4 + i}`).toEqual([2, 0, 4 + i]);
  }
});

/**
 * Random batches of chunks, some of which give nothing (a damaged frame):
 * every batch is said done with its last seq; never before every output of the batch's chunks
 * that give one has been written; and a batch whose chunks all give one output each is done
 * without waiting for QUIET (property, seeded).
 */
test("property: done for every batch, never before its samples, quick when nothing was lost", async () => {
  let x = 77;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 12; run++) {
    const k = new FakeKura();
    // (byte 99: no output)
    const { Kind } = fakeDecoder({ silent: (b) => b[0] === 99 });
    const a = new AudioDecode(k, () => {}, Kind, chunk, false);
    k.streams.set(1, { gen: 0, codec: "mp3", rate: 44100, ch: 1, desc: [], queue: [], taken: 0 });
    let seq = 0;
    let lossyBefore = false;
    for (let batch = 0; batch < 6; batch++) {
      const n = 1 + rnd(4);
      const bytes = Array.from({ length: n }, () => (rnd(5) === 0 ? 99 : 1 + rnd(90)));
      const lossy = bytes.includes(99);
      k.streams.get(1)!.queue.push(...bytes.map((b) => [b]));
      seq += n;
      const pcmBefore = k.pcm.length;
      const want = bytes.filter((b) => b !== 99).length;
      a.tick();
      await tick(6);
      if (!lossy) {
        expect(k.done.at(-1), `run ${run} batch ${batch}: quick (lossy before: ${lossyBefore})`).toEqual([
          1,
          0,
          seq,
        ]);
      } else {
        await tick(QUIET + 15);
        expect(k.done.at(-1), `run ${run} batch ${batch}`).toEqual([1, 0, seq]);
      }
      expect(k.pcm.length - pcmBefore, `run ${run} batch ${batch}: samples before done`).toBe(want);
      lossyBefore ||= lossy;
    }
    a.stop();
  }
}, 60_000);

test("a first output later than QUIET (the decoder starting) is waited for, not said done without it", async () => {
  const k = new FakeKura();
  const { Kind } = fakeDecoder({ firstLate: QUIET * 4 });
  const a = new AudioDecode(k, () => {}, Kind, chunk, false);
  k.streams.set(3, { gen: 0, codec: "mp3", rate: 44100, ch: 1, desc: [], queue: [[7]], taken: 0 });
  a.tick();
  await tick(QUIET * 2);
  expect(k.done).toEqual([]);
  await tick(QUIET * 3);
  expect(k.pcm.length).toBe(1);
  expect(k.done).toEqual([[3, 0, 1]]);
  // later chunks: QUIET again for one that gives nothing
  expect(FIRST_QUIET).toBeGreaterThan(QUIET * 4);
});
