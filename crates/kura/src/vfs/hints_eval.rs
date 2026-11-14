//! How well the hints (hints.rs) foresee real games' reads: masu serve's read-order lists
//! (`<jit>/game.<id>.chunks`: the store chunks runs of a game read first, in that order) replayed
//! against the games' trees, for the hints as they are and for variants of them. Per engine
//! (the game entry's `engine`): reads, how many of them waited for the network (none hinted
//! them before), and the bytes hinted that the list never reads (fetched for nothing).
//!
//!   HINTS_STORE=<store> HINTS_JIT=<jit> cargo test -p kura --lib hints_eval -- --ignored --nocapture
//!
//! With HINTS_FORESEE=<dir> (`masu foresee --out <dir>`: each tree's table and archive members)
//! also what the static foresight (vfs.rs `predicted` with a table) adds: each read reaches the
//! members (or the loose file) its chunk holds, and those hint what the table says they name or
//! what follows them in their scenes, as kura does. HINTS_DEBUG=1: a line for each read (what
//! it reached, what that named, how much of it is a file).
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

/// `json` without the whitespace between its tokens (masu writes games/<id>.json pretty).
fn compact(json: &str) -> String {
    let (mut out, mut quoted, mut escaped) = (String::with_capacity(json.len()), false, false);
    for c in json.chars() {
        if quoted {
            out.push(c);
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => quoted = false,
                _ => {}
            }
        } else if !c.is_whitespace() {
            quoted = c == '"';
            out.push(c);
        }
    }
    out
}

/// A JSON string field's value, the simple way (games/<id>.json is masu's own, compacted).
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

/// A variant of the hints: the policy of a file, and the whole-archive limit; and whether the
/// static foresight hints too.
struct Variant {
    name: &'static str,
    pol: fn(&str) -> Policy,
    whole_max: u64,
    foresee: bool,
    /// What every start reads, at the first read (hints::boot_chunks).
    boot: bool,
}

/// What a game's trees' tables foresee (`masu foresee`), over its files.
#[derive(Default)]
struct Foresight {
    table: HashMap<String, Vec<String>>,
    stems: HashMap<String, String>,
    places: HashMap<String, Vec<(String, usize)>>,
    /// Archives' members (name, start, bytes), by file, sorted by start.
    members: HashMap<usize, Vec<(String, u64, u64)>>,
    /// Names (and stems) -> (file, start, bytes): a loose file whole, or a member.
    names: HashMap<String, Vec<(usize, u64, u64)>>,
}

impl Foresight {
    /// From the tables' texts and members' lines (`<tree>.tsv`, `<tree>.members`).
    fn new(files: &[File], tables: &[String], members: &[String]) -> Foresight {
        let mut f = Foresight::default();
        for t in tables {
            for (k, v) in foresee::parse_table(t) {
                let e = f.table.entry(k).or_default();
                for n in v {
                    if !e.contains(&n) {
                        e.push(n);
                    }
                }
            }
        }
        f.stems = foresee::table_stems(&f.table);
        f.places = foresee::table_places(&f.table, crate::vfs::PLACES_COMMON);
        let by_path: HashMap<String, usize> = files.iter().enumerate().map(|(i, x)| (x.path.to_lowercase(), i)).collect();
        let add = |names: &mut HashMap<String, Vec<(usize, u64, u64)>>, name: &str, t: (usize, u64, u64)| {
            let (full, stem) = foresee::keys(name.rsplit(['/', '\\']).next().unwrap_or(name));
            for k in std::iter::once(full).chain(stem) {
                let v = names.entry(k).or_default();
                if v.len() < 8 {
                    v.push(t);
                }
            }
        };
        for (i, x) in files.iter().enumerate() {
            add(&mut f.names, &x.path, (i, 0, x.size));
        }
        for text in members {
            for l in text.lines() {
                let c: Vec<&str> = l.split('\t').collect();
                let [path, name, start, n] = c[..] else { continue };
                let (Some(&i), Ok(start), Ok(n)) = (by_path.get(path), start.parse::<u64>(), n.parse::<u64>()) else { continue };
                f.members.entry(i).or_default().push((name.to_string(), start, n));
                add(&mut f.names, name, (i, start, n));
            }
        }
        for v in f.members.values_mut() {
            v.sort_by_key(|m| m.1);
        }
        f
    }

    /// The chunks (file, chunk) a first read of `files[f]`'s chunk `c` foresees, as vfs.rs
    /// `predicted` would; `scanned`: members (file, start) looked at already.
    fn hints(&self, files: &[File], f: usize, c: usize, scanned: &mut HashSet<(usize, u64)>) -> Vec<(usize, usize)> {
        let file = &files[f];
        let start: u64 = file.chunks[..c].iter().map(|x| x.1).sum();
        let end = start + file.chunks[c].1;
        let mut lists = Vec::new();
        let look = |name: &str| -> Vec<String> {
            let k = foresee::script_key(name);
            if self.table.contains_key(&k) {
                foresee::foreseen(&self.table, &self.stems, &k, crate::vfs::FORESEE_DEPTH, foresee::MENTIONS_MAX)
            } else {
                foresee::following(&self.table, &self.places, &k, crate::vfs::FOLLOWING)
            }
        };
        let debug = std::env::var_os("HINTS_DEBUG").is_some();
        let mut reached = Vec::new();
        match self.members.get(&f) {
            Some(ms) => {
                for m in ms.iter().filter(|m| m.1 < end && start < m.1 + m.2.max(1)) {
                    if scanned.insert((f, m.1)) {
                        lists.push(look(&m.0));
                        reached.push(m.0.clone());
                    }
                }
            }
            None => {
                if scanned.insert((f, 0)) {
                    let name = file.path.rsplit('/').next().unwrap_or(&file.path);
                    lists.push(look(name));
                    reached.push(name.to_string());
                }
            }
        }
        if debug {
            let named: usize = lists.iter().map(Vec::len).sum();
            let known = lists.iter().flatten().filter(|n| {
                let (full, stem) = foresee::keys(n);
                self.names.contains_key(&full) || stem.is_some_and(|s| self.names.contains_key(&s))
            });
            let scripts = reached.iter().filter(|n| self.table.contains_key(&foresee::script_key(n))).count();
            eprintln!(
                "  read {} chunk {c} [{start}..{end}): archive {}, reached {} ({scripts} in the table; e.g. {:?}), named {named}, of them files {}",
                file.path,
                self.members.contains_key(&f),
                reached.len(),
                reached.iter().take(3).collect::<Vec<_>>(),
                known.count()
            );
        }
        // (the targets' chunks, in the order named: SIBLING_MAX each, PREDICT_BYTES in all)
        let span = |g: usize, a: u64, n: u64| {
            let mut at = 0u64;
            let mut v = Vec::new();
            for (j, x) in files[g].chunks.iter().enumerate() {
                if at < a + n.max(1) && a < at + x.1 {
                    v.push((g, j));
                }
                at += x.1;
            }
            v
        };
        let (mut out, mut bytes) = (Vec::new(), 0u64);
        for names in lists {
            for n in names {
                let (full, stem) = foresee::keys(&n);
                let Some(ts) = self.names.get(&full).or_else(|| stem.and_then(|s| self.names.get(&s))) else { continue };
                for &(g, a, n) in ts {
                    if n > super::SIBLING_MAX || n == 0 {
                        continue;
                    }
                    if bytes + n > crate::vfs::PREDICT_BYTES {
                        return out;
                    }
                    bytes += n;
                    out.extend(span(g, a, n));
                }
            }
        }
        out
    }
}

#[derive(Default, Clone, Copy)]
struct Score {
    reads: u64,
    misses: u64,
    hinted: u64,
    wasted: u64,
}

/// Replays `seq` (file, chunk) against `files` with variant `v` (and `fs` the foresight).
fn replay(files: &[File], dirs: &HashMap<String, Vec<usize>>, seq: &[(usize, usize)], v: &Variant, fs: Option<&Foresight>) -> Score {
    let mut have: HashSet<(usize, usize)> = HashSet::new();
    let mut touched: HashSet<usize> = HashSet::new();
    let mut hinted: HashSet<(usize, usize)> = HashSet::new();
    let mut scanned = HashSet::new();
    let mut s = Score::default();
    for (n, &(f, c)) in seq.iter().enumerate() {
        s.reads += 1;
        let first = touched.insert(f);
        let hit = have.contains(&(f, c));
        // (the first read waits: then what every start reads goes too)
        if n == 0 && v.boot {
            for k in super::boot_chunks(files.iter().map(|x| (x.path.as_str(), x.size, x.chunks.iter().map(|c| c.1).collect()))) {
                if k != (f, c) && have.insert(k) {
                    hinted.insert(k);
                }
            }
        }
        // (every read asks the foresight, as kura does: what it reaches names)
        if let Some(fs) = fs.filter(|_| v.foresee) {
            for k in fs.hints(files, f, c, &mut scanned) {
                if k != (f, c) && have.insert(k) {
                    hinted.insert(k);
                }
            }
        }
        if hit {
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
/// The engine a game's files say (games carry no engine field): by the archives it packs.
fn engine(files: &[File]) -> &'static str {
    let has = |f: &dyn Fn(&str, &str) -> bool| {
        files.iter().any(|x| {
            let name = x.path.rsplit('/').next().unwrap_or("").to_ascii_lowercase();
            let e = name.rsplit_once('.').map_or("", |p| p.1).to_string();
            f(&name, &e)
        })
    };
    type Test<'a> = &'a dyn Fn(&str, &str) -> bool;
    let table: [(&str, Test); 14] = [
        ("KiriKiri", &|_, e| e == "xp3"),
        ("Siglus", &|n, e| e == "ovk" || n == "scene.pck" || n == "gameexe.dat"),
        ("Unity", &|_, e| e == "assets" || e == "unity3d" || e == "ress"),
        ("Ren'Py", &|_, e| e == "rpa" || e == "rpyc"),
        ("Godot", &|_, e| e == "pck"),
        ("Artemis", &|_, e| e == "pfs"),
        ("NScripter", &|_, e| e == "nsa" || e == "sar" || e == "ns2"),
        ("CatSystem2", &|_, e| e == "int"),
        ("YU-RIS", &|_, e| e == "ypf" || e == "ybn"),
        ("System4", &|_, e| e == "ain" || e == "ald" || e == "afa"),
        ("BGI", &|n, e| e == "arc" && n.starts_with("data")),
        ("RealLive", &|_, e| e == "g00" || e == "nwa"),
        ("NeXAS", &|_, e| e == "pac"),
        ("Majiro", &|_, e| e == "mjo"),
    ];
    table.iter().find(|(_, f)| has(f)).map_or("?", |t| t.0)
}

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
        Variant { name: "as is", pol: policy, whole_max: WHOLE_MAX, foresee: false, boot: false },
        Variant { name: "none", pol: |_| Policy { siblings: 0, chunks: 0 }, whole_max: 0, foresee: false, boot: false },
        Variant { name: "foresee only", pol: |_| Policy { siblings: 0, chunks: 0 }, whole_max: 0, foresee: true, boot: false },
        Variant { name: "as is + foresee", pol: policy, whole_max: WHOLE_MAX, foresee: true, boot: false },
        Variant { name: "as is + boot", pol: policy, whole_max: WHOLE_MAX, foresee: false, boot: true },
        Variant { name: "as is + foresee + boot", pol: policy, whole_max: WHOLE_MAX, foresee: true, boot: true },
        Variant { name: "siblings x0", pol: |n| scale(policy(n), 0.0, 1.0), whole_max: WHOLE_MAX, foresee: false, boot: false },
        Variant { name: "siblings x2", pol: |n| scale(policy(n), 2.0, 1.0), whole_max: WHOLE_MAX, foresee: false, boot: false },
        Variant { name: "siblings x4", pol: |n| scale(policy(n), 4.0, 1.0), whole_max: WHOLE_MAX, foresee: false, boot: false },
        Variant { name: "chunks x0", pol: |n| scale(policy(n), 1.0, 0.0), whole_max: WHOLE_MAX, foresee: false, boot: false },
        Variant { name: "chunks x2", pol: |n| scale(policy(n), 1.0, 2.0), whole_max: WHOLE_MAX, foresee: false, boot: false },
        Variant { name: "chunks x4", pol: |n| scale(policy(n), 1.0, 4.0), whole_max: WHOLE_MAX, foresee: false, boot: false },
        Variant { name: "whole 64 MiB", pol: policy, whole_max: 64 << 20, foresee: false, boot: false },
        Variant { name: "whole none", pol: policy, whole_max: 0, foresee: false, boot: false },
    ];
    // engine -> variant -> score; and by kind of what was missed with the hints as they are
    let mut by: BTreeMap<String, Vec<Score>> = BTreeMap::new();
    let mut missed_kind: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    let foresee_dir = std::env::var("HINTS_FORESEE").ok();
    let mut games: Vec<_> = std::fs::read_dir(format!("{store}/games")).unwrap().filter_map(|e| e.ok()).collect();
    games.sort_by_key(|e| e.file_name());
    for e in games {
        let Some(id) = e.file_name().to_str().and_then(|n| n.strip_suffix(".json")).map(String::from) else { continue };
        let json = compact(&std::fs::read_to_string(e.path()).unwrap_or_default());
        // (its own list, else its program's newest: masu pack.rs read_order_list)
        let list = std::fs::read(format!("{jit}/game.{id}.chunks")).ok().filter(|l| l.len() >= 16).or_else(|| {
            let run = field(&json, "run")?.replace("\\\\", "\\");
            let prog: String = run.chars().map(|c| if c.is_ascii_alphanumeric() || "_.-".contains(c) { c } else { '_' }).collect();
            let prefix = format!("sake-{prog}-");
            let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
            for e in std::fs::read_dir(&jit).ok()?.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if n.strip_prefix(&prefix).and_then(|r| r.strip_suffix(".chunks")).is_some_and(|b| !b.contains('-')) {
                    let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                    if best.as_ref().is_none_or(|b| t > b.0) {
                        best = Some((t, e.path()));
                    }
                }
            }
            std::fs::read(best?.1).ok()
        });
        let Some(list) = list else { continue };
        let mut files = Vec::new();
        let (mut tables, mut members) = (Vec::new(), Vec::new());
        for t in mounts(&json) {
            if let Ok(text) = std::fs::read_to_string(format!("{store}/trees/{t}.tsv")) {
                files.extend(tree(&text));
            }
            if let Some(d) = &foresee_dir {
                tables.extend(std::fs::read_to_string(format!("{d}/{t}.tsv")));
                members.extend(std::fs::read_to_string(format!("{d}/{t}.members")));
            }
        }
        let fs = foresee_dir.as_ref().map(|_| Foresight::new(&files, &tables, &members));
        if std::env::var_os("HINTS_DEBUG").is_some() {
            if let Some(f) = &fs {
                let ms: usize = f.members.values().map(Vec::len).sum();
                eprintln!("{id}: files {}, tables {} ({} scripts), members {ms} in {} archives (of {} lines)", files.len(), tables.len(), f.table.len(), f.members.len(), members.iter().map(|m| m.lines().count()).sum::<usize>());
            }
        }
        let engine = field(&json, "engine").unwrap_or(engine(&files)).to_string();
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
        let scores: Vec<Score> = variants.iter().map(|v| replay(&files, &dirs, &seq, v, fs.as_ref())).collect();
        let scripts = fs.as_ref().map_or(0, |f| f.table.len());
        eprintln!("{id} ({engine}): {} reads, misses {} -> {} as is -> {} with foresee ({scripts} scripts in its tables) -> {} with boot too, {} / {} / {} MiB hinted for nothing", seq.len(), scores[1].misses, scores[0].misses, scores[3].misses, scores[5].misses, scores[0].wasted >> 20, scores[3].wasted >> 20, scores[5].wasted >> 20);
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
    let as_is = Variant { name: "as is", pol: policy, whole_max: WHOLE_MAX, foresee: false, boot: false };
    let none = Variant { name: "none", pol: |_| Policy { siblings: 0, chunks: 0 }, whole_max: 0, foresee: false, boot: false };
    let s = replay(&files, &dirs, &seq, &as_is, None);
    // (v000 waits and hints 001..019; the movie's 3 chunks wait)
    assert_eq!((s.reads, s.misses, s.wasted), (23, 4, 0));
    assert_eq!(replay(&files, &dirs, &seq, &none, None).misses, 23);
    // (read only the first: 19 hinted for nothing)
    let s = replay(&files, &dirs, &[(0, 0)], &as_is, None);
    assert_eq!((s.misses, s.wasted), (1, 19_000));
    let names = |ps: &[&str]| ps.iter().map(|p| File { path: p.to_string(), size: 1, chunks: vec![] }).collect::<Vec<_>>();
    assert_eq!(engine(&names(&["game.exe", "data.xp3", "voice.xp3"])), "KiriKiri");
    assert_eq!(engine(&names(&["SiglusEngine.exe", "Scene.pck", "koe/z0001.ovk"])), "Siglus");
    assert_eq!(engine(&names(&["g_Data/data.unity3d", "g_Data/sharedassets0.assets"])), "Unity");
    assert_eq!(engine(&names(&["game.exe", "readme.txt"])), "?");
    assert_eq!(mounts(r#"{"mounts":["/masu/game=g1","/x="],"engine":"KiriKiri"}"#), vec!["g1"]);
    assert_eq!(field(r#"{"id":"a","engine":"KiriKiri Z"}"#, "engine"), Some("KiriKiri Z"));
    // (as masu writes them: pretty, spaces inside strings kept)
    let pretty = compact("{\n  \"run\": \"Z:\\\\masu\\\\a b.exe\",\n  \"mounts\": [\n    \"/masu/game=g 1\"\n  ],\n  \"engine\": \"Siglus \\\"x\\\"\"\n}");
    assert_eq!(mounts(&pretty), vec!["g 1"]);
    assert_eq!(field(&pretty, "run"), Some("Z:\\\\masu\\\\a b.exe"));
    let t = tree("v1\t\t\nf\t0\t0\tvoice/a\\tb.ogg\t3\taa:1,bb:2\nd\t0\t0\tvoice\t0\t\n");
    assert_eq!((t.len(), t[0].path.as_str(), t[0].chunks.len()), (1, "voice/a\tb.ogg", 2));
}

#[test]
fn replay_counts_what_the_tables_foresaw() {
    // (data.arc: a script in its first chunk, a picture in its second; loose voices; a far file)
    let files = vec![
        File { path: "data.arc".into(), size: 2000, chunks: vec![("a0".into(), 1000), ("a1".into(), 1000)] },
        File { path: "voice/v001.ogg".into(), size: 10, chunks: vec![("v1".into(), 10)] },
        File { path: "voice/v002.ogg".into(), size: 10, chunks: vec![("v2".into(), 10)] },
        File { path: "bgm/far.ogg".into(), size: 10, chunks: vec![("f".into(), 10)] },
    ];
    let table = format!("{}\n{}{}", foresee::TABLE_HEADER, foresee::table_line("01_start", &["bg01a".into(), "v001".into()]), foresee::table_line("02_next", &["v001".into(), "v002".into(), "far".into()]));
    let members = "data.arc\t01_start\t16\t500\ndata.arc\tbg01a\t1200\t300\n".to_string();
    let fs = Foresight::new(&files, &[table], &[members]);
    let dirs = HashMap::new();
    let seq = [(0, 0), (0, 1), (1, 0), (2, 0), (3, 0)];
    let none = Variant { name: "none", pol: |_| Policy { siblings: 0, chunks: 0 }, whole_max: 0, foresee: false, boot: false };
    let only = Variant { name: "foresee only", pol: none.pol, whole_max: 0, foresee: true, boot: false };
    assert_eq!(replay(&files, &dirs, &seq, &none, Some(&fs)).misses, 5, "not asked: as none");
    // (01_start's chunk names bg01a's chunk and v001; v001, read, is followed by v002 and far
    // in 02_next)
    let s = replay(&files, &dirs, &seq, &only, Some(&fs));
    assert_eq!((s.reads, s.misses, s.wasted), (5, 1, 0));
    // (none read: all hinted for nothing; a table-less game: nothing foreseen)
    let s = replay(&files, &dirs, &[(0, 0)], &only, Some(&fs));
    assert_eq!((s.misses, s.wasted), (1, 1010));
    assert_eq!(replay(&files, &dirs, &seq, &only, Some(&Foresight::new(&files, &[], &[]))).misses, 5);
    // (the start's hints: the archive's ends at the first read, a voice not)
    let boot = Variant { name: "boot", pol: none.pol, whole_max: 0, foresee: false, boot: true };
    let s = replay(&files, &dirs, &[(1, 0), (0, 0), (0, 1), (2, 0)], &boot, None);
    assert_eq!((s.misses, s.wasted), (2, 0));
}
