//! The _s string functions against their models on any buffers (Kani proves it up to 4
//! bytes; this goes to any size).
#![no_main]
use crt::sec::{self, model};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 4 {
        return;
    }
    let (sel, dn, cnt) = (data[0], data[1] as usize % 48, data[2]);
    let rest = &data[3..];
    let d: Vec<u8> = rest.iter().copied().chain(std::iter::repeat(7)).take(dn).collect();
    let s: Vec<u8> = rest.iter().skip(dn).copied().take_while(|&b| b != 0).collect();
    let count = if cnt == 255 { sec::TRUNCATE } else { cnt as usize % 64 };
    let (dnull, snull) = (sel & 1 != 0, sel & 2 != 0);
    let dm = (!dnull).then_some(&d[..]);
    let sm = (!snull).then_some(&s[..]);
    let mut x = d.clone();
    let xm = (!dnull).then_some(&mut x[..]);
    let (got, want) = match sel >> 2 & 3 {
        0 => (sec::strcpy_s(xm, sm), model::strcpy_s(dm, sm)),
        1 => (sec::strcat_s(xm, sm), model::strcat_s(dm, sm)),
        2 => (sec::strncpy_s(xm, sm, count), model::strncpy_s(dm, sm, count)),
        _ => (sec::strncat_s(xm, sm, count), model::strncat_s(dm, sm, count)),
    };
    assert_eq!(got, want.0);
    if let Some(w) = want.1 {
        assert_eq!(x, w);
    }
});
