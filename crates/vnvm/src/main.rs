//! vnvm: the server and the store tool in one binary.
//!
//! ```text
//! vnvm serve [--web web/dist --jit jit --logs logs --listen 127.0.0.1:8080]
//! vnvm pack <dir> <name>                  pack a directory tree into the store
//! vnvm add-game <id> --title --tree --exe  put a packed game in the launcher's list
//! vnvm remove-game <id>
//! vnvm meta <id>... | --all | --search kw  game metadata from ErogameScape / VNDB
//! vnvm ttc <out.ttc> <a.ttf>...           one font collection sharing the fonts' glyphs
//! ```
//! `--store` (default `store`) goes before the command.

mod meta;
mod movie;
mod nwjs;
mod pack;
mod saves;
mod serve;
mod treefiles;
mod ttc;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about, version)]
struct Cli {
    /// The store: packed trees (trees/, chunks/) and the game list (games/).
    #[arg(long, default_value = "store", global = true)]
    store: PathBuf,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Serve the web app, the store, saved x86 regions, saves and logs.
    Serve(serve::Args),
    #[command(flatten)]
    Pack(pack::Cmd),
    /// One TrueType collection from fonts that share their glyphs (sake's MS-named fonts:
    /// msgothic.ttc from msgothic, mspgothic, msuigothic.ttf; msmincho.ttc from msmincho,
    /// mspmincho.ttf), the first font's glyphs shared by the others where theirs are the same.
    Ttc { out: PathBuf, fonts: Vec<PathBuf> },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve(a) => serve::run(cli.store, a),
        Cmd::Pack(c) => pack::run(&cli.store, c),
        Cmd::Ttc { out, fonts } => ttc::run(&out, &fonts),
    }
}
