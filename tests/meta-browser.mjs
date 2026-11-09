// A game's information from a page the user pastes (web/src/ui/Library.tsx MetaSource,
// crates/ippai/src/library_api.rs, meta_url.rs, meta.rs) in Chromium, on ippai serve with a
// small store and local stand-ins for 批評空間's SQL form and VNDB's API (IPPAI_META_EGS_SQL,
// IPPAI_META_VNDB; its game list: max(id), then windows of ids): something that is no game page is refused on the page and by the server;
// a 批評空間 page fills the game's dialog and card (title, brand, year, score, tags) without a
// reload, and stays after one; a source that fails says why and keeps what was there; a VNDB page
// brings its cover, which the card then shows; 候補を探す lists the list's games like a name and
// picks one; `ippai meta` identifies a game by its names from the list (egs_match.rs) without a
// search. SHOTS=<dir> keeps screenshots.
//
//   node tests/meta-browser.mjs [path to ippai]   (web/dist built; PLAYWRIGHT as elsewhere)
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { deflateSync } from "node:zlib";
const pw = await import(process.env.PLAYWRIGHT ?? "playwright");
const { chromium } = pw.default ?? pw;

const bin = resolve(process.argv[2] ?? "target/debug/ippai");
const work = mkdtempSync(join(tmpdir(), "ippai-meta-"));
const store = join(work, "store");
const run = (...a) => execFileSync(bin, ["--store", store, ...a], { stdio: "pipe" });
const shots = process.env.SHOTS && resolve(process.env.SHOTS);
if (shots) mkdirSync(shots, { recursive: true });

for (const [id, title] of [["anemoi", "anemoi"], ["ever17", "ever17"], ["loopers", "LOOPERS DL版"], ["hoshi", "hoshi"]]) {
  const dir = join(work, id);
  mkdirSync(dir);
  writeFileSync(join(dir, "game.exe"), `MZ ${id}`);
  run("pack", dir, id);
  run("add-game", id, "--title", title, "--tree", id, "--exe", "game.exe", "--no-meta");
}

// a 64x64 PNG of one color (the cover VNDB's stand-in gives)
function png(r, g, b) {
  const crc = (buf) => {
    let c = ~0;
    for (const x of buf) {
      c ^= x;
      for (let k = 0; k < 8; k++) c = (c >>> 1) ^ (0xedb88320 & -(c & 1));
    }
    return ~c >>> 0;
  };
  const chunk = (type, data) => {
    const out = Buffer.alloc(12 + data.length);
    out.writeUInt32BE(data.length, 0);
    out.write(type, 4, "latin1");
    data.copy(out, 8);
    out.writeUInt32BE(crc(out.subarray(4, 8 + data.length)), 8 + data.length);
    return out;
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(64, 0);
  ihdr.writeUInt32BE(64, 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: 64 }, () => [r, g, b]).flat())]);
  const raw = Buffer.concat(Array.from({ length: 64 }, () => row));
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}
const cover = png(200, 40, 90);

// the stand-ins: 批評空間's form answers with an HTML table, VNDB's API with JSON
const asked = [];
// ErogameScape's list as the stand-in has it: id, name, votes
const LIST = [
  [35435, "アネモイ", 512],
  [1201, "星の欠片の物語", 900],
  [1202, "星の欠片の物語 アフターストーリー", 20],
  [4400, "LOOPERS", 800],
  [5000, "まったく別のゲーム", 3],
];
let egsDown = false;
const table = (cols, rows) =>
  `<html><body><table><tr>${cols.map((c) => `<th>${c}</th>`).join("")}</tr>${rows
    .map((r) => `<tr>${r.map((v) => `<td>${v}</td>`).join("")}</tr>`)
    .join("")}</table></body></html>`;
const sources = createServer(async (req, res) => {
  let body = "";
  for await (const c of req) body += c;
  if (req.url === "/cover.png") {
    res.writeHead(200, { "content-type": "image/png" });
    return res.end(cover);
  }
  if (req.url === "/egs") {
    const sql = new URLSearchParams(body).get("sql") ?? "";
    asked.push(`egs ${sql.match(/WHERE (?:g|pt)\.id = (\d+)|pt\.game = (\d+)/)?.slice(1).find(Boolean) ?? "?"}`);
    if (egsDown) {
      res.writeHead(503);
      return res.end("down");
    }
    res.writeHead(200, { "content-type": "text/html; charset=utf-8" });
    // the game list (Library::egs_index): ids up to 5000, in two windows
    if (sql.includes("max(id)")) return res.end(table(["max"], [["5000"]]));
    if (sql.includes("FROM gamelist WHERE id >=")) {
      const from = Number(sql.match(/id >= (\d+)/)[1]);
      return res.end(table(["id", "gamename", "count2"], LIST.filter(([id]) => id >= from && id < from + 5000)));
    }
    if (sql.includes("FROM gamelist g")) {
      const id = sql.match(/WHERE g\.id = (\d+)/)[1];
      const name = LIST.find(([i]) => String(i) === id)?.[1] ?? "アネモイ";
      return res.end(
        table(
          ["id", "gamename", "furigana", "sellday", "median", "count2", "dmm", "dmm_subsc", "genre", "shoukai", "dlsite_id", "dlsite_domain", "brandname", "url"],
          [[id, name, "", "2025-06-27", "81", "512", "", "", "ノベル", "風の島の物語。", "", "", "Key", "https://key.example/"]],
        ),
      );
    }
    return res.end(
      table(["src", "val", "grp", "cnt"], [["tag", "泣きゲー", "傾向", "40"], ["tag", "ファンタジー系のゲーム", "背景", "12"]]),
    );
  }
  if (req.url === "/vndb") {
    const q = JSON.parse(body);
    asked.push(`vndb ${q.filters[2]}`);
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(
      JSON.stringify({
        results: [
          {
            id: q.filters[2],
            title: "Ever17 -the out of infinity-",
            alttitle: "Ever17 -the out of infinity-",
            released: "2002-08-29",
            votecount: 9000,
            rating: 85.4,
            description: "[b]LeMU[/b] の 119 時間。",
            developers: [{ name: "KID", original: "キッド" }],
            image: { url: `http://127.0.0.1:${sources.address().port}/cover.png`, sexual: 0, violence: 0 },
            tags: [{ name: "Mystery", rating: 2.8, spoiler: 0, category: "cont" }],
            extlinks: [],
          },
        ],
      }),
    );
  }
  res.writeHead(404);
  res.end();
});
await new Promise((ok) => sources.listen(0, "127.0.0.1", ok));
const src = `http://127.0.0.1:${sources.address().port}`;

const port = 9300 + Math.floor(Math.random() * 200) * 2;
const env = { ...process.env, IPPAI_META_EGS_SQL: `${src}/egs`, IPPAI_META_VNDB: `${src}/vndb`, IPPAI_META_SEED: `${src}/seed` };
// (the stand-ins are on this machine: no proxy in between)
for (const k of ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"]) delete env[k];
const server = spawn(
  bin,
  ["serve", "--foreground", "--web", resolve("web/dist"), "--store", store, "--jit", join(work, "jit"), "--listen", `127.0.0.1:${port}`, "--no-prerun"],
  { stdio: "inherit", env },
);
process.on("exit", () => server.kill());
const base = `http://127.0.0.1:${port}/`;
for (let i = 0; i < 100; i++) {
  if (await fetch(base).then((r) => r.ok, () => false)) break;
  await new Promise((r) => setTimeout(r, 300));
}

// `ippai meta`: "LOOPERS DL版" is LOOPERS (4400) from the list, asked for by its id
// (not spawnSync: the stand-ins answer from this process)
const said = await new Promise((ok) => {
  const p = spawn(bin, ["--store", store, "meta", "loopers"], { env });
  let out = "";
  p.stdout.on("data", (d) => (out += d));
  p.stderr.on("data", (d) => (out += d));
  p.on("close", () => ok(out));
});
assert.ok(asked.includes("egs 4400"), `${asked.join(", ")}\n${said}`);
assert.ok(!asked.some((a) => a.startsWith("vndb")), "no search");
assert.equal(JSON.parse(readFileSync(join(store, "games/loopers.json"), "utf8")).meta.egs_id, 4400, said);
asked.length = 0;

// the server refuses what names no game page, and a game it does not have
const put = (game, url, headers = {}) =>
  fetch(`${base}api/library/${game}/meta`, {
    method: "PUT",
    headers: { "content-type": "application/json", ...headers },
    body: JSON.stringify({ url }),
  });
assert.equal((await put("anemoi", "https://example.com/game.php?game=1")).status, 400);
assert.equal((await put("nothere", "https://vndb.org/v17")).status, 404);
// (a write from another site's page is refused, as every write: serve.rs same_origin_write)
assert.equal((await put("anemoi", "https://vndb.org/v17", { "sec-fetch-site": "cross-site" })).status, 403);
assert.deepEqual(asked, [], "nothing fetched for those");

const browser = await chromium.launch();
try {
  const page = await browser.newPage({ viewport: { width: 1100, height: 900 } });
  const errors = [];
  page.on("pageerror", (e) => errors.push(String(e)));
  await page.goto(base);
  const card = (id) => page.locator(`[data-game="${id}"]`);
  await card("anemoi").waitFor();
  await page.getByRole("button", { name: "anemoi の情報" }).click();
  let dialog = page.getByRole("dialog");
  const box = dialog.getByTestId("meta-url");
  const go = dialog.getByTestId("meta-fetch");

  // not a game page: said on the page, the button stays off
  await box.fill("https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/brand.php?brand=1");
  assert.equal(await go.isDisabled(), true);
  assert.match(await dialog.innerText(), /作品ページの URL を入れてください/);

  // 批評空間: the dialog and the card fill in without a reload
  await box.fill("https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?game=35435");
  await go.click();
  await dialog.getByRole("heading", { name: "アネモイ" }).waitFor({ timeout: 30_000 });
  const text = await dialog.innerText();
  for (const want of ["Key", "2025-06-27", "81（512 票）", "ノベル", "泣き", "ファンタジー", "風の島の物語。", "に更新しました"]) assert.ok(text.includes(want), `${want} in:\n${text}`);
  assert.deepEqual(asked.slice(0, 2), ["egs 35435", "egs 35435"]);
  if (shots) await page.screenshot({ path: join(shots, "meta-egs.png") });
  await page.keyboard.press("Escape");
  // (the card keeps the store's title; brand, year and score come from what was fetched)
  assert.match(await card("anemoi").innerText(), /Key · 2025[\s\S]*81/);

  // after a reload: from the server's database
  await page.reload();
  await card("anemoi").waitFor();
  assert.match(await card("anemoi").innerText(), /Key · 2025/);

  // the source failing: the reason, and what was there stays
  egsDown = true;
  await page.getByRole("button", { name: "anemoi の情報" }).click();
  dialog = page.getByRole("dialog");
  assert.equal(await dialog.getByTestId("meta-url").inputValue(), "https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?game=35435", "the last address is offered again");
  await dialog.getByTestId("meta-fetch").click();
  await dialog.getByTestId("meta-status").filter({ hasText: "取得できませんでした" }).waitFor({ timeout: 30_000 });
  assert.match(await dialog.getByTestId("meta-status").innerText(), /503/);
  await dialog.getByRole("heading", { name: "アネモイ" }).waitFor();
  await page.keyboard.press("Escape");
  egsDown = false;

  // VNDB, with a cover: the card shows the server's copy
  await page.getByRole("button", { name: "ever17 の情報" }).click();
  dialog = page.getByRole("dialog");
  await dialog.getByTestId("meta-url").fill("https://vndb.org/v17/chars#main");
  await dialog.getByTestId("meta-fetch").click();
  await dialog.getByRole("heading", { name: "Ever17 -the out of infinity-" }).waitFor({ timeout: 30_000 });
  assert.ok(asked.includes("vndb v17"), asked.join(", "));
  const img = dialog.locator("img").first();
  await img.waitFor();
  assert.match(await img.getAttribute("src"), /\/api\/library\/ever17\/cover\?v=\d+$/);
  assert.equal(await img.evaluate((i) => i.complete && i.naturalWidth), 64);
  assert.match(await dialog.innerText(), /LeMU の 119 時間。/, "BBCode taken out");
  if (shots) await page.screenshot({ path: join(shots, "meta-vndb.png") });
  await page.keyboard.press("Escape");
  const cardImg = card("ever17").locator("img").first();
  assert.equal(await cardImg.evaluate((i) => i.complete && i.naturalWidth), 64);
  if (shots) await page.screenshot({ path: join(shots, "meta-list.png") });

  // 候補を探す by a name: the list's games like it, the most voted marked; これにする fetches it
  await page.getByRole("button", { name: "hoshi の情報" }).click();
  dialog = page.getByRole("dialog");
  assert.equal(await dialog.getByTestId("meta-url").inputValue(), "", "nothing from the game opened before");
  await dialog.getByTestId("meta-search").fill("星の欠片");
  await dialog.getByTestId("meta-candidates").click();
  const list = dialog.getByTestId("meta-candidate-list");
  await list.waitFor({ timeout: 30_000 });
  const items = await list.getByRole("listitem").allInnerTexts();
  assert.match(items[0], /星の欠片の物語/);
  assert.match(items.join("\n"), /星の欠片の物語 アフターストーリー/);
  assert.match(items[1], /星の欠片の物語 アフターストーリー/, "the more voted first");
  assert.ok(!items.join("").includes("自動で選ばれる作品"), "typed: none taken without asking");
  assert.ok(!items.join("").includes("まったく別のゲーム"), "nothing unlike it");
  if (shots) await page.screenshot({ path: join(shots, "meta-candidates.png") });
  await list.getByRole("listitem").first().getByRole("button", { name: "これにする" }).click();
  await dialog.getByRole("heading", { name: "星の欠片の物語" }).waitFor({ timeout: 30_000 });
  assert.ok(asked.includes("egs 1201"), asked.join(", "));
  await page.keyboard.press("Escape");

  // 候補を探す with nothing typed: by the game's names ("LOOPERS DL版"), the one `ippai meta` took marked
  await page.getByRole("button", { name: "LOOPERS DL版 の情報" }).click();
  dialog = page.getByRole("dialog");
  await dialog.getByTestId("meta-candidates").click();
  await dialog.getByTestId("meta-candidate-list").waitFor({ timeout: 30_000 });
  const auto = await dialog.getByTestId("meta-candidate-list").getByRole("listitem").allInnerTexts();
  assert.match(auto[0], /^LOOPERS100%[\s\S]*自動で選ばれる作品/, auto.join(" / "));
  await page.keyboard.press("Escape");
  assert.deepEqual(errors, []);
  console.log("meta-browser: ok");
} finally {
  await browser.close();
  server.kill();
  sources.close();
}
