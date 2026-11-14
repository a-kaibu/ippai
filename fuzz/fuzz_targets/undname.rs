//! MSVC name undecoration (sake::undname: __unDName, UnDecorateSymbolName, type_info::name on a
//! game's RTTI and import names) on any text and flags: no panic, and undecorating the text
//! it printed is no panic either.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let flags = u16::from_le_bytes([data[0], data[1]]) as u32;
    let rest = &data[2..];
    let s = String::from_utf8_lossy(rest);
    if let Some(d) = sake::undname::undname(&s, flags) {
        let _ = sake::undname::undname(&d, flags);
    }
    let _ = sake::undname::undecorate(rest, flags);
});
