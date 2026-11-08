import { describe, expect, it } from "vite-plus/test";
import { installationPath, joinRecords, portablePath, selectRecords } from "./file-records";
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
  it("keeps private keys and registry local, excluding generated executables from portable exports", () => {
    const save = record("/vnvm/game/save/日本語.dat");
    const key = record("/vnvm/prefix/drive_c/users/user/AppData/Roaming/Microsoft/Crypto/RSA/sake/key");
    const registry = record("/vnvm/prefix/drive_c/users/user/sake-registry.tsv");
    const exe = record("/vnvm/game/MAJOKOI.exe");
    const all = joinRecords(joinRecords(save, key), joinRecords(registry, exe));
    expect(selectRecords(all, portablePath)).toEqual(save);
    expect(selectRecords(all, installationPath)).toEqual(joinRecords(key, registry));
  });
  it("does not persist runtime helpers or paths outside the installation", () => {
    expect(installationPath("/vnvm/prefix/drive_c/windows/temp/helper.dat")).toBe(false);
    expect(installationPath("/vnvm/prefix/drive_c/../game/state")).toBe(false);
    expect(installationPath("/vnvm/game/config.dat")).toBe(false);
    expect(installationPath("/vnvm/prefix/drive_c/ProgramData/SoftDenchi/state")).toBe(true);
  });
  it("rejects truncation without returning partial installation data", () => {
    const data = record("/vnvm/prefix/drive_c/users/user/sake-registry.tsv");
    for (let i = 1; i < data.length; i++)
      expect(() => selectRecords(data.slice(0, i), installationPath)).toThrow();
  });
});
