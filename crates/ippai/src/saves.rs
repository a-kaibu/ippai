//! Versioned saves; local SQLite or encrypted S3, with optimistic concurrency.
use crate::{library::Library, storage::Storage};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rusqlite::OptionalExtension;
use std::{path::Path as FsPath, sync::Arc};

pub struct Saves {
    pub db: Library,
    pub remote: Option<Storage>,
    pub lock: tokio::sync::Mutex<()>,
}
impl Saves {
    pub fn new(store: &FsPath) -> anyhow::Result<Arc<Self>> {
        let db = Library::new(store)?;
        db.connect()?.execute_batch("CREATE TABLE IF NOT EXISTS saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB);")?;
        let remote = Storage::configured(db.clone())?;
        Ok(Arc::new(Self {
            db,
            remote,
            lock: tokio::sync::Mutex::new(()),
        }))
    }
    pub async fn read(&self, name: &str) -> anyhow::Result<Option<(i64, Vec<u8>)>> {
        let row: Option<(i64, Option<Vec<u8>>)> = self
            .db
            .connect()?
            .query_row(
                "SELECT revision,data FROM saves WHERE name=?",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let Some((revision, data)) = row else {
            return Ok(None);
        };
        let data = match data {
            Some(data) => data,
            None => self
                .remote
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("S3 configuration required"))?
                .get(&format!("saves/{name}/{revision}"))
                .await?
                .ok_or_else(|| anyhow::anyhow!("save object missing"))?
                .to_vec(),
        };
        Ok(Some((revision, data)))
    }
    pub async fn write(&self, name: &str, expected: i64, data: &[u8]) -> anyhow::Result<bool> {
        let _guard = self.lock.lock().await;
        let revision: i64 = self
            .db
            .connect()?
            .query_row("SELECT revision FROM saves WHERE name=?", [name], |r| {
                r.get(0)
            })
            .optional()?
            .unwrap_or(0);
        if revision != expected {
            return Ok(false);
        }
        let next = revision + 1;
        if let Some(remote) = &self.remote {
            remote.put(&format!("saves/{name}/{next}"), data).await?;
        }
        let local = self.remote.is_none().then_some(data);
        self.db.connect()?.execute("INSERT INTO saves(name,revision,data) VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET revision=excluded.revision,data=excluded.data", rusqlite::params![name,next,local])?;
        if let Some(remote) = &self.remote {
            remote.prune_save(name, next)?;
        }
        Ok(true)
    }
}
fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
fn failure(e: anyhow::Error) -> Response {
    eprintln!("saves: {e}");
    StatusCode::BAD_GATEWAY.into_response()
}
async fn get_save(State(s): State<Arc<Saves>>, Path(name): Path<String>) -> Response {
    if !valid(&name) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match s.read(&name).await {
        Ok(Some((rev, data))) => (
            [
                (header::ETAG, format!("\"{rev}\"")),
                (header::CACHE_CONTROL, "no-store".into()),
            ],
            data,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => failure(e),
    }
}
async fn put_save(
    State(s): State<Arc<Saves>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !valid(&name) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(expected) = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim_matches('"').parse::<i64>().ok())
    else {
        return StatusCode::PRECONDITION_REQUIRED.into_response();
    };
    match s.write(&name, expected, &body).await {
        Ok(true) => (
            StatusCode::NO_CONTENT,
            [(header::ETAG, format!("\"{}\"", expected + 1))],
        )
            .into_response(),
        Ok(false) => StatusCode::PRECONDITION_FAILED.into_response(),
        Err(e) => failure(e),
    }
}
pub fn router(s: Arc<Saves>) -> Router {
    Router::new()
        .route("/saves/{name}", get(get_save).put(put_save))
        .layer(axum::extract::DefaultBodyLimit::max(256 << 20))
        .with_state(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn http_save_preconditions_and_persistence() {
        let dir = std::env::temp_dir().join(format!("ippai-save-http-{}", uuid::Uuid::new_v4()));
        let db = Library::new(&dir).unwrap();
        db.connect()
            .unwrap()
            .execute_batch(
                "CREATE TABLE saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB);",
            )
            .unwrap();
        let state = Arc::new(Saves {
            db,
            remote: None,
            lock: Default::default(),
        });
        let app = router(state.clone());
        let request = |method, path: &str, revision: Option<&str>, bytes: &[u8]| {
            let mut r = Request::builder().method(method).uri(path);
            if let Some(revision) = revision {
                r = r.header(header::IF_MATCH, revision);
            }
            r.body(Body::from(bytes.to_vec())).unwrap()
        };
        let missing = app
            .clone()
            .oneshot(request("GET", "/saves/game", None, b""))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        for (revision, status) in [
            (None, StatusCode::PRECONDITION_REQUIRED),
            (Some("bad"), StatusCode::PRECONDITION_REQUIRED),
            (Some("\"1\""), StatusCode::PRECONDITION_FAILED),
            (Some("\"0\""), StatusCode::NO_CONTENT),
        ] {
            let r = app
                .clone()
                .oneshot(request("PUT", "/saves/game", revision, b"\0\xffsave"))
                .await
                .unwrap();
            assert_eq!(r.status(), status);
        }
        let stale = app
            .clone()
            .oneshot(request("PUT", "/saves/game", Some("\"0\""), b"stale"))
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
        let r = app
            .clone()
            .oneshot(request("GET", "/saves/game", None, b""))
            .await
            .unwrap();
        assert_eq!(r.headers()[header::ETAG], "\"1\"");
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            to_bytes(r.into_body(), 1024).await.unwrap().as_ref(),
            b"\0\xffsave"
        );
        let invalid = app
            .oneshot(request("PUT", "/saves/.hidden", Some("0"), b"invalid"))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let reopened = Saves::new(&dir).unwrap();
        assert_eq!(
            reopened.read("game").await.unwrap(),
            Some((1, b"\0\xffsave".to_vec()))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
