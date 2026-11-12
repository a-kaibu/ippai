//! Build-time adapters for vn-archive's SQLite exports. Runtime databases stay compact.
use super::*;
use rusqlite::types::ValueRef;
use serde_json::{json, Value};
use std::path::PathBuf;

struct Scratch(PathBuf);
impl Scratch {
    fn new(out: &Path) -> Self {
        Self(out.with_file_name(format!(".gamedb-{}.tmp", uuid::Uuid::new_v4())))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}
fn open(path: &Path) -> Result<Connection> {
    Ok(Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?)
}
fn download(url: &str, out: &Path) -> Result<()> {
    let response = ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(60))
        .build()
        .get(url)
        .set("Accept-Encoding", "identity")
        .call()?;
    let length = response.header("Content-Length").map(str::parse::<u64>).transpose()?;
    let mut input = response.into_reader().take(8 * 1024 * 1024 * 1024 + 1);
    let mut output = std::fs::File::create(out)?;
    let n = std::io::copy(&mut input, &mut output)?;
    if n > 8 * 1024 * 1024 * 1024 || length.is_some_and(|l| l != n) {
        bail!("incomplete or oversized SQLite download ({n} bytes)");
    }
    output.sync_all()?;
    Ok(())
}

pub(super) fn build(out: &Path, part: Part, local: Option<&Path>) -> Result<()> {
    let name = if part == Part::Egs { "erogamescape" } else { "vndb" };
    let url = format!("https://huggingface.co/buckets/vn-archive/mirrors/resolve/{name}.sqlite3?download=true");
    let downloaded = Scratch::new(out);
    let source = match local {
        Some(path) => path,
        None => {
            eprintln!("downloading {name} SQLite mirror");
            download(&url, &downloaded.0)?;
            &downloaded.0
        }
    };
    if source.canonicalize()? == out.canonicalize().unwrap_or_else(|_| out.to_path_buf()) {
        bail!("source and output must differ");
    }
    let c = open(source)?;
    let integrity: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        bail!("SQLite integrity: {integrity}");
    }
    let stage = Scratch::new(out);
    match part {
        Part::Egs => write(&stage.0, &egs(&c, out)?, &[])?,
        Part::Vndb => write(&stage.0, &[], &vndb(&c)?)?,
    }
    check(&stage.0, part)?;
    let dest = Connection::open(&stage.0)?;
    dest.execute("INSERT INTO info VALUES('mirror',?)", [&url])?;
    let table = if part == Part::Egs { "archive_info" } else { "_archive_info" };
    for row in rows(&c, &format!("SELECT key,value FROM {table}"))? {
        dest.execute("INSERT INTO info VALUES(?,?)", params![format!("source.{}", col(&row, "key")), col(&row, "value")])?;
    }
    if part == Part::Vndb {
        for row in rows(&c, "SELECT name,content FROM _source_documents WHERE name='README.txt' OR name IN ('LICENSE-ODBL.txt','LICENSE-DBCL.txt','LICENSE-CC0.txt')")? {
            dest.execute("INSERT INTO info VALUES(?,?)", params![format!("notice.{}", col(&row, "name")), col(&row, "content")])?;
        }
    }
    drop(dest);
    std::fs::File::open(&stage.0)?.sync_all()?;
    std::fs::rename(&stage.0, out)?;
    Ok(())
}

fn rows(c: &Connection, sql: &str) -> Result<Vec<meta::Row>> {
    let mut stmt = c.prepare(sql)?;
    let names: Vec<String> = stmt.column_names().map(|n| n.to_string()).collect();
    let result = stmt
        .query_map([], |r| {
            names
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let value = match r.get_ref(i)? {
                        ValueRef::Null => String::new(),
                        ValueRef::Text(s) => std::str::from_utf8(s).map_err(|e| rusqlite::Error::FromSqlConversionFailure(i, rusqlite::types::Type::Text, Box::new(e)))?.to_owned(),
                        ValueRef::Integer(v) => v.to_string(),
                        ValueRef::Real(v) => v.to_string(),
                        ValueRef::Blob(_) => return Err(rusqlite::Error::InvalidColumnType(i, name.clone(), rusqlite::types::Type::Blob)),
                    };
                    Ok((name.clone(), value))
                })
                .collect::<rusqlite::Result<meta::Row>>()
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(result)
}
fn egs(c: &Connection, previous: &Path) -> Result<EgsGames> {
    let sync: String = c.query_row("SELECT value FROM archive_info WHERE key='sync'", [], |r| r.get(0))?;
    let sync: Vec<Value> = serde_json::from_str(&sync)?;
    for entity in ["brand", "game"] {
        if !sync.iter().any(|s| s["entity"] == entity && s["complete"] == 1) {
            bail!("incomplete EGS {entity} export");
        }
    }
    for (table, key) in [("gamelist", "game_count"), ("brandlist", "brand_count")] {
        let count: i64 = c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        let declared: String = c.query_row("SELECT value FROM archive_info WHERE key=?", [key], |r| r.get(0))?;
        if declared.parse::<i64>()? != count {
            bail!("EGS {table} count mismatch");
        }
    }
    let mut more: HashMap<u32, Vec<meta::Row>> = HashMap::new();
    if previous.exists() {
        let old = open(previous)?;
        let mut stmt = old.prepare("SELECT id,more FROM egs")?;
        for row in stmt.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)))? {
            let (id, value) = row?;
            more.insert(id, serde_json::from_str(&value)?);
        }
        eprintln!("EGS: preserving previous tags/series; the mirror does not supply them");
    } else {
        eprintln!("EGS: no previous database; building without tags/series (use update to preserve published enrichment)");
    }
    rows(
        c,
        "SELECT g.id,g.gamename,g.furigana,g.sellday,g.median,g.count2,g.dmm,g.dmm_subsc,g.genre,g.shoukai,g.dlsite_id,g.dlsite_domain,b.brandname,b.url,g.model FROM gamelist g LEFT JOIN brandlist b ON g.brandname=b.id ORDER BY g.id",
    )?
    .into_iter()
    .map(|row| {
        let id = col(&row, "id").parse()?;
        Ok((id, row, more.remove(&id).unwrap_or_default()))
    })
    .collect()
}

fn date(n: i64) -> Option<String> {
    if n == 0 {
        None
    } else if n / 10000 == 9999 {
        Some("TBA".into())
    } else if n / 100 % 100 == 99 {
        Some(format!("{:04}", n / 10000))
    } else if n % 100 == 99 {
        Some(format!("{:04}-{:02}", n / 10000, n / 100 % 100))
    } else {
        Some(format!("{:04}-{:02}-{:02}", n / 10000, n / 100 % 100, n % 100))
    }
}
fn image_url(id: &str) -> Result<String> {
    let split = id.find(|c: char| c.is_ascii_digit()).context("invalid image id")?;
    let n: u64 = id[split..].parse()?;
    Ok(format!("https://t.vndb.org/{}/{:02}/{n}.jpg", &id[..split], n % 100))
}
fn vndb(c: &Connection) -> Result<Vec<Value>> {
    let complete: String = c.query_row("SELECT value FROM _archive_info WHERE key='complete'", [], |r| r.get(0))?;
    if complete != "true" {
        bail!("incomplete VNDB export");
    }
    for table in [
        "vn",
        "vn_titles",
        "releases",
        "releases_vn",
        "releases_producers",
        "producers",
        "images",
        "tags",
        "tags_vn",
        "users",
        "vn_extlinks",
        "extlinks",
        "wikidata",
    ] {
        let declared: i64 = c.query_row("SELECT rows FROM _source_tables WHERE name=?", [table], |r| r.get(0))?;
        let count: i64 = c.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        if count != declared {
            bail!("VNDB {table} count mismatch");
        }
    }
    let mut all = Vec::new();
    let mut at = HashMap::new();
    let mut stmt = c.prepare("SELECT v.id,t.title,t.latin,v.c_votecount,v.c_rating,v.description,i.id,i.c_sexual_avg,i.c_violence_avg FROM vn v JOIN vn_titles t ON t.id=v.id AND t.lang=v.olang LEFT JOIN images i ON i.id=v.c_image WHERE v.olang='ja' ORDER BY CAST(substr(v.id,2) AS INTEGER)")?;
    let mut r = stmt.query([])?;
    while let Some(r) = r.next()? {
        let id: String = r.get(0)?;
        let original: String = r.get(1)?;
        let title = r.get::<_, Option<String>>(2)?.unwrap_or_else(|| original.clone());
        let image = match (r.get::<_, Option<String>>(6)?, r.get::<_, Option<f64>>(7)?, r.get::<_, Option<f64>>(8)?) {
            (Some(id), Some(sexual), Some(violence)) => {
                json!({"url":image_url(&id)?,"sexual":sexual / 100.,"violence":violence / 100.})
            }
            _ => Value::Null,
        };
        at.insert(id.clone(), all.len());
        all.push(json!({"id":id,"alttitle":if original == title {None} else {Some(original)},"title":title,
            "votecount":r.get::<_,i64>(3)?,"rating":r.get::<_,Option<f64>>(4)?.map(|n|n / 10.),
            "description":r.get::<_,Option<String>>(5)?.filter(|s| !s.is_empty()),"image":image,"released":null,"developers":[],"tags":[],"extlinks":[]}));
    }
    let expected: i64 = c.query_row("SELECT count(*) FROM vn WHERE olang='ja'", [], |r| r.get(0))?;
    if all.len() as i64 != expected {
        bail!("VNDB original-language titles missing");
    }
    let mut stmt = c.prepare("SELECT rv.vid,min(r.released) FROM releases_vn rv JOIN releases r ON r.id=rv.id WHERE r.official=1 AND rv.rtype<>'trial' AND r.released<>0 GROUP BY rv.vid")?;
    for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
        let (id, n) = r?;
        if let Some(&i) = at.get(&id) {
            all[i]["released"] = json!(date(n));
        }
    }
    let mut stmt = c.prepare("SELECT DISTINCT rv.vid,p.id,p.name,p.latin FROM releases_vn rv JOIN releases r ON r.id=rv.id JOIN releases_producers rp ON rp.id=r.id JOIN producers p ON p.id=rp.pid WHERE r.official=1 AND rp.developer=1 ORDER BY CAST(substr(p.id,2) AS INTEGER)")?;
    for r in stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(2)?, r.get::<_, Option<String>>(3)?)))? {
        let (id, original, latin) = r?;
        if let Some(&i) = at.get(&id) {
            let name = latin.unwrap_or_else(|| original.clone());
            all[i]["developers"].as_array_mut().unwrap().push(json!({"name":name,"original":if name==original {None} else {Some(original)}}));
        }
    }
    // VNDB API's direct votes, not inherited tags. Preserve PostgreSQL real rounding.
    let mut stmt = c.prepare("SELECT tv.vid,t.id,t.name,t.cat,t.defaultspoil,avg(CASE WHEN tv.vote>0 THEN tv.vote END),sum(sign(tv.vote)),count(*),sum(tv.spoiler),count(tv.spoiler) FROM tags_vn tv JOIN tags t ON t.id=tv.tag JOIN vn v ON v.id=tv.vid LEFT JOIN users u ON u.id=tv.uid WHERE v.olang='ja' AND t.cat<>'ero' AND tv.ignore=0 AND (u.id IS NULL OR u.perm_tag=1) GROUP BY tv.vid,t.id HAVING sum(sign(tv.vote))>0 ORDER BY CAST(substr(t.id,2) AS INTEGER)")?;
    let mut r = stmt.query([])?;
    while let Some(r) = r.next()? {
        let id: String = r.get(0)?;
        let Some(&i) = at.get(&id) else { continue };
        let rating = (r.get::<_, f64>(5)? * r.get::<_, f64>(6)? / r.get::<_, f64>(7)?) as f32 as f64;
        let count: i64 = r.get(9)?;
        let spoiler = if count == 0 { r.get::<_, i64>(4)? == 0 } else { r.get::<_, i64>(8)? * 5 <= count * 2 };
        if spoiler && rating >= 2. {
            all[i]["tags"].as_array_mut().unwrap().push(json!({"name":r.get::<_,String>(2)?,"category":r.get::<_,String>(3)?,"rating":rating,"spoiler":0}));
        }
    }
    let mut wiki = HashMap::new();
    for row in rows(c, "SELECT * FROM wikidata")? {
        wiki.insert(col(&row, "id").to_owned(), row);
    }
    let mut seen = std::collections::HashSet::new();
    for row in rows(
        c,
        "SELECT ve.id,e.site,e.value FROM vn_extlinks ve JOIN extlinks e ON e.id=ve.link WHERE e.site IN ('wikidata','renai') ORDER BY CASE e.site WHEN 'wikidata' THEN 0 ELSE 1 END,ve.link",
    )? {
        let id = col(&row, "id");
        let Some(&i) = at.get(id) else { continue };
        let links = all[i]["extlinks"].as_array_mut().unwrap();
        let value = col(&row, "value");
        if col(&row, "site") == "renai" {
            link(links, "Renai.us", format!("https://renai.us/game/{value}"));
        } else if seen.insert(id.to_owned()) {
            wikilinks(links, value, wiki.get(value))?;
        }
    }
    Ok(all.into_iter().map(prune_vndb).collect())
}
fn link(out: &mut Vec<Value>, label: &str, url: String) {
    out.push(json!({"label":label,"url":url}));
}
// PostgreSQL array text is retained by the SQLite exporter (including quoted commas).
fn array(s: &str) -> Result<Vec<String>> {
    if s.is_empty() || s == "{}" {
        return Ok(vec![]);
    }
    let s = s.strip_prefix('{').and_then(|s| s.strip_suffix('}')).context("invalid PostgreSQL array")?;
    let mut values = Vec::new();
    let (mut value, mut quoted, mut escaped, mut was_quoted) = (String::new(), false, false, false);
    for c in s.chars() {
        if escaped {
            value.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == '"' {
            quoted = !quoted;
            was_quoted = true;
        } else if c == ',' && !quoted {
            if was_quoted || value != "NULL" {
                values.push(std::mem::take(&mut value));
            } else {
                value.clear();
            }
            was_quoted = false;
        } else {
            value.push(c);
        }
    }
    if quoted || escaped {
        bail!("unterminated PostgreSQL array");
    }
    if was_quoted || value != "NULL" {
        values.push(value);
    }
    Ok(values)
}
fn wikilinks(out: &mut Vec<Value>, id: &str, row: Option<&meta::Row>) -> Result<()> {
    let empty = vec![];
    let row = row.unwrap_or(&empty);
    for (key, lang) in [("enwiki", "en"), ("jawiki", "ja")] {
        let title = col(row, key);
        if !title.is_empty() {
            link(out, &format!("Wikipedia ({lang})"), format!("https://{lang}.wikipedia.org/wiki/{}", title.replace(' ', "_").replace('?', "%3f")));
        }
    }
    link(out, "Wikidata", format!("https://www.wikidata.org/wiki/Q{id}"));
    for (key, label, prefix, suffix) in [
        ("mobygames", "MobyGames", "https://www.mobygames.com/game/", ""),
        ("mobygames_game", "MobyGames", "https://www.mobygames.com/game/", "/"),
        ("gamefaqs_game", "GameFAQs", "https://gamefaqs.gamespot.com/-/", "-"),
        ("vgmdb_product", "VGMdb", "https://vgmdb.net/product/", ""),
        ("acdb_source", "ACDB", "https://www.animecharactersdatabase.com/source.php?id=", ""),
        ("indiedb_game", "IndieDB", "https://www.indiedb.com/games/", ""),
        ("howlongtobeat", "HowLongToBeat", "http://howlongtobeat.com/game.php?id=", ""),
        ("igdb_game", "IGDB", "https://www.igdb.com/games/", ""),
        ("pcgamingwiki", "PCGamingWiki", "https://www.pcgamingwiki.com/wiki/", ""),
        ("lutris", "Lutris", "https://lutris.net/games/", ""),
        ("wine", "Wine AppDB", "https://appdb.winehq.org/appview.php?iAppId=", ""),
    ] {
        if key == "mobygames" && !array(col(row, "mobygames_game"))?.is_empty() {
            continue;
        }
        for value in array(col(row, key))? {
            link(out, label, format!("{prefix}{value}{suffix}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch() -> Scratch {
        Scratch::new(&std::env::temp_dir().join("gamedb.sqlite"))
    }
    fn egs_fixture() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE archive_info(key,value); INSERT INTO archive_info VALUES('sync','[{\"entity\":\"brand\",\"complete\":1},{\"entity\":\"game\",\"complete\":1}]'),('game_count','2'),('brand_count','1'); CREATE TABLE brandlist(id,brandname,url); INSERT INTO brandlist VALUES(83,'Key','https://key.visualarts.gr.jp/'); CREATE TABLE gamelist(id,gamename,furigana,sellday,median,count2,dmm,dmm_subsc,genre,shoukai,dlsite_id,dlsite_domain,brandname,model); INSERT INTO gamelist VALUES(30345,'LOOPERS',NULL,'2021-05-28',76,412,NULL,NULL,NULL,NULL,NULL,NULL,83,'PC'),(40000,'new',NULL,NULL,NULL,0,NULL,NULL,NULL,NULL,NULL,NULL,NULL,'PC');").unwrap();
        c
    }
    #[test]
    fn egs_refresh_preserves_enrichment_but_removes_deleted_games() {
        let previous = scratch();
        let tag = vec![("src".into(), "tag".into()), ("val".into(), "時間ループ".into())];
        let series = vec![("src".into(), "series".into()), ("val".into(), "シリーズ".into())];
        write(&previous.0, &[(30345, vec![], vec![tag.clone(), series.clone()]), (999, vec![], vec![tag.clone()])], &[]).unwrap();
        let c = egs_fixture();
        let games = egs(&c, &previous.0).unwrap();
        assert_eq!(games.len(), 2);
        assert_eq!(games[0].2, vec![tag, series]);
        assert_eq!(col(&games[0].1, "count2"), "412");
        assert_eq!(col(&games[0].1, "brandname"), "Key");
        assert!(games[1].2.is_empty());
        c.execute("UPDATE archive_info SET value='3' WHERE key='game_count'", []).unwrap();
        assert!(egs(&c, &previous.0).is_err());
    }
    #[test]
    fn rejected_export_never_replaces_previous_output() {
        let source = scratch();
        let out = scratch();
        let c = egs_fixture();
        c.backup("main", &source.0, None::<fn(rusqlite::backup::Progress)>).unwrap();
        std::fs::write(&out.0, b"keep this file").unwrap();
        assert!(build(&out.0, Part::Egs, Some(&source.0)).is_err());
        assert_eq!(std::fs::read(&out.0).unwrap(), b"keep this file");
        assert!(build(&source.0, Part::Egs, Some(&source.0)).is_err());
        assert_eq!(open(&source.0).unwrap().query_row("SELECT count(*) FROM gamelist", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
    }
    fn vndb_fixture() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE _archive_info(key,value); INSERT INTO _archive_info VALUES('complete','true');
CREATE TABLE _source_tables(name,rows);
CREATE TABLE vn(id,olang,c_image,c_votecount,c_rating,description);
CREATE TABLE vn_titles(id,lang,title,latin);
CREATE TABLE images(id,c_sexual_avg,c_violence_avg);
CREATE TABLE releases(id,released,official);
CREATE TABLE releases_vn(id,vid,rtype);
CREATE TABLE releases_producers(id,pid,developer);
CREATE TABLE producers(id,name,latin);
CREATE TABLE tags(id,name,cat,defaultspoil);
CREATE TABLE tags_vn(vid,tag,uid,vote,spoiler,ignore);
CREATE TABLE users(id,perm_tag);
CREATE TABLE vn_extlinks(id,link);
CREATE TABLE extlinks(id,site,value);
CREATE TABLE wikidata(id);
INSERT INTO vn VALUES('v1','ja','cv123',5,762,'description'),('v2','en',NULL,0,NULL,'');
INSERT INTO vn_titles VALUES('v1','ja','原題','Gendai'),('v1','en','English',NULL),('v2','en','Other',NULL);
INSERT INTO images VALUES('cv123',165,0);
INSERT INTO releases VALUES('r1',20019999,1),('r2',19990101,1),('r3',19900101,0),('r4',20010399,1);
INSERT INTO releases_vn VALUES('r1','v1','complete'),('r2','v1','trial'),('r3','v1','complete'),('r4','v1','complete');
INSERT INTO producers VALUES('p10','開発者','Dev'),('p2','Trial',NULL),('p3','Excluded',NULL);
INSERT INTO releases_producers VALUES('r1','p10',1),('r2','p2',1),('r3','p3',1),('r4','p10',1);
INSERT INTO tags VALUES('g1','safe','cont',0),('g2','spoiler','cont',1),('g3','sexual','ero',0),('g4','negative','cont',0),('g5','weighted','cont',0);
INSERT INTO users VALUES('u1',0);
INSERT INTO tags_vn VALUES('v1','g1',NULL,3,0,0),('v1','g1','u1',-3,2,0),('v1','g1',NULL,-3,2,1),('v1','g2',NULL,3,NULL,0),('v1','g3',NULL,3,0,0),('v1','g4',NULL,3,0,0),('v1','g4',NULL,-1,0,0),('v1','g5',NULL,3,0,0),('v1','g5',NULL,2,0,0),('v1','g5',NULL,2,0,0);").unwrap();
        for table in [
            "vn",
            "vn_titles",
            "images",
            "releases",
            "releases_vn",
            "releases_producers",
            "producers",
            "tags",
            "tags_vn",
            "users",
            "vn_extlinks",
            "extlinks",
            "wikidata",
        ] {
            c.execute(&format!("INSERT INTO _source_tables SELECT ?,count(*) FROM {table}"), [table]).unwrap();
        }
        c
    }
    #[test]
    fn vndb_matches_api_semantics_for_titles_dates_developers_and_votes() {
        let c = vndb_fixture();
        let games = vndb(&c).unwrap();
        assert_eq!(games.len(), 1);
        let g = &games[0];
        let info = meta::vndb_info_json(g.clone()).unwrap();
        assert_eq!(info.title, "原題");
        assert_eq!(info.release, "2001-03");
        assert_eq!(info.score, Some(76));
        assert_eq!(info.tags, vec!["safe", "weighted"]);
        assert_eq!(info.cover_url, "https://t.vndb.org/cv/23/123.jpg");
        assert_eq!(g["title"], "Gendai");
        assert_eq!(g["alttitle"], "原題");
        assert_eq!(g["rating"], 76.2);
        assert_eq!(g["released"], "2001-03");
        assert_eq!(g["image"]["url"], "https://t.vndb.org/cv/23/123.jpg");
        assert_eq!(g["image"]["sexual"], 1.65);
        assert_eq!(g["developers"], json!([{"name":"Trial","original":null},{"name":"Dev","original":"開発者"}]));
        assert_eq!(g["tags"].as_array().unwrap().len(), 2);
        assert_eq!(g["tags"][0]["name"], "safe");
        assert_eq!(g["tags"][1]["rating"], json!(7.0f32 / 3.0));
        c.execute("DELETE FROM vn_titles WHERE id='v1' AND lang='ja'", []).unwrap();
        assert!(vndb(&c).is_err());
    }
    #[test]
    fn dates_and_wikidata_arrays() {
        assert_eq!(date(0), None);
        assert_eq!(date(99999999).as_deref(), Some("TBA"));
        assert_eq!(date(20269999).as_deref(), Some("2026"));
        assert_eq!(date(20261199).as_deref(), Some("2026-11"));
        assert_eq!(date(20261112).as_deref(), Some("2026-11-12"));
        assert_eq!(array(r#"{abc,"a,b","a\"b",NULL,"NULL"}"#).unwrap(), vec!["abc", "a,b", "a\"b", "NULL"]);
        assert!(array(r#"{"broken}"#).is_err());
        assert!(array("{broken\\}").is_err());
        let mut links = vec![];
        wikilinks(&mut links, "42", Some(&vec![("enwiki".into(), "A ?".into()), ("mobygames".into(), "{old}".into()), ("mobygames_game".into(), "{123}".into())])).unwrap();
        assert_eq!(
            links,
            json!([{"label":"Wikipedia (en)","url":"https://en.wikipedia.org/wiki/A_%3f"},{"label":"Wikidata","url":"https://www.wikidata.org/wiki/Q42"},{"label":"MobyGames","url":"https://www.mobygames.com/game/123/"}])
                .as_array()
                .unwrap()
                .clone()
        );
    }
    #[test]
    fn downloads_follow_redirects_and_reject_truncation() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            for response in [
                "HTTP/1.1 302 Found\r\nLocation: /file\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata",
                "HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\nshort",
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut header = Vec::new();
                let mut byte = [0];
                while !header.ends_with(b"\r\n\r\n") {
                    stream.read_exact(&mut byte).unwrap();
                    header.push(byte[0]);
                }
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            std::env::remove_var(key);
        }
        let out = scratch();
        download(&url, &out.0).unwrap();
        assert_eq!(std::fs::read(&out.0).unwrap(), b"data");
        assert!(download(&url, &out.0).is_err());
        server.join().unwrap();
    }
}
