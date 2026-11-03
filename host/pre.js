// The program's environment: node's, or in the page the object it passes as `vnvmEnv`.
Module.preRun = Module.preRun || [];
Module.preRun.push(() => {
  if (ENVIRONMENT_IS_NODE) Object.assign(ENV, process.env);
  if (Module.vnvmEnv) Object.assign(ENV, Module.vnvmEnv);
});

// Regions an earlier run saved (crates/vnhost/src/jit.rs): `vnvmJit` = { parts: [{ module, idx }] }
// from the page, or under node VNVM_JIT_FILE.wasm/.idx. Every thread gets them in the message
// that loads it ('load', see host/vnjit.js).
var vnvmJitBundle = null;
Module.preInit = [].concat(Module.preInit || [], () => {
  if (ENVIRONMENT_IS_PTHREAD) return;
  const f = ENVIRONMENT_IS_NODE && process.env.VNVM_JIT_FILE;
  if (f && !Module.vnvmJit) {
    try {
      const fs = require('fs');
      Module.vnvmJit = { parts: [{ module: new WebAssembly.Module(fs.readFileSync(f + '.wasm')), idx: new Uint8Array(fs.readFileSync(f + '.idx')) }] };
    } catch (e) {
      if (e.code != 'ENOENT') err(`vnjit: ${f}: ${e}`);
    }
  }
  vnvmJitBundle = Module.vnvmJit || null;
});
if (!ENVIRONMENT_IS_PTHREAD) {
  // the main thread adds them to each worker's 'load' message
  const W = ENVIRONMENT_IS_NODE ? require('worker_threads').Worker : globalThis.Worker;
  if (W && !W.prototype.vnvmPost) {
    W.prototype.vnvmPost = W.prototype.postMessage;
    W.prototype.postMessage = function (m, ...rest) {
      if (m && m.cmd === 'load' && vnvmJitBundle) m = { ...m, vnvmJit: vnvmJitBundle };
      return this.vnvmPost(m, ...rest);
    };
  }
} else {
  const take = (d) => {
    if (d && d.cmd === 'load' && d.vnvmJit) vnvmJitBundle = d.vnvmJit;
  };
  if (ENVIRONMENT_IS_NODE) require('worker_threads').parentPort.on('message', take);
  else self.addEventListener('message', (e) => take(e.data));
}
