//! `GET /foresee/<tree>`: what each script of a packed tree names (crates/foresee: loose
//! scripts, and the script members of its KiriKiri .xp3 archives), read once from the store
//! ahead of any play and kept in `<jit>/foresee/<tree>.tsv`. The page hands it to kura at the
//! start (vnhost_vfs_foresee): a script the game reads then foresees the pictures, voices and
//! scripts it names and what the scripts it goes on to name, on the game's first run, whichever
//! way it goes. The first ask starts the reading and is answered 202; the page asks again.

use crate::treefiles::Trees;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Foresee {
    trees: Trees,
    dir: PathBuf,
    going: Arc<Mutex<HashSet<String>>>,
}

pub fn router(trees: Trees, jit: PathBuf) -> Router {
    Router::new().route("/foresee/{tree}", get(table)).with_state(Foresee { trees, dir: jit.join("foresee"), going: Default::default() })
}

async fn table(State(f): State<Foresee>, Path(tree): Path<String>) -> Response {
    if tree.is_empty() || !tree.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return StatusCode::BAD_REQUEST.into_response();
    }
    let Some(changed) = f.trees.changed(&tree) else { return StatusCode::NOT_FOUND.into_response() };
    let path = f.dir.join(format!("{tree}.tsv"));
    // (made after the tree's list last changed: as is)
    if let Ok(m) = tokio::fs::metadata(&path).await {
        if m.modified().is_ok_and(|t| t >= changed) {
            if let Ok(text) = tokio::fs::read(&path).await {
                return ([(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], text).into_response();
            }
        }
    }
    if f.going.lock().unwrap().insert(tree.clone()) {
        let g = f.clone();
        tokio::spawn(async move {
            let text = build(&g.trees, &tree).await;
            let _ = tokio::fs::create_dir_all(&g.dir).await;
            let tmp = g.dir.join(format!("{tree}.tsv.tmp"));
            if tokio::fs::write(&tmp, &text).await.is_ok() {
                let _ = tokio::fs::rename(&tmp, g.dir.join(format!("{tree}.tsv"))).await;
            }
            g.going.lock().unwrap().remove(&tree);
        });
    }
    StatusCode::ACCEPTED.into_response()
}

/// Tree `tree`'s table (foresee::table_line for each script it has, loose or in an .xp3).
pub async fn build(trees: &Trees, tree: &str) -> String {
    let mut out = format!("{}\n", foresee::TABLE_HEADER);
    let mut files = trees.list(tree).unwrap_or_default();
    files.sort();
    for (path, size) in files {
        let mut cache = HashMap::new();
        if foresee::is_script(&path) {
            if size as usize <= foresee::SCRIPT_MAX {
                if let Some(b) = trees.read(tree, &path, 0, size, &mut cache).await {
                    out += &foresee::table_line(&path, &foresee::names_in(&b));
                }
            }
            continue;
        }
        if !path.ends_with(".xp3") {
            continue;
        }
        let Some(head) = trees.read(tree, &path, 0, 0x28, &mut cache).await else { continue };
        let Some(at) = foresee::xp3_index_at(&head) else { continue };
        let Some(len) = trees.read(tree, &path, at, 17, &mut cache).await.as_deref().and_then(foresee::xp3_index_len) else { continue };
        if len > foresee::INDEX_MAX as u64 {
            continue;
        }
        let Some(index) = trees.read(tree, &path, at, len, &mut cache).await.as_deref().and_then(foresee::xp3_index_bytes) else { continue };
        let mut members: Vec<foresee::Member> = foresee::xp3_index(&index).into_iter().filter(|m| foresee::is_script(&m.name)).collect();
        // (in the archive's order: the chunks they lie in read once)
        members.sort_by_key(|m| m.segments[0].0);
        for m in members {
            if m.segments.iter().map(|s| s.1).sum::<u64>() > foresee::SCRIPT_MAX as u64 {
                continue;
            }
            let mut parts = Vec::new();
            for s in &m.segments {
                match trees.read(tree, &path, s.0, s.1, &mut cache).await {
                    Some(b) => parts.push((b, s.2)),
                    None => break,
                }
            }
            if parts.len() != m.segments.len() {
                continue;
            }
            let refs: Vec<(&[u8], bool)> = parts.iter().map(|(b, z)| (&b[..], *z)).collect();
            if let Some(b) = foresee::member_bytes(&refs) {
                let names = foresee::names_in(&b);
                if !names.is_empty() {
                    out += &foresee::table_line(&m.name, &names);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::to_bytes, body::Body, http::Request};
    use tower::ServiceExt;

    /// A store with tree `g`: an .xp3 (a zlib'd UTF-16 scenario naming a picture and the next
    /// scenario; a plain one; the picture), a loose script, a picture; chunks of 64 bytes.
    fn store() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ippai-foresee-{}", uuid::Uuid::new_v4()));
        let mut line = |name: &str, data: &[u8]| {
            let mut field = Vec::new();
            for c in data.chunks(64) {
                let h = blake3::hash(c).to_hex()[..32].to_string();
                let d = dir.join("chunks").join(&h[..2]);
                std::fs::create_dir_all(&d).unwrap();
                std::fs::write(d.join(&h), zstd::encode_all(c, 3).unwrap()).unwrap();
                field.push(format!("{h}:{}", c.len()));
            }
            format!("f\t644\t0\t{name}\t{}\t{}\n", data.len(), field.join(","))
        };
        let utf16 = |s: &str| [&[0xff, 0xfe][..], &s.encode_utf16().flat_map(|c| c.to_le_bytes()).collect::<Vec<u8>>()].concat();
        let first = utf16("[bg storage=\"bg01\"]\r\n[jump storage=\"second.ks\"]");
        let arc = foresee::xp3_archive(&[("scenario/first.ks", &first, true), ("scenario/second.ks", b"@voice v_0002", false), ("bg01.png", &[7; 300], false)]);
        let mut tsv = String::from("vnvm-tree\t2\t64\n");
        tsv += &line("data.xp3", &arc);
        tsv += &line("Scene.ks", b"[image storage=cg01]");
        tsv += &line("cg01.png", &[1; 10]);
        std::fs::create_dir_all(dir.join("trees")).unwrap();
        std::fs::write(dir.join("trees/g.tsv"), tsv).unwrap();
        dir
    }

    #[tokio::test]
    async fn a_trees_scripts_and_what_they_name() {
        let dir = store();
        let trees = Trees::new(dir.clone()).unwrap();
        let text = build(&trees, "g").await;
        let t = foresee::parse_table(&text);
        let v = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(t["first.ks"], v(&["storage", "bg01", "jump", "second.ks"]));
        assert_eq!(t["second.ks"], v(&["voice", "v_0002"]));
        assert_eq!(t["scene.ks"], v(&["image", "storage", "cg01"]));
        assert_eq!(t.len(), 3, "{text}");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// GET /foresee/<tree>: 202 while it is read, then the table; read again when the tree's
    /// list changes; 404 for no tree, 400 for a name that is not one.
    #[tokio::test]
    async fn asked_read_then_kept() {
        let dir = store();
        let jit = dir.join("jit");
        let app = router(Trees::new(dir.clone()).unwrap(), jit.clone());
        let get = |p: &str| {
            let app = app.clone();
            let r = Request::builder().uri(p).body(Body::empty()).unwrap();
            async move {
                let res = app.oneshot(r).await.unwrap();
                (res.status(), to_bytes(res.into_body(), usize::MAX).await.unwrap().to_vec())
            }
        };
        assert_eq!(get("/foresee/g").await.0, StatusCode::ACCEPTED);
        let mut got = None;
        for _ in 0..200 {
            let (s, b) = get("/foresee/g").await;
            if s == StatusCode::OK {
                got = Some(b);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let got = String::from_utf8(got.expect("the table in time")).unwrap();
        assert!(got.starts_with(foresee::TABLE_HEADER) && got.contains("first.ks\t"));
        assert!(jit.join("foresee/g.tsv").exists());
        // (the tree's list newer: read again)
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let tsv = std::fs::read_to_string(dir.join("trees/g.tsv")).unwrap();
        std::fs::write(dir.join("trees/g.tsv"), tsv).unwrap();
        assert_eq!(get("/foresee/g").await.0, StatusCode::ACCEPTED);
        assert_eq!(get("/foresee/none").await.0, StatusCode::NOT_FOUND);
        assert_eq!(get("/foresee/..%2Fx").await.0, StatusCode::BAD_REQUEST);
        let _ = std::fs::remove_dir_all(dir);
    }
}
