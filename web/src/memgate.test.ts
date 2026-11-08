import { expect, test } from "vite-plus/test";
import { AudioDecode, type DecoderKind } from "./adec";
import { Gate } from "./memgate";
import { Video, type VideoWasm } from "./video";

const tick = (ms = 0) => new Promise((r) => setTimeout(r, ms));

test("gate: shutting waits for writes under way; what comes meanwhile runs after, in order, once per key", async () => {
  const g = new Gate();
  const ran: string[] = [];
  expect(g.enter()).toBe(true);
  let shut = false;
  const closing = g.close().then(() => (shut = true));
  await tick(5);
  expect(shut).toBe(false);
  expect(g.enter()).toBe(false);
  g.run(() => ran.push("a"));
  const key = {};
  g.run(() => ran.push("k1"), key);
  g.run(() => ran.push("k2"), key);
  g.run(() => ran.push("b"));
  g.leave();
  await closing;
  expect(ran).toEqual([]);
  g.open();
  expect(ran).toEqual(["a", "k1", "b"]);
  g.run(() => ran.push("now"));
  expect(ran.at(-1)).toBe("now");
});

test("gate: a write that never ends fails the state, and the gate opens again", async () => {
  const g = new Gate();
  g.enter();
  await expect(g.close(30)).rejects.toThrow();
  expect(g.closed).toBe(false);
});

// property: random writers (some across awaits) and random states; no write lands while shut,
// and every write asked for lands once
test("property: nothing is written while the gate is shut, nothing is lost", async () => {
  let seed = 7;
  const rnd = (n: number) => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff), seed % n);
  for (let run = 0; run < 60; run++) {
    const g = new Gate();
    let wrote = 0;
    let asked = 0;
    let bad = 0;
    /** the state's copy of the memory under way (the gate shut and the writes under way done) */
    let reading = false;
    const write = () => {
      if (reading) bad++;
      wrote++;
    };
    const writers: Promise<void>[] = [];
    for (let i = 0; i < 30; i++) {
      asked++;
      if (rnd(2)) g.run(write);
      else if (g.enter())
        writers.push(
          tick(rnd(4)).then(() => {
            write();
            g.leave();
          }),
        );
      else g.run(write);
      if (rnd(6) === 0) {
        await g.close();
        reading = true;
        await tick(rnd(3));
        reading = false;
        g.open();
      }
      await tick(rnd(2));
    }
    await Promise.all(writers);
    expect(bad, `run ${run}`).toBe(0);
    expect(wrote, `run ${run}`).toBe(asked);
  }
});

// ---- video.ts: a VideoFrame.copyTo that ends late, during a state ----

/** A module's memory with one stream of `frames` VP9 frames (all key frames). */
function videoModule(frames: number) {
  const mem = new Uint8Array(1 << 20);
  const dv = new DataView(mem.buffer);
  const header = 1024;
  const codec = new TextEncoder().encode("vp09.00.10.08");
  let o = header + 16;
  dv.setUint32(header + 4, 16, true);
  dv.setUint32(header + 8, 16, true);
  dv.setUint32(header + 12, 1, true);
  dv.setUint32(o, codec.length, true);
  mem.set(codec, o + 4);
  o += 4 + codec.length;
  dv.setUint32(o, frames, true);
  o += 4;
  for (let i = 0; i < frames; i++) dv.setUint32(o + 12 * i + 8, 1, true);
  o += 12 * frames;
  const len = o - header;
  const calls: string[] = [];
  let shut: () => boolean = () => false;
  const during: string[] = [];
  const has = new Set<number>();
  let want = 0;
  let nextBuf = 0x20000;
  const note = (c: string) => {
    calls.push(c);
    if (shut()) during.push(c);
  };
  const wasm: VideoWasm = {
    wasmMemory: { buffer: mem.buffer } as WebAssembly.Memory,
    _malloc: () => 0x10000,
    _vnhost_video_doorbell: () => 0x100,
    _vnhost_video_list(out) {
      note("list");
      new Uint32Array(mem.buffer, out, 6).set([1, header, len, want, 0, 0]);
      return 1;
    },
    _vnhost_video_has: (_id, n) => (note("has"), has.has(n) ? 1 : 0),
    _vnhost_video_chunk(_id, _t, _n, size) {
      note("chunk");
      new Uint32Array(mem.buffer, size, 1)[0] = 4;
      return 0x8000;
    },
    _vnhost_video_begin: () => (note("begin"), (nextBuf += 0x100)),
    _vnhost_video_ready(_id, buf, _layout, ok) {
      note(`ready ${buf.toString(16)} ${ok}`);
    },
    _vnhost_video_fail: () => note("fail"),
  };
  return {
    wasm,
    mem,
    calls,
    during,
    has,
    setWant: (n: number) => (want = n),
    watch: (f: () => boolean) => (shut = f),
  };
}

test("video: a frame copy under way when a state starts ends before the copy of the memory; outputs during it wait", async () => {
  const m = videoModule(8);
  const g = new Gate();
  let reading = false;
  m.watch(() => reading);
  let release: (() => void) | undefined;
  const copies: number[] = [];
  const outputs: ((f: VideoFrame) => void)[] = [];
  const frame = (n: number) =>
    ({
      timestamp: n * 1000,
      format: "I420",
      allocationSize: () => 16,
      async copyTo(dest: Uint8Array) {
        // the first copy ends late (a busy GPU process)
        if (n === 0) await new Promise<void>((r) => (release = r));
        dest.fill(0xa0 + n);
        copies.push(n);
        return [{ offset: 0, stride: 4 }];
      },
      close() {},
    }) as unknown as VideoFrame;
  const fed: number[] = [];
  const g2 = globalThis as unknown as { VideoDecoder?: unknown; EncodedVideoChunk?: unknown };
  const old = g2.VideoDecoder;
  const oldChunk = g2.EncodedVideoChunk;
  g2.EncodedVideoChunk = class {
    timestamp: number;
    constructor(i: { timestamp: number }) {
      this.timestamp = i.timestamp;
    }
  };
  g2.VideoDecoder = class {
    static isConfigSupported = async () => ({ supported: true });
    state = "unconfigured";
    decodeQueueSize = 0;
    constructor(init: { output: (f: VideoFrame) => void }) {
      outputs.push(init.output);
    }
    configure() {
      this.state = "configured";
    }
    reset() {}
    decode(c: { timestamp: number }) {
      fed.push(c.timestamp / 1000);
    }
    close() {}
  };
  try {
    const v = new Video(m.wasm, () => {}, g, false);
    v.tick();
    await tick(5);
    expect(fed.length).toBeGreaterThan(0);
    outputs[0]!(frame(0));
    await tick(1);
    expect(g.busy).toBe(1);
    // a state starts: the copy under way must end before the memory is read
    let shut = false;
    const closing = g.close().then(() => (shut = true));
    await tick(5);
    expect(shut).toBe(false);
    release!();
    await closing;
    reading = true;
    expect(copies).toEqual([0]);
    expect(m.calls.some((c) => c.startsWith("ready"))).toBe(true);
    // the memory, as the state's worker reads it
    const before = m.mem.slice();
    // frames decoded meanwhile, a tick: nothing reaches the memory
    outputs[0]!(frame(1));
    outputs[0]!(frame(2));
    v.tick();
    await tick(10);
    expect(m.during).toEqual([]);
    expect(m.mem).toEqual(before);
    reading = false;
    g.open();
    await tick(5);
    expect(copies).toEqual([0, 1, 2]);
  } finally {
    g2.VideoDecoder = old;
    g2.EncodedVideoChunk = oldChunk;
  }
});

// ---- adec.ts: decoded samples that come during a state ----

test("audio: outputs during a state are written after it, in order, none lost", async () => {
  const mem = new Uint8Array(1 << 20);
  const g = new Gate();
  const pcm: number[][] = [];
  const during: string[] = [];
  let seq = 0;
  let output!: (d: AudioData) => void;
  const kind = class {
    static isConfigSupported = async () => ({ supported: true });
    state = "configured";
    decodeQueueSize = 0;
    constructor(init: { output: (d: AudioData) => void }) {
      output = init.output;
    }
    configure() {}
    decode() {}
    close() {}
  } as unknown as DecoderKind;
  let pushed = 0;
  const codec = new TextEncoder().encode("mp3");
  mem.set(codec, 0x4000);
  const note = (c: string) => g.closed && during.push(c);
  const dec = new AudioDecode(
    {
      wasmMemory: { buffer: mem.buffer } as WebAssembly.Memory,
      _malloc: () => 0x8000,
      _free: () => {},
      _vnhost_adec_doorbell: () => 0x100,
      _vnhost_adec_list(out) {
        note("list");
        new Uint32Array(mem.buffer, out, 10).set([1, 1, 0x4000, codec.length, 44100, 1, 0, 0, 0, 0]);
        return 1;
      },
      _vnhost_adec_chunk(_id, _gen, size, sq) {
        note("chunk");
        if (pushed >= 6) return 0;
        pushed++;
        new Uint32Array(mem.buffer, size, 1)[0] = 4;
        new Uint32Array(mem.buffer, sq, 1)[0] = ++seq;
        return 0x5000;
      },
      _vnhost_adec_pcm(_id, _gen, p, frames) {
        note("pcm");
        pcm.push(Array.from(new Float32Array(mem.buffer, p, frames)));
      },
      _vnhost_adec_done: () => note("done"),
      _vnhost_adec_fail: () => note("fail"),
    },
    () => {},
    kind,
    (i) => i,
    false,
    g,
  );
  dec.tick();
  await tick(5);
  const data = (v: number) =>
    ({
      numberOfChannels: 1,
      numberOfFrames: 2,
      format: "f32",
      copyTo: (out: Float32Array) => out.set([v, v]),
      close() {},
    }) as unknown as AudioData;
  output(data(1));
  await g.close();
  output(data(2));
  output(data(3));
  dec.tick();
  await tick(40);
  expect(during).toEqual([]);
  expect(pcm).toEqual([[1, 1]]);
  g.open();
  output(data(4));
  expect(pcm).toEqual([
    [1, 1],
    [2, 2],
    [3, 3],
    [4, 4],
  ]);
});
