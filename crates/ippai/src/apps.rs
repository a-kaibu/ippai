//! Kept web apps, for save states of older builds (docs/savestate.md). A state is of the sake
//! build it was taken with, and loads only in that build with the page code of its time (the
//! module's exports and the page's host code change together). So the server keeps each web app
//! it has served in `<store>/apps/<key>/` (key: the app's sake build ids, build-id.txt) and
//! serves it at `/apps/<key>/`: its own files from there, everything else (saves, store, jit,
//! the API) as at the root, so an old app's relative URLs reach the same server. The page
//! asks `/apps/builds` (build id -> key) and opens a state of another build in its app.
//! Kept: the apps the states (`state.<game>.index` saves) name, and the newest KEEP_NEWEST.

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, HeaderValue, Request, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Component, Path as FsPath, PathBuf},
};
use tower::ServiceExt;

/// Apps kept besides those the states name (the newest first).
pub const KEEP_NEWEST: usize = 3;

/// The key of the web app in `web` (none: a web app without a sake build).
pub fn key(web: &FsPath) -> Option<String> {
    let ids = std::fs::read(web.join("sake/build-id.txt")).ok()?;
    Some(blake3::hash(&ids).to_hex()[..16].to_string())
}

/// Copies the web app in `web` to `<apps>/<key>/` unless it is there (the `.zst` files left
/// out); its key.
pub fn keep(apps: &FsPath, web: &FsPath) -> anyhow::Result<Option<String>> {
    let Some(key) = key(web) else { return Ok(None) };
    let dir = apps.join(&key);
    if dir.join(".done").exists() {
        // (the newest: its time is when it was last served)
        std::fs::write(dir.join(".done"), "")?;
        return Ok(Some(key));
    }
    let tmp = apps.join(format!(".{key}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    copy(web, &tmp)?;
    std::fs::write(tmp.join(".done"), "")?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::rename(&tmp, &dir)?;
    Ok(Some(key))
}

fn copy(from: &FsPath, to: &FsPath) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let (src, dst) = (e.path(), to.join(e.file_name()));
        if e.file_type()?.is_dir() {
            copy(&src, &dst)?;
        } else if src.extension().is_none_or(|x| x != "zst") {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

/// The kept apps' sake builds: build id -> key.
pub fn builds(apps: &FsPath) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for e in std::fs::read_dir(apps).into_iter().flatten().flatten() {
        let key = e.file_name().to_string_lossy().to_string();
        if key.starts_with('.') || !e.path().join(".done").exists() {
            continue;
        }
        let ids = std::fs::read_to_string(e.path().join("sake/build-id.txt")).unwrap_or_default();
        for l in ids.lines() {
            if let Some((_, id)) = l.trim().split_once(char::is_whitespace) {
                out.insert(id.trim().to_string(), key.clone());
            }
        }
    }
    out
}

/// Removes the kept apps no build in `used` is of, but the newest KEEP_NEWEST and `current`.
/// a temporary copy older than this was left by a server that died copying
const STALE_COPY: std::time::Duration = std::time::Duration::from_secs(3600);

pub fn prune(apps: &FsPath, used: &HashSet<String>, current: Option<&str>) {
    let builds = builds(apps);
    let wanted: HashSet<&String> = builds.iter().filter(|(id, _)| used.contains(*id)).map(|(_, k)| k).collect();
    let mut all: Vec<(std::time::SystemTime, String)> = std::fs::read_dir(apps)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let key = e.file_name().to_string_lossy().to_string();
            let t = std::fs::metadata(e.path().join(".done")).and_then(|m| m.modified()).ok()?;
            (!key.starts_with('.')).then_some((t, key))
        })
        .collect();
    // (a `.key.pid` copy a killed server left: one whole copy of the app per crash in the
    // first start's copy; none takes an hour)
    for e in std::fs::read_dir(apps).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        let stale = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t.elapsed().is_ok_and(|d| d > STALE_COPY));
        if name.starts_with('.') && name.len() > 1 && stale && e.path().is_dir() {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
    all.sort_by(|a, b| b.cmp(a));
    for (i, (_, key)) in all.iter().enumerate() {
        if i < KEEP_NEWEST || wanted.contains(key) || Some(key.as_str()) == current {
            continue;
        }
        let _ = std::fs::remove_dir_all(apps.join(key));
    }
}

/// The builds the states of `index` (a `state.<game>.index` save) are of.
pub fn state_builds(index: &[u8], into: &mut HashSet<String>) {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(index) else { return };
    for s in v["states"].as_array().into_iter().flatten() {
        if let Some(b) = s["build"].as_str() {
            into.insert(b.to_string());
        }
    }
}

#[derive(Clone)]
pub struct Apps {
    pub dir: PathBuf,
    /// the server's own routes, for what is not a file of the app
    pub root: Router,
}

pub async fn list(State(a): State<Apps>) -> Response {
    // (a directory and a file per app: off the runtime's threads)
    let dir = a.dir.clone();
    match tokio::task::spawn_blocking(move || builds(&dir)).await {
        Ok(b) => axum::Json(b).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// `/apps/<key>/<path>`: the kept app's file, else `/<path>` as the server answers it.
pub async fn file(State(a): State<Apps>, Path((key, path)): Path<(String, String)>, req: Request<Body>) -> Response {
    let safe = !key.is_empty()
        && key.bytes().all(|c| c.is_ascii_hexdigit())
        && FsPath::new(&path).components().all(|c| matches!(c, Component::Normal(_)));
    if !safe {
        return StatusCode::NOT_FOUND.into_response();
    }
    let dir = a.dir.join(&key);
    let file = dir.join(if path.is_empty() { "index.html" } else { &path });
    if dir.join(".done").exists() && file.is_file() && req.method() == axum::http::Method::GET {
        // Even already-kept page code returns to its own relative list URL. Redirect
        // that navigation here so old snapshots need no rewriting or runtime shim.
        if path == "play.html" || path == "index.html" {
            let query: Vec<_> = form_urlencoded::parse(req.uri().query().unwrap_or("").as_bytes()).collect();
            if !query.iter().any(|(k, _)| k == "game" || k == "run") {
                let mut kept = form_urlencoded::Serializer::new(String::new());
                for (k, v) in &query {
                    if k != "state" && k != "autostart" {
                        kept.append_pair(k, v);
                    }
                }
                let q = kept.finish();
                let url = if q.is_empty() { format!("/{path}") } else { format!("/{path}?{q}") };
                return axum::response::Redirect::temporary(&url).into_response();
            }
        }
        let versioned = req.uri().query().is_some_and(|q| q.split('&').any(|p| p.starts_with("v=")));
        let mut res = tower_http::services::ServeFile::new(file).oneshot(req).await.into_response();
        let h = res.headers_mut();
        h.insert("cross-origin-opener-policy", HeaderValue::from_static("same-origin"));
        h.insert("cross-origin-embedder-policy", HeaderValue::from_static("require-corp"));
        h.insert("cross-origin-resource-policy", HeaderValue::from_static("same-origin"));
        h.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(if versioned { "public, max-age=31536000, immutable" } else { "no-cache" }),
        );
        return res;
    }
    let (mut parts, body) = req.into_parts();
    let q = parts.uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let Ok(uri) = format!("/{path}{q}").parse::<Uri>() else { return StatusCode::BAD_REQUEST.into_response() };
    parts.uri = uri;
    // (a fresh request but for the peer: the route's own path parameters must not go along)
    let peer = parts.extensions.get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().cloned();
    parts.extensions = Default::default();
    if let Some(p) = peer {
        parts.extensions.insert(p);
    }
    match a.root.oneshot(Request::from_parts(parts, body)).await {
        Ok(r) => r.into_response(),
        Err(e) => match e {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(what: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ippai-apps-{what}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }
    fn web(root: &FsPath, name: &str, ids: &str) -> PathBuf {
        let w = root.join(name);
        std::fs::create_dir_all(w.join("sake")).unwrap();
        std::fs::write(w.join("sake/build-id.txt"), ids).unwrap();
        std::fs::write(w.join("sake/sake.wasm"), name).unwrap();
        std::fs::write(w.join("sake/sake.wasm.zst"), "compressed").unwrap();
        std::fs::write(w.join("play.html"), format!("<p>{name}")).unwrap();
        w
    }

    #[test]
    fn apps_are_kept_by_build_and_pruned_unless_a_state_needs_them() {
        let root = scratch("keep");
        let apps = root.join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        let mut keys = vec![];
        for i in 0..6 {
            let w = web(&root, &format!("w{i}"), &format!("sake {i:016x}\nsake-grow {:016x}\n", i + 100));
            keys.push(keep(&apps, &w).unwrap().unwrap());
            // (their times apart)
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // the same app again: the same key, not copied again
        assert_eq!(keep(&apps, &root.join("w2")).unwrap().unwrap(), keys[2]);
        let b = builds(&apps);
        assert_eq!(b[&format!("{:016x}", 1)], keys[1]);
        assert_eq!(b[&format!("{:016x}", 101)], keys[1]);
        assert!(!apps.join(&keys[1]).join("sake/sake.wasm.zst").exists());
        assert_eq!(std::fs::read_to_string(apps.join(&keys[1]).join("sake/sake.wasm")).unwrap(), "w1");
        // a state of w0's sake-grow; newest: w2 (served again), w5, w4
        let mut used = HashSet::new();
        state_builds(br#"{"states":[{"slot":0,"build":"0000000000000064"}]}"#, &mut used);
        prune(&apps, &used, None);
        let left: HashSet<String> = std::fs::read_dir(&apps).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        let want: HashSet<String> = [0, 2, 4, 5].iter().map(|&i| keys[i].clone()).collect();
        assert_eq!(left, want);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// review b00: a copy a killed server left (`.key.pid`, an hour old) goes at the next prune;
    /// one being copied now stays, and so do the kept apps
    #[test]
    fn copies_left_by_a_crash_are_removed() {
        let root = scratch("stale");
        let apps = root.join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        let key = keep(&apps, &web(&root, "w", "sake 0000000000000001\n")).unwrap().unwrap();
        let (old, new) = (apps.join(format!(".{key}.4242")), apps.join(format!(".{key}.4343")));
        for d in [&old, &new] {
            std::fs::create_dir_all(d.join("sake")).unwrap();
            std::fs::write(d.join("sake/sake.wasm"), "partial").unwrap();
        }
        let t = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
        std::fs::File::open(&old).unwrap().set_modified(t).unwrap();
        prune(&apps, &HashSet::new(), None);
        assert!(!old.exists());
        assert!(new.exists());
        assert!(apps.join(&key).join(".done").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_web_app_without_sake_is_not_kept() {
        let root = scratch("none");
        std::fs::create_dir_all(root.join("w")).unwrap();
        assert_eq!(keep(&root.join("apps"), &root.join("w")).unwrap(), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn an_apps_files_are_its_own_the_rest_the_servers() {
        let root = scratch("serve");
        let apps = root.join("apps");
        std::fs::create_dir_all(&apps).unwrap();
        let key = keep(&apps, &web(&root, "old", "sake 00000000000000aa\n")).unwrap().unwrap();
        let server = Router::new().route("/saves/{n}", axum::routing::get(|Path(n): Path<String>| async move { format!("save {n}") }));
        let app = Router::new()
            .route("/apps/builds", axum::routing::get(list))
            .route("/apps/{key}/{*path}", axum::routing::any(file))
            .with_state(Apps { dir: apps.clone(), root: server });
        let get = |u: String| {
            let app = app.clone();
            async move {
                let r = app.oneshot(Request::get(u).body(Body::empty()).unwrap()).await.unwrap();
                let s = r.status();
                let coep = r.headers().get("cross-origin-embedder-policy").is_some();
                (s, coep, String::from_utf8(axum::body::to_bytes(r.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap())
            }
        };
        assert_eq!(get(format!("/apps/{key}/play.html?game=g")).await, (StatusCode::OK, true, "<p>old".into()));
        for (query, target) in [
            ("", "/play.html"),
            ("?state=2&autostart&jit=0&memory=grow", "/play.html?jit=0&memory=grow"),
            ("?mount=a&mount=b&env=x%3Dy", "/play.html?mount=a&mount=b&env=x%3Dy"),
        ] {
            let res = app.clone().oneshot(Request::get(format!("/apps/{key}/play.html{query}")).body(Body::empty()).unwrap()).await.unwrap();
            assert_eq!(res.status(), StatusCode::TEMPORARY_REDIRECT);
            assert_eq!(res.headers()[header::LOCATION], target);
        }
        for query in ["?game=g&state=0", "?%67ame=g", "?%72un=a.exe&state=0"] {
            assert_eq!(get(format!("/apps/{key}/play.html{query}")).await.0, StatusCode::OK);
        }
        assert_eq!(get(format!("/apps/{key}/saves/game")).await.2, "save game");
        assert_eq!(get(format!("/apps/{key}/../secret")).await.0, StatusCode::NOT_FOUND);
        assert_eq!(get("/apps/builds".into()).await.2, format!(r#"{{"00000000000000aa":"{key}"}}"#));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
