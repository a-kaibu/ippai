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
    /// Uploads at once in `push` [default: PUTS]. Each object is a 256 KiB chunk: with a far
    /// gateway a round trip, not the line, bounds one upload, so more at once go faster.
    #[serde(default)]
    uploads: Option<usize>,
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
            uploads: None,
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
        /// requests of any kind, answered or refused
        requests: Arc<AtomicUsize>,
        /// refuse the next this many requests with `fail_status`
        fail_next: Arc<AtomicUsize>,
        /// refuse every n-th request with `fail_status` (0: none)
        fail_every: Arc<AtomicUsize>,
        /// what a refused request gets (0: 503)
        fail_status: Arc<AtomicUsize>,
    }

    impl Fake {
        /// Some(status) when this request is refused.
        fn refuse(&self) -> Option<StatusCode> {
            let n = self.requests.fetch_add(1, SeqCst) + 1;
            let mut left = self.fail_next.load(SeqCst);
            let burst = loop {
                if left == 0 {
                    break false;
                }
                match self.fail_next.compare_exchange(left, left - 1, SeqCst, SeqCst) {
                    Ok(_) => break true,
                    Err(now) => left = now,
                }
            };
            let every = self.fail_every.load(SeqCst);
            (burst || (every > 0 && n % every == 0)).then(|| match self.fail_status.load(SeqCst) {
                0 => StatusCode::SERVICE_UNAVAILABLE,
                s => StatusCode::from_u16(s as u16).unwrap(),
            })
        }
    }

    pub(crate) async fn serve(delay_ms: u64) -> (Fake, Storage, std::path::PathBuf) {
        async fn busy(f: &Fake) {
            let n = f.now.fetch_add(1, SeqCst) + 1;
            f.most.fetch_max(n, SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(f.delay_ms)).await;
            f.now.fetch_sub(1, SeqCst);
        }
        async fn put(State(f): State<Fake>, UrlPath(p): UrlPath<String>, b: Bytes) -> StatusCode {
            if let Some(s) = f.refuse() {
                return s;
            }
            busy(&f).await;
            f.objects.lock().unwrap().insert(p, b.to_vec());
            StatusCode::OK
        }
        async fn read(State(f): State<Fake>, UrlPath(p): UrlPath<String>) -> axum::response::Response {
            if let Some(s) = f.refuse() {
                return s.into_response();
            }
            f.gets.fetch_add(1, SeqCst);
            busy(&f).await;
            f.objects.lock().unwrap().get(&p).cloned().map(IntoResponse::into_response).unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
        }
        async fn delete(State(f): State<Fake>, UrlPath(p): UrlPath<String>) -> StatusCode {
            if let Some(s) = f.refuse() {
                return s;
            }
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
            uploads: None,
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
            let r = Storage { db: Library(out), bucket: s.bucket.clone(), config: Config { keys: s.config.keys.clone(), active_key: "k".into(), bucket: "test".into(), region: "us-east-1".into(), endpoint: None, prefix: "".into(), uploads: None }, cache: Default::default(), inflight: Default::default(), requests: tokio::sync::Semaphore::new(8) };
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

    /// The server's store has game a, a save and a favorite; another machine's store (the same
    /// bucket and key, its own database) adds a again (the same files) and b, a newer revision
    /// of the save and a play session, and backs up. The server merges that backup: b shows up
    /// (entry, tree, every chunk reads back), a's chunks stay the server's objects and the other
    /// copies are retired, a's catalog files are the other's, the save is at the newer revision,
    /// the favorite stays, the session is added. gc then leaves the bucket holding what the
    /// server names plus the other machine's backup; a second merge adds nothing.
    #[tokio::test]
    async fn merge_another_stores_backup() {
        let (f, s, dir) = serve(0).await;
        let pc_db = Library::new(&dir.join("pc")).unwrap();
        let pc = Storage {
            db: pc_db.clone(),
            bucket: s.bucket.clone(),
            config: Config { keys: s.config.keys.clone(), active_key: s.config.active_key.clone(), bucket: "test".into(), region: "us-east-1".into(), endpoint: None, prefix: "".into(), uploads: None },
            cache: Default::default(),
            inflight: Default::default(),
            requests: tokio::sync::Semaphore::new(8),
        };
        let shared: Vec<u8> = (0..600_000u32).map(|i| (i * 13 % 251) as u8).collect();
        for (name, own) in [("a", 1u8), ("b", 2)] {
            let d = dir.join(format!("files-{name}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("game.exe"), [b'M', b'Z', own]).unwrap();
            std::fs::write(d.join("shared.dat"), &shared).unwrap();
        }
        let (server_store, pc_store) = (dir.join("server"), dir.join("pc"));
        crate::pack::add_for_test(&server_store, &dir.join("files-a"), "a", vec![]).unwrap();
        s.push_store(&server_store).await.unwrap();
        s.db.set_profile("a", &crate::library::Profile { favorite: true, note: "server".into() }).unwrap();
        let saves = |db: &Library, rev: i64| {
            let c = db.connect().unwrap();
            c.execute_batch("CREATE TABLE IF NOT EXISTS saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB);").unwrap();
            c.execute("INSERT INTO saves(name,revision,data) VALUES('a',?,NULL) ON CONFLICT(name) DO UPDATE SET revision=excluded.revision", [rev]).unwrap();
        };
        s.put("saves/a/1", b"save 1").await.unwrap();
        saves(&s.db, 1);
        crate::pack::add_for_test(&pc_store, &dir.join("files-a"), "a", vec![]).unwrap();
        crate::pack::add_for_test(&pc_store, &dir.join("files-b"), "b", vec![]).unwrap();
        pc.push_store(&pc_store).await.unwrap();
        pc.put("saves/a/2", b"save 2").await.unwrap();
        saves(&pc_db, 2);
        pc_db.connect().unwrap().execute("INSERT INTO sessions(id,game,started_ms,elapsed_ms) VALUES('s1','b',1,60000)", []).unwrap();
        pc.db.set_profile("a", &crate::library::Profile { favorite: false, note: "pc".into() }).unwrap();
        let server_a_chunk = |l: &str| s.db.connect().unwrap().query_row("SELECT envelope FROM objects WHERE logical=?", [l], |r| r.get::<_, String>(0)).unwrap();
        let a_chunks: Vec<String> = s.db.connect().unwrap().prepare("SELECT logical FROM objects WHERE logical LIKE 'chunks/%'").unwrap()
            .query_map([], |r| r.get(0)).unwrap().collect::<rusqlite::Result<_>>().unwrap();
        let before: Vec<String> = a_chunks.iter().map(|l| server_a_chunk(l)).collect();
        let backup = pc.backup().await.unwrap();

        let m = s.merge(backup, &server_store).await.unwrap();
        assert!(m.added > 0 && m.replaced >= 2 && m.kept >= a_chunks.len(), "{m:?}");
        assert_eq!((m.saves, m.records), (1, 1), "{m:?}");
        assert!(server_store.join("games/b.json").exists() && server_store.join("trees/b.tsv").exists());
        assert_eq!(a_chunks.iter().map(|l| server_a_chunk(l)).collect::<Vec<_>>(), before, "a's chunks stay the server's objects");
        *s.cache.lock().unwrap() = Default::default();
        for line in std::fs::read_to_string(server_store.join("trees/b.tsv")).unwrap().lines().filter(|l| l.starts_with("f\t")) {
            for c in line.split('\t').nth(5).unwrap().split(',').filter(|c| c.len() > 2) {
                assert!(s.get(&format!("chunks/{}/{c}", &c[..2])).await.unwrap().is_some(), "b's chunk {c}");
            }
        }
        let rev: i64 = s.db.connect().unwrap().query_row("SELECT revision FROM saves WHERE name='a'", [], |r| r.get(0)).unwrap();
        assert_eq!(rev, 2);
        assert_eq!(&*s.get("saves/a/2").await.unwrap().unwrap(), b"save 2");
        assert_eq!(s.db.profile("a").unwrap().note, "server");
        let sessions: i64 = s.db.connect().unwrap().query_row("SELECT COUNT(*) FROM sessions WHERE id='s1'", [], |r| r.get(0)).unwrap();
        assert_eq!(sessions, 1);

        s.gc(std::time::Duration::ZERO, None).await.unwrap();
        let pc_backup: std::collections::BTreeSet<String> = {
            let snapshot: String = pc_db.connect().unwrap().query_row("SELECT snapshot FROM backups", [], |r| r.get(0)).unwrap();
            [backup.to_string(), snapshot].into()
        };
        assert_eq!(names(&f), named(&s).union(&pc_backup).cloned().collect(), "the bucket: what the server names, and the other's backup");
        let again = s.merge(backup, &server_store).await.unwrap();
        assert_eq!(again.added, 0, "{again:?}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Two games pushed, with chunks in common: removing one drops its entry, tree and own
    /// chunks from the store's rows (the other's all stay and read back); a backup taken before
    /// keeps the objects in the bucket until it goes; then gc deletes exactly them, and a fresh
    /// server's catalog has only the other game.
    #[tokio::test]
    async fn removed_game_leaves_the_bucket() {
        let (f, s, dir) = serve(0).await;
        let store = dir.join("store");
        let shared: Vec<u8> = (0..600_000u32).map(|i| (i * 13 % 251) as u8).collect();
        for (name, own) in [("a", 1u8), ("b", 2)] {
            let d = dir.join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("game.exe"), [b'M', b'Z', own]).unwrap();
            std::fs::write(d.join("own.dat"), vec![own; 300_000]).unwrap();
            std::fs::write(d.join("shared.dat"), &shared).unwrap();
            crate::pack::add_for_test(&store, &d, name, vec![]).unwrap();
        }
        s.push_store(&store).await.unwrap();
        let mut b_files: Vec<String> = vec!["games/b.json".into(), "trees/b.tsv".into()];
        for line in std::fs::read_to_string(store.join("trees/b.tsv")).unwrap().lines().filter(|l| l.starts_with("f\t")) {
            b_files.extend(line.split('\t').nth(5).unwrap().split(',').filter(|c| c.len() > 2).map(|c| format!("chunks/{}/{c}", &c[..2])));
        }
        let backup = s.backup().await.unwrap();
        let before = names(&f);

        let gone = crate::pack::remove_game(&store, "a", Some(&s)).await.unwrap();
        assert!(gone.iter().any(|l| l.starts_with("chunks/")) && gone.contains(&"trees/a.tsv".into()) && gone.contains(&"games/a.json".into()), "{gone:?}");
        for l in &gone {
            assert!(s.get(l).await.unwrap().is_none(), "{l} still named");
            assert!(!store.join(l).exists(), "{l} still here");
        }
        // what b uses: chunks a shared are not among them, and all read back
        for l in &b_files {
            assert!(!gone.contains(l), "{l} is b's");
            assert!(s.get(l).await.unwrap().is_some(), "{l}");
        }

        age_retired(&s, 30 * 86_400_000);
        let r = s.gc(std::time::Duration::from_secs(86_400), None).await.unwrap();
        assert_eq!((r.deleted, names(&f)), (0, before.clone()), "the backup from before still needs them");
        let r = s.gc(std::time::Duration::from_secs(86_400), Some(0)).await.unwrap();
        assert_eq!(r.backups_dropped, 1);
        assert_eq!(r.deleted, gone.len());
        assert_eq!(names(&f), named(&s));
        assert_eq!(before.len() - names(&f).len(), gone.len() + 2, "a's objects and the backup's two");
        assert!(!names(&f).contains(&backup.to_string()));

        let fresh = dir.join("fresh");
        s.hydrate_catalog(&fresh).await.unwrap();
        assert!(fresh.join("games/b.json").exists() && fresh.join("trees/b.tsv").exists());
        assert!(!fresh.join("games/a.json").exists() && !fresh.join("trees/a.tsv").exists(), "a does not come back");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A bucket that turns requests away with 503 (as Hugging Face Buckets do under load):
    /// push_store still uploads the whole store, every object reads back, and gc's deletes go
    /// through; the refused requests were tried again, not skipped.
    #[tokio::test]
    async fn push_store_rides_out_503() {
        let (f, s, dir) = serve(0).await;
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
        // a burst at the start (more than PUTS at once, so every upload sees one), then every third
        f.fail_next.store(PUTS * 2, SeqCst);
        f.fail_every.store(3, SeqCst);
        s.push_store(&store).await.unwrap();
        assert_eq!(names(&f).len(), 66);
        assert!(f.requests.load(SeqCst) > 66 + PUTS * 2, "{}", f.requests.load(SeqCst));
        for i in 0..64 {
            let logical = format!("chunks/{:02x}/c{i}", i % 4);
            assert_eq!(&*s.get(&logical).await.unwrap().unwrap(), &[i as u8; 100][..], "{logical}");
        }
        assert_eq!(&*s.get("games/g.json").await.unwrap().unwrap(), b"game");
        // replaced objects are retired, and gc deletes them through the refusals too
        std::fs::write(store.join("games/g.json"), "game 2").unwrap();
        s.push_store(&store).await.unwrap();
        age_retired(&s, 30 * 86_400_000);
        let r = s.gc(std::time::Duration::from_secs(86_400), None).await.unwrap();
        assert!(r.deleted >= 1, "{r:?}");
        assert_eq!(names(&f), named(&s));
        f.fail_every.store(0, SeqCst);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// A bucket that keeps refusing: the upload fails after RETRIES more tries (not forever);
    /// an answer that will not change (403) fails at the first.
    #[tokio::test]
    async fn retries_end() {
        let (f, s, dir) = serve(0).await;
        f.fail_next.store(1000, SeqCst);
        let e = s.put("chunks/x", b"x").await.unwrap_err();
        assert!(e.to_string().contains("503"), "{e}");
        assert_eq!(f.requests.load(SeqCst), RETRIES as usize + 1);
        f.fail_next.store(1000, SeqCst);
        f.fail_status.store(403, SeqCst);
        f.requests.store(0, SeqCst);
        let e = s.put("chunks/x", b"x").await.unwrap_err();
        assert!(e.to_string().contains("403"), "{e}");
        assert_eq!(f.requests.load(SeqCst), 1);
        assert!(names(&f).is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// storage.toml's `uploads` sets how many go up at once (a far gateway: the round trip
    /// bounds one upload), and every chunk is named once all is through.
    #[tokio::test]
    async fn uploads_at_once_as_configured() {
        let (f, mut s, dir) = serve(40).await;
        s.config.uploads = Some(40);
        let store = dir.join("store");
        for i in 0..200 {
            let d = store.join(format!("chunks/{:02x}", i % 4));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(format!("c{i}")), (i as u32).to_le_bytes()).unwrap();
        }
        s.push_store(&store).await.unwrap();
        assert!(f.most.load(SeqCst) > PUTS, "{} at once", f.most.load(SeqCst));
        assert_eq!(names(&f).len(), 200);
        assert_eq!(named(&s).len(), 200);
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
        // the limits are inclusive: an item of exactly CACHE_ITEM, a cache of exactly the limit
        let mut c = Cache::default();
        c.put(Uuid::new_v4(), vec![0u8; CACHE_ITEM].into(), CACHE_ITEM);
        assert_eq!((c.bytes, c.map.len()), (CACHE_ITEM, 1));
        c.put(Uuid::new_v4(), vec![0u8; 1].into(), CACHE_ITEM);
        assert_eq!((c.bytes, c.map.len()), (1, 1), "the oldest goes first");
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

/// Uploads at once in push_store unless storage.toml says `uploads`.
const PUTS: usize = 16;
/// Objects push_store names in one transaction.
const RECORD_BATCH: usize = 64;

/// An object uploaded, not yet named (Storage::send, Storage::record).
struct Sent {
    logical: String,
    envelope: Envelope,
    size: i64,
}
/// Revisions of a save kept in S3 (older ones are retired, see [`Storage::gc`]).
pub const SAVE_REVISIONS: i64 = 8;

/// Tries after the first for a request the bucket could not answer (see [`retrying`]).
pub(crate) const RETRIES: u32 = 8;
/// Wait before the first try again; doubled each time, at most [`RETRY_MAX`].
#[cfg(not(test))]
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(1);
#[cfg(test)]
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_millis(1);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// Whether a request that got `status` may succeed when tried again: throttling (429) and the
/// server's own errors (5xx). Hugging Face Buckets answer 503 often while uploading many
/// objects at once; a store of 60k chunks never got through in one push without this.
fn retryable(status: u16) -> bool {
    status == 429 || status / 100 == 5
}

/// `request`, tried again with doubling waits ([`RETRIES`] times at most) when it fails to
/// connect or the bucket answers [`retryable`]; the last answer otherwise (a 404 or 403 at once).
async fn retrying<F, Fut>(mut request: F) -> Result<s3::request::ResponseData>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<s3::request::ResponseData, s3::error::S3Error>>,
{
    let mut wait = RETRY_FIRST;
    for attempt in 0..=RETRIES {
        let last = attempt == RETRIES;
        match request().await {
            Ok(r) if !retryable(r.status_code()) || last => return Ok(r),
            Ok(r) => eprintln!("S3: {}, trying again in {wait:?}", r.status_code()),
            Err(e) if last => return Err(e.into()),
            Err(e) => eprintln!("S3: {e}, trying again in {wait:?}"),
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(RETRY_MAX);
    }
    unreachable!()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// What [`Storage::merge`] did.
#[derive(Debug, Default, PartialEq)]
pub struct Merged {
    /// objects only the other store named
    pub added: usize,
    /// catalog files taken from the other store
    pub replaced: usize,
    /// named by both, kept as this store had them (the other's copy retired)
    pub kept: usize,
    /// the same object in both
    pub same: usize,
    /// saves added or moved to the other's higher revision
    pub saves: usize,
    /// profiles, play sessions and game settings added
    pub records: usize,
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
        let path = self.path(id);
        let r = retrying(|| self.bucket.put_object(&path, bytes)).await?;
        ensure!(
            r.status_code() / 100 == 2,
            "S3 upload failed: {}",
            r.status_code()
        );
        Ok(())
    }

    async fn download(&self, id: Uuid) -> Result<Vec<u8>> {
        let path = self.path(id);
        let r = retrying(|| self.bucket.get_object(&path)).await?;
        ensure!(
            r.status_code() == 200,
            "S3 download failed: {}",
            r.status_code()
        );
        Ok(r.bytes().to_vec())
    }

    pub async fn put(&self, logical: &str, bytes: &[u8]) -> Result<()> {
        let sent = self.send(logical, bytes).await?;
        self.record(&[sent])
    }

    /// Encrypts `bytes` as a new object and uploads it (not yet named: see [`Storage::record`]).
    async fn send(&self, logical: &str, bytes: &[u8]) -> Result<Sent> {
        let (envelope, ciphertext) = Envelope::encrypt(
            &self.key(&self.config.active_key)?,
            &self.config.active_key,
            bytes,
        )?;
        self.upload(envelope.object, &ciphertext).await?;
        Ok(Sent { logical: logical.to_string(), envelope, size: bytes.len() as i64 })
    }

    /// Names the objects `sent` in one transaction (one commit for many: a commit is a few
    /// fsyncs, and a push sends thousands of chunks); what they replace is retired.
    fn record(&self, sent: &[Sent]) -> Result<()> {
        let mut c = self.db.connect()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for s in sent {
            let old: Option<String> = tx.query_row("SELECT envelope FROM objects WHERE logical=?", [&s.logical], |r| r.get(0)).optional()?;
            tx.execute("INSERT INTO objects(logical,envelope,size) VALUES(?,?,?) ON CONFLICT(logical) DO UPDATE SET envelope=excluded.envelope,size=excluded.size",
                rusqlite::params![s.logical, serde_json::to_string(&s.envelope)?, s.size])?;
            if let Some(old) = old {
                retire(&tx, serde_json::from_str::<Envelope>(&old)?.object)?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Drops `logical`; its object is retired.
    pub fn forget(&self, logical: &str) -> Result<()> {
        self.forget_all(&[logical])
    }

    /// Drops every name in `logicals` in one transaction (a game's thousands of chunks: one
    /// commit, not one each); their objects are retired.
    pub fn forget_all<S: AsRef<str>>(&self, logicals: &[S]) -> Result<()> {
        let mut c = self.db.connect()?;
        let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        for logical in logicals {
            let logical = logical.as_ref();
            let old: Option<String> = tx.query_row("SELECT envelope FROM objects WHERE logical=?", [logical], |r| r.get(0)).optional()?;
            if let Some(old) = old {
                tx.execute("DELETE FROM objects WHERE logical=?", [logical])?;
                retire(&tx, serde_json::from_str::<Envelope>(&old)?.object)?;
            }
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
        let path = self.path(id);
        let r = retrying(|| self.bucket.delete_object(&path)).await?;
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

    /// Uploads the store's files not uploaded yet, `uploads` (storage.toml, [`PUTS`]) at once:
    /// chunks first, then trees, then games (a game's description goes up once what it names
    /// is there). The objects sent are named [`RECORD_BATCH`] at a time (a push cut short can
    /// leave up to that many sent and never named in the bucket).
    pub async fn push_store(&self, store: &Path) -> Result<()> {
        use futures_util::TryStreamExt;
        // (chunks are named by their content: one uploaded is never again)
        let have: std::collections::HashSet<String> = {
            let c = self.db.connect()?;
            let mut q = c.prepare("SELECT logical FROM objects WHERE logical LIKE 'chunks/%'")?;
            let rows = q.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
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
                        // (with / between the parts on Windows too: the names the S3 store knows)
                        let logical = path
                            .strip_prefix(store)?
                            .components()
                            .map(|c| c.as_os_str().to_str().context("non-UTF8 store path"))
                            .collect::<Result<Vec<_>>>()?
                            .join("/");
                        files.push((path, logical));
                    }
                }
            }
            files.sort();
            files.retain(|(_, logical)| !have.contains(logical));
            let mut sent = futures_util::stream::iter(files.into_iter().map(Ok::<_, anyhow::Error>))
                .map_ok(|(path, logical)| async move {
                    let data = tokio::fs::read(&path).await?;
                    self.send(&logical, &data).await
                })
                .try_buffer_unordered(self.config.uploads.unwrap_or(PUTS).max(1));
            let mut batch = Vec::new();
            while let Some(s) = sent.try_next().await? {
                batch.push(s);
                if batch.len() >= RECORD_BATCH {
                    self.record(&batch)?;
                    batch.clear();
                }
            }
            self.record(&batch)?;
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

    /// Merges another store's backup `id` (`ippai storage backup` there, the same bucket and
    /// keys) into this store's database, so games added and pushed elsewhere show up here:
    /// - objects only the other names are added (rewrapped with the active key, as `restore`);
    /// - a catalog file (games/, trees/) both name differently is taken from the other, this
    ///   one's object retired: what was added there last wins;
    /// - anything else both name (chunks: the same content; saves) stays this one's, and the
    ///   other's object is retired (`gc` deletes it);
    /// - a save takes the higher of the two revisions; profiles, play sessions and game settings
    ///   only the other has are added. Backups and the retired list stay this one's.
    ///
    /// Then `hydrate_catalog` brings the new games' entries and trees here.
    pub async fn merge(&self, id: Uuid, store: &Path) -> Result<Merged> {
        let other = self.db.0.with_file_name(format!(".merge-{}.sqlite3", Uuid::new_v4()));
        self.restore(id, &other).await?;
        let mut replaced = Vec::new();
        let result = (|| -> Result<Merged> {
            let mut out = Merged::default();
            let mut c = self.db.connect()?;
            c.execute("ATTACH DATABASE ? AS other", [other.to_str().context("non-UTF8 path")?])?;
            let tx = c.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let rows: Vec<(String, String, i64, Option<String>)> = {
                let mut q = tx.prepare("SELECT o.logical,o.envelope,o.size,m.envelope FROM other.objects o LEFT JOIN main.objects m ON m.logical=o.logical")?;
                let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            for (logical, theirs, size, ours) in rows {
                let object = |e: &str| -> Result<Uuid> { Ok(serde_json::from_str::<Envelope>(e)?.object) };
                match ours {
                    None => {
                        tx.execute("INSERT INTO main.objects(logical,envelope,size) VALUES(?,?,?)", rusqlite::params![logical, theirs, size])?;
                        out.added += 1;
                    }
                    Some(ours) if object(&ours)? == object(&theirs)? => out.same += 1,
                    Some(ours) if logical.starts_with("games/") || logical.starts_with("trees/") => {
                        tx.execute("UPDATE main.objects SET envelope=?,size=? WHERE logical=?", rusqlite::params![theirs, size, logical])?;
                        retire(&tx, object(&ours)?)?;
                        replaced.push(logical.clone());
                        out.replaced += 1;
                    }
                    Some(_) => {
                        retire(&tx, object(&theirs)?)?;
                        out.kept += 1;
                    }
                }
            }
            let has = |t: &str| -> rusqlite::Result<bool> {
                tx.query_row("SELECT EXISTS(SELECT 1 FROM other.sqlite_master WHERE type='table' AND name=?)", [t], |r| r.get(0))
            };
            if has("saves")? {
                tx.execute_batch("CREATE TABLE IF NOT EXISTS main.saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB);")?;
                out.saves = tx.execute(
                    "INSERT INTO main.saves(name,revision,data) SELECT name,revision,data FROM other.saves WHERE true
                     ON CONFLICT(name) DO UPDATE SET revision=excluded.revision,data=excluded.data WHERE excluded.revision>main.saves.revision",
                    [],
                )?;
            }
            for t in ["profiles", "sessions", "game_settings"] {
                if has(t)? {
                    out.records += tx.execute(&format!("INSERT OR IGNORE INTO main.{t} SELECT * FROM other.{t}"), [])?;
                }
            }
            tx.commit()?;
            c.execute("DETACH DATABASE other", [])?;
            Ok(out)
        })();
        let _ = std::fs::remove_file(&other);
        let out = result?;
        // (the old copies here go: hydrate_catalog fetches what is not here)
        for l in &replaced {
            let _ = std::fs::remove_file(store.join(l));
        }
        self.hydrate_catalog(store).await?;
        Ok(out)
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
