//! The Source Reader (mfreadwrite) for sound files, as games decode their music and voices with
//! it: MFCreateSourceReaderFromURL / FromByteStream, its one audio stream, the native type, a
//! PCM or float type set as the current one, ReadSample in order, SetCurrentPosition, the
//! duration. The file is read here (crates/sake/src/audiofile.rs: WAVE, MP3, MP4/M4A) and
//! its packets decoded by the page's AudioDecoder (crates/kura/src/adec.rs); PCM is taken as it
//! is. Also the objects games use with it: media types (MFCreateMediaType,
//! MFCreateWaveFormatExFromMFMediaType, MFInitMediaTypeFromWaveFormatEx), samples and memory
//! buffers (MFCreateSample, MFCreateMemoryBuffer).
//!
//! Synchronous only (a reader with MF_SOURCE_READER_ASYNC_CALLBACK is refused), no video, no
//! resampling: a current type of another rate or channel count is refused (MF_E_INVALIDMEDIATYPE).
//! Windows' answers: tests/winref/probes/mfreader.c.

use super::*;
use crate::audiofile::{self, Codec, Track};
use std::sync::Arc;

const MF_E_INVALIDMEDIATYPE: u32 = 0xc00d_36b4;
const MF_E_INVALIDSTREAMNUMBER: u32 = 0xc00d_36b3;
const MF_E_NO_MORE_TYPES: u32 = 0xc00d_36b9;
const MF_E_UNSUPPORTED_SERVICE: u32 = 0xc00d_36ba;
const MF_E_NOTACCEPTING: u32 = 0xc00d_36b5;

const FIRST_AUDIO: u32 = 0xffff_fffd;
const ALL_STREAMS: u32 = 0xffff_fffe;
const MEDIASOURCE: u32 = 0xffff_ffff;
const ENDOFSTREAM: u32 = 2;
/// How long a read waits for the page's decoder.
const WAIT: std::time::Duration = std::time::Duration::from_secs(2);

fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> Guid {
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&d1.to_le_bytes());
    g[4..6].copy_from_slice(&d2.to_le_bytes());
    g[6..8].copy_from_slice(&d3.to_le_bytes());
    g[8..].copy_from_slice(&d4);
    g
}
/// MFAudioFormat_* (and MFMediaType_Audio): Data1 the WAVE_FORMAT tag, then the base GUID
fn audio_format(tag: u32) -> Guid {
    guid(tag, 0, 0x10, [0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71])
}
fn major_audio() -> Guid {
    audio_format(0x7364_7561)
}
fn tag_of(g: &Guid) -> Option<u32> {
    (g[4..] == audio_format(0)[4..]).then(|| u32::from_le_bytes(g[0..4].try_into().unwrap()))
}

pub fn k_major() -> Guid {
    guid(0x48eb_a18e, 0xf8c9, 0x4687, [0xbf, 0x11, 0x0a, 0x74, 0xc9, 0xf9, 0x6a, 0x8f])
}
pub fn k_subtype() -> Guid {
    guid(0xf7e3_4c9a, 0x42e8, 0x4714, [0xb7, 0x4b, 0xcb, 0x29, 0xd7, 0x2c, 0x35, 0xe5])
}
pub fn k_channels() -> Guid {
    guid(0x37e4_8bf5, 0x645e, 0x4c5b, [0x89, 0xde, 0xad, 0xa9, 0xe2, 0x9b, 0x69, 0x6a])
}
pub fn k_rate() -> Guid {
    guid(0x5fae_eae7, 0x0290, 0x4c31, [0x9e, 0x8a, 0xc5, 0x34, 0xf6, 0x8d, 0x9d, 0xba])
}
pub fn k_bits() -> Guid {
    guid(0xf2de_b57f, 0x40fa, 0x4764, [0xaa, 0x33, 0xed, 0x4f, 0x2d, 0x1f, 0xf6, 0x69])
}
pub fn k_align() -> Guid {
    guid(0x322d_e230, 0x9eeb, 0x43bd, [0xab, 0x7a, 0xff, 0x41, 0x22, 0x51, 0x54, 0x1d])
}
pub fn k_avg() -> Guid {
    guid(0x1aab_75c8, 0xcfef, 0x451c, [0xab, 0x95, 0xac, 0x03, 0x4b, 0x8e, 0x17, 0x31])
}
pub fn k_independent() -> Guid {
    guid(0xc917_3739, 0x5e56, 0x461c, [0xb7, 0x13, 0x46, 0xfb, 0x99, 0x5c, 0xb9, 0x5f])
}
pub fn k_mask() -> Guid {
    guid(0x55fb_5765, 0x644a, 0x4caf, [0x84, 0x79, 0x93, 0x89, 0x83, 0xbb, 0x15, 0x88])
}
pub fn k_user_data() -> Guid {
    guid(0xb6bc_765f, 0x4c3b, 0x40a4, [0xbd, 0x51, 0x25, 0x35, 0xb6, 0x6f, 0xe0, 0x9d])
}
fn k_duration() -> Guid {
    guid(0x6c99_0d33, 0xbb8e, 0x477a, [0x85, 0x98, 0x0d, 0x5d, 0x96, 0xfc, 0xd8, 0x8a])
}
fn k_characteristics() -> Guid {
    guid(0x6d23_f5c8, 0xc5d7, 0x4a9b, [0x99, 0x71, 0x5d, 0x11, 0xf8, 0xbc, 0xa8, 0x80])
}
fn k_async() -> Guid {
    guid(0x1e3d_beac, 0xbb43, 0x4c35, [0xb5, 0x07, 0xcd, 0x64, 0x44, 0x64, 0xc9, 0x65])
}

fn u32_of(s: &Store, k: &Guid) -> Option<u32> {
    match s.get(k) {
        Some(Val::U32(v)) => Some(*v),
        _ => None,
    }
}
fn guid_of(s: &Store, k: &Guid) -> Option<Guid> {
    match s.get(k) {
        Some(Val::Guid(g)) => Some(*g),
        _ => None,
    }
}

/// The native type of a track: what the file holds.
pub fn native_type(t: &Track) -> Store {
    let mut s = Store::default();
    s.set(k_major(), Val::Guid(major_audio()));
    let tag = match &t.codec {
        Codec::Pcm { float, .. } => {
            if *float {
                audiofile::FLOAT
            } else {
                audiofile::PCM
            }
        }
        Codec::Page { subtype, .. } => *subtype,
    };
    s.set(k_subtype(), Val::Guid(audio_format(tag)));
    s.set(k_channels(), Val::U32(t.channels));
    s.set(k_rate(), Val::U32(t.rate));
    match &t.codec {
        Codec::Pcm { bits, .. } => {
            let align = t.channels * *bits as u32 / 8;
            s.set(k_bits(), Val::U32(*bits as u32));
            s.set(k_align(), Val::U32(align));
            s.set(k_avg(), Val::U32(align * t.rate));
            s.set(k_independent(), Val::U32(1));
        }
        Codec::Page { desc, subtype, .. } => {
            s.set(k_avg(), Val::U32(t.avg));
            s.set(k_align(), Val::U32(1));
            if *subtype == audiofile::AAC {
                // (Windows' MP4 source: 16 bits and independent samples, so not "compressed" to
                // IsCompressedFormat; its MP3 source gives neither: winref mfreader)
                s.set(k_bits(), Val::U32(16));
                s.set(k_independent(), Val::U32(1));
                // HEAACWAVEINFO after its WAVEFORMATEX: payload type 0 (raw), profile 0xfe
                // (not given), then the AudioSpecificConfig
                let mut u = vec![0u8, 0, 0xfe, 0, 0, 0, 0, 0, 0, 0, 0, 0];
                u.extend(desc);
                s.set(k_user_data(), Val::Blob(u));
            }
        }
    }
    s
}

/// The current type for samples of `bits` (16-bit PCM, or 32-bit float).
#[cfg(test)]
pub fn pcm_type(t: &Track, bits: u32, float: bool) -> Store {
    pcm_type_at(t.rate, t.channels, bits, float)
}
/// The same at a rate and channel count of its own (the reader's resampler's output).
pub fn pcm_type_at(rate: u32, channels: u32, bits: u32, float: bool) -> Store {
    let mut s = Store::default();
    let align = channels * bits / 8;
    s.set(k_major(), Val::Guid(major_audio()));
    s.set(k_subtype(), Val::Guid(audio_format(if float { audiofile::FLOAT } else { audiofile::PCM })));
    s.set(k_channels(), Val::U32(channels));
    s.set(k_rate(), Val::U32(rate));
    s.set(k_bits(), Val::U32(bits));
    s.set(k_align(), Val::U32(align));
    s.set(k_avg(), Val::U32(align * rate));
    s.set(k_independent(), Val::U32(1));
    s
}

/// What a reader hands out.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Out {
    /// the packets as they are (the current type is the native one)
    Native,
    /// samples: 8/16/24/32-bit PCM or 32-bit float, at a rate and channel count (the
    /// track's, or other ones: Windows' reader puts its resampler in, winref mfreader)
    Pcm { bits: u32, float: bool, rate: u32, channels: u32 },
}

impl Out {
    /// PCM at the track's own rate and channels
    #[cfg(test)]
    pub fn pcm(t: &Track, bits: u32, float: bool) -> Out {
        Out::Pcm {
            bits,
            float,
            rate: t.rate,
            channels: t.channels,
        }
    }
}

/// A linear resampler and channel mixer kept across pieces (stereo to mono the mean, mono to
/// stereo both the same).
struct Resample {
    from_ch: usize,
    to_ch: usize,
    step: f64,
    /// where the next output falls, in input frames from the first of the next piece (-1: the
    /// last of the one before, `prev`)
    t: f64,
    prev: Vec<f32>,
}

impl Resample {
    fn new(from: (u32, u32), to: (u32, u32)) -> Resample {
        Resample {
            from_ch: from.1.max(1) as usize,
            to_ch: to.1.max(1) as usize,
            step: from.0 as f64 / to.0.max(1) as f64,
            t: 0.0,
            prev: Vec::new(),
        }
    }

    fn run(&mut self, v: &[f32]) -> Vec<f32> {
        let (fc, tc) = (self.from_ch, self.to_ch);
        let mixed: Vec<f32> = match (fc, tc) {
            (a, b) if a == b => v.to_vec(),
            (_, 1) => v.chunks_exact(fc).map(|f| f.iter().sum::<f32>() / fc as f32).collect(),
            (1, _) => v.iter().flat_map(|&x| std::iter::repeat_n(x, tc)).collect(),
            _ => v.chunks_exact(fc).flat_map(|f| (0..tc).map(move |c| f[c.min(fc - 1)])).collect(),
        };
        if self.step == 1.0 {
            return mixed;
        }
        let n = mixed.len() / tc;
        let mut out = Vec::new();
        let get = |prev: &[f32], i: i64, c: usize| if i < 0 { prev[c] } else { mixed[i as usize * tc + c] };
        loop {
            let i0 = self.t.floor() as i64;
            if i0 + 1 >= n as i64 || i0 < 0 && self.prev.is_empty() {
                break;
            }
            let f = (self.t - i0 as f64) as f32;
            for c in 0..tc {
                out.push(get(&self.prev, i0, c) * (1.0 - f) + get(&self.prev, i0 + 1, c) * f);
            }
            self.t += self.step;
        }
        if n > 0 {
            self.t -= n as f64;
            self.prev = mixed[(n - 1) * tc..].to_vec();
        }
        out
    }
}

/// One stream of a reader, kept free of the guest: packets in order, decoded as needed.
pub struct Stream {
    pub track: Track,
    data: Arc<Vec<u8>>,
    pub out: Out,
    /// next packet, the frame it starts at
    next: usize,
    /// the page's decoder (opened at the first read of a Page codec)
    dec: Option<u32>,
    /// decoded samples not handed out, interleaved; the frame the first of them is
    pending: VecDeque<f32>,
    at: u64,
    /// frames of packet `next` already gone (a PCM seek lands inside a packet)
    into: u64,
    /// to another rate or channel count
    rs: Option<Resample>,
    pub ended: bool,
}

impl Drop for Stream {
    fn drop(&mut self) {
        if let Some(d) = self.dec {
            kura::adec::close(d);
        }
    }
}

/// A sample: its time and duration (100 ns) and bytes.
#[derive(Debug, PartialEq)]
pub struct Piece {
    pub time: u64,
    pub dur: u64,
    pub bytes: Vec<u8>,
}

impl Stream {
    pub fn new(track: Track, data: Arc<Vec<u8>>) -> Stream {
        Stream {
            track,
            data,
            out: Out::Native,
            next: 0,
            dec: None,
            pending: VecDeque::new(),
            at: 0,
            into: 0,
            rs: None,
            ended: false,
        }
    }

    fn time(&self, frames: u64) -> u64 {
        (frames as u128 * 10_000_000 / self.track.rate.max(1) as u128) as u64
    }

    pub fn duration(&self) -> u64 {
        self.track.duration.unwrap_or_else(|| self.time(self.track.total_frames()))
    }

    /// Sets what reads hand out.
    pub fn set_out(&mut self, out: Out) {
        self.out = out;
        self.rs = match out {
            Out::Pcm { rate, channels, .. } if (rate, channels) != (self.track.rate, self.track.channels) => Some(Resample::new((self.track.rate, self.track.channels), (rate, channels))),
            _ => None,
        };
    }

    /// Goes to `t` (100 ns): from the packet it falls in; PCM to the frame, rounded up
    /// (Windows' WAVE source: a seek to 250 ms reads from just past it, winref mfreader).
    pub fn seek(&mut self, t: u64) {
        let pcm = matches!(self.track.codec, Codec::Pcm { .. });
        let exact = t as u128 * self.track.rate as u128;
        let want = if pcm { exact.div_ceil(10_000_000) } else { exact / 10_000_000 } as u64;
        let mut f = 0u64;
        let mut k = 0;
        while k < self.track.frames.len() && f + self.track.frames[k] as u64 <= want {
            f += self.track.frames[k] as u64;
            k += 1;
        }
        self.next = k;
        self.at = f;
        self.into = 0;
        if pcm && k < self.track.frames.len() {
            self.into = want - f;
            self.at = want;
        }
        self.pending.clear();
        self.ended = false;
        self.set_out(self.out);
        if let Some(d) = self.dec {
            kura::adec::reset(d);
        }
    }

    /// The next sample, or None at the end of the stream (Err: the page did not decode).
    pub fn read(&mut self) -> Result<Option<Piece>, u32> {
        let t = &self.track;
        let ch = t.channels.max(1) as usize;
        match (self.out, &t.codec) {
            (Out::Native, _) | (Out::Pcm { .. }, Codec::Pcm { .. }) => {
                let Some(&(o, n)) = t.packets.get(self.next) else {
                    self.ended = true;
                    return Ok(None);
                };
                let all = t.frames[self.next] as u64;
                let skip = self.into.min(all);
                let frames = all - skip;
                let per = if all > 0 { n as u64 / all } else { 0 };
                let raw = &self.data[(o + skip * per) as usize..(o + n as u64) as usize];
                let bytes = match (self.out, &t.codec) {
                    (Out::Pcm { bits, float, .. }, Codec::Pcm { bits: from, float: ffloat }) if (bits, float) != (*from as u32, *ffloat) || self.rs.is_some() => {
                        let mut v = Vec::new();
                        decode_pcm(raw, *from, *ffloat, &mut v);
                        if let Some(r) = &mut self.rs {
                            v = r.run(&v);
                        }
                        encode(&v, bits, float)
                    }
                    _ => raw.to_vec(),
                };
                self.into = 0;
                let p = Piece {
                    time: self.time(self.at),
                    dur: self.time(self.at + frames) - self.time(self.at),
                    bytes,
                };
                self.next += 1;
                self.at += frames;
                Ok(Some(p))
            }
            (Out::Pcm { bits, float, .. }, Codec::Page { codec, desc, .. }) => {
                let per = t.frames.first().copied().unwrap_or(1152).max(1) as usize;
                if self.dec.is_none() {
                    self.dec = Some(kura::adec::open(codec, t.rate, t.channels, desc).ok_or(MF_E_INVALIDMEDIATYPE)?);
                }
                let dec = self.dec.unwrap();
                // decode a few packets ahead of what is wanted
                while self.pending.len() < per * ch && self.next < t.packets.len() {
                    let end = (self.next + 8).min(t.packets.len());
                    let chunks: Vec<&[u8]> = t.packets[self.next..end].iter().map(|&(o, n)| &self.data[o as usize..(o + n as u64) as usize]).collect();
                    let mut got = Vec::new();
                    let ok = kura::adec::decode(dec, chunks, WAIT, &mut got);
                    self.pending.extend(got);
                    self.next = end;
                    if ok.is_none() {
                        if kura::adec::failed(dec) {
                            return Err(E_FAIL);
                        }
                        eprintln!("sake: mf reader: the page's decoder did not answer");
                        break;
                    }
                }
                if self.pending.is_empty() {
                    self.ended = true;
                    return Ok(None);
                }
                let n = (per * ch).min(self.pending.len()) / ch * ch;
                let n = if n == 0 { self.pending.len() } else { n };
                let mut v: Vec<f32> = self.pending.drain(..n).collect();
                let frames = (n / ch) as u64;
                if let Some(r) = &mut self.rs {
                    v = r.run(&v);
                }
                let p = Piece {
                    time: self.time(self.at),
                    dur: self.time(self.at + frames) - self.time(self.at),
                    bytes: encode(&v, bits, float),
                };
                self.at += frames;
                Ok(Some(p))
            }
        }
    }
}

/// PCM bytes as floats in [-1, 1).
fn decode_pcm(b: &[u8], bits: u16, float: bool, out: &mut Vec<f32>) {
    match (bits, float) {
        (32, true) => out.extend(b.chunks_exact(4).map(|x| f32::from_le_bytes(x.try_into().unwrap()))),
        (8, _) => out.extend(b.iter().map(|&x| (x as f32 - 128.0) / 128.0)),
        (16, _) => out.extend(b.chunks_exact(2).map(|x| i16::from_le_bytes([x[0], x[1]]) as f32 / 32768.0)),
        (24, _) => out.extend(b.chunks_exact(3).map(|x| (i32::from_le_bytes([0, x[0], x[1], x[2]]) >> 8) as f32 / 8_388_608.0)),
        _ => out.extend(b.chunks_exact(4).map(|x| i32::from_le_bytes(x.try_into().unwrap()) as f32 / 2_147_483_648.0)),
    }
}

/// Floats as PCM bytes (rounded and clipped as FFmpeg converts) or 32-bit floats.
pub fn encode(v: &[f32], bits: u32, float: bool) -> Vec<u8> {
    if float {
        return v.iter().flat_map(|x| x.to_le_bytes()).collect();
    }
    let q = |x: f32, scale: f64, lo: f64, hi: f64| (x as f64 * scale).round().clamp(lo, hi) as i64;
    match bits {
        8 => v.iter().map(|&x| (q(x, 128.0, -128.0, 127.0) + 128) as u8).collect(),
        24 => v.iter().flat_map(|&x| q(x, 8_388_608.0, -8_388_608.0, 8_388_607.0).to_le_bytes()[..3].to_vec()).collect(),
        32 => v.iter().flat_map(|&x| (q(x, 2_147_483_648.0, -2_147_483_648.0, 2_147_483_647.0) as i32).to_le_bytes()).collect(),
        _ => v.iter().flat_map(|&x| (q(x, 32768.0, -32768.0, 32767.0) as i16).to_le_bytes()).collect(),
    }
}

/// Which output a current type asks for: None when it can't be had from this track.
pub fn out_for(t: &Track, want: &Store) -> Option<Out> {
    if guid_of(want, &k_major()).is_some_and(|g| g != major_audio()) {
        return None;
    }
    let sub = guid_of(want, &k_subtype())?;
    // (PCM and float at another rate or 1 / 2 channels: through the resampler)
    let channels = u32_of(want, &k_channels()).unwrap_or(t.channels);
    let rate = u32_of(want, &k_rate()).unwrap_or(t.rate);
    let same = channels == t.channels && rate == t.rate;
    if !(1..=8).contains(&channels) || channels != t.channels && channels > 2 || !(1000..=384_000).contains(&rate) {
        return None;
    }
    let native = guid_of(&native_type(t), &k_subtype());
    match tag_of(&sub)? {
        audiofile::PCM => {
            let bits = u32_of(want, &k_bits()).unwrap_or(16);
            matches!(bits, 8 | 16 | 24 | 32).then_some(Out::Pcm { bits, float: false, rate, channels })
        }
        audiofile::FLOAT => matches!(u32_of(want, &k_bits()), None | Some(32)).then_some(Out::Pcm {
            bits: 32,
            float: true,
            rate,
            channels,
        }),
        _ if Some(sub) == native && same => Some(Out::Native),
        _ => None,
    }
}

// ---------------------------------------------------------------------------------------------
// the guest's objects

/// Readers: references, the stream (behind its own lock: a read waits for the page), whether
/// it is selected, its current type (a store of ours, the reader holds a reference).
struct Reader {
    refs: u32,
    stream: Arc<Mutex<Stream>>,
    selected: bool,
    current: u32,
}
static READERS: Mutex<Option<HashMap<u32, Reader>>> = Mutex::new(None);

fn reader<R>(b: u32, f: impl FnOnce(&mut Reader) -> R) -> Option<R> {
    READERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

/// IMFMediaType {44ae0fa8-ea31-4109-8d2e-4cae4997c555}, IMFSample {c40a00f2-...}: both
/// attribute stores with more methods; their QueryInterface answers for themselves too.
fn typed_qi(c: &mut Ctx, d1: u32, d2: u16, d3: u16) -> u32 {
    let (this, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(iid);
    if is_unknown(&g) || is(&g, 0x2cd2_d921, 0xc447, 0x44a7) || is(&g, d1, d2, d3) {
        attrs(this, |a| a.0 += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}

fn type_qi(c: &mut Ctx) -> u32 {
    typed_qi(c, 0x44ae_0fa8, 0xea31, 0x4109)
}
/// GetMajorType(GUID *)
fn mt_major(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    match store_of(c.a(0)).and_then(|s| guid_of(&s, &k_major())) {
        Some(g) => {
            mem::write(c.a(1), &g);
            0
        }
        None => MF_E_ATTRIBUTENOTFOUND,
    }
}
/// IsCompressedFormat(BOOL *): not PCM or float
fn mt_compressed(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let s = store_of(c.a(0)).unwrap_or_default();
    // (as Windows: compressed unless MF_MT_ALL_SAMPLES_INDEPENDENT is set, whatever the
    // subtype; AAC's native type has it, MP3's not: winref mfreader)
    w32(c.a(1), (u32_of(&s, &k_independent()).unwrap_or(0) == 0) as u32);
    0
}
/// IsEqual(IMFMediaType *, DWORD *flags): MF_MEDIATYPE_EQUAL_MAJOR_TYPES 1, FORMAT_TYPES 2,
/// FORMAT_DATA 4, FORMAT_USER_DATA 8; S_OK when all
fn mt_equal(c: &mut Ctx) -> u32 {
    let (other, out) = (c.a(1), c.a(2));
    if other == 0 || out == 0 {
        return E_POINTER;
    }
    let (Some(a), Some(b)) = (store_of(c.a(0)), store_of(other)) else { return E_INVALIDARG };
    let f = equal_flags(&a, &b);
    w32(out, f);
    if f == 0xf {
        0
    } else {
        S_FALSE
    }
}
pub fn equal_flags(a: &Store, b: &Store) -> u32 {
    let mut f = 0;
    let major = guid_of(a, &k_major());
    if major.is_some() && major == guid_of(b, &k_major()) {
        f |= 1;
    }
    let sub = guid_of(a, &k_subtype());
    if sub.is_some() && sub == guid_of(b, &k_subtype()) {
        f |= 2;
    }
    let ud = k_user_data();
    let strip = |s: &Store| Store {
        items: s.items.iter().filter(|i| i.0 != ud).cloned().collect(),
    };
    if strip(a).matches(&strip(b), 2) == Some(true) {
        f |= 4;
    }
    if a.get(&ud) == b.get(&ud) {
        f |= 8;
    }
    f
}

fn type_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (type_qi, 12);
        m.extend([(mt_major as Handler, 8), (mt_compressed, 8), (mt_equal, 12), (notimpl, 24), (notimpl, 24)]);
        vtable("IMFMediaType", &m)
    })
}
/// A media type holding `s` (one reference).
pub fn new_type(s: Store) -> u32 {
    let b = object(type_vtable(), 1);
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, s));
    b
}

fn MFCreateMediaType(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return E_POINTER;
    }
    putp(c.a(0), new_type(Store::default()));
    0
}

/// The WAVEFORMATEX (or WAVEFORMATEXTENSIBLE: more than 2 channels, more than 16 bits, or
/// asked with flag 1) of a type's store; None: not audio.
pub fn wave_format(s: &Store, force_ext: bool) -> Option<Vec<u8>> {
    if guid_of(s, &k_major()) != Some(major_audio()) {
        return None;
    }
    let tag = tag_of(&guid_of(s, &k_subtype())?)?;
    let ch = u32_of(s, &k_channels()).unwrap_or(0);
    let rate = u32_of(s, &k_rate()).unwrap_or(0);
    let bits = u32_of(s, &k_bits()).unwrap_or(0);
    let align = u32_of(s, &k_align()).unwrap_or(ch * bits / 8);
    let avg = u32_of(s, &k_avg()).unwrap_or(align * rate);
    let raw = tag == audiofile::PCM || tag == audiofile::FLOAT;
    let ext = raw && (force_ext || ch > 2 || bits > 16 && tag == audiofile::PCM);
    let user = match s.get(&k_user_data()) {
        Some(Val::Blob(b)) if !raw => b.clone(),
        _ => Vec::new(),
    };
    let mut w = Vec::new();
    w.extend((if ext { 0xfffe } else { tag as u16 }).to_le_bytes());
    w.extend((ch as u16).to_le_bytes());
    w.extend(rate.to_le_bytes());
    w.extend(avg.to_le_bytes());
    w.extend((align as u16).to_le_bytes());
    w.extend((bits as u16).to_le_bytes());
    if ext {
        w.extend(22u16.to_le_bytes());
        w.extend((bits as u16).to_le_bytes());
        let mask = u32_of(s, &k_mask()).unwrap_or(match ch {
            1 => 4,
            2 => 3,
            4 => 0x33,
            6 => 0x3f,
            8 => 0x63f,
            _ => 0,
        });
        w.extend(mask.to_le_bytes());
        w.extend(audio_format(tag));
    } else {
        w.extend((user.len() as u16).to_le_bytes());
        w.extend(user);
    }
    Some(w)
}

/// MFCreateWaveFormatExFromMFMediaType(type, WAVEFORMATEX **out, UINT32 *size, flags)
fn MFCreateWaveFormatExFromMFMediaType(c: &mut Ctx) -> u32 {
    let (t, out, size, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if t == 0 || out == 0 {
        return E_POINTER;
    }
    let Some(w) = store_of(t).and_then(|s| wave_format(&s, flags & 1 != 0)) else {
        return MF_E_INVALIDMEDIATYPE;
    };
    let p = mem::heap_alloc(w.len() as u32, false);
    mem::write(p, &w);
    putp(out, p);
    if size != 0 {
        w32(size, w.len() as u32);
    }
    0
}

/// A WAVEFORMATEX as a type's store.
pub fn from_wave_format(w: &[u8]) -> Option<Store> {
    let r16 = |o: usize| w.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let r32 = |o: usize| w.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap()));
    let mut tag = r16(0)? as u32;
    let (ch, rate, avg, align, bits) = (r16(2)? as u32, r32(4)?, r32(8)?, r16(12)? as u32, r16(14)? as u32);
    let extra = r16(16).unwrap_or(0) as usize;
    let mut s = Store::default();
    if tag == 0xfffe {
        tag = r16(24)? as u32;
        if let Some(m) = r32(20) {
            s.set(k_mask(), Val::U32(m));
        }
    }
    s.set(k_major(), Val::Guid(major_audio()));
    s.set(k_subtype(), Val::Guid(audio_format(tag)));
    s.set(k_channels(), Val::U32(ch));
    s.set(k_rate(), Val::U32(rate));
    s.set(k_avg(), Val::U32(avg));
    s.set(k_align(), Val::U32(align));
    if bits != 0 {
        s.set(k_bits(), Val::U32(bits));
    }
    if tag == audiofile::PCM || tag == audiofile::FLOAT {
        s.set(k_independent(), Val::U32(1));
    } else if extra > 0 {
        s.set(k_user_data(), Val::Blob(w.get(18..18 + extra)?.to_vec()));
    }
    Some(s)
}

/// MFInitMediaTypeFromWaveFormatEx(type, const WAVEFORMATEX *, UINT32 size)
fn MFInitMediaTypeFromWaveFormatEx(c: &mut Ctx) -> u32 {
    let (t, w, n) = (c.a(0), c.a(1), c.a(2));
    if t == 0 || w == 0 {
        return E_POINTER;
    }
    let Some(s) = from_wave_format(mem::slice(w, n.max(16))) else { return E_INVALIDARG };
    if attrs(t, |a| a.1 = s).is_none() {
        return E_INVALIDARG;
    }
    0
}

// --- memory buffers -----------------------------------------------------------------------

struct Buf {
    refs: u32,
    data: u32,
    max: u32,
    len: u32,
}
static BUFS: Mutex<Option<HashMap<u32, Buf>>> = Mutex::new(None);
fn buf<R>(b: u32, f: impl FnOnce(&mut Buf) -> R) -> Option<R> {
    BUFS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

fn mb_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFMediaBuffer {045fa593-8799-42b8-bc8d-8968c6453507}
    if is_unknown(&g) || is(&g, 0x045f_a593, 0x8799, 0x42b8) {
        buf(this, |b| b.refs += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn mb_add_ref(c: &mut Ctx) -> u32 {
    buf(c.a(0), |b| {
        b.refs += 1;
        b.refs
    })
    .unwrap_or(1)
}
fn mb_release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = buf(this, |b| {
        b.refs = b.refs.saturating_sub(1);
        b.refs
    })
    .unwrap_or(0);
    if n == 0 {
        if let Some(b) = BUFS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&this) {
            mem::heap_free(b.data);
            mem::heap_free(this);
        }
    }
    n
}
/// Lock(BYTE **, DWORD *max, DWORD *current)
fn mb_lock(c: &mut Ctx) -> u32 {
    let (pp, pmax, plen) = (c.a(1), c.a(2), c.a(3));
    if pp == 0 {
        return E_POINTER;
    }
    let Some((d, m, l)) = buf(c.a(0), |b| (b.data, b.max, b.len)) else { return E_INVALIDARG };
    putp(pp, d);
    if pmax != 0 {
        w32(pmax, m);
    }
    if plen != 0 {
        w32(plen, l);
    }
    0
}
fn mb_get_len(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), buf(c.a(0), |b| b.len).unwrap_or(0));
    0
}
fn mb_set_len(c: &mut Ctx) -> u32 {
    let n = c.a(1);
    buf(c.a(0), |b| {
        if n > b.max {
            E_INVALIDARG
        } else {
            b.len = n;
            0
        }
    })
    .unwrap_or(E_INVALIDARG)
}
fn mb_get_max(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), buf(c.a(0), |b| b.max).unwrap_or(0));
    0
}

/// A memory buffer of `max` bytes (one reference), `bytes` in it.
fn new_buffer(max: u32, bytes: &[u8]) -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    let vt = *VT.get_or_init(|| {
        vtable(
            "IMFMediaBuffer",
            &[
                (mb_qi as Handler, 12),
                (mb_add_ref, 4),
                (mb_release, 4),
                (mb_lock, 16),
                (ok, 4),
                (mb_get_len, 8),
                (mb_set_len, 8),
                (mb_get_max, 8),
            ],
        )
    });
    let b = object(vt, 1);
    let data = mem::heap_alloc(max.max(1), true);
    mem::write(data, &bytes[..bytes.len().min(max as usize)]);
    BUFS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(
        b,
        Buf {
            refs: 1,
            data,
            max,
            len: bytes.len().min(max as usize) as u32,
        },
    );
    b
}

fn MFCreateMemoryBuffer(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), new_buffer(c.a(0), &[]));
    0
}

/// A buffer's bytes (None: not one of ours).
fn buffer_bytes(b: u32) -> Option<Vec<u8>> {
    buf(b, |x| mem::slice(x.data, x.len).to_vec())
}

// --- samples ------------------------------------------------------------------------------

#[derive(Default)]
struct Sample {
    time: Option<u64>,
    dur: Option<u64>,
    flags: u32,
    bufs: Vec<u32>,
}
static SAMPLES: Mutex<Option<HashMap<u32, Sample>>> = Mutex::new(None);
fn sample<R>(b: u32, f: impl FnOnce(&mut Sample) -> R) -> Option<R> {
    SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

fn sm_qi(c: &mut Ctx) -> u32 {
    typed_qi(c, 0xc40a_00f2, 0xb93a, 0x4d80)
}
fn sm_release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = at_release(c);
    if n == 0 {
        let gone = SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&this);
        for b in gone.map(|s| s.bufs).unwrap_or_default() {
            release(c, b);
        }
    }
    n
}
fn sm_get_flags(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), sample(c.a(0), |s| s.flags).unwrap_or(0));
    0
}
fn sm_set_flags(c: &mut Ctx) -> u32 {
    let f = c.a(1);
    sample(c.a(0), |s| s.flags = f);
    0
}
fn sm_get_time(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    match sample(c.a(0), |s| s.time).flatten() {
        Some(t) => {
            w64(c.a(1), t);
            0
        }
        None => 0xc00d_36e7, // MF_E_NO_SAMPLE_TIMESTAMP
    }
}
fn sm_set_time(c: &mut Ctx) -> u32 {
    let t = u64_arg(c, 1, 1);
    sample(c.a(0), |s| s.time = Some(t));
    0
}
fn sm_get_dur(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    match sample(c.a(0), |s| s.dur).flatten() {
        Some(t) => {
            w64(c.a(1), t);
            0
        }
        None => 0xc00d_36e8, // MF_E_NO_SAMPLE_DURATION
    }
}
fn sm_set_dur(c: &mut Ctx) -> u32 {
    let t = u64_arg(c, 1, 1);
    sample(c.a(0), |s| s.dur = Some(t));
    0
}
fn sm_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), sample(c.a(0), |s| s.bufs.len() as u32).unwrap_or(0));
    0
}
/// GetBufferByIndex(DWORD, IMFMediaBuffer **)
fn sm_by_index(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let Some(b) = sample(c.a(0), |s| s.bufs.get(i as usize).copied()).flatten() else {
        return E_INVALIDARG;
    };
    add_ref(c, b);
    putp(out, b);
    0
}
/// ConvertToContiguousBuffer(IMFMediaBuffer **): the one buffer, or a new one of them all
fn sm_contiguous(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let bufs = sample(this, |s| s.bufs.clone()).unwrap_or_default();
    if bufs.is_empty() {
        return E_FAIL;
    }
    if let [b] = bufs[..] {
        add_ref(c, b);
        putp(out, b);
        return 0;
    }
    let mut all = Vec::new();
    for &b in &bufs {
        all.extend(buffer_bytes(b).unwrap_or_default());
    }
    let nb = new_buffer(all.len() as u32, &all);
    for b in bufs {
        release(c, b);
    }
    add_ref(c, nb);
    sample(this, |s| s.bufs = vec![nb]);
    putp(out, nb);
    0
}
fn sm_add(c: &mut Ctx) -> u32 {
    let b = c.a(1);
    if b == 0 {
        return E_POINTER;
    }
    add_ref(c, b);
    sample(c.a(0), |s| s.bufs.push(b));
    0
}
fn sm_remove(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    let Some(b) = sample(c.a(0), |s| (i < s.bufs.len()).then(|| s.bufs.remove(i))).flatten() else {
        return E_INVALIDARG;
    };
    release(c, b);
    0
}
fn sm_remove_all(c: &mut Ctx) -> u32 {
    for b in sample(c.a(0), |s| std::mem::take(&mut s.bufs)).unwrap_or_default() {
        release(c, b);
    }
    0
}
fn sm_total(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let n: u32 = sample(c.a(0), |s| s.bufs.clone()).unwrap_or_default().iter().map(|&b| buf(b, |x| x.len).unwrap_or(0)).sum();
    w32(c.a(1), n);
    0
}
/// CopyToBuffer(IMFMediaBuffer *)
fn sm_copy_to(c: &mut Ctx) -> u32 {
    let to = c.a(1);
    let mut all = Vec::new();
    for b in sample(c.a(0), |s| s.bufs.clone()).unwrap_or_default() {
        all.extend(buffer_bytes(b).unwrap_or_default());
    }
    buf(to, |x| {
        if all.len() as u32 > x.max {
            return MF_E_NOTACCEPTING;
        }
        mem::write(x.data, &all);
        x.len = all.len() as u32;
        0
    })
    .unwrap_or(E_INVALIDARG)
}

/// A sample (one reference): its time, duration and one buffer of `bytes`.
fn new_sample(time: Option<u64>, dur: Option<u64>, bytes: Option<&[u8]>) -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    let vt = *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (sm_qi, 12);
        m[2] = (sm_release, 4);
        m.extend([
            (sm_get_flags as Handler, 8),
            (sm_set_flags, 8),
            (sm_get_time, 8),
            (sm_set_time, 12),
            (sm_get_dur, 8),
            (sm_set_dur, 12),
            (sm_count, 8),
            (sm_by_index, 12),
            (sm_contiguous, 8),
            (sm_add, 8),
            (sm_remove, 8),
            (sm_remove_all, 4),
            (sm_total, 8),
            (sm_copy_to, 8),
        ]);
        vtable("IMFSample", &m)
    });
    let b = object(vt, 1);
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, Store::default()));
    let bufs = bytes.map(|x| vec![new_buffer(x.len() as u32, x)]).unwrap_or_default();
    SAMPLES
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(b, Sample { time, dur, flags: 0, bufs });
    b
}

fn MFCreateSample(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        return E_POINTER;
    }
    putp(c.a(0), new_sample(None, None, None));
    0
}

// --- the reader -----------------------------------------------------------------------------

fn sr_qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // IMFSourceReader {70ae66f2-c809-4e4f-8915-bdcb406b7993}
    if is_unknown(&g) || is(&g, 0x70ae_66f2, 0xc809, 0x4e4f) {
        reader(this, |r| r.refs += 1);
        putp(out, this);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn sr_add_ref(c: &mut Ctx) -> u32 {
    reader(c.a(0), |r| {
        r.refs += 1;
        r.refs
    })
    .unwrap_or(1)
}
fn sr_release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = reader(this, |r| {
        r.refs = r.refs.saturating_sub(1);
        r.refs
    })
    .unwrap_or(0);
    if n == 0 {
        let gone = READERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&this);
        if let Some(r) = gone {
            release(c, r.current);
            mem::heap_free(this);
        }
    }
    n
}
/// Whether `i` names the reader's one stream.
fn ours(i: u32) -> bool {
    i == 0 || i == FIRST_AUDIO
}
/// GetStreamSelection(DWORD, BOOL *)
fn sr_get_selection(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if !ours(i) {
        return MF_E_INVALIDSTREAMNUMBER;
    }
    w32(out, reader(c.a(0), |r| r.selected).unwrap_or(false) as u32);
    0
}
/// SetStreamSelection(DWORD, BOOL)
fn sr_set_selection(c: &mut Ctx) -> u32 {
    let (i, on) = (c.a(1), c.a(2) != 0);
    if !ours(i) && i != ALL_STREAMS {
        return MF_E_INVALIDSTREAMNUMBER;
    }
    reader(c.a(0), |r| r.selected = on);
    0
}
/// GetNativeMediaType(DWORD stream, DWORD index, IMFMediaType **)
fn sr_native(c: &mut Ctx) -> u32 {
    let (i, k, out) = (c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    if !ours(i) {
        return MF_E_INVALIDSTREAMNUMBER;
    }
    if k != 0 {
        return MF_E_NO_MORE_TYPES;
    }
    let Some(s) = reader(c.a(0), |r| r.stream.clone()) else { return E_INVALIDARG };
    let t = native_type(&s.lock().unwrap_or_else(|e| e.into_inner()).track);
    putp(out, new_type(t));
    0
}
/// GetCurrentMediaType(DWORD stream, IMFMediaType **): a copy (Windows hands out its own
/// object; changing the copy changes nothing here)
fn sr_current(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if !ours(i) {
        return MF_E_INVALIDSTREAMNUMBER;
    }
    let Some(cur) = reader(c.a(0), |r| r.current) else { return E_INVALIDARG };
    putp(out, new_type(store_of(cur).unwrap_or_default()));
    0
}
/// SetCurrentMediaType(DWORD stream, DWORD *reserved, IMFMediaType *)
fn sr_set_current(c: &mut Ctx) -> u32 {
    let (this, i, t) = (c.a(0), c.a(1), c.a(3));
    if t == 0 {
        return E_POINTER;
    }
    if !ours(i) {
        return MF_E_INVALIDSTREAMNUMBER;
    }
    let Some(want) = store_of(t) else { return E_INVALIDARG };
    let Some(s) = reader(this, |r| r.stream.clone()) else { return E_INVALIDARG };
    let mut st = s.lock().unwrap_or_else(|e| e.into_inner());
    let Some(out) = out_for(&st.track, &want) else { return MF_E_INVALIDMEDIATYPE };
    // a decoded type needs the page's decoder
    if matches!((out, &st.track.codec), (Out::Pcm { .. }, Codec::Page { .. })) && !kura::adec::available() {
        return MF_E_INVALIDMEDIATYPE;
    }
    st.set_out(out);
    let store = match out {
        Out::Native => native_type(&st.track),
        Out::Pcm { bits, float, rate, channels } => pcm_type_at(rate, channels, bits, float),
    };
    drop(st);
    let nt = new_type(store);
    let old = reader(this, |r| std::mem::replace(&mut r.current, nt)).unwrap_or(0);
    release(c, old);
    0
}
/// SetCurrentPosition(REFGUID format, REFPROPVARIANT position): GUID_NULL and VT_I8 100 ns
fn sr_seek(c: &mut Ctx) -> u32 {
    let (g, p) = (c.a(1), c.a(2));
    if g == 0 || p == 0 {
        return E_POINTER;
    }
    if guid_at(g) != [0; 16] || r16(p) != 20 {
        return E_INVALIDARG;
    }
    let t = mem::r64(p + 8) as i64;
    let Some(s) = reader(c.a(0), |r| r.stream.clone()) else { return E_INVALIDARG };
    s.lock().unwrap_or_else(|e| e.into_inner()).seek(t.max(0) as u64);
    0
}
/// ReadSample(DWORD stream, DWORD flags, DWORD *actual, DWORD *streamflags, LONGLONG *time,
/// IMFSample **)
fn sr_read(c: &mut Ctx) -> u32 {
    let (this, i, actual, sflags, time, out) = (c.a(0), c.a(1), c.a(3), c.a(4), c.a(5), c.a(6));
    if out == 0 || sflags == 0 {
        // (synchronous mode: both are needed)
        return E_POINTER;
    }
    putp(out, 0);
    if !ours(i) && i != ALL_STREAMS {
        return MF_E_INVALIDSTREAMNUMBER;
    }
    let Some((s, on)) = reader(this, |r| (r.stream.clone(), r.selected)) else { return E_INVALIDARG };
    if actual != 0 {
        w32(actual, 0);
    }
    if !on {
        return MF_E_INVALIDREQUEST;
    }
    let mut st = s.lock().unwrap_or_else(|e| e.into_inner());
    let got = st.read();
    let end_time = st.duration();
    drop(st);
    match got {
        Ok(Some(p)) => {
            w32(sflags, 0);
            if time != 0 {
                w64(time, p.time);
            }
            putp(out, new_sample(Some(p.time), Some(p.dur), Some(&p.bytes)));
            0
        }
        Ok(None) => {
            w32(sflags, ENDOFSTREAM);
            if time != 0 {
                w64(time, end_time);
            }
            0
        }
        Err(e) => {
            w32(sflags, 1); // MF_SOURCE_READERF_ERROR
            e
        }
    }
}
/// GetPresentationAttribute(DWORD stream, REFGUID, PROPVARIANT *)
fn sr_presentation(c: &mut Ctx) -> u32 {
    let (i, g, p) = (c.a(1), c.a(2), c.a(3));
    if g == 0 || p == 0 {
        return E_POINTER;
    }
    let k = guid_at(g);
    let Some(s) = reader(c.a(0), |r| r.stream.clone()) else { return E_INVALIDARG };
    mem::fill(p, 8 + 2 * ps(), 0);
    if i == MEDIASOURCE && k == k_duration() {
        w16(p, 21); // VT_UI8
        w64(p + 8, s.lock().unwrap_or_else(|e| e.into_inner()).duration());
        return 0;
    }
    if i == MEDIASOURCE && k == k_characteristics() {
        w16(p, 19); // VT_UI4: MFMEDIASOURCE_CAN_SEEK | CAN_PAUSE
        w32(p + 8, 6);
        return 0;
    }
    if i != MEDIASOURCE && !ours(i) {
        return MF_E_INVALIDSTREAMNUMBER;
    }
    MF_E_ATTRIBUTENOTFOUND
}

fn reader_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        vtable(
            "IMFSourceReader",
            &[
                (sr_qi as Handler, 12),
                (sr_add_ref, 4),
                (sr_release, 4),
                (sr_get_selection, 12),
                (sr_set_selection, 12),
                (sr_native, 16),
                (sr_current, 12),
                (sr_set_current, 16),
                (sr_seek, 12),
                (sr_read, 28),
                (ok, 8), // Flush
                (|_| MF_E_UNSUPPORTED_SERVICE, 20),
                (sr_presentation, 16),
            ],
        )
    })
}

/// A reader of the sound file whose bytes are `data`.
fn new_reader(c: &mut Ctx, attributes: u32, data: Vec<u8>, out: u32) -> u32 {
    if attributes != 0 && store_of(attributes).is_some_and(|s| s.get(&k_async()).is_some()) {
        eprintln!("sake: mf reader: asynchronous mode is not there");
        return E_NOTIMPL;
    }
    let Some(track) = audiofile::open(&data) else { return MF_E_UNSUPPORTED_BYTESTREAM_TYPE };
    let native = native_type(&track);
    let stream = Stream::new(track, Arc::new(data));
    let b = object(reader_vtable(), 1);
    let current = new_type(native);
    READERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(
        b,
        Reader {
            refs: 1,
            stream: Arc::new(Mutex::new(stream)),
            selected: true,
            current,
        },
    );
    let _ = c;
    putp(out, b);
    0
}

/// MFCreateSourceReaderFromURL(LPCWSTR url, IMFAttributes *, IMFSourceReader **)
fn MFCreateSourceReaderFromURL(c: &mut Ctx) -> u32 {
    let (url, at, out) = (c.a(0), c.a(1), c.a(2));
    if url == 0 || out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let w = String::from_utf16_lossy(&mem::wstr(url));
    let path = file_url_path(&w);
    // (HRESULT_FROM_WIN32: ERROR_PATH_NOT_FOUND when the folder is missing too, winref mfreader)
    let missing = || 0x8007_0000 | crate::files::missing(&path);
    let Some(host) = crate::files::resolve(&path) else { return missing() };
    let Ok(data) = kura::fs::read(&host) else { return missing() };
    new_reader(c, at, data, out)
}

/// A file: URL's path ("file:///C:/a%20b.mp3" -> "C:\a b.mp3"); anything else as it is.
pub fn file_url_path(u: &str) -> String {
    let Some(rest) = u.strip_prefix("file:///").or_else(|| u.strip_prefix("file://")) else {
        return u.to_string();
    };
    let mut out = Vec::new();
    let b = rest.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 3 <= b.len() {
            if let Ok(v) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'/' { b'\\' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// MFCreateSourceReaderFromByteStream(IMFByteStream *, IMFAttributes *, IMFSourceReader **):
/// the stream's bytes read from its start (GetLength, SetCurrentPosition, Read)
fn MFCreateSourceReaderFromByteStream(c: &mut Ctx) -> u32 {
    let (bs, at, out) = (c.a(0), c.a(1), c.a(2));
    if bs == 0 || out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let tmp = mem::heap_alloc(16, true);
    let hr = com(c, bs, 4, &[tmp]);
    let len = mem::r64(tmp);
    if hr & 0x8000_0000 != 0 || len > 1 << 30 {
        mem::heap_free(tmp);
        return if hr & 0x8000_0000 != 0 { hr } else { E_INVALIDARG };
    }
    // SetCurrentPosition(QWORD): two words on x86
    if mem::x64() {
        com(c, bs, 7, &[0]);
    } else {
        com(c, bs, 7, &[0, 0]);
    }
    const PIECE: u32 = 1 << 20;
    let b = mem::heap_alloc(PIECE, false);
    let mut data = Vec::with_capacity(len as usize);
    while (data.len() as u64) < len {
        let want = (len - data.len() as u64).min(PIECE as u64) as u32;
        w32(tmp, 0);
        let hr = com(c, bs, 9, &[b, want, tmp]);
        let got = r32(tmp).min(want);
        if hr & 0x8000_0000 != 0 || got == 0 {
            break;
        }
        data.extend_from_slice(mem::slice(b, got));
    }
    mem::heap_free(b);
    mem::heap_free(tmp);
    new_reader(c, at, data, out)
}

pub fn init() {
    implement("mfreadwrite", "MFCreateSourceReaderFromURL", MFCreateSourceReaderFromURL);
    implement("mfreadwrite", "MFCreateSourceReaderFromByteStream", MFCreateSourceReaderFromByteStream);
    implement("mfplat", "MFCreateMediaType", MFCreateMediaType);
    implement("mfplat", "MFCreateWaveFormatExFromMFMediaType", MFCreateWaveFormatExFromMFMediaType);
    implement("mfplat", "MFInitMediaTypeFromWaveFormatEx", MFInitMediaTypeFromWaveFormatEx);
    implement("mfplat", "MFCreateMemoryBuffer", MFCreateMemoryBuffer);
    implement("mfplat", "MFCreateSample", MFCreateSample);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    fn pcm_track(frames: u32, ch: u32) -> (Track, Arc<Vec<u8>>) {
        let data: Vec<u8> = (0..frames * ch).flat_map(|i| ((i as i16).wrapping_mul(7)).to_le_bytes()).collect();
        let mut packets = Vec::new();
        let mut fr = Vec::new();
        let mut at = 0;
        while at < frames {
            let n = (frames - at).min(4096);
            packets.push(((at * ch * 2) as u64, n * ch * 2));
            fr.push(n);
            at += n;
        }
        (
            Track {
                codec: Codec::Pcm { bits: 16, float: false },
                rate: 48000,
                channels: ch,
                packets,
                frames: fr,
                avg: 48000 * ch * 2,
                duration: None,
            },
            Arc::new(data),
        )
    }

    #[test]
    fn pcm_in_order_with_times_and_seek() {
        let (t, d) = pcm_track(10000, 2);
        let mut s = Stream::new(t.clone(), d.clone());
        s.set_out(Out::pcm(&s.track.clone(), 16, false));
        let mut all = Vec::new();
        let mut times = Vec::new();
        while let Some(p) = s.read().unwrap() {
            times.push((p.time, p.dur));
            all.extend(p.bytes);
        }
        assert!(s.ended);
        assert_eq!(all, *d);
        assert_eq!(times, [(0, 853_333), (853_333, 853_333), (1_706_666, 376_667)]);
        assert_eq!(s.duration(), 2_083_333);
        // to 0.1 s: PCM from that frame (rounded up: 1 / 48000 s past 0.1 s when it falls
        // between frames), the packet's rest
        s.seek(1_000_000);
        let p = s.read().unwrap().unwrap();
        assert_eq!((p.time, p.bytes.len()), (1_000_000, (8192 - 4800) * 4));
        assert_eq!(&p.bytes[..], &d[4800 * 4..8192 * 4]);
        s.seek(1_000_001);
        assert_eq!(s.read().unwrap().unwrap().time, 4801 * 10_000_000 / 48000);
        // then on as before
        assert_eq!(s.read().unwrap().unwrap().time, 1_706_666);
        // floats out
        s.seek(0);
        s.set_out(Out::pcm(&s.track.clone(), 32, true));
        let p = s.read().unwrap().unwrap();
        assert_eq!(p.bytes.len(), 4096 * 2 * 4);
        assert_eq!(f32::from_le_bytes(p.bytes[4..8].try_into().unwrap()), 7.0 / 32768.0);
    }

    #[test]
    fn page_decoded_in_samples_of_a_frame() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let page = std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                // a packet [k, ...] decodes to 1152 frames of k / 256
                kura::adec::test_page_pass(&mut |b, ch| vec![b[0] as f32 / 256.0; 1152 * ch as usize]);
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        });
        let data: Vec<u8> = (0..20u8).flat_map(|k| vec![k; 10]).collect();
        let t = Track {
            codec: Codec::Page {
                codec: "mp3".into(),
                desc: vec![],
                subtype: audiofile::MP3,
            },
            rate: 44100,
            channels: 2,
            packets: (0..20).map(|k| (k * 10, 10)).collect(),
            frames: vec![1152; 20],
            avg: 16000,
            duration: None,
        };
        let mut s = Stream::new(t, Arc::new(data));
        s.set_out(Out::pcm(&s.track.clone(), 16, false));
        let mut n = 0;
        while let Some(p) = s.read().unwrap() {
            assert_eq!(p.bytes.len(), 1152 * 4);
            assert_eq!(p.time, (n as u128 * 1152 * 10_000_000 / 44100) as u64);
            assert_eq!(i16::from_le_bytes([p.bytes[0], p.bytes[1]]), (n * 128) as i16);
            n += 1;
        }
        assert_eq!(n, 20);
        // a seek starts the page's decoder again from that packet
        s.seek(10 * 1152 * 10_000_000u64 / 44100 + 1);
        let p = s.read().unwrap().unwrap();
        assert_eq!(i16::from_le_bytes([p.bytes[0], p.bytes[1]]), 10 * 128);
        stop.store(true, Ordering::Relaxed);
        page.join().unwrap();
    }

    #[test]
    fn resampled_and_mixed() {
        // 48 kHz stereo (sample i of the file 7 i) to 24 kHz mono: every other frame's mean;
        // to 96 kHz: the frames between in a line; across pieces as in one
        let (t, d) = pcm_track(10000, 2);
        let mut s = Stream::new(t.clone(), d.clone());
        s.set_out(Out::Pcm {
            bits: 32,
            float: true,
            rate: 24000,
            channels: 1,
        });
        let mut n = 0;
        while let Some(p) = s.read().unwrap() {
            if n == 0 {
                for (j, x) in p.bytes.chunks(4).take(1000).enumerate() {
                    let want = (8 * j + 1) as f32 * 3.5 / 32768.0;
                    assert!((f32::from_le_bytes(x.try_into().unwrap()) - want).abs() < 1e-6, "{j}");
                }
            }
            n += p.bytes.len() / 4;
        }
        assert!((4999..=5000).contains(&n), "{n}");
        let mut r = Resample::new((48000, 1), (96000, 1));
        let v: Vec<f32> = (0..100).map(|i| i as f32).collect();
        let mut got = r.run(&v[..37]);
        got.extend(r.run(&v[37..]));
        // (the last input frame waits for the next piece: the line to it is not known yet)
        let want: Vec<f32> = (0..198).map(|i| i as f32 / 2.0).collect();
        assert_eq!(got, want);
        let mut r = Resample::new((44100, 1), (8000, 1));
        let v: Vec<f32> = (0..44100).map(|i| i as f32).collect();
        let a = r.run(&v);
        assert!((7999..=8000).contains(&a.len()));
        assert!(a.windows(2).all(|w| (w[1] - w[0] - 44100.0 / 8000.0).abs() < 1e-2));
    }

    #[test]
    fn types() {
        let (t, _) = pcm_track(10, 2);
        let n = native_type(&t);
        assert_eq!(u32_of(&n, &k_align()), Some(4));
        let mut want = Store::default();
        want.set(k_major(), Val::Guid(major_audio()));
        want.set(k_subtype(), Val::Guid(audio_format(audiofile::FLOAT)));
        assert_eq!(out_for(&t, &want), Some(Out::pcm(&t, 32, true)));
        want.set(k_subtype(), Val::Guid(audio_format(audiofile::PCM)));
        assert_eq!(out_for(&t, &want), Some(Out::pcm(&t, 16, false)));
        // another rate or channel count: resampled (as Windows' reader), within bounds
        want.set(k_rate(), Val::U32(44100));
        assert_eq!(
            out_for(&t, &want),
            Some(Out::Pcm {
                bits: 16,
                float: false,
                rate: 44100,
                channels: 2
            })
        );
        want.set(k_channels(), Val::U32(1));
        assert_eq!(
            out_for(&t, &want),
            Some(Out::Pcm {
                bits: 16,
                float: false,
                rate: 44100,
                channels: 1
            })
        );
        want.set(k_channels(), Val::U32(6));
        assert_eq!(out_for(&t, &want), None);
        want.delete(&k_channels());
        want.set(k_rate(), Val::U32(10));
        assert_eq!(out_for(&t, &want), None);
        want.delete(&k_rate());
        want.set(k_subtype(), Val::Guid(audio_format(audiofile::AAC)));
        assert_eq!(out_for(&t, &want), None);
        // WAVEFORMATEX both ways
        let w = wave_format(&pcm_type(&t, 16, false), false).unwrap();
        assert_eq!(w, [1, 0, 2, 0, 0x80, 0xbb, 0, 0, 0, 0xee, 2, 0, 4, 0, 16, 0, 0, 0]);
        assert_eq!(from_wave_format(&w).unwrap().matches(&pcm_type(&t, 16, false), 2), Some(true));
        let x = wave_format(&pcm_type(&t, 32, true), true).unwrap();
        assert_eq!((x.len(), u16::from_le_bytes([x[0], x[1]]), x[24]), (40, 0xfffe, 3));
        assert_eq!(equal_flags(&pcm_type(&t, 16, false), &pcm_type(&t, 16, false)), 0xf);
        assert_eq!(equal_flags(&pcm_type(&t, 16, false), &pcm_type(&t, 32, true)), 1 | 8);
        // AAC's native type carries HEAACWAVEINFO's tail and the config
        let aac = Track {
            codec: Codec::Page {
                codec: "mp4a.40.2".into(),
                desc: vec![0x12, 0x10],
                subtype: audiofile::AAC,
            },
            ..t
        };
        // (16 bits, independent: not compressed to IsCompressedFormat, as Windows has it)
        assert_eq!((u32_of(&native_type(&aac), &k_bits()), u32_of(&native_type(&aac), &k_independent())), (Some(16), Some(1)));
        let w = wave_format(&native_type(&aac), false).unwrap();
        assert_eq!(&w[16..18], &14u16.to_le_bytes());
        assert_eq!(&w[w.len() - 2..], &[0x12, 0x10]);
    }

    #[test]
    fn file_urls() {
        assert_eq!(file_url_path("file:///C:/Game/bgm%2001.mp3"), "C:\\Game\\bgm 01.mp3");
        assert_eq!(file_url_path("bgm\\a.m4a"), "bgm\\a.m4a");
        assert_eq!(file_url_path("file:///C:/x%2"), "C:\\x%2");
    }
}
