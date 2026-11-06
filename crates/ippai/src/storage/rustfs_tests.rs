//! Explicitly run against a disposable RustFS server; never uses a production bucket.
use super::*;
use crate::{library::Profile, saves::Saves};

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
    let endpoint = std::env::var("IPPAI_TEST_S3_ENDPOINT")
        .context("IPPAI_TEST_S3_ENDPOINT must point to disposable RustFS")?;
    let bucket_name = format!("ippai-test-{}", Uuid::new_v4());
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
    std::fs::write(&config, format!(
        "bucket = {bucket_name:?}\nregion = 'us-east-1'\nendpoint = {endpoint:?}\nprefix = 'test/'\nactive_key = 'old'\n[keys]\nold = '{}'\nnew = '{}'\n",
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
            "CREATE TABLE saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB);",
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

        // A cold cache must reject a modified ciphertext from the real object store.
        let mut damaged = s.download(original.object).await?;
        damaged[0] ^= 1;
        s.upload(original.object, &damaged).await?;
        *s.cache.lock().unwrap() = Default::default();
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
