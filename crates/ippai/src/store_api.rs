use crate::storage::Storage;
use axum::{
    extract::{Path, State},
    http::StatusCode,
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
}

async fn file(State(s): State<Store>, Path(path): Path<String>) -> Response {
    let p = std::path::Path::new(&path);
    if !p.components().all(|c| matches!(c, Component::Normal(_)))
        || !matches!(path.split('/').next(), Some("chunks" | "trees" | "games"))
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    // The DB and keys are never reachable through /store.
    if let Some(remote) = s.remote.as_ref() {
        match remote.get(&path).await {
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
    };
    Ok(Router::new()
        .route("/store/{*path}", get(file))
        .with_state(state))
}
