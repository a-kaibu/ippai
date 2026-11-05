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
      globalThis.fetch = async url => {
        await new Promise(r => setTimeout(r, url === 'slow' ? 30 : 1));
        if (url === 'fail') throw new Error('offline');
        return {ok: url !== '404', status: 404, arrayBuffer: async () =>
          Uint8Array.from({length: url === 'empty' ? 0 : 150000}, (_, i) => i % 251).buffer};
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
try { parentPort.postMessage({data: Array.from(ippaiFetchSync(workerData.url))}); }
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
test('empty response completes', async () => assert.deepEqual((await request('empty')).data, []));
test('network and HTTP failures unblock the caller', async () => {
  assert.ok((await request('fail')).error);
  assert.ok((await request('404')).error);
});
