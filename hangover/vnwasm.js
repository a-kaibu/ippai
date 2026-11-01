// Wine PE DLLs compiled to wasm (crates/vnhost/src/wasmdll.rs, docs/wasm-dlls.md): every
// thread (worker) instantiates its process's modules into its own funcref table, at the slots
// vnhost gave them, with its own stack pointer and TEB globals. A worker runs one thread after
// another: vnvm_wasm_thread_init starts over.
addToLibrary({
  $VNWASM: { modules: new Map(), t: null },

  vnvm_wasm_thread_init__deps: ['$VNWASM'],
  vnvm_wasm_thread_init__sig: 'vi',
  vnvm_wasm_thread_init: (teb) => {
    VNWASM.t = {
      table: new WebAssembly.Table({ element: 'anyfunc', initial: 0 }),
      sp: new WebAssembly.Global({ value: 'i32', mutable: true }, 0),
      teb: new WebAssembly.Global({ value: 'i32', mutable: false }, teb),
      limit: new WebAssembly.Global({ value: 'i32', mutable: true }, 0),
      insts: [],
    };
  },

  // instance `id` (the process's id-th wasm DLL) on this thread; its entries go into this
  // worker's own function table at `slots` (n of them), where vnhost calls them; 0 or -1
  vnvm_wasm_instantiate__deps: ['$VNWASM'],
  vnvm_wasm_instantiate__sig: 'iipiiiiipii',
  vnvm_wasm_instantiate: (id, ptr, len, mem, slot, image, size, slots, n, tags) => {
    const t = VNWASM.t;
    ptr >>>= 0;
    mem >>>= 0;
    image >>>= 0;
    try {
      // compiled once per worker (V8 shares the code of identical bytes between workers)
      const key = `${len}:${HEAPU8[ptr + (len >> 1)]}:${HEAPU8[ptr + len - 1]}:${image}`;
      let module = VNWASM.modules.get(key);
      if (!module) VNWASM.modules.set(key, (module = new WebAssembly.Module(HEAPU8.slice(ptr, ptr + len))));
      if (t.table.length < slot + size) t.table.grow(slot + size - t.table.length);
      const got = {};
      const imports = {
        env: {
          memory: wasmMemory,
          __indirect_function_table: t.table,
          __stack_pointer: t.sp,
          __memory_base: new WebAssembly.Global({ value: 'i32', mutable: false }, mem),
          __table_base: new WebAssembly.Global({ value: 'i32', mutable: false }, (0xfe000000 + slot) | 0),
        },
        vn: {
          table_slot: new WebAssembly.Global({ value: 'i32', mutable: false }, slot),
          image_base: new WebAssembly.Global({ value: 'i32', mutable: false }, image),
          teb: t.teb,
          limit: t.limit,
          tags: new WebAssembly.Global({ value: 'i32', mutable: false }, tags),
          x86_call: wasmExports.vnhost_wasm_x86_call,
          x86_call_f: wasmExports.vnhost_wasm_x86_call_f,
          syscall: wasmExports.vnhost_wasm_syscall,
        },
        'GOT.mem': got,
        'GOT.func': got,
      };
      for (const i of WebAssembly.Module.imports(module))
        if (i.module == 'GOT.mem' || i.module == 'GOT.func') got[i.name] ||= new WebAssembly.Global({ value: 'i32', mutable: true }, 0);
      const inst = new WebAssembly.Instance(module, imports);
      const entries = [];
      for (let k = 0; inst.exports['__vn_in' + k]; k++) entries.push(inst.exports['__vn_in' + k]);
      t.insts[id] = { inst, got };
      t.limit.value = Math.max(t.limit.value, slot + size);
      // over placeholders at the same indices on every worker (crates/vnhost/src/wasmdll_slots.rs)
      if (entries.length != n) throw new Error(`${entries.length} entries, ${n} slots`);
      entries.forEach((f, k) => wasmTable.set(HEAPU32[(slots >>> 2) + k], f));
      return 0;
    } catch (e) {
      err(`vnwasm: ${e}`);
      return -1;
    }
  },

  vnvm_wasm_got__deps: ['$VNWASM'],
  vnvm_wasm_got__sig: 'vipii',
  vnvm_wasm_got: (id, name, len, value) => {
    const g = VNWASM.t.insts[id].got[UTF8ToString(name, len)];
    if (g) g.value = value;
  },

  // the module's first entry: data relocations (they need the GOT) and constructors
  vnvm_wasm_fixup__deps: ['$VNWASM'],
  vnvm_wasm_fixup__sig: 'vi',
  vnvm_wasm_fixup: (id) => {
    const e = VNWASM.t.insts[id].inst.exports;
    e.__wasm_apply_data_relocs?.();
    e.__wasm_call_ctors?.();
  },
});
