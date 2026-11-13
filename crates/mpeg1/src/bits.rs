//! A big-endian bit reader over a byte slice, as pl_mpeg's plm_buffer reads: a read past the
//! end gives 0 and moves nothing, so a cut or corrupt stream decodes to something without
//! panicking.

pub struct Bits<'a> {
    d: &'a [u8],
    /// in bits
    pub pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(d: &'a [u8]) -> Self {
        Bits { d, pos: 0 }
    }

    pub fn has(&self, n: usize) -> bool {
        self.d.len() * 8 - self.pos.min(self.d.len() * 8) >= n
    }

    /// `n` (at most 32) bits, 0 when fewer are left
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 || !self.has(n as usize) {
            return 0;
        }
        let mut v: u64 = 0;
        let mut left = n;
        while left > 0 {
            let byte = self.d[self.pos >> 3] as u64;
            let rem = 8 - (self.pos & 7) as u32;
            let take = rem.min(left);
            let shift = rem - take;
            v = (v << take) | ((byte >> shift) & ((1 << take) - 1));
            self.pos += take as usize;
            left -= take;
        }
        v as u32
    }

    pub fn skip(&mut self, n: usize) {
        if self.has(n) {
            self.pos += n;
        }
    }

    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    /// The byte at the reading position (aligned), if any.
    pub fn byte_index(&self) -> usize {
        self.pos >> 3
    }

    /// Skips bytes equal to `v` (after aligning); how many.
    pub fn skip_bytes(&mut self, v: u8) -> usize {
        self.align();
        let mut n = 0;
        while self.has(8) && self.d[self.pos >> 3] == v {
            self.pos += 8;
            n += 1;
        }
        n
    }

    /// The next start code (00 00 01 xx) from here, aligned, with the reading position after it.
    /// (As pl_mpeg: one in the last 4 bytes is not looked at.)
    pub fn next_start_code(&mut self) -> Option<u8> {
        self.align();
        while self.has(5 << 3) {
            let i = self.pos >> 3;
            if self.d[i] == 0 && self.d[i + 1] == 0 && self.d[i + 2] == 1 {
                self.pos = (i + 4) << 3;
                return Some(self.d[i + 3]);
            }
            self.pos += 8;
        }
        None
    }

    pub fn peek_non_zero(&mut self, n: u32) -> bool {
        if !self.has(n as usize) {
            return false;
        }
        let v = self.read(n);
        self.pos -= n as usize;
        v != 0
    }

    /// A value of a variable-length code table: (index of the next pair, value) pairs walked a bit
    /// at a time from pair 0, as pl_mpeg's plm_vlc_t tables.
    pub fn vlc(&mut self, t: &[(i16, u16)]) -> u16 {
        let mut i = 0usize;
        loop {
            let (next, value) = t[i + self.read(1) as usize];
            if next <= 0 {
                return value;
            }
            i = next as usize;
            if i + 1 >= t.len() {
                return 0;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn reads_across_bytes_and_stops_at_the_end() {
        let mut b = Bits::new(&[0b1010_1100, 0b0101_0011]);
        assert_eq!(b.read(3), 0b101);
        assert_eq!(b.read(7), 0b0110001);
        assert_eq!(b.read(7), 0, "only 6 left");
        assert_eq!(b.pos, 10);
        assert_eq!(b.read(6), 0b010011);
        assert!(!b.has(1));
    }

    #[test]
    fn start_codes() {
        let d = [9, 0, 0, 1, 0xb3, 7, 7, 0, 0, 1, 0, 1, 2, 3, 4, 0, 0, 1, 0xb7];
        let mut b = Bits::new(&d);
        assert_eq!(b.next_start_code(), Some(0xb3));
        assert_eq!(b.pos, 5 * 8);
        assert_eq!(b.next_start_code(), Some(0));
        // (the last one is within the last 4 bytes: not looked at, as pl_mpeg)
        assert_eq!(b.next_start_code(), None);
    }

    proptest! {
        /// Reading n bits gives what the bits are, as a big-endian number, whatever the split.
        #[test]
        fn reads_match_the_bits(d in proptest::collection::vec(any::<u8>(), 0..16), splits in proptest::collection::vec(1u32..=32, 0..20)) {
            let mut b = Bits::new(&d);
            let total = d.len() * 8;
            let bit = |i: usize| (d[i >> 3] >> (7 - (i & 7))) & 1;
            for n in splits {
                let at = b.pos;
                let v = b.read(n);
                if at + n as usize <= total {
                    let want = (0..n as usize).fold(0u64, |a, k| a << 1 | bit(at + k) as u64) as u32;
                    prop_assert_eq!(v, want);
                    prop_assert_eq!(b.pos, at + n as usize);
                } else {
                    prop_assert_eq!(v, 0);
                    prop_assert_eq!(b.pos, at);
                }
            }
        }
    }
}

#[cfg(test)]
mod table_tests {
    use super::*;
    use crate::tables::*;

    /// Every code table walks inside itself for any bits (each link points at a whole pair),
    /// and every 16-bit input ends in at most 16 reads.
    #[test]
    fn code_tables_stay_inside() {
        let all: [(&str, &[(i16, u16)]); 9] = [
            ("mb_address_increment", MB_ADDRESS_INCREMENT),
            ("mb_type_i", MB_TYPE_I),
            ("mb_type_p", MB_TYPE_P),
            ("mb_type_b", MB_TYPE_B),
            ("coded_block_pattern", CODED_BLOCK_PATTERN),
            ("motion", MOTION),
            ("dct_size_y", DCT_SIZE_Y),
            ("dct_size_c", DCT_SIZE_C),
            ("dct_coeff", DCT_COEFF),
        ];
        for (name, t) in all {
            assert!(t.len() >= 2 && t.len() % 2 == 0, "{name}");
            for (i, &(next, _)) in t.iter().enumerate() {
                if next > 0 {
                    assert!(next as usize % 2 == 0 && next as usize + 1 < t.len() && next as usize > i / 2 * 2, "{name}[{i}] -> {next}");
                }
            }
            for x in 0..=u16::MAX {
                let d = x.to_be_bytes();
                let mut b = Bits::new(&d);
                b.vlc(t);
                assert!(b.pos <= 16, "{name} {x:#x}");
            }
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any read of any length at any place in any 5 bytes: in bounds, and the bits there as a
    /// big-endian number (or 0, not moving, past the end).
    #[kani::proof]
    #[kani::unwind(34)]
    fn read_is_the_bits() {
        let d: [u8; 5] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= 5);
        let pos: usize = kani::any();
        kani::assume(pos <= 48);
        let n: u32 = kani::any();
        kani::assume(n <= 32);
        let mut b = Bits { d: &d[..len], pos };
        let v = b.read(n);
        if n > 0 && pos + n as usize <= len * 8 {
            let mut want = 0u64;
            let mut k = 0;
            while k < n as usize {
                let i = pos + k;
                want = want << 1 | ((d[i >> 3] >> (7 - (i & 7))) & 1) as u64;
                k += 1;
            }
            assert_eq!(v, want as u32);
            assert_eq!(b.pos, pos + n as usize);
        } else {
            assert_eq!(v, 0);
            assert_eq!(b.pos, pos);
        }
    }

    /// peek_non_zero leaves the position as it was; skip_bytes and next_start_code never go
    /// past the end.
    #[kani::proof]
    #[kani::unwind(8)]
    fn moves_stay_inside() {
        let d: [u8; 6] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= 6);
        let pos: usize = kani::any();
        kani::assume(pos <= 48);
        let mut b = Bits { d: &d[..len], pos };
        let n: u32 = kani::any();
        kani::assume(n <= 32);
        b.peek_non_zero(n);
        assert_eq!(b.pos, pos);
        if pos <= len * 8 {
            b.skip_bytes(kani::any());
            assert!(b.pos <= len * 8);
            let at = b.pos;
            if let Some(_) = b.next_start_code() {
                assert!(b.pos <= len * 8 && b.pos > at);
            }
        }
    }
}
