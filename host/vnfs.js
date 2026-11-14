// vnvm_fetch(name, &len): one file of the store (trees/<name>.tsv or chunks/ab/<hash>, the
// latter zstd-compressed; "chunks/ab/<hash>#<from>+<len>/<block>": those bytes of the decoded
// chunk, fewer past its stored end, asked for in blocks of <block> bytes), for crates/kura/src/vfs.rs; or "@saves", the saved games the page
// passed (host/pre.js; node: the file $IPPAI_SAVES). Returns a malloc'ed buffer, or 0. The bytes
// that came over the network (compressed; none from the browser's HTTP cache) go to vfs.rs's
// counters (vnhost_net_wire).
//
// node: read from $IPPAI_STORE. Browser: a dedicated worker uses fetch; the game pthread waits for its response from
// store/ beside the directory of sake.js; the server sends chunks with "Content-Encoding: zstd",
// so the browser decompresses them.
addToLibrary({
  $vnvmLastChunk: null,
  $vnfsSharedH: () => vnvmFs ? new Int32Array(vnvmFs, 0, 8) : null,
  vnhost_vfs_shared_lock__deps: ['$vnfsSharedH'],
  vnhost_vfs_shared_lock: () => {
    const h = vnfsSharedH();
    // (off until the first child process: a single process keeps its own file system)
    if (!h || !Atomics.load(h, 3)) return 0;
    // (the lock word holds its holder's process id, not 1: a process that ends holding it,
    // a crash in the middle, has it released for the others, host/pre.js `exited`)
    const me = vnvmPid || 0x10;
    let c;
    while ((c = Atomics.compareExchange(h, 0, 0, me)) !== 0) {
      if (ENVIRONMENT_IS_PTHREAD) Atomics.wait(h, 0, c);
      // The page's export can only wait for a short in-memory transaction;
      // tree loading and network reads happen outside this lock.
    }
    return 1;
  },
  vnhost_vfs_shared_unlock__deps: ['$vnfsSharedH'],
  vnhost_vfs_shared_unlock: () => { const h = vnfsSharedH(); Atomics.store(h, 0, 0); Atomics.notify(h, 0); },
  vnhost_vfs_shared_enable__deps: ['$vnfsSharedH'],
  vnhost_vfs_shared_enable: () => { const h = vnfsSharedH(); if (!h) return 0; Atomics.store(h, 3, 1); return 1; },
  vnhost_vfs_shared_generation__deps: ['$vnfsSharedH'],
  vnhost_vfs_shared_generation: () => { const h = vnfsSharedH(); return h ? Atomics.load(h, 1) : 0; },
  // (the buffer's bytes from 32 on: kura/src/vfs/shared.rs's snapshot and journal; [2] how
  // many are in use, [1] their generation)
  vnhost_vfs_shared_len__deps: ['$vnfsSharedH'],
  vnhost_vfs_shared_len__sig: 'p',
  vnhost_vfs_shared_len: () => Atomics.load(vnfsSharedH(), 2),
  vnhost_vfs_shared_read__sig: 'vppp',
  vnhost_vfs_shared_read: (dst, off, len) => {
    HEAPU8.set(new Uint8Array(vnvmFs, 32 + (off >>> 0), len >>> 0), dst >>> 0);
  },
  vnhost_vfs_shared_write__sig: 'ippp',
  vnhost_vfs_shared_write: (src, off, len) => {
    off >>>= 0; len >>>= 0;
    const need = 32 + off + len;
    if (need > vnvmFs.byteLength) {
      // a growable buffer takes what the namespace needs (up to its maximum)
      const want = Math.min(need + (need >>> 2), vnvmFs.maxByteLength ?? 0);
      if (!vnvmFs.growable || want < need) return 0;
      try { vnvmFs.grow(want); } catch { return 0; }
    }
    new Uint8Array(vnvmFs, 32 + off, len).set(HEAPU8.subarray(src >>> 0, (src >>> 0) + len));
    return 1;
  },
  vnhost_vfs_shared_commit__deps: ['$vnfsSharedH'],
  vnhost_vfs_shared_commit__sig: 'ip',
  vnhost_vfs_shared_commit: (len) => {
    const h = vnfsSharedH(); Atomics.store(h, 2, len >>> 0);
    return (Atomics.add(h, 1, 1) + 1) | 0;
  },
  vnvm_fetch__deps: ['malloc', '$vnvmLastChunk'],
  // pointers are above 2GB (GLOBAL_BASE): 'p' makes them unsigned
  vnvm_fetch__sig: 'ppp',
  vnvm_fetch: (name, lenp) => {
    name = UTF8ToString(name >>> 0);
    // (not ENV: each thread has its own copy, filled only on the main one)
#if EXPORT_ES6
    const store = new URL('../store', import.meta.url).href;
#else
    const store = process.env.IPPAI_STORE || 'store';
#endif
    let data;
    try {
      if (name === '@saves') {
        data = ENVIRONMENT_IS_NODE ? (process.env.IPPAI_SAVES ? require('fs').readFileSync(process.env.IPPAI_SAVES) : null) : vnvmSaves;
        if (!data) return 0;
      } else if (ENVIRONMENT_IS_NODE) {
        // (the chunk decoded last, kept: a read asks for its blocks one range at a time)
        const [file, from, len] = ippaiRange(name);
        if (vnvmLastChunk?.[0] === file) data = vnvmLastChunk[1];
        else {
          data = require('fs').readFileSync(require('path').join(store, file));
          if (file.startsWith('chunks/')) vnvmLastChunk = [file, (data = require('zlib').zstdDecompressSync(data))];
        }
        if (from !== undefined) data = data.subarray(from, from + len);
      } else {
        const [file, from, len, block, cls] = ippaiRange(name);
        data = Module.vnvmFiles?.[file] ?? ippaiFetchSync(store + '/' + file, from, len, block, cls);
        if (data.wire !== undefined) wasmExports.vnhost_net_wire(data.wire, data.cached);
        else if (from !== undefined) data = data.subarray(from, from + len);
      }
    } catch (e) {
      if (!ENVIRONMENT_IS_NODE) err(`vnvm_fetch ${name}: ${e}`);
      return 0;
    }
    const p = _malloc(Math.max(data.length, 1));
    if (!p) return 0;
    HEAPU8.set(data, p >>> 0);
    HEAPU32[lenp >>> 2] = data.length;
    if (ENVIRONMENT_IS_NODE && name !== '@saves') wasmExports.vnhost_net_wire(data.length, 0);
    return p;
  },
  // vnvm_fetch_hint(names, len): store files (newline-separated, as vnvm_fetch's) the game reads
  // soon (crates/kura/src/vfs.rs hints): fetched onto the device by the network worker, not
  // waited for (host/fetch-bridge.js ippaiHint). node: nothing (the store is on the disk).
  vnvm_fetch_hint: (names, len) => {
    if (ENVIRONMENT_IS_NODE) return;
#if EXPORT_ES6
    const store = new URL('../store', import.meta.url).href;
#else
    const store = 'store';
#endif
    const s = UTF8ToString(names >>> 0, len >>> 0);
    ippaiFetchHint(s.split('\n').filter(Boolean).map((n) => store + '/' + n));
  },
  // kura::fs::hard_link of two host paths (node, NODERAWFS: emscripten's own link answers
  // EMLINK): 0; -1 the page (no host files), -2 the new name exists, -3 the file is missing,
  // -4 another error
  vnhost_hard_link__sig: 'ipp',
  vnhost_hard_link: (a, b) => {
    if (!ENVIRONMENT_IS_NODE) return -1;
    try {
      require('fs').linkSync(UTF8ToString(a >>> 0), UTF8ToString(b >>> 0));
      return 0;
    } catch (e) {
      return e.code === 'EEXIST' ? -2 : e.code === 'ENOENT' ? -3 : -4;
    }
  },
});
