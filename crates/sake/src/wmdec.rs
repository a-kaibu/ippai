//! Decoders for Windows Media streams: what wmvcore.rs's readers give as uncompressed output
//! and what the decoder DMOs (dshow_dmo.rs) hand downstream. A codec (`Codec`) turns a
//! compressed object into pictures (BGRA, top-down) or sound (16-bit PCM); `Decoder` puts them
//! into the output type the program chose (RGB24 / RGB32 bottom-up, PCM).
//!
//! Uncompressed streams (BI_RGB pictures, WAVE_FORMAT_PCM) pass through here; compressed
//! ones go to the codec backend (`backend`).

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
        let r32 = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(bih.get(o..o + 4)?.try_into().ok()?)) };
        let size = r32(0)?;
        let width = r32(4)?;
        let height = (r32(8)? as i32).unsigned_abs();
        let bits = u16::from_le_bytes(bih.get(14..16)?.try_into().ok()?);
        let fourcc = r32(16)?;
        let extra = bih.get((size as usize).clamp(40, bih.len())..).unwrap_or(&[]).to_vec();
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
    Picture { time: u64, w: u32, h: u32, bgra: Vec<u8> },
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
        let stride = (self.w as usize * bpp).next_multiple_of(4);
        if data.len() < stride * self.h as usize {
            return Vec::new();
        }
        let mut bgra = vec![0u8; self.w as usize * self.h as usize * 4];
        for y in 0..self.h as usize {
            let src = &data[(self.h as usize - 1 - y) * stride..];
            for x in 0..self.w as usize {
                let d = &mut bgra[(y * self.w as usize + x) * 4..][..4];
                d[..3].copy_from_slice(&src[x * bpp..x * bpp + 3]);
                d[3] = 255;
            }
        }
        vec![Frame::Picture { time, w: self.w, h: self.h, bgra }]
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

/// The compressed codecs: WMA 1 / 2 (WMV next).
fn backend(i: &Info) -> Option<Box<dyn Codec>> {
    match i {
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

/// A picture as `bits`-bit RGB, bottom-up, of the output's size (cut or padded).
pub fn to_rgb(bgra: &[u8], w: u32, h: u32, ow: u32, oh: u32, bits: u16) -> Vec<u8> {
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

impl Out {
    fn put(&self, f: Frame) -> Option<(u64, u64, Vec<u8>)> {
        match (f, Info::of_type(&self.ty)?) {
            (Frame::Picture { time, w, h, bgra }, Info::Video { width, height, bits, .. }) => Some((time, self.time_per_frame, to_rgb(&bgra, w, h, width, height, bits))),
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
