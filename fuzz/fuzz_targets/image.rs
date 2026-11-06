//! The image decoder OleLoadPicture and the WIC decoder use (BMP, PNG, JPEG from game files):
//! no panic, and what it returns is w x h BGRA.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some((w, h, px)) = sake::ole::decode(data) {
        assert_eq!(px.len() as u64, w as u64 * h as u64 * 4);
    }
});
