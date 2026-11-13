// Written before migrating vnfs.js. Exercise the real worker protocol with reversed replies.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Worker } from 'node:worker_threads';
import { readFile } from 'node:fs/promises';

const source = await readFile(new URL('../host/fetch-bridge.js', import.meta.url), 'utf8');
// chunk names of i % 251 inputs from the blake3 crate (crates/ippai/src/pack.rs makes the file)
const vectors = (await readFile(new URL('../testdata/blake3.tsv', import.meta.url), 'utf8')).trim().split('\n').slice(1).map((l) => l.split('\t'));
const names = Object.fromEntries(vectors.filter((v) => v[0] === 'p').map(([, n, h]) => [h, +n]));
/** The inputs testdata/blake3.tsv names: `p` i % 251, `r` an LCG seeded with the length. */
const input = (kind, n) => {
  let x = n >>> 0;
  return Uint8Array.from({length: n}, (_, i) => {
    if (kind === 'p') return i % 251;
    x = (Math.imul(x, 1103515245) + 12345) >>> 0;
    return (x >>> 16) & 255;
  });
};
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
      const NAMES = ${JSON.stringify(names)};
      // Cache Storage as the browser's: bodies kept, Blob.slice reading only its bytes (counted)
      IPPAI_RECENT_BYTES = 1;
      const kept = new Map();
      let fetches = 0, slices = 0;
      globalThis.caches = {open: async () => ({
        match: async (u) => {
          const d = kept.get(u);
          return d && {headers: {get: (k) => d.h[k] ?? null}, arrayBuffer: async () => d.b.slice().buffer,
            blob: async () => ({slice: (a, b) => ({arrayBuffer: async () => (slices++, d.b.slice(a, b).buffer)})})};
        },
        put: async (u, r) => void kept.set(u, {b: new Uint8Array(await r.arrayBuffer()), h: Object.fromEntries(r.headers)}),
        delete: async (u) => kept.delete(u),
      })};
      globalThis.fetch = async (url, init) => {
        const n = (tries.get(url) ?? 0) + 1;
        tries.set(url, n);
        // (how many chunk fetches and Cache Storage slices so far, as a body's length)
        if (url === 'count' || url === 'slices') return {ok: true, status: 200, arrayBuffer: async () => new ArrayBuffer(url === 'count' ? fetches : slices)};
        // (a server without S3: no chunk from the bucket itself; tests/direct-browser.mjs has one)
        if (url.includes('/store-direct/')) return {ok: false, status: 501};
        if (url.includes('/chunks/')) fetches++;
        // store chunks: the bytes their name says (the 262144-byte one damaged the first time,
        // and then only past the HTTP cache), any other name 1000 bytes that never match
        const chunk = /[/]chunks[/][0-9a-f]{2}[/]([0-9a-f]{32})$/.exec(url);
        if (chunk) {
          const length = NAMES[chunk[1]] ?? 1000;
          const data = new Uint8Array(body(length));
          // ranges of the decoded chunk (ippai serve's store_api.rs), but from "old" (a server
          // without them: the whole chunk); "bad" answers the wrong place the first time
          const range = /^bytes=([0-9]+)-([0-9]+)$/.exec(init?.headers?.range ?? '');
          if (range && !url.startsWith('https://old/')) {
            const a = +range[1];
            if (a >= length) return {ok: false, status: 416, headers: {get: () => null}};
            const b = Math.min(+range[2], length - 1);
            const off = url.startsWith('https://bad/') && n === 1 ? 1 : 0;
            return {ok: true, status: 206, headers: {get: (k) => (k === 'content-range' ? 'bytes ' + (a + off) + '-' + (b + off) + '/' + length : null)},
              arrayBuffer: async () => data.slice(a, b + 1).buffer};
          }
          if (length === 262144 && (n === 1 || init?.cache !== 'reload')) data[1000] ^= 1;
          return {ok: true, status: 200, arrayBuffer: async () => data.buffer};
        }
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
try { const all = workerData.url.split('|').map((u) => Array.from(ippaiFetchSync(...ippaiRange(u)))); parentPort.postMessage({data: all.length === 1 ? all[0] : all}); }
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

test('BLAKE3 chunk names as the blake3 crate makes them', () => {
  const ippaiBlake3 = new Function(`${source}; return ippaiBlake3;`)();
  for (const [kind, n, h] of vectors) assert.equal(ippaiBlake3(input(kind, +n)), h, `${kind} ${n}`);
});
test('a chunk is checked against its name: fetched again when damaged, a failure when it never matches', async () => {
  const name = (n) => vectors.find((v) => v[0] === 'p' && +v[1] === n)[2];
  const good = await request(`https://s/store/chunks/ab/${name(200000)}`);
  assert.equal(good.data.length, 200000);
  const once = await request(`https://s/store/chunks/ab/${name(262144)}`);
  assert.equal(once.data.length, 262144);
  assert.ok(once.data.every((b, i) => b === i % 251), 'the second, undamaged copy');
  const never = await request('https://s/store/chunks/ab/00000000000000000000000000000000');
  assert.ok(never.error, 'a chunk that never matches its name fails');
});
const nameOf = (n) => vectors.find((v) => v[0] === 'p' && +v[1] === n)[2];
const want = (from, len, n) => Array.from({length: Math.max(0, Math.min(from + len, n) - from)}, (_, i) => (from + i) % 251);
test('block ranges of a chunk: only they cross the network, kept as blocks; a whole chunk kept is read by slices', async () => {
  const c = `https://s/store/chunks/ab/${nameOf(262143)}`;
  const B = 65536;
  const r = await request([`${c}#0+100/${B}`, `${c}#65536+70000/${B}`, `${c}#196608+65535/${B}`, 'count', 'slices',
    `${c}#65536+65536/${B}`, `${c}#0+262143/${B}`, 'count', c, 'count', `${c}#7+262136/${B}`, `${c}#300000+5/${B}`, 'count', 'slices'].join('|'));
  assert.ok(!r.error, r.error);
  const [a, b, e, n1, s1, f, g, n2, whole, n3, h, past, n4, s4] = r.data;
  assert.deepEqual(a, want(0, 100, 262143));
  assert.deepEqual(b, want(65536, 70000, 262143));
  // (the chunk's last block, short)
  assert.deepEqual(e, want(196608, 65535, 262143));
  assert.equal(n1.length, 3, 'a request per range');
  assert.equal(s1.length, 0);
  // (blocks 1 and 0..3 kept alone: no network)
  assert.deepEqual(f, want(65536, 65536, 262143));
  assert.deepEqual(g, want(0, 262143, 262143));
  assert.equal(n2.length, 3);
  // the whole chunk (the page's): fetched and kept; its ranges then by slices
  assert.deepEqual(whole, want(0, 262143, 262143));
  assert.equal(n3.length, 4);
  assert.deepEqual(h, want(7, 262136, 262143));
  assert.deepEqual(past, []);
  assert.equal(n4.length, 4);
});
test('a server without ranges: the whole chunk once (checked, kept), its other blocks from it', async () => {
  const c = `https://old/store/chunks/ab/${nameOf(200000)}`;
  const d = `https://old/store/chunks/ab/${nameOf(262143)}`;
  const r = await request([`${c}#0+100/65536`, `${c}#100000+70000/65536`, `${c}#196608+65536/65536`, 'count', `${d}#5+10/65536`, `${c}#7+1000/65536`, 'count', 'slices'].join('|'));
  assert.ok(!r.error, r.error);
  const [a, b, e, n1, f, g, n2, s2] = r.data;
  assert.deepEqual(a, want(0, 100, 200000));
  assert.deepEqual(b, want(100000, 70000, 200000));
  assert.deepEqual(e, want(196608, 65536, 200000));
  assert.equal(n1.length, 1, 'one fetch for three ranges');
  assert.deepEqual(f, want(5, 10, 262143));
  // (the other chunk pushed this one out of the worker's memory: Cache Storage by a slice)
  assert.deepEqual(g, want(7, 1000, 200000));
  assert.equal(n2.length, 2);
  assert.equal(s2.length, 1);
});
test('a range answered for another place is asked again', async () => {
  const c = `https://bad/store/chunks/ab/${nameOf(200000)}`;
  const r = await request([`${c}#65536+65536/65536`, 'count'].join('|'));
  assert.ok(!r.error, r.error);
  assert.deepEqual(r.data[0], want(65536, 65536, 200000));
  assert.equal(r.data[1].length, 2);
});
test('ranges parse only at the end of a name', () => {
  const ippaiRange = new Function(`${source}; return ippaiRange;`)();
  // (the last: the request's class, 0 the game's, 1 a fetcher thread's "!a")
  assert.deepEqual(ippaiRange('chunks/ab/x#0+5'), ['chunks/ab/x', 0, 5, undefined, 0]);
  assert.deepEqual(ippaiRange('chunks/ab/x#262144+5/262144'), ['chunks/ab/x', 262144, 5, 262144, 0]);
  assert.deepEqual(ippaiRange('chunks/ab/x#262144+5/262144!a'), ['chunks/ab/x', 262144, 5, 262144, 1]);
  assert.deepEqual(ippaiRange('chunks/ab/x!a'), ['chunks/ab/x!a']);
  assert.deepEqual(ippaiRange('trees/a#b.tsv'), ['trees/a#b.tsv']);
  assert.deepEqual(ippaiRange('chunks/ab/x#12+'), ['chunks/ab/x#12+']);
});
