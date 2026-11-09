//! Game metadata for the launcher (title, brand, release date, score, tags, story, cover)
//! from ErogameScape (批評空間) and, as a fallback or when it cannot be reached, VNDB.
//! A port of vnbox's internal/meta (Go).
//!
//! The ErogameScape queries follow minuspiral/playnite-with-erogamescape (MIT License,
//! Copyright (c) 2025 minuspiral): one SQL form endpoint, hex-encoded UTF-8 literals for
//! Japanese keywords, and at least 2.5 s between requests.

use anyhow::{anyhow, bail, Result};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use unicode_normalization::UnicodeNormalization;

/// What the launcher shows about a game (`meta` in games/<id>.json).
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Info {
    /// "erogamescape" or "vndb"
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub egs_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vndb_id: Option<String>,
    pub title: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub furigana: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub brand: String,
    /// yyyy-mm-dd
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub release: String,
    /// 0-100: ErogameScape median or VNDB rating
    #[serde(skip_serializing_if = "Option::is_none")]
    pub score: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub votes: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub series: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub description: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub links: Vec<Link>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub cover_url: String,
    pub fetched: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Link {
    pub label: String,
    pub url: String,
}

/// A search hit.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub source: &'static str,
    /// ErogameScape game id or VNDB id
    pub id: String,
    pub title: String,
    pub brand: String,
    pub release: String,
    pub votes: u32,
}

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";
const EGS_SQL: &str = "https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/sql_for_erogamer_form.php";
const EGS_GAME: &str = "https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?game=";
const VNDB_VN: &str = "https://api.vndb.org/kana/vn";

/// `url`, or what the environment variable `var` names instead (tests/meta-browser.mjs points
/// them at a local stand-in).
fn endpoint(var: &str, url: &'static str) -> String {
    std::env::var(var).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| url.to_string())
}

fn agent() -> &'static ureq::Agent {
    static A: LazyLock<ureq::Agent> = LazyLock::new(|| {
        ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).user_agent(UA).try_proxy_from_env(true).build()
    });
    &A
}

// ---- ErogameScape

/// Runs one SQL statement on ErogameScape's form; rows keyed by column name.
fn egs_query(sql: &str) -> Result<Vec<Vec<(String, String)>>> {
    static NEXT: Mutex<Option<Instant>> = Mutex::new(None);
    {
        let mut next = NEXT.lock().unwrap();
        if let Some(t) = *next {
            std::thread::sleep(t.saturating_duration_since(Instant::now()));
        }
        *next = Some(Instant::now() + Duration::from_millis(2500));
    }
    let page = agent().post(&endpoint("IPPAI_META_EGS_SQL", EGS_SQL)).send_form(&[("sql", sql)])?.into_string()?;
    Ok(parse_table(&page))
}

fn col<'a>(row: &'a [(String, String)], name: &str) -> &'a str {
    row.iter().find(|(k, _)| k == name).map_or("", |(_, v)| v)
}

/// The first HTML table; its first row holds the column names.
fn parse_table(page: &str) -> Vec<Vec<(String, String)>> {
    static TABLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<table[^>]*>(.*?)</table>").unwrap());
    static ROW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<tr[^>]*>(.*?)</tr>").unwrap());
    static CELL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)<t[hd][^>]*>(.*?)</t[hd]>").unwrap());
    static TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<[^>]*>").unwrap());
    let Some(t) = TABLE.captures(page) else { return vec![] };
    let mut cols = vec![];
    let mut out = vec![];
    for (i, r) in ROW.captures_iter(&t[1]).enumerate() {
        let cells: Vec<String> =
            CELL.captures_iter(&r[1]).map(|c| unescape(TAG.replace_all(&c[1], "").trim())).collect();
        if i == 0 {
            cols = cells;
        } else {
            out.push(cols.iter().cloned().zip(cells).collect());
        }
    }
    out
}

fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    static ENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&(#x[0-9a-fA-F]+|#[0-9]+|[a-z]+);").unwrap());
    ENT.replace_all(s, |c: &regex::Captures| {
        let e = &c[1];
        let ch = if let Some(h) = e.strip_prefix("#x") {
            u32::from_str_radix(h, 16).ok().and_then(char::from_u32)
        } else if let Some(d) = e.strip_prefix('#') {
            d.parse().ok().and_then(char::from_u32)
        } else {
            match e {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => None,
            }
        };
        ch.map_or_else(|| c[0].to_string(), String::from)
    })
    .into_owned()
}

/// An SQL string literal. Non-ASCII text goes through decode(hex): the form rejects non-ASCII
/// bytes in quoted strings.
fn literal(s: &str) -> String {
    if s.is_ascii() {
        return format!("'{}'", s.replace('\'', "''"));
    }
    let hex: String = s.bytes().map(|b| format!("{b:02x}")).collect();
    format!("convert_from(decode('{hex}','hex'),'UTF8')")
}

fn egs_search(keyword: &str) -> Result<Vec<Candidate>> {
    let esc = keyword.replace('!', "!!").replace('%', "!%").replace('_', "!_");
    let rows = egs_query(&format!(
        "SELECT g.id, g.gamename, g.sellday, g.count2, b.brandname
FROM gamelist g LEFT JOIN brandlist b ON g.brandname = b.id
WHERE g.gamename ILIKE '%' || {} || '%' ESCAPE '!'
ORDER BY g.count2 DESC NULLS LAST LIMIT 30",
        literal(&esc)
    ))?;
    Ok(rows
        .iter()
        .map(|r| Candidate {
            source: "erogamescape",
            id: col(r, "id").into(),
            title: col(r, "gamename").into(),
            brand: col(r, "brandname").into(),
            release: col(r, "sellday").into(),
            votes: col(r, "count2").parse().unwrap_or(0),
        })
        .collect())
}

/// Every PC game on ErogameScape: (id, name, votes), in windows of 5000 ids (the form's own
/// limit on rows), as ryoha000/launcherg gathers its list.
pub fn egs_all_games() -> Result<Vec<(u32, String, u32)>> {
    let max: u32 = egs_query("SELECT max(id) AS max FROM gamelist")?.first().map(|r| col(r, "max").parse()).transpose()?.unwrap_or(0);
    let mut out = vec![];
    let mut from = 0;
    while from <= max {
        for r in egs_query(&format!(
            "SELECT id, gamename, count2 FROM gamelist WHERE id >= {from} AND id < {} AND model = 'PC' ORDER BY id",
            from + 5000
        ))? {
            if let Ok(id) = col(&r, "id").parse() {
                out.push((id, col(&r, "gamename").to_string(), col(&r, "count2").parse().unwrap_or(0)));
            }
        }
        from += 5000;
    }
    if out.is_empty() {
        bail!("erogamescape: no games listed");
    }
    Ok(out)
}

/// The list ryoha000/launcherg ships (public domain; ErogameScape's, without votes and not
/// the newest games): for when ErogameScape cannot be reached.
pub fn seed_games() -> Result<Vec<(u32, String, u32)>> {
    #[derive(Deserialize)]
    struct G {
        id: u32,
        gamename: String,
    }
    let url = endpoint("IPPAI_META_SEED", "https://raw.githubusercontent.com/ryoha000/launcherg/main/script/all_games.json");
    let mut body = vec![];
    agent().get(&url).timeout(Duration::from_secs(60)).call()?.into_reader().take(64 << 20).read_to_end(&mut body)?;
    let list: Vec<G> = serde_json::from_slice(&body)?;
    if list.is_empty() {
        bail!("{url}: no games");
    }
    Ok(list.into_iter().map(|g| (g.id, g.gamename, 0)).collect())
}

/// POV groups shown as tags (genre, setting, tone).
const POV_GROUPS: [&str; 3] = ["ジャンル", "背景", "傾向"];

fn egs_get(id: u32) -> Result<Info> {
    let rows = egs_query(&format!(
        "SELECT g.id, g.gamename, g.furigana, g.sellday, g.median, g.count2,
g.dmm, g.dmm_subsc, g.genre, g.shoukai, g.dlsite_id, g.dlsite_domain, b.brandname, b.url
FROM gamelist g LEFT JOIN brandlist b ON g.brandname = b.id WHERE g.id = {id}"
    ))?;
    let Some(r) = rows.first() else { bail!("erogamescape: no game {id}") };
    let mut in_ = Info {
        source: "erogamescape".into(),
        egs_id: Some(id),
        title: col(r, "gamename").into(),
        furigana: col(r, "furigana").into(),
        brand: col(r, "brandname").into(),
        score: col(r, "median").parse().ok(),
        votes: col(r, "count2").parse().ok(),
        ..Default::default()
    };
    let d = col(r, "sellday");
    if d.len() >= 4 && &d[..4] < "2030" {
        // 2030+ means "not yet decided"
        in_.release = d.into();
    }
    in_.links.push(Link { label: "批評空間".into(), url: format!("{EGS_GAME}{id}") });
    let s = col(r, "shoukai");
    if s.starts_with("http") {
        in_.links.push(Link { label: "公式".into(), url: s.into() });
    } else {
        in_.description = s.into();
    }
    if !col(r, "url").is_empty() {
        in_.links.push(Link { label: "ブランド".into(), url: col(r, "url").into() });
    }
    let dmm = [col(r, "dmm_subsc"), col(r, "dmm")].into_iter().find(|s| !s.is_empty());
    if let Some(x) = dmm {
        in_.cover_url = format!("https://pics.dmm.co.jp/digital/pcgame/{x}/{x}pl.jpg");
        in_.links.push(Link { label: "DMM".into(), url: format!("https://dlsoft.dmm.co.jp/detail/{x}/") });
    }
    let dl = col(r, "dlsite_id");
    if !dl.is_empty() {
        let dom = Some(col(r, "dlsite_domain")).filter(|s| !s.is_empty()).unwrap_or("pro");
        in_.links.push(Link { label: "DLsite".into(), url: format!("https://www.dlsite.com/{dom}/work/=/product_id/{dl}.html") });
    }
    push_unique(&mut in_.tags, col(r, "genre"));
    // POV tags (voted by users) and the series
    static POV_SUFFIX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(仕立てのゲーム|系のゲーム|系ゲーム|なゲーム|のゲーム|な作品|ゲーム|ゲー)$").unwrap());
    if let Ok(more) = egs_query(&format!(
        "(SELECT 'tag' AS src, p.title AS val, p.system_group AS grp, pt.count AS cnt
FROM povgroups_toukei pt JOIN povlist p ON pt.pov = p.id WHERE pt.game = {id} ORDER BY pt.count DESC)
UNION ALL (SELECT 'series', g.name, '', 0 FROM belong_to_gamegroup_list b JOIN gamegrouplist g ON b.gamegroup = g.id WHERE b.game = {id} LIMIT 1)"
    )) {
        for m in &more {
            match col(m, "src") {
                "tag" => {
                    let n: u32 = col(m, "cnt").parse().unwrap_or(0);
                    if n >= 2 && POV_GROUPS.contains(&col(m, "grp")) && in_.tags.len() < 12 {
                        push_unique(&mut in_.tags, &POV_SUFFIX.replace(col(m, "val"), ""));
                    }
                }
                "series" => in_.series = col(m, "val").into(),
                _ => {}
            }
        }
    }
    Ok(in_)
}

fn push_unique(list: &mut Vec<String>, v: &str) {
    if !v.is_empty() && !list.iter().any(|x| x == v) {
        list.push(v.into());
    }
}

// ---- VNDB

#[derive(Deserialize, Default)]
#[serde(default)]
struct VndbVn {
    id: String,
    title: String,
    alttitle: Option<String>,
    released: Option<String>,
    votecount: u32,
    rating: Option<f64>,
    description: Option<String>,
    developers: Vec<VndbDev>,
    image: Option<VndbImage>,
    tags: Vec<VndbTag>,
    extlinks: Vec<Link>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct VndbDev {
    name: String,
    original: Option<String>,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct VndbImage {
    url: String,
    sexual: f64,
    violence: f64,
}
#[derive(Deserialize, Default)]
#[serde(default)]
struct VndbTag {
    name: String,
    rating: f64,
    spoiler: f64,
    category: String,
}

impl VndbVn {
    /// The original-script title when there is one.
    fn name(&self) -> String {
        self.alttitle.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| self.title.clone())
    }
    fn brand(&self) -> String {
        self.developers.first().map_or_else(String::new, |d| d.original.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| d.name.clone()))
    }
}

fn vndb_query(filters: serde_json::Value, fields: &str, n: u32) -> Result<Vec<VndbVn>> {
    #[derive(Deserialize)]
    struct Out {
        results: Vec<VndbVn>,
    }
    let body = serde_json::json!({ "filters": filters, "fields": fields, "results": n });
    let out: Out = agent().post(&endpoint("IPPAI_META_VNDB", VNDB_VN)).send_json(body)?.into_json()?;
    Ok(out.results)
}

fn vndb_search(keyword: &str) -> Result<Vec<Candidate>> {
    let res = vndb_query(serde_json::json!(["search", "=", keyword]), "title,alttitle,released,votecount,developers{name,original}", 15)?;
    let mut out = vec![];
    for vn in &res {
        let c = Candidate {
            source: "vndb",
            id: vn.id.clone(),
            title: vn.name(),
            brand: vn.brand(),
            release: vn.released.clone().unwrap_or_default(),
            votes: vn.votecount,
        };
        if vn.alttitle.as_deref().is_some_and(|s| !s.is_empty()) {
            // let the romanised title match too
            out.push(Candidate { title: vn.title.clone(), ..c.clone() });
        }
        out.push(c);
    }
    Ok(out)
}

fn vndb_get(id: &str) -> Result<Info> {
    let res = vndb_query(
        serde_json::json!(["id", "=", id]),
        "title,alttitle,released,votecount,rating,description,developers{name,original},image{url,sexual,violence},tags{name,rating,spoiler,category},extlinks{url,label}",
        1,
    )?;
    let Some(vn) = res.into_iter().next() else { bail!("vndb: no {id}") };
    static BBCODE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[/?[a-z]+(=[^\]]+)?\]").unwrap());
    let mut in_ = Info {
        source: "vndb".into(),
        vndb_id: Some(vn.id.clone()),
        title: vn.name(),
        brand: vn.brand(),
        release: vn.released.clone().unwrap_or_default(),
        score: vn.rating.map(|r| r.round() as u32),
        votes: Some(vn.votecount),
        description: BBCODE.replace_all(vn.description.as_deref().unwrap_or(""), "").into_owned(),
        ..Default::default()
    };
    in_.links.push(Link { label: "VNDB".into(), url: format!("https://vndb.org/{}", vn.id) });
    in_.links.extend(vn.extlinks);
    // eroge package art is usually rated "suggestive" (1-1.9); skip explicit (2) or violent covers
    if let Some(img) = vn.image.filter(|i| i.sexual < 2.0 && i.violence < 2.0) {
        in_.cover_url = img.url;
    }
    let mut tags = vn.tags;
    tags.sort_by(|a, b| b.rating.total_cmp(&a.rating));
    for t in tags {
        if t.spoiler == 0.0 && t.category != "ero" && t.rating >= 2.0 && in_.tags.len() < 10 {
            in_.tags.push(t.name);
        }
    }
    Ok(in_)
}

// ---- matching

/// Edition words that distribution titles carry but database titles do not.
static TRIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)((ダウンロード|web|ウェブ)\s*)?(体験|お試し読み|お試し|プレイアブル)\s*版|(ダウンロード|web|ウェブ)\s*版|trial(\s*version|\s*edition)?|demo|taiken|ver\.?\s*[0-9.]+|[-－‐]?\s*(web|dl)\s*$").unwrap()
});

/// Full-width ASCII to half-width, half-width kana to full-width.
fn fold(s: &str) -> String {
    s.nfkc().collect()
}

/// A keyword for a substring search from a package title.
pub fn search_key(title: &str) -> String {
    let f = fold(title);
    let s = TRIAL.replace_all(&f, " ");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.trim_matches(|c| " -_~〜～()（）[]【】「」".contains(c)).to_string()
}

/// The keyword to try first and, when the title has symbols whose spelling differs between
/// databases (～ vs ~ ...), its longest symbol-free part as a retry.
pub fn search_keys(title: &str) -> Vec<String> {
    static SEG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s~〜～・:：!！?？()（）\[\]【】「」『』]+").unwrap());
    let k = search_key(title);
    let longest = SEG.split(&k).max_by_key(|p| p.chars().count()).unwrap_or("").to_string();
    let mut keys = vec![k.clone()];
    if longest != k && longest.chars().count() >= 3 {
        keys.push(longest);
    }
    keys
}

/// Width and case folded, edition words, spaces and symbols dropped.
fn norm(s: &str) -> String {
    TRIAL.replace_all(&fold(s).to_lowercase(), "").chars().filter(|c| c.is_alphanumeric() || *c == 'ー').collect()
}

/// The candidate matching title: an exact match after normalisation, else the closest one that
/// contains (or is contained in) the title; ties go to the most voted entry.
pub fn best<'a>(title: &str, cands: &'a [Candidate]) -> Option<&'a Candidate> {
    let q = norm(title);
    if q.is_empty() {
        return None;
    }
    cands
        .iter()
        .filter_map(|c| {
            let n = norm(&c.title);
            let score = if n.is_empty() {
                return None;
            } else if n == q {
                1i64 << 20
            } else if n.contains(&q) || q.contains(&n) {
                (1i64 << 19) - (n.chars().count() as i64 - q.chars().count() as i64).abs() * 1000
            } else {
                return None;
            };
            Some((score + c.votes.min(999) as i64, c))
        })
        .fold(None, |acc: Option<(i64, &Candidate)>, x| if acc.is_some_and(|a| a.0 >= x.0) { acc } else { Some(x) })
        .map(|x| x.1)
}

// ---- entry points

/// The game best matching `title` (a package title such as "○○ 体験版"). `id` may pin the
/// entry: "egs:12345" or "vndb:v1195".
pub fn lookup(title: &str, id: Option<&str>) -> Result<Info> {
    let mut in_ = match id {
        Some(id) => {
            if let Some(n) = id.strip_prefix("egs:") {
                egs_get(n.parse().map_err(|_| anyhow!("bad id {id:?}"))?)?
            } else if let Some(v) = id.strip_prefix("vndb:") {
                vndb_get(v)?
            } else {
                bail!("bad id {id:?} (egs:<n> or vndb:v<n>)")
            }
        }
        None => find(title)?,
    };
    in_.fetched = now_rfc3339();
    Ok(in_)
}

fn find(title: &str) -> Result<Info> {
    let keys = search_keys(title);
    for key in &keys {
        match egs_search(key) {
            Ok(c) => {
                if let Some(c) = best(title, &c) {
                    return egs_get(c.id.parse()?);
                }
            }
            Err(e) => {
                eprintln!("erogamescape: {e} (falling back to VNDB)");
                break;
            }
        }
    }
    let mut last = anyhow!("no matching game found for {title:?}");
    for key in &keys {
        match vndb_search(key) {
            Ok(c) => {
                if let Some(c) = best(title, &c) {
                    return vndb_get(&c.id);
                }
            }
            Err(e) => last = anyhow!("vndb: {e}"),
        }
    }
    Err(last)
}

/// Candidates from every reachable source (for choosing an id by hand).
pub fn search(keyword: &str) -> Result<Vec<Candidate>> {
    let mut all = vec![];
    let mut errs = vec![];
    match egs_search(keyword) {
        Ok(c) => all.extend(c),
        Err(e) => errs.push(format!("erogamescape: {e}")),
    }
    match vndb_search(keyword) {
        Ok(c) => all.extend(c),
        Err(e) => errs.push(format!("vndb: {e}")),
    }
    if all.is_empty() && !errs.is_empty() {
        bail!("{}", errs.join("; "));
    }
    Ok(all)
}

/// Downloads an image; returns its bytes and extension.
pub fn download_cover(url: &str) -> Result<(Vec<u8>, &'static str)> {
    let r = agent().get(url).timeout(Duration::from_secs(30)).call()?;
    let ext = match r.content_type() {
        "image/jpeg" => "jpg",
        "image/png" => "png",
        "image/webp" => "webp",
        t => bail!("cover {url}: {t}"),
    };
    let mut b = vec![];
    r.into_reader().take(20 << 20).read_to_end(&mut b)?;
    Ok((b, ext))
}

fn now_rfc3339() -> String {
    rfc3339(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()) as i64)
}

/// Seconds since 1970 as "YYYY-MM-DDThh:mm:ssZ".
fn rfc3339(s: i64) -> String {
    let (days, rem) = (s.div_euclid(86400), s.rem_euclid(86400));
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem / 60 % 60, rem % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        assert_eq!(search_key("Reunion 体験版"), "Reunion");
        assert_eq!(search_key("ＡＢＣ　ｖｅｒ１．２"), "ABC");
        assert_eq!(search_keys("恋×シンアイ彼女 ～体験版～"), vec!["恋×シンアイ彼女".to_string()]);
        assert_eq!(search_keys("ダンジョン・タウン"), vec!["ダンジョン・タウン".to_string(), "ダンジョン".to_string()]);
    }

    #[test]
    fn matching() {
        let c = |t: &str, v| Candidate { source: "erogamescape", id: t.into(), title: t.into(), brand: String::new(), release: String::new(), votes: v };
        let cands = [c("Reunion FD", 900), c("Reunion", 10), c("Re:union", 5)];
        assert_eq!(best("Reunion 体験版", &cands).unwrap().title, "Reunion");
        assert!(best("無関係", &cands).is_none());
    }

    #[test]
    fn table() {
        let rows = parse_table("<table><tr><th>id</th><th>gamename</th></tr><tr><td>1</td><td>A&amp;B &#12354;</td></tr></table>");
        assert_eq!(col(&rows[0], "id"), "1");
        assert_eq!(col(&rows[0], "gamename"), "A&B あ");
        assert_eq!(literal("あ"), "convert_from(decode('e38182','hex'),'UTF8')");
        assert_eq!(literal("it's"), "'it''s'");
    }

    #[test]
    fn date() {
        assert_eq!(now_rfc3339().len(), 20);
        for (s, d) in [
            (0, "1970-01-01T00:00:00Z"),
            (951_782_400, "2000-02-29T00:00:00Z"),
            (951_868_799, "2000-02-29T23:59:59Z"),
            (1_709_164_800, "2024-02-29T00:00:00Z"),
            (1_735_689_599, "2024-12-31T23:59:59Z"),
            (1_793_975_430, "2026-11-06T14:30:30Z"),
            (4_107_542_400, "2100-03-01T00:00:00Z"),
            (-86_400, "1969-12-31T00:00:00Z"),
        ] {
            assert_eq!(rfc3339(s), d, "{s}");
        }
    }

    #[test]
    fn entities() {
        assert_eq!(unescape("&lt;a&gt; &quot;b&quot; &apos;c&apos;&nbsp;&amp;"), "<a> \"b\" 'c' &");
        assert_eq!(unescape("&#x41;&#66;&bogus;&#xzz;"), "AB&bogus;&#xzz;");
        assert_eq!(unescape("plain"), "plain");
    }

    #[test]
    fn unique() {
        let mut l = vec![];
        push_unique(&mut l, "a");
        push_unique(&mut l, "");
        push_unique(&mut l, "b");
        push_unique(&mut l, "a");
        assert_eq!(l, ["a", "b"]);
    }

    #[test]
    fn vndb_names() {
        let dev = |n: &str, o: Option<&str>| VndbDev { name: n.into(), original: o.map(Into::into) };
        let mut v = VndbVn { title: "Romaji".into(), alttitle: Some("原題".into()), ..Default::default() };
        assert_eq!((v.name(), v.brand()), ("原題".to_string(), String::new()));
        v.alttitle = Some(String::new());
        v.developers = vec![dev("Brand", Some("ブランド")), dev("Other", None)];
        assert_eq!((v.name(), v.brand()), ("Romaji".to_string(), "ブランド".to_string()));
        v.alttitle = None;
        v.developers = vec![dev("Brand", Some(""))];
        assert_eq!((v.name(), v.brand()), ("Romaji".to_string(), "Brand".to_string()));
    }

    #[test]
    fn closest_and_most_voted() {
        let c = |t: &str, v| Candidate { source: "vndb", id: t.into(), title: t.into(), brand: String::new(), release: String::new(), votes: v };
        assert_eq!(norm("アート ー！"), "アートー");
        // contained either way: the closest length wins over votes; votes break ties
        let cands = [c("Reunion Fan Disc", 999), c("Reunion FD", 3), c("Re", 999)];
        assert_eq!(best("Reunion", &cands).unwrap().title, "Reunion FD");
        let cands = [c("Reunion A", 3), c("Reunion B", 7)];
        assert_eq!(best("Reunion", &cands).unwrap().title, "Reunion B");
        let cands = [c("Reunion B", 7), c("Reunion A", 7)];
        assert_eq!(best("Reunion", &cands).unwrap().title, "Reunion B", "first of equals");
        let cands = [c("Reunion Extra Long", 999), c("Reunion", 0)];
        assert_eq!(best("Reunion", &cands).unwrap().title, "Reunion");
        assert_eq!(best("Reunion Fan Disc", &[c("Reunion", 1)]).unwrap().title, "Reunion");
        assert!(best("", &cands).is_none() && best("x", &[c("", 5)]).is_none());
    }
}
