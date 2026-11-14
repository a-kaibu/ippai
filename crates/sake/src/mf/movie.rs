//! Movies Media Foundation's MP4 source opens (H.264 with AAC or MP3), played as a `.vnv` is
//! (dshow_play.rs): by the Media Session (session.rs), and by DirectShow and the Media Engine
//! for an MP4 file `ippai pack` made no `.vnv` of. The pictures come by frame number (`frame`:
//! bytes R G B A, as kura::video's Rgb601) from the page's VideoDecoder (vstream.rs), the sound
//! as dshow_play's `Sound::Mf` (`Audio`: the Source Reader's stream, the page's AudioDecoder).
//! The pictures' ids are apart from kura's streams and asfmovie's (ID_BASE..ID_END).

use super::reader::{Out, Stream};
use super::source::{self, Data, Media};
use super::vstream::{VOut, VStream};
use super::*;
use crate::dshow_play::SEC;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

const ID_BASE: u32 = 0x7e00_0000;
const ID_END: u32 = 0x7f00_0000;

/// A movie's pictures: the stream, its frames, the last one handed out (B G R A).
struct Video {
    s: VStream,
    frames: usize,
    shown: Option<(usize, Vec<u8>)>,
    failed: bool,
}

static VIDEOS: Mutex<Option<HashMap<u32, Video>>> = Mutex::new(None);

fn videos<R>(f: impl FnOnce(&mut HashMap<u32, Video>) -> R) -> R {
    let mut g = VIDEOS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Whether `id` is a movie's pictures from here.
pub fn is(id: u32) -> bool {
    (ID_BASE..ID_END).contains(&id)
}

/// Frame `n` (`w` x `h`, rows `pitch` bytes apart) of the movie's pictures `id`, bytes R G B A.
/// False: none (a movie of sound only, or the page did not decode it).
///
/// # Safety
/// `dst` holds `h` rows of `pitch` bytes, each `4 * w` long.
pub unsafe fn frame(id: u32, n: i32, dst: *mut u8, pitch: usize, w: u32, h: u32) -> bool {
    videos(|m| {
        let Some(v) = m.get_mut(&id) else { return false };
        if v.frames == 0 {
            return false;
        }
        let p = (n.max(0) as usize).min(v.frames - 1);
        if v.shown.as_ref().map(|s| s.0) != Some(p) {
            match v.s.frame(None, p) {
                Ok(Some(piece)) => v.shown = Some((p, piece.bytes)),
                Ok(None) => return false,
                Err(_) => {
                    v.failed = true;
                    return false;
                }
            }
        }
        let Some((_, bgra)) = &v.shown else { return false };
        let row = 4 * w as usize;
        let rows = bgra.len() / row.max(1);
        for y in 0..(h as usize).min(rows) {
            // SAFETY: the caller's rows
            let d = unsafe { std::slice::from_raw_parts_mut(dst.add(y * pitch), row) };
            for (d, s) in d.chunks_exact_mut(4).zip(bgra[y * row..][..row].chunks_exact(4)) {
                d.copy_from_slice(&[s[2], s[1], s[0], 255]);
            }
        }
        true
    })
}

/// Whether the page could not decode movie `id`'s pictures.
pub fn failed(id: u32) -> bool {
    videos(|m| m.get(&id).is_some_and(|v| v.failed))
}

pub fn close(id: u32) {
    let v = videos(|m| m.remove(&id));
    // (the page's stream closes outside the lock)
    drop(v);
}

/// A movie's sound: stereo frames at `rate` from any position (one before what is kept, or far
/// after it, seeks).
pub struct Audio {
    s: Stream,
    pub rate: u32,
    pcm: VecDeque<[f32; 2]>,
    base: Option<u64>,
    ended: bool,
}

impl Audio {
    pub fn new(track: crate::audiofile::Track, data: Arc<Data>) -> Audio {
        let rate = track.rate.max(1);
        let mut s = Stream::with_data(track, data);
        s.set_out(Out::Pcm { bits: 32, float: true, rate, channels: 2 });
        Audio { s, rate, pcm: VecDeque::new(), base: None, ended: false }
    }

    fn seek(&mut self, frame: u64) {
        self.s.seek((frame as u128 * SEC as u128 / self.rate as u128) as u64);
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
            let p = match self.s.read_in(None) {
                Ok(Some(p)) => p,
                _ => {
                    self.ended = true;
                    break;
                }
            };
            // (the piece's time is the frame's, truncated: rounded up it is the frame again)
            let at = (p.time as u128 * self.rate as u128).div_ceil(SEC as u128) as u64;
            let base = *self.base.get_or_insert(at);
            let f = |b: &[u8]| f32::from_le_bytes(b.try_into().unwrap());
            self.pcm.extend(p.bytes.chunks_exact(8).map(|x| [f(&x[..4]), f(&x[4..])]));
            // (what is no longer wanted: a second's worth kept behind for small steps back)
            let keep = from.saturating_sub(self.rate as u64);
            if keep > base {
                let k = ((keep - base) as usize).min(self.pcm.len());
                self.pcm.drain(..k);
                self.base = Some(base + k as u64);
            }
        }
        let base = self.base.unwrap_or(u64::MAX);
        for i in 0..n as u64 {
            let f = (from + i).checked_sub(base).and_then(|k| self.pcm.get(k as usize)).copied().unwrap_or([0.0; 2]);
            out.extend(f);
        }
    }
}

/// The movie of tracks `video` and `audio` of `media` (its bytes `data`). None when neither
/// is a track of that kind.
pub fn open(data: Arc<Data>, media: &Media, video: Option<usize>, audio: Option<usize>) -> Option<crate::dshow_play::Movie> {
    let v = video.and_then(|i| match media.tracks.get(i) {
        Some(mp4::Track::Video(v)) => Some(v.clone()),
        _ => None,
    });
    let a = audio.and_then(|i| match media.tracks.get(i) {
        Some(mp4::Track::Audio(a)) => Some(a.clone()),
        _ => None,
    });
    if v.is_none() && a.is_none() {
        return None;
    }
    let sound = a.map(|t| Audio::new(t, media.packed.clone().unwrap_or_else(|| data.clone())));
    let sound_len = sound.as_ref().map(|a| a.s.duration()).unwrap_or(0);
    static NEXT: AtomicU32 = AtomicU32::new(ID_BASE);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (w, h, frames, rate, vlen) = match v {
        Some(v) => {
            let rate = if v.rate.0 != 0 && v.rate.1 != 0 { v.rate } else { (25, 1) };
            let (w, h, frames) = (v.w, v.h, v.samples.len());
            let mut s = VStream::new(v, media.packed.clone().unwrap_or(data));
            s.set_out(VOut::Decoded(source::RGB32));
            let len = s.duration();
            videos(|m| m.insert(id, Video { s, frames, shown: None, failed: false }));
            (w, h, frames.max(1) as u32, rate, len)
        }
        None => (0, 0, 1, (1, 1), 0),
    };
    let duration = media.duration.filter(|&d| d > 0).unwrap_or(vlen.max(sound_len));
    Some(crate::dshow_play::Movie { stream: id, w, h, frames, rate, duration, sound: sound.map(crate::dshow_play::Sound::Mf) })
}

/// The MP4 movie at host path `path` with a video track (anything else: None, unread).
pub fn open_path(path: &str) -> Option<crate::dshow_play::Movie> {
    let d = Data::host(kura::fs::File::open(path).ok()?)?;
    let head = d.read(None, 0, 8)?;
    if head.len() < 8 || &head[4..8] != b"ftyp" {
        return None;
    }
    let m = source::open_media(None, &d)?;
    let v = m.tracks.iter().position(|t| matches!(t, mp4::Track::Video(_)))?;
    let a = m.tracks.iter().position(|t| matches!(t, mp4::Track::Audio(_)));
    open(Arc::new(d), &m, Some(v), a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn media(name: &str) -> (Arc<Data>, Media) {
        let b = std::fs::read(format!("{}/src/mf/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let d = Data::mem_arc(Arc::new(b));
        let m = source::open_media(None, &d).unwrap();
        (Arc::new(d), m)
    }

    /// winref mfvideo's movie: its size, frames and rate as the reader has them; its pictures
    /// by an id of ours; a track of the other kind (or none) is no movie.
    #[test]
    fn an_mp4_opens() {
        let (d, m) = media("bframes.mp4");
        let mv = open(d.clone(), &m, Some(0), None).unwrap();
        assert!(is(mv.stream) && !crate::asfmovie::is(mv.stream));
        assert_eq!((mv.w, mv.h, mv.frames, mv.rate), (64, 48, 18, (24, 1)));
        assert_eq!(mv.duration / 10_000, 750);
        assert!(mv.sound.is_none());
        assert!(!failed(mv.stream));
        let id = mv.stream;
        drop(mv);
        let mut px = [0u8; 64 * 48 * 4];
        // SAFETY: px is 48 rows of 256 bytes
        assert!(!unsafe { frame(id, 0, px.as_mut_ptr(), 256, 64, 48) }, "a closed movie has no frames");
        assert!(open(d.clone(), &m, None, Some(0)).is_none());
        assert!(open(d, &m, None, None).is_none());
    }

    /// An ASF movie (WMV2 + WMA2) the same way: pictures decoded here, its sound as a track.
    #[test]
    fn a_wmv_opens() {
        let (d, m) = media("colors.wmv");
        assert_eq!(m.mime, "video/x-ms-asf");
        let mut mv = open(d, &m, Some(0), Some(1)).unwrap();
        assert_eq!((mv.w, mv.h, mv.frames, mv.rate), (64, 48, 18, (24, 1)));
        let mut px = vec![0u8; 64 * 48 * 4];
        for (n, want) in [(0i32, [255u8, 0, 0]), (8, [0, 255, 0]), (17, [0, 0, 255]), (2, [255, 0, 0])] {
            // SAFETY: px is 48 rows of 256 bytes
            assert!(unsafe { frame(mv.stream, n, px.as_mut_ptr(), 256, 64, 48) });
            let o = 4 * (24 * 64 + 32);
            let near = (0..3).map(|k| (px[o + k] as i32 - want[k] as i32).abs()).sum::<i32>();
            assert!(near < 60, "frame {n}: {:?}", &px[o..o + 4]);
        }
        let Some(crate::dshow_play::Sound::Mf(a)) = &mut mv.sound else { panic!() };
        assert_eq!(a.rate, 44100);
        let mut out = Vec::new();
        a.take(4410, 4410, &mut out);
        // (lavfi sine: amplitude 1/8)
        assert!(out.iter().any(|&v| v.abs() > 0.05), "the tone");
    }

    proptest! {
        /// Sound from any positions, in any order: what reading it all from the start gives,
        /// silence past the end.
        #[test]
        fn sound_from_any_position(takes in prop::collection::vec((0u64..26_000, 1usize..5000), 1..12)) {
            let (d, m) = media("tone.ogg");
            let mut mv = open(d, &m, None, Some(0)).unwrap();
            prop_assert_eq!((mv.w, mv.h, mv.duration / 10_000), (0, 0, 500));
            let Some(crate::dshow_play::Sound::Mf(a)) = &mut mv.sound else { panic!() };
            prop_assert_eq!(a.rate, 44100);
            let mut all = Vec::new();
            a.take(0, 22050, &mut all);
            prop_assert!(all.iter().any(|&v| v.abs() > 0.1));
            for (from, n) in takes {
                let mut out = Vec::new();
                a.take(from, n, &mut out);
                prop_assert_eq!(out.len(), 2 * n);
                for i in 0..n {
                    let f = from as usize + i;
                    let want = if f < 22050 { [all[2 * f], all[2 * f + 1]] } else { [0.0; 2] };
                    prop_assert!((out[2 * i] - want[0]).abs() < 1e-6 && (out[2 * i + 1] - want[1]).abs() < 1e-6, "frame {}", f);
                }
            }
        }
    }
}
