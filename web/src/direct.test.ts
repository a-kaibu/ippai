import { describe, expect, it } from "vite-plus/test";
import { blake3Hex } from "./blake3";
import { directFetcher, isDirectInfo, limit, openSealed } from "./direct";
import { roomFor } from "./offline";

type B = Uint8Array<ArrayBuffer>;
const urlOf = (i: RequestInfo | URL) => (typeof i === "string" ? i : i instanceof URL ? i.href : i.url);
const randomBytes = (n: number) => crypto.getRandomValues(new Uint8Array(n));
const randomUUID = () => crypto.randomUUID();
const toHex = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, "0")).join("");
const unhex = (h: string) => Uint8Array.from(h.match(/../g) ?? [], (x) => parseInt(x, 16));
/** envelope.rs: nonce (12) + AES-256-GCM ciphertext + tag, the object's UUID bytes as associated data. */
const seal = async (key: B, object: string, plain: B) => {
  const nonce = randomBytes(12);
  const k = await crypto.subtle.importKey("raw", key, "AES-GCM", false, ["encrypt"]);
  const c = new Uint8Array(
    await crypto.subtle.encrypt(
      { name: "AES-GCM", iv: nonce, additionalData: unhex(object.replace(/-/g, "")) },
      k,
      plain,
    ),
  );
  const out = new Uint8Array(12 + c.length);
  out.set(nonce);
  out.set(c, 12);
  return out;
};
const bytes = (n: number, seed: number) => {
  let x = seed >>> 0;
  return Uint8Array.from({ length: n }, () => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 16) & 255);
};
// (the "zstd" of these tests: a marker byte before the bytes; the real one is sake's snapz,
// tests/save-first-browser.mjs and tests/direct-browser.mjs)
const frame = (d: Uint8Array): B => {
  const f = new Uint8Array(d.length + 1);
  f[0] = 0x5a;
  f.set(d, 1);
  return f;
};
const unzstd = async (f: Uint8Array) => {
  if (f[0] !== 0x5a) throw new Error("not a frame");
  return f.slice(1);
};

interface Obj {
  data: B;
  key: B;
  object: string;
  sealed: B;
}

/** A server with /store-direct and a bucket: `how` changes one answer. */
function world(
  how: {
    direct?: (hex: string) => Response | undefined;
    bucket?: (hex: string) => Response | Promise<Response> | undefined;
  } = {},
) {
  const objs = new Map<string, Obj>();
  const asked: string[] = [];
  const add = async (data: B) => {
    const key = randomBytes(32);
    const object = randomUUID();
    const name = blake3Hex(data);
    objs.set(name, { data, key, object, sealed: await seal(key, object, frame(data)) });
    return name;
  };
  const request: typeof fetch = async (input) => {
    const url = urlOf(input);
    asked.push(url);
    let m = /\/store-direct\/chunks\/[0-9a-f]{2}\/([0-9a-f]{32})$/.exec(url);
    if (m) {
      const hex = m[1]!;
      const own = how.direct?.(hex);
      if (own) return own;
      const o = objs.get(hex);
      if (!o) return new Response("", { status: 404 });
      return Response.json({
        url: `https://bucket.test/${o.object}`,
        key: toHex(o.key),
        object: o.object,
      });
    }
    m = /^https:\/\/bucket\.test\/(.+)$/.exec(url);
    if (m) {
      const o = [...objs.entries()].find(([, v]) => v.object === m![1]);
      if (!o) return new Response("", { status: 404 });
      const own = how.bucket?.(o[0]);
      if (own) return await own;
      return new Response(o[1].sealed);
    }
    return new Response("", { status: 500 });
  };
  return { add, request, asked, objs };
}

const STORE = "https://ippai.test/store/";

describe("directFetcher", () => {
  it("opens, decodes and checks a chunk from the bucket", async () => {
    const w = world();
    const data = bytes(300000, 1);
    const hex = await w.add(data);
    const d = directFetcher(STORE, { request: w.request, unzstd });
    expect(await d.get(hex)).toEqual(data);
    expect(w.asked).toEqual([
      `https://ippai.test/store-direct/chunks/${hex.slice(0, 2)}/${hex}`,
      expect.stringMatching(/^https:\/\/bucket\.test\//),
    ]);
  });

  it("gives null for a chunk only on the server, and goes on with the next", async () => {
    const w = world();
    const hex = await w.add(bytes(1000, 2));
    const d = directFetcher(STORE, { request: w.request, unzstd });
    expect(await d.get("0".repeat(32))).toBeNull();
    expect(d.off()).toBe(false);
    expect(await d.get(hex)).not.toBeNull();
  });

  it("stops asking a store without S3 (501) or a server without /store-direct (not JSON)", async () => {
    for (const answer of [new Response("", { status: 501 }), new Response("<html>", { status: 200 })]) {
      const w = world({ direct: () => answer.clone() });
      const hex = await w.add(bytes(1000, 3));
      const d = directFetcher(STORE, { request: w.request, unzstd });
      expect(await d.get(hex)).toBeNull();
      expect(d.off()).toBe(true);
      const n = w.asked.length;
      expect(await d.get(hex)).toBeNull();
      expect(w.asked.length, "asked again").toBe(n);
    }
  });

  it("gives up on a stuck server after the time asked, without stopping", async () => {
    const w = world();
    const hex = await w.add(bytes(1000, 4));
    const stuck: typeof fetch = (input, init) =>
      urlOf(input).includes("store-direct")
        ? new Promise((_, no) => init?.signal?.addEventListener("abort", () => no(init.signal!.reason)))
        : w.request(input, init);
    const d = directFetcher(STORE, { request: stuck, unzstd, askMs: 50 });
    const t = Date.now();
    expect(await d.get(hex)).toBeNull();
    expect(Date.now() - t).toBeLessThan(2000);
    expect(d.off()).toBe(false);
  });

  it("gives null for an object that will not open, decode or match its name (3 tries)", async () => {
    const cases: [string, (o: Obj) => Promise<B>][] = [
      ["wrong key", (o) => seal(randomBytes(32), o.object, frame(o.data))],
      ["damaged", async (o) => o.sealed.map((b, i) => (i === 40 ? b ^ 1 : b))],
      ["not a frame", (o) => seal(o.key, o.object, o.data)],
      ["another chunk", (o) => seal(o.key, o.object, frame(bytes(o.data.length, 99)))],
      ["short", async () => new Uint8Array(10)],
    ];
    for (const [label, bad] of cases) {
      let gets = 0;
      const w = world({
        bucket: (hex) => {
          gets++;
          return bad(w.objs.get(hex)!).then((b) => new Response(b));
        },
      });
      const hex = await w.add(bytes(5000, 5));
      const d = directFetcher(STORE, { request: w.request, unzstd });
      expect(await d.get(hex), label).toBeNull();
      expect(gets, label).toBe(3);
    }
  });

  it("takes a good copy after a bad one", async () => {
    let gets = 0;
    const w = world({ bucket: () => (gets++ === 0 ? new Response(new Uint8Array(40)) : undefined) });
    const data = bytes(5000, 6);
    const hex = await w.add(data);
    expect(await directFetcher(STORE, { request: w.request, unzstd }).get(hex)).toEqual(data);
  });

  it("gives null for an expired URL (403), and throws when stopped", async () => {
    const w = world({ bucket: () => new Response("", { status: 403 }) });
    const hex = await w.add(bytes(100, 7));
    const d = directFetcher(STORE, { request: w.request, unzstd });
    expect(await d.get(hex)).toBeNull();
    const stop = new AbortController();
    stop.abort(new Error("中止"));
    await expect(d.get(hex, { signal: stop.signal })).rejects.toThrow("中止");
  });

  it("checks the answer's shape", () => {
    const ok = { url: "https://b/x", key: "a".repeat(64), object: randomUUID() };
    expect(isDirectInfo(ok)).toBe(true);
    expect(isDirectInfo({ ...ok, key: "a".repeat(63) })).toBe(false);
    expect(isDirectInfo({ ...ok, object: "x" })).toBe(false);
    expect(isDirectInfo({ ...ok, url: 1 })).toBe(false);
    expect(isDirectInfo(null)).toBe(false);
  });

  it("opens what envelope.rs seals (the UUID bound in)", async () => {
    const key = randomBytes(32);
    const object = randomUUID();
    const sealed = await seal(key, object, Uint8Array.of(1, 2, 3));
    expect(await openSealed({ url: "", key: toHex(key), object }, sealed)).toEqual(Uint8Array.of(1, 2, 3));
    await expect(openSealed({ url: "", key: toHex(key), object: randomUUID() }, sealed)).rejects.toThrow();
  });
});

describe("limit", () => {
  it("runs at most n at once, all of them, in order of asking", async () => {
    const at = limit(3);
    let now = 0;
    let most = 0;
    const order: number[] = [];
    await Promise.all(
      Array.from({ length: 20 }, (_, i) =>
        at(async () => {
          most = Math.max(most, ++now);
          order.push(i);
          await new Promise((r) => setTimeout(r, 1 + (i % 3)));
          now--;
        }),
      ),
    );
    expect(most).toBe(3);
    expect(order).toEqual(Array.from({ length: 20 }, (_, i) => i));
  });

  it("frees the place of one that failed", async () => {
    const at = limit(1);
    await expect(at(() => Promise.reject(new Error("x")))).rejects.toThrow("x");
    expect(await at(async () => 5)).toBe(5);
  });
});

describe("roomFor", () => {
  const GB = 2 ** 30;
  it("leaves a margin under the quota", () => {
    expect(roomFor(1 * GB, 10 * GB, 1 * GB)).toBe(true);
    expect(roomFor(7 * GB, 10 * GB, 1 * GB)).toBe(true);
    expect(roomFor(8 * GB, 10 * GB, 1 * GB)).toBe(false);
    expect(roomFor(1, 10 * GB, 8.9 * GB)).toBe(true);
    expect(roomFor(1, 10 * GB, 9.5 * GB)).toBe(false);
  });
  it("does not stop a save where the browser says nothing", () => {
    expect(roomFor(100 * GB)).toBe(true);
    expect(roomFor(100 * GB, 0, 0)).toBe(true);
  });
});
