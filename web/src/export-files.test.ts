import { expect, test } from "vite-plus/test";
import { installationPath, portablePath, selectRecords } from "./file-records";
import { exportFiles, listFiles, type SavesModule } from "./saves";

// exportFiles over a model of kura's vnhost_vfs_list / vnhost_vfs_export (crates/kura/src/vfs.rs:
// the same record layouts, a root being a directory or one file): with a filter, the files it
// drops are never copied out of the module (#234: an installer's 220 MB temp 7z exported with
// the saves ran the page out of memory).

type Files = Map<string, { data: Uint8Array; mtime: number }>;

function fake(files: Files) {
  const mem = new WebAssembly.Memory({ initial: 64 });
  let top = 1024;
  const exported: string[] = [];
  const malloc = (n: number) => {
    const p = top;
    top += Math.ceil(Math.max(n, 1) / 8) * 8;
    while (top > mem.buffer.byteLength) mem.grow(16);
    return p;
  };
  const str = (p: number) => {
    const b = new Uint8Array(mem.buffer);
    let e = p;
    while (b[e]) e++;
    return new TextDecoder().decode(b.subarray(p, e));
  };
  const under = (roots: string) => {
    const out: string[] = [];
    for (const r of roots.split(":").filter(Boolean))
      for (const p of [...files.keys()].sort()) if (p === r || p.startsWith(`${r}/`)) out.push(p);
    return out;
  };
  const give = (parts: Uint8Array[], lenp: number) => {
    const n = parts.reduce((a, b) => a + b.length, 0);
    const p = malloc(n);
    let at = p;
    for (const b of parts) {
      new Uint8Array(mem.buffer).set(b, at);
      at += b.length;
    }
    new Uint32Array(mem.buffer, lenp, 1)[0] = n;
    return p;
  };
  const u32 = (n: number) => new Uint8Array(new Uint32Array([n]).buffer);
  const m: SavesModule = {
    wasmMemory: mem,
    _malloc: malloc,
    _vnhost_free: () => {},
    _vnhost_vfs_list(roots, lenp) {
      const parts: Uint8Array[] = [];
      for (const p of under(str(roots))) {
        const path = new TextEncoder().encode(p);
        parts.push(
          u32(path.length),
          path,
          new Uint8Array(new BigUint64Array([BigInt(files.get(p)!.data.length)]).buffer),
        );
      }
      return give(parts, lenp);
    },
    _vnhost_vfs_export(roots, lenp) {
      const parts: Uint8Array[] = [];
      for (const p of under(str(roots))) {
        exported.push(p);
        const f = files.get(p)!;
        const path = new TextEncoder().encode(p);
        parts.push(
          u32(path.length),
          path,
          new Uint8Array(new BigInt64Array([BigInt(f.mtime)]).buffer),
          u32(f.data.length),
          f.data,
        );
      }
      return give(parts, lenp);
    },
  };
  return { m, exported };
}

const C = "/ippai/prefix/drive_c";

test("the installer's temp 7z and the installed exes are not exported with the saves or the installation", () => {
  const files: Files = new Map([
    [`${C}/windows/temp/nsx1.tmp/app-64.7z`, { data: new Uint8Array(5000), mtime: 1 }],
    [`${C}/users/user/AppData/Local/Programs/am/am.exe`, { data: new Uint8Array(4000), mtime: 2 }],
    [`${C}/users/user/Documents/save/s1.dat`, { data: new Uint8Array([1, 2, 3]), mtime: 3 }],
    [`${C}/ProgramData/rt/config.ini`, { data: new Uint8Array([4]), mtime: 4 }],
  ]);
  const { m, exported } = fake(files);
  const saves = exportFiles(m, [`${C}/users`], portablePath);
  expect(exported).toEqual([`${C}/users/user/Documents/save/s1.dat`]);
  expect(saves.length).toBe(16 + `${C}/users/user/Documents/save/s1.dat`.length + 3);
  exported.length = 0;
  exportFiles(m, [C], installationPath);
  expect(exported).toEqual([`${C}/ProgramData/rt/config.ini`]);
  exported.length = 0;
  expect(exportFiles(m, [`${C}/windows`], installationPath).length).toBe(0);
  expect(exported).toEqual([]);
});

test("property: a filtered export is the full export's records the filter keeps, and copies nothing else", () => {
  const names = ["a.dat", "b.exe", "temp", "c d.sav", "データ.bin", "x.DLL", "microsoft", "crypto", "sub"];
  let seed = 7;
  const rnd = (n: number) => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff), seed % n);
  for (let run = 0; run < 200; run++) {
    const files: Files = new Map();
    for (let i = rnd(12); i > 0; i--) {
      const depth = 1 + rnd(4);
      const p = `${C}/${Array.from({ length: depth }, () => names[rnd(names.length)]).join("/")}`;
      // (no file is also a directory: a tree)
      if ([...files.keys()].some((q) => q.startsWith(`${p}/`) || p.startsWith(`${q}/`))) continue;
      files.set(p, { data: Uint8Array.from({ length: rnd(40) }, () => rnd(256)), mtime: rnd(1000) - 500 });
    }
    const roots = [C, `${C}/${names[rnd(names.length)]}`].slice(0, 1 + rnd(2));
    for (const keep of [portablePath, installationPath, () => true, () => false]) {
      const { m, exported } = fake(files);
      const want = selectRecords(exportFiles(m, roots), keep)!;
      exported.length = 0;
      const got = exportFiles(m, roots, keep);
      expect(got).toEqual(want);
      expect(exported.every(keep)).toBe(true);
      expect(new Set(listFiles(m, roots).filter(keep))).toEqual(new Set(exported));
    }
  }
});
