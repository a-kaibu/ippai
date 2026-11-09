import { afterAll, beforeAll, describe, expect, it } from "vite-plus/test";
import { checkUpdate, installUpdate, parseUpdate, sameVersion, waitForVersion } from "./server-update";

const g = globalThis as unknown as { location?: unknown };
const oldLocation = g.location;
beforeAll(() => {
  g.location = { href: "https://ippai.test/sub/index.html" };
});
afterAll(() => {
  g.location = oldLocation;
});

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

function rng(seed: number) {
  return () => (seed = (seed * 1103515245 + 12345) >>> 0) / 2 ** 32;
}

describe("parseUpdate", () => {
  it("reads the server's answer", () => {
    expect(
      parseUpdate({ current: "0.3.3", latest: "0.3.4", newer: true, supported: true, error: null }),
    ).toEqual({
      current: "0.3.3",
      latest: "0.3.4",
      newer: true,
      supported: true,
      error: "",
    });
    expect(
      parseUpdate({ current: "0.3.3", latest: null, newer: false, supported: false, error: "rate limited" }),
    ).toEqual({ current: "0.3.3", latest: "", newer: false, supported: false, error: "rate limited" });
  });
  it("is null for anything that is not one (random values)", () => {
    for (const v of [null, undefined, 1, "x", [], {}, { current: 1, newer: true }, { current: "1" }])
      expect(parseUpdate(v)).toBeNull();
    const r = rng(9);
    const vals = [null, 0, true, false, "", "0.3.4", [], {}];
    for (let n = 0; n < 500; n++) {
      const o: Record<string, unknown> = {};
      for (const k of ["current", "latest", "newer", "supported", "error"])
        if (r() < 0.8) o[k] = vals[Math.floor(r() * vals.length)];
      const p = parseUpdate(o);
      if (p) {
        expect(typeof o.current).toBe("string");
        expect(typeof o.newer).toBe("boolean");
        for (const k of ["current", "latest", "error"] as const) expect(typeof p[k]).toBe("string");
      } else expect(typeof o.current !== "string" || typeof o.newer !== "boolean").toBe(true);
    }
  });
});

describe("sameVersion", () => {
  it("ignores a leading v, and nothing is no version", () => {
    expect(sameVersion("v0.3.4", "0.3.4")).toBe(true);
    expect(sameVersion("0.3.4", "v0.3.4")).toBe(true);
    expect(sameVersion("0.3.4", "0.3.40")).toBe(false);
    expect(sameVersion("", "")).toBe(false);
  });
});

describe("checkUpdate / installUpdate", () => {
  it("asks the page's own server, under its path", async () => {
    const asked: string[] = [];
    const f = (u: URL, init?: RequestInit) => {
      asked.push(`${init?.method ?? "GET"} ${u.href}`);
      return Promise.resolve(
        init?.method === "POST"
          ? json({ installed: "0.3.4" })
          : json({ current: "0.3.3", latest: "0.3.4", newer: true, supported: true }),
      );
    };
    expect((await checkUpdate(f)).newer).toBe(true);
    expect(await installUpdate(f)).toBe("0.3.4");
    expect(asked).toEqual([
      "GET https://ippai.test/sub/api/update",
      "POST https://ippai.test/sub/api/update",
    ]);
  });
  it("says why it did not update", async () => {
    const answer = (r: Response) => () => Promise.resolve(r);
    await expect(installUpdate(answer(new Response("最新です", { status: 409 })))).rejects.toThrow(
      /^最新です$/,
    );
    await expect(installUpdate(answer(new Response("更新中です", { status: 409 })))).rejects.toThrow(
      /^更新中です$/,
    );
    await expect(installUpdate(answer(new Response("", { status: 409 })))).rejects.toThrow(/^最新です$/);
    await expect(installUpdate(answer(new Response("", { status: 501 })))).rejects.toThrow(/Linux/);
    await expect(
      installUpdate(answer(new Response("更新できませんでした: checksum", { status: 502 }))),
    ).rejects.toThrow(/^更新できませんでした: checksum$/);
    await expect(installUpdate(answer(new Response("download failed", { status: 500 })))).rejects.toThrow(
      /500: download failed/,
    );
    await expect(installUpdate(answer(json({})))).rejects.toThrow(/読めません/);
    await expect(checkUpdate(answer(new Response("", { status: 404 })))).rejects.toThrow(/対応していません/);
    // (an older server answers /api/update with the page)
    await expect(checkUpdate(answer(new Response("<!doctype html>")))).rejects.toThrow(/対応していません/);
  });
});

describe("waitForVersion", () => {
  it("waits through the old server and the restart until the new version answers", async () => {
    let t = 0;
    const seq = [json({ version: "0.3.3" }), "down", "down", json({ version: "0.3.4", commit: "abc1234" })];
    let i = 0;
    const f = () => {
      const s = seq[Math.min(i++, seq.length - 1)];
      return s === "down" ? Promise.reject(new TypeError("Failed to fetch")) : Promise.resolve(s as Response);
    };
    const ok = await waitForVersion("v0.3.4", {
      f,
      sleep: async (ms) => void (t += ms),
      now: () => t,
      everyMs: 500,
    });
    expect(ok).toBe(true);
    expect(i).toBe(4);
    expect(t).toBe(1500);
  });
  it("gives up after the timeout (random answers that never match)", async () => {
    const r = rng(4);
    for (let n = 0; n < 50; n++) {
      let t = 0;
      let calls = 0;
      const f = () => {
        calls++;
        const x = r();
        return x < 0.3
          ? Promise.reject(new Error("down"))
          : Promise.resolve(
              x < 0.6 ? json({ version: "0.3.3" }) : x < 0.8 ? new Response("<html>") : json({}, 503),
            );
      };
      const ok = await waitForVersion("0.3.4", {
        f,
        timeoutMs: 10_000,
        everyMs: 700,
        sleep: async (ms) => void (t += ms),
        now: () => t,
      });
      expect(ok).toBe(false);
      expect(calls).toBe(Math.ceil(10_000 / 700));
    }
  });
});
