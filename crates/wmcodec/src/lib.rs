//! Windows Media decoders ported from FFmpeg 7.1 (LGPL-2.1-or-later): WMA 1 and 2 (`wma`).
//! sake's wmvcore readers and decoder DMOs use them (crates/sake/src/wmdec.rs).

pub mod bits;
pub mod mdct;
pub mod vlc;
pub mod wma;
#[allow(clippy::all)]
pub mod wma_tables;

#[cfg(test)]
mod tests;
