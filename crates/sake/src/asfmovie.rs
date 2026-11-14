//! Movies sake decodes itself (crates/wmcodec, through wmdec.rs): an ASF file (.wmv / .asf)
//! that `ippai pack` made no `.vnv` of plays as a `.vnv` does (dshow_play.rs). Its pictures
//! come by frame number (`frame`: kura::video's Rgb601 layout, bytes R G B A) and its sound as
//! dshow_play's `Sound::Asf` (`Audio`). The pictures' ids are kept apart from kura's streams
//! (ID_BASE up).

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;

use crate::dshow_play::SEC;
use crate::wmdec::{codec, Codec, Frame, Info};
use crate::wmvcore::{HostFile, Src};

/// The first id of a movie's pictures here (kura's count up from 1).
const ID_BASE: u32 = 0x7f00_0000;

/// A video stream, decoded in order as its frames are asked for.
struct Video {
    r: asf::Reader<Src>,
    codec: Box<dyn Codec>,
    rate: (u32, u32),
    /// pictures decoded and not shown yet (time, B G R A), in time order
    pending: VecDeque<(u64, Vec<u8>)>,
    /// the picture of the last time asked for
    shown: Option<(u64, Vec<u8>)>,
    ended: bool,
}

/// Seeking (and not decoding on) to a time further ahead than this
const SKIP: u64 = 2 * SEC;

impl Video {
    /// The picture at time `t`: the last one whose time has come (the first before it).
    fn at(&mut self, t: u64) -> Option<&[u8]> {
        let back = self.shown.as_ref().is_some_and(|s| s.0 > t);
        let far = self.shown.as_ref().map_or(t > SKIP, |s| t > s.0 + SKIP) && self.pending.back().is_none_or(|p| p.0 < t);
        if back || far {
            self.r.seek(t);
            self.codec.flush();
            self.pending.clear();
            self.shown = None;
            self.ended = false;
        }
        loop {
            while self.pending.front().is_some_and(|p| p.0 <= t) {
                self.shown = self.pending.pop_front();
            }
            if !self.pending.is_empty() || self.ended {
                break;
            }
            let frames = match self.r.next_sample() {
                Some(s) => self.codec.decode(&s.data, s.time, s.key),
                None => {
                    self.ended = true;
                    self.codec.drain()
                }
            };
            for f in frames {
                if let Frame::Picture { time, bgra, .. } = f {
                    self.pending.push_back((time, bgra));
                }
            }
        }
        self.shown.as_ref().or(self.pending.front()).map(|p| &p.1[..])
    }
}

static VIDEOS: Mutex<Option<HashMap<u32, Video>>> = Mutex::new(None);

fn videos<R>(f: impl FnOnce(&mut HashMap<u32, Video>) -> R) -> R {
    let mut g = VIDEOS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Whether `id` is a movie's pictures from here (not kura's).
pub fn is(id: u32) -> bool {
    id >= ID_BASE
}

/// Frame `n` (`w` x `h`, rows `pitch` bytes apart) of the movie's pictures `id`, as kura's
/// Rgb601 frames are: bytes R G B A. False: there is none (the stream could not be decoded).
///
/// # Safety
/// `dst` holds `h` rows of `pitch` bytes, each `4 * w` long.
pub unsafe fn frame(id: u32, n: i32, dst: *mut u8, pitch: usize, w: u32, h: u32) -> bool {
    videos(|m| {
        let Some(v) = m.get_mut(&id) else { return false };
        let t = (n.max(0) as u128 * v.rate.1 as u128 * SEC as u128 / v.rate.0.max(1) as u128) as u64;
        let Some(bgra) = v.at(t) else { return false };
        // (the picture's size: what the header says, as `w` x `h` are)
        let rows = bgra.len() / (4 * w as usize).max(1);
        for y in 0..(h as usize).min(rows) {
            // SAFETY: the caller's rows
            let d = unsafe { std::slice::from_raw_parts_mut(dst.add(y * pitch), 4 * w as usize) };
            rgba(&bgra[y * 4 * w as usize..][..4 * w as usize], d);
        }
        true
    })
}

/// B G R A to R G B A
fn rgba(src: &[u8], dst: &mut [u8]) {
    for (d, s) in dst.chunks_exact_mut(4).zip(src.chunks_exact(4)) {
        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
    }
}

pub fn close(id: u32) {
    videos(|m| m.remove(&id));
}

/// A movie's sound: stereo frames at `rate` from any position (a position before what is
/// kept, or far after it, seeks the file).
pub struct Audio {
    r: asf::Reader<Src>,
    codec: Box<dyn Codec>,
    pub rate: u32,
    /// decoded stereo frames, the first of them frame `base` (None: not known yet, after a
    /// seek: the next sound's time says)
    pcm: VecDeque<[f32; 2]>,
    base: Option<u64>,
    ended: bool,
}

impl Audio {
    fn seek(&mut self, frame: u64) {
        self.r.seek((frame as u128 * SEC as u128 / self.rate.max(1) as u128) as u64);
        self.codec.flush();
        self.pcm.clear();
        self.base = None;
        self.ended = false;
    }

    /// Appends `n` stereo frames from frame `from` to `out` (silence past the end).
    pub fn take(&mut self, from: u64, n: usize, out: &mut Vec<f32>) {
        let have = self.base.map(|b| (b, b + self.pcm.len() as u64));
        if have.is_none_or(|(b, e)| from < b || from > e + 2 * self.rate as u64) {
            self.seek(from);
        }
        while !self.ended && self.base.is_none_or(|b| b + (self.pcm.len() as u64) < from + n as u64) {
            let frames = match self.r.next_sample() {
                Some(s) => self.codec.decode(&s.data, s.time, s.key),
                None => {
                    self.ended = true;
                    self.codec.drain()
                }
            };
            for f in frames {
                if let Frame::Sound { time, channels, pcm, .. } = f {
                    let base = *self.base.get_or_insert((time as u128 * self.rate as u128 / SEC as u128) as u64);
                    let ch = channels.max(1) as usize;
                    let s = |v: i16| v as f32 / 32768.0;
                    self.pcm.extend(pcm.chunks_exact(ch).map(|f| if ch == 1 { [s(f[0]), s(f[0])] } else { [s(f[0]), s(f[1])] }));
                    // (what is no longer wanted: a second's worth kept behind for small steps back)
                    let keep = from.saturating_sub(self.rate as u64);
                    if keep > base {
                        let k = ((keep - base) as usize).min(self.pcm.len());
                        self.pcm.drain(..k);
                        self.base = Some(base + k as u64);
                    }
                }
            }
        }
        let base = self.base.unwrap_or(u64::MAX);
        for i in 0..n as u64 {
            let f = (from + i).checked_sub(base).and_then(|k| self.pcm.get(k as usize)).copied().unwrap_or([0.0; 2]);
            out.extend(f);
        }
    }
}

/// What a movie file gives (dshow_play::Movie's parts)
pub struct Opened {
    pub id: u32,
    pub w: u32,
    pub h: u32,
    pub frames: u32,
    pub rate: (u32, u32),
    pub duration: u64,
    pub audio: Option<Audio>,
}

/// The reader of stream `number` of the file at `path`
fn reader(path: &str, number: u8) -> Option<asf::Reader<Src>> {
    let f = kura::fs::File::open(path).ok()?;
    let mut r = asf::Reader::new(Box::new(HostFile(f)) as Src).ok()?;
    r.only = Some(vec![number]);
    Some(r)
}

/// The ASF file at `path`: its first video stream sake decodes (and first audio stream).
pub fn open(path: &str) -> Option<Opened> {
    let f = kura::fs::File::open(path).ok()?;
    let h = asf::Reader::new(Box::new(HostFile(f)) as Src).ok()?.header;
    let pick = |video: bool| {
        h.streams.iter().filter(|s| !s.encrypted && matches!(s.kind, asf::Kind::Video { .. }) == video && (video || matches!(s.kind, asf::Kind::Audio))).find_map(|s| {
            let i = Info::of_stream(s)?;
            Some((s, i.clone(), codec(&i)?))
        })
    };
    let (vs, vi, codec) = pick(true)?;
    let Info::Video { width, height, .. } = vi else { return None };
    let avg = if vs.avg_time_per_frame != 0 { vs.avg_time_per_frame } else { SEC / 30 };
    let rate = (SEC as u32, avg.min(u32::MAX as u64) as u32);
    let frames = (h.duration as u128 * rate.0 as u128).div_ceil(rate.1 as u128 * SEC as u128).max(1) as u32;
    let audio = pick(false).and_then(|(s, i, codec)| {
        let Info::Audio { rate, .. } = i else { return None };
        Some(Audio { r: reader(path, s.number)?, codec, rate, pcm: VecDeque::new(), base: None, ended: false })
    });
    let v = Video { r: reader(path, vs.number)?, codec, rate, pending: VecDeque::new(), shown: None, ended: false };
    static NEXT: AtomicU32 = AtomicU32::new(ID_BASE);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    videos(|m| m.insert(id, v));
    Some(Opened { id, w: width, h: height, frames, rate, duration: h.duration, audio })
}

#[cfg(test)]
mod tests {
    use super::*;
    use asf::write::Mux;
    use proptest::prelude::*;

    const W: u32 = 4;
    const H: u32 = 2;
    /// pictures 40 ms apart (25 a second), sound 8000 Hz stereo
    const PICS: u32 = 10;
    const RATE: u32 = 8000;

    fn wave() -> Vec<u8> {
        let mut w = Vec::new();
        for v in [1u32 | 2 << 16, RATE, RATE * 4, 4 | 16 << 16] {
            w.extend(v.to_le_bytes());
        }
        w.extend(0u16.to_le_bytes());
        w
    }

    fn bih() -> Vec<u8> {
        let mut b = Vec::new();
        for v in [40, W, H, 1 | 24 << 16] {
            b.extend(v.to_le_bytes());
        }
        b.extend([0u8; 24]);
        b
    }

    /// Picture k (bottom-up RGB24, rows of 12 bytes)
    fn picture(k: u32) -> Vec<u8> {
        (0..24).map(|i| (i * 7 + k * 31) as u8).collect()
    }

    /// The sound's frame i: (left, right)
    fn pcm(i: u32) -> (i16, i16) {
        ((i * 3) as i16, -((i * 5 % 30000) as i16))
    }

    /// A file of PICS pictures and 400 ms of sound in 50 ms objects, interleaved; its path.
    fn file() -> String {
        static P: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        P.get_or_init(|| {
            let streams = vec![
                asf::Stream { number: 1, kind: asf::Kind::Audio, format: wave(), encrypted: false, time_offset: 0, spread: None, avg_time_per_frame: 0, bitrate: RATE * 32 },
                asf::Stream { number: 2, kind: asf::Kind::Video { width: W, height: H }, format: bih(), encrypted: false, time_offset: 0, spread: None, avg_time_per_frame: 400_000, bitrate: 0 },
            ];
            let mut s = Vec::new();
            for k in 0..PICS {
                s.push((2u8, k % 4 == 0, k * 40, picture(k)));
                if k % 5 == 0 || k % 5 == 2 {
                    let a = s.iter().filter(|x| x.0 == 1).count() as u32;
                    let data = (a * 400..a * 400 + 400).flat_map(|i| { let (l, r) = pcm(i); [l.to_le_bytes(), r.to_le_bytes()].concat() }).collect();
                    s.push((1, true, a * 50, data));
                }
            }
            let m = Mux { packet_size: 800, preroll: 0, streams, attributes: vec![], compress_small: false };
            let p = std::env::temp_dir().join(format!("sake-asfmovie-{}.wmv", std::process::id()));
            std::fs::write(&p, m.write(&s, PICS as u64 * 40)).unwrap();
            p.to_str().unwrap().to_string()
        })
        .clone()
    }

    /// Frame n as R G B A, top row first: what picture `k` is
    fn rgba_of(k: u32) -> Vec<u8> {
        let p = picture(k);
        let mut out = Vec::new();
        for y in 0..H as usize {
            let row = &p[(H as usize - 1 - y) * 12..][..12];
            for x in 0..W as usize {
                out.extend([row[x * 3 + 2], row[x * 3 + 1], row[x * 3], 255]);
            }
        }
        out
    }

    #[test]
    fn the_file_opens() {
        let o = open(&file()).unwrap();
        assert_eq!((o.w, o.h, o.rate, o.frames, o.duration), (W, H, (10_000_000, 400_000), PICS, PICS as u64 * 400_000));
        assert_eq!(o.audio.as_ref().map(|a| a.rate), Some(RATE));
        close(o.id);
        assert!(open("/nonexistent.wmv").is_none());
    }

    proptest! {
        /// Frames asked for in any order (back, forward, far past the end): each is its
        /// picture (the last one past the end), never a panic.
        #[test]
        fn frames_in_any_order(ns in prop::collection::vec(0i32..PICS as i32 + 5, 1..30)) {
            let o = open(&file()).unwrap();
            for n in ns {
                let mut px = vec![0u8; (W * H * 4) as usize];
                // SAFETY: px is H rows of 4 * W bytes
                let got = unsafe { frame(o.id, n, px.as_mut_ptr(), 4 * W as usize, W, H) };
                prop_assert!(got);
                prop_assert_eq!(&px, &rgba_of((n as u32).min(PICS - 1)), "frame {}", n);
            }
            close(o.id);
            let mut px = vec![0u8; (W * H * 4) as usize];
            // SAFETY: as above
            let got = unsafe { frame(o.id, 0, px.as_mut_ptr(), 4 * W as usize, W, H) };
            prop_assert!(!got, "a closed movie has no frames");
        }

        /// Sound from any positions, in any order: the file's samples, silence past its end.
        #[test]
        fn sound_from_any_position(takes in prop::collection::vec((0u64..6000, 1usize..900), 1..20)) {
            let mut a = open(&file()).unwrap().audio.unwrap();
            let total = 4 * 400;
            for (from, n) in takes {
                let mut out = Vec::new();
                a.take(from, n, &mut out);
                prop_assert_eq!(out.len(), 2 * n);
                for i in 0..n {
                    let f = from + i as u64;
                    let want = if f < total { let (l, r) = pcm(f as u32); [l as f32 / 32768.0, r as f32 / 32768.0] } else { [0.0; 2] };
                    prop_assert_eq!([out[2 * i], out[2 * i + 1]], want, "frame {}", f);
                }
            }
        }
    }
}
