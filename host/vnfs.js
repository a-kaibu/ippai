// vnvm_fetch(name, &len): one file of the store (trees/<name>.tsv or chunks/ab/<hash>, the
// latter zstd-compressed), for crates/vnhost/src/vfs.rs; or "@saves", the saved games the page
// passed (host/pre.js; node: the file $VNVM_SAVES). Returns a malloc'ed buffer, or 0. The bytes
// that came over the network (compressed; none from the browser's HTTP cache) go to vfs.rs's
// counters (vnhost_net_wire).
//
// node: read from $VNVM_STORE. Browser (a worker, so synchronous XHR is allowed): fetch from
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
        const x = new XMLHttpRequest();
        const url = store + '/' + name;
        x.open('GET', url, false);
        // a binary responseType is refused for synchronous requests on the page's own thread
        // (the trees are read there); workers take the bytes as they are
        const doc = typeof document != 'undefined';
        if (doc) x.overrideMimeType('text/plain; charset=x-user-defined');
        else x.responseType = 'arraybuffer';
        x.send();
        if (x.status !== 200) throw new Error(`HTTP ${x.status}`);
        // what came over the network: the resource timing entries of this thread's requests
        // (an entry may come in after its request returns: the next request counts it)
        const perf = typeof performance != 'undefined' && performance.getEntriesByType ? performance : null;
        if (!perf) wasmExports.vnhost_net_wire(+(x.getResponseHeader('content-length') || 0), 0);
        else {
          for (const e of perf.getEntriesByType('resource'))
            if (e.initiatorType === 'xmlhttprequest' && e.name.includes('/store/')) wasmExports.vnhost_net_wire(e.transferSize, e.transferSize === 0 && e.decodedBodySize > 0);
          perf.clearResourceTimings();
        }
        if (doc) {
          const t = x.responseText;
          data = new Uint8Array(t.length);
          for (let i = 0; i < t.length; i++) data[i] = t.charCodeAt(i);
        } else data = new Uint8Array(x.response);
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
