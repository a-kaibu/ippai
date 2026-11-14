import { expect, test } from "vite-plus/test";
import source from "../../host/fetch-bridge.js?raw";
import { chunkFileName, EMPTY_STALE_MS, listChunks, opfsStore, type ChunkStore } from "./chunkstore";

// An OPFS folder as Chromium's behaves for what the stores use: a file exists (empty) from its
// creation, a writable's bytes land whole at close (abort: none), removeEntry of a file with an
// open read-only sync access handle refuses, sync handles read at an offset.
class FakeDir {
  files = new Map<string, Uint8Array>();
  open = new Map<string, number>();
  /** when each file was written last (ms), and the clock */
  times = new Map<string, number>();
  now = 1_000_000;
  /** files whose writables refuse to open (a read in the network worker holds them) */
  held = new Set<string>();
  async getFileHandle(name: string, opt: { create?: boolean } = {}) {
    if (!this.files.has(name)) {
      if (!opt.create) throw new DOMException("no", "NotFoundError");
      this.files.set(name, new Uint8Array(0));
      this.times.set(name, this.now);
    }
    return {
      kind: "file" as const,
      name,
      getFile: async () => {
        return new File([this.files.get(name)!.slice()], name, { lastModified: this.times.get(name) });
      },
      createWritable: async () => {
        if (this.held.has(name)) throw new DOMException("held", "NoModificationAllowedError");
        let parts: Uint8Array[] = [];
        return {
          write: async (d: Uint8Array) => {
            parts.push(d.slice());
          },
          close: async () => {
            const n = parts.reduce((a, p) => a + p.length, 0);
            const all = new Uint8Array(n);
            let at = 0;
            for (const p of parts) all.set(p, (at += p.length) - p.length);
            this.files.set(name, all);
            this.times.set(name, this.now);
          },
          abort: async () => {
            parts = [];
          },
        };
      },
      createSyncAccessHandle: async (o?: { mode?: string }) => {
        if (o?.mode !== "read-only") throw new Error("read-only only here");
        this.open.set(name, (this.open.get(name) ?? 0) + 1);
        let closed = false;
        return {
          getSize: () => this.files.get(name)?.length ?? 0,
          read: (buf: Uint8Array, { at }: { at: number }) => {
            const d = this.files.get(name) ?? new Uint8Array(0);
            const part = d.subarray(at, at + buf.length);
            buf.set(part);
            return part.length;
          },
          close: () => {
            if (closed) return;
            closed = true;
            this.open.set(name, this.open.get(name)! - 1);
          },
        };
      },
    };
  }
  async removeEntry(name: string) {
    if (!this.files.has(name)) throw new DOMException("no", "NotFoundError");
    if (this.open.get(name)) throw new DOMException("open", "NoModificationAllowedError");
    this.files.delete(name);
  }
  async *keys() {
    yield* Array.from(this.files.keys());
  }
  async *values() {
    for (const name of Array.from(this.files.keys())) yield await this.getFileHandle(name);
  }
}

/** Cache Storage as far as the stores use it. */
class FakeCache {
  m = new Map<string, { b: Uint8Array; h: Record<string, string> }>();
  async match(k: string) {
    const v = this.m.get(k);
    return v && new Response(v.b.slice(), { headers: v.h });
  }
  async put(k: string, r: Response) {
    this.m.set(k, { b: new Uint8Array(await r.arrayBuffer()), h: Object.fromEntries(r.headers) });
  }
  async delete(k: string) {
    return this.m.delete(k);
  }
  async keys() {
    return [...this.m.keys()].map((url) => ({ url }));
  }
}

const BASE = "https://masu.test/store/chunks/";
const hex = (i: number) => i.toString(16).padStart(32, "0");
const url = (i: number, block?: number) =>
  `${BASE}${hex(i).slice(0, 2)}/${hex(i)}${block === undefined ? "" : `?b=${block}&s=262144`}`;
const resp = (d: Uint8Array, checked = true) =>
  new Response(d.slice(), {
    headers: checked ? { "x-ippai-checked": "1", "x-ippai-size": String(d.length) } : {},
  });

// the network worker's copy, run as the worker runs it
interface BridgeStore {
  masuChunkFile(u: string): string;
  masuOpfsStore(
    dir: unknown,
    old: unknown,
  ): ChunkStore & {
    read(u: string, from: number, len: number): Promise<Uint8Array | null>;
    closeAll(): void;
  };
}
const bridge = (): BridgeStore =>
  // oxlint-disable-next-line typescript/no-implied-eval -- (the worker's script, as the worker runs it)
  new Function(`${source}; return { masuOpfsStore, masuChunkFile };`)() as BridgeStore;

test("file names: a chunk's hex, a block's hex and place; nothing else", () => {
  expect(chunkFileName(url(5))).toBe(hex(5));
  expect(chunkFileName(url(5, 3))).toBe(`${hex(5)}?b=3&s=262144`);
  expect(chunkFileName(`opfs:/ippai-chunks/${hex(7)}`)).toBe(hex(7));
  expect(chunkFileName(hex(7))).toBe(hex(7));
  // (a folder that is not the name's first two digits: not a chunk's place)
  expect(chunkFileName(`${BASE}cd/${hex(5)}`)).toBe("");
  expect(bridge().masuChunkFile(`${BASE}cd/${hex(5)}`)).toBe("");
  for (const u of [url(5), url(5, 3), `opfs:/ippai-chunks/${hex(7)}`, hex(7), `${BASE}xx`])
    expect(bridge().masuChunkFile(u), u).toBe(chunkFileName(u));
  for (const bad of [`${BASE}xx`, `${BASE}${hex(1)}.tmp`, `${BASE}../${hex(1)}x`, "https://masu.test/"])
    expect(chunkFileName(bad), bad).toBe("");
});

/**
 * Random puts, deletes and reads on the page's store and the worker's over one folder and one
 * old Cache Storage, against a model: what was put last reads back (whole chunks as checked,
 * blocks not), what was deleted is gone from both places, old entries read until put or
 * deleted again (then gone from the old cache), keys() names each once, and the worker's
 * synchronous reads give the same bytes as the page's.
 */
test("the page's and the worker's stores over one folder agree with a model (differential, random)", async () => {
  let x = 41;
  const rnd = (n: number) => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 8) % n;
  for (let run = 0; run < 30; run++) {
    const dir = new FakeDir();
    const old = new FakeCache();
    const model = new Map<string, { d: Uint8Array; checked: boolean }>();
    // (saved by an older page: in Cache Storage)
    for (let i = 0; i < 4; i++) {
      const d = Uint8Array.from({ length: 1 + rnd(300) }, () => rnd(256));
      await old.put(url(i), resp(d));
      model.set(url(i), { d, checked: true });
    }
    const page = opfsStore(dir as unknown as FileSystemDirectoryHandle, old as unknown as Cache);
    const worker = bridge().masuOpfsStore(dir, old);
    for (let step = 0; step < 80; step++) {
      const s = rnd(2) ? page : worker;
      const k = rnd(3) ? url(rnd(10)) : url(rnd(10), rnd(4));
      const op = rnd(5);
      if (op <= 1) {
        const d = Uint8Array.from({ length: 1 + rnd(400) }, () => rnd(256));
        await s.put(k, resp(d, !k.includes("?")));
        model.set(k, { d, checked: !k.includes("?") });
        expect(old.m.has(k), `${run}/${step}: kept again, not in the old cache`).toBe(false);
      } else if (op === 2) {
        worker.closeAll();
        await s.delete(k);
        model.delete(k);
        expect(old.m.has(k)).toBe(false);
      } else {
        const want = model.get(k);
        const r = await s.match(k);
        if (!want) expect(r, `${run}/${step} ${k}`).toBeUndefined();
        else {
          expect(new Uint8Array(await r!.arrayBuffer()), `${run}/${step} ${k}`).toEqual(want.d);
          expect(r!.headers.get("x-ippai-checked") === "1").toBe(want.checked);
          expect(Number(r!.headers.get("x-ippai-size"))).toBe(want.d.length);
        }
        // (the worker's quick read, a part)
        const from = rnd(200);
        const got = await worker.read(k, from, 100);
        if (want && dir.files.get(chunkFileName(k))?.length)
          expect(got).toEqual(want.d.subarray(from, from + 100));
        else if (!dir.files.has(chunkFileName(k))) expect(got).toBeNull();
      }
    }
    // (one in both places, as a page that kept it in both left it: named once)
    const both = [...model.keys()].find((k) => dir.files.has(chunkFileName(k)) && !k.includes("?"));
    if (both) await old.put(both, resp(model.get(both)!.d));
    const names = (await page.keys()).map((k) => chunkFileName(k));
    expect(new Set(names).size).toBe(names.length);
    expect(new Set(names)).toEqual(new Set([...model.keys()].map((k) => chunkFileName(k))));
    // (list: the same ones, each with its bytes, once; listChunks over the old cache alone: its own)
    const listed = await page.list!();
    expect(listed.length).toBe(model.size);
    for (const k of listed) {
      const m = [...model.entries()].find(([u]) => chunkFileName(u) === chunkFileName(k));
      expect(k.size, k.url).toBe(m![1].d.length);
    }
    const oldList = await listChunks(old as unknown as ChunkStore);
    expect(oldList.map((k) => k.url).sort()).toEqual([...old.m.keys()].sort());
    for (const k of oldList) expect(k.size).toBe(old.m.get(k.url)!.b.length);
    worker.closeAll();
  }
});

test("a file being written reads as not there; a write cut short leaves none", async () => {
  const dir = new FakeDir();
  const page = opfsStore(dir as unknown as FileSystemDirectoryHandle, null);
  const worker = bridge().masuOpfsStore(dir, null);
  // (created, not closed yet: empty)
  await dir.getFileHandle(hex(1), { create: true });
  expect(await page.match(url(1))).toBeUndefined();
  expect(await worker.match(url(1))).toBeUndefined();
  expect(await worker.read(url(1), 0, 10)).toBeNull();
  // (an unchecked whole chunk is not kept as checked)
  await page.put(url(2), resp(new Uint8Array([1, 2, 3]), false));
  expect(dir.files.has(hex(2))).toBe(false);
  await worker.put(url(2), resp(new Uint8Array([1, 2, 3]), false));
  expect(dir.files.has(hex(2))).toBe(false);
  worker.closeAll();
});

test("the worker's handles: kept a while for the next reads, closed so a delete goes through", async () => {
  const dir = new FakeDir();
  const worker = bridge().masuOpfsStore(dir, null);
  await worker.put(url(3), resp(new Uint8Array(1000).fill(7)));
  for (let i = 0; i < 5; i++)
    expect(await worker.read(url(3), i * 100, 100)).toEqual(new Uint8Array(100).fill(7));
  expect(dir.open.get(hex(3))).toBe(1);
  // (the page's delete meanwhile: refused while open; the worker's own delete closes first)
  const page = opfsStore(dir as unknown as FileSystemDirectoryHandle, null);
  expect(await page.delete(url(3))).toBe(false);
  expect(dir.files.has(hex(3))).toBe(true);
  expect(await worker.delete(url(3))).toBe(true);
  expect(dir.files.has(hex(3))).toBe(false);
  worker.closeAll();
});

test("list: sizes and times as written, an empty file not listed (gone once stale); a put to a file a read holds is a no-op", async () => {
  const dir = new FakeDir();
  const page = opfsStore(dir as unknown as FileSystemDirectoryHandle, null);
  const t0 = (dir.now = Date.now());
  await page.put(url(1), resp(new Uint8Array([1, 2, 3])));
  dir.now += 5000;
  await page.put(url(2), resp(new Uint8Array(7)));
  // (a write cut short by a killed tab: an empty file)
  await dir.getFileHandle(hex(3), { create: true });
  // (an empty body: nothing kept)
  await page.put(url(4), resp(new Uint8Array(0)));
  expect(dir.files.has(hex(4))).toBe(false);
  let l = await page.list!();
  expect(l.map((k) => [chunkFileName(k), k.size, k.time])).toEqual([
    [hex(1), 3, t0],
    [hex(2), 7, t0 + 5000],
  ]);
  expect(dir.files.has(hex(3))).toBe(true);
  dir.now += EMPTY_STALE_MS + 1;
  const real = Date.now;
  Date.now = () => dir.now;
  try {
    l = await page.list!();
  } finally {
    Date.now = real;
  }
  expect(l.length).toBe(2);
  expect(dir.files.has(hex(3)), "a stale empty file goes").toBe(false);
  dir.held.add(hex(1));
  await page.put(url(1), resp(new Uint8Array([9])));
  expect(dir.files.get(hex(1))).toEqual(new Uint8Array([1, 2, 3]));
});
