//! Ogg pages to packets (one logical stream; CRCs are not checked).

use std::collections::VecDeque;

/// The packets of the first logical stream in an Ogg file, and the stream's last granule
/// position.
pub fn packets(data: &[u8]) -> (Vec<Vec<u8>>, u64) {
    let mut r = Reader::default();
    r.push(data);
    r.finish();
    (r.out.into(), r.granule)
}

/// [`packets`] of bytes given a piece at a time (a movie's sound read as it plays): `push` the
/// next bytes, `pop` the packets they complete, `finish` at the end of the file.
#[derive(Clone, Default)]
pub struct Reader {
    /// bytes not yet taken into a page
    buf: Vec<u8>,
    /// the packet being put together (it can span pages)
    cur: Vec<u8>,
    serial: Option<u32>,
    granule: u64,
    out: VecDeque<Vec<u8>>,
    /// the packet being put together went past the limit: its bytes are dropped up to its end
    skipping: bool,
    /// the longest packet kept (None: MAX_PACKET)
    limit: Option<usize>,
}

/// The longest packet kept (a Vorbis packet is a few KiB, a setup header with big codebooks a
/// few hundred): a damaged or hostile stream whose lacing never ends a packet no longer
/// buffers the rest of the file; such a packet is dropped.
pub const MAX_PACKET: usize = 8 << 20;

impl Reader {
    /// A reader keeping packets of at most `limit` bytes.
    pub fn with_limit(limit: usize) -> Self {
        Reader { limit: Some(limit), ..Default::default() }
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        let used = self.pages(false);
        self.buf.drain(..used);
    }

    /// No more bytes: a page cut short gives what it has (as [`packets`] of the whole file).
    pub fn finish(&mut self) {
        let used = self.pages(true);
        self.buf.drain(..used);
    }

    pub fn pop(&mut self) -> Option<Vec<u8>> {
        self.out.pop_front()
    }

    /// The stream's last granule position so far.
    pub fn granule(&self) -> u64 {
        self.granule
    }

    /// Takes the whole pages in `buf` (and, at the end, one cut short); returns the bytes used.
    fn pages(&mut self, end: bool) -> usize {
        let data = &self.buf[..];
        let mut pos = 0;
        while pos + 27 <= data.len() {
            if &data[pos..pos + 4] != b"OggS" {
                pos += 1;
                continue;
            }
            let nseg = data[pos + 26] as usize;
            if pos + 27 + nseg > data.len() {
                break;
            }
            let lacing = &data[pos + 27..pos + 27 + nseg];
            let body: usize = lacing.iter().map(|&l| l as usize).sum();
            if !end && pos + 27 + nseg + body > data.len() {
                break;
            }
            let s = u32::from_le_bytes(data[pos + 14..pos + 18].try_into().unwrap());
            let g = u64::from_le_bytes(data[pos + 6..pos + 14].try_into().unwrap());
            let mut body = pos + 27 + nseg;
            let mine = *self.serial.get_or_insert(s) == s;
            for &l in lacing {
                let e = (body + l as usize).min(data.len());
                if mine {
                    if !self.skipping {
                        self.cur.extend_from_slice(&data[body..e]);
                        if self.cur.len() > self.limit.unwrap_or(MAX_PACKET) {
                            self.cur = Vec::new();
                            self.skipping = true;
                        }
                    }
                    if l < 255 {
                        if !std::mem::take(&mut self.skipping) {
                            self.out.push_back(std::mem::take(&mut self.cur));
                        }
                    }
                }
                body = e;
            }
            if mine && g != u64::MAX {
                self.granule = g;
            }
            pos = body;
        }
        pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The whole-file parser as it was before [`Reader`] (the reference).
    fn whole(data: &[u8]) -> (Vec<Vec<u8>>, u64) {
        whole_limited(data, usize::MAX)
    }

    /// The same, a packet longer than `limit` left out.
    fn whole_limited(data: &[u8], limit: usize) -> (Vec<Vec<u8>>, u64) {
        let mut out: Vec<Vec<u8>> = Vec::new();
        let mut cur = Vec::new();
        let mut pos = 0;
        let mut serial = None;
        let mut granule = 0;
        while pos + 27 <= data.len() {
            if &data[pos..pos + 4] != b"OggS" {
                pos += 1;
                continue;
            }
            let nseg = data[pos + 26] as usize;
            if pos + 27 + nseg > data.len() {
                break;
            }
            let s = u32::from_le_bytes(data[pos + 14..pos + 18].try_into().unwrap());
            let g = u64::from_le_bytes(data[pos + 6..pos + 14].try_into().unwrap());
            let lacing = &data[pos + 27..pos + 27 + nseg];
            let mut body = pos + 27 + nseg;
            let mine = *serial.get_or_insert(s) == s;
            for &l in lacing {
                let end = (body + l as usize).min(data.len());
                if mine {
                    cur.extend_from_slice(&data[body..end]);
                    if l < 255 {
                        let p = std::mem::take(&mut cur);
                        if p.len() <= limit {
                            out.push(p);
                        }
                    }
                }
                body = end;
            }
            if mine && g != u64::MAX {
                granule = g;
            }
            pos = body;
        }
        (out, granule)
    }

    /// An Ogg page: serial, granule, segments (lacing values and their bytes).
    fn page(serial: u32, granule: u64, lacing: &[u8], fill: u8) -> Vec<u8> {
        let mut p = b"OggS\0\0".to_vec();
        p.extend(granule.to_le_bytes());
        p.extend(serial.to_le_bytes());
        p.extend([0; 8]);
        p.push(lacing.len() as u8);
        p.extend(lacing);
        for &l in lacing {
            p.extend(std::iter::repeat_n(fill, l as usize));
        }
        p
    }

    fn file() -> impl Strategy<Value = Vec<u8>> {
        let pg = (0u32..2, any::<u64>(), prop::collection::vec(prop_oneof![0u8..=254, Just(255u8)], 0..6), any::<u8>(), prop::collection::vec(any::<u8>(), 0..3));
        prop::collection::vec(pg, 0..8).prop_map(|ps| {
            let mut d = Vec::new();
            for (s, g, l, f, junk) in ps {
                d.extend(page(s, g, &l, f));
                d.extend(junk);
            }
            d
        })
    }

    proptest! {
        /// Pieces of any sizes give what the whole file gives (pages, junk between them, a page
        /// cut short at the end, other streams' pages, packets over several pages).
        #[test]
        fn pieces(d in file(), cut in 0usize..40, sizes in prop::collection::vec(1usize..50, 1..20)) {
            let d = &d[..d.len().saturating_sub(cut)];
            let mut r = Reader::default();
            let mut got = Vec::new();
            let mut at = 0;
            for &n in sizes.iter().cycle() {
                if at >= d.len() {
                    break;
                }
                let e = (at + n).min(d.len());
                r.push(&d[at..e]);
                at = e;
                while let Some(p) = r.pop() {
                    got.push(p);
                }
            }
            r.finish();
            while let Some(p) = r.pop() {
                got.push(p);
            }
            let (want, g) = whole(d);
            prop_assert_eq!(got, want);
            prop_assert_eq!(r.granule(), g);
        }

        /// Any bytes: as the whole file.
        #[test]
        fn any_bytes(d in prop::collection::vec(any::<u8>(), 0..200)) {
            prop_assert_eq!(packets(&d), whole(&d));
        }

        /// review b11: with a packet limit, a packet longer than it is left out whole (the
        /// packets around it come as without the limit), however the bytes come, and the
        /// packet being put together never holds more than the limit and one page.
        #[test]
        fn long_packets_are_dropped(d in file(), limit in 0usize..600, sizes in prop::collection::vec(1usize..50, 1..20)) {
            let mut r = Reader::with_limit(limit);
            let mut got = Vec::new();
            let mut at = 0;
            for &n in sizes.iter().cycle() {
                if at >= d.len() {
                    break;
                }
                let e = (at + n).min(d.len());
                r.push(&d[at..e]);
                prop_assert!(r.cur.len() <= limit + 255 * 255);
                at = e;
                while let Some(p) = r.pop() {
                    got.push(p);
                }
            }
            r.finish();
            while let Some(p) = r.pop() {
                got.push(p);
            }
            let (want, _) = whole_limited(&d, limit);
            prop_assert_eq!(got, want);
        }
    }

    /// A stream whose lacing never ends a packet (all 255s) keeps none of it past the limit.
    #[test]
    fn endless_packet_is_not_buffered() {
        let mut r = Reader::with_limit(100_000);
        let lacing = [255u8; 255];
        for i in 0..40 {
            r.push(&page(1, i, &lacing, 7));
            assert!(r.cur.len() <= 100_000 + 255 * 255);
        }
        let mut end = page(1, 99, &[3], 1);
        end.extend(page(1, 100, &[2], 2));
        r.push(&end);
        r.finish();
        assert_eq!(r.pop(), Some(vec![2, 2]), "the long packet is dropped, the next one kept");
        assert_eq!(r.pop(), None);
    }
}
