// host/node-stdio.js: sake under node waits for a full stdout/stderr pipe instead of failing
// with EAGAIN (eprintln!'s panic, "failed printing to stderr: Resource temporarily unavailable":
// shlwapi:url's 13000 failure lines ended there when other tests ran beside it).
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFileSync, writeFileSync, mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import vm from "node:vm";

const script = new URL("../host/node-stdio.js", import.meta.url).pathname;
const source = readFileSync(script, "utf8");

// fs.writeSync of a fake fs: `fails` EAGAINs, then writes
function fakeFs(fails, code = "EAGAIN") {
  const calls = [];
  const fs = {
    writeSync(fd, buf) {
      calls.push(fd);
      if (fails-- > 0) throw Object.assign(new Error(code), { code });
      return buf.length;
    },
  };
  vm.runInNewContext(source, { process, require: () => fs, SharedArrayBuffer, Int32Array, Atomics });
  return { fs, calls };
}

test("a write to stdout or stderr that finds the pipe full is tried again until it goes", () => {
  for (const fd of [1, 2]) {
    const { fs, calls } = fakeFs(3);
    assert.equal(fs.writeSync(fd, "abc"), 3);
    assert.deepEqual(calls, [fd, fd, fd, fd]);
  }
});

test("other files and other errors are as fs gives them", () => {
  const f = fakeFs(1);
  assert.throws(() => f.fs.writeSync(7, "abc"), { code: "EAGAIN" });
  assert.deepEqual(f.calls, [7]);
  const g = fakeFs(1, "EPIPE");
  assert.throws(() => g.fs.writeSync(2, "abc"), { code: "EPIPE" });
  // (loaded twice, as the main thread and a worker of one process may: wrapped once)
  const h = fakeFs(0);
  const once = h.fs.writeSync;
  vm.runInNewContext(source, { process, require: () => h.fs, SharedArrayBuffer, Int32Array, Atomics });
  assert.equal(h.fs.writeSync, once);
});

// a real pipe: node makes it non-blocking, and the reader does not read for half a second
function run(patch) {
  const dir = mkdtempSync(join(tmpdir(), "node-stdio-"));
  const child = join(dir, "child.cjs");
  writeFileSync(child, `${patch ? `require(${JSON.stringify(script)});` : ""}
const fs = require("fs");
process.stderr;
const line = Buffer.alloc(4096, 0x61);
let n = 0;
for (let i = 0; i < 4096; i++) n += fs.writeSync(2, line);
fs.writeSync(1, String(n));
`);
  return new Promise((done) => {
    const c = spawn(process.execPath, [child], { stdio: ["ignore", "pipe", "pipe"] });
    let err = 0, out = "";
    c.stderr.pause();
    setTimeout(() => {
      c.stderr.on("data", (d) => (err += d.length));
      c.stderr.resume();
    }, 500);
    c.stdout.on("data", (d) => (out += d));
    c.on("close", (code) => done({ code, out, err }));
  });
}

test("16 MB to a pipe that is not read for a while: all of it, exit 0", async () => {
  // (without it node ends the program: the pipe is full and the write fails)
  const bare = await run(false);
  assert.notEqual(bare.code, 0);
  const r = await run(true);
  assert.deepEqual(r, { code: 0, out: String(16 << 20), err: 16 << 20 });
});
