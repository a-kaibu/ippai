import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import { Worker } from "node:worker_threads";
import { test } from "node:test";
import vm from "node:vm";

const source = await readFile(new URL("../host/account-bridge.js", import.meta.url), "utf8");
const size = 8 + 2 * (320 + 1024);
test("account input is consumed by exactly one concurrent process and erased", async () => {
  const shared = new SharedArrayBuffer(size);
  const bytes = new Uint8Array(shared, 4);
  bytes.fill(0x61);
  Atomics.store(new Int32Array(shared), 0, 1);
  const results = await Promise.all(Array.from({ length: 8 }, () => new Promise((resolve, reject) => {
    const worker = new Worker(`
      const {parentPort,workerData}=require('node:worker_threads');
      const vm=require('node:vm');let library;
      const heap=new Uint8Array(${size});
      vm.runInNewContext(workerData.source, {addToLibrary:l=>library=l,vnvmAccount:workerData.shared,HEAPU8:heap,Int32Array,Uint8Array,Atomics});
      const consumed=library.sake_account_take(0,${size - 4});
      parentPort.postMessage({consumed,byte:heap[0]});
    `, { eval: true, workerData: { shared, source } });
    worker.once("message", resolve);
    worker.once("error", reject);
  })));
  assert.equal(results.filter((r) => r.consumed).length, 1);
  assert.equal(results.find((r) => r.consumed).byte, 0x61);
  assert.equal(bytes.every((b) => b === 0), true);
  assert.equal(Atomics.load(new Int32Array(shared), 0), 3);
});
test("incorrect buffer capacity leaves input untouched", () => {
  const shared = new SharedArrayBuffer(size);
  Atomics.store(new Int32Array(shared), 0, 1);
  let library;
  vm.runInNewContext(source, { addToLibrary: (l) => library = l, vnvmAccount: shared, HEAPU8: new Uint8Array(size), Int32Array, Uint8Array, Atomics });
  assert.equal(library.sake_account_take(0, 1), 0);
  assert.equal(Atomics.load(new Int32Array(shared), 0), 1);
});
test("runtime error status carries only a bounded numeric code", () => {
  const shared = new SharedArrayBuffer(size);
  Atomics.store(new Int32Array(shared), 0, 3);
  let library;
  vm.runInNewContext(source, { addToLibrary: (l) => library = l, vnvmAccount: shared, Int32Array, Atomics });
  library.sake_account_status(1);
  assert.equal(Atomics.load(new Int32Array(shared), 0), -2);
  library.sake_account_status(65536);
  assert.equal(Atomics.load(new Int32Array(shared), 0), -2);
  library.sake_account_status(-1);
  assert.equal(Atomics.load(new Int32Array(shared), 0), -2);
});
