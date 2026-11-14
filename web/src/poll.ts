// A task run again and again, a pause after each run ends (not setInterval: a run that takes
// longer than the pause does not pile up behind itself, as the Storage tab's did on a phone with
// a large chunk cache: every 5 s another full listing while the last ones were still going).

/** Runs `f` now and `ms` after each run ends, until the returned stop is called; a run that
 * throws is passed to `failed` and the next one comes all the same. */
export function every(ms: number, f: () => unknown, failed: (e: unknown) => void = () => {}): () => void {
  let stopped = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const run = async () => {
    try {
      await f();
    } catch (e) {
      failed(e);
    }
    if (!stopped) timer = setTimeout(() => void run(), ms);
  };
  void run();
  return () => {
    stopped = true;
    clearTimeout(timer);
  };
}

/** `f` called at most once every `ms` (the last call's wish kept: one more call after the pause). */
export function throttled(ms: number, f: () => void): { (): void; cancel(): void } {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let last = -Infinity;
  const call = () => {
    const wait = last + ms - Date.now();
    if (wait <= 0) {
      clearTimeout(timer);
      timer = undefined;
      last = Date.now();
      f();
    } else
      timer ??= setTimeout(() => {
        timer = undefined;
        call();
      }, wait);
  };
  call.cancel = () => clearTimeout(timer);
  return call;
}

/** `f`'s answer shared: callers within `ms` of the last answer (or while one is coming) get
 * that one; `fresh` asks anew. */
export function shared<T>(ms: number, f: () => Promise<T>): (fresh?: boolean) => Promise<T> {
  let p: Promise<T> | undefined;
  let at = -Infinity;
  let pending = false;
  return (fresh = false) => {
    if (p && !fresh && (pending || Date.now() - at < ms)) return p;
    pending = true;
    const q = f().finally(() => {
      if (p === q) {
        pending = false;
        at = Date.now();
      }
    });
    // (a failure is not kept)
    q.catch(() => p === q && (p = undefined));
    return (p = q);
  };
}
