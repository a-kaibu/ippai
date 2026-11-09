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

/// The game list (egs_index) is filled again past this age.
pub const EGS_GAMES_MAX_AGE_MS: i64 = 24 * 3_600_000;

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
            CREATE TABLE IF NOT EXISTS egs_games(id INTEGER PRIMARY KEY, name TEXT NOT NULL, votes INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS kv(key TEXT PRIMARY KEY, value TEXT NOT NULL);
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

    /// Every ErogameScape game kept (id, name, votes), and when the list was last filled (ms; 0:
    /// never).
    pub fn egs_games(&self) -> Result<(Vec<(u32, String, u32)>, i64)> {
        use rusqlite::OptionalExtension;
        let c = self.connect()?;
        let mut q = c.prepare("SELECT id,name,votes FROM egs_games ORDER BY id")?;
        let rows = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let at: Option<String> = c.query_row("SELECT value FROM kv WHERE key='egs_games_ms'", [], |r| r.get(0)).optional()?;
        Ok((rows, at.and_then(|s| s.parse().ok()).unwrap_or(0)))
    }

    /// Takes `list` as the games (names and votes updated, games not in it kept: a partial list
    /// loses nothing), now as the time it was filled.
    pub fn set_egs_games(&self, list: &[(u32, String, u32)]) -> Result<()> {
        let mut c = self.connect()?;
        let t = c.transaction()?;
        {
            let mut q = t.prepare("INSERT INTO egs_games(id,name,votes) VALUES(?,?,?) ON CONFLICT(id) DO UPDATE SET name=excluded.name,votes=max(excluded.votes,egs_games.votes)")?;
            for (id, name, votes) in list {
                q.execute(params![id, name, votes])?;
            }
        }
        t.execute("INSERT INTO kv(key,value) VALUES('egs_games_ms',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [now_ms().to_string()])?;
        t.commit()?;
        Ok(())
    }

    /// The games to match packages against (egs_match.rs), filled from ErogameScape when the list
    /// is a day old or empty (launcherg's list when ErogameScape cannot be reached and there is
    /// none yet); kept in memory while the list stays the same.
    pub fn egs_index(&self) -> Result<std::sync::Arc<crate::egs_match::Index>> {
        use std::sync::{Arc, Mutex};
        static CACHE: Mutex<Option<(PathBuf, i64, Arc<crate::egs_match::Index>)>> = Mutex::new(None);
        // (one filling at a time: the lock is held through it)
        let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        let (mut list, mut at) = self.egs_games()?;
        if list.is_empty() || now_ms() - at > EGS_GAMES_MAX_AGE_MS {
            // the game database's (gamedb.rs) first, then ErogameScape itself, then launcherg's
            let fresh = self.pull_gamedb().or_else(|e| {
                eprintln!("game database: {e:#}");
                crate::meta::egs_all_games()
            });
            let fresh = fresh.or_else(|e| {
                eprintln!("game list: {e:#}");
                if list.is_empty() { crate::meta::seed_games() } else { Err(e) }
            });
            match fresh {
                Ok(f) => {
                    self.set_egs_games(&f)?;
                    (list, at) = self.egs_games()?;
                }
                // (an old list stays in use; tried again in an hour)
                Err(e) if !list.is_empty() => {
                    eprintln!("game list: keeping the one from before: {e:#}");
                    self.connect()?.execute("UPDATE kv SET value=? WHERE key='egs_games_ms'", [(now_ms() - EGS_GAMES_MAX_AGE_MS + 3_600_000).to_string()])?;
                }
                Err(e) => return Err(e),
            }
        }
        if let Some((p, t, idx)) = cache.as_ref() {
            if *p == self.0 && *t == at {
                return Ok(idx.clone());
            }
        }
        let idx = Arc::new(crate::egs_match::Index::new(list));
        *cache = Some((self.0.clone(), at, idx.clone()));
        Ok(idx)
    }

    /// The game database's newest copy into `<store>/gamedb.sqlite` (when it changed), and its
    /// PC games.
    fn pull_gamedb(&self) -> Result<Vec<(u32, String, u32)>> {
        use rusqlite::OptionalExtension;
        let dest = self.0.with_file_name("gamedb.sqlite");
        let have: Option<String> = self.connect()?.query_row("SELECT value FROM kv WHERE key='gamedb_digest'", [], |r| r.get(0)).optional()?;
        let digest = crate::gamedb::pull(&dest, have.as_deref())?;
        self.connect()?.execute("INSERT INTO kv(key,value) VALUES('gamedb_digest',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [&digest])?;
        // (however old: it is rebuilt by hand, gamedb.yml)
        let (games, _) = crate::gamedb::egs_games(&dest)?;
        // (one built where ErogameScape never answered has VNDB's part only: entries, no list)
        ensure!(!games.is_empty(), "the game database has no ErogameScape games");
        Ok(games)
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

#[cfg(test)]
mod egs_games_tests {
    use super::*;
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct Stand {
        /// ErogameScape answers (else 503)
        up: AtomicBool,
        /// requests to /egs and /seed
        asked: AtomicUsize,
        /// the game database package's blob (zstd), when there is one
        gamedb: Mutex<Option<Vec<u8>>>,
        /// blob downloads
        blobs: AtomicUsize,
    }

    /// Stand-ins for ErogameScape's SQL form (POST /egs: max(id), then windows of ids),
    /// launcherg's list (GET /seed) and the package registry (/v2/t/gamedb: a token asked for by
    /// its 401, the blob behind a redirect that must come without it).
    fn sources(st: Arc<Stand>) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let at = format!("http://{}", l.local_addr().unwrap());
        let me = at.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let mut r = BufReader::new(s.try_clone().unwrap());
                let mut line = String::new();
                r.read_line(&mut line).unwrap();
                let mut h: HashMap<String, String> = HashMap::new();
                loop {
                    let mut x = String::new();
                    r.read_line(&mut x).unwrap();
                    if x.trim().is_empty() {
                        break;
                    }
                    if let Some((k, v)) = x.split_once(':') {
                        h.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
                    }
                }
                let mut body = vec![0; h.get("content-length").map_or(0, |v| v.parse().unwrap())];
                r.read_exact(&mut body).unwrap();
                let path = line.split(' ').nth(1).unwrap_or("").to_string();
                let auth = h.get("authorization").cloned().unwrap_or_default();
                let sql: String = form_urlencoded::parse(&body).find(|(k, _)| k == "sql").map(|(_, v)| v.into_owned()).unwrap_or_default();
                let blob = st.gamedb.lock().unwrap().clone();
                let digest = blob.as_ref().map(|b| format!("sha256:{}", ring::digest::digest(&ring::digest::SHA256, b).as_ref().iter().map(|x| format!("{x:02x}")).collect::<String>()));
                let (status, extra, ty, out): (&str, String, &str, Vec<u8>) = if path.starts_with("/v2/") && auth != "Bearer tok" {
                    ("401 Unauthorized", format!("www-authenticate: Bearer realm=\"{me}/token\",service=\"t\",scope=\"repository:t/gamedb:pull\"\r\n"), "text/plain", b"auth".to_vec())
                } else if path.starts_with("/token?") {
                    assert!(path.contains("scope=repository%3At%2Fgamedb%3Apull") || path.contains("scope=repository:t/gamedb:pull"), "{path}");
                    ("200 OK", String::new(), "application/json", br#"{"token":"tok"}"#.to_vec())
                } else if path == "/v2/t/gamedb/manifests/latest" {
                    match &digest {
                        Some(d) => ("200 OK", String::new(), "application/vnd.oci.image.manifest.v1+json", serde_json::json!({"schemaVersion": 2, "layers": [{"mediaType": "application/zstd", "digest": d, "size": blob.as_ref().unwrap().len()}]}).to_string().into_bytes()),
                        None => ("404 Not Found", String::new(), "text/plain", b"none".to_vec()),
                    }
                } else if path.starts_with("/v2/t/gamedb/blobs/") {
                    assert_eq!(Some(&path[19..]), digest.as_deref());
                    ("307 Temporary Redirect", format!("location: {me}/signed\r\n"), "text/plain", vec![])
                } else if path == "/signed" {
                    assert!(auth.is_empty(), "the signed URL gets no token");
                    st.blobs.fetch_add(1, Ordering::SeqCst);
                    ("200 OK", String::new(), "application/octet-stream", blob.unwrap())
                } else {
                    st.asked.fetch_add(1, Ordering::SeqCst);
                    if line.starts_with("GET /seed") {
                        ("200 OK", String::new(), "application/json", r#"[{"id":1,"gamename":"サクラノ詩 -櫻の森の上を舞う-","thumbnailUrl":""},{"id":2,"gamename":"終のステラ"}]"#.as_bytes().to_vec())
                    } else if !st.up.load(Ordering::SeqCst) {
                        ("503 Service Unavailable", String::new(), "text/plain", b"down".to_vec())
                    } else if sql.contains("max(id)") {
                        ("200 OK", String::new(), "text/html", "<table><tr><th>max</th></tr><tr><td>7000</td></tr></table>".as_bytes().to_vec())
                    } else if sql.contains("id >= 0 ") {
                        ("200 OK", String::new(), "text/html", "<table><tr><th>id</th><th>gamename</th><th>count2</th></tr><tr><td>1</td><td>サクラノ詩 -櫻の森の上を舞う-</td><td>5000</td></tr><tr><td>3</td><td>LOOPERS</td><td>800</td></tr></table>".as_bytes().to_vec())
                    } else {
                        ("200 OK", String::new(), "text/html", "<table><tr><th>id</th><th>gamename</th><th>count2</th></tr><tr><td>6000</td><td>アネモイ</td><td>90</td></tr></table>".as_bytes().to_vec())
                    }
                };
                let mut s = s;
                let _ = write!(s, "HTTP/1.1 {status}\r\n{extra}content-type: {ty}; charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n", out.len());
                let _ = s.write_all(&out);
            }
        });
        at
    }

    /// No list yet, no game database and ErogameScape down: launcherg's; a day later and
    /// ErogameScape up: its whole list (names and votes, games only the old one had kept); down
    /// again past a day: the list before stays, tried again within the hour, not on every call;
    /// then a game database published: its games and entries, downloaded again only when it
    /// changes.
    #[test]
    fn filled_from_the_game_database_erogamescape_or_the_seed() {
        let st = Arc::new(Stand::default());
        let at = sources(st.clone());
        // (the only test that sets these: meta.rs and gamedb.rs read them on each request; and
        // no proxy, which ureq would send these local requests through)
        for k in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            std::env::remove_var(k);
        }
        std::env::set_var("IPPAI_META_EGS_SQL", format!("{at}/egs"));
        std::env::set_var("IPPAI_META_SEED", format!("{at}/seed"));
        std::env::set_var("IPPAI_GAMEDB_REF", format!("{}/t/gamedb:latest", at.trim_start_matches("http://")));
        let dir = std::env::temp_dir().join(format!("ippai-test-{}", uuid::Uuid::new_v4()));
        let db = Library::new(&dir).unwrap();
        let q = |idx: &crate::egs_match::Index, s: &str| idx.best(&[s.to_string()]).map(|h| (h.id, h.votes));
        let stale = || db.connect().unwrap().execute("UPDATE kv SET value='0' WHERE key='egs_games_ms'", []).unwrap();

        let idx = db.egs_index().unwrap();
        assert_eq!(q(&idx, "終のステラ"), Some((2, 0)), "the seed");
        let n = st.asked.load(Ordering::SeqCst);
        let again = db.egs_index().unwrap();
        assert!(Arc::ptr_eq(&idx, &again), "the same list: kept in memory");
        assert_eq!(st.asked.load(Ordering::SeqCst), n, "not asked again within the day");

        st.up.store(true, Ordering::SeqCst);
        stale();
        let idx = db.egs_index().unwrap();
        assert_eq!(q(&idx, "サクラノ詩"), Some((1, 5000)), "votes from ErogameScape");
        assert_eq!(q(&idx, "アネモイ"), Some((6000, 90)), "the second window");
        assert_eq!(q(&idx, "LOOPERS"), Some((3, 800)));
        assert_eq!(q(&idx, "終のステラ"), Some((2, 0)), "kept from the seed");

        st.up.store(false, Ordering::SeqCst);
        stale();
        let idx = db.egs_index().unwrap();
        assert_eq!(q(&idx, "アネモイ"), Some((6000, 90)), "the list before");
        let n = st.asked.load(Ordering::SeqCst);
        db.egs_index().unwrap();
        assert_eq!(st.asked.load(Ordering::SeqCst), n, "not tried again at once");
        let (_, t) = db.egs_games().unwrap();
        assert!(now_ms() - t > EGS_GAMES_MAX_AGE_MS - 3_700_000, "tried again within the hour");

        // the game database
        let file = dir.join("built.sqlite");
        let row = |kv: &[(&str, &str)]| kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<crate::meta::Row>();
        crate::gamedb::write(
            &file,
            &[(777, row(&[("id", "777"), ("gamename", "ゲームデータベースの作品"), ("count2", "42"), ("model", "PC"), ("brandname", "ブランド")]), vec![])],
            &[serde_json::json!({"id": "v5", "title": "VN", "votecount": 1})],
        )
        .unwrap();
        // (built by hand two months ago: still taken)
        rusqlite::Connection::open(&file).unwrap().execute("UPDATE info SET value=? WHERE key='built_ms'", [(now_ms() - 60 * 86_400_000).to_string()]).unwrap();
        *st.gamedb.lock().unwrap() = Some(zstd::encode_all(&std::fs::read(&file).unwrap()[..], 3).unwrap());
        stale();
        let n = st.asked.load(Ordering::SeqCst);
        let idx = db.egs_index().unwrap();
        assert_eq!(q(&idx, "ゲームデータベースの作品"), Some((777, 42)));
        assert_eq!(st.asked.load(Ordering::SeqCst), n, "ErogameScape not asked");
        let local = dir.join("gamedb.sqlite");
        let i = crate::meta::lookup("", Some("egs:777"), Some(&local)).unwrap();
        assert_eq!((i.title.as_str(), i.brand.as_str()), ("ゲームデータベースの作品", "ブランド"));
        assert_eq!(crate::meta::lookup("", Some("vndb:v5"), Some(&local)).unwrap().title, "VN");
        assert_eq!(st.blobs.load(Ordering::SeqCst), 1);
        stale();
        db.egs_index().unwrap();
        assert_eq!(st.blobs.load(Ordering::SeqCst), 1, "the same digest: not downloaded again");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
