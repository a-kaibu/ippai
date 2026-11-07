// Save states under node (the page's are web/src/savestate.ts; docs/savestate.md): the
// memory image of crates/kura/src/snap.rs, as a file.
//
//   IPPAI_SNAP_SAVE=<file>@<s>[,<s>...]  take a state <s> seconds after the start (each time
//                                       to <file>, or <file>.<n> for the n-th after the first)
//   IPPAI_SNAP_EXIT=1                   end the process right after the (last) state
//   IPPAI_SNAP_LOAD=<file>              start from a state instead of the program's beginning
//
// File: "IPPAISNP", u32 version (1), u32 header length, the header (JSON: build, ctl, ...),
// then blocks (u32 address, u32 length, u32 compressed length, a zstd frame), the end marked
// by an address of 0xffffffff. Blocks of zeros are left out (a new memory is zero).
var vnvmSnap = {
  MAGIC: 'IPPAISNP',
  BLOCK: 1 << 20,
  // Ctl's offsets (crates/kura/src/snap.rs)
  CMD: 8, STATE: 12, RESTORING: 24, RANGES: 28, NRANGES: 32, MSG: 36, MSG_LEN: 40, STIRRED: 88,
  build() {
    const fs = require('fs');
    // (this script's own: sake.wasm or sake-grow.wasm)
    const wasm = __filename.replace(/\.js$/, '.wasm');
    return require('crypto').createHash('sha256').update(fs.readFileSync(wasm)).digest('hex').slice(0, 16);
  },
  zero(u8, at, n) {
    const w = new BigUint64Array(u8.buffer, at, n >>> 3);
    for (let i = 0; i < w.length; i++) if (w[i] !== 0n) return false;
    return true;
  },
  // the blocks of `ranges` ([addr, len]...) that are not all zero: [addr, len]
  blocks(u8, ranges) {
    const out = [];
    for (let r = 0; r < ranges.length; r += 2) {
      const end = ranges[r] + ranges[r + 1];
      for (let a = ranges[r]; a < end; a += vnvmSnap.BLOCK) {
        const n = Math.min(vnvmSnap.BLOCK, end - a);
        if (!vnvmSnap.zero(u8, a, n)) out.push([a, n]);
      }
    }
    return out;
  },
  // asks the module for a state; resolves with the file's bytes (or rejects with the reason).
  // `stay`: the program is left standing (the process ends after it).
  async take(header, stay) {
    const zlib = require('zlib');
    const ctl = wasmExports.vnhost_snap_ctl() >>> 0;
    const i32 = new Int32Array(wasmMemory.buffer);
    Atomics.store(i32, (ctl + vnvmSnap.CMD) >>> 2, 1);
    Atomics.notify(i32, (ctl + vnvmSnap.CMD) >>> 2);
    const t0 = Date.now();
    let st;
    while ((st = Atomics.load(i32, (ctl + vnvmSnap.STATE) >>> 2)) !== 2 && st !== 3) await new Promise((r) => setTimeout(r, 5));
    const u8 = new Uint8Array(wasmMemory.buffer);
    if (st === 3) {
      const p = i32[(ctl + vnvmSnap.MSG) >>> 2] >>> 0, n = i32[(ctl + vnvmSnap.MSG_LEN) >>> 2];
      throw new Error(Buffer.from(u8.subarray(p, p + n)).toString());
    }
    const rp = i32[(ctl + vnvmSnap.RANGES) >>> 2] >>> 0, rn = i32[(ctl + vnvmSnap.NRANGES) >>> 2];
    const ranges = Array.from(new Uint32Array(wasmMemory.buffer, rp, rn * 2));
    const parts = [];
    let raw = 0;
    for (const [a, n] of vnvmSnap.blocks(u8, ranges)) {
      const c = zlib.zstdCompressSync(u8.subarray(a, a + n), { params: { [zlib.constants.ZSTD_c_compressionLevel]: 1 } });
      const h = Buffer.alloc(12);
      h.writeUInt32LE(a, 0);
      h.writeUInt32LE(n, 4);
      h.writeUInt32LE(c.length, 8);
      parts.push(h, c);
      raw += n;
    }
    const took = Date.now() - t0;
    if (!stay) {
      Atomics.store(i32, (ctl + vnvmSnap.CMD) >>> 2, 2);
      Atomics.notify(i32, (ctl + vnvmSnap.CMD) >>> 2);
      // (a thread that ran while copied may have left a lock held in the copy: not kept)
      while (Atomics.load(i32, (ctl + vnvmSnap.STATE) >>> 2) !== 0) await new Promise((r) => setTimeout(r, 2));
      if (Atomics.load(i32, (ctl + vnvmSnap.STIRRED) >>> 2)) throw new Error('a thread ran while the state was copied');
    }
    const head = Buffer.from(JSON.stringify({ format: 1, build: vnvmSnap.build(), ctl, raw, pages: wasmMemory.buffer.byteLength / 65536, created: new Date().toISOString(), ...header }));
    const pre = Buffer.alloc(16);
    pre.write(vnvmSnap.MAGIC, 0, 'latin1');
    pre.writeUInt32LE(1, 8);
    pre.writeUInt32LE(head.length, 12);
    const end = Buffer.alloc(4);
    end.writeUInt32LE(0xffffffff, 0);
    const file = Buffer.concat([pre, head, ...parts, end]);
    err(`snap: state of ${(raw / 1048576).toFixed(1)} MiB in ${(file.length / 1048576).toFixed(1)} MiB, the program stood still ${took} ms`);
    return file;
  },
  // a memory with the state in it (and Ctl.restoring set)
  load(file) {
    const zlib = require('zlib');
    if (file.toString('latin1', 0, 8) !== vnvmSnap.MAGIC || file.readUInt32LE(8) !== 1) throw new Error('not a state');
    const hl = file.readUInt32LE(12);
    const head = JSON.parse(file.toString('utf8', 16, 16 + hl));
    if (head.build !== vnvmSnap.build()) throw new Error(`the state is of another build (${head.build})`);
    // (as large as it was: sake-grow's may have grown; crates/sake/build.sh)
    const mem = new WebAssembly.Memory({ initial: head.pages || 65535, maximum: 65535, shared: true });
    const u8 = new Uint8Array(mem.buffer);
    let at = 16 + hl;
    for (;;) {
      const a = file.readUInt32LE(at);
      if (a === 0xffffffff) break;
      const n = file.readUInt32LE(at + 4), c = file.readUInt32LE(at + 8);
      const d = zlib.zstdDecompressSync(file.subarray(at + 12, at + 12 + c));
      if (d.length !== n) throw new Error(`block at ${a.toString(16)}: ${d.length} bytes, not ${n}`);
      u8.set(d, a);
      at += 12 + c;
    }
    new Uint32Array(mem.buffer)[(head.ctl + vnvmSnap.RESTORING) >>> 2] = 1;
    return mem;
  },
};
// (the first process only: the ones it starts, host/node-main.js, have the host's kernel)
if (ENVIRONMENT_IS_NODE && !ENVIRONMENT_IS_PTHREAD && !Module.vnvmKernel) {
  // (node 22's V8 12.4 drops i32 atomic stores to constant addresses at 2 GiB and above in its
  // first tier, and the host's statics are all there: a store to a static atomic is lost)
  if (+process.versions.node.split('.')[0] < 26) err(`sake: node ${process.versions.node}: use node 26 or later (node 22 loses atomic stores)`);
  if (process.env.IPPAI_SNAP_LOAD) {
    Module.wasmMemory = vnvmSnap.load(require('fs').readFileSync(process.env.IPPAI_SNAP_LOAD));
  }
  const save = process.env.IPPAI_SNAP_SAVE;
  if (save) {
    const at = save.lastIndexOf('@');
    const path = save.slice(0, at);
    const times = save.slice(at + 1).split(',').map(Number);
    Module.preRun = Module.preRun || [];
    Module.preRun.push(() => {
      times.forEach((s, k) =>
        setTimeout(async () => {
          const out = k ? `${path}.${k}` : path;
          const last = k === times.length - 1 && process.env.IPPAI_SNAP_EXIT;
          try {
            require('fs').writeFileSync(out, await vnvmSnap.take({ at: s }, last));
            err(`snap: saved ${out}`);
          } catch (e) {
            err(`snap: ${out}: ${e.message}`);
          }
          if (last) process.exit(0);
        }, s * 1000),
      );
    });
  }
}
