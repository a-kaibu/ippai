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
            inflight: Default::default(),
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
        assert_eq!(&*s.get("chunks/test").await.unwrap().unwrap(), b"game bytes");
        let restored = dir.join("restored.db");
        s.restore(snapshot, &restored).await.unwrap();
        s.db = Library(restored);
        *s.cache.lock().unwrap() = Default::default();
        assert!(s.db.profile("game").unwrap().favorite);
        assert_eq!(&*s.get("chunks/test").await.unwrap().unwrap(), b"game bytes");
        server.abort();
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
pub(crate) mod gc_tests {
    use super::*;
    use axum::{body::Bytes, extract::{Path as UrlPath, State}, http::StatusCode, response::IntoResponse, routing::get, Router};
    use std::sync::{atomic::{AtomicUsize, Ordering::SeqCst}, Arc, Mutex};

    /// An S3 bucket in memory answering after `delay`; counts requests and the most PUTs or
    /// GETs on their way at once.
    #[derive(Clone, Default)]
    pub(crate) struct Fake {
        objects: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
        delay_ms: u64,
        now: Arc<AtomicUsize>,
        most: Arc<AtomicUsize>,
        gets: Arc<AtomicUsize>,
    }

    pub(crate) async fn serve(delay_ms: u64) -> (Fake, Storage, std::path::PathBuf) {
        async fn busy(f: &Fake) {
            let n = f.now.fetch_add(1, SeqCst) + 1;
            f.most.fetch_max(n, SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(f.delay_ms)).await;
            f.now.fetch_sub(1, SeqCst);
        }
        async fn put(State(f): State<Fake>, UrlPath(p): UrlPath<String>, b: Bytes) -> StatusCode {
            busy(&f).await;
            f.objects.lock().unwrap().insert(p, b.to_vec());
            StatusCode::OK
        }
        async fn read(State(f): State<Fake>, UrlPath(p): UrlPath<String>) -> axum::response::Response {
            f.gets.fetch_add(1, SeqCst);
            busy(&f).await;
            f.objects.lock().unwrap().get(&p).cloned().map(IntoResponse::into_response).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
        }
        async fn delete(State(f): State<Fake>, UrlPath(p): UrlPath<String>) -> StatusCode {
            f.objects.lock().unwrap().remove(&p);
            StatusCode::NO_CONTENT
        }
        let f = Fake { delay_ms, ..Default::default() };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().route("/{*path}", get(read).put(put).delete(delete)).with_state(f.clone());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let dir = std::env::temp_dir().join(format!("ippai-gc-{}", Uuid::new_v4()));
        let config = Config {
            bucket: "test".into(),
            region: "us-east-1".into(),
            endpoint: Some(endpoint.clone()),
            prefix: "".into(),
            active_key: "k".into(),
            keys: BTreeMap::from([("k".into(), "03".repeat(32))]),
        };
        let bucket = Bucket::new("test", Region::Custom { region: "us-east-1".into(), endpoint }, Credentials::new(Some("t"), Some("t"), None, None, None).unwrap()).unwrap().with_path_style();
        let s = Storage { db: Library::new(&dir).unwrap(), bucket, config, cache: Default::default(), inflight: Default::default(), requests: tokio::sync::Semaphore::new(8) };
        (f, s, dir)
    }

    fn names(f: &Fake) -> std::collections::BTreeSet<String> {
        f.objects.lock().unwrap().keys().map(|k| k.trim_start_matches("test/").to_string()).collect()
    }

    fn named(s: &Storage) -> std::collections::BTreeSet<String> {
        let c = s.db.connect().unwrap();
        let mut q = c.prepare("SELECT envelope FROM objects").unwrap();
        let v: Vec<String> = q.query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        v.iter().map(|e| serde_json::from_str::<Envelope>(e).unwrap().object.to_string()).collect()
    }

    /// Moves everything recorded `ms` into the past.
    fn age_retired(s: &Storage, ms: i64) {
        let c = s.db.connect().unwrap();
        c.execute("UPDATE retired SET retired_ms=retired_ms-?", [ms]).unwrap();
        c.execute("UPDATE backups SET created_ms=created_ms-?", [ms]).unwrap();
    }

    /// Random puts over a few names, saves, forgets and backups, then gc: what is named
    /// stays and reads back; what is gone is only what nothing (no row, no kept backup's
    /// database) names; a backup restores whole after gc.
    #[tokio::test]
    async fn gc_deletes_only_what_nothing_names() {
        let (f, s, dir) = serve(0).await;
        let mut seed = 0x6a09e667f3bcc909u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut want: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        let mut backups = Vec::new();
        for step in 0..120 {
            match rnd(10) {
                0 => {
                    let l = format!("trees/t{}", rnd(4));
                    s.forget(&l).unwrap();
                    want.remove(&l);
                }
                1 if step > 60 && backups.len() < 2 => {
                    backups.push((s.backup().await.unwrap(), want.clone()));
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
                _ => {
                    let l = format!("trees/t{}", rnd(4));
                    let v = format!("{step}").into_bytes();
                    s.put(&l, &v).await.unwrap();
                    want.insert(l, v);
                }
            }
        }
        // nothing is old enough yet
        assert_eq!(s.gc(std::time::Duration::from_secs(3600), None).await.unwrap().deleted, 0);
        age_retired(&s, 7_200_000);
        let before = names(&f);
        let r = s.gc(std::time::Duration::from_secs(3600), None).await.unwrap();
        let after = names(&f);
        assert!(r.deleted > 0, "{r:?}");
        assert_eq!(before.len() - after.len(), r.deleted);
        assert!(named(&s).is_subset(&after));
        *s.cache.lock().unwrap() = Default::default();
        for (l, v) in &want {
            assert_eq!(&*s.get(l).await.unwrap().unwrap(), &v[..]);
        }
        // each backup restores whole: its rows' objects are all still there
        for (i, (b, then)) in backups.iter().enumerate() {
            let out = dir.join(format!("restored{i}.db"));
            s.restore(*b, &out).await.unwrap();
            let r = Storage { db: Library(out), bucket: s.bucket.clone(), config: Config { keys: s.config.keys.clone(), active_key: "k".into(), bucket: "test".into(), region: "us-east-1".into(), endpoint: None, prefix: "".into() }, cache: Default::default(), inflight: Default::default(), requests: tokio::sync::Semaphore::new(8) };
            for (l, v) in then {
                assert_eq!(&*r.get(l).await.unwrap().unwrap(), &v[..], "backup {i} {l}");
            }
        }
        // with the backups dropped, what only they named goes too; what is named stays
        let r = s.gc(std::time::Duration::from_secs(3600), Some(0)).await.unwrap();
        assert_eq!(r.backups_dropped, backups.len());
        assert_eq!(names(&f), named(&s));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Saves keep their last SAVE_REVISIONS revisions in S3.
    #[tokio::test]
    async fn old_save_revisions_are_retired() {
        let (f, s, dir) = serve(0).await;
        let db = s.db.clone();
        db.connect().unwrap().execute_batch("CREATE TABLE IF NOT EXISTS saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB);").unwrap();
        let saves = crate::saves::Saves { db, remote: Some(s), lock: tokio::sync::Mutex::new(()) };
        for rev in 0..20 {
            assert!(saves.write("game", rev, format!("save {rev}").as_bytes()).await.unwrap());
        }
        assert_eq!(saves.read("game").await.unwrap().unwrap(), (20, b"save 19".to_vec()));
        let s = saves.remote.as_ref().unwrap();
        let rows: i64 = s.db.connect().unwrap().query_row("SELECT COUNT(*) FROM objects WHERE logical LIKE 'saves/game/%'", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, SAVE_REVISIONS);
        age_retired(s, 7_200_000);
        s.gc(std::time::Duration::from_secs(3600), None).await.unwrap();
        assert_eq!(names(&f).len() as i64, SAVE_REVISIONS);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// push_store uploads several at once (A/B: 64 chunks against a bucket answering in 40 ms
    /// each); chunks before trees before games; one pushed is not pushed again.
    #[tokio::test]
    async fn push_store_in_parallel() {
        let (f, s, dir) = serve(40).await;
        let store = dir.join("store");
        for i in 0..64 {
            let d = store.join(format!("chunks/{:02x}", i % 4));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("c{i}")), [i as u8; 100]).unwrap();
        }
        std::fs::create_dir_all(store.join("trees")).unwrap();
        std::fs::write(store.join("trees/t.tsv"), "tree").unwrap();
        std::fs::create_dir_all(store.join("games")).unwrap();
        std::fs::write(store.join("games/g.json"), "game").unwrap();
        let t = std::time::Instant::now();
        s.push_store(&store).await.unwrap();
        let took = t.elapsed();
        eprintln!("push_store: 66 files at 40 ms each in {took:?} (one at a time: {} ms)", 66 * 40);
        assert!(f.most.load(SeqCst) >= 4, "{}", f.most.load(SeqCst));
        assert!(took < std::time::Duration::from_millis(66 * 40 / 2));
        assert_eq!(names(&f).len(), 66);
        let order: Vec<i64> = ["chunks/00/c0", "trees/t.tsv", "games/g.json"]
            .iter()
            .map(|l| s.db.connect().unwrap().query_row("SELECT rowid FROM objects WHERE logical=?", [l], |r| r.get(0)).unwrap())
            .collect();
        assert!(order[0] < order[1] && order[1] < order[2], "{order:?}");
        let max_chunk: i64 = s.db.connect().unwrap().query_row("SELECT MAX(rowid) FROM objects WHERE logical LIKE 'chunks/%'", [], |r| r.get(0)).unwrap();
        assert!(max_chunk < order[1]);
        s.push_store(&store).await.unwrap();
        assert_eq!(names(&f).len(), 66 + 2, "only trees and games go again");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// The same object asked for many times at once: one download; the cache holds what it
    /// says it holds.
    #[tokio::test]
    async fn one_download_per_object() {
        let (f, s, dir) = serve(30).await;
        s.put("chunks/x", b"payload").await.unwrap();
        let s = Arc::new(s);
        let tasks: Vec<_> = (0..16).map(|_| { let s = s.clone(); tokio::spawn(async move { s.get("chunks/x").await.unwrap().unwrap() }) }).collect();
        for t in tasks {
            assert_eq!(&*t.await.unwrap(), b"payload");
        }
        assert_eq!(f.gets.load(SeqCst), 1);
        assert!(s.inflight.lock().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn cache_counts_and_bounds() {
        let mut c = Cache::default();
        let mut seed = 0xbb67ae8584caa73bu64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let ids: Vec<Uuid> = (0..30).map(|_| Uuid::new_v4()).collect();
        for _ in 0..5000 {
            let id = ids[rnd(30) as usize];
            c.put(id, vec![0u8; rnd(2000) as usize].into(), 10_000);
            assert_eq!(c.bytes, c.map.values().map(|d| d.len()).sum::<usize>());
            assert!(c.bytes <= 10_000);
            assert_eq!(c.order.len(), c.map.len());
        }
        c.put(Uuid::new_v4(), vec![0u8; CACHE_ITEM + 1].into(), usize::MAX);
        assert!(c.bytes <= 10_000);
    }
}

pub struct Storage {
    pub db: Library,
    bucket: Box<Bucket>,
    config: Config,
    cache: std::sync::Mutex<Cache>,
    /// one download per object at a time (the others wait for it and take it from the cache)
    inflight: std::sync::Mutex<std::collections::HashMap<Uuid, std::sync::Arc<tokio::sync::Mutex<()>>>>,
    requests: tokio::sync::Semaphore,
}

/// Decrypted objects of at most 1 MiB, 64 MiB of them, the oldest put dropped first.
#[derive(Default)]
struct Cache {
    map: std::collections::HashMap<Uuid, std::sync::Arc<[u8]>>,
    order: std::collections::VecDeque<Uuid>,
    bytes: usize,
}

const CACHE_ITEM: usize = 1 << 20;
const CACHE_BYTES: usize = 64 << 20;

impl Cache {
    fn get(&self, id: &Uuid) -> Option<std::sync::Arc<[u8]>> {
        self.map.get(id).cloned()
    }
    fn put(&mut self, id: Uuid, data: std::sync::Arc<[u8]>, limit: usize) {
        if data.len() > CACHE_ITEM || self.map.contains_key(&id) {
            return;
        }
        self.bytes += data.len();
        self.map.insert(id, data);
        self.order.push_back(id);
        while self.bytes > limit {
            let Some(old) = self.order.pop_front() else { break };
            if let Some(d) = self.map.remove(&old) {
                self.bytes -= d.len();
            }
        }
    }
}

/// Uploads at once (push_store).
const PUTS: usize = 8;
/// Revisions of a save kept in S3 (older ones are retired, see [`Storage::gc`]).
pub const SAVE_REVISIONS: i64 = 8;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// What [`Storage::gc`] did.
#[derive(Debug, Default, PartialEq)]
pub struct Gc {
    pub deleted: usize,
    /// retired but kept: within the grace period, or a backup still refers to them
    pub kept: usize,
    pub backups_dropped: usize,
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
            inflight: Default::default(),
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
        let mut c = self.db.connect()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old: Option<String> = tx.query_row("SELECT envelope FROM objects WHERE logical=?", [logical], |r| r.get(0)).optional()?;
        tx.execute("INSERT INTO objects(logical,envelope,size) VALUES(?,?,?) ON CONFLICT(logical) DO UPDATE SET envelope=excluded.envelope,size=excluded.size",
            rusqlite::params![logical, serde_json::to_string(&envelope)?, bytes.len() as i64])?;
        if let Some(old) = old {
            retire(&tx, serde_json::from_str::<Envelope>(&old)?.object)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Drops `logical`; its object is retired.
    pub fn forget(&self, logical: &str) -> Result<()> {
        let mut c = self.db.connect()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let old: Option<String> = tx.query_row("SELECT envelope FROM objects WHERE logical=?", [logical], |r| r.get(0)).optional()?;
        if let Some(old) = old {
            tx.execute("DELETE FROM objects WHERE logical=?", [logical])?;
            retire(&tx, serde_json::from_str::<Envelope>(&old)?.object)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Forgets save `name`'s revisions older than the last [`SAVE_REVISIONS`] up to `current`.
    pub fn prune_save(&self, name: &str, current: i64) -> Result<()> {
        let prefix = format!("saves/{name}/");
        let old: Vec<String> = {
            let c = self.db.connect()?;
            let mut q = c.prepare("SELECT logical FROM objects WHERE substr(logical,1,?)=?")?;
            let rows = q.query_map(rusqlite::params![prefix.len() as i64, prefix], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|l| l[prefix.len()..].parse::<i64>().is_ok_and(|r| r <= current - SAVE_REVISIONS))
                .collect()
        };
        for l in old {
            self.forget(&l)?;
        }
        Ok(())
    }

    /// Deletes retired objects (replaced or forgotten) that no backup refers to, once retired for
    /// `grace`: a backup taken before an object was retired has it in its database, so objects
    /// go only when retired before the oldest backup kept. `keep_backups`: the newest that many
    /// backups are kept, the others deleted first.
    pub async fn gc(&self, grace: std::time::Duration, keep_backups: Option<usize>) -> Result<Gc> {
        let mut out = Gc::default();
        if let Some(n) = keep_backups {
            let old: Vec<(String, String)> = {
                let c = self.db.connect()?;
                let mut q = c.prepare("SELECT id,snapshot FROM backups ORDER BY created_ms DESC LIMIT -1 OFFSET ?")?;
                let rows = q.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for (id, snapshot) in old {
                self.delete(Uuid::parse_str(&id)?).await?;
                self.delete(Uuid::parse_str(&snapshot)?).await?;
                self.db.connect()?.execute("DELETE FROM backups WHERE id=?", [&id])?;
                out.backups_dropped += 1;
            }
        }
        let (oldest, retired): (Option<i64>, Vec<(String, i64)>) = {
            let c = self.db.connect()?;
            let oldest = c.query_row("SELECT MIN(created_ms) FROM backups", [], |r| r.get(0))?;
            let mut q = c.prepare("SELECT object,retired_ms FROM retired")?;
            let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            (oldest, rows.collect::<rusqlite::Result<_>>()?)
        };
        let cutoff = (now_ms() - grace.as_millis() as i64).min(oldest.unwrap_or(i64::MAX));
        for (object, at) in retired {
            // (an object still named by a row: never; new objects get new UUIDs)
            let named: bool = self.db.connect()?.query_row("SELECT EXISTS(SELECT 1 FROM objects WHERE instr(envelope,?) > 0)", [&object], |r| r.get(0))?;
            if at >= cutoff || named {
                out.kept += 1;
                continue;
            }
            self.delete(Uuid::parse_str(&object)?).await?;
            self.db.connect()?.execute("DELETE FROM retired WHERE object=?", [&object])?;
            out.deleted += 1;
        }
        Ok(out)
    }

    async fn delete(&self, id: Uuid) -> Result<()> {
        let r = self.bucket.delete_object(self.path(id)).await?;
        ensure!(r.status_code() / 100 == 2 || r.status_code() == 404, "S3 delete failed: {}", r.status_code());
        Ok(())
    }

    pub async fn get(&self, logical: &str) -> Result<Option<std::sync::Arc<[u8]>>> {
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
        if let Some(data) = self.cache.lock().unwrap().get(&e.object) {
            return Ok(Some(data));
        }
        // the same object asked for at once (a file's chunks fetched ahead, two clients):
        // one download
        let gate = self.inflight.lock().unwrap().entry(e.object).or_default().clone();
        let data = async {
            let _one = gate.lock().await;
            if let Some(data) = self.cache.lock().unwrap().get(&e.object) {
                return Ok(data);
            }
            let _permit = self.requests.acquire().await?;
            let ciphertext = self.download(e.object).await?;
            let data = std::sync::Arc::<[u8]>::from(e.decrypt(&self.key(&e.key_id)?, &ciphertext)?);
            self.cache.lock().unwrap().put(e.object, data.clone(), CACHE_BYTES);
            Ok::<_, anyhow::Error>(data)
        }
        .await;
        {
            let mut f = self.inflight.lock().unwrap();
            // (the last one out: the map's and ours are all that are left)
            if f.get(&e.object).is_some_and(|g| std::sync::Arc::ptr_eq(g, &gate)) && std::sync::Arc::strong_count(&gate) == 2 {
                f.remove(&e.object);
            }
        }
        Ok(Some(data?))
    }

    /// Uploads the store's files not uploaded yet, [`PUTS`] at once: chunks first, then trees,
    /// then games (a game's description goes up once what it names is there).
    pub async fn push_store(&self, store: &Path) -> Result<()> {
        use futures_util::TryStreamExt;
        for top in ["chunks", "trees", "games"] {
            let mut files = Vec::new();
            let mut dirs = vec![store.join(top)];
            while let Some(dir) = dirs.pop() {
                if !dir.exists() {
                    continue;
                }
                for entry in std::fs::read_dir(dir)? {
                    let entry = entry?;
                    let kind = entry.file_type()?;
                    if kind.is_dir() {
                        dirs.push(entry.path());
                    } else if kind.is_file() {
                        let path = entry.path();
                        let logical = path.strip_prefix(store)?.to_str().context("non-UTF8 store path")?.to_string();
                        files.push((path, logical));
                    }
                }
            }
            files.sort();
            futures_util::stream::iter(files.into_iter().map(Ok::<_, anyhow::Error>))
                .map_ok(|(path, logical)| async move {
                    // (chunks are named by their content: one uploaded is never again)
                    if logical.starts_with("chunks/") {
                        let exists: bool = self.db.connect()?.query_row(
                            "SELECT EXISTS(SELECT 1 FROM objects WHERE logical=?)",
                            [&logical],
                            |r| r.get(0),
                        )?;
                        if exists {
                            return Ok(());
                        }
                    }
                    let data = tokio::fs::read(&path).await?;
                    self.put(&logical, &data).await
                })
                .try_buffer_unordered(PUTS)
                .try_collect::<Vec<()>>()
                .await?;
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
        self.db.connect()?.execute(
            "INSERT INTO backups(id,snapshot,created_ms) VALUES(?,?,?)",
            rusqlite::params![id.to_string(), backup.envelope.object.to_string(), backup.created_ms as i64],
        )?;
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

/// Notes that `object` is no longer named (gc deletes it later).
fn retire(tx: &rusqlite::Transaction, object: Uuid) -> Result<()> {
    tx.execute("INSERT OR IGNORE INTO retired(object,retired_ms) VALUES(?,?)", rusqlite::params![object.to_string(), now_ms()])?;
    Ok(())
}
