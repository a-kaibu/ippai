//! Turns disk images into what the browser streams: a manifest plus
//! content-addressed, zstd-compressed chunks.
//!
//! ```text
//! store/
//!   chunks/ab/ab12...      zstd-compressed chunk, named by the BLAKE3 of its raw bytes
//!   disks/<name>/manifest.json
//! ```
//!
//! Identical chunks (across disks too) are stored once, all-zero chunks not at
//! all. The QEMU side is block/vnvm.c + web/qemu/vnvm-disk.js.
//!
//! `tree` packs a directory instead (the Wine install and prefix of the wasm runtime):
//! `trees/<name>.tsv` lists its files, each as chunks in the same store (see `tree`).
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
use std::process::Command;

#[derive(Parser)]
#[command(about)]
struct Cli {
    /// Output store (shared by all disks so chunks dedupe across them).
    #[arg(long, default_value = "store")]
    store: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Pack a raw disk image.
    Disk {
        image: PathBuf,
        name: String,
        /// Chunk size in KiB.
        #[arg(long, default_value_t = 256)]
        chunk_kib: u64,
        /// Virtual size reported to the guest (default: the image's size).
        #[arg(long)]
        size: Option<u64>,
    },
    /// Make a game disk: an uncompressed squashfs of DIR (the guest reads it
    /// without spending emulated cycles on decompression; the network transfer
    /// is compressed per chunk instead) plus .vnvm/launch.env.
    Game {
        dir: PathBuf,
        name: String,
        /// Executable to start, relative to DIR.
        #[arg(long)]
        exe: String,
        #[arg(long, default_value_t = 800)]
        width: u32,
        #[arg(long, default_value_t = 600)]
        height: u32,
        #[arg(long, default_value = "")]
        args: String,
        /// Every game disk has the same virtual size, so one VM snapshot fits all.
        #[arg(long, default_value_t = 8 << 30)]
        size: u64,
        #[arg(long, default_value_t = 256)]
        chunk_kib: u64,
    },
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
    /// An all-zero disk (the save disk; the guest formats it on first use).
    Blank {
        name: String,
        #[arg(long, default_value_t = 256 << 20)]
        size: u64,
        #[arg(long, default_value_t = 256)]
        chunk_kib: u64,
    },
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

#[derive(Serialize)]
struct Manifest {
    size: u64,
    chunk_size: u64,
    /// Relative to the manifest.
    chunks_url: String,
    /// BLAKE3 (hex, 32 chars) per chunk, "" for all zero.
    chunks: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Disk { image, name, chunk_kib, size } => {
            pack(&cli.store, &image, &name, chunk_kib << 10, size)?;
        }
        Cmd::Game { dir, name, exe, width, height, args, size, chunk_kib } => {
            if !dir.join(&exe).is_file() {
                bail!("{} not found in {}", exe, dir.display());
            }
            let tmp = cli.store.join("tmp");
            fs::create_dir_all(&tmp)?;
            let meta = tmp.join(format!("{name}-meta"));
            let _ = fs::remove_dir_all(&meta);
            fs::create_dir_all(meta.join(".vnvm"))?;
            fs::write(
                meta.join(".vnvm/launch.env"),
                format!(
                    "EXE={}\nWIDTH={width}\nHEIGHT={height}\nARGS={}\n",
                    shell_quote(&exe),
                    shell_quote(&args)
                ),
            )?;
            let img = tmp.join(format!("{name}.squashfs"));
            let _ = fs::remove_file(&img);
            // Pseudo files add .vnvm/ without copying the game folder.
            let env = meta.join(".vnvm/launch.env");
            let st = Command::new("mksquashfs")
                .arg(&dir)
                .arg(&img)
                .args(["-noI", "-noD", "-noF", "-noX", "-no-xattrs", "-all-root", "-quiet", "-no-progress"])
                .args(["-p", "/.vnvm d 755 0 0"])
                .arg("-p")
                .arg(format!("/.vnvm/launch.env f 644 0 0 cat {}", env.display()))
                .status()
                .context("mksquashfs (squashfs-tools)")?;
            if !st.success() {
                bail!("mksquashfs failed");
            }
            pack(&cli.store, &img, &name, chunk_kib << 10, Some(size))?;
            fs::remove_file(&img)?;
            fs::remove_dir_all(&meta)?;
        }
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
        Cmd::Blank { name, size, chunk_kib } => {
            let chunk = chunk_kib << 10;
            write_manifest(
                &cli.store,
                &name,
                &Manifest {
                    size,
                    chunk_size: chunk,
                    chunks_url: "../../chunks/".into(),
                    chunks: vec![String::new(); size.div_ceil(chunk) as usize],
                },
            )?;
        }
    }
    Ok(())
}

/// `launch.env` is sourced by the guest's vnvm-session, so values are single-quoted.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn pack(store: &Path, image: &Path, name: &str, chunk: u64, size: Option<u64>) -> Result<()> {
    let f = File::open(image).with_context(|| image.display().to_string())?;
    let len = f.metadata()?.len();
    let size = size.unwrap_or(len);
    if size < len {
        bail!("--size {size} is smaller than the image ({len})");
    }
    let n = size.div_ceil(chunk);
    let chunks_dir = store.join("chunks");
    fs::create_dir_all(&chunks_dir)?;

    let hashes: Vec<String> = (0..n)
        .into_par_iter()
        .map(|i| -> Result<String> {
            let off = i * chunk;
            if off >= len {
                return Ok(String::new());
            }
            let mut buf = vec![0u8; chunk.min(len - off) as usize];
            f.read_exact_at(&mut buf, off)?;
            buf.resize(chunk as usize, 0);
            if buf.iter().all(|&b| b == 0) {
                return Ok(String::new());
            }
            let h = blake3::hash(&buf).to_hex()[..32].to_string();
            let dir = chunks_dir.join(&h[..2]);
            write_chunk(&dir, &h, &buf)?;
            Ok(h)
        })
        .collect::<Result<_>>()?;

    let used = hashes.iter().filter(|h| !h.is_empty()).count();
    write_manifest(
        store,
        name,
        &Manifest { size, chunk_size: chunk, chunks_url: "../../chunks/".into(), chunks: hashes },
    )?;
    eprintln!(
        "{name}: {} virtual, {used} chunks of {} KiB with data ({} MiB raw)",
        human(size),
        chunk >> 10,
        (used as u64 * chunk) >> 20
    );
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

fn write_manifest(store: &Path, name: &str, m: &Manifest) -> Result<()> {
    let dir = store.join("disks").join(name);
    fs::create_dir_all(&dir)?;
    serde_json::to_writer(File::create(dir.join("manifest.json"))?, m)?;
    Ok(())
}

fn human(n: u64) -> String {
    if n >= 1 << 30 {
        format!("{:.1} GiB", n as f64 / (1u64 << 30) as f64)
    } else {
        format!("{} MiB", n >> 20)
    }
}
