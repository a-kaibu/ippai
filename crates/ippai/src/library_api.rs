//! The user's records (library.rs) over HTTP:
//! - `GET`/`PUT /api/library/<game>`: favorite and note
//! - `GET /api/favorites`, `GET`/`PUT /api/sessions`: play time
//! - `PUT /api/library/<game>/meta` `{"url": ...}`: fetch the game's metadata from the page the
//!   user pasted (批評空間 / VNDB, meta_url.rs) in the background; 202 with the state, 400 when
//!   the address names no game page, 404 for a game not in the store. `GET` gives the state
//!   (`null` before any). The result stands in for the store's (`GET /games`: Library::apply_meta)
//! - `GET /api/library/<game>/cover`: the cover that came with it
use crate::library::{Library, MetaStatus, Profile, Session};
use crate::meta;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use serde::Deserialize;
use std::sync::Arc;

/// What a fetch brings: the info and, when it could be downloaded, the cover (bytes, media type).
pub struct Fetched {
    pub info: meta::Info,
    pub cover: Option<(Vec<u8>, String)>,
}

/// Fetches the entry `meta_url::entry_id` named (blocking: run off the async threads).
pub type Fetcher = Arc<dyn Fn(&str) -> anyhow::Result<Fetched> + Send + Sync>;

/// ErogameScape / VNDB (meta.rs), the cover too when the entry has one.
fn fetch(id: &str) -> anyhow::Result<Fetched> {
    let info = meta::lookup("", Some(id))?;
    let cover = if info.cover_url.is_empty() {
        None
    } else {
        match meta::download_cover(&info.cover_url) {
            Ok((b, ext)) => Some((b, format!("image/{}", if ext == "jpg" { "jpeg" } else { ext }))),
            Err(e) => {
                eprintln!("meta {id}: cover: {e:#}");
                None
            }
        }
    };
    Ok(Fetched { info, cover })
}

#[derive(Clone)]
struct Api {
    db: Library,
    fetch: Fetcher,
}

fn error(e: impl std::fmt::Display) -> StatusCode {
    eprintln!("library: {e}");
    StatusCode::INTERNAL_SERVER_ERROR
}

fn valid(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && !s.starts_with('.')
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

async fn profile(
    State(Api { db, .. }): State<Api>,
    Path(game): Path<String>,
) -> Result<Json<Profile>, StatusCode> {
    if !valid(&game) {
        return Err(StatusCode::BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || db.profile(&game))
        .await
        .map_err(error)?
        .map(Json)
        .map_err(error)
}

async fn update(
    State(Api { db, .. }): State<Api>,
    Path(game): Path<String>,
    Json(p): Json<Profile>,
) -> Result<StatusCode, StatusCode> {
    if !valid(&game) || p.note.len() > 100_000 {
        return Err(StatusCode::BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || db.set_profile(&game, &p))
        .await
        .map_err(error)?
        .map_err(error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn session(
    State(Api { db, .. }): State<Api>,
    Json(s): Json<Session>,
) -> Result<StatusCode, StatusCode> {
    if !valid(&s.game) {
        return Err(StatusCode::BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || db.session(&s))
        .await
        .map_err(error)?
        .map_err(|_| StatusCode::BAD_REQUEST)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn stats(State(Api { db, .. }): State<Api>) -> Result<Json<Vec<Session>>, StatusCode> {
    tokio::task::spawn_blocking(move || db.sessions())
        .await
        .map_err(error)?
        .map(Json)
        .map_err(error)
}

pub fn router(db: Library) -> Router {
    router_with(db, Arc::new(fetch))
}

pub fn router_with(db: Library, fetch: Fetcher) -> Router {
    Router::new()
        .route("/api/favorites", get(favorites))
        .route("/api/library/{game}", get(profile).put(update))
        .route("/api/library/{game}/meta", get(meta_state).put(meta_fetch))
        .route("/api/library/{game}/cover", get(cover))
        .route("/api/sessions", put(session).get(stats))
        .with_state(Api { db, fetch })
}

#[derive(Deserialize)]
struct MetaRequest {
    url: String,
}

async fn meta_state(State(Api { db, .. }): State<Api>, Path(game): Path<String>) -> Result<Json<Option<MetaStatus>>, StatusCode> {
    if !valid(&game) {
        return Err(StatusCode::BAD_REQUEST);
    }
    tokio::task::spawn_blocking(move || db.meta_status(&game)).await.map_err(error)?.map(Json).map_err(error)
}

async fn meta_fetch(State(Api { db, fetch }): State<Api>, Path(game): Path<String>, Json(r): Json<MetaRequest>) -> Response {
    if !valid(&game) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(id) = crate::meta_url::entry_id(&r.url) else {
        return (StatusCode::BAD_REQUEST, "批評空間か VNDB の作品ページの URL を入れてください").into_response();
    };
    // (the database is the store's library.sqlite3: the game's entry is beside it)
    let known = db.0.parent().is_some_and(|store| store.join("games").join(format!("{game}.json")).is_file());
    if !known {
        return StatusCode::NOT_FOUND.into_response();
    }
    let url = r.url.trim().to_string();
    let started = {
        let (db, game) = (db.clone(), game.clone());
        tokio::task::spawn_blocking(move || {
            let job = db.begin_meta(&game, &url)?;
            Ok::<_, anyhow::Error>((job, db.meta_status(&game)?))
        })
        .await
    };
    let (job, status) = match started {
        Ok(Ok(x)) => x,
        Ok(Err(e)) => return error(e).into_response(),
        Err(e) => return error(e).into_response(),
    };
    tokio::task::spawn_blocking(move || {
        let result = fetch(&id).and_then(|f| Ok((serde_json::to_string(&f.info)?, f.cover))).map_err(|e| format!("{e:#}"));
        if let Err(e) = &result {
            eprintln!("meta {game} ({id}): {e}");
        }
        if let Err(e) = db.finish_meta(&game, &job, result) {
            eprintln!("meta {game}: {e:#}");
        }
    });
    (StatusCode::ACCEPTED, Json(status)).into_response()
}

async fn cover(State(Api { db, .. }): State<Api>, Path(game): Path<String>) -> Response {
    if !valid(&game) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match tokio::task::spawn_blocking(move || db.meta_cover(&game)).await {
        Ok(Ok(Some((bytes, ty)))) => (
            [(header::CONTENT_TYPE, ty), (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string())],
            bytes,
        )
            .into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => error(e).into_response(),
        Err(e) => error(e).into_response(),
    }
}

async fn favorites(State(Api { db, .. }): State<Api>) -> Result<Json<Vec<String>>, StatusCode> {
    tokio::task::spawn_blocking(move || db.favorites())
        .await
        .map_err(error)?
        .map(Json)
        .map_err(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use std::sync::Mutex;
    use tower::ServiceExt;

    fn store() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ippai-meta-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(d.join("games")).unwrap();
        std::fs::write(d.join("games/anemoi.json"), r#"{"id":"anemoi","title":"anemoi","run":"C:\\a.exe","args":[],"mounts":[],"env":[]}"#).unwrap();
        d
    }

    fn info(title: &str) -> meta::Info {
        meta::Info { source: "erogamescape".into(), egs_id: Some(35435), title: title.into(), brand: "Key".into(), ..Default::default() }
    }

    async fn call(app: &Router, method: &str, uri: &str, body: Option<&str>) -> (StatusCode, Vec<u8>, Option<String>) {
        let mut r = Request::builder().method(method).uri(uri);
        if body.is_some() {
            r = r.header("content-type", "application/json");
        }
        let res = app.clone().oneshot(r.body(body.map_or(Body::empty(), |b| Body::from(b.to_string()))).unwrap()).await.unwrap();
        let ty = res.headers().get(header::CONTENT_TYPE).map(|v| v.to_str().unwrap().to_string());
        (res.status(), to_bytes(res.into_body(), 1 << 24).await.unwrap().to_vec(), ty)
    }

    async fn state(app: &Router, game: &str) -> serde_json::Value {
        let (s, b, _) = call(app, "GET", &format!("/api/library/{game}/meta"), None).await;
        assert_eq!(s, StatusCode::OK);
        serde_json::from_slice(&b).unwrap()
    }

    /// Waits for the background fetch to end.
    async fn settled(app: &Router, game: &str) -> serde_json::Value {
        for _ in 0..500 {
            let s = state(app, game).await;
            if s["state"] != "pending" {
                return s;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("still pending");
    }

    fn listed(db: &Library) -> serde_json::Value {
        let mut g: serde_json::Value = serde_json::from_slice(&std::fs::read(db.0.parent().unwrap().join("games/anemoi.json")).unwrap()).unwrap();
        db.apply_settings(&mut g).unwrap();
        g
    }

    /// A pasted 批評空間 page: pending at once, then the game's entry in the list carries what it
    /// fetched (meta and cover), the cover served with its type.
    #[tokio::test]
    async fn fetches_in_the_background_and_the_list_shows_it() {
        let dir = store();
        let db = Library::new(&dir).unwrap();
        let seen = Arc::new(Mutex::new(vec![]));
        let gate = Arc::new(std::sync::Barrier::new(2));
        let fetch: Fetcher = {
            let (seen, gate) = (seen.clone(), gate.clone());
            Arc::new(move |id: &str| {
                seen.lock().unwrap().push(id.to_string());
                gate.wait();
                Ok(Fetched { info: info("アネモイ"), cover: Some((b"\xff\xd8jpeg".to_vec(), "image/jpeg".into())) })
            })
        };
        let app = router_with(db.clone(), fetch);
        assert_eq!(state(&app, "anemoi").await, serde_json::Value::Null);
        let url = "https://erogamescape.dyndns.org/~ap2/ero/toukei_kaiseki/game.php?game=35435";
        let (s, b, _) = call(&app, "PUT", "/api/library/anemoi/meta", Some(&serde_json::json!({ "url": url }).to_string())).await;
        assert_eq!(s, StatusCode::ACCEPTED);
        let started: serde_json::Value = serde_json::from_slice(&b).unwrap();
        assert_eq!((started["state"].as_str(), started["url"].as_str()), (Some("pending"), Some(url)));
        assert_eq!(state(&app, "anemoi").await["state"], "pending", "still fetching");
        assert!(listed(&db).get("meta").is_none(), "nothing in the list before it ends");
        tokio::task::spawn_blocking(move || gate.wait()).await.unwrap();
        let done = settled(&app, "anemoi").await;
        assert_eq!(done["state"], "ok", "{done}");
        assert_eq!(*seen.lock().unwrap(), ["egs:35435"]);
        let g = listed(&db);
        assert_eq!(g["meta"]["title"], "アネモイ");
        assert_eq!(g["meta"]["brand"], "Key");
        let src = g["cover_src"].as_str().unwrap().to_string();
        assert!(src.starts_with("api/library/anemoi/cover?v="), "{src}");
        let (s, b, ty) = call(&app, "GET", &format!("/{}", src), None).await;
        assert_eq!((s, b.as_slice(), ty.as_deref()), (StatusCode::OK, &b"\xff\xd8jpeg"[..], Some("image/jpeg")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn refuses_what_names_no_game_page_or_no_game() {
        let dir = store();
        let db = Library::new(&dir).unwrap();
        let calls = Arc::new(Mutex::new(0));
        let fetch: Fetcher = {
            let calls = calls.clone();
            Arc::new(move |_: &str| {
                *calls.lock().unwrap() += 1;
                anyhow::bail!("unreachable")
            })
        };
        let app = router_with(db, fetch);
        for (game, url, want) in [
            ("anemoi", "https://example.com/game.php?game=1", StatusCode::BAD_REQUEST),
            ("anemoi", "アネモイ", StatusCode::BAD_REQUEST),
            ("nothere", "https://vndb.org/v1", StatusCode::NOT_FOUND),
            (".hidden", "https://vndb.org/v1", StatusCode::BAD_REQUEST),
        ] {
            let (s, _, _) = call(&app, "PUT", &format!("/api/library/{game}/meta"), Some(&serde_json::json!({ "url": url }).to_string())).await;
            assert_eq!(s, want, "{game} {url}");
        }
        let (s, _, _) = call(&app, "PUT", "/api/library/anemoi/meta", Some("{}")).await;
        assert!(s.is_client_error());
        let (s, _, _) = call(&app, "GET", "/api/library/anemoi/cover", None).await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(*calls.lock().unwrap(), 0, "nothing fetched");
        assert_eq!(state(&app, "anemoi").await, serde_json::Value::Null);
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A failed fetch says why and leaves what an earlier one brought; one with no cover keeps the
    /// cover before.
    #[tokio::test]
    async fn a_failure_keeps_the_last_result() {
        let dir = store();
        let db = Library::new(&dir).unwrap();
        let next: Arc<Mutex<Vec<anyhow::Result<Fetched>>>> = Arc::new(Mutex::new(vec![
            Err(anyhow::anyhow!("vndb: timed out")),
            Ok(Fetched { info: info("二回目"), cover: None }),
            Ok(Fetched { info: info("一回目"), cover: Some((b"png".to_vec(), "image/png".into())) }),
        ]));
        let fetch: Fetcher = {
            let next = next.clone();
            Arc::new(move |_: &str| next.lock().unwrap().pop().unwrap())
        };
        let app = router_with(db.clone(), fetch);
        let put = |url: &'static str| {
            let app = app.clone();
            async move { call(&app, "PUT", "/api/library/anemoi/meta", Some(&serde_json::json!({ "url": url }).to_string())).await.0 }
        };
        assert_eq!(put("https://vndb.org/v5").await, StatusCode::ACCEPTED);
        assert_eq!(settled(&app, "anemoi").await["state"], "ok");
        assert_eq!(put("egs:7").await, StatusCode::ACCEPTED);
        assert_eq!(settled(&app, "anemoi").await["state"], "ok");
        let g = listed(&db);
        assert_eq!(g["meta"]["title"], "二回目");
        assert!(g["cover_src"].is_string(), "the first cover stays");
        assert_eq!(put("https://vndb.org/v6").await, StatusCode::ACCEPTED);
        let s = settled(&app, "anemoi").await;
        assert_eq!((s["state"].as_str(), s["error"].as_str(), s["url"].as_str()), (Some("error"), Some("vndb: timed out"), Some("https://vndb.org/v6")));
        assert_eq!(listed(&db)["meta"]["title"], "二回目");
        let (_, b, ty) = call(&app, "GET", "/api/library/anemoi/cover", None).await;
        assert_eq!((b.as_slice(), ty.as_deref()), (&b"png"[..], Some("image/png")));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
