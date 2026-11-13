//! The MPEG-1 system stream demuxer and the video and layer II audio decoders sake's
//! DirectShow MPEG-1 splitter uses (crates/mpeg1): nothing panics on any bytes, and a stream cut
//! into any slices gives the same packets, pictures and samples as whole (mpeg1::check).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|input: &[u8]| {
    let Some((&n, rest)) = input.split_first() else { return };
    let n = (n as usize % 8 * 2).min(rest.len());
    let (cuts, data) = rest.split_at(n);
    let cuts: Vec<usize> = cuts.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as usize).collect();
    mpeg1::check::check(data, &cuts);
});
