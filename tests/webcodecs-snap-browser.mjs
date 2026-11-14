// Save states and WebCodecs, in real Chromium (web/src/memgate.ts, video.ts, adec.ts): the
// page's decoders write straight into the module's memory, so a state must neither copy the
// memory while a VideoFrame.copyTo is under way nor let decoded frames or samples land while it
// copies. With the real VideoDecoder and AudioDecoder (VP9 and Opus made in the page with
// VideoEncoder/AudioEncoder) over a stand-in for sake's memory: the first frame copy is made to
// end late, a "state" shuts the gate meanwhile; the memory must stay as it was from the moment
// the gate is shut until it opens, and what the decoders gave meanwhile must land after, the
// same bytes in the same order as a run with no state in the middle.
//
//   node tests/webcodecs-snap-browser.mjs   (web's node_modules installed; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const work = mkdtempSync(join(tmpdir(), "masu-wcsnap-"));
const entry = join(work, "entry.ts");
writeFileSync(
  entry,
  ["memgate", "video", "adec"].map((m) => `export * from ${JSON.stringify(resolve(`web/src/${m}.ts`))};`).join("\n"),
);
const vp = await import(createRequire(resolve("web/package.json")).resolve("vite-plus"));
const build = vp.build ?? vp.default.build;
const built = await build({
  configFile: false,
  logLevel: "silent",
  root: work,
  build: { write: false, minify: false, lib: { entry, formats: ["iife"], name: "masu" } },
});
const script = (Array.isArray(built) ? built[0] : built).output[0].code;

const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  const page = await browser.newPage();
  page.on("console", (m) => process.env.VERBOSE && console.log(m.text()));
  page.on("pageerror", (e) => console.log(`page error: ${e}`));
  // (WebCodecs wants a secure context: a page of its own at an https URL)
  await page.route("https://masu.test/**", (r) =>
    r.fulfill({ contentType: "text/html", body: "<!doctype html><title>webcodecs and states</title>" }),
  );
  await page.goto("https://masu.test/");
  await page.addScriptTag({ content: script });

  const result = await page.evaluate(async () => {
    const { Gate, Video, AudioDecode } = window.masu;
    const wait = (ms) => new Promise((r) => setTimeout(r, ms));
    const W = 64,
      H = 48,
      FRAMES = 8;

    // ---- VP9 frames (all key frames: any may be asked for first) ----
    const chunks = [];
    const enc = new VideoEncoder({
      output: (c) => {
        const b = new Uint8Array(c.byteLength);
        c.copyTo(b);
        chunks.push({ key: c.type === "key", data: b });
      },
      error: (e) => {
        throw e;
      },
    });
    enc.configure({ codec: "vp09.00.10.08", width: W, height: H, bitrate: 200000 });
    const canvas = new OffscreenCanvas(W, H);
    const ctx = canvas.getContext("2d");
    for (let i = 0; i < FRAMES; i++) {
      ctx.fillStyle = `rgb(${i * 30},${255 - i * 30},${(i * 70) & 255})`;
      ctx.fillRect(0, 0, W, H);
      ctx.fillStyle = "#fff";
      ctx.fillRect(i * 6, i * 4, 10, 10);
      const f = new VideoFrame(canvas, { timestamp: i * 1000 });
      enc.encode(f, { keyFrame: true });
      f.close();
    }
    await enc.flush();
    enc.close();

    /** One run of the movie through video.ts; `state(gate, mem)` is called once a copy is under way. */
    async function movie(state) {
      const mem = new Uint8Array(4 << 20);
      const dv = new DataView(mem.buffer);
      const header = 1024;
      const codec = new TextEncoder().encode("vp09.00.10.08");
      dv.setUint32(header + 4, W, true);
      dv.setUint32(header + 8, H, true);
      dv.setUint32(header + 12, 1, true);
      let o = header + 16;
      dv.setUint32(o, codec.length, true);
      mem.set(codec, o + 4);
      o += 4 + codec.length;
      dv.setUint32(o, FRAMES, true);
      o += 4;
      for (let i = 0; i < FRAMES; i++) dv.setUint32(o + 12 * i + 8, chunks[i].key ? 1 : 0, true);
      o += 12 * FRAMES;
      const len = o - header;
      const gate = new Gate();
      let reading = false;
      const during = [];
      const slots = new Map();
      let want = 0;
      let next = 0x100000;
      const note = (c) => reading && during.push(c);
      const wasm = {
        wasmMemory: { buffer: mem.buffer },
        _malloc: () => 0x10000,
        _vnhost_video_doorbell: () => 0x100,
        _vnhost_video_list(out) {
          note("list");
          new Uint32Array(mem.buffer, out, 6).set([1, header, len, want, 0, 0]);
          return 1;
        },
        _vnhost_video_has: (_id, n) => (note("has"), slots.has(n) ? 1 : 0),
        _vnhost_video_chunk(_id, _t, n, size) {
          note("chunk");
          const at = 0x80000 + n * 0x4000;
          mem.set(chunks[n].data, at);
          new Uint32Array(mem.buffer, size, 1)[0] = chunks[n].data.length;
          return at;
        },
        _vnhost_video_begin(_id, n, size) {
          note("begin");
          const need = new Uint32Array(mem.buffer, size, 1)[0];
          const at = next;
          next += need + 64;
          slots.set(n, { at, need, ok: -1 });
          return at;
        },
        _vnhost_video_ready(_id, buf, _layout, ok) {
          note("ready");
          for (const s of slots.values()) if (s.at === buf) s.ok = ok;
        },
        _vnhost_video_fail: () => note("fail"),
      };
      const video = new Video(wasm, () => {}, gate, false);
      let tookState = false;
      let unchanged = true;
      for (let t = 0; t < 400 && [...slots.values()].filter((s) => s.ok === 1).length < FRAMES; t++) {
        video.tick();
        if (state && !tookState && gate.busy > 0) {
          tookState = true;
          await gate.close();
          reading = true;
          const copy = mem.slice();
          // the decoder goes on meanwhile; ticks come (the page's frames)
          for (let k = 0; k < 10; k++) {
            video.tick();
            await wait(20);
          }
          unchanged = copy.every((b, i) => b === mem[i]);
          reading = false;
          gate.open();
        }
        // (the guest takes the frames it asked for, then asks for more)
        if (slots.get(want)?.ok === 1 && want < FRAMES - 1) want++;
        await wait(5);
      }
      const frames = [...slots.entries()]
        .sort((a, b) => a[0] - b[0])
        .map(([n, s]) => [n, s.ok, Array.from(mem.subarray(s.at, s.at + s.need))]);
      return { frames, during, tookState, unchanged };
    }

    // the first copy of each run ends late (a busy GPU process)
    const copyTo = VideoFrame.prototype.copyTo;
    let first = true;
    VideoFrame.prototype.copyTo = async function (...a) {
      if (first) {
        first = false;
        await wait(150);
      }
      return copyTo.apply(this, a);
    };
    const plain = await movie(null);
    first = true;
    const withState = await movie(true);

    // ---- Opus samples ----
    const audioChunks = [];
    const aenc = new AudioEncoder({
      output: (c) => {
        const b = new Uint8Array(c.byteLength);
        c.copyTo(b);
        audioChunks.push(b);
      },
      error: (e) => {
        throw e;
      },
    });
    aenc.configure({ codec: "opus", sampleRate: 48000, numberOfChannels: 1, bitrate: 64000 });
    const pcmIn = new Float32Array(48000 / 2).map((_, i) => Math.sin(i / 10) * 0.5);
    aenc.encode(new AudioData({ format: "f32", sampleRate: 48000, numberOfFrames: pcmIn.length, numberOfChannels: 1, timestamp: 0, data: pcmIn }));
    await aenc.flush();
    aenc.close();

    async function sound(state) {
      const mem = new Uint8Array(4 << 20);
      const gate = new Gate();
      let reading = false;
      const during = [];
      const pcm = [];
      let pushed = 0;
      let done = 0;
      const codec = new TextEncoder().encode("opus");
      mem.set(codec, 0x4000);
      const note = (c) => reading && during.push(c);
      const adec = new AudioDecode(
        {
          wasmMemory: { buffer: mem.buffer },
          _malloc: (n) => 0x100000,
          _free: () => {},
          _vnhost_adec_doorbell: () => 0x100,
          _vnhost_adec_list(out) {
            note("list");
            new Uint32Array(mem.buffer, out, 10).set([1, 1, 0x4000, codec.length, 48000, 1, 0, 0, 0, 0]);
            return 1;
          },
          _vnhost_adec_chunk(_id, _gen, size, sq) {
            note("chunk");
            if (pushed >= audioChunks.length) return 0;
            const b = audioChunks[pushed++];
            mem.set(b, 0x8000);
            new Uint32Array(mem.buffer, size, 1)[0] = b.length;
            new Uint32Array(mem.buffer, sq, 1)[0] = pushed;
            return 0x8000;
          },
          _vnhost_adec_pcm(_id, _gen, p, frames, ch) {
            note("pcm");
            pcm.push(...new Float32Array(mem.buffer, p, frames * ch));
          },
          _vnhost_adec_done: (_id, _gen, seq) => (note("done"), (done = seq)),
          _vnhost_adec_fail: () => note("fail"),
        },
        () => {},
        undefined,
        undefined,
        false,
        gate,
      );
      adec.tick();
      let unchanged = true;
      if (state) {
        // (the decoder's first outputs come while the state copies)
        await wait(1);
        adec.tick();
        await gate.close();
        reading = true;
        const copy = mem.slice();
        await wait(300);
        unchanged = copy.every((b, i) => b === mem[i]);
        reading = false;
        gate.open();
      }
      for (let t = 0; t < 200 && done < audioChunks.length; t++) {
        adec.tick();
        await wait(5);
      }
      return { pcm, during, unchanged, done, chunks: audioChunks.length };
    }
    const soundPlain = await sound(false);
    const soundState = await sound(true);
    return { plain, withState, soundPlain, soundState };
  });

  const { plain, withState, soundPlain, soundState } = result;
  assert.equal(plain.frames.length, 8, "every frame copied");
  assert.ok(plain.frames.every(([, ok]) => ok === 1), "frames copied fine");
  assert.ok(withState.tookState, "a state was taken while a copy was under way");
  assert.deepEqual(withState.during, [], "the module was called while the state copied");
  assert.ok(withState.unchanged, "the memory changed while the state copied");
  assert.deepEqual(withState.frames, plain.frames, "the frames after the state are not those of a run without it");
  assert.ok(soundPlain.pcm.length > 10000, `sound decoded (${soundPlain.pcm.length} samples)`);
  assert.equal(soundState.done, soundState.chunks, "every chunk done after the state");
  assert.deepEqual(soundState.during, [], "the module was called while the state copied (sound)");
  assert.ok(soundState.unchanged, "the memory changed while the state copied (sound)");
  assert.equal(soundState.pcm.length, soundPlain.pcm.length, "samples lost or added by the state");
  assert.ok(soundState.pcm.every((v, i) => v === soundPlain.pcm[i]), "samples differ from a run without the state");
  console.log(`webcodecs-snap-browser: ok (${plain.frames.length} frames, ${soundPlain.pcm.length} samples)`);
} finally {
  await browser.close();
}
