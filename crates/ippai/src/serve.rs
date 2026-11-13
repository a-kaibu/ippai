//! `ippai serve`: the web app and the store made by `ippai pack`.
//!
//! - `/` the built web app (web/dist)
//! - `/store/trees/<name>.tsv`: a packed tree's file list
//! - `/store/chunks/ab/<hash>`: stored zstd-compressed, sent with
//!   `Content-Encoding: zstd` so the browser decodes them natively, and marked
//!   immutable so its HTTP cache keeps them; with `Range`, those bytes of the decoded chunk
//!   (206, as they are: store_api.rs; a game's read needs a few blocks of a chunk)
//! - `?v=...` on any URL: kept by the browser for good (the page changes the version when
//!   the content changes)
//! - `/jit/...`: sent zstd-compressed on the way (CompressionLayer)
//! - `/jit/<name>.parts`: the x86 regions runs of a game saved (koji::jit::saved), one line
//!   per part: module, index, version; `PUT /jit-save/<name>` with (u32 module length, module,
//!   index) adds a run's new regions as a new part (parts compiled before stay the same); `PUT /jit-save/<name>.chunks` stores the store chunks a
//!   run read first (16-byte hashes), which the page fetches ahead next time.
//! - `/jit/_shared-<build>.parts`, `/jit/_shared.chunks`: the regions and chunks several
//!   programs saved (shared libraries and the like; the chunks only for a program with no list
//!   of its own yet): every page loads them too, so a
//!   game's first run starts with them
//! - `GET /tree/<name>/<path>`: one file of a tree as itself (treefiles.rs)
//! - web games (NW.js / Electron pages: web/src/html.ts) run in an iframe from a second
//!   listener (`--game-listen`, the next port by default): another origin than the app's, so their
//!   scripts cannot reach the page, the saves or anything else here. It serves only `/tree/...`
//!   and `/frame.js` (web/src/frame.ts); `GET /game-origin` tells the page where it is
//! - other devices need the server's token (auth.rs); writes (PUT) come only from the app's own
//!   pages (`Sec-Fetch-Site`, `Origin`), with a body limit each
//! - `GET /games`: the launcher's list, the store's `games/<id>.json` (`ippai add-game`) as
//!   one JSON array; covers are under `/store/games/`
//! - pre-runs: at the start and when the launcher lists the games, each listed game with no
//!   saved regions for this build's translator (`web/sake/jit-version.txt`) runs once for a
//!   minute in a headless Chromium (IPPAI_CHROME or one on the PATH; `--no-prerun` for none),
//!   saving its regions like any run: the first real run starts like a second one.
//!   `<jit>/<name>.prerun` marks it done
//! - saved regions are optimized on the server after each save (binaryen wasm-opt, when there
//!   is one: see optimize); `.parts` then lists the optimized copies
//! - `PUT /bench-log`: a result from web/bench.html, printed and appended to
//!   `<jit>/bench-results.txt`
//! - `GET`/`PUT /saves/<name>`: saved games backed up to S3 (see saves.rs)
//! - `PUT /log-save/<name>`: a run's log from the page (web/src/crashlog.ts: crash reports,
//!   sessions a killed tab left, ...), kept as `<logs>/<name>.txt`; `GET /log-list` lists them
//!   (JSON, newest first), `/logs/<name>.txt` serves one
//!
//! - the web app's files are sent zstd-compressed to browsers that accept it: once it listens the
//!   server writes `<file>.zst` beside each script, wasm, page and style sheet of `--web` (again
//!   when the file is newer; an older `.zst` is removed before it listens), and `ServeDir` picks
//!   it when there is one (see `precompress`)
//!
//! Every response carries COOP/COEP: the program's threads (wasm pthreads) need SharedArrayBuffer,
//! which browsers only allow on cross-origin isolated pages.

use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderValue, Request, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, put},
    Router,
};
use crate::saves;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use koji::jit::{saved, wasm};
use tower_http::services::ServeDir;

#[derive(clap::Args)]
pub struct Args {
    /// The web app to serve instead of the one built in.
    #[arg(long)]
    web: Option<PathBuf>,
    /// Saved x86 regions, per game [default: ~/.cache/ippai/jit]
    #[arg(long)]
    jit: Option<PathBuf>,
    /// The runs' logs the pages send (crash reports) [default: ~/.local/state/ippai/logs]
    #[arg(long)]
    pub logs: Option<PathBuf>,
    /// [default: 127.0.0.1:8656, or listen in config.toml]
    #[arg(long)]
    listen: Option<SocketAddr>,
    /// Where web games' files are served, a different origin than the app's [default: the
    /// port after --listen's]
    #[arg(long)]
    game_listen: Option<SocketAddr>,
    /// The URL the browser reaches --game-listen at, when not the app's host with that port
    /// (behind a proxy) [default: the app's scheme and host, --game-listen's port]
    #[arg(long)]
    game_url: Option<String>,
    /// No pre-runs (see prerun_games): a game's first run translates its code itself.
    #[arg(long)]
    no_prerun: bool,
    /// The token from everyone, this machine's browser and `tailscale serve` too (who need
    /// none by default): for a machine with another reverse proxy in front of the server
    /// (see auth.rs) [default: strict_auth in config.toml, else off]
    #[arg(long)]
    strict_auth: bool,
    /// (the default now; kept for scripts that pass it)
    #[arg(long, hide = true)]
    trust_loopback: bool,
    /// Run in this process (for systemd and the like) instead of in the background.
    #[arg(long)]
    pub foreground: bool,
}

/// Whether `req` may change things: reads always; writes not from another site's or origin's
/// page (a web game's, on the game listener: `Sec-Fetch-Site: same-site`), nor, for browsers
/// without that header, with an `Origin` of another host than the request's (`Host`, or the
/// proxy's `X-Forwarded-Host`). Requests with neither (curl, the tests) are let through: who
/// may send any is auth.rs's.
fn same_origin_write<B>(req: &Request<B>) -> bool {
    use axum::http::Method;
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return true;
    }
    let h = req.headers();
    if !h.get("sec-fetch-site").is_none_or(|v| v == "same-origin" || v == "none") {
        return false;
    }
    let Some(origin) = h.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else { return true };
    let Some(from) = origin.split_once("://").map(|x| x.1) else { return false };
    let host = |k: &str| h.get(k).and_then(|v| v.to_str().ok()).map(|v| v.split(',').next().unwrap_or("").trim().to_ascii_lowercase());
    let from = from.to_ascii_lowercase();
    // (an origin leaves out the scheme's default port, a Host header may not)
    let bare = |h: &str| h.strip_suffix(":80").or(h.strip_suffix(":443")).unwrap_or(h).to_string();
    [host("host"), host("x-forwarded-host")].into_iter().flatten().any(|h| bare(&h) == bare(&from))
}

/// The game listener: web games' files and the frame's script, nothing else. Pages there may
/// be framed by the app's (another origin, cross-origin isolated: they need COEP and CORP).
fn games_router(trees: crate::treefiles::Trees, web: &std::path::Path) -> Router {
    async fn game_headers(req: Request<axum::body::Body>, next: Next) -> Response {
        let mut res = next.run(req).await;
        let h = res.headers_mut();
        h.insert("cross-origin-embedder-policy", HeaderValue::from_static("require-corp"));
        h.insert("cross-origin-resource-policy", HeaderValue::from_static("cross-origin"));
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
        res
    }
    Router::new()
        .route("/tree/{name}/{*path}", get(crate::treefiles::file).with_state(trees))
        .route_service("/frame.js", tower_http::services::ServeFile::new(web.join("frame.js")))
        .layer(middleware::from_fn(game_headers))
}

/// The largest run log a page sends (web/src/crashlog.ts keeps its last lines).
const LOG_MAX: usize = 32 << 20;
/// The largest one run's new regions (a whole game's are ~15 MB of wasm).
const JIT_SAVE_MAX: usize = 128 << 20;

/// The jit directory; the lock keeps merges one at a time.
type Jit = Arc<(PathBuf, tokio::sync::Mutex<()>)>;

/// Chunks a list holds at most (16 bytes each): 32768 chunks of 4 MiB on average, 128 GiB.
const CHUNKS_MAX: usize = 32768;

/// A run's chunks list `new` (in the order it read them) with an earlier list `old` of the same
/// name after it: `old`'s chunks `new` lacks, in their order; each chunk once, CHUNKS_MAX at most.
fn merge_chunks(new: &[u8], old: &[u8]) -> Vec<u8> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(new.len() + old.len());
    for c in new.chunks_exact(16).chain(old.chunks_exact(16)) {
        if out.len() >= 16 * CHUNKS_MAX {
            break;
        }
        if seen.insert(c) {
            out.extend_from_slice(c);
        }
    }
    out
}

/// Whose list `file` is, for _shared.chunks's count: a game's (`game.<id>.chunks`, the page's
/// IPPAI_CHUNKS_URL) by its id; a program's (`<program>-<build>.chunks`) by its name without
/// the build (the newest build's list stands for it).
fn list_owner(file: &str) -> String {
    if file.starts_with("game.") {
        return file.to_string();
    }
    file.rsplit_once('-').map_or(file.to_string(), |x| x.0.to_string())
}

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
        // (what earlier runs read and this one did not yet stays, after it: a run that ended at
        // the title does not take away what a longer one read)
        let old = tokio::fs::read(jit.0.join(&name)).await.unwrap_or_default();
        let list = merge_chunks(&body, &old);
        let tmp = jit.0.join(format!("{name}.tmp"));
        if tokio::fs::write(&tmp, &list).await.is_err() || tokio::fs::rename(&tmp, jit.0.join(&name)).await.is_err() {
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
                let prog = list_owner(&f);
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
    // (kept for every page of this build: only modules shaped like the translators', see
    // wasm::check)
    if !wasm::check(new_wasm, new_idx.iter().map(|e| e.width() as usize).sum()) {
        return StatusCode::BAD_REQUEST;
    }
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
/// `wasm-opt -O2`, IPPAI_WASM_OPT, beside the server or on the PATH; nothing without it): each part's module gets
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
    let Some(opt) = std::env::var_os("IPPAI_WASM_OPT").map(PathBuf::from).or(beside).or_else(|| find_on_path(&["wasm-opt"])) else { return };
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
        for (k, (w, e)) in parts.iter().enumerate() {
            let stem = if k == 0 { String::new() } else { format!("{k}.") };
            let v = version(w, &saved::encode(e));
            let o = format!("{name}.{stem}{v}.opt.wasm");
            let wasm = if tokio::fs::metadata(jit.0.join(&o)).await.is_ok() { o } else { format!("{name}.{stem}wasm") };
            keep.insert(wasm.clone());
            list.push_str(&format!("{wasm} {name}.{stem}idx {v}\n"));
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
    add_part_within(dir, name, new_wasm, new_idx, (JIT_MAX_BYTES, JIT_DISK_MAX_BYTES)).await
}

/// [`add_part`] with these limits (modules, disk).
async fn add_part_within(dir: &std::path::Path, name: &str, new_wasm: Vec<u8>, new_idx: Vec<saved::Entry>, max: (u64, u64)) -> StatusCode {
    let path = |ext: &str| dir.join(format!("{name}.{ext}"));
    let mut parts = read_parts(dir, name).await;
    let new_wasm = &new_wasm[..];
    // the new module's functions to keep
    let keep: Vec<bool> = new_idx.iter().map(|e| !parts.iter().any(|(_, es)| es.contains(e))).collect();
    if !keep.iter().any(|&k| k) {
        return StatusCode::NO_CONTENT;
    }
    let fresh = if keep.iter().all(|&k| k) { Some(new_wasm.to_vec()) } else { wasm::select(new_wasm, &saved::keep_funcs(&new_idx, &keep)) };
    let Some(fresh) = fresh else { return StatusCode::BAD_REQUEST };
    // (a game's regions stop growing once its code paths have run; this only bounds a runaway:
    // all the parts' modules, and on the disk with the optimized copies besides)
    let modules: u64 = parts.iter().map(|(w, _)| w.len() as u64).sum::<u64>() + fresh.len() as u64;
    if modules > max.0 || on_disk(dir, name).await + fresh.len() as u64 > max.1 {
        return StatusCode::INSUFFICIENT_STORAGE;
    }
    let fresh_idx: Vec<saved::Entry> = new_idx.into_iter().zip(&keep).filter(|x| *x.1).map(|x| x.0).collect();
    // A save goes in a part of its own: the parts the browser has already compiled stay as
    // they are (and their compiled code in its cache). Past PARTS_MAX they become one again.
    let old_parts = parts.len();
    parts.push((fresh, fresh_idx));
    let merged = parts.len() > PARTS_MAX;
    if merged {
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
            // parts already on the disk stay as they are (the browser has compiled them): only
            // the new one is written, or the one they were merged into
            if merged || k >= old_parts {
                let (tw, ti) = (path(&format!("{stem}wasm.tmp")), path(&format!("{stem}idx.tmp")));
                tokio::fs::write(&tw, w).await?;
                tokio::fs::write(&ti, &idx).await?;
                tokio::fs::rename(&tw, path(&format!("{stem}wasm"))).await?;
                tokio::fs::rename(&ti, path(&format!("{stem}idx"))).await?;
            }
            // (the optimized copy of the module, when there is one: see optimize)
            let v = version(w, &idx);
            let opt = format!("{name}.{stem}{v}.opt.wasm");
            let wasm = if tokio::fs::metadata(dir.join(&opt)).await.is_ok() { opt } else { format!("{name}.{stem}wasm") };
            list.push_str(&format!("{wasm} {name}.{stem}idx {v}\n"));
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

/// A program's saved regions at most: the modules of all its parts, and its files on the disk
/// (the parts, their indexes, the optimized copies). A save past them gets 507 (the page does
/// not send it again: kura jit.rs Ack::Refused).
const JIT_MAX_BYTES: u64 = 256 << 20;
const JIT_DISK_MAX_BYTES: u64 = 512 << 20;

/// The bytes of `name`'s files in `dir` (`<name>.*`).
async fn on_disk(dir: &std::path::Path, name: &str) -> u64 {
    let mut n = 0;
    let prefix = format!("{name}.");
    if let Ok(mut d) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(e)) = d.next_entry().await {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                n += e.metadata().await.map_or(0, |m| m.len());
            }
        }
    }
    n
}

/// A part's version in `<name>.parts` (the browser's `?v=`, so its compiled-code cache, and the
/// optimized copy's name): BLAKE3 of the module and its index, 64 bits in hex. (It was FNV-1a
/// of the index alone: 32 bits, and a module changed under the same index kept its version.)
fn version(wasm: &[u8], idx: &[u8]) -> String {
    let mut h = blake3::Hasher::new();
    h.update(&(wasm.len() as u64).to_le_bytes()).update(wasm).update(idx);
    h.finalize().as_bytes()[..8].iter().map(|b| format!("{b:02x}")).collect()
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
    /// (its Chromium signs in with a ticket)
    auth: Arc<crate::auth::Auth>,
    lock: tokio::sync::Mutex<()>,
}

/// How long a pre-run lasts: past the title, and past the second save of regions (a run saves
/// 20 s after its start and then every 30 s, kura jit.rs maybe_save).
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
        eprintln!("prerun: no Chromium found (set IPPAI_CHROME): {} games start without saved regions", todo.len());
        return;
    };
    let host = if p.listen.ip().is_unspecified() { SocketAddr::from(([127, 0, 0, 1], p.listen.port())) } else { p.listen };
    for (id, name) in todo {
        let profile = p.jit.join(".prerun-profile");
        let _ = tokio::fs::remove_dir_all(&profile).await;
        let page = format!("/play.html?game={id}&autostart&nosave&headless");
        let next: String = form_urlencoded::byte_serialize(page.as_bytes()).collect();
        let url = format!("http://{host}/login?ticket={}&next={next}", p.auth.ticket(120));
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
        #[cfg(unix)]
        if std::fs::metadata("/proc/self").is_ok_and(|m| std::os::unix::fs::MetadataExt::uid(&m) == 0) {
            cmd.arg("--no-sandbox");
        }
        eprintln!("prerun {id}: {page} for {PRERUN_SECS} s");
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

/// The browser for pre-runs: IPPAI_CHROME, else the first Chromium or Chrome on the PATH.
fn chrome() -> Option<PathBuf> {
    if let Some(c) = std::env::var_os("IPPAI_CHROME") {
        return Some(c.into());
    }
    find_on_path(&["chromium", "chromium-browser", "google-chrome", "google-chrome-stable", "chrome"])
}

/// The store's `games/*.json` as one array, by file name; and a pre-run of the games without
/// saved regions (one added since the start: before it is clicked, usually).
/// What `ippai --version` prints: the version, and the commit built when known.
pub const VERSION_LINE: &str = if env!("IPPAI_COMMIT").is_empty() {
    env!("IPPAI_VERSION")
} else {
    concat!(env!("IPPAI_VERSION"), " (", env!("IPPAI_COMMIT"), ")")
};

/// `GET /version`: this binary's version and commit (the list page shows them by its name).
fn version_info() -> serde_json::Value {
    serde_json::json!({ "version": env!("IPPAI_VERSION"), "commit": env!("IPPAI_COMMIT") })
}

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
            let text = match serde_json::from_str::<serde_json::Value>(&text) {
                Ok(mut game) => {
                    if let Ok(db) = crate::library::Library::new(store) {
                        if let Err(e) = db.apply_settings(&mut game) { eprintln!("game settings: {e}"); }
                    }
                    game.to_string()
                }
                Err(_) => continue,
            };
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

async fn headers(mut req: Request<axum::body::Body>, next: Next) -> Response {
    if !same_origin_write(&req) {
        return (StatusCode::FORBIDDEN, "writes come from this server's own pages").into_response();
    }
    // Build identity must not be revalidated by mtime: extracted artifacts and
    // rollbacks can have equal/older timestamps but incompatible Wasm contents.
    // These tiny manifests are read at startup only; keep compiled Wasm caching.
    let build_identity = matches!(req.uri().path(), "/sake/build-id.txt" | "/sake/jit-version.txt");
    if build_identity {
        req.headers_mut().remove(header::IF_MODIFIED_SINCE);
        req.headers_mut().remove(header::IF_NONE_MATCH);
    }
    let chunk = req.uri().path().starts_with("/store/chunks/");
    let log = req.uri().path().starts_with("/logs/");
    // a versioned URL (?v=...) names content that never changes: the browser keeps it without
    // asking again, and so keeps its compiled code (a revalidated .wasm loses it)
    let versioned = req.uri().query().is_some_and(|q| q.split('&').any(|p| p.starts_with("v=")));
    let mut res = next.run(req).await;
    let status = res.status();
    let ok = status.is_success();
    let h = res.headers_mut();
    h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
    h.insert("cross-origin-embedder-policy", HeaderValue::from_static("require-corp"));
    h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
    // (a range of a chunk, 206, is of its decoded bytes: store_api.rs)
    if chunk && ok {
        if status == StatusCode::OK {
            h.insert(header::CONTENT_ENCODING, HeaderValue::from_static("zstd"));
        }
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
/// (sake.wasm: 8.1 MB, 2.3 MB compressed). With `write` false it only removes the older ones:
/// the server does that before it listens (nothing stale is ever served) and compresses after,
/// in the background: level 19 over a web app with several sake builds took more than the
/// 10 s tests/winetest/run-browser.mjs waits for the server (winetest-gpu, run 42445713214).
fn precompress(dir: &std::path::Path, write: bool) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(meta) = e.metadata() else { continue };
        if meta.is_dir() {
            precompress(&p, write);
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
        if !write {
            continue;
        }
        let Ok(data) = std::fs::read(&p) else { continue };
        match zstd::encode_all(&data[..], 19) {
            Ok(z) if z.len() < data.len() => {
                let tmp = zst.with_extension("zst.tmp");
                if std::fs::write(&tmp, &z).and_then(|_| std::fs::rename(&tmp, &zst)).is_ok() {
                    eprintln!("ippai: {} {} -> {} bytes", zst.display(), data.len(), z.len());
                }
            }
            _ => {}
        }
    }
}

use axum::serve::ListenerExt;

struct Resolved {
    web: PathBuf,
    jit: PathBuf,
    logs: PathBuf,
    listen: SocketAddr,
    game_listen: SocketAddr,
    game_url: Option<String>,
    no_prerun: bool,
    strict_auth: bool,
}

pub fn run(dirs: &crate::dirs::Dirs, a: Args) -> anyhow::Result<()> {
    let web = match a.web {
        Some(w) => w,
        None => crate::webassets::unpack(&dirs.cache)?,
    };
    let args = Resolved {
        web,
        jit: a.jit.unwrap_or_else(|| dirs.cache.join("jit")),
        logs: a.logs.unwrap_or_else(|| dirs.logs.clone()),
        listen: a.listen.unwrap_or(dirs.listen),
        game_listen: a.game_listen.unwrap_or_else(|| {
            let l = a.listen.unwrap_or(dirs.listen);
            SocketAddr::new(l.ip(), l.port().wrapping_add(1))
        }),
        game_url: a.game_url,
        no_prerun: a.no_prerun,
        strict_auth: (a.strict_auth || dirs.strict_auth) && !a.trust_loopback,
    };
    for d in [&dirs.store, &args.jit, &args.logs] {
        std::fs::create_dir_all(d)?;
    }
    match crate::pack::migrate_guest_paths(&dirs.store, &args.jit) {
        Ok(0) => {}
        Ok(n) => eprintln!("{n} games and saved regions moved from Z:\\vnvm to Z:\\ippai"),
        Err(e) => eprintln!("moving games from Z:\\vnvm to Z:\\ippai: {e:#}"),
    }
    if crate::pack::ensure_prefix(&dirs.store, &dirs.fonts)? {
        eprintln!("sake's C:\\ ({}) made from {}", crate::pack::PREFIX_TREE, dirs.fonts.display());
    }
    serve(dirs, dirs.store.clone(), args)
}

/// Removes the kept web apps no state needs (apps.rs prune; the states' builds from their
/// indexes, `state.<game>.index` saves).
async fn prune_apps(dir: PathBuf, current: Option<String>, saves: Arc<saves::Saves>) {
    let names: Vec<String> = match saves.db.connect().and_then(|c| {
        let mut q = c.prepare("SELECT name FROM saves WHERE name LIKE 'state.%.index'")?;
        let v = q.query_map([], |r| r.get(0))?.collect::<Result<Vec<String>, _>>()?;
        Ok(v)
    }) {
        Ok(v) => v,
        Err(e) => return eprintln!("apps: not pruned: {e}"),
    };
    let mut used = std::collections::HashSet::new();
    for n in names {
        match saves.read(&n).await {
            Ok(Some((_, data))) => crate::apps::state_builds(&data, &mut used),
            Ok(None) => {}
            // (an index out of reach: nothing goes)
            Err(e) => return eprintln!("apps: not pruned: {e}"),
        }
    }
    let _ = tokio::task::spawn_blocking(move || crate::apps::prune(&dir, &used, current.as_deref())).await;
}

/// How often [`watchdog`] looks, and how long the runtime may run no task before it is taken
/// for stuck. 2026-11-09 on worker2: every worker thread waited on a futex (none in epoll), 109
/// connections stood unaccepted and S3 answers unread, until the server was restarted by hand.
const WATCH_EVERY: std::time::Duration = std::time::Duration::from_secs(5);
const HANG_LIMIT: std::time::Duration = std::time::Duration::from_secs(60);

/// A thread of its own that hands the runtime a task every `every` and calls `on_hang` (once)
/// when none of them ran for `limit`: the runtime runs nothing at all (its threads blocked).
fn watchdog(rt: tokio::runtime::Handle, every: std::time::Duration, limit: std::time::Duration, on_hang: impl FnOnce() + Send + 'static) {
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    let t0 = std::time::Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    std::thread::spawn(move || loop {
        std::thread::sleep(every);
        let l = last.clone();
        rt.spawn(async move { l.store(t0.elapsed().as_millis() as u64, Relaxed) });
        let since = t0.elapsed().saturating_sub(std::time::Duration::from_millis(last.load(Relaxed)));
        if since > limit + every {
            on_hang();
            return;
        }
    });
}

/// Every thread of the process says where it is, on stderr (the journal): what a stuck server
/// waits on. Each thread, from a signal handler, only takes its return addresses (glibc's
/// backtrace into memory set aside: no allocation, no lock a stuck thread may hold; worker2's
/// first dumps, std's Backtrace in the handler, showed every thread in pthread_mutex_lock under
/// frames it could not name). This thread then prints them as offsets in their files (for
/// `addr2line -f -C -i -e ippai`, the release build keeps its line tables) and, where
/// addr2line is installed, with the functions and lines themselves.
#[cfg(target_os = "linux")]
fn dump_threads() {
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering::SeqCst};
    const DEPTH: usize = 64;
    const SLOTS: usize = 512;
    static IPS: [[AtomicUsize; DEPTH]; SLOTS] = [const { [const { AtomicUsize::new(0) }; DEPTH] }; SLOTS];
    static LENS: [AtomicUsize; SLOTS] = [const { AtomicUsize::new(0) }; SLOTS];
    static TIDS: [AtomicI64; SLOTS] = [const { AtomicI64::new(0) }; SLOTS];
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    extern "C" {
        fn backtrace(buf: *mut *mut libc::c_void, size: libc::c_int) -> libc::c_int;
    }
    extern "C" fn on_signal(_: libc::c_int) {
        let i = NEXT.fetch_add(1, SeqCst);
        if i >= SLOTS {
            return;
        }
        let mut buf = [std::ptr::null_mut::<libc::c_void>(); DEPTH];
        let n = unsafe { backtrace(buf.as_mut_ptr(), DEPTH as libc::c_int) }.max(0) as usize;
        for (k, ip) in buf[..n].iter().enumerate() {
            IPS[i][k].store(*ip as usize, SeqCst);
        }
        TIDS[i].store(unsafe { libc::syscall(libc::SYS_gettid) }, SeqCst);
        // (last: the slot is whole)
        LENS[i].store(n.max(1), SeqCst);
    }
    // (the first call loads libgcc's unwinder: here, not in a handler)
    let mut warm = [std::ptr::null_mut::<libc::c_void>(); 4];
    unsafe { backtrace(warm.as_mut_ptr(), 4) };
    unsafe { libc::signal(libc::SIGUSR2, on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t) };
    let me = unsafe { libc::syscall(libc::SYS_gettid) };
    let pid = std::process::id() as libc::c_long;
    let mut asked = 0;
    for e in std::fs::read_dir("/proc/self/task").into_iter().flatten().flatten() {
        let Some(tid) = e.file_name().to_str().and_then(|t| t.parse::<libc::c_long>().ok()) else { continue };
        if tid != me && asked < SLOTS {
            unsafe { libc::syscall(libc::SYS_tgkill, pid, tid, libc::SIGUSR2) };
            asked += 1;
        }
    }
    // (a thread stuck with signals blocked does not answer)
    let t = std::time::Instant::now();
    while (0..asked).any(|i| LENS[i].load(SeqCst) == 0) && t.elapsed() < std::time::Duration::from_secs(2) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let maps = std::fs::read_to_string("/proc/self/maps").unwrap_or_default();
    let exe = std::env::current_exe().ok().map(|p| p.display().to_string()).unwrap_or_default();
    eprint!("{}", describe_threads(&maps, &exe, (0..asked.min(NEXT.load(SeqCst)).min(SLOTS)).filter(|&i| LENS[i].load(SeqCst) > 0).map(|i| {
        let tid = TIDS[i].load(SeqCst);
        let name = std::fs::read_to_string(format!("/proc/self/task/{tid}/comm")).unwrap_or_default().trim().to_string();
        let ips: Vec<usize> = (0..LENS[i].load(SeqCst).min(DEPTH)).map(|k| IPS[i][k].load(SeqCst)).collect();
        (tid, name, ips)
    }).collect(), true));
}

/// What [`dump_threads`] prints for `threads` (tid, name, return addresses): each frame as
/// `<file>+0x<offset>` from `maps` (/proc/self/maps), threads with the same frames once with
/// their count, and, when `symbols` and addr2line is there, each frame of `exe` named.
#[cfg(target_os = "linux")]
fn describe_threads(maps: &str, exe: &str, threads: Vec<(i64, String, Vec<usize>)>, symbols: bool) -> String {
    use std::fmt::Write;
    // (start, end, file offset, path) of each mapped file
    let regions: Vec<(usize, usize, usize, &str)> = maps
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let (a, b) = f.first()?.split_once('-')?;
            Some((usize::from_str_radix(a, 16).ok()?, usize::from_str_radix(b, 16).ok()?, usize::from_str_radix(f.get(2)?, 16).ok()?, *f.get(5)?))
        })
        .collect();
    // a file's load address: its mapping at offset 0
    let base = |path: &str| regions.iter().filter(|r| r.3 == path).map(|r| r.0 - r.2).min();
    let place = |ip: usize| -> Option<(&str, usize)> {
        let r = regions.iter().find(|r| ip >= r.0 && ip < r.1)?;
        Some((r.3, ip - base(r.3)?))
    };
    let mut out = String::new();
    let mut groups: Vec<(Vec<usize>, Vec<(i64, String)>)> = Vec::new();
    for (tid, name, ips) in threads {
        match groups.iter_mut().find(|g| g.0 == ips) {
            Some(g) => g.1.push((tid, name)),
            None => groups.push((ips, vec![(tid, name)])),
        }
    }
    for (ips, who) in groups {
        let names: Vec<String> = who.iter().map(|(t, n)| format!("{t} {n}")).collect();
        let _ = writeln!(out, "ippai: thread{} {} ({} frames):", if who.len() > 1 { format!("s (x{})", who.len()) } else { String::new() }, names.join(", "), ips.len());
        // (a return address is just after its call: the call's line is at ip - 1)
        let frames: Vec<(usize, Option<(&str, usize)>)> = ips.iter().enumerate().map(|(k, &ip)| (ip, place(if k > 0 { ip.saturating_sub(1) } else { ip }))).collect();
        let named = if symbols { addr2line(exe, frames.iter().filter_map(|f| f.1.filter(|p| p.0 == exe).map(|p| p.1)).collect()) } else { Default::default() };
        for (k, (ip, at)) in frames.iter().enumerate() {
            let _ = match at {
                Some((file, off)) => {
                    let short = file.rsplit('/').next().unwrap_or(file);
                    let name = named.get(off).map(|n| format!("  {n}")).unwrap_or_default();
                    writeln!(out, "  {k:2}: {short}+{off:#x}{name}")
                }
                None => writeln!(out, "  {k:2}: {ip:#x}"),
            };
        }
    }
    out
}

/// addr2line's names for `offsets` in `exe` ("function at file:line", the inlined ones
/// joined with " <- "); empty when it is not installed or the file has no symbols.
#[cfg(target_os = "linux")]
fn addr2line(exe: &str, offsets: Vec<usize>) -> std::collections::HashMap<usize, String> {
    let mut out = std::collections::HashMap::new();
    if offsets.is_empty() {
        return out;
    }
    let args: Vec<String> = offsets.iter().map(|o| format!("{o:#x}")).collect();
    let Ok(r) = std::process::Command::new("addr2line").args(["-f", "-C", "-i", "-a", "-e", exe]).args(&args).output() else { return out };
    let text = String::from_utf8_lossy(&r.stdout);
    // (-a: each address on a line of its own, then function / place pairs, more when inlined)
    let mut cur: Option<usize> = None;
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        if let Some(a) = l.strip_prefix("0x").and_then(|h| usize::from_str_radix(h, 16).ok()) {
            cur = Some(a);
            continue;
        }
        let Some(a) = cur else { continue };
        let at = lines.next().unwrap_or("??:0");
        if l == "??" {
            continue;
        }
        let at = at.rsplit('/').next().unwrap_or(at);
        let e = out.entry(a).or_insert_with(String::new);
        if !e.is_empty() {
            e.push_str(" <- ");
        }
        let _ = std::fmt::Write::write_fmt(e, format_args!("{l} at {at}"));
    }
    out
}
#[cfg(not(target_os = "linux"))]
fn dump_threads() {}

#[tokio::main]
async fn serve(dirs: &crate::dirs::Dirs, store: PathBuf, args: Resolved) -> anyhow::Result<()> {
    // (before anything can replace it: /api/update)
    let exe = std::env::current_exe()?;
    if let Some(storage) = crate::storage::Storage::configured(crate::library::Library::new(&store)?)? {
        storage.hydrate_catalog(&store).await?;
    }
    let auth = crate::auth::Auth::load(&dirs.token, args.strict_auth)?;
    let jit: Jit = Arc::new((args.jit.clone(), tokio::sync::Mutex::new(())));
    let pre = Arc::new(Prerun {
        store: store.clone(),
        web: args.web.clone(),
        jit: args.jit.clone(),
        listen: args.listen,
        off: args.no_prerun,
        auth: auth.clone(),
        lock: Default::default(),
    });
    let logs = Arc::new(args.logs.clone());
    // (a server that stops answering ends, for systemd to start it again: see `watchdog`)
    watchdog(tokio::runtime::Handle::current(), WATCH_EVERY, HANG_LIMIT, || {
        eprintln!("ippai: the server answered nothing for {} s (its threads stuck): where each thread is, then ending it, to be started again", HANG_LIMIT.as_secs());
        dump_threads();
        std::thread::sleep(std::time::Duration::from_secs(3));
        std::process::abort()
    });
    let web = args.web.clone();
    precompress(&web, false);
    // (then again every few seconds: a deploy that replaces files under a running server)
    std::thread::spawn(move || loop {
        precompress(&web, true);
        std::thread::sleep(std::time::Duration::from_secs(3));
    });
    let saves = saves::Saves::new(&store)?;
    let trees = crate::treefiles::Trees::new(store.clone())?;
    let game_origin = serde_json::json!({ "port": args.game_listen.port(), "url": args.game_url });
    let inner = Router::new()
        // (each write its own limit: a run's log, one run's new regions, a result line; the
        // rest axum's 2 MB, saves and packages their own)
        .route("/log-save/{name}", put(save_log).layer(DefaultBodyLimit::max(LOG_MAX)).with_state(logs.clone()))
        .route("/log-list", get(log_list).with_state(logs))
        .nest_service("/logs", ServeDir::new(&args.logs))
        .route("/jit-save/{name}", put(save_jit).layer(DefaultBodyLimit::max(JIT_SAVE_MAX)))
        .route("/bench-log", put(bench_log).layer(DefaultBodyLimit::max(1 << 20)))
        .with_state(jit)
        .merge(crate::tls_relay::router())
        .merge(crate::library_api::router(crate::library::Library::new(&store)?))
        .merge(crate::update::router(exe, dirs.fonts.clone()))
        .merge(saves::router(saves.clone()))
        .merge(crate::portable::router(store.clone(), saves.clone()))
        .route("/games", get(games).with_state(pre.clone()))
        // (zstd on the way: a game's regions are ~15 MB of wasm, ~2.5 MB compressed, and on a
        // slow connection they held the start back)
        .nest_service(
            "/jit",
            tower::ServiceBuilder::new()
                .layer(tower_http::compression::CompressionLayer::new().no_br().no_gzip().no_deflate())
                .service(ServeDir::new(&args.jit)),
        )
        .merge(crate::store_api::router(store.clone())?)
        .route("/tree/{name}/{*path}", get(crate::treefiles::file).with_state(trees.clone()))
        .route("/game-origin", get(move || async move { axum::Json(game_origin) }))
        .route("/version", get(|| async { axum::Json(version_info()) }))
        .fallback_service(ServeDir::new(&args.web).precompressed_zstd())
        .layer(middleware::from_fn(headers));
    // this web app kept for the states it takes, older ones while states need them (apps.rs)
    let apps_dir = store.join("apps");
    std::fs::create_dir_all(&apps_dir)?;
    let apps_dir_prune = apps_dir.clone();
    let current = crate::apps::keep(&apps_dir, &args.web).unwrap_or_else(|e| {
        eprintln!("apps: this web app not kept: {e}");
        None
    });
    tokio::spawn({
        let saves = saves.clone();
        async move {
            // (the states' files no index names first: then the apps only they were of go too)
            match saves.prune_states(saves::STATE_GRACE).await {
                Ok(0) => {}
                Ok(n) => eprintln!("saves: {n} state files no index names deleted"),
                Err(e) => eprintln!("saves: state files not pruned: {e:#}"),
            }
            prune_apps(apps_dir_prune, current, saves).await
        }
    });
    let app = Router::new()
        .route("/apps/builds", get(crate::apps::list))
        .route("/apps/{key}/{*path}", axum::routing::any(crate::apps::file))
        .with_state(crate::apps::Apps { dir: apps_dir, root: inner.clone() })
        .fallback_service(inner)
        .layer(middleware::from_fn_with_state(auth.clone(), crate::auth::guard));
    let games = games_router(trees, &args.web).layer(middleware::from_fn_with_state(auth, crate::auth::guard));
    // without TCP_NODELAY a kept-alive connection waits out the peer's delayed ACK (~40ms) on
    // every small response; the page reads the store one chunk at a time
    let listener = tokio::net::TcpListener::bind(args.listen).await?.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    let game_listener = tokio::net::TcpListener::bind(args.game_listen).await?.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    let url = format!("http://{}", args.listen);
    // (the control socket for `ippai stop` and `status`: Linux and macOS)
    #[cfg(unix)]
    crate::daemon::listen(dirs, url.clone(), store.display().to_string()).await?;
    eprintln!(
        "ippai {} on {url}, web games on http://{} (store {}, web {}; {}sign in with `ippai token`)",
        env!("IPPAI_VERSION"),
        args.game_listen,
        store.display(),
        args.web.display(),
        if args.strict_auth { "strict: everyone " } else { "this machine and tailscale serve need no token, others " }
    );
    tokio::spawn(prerun_games(pre));
    use std::future::IntoFuture;
    tokio::try_join!(
        axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).into_future(),
        axum::serve(game_listener, games.into_make_service_with_connect_info::<SocketAddr>()).into_future()
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// What a stuck server prints: each thread's backtrace, a thread blocked on a lock among
    /// them with the function it waits in (run in a child process: its stderr is looked at).
    #[cfg(target_os = "linux")]
    #[test]
    fn stuck_threads_print_where_they_wait() {
        if std::env::var_os("IPPAI_DUMP_CHILD").is_some() {
            let lock = std::sync::Arc::new(std::sync::Mutex::new(()));
            let held = lock.lock().unwrap();
            let l = lock.clone();
            std::thread::Builder::new()
                .name("stuck-one".into())
                .spawn(move || {
                    #[inline(never)]
                    fn waits_for_the_lock(l: &std::sync::Mutex<()>) {
                        let _g = l.lock();
                    }
                    waits_for_the_lock(&l)
                })
                .unwrap();
            std::thread::sleep(std::time::Duration::from_millis(200));
            super::dump_threads();
            std::thread::sleep(std::time::Duration::from_millis(500));
            drop(held);
            return;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "serve::tests::stuck_threads_print_where_they_wait", "--nocapture", "--test-threads", "1"])
            .env("IPPAI_DUMP_CHILD", "1")
            .env("RUST_BACKTRACE", "1")
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{err}");
        let stuck = err.split("ippai: thread").find(|t| t.contains("stuck-one")).unwrap_or_else(|| panic!("no stuck thread in:\n{err}"));
        // (offsets in the test binary, named by addr2line where it is installed)
        assert!(stuck.contains("+0x"), "{stuck}");
        if std::process::Command::new("addr2line").arg("--version").output().is_ok() {
            assert!(stuck.contains("waits_for_the_lock"), "{stuck}");
        }
    }

    /// Frames as offsets in their files (a PIE's from its mapping at offset 0, a library's
    /// too), threads with the same frames once with their count, an address in no file as is.
    #[cfg(target_os = "linux")]
    #[test]
    fn stuck_threads_are_described_by_file_and_offset() {
        let maps = "\
55550000-55551000 r--p 00000000 08:01 1 /home/k/.local/bin/ippai
55551000-55560000 r-xp 00001000 08:01 1 /home/k/.local/bin/ippai
7f000000-7f001000 r--p 00000000 08:01 2 /usr/lib/libc.so.6
7f001000-7f100000 r-xp 00001000 08:01 2 /usr/lib/libc.so.6
";
        let exe = "/home/k/.local/bin/ippai";
        let a = vec![0x7f001234, 0x55552010, 0x99];
        let threads = (1..=12).map(|t| (t, "tokio-rt-worker".to_string(), a.clone())).chain([(13, "main".to_string(), vec![0x55553000])]).collect();
        let d = super::describe_threads(maps, exe, threads, false);
        assert!(d.starts_with("ippai: threads (x12) 1 tokio-rt-worker, 2 tokio-rt-worker"), "{d}");
        assert!(d.contains("   0: libc.so.6+0x1234\n"), "{d}");
        // (a return address: the call before it)
        assert!(d.contains("   1: ippai+0x200f\n"), "{d}");
        assert!(d.contains("   2: 0x99\n"), "{d}");
        assert!(d.contains("ippai: thread 13 main (1 frames):\n   0: ippai+0x3000\n"), "{d}");
        assert_eq!(d.matches("ippai: thread").count(), 2, "{d}");
    }

    /// A runtime whose every thread is blocked (as worker2's was) is noticed, and once; one
    /// that is busy but runs its tasks is not.
    #[test]
    fn watchdog_notices_a_stuck_runtime() {
        use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
        use std::time::Duration;
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        use std::sync::Arc;
        let hangs = Arc::new(AtomicU32::new(0));
        let h = hangs.clone();
        super::watchdog(rt.handle().clone(), Duration::from_millis(20), Duration::from_millis(200), move || {
            h.fetch_add(1, SeqCst);
        });
        // busy with tasks that await: fine
        rt.block_on(async {
            for _ in 0..20 {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
        });
        assert_eq!(hangs.load(SeqCst), 0, "a runtime that runs its tasks");
        // every worker blocked on a lock nobody lets go of
        let lock = Arc::new(std::sync::Mutex::new(()));
        let held = lock.lock().unwrap();
        for _ in 0..2 {
            let l = lock.clone();
            rt.spawn(async move {
                let _g = l.lock();
            });
        }
        let t = std::time::Instant::now();
        while hangs.load(SeqCst) == 0 && t.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(hangs.load(SeqCst), 1, "noticed");
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(hangs.load(SeqCst), 1, "once");
        drop(held);
        rt.shutdown_timeout(Duration::from_secs(1));
    }

    use axum::http::Request;
    use koji::jit::{saved, wasm};

    /// A module shaped like the translators' with `n` functions (`seed` makes it another).
    fn module(n: usize, seed: i32) -> Vec<u8> {
        let mut m = wasm::Module::default();
        m.types.push(wasm::FuncType { params: vec![wasm::I32], results: vec![wasm::I32] });
        m.imports.push(wasm::Import::Memory);
        m.imports.push(wasm::Import::Func("step", 0));
        m.imports.push(wasm::Import::Table("table"));
        for k in 0..n {
            let mut code = wasm::Code::default();
            code.get(0).i32c(seed * 1000 + k as i32).op(0x6a);
            m.funcs.push(wasm::Func { ty: 0, locals: vec![], code });
        }
        m.finish()
    }

    /// `n` regions from `base` (one function each).
    fn regions(n: u32, base: u32) -> Vec<saved::Entry> {
        (0..n)
            .map(|i| saved::Entry { roots: vec![base + i * 16], starts: vec![(base + i * 16, 0)], code: vec![(base + i * 16, 4)], text: (base + i).to_le_bytes().to_vec(), build: 0, sites: vec![] })
            .collect()
    }

    /// GET /version names the commit built (a checkout: here and in CI), and `--version` too.
    #[test]
    fn version_names_the_commit() {
        let v = super::version_info();
        assert_eq!(v["version"], env!("IPPAI_VERSION"));
        let c = v["commit"].as_str().unwrap();
        assert!(c.len() == 7 && c.bytes().all(|b| b.is_ascii_hexdigit()), "{c:?}");
        assert_eq!(super::VERSION_LINE, format!("{} ({c})", env!("IPPAI_VERSION")));
    }

    /// Loopback needs no token since 0f274c4, so the repository's own scripts start
    /// `ippai serve` without --trust-loopback (a hidden no-op kept for old scripts; only the
    /// bench's comparison of older builds may still pass it, to those that know it)
    #[test]
    fn scripts_need_no_trust_loopback() {
        let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
        let mut stack = vec![std::path::PathBuf::from(root).join("tests"), std::path::PathBuf::from(root).join("tools"), std::path::PathBuf::from(root).join(".github")];
        let mut seen = 0;
        while let Some(d) = stack.pop() {
            for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
                let p = e.path();
                if p.is_dir() {
                    if !p.ends_with("node_modules") && !p.ends_with("golden") {
                        stack.push(p);
                    }
                    continue;
                }
                if !matches!(p.extension().and_then(|x| x.to_str()), Some("mjs" | "js" | "sh" | "yml" | "py")) {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&p) else { continue };
                seen += 1;
                for l in text.lines().filter(|l| l.contains("--trust-loopback")) {
                    assert!(p.ends_with("tests/bench/ci.sh") && l.contains("serve --help"), "{}: {l}", p.display());
                }
            }
        }
        assert!(seen > 10);
        // and the flag still parses (old scripts) without changing anything
        #[derive(clap::Parser)]
        struct Cli {
            #[command(flatten)]
            a: super::Args,
        }
        let a = <Cli as clap::Parser>::try_parse_from(["serve", "--trust-loopback"]).unwrap();
        assert!(a.a.trust_loopback);
        let b = <Cli as clap::Parser>::try_parse_from(["serve"]).unwrap();
        assert!(!b.a.trust_loopback && !b.a.strict_auth);
    }

    fn scratch(what: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ippai-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn ino(p: std::path::PathBuf) -> (u64, std::time::SystemTime) {
        let m = std::fs::metadata(p).unwrap();
        (std::os::unix::fs::MetadataExt::ino(&m), m.modified().unwrap())
    }

    #[tokio::test]
    async fn a_new_part_leaves_the_others_on_the_disk_as_they_are() {
        let d = scratch("parts");
        let n = "sake-x-0";
        let add = |k: u32| super::add_part(&d, n, module(4, k as i32), regions(4, 0x1000 * (k + 1)));
        assert_eq!(add(0).await, axum::http::StatusCode::NO_CONTENT);
        assert_eq!(add(1).await, axum::http::StatusCode::NO_CONTENT);
        let before = [ino(d.join(format!("{n}.wasm"))), ino(d.join(format!("{n}.idx"))), ino(d.join(format!("{n}.1.wasm"))), ino(d.join(format!("{n}.1.idx")))];
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(add(2).await, axum::http::StatusCode::NO_CONTENT);
        let after = [ino(d.join(format!("{n}.wasm"))), ino(d.join(format!("{n}.idx"))), ino(d.join(format!("{n}.1.wasm"))), ino(d.join(format!("{n}.1.idx")))];
        assert_eq!(before, after, "parts 0 and 1 not written again");
        assert!(d.join(format!("{n}.2.wasm")).is_file());
        let list = std::fs::read_to_string(d.join(format!("{n}.parts"))).unwrap();
        assert_eq!(list.lines().count(), 3, "{list}");
        // the same regions again: nothing
        assert_eq!(add(2).await, axum::http::StatusCode::NO_CONTENT);
        assert!(!d.join(format!("{n}.3.wasm")).exists());
        // past PARTS_MAX they become one, the others' files gone
        for k in 3..=super::PARTS_MAX as u32 {
            assert_eq!(add(k).await, axum::http::StatusCode::NO_CONTENT);
        }
        let parts = super::read_parts(&d, n).await;
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].1.len(), 4 * (super::PARTS_MAX + 1));
        assert!(wasm::check(&parts[0].0, 4 * (super::PARTS_MAX + 1)));
        assert!(!d.join(format!("{n}.1.wasm")).exists() && !d.join(format!("{n}.1.idx")).exists());
        assert_eq!(std::fs::read_to_string(d.join(format!("{n}.parts"))).unwrap().lines().count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A run's chunks list goes first, an earlier one's chunks it lacks after (a run that ended
    /// at the title keeps what a longer one read); each chunk once, CHUNKS_MAX at most.
    #[test]
    fn chunk_lists_merge_new_first() {
        let c = |k: u8| [k; 16];
        let l = |ks: &[u8]| ks.iter().flat_map(|&k| c(k)).collect::<Vec<u8>>();
        assert_eq!(super::merge_chunks(&l(&[3, 1]), &l(&[1, 2, 3, 4])), l(&[3, 1, 2, 4]));
        assert_eq!(super::merge_chunks(&l(&[]), &l(&[1, 2])), l(&[1, 2]));
        assert_eq!(super::merge_chunks(&l(&[5, 5, 6]), &[]), l(&[5, 6]));
        // random lists: new is a prefix, then old's others in order, nothing twice, the cap
        let mut seed = 11u32;
        let mut rnd = |n: u32| {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 8) % n
        };
        for _ in 0..300 {
            let new: Vec<u8> = (0..rnd(30)).map(|_| rnd(40) as u8).collect();
            let old: Vec<u8> = (0..rnd(30)).map(|_| rnd(40) as u8).collect();
            let got: Vec<u8> = super::merge_chunks(&l(&new), &l(&old)).chunks_exact(16).map(|x| x[0]).collect();
            let mut want = Vec::new();
            for k in new.iter().chain(&old) {
                if !want.contains(k) {
                    want.push(*k);
                }
            }
            assert_eq!(got, want);
        }
        let many: Vec<u8> = (0..super::CHUNKS_MAX as u32 + 10).flat_map(|i| {
            let mut b = [0u8; 16];
            b[..4].copy_from_slice(&i.to_le_bytes());
            b
        }).collect();
        assert_eq!(super::merge_chunks(&many, &[]).len(), 16 * super::CHUNKS_MAX);
    }

    /// PUT /jit-save/<name>.chunks: merged with what was there; the games' lists
    /// (game.<id>.chunks) count one each for _shared.chunks, as programs' lists do.
    #[tokio::test]
    async fn chunk_lists_are_saved_merged_and_shared_by_owner() {
        use axum::extract::{Path, State};
        let d = scratch("chunklists");
        let jit: super::Jit = std::sync::Arc::new((d.clone(), tokio::sync::Mutex::new(())));
        let put = |name: &str, ks: &[u8]| {
            let body: Vec<u8> = ks.iter().flat_map(|&k| [k; 16]).collect();
            super::save_jit(State(jit.clone()), Path(name.to_string()), body.into())
        };
        let read = |name: &str| std::fs::read(d.join(name)).unwrap().chunks_exact(16).map(|x| x[0]).collect::<Vec<u8>>();
        assert_eq!(put("game.saga_planets-2.chunks", &[1, 2, 3]).await, axum::http::StatusCode::NO_CONTENT);
        assert_eq!(put("game.saga_planets-2.chunks", &[4, 1]).await, axum::http::StatusCode::NO_CONTENT);
        assert_eq!(read("game.saga_planets-2.chunks"), [4, 1, 2, 3]);
        // another game of the same program: a list of its own, and what both read is shared
        assert_eq!(put("game.saga_planets-3.chunks", &[9, 1]).await, axum::http::StatusCode::NO_CONTENT);
        assert_eq!(read("game.saga_planets-3.chunks"), [9, 1]);
        assert_eq!(read("_shared.chunks"), [1]);
        assert_eq!(super::list_owner("game.saga_planets-2.chunks"), "game.saga_planets-2.chunks");
        assert_eq!(super::list_owner("sake-Z__ippai_game_SiglusEngine.exe-24d68ccbg.chunks"), "sake-Z__ippai_game_SiglusEngine.exe");
        assert_eq!(put("game.x.chunks", &[]).await, axum::http::StatusCode::NO_CONTENT);
        assert_eq!(super::save_jit(State(jit.clone()), Path("game.x.chunks".into()), vec![0u8; 15].into()).await, axum::http::StatusCode::BAD_REQUEST);
        assert!(!d.join("game.saga_planets-2.chunks.tmp").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[tokio::test]
    async fn the_limit_is_all_the_parts() {
        let d = scratch("limit");
        let n = "sake-y-0";
        let one = module(4, 0).len() as u64;
        // room for two parts' modules, not three (each well below the limit on its own)
        let max = (2 * one + one / 2, u64::MAX);
        let add = |k: u32| super::add_part_within(&d, n, module(4, k as i32), regions(4, 0x1000 * (k + 1)), max);
        assert_eq!(add(0).await, axum::http::StatusCode::NO_CONTENT);
        assert_eq!(add(1).await, axum::http::StatusCode::NO_CONTENT);
        assert_eq!(add(2).await, axum::http::StatusCode::INSUFFICIENT_STORAGE);
        assert_eq!(super::read_parts(&d, n).await.len(), 2, "nothing written");
        // and the disk: an optimized copy counts
        let used = super::on_disk(&d, n).await;
        std::fs::write(d.join(format!("{n}.0123456789abcdef.opt.wasm")), vec![0u8; 100]).unwrap();
        assert_eq!(super::on_disk(&d, n).await, used + 100);
        let nine = module(4, 9).len() as u64;
        let disk = (u64::MAX, used + nine + 50);
        assert_eq!(super::add_part_within(&d, n, module(4, 9), regions(4, 0x9000), disk).await, axum::http::StatusCode::INSUFFICIENT_STORAGE);
        let disk = (u64::MAX, used + 100 + nine);
        assert_eq!(super::add_part_within(&d, n, module(4, 9), regions(4, 0x9000), disk).await, axum::http::StatusCode::NO_CONTENT);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn part_version_follows_module_and_index() {
        let v = super::version(b"wasm", b"idx");
        assert_eq!(v.len(), 16);
        assert!(v.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(v, super::version(b"wasm", b"idx"));
        assert_ne!(v, super::version(b"wasn", b"idx"), "a module changed under the same index");
        assert_ne!(v, super::version(b"wasm", b"idy"));
        // (the module's length first: where one ends and the index starts counts)
        assert_ne!(super::version(b"ab", b"c"), super::version(b"a", b"bc"));
    }

    #[test]
    fn writes_only_from_the_apps_pages() {
        let req = |method: &str, site: Option<&str>| {
            let mut r = Request::builder().method(method).uri("/saves/x");
            if let Some(s) = site {
                r = r.header("sec-fetch-site", s);
            }
            r.body(()).unwrap()
        };
        assert!(super::same_origin_write(&req("PUT", Some("same-origin"))));
        assert!(super::same_origin_write(&req("PUT", None)), "curl and the tests");
        assert!(!super::same_origin_write(&req("PUT", Some("same-site"))), "a web game on the game listener");
        assert!(!super::same_origin_write(&req("POST", Some("cross-site"))));
        assert!(super::same_origin_write(&req("GET", Some("cross-site"))));
        let with = |origin: &str, hosts: &[(&str, &str)]| {
            let mut r = Request::builder().method("PUT").uri("/saves/x").header("origin", origin);
            for (k, v) in hosts {
                r = r.header(*k, *v);
            }
            super::same_origin_write(&r.body(()).unwrap())
        };
        assert!(with("http://127.0.0.1:8656", &[("host", "127.0.0.1:8656")]));
        assert!(with("https://box.ts.net", &[("host", "box.ts.net:443")]));
        assert!(with("https://box.ts.net", &[("host", "127.0.0.1:8080"), ("x-forwarded-host", "box.ts.net")]), "tailscale serve");
        assert!(!with("http://127.0.0.1:8657", &[("host", "127.0.0.1:8656")]), "the game listener's pages");
        assert!(!with("https://evil.example", &[("host", "box.ts.net")]));
        assert!(!with("null", &[("host", "box.ts.net")]));
    }

    /// The server's start only removes `.zst` files older than their source (it must not wait
    /// for level 19, winetest-gpu); the background pass then writes the missing ones, each
    /// decoding to its source, and leaves what is small, of another kind, or fresh alone.
    #[test]
    fn precompress_drops_stale_first_and_writes_after() {
        use std::fs;
        let d = scratch("precompress");
        fs::create_dir_all(d.join("sub")).unwrap();
        let big: String = (0..4000).map(|i| format!("let v{i} = {};\n", i % 7)).collect();
        fs::write(d.join("a.js"), &big).unwrap();
        fs::write(d.join("sub/b.wasm"), big.as_bytes()).unwrap();
        fs::write(d.join("small.js"), "x").unwrap();
        fs::write(d.join("c.png"), &big).unwrap();
        // a stale one (older than its source) and a fresh one, set apart by their times
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100);
        fs::write(d.join("a.js.zst"), b"stale").unwrap();
        fs::File::options().write(true).open(d.join("a.js.zst")).unwrap().set_modified(old).unwrap();
        fs::write(d.join("sub/b.wasm.zst"), b"kept").unwrap();
        fs::File::options().write(true).open(d.join("sub/b.wasm")).unwrap().set_modified(old).unwrap();

        super::precompress(&d, false);
        assert!(!d.join("a.js.zst").exists(), "a stale .zst is gone before the server listens");
        assert_eq!(fs::read(d.join("sub/b.wasm.zst")).unwrap(), b"kept", "a fresh one stays");
        let zst = |d: &std::path::Path| {
            let mut n: Vec<_> = walk(d).into_iter().filter(|p| p.extension().is_some_and(|e| e == "zst")).collect();
            n.sort();
            n
        };
        assert_eq!(zst(&d), vec![d.join("sub/b.wasm.zst")], "and nothing is written yet");

        super::precompress(&d, true);
        assert_eq!(zstd::decode_all(&fs::read(d.join("a.js.zst")).unwrap()[..]).unwrap(), big.as_bytes());
        assert_eq!(fs::read(d.join("sub/b.wasm.zst")).unwrap(), b"kept");
        assert_eq!(zst(&d), vec![d.join("a.js.zst"), d.join("sub/b.wasm.zst")], "not small.js, not c.png");
        let _ = fs::remove_dir_all(&d);
    }

    fn walk(d: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut out = vec![];
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walk(&p));
            } else {
                out.push(p);
            }
        }
        out
    }
}
