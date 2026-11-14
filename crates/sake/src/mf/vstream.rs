//! A Source Reader's video stream (H.264 from an MP4): compressed samples in decode order as
//! the file has them (Annex B, the native type), or frames decoded by the page's VideoDecoder
//! (crates/kura/src/video.rs `open_chunks`) in presentation order, as NV12, I420 / IYUV, YV12,
//! YUY2 or RGB32 / ARGB32 (BT.709 for HD, 601 below, from video range: what Windows' decoder
//! and video processor give).
//!
//! Samples are read from the file as the page needs them: from the key frame before the frame
//! asked for to a few frames past it (B frames come out after the frames they need).

use super::reader::Piece;
use super::source::{self, Data};
use super::*;
use std::sync::Arc;

/// Frames past the one asked for whose samples go to the page before it (the decoder holds
/// back what it reorders).
const AHEAD: usize = 8;
/// How long a read waits for the page's frame: the first (the decoder starting), then each.
const FIRST: std::time::Duration = std::time::Duration::from_secs(10);
const EACH: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum VOut {
    /// the samples as they are (Annex B)
    Native,
    /// decoded frames of this FOURCC (source::DECODED)
    Decoded(u32),
}

pub struct VStream {
    pub v: mp4::Video,
    data: Arc<Data>,
    pub out: VOut,
    /// the next sample: by decode order (Native) or presentation order (Decoded)
    next: usize,
    /// presentation order -> decode order
    by_order: Vec<usize>,
    /// the page's stream, and the samples (decode order) handed to it
    id: Option<u32>,
    pushed: std::ops::Range<usize>,
    pub ended: bool,
    /// a Windows Media stream's decoder here (wmdec), opened at the first decoded read
    soft: Option<Soft>,
}

/// Decoding a Windows Media stream: the codec, the next sample (decode order) to give it, the
/// pictures it gave not handed out yet (time, B G R A), in time order.
struct Soft {
    codec: Box<dyn crate::wmdec::Codec>,
    next: usize,
    pics: VecDeque<(u64, Arc<Vec<u8>>)>,
    /// the time of the last picture handed out
    last: Option<u64>,
}

impl Drop for VStream {
    fn drop(&mut self) {
        if let Some(id) = self.id {
            kura::video::close(id);
        }
    }
}

impl VStream {
    pub fn new(v: mp4::Video, data: Arc<Data>) -> VStream {
        let mut by_order = vec![0; v.samples.len()];
        for (i, s) in v.samples.iter().enumerate() {
            by_order[s.order as usize] = i;
        }
        VStream { v, data, out: VOut::Native, next: 0, by_order, id: None, pushed: 0..0, ended: false, soft: None }
    }

    pub fn duration(&self) -> u64 {
        self.v.samples.iter().map(|s| (s.pts.max(0) as u64) + s.dur).max().unwrap_or(0)
    }

    /// The time of the next sample (for reads of any stream: the earliest goes first).
    pub fn next_time(&self) -> Option<i64> {
        let i = match self.out {
            VOut::Native => self.next,
            VOut::Decoded(_) => *self.by_order.get(self.next)?,
        };
        self.v.samples.get(i).map(|s| s.pts)
    }

    pub fn set_out(&mut self, out: VOut) {
        if out != self.out {
            self.out = out;
            self.seek_index(0);
        }
    }

    /// Goes to the key frame at or before `t` (100 ns): the reader gives frames from there, as
    /// Windows' does (the program drops those before its time).
    pub fn seek(&mut self, t: i64) {
        let k = self.v.samples.iter().enumerate().filter(|(_, s)| s.key && s.pts <= t).max_by_key(|(_, s)| s.pts).map_or(0, |(i, _)| i);
        self.seek_index(k);
    }
    fn seek_index(&mut self, k: usize) {
        self.next = match self.out {
            VOut::Native => k,
            VOut::Decoded(_) => self.v.samples.get(k).map_or(0, |s| s.order as usize),
        };
        self.ended = false;
    }

    /// The key frame at or before decode index `i`.
    fn key_before(&self, i: usize) -> usize {
        (0..=i.min(self.v.samples.len().saturating_sub(1))).rev().find(|&k| self.v.samples[k].key).unwrap_or(0)
    }

    /// Sample `i` (decode order) as Annex B.
    fn sample(&self, c: Option<&mut Ctx>, i: usize) -> Result<Vec<u8>, u32> {
        let s = &self.v.samples[i];
        let b = self.data.read(c, s.off, s.size as usize).ok_or(E_FAIL)?;
        if self.v.wm.is_some() {
            return Ok(b);
        }
        mp4::annexb(&self.v, s.key, &b).ok_or(E_FAIL)
    }

    /// Frame `p` (presentation order) of a Decoded stream, wherever the reads were: the page
    /// decodes from the key frame before it (a movie's frames by number, movie.rs).
    pub fn frame(&mut self, c: Option<&mut Ctx>, p: usize) -> Result<Option<Piece>, u32> {
        self.next = p;
        self.ended = false;
        self.read(c)
    }

    /// The next sample, or None at the end (Err: it could not be read or decoded).
    pub fn read(&mut self, mut c: Option<&mut Ctx>) -> Result<Option<Piece>, u32> {
        let n = self.v.samples.len();
        if self.next >= n {
            self.ended = true;
            return Ok(None);
        }
        let VOut::Decoded(sub) = self.out else {
            let i = self.next;
            let bytes = self.sample(c, i)?;
            let s = &self.v.samples[i];
            self.next += 1;
            return Ok(Some(Piece { time: s.pts.max(0) as u64, dur: s.dur, bytes }));
        };
        if self.v.wm.is_some() {
            let p = self.next;
            let bgra = self.soft_picture(c, p)?;
            let s = &self.v.samples[self.by_order[p]];
            let (time, dur) = (s.pts.max(0) as u64, s.dur);
            self.next += 1;
            return Ok(Some(Piece { time, dur, bytes: soft_convert(&bgra, self.v.w, self.v.h, sub) }));
        }
        let first = self.id.is_none();
        let id = match self.id {
            Some(id) => id,
            None => {
                let frames: Vec<(bool, u32)> = self.v.samples.iter().map(|s| (s.key, s.order)).collect();
                let id = kura::video::open_chunks(&self.v.codec, self.v.w, self.v.h, &frames).ok_or(E_FAIL)?;
                self.id = Some(id);
                id
            }
        };
        let p = self.next;
        let d = self.by_order[p];
        // the samples the decoder needs for frames p..p+AHEAD: from the key frame before, to
        // the last of them in decode order
        let from = self.key_before(d);
        let to = (p..(p + AHEAD).min(n)).map(|q| self.by_order[q]).max().unwrap_or(d) + 1;
        if !(self.pushed.start <= from && from <= self.pushed.end) {
            self.pushed = from..from;
        }
        for i in self.pushed.end..to {
            let b = self.sample(c.as_deref_mut(), i)?;
            kura::video::put(id, i as u32, b);
        }
        self.pushed.end = self.pushed.end.max(to);
        kura::video::keep_chunks(id, from as u32..self.pushed.end as u32);
        self.pushed.start = from;
        let (w, h) = (self.v.w, self.v.h);
        let mut bytes = Vec::new();
        let got = kura::video::frame_exact(id, p as i32, if first { FIRST } else { EACH }, |layout, buf| {
            bytes = unsafe { convert(layout, buf, w, h, sub) };
        });
        if !got {
            eprintln!("sake: mf reader: frame {p} of {}x{} {} not decoded", w, h, self.v.codec);
            return Err(E_FAIL);
        }
        let s = &self.v.samples[d];
        self.next += 1;
        Ok(Some(Piece { time: s.pts.max(0) as u64, dur: s.dur, bytes }))
    }
}

impl VStream {
    /// Picture `p` (presentation order) of a Windows Media stream, decoded here: from the key
    /// frame before it when going back or past a key frame, else on from where decoding is.
    fn soft_picture(&mut self, mut c: Option<&mut Ctx>, p: usize) -> Result<Arc<Vec<u8>>, u32> {
        let d = self.by_order[p];
        let t = self.v.samples[d].pts.max(0) as u64;
        let key = self.key_before(d);
        let restart = match &self.soft {
            None => true,
            Some(s) => s.last.is_some_and(|l| t < l) || (key > s.next && s.pics.back().is_none_or(|x| x.0 < t)),
        };
        if restart {
            let info = self.v.wm.as_ref().ok_or(E_FAIL)?;
            let mut codec = match self.soft.take() {
                Some(s) => s.codec,
                None => crate::wmdec::codec(info).ok_or(MF_E_INVALIDMEDIATYPE)?,
            };
            codec.flush();
            self.soft = Some(Soft { codec, next: key, pics: VecDeque::new(), last: None });
        }
        let n = self.v.samples.len();
        loop {
            let s = self.soft.as_mut().unwrap();
            while s.pics.len() > 1 && s.pics[1].0 <= t {
                s.pics.pop_front();
            }
            if let Some(&(pt, ref b)) = s.pics.front() {
                // (the picture whose time has come: at `t`, or the last before what comes next)
                if pt >= t || s.pics.len() > 1 || s.next >= n {
                    s.last = Some(t);
                    return Ok(b.clone());
                }
            }
            if s.next >= n {
                let more = s.codec.drain();
                let got = push_pictures(s, more);
                if !got && s.pics.is_empty() {
                    return Err(E_FAIL);
                }
                s.next = n + 1;
                continue;
            }
            let i = s.next;
            s.next += 1;
            let smp = &self.v.samples[i];
            let (time, key) = (smp.pts.max(0) as u64, smp.key);
            let b = self.data.read(c.as_deref_mut(), smp.off, smp.size as usize).ok_or(E_FAIL)?;
            let s = self.soft.as_mut().unwrap();
            let fs = s.codec.decode(&b, time, key);
            push_pictures(s, fs);
        }
    }
}

/// The pictures of `fs` into `s`'s, in time order; whether there were any.
fn push_pictures(s: &mut Soft, fs: Vec<crate::wmdec::Frame>) -> bool {
    let mut any = false;
    for f in fs {
        if let crate::wmdec::Frame::Picture { time, bgra, .. } = f {
            let at = s.pics.iter().position(|x| x.0 > time).unwrap_or(s.pics.len());
            s.pics.insert(at, (time, bgra));
            any = true;
        }
    }
    any
}

/// A B G R A picture (top-down) as a sample of FOURCC `sub`: RGB32 as it is, the YUV formats
/// by BT.601 video range (convert's RGBA planes).
fn soft_convert(bgra: &[u8], w: u32, h: u32, sub: u32) -> Vec<u8> {
    let n = 4 * w as usize * h as usize;
    let bgra = &bgra[..n.min(bgra.len())];
    if sub == source::RGB32 || sub == source::ARGB32 {
        let mut v = bgra.to_vec();
        v.resize(n, 0);
        return v;
    }
    let mut rgba: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
    rgba.resize(n, 0);
    let layout = [[2, 0, 4 * w, 0, 0, 0, 0], [0; 7]];
    // SAFETY: the buffer holds the RGBA plane the layout says
    unsafe { convert(&layout, rgba.as_ptr(), w, h, sub) }
}

/// A slot's planes (kura's layout: format, then offset and stride of 3 planes) as a sample of
/// FOURCC `sub`, rows top-down and tight (the type's stride).
///
/// # Safety
/// `buf` must hold the planes `layout` says.
pub unsafe fn convert(layout: &[[u32; 7]; 2], buf: *const u8, w: u32, h: u32, sub: u32) -> Vec<u8> {
    let l = &layout[0];
    let (w, h) = (w as usize, h as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let plane = |i: usize| buf.add(l[1 + 2 * i] as usize);
    let stride = |i: usize| l[2 + 2 * i] as usize;
    if sub == source::RGB32 || sub == source::ARGB32 {
        let mut out = vec![0u8; 4 * w * h];
        let m = if h >= 720 { kura::video::Out::Rgb709 } else { kura::video::Out::Rgb601 };
        kura::video::convert(buf, layout, 1, w as u32, h as u32, out.as_mut_ptr(), 4 * w, w as u32, h as u32, m);
        return out;
    }
    // Y, Cb, Cr of pixel (x, y), and of the chroma sample (x, y) (x, y < cw, ch)
    const I420F: u32 = 0;
    const NV12F: u32 = 1;
    let luma = |x: usize, y: usize| -> u8 {
        match l[0] {
            I420F | NV12F => *plane(0).add(y * stride(0) + x),
            _ => {
                let q = plane(0).add(y * stride(0) + 4 * x);
                rgb_ycc(*q, *q.add(1), *q.add(2)).0
            }
        }
    };
    let chroma = |x: usize, y: usize| -> (u8, u8) {
        match l[0] {
            I420F => (*plane(1).add(y * stride(1) + x), *plane(2).add(y * stride(2) + x)),
            NV12F => {
                let q = plane(1).add(y * stride(1) + 2 * x);
                (*q, *q.add(1))
            }
            _ => {
                let (xx, yy) = ((2 * x).min(w - 1), (2 * y).min(h - 1));
                let q = plane(0).add(yy * stride(0) + 4 * xx);
                let (_, u, v) = rgb_ycc(*q, *q.add(1), *q.add(2));
                (u, v)
            }
        }
    };
    let mut out = Vec::with_capacity(w * h * 2);
    if sub == source::YUY2 {
        let sw = w.next_multiple_of(2);
        for y in 0..h {
            for x in (0..sw).step_by(2) {
                let (u, v) = chroma(x / 2, y / 2);
                out.extend_from_slice(&[luma(x.min(w - 1), y), u, luma((x + 1).min(w - 1), y), v]);
            }
        }
        return out;
    }
    for y in 0..h {
        if matches!(l[0], I420F | NV12F) {
            out.extend_from_slice(std::slice::from_raw_parts(plane(0).add(y * stride(0)), w));
        } else {
            out.extend((0..w).map(|x| luma(x, y)));
        }
    }
    match sub {
        source::NV12 => {
            for y in 0..ch {
                for x in 0..cw {
                    let (u, v) = chroma(x, y);
                    out.extend_from_slice(&[u, v]);
                }
            }
        }
        _ => {
            // I420 / IYUV: Cb then Cr; YV12: Cr then Cb
            let first_v = sub == source::YV12;
            for k in 0..2 {
                for y in 0..ch {
                    for x in 0..cw {
                        let (u, v) = chroma(x, y);
                        out.push(if (k == 0) != first_v { u } else { v });
                    }
                }
            }
        }
    }
    out
}

/// BT.601 video range of an RGB pixel (frames the page could only give as RGBA).
fn rgb_ycc(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (r, g, b) = (r as f64, g as f64, b as f64);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let c = |x: f64| x.round().clamp(0.0, 255.0) as u8;
    (c(16.0 + y * 219.0 / 255.0), c(128.0 + (b - y) / 1.772 * 224.0 / 255.0), c(128.0 + (r - y) / 1.402 * 224.0 / 255.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A w x h I420 or NV12 picture of pseudo-random bytes, with padded strides; its layout.
    fn picture(w: usize, h: usize, nv12: bool, pad: usize, seed: u64) -> (Vec<u8>, [[u32; 7]; 2]) {
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let ys = w + pad;
        let mut x = seed | 1;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        };
        let (n, layout) = if nv12 {
            let cs = 2 * cw + pad;
            (ys * h + cs * ch, [1, 0, ys as u32, (ys * h) as u32, cs as u32, 0, 0])
        } else {
            let cs = cw + pad;
            (ys * h + 2 * cs * ch, [0, 0, ys as u32, (ys * h) as u32, cs as u32, (ys * h + cs * ch) as u32, cs as u32])
        };
        ((0..n).map(|_| rnd()).collect(), [layout, [0; 7]])
    }
    fn at(b: &[u8], l: &[u32; 7], i: usize, x: usize, y: usize) -> u8 {
        b[l[1 + 2 * i] as usize + y * l[2 + 2 * i] as usize + x]
    }

    proptest! {
        /// Every output format from I420 and NV12 slots: planes where the format puts them,
        /// each byte the slot's (the 4:2:0 formats), the sizes of MFCalculateImageSize.
        #[test]
        fn yuv_layouts(w in 1usize..24, h in 1usize..12, nv12 in any::<bool>(), pad in 0usize..4, seed in any::<u64>()) {
            let (b, layout) = picture(w, h, nv12, pad, seed);
            let l = &layout[0];
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let cb = |x: usize, y: usize| if nv12 { at(&b, l, 1, 2 * x, y) } else { at(&b, l, 1, x, y) };
            let cr = |x: usize, y: usize| if nv12 { at(&b, l, 1, 2 * x + 1, y) } else { at(&b, l, 2, x, y) };
            for sub in [source::NV12, source::I420, source::IYUV, source::YV12, source::YUY2] {
                let o = unsafe { convert(&layout, b.as_ptr(), w as u32, h as u32, sub) };
                if sub != source::YUY2 {
                    for y in 0..h {
                        for x in 0..w {
                            prop_assert_eq!(o[y * w + x], at(&b, l, 0, x, y));
                        }
                    }
                }
                let base = w * h;
                for y in 0..ch {
                    for x in 0..cw {
                        match sub {
                            source::NV12 => {
                                let s = 2 * cw;
                                prop_assert_eq!((o[base + y * s + 2 * x], o[base + y * s + 2 * x + 1]), (cb(x, y), cr(x, y)));
                            }
                            source::YV12 => prop_assert_eq!((o[base + y * cw + x], o[base + cw * ch + y * cw + x]), (cr(x, y), cb(x, y))),
                            source::YUY2 => {
                                let s = 2 * w.next_multiple_of(2);
                                prop_assert_eq!((o[2 * y * s + 4 * x + 1], o[2 * y * s + 4 * x + 3]), (cb(x, y), cr(x, y)));
                            }
                            _ => prop_assert_eq!((o[base + y * cw + x], o[base + cw * ch + y * cw + x]), (cb(x, y), cr(x, y))),
                        }
                    }
                }
                if w % 2 == 0 && h % 2 == 0 {
                    prop_assert_eq!(Some(o.len() as u32), plat::image_size(sub, w as u32, h as u32), "{}", sub);
                }
            }
        }
    }

    /// RGB32 by BT.709 for HD, 601 below (kura's tables).
    #[test]
    fn rgb_by_size() {
        for (w, h, f) in [(4usize, 720usize, kura::video::ycc709_to_rgb as fn(u8, u8, u8) -> (u8, u8, u8)), (4, 480, kura::video::ycc601_to_rgb)] {
            let mut b = vec![81u8; w * h];
            b.extend(std::iter::repeat_n(90u8, w * h / 4));
            b.extend(std::iter::repeat_n(240u8, w * h / 4));
            let layout = [[0, 0, w as u32, (w * h) as u32, (w / 2) as u32, (w * h + w * h / 4) as u32, (w / 2) as u32], [0; 7]];
            let o = unsafe { convert(&layout, b.as_ptr(), w as u32, h as u32, source::RGB32) };
            let (r, g, bl) = f(81, 90, 240);
            assert_eq!(&o[..4], &[bl, g, r, 255], "{h}");
            assert_eq!(o.len(), 4 * w * h);
        }
    }

    fn wmv() -> VStream {
        let b = include_bytes!("testdata/colors.wmv").to_vec();
        let m = source::open_media(None, &Data::mem_arc(Arc::new(b))).unwrap();
        let mp4::Track::Video(v) = &m.tracks[0] else { panic!() };
        VStream::new(v.clone(), m.packed.clone().unwrap())
    }
    /// The colour of a B G R A picture's middle: 0 red, 1 lime, 2 blue (testdata/colors_gen.sh).
    fn colour(bgra: &[u8]) -> usize {
        let o = 4 * (24 * 64 + 32);
        let (b, g, r) = (bgra[o] as i32, bgra[o + 1] as i32, bgra[o + 2] as i32);
        let near = |x: [i32; 3]| (x[0] - r).abs() + (x[1] - g).abs() + (x[2] - b).abs();
        let d = [near([255, 0, 0]), near([0, 255, 0]), near([0, 0, 255])];
        let k = (0..3).min_by_key(|&k| d[k]).unwrap();
        assert!(d[k] < 60, "{:?}", (r, g, b));
        k
    }

    /// colors.wmv through the MF source: WMV2, its size and frames; decoded here (no page) to
    /// RGB32 in presentation order, 6 frames of each colour, each at its time.
    #[test]
    fn a_wmv_decodes_in_order() {
        let mut s = wmv();
        let wm = s.v.wm.clone().unwrap();
        assert!(matches!(wm, crate::wmdec::Info::Video { fourcc, .. } if fourcc == u32::from_le_bytes(*b"WMV2")));
        assert_eq!((s.v.codec.as_str(), s.v.w, s.v.h, s.v.samples.len()), ("wmv2", 64, 48, 18));
        assert!(s.v.samples[0].key && s.v.samples[6].key);
        assert_eq!(s.v.rate, (24, 1));
        let t0 = s.v.samples[0].pts as u64;
        assert!(t0 < 1_000_000 && s.duration() - t0 >= 7_400_000 && s.duration() - t0 <= 7_600_000, "{t0} {}", s.duration());
        s.set_out(VOut::Decoded(source::RGB32));
        let mut last = None;
        for i in 0..18 {
            let p = s.read(None).unwrap().unwrap();
            assert_eq!(p.bytes.len(), 64 * 48 * 4);
            assert_eq!(colour(&p.bytes), i / 6, "frame {i}");
            assert!(last.is_none_or(|l| l < p.time) && p.dur > 0);
            assert!((p.time - t0).abs_diff(i as u64 * 416_667) <= 10_000, "time of frame {i}: {}", p.time);
            last = Some(p.time);
        }
        assert!(s.read(None).unwrap().is_none());
        // native: the bitstream as the file has it
        s.set_out(VOut::Native);
        let p = s.read(None).unwrap().unwrap();
        assert!(!p.bytes.is_empty() && p.bytes.len() < 64 * 48);
    }

    proptest! {
        /// Any cut or damage of colors.wmv: opening it as media and decoding its frames never
        /// panics, and what opens has its samples inside the packed bytes.
        #[test]
        fn a_damaged_wmv_never_panics(cut in 0usize..11_000, flips in prop::collection::vec((0usize..11_000, any::<u8>()), 0..8), rgb in any::<bool>()) {
            let mut b = include_bytes!("testdata/colors.wmv").to_vec();
            for (o, x) in flips {
                let n = b.len();
                b[o % n] ^= x;
            }
            b.truncate(cut.min(b.len()));
            if let Some(m) = source::open_asf(&b) {
                let packed = m.packed.clone().unwrap();
                for t in &m.tracks {
                    if let mp4::Track::Video(v) = t {
                        for x in &v.samples {
                            prop_assert!(packed.read(None, x.off, x.size as usize).is_some_and(|r| r.len() == x.size as usize));
                        }
                        let mut s = VStream::new(v.clone(), packed.clone());
                        s.set_out(VOut::Decoded(if rgb { source::RGB32 } else { source::NV12 }));
                        for _ in 0..40 {
                            match s.read(None) {
                                Ok(Some(p)) => prop_assert!(!p.bytes.is_empty()),
                                _ => break,
                            }
                        }
                    }
                }
            }
        }

        /// Frames of a WMV wanted in any order (seeks back, jumps over key frames) and as any
        /// 4:2:0 or RGB format: what reading from the start gives.
        #[test]
        fn wmv_frames_in_any_order(ps in prop::collection::vec(0usize..18, 1..24), nv12 in any::<bool>()) {
            let mut s = wmv();
            s.set_out(VOut::Decoded(source::RGB32));
            let all: Vec<Vec<u8>> = (0..18).map(|_| s.read(None).unwrap().unwrap().bytes).collect();
            let mut s = wmv();
            let sub = if nv12 { source::NV12 } else { source::RGB32 };
            s.set_out(VOut::Decoded(sub));
            for p in ps {
                let got = s.frame(None, p).unwrap().unwrap().bytes;
                if nv12 {
                    prop_assert_eq!(got, soft_convert(&all[p], 64, 48, source::NV12), "frame {}", p);
                } else {
                    prop_assert_eq!(&got, &all[p], "frame {}", p);
                }
            }
        }
    }
}
