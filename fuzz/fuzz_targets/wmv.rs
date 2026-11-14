//! The MS-MPEG4 v3 / WMV 1 / WMV 2 decoder (crates/wmcodec::msmpeg4, FFmpeg's ported): any
//! version, size, extradata and pictures: no panic, and pictures of the stream's size.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wmcodec::msmpeg4::{Decoder, Version};

fuzz_target!(|data: &[u8]| {
    if data.len() < 8 {
        return;
    }
    let ver = [Version::V3, Version::Wmv1, Version::Wmv2][data[0] as usize % 3];
    let (w, h) = (data[1] as usize % 200 + 1, data[2] as usize % 200 + 1);
    let Ok(mut d) = Decoder::new(ver, w, h, &data[3..7]) else { return };
    // (pictures split at each 0xff 0x00 0x5a)
    let body = &data[7..];
    let mut start = 0;
    for i in 0..=body.len() {
        if i == body.len() || body[i..].starts_with(&[0xff, 0x00, 0x5a]) {
            if let Ok(Some(p)) = d.decode(&body[start..i]) {
                assert_eq!((p.width, p.height), (w, h));
            }
            start = i + 3;
        }
    }
});
