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
mod meta;
mod movie;
mod nwjs;
mod pack;
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

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dirs = dirs::Dirs::load(cli.store)?;
    match cli.cmd {
        Cmd::Serve(a) if a.foreground => serve::run(&dirs, a),
        Cmd::Serve(a) => daemon::start(&dirs, &a),
        Cmd::Stop => daemon::stop(&dirs),
        Cmd::Status => daemon::status(&dirs),
        Cmd::Logs { follow, lines } => daemon::logs(&dirs, follow, lines),
        Cmd::Pack(c) => pack::run(&dirs.store, c),
        Cmd::Ttc { out, fonts } => ttc::run(&out, &fonts),
    }
}
