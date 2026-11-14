// The sound's AudioWorklet (web/src/audio.ts) in real Chromium, built as the app builds it
// (minified: the worklet's code is sinkStep's own text): an OfflineAudioContext plays a ring
// in a SharedArrayBuffer whose head moves at the audio rate, then stops (an underrun), then
// starts again. While fed, the output is the ring's samples exactly; at the stop the sound
// fades out instead of cutting (no step near full scale into the silence),
// and it fades in again after.
//
//   node tests/audio-sink-browser.mjs   (web's node_modules installed; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import { resolve } from "node:path";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const vp = await import(createRequire(resolve("web/package.json")).resolve("vite-plus"));
const build = vp.build ?? vp.default.build;
const built = await build({
  configFile: false,
  logLevel: "silent",
  build: { write: false, minify: true, lib: { entry: resolve("web/src/audio.ts"), formats: ["iife"], name: "ippaiAudio" } },
});
const script = (Array.isArray(built) ? built[0] : built).output[0].code;

const browser = await chromium.launch({ args: ["--no-proxy-server"] });
try {
  const page = await browser.newPage();
  const origin = "https://ippai.test";
  // (cross-origin isolated: SharedArrayBuffer, as play.html is served)
  await page.route(`${origin}/**`, (r) =>
    r.fulfill({
      contentType: "text/html",
      headers: { "cross-origin-opener-policy": "same-origin", "cross-origin-embedder-policy": "require-corp" },
      body: "<!doctype html><title>sink</title>",
    }),
  );
  await page.goto(`${origin}/`);
  await page.addScriptTag({ content: script });
  const r = await page.evaluate(async () => {
    const RATE = 44100;
    const FRAMES = 1 << 16;
    const sab = new SharedArrayBuffer(8 + FRAMES * 8);
    const head = new Uint32Array(sab, 0, 1);
    const data = new Float32Array(sab, 8, FRAMES * 2);
    // a full-scale square wave, its period 128 frames
    for (let i = 0; i < FRAMES; i++) data[2 * i] = data[2 * i + 1] = i & 64 ? 0.9 : -0.9;
    const LEN = RATE; // one second
    const ctx = new OfflineAudioContext(2, LEN, RATE);
    const url = URL.createObjectURL(new Blob([window.ippaiAudio.worklet], { type: "text/javascript" }));
    await ctx.audioWorklet.addModule(url);
    const node = new AudioWorkletNode(ctx, "vnvm-sink", { numberOfInputs: 0, outputChannelCount: [2] });
    node.connect(ctx.destination);
    node.port.postMessage({ mem: sab, ptr: 0, frames: FRAMES });
    // the head: 4000 frames ahead of the output, moving with it; it stops at 0.5 s (an
    // underrun once the lag is used up) and moves on at 0.75 s (the sink, far behind, skips)
    const LAG = 4000;
    Atomics.store(head, 0, LAG);
    const at = (t) => ctx.suspend(t).then(() => {
      const moved = t < 0.5 || t >= 0.75;
      if (moved) Atomics.store(head, 0, Math.round(t * RATE) + LAG);
      void ctx.resume();
    });
    for (let k = 1; k < 100; k++) {
      const t = (k * 128 * 3) / RATE;
      if (t < 0.99) void at(t);
    }
    const out = await ctx.startRendering();
    return { l: Array.from(out.getChannelData(0)), r: Array.from(out.getChannelData(1)) };
  });
  assert.deepEqual(r.l, r.r, "both channels the same");
  // the largest step between neighbours where the signal goes to or from silence
  let silences = 0;
  let worstEdge = 0;
  for (let i = 1; i < r.l.length; i++) {
    const a = r.l[i - 1],
      b = r.l[i];
    if ((a === 0) !== (b === 0)) {
      silences++;
      worstEdge = Math.max(worstEdge, Math.abs(a - b));
    }
  }
  // (into the underrun's silence and out of it; the first block may come before the ring)
  assert.ok(silences >= 2, `edges into or out of silence: ${silences}`);
  // (a fade of up to 32 frames: what there was before the silence, when fewer, fades over them;
  // a cut is a step of 0.9)
  assert.ok(worstEdge < 0.1, `a cut into or out of silence: ${worstEdge}`);
  // fed: the ring's own samples (nonzero, full scale) for the first block after the fade in
  const full = r.l.filter((x) => Math.abs(Math.abs(x) - 0.9) < 1e-6).length;
  assert.ok(full > r.l.length / 4, `played the ring: ${full} full-scale samples`);
  console.log(`audio-sink-browser: ok (${silences} edges into or out of silence, worst step ${worstEdge.toFixed(4)})`);
} finally {
  await browser.close();
}
