import { describe, expect, it } from "vite-plus/test";
import { installationPath, joinRecords, portablePath, selectRecords, withRegistry } from "./file-records";
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
    const save = record("/vnvm/game/save/日本語.dat");
    const key = record("/vnvm/prefix/drive_c/users/user/AppData/Roaming/Microsoft/Crypto/RSA/sake/key");
    const registry = record("/vnvm/prefix/drive_c/users/user/sake-registry.tsv");
    const appdata = record("/vnvm/prefix/drive_c/users/user/AppData/Roaming/Maker/save.dat");
    const machine = record("/vnvm/prefix/drive_c/ProgramData/SoftDenchi/state");
    const exe = record("/vnvm/game/MAJOKOI.exe");
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
        const p = `/vnvm/prefix/drive_c/users/${user}/${n}`;
        expect(installationPath(p) && portablePath(p)).toBe(false);
      }
  });
  it("gives the saves an older installation's registry only when they have none", () => {
    const old = record("/vnvm/prefix/drive_c/users/user/sake-registry.tsv", new Uint8Array([9]));
    const own = record("/vnvm/prefix/drive_c/users/user/sake-registry.tsv", new Uint8Array([7]));
    const save = record("/vnvm/game/save/a.dat");
    expect(withRegistry(save, old)).toEqual(joinRecords(old, save));
    expect(withRegistry(joinRecords(save, own), old)).toEqual(joinRecords(save, own));
    expect(withRegistry(null, old)).toEqual(old);
    expect(withRegistry(save, null)).toEqual(save);
  });
  it("does not persist runtime helpers or paths outside the installation", () => {
    expect(installationPath("/vnvm/prefix/drive_c/windows/temp/helper.dat")).toBe(false);
    expect(installationPath("/vnvm/prefix/drive_c/../game/state")).toBe(false);
    expect(installationPath("/vnvm/game/config.dat")).toBe(false);
    expect(installationPath("/vnvm/prefix/drive_c/ProgramData/SoftDenchi/state")).toBe(true);
    expect(installationPath("/vnvm/prefix/drive_c/users/user/sake-registry.tsv")).toBe(false);
  });
  it("rejects truncation without returning partial installation data", () => {
    const data = record("/vnvm/prefix/drive_c/users/user/sake-registry.tsv");
    for (let i = 1; i < data.length; i++)
      expect(() => selectRecords(data.slice(0, i), installationPath)).toThrow();
  });
});
