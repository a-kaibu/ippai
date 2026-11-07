//! apitrace (github.com/apitrace/apitrace) traces for the differential tests: a game's or a
//! probe's D3D9 calls recorded on real Windows, replayed on Windows and on sake by
//! tests/replay/d3d9replay.c, what each draws compared (docs/replay.md).

pub mod replay;
pub mod snappy;
pub mod trace;

#[cfg(test)]
mod tests;
