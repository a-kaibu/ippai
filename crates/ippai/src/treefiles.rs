//! `GET /tree/<name>/<path>`: one file of a packed tree (`trees/<name>.tsv`, see pack.rs), as
//! itself. Games that are web pages (NW.js: web/src/html.ts) run from these URLs in an iframe.
//! Paths match without regard to case (as on Windows); one `Range: bytes=a-b` is honoured.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

struct Entry {
    size: u64,
    chunks: Vec<String>,
}

struct Tree {
    mtime: SystemTime,
    chunk: u64,
    files: HashMap<String, Arc<Entry>>,
}

#[derive(Clone)]
pub struct Trees {
    store: PathBuf,
    remote: Arc<Option<crate::storage::Storage>>,
    cache: Arc<Mutex<HashMap<String, Arc<Tree>>>>,
}

impl Trees {
    pub fn new(store: PathBuf) -> anyhow::Result<Self> {
        let remote = Arc::new(crate::storage::Storage::configured(crate::library::Library::new(&store)?)?);
        Ok(Trees { store, remote, cache: Default::default() })
    }

    fn tree(&self, name: &str) -> Option<Arc<Tree>> {
        let path = self.store.join("trees").join(format!("{name}.tsv"));
        let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
        if let Some(t) = self.cache.lock().unwrap().get(name).filter(|t| t.mtime == mtime) {
            return Some(t.clone());
        }
        let text = std::fs::read_to_string(&path).ok()?;
        let mut lines = text.lines();
        let chunk = lines.next()?.split('\t').nth(2)?.parse().ok()?;
        let unesc = |s: &str| s.replace("\\t", "\t").replace("\\n", "\n").replace("\\\\", "\\");
        let mut files = HashMap::new();
        for l in lines {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() == 6 && f[0] == "f" {
                let chunks = if f[5].is_empty() && f[4] == "0" { vec![] } else { f[5].split(',').map(String::from).collect() };
                let e = Entry { size: f[4].parse().ok()?, chunks };
                files.insert(unesc(f[3]).to_lowercase(), Arc::new(e));
            }
        }
        let t = Arc::new(Tree { mtime, chunk, files });
        self.cache.lock().unwrap().insert(name.to_string(), t.clone());
        Some(t)
    }
}

fn mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "css" => "text/css",
        "txt" | "ks" | "tjs" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ogg" | "oga" => "audio/ogg",
        "m4a" => "audio/mp4",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "webm" => "video/webm",
        "mp4" | "m4v" => "video/mp4",
        "ogv" => "video/ogg",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` against `size`: the first and last byte.
fn range(h: &HeaderMap, size: u64) -> Option<(u64, u64)> {
    let v = h.get(header::RANGE)?.to_str().ok()?.strip_prefix("bytes=")?;
    let (a, b) = v.split_once('-')?;
    let (a, b) = match (a.trim(), b.trim()) {
        ("", n) => (size.saturating_sub(n.parse().ok()?), size.checked_sub(1)?),
        (a, "") => (a.parse().ok()?, size.checked_sub(1)?),
        (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(size.checked_sub(1)?)),
    };
    (a <= b).then_some((a, b))
}

/// A page with the call to the player first (see web/src/html.ts): after `<head>` when it has one.
fn with_hook(page: Vec<u8>) -> Vec<u8> {
    const HOOK: &[u8] = b"<script>parent.vnvmFrame?.(window)</script>";
    let low = page.to_ascii_lowercase();
    let at = low
        .windows(5)
        .position(|w| w == b"<head")
        .and_then(|i| low[i..].iter().position(|&c| c == b'>').map(|j| i + j + 1))
        .unwrap_or(0);
    [&page[..at], HOOK, &page[at..]].concat()
}

pub async fn file(State(t): State<Trees>, Path((name, path)): Path<(String, String)>, h: HeaderMap) -> Response {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let res: Result<Response, StatusCode> = async {
        let tree = t.tree(&name).ok_or(StatusCode::NOT_FOUND)?;
        let e = tree.files.get(&path.to_lowercase()).ok_or(StatusCode::NOT_FOUND)?;
        let part = range(&h, e.size);
        let (a, b) = part.unwrap_or((0, e.size.saturating_sub(1)));
        let mut out = Vec::with_capacity(if e.size == 0 { 0 } else { (b - a + 1) as usize });
        if e.size > 0 {
            for i in a / tree.chunk..=b / tree.chunk {
                let hash = &e.chunks[i as usize];
                let len = tree.chunk.min(e.size - i * tree.chunk) as usize;
                let data = if hash.is_empty() {
                    vec![0; len]
                } else {
                    if hash.len() < 2 { return Err(StatusCode::INTERNAL_SERVER_ERROR); }
                    let logical = format!("chunks/{}/{hash}", &hash[..2]);
                    let remote = match t.remote.as_ref() {
                        Some(remote) => remote.get(&logical).await.map_err(|_| StatusCode::BAD_GATEWAY)?,
                        None => None,
                    };
                    let raw = match remote {
                        Some(raw) => raw,
                        None => tokio::fs::read(t.store.join(&logical)).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?,
                    };
                    zstd::decode_all(&raw[..]).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                };
                let from = if i == a / tree.chunk { (a % tree.chunk) as usize } else { 0 };
                let to = if i == b / tree.chunk { (b % tree.chunk) as usize + 1 } else { len };
                out.extend_from_slice(&data[from..to]);
            }
        }
        if part.is_none() && mime(&path).starts_with("text/html") {
            out = with_hook(out);
        }
        let mut r = Response::builder()
            .header(header::CONTENT_TYPE, mime(&path))
            .header(header::ACCEPT_RANGES, "bytes");
        if part.is_some() {
            r = r.status(StatusCode::PARTIAL_CONTENT).header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{}", e.size));
        }
        Ok(r.body(Body::from(out)).unwrap())
    }.await;
    match res { Ok(r) => r, Err(s) => s.into_response() }
}
