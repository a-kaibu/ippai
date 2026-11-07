//! The `.vnv` movie file, read here (video.rs) and written by `ippai pack`
//! (crates/ippai/src/movie.rs includes this file): no dependencies.
//!
//! ```text
//! "VNV1" w h tracks                                  (u32 little endian)
//! per track: codec_len codec(utf-8) frames, then per frame: offset size key
//! the frames, at their offsets from the start of the file
//! optional (movies other than Siglus's): the sound (Ogg Vorbis), then the trailer:
//!   num den duration(u64, 100 ns) audio_off audio_len "VNVt"
//! ```
//! A track's frame k is the movie's frame k; codecs are WebCodecs strings (VP9), or
//! "raw-i420" (uncompressed, read without a page: tests under node).

/// What a `.vnv` says of itself (`info`).
/// A track's frames: offset, size, key frame.
pub type Frames = Vec<(u32, u32, bool)>;

#[derive(Clone, Debug, PartialEq)]
pub struct Info {
    pub w: u32,
    pub h: u32,
    /// per track: its codec and frames (offset, size, key frame)
    pub tracks: Vec<(String, Frames)>,
    /// from the trailer (movies other than Siglus's, crates/ippai/src/movie.rs): frames per
    /// second as a fraction, the movie's length in 100 ns units, and where its sound (Ogg
    /// Vorbis) is in the file
    pub rate: Option<(u32, u32)>,
    pub duration: u64,
    pub audio: Option<(usize, usize)>,
}

impl Info {
    /// Frames of the movie (of its shortest track).
    pub fn frames(&self) -> u32 {
        self.tracks.iter().map(|t| t.1.len() as u32).min().unwrap_or(0)
    }
}

/// The trailer's tag, the file's last 4 bytes: before it num den duration(u64) audio_off audio_len.
pub const TRAILER: &[u8; 4] = b"VNVt";
pub const TRAILER_LEN: usize = 28;

/// Reads the header, frame tables and trailer of `data`; None when it is not a `.vnv` or does
/// not hold together (a table or the sound past the end).
pub fn info(data: &[u8]) -> Option<Info> {
    let mut at = |o: u64, buf: &mut [u8]| {
        let o = o as usize;
        let Some(s) = o.checked_add(buf.len()).and_then(|e| data.get(o..e)) else { return false };
        buf.copy_from_slice(s);
        true
    };
    read_info(data.len() as u64, &mut at).map(|(i, _)| i)
}

/// `info` of a `.vnv` of `len` bytes read through `at` (fills the buffer from an offset; false
/// past the end), without its frames: also how many bytes the header and tables take (all the
/// page needs of it to decode, `kura::video`).
pub fn read_info(len: u64, at: &mut dyn FnMut(u64, &mut [u8]) -> bool) -> Option<(Info, usize)> {
    let mut head = [0u8; 16];
    if !at(0, &mut head) || &head[..4] != b"VNV1" {
        return None;
    }
    let u = |b: &[u8], o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let (w, h, nt) = (u(&head, 4), u(&head, 8), u(&head, 12));
    if w == 0 || h == 0 || w > 8192 || h > 8192 || !(1..=2).contains(&nt) {
        return None;
    }
    let mut o = 16u64;
    let mut tracks = Vec::new();
    for _ in 0..nt {
        let mut b = [0u8; 4];
        if !at(o, &mut b) {
            return None;
        }
        let cl = u32::from_le_bytes(b) as u64;
        // (a codec string is short: no huge allocation from a bad length)
        if cl > 256 {
            return None;
        }
        let mut c = vec![0u8; cl as usize + 4];
        if !at(o + 4, &mut c) {
            return None;
        }
        let codec = std::str::from_utf8(&c[..cl as usize]).ok()?.to_string();
        let n = u(&c, cl as usize) as u64;
        o += 8 + cl;
        if n > len.saturating_sub(o) / 12 {
            return None;
        }
        let mut t = vec![0u8; 12 * n as usize];
        if !at(o, &mut t) {
            return None;
        }
        let mut frames = Vec::with_capacity(n as usize);
        for e in t.chunks_exact(12) {
            let (off, size, key) = (u(e, 0), u(e, 4), u(e, 8));
            if off as u64 + size as u64 > len {
                return None;
            }
            frames.push((off, size, key != 0));
        }
        o += 12 * n;
        tracks.push((codec, frames));
    }
    let mut i = Info { w, h, tracks, rate: None, duration: 0, audio: None };
    let mut t = [0u8; TRAILER_LEN];
    if len >= o + TRAILER_LEN as u64 && at(len - TRAILER_LEN as u64, &mut t) && &t[24..] == TRAILER {
        let tpos = len - TRAILER_LEN as u64;
        let (num, den) = (u(&t, 0), u(&t, 4));
        let dur = u(&t, 8) as u64 | (u(&t, 12) as u64) << 32;
        let (ao, al) = (u(&t, 16) as u64, u(&t, 20) as u64);
        if num == 0 || den == 0 || ao + al > tpos {
            return None;
        }
        i.rate = Some((num, den));
        i.duration = dur;
        i.audio = (al > 0).then_some((ao as usize, al as usize));
    }
    Some((i, o as usize))
}

/// The size of an uncompressed I420 frame ("raw-i420") of a `w` x `h` movie (rounded up to
/// even sizes, as the encoder pads).
pub fn raw_size(w: u32, h: u32) -> usize {
    let (w, h) = (w.next_multiple_of(2) as usize, h.next_multiple_of(2) as usize);
    w * h + 2 * (w / 2) * (h / 2)
}


/// One track for `write`: its codec and frames (bytes, key frame).
pub type Track = (String, Vec<(Vec<u8>, bool)>);

/// The trailer for `write`: frames per second (num, den), length (100 ns), sound (Ogg Vorbis).
pub struct Trailer {
    pub rate: (u32, u32),
    pub duration: u64,
    pub audio: Vec<u8>,
}

/// Writes a `.vnv`'s header and frame tables for `tracks` (codec, then per frame its size and
/// whether it is a key frame; cut to the shortest track) to `out`; the caller writes the frames
/// next, track after track, and then, for the trailer, the sound and `write_tail`. Returns where
/// the frames end. Err when the file would not fit the format's 32-bit offsets (4 GiB).
pub fn write_head(out: &mut impl std::io::Write, w: u32, h: u32, tracks: &[(&str, &[(u32, bool)])]) -> std::io::Result<u64> {
    let n = tracks.iter().map(|t| t.1.len()).min().unwrap_or(0);
    let table: u64 = tracks.iter().map(|t| 8 + t.0.len() as u64 + 12 * n as u64).sum();
    let mut at = 16 + table;
    let mut head = Vec::with_capacity(at as usize);
    for v in [*b"VNV1", w.to_le_bytes(), h.to_le_bytes(), (tracks.len() as u32).to_le_bytes()] {
        head.extend(v);
    }
    for (codec, frames) in tracks {
        head.extend((codec.len() as u32).to_le_bytes());
        head.extend(codec.as_bytes());
        head.extend((n as u32).to_le_bytes());
        for &(size, key) in &frames[..n] {
            for v in [at as u32, size, key as u32] {
                head.extend(v.to_le_bytes());
            }
            at += size as u64;
        }
    }
    too_big(at)?;
    out.write_all(&head)?;
    Ok(at)
}

/// The trailer, after the sound (`audio_len` bytes at `audio_off`, where the frames end).
pub fn write_tail(out: &mut impl std::io::Write, rate: (u32, u32), duration: u64, audio_off: u64, audio_len: u64) -> std::io::Result<()> {
    too_big(audio_off + audio_len + TRAILER_LEN as u64)?;
    for v in [rate.0, rate.1, duration as u32, (duration >> 32) as u32, audio_off as u32, audio_len as u32] {
        out.write_all(&v.to_le_bytes())?;
    }
    out.write_all(TRAILER)
}

fn too_big(end: u64) -> std::io::Result<()> {
    if end > u32::MAX as u64 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("a .vnv holds at most 4 GiB ({end} bytes)")));
    }
    Ok(())
}

/// A `.vnv` of `tracks` (cut to the shortest one's frames), in memory (tests and small movies;
/// panics past 4 GiB).
pub fn write(w: u32, h: u32, tracks: &[Track], trailer: Option<&Trailer>) -> Vec<u8> {
    let n = tracks.iter().map(|t| t.1.len()).min().unwrap_or(0);
    let sizes: Vec<Vec<(u32, bool)>> = tracks.iter().map(|t| t.1[..n].iter().map(|(f, k)| (f.len() as u32, *k)).collect()).collect();
    let index: Vec<(&str, &[(u32, bool)])> = tracks.iter().zip(&sizes).map(|(t, s)| (t.0.as_str(), &s[..])).collect();
    let mut out = Vec::new();
    let end = write_head(&mut out, w, h, &index).expect("a .vnv in memory under 4 GiB");
    for (_, frames) in tracks {
        for (f, _) in &frames[..n] {
            out.extend_from_slice(f);
        }
    }
    debug_assert_eq!(out.len() as u64, end);
    if let Some(t) = trailer {
        out.extend_from_slice(&t.audio);
        write_tail(&mut out, t.rate, t.duration, end, t.audio.len() as u64).expect("a .vnv in memory under 4 GiB");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn tracks() -> impl Strategy<Value = Vec<Track>> {
        let frames = prop::collection::vec((prop::collection::vec(any::<u8>(), 0..40), any::<bool>()), 0..12);
        prop::collection::vec(("[a-z0-9.\\-]{0,12}", frames), 1..=2)
    }

    proptest! {
        /// What `write` writes, `info` reads back: sizes, codecs, every frame's bytes and key
        /// flag (tracks cut to the shortest), and the trailer.
        #[test]
        fn round_trip(w in 1u32..=8192, h in 1u32..=8192, t in tracks(), rate in (1u32.., 1u32..), duration: u64, audio in prop::collection::vec(any::<u8>(), 0..64), with_trailer: bool) {
            let tr = Trailer { rate, duration, audio: audio.clone() };
            let d = write(w, h, &t, with_trailer.then_some(&tr));
            let i = info(&d).unwrap();
            let n = t.iter().map(|x| x.1.len()).min().unwrap();
            prop_assert_eq!((i.w, i.h, i.tracks.len(), i.frames() as usize), (w, h, t.len(), n));
            for ((codec, frames), (c2, f2)) in t.iter().zip(&i.tracks) {
                prop_assert_eq!(codec, c2);
                for ((bytes, key), &(off, size, k2)) in frames.iter().zip(f2) {
                    prop_assert_eq!(&d[off as usize..(off + size) as usize], &bytes[..]);
                    prop_assert_eq!(*key, k2);
                }
            }
            if with_trailer {
                prop_assert_eq!(i.rate, Some(rate));
                prop_assert_eq!(i.duration, duration);
                match i.audio {
                    Some((o, l)) => prop_assert_eq!(&d[o..o + l], &audio[..]),
                    None => prop_assert!(audio.is_empty()),
                }
            } else {
                prop_assert_eq!((i.rate, i.duration, i.audio), (None, 0, None));
            }
        }

        /// Any bytes: no panic, and what is read stays inside them.
        #[test]
        fn any_bytes(mut d in prop::collection::vec(any::<u8>(), 0..300), head: bool, tail: bool) {
            if head && d.len() >= 16 {
                d[..4].copy_from_slice(b"VNV1");
                d[12] &= 1;
                d[12] += 1;
                d[13..16].fill(0);
            }
            if tail && d.len() >= 4 {
                let n = d.len();
                d[n - 4..].copy_from_slice(TRAILER);
            }
            if let Some(i) = info(&d) {
                for (_, f) in &i.tracks {
                    for &(o, s, _) in f {
                        prop_assert!(o as usize + s as usize <= d.len());
                    }
                }
                if let Some((o, l)) = i.audio {
                    prop_assert!(o + l <= d.len() - TRAILER_LEN);
                }
            }
        }

        /// A file cut short is either refused or read as one without its trailer (never with
        /// frames past its end).
        #[test]
        fn truncated(t in tracks(), cut in 0usize..400) {
            let tr = Trailer { rate: (30, 1), duration: 10_000_000, audio: vec![7; 20] };
            let d = write(64, 48, &t, Some(&tr));
            let c = &d[..cut.min(d.len())];
            if let Some(i) = info(c) {
                if c.len() < d.len() {
                    prop_assert_eq!(i.rate, None);
                }
                for (_, f) in &i.tracks {
                    for &(o, s, _) in f {
                        prop_assert!(o as usize + s as usize <= c.len());
                    }
                }
            }
        }
    }

    proptest! {
        /// Read through a reader of the file, the same as from all of it in memory, and the
        /// header the page gets is everything before the first frame.
        #[test]
        fn read_through(t in tracks(), with_trailer: bool) {
            let tr = Trailer { rate: (30, 1), duration: 5, audio: vec![1, 2, 3] };
            let d = write(16, 16, &t, with_trailer.then_some(&tr));
            let mut reads = 0;
            let mut at = |o: u64, b: &mut [u8]| {
                reads += 1;
                match d.get(o as usize..o as usize + b.len()) {
                    Some(s) => { b.copy_from_slice(s); true }
                    None => false,
                }
            };
            let (i, head) = read_info(d.len() as u64, &mut at).unwrap();
            prop_assert_eq!(&i, &info(&d).unwrap());
            let first = i.tracks.iter().flat_map(|t| t.1.iter().map(|f| f.0 as usize)).min().unwrap_or(head);
            prop_assert_eq!(head, first);
            // (a few reads per track, not one per frame)
            prop_assert!(reads <= 2 + 3 * i.tracks.len());
        }
    }

    #[test]
    fn four_gib() {
        // a frame table pointing past 4 GiB is refused, not cut short
        let big = [(u32::MAX, true), (16, false)];
        assert!(write_head(&mut Vec::new(), 2, 2, &[("vp09", &big[..])]).is_err());
        assert!(write_tail(&mut Vec::new(), (30, 1), 0, u32::MAX as u64 - 10, 20).is_err());
        assert!(write_head(&mut Vec::new(), 2, 2, &[("vp09", &[(1000, true)][..])]).is_ok());
    }

    #[test]
    fn trailer_fields() {
        let tr = Trailer { rate: (30000, 1001), duration: 0x1_0000_0005, audio: b"OggS".to_vec() };
        let d = write(2, 2, &[("raw-i420".into(), vec![(vec![0; 6], true)])], Some(&tr));
        let i = info(&d).unwrap();
        assert_eq!(i.rate, Some((30000, 1001)));
        assert_eq!(i.duration, 0x1_0000_0005);
        assert_eq!(&d[i.audio.unwrap().0..][..4], b"OggS");
        // a zero rate is no movie
        let bad = write(2, 2, &[("raw-i420".into(), vec![])], Some(&Trailer { rate: (0, 1), duration: 0, audio: vec![] }));
        assert!(info(&bad).is_none());
        // (odd sizes padded to even: 4x4 luma, two 2x2 chroma planes)
        assert_eq!(raw_size(3, 3), 24);
        assert_eq!(raw_size(4, 2), 12);
    }
}
