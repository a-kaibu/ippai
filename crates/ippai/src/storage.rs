//! Encrypted immutable S3 objects. Names and key envelopes live in SQLite.
use crate::{envelope::Envelope, library::Library};
use anyhow::{anyhow, ensure, Context, Result};
use rusqlite::OptionalExtension;
use s3::{creds::Credentials, Bucket, Region};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
use uuid::Uuid;

#[cfg(test)]
mod rustfs_tests;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    bucket: String,
    region: String,
    endpoint: Option<String>,
    #[serde(default)]
    prefix: String,
    active_key: String,
    /// Hex encoded 256-bit keys. This file must be backed up separately from S3.
    keys: BTreeMap<String, String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Bytes,
        extract::{Path as UrlPath, State},
        http::StatusCode,
        response::IntoResponse,
        routing::get,
        Router,
    };
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn s3_roundtrip_rotation_and_disaster_restore() {
        type Objects = Arc<Mutex<BTreeMap<String, Vec<u8>>>>;
        async fn put(
            State(s): State<Objects>,
            UrlPath(p): UrlPath<String>,
            b: Bytes,
        ) -> StatusCode {
            s.lock().unwrap().insert(p, b.to_vec());
            StatusCode::OK
        }
        async fn read(
            State(s): State<Objects>,
            UrlPath(p): UrlPath<String>,
        ) -> axum::response::Response {
            s.lock()
                .unwrap()
                .get(&p)
                .cloned()
                .map(IntoResponse::into_response)
                .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
        }
        let objects: Objects = Default::default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new()
            .route("/{*path}", get(read).put(put))
            .with_state(objects.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = std::env::temp_dir().join(format!("ippai-s3-{}", Uuid::new_v4()));
        let db = Library::new(&dir).unwrap();
        db.set_profile(
            "game",
            &crate::library::Profile {
                favorite: true,
                note: "restore me".into(),
            },
        )
        .unwrap();
        let config = Config {
            bucket: "test".into(),
            region: "us-east-1".into(),
            endpoint: Some(endpoint.clone()),
            prefix: "".into(),
            active_key: "old".into(),
            keys: BTreeMap::from([
                ("old".into(), "01".repeat(32)),
                ("new".into(), "02".repeat(32)),
            ]),
        };
        let bucket = Bucket::new(
            "test",
            Region::Custom {
                region: "us-east-1".into(),
                endpoint,
            },
            Credentials::new(Some("test"), Some("test"), None, None, None).unwrap(),
        )
        .unwrap()
        .with_path_style();
        let mut s = Storage {
            db,
            bucket,
            config,
            cache: Default::default(),
            requests: tokio::sync::Semaphore::new(8),
        };
        s.put("chunks/test", b"game bytes").await.unwrap();
        let snapshot = s.backup().await.unwrap();
        let before = objects.lock().unwrap().clone();
        s.config.active_key = "new".into();
        s.rotate_objects().unwrap();
        s.rotate_backup(snapshot).await.unwrap();
        // Only the small backup envelope changed; encrypted object payloads are immutable.
        for (name, bytes) in before {
            if !name.ends_with(&snapshot.to_string()) {
                assert_eq!(objects.lock().unwrap()[&name], bytes);
            }
        }
        s.config.keys.remove("old");
        assert_eq!(s.get("chunks/test").await.unwrap().unwrap(), b"game bytes");
        let restored = dir.join("restored.db");
        s.restore(snapshot, &restored).await.unwrap();
        s.db = Library(restored);
        s.cache.lock().unwrap().clear();
        assert!(s.db.profile("game").unwrap().favorite);
        assert_eq!(s.get("chunks/test").await.unwrap().unwrap(), b"game bytes");
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

pub struct Storage {
    pub db: Library,
    bucket: Box<Bucket>,
    config: Config,
    cache: std::sync::Mutex<std::collections::VecDeque<(Uuid, Vec<u8>)>>,
    requests: tokio::sync::Semaphore,
}

#[derive(Serialize, Deserialize)]
struct Backup {
    envelope: Envelope,
    created_ms: u128,
    recovery: Envelope,
    recovery_ciphertext: Vec<u8>,
}

impl Storage {
    /// Restore the small catalogs locally. Chunk payloads remain remote and stream on demand.
    pub async fn hydrate_catalog(&self, store: &Path) -> Result<()> {
        let paths: Vec<String> = {
            let c = self.db.connect()?;
            let mut q = c.prepare("SELECT logical FROM objects WHERE logical LIKE 'trees/%' OR logical LIKE 'games/%'")?;
            let rows = q.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for logical in paths {
            ensure!(
                Path::new(&logical)
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_))),
                "invalid catalog path"
            );
            let path = store.join(&logical);
            if path.exists() {
                continue;
            }
            let data = self
                .get(&logical)
                .await?
                .context("catalog object missing")?;
            tokio::fs::create_dir_all(path.parent().unwrap()).await?;
            let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
            tokio::fs::write(&temp, data).await?;
            tokio::fs::rename(temp, path).await?;
        }
        Ok(())
    }
    pub fn configured(db: Library) -> Result<Option<Self>> {
        let Some(path) = std::env::var_os("IPPAI_STORAGE_CONFIG") else {
            return Ok(None);
        };
        Self::open(db, Path::new(&path)).map(Some)
    }

    pub fn open(db: Library, path: &Path) -> Result<Self> {
        let config: Config = toml::from_str(&std::fs::read_to_string(path)?)?;
        ensure!(!config.active_key.is_empty(), "active key id is empty");
        let region = match &config.endpoint {
            Some(endpoint) => Region::Custom {
                region: config.region.clone(),
                endpoint: endpoint.clone(),
            },
            None => config.region.parse()?,
        };
        let mut bucket = Bucket::new(&config.bucket, region, Credentials::default()?)?;
        if config.endpoint.is_some() {
            bucket = bucket.with_path_style();
        }
        let s = Self {
            db,
            bucket,
            config,
            cache: Default::default(),
            requests: tokio::sync::Semaphore::new(8),
        };
        s.key(&s.config.active_key)?;
        Ok(s)
    }

    fn key(&self, id: &str) -> Result<[u8; 32]> {
        let hex = self
            .config
            .keys
            .get(id)
            .context("key id missing from storage configuration")?;
        ensure!(
            hex.len() == 64 && hex.is_ascii(),
            "key must be 64 hexadecimal characters"
        );
        let mut out = [0; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).context("invalid key encoding")?;
        }
        Ok(out)
    }

    fn path(&self, id: Uuid) -> String {
        format!("{}{}", self.config.prefix, id)
    }

    async fn upload(&self, id: Uuid, bytes: &[u8]) -> Result<()> {
        let r = self.bucket.put_object(self.path(id), bytes).await?;
        ensure!(
            r.status_code() / 100 == 2,
            "S3 upload failed: {}",
            r.status_code()
        );
        Ok(())
    }

    async fn download(&self, id: Uuid) -> Result<Vec<u8>> {
        let r = self.bucket.get_object(self.path(id)).await?;
        ensure!(
            r.status_code() == 200,
            "S3 download failed: {}",
            r.status_code()
        );
        Ok(r.bytes().to_vec())
    }

    pub async fn put(&self, logical: &str, bytes: &[u8]) -> Result<()> {
        let (envelope, ciphertext) = Envelope::encrypt(
            &self.key(&self.config.active_key)?,
            &self.config.active_key,
            bytes,
        )?;
        self.upload(envelope.object, &ciphertext).await?;
        self.db.connect()?.execute("INSERT INTO objects(logical,envelope,size) VALUES(?,?,?) ON CONFLICT(logical) DO UPDATE SET envelope=excluded.envelope,size=excluded.size",
            rusqlite::params![logical, serde_json::to_string(&envelope)?, bytes.len() as i64])?;
        Ok(())
    }

    pub async fn get(&self, logical: &str) -> Result<Option<Vec<u8>>> {
        let entry: Option<String> = self
            .db
            .connect()?
            .query_row(
                "SELECT envelope FROM objects WHERE logical=?",
                [logical],
                |r| r.get(0),
            )
            .optional()?;
        let Some(entry) = entry else { return Ok(None) };
        let e: Envelope = serde_json::from_str(&entry)?;
        // Cache compressed plaintext chunks, bounded to 64 MiB; cap remote concurrency.
        if let Some((_, data)) = self
            .cache
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == e.object)
        {
            return Ok(Some(data.clone()));
        }
        let _permit = self.requests.acquire().await?;
        let ciphertext = self.download(e.object).await?;
        let data = e.decrypt(&self.key(&e.key_id)?, &ciphertext)?;
        if data.len() <= 1 << 20 {
            let mut cache = self.cache.lock().unwrap();
            if !cache.iter().any(|(id, _)| *id == e.object) {
                cache.push_back((e.object, data.clone()));
            }
            while cache.iter().map(|(_, b)| b.len()).sum::<usize>() > 64 << 20 {
                cache.pop_front();
            }
        }
        Ok(Some(data))
    }

    /// Upload immutable store files in bounded units. Existing content hashes are reusable.
    pub async fn push_store(&self, store: &Path) -> Result<()> {
        let mut dirs = vec![
            store.join("chunks"),
            store.join("trees"),
            store.join("games"),
        ];
        while let Some(dir) = dirs.pop() {
            if !dir.exists() {
                continue;
            }
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                if entry.file_type()?.is_dir() {
                    dirs.push(entry.path());
                    continue;
                }
                if !entry.file_type()?.is_file() {
                    continue;
                }
                let path = entry.path();
                let logical = path
                    .strip_prefix(store)?
                    .to_str()
                    .context("non-UTF8 store path")?;
                if logical.starts_with("chunks/") {
                    let exists: bool = self.db.connect()?.query_row(
                        "SELECT EXISTS(SELECT 1 FROM objects WHERE logical=?)",
                        [logical],
                        |r| r.get(0),
                    )?;
                    if exists {
                        continue;
                    }
                }
                let data = tokio::fs::read(&path).await?;
                self.put(logical, &data).await?;
            }
        }
        Ok(())
    }

    pub async fn backup(&self) -> Result<Uuid> {
        let temp = self
            .db
            .0
            .with_file_name(format!(".{}.snapshot", Uuid::new_v4()));
        let result = (|| -> Result<Vec<u8>> {
            self.db.snapshot(&temp)?;
            Ok(zstd::encode_all(std::fs::File::open(&temp)?, 3)?)
        })();
        let _ = std::fs::remove_file(&temp);
        let data = result?;
        let (envelope, ciphertext) = Envelope::encrypt(
            &self.key(&self.config.active_key)?,
            &self.config.active_key,
            &data,
        )?;
        self.upload(envelope.object, &ciphertext).await?;
        let id = Uuid::new_v4();
        // A snapshot contains envelopes from that date. Keep its required wrapping keys
        // encrypted alongside it so rotation can retire old keys without rewriting the DB.
        let (recovery, recovery_ciphertext) = Envelope::encrypt(
            &self.key(&self.config.active_key)?,
            &self.config.active_key,
            &serde_json::to_vec(&self.config.keys)?,
        )?;
        let backup = Backup {
            envelope,
            recovery,
            recovery_ciphertext,
            created_ms: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
        };
        self.upload(id, &serde_json::to_vec(&backup)?).await?;
        Ok(id)
    }

    /// Restores into a NEW database path, never over a running database.
    pub async fn restore(&self, id: Uuid, output: &Path) -> Result<()> {
        use std::io::{Read, Write};
        ensure!(!output.exists(), "restore destination already exists");
        let b: Backup = serde_json::from_slice(&self.download(id).await?)?;
        let recovered: BTreeMap<String, String> = serde_json::from_slice(
            &b.recovery
                .decrypt(&self.key(&b.recovery.key_id)?, &b.recovery_ciphertext)?,
        )?;
        let compressed = b.envelope.decrypt(
            &self.key(&b.envelope.key_id)?,
            &self.download(b.envelope.object).await?,
        )?;
        let temp = output.with_extension(format!("{}.tmp", Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            let mut decoder = zstd::Decoder::new(compressed.as_slice())?.take((4u64 << 30) + 1);
            let bytes = std::io::copy(&mut decoder, &mut f)?;
            ensure!(bytes <= 4u64 << 30, "backup exceeds restore limit");
            f.flush()?;
            f.sync_all()?;
            let mut c = rusqlite::Connection::open(&temp)?;
            let check: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
            ensure!(check == "ok", "invalid database snapshot");
            let tx = c.transaction()?;
            let rows: Vec<(String, String)> = {
                let mut q = tx.prepare("SELECT logical,envelope FROM objects")?;
                let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for (logical, json) in rows {
                let e: Envelope = serde_json::from_str(&json)?;
                let hex = recovered
                    .get(&e.key_id)
                    .context("backup recovery key missing")?;
                ensure!(hex.len() == 64 && hex.is_ascii(), "invalid recovery key");
                let mut old = [0; 32];
                for (i, b) in old.iter_mut().enumerate() {
                    *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)?;
                }
                let next = e.rewrap(
                    &old,
                    &self.key(&self.config.active_key)?,
                    &self.config.active_key,
                )?;
                tx.execute(
                    "UPDATE objects SET envelope=? WHERE logical=?",
                    rusqlite::params![serde_json::to_string(&next)?, logical],
                )?;
            }
            tx.commit()?;
            drop(c);
            // hard_link fails rather than replacing a concurrently created destination.
            std::fs::hard_link(&temp, output)?;
            Ok(())
        })();
        let _ = std::fs::remove_file(&temp);
        result
    }

    pub fn rotate_objects(&self) -> Result<()> {
        let mut c = self.db.connect()?;
        let tx = c.transaction()?;
        let rows: Vec<(String, String)> = {
            let mut q = tx.prepare("SELECT logical,envelope FROM objects")?;
            let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let new = self.key(&self.config.active_key)?;
        for (logical, json) in rows {
            let e: Envelope = serde_json::from_str(&json)?;
            let next = e.rewrap(&self.key(&e.key_id)?, &new, &self.config.active_key)?;
            tx.execute(
                "UPDATE objects SET envelope=? WHERE logical=?",
                rusqlite::params![serde_json::to_string(&next)?, logical],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub async fn rotate_backup(&self, id: Uuid) -> Result<()> {
        let mut b: Backup = serde_json::from_slice(&self.download(id).await?)
            .map_err(|_| anyhow!("not a backup envelope"))?;
        b.envelope = b.envelope.rewrap(
            &self.key(&b.envelope.key_id)?,
            &self.key(&self.config.active_key)?,
            &self.config.active_key,
        )?;
        b.recovery = b.recovery.rewrap(
            &self.key(&b.recovery.key_id)?,
            &self.key(&self.config.active_key)?,
            &self.config.active_key,
        )?;
        self.upload(id, &serde_json::to_vec(&b)?).await
    }
}
