//! Snappy decompression (the raw block format, github.com/google/snappy format_description.txt):
//! apitrace's .trace file is "at" and then chunks of a u32 LE length and one snappy block each
//! (lib/trace/trace_file_snappy.cpp).

/// One snappy block -> its bytes
pub fn decompress(src: &[u8]) -> Result<Vec<u8>, String> {
    let (out, n) = decode(src)?;
    if out.len() != n {
        return Err(format!("snappy: {} bytes, the header says {n}", out.len()));
    }
    Ok(out)
}

/// The bytes a block cut short still holds (a trace's last chunk, written when the program was
/// killed): those of its whole literals and copies, a prefix of the whole block's; a copy from
/// before the start is still an error.
pub fn decompress_prefix(src: &[u8]) -> Result<Vec<u8>, String> {
    let mut i = 0;
    if varint(src, &mut i).is_err() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    match step_all(src, i, &mut out) {
        Err(Cut::Short) | Ok(()) => Ok(out),
        Err(Cut::Bad(e)) => Err(e),
    }
}

enum Cut {
    Short,
    Bad(String),
}

fn decode(src: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let mut i = 0;
    let n = varint(src, &mut i)? as usize;
    let mut out = Vec::with_capacity(n.min(1 << 26));
    match step_all(src, i, &mut out) {
        Ok(()) => Ok((out, n)),
        Err(Cut::Short) => Err("snappy: past the end".into()),
        Err(Cut::Bad(e)) => Err(e),
    }
}

/// the elements from src[i..] onto out; an element the bytes stop inside is Cut::Short, and
/// out keeps the elements before it
fn step_all(src: &[u8], mut i: usize, out: &mut Vec<u8>) -> Result<(), Cut> {
    let le = |i: usize, k: usize| src.get(i..i + k).map(|b| b.iter().rev().fold(0u64, |v, &x| v << 8 | x as u64)).ok_or(Cut::Short);
    while i < src.len() {
        let tag = src[i];
        i += 1;
        let (len, off) = match tag & 3 {
            0 => {
                let mut len = (tag >> 2) as usize;
                if len >= 60 {
                    let k = len - 59;
                    len = le(i, k)? as usize;
                    i += k;
                }
                let len = len + 1;
                let lit = src.get(i..i.checked_add(len).ok_or(Cut::Short)?).ok_or(Cut::Short)?;
                out.extend_from_slice(lit);
                i += len;
                continue;
            }
            1 => {
                let b = *src.get(i).ok_or(Cut::Short)? as usize;
                i += 1;
                (4 + ((tag >> 2) & 7) as usize, ((tag as usize >> 5) << 8) | b)
            }
            2 => {
                let o = le(i, 2)? as usize;
                i += 2;
                ((tag >> 2) as usize + 1, o)
            }
            _ => {
                let o = le(i, 4)? as usize;
                i += 4;
                ((tag >> 2) as usize + 1, o)
            }
        };
        if off == 0 || off > out.len() {
            return Err(Cut::Bad(format!("snappy: copy offset {off} with {} bytes out", out.len())));
        }
        // (the copy may overlap what it writes: then in runs of the offset, each a copy of
        // bytes already out)
        let start = out.len() - off;
        let mut left = len;
        while left > 0 {
            let k = left.min(off);
            out.extend_from_within(start..start + k);
            left -= k;
        }
    }
    Ok(())
}

fn varint(src: &[u8], i: &mut usize) -> Result<u64, String> {
    let mut v = 0u64;
    for shift in (0..64).step_by(7) {
        let b = *src.get(*i).ok_or("snappy: length past the end")?;
        *i += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(v);
        }
    }
    Err("snappy: length too long".into())
}

/// A valid snappy block of `data` (literals and copies of earlier bytes found by a small hash
/// table, every tag kind the format has): for the tests and for writing traces in them.
pub fn compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut n = data.len() as u64;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
    let mut table = vec![usize::MAX; 1 << 12];
    let (mut i, mut lit) = (0, 0);
    let hash = |p: &[u8]| (u32::from_le_bytes([p[0], p[1], p[2], p[3]]).wrapping_mul(0x1e35a7bd) >> 20) as usize;
    while i + 4 <= data.len() {
        let h = hash(&data[i..]);
        let cand = table[h];
        table[h] = i;
        if cand != usize::MAX && data[cand..cand + 4] == data[i..i + 4] {
            let mut len = 4;
            while i + len < data.len() && data[cand + len] == data[i + len] {
                len += 1;
            }
            literal(&mut out, &data[lit..i]);
            let off = i - cand;
            let mut rest = len;
            while rest > 0 {
                let l = rest.min(64);
                // (a 1-byte-offset copy holds 4..=11 bytes)
                if (4..=11).contains(&l) && off < 2048 {
                    out.push(1 | ((l - 4) as u8) << 2 | ((off >> 8) as u8) << 5);
                    out.push(off as u8);
                } else if off < 65536 {
                    out.push(2 | ((l - 1) as u8) << 2);
                    out.extend_from_slice(&(off as u16).to_le_bytes());
                } else {
                    out.push(3 | ((l - 1) as u8) << 2);
                    out.extend_from_slice(&(off as u32).to_le_bytes());
                }
                rest -= l;
            }
            i += len;
            lit = i;
        } else {
            i += 1;
        }
    }
    literal(&mut out, &data[lit..]);
    out
}

fn literal(out: &mut Vec<u8>, lit: &[u8]) {
    for part in lit.chunks(1 << 20) {
        let n = part.len() - 1;
        if n < 60 {
            out.push((n as u8) << 2);
        } else {
            let bytes = (64 - (n as u64).leading_zeros() as usize).div_ceil(8);
            out.push(((59 + bytes) as u8) << 2);
            out.extend_from_slice(&(n as u32).to_le_bytes()[..bytes]);
        }
        out.extend_from_slice(part);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn spec_examples() {
        // literal "abc" then a 2-byte-offset copy of 6 bytes at offset 3 (overlapping)
        assert_eq!(decompress(&[9, 2 << 2, b'a', b'b', b'c', 2 | 5 << 2, 3, 0]).unwrap(), b"abcabcabc");
        // 1-byte-offset copy: 4 bytes at offset 1 (a run)
        assert_eq!(decompress(&[5, 0, b'x', 1, 1]).unwrap(), b"xxxxx");
        // 4-byte-offset copy
        assert_eq!(decompress(&[4, 1 << 2, b'h', b'i', 3 | 1 << 2, 2, 0, 0, 0]).unwrap(), b"hihi");
        // a literal with a 1-byte length (60 = 61..=256 bytes)
        let mut v = vec![100, 60 << 2, 99];
        v.extend(std::iter::repeat_n(7u8, 100));
        assert_eq!(decompress(&v).unwrap(), vec![7u8; 100]);
    }

    #[test]
    fn bad_blocks_are_errors() {
        assert!(decompress(&[3, 0, b'a']).is_err()); // short
        assert!(decompress(&[4, 1, 5]).is_err()); // copy before any byte
        assert!(decompress(&[2, 4 << 2, b'a']).is_err()); // literal past the end
        assert!(decompress(&[0x80]).is_err());
    }

    proptest! {
        #[test]
        fn roundtrip(data in proptest::collection::vec(prop_oneof![Just(0u8), Just(1u8), any::<u8>()], 0..5000)) {
            prop_assert_eq!(decompress(&compress(&data)).unwrap(), data);
        }

        #[test]
        fn roundtrip_runs(parts in proptest::collection::vec((any::<u8>(), 1usize..300), 0..40)) {
            let data: Vec<u8> = parts.iter().flat_map(|&(b, n)| std::iter::repeat_n(b, n)).collect();
            prop_assert_eq!(decompress(&compress(&data)).unwrap(), data);
        }

        #[test]
        fn garbage_never_panics(data in proptest::collection::vec(any::<u8>(), 0..200)) {
            let _ = decompress(&data);
            let _ = decompress_prefix(&data);
        }

        // (a block cut at any byte: a prefix of its bytes, all of them when whole, more the
        // more of it there is; one cut where an element ends loses nothing before it)
        #[test]
        fn a_cut_block_is_a_prefix(parts in proptest::collection::vec((any::<u8>(), 1usize..80, any::<bool>()), 0..30)) {
            let data: Vec<u8> = parts.iter().flat_map(|&(b, n, run)| (0..n).map(move |k| if run { b } else { b.wrapping_add((k as u8).wrapping_mul(31)) })).collect();
            let z = compress(&data);
            let mut last = 0;
            for n in 0..=z.len() {
                let got = decompress_prefix(&z[..n]).unwrap();
                prop_assert!(data.starts_with(&got));
                prop_assert!(got.len() >= last);
                last = got.len();
            }
            prop_assert_eq!(decompress_prefix(&z).unwrap(), data.clone());
        }
    }

    #[test]
    fn a_cut_block_spec() {
        // literal "abc", copy of 6: cut inside the copy keeps "abc"; inside the literal, nothing
        let b = [9, 2 << 2, b'a', b'b', b'c', 2 | 5 << 2, 3, 0];
        assert_eq!(decompress_prefix(&b[..6]).unwrap(), b"abc");
        assert_eq!(decompress_prefix(&b[..7]).unwrap(), b"abc");
        assert_eq!(decompress_prefix(&b[..4]).unwrap(), b"");
        assert_eq!(decompress_prefix(&b).unwrap(), b"abcabcabc");
        assert_eq!(decompress_prefix(&[]).unwrap(), b"");
        assert_eq!(decompress_prefix(&[0x80]).unwrap(), b"");
        // (a bad copy is still an error, cut or not; a cut block is not a whole one)
        assert!(decompress_prefix(&[4, 1, 5]).is_err());
        assert!(decompress(&b[..6]).is_err());
    }
}
