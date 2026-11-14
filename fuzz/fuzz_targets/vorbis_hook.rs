//! The Vorbis-in-the-exe locator (sake::vorbis_hook::locate_in): any bytes as a module image
//! (code then data, as the patcher scans them): no panic, and what it finds lies in the image.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let base = 0x40_0000u32;
    if let Some((funcs, _)) = sake::vorbis_hook::locate_in(base, data) {
        for f in funcs {
            assert!(f >= base && ((f - base) as usize) < data.len().max(1) + 0x1000, "{f:x}");
        }
    }
});
