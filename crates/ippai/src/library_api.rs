//! The user's records (library.rs) over HTTP:
//! - `GET`/`PUT /api/library/<game>`: favorite and note
//! - `GET /api/favorites`, `GET`/`PUT /api/sessions`: play time; `DELETE /api/sessions`
//!   forgets it all (the statistics' reset: 204, favorites and notes stay)
//! - `PUT /api/library/<game>/meta` `{"url": ...}`: fetch the game's metadata from the page the
//!   user pasted (批評空間 / VNDB, meta_url.rs) in the background; 202 with the state, 400 when
//!   the address names no game page, 404 for a game not in the store. `GET` gives the state
//!   (`null` before any). The result stands in for the store's (`GET /games`: Library::apply_meta)
//! - `GET /api/library/<game>/cover`: the cover that came with it
//! - `GET /api/library/<game>/candidates[?q=...]`: the ErogameScape games most like the game's
//!   names (its title, program and folders: egs_match.rs), most alike first, `best` marking the
//!   one taken without asking; or those a typed `q` finds (Index::search). From the whole list
//!   the server keeps (Library::egs_index)
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

/// The game database's entry (gamedb.rs) or ErogameScape / VNDB's (meta.rs), the cover too when
/// the entry has one.
fn fetch(id: &str, gamedb: &std::path::Path) -> anyhow::Result<Fetched> {
    let info = meta::lookup("", Some(id), Some(gamedb))?;
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

async fn reset_stats(State(Api { db, .. }): State<Api>) -> Result<StatusCode, StatusCode> {
    tokio::task::spawn_blocking(move || db.clear_sessions()).await.map_err(error)?.map_err(error)?;
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
    let gamedb = db.0.parent().map_or_else(|| std::path::PathBuf::from("."), |p| p.to_path_buf());
    router_with(db, Arc::new(move |id: &str| fetch(id, &gamedb)))
}

pub fn router_with(db: Library, fetch: Fetcher) -> Router {
    Router::new()
        .route("/api/favorites", get(favorites))
        .route("/api/library/{game}", get(profile).put(update))
        .route("/api/library/{game}/meta", get(meta_state).put(meta_fetch))
        .route("/api/library/{game}/cover", get(cover))
        .route("/api/library/{game}/candidates", get(candidates))
        .route("/api/sessions", put(session).get(stats).delete(reset_stats))
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

#[derive(Deserialize)]
struct CandidatesQuery {
    q: Option<String>,
}

#[derive(serde::Serialize)]
struct Candidate {
    #[serde(flatten)]
    hit: crate::egs_match::Hit,
    best: bool,
}

async fn candidates(
    State(Api { db, .. }): State<Api>,
    Path(game): Path<String>,
    axum::extract::Query(cq): axum::extract::Query<CandidatesQuery>,
) -> Response {
    if !valid(&game) || cq.q.as_ref().is_some_and(|q| q.len() > 500) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(entry) = db.0.parent().map(|store| store.join("games").join(format!("{game}.json"))) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let found = tokio::task::spawn_blocking(move || -> anyhow::Result<Option<Vec<Candidate>>> {
        let Ok(text) = std::fs::read(&entry) else { return Ok(None) };
        let mut g: serde_json::Value = serde_json::from_slice(&text)?;
        let idx = db.egs_index()?;
        if let Some(q) = cq.q.filter(|q| !q.trim().is_empty()) {
            // (typed: what it finds; nothing is taken without asking)
            return Ok(Some(idx.search(&q, 8).into_iter().map(|hit| Candidate { best: false, hit }).collect()));
        }
        db.apply_settings(&mut g)?;
        // (not the metadata's title: that may be the wrong game this is to correct)
        let queries = crate::egs_match::queries(g["title"].as_str().unwrap_or(""), g["run"].as_str().unwrap_or(""));
        let best = idx.best(&queries);
        let mut hits = idx.candidates(&queries, 8);
        if let Some(b) = best.as_ref().filter(|b| !hits.iter().any(|h| h.id == b.id)) {
            hits.pop();
            hits.insert(1.min(hits.len()), b.clone());
        }
        let best = best.map(|b| b.id);
        Ok(Some(hits.into_iter().map(|hit| Candidate { best: Some(hit.id) == best, hit }).collect()))
    })
    .await;
    match found {
        Ok(Ok(Some(c))) => Json(c).into_response(),
        Ok(Ok(None)) => StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => (StatusCode::BAD_GATEWAY, format!("作品一覧を取得できませんでした：{e:#}")).into_response(),
        Err(e) => error(e).into_response(),
    }
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

    /// DELETE /api/sessions: the statistics empty, recording goes on.
    #[tokio::test]
    async fn statistics_reset() {
        let dir = store();
        let app = router_with(Library::new(&dir).unwrap(), Arc::new(|_: &str| unreachable!()));
        for at in [1000, 2000] {
            let id = uuid::Uuid::new_v4();
            let body = format!(r#"{{"id":"{id}","game":"anemoi","started_ms":{at},"elapsed_ms":500}}"#);
            assert_eq!(call(&app, "PUT", "/api/sessions", Some(&body)).await.0, StatusCode::NO_CONTENT);
        }
        let count = |b: Vec<u8>| serde_json::from_slice::<Vec<serde_json::Value>>(&b).unwrap().len();
        assert_eq!(count(call(&app, "GET", "/api/sessions", None).await.1), 2);
        assert_eq!(call(&app, "DELETE", "/api/sessions", None).await.0, StatusCode::NO_CONTENT);
        assert_eq!(count(call(&app, "GET", "/api/sessions", None).await.1), 0);
        assert_eq!(call(&app, "DELETE", "/api/sessions", None).await.0, StatusCode::NO_CONTENT);
        let body = format!(r#"{{"id":"{}","game":"anemoi","started_ms":3000,"elapsed_ms":500}}"#, uuid::Uuid::new_v4());
        assert_eq!(call(&app, "PUT", "/api/sessions", Some(&body)).await.0, StatusCode::NO_CONTENT);
        assert_eq!(count(call(&app, "GET", "/api/sessions", None).await.1), 1);
        let _ = std::fs::remove_dir_all(dir);
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

    /// 候補: from the list the server keeps (fresh: nothing fetched), by the game's names or
    /// `q`; the one taken without asking marked.
    #[tokio::test]
    async fn candidates_from_the_kept_list() {
        let dir = store();
        std::fs::write(dir.join("games/loopers.json"), r#"{"id":"loopers","title":"LOOPERS DL版","run":"Z:\\ippai\\game\\SiglusEngine.exe","args":[],"mounts":[],"env":[]}"#).unwrap();
        let db = Library::new(&dir).unwrap();
        db.set_egs_games(&[(1, "LOOPERS".into(), 800), (2, "LOOPERS+".into(), 40), (3, "アネモイ".into(), 500), (4, "サクラノ詩 -櫻の森の上を舞う-".into(), 5000), (5, "サクラノ詩 春ノ雪".into(), 30)]).unwrap();
        let app = router_with(db, Arc::new(|_: &str| anyhow::bail!("not here")));
        let get = |uri: &'static str| {
            let app = app.clone();
            async move {
                let (s, b, _) = call(&app, "GET", uri, None).await;
                (s, if s == StatusCode::OK { serde_json::from_slice::<serde_json::Value>(&b).unwrap() } else { serde_json::Value::Null })
            }
        };
        let (s, c) = get("/api/library/loopers/candidates").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!((c[0]["id"].as_u64(), c[0]["best"].as_bool(), c[0]["score"].as_f64()), (Some(1), Some(true), Some(1.0)), "{c}");
        assert!(c.as_array().unwrap().iter().all(|h| h["id"] != 4), "nothing unlike it: {c}");
        // typed: names with it in, most voted first, none taken without asking
        let (_, c) = get("/api/library/anemoi/candidates?q=%E3%82%B5%E3%82%AF%E3%83%A9").await;
        let ids: Vec<u64> = c.as_array().unwrap().iter().map(|h| h["id"].as_u64().unwrap()).collect();
        assert_eq!(ids[..2], [4, 5], "{c}");
        assert!(c.as_array().unwrap().iter().all(|h| h["best"] == false), "{c}");
        assert_eq!(get("/api/library/nothere/candidates").await.0, StatusCode::NOT_FOUND);
        assert_eq!(get("/api/library/.x/candidates").await.0, StatusCode::BAD_REQUEST);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
