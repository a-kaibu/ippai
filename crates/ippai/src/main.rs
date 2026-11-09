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
mod nwjs;
mod pack;
mod portable;
mod saves;
mod serve;
mod tls_relay;
mod treefiles;
mod unpack;
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
    /// Every ErogameScape game and Japanese VNDB VN into a new SQLite file (gamedb.yml).
    Build {
        out: std::path::PathBuf,
        /// An earlier copy whose ErogameScape part to keep when ErogameScape does not answer.
        #[arg(long)]
        keep: Option<std::path::PathBuf>,
    },
    /// The published copy (ghcr.io/a-kaibu/ippai-gamedb:latest) into a file.
    Pull { out: std::path::PathBuf },
    /// Fails unless a copy holds enough to publish (VNDB's VNs; ErogameScape's games if any).
    Check {
        file: std::path::PathBuf,
        /// ErogameScape's games must be there too (a host it answers: tools/gamedb/README.md).
        #[arg(long)]
        egs: bool,
    },
    /// A copy onto the package (GHCR_TOKEN: a token with write:packages).
    Push { file: std::path::PathBuf },
    /// pull, build --keep, check, push in a directory: the daily job (gamedb.yml, worker2's timer).
    Update {
        dir: std::path::PathBuf,
        /// ErogameScape's games must be there (a host it answers).
        #[arg(long)]
        egs: bool,
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
    /// Delete objects no longer named (replaced, old save revisions) that no kept backup
    /// refers to, once retired for the grace period.
    Gc {
        #[arg(long, default_value_t = 7)]
        grace_days: u64,
        /// Keep only the newest N backups (the others are deleted first).
        #[arg(long)]
        keep_backups: Option<usize>,
    },
}

fn storage_command(store: &std::path::Path, action: StorageCmd) -> anyhow::Result<()> {
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
            StorageCmd::Gc { grace_days, keep_backups } => {
                let r = storage.gc(std::time::Duration::from_secs(grace_days * 86400), keep_backups).await?;
                println!("deleted {} objects, kept {} retired, dropped {} backups", r.deleted, r.kept, r.backups_dropped);
                Ok(())
            }
        }
    })
}

fn ghcr_token() -> anyhow::Result<String> {
    std::env::var("GHCR_TOKEN").ok().filter(|t| !t.is_empty()).ok_or_else(|| anyhow::anyhow!("GHCR_TOKEN: a token with write:packages"))
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dirs = dirs::Dirs::load(cli.store)?;
    match cli.cmd {
        Cmd::Storage { action } => storage_command(&dirs.store, action),
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
        Cmd::Gamedb { action: GamedbCmd::Build { out, keep } } => gamedb::build(&out, keep.as_deref()),
        Cmd::Gamedb { action: GamedbCmd::Pull { out } } => gamedb::pull(&out, None).map(|d| println!("{d}")),
        Cmd::Gamedb { action: GamedbCmd::Check { file, egs } } => gamedb::check(&file, egs),
        Cmd::Gamedb { action: GamedbCmd::Push { file } } => gamedb::push(&file, &ghcr_token()?).map(|d| println!("{d}")),
        Cmd::Gamedb { action: GamedbCmd::Update { dir, egs } } => gamedb::update(&dir, egs, &ghcr_token()?).map(|d| println!("{d}")),
        Cmd::Ttc { out, fonts } => ttc::run(&out, &fonts),
    }
}
