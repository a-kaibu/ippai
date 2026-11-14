// Real Chromium WebGPU: host/vngpu.js when the drawing thread's device is lost (P0-1 of the
// 2026-11-14 review). In a worker holding an OffscreenCanvas, as the drawing thread does, with
// the wasm side stubbed: device.destroy() resolves `device.lost`; the frames stop, render/mod.rs
// is told (vnhost_render_losing), a new device is had and the canvas configured for it, then
// vnhost_render_lost (which empties the table, vngpu_reset) and the frames go on, drawing on the
// new device without WebGPU errors; the old device's late answers (readbacks, work done) do not
// reach the new one's tokens; twice in a row too; and with no adapter left, after the retries,
// vnhost_render_lost_for_good; a device lost again as soon as it came, twice in a row: given up
// too.
//   node tests/gpulost-browser.mjs          (PLAYWRIGHT as elsewhere)
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
const pw = await import(process.env.PLAYWRIGHT ?? 'playwright');
const { chromium } = pw.default ?? pw;
const source = await readFile(new URL('../host/vngpu.js', import.meta.url), 'utf8');

const worker = `
let lib;
const addToLibrary = (o) => (lib = o);
${source}
self.VG = lib.$VG;
VG.retryMs = 5;
const log = [];
let frames = 0;
const err = (m) => log.push(m);
const UTF8ToString = () => '';
const wasmMemory = new WebAssembly.Memory({ initial: 1 });
let vnvmCanvas;
const at = (name) => log.push(name + ' ' + frames);
const lateFromOld = [];
let oldDevice = null;
const wasmExports = {
  vnhost_render_frame: () => {
    frames++;
    // (a frame's drawing: a pass into a texture of the device; headless Chromium loses the
    // device that presents a canvas, so not into the canvas, as IPPAI_HEADLESS runs)
    const enc = VG.device.createCommandEncoder();
    if (VG.frameTex?.device !== VG.device) VG.frameTex = { device: VG.device, tex: VG.device.createTexture({ size: [4, 4], format: 'rgba8unorm', usage: GPUTextureUsage.RENDER_ATTACHMENT }) };
    const pass = enc.beginRenderPass({ colorAttachments: [{ view: VG.frameTex.tex.createView(), loadOp: 'clear', storeOp: 'store', clearValue: [0, 0.5, 1, 1] }] });
    pass.end();
    VG.device.queue.submit([enc.finish()]);
  },
  vnhost_render_losing: () => at('losing'),
  vnhost_render_lost: () => {
    at('lost');
    lib.vngpu_reset();
  },
  vnhost_render_lost_for_good: () => at('for good'),
  // (the old device's token 78 must not come once the new device is there)
  vnhost_render_done: (token) => token === 78 && VG.device !== oldDevice && lateFromOld.push('done ' + token),
  vnhost_render_mapped: (token, ok) => log.push('mapped ' + token + ' ' + ok),
  vnhost_gpu_time: () => {},
};
const wait = async (f, what, ms = 10000) => {
  const t = Date.now();
  while (!f()) {
    if (Date.now() - t > ms) throw new Error('timed out: ' + what + ' (' + log.join('; ') + ')');
    await new Promise((r) => setTimeout(r, 5));
  }
};
self.onmessage = async (e) => {
  try {
    vnvmCanvas = Promise.resolve(e.data.canvas);
    // ('once': each loss counts as one on its own)
    if (e.data.mode === 'once') VG.quickMs = 0;
    lib.vngpu_start();
    await wait(() => VG.device && frames > 3, 'first frames');
    const out = {};
    if (e.data.mode === 'loop') {
      // lost again as soon as each new device comes: given up when the second new one is
      let made = 0;
      for (let k = 0; k < 3; k++) {
        const d = VG.device;
        d.destroy();
        await wait(() => VG.device !== d || log.some((l) => l.startsWith('for good')), 'a new device or giving up');
        if (VG.device !== d) made++;
      }
      await wait(() => log.some((l) => l.startsWith('for good')), 'giving up');
      out.made = made;
      out.log = log;
      return postMessage(out);
    }
    // objects of the old device in the table, a readback and work-done on their way
    const first = VG.device;
    lib.vngpu_buffer(64, GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ);
    const rb = lib.vngpu_buffer(64, GPUBufferUsage.COPY_DST | GPUBufferUsage.MAP_READ);
    out.tableBefore = VG.objs.length;
    oldDevice = first;
    lib.vngpu_read_buffer(rb, 64, 0, 77);
    lib.vngpu_on_done(78);
    first.destroy();
    await wait(() => log.some((l) => l.startsWith('lost ')), 'the new device');
    out.newDevice = VG.device !== first;
    out.tableAfter = VG.objs.length;
    const f0 = frames;
    await wait(() => frames > f0 + 3, 'frames after the loss');
    // (the new device draws, and takes new objects, without errors)
    VG.device.pushErrorScope('validation');
    const b = lib.vngpu_buffer(16, GPUBufferUsage.COPY_DST);
    VG.device.queue.writeBuffer(VG.objs[b], 0, new Uint8Array(16));
    out.error = (await VG.device.popErrorScope())?.message ?? null;
    out.firstIndex = b;
    await new Promise((r) => setTimeout(r, 100));
    // a second loss
    const second = VG.device;
    second.destroy();
    await wait(() => log.filter((l) => l.startsWith('lost ')).length === 2, 'the second new device');
    out.again = VG.device !== second && VG.device !== first;
    const f1 = frames;
    await wait(() => frames > f1 + 3, 'frames after the second loss');
    // no adapter any more: given up after the tries
    const real = navigator.gpu.requestAdapter.bind(navigator.gpu);
    let asked = 0;
    navigator.gpu.requestAdapter = async () => (asked++, null);
    VG.device.destroy();
    await wait(() => log.some((l) => l.startsWith('for good')), 'giving up');
    out.asked = asked;
    const f2 = frames;
    await new Promise((r) => setTimeout(r, 200));
    out.framesAfterGivingUp = frames - f2;
    navigator.gpu.requestAdapter = real;
    out.lateFromOld = lateFromOld;
    out.log = log;
    postMessage(out);
  } catch (e) {
    postMessage({ failed: String(e), log });
  }
};
`;

const server = createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end('<!doctype html><title>gpulost</title>');
}).listen(0, '127.0.0.1');
await new Promise((r) => server.once('listening', r));
const browser = await chromium.launch({ args: ['--enable-unsafe-webgpu', '--use-webgpu-adapter=swiftshader', '--enable-features=Vulkan'] });
try {
  const page = await browser.newPage();
  page.on('console', (m) => console.log(`page: ${m.text()}`));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const run = (mode) =>
    page.evaluate(
      async ([code, mode]) => {
        if (!navigator.gpu || !(await navigator.gpu.requestAdapter())) return { skip: 'no WebGPU adapter' };
        const w = new Worker(URL.createObjectURL(new Blob([code], { type: 'text/javascript' })));
        const c = document.createElement('canvas');
        document.body.append(c);
        const canvas = c.transferControlToOffscreen();
        const done = new Promise((res) => (w.onmessage = (e) => res(e.data)));
        w.onerror = (e) => console.log(`worker error: ${e.message}`);
        w.postMessage({ canvas, mode }, [canvas]);
        const r = await done;
        w.terminate();
        return r;
      },
      [worker, mode],
    );
  const r = await run('once');
  console.log(JSON.stringify(r, null, 1));
  assert(!r.skip, r.skip);
  assert(!r.failed, r.failed);
  // the order: stopped, then made again on a new device, the frames stopped in between
  const losing = r.log.find((l) => l.startsWith('losing '));
  const lost = r.log.find((l) => l.startsWith('lost '));
  assert(losing && lost, 'losing, then lost');
  assert(r.log.indexOf(losing) < r.log.indexOf(lost));
  assert.equal(losing.split(' ')[1], lost.split(' ')[1], 'no frame while there is no device');
  assert(r.log.some((l) => l.startsWith('webgpu: device lost (destroyed)')), 'the loss is logged');
  assert(r.newDevice, 'a new device');
  assert(r.tableBefore > 1);
  assert.equal(r.tableAfter, 1, 'the table starts again (vngpu_reset)');
  assert.equal(r.firstIndex, 1, "the new device's objects from index 1");
  assert.equal(r.error, null, 'no WebGPU error on the new device');
  assert(r.again, 'a second loss comes back too');
  // (the old device's readback, if it answered, answered before the new device came)
  const mapped = r.log.findIndex((l) => l.startsWith('mapped 77'));
  assert(mapped < 0 || mapped < r.log.indexOf(lost), "a lost device's readback does not answer after it was replaced");
  assert.deepEqual(r.lateFromOld, [], "the old device's work-done does not reach the new one");
  assert.equal(r.asked, 6, 'six tries for a new device');
  assert(r.log.some((l) => l.startsWith('for good')));
  assert.equal(r.framesAfterGivingUp, 0, 'no frames once given up');
  const loop = await run('loop');
  console.log(JSON.stringify(loop, null, 1));
  assert(!loop.failed, loop.failed);
  assert.equal(loop.made, 2, 'two new devices, then given up');
  assert(!loop.log.some((l) => l.startsWith('webgpu: no new device yet')));
  assert.equal(loop.log.filter((l) => l.startsWith('lost ')).length, 2);
  console.log('gpulost: ok');
} finally {
  await browser.close();
  server.close();
}
