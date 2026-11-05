// The page owns a network worker and gives each game pthread a MessagePort.
// The network worker never relies on a blocked pthread's event loop. A bounded shared buffer transfers one response, with explicit acknowledgement.
function ippaiFetchWorker(event) {
  const { url, shared } = event.data;
  const state = new Int32Array(shared, 0, 4);
  const output = new Uint8Array(shared, 16);
  const publish = (status) => {
    Atomics.store(state, 0, status);
    Atomics.notify(state, 0);
  };
  (async () => {
    try {
      const response = await fetch(url, { signal: AbortSignal.timeout(60000) });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = new Uint8Array(await response.arrayBuffer());
      const timing = typeof performance !== 'undefined' && performance.getEntriesByName?.(url).at(-1);
      Atomics.store(state, 2, timing ? timing.transferSize : +(response.headers?.get('content-length') || 0));
      Atomics.store(state, 3, timing && timing.transferSize === 0 && timing.decodedBodySize > 0 ? 1 : 0);
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
    }
  })();
}

var ippaiFetchWorkerInstance;
var ippaiFetchPort;
function ippaiFetchConnect() {
  if (!ippaiFetchWorkerInstance) {
    const blob = new Blob([`const receive = ${ippaiFetchWorker.toString()}; self.onmessage = e => { e.data.port.onmessage = receive; };`], {type:'text/javascript'});
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

function ippaiFetchSync(url) {
  if (typeof document !== 'undefined') throw new Error('synchronous reads on the page must be preloaded');
  if (!ippaiFetchPort && !ippaiFetchWorkerInstance) {
    const blob = new Blob([`self.onmessage = ${ippaiFetchWorker.toString()}`], {type: 'text/javascript'});
    const location = URL.createObjectURL(blob);
    ippaiFetchWorkerInstance = new Worker(location);
    URL.revokeObjectURL(location);
  }
  const shared = new SharedArrayBuffer(16 + 65536);
  const state = new Int32Array(shared, 0, 4);
  const bytes = new Uint8Array(shared, 16);
  const parts = [];
  let length = 0;
  (ippaiFetchPort || ippaiFetchWorkerInstance).postMessage({url, shared});
  for (;;) {
    while (Atomics.load(state, 0) === 0) {
      if (Atomics.wait(state, 0, 0, 65000) === 'timed-out') {
        Atomics.store(state, 0, -1);
        Atomics.notify(state, 0);
        ippaiFetchDispose();
        throw new Error('fetch worker timed out');
      }
    }
    const status = Atomics.load(state, 0);
    if (status === -1) throw new Error(`fetch failed: ${url}`);
    if (status === 2) break;
    const part = bytes.slice(0, Atomics.load(state, 1));
    parts.push(part);
    length += part.length;
    Atomics.store(state, 0, 0);
    Atomics.notify(state, 0);
  }
  const result = new Uint8Array(length);
  let offset = 0;
  for (const part of parts) { result.set(part, offset); offset += part.length; }
  result.wire = Atomics.load(state, 2) >>> 0;
  result.cached = Atomics.load(state, 3);
  return result;
}
