// The program's environment: node's, or in the page the object it passes as `vnvmEnv`.
Module.preRun = Module.preRun || [];
Module.preRun.push(() => {
  if (ENVIRONMENT_IS_NODE) Object.assign(ENV, process.env);
  if (Module.vnvmEnv) Object.assign(ENV, Module.vnvmEnv);
});

// Regions an earlier run saved (crates/kura/src/jit.rs): `vnvmJit` = { parts: [{ module, idx }] }
// from the page, or under node MASU_JIT_FILE.wasm/.idx; and the page's saved games `vnvmSaves`
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
var MASU_KERNEL_BYTES = 32 << 20;
// the tree lists the page fetched before starting (`vnvmFiles`, host/vnfs.js): every thread gets
// them, so a thread's first look at the file system needs no request
var vnvmTrees = null;
// The app origin, copied into pthreads whose location may be a blob URL.
var vnvmTlsRelay = null;
var vnvmAccount = null;
var vnvmSensitive = null;
var vnvmFs = null;
// this process's id (SAKE_PID: a process another one started; the first is 0x10), what the
// shared locks' words hold while one of its threads has them (host/vnfs.js, host/vnkern.js)
var vnvmPid = 0x10;
// web/bench.html reads the program's output: the page names a BroadcastChannel (`vnvmPrint`)
// and its threads' lines go there too (a program's output, sake's console_write on stderr,
// comes from its thread's worker, not the page)
var vnvmPrintChannel = null;
if (ENVIRONMENT_IS_PTHREAD && !ENVIRONMENT_IS_NODE) {
  Module.print = (line) => {
    console.log(line);
    if (vnvmPrintChannel) vnvmPrintChannel.postMessage(line);
  };
  Module.printErr = (line) => {
    console.error(line);
    if (vnvmPrintChannel) vnvmPrintChannel.postMessage(line);
  };
}
function masuSharedFs() {
  try {
    const b = new SharedArrayBuffer(1 << 20, { maxByteLength: 1 << 30 });
    if (b.growable) return b;
  } catch {}
  return new SharedArrayBuffer(64 << 20);
}
Module.preInit = [].concat(Module.preInit || [], () => {
  if (ENVIRONMENT_IS_PTHREAD) return;
  const f = ENVIRONMENT_IS_NODE && process.env.MASU_JIT_FILE;
  if (f && !Module.vnvmJit) {
    try {
      const fs = require('fs');
      Module.vnvmJit = { parts: [{ module: new WebAssembly.Module(fs.readFileSync(f + '.wasm')), idx: new Uint8Array(fs.readFileSync(f + '.idx')) }] };
    } catch (e) {
      if (e.code != 'ENOENT') err(`vnjit: ${f}: ${e}`);
    }
  }
  vnvmAccount = Module.vnvmAccount || null;
  vnvmSensitive = Module.vnvmSensitive || null;
  // (the processes' shared namespace: grown as needed where the browser can, else 64 MiB;
  // used only once a child process starts, kura/src/vfs/shared.rs)
  vnvmFs = Module.vnvmFs || (!ENVIRONMENT_IS_NODE ? masuSharedFs() : null);
  vnvmJitBundle = Module.vnvmJit || null;
  vnvmPid = Number((Module.vnvmEnv && Module.vnvmEnv.SAKE_PID) || (ENVIRONMENT_IS_NODE && process.env.SAKE_PID) || 0x10);
  vnvmTrees = Module.vnvmFiles || null;
  if (!ENVIRONMENT_IS_NODE) { const u = new URL("/net/tls/soft-denchi", location.href); u.protocol = u.protocol === "https:" ? "wss:" : "ws:"; vnvmTlsRelay = u.href; }
  vnvmKernel = Module.vnvmKernel || (typeof SharedArrayBuffer == 'function' ? new SharedArrayBuffer(MASU_KERNEL_BYTES) : null);
  vnvmCanSpawn = !!Module.vnvmSpawn;
  if (Module.vnvmSaves) {
    vnvmSaves = new Uint8Array(new SharedArrayBuffer(Module.vnvmSaves.length));
    vnvmSaves.set(Module.vnvmSaves);
  }
});
// What this instance's threads get, and the end of it (vnvmForget, at the exit): each instance
// in the realm (the page's child processes, host/node-main.js's) adds its own, found again by
// the memory the 'load' message names (else the first instance's values, its saved JIT regions
// and saves, went to every later instance's threads, and its closure kept it alive).
var vnvmForget = () => {};
if (!ENVIRONMENT_IS_PTHREAD) {
  // the main thread adds them to each worker's 'load' message
  const W = ENVIRONMENT_IS_NODE ? require('worker_threads').Worker : globalThis.Worker;
  if (W) {
    const host = {
      memory: () => (typeof wasmMemory == 'undefined' ? undefined : wasmMemory),
      load: () => ({ vnvmJit: vnvmJitBundle, vnvmSaves, vnvmTrees, vnvmKernel, vnvmCanSpawn, vnvmTlsRelay, vnvmAccount, vnvmSensitive, vnvmFs, vnvmPid, vnvmPrint: Module.vnvmPrint || null }),
      // (what the thread asks of the host: to start a process)
      ask: (d) => d && d.vnvmSpawn && Module.vnvmSpawn?.(d.vnvmSpawn, vnvmKernel, vnvmFs),
      ports: () => [masuFetchConnect(), masuTlsConnect()],
    };
    const hosts = (W.vnvmHosts ??= new Set());
    hosts.add(host);
    vnvmForget = () => hosts.delete(host);
    if (!W.prototype.vnvmPost) {
      W.prototype.vnvmPost = W.prototype.postMessage;
      W.prototype.postMessage = function (m, ...rest) {
        if (m && m.cmd === 'load') {
          const all = [...hosts];
          const h = all.find((x) => x.memory() === m.wasmMemory) ?? all[0];
          if (h) {
            m = { ...m, ...h.load() };
            if (ENVIRONMENT_IS_NODE) this.on('message', h.ask);
            else this.addEventListener('message', (e) => h.ask(e.data));
            if (!ENVIRONMENT_IS_NODE) {
              [m.vnvmFetchPort, m.vnvmTlsPort] = h.ports();
              rest[0] = [...(rest[0] || []), m.vnvmFetchPort, m.vnvmTlsPort];
            }
          }
        }
        return this.vnvmPost(m, ...rest);
      };
    }
  }
} else {
  const take = (d) => {
    if (d && d.cmd === 'load') {
      vnvmJitBundle = d.vnvmJit || null;
      vnvmSaves = d.vnvmSaves || null;
      vnvmKernel = d.vnvmKernel || null;
      vnvmCanSpawn = !!d.vnvmCanSpawn;
      vnvmTlsRelay = d.vnvmTlsRelay || null;
      vnvmAccount = d.vnvmAccount || null;
      vnvmSensitive = d.vnvmSensitive || null;
      vnvmFs = d.vnvmFs || null;
      vnvmPid = d.vnvmPid || 0x10;
      if (d.vnvmTrees) Module.vnvmFiles = d.vnvmTrees;
      if (d.vnvmPrint && !ENVIRONMENT_IS_NODE && !vnvmPrintChannel) vnvmPrintChannel = new BroadcastChannel(d.vnvmPrint);
      if (d.vnvmFetchPort) masuFetchPort = d.vnvmFetchPort;
      if (d.vnvmTlsPort) masuTlsPort = d.vnvmTlsPort;
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
    const pid = vnvmPid;
    // (the processes' shared file system's lock, if a thread of this one had it: host/vnfs.js;
    // else every other process would wait for it for good, the page's thread spinning)
    if (vnvmFs) {
      const f = new Int32Array(vnvmFs, 0, 8);
      if (Atomics.compareExchange(f, 0, pid, 0) === pid) Atomics.notify(f, 0);
    }
    if (!vnvmKernel) return;
    const h = new Int32Array(vnvmKernel, 0, 256);
    if (!Atomics.load(h, 2)) return;
    // (the kernel's lock, if a thread of this one had it: host/vnkern.js)
    if (Atomics.compareExchange(h, 5, pid, 0) === pid) {
      Atomics.store(h, 0, 0);
      Atomics.notify(h, 0);
    }
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
    vnvmForget();
    onExit?.(code);
  };
  const onAbort = Module.onAbort;
  Module.onAbort = (what) => {
    exited(3);
    vnvmForget();
    onAbort?.(what);
  };
}

// The page's threads' stdout / stderr (crates/kura/src/wrap.rs, #221): they queue it and the
// page's thread prints it from here, every 50 ms and at the end, so that a write never waits for
// the page's thread (emscripten's own write from a thread does, and a game stopped for good in
// one, rarely). Records: fd, u16 length, bytes.
if (!ENVIRONMENT_IS_PTHREAD && !ENVIRONMENT_IS_NODE) {
  let buf = 0;
  let every = 0;
  const CAP = 1 << 16;
  const drain = () => {
    if (!buf) return;
    for (let k = 0; k < 64; k++) {
      const n = wasmExports.vnhost_output_take(buf, CAP) >>> 0;
      if (!n) return;
      const b = HEAPU8.subarray(buf >>> 0, (buf >>> 0) + n);
      for (let at = 0; at + 3 <= n; ) {
        const fd = b[at], len = b[at + 1] | (b[at + 2] << 8);
        for (let i = at + 3; i < at + 3 + len; i++) printChar(fd, b[i]);
        at += 3 + len;
      }
    }
  };
  const ready = Module.onRuntimeInitialized;
  Module.onRuntimeInitialized = () => {
    if (wasmExports.vnhost_output_queue) {
      buf = _malloc(CAP);
      wasmExports.vnhost_output_queue();
      every = setInterval(drain, 50);
    }
    ready?.();
  };
  const onExit = Module.onExit;
  // (the last lines, then no more: the timer's closure would keep the ended instance, its
  // whole memory, alive and call into it 20 times a second)
  const last = () => {
    try {
      drain();
    } catch {}
    clearInterval(every);
    buf = 0;
  };
  Module.onExit = (code) => {
    last();
    onExit?.(code);
  };
  const onAbort = Module.onAbort;
  Module.onAbort = (what) => {
    last();
    onAbort?.(what);
  };
}
