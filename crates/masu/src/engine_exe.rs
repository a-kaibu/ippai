//! The engine a game's launcher would start, started in its place. Key's and others' Siglus
//! games ship an autorun menu (anemoi: Start.exe -> StartData\StartMenu.exe /from="autorun" ->
//! GameData\SiglusEngine.exe /window -> GameData\data\SiglusEngine.exe): every hop is a page
//! reload with sake started again, and the menu itself is only buttons (start windowed/full
//! screen, manual, uninstall). The engine is the SiglusEngine.exe beside Gameexe.dat (its
//! settings: Siglus reads them from its own folder), so a game registered with a launcher
//! starts that instead (`/games`, serve.rs; `masu add`, unpack.rs pick_exe).

/// In a game's file list (paths relative to its tree, `/` separated): the engine to start, if it
/// is a Siglus game: the SiglusEngine.exe in a folder that has Gameexe.dat, the shallowest
/// (then the first by name). Names are matched as Windows does, without case.
pub fn siglus_engine<'a>(files: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut exes = Vec::new();
    let mut dats = std::collections::HashSet::new();
    for f in files {
        let (dir, name) = f.rsplit_once('/').unwrap_or(("", f));
        if name.eq_ignore_ascii_case("SiglusEngine.exe") {
            exes.push(f);
        } else if name.eq_ignore_ascii_case("Gameexe.dat") {
            dats.insert(dir.to_lowercase());
        }
    }
    exes.retain(|f| dats.contains(&f.rsplit_once('/').map_or("", |x| x.0).to_lowercase()));
    exes.sort_by(|a, b| a.matches('/').count().cmp(&b.matches('/').count()).then(a.cmp(b)));
    exes.first().map(|s| s.to_string())
}

/// The paths of the files `trees/<name>.tsv` lists (escapes undone, pack.rs `esc`).
pub fn tree_files(tsv: &str) -> Vec<String> {
    let unesc = |s: &str| {
        let mut out = String::with_capacity(s.len());
        let mut it = s.chars();
        while let Some(c) = it.next() {
            if c == '\\' {
                match it.next() {
                    Some('t') => out.push('\t'),
                    Some('n') => out.push('\n'),
                    Some(o) => out.push(o),
                    None => out.push('\\'),
                }
            } else {
                out.push(c);
            }
        }
        out
    };
    tsv.lines().skip(1).filter_map(|l| {
        let mut f = l.split('\t');
        (f.next()? == "f").then_some(())?;
        Some(unesc(f.nth(2)?))
    }).collect()
}

/// For a game entry (store/games/<id>.json) whose `run` is a program in the tree mounted at
/// /ippai/game other than its Siglus engine: the entry with `run` that engine, no arguments (the
/// launcher's are its own), and `launcher` the program it named (the page reads saves made
/// under it: play.ts). None for anything else. `tree` reads a tree's list by name.
#[cfg(test)]
pub fn engine_entry(game: &serde_json::Value, tree: impl Fn(&str) -> Option<std::sync::Arc<Vec<String>>>) -> Option<serde_json::Value> {
    entry_of(game, |name| siglus_engine(tree(name)?.iter().map(String::as_str)))
}

/// [`engine_entry`] with `engine` giving a tree's Siglus engine by the tree's name
fn entry_of(game: &serde_json::Value, engine: impl Fn(&str) -> Option<String>) -> Option<serde_json::Value> {
    const ROOT: &str = "Z:\\ippai\\game\\";
    let run = game["run"].as_str()?;
    if run.len() <= ROOT.len() || !run.get(..ROOT.len()).is_some_and(|r| r.eq_ignore_ascii_case(ROOT)) {
        return None;
    }
    let name = game["mounts"].as_array()?.iter().filter_map(|m| m.as_str()?.strip_prefix("/ippai/game=")).next()?;
    let engine = engine(name)?;
    let rel = run[ROOT.len()..].replace('\\', "/");
    if rel.eq_ignore_ascii_case(&engine) {
        return None;
    }
    let mut g = game.clone();
    g["run"] = format!("{ROOT}{}", engine.replace('/', "\\")).into();
    g["args"] = serde_json::json!([]);
    g["launcher"] = run.into();
    Some(g)
}

/// [`engine_entry`] with the trees read from `store/trees`, each tree's engine (not its whole
/// list of files: tens of MB for a library) kept while its file is unchanged (the game list is
/// asked for at every visit of the list page); a tree gone is dropped.
pub fn resolve(store: &std::path::Path, game: &mut serde_json::Value) {
    type Cache = std::collections::HashMap<String, (std::time::SystemTime, Option<String>)>;
    static CACHE: std::sync::LazyLock<std::sync::Mutex<Cache>> = std::sync::LazyLock::new(Default::default);
    if let Some(g) = entry_of(game, |name| engine_cached(&CACHE, store, name)) {
        *game = g;
    }
}

fn engine_cached(cache: &std::sync::Mutex<std::collections::HashMap<String, (std::time::SystemTime, Option<String>)>>, store: &std::path::Path, name: &str) -> Option<String> {
    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        return None;
    }
    let p = store.join("trees").join(format!("{name}.tsv"));
    let Some(mtime) = std::fs::metadata(&p).ok().and_then(|m| m.modified().ok()) else {
        cache.lock().unwrap().remove(name);
        return None;
    };
    if let Some((t, e)) = cache.lock().unwrap().get(name) {
        if *t == mtime {
            return e.clone();
        }
    }
    let e = siglus_engine(tree_files(&std::fs::read_to_string(&p).ok()?).iter().map(String::as_str));
    cache.lock().unwrap().insert(name.to_string(), (mtime, e.clone()));
    e
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// anemoi's tree as worker2 has it (2026-11-14), the parts that matter.
    const ANEMOI: &[&str] = &[
        "Start.exe",
        "StartData/Config.exe",
        "StartData/Start.ini",
        "StartData/StartMenu.exe",
        "StartData/TraceLog.exe",
        "StartData/GameData/SiglusEngine.exe",
        "StartData/GameData/data/Gameexe.dat",
        "StartData/GameData/data/SiglusEngine.exe",
        "StartData/GameData/data/savedata/global.sav",
        "license/license.txt",
    ];

    #[test]
    fn engines() {
        assert_eq!(siglus_engine(ANEMOI.iter().copied()).as_deref(), Some("StartData/GameData/data/SiglusEngine.exe"));
        // LOOPERS, 終のステラ: at the top, beside Gameexe.dat
        assert_eq!(siglus_engine(["Gameexe.dat", "Scene.pck", "SiglusEngine.exe", "Start.bmp"]).as_deref(), Some("SiglusEngine.exe"));
        // names without case, as Windows; the shallowest of two
        assert_eq!(siglus_engine(["a/b/GAMEEXE.DAT", "a/b/siglusengine.exe", "c/Gameexe.dat", "c/SiglusEngine.exe"]).as_deref(), Some("c/SiglusEngine.exe"));
        // a SiglusEngine.exe with no Gameexe.dat beside it is not one; not a Siglus game
        assert_eq!(siglus_engine(["x/SiglusEngine.exe", "y/Gameexe.dat"]), None);
        assert_eq!(siglus_engine(["game.exe", "data.xp3"]), None);
    }

    fn entry(run: &str, mounts: &[&str]) -> serde_json::Value {
        serde_json::json!({ "id": "anemoi", "title": "anemoi", "run": run, "args": ["/from=\"autorun\""], "mounts": mounts, "env": ["A=1"] })
    }

    fn trees(name: &str) -> Option<std::sync::Arc<Vec<String>>> {
        (name == "anemoi").then(|| std::sync::Arc::new(ANEMOI.iter().map(|s| s.to_string()).collect()))
    }

    #[test]
    fn launcher_entries_start_the_engine() {
        for run in ["Z:\\ippai\\game\\Start.exe", "Z:\\ippai\\game\\StartData\\StartMenu.exe", "z:\\IPPAI\\game\\StartData\\GameData\\SiglusEngine.exe"] {
            let g = engine_entry(&entry(run, &["/ippai/game=anemoi"]), trees).unwrap();
            assert_eq!(g["run"], "Z:\\ippai\\game\\StartData\\GameData\\data\\SiglusEngine.exe", "{run}");
            assert_eq!(g["args"], serde_json::json!([]));
            assert_eq!(g["launcher"], run);
            // (the rest as it was)
            assert_eq!(g["env"], serde_json::json!(["A=1"]));
            assert_eq!(g["mounts"], serde_json::json!(["/ippai/game=anemoi"]));
        }
        // the engine already (any case), another tree, no tree, a program outside the game: as they are
        assert!(engine_entry(&entry("Z:\\ippai\\game\\StartData\\GameData\\data\\siglusengine.exe", &["/ippai/game=anemoi"]), trees).is_none());
        assert!(engine_entry(&entry("Z:\\ippai\\game\\Start.exe", &["/ippai/game=other"]), trees).is_none());
        assert!(engine_entry(&entry("Z:\\ippai\\game\\Start.exe", &[]), trees).is_none());
        assert!(engine_entry(&entry("C:\\windows\\notepad.exe", &["/ippai/game=anemoi"]), trees).is_none());
        assert!(engine_entry(&serde_json::json!({ "id": "x" }), trees).is_none());
        // (the root's length falls inside a character)
        assert!(engine_entry(&entry("C:\\Games\\さくら\\x.exe", &["/ippai/game=anemoi"]), trees).is_none());
    }

    proptest! {
        #[test]
        fn any_run_is_answered(run in "\\PC{0,24}", cut in 0usize..20) {
            let root: String = "Z:\\ippai\\game\\".chars().take(cut).collect();
            engine_entry(&entry(&(root + &run), &["/ippai/game=anemoi"]), trees);
        }
    }

    #[test]
    fn tree_lists() {
        let tsv = "vnvm-tree\t2\t4194304\nd\t755\t0\tStartData\nf\t644\t0\tStart.exe\t3\tab:3\nf\t644\t0\ta\\tb\\\\c.dat\t0\t\nl\t777\t0\tlink\tStart.exe\n";
        assert_eq!(tree_files(tsv), ["Start.exe", "a\tb\\c.dat"]);
    }

    #[test]
    fn resolve_reads_the_store_and_keeps_the_list() {
        let dir = std::env::temp_dir().join(format!("masu-engine-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        let mut tsv = String::from("vnvm-tree\t2\t4194304\n");
        for f in ANEMOI {
            tsv.push_str(&format!("f\t644\t0\t{f}\t1\tab:1\n"));
        }
        std::fs::write(dir.join("trees/anemoi-t.tsv"), &tsv).unwrap();
        let mut g = entry("Z:\\ippai\\game\\Start.exe", &["/ippai/game=anemoi-t"]);
        resolve(&dir, &mut g);
        assert_eq!(g["run"], "Z:\\ippai\\game\\StartData\\GameData\\data\\SiglusEngine.exe");
        // a tree path that would leave trees/: not read
        let mut bad = entry("Z:\\ippai\\game\\Start.exe", &["/ippai/game=../trees/anemoi-t"]);
        resolve(&dir, &mut bad);
        assert_eq!(bad["run"], "Z:\\ippai\\game\\Start.exe");
        let _ = std::fs::remove_dir_all(dir);
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]
        /// review b00: the cache keeps each tree's engine only (not its list of files), answers
        /// as reading the tree each time would, whatever trees are written, rewritten and
        /// removed in any order, and forgets a removed tree
        #[test]
        fn cache_answers_as_reading(ops in proptest::collection::vec((0usize..3, 0u8..4), 1..24)) {
            let dir = std::env::temp_dir().join(format!("masu-engine-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(dir.join("trees")).unwrap();
            let cache = std::sync::Mutex::new(std::collections::HashMap::new());
            for (step, (t, what)) in ops.into_iter().enumerate() {
                let name = format!("t{t}");
                let p = dir.join("trees").join(format!("{name}.tsv"));
                match what {
                    0 => { let _ = std::fs::remove_file(&p); }
                    k => {
                        let mut tsv = String::from("vnvm-tree\t2\t4194304\n");
                        let files: &[&str] = match k { 1 => ANEMOI, 2 => &["Start.exe", "x/SiglusEngine.exe", "x/Gameexe.dat"], _ => &["Start.exe", "a.dat"] };
                        for f in files {
                            tsv.push_str(&format!("f\t644\t0\t{f}\t1\tab:1\n"));
                        }
                        std::fs::write(&p, &tsv).unwrap();
                        // (each write a time of its own: the cache goes by the file's time)
                        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000 + step as u64);
                        std::fs::File::options().write(true).open(&p).unwrap().set_modified(at).unwrap();
                    }
                }
                for q in 0..3 {
                    let name = format!("t{q}");
                    let p = dir.join("trees").join(format!("{name}.tsv"));
                    let want = std::fs::read_to_string(&p).ok().and_then(|tsv| siglus_engine(tree_files(&tsv).iter().map(String::as_str)));
                    prop_assert_eq!(engine_cached(&cache, &dir, &name), want);
                    prop_assert_eq!(cache.lock().unwrap().contains_key(&name), p.exists());
                }
            }
            let _ = std::fs::remove_dir_all(dir);
        }

        /// Whatever the tree: the engine picked is a SiglusEngine.exe of the list with a
        /// Gameexe.dat in its folder, none shallower; none when no such pair exists.
        #[test]
        fn engine_model(files in proptest::collection::vec(("(a|b|a/c|)", "(SiglusEngine\\.exe|siglusengine\\.EXE|Gameexe\\.dat|GAMEEXE\\.DAT|x\\.exe)"), 0..10)) {
            let paths: Vec<String> = files.iter().map(|(d, n)| if d.is_empty() { n.clone() } else { format!("{d}/{n}") }).collect();
            let dir = |p: &str| p.rsplit_once('/').map_or(String::new(), |x| x.0.to_lowercase());
            let ok = |p: &String| p.to_lowercase().ends_with("siglusengine.exe") && paths.iter().any(|q| q.to_lowercase().ends_with("gameexe.dat") && dir(q) == dir(p));
            let got = siglus_engine(paths.iter().map(String::as_str));
            match got {
                Some(e) => {
                    prop_assert!(paths.contains(&e) && ok(&e));
                    prop_assert!(paths.iter().filter(|p| ok(p)).all(|p| p.matches('/').count() >= e.matches('/').count()));
                }
                None => prop_assert!(!paths.iter().any(ok)),
            }
        }
    }
}
