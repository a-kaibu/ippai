//! Turns directory trees (the Wine root, the prefix, games) into what the browser streams:
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
//! `add-game` puts a packed game in the launcher's list (web/wine.html without `?run=`):
//! `games/<id>.json` (see `GameEntry`), served as one list by vnvm-server's `GET /games`.

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use rayon::prelude::*;
use serde::Serialize;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(about)]
struct Cli {
    /// Output store (shared by all trees so chunks dedupe across them).
    #[arg(long, default_value = "store")]
    store: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Pack a directory tree for the wasm runtime's file system (crates/vnhost/src/vfs.rs).
    Tree {
        dir: PathBuf,
        name: String,
        /// Store symlinks that point outside DIR as what they point to (files and directories),
        /// except links to "/".
        #[arg(long)]
        follow: bool,
        #[arg(long, default_value_t = 256)]
        chunk_kib: u64,
    },
    /// Add a game (its files packed with `tree`) to the launcher's list, or replace it.
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
    },
    /// Remove a game from the launcher's list (its trees and chunks stay).
    RemoveGame { id: String },
}

/// `games/<id>.json`: what the page needs to start a game (web/src/launcher.ts reads it).
#[derive(Serialize)]
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
    /// `/path=tree`, as wine.html's `&mount=`
    mounts: Vec<String>,
    env: Vec<String>,
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
) -> Result<()> {
    if !valid_id(&id) {
        bail!("id {id:?}: use letters, digits, _ and -");
    }
    // the exe must be in the tree (a typo would only show up as Wine's "file not found")
    let list = fs::read_to_string(store.join("trees").join(format!("{tree}.tsv")))
        .with_context(|| format!("tree {tree} (pack it with `tree` first)"))?;
    let exe = exe.replace('\\', "/");
    if !list.lines().any(|l| l.starts_with("f\t") && l.split('\t').nth(3).is_some_and(|p| p.eq_ignore_ascii_case(&exe))) {
        bail!("{exe} is not a file in tree {tree}");
    }
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
    let cover = match cover {
        Some(src) => {
            let ext = src.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
            if !["png", "jpg", "jpeg", "webp"].contains(&ext.as_str()) {
                bail!("cover {}: png, jpg or webp", src.display());
            }
            let name = format!("{id}.{ext}");
            fs::copy(&src, dir.join(&name)).with_context(|| src.display().to_string())?;
            Some(name)
        }
        None => None,
    };
    let entry = GameEntry {
        run: format!("Z:\\vnvm\\game\\{}", exe.replace('/', "\\")),
        mounts: [format!("/vnvm/game={tree}")].into_iter().chain(mount).collect(),
        id: id.clone(),
        title,
        engine,
        cover,
        args,
        env,
    };
    let tmp = dir.join(format!(".{id}.json.tmp"));
    fs::write(&tmp, serde_json::to_vec_pretty(&entry)?)?;
    fs::rename(&tmp, dir.join(format!("{id}.json")))?;
    eprintln!("{id}: {} ({})", entry.title, entry.run);
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Tree { dir, name, follow, chunk_kib } => tree(&cli.store, &dir, &name, follow, chunk_kib << 10)?,
        Cmd::AddGame { id, title, tree, exe, engine, cover, args, env, mount } => {
            add_game(&cli.store, id, title, tree, exe, engine, cover, args, env, mount)?
        }
        Cmd::RemoveGame { id } => {
            if !valid_id(&id) {
                bail!("id {id:?}: use letters, digits, _ and -");
            }
            let dir = cli.store.join("games");
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
/// an older, brotli-compressing vnvm-pack is rewritten).
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
fn tree(store: &Path, root: &Path, name: &str, follow: bool, chunk: u64) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let root = root.canonicalize()?;
    let mut lines = Vec::new();
    let mut files = Vec::new();
    // (path on disk, path in the tree)
    let mut todo = vec![(root.clone(), String::new())];
    while let Some((dir, rel)) = todo.pop() {
        let mut ents: Vec<_> = fs::read_dir(&dir)?.collect::<std::io::Result<_>>()?;
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let fname = e.file_name().to_string_lossy().into_owned();
            let path = if rel.is_empty() { fname.clone() } else { format!("{rel}/{fname}") };
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
                lines.push(format!("d\t{mode:o}\t{}\t{}", meta.mtime(), esc(&path)));
                todo.push((disk, path));
            } else if meta.is_file() {
                lines.push(String::new());
                files.push((lines.len() - 1, disk, path, mode, meta.mtime(), meta.len()));
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
    for (line, text) in done {
        lines[line] = text;
    }
    let dir = store.join("trees");
    fs::create_dir_all(&dir)?;
    let mut out = format!("vnvm-tree\t1\t{chunk}\n");
    for l in lines {
        out.push_str(&l);
        out.push('\n');
    }
    fs::write(dir.join(format!("{name}.tsv")), out)?;
    eprintln!("{name}: {} files, {} MiB", files.len(), bytes >> 20);
    Ok(())
}
