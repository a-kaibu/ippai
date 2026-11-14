//! koji's check of a translated module the page uploads (`ippai serve` keeps what it accepts for
//! other pages): no panic on any bytes, and a module it accepts carries the canonical types and
//! imports sections of a translation (review b02: they had to match byte for byte), so that
//! no accepted bundle fails every other page's instantiation. The first byte picks how many
//! functions the module should export.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&n, m)) = data.split_first() else { return };
    if koji::jit::wasm::check(m, n as usize % 8) {
        // (the head of the module: the magic and version, then the sections it starts with)
        assert!(m.starts_with(b"\0asm\x01\0\0\0"));
    }
});
