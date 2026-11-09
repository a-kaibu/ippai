import { describe, expect, it } from "vite-plus/test";
import {
  installationPath,
  joinRecords,
  legacyNames,
  movedPath,
  portablePath,
  renameRecords,
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
