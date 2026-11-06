//! `GET /tree/<name>/<path>`: one file of a packed tree (`trees/<name>.tsv`, see pack.rs), as
//! itself. Games that are web pages (NW.js: web/src/html.ts) run from these URLs in an iframe.
//! Paths match without regard to case (as on Windows); one `Range: bytes=a-b` is honoured (416
//! for one that is not satisfiable). Files go out one chunk at a time: a big movie does not sit
//! in memory whole.

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

/// `bytes=a-b` / `bytes=a-` / `bytes=-n` against `size`: the first and last byte. None: no
/// Range; Some(None): one that is not satisfiable (or not one range of bytes).
fn range(h: &HeaderMap, size: u64) -> Option<Option<(u64, u64)>> {
    let v = h.get(header::RANGE)?;
    let parse = || -> Option<(u64, u64)> {
        let v = v.to_str().ok()?.trim().strip_prefix("bytes=")?;
        let (a, b) = v.split_once('-')?;
        let last = size.checked_sub(1)?;
        let (a, b) = match (a.trim(), b.trim()) {
            ("", n) => (size.saturating_sub(n.parse::<u64>().ok().filter(|&n| n > 0)?), last),
            (a, "") => (a.parse().ok()?, last),
            (a, b) => (a.parse().ok()?, b.parse::<u64>().ok()?.min(last)),
        };
        (a <= b).then_some((a, b))
    };
    Some(parse())
}

/// Pages bigger than this go out as they are (no game's start page is).
const MAX_PAGE: u64 = 16 << 20;

/// A page with the frame's script first (web/src/frame.ts, served by the game listener): after
/// `<head>` when it has one.
fn with_hook(page: Vec<u8>) -> Vec<u8> {
    const HOOK: &[u8] = b"<script src=\"/frame.js\"></script>";
    let low = page.to_ascii_lowercase();
    let at = low
        .windows(5)
        .position(|w| w == b"<head")
        .and_then(|i| low[i..].iter().position(|&c| c == b'>').map(|j| i + j + 1))
        .unwrap_or(0);
    [&page[..at], HOOK, &page[at..]].concat()
}

/// Chunks of a file on their way at once (the one going out and those after it).
const AHEAD: usize = 4;

/// A stored chunk decoded, if it is `len` bytes: decoding stops past that (a broken or hostile
/// chunk does not get to fill memory).
fn decode(raw: &[u8], len: usize) -> Option<Vec<u8>> {
    use std::io::Read;
    let mut out = Vec::with_capacity(len);
    zstd::stream::Decoder::new(raw).ok()?.take(len as u64 + 1).read_to_end(&mut out).ok()?;
    (out.len() == len).then_some(out)
}

impl Trees {
    /// Chunk `i` of `e`, decoded.
    async fn chunk(&self, tree: &Tree, e: &Entry, i: u64) -> Result<Vec<u8>, StatusCode> {
        let hash = e.chunks.get(i as usize).ok_or(StatusCode::INTERNAL_SERVER_ERROR)?;
        let len = tree.chunk.min(e.size - i * tree.chunk) as usize;
        if hash.is_empty() {
            return Ok(vec![0; len]);
        }
        if hash.len() < 2 {
            return Err(StatusCode::INTERNAL_SERVER_ERROR);
        }
        let logical = format!("chunks/{}/{hash}", &hash[..2]);
        let remote = match self.remote.as_ref() {
            Some(remote) => remote.get(&logical).await.map_err(|_| StatusCode::BAD_GATEWAY)?,
            None => None,
        };
        let raw: Arc<[u8]> = match remote {
            Some(raw) => raw,
            None => tokio::fs::read(self.store.join(&logical)).await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?.into(),
        };
        tokio::task::spawn_blocking(move || decode(&raw, len))
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)
    }

    /// Bytes `a..=b` of `e`, in order, the next [`AHEAD`] chunks fetched while one goes out
    /// (from S3 each is a round trip).
    fn bytes(self, tree: Arc<Tree>, e: Arc<Entry>, a: u64, b: u64) -> impl futures_util::Stream<Item = std::io::Result<Vec<u8>>> {
        use futures_util::StreamExt;
        let (first, last) = (a / tree.chunk, b / tree.chunk);
        futures_util::stream::iter(first..=last)
            .map(move |i| {
                let (t, tree, e) = (self.clone(), tree.clone(), e.clone());
                async move {
                    let data = t.chunk(&tree, &e, i).await.map_err(|s| std::io::Error::other(s.to_string()))?;
                    let from = if i == first { (a % tree.chunk) as usize } else { 0 };
                    let to = if i == last { (b % tree.chunk) as usize + 1 } else { data.len() };
                    Ok(data[from..to].to_vec())
                }
            })
            .buffered(AHEAD)
    }
}

pub async fn file(State(t): State<Trees>, Path((name, path)): Path<(String, String)>, h: HeaderMap) -> Response {
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let res: Result<Response, StatusCode> = async {
        let tree = t.tree(&name).ok_or(StatusCode::NOT_FOUND)?;
        let e = tree.files.get(&path.to_lowercase()).ok_or(StatusCode::NOT_FOUND)?.clone();
        let r = Response::builder().header(header::CONTENT_TYPE, mime(&path)).header(header::ACCEPT_RANGES, "bytes");
        let part = match range(&h, e.size) {
            None => None,
            Some(Some(p)) => Some(p),
            Some(None) => {
                return Ok(r
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{}", e.size))
                    .body(Body::empty())
                    .unwrap());
            }
        };
        if e.size == 0 {
            return Ok(r.header(header::CONTENT_LENGTH, 0).body(Body::empty()).unwrap());
        }
        let (a, b) = part.unwrap_or((0, e.size - 1));
        if part.is_none() && e.size <= MAX_PAGE && mime(&path).starts_with("text/html") {
            use futures_util::TryStreamExt;
            let page: Vec<u8> = t.clone().bytes(tree, e, a, b).try_concat().await.map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let page = with_hook(page);
            return Ok(r.header(header::CONTENT_LENGTH, page.len()).body(Body::from(page)).unwrap());
        }
        let mut r = r.header(header::CONTENT_LENGTH, b - a + 1);
        if part.is_some() {
            r = r.status(StatusCode::PARTIAL_CONTENT).header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{}", e.size));
        }
        Ok(r.body(Body::from_stream(t.clone().bytes(tree, e, a, b))).unwrap())
    }
    .await;
    match res {
        Ok(r) => r,
        Err(s) => s.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, http::Request, routing::get, Router};
    use tower::ServiceExt;

    /// A store with tree `t`: a.bin (10 bytes in chunks of 4, the middle one all zero) and
    /// index.html.
    fn store() -> (PathBuf, Router) {
        let dir = std::env::temp_dir().join(format!("ippai-treefiles-{}", uuid::Uuid::new_v4()));
        let put = |data: &[u8]| {
            let h = blake3::hash(data).to_hex()[..32].to_string();
            let d = dir.join("chunks").join(&h[..2]);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(&h), zstd::encode_all(data, 3).unwrap()).unwrap();
            h
        };
        let (c0, c2) = (put(b"0123"), put(b"89"));
        let page = put(b"<html><head><title>x</title></head></html>");
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        std::fs::write(
            dir.join("trees/t.tsv"),
            format!("vnvm-tree\t1\t4\nf\t644\t0\ta.bin\t10\t{c0},,{c2}\nf\t644\t0\tempty.txt\t0\t\n"),
        )
        .unwrap();
        // (a second tree, chunked so the page is one chunk)
        std::fs::write(dir.join("trees/p.tsv"), format!("vnvm-tree\t1\t1024\nf\t644\t0\tWWW/Index.html\t42\t{page}\n")).unwrap();
        let t = Trees { store: dir.clone(), remote: Arc::new(None), cache: Default::default() };
        (dir, Router::new().route("/tree/{name}/{*path}", get(file)).with_state(t))
    }

    async fn get_(app: &Router, path: &str, range: Option<&str>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut r = Request::builder().uri(path);
        if let Some(v) = range {
            r = r.header(header::RANGE, v);
        }
        let res = app.clone().oneshot(r.body(Body::empty()).unwrap()).await.unwrap();
        let (status, headers) = (res.status(), res.headers().clone());
        (status, headers, to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec())
    }

    #[tokio::test]
    async fn whole_files_and_ranges() {
        let (dir, app) = store();
        let (s, h, b) = get_(&app, "/tree/t/a.bin", None).await;
        assert_eq!((s, &b[..]), (StatusCode::OK, &b"0123\0\0\0\089"[..]));
        assert_eq!(h[header::CONTENT_LENGTH], "10");
        for (range, want, cr) in [
            ("bytes=2-8", &b"23\0\0\0\08"[..], "bytes 2-8/10"),
            ("bytes=8-", b"89", "bytes 8-9/10"),
            ("bytes=-3", b"\089", "bytes 7-9/10"),
            ("bytes=3-100", b"3\0\0\0\089", "bytes 3-9/10"),
            ("bytes=4-4", b"\0", "bytes 4-4/10"),
        ] {
            let (s, h, b) = get_(&app, "/tree/t/a.bin", Some(range)).await;
            assert_eq!(s, StatusCode::PARTIAL_CONTENT, "{range}");
            assert_eq!(&b[..], want, "{range}");
            assert_eq!(h[header::CONTENT_RANGE], cr);
            assert_eq!(h[header::CONTENT_LENGTH], want.len().to_string());
        }
        let (s, _, b) = get_(&app, "/tree/t/EMPTY.TXT", None).await;
        assert_eq!((s, b.len()), (StatusCode::OK, 0));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Random files of many chunks (some all zero), random ranges: what comes out is the
    /// file's bytes, in order. A chunk that decodes to more than its length is an error, found
    /// without decoding it all.
    #[tokio::test]
    async fn many_chunks_in_order() {
        let dir = std::env::temp_dir().join(format!("ippai-treefiles-{}", uuid::Uuid::new_v4()));
        let mut seed = 0x853c49e6748fea9bu64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let chunk = 64u64;
        let mut lines = format!("vnvm-tree\t1\t{chunk}\n");
        let mut files = Vec::new();
        for f in 0..6 {
            let size = 1 + rnd(40 * chunk);
            let data: Vec<u8> = (0..size).map(|_| rnd(256) as u8).collect();
            let mut hashes = Vec::new();
            for (i, c) in data.chunks(chunk as usize).enumerate() {
                if i % 5 == 3 {
                    hashes.push(String::new());
                    continue;
                }
                let h = blake3::hash(c).to_hex()[..32].to_string();
                std::fs::create_dir_all(dir.join("chunks").join(&h[..2])).unwrap();
                std::fs::write(dir.join("chunks").join(&h[..2]).join(&h), zstd::encode_all(c, 3).unwrap()).unwrap();
                hashes.push(h);
            }
            let want: Vec<u8> = data.chunks(chunk as usize).enumerate().flat_map(|(i, c)| if i % 5 == 3 { vec![0; c.len()] } else { c.to_vec() }).collect();
            lines += &format!("f\t644\t0\tf{f}\t{size}\t{}\n", hashes.join(","));
            files.push(want);
        }
        // a chunk that decodes to 64 MiB, listed as 64 bytes
        let bomb = zstd::encode_all(&vec![0u8; 64 << 20][..], 19).unwrap();
        std::fs::create_dir_all(dir.join("chunks/bb")).unwrap();
        std::fs::write(dir.join("chunks/bb/bb00"), bomb).unwrap();
        lines += "f\t644\t0\tbomb\t128\tbb00,bb00\n";
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        std::fs::write(dir.join("trees/m.tsv"), lines).unwrap();
        let t = Trees { store: dir.clone(), remote: Arc::new(None), cache: Default::default() };
        let app = Router::new().route("/tree/{name}/{*path}", get(file)).with_state(t);
        for (f, want) in files.iter().enumerate() {
            let (s, _, b) = get_(&app, &format!("/tree/m/f{f}"), None).await;
            assert_eq!((s, &b), (StatusCode::OK, want));
            for _ in 0..20 {
                let a = rnd(want.len() as u64);
                let z = a + rnd(want.len() as u64 - a);
                let (s, _, b) = get_(&app, &format!("/tree/m/f{f}"), Some(&format!("bytes={a}-{z}"))).await;
                assert_eq!(s, StatusCode::PARTIAL_CONTENT);
                assert_eq!(&b[..], &want[a as usize..=z as usize], "f{f} {a}-{z}");
            }
        }
        let res = app.clone().oneshot(Request::builder().uri("/tree/m/bomb").body(Body::empty()).unwrap()).await.unwrap();
        assert!(to_bytes(res.into_body(), usize::MAX).await.is_err());
        assert_eq!(decode(&zstd::encode_all(&b"abcd"[..], 3).unwrap(), 4).as_deref(), Some(&b"abcd"[..]));
        assert_eq!(decode(&zstd::encode_all(&b"abcde"[..], 3).unwrap(), 4), None);
        assert_eq!(decode(&zstd::encode_all(&b"abc"[..], 3).unwrap(), 4), None);
        assert_eq!(decode(b"not zstd", 4), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A file of 24 chunks from S3 answering in 30 ms each: the chunks after the one going
    /// out are on their way meanwhile (one at a time it takes 24 round trips).
    #[tokio::test]
    async fn s3_chunks_fetched_ahead() {
        let (_f, s, dir) = crate::storage::gc_tests::serve(30).await;
        let mut hashes = Vec::new();
        let mut want = Vec::new();
        for i in 0..24u8 {
            let c = [i; 64];
            let h = blake3::hash(&c).to_hex()[..32].to_string();
            s.put(&format!("chunks/{}/{h}", &h[..2]), &zstd::encode_all(&c[..], 3).unwrap()).await.unwrap();
            hashes.push(h);
            want.extend_from_slice(&c);
        }
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        std::fs::write(dir.join("trees/s.tsv"), format!("vnvm-tree\t1\t64\nf\t644\t0\tbig.bin\t{}\t{}\n", want.len(), hashes.join(","))).unwrap();
        let t = Trees { store: dir.clone(), remote: Arc::new(Some(s)), cache: Default::default() };
        let app = Router::new().route("/tree/{name}/{*path}", get(file)).with_state(t);
        let t0 = std::time::Instant::now();
        let (st, _, b) = get_(&app, "/tree/s/big.bin", None).await;
        let took = t0.elapsed();
        eprintln!("24 chunks at 30 ms: {took:?} (one at a time: 720 ms)");
        assert_eq!((st, b), (StatusCode::OK, want));
        assert!(took < std::time::Duration::from_millis(400), "{took:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn bad_ranges_are_416() {
        let (dir, app) = store();
        for range in ["bytes=10-", "bytes=5-2", "bytes=-0", "bytes=x-1", "items=0-1", "bytes=0-1,4-5"] {
            let (s, h, _) = get_(&app, "/tree/t/a.bin", Some(range)).await;
            assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE, "{range}");
            assert_eq!(h[header::CONTENT_RANGE], "bytes */10");
        }
        let (s, _, _) = get_(&app, "/tree/t/empty.txt", Some("bytes=0-")).await;
        assert_eq!(s, StatusCode::RANGE_NOT_SATISFIABLE);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn pages_get_the_frame_script() {
        let (dir, app) = store();
        let (s, h, b) = get_(&app, "/tree/p/www/index.html", None).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(String::from_utf8(b.clone()).unwrap(), "<html><head><script src=\"/frame.js\"></script><title>x</title></head></html>");
        assert_eq!(h[header::CONTENT_LENGTH], b.len().to_string());
        let (s, _, _) = get_(&app, "/tree/p/www/nothere.html", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        let (s, _, _) = get_(&app, "/tree/..%2Fx/a", None).await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }
}
