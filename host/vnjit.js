// The JIT's module loader (see crates/vnhost/src/jit.rs). Every thread (worker) has its own
// funcref table, dispatcher instance and block instances; all share the one memory.
addToLibrary({
  $VNJIT: { table: null, used: 0, run: null, env: null },

  vnvm_jit_init__deps: ['$VNJIT'],
  vnvm_jit_init__sig: 'vpp',
  vnvm_jit_init: (ptr, len) => {
    ptr >>>= 0;
    const bytes = HEAPU8.slice(ptr, ptr + len);
    VNJIT.table = new WebAssembly.Table({ element: 'anyfunc', initial: 0 });
    VNJIT.used = 0;
    const inst = new WebAssembly.Instance(new WebAssembly.Module(bytes),
      { env: { memory: wasmMemory, table: VNJIT.table } });
    VNJIT.run = inst.exports.f0;
    VNJIT.env = {
      memory: wasmMemory,
      step: wasmExports.vnhost_jit_step,
      eflags: wasmExports.vnhost_jit_eflags,
      status: wasmExports.vnhost_jit_status,
      x87: wasmExports.vnhost_jit_x87,
      callmiss: wasmExports.vnhost_jit_callmiss,
      table: VNJIT.table,
    };
  },

  vnvm_jit_load__deps: ['$VNJIT'],
  vnvm_jit_load__sig: 'ippi',
  vnvm_jit_load: (ptr, len, n) => {
    ptr >>>= 0;
    let inst;
    try {
      inst = new WebAssembly.Instance(new WebAssembly.Module(HEAPU8.slice(ptr, ptr + len)), { env: VNJIT.env });
    } catch (e) {
      err(`vnjit: ${e}`);
      return -1;
    }
    // grown by half again each time: growing copies the table
    const base = VNJIT.used;
    if (base + n > VNJIT.table.length) VNJIT.table.grow(Math.max(n, VNJIT.table.length >> 1, 256));
    VNJIT.used += n;
    for (let i = 0; i < n; i++) VNJIT.table.set(base + i, inst.exports['f' + i]);
    return base;
  },

  // the index of the saved regions (see pre.js; the parts' indexes one after another): its
  // length, copied to `ptr` if it fits
  vnvm_jit_bundle_idx__sig: 'ipp',
  vnvm_jit_bundle_idx: (ptr, cap) => {
    const b = vnvmJitBundle;
    if (!b) return 0;
    const n = b.parts.reduce((n, p) => n + p.idx.length, 0);
    if (cap >= n) {
      let at = ptr >>> 0;
      for (const p of b.parts) {
        HEAPU8.set(p.idx, at);
        at += p.idx.length;
      }
    }
    return n;
  },

  // the saved regions on this thread, every part's functions in a row: the table index of the
  // first, or -1
  vnvm_jit_bundle_load__deps: ['$VNJIT'],
  vnvm_jit_bundle_load__sig: 'i',
  vnvm_jit_bundle_load: () => {
    const b = vnvmJitBundle;
    if (!b) return -1;
    let insts;
    try {
      insts = b.parts.map((p) => new WebAssembly.Instance(p.module, { env: VNJIT.env }));
    } catch (e) {
      err(`vnjit: saved regions: ${e}`);
      return -1;
    }
    const n = insts.reduce((n, i) => n + Object.keys(i.exports).length, 0);
    const base = VNJIT.used;
    if (base + n > VNJIT.table.length) VNJIT.table.grow(Math.max(n, VNJIT.table.length >> 1, 256));
    VNJIT.used += n;
    let at = base;
    for (const inst of insts) {
      const k = Object.keys(inst.exports).length;
      for (let i = 0; i < k; i++) VNJIT.table.set(at + i, inst.exports['f' + i]);
      at += k;
    }
    return base;
  },

  // sends new regions to be saved (PUT to `url`; the server merges them in), from the main
  // thread: the program's threads never return to their event loop
  vnvm_jit_save__proxy: 'sync',
  vnvm_jit_save__sig: 'vppp',
  vnvm_jit_save: (url, ptr, len) => {
    ptr >>>= 0;
    const body = HEAPU8.slice(ptr, ptr + len);
    fetch(UTF8ToString(url), { method: 'PUT', body }).then(
      (r) => r.ok || err(`vnjit: save: HTTP ${r.status}`),
      (e) => err(`vnjit: save: ${e}`));
  },

  vnvm_jit_run__deps: ['$VNJIT'],
  vnvm_jit_run__sig: 'ip',
  vnvm_jit_run: (cpu) => VNJIT.run(cpu),
});
