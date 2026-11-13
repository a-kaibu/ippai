//! Variable-length code tables (FFmpeg's VLC: vlc_init / ff_vlc_init_from_lengths / get_vlc2):
//! lookups of `bits` bits, with sub-tables for longer codes.

use crate::bits::Bits;

#[derive(Clone, Copy, Debug, Default)]
struct Entry {
    /// symbol, or the index of a sub-table (`len` < 0: its width is -len)
    sym: i32,
    /// bits of the code (0: no code here)
    len: i8,
}

#[derive(Clone, Debug)]
pub struct Vlc {
    table: Vec<Entry>,
    bits: u32,
}

impl Vlc {
    /// Codes given as (code, length, symbol), the code in the low `length` bits.
    pub fn new(bits: u32, codes: &[(u32, u8, i32)]) -> Vlc {
        let mut v = Vlc { table: Vec::new(), bits };
        let cs: Vec<(u32, u8, i32)> = codes.iter().copied().filter(|c| c.1 > 0 && c.1 <= 32).collect();
        v.build(0, bits, &cs, 0);
        v
    }
    /// Codes of the lengths `lens` (symbols `syms`, or their index + `offset`), assigned in
    /// list order (ff_vlc_init_from_lengths).
    pub fn from_lengths(bits: u32, lens: &[u8], syms: Option<&[i32]>, offset: i32) -> Vlc {
        let mut code: u64 = 0;
        let mut cs = Vec::new();
        for (i, &l) in lens.iter().enumerate() {
            let s = syms.map_or(i as i32, |s| s[i]) + offset;
            if l > 0 {
                cs.push(((code >> (32 - l as u32)) as u32, l, s));
                code += 1u64 << (32 - l as u32);
            }
        }
        Vlc::new(bits, &cs)
    }
    /// Codes `codes[i]` of lengths `lens[i]` for symbols i (vlc_init's tables).
    pub fn from_codes(bits: u32, lens: &[u8], codes: &[u32]) -> Vlc {
        let cs: Vec<(u32, u8, i32)> = lens.iter().zip(codes).enumerate().map(|(i, (&l, &c))| (c, l, i as i32)).collect();
        Vlc::new(bits, &cs)
    }

    /// Fills a table of `nb` bits at `base` for codes that start with `prefix_len` bits already
    /// matched (`codes` hold what is left of them, in their low bits).
    fn build(&mut self, base: usize, nb: u32, codes: &[(u32, u8, i32)], _depth: u32) {
        let size = 1usize << nb;
        if self.table.len() < base + size {
            self.table.resize(base + size, Entry::default());
        }
        // short codes fill all the entries they prefix
        let mut long: std::collections::BTreeMap<u32, Vec<(u32, u8, i32)>> = Default::default();
        for &(code, len, sym) in codes {
            if (len as u32) <= nb {
                let shift = nb - len as u32;
                let start = (code << shift) as usize;
                for k in 0..(1usize << shift) {
                    let e = &mut self.table[base + start + k];
                    if e.len == 0 {
                        *e = Entry { sym, len: len as i8 };
                    }
                }
            } else {
                let rest = len as u32 - nb;
                let idx = code >> rest;
                long.entry(idx).or_default().push((code & ((1u32 << rest) - 1), rest as u8, sym));
            }
        }
        for (idx, sub) in long {
            let max = sub.iter().map(|c| c.1 as u32).max().unwrap_or(1);
            let snb = max.min(self.bits);
            let sbase = self.table.len();
            self.table[base + idx as usize] = Entry { sym: sbase as i32, len: -(snb as i8) };
            self.build(sbase, snb, &sub, _depth + 1);
        }
    }

    /// The symbol of the code at the reader (-1: no such code; its bits are taken anyway).
    #[inline]
    pub fn read(&self, r: &mut Bits) -> i32 {
        let mut base = 0usize;
        let mut nb = self.bits;
        loop {
            let idx = r.peek(nb) as usize;
            let e = self.table[base + idx];
            if e.len > 0 {
                r.skip(e.len as usize);
                return e.sym;
            }
            if e.len == 0 {
                r.skip(nb as usize);
                return -1;
            }
            r.skip(nb as usize);
            base = e.sym as usize;
            nb = (-e.len) as u32;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A prefix-free code of random lengths (canonical, Kraft sum <= 1).
    fn prefix_code(lens: &[u8]) -> Vec<(u32, u8, i32)> {
        let mut order: Vec<usize> = (0..lens.len()).collect();
        order.sort_by_key(|&i| lens[i]);
        let mut code: u64 = 0;
        let mut out = Vec::new();
        for i in order {
            let l = lens[i] as u32;
            if code >= 1 << 32 {
                break;
            }
            out.push(((code >> (32 - l)) as u32, lens[i], i as i32));
            code += 1 << (32 - l);
        }
        out
    }

    proptest! {
        /// Every code reads back as its symbol, whatever bits follow it, with any lookup width.
        #[test]
        fn codes_read_back(lens in prop::collection::vec(1u8..=22, 1..300), bits in 3u32..12, tail in any::<u32>()) {
            let cs = prefix_code(&lens);
            let v = Vlc::new(bits, &cs);
            for &(code, len, sym) in &cs {
                let word = ((code as u64) << (64 - len as u32)) | ((tail as u64 | (tail as u64) << 32) >> len);
                let b = word.to_be_bytes();
                let mut r = Bits::new(&b);
                prop_assert_eq!(v.read(&mut r), sym);
                prop_assert_eq!(r.count(), len as usize);
            }
        }
    }
}
