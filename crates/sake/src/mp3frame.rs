//! MPEG audio layer III frames out of a byte stream (what msacm32's MP3 codec is given: any
//! slices of a file, or of a WAVE's data), one frame per chunk for the page's AudioDecoder
//! (crates/kura/src/adec.rs). Bytes that are not a frame (an ID3 tag, junk between frames) are
//! skipped as decoders do: a header is taken when the frame it describes is followed by another
//! header, or by the end of what is here.

/// A layer III frame header's facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub rate: u32,
    pub channels: u32,
    /// bytes of the whole frame (header included)
    pub len: usize,
    /// samples per channel it holds (1152 MPEG-1, 576 MPEG-2 / 2.5)
    pub samples: u32,
}

const V1: [u32; 15] = [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320];
const V2: [u32; 15] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160];

/// The header at the start of `b` (4 bytes), if it is one of a layer III frame this can split
/// (not free format).
pub fn header(b: &[u8]) -> Option<Header> {
    let b: [u8; 4] = b.get(..4)?.try_into().ok()?;
    if b[0] != 0xff || b[1] & 0xe0 != 0xe0 {
        return None;
    }
    let version = (b[1] >> 3) & 3; // 3 MPEG-1, 2 MPEG-2, 0 MPEG-2.5
    let layer = (b[1] >> 1) & 3; // 1 layer III
    let (bri, sri) = ((b[2] >> 4) as usize, ((b[2] >> 2) & 3) as usize);
    if version == 1 || layer != 1 || bri == 0 || bri == 15 || sri == 3 {
        return None;
    }
    let base = [44100, 48000, 32000][sri];
    let (rate, kbps, per) = match version {
        3 => (base, V1[bri], 144_000),
        2 => (base / 2, V2[bri], 72_000),
        _ => (base / 4, V2[bri], 72_000),
    };
    let pad = ((b[2] >> 1) & 1) as usize;
    let channels = if b[3] >> 6 == 3 { 1 } else { 2 };
    Some(Header {
        rate,
        channels,
        len: (per * kbps / rate) as usize + pad,
        samples: if version == 3 { 1152 } else { 576 },
    })
}

/// Frames as their bytes come.
#[derive(Default)]
pub struct Splitter {
    buf: Vec<u8>,
    /// an ID3v2 tag's bytes still to skip
    skip: usize,
    /// at the start of the stream (where an ID3v2 tag can be)
    started: bool,
}

impl Splitter {
    /// Adds `b`; whole frames out (each `Header` with its bytes). A frame whose end is here but
    /// with fewer than 4 bytes after it is let out (the stream's last, likely), not held.
    pub fn push(&mut self, b: &[u8], out: &mut Vec<(Header, Vec<u8>)>) {
        let skip = self.skip.min(b.len());
        self.skip -= skip;
        self.buf.extend_from_slice(&b[skip..]);
        let mut at = 0;
        loop {
            let rest = &self.buf[at..];
            if !self.started {
                // "ID3", version, flags, a 28-bit size (and a footer when flag 0x10)
                if rest.len() < 10 && b"ID3".starts_with(&rest[..rest.len().min(3)]) {
                    break;
                }
                self.started = true;
                if rest.starts_with(b"ID3") && rest[6..10].iter().all(|&x| x < 0x80) {
                    let size = rest[6..10].iter().fold(0usize, |a, &x| a << 7 | x as usize) + 10 + if rest[5] & 0x10 != 0 { 10 } else { 0 };
                    let here = size.min(rest.len());
                    at += here;
                    self.skip = size - here;
                    continue;
                }
            }
            if rest.len() < 4 {
                break;
            }
            let Some(h) = header(rest) else {
                at += 1;
                continue;
            };
            if rest.len() < h.len {
                break;
            }
            // (a header the next 4 bytes do not follow up is junk that looked like one)
            if rest.len() >= h.len + 4 && header(&rest[h.len..]).is_none() && !rest[h.len..].starts_with(b"TAG") {
                at += 1;
                continue;
            }
            out.push((h, rest[..h.len].to_vec()));
            at += h.len;
        }
        self.buf.drain(..at);
    }

    /// Starts over (a new stream).
    pub fn reset(&mut self) {
        *self = Splitter::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A frame: MPEG-1 layer III 44.1 kHz at `kbps` (V1 table index `bri`), stereo or mono,
    /// padded or not, its body bytes from `fill`.
    fn frame(bri: usize, pad: bool, mono: bool, fill: u8) -> Vec<u8> {
        let h = [0xff, 0xfb, (bri as u8) << 4 | (pad as u8) << 1, if mono { 0xc0 } else { 0x00 }];
        let len = 144_000 * V1[bri] as usize / 44100 + pad as usize;
        let mut f = h.to_vec();
        // (no 0xff in the body: no false header inside it)
        f.resize(len, fill & 0x7f);
        f
    }

    #[test]
    fn headers() {
        // 128 kbps 44.1 kHz stereo: 417 bytes, 418 padded
        assert_eq!(
            header(&[0xff, 0xfb, 0x90, 0x00]),
            Some(Header {
                rate: 44100,
                channels: 2,
                len: 417,
                samples: 1152
            })
        );
        assert_eq!(header(&[0xff, 0xfb, 0x92, 0xc0]).map(|h| (h.len, h.channels)), Some((418, 1)));
        // MPEG-2 22.05 kHz 64 kbps: 72000 * 64 / 22050 = 208; MPEG-2.5 11.025 kHz 32 kbps: 208
        assert_eq!(
            header(&[0xff, 0xf3, 0x80, 0x00]),
            Some(Header {
                rate: 22050,
                channels: 2,
                len: 208,
                samples: 576
            })
        );
        assert_eq!(header(&[0xff, 0xe3, 0x40, 0x00]).map(|h| (h.rate, h.len)), Some((11025, 208)));
        // free format, bad bitrate, reserved rate / version, layer II, no sync
        for b in [
            [0xff, 0xfb, 0x00, 0],
            [0xff, 0xfb, 0xf0, 0],
            [0xff, 0xfb, 0x9c, 0],
            [0xff, 0xeb, 0x90, 0],
            [0xff, 0xfd, 0x90, 0],
            [0xfe, 0xfb, 0x90, 0],
        ] {
            assert_eq!(header(&b), None, "{b:x?}");
        }
        assert_eq!(header(&[0xff, 0xfb]), None);
    }

    #[test]
    fn id3_and_trailing_tag() {
        let mut s = Vec::new();
        // ID3v2, 300 bytes of tag (size 0x0000022c = 300 as 7-bit groups: 2 << 7 | 0x2c)
        s.extend([b'I', b'D', b'3', 3, 0, 0, 0, 0, 2, 0x2c]);
        s.extend(std::iter::repeat_n(0xffu8, 300));
        let (a, b) = (frame(9, false, false, 1), frame(9, true, false, 2));
        s.extend(&a);
        s.extend(&b);
        s.extend(b"TAG");
        s.extend([0u8; 125]);
        let mut out = Vec::new();
        Splitter::default().push(&s, &mut out);
        assert_eq!(out.into_iter().map(|f| f.1).collect::<Vec<_>>(), [a, b]);
    }

    #[test]
    fn false_sync_skipped() {
        // junk holding a header whose "frame" is not followed by another
        let mut s = vec![0xff, 0xfb, 0x90, 0x00, 1, 2, 3];
        s.resize(600, 5);
        let a = frame(5, false, true, 3);
        s.extend(&a);
        s.extend(&a);
        let mut out = Vec::new();
        Splitter::default().push(&s, &mut out);
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|f| f.1 == a));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Frames, with an ID3 tag or junk before them, cut anywhere into pieces: the same frames
        /// come out as from the whole, in order, each once.
        #[test]
        fn any_cuts(spec in proptest::collection::vec((1usize..15, any::<bool>(), any::<bool>(), any::<u8>()), 1..12),
                    cuts in proptest::collection::vec(0usize..2000, 0..8), id3 in 0usize..50, junk in proptest::collection::vec(0u8..0xfe, 0..20)) {
            let frames: Vec<Vec<u8>> = spec.iter().map(|&(b, p, m, f)| frame(b, p, m, f)).collect();
            let mut s = Vec::new();
            if id3 > 0 {
                s.extend([b'I', b'D', b'3', 4, 0, 0, 0, 0, 0, id3 as u8]);
                s.extend(std::iter::repeat_n(0xffu8, id3));
            }
            s.extend(&junk);
            for f in &frames {
                s.extend(f);
            }
            let mut cuts: Vec<usize> = cuts.into_iter().map(|c| c % (s.len() + 1)).collect();
            cuts.sort();
            let mut sp = Splitter::default();
            let mut out = Vec::new();
            let mut last = 0;
            for c in cuts.into_iter().chain([s.len()]) {
                sp.push(&s[last..c], &mut out);
                last = c;
            }
            let got: Vec<Vec<u8>> = out.into_iter().map(|f| f.1).collect();
            prop_assert_eq!(got, frames);
        }

        /// Any bytes in any pieces: no panic, every frame out starts with a header of its length.
        #[test]
        fn any_bytes(b in proptest::collection::vec(any::<u8>(), 0..3000), cut in 0usize..3000) {
            let cut = cut.min(b.len());
            let mut sp = Splitter::default();
            let mut out = Vec::new();
            sp.push(&b[..cut], &mut out);
            sp.push(&b[cut..], &mut out);
            for (h, f) in out {
                prop_assert_eq!(header(&f), Some(h));
                prop_assert_eq!(f.len(), h.len);
            }
        }
    }
}
