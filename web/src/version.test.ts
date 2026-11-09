import { describe, expect, it } from "vite-plus/test";
import { buildLabel } from "./version";

describe("buildLabel", () => {
  it("shows the version and the short commit", () => {
    expect(buildLabel({ version: "0.3.0", commit: "4f67fe6" })).toBe("v0.3.0 · 4f67fe6");
    expect(buildLabel({ version: "0.3.0", commit: "4f67fe6aabbccdd" })).toBe("v0.3.0 · 4f67fe6");
  });
  it("leaves out a commit it does not know", () => {
    expect(buildLabel({ version: "0.3.0", commit: "" })).toBe("v0.3.0");
    expect(buildLabel({ version: "0.3.0" })).toBe("v0.3.0");
    expect(buildLabel({ version: "0.3.0", commit: "<b>x</b>" })).toBe("v0.3.0");
  });
  it("shows nothing for what is not a build", () => {
    for (const v of [
      null,
      undefined,
      3,
      "0.3.0",
      [],
      {},
      { version: 3 },
      { version: "" },
      { version: "a b" },
      { version: "x".repeat(40) },
    ])
      expect(buildLabel(v)).toBe("");
  });
  // seeded random objects: the label is empty or has the form, never anything else
  it("is empty or well formed for any input", () => {
    let s = 0x9e3779b9;
    const rnd = (n: number) => {
      s ^= s << 13;
      s ^= s >>> 17;
      s ^= s << 5;
      return (s >>> 0) % n;
    };
    const alphabet = "0123456789abcdefv.-+ <>·あ";
    const str = () => Array.from({ length: rnd(12) }, () => alphabet[rnd(alphabet.length)]).join("");
    for (let i = 0; i < 2000; i++) {
      const label = buildLabel({ version: str(), commit: rnd(2) ? str() : undefined });
      expect(label === "" || /^v[0-9A-Za-z.+-]{1,32}( · [0-9a-f]{7})?$/.test(label)).toBe(true);
    }
  });
});
