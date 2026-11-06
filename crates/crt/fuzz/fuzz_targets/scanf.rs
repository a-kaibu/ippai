//! scanf on any input and format: no panic; every write fits the buffer the secure forms
//! were given; the count never exceeds the conversions.
#![no_main]
use crt::scan::{self, Mode, Sink};
use libfuzzer_sys::fuzz_target;

struct Check {
    sizes: u64,
    last_size: u64,
    secure: bool,
}

impl Sink for Check {
    fn ptr(&mut self) -> u64 {
        1
    }
    fn size(&mut self) -> u64 {
        self.sizes = self.sizes.rotate_left(7);
        self.last_size = self.sizes % 40;
        self.last_size
    }
    fn int(&mut self, _: u64, bytes: u32, _: u64) {
        assert!(matches!(bytes, 1 | 2 | 4 | 8));
    }
    fn float(&mut self, _: u64, bytes: u32, _: f64) {
        assert!(bytes == 4 || bytes == 8);
    }
    fn narrow(&mut self, _: u64, s: &[u8]) {
        assert!(!self.secure || s.len() as u64 <= self.last_size);
    }
    fn wide(&mut self, _: u64, s: &[u16]) {
        assert!(!self.secure || s.len() as u64 <= self.last_size);
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        s.iter().map(|&b| b as u16).collect()
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        s.iter().map(|&u| u as u8).collect()
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let (sel, rest) = (data[0], &data[1..]);
    let split = (rest[0] as usize).min(rest.len() - 1);
    let (f, input) = (&rest[1..1 + split], &rest[1 + split..]);
    let m = Mode { ptr64: sel & 1 != 0, secure: sel & 2 != 0, legacy: sel & 4 != 0, legacy_wide: sel & 8 != 0 };
    let mut k = Check { sizes: 0x9e37_79b9_7f4a_7c15 ^ sel as u64, last_size: 0, secure: m.secure };
    let n = if sel & 16 == 0 {
        scan::scan(input, f, m, &mut k)
    } else {
        let w = |b: &[u8]| -> Vec<u16> { b.iter().map(|&x| x as u16 | if x > 0xf0 { 0x3000 } else { 0 }).collect() };
        scan::scan(&w(input), &w(f), m, &mut k)
    };
    let convs = f.iter().filter(|&&c| c == b'%').count() as i32;
    assert!(n >= -1 && n <= convs);
});
