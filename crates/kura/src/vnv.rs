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
    if data.len() < 16 || &data[..4] != b"VNV1" {
        return None;
    }
    let u = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(data.get(o..o.checked_add(4)?)?.try_into().unwrap())) };
    let (w, h, nt) = (u(4)?, u(8)?, u(12)?);
    if w == 0 || h == 0 || w > 8192 || h > 8192 || !(1..=2).contains(&nt) {
        return None;
    }
    let mut o = 16usize;
    let mut tracks = Vec::new();
    for _ in 0..nt {
        let cl = u(o)? as usize;
        let codec = std::str::from_utf8(data.get(o + 4..(o + 4).checked_add(cl)?)?).ok()?.to_string();
        o += 4 + cl;
        let n = u(o)? as usize;
        o += 4;
        if n > (data.len() - o.min(data.len())) / 12 {
            return None;
        }
        let mut frames = Vec::with_capacity(n);
        for k in 0..n {
            let (off, size, key) = (u(o + 12 * k)?, u(o + 12 * k + 4)?, u(o + 12 * k + 8)?);
            if (off as u64 + size as u64) > data.len() as u64 {
                return None;
            }
            frames.push((off, size, key != 0));
        }
        o += 12 * n;
        tracks.push((codec, frames));
    }
    let mut i = Info { w, h, tracks, rate: None, duration: 0, audio: None };
    if data.len() >= o + TRAILER_LEN && &data[data.len() - 4..] == TRAILER {
        let t = data.len() - TRAILER_LEN;
        let (num, den) = (u(t)?, u(t + 4)?);
        let dur = u(t + 8)? as u64 | (u(t + 12)? as u64) << 32;
        let (ao, al) = (u(t + 16)? as usize, u(t + 20)? as usize);
        if num == 0 || den == 0 || ao.checked_add(al)? > t {
            return None;
        }
        i.rate = Some((num, den));
        i.duration = dur;
        i.audio = (al > 0).then_some((ao, al));
    }
    Some(i)
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

/// A `.vnv` of `tracks` (cut to the shortest one's frames).
pub fn write(w: u32, h: u32, tracks: &[Track], trailer: Option<&Trailer>) -> Vec<u8> {
    let n = tracks.iter().map(|t| t.1.len()).min().unwrap_or(0);
    let mut out = Vec::new();
    for v in [*b"VNV1", w.to_le_bytes(), h.to_le_bytes(), (tracks.len() as u32).to_le_bytes()] {
        out.extend(v);
    }
    let table: usize = tracks.iter().map(|t| 8 + t.0.len() + 12 * n).sum();
    let mut at = out.len() + table;
    for (codec, frames) in tracks {
        out.extend((codec.len() as u32).to_le_bytes());
        out.extend(codec.as_bytes());
        out.extend((n as u32).to_le_bytes());
        for (f, key) in &frames[..n] {
            for v in [at as u32, f.len() as u32, *key as u32] {
                out.extend(v.to_le_bytes());
            }
            at += f.len();
        }
    }
    for (_, frames) in tracks {
        for (f, _) in &frames[..n] {
            out.extend_from_slice(f);
        }
    }
    if let Some(t) = trailer {
        let ao = out.len();
        out.extend_from_slice(&t.audio);
        for v in [t.rate.0, t.rate.1, t.duration as u32, (t.duration >> 32) as u32, ao as u32, t.audio.len() as u32] {
            out.extend(v.to_le_bytes());
        }
        out.extend(TRAILER);
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
