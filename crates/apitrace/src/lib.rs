//! apitrace (github.com/apitrace/apitrace) traces for the differential tests: a game's or a
//! probe's D3D8 / D3D9 / D3D11 calls recorded on real Windows (32- or 64-bit), replayed on
//! Windows and on sake by tests/replay/replay.c, what each draws compared (docs/replay.md).

pub mod layout;
pub mod replay;
pub mod snappy;
pub mod trace;

#[cfg(test)]
mod tests;
