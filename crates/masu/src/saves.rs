//! Versioned saves; local SQLite or encrypted S3, with optimistic concurrency.
use crate::{library::Library, storage::{Storage, SAVE_REVISIONS}};
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use rusqlite::OptionalExtension;
use std::{path::Path as FsPath, sync::Arc};

/// How long a state file no index names is kept ([`Saves::prune_states`] when the server
/// starts): a device uploads the file before the index naming it.
pub const STATE_GRACE: std::time::Duration = std::time::Duration::from_secs(7 * 86400);

pub struct Saves {
    pub db: Library,
    pub remote: Option<Storage>,
    /// One writer per save name (the revision check needs no more): a big state going up to a
    /// slow bucket holds up only its own name (review D-1: it was one lock for every save of
    /// every game and device).
    pub lock: NameLocks,
}

/// A lock for each name that has a holder or waiters (gone with the last of them).
#[derive(Default)]
pub struct NameLocks(std::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>);

/// [`NameLocks::lock`]'s hold of one name.
pub struct NameGuard<'a> {
    locks: &'a NameLocks,
    name: String,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl NameLocks {
    pub async fn lock(&self, name: &str) -> NameGuard<'_> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner()).entry(name.to_string()).or_default().clone();
        // (dropped while waiting, the guard below is not made: the Arc goes with the future
        // and the next unlock of the name finds it unused)
        let mut cleanup = NameGuard { locks: self, name: name.to_string(), guard: None };
        cleanup.guard = Some(m.lock_owned().await);
        cleanup
    }

    /// Names with a lock now (tests: none left once nobody holds or waits).
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
}

impl Drop for NameGuard<'_> {
    fn drop(&mut self) {
        drop(self.guard.take());
        let mut map = self.locks.0.lock().unwrap_or_else(|e| e.into_inner());
        // (only the map's own reference left: no holder, no waiter; a new one clones under
        // this same lock)
        if map.get(&self.name).is_some_and(|m| Arc::strong_count(m) == 1) {
            map.remove(&self.name);
        }
    }
}
impl Saves {
    pub fn new(store: &FsPath) -> anyhow::Result<Arc<Self>> {
        let db = Library::new(store)?;
        db.connect()?.execute_batch("CREATE TABLE IF NOT EXISTS saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB);")?;
        written_column(&db)?;
        let remote = Storage::configured(db.clone())?;
        Ok(Arc::new(Self {
            db,
            remote,
            lock: NameLocks::default(),
        }))
    }
    pub async fn read(&self, name: &str) -> anyhow::Result<Option<(i64, Vec<u8>)>> {
        // (the row, a blob of up to 256 MiB, read on a blocking thread: review b01)
        let (db, n) = (self.db.clone(), name.to_string());
        let row: Option<(i64, Option<Vec<u8>>)> = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            Ok(db.connect()?.query_row("SELECT revision,data FROM saves WHERE name=?", [n], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
        })
        .await??;
        let Some((revision, data)) = row else {
            return Ok(None);
        };
        let data = match data {
            Some(data) => data,
            None => self
                .remote
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("S3 configuration required"))?
                .get(&format!("saves/{name}/{revision}"))
                .await?
                .ok_or_else(|| anyhow::anyhow!("save object missing"))?
                .to_vec(),
        };
        Ok(Some((revision, data)))
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn write(&self, name: &str, expected: i64, data: &[u8]) -> anyhow::Result<bool> {
        self.write_bytes(name, expected, Bytes::copy_from_slice(data)).await
    }

    /// [`Saves::write`] of a request's body (not copied). The database's parts run on blocking
    /// threads (a 256 MiB blob held a request thread for its whole write: review b01).
    pub async fn write_bytes(&self, name: &str, expected: i64, data: Bytes) -> anyhow::Result<bool> {
        let _guard = self.lock.lock(name).await;
        let (db, n) = (self.db.clone(), name.to_string());
        let revision: i64 = tokio::task::spawn_blocking(move || -> anyhow::Result<i64> {
            Ok(db.connect()?.query_row("SELECT revision FROM saves WHERE name=?", [n], |r| r.get(0)).optional()?.unwrap_or(0))
        })
        .await??;
        if revision != expected {
            return Ok(false);
        }
        let next = revision + 1;
        if let Some(remote) = &self.remote {
            remote.put(&format!("saves/{name}/{next}"), &data).await?;
        }
        let local = self.remote.is_none().then_some(data);
        let (db, n) = (self.db.clone(), name.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            db.connect()?.execute("INSERT INTO saves(name,revision,data,written_ms) VALUES(?,?,?,?) ON CONFLICT(name) DO UPDATE SET revision=excluded.revision,data=excluded.data,written_ms=excluded.written_ms", rusqlite::params![n,next,local.as_deref(),now_ms()])?;
            Ok(())
        })
        .await??;
        if let Some(remote) = &self.remote {
            // (the save is stored: a failure to forget old revisions is no failure of the write,
            // or the client's next PUT with the old revision gets 412: review b01)
            if let Err(e) = remote.prune_save(name, next) {
                eprintln!("saves: {name}: forgetting old revisions: {e:#}");
            }
        }
        Ok(true)
    }

    /// Deletes the save states' files (`state.<game>.<hash>`, named by their content, see
    /// web/src/savestate.ts) that no index of their game names, once written `grace` ago: an
    /// index is `state.<game>.index`, its current revision and, with S3, the older ones kept
    /// ([`SAVE_REVISIONS`]: a device may still list a state of one of them). Files of the old
    /// layout (`state.<game>.<slot>`) are left. Returns how many went.
    pub async fn prune_states(&self, grace: std::time::Duration) -> anyhow::Result<usize> {
        // (no lock while the indexes are read from the bucket: each game's files go later under
        // its index's lock, and only if the index stayed the revision read here)
        written_column(&self.db)?;
        let rows: Vec<(String, i64, i64)> = {
            let c = self.db.connect()?;
            let mut q = c.prepare("SELECT name,revision,written_ms FROM saves WHERE name LIKE 'state.%'")?;
            let v = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<Result<_, _>>()?;
            v
        };
        // (None: a game with an index that did not parse, whose files all stay; the revision:
        // the index's when read, 0 for none)
        let mut named: std::collections::HashMap<&str, (i64, Option<std::collections::HashSet<String>>)> = Default::default();
        for (name, revision, _) in &rows {
            let Some(("index", game)) = state_name(name) else { continue };
            let (seen, set) = named.entry(game).or_insert_with(|| (0, Some(Default::default())));
            *seen = *revision;
            let revisions = match &self.remote {
                Some(_) => (revision - SAVE_REVISIONS + 1).max(1)..=*revision,
                None => *revision..=*revision,
            };
            for r in revisions {
                let data = match &self.remote {
                    // (an older revision gone already: nothing it named is needed)
                    Some(remote) => remote.get(&format!("saves/{name}/{r}")).await?.map(|d| d.to_vec()),
                    None => self.read(name).await?.map(|(_, d)| d),
                };
                if let (Some(d), Some(hashes)) = (data, set.as_mut()) {
                    if !state_hashes(&d, hashes) {
                        *set = None;
                    }
                }
            }
        }
        let before = now_ms() - grace.as_millis() as i64;
        let mut doomed: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for (name, _, written) in &rows {
            let Some((hash, game)) = state_name(name) else { continue };
            if !is_hash(hash) || *written >= before || named.get(game).is_some_and(|(_, s)| s.as_ref().is_none_or(|s| s.contains(hash))) {
                continue;
            }
            doomed.entry(game).or_default().push(name);
        }
        let revision = |name: &str| -> anyhow::Result<Option<(i64, i64)>> {
            Ok(self.db.connect()?.query_row("SELECT revision,written_ms FROM saves WHERE name=?", [name], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
        };
        let mut gone = 0;
        for (game, names) in doomed {
            let index = format!("state.{game}.index");
            let _index = self.lock.lock(&index).await;
            // (an index written meanwhile may name any of them: they stay, until the next start)
            if revision(&index)?.map_or(0, |r| r.0) != named.get(game).map_or(0, |n| n.0) {
                continue;
            }
            for name in names {
                let _file = self.lock.lock(name).await;
                // (written again meanwhile: new)
                let Some((revision, written)) = revision(name)? else { continue };
                if written >= before {
                    continue;
                }
                if let Some(remote) = &self.remote {
                    let revs: Vec<String> = (1..=revision).map(|r| format!("saves/{name}/{r}")).collect();
                    remote.forget_all(&revs)?;
                }
                self.db.connect()?.execute("DELETE FROM saves WHERE name=?", [name])?;
                gone += 1;
            }
        }
        Ok(gone)
    }
}

/// Adds saves.written_ms (when a save was last written) to a database made before it; rows
/// without one (from before, or merged from another store) count as written now.
fn written_column(db: &Library) -> anyhow::Result<()> {
    let c = db.connect()?;
    let has: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('saves') WHERE name='written_ms')", [], |r| r.get(0))?;
    if !has {
        c.execute_batch("ALTER TABLE saves ADD COLUMN written_ms INTEGER")?;
    }
    c.execute("UPDATE saves SET written_ms=? WHERE written_ms IS NULL", [now_ms()])?;
    Ok(())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `state.<game>.<last>`: (last, game).
fn state_name(name: &str) -> Option<(&str, &str)> {
    let (rest, last) = name.strip_prefix("state.")?.rsplit_once('.')?;
    Some((last, rest))
}

/// A state file's content name (web/src/savestate.ts stateHash: 32 lowercase hex digits).
fn is_hash(s: &str) -> bool {
    s.len() == 32 && s.bytes().all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

/// The hashes of the states an index (`{"states":[{"hash":..},..]}`) names, into `into`;
/// false when it is not such an index.
fn state_hashes(index: &[u8], into: &mut std::collections::HashSet<String>) -> bool {
    let Some(states) = serde_json::from_slice::<serde_json::Value>(index).ok().and_then(|v| v["states"].as_array().cloned()) else { return false };
    for s in states {
        if let Some(h) = s["hash"].as_str() {
            into.insert(h.to_string());
        }
    }
    true
}

fn valid(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}
fn failure(e: anyhow::Error) -> Response {
    eprintln!("saves: {e}");
    StatusCode::BAD_GATEWAY.into_response()
}
async fn get_save(State(s): State<Arc<Saves>>, Path(name): Path<String>) -> Response {
    if !valid(&name) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    match s.read(&name).await {
        Ok(Some((rev, data))) => (
            [
                (header::ETAG, format!("\"{rev}\"")),
                (header::CACHE_CONTROL, "no-store".into()),
            ],
            data,
        )
            .into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => failure(e),
    }
}
async fn put_save(
    State(s): State<Arc<Saves>>,
    Path(name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !valid(&name) {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(expected) = headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim_matches('"').parse::<i64>().ok())
    else {
        return StatusCode::PRECONDITION_REQUIRED.into_response();
    };
    match s.write_bytes(&name, expected, body).await {
        Ok(true) => (
            StatusCode::NO_CONTENT,
            [(header::ETAG, format!("\"{}\"", expected + 1))],
        )
            .into_response(),
        Ok(false) => StatusCode::PRECONDITION_FAILED.into_response(),
        Err(e) => failure(e),
    }
}
pub fn router(s: Arc<Saves>) -> Router {
    Router::new()
        .route("/saves/{name}", get(get_save).put(put_save))
        .layer(axum::extract::DefaultBodyLimit::max(256 << 20))
        .with_state(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn http_save_preconditions_and_persistence() {
        let dir = std::env::temp_dir().join(format!("masu-save-http-{}", uuid::Uuid::new_v4()));
        let db = Library::new(&dir).unwrap();
        db.connect()
            .unwrap()
            .execute_batch(
                "CREATE TABLE saves(name TEXT PRIMARY KEY, revision INTEGER NOT NULL, data BLOB, written_ms INTEGER);",
            )
            .unwrap();
        let state = Arc::new(Saves {
            db,
            remote: None,
            lock: Default::default(),
        });
        let app = router(state.clone());
        let request = |method, path: &str, revision: Option<&str>, bytes: &[u8]| {
            let mut r = Request::builder().method(method).uri(path);
            if let Some(revision) = revision {
                r = r.header(header::IF_MATCH, revision);
            }
            r.body(Body::from(bytes.to_vec())).unwrap()
        };
        let missing = app
            .clone()
            .oneshot(request("GET", "/saves/game", None, b""))
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        for (revision, status) in [
            (None, StatusCode::PRECONDITION_REQUIRED),
            (Some("bad"), StatusCode::PRECONDITION_REQUIRED),
            (Some("\"1\""), StatusCode::PRECONDITION_FAILED),
            (Some("\"0\""), StatusCode::NO_CONTENT),
        ] {
            let r = app
                .clone()
                .oneshot(request("PUT", "/saves/game", revision, b"\0\xffsave"))
                .await
                .unwrap();
            assert_eq!(r.status(), status);
        }
        let stale = app
            .clone()
            .oneshot(request("PUT", "/saves/game", Some("\"0\""), b"stale"))
            .await
            .unwrap();
        assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
        let r = app
            .clone()
            .oneshot(request("GET", "/saves/game", None, b""))
            .await
            .unwrap();
        assert_eq!(r.headers()[header::ETAG], "\"1\"");
        assert_eq!(r.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            to_bytes(r.into_body(), 1024).await.unwrap().as_ref(),
            b"\0\xffsave"
        );
        let invalid = app
            .oneshot(request("PUT", "/saves/.hidden", Some("0"), b"invalid"))
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        let reopened = Saves::new(&dir).unwrap();
        assert_eq!(
            reopened.read("game").await.unwrap(),
            Some((1, b"\0\xffsave".to_vec()))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn index(hashes: &[(u32, &str)]) -> Vec<u8> {
        let states: Vec<serde_json::Value> = hashes.iter().map(|(slot, h)| serde_json::json!({"slot": slot, "hash": h, "created": 1})).collect();
        serde_json::to_vec(&serde_json::json!({ "states": states })).unwrap()
    }

    fn names(s: &Saves) -> std::collections::BTreeSet<String> {
        let c = s.db.connect().unwrap();
        let mut q = c.prepare("SELECT name FROM saves").unwrap();
        let v = q.query_map([], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        v
    }

    /// Two devices save slots of two games in turn (the file once, then the index merged with
    /// the server's, as web/src/savestate.ts putState), a game has an index that does not
    /// parse, and a file of the old layout: once old enough, the files no current index names
    /// go; what an index names, the bad index's game's files and the old layout's stay; a file
    /// written just now stays however unnamed; then saves go on.
    #[tokio::test]
    async fn prune_states_keeps_what_an_index_names() {
        let dir = std::env::temp_dir().join(format!("masu-prune-states-{}", uuid::Uuid::new_v4()));
        let s = Saves::new(&dir).unwrap();
        let mut seed = 0x9e3779b97f4a7c15u64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let mut slots: std::collections::BTreeMap<(&str, u32), String> = Default::default();
        let mut written = std::collections::BTreeSet::new();
        for step in 0..60 {
            let game = ["g", "h.x"][rnd(2) as usize];
            let slot = rnd(4) as u32;
            // (a state saved again with the same bytes: the same name)
            let hash = format!("{:032x}", if rnd(5) == 0 { 7 } else { step });
            let name = format!("state.{game}.{hash}");
            if s.read(&name).await.unwrap().is_none() {
                assert!(s.write(&name, 0, hash.as_bytes()).await.unwrap());
            }
            written.insert(name);
            slots.insert((game, slot), hash);
            let iname = format!("state.{game}.index");
            let rev = s.read(&iname).await.unwrap().map_or(0, |r| r.0);
            let list: Vec<(u32, &str)> = slots.iter().filter(|((g, _), _)| *g == game).map(|((_, k), h)| (*k, h.as_str())).collect();
            assert!(s.write(&iname, rev, &index(&list)).await.unwrap());
        }
        let bad = format!("state.bad.{:032x}", 1);
        s.write("state.bad.index", 0, b"{not json").await.unwrap();
        s.write(&bad, 0, b"x").await.unwrap();
        s.write("state.g.3", 0, b"old layout").await.unwrap();
        let all = names(&s);
        assert_eq!(s.prune_states(STATE_GRACE).await.unwrap(), 0, "nothing is old enough");
        assert_eq!(names(&s), all);
        s.db.connect().unwrap().execute("UPDATE saves SET written_ms=written_ms-?", [8 * 86_400_000i64]).unwrap();
        let fresh = format!("state.g.{:032x}", 999);
        s.write(&fresh, 0, b"uploaded, index not yet").await.unwrap();
        let named: std::collections::BTreeSet<String> = slots.iter().map(|((g, _), h)| format!("state.{g}.{h}")).collect();
        let gone = s.prune_states(STATE_GRACE).await.unwrap();
        let left = names(&s);
        let want: std::collections::BTreeSet<String> = all.iter().filter(|n| !written.contains(*n) || named.contains(*n)).cloned().chain([fresh.clone()]).collect();
        assert_eq!(left, want);
        assert_eq!(gone, written.len() - named.len());
        assert!(gone > 0);
        assert!(left.contains(&bad) && left.contains("state.g.3"));
        for n in &named {
            assert!(s.read(n).await.unwrap().is_some(), "{n}");
        }
        // (again: nothing more)
        assert_eq!(s.prune_states(STATE_GRACE).await.unwrap(), 0);
        // a file gone is written anew when saved again
        let again = written.difference(&named).next().unwrap();
        assert!(s.write(again, 0, b"again").await.unwrap());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn state_names() {
        assert_eq!(state_name("state.g.index"), Some(("index", "g")));
        assert_eq!(state_name("state.a.b.0123"), Some(("0123", "a.b")));
        assert_eq!(state_name("save.g.index"), None);
        assert!(is_hash(&"0a".repeat(16)) && !is_hash(&"0A".repeat(16)) && !is_hash("12") && !is_hash(&"g".repeat(32)));
    }
}
