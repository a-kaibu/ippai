import { describe, expect, it } from "vite-plus/test";
import { blake3Hex } from "./blake3";
import {
  directFetcher,
  isDirectInfo,
  isPackSpan,
  limit,
  openSealed,
  PACK_RUN_BYTES,
  PACK_RUN_MAX,
} from "./direct";
import { byOrder, roomFor } from "./offline";

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
    many?: () => Response;
    onlyServer?: string[];
    /** a bucket that sends the whole pack for a range */
    noRange?: boolean;
  } = {},
) {
  const objs = new Map<string, Obj>();
  const asked: string[] = [];
  const ranges: string[] = [];
  // (a delivery pack: objects side by side, crates/ippai/src/storage.rs pack_objects)
  const PACK = "0f0e0d0c-0b0a-4908-8706-050403020100";
  const at = new Map<string, { id: string; offset: number; len: number }>();
  const packs = new Map<string, Uint8Array>();
  // (`start`: where the first one goes, after that many bytes of something else)
  const pack = (names: string[], id = PACK, start = 0) => {
    const parts = names.map((n) => objs.get(n)!.sealed);
    const body = new Uint8Array(parts.reduce((n, p) => n + p.length, start));
    let off = start;
    names.forEach((n, i) => {
      at.set(n, { id, offset: off, len: parts[i]!.length });
      body.set(parts[i]!, off);
      off += parts[i]!.length;
    });
    packs.set(id, body);
    return body;
  };
  // (`wrong`: sealed as the bucket has it, but not the chunk its name says)
  const add = async (data: B, wrong = false) => {
    const key = randomBytes(32);
    const object = randomUUID();
    const name = blake3Hex(data);
    objs.set(name, {
      data,
      key,
      object,
      sealed: await seal(key, object, frame(wrong ? data.map((b) => b ^ 1) : data)),
    });
    return name;
  };
  const answer = (o: Obj) => {
    const n = [...objs.entries()].find(([, v]) => v === o)![0];
    return {
      url: `https://bucket.test/${o.object}`,
      key: toHex(o.key),
      object: o.object,
      ...(at.has(n) ? { pack: { url: `https://bucket.test/pack/${at.get(n)!.id}`, ...at.get(n)! } } : {}),
    };
  };
  const request: typeof fetch = async (input, init) => {
    const url = urlOf(input);
    asked.push(url);
    if (url.endsWith("/store-direct") && init?.method === "POST") {
      if (how.many) return how.many();
      const out: Record<string, unknown> = {};
      for (const p of JSON.parse(init.body as string) as string[]) {
        const o = objs.get(p.slice(p.lastIndexOf("/") + 1));
        if (o && !how.onlyServer?.includes(p.slice(-32))) out[p] = answer(o);
      }
      return Response.json(out);
    }
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
    const pm = /^https:\/\/bucket\.test\/pack\/(.+)$/.exec(url);
    if (pm) {
      const body = packs.get(pm[1]!);
      if (!body) return new Response("", { status: 404 });
      const range = new Headers(init?.headers).get("range") ?? "";
      ranges.push(range);
      // (past the HTTP cache: #213, a cached whole object answered ranges wrongly)
      if (init?.cache !== "no-store") return new Response("", { status: 500 });
      const r = /^bytes=(\d+)-(\d+)$/.exec(range);
      if (!r || how.noRange) return new Response(body.slice());
      return new Response(body.slice(+r[1]!, +r[2]! + 1), { status: 206 });
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
  return { add, request, asked, objs, pack, ranges, at };
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

describe("directFetcher.prime", () => {
  it("asks for many at once (512 a POST), then fetches each from the bucket without asking alone", async () => {
    const w = world();
    const hexes = [];
    for (let i = 0; i < 600; i++) hexes.push(await w.add(bytes(64 + i, 100 + i)));
    const d = directFetcher(STORE, { request: w.request, unzstd });
    await d.prime(hexes);
    expect(w.asked.filter((u) => u.endsWith("/store-direct")).length).toBe(2);
    const n = w.asked.length;
    for (const h of hexes.slice(0, 20)) expect(await d.get(h)).toEqual(w.objs.get(h)!.data);
    expect(w.asked.slice(n).every((u) => u.startsWith("https://bucket.test/"))).toBe(true);
    // (asked again: nothing new)
    await d.prime(hexes);
    expect(w.asked.filter((u) => u.endsWith("/store-direct")).length).toBe(2);
  });

  it("a chunk the answer leaves out is only on the server: null, without asking alone", async () => {
    const w0 = world();
    const hex = await w0.add(bytes(500, 7));
    const w = world({ onlyServer: [hex] });
    w.objs.set(hex, w0.objs.get(hex)!);
    const d = directFetcher(STORE, { request: w.request, unzstd });
    await d.prime([hex]);
    const n = w.asked.length;
    expect(await d.get(hex)).toBeNull();
    expect(w.asked.length).toBe(n);
  });

  it("an older server without the route: asked one by one, as before", async () => {
    const w = world({ many: () => new Response("", { status: 405 }) });
    const hex = await w.add(bytes(500, 8));
    const d = directFetcher(STORE, { request: w.request, unzstd });
    await d.prime([hex]);
    expect(await d.get(hex)).toEqual(w.objs.get(hex)!.data);
    expect(w.asked.some((u) => u.includes("/store-direct/chunks/"))).toBe(true);
    // (and not asked in batches again)
    await d.prime([await w.add(bytes(9, 9))]);
    expect(w.asked.filter((u) => u.endsWith("/store-direct")).length).toBe(1);
  });
});

describe("byOrder", () => {
  it("puts what earlier runs read first, in their order, then the rest as it was", () => {
    expect(byOrder(["a", "b", "c", "d"], ["c", "x", "a", "c"])).toEqual(["c", "a", "b", "d"]);
    // (a permutation of what it was given, whatever the order says: random cases)
    let x = 5;
    const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
    for (let run = 0; run < 300; run++) {
      const hashes = [...new Set(Array.from({ length: rnd(30) }, () => `h${rnd(40)}`))];
      const order = Array.from({ length: rnd(30) }, () => `h${rnd(40)}`);
      const out = byOrder(hashes, order);
      expect([...out].sort()).toEqual([...hashes].sort());
      const firsts = [...new Set(order.filter((h) => hashes.includes(h)))];
      expect(out.slice(0, firsts.length)).toEqual(firsts);
    }
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

describe("delivery packs", () => {
  const setup = async (n: number, how: Parameters<typeof world>[0] = {}) => {
    const w = world(how);
    const names: string[] = [];
    for (let i = 0; i < n; i++) names.push(await w.add(bytes(200 + ((i * 7919) % 900), 100 + i)));
    w.pack(names);
    const d = directFetcher(STORE, { request: w.request, unzstd });
    await d.prime(names);
    return { w, d, names };
  };

  it("a run of a pack's chunks side by side comes with one ranged GET", async () => {
    const { w, d, names } = await setup(10);
    expect(d.span(names, 0)).toBe(10);
    const got = await d.getRun(names);
    expect(w.ranges).toEqual([`bytes=0-${w.at.get(names[9]!)!.offset + w.at.get(names[9]!)!.len - 1}`]);
    for (const n of names) expect(got.get(n)).toEqual(w.objs.get(n)!.data);
    // (one from the middle on, and with those already here left out: from the first to the last asked)
    w.ranges.length = 0;
    expect(d.span(names, 4)).toBe(6);
    const some = [names[5]!, names[8]!];
    const part = await d.getRun(some);
    expect([...part.keys()]).toEqual(some);
    expect(w.ranges).toEqual([
      `bytes=${w.at.get(names[5]!)!.offset}-${w.at.get(names[8]!)!.offset + w.at.get(names[8]!)!.len - 1}`,
    ]);
  });

  it("span: the longest run from there that is known, of one pack, side by side, within the caps (random orders)", async () => {
    const { d, names, w } = await setup(40);
    let x = 7;
    const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
    const pos = new Map(names.map((n, i) => [n, i]));
    for (let t = 0; t < 300; t++) {
      // (mostly in pack order, with swaps, unknown names and repeats)
      const order = names.slice(rnd(10), 30 + rnd(10));
      for (let s = rnd(4); s > 0; s--) {
        const a = rnd(order.length);
        const b = rnd(order.length);
        [order[a], order[b]] = [order[b]!, order[a]!];
      }
      if (rnd(3) === 0) order.splice(rnd(order.length), 0, "f".repeat(32));
      const from = rnd(order.length);
      let want = 1;
      if (pos.has(order[from]!)) {
        let bytes = w.at.get(order[from]!)!.len;
        while (from + want < order.length && want < PACK_RUN_MAX) {
          const prev = pos.get(order[from + want - 1]!);
          const cur = pos.get(order[from + want]!);
          if (
            prev === undefined ||
            cur !== prev + 1 ||
            bytes + w.at.get(order[from + want]!)!.len > PACK_RUN_BYTES
          )
            break;
          bytes += w.at.get(order[from + want]!)!.len;
          want++;
        }
      }
      expect(d.span(order, from), `${t}`).toBe(want);
    }
  });

  it("a bucket without ranges: the run cut from the whole pack; a damaged span or a wrong one left out", async () => {
    {
      const { d, names } = await setup(5, { noRange: true });
      const got = await d.getRun(names.slice(1, 4));
      expect([...got.keys()]).toEqual(names.slice(1, 4));
    }
    {
      const { w, d, names } = await setup(5);
      // (damaged in the bucket: that one does not open)
      const body = w.pack(names);
      body[w.at.get(names[2]!)!.offset + 30]! ^= 1;
      const got = await d.getRun(names);
      expect([...got.keys()]).toEqual([names[0], names[1], names[3], names[4]]);
    }
    {
      // (a chunk in no pack: alone)
      const w = world({ bucket: () => undefined });
      const n = await w.add(bytes(500, 9));
      const d = directFetcher(STORE, { request: w.request, unzstd });
      await d.prime([n]);
      expect(d.span([n], 0)).toBe(1);
      expect((await d.getRun([n])).size).toBe(0);
    }
  });

  it("span stops at another pack, at the caps; a run's chunk that opens but is not its name is left out", async () => {
    const w = world();
    const names: string[] = [];
    for (let i = 0; i < 12; i++) names.push(await w.add(bytes(300, 300 + i), i === 3));
    w.pack(names.slice(0, 6));
    // (the other pack's first one where this one's last ends: not side by side all the same)
    const end = w.at.get(names[5]!)!.offset + w.at.get(names[5]!)!.len;
    w.pack(names.slice(6), "1f0e0d0c-0b0a-4908-8706-050403020100", end);
    const size = w.at.get(names[0]!)!.len;
    const d = directFetcher(STORE, { request: w.request, unzstd, packRun: [size * 4, 3] });
    await d.prime(names);
    // (3 at most; 4 sizes of bytes at most: 3 again; the other pack from 6 on)
    expect([0, 3, 4, 5, 6, 9, 10].map((i) => d.span(names, i))).toEqual([3, 3, 2, 1, 3, 3, 2]);
    const wide = directFetcher(STORE, { request: w.request, unzstd, packRun: [size * 2 + size / 2, 100] });
    await wide.prime(names);
    expect(wide.span(names, 0)).toBe(2);
    const got = await d.getRun(names.slice(2, 5));
    expect([...got.keys()]).toEqual([names[2], names[4]]);
    // (one pack a GET: the first one's)
    w.ranges.length = 0;
    expect([...(await d.getRun(names.slice(4, 8))).keys()]).toEqual([names[4], names[5]]);
    expect(w.ranges).toEqual([`bytes=${w.at.get(names[4]!)!.offset}-${end - 1}`]);
  });

  it("a malformed pack in an answer is no pack", () => {
    const id = "0f0e0d0c-0b0a-4908-8706-050403020100";
    for (const bad of [
      null,
      {},
      { id: "x", url: "u", offset: 0, len: 100 },
      { id, url: 1, offset: 0, len: 100 },
      { id, url: "u", offset: -1, len: 100 },
      { id, url: "u", offset: 0, len: 27 },
      { id, url: "u", offset: 0.5, len: 100 },
    ])
      expect(isPackSpan(bad), JSON.stringify(bad)).toBe(false);
    expect(isPackSpan({ id, url: "u", offset: 0, len: 28 })).toBe(true);
  });
});

describe("the page's share of the fixes of the 2026-11-14 review", () => {
  it("fetchRetry takes from the budget for every try of a bucket request, and a 429 stops every class", async () => {
    const { netBudget, CLASS, take } = await import("./budget");
    const { fetchRetry } = await import("./net");
    const v = new Float64Array(netBudget(), 8, 3);
    // (no refill meanwhile: the refill clock far ahead)
    v.set([40, Date.now() + 1e9, 0]);
    let n = 0;
    const request = (async () =>
      ++n < 3 ? new Response(null, { status: 503 }) : new Response("ok")) as typeof fetch;
    await fetchRetry("https://bucket.example/x", {}, { request, budget: CLASS.save, stallMs: 1000 });
    expect(n).toBe(3);
    expect(v[0]).toBe(37);
    // (a 429: nobody, the game neither, until its wait is over)
    v.set([40, Date.now(), 0]);
    let first = true;
    const r429 = (async () =>
      first
        ? ((first = false), new Response(null, { status: 429, headers: { "retry-after": "1" } }))
        : new Response("ok")) as typeof fetch;
    const going = fetchRetry("https://bucket.example/y", {}, { request: r429, budget: CLASS.save });
    await new Promise((r) => setTimeout(r, 50));
    expect(take(CLASS.game)).toBe(false);
    expect(v[2]!).toBeGreaterThan(Date.now() + 500);
    await going;
    v.set([40, Date.now(), 0]);
  }, 20000);

  it("a run of a pack holds the locks nobody else holds, and leaves the others out", async () => {
    const { withFreeChunkLocks } = await import("./offline");
    const held = new Set(["ippai-chunk:b", "ippai-chunk:d"]);
    const locks = {
      async request(name: string, o: { ifAvailable?: boolean }, f: (l: object | null) => Promise<unknown>) {
        if (held.has(name)) return f(o.ifAvailable ? null : {});
        held.add(name);
        try {
          return await f({});
        } finally {
          held.delete(name);
        }
      },
    };
    const nav = globalThis.navigator as unknown as { locks?: unknown };
    const had = Object.getOwnPropertyDescriptor(nav, "locks");
    Object.defineProperty(nav, "locks", { value: locks, configurable: true });
    try {
      const got = await withFreeChunkLocks(["a", "b", "c", "d", "e"], async (mine) => {
        // (while the run goes: its locks held)
        expect([...held].sort()).toEqual(["a", "b", "c", "d", "e"].map((h) => `ippai-chunk:${h}`));
        return mine;
      });
      expect(got).toEqual(["a", "c", "e"]);
      expect([...held].sort()).toEqual(["ippai-chunk:b", "ippai-chunk:d"]);
    } finally {
      if (had) Object.defineProperty(nav, "locks", had);
      else delete nav.locks;
    }
  });

  it("byteLimit never holds more than its bytes at once, one alone always goes (seeded)", async () => {
    const { byteLimit } = await import("./offline");
    let x = 7;
    const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
    for (let run = 0; run < 20; run++) {
      const max = 1 + rnd(100);
      const held = byteLimit(max);
      let now = 0;
      let peak = 0;
      const sizes = Array.from({ length: 30 }, () => 1 + rnd(150));
      await Promise.all(
        sizes.map((n) =>
          held(n, async () => {
            now += n;
            expect(now <= max || now === n, `run ${run}: ${now} of ${max}`).toBe(true);
            peak = Math.max(peak, now);
            await new Promise((r) => setTimeout(r, rnd(3)));
            now -= n;
          }),
        ),
      );
      expect(now).toBe(0);
      expect(peak).toBeGreaterThan(0);
    }
  });
});
