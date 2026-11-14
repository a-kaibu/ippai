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

/// Tree `tree`'s table (foresee::table_line for each script it has: loose, in a KiriKiri .xp3,
/// in a BGI .arc, a Siglus Scene.pck's scenes).
pub async fn build(trees: &Trees, tree: &str) -> String {
    let mut out = format!("{}\n", foresee::TABLE_HEADER);
    let mut files = trees.list(tree).unwrap_or_default();
    files.sort();
    for (path, size) in &files {
        let mut cache = HashMap::new();
        let base = path.rsplit('/').next().unwrap_or(path);
        if base == "scene.pck" {
            out += &siglus(trees, tree, path, *size, &files).await;
        } else if foresee::is_script(path) {
            if *size as usize <= foresee::SCRIPT_MAX {
                if let Some(b) = trees.read(tree, path, 0, *size, &mut cache).await {
                    out += &foresee::table_line(path, &foresee::names_in(&b));
                }
            }
        } else if path.ends_with(".xp3") {
            out += &xp3(trees, tree, path, &mut cache).await;
        } else if path.ends_with(".arc") {
            out += &bgi(trees, tree, path, &mut cache).await;
        }
    }
    out
}

/// An archive's script members' lines (`members` in the archive's order; `script`: whether one
/// is a script, by its name or its first bytes).
async fn members_lines(trees: &Trees, tree: &str, path: &str, mut members: Vec<foresee::Member>, cache: &mut HashMap<u64, std::sync::Arc<[u8]>>, by_bytes: fn(&[u8]) -> bool) -> String {
    let mut out = String::new();
    // (in the archive's order: the chunks they lie in read once)
    members.sort_by_key(|m| m.segments[0].0);
    for m in members {
        if m.segments.iter().map(|s| s.1).sum::<u64>() > foresee::SCRIPT_MAX as u64 {
            continue;
        }
        if !foresee::is_script(&m.name) {
            // (named without a script's extension, as BGI's are: by its first bytes; an .xp3's
            // others are not read at all, or the whole archive would be)
            let Some(by_bytes) = by_bytes else { continue };
            let s = m.segments[0];
            if s.2 || !trees.read(tree, path, s.0, 64.min(s.1), cache).await.is_some_and(|h| by_bytes(&h)) {
                continue;
            }
        }
        let mut parts = Vec::new();
        for s in &m.segments {
            match trees.read(tree, path, s.0, s.1, cache).await {
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
    out
}

async fn xp3(trees: &Trees, tree: &str, path: &str, cache: &mut HashMap<u64, std::sync::Arc<[u8]>>) -> String {
    let members = xp3_members(trees, tree, path, cache).await;
    members_lines(trees, tree, path, members, cache, None).await
}

async fn bgi(trees: &Trees, tree: &str, path: &str, cache: &mut HashMap<u64, std::sync::Arc<[u8]>>) -> String {
    let members = bgi_members(trees, tree, path, cache).await;
    members_lines(trees, tree, path, members, cache, Some(foresee::is_bgi_script)).await
}

/// An .xp3's members, from its index.
async fn xp3_members(trees: &Trees, tree: &str, path: &str, cache: &mut HashMap<u64, std::sync::Arc<[u8]>>) -> Vec<foresee::Member> {
    let Some(head) = trees.read(tree, path, 0, 0x28, cache).await else { return Vec::new() };
    let Some(at) = foresee::xp3_index_at(&head) else { return Vec::new() };
    let Some(len) = trees.read(tree, path, at, 17, cache).await.as_deref().and_then(foresee::xp3_index_len) else { return Vec::new() };
    if len > foresee::INDEX_MAX as u64 {
        return Vec::new();
    }
    let Some(index) = trees.read(tree, path, at, len, cache).await.as_deref().and_then(foresee::xp3_index_bytes) else { return Vec::new() };
    foresee::xp3_index(&index)
}

/// A BGI .arc's members, from its index.
async fn bgi_members(trees: &Trees, tree: &str, path: &str, cache: &mut HashMap<u64, std::sync::Arc<[u8]>>) -> Vec<foresee::Member> {
    let Some(len) = trees.read(tree, path, 0, 16, cache).await.as_deref().and_then(foresee::bgi_index_len) else { return Vec::new() };
    if len > foresee::INDEX_MAX as u64 {
        return Vec::new();
    }
    let Some(head) = trees.read(tree, path, 0, len, cache).await else { return Vec::new() };
    foresee::bgi_index(&head)
}

/// Tree `tree`'s archive members, a line each: `<archive path>\t<member name>\t<start>\t<bytes>`
/// (their first segment; hints_eval.rs maps a list's chunk reads to the members they read).
pub async fn members(trees: &Trees, tree: &str) -> String {
    let mut out = String::new();
    let mut files = trees.list(tree).unwrap_or_default();
    files.sort();
    for (path, _) in &files {
        let mut cache = HashMap::new();
        let ms = if path.ends_with(".xp3") {
            xp3_members(trees, tree, path, &mut cache).await
        } else if path.ends_with(".arc") {
            bgi_members(trees, tree, path, &mut cache).await
        } else {
            continue;
        };
        for m in ms {
            let (start, n) = (m.segments[0].0, m.segments.iter().map(|s| s.1).sum::<u64>());
            out += &format!("{path}\t{}\t{start}\t{n}\n", m.name.replace(['\t', '\n'], " "));
        }
    }
    out
}

/// `ippai foresee <trees> --out <dir>`: each tree's table (`<tree>.tsv`, as GET /foresee
/// serves it) and its archives' members (`<tree>.members`), for hints_eval.rs.
pub fn write_all(store: &std::path::Path, names: &[String], out: &std::path::Path) -> anyhow::Result<()> {
    let trees = Trees::new(store.to_path_buf())?;
    std::fs::create_dir_all(out)?;
    let rt = tokio::runtime::Runtime::new()?;
    let names: Vec<String> = if names.is_empty() {
        let mut v: Vec<String> = std::fs::read_dir(store.join("trees"))?.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".tsv").map(String::from)).collect();
        v.sort();
        v
    } else {
        names.to_vec()
    };
    for (k, n) in names.iter().enumerate() {
        let t0 = std::time::Instant::now();
        let (table, ms) = rt.block_on(async { (build(&trees, n).await, members(&trees, n).await) });
        std::fs::write(out.join(format!("{n}.tsv")), &table)?;
        std::fs::write(out.join(format!("{n}.members")), &ms)?;
        eprintln!("{}/{} {n}: {} scripts, {} members, {:.1} s", k + 1, names.len(), table.lines().count().saturating_sub(1), ms.lines().count(), t0.elapsed().as_secs_f64());
    }
    Ok(())
}

/// Siglus' Scene.pck: each scene's names (its string table), the pack decrypted with the key
/// its game's Gameexe.dat and the pack itself give (siglus_key_recovery) when it needs one.
async fn siglus(trees: &Trees, tree: &str, path: &str, size: u64, files: &[(String, u64)]) -> String {
    if size > 512 << 20 {
        return String::new();
    }
    let dir = path.rsplit_once('/').map_or("", |d| d.0);
    let gameexe = if dir.is_empty() { "gameexe.dat".to_string() } else { format!("{dir}/gameexe.dat") };
    let mut cache = HashMap::new();
    let Some(scene) = trees.read(tree, path, 0, size, &mut cache).await else { return String::new() };
    let game = match files.iter().find(|f| f.0 == gameexe) {
        Some((p, n)) => trees.read(tree, p, 0, *n, &mut HashMap::new()).await.unwrap_or_default(),
        None => Vec::new(),
    };
    tokio::task::spawn_blocking(move || siglus_lines(scene, &game)).await.unwrap_or_default()
}

pub fn siglus_lines(scene: Vec<u8>, game: &[u8]) -> String {
    use siglus_assets::scene_pck::{ScenePck, ScenePckDecodeOptions, SceneStringCodec};
    let mut opt = ScenePckDecodeOptions { easy_angou_code: Some(siglus_assets::keys::SCENE_KEY.to_vec()), ..Default::default() };
    if !game.is_empty() && siglus_key_recovery::resources_require_exe_key(game, &scene).unwrap_or(false) {
        match siglus_key_recovery::recover_key_from_resources(game, &scene) {
            Ok(k) => opt.exe_angou_element = Some(k.to_vec()),
            Err(e) => {
                eprintln!("foresee: Scene.pck key: {e}");
                return String::new();
            }
        }
    }
    let pck = match ScenePck::load_and_rebuild_from_bytes(scene, &opt) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("foresee: Scene.pck: {e}");
            return String::new();
        }
    };
    let xor = pck.string_codec == SceneStringCodec::Xor;
    let mut out = String::new();
    let n = pck.header.scn_data_cnt.max(pck.header.scn_data_index_cnt).max(0) as usize;
    for no in 0..n {
        let (Some(name), Ok(data)) = (pck.find_scene_name(no), pck.scn_data_slice(no)) else { continue };
        let names = foresee::names_in_strings(&foresee::siglus_strings(&data, xor));
        if !names.is_empty() {
            out += &foresee::table_line(name, &names);
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
        let line = |name: &str, data: &[u8]| {
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
        // (a BGI .arc: a compiled script, known by its first bytes, and a picture)
        let script = b"BurikoCompiledScriptVer1.00\0\x10\0\0\0....bg02a\0ch_03b\0";
        let mut bgi = b"PackFile    ".to_vec();
        bgi.extend(2u32.to_le_bytes());
        for (name, off, size) in [("02_route", 0u32, script.len() as u32), ("bg02a", script.len() as u32, 7)] {
            let mut e = vec![0u8; 32];
            e[..name.len()].copy_from_slice(name.as_bytes());
            e[16..20].copy_from_slice(&off.to_le_bytes());
            e[20..24].copy_from_slice(&size.to_le_bytes());
            bgi.extend(e);
        }
        bgi.extend(script);
        bgi.extend([9; 7]);
        tsv += &line("data01000.arc", &bgi);
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
        assert!(t["02_route"].ends_with(&v(&["bg02a", "ch_03b"])), "{text}");
        assert_eq!(t.len(), 4, "{text}");
        let ms = members(&trees, "g").await;
        let ls: Vec<&str> = ms.lines().collect();
        assert_eq!(ls.len(), 5, "{ms}");
        assert!(ls[0].starts_with("data.xp3\tscenario/first.ks\t") && ls[3].starts_with("data01000.arc\t02_route\t80\t49"), "{ms}");
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
