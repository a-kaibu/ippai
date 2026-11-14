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
/// The same converter enumerated a second time as Windows' msadp32.acm ("MS-ADPCM"): F&C's
/// codec plugin looks for that short name and else plays no sound and halts
const DRIVER_MS: u32 = 0x00ac_0002;
/// (driver id, wPid, short name, long name) of the drivers acmDriverEnum lists, as Windows'
/// imaadp32.acm and msadp32.acm name themselves
const DRIVERS: [(u32, u32, &str, &str); 2] = [
    (DRIVER, 17, "Microsoft IMA ADPCM", "Microsoft IMA ADPCM CODEC"),
    (DRIVER_MS, 33, "MS-ADPCM", "Microsoft ADPCM"),
];
const IMA: u16 = 0x11;
const MSADPCM: u16 = 2;
const PCM: u16 = 1;
const MP3: u16 = 0x55;
/// How long a conversion waits for the page's decoder.
const MP3_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// How long a stream whose page did not answer last time waits (it answers late or never).
const MP3_LATE_WAIT: std::time::Duration = std::time::Duration::from_millis(20);
/// Samples (per channel) a stream keeps decoded past the destination's room: 3 s at 48 kHz.
const PENDING_MAX: usize = 3 * 48000;
/// MP3 bytes taken at a time (a few dozen frames).
const MP3_PIECE: usize = 16 << 10;
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
    /// MP3: MPEGLAYER3WAVEFORMAT's nBlockSize (0 when not given)
    block: u16,
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
    // MPEGLAYER3WAVEFORMAT: cbSize 12, wID, fdwFlags, nBlockSize, ...
    let block = if tag == MP3 && r16(p + 16) >= 12 { r16(p + 24) } else { 0 };
    Fmt { tag, channels, rate, avg: r32(p + 8), align, bits, spb, block, bad }
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
    /// samples owed for the frames taken (l3codecp gives each frame's, whatever the
    /// decoder's delay), not handed out yet
    due: usize,
    channels: usize,
    /// the decoder has given samples since the stream began (its delay is made up only before)
    heard: bool,
    /// the page did not answer the last decode in time
    late: bool,
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
/// MP3 to PCM: Windows takes 8-bit, mono and half the rate as well (winref acmmp3); those go
/// through the PCM converter after the decoder.
fn mp3_pair(s: &Fmt, d: &Fmt) -> bool {
    s.tag == MP3 && matches!(s.channels, 1 | 2) && s.rate != 0 && pcm_ok(d) && mp3_ok()
}
/// MP3's samples per frame: MPEG-1 1152, MPEG-2 and 2.5 (under 32 kHz) 576
fn mp3_spf(s: &Fmt) -> u32 {
    if s.rate >= 32000 { 1152 } else { 576 }
}
/// The 16-bit PCM the decoder gives (before any conversion to the stream's output)
fn mp3_native(s: &Fmt) -> Fmt {
    let ch = s.channels.max(1);
    Fmt { tag: PCM, channels: ch, rate: s.rate, avg: s.rate * ch as u32 * 2, align: ch * 2, bits: 16, spb: 1, block: 0, bad: false }
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
/// `src` as `d`, `max` frames at most (the destination's room: the rates are the guest's, and
/// 8000 -> 0xFFFFFFFF made a multi-GB Vec before it was cut to the buffer).
fn pcm_convert(s: &Fmt, d: &Fmt, src: &[u8], max: usize) -> Vec<u8> {
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
    for i in 0u64..max as u64 {
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
// drivers the program adds itself (acmDriverAdd, ACM_DRIVERADDF_FUNCTION): its own DriverProc
// gets the ACMDM_ messages Windows' msacm32 sends (a game's Vorbis.acm: LoadLibrary, then
// acmDriverAdd(&id, module, DriverProc, 0, ACM_DRIVERADDF_FUNCTION) and acmDriverOpen)

const DRV_LOAD: u32 = 1;
const DRV_ENABLE: u32 = 2;
const DRV_OPEN: u32 = 3;
const DRV_CLOSE: u32 = 4;
const DRV_DISABLE: u32 = 5;
const DRV_FREE: u32 = 6;
const ACMDM_DRIVER_DETAILS: u32 = 0x600a;
const ACMDM_FORMATTAG_DETAILS: u32 = 0x6019;
const ACMDM_FORMAT_SUGGEST: u32 = 0x601b;
const ACMDM_STREAM_OPEN: u32 = 0x604c;
const ACMDM_STREAM_CLOSE: u32 = 0x604d;
const ACMDM_STREAM_SIZE: u32 = 0x604e;
const ACMDM_STREAM_CONVERT: u32 = 0x604f;
const ACMDM_STREAM_RESET: u32 = 0x6050;
const ACMDM_STREAM_PREPARE: u32 = 0x6051;
const ACMDM_STREAM_UNPREPARE: u32 = 0x6052;
const MMSYSERR_ERROR: u32 = 1;
const MMSYSERR_INVALHANDLE: u32 = 5;
const MMSYSERR_NOTSUPPORTED: u32 = 8;
const ACMERR_UNPREPARED: u32 = 514;
/// ACMSTREAMHEADER's fdwStatus
const STATUSF_DONE: u32 = 0x0001_0000;
const STATUSF_PREPARED: u32 = 0x0002_0000;

/// HACMDRIVERID of added driver i: ADDED + i; its HACMDRIVER once opened: OPENED + i
const ADDED: u32 = 0x00ad_0000;
const OPENED: u32 = 0x00ae_0000;

#[derive(Clone)]
struct Added {
    proc_: u32,
    /// DRV_OPEN's answer, the dwDriverId of every later message (0: not open)
    id: u32,
    removed: bool,
}
static ADDED_DRIVERS: Mutex<Vec<Added>> = Mutex::new(Vec::new());

/// A stream on an added driver: its driver and the ACMDRVSTREAMINSTANCE (with the formats
/// copied next to it) it keeps for the stream's life
struct GuestStream {
    drv: usize,
    inst: u32,
    /// the program's ACMSTREAMHEADERs prepared on it -> the ACMDRVSTREAMHEADER the driver sees
    hdrs: HashMap<u32, u32>,
}
static GUEST_STREAMS: Mutex<Option<HashMap<u32, GuestStream>>> = Mutex::new(None);

fn added(i: usize) -> Option<Added> {
    ADDED_DRIVERS.lock().unwrap().get(i).cloned().filter(|d| !d.removed)
}
/// The added driver a HACMDRIVERID or HACMDRIVER names
fn added_index(h: u32) -> Option<usize> {
    let i = match h & 0xffff_0000 {
        ADDED | OPENED => (h & 0xffff) as usize,
        _ => return None,
    };
    added(i).map(|_| i)
}
fn send(c: &mut Ctx, i: usize, msg: u32, l1: u32, l2: u32) -> u32 {
    let Some(d) = added(i) else { return MMSYSERR_INVALHANDLE };
    c.call(d.proc_, &[d.id, OPENED + i as u32, msg, l1, l2])
}

/// Byte offsets of a structure's fields: DWORDs are 4 bytes, pointers `ps` and aligned to it
/// Field offsets and size of an msacm struct; msacm.h packs them to 1 byte, so a pointer after
/// a DWORD is not aligned on x64.
fn layout(fields: &[bool], ps: u32) -> (Vec<u32>, u32) {
    let mut o = 0u32;
    let mut v = Vec::new();
    for &ptr in fields {
        v.push(o);
        o += if ptr { ps } else { 4 };
    }
    (v, o)
}
fn put_field(at: u32, ptr: bool, v: u32) {
    if ptr { mem::wp(at, v) } else { w32(at, v) }
}
/// A WAVEFORMATEX's size (18 + cbSize; PCM's may be 16 bytes)
fn wfx_len(p: u32) -> u32 {
    if r16(p) == PCM { 16 } else { 18 + r16(p + 16) as u32 }
}
fn copy_out(p: u32, n: u32) -> u32 {
    let q = mem::heap_alloc(n.max(4), true);
    for i in 0..n {
        mem::w8(q + i, r8(p + i));
    }
    q
}

/// ACMDRVSTREAMINSTANCE: cbStruct, pwfxSrc, pwfxDst, pwfltr, dwCallback, dwInstance, fdwOpen,
/// fdwDriver, dwDriver, has
const INST: [bool; 10] = [false, true, true, true, true, true, false, false, true, true];
/// ACMDRVSTREAMHEADER: ACMSTREAMHEADER's fields, then fdwConvert, padshNext, fdwDriver,
/// dwDriver, fdwPrepared, dwPrepared, pbPreparedSrc, cbPreparedSrcLength, pbPreparedDst,
/// cbPreparedDstLength
const HDR: [bool; 21] = [
    false, false, true, true, false, false, true, true, false, false, true, // ACMSTREAMHEADER
    false, true, false, true, false, true, true, false, true, false,
];
const H_STATUS: usize = 1;
const H_SRC: usize = 3;
const H_SRC_LEN: usize = 4;
const H_SRC_USED: usize = 5;
const H_DST: usize = 7;
const H_DST_LEN: usize = 8;
const H_DST_USED: usize = 9;
const H_CONVERT: usize = 11;
const H_PREP_SRC: usize = 17;
const H_PREP_SRC_LEN: usize = 18;
const H_PREP_DST: usize = 19;
const H_PREP_DST_LEN: usize = 20;

fn hdr_get(h: u32, f: usize) -> u32 {
    let (o, _) = layout(&HDR, mem::ps());
    if HDR[f] { rp(h + o[f]) } else { r32(h + o[f]) }
}
fn hdr_set(h: u32, f: usize, v: u32) {
    let (o, _) = layout(&HDR, mem::ps());
    put_field(h + o[f], HDR[f], v)
}
/// The program's header's own fields (cbStruct .. dwDstUser) into the driver's, or back
fn hdr_sync(from: u32, to: u32) {
    for f in 0..11 {
        hdr_set(to, f, hdr_get(from, f));
    }
}

/// acmDriverAdd(&hadid, hinstModule, lParam, priority, flags): ACM_DRIVERADDF_FUNCTION (lParam
/// is the DriverProc); a notification window (ACM_DRIVERADDF_NOTIFYHWND) is taken and never
/// told; drivers by registry name are not there
fn driver_add(c: &mut Ctx) -> u32 {
    let (out, lparam, flags) = (c.a(0), c.a(2), c.a(4));
    if out == 0 {
        return MMSYSERR_INVALPARAM;
    }
    match flags & 7 {
        3 if lparam != 0 => {}
        4 => {
            putp(out, 0);
            return 0;
        }
        _ => return MMSYSERR_INVALPARAM,
    }
    let i = {
        let mut v = ADDED_DRIVERS.lock().unwrap();
        v.push(Added { proc_: lparam, id: 0, removed: false });
        v.len() - 1
    };
    // (Windows loads and enables it when it is added: a DriverProc that refuses DRV_LOAD is
    // not added)
    if send(c, i, DRV_LOAD, 0, 0) == 0 {
        ADDED_DRIVERS.lock().unwrap()[i].removed = true;
        return MMSYSERR_ERROR;
    }
    send(c, i, DRV_ENABLE, 0, 0);
    putp(out, ADDED + i as u32);
    0
}
fn acmDriverAddA(c: &mut Ctx) -> u32 {
    driver_add(c)
}
fn acmDriverAddW(c: &mut Ctx) -> u32 {
    driver_add(c)
}
/// acmDriverRemove(hadid, 0)
fn acmDriverRemove(c: &mut Ctx) -> u32 {
    let Some(i) = added_index(c.a(0)) else { return MMSYSERR_INVALHANDLE };
    send(c, i, DRV_DISABLE, 0, 0);
    send(c, i, DRV_FREE, 0, 0);
    ADDED_DRIVERS.lock().unwrap()[i].removed = true;
    0
}
/// Opens added driver i (DRV_OPEN with an ACMDRVOPENDESC); its dwDriverId
fn open_added(c: &mut Ctx, i: usize) -> u32 {
    if let Some(d) = added(i) {
        if d.id != 0 {
            return d.id;
        }
    }
    let ps = mem::ps();
    // ACMDRVOPENDESC: cbStruct, fccType, fccComp, dwVersion, dwFlags, dwError, pszSectionName,
    // pszAliasName, dnDevNode
    let f = [false, false, false, false, false, false, true, true, false];
    let (o, size) = layout(&f, ps);
    let d = mem::heap_alloc(size, true);
    w32(d + o[0], size);
    w32(d + o[1], u32::from_le_bytes(*b"audc"));
    w32(d + o[3], 0x0500_0000);
    let id = send(c, i, DRV_OPEN, 0, d);
    mem::heap_free(d);
    if id != 0 {
        ADDED_DRIVERS.lock().unwrap()[i].id = id;
    }
    id
}
fn acmDriverOpen(c: &mut Ctx) -> u32 {
    let (out, id) = (c.a(0), c.a(1));
    if out == 0 {
        return MMSYSERR_INVALPARAM;
    }
    if let Some(i) = added_index(id) {
        if open_added(c, i) == 0 {
            return MMSYSERR_ERROR;
        }
        putp(out, OPENED + i as u32);
        return 0;
    }
    if id & 0xffff_0000 == ADDED {
        return MMSYSERR_INVALHANDLE;
    }
    // (a built-in driver gives back itself; another id: the first)
    putp(out, if DRIVERS.iter().any(|d| d.0 == id) { id } else { DRIVER });
    0
}
fn acmDriverClose(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h & 0xffff_0000 == OPENED {
        let Some(i) = added_index(h) else { return MMSYSERR_INVALHANDLE };
        if let Some(d) = added(i) {
            if d.id != 0 {
                send(c, i, DRV_CLOSE, 0, 0);
                ADDED_DRIVERS.lock().unwrap()[i].id = 0;
            }
        }
    }
    0
}

/// acmStreamOpen on added driver i; Some(result) when the driver answered
fn stream_open_added(c: &mut Ctx, i: usize, ph: u32, src: u32, dst: u32, filter: u32, cb: u32, inst: u32, flags: u32) -> u32 {
    if open_added(c, i) == 0 {
        return MMSYSERR_ERROR;
    }
    let ps = mem::ps();
    let (o, size) = layout(&INST, ps);
    let (ls, ld) = (wfx_len(src), wfx_len(dst));
    let si = mem::heap_alloc(size, true);
    let (s2, d2) = (copy_out(src, ls), copy_out(dst, ld));
    let f2 = if filter != 0 { copy_out(filter, r32(filter).clamp(12, 0x1000)) } else { 0 };
    let h = {
        let mut n = NEXT.lock().unwrap();
        *n += 1;
        *n
    };
    for (k, v) in [(0, size), (1, s2), (2, d2), (3, f2), (4, cb), (5, inst), (6, flags), (9, h)] {
        put_field(si + o[k], INST[k], v);
    }
    let r = send(c, i, ACMDM_STREAM_OPEN, si, 0);
    // ACM_STREAMOPENF_QUERY, or a refusal: nothing stays open
    if r != 0 || flags & 1 != 0 {
        for p in [si, s2, d2, f2] {
            if p != 0 {
                mem::heap_free(p);
            }
        }
        if ph != 0 && r != 0 {
            putp(ph, 0);
        }
        return r;
    }
    GUEST_STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, GuestStream { drv: i, inst: si, hdrs: HashMap::default() });
    putp(ph, h);
    0
}
fn guest<R>(h: u32, f: impl FnOnce(&mut GuestStream) -> R) -> Option<R> {
    GUEST_STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).get_mut(&h).map(f)
}
fn guest_close(c: &mut Ctx, h: u32) -> Option<u32> {
    let (drv, inst) = guest(h, |g| (g.drv, g.inst))?;
    let r = send(c, drv, ACMDM_STREAM_CLOSE, inst, 0);
    if r != 0 {
        return Some(r);
    }
    let g = GUEST_STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h)?;
    let (o, _) = layout(&INST, mem::ps());
    for k in [1, 2, 3] {
        let p = rp(g.inst + o[k]);
        if p != 0 {
            mem::heap_free(p);
        }
    }
    mem::heap_free(g.inst);
    for (_, d) in g.hdrs {
        mem::heap_free(d);
    }
    Some(0)
}
fn guest_size(c: &mut Ctx, h: u32, n: u32, out: u32, flags: u32) -> Option<u32> {
    let (drv, inst) = guest(h, |g| (g.drv, g.inst))?;
    // ACMDRVSTREAMSIZE: cbStruct, fdwSize, cbSrcLength, cbDstLength
    let z = mem::heap_alloc(16, true);
    w32(z, 16);
    w32(z + 4, flags);
    w32(z + if flags & 0xf == 0 { 8 } else { 12 }, n);
    let r = send(c, drv, ACMDM_STREAM_SIZE, inst, z);
    if r == 0 {
        put32(out, r32(z + if flags & 0xf == 0 { 12 } else { 8 }));
    }
    mem::heap_free(z);
    Some(r)
}
fn guest_prepare(c: &mut Ctx, h: u32, hdr: u32, flags: u32) -> Option<u32> {
    let (drv, inst) = guest(h, |g| (g.drv, g.inst))?;
    if hdr == 0 || r32(hdr) < 44 {
        return Some(MMSYSERR_INVALPARAM);
    }
    if r32(hdr + 4) & STATUSF_PREPARED != 0 {
        return Some(0);
    }
    let (_, size) = layout(&HDR, mem::ps());
    let d = mem::heap_alloc(size, true);
    hdr_sync(hdr, d);
    hdr_set(d, H_CONVERT, flags);
    let r = send(c, drv, ACMDM_STREAM_PREPARE, inst, d);
    // (a driver with nothing to prepare says MMSYSERR_NOTSUPPORTED: msacm32 prepares it)
    if r != 0 && r != MMSYSERR_NOTSUPPORTED {
        mem::heap_free(d);
        return Some(r);
    }
    hdr_set(d, H_PREP_SRC, hdr_get(hdr, H_SRC));
    hdr_set(d, H_PREP_SRC_LEN, hdr_get(hdr, H_SRC_LEN));
    hdr_set(d, H_PREP_DST, hdr_get(hdr, H_DST));
    hdr_set(d, H_PREP_DST_LEN, hdr_get(hdr, H_DST_LEN));
    w32(hdr + 4, (r32(hdr + 4) | STATUSF_PREPARED) & !STATUSF_DONE);
    guest(h, |g| g.hdrs.insert(hdr, d));
    Some(0)
}
fn guest_unprepare(c: &mut Ctx, h: u32, hdr: u32) -> Option<u32> {
    let (drv, inst) = guest(h, |g| (g.drv, g.inst))?;
    let Some(d) = guest(h, |g| g.hdrs.get(&hdr).copied()).flatten() else { return Some(ACMERR_UNPREPARED) };
    hdr_sync(hdr, d);
    let r = send(c, drv, ACMDM_STREAM_UNPREPARE, inst, d);
    if r != 0 && r != MMSYSERR_NOTSUPPORTED {
        return Some(r);
    }
    guest(h, |g| g.hdrs.remove(&hdr));
    mem::heap_free(d);
    w32(hdr + 4, r32(hdr + 4) & !STATUSF_PREPARED);
    Some(0)
}
fn guest_convert(c: &mut Ctx, h: u32, hdr: u32, flags: u32) -> Option<u32> {
    let (drv, inst) = guest(h, |g| (g.drv, g.inst))?;
    let Some(d) = guest(h, |g| g.hdrs.get(&hdr).copied()).flatten() else { return Some(ACMERR_UNPREPARED) };
    // (the buffers have to be the ones prepared)
    if hdr_get(hdr, H_SRC) != hdr_get(d, H_PREP_SRC) || hdr_get(hdr, H_DST) != hdr_get(d, H_PREP_DST)
        || hdr_get(hdr, H_SRC_LEN) > hdr_get(d, H_PREP_SRC_LEN) || hdr_get(hdr, H_DST_LEN) > hdr_get(d, H_PREP_DST_LEN)
    {
        return Some(MMSYSERR_INVALPARAM);
    }
    hdr_sync(hdr, d);
    hdr_set(d, H_CONVERT, flags);
    hdr_set(d, H_SRC_USED, 0);
    hdr_set(d, H_DST_USED, 0);
    hdr_set(d, H_STATUS, hdr_get(hdr, H_STATUS) & !STATUSF_DONE);
    let r = send(c, drv, ACMDM_STREAM_CONVERT, inst, d);
    hdr_set(hdr, H_SRC_USED, hdr_get(d, H_SRC_USED));
    hdr_set(hdr, H_DST_USED, hdr_get(d, H_DST_USED));
    if r == 0 {
        w32(hdr + 4, r32(hdr + 4) | STATUSF_DONE);
    }
    Some(r)
}
fn acmStreamReset(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if let Some((drv, inst)) = guest(h, |g| (g.drv, g.inst)) {
        let r = send(c, drv, ACMDM_STREAM_RESET, inst, 0);
        return if r == MMSYSERR_NOTSUPPORTED { 0 } else { r };
    }
    0
}
/// ACMDM_FORMAT_SUGGEST on added driver i (ACMDRVFORMATSUGGEST: cbStruct, fdwSuggest,
/// pwfxSrc, cbwfxSrc, pwfxDst, cbwfxDst)
fn suggest_added(c: &mut Ctx, i: usize, src: u32, dst: u32, n: u32, flags: u32) -> u32 {
    if open_added(c, i) == 0 {
        return MMSYSERR_ERROR;
    }
    let f = [false, false, true, false, true, false];
    let (o, size) = layout(&f, mem::ps());
    let z = mem::heap_alloc(size, true);
    for (k, v) in [(0, size), (1, flags), (2, src), (3, wfx_len(src)), (4, dst), (5, n)] {
        put_field(z + o[k], f[k], v);
    }
    let r = send(c, i, ACMDM_FORMAT_SUGGEST, z, 0);
    mem::heap_free(z);
    r
}
fn added_count() -> usize {
    ADDED_DRIVERS.lock().unwrap().len()
}

// ---------------------------------------------------------------------------------------------
// the API

fn acmGetVersion(_: &mut Ctx) -> u32 {
    0x0500_0000
}
fn acmDriverEnum(c: &mut Ctx) -> u32 {
    let (cb, inst) = (c.a(0), c.a(1));
    if cb != 0 {
        for (id, ..) in DRIVERS {
            // ACMDRIVERDETAILS_SUPPORTF_CODEC; FALSE stops the enumeration
            if c.call(cb, &[id, inst, 1]) == 0 {
                break;
            }
        }
    }
    0
}
/// acmDriverID(hao, &hadid, 0): an added driver's id for its HACMDRIVER or streams
fn acmDriverID(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let id = if let Some(i) = added_index(h) {
        ADDED + i as u32
    } else if let Some(i) = guest(h, |g| g.drv) {
        ADDED + i as u32
    } else if let Some(&(d, ..)) = DRIVERS.iter().find(|d| d.0 == h) {
        d
    } else {
        DRIVER
    };
    crate::mem::putp(c.a(1), id);
    0
}
/// ACMDRIVERDETAILS' numbers and hicon, before its strings
fn details_head() -> u32 {
    if mem::x64() { 44 } else { 40 }
}
fn driver_details(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(1);
    let (_, pid, short, long) = DRIVERS.iter().copied().find(|d| d.0 == c.a(0)).unwrap_or(DRIVERS[0]);
    if p == 0 {
        return MMSYSERR_INVALPARAM;
    }
    let size = r32(p);
    if !(4..=0x10000).contains(&size) {
        return MMSYSERR_INVALPARAM;
    }
    // (the whole ACMDRIVERDETAILS, of which the first cbStruct bytes are copied and cbStruct
    // becomes the bytes given, as msacm32 does: a field cut in two keeps its first bytes)
    let full = details_head() as usize + if wide { 2 * 880 } else { 880 };
    let mut b = vec![0u8; full];
    let put = |b: &mut Vec<u8>, at: usize, v: u32| {
        if at + 4 <= b.len() {
            b[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
    };
    put(&mut b, 4, u32::from_le_bytes(*b"audc"));
    put(&mut b, 8, 0);
    put(&mut b, 12, 1 | pid << 16); // wMid, wPid
    put(&mut b, 16, 0x0300_0000);
    put(&mut b, 20, 0x0400_0000);
    put(&mut b, 24, 1); // codec
    put(&mut b, 28, tags().len() as u32); // format tags
    let strs: [(&str, usize); 4] = [(short, 32), (long, 128), ("Copyright (C) Microsoft Corp.", 80), ("", 128)];
    // (the strings follow hicon: offset 36 on x86, 40 and 8 bytes on x64, packed)
    let mut at = details_head() as usize;
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
    let n = (size as usize).min(full);
    b.truncate(n);
    b[0..4].copy_from_slice(&(n as u32).to_le_bytes());
    mem::write(p, &b);
    0
}
/// An added driver's ACMDRIVERDETAILSW (ACMDM_DRIVER_DETAILS); the A form's strings are the
/// W ones in the ANSI code page (the driver only speaks W)
fn added_details(c: &mut Ctx, i: usize, p: u32, wide: bool) -> u32 {
    if p == 0 || r32(p) < 4 {
        return MMSYSERR_INVALPARAM;
    }
    if open_added(c, i) == 0 {
        return MMSYSERR_ERROR;
    }
    // (40 bytes of numbers, then short name 32, long name 128, copyright 80, licensing 128,
    // features 512 characters)
    const STRS: [u32; 5] = [32, 128, 80, 128, 512];
    let head = details_head();
    let full_w = head + 2 * STRS.iter().sum::<u32>();
    let w = mem::heap_alloc(full_w, true);
    w32(w, full_w);
    let r = send(c, i, ACMDM_DRIVER_DETAILS, w, 0);
    if r == 0 {
        let want = r32(p);
        let mut b: Vec<u8> = (0..head).map(|k| r8(w + k)).collect();
        let mut at = head;
        for n in STRS {
            let units: Vec<u16> = (0..n).map(|k| r16(w + at + 2 * k)).take_while(|&u| u != 0).collect();
            if wide {
                let mut v = vec![0u8; 2 * n as usize];
                for (k, u) in units.iter().enumerate() {
                    v[2 * k..2 * k + 2].copy_from_slice(&u.to_le_bytes());
                }
                b.extend(v);
            } else {
                let mut v = crate::text::w2a(&units);
                v.truncate(n as usize - 1);
                v.resize(n as usize, 0);
                b.extend(v);
            }
            at += 2 * n;
        }
        b.truncate(want as usize);
        let k = 4.min(b.len());
        b[..k].copy_from_slice(&want.to_le_bytes()[..k]);
        mem::write(p, &b);
    }
    mem::heap_free(w);
    r
}
fn acmDriverDetailsA(c: &mut Ctx) -> u32 {
    if let Some(i) = added_index(c.a(0)) {
        return added_details(c, i, c.a(1), false);
    }
    driver_details(c, false)
}
fn acmDriverDetailsW(c: &mut Ctx) -> u32 {
    if let Some(i) = added_index(c.a(0)) {
        return added_details(c, i, c.a(1), true);
    }
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
    // (an added driver opened: its own answer)
    if let Some(i) = added_index(c.a(0)) {
        if open_added(c, i) == 0 {
            return MMSYSERR_ERROR;
        }
        return send(c, i, ACMDM_FORMATTAG_DETAILS, c.a(1), c.a(2));
    }
    format_tag_details(c, true)
}
fn acmMetrics(c: &mut Ctx) -> u32 {
    let (metric, out) = (c.a(1), c.a(2));
    let v = match metric {
        1 | 2 | 3 => DRIVERS.len() as u32, // counts of drivers / codecs
        50 => 50,             // ACM_METRIC_MAX_SIZE_FORMAT
        51 => 0,              // ..._FILTER
        _ => 0,
    };
    put32(out, v);
    0
}
/// acmFormatSuggest(had, src, dst, cbDst, flags): 16-bit PCM for ADPCM
fn acmFormatSuggest(c: &mut Ctx) -> u32 {
    let (had, src, dst, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if src == 0 || dst == 0 || n < 16 {
        return MMSYSERR_INVALPARAM;
    }
    if let Some(i) = added_index(had) {
        return suggest_added(c, i, src, dst, n, c.a(4));
    }
    let s = read_fmt(src);
    if !matches!(s.tag, IMA | MSADPCM | PCM) && !(s.tag == MP3 && mp3_ok()) || s.bad {
        if had == 0 {
            for i in (0..added_count()).rev() {
                if added(i).is_some() && suggest_added(c, i, src, dst, n, c.a(4)) == 0 {
                    return 0;
                }
            }
        }
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
    let (ph, had, src, dst, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(7));
    if src == 0 || dst == 0 {
        return MMSYSERR_INVALPARAM;
    }
    // a driver the program added: the one it opened, or with no driver named, any of them
    // before the built-in one (as Windows tries the process's own drivers first)
    if let Some(i) = added_index(had) {
        return stream_open_added(c, i, ph, src, dst, c.a(4), c.a(5), c.a(6), flags);
    }
    if had & 0xffff_0000 == ADDED || had & 0xffff_0000 == OPENED {
        return MMSYSERR_INVALHANDLE;
    }
    if had == 0 {
        for i in (0..added_count()).rev() {
            if added(i).is_some() && stream_open_added(c, i, ph, src, dst, c.a(4), c.a(5), c.a(6), flags) == 0 {
                return 0;
            }
        }
    }
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
        Some(std::sync::Arc::new(Mutex::new(Mp3 { dec, split: Default::default(), pending: Default::default(), due: 0, channels: s.channels as usize, heard: false, late: false })))
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
    if let Some(r) = guest_close(c, c.a(0)) {
        return r;
    }
    STREAMS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0));
    0
}
/// acmStreamSize(has, cbInput, pdwOutputBytes, flags): SOURCE (0) or DESTINATION (1)
fn acmStreamSize(c: &mut Ctx) -> u32 {
    let (h, n, out, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if let Some(r) = guest_size(c, h, n, out, flags) {
        return r;
    }
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
/// MP3's sizes as l3codecp gives them (winref acmmp3): a frame's output for each block of
/// the source begun (nBlockSize, else a frame at the average byte rate), and for an output
/// size the source of the whole frames it holds.
fn mp3_size(s: &Fmt, d: &Fmt, n: u32, from_dst: bool) -> u32 {
    let spf = mp3_spf(s) as u64;
    let block = if s.block != 0 { s.block as u32 } else { ((s.avg as u64 * spf / s.rate.max(1) as u64) as u32).max(1) };
    let dst_frame = ((spf * d.rate as u64).div_ceil(s.rate.max(1) as u64) * d.align as u64).max(1) as u32;
    if from_dst { n / dst_frame * block } else { n.div_ceil(block).saturating_mul(dst_frame) }
}
fn acmStreamPrepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    if let Some(r) = guest_prepare(c, c.a(0), h, c.a(2)) {
        return r;
    }
    // (l3codecp refuses a header with no source: winref acmmp3, its probe's last call)
    let p = mem::ps();
    if with(c.a(0), |st| st.src.tag == MP3).unwrap_or(false) && r32(h + 8 + 2 * p) == 0 {
        return MMSYSERR_INVALPARAM;
    }
    w32(h + 4, r32(h + 4) | 0x0002_0000);
    0
}
fn acmStreamUnprepareHeader(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    if let Some(r) = guest_unprepare(c, c.a(0), h) {
        return r;
    }
    w32(h + 4, r32(h + 4) & !0x0002_0000);
    0
}
/// acmStreamConvert(has, ACMSTREAMHEADER*, flags)
fn acmStreamConvert(c: &mut Ctx) -> u32 {
    let (h, hdr) = (c.a(0), c.a(1));
    let (h, hdr, flags) = (h, hdr, c.a(2));
    if let Some(r) = guest_convert(c, h, hdr, flags) {
        return r;
    }
    let Some((s, d, mp3)) = with(h, |st| (st.src, st.dst, st.mp3.clone())) else { return 5 };
    // ACMSTREAMHEADER: cbStruct, fdwStatus, dwUser, pbSrc, cbSrcLength, cbSrcLengthUsed,
    // dwSrcUser, pbDst, cbDstLength, cbDstLengthUsed (the pointer-sized ones 8 bytes on x64)
    let p = mem::ps();
    let (o_src, o_dst) = (8 + p, 8 + 3 * p + 8);
    let (src, slen, dst, dlen) = (rp(hdr + o_src), r32(hdr + o_src + p), rp(hdr + o_dst), r32(hdr + o_dst + p));
    let (o_sused, o_dused) = (o_src + p + 4, o_dst + p + 4);
    let (used_in, used_out) = if let Some(m) = mp3 {
        let input = mem::slice(src, if mem::guest(src, slen) { slen } else { 0 });
        let n = mp3_native(&s);
        // (the decoder's frames that fit, at its rate)
        let room = (dlen / d.align.max(1) as u32) as usize;
        let frames = (room as u64 * s.rate as u64 / d.rate.max(1) as u64).min(usize::MAX as u64 / 4);
        let (out, used) = mp3_convert(&mut m.lock().unwrap_or_else(|e| e.into_inner()), input, frames as usize * n.channels as usize, flags & 0x10 != 0, flags & 0x20 != 0);
        let mut bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
        if (d.bits, d.channels, d.rate) != (16, n.channels, n.rate) {
            bytes = pcm_convert(&n, &d, &bytes, room);
            // (a format whose block align is not its frame's)
            bytes.truncate(room * d.align as usize);
        }
        mem::write(dst, &bytes);
        (used as u32, bytes.len() as u32)
    } else if s.tag == PCM {
        let sa = s.align.max(1) as u32;
        let n = slen / sa * sa;
        let input = mem::slice(src, if mem::guest(src, n) { n } else { 0 });
        let room = (dlen / d.align.max(1) as u32) as usize;
        let mut out = pcm_convert(&s, &d, input, room);
        out.truncate(room * d.align as usize);
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
            let b = mem::slice(src + k * align, align);
            if s.tag == IMA {
                ima_block(b, ch, s.spb as usize, &mut out);
            } else {
                ms_block(b, ch, s.spb as usize, &mut out);
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

/// Takes MP3 bytes (`start`: ACM_STREAMCONVERTF_START, a new stream; `end`:
/// ACM_STREAMCONVERTF_END, the last frame goes too); up to `max` samples out. As l3codecp,
/// each frame gives its samples once the next one's header is here: what the decoder holds
/// back for its delay (FFmpeg's first 529, or a chunk it answers only with the next) comes as
/// silence first.
/// The bytes taken are fewer than given (cbSrcLengthUsed) once what is decoded and not handed
/// out yet passes the destination's room and PENDING_MAX: a whole track converted into a small
/// buffer at a time kept all of it decoded (10 MB a minute).
fn mp3_convert(m: &mut Mp3, input: &[u8], max: usize, start: bool, end: bool) -> (Vec<i16>, usize) {
    if start {
        m.split.reset();
        m.pending.clear();
        m.due = 0;
        m.heard = false;
        kura::adec::reset(m.dec);
    }
    let cap = max.saturating_add(PENDING_MAX * m.channels.max(1));
    let mut used = 0;
    loop {
        // (a piece at a time, while there is room for what it decodes to)
        let piece = if m.pending.len() < cap { (input.len() - used).min(MP3_PIECE) } else { 0 };
        let last = used + piece == input.len();
        let mut frames = Vec::new();
        m.split.push(&input[used..used + piece], &mut frames);
        used += piece;
        if end && last {
            m.split.finish(&mut frames);
        }
        m.due += frames.iter().map(|f| f.0.samples as usize * m.channels).sum::<usize>();
        let mut pcm = Vec::new();
        // (a page that did not answer is waited for briefly until it does: a game converting in
        // its main loop waited MP3_WAIT every call)
        let wait = if m.late { MP3_LATE_WAIT } else { MP3_WAIT };
        let answered = kura::adec::decode(m.dec, frames.iter().map(|f| &f.1[..]), wait, &mut pcm).is_some();
        if !answered && !frames.is_empty() && !m.late {
            eprintln!("sake: acm: mp3: the page's decoder did not answer");
        }
        m.late = !answered;
        // (as FFmpeg's float to 16-bit: rounded, clipped)
        m.pending.extend(pcm.iter().map(|&x| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16));
        // (only at the start: later a decoder that answers a chunk late, as Chrome's can, is
        // waited for, not filled in, or the sound would have gaps)
        if answered && !m.heard && m.pending.len() < m.due {
            let short = m.due - m.pending.len();
            for _ in 0..short {
                m.pending.push_front(0);
            }
        }
        m.heard |= !pcm.is_empty();
        if last || piece == 0 || !answered {
            break;
        }
    }
    let n = max.min(m.pending.len()).min(m.due);
    m.due -= n;
    (m.pending.drain(..n).collect(), used)
}

pub fn init() {
    for (n, f) in [
        ("acmGetVersion", acmGetVersion as crate::api::Handler),
        ("acmDriverEnum", acmDriverEnum),
        ("acmDriverID", acmDriverID),
        ("acmDriverOpen", acmDriverOpen),
        ("acmDriverClose", acmDriverClose),
        ("acmDriverAddA", acmDriverAddA),
        ("acmDriverAddW", acmDriverAddW),
        ("acmDriverRemove", acmDriverRemove),
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
        ("acmStreamReset", acmStreamReset),
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

    /// msacmdrv.h structs are pack(1): the sizes a driver built with it expects.
    #[test]
    fn driver_struct_layouts() {
        assert_eq!(layout(&INST, 4).1, 40);
        assert_eq!(layout(&INST, 8).1, 68);
        assert_eq!(layout(&HDR, 4).1, 84);
        assert_eq!(layout(&HDR, 8).1, 124);
        // ACMDRVSTREAMHEADER.pbSrc follows dwUser; on x64 padshNext sits right after fdwConvert
        assert_eq!(layout(&HDR, 8).0[3], 16);
        assert_eq!(layout(&HDR, 8).0[12], 68);
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

        /// Packed layout: every field starts where the one before ends, whatever the mix.
        #[test]
        fn layout_is_packed(fields in vec(any::<bool>(), 0..32), x64 in any::<bool>()) {
            let ps = if x64 { 8 } else { 4 };
            let (offs, size) = layout(&fields, ps);
            let mut o = 0;
            for (f, at) in fields.iter().zip(&offs) {
                prop_assert_eq!(*at, o);
                o += if *f { ps } else { 4 };
            }
            prop_assert_eq!(size, o);
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
    use proptest::prelude::*;

    fn pcm(channels: u16, rate: u32, bits: u16, align: u16) -> Fmt {
        Fmt { tag: PCM, channels, rate, avg: rate * align as u32, align, bits, spb: 1, block: 0, bad: false }
    }

    /// pcm_convert as it was: the whole output, then cut to the destination.
    fn pcm_convert_old(s: &Fmt, d: &Fmt, src: &[u8], room: usize) -> Vec<u8> {
        let mut v = pcm_convert(s, d, src, usize::MAX);
        v.truncate(room * d.align as usize);
        v
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]

        /// Any rates, sizes and rooms: the bytes are those of the whole conversion cut to the
        /// room, and never more than the room asks for (rates up to 0xFFFFFFFF made GB first).
        #[test]
        fn pcm_stops_at_the_room(sc in 1u16..=2, dc in 1u16..=2, s8 in any::<bool>(), d8 in any::<bool>(), sr in 1u32..200_000, dr in 1u32..200_000, frames in 0usize..300, room in 0usize..2000, seed: u8) {
            let (sb, db) = (if s8 { 8 } else { 16 }, if d8 { 8 } else { 16 });
            let s = pcm(sc, sr, sb, sc * sb / 8);
            let d = pcm(dc, dr, db, dc * db / 8);
            let src: Vec<u8> = (0..frames * s.align as usize).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)).collect();
            let got = pcm_convert(&s, &d, &src, room);
            prop_assert!(got.len() <= room * d.align as usize);
            // (the old one only where its whole output is small)
            if (frames as u64) * (dr as u64) / (sr as u64) < 100_000 {
                prop_assert_eq!(got, pcm_convert_old(&s, &d, &src, room));
            }
        }
    }

    /// A rate of 0xFFFFFFFF into a small buffer: the room's frames, at once.
    #[test]
    fn pcm_huge_rate_is_cut_early() {
        let s = pcm(2, 8000, 16, 4);
        let d = pcm(2, u32::MAX, 16, 4);
        let src = vec![1u8; 1 << 20];
        let t = std::time::Instant::now();
        assert_eq!(pcm_convert(&s, &d, &src, 1024).len(), 4096);
        assert!(t.elapsed() < std::time::Duration::from_secs(1));
    }
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
        Mp3 { dec, split: Default::default(), pending: Default::default(), due: 0, channels: ch as usize, heard: false, late: false }
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
        // a frame alone: nothing yet (it waits for the next header)
        assert!(mp3_convert(&mut m, &bytes[..417], usize::MAX, false, false).0.is_empty());
        // and a half more: one frame out, the half waits
        let out = mp3_convert(&mut m, &bytes[417..417 + 200], usize::MAX, false, false).0;
        assert_eq!(out.len(), 1152 * 2);
        assert!(out.chunks(2).all(|f| f == [sample(1, 0), sample(1, 1)]));
        // the rest, into a buffer of 1000 samples: the others come on later calls, in order
        let mut got = mp3_convert(&mut m, &bytes[417 + 200..], 1000, false, true).0;
        assert_eq!(got.len(), 1000);
        loop {
            let more = mp3_convert(&mut m, &[], 1000, false, false).0;
            if more.is_empty() {
                break;
            }
            got.extend(more);
        }
        assert_eq!(got.len(), 3 * 1152 * 2);
        let want: Vec<i16> = (2..=4).flat_map(|k| (0..1152).flat_map(move |_| [sample(k, 0), sample(k, 1)])).collect();
        assert_eq!(got, want);
        // START: what was waiting goes; the stream begins again
        mp3_convert(&mut m, &bytes[..417 + 9], 10, false, false);
        let out = mp3_convert(&mut m, &frame(9, false), usize::MAX, true, true).0;
        assert_eq!(out.len(), 1152 * 2);
        assert_eq!(&out[..2], [sample(9, 0), sample(9, 1)]);
        stop.store(true, Ordering::Relaxed);
        p.join().unwrap();
    }

    /// A whole track converted into a small buffer a call at a time: what is decoded and not
    /// handed out stays near PENDING_MAX (cbSrcLengthUsed falls short and the rest comes again),
    /// and every sample comes out once, in order (it kept the whole track decoded).
    #[test]
    fn a_whole_track_into_a_small_buffer() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let stop = Arc::new(AtomicBool::new(false));
        let p = page(stop.clone());
        let mut m = stream(2);
        // 600 frames (15 s); the body byte names each
        let track: Vec<u8> = (0..600u32).flat_map(|k| frame((k % 251) as u8 + 1, false)).collect();
        let mut at = 0;
        let mut got: Vec<i16> = Vec::new();
        let mut most = 0;
        for call in 0.. {
            assert!(call < 10_000, "no progress");
            let (out, used) = mp3_convert(&mut m, &track[at..], 4096, call == 0, true);
            assert!(out.len() <= 4096);
            at += used;
            most = most.max(m.pending.len());
            if out.is_empty() && at == track.len() {
                break;
            }
            got.extend(out);
        }
        assert!(most <= 4096 + PENDING_MAX * 2 + 2 * MP3_PIECE / 417 * 1152 * 2, "{most} samples kept");
        // (the decoder's frames: each frame's samples once the next header is here, the last at END)
        let want: Vec<i16> = (0..600u32).flat_map(|k| {
            let b = (k % 251) as u8 + 1;
            (0..1152).flat_map(move |_| [sample(b, 0), sample(b, 1)])
        }).collect();
        assert_eq!(got.len(), want.len());
        assert!(got == want);
        stop.store(true, Ordering::Relaxed);
        p.join().unwrap();
    }

    /// After the page did not answer once, later calls wait briefly (a game converting in its
    /// main loop waited 2 s a call), and the samples still come when it answers.
    #[test]
    fn a_late_page_is_not_waited_for_again() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let mut m = stream(1);
        assert!(mp3_convert(&mut m, &frame(3, true), usize::MAX, false, true).0.is_empty());
        assert!(m.late);
        let t = std::time::Instant::now();
        for k in 4..10 {
            let _ = mp3_convert(&mut m, &frame(k, true), usize::MAX, false, false);
        }
        assert!(t.elapsed() < MP3_WAIT, "{:?} for 6 calls", t.elapsed());
        // (the page comes back)
        let stop = Arc::new(AtomicBool::new(false));
        let p = page(stop.clone());
        let (out, _) = mp3_convert(&mut m, &[], usize::MAX, false, true);
        stop.store(true, Ordering::Relaxed);
        p.join().unwrap();
        assert!(!m.late);
        assert_eq!(out.len(), 1152 * 7);
        assert_eq!(out[0], sample(3, 0));
        assert_eq!(out[1152 * 6], sample(9, 0));
    }

    #[test]
    fn no_answer_gives_what_there_is() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let mut m = stream(1);
        // no page running: the wait ends, nothing out, nothing lost later
        let t = std::time::Instant::now();
        assert!(mp3_convert(&mut m, &frame(3, true), usize::MAX, false, true).0.is_empty());
        assert!(t.elapsed() >= MP3_WAIT);
        kura::adec::test_page_pass(&mut |b, ch| vec![b[4] as f32 / 256.0; 1152 * ch as usize]);
        let out = mp3_convert(&mut m, &[], usize::MAX, false, false).0;
        assert_eq!(out.len(), 1152);
        assert_eq!(out[0], sample(3, 0));
    }

    #[test]
    fn decoder_delay_comes_as_silence() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let stop = Arc::new(AtomicBool::new(false));
        // a decoder that drops its first 529 samples (as FFmpeg's)
        let first = Arc::new(AtomicBool::new(true));
        let f2 = first.clone();
        let st = stop.clone();
        let p = std::thread::spawn(move || {
            while !st.load(Ordering::Relaxed) {
                kura::adec::test_page_pass(&mut |b, ch| {
                    let n = if f2.swap(false, Ordering::Relaxed) { 1152 - 529 } else { 1152 };
                    vec![b[4] as f32 / 256.0; n * ch as usize]
                });
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        });
        let mut m = stream(1);
        let bytes: Vec<u8> = (1..=3).flat_map(|k| frame(k, true)).collect();
        // each frame gives 1152 once the next header is here, the first led by 529 zeros
        let a = mp3_convert(&mut m, &bytes[..417 + 4], usize::MAX, true, false).0;
        assert_eq!(a.len(), 1152);
        assert!(a[..529].iter().all(|&v| v == 0));
        assert!(a[529..].iter().all(|&v| v == sample(1, 0)));
        let b = mp3_convert(&mut m, &bytes[417 + 4..], usize::MAX, false, true).0;
        assert_eq!(b.len(), 2 * 1152);
        assert_eq!(b[0], sample(2, 0));
        assert_eq!(b[1152], sample(3, 0));
        stop.store(true, Ordering::Relaxed);
        p.join().unwrap();
    }

    #[test]
    fn decoder_a_chunk_late_has_no_gaps() {
        let _g = crate::ADEC_SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        kura::adec::test_page_pass(&mut |_, _| Vec::new());
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        // a decoder that answers each chunk with the one before's samples (nothing for the first)
        let p = std::thread::spawn(move || {
            let mut last: Option<u8> = None;
            while !st.load(Ordering::Relaxed) {
                kura::adec::test_page_pass(&mut |b, ch| {
                    let out = last.map(|k| vec![k as f32 / 256.0; 1152 * ch as usize]).unwrap_or_default();
                    last = Some(b[4]);
                    out
                });
                std::thread::sleep(std::time::Duration::from_micros(200));
            }
        });
        let mut m = stream(1);
        let bytes: Vec<u8> = (1..=6).flat_map(|k| frame(k, true)).collect();
        let mut got = Vec::new();
        for (k, piece) in bytes.chunks(300).enumerate() {
            got.extend(mp3_convert(&mut m, piece, usize::MAX, k == 0, false).0);
        }
        // a frame of silence, then frames 1.. each whole and in order
        assert!(got.len() >= 4 * 1152, "{}", got.len());
        assert!(got[..1152].iter().all(|&v| v == 0));
        for (i, f) in got[1152..].chunks(1152).enumerate() {
            assert!(f.iter().all(|&v| v == sample(i as u8 + 1, 0)), "frame {i}");
        }
        stop.store(true, Ordering::Relaxed);
        p.join().unwrap();
    }

    #[test]
    fn sizes_as_l3codecp() {
        // winref acmmp3 on Windows: 64 kbps 44.1 kHz stereo, nBlockSize 208, 16-bit stereo out
        let mut s = Fmt { tag: MP3, channels: 2, rate: 44100, avg: 8000, align: 1, bits: 0, spb: 1, block: 208, bad: false };
        let d = Fmt { tag: PCM, channels: 2, rate: 44100, avg: 176400, align: 4, bits: 16, spb: 1, block: 0, bad: false };
        for (n, want) in [(1, 4608), (208, 4608), (209, 9216), (4096, 92160), (100000, 2216448), (1000, 23040), (4388, 101376)] {
            assert_eq!(mp3_size(&s, &d, n, false), want, "{n}");
        }
        for (n, want) in [(1, 0), (4608, 208), (7608, 208), (20000, 832)] {
            assert_eq!(mp3_size(&s, &d, n, true), want, "{n}");
        }
        // 22.05 kHz mono 32 kbps, nBlockSize 104: 576-sample frames
        let m = Fmt { tag: MP3, channels: 1, rate: 22050, avg: 4000, align: 1, bits: 0, spb: 1, block: 104, bad: false };
        let md = Fmt { tag: PCM, channels: 1, rate: 22050, avg: 44100, align: 2, bits: 16, spb: 1, block: 0, bad: false };
        assert_eq!(mp3_size(&m, &md, 4096, false), 46080);
        // no nBlockSize: a frame at the average byte rate (8000 * 1152 / 44100 = 208)
        s.block = 0;
        assert_eq!(mp3_size(&s, &d, 209, false), 9216);
        assert_eq!(mp3_size(&s, &d, 4608, true), 208);
    }
}

/// The drivers acmDriverEnum lists and their details, as Windows names imaadp32.acm and
/// msadp32.acm (F&C's codec plugin compares szShortName with "MS-ADPCM").
#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod drivers {
    use super::*;
    use crate::test_guest::{call, enter as enter_mem, guest_thread};
    use proptest::prelude::*;

    fn enter() -> std::sync::MutexGuard<'static, ()> {
        let g = enter_mem();
        guest_thread();
        g
    }
    /// (result, fdwSupport, cFormatTags, wPid, short name, long name) of acmDriverDetailsA(id)
    fn details(id: u32, size: u32) -> (u32, u32, u32, u32, String, String) {
        let p = mem::heap_alloc(0x400, true);
        mem::fill(p, 0x400, 0xcc);
        w32(p, size);
        let r = call(acmDriverDetailsA, &[id, p, 0]);
        let s = |at: u32| crate::text::a2s(&crate::mem::cstr(p + at));
        (r, r32(p + 24), r32(p + 28), r32(p + 12) >> 16, s(40), s(72))
    }

    #[test]
    fn ms_adpcm_is_a_driver_of_its_own() {
        let _g = enter();
        let ids: Vec<u32> = DRIVERS.iter().map(|d| d.0).collect();
        assert_eq!(ids, [DRIVER, DRIVER_MS]);
        let (r, support, tags, pid, short, long) = details(DRIVER_MS, 0x398);
        assert_eq!((r, support, pid), (0, 1, 33));
        assert_eq!(short, "MS-ADPCM");
        assert_eq!(long, "Microsoft ADPCM");
        assert!(tags >= 3);
        let (_, _, _, pid, short, _) = details(DRIVER, 0x398);
        assert_eq!((pid, short.as_str()), (17, "Microsoft IMA ADPCM"));
        // (acmDriverOpen gives back the driver asked for; another id: the first)
        let out = mem::heap_alloc(8, true);
        assert_eq!(call(acmDriverOpen, &[out, DRIVER_MS, 0]), 0);
        assert_eq!(r32(out), DRIVER_MS);
        assert_eq!(call(acmDriverOpen, &[out, 0x1234, 0]), 0);
        assert_eq!(r32(out), DRIVER);
        // (ACM_METRIC_COUNT_DRIVERS)
        assert_eq!(call(acmMetrics, &[0, 1, out]), 0);
        assert_eq!(r32(out), 2);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        /// Any cbStruct from 4 up: the details stop there (nothing past it is written) and the
        /// fields that fit are those of the full answer.
        #[test]
        fn details_fit_cb_struct(which in 0usize..2, size in 4u32..0x398) {
            let _g = enter();
            let id = DRIVERS[which].0;
            let p = mem::heap_alloc(0x400, true);
            mem::fill(p, 0x400, 0xcc);
            w32(p, size);
            prop_assert_eq!(call(acmDriverDetailsA, &[id, p, 0]), 0);
            prop_assert_eq!(r32(p), size);
            for k in size..0x400 { prop_assert_eq!(mem::r8(p + k), 0xcc); }
            let full = mem::heap_alloc(0x400, true);
            w32(full, 0x398);
            call(acmDriverDetailsA, &[id, full, 0]);
            for k in 4..size { prop_assert_eq!(mem::r8(p + k), mem::r8(full + k)); }
        }
    }
}
