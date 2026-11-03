//! `vnvm serve`: the web app and the store made by `vnvm pack`.
//!
//! - `/` the built web app (web/dist)
//! - `/store/trees/<name>.tsv`: a packed tree's file list
//! - `/store/chunks/ab/<hash>`: stored zstd-compressed, sent with
//!   `Content-Encoding: zstd` so the browser decodes them natively, and marked
//!   immutable so its HTTP cache keeps them.
//! - `?v=...` on any URL: kept by the browser for good (the page changes the version when
//!   the content changes)
//! - `/jit/...`: sent zstd-compressed on the way (CompressionLayer)
//! - `/jit/<name>.parts`: the x86 regions runs of a game saved (vnx86::jit::saved), one line
//!   per part: module, index, version; `PUT /jit-save/<name>` with (u32 module length, module,
//!   index) adds a run's new regions as a new part (parts compiled before stay the same); `PUT /jit-save/<name>.chunks` stores the store chunks a
//!   run read first (16-byte hashes), which the page fetches ahead next time.
//! - `/jit/_shared-<build>.parts`, `/jit/_shared.chunks`: the regions and chunks several
//!   programs saved (shared libraries and the like; the chunks only for a program with no list
//!   of its own yet): every page loads them too, so a
//!   game's first run starts with them
//! - `GET /tree/<name>/<path>`: one file of a tree as itself (web games in an iframe; treefiles.rs)
//! - `GET /games`: the launcher's list, the store's `games/<id>.json` (`vnvm add-game`) as
//!   one JSON array; covers are under `/store/games/`
//! - pre-runs: at the start and when the launcher lists the games, each listed game with no
//!   saved regions for this build's translator (`web/sake/jit-version.txt`) runs once for a
//!   minute in a headless Chromium (VNVM_CHROME or one on the PATH; `--no-prerun` for none),
//!   saving its regions like any run: the first real run starts like a second one.
//!   `<jit>/<name>.prerun` marks it done
//! - saved regions are optimized on the server after each save (binaryen wasm-opt, when there
//!   is one: see optimize); `.parts` then lists the optimized copies
//! - `PUT /bench-log`: a result from web/bench.html, printed and appended to
//!   `<jit>/bench-results.txt`
//! - `GET`/`PUT /saves/<name>`: saved games backed up to S3 (see saves.rs)
//! - `PUT /log-save/<name>`: a run's log from the page (web/src/crashlog.ts: crash reports,
//!   sessions a killed tab left, ...), kept as `<logs>/<name>.txt`; `GET /log-list` lists them
//!   (JSON, newest first), `/logs/<name>.txt` serves one; web/logs.html shows them
//!
//! - the web app's files are sent zstd-compressed to browsers that accept it: at the start the
//!   server writes `<file>.zst` beside each script, wasm, page and style sheet of `--web` (again
//!   when the file is newer), and `ServeDir` picks it (see `precompress`)
//!
//! Every response carries COOP/COEP: the program's threads (wasm pthreads) need SharedArrayBuffer,
//! which browsers only allow on cross-origin isolated pages.

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, put},
    Router,
};
use crate::saves;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use vnx86::jit::{saved, wasm};
use tower_http::services::ServeDir;

#[derive(clap::Args)]
pub struct Args {
    /// The built web app.
    #[arg(long, default_value = "web/dist")]
    web: PathBuf,
    /// Saved x86 regions, per game.
    #[arg(long, default_value = "jit")]
    jit: PathBuf,
    /// The runs' logs the pages send (crash reports).
    #[arg(long, default_value = "logs")]
    logs: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
    /// No pre-runs (see prerun_games): a game's first run translates its code itself.
    #[arg(long)]
    no_prerun: bool,
}

/// The jit directory; the lock keeps merges one at a time.
type Jit = Arc<(PathBuf, tokio::sync::Mutex<()>)>;

/// Merges a run's regions into `name`'s: those not saved already (same roots and code) go after
/// the others.
async fn save_jit(State(jit): State<Jit>, Path(name): Path<String>, body: Bytes) -> StatusCode {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return StatusCode::BAD_REQUEST;
    }
    // the store chunks a run read first, as is (the page fetches them ahead)
    if name.ends_with(".chunks") {
        if body.len() % 16 != 0 {
            return StatusCode::BAD_REQUEST;
        }
        let _g = jit.1.lock().await;
        let _ = tokio::fs::create_dir_all(&jit.0).await;
        if tokio::fs::write(jit.0.join(&name), &body).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        // chunks two programs or more read first (fonts, the prefix) make _shared.chunks, which
        // a page fetches ahead when its program has no list yet (a game's first run). Made
        // again each time from the programs' lists: what no program reads any more (fonts
        // replaced, old DLLs) goes.
        let shared_name = format!("{SHARED}.chunks");
        let mut lists: std::collections::BTreeMap<String, (std::time::SystemTime, Vec<u8>)> = Default::default();
        if let Ok(mut d) = tokio::fs::read_dir(&jit.0).await {
            while let Ok(Some(e)) = d.next_entry().await {
                let f = e.file_name().to_string_lossy().into_owned();
                if !f.ends_with(".chunks") || f == shared_name {
                    continue;
                }
                let t = e.metadata().await.and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
                // (a program by its name without the build: the newest build's list)
                let prog = f.rsplit_once('-').map_or(f.clone(), |x| x.0.to_string());
                if lists.get(&prog).is_some_and(|l| l.0 >= t) {
                    continue;
                }
                if let Ok(b) = tokio::fs::read(jit.0.join(&f)).await {
                    lists.insert(prog, (t, b));
                }
            }
        }
        let lists: Vec<Vec<u8>> = lists.into_values().map(|l| l.1).collect();
        let mut count: std::collections::HashMap<[u8; 16], u32> = Default::default();
        for b in &lists {
            let mut once = std::collections::HashSet::new();
            for c in b.chunks_exact(16) {
                let c: [u8; 16] = c.try_into().unwrap();
                if once.insert(c) {
                    *count.entry(c).or_default() += 1;
                }
            }
        }
        let mut shared = Vec::new();
        let mut have = std::collections::HashSet::new();
        for b in &lists {
            for c in b.chunks_exact(16) {
                let c: [u8; 16] = c.try_into().unwrap();
                if count[&c] >= 2 && shared.len() < 16 * 8192 && have.insert(c) {
                    shared.extend_from_slice(&c);
                }
            }
        }
        let tmp = jit.0.join(format!("{shared_name}.tmp"));
        if tokio::fs::write(&tmp, &shared).await.is_err() || tokio::fs::rename(&tmp, jit.0.join(&shared_name)).await.is_err() {
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
        return StatusCode::NO_CONTENT;
    }
    let Some(n) = body.get(..4).map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize) else {
        return StatusCode::BAD_REQUEST;
    };
    let (Some(new_wasm), Some(new_idx)) = (body.get(4..4 + n), body.get(4 + n..).and_then(saved::decode)) else {
        return StatusCode::BAD_REQUEST;
    };
    let n_funcs = new_idx.len();
    let all_idx = new_idx.clone();
    let _g = jit.1.lock().await;
    // Regions another program saved too (same roots and guest code: shared DLLs and helper
    // processes, the same in every game) go to the shared bundle of this build, which every
    // program's page loads as well: a game's first run starts with them translated (and
    // compiled, from the browser's code cache), like Windows on ARM's per-module translation
    // cache that "other apps can make use of on first launch".
    let (prog, ver) = name.rsplit_once('-').unwrap_or((&name, ""));
    let shared_name = format!("{SHARED}-{ver}");
    let shared = read_parts(&jit.0, &shared_name).await;
    let new_idx: Vec<(usize, saved::Entry)> = new_idx.into_iter().enumerate().filter(|(_, e)| !shared.iter().any(|(_, es)| es.iter().any(|s| same(s, e)))).collect();
    let mut elsewhere = vec![false; new_idx.len()];
    if prog != SHARED && !ver.is_empty() {
        let mut dir = match tokio::fs::read_dir(&jit.0).await {
            Ok(d) => Some(d),
            Err(_) => None,
        };
        while let Some(d) = dir.as_mut() {
            let Ok(Some(f)) = d.next_entry().await else { break };
            let f = f.file_name().to_string_lossy().into_owned();
            let Some(other) = f.strip_suffix(".parts") else { continue };
            if other == name || other == shared_name || other.rsplit_once('-').map(|x| x.1) != Some(ver) {
                continue;
            }
            for (_, es) in read_parts(&jit.0, other).await {
                for (k, (_, e)) in new_idx.iter().enumerate() {
                    elsewhere[k] |= es.iter().any(|s| same(s, e));
                }
            }
        }
    }
    let pick = |to_shared: bool| -> Option<(Vec<u8>, Vec<saved::Entry>)> {
        let mut keep = vec![false; n_funcs];
        let mut idx = Vec::new();
        for (k, (i, e)) in new_idx.iter().enumerate() {
            if elsewhere[k] == to_shared {
                keep[*i] = true;
                idx.push(e.clone());
            }
        }
        if idx.is_empty() {
            return None;
        }
        Some((if keep.iter().all(|&k| k) { new_wasm.to_vec() } else { wasm::select(new_wasm, &saved::keep_funcs(&all_idx, &keep))? }, idx))
    };
    let mut status = StatusCode::NO_CONTENT;
    for (to_shared, target) in [(true, &shared_name), (false, &name)] {
        if let Some((w, idx)) = pick(to_shared) {
            let s = add_part(&jit.0, target, w, idx).await;
            if s != StatusCode::NO_CONTENT {
                status = s;
            }
        }
    }
    // (after the parts are written: before, it read the old lists, and a game's first save
    // stayed unoptimized until the next)
    tokio::spawn(optimize(jit.clone(), vec![shared_name.clone(), name.clone()]));
    status
}

/// Optimizes the saved regions of `names` on the server, where there is time to (binaryen's
/// `wasm-opt -O2`, VNVM_WASM_OPT, beside the server or on the PATH; nothing without it): each part's module gets
/// an optimized copy `<name>.<k.><version>.opt.wasm` that `<name>.parts` lists instead. The
/// translator's own module stays the part (merges and selections read it: wasm-opt renumbers
/// types and folds identical functions). Reunion's pre-run regions: 7.6 -> 5.4 MB, title
/// 12.6 -> 11.9 s in Chromium, 22.8 -> 21.4 s Android-like; the story's CPU is the same
/// (V8's optimizing tier already does what binaryen does inside a function).
async fn optimize(jit: Jit, names: Vec<String>) {
    static ONE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _one = ONE.lock().await;
    // (a release has its own next to the server)
    let beside = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("wasm-opt"))).filter(|p| p.is_file());
    let Some(opt) = std::env::var_os("VNVM_WASM_OPT").map(PathBuf::from).or(beside).or_else(|| find_on_path(&["wasm-opt"])) else { return };
    for name in names {
        let Ok(list) = tokio::fs::read_to_string(jit.0.join(format!("{name}.parts"))).await else { continue };
        let mut made = false;
        for line in list.lines() {
            let f: Vec<&str> = line.split(' ').collect();
            let [wasm, _, v] = f[..] else { continue };
            let Some(stem) = wasm.strip_suffix("wasm") else { continue };
            if wasm.ends_with(".opt.wasm") {
                continue;
            }
            let (out, tmp) = (jit.0.join(format!("{stem}{v}.opt.wasm")), jit.0.join(format!("{stem}{v}.opt.tmp")));
            let t0 = std::time::Instant::now();
            // (multivalue: the function-level translator's adapters return several values)
            let run = tokio::process::Command::new(&opt)
                .args(["--enable-threads", "--enable-simd", "--enable-bulk-memory", "--enable-tail-call", "--enable-sign-ext"])
                .args(["--enable-nontrapping-float-to-int", "--enable-mutable-globals", "--enable-multivalue", "-O2"])
                .arg(jit.0.join(wasm))
                .arg("-o")
                .arg(&tmp)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .output()
                .await;
            let ok = run.as_ref().is_ok_and(|o| o.status.success());
            if ok && tokio::fs::rename(&tmp, &out).await.is_ok() {
                eprintln!("jit {wasm}: optimized in {:?}", t0.elapsed());
                made = true;
            } else {
                let _ = tokio::fs::remove_file(&tmp).await;
                // (only the first lines: a validation error dumps the whole module)
                let err = run.map(|o| String::from_utf8_lossy(&o.stderr).lines().take(3).collect::<Vec<_>>().join(" | ")).unwrap_or_default();
                eprintln!("jit {wasm}: {} failed: {}", opt.display(), err.chars().take(400).collect::<String>());
                return;
            }
        }
        if !made {
            continue;
        }
        // list the copies (the parts may have changed meanwhile: list what is there now)
        let _g = jit.1.lock().await;
        let parts = read_parts(&jit.0, &name).await;
        let mut list = String::new();
        let mut keep = std::collections::HashSet::new();
        for (k, (_, e)) in parts.iter().enumerate() {
            let stem = if k == 0 { String::new() } else { format!("{k}.") };
            let v = fnv(&saved::encode(e));
            let o = format!("{name}.{stem}{v:08x}.opt.wasm");
            let wasm = if tokio::fs::metadata(jit.0.join(&o)).await.is_ok() { o } else { format!("{name}.{stem}wasm") };
            keep.insert(wasm.clone());
            list.push_str(&format!("{wasm} {name}.{stem}idx {v:08x}\n"));
        }
        let tl = jit.0.join(format!("{name}.parts.tmp"));
        if tokio::fs::write(&tl, list).await.is_ok() {
            let _ = tokio::fs::rename(&tl, jit.0.join(format!("{name}.parts"))).await;
        }
        // copies of parts that are gone
        if let Ok(mut d) = tokio::fs::read_dir(&jit.0).await {
            while let Ok(Some(e)) = d.next_entry().await {
                let f = e.file_name().to_string_lossy().into_owned();
                if f.starts_with(&format!("{name}.")) && f.ends_with(".opt.wasm") && !keep.contains(&f) {
                    let _ = tokio::fs::remove_file(e.path()).await;
                }
            }
        }
    }
}

/// The first of `names` found on the PATH.
fn find_on_path(names: &[&str]) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    names.iter().flat_map(|n| std::env::split_paths(&path).map(move |d| d.join(n))).find(|f| f.is_file())
}

/// The name of the bundle of regions several programs saved (`<SHARED>-<build>`).
const SHARED: &str = "_shared";

/// Whether two saved regions are the same translation's (any thread could have built either).
fn same(a: &saved::Entry, b: &saved::Entry) -> bool {
    a.roots == b.roots && a.code == b.code && a.text == b.text && (a.build > 0) == (b.build > 0)
}

/// The parts of `name` (the base <name>.wasm/.idx, then <name>.<k>.wasm/.idx).
async fn read_parts(dir: &std::path::Path, name: &str) -> Vec<(Vec<u8>, Vec<saved::Entry>)> {
    let mut parts = Vec::new();
    for k in 0..PARTS_MAX {
        let stem = if k == 0 { String::new() } else { format!("{k}.") };
        let (Ok(w), Some(e)) = (
            tokio::fs::read(dir.join(format!("{name}.{stem}wasm"))).await,
            tokio::fs::read(dir.join(format!("{name}.{stem}idx"))).await.ok().and_then(|b| saved::decode(&b)),
        ) else {
            break;
        };
        parts.push((w, e));
    }
    parts
}

/// Adds the regions `new_idx` (the functions of `new_wasm`) to `name`'s parts, those not saved
/// already.
async fn add_part(dir: &std::path::Path, name: &str, new_wasm: Vec<u8>, new_idx: Vec<saved::Entry>) -> StatusCode {
    // (a game's regions stop growing once its code paths have run; this only bounds a runaway)
    const MAX_BYTES: u64 = 256 << 20;
    let path = |ext: &str| dir.join(format!("{name}.{ext}"));
    if tokio::fs::metadata(path("wasm")).await.is_ok_and(|m| m.len() > MAX_BYTES) {
        return StatusCode::INSUFFICIENT_STORAGE;
    }
    let mut parts = read_parts(dir, name).await;
    let new_wasm = &new_wasm[..];
    // the new module's functions to keep
    let keep: Vec<bool> = new_idx.iter().map(|e| !parts.iter().any(|(_, es)| es.contains(e))).collect();
    if !keep.iter().any(|&k| k) {
        return StatusCode::NO_CONTENT;
    }
    let fresh = if keep.iter().all(|&k| k) { Some(new_wasm.to_vec()) } else { wasm::select(new_wasm, &saved::keep_funcs(&new_idx, &keep)) };
    let Some(fresh) = fresh else { return StatusCode::BAD_REQUEST };
    let fresh_idx: Vec<saved::Entry> = new_idx.into_iter().zip(&keep).filter(|x| *x.1).map(|x| x.0).collect();
    // A save goes in a part of its own: the parts the browser has already compiled stay as
    // they are (and their compiled code in its cache). Past PARTS_MAX they become one again.
    let old_parts = parts.len();
    parts.push((fresh, fresh_idx));
    if parts.len() > PARTS_MAX {
        let entries: Vec<saved::Entry> = parts.iter().flat_map(|(_, e)| e.iter().cloned()).collect();
        let names = saved::func_names(&entries);
        let mods: Vec<&[u8]> = parts.iter().map(|(w, _)| &w[..]).collect();
        let Some(merged) = wasm::merge(&mods, &names) else { return StatusCode::BAD_REQUEST };
        parts = vec![(merged, entries)];
    }
    let write = async {
        tokio::fs::create_dir_all(dir).await?;
        let mut list = String::new();
        for (k, (w, e)) in parts.iter().enumerate() {
            let stem = if k == 0 { String::new() } else { format!("{k}.") };
            let idx = saved::encode(e);
            // parts the browser has already compiled stay as they are
            if parts.len() > old_parts || k + 1 == parts.len() {
                let (tw, ti) = (path(&format!("{stem}wasm.tmp")), path(&format!("{stem}idx.tmp")));
                tokio::fs::write(&tw, w).await?;
                tokio::fs::write(&ti, &idx).await?;
                tokio::fs::rename(&tw, path(&format!("{stem}wasm"))).await?;
                tokio::fs::rename(&ti, path(&format!("{stem}idx"))).await?;
            }
            // (the optimized copy of the module, when there is one: see optimize)
            let v = fnv(&idx);
            let opt = format!("{name}.{stem}{v:08x}.opt.wasm");
            let wasm = if tokio::fs::metadata(dir.join(&opt)).await.is_ok() { opt } else { format!("{name}.{stem}wasm") };
            list.push_str(&format!("{wasm} {name}.{stem}idx {v:08x}\n"));
        }
        for k in parts.len()..=PARTS_MAX {
            for ext in ["wasm", "idx"] {
                let _ = tokio::fs::remove_file(path(&format!("{k}.{ext}"))).await;
            }
        }
        let tl = path("parts.tmp");
        tokio::fs::write(&tl, list).await?;
        tokio::fs::rename(&tl, path("parts")).await
    };
    match write.await {
        Ok(()) => {
            let n: usize = parts.iter().map(|(_, e)| e.len()).sum();
            let bytes: usize = parts.iter().map(|(w, _)| w.len()).sum();
            eprintln!("jit {name}: {n} regions, {bytes} bytes, {} parts", parts.len());
            StatusCode::NO_CONTENT
        }
        Err(e) => {
            eprintln!("jit {name}: {e}");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Saved-region parts of a program at most (see save_jit)
const PARTS_MAX: usize = 8;

/// FNV-1a: a part's version in `<name>.parts` (its index changes with its module)
fn fnv(b: &[u8]) -> u32 {
    b.iter().fold(0x811c9dc5u32, |h, &x| (h ^ x as u32).wrapping_mul(0x01000193))
}

/// Prints a bench.html result and appends it to `<jit>/bench-results.txt`.
async fn bench_log(State(jit): State<Jit>, body: Bytes) -> StatusCode {
    use tokio::io::AsyncWriteExt;
    let text = String::from_utf8_lossy(&body);
    eprintln!("{text}");
    let _lock = jit.1.lock().await;
    let path = jit.0.join("bench-results.txt");
    match tokio::fs::OpenOptions::new().create(true).append(true).open(path).await {
        Ok(mut f) => match f.write_all(format!("{text}\n\n").as_bytes()).await {
            Ok(()) => StatusCode::NO_CONTENT,
            Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
        },
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Logs kept at most (the oldest go).
const LOGS_MAX: usize = 2000;

/// Stores a page's log as `<logs>/<name>.txt`.
async fn save_log(State(dir): State<Arc<PathBuf>>, Path(name): Path<String>, body: Bytes) -> StatusCode {
    if name.is_empty() || name.len() > 200 || name.starts_with('.') || !name.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) {
        return StatusCode::BAD_REQUEST;
    }
    let _ = tokio::fs::create_dir_all(&*dir).await;
    let path = dir.join(format!("{name}.txt"));
    let tmp = dir.join(format!(".{name}.tmp"));
    if tokio::fs::write(&tmp, &body).await.is_err() || tokio::fs::rename(&tmp, &path).await.is_err() {
        return StatusCode::INTERNAL_SERVER_ERROR;
    }
    // the first lines say what happened
    let text = String::from_utf8_lossy(&body[..body.len().min(4096)]);
    let reason = text.lines().find_map(|l| l.strip_prefix("reason: ")).unwrap_or("");
    eprintln!("log {}: {} KB, {reason}", path.display(), body.len() / 1024);
    let mut list = list_logs(&dir).await;
    for (old, _, _) in list.drain(..).skip(LOGS_MAX) {
        let _ = tokio::fs::remove_file(dir.join(old)).await;
    }
    StatusCode::NO_CONTENT
}

/// The logs: (file name, size, modified ms), newest first.
async fn list_logs(dir: &std::path::Path) -> Vec<(String, u64, u64)> {
    let mut out = Vec::new();
    let Ok(mut d) = tokio::fs::read_dir(dir).await else { return out };
    while let Ok(Some(e)) = d.next_entry().await {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(m) = e.metadata().await else { continue };
        if !name.ends_with(".txt") || name.starts_with('.') || !m.is_file() {
            continue;
        }
        let t = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
        out.push((name, m.len(), t));
    }
    out.sort_by(|a, b| b.2.cmp(&a.2));
    out
}

async fn log_list(State(dir): State<Arc<PathBuf>>) -> Response {
    let list: Vec<serde_json::Value> = list_logs(&dir).await.into_iter().map(|(name, size, time)| serde_json::json!({ "name": name, "size": size, "time": time })).collect();
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(serde_json::Value::Array(list).to_string()))
        .unwrap()
}

/// What a pre-run needs, and the lock that keeps them one at a time.
struct Prerun {
    store: PathBuf,
    web: PathBuf,
    jit: PathBuf,
    /// where the page is served (the browser opens it here)
    listen: SocketAddr,
    off: bool,
    lock: tokio::sync::Mutex<()>,
}

/// How long a pre-run lasts: past the title, and past the second save of regions (a run saves
/// 20 s after its start and then every 30 s, vnhost jit.rs maybe_save).
const PRERUN_SECS: u64 = 60;

/// Runs each listed game that has no saved regions for this build of the translator once, in a
/// headless browser, before anyone plays it: the run saves its regions (and the store chunks it
/// read first) as any run does, so the first real run starts the way a second one would.
///
/// This is the translation ahead of time: a static translation of a game's exe (every function
/// found from its entry, exports, pointers in its data and jump tables, vnvm-wine docs/hangover.md) was
/// 7x more code than runs and slower to start even when cut to the functions that ran, because
/// it holds whole functions where the JIT's regions hold the blocks that ran.
async fn prerun_games(p: Arc<Prerun>) {
    if p.off {
        return;
    }
    let Ok(_one) = p.lock.try_lock() else { return };
    let Ok(ver) = tokio::fs::read_to_string(p.web.join("sake/jit-version.txt")).await else { return };
    let ver = ver.trim().to_string();
    let Ok(mut dir) = tokio::fs::read_dir(p.store.join("games")).await else { return };
    let mut todo = Vec::new();
    while let Ok(Some(e)) = dir.next_entry().await {
        let Ok(text) = tokio::fs::read_to_string(e.path()).await else { continue };
        let Ok(g) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        let (Some(id), Some(run)) = (g["id"].as_str(), g["run"].as_str()) else { continue };
        // (named as the page names the regions it saves: play.ts jitName)
        let prog: String = run.chars().map(|c| if c.is_ascii_alphanumeric() || "_.-".contains(c) { c } else { '_' }).collect();
        let name = format!("sake-{prog}-{ver}");
        let seen = |ext: &str| tokio::fs::metadata(p.jit.join(format!("{name}.{ext}")));
        if seen("parts").await.is_err() && seen("prerun").await.is_err() {
            todo.push((id.to_string(), name));
        }
    }
    if todo.is_empty() {
        return;
    }
    let Some(chrome) = chrome() else {
        eprintln!("prerun: no Chromium found (set VNVM_CHROME): {} games start without saved regions", todo.len());
        return;
    };
    let host = if p.listen.ip().is_unspecified() { SocketAddr::from(([127, 0, 0, 1], p.listen.port())) } else { p.listen };
    for (id, name) in todo {
        let profile = p.jit.join(".prerun-profile");
        let _ = tokio::fs::remove_dir_all(&profile).await;
        let url = format!("http://{host}/play.html?game={id}&autostart&nosave&headless");
        let mut cmd = tokio::process::Command::new(&chrome);
        // --incognito: an in-memory HTTP cache (on worker2, Ubuntu 26.04, the disk cache never
        // opens and every page load stalls before connecting)
        cmd.args(["--headless=new", "--incognito", "--mute-audio", "--enable-unsafe-webgpu", "--autoplay-policy=no-user-gesture-required"])
            .arg(format!("--user-data-dir={}", profile.display()))
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        // (Chromium's sandbox refuses to run as root)
        if std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0) {
            cmd.arg("--no-sandbox");
        }
        eprintln!("prerun {id}: {} for {PRERUN_SECS} s", url);
        let t0 = std::time::Instant::now();
        match cmd.spawn() {
            Ok(mut child) => {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(PRERUN_SECS), child.wait()).await;
                let _ = child.kill().await;
            }
            Err(e) => {
                eprintln!("prerun {id}: {}: {e}", chrome.display());
                return;
            }
        }
        let _ = tokio::fs::remove_dir_all(&profile).await;
        let saved = tokio::fs::metadata(p.jit.join(format!("{name}.parts"))).await.is_ok();
        eprintln!("prerun {id}: done in {:?}, regions saved: {saved}", t0.elapsed());
        // (once per build: a game that saves nothing in a minute is not tried again and again)
        let _ = tokio::fs::write(p.jit.join(format!("{name}.prerun")), format!("{} s, regions saved: {saved}\n", t0.elapsed().as_secs())).await;
    }
}

/// The browser for pre-runs: VNVM_CHROME, else the first Chromium or Chrome on the PATH.
fn chrome() -> Option<PathBuf> {
    if let Some(c) = std::env::var_os("VNVM_CHROME") {
        return Some(c.into());
    }
    find_on_path(&["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome"])
}

/// The store's `games/*.json` as one array, by file name; and a pre-run of the games without
/// saved regions (one added since the start: before it is clicked, usually).
async fn games(State(p): State<Arc<Prerun>>) -> Response {
    tokio::spawn(prerun_games(p.clone()));
    let store = &p.store;
    let mut files = Vec::new();
    if let Ok(mut dir) = tokio::fs::read_dir(store.join("games")).await {
        while let Ok(Some(e)) = dir.next_entry().await {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".json") && !name.starts_with('.') {
                files.push(e.path());
            }
        }
    }
    files.sort();
    let mut body = String::from("[");
    for f in files {
        if let Ok(text) = tokio::fs::read_to_string(&f).await {
            if body.len() > 1 {
                body.push(',');
            }
            body.push_str(text.trim());
        }
    }
    body.push(']');
    Response::builder()
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.into())
        .unwrap()
}

async fn headers(req: Request<axum::body::Body>, next: Next) -> Response {
    let chunk = req.uri().path().starts_with("/store/chunks/");
    let log = req.uri().path().starts_with("/logs/");
    // a versioned URL (?v=...) names content that never changes: the browser keeps it without
    // asking again, and so keeps its compiled code (a revalidated .wasm loses it)
    let versioned = req.uri().query().is_some_and(|q| q.split('&').any(|p| p.starts_with("v=")));
    let mut res = next.run(req).await;
    let ok = res.status().is_success();
    let h = res.headers_mut();
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    h.insert("cross-origin-embedder-policy", HeaderValue::from_static("require-corp"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    if chunk && ok {
        h.insert(header::CONTENT_ENCODING, HeaderValue::from_static("zstd"));
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    } else if log && ok {
        h.insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    } else if versioned && ok {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    } else {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    res
}

/// Writes `<file>.zst` beside every compressible file under `dir` that has none or an older one
/// (sake.wasm: 8.1 MB, 2.3 MB compressed).
fn precompress(dir: &std::path::Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            precompress(&p);
            continue;
        }
        let ext = p.extension().and_then(|x| x.to_str()).unwrap_or("");
        if !matches!(ext, "wasm" | "js" | "mjs" | "html" | "css" | "json" | "txt" | "svg") || meta.len() < 1024 {
            continue;
        }
        let zst = p.with_file_name(format!("{}.zst", p.file_name().unwrap().to_string_lossy()));
        let fresh = std::fs::metadata(&zst).and_then(|z| Ok(z.modified()? >= meta.modified()?)).unwrap_or(false);
        if fresh {
            continue;
        }
        // (an old one must not be served meanwhile)
        let _ = std::fs::remove_file(&zst);
        let Ok(data) = std::fs::read(&p) else { continue };
        match zstd::encode_all(&data[..], 19) {
            Ok(z) if z.len() < data.len() => {
                let tmp = zst.with_extension("zst.tmp");
                if std::fs::write(&tmp, &z).and_then(|_| std::fs::rename(&tmp, &zst)).is_ok() {
                    eprintln!("vnvm-server: {} {} -> {} bytes", zst.display(), data.len(), z.len());
                }
            }
            _ => {}
        }
    }
}

use axum::serve::ListenerExt;

#[tokio::main]
pub async fn run(store: PathBuf, args: Args) -> anyhow::Result<()> {
    let jit: Jit = Arc::new((args.jit.clone(), tokio::sync::Mutex::new(())));
    let pre = Arc::new(Prerun {
        store: store.clone(),
        web: args.web.clone(),
        jit: args.jit.clone(),
        listen: args.listen,
        off: args.no_prerun,
        lock: Default::default(),
    });
    let logs = Arc::new(args.logs.clone());
    let web = args.web.clone();
    tokio::task::spawn_blocking({
        let web = web.clone();
        move || precompress(&web)
    })
    .await?;
    // (again every few seconds: a deploy that replaces files under a running server)
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(3));
        precompress(&web);
    });
    let app = Router::new()
        .route("/log-save/{name}", put(save_log).with_state(logs.clone()))
        .route("/log-list", get(log_list).with_state(logs))
        .nest_service("/logs", ServeDir::new(&args.logs))
        .route("/jit-save/{name}", put(save_jit))
        .route("/bench-log", put(bench_log))
        .layer(DefaultBodyLimit::max(1 << 30))
        .with_state(jit)
        .merge(saves::router()?)
        .route("/games", get(games).with_state(pre.clone()))
        // (zstd on the way: a game's regions are ~15 MB of wasm, ~2.5 MB compressed, and on a
        // slow connection they held the start back)
        .nest_service(
            "/jit",
            tower::ServiceBuilder::new()
                .layer(tower_http::compression::CompressionLayer::new().no_br().no_gzip().no_deflate())
                .service(ServeDir::new(&args.jit)),
        )
        .nest_service("/store", ServeDir::new(&store))
        .route("/tree/{name}/{*path}", get(crate::treefiles::file).with_state(crate::treefiles::Trees::new(store.clone())))
        .fallback_service(ServeDir::new(&args.web).precompressed_zstd())
        .layer(middleware::from_fn(headers));
    // without TCP_NODELAY a kept-alive connection waits out the peer's delayed ACK (~40ms) on
    // every small response; the page reads the store one chunk at a time
    let listener = tokio::net::TcpListener::bind(args.listen).await?.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    eprintln!("vnvm serve on http://{}", args.listen);
    tokio::spawn(prerun_games(pre));
    axum::serve(listener, app).await?;
    Ok(())
}
