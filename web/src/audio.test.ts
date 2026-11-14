import { expect, test } from "vite-plus/test";
import { sinkStep, type SinkState } from "./audio";

const MAX = 8820,
  TARGET = 2205,
  FADE = 32,
  FRAMES = 1 << 14;

/**
 * The worklet's block step over random heads (a steady producer, stalls, bursts that pass the
 * lag limit): with the ring keeping up and no jump it plays the ring's samples exactly; where
 * the sound stops (an underrun) the last sample before the silence and the first after it are
 * faded to at most 1/(FADE+1) of the signal (no click), as at a skip ahead; it never reads past
 * the head; output is never louder than the ring (property, seeded).
 */
test("sink: exact while fed, faded at underruns and skips, never past the head (property, seeded)", () => {
  let x = 4;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 60; run++) {
    const d = new Float32Array(FRAMES * 2);
    // (a full-scale square-ish signal: the worst case for a click)
    for (let i = 0; i < FRAMES; i++) d[2 * i] = d[2 * i + 1] = i & 64 ? 1 : -1;
    const st: SinkState = { read: -1, fadeIn: false };
    let head = TARGET + rnd(1000);
    let lastOut = 0;
    let wasSilent = false;
    for (let block = 0; block < 300; block++) {
      const step = rnd(10) === 0 ? 0 : rnd(20) === 0 ? 20000 : 64 + rnd(128);
      head = (head + step) >>> 0;
      const l = new Float32Array(128).fill(NaN),
        r = new Float32Array(128).fill(NaN);
      const before = { ...st };
      sinkStep(st, head, d, FRAMES - 1, l, r, MAX, TARGET, FADE);
      let avail = (head - (before.read < 0 ? (head - TARGET) >>> 0 : before.read)) >>> 0;
      const skipped = avail > MAX;
      if (skipped) avail = TARGET;
      const n = Math.min(avail, 128);
      const at = `run ${run} block ${block}`;
      const bad = (why: string) => expect.fail(`${at}: ${why}`);
      if ((head - st.read) >>> 0 > MAX) bad("past the head");
      for (let i = 0; i < 128; i++)
        if (Number.isNaN(l[i]!) || Math.abs(l[i]!) > 1 || l[i] !== r[i]) bad(`sample ${i} ${l[i]} ${r[i]}`);
      const start = skipped ? (head - TARGET) >>> 0 : before.read < 0 ? (head - TARGET) >>> 0 : before.read;
      const plain = !before.fadeIn && !skipped && n === 128;
      if (plain)
        for (let i = 0; i < 128; i++)
          if (l[i] !== d[((start + i) & (FRAMES - 1)) * 2]) bad(`not the ring's at ${i}`);
      const edge = 1 / (Math.min(FADE, n) + 1) + 1e-6;
      if (n < 128) {
        for (let i = n; i < 128; i++) if (l[i] !== 0) bad(`not silent at ${i}`);
        if (n && Math.abs(l[n - 1]!) > edge) bad(`a cut before the silence: ${l[n - 1]}`);
      }
      if ((wasSilent || skipped) && n && Math.abs(l[0]!) > edge) bad(`a cut after a stop or jump: ${l[0]}`);
      wasSilent = n < 128;
      lastOut = l[127]!;
    }
    void lastOut;
  }
});

test("the worklet's text runs sinkStep as written (no helper it lacks)", () => {
  // oxlint-disable-next-line typescript/no-implied-eval -- (the worklet runs this text)
  const f = new Function(`return (${sinkStep.toString()})`)() as typeof sinkStep;
  const d = new Float32Array(64).map((_, i) => i);
  const st: SinkState = { read: 0, fadeIn: false };
  const l = new Float32Array(4),
    r = new Float32Array(4);
  f(st, 10, d, 31, l, r, 100, 5, 2);
  expect(Array.from(l)).toEqual([0, 2, 4, 6]);
  expect(st.read).toBe(4);
});
