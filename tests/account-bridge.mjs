import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { Worker } from "node:worker_threads";
import { test } from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../host/account-bridge.js", import.meta.url), "utf8");
const size = 8 + 2 * (320 + 1024);
test("account input is consumed by exactly one concurrent process and erased", async () => {
  const shared = new SharedArrayBuffer(size);
  const flag = new SharedArrayBuffer(4);
  const bytes = new Uint8Array(shared, 4);
  bytes.fill(0x61);
  Atomics.store(new Int32Array(shared), 0, 1);
  const results = await Promise.all(Array.from({ length: 8 }, () => new Promise((resolve, reject) => {
    const worker = new Worker(`
      const {parentPort,workerData}=require('node:worker_threads');
      const vm=require('node:vm');let library;
      const heap=new Uint8Array(${size});
      vm.runInNewContext(workerData.source, {addToLibrary:l=>library=l,vnvmAccount:workerData.shared,vnvmSensitive:workerData.flag,HEAPU8:heap,Int32Array,Uint8Array,Atomics});
      const consumed=library.sake_account_take(0,${size - 4});
      parentPort.postMessage({consumed,byte:heap[0]});
    `, { eval: true, workerData: { shared, source, flag } });
    worker.once("message", resolve);
    worker.once("error", reject);
  })));
  assert.equal(results.filter((r) => r.consumed).length, 1);
  assert.equal(results.find((r) => r.consumed).byte, 0x61);
  assert.equal(bytes.every((b) => b === 0), true);
  assert.equal(Atomics.load(new Int32Array(shared), 0), 3);
  // (the run holds the credentials now: no crash logs or memory states from here)
  assert.equal(Atomics.load(new Int32Array(flag), 0), 1);
});
test("only a taken input or a persisted key marks the run sensitive", () => {
  const shared = new SharedArrayBuffer(size), flag = new SharedArrayBuffer(4);
  let library;
  vm.runInNewContext(source, { addToLibrary: (l) => library = l, vnvmAccount: shared, vnvmSensitive: flag, HEAPU8: new Uint8Array(size), Int32Array, Uint8Array, Atomics });
  // (no input offered: nothing taken, nothing marked)
  assert.equal(library.sake_account_take(0, size - 4), 0);
  assert.equal(Atomics.load(new Int32Array(flag), 0), 0);
  library.sake_mark_sensitive();
  assert.equal(Atomics.load(new Int32Array(flag), 0), 1);
  vm.runInNewContext(source, { addToLibrary: (l) => library = l, vnvmAccount: null, vnvmSensitive: null, Int32Array, Atomics });
  library.sake_mark_sensitive();
});
test("incorrect buffer capacity leaves input untouched", () => {
  const shared = new SharedArrayBuffer(size);
  Atomics.store(new Int32Array(shared), 0, 1);
  let library;
  vm.runInNewContext(source, { addToLibrary: (l) => library = l, vnvmAccount: shared, vnvmSensitive: null, HEAPU8: new Uint8Array(size), Int32Array, Uint8Array, Atomics });
  assert.equal(library.sake_account_take(0, 1), 0);
  assert.equal(Atomics.load(new Int32Array(shared), 0), 1);
});
test("runtime error status carries only a bounded numeric code", () => {
  const shared = new SharedArrayBuffer(size);
  Atomics.store(new Int32Array(shared), 0, 3);
  let library;
  vm.runInNewContext(source, { addToLibrary: (l) => library = l, vnvmAccount: shared, vnvmSensitive: null, Int32Array, Atomics });
  library.sake_account_status(1);
  assert.equal(Atomics.load(new Int32Array(shared), 0), -2);
  library.sake_account_status(65536);
  assert.equal(Atomics.load(new Int32Array(shared), 0), -2);
  library.sake_account_status(-1);
  assert.equal(Atomics.load(new Int32Array(shared), 0), -2);
});
test("an unverified runtime build leaves the input untaken and says so", () => {
  const shared = new SharedArrayBuffer(size);
  new Uint8Array(shared, 4).fill(0x61);
  Atomics.store(new Int32Array(shared), 0, 1);
  let library;
  vm.runInNewContext(source, { addToLibrary: (l) => library = l, vnvmAccount: shared, vnvmSensitive: null, HEAPU8: new Uint8Array(size), Int32Array, Uint8Array, Atomics });
  library.sake_account_unverified();
  assert.equal(Atomics.load(new Int32Array(shared), 0), 4);
  // (nothing can be taken afterwards)
  assert.equal(library.sake_account_take(0, size - 4), 0);
  // (an input already taken is not relabeled)
  Atomics.store(new Int32Array(shared), 0, 3);
  library.sake_account_unverified();
  assert.equal(Atomics.load(new Int32Array(shared), 0), 3);
});
