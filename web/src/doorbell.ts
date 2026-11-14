// The guest's doorbells (a word kura changes when it wants the page to look: adec.ts, idec.ts,
// video.ts): the page waits on it with Atomics.waitAsync (or a second at most) and looks.

type WaitAsync = (
  a: Int32Array,
  i: number,
  v: number,
  t: number,
) => { async: boolean; value: Promise<string> | string };

/** Errors of one watcher written to the log, at most (the rest are only counted). */
export const NOTED_ERRORS = 5;

/**
 * Calls `tick` each time the word at byte `at` of `memory()` changes, or each `ms`, until
 * `stopped()`. A throw from `tick` is noted (the first NOTED_ERRORS) and the watching goes on:
 * one bad stream (a RangeError on a bad pointer, a decoder that went to "closed") used to end
 * the loop for good, and every later stream went unanswered with the guest waiting on it.
 * `stopped`: the process ended (play.ts finish), else its whole memory stayed reachable from
 * here and a dead runtime was called once a second.
 */
export async function watchDoorbell(
  memory: () => ArrayBufferLike,
  at: number,
  tick: () => void,
  note: (line: string) => void,
  stopped: () => boolean,
  what: string,
  ms = 1000,
): Promise<void> {
  const i32 = () => new Int32Array(memory());
  let errors = 0;
  let seen = Atomics.load(i32(), at >>> 2);
  while (!stopped()) {
    // (Atomics.waitAsync: ES2024, in Chrome since 87)
    const w = (Atomics as unknown as { waitAsync: WaitAsync }).waitAsync(i32(), at >>> 2, seen, ms);
    if (w.async) await w.value;
    if (stopped()) return;
    seen = Atomics.load(i32(), at >>> 2);
    try {
      tick();
    } catch (e) {
      if (++errors <= NOTED_ERRORS)
        note(`${what}: ${String(e)}${errors === NOTED_ERRORS ? " (no more of these noted)" : ""}`);
    }
  }
}
