//! Bit reading, most significant bit first (FFmpeg's GetBitContext). Reads past the end give
//! zeros (FFmpeg pads its buffers with zeros); `left` says how far a reader went.

#[derive(Clone)]
pub struct Bits<'a> {
    buf: &'a [u8],
    /// position in bits
    pos: usize,
    /// bits in the buffer (may be fewer than buf.len() * 8)
    end: usize,
}

impl<'a> Bits<'a> {
    pub fn new(buf: &'a [u8]) -> Bits<'a> {
        Bits { buf, pos: 0, end: buf.len() * 8 }
    }
    /// The first `bits` bits of `buf`.
    pub fn with_len(buf: &'a [u8], bits: usize) -> Bits<'a> {
        Bits { buf, pos: 0, end: bits.min(buf.len() * 8) }
    }
    /// 32 bits from `pos` on (zeros past the end).
    #[inline]
    pub fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        // (one 8-byte load where the buffer has it; the five byte loads near its end)
        let v = match self.buf.get(byte..byte + 8) {
            Some(w) => ((u64::from_be_bytes(w.try_into().unwrap()) << (self.pos & 7)) >> 32) as u32,
            None => {
                let mut v: u64 = 0;
                for k in 0..5 {
                    v = v << 8 | *self.buf.get(byte + k).unwrap_or(&0) as u64;
                }
                ((v << (self.pos & 7)) >> 8) as u32
            }
        };
        // (bits past `end` read as 0)
        let have = self.end.saturating_sub(self.pos);
        if have >= 32 {
            v
        } else if have == 0 {
            0
        } else {
            v & !(u32::MAX >> have)
        }
    }
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.peek32() >> (32 - n)
        }
    }
    #[inline]
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
    }
    /// Up to 25 bits.
    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }
    /// Up to 32 bits.
    pub fn get_long(&mut self, n: u32) -> u32 {
        self.get(n)
    }
    #[inline]
    pub fn get1(&mut self) -> u32 {
        self.get(1)
    }
    /// A signed n-bit value (two's complement).
    pub fn get_sbits(&mut self, n: u32) -> i32 {
        let v = self.get(n);
        // (n 0 is 0: the shift by 32 wrapped, and panics with overflow checks)
        if n == 0 {
            return 0;
        }
        ((v << (32 - n)) as i32) >> (32 - n)
    }
    pub fn count(&self) -> usize {
        self.pos
    }
    pub fn left(&self) -> isize {
        self.end as isize - self.pos as isize
    }
    pub fn size(&self) -> usize {
        self.end
    }
    pub fn align(&mut self) {
        self.pos = self.pos.next_multiple_of(8);
    }
    pub fn seek(&mut self, pos: usize) {
        self.pos = pos;
    }
    /// The decimal digit-like unary code FFmpeg calls get_unary(gb, stop, len): ones (or zeros,
    /// `stop` 0) counted up to the first `stop` bit or `len`.
    pub fn unary(&mut self, stop: u32, len: u32) -> u32 {
        let mut i = 0;
        while i < len && self.get1() != stop {
            i += 1;
        }
        i
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Reading bit by bit and in runs of any widths give the same bits.
        #[test]
        fn runs_match_single_bits(buf in prop::collection::vec(any::<u8>(), 0..40), widths in prop::collection::vec(1u32..=25, 0..60)) {
            let mut a = Bits::new(&buf);
            let mut b = Bits::new(&buf);
            for w in widths {
                let mut v = 0u32;
                for _ in 0..w {
                    v = v << 1 | b.get1();
                }
                prop_assert_eq!(a.get(w), v);
            }
        }

        #[test]
        fn past_the_end_is_zero(buf in prop::collection::vec(any::<u8>(), 0..8), bits in 0usize..64) {
            let mut r = Bits::with_len(&buf, bits);
            let n = bits.min(buf.len() * 8);
            r.skip(n);
            prop_assert_eq!(r.peek32(), 0);
            prop_assert_eq!(r.left(), 0);
        }
    }

    /// the five-byte peek32 (before the 8-byte load)
    fn peek32_bytes(buf: &[u8], pos: usize, end: usize) -> u32 {
        let mut v: u64 = 0;
        for k in 0..5 {
            v = v << 8 | *buf.get((pos >> 3) + k).unwrap_or(&0) as u64;
        }
        let v = ((v << (pos & 7)) >> 8) as u32;
        let have = end.saturating_sub(pos);
        if have >= 32 { v } else if have == 0 { 0 } else { v & !(u32::MAX >> have) }
    }

    proptest! {
        /// review b11: the 8-byte load reads what five byte loads read, at every position
        #[test]
        fn wide_peek_matches_bytes(buf in prop::collection::vec(any::<u8>(), 0..40), bits in 0usize..400, pos in 0usize..400) {
            let end = bits.min(buf.len() * 8);
            let mut r = Bits::with_len(&buf, end);
            r.skip(pos);
            prop_assert_eq!(r.peek32(), peek32_bytes(&buf, pos, end));
        }

        /// review b11: get_sbits(n) is the n-bit value sign-extended, and 0 for n 0 (the shift
        /// by 32 wrapped)
        #[test]
        fn signed_bits(buf in prop::collection::vec(any::<u8>(), 0..8), n in 0u32..=25) {
            let v = Bits::new(&buf).get(n);
            let want = if n == 0 { 0 } else if v >> (n - 1) & 1 == 1 { v as i64 - (1i64 << n) } else { v as i64 };
            prop_assert_eq!(Bits::new(&buf).get_sbits(n) as i64, want);
        }
    }

    #[test]
    fn signed() {
        let b = [0b1110_0000];
        assert_eq!(Bits::new(&b).get_sbits(3), -1);
        assert_eq!(Bits::new(&[0b0110_0000]).get_sbits(3), 3);
    }
}
