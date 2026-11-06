//! strtod/strtof/strtol/strtoul on any bytes, against glibc (the C standard's cases: the MS
//! extensions NAN(IND)/NAN(SNAN) excepted).
#![no_main]
use std::ffi::{c_char, CString};

use crt::num::{self, Float};
use libfuzzer_sys::fuzz_target;

fn errno() -> i32 {
    unsafe { *libc::__errno_location() }
}
fn clear() {
    unsafe { *libc::__errno_location() = 0 }
}

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let base = [0u32, 10, 16, 8, 2, 36][data[0] as usize % 6];
    let s: Vec<u8> = data[1..].iter().copied().take_while(|&b| b != 0).collect();
    let cs = CString::new(s.clone()).unwrap();
    let mut end: *mut c_char = std::ptr::null_mut();
    let used = |end: *mut c_char| end as usize - cs.as_ptr() as usize;

    clear();
    let g = unsafe { libc::strtod(cs.as_ptr(), &mut end) };
    let (ge, gn) = (errno(), used(end));
    let (v, n, e) = num::strtod(&s, Float::F64, false);
    let ext = s.to_ascii_lowercase().windows(4).any(|w| w == b"(ind" || w == b"(sna");
    assert_eq!(n, gn, "{s:?}");
    assert!(v.to_bits() == g.to_bits() || (v.is_nan() && g.is_nan() && (ext || v.is_sign_negative() == g.is_sign_negative())), "{s:?}");
    // (glibc: ERANGE for an inexact subnormal too, C11; Windows only when it rounds to zero)
    let sub = |v: f64, min: f64| v != 0.0 && v.abs() < min;
    // (and glibc leaks the ERANGE of reading a long NAN(digits) payload)
    let ge = if sub(v, f64::MIN_POSITIVE) || v.is_nan() { 0 } else { ge };
    assert_eq!(e, ge, "{s:?}");

    clear();
    let gf = unsafe { libc::strtof(cs.as_ptr(), &mut end) };
    let (ge, gn) = (errno(), used(end));
    let (v, n, e) = num::strtod(&s, Float::F32, false);
    assert_eq!(n, gn, "{s:?}");
    assert!((v as f32).to_bits() == gf.to_bits() || (v.is_nan() && gf.is_nan()), "{s:?}");
    // (glibc's strtof leaks the ERANGE of reading a long NAN(digits) payload)
    let ge = if sub(v, f32::MIN_POSITIVE as f64) || v.is_nan() { 0 } else { ge };
    assert_eq!(e, ge, "{s:?}");

    clear();
    let g = unsafe { libc::strtoll(cs.as_ptr(), &mut end, base as i32) };
    assert_eq!(num::strtol(&s, base, 64), (g, used(end), errno()), "{s:?}");
    clear();
    let g = unsafe { libc::strtoull(cs.as_ptr(), &mut end, base as i32) };
    assert_eq!(num::strtoul(&s, base, 64), (g, used(end), errno()), "{s:?}");
});
