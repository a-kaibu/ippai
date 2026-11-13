// A game not yet on the device is saved to it whole before it starts (web/src/ui/SaveFirst.tsx,
// offline.ts saveOffline, direct.ts), in Chromium on ippai serve with a small store and a bucket
// stood in by the test (page.route: /store-direct answers, many at once, with a URL on https://bucket.test and
// the object's key; the bucket answers objects sealed as crates/ippai/src/envelope.rs seals them,
// in zstd frames, each held back 300 ms as the real one holds them ~2 s):
// - picking the game opens the dialog, which saves it: the bucket gets many at once (more than the
//   server's 4), the chunks only on the server come through /store at most 4 at once, every chunk
//   of the game and the prefix ends in the chunk cache with the server's bytes; then 遊ぶ starts it;
// - picked again: it starts at once (no dialog);
// - another game's 保存せずに遊ぶ: starts at once, the save stopped (続きから next time).
// SHOTS=<dir> keeps screenshots of the dialog. With web/dist/sake/snapz.mjs (sake built) the real zstd decodes the objects; without, a stand-in
// that reads the frames' raw blocks.
//
//   node tests/save-first-browser.mjs [path to ippai]   (web/dist built; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { execFileSync, spawn } from "node:child_process";
import { createCipheriv, randomBytes, randomUUID } from "node:crypto";
import { cpSync, existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { get } from "node:http";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { zstdDecompressSync } from "node:zlib";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-savefirst-"));
const web = join(work, "web");
cpSync(resolve("web/dist"), web, { recursive: true });
if (!existsSync(join(web, "sake/sake.wasm"))) {
  mkdirSync(join(web, "sake"), { recursive: true });
  writeFileSync(join(web, "sake/sake.js"), "export default () => {};\n");
  writeFileSync(join(web, "sake/sake.wasm"), new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0]));
  writeFileSync(join(web, "sake/jit-version.txt"), "00000000\n");
}
const realZstd = existsSync(join(web, "sake/snapz.mjs"));
if (!realZstd)
  // (a frame of raw blocks only, as rawFrame below writes them)
  writeFileSync(
    join(web, "sake/snapz.mjs"),
    `export default async () => {
  const HEAPU8 = new Uint8Array(100 << 20);
  let top = 8, live = 0;
  return {
    HEAPU8,
    _malloc(n) { const p = top; top += (n + 7) & ~7; if (top > HEAPU8.length) { top = p; return 0; } live++; return p; },
    _free(p) { if (p && --live === 0) top = 8; },
    _snapz_decompress(dst, cap, src, n) {
      const h = HEAPU8;
      if (h[src] !== 0x28 || h[src + 1] !== 0xb5 || h[src + 2] !== 0x2f || h[src + 3] !== 0xfd || h[src + 4] !== 0xa0) return 0xffffffff;
      let at = src + 9, out = 0;
      for (;;) {
        const b = h[at] | (h[at + 1] << 8) | (h[at + 2] << 16);
        const size = b >>> 3;
        if (((b >>> 1) & 3) !== 0 || out + size > cap || at + 3 + size > src + n) return 0xffffffff;
        h.copyWithin(dst + out, at + 3, at + 3 + size);
        out += size;
        at += 3 + size;
        if (b & 1) return out;
      }
    },
  };
};
`,
  );
const store = join(work, "store");
// SHOTS=<dir>: screenshots of the dialog while it saves and when done
const shots = process.env.SHOTS && resolve(process.env.SHOTS);
if (shots) mkdirSync(shots, { recursive: true });
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });

/** `n` bytes that do not compress (an LCG from `seed`). */
function noise(n, seed) {
  let x = seed >>> 0;
  return Uint8Array.from({ length: n }, () => ((x = (Math.imul(x, 1103515245) + 12345) >>> 0) >>> 16) & 255);
}
const prefix = join(work, "prefix/drive_c/windows");
mkdirSync(join(prefix, "Fonts"), { recursive: true });
writeFileSync(join(prefix, "Fonts/font.ttf"), noise(200 << 10, 1));
run("pack", join(work, "prefix"), "prefix32");
let seed = 10;
for (const [id, title, kib] of [
  ["g", "先に保存するゲーム", [900, 700, 500, 300, 200]],
  ["h", "すぐ遊ぶゲーム", [2000, 2000, 2000]],
]) {
  const dir = join(work, id);
  mkdirSync(join(dir, "data"), { recursive: true });
  kib.forEach((k, i) => writeFileSync(join(dir, "data", `${i}.bin`), noise(k << 10, seed++)));
  writeFileSync(join(dir, "game.exe"), `MZ ${id}`);
  // (small chunks: many objects)
  run("pack", dir, id, "--chunk-kib", "64");
  run("add-game", id, "--title", title, "--tree", id, "--exe", "game.exe", "--no-meta");
}

const port = 9500 + Math.floor(Math.random() * 200) * 2;
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", web, "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit" },
);
process.on("exit", () => server.kill());
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(`${base}play.html`).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}

/** The chunks of trees `names` (trees/<name>.tsv: "f" lines, <hash>:<length> in column 6). */
async function chunksOf(...names) {
  const out = new Set();
  for (const n of names) {
    const tsv = await (await fetch(`${base}store/trees/${n}.tsv`)).text();
    for (const l of tsv.split("\n").slice(1)) {
      const f = l.split("\t");
      if (f[0] === "f" && f[5]) for (const c of f[5].split(",")) if (/^[0-9a-f]{32}$/.test(c.split(":")[0])) out.add(c.split(":")[0]);
    }
  }
  return out;
}
const chunkUrl = (h) => `${base}store/chunks/${h.slice(0, 2)}/${h}`;
/** A zstd frame of raw blocks (single segment, its size in 4 bytes, no checksum). */
function rawFrame(d) {
  const parts = [Buffer.from([0x28, 0xb5, 0x2f, 0xfd, 0xa0]), Buffer.alloc(4)];
  parts[1].writeUInt32LE(d.length);
  const B = 128 << 10;
  for (let at = 0; ; at += B) {
    const n = Math.min(B, d.length - at);
    const last = at + n >= d.length;
    const h = (n << 3) | (last ? 1 : 0);
    parts.push(Buffer.from([h & 255, (h >> 8) & 255, (h >> 16) & 255]), Buffer.from(d.subarray(at, at + n)));
    if (last) break;
  }
  return Buffer.concat(parts);
}
const seal = (key, object, plain) => {
  const nonce = randomBytes(12);
  const c = createCipheriv("aes-256-gcm", key, nonce);
  c.setAAD(Buffer.from(object.replace(/-/g, ""), "hex"));
  return Buffer.concat([nonce, c.update(plain), c.final(), c.getAuthTag()]);
};

const gChunks = await chunksOf("prefix32", "g");
const hChunks = await chunksOf("h");
// (a few only on the server: /store-direct leaves them out, or says 404 asked alone)
const serverOnly = new Set([...gChunks].slice(0, 6));
const decoded = new Map();
// (the store keeps chunks zstd-coded and sends them so, Content-Encoding: zstd. Node 26's fetch
// decodes that itself and keeps the header, so the bytes are read off the wire here)
const raw = (url) =>
  new Promise((ok, fail) =>
    get(url, (r) => {
      const parts = [];
      r.on("data", (d) => parts.push(d));
      r.on("end", () => ok({ encoding: r.headers["content-encoding"], body: Buffer.concat(parts) }));
      r.on("error", fail);
    }).on("error", fail),
  );
for (const h of [...gChunks, ...hChunks]) {
  const r = await raw(chunkUrl(h));
  decoded.set(h, new Uint8Array(r.encoding === "zstd" ? zstdDecompressSync(r.body) : r.body));
}

const browser = await chromium.launchPersistentContext(join(work, "profile"), {
  args: ["--enable-unsafe-webgpu"],
  viewport: { width: 1280, height: 800 },
  serviceWorkers: "block",
});
try {
  const page = browser.pages()[0] ?? (await browser.newPage());
  const objects = new Map();
  const count = { bucket: 0, bucketNow: 0, bucketMost: 0, server: 0, serverNow: 0, serverMost: 0, serverOnlyAsked: 0, many: 0, one: 0, pack: 0, packChunks: 0 };
  // (each chunk sealed once, as the bucket keeps it)
  const sealed = new Map();
  const sealOnce = (h) => {
    if (!sealed.has(h)) {
      const key = randomBytes(32);
      const object = randomUUID();
      objects.set(object, seal(key, object, rawFrame(decoded.get(h))));
      sealed.set(h, { key, object });
    }
    return sealed.get(h);
  };
  // (a delivery pack of the game's last PACKED chunks, in its order: crates/ippai/src/storage.rs
  // pack_objects; the save reads them with ranged GETs of it, several a GET)
  const PACK = "0f0e0d0c-0b0a-4908-8706-050403020100";
  const PACKED = 20;
  const packed = [...gChunks].filter((h) => !serverOnly.has(h)).slice(-PACKED);
  const at = new Map();
  let off = 0;
  for (const h of packed) {
    const o = objects.get(sealOnce(h).object);
    at.set(h, { offset: off, len: o.length });
    off += o.length;
  }
  const packBody = Buffer.concat(packed.map((h) => objects.get(sealOnce(h).object)));
  const answer = (h) => {
    const { key, object } = sealOnce(h);
    return {
      url: `https://bucket.test/${object}`,
      key: key.toString("hex"),
      object,
      ...(at.has(h) ? { pack: { id: PACK, url: `https://bucket.test/pack/${PACK}`, ...at.get(h) } } : {}),
    };
  };
  await page.route(`https://bucket.test/pack/${PACK}`, async (route) => {
    count.pack++;
    const m = /^bytes=(\d+)-(\d+)$/.exec(route.request().headers()["range"] ?? "");
    const body = m ? packBody.subarray(+m[1], +m[2] + 1) : packBody;
    count.packChunks += packed.filter((h) => m && at.get(h).offset >= +m[1] && at.get(h).offset + at.get(h).len <= +m[2] + 1).length;
    await route.fulfill({
      status: m ? 206 : 200,
      body,
      headers: { "access-control-allow-origin": "*", "access-control-expose-headers": "content-range", ...(m ? { "content-range": `bytes ${m[1]}-${+m[1] + body.length - 1}/${packBody.length}` } : {}) },
    });
  });
  // (many at once, as ippai serve's POST /store-direct answers: the server's own left out)
  await page.route(/\/store-direct$/, async (route) => {
    if (route.request().method() !== "POST") return route.continue();
    count.many++;
    const out = {};
    for (const p of route.request().postDataJSON()) {
      const h = p.slice(-32);
      if (!serverOnly.has(h) && decoded.has(h)) out[p] = answer(h);
    }
    await route.fulfill({ json: out });
  });
  await page.route(/\/store-direct\/chunks\//, async (route) => {
    const h = route.request().url().slice(-32);
    count.one++;
    if (serverOnly.has(h)) {
      count.serverOnlyAsked++;
      return route.fulfill({ status: 404, body: "" });
    }
    await route.fulfill({ json: answer(h) });
  });
  await page.route(/^https:\/\/bucket\.test\/[0-9a-f-]{36}$/, async (route) => {
    count.bucket++;
    count.bucketMost = Math.max(count.bucketMost, ++count.bucketNow);
    await new Promise((r) => setTimeout(r, 300));
    count.bucketNow--;
    const body = objects.get(route.request().url().slice("https://bucket.test/".length));
    await route.fulfill({ status: body ? 200 : 404, body: body ?? "", headers: { "access-control-allow-origin": "*" } });
  });
  await page.route(/\/store\/chunks\//, async (route) => {
    count.server++;
    count.serverMost = Math.max(count.serverMost, ++count.serverNow);
    await new Promise((r) => setTimeout(r, 300));
    count.serverNow--;
    await route.continue();
  });
  const dialog = page.getByTestId("save-first");
  const game = () => new URL(page.url()).searchParams.get("game");

  // 1. picked: saved first, then 遊ぶ
  await page.goto(`${base}play.html`);
  await page.getByRole("button", { name: "先に保存するゲーム を起動" }).click();
  await dialog.waitFor();
  assert.equal(game(), null, "not started before it is saved");
  await dialog.getByText(/丸ごとこの端末に保存してから遊びます/).waitFor();
  await dialog.getByRole("progressbar", { name: "保存の進み" }).waitFor({ timeout: 30000 });
  if (shots) await page.screenshot({ path: join(shots, "saving.png") });
  await dialog.getByRole("button", { name: "遊ぶ", exact: true }).waitFor({ timeout: 120000 });
  if (shots) await page.screenshot({ path: join(shots, "done.png") });
  console.log(`save first: ${JSON.stringify(count)} (${gChunks.size} chunks, real zstd: ${realZstd})`);
  // (the addresses asked for in batches: none one by one, the server's own known from them)
  assert.ok(count.many >= 1 && count.many <= Math.ceil(gChunks.size / 256), `batches: ${count.many}`);
  assert.equal(count.one, 0, "no chunk asked of /store-direct one by one");
  // (the packed ones from the pack, several a GET; the rest from the bucket alone)
  assert.equal(count.packChunks, PACKED, "every packed chunk from the pack");
  assert.ok(count.pack >= 1 && count.pack < PACKED / 2, `pack GETs: ${count.pack}`);
  assert.equal(count.bucket, gChunks.size - serverOnly.size - PACKED, "the rest from the bucket alone");
  assert.ok(count.bucketMost > 4, `the bucket many at once: ${count.bucketMost}`);
  assert.equal(count.server, serverOnly.size, "only the server's own chunks through /store");
  assert.ok(count.serverMost <= 4, `the server at most 4 at once: ${count.serverMost}`);
  const kept = await page.evaluate(async (hashes) => {
    const cache = await caches.open("ippai-chunks-v1");
    const out = {};
    for (const h of hashes) {
      const r = await cache.match(new URL(`store/chunks/${h.slice(0, 2)}/${h}`, location.href).href);
      out[h] = r ? Array.from(new Uint8Array(await r.arrayBuffer()).subarray(0, 64)).join(",") + `/${r.headers.get("x-ippai-size")}` : null;
    }
    return out;
  }, [...gChunks]);
  for (const h of gChunks) {
    const d = decoded.get(h);
    assert.equal(kept[h], `${Array.from(d.subarray(0, 64)).join(",")}/${d.length}`, `chunk ${h} on the device as the server has it`);
  }
  const saved = () => page.evaluate(() => JSON.parse(localStorage.getItem("ippai-offline") ?? "{}"));
  assert.equal((await saved()).g?.status, "done");
  await dialog.getByRole("button", { name: "遊ぶ", exact: true }).click();
  await page.waitForURL((u) => u.searchParams.get("game") === "g");
  assert.equal(await dialog.count(), 0, "the dialog gone");

  // 2. picked again: at once
  await page.goBack();
  await page.waitForURL((u) => !u.searchParams.has("game"));
  const before = count.bucket + count.server;
  await page.getByRole("button", { name: "先に保存するゲーム を起動" }).click();
  await page.waitForURL((u) => u.searchParams.get("game") === "g");
  assert.equal(await dialog.count(), 0, "no dialog for a saved game");

  // 3. 保存せずに遊ぶ: at once, the save stopped
  await page.goBack();
  await page.waitForURL((u) => !u.searchParams.has("game"));
  await page.getByRole("button", { name: "すぐ遊ぶゲーム を起動" }).click();
  await dialog.getByRole("progressbar", { name: "保存の進み" }).waitFor({ timeout: 30000 });
  await dialog.getByRole("button", { name: "保存せずに遊ぶ" }).click();
  await page.waitForURL((u) => u.searchParams.get("game") === "h");
  assert.equal(await dialog.count(), 0);
  // (the stop is seen once the fetches going on end)
  for (let i = 0; i < 50 && (await saved()).h?.status === "saving"; i++) await page.waitForTimeout(100);
  assert.equal((await saved()).h?.status, "stopped");
  assert.ok(count.bucket + count.server - before < hChunks.size, "not all of it fetched");
  console.log("save-first browser: saved whole before play (bucket many at once, server at most 4), saved game at once, 保存せずに遊ぶ passed");
} finally {
  await browser.close();
  server.kill();
  rmSync(work, { recursive: true, force: true });
}
