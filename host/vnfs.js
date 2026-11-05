// vnvm_fetch(name, &len): one file of the store (trees/<name>.tsv or chunks/ab/<hash>, the
// latter zstd-compressed), for crates/kura/src/vfs.rs; or "@saves", the saved games the page
// passed (host/pre.js; node: the file $VNVM_SAVES). Returns a malloc'ed buffer, or 0. The bytes
// that came over the network (compressed; none from the browser's HTTP cache) go to vfs.rs's
// counters (vnhost_net_wire).
//
// node: read from $VNVM_STORE. Browser: a dedicated worker uses fetch; the game pthread waits for its response from
// store/ beside the directory of sake.js; the server sends chunks with "Content-Encoding: zstd",
// so the browser decompresses them.
addToLibrary({
  vnvm_fetch__deps: ['malloc'],
  // pointers are above 2GB (GLOBAL_BASE): 'p' makes them unsigned
  vnvm_fetch__sig: 'ppp',
  vnvm_fetch: (name, lenp) => {
    name = UTF8ToString(name >>> 0);
    // (not ENV: each thread has its own copy, filled only on the main one)
#if EXPORT_ES6
    const store = new URL('../store', import.meta.url).href;
#else
    const store = process.env.VNVM_STORE || 'store';
#endif
    let data;
    try {
      if (name === '@saves') {
        data = ENVIRONMENT_IS_NODE ? (process.env.VNVM_SAVES ? require('fs').readFileSync(process.env.VNVM_SAVES) : null) : vnvmSaves;
        if (!data) return 0;
      } else if (ENVIRONMENT_IS_NODE) {
        data = require('fs').readFileSync(require('path').join(store, name));
        if (name.startsWith('chunks/')) data = require('zlib').zstdDecompressSync(data);
      } else {
        data = Module.vnvmFiles?.[name] ?? ippaiFetchSync(store + '/' + name);
        if (data.wire !== undefined) wasmExports.vnhost_net_wire(data.wire, data.cached);
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
});
