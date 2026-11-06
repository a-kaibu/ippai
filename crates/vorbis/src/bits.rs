//! Vorbis's bit packing: least significant bit first, as libogg's oggpack_read.

/// Reads bits from a packet. Past its end every read fails (as oggpack_read's -1), and the
/// reader stays at the end.
#[derive(Clone)]
pub struct Bits<'a> {
    data: &'a [u8],
    /// bits read so far
    pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Bits { data, pos: 0 }
    }

    /// A reader `pos` bits into `data` (where libogg's endbyte * 8 + endbit is).
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Bits { data, pos: pos.min(data.len() * 8) }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    fn left(&self) -> usize {
        self.data.len() * 8 - self.pos
    }

    /// The next `n` bits (0..=32) without taking them, and how many of them are there.
    #[inline]
    pub fn peek(&self, n: u32) -> (u32, u32) {
        debug_assert!(n <= 32);
        let have = (self.left() as u32).min(n);
        let byte = self.pos / 8;
        let shift = (self.pos % 8) as u32;
        let mut v: u64 = 0;
        // (up to 5 bytes hold 32 bits at any shift)
        for k in 0..5 {
            if let Some(&b) = self.data.get(byte + k) {
                v |= (b as u64) << (8 * k);
            }
        }
        let v = (v >> shift) as u32;
        let mask = if have == 32 { u32::MAX } else { (1u32 << have) - 1 };
        (v & mask, have)
    }

    #[inline]
    pub fn skip(&mut self, n: u32) {
        self.pos = (self.pos + n as usize).min(self.data.len() * 8);
    }

    /// `n` bits (0..=32), or None past the end (then the reader is at the end).
    #[inline]
    pub fn read(&mut self, n: u32) -> Option<u32> {
        let (v, have) = self.peek(n);
        if have < n {
            self.pos = self.data.len() * 8;
            return None;
        }
        self.pos += n as usize;
        Some(v)
    }

    pub fn flag(&mut self) -> Option<bool> {
        self.read(1).map(|b| b == 1)
    }
}

/// The number of bits in `v` (the specification's ilog: 0 for 0).
pub fn ilog(v: u32) -> u32 {
    32 - v.leading_zeros()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsb_first() {
        // the specification's example: 0b0000_1100 | ..., reading 4 bits then 3
        let d = [0b1010_1100u8, 0b0000_0001];
        let mut b = Bits::new(&d);
        assert_eq!(b.read(4), Some(0b1100));
        assert_eq!(b.read(3), Some(0b010));
        assert_eq!(b.read(2), Some(0b11));
        assert_eq!(b.read(7), Some(0));
        assert_eq!(b.read(1), None);
        assert_eq!(b.read(0), Some(0));
    }

    #[test]
    fn wide_reads() {
        let d = [0x78, 0x56, 0x34, 0x12, 0xff];
        let mut b = Bits::new(&d);
        assert_eq!(b.read(32), Some(0x1234_5678));
        let mut b = Bits::at(&d, 4);
        assert_eq!(b.read(32), Some(0xf123_4567));
        assert_eq!(b.read(5), None);
        assert_eq!(b.pos(), 40);
    }

    #[test]
    fn ilog_values() {
        assert_eq!([0, 1, 2, 3, 4, 7, 8, 0x7fff_ffff].map(ilog), [0, 1, 2, 2, 3, 3, 4, 31]);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Any read on any packet stays inside it and agrees with peek.
    #[kani::proof]
    #[kani::unwind(7)]
    fn read_in_bounds() {
        let data: [u8; 4] = kani::any();
        let len: usize = kani::any();
        kani::assume(len <= 4);
        let pos: usize = kani::any();
        let n: u32 = kani::any();
        kani::assume(n <= 32);
        let mut b = Bits::at(&data[..len], pos);
        let (p, have) = b.peek(n);
        let r = b.read(n);
        assert!(b.pos() <= len * 8);
        if have == n {
            assert_eq!(r, Some(p));
        } else {
            assert!(r.is_none());
            assert_eq!(b.pos(), len * 8);
        }
    }
}
