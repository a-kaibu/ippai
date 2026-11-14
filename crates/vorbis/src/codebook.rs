//! Codebooks: the Huffman trees (codewords assigned as libvorbis's _make_words does) and the
//! VQ value vectors (as _book_unquantize).

use crate::bits::{ilog, Bits};
use crate::Error;

/// Codewords up to this long are found with one table lookup.
const FAST: u32 = 10;

pub struct Codebook {
    pub dim: usize,
    pub entries: usize,
    /// entries with a codeword
    pub used: usize,
    /// FAST bits (as read: least significant first) -> (entry, length); length 0: longer codeword
    fast: Vec<(u32, u8)>,
    /// codewords longer than FAST: (codeword bit-reversed to read order, length, entry)
    slow: Vec<(u32, u8, u32)>,
    /// a book with one entry (its codeword is any single bit)
    single: Option<u32>,
    /// entry * dim -> values (empty without a lookup)
    pub values: Vec<f32>,
}

/// The specification's float32_unpack (libvorbis _float32_unpack).
pub fn float32_unpack(x: u32) -> f32 {
    let mut mant = (x & 0x1f_ffff) as f64;
    let exp = ((x & 0x7fe0_0000) >> 21) as i32;
    if x & 0x8000_0000 != 0 {
        mant = -mant;
    }
    (mant * 2f64.powi(exp - 20 - 768)) as f32
}

/// The greatest r with r^dim <= entries (libvorbis _book_maptype1_quantvals).
pub fn lookup1_values(entries: usize, dim: usize) -> usize {
    if dim == 0 {
        return 0;
    }
    let mut r = (entries as f64).powf(1.0 / dim as f64).floor() as usize;
    // (fix floating point error either way)
    let pow = |r: usize| (0..dim).try_fold(1usize, |a, _| a.checked_mul(r));
    while pow(r + 1).is_some_and(|p| p <= entries) {
        r += 1;
    }
    while r > 0 && pow(r).is_none_or(|p| p > entries) {
        r -= 1;
    }
    r
}

/// Codewords for the lengths (0: unused), as libvorbis's _make_words: None when the tree is
/// over- or underfull (but a single entry is allowed).
pub fn make_words(lengths: &[u8]) -> Option<Vec<u32>> {
    let mut marker = [0u32; 33];
    let mut words = vec![0u32; lengths.len()];
    let mut count = 0;
    for (i, &len) in lengths.iter().enumerate() {
        if len == 0 {
            continue;
        }
        let len = len as usize;
        let mut entry = marker[len];
        if len < 32 && (entry >> len) != 0 {
            return None;
        }
        words[i] = entry;
        count += 1;
        let mut j = len;
        while j > 0 {
            if marker[j] & 1 != 0 {
                if j == 1 {
                    marker[1] += 1;
                } else {
                    marker[j] = marker[j - 1] << 1;
                }
                break;
            }
            marker[j] += 1;
            j -= 1;
        }
        for j in len + 1..33 {
            if marker[j] >> 1 == entry {
                entry = marker[j];
                marker[j] = marker[j - 1] << 1;
            } else {
                break;
            }
        }
    }
    if !(count == 1 && marker[2] == 2) {
        for (i, &m) in marker.iter().enumerate().skip(1) {
            if m & (u32::MAX >> (32 - i)) != 0 {
                return None;
            }
        }
    }
    Some(words)
}

fn reverse(word: u32, len: u32) -> u32 {
    if len == 0 {
        0
    } else {
        word.reverse_bits() >> (32 - len)
    }
}

impl Codebook {
    /// Builds a book from its codeword lengths (0: unused) and values.
    pub fn new(dim: usize, lengths: &[u8], values: Vec<f32>) -> Result<Codebook, Error> {
        let entries = lengths.len();
        let used = lengths.iter().filter(|&&l| l > 0).count();
        let mut book = Codebook { dim, entries, used, fast: Vec::new(), slow: Vec::new(), single: None, values };
        if used == 0 {
            return Ok(book);
        }
        let words = make_words(lengths).ok_or(Error::Codebook)?;
        if used == 1 {
            book.single = lengths.iter().position(|&l| l > 0).map(|e| e as u32);
            return Ok(book);
        }
        book.fast = vec![(0, 0); 1 << FAST];
        for (e, (&len, &w)) in lengths.iter().zip(&words).enumerate() {
            if len == 0 {
                continue;
            }
            let len32 = len as u32;
            let r = reverse(w, len32);
            if len32 <= FAST {
                let step = 1usize << len32;
                let mut k = r as usize;
                while k < 1 << FAST {
                    book.fast[k] = (e as u32, len);
                    k += step;
                }
            } else {
                book.slow.push((r, len, e as u32));
            }
        }
        book.slow.sort_by_key(|s| (s.1, s.0));
        Ok(book)
    }

    /// Reads one entry number (libvorbis decode_packed_entry_number); None at the packet's end
    /// (or for a codeword no entry has).
    #[inline]
    pub fn decode(&self, b: &mut Bits) -> Option<u32> {
        if let Some(e) = self.single {
            // (libvorbis: a one-entry book's codeword is a single bit)
            b.read(1)?;
            return Some(e);
        }
        if self.used == 0 {
            return None;
        }
        let (bits, have) = b.peek(FAST);
        let (e, len) = self.fast[bits as usize];
        if len != 0 {
            if (len as u32) > have {
                b.skip(have);
                return None;
            }
            b.skip(len as u32);
            return Some(e);
        }
        let (bits, have) = b.peek(32);
        // (one binary search per codeword length past FAST, shortest first: the slow entries
        // are sorted by (length, reversed codeword); a linear scan of a big residue book cost
        // O(entries) per long symbol)
        let mut i = 0;
        while i < self.slow.len() {
            let len = self.slow[i].1;
            let n = self.slow[i..].partition_point(|s| s.1 == len);
            let l32 = len as u32;
            if l32 > have {
                break;
            }
            let mask = if l32 == 32 { u32::MAX } else { (1 << l32) - 1 };
            let group = &self.slow[i..i + n];
            if let Ok(k) = group.binary_search_by_key(&(bits & mask), |s| s.0) {
                b.skip(l32);
                return Some(group[k].2);
            }
            i += n;
        }
        b.skip(have);
        None
    }

    /// [`Codebook::decode`]'s slow part as a scan of every long entry (the reference).
    #[cfg(test)]
    pub fn decode_scan(&self, b: &mut Bits) -> Option<u32> {
        if self.single.is_some() || self.used == 0 {
            return self.decode(b);
        }
        let (bits, have) = b.peek(FAST);
        let (e, len) = self.fast[bits as usize];
        if len != 0 {
            if (len as u32) > have {
                b.skip(have);
                return None;
            }
            b.skip(len as u32);
            return Some(e);
        }
        let (bits, have) = b.peek(32);
        for &(r, len, e) in &self.slow {
            let len = len as u32;
            if len > have {
                break;
            }
            let mask = if len == 32 { u32::MAX } else { (1 << len) - 1 };
            if bits & mask == r {
                b.skip(len);
                return Some(e);
            }
        }
        b.skip(have);
        None
    }

    /// The value vector of an entry.
    #[inline]
    pub fn vector(&self, e: u32) -> &[f32] {
        let s = e as usize * self.dim;
        &self.values[s..s + self.dim]
    }

    /// Reads a codebook from a setup header.
    pub fn read(b: &mut Bits) -> Result<Codebook, Error> {
        let e = Error::Header;
        if b.read(24).ok_or(e)? != 0x56_4342 {
            return Err(e);
        }
        let dim = b.read(16).ok_or(e)? as usize;
        let entries = b.read(24).ok_or(e)? as usize;
        let ordered = b.flag().ok_or(e)?;
        let mut lengths = vec![0u8; entries];
        if !ordered {
            let sparse = b.flag().ok_or(e)?;
            for l in lengths.iter_mut() {
                if !sparse || b.flag().ok_or(e)? {
                    *l = b.read(5).ok_or(e)? as u8 + 1;
                }
            }
        } else {
            let mut cur = 0usize;
            let mut len = b.read(5).ok_or(e)? + 1;
            while cur < entries {
                let n = b.read(ilog((entries - cur) as u32)).ok_or(e)? as usize;
                if len > 32 || cur + n > entries {
                    return Err(e);
                }
                lengths[cur..cur + n].fill(len as u8);
                cur += n;
                len += 1;
            }
        }
        let lookup = b.read(4).ok_or(e)?;
        let values = match lookup {
            0 => Vec::new(),
            1 | 2 => {
                let min = float32_unpack(b.read(32).ok_or(e)?);
                let delta = float32_unpack(b.read(32).ok_or(e)?);
                let bits = b.read(4).ok_or(e)? + 1;
                let seq = b.flag().ok_or(e)?;
                let quantvals = if lookup == 1 { lookup1_values(entries, dim) } else { entries * dim };
                // (no real book comes near; a damaged header must not ask for gigabytes)
                if quantvals > 1 << 20 || entries.saturating_mul(dim) > 1 << 22 {
                    return Err(e);
                }
                let mut mult = Vec::with_capacity(quantvals);
                for _ in 0..quantvals {
                    mult.push(b.read(bits).ok_or(e)?);
                }
                unquantize(lookup, dim, entries, &lengths, min, delta, seq, &mult)
            }
            _ => return Err(e),
        };
        if dim == 0 && lookup != 0 {
            return Err(e);
        }
        Codebook::new(dim, &lengths, values)
    }
}

/// The value vectors (libvorbis _book_unquantize, for every entry: unused ones too, never read).
#[allow(clippy::too_many_arguments)]
pub fn unquantize(lookup: u32, dim: usize, entries: usize, _lengths: &[u8], min: f32, delta: f32, seq: bool, mult: &[u32]) -> Vec<f32> {
    let mut v = vec![0f32; entries * dim];
    let quantvals = mult.len();
    for e in 0..entries {
        let mut last = 0f32;
        let mut div = 1usize;
        for k in 0..dim {
            let m = if lookup == 1 {
                if quantvals == 0 {
                    0
                } else {
                    mult[(e / div) % quantvals]
                }
            } else {
                mult.get(e * dim + k).copied().unwrap_or(0)
            };
            let val = m as f32 * delta + min + last;
            if seq {
                last = val;
            }
            v[e * dim + k] = val;
            if lookup == 1 {
                div = div.saturating_mul(quantvals.max(1));
            }
        }
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn spec_example_words() {
        // the specification's example (section 3.2.1): lengths 2 4 4 4 4 2 3 3
        let w = make_words(&[2, 4, 4, 4, 4, 2, 3, 3]).unwrap();
        assert_eq!(w, [0b00, 0b0100, 0b0101, 0b0110, 0b0111, 0b10, 0b110, 0b111]);
    }

    #[test]
    fn bad_trees() {
        assert!(make_words(&[1, 1, 1]).is_none()); // overfull
        assert!(make_words(&[1, 2]).is_none()); // underfull
        assert!(make_words(&[0, 1, 0]).is_some()); // one entry: codeword '0'
        assert!(make_words(&[0, 3, 0]).is_none()); // (libvorbis allows length 1 only)
    }

    #[test]
    fn float32() {
        // 1.0: mantissa 1<<20, exponent 788 - 20
        assert_eq!(float32_unpack((1 << 20) | (768 << 21)), 1.0);
        assert_eq!(float32_unpack(0x8000_0000 | 3 | ((788) << 21)), -3.0);
    }

    #[test]
    fn lookup1() {
        assert_eq!(lookup1_values(81, 4), 3);
        assert_eq!(lookup1_values(80, 4), 2);
        assert_eq!(lookup1_values(1, 8), 1);
        assert_eq!(lookup1_values(1 << 16, 2), 256);
    }

    /// Encodes `entries` with their codewords (as the encoder writes them: reversed).
    fn encode(lengths: &[u8], words: &[u32], entries: &[usize]) -> Vec<u8> {
        let mut out = vec![0u8; 64 + entries.len() * 4];
        let mut pos = 0usize;
        for &e in entries {
            let len = lengths[e] as u32;
            let r = reverse(words[e], len);
            for k in 0..len {
                if r >> k & 1 == 1 {
                    out[pos / 8] |= 1 << (pos % 8);
                }
                pos += 1;
            }
        }
        out.truncate(pos.div_ceil(8));
        out
    }

    /// Lengths of a full tree: split random leaves until there are `n`.
    fn full_tree(splits: Vec<usize>) -> Vec<u8> {
        let mut leaves = vec![1u8, 1];
        for s in splits {
            let i = s % leaves.len();
            if leaves[i] < 24 {
                let l = leaves[i] + 1;
                leaves[i] = l;
                leaves.insert(i + 1, l);
            }
        }
        leaves
    }

    proptest! {
        /// Any complete tree decodes what was encoded with it, through the table and past it.
        #[test]
        fn roundtrip(splits in prop::collection::vec(0usize..1000, 1..300), picks in prop::collection::vec(0usize..10000, 1..200), unused in prop::collection::vec(0usize..1000, 0..20)) {
            let tree = full_tree(splits);
            // (unused entries between used ones)
            let mut lengths = tree.clone();
            for u in unused { lengths.insert(u % (lengths.len() + 1), 0); }
            let words = make_words(&lengths).unwrap();
            let used: Vec<usize> = (0..lengths.len()).filter(|&i| lengths[i] > 0).collect();
            let seq: Vec<usize> = picks.iter().map(|p| used[p % used.len()]).collect();
            let data = encode(&lengths, &words, &seq);
            let book = Codebook::new(1, &lengths, vec![0.0; lengths.len()]).unwrap();
            let mut b = Bits::new(&data);
            for &e in &seq {
                prop_assert_eq!(book.decode(&mut b), Some(e as u32));
            }
            prop_assert_eq!(b.pos(), seq.iter().map(|&e| lengths[e] as usize).sum::<usize>());
        }

        /// review b11: the per-length binary search finds what the scan of every long entry
        /// found, at the same position, on encoded and on random bits (also underfull trees).
        #[test]
        fn search_matches_the_scan(splits in prop::collection::vec(0usize..1000, 1..400), drop in prop::collection::vec(0usize..1000, 0..3), picks in prop::collection::vec(0usize..10000, 1..100), noise in prop::collection::vec(any::<u8>(), 0..60)) {
            let mut lengths = full_tree(splits);
            for d in drop {
                if lengths.len() > 2 {
                    lengths.remove(d % lengths.len());
                }
            }
            let Ok(book) = Codebook::new(1, &lengths, vec![0.0; lengths.len()]) else { return Ok(()) };
            let words = make_words(&lengths).unwrap();
            let used: Vec<usize> = (0..lengths.len()).filter(|&i| lengths[i] > 0).collect();
            let seq: Vec<usize> = picks.iter().map(|p| used[p % used.len()]).collect();
            let mut data = encode(&lengths, &words, &seq);
            data.extend(noise);
            let (mut a, mut b) = (Bits::new(&data), Bits::new(&data));
            loop {
                let (x, y) = (book.decode(&mut a), book.decode_scan(&mut b));
                prop_assert_eq!(x, y);
                prop_assert_eq!(a.pos(), b.pos());
                if x.is_none() {
                    break;
                }
            }
        }

        /// Never panics and never reads past the packet, whatever the bits.
        #[test]
        fn garbage(splits in prop::collection::vec(0usize..1000, 1..100), data in prop::collection::vec(any::<u8>(), 0..40)) {
            let lengths = full_tree(splits);
            let book = Codebook::new(1, &lengths, vec![0.0; lengths.len()]).unwrap();
            let mut b = Bits::new(&data);
            while book.decode(&mut b).is_some() {}
            prop_assert!(b.pos() <= data.len() * 8);
        }
    }
}
