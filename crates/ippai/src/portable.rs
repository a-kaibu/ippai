//! .ippai v1: bounded ZIP with a JSON manifest and opaque save blobs, no game binaries.
use crate::{
    library::{Profile, Session},
    saves::Saves,
};
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    io::{Cursor, Read, Write},
    path::Path,
    sync::Arc,
};

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    game: Value,
    fingerprints: Vec<String>,
    profile: Profile,
    sessions: Vec<Session>,
}

fn game(store: &Path, id: &str) -> Result<Value> {
    ensure!(
        !id.is_empty()
            && !id.starts_with('.')
            && id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c)),
        "invalid game id"
    );
    Ok(serde_json::from_slice(&std::fs::read(
        store.join("games").join(format!("{id}.json")),
    )?)?)
}

fn fingerprints(store: &Path, g: &Value) -> Result<Vec<String>> {
    g["mounts"]
        .as_array()
        .context("missing mounts")?
        .iter()
        .map(|m| {
            let (_, tree) = m
                .as_str()
                .context("invalid mount")?
                .split_once('=')
                .context("invalid mount")?;
            ensure!(
                !tree.contains('/') && !tree.contains('\\') && !tree.starts_with('.'),
                "invalid tree"
            );
            let manifest =
                std::fs::read_to_string(store.join("trees").join(format!("{tree}.tsv")))?;
            let mut h = blake3::Hasher::new();
            for line in manifest.lines().filter(|l| l.starts_with("f\t")) {
                let f: Vec<_> = line.split('\t').collect();
                ensure!(f.len() == 6, "invalid tree manifest");
                // Ignore host permissions and mtimes; compare paths, sizes and content hashes.
                h.update(f[3..].join("\t").as_bytes());
                h.update(b"\n");
            }
            Ok(h.finalize().to_hex().to_string())
        })
        .collect()
}

fn save_name(g: &Value) -> Result<String> {
    let run = g["run"].as_str().context("missing run")?;
    let mounts = g["mounts"]
        .as_array()
        .context("missing mounts")?
        .iter()
        .map(|m| m.as_str().context("invalid mount"))
        .collect::<Result<Vec<_>>>()?
        .join(":");
    let mut hash = 0x811c9dc5u32;
    for b in mounts.bytes() {
        hash = (hash ^ u32::from(b)).wrapping_mul(0x01000193);
    }
    let run: String = run
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    Ok(format!("{run}-{hash:x}"))
}

pub async fn export(store: &Path, id: &str, saves: &Saves) -> Result<Vec<u8>> {
    let mut g = game(store, id)?;
    saves.db.apply_settings(&mut g)?;
    let save = save_name(&g)?;
    let m = Manifest {
        version: 1,
        fingerprints: fingerprints(store, &g)?,
        game: g,
        profile: saves.db.profile(id)?,
        sessions: saves
            .db
            .sessions()?
            .into_iter()
            .filter(|s| s.game == id)
            .collect(),
    };
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("manifest.json", options)?;
    zip.write_all(&serde_json::to_vec(&m)?)?;
    for (entry, name) in [
        ("saves.bin", save.clone()),
        ("web.bin", format!("{save}-web")),
    ] {
        if let Some((_, data)) = saves.read(&name).await? {
            zip.start_file(entry, options)?;
            zip.write_all(&data)?;
        }
    }
    Ok(zip.finish()?.into_inner())
}

pub async fn import(store: &Path, id: &str, bytes: &[u8], saves: &Saves) -> Result<()> {
    let target = game(store, id)?;
    let mut zip = zip::ZipArchive::new(Cursor::new(bytes))?;
    ensure!(zip.len() <= 3, "too many package entries");
    let mut entries = std::collections::HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = f.name().to_string();
        ensure!(
            matches!(name.as_str(), "manifest.json" | "saves.bin" | "web.bin"),
            "unknown package entry"
        );
        ensure!(!entries.contains_key(&name), "duplicate package entry");
        let limit = if name == "manifest.json" {
            4 << 20
        } else {
            256 << 20
        };
        ensure!(f.size() <= limit, "package entry too large");
        let mut data = Vec::new();
        (&mut f).take(limit + 1).read_to_end(&mut data)?;
        ensure!(data.len() as u64 <= limit, "package entry too large");
        entries.insert(name, data);
    }
    let m: Manifest =
        serde_json::from_slice(entries.get("manifest.json").context("missing manifest")?)?;
    ensure!(m.version == 1, "unsupported package version");
    ensure!(
        fingerprints(store, &target)? == m.fingerprints,
        "game content does not match package"
    );
    // Retain the original guest paths (including paths inside opaque saves), but point
    // them at the matching trees registered on this server.
    let mut restored = m.game.clone();
    restored["id"] = Value::String(id.to_owned());
    let old = m.game["mounts"]
        .as_array()
        .context("missing original mounts")?;
    let new = target["mounts"]
        .as_array()
        .context("missing target mounts")?;
    ensure!(old.len() == new.len(), "mount count mismatch");
    let mut mounts = Vec::new();
    for (old, new) in old.iter().zip(new) {
        let (guest, _) = old
            .as_str()
            .context("invalid original mount")?
            .split_once('=')
            .context("invalid original mount")?;
        let (_, tree) = new
            .as_str()
            .context("invalid target mount")?
            .split_once('=')
            .context("invalid target mount")?;
        ensure!(
            guest.starts_with('/') && !guest.contains(':') && !guest.contains(".."),
            "invalid guest mount"
        );
        mounts.push(Value::String(format!("{guest}={tree}")));
    }
    restored["mounts"] = Value::Array(mounts);
    ensure!(m.profile.note.len() <= 100_000, "note too long");
    for s in &m.sessions {
        uuid::Uuid::parse_str(&s.id)?;
        ensure!(s.elapsed_ms >= 0, "invalid duration");
    }
    let name = save_name(&restored)?;
    // Stage remote blobs first; one SQLite transaction publishes all saves and profile data.
    let _guard = saves.lock.lock().await;
    let mut staged = Vec::new();
    for (entry, key) in [
        ("saves.bin", name.clone()),
        ("web.bin", format!("{name}-web")),
    ] {
        {
            let data = entries.get(entry).map(Vec::as_slice).unwrap_or(&[]);
            use rusqlite::OptionalExtension;
            let revision: i64 = saves
                .db
                .connect()?
                .query_row("SELECT revision FROM saves WHERE name=?", [&key], |r| {
                    r.get(0)
                })
                .optional()?
                .unwrap_or(0)
                + 1;
            if let Some(remote) = &saves.remote {
                remote.put(&format!("saves/{key}/{revision}"), data).await?;
            }
            staged.push((key, revision, data));
        }
    }
    let mut c = saves.db.connect()?;
    let tx = c.transaction()?;
    for field in ["args", "env"] {
        if let Some(value) = m.game.get(field) {
            ensure!(
                value
                    .as_array()
                    .is_some_and(|a| a.iter().all(Value::is_string)),
                "invalid game settings"
            );
        }
    }
    tx.execute("INSERT INTO game_settings(game,config) VALUES(?,?) ON CONFLICT(game) DO UPDATE SET config=excluded.config",rusqlite::params![id,serde_json::to_string(&restored)?])?;
    for (key, revision, data) in staged {
        tx.execute("INSERT INTO saves(name,revision,data) VALUES(?,?,?) ON CONFLICT(name) DO UPDATE SET revision=excluded.revision,data=excluded.data",rusqlite::params![key,revision,saves.remote.is_none().then_some(data)])?;
    }
    tx.execute("INSERT INTO profiles(game,favorite,note) VALUES(?,?,?) ON CONFLICT(game) DO UPDATE SET favorite=excluded.favorite,note=excluded.note",rusqlite::params![id,m.profile.favorite,m.profile.note])?;
    for s in m.sessions {
        tx.execute("INSERT INTO sessions(id,game,started_ms,elapsed_ms) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET elapsed_ms=max(sessions.elapsed_ms,excluded.elapsed_ms) WHERE sessions.game=excluded.game AND sessions.started_ms=excluded.started_ms",rusqlite::params![s.id,id,s.started_ms,s.elapsed_ms])?;
    }
    tx.commit()?;
    Ok(())
}

pub fn router(store: std::path::PathBuf, saves: Arc<Saves>) -> axum::Router {
    use axum::{
        body::Bytes,
        extract::{Path as UrlPath, State},
        http::{header, StatusCode},
        response::{IntoResponse, Response},
        routing::get,
    };
    type St = (std::path::PathBuf, Arc<Saves>);
    async fn download(State((store, saves)): State<St>, UrlPath(id): UrlPath<String>) -> Response {
        match export(&store, &id, &saves).await {
            Ok(bytes) => (
                [
                    (
                        header::CONTENT_TYPE,
                        "application/vnd.ippai.package".to_string(),
                    ),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{id}.ippai\""),
                    ),
                ],
                bytes,
            )
                .into_response(),
            Err(e) => {
                eprintln!("package export: {e}");
                StatusCode::BAD_REQUEST.into_response()
            }
        }
    }
    async fn upload(
        State((store, saves)): State<St>,
        UrlPath(id): UrlPath<String>,
        body: Bytes,
    ) -> Response {
        match import(&store, &id, &body, &saves).await {
            Ok(()) => StatusCode::NO_CONTENT.into_response(),
            Err(e) => {
                eprintln!("package import: {e}");
                (StatusCode::BAD_REQUEST, e.to_string()).into_response()
            }
        }
    }
    axum::Router::new()
        .route("/api/packages/{id}", get(download).put(upload))
        .layer(axum::extract::DefaultBodyLimit::max(512 << 20))
        .with_state((store, saves))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn package_restores_saves_profile_and_rejects_wrong_game() {
        let dir = std::env::temp_dir().join(format!("ippai-package-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("games")).unwrap();
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        let g =
            serde_json::json!({"id":"sample","run":"Z:\\game\\game.exe","mounts":["/game=sample"]});
        std::fs::write(
            dir.join("games/sample.json"),
            serde_json::to_vec(&g).unwrap(),
        )
        .unwrap();
        let tree = dir.join("trees/sample.tsv");
        std::fs::write(&tree, "vnvm-tree\t1\t262144\nf\t0\t0\tgame.exe\t1\tabcd\n").unwrap();
        let saves = Saves::new(&dir).unwrap();
        let name = save_name(&g).unwrap();
        saves.write(&name, 0, b"progress").await.unwrap();
        saves
            .db
            .set_profile(
                "sample",
                &Profile {
                    favorite: true,
                    note: "note".into(),
                },
            )
            .unwrap();
        let bytes = export(&dir, "sample", &saves).await.unwrap();
        saves.write(&name, 1, b"other").await.unwrap();
        import(&dir, "sample", &bytes, &saves).await.unwrap();
        assert_eq!(saves.read(&name).await.unwrap().unwrap().1, b"progress");
        assert!(saves.db.profile("sample").unwrap().favorite);
        assert!(!saves.write(&name, 1, b"stale").await.unwrap());
        std::fs::copy(&tree, dir.join("trees/renamed.tsv")).unwrap();
        let mut renamed = g.clone();
        renamed["id"] = Value::String("renamed".into());
        renamed["mounts"] = serde_json::json!(["/different=renamed"]);
        renamed["run"] = Value::String("Z:\\different\\game.exe".into());
        std::fs::write(
            dir.join("games/renamed.json"),
            serde_json::to_vec(&renamed).unwrap(),
        )
        .unwrap();
        import(&dir, "renamed", &bytes, &saves).await.unwrap();
        saves.db.apply_settings(&mut renamed).unwrap();
        assert_eq!(renamed["run"], g["run"]);
        assert_eq!(renamed["mounts"], serde_json::json!(["/game=renamed"]));
        assert_eq!(
            saves
                .read(&save_name(&renamed).unwrap())
                .await
                .unwrap()
                .unwrap()
                .1,
            b"progress"
        );
        std::fs::write(
            &tree,
            "vnvm-tree\t1\t262144\nf\t0\t0\tgame.exe\t1\tdifferent\n",
        )
        .unwrap();
        assert!(import(&dir, "sample", &bytes, &saves).await.is_err());
        assert!(import(&dir, "sample", b"broken", &saves).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
