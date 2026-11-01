//! Saved games backed up to S3 (or anything S3-compatible: RustFS, MinIO).
//!
//! - `PUT /saves/<name>`: the page's blob of a game's saved files (web/src/saves.ts), stored as
//!   `<prefix><name>.bin`
//! - `GET /saves/<name>`: that blob (404 if there is none, or no bucket is set up)
//!
//! Set up by environment variables: `VNVM_S3_BUCKET` (none: the routes answer 404),
//! `VNVM_S3_ENDPOINT` (e.g. `http://127.0.0.1:9000` for RustFS; none: AWS),
//! `VNVM_S3_REGION` (default us-east-1), `VNVM_S3_PREFIX` (default `vnvm/saves/`), and the
//! credentials as `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` (or the usual AWS profile).

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use s3::{creds::Credentials, Bucket, Region};
use std::sync::Arc;

struct S3 {
    bucket: Box<Bucket>,
    prefix: String,
}

fn setup() -> anyhow::Result<Option<S3>> {
    let Ok(name) = std::env::var("VNVM_S3_BUCKET") else { return Ok(None) };
    let region_name = std::env::var("VNVM_S3_REGION").unwrap_or_else(|_| "us-east-1".into());
    let region = match std::env::var("VNVM_S3_ENDPOINT") {
        Ok(endpoint) => Region::Custom { region: region_name, endpoint },
        Err(_) => region_name.parse()?,
    };
    let custom = matches!(region, Region::Custom { .. });
    let mut bucket = Bucket::new(&name, region, Credentials::default()?)?;
    if custom {
        bucket = bucket.with_path_style();
    }
    let prefix = std::env::var("VNVM_S3_PREFIX").unwrap_or_else(|_| "vnvm/saves/".into());
    eprintln!("vnvm-server: saved games go to s3://{name}/{prefix}");
    Ok(Some(S3 { bucket, prefix }))
}

fn valid(name: &str) -> bool {
    !name.is_empty() && name.len() <= 200 && name.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c)) && !name.starts_with('.')
}

type St = Arc<Option<S3>>;

async fn put_save(State(s3): State<St>, Path(name): Path<String>, body: Bytes) -> StatusCode {
    let Some(s3) = s3.as_ref() else { return StatusCode::NOT_FOUND };
    if !valid(&name) {
        return StatusCode::BAD_REQUEST;
    }
    match s3.bucket.put_object(format!("{}{name}.bin", s3.prefix), &body).await {
        Ok(r) if r.status_code() / 100 == 2 => StatusCode::NO_CONTENT,
        Ok(r) => {
            eprintln!("vnvm-server: saving {name} to S3: HTTP {}", r.status_code());
            StatusCode::BAD_GATEWAY
        }
        Err(e) => {
            eprintln!("vnvm-server: saving {name} to S3: {e}");
            StatusCode::BAD_GATEWAY
        }
    }
}

async fn get_save(State(s3): State<St>, Path(name): Path<String>) -> Response {
    let Some(s3) = s3.as_ref() else { return StatusCode::NOT_FOUND.into_response() };
    if !valid(&name) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match s3.bucket.get_object(format!("{}{name}.bin", s3.prefix)).await {
        Ok(r) if r.status_code() == 200 => r.bytes().clone().into_response(),
        Ok(r) if r.status_code() == 404 => StatusCode::NOT_FOUND.into_response(),
        Ok(r) => {
            eprintln!("vnvm-server: reading {name} from S3: HTTP {}", r.status_code());
            StatusCode::BAD_GATEWAY.into_response()
        }
        // (rust-s3 reports 404 as an error)
        Err(s3::error::S3Error::HttpFailWithBody(404, _)) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => {
            eprintln!("vnvm-server: reading {name} from S3: {e}");
            StatusCode::BAD_GATEWAY.into_response()
        }
    }
}

pub fn router() -> anyhow::Result<Router> {
    let s3: St = Arc::new(setup()?);
    Ok(Router::new()
        .route("/saves/{name}", get(get_save).put(put_save))
        .layer(axum::extract::DefaultBodyLimit::max(256 << 20))
        .with_state(s3))
}
