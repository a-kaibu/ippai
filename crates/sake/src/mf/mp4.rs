//! MP4 (ISO BMFF) files as Media Foundation's MPEG-4 source opens them for the Source Reader:
//! the tracks in the file's order, H.264 video (`avc1` / `avc3`) and sound (AAC / MP3, read by
//! crates/sake/src/audiofile.rs), each sample's place in the file and its times.
//!
//! Only the `moov` box is read whole; samples are read as they are wanted, so a movie inside a
//! game's archive (Unity's .resource files, through the game's own IMFByteStream) is not
//! copied. H.264 goes to the page's VideoDecoder as Annex B (start codes; the SPS and PPS of
//! the `avcC` before each key frame), so the page needs no description.

use crate::audiofile;

/// A video sample: where it is, its presentation and decode times and duration (100 ns), and
/// whether it is a sync sample.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub off: u64,
    pub size: u32,
    pub pts: i64,
    pub dts: i64,
    pub dur: u64,
    pub key: bool,
    /// its place in presentation order
    pub order: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Video {
    /// WebCodecs' codec string ("avc1.4d4029")
    pub codec: String,
    pub w: u32,
    pub h: u32,
    /// avcC's profile and level (MF_MT_MPEG2_PROFILE / LEVEL)
    pub profile: u32,
    pub level: u32,
    /// bytes of a NAL unit's length (1, 2 or 4)
    pub nal: u8,
    /// the SPS and PPS as Annex B (MF_MT_MPEG_SEQUENCE_HEADER, and before each key frame)
    pub head: Vec<u8>,
    /// in decode order
    pub samples: Vec<Sample>,
    /// frames per second (MF_MT_FRAME_RATE): from the samples' mean duration as
    /// MFAverageTimePerFrameToFrameRate gives it
    pub rate: (u32, u32),
    /// bits per second (MF_MT_AVG_BITRATE)
    pub bitrate: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Track {
    Video(Video),
    Audio(audiofile::Track),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Movie {
    pub tracks: Vec<Track>,
    /// mvhd's duration (MF_PD_DURATION)
    pub duration: Option<u64>,
}

fn u16be(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn u32be(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn u64be(b: &[u8], o: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(o..o + 8)?.try_into().ok()?))
}

/// At most this much `moov` is read (a bad size: no huge allocation).
const MOOV_MAX: u64 = 64 << 20;

/// Opens the MP4 file of `len` bytes that `at(offset, n)` reads (None: it could not); None when
/// it is not one or holds no track we read.
pub fn open(len: u64, at: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>) -> Option<Movie> {
    let mut o = 0u64;
    let mut first = true;
    while o + 8 <= len {
        let h = at(o, 16.min((len - o) as usize))?;
        let size = u32be(&h, 0)? as u64;
        let kind = &h[4..8];
        if first && kind != b"ftyp" {
            return None;
        }
        first = false;
        let (body, end) = match size {
            0 => (o + 8, len),
            1 => (o + 16, o.checked_add(u64be(&h, 8)?)?),
            s if s >= 8 => (o + 8, o + s),
            _ => return None,
        };
        if end > len || end <= o {
            return None;
        }
        if kind == b"moov" {
            if end - o > MOOV_MAX {
                return None;
            }
            let m = at(o, (end - o) as usize)?;
            return parse(&m, (body - o) as usize, len);
        }
        o = end;
    }
    None
}

/// The movie whose `moov` box is `b` (its body from `ms`), in a file of `file_len` bytes.
pub fn parse(b: &[u8], ms: usize, file_len: u64) -> Option<Movie> {
    use audiofile::{boxes, child};
    let me = b.len();
    let mut tracks = Vec::new();
    for (kind, ts, te) in boxes(b, ms, me) {
        if &kind != b"trak" {
            continue;
        }
        let Some((ds, de)) = child(b, ts, te, b"mdia") else { continue };
        let Some((hs, _)) = child(b, ds, de, b"hdlr") else { continue };
        let Some((mds, _)) = child(b, ds, de, b"mdhd") else { continue };
        let timescale = if b.get(mds) == Some(&1) { u32be(b, mds + 20) } else { u32be(b, mds + 12) };
        let Some(timescale) = timescale.filter(|&t| t != 0) else { continue };
        let Some((ns, ne)) = child(b, ds, de, b"minf") else { continue };
        let Some((ss, se)) = child(b, ns, ne, b"stbl") else { continue };
        match b.get(hs + 8..hs + 12) {
            Some(b"soun") => {
                if let Some(t) = audiofile::sound_track(b, ss, se, timescale, file_len) {
                    tracks.push(Track::Audio(t));
                }
            }
            Some(b"vide") => {
                let shift = edit_shift(b, ts, te);
                if let Some(v) = video_track(b, ss, se, timescale, shift, file_len) {
                    tracks.push(Track::Video(v));
                }
            }
            _ => {}
        }
    }
    if tracks.is_empty() {
        return None;
    }
    let duration = audiofile::movie_duration(b, ms, me);
    for t in &mut tracks {
        if let Track::Audio(a) = t {
            a.duration = duration;
        }
    }
    Some(Movie { tracks, duration })
}

/// The edit list's first media time (where presentation starts, in the track's timescale): what
/// the decoder's reordering delay (ctts) is taken back by.
fn edit_shift(b: &[u8], ts: usize, te: usize) -> i64 {
    use audiofile::child;
    let Some((es, ee)) = child(b, ts, te, b"edts") else { return 0 };
    let Some((ls, _)) = child(b, es, ee, b"elst") else { return 0 };
    let v1 = b.get(ls) == Some(&1);
    let n = u32be(b, ls + 4).unwrap_or(0) as usize;
    for i in 0..n.min(16) {
        let t = if v1 { u64be(b, ls + 8 + 20 * i + 8).map(|v| v as i64) } else { u32be(b, ls + 8 + 12 * i + 4).map(|v| v as i32 as i64) };
        match t {
            // (-1: an empty edit, a delay before the media)
            Some(-1) => continue,
            Some(t) => return t,
            None => return 0,
        }
    }
    0
}

fn video_track(b: &[u8], ss: usize, se: usize, timescale: u32, shift: i64, file_len: u64) -> Option<Video> {
    use audiofile::{boxes, child};
    let (sds, sde) = child(b, ss, se, b"stsd")?;
    let (kind, es, ee) = *boxes(b, sds + 8, sde).first()?;
    if &kind != b"avc1" && &kind != b"avc3" {
        return None;
    }
    // VisualSampleEntry: 6 reserved, data reference (2), 16 more, width (2), height (2), then
    // 50 bytes to its boxes
    let (w, h) = (u16be(b, es + 24)? as u32, u16be(b, es + 26)? as u32);
    let (cs, ce) = child(b, es + 78, ee, b"avcC")?;
    let c = b.get(cs..ce)?;
    if c.len() < 7 || c[0] != 1 {
        return None;
    }
    let nal = (c[4] & 3) + 1;
    if nal == 3 {
        return None;
    }
    let mut head = Vec::new();
    let mut p = 5;
    for (count_mask, _) in [(0x1f, "sps"), (0xff, "pps")] {
        let n = (*c.get(p)? & count_mask) as usize;
        p += 1;
        for _ in 0..n {
            let l = u16be(c, p)? as usize;
            head.extend_from_slice(&[0, 0, 0, 1]);
            head.extend_from_slice(c.get(p + 2..p + 2 + l)?);
            p += 2 + l;
        }
    }
    // the sample table: sizes, chunk offsets, samples per chunk, durations, composition offsets,
    // sync samples
    let (zs, _) = child(b, ss, se, b"stsz")?;
    let (fixed, count) = (u32be(b, zs + 4)?, u32be(b, zs + 8)? as usize);
    if count == 0 || count > b.len() {
        return None;
    }
    let sizes: Vec<u32> = if fixed != 0 { vec![fixed; count] } else { (0..count).map(|i| u32be(b, zs + 12 + 4 * i)).collect::<Option<_>>()? };
    let chunks: Vec<u64> = if let Some((cs, _)) = child(b, ss, se, b"stco") {
        let n = u32be(b, cs + 4)? as usize;
        (0..n.min(b.len())).map(|i| u32be(b, cs + 8 + 4 * i).map(|v| v as u64)).collect::<Option<_>>()?
    } else {
        let (cs, _) = child(b, ss, se, b"co64")?;
        let n = u32be(b, cs + 4)? as usize;
        (0..n.min(b.len())).map(|i| u64be(b, cs + 8 + 8 * i)).collect::<Option<_>>()?
    };
    let (scs, _) = child(b, ss, se, b"stsc")?;
    let runs: Vec<(u32, u32)> = (0..(u32be(b, scs + 4)? as usize).min(b.len())).map(|i| Some((u32be(b, scs + 8 + 12 * i)?, u32be(b, scs + 12 + 12 * i)?))).collect::<Option<_>>()?;
    let mut place = Vec::with_capacity(count);
    for (ci, &off) in chunks.iter().enumerate() {
        let per = runs.iter().rev().find(|r| r.0 as usize <= ci + 1).map_or(0, |r| r.1);
        let mut o = off;
        for _ in 0..per {
            let Some(&s) = sizes.get(place.len()) else { break };
            place.push((o, s));
            o += s as u64;
        }
    }
    if place.len() != count || place.iter().any(|&(o, s)| o + s as u64 > file_len) {
        return None;
    }
    let (ts, _) = child(b, ss, se, b"stts")?;
    let mut durs = Vec::with_capacity(count);
    for i in 0..(u32be(b, ts + 4)? as usize).min(count) {
        let (n, d) = (u32be(b, ts + 8 + 8 * i)?, u32be(b, ts + 12 + 8 * i)?);
        durs.extend(std::iter::repeat_n(d, (n as usize).min(count - durs.len())));
    }
    durs.resize(count, *durs.last().unwrap_or(&1));
    let mut cts = vec![0i64; count];
    if let Some((os, _)) = child(b, ss, se, b"ctts") {
        let v1 = b.get(os) == Some(&1);
        let mut k = 0usize;
        for i in 0..(u32be(b, os + 4)? as usize).min(count) {
            let (n, d) = (u32be(b, os + 8 + 8 * i)?, u32be(b, os + 12 + 8 * i)?);
            let d = if v1 { d as i32 as i64 } else { d as i64 };
            for _ in 0..n {
                if k < count {
                    cts[k] = d;
                    k += 1;
                }
            }
        }
    }
    let mut key = vec![true; count];
    if let Some((ys, _)) = child(b, ss, se, b"stss") {
        key = vec![false; count];
        for i in 0..(u32be(b, ys + 4)? as usize).min(count) {
            if let Some(k) = u32be(b, ys + 8 + 4 * i).and_then(|n| (n as usize).checked_sub(1)) {
                if k < count {
                    key[k] = true;
                }
            }
        }
    }
    let t100 = |v: i64| (v as i128 * 10_000_000 / timescale as i128) as i64;
    let mut dts = 0i64;
    let mut samples = Vec::with_capacity(count);
    for i in 0..count {
        samples.push(Sample {
            off: place[i].0,
            size: place[i].1,
            pts: t100(dts + cts[i] - shift),
            dts: t100(dts - shift),
            dur: t100(durs[i] as i64).max(0) as u64,
            key: key[i],
            order: 0,
        });
        dts += durs[i] as i64;
    }
    let mut by_pts: Vec<usize> = (0..count).collect();
    by_pts.sort_by_key(|&i| (samples[i].pts, i));
    for (n, &i) in by_pts.iter().enumerate() {
        samples[i].order = n as u32;
    }
    let total = t100(dts).max(1) as u64;
    let rate = super::plat::frame_rate(total / count as u64);
    let bytes: u64 = sizes.iter().map(|&s| s as u64).sum();
    Some(Video {
        codec: format!("avc1.{:02x}{:02x}{:02x}", c[1], c[2], c[3]),
        w,
        h,
        profile: c[1] as u32,
        level: c[3] as u32,
        nal,
        head,
        samples,
        rate,
        bitrate: (bytes as u128 * 8 * 10_000_000 / total as u128) as u32,
    })
}

/// A sample's NAL units as Annex B (each length prefix a start code), the SPS and PPS before
/// a key frame's; None when the lengths do not add up.
pub fn annexb(v: &Video, key: bool, s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() + if key { v.head.len() } else { 0 } + 16);
    if key {
        out.extend_from_slice(&v.head);
    }
    let n = v.nal as usize;
    let mut p = 0;
    while p < s.len() {
        let l = s.get(p..p + n)?.iter().fold(0usize, |a, &x| a << 8 | x as usize);
        let nal = s.get(p + n..p + n + l)?;
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        p += n + l;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind);
        v.extend_from_slice(body);
        v
    }
    fn full(kind: &[u8; 4], ver: u8, body: &[u8]) -> Vec<u8> {
        let mut b = vec![ver, 0, 0, 0];
        b.extend_from_slice(body);
        bx(kind, &b)
    }
    fn be(v: &[u32]) -> Vec<u8> {
        v.iter().flat_map(|x| x.to_be_bytes()).collect()
    }

    /// An MP4 of one H.264 track: samples of `sizes` (their bytes the sample's number), one
    /// per chunk, durations `dur` in timescale 1000, composition offsets `ctts`, sync samples
    /// `sync` (1-based; None: no stss), an edit list from `shift`; the moov after the data.
    fn movie(sizes: &[u32], dur: u32, ctts: &[i32], sync: Option<&[u32]>, shift: Option<u32>) -> Vec<u8> {
        let ftyp = bx(b"ftyp", b"isom\0\0\0\0isom");
        let mut data = Vec::new();
        let mut offs = Vec::new();
        let base = ftyp.len() as u32 + 8;
        for (i, &s) in sizes.iter().enumerate() {
            offs.push(base + data.len() as u32);
            // a sample of one NAL unit: 4-byte length, then the bytes
            data.extend_from_slice(&(s - 4).to_be_bytes());
            data.extend(std::iter::repeat_n(i as u8, s as usize - 4));
        }
        let mdat = bx(b"mdat", &data);
        let avcc = bx(b"avcC", &[1, 0x4d, 0x40, 0x29, 0xff, 0xe1, 0, 3, 0x67, 1, 2, 1, 0, 2, 0x68, 3]);
        let mut entry = vec![0u8; 78];
        entry[6..8].copy_from_slice(&1u16.to_be_bytes());
        entry[24..26].copy_from_slice(&1920u16.to_be_bytes());
        entry[26..28].copy_from_slice(&1080u16.to_be_bytes());
        entry.extend_from_slice(&avcc);
        let stsd = full(b"stsd", 0, &[be(&[1]), bx(b"avc1", &entry)].concat());
        let mut stbl = stsd;
        stbl.extend(full(b"stts", 0, &be(&[1, sizes.len() as u32, dur])));
        if !ctts.is_empty() {
            let mut t = be(&[ctts.len() as u32]);
            for &c in ctts {
                t.extend(be(&[1, c as u32]));
            }
            stbl.extend(full(b"ctts", 1, &t));
        }
        if let Some(s) = sync {
            let mut t = be(&[s.len() as u32]);
            t.extend(be(s));
            stbl.extend(full(b"stss", 0, &t));
        }
        stbl.extend(full(b"stsc", 0, &be(&[1, 1, 1, 1])));
        let mut z = be(&[0, sizes.len() as u32]);
        z.extend(be(sizes));
        stbl.extend(full(b"stsz", 0, &z));
        let mut co = be(&[offs.len() as u32]);
        co.extend(be(&offs));
        stbl.extend(full(b"stco", 0, &co));
        let minf = bx(b"minf", &bx(b"stbl", &stbl));
        let mut mdhd = be(&[0, 0, 1000, sizes.len() as u32 * dur]);
        mdhd.extend([0u8; 4]);
        let hdlr = full(b"hdlr", 0, &[be(&[0]), b"vide".to_vec(), vec![0; 13]].concat());
        let mdia = bx(b"mdia", &[full(b"mdhd", 0, &mdhd), hdlr, minf].concat());
        let mut trak = Vec::new();
        if let Some(s) = shift {
            trak.extend(bx(b"edts", &full(b"elst", 0, &be(&[1, sizes.len() as u32 * dur, s, 0x10000]))));
        }
        trak.extend(mdia);
        let mvhd = full(b"mvhd", 0, &[be(&[0, 0, 1000, sizes.len() as u32 * dur]), vec![0; 80]].concat());
        let moov = bx(b"moov", &[mvhd, bx(b"trak", &trak)].concat());
        [ftyp, mdat, moov].concat()
    }
    fn open_mem(f: &[u8]) -> Option<Movie> {
        open(f.len() as u64, &mut |o, n| f.get(o as usize..o as usize + n).map(|b| b.to_vec()))
    }

    #[test]
    fn h264_track() {
        // I P B B: decode order, composition offsets as an encoder with B frames writes them
        let f = movie(&[20, 10, 8, 8], 40, &[40, 120, 0, 0], Some(&[1]), Some(40));
        let m = open_mem(&f).unwrap();
        assert_eq!(m.duration, Some(1_600_000));
        let Track::Video(v) = &m.tracks[0] else { panic!() };
        assert_eq!((v.codec.as_str(), v.w, v.h, v.profile, v.level, v.nal), ("avc1.4d4029", 1920, 1080, 0x4d, 0x29, 4));
        assert_eq!(v.head, [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3]);
        assert_eq!(v.samples.iter().map(|s| s.pts / 10_000).collect::<Vec<_>>(), [0, 120, 40, 80]);
        assert_eq!(v.samples.iter().map(|s| s.order).collect::<Vec<_>>(), [0, 3, 1, 2]);
        assert_eq!(v.samples.iter().map(|s| s.key).collect::<Vec<_>>(), [true, false, false, false]);
        assert_eq!(v.rate, (25, 1));
        // the bytes: each sample where the table says
        for (i, s) in v.samples.iter().enumerate() {
            assert_eq!(f[s.off as usize + 4], i as u8);
            let a = annexb(v, s.key, &f[s.off as usize..(s.off + s.size as u64) as usize]).unwrap();
            let mut want = if s.key { v.head.clone() } else { vec![] };
            want.extend_from_slice(&[0, 0, 0, 1]);
            want.extend(std::iter::repeat_n(i as u8, s.size as usize - 4));
            assert_eq!(a, want);
        }
        // no stss: every sample a sync sample; no edit list: times as they are
        let m = open_mem(&movie(&[9, 9], 33, &[], None, None)).unwrap();
        let Track::Video(v) = &m.tracks[0] else { panic!() };
        assert!(v.samples.iter().all(|s| s.key));
        assert_eq!(v.samples[1].pts, 330_000);
        // (33 ms is no rate of Windows' table: the exact fraction)
        assert_eq!(v.rate, (1000, 33));
    }

    #[test]
    fn not_mp4() {
        assert!(open_mem(b"RIFF\0\0\0\0WAVE").is_none());
        // the moov's end past the file's
        let mut f = movie(&[9], 40, &[], None, None);
        f.truncate(f.len() - 3);
        assert!(open_mem(&f).is_none());
    }

    /// The winref probe mfvideo's movie (tests/winref/probes/mfvideo_gen.sh: x264 Main 64x48, 24
    /// fps, 18 frames, 2 B-frames, a key frame every 6, an edit list): the edit list puts the first
    /// frame at 0, the presentation times are the frames' in order, the key frames 0, 6 and 12.
    #[test]
    fn b_frames_movie() {
        let f = include_bytes!("testdata/bframes.mp4");
        let m = open_mem(f).expect("an MP4");
        let Track::Video(v) = &m.tracks[0] else { panic!() };
        assert_eq!((v.codec.as_str(), v.w, v.h, v.rate, v.nal), ("avc1.4d400a", 64, 48, (24, 1), 4));
        assert_eq!(v.samples.len(), 18);
        let mut by_order: Vec<&Sample> = v.samples.iter().collect();
        by_order.sort_by_key(|s| s.order);
        for (k, s) in by_order.iter().enumerate() {
            assert_eq!(s.order, k as u32);
            assert_eq!(s.pts, k as i64 * 10_000_000 / 24, "frame {k}");
            assert_eq!(s.key, k % 6 == 0, "frame {k}");
        }
        // decode order differs from presentation order (B-frames), and decode times rise
        assert!(v.samples.iter().enumerate().any(|(i, s)| s.order != i as u32));
        assert!(v.samples.windows(2).all(|w| w[0].dts < w[1].dts));
        assert!(v.samples.iter().all(|s| s.dts <= s.pts));
        for s in &v.samples {
            let b = annexb(v, s.key, &f[s.off as usize..(s.off + s.size as u64) as usize]).unwrap();
            assert_eq!(&b[..4], &[0, 0, 0, 1]);
            // a key frame carries the SPS (7) and PPS (8) first
            if s.key {
                assert_eq!(b[4] & 31, 7);
            }
        }
        assert_eq!(m.duration, Some(7_500_000));
    }

    /// A real file (MP4_FILE=path): every track read, each H.264 sample's NAL lengths add up.
    #[test]
    fn file() {
        let Some(p) = std::env::var_os("MP4_FILE") else { return };
        let f = std::fs::read(p).unwrap();
        let m = open_mem(&f).expect("an MP4");
        for t in &m.tracks {
            match t {
                Track::Video(v) => {
                    eprintln!("{} {}x{} {} samples {:?} fps {} bit/s, {} key", v.codec, v.w, v.h, v.samples.len(), v.rate, v.bitrate, v.samples.iter().filter(|s| s.key).count());
                    for s in &v.samples {
                        assert!(annexb(v, s.key, &f[s.off as usize..(s.off + s.size as u64) as usize]).is_some());
                    }
                }
                Track::Audio(a) => eprintln!("{:?} {} Hz {} ch {} packets", a.codec, a.rate, a.channels, a.packets.len()),
            }
        }
        eprintln!("duration {:?}", m.duration);
    }

    proptest! {
        /// Any sizes, offsets, order: the table reads back, presentation order is a permutation
        /// that sorts the times, and a cut file never panics.
        #[test]
        fn tables(sizes in proptest::collection::vec(5u32..200, 1..40), ct in proptest::collection::vec(0i32..5, 1..40), cut in 0usize..4000) {
            let ctts: Vec<i32> = (0..sizes.len()).map(|i| ct[i % ct.len()] * 40).collect();
            let f = movie(&sizes, 40, &ctts, Some(&[1]), None);
            let m = open_mem(&f).unwrap();
            let Track::Video(v) = &m.tracks[0] else { panic!() };
            prop_assert_eq!(v.samples.len(), sizes.len());
            let mut seen = vec![false; sizes.len()];
            for (i, s) in v.samples.iter().enumerate() {
                prop_assert_eq!(s.size, sizes[i]);
                prop_assert_eq!(f[s.off as usize + 4], i as u8);
                seen[s.order as usize] = true;
            }
            prop_assert!(seen.iter().all(|&x| x));
            let mut by: Vec<&Sample> = v.samples.iter().collect();
            by.sort_by_key(|s| s.order);
            prop_assert!(by.windows(2).all(|w| w[0].pts <= w[1].pts));
            let _ = open_mem(&f[..cut.min(f.len())]);
        }

        /// Annex B of any NAL units: start codes where the lengths were, the rest as it was;
        /// lengths that run past the end: None.
        #[test]
        fn annexb_units(units in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..50), 0..8), nal in prop_oneof![Just(1u8), Just(2), Just(4)], extra in 0usize..3) {
            let v = Video { codec: String::new(), w: 0, h: 0, profile: 0, level: 0, nal, head: vec![9, 9], samples: vec![], rate: (0, 0), bitrate: 0 };
            let mut s = Vec::new();
            let mut want = vec![9, 9];
            for u in &units {
                let l = (u.len() as u32).to_be_bytes();
                s.extend_from_slice(&l[4 - nal as usize..]);
                s.extend_from_slice(u);
                want.extend_from_slice(&[0, 0, 0, 1]);
                want.extend_from_slice(u);
            }
            prop_assert_eq!(annexb(&v, true, &s), Some(want));
            if extra > 0 {
                s.extend(std::iter::repeat_n(0xffu8, extra));
                prop_assert!(annexb(&v, false, &s).is_none());
            }
        }
    }
}
