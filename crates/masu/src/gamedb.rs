//! The game database: every ErogameScape (批評空間) game and every Japanese VN on VNDB, each part
//! its own SQLite file (Part), rebuilt by hand now and then (`masu gamedb update --part`:
//! from public SQLite mirrors; tools/gamedb/README.md) and
//! published as a public package (ghcr.io/a-kaibu/ippai-gamedb, a tag for each part, zstd), so
//! a server that cannot reach ErogameScape (it resets many hosts' connections) still identifies
//! games and shows their information. The server keeps them as `<store>/gamedb-<part>.sqlite`
//! (Library::egs_index pulls them when a day old) and meta.rs reads entries from them before
//! asking the sites.
//!
//! Tables: `egs(id, name, votes, model, row, more)`: the game's gamelist row (with its brand's
//! name and url) and its tag and series rows, as JSON, exactly what meta::egs_info reads from the
//! form; `vndb(id, title, alttitle, votes, json)`: the VN as the API gives it (meta::VNDB_FIELDS);
//! `info(key, value)`: built_ms, egs, vndb.

use crate::meta;
use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

mod mirror;

#[derive(clap::Args, Default)]
pub struct Source {
    /// Import an already downloaded vn-archive SQLite mirror instead of downloading it.
    #[arg(long, value_name = "SQLITE")]
    mirror: Option<std::path::PathBuf>,
    /// Use the original websites/API instead of the public SQLite mirrors.
    #[arg(long, conflicts_with = "mirror")]
    live: bool,
}

/// Where the package is, host/repo (MASU_GAMEDB_REF: another registry, for tests). Each part has
/// its tag there (Part::tag).
pub fn reference() -> String {
    std::env::var("MASU_GAMEDB_REF").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "ghcr.io/a-kaibu/ippai-gamedb".into())
}

/// The two parts keep separate files and tags; both can be rebuilt on a GitHub runner
/// from vn-archive's public SQLite mirrors. Updates are manually triggered.
#[derive(Clone, Copy, PartialEq, Debug, clap::ValueEnum)]
pub enum Part {
    Egs,
    Vndb,
}

impl Part {
    pub const ALL: [Part; 2] = [Part::Egs, Part::Vndb];

    pub fn tag(self) -> &'static str {
        match self {
            Part::Egs => "egs",
            Part::Vndb => "vndb",
        }
    }

    /// The copy's name in a store (`<store>/gamedb-egs.sqlite`).
    pub fn file(self) -> &'static str {
        match self {
            Part::Egs => "gamedb-egs.sqlite",
            Part::Vndb => "gamedb-vndb.sqlite",
        }
    }
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS egs(id INTEGER PRIMARY KEY, name TEXT NOT NULL, votes INTEGER NOT NULL, model TEXT NOT NULL, row TEXT NOT NULL, more TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS vndb(id INTEGER PRIMARY KEY, title TEXT NOT NULL, alttitle TEXT NOT NULL, votes INTEGER NOT NULL, json TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS info(key TEXT PRIMARY KEY, value TEXT NOT NULL);";

// ---- building

/// The POV groups meta::egs_info shows as tags (only those rows are kept).
const POV_GROUPS: &str = "'ジャンル','背景','傾向'";

fn col<'a>(r: &'a [(String, String)], k: &str) -> &'a str {
    r.iter().find(|(c, _)| c == k).map_or("", |(_, v)| v)
}

/// ErogameScape's games: (row, tag and series rows) by id, from the form in windows of ids.
pub fn egs_all(query: &dyn Fn(&str) -> Result<Vec<meta::Row>>) -> Result<Vec<(u32, meta::Row, Vec<meta::Row>)>> {
    let max: u32 = query("SELECT max(id) AS max FROM gamelist")?.first().map(|r| col(r, "max").parse()).transpose()?.unwrap_or(0);
    let mut games: Vec<(u32, meta::Row, Vec<meta::Row>)> = vec![];
    let mut at: HashMap<u32, usize> = HashMap::new();
    let mut from = 0;
    while from <= max {
        for r in query(&format!(
            "SELECT g.id, g.gamename, g.furigana, g.sellday, g.median, g.count2, g.dmm, g.dmm_subsc, g.genre, g.shoukai, g.dlsite_id, g.dlsite_domain, b.brandname, b.url, g.model
FROM gamelist g LEFT JOIN brandlist b ON g.brandname = b.id WHERE g.id >= {from} AND g.id < {} ORDER BY g.id",
            from + 5000
        ))? {
            if let Ok(id) = col(&r, "id").parse::<u32>() {
                at.insert(id, games.len());
                games.push((id, r, vec![]));
            }
        }
        from += 5000;
    }
    if games.is_empty() {
        bail!("erogamescape: no games");
    }
    // (tags: many rows a game; smaller windows)
    let mut from = 0;
    while from <= max {
        for r in query(&format!(
            "SELECT pt.game, 'tag' AS src, p.title AS val, p.system_group AS grp, pt.count AS cnt
FROM povgroups_toukei pt JOIN povlist p ON pt.pov = p.id
WHERE pt.game >= {from} AND pt.game < {} AND pt.count >= 2 AND p.system_group IN ({POV_GROUPS}) ORDER BY pt.game, pt.count DESC",
            from + 1000
        ))? {
            if let Some(&i) = col(&r, "game").parse::<u32>().ok().and_then(|id| at.get(&id)) {
                games[i].2.push(r.into_iter().filter(|(k, _)| k != "game").collect());
            }
        }
        from += 1000;
    }
    let mut from = 0;
    while from <= max {
        for r in query(&format!(
            "SELECT b.game, 'series' AS src, g.name AS val FROM belong_to_gamegroup_list b JOIN gamegrouplist g ON b.gamegroup = g.id
WHERE b.game >= {from} AND b.game < {} ORDER BY b.game, g.id",
            from + 5000
        ))? {
            if let Some(&i) = col(&r, "game").parse::<u32>().ok().and_then(|id| at.get(&id)) {
                if !games[i].2.iter().any(|m| col(m, "src") == "series") {
                    games[i].2.push(r.into_iter().filter(|(k, _)| k != "game").collect());
                }
            }
        }
        from += 5000;
    }
    Ok(games)
}

/// VNDB's VNs whose original language is Japanese, as the API gives them, page by page (within
/// its limit of 200 requests in 5 minutes).
pub fn vndb_all(page: &dyn Fn(u32) -> Result<(Vec<serde_json::Value>, bool)>, pause: Duration) -> Result<Vec<serde_json::Value>> {
    let mut all = vec![];
    let mut n = 1;
    let mut waits = 0;
    loop {
        match page(n) {
            Ok((mut r, more)) => {
                all.append(&mut r);
                if !more {
                    break;
                }
                n += 1;
                waits = 0;
                std::thread::sleep(pause);
            }
            // (over the rate limit all the same: wait, longer each time, and ask again)
            Err(e) if format!("{e:#}").contains("429") && waits < 8 => {
                waits += 1;
                eprintln!("vndb: page {n}: {e:#}; waiting");
                std::thread::sleep(pause * 20 * waits);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(all)
}

/// A VN with only the tags meta::vndb_info can show (no spoiler, not sexual, rated 2 or more: the
/// ten best rated), which are most of the API's answer otherwise.
pub fn prune_vndb(mut v: serde_json::Value) -> serde_json::Value {
    if let Some(tags) = v.get_mut("tags").and_then(|t| t.as_array_mut()) {
        tags.retain(|t| t["spoiler"].as_f64() == Some(0.0) && t["category"].as_str() != Some("ero") && t["rating"].as_f64().is_some_and(|r| r >= 2.0));
        tags.sort_by(|a, b| b["rating"].as_f64().unwrap_or(0.0).total_cmp(&a["rating"].as_f64().unwrap_or(0.0)));
        tags.truncate(10);
    }
    v
}

/// Writes the database to `out` (a new file).
pub fn write(out: &Path, egs: &[(u32, meta::Row, Vec<meta::Row>)], vndb: &[serde_json::Value]) -> Result<()> {
    if out.exists() {
        std::fs::remove_file(out)?;
    }
    let mut c = Connection::open(out)?;
    c.execute_batch(SCHEMA)?;
    let t = c.transaction()?;
    {
        let mut q = t.prepare("INSERT INTO egs(id,name,votes,model,row,more) VALUES(?,?,?,?,?,?)")?;
        for (id, r, more) in egs {
            q.execute(params![id, col(r, "gamename"), col(r, "count2").parse::<u32>().unwrap_or(0), col(r, "model"), serde_json::to_string(r)?, serde_json::to_string(more)?])?;
        }
        let mut q = t.prepare("INSERT OR REPLACE INTO vndb(id,title,alttitle,votes,json) VALUES(?,?,?,?,?)")?;
        for v in vndb {
            let Some(n) = v["id"].as_str().and_then(|s| s.strip_prefix('v')).and_then(|n| n.parse::<u32>().ok()) else { continue };
            q.execute(params![n, v["title"].as_str().unwrap_or(""), v["alttitle"].as_str().unwrap_or(""), v["votecount"].as_i64().unwrap_or(0), v.to_string()])?;
        }
        let mut q = t.prepare("INSERT INTO info(key,value) VALUES(?,?)")?;
        q.execute(params!["built_ms", now_ms().to_string()])?;
        q.execute(params!["egs", egs.len().to_string()])?;
        q.execute(params!["vndb", vndb.len().to_string()])?;
    }
    t.commit()?;
    c.execute_batch("VACUUM")?;
    Ok(())
}

/// Build a compact part from its SQLite mirror, or the original service with --live.
pub fn build(out: &Path, part: Part, source: &Source) -> Result<()> {
    if source.live {
        build_with(out, part, &|| egs_all(&meta::egs_rows), &vndb_fetch)
    } else {
        mirror::build(out, part, source.mirror.as_deref())
    }
}

fn vndb_fetch() -> Result<Vec<serde_json::Value>> {
    vndb_all(
        &|n| meta::vndb_page(serde_json::json!(["olang", "=", "ja"]), n).map(|(v, more)| (v.into_iter().map(prune_vndb).collect(), more)),
        Duration::from_millis(2000),
    )
}

type EgsGames = Vec<(u32, meta::Row, Vec<meta::Row>)>;

/// Only `part`'s site asked; the other part's table stays empty.
fn build_with(out: &Path, part: Part, egs: &dyn Fn() -> Result<EgsGames>, vndb: &dyn Fn() -> Result<Vec<serde_json::Value>>) -> Result<()> {
    match part {
        Part::Egs => {
            let games = egs().context("erogamescape")?;
            eprintln!("erogamescape: {} games", games.len());
            write(out, &games, &[])
        }
        Part::Vndb => {
            let vns = vndb().context("vndb")?;
            eprintln!("vndb: {} VNs", vns.len());
            write(out, &[], &vns)
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

// ---- reading

/// The entry `id` names (egs:<n>, vndb:v<n>) from the copies in `dir` (a store: its part's
/// file), when that is there and has it.
pub fn info(dir: &Path, id: &str) -> Option<meta::Info> {
    let part = if id.starts_with("egs:") { Part::Egs } else { Part::Vndb };
    let path = dir.join(part.file());
    if !path.is_file() {
        return None;
    }
    info_in(&Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?, id)
}

fn info_in(c: &Connection, id: &str) -> Option<meta::Info> {
    let r = if let Some(n) = id.strip_prefix("egs:").and_then(|n| n.parse::<u32>().ok()) {
        let (row, more): (String, String) = c.query_row("SELECT row,more FROM egs WHERE id=?", [n], |r| Ok((r.get(0)?, r.get(1)?))).optional().ok()??;
        meta::egs_info(&serde_json::from_str::<meta::Row>(&row).ok()?, &serde_json::from_str::<Vec<meta::Row>>(&more).ok()?)
    } else if let Some(n) = id.strip_prefix("vndb:v").and_then(|n| n.parse::<u32>().ok()) {
        let json: String = c.query_row("SELECT json FROM vndb WHERE id=?", [n], |r| r.get(0)).optional().ok()??;
        meta::vndb_info_json(serde_json::from_str(&json).ok()?)
    } else {
        return None;
    };
    r.ok()
}

/// A copy's ErogameScape part, as `write` takes it.
#[cfg(test)]
fn egs_rows_of(path: &Path) -> Result<Vec<(u32, meta::Row, Vec<meta::Row>)>> {
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut q = c.prepare("SELECT id,row,more FROM egs ORDER BY id")?;
    let rows = q.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(|(id, r, m)| Ok((id, serde_json::from_str(&r)?, serde_json::from_str(&m)?))).collect()
}

/// ErogameScape's PC games (id, name, votes) in a copy, and when it was built (ms).
pub fn egs_games(path: &Path) -> Result<(Vec<(u32, String, u32)>, i64)> {
    let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut q = c.prepare("SELECT id,name,votes FROM egs WHERE model='PC' ORDER BY id")?;
    let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let built: String = c.query_row("SELECT value FROM info WHERE key='built_ms'", [], |r| r.get(0))?;
    Ok((rows, built.parse()?))
}

// ---- the package

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(300)).try_proxy_from_env(true).redirects(0).build()
}

/// A package registry's repository (ghcr.io and the OCI distribution API): requests with the
/// token its 401 asks for, anonymous (a public package's pull) or with the secret as Basic (a
/// push: GHCR_TOKEN, write:packages), and a redirect (a blob's signed URL) followed without it.
struct Registry {
    a: ureq::Agent,
    base: String,
    repo: String,
    secret: Option<String>,
    token: Option<String>,
}

impl Registry {
    /// For the package `name` (host/repo).
    fn new(name: &str, secret: Option<String>) -> Result<Registry> {
        let (host, repo) = name.split_once('/').ok_or_else(|| anyhow!("{name}: no repository"))?;
        let local = host.split(':').next().is_some_and(|h| h == "127.0.0.1" || h == "localhost");
        let base = format!("{}{host}", if local { "http://" } else { "https://" });
        Ok(Registry { a: agent(), base, repo: repo.into(), secret, token: None })
    }

    /// `method` on `path` (under /v2/<repo>/, or a whole URL such as an upload's location).
    fn call(&mut self, method: &str, path: &str, body: &[u8], headers: &[(&str, &str)]) -> Result<ureq::Response> {
        let url = if path.starts_with("http") { path.to_string() } else { format!("{}/v2/{}/{path}", self.base, self.repo) };
        for _ in 0..2 {
            let mut req = self.a.request(method, &url);
            for (k, v) in headers {
                req = req.set(k, v);
            }
            if let Some(t) = &self.token {
                req = req.set("Authorization", &format!("Bearer {t}"));
            }
            match req.send_bytes(body) {
                Ok(r) if method == "GET" && (300..400).contains(&r.status()) => {
                    let loc = r.header("location").ok_or_else(|| anyhow!("{url}: redirect without a location"))?.to_string();
                    return Ok(self.a.get(&loc).call()?);
                }
                Ok(r) => return Ok(r),
                Err(ureq::Error::Status(401, r)) if self.token.is_none() => self.token = Some(self.auth(r.header("www-authenticate").unwrap_or(""), &url)?),
                Err(ureq::Error::Status(code, r)) => {
                    let text = r.into_string().unwrap_or_default();
                    bail!("{method} {url}: {code} {}", text.chars().take(300).collect::<String>())
                }
                Err(e) => bail!("{method} {url}: {e}"),
            }
        }
        bail!("{method} {url}: refused")
    }

    fn auth(&self, challenge: &str, url: &str) -> Result<String> {
        let f: HashMap<String, String> = regex::Regex::new(r#"(\w+)="([^"]*)""#)
            .unwrap()
            .captures_iter(challenge)
            .map(|c| (c[1].to_string(), c[2].to_string()))
            .collect();
        let realm = f.get("realm").ok_or_else(|| anyhow!("{url}: 401 without a realm"))?;
        let want = if self.secret.is_some() { "pull,push" } else { "pull" };
        let scope = format!("repository:{}:{want}", self.repo);
        let mut req = self.a.get(realm).query("service", f.get("service").map_or("", |s| s.as_str())).query("scope", &scope);
        if let Some(s) = &self.secret {
            use base64::Engine;
            req = req.set("Authorization", &format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("x:{s}"))));
        }
        let j: serde_json::Value = match req.call() {
            Ok(r) => r.into_json()?,
            Err(ureq::Error::Status(code, _)) => bail!("{realm}: {scope}: {code}{}", if self.secret.is_some() { "" } else { " (no GHCR_TOKEN)" }),
            Err(e) => bail!("{realm}: {e}"),
        };
        Ok(j["token"].as_str().or(j["access_token"].as_str()).ok_or_else(|| anyhow!("{realm}: no token"))?.to_string())
    }

    /// `data` as a blob (unless the registry has it already); its digest.
    fn put_blob(&mut self, data: &[u8]) -> Result<String> {
        let digest = format!("sha256:{}", hex(ring::digest::digest(&ring::digest::SHA256, data)));
        match self.call("HEAD", &format!("blobs/{digest}"), &[], &[]) {
            Ok(_) => return Ok(digest),
            Err(e) if format!("{e:#}").contains(": 404") => {}
            Err(e) => return Err(e),
        }
        let r = self.call("POST", "blobs/uploads/", &[], &[])?;
        let loc = r.header("location").ok_or_else(|| anyhow!("{}: an upload without a location", self.repo))?;
        let loc = if loc.starts_with("http") { loc.to_string() } else { format!("{}{loc}", self.base) };
        let sep = if loc.contains('?') { '&' } else { '?' };
        self.call("PUT", &format!("{loc}{sep}digest={}", digest.replace(':', "%3A")), data, &[("Content-Type", "application/octet-stream")])?;
        Ok(digest)
    }
}

/// Brings `part`'s copy in the package to `dest` when it differs from the copy there (`have`:
/// the digest of that copy). Returns the digest now there.
pub fn pull(dest: &Path, part: Part, have: Option<&str>) -> Result<String> {
    pull_from(&reference(), part, dest, have)
}

fn pull_from(name: &str, part: Part, dest: &Path, have: Option<&str>) -> Result<String> {
    let mut reg = Registry::new(name, None)?;
    let r = format!("{name}:{}", part.tag());
    let m: serde_json::Value = reg.call("GET", &format!("manifests/{}", part.tag()), &[], &[("Accept", MANIFEST)])?.into_json()?;
    let layer = &m["layers"][0];
    let digest = layer["digest"].as_str().ok_or_else(|| anyhow!("{r}: no layer"))?.to_string();
    if have == Some(digest.as_str()) && dest.is_file() {
        return Ok(digest);
    }
    let blob = reg.call("GET", &format!("blobs/{digest}"), &[], &[("Accept", "*/*")])?.into_reader().take(1 << 30);
    let tmp = dest.with_extension("sqlite.tmp");
    unpack(blob, &digest, &tmp, MAX_DB).with_context(|| format!("{r}: {digest}"))?;
    // (a copy it would not publish is not taken)
    if let Err(e) = check(&tmp, part) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context(format!("{r}: not {part:?}'s part")));
    }
    std::fs::rename(&tmp, dest)?;
    Ok(digest)
}

const MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";

/// The largest database a pull writes (the parts are a few hundred MB)
const MAX_DB: u64 = 8 << 30;

/// Reads through to a SHA-256 of what it read.
struct Hashing<R> {
    inner: R,
    sum: ring::digest::Context,
}
impl<R: Read> Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.sum.update(&buf[..n]);
        Ok(n)
    }
}

/// A zstd layer `blob` decompressed into `tmp` as it arrives (not held in memory, neither
/// compressed nor whole), at most `limit` bytes of it: Ok when every byte read is the layer
/// `digest` names; otherwise `tmp` is gone.
fn unpack(blob: impl Read, digest: &str, tmp: &Path, limit: u64) -> Result<()> {
    let r = (|| -> Result<()> {
        let mut h = Hashing { inner: blob, sum: ring::digest::Context::new(&ring::digest::SHA256) };
        let mut out = std::io::BufWriter::new(std::fs::File::create(tmp)?);
        let mut z = zstd::stream::read::Decoder::new(&mut h)?;
        if std::io::copy(&mut (&mut z).take(limit + 1), &mut out)? > limit {
            bail!("more than {limit} bytes once decompressed");
        }
        drop(z);
        // (what the decoder left of the blob is in the digest too)
        std::io::copy(&mut h, &mut std::io::sink())?;
        let sum = format!("sha256:{}", hex(h.sum.finish()));
        if sum != digest {
            bail!("the blob is not {digest}");
        }
        std::io::Write::flush(&mut out)?;
        Ok(())
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(tmp);
    }
    r
}

/// A built copy of `part` onto the package (GHCR_TOKEN: a token with write:packages),
/// zstd-compressed, as an OCI artifact tagged with the part and the part and day (UTC: egs-20261109),
/// linked to the repository. Checked first (`check`). Returns the layer's digest.
pub fn push(file: &Path, part: Part, secret: &str) -> Result<String> {
    push_to(&reference(), part, file, secret, &meta::now_rfc3339()[..10].replace('-', ""))
}

fn push_to(name: &str, part: Part, file: &Path, secret: &str, day: &str) -> Result<String> {
    check(file, part)?;
    let z = zstd::encode_all(&std::fs::read(file)?[..], 19)?;
    let mut reg = Registry::new(name, Some(secret.into()))?;
    let layer = reg.put_blob(&z)?;
    let empty = reg.put_blob(b"{}")?;
    let m = serde_json::json!({
        "schemaVersion": 2, "mediaType": MANIFEST, "artifactType": "application/vnd.masu.gamedb.v1",
        "config": {"mediaType": "application/vnd.oci.empty.v1+json", "digest": empty, "size": 2},
        "layers": [{"mediaType": "application/zstd", "digest": layer, "size": z.len(), "annotations": {"org.opencontainers.image.title": part.file().to_string() + ".zst"}}],
        "annotations": {"org.opencontainers.image.source": "https://github.com/a-kaibu/masu", "org.opencontainers.image.description": "ErogameScape and VNDB entries for masu (crates/masu/src/gamedb.rs), a tag for each"},
    })
    .to_string();
    for tag in [part.tag().to_string(), format!("{}-{day}", part.tag())] {
        reg.call("PUT", &format!("manifests/{tag}"), m.as_bytes(), &[("Content-Type", MANIFEST)])?;
    }
    eprintln!("{name}:{} {layer} ({} compressed)", part.tag(), z.len());
    Ok(layer)
}

/// Rebuild and publish one part. Mirror updates retain previous EGS enrichment.
pub fn update(dir: &Path, part: Part, secret: &str, source: &Source) -> Result<String> {
    std::fs::create_dir_all(dir)?;
    let out = dir.join(part.file());
    // The EGS mirror does not include POV/series tables. A fresh updater first gets
    // the published copy so rebuilding cannot silently erase that enrichment.
    if part == Part::Egs && !source.live && !out.exists() {
        pull(&out, part, None).context("fetching previous EGS tags/series (for a first publication, use build then push)")?;
    }
    build(&out, part, source)?;
    push(&out, part, secret)
}

/// The least a copy of `part` holds to be published or taken: ErogameScape's PC games with a
/// known one (30345: LOOPERS), or VNDB's Japanese VNs.
pub fn check(file: &Path, part: Part) -> Result<()> {
    let c = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).with_context(|| format!("{}", file.display()))?;
    let counts = || -> rusqlite::Result<(u32, u32, Option<String>)> {
        Ok((
            c.query_row("SELECT count(*) FROM egs WHERE model='PC'", [], |r| r.get(0))?,
            c.query_row("SELECT count(*) FROM vndb", [], |r| r.get(0))?,
            c.query_row("SELECT name FROM egs WHERE id=30345", [], |r| r.get(0)).optional()?,
        ))
    };
    let (n, v, known) = counts().with_context(|| format!("{}: not a game database", file.display()))?;
    eprintln!("erogamescape: {n} PC games, 30345 {known:?}; vndb: {v} VNs");
    counts_ok(part, n, v, known.as_deref())
}

const MIN_EGS: u32 = 20_000;
const MIN_VNDB: u32 = 10_000;

fn counts_ok(part: Part, egs_pc: u32, vndb: u32, known: Option<&str>) -> Result<()> {
    match part {
        Part::Egs if egs_pc <= MIN_EGS || known != Some("LOOPERS") => bail!("only {egs_pc} games from ErogameScape (30345: {known:?})"),
        Part::Vndb if vndb <= MIN_VNDB => bail!("only {vndb} VNs from VNDB"),
        _ => Ok(()),
    }
}

fn hex(b: impl AsRef<[u8]>) -> String {
    b.as_ref().iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(kv: &[(&str, &str)]) -> meta::Row {
        kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    /// The windows asked for, and what each answers, as the form would.
    fn form(sql: &str) -> Result<Vec<meta::Row>> {
        if sql.contains("max(id)") {
            return Ok(vec![row(&[("max", "5200")])]);
        }
        let from: u32 = regex::Regex::new(r">= (\d+)").unwrap().captures(sql).unwrap()[1].parse().unwrap();
        Ok(if sql.contains("FROM gamelist g") {
            [(1u32, "サクラノ詩 -櫻の森の上を舞う-", "PC", "5000"), (5100, "アネモイ", "PC", "512"), (5101, "アネモイ (PS5)", "PS5", "3")]
                .into_iter()
                .filter(|(id, ..)| *id >= from && *id < from + 5000)
                .map(|(id, n, m, v)| row(&[("id", &id.to_string()), ("gamename", n), ("count2", v), ("median", "80"), ("model", m), ("brandname", "Key"), ("url", ""), ("genre", "ノベル"), ("shoukai", "あらすじ")]))
                .collect()
        } else if sql.contains("povgroups_toukei") {
            if from == 5000 {
                vec![
                    row(&[("game", "5100"), ("src", "tag"), ("val", "泣きゲー"), ("grp", "傾向"), ("cnt", "40")]),
                    row(&[("game", "5100"), ("src", "tag"), ("val", "ファンタジー系のゲーム"), ("grp", "背景"), ("cnt", "12")]),
                    row(&[("game", "9999"), ("src", "tag"), ("val", "どこにもない"), ("grp", "傾向"), ("cnt", "9")]),
                ]
            } else {
                vec![]
            }
        } else if sql.contains("belong_to_gamegroup_list") && from == 0 {
            vec![row(&[("game", "1"), ("src", "series"), ("val", "サクラノ")]), row(&[("game", "1"), ("src", "series"), ("val", "二つ目")])]
        } else {
            vec![]
        })
    }

    /// Built from the form and VNDB's pages, read back: the same information as from the sites
    /// (meta::egs_info / vndb_info on the same rows), the PC games listed for matching.
    #[test]
    fn build_and_read() {
        let egs = egs_all(&form).unwrap();
        assert_eq!(egs.iter().map(|g| g.0).collect::<Vec<_>>(), [1, 5100, 5101]);
        let pages = |n: u32| -> Result<(Vec<serde_json::Value>, bool)> {
            Ok(match n {
                1 => (vec![serde_json::json!({"id": "v17", "title": "Ever17", "alttitle": "Ever17 -the out of infinity-", "votecount": 9000, "rating": 85.4, "developers": [{"name": "KID", "original": "キッド"}], "description": "[b]LeMU[/b]"})], true),
                _ => (vec![serde_json::json!({"id": "v18", "title": "Other", "votecount": 3})], false),
            })
        };
        let vndb = vndb_all(&pages, Duration::ZERO).unwrap();
        assert_eq!(vndb.len(), 2);
        // over the rate limit: asked again for the same page; another error ends it
        let tries = std::cell::Cell::new(0);
        let limited = |n: u32| -> Result<(Vec<serde_json::Value>, bool)> {
            tries.set(tries.get() + 1);
            if n == 2 && tries.get() < 5 { anyhow::bail!("https://api.vndb.org/kana/vn: status code 429") }
            pages(n)
        };
        assert_eq!(vndb_all(&limited, Duration::ZERO).unwrap(), vndb);
        assert_eq!(tries.get(), 5);
        assert!(vndb_all(&|_| anyhow::bail!("status code 500"), Duration::ZERO).is_err());
        let dir = std::env::temp_dir().join(format!("masu-gamedb-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("gamedb.sqlite");
        write(&db, &egs, &vndb).unwrap();
        let (games, built) = egs_games(&db).unwrap();
        assert_eq!(games, [(1, "サクラノ詩 -櫻の森の上を舞う-".into(), 5000), (5100, "アネモイ".into(), 512)], "PC only");
        assert!(now_ms() - built < 60_000);
        let c = Connection::open(&db).unwrap();
        let info = |id: &str| info_in(&c, id);
        let a = info("egs:5100").unwrap();
        let direct = meta::egs_info(&egs[1].1, &egs[1].2).unwrap();
        assert_eq!(serde_json::to_value(&a).unwrap(), serde_json::to_value(&direct).unwrap());
        assert_eq!((a.title.as_str(), a.brand.as_str(), a.score), ("アネモイ", "Key", Some(80)));
        assert_eq!(a.tags, ["ノベル", "泣き", "ファンタジー"]);
        assert_eq!(info("egs:1").unwrap().series, "サクラノ", "the first series only");
        let v = info("vndb:v17").unwrap();
        assert_eq!((v.title.as_str(), v.brand.as_str(), v.description.as_str()), ("Ever17 -the out of infinity-", "キッド", "LeMU"));
        assert!(info("egs:2").is_none() && info("vndb:v1").is_none() && info("x").is_none());
        assert_eq!(egs_rows_of(&db).unwrap(), egs, "an earlier copy's part, as it was written");
        // the tags left out are ones vndb_info would not show
        let full = serde_json::json!({"id": "v9", "title": "T", "tags": (0..30).map(|k| serde_json::json!({"name": format!("t{k}"), "rating": (k % 7) as f64 * 0.5, "spoiler": (k % 5 == 0) as u8 as f64, "category": if k % 3 == 0 { "ero" } else { "cont" }})).collect::<Vec<_>>()});
        let pruned = prune_vndb(full.clone());
        assert_eq!(meta::vndb_info_json(full).unwrap().tags, meta::vndb_info_json(pruned.clone()).unwrap().tags);
        assert!(pruned["tags"].as_array().unwrap().len() <= 10);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A pulled layer is decompressed as it arrives: the bytes zstd gives, or an error and no
    /// file when the digest differs, the layer is no zstd, or it expands past the limit (a
    /// zstd bomb from whatever MASU_GAMEDB_REF points at)
    mod unpack_tests {
        use super::super::{hex, unpack};
        use proptest::prelude::*;
        use std::path::PathBuf;

        fn sum(b: &[u8]) -> String {
            format!("sha256:{}", hex(ring::digest::digest(&ring::digest::SHA256, b)))
        }
        fn tmp() -> PathBuf {
            std::env::temp_dir().join(format!("masu-unpack-{}.tmp", uuid::Uuid::new_v4()))
        }

        #[test]
        fn layers_and_bombs() {
            let db: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8 ^ (i >> 13) as u8).collect();
            let z = zstd::encode_all(&db[..], 3).unwrap();
            let t = tmp();
            unpack(&z[..], &sum(&z), &t, 1 << 30).unwrap();
            assert_eq!(std::fs::read(&t).unwrap(), db);
            std::fs::remove_file(&t).unwrap();
            // the right bytes under another digest; past the limit; not zstd
            assert!(unpack(&z[..], &sum(b"x"), &t, 1 << 30).is_err());
            assert!(!t.exists());
            let bomb = zstd::encode_all(&vec![0u8; 64 << 20][..], 19).unwrap();
            assert!(bomb.len() < 64 << 10);
            let e = format!("{:#}", unpack(&bomb[..], &sum(&bomb), &t, 1 << 20).unwrap_err());
            assert!(e.contains("more than"), "{e}");
            assert!(!t.exists());
            assert!(unpack(&b"not zstd"[..], &sum(b"not zstd"), &t, 1 << 20).is_err());
            assert!(!t.exists());
            // two frames, and bytes after the frame: all in the digest
            let mut two = zstd::encode_all(&b"abc"[..], 1).unwrap();
            two.extend(zstd::encode_all(&b"def"[..], 1).unwrap());
            unpack(&two[..], &sum(&two), &t, 100).unwrap();
            assert_eq!(std::fs::read(&t).unwrap(), b"abcdef");
            let mut cut = two.clone();
            cut.pop();
            assert!(unpack(&cut[..], &sum(&two), &t, 100).is_err());
            let _ = std::fs::remove_file(&t);
        }

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(64))]
            /// as decode_all over the whole blob, for any data and any damage
            #[test]
            fn as_decode_all(data in prop::collection::vec(any::<u8>(), 0..20_000), flip in proptest::option::of((any::<prop::sample::Index>(), 1u8..)), limit in prop_oneof![Just(1u64 << 30), 0u64..30_000]) {
                let mut z = zstd::encode_all(&data[..], 1).unwrap();
                if let Some((i, x)) = flip {
                    let i = i.index(z.len());
                    z[i] ^= x;
                }
                let t = tmp();
                let got = unpack(&z[..], &sum(&z), &t, limit);
                match zstd::decode_all(&z[..]) {
                    Ok(want) if want.len() as u64 <= limit => {
                        prop_assert!(got.is_ok(), "{:?}", got);
                        prop_assert_eq!(std::fs::read(&t).unwrap(), want);
                    }
                    _ => {
                        prop_assert!(got.is_err());
                        prop_assert!(!t.exists());
                    }
                }
                let _ = std::fs::remove_file(&t);
            }
        }
    }

    /// A package registry as ghcr.io answers (the OCI distribution API): tokens from /token (a
    /// push needs the secret as Basic; anonymous gets pull only), uploads at a location with its
    /// own query, blobs behind a redirect to a URL that must come without a token.
    #[derive(Default)]
    struct Reg {
        blobs: std::sync::Mutex<HashMap<String, Vec<u8>>>,
        tags: std::sync::Mutex<HashMap<String, String>>,
        uploads: std::sync::atomic::AtomicUsize,
    }

    fn registry(st: std::sync::Arc<Reg>) -> String {
        use std::io::{BufRead, BufReader, Write};
        use std::sync::atomic::Ordering;
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let me = format!("http://{}", l.local_addr().unwrap());
        let at = me.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut h: HashMap<String, String> = HashMap::new();
                loop {
                    let mut x = String::new();
                    r.read_line(&mut x).unwrap();
                    if x.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = x.split_once(':') {
                        h.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let mut body = vec![0; h.get("content-length").map_or(0, |v| v.parse().unwrap())];
                r.read_exact(&mut body).unwrap();
                let mut w = line.split(' ');
                let (method, target) = (w.next().unwrap_or("").to_string(), w.next().unwrap_or("").to_string());
                let (path, query) = target.split_once('?').unwrap_or((&target, ""));
                let q: HashMap<String, String> = form_urlencoded::parse(query.as_bytes()).into_owned().collect();
                let auth = h.get("authorization").cloned().unwrap_or_default();
                let sum = |b: &[u8]| format!("sha256:{}", hex(ring::digest::digest(&ring::digest::SHA256, b)));
                let ok = |s: &str| (s.to_string(), String::new(), vec![]);
                let (status, extra, out): (String, String, Vec<u8>) = if path == "/token" {
                    let scope = q.get("scope").cloned().unwrap_or_default();
                    assert_eq!(q.get("service").map(|s| s.as_str()), Some("t"));
                    // (base64 of "x:secret")
                    if auth == "Basic eDpzZWNyZXQ=" && scope == "repository:t/gamedb:pull,push" {
                        ("200 OK".into(), String::new(), br#"{"token":"push"}"#.to_vec())
                    } else if auth.is_empty() && scope == "repository:t/gamedb:pull" {
                        ("200 OK".into(), String::new(), br#"{"access_token":"pull"}"#.to_vec())
                    } else {
                        ok("403 Forbidden")
                    }
                } else if let Some(d) = path.strip_prefix("/signed/") {
                    assert!(auth.is_empty(), "the signed URL gets no token");
                    ("200 OK".into(), String::new(), st.blobs.lock().unwrap()[d].clone())
                } else if auth != "Bearer push" && auth != "Bearer pull" {
                    ("401 Unauthorized".into(), format!("www-authenticate: Bearer realm=\"{me}/token\",service=\"t\",scope=\"repository:t/gamedb:pull\"\r\n"), vec![])
                } else if method != "GET" && method != "HEAD" && auth != "Bearer push" {
                    ok("403 Forbidden")
                } else if let Some(d) = path.strip_prefix("/v2/t/gamedb/blobs/").filter(|d| d.starts_with("sha256:")) {
                    match (method.as_str(), st.blobs.lock().unwrap().contains_key(d)) {
                        ("HEAD", true) => ok("200 OK"),
                        ("GET", true) => ("307 Temporary Redirect".into(), format!("location: {me}/signed/{d}\r\n"), vec![]),
                        _ => ok("404 Not Found"),
                    }
                } else if method == "POST" && path == "/v2/t/gamedb/blobs/uploads/" {
                    let n = st.uploads.fetch_add(1, Ordering::SeqCst);
                    ("202 Accepted".into(), format!("location: /v2/t/gamedb/blobs/uploads/{n}?state=s{n}\r\n"), vec![])
                } else if method == "PUT" && path.starts_with("/v2/t/gamedb/blobs/uploads/") {
                    assert!(q.contains_key("state"), "the location's own query kept: {target}");
                    let d = q.get("digest").cloned().unwrap_or_default();
                    if d == sum(&body) {
                        st.blobs.lock().unwrap().insert(d, body);
                        ok("201 Created")
                    } else {
                        ok("400 Bad Request")
                    }
                } else if let Some(tag) = path.strip_prefix("/v2/t/gamedb/manifests/") {
                    if method == "PUT" {
                        assert_eq!(h.get("content-type").map(|s| s.as_str()), Some(MANIFEST));
                        let m: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let blobs = st.blobs.lock().unwrap();
                        assert!(blobs.contains_key(m["config"]["digest"].as_str().unwrap()) && blobs.contains_key(m["layers"][0]["digest"].as_str().unwrap()), "blobs before the manifest");
                        st.tags.lock().unwrap().insert(tag.into(), String::from_utf8(body).unwrap());
                        ok("201 Created")
                    } else {
                        match st.tags.lock().unwrap().get(tag) {
                            Some(m) => ("200 OK".into(), String::new(), m.clone().into_bytes()),
                            None => ok("404 Not Found"),
                        }
                    }
                } else {
                    ok("404 Not Found")
                };
                let mut s = s;
                let _ = write!(s, "HTTP/1.1 {status}\r\n{extra}content-length: {}\r\nconnection: close\r\n\r\n", if method == "HEAD" { 0 } else { out.len() });
                if method != "HEAD" {
                    let _ = s.write_all(&out);
                }
            }
        });
        at.trim_start_matches("http://").to_string()
    }

    fn games(extra: u32) -> Result<EgsGames> {
        Ok((1..=30345 + extra)
            .map(|id| {
                let name = if id == 30345 { "LOOPERS".to_string() } else { format!("g{id}") };
                (id, row(&[("id", &id.to_string()), ("gamename", &name), ("count2", "1"), ("model", "PC")]), vec![])
            })
            .collect())
    }

    fn vns(extra: u32) -> Result<Vec<serde_json::Value>> {
        Ok((1..=MIN_VNDB + extra).map(|n| serde_json::json!({"id": format!("v{n}"), "title": format!("t{n}")})).collect())
    }

    fn no_proxy() {
        for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            std::env::remove_var(k);
        }
    }

    /// A built copy pushed (with the secret) comes back from an anonymous pull byte for byte,
    /// tagged with its part and the part and day; a second push uploads nothing it has; a wrong
    /// secret, a file that is no game database or a part too small pushes nothing.
    #[test]
    fn push_then_pull() {
        let st = std::sync::Arc::new(Reg::default());
        let name = format!("{}/t/gamedb", registry(st.clone()));
        no_proxy();
        let dir = std::env::temp_dir().join(format!("masu-gamedb-push-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("egs.sqlite");
        build_with(&db, Part::Egs, &|| games(0), &|| panic!("VNDB asked")).unwrap();
        let bad = dir.join("bad.sqlite");
        std::fs::write(&bad, b"not sqlite").unwrap();
        assert!(push_to(&name, Part::Egs, &bad, "secret", "20261109").is_err());
        assert!(push_to(&name, Part::Vndb, &db, "secret", "20261109").is_err(), "ErogameScape's copy is no VNDB part");
        let e = format!("{:#}", push_to(&name, Part::Egs, &db, "wrong", "20261109").unwrap_err());
        assert!(e.contains("403"), "{e}");
        assert!(st.tags.lock().unwrap().is_empty() && st.blobs.lock().unwrap().is_empty());
        let layer = push_to(&name, Part::Egs, &db, "secret", "20261109").unwrap();
        let tags = st.tags.lock().unwrap().clone();
        assert_eq!(tags.keys().map(|s| s.as_str()).collect::<std::collections::BTreeSet<_>>(), ["egs", "egs-20261109"].into());
        let m: serde_json::Value = serde_json::from_str(&tags["egs"]).unwrap();
        assert_eq!(m["layers"][0]["digest"], layer);
        assert_eq!(m["annotations"]["org.opencontainers.image.source"], "https://github.com/a-kaibu/masu");
        let back = dir.join("back.sqlite");
        assert_eq!(pull_from(&name, Part::Egs, &back, None).unwrap(), layer);
        assert_eq!(std::fs::read(&back).unwrap(), std::fs::read(&db).unwrap());
        assert_eq!(pull_from(&name, Part::Egs, &back, Some(&layer)).unwrap(), layer, "the copy there is kept");
        let uploads = st.uploads.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(uploads, 2, "the layer and the empty config");
        push_to(&name, Part::Egs, &db, "secret", "20261110").unwrap();
        assert_eq!(st.uploads.load(std::sync::atomic::Ordering::SeqCst), uploads, "nothing uploaded again");
        assert!(pull_from(&name, Part::Vndb, &back, None).is_err(), "no VNDB part yet");
        // (a published copy too small is not taken either)
        let small = dir.join("small.sqlite");
        write(&small, &[], &vns(0).unwrap()[..5]).unwrap();
        let d = Registry::new(&name, Some("secret".into())).unwrap().put_blob(&zstd::encode_all(&std::fs::read(&small).unwrap()[..], 3).unwrap()).unwrap();
        st.tags.lock().unwrap().insert("vndb".into(), tags["egs"].replace(&layer, &d));
        let e = format!("{:#}", pull_from(&name, Part::Vndb, &back, None).unwrap_err());
        assert!(e.contains("only 5 VNs"), "{e}");
        assert_eq!(std::fs::read(&back).unwrap(), std::fs::read(&db).unwrap(), "the copy there kept");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Each part from its own site into its own tag: ErogameScape's never asks VNDB and the
    /// runner's VNDB part never asks ErogameScape; one part failing or too small publishes
    /// nothing and leaves the other as it was; the server's copies (info) find each entry in its
    /// part's file.
    #[test]
    fn the_parts_apart() {
        let st = std::sync::Arc::new(Reg::default());
        let name = format!("{}/t/gamedb", registry(st.clone()));
        no_proxy();
        let dir = std::env::temp_dir().join(format!("masu-gamedb-parts-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let run = |part, day: &str, e: &dyn Fn() -> Result<EgsGames>, v: &dyn Fn() -> Result<Vec<serde_json::Value>>| -> Result<String> {
            let out = dir.join(format!("{}-{day}", part_file(part)));
            build_with(&out, part, e, v)?;
            push_to(&name, part, &out, "secret", day)
        };
        fn part_file(p: Part) -> &'static str {
            p.file()
        }
        let egs_down = || -> Result<EgsGames> { anyhow::bail!("connection reset") };
        let not_egs = || -> Result<EgsGames> { panic!("ErogameScape asked") };
        let not_vndb = || -> Result<Vec<serde_json::Value>> { panic!("VNDB asked") };
        assert!(run(Part::Egs, "20261108", &egs_down, &not_vndb).is_err());
        assert!(st.tags.lock().unwrap().is_empty());
        let e1 = run(Part::Egs, "20261109", &|| games(0), &not_vndb).unwrap();
        let v1 = run(Part::Vndb, "20261109", &not_egs, &|| vns(1)).unwrap();
        // VNDB failing, or too few: VNDB's tag stays, ErogameScape's untouched
        assert!(run(Part::Vndb, "20261110", &not_egs, &|| anyhow::bail!("429")).is_err());
        assert!(run(Part::Vndb, "20261110", &not_egs, &|| Ok(vns(0)?[..9].to_vec())).is_err());
        let digest = |tag: &str| serde_json::from_str::<serde_json::Value>(&st.tags.lock().unwrap()[tag]).unwrap()["layers"][0]["digest"].as_str().unwrap().to_string();
        assert_eq!((digest("egs"), digest("vndb")), (e1.clone(), v1.clone()));
        // a new ErogameScape part: VNDB's as it was
        let e2 = run(Part::Egs, "20261111", &|| games(1), &not_vndb).unwrap();
        assert_ne!(e1, e2);
        assert_eq!((digest("egs"), digest("vndb")), (e2, v1));
        assert!(st.tags.lock().unwrap().contains_key("egs-20261109"), "each day's kept");
        // the server's copies
        let store = dir.join("store");
        std::fs::create_dir_all(&store).unwrap();
        for part in Part::ALL {
            pull_from(&name, part, &store.join(part.file()), None).unwrap();
        }
        assert_eq!(egs_games(&store.join(Part::Egs.file())).unwrap().0.len(), 30346);
        assert_eq!(info(&store, "egs:30345").unwrap().title, "LOOPERS");
        assert_eq!(info(&store, "vndb:v7").unwrap().title, "t7");
        assert!(info(&store, "vndb:v99999").is_none() && info(&store, "egs:99999").is_none());
        std::fs::remove_file(store.join(Part::Vndb.file())).unwrap();
        assert!(info(&store, "vndb:v7").is_none() && info(&store, "egs:30345").is_some(), "a part missing leaves the other");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn enough_to_publish() {
        assert!(counts_ok(Part::Egs, 25_000, 0, Some("LOOPERS")).is_ok());
        assert!(counts_ok(Part::Vndb, 0, 40_000, None).is_ok());
        assert!(counts_ok(Part::Egs, 0, 40_000, None).is_err());
        assert!(counts_ok(Part::Vndb, 25_000, 0, Some("LOOPERS")).is_err());
        assert!(counts_ok(Part::Egs, 25_000, 0, Some("ループス")).is_err(), "a known entry wrong");
        assert!(counts_ok(Part::Egs, 25_000, 0, None).is_err());
        assert!(counts_ok(Part::Egs, MIN_EGS, 0, Some("LOOPERS")).is_err());
        assert!(counts_ok(Part::Egs, MIN_EGS + 1, 0, Some("LOOPERS")).is_ok());
        assert!(counts_ok(Part::Vndb, 0, MIN_VNDB, None).is_err());
        assert!(counts_ok(Part::Vndb, 0, MIN_VNDB + 1, None).is_ok());
    }
}
