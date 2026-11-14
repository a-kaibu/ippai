// The kernel the host's processes share (crates/sake/src/kern/host.rs): one SharedArrayBuffer
// for all of them, `vnvmKernel` (host/pre.js: the host's, or the first process's own), every
// thread of every process has it. Int32 words:
//   0 lock (0 free, 1 taken, 2 taken with waiters), 1 generation (changes with every change,
//   what waiting is on), 2 sharing (1 once a process started another), 3 the state's length,
//   4 how many processes ended, 16.. a ring of 64 (id, exit code) of them;
// the state's bytes from byte 1024.
addToLibrary({
  $vnkernH: () => (vnvmKernelI32 ??= new Int32Array(vnvmKernel, 0, 256)),
  vnhost_kern_shared__deps: ['$vnkernH'],
  vnhost_kern_shared: () => (vnvmKernel && Atomics.load(vnkernH(), 2) ? 1 : 0),
  vnhost_kern_can_share: () => (vnvmKernel && vnvmCanSpawn ? 1 : 0),
  vnhost_kern_start_sharing__deps: ['$vnkernH'],
  vnhost_kern_start_sharing: () => Atomics.store(vnkernH(), 2, 1),
  vnhost_kern_lock__deps: ['$vnkernH'],
  vnhost_kern_lock: (pid) => {
    // (Drepper's futex mutex; its holder's process in [5], so that the lock goes when that
    // process ends holding it: host/pre.js)
    const h = vnkernH();
    let c = Atomics.compareExchange(h, 0, 0, 1);
    if (c !== 0) {
      if (c !== 2) c = Atomics.exchange(h, 0, 2);
      while (c !== 0) {
        Atomics.wait(h, 0, 2);
        c = Atomics.exchange(h, 0, 2);
      }
    }
    Atomics.store(h, 5, pid);
  },
  vnhost_kern_unlock__deps: ['$vnkernH'],
  vnhost_kern_unlock: () => {
    const h = vnkernH();
    Atomics.store(h, 5, 0);
    if (Atomics.sub(h, 0, 1) !== 1) {
      Atomics.store(h, 0, 0);
      Atomics.notify(h, 0, 1);
    }
  },
  vnhost_kern_get__deps: ['$vnkernH'],
  vnhost_kern_get__sig: 'ppp',
  vnhost_kern_get: (dst, cap) => {
    const n = vnkernH()[3];
    if (n && cap >= n) HEAPU8.set(new Uint8Array(vnvmKernel, 1024, n), dst >>> 0);
    return n;
  },
  vnhost_kern_put__deps: ['$vnkernH'],
  vnhost_kern_put__sig: 'ipp',
  vnhost_kern_put: (src, len) => {
    if (1024 + len > vnvmKernel.byteLength) return 0;
    new Uint8Array(vnvmKernel, 1024, len).set(HEAPU8.subarray(src >>> 0, (src >>> 0) + len));
    vnkernH()[3] = len;
    return 1;
  },
  vnhost_kern_gen__deps: ['$vnkernH'],
  vnhost_kern_gen: () => Atomics.load(vnkernH(), 1),
  vnhost_kern_bump__deps: ['$vnkernH'],
  vnhost_kern_bump: () => (Atomics.add(vnkernH(), 1, 1) + 1) | 0,
  vnhost_kern_notify__deps: ['$vnkernH'],
  vnhost_kern_notify: () => Atomics.notify(vnkernH(), 1),
  vnhost_kern_wait__deps: ['$vnkernH'],
  vnhost_kern_wait: (seen, ms) => (Atomics.wait(vnkernH(), 1, seen, ms) === 'timed-out' ? 0 : 1),
  vnhost_kern_dead__deps: ['$vnkernH'],
  vnhost_kern_dead__sig: 'iip',
  vnhost_kern_dead: (i, out) => {
    const h = vnkernH();
    if ((i >>> 0) >= (Atomics.load(h, 4) >>> 0)) return 0;
    const at = 16 + 2 * (i % 64);
    HEAPU32[(out >>> 2)] = Atomics.load(h, at);
    HEAPU32[(out >>> 2) + 1] = Atomics.load(h, at + 1);
    return 1;
  },
  // CreateProcess: asks the page or node (Module.vnvmSpawn, on the main thread) to start
  // process `pid` from host file `exe`
  vnhost_spawn__sig: 'iipp',
  vnhost_spawn: (pid, exe, len) => {
    if (!vnvmCanSpawn) return 0;
    const m = { vnvmSpawn: { pid, exe: UTF8ToString(exe >>> 0, len) } };
    if (ENVIRONMENT_IS_NODE) require('worker_threads').parentPort.postMessage(m);
    else self.postMessage(m);
    return 1;
  },
});
