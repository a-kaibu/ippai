import { describe, expect, it, test } from "vite-plus/test";
import {
  installationPath,
  joinRecords,
  legacyNames,
  movedPath,
  portablePath,
  renameRecords,
  same,
  selectRecords,
  withRegistry,
} from "./file-records";
function record(path: string, data: Uint8Array = new Uint8Array([1, 2, 3])) {
  const p = new TextEncoder().encode(path),
    out = new Uint8Array(16 + p.length + data.length),
    v = new DataView(out.buffer);
  v.setUint32(0, p.length, true);
  out.set(p, 4);
  v.setBigInt64(4 + p.length, 123n, true);
  v.setUint32(12 + p.length, data.length, true);
  out.set(data, 16 + p.length);
  return out;
}
describe("installation and portable saves", () => {
  it("keeps private keys local; the registry and the user folders travel with the game's saves", () => {
    const save = record("/ippai/game/save/日本語.dat");
    const key = record("/ippai/prefix/drive_c/users/user/AppData/Roaming/Microsoft/Crypto/RSA/sake/key");
    const registry = record("/ippai/prefix/drive_c/users/user/sake-registry.tsv");
    const appdata = record("/ippai/prefix/drive_c/users/user/AppData/Roaming/Maker/save.dat");
    const machine = record("/ippai/prefix/drive_c/ProgramData/SoftDenchi/state");
    const exe = record("/ippai/game/MAJOKOI.exe");
    const all = [save, key, registry, appdata, machine, exe].reduce((a, b) => joinRecords(a, b)!);
    expect(selectRecords(all, portablePath)).toEqual(
      [save, registry, appdata, machine].reduce((a, b) => joinRecords(a, b)!),
    );
    expect(selectRecords(all, installationPath)).toEqual(joinRecords(key, machine));
  });
  it("never puts one path in both the saves' user folders and the installation", () => {
    // (the installation is applied after the saves: a shared path would let this device's copy
    // override the game's newer saves, and one game's files reach another's)
    const names = [
      "sake-registry.tsv",
      "AppData/Roaming/x/save.dat",
      "AppData/Local/Temp/t",
      "Documents/My Games/a.sav",
      "AppData/Roaming/Microsoft/Crypto/RSA/sake/k/1.key",
      "Saved Games/b",
      "NTUSER.DAT",
      "AppData/Roaming/m/plugin.dll",
    ];
    for (const user of ["user", "Public", "Default"])
      for (const n of names) {
        const p = `/ippai/prefix/drive_c/users/${user}/${n}`;
        expect(installationPath(p) && portablePath(p)).toBe(false);
      }
  });
  it("gives the saves an older installation's registry only when they have none", () => {
    const old = record("/ippai/prefix/drive_c/users/user/sake-registry.tsv", new Uint8Array([9]));
    const own = record("/ippai/prefix/drive_c/users/user/sake-registry.tsv", new Uint8Array([7]));
    const save = record("/ippai/game/save/a.dat");
    expect(withRegistry(save, old)).toEqual(joinRecords(old, save));
    expect(withRegistry(joinRecords(save, own), old)).toEqual(joinRecords(save, own));
    expect(withRegistry(null, old)).toEqual(old);
    expect(withRegistry(save, null)).toEqual(save);
  });
  it("does not persist runtime helpers or paths outside the installation", () => {
    expect(installationPath("/ippai/prefix/drive_c/windows/temp/helper.dat")).toBe(false);
    expect(installationPath("/ippai/prefix/drive_c/../game/state")).toBe(false);
    expect(installationPath("/ippai/game/config.dat")).toBe(false);
    expect(installationPath("/ippai/prefix/drive_c/ProgramData/SoftDenchi/state")).toBe(true);
    expect(installationPath("/ippai/prefix/drive_c/users/user/sake-registry.tsv")).toBe(false);
  });
  it("rejects truncation without returning partial installation data", () => {
    const data = record("/ippai/prefix/drive_c/users/user/sake-registry.tsv");
    for (let i = 1; i < data.length; i++)
      expect(() => selectRecords(data.slice(0, i), installationPath)).toThrow();
  });
});

describe("saves of before the rename (Z:\\vnvm)", () => {
  it("moves /vnvm/game and /vnvm/prefix paths, keeps the rest of each record", () => {
    const data = new Uint8Array([5, 6, 7, 8]);
    const old = [
      record("/vnvm/game/save/日本語.dat", data),
      record("/vnvm/prefix/drive_c/users/user/sake-registry.tsv"),
      record("/other/x"),
      record("/vnvm/gamex/y"),
    ].reduce((a, b) => joinRecords(a, b)!);
    const want = [
      record("/ippai/game/save/日本語.dat", data),
      record("/ippai/prefix/drive_c/users/user/sake-registry.tsv"),
      record("/other/x"),
      record("/vnvm/gamex/y"),
    ].reduce((a, b) => joinRecords(a, b)!);
    expect(renameRecords(old)).toEqual(want);
    // (moved, they are the game's saves and the registry as the new paths are)
    expect(selectRecords(renameRecords(old), portablePath)!.length).toBe(want.length);
    expect(renameRecords(want)).toEqual(want);
    expect(renameRecords(null)).toBeNull();
  });
  it("rejects truncated data", () => {
    const data = record("/vnvm/game/a.dat");
    for (let i = 1; i < data.length; i++) expect(() => renameRecords(data.slice(0, i))).toThrow();
  });
  it("moves paths only at their start", () => {
    expect(movedPath("/vnvm/game")).toBe("/ippai/game");
    expect(movedPath("/vnvm/prefix/drive_c")).toBe("/ippai/prefix/drive_c");
    expect(movedPath("/a/vnvm/game/x")).toBe("/a/vnvm/game/x");
    expect(movedPath("/vnvm/games/x")).toBe("/vnvm/games/x");
  });
  it("names the old saves of a program from before", () => {
    expect(legacyNames("Z:\\ippai\\game\\a.exe", ["/ippai/game=t", "/x=y"])).toEqual({
      run: "Z:\\vnvm\\game\\a.exe",
      mounts: ["/vnvm/game=t", "/x=y"],
    });
    expect(legacyNames("C:\\wintest.exe", ["/x=y"])).toBeNull();
  });
  // seeded random records: moving is the identity on new paths, and the paths alone change
  it("holds for any records", () => {
    let seed = 11;
    const r = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff;
    const parts = ["/vnvm/game", "/vnvm/prefix", "/ippai/game", "/other", "/vnvm/gamey"];
    for (let k = 0; k < 200; k++) {
      const recs = Array.from({ length: Math.floor(r() * 6) }, () => {
        const p = `${parts[Math.floor(r() * parts.length)]}/${Math.floor(r() * 1000)}`;
        return [p, new Uint8Array(Math.floor(r() * 40)).map(() => Math.floor(r() * 256))] as const;
      });
      const join = (xs: Uint8Array<ArrayBuffer>[]) =>
        xs.reduce((a, b) => joinRecords(a, b)!, new Uint8Array(0));
      const got = renameRecords(join(recs.map(([p, d]) => record(p, d))));
      expect(got).toEqual(join(recs.map(([p, d]) => record(movedPath(p), d))));
      expect(renameRecords(got)).toEqual(got);
    }
  });
});

/** A record: path bytes, mtime, data (file-records.ts's layout). */
function rec(path: Uint8Array, data: Uint8Array): Uint8Array {
  const out = new Uint8Array(16 + path.length + data.length);
  const v = new DataView(out.buffer);
  v.setUint32(0, path.length, true);
  out.set(path, 4);
  v.setBigInt64(4 + path.length, 123n, true);
  v.setUint32(12 + path.length, data.length, true);
  out.set(data, 16 + path.length);
  return out;
}

test("records whose paths are any bytes (not UTF-8 too): kept byte for byte, never a throw (property, seeded)", () => {
  let x = 5;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 300; run++) {
    const recs: Uint8Array[] = [];
    for (let i = rnd(6); i > 0; i--) {
      const ascii = rnd(2) ? "/vnvm/game/" : "/ippai/prefix/drive_c/ProgramData/";
      const tail = Uint8Array.from({ length: rnd(12) }, () => (rnd(3) ? 0x61 + rnd(26) : 0x80 + rnd(128)));
      const path = new Uint8Array([...new TextEncoder().encode(ascii), ...tail]);
      recs.push(
        rec(
          path,
          Uint8Array.from({ length: rnd(20) }, () => rnd(256)),
        ),
      );
    }
    const all = joinRecords(new Uint8Array(0), null) ?? new Uint8Array(0);
    const data = new Uint8Array(recs.reduce((a, r) => a + r.length, all.length));
    let at = 0;
    for (const r of recs) data.set(r, (at += r.length) - r.length);
    // (everything kept: the same bytes, the same array; nothing: empty)
    const every = selectRecords(data, () => true)!;
    expect(every).toBe(data);
    expect(selectRecords(data, () => false)!.length).toBe(0);
    // (kept by a test of the decoded path: the kept records whole and in order)
    const want: number[] = [];
    for (const r of recs)
      if (
        new TextDecoder()
          .decode(r.subarray(4, 4 + new DataView(r.buffer).getUint32(0, true)))
          .includes("Program")
      )
        want.push(...r);
    expect(Array.from(selectRecords(data, (p) => p.includes("Program"))!)).toEqual(want);
    // (renamed: a valid UTF-8 /vnvm/ path moved, an odd one left as its bytes; nothing lost)
    const moved = renameRecords(data)!;
    const back: number[] = [];
    for (const r of recs) {
      const n = new DataView(r.buffer).getUint32(0, true);
      const raw = r.subarray(4, 4 + n);
      let text: string | null;
      try {
        text = new TextDecoder("utf-8", { fatal: true }).decode(raw);
      } catch {
        text = null;
      }
      if (text?.startsWith("/vnvm/game/"))
        back.push(...rec(new TextEncoder().encode(text.replace("/vnvm/", "/ippai/")), r.subarray(16 + n)));
      else back.push(...r);
    }
    expect(Array.from(moved)).toEqual(back);
  }
});

test("same: true exactly when the bytes are, at any offsets (property, differential with a byte loop)", () => {
  let x = 9;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 2000; run++) {
    const n = rnd(40);
    const buf = new Uint8Array(n + 16);
    for (let i = 0; i < buf.length; i++) buf[i] = rnd(2) ? 0 : rnd(256);
    const a = buf.subarray(rnd(8), 0).length === 0 ? new Uint8Array(buf.buffer, rnd(8), n) : buf;
    const b = new Uint8Array(new ArrayBuffer(n + 8), rnd(2) ? 0 : rnd(8), n);
    b.set(a);
    // (bytes that floats would call equal: +0 and -0)
    if (n >= 8 && rnd(3) === 0) b[b.length - 1] = a[a.length - 1]! ^ 0x80;
    else if (n && rnd(2)) b[rnd(n)]! ^= 1 << rnd(8);
    const want = a.every((v, i) => v === b[i]);
    expect(same(a, b), `run ${run}`).toBe(want);
    expect(same(a, a)).toBe(true);
  }
  expect(same(null, new Uint8Array(0))).toBe(false);
  expect(same(new Uint8Array(3), new Uint8Array(4))).toBe(false);
});
