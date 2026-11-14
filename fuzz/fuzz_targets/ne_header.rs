//! The Win16 NE loader's header readers (sake::w16::check_ne: module name, entry points,
//! imported module names, export lookup) on any bytes after an MZ / NE stub: no panic, no hang.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // (an MZ header pointing at an NE header right after it, then the fuzzed bytes)
    let mut f = vec![0u8; 0x40];
    f[0..2].copy_from_slice(b"MZ");
    f[0x3c..0x40].copy_from_slice(&0x40u32.to_le_bytes());
    f.extend_from_slice(b"NE");
    f.extend_from_slice(data);
    let _ = sake::w16::check_ne(&f);
    let _ = sake::w16::check_ne(data);
});
