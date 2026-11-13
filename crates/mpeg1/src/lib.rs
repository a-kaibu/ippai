//! MPEG-1: system streams (ps), video (video) and audio layer II (audio), decoded in Rust for
//! the DirectShow MPEG-1 splitter and decoders sake gives games (KiriKiri's krmovie builds its
//! graph by hand around CLSID_MPEG1Splitter). The decoders are ports of pl_mpeg (MIT, Dominic
//! Szablewski); tests compare them with FFmpeg's own decoders.

// (pl_mpeg's constants as it writes them)
#![allow(clippy::approx_constant, clippy::excessive_precision)]

pub mod audio;
pub mod bits;
#[doc(hidden)]
pub mod check;
pub mod ps;
pub mod tables;
pub mod video;

pub use audio::Audio;
pub use ps::{Demux, Packet};
pub use video::{Frame, Video};

/// What a whole stream holds, found by reading its start (and end, for the duration).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Info {
    pub width: usize,
    pub height: usize,
    /// pictures a second
    pub rate: f64,
    /// audio samples a second (0: no audio)
    pub audio_rate: u32,
    /// seconds, from the first to the last time stamp (and a picture)
    pub duration: f64,
}

/// The facts of a stream from its first `head` bytes and its last `tail` bytes (`tail` may
/// be the same bytes, for a short stream). A bare video stream has no times: its pictures are
/// counted in `head` when it is all of it.
pub fn probe(head: &[u8], tail: &[u8], whole: bool) -> Option<Info> {
    let mut info = Info::default();
    let mut first_pts = None;
    if ps::is_video_stream(head) {
        let mut v = Video::new();
        v.push(head);
        if whole {
            v.end();
        }
        let mut n = 0u64;
        while v.decode().is_some() {
            n += 1;
            if !whole {
                break;
            }
        }
        let h = v.header()?;
        info.width = h.width;
        info.height = h.height;
        info.rate = h.rate;
        if whole && h.rate > 0.0 {
            info.duration = n as f64 / h.rate;
        }
        return Some(info);
    }
    if !ps::is_system(head) {
        return None;
    }
    let mut m = Demux::new();
    m.push(head);
    let mut v = Video::new();
    let mut a = Audio::new();
    while let Some(p) = m.next() {
        if ps::is_video(p.id) {
            if first_pts.is_none() {
                first_pts = p.pts;
            }
            if v.header().is_none() {
                v.push(&p.data);
                while v.header().is_none() && v.decode().is_some() {}
                // (the header is read on the way to the first picture)
                let _ = v.decode();
            }
        } else if ps::is_audio(p.id) && a.header().is_none() {
            a.push(&p.data);
            a.decode();
        }
        if v.header().is_some() && a.header().is_some() && first_pts.is_some() {
            break;
        }
    }
    let h = v.header()?;
    info.width = h.width;
    info.height = h.height;
    info.rate = h.rate;
    info.audio_rate = a.header().map_or(0, |h| h.rate);
    if let (Some(f), Some(l)) = (first_pts, last_shown(tail, h.rate)) {
        let frame = if h.rate > 0.0 { 1.0 / h.rate } else { 0.0 };
        info.duration = (l - f).max(0.0) + frame;
    }
    Some(info)
}

/// How much of a stream's end `probe_stream` reads at most, looking for its last picture.
pub const MAX_TAIL: u64 = 16 << 20;

/// The facts of a stream of `len` bytes that `read(pos, n)` reads (n bytes at pos): its first
/// `part` bytes and, for the duration, its last `part` bytes, then 4 times as many and so on
/// (up to MAX_TAIL) while those hold no picture's time. A stream may end in padding: アクアリウム
/// (aquras) 体験版's opening has 366 KB of padding packets after its last picture, and its
/// duration read as 0 stopped it at once (#230).
pub fn probe_stream(len: u64, part: u64, mut read: impl FnMut(u64, usize) -> Option<Vec<u8>>) -> Option<Info> {
    let part = part.max(1);
    let head = read(0, part.min(len) as usize)?;
    if len <= part {
        return probe(&head, &head, true);
    }
    let mut back = part;
    loop {
        let tail = read(len - back, back as usize)?;
        let i = probe(&head, &tail, false)?;
        // (a bare video stream has no times to find)
        if i.duration > 0.0 || back >= len.min(MAX_TAIL) || ps::is_video_stream(&head) {
            return Some(i);
        }
        back = back.saturating_mul(4).min(len).min(MAX_TAIL.max(part));
    }
}

/// When the last picture of `tail` is shown: a packet's time stamp is that of the first picture
/// that starts in it, so the pictures after the last stamp are placed by their temporal
/// references (and a GOP after them follows the last picture of the one before).
fn last_shown(tail: &[u8], rate: f64) -> Option<f64> {
    let mut i = tail.len().saturating_sub(4);
    let (pts, es) = loop {
        if i == 0 {
            return None;
        }
        i -= 1;
        if tail[i] == 0 && tail[i + 1] == 0 && tail[i + 2] == 1 && ps::is_video(tail[i + 3]) {
            let mut m = Demux::new();
            m.push(&tail[i..]);
            m.end();
            if let Some(Packet { pts: Some(t), id, data }) = m.next() {
                let mut es = data;
                while let Some(p) = m.next() {
                    if p.id == id {
                        es.extend(p.data);
                    }
                }
                break (t, es);
            }
        }
    };
    if rate <= 0.0 {
        return Some(pts);
    }
    // (base: when temporal reference 0 of this GOP is shown, once the first picture is seen)
    let (mut base, mut top, mut last) = (None::<f64>, 0u32, pts);
    for j in 0..es.len().saturating_sub(5) {
        if es[j] != 0 || es[j + 1] != 0 || es[j + 2] != 1 {
            continue;
        }
        match es[j + 3] {
            0xb8 => {
                if let Some(b) = base {
                    base = Some(b + (top + 1) as f64 / rate);
                    top = 0;
                }
            }
            0x00 => {
                let t = (es[j + 4] as u32) << 2 | (es[j + 5] as u32) >> 6;
                let b = *base.get_or_insert(pts - t as f64 / rate);
                top = top.max(t);
                last = last.max(b + t as f64 / rate);
            }
            _ => {}
        }
    }
    Some(last)
}
