//! Decoders for Windows Media streams: what wmvcore.rs's readers give as uncompressed output
//! and what the decoder DMOs (dshow_dmo.rs) hand downstream. A codec (`Codec`) turns a
//! compressed object into pictures (BGRA, top-down) or sound (16-bit PCM); `Decoder` puts them
//! into the output type the program chose (RGB24 / RGB32 bottom-up, PCM).
//!
//! Uncompressed streams (BI_RGB pictures, WAVE_FORMAT_PCM) pass through here; compressed
//! ones go to the codec backend (`backend`).

use std::sync::Arc;
use crate::wmvcore::{MediaType, FORMAT_VIDEO_INFO, FORMAT_WAVE_FORMAT_EX, MEDIATYPE_AUDIO, MEDIATYPE_VIDEO, SUBTYPE_PCM, SUBTYPE_RGB24, SUBTYPE_RGB32};

/// What a codec is given of a stream.
#[derive(Clone, Debug, PartialEq)]
pub enum Info {
    /// FOURCC (biCompression), picture size, the bytes after the BITMAPINFOHEADER (codec data)
    Video { fourcc: u32, width: u32, height: u32, bits: u16, extra: Vec<u8> },
    /// wFormatTag, channels, rate, bits, block align, bytes a second, the bytes after the
    /// WAVEFORMATEX (codec data)
    Audio { tag: u16, channels: u16, rate: u32, bits: u16, align: u16, bytes_per_sec: u32, extra: Vec<u8> },
}

impl Info {
    /// From a BITMAPINFOHEADER and what follows it.
    pub fn from_bih(bih: &[u8]) -> Option<Info> {
        // (a header shorter than BITMAPINFOHEADER's 40 bytes is no picture format)
        if bih.len() < 40 {
            return None;
        }
        let r32 = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(bih.get(o..o + 4)?.try_into().ok()?)) };
        let size = r32(0)?;
        let width = r32(4)?;
        let height = (r32(8)? as i32).unsigned_abs();
        let bits = u16::from_le_bytes(bih.get(14..16)?.try_into().ok()?);
        let fourcc = r32(16)?;
        // biSize counts the 40-byte header and the codec data after it.
        let extra = bih.get(40..(size as usize).max(40).min(bih.len())).unwrap_or(&[]).to_vec();
        Some(Info::Video { fourcc, width, height, bits, extra })
    }
    /// From a WAVEFORMATEX and what follows it.
    pub fn from_wave(w: &[u8]) -> Option<Info> {
        let r16 = |o: usize| Some(u16::from_le_bytes(w.get(o..o + 2)?.try_into().ok()?));
        let r32 = |o: usize| Some(u32::from_le_bytes(w.get(o..o + 4)?.try_into().ok()?));
        let cb = r16(16).unwrap_or(0) as usize;
        let extra = w.get(18..(18 + cb).min(w.len())).unwrap_or(&[]).to_vec();
        Some(Info::Audio { tag: r16(0)?, channels: r16(2)?, rate: r32(4)?, bytes_per_sec: r32(8)?, align: r16(12)?, bits: r16(14)?, extra })
    }
    /// From a stream of an ASF file.
    pub fn of_stream(s: &asf::Stream) -> Option<Info> {
        match s.kind {
            asf::Kind::Video { .. } => Info::from_bih(&s.format),
            asf::Kind::Audio => Info::from_wave(&s.format),
            _ => None,
        }
    }
    /// From a DirectShow / WM media type (VIDEOINFOHEADER or WAVEFORMATEX).
    pub fn of_type(t: &MediaType) -> Option<Info> {
        if t.format_type == FORMAT_VIDEO_INFO {
            Info::from_bih(t.format.get(48..)?)
        } else if t.format_type == FORMAT_WAVE_FORMAT_EX {
            Info::from_wave(&t.format)
        } else {
            None
        }
    }
}

/// What a codec gives.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// a picture `w` x `h`, B G R A bytes, top row first; its time (100 ns)
    /// (shared: a repeated picture is the same bytes, not a copy)
    Picture { time: u64, w: u32, h: u32, bgra: Arc<Vec<u8>> },
    /// interleaved 16-bit samples; the time of the first
    Sound { time: u64, channels: u16, rate: u32, pcm: Vec<i16> },
}

pub trait Codec: Send {
    /// Decodes one compressed object (`key`: a clean point) of time `time`.
    fn decode(&mut self, data: &[u8], time: u64, key: bool) -> Vec<Frame>;
    /// What it holds back, at the end of the stream.
    fn drain(&mut self) -> Vec<Frame> {
        Vec::new()
    }
    /// Forgets the stream so far (a seek).
    fn flush(&mut self) {}
}

/// Uncompressed pictures (BI_RGB 24 / 32 bits, bottom-up).
struct RawVideo {
    w: u32,
    h: u32,
    bits: u16,
}

impl Codec for RawVideo {
    fn decode(&mut self, data: &[u8], time: u64, _key: bool) -> Vec<Frame> {
        let bpp = (self.bits / 8) as usize;
        if bpp != 3 && bpp != 4 {
            return Vec::new();
        }
        let (w, h) = (self.w as usize, self.h as usize);
        // (the sizes come from the file: products are checked, a picture is at most MAX_SIDE)
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Vec::new();
        }
        let stride = (w * bpp).next_multiple_of(4);
        if data.len() < stride * h {
            return Vec::new();
        }
        let mut bgra = vec![0u8; w * h * 4];
        for (y, row) in bgra.chunks_exact_mut(w * 4).enumerate() {
            let src = &data[(h - 1 - y) * stride..][..w * bpp];
            for (d, s) in row.chunks_exact_mut(4).zip(src.chunks_exact(bpp)) {
                d[..3].copy_from_slice(&s[..3]);
                d[3] = 255;
            }
        }
        vec![Frame::Picture { time, w: self.w, h: self.h, bgra: Arc::new(bgra) }]
    }
}

/// 16-bit PCM.
struct RawAudio {
    channels: u16,
    rate: u32,
}

impl Codec for RawAudio {
    fn decode(&mut self, data: &[u8], time: u64, _key: bool) -> Vec<Frame> {
        let pcm = data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
        vec![Frame::Sound { time, channels: self.channels, rate: self.rate, pcm }]
    }
}

/// The codec for a stream, if any decodes it.
pub fn codec(i: &Info) -> Option<Box<dyn Codec>> {
    match i {
        Info::Video { fourcc: 0, width, height, bits, .. } if *bits == 24 || *bits == 32 => Some(Box::new(RawVideo { w: *width, h: *height, bits: *bits })),
        Info::Audio { tag: 1, channels, rate, bits: 16, .. } => Some(Box::new(RawAudio { channels: *channels, rate: *rate })),
        _ => backend(i),
    }
}

/// WMA 1 / 2 (crates/wmcodec): a media object is whole packets of `block_align` bytes.
struct Wma {
    d: wmcodec::wma::Decoder,
    align: usize,
    /// where the samples given so far end (100 ns)
    end: u64,
}

impl Codec for Wma {
    fn decode(&mut self, data: &[u8], time: u64, _key: bool) -> Vec<Frame> {
        let mut pcm = Vec::new();
        for p in data.chunks(self.align.max(1)) {
            // (a damaged packet: its samples are lost, the stream goes on)
            if let Ok(s) = self.d.decode(p) {
                pcm.extend(s);
            }
        }
        let frames = pcm.len() as u64 / self.d.channels.max(1) as u64;
        self.end = time + frames * 10_000_000 / self.d.rate.max(1) as u64;
        vec![Frame::Sound { time, channels: self.d.channels as u16, rate: self.d.rate, pcm }]
    }
    fn drain(&mut self) -> Vec<Frame> {
        vec![Frame::Sound { time: self.end, channels: self.d.channels as u16, rate: self.d.rate, pcm: self.d.drain() }]
    }
    fn flush(&mut self) {
        self.d.flush();
    }
}

/// MS-MPEG4 v3 / WMV 1 / WMV 2 (crates/wmcodec): one picture per object.
struct MsMpeg4 {
    d: wmcodec::msmpeg4::Decoder,
    /// the last picture (a WMV2 "skipped" picture repeats it)
    last: Option<Arc<Vec<u8>>>,
}

impl Codec for MsMpeg4 {
    fn decode(&mut self, data: &[u8], time: u64, _key: bool) -> Vec<Frame> {
        let (w, h) = match self.d.decode(data) {
            Ok(Some(p)) => {
                self.last = Some(Arc::new(yuv_to_bgra(&p)));
                (p.width as u32, p.height as u32)
            }
            // (a skipped picture, or a damaged one: the last again)
            _ => match self.d.size() {
                (w, h) if self.last.is_some() => (w as u32, h as u32),
                _ => return Vec::new(),
            },
        };
        vec![Frame::Picture { time, w, h, bgra: self.last.clone().unwrap_or_default() }]
    }
}

/// WMV 9 (WMV3, VC-1 Simple / Main; crates/wmcodec::vc1). With B pictures the decoder hands
/// an I / P picture back when the next one comes, so it keeps that one's time.
struct Wmv3 {
    d: wmcodec::vc1::Decoder,
    w: usize,
    h: usize,
    extra: Vec<u8>,
    last: Option<Arc<Vec<u8>>>,
    /// the time of the I / P picture the decoder holds
    held: Option<u64>,
}

impl Codec for Wmv3 {
    fn decode(&mut self, data: &[u8], time: u64, _key: bool) -> Vec<Frame> {
        let got = self.d.decode(data);
        let (w, h) = (self.w as u32, self.h as u32);
        if !self.d.delayed() {
            match got {
                Ok(Some(p)) => self.last = Some(Arc::new(yuv_to_bgra(&p))),
                // (a damaged picture: the last again)
                _ if self.last.is_some() => {}
                _ => return Vec::new(),
            }
            return vec![Frame::Picture { time, w, h, bgra: self.last.clone().unwrap_or_default() }];
        }
        let Ok(got) = got else { return Vec::new() };
        let shown = if self.d.last_was_b() { Some(time) } else { self.held.replace(time) };
        match (got, shown) {
            (Some(p), Some(t)) => vec![Frame::Picture { time: t, w, h, bgra: Arc::new(yuv_to_bgra(&p)) }],
            _ => Vec::new(),
        }
    }
    fn drain(&mut self) -> Vec<Frame> {
        match (self.d.flush(), self.held.take()) {
            (Some(p), Some(t)) => vec![Frame::Picture { time: t, w: self.w as u32, h: self.h as u32, bgra: Arc::new(yuv_to_bgra(&p)) }],
            _ => Vec::new(),
        }
    }
    fn flush(&mut self) {
        if let Ok(d) = wmcodec::vc1::Decoder::new(self.w, self.h, &self.extra) {
            self.d = d;
        }
        self.held = None;
        self.last = None;
    }
}

/// BT.601 limited range YUV 4:2:0 to B G R A, top row first (what Windows' WMV decoder DMO
/// gives as RGB).
pub fn yuv_to_bgra(p: &wmcodec::msmpeg4::Picture) -> Vec<u8> {
    p.bgra()
}

const fn fcc(s: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*s)
}

/// The compressed codecs: WMA 1 / 2, MS-MPEG4 v3, WMV 1 / 2 / 3 (WMV 9 Simple / Main).
fn backend(i: &Info) -> Option<Box<dyn Codec>> {
    use wmcodec::msmpeg4::Version;
    match i {
        Info::Video { fourcc, width, height, extra, .. } => {
            let ver = match u32::from_le_bytes(fourcc.to_le_bytes().map(|b| b.to_ascii_uppercase())) {
                x if x == fcc(b"WMV1") => Version::Wmv1,
                x if x == fcc(b"WMV2") => Version::Wmv2,
                x if x == fcc(b"MP43") || x == fcc(b"DIV3") => Version::V3,
                x if x == fcc(b"WMV3") => {
                    let (w, h) = (*width as usize, (*height as i32).unsigned_abs() as usize);
                    let d = wmcodec::vc1::Decoder::new(w, h, extra).ok()?;
                    return Some(Box::new(Wmv3 { d, w, h, extra: extra.clone(), last: None, held: None }));
                }
                _ => return None,
            };
            let d = wmcodec::msmpeg4::Decoder::new(ver, *width as usize, (*height as i32).unsigned_abs() as usize, extra).ok()?;
            Some(Box::new(MsMpeg4 { d, last: None }))
        }
        Info::Audio { tag: tag @ (0x160 | 0x161), channels, rate, align, bytes_per_sec, extra, .. } => {
            let d = wmcodec::wma::Decoder::new(*tag, *channels, *rate, bytes_per_sec * 8, *align, extra).ok()?;
            Some(Box::new(Wma { d, align: *align as usize, end: 0 }))
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// output types

fn bih(w: u32, h: u32, bits: u16) -> Vec<u8> {
    let mut b = vec![0u8; 40];
    b[0..4].copy_from_slice(&40u32.to_le_bytes());
    b[4..8].copy_from_slice(&w.to_le_bytes());
    b[8..12].copy_from_slice(&h.to_le_bytes());
    b[12..14].copy_from_slice(&1u16.to_le_bytes());
    b[14..16].copy_from_slice(&bits.to_le_bytes());
    let size = (w * bits as u32 / 8).next_multiple_of(4) * h;
    b[20..24].copy_from_slice(&size.to_le_bytes());
    b
}

/// An uncompressed RGB picture type (`bits` 24 or 32, bottom-up).
pub fn rgb_type(w: u32, h: u32, bits: u16, time_per_frame: u64) -> MediaType {
    let size = (w * bits as u32 / 8).next_multiple_of(4) * h;
    MediaType {
        major: MEDIATYPE_VIDEO,
        sub: if bits == 32 { SUBTYPE_RGB32 } else { SUBTYPE_RGB24 },
        fixed: true,
        temporal: false,
        sample_size: size,
        format_type: FORMAT_VIDEO_INFO,
        format: crate::wmvcore::video_info(&bih(w, h, bits), w, h, 0, time_per_frame),
    }
}

/// A 16-bit PCM type.
pub fn pcm_type(channels: u16, rate: u32) -> MediaType {
    let align = channels * 2;
    let mut w = Vec::new();
    w.extend(1u16.to_le_bytes());
    w.extend(channels.to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend((rate * align as u32).to_le_bytes());
    w.extend(align.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(0u16.to_le_bytes());
    MediaType { major: MEDIATYPE_AUDIO, sub: SUBTYPE_PCM, fixed: true, temporal: false, sample_size: align as u32, format_type: FORMAT_WAVE_FORMAT_EX, format: w }
}

/// The types an output of stream `s` can give, the default first: RGB24 / RGB32 pictures,
/// PCM sound as the stream's channels and rate.
pub fn formats(s: &asf::Stream) -> Vec<MediaType> {
    match Info::of_stream(s) {
        Some(Info::Video { width, height, .. }) => vec![rgb_type(width, height, 24, s.avg_time_per_frame), rgb_type(width, height, 32, s.avg_time_per_frame)],
        Some(Info::Audio { channels, rate, .. }) => vec![pcm_type(channels.clamp(1, 2), rate)],
        _ => Vec::new(),
    }
}

pub fn default_output(s: &asf::Stream) -> Option<MediaType> {
    formats(s).into_iter().next()
}

/// The bytes of one sample of type `t` (a picture; sound: a second's worth).
pub fn sample_size(t: &MediaType) -> u32 {
    if t.major == MEDIATYPE_AUDIO {
        u32::from_le_bytes(t.format.get(8..12).and_then(|b| b.try_into().ok()).unwrap_or([0; 4]))
    } else {
        t.sample_size
    }
}

/// A stream's codec with its frames put into an output type.
pub trait Decoder: Send {
    /// time, duration, bytes of the samples one compressed object gives
    fn decode(&mut self, s: &asf::Sample) -> Vec<(u64, u64, Vec<u8>)>;
    fn drain(&mut self) -> Vec<(u64, u64, Vec<u8>)>;
    fn flush(&mut self);
}

struct Out {
    codec: Box<dyn Codec>,
    ty: MediaType,
    time_per_frame: u64,
}

/// The widest / tallest picture decoded or given (a file's or a program's sizes past it are
/// refused rather than allocated)
pub const MAX_SIDE: usize = 16384;

/// A picture as `bits`-bit RGB (32: B G R 0, else 24: B G R), bottom-up, of the output's size
/// (cut or padded). A `bgra` shorter than `w` x `h` pictures is cut to the rows it holds;
/// an output past MAX_SIDE is empty.
pub fn to_rgb(bgra: &[u8], w: u32, h: u32, ow: u32, oh: u32, bits: u16) -> Vec<u8> {
    if ow as usize > MAX_SIDE || oh as usize > MAX_SIDE {
        return Vec::new();
    }
    let mut out = vec![0u8; rgb_size(ow, oh, bits)];
    to_rgb_into(bgra, w, h, ow, oh, bits, &mut out);
    out
}
/// The bytes of `to_rgb`'s picture.
pub fn rgb_size(ow: u32, oh: u32, bits: u16) -> usize {
    let bpp = if bits == 32 { 4 } else { 3 };
    (ow as usize * bpp).next_multiple_of(4) * oh as usize
}
/// `to_rgb` into `out` (a sample's buffer, written straight: no picture-sized Vec on the way),
/// all of it written (what the picture does not cover 0; a `bgra` short of `w` x `h` is cut to
/// the rows it holds). False (nothing written) when `out` is shorter than the output, or the
/// output is past MAX_SIDE.
pub fn to_rgb_into(bgra: &[u8], w: u32, h: u32, ow: u32, oh: u32, bits: u16, out: &mut [u8]) -> bool {
    let bpp = if bits == 32 { 4 } else { 3 };
    let (w, ow, oh) = (w as usize, ow as usize, oh as usize);
    if ow > MAX_SIDE || oh > MAX_SIDE {
        return false;
    }
    let stride = (ow * bpp).next_multiple_of(4);
    let Some(out) = out.get_mut(..stride * oh) else { return false };
    let cols = ow.min(w);
    let rows = if w == 0 { 0 } else { oh.min(h as usize).min(bgra.len() / (w * 4)) };
    for y in 0..oh {
        let dst = &mut out[(oh - 1 - y) * stride..][..stride];
        if y >= rows {
            dst.fill(0);
            continue;
        }
        let src = &bgra[y * w * 4..][..cols * 4];
        for (d, s) in dst.chunks_exact_mut(bpp).zip(src.chunks_exact(4)) {
            d[..3].copy_from_slice(&s[..3]);
            if bpp == 4 {
                d[3] = 0;
            }
        }
        dst[cols * bpp..].fill(0);
    }
    true
}

impl Out {
    fn put(&self, f: Frame) -> Option<(u64, u64, Vec<u8>)> {
        match (f, Info::of_type(&self.ty)?) {
            (Frame::Picture { time, w, h, bgra }, Info::Video { width, height, bits, .. }) => Some((time, self.time_per_frame, to_rgb(&bgra, w, h, width, height, bits))),
            // (a codec that says 0 channels gives no sound)
            (Frame::Sound { channels: 0, .. }, _) => None,
            (Frame::Sound { time, channels, rate, pcm }, Info::Audio { channels: oc, .. }) => {
                let frames = pcm.len() / channels.max(1) as usize;
                let mut out = Vec::with_capacity(frames * oc as usize * 2);
                for k in 0..frames {
                    for ch in 0..oc as usize {
                        let v = pcm[k * channels as usize + ch.min(channels as usize - 1)];
                        out.extend(v.to_le_bytes());
                    }
                }
                Some((time, frames as u64 * 10_000_000 / rate.max(1) as u64, out))
            }
            _ => None,
        }
    }
}

impl Decoder for Out {
    fn decode(&mut self, s: &asf::Sample) -> Vec<(u64, u64, Vec<u8>)> {
        let fs = self.codec.decode(&s.data, s.time, s.key);
        fs.into_iter().filter_map(|f| self.put(f)).collect()
    }
    fn drain(&mut self) -> Vec<(u64, u64, Vec<u8>)> {
        let fs = self.codec.drain();
        fs.into_iter().filter_map(|f| self.put(f)).collect()
    }
    fn flush(&mut self) {
        self.codec.flush();
    }
}

/// The decoder of stream `s` into type `ty`, when a codec decodes it.
pub fn decoder(s: &asf::Stream, ty: &MediaType) -> Option<Box<dyn Decoder>> {
    let codec = codec(&Info::of_stream(s)?)?;
    Some(Box::new(Out { codec, ty: ty.clone(), time_per_frame: s.avg_time_per_frame }))
}

/// Any bytes as a stream's format and its compressed objects, decoded into an output type
/// whose sizes come from the bytes too (fuzz/fuzz_targets/wmdec.rs): no panic, and every
/// picture the right size for its output.
#[doc(hidden)]
pub fn fuzz(data: &[u8]) {
    let (&sel, rest) = match data.split_first() {
        Some(x) => x,
        None => return,
    };
    let hl = (sel as usize % 96).min(rest.len());
    let (head, body) = rest.split_at(hl);
    let info = if sel & 0x80 == 0 { Info::from_bih(head) } else { Info::from_wave(head) };
    let Some(info) = info else { return };
    // (decoders of real sizes only: a fuzzed 65536 x 65536 is refused by the codecs' own checks)
    if let Info::Video { width, height, .. } = info {
        if width as usize > 512 || height as usize > 512 {
            return;
        }
    }
    let Some(codec) = codec(&info) else { return };
    let (ow, oh) = (u32::from(body.first().copied().unwrap_or(0) % 64), u32::from(body.get(1).copied().unwrap_or(0) % 64));
    let bits = if sel & 0x40 == 0 { 24 } else { 32 };
    let ty = match info {
        Info::Video { .. } => rgb_type(ow, oh, bits, 0),
        Info::Audio { .. } => pcm_type(u16::from(sel & 3), 8000),
    };
    let mut out = Out { codec, ty, time_per_frame: 0 };
    let stride = (ow as usize * (bits as usize / 8)).next_multiple_of(4);
    for (i, chunk) in body.chunks(97).enumerate() {
        let x = asf::Sample { stream: 1, key: i == 0, time: i as u64 * 1000, data: chunk.to_vec() };
        for (_, _, b) in out.decode(&x) {
            if matches!(info, Info::Video { .. }) {
                assert_eq!(b.len(), stride * oh as usize);
            }
        }
    }
    let _ = out.drain();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn bih(size: u32, fourcc: &[u8; 4], tail: &[u8]) -> Vec<u8> {
        let mut b = vec![0u8; 40];
        b[0..4].copy_from_slice(&size.to_le_bytes());
        b[4..8].copy_from_slice(&800u32.to_le_bytes());
        b[8..12].copy_from_slice(&(-600i32).to_le_bytes());
        b[14..16].copy_from_slice(&24u16.to_le_bytes());
        b[16..20].copy_from_slice(fourcc);
        b.extend_from_slice(tail);
        b
    }

    /// 凛 Demo.wmv: biSize 44 = header + the 4 WMV2 extradata bytes (ffprobe: f7ff ad00).
    #[test]
    fn wmv2_extradata_is_inside_bisize() {
        let Some(Info::Video { fourcc, width, height, bits, extra }) = Info::from_bih(&bih(44, b"WMV2", &[0xf7, 0xff, 0xad, 0x00])) else { panic!() };
        assert_eq!((fourcc, width, height, bits), (u32::from_le_bytes(*b"WMV2"), 800, 600, 24));
        assert_eq!(extra, [0xf7, 0xff, 0xad, 0x00]);
        assert!(codec(&Info::from_bih(&bih(44, b"WMV2", &[0xf7, 0xff, 0xad, 0x00])).unwrap()).is_some());
    }

    /// A WMV file ffmpeg makes (None without ffmpeg).
    fn encoded(codec: &str, w: u32, h: u32) -> Option<Vec<u8>> {
        let dir = std::env::temp_dir().join(format!("wmdec-{}-{codec}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let out = dir.join("v.wmv");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc=size={w}x{h}:rate=15:duration=2"), "-c:v", codec, "-q:v", "6", "-g", "10"])
            .arg(&out)
            .status()
            .ok()?
            .success();
        let v = ok.then(|| std::fs::read(&out).ok()).flatten();
        let _ = std::fs::remove_dir_all(&dir);
        v
    }

    /// The DMO wrapper's path: the file's stream type picks the decoder, each object gives one
    /// picture of the stream's size, and a damaged object repeats the last picture.
    #[test]
    fn wmv_streams_give_a_picture_per_object() {
        for codec in ["wmv1", "wmv2", "msmpeg4"] {
            let Some(file) = encoded(codec, 176, 144) else {
                eprintln!("no ffmpeg: skipped");
                return;
            };
            let mut r = asf::Reader::new(file).unwrap();
            let s = r.header.streams.iter().find(|s| matches!(s.kind, asf::Kind::Video { .. })).unwrap().clone();
            let info = Info::of_stream(&s).unwrap();
            // (what the DMO wrapper is handed: the type wmvcore builds of the stream)
            let t = crate::wmvcore::stream_type(&s);
            assert_eq!(Info::of_type(&t), Some(info.clone()), "{codec}");
            let mut d = codec_for(&info).unwrap_or_else(|| panic!("{codec}: no decoder"));
            let mut n = 0;
            let mut last = Arc::new(Vec::new());
            while let Some(x) = r.next_sample() {
                if x.stream != s.number {
                    continue;
                }
                let f = d.decode(&x.data, n, n == 0);
                let [Frame::Picture { w: 176, h: 144, bgra, .. }] = &f[..] else { panic!("{codec}: object {n}") };
                assert_eq!(bgra.len(), 176 * 144 * 4);
                last = bgra.clone();
                n += 1;
            }
            assert_eq!(n, 30, "{codec}");
            let [Frame::Picture { bgra, .. }] = &d.decode(&[0xff; 7], n, false)[..] else { panic!("{codec}: damaged") };
            assert_eq!(bgra, &last, "{codec}");
            // (the repeat is the same bytes, not a copy)
            assert!(Arc::ptr_eq(bgra, &last), "{codec}");
        }
    }

    /// WMV3 with B pictures: pictures come out in display order with the times of the objects
    /// that carried them, the held one at the end; a seek forgets it.
    #[test]
    fn wmv3_b_pictures_keep_their_times() {
        use wmcodec::vc1::synth;
        let info = Info::from_bih(&bih(44, b"WMV3", &synth::extra(true))).unwrap();
        let Info::Video { width: 800, height: 600, .. } = info else { panic!() };
        let mut d = codec_for(&info).expect("WMV3 decoder");
        let mut times = Vec::new();
        for (k, t) in [('P', 0), ('P', 2000), ('B', 1000), ('P', 4000), ('B', 3000)] {
            for f in d.decode(&synth::picture(k, true, 38), t, false) {
                let Frame::Picture { time, w: 800, h: 600, bgra } = f else { panic!() };
                assert_eq!(bgra.len(), 800 * 600 * 4);
                // (Y 128 gray: (112 * 298 + 128) >> 8)
                assert!(bgra.chunks(4).all(|p| p == [130, 130, 130, 255]));
                times.push(time);
            }
        }
        for f in d.drain() {
            let Frame::Picture { time, .. } = f else { panic!() };
            times.push(time);
        }
        assert_eq!(times, [0, 1000, 2000, 3000, 4000]);
        d.decode(&synth::picture('P', true, 38), 5000, false);
        d.flush();
        assert!(d.drain().is_empty());
        // without B pictures: one picture per object, at its time
        let mut d = codec_for(&Info::from_bih(&bih(44, b"WMV3", &synth::extra(false))).unwrap()).unwrap();
        for t in [0, 1000, 2000] {
            let [Frame::Picture { time, .. }] = &d.decode(&synth::picture('P', false, 38), t, false)[..] else { panic!() };
            assert_eq!(*time, t);
        }
        // a damaged object repeats the last picture
        let [Frame::Picture { time: 3000, .. }] = &d.decode(&[0xff; 3], 3000, false)[..] else { panic!() };
    }

    #[test]
    fn fourccs_are_case_blind() {
        for f in [b"WMV1", b"wmv1", b"WMV2", b"wmv2", b"MP43", b"mp43", b"DIV3", b"div3"] {
            assert!(codec_for(&Info::from_bih(&bih(40, f, &[])).unwrap()).is_some() || f.eq_ignore_ascii_case(b"WMV2"), "{f:?}");
        }
        for f in [b"WMV3", b"wmv3"] {
            assert!(codec_for(&Info::from_bih(&bih(44, f, &wmcodec::vc1::synth::extra(false))).unwrap()).is_some(), "{f:?}");
        }
        // (WMV2 and WMV3 need their extradata; WVC1 / the Advanced profile is not ported)
        assert!(codec_for(&Info::from_bih(&bih(40, b"WMV2", &[])).unwrap()).is_none());
        assert!(codec_for(&Info::from_bih(&bih(40, b"WMV3", &[])).unwrap()).is_none());
        assert!(codec_for(&Info::from_bih(&bih(44, b"WVC1", &[0xc0, 0, 0, 0])).unwrap()).is_none());
    }

    fn codec_for(i: &Info) -> Option<Box<dyn Codec>> {
        codec(i)
    }

    proptest! {
        /// The extradata is bytes 40..biSize, cut to what is there; trailing bytes past biSize
        /// (a VIDEOINFOHEADER copied with padding) are not codec data.
        #[test]
        fn extra_is_header_tail(size in 0u32..200, tail in proptest::collection::vec(any::<u8>(), 0..100)) {
            let Some(Info::Video { extra, .. }) = Info::from_bih(&bih(size, b"WMV1", &tail)) else { panic!() };
            let n = (size as usize).saturating_sub(40).min(tail.len());
            prop_assert_eq!(&extra[..], &tail[..n]);
        }
    }
    proptest! {
        /// BT.601 limited range, within one step of the real-number formula.
        #[test]
        fn yuv_is_bt601(w in 1usize..9, h in 1usize..9, px in proptest::collection::vec(any::<u8>(), 3 * 81)) {
            let cw = w.div_ceil(2);
            let ch = h.div_ceil(2);
            let planes = [px[..w * h].to_vec(), px[81..81 + cw * ch].to_vec(), px[162..162 + cw * ch].to_vec()];
            let p = wmcodec::msmpeg4::Picture { width: w, height: h, stride: [w, cw], planes, key: true };
            let out = yuv_to_bgra(&p);
            for y in 0..h {
                for x in 0..w {
                    let yy = 1.164383 * (p.planes[0][y * w + x] as f64 - 16.0);
                    let u = p.planes[1][(y / 2) * cw + x / 2] as f64 - 128.0;
                    let v = p.planes[2][(y / 2) * cw + x / 2] as f64 - 128.0;
                    let want = [yy + 2.017232 * u, yy - 0.391762 * u - 0.812968 * v, yy + 1.596027 * v];
                    for k in 0..3 {
                        let got = out[(y * w + x) * 4 + k] as f64;
                        prop_assert!((got - want[k].clamp(0.0, 255.0)).abs() <= 1.0, "{x},{y} {k}: {got} vs {}", want[k]);
                    }
                    prop_assert_eq!(out[(y * w + x) * 4 + 3], 255);
                }
            }
        }
    }

    proptest! {
        /// any header bytes: a format or none, never a panic (20..39 bytes used to abort)
        #[test]
        fn any_header_is_safe(b in proptest::collection::vec(any::<u8>(), 0..120)) {
            let i = Info::from_bih(&b);
            prop_assert_eq!(i.is_some(), b.len() >= 40);
            if let Some(Info::Video { extra, .. }) = &i {
                prop_assert!(extra.len() <= b.len() - 40);
            }
            let _ = Info::from_wave(&b);
            if let Some(i) = i {
                if let Some(mut c) = codec_for(&i) {
                    let _ = c.decode(&b, 0, true);
                }
            }
        }

        /// to_rgb for any sizes, depth and (short) picture: the output's shape, the copied
        /// pixels where both pictures have them, never a panic
        #[test]
        fn to_rgb_any_shape(w in 0u32..9, h in 0u32..9, ow in 0u32..9, oh in 0u32..9, bits in prop_oneof![Just(8u16), Just(16), Just(24), Just(32), any::<u16>()], short in 0usize..40, seed in any::<u8>()) {
            let full = (w * h * 4) as usize;
            let px: Vec<u8> = (0..full.saturating_sub(short)).map(|i| (i as u8).wrapping_mul(31) ^ seed).collect();
            let out = to_rgb(&px, w, h, ow, oh, bits);
            let bpp = if bits == 32 { 4 } else { 3 };
            let stride = (ow as usize * bpp).next_multiple_of(4);
            prop_assert_eq!(out.len(), stride * oh as usize);
            let rows = if w == 0 { 0 } else { px.len() / (w as usize * 4) };
            for y in 0..oh.min(h) as usize {
                for x in 0..ow.min(w) as usize {
                    let d = &out[(oh as usize - 1 - y) * stride + x * bpp..][..3];
                    if y < rows {
                        prop_assert_eq!(d, &px[(y * w as usize + x) * 4..][..3]);
                    } else {
                        prop_assert_eq!(d, &[0u8; 3][..]);
                    }
                }
            }
        }

        /// sound into a PCM output: 0 channels from a codec give nothing (used to underflow),
        /// otherwise frames x output channels samples
        #[test]
        fn sound_into_any_channels(ch in 0u16..4, oc in 0u16..4, pcm in proptest::collection::vec(any::<i16>(), 0..40)) {
            let o = Out { codec: Box::new(RawAudio { channels: 1, rate: 8000 }), ty: pcm_type(oc, 8000), time_per_frame: 0 };
            let got = o.put(Frame::Sound { time: 0, channels: ch, rate: 8000, pcm: pcm.clone() });
            if ch == 0 {
                prop_assert!(got.is_none());
            } else {
                let (_, _, b) = got.unwrap();
                prop_assert_eq!(b.len(), pcm.len() / ch as usize * oc as usize * 2);
            }
        }
    }

    #[test]
    fn huge_outputs_are_refused() {
        assert!(to_rgb(&[0; 16], 2, 2, MAX_SIDE as u32 + 1, 2, 32).is_empty());
        assert!(to_rgb(&[0; 16], 2, 2, 2, u32::MAX, 24).is_empty());
        // a raw picture whose header says 65536 x 65536 is not decoded
        let mut c = RawVideo { w: 65536, h: 65536, bits: 32 };
        assert!(c.decode(&[0; 64], 0, true).is_empty());
        // short BITMAPINFOHEADERs (20..39 bytes) are no format
        for n in 0..40 {
            assert_eq!(Info::from_bih(&vec![0x28; n]), None, "{n}");
        }
    }

}

#[cfg(test)]
mod rgb_tests {
    use super::*;
    use proptest::prelude::*;

    /// to_rgb as it was.
    fn old_to_rgb(bgra: &[u8], w: u32, h: u32, ow: u32, oh: u32, bits: u16) -> Vec<u8> {
        let bpp = (bits / 8) as usize;
        let stride = (ow as usize * bpp).next_multiple_of(4);
        let mut out = vec![0u8; stride * oh as usize];
        for y in 0..oh.min(h) as usize {
            let dst = &mut out[(oh as usize - 1 - y) * stride..][..stride];
            for x in 0..ow.min(w) as usize {
                let s = &bgra[(y * w as usize + x) * 4..][..4];
                dst[x * bpp..x * bpp + 3].copy_from_slice(&s[..3]);
                if bpp == 4 {
                    dst[x * bpp + 3] = 0;
                }
            }
        }
        out
    }

    proptest! {
        /// A DMO's picture written straight into a sample (whatever its bytes were) is the
        /// picture to_rgb made; short buffers are left alone
        #[test]
        fn rgb_into_a_sample_is_to_rgb(w in 0u32..20, h in 0u32..20, ow in 0u32..24, oh in 0u32..24, bits in prop::sample::select(vec![32u16, 24]), seed in any::<u32>(), junk in any::<u8>()) {
            let bgra: Vec<u8> = (0..4 * w * h).map(|i| (i.wrapping_mul(2246822519).wrapping_add(seed) >> 11) as u8).collect();
            let want = old_to_rgb(&bgra, w, h, ow, oh, bits);
            prop_assert_eq!(rgb_size(ow, oh, bits), want.len());
            let mut got = vec![junk; want.len()];
            prop_assert!(to_rgb_into(&bgra, w, h, ow, oh, bits, &mut got));
            prop_assert_eq!(&got, &want);
            prop_assert_eq!(to_rgb(&bgra, w, h, ow, oh, bits), want.clone());
            if !want.is_empty() {
                let mut short = vec![junk; want.len() - 1];
                prop_assert!(!to_rgb_into(&bgra, w, h, ow, oh, bits, &mut short));
            }
        }
    }
}
