//! sake's font collections (crates/ippai/src/ttc.rs, `ippai ttc`): from any bytes, split in up
//! to three fonts, a collection or an error, never a panic (review b01: unchecked offsets of a
//! font cut short or damaged did). A collection made is a TrueType collection of that many fonts.
#![no_main]

#[path = "../../src/ttc.rs"]
#[allow(dead_code)]
mod ttc;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&n, rest)) = data.split_first() else { return };
    let n = (n % 3 + 1) as usize;
    let part = rest.len() / n;
    let fonts: Vec<&[u8]> = (0..n).map(|i| &rest[i * part..if i + 1 == n { rest.len() } else { (i + 1) * part }]).collect();
    if let Ok(c) = ttc::collection(&fonts) {
        assert_eq!(&c[..4], b"ttcf");
        assert_eq!(u32::from_be_bytes(c[8..12].try_into().unwrap()) as usize, n);
    }
});
