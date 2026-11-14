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
    _vnhost_video_chunk(_id, _t, n, size) {
      note("chunk");
      new Uint32Array(mem.buffer, size, 2).set([4, n]);
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
    async flush() {}
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

test("video: a jump back to a frame already passed (a seek back after the end) feeds the decoder again from that frame", async () => {
  const m = videoModule(8);
  const g = new Gate();
  const fed: number[] = [];
  let resets = 0;
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
    configure() {
      this.state = "configured";
    }
    reset() {
      resets++;
    }
    decode(c: { timestamp: number }) {
      fed.push(c.timestamp / 1000);
    }
    async flush() {}
    close() {}
  };
  try {
    const v = new Video(m.wasm, () => {}, g, false);
    v.tick();
    await tick(5);
    // read to the end: every frame fed once, no reset
    for (let n = 0; n < 8; n++) {
      m.setWant(n);
      v.tick();
      await tick(1);
    }
    expect(fed).toEqual([0, 1, 2, 3, 4, 5, 6, 7]);
    const before = resets;
    // the same frame again: nothing new
    v.tick();
    await tick(1);
    expect(resets).toBe(before);
    // back to frame 3 (all key frames): the decoder starts over there
    fed.length = 0;
    m.setWant(3);
    v.tick();
    await tick(1);
    expect(resets).toBe(before + 1);
    expect(fed[0]).toBe(3);
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

test("gate: a queued write that throws does not keep the rest queued (property, seeded)", async () => {
  let x = 13;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 200; run++) {
    const g = new Gate();
    const errors: unknown[] = [];
    g.failed = (e) => errors.push(e);
    await g.close();
    const want: number[] = [];
    const ran: number[] = [];
    let throws = 0;
    for (let i = 0; i < 1 + rnd(10); i++) {
      if (rnd(3) === 0) {
        throws++;
        g.run(() => {
          throw new Error(`w${i}`);
        });
      } else {
        want.push(i);
        g.run(() => ran.push(i));
      }
    }
    g.open();
    expect(ran, `run ${run}`).toEqual(want);
    expect(errors.length).toBe(throws);
    // (open: what comes now runs at once)
    g.run(() => ran.push(-1));
    expect(ran.at(-1)).toBe(-1);
  }
});

test("gate: closeWhile opens again whatever f does; a close that timed out leaves no waiter", async () => {
  const g = new Gate();
  await expect(
    g.closeWhile(async () => {
      expect(g.closed).toBe(true);
      throw new Error("copy failed");
    }),
  ).rejects.toThrow("copy failed");
  expect(g.closed).toBe(false);
  expect(await g.closeWhile(async () => 7)).toBe(7);
  expect(g.closed).toBe(false);
  // (a write under way that does not end: the close gives up, its waiter goes)
  expect(g.enter()).toBe(true);
  await expect(g.close(5)).rejects.toThrow();
  expect(g.closed).toBe(false);
  expect((g as unknown as { idle: unknown[] }).idle.length).toBe(0);
  g.leave();
});

// ---- video.ts: a decoder that breaks, a stream that goes while a copy runs ----

function fakeVideo(onMake: (d: FakeDec) => void) {
  const g2 = globalThis as unknown as {
    VideoDecoder?: unknown;
    EncodedVideoChunk?: unknown;
    VideoFrame?: unknown;
  };
  const old = [g2.VideoDecoder, g2.EncodedVideoChunk];
  g2.EncodedVideoChunk = class {
    timestamp: number;
    constructor(i: { timestamp: number }) {
      this.timestamp = i.timestamp;
    }
  };
  g2.VideoDecoder = class implements FakeDec {
    static isConfigSupported = async () => ({ supported: true });
    state = "unconfigured";
    decodeQueueSize = 0;
    configs: { hardwareAcceleration?: string }[] = [];
    fed: number[] = [];
    closed = false;
    init: FakeDec["init"];
    constructor(init: FakeDec["init"]) {
      this.init = init;
      onMake(this);
    }
    configure(c: { hardwareAcceleration?: string }) {
      this.configs.push(c);
      this.state = "configured";
    }
    reset() {
      this.state = "unconfigured";
    }
    decode(c: { timestamp: number }) {
      this.fed.push(c.timestamp / 1000);
    }
    async flush() {}
    close() {
      this.closed = true;
      this.state = "closed";
    }
  };
  return () => {
    g2.VideoDecoder = old[0];
    g2.EncodedVideoChunk = old[1];
  };
}
interface FakeDec {
  configs: { hardwareAcceleration?: string }[];
  fed: number[];
  closed: boolean;
  init: { output: (f: unknown) => void; error: (e: unknown) => void };
}
function fakeFrame(n: number, onClose: () => void, copy?: () => Promise<unknown>) {
  return {
    timestamp: n * 1000,
    codedWidth: 16,
    codedHeight: 16,
    visibleRect: { x: 0, y: 0, width: 16, height: 16 },
    format: "I420",
    allocationSize: () => 16 * 16 * 2,
    copyTo: copy ?? (async () => [{ offset: 0, stride: 16 }]),
    close: onClose,
  };
}

test("video: a decoder that breaks is made again once in software, fed from the key frame; a second break fails the stream and lets its frames go", async () => {
  const m = videoModule(6);
  const decs: FakeDec[] = [];
  const restore = fakeVideo((d) => decs.push(d));
  try {
    const v = new Video(m.wasm, () => {}, new Gate(), false);
    v.tick();
    await tick(5);
    for (let n = 0; n < 3; n++) {
      m.setWant(n);
      v.tick();
      await tick(1);
    }
    expect(decs).toHaveLength(1);
    expect(decs[0]!.fed.slice(0, 3)).toEqual([0, 1, 2]);
    // a frame waiting in the first decoder's outputs: let go when it breaks
    let closed = 0;
    decs[0]!.init.output(
      fakeFrame(
        5,
        () => closed++,
        () => new Promise(() => {}),
      ),
    );
    decs[0]!.init.error(new Error("hw gone"));
    await tick(5);
    expect(decs).toHaveLength(2);
    expect(decs[1]!.configs.at(-1)?.hardwareAcceleration).toBe("prefer-software");
    expect(decs[1]!.fed[0]).toBe(2);
    expect(closed).toBe(1);
    expect(m.calls).not.toContain("fail");
    // (the old one's late outputs: dropped, not shown)
    let late = 0;
    decs[0]!.init.output(fakeFrame(4, () => late++));
    expect(late).toBe(1);
    // a frame held, then the software decoder breaks too: failed, frames and decoder let go
    let held = 0;
    decs[1]!.init.output(
      fakeFrame(
        5,
        () => held++,
        () => new Promise(() => {}),
      ),
    );
    decs[1]!.init.error(new Error("sw gone"));
    await tick(5);
    expect(decs).toHaveLength(2);
    expect(m.calls.filter((c) => c === "fail")).toHaveLength(1);
    expect(held).toBe(1);
    expect(decs[1]!.closed).toBe(true);
    // (a broken stream is not fed again)
    const fed = decs[1]!.fed.length;
    m.setWant(4);
    v.tick();
    await tick(2);
    expect(decs[1]!.fed.length).toBe(fed);
  } finally {
    restore();
  }
});

test("video: a copy that ends after the guest closed the stream tells the guest nothing and lets the frame go", async () => {
  const m = videoModule(4);
  const decs: FakeDec[] = [];
  const restore = fakeVideo((d) => decs.push(d));
  try {
    const v = new Video(m.wasm, () => {}, new Gate(), false);
    v.tick();
    await tick(5);
    m.setWant(0);
    v.tick();
    await tick(1);
    let done!: () => void;
    let closed = 0;
    decs[0]!.init.output(
      fakeFrame(
        0,
        () => closed++,
        () => new Promise((r) => (done = () => r([{ offset: 0, stride: 16 }]))),
      ),
    );
    await tick(3);
    expect(done).toBeDefined();
    // the guest drops the stream: the list comes back empty
    const w = m.wasm as { _vnhost_video_list: (out: number, max: number) => number };
    const list = w._vnhost_video_list.bind(m.wasm);
    w._vnhost_video_list = () => 0;
    v.tick();
    w._vnhost_video_list = list;
    done();
    await tick(5);
    expect(m.calls.some((c) => c.startsWith("ready"))).toBe(false);
    expect(closed).toBeGreaterThanOrEqual(1);
  } finally {
    restore();
  }
});

test("video: a stream whose header cannot be read fails alone, once; a decoder that throws on feeding fails its stream", async () => {
  const m = videoModule(4);
  const decs: FakeDec[] = [];
  const restore = fakeVideo((d) => decs.push(d));
  try {
    const w = m.wasm as { _vnhost_video_list: (out: number, max: number) => number };
    const list = w._vnhost_video_list.bind(m.wasm);
    // a second stream (id 2) with a header past the end of memory
    w._vnhost_video_list = (out, max) => {
      const n = list(out, max);
      new Uint32Array(m.mem.buffer, out + 24, 6).set([2, 0xfffff0, 64, 0, 0, 0]);
      return n + 1;
    };
    const notes: string[] = [];
    const v = new Video(m.wasm, (t) => notes.push(t), new Gate(), false);
    for (let i = 0; i < 5; i++) {
      v.tick();
      await tick(2);
    }
    expect(m.calls.filter((c) => c === "fail")).toHaveLength(1);
    // (stream 1 is served all the same)
    expect(decs).toHaveLength(1);
    expect(decs[0]!.fed.length).toBeGreaterThan(0);
    // stream 1's decoder throws when fed: that stream fails, the tick does not throw
    (decs[0] as unknown as { decode: () => void }).decode = () => {
      throw new Error("InvalidStateError");
    };
    m.setWant(3);
    expect(() => v.tick()).not.toThrow();
    await tick(2);
    expect(m.calls.filter((c) => c === "fail")).toHaveLength(2);
    expect(decs[0]!.closed).toBe(true);
  } finally {
    restore();
  }
});
