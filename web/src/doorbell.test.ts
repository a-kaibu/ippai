import { expect, test } from "vite-plus/test";
import { NOTED_ERRORS, watchDoorbell } from "./doorbell";

// The doorbell loop of adec.ts, idec.ts and video.ts: a tick that throws does not end it, and
// stop() does.

function bell() {
  const buffer = new SharedArrayBuffer(64);
  const i32 = new Int32Array(buffer);
  return {
    buffer,
    ring: () => {
      Atomics.add(i32, 2, 1);
      Atomics.notify(i32, 2);
    },
  };
}
const settle = () => new Promise((r) => setTimeout(r, 20));

test("a tick that throws is noted and the watching goes on", async () => {
  const b = bell();
  const notes: string[] = [];
  let ticks = 0;
  let stopped = false;
  const done = watchDoorbell(
    () => b.buffer,
    8,
    () => {
      ticks++;
      if (ticks % 2) throw new RangeError(`bad pointer ${ticks}`);
    },
    (l) => notes.push(l),
    () => stopped,
    "adec",
    50,
  );
  for (let i = 0; i < 2 * NOTED_ERRORS + 4; i++) {
    b.ring();
    await settle();
  }
  expect(ticks).toBeGreaterThanOrEqual(2 * NOTED_ERRORS + 4);
  // (the first few noted, the rest only counted)
  expect(notes.length).toBe(NOTED_ERRORS);
  expect(notes[0]).toMatch(/^adec: RangeError: bad pointer 1/);
  expect(notes.at(-1)).toMatch(/no more of these noted/);
  stopped = true;
  b.ring();
  await done;
});

test("stopped, the loop ends without another tick", async () => {
  const b = bell();
  let ticks = 0;
  let stopped = false;
  const done = watchDoorbell(
    () => b.buffer,
    8,
    () => void ticks++,
    () => {},
    () => stopped,
    "idec",
    30,
  );
  b.ring();
  await settle();
  const before = ticks;
  stopped = true;
  // (it ends at its next wake, a ring or the timeout, and does not tick then)
  await done;
  expect(ticks).toBe(before);
});
