//! Serves the vnvm web app and the disk store made by vnvm-pack.
//!
//! - `/` the built web app (web/dist)
//! - `/store/disks/<name>/manifest.json`
//! - `/store/chunks/ab/<hash>`: stored zstd-compressed, sent with
//!   `Content-Encoding: zstd` so the browser decodes them natively, and marked
//!   immutable so its HTTP cache keeps them.
//! - `?v=...` on any URL: kept by the browser for good (the page changes the version when
//!   the content changes)
//! - `/jit/<name>.parts`: the x86 regions runs of a game saved (vnx86::jit::saved), one line
//!   per part: module, index, version; `PUT /jit-save/<name>` with (u32 module length, module,
//!   index) adds a run's new regions as a new part (parts compiled before stay the same); `PUT /jit-save/<name>.chunks` stores the store chunks a
//!   run read first (16-byte hashes), which the page fetches ahead next time.
//! - `/jit/_shared-<build>.parts`, `/jit/_shared.chunks`: the regions and chunks several
//!   programs saved (Wine's DLLs, helper processes and files): every page loads them too, so a
//!   game's first run starts with them
//! - `GET /games`: the launcher's list, the store's `games/<id>.json` (vnvm-pack add-game) as
//!   one JSON array; covers are under `/store/games/`
//! - `PUT /bench-log`: a result from web/bench.html, printed and appended to
//!   `<jit>/bench-results.txt`
//! - `GET`/`PUT /saves/<name>`: saved games backed up to S3 (see saves.rs)
//!
//! Every response carries COOP/COEP: QEMU's threads need SharedArrayBuffer,
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
use clap::Parser;

mod saves;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use vnx86::jit::{saved, wasm};
use tower_http::services::ServeDir;

#[derive(Parser)]
struct Args {
    /// The built web app.
    #[arg(long, default_value = "web/dist")]
    web: PathBuf,
    /// The vnvm-pack store.
    #[arg(long, default_value = "store")]
    store: PathBuf,
    /// Saved x86 regions, per game.
    #[arg(long, default_value = "jit")]
    jit: PathBuf,
    #[arg(long, default_value = "127.0.0.1:8080")]
    listen: SocketAddr,
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
        // chunks other programs read first too (Wine's own files: DLLs, fonts, the prefix) go
        // to _shared.chunks, which every page fetches ahead as well (a game's first run too)
        let shared_name = format!("{SHARED}.chunks");
        let mut others = std::collections::HashSet::new();
        if let Ok(mut d) = tokio::fs::read_dir(&jit.0).await {
            while let Ok(Some(f)) = d.next_entry().await {
                let f = f.file_name().to_string_lossy().into_owned();
                let prog = |n: &str| n.rsplit_once('-').map_or(n.to_string(), |x| x.0.to_string());
                if !f.ends_with(".chunks") || f == shared_name || prog(&f) == prog(&name) {
                    continue;
                }
                if let Ok(b) = tokio::fs::read(jit.0.join(&f)).await {
                    others.extend(b.chunks_exact(16).map(|c| <[u8; 16]>::try_from(c).unwrap()));
                }
            }
        }
        let mut shared = tokio::fs::read(jit.0.join(&shared_name)).await.unwrap_or_default();
        let mut have: std::collections::HashSet<[u8; 16]> = shared.chunks_exact(16).map(|c| c.try_into().unwrap()).collect();
        for c in body.chunks_exact(16) {
            let c: [u8; 16] = c.try_into().unwrap();
            if others.contains(&c) && shared.len() < 16 * 8192 && have.insert(c) {
                shared.extend_from_slice(&c);
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
    let _g = jit.1.lock().await;
    // Regions another program saved too (same roots and guest code: Wine's own DLLs and helper
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
        Some((if keep.iter().all(|&k| k) { new_wasm.to_vec() } else { wasm::select(new_wasm, &keep)? }, idx))
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
    status
}

/// The name of the bundle of regions several programs saved (`<SHARED>-<build>`).
const SHARED: &str = "_shared";

/// Whether two saved regions are the same translation's (any thread could have built either).
fn same(a: &saved::Entry, b: &saved::Entry) -> bool {
    a.roots == b.roots && a.code == b.code && a.text == b.text
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
    let fresh = if keep.iter().all(|&k| k) { Some(new_wasm.to_vec()) } else { wasm::select(new_wasm, &keep) };
    let Some(fresh) = fresh else { return StatusCode::BAD_REQUEST };
    let fresh_idx: Vec<saved::Entry> = new_idx.into_iter().zip(&keep).filter(|x| *x.1).map(|x| x.0).collect();
    // A save goes in a part of its own: the parts the browser has already compiled stay as
    // they are (and their compiled code in its cache). Past PARTS_MAX they become one again.
    let old_parts = parts.len();
    parts.push((fresh, fresh_idx));
    if parts.len() > PARTS_MAX {
        let entries: Vec<saved::Entry> = parts.iter().flat_map(|(_, e)| e.iter().cloned()).collect();
        let names: Vec<String> = entries.iter().map(|e| e.name()).collect();
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
            list.push_str(&format!("{name}.{stem}wasm {name}.{stem}idx {:08x}\n", fnv(&idx)));
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

/// The store's `games/*.json` as one array, by file name.
async fn games(State(store): State<Arc<PathBuf>>) -> Response {
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
    } else if versioned && ok {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
    } else {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    }
    res
}

use axum::serve::ListenerExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let jit: Jit = Arc::new((args.jit.clone(), tokio::sync::Mutex::new(())));
    let app = Router::new()
        .route("/jit-save/{name}", put(save_jit))
        .route("/bench-log", put(bench_log))
        .layer(DefaultBodyLimit::max(1 << 30))
        .with_state(jit)
        .merge(saves::router()?)
        .route("/games", get(games).with_state(Arc::new(args.store.clone())))
        .nest_service("/jit", ServeDir::new(&args.jit))
        .nest_service("/store", ServeDir::new(&args.store))
        .fallback_service(ServeDir::new(&args.web))
        .layer(middleware::from_fn(headers));
    // without TCP_NODELAY a kept-alive connection waits out the peer's delayed ACK (~40ms) on
    // every small response; the page reads the store one chunk at a time
    let listener = tokio::net::TcpListener::bind(args.listen).await?.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    eprintln!("vnvm-server on http://{}", args.listen);
    axum::serve(listener, app).await?;
    Ok(())
}
