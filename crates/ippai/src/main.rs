//! ippai: one binary that serves the web app (built in) and manages the games.
//!
//! ```text
//! ippai serve [--foreground] [--listen 127.0.0.1:8656]   start the server (in the background)
//! ippai stop | status | logs [--follow]
//! ippai add <dir> --exe game.exe [--id --title --cover ...]  pack a game folder and list it
//! ippai remove <id>
//! ippai meta <id>... | --all | --search kw  game metadata from ErogameScape / VNDB
//! ```
//! Where things live: see dirs.rs (XDG on Linux; `--store` or config.toml move the store).
//! `ttc` (hidden) makes sake's font collections. `pack` and `add-game` (hidden) are the two halves of `add`, for trees that are no game
//! (sake's C:\ with the fonts) and for scripts.

mod apps;
mod asar;
mod auth;
mod cdc;
#[cfg(unix)]
mod daemon;
mod dirs;
mod envelope;
mod library;
mod library_api;
mod storage;
mod store_api;
mod meta;
mod meta_url;
mod egs_match;
mod gamedb;
mod movie;
mod evb;
mod nwjs;
mod pack;
mod portable;
mod saves;
mod serve;
mod tls_relay;
mod treefiles;
mod unpack;
mod update;
mod ttc;
mod webassets;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about, version = serve::VERSION_LINE)]
struct Cli {
    /// The store: packed trees (trees/, chunks/), the game list (games/) [default: ~/.local/share/ippai/store]
    #[arg(long, global = true)]
    store: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Encrypted S3 storage operations (IPPAI_STORAGE_CONFIG).
    Storage {
        #[command(subcommand)]
        action: StorageCmd,
    },
    /// Start the server in the background (`--foreground` to keep it in this process).
    Serve(serve::Args),
    /// Stop the server.
    Stop,
    /// Whether the server runs, and its URL.
    Status,
    /// The token other devices' browsers sign in with, and the URL that does it.
    Token {
        /// Make another one (the old one stops working when the server starts again).
        #[arg(long)]
        new: bool,
    },
    /// The server's log.
    Logs {
        /// Keep printing what the server writes.
        #[arg(long, short)]
        follow: bool,
        /// Lines from the end to print first.
        #[arg(long, short = 'n', default_value_t = 100)]
        lines: usize,
    },
    #[command(flatten)]
    Pack(pack::Cmd),
    /// The game database (gamedb.rs): `build <out>` from ErogameScape and VNDB.
    Gamedb {
        #[command(subcommand)]
        action: GamedbCmd,
    },
    /// One TrueType collection from fonts that share their glyphs (sake's MS-named fonts:
    /// msgothic.ttc from msgothic, msuigothic, mspgothic.ttf (Windows' order); msmincho.ttc from msmincho,
    /// mspmincho.ttf), the first font's glyphs shared by the others where theirs are the same.
    #[command(hide = true)]
    Ttc { out: PathBuf, fonts: Vec<PathBuf> },
}

#[derive(Subcommand)]
enum GamedbCmd {
    /// Build a compact SQLite part from the public mirror (or --live for the original service).
    Build {
        out: std::path::PathBuf,
        #[arg(long, value_enum)]
        part: gamedb::Part,
        #[command(flatten)]
        source: gamedb::Source,
    },
    /// A part's published copy (ghcr.io/a-kaibu/ippai-gamedb:<part>) into a file.
    Pull {
        out: std::path::PathBuf,
        #[arg(long, value_enum)]
        part: gamedb::Part,
    },
    /// Fails unless a copy holds enough of its part to publish.
    Check {
        file: std::path::PathBuf,
        #[arg(long, value_enum)]
        part: gamedb::Part,
    },
    /// A copy onto the package as its part (GHCR_TOKEN, or gh's token: write:packages).
    Push {
        file: std::path::PathBuf,
        #[arg(long, value_enum)]
        part: gamedb::Part,
    },
    /// Build, check and publish a part from its SQLite mirror (manual update).
    Update {
        dir: std::path::PathBuf,
        #[arg(long, value_enum)]
        part: gamedb::Part,
        #[command(flatten)]
        source: gamedb::Source,
    },
}

#[derive(Subcommand)]
enum StorageCmd {
    /// Preprocess locally with add/pack, then upload the store as encrypted UUID objects.
    Push,
    /// Save a consistent encrypted SQLite snapshot; prints its recovery UUID.
    Backup,
    /// Restore a backup to a new database path, never over an existing database.
    Restore { backup: uuid::Uuid, output: PathBuf },
    /// Merge another store's backup (its `storage backup` UUID, same bucket and keys) into this
    /// store: the games it added show up here (see Storage::merge for who wins).
    Merge { backup: uuid::Uuid },
    /// Rewrap current object keys with the configured active key.
    Rotate,
    /// Rewrap an existing backup envelope without rewriting its ciphertext.
    RotateBackup { backup: uuid::Uuid },
    /// Make delivery packs of games' chunks (docs/s3-reads.md): their objects side by side in
    /// read order, about --mib each, so a browser fetches many with one request. The objects
    /// stay; packing a game again packs only what was added.
    Packs {
        /// Games to pack [default: every game]
        games: Vec<String>,
        /// ippai serve's saved regions and read-order lists [default: ~/.cache/ippai/jit]
        #[arg(long)]
        jit: Option<PathBuf>,
        #[arg(long, default_value_t = storage::PACK_BYTES >> 20)]
        mib: usize,
    },
    /// Delete objects no longer named (replaced, old save revisions, save states' files no
    /// state index names) that no kept backup refers to, once retired for the grace period.
    Gc {
        #[arg(long, default_value_t = 7)]
        grace_days: u64,
        /// Keep only the newest N backups (the others are deleted first).
        #[arg(long)]
        keep_backups: Option<usize>,
    },
}

fn storage_command(store: &std::path::Path, cache: &std::path::Path, action: StorageCmd) -> anyhow::Result<()> {
    let db = library::Library::new(store)?;
    let storage = storage::Storage::configured(db)?.ok_or_else(|| anyhow::anyhow!("set IPPAI_STORAGE_CONFIG"))?;
    tokio::runtime::Runtime::new()?.block_on(async {
        match action {
            StorageCmd::Push => storage.push_store(store).await,
            StorageCmd::Backup => { println!("{}", storage.backup().await?); Ok(()) },
            StorageCmd::Restore { backup, output } => storage.restore(backup, &output).await,
            StorageCmd::Merge { backup } => {
                let m = storage.merge(backup, store).await?;
                println!(
                    "merged: {} objects added, {} catalog files taken, {} kept as here, {} the same; {} saves, {} records",
                    m.added, m.replaced, m.kept, m.same, m.saves, m.records
                );
                Ok(())
            }
            StorageCmd::Rotate => storage.rotate_objects(),
            StorageCmd::RotateBackup { backup } => storage.rotate_backup(backup).await,
            StorageCmd::Packs { games, jit, mib } => {
                let jit = jit.unwrap_or_else(|| cache.join("jit"));
                let games = if games.is_empty() { pack::game_ids(store)? } else { games };
                let mut failed = 0;
                for id in games {
                    let list = pack::read_order_list(store, &jit, &id);
                    let order = pack::read_order(store, &id, &list)?;
                    // (one game failing leaves the others: run again to retry, what was packed stays)
                    match storage.pack_objects(&order, mib << 20).await {
                        Ok(p) => println!("{id}: {} packs, {} objects, {} MiB ({} chunks in read order first; {} in a pack already)", p.packs, p.objects, p.bytes >> 20, list.len() / 16, p.skipped),
                        Err(e) => {
                            eprintln!("{id}: {e:#}");
                            failed += 1;
                        }
                    }
                }
                anyhow::ensure!(failed == 0, "{failed} games not packed");
                Ok(())
            }
            StorageCmd::Gc { grace_days, keep_backups } => {
                // (save states' files no index names: forgotten, their objects retired)
                let n = saves::Saves::new(store)?.prune_states(std::time::Duration::from_secs(grace_days * 86400)).await?;
                println!("forgot {n} save state files no index names");
                let r = storage.gc(std::time::Duration::from_secs(grace_days * 86400), keep_backups).await?;
                println!("deleted {} objects, kept {} retired, dropped {} backups", r.deleted, r.kept, r.backups_dropped);
                Ok(())
            }
        }
    })
}

/// GHCR_TOKEN, or the GitHub CLI's own token (`gh auth token`; it needs the write:packages scope:
/// `gh auth refresh -s write:packages`).
fn ghcr_token() -> anyhow::Result<String> {
    if let Some(t) = std::env::var("GHCR_TOKEN").ok().filter(|t| !t.is_empty()) {
        return Ok(t);
    }
    let gh = std::process::Command::new("gh").args(["auth", "token"]).stderr(std::process::Stdio::inherit()).output();
    match gh {
        Ok(o) if o.status.success() && !o.stdout.trim_ascii().is_empty() => Ok(String::from_utf8(o.stdout)?.trim().to_string()),
        _ => anyhow::bail!("GHCR_TOKEN (a token with write:packages), or gh signed in (gh auth login; gh auth refresh -s write:packages)"),
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dirs = dirs::Dirs::load(cli.store)?;
    match cli.cmd {
        Cmd::Storage { action } => storage_command(&dirs.store, &dirs.cache, action),
        Cmd::Serve(a) if a.foreground => serve::run(&dirs, a),
        #[cfg(unix)]
        Cmd::Serve(a) => daemon::start(&dirs, &a),
        #[cfg(unix)]
        Cmd::Stop => daemon::stop(&dirs),
        #[cfg(unix)]
        Cmd::Status => daemon::status(&dirs),
        #[cfg(not(unix))]
        Cmd::Serve(_) | Cmd::Stop | Cmd::Status | Cmd::Logs { .. } => {
            anyhow::bail!("the server in the background is for Linux and macOS: on Windows, `ippai serve --foreground` (pack, add, storage work as on Linux)")
        }
        Cmd::Token { new } => {
            let t = auth::token(&dirs.token, new)?;
            println!("{t}");
            // (the token after #: no request carries it, nor a log of the server's or a proxy's)
            eprintln!("sign a browser in once: open <the server's URL>/login#{t} (http://{}/login#{t} on this machine)", dirs.listen);
            if new {
                eprintln!("restart the server (ippai stop; ippai serve) for the new one");
            }
            Ok(())
        }
        #[cfg(unix)]
        Cmd::Logs { follow, lines } => daemon::logs(&dirs, follow, lines),
        Cmd::Pack(c) => pack::run(&dirs.store, c),
        Cmd::Gamedb { action: GamedbCmd::Build { out, part, source } } => gamedb::build(&out, part, &source),
        Cmd::Gamedb { action: GamedbCmd::Pull { out, part } } => gamedb::pull(&out, part, None).map(|d| println!("{d}")),
        Cmd::Gamedb { action: GamedbCmd::Check { file, part } } => gamedb::check(&file, part),
        Cmd::Gamedb { action: GamedbCmd::Push { file, part } } => gamedb::push(&file, part, &ghcr_token()?).map(|d| println!("{d}")),
        Cmd::Gamedb { action: GamedbCmd::Update { dir, part, source } } => gamedb::update(&dir, part, &ghcr_token()?, &source).map(|d| println!("{d}")),
        Cmd::Ttc { out, fonts } => ttc::run(&out, &fonts),
    }
}
