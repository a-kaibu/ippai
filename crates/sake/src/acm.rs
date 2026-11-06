//! msacm32: one built-in driver that decodes IMA ADPCM and MS ADPCM to 16-bit PCM (and passes
//! PCM through). FVP and others check the IMA ADPCM codec is there and decode their voices
//! with acmStreamConvert.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::Mutex;

use crate::api::{implement, Ctx};
use crate::mem::{self, put32, r16, r32, r8, w16, w32};

const DRIVER: u32 = 0x00ac_0001;
const IMA: u16 = 0x11;
const MSADPCM: u16 = 2;
const PCM: u16 = 1;
const ACMERR_NOTPOSSIBLE: u32 = 512;
const MMSYSERR_INVALPARAM: u32 = 11;

#[derive(Clone, Copy)]
struct Fmt {
    tag: u16,
    channels: u16,
    rate: u32,
    align: u16,
    bits: u16,
    /// samples per block (ADPCM)
    spb: u32,
}

fn read_fmt(p: u32) -> Fmt {
    let (tag, channels, rate, align, bits) = (r16(p), r16(p + 2), r32(p + 4), r16(p + 12), r16(p + 14));
    let ch = channels.max(1) as u32;
    let spb = match tag {
        IMA if r16(p + 16) >= 2 => r16(p + 18) as u32,
        IMA => (align as u32).saturating_sub(4 * ch) * 8 / (4 * ch) + 1,
        MSADPCM if r16(p + 16) >= 2 => r16(p + 18) as u32,
        MSADPCM => ((align as u32).saturating_sub(7 * ch)) * 8 / (4 * ch) + 2,
        _ => 1,
    };
    Fmt { tag, channels, rate, align, bits, spb }
}

struct Stream {
    src: Fmt,
    dst: Fmt,
}

static STREAMS: Mutex<Option<HashMap<u32, Stream>>> = Mutex::new(None);
static NEXT: Mutex<u32> = Mutex::new(0x00ac_1000);

fn supported(s: &Fmt, d: &Fmt) -> bool {
    d.tag == PCM && d.bits == 16 && d.channels == s.channels && d.rate == s.rate && matches!(s.tag, IMA | MSADPCM)
        || s.tag == PCM && d.tag == PCM && s.channels == d.channels && s.rate == d.rate && s.bits == d.bits
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
            delta[c] = (MS_ADAPT[nib as usize] * delta[c] >> 8).max(16);
            out[start + i * ch + c] = p as i16;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// the API

fn acmGetVersion(_: &mut Ctx) -> u32 {
    0x0400_0000
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
    put(&mut b, 28, 3); // format tags
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
const TAGS: [(u16, u32, &str); 3] = [(PCM, 16, "PCM"), (IMA, 20, "IMA ADPCM"), (MSADPCM, 50, "Microsoft ADPCM")];
fn format_tag_details(c: &mut Ctx, wide: bool) -> u32 {
    let (p, flags) = (c.a(1), c.a(2));
    let t = match flags & 0xf {
        0 => TAGS.get(r32(p + 4) as usize),
        1 => TAGS.iter().find(|t| t.0 as u32 == r32(p + 8)),
        _ => TAGS.iter().max_by_key(|t| t.1),
    };
    let Some(&(tag, size, name)) = t else { return ACMERR_NOTPOSSIBLE };
    let i = TAGS.iter().position(|x| x.0 == tag).unwrap_or(0) as u32;
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
    if matches!(s.tag, IMA | MSADPCM | PCM) { 0 } else { ACMERR_NOTPOSSIBLE }
}
/// acmStreamOpen(phas, had, src, dst, filter, cb, inst, flags)
fn acmStreamOpen(c: &mut Ctx) -> u32 {
    let (ph, src, dst, flags) = (c.a(0), c.a(2), c.a(3), c.a(7));
    let (s, d) = (read_fmt(src), read_fmt(dst));
    if !supported(&s, &d) {
        if ph != 0 {
            put32(ph, 0);
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
    STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, Stream { src: s, dst: d });
    put32(ph, h);
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
    let v = if s.tag == PCM {
        n
    } else if flags & 0xf == 0 {
        // the output of n source bytes (whole blocks)
        n / s.align.max(1) as u32 * s.spb * ch * 2
    } else {
        // the source for n output bytes
        let per = (s.spb * ch * 2).max(1);
        (n / per) * s.align as u32
    };
    let _ = d;
    put32(out, v);
    if v == 0 { ACMERR_NOTPOSSIBLE } else { 0 }
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
    let Some((s, _d)) = with(h, |st| (st.src, st.dst)) else { return 5 };
    let (src, slen, dst, dlen) = (r32(hdr + 12), r32(hdr + 16), r32(hdr + 28), r32(hdr + 32));
    let (used_in, used_out) = if s.tag == PCM {
        let n = slen.min(dlen);
        mem::copy(dst, src, n);
        (n, n)
    } else {
        let ch = s.channels.max(1) as usize;
        let align = s.align.max(1) as u32;
        let per_block = s.spb * ch as u32 * 2;
        let blocks = (slen / align).min(dlen / per_block.max(1));
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
    w32(hdr + 20, used_in);
    w32(hdr + 36, used_out);
    // ACMSTREAMHEADER_STATUSF_DONE
    w32(hdr + 4, r32(hdr + 4) | 0x0001_0000);
    0
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
