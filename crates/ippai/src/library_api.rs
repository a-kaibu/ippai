use crate::library::{Library, Profile, Session};
use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, put},
    Json, Router,
};

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
    State(db): State<Library>,
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
    State(db): State<Library>,
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
    State(db): State<Library>,
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

async fn stats(State(db): State<Library>) -> Result<Json<Vec<Session>>, StatusCode> {
    tokio::task::spawn_blocking(move || db.sessions())
        .await
        .map_err(error)?
        .map(Json)
        .map_err(error)
}

pub fn router(db: Library) -> Router {
    Router::new()
        .route("/api/favorites", get(favorites))
        .route("/api/library/{game}", get(profile).put(update))
        .route("/api/sessions", put(session).get(stats))
        .with_state(db)
}

async fn favorites(State(db): State<Library>) -> Result<Json<Vec<String>>, StatusCode> {
    tokio::task::spawn_blocking(move || db.favorites())
        .await
        .map_err(error)?
        .map(Json)
        .map_err(error)
}
