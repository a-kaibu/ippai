// The host's JS over a long session (review 2026-11-14, unit 8): host/vnjit.js's table,
// host/pre.js's per-instance thread messages and its end, host/vnfs.js's lock of a process
// that ended holding it, host/tls-bridge.js's dead sockets. Run: node --test tests/host-stability.mjs
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import vm from "node:vm";

const read = (f) => readFileSync(new URL(`../host/${f}`, import.meta.url), "utf8");

/** An emscripten --js-library file's object, its `$names` also made globals of the context. */
function library(file, globals, cut) {
  let lib;
  const context = vm.createContext({ ...globals, addToLibrary: (x) => (lib = x) });
  // (cut: where the file's emscripten preprocessor part begins, left out)
  vm.runInContext(cut ? read(file).split(cut)[0] + "});" : read(file), context);
  for (const [k, v] of Object.entries(lib)) if (k.startsWith("$") && !k.includes("__")) context[k.slice(1)] = v;
  return { lib, context };
}

// ---- host/vnjit.js

/** A wasm module of `n` functions f0.., none importing anything. */
function moduleOf(n) {
  const leb = (v) => {
    const out = [];
    do {
      let b = v & 0x7f;
      v >>>= 7;
      if (v) b |= 0x80;
      out.push(b);
    } while (v);
    return out;
  };
  const section = (id, body) => [id, ...leb(body.length), ...body];
  const exports = [];
  for (let i = 0; i < n; i++) {
    const name = [...Buffer.from(`f${i}`)];
    exports.push(...leb(name.length), ...name, 0, ...leb(i));
  }
  return Uint8Array.from([
    0, 0x61, 0x73, 0x6d, 1, 0, 0, 0,
    ...section(1, [1, 0x60, 0, 0]),
    ...section(3, [...leb(n), ...new Array(n).fill(0)]),
    ...section(7, [...leb(n), ...exports]),
    ...section(10, [...leb(n), ...new Array(n).fill([2, 0, 0x0b]).flat()]),
  ]);
}

function jit(table) {
  const HEAPU8 = new Uint8Array(1 << 16);
  const errors = [];
  const { lib, context } = library("vnjit.js", { HEAPU8, WebAssembly, err: (l) => errors.push(l) });
  context.VNJIT.table = table ?? new WebAssembly.Table({ element: "anyfunc", initial: 0 });
  context.VNJIT.env = {};
  const load = (n) => {
    const b = moduleOf(n);
    HEAPU8.set(b, 0);
    return lib.vnvm_jit_load(0, b.length, n);
  };
  return { lib, VNJIT: context.VNJIT, load, errors };
}

test("vnjit: modules get slots one after another, freed ones are not handed out again", () => {
  const j = jit();
  assert.equal(j.load(10), 0);
  assert.equal(j.load(5), 10);
  assert.equal(typeof j.VNJIT.table.get(12), "function");
  j.lib.vnvm_jit_free(0, 10);
  assert.equal(j.VNJIT.table.get(3), null);
  assert.equal(j.VNJIT.freed, 10);
  // (a stale index in kura's ring must never name another module)
  assert.equal(j.load(3), 15);
});

test("vnjit: a table V8 will not grow answers TABLE_FULL instead of killing the thread", () => {
  const j = jit(new WebAssembly.Table({ element: "anyfunc", initial: 0, maximum: 300 }));
  assert.equal(j.load(200), 0);
  // (the grow throws a RangeError: caught, -2 = crates/kura/src/jit.rs TABLE_FULL)
  assert.equal(j.load(200), -2);
  assert.equal(j.errors.length, 1);
  assert.match(j.errors[0], /vnjit: table of/);
  assert.equal(j.VNJIT.used, 200);
});

test("vnjit: past 2^20 slots with half of them freed, the thread starts over (TABLE_FULL)", () => {
  const fake = (length) => ({ length, grow(n) { this.length += n; }, set() {}, get() { return null; } });
  const j = jit(fake(1 << 20));
  j.VNJIT.used = (1 << 20) - 4;
  j.VNJIT.freed = 1 << 18;
  // (a quarter freed: grown)
  assert.equal(j.load(8), (1 << 20) - 4);
  assert.ok(j.VNJIT.table.length > 1 << 20);
  const k = jit(fake(1 << 20));
  k.VNJIT.used = (1 << 20) - 4;
  k.VNJIT.freed = 1 << 19;
  assert.equal(k.load(8), -2);
  // (init: a fresh table, the counts again from 0)
  assert.equal(k.VNJIT.used, (1 << 20) - 4);
});

// ---- host/pre.js (each instance as emscripten's MODULARIZE has it: its own function scope, the
// realm's Worker class shared)

class FakeWorker {
  constructor() {
    this.sent = [];
  }
  postMessage(m, transfer) {
    this.sent.push({ m, transfer });
  }
  addEventListener() {}
}

function realm() {
  const timers = { set: new Map(), cleared: [], next: 1 };
  const context = vm.createContext({
    ENVIRONMENT_IS_NODE: false,
    ENVIRONMENT_IS_PTHREAD: false,
    Worker: class extends FakeWorker {},
    location: { href: "https://ippai.test/play.html" },
    URL,
    SharedArrayBuffer,
    Atomics,
    Int32Array,
    Uint8Array,
    WebAssembly,
    setInterval: (f) => (timers.set.set(timers.next, f), timers.next++),
    clearInterval: (id) => timers.cleared.push(id),
  });
  context.globalThis = context;
  const source = `(function (Module) {
    var wasmMemory = Module.memory, wasmExports = Module.exports, HEAPU8 = new Uint8Array(64), ENV = {};
    var _malloc = () => 8, printChar = () => {}, err = () => {};
    function ippaiFetchConnect() { return 'fetch-port'; }
    function ippaiTlsConnect() { return 'tls-port'; }
    ${read("pre.js")}
    for (const f of Module.preInit) f();
    return { pid: () => vnvmPid, fs: () => vnvmFs };
  })`;
  const instance = vm.runInContext(source, context);
  return { context, timers, instance };
}

function start(r, pid, extra = {}) {
  const Module = {
    memory: new WebAssembly.Memory({ initial: 1 }),
    exports: { vnhost_output_queue() {}, vnhost_output_take: () => 0 },
    vnvmEnv: pid ? { SAKE_PID: String(pid) } : {},
    ...extra,
  };
  const api = r.instance(Module);
  Module.onRuntimeInitialized?.();
  return { Module, api };
}

test("pre.js: each instance's threads get its own values, found by its memory", () => {
  const r = realm();
  const fs = new SharedArrayBuffer(1 << 12);
  const root = start(r, 0, { vnvmSaves: new Uint8Array([1]), vnvmFs: fs });
  const child = start(r, 0x24, { vnvmSaves: new Uint8Array([2]), vnvmFs: fs });
  assert.equal(root.api.pid(), 0x10);
  assert.equal(child.api.pid(), 0x24);
  const W = r.context.Worker;
  const a = new W(), b = new W();
  b.postMessage({ cmd: "load", wasmMemory: child.Module.memory }, []);
  a.postMessage({ cmd: "load", wasmMemory: root.Module.memory }, []);
  assert.equal(b.sent[0].m.vnvmPid, 0x24);
  assert.deepEqual([...b.sent[0].m.vnvmSaves], [2]);
  assert.equal(a.sent[0].m.vnvmPid, 0x10);
  assert.deepEqual([...a.sent[0].m.vnvmSaves], [1]);
  assert.deepEqual([...b.sent[0].transfer], ["fetch-port", "tls-port"]);
  // (other messages go as they are)
  a.postMessage({ cmd: "run" });
  assert.deepEqual(a.sent[1].m, { cmd: "run" });
});

test("pre.js: an instance that ends stops its output timer and leaves the realm's list", () => {
  const r = realm();
  const root = start(r, 0);
  const child = start(r, 0x30);
  const W = r.context.Worker;
  assert.equal(W.vnvmHosts.size, 2);
  assert.equal(r.timers.set.size, 2);
  child.Module.onExit(0);
  assert.equal(W.vnvmHosts.size, 1);
  assert.deepEqual(r.timers.cleared, [2]);
  root.Module.onAbort("boom");
  assert.equal(W.vnvmHosts.size, 0);
  assert.deepEqual(r.timers.cleared, [2, 1]);
  // (no instance left: a load message goes as it is)
  const w = new W();
  w.postMessage({ cmd: "load" });
  assert.deepEqual(w.sent[0].m, { cmd: "load" });
});

// ---- host/vnfs.js's lock with host/pre.js's exit

function vfsLock(fs, pid) {
  return library("vnfs.js", { vnvmFs: fs, vnvmPid: pid, Atomics, Int32Array, Uint8Array, ENVIRONMENT_IS_PTHREAD: true }, "  vnvm_fetch__deps:").lib;
}

test("vnfs: a process that ends holding the shared file system's lock releases it", () => {
  const r = realm();
  const fs = new SharedArrayBuffer(1 << 12);
  start(r, 0, { vnvmFs: fs });
  const child = start(r, 0x24, { vnvmFs: fs });
  const h = new Int32Array(fs, 0, 8);
  const l = vfsLock(fs, 0x24);
  assert.equal(l.vnhost_vfs_shared_enable(), 1);
  assert.equal(l.vnhost_vfs_shared_lock(), 1);
  // (the lock word holds its holder's process)
  assert.equal(Atomics.load(h, 0), 0x24);
  child.Module.onAbort("crash");
  assert.equal(Atomics.load(h, 0), 0);
  // (and the root can take it at once)
  const root = vfsLock(fs, 0x10);
  assert.equal(root.vnhost_vfs_shared_lock(), 1);
  root.vnhost_vfs_shared_unlock();
});

test("vnfs: a process that ends leaves another process's lock alone", () => {
  const r = realm();
  const fs = new SharedArrayBuffer(1 << 12);
  start(r, 0, { vnvmFs: fs });
  const child = start(r, 0x24, { vnvmFs: fs });
  const h = new Int32Array(fs, 0, 8);
  const root = vfsLock(fs, 0x10);
  root.vnhost_vfs_shared_enable();
  root.vnhost_vfs_shared_lock();
  child.Module.onExit(1);
  assert.equal(Atomics.load(h, 0), 0x10);
  root.vnhost_vfs_shared_unlock();
  assert.equal(Atomics.load(h, 0), 0);
});

// ---- host/tls-bridge.js

test("tls-bridge: a socket that timed out fails every later call at once", () => {
  const HEAPU8 = new Uint8Array(1 << 17);
  const { lib, context } = library("tls-bridge.js", { HEAPU8, Atomics, Int32Array, Uint8Array, Date, ENVIRONMENT_IS_NODE: true });
  const shared = new SharedArrayBuffer(16 + 2 * 65536);
  const worker = { posts: [], ends: 0, postMessage(m) { this.posts.push(m.op); }, terminate() { this.ends++; } };
  context.sakeTlsSockets.sockets.set(7, { worker, ctl: new Int32Array(shared, 0, 4), tx: new Uint8Array(shared, 16, 65536), rx: new Uint8Array(shared, 16 + 65536, 65536) });
  let t0 = Date.now();
  assert.equal(lib.sake_tls_read(7, 0, 65536, 60), -1);
  assert.ok(Date.now() - t0 >= 55);
  assert.equal(worker.ends, 1);
  // (no second wait for an answer that cannot come)
  t0 = Date.now();
  assert.equal(lib.sake_tls_read(7, 0, 65536, 2000), -1);
  assert.equal(lib.sake_tls_write(7, 0, 10, 2000), -1);
  assert.ok(Date.now() - t0 < 100);
  assert.deepEqual(worker.posts, ["recv"]);
  lib.sake_tls_close(7);
  assert.equal(worker.ends, 1);
  assert.equal(context.sakeTlsSockets.sockets.size, 0);
});

test("tls-bridge: a socket that answers keeps working", () => {
  const HEAPU8 = new Uint8Array(1 << 17);
  const { lib, context } = library("tls-bridge.js", { HEAPU8, Atomics, Int32Array, Uint8Array, Date, ENVIRONMENT_IS_NODE: true });
  const shared = new SharedArrayBuffer(16 + 2 * 65536);
  const ctl = new Int32Array(shared, 0, 4), rx = new Uint8Array(shared, 16 + 65536, 65536);
  // (the worker answers at once: 3 bytes)
  const worker = { postMessage() { rx.set([1, 2, 3]); Atomics.store(ctl, 1, 3); Atomics.add(ctl, 0, 1); }, terminate() {} };
  context.sakeTlsSockets.sockets.set(8, { worker, ctl, tx: new Uint8Array(shared, 16, 65536), rx });
  for (let i = 0; i < 3; i++) assert.equal(lib.sake_tls_read(8, 100, 65536, 50), 3);
  assert.deepEqual([...HEAPU8.slice(100, 103)], [1, 2, 3]);
});
