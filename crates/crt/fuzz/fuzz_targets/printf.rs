//! printf on any format and any argument bits, narrow and wide, both dialects: no panic, the
//! output bounded, and a format without '%' copied as it is. The positional form (format_p,
//! _printf_p) on the same: no panic, bounded, and a format without '$' as printf has it.
#![no_main]
use crt::fmt::{self, Args, Mode};
use libfuzzer_sys::fuzz_target;

struct Bytes<'a>(&'a [u8]);

impl Bytes<'_> {
    fn take(&mut self, n: usize) -> u64 {
        let mut v = [0u8; 8];
        let k = n.min(self.0.len());
        v[..k].copy_from_slice(&self.0[..k]);
        self.0 = &self.0[k..];
        u64::from_le_bytes(v)
    }
}

impl Args for Bytes<'_> {
    fn int(&mut self, bytes: u32) -> u64 {
        self.take(bytes as usize)
    }
    fn float(&mut self) -> f64 {
        f64::from_bits(self.take(8))
    }
    fn narrow(&mut self, p: u64, max: Option<usize>) -> Vec<u8> {
        let n = (p % 64) as usize;
        (0..n.min(max.unwrap_or(usize::MAX))).map(|i| b'a' + (i % 26) as u8).collect()
    }
    fn wide(&mut self, p: u64, max: Option<usize>) -> Vec<u16> {
        let n = (p % 64) as usize;
        (0..n.min(max.unwrap_or(usize::MAX))).map(|i| 0x3042 + i as u16).collect()
    }
    fn counted(&mut self, p: u64) -> (usize, u64) {
        ((p % 300) as usize, p >> 8)
    }
    fn a2w(&self, s: &[u8]) -> Vec<u16> {
        s.iter().map(|&b| b as u16).collect()
    }
    fn w2a(&self, s: &[u16]) -> Vec<u8> {
        s.iter().flat_map(|&u| if u < 0x80 { vec![u as u8] } else { vec![0x82, 0xa0] }).collect()
    }
    fn store(&mut self, _: u64, bytes: u32, _: u64) {
        assert!(matches!(bytes, 1 | 2 | 4 | 8));
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        return;
    }
    let (sel, rest) = (data[0], &data[1..]);
    let split = (rest[0] as usize).min(rest.len() - 1);
    let (f, args) = (&rest[1..1 + split], &rest[1 + split..]);
    let m = match sel % 4 {
        0 => Mode::ucrt(sel & 4 != 0, (sel >> 3) as u64),
        1 => Mode::legacy(sel & 4 != 0),
        2 => Mode { allow_n: true, ..Mode::ucrt(true, 4) },
        _ => Mode::legacy(true),
    };
    if sel & 0x80 == 0 {
        let plain = fmt::format(f, m, &mut Bytes(args));
        if let Ok(o) = &plain {
            assert!(o.len() <= fmt::MAX_OUT);
            if !f.contains(&b'%') {
                assert_eq!(o, f);
            }
        }
        let p = fmt::format_p(f, m, &mut Bytes(args));
        if let Ok(o) = &p {
            assert!(o.len() <= fmt::MAX_OUT);
        }
        if !f.contains(&b'$') {
            assert_eq!(p, plain);
        }
    } else {
        let w: Vec<u16> = f.chunks(2).map(|c| c[0] as u16 | (c.get(1).copied().unwrap_or(0) as u16) << 8).collect();
        if let Ok(o) = fmt::format(&w, m, &mut Bytes(args)) {
            assert!(o.len() <= fmt::MAX_OUT);
        }
        if let Ok(o) = fmt::format_p(&w, m, &mut Bytes(args)) {
            assert!(o.len() <= fmt::MAX_OUT);
        }
    }
});
