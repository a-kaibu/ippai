//! How well the hints (hints.rs) foresee real games' reads: ippai serve's read-order lists
//! (`<jit>/game.<id>.chunks`: the store chunks runs of a game read first, in that order) replayed
//! against the games' trees, for the hints as they are and for variants of them. Per engine
//! (the game entry's `engine`): reads, how many of them waited for the network (none hinted
//! them before), and the bytes hinted that the list never reads (fetched for nothing).
//!
//!   HINTS_STORE=<store> HINTS_JIT=<jit> cargo test -p kura --lib hints_eval -- --ignored --nocapture
//!
//! A list is the order of first reads, not their times: a chunk hinted just before its read
//! still waits a little. The misses are a floor, the waste is as it would be.

use super::{chunks_with, kind, policy, siblings_with, Kind, Policy, WHOLE_MAX};
use std::collections::{BTreeMap, HashMap, HashSet};

struct File {
    path: String,
    size: u64,
    chunks: Vec<(String, u64)>,
}

/// A tree list's files (trees/<name>.tsv: "f" lines, path in column 4, size 5, chunks 6).
fn tree(text: &str) -> Vec<File> {
    let unesc = |s: &str| s.replace("\\t", "\t").replace("\\n", "\n").replace("\\\\", "\\");
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() != 6 || f[0] != "f" {
                return None;
            }
            let chunks = f[5].split(',').filter(|c| !c.is_empty()).filter_map(|c| c.split_once(':').map(|(h, n)| (h.to_string(), n.parse().unwrap_or(0)))).collect();
            Some(File { path: unesc(f[3]), size: f[4].parse().ok()?, chunks })
        })
        .collect()
}

/// A JSON string field's value, the simple way (games/<id>.json is ippai's own).
fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let at = json.find(&format!("\"{key}\":\""))? + key.len() + 4;
    json[at..].find('"').map(|e| &json[at..at + e])
}

/// The trees a game mounts (`"mounts":["/x=tree",...]`).
fn mounts(json: &str) -> Vec<String> {
    let Some(at) = json.find("\"mounts\":[") else { return Vec::new() };
    let rest = &json[at + 10..];
    let end = rest.find(']').unwrap_or(0);
    rest[..end].split(',').filter_map(|m| m.trim().trim_matches('"').split_once('=').map(|(_, t)| t.to_string())).filter(|t| !t.is_empty()).collect()
}

/// A variant of the hints: the policy of a file, and the whole-archive limit.
struct Variant {
    name: &'static str,
    pol: fn(&str) -> Policy,
    whole_max: u64,
}

#[derive(Default, Clone, Copy)]
struct Score {
    reads: u64,
    misses: u64,
    hinted: u64,
    wasted: u64,
}

/// Replays `seq` (file, chunk) against `files` with variant `v`.
fn replay(files: &[File], dirs: &HashMap<String, Vec<usize>>, seq: &[(usize, usize)], v: &Variant) -> Score {
    let mut have: HashSet<(usize, usize)> = HashSet::new();
    let mut touched: HashSet<usize> = HashSet::new();
    let mut hinted: HashSet<(usize, usize)> = HashSet::new();
    let mut s = Score::default();
    for &(f, c) in seq {
        s.reads += 1;
        let first = touched.insert(f);
        if have.contains(&(f, c)) {
            continue;
        }
        s.misses += 1;
        have.insert((f, c));
        let file = &files[f];
        let name = file.path.rsplit('/').next().unwrap_or(&file.path);
        let pol = (v.pol)(name);
        let mut add = |k: (usize, usize), have: &mut HashSet<(usize, usize)>| {
            if have.insert(k) {
                hinted.insert(k);
            }
        };
        for k in chunks_with(name, file.size, file.chunks.len(), c, first, pol, v.whole_max) {
            add((f, k), &mut have);
        }
        if first {
            let dir = file.path.rsplit_once('/').map_or("", |d| d.0);
            if let Some(list) = dirs.get(dir) {
                let names: Vec<(&str, u64)> = list.iter().map(|&i| (files[i].path.rsplit('/').next().unwrap(), files[i].size)).collect();
                for k in siblings_with(&names, name, pol) {
                    let g = list[k];
                    for j in 0..files[g].chunks.len() {
                        add((g, j), &mut have);
                    }
                }
            }
        }
    }
    let read: HashSet<(usize, usize)> = seq.iter().copied().collect();
    for &(f, c) in &hinted {
        let n = files[f].chunks[c].1;
        s.hinted += n;
        if !read.contains(&(f, c)) {
            s.wasted += n;
        }
    }
    s
}

/// `p` with its siblings times `s` and its chunks times `c`.
fn scale(p: Policy, s: f64, c: f64) -> Policy {
    Policy { siblings: (p.siblings as f64 * s).round() as usize, chunks: (p.chunks as f64 * c).round() as usize }
}

#[test]
#[ignore]
fn hints_eval() {
    let (Ok(store), Ok(jit)) = (std::env::var("HINTS_STORE"), std::env::var("HINTS_JIT")) else {
        eprintln!("set HINTS_STORE and HINTS_JIT");
        return;
    };
    let variants = [
        Variant { name: "as is", pol: policy, whole_max: WHOLE_MAX },
        Variant { name: "none", pol: |_| Policy { siblings: 0, chunks: 0 }, whole_max: 0 },
        Variant { name: "siblings x0", pol: |n| scale(policy(n), 0.0, 1.0), whole_max: WHOLE_MAX },
        Variant { name: "siblings x2", pol: |n| scale(policy(n), 2.0, 1.0), whole_max: WHOLE_MAX },
        Variant { name: "siblings x4", pol: |n| scale(policy(n), 4.0, 1.0), whole_max: WHOLE_MAX },
        Variant { name: "chunks x0", pol: |n| scale(policy(n), 1.0, 0.0), whole_max: WHOLE_MAX },
        Variant { name: "chunks x2", pol: |n| scale(policy(n), 1.0, 2.0), whole_max: WHOLE_MAX },
        Variant { name: "chunks x4", pol: |n| scale(policy(n), 1.0, 4.0), whole_max: WHOLE_MAX },
        Variant { name: "whole 64 MiB", pol: policy, whole_max: 64 << 20 },
        Variant { name: "whole none", pol: policy, whole_max: 0 },
    ];
    // engine -> variant -> score; and by kind of what was missed with the hints as they are
    let mut by: BTreeMap<String, Vec<Score>> = BTreeMap::new();
    let mut missed_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let mut games: Vec<_> = std::fs::read_dir(format!("{store}/games")).unwrap().filter_map(|e| e.ok()).collect();
    games.sort_by_key(|e| e.file_name());
    for e in games {
        let Some(id) = e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(String::from) else { continue };
        let Ok(list) = std::fs::read(format!("{jit}/game.{id}.chunks")) else { continue };
        let json = std::fs::read_to_string(e.path()).unwrap_or_default();
        let engine = field(&json, "engine").unwrap_or("?").to_string();
        let mut files = Vec::new();
        for t in mounts(&json) {
            if let Ok(text) = std::fs::read_to_string(format!("{store}/trees/{t}.tsv")) {
                files.extend(tree(&text));
            }
        }
        let mut at: HashMap<&str, (usize, usize)> = HashMap::new();
        let mut dirs: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, f) in files.iter().enumerate() {
            for (j, (h, _)) in f.chunks.iter().enumerate() {
                at.entry(h.as_str()).or_insert((i, j));
            }
            dirs.entry(f.path.rsplit_once('/').map_or("", |d| d.0).to_string()).or_default().push(i);
        }
        // (a folder's files in the order kura lists them: by name)
        for v in dirs.values_mut() {
            v.sort_by(|&a, &b| files[a].path.rsplit('/').next().cmp(&files[b].path.rsplit('/').next()));
        }
        let hex = |c: &[u8]| c.iter().map(|b| format!("{b:02x}")).collect::<String>();
        let seq: Vec<(usize, usize)> = list.chunks_exact(16).filter_map(|c| at.get(hex(c).as_str()).copied()).collect();
        if seq.is_empty() {
            continue;
        }
        let scores: Vec<Score> = variants.iter().map(|v| replay(&files, &dirs, &seq, v)).collect();
        eprintln!("{id} ({engine}): {} reads, misses {} -> {} as is, {} MiB hinted for nothing", seq.len(), scores[1].misses, scores[0].misses, scores[0].wasted >> 20);
        let e = by.entry(engine.clone()).or_insert_with(|| vec![Score::default(); variants.len()]);
        for (a, b) in e.iter_mut().zip(&scores) {
            a.reads += b.reads;
            a.misses += b.misses;
            a.hinted += b.hinted;
            a.wasted += b.wasted;
        }
        // (what still waited, by kind: replay again noting them)
        let mut have = HashSet::new();
        let v = &variants[0];
        let mut touched = HashSet::new();
        for &(f, c) in &seq {
            let first = touched.insert(f);
            if have.insert((f, c)) {
                let name = files[f].path.rsplit('/').next().unwrap_or("");
                let k = match kind(name) {
                    Kind::Other => format!("other .{}", name.rsplit_once('.').map_or("", |x| x.1).to_ascii_lowercase()),
                    k => format!("{k:?}"),
                };
                *missed_kind.entry(engine.clone()).or_default().entry(k).or_default() += 1;
                for k in chunks_with(name, files[f].size, files[f].chunks.len(), c, first, (v.pol)(name), v.whole_max) {
                    have.insert((f, k));
                }
                if first {
                    let dir = files[f].path.rsplit_once('/').map_or("", |d| d.0);
                    if let Some(list) = dirs.get(dir) {
                        let names: Vec<(&str, u64)> = list.iter().map(|&i| (files[i].path.rsplit('/').next().unwrap(), files[i].size)).collect();
                        for k in siblings_with(&names, name, (v.pol)(name)) {
                            for j in 0..files[list[k]].chunks.len() {
                                have.insert((list[k], j));
                            }
                        }
                    }
                }
            }
        }
    }
    println!("engine\tvariant\treads\tmisses\tmiss%\thinted MiB\twasted MiB");
    for (engine, scores) in &by {
        for (v, s) in variants.iter().zip(scores) {
            println!("{engine}\t{}\t{}\t{}\t{:.1}\t{}\t{}", v.name, s.reads, s.misses, 100.0 * s.misses as f64 / s.reads.max(1) as f64, s.hinted >> 20, s.wasted >> 20);
        }
        println!("{engine}\tstill waited by kind\t{:?}", missed_kind.get(engine));
    }
}

#[test]
fn replay_counts_what_hints_foresaw() {
    // (a folder of voices read in order: the first waits, the next 12 were hinted; a movie alone)
    let mut files: Vec<File> = (0..20).map(|i| File { path: format!("voice/v{i:03}.ogg"), size: 1000, chunks: vec![(format!("{i:032x}"), 1000)] }).collect();
    files.push(File { path: "op.mpg".into(), size: 3000, chunks: (0..3).map(|i| (format!("m{i}"), 1000)).collect() });
    let mut dirs: HashMap<String, Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        dirs.entry(f.path.rsplit_once('/').map_or("", |d| d.0).to_string()).or_default().push(i);
    }
    let seq: Vec<(usize, usize)> = (0..20).map(|i| (i, 0)).chain((0..3).map(|c| (20, c))).collect();
    let as_is = Variant { name: "as is", pol: policy, whole_max: WHOLE_MAX };
    let none = Variant { name: "none", pol: |_| Policy { siblings: 0, chunks: 0 }, whole_max: 0 };
    let s = replay(&files, &dirs, &seq, &as_is);
    // (v000 waits and hints 001..012; v013 waits and hints 014..019; the movie's 3 chunks wait)
    assert_eq!((s.reads, s.misses, s.wasted), (23, 5, 0));
    assert_eq!(replay(&files, &dirs, &seq, &none).misses, 23);
    // (read only the first: 12 hinted for nothing)
    let s = replay(&files, &dirs, &[(0, 0)], &as_is);
    assert_eq!((s.misses, s.wasted), (1, 12_000));
    assert_eq!(mounts(r#"{"mounts":["/ippai/game=g1","/x="],"engine":"KiriKiri"}"#), vec!["g1"]);
    assert_eq!(field(r#"{"id":"a","engine":"KiriKiri Z"}"#, "engine"), Some("KiriKiri Z"));
    let t = tree("v1\t\t\nf\t0\t0\tvoice/a\\tb.ogg\t3\taa:1,bb:2\nd\t0\t0\tvoice\t0\t\n");
    assert_eq!((t.len(), t[0].path.as_str(), t[0].chunks.len()), (1, "voice/a\tb.ogg", 2));
}
