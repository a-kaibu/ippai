//! The game database: every ErogameScape (批評空間) game and every Japanese VN on VNDB in one
//! SQLite file, built once a day on GitHub Actions (gamedb.yml: `ippai gamedb build`) and
//! published as a public package (ghcr.io/a-kaibu/ippai-gamedb:latest, zstd), so a server that
//! cannot reach ErogameScape (it resets many hosts' connections) still identifies games and
//! shows their information. The server keeps it as `<store>/gamedb.sqlite` (Library::egs_index
//! pulls it when a day old) and meta.rs reads entries from it before asking the sites.
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

/// Where the package is (IPPAI_GAMEDB_REF: another registry, for tests).
pub fn reference() -> String {
    std::env::var("IPPAI_GAMEDB_REF").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "ghcr.io/a-kaibu/ippai-gamedb:latest".into())
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
    for n in 1.. {
        let (mut r, more) = page(n)?;
        all.append(&mut r);
        if !more {
            break;
        }
        std::thread::sleep(pause);
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

/// `ippai gamedb build <out>`: from the sites (about 3 minutes of ErogameScape at its pace, 10 of
/// VNDB at its).
pub fn build(out: &Path) -> Result<()> {
    let egs = egs_all(&meta::egs_rows).context("erogamescape")?;
    eprintln!("erogamescape: {} games", egs.len());
    let vndb = vndb_all(
        &|n| meta::vndb_page(serde_json::json!(["olang", "=", "ja"]), n).map(|(v, more)| (v.into_iter().map(prune_vndb).collect(), more)),
        Duration::from_millis(1600),
    )
    .context("vndb")?;
    eprintln!("vndb: {} VNs", vndb.len());
    write(out, &egs, &vndb)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

// ---- reading

/// The entry `id` names (egs:<n>, vndb:v<n>) from a copy (`<store>/gamedb.sqlite`), when it is
/// there and has it.
pub fn info(path: &Path, id: &str) -> Option<meta::Info> {
    if !path.is_file() {
        return None;
    }
    info_in(&Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?, id)
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

/// A GET on the registry: with the anonymous token its 401 asks for (a public package), and a
/// redirect (a blob's signed URL) followed without it.
fn registry_get(base: &str, repo: &str, path: &str, accept: &str) -> Result<ureq::Response> {
    let a = agent();
    let url = format!("{base}/v2/{repo}/{path}");
    let mut token: Option<String> = None;
    for _ in 0..2 {
        let mut req = a.get(&url).set("Accept", accept);
        if let Some(t) = &token {
            req = req.set("Authorization", &format!("Bearer {t}"));
        }
        match req.call() {
            Ok(r) if (300..400).contains(&r.status()) => {
                let loc = r.header("location").ok_or_else(|| anyhow!("{url}: redirect without a location"))?.to_string();
                return Ok(a.get(&loc).call()?);
            }
            Ok(r) => return Ok(r),
            Err(ureq::Error::Status(401, r)) if token.is_none() => {
                let ch = r.header("www-authenticate").unwrap_or("").to_string();
                let f: HashMap<String, String> = regex::Regex::new(r#"(\w+)="([^"]*)""#)
                    .unwrap()
                    .captures_iter(&ch)
                    .map(|c| (c[1].to_string(), c[2].to_string()))
                    .collect();
                let realm = f.get("realm").ok_or_else(|| anyhow!("{url}: 401 without a realm"))?;
                let scope = f.get("scope").cloned().unwrap_or_else(|| format!("repository:{repo}:pull"));
                let j: serde_json::Value =
                    a.get(realm).query("service", f.get("service").map_or("", |s| s.as_str())).query("scope", &scope).call()?.into_json()?;
                token = Some(j["token"].as_str().or(j["access_token"].as_str()).ok_or_else(|| anyhow!("{realm}: no token"))?.to_string());
            }
            Err(e) => return Err(anyhow!("{url}: {e}")),
        }
    }
    bail!("{url}: refused")
}

/// Brings the package's database to `dest` when it differs from the copy there (`have`: the
/// digest of that copy). Returns the digest now there.
pub fn pull(dest: &Path, have: Option<&str>) -> Result<String> {
    let r = reference();
    let (name, tag) = r.rsplit_once(':').filter(|(n, _)| n.contains('/')).unwrap_or((&r, "latest"));
    let (host, repo) = name.split_once('/').ok_or_else(|| anyhow!("{r}: no repository"))?;
    let local = host.split(':').next().is_some_and(|h| h == "127.0.0.1" || h == "localhost");
    let base = format!("{}{host}", if local { "http://" } else { "https://" });
    let m: serde_json::Value = registry_get(&base, repo, &format!("manifests/{tag}"), "application/vnd.oci.image.manifest.v1+json")?.into_json()?;
    let layer = &m["layers"][0];
    let digest = layer["digest"].as_str().ok_or_else(|| anyhow!("{r}: no layer"))?.to_string();
    if have == Some(digest.as_str()) && dest.is_file() {
        return Ok(digest);
    }
    let mut z = vec![];
    registry_get(&base, repo, &format!("blobs/{digest}"), "*/*")?.into_reader().take(1 << 30).read_to_end(&mut z)?;
    let sum = format!("sha256:{}", hex(&ring::digest::digest(&ring::digest::SHA256, &z)));
    if sum != digest {
        bail!("{r}: the blob is not {digest}");
    }
    let tmp = dest.with_extension("sqlite.tmp");
    std::fs::write(&tmp, zstd::decode_all(&z[..])?)?;
    // (a database it cannot read is not taken)
    egs_games(&tmp).with_context(|| format!("{r}: not a game database"))?;
    std::fs::rename(&tmp, dest)?;
    Ok(digest)
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
        let dir = std::env::temp_dir().join(format!("ippai-gamedb-{}", uuid::Uuid::new_v4()));
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
        // the tags left out are ones vndb_info would not show
        let full = serde_json::json!({"id": "v9", "title": "T", "tags": (0..30).map(|k| serde_json::json!({"name": format!("t{k}"), "rating": (k % 7) as f64 * 0.5, "spoiler": (k % 5 == 0) as u8 as f64, "category": if k % 3 == 0 { "ero" } else { "cont" }})).collect::<Vec<_>>()});
        let pruned = prune_vndb(full.clone());
        assert_eq!(meta::vndb_info_json(full).unwrap().tags, meta::vndb_info_json(pruned.clone()).unwrap().tags);
        assert!(pruned["tags"].as_array().unwrap().len() <= 10);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
