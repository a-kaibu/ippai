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

pub fn router(local: PathBuf) -> anyhow::Result<Router> {
    let db = crate::library::Library::new(&local)?;
    let state = Store {
        local,
        remote: Arc::new(Storage::configured(db)?),
        decoded: Default::default(),
    };
    Ok(Router::new()
        .route("/store/{*path}", get(file))
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
        let state = Store { local: dir.clone(), remote: Arc::new(None), decoded: Default::default() };
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
}
