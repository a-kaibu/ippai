//! msacm32: one built-in driver that decodes IMA ADPCM and MS ADPCM to 16-bit PCM (and passes
//! PCM through). FVP and others check the IMA ADPCM codec is there and decode their voices
//! with acmStreamConvert.
//!
//! MP3 (WAVE_FORMAT_MPEGLAYER3) is decoded by the page's WebCodecs AudioDecoder
//! (crates/kura/src/adec.rs): with no page (node) there is no MP3 codec, as before. Like
//! Windows' l3codecp the stream takes all the bytes it is given (a frame cut short waits for
//! the next call) and hands out what it decoded, the rest on later calls.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::mem::{self, put32, putp, r16, r32, r8, rp, w16, w32};

const DRIVER: u32 = 0x00ac_0001;
const IMA: u16 = 0x11;
const MSADPCM: u16 = 2;
const PCM: u16 = 1;
const MP3: u16 = 0x55;
/// How long a conversion waits for the page's decoder.
const MP3_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
const ACMERR_NOTPOSSIBLE: u32 = 512;
const MMSYSERR_INVALPARAM: u32 = 11;

#[derive(Clone, Copy)]
struct Fmt {
    tag: u16,
    channels: u16,
    rate: u32,
    /// nAvgBytesPerSec
    avg: u32,
    align: u16,
    bits: u16,
    /// samples per block (ADPCM)
    spb: u32,
    /// IMA ADPCM with a samples-per-block field that doesn't fit its block size (imaadp32
    /// refuses those)
    bad: bool,
}

fn read_fmt(p: u32) -> Fmt {
    let (tag, channels, rate, align, bits) = (r16(p), r16(p + 2), r32(p + 4), r16(p + 12), r16(p + 14));
    let ch = channels.max(1) as u32;
    let ima_spb = (align as u32).saturating_sub(4 * ch) * 8 / (4 * ch) + 1;
    let bad = tag == IMA && r16(p + 16) >= 2 && r16(p + 18) as u32 != ima_spb;
    let spb = match tag {
        IMA => ima_spb,
        MSADPCM if r16(p + 16) >= 2 => r16(p + 18) as u32,
        MSADPCM => ((align as u32).saturating_sub(7 * ch)) * 8 / (4 * ch) + 2,
        _ => 1,
    };
    Fmt { tag, channels, rate, avg: r32(p + 8), align, bits, spb, bad }
}

struct Stream {
    src: Fmt,
    dst: Fmt,
    mp3: Option<std::sync::Arc<Mutex<Mp3>>>,
}

/// An MP3 stream: the page's decoder, frames not whole yet, samples not handed out yet.
struct Mp3 {
    dec: u32,
    split: crate::mp3frame::Splitter,
    pending: std::collections::VecDeque<i16>,
}

impl Drop for Mp3 {
    fn drop(&mut self) {
        kura::adec::close(self.dec);
    }
}

/// The page decodes MP3.
fn mp3_ok() -> bool {
    kura::adec::available()
}

static STREAMS: Mutex<Option<HashMap<u32, Stream>>> = Mutex::new(None);
static NEXT: Mutex<u32> = Mutex::new(0x00ac_1000);

fn pcm_ok(f: &Fmt) -> bool {
    f.tag == PCM && matches!(f.bits, 8 | 16) && matches!(f.channels, 1 | 2) && f.rate != 0 && f.align == f.channels * f.bits / 8
}
fn supported(s: &Fmt, d: &Fmt) -> bool {
    d.tag == PCM && d.bits == 16 && d.channels == s.channels && d.rate == s.rate && matches!(s.tag, IMA | MSADPCM) && !s.bad
        || pcm_ok(s) && pcm_ok(d)
        || mp3_pair(s, d)
}
fn mp3_pair(s: &Fmt, d: &Fmt) -> bool {
    s.tag == MP3 && matches!(s.channels, 1 | 2) && s.rate != 0 && d.tag == PCM && d.bits == 16 && d.channels == s.channels && d.rate == s.rate && mp3_ok()
}

/// Whether every block header is one the codec takes: Windows' imaadp32/msadp32 convert
/// nothing (and say so with no error) when any block has a step index past 88 or a
/// predictor past the coefficient table.
fn headers_ok(s: &Fmt, src: u32, blocks: u32) -> bool {
    let ch = s.channels.max(1) as u32;
    (0..blocks).all(|k| {
        let b = src + k * s.align as u32;
        (0..ch).all(|c| if s.tag == IMA { r8(b + 4 * c + 2) <= 88 } else { r8(b + c) < 7 })
    })
}

/// PCM to PCM as msacm32's converter does it: 8-bit samples are (b - 128) << 8 and back,
/// stereo to mono adds the channels (clamped), mono to stereo copies; another rate repeats
/// or skips frames, output frame i taking source frame (i * src + c) / dst, with c fitted
/// to Windows' answers (half a source frame, a quarter of a destination one when that is
/// more), while that frame exists.
fn pcm_convert(s: &Fmt, d: &Fmt, src: &[u8]) -> Vec<u8> {
    let sa = s.align as usize;
    let frame = |i: usize| -> [i32; 2] {
        let get = |k: usize| -> i32 {
            let o = i * sa + k * (s.bits as usize / 8);
            if s.bits == 8 { (src[o] as i32 - 128) << 8 } else { i16::from_le_bytes([src[o], src[o + 1]]) as i32 }
        };
        let l = get(0);
        let r = if s.channels == 2 { get(1) } else { l };
        match (s.channels, d.channels) {
            (2, 1) => [(l + r).clamp(-32768, 32767), 0],
            _ => [l, r],
        }
    };
    let n = src.len() / sa.max(1);
    let (sr, dr) = (s.rate as u64, d.rate as u64);
    // fitted to Windows' output (winref acm): upsampling steps an error term that starts at
    // dr/2 and moves to the next frame once it exceeds dr; downsampling rounds from sr/2
    let c = if sr < dr { dr / 2 - 1 } else { (sr / 2).max(dr / 4) };
    let mut out = Vec::new();
    for i in 0u64.. {
        let k = ((i * sr + c) / dr) as usize;
        if k >= n {
            break;
        }
        let f = frame(k);
        for v in &f[..d.channels as usize] {
            if d.bits == 8 {
                out.push(((v >> 8) + 128) as u8);
            } else {
                out.extend_from_slice(&(*v as i16).to_le_bytes());
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// decoders: whole blocks of `src` to 16-bit samples

const IMA_INDEX: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
const IMA_STEP: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107, 118, 130, 143,
    157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411,
    1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493, 10442,
    11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

pub fn ima_block(b: &[u8], ch: usize, spb: usize, out: &mut Vec<i16>) {
    // (a block shorter than its headers: nothing, as ms_block; found by fuzz/acm)
    if b.len() < 4 * ch || spb == 0 {
        return;
    }
    let start = out.len();
    out.resize(start + spb * ch, 0);
    for c in 0..ch {
        let h = &b[4 * c..4 * c + 4];
        let mut pred = i16::from_le_bytes([h[0], h[1]]) as i32;
        let mut idx = (h[2] as i32).clamp(0, 88);
        out[start + c] = pred as i16;
        // data: 4 bytes (8 samples) per channel in turn
        for i in 1..spb {
            let k = i - 1;
            let at = 4 * ch + (k / 8) * 4 * ch + 4 * c + (k % 8) / 2;
            let Some(&byte) = b.get(at) else { break };
            let nib = if k % 2 == 0 { byte & 15 } else { byte >> 4 } as i32;
            let step = IMA_STEP[idx as usize];
            let mut diff = step >> 3;
            if nib & 4 != 0 {
                diff += step;
            }
            if nib & 2 != 0 {
                diff += step >> 1;
            }
            if nib & 1 != 0 {
                diff += step >> 2;
            }
            pred = if nib & 8 != 0 { pred - diff } else { pred + diff }.clamp(-32768, 32767);
            idx = (idx + IMA_INDEX[nib as usize]).clamp(0, 88);
            out[start + i * ch + c] = pred as i16;
        }
    }
}

const MS_ADAPT: [i32; 16] = [230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230];
const MS_COEF: [(i32, i32); 7] = [(256, 0), (512, -256), (0, 0), (192, 64), (240, 0), (460, -208), (392, -232)];

pub fn ms_block(b: &[u8], ch: usize, spb: usize, out: &mut Vec<i16>) {
    if b.len() < 7 * ch || spb < 2 {
        return;
    }
    let s16 = |o: usize| i16::from_le_bytes([b[o], b[o + 1]]) as i32;
    let mut coef = vec![(0, 0); ch];
    let mut delta = vec![0; ch];
    let mut s1 = vec![0; ch];
    let mut s2 = vec![0; ch];
    for c in 0..ch {
        coef[c] = MS_COEF[(b[c] as usize).min(6)];
        delta[c] = s16(ch + 2 * c);
        s1[c] = s16(3 * ch + 2 * c);
        s2[c] = s16(5 * ch + 2 * c);
    }
    let start = out.len();
    out.resize(start + spb * ch, 0);
    for c in 0..ch {
        out[start + c] = s2[c] as i16;
        out[start + ch + c] = s1[c] as i16;
    }
    let mut n = 0usize;
    for i in 2..spb {
        for c in 0..ch {
            let Some(&byte) = b.get(7 * ch + n / 2) else { return };
            let nib = if n % 2 == 0 { byte >> 4 } else { byte & 15 } as i32;
            n += 1;
            let signed = if nib >= 8 { nib - 16 } else { nib };
            let p = ((s1[c] * coef[c].0 + s2[c] * coef[c].1) >> 8) + signed * delta[c];
            let p = p.clamp(-32768, 32767);
            s2[c] = s1[c];
            s1[c] = p;
            // the spec's delta has no upper bound: a run of big nibbles grows it 3x a sample until
            // the multiply overflows, so cap it where it still fits (as FFmpeg does)
            delta[c] = (MS_ADAPT[nib as usize] * delta[c] >> 8).clamp(16, i32::MAX / 768);
            out[start + i * ch + c] = p as i16;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the API

fn acmGetVersion(_: &mut Ctx) -> u32 {
    0x0500_0000
}
fn acmDriverEnum(c: &mut Ctx) -> u32 {
    let (cb, inst) = (c.a(0), c.a(1));
    if cb != 0 {
        // ACMDRIVERDETAILS_SUPPORTF_CODEC
        c.call(cb, &[DRIVER, inst, 1]);
    }
    0
}
fn acmDriverID(c: &mut Ctx) -> u32 {
    put32(c.a(1), DRIVER);
    0
}
fn acmDriverOpen(c: &mut Ctx) -> u32 {
    put32(c.a(0), DRIVER);
    0
}
fn ok0(_: &mut Ctx) -> u32 {
    0
}
fn driver_details(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return MMSYSERR_INVALPARAM;
    }
    let size = r32(p);
    if !(4..=0x10000).contains(&size) {
        return MMSYSERR_INVALPARAM;
    }
    let mut b = vec![0u8; size as usize];
    b[0..4].copy_from_slice(&size.to_le_bytes());
    let put = |b: &mut Vec<u8>, at: usize, v: u32| {
        if at + 4 <= b.len() {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
    };
    put(&mut b, 4, u32::from_le_bytes(*b"audc"));
    put(&mut b, 8, 0);
    put(&mut b, 12, 1 | 17 << 16); // wMid, wPid
    put(&mut b, 16, 0x0300_0000);
    put(&mut b, 20, 0x0400_0000);
    put(&mut b, 24, 1); // codec
    put(&mut b, 28, tags().len() as u32); // format tags
    let strs: [(&str, usize); 4] = [("Microsoft IMA ADPCM", 32), ("Microsoft IMA ADPCM CODEC", 128), ("Copyright (C) Microsoft Corp.", 80), ("", 128)];
    let mut at = 40;
    for (s, n) in strs {
        let units: Vec<u16> = if wide { s.encode_utf16().collect() } else { s.bytes().map(|x| x as u16).collect() };
        for (i, u) in units.iter().take(n - 1).enumerate() {
            if wide {
                if at + 2 * i + 2 <= b.len() {
                    b[at + 2 * i..at + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
                }
            } else if at + i < b.len() {
                b[at + i] = *u as u8;
            }
        }
        at += if wide { 2 * n } else { n };
    }
    mem::write(p, &b);
    0
}
fn acmDriverDetailsA(c: &mut Ctx) -> u32 {
    driver_details(c, false)
}
fn acmDriverDetailsW(c: &mut Ctx) -> u32 {
    driver_details(c, true)
}
const TAGS: [(u16, u32, &str); 4] = [(PCM, 16, "PCM"), (IMA, 20, "IMA ADPCM"), (MSADPCM, 50, "Microsoft ADPCM"), (MP3, 30, "MPEG Layer-3")];
/// The format tags there are (MP3 when the page decodes it)
fn tags() -> &'static [(u16, u32, &'static str)] {
    if mp3_ok() { &TAGS } else { &TAGS[..3] }
}
fn format_tag_details(c: &mut Ctx, wide: bool) -> u32 {
    let (p, flags) = (c.a(1), c.a(2));
    let tags = tags();
    let t = match flags & 0xf {
        0 => tags.get(r32(p + 4) as usize),
        1 => tags.iter().find(|t| t.0 as u32 == r32(p + 8)),
        _ => tags.iter().max_by_key(|t| t.1),
    };
    let Some(&(tag, size, name)) = t else { return ACMERR_NOTPOSSIBLE };
    let i = tags.iter().position(|x| x.0 == tag).unwrap_or(0) as u32;
    w32(p + 4, i);
    w32(p + 8, tag as u32);
    w32(p + 12, size);
    w32(p + 16, 1);
    w32(p + 20, 1);
    let at = p + 24;
    if wide {
        for (k, u) in name.encode_utf16().chain([0]).enumerate() {
            w16(at + 2 * k as u32, u);
        }
    } else {
        mem::write(at, name.as_bytes());
        mem::w8(at + name.len() as u32, 0);
    }
    0
}
fn acmFormatTagDetailsA(c: &mut Ctx) -> u32 {
    format_tag_details(c, false)
}
fn acmFormatTagDetailsW(c: &mut Ctx) -> u32 {
    format_tag_details(c, true)
}
fn acmMetrics(c: &mut Ctx) -> u32 {
    let (metric, out) = (c.a(1), c.a(2));
    let v = match metric {
        1 | 2 | 3 => 1,       // counts of drivers / codecs
        50 => 50,             // ACM_METRIC_MAX_SIZE_FORMAT
        51 => 0,              // ..._FILTER
        _ => 0,
    };
    put32(out, v);
    0
}
/// acmFormatSuggest(had, src, dst, cbDst, flags): 16-bit PCM for ADPCM
fn acmFormatSuggest(c: &mut Ctx) -> u32 {
    let (src, dst, n) = (c.a(1), c.a(2), c.a(3));
    let s = read_fmt(src);
    if n < 16 {
        return MMSYSERR_INVALPARAM;
    }
    if !matches!(s.tag, IMA | MSADPCM | PCM) && !(s.tag == MP3 && mp3_ok()) || s.bad {
        return ACMERR_NOTPOSSIBLE;
    }
    let ch = s.channels.max(1) as u32;
    w16(dst, PCM);
    w16(dst + 2, ch as u16);
    w32(dst + 4, s.rate);
    w32(dst + 8, s.rate * ch * 2);
    w16(dst + 12, (ch * 2) as u16);
    w16(dst + 14, 16);
    if n >= 18 {
        w16(dst + 16, 0);
    }
    0
}
/// acmStreamOpen(phas, had, src, dst, filter, cb, inst, flags)
fn acmStreamOpen(c: &mut Ctx) -> u32 {
    let (ph, src, dst, flags) = (c.a(0), c.a(2), c.a(3), c.a(7));
    let (s, d) = (read_fmt(src), read_fmt(dst));
    if !supported(&s, &d) {
        // (once a pair: which codecs games ask for, MP3 = 0x55, shows in corpus logs)
        static SEEN: Mutex<Vec<(u16, u16)>> = Mutex::new(Vec::new());
        let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if !seen.contains(&(s.tag, d.tag)) {
            seen.push((s.tag, d.tag));
            eprintln!("sake: acm: no conversion from format {:#x} ({} Hz, {} ch) to {:#x} ({} bit)", s.tag, s.rate, s.channels, d.tag, d.bits);
        }
        if ph != 0 {
            putp(ph, 0);
        }
        return ACMERR_NOTPOSSIBLE;
    }
    // ACM_STREAMOPENF_QUERY
    if flags & 1 != 0 {
        return 0;
    }
    let h = {
        let mut n = NEXT.lock().unwrap();
        *n += 1;
        *n
    };
    let mp3 = if s.tag == MP3 {
        let Some(dec) = kura::adec::open("mp3", s.rate, s.channels as u32, &[]) else {
            putp(ph, 0);
            return ACMERR_NOTPOSSIBLE;
        };
        Some(std::sync::Arc::new(Mutex::new(Mp3 { dec, split: Default::default(), pending: Default::default() })))
    } else {
        None
    };
    STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, Stream { src: s, dst: d, mp3 });
    putp(ph, h);
    0
}
fn with<R>(h: u32, f: impl FnOnce(&Stream) -> R) -> Option<R> {
    STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).map(f)
}
fn acmStreamClose(c: &mut Ctx) -> u32 {
    STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0));
    0
}
/// acmStreamSize(has, cbInput, pdwOutputBytes, flags): SOURCE (0) or DESTINATION (1)
fn acmStreamSize(c: &mut Ctx) -> u32 {
    let (h, n, out, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((s, d)) = with(h, |st| (st.src, st.dst)) else { return 5 };
    let ch = s.channels.max(1) as u32;
    let v = if s.tag == MP3 {
        mp3_size(&s, &d, n, flags & 0xf != 0)
    } else if s.tag == PCM {
        // frames at the other rate, rounded up
        let (from, to) = if flags & 0xf == 0 { (s, d) } else { (d, s) };
        let frames = (n / from.align.max(1) as u32) as u64;
        ((frames * to.rate as u64).div_ceil(from.rate.max(1) as u64) * to.align as u64) as u32
    } else if flags & 0xf == 0 {
        // the output of n source bytes: a partial block counts once there is a whole one
        let align = s.align.max(1) as u32;
        let blocks = if n < align { 0 } else { n.div_ceil(align) };
        blocks * s.spb * ch * 2
    } else {
        // the source for n output bytes
        let per = (s.spb * ch * 2).max(1);
        (n / per) * s.align as u32
    };
    put32(out, v);
    if v == 0 { ACMERR_NOTPOSSIBLE } else { 0 }
}
/// MP3's sizes as l3codecp gives them (Wine's mpeg3.c works them out the same way): whole
/// 1152-sample frames at the source's average byte rate, and 3000 bytes more of output.
fn mp3_size(s: &Fmt, d: &Fmt, n: u32, from_dst: bool) -> u32 {
    let src_frame = ((s.avg as u64 * 1152 / s.rate.max(1) as u64) as u32).max(1);
    let dst_frame = d.align as u32 * 1152;
    if from_dst {
        n.saturating_sub(3000) / dst_frame.max(1) * src_frame
    } else {
        let frames = n / src_frame;
        if frames == 0 { 0 } else { 3000 + frames * dst_frame }
    }
}
fn acmStreamPrepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    w32(h + 4, r32(h + 4) | 0x0002_0000);
    0
}
fn acmStreamUnprepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    w32(h + 4, r32(h + 4) & !0x0002_0000);
    0
}
/// acmStreamConvert(has, ACMSTREAMHEADER*, flags)
fn acmStreamConvert(c: &mut Ctx) -> u32 {
    let (h, hdr) = (c.a(0), c.a(1));
    let (h, hdr, flags) = (h, hdr, c.a(2));
    let Some((s, d, mp3)) = with(h, |st| (st.src, st.dst, st.mp3.clone())) else { return 5 };
    // ACMSTREAMHEADER: cbStruct, fdwStatus, dwUser, pbSrc, cbSrcLength, cbSrcLengthUsed,
    // dwSrcUser, pbDst, cbDstLength, cbDstLengthUsed (the pointer-sized ones 8 bytes on x64)
    let p = mem::ps();
    let (o_src, o_dst) = (8 + p, 8 + 3 * p + 8);
    let (src, slen, dst, dlen) = (rp(hdr + o_src), r32(hdr + o_src + p), rp(hdr + o_dst), r32(hdr + o_dst + p));
    let (o_sused, o_dused) = (o_src + p + 4, o_dst + p + 4);
    let (used_in, used_out) = if let Some(m) = mp3 {
        let input: Vec<u8> = (0..slen).map(|i| r8(src + i)).collect();
        let out = mp3_convert(&mut m.lock().unwrap_or_else(|e| e.into_inner()), &input, (dlen / d.align.max(1) as u32 * d.align as u32) as usize / 2, flags & 0x10 != 0);
        let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        mem::write(dst, &bytes);
        (slen, bytes.len() as u32)
    } else if s.tag == PCM {
        let sa = s.align.max(1) as u32;
        let input: Vec<u8> = (0..slen / sa * sa).map(|i| r8(src + i)).collect();
        let mut out = pcm_convert(&s, &d, &input);
        out.truncate((dlen / d.align.max(1) as u32 * d.align as u32) as usize);
        mem::write(dst, &out);
        (input.len() as u32, out.len() as u32)
    } else {
        let ch = s.channels.max(1) as usize;
        let align = s.align.max(1) as u32;
        let per_block = s.spb * ch as u32 * 2;
        let blocks = (slen / align).min(dlen / per_block.max(1));
        if blocks == 0 {
            // (less than a block, without ACM_STREAMCONVERTF_BLOCKALIGN too)
            w32(hdr + o_sused, 0);
            w32(hdr + o_dused, 0);
            return MMSYSERR_INVALPARAM;
        }
        if !headers_ok(&s, src, blocks) {
            w32(hdr + o_sused, blocks * align);
            w32(hdr + o_dused, 0);
            w32(hdr + 4, r32(hdr + 4) | 0x0001_0000);
            return 0;
        }
        let mut out = Vec::with_capacity((blocks * per_block / 2) as usize);
        for k in 0..blocks {
            let b: Vec<u8> = (0..align).map(|i| r8(src + k * align + i)).collect();
            if s.tag == IMA {
                ima_block(&b, ch, s.spb as usize, &mut out);
            } else {
                ms_block(&b, ch, s.spb as usize, &mut out);
            }
        }
        let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        mem::write(dst, &bytes);
        (blocks * align, bytes.len() as u32)
    };
    w32(hdr + o_sused, used_in);
    w32(hdr + o_dused, used_out);
    // ACMSTREAMHEADER_STATUSF_DONE
    w32(hdr + 4, r32(hdr + 4) | 0x0001_0000);
    0
}

/// Takes MP3 bytes (`start`: ACM_STREAMCONVERTF_START, a new stream); up to `max` samples out.
fn mp3_convert(m: &mut Mp3, input: &[u8], max: usize, start: bool) -> Vec<i16> {
    if start {
        m.split.reset();
        m.pending.clear();
        kura::adec::reset(m.dec);
    }
    let mut frames = Vec::new();
    m.split.push(input, &mut frames);
    let mut pcm = Vec::new();
    if kura::adec::decode(m.dec, frames.iter().map(|f| &f.1[..]), MP3_WAIT, &mut pcm).is_none() && !frames.is_empty() {
        eprintln!("sake: acm: mp3: the page's decoder did not answer");
    }
    // (as FFmpeg's float to 16-bit: rounded, clipped)
    m.pending.extend(pcm.iter().map(|&x| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16));
    let n = max.min(m.pending.len());
    m.pending.drain(..n).collect()
}

pub fn init() {
    for (n, f) in [
        ("acmGetVersion", acmGetVersion as crate::api::Handler),
        ("acmDriverEnum", acmDriverEnum),
        ("acmDriverID", acmDriverID),
        ("acmDriverOpen", acmDriverOpen),
        ("acmDriverClose", ok0),
        ("acmDriverDetailsA", acmDriverDetailsA),
        ("acmDriverDetailsW", acmDriverDetailsW),
        ("acmFormatTagDetailsA", acmFormatTagDetailsA),
        ("acmFormatTagDetailsW", acmFormatTagDetailsW),
        ("acmMetrics", acmMetrics),
        ("acmFormatSuggest", acmFormatSuggest),
        ("acmStreamOpen", acmStreamOpen),
        ("acmStreamClose", acmStreamClose),
        ("acmStreamSize", acmStreamSize),
        ("acmStreamPrepareHeader", acmStreamPrepareHeader),
        ("acmStreamUnprepareHeader", acmStreamUnprepareHeader),
        ("acmStreamConvert", acmStreamConvert),
        ("acmStreamReset", ok0),
    ] {
        implement("msacm32", n, f);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::collection::vec;
    use proptest::prelude::*;

    /// The IMA ADPCM encoder of the format's reference (each nibble the closest step, the
    /// predictor and index updated exactly as a decoder does), into one block of `ch` channels
    /// laid out as the decoder reads it; also the samples a decoder must give back.
    fn ima_encode(pcm: &[Vec<i16>]) -> (Vec<u8>, Vec<i16>) {
        let (ch, spb) = (pcm.len(), pcm[0].len());
        let mut b = vec![0u8; 4 * ch + (spb - 1).div_ceil(8) * 4 * ch];
        let mut want = vec![0i16; spb * ch];
        for (c, s) in pcm.iter().enumerate() {
            let (mut pred, mut idx) = (s[0] as i32, 0i32);
            b[4 * c..4 * c + 4].copy_from_slice(&[s[0] as u8, (s[0] >> 8) as u8, 0, 0]);
            want[c] = s[0];
            for i in 1..spb {
                let step = IMA_STEP[idx as usize];
                let mut diff = s[i] as i32 - pred;
                let mut nib = 0;
                if diff < 0 {
                    nib = 8;
                    diff = -diff;
                }
                let mut d = step >> 3;
                for (bit, part) in [(4, step), (2, step >> 1), (1, step >> 2)] {
                    if diff >= part {
                        nib |= bit;
                        diff -= part;
                        d += part;
                    }
                }
                pred = if nib & 8 != 0 { pred - d } else { pred + d }.clamp(-32768, 32767);
                idx = (idx + IMA_INDEX[nib as usize]).clamp(0, 88);
                want[i * ch + c] = pred as i16;
                let k = i - 1;
                let at = 4 * ch + (k / 8) * 4 * ch + 4 * c + (k % 8) / 2;
                b[at] |= if k % 2 == 0 { nib as u8 } else { (nib as u8) << 4 };
            }
        }
        (b, want)
    }

    /// MS ADPCM worked by hand from the format's definition: predictor 1 (512, -256), delta 16,
    /// sample1 100, sample2 50; nibbles 1, 7, -8, 4. Each sample is
    /// (s1 * c1 + s2 * c2) >> 8 + nibble * delta, then delta = adapt[nibble] * delta >> 8, min 16:
    /// 166 (delta 230*16>>8 -> 16), 344 (614*16>>8 = 38), 218 (768*38>>8 = 114), 548.
    #[test]
    fn ms_by_hand() {
        let b = [1, 16, 0, 100, 0, 50, 0, 0x17, 0x84];
        let mut out = Vec::new();
        ms_block(&b, 1, 6, &mut out);
        assert_eq!(out, [50, 100, 166, 344, 218, 548]);
        // stereo: the nibbles alternate channels, high nibble first
        let b = [1, 0, 16, 0, 16, 0, 100, 0, 0, 0, 50, 0, 0, 0, 0x10, 0x70];
        out.clear();
        ms_block(&b, 2, 4, &mut out);
        assert_eq!(out, [50, 0, 100, 0, 166, 0, 344, 0]);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// A block in the encoder's layout decodes to exactly what the encoder tracked.
        #[test]
        fn ima_matches_the_reference_encoder(ch in 1usize..=2, pcm in vec(vec(any::<i16>(), 1..80), 2)) {
            let spb = pcm[0].len().min(pcm[1].len());
            let pcm: Vec<Vec<i16>> = pcm[..ch].iter().map(|s| s[..spb].to_vec()).collect();
            let (block, want) = ima_encode(&pcm);
            let mut out = Vec::new();
            ima_block(&block, ch, spb, &mut out);
            prop_assert_eq!(out, want);
        }

        /// Any bytes, channel count and block length the header claims: no panic, and either
        /// nothing or the whole block of samples.
        #[test]
        fn blocks_any_bytes(b in vec(any::<u8>(), 0..200), ch in 1usize..9, spb in 0usize..600, ms in any::<bool>()) {
            let mut out = vec![7i16];
            if ms { ms_block(&b, ch, spb, &mut out) } else { ima_block(&b, ch, spb, &mut out) }
            prop_assert!(out.len() == 1 || out.len() == 1 + spb * ch);
            prop_assert_eq!(out[0], 7);
        }

        /// MS ADPCM: a block the header says has the largest delta, then nibbles that grow it
        /// every sample, saturates instead of overflowing.
        #[test]
        fn ms_delta_saturates(ch in 1usize..=2, nib in 8u8..12, spb in 2usize..200) {
            let mut b = vec![0u8; ch];
            b.extend((0..ch).flat_map(|_| i16::MAX.to_le_bytes()));
            b.extend(vec![0u8; 4 * ch]);
            b.extend(vec![nib * 17; spb * ch / 2 + 1]);
            let mut out = Vec::new();
            ms_block(&b, ch, spb, &mut out);
            prop_assert_eq!(out.len(), spb * ch);
        }

        /// MS ADPCM: the two header samples come out first, in order, per channel.
        #[test]
        fn ms_header_samples(ch in 1usize..=2, head in vec(any::<i16>(), 4), rest in vec(any::<u8>(), 0..64), spb in 2usize..100) {
            let mut b = vec![0u8; ch];
            b.extend((0..ch).flat_map(|_| 16i16.to_le_bytes()));
            b.extend((0..ch).flat_map(|c| head[c].to_le_bytes()));
            b.extend((0..ch).flat_map(|c| head[2 + c].to_le_bytes()));
            b.extend(&rest);
            let mut out = Vec::new();
            ms_block(&b, ch, spb, &mut out);
            prop_assert_eq!(out.len(), spb * ch);
            for c in 0..ch {
                prop_assert_eq!(out[c], head[2 + c]);
                prop_assert_eq!(out[ch + c], head[c]);
            }
        }
    }
}

#[cfg(test)]
mod mp3_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    /// MPEG-1 layer III 44.1 kHz 128 kbps frames (417 bytes, no padding), stereo or mono, the
    /// body's first byte `k`.
    fn frame(k: u8, mono: bool) -> Vec<u8> {
        let mut f = vec![0xff, 0xfb, 0x90, if mono { 0xc0 } else { 0 }];
        f.resize(417, 0);
        f[4] = k;
        f
    }

    /// A page that "decodes" a frame with body byte k into 1152 frames of value k / 256 (+ c /
    /// 1024 on channel c), in a thread until `stop`.
    fn page(stop: Arc<AtomicBool>) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                kura::adec::test_page_pass(&mut |b, ch| (0..1152).flat_map(|_| (0..ch).map(move |c| b[4] as f32 / 256.0 + c as f32 / 1024.0)).collect());
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        })
    }

    fn sample(k: u8, c: usize) -> i16 {
        ((k as f32 / 256.0 + c as f32 / 1024.0) * 32768.0).round() as i16
    }

    fn stream(ch: u32) -> Mp3 {
        let dec = kura::adec::open("mp3", 44100, ch, &[]).expect("a page");
        Mp3 { dec, split: Default::default(), pending: Default::default() }
    }

    #[test]
    fn frames_across_calls_and_small_buffers() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        // (a page has listed: kura::adec::available)
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let stop = Arc::new(AtomicBool::new(false));
        let p = page(stop.clone());
        let mut m = stream(2);
        let bytes: Vec<u8> = (1..=4).flat_map(|k| frame(k, false)).collect();
        // a frame and a half: one frame out, the half waits
        let out = mp3_convert(&mut m, &bytes[..417 + 200], usize::MAX, false);
        assert_eq!(out.len(), 1152 * 2);
        assert!(out.chunks(2).all(|f| f == [sample(1, 0), sample(1, 1)]));
        // the rest, into a buffer of 1000 samples: the others come on later calls, in order
        let mut got = mp3_convert(&mut m, &bytes[417 + 200..], 1000, false);
        assert_eq!(got.len(), 1000);
        loop {
            let more = mp3_convert(&mut m, &[], 1000, false);
            if more.is_empty() {
                break;
            }
            got.extend(more);
        }
        assert_eq!(got.len(), 3 * 1152 * 2);
        let want: Vec<i16> = (2..=4).flat_map(|k| (0..1152).flat_map(move |_| [sample(k, 0), sample(k, 1)])).collect();
        assert_eq!(got, want);
        // START: what was waiting goes; the stream begins again
        mp3_convert(&mut m, &bytes[..417 + 9], 10, false);
        let out = mp3_convert(&mut m, &frame(9, false), usize::MAX, true);
        assert_eq!(out.len(), 1152 * 2);
        assert_eq!(&out[..2], [sample(9, 0), sample(9, 1)]);
        stop.store(true, Ordering::Relaxed);
        p.join().unwrap();
    }

    #[test]
    fn no_answer_gives_what_there_is() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let mut m = stream(1);
        // no page running: the wait ends, nothing out, nothing lost later
        let t = std::time::Instant::now();
        assert!(mp3_convert(&mut m, &frame(3, true), usize::MAX, false).is_empty());
        assert!(t.elapsed() >= MP3_WAIT);
        kura::adec::test_page_pass(&mut |b, ch| vec![b[4] as f32 / 256.0; 1152 * ch as usize]);
        let out = mp3_convert(&mut m, &[], usize::MAX, false);
        assert_eq!(out.len(), 1152);
        assert_eq!(out[0], sample(3, 0));
    }

    #[test]
    fn sizes_as_l3codecp() {
        // 128 kbps 44.1 kHz stereo: 16000 bytes/s, 417 per frame; 16-bit stereo out: 4608 per frame
        let s = Fmt { tag: MP3, channels: 2, rate: 44100, avg: 16000, align: 1, bits: 0, spb: 1, bad: false };
        let d = Fmt { tag: PCM, channels: 2, rate: 44100, avg: 176400, align: 4, bits: 16, spb: 1, bad: false };
        assert_eq!(mp3_size(&s, &d, 417, false), 3000 + 4608);
        assert_eq!(mp3_size(&s, &d, 4170, false), 3000 + 10 * 4608);
        assert_eq!(mp3_size(&s, &d, 416, false), 0);
        assert_eq!(mp3_size(&s, &d, 3000 + 2 * 4608, true), 2 * 417);
        assert_eq!(mp3_size(&s, &d, 100, true), 0);
    }
}
