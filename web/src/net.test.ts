import { expect, test } from "vite-plus/test";
import { blake3Hex } from "./blake3";
import { fetchRetry, getChunk, HttpError, treeChunks, trimChunks, watchRetries } from "./net";

const bytes = (n: number) => Uint8Array.from({ length: n }, (_, i) => i % 251);

test("dropped connections and 503s are retried; a 404 is final", async () => {
  let n = 0;
  const request: typeof fetch = async () => {
    n++;
    if (n === 1) throw new TypeError("Failed to fetch");
    if (n === 2) return new Response(null, { status: 503 });
    return new Response(bytes(1000));
  };
  expect(Array.from(await fetchRetry("/x", {}, { request }))).toEqual(Array.from(bytes(1000)));
  expect(n).toBe(3);
  let m = 0;
  const missing: typeof fetch = async () => (m++, new Response(null, { status: 404 }));
  await expect(fetchRetry("/x", {}, { request: missing })).rejects.toBeInstanceOf(HttpError);
  expect(m).toBe(1);
});

test("a body that stops arriving is aborted and fetched again", async () => {
  let n = 0;
  const request: typeof fetch = async (_u, init) => {
    n++;
    if (n > 1) return new Response(bytes(70000));
    // half the body, then nothing: the stall watch aborts it
    const body = new ReadableStream<Uint8Array>({
      start(c) {
        c.enqueue(bytes(100));
        init!.signal!.addEventListener("abort", () => c.error(init!.signal!.reason));
      },
    });
    return new Response(body);
  };
  const t = Date.now();
  expect((await fetchRetry("/x", {}, { request, stallMs: 150 })).length).toBe(70000);
  expect(n).toBe(2);
  expect(Date.now() - t).toBeLessThan(3000);
});

test("gives up after giveUpMs, and the page's retries are counted while they last", async () => {
  const seen: number[] = [];
  const stop = watchRetries((r) => seen.push(r));
  const request: typeof fetch = async () => {
    throw new TypeError("Failed to fetch");
  };
  await expect(fetchRetry("/x", {}, { request, giveUpMs: 500 })).rejects.toThrow("Failed to fetch");
  stop();
  expect(seen[0]).toBe(1);
  expect(seen.at(-1)).toBe(0);
});

test("a tree's chunk hashes (holes left out)", () => {
  const h = (c: string) => c.repeat(32);
  const tsv = `vnvm-tree\t2\t16777216\nd\t755\t0\tdir\nf\t644\t0\tdir/a\t30\t${h("a")}:10,:5,${h("b")}:15\nf\t644\t0\tempty\t0\t\nl\t777\t0\tlink\ta\n`;
  expect(treeChunks(tsv)).toEqual([h("a"), h("b")]);
});

/** A Cache Storage stand-in: what was put, with its headers. */
function fakeCache() {
  const m = new Map<string, Response>();
  const k = (u: string | Request) => (typeof u === "string" ? u : u.url);
  const cache = {
    match: async (u: string | Request) => m.get(k(u))?.clone(),
    put: async (u: string | Request, r: Response) => void m.set(k(u), r),
    delete: async (u: string | Request) => m.delete(k(u)),
  };
  return { m, cache: cache as unknown as Cache };
}

test("a chunk is checked against its name: kept marked, fetched again past the HTTP cache when damaged", async () => {
  const good = bytes(5000);
  const hex = blake3Hex(good);
  const inits: RequestInit[] = [];
  const damaged = Uint8Array.from(good);
  damaged[100]! ^= 1;
  const request: typeof fetch = async (_u, init) => (
    inits.push(init!),
    new Response(inits.length === 1 ? damaged : good)
  );
  const { m, cache } = fakeCache();
  const r = await getChunk("https://s/store/", hex, { cache, request });
  expect(Array.from(r.data)).toEqual(Array.from(good));
  expect(r.cached).toBe(false);
  expect(inits.map((i) => i.cache)).toEqual([undefined, "reload"]);
  const kept = m.get(`https://s/store/chunks/${hex.slice(0, 2)}/${hex}`)!;
  expect(kept.headers.get("x-ippai-checked")).toBe("1");
  // next time from the cache, no request
  const again = await getChunk("https://s/store/", hex, { cache, request });
  expect(again.cached).toBe(true);
  expect(inits.length).toBe(2);
});

test("a chunk that never matches fails; one kept before the check is checked once", async () => {
  const good = bytes(3000);
  const hex = blake3Hex(good);
  let n = 0;
  const wrong: typeof fetch = async () => (n++, new Response(bytes(10)));
  await expect(getChunk("https://s/store/", hex, { cache: null, request: wrong })).rejects.toThrow(
    /does not match/,
  );
  expect(n).toBe(3);
  const url = `https://s/store/chunks/${hex.slice(0, 2)}/${hex}`;
  // kept by an older page (no mark): checked, kept again with the mark
  const { m, cache } = fakeCache();
  m.set(url, new Response(good));
  const none: typeof fetch = async () => {
    throw new Error("no request expected");
  };
  expect((await getChunk("https://s/store/", hex, { cache, request: none })).cached).toBe(true);
  await new Promise((r) => setTimeout(r, 0));
  expect(m.get(url)!.headers.get("x-ippai-checked")).toBe("1");
  // damaged in the cache: dropped, fetched
  m.set(url, new Response(bytes(10)));
  const fine: typeof fetch = async () => new Response(good);
  const r = await getChunk("https://s/store/", hex, { cache, request: fine });
  expect(r.cached).toBe(false);
  expect(Array.from(r.data)).toEqual(Array.from(good));
});

test("the chunk cache is trimmed by the chunks' real sizes, the saved games' chunks kept", async () => {
  const { m, cache } = fakeCache();
  const keyed = cache as unknown as { keys: () => Promise<Request[]> };
  keyed.keys = async () => [...m.keys()].map((u) => new Request(u));
  const g = globalThis as unknown as { caches?: unknown };
  const old = g.caches;
  g.caches = { open: async () => cache };
  const storage = { estimate: async () => ({ quota: 4 << 20 }), persist: async () => true };
  Object.defineProperty(navigator, "storage", { value: storage, configurable: true });
  try {
    // limit: half the quota, 2 MiB. 20 chunks of 64 KiB (1.25 MiB): all stay, though 20 x 256
    // KiB would be over
    const put = (i: number, n: number, sized = true) =>
      m.set(
        `https://s/store/chunks/00/${i.toString(16).padStart(32, "0")}`,
        new Response(new Uint8Array(n), { headers: sized ? { "x-ippai-size": String(n) } : {} }),
      );
    for (let i = 0; i < 20; i++) put(i, 64 << 10);
    await trimChunks();
    expect(m.size).toBe(20);
    // 12 more of 256 KiB without the header (an older page's): 4.25 MiB, the oldest go, but
    // not the saved ones (0 and 1)
    for (let i = 20; i < 32; i++) put(i, 256 << 10, false);
    const saved = new Set([0, 1].map((i) => i.toString(16).padStart(32, "0")));
    await trimChunks(async () => saved);
    const left = [...m.keys()].map((u) => parseInt(u.slice(-32), 16));
    expect(left).toContain(0);
    expect(left).toContain(1);
    const bytes = left.reduce((a, i) => a + (i < 20 ? 64 << 10 : 256 << 10), 0);
    expect(bytes).toBeLessThanOrEqual(2 << 20);
    expect(bytes).toBeGreaterThan((2 << 20) - (256 << 10));
  } finally {
    g.caches = old;
  }
});

test("a big quota keeps a small chunk cache whole (the 2 GB cap is not negative)", async () => {
  const { m, cache } = fakeCache();
  (cache as unknown as { keys: () => Promise<Request[]> }).keys = async () =>
    [...m.keys()].map((u) => new Request(u));
  const g = globalThis as unknown as { caches?: unknown };
  const old = g.caches;
  g.caches = { open: async () => cache };
  for (const quota of [100e9, 0]) {
    Object.defineProperty(navigator, "storage", {
      value: { estimate: async () => ({ quota }) },
      configurable: true,
    });
    for (let i = 0; i < 3; i++)
      m.set(`https://s/store/chunks/00/${String(i).padStart(32, "0")}`, new Response(new Uint8Array(10)));
    try {
      await trimChunks();
      expect(m.size, `quota ${quota}`).toBe(3);
    } finally {
      g.caches = old;
    }
    g.caches = { open: async () => cache };
  }
  g.caches = old;
});

/** trimChunks over a stand-in cache: `chunks` [size said, body bytes] oldest first; what stays. */
async function trimmed(
  chunks: [number | null, number][],
  storage: { quota: number; usage?: number },
  saved = new Set<string>(),
): Promise<number[]> {
  const { m, cache } = fakeCache();
  (cache as unknown as { keys: () => Promise<Request[]> }).keys = async () =>
    [...m.keys()].map((u) => new Request(u));
  const hex = (i: number) => i.toString(16).padStart(32, "0");
  for (const [i, [said, body]] of chunks.entries())
    m.set(
      `https://s/store/chunks/00/${hex(i)}`,
      new Response(new Uint8Array(body), { headers: said === null ? {} : { "x-ippai-size": String(said) } }),
    );
  const g = globalThis as unknown as { caches?: unknown };
  const old = g.caches;
  g.caches = { open: async () => cache };
  Object.defineProperty(navigator, "storage", {
    value: { estimate: async () => storage, persist: async () => true },
    configurable: true,
  });
  try {
    await trimChunks(async () => saved);
  } finally {
    g.caches = old;
  }
  return [...m.keys()].map((u) => parseInt(u.slice(-32), 16));
}

test("16 MiB chunks are counted as 16 MiB: 100 of them (1.6 GiB) are trimmed to the limit", async () => {
  const MiB = 1 << 20;
  // quota 1 GiB: the chunks' share is 512 MiB, so 32 of 16 MiB stay (the newest)
  const left = await trimmed(
    Array.from({ length: 100 }, () => [16 * MiB, 16] as [number, number]),
    { quota: 1024 * MiB },
  );
  expect(left.length).toBe(32);
  expect(left[0]).toBe(68);
});

test("what else the origin keeps (saves, states) counts: chunks give way so the whole fits", async () => {
  const MiB = 1 << 20;
  // quota 1000 MiB, 600 MiB of states besides 20 chunks of 16 MiB (320 MiB): 800 MiB in all
  // allowed, so 200 MiB of chunks (12) stay
  const left = await trimmed(
    Array.from({ length: 20 }, () => [16 * MiB, 16] as [number, number]),
    { quota: 1000 * MiB, usage: 920 * MiB },
  );
  expect(left.length).toBe(12);
});

test("a chunk kept without its size (an older page's) is measured, not guessed", async () => {
  // 3 chunks of 1 MiB with no header, quota 4 MiB (2 MiB for chunks): one goes
  const left = await trimmed(
    Array.from({ length: 3 }, () => [null, 1 << 20] as [null, number]),
    { quota: 4 << 20 },
  );
  expect(left).toEqual([1, 2]);
});

test("property: after a trim the chunks fit their share, only the oldest unsaved went", async () => {
  let seed = 99;
  const rnd = (n: number) => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff), seed % n);
  for (let run = 0; run < 100; run++) {
    const n = 1 + rnd(40);
    const sizes = Array.from({ length: n }, () => (1 + rnd(64)) << 10);
    const quota = (1 + rnd(4000)) << 10;
    const others = rnd(2) ? rnd(quota) : 0;
    const total = sizes.reduce((a, b) => a + b, 0);
    const saved = new Set(
      Array.from({ length: n }, (_, i) => i)
        .filter(() => rnd(5) === 0)
        .map((i) => i.toString(16).padStart(32, "0")),
    );
    const left = await trimmed(
      sizes.map((s) => [s, 0] as [number, number]),
      { quota, usage: others ? others + total : 0 },
      saved,
    );
    const limit = Math.min(quota * 0.5, others ? quota * 0.8 - others : Infinity);
    const isSaved = (i: number) => saved.has(i.toString(16).padStart(32, "0"));
    const kept = left.reduce((a, i) => a + sizes[i]!, 0);
    const savedBytes = sizes.reduce((a, s, i) => a + (isSaved(i) ? s : 0), 0);
    // fits (unless the saved games' chunks alone are over)
    expect(kept <= Math.max(limit, savedBytes) || kept === savedBytes, `run ${run}`).toBe(true);
    // every saved chunk stays; what went is a prefix of the unsaved, oldest first
    for (let i = 0; i < n; i++) if (isSaved(i)) expect(left).toContain(i);
    const gone = sizes.map((_, i) => i).filter((i) => !left.includes(i));
    const unsaved = sizes.map((_, i) => i).filter((i) => !isSaved(i));
    expect(gone, `run ${run}`).toEqual(unsaved.slice(0, gone.length));
    // and no more went than needed
    if (gone.length) expect(kept + sizes[gone.at(-1)!]! > limit, `run ${run}`).toBe(true);
  }
});

test("a tree's chunk lengths, for the read-ahead's reservations", async () => {
  const { treeChunkSizes } = await import("./net");
  const h = (c: string) => c.repeat(32);
  const tsv = `vnvm-tree\t2\t16777216\nd\t755\t0\tdir\nf\t644\t0\tdir/a\t16777236\t${h("a")}:16777216,:15,${h("b")}:5\nf\t644\t0\tempty\t0\t\nl\t777\t0\tlink\ta\n`;
  expect([...treeChunkSizes(tsv)]).toEqual([
    [h("a"), 16777216],
    [h("b"), 5],
  ]);
});
