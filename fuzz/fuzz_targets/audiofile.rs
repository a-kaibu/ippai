//! sake's audio files (WAVE, MP4/M4A, Ogg: what games open with MCI, DirectShow and Media
//! Foundation): no panic on any bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(t) = sake::audiofile::open(data) {
        let _ = t.total_frames();
    }
});
