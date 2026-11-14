//! Explicitly run against a disposable RustFS server; never uses a production bucket.
use super::*;
use crate::{library::Profile, saves::Saves};
use std::sync::{atomic::{AtomicUsize, Ordering::SeqCst}, Arc};

async fn raw_objects(s: &Storage) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut objects = BTreeMap::new();
    for page in s.bucket.list(s.config.prefix.clone(), None).await? {
        for object in page.contents {
            let name = object.key.strip_prefix(&s.config.prefix).unwrap();
            assert_eq!(Uuid::parse_str(name)?.get_version_num(), 4);
            objects.insert(
                object.key.clone(),
                s.bucket.get_object(&object.key).await?.to_vec(),
            );
        }
    }
    Ok(objects)
}

#[tokio::test]
#[ignore = "requires disposable RustFS; run tools/ci/test-rustfs.sh"]
async fn rustfs_encrypted_storage() -> Result<()> {
    let endpoint = std::env::var("MASU_TEST_S3_ENDPOINT")
        .context("MASU_TEST_S3_ENDPOINT must point to disposable RustFS")?;
    let bucket_name = format!("masu-test-{}", Uuid::new_v4());
    let created = Bucket::create_with_path_style(
        &bucket_name,
        Region::Custom {
            region: "us-east-1".into(),
            endpoint: endpoint.clone(),
        },
        Credentials::default()?,
        s3::BucketConfiguration::default(),
    )
    .await?;
    ensure!(
        (200..300).contains(&created.response_code),
        "bucket creation failed"
    );
    let dir = std::env::temp_dir().join(&bucket_name);
    let db = Library::new(&dir)?;
    let config = dir.join("storage.toml");
    // (the disk cache in the test's own directory, not the user's)
    let cache = dir.join("s3");
    std::fs::write(&config, format!(
        "bucket = {bucket_name:?}\nregion = 'us-east-1'\nendpoint = {endpoint:?}\nprefix = 'test/'\nactive_key = 'old'\ncache = {cache:?}\n[keys]\nold = '{}'\nnew = '{}'\n",
        "01".repeat(32), "02".repeat(32)
    ))?;
    let mut s = Storage::open(db.clone(), &config)?;
    let result: Result<()> = async {
        db.set_profile(
            "game",
            &Profile {
                favorite: true,
                note: "private note".into(),
            },
        )?;
        let chunk = zstd::encode_all(&b"game payload for remote streaming"[..], 3)?;
        for (path, bytes) in [
            ("chunks/test", chunk.as_slice()),
            ("trees/test", &b"catalog"[..]),
            ("games/test", &b"game metadata"[..]),
        ] {
            let path = dir.join(path);
            std::fs::create_dir_all(path.parent().unwrap())?;
            std::fs::write(path, bytes)?;
        }
        s.push_store(&dir).await?;
        let original: Envelope = serde_json::from_str(&db.connect()?.query_row(
            "SELECT envelope FROM objects WHERE logical='chunks/test'",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        s.push_store(&dir).await?;
        let again: Envelope = serde_json::from_str(&db.connect()?.query_row(
            "SELECT envelope FROM objects WHERE logical='chunks/test'",
            [],
            |r| r.get::<_, String>(0),
        )?)?;
        assert_eq!(
            original.object, again.object,
            "existing chunks must not be uploaded again"
        );
        db.connect()?.execute_batch(
            "CREATE TABLE saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB, written_ms INTEGER);",
        )?;
        let saves = Saves {
            db: db.clone(),
            remote: Some(Storage::open(db.clone(), &config)?),
            lock: Default::default(),
        };
        assert!(saves.write("game", 0, b"private save").await?);
        assert!(!saves.write("game", 0, b"stale save").await?);
        assert_eq!(
            saves.read("game").await?,
            Some((1, b"private save".to_vec()))
        );
        let local_data: Option<Vec<u8>> =
            db.connect()?
                .query_row("SELECT data FROM saves", [], |r| r.get(0))?;
        assert!(local_data.is_none());
        drop(saves);

        let backup = s.backup().await?;
        let before = raw_objects(&s).await?;
        assert!(before.len() >= 6);
        for bytes in before.values() {
            for plain in [
                b"private save".as_slice(),
                b"private note",
                b"game metadata",
                b"SQLite format 3",
            ] {
                assert!(!bytes.windows(plain.len()).any(|w| w == plain));
            }
        }
        assert_ne!(s.download(original.object).await?, chunk);
        s.config.active_key = "new".into();
        s.rotate_objects()?;
        s.rotate_backup(backup).await?;
        let after = raw_objects(&s).await?;
        assert_eq!(before.len(), after.len());
        for (key, bytes) in &before {
            if *key != s.path(backup) {
                assert_eq!(after[key], *bytes, "rotation rewrote payload {key}");
            }
        }
        s.config.keys.remove("old");
        for name in ["chunks", "trees", "games"] {
            std::fs::remove_dir_all(dir.join(name))?;
        }
        let restored = dir.join("restored.sqlite3");
        s.restore(backup, &restored).await?;
        assert!(
            s.restore(backup, &restored).await.is_err(),
            "must refuse overwrite"
        );
        s.db = Library(restored);
        *s.cache.lock().unwrap() = Default::default();
        assert_eq!(s.db.profile("game")?.note, "private note");
        assert!(s.db.profile("game")?.favorite);
        s.hydrate_catalog(&dir).await?;
        assert_eq!(std::fs::read(dir.join("trees/test"))?, b"catalog");
        assert_eq!(std::fs::read(dir.join("games/test"))?, b"game metadata");
        assert!(
            !dir.join("chunks").exists(),
            "catalog hydration downloaded chunks"
        );
        assert_eq!(s.get("chunks/test").await?.as_deref(), Some(&chunk[..]));
        assert_eq!(s.get("saves/game/1").await?.as_deref(), Some(&b"private save"[..]));
        assert!(s.get("chunks/missing").await?.is_none());

        // A ciphertext modified in the real object store. The whole copy the get above kept
        // on the disk still answers (the disk write does not hold the get back: let it land)...
        let kept = s.disk.as_ref().context("no disk cache")?.path(original.object);
        for _ in 0..200 {
            if kept.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        ensure!(kept.exists(), "the chunk was not kept on the disk");
        let mut damaged = s.download(original.object).await?;
        damaged[0] ^= 1;
        s.upload(original.object, &damaged).await?;
        *s.cache.lock().unwrap() = Default::default();
        assert_eq!(s.get("chunks/test").await?.as_deref(), Some(&chunk[..]));
        // ...and with it gone (memory and disk both cold) the damaged one is refused
        s.disk.as_ref().unwrap().remove(original.object);
        *s.cache.lock().unwrap() = Default::default();
        assert!(s.get("chunks/test").await.is_err());
        s.disk = None;
        assert!(s.get("chunks/test").await.is_err());
        Ok(())
    }
    .await;
    // Clean up even on ordinary errors. Assertion failures remain in the disposable container.
    for page in s.bucket.list(String::new(), None).await? {
        for object in page.contents {
            s.bucket.delete_object(object.key).await?;
        }
    }
    s.bucket.delete().await?;
    std::fs::remove_dir_all(dir)?;
    result
}

/// A proxy in front of the S3 server at `upstream` that answers 503 to every `every`-th
/// request (as Hugging Face Buckets do under load) and passes the others through as they are,
/// Host header included so the signatures still hold. Gives its endpoint and how many
/// requests it refused and passed.
async fn flaky_proxy(upstream: String, every: usize) -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    use axum::{body::Body, extract::{Request, State}, http::StatusCode, response::Response, Router};
    #[derive(Clone)]
    struct P {
        upstream: String,
        every: usize,
        seen: Arc<AtomicUsize>,
        refused: Arc<AtomicUsize>,
        passed: Arc<AtomicUsize>,
        client: reqwest::Client,
    }
    async fn pass(State(p): State<P>, req: Request) -> Response {
        let n = p.seen.fetch_add(1, SeqCst) + 1;
        if n % p.every == 0 {
            p.refused.fetch_add(1, SeqCst);
            return Response::builder().status(StatusCode::SERVICE_UNAVAILABLE).body(Body::from("SlowDown")).unwrap();
        }
        let (parts, body) = req.into_parts();
        let body = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let path = parts.uri.path_and_query().map_or("/", |p| p.as_str());
        let r = p.client.request(parts.method, format!("{}{path}", p.upstream)).headers(parts.headers).body(body).send().await.unwrap();
        p.passed.fetch_add(1, SeqCst);
        let mut out = Response::builder().status(r.status());
        for (k, v) in r.headers() {
            if k != "transfer-encoding" && k != "connection" {
                out = out.header(k, v);
            }
        }
        out.body(Body::from(r.bytes().await.unwrap())).unwrap()
    }
    let p = P {
        upstream,
        every,
        seen: Default::default(),
        refused: Default::default(),
        passed: Default::default(),
        client: reqwest::Client::new(),
    };
    let (refused, passed) = (p.refused.clone(), p.passed.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let app = Router::new().fallback(pass).with_state(p);
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (endpoint, refused, passed)
}

/// RustFS behind a proxy that refuses every third request: push_store uploads the whole
/// store anyway (no object missing, none twice), and everything reads back through the
/// proxy. Without retrying, the first refusal ended the push ("S3 upload failed: 503").
#[tokio::test]
#[ignore = "requires disposable RustFS; run tools/ci/test-rustfs.sh"]
async fn rustfs_push_through_503() -> Result<()> {
    let upstream = std::env::var("MASU_TEST_S3_ENDPOINT")
        .context("MASU_TEST_S3_ENDPOINT must point to disposable RustFS")?;
    let bucket_name = format!("masu-test-{}", Uuid::new_v4());
    let direct = Bucket::create_with_path_style(
        &bucket_name,
        Region::Custom { region: "us-east-1".into(), endpoint: upstream.clone() },
        Credentials::default()?,
        s3::BucketConfiguration::default(),
    )
    .await?;
    ensure!((200..300).contains(&direct.response_code), "bucket creation failed");
    let direct = direct.bucket;
    let (endpoint, refused, passed) = flaky_proxy(upstream, 3).await;
    let dir = std::env::temp_dir().join(&bucket_name);
    let db = Library::new(&dir)?;
    let config = dir.join("storage.toml");
    std::fs::write(&config, format!(
        "bucket = {bucket_name:?}\nregion = 'us-east-1'\nendpoint = {endpoint:?}\nprefix = 'test/'\nactive_key = 'k'\n[keys]\nk = '{}'\n",
        "04".repeat(32)
    ))?;
    let s = Storage::open(db.clone(), &config)?;
    let mut files = Vec::new();
    for i in 0..48u32 {
        files.push((format!("chunks/{:02x}/c{i}", i % 4), zstd::encode_all(&i.to_le_bytes().repeat(1000)[..], 3)?));
    }
    files.push(("trees/t".into(), b"catalog".to_vec()));
    files.push(("games/g".into(), b"game metadata".to_vec()));
    for (logical, bytes) in &files {
        let path = dir.join(logical);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, bytes)?;
    }
    let result: Result<()> = async {
        s.push_store(&dir).await?;
        assert!(refused.load(SeqCst) >= files.len() / 3, "refused {}", refused.load(SeqCst));
        let mut stored = 0;
        for page in direct.list("test/".into(), None).await? {
            stored += page.contents.len();
        }
        assert_eq!(stored, files.len(), "one object for each file");
        *s.cache.lock().unwrap() = Default::default();
        for (logical, bytes) in &files {
            assert_eq!(s.get(logical).await?.as_deref(), Some(&bytes[..]), "{logical}");
        }
        eprintln!("through the proxy: {} refused, {} passed", refused.load(SeqCst), passed.load(SeqCst));
        Ok(())
    }
    .await;
    for page in direct.list(String::new(), None).await? {
        for object in page.contents {
            direct.delete_object(object.key).await?;
        }
    }
    direct.delete().await?;
    std::fs::remove_dir_all(dir)?;
    result
}

/// Against RustFS: two games pushed, one removed and gc run: the bucket then holds exactly what
/// the rows name (the other game whole, no object of the removed one), and a fresh server's
/// catalog has only the other game.
#[tokio::test]
#[ignore = "requires disposable RustFS; run tools/ci/test-rustfs.sh"]
async fn rustfs_remove_game() -> Result<()> {
    let endpoint = std::env::var("MASU_TEST_S3_ENDPOINT")
        .context("MASU_TEST_S3_ENDPOINT must point to disposable RustFS")?;
    let bucket_name = format!("masu-test-{}", Uuid::new_v4());
    let created = Bucket::create_with_path_style(
        &bucket_name,
        Region::Custom { region: "us-east-1".into(), endpoint: endpoint.clone() },
        Credentials::default()?,
        s3::BucketConfiguration::default(),
    )
    .await?;
    ensure!((200..300).contains(&created.response_code), "bucket creation failed");
    let dir = std::env::temp_dir().join(&bucket_name);
    let store = dir.join("store");
    let db = Library::new(&store)?;
    let config = dir.join("storage.toml");
    std::fs::write(&config, format!(
        "bucket = {bucket_name:?}\nregion = 'us-east-1'\nendpoint = {endpoint:?}\nprefix = 'test/'\nactive_key = 'k'\n[keys]\nk = '{}'\n",
        "05".repeat(32)
    ))?;
    let s = Storage::open(db.clone(), &config)?;
    let result: Result<()> = async {
        let shared: Vec<u8> = (0..600_000u32).map(|i| (i * 13 % 251) as u8).collect();
        for (name, own) in [("a", 1u8), ("b", 2)] {
            let d = dir.join(name);
            std::fs::create_dir_all(&d)?;
            std::fs::write(d.join("game.exe"), [b'M', b'Z', own])?;
            std::fs::write(d.join("own.dat"), vec![own; 300_000])?;
            std::fs::write(d.join("shared.dat"), &shared)?;
            crate::pack::add_for_test(&store, &d, name, vec![])?;
        }
        s.push_store(&store).await?;
        let pushed = raw_objects(&s).await?.len();
        let gone = crate::pack::remove_game(&store, "a", Some(&s)).await?;
        let r = s.gc(std::time::Duration::ZERO, None).await?;
        assert_eq!(r.deleted, gone.len());
        let left = raw_objects(&s).await?;
        assert_eq!(left.len(), pushed - gone.len());
        let rows: i64 = db.connect()?.query_row("SELECT COUNT(*) FROM objects", [], |r| r.get(0))?;
        assert_eq!(left.len() as i64, rows, "the bucket holds what the rows name");
        let fresh = dir.join("fresh");
        s.hydrate_catalog(&fresh).await?;
        assert!(fresh.join("games/b.json").exists() && !fresh.join("games/a.json").exists());
        *s.cache.lock().unwrap() = Default::default();
        for line in std::fs::read_to_string(fresh.join("trees/b.tsv"))?.lines().filter(|l| l.starts_with("f\t")) {
            for c in line.split('\t').nth(5).unwrap().split(',').map(|c| c.split(':').next().unwrap()).filter(|c| c.len() > 2) {
                assert!(s.get(&format!("chunks/{}/{c}", &c[..2])).await?.is_some(), "b's chunk {c}");
            }
        }
        Ok(())
    }
    .await;
    for page in s.bucket.list(String::new(), None).await? {
        for object in page.contents {
            s.bucket.delete_object(object.key).await?;
        }
    }
    s.bucket.delete().await?;
    std::fs::remove_dir_all(dir)?;
    result
}
