// The program's environment: node's, or in the page the object it passes as `vnvmEnv`.
Module.preRun = Module.preRun || [];
Module.preRun.push(() => {
  if (ENVIRONMENT_IS_NODE) Object.assign(ENV, process.env);
  if (Module.vnvmEnv) Object.assign(ENV, Module.vnvmEnv);
});

// Regions an earlier run saved (crates/kura/src/jit.rs): `vnvmJit` = { parts: [{ module, idx }] }
// from the page, or under node IPPAI_JIT_FILE.wasm/.idx; and the page's saved games `vnvmSaves`
// (bytes, read as "@saves" by host/vnfs.js). Every thread gets them in the message that loads it
// ('load'): the saves as a SharedArrayBuffer, not copied per thread.
var vnvmJitBundle = null;
var vnvmSaves = null;
// the kernel the host's processes share (host/vnkern.js): the host's buffer (`vnvmKernel`, node's
// runner and the page pass the one of the process that started this one), else this
// process's own; and whether the host starts processes (`vnvmSpawn`, on the main thread)
var vnvmKernel = null;
var vnvmKernelI32 = null;
var vnvmCanSpawn = false;
var IPPAI_KERNEL_BYTES = 32 << 20;
// the tree lists the page fetched before starting (`vnvmFiles`, host/vnfs.js): every thread gets
// them, so a thread's first look at the file system needs no request
var vnvmTrees = null;
Module.preInit = [].concat(Module.preInit || [], () => {
  if (ENVIRONMENT_IS_PTHREAD) return;
  const f = ENVIRONMENT_IS_NODE && process.env.IPPAI_JIT_FILE;
  if (f && !Module.vnvmJit) {
    try {
      const fs = require('fs');
      Module.vnvmJit = { parts: [{ module: new WebAssembly.Module(fs.readFileSync(f + '.wasm')), idx: new Uint8Array(fs.readFileSync(f + '.idx')) }] };
    } catch (e) {
      if (e.code != 'ENOENT') err(`vnjit: ${f}: ${e}`);
    }
  }
  vnvmJitBundle = Module.vnvmJit || null;
  vnvmTrees = Module.vnvmFiles || null;
  vnvmKernel = Module.vnvmKernel || (typeof SharedArrayBuffer == 'function' ? new SharedArrayBuffer(IPPAI_KERNEL_BYTES) : null);
  vnvmCanSpawn = !!Module.vnvmSpawn;
  if (Module.vnvmSaves) {
    vnvmSaves = new Uint8Array(new SharedArrayBuffer(Module.vnvmSaves.length));
    vnvmSaves.set(Module.vnvmSaves);
  }
});
if (!ENVIRONMENT_IS_PTHREAD) {
  // the main thread adds them to each worker's 'load' message
  const W = ENVIRONMENT_IS_NODE ? require('worker_threads').Worker : globalThis.Worker;
  if (W && !W.prototype.vnvmPost) {
    W.prototype.vnvmPost = W.prototype.postMessage;
    W.prototype.postMessage = function (m, ...rest) {
      if (m && m.cmd === 'load') {
        m = { ...m, vnvmJit: vnvmJitBundle, vnvmSaves, vnvmTrees, vnvmKernel, vnvmCanSpawn };
        // (what the thread asks of the host: to start a process)
        const ask = (d) => d && d.vnvmSpawn && Module.vnvmSpawn?.(d.vnvmSpawn, vnvmKernel);
        if (ENVIRONMENT_IS_NODE) this.on('message', ask);
        else this.addEventListener('message', (e) => ask(e.data));
        if (!ENVIRONMENT_IS_NODE) {
          m.vnvmFetchPort = ippaiFetchConnect();
          rest[0] = [...(rest[0] || []), m.vnvmFetchPort];
        }
      }
      return this.vnvmPost(m, ...rest);
    };
  }
} else {
  const take = (d) => {
    if (d && d.cmd === 'load') {
      vnvmJitBundle = d.vnvmJit || null;
      vnvmSaves = d.vnvmSaves || null;
      vnvmKernel = d.vnvmKernel || null;
      vnvmCanSpawn = !!d.vnvmCanSpawn;
      if (d.vnvmTrees) Module.vnvmFiles = d.vnvmTrees;
      if (d.vnvmFetchPort) ippaiFetchPort = d.vnvmFetchPort;
    }
  };
  if (ENVIRONMENT_IS_NODE) require('worker_threads').parentPort.on('message', take);
  else self.addEventListener('message', (e) => take(e.data));
}

// The page's canvas, for the thread that draws (crates/kura/src/render, host/vngpu.js): the
// page passes `vnvmCanvas`, an OffscreenCanvas; vngpu_attach sends it to that thread's worker,
// where this is a promise of it.
var vnvmCanvas = null;
if (ENVIRONMENT_IS_PTHREAD && !ENVIRONMENT_IS_NODE) {
  let got;
  vnvmCanvas = new Promise((r) => (got = r));
  // (a message without cmd: emscripten's handler leaves it alone)
  self.addEventListener('message', (e) => e.data && e.data.vnvmCanvas && got(e.data.vnvmCanvas));
}

// A process that ends (however: ExitProcess, a crash) goes on the shared kernel's list of
// ended processes (host/vnkern.js), which the others read.
if (!ENVIRONMENT_IS_PTHREAD) {
  const exited = (code) => {
    if (!vnvmKernel) return;
    const h = new Int32Array(vnvmKernel, 0, 256);
    if (!Atomics.load(h, 2)) return;
    const pid = Number((Module.vnvmEnv && Module.vnvmEnv.SAKE_PID) || (ENVIRONMENT_IS_NODE && process.env.SAKE_PID) || 0x10);
    const i = Atomics.load(h, 4) >>> 0;
    Atomics.store(h, 16 + 2 * (i % 64), pid);
    Atomics.store(h, 17 + 2 * (i % 64), code | 0);
    Atomics.store(h, 4, i + 1);
    Atomics.add(h, 1, 1);
    Atomics.notify(h, 1);
  };
  const onExit = Module.onExit;
  Module.onExit = (code) => {
    exited(code);
    onExit?.(code);
  };
  const onAbort = Module.onAbort;
  Module.onAbort = (what) => {
    exited(3);
    onAbort?.(what);
  };
}
