//! Windows Media decoders ported from FFmpeg 7.1 (LGPL-2.1-or-later): WMA 1 and 2 (`wma`),
//! MS-MPEG4/WMV1/WMV2 (`msmpeg4`), WMV3 Simple/Main (`vc1`).
//! sake's wmvcore readers and decoder DMOs use them (crates/sake/src/wmdec.rs).

pub mod bits;
pub mod hpel;
pub mod idct;
pub mod msmpeg4;
#[allow(clippy::all)]
pub mod msmpeg4_tables;
pub mod mdct;
pub mod vc1;
#[allow(clippy::all)]
pub mod vc1_tables;
pub mod vlc;
pub mod wma;
#[allow(clippy::all)]
pub mod wma_tables;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod video_tests;
