//! Which ErogameScape (批評空間) game a package is, from its names (title, folder, program),
//! against a list of every game the server keeps (id, name): the way ryoha000/launcherg
//! identifies games (public domain): names folded the same way on both sides, an exact match
//! first, else the closest by an insertion/deletion distance over the longer length (score over
//! 0.8 to count), a whole game name inside a longer folder name counting too.
//!
//! No network here: meta.rs fetches the list, library.rs keeps it.

use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

/// A score over this names the game.
pub const THRESHOLD: f32 = 0.8;
/// A game name inside the query (or the query inside it) counts only from this many characters.
const PARTIAL_MIN: usize = 5;

/// Edition and download words that package and folder names carry and database names do not.
static EDITION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)((ダウンロード|web|ウェブ|パッケージ|dl)\s*)?(体験|お試し読み|お試し|プレイアブル)\s*版|(ダウンロード|web|ウェブ|パッケージ|dl|通常|初回限定|限定|特装|豪華|廉価)\s*版|を起動|trial(\s*version|\s*edition)?|\bdemo\b|taiken|(^|[^a-z])ver\.?\s*[0-9][0-9.]*|\bv[0-9]+(\.[0-9]+)+|\s[0-9]+(\.[0-9]+)+\s*$|[-－‐_\s]+(web|dl)\s*$").unwrap()
});

/// Width and case folded (NFKC), edition words out, then only letters and digits (and ー, which
/// is part of words) left: "セレクトオブリージュ DL版" and "セレクトオブリージュ" are the same.
pub fn normalize(s: &str) -> Vec<char> {
    let f: String = s.nfkc().collect::<String>().to_lowercase();
    EDITION.replace_all(&f, " ").chars().filter(|c| c.is_alphanumeric() || *c == 'ー').collect()
}

/// Longest common subsequence length (two rows).
fn lcs(a: &[char], b: &[char]) -> usize {
    let mut prev = vec![0usize; b.len() + 1];
    let mut cur = vec![0usize; b.len() + 1];
    for &x in a {
        for (j, &y) in b.iter().enumerate() {
            cur[j + 1] = if x == y { prev[j] + 1 } else { cur[j].max(prev[j + 1]) };
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// The runs of digits in a name, leading zeros left out.
fn numbers(s: &[char]) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    for &c in s.iter().chain(std::iter::once(&' ')) {
        if c.is_ascii_digit() {
            cur.push(c);
        } else if !cur.is_empty() {
            let n = cur.trim_start_matches('0');
            out.push(if n.is_empty() { "0".into() } else { n.to_string() });
            cur.clear();
        }
    }
    out
}

/// How alike a query and a game's name are (both normalized), 0 to 1 (1: the same).
pub fn score(q: &[char], g: &[char]) -> f32 {
    if q.is_empty() || g.is_empty() {
        return 0.0;
    }
    if q == g {
        return 1.0;
    }
    let long = q.len().max(g.len());
    let common = lcs(q, g);
    // insertions and deletions turning one into the other, over the longer length
    let d = (q.len() + g.len() - 2 * common) as f32;
    let mut s = (1.0 - d / long as f32).max(0.0);
    // a whole name inside the other: the game's at the start of a longer folder name
    // ("深淵のラビリントス ハーレムコンテンツ"), or the query in a longer game name, best at its
    // start ("さくら、もゆ。" for "さくら、もゆ。 -as the Night's, Reincarnation-"), less anywhere
    // else ("時間停止と不可避な運命" for "出会って5分は俺のもの! 時間停止と不可避な運命")
    let ratio = q.len().min(g.len()) as f32 / long as f32;
    let mut within = false;
    if (g.len() >= PARTIAL_MIN && q.starts_with(g)) || (q.len() >= PARTIAL_MIN && g.starts_with(q)) {
        s = s.max(THRESHOLD + 0.2 * ratio);
    } else if q.len() >= PARTIAL_MIN && g.windows(q.len()).any(|w| w == q) {
        s = s.max(THRESHOLD + 0.1 * ratio);
        within = true;
    }
    // numbers name the sequel: WHITE ALBUM2 is not WHITE ALBUM, D.C.5 not D.C.4. Every number of
    // the query in the game's name, and the game's first number in the query (unless the query
    // is a part of the name after it)
    let (qn, gn) = (numbers(q), numbers(g));
    if !qn.iter().all(|n| gn.contains(n)) || (!within && gn.first().is_some_and(|n| !qn.contains(n))) {
        s = s.min(THRESHOLD - 0.01);
    }
    s.clamp(0.0, 0.9999)
}

struct Game {
    id: u32,
    name: String,
    norm: Vec<char>,
    /// ErogameScape's vote count (0 when the list has none)
    votes: u32,
}

/// Every game, ready to match.
pub struct Index {
    games: Vec<Game>,
}

/// A match: the game's id and name, and how alike.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Hit {
    pub id: u32,
    pub name: String,
    pub score: f32,
    pub votes: u32,
}

impl Index {
    /// From (id, name, votes).
    pub fn new(list: impl IntoIterator<Item = (u32, String, u32)>) -> Self {
        let mut games: Vec<Game> = list
            .into_iter()
            .map(|(id, name, votes)| Game { id, norm: normalize(&name), name, votes })
            .filter(|g| !g.norm.is_empty())
            .collect();
        games.sort_by_key(|g| g.id);
        Self { games }
    }

    /// The `n` games most like any of `queries` (each game's best), most alike first; equal
    /// scores go to the older entry (the original release before its re-releases).
    pub fn candidates(&self, queries: &[String], n: usize) -> Vec<Hit> {
        let qs: Vec<Vec<char>> = queries.iter().map(|q| normalize(q)).filter(|q| q.len() >= 2).collect();
        if qs.is_empty() {
            return vec![];
        }
        let bit = |c: char| c as usize % 256;
        let masks: Vec<[bool; 256]> = qs
            .iter()
            .map(|q| {
                let mut m = [false; 256];
                for &c in q {
                    m[bit(c)] = true;
                }
                m
            })
            .collect();
        let mut hits: Vec<(f32, usize)> = self
            .games
            .iter()
            .enumerate()
            .filter_map(|(i, g)| {
                let s = qs
                    .iter()
                    .zip(&masks)
                    .filter(|(q, m)| {
                        // (the characters of the game's name the query may share bound what they
                        // have in common: too few for a score over 0.3, no need to compare them)
                        let shared = g.norm.iter().filter(|c| m[bit(**c)]).count().min(q.len());
                        let long = q.len().max(g.norm.len()) as f32;
                        let short = q.len().min(g.norm.len());
                        2.0 * shared as f32 > q.len() as f32 + g.norm.len() as f32 - 0.7 * long || (short >= PARTIAL_MIN && shared >= short)
                    })
                    .map(|(q, _)| score(q, &g.norm))
                    .fold(0.0f32, f32::max);
                (s > 0.3).then_some((s, i))
            })
            .collect();
        hits.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        hits.truncate(n);
        hits.into_iter().map(|(s, i)| Hit { id: self.games[i].id, name: self.games[i].name.clone(), score: s, votes: self.games[i].votes }).collect()
    }

    /// The games a typed name finds: those whose name has it in it (most voted first), then the
    /// most alike of the rest.
    pub fn search(&self, q: &str, n: usize) -> Vec<Hit> {
        let k = normalize(q);
        if k.is_empty() {
            return vec![];
        }
        let mut hits: Vec<Hit> = self
            .games
            .iter()
            .filter(|g| g.norm.windows(k.len()).any(|w| w == k.as_slice()))
            .map(|g| Hit { id: g.id, name: g.name.clone(), score: score(&k, &g.norm), votes: g.votes })
            .collect();
        hits.sort_by(|a, b| b.votes.cmp(&a.votes).then(b.score.total_cmp(&a.score)).then(a.id.cmp(&b.id)));
        hits.truncate(n);
        for h in self.candidates(&[q.to_string()], n) {
            if hits.len() >= n {
                break;
            }
            if !hits.iter().any(|x| x.id == h.id) {
                hits.push(h);
            }
        }
        hits
    }

    /// The game `queries` name, when one is alike enough. Short of an exact match, of the ones
    /// nearly as alike as the closest the most voted (サクラノ詩 -櫻の森の上を舞う- rather than its
    /// fan disc サクラノ詩 春ノ雪, which is as close to "サクラノ詩").
    pub fn best(&self, queries: &[String]) -> Option<Hit> {
        let hits = self.candidates(queries, 16);
        let top = hits.first()?.score;
        if top <= THRESHOLD {
            return None;
        }
        if top >= 1.0 {
            return hits.into_iter().next();
        }
        hits.into_iter().filter(|h| h.score > THRESHOLD && h.score >= top - 0.1).reduce(|a, b| if b.votes > a.votes { b } else { a })
    }
}

/// Program names that say nothing about the game (engines, launchers, installers).
const GENERIC: &[&str] = &[
    "game", "start", "startup", "launcher", "launch", "play", "main", "setup", "install", "uninstall", "unins000", "config", "configure",
    "setting", "settings", "siglusengine", "reallive", "bgi", "krkr", "tvpwin32", "nscripter", "nscr", "arc", "malie", "cmvs32", "cmvs64",
    "advhd", "system", "autorun", "menu", "startmenu", "supporttools", "exe", "program", "data", "files", "programfiles", "x86", "x64",
    "win32", "win64", "windows", "bin", "app", "vnvm", "trial",
];

/// What to match a game by: its title, the folders of its program (from the innermost) and the
/// program's name, generic ones left out.
pub fn queries(title: &str, run: &str) -> Vec<String> {
    let mut out = vec![title.to_string()];
    let parts: Vec<&str> = run.split(['\\', '/']).filter(|p| !p.is_empty() && !p.ends_with(':')).collect();
    if let Some((exe, dirs)) = parts.split_last() {
        let stem = exe.rsplit_once('.').map_or(*exe, |x| x.0);
        for p in dirs.iter().rev().chain(std::iter::once(&stem)) {
            let n: String = normalize(p).into_iter().collect();
            if n.chars().count() >= 2 && !GENERIC.contains(&n.as_str()) && !out.iter().any(|o| o == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn index() -> Index {
        Index::new(
            [
                (100, "夜明け前より瑠璃色な"),
                (101, "夜明け前より瑠璃色な -Moonlight Cradle-"),
                (200, "セレクトオブリージュ"),
                (300, "深淵のラビリントス"),
                (400, "LOOPERS"),
                (401, "LOOPERS+"),
                (500, "ATRI -My Dear Moments-"),
                (600, "千恋＊万花"),
                (700, "艶嬢学園2 ー熾天使たちの花園ー"),
                (800, "ピュアガール"),
            ]
            .map(|(i, n)| (i, n.to_string(), 0)),
        )
    }

    fn best(q: &[&str]) -> Option<u32> {
        index().best(&q.iter().map(|s| s.to_string()).collect::<Vec<_>>()).map(|h| h.id)
    }

    #[test]
    fn folded() {
        assert_eq!(normalize("ＬＯＯＰＥＲＳ"), normalize("loopers"));
        assert_eq!(normalize("セレクトオブリージュ DL版"), normalize("セレクトオブリージュ"));
        assert_eq!(normalize("極限痴漢特異点 Ver1.01"), normalize("極限痴漢特異点"));
        assert_eq!(normalize("ゲーム 体験版"), normalize("ゲーム"));
        assert_eq!(normalize("千恋＊万花"), "千恋万花".chars().collect::<Vec<_>>());
        assert_eq!(normalize("ゲームを起動"), normalize("ゲーム"));
        assert!(normalize("・〜！？").is_empty());
    }

    #[test]
    fn names() {
        assert_eq!(best(&["夜明け前より瑠璃色な"]), Some(100));
        assert_eq!(best(&["夜明け前より瑠璃色な - Moonlight Cradle"]), Some(101));
        assert_eq!(best(&["セレクトオブリージュ DL版"]), Some(200));
        assert_eq!(best(&["深淵のラビリントス ハーレムコンテンツ"]), Some(300), "a whole name inside");
        assert_eq!(best(&["ＬＯＯＰＥＲＳ"]), Some(400));
        assert_eq!(best(&["千恋万花"]), Some(600));
        assert_eq!(best(&["艶嬢学園２ ー熾天使たちの花園ー 1.01"]), Some(700));
        assert_eq!(best(&["DC2BS", "ピュアガール"]), Some(800), "the best of the queries");
        assert_eq!(best(&["まったく別のゲーム"]), None);
        assert_eq!(best(&["ATRI"]), None, "too short to be inside");
        assert_eq!(best(&[""]), None);
    }

    /// Nearly as close and more voted wins over the closest, an exact name over votes.
    #[test]
    fn votes() {
        let idx = Index::new([
            (1, "サクラノ詩 -櫻の森の上を舞う-".to_string(), 5000),
            (2, "サクラノ詩 春ノ雪".to_string(), 30),
            (3, "Rewrite".to_string(), 3000),
            (4, "Rewrite+".to_string(), 10),
        ]);
        let q = |s: &str| idx.best(&[s.to_string()]).map(|h| h.id);
        assert_eq!(q("サクラノ詩"), Some(1));
        assert_eq!(q("サクラノ詩 春ノ雪"), Some(2), "exact");
        assert_eq!(idx.candidates(&["サクラノ詩".into()], 2)[0].id, 2, "the closest first in the list");
        let found: Vec<u32> = idx.search("サクラ", 5).iter().map(|h| h.id).collect();
        assert_eq!(found, [1, 2], "a part of the name, most voted first");
        let found: Vec<u32> = idx.search("Rewrit", 5).iter().map(|h| h.id).collect();
        assert_eq!(found[..2], [3, 4]);
        assert!(idx.search("・", 5).is_empty());
    }

    #[test]
    fn program_paths() {
        assert_eq!(queries("anemoi", r"Z:\vnvm\game\Start.exe"), ["anemoi"]);
        assert_eq!(
            queries("t", r"C:\Astronauts\深淵のラビリントス\LabyrinthosoftheAbyss.exe"),
            ["t", "深淵のラビリントス", "Astronauts", "LabyrinthosoftheAbyss"]
        );
        assert_eq!(queries("Key", "C:/Key/SiglusEngine.exe"), ["Key"]);
    }

    proptest! {
        /// A name always finds itself (or an entry with the same name), whatever is put around
        /// it as edition words, width and case.
        #[test]
        fn finds_itself(k in 0usize..10, upper in any::<bool>(), suffix in prop_oneof![Just(""), Just(" DL版"), Just(" 体験版"), Just(" ver1.2"), Just("　")]) {
            let idx = index();
            let g = &idx.games[k];
            let mut q: String = if upper { g.name.to_uppercase() } else { g.name.clone() };
            q.push_str(suffix);
            let hit = idx.best(&[q.clone()]).unwrap();
            prop_assert_eq!(normalize(&hit.name), g.norm.clone(), "{} -> {:?}", q, hit);
        }

        /// The quick skip in `candidates` drops nothing a full comparison would keep.
        #[test]
        fn skip_loses_nothing(names in proptest::collection::vec("[あいうかきくアイウab12 ]{1,10}", 1..30), q in "[あいうかきくアイウab12 ]{1,10}") {
            let idx = Index::new(names.iter().enumerate().map(|(i, n)| (i as u32, n.clone(), 0)));
            let qn = normalize(&q);
            prop_assume!(qn.len() >= 2);
            let mut all: Vec<(u32, f32)> = idx.games.iter().map(|g| (g.id, score(&qn, &g.norm))).filter(|x| x.1 > 0.3).collect();
            all.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let got: Vec<(u32, f32)> = idx.candidates(&[q.clone()], 100).into_iter().map(|h| (h.id, h.score)).collect();
            prop_assert_eq!(got, all);
        }

        /// Scores: in 0..=1, symmetric, 1 only for the same name.
        #[test]
        fn scores(a in "[あ-んア-ンa-z0-9]{0,12}", b in "[あ-んア-ンa-z0-9]{0,12}") {
            let (x, y) = (normalize(&a), normalize(&b));
            let s = score(&x, &y);
            prop_assert!((0.0..=1.0).contains(&s));
            prop_assert_eq!(s, score(&y, &x));
            prop_assert_eq!(s == 1.0, !x.is_empty() && x == y);
        }
    }
}

#[cfg(test)]
mod corpus {
    use super::*;
    use std::io::Read;

    /// The game list launcherg ships (script/all_games.json, as id\tname; public domain).
    pub fn games() -> Index {
        let mut s = String::new();
        flate2::read::GzDecoder::new(&include_bytes!("../../../testdata/egs-games.tsv.gz")[..]).read_to_string(&mut s).unwrap();
        Index::new(s.lines().filter_map(|l| l.split_once('\t')).map(|(i, n)| (i.parse().unwrap(), n.to_string(), 0)))
    }

    /// The user's library (testdata/egs-match-corpus.tsv): every game checked by hand found
    /// again, nothing found where none should be, the known wrong ones as they are (a change
    /// fixing one updates the table).
    #[test]
    fn library() {
        let idx = games();
        let (mut right, mut none, mut wrong) = (0, 0, 0);
        let mut bad = vec![];
        for l in include_str!("../../../testdata/egs-match-corpus.tsv").lines().filter(|l| !l.starts_with('#')) {
            let mut c = l.split('\t');
            let (folder, exe, want) = (c.next().unwrap(), c.next().unwrap(), c.next().unwrap());
            let title = folder.rsplit('\\').next().unwrap();
            let got = idx.best(&queries(title, &format!("Z:\\vnvm\\game\\{exe}"))).map(|h| h.id.to_string());
            let ok = match want {
                "-" => got.is_none(),
                w if w.starts_with('x') => got.as_deref() == Some(&w[1..]),
                w => got.as_deref() == Some(w),
            };
            if !ok {
                bad.push(format!("{folder} / {exe}: want {want}, got {got:?}"));
            }
            match want {
                "-" => none += 1,
                w if w.starts_with('x') => wrong += 1,
                _ => right += 1,
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
        assert!(right >= 110 && wrong <= 8, "{right} right, {none} none, {wrong} wrong");
    }

    /// (prints what a folder list matches: EGS_CORPUS_IN=<folder\texe lines>)
    #[test]
    #[ignore]
    fn print() {
        let idx = games();
        let input = std::fs::read_to_string(std::env::var("EGS_CORPUS_IN").unwrap()).unwrap();
        for l in input.lines() {
            let (folder, exe) = l.split_once('\t').unwrap();
            let title = folder.rsplit('\\').next().unwrap();
            let q = queries(title, &format!("Z:\\vnvm\\game\\{exe}"));
            let c = idx.candidates(&q, 3);
            let top: Vec<String> = c.iter().map(|h| format!("{}:{}:{:.2}", h.id, h.name, h.score)).collect();
            println!("{folder}\t{exe}\t{}", top.join(" | "));
        }
    }
}
