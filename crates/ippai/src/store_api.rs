use crate::storage::Storage;
use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::{
    path::{Component, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
struct Store {
    local: PathBuf,
    remote: Arc<Option<Storage>>,
    decoded: Arc<std::sync::Mutex<Decoded>>,
    /// trees whose chunks were fetched ahead (or are being) since the server started
    ahead: Arc<std::sync::Mutex<std::collections::HashSet<String>>>,
}

/// Files up to this size are fetched ahead whole when their tree is first asked for; of bigger
/// ones the first and last chunks (where archives keep their headers and indexes: xp3, zip).
const AHEAD_WHOLE: u64 = 8 << 20;
/// Objects fetched ahead at once. From Hugging Face each object takes ~2 s before its first
/// byte whatever its size, so throughput is a matter of how many at once: 4 gave 3 MB/s, 16
/// 6 MB/s, 64 10 MB/s (worker2's line).
const AHEAD_WIDTH: usize = 48;

/// What a game's start reads first, from its tree list (`trees/<name>.tsv`): the chunks of its
/// small files (the exe, DLLs, scripts, small archives), smallest first, then the first and
/// last chunks of the others; each chunk once.
pub(crate) fn start_set(tsv: &str) -> Vec<String> {
    let mut files: Vec<(u64, Vec<&str>)> = Vec::new();
    for l in tsv.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() != 6 || f[0] != "f" {
            continue;
        }
        let Ok(size) = f[4].parse::<u64>() else { continue };
        let hashes: Vec<&str> = f[5].split(',').filter_map(|c| c.split_once(':').map(|x| x.0)).filter(|h| h.len() > 2 && h.bytes().all(|b| b.is_ascii_hexdigit())).collect();
        if !hashes.is_empty() {
            files.push((size, hashes));
        }
    }
    files.sort_by_key(|f| f.0);
    let (small, big): (Vec<_>, Vec<_>) = files.into_iter().partition(|f| f.0 <= AHEAD_WHOLE);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    let picks = small.iter().flat_map(|f| f.1.iter().copied()).chain(big.iter().flat_map(|f| [f.1[0], f.1[f.1.len() - 1]]));
    for h in picks {
        if seen.insert(h) {
            out.push(format!("chunks/{}/{h}", &h[..2]));
        }
    }
    out
}

/// Fetches tree `name`'s [`start_set`] into the disk cache, AHEAD_WIDTH at once.
async fn fetch_ahead(s: Store, name: String) {
    if s.remote.is_none() {
        return;
    }
    let Ok(tsv) = tokio::fs::read_to_string(s.local.join("trees").join(format!("{name}.tsv"))).await else { return };
    let set = start_set(&tsv);
    let t0 = std::time::Instant::now();
    let width = Arc::new(tokio::sync::Semaphore::new(AHEAD_WIDTH));
    let mut jobs = tokio::task::JoinSet::new();
    for l in set.iter().cloned() {
        let (remote, width) = (s.remote.clone(), width.clone());
        jobs.spawn(async move {
            let _p = width.acquire_owned().await.ok()?;
            let r = (*remote).as_ref()?;
            match r.warm(&l).await {
                Ok(got) => Some(got),
                Err(e) => {
                    eprintln!("ahead {l}: {e}");
                    None
                }
            }
        });
    }
    let mut fetched = 0;
    while let Some(r) = jobs.join_next().await {
        fetched += matches!(r, Ok(Some(true))) as usize;
    }
    if fetched > 0 {
        eprintln!("ahead: {name}: {fetched} of {} start chunks from S3 in {:.1} s", set.len(), t0.elapsed().as_secs_f64());
    }
}

/// Chunks decoded for ranges lately (a game's read asks for a chunk's blocks one range after
/// another: decoded once), newest last, DECODED_MAX bytes in all (and the newest whatever its size).
#[derive(Default)]
struct Decoded {
    map: std::collections::HashMap<String, Arc<[u8]>>,
    order: std::collections::VecDeque<String>,
    bytes: usize,
    /// being decoded (the first asking decodes it, the others wait for that)
    decoding: std::collections::HashMap<String, Arc<tokio::sync::OnceCell<Option<Arc<[u8]>>>>>,
}

const DECODED_MAX: usize = 256 << 20;
/// A chunk decodes to this much at most (FastCDC's longest is 4x its average): decoding stops
/// past it (a broken or hostile chunk does not get to fill memory).
const CHUNK_MAX: u64 = 512 << 20;

impl Decoded {
    fn get(&mut self, k: &str) -> Option<Arc<[u8]>> {
        let d = self.map.get(k)?.clone();
        self.order.retain(|x| x != k);
        self.order.push_back(k.to_string());
        Some(d)
    }
    fn put(&mut self, k: &str, d: Arc<[u8]>) {
        if let Some(old) = self.map.insert(k.to_string(), d.clone()) {
            self.bytes -= old.len();
            self.order.retain(|x| x != k);
        }
        self.bytes += d.len();
        self.order.push_back(k.to_string());
        while self.bytes > DECODED_MAX && self.order.len() > 1 {
            let k = self.order.pop_front().unwrap();
            self.bytes -= self.map.remove(&k).map_or(0, |d| d.len());
        }
    }
}

/// `Range: bytes=a-b` or `bytes=a-` (one range: what host/fetch-bridge.js asks for), as
/// (a, b inclusive) clamped to `len`; None for anything else, Some(None) past the end.
fn range(h: &str, len: u64) -> Option<Option<(u64, u64)>> {
    let (a, b) = h.trim().strip_prefix("bytes=")?.split_once('-')?;
    let a: u64 = a.parse().ok()?;
    let b: u64 = if b.is_empty() { u64::MAX } else { b.parse().ok()? };
    if b < a {
        return None;
    }
    Some((a < len).then(|| (a, b.min(len - 1))))
}

async fn file(State(s): State<Store>, Path(path): Path<String>, headers: HeaderMap) -> Response {
    // a tree list asked for: a game starts; what it reads first is fetched from S3 meanwhile
    if let Some(name) = path.strip_prefix("trees/").and_then(|n| n.strip_suffix(".tsv")) {
        if s.remote.is_some() && !name.contains('/') && s.ahead.lock().unwrap_or_else(|e| e.into_inner()).insert(name.to_string()) {
            tokio::spawn(fetch_ahead(s.clone(), name.to_string()));
        }
    }
    // a range of a chunk: of its decoded bytes, sent as they are (206, no Content-Encoding): a
    // game's scattered reads need a few blocks of a chunk, not all of it (crates/kura/src/vfs.rs)
    let want = headers.get(header::RANGE).and_then(|v| v.to_str().ok()).filter(|_| path.starts_with("chunks/"));
    // (decoded lately: not read again; 32 MiB read from the disk per 256 KiB asked for made
    // big chunks slow)
    if let Some(want) = want {
        if let Some(d) = s.decoded.lock().unwrap_or_else(|e| e.into_inner()).get(&path) {
            return ranged(d, want);
        }
    }
    let res = whole(&s, &path).await;
    if res.status() != StatusCode::OK {
        return res;
    }
    let Some(want) = want else { return res };
    let raw = match axum::body::to_bytes(res.into_body(), usize::MAX).await {
        Ok(b) => b,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    // (decoded once: the fetcher threads ask for several ranges of a new chunk at once)
    let cell = {
        let mut g = s.decoded.lock().unwrap_or_else(|e| e.into_inner());
        match g.get(&path) {
            Some(d) => Ok(d),
            None => Err(g.decoding.entry(path.clone()).or_default().clone()),
        }
    };
    let data = match cell {
        Ok(d) => d,
        Err(cell) => {
            let d = cell
                .get_or_init(|| async move {
                    tokio::task::spawn_blocking(move || {
                        use std::io::Read;
                        let mut out = Vec::new();
                        zstd::stream::Decoder::new(&raw[..]).ok()?.take(CHUNK_MAX + 1).read_to_end(&mut out).ok()?;
                        ((out.len() as u64) <= CHUNK_MAX).then(|| Arc::<[u8]>::from(out))
                    })
                    .await
                    .ok()
                    .flatten()
                })
                .await
                .clone();
            let mut g = s.decoded.lock().unwrap_or_else(|e| e.into_inner());
            g.decoding.remove(&path);
            let Some(d) = d else { return StatusCode::INTERNAL_SERVER_ERROR.into_response() };
            g.put(&path, d.clone());
            d
        }
    };
    ranged(data, want)
}

/// Range `want` of decoded chunk `data`.
fn ranged(data: Arc<[u8]>, want: &str) -> Response {
    let len = data.len() as u64;
    match range(want, len) {
        None => full_decoded(data),
        Some(None) => Response::builder()
            .status(StatusCode::RANGE_NOT_SATISFIABLE)
            .header(header::CONTENT_RANGE, format!("bytes */{len}"))
            .body(axum::body::Body::empty())
            .unwrap(),
        Some(Some((a, b))) => Response::builder()
            .status(StatusCode::PARTIAL_CONTENT)
            .header(header::CONTENT_RANGE, format!("bytes {a}-{b}/{len}"))
            .body(axum::body::Body::from(axum::body::Bytes::from_owner(data).slice(a as usize..b as usize + 1)))
            .unwrap(),
    }
}

/// A chunk decoded, whole (a range header this does not take: as if there was none, but sent
/// decoded; the middleware marks only 200s of chunks as zstd-coded, so this is a 206 of all).
fn full_decoded(data: Arc<[u8]>) -> Response {
    let len = data.len() as u64;
    Response::builder()
        .status(StatusCode::PARTIAL_CONTENT)
        .header(header::CONTENT_RANGE, format!("bytes 0-{}/{len}", len.saturating_sub(1)))
        .body(axum::body::Body::from(axum::body::Bytes::from_owner(data)))
        .unwrap()
}

/// `/store/<path>` as stored (chunks zstd-coded).
async fn whole(s: &Store, path: &str) -> Response {
    let p = std::path::Path::new(&path);
    if !p.components().all(|c| matches!(c, Component::Normal(_)))
        || !matches!(path.split('/').next(), Some("chunks" | "trees" | "games"))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    // The DB and keys are never reachable through /store.
    if let Some(remote) = s.remote.as_ref() {
        match remote.get(path).await {
            Ok(Some(bytes)) => return axum::body::Bytes::from_owner(bytes).into_response(),
            Ok(None) => {}
            Err(e) => {
                eprintln!("store: {e}");
                return StatusCode::BAD_GATEWAY.into_response();
            }
        }
    }
    match tokio::fs::read(s.local.join(path)).await {
        Ok(bytes) => bytes.into_response(),
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// How long a presigned URL from `/store-direct` works (a game's chunk asked for again after
/// that asks for a new one).
const DIRECT_EXPIRY: u32 = 6 * 3600;

/// `GET /store-direct/chunks/<ab>/<hash>`: how the page fetches a chunk from the bucket itself
/// (host/fetch-bridge.js): `{"url": presigned GET, "key": hex AES-256-GCM key, "object": UUID}`.
/// 404 when the chunk is not in the bucket (a chunk only here, not a chunk path): the page asks
/// /store for it as before; 501 for a store without S3, so the page stops asking.
async fn direct(State(s): State<Store>, Path(path): Path<String>) -> Response {
    let Some(remote) = s.remote.as_ref().as_ref() else { return StatusCode::NOT_IMPLEMENTED.into_response() };
    if !chunk_path(&path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match remote.direct(&path, DIRECT_EXPIRY).await {
        Ok(Some(d)) => {
            let mut r = axum::Json(direct_json(&d)).into_response();
            // (a key: kept by nobody else; the page keeps it in memory for DIRECT_EXPIRY at most)
            r.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
            r
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            eprintln!("store-direct {path}: {e}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

/// A /store-direct answer: `{url, key, object}`, and `pack: {id, url, offset, len}` when a
/// delivery pack holds the object too (storage.rs pack_objects; the page reads its neighbours
/// in the pack with the same GET).
fn direct_json(d: &crate::storage::Direct) -> serde_json::Value {
    let key: String = d.key.iter().map(|b| format!("{b:02x}")).collect();
    let mut v = serde_json::json!({ "url": d.url, "key": key, "object": d.object.to_string() });
    if let Some(p) = &d.pack {
        v["pack"] = serde_json::json!({ "id": p.id.to_string(), "url": p.url, "offset": p.offset, "len": p.len });
    }
    v
}

/// Whether `path` is a chunk's path (`chunks/<ab>/<hash>`), all /store-direct answers for.
fn chunk_path(path: &str) -> bool {
    path.split('/').count() == 3 && path.starts_with("chunks/") && path.bytes().all(|b| b.is_ascii_hexdigit() || b == b'/' || b.is_ascii_lowercase())
}

/// Chunk paths one `POST /store-direct` may ask for.
const DIRECT_MANY: usize = 4096;

/// `POST /store-direct` with a JSON list of chunk paths: `{"<path>": {url, key, object}}` for
/// those the page may fetch from the bucket itself, as `GET /store-direct/<path>` answers each
/// (host/fetch-bridge.js ippaiDirectMany: a game's chunks asked for at once, so its first read
/// of each goes to the bucket without a round trip here first). Those it leaves out are only
/// here (or this server's disk has them: /store is quicker then). 501 without S3; 413 past
/// DIRECT_MANY paths.
async fn direct_many(State(s): State<Store>, axum::Json(paths): axum::Json<Vec<String>>) -> Response {
    let Some(remote) = s.remote.as_ref().as_ref() else { return StatusCode::NOT_IMPLEMENTED.into_response() };
    if paths.len() > DIRECT_MANY {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    let mut out = serde_json::Map::new();
    for path in paths.iter().filter(|p| chunk_path(p)) {
        match remote.direct(path, DIRECT_EXPIRY).await {
            Ok(Some(d)) => {
                out.insert(path.clone(), direct_json(&d));
            }
            Ok(None) => {}
            Err(e) => eprintln!("store-direct {path}: {e}"),
        }
    }
    let mut r = axum::Json(serde_json::Value::Object(out)).into_response();
    r.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
    r
}

pub fn router(local: PathBuf) -> anyhow::Result<Router> {
    let db = crate::library::Library::new(&local)?;
    let state = Store {
        local,
        remote: Arc::new(Storage::configured(db)?),
        decoded: Default::default(),
        ahead: Default::default(),
    };
    Ok(Router::new()
        .route("/store/{*path}", get(file))
        .route("/store-direct/{*path}", get(direct))
        .route("/store-direct", axum::routing::post(direct_many))
        .with_state(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    async fn get_(app: &Router, uri: &str, range: Option<&str>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let mut r = Request::builder().uri(uri);
        if let Some(v) = range {
            r = r.header(header::RANGE, v);
        }
        let res = app.clone().oneshot(r.body(Body::empty()).unwrap()).await.unwrap();
        let (p, b) = res.into_parts();
        (p.status, p.headers, to_bytes(b, usize::MAX).await.unwrap().to_vec())
    }

    /// Ranges of chunks are of their decoded bytes (any range, past the end, open-ended, many
    /// at once while it is decoded); without one a chunk is sent as stored; a range of a tree
    /// list, or one this does not take, is no range.
    #[tokio::test]
    async fn chunk_ranges() {
        let dir = std::env::temp_dir().join(format!("ippai-store-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("chunks/ab")).unwrap();
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        let data: Vec<u8> = (0..300_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        let stored = zstd::encode_all(&data[..], 3).unwrap();
        std::fs::write(dir.join("chunks/ab/abc"), &stored).unwrap();
        std::fs::write(dir.join("trees/t.tsv"), b"vnvm-tree\t2\t64\n").unwrap();
        let state = Store { local: dir.clone(), remote: Arc::new(None), decoded: Default::default(), ahead: Default::default() };
        let app = Router::new().route("/store/{*path}", get(file)).with_state(state.clone());
        let (s, _, b) = get_(&app, "/store/chunks/ab/abc", None).await;
        assert_eq!((s, b), (StatusCode::OK, stored.clone()));
        let mut seed = 5u64;
        for _ in 0..40 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let a = (seed >> 33) % data.len() as u64;
            let z = a + (seed >> 12) % 70_000;
            let (s, h, b) = get_(&app, "/store/chunks/ab/abc", Some(&format!("bytes={a}-{z}"))).await;
            let end = (z as usize).min(data.len() - 1);
            assert_eq!(s, StatusCode::PARTIAL_CONTENT);
            assert_eq!(h[header::CONTENT_RANGE], format!("bytes {a}-{end}/{}", data.len()).as_str());
            assert_eq!(&b[..], &data[a as usize..=end]);
        }
        let (s, _, b) = get_(&app, "/store/chunks/ab/abc", Some("bytes=299990-")).await;
        assert_eq!((s, &b[..]), (StatusCode::PARTIAL_CONTENT, &data[299_990..]));
        let (s, h, _) = get_(&app, "/store/chunks/ab/abc", Some("bytes=300000-300010")).await;
        assert_eq!((s, h[header::CONTENT_RANGE].to_str().unwrap()), (StatusCode::RANGE_NOT_SATISFIABLE, "bytes */300000"));
        for odd in ["bytes=5-1", "bytes=-5", "bytes=0-1,4-5", "items=0-1"] {
            let (s, _, b) = get_(&app, "/store/chunks/ab/abc", Some(odd)).await;
            assert_eq!((s, b.len()), (StatusCode::PARTIAL_CONTENT, data.len()), "{odd}");
        }
        let (s, _, b) = get_(&app, "/store/trees/t.tsv", Some("bytes=0-3")).await;
        assert_eq!((s, &b[..]), (StatusCode::OK, &b"vnvm-tree\t2\t64\n"[..]));
        // (side by side while it is decoded: one decode, every answer right)
        state.decoded.lock().unwrap().map.clear();
        let all = futures_util::future::join_all((0..8u64).map(|i| {
            let app = app.clone();
            async move { get_(&app, "/store/chunks/ab/abc", Some(&format!("bytes={}-{}", i * 1000, i * 1000 + 99))).await }
        }))
        .await;
        for (i, (s, _, b)) in all.into_iter().enumerate() {
            assert_eq!((s, &b[..]), (StatusCode::PARTIAL_CONTENT, &data[i * 1000..i * 1000 + 100]));
        }
        assert!(state.decoded.lock().unwrap().decoding.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    proptest::proptest! {
        /// `range` against the plain reading of one byte range.
        #[test]
        fn range_model(a in 0u64..2000, b in proptest::option::of(0u64..2000), len in 0u64..1500) {
            let h = format!("bytes={a}-{}", b.map_or(String::new(), |b| b.to_string()));
            let got = range(&h, len);
            let z = b.unwrap_or(u64::MAX);
            if z < a {
                proptest::prop_assert_eq!(got, None);
            } else if a >= len {
                proptest::prop_assert_eq!(got, Some(None));
            } else {
                proptest::prop_assert_eq!(got, Some(Some((a, z.min(len - 1)))));
            }
        }
    }

    /// The decoded chunks kept: within DECODED_MAX (the newest always), newest kept longest.
    #[test]
    fn decoded_bounded() {
        let mut d = Decoded::default();
        let big = |n: usize| Arc::<[u8]>::from(vec![0u8; n]);
        for i in 0..20 {
            d.put(&format!("c{i}"), big(40 << 20));
            assert!(d.bytes <= DECODED_MAX);
            assert_eq!(d.bytes, d.map.values().map(|x| x.len()).sum::<usize>());
            assert!(d.map.contains_key(&format!("c{i}")));
        }
        assert!(d.get("c0").is_none() && d.get("c19").is_some());
        d.put("huge", big(DECODED_MAX + 1));
        assert_eq!((d.map.len(), d.bytes), (1, DECODED_MAX + 1));
    }

    fn line(path: &str, size: u64, chunks: &[(&str, u64)]) -> String {
        let c: Vec<String> = chunks.iter().map(|(h, n)| format!("{h}:{n}")).collect();
        format!("f\t644\t0\t{path}\t{size}\t{}\n", c.join(","))
    }

    /// A game's start: its small files whole (smallest first), the first and last chunk of
    /// each big one (an xp3's header and index), every chunk once, directories and empty
    /// files left out.
    #[test]
    fn start_set_small_whole_big_ends() {
        let m = 1u64 << 20;
        let mut tsv = String::from("vnvm-tree\t2\t64\n");
        tsv += &line("game.exe", 5 * m, &[("aa01", 4 * m), ("aa02", m)]);
        tsv += "d\t755\t0\tplugin\n";
        tsv += &line("plugin/x.dll", 100, &[("bb01", 100)]);
        tsv += &line("empty.txt", 0, &[]);
        tsv += &line("bgimage.xp3", 300 * m, &[("cc01", 4 * m), ("cc02", 4 * m), ("cc03", 292 * m)]);
        tsv += &line("voice.xp3", 9 * m, &[("dd01", 9 * m)]);
        tsv += &line("dup.dll", 100, &[("bb01", 100)]);
        tsv += "garbage line\n";
        let got = start_set(&tsv);
        let names: Vec<&str> = got.iter().map(|l| l.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, ["bb01", "aa01", "aa02", "dd01", "cc01", "cc03"]);
        assert_eq!(got[0], "chunks/bb/bb01");
    }

    proptest::proptest! {
        /// Any tree: what start_set names is chunks of its files, each once; every chunk of a
        /// file up to AHEAD_WHOLE is in it, and the ends of every bigger one.
        #[test]
        fn start_set_model(files in proptest::collection::vec((0u64..(20 << 20), proptest::collection::vec(0u16..64, 1..6)), 0..12)) {
            let mut tsv = String::from("vnvm-tree\t2\t64\n");
            for (k, (size, hs)) in files.iter().enumerate() {
                let c: Vec<(String, u64)> = hs.iter().map(|h| (format!("{h:04x}"), 1)).collect();
                let c: Vec<(&str, u64)> = c.iter().map(|(h, n)| (h.as_str(), *n)).collect();
                tsv += &line(&format!("f{k}"), *size, &c);
            }
            let got = start_set(&tsv);
            let set: std::collections::HashSet<&String> = got.iter().collect();
            proptest::prop_assert_eq!(set.len(), got.len());
            let name = |h: &u16| format!("chunks/{:02x}/{h:04x}", h >> 8);
            let all: std::collections::HashSet<String> = files.iter().flat_map(|f| f.1.iter().map(name)).collect();
            for l in &got {
                proptest::prop_assert!(all.contains(l));
            }
            for (size, hs) in &files {
                if *size <= AHEAD_WHOLE {
                    for h in hs {
                        proptest::prop_assert!(set.contains(&name(h)));
                    }
                } else {
                    proptest::prop_assert!(set.contains(&name(&hs[0])) && set.contains(&name(hs.last().unwrap())));
                }
            }
        }
    }

    /// A game's tree list asked for: its start set reaches the disk cache from S3 by itself
    /// (once per server run), and the game's reads of it then ask S3 for nothing.
    #[tokio::test]
    async fn tree_list_fetches_the_start_ahead() {
        let (fake, mut remote, dir) = crate::storage::gc_tests::serve(20).await;
        remote.set_disk(dir.join("s3"), 64 << 20);
        let mut tsv = String::from("vnvm-tree\t2\t64\n");
        let mut names = Vec::new();
        for k in 0..20u32 {
            let h = format!("{:02x}{k:062x}", k * 11 % 256);
            let data = zstd::encode_all(&format!("chunk {k}").into_bytes()[..], 3).unwrap();
            remote.put(&format!("chunks/{}/{h}", &h[..2]), &data).await.unwrap();
            tsv += &line(&format!("file{k}.dat"), 7, &[(&h, 7)]);
            names.push(format!("chunks/{}/{h}", &h[..2]));
        }
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        std::fs::write(dir.join("trees/game.tsv"), &tsv).unwrap();
        let before = fake.gets();
        let state = Store { local: dir.clone(), remote: Arc::new(Some(remote)), decoded: Default::default(), ahead: Default::default() };
        let app = Router::new().route("/store/{*path}", get(file)).with_state(state.clone());
        let (st, _, b) = get_(&app, "/store/trees/game.tsv", None).await;
        assert_eq!((st, b), (StatusCode::OK, tsv.clone().into_bytes()));
        let disk = dir.join("s3");
        let kept = || std::fs::read_dir(&disk).map_or(0, |d| d.flatten().map(|e| std::fs::read_dir(e.path()).map_or(0, |x| x.count())).sum::<usize>());
        for _ in 0..300 {
            if kept() == 20 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(kept(), 20);
        assert_eq!(fake.gets() - before, 20);
        // asked again (another start): nothing more
        get_(&app, "/store/trees/game.tsv", None).await;
        for n in &names {
            let (st, _, _) = get_(&app, &format!("/store/{n}"), None).await;
            assert_eq!(st, StatusCode::OK);
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert_eq!(fake.gets() - before, 20, "the reads came from the disk");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// /store-direct: a presigned URL the bucket answers, and the key that opens what it
    /// gives as the browser opens it (AES-256-GCM, nonce first, the UUID as associated data):
    /// the chunk as /store has it. 404 for what the bucket does not have, or without S3.
    #[tokio::test]
    async fn direct_gives_what_opens_the_chunk() {
        let (_fake, remote, dir) = crate::storage::gc_tests::serve(0).await;
        let raw: Vec<u8> = (0..200_000u32).map(|i| (i.wrapping_mul(2654435761) >> 11) as u8).collect();
        let stored = zstd::encode_all(&raw[..], 3).unwrap();
        let h = "ab".to_string() + &"5".repeat(30);
        remote.put(&format!("chunks/ab/{h}"), &stored).await.unwrap();
        let state = Store { local: dir.clone(), remote: Arc::new(Some(remote)), decoded: Default::default(), ahead: Default::default() };
        let app = Router::new().route("/store/{*path}", get(file)).route("/store-direct/{*path}", get(direct)).with_state(state);
        let (st, hd, body) = get_(&app, &format!("/store-direct/chunks/ab/{h}"), None).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(hd[header::CACHE_CONTROL], "no-store");
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let (url, key, object) = (v["url"].as_str().unwrap(), v["key"].as_str().unwrap(), v["object"].as_str().unwrap());
        assert!(url.contains("X-Amz-Signature=") && url.contains("X-Amz-Expires=21600"), "{url}");
        let cipher = reqwest::get(url).await.unwrap().bytes().await.unwrap().to_vec();
        let key: Vec<u8> = (0..32).map(|i| u8::from_str_radix(&key[i * 2..i * 2 + 2], 16).unwrap()).collect();
        let uuid = uuid::Uuid::parse_str(object).unwrap();
        let open = |key: &[u8], cipher: &[u8]| -> Option<Vec<u8>> {
            use ring::aead;
            let k = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_256_GCM, key).ok()?);
            let mut d = cipher.get(12..)?.to_vec();
            let p = k.open_in_place(aead::Nonce::assume_unique_for_key(cipher[..12].try_into().ok()?), aead::Aad::from(uuid.as_bytes()), &mut d).ok()?;
            Some(p.to_vec())
        };
        assert_eq!(open(&key, &cipher).unwrap(), stored);
        assert_eq!(zstd::decode_all(&stored[..]).unwrap(), raw);
        // another key, a changed byte: refused
        let mut bad = key.clone();
        bad[0] ^= 1;
        assert!(open(&bad, &cipher).is_none());
        let mut c2 = cipher.clone();
        c2[40] ^= 1;
        assert!(open(&key, &c2).is_none());
        // what the bucket does not have, what is not a chunk path
        for p in [format!("/store-direct/chunks/ab/{}", "6".repeat(32)), "/store-direct/trees/x.tsv".into(), "/store-direct/chunks/../x".into()] {
            assert_eq!(get_(&app, &p, None).await.0, StatusCode::NOT_FOUND, "{p}");
        }
        // a store without S3
        let none = Store { local: dir.clone(), remote: Arc::new(None), decoded: Default::default(), ahead: Default::default() };
        let app2 = Router::new().route("/store-direct/{*path}", get(direct)).with_state(none);
        assert_eq!(get_(&app2, &format!("/store-direct/chunks/ab/{h}"), None).await.0, StatusCode::NOT_IMPLEMENTED);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// POST /store-direct: many chunks at once, each answered as GET /store-direct/<path>
    /// answers it alone (the same key and object; the URL opens the same ciphertext); what the
    /// bucket lacks or is not a chunk path left out; too many refused; 501 without S3.
    #[tokio::test]
    async fn direct_many_answers_each_as_alone() {
        let (_fake, remote, dir) = crate::storage::gc_tests::serve(0).await;
        let mut names = Vec::new();
        for i in 0..5u8 {
            let raw = vec![i; 1000 + i as usize];
            let h = format!("{:02x}{}", 0xa0 + i, "7".repeat(30));
            remote.put(&format!("chunks/{}/{h}", &h[..2]), &zstd::encode_all(&raw[..], 3).unwrap()).await.unwrap();
            names.push(format!("chunks/{}/{h}", &h[..2]));
        }
        // (the first three in a delivery pack: the answers say where, the rest do not)
        assert_eq!(remote.pack_objects("g", &names[..3], 1 << 20).await.unwrap().packs, 1);
        let state = Store { local: dir.clone(), remote: Arc::new(Some(remote)), decoded: Default::default(), ahead: Default::default() };
        let app = Router::new().route("/store-direct/{*path}", get(direct)).route("/store-direct", axum::routing::post(direct_many)).with_state(state);
        let post = |body: String| {
            let app = app.clone();
            async move {
                let r = app.oneshot(Request::post("/store-direct").header("content-type", "application/json").body(Body::from(body)).unwrap()).await.unwrap();
                let st = r.status();
                (st, to_bytes(r.into_body(), usize::MAX).await.unwrap())
            }
        };
        let mut ask = names.clone();
        ask.push(format!("chunks/ab/{}", "6".repeat(32)));
        ask.push("trees/x.tsv".into());
        let (st, body) = post(serde_json::to_string(&ask).unwrap()).await;
        assert_eq!(st, StatusCode::OK);
        let all: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(all.as_object().unwrap().len(), names.len(), "{all}");
        for n in &names {
            let (st, _, one) = get_(&app, &format!("/store-direct/{n}"), None).await;
            assert_eq!(st, StatusCode::OK);
            let one: serde_json::Value = serde_json::from_slice(&one).unwrap();
            assert_eq!(all[n]["key"], one["key"]);
            assert_eq!(all[n]["object"], one["object"]);
            let a = reqwest::get(all[n]["url"].as_str().unwrap()).await.unwrap().bytes().await.unwrap();
            let b = reqwest::get(one["url"].as_str().unwrap()).await.unwrap().bytes().await.unwrap();
            assert_eq!(a, b);
            assert_eq!(all[n]["pack"], one["pack"]);
            if names[..3].contains(n) {
                let p = &all[n]["pack"];
                let body = reqwest::get(p["url"].as_str().unwrap()).await.unwrap().bytes().await.unwrap();
                let (o, l) = (p["offset"].as_u64().unwrap() as usize, p["len"].as_u64().unwrap() as usize);
                assert_eq!(&body[o..o + l], &a[..], "{n}: the pack's span is the object");
                assert!(p["id"].as_str().is_some_and(|id| p["url"].as_str().unwrap().contains(id)));
            } else {
                assert!(all[n].get("pack").is_none(), "{n}");
            }
        }
        let many = vec![names[0].clone(); DIRECT_MANY + 1];
        assert_eq!(post(serde_json::to_string(&many).unwrap()).await.0, StatusCode::PAYLOAD_TOO_LARGE);
        let none = Store { local: dir.clone(), remote: Arc::new(None), decoded: Default::default(), ahead: Default::default() };
        let app2 = Router::new().route("/store-direct", axum::routing::post(direct_many)).with_state(none);
        let r = app2.oneshot(Request::post("/store-direct").header("content-type", "application/json").body(Body::from("[]")).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::NOT_IMPLEMENTED);
        let _ = std::fs::remove_dir_all(dir);
    }
}
