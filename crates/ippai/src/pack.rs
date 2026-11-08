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
use crate::unpack;

use anyhow::{bail, Context, Result};
use clap::Subcommand;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Subcommand)]
pub enum Cmd {
    /// Pack a game folder into the store and add it to the launcher's list (or replace it).
    Add {
        /// The game folder, or its download: a zip, lzh, rar or 7z archive, or an installer
        /// (InstallShield, Inno Setup, Windows Installer, self-extracting archives), unpacked
        /// here, installers inside it too.
        dir: PathBuf,
        /// Executable to start, relative to the folder (also found one folder further down)
        /// [default: the biggest .exe that is not a tool or installer]
        #[arg(long)]
        exe: Option<String>,
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
        /// Average chunk size (KiB; a power of two): files are cut where their content says
        /// (FastCDC, cdc.rs; chunks avg/4 to avg*4). Big for an S3 store with a request rate
        /// limit (Hugging Face Buckets answer SlowDown at a few PUTs a second); a file smaller
        /// than a chunk is one chunk of its own size either way.
        #[arg(long, default_value_t = DEFAULT_CHUNK_KIB)]
        chunk_kib: u64,
    },
    /// Unpack an archive or installer as `add` does (prints the exe it would start).
    #[command(hide = true)]
    Unpack { src: PathBuf, out: PathBuf },
    /// Pack a directory tree for the wasm runtime's file system (crates/kura/src/vfs.rs).
    #[command(hide = true)]
    Pack {
        dir: PathBuf,
        name: String,
        /// Store symlinks that point outside DIR as what they point to (files and directories),
        /// except links to "/".
        #[arg(long)]
        follow: bool,
        /// Average chunk size (KiB), as `add`.
        #[arg(long, default_value_t = DEFAULT_CHUNK_KIB)]
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
    /// Remove a game: its entry and cover, the trees only it mounts and the chunks no tree left
    /// lists. With IPPAI_STORAGE_CONFIG they leave the S3 store too (retired: `ippai storage gc`
    /// deletes the objects; its saves stay).
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
    /// From ErogameScape / VNDB (meta.rs)
    #[serde(skip_serializing_if = "Option::is_none", default)]
    meta: Option<meta::Info>,
    /// What did not unpack of the download the game was added from (`ippai add` of an archive
    /// a tool failed on in part, unpack.rs); the list shows the game as partly unpacked.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    unpack_issues: Vec<String>,
}

fn game_path(store: &Path, id: &str) -> PathBuf {
    store.join("games").join(format!("{id}.json"))
}

fn read_game(store: &Path, id: &str) -> Result<GameEntry> {
    let p = game_path(store, id);
    serde_json::from_slice(&fs::read(&p).with_context(|| format!("game {id}"))?).with_context(|| p.display().to_string())
}

/// The trees a game mounts (`/path=tree`).
fn game_trees(g: &GameEntry) -> impl Iterator<Item = &str> {
    g.mounts.iter().filter_map(|m| m.split_once('=').map(|(_, t)| t)).filter(|t| !t.is_empty())
}

/// The chunks `trees/<name>.tsv` lists (none for a tree that is not there).
fn tree_chunks(store: &Path, name: &str) -> Result<std::collections::HashSet<String>> {
    let text = match fs::read_to_string(store.join("trees").join(format!("{name}.tsv"))) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Default::default()),
        Err(e) => return Err(e.into()),
    };
    Ok(text
        .lines()
        .filter(|l| l.starts_with("f\t"))
        .filter_map(|l| l.split('\t').nth(5))
        .flat_map(|c| c.split(','))
        .map(|c| c.split_once(':').map_or(c, |(h, _)| h))
        .filter(|c| c.len() > 2)
        .map(String::from)
        .collect())
}

/// What removing game `id` takes away, as paths in the store (the names the S3 store knows them
/// by): its entry and cover; the trees it mounts that no other game mounts, never sake's C:\
/// ([`PREFIX_TREE`]); and their chunks that no tree left lists. Saves are not touched.
fn removal(store: &Path, id: &str) -> Result<Vec<String>> {
    if !valid_id(id) {
        bail!("id {id:?}: use letters, digits, _ and -");
    }
    let game = read_game(store, id)?;
    let mut others = std::collections::HashSet::new();
    for e in fs::read_dir(store.join("games"))? {
        let name = e?.file_name();
        let Some(other) = name.to_str().and_then(|n| n.strip_suffix(".json")) else { continue };
        if other == id || other.starts_with('.') {
            continue;
        }
        let g = read_game(store, other)?;
        others.extend(game_trees(&g).map(String::from));
    }
    let trees: Vec<&str> = game_trees(&game).filter(|t| *t != PREFIX_TREE && !others.contains(*t)).collect();
    let mut out = vec![format!("games/{id}.json")];
    for ext in ["png", "jpg", "jpeg", "webp"] {
        if store.join(format!("games/{id}.{ext}")).exists() {
            out.push(format!("games/{id}.{ext}"));
        }
    }
    // (every tree in the store that stays: a game's, sake's C:\, one packed by hand)
    let mut kept = std::collections::HashSet::new();
    if let Ok(dir) = fs::read_dir(store.join("trees")) {
        for e in dir {
            let name = e?.file_name();
            let Some(t) = name.to_str().and_then(|n| n.strip_suffix(".tsv")) else { continue };
            if !t.starts_with('.') && !trees.contains(&t) {
                kept.extend(tree_chunks(store, t)?);
            }
        }
    }
    let mut chunks = std::collections::BTreeSet::new();
    for t in &trees {
        out.push(format!("trees/{t}.tsv"));
        chunks.extend(tree_chunks(store, t)?.into_iter().filter(|c| !kept.contains(c)));
    }
    out.extend(chunks.into_iter().map(|c| format!("chunks/{}/{c}", &c[..2])));
    Ok(out)
}

/// Removes game `id` ([`removal`]): from `storage` first when there is one (the catalog
/// fetched first, so that the trees only the S3 store had count as kept; the objects are
/// retired, `Storage::gc` deletes them), then the files here. Gives what it removed.
pub async fn remove_game(store: &Path, id: &str, storage: Option<&crate::storage::Storage>) -> Result<Vec<String>> {
    if let Some(s) = storage {
        s.hydrate_catalog(store).await?;
    }
    let gone = removal(store, id)?;
    if let Some(s) = storage {
        s.forget_all(&gone)?;
    }
    // (the entry first: a removal cut short leaves no game that names a missing tree)
    for l in &gone {
        match fs::remove_file(store.join(l)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    Ok(gone)
}

/// `ippai add <dir> --exe game.exe --id <id> [--mount ...]` without looking anything up (tests).
#[cfg(test)]
pub(crate) fn add_for_test(store: &Path, dir: &Path, id: &str, mount: Vec<String>) -> Result<()> {
    run(
        store,
        Cmd::Add {
            dir: dir.to_path_buf(),
            exe: Some("game.exe".into()),
            id: Some(id.into()),
            title: None,
            engine: None,
            cover: None,
            args: vec![],
            env: vec![],
            mount,
            meta_id: None,
            no_meta: true,
            follow: false,
            chunk_kib: 64,
        },
    )
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
    unpack_issues: Vec<String>,
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
        meta: old.and_then(|o| o.meta),
        unpack_issues,
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
    eprintln!("{id}: {} ({})", entry.title, entry.run);
    Ok(())
}

pub fn run(store: &Path, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Pack { dir, name, follow, chunk_kib } => tree(store, &dir, &name, follow, chunking(chunk_kib))?,
        Cmd::AddGame { id, title, tree, exe, engine, cover, args, env, mount, meta_id, no_meta } => {
            add_game(store, id, title, tree, exe, engine, cover, args, env, mount, meta_id, no_meta, None, vec![])?
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
        Cmd::Unpack { src, out } => {
            if out.exists() && fs::read_dir(&out)?.next().is_some() {
                bail!("{}: not empty", out.display());
            }
            let issues = unpack::unpack(&src, &out)?;
            if !issues.is_empty() {
                eprintln!("{}: unpacked in part ({} issues)", src.display(), issues.len());
            }
            println!("{}", unpack::pick_exe(&out, None).unwrap_or_default());
        }
        Cmd::Add { dir, exe, id, title, engine, cover, args, env, mount, meta_id, no_meta, follow, chunk_kib } => {
            let path = dir.canonicalize().with_context(|| dir.display().to_string())?;
            let package = unpack::is_package(&path);
            let name = if package { path.file_stem() } else { path.file_name() }.map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let id = id.unwrap_or_else(|| default_id(&name));
            if !valid_id(&id) {
                bail!("id {id:?}: use letters, digits, _ and - (--id)");
            }
            // a download: unpacked into the store's tmp, packed from there, then removed
            let staged = store.join("tmp").join(format!("unpack-{id}-{}", std::process::id()));
            let r = (|| {
                let mut root = path.clone();
                let mut issues = vec![];
                if package {
                    let _ = fs::remove_dir_all(&staged);
                    issues = unpack::unpack(&path, &staged)?;
                    if !issues.is_empty() {
                        eprintln!("{}: unpacked in part, added all the same ({} issues, kept in the game's entry)", dir.display(), issues.len());
                    }
                    root = staged.clone();
                    // (the usual one folder holding the game: its files at the tree's top)
                    let top: Vec<_> = fs::read_dir(&root)?.collect::<Result<_, _>>()?;
                    if let [one] = &top[..] {
                        if one.file_type()?.is_dir() {
                            root = one.path();
                        }
                    }
                } else if !path.is_dir() {
                    bail!("{}: not a folder, archive or installer", dir.display());
                }
                let exe = match &exe {
                    Some(e) if root.join(e.replace('\\', "/")).is_file() => e.clone(),
                    Some(e) => unpack::pick_exe(&root, Some(e)).with_context(|| format!("{e} is not in {}", dir.display()))?,
                    None => unpack::pick_exe(&root, None).with_context(|| format!("no program in {} (--exe)", dir.display()))?,
                };
                let list = pack_tree(store, &root, &id, follow, chunking(chunk_kib))?;
                add_game(store, id.clone(), title.unwrap_or(name), id.clone(), exe, engine, cover, args, env, mount, meta_id, no_meta, Some(list), issues)
            })();
            if package {
                let _ = fs::remove_dir_all(&staged);
                let _ = fs::remove_dir(store.join("tmp"));
            }
            r?
        }
        Cmd::Remove { id } => {
            let storage = crate::storage::Storage::configured(crate::library::Library::new(store)?)?;
            let gone = tokio::runtime::Runtime::new()?.block_on(remove_game(store, &id, storage.as_ref()))?;
            let chunks = gone.iter().filter(|l| l.starts_with("chunks/")).count();
            eprintln!("{id}: removed ({} catalog files, {chunks} chunks no other game uses)", gone.len() - chunks);
            if storage.is_some() {
                eprintln!("  dropped from the S3 store too; `ippai storage gc` deletes the objects once retired long enough");
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

/// The chunks' zstd level: 19 (IPPAI_ZSTD_LEVEL to change). Measured on prefix32 (311 MiB of
/// DLLs and fonts in 256 KiB chunks): level 3 is 99.2 MiB at 1 s of CPU, 6 96.9 MiB 2.8 s,
/// 12 93.9 MiB 16 s, 19 85.8 MiB 80 s. A pack is once and every play downloads the chunks
/// (decoding is about as fast at every level), so 19.
fn zstd_level() -> i32 {
    static L: std::sync::OnceLock<i32> = std::sync::OnceLock::new();
    *L.get_or_init(|| std::env::var("IPPAI_ZSTD_LEVEL").ok().and_then(|v| v.parse().ok()).unwrap_or(19))
}

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
    fs::write(&tmp, zstd::bulk::compress(buf, zstd_level())?)?;
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
/// vnvm-tree	2	<average chunk size>
/// d	<mode>	<mtime>	<path>
/// f	<mode>	<mtime>	<path>	<size>	<hash>:<length>,...     (":<length>": all zero)
/// l	<mode>	<mtime>	<path>	<target>
/// ```
///
/// Paths are relative to the tree, parents before children; modes in octal. Files are cut where
/// their content says (cdc.rs); the chunks' lengths add up to the file's size.
/// Whether ffmpeg is there to make the movies' VP9 copies (said once when it is not).
static MOVIES: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    let ok = crate::movie::available();
    if !ok {
        eprintln!("ffmpeg not found: no movies made playable (Siglus's .omv are left to the game's own decoder, DirectShow movies end at once)");
    }
    ok
});

/// The tree every game mounts as sake's C:\ (web/src/play.ts: `/vnvm/prefix=prefix32`).
pub const PREFIX_TREE: &str = "prefix32";

/// Makes the tree [`PREFIX_TREE`] from the fonts in `fonts` (a release's fonts/, put in place by
/// install.sh; tools/ci/sake-fonts.py) when the store has none: C:\windows\Fonts with those
/// fonts and an empty C:\windows\temp, which is all of sake's C:\. True when it made one; a
/// store that has the tree (made by hand with `ippai pack`, or earlier) keeps it.
pub fn ensure_prefix(store: &Path, fonts: &Path) -> Result<bool> {
    if store.join("trees").join(format!("{PREFIX_TREE}.tsv")).exists() {
        return Ok(false);
    }
    let Ok(entries) = fs::read_dir(fonts) else { return Ok(false) };
    let files: Vec<_> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.path())
        .collect();
    let font = |p: &PathBuf| p.extension().and_then(|e| e.to_str()).is_some_and(|e| ["ttf", "ttc", "otf"].contains(&e.to_ascii_lowercase().as_str()));
    if !files.iter().any(font) {
        return Ok(false);
    }
    fs::create_dir_all(store)?;
    let root = store.join(format!(".{PREFIX_TREE}.{}.tmp", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let made = (|| {
        let dir = root.join("drive_c/windows/Fonts");
        fs::create_dir_all(&dir)?;
        fs::create_dir_all(root.join("drive_c/windows/temp"))?;
        // (the license goes along with the fonts)
        for f in &files {
            fs::copy(f, dir.join(f.file_name().unwrap()))?;
        }
        tree(store, &root, PREFIX_TREE, false, chunking(DEFAULT_CHUNK_KIB))
    })();
    let _ = fs::remove_dir_all(&root);
    made.map(|()| true)
}

/// Reads `buf.len()` bytes at `off` (pread; on Windows seek_read, which takes the offset too).
#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}
#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        match f.seek_read(buf, off)? {
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => {
                buf = &mut buf[n..];
                off += n as u64;
            }
        }
    }
    Ok(())
}

/// A file's mode for the tree's list (Windows has none: 755 for folders, 644 or 444 read-only).
fn mode(m: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    return std::os::unix::fs::MetadataExt::mode(m);
    #[cfg(not(unix))]
    return if m.is_dir() { 0o755 } else if m.permissions().readonly() { 0o444 } else { 0o644 };
}

/// A file's modification time for the tree's list (seconds since 1970).
fn mtime(m: &fs::Metadata) -> i64 {
    #[cfg(unix)]
    return std::os::unix::fs::MetadataExt::mtime(m);
    #[cfg(not(unix))]
    return m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
}

/// Packs `root` as tree `name` and publishes its list.
fn tree(store: &Path, root: &Path, name: &str, follow: bool, chunking: Params) -> Result<()> {
    let list = pack_tree(store, root, name, follow, chunking)?;
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

/// The average chunk size of `ippai add` and `pack` (KiB). Measured with LOOPERS on Hugging
/// Face Buckets: a push of 1.9 GB took 1723 s in 256 KiB chunks, 358 s in 1 MiB, 105 s in 4 MiB,
/// 69 s in 16 MiB (the PUT rate limit); the game started as fast with any of 1 MiB and more.
/// Played from the device (docs/chunk-size.md, tests/chunkbench, Android-like): scattered
/// 64 KiB reads were 3x faster in 4 MiB chunks than in 16 MiB (p95 53 vs 187 ms) with the least
/// memory; 2 MiB was as fast with twice the chunks, 8 MiB ~1.5x slower. So 4 MiB (user
/// 2026-11-08), uploads ~1.5x longer.
pub const DEFAULT_CHUNK_KIB: u64 = 4096;

use crate::cdc::Params;

/// The chunking of an average size in KiB (a power of two, 64 KiB to 64 MiB).
fn chunking(kib: u64) -> Params {
    Params::with_avg(kib.clamp(64, 1 << 16) << 10)
}

/// Stores the chunks of `root`'s files and returns the tree's list (not yet published: the
/// chunks are named by their content, so a list never published leaves nothing wrong behind).
fn pack_tree(store: &Path, root: &Path, name: &str, follow: bool, chunking: Params) -> Result<String> {
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
                    lines.push(format!("l\t{:o}\t{}\t{}\t{}", 0o777, mtime(&meta), esc(&path), esc(&target.to_string_lossy())));
                    continue;
                }
                disk = resolved.unwrap();
                meta = fs::metadata(&disk)?;
            }
            let mode = mode(&meta) & 0o7777;
            if meta.is_dir() {
                if first {
                    lines.push(format!("d\t{mode:o}\t{}\t{}", mtime(&meta), esc(&path)));
                }
                todo.push((disk, path));
            } else if !first {
                continue;
            } else if meta.is_file() {
                lines.push(String::new());
                files.push((lines.len() - 1, disk.clone(), path.clone(), mode, mtime(&meta), meta.len()));
                // Siglus movies: a VP9 copy the browser decodes in hardware (see movie.rs)
                if fname.to_ascii_lowercase().ends_with(".omv") && *MOVIES {
                    match crate::movie::vnv_for(store, &disk) {
                        Ok(v) => {
                            let len = fs::metadata(&v)?.len();
                            lines.push(String::new());
                            files.push((lines.len() - 1, v, format!("{path}.vnv"), mode, mtime(&meta), len));
                        }
                        Err(e) => eprintln!("{path}: no VP9 copy: {e:#}"),
                    }
                }
                // other movies (DirectShow, Media Foundation): VP9 and Vorbis (see movie.rs)
                // (by name, or by its first bytes when it is not an .omv)
                let by_name = crate::movie::is_movie(&fname);
                let sniffed = || {
                    let mut head = [0u8; 40];
                    let n = File::open(&disk).and_then(|mut f| std::io::Read::read(&mut f, &mut head)).unwrap_or(0);
                    crate::movie::sniff(&head[..n])
                };
                if *MOVIES && !fname.to_ascii_lowercase().ends_with(".omv") && (by_name || sniffed()) {
                    match crate::movie::movie_vnv_for(store, &disk) {
                        Ok(v) => {
                            let len = fs::metadata(&v)?.len();
                            lines.push(String::new());
                            files.push((lines.len() - 1, v, format!("{path}.vnv"), mode, mtime(&meta), len));
                        }
                        Err(e) => eprintln!("{path}: not made playable: {e:#}"),
                    }
                }
            }
        }
    }
    let done: Vec<(usize, String)> = files
        .par_iter()
        .map(|(line, disk, path, mode, mtime, size)| -> Result<(usize, String)> {
            let f = File::open(disk).with_context(|| disk.display().to_string())?;
            // (a file's chunks in parallel too: a game that is one big archive packs on every
            // core; rayon runs a task per thread at a time, so the buffers in memory are about
            // one chunk per core)
            // (offset, length) of each chunk: the cuts take a pass over the file first (gear
            // hashing is far faster than the compression after it)
            let mut at = 0;
            let spans: Vec<(u64, u64)> = crate::cdc::lengths(&mut std::io::BufReader::new(&f), chunking)?
                .into_iter()
                .map(|n| {
                    at += n;
                    (at - n, n)
                })
                .collect();
            let hashes: Vec<String> = spans
                .into_par_iter()
                .map(|(off, n)| {
                    let mut buf = vec![0u8; n as usize];
                    read_exact_at(&f, &mut buf, off)?;
                    Ok(format!("{}:{n}", put_chunk(store, &buf)?))
                })
                .collect::<Result<_>>()?;
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
    let mut out = format!("vnvm-tree\t2\t{}\n", chunking.avg);
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

    /// testdata/blake3.tsv: chunk names (the first 16 bytes of BLAKE3) from the blake3 crate,
    /// for the page's own BLAKE3 (web/src/blake3.ts, host/fetch-bridge.js) to be checked
    /// against: `p` inputs are `i % 251`, `r` ones bytes of an LCG seeded with the length
    /// (x = x * 1103515245 + 12345, byte = x >> 16 & 255). IPPAI_REGEN=1 writes it again.
    #[test]
    fn blake3_vectors_for_the_page() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata/blake3.tsv");
        let mut want = String::from("kind\tlength\tname\n");
        let lens = [0usize, 1, 63, 64, 65, 127, 128, 1023, 1024, 1025, 2047, 2048, 2049, 3072, 3073, 4096, 4097, 5120, 8191, 8192, 8193, 16384, 31744, 65536, 102400, 200000, 262143, 262144];
        let more = [7usize, 333, 1500, 7777, 12345, 33000, 99999, 131071, 131072, 131073, 180001, 250000];
        for (kind, lens) in [("p", &lens[..]), ("r", &[&lens[..], &more[..]].concat()[..])] {
            for &n in lens {
                let mut x = n as u32;
                let data: Vec<u8> = (0..n)
                    .map(|i| {
                        if kind == "p" {
                            return (i % 251) as u8;
                        }
                        x = x.wrapping_mul(1103515245).wrapping_add(12345);
                        (x >> 16) as u8
                    })
                    .collect();
                want.push_str(&format!("{kind}\t{n}\t{}\n", &blake3::hash(&data).to_hex()[..32]));
            }
        }
        if std::env::var_os("IPPAI_REGEN").is_some() {
            std::fs::write(&path, &want).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), want, "IPPAI_REGEN=1 to write it");
    }

    #[test]
    fn ids_from_names() {
        assert_eq!(default_id("My Game_2 (Trial)"), "my-game_2-trial");
        assert_eq!(default_id("--AB--cd--"), "ab-cd");
        assert_eq!(default_id("ab"), "ab");
        let h = default_id("体験版");
        assert!(h.starts_with("game-") && h.len() == 13 && h != default_id("製品版"), "{h}");
        assert!(default_id("a").starts_with("game-"));
        for ok in ["a", "a-b_c9", "Z"] {
            assert!(valid_id(ok), "{ok}");
        }
        for bad in ["", "a b", "a/b", "é", "a.b"] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    fn add(store: &Path, dir: &Path, exe: &str) -> Result<()> {
        run(
            store,
            Cmd::Add {
                dir: dir.to_path_buf(),
                exe: Some(exe.into()),
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
                chunk_kib: 64,
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

    /// Two games with a file in common, sake's C:\, and a third game that mounts the second's
    /// tree: removing the first takes its entry, cover, tree and the chunks only it had, keeps
    /// the shared chunks and everything of the others; removing the second keeps its tree (the
    /// third mounts it); nothing left names a missing tree or chunk.
    #[test]
    fn remove_takes_only_what_nothing_else_uses() {
        let t = std::env::temp_dir().join(format!("ippai-remove-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let store = t.join("store");
        let shared: Vec<u8> = (0..600_000u32).map(|i| (i * 13 % 251) as u8).collect();
        for (name, own) in [("a", 1u8), ("b", 2), ("c", 3)] {
            let d = t.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("game.exe"), [b'M', b'Z', own]).unwrap();
            fs::write(d.join("own.dat"), vec![own; 300_000]).unwrap();
            fs::write(d.join("shared.dat"), &shared).unwrap();
        }
        add_for_test(&store, &t.join("a"), "a", vec![]).unwrap();
        add_for_test(&store, &t.join("b"), "b", vec![]).unwrap();
        add_for_test(&store, &t.join("c"), "c", vec!["/extra=b".into()]).unwrap();
        fs::write(store.join("games/a.jpg"), b"cover").unwrap();
        let fonts = t.join("fonts");
        fs::create_dir_all(&fonts).unwrap();
        fs::write(fonts.join("msgothic.ttf"), vec![9u8; 1000]).unwrap();
        ensure_prefix(&store, &fonts).unwrap();
        let chunks = |tree: &str| tree_chunks(&store, tree).unwrap();
        let (a, b, c, prefix) = (chunks("a"), chunks("b"), chunks("c"), chunks(PREFIX_TREE));
        let only_a: std::collections::HashSet<_> = a.iter().filter(|x| !b.contains(*x) && !c.contains(*x)).cloned().collect();
        assert!(!only_a.is_empty() && only_a.len() < a.len(), "a has chunks of its own and shared ones");
        let on_disk = |x: &String| store.join("chunks").join(&x[..2]).join(x).exists();

        let gone = remove_game_blocking(&store, "a").unwrap();
        assert!(gone.contains(&"games/a.json".to_string()) && gone.contains(&"games/a.jpg".to_string()) && gone.contains(&"trees/a.tsv".to_string()), "{gone:?}");
        assert_eq!(gone.iter().filter(|l| l.starts_with("chunks/")).count(), only_a.len());
        assert!(!store.join("games/a.json").exists() && !store.join("games/a.jpg").exists() && !store.join("trees/a.tsv").exists());
        assert!(only_a.iter().all(|x| !on_disk(x)), "a's own chunks gone");
        for x in b.iter().chain(&c).chain(&prefix) {
            assert!(on_disk(x), "{x} kept");
        }

        let gone = remove_game_blocking(&store, "b").unwrap();
        assert_eq!(gone, vec!["games/b.json".to_string()], "c still mounts b's tree");
        assert!(store.join("trees/b.tsv").exists() && b.iter().all(on_disk));
        assert!(remove_game_blocking(&store, "b").is_err(), "no such game any more");
        assert!(remove_game_blocking(&store, "../c").is_err());
        let _ = fs::remove_dir_all(&t);
    }

    fn remove_game_blocking(store: &Path, id: &str) -> Result<Vec<String>> {
        tokio::runtime::Runtime::new().unwrap().block_on(remove_game(store, id, None))
    }

    /// `add` cuts files where their content says (`<hash>:<length>`): the lengths add up and the
    /// chunks read back as the file; a second game whose archive has the first one's bytes after
    /// 1234 others shares nearly all of its chunks (fixed-size chunks would share none).
    #[test]
    fn add_cuts_by_content() {
        let t = std::env::temp_dir().join(format!("ippai-cdc-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let store = t.join("store");
        let mut x = 0x2545F4914F6CDD1Du64;
        let body: Vec<u8> = (0..3_000_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect();
        let mut shifted = vec![7u8; 1234];
        shifted.extend_from_slice(&body);
        for (id, data) in [("one", &body), ("two", &shifted)] {
            let d = t.join(id);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("game.exe"), b"MZ").unwrap();
            fs::write(d.join("data.arc"), data).unwrap();
            let cmd = Cmd::Add { dir: d, exe: Some("game.exe".into()), id: Some(id.into()), title: None, engine: None, cover: None, args: vec![], env: vec![], mount: vec![], meta_id: None, no_meta: true, follow: false, chunk_kib: 64 };
            run(&store, cmd).unwrap();
        }
        let entries = |id: &str| -> Vec<(String, u64)> {
            let list = fs::read_to_string(store.join(format!("trees/{id}.tsv"))).unwrap();
            assert_eq!(list.lines().next().unwrap().split('\t').nth(1), Some("2"));
            let line = list.lines().find(|l| l.split('\t').nth(3) == Some("data.arc")).unwrap();
            line.split('\t').nth(5).unwrap().split(',').map(|c| {
                let (h, n) = c.split_once(':').unwrap();
                (h.to_string(), n.parse().unwrap())
            }).collect()
        };
        let (one, two) = (entries("one"), entries("two"));
        assert_eq!(one.iter().map(|e| e.1).sum::<u64>(), body.len() as u64);
        let mut back = Vec::new();
        for (h, n) in &one {
            let data = zstd::decode_all(&fs::read(store.join("chunks").join(&h[..2]).join(h)).unwrap()[..]).unwrap();
            assert_eq!(data.len() as u64, *n);
            back.extend(data);
        }
        assert_eq!(back, body);
        let names: std::collections::HashSet<_> = one.iter().map(|e| &e.0).collect();
        let shared = two.iter().filter(|e| names.contains(&e.0)).count();
        assert!(shared + 2 >= one.len(), "{shared} of {} chunks shared", one.len());
        let _ = fs::remove_dir_all(&t);
    }

    /// A store without sake's C:\ gets it from the fonts (a release's fonts/): the fonts under
    /// drive_c/windows/Fonts, read back as written; a store that has one keeps it; no fonts,
    /// no tree.
    #[test]
    fn prefix_from_the_fonts() {
        let t = std::env::temp_dir().join(format!("ippai-prefix-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let (store, fonts) = (t.join("store"), t.join("fonts"));
        assert!(!ensure_prefix(&store, &fonts).unwrap(), "no fonts folder");
        fs::create_dir_all(&fonts).unwrap();
        fs::write(fonts.join("README.txt"), b"readme").unwrap();
        assert!(!ensure_prefix(&store, &fonts).unwrap(), "no font in it");
        let font: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        fs::write(fonts.join("msgothic.ttf"), &font).unwrap();
        fs::write(fonts.join("IPA_Font_License.txt"), b"license").unwrap();
        assert!(ensure_prefix(&store, &fonts).unwrap());
        let tsv = store.join(format!("trees/{PREFIX_TREE}.tsv"));
        let list = fs::read_to_string(&tsv).unwrap();
        for f in ["drive_c/windows/Fonts/msgothic.ttf", "drive_c/windows/Fonts/IPA_Font_License.txt", "drive_c/windows/temp"] {
            assert!(list.contains(f), "{f} in\n{list}");
        }
        let line = list.lines().find(|l| l.split('\t').nth(3) == Some("drive_c/windows/Fonts/msgothic.ttf")).unwrap();
        let mut back = Vec::new();
        for c in line.split('\t').nth(5).unwrap().split(',') {
            let name = c.split(':').next().unwrap();
            let raw = fs::read(store.join("chunks").join(&name[..2]).join(name)).unwrap();
            back.extend(zstd::decode_all(&raw[..]).unwrap());
        }
        assert_eq!(back, font);
        assert!(fs::read_dir(&store).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
        // (made once: the next start leaves it, even with other fonts)
        fs::write(fonts.join("msmincho.ttf"), b"other").unwrap();
        assert!(!ensure_prefix(&store, &fonts).unwrap());
        assert_eq!(fs::read_to_string(&tsv).unwrap(), list);
        let _ = fs::remove_dir_all(&t);
    }

    /// Random files of many chunks (zero runs, repeated parts), packed with a file's chunks in
    /// parallel: the tree lists them in order, their lengths add up to the file's, and each
    /// decodes to its part (a zero chunk is `:<length>`).
    #[test]
    fn files_chunked_in_order() {
        let t = std::env::temp_dir().join(format!("ippai-chunks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        let (store, dir) = (t.join("store"), t.join("files"));
        fs::create_dir_all(&dir).unwrap();
        let mut seed = 0x9b05688c2b3e6c1fu64;
        let mut rnd = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let chunk = 4096u64;
        let mut files = Vec::new();
        for f in 0..5 {
            let n = 1 + rnd(60 * chunk);
            let data: Vec<u8> = (0..n).map(|i| match (i / chunk) % 7 { 3 => 0, 5 => (i % 251) as u8, _ => rnd(256) as u8 }).collect();
            fs::write(dir.join(format!("f{f}")), &data).unwrap();
            files.push(data);
        }
        let tsv = pack_tree(&store, &dir, "t", false, chunking(64)).unwrap();
        for (f, data) in files.iter().enumerate() {
            let line = tsv.lines().find(|l| l.split('\t').nth(3) == Some(&format!("f{f}"))).unwrap();
            let mut at = 0usize;
            for (i, c) in line.split('\t').nth(5).unwrap().split(',').enumerate() {
                let (name, n) = c.split_once(':').unwrap();
                let part = &data[at..at + n.parse::<usize>().unwrap()];
                at += part.len();
                if name.is_empty() {
                    assert!(part.iter().all(|&b| b == 0), "f{f} chunk {i}");
                    continue;
                }
                let raw = fs::read(store.join("chunks").join(&name[..2]).join(name)).unwrap();
                assert_eq!(zstd::decode_all(&raw[..]).unwrap(), part, "f{f} chunk {i}");
            }
            assert_eq!(at, data.len());
        }
        let _ = fs::remove_dir_all(&t);
    }
}