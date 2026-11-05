//! `ippai pack` turns directory trees (the prefix, games) into what the browser streams:
//! `trees/<name>.tsv` lists a tree's files, each as content-addressed, zstd-compressed chunks.
//!
//! ```text
//! store/
//!   chunks/ab/ab12...      zstd-compressed chunk, named by the BLAKE3 of its raw bytes
//!   trees/<name>.tsv
//! ```
//!
//! Identical chunks (across trees too) are stored once, all-zero chunks not at all.
//!
//! `ippai add` packs a game folder and puts it in the launcher's list (`add-game`: one packed already) (web/play.html without `?run=`):
//! `games/<id>.json` (see `GameEntry`), served as one list by ippai serve's `GET /games`.
//! It also looks the game up on ErogameScape (批評空間) / VNDB (see meta.rs) for the card's
//! brand, release date, score, tags, story and cover; `ippai meta` does that again or for older entries.

use crate::meta;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum Cmd {
    /// Pack a game folder into the store and add it to the launcher's list (or replace it).
    Add {
        /// The game folder.
        dir: PathBuf,
        /// Executable to start, relative to the folder.
        #[arg(long)]
        exe: String,
        /// Short name: letters, digits, `_`, `-` (the URL's `?game=`) [default: from the folder's name]
        #[arg(long)]
        id: Option<String>,
        /// Shown on the card [default: the folder's name]
        #[arg(long)]
        title: Option<String>,
        /// Engine name, shown on the card.
        #[arg(long)]
        engine: Option<String>,
        /// Cover image (png, jpg or webp), copied into the store.
        #[arg(long)]
        cover: Option<PathBuf>,
        /// Argument for the program (repeatable).
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Environment variable NAME=value (repeatable).
        #[arg(long)]
        env: Vec<String>,
        /// Another tree to mount, /path=tree (repeatable).
        #[arg(long)]
        mount: Vec<String>,
        /// Pin the metadata entry: egs:<n> (ErogameScape game id) or vndb:v<n>.
        #[arg(long = "meta-id")]
        meta_id: Option<String>,
        /// Do not look the game up (an entry being replaced keeps its metadata).
        #[arg(long)]
        no_meta: bool,
        /// Store symlinks that point outside DIR as what they point to.
        #[arg(long)]
        follow: bool,
    },
    /// Pack a directory tree for the wasm runtime's file system (crates/kura/src/vfs.rs).
    #[command(hide = true)]
    Pack {
        dir: PathBuf,
        name: String,
        /// Store symlinks that point outside DIR as what they point to (files and directories),
        /// except links to "/".
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value_t = 256)]
        chunk_kib: u64,
    },
    /// Add a game (its files packed with `ippai pack`) to the launcher's list, or replace it.
    #[command(hide = true)]
    AddGame {
        /// Short name: letters, digits, `_`, `-` (also the URL's `?game=`).
        id: String,
        /// Shown on the card.
        #[arg(long)]
        title: String,
        /// The tree with the game folder, mounted at Z:\vnvm\game.
        #[arg(long)]
        tree: String,
        /// Executable to start, relative to the game folder.
        #[arg(long)]
        exe: String,
        /// Engine name, shown on the card.
        #[arg(long)]
        engine: Option<String>,
        /// Cover image (png, jpg or webp), copied into the store.
        #[arg(long)]
        cover: Option<PathBuf>,
        /// Argument for the program (repeatable).
        #[arg(long = "arg")]
        args: Vec<String>,
        /// Environment variable NAME=value (repeatable).
        #[arg(long)]
        env: Vec<String>,
        /// Another tree to mount, /path=tree (repeatable).
        #[arg(long)]
        mount: Vec<String>,
        /// Pin the metadata entry: egs:<n> (ErogameScape game id) or vndb:v<n>.
        #[arg(long = "meta-id")]
        meta_id: Option<String>,
        /// Do not look the game up (an entry being replaced keeps its metadata).
        #[arg(long)]
        no_meta: bool,
    },
    /// Fetch the metadata (and a cover when the game has none) from ErogameScape / VNDB.
    Meta {
        /// Games to look up (default with --all: every game without metadata).
        ids: Vec<String>,
        /// Every game that has no metadata yet.
        #[arg(long)]
        all: bool,
        /// Pin the entry for the one game given: egs:<n> or vndb:v<n>.
        #[arg(long = "meta-id")]
        meta_id: Option<String>,
        /// Look up by this title instead of the game's.
        #[arg(long)]
        title: Option<String>,
        /// List the candidates for a keyword (to find an id for --meta-id); changes nothing.
        #[arg(long)]
        search: Option<String>,
    },
    /// Remove a game from the launcher's list (its trees and chunks stay).
    #[command(alias = "remove-game")]
    Remove { id: String },
}

/// `games/<id>.json`: what the page needs to start a game (web/src/launcher.ts reads it).
#[derive(Serialize, Deserialize)]
struct GameEntry {
    id: String,
    title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<String>,
    /// Image file next to the entry.
    #[serde(skip_serializing_if = "Option::is_none")]
    cover: Option<String>,
    /// Windows path of the program.
    run: String,
    args: Vec<String>,
    /// `/path=tree`, as play.html's `&mount=`
    mounts: Vec<String>,
    env: Vec<String>,
    /// "x86_64" for a 64-bit (PE32+) program: play.html runs it with the 64-bit build
    #[serde(skip_serializing_if = "Option::is_none")]
    arch: Option<String>,
    /// From ErogameScape / VNDB (meta.rs)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    meta: Option<meta::Info>,
}

fn game_path(store: &Path, id: &str) -> PathBuf {
    store.join("games").join(format!("{id}.json"))
}

fn read_game(store: &Path, id: &str) -> Result<GameEntry> {
    let p = game_path(store, id);
    serde_json::from_slice(&fs::read(&p).with_context(|| format!("game {id}"))?).with_context(|| p.display().to_string())
}

fn write_game(store: &Path, g: &GameEntry) -> Result<()> {
    let tmp = store.join("games").join(format!(".{}.json.tmp", g.id));
    fs::write(&tmp, serde_json::to_vec_pretty(g)?)?;
    fs::rename(&tmp, game_path(store, &g.id))?;
    Ok(())
}

/// Looks `g` up (by `title`, or the pinned `id`) and stores the result in it, with the cover
/// downloaded when the game has none. A failed lookup only warns.
fn fetch_meta(store: &Path, g: &mut GameEntry, title: Option<&str>, id: Option<&str>) {
    let title = title.unwrap_or(&g.title).to_string();
    let info = match meta::lookup(&title, id) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("{}: no metadata: {e:#}", g.id);
            return;
        }
    };
    eprintln!(
        "{}: {} / {} / {} ({}{})",
        g.id,
        info.title,
        info.brand,
        info.release,
        info.source,
        info.egs_id.map(|n| format!(" {n}")).or(info.vndb_id.as_ref().map(|v| format!(" {v}"))).unwrap_or_default()
    );
    if g.cover.is_none() && !info.cover_url.is_empty() {
        match meta::download_cover(&info.cover_url) {
            Ok((b, ext)) => {
                let name = format!("{}.{ext}", g.id);
                match fs::write(store.join("games").join(&name), b) {
                    Ok(()) => g.cover = Some(name),
                    Err(e) => eprintln!("{}: cover: {e}", g.id),
                }
            }
            Err(e) => eprintln!("{}: cover: {e:#}", g.id),
        }
    }
    g.meta = Some(info);
}

/// Whether the file's first chunk is a PE32+ (64-bit) image.
fn is_pe64(store: &Path, chunks: &str) -> Result<bool> {
    let Some(h) = chunks.split(',').next().filter(|h| h.len() >= 2) else { return Ok(false) };
    let raw = fs::read(store.join("chunks").join(&h[..2]).join(h))?;
    let b = zstd::decode_all(&raw[..])?;
    let u32_at = |o: usize| b.get(o..o + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap()));
    let Some(pe) = u32_at(0x3c).map(|v| v as usize) else { return Ok(false) };
    Ok(u32_at(pe) == Some(0x4550) && b.get(pe + 24..pe + 26) == Some(&[0x0b, 0x02]))
}

/// An id from a folder name: its ASCII letters and digits (`-` for the rest), or a hash of the
/// name when that leaves nothing (a Japanese name).
fn default_id(name: &str) -> String {
    let mut id = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            id.push(c.to_ascii_lowercase());
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    let id = id.trim_end_matches('-').to_string();
    if id.len() >= 2 {
        id
    } else {
        format!("game-{}", &blake3::hash(name.as_bytes()).to_hex()[..8])
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

#[allow(clippy::too_many_arguments)]
fn add_game(
    store: &Path,
    id: String,
    title: String,
    tree: String,
    exe: String,
    engine: Option<String>,
    cover: Option<PathBuf>,
    args: Vec<String>,
    env: Vec<String>,
    mount: Vec<String>,
    meta_id: Option<String>,
    no_meta: bool,
    new_list: Option<String>,
) -> Result<()> {
    if !valid_id(&id) {
        bail!("id {id:?}: use letters, digits, _ and -");
    }
    // the exe must be in the tree (a typo would only show up as "file not found"). With
    // `new_list` (ippai add) the tree is published only once all of this holds, so a failed
    // add leaves the game as it was
    let list = match &new_list {
        Some(l) => l.clone(),
        None => fs::read_to_string(store.join("trees").join(format!("{tree}.tsv")))
            .with_context(|| format!("tree {tree} (pack it with `tree` first)"))?,
    };
    let exe = exe.replace('\\', "/");
    let Some(line) = list.lines().find(|l| l.starts_with("f\t") && l.split('\t').nth(3).is_some_and(|p| p.eq_ignore_ascii_case(&exe)))
    else {
        bail!("{exe} is not a file in tree {tree}");
    };
    let arch = is_pe64(store, line.split('\t').nth(5).unwrap_or(""))
        .with_context(|| format!("reading {exe}"))?
        .then(|| "x86_64".to_string());
    for m in &mount {
        if !m.starts_with('/') || !m.contains('=') {
            bail!("--mount {m}: expected /path=tree");
        }
    }
    if let Some(e) = env.iter().find(|e| !e.contains('=')) {
        bail!("--env {e}: expected NAME=value");
    }
    let dir = store.join("games");
    fs::create_dir_all(&dir)?;
    // a replaced entry keeps its metadata and cover
    let old = read_game(store, &id).ok();
    let new_cover = match cover {
        Some(src) => {
            let ext = src.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
            if !["png", "jpg", "jpeg", "webp"].contains(&ext.as_str()) {
                bail!("cover {}: png, jpg or webp", src.display());
            }
            if !src.is_file() {
                bail!("cover {}: no such file", src.display());
            }
            Some((src, format!("{id}.{ext}")))
        }
        None => None,
    };
    let cover = match &new_cover {
        Some((_, name)) => Some(name.clone()),
        None => old.as_ref().and_then(|o| o.cover.clone()),
    };
    let mut entry = GameEntry {
        run: format!("Z:\\vnvm\\game\\{}", exe.replace('/', "\\")),
        mounts: [format!("/vnvm/game={tree}")].into_iter().chain(mount).collect(),
        id: id.clone(),
        title,
        engine,
        cover,
        args,
        env,
        arch,
        meta: old.and_then(|o| o.meta),
    };
    if !no_meta && (entry.meta.is_none() || meta_id.is_some()) {
        fetch_meta(store, &mut entry, None, meta_id.as_deref());
    }
    // all checked: publish
    if let Some(l) = &new_list {
        publish_tree(store, &tree, l)?;
    }
    if let Some((src, name)) = &new_cover {
        fs::copy(src, dir.join(name)).with_context(|| src.display().to_string())?;
    }
    write_game(store, &entry)?;
    eprintln!("{id}: {} ({}{})", entry.title, entry.run, if entry.arch.is_some() { ", 64-bit" } else { "" });
    Ok(())
}

pub fn run(store: &Path, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Pack { dir, name, follow, chunk_kib } => tree(store, &dir, &name, follow, chunk_kib << 10)?,
        Cmd::AddGame { id, title, tree, exe, engine, cover, args, env, mount, meta_id, no_meta } => {
            add_game(store, id, title, tree, exe, engine, cover, args, env, mount, meta_id, no_meta, None)?
        }
        Cmd::Meta { ids, all, meta_id, title, search } => {
            if let Some(k) = search {
                for c in meta::search(&k)? {
                    let id = if c.source == "vndb" { format!("vndb:{}", c.id) } else { format!("egs:{}", c.id) };
                    println!("{id}\t{}\t{}\t{}\t{} votes", c.title, c.brand, c.release, c.votes);
                }
                return Ok(());
            }
            let ids = if all && ids.is_empty() {
                let mut v: Vec<String> = fs::read_dir(store.join("games"))?
                    .filter_map(|e| e.ok()?.file_name().to_str()?.strip_suffix(".json").map(String::from))
                    .filter(|id| !id.starts_with('.'))
                    .collect();
                v.sort();
                v
            } else {
                ids
            };
            if ids.is_empty() {
                bail!("give game ids or --all");
            }
            if ids.len() > 1 && (meta_id.is_some() || title.is_some()) {
                bail!("--meta-id and --title are for one game");
            }
            for id in &ids {
                let mut g = read_game(store, id)?;
                if all && g.meta.is_some() {
                    continue;
                }
                fetch_meta(store, &mut g, title.as_deref(), meta_id.as_deref());
                write_game(store, &g)?;
            }
        }
        Cmd::Add { dir, exe, id, title, engine, cover, args, env, mount, meta_id, no_meta, follow } => {
            let name = dir.canonicalize().with_context(|| dir.display().to_string())?.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let id = id.unwrap_or_else(|| default_id(&name));
            if !valid_id(&id) {
                bail!("id {id:?}: use letters, digits, _ and - (--id)");
            }
            let list = pack_tree(store, &dir, &id, follow, 256 << 10)?;
            let tree_name = id.clone();
            add_game(store, id, title.unwrap_or(name), tree_name, exe, engine, cover, args, env, mount, meta_id, no_meta, Some(list))?
        }
        Cmd::Remove { id } => {
            if !valid_id(&id) {
                bail!("id {id:?}: use letters, digits, _ and -");
            }
            let dir = store.join("games");
            fs::remove_file(dir.join(format!("{id}.json"))).with_context(|| format!("game {id}"))?;
            for ext in ["png", "jpg", "jpeg", "webp"] {
                let _ = fs::remove_file(dir.join(format!("{id}.{ext}")));
            }
        }
    }
    Ok(())
}

/// Stores `buf` as a chunk; returns its name ("" for all zero).
fn put_chunk(store: &Path, buf: &[u8]) -> Result<String> {
    if buf.iter().all(|&b| b == 0) {
        return Ok(String::new());
    }
    let h = blake3::hash(buf).to_hex()[..32].to_string();
    write_chunk(&store.join("chunks").join(&h[..2]), &h, buf)?;
    Ok(h)
}

const ZSTD_MAGIC: [u8; 4] = [0x28, 0xb5, 0x2f, 0xfd];

/// Writes chunk `h` zstd-compressed into `dir`, unless it is there already (a chunk left by
/// an older, brotli-compressing ippai pack is rewritten).
fn write_chunk(dir: &Path, h: &str, buf: &[u8]) -> Result<()> {
    let path = dir.join(h);
    let mut magic = [0u8; 4];
    if File::open(&path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && magic == ZSTD_MAGIC {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{h}.{:?}.tmp", std::thread::current().id()));
    fs::write(&tmp, zstd::bulk::compress(buf, 19)?)?;
    fs::rename(&tmp, &path)?;
    Ok(())
}

/// Escapes a path or link target for the tsv (tab, newline and backslash).
fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\t', "\\t").replace('\n', "\\n")
}

/// `trees/<name>.tsv`:
///
/// ```text
/// vnvm-tree	1	<chunk size>
/// d	<mode>	<mtime>	<path>
/// f	<mode>	<mtime>	<path>	<size>	<chunk>,<chunk>,...     ("" chunk: all zero)
/// l	<mode>	<mtime>	<path>	<target>
/// ```
///
/// Paths are relative to the tree, parents before children; modes in octal. A file's last
/// chunk holds only what is left of it.
/// Whether ffmpeg is there to make the movies' VP9 copies (said once when it is not).
static MOVIES: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    let ok = crate::movie::available();
    if !ok {
        eprintln!("ffmpeg not found: Siglus movies (.omv) are left to the game's own decoder");
    }
    ok
});

/// Packs `root` as tree `name` and publishes its list.
fn tree(store: &Path, root: &Path, name: &str, follow: bool, chunk: u64) -> Result<()> {
    let list = pack_tree(store, root, name, follow, chunk)?;
    publish_tree(store, name, &list)
}

/// `trees/<name>.tsv` = `list`, replaced whole (a reader sees the old list or the new one).
fn publish_tree(store: &Path, name: &str, list: &str) -> Result<()> {
    let dir = store.join("trees");
    fs::create_dir_all(&dir)?;
    let tmp = dir.join(format!(".{name}.tsv.{}.tmp", std::process::id()));
    fs::write(&tmp, list)?;
    fs::rename(&tmp, dir.join(format!("{name}.tsv")))?;
    Ok(())
}

/// Stores the chunks of `root`'s files and returns the tree's list (not yet published: the
/// chunks are named by their content, so a list never published leaves nothing wrong behind).
fn pack_tree(store: &Path, root: &Path, name: &str, follow: bool, chunk: u64) -> Result<String> {
    use std::os::unix::fs::MetadataExt;
    let root = root.canonicalize()?;
    let mut lines = Vec::new();
    let mut files = Vec::new();
    // (path on disk, path in the tree)
    let mut todo = vec![(root.clone(), String::new())];
    // NW.js apps unpacked from their exe (see nwjs.rs), walked as if in the exe's folder; the
    // first of two same paths wins
    let unpacked = store.join("tmp").join(format!("nwjs-{name}-{}", std::process::id()));
    let mut apps = 0;
    let mut seen = std::collections::HashSet::new();
    while let Some((dir, rel)) = todo.pop() {
        let mut ents: Vec<_> = fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
        ents.sort_by_key(|e| e.file_name());
        let has_package = ents.iter().any(|e| e.file_name().eq_ignore_ascii_case("package.json"));
        for e in ents {
            let fname = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { fname.clone() } else { format!("{rel}/{fname}") };
            let first = seen.insert(path.clone());
            let lower = fname.to_ascii_lowercase();
            if first && !has_package && !dir.starts_with(&unpacked) && (lower.ends_with(".exe") || lower.ends_with(".nw")) {
                let to = unpacked.join(apps.to_string());
                if e.metadata()?.is_file() && crate::nwjs::unpack(&e.path(), &to)? {
                    eprintln!("{path}: NW.js app unpacked next to it");
                    apps += 1;
                    todo.push((to, rel.clone()));
                }
            }
            let mut disk = e.path();
            let mut meta = fs::symlink_metadata(&disk)?;
            if meta.file_type().is_symlink() {
                let target = fs::read_link(&disk)?;
                let resolved = fs::canonicalize(&disk).ok();
                let inside = resolved.as_ref().is_some_and(|r| r.starts_with(&root));
                if !follow || inside || target == Path::new("/") || resolved.is_none() {
                    lines.push(format!("l\t{:o}\t{}\t{}\t{}", 0o777, meta.mtime(), esc(&path), esc(&target.to_string_lossy())));
                    continue;
                }
                disk = resolved.unwrap();
                meta = fs::metadata(&disk)?;
            }
            let mode = meta.mode() & 0o7777;
            if meta.is_dir() {
                if first {
                    lines.push(format!("d\t{mode:o}\t{}\t{}", meta.mtime(), esc(&path)));
                }
                todo.push((disk, path));
            } else if !first {
                continue;
            } else if meta.is_file() {
                lines.push(String::new());
                files.push((lines.len() - 1, disk.clone(), path.clone(), mode, meta.mtime(), meta.len()));
                // Siglus movies: a VP9 copy the browser decodes in hardware (see movie.rs)
                if fname.to_ascii_lowercase().ends_with(".omv") && *MOVIES {
                    match crate::movie::vnv_for(store, &disk) {
                        Ok(v) => {
                            let len = fs::metadata(&v)?.len();
                            lines.push(String::new());
                            files.push((lines.len() - 1, v, format!("{path}.vnv"), mode, meta.mtime(), len));
                        }
                        Err(e) => eprintln!("{path}: no VP9 copy: {e:#}"),
                    }
                }
            }
        }
    }
    let done: Vec<(usize, String)> = files
        .par_iter()
        .map(|(line, disk, path, mode, mtime, size)| -> Result<(usize, String)> {
            let f = File::open(disk).with_context(|| disk.display().to_string())?;
            let mut hashes = Vec::new();
            let mut off = 0;
            while off < *size {
                let mut buf = vec![0u8; chunk.min(size - off) as usize];
                f.read_exact_at(&mut buf, off)?;
                hashes.push(put_chunk(store, &buf)?);
                off += chunk;
            }
            Ok((*line, format!("f\t{mode:o}\t{mtime}\t{}\t{size}\t{}", esc(path), hashes.join(","))))
        })
        .collect::<Result<_>>()?;
    let bytes: u64 = files.iter().map(|f| f.5).sum();
    if apps > 0 {
        fs::remove_dir_all(&unpacked)?;
        let _ = fs::remove_dir(store.join("tmp"));
    }
    for (line, text) in done {
        lines[line] = text;
    }
    let mut out = format!("vnvm-tree\t1\t{chunk}\n");
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    eprintln!("{name}: {} files, {} MiB", files.len(), bytes >> 20);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add(store: &Path, dir: &Path, exe: &str) -> Result<()> {
        run(
            store,
            Cmd::Add {
                dir: dir.to_path_buf(),
                exe: exe.into(),
                id: Some("sample".into()),
                title: None,
                engine: None,
                cover: None,
                args: vec![],
                env: vec![],
                mount: vec![],
                meta_id: None,
                no_meta: true,
                follow: false,
            },
        )
    }

    #[test]
    fn failed_add_leaves_the_game_as_it_was() {
        let t = std::env::temp_dir().join(format!("ippai-add-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let (store, game) = (t.join("store"), t.join("game"));
        fs::create_dir_all(&game).unwrap();
        fs::write(game.join("game.exe"), b"MZ old").unwrap();
        add(&store, &game, "game.exe").unwrap();
        let tsv = store.join("trees/sample.tsv");
        let (list, entry) = (fs::read(&tsv).unwrap(), fs::read(store.join("games/sample.json")).unwrap());

        // the folder changed, and --exe names a file that is not there
        fs::write(game.join("game.exe"), b"MZ new, longer").unwrap();
        fs::write(game.join("extra.dat"), b"x").unwrap();
        assert!(add(&store, &game, "missing.exe").is_err());
        assert_eq!(fs::read(&tsv).unwrap(), list, "the old tree is still published");
        assert_eq!(fs::read(store.join("games/sample.json")).unwrap(), entry);
        assert!(fs::read_dir(store.join("trees")).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));

        // a good add then replaces both
        add(&store, &game, "game.exe").unwrap();
        assert!(fs::read_to_string(&tsv).unwrap().contains("extra.dat"));
        let _ = fs::remove_dir_all(&t);
    }
}
