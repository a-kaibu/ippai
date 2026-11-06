// The page owns a network worker and gives each game pthread a MessagePort.
// The network worker never relies on a blocked pthread's event loop. A bounded shared buffer transfers one response, with explicit acknowledgement.
//
// Mobile links drop, stall and come back: a request is not given up on the first failure. The
// worker retries network errors, stalled bodies (no byte for IPPAI_STALL_MS) and 5xx/408/429
// with backoff, waits while the browser says it is offline, and only gives up after
// IPPAI_GIVE_UP_MS (a 404 fails at once). While it retries it wakes the waiting pthread every
// few seconds, so the pthread's own timeout only catches a dead worker. Store chunks are named by
// their content: they are kept in Cache Storage (IPPAI_CHUNKS, web/src/net.ts uses the same one),
// so a second play, or a game saved to the device beforehand, needs no network for them.
// Retries are announced on the BroadcastChannel "ippai-net" (the page shows that it waits).
var IPPAI_CHUNKS = 'ippai-chunks-v1';
var IPPAI_STALL_MS = 20000;
var IPPAI_GIVE_UP_MS = 15 * 60000;
var IPPAI_RETRY_STATUS = [0, 408, 425, 429, 500, 502, 503, 504];

/** A store chunk: content-addressed, the same bytes forever. */
function ippaiIsChunk(url) {
  return /\/chunks\/[0-9a-f]{2}\/[0-9a-f]{32}$/.test(url);
}

/** The delay before retry `attempt` (from 0): exponential with jitter, at most 8 s. */
function ippaiBackoff(attempt) {
  return Math.min(8000, 300 * 2 ** attempt) * (0.5 + Math.random() * 0.5);
}

/** Waits `ms`; while the browser reports no network, until it reports one again (at most 30 s). */
async function ippaiPause(ms) {
  await new Promise((r) => setTimeout(r, ms));
  // (polled: a worker's 'online' event is not dependable)
  for (let i = 0; i < 60 && globalThis.navigator?.onLine === false; i++) await new Promise((r) => setTimeout(r, 500));
}

/** One try: the body read in pieces; aborted when no byte arrives for `stallMs`. */
async function ippaiFetchOnce(url, stallMs) {
  const abort = new AbortController();
  let last = Date.now();
  const watch = setInterval(() => {
    if (Date.now() - last > stallMs) abort.abort(new Error('stalled'));
  }, Math.min(1000, stallMs / 4));
  try {
    const response = await fetch(url, { signal: abort.signal });
    last = Date.now();
    if (!response.ok) {
      response.body?.cancel?.().catch?.(() => {});
      return { response };
    }
    if (!response.body?.getReader) return { response, data: new Uint8Array(await response.arrayBuffer()) };
    const reader = response.body.getReader();
    const parts = [];
    let length = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      last = Date.now();
      parts.push(value);
      length += value.length;
    }
    const data = new Uint8Array(length);
    let at = 0;
    for (const p of parts) {
      data.set(p, at);
      at += p.length;
    }
    return { response, data };
  } finally {
    clearInterval(watch);
  }
}

var ippaiNet = { retrying: new Map(), channel: undefined, id: Math.random().toString(36).slice(2) };
/** Tells the page how many requests are being retried now (and the last error). */
function ippaiNetReport(error) {
  try {
    ippaiNet.channel ??= new BroadcastChannel('ippai-net');
    ippaiNet.channel.unref?.();
    ippaiNet.channel.postMessage({ id: ippaiNet.id, retrying: ippaiNet.retrying.size, error: String(error ?? '') });
  } catch (_) {}
}

async function ippaiCache() {
  try {
    return typeof caches === 'undefined' ? null : await caches.open(IPPAI_CHUNKS);
  } catch (_) {
    return null;
  }
}

/**
 * `url`'s bytes, retried as above: { data, wire (bytes over the network), cached (1: no
 * network) }. Throws for a final failure.
 */
async function ippaiFetchRobust(url, opt = {}) {
  const stallMs = opt.stallMs ?? IPPAI_STALL_MS;
  const giveUpMs = opt.giveUpMs ?? IPPAI_GIVE_UP_MS;
  const chunk = ippaiIsChunk(url);
  const cache = chunk ? await ippaiCache() : null;
  const hit = await cache?.match(url).catch(() => undefined);
  if (hit) {
    try {
      return { data: new Uint8Array(await hit.arrayBuffer()), wire: 0, cached: 1 };
    } catch (_) {}
  }
  const start = Date.now();
  const key = {};
  let after = 0;
  try {
    for (let attempt = 0; ; attempt++) {
      let error;
      try {
        const { response, data } = await ippaiFetchOnce(url, stallMs);
        if (data) {
          const timing = typeof performance !== 'undefined' && performance.getEntriesByName?.(url).at(-1);
          const wire = timing ? timing.transferSize : +(response.headers?.get?.('content-length') || data.length);
          const cached = timing && timing.transferSize === 0 && timing.decodedBodySize > 0 ? 1 : 0;
          if (cache) cache.put(url, new Response(data)).catch(() => {});
          return { data, wire, cached };
        }
        if (!IPPAI_RETRY_STATUS.includes(response.status)) throw Object.assign(new Error(`HTTP ${response.status}`), { final: true });
        error = `HTTP ${response.status}`;
        // (seconds; a date form counts as none)
        after = Math.min((+(response.headers?.get?.('retry-after')) || 0) * 1000, 30000);
      } catch (e) {
        if (e.final) throw e;
        error = e?.message ?? e;
      }
      if (Date.now() - start > giveUpMs) throw new Error(`${error} (gave up)`);
      ippaiNet.retrying.set(key, attempt);
      ippaiNetReport(error);
      await ippaiPause(after || ippaiBackoff(attempt));
      after = 0;
    }
  } finally {
    if (ippaiNet.retrying.delete(key)) ippaiNetReport('');
  }
}

/**
 * The transfer buffer a synchronous read and the network worker share: a header (status,
 * part length, wire bytes, from the HTTP cache, body length) and IPPAI_BRIDGE_BYTES of body
 * at a time, one per thread. A/B (node workers, 400 reads, median of 5): a 256 KiB chunk took
 * 0.369 ms with 64 KiB parts copied twice (the old protocol), 0.342 ms copied once, 0.214 ms
 * in one 256 KiB part, 0.216 ms with 1 MiB; a 64 KiB read 0.135-0.139 ms with any. Chunks
 * are 256 KiB at most.
 */
var IPPAI_BRIDGE_HEADER = 32;
var IPPAI_BRIDGE_BYTES = 262144;

function ippaiFetchWorker(event) {
  const { url, shared } = event.data;
  const state = new Int32Array(shared, 0, 8);
  const output = new Uint8Array(shared, IPPAI_BRIDGE_HEADER);
  const publish = (status) => {
    Atomics.store(state, 0, status);
    Atomics.notify(state, 0);
  };
  // (the pthread waits with a timeout: a wake-up without news tells it this worker is alive)
  const alive = setInterval(() => Atomics.notify(state, 0), 5000);
  (async () => {
    try {
      const { data, wire, cached } = await ippaiFetchRobust(url);
      Atomics.store(state, 2, wire);
      Atomics.store(state, 3, cached);
      Atomics.store(state, 4, data.length);
      for (let offset = 0; offset < data.length; offset += output.length) {
        const part = data.subarray(offset, offset + output.length);
        output.set(part);
        Atomics.store(state, 1, part.length);
        publish(1);
        while (Atomics.load(state, 0) === 1) {
          const wait = Atomics.waitAsync(state, 0, 1, 65000);
          if ((wait.async ? await wait.value : wait.value) === 'timed-out') return;
        }
        if (Atomics.load(state, 0) === -1) return;
      }
      publish(2);
    } catch (_) {
      publish(-1);
    } finally {
      clearInterval(alive);
    }
  })();
}

/** The network worker's script: the functions above, `main` its message handler. */
function ippaiFetchWorkerSource(main) {
  const vars = { IPPAI_CHUNKS, IPPAI_STALL_MS, IPPAI_GIVE_UP_MS, IPPAI_RETRY_STATUS, IPPAI_BRIDGE_HEADER, IPPAI_BRIDGE_BYTES };
  const fns = [ippaiIsChunk, ippaiBackoff, ippaiPause, ippaiFetchOnce, ippaiNetReport, ippaiCache, ippaiFetchRobust, ippaiFetchWorker];
  return [
    ...Object.entries(vars).map(([k, v]) => `var ${k} = ${JSON.stringify(v)};`),
    'var ippaiNet = { retrying: new Map(), channel: undefined, id: Math.random().toString(36).slice(2) };',
    ...fns.map(String),
    main,
  ].join('\n');
}

var ippaiFetchWorkerInstance;
var ippaiFetchPort;
function ippaiFetchConnect() {
  if (!ippaiFetchWorkerInstance) {
    const blob = new Blob([ippaiFetchWorkerSource('self.onmessage = e => { e.data.port.onmessage = ippaiFetchWorker; };')], {type:'text/javascript'});
    const url = URL.createObjectURL(blob);
    ippaiFetchWorkerInstance = new Worker(url);
    URL.revokeObjectURL(url);
  }
  const channel = new MessageChannel();
  ippaiFetchWorkerInstance.postMessage({port:channel.port1}, [channel.port1]);
  return channel.port2;
}
function ippaiFetchDispose() {
  ippaiFetchWorkerInstance?.terminate();
  ippaiFetchWorkerInstance = undefined;
}

var ippaiBridgeBuffer;
function ippaiFetchSync(url) {
  if (typeof document !== 'undefined') throw new Error('synchronous reads on the page must be preloaded');
  if (!ippaiFetchPort && !ippaiFetchWorkerInstance) {
    const blob = new Blob([ippaiFetchWorkerSource('self.onmessage = ippaiFetchWorker;')], {type: 'text/javascript'});
    const location = URL.createObjectURL(blob);
    ippaiFetchWorkerInstance = new Worker(location);
    URL.revokeObjectURL(location);
  }
  // (this thread's buffer, kept: one read at a time per thread; a new one after a worker
  // that timed out, which may still write into the old)
  const shared = (ippaiBridgeBuffer ??= new SharedArrayBuffer(IPPAI_BRIDGE_HEADER + IPPAI_BRIDGE_BYTES));
  const state = new Int32Array(shared, 0, 8);
  const bytes = new Uint8Array(shared, IPPAI_BRIDGE_HEADER);
  Atomics.store(state, 0, 0);
  let result;
  let length = 0;
  (ippaiFetchPort || ippaiFetchWorkerInstance).postMessage({url, shared});
  for (;;) {
    while (Atomics.load(state, 0) === 0) {
      if (Atomics.wait(state, 0, 0, 65000) === 'timed-out') {
        Atomics.store(state, 0, -1);
        Atomics.notify(state, 0);
        ippaiBridgeBuffer = undefined;
        ippaiFetchDispose();
        throw new Error('fetch worker timed out');
      }
    }
    const status = Atomics.load(state, 0);
    if (status === -1) throw new Error(`fetch failed: ${url}`);
    if (status === 2) break;
    // (copied once, into the body's place: the length came first)
    result ??= new Uint8Array(Atomics.load(state, 4));
    const n = Atomics.load(state, 1);
    result.set(bytes.subarray(0, n), length);
    length += n;
    Atomics.store(state, 0, 0);
    Atomics.notify(state, 0);
  }
  result ??= new Uint8Array(0);
  result.wire = Atomics.load(state, 2) >>> 0;
  result.cached = Atomics.load(state, 3);
  return result;
}
