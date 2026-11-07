//! wsprintf's format filter (sake::user::wsprintf_format) on any UTF-16 text: no panic, never more
//! '%' than it was given, text without '%' unchanged, and filtering twice is filtering once.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let f: Vec<u16> = data.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let pct = |v: &[u16]| v.iter().filter(|&&c| c == u16::from(b'%')).count();
    let g = sake::user::wsprintf_format(&f);
    assert!(pct(&g) <= pct(&f));
    if pct(&f) == 0 {
        assert_eq!(g, f);
    }
    assert_eq!(sake::user::wsprintf_format(&g), g);
});
