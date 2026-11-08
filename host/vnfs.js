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
        const [file, from, len, block] = ippaiRange(name);
        data = Module.vnvmFiles?.[file] ?? ippaiFetchSync(store + '/' + file, from, len, block);
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
});
