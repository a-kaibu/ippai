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

mod daemon;
mod dirs;
mod envelope;
mod library;
mod library_api;
mod storage;
mod store_api;
mod meta;
mod movie;
mod nwjs;
mod pack;
mod portable;
mod saves;
mod serve;
mod treefiles;
mod ttc;
mod webassets;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about, version = env!("IPPAI_VERSION"))]
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
    /// One TrueType collection from fonts that share their glyphs (sake's MS-named fonts:
    /// msgothic.ttc from msgothic, mspgothic, msuigothic.ttf; msmincho.ttc from msmincho,
    /// mspmincho.ttf), the first font's glyphs shared by the others where theirs are the same.
    #[command(hide = true)]
    Ttc { out: PathBuf, fonts: Vec<PathBuf> },
}

#[derive(Subcommand)]
enum StorageCmd {
    /// Preprocess locally with add/pack, then upload the store as encrypted UUID objects.
    Push,
    /// Save a consistent encrypted SQLite snapshot; prints its recovery UUID.
    Backup,
    /// Restore a backup to a new database path, never over an existing database.
    Restore { backup: uuid::Uuid, output: PathBuf },
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dirs = dirs::Dirs::load(cli.store)?;
    match cli.cmd {
        Cmd::Storage { action } => storage_command(&dirs.store, action),
        Cmd::Serve(a) if a.foreground => serve::run(&dirs, a),
        Cmd::Serve(a) => daemon::start(&dirs, &a),
        Cmd::Stop => daemon::stop(&dirs),
        Cmd::Status => daemon::status(&dirs),
        Cmd::Logs { follow, lines } => daemon::logs(&dirs, follow, lines),
        Cmd::Pack(c) => pack::run(&dirs.store, c),
        Cmd::Ttc { out, fonts } => ttc::run(&out, &fonts),
    }
}
