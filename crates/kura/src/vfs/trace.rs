//! The run's read trace: when the game first read each store chunk, of which file and where,
//! how long that read waited, and what each foresight (vfs/hints.rs by layout, vfs/predict.rs
//! by what scripts name) hinted when. masu serve keeps a game's last runs (`jit/traces/`); a
//! replay of them (hints_eval.rs) scores ways to foresee reads against what real plays read
//! and when, and is what a model of the next reads learns from.
//!
//! Text, a line each, tab-separated, times in ms from the run's first line:
//!   `P <ino> <path>`                          a file's path, before its first read
//!   `R <ms> <ino> <offset> <chunk> <hash> <waited ms>`   a chunk's first read (offset: the read's)
//!   `H <ms> <source> <hash>,<hash>,...`        a hint sent to the page
//! The first line is `ippai-trace 1`. TRACE_MAX bytes at most (the rest is not kept).

use super::{hex, Hash, Ino, Layout};
use std::collections::HashSet;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const TRACE_MAX: usize = 4 << 20;

#[derive(Default)]
struct Trace {
    start: Option<Instant>,
    text: String,
    named: HashSet<Ino>,
    read: HashSet<(Ino, u32)>,
    full: bool,
}

static TRACE: Mutex<Option<Trace>> = Mutex::new(None);

fn with(f: impl FnOnce(&mut Trace, u64)) {
    let mut g = TRACE.lock().unwrap_or_else(|e| e.into_inner());
    let t = g.get_or_insert_with(Trace::default);
    if t.full {
        return;
    }
    let start = *t.start.get_or_insert_with(Instant::now);
    if t.text.is_empty() {
        t.text.push_str("ippai-trace 1\n");
    }
    f(t, start.elapsed().as_millis() as u64);
    if t.text.len() > TRACE_MAX {
        // (cut at the last whole line)
        let at = t.text[..TRACE_MAX].rfind('\n').map_or(0, |i| i + 1);
        t.text.truncate(at);
        t.full = true;
    }
}

/// File `ino` is at `path` (a line once, before the file's reads).
pub(super) fn name(ino: Ino, path: &str) {
    with(|t, _| {
        if t.named.insert(ino) {
            let _ = writeln!(t.text, "P\t{ino}\t{}", path.replace(['\t', '\n'], " "));
        }
    });
}

/// The game read `ino`'s bytes `off..end` (laid out as `l`, `chunks`), waiting `waited`: a
/// line for each chunk read the first time.
pub(super) fn read(ino: Ino, off: u64, end: u64, l: &Layout, chunks: &[Option<Hash>], waited: Duration) {
    if end <= off {
        return;
    }
    with(|t, ms| {
        for i in l.index(off)..=l.index(end - 1) {
            let Some(h) = chunks.get(i).copied().flatten() else { continue };
            if t.read.insert((ino, i as u32)) {
                let _ = writeln!(t.text, "R\t{ms}\t{ino}\t{off}\t{i}\t{}\t{}", hex(&h), waited.as_millis());
            }
        }
    });
}

/// A hint of `hs`, from `source` ("layout", "script").
pub(super) fn hint(source: &str, hs: &[Hash]) {
    with(|t, ms| {
        let _ = write!(t.text, "H\t{ms}\t{source}\t");
        for (k, h) in hs.iter().enumerate() {
            if k > 0 {
                t.text.push(',');
            }
            t.text.push_str(&hex(h));
        }
        t.text.push('\n');
    });
}

/// The trace so far.
pub fn text() -> String {
    TRACE.lock().unwrap_or_else(|e| e.into_inner()).as_ref().map_or(String::new(), |t| t.text.clone())
}

#[cfg(test)]
pub(super) fn reset() {
    *TRACE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// A trace's lines, parsed (hints_eval.rs, tests).
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Line {
    Path { ino: Ino, path: String },
    Read { ms: u64, ino: Ino, off: u64, chunk: u32, hash: String, waited: u64 },
    Hint { ms: u64, source: String, hashes: Vec<String> },
}

/// `text`'s lines (others passed over).
pub fn parse(text: &str) -> Vec<Line> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            match f.as_slice() {
                ["P", ino, path] => Some(Line::Path { ino: ino.parse().ok()?, path: path.to_string() }),
                ["R", ms, ino, off, chunk, hash, waited] => Some(Line::Read { ms: ms.parse().ok()?, ino: ino.parse().ok()?, off: off.parse().ok()?, chunk: chunk.parse().ok()?, hash: hash.to_string(), waited: waited.parse().ok()? }),
                ["H", ms, source, hs] => Some(Line::Hint { ms: ms.parse().ok()?, source: source.to_string(), hashes: hs.split(',').filter(|h| !h.is_empty()).map(String::from).collect() }),
                _ => None,
            }
        })
        .collect()
}
