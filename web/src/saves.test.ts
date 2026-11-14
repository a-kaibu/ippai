import { afterAll, beforeAll, expect, test, vi } from "vite-plus/test";
import { keepSaving, loadSaves, savesFor, storeSaves, type SaveFiles, type SavesModule } from "./saves";

// The saved-game blobs across pages and devices (saves.ts): a model of ippai serve's /saves
// (crates/ippai/src/saves.rs: a revision per name, If-Match, its revisions kept) and of each
// device's OPFS; a "page" is a fresh transport over the same files.

class Server {
  rev = 0;
  data: Uint8Array | null = null;
  history: Uint8Array[] = [];
  up = true;
  request: typeof fetch = async (_url, init) => {
    if (!this.up || !online) throw new TypeError("Failed to fetch");
    if (init?.method === "PUT") {
      const want = Number(new Headers(init.headers).get("if-match")?.replaceAll('"', ""));
      if (want !== this.rev) return new Response(null, { status: 412 });
      this.data = new Uint8Array(init.body as Uint8Array);
      this.history.push(this.data);
      this.rev++;
      return new Response(null, { status: 204, headers: { etag: `"${this.rev}"` } });
    }
    if (!this.data) return new Response(null, { status: 404 });
    return new Response(this.data.slice(), { headers: { etag: `"${this.rev}"` } });
  };
}

class Device {
  m = new Map<string, Uint8Array>();
  files: SaveFiles = {
    read: async (f) => {
      const v = this.m.get(f);
      return v ? (v.slice() as Uint8Array<ArrayBuffer>) : null;
    },
    write: async (f, d) => void this.m.set(f, d.slice()),
  };
  /** what this browser keeps of the game: its blob and anything kept aside */
  kept(): Uint8Array[] {
    return [...this.m.entries()].filter(([k]) => k.endsWith(".bin")).map(([, v]) => v);
  }
}

const g = globalThis as unknown as { location?: unknown };
const oldLocation = g.location;
let online = true;
beforeAll(() => {
  g.location = { href: "https://ippai.test/play.html" };
  Object.defineProperty(navigator, "onLine", { get: () => online, configurable: true });
});
afterAll(() => {
  g.location = oldLocation;
});

const blob = (n: number) => new Uint8Array([n & 255, n >> 8]);
const eq = (a: Uint8Array | null | undefined, b: Uint8Array | null | undefined) =>
  !!a && !!b && a.length === b.length && a.every((x, i) => x === b[i]);
/** A new page on `d`: the start (loadSaves) and what it returned. */
async function page(d: Device, s: Server, notice?: (m: string) => void) {
  savesFor(d.files, s.request);
  return loadSaves("game", notice);
}
const save = (n: number) =>
  storeSaves("game", blob(n)).then(
    () => true,
    () => false,
  );

test("a save made offline is not lost to the server's older copy at the next online start", async () => {
  const s = new Server();
  const phone = new Device();
  online = true;
  await page(phone, s);
  expect(await save(1)).toBe(true);
  expect(eq(s.data, blob(1))).toBe(true);
  // offline: the game saves again, the page closes
  online = false;
  expect(eq(await page(phone, s), blob(1))).toBe(true);
  expect(await save(2)).toBe(false);
  expect(eq(s.data, blob(1))).toBe(true);
  // a later day, online: this device's save 2, and the server gets it
  online = true;
  expect(eq(await page(phone, s), blob(2))).toBe(true);
  expect(eq(s.data, blob(2))).toBe(true);
  // and the start after that is the server's, as usual
  const pc = new Device();
  expect(eq(await page(pc, s), blob(2))).toBe(true);
});

test("a page that closed before its upload was acknowledged sends its save at the next start", async () => {
  const s = new Server();
  const phone = new Device();
  online = true;
  await page(phone, s);
  await save(1);
  // the server takes nothing more (the page is closing: its PUT never lands)
  s.up = false;
  expect(await save(2)).toBe(false);
  s.up = true;
  expect(eq(await page(phone, s), blob(2))).toBe(true);
  expect(eq(s.data, blob(2))).toBe(true);
});

test("saved here offline and on another device meanwhile: this device's wins, the other's is kept", async () => {
  const s = new Server();
  const phone = new Device();
  const pc = new Device();
  online = true;
  await page(phone, s);
  await save(1);
  online = false;
  await page(phone, s);
  await save(2);
  online = true;
  await page(pc, s);
  await save(3);
  const notices: string[] = [];
  expect(eq(await page(phone, s, (m) => notices.push(m)), blob(2))).toBe(true);
  expect(notices.length).toBe(1);
  expect(eq(s.data, blob(2))).toBe(true);
  // the other device's save 3: on this device beside the blob, and in the server's revisions
  expect(phone.kept().some((b) => eq(b, blob(3)))).toBe(true);
  expect(s.history.some((b) => eq(b, blob(3)))).toBe(true);
});

test("a game started offline whose server copy moved meanwhile does not write over it", async () => {
  const s = new Server();
  const phone = new Device();
  const pc = new Device();
  online = true;
  await page(phone, s);
  await save(1);
  // started offline on the phone; the pc saves; the phone's link comes back mid-game
  online = false;
  await page(phone, s);
  online = true;
  savesFor(pc.files, s.request);
  await loadSaves("game");
  await save(5);
  savesFor(phone.files, s.request);
  online = false;
  await loadSaves("game");
  online = true;
  expect(await save(2)).toBe(false);
  expect(eq(s.data, blob(5))).toBe(true);
  // the next start: the phone's save 2 (made after), the pc's 5 kept on the phone
  expect(eq(await page(phone, s), blob(2))).toBe(true);
  expect(phone.kept().some((b) => eq(b, blob(5)))).toBe(true);
});

// Random histories of two devices (pages started on- or offline, saves, uploads lost, the
// server down): the last save made on each device is never lost (it is on the server, in its
// revisions, or on that device), and a device whose last save was not sent, with nobody else
// saving since, starts from that save and sends it.
test("property: no device's last save is ever lost", async () => {
  let seed = 12345;
  const rnd = (n: number) => {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    return seed % n;
  };
  let value = 0;
  for (let run = 0; run < 400; run++) {
    const s = new Server();
    const ds = [new Device(), new Device()];
    const last: (Uint8Array | null)[] = [null, null];
    /** device that saved last, and whether that save is on the server */
    let newest = -1;
    let sent = true;
    for (let step = 0; step < 12; step++) {
      const i = rnd(2);
      const d = ds[i]!;
      online = rnd(3) !== 0;
      s.up = true;
      const got = await page(d, s);
      if (online && newest === i && !sent) {
        expect(eq(got, last[i]), `run ${run} step ${step}: the unsent save`).toBe(true);
        expect(eq(s.data, last[i]), `run ${run} step ${step}: sent`).toBe(true);
        sent = true;
      }
      for (let k = rnd(3); k > 0; k--) {
        if (rnd(4) === 0) s.up = false;
        const v = blob(++value);
        const ok = await save(value);
        last[i] = v;
        newest = i;
        sent = ok && eq(s.data, v);
      }
      for (const [j, v] of last.entries()) {
        if (!v) continue;
        const where = [...s.history, ...ds[j]!.kept()];
        expect(
          where.some((b) => eq(b, v)),
          `run ${run} step ${step}: device ${j}'s last save`,
        ).toBe(true);
      }
    }
  }
  online = true;
});

test("an older page's copy, not known to be sent, is kept aside when the server's is taken", async () => {
  const s = new Server();
  const pc = new Device();
  online = true;
  await page(pc, s);
  await save(7);
  // (a page before saves.ts kept the sync state: a blob and nothing beside it)
  const phone = new Device();
  phone.m.set("game.bin", blob(6));
  expect(eq(await page(phone, s), blob(7))).toBe(true);
  expect(phone.kept().some((b) => eq(b, blob(6)))).toBe(true);
  expect(eq(phone.m.get("game.bin"), blob(7))).toBe(true);
});

/** keepSaving over `d` and `s` with a game whose files are `files()` (the export), a clock the
 * test moves, and the 5 s timer and page events left out (the test calls the save itself). */
async function keeper(d: Device, s: Server, files: () => Uint8Array<ArrayBuffer>, last: Uint8Array | null) {
  // (a page's start first: it learns the server's revision, or that it was out of reach)
  await page(d, s);
  let changes = 0;
  let t = 0;
  let reads = 0;
  const m = { _vnhost_vfs_changes: () => changes } as unknown as SavesModule;
  const g = globalThis as unknown as { addEventListener?: unknown };
  const had = g.addEventListener;
  g.addEventListener ??= () => {};
  vi.useFakeTimers();
  const save = keepSaving(m, "game", [], last, {
    now: () => t,
    read: () => (reads++, files()),
  });
  vi.useRealTimers();
  if (!had) delete g.addEventListener;
  let writes = 0;
  const write = d.files.write.bind(d.files);
  d.files.write = async (f, data) => {
    if (f.endsWith(".bin")) writes++;
    return write(f, data);
  };
  return {
    save,
    write: () => void changes++,
    at: (ms: number) => void (t = ms),
    counts: () => ({ reads, writes, puts: s.history.length }),
  };
}

test("offline, a changed save is written to this browser once, and the upload backs off", async () => {
  const s = new Server();
  const d = new Device();
  online = true;
  let current = blob(1);
  const k = await keeper(d, s, () => current as Uint8Array<ArrayBuffer>, null);
  s.up = false;
  current = blob(2);
  k.write();
  await k.save();
  expect(k.counts()).toEqual({ reads: 1, writes: 1, puts: 0 });
  expect(eq(d.m.get("game.bin"), blob(2))).toBe(true);
  // (nothing new written by the game, and the next try not due: no export, no write, no request)
  for (let i = 1; i <= 5; i++) {
    k.at(i * 1000);
    await k.save();
  }
  expect(k.counts()).toEqual({ reads: 1, writes: 1, puts: 0 });
  // (due again: the upload only, not this browser's copy again)
  s.up = true;
  k.at(60_000);
  await k.save();
  expect(k.counts()).toEqual({ reads: 1, writes: 1, puts: 1 });
  expect(eq(s.data, blob(2))).toBe(true);
  // (sent: nothing more to do)
  k.at(120_000);
  await k.save();
  expect(k.counts().puts).toBe(1);
});

test("offline, the game writing the same bytes again makes no new local write", async () => {
  const s = new Server();
  const d = new Device();
  online = true;
  s.up = false;
  const k = await keeper(d, s, () => blob(3) as Uint8Array<ArrayBuffer>, blob(1));
  for (let i = 0; i < 4; i++) {
    k.write();
    k.at(i * 10);
    await k.save();
  }
  expect(k.counts()).toEqual({ reads: 4, writes: 1, puts: 0 });
  s.up = true;
});

test("a forced save (the game quits) uploads at once, even in the pause after a failure", async () => {
  const s = new Server();
  const d = new Device();
  online = true;
  s.up = false;
  const k = await keeper(d, s, () => blob(4) as Uint8Array<ArrayBuffer>, blob(1));
  k.write();
  await k.save();
  s.up = true;
  k.at(1);
  await k.save();
  expect(k.counts().puts).toBe(0);
  await k.save(true);
  expect(k.counts().puts).toBe(1);
  expect(eq(s.data, blob(4))).toBe(true);
});

test("saves asked for while one is stuck wait as one, not one per tick", async () => {
  const s = new Server();
  const d = new Device();
  online = true;
  let release = () => {};
  const stuck = new Promise<void>((r) => (release = r));
  const request = s.request;
  s.request = async (u, init) => {
    if (init?.method === "PUT") await stuck;
    return request(u, init);
  };
  let current = blob(5);
  const k = await keeper(d, s, () => current as Uint8Array<ArrayBuffer>, blob(1));
  k.write();
  const first = k.save();
  const waiting = Array.from({ length: 50 }, () => k.save());
  // (all the waiting ones are the same promise)
  expect(new Set(waiting).size).toBe(1);
  current = blob(6);
  k.write();
  release();
  await Promise.all([first, ...waiting]);
  expect(k.counts().reads).toBe(2);
  expect(eq(s.data, blob(6))).toBe(true);
});
