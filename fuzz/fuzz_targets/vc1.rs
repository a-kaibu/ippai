//! The WMV 9 / VC-1 Simple / Main decoder (crates/wmcodec::vc1, FFmpeg's ported): any size,
//! sequence header and pictures, with and without B pictures: no panic, and pictures of the
//! stream's size.
#![no_main]

use libfuzzer_sys::fuzz_target;
use wmcodec::vc1::Decoder;

fuzz_target!(|data: &[u8]| {
    if data.len() < 7 {
        return;
    }
    let (w, h) = (data[0] as usize % 200 + 1, data[1] as usize % 200 + 1);
    let Ok(mut d) = Decoder::new(w, h, &data[2..6]) else { return };
    // (pictures split at each 0xff 0x00 0x5a)
    let body = &data[6..];
    let mut start = 0;
    for i in 0..=body.len() {
        if i == body.len() || body[i..].starts_with(&[0xff, 0x00, 0x5a]) {
            if let Ok(Some(p)) = d.decode(&body[start..i]) {
                assert_eq!((p.width, p.height), (w, h));
            }
            start = i + 3;
        }
    }
    if let Some(p) = d.flush() {
        assert_eq!((p.width, p.height), (w, h));
    }
});
