//! User data is separate from the immutable game store.
use anyhow::{ensure, Result};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct Library(pub PathBuf);

#[derive(Default, Serialize, Deserialize)]
pub struct Profile {
    pub favorite: bool,
    pub note: String,
}

/// A game's metadata fetched from a page the user gave (library_api.rs `PUT /api/library/<game>/meta`):
/// the state of the last fetch. Its result stands in for the store entry's `meta` and cover.
#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct MetaStatus {
    pub url: String,
    /// "pending", "ok" or "error"
    pub state: String,
    pub error: String,
    pub updated_ms: i64,
}

/// A fetch left pending this long (the server stopped during it) reads as failed.
pub const META_STALE_MS: i64 = 10 * 60_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub game: String,
    pub started_ms: i64,
    pub elapsed_ms: i64,
}

impl Library {
    pub fn new(store: &Path) -> Result<Self> {
        std::fs::create_dir_all(store)?;
        let db = Self(store.join("library.sqlite3"));
        let c = db.connect()?;
        c.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS profiles(game TEXT PRIMARY KEY, favorite INTEGER NOT NULL DEFAULT 0, note TEXT NOT NULL DEFAULT '');
            CREATE TABLE IF NOT EXISTS sessions(id TEXT PRIMARY KEY, game TEXT NOT NULL, started_ms INTEGER NOT NULL, elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms >= 0));
            CREATE INDEX IF NOT EXISTS sessions_game ON sessions(game,started_ms);
            CREATE TABLE IF NOT EXISTS objects(logical TEXT PRIMARY KEY, envelope TEXT NOT NULL, size INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS game_settings(game TEXT PRIMARY KEY, config TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS retired(object TEXT PRIMARY KEY, retired_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS backups(id TEXT PRIMARY KEY, snapshot TEXT NOT NULL, created_ms INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS game_meta(game TEXT PRIMARY KEY, url TEXT NOT NULL, job TEXT NOT NULL, state TEXT NOT NULL CHECK(state IN ('pending','ok','error')), error TEXT NOT NULL DEFAULT '', info TEXT, cover BLOB, cover_type TEXT, updated_ms INTEGER NOT NULL);
            PRAGMA user_version=1;")?;
        Ok(db)
    }

    pub fn connect(&self) -> Result<Connection> {
        let c = Connection::open(&self.0)?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        c.execute_batch("PRAGMA foreign_keys=ON;")?;
        Ok(c)
    }

    pub fn profile(&self, game: &str) -> Result<Profile> {
        use rusqlite::OptionalExtension;
        Ok(self
            .connect()?
            .query_row(
                "SELECT favorite,note FROM profiles WHERE game=?",
                [game],
                |r| {
                    Ok(Profile {
                        favorite: r.get(0)?,
                        note: r.get(1)?,
                    })
                },
            )
            .optional()?
            .unwrap_or_default())
    }

    pub fn apply_settings(&self, game: &mut serde_json::Value) -> Result<()> {
        use rusqlite::OptionalExtension;
        let Some(id) = game["id"].as_str().map(str::to_owned) else {
            return Ok(());
        };
        let settings: Option<String> = self
            .connect()?
            .query_row("SELECT config FROM game_settings WHERE game=?", [&id], |r| {
                r.get(0)
            })
            .optional()?;
        if let Some(settings) = settings {
            let saved: serde_json::Value = serde_json::from_str(&settings)?;
            for field in ["run", "mounts", "args", "env"] {
                if let Some(value) = saved.get(field) {
                    game[field] = value.clone();
                }
            }
        }
        self.apply_meta(&id, game)
    }

    /// The metadata fetched from the user's page in place of the store entry's: `meta`, and
    /// `cover_src` (the page's cover, versioned) when one came with it.
    pub fn apply_meta(&self, id: &str, game: &mut serde_json::Value) -> Result<()> {
        use rusqlite::OptionalExtension;
        let row: Option<(Option<String>, bool, i64)> = self
            .connect()?
            .query_row("SELECT info, cover IS NOT NULL, updated_ms FROM game_meta WHERE game=? AND info IS NOT NULL", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()?;
        if let Some((Some(info), cover, updated)) = row {
            game["meta"] = serde_json::from_str(&info)?;
            if cover {
                game["cover_src"] = format!("api/library/{id}/cover?v={updated}").into();
            }
        }
        Ok(())
    }

    /// Starts a fetch of `url` for `game`: its state is pending until `finish_meta` with the
    /// job returned (a later start makes an earlier job's result go nowhere).
    pub fn begin_meta(&self, game: &str, url: &str) -> Result<String> {
        ensure!(url.len() <= 2048, "url too long");
        let job = uuid::Uuid::new_v4().to_string();
        self.connect()?.execute(
            "INSERT INTO game_meta(game,url,job,state,error,updated_ms) VALUES(?,?,?,'pending','',?) ON CONFLICT(game) DO UPDATE SET url=excluded.url,job=excluded.job,state='pending',error='',updated_ms=excluded.updated_ms",
            params![game, url, job, now_ms()],
        )?;
        Ok(job)
    }

    /// Ends `job`: its info (JSON) and cover (bytes, media type; `None` keeps the one before), or
    /// the error. False when the job is no longer the game's last.
    pub fn finish_meta(&self, game: &str, job: &str, result: std::result::Result<(String, Option<(Vec<u8>, String)>), String>) -> Result<bool> {
        let c = self.connect()?;
        let n = match result {
            Ok((info, cover)) => {
                let (bytes, ty) = cover.map_or((None, None), |(b, t)| (Some(b), Some(t)));
                c.execute(
                    "UPDATE game_meta SET state='ok',error='',info=?,cover=coalesce(?,cover),cover_type=coalesce(?,cover_type),updated_ms=? WHERE game=? AND job=?",
                    params![info, bytes, ty, now_ms(), game, job],
                )?
            }
            Err(e) => c.execute(
                "UPDATE game_meta SET state='error',error=?,updated_ms=? WHERE game=? AND job=?",
                params![e.chars().take(1000).collect::<String>(), now_ms(), game, job],
            )?,
        };
        Ok(n == 1)
    }

    pub fn meta_status(&self, game: &str) -> Result<Option<MetaStatus>> {
        use rusqlite::OptionalExtension;
        let s = self
            .connect()?
            .query_row("SELECT url,state,error,updated_ms FROM game_meta WHERE game=?", [game], |r| {
                Ok(MetaStatus { url: r.get(0)?, state: r.get(1)?, error: r.get(2)?, updated_ms: r.get(3)? })
            })
            .optional()?;
        Ok(s.map(|mut s| {
            if s.state == "pending" && now_ms() - s.updated_ms > META_STALE_MS {
                s.state = "error".into();
                s.error = "取得が途中で止まりました（サーバーの再起動など）".into();
            }
            s
        }))
    }

    /// The cover fetched with the game's metadata: bytes and media type.
    pub fn meta_cover(&self, game: &str) -> Result<Option<(Vec<u8>, String)>> {
        use rusqlite::OptionalExtension;
        Ok(self
            .connect()?
            .query_row("SELECT cover,cover_type FROM game_meta WHERE game=? AND cover IS NOT NULL", [game], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?)
    }

    pub fn set_profile(&self, game: &str, p: &Profile) -> Result<()> {
        ensure!(p.note.len() <= 100_000, "note too long");
        self.connect()?.execute("INSERT INTO profiles(game,favorite,note) VALUES(?,?,?) ON CONFLICT(game) DO UPDATE SET favorite=excluded.favorite,note=excluded.note",
            params![game,p.favorite,p.note])?;
        Ok(())
    }

    pub fn favorites(&self) -> Result<Vec<String>> {
        let c = self.connect()?;
        let mut q = c.prepare("SELECT game FROM profiles WHERE favorite=1 ORDER BY game")?;
        let rows = q.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    // Cumulative time makes retries and out-of-order heartbeats idempotent.
    pub fn session(&self, s: &Session) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&s.id)?.get_version_num() == 4,
            "invalid session id"
        );
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis() as i64;
        ensure!(
            s.started_ms > 0
                && s.started_ms <= now + 60_000
                && s.elapsed_ms >= 0
                && s.elapsed_ms <= (now - s.started_ms).max(0) + 60_000,
            "invalid session time"
        );
        let c = self.connect()?;
        let n = c.execute("INSERT INTO sessions(id,game,started_ms,elapsed_ms) VALUES(?,?,?,?) ON CONFLICT(id) DO UPDATE SET elapsed_ms=max(sessions.elapsed_ms,excluded.elapsed_ms) WHERE sessions.game=excluded.game AND sessions.started_ms=excluded.started_ms",
            params![s.id,s.game,s.started_ms,s.elapsed_ms])?;
        ensure!(n == 1, "session identity conflict");
        Ok(())
    }

    pub fn sessions(&self) -> Result<Vec<Session>> {
        let c = self.connect()?;
        let mut q = c.prepare(
            "SELECT id,game,started_ms,elapsed_ms FROM sessions ORDER BY started_ms DESC",
        )?;
        let rows = q.query_map([], |r| {
            Ok(Session {
                id: r.get(0)?,
                game: r.get(1)?,
                started_ms: r.get(2)?,
                elapsed_ms: r.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn snapshot(&self, path: &Path) -> Result<()> {
        ensure!(!path.exists(), "snapshot destination already exists");
        self.connect()?.backup("main", path, None)?;
        let c = Connection::open(path)?;
        let check: String = c.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
        ensure!(check == "ok", "snapshot integrity check failed");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn favorites_in_order() {
        let dir = std::env::temp_dir().join(format!("ippai-test-{}", uuid::Uuid::new_v4()));
        let db = Library::new(&dir).unwrap();
        for (g, f) in [("b", true), ("c", false), ("a", true)] {
            db.set_profile(g, &Profile { favorite: f, note: String::new() }).unwrap();
        }
        assert_eq!(db.favorites().unwrap(), ["a", "b"]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn retries_reordering_and_snapshot() {
        let dir = std::env::temp_dir().join(format!("ippai-test-{}", uuid::Uuid::new_v4()));
        let db = Library::new(&dir).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
        let mut s = Session {
            id: uuid::Uuid::new_v4().to_string(),
            game: "game".into(),
            started_ms: now - 20_000,
            elapsed_ms: 10_000,
        };
        db.session(&s).unwrap();
        db.session(&s).unwrap();
        s.elapsed_ms = 5_000;
        db.session(&s).unwrap();
        assert_eq!(db.sessions().unwrap()[0].elapsed_ms, 10_000);
        s.game = "other".into();
        assert!(db.session(&s).is_err());
        db.set_profile(
            "game",
            &Profile {
                favorite: true,
                note: "メモ".into(),
            },
        )
        .unwrap();
        let snapshot = dir.join("snapshot.db");
        db.snapshot(&snapshot).unwrap();
        let restored = Library(snapshot);
        assert!(restored.profile("game").unwrap().favorite);
        assert_eq!(restored.sessions().unwrap().len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[cfg(test)]
mod meta_tests {
    use super::*;
    use proptest::prelude::*;

    fn db() -> (Library, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ippai-test-{}", uuid::Uuid::new_v4()));
        (Library::new(&dir).unwrap(), dir)
    }

    #[test]
    fn stale_pending_reads_as_failed() {
        let (db, dir) = db();
        db.begin_meta("g", "egs:1").unwrap();
        assert_eq!(db.meta_status("g").unwrap().unwrap().state, "pending");
        db.connect().unwrap().execute("UPDATE game_meta SET updated_ms=updated_ms-?", [META_STALE_MS + 1]).unwrap();
        let s = db.meta_status("g").unwrap().unwrap();
        assert_eq!(s.state, "error");
        assert!(!s.error.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Settings and fetched metadata both apply; a game with neither is left as it is.
    #[test]
    fn list_entry() {
        let (db, dir) = db();
        let plain = serde_json::json!({"id": "a", "title": "A", "meta": {"title": "old"}, "cover": "a.jpg"});
        let mut g = plain.clone();
        db.apply_settings(&mut g).unwrap();
        assert_eq!(g, plain);
        let job = db.begin_meta("a", "egs:1").unwrap();
        db.apply_settings(&mut g).unwrap();
        assert_eq!(g, plain, "pending: nothing yet");
        assert!(db.finish_meta("a", &job, Ok((r#"{"title":"new"}"#.into(), None))).unwrap());
        db.apply_settings(&mut g).unwrap();
        assert_eq!(g["meta"]["title"], "new");
        assert!(g.get("cover_src").is_none(), "no cover fetched: the store's stays");
        assert_eq!(g["cover"], "a.jpg");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[derive(Debug, Clone)]
    enum Op {
        Begin,
        /// ends the k-th job started (mod count), with a result or an error
        Finish(usize, bool),
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        /// Model: only the last job started lands; its result replaces the info, an error keeps it;
        /// a cover once fetched stays (this model's results all bring one or none alternately).
        #[test]
        fn only_the_last_job_lands(ops in proptest::collection::vec(prop_oneof![Just(Op::Begin), (0usize..8, any::<bool>()).prop_map(|(k, ok)| Op::Finish(k, ok))], 1..24)) {
            let (db, dir) = db();
            let mut jobs: Vec<String> = vec![];
            let mut done: Vec<bool> = vec![];
            let (mut info, mut state, mut cover): (Option<String>, Option<&str>, Option<Vec<u8>>) = (None, None, None);
            for (step, op) in ops.iter().enumerate() {
                match op {
                    Op::Begin => {
                        jobs.push(db.begin_meta("g", &format!("egs:{}", step + 1)).unwrap());
                        done.push(false);
                        state = Some("pending");
                    }
                    Op::Finish(k, ok) => {
                        if jobs.is_empty() { continue; }
                        let k = k % jobs.len();
                        let c = (step % 2 == 0).then(|| vec![step as u8]);
                        let r = if *ok { Ok((format!("{{\"n\":{step}}}"), c.clone().map(|b| (b, "image/png".to_string())))) } else { Err(format!("e{step}")) };
                        let landed = db.finish_meta("g", &jobs[k], r).unwrap();
                        // (a job ended once may end again while it is the last: the same row)
                        let current = k == jobs.len() - 1;
                        prop_assert_eq!(landed, current);
                        if current {
                            done[k] = true;
                            if *ok {
                                info = Some(format!("{{\"n\":{step}}}"));
                                if c.is_some() { cover = c; }
                                state = Some("ok");
                            } else {
                                state = Some("error");
                            }
                        }
                    }
                }
                let s = db.meta_status("g").unwrap();
                prop_assert_eq!(s.as_ref().map(|s| s.state.as_str()), state);
                let mut g = serde_json::json!({"id": "g"});
                db.apply_meta("g", &mut g).unwrap();
                prop_assert_eq!(g.get("meta").map(|m| m.to_string()), info.clone());
                prop_assert_eq!(db.meta_cover("g").unwrap().map(|c| c.0), cover.clone());
            }
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}
