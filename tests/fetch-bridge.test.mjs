// Written before migrating vnfs.js. Exercise the real worker protocol with reversed replies.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Worker } from 'node:worker_threads';
import { readFile } from 'node:fs/promises';

const source = await readFile(new URL('../host/fetch-bridge.js', import.meta.url), 'utf8');
const harness = `
const {parentPort, workerData, Worker: NodeWorker} = require('node:worker_threads');
globalThis.Worker = class {
  constructor() {
    this.w = new NodeWorker(workerData.source + '\\n' + \`
      const {parentPort} = require('node:worker_threads');
      IPPAI_STALL_MS = 200;
      IPPAI_GIVE_UP_MS = 1500;
      const tries = new Map();
      const body = (length) => Uint8Array.from({length}, (_, i) => i % 251).buffer;
      globalThis.fetch = async (url, init) => {
        const n = (tries.get(url) ?? 0) + 1;
        tries.set(url, n);
        await new Promise(r => setTimeout(r, url === 'slow' ? 30 : 1));
        // a link that drops twice, a server that is restarting, then both fine
        if (url === 'fail' || (url === 'flaky' && n <= 2)) throw new TypeError('Failed to fetch');
        if (url === 'restart' && n <= 2) return {ok: false, status: 503};
        // a body that stops halfway the first time (the stall watch aborts it)
        if (url === 'stall' && n === 1) return {ok: true, status: 200, body: {getReader: () => ({
          read: () => new Promise((_, reject) => init.signal.addEventListener('abort', () => reject(init.signal.reason))),
        })}};
        return {ok: url !== '404', status: 404, arrayBuffer: async () => body(url === 'empty' ? 0 : url === 'big' ? 700001 : url === 'exact' ? 262144 * 2 : 150000)};
      };
      parentPort.on('message', data => ippaiFetchWorker({data}));
    \`, {eval:true});
  }
  postMessage(m) { this.w.postMessage(m); }
  terminate() { this.w.terminate(); }
};
globalThis.Blob = class {};
globalThis.URL = {createObjectURL: () => '', revokeObjectURL: () => {}};
eval(workerData.source);
try { const all = workerData.url.split('|').map((u) => Array.from(ippaiFetchSync(u))); parentPort.postMessage({data: all.length === 1 ? all[0] : all}); }
catch(e) { parentPort.postMessage({error: e.message}); }
finally { ippaiFetchDispose(); }
`;

function request(url) {
  return new Promise((resolve, reject) => {
    const worker = new Worker(harness, {eval: true, workerData: {url, source}});
    worker.once('message', resolve);
    worker.once('error', reject);
  });
}

test('parallel reverse-order requests preserve every byte across buffer boundaries', async () => {
  const [slow, fast] = await Promise.all([request('slow'), request('fast')]);
  const expected = Array.from({length:150000}, (_, i) => i % 251);
  assert.deepEqual(slow.data, expected);
  assert.deepEqual(fast.data, expected);
});
test('bodies over several transfer buffers (and exactly two) arrive whole, one after another on a thread', async () => {
  for (const [url, n] of [['big', 700001], ['exact', 524288], ['fast', 150000]]) {
    const r = await request(url);
    assert.equal(r.data.length, n, url);
    assert.ok(r.data.every((b, i) => b === i % 251), url);
  }
  // (several reads on one thread share its transfer buffer)
  const seq = await request('big|empty|fast|exact|big');
  assert.deepEqual(seq.data.map((d) => d.length), [700001, 0, 150000, 524288, 700001]);
  assert.ok(seq.data.every((d) => d.every((b, i) => b === i % 251)));
});
test('empty response completes', async () => assert.deepEqual((await request('empty')).data, []));
test('network and HTTP failures unblock the caller', async () => {
  assert.ok((await request('fail')).error);
  assert.ok((await request('404')).error);
});
test('dropped links, 503s and stalled bodies are retried until the bytes arrive', async () => {
  const expected = Array.from({length:150000}, (_, i) => i % 251);
  for (const url of ['flaky', 'restart', 'stall']) assert.deepEqual((await request(url)).data, expected, url);
});
test('a 404 fails at once, without retries', async () => {
  const t = Date.now();
  assert.ok((await request('404')).error);
  assert.ok(Date.now() - t < 1000);
});
