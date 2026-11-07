//! Content-defined chunking (FastCDC, normalized chunking level 2) for the store's trees: a
//! file is cut where a rolling gear hash of its bytes says, so the same bytes at another offset
//! (an archive with a file added in front, a fan disc that repacks the main game's images) cut
//! the same chunks, which the store keeps once. Measured on 43 titles (188 GB): fixed chunks of
//! any size from 256 KiB to 16 MiB left 94% of the bytes, this 88%.
//!
//! The cut points are part of the stored format (a chunk's name is the hash of its bytes, so
//! other cuts are other chunks): the gear table and the masks below must not change.
use std::io::Read;

/// Sizes of the chunks: none shorter than `min` (but a file's last), none longer than `max`,
/// about `avg` on average.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Params {
    pub min: u64,
    pub avg: u64,
    pub max: u64,
}

impl Params {
    /// min avg/4, max avg*4 (avg a power of two, at least 64 KiB).
    pub fn with_avg(avg: u64) -> Params {
        let avg = avg.max(64 << 10).next_power_of_two();
        Params { min: avg / 4, avg, max: avg * 4 }
    }
}

/// The gear table: splitmix64 from 0x9E3779B97F4A7C15, 256 values.
static GEAR: std::sync::LazyLock<[u64; 256]> = std::sync::LazyLock::new(|| {
    let mut t = [0u64; 256];
    let mut x: u64 = 0x9E3779B97F4A7C15;
    for v in t.iter_mut() {
        x = x.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        *v = z ^ (z >> 31);
    }
    t
});

/// A mask of `bits` ones among the hash's high bits: every other bit from bit 63 down, then the
/// ones between.
fn mask(bits: u32) -> u64 {
    let mut m = 0u64;
    let mut placed = 0;
    for start in [63i32, 62] {
        let mut i = start;
        while placed < bits && i >= 0 {
            if m & (1 << i) == 0 {
                m |= 1 << i;
                placed += 1;
            }
            i -= 2;
        }
    }
    m
}

/// Cuts a stream into chunks: feed it bytes, it tells where chunks end.
pub struct Cutter {
    p: Params,
    /// harder to match before avg, easier after (normalized chunking)
    small: u64,
    large: u64,
    len: u64,
    h: u64,
}

impl Cutter {
    pub fn new(p: Params) -> Cutter {
        let bits = p.avg.trailing_zeros();
        Cutter { p, small: mask(bits + 2), large: mask(bits.saturating_sub(2)), len: 0, h: 0 }
    }

    /// Where in `buf` the current chunk ends (its length so far plus this many bytes), if it
    /// does; the bytes after it start the next chunk (feed them again).
    pub fn next(&mut self, buf: &[u8]) -> Option<usize> {
        let mut i = 0;
        // (no cut before min: the hash is not even kept there)
        if self.len < self.p.min {
            let skip = ((self.p.min - self.len) as usize).min(buf.len());
            self.len += skip as u64;
            i = skip;
            if self.len < self.p.min {
                return None;
            }
        }
        let g = &*GEAR;
        let mut h = self.h;
        while i < buf.len() {
            h = (h << 1).wrapping_add(g[buf[i] as usize]);
            self.len += 1;
            i += 1;
            let m = if self.len < self.p.avg { self.small } else { self.large };
            if h & m == 0 || self.len >= self.p.max {
                self.len = 0;
                self.h = 0;
                return Some(i);
            }
        }
        self.h = h;
        None
    }
}

/// The lengths of the chunks of `r`'s bytes (the last one what is left; none for nothing).
pub fn lengths(r: &mut impl Read, p: Params) -> std::io::Result<Vec<u64>> {
    let mut out = Vec::new();
    let mut c = Cutter::new(p);
    let mut buf = vec![0u8; 8 << 20];
    let mut cur = 0u64;
    loop {
        let n = match r.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let mut at = 0;
        while at < n {
            match c.next(&buf[at..n]) {
                Some(k) => {
                    out.push(cur + k as u64);
                    cur = 0;
                    at += k;
                }
                None => {
                    cur += (n - at) as u64;
                    at = n;
                }
            }
        }
    }
    if cur > 0 {
        out.push(cur);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(n: usize, seed: u64) -> Vec<u8> {
        let mut x = seed;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                x as u8
            })
            .collect()
    }

    /// Lengths add up, stay within min..=max (but the last), average near avg; the same however
    /// the bytes are fed; and they are pinned: other cut points would be other chunks.
    #[test]
    fn cuts() {
        let p = Params::with_avg(64 << 10);
        let data = bytes(8 << 20, 1);
        let l = lengths(&mut &data[..], p).unwrap();
        assert_eq!(l.iter().sum::<u64>(), data.len() as u64);
        for &x in &l[..l.len() - 1] {
            assert!((p.min..=p.max).contains(&x), "{x}");
        }
        let avg = data.len() as f64 / l.len() as f64;
        assert!((40_000.0..100_000.0).contains(&avg), "{avg}");
        // fed in odd pieces: the same cuts
        let mut c = Cutter::new(p);
        let mut got = Vec::new();
        let (mut cur, mut at) = (0u64, 0usize);
        for piece in data.chunks(7919) {
            let mut i = 0;
            while i < piece.len() {
                match c.next(&piece[i..]) {
                    Some(k) => {
                        got.push(cur + k as u64);
                        cur = 0;
                        i += k;
                    }
                    None => {
                        cur += (piece.len() - i) as u64;
                        i = piece.len();
                    }
                }
            }
            at += piece.len();
        }
        if cur > 0 {
            got.push(cur);
        }
        assert_eq!(at, data.len());
        assert_eq!(got, l);
        // pinned (the format): the first cuts of these bytes
        assert_eq!(&l[..4], &PINNED[..], "the cut points changed: chunks packed before would not be found again ({:?})", &l[..4]);
    }

    const PINNED: [u64; 4] = [82247, 81550, 67390, 80966];

    /// The same bytes after an insertion cut the same chunks again (what fixed chunks lose).
    #[test]
    fn shifted_bytes_cut_the_same() {
        let p = Params::with_avg(64 << 10);
        let data = bytes(4 << 20, 2);
        let mut shifted = bytes(12345, 3);
        shifted.extend_from_slice(&data);
        let cut = |d: &[u8]| {
            let mut at = 0usize;
            lengths(&mut &d[..], p)
                .unwrap()
                .into_iter()
                .map(|n| {
                    let c = blake3::hash(&d[at..at + n as usize]);
                    at += n as usize;
                    c
                })
                .collect::<std::collections::HashSet<_>>()
        };
        let (a, b) = (cut(&data), cut(&shifted));
        let common = a.intersection(&b).count();
        assert!(common * 10 >= a.len() * 9, "{common} of {} chunks found again", a.len());
    }

    #[test]
    fn short_and_empty() {
        let p = Params::with_avg(64 << 10);
        assert!(lengths(&mut &[][..], p).unwrap().is_empty());
        assert_eq!(lengths(&mut &[7u8; 100][..], p).unwrap(), vec![100]);
        let zero = lengths(&mut &vec![0u8; 1 << 20][..], p).unwrap();
        assert_eq!(zero.iter().sum::<u64>(), 1 << 20);
    }
}
