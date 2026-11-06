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
        let Some(id) = game["id"].as_str() else {
            return Ok(());
        };
        let settings: Option<String> = self
            .connect()?
            .query_row("SELECT config FROM game_settings WHERE game=?", [id], |r| {
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
        Ok(())
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
