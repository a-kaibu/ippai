//! DirectShow's MPEG-1 filters (quartz.dll's): the MPEG-I Stream Splitter
//! (CLSID_MPEG1Splitter), the MPEG Video Decoder (CLSID_CMpegVideoCodec), the MPEG Audio
//! Decoder (CLSID_CMpegAudioCodec) and the DirectSound renderer (CLSID_DSoundRender), for games
//! that build their graph by hand around them. KiriKiri's krmovie (#138) adds its own source
//! (an IAsyncReader over the game's archive) and its own video renderer (RGB32 into a buffer
//! it draws), and connects source -> splitter -> video decoder -> its renderer, and splitter ->
//! audio decoder -> sound renderer.
//!
//! The stream is decoded here (crates/mpeg1). The splitter pulls it from the source pin's
//! IAsyncReader::SyncRead on a guest thread of its own, which also hands the pictures, as RGB32
//! bottom-up samples (Windows' decoder's type), to the renderer's IMemInputPin::Receive when
//! they are due (the renderer has no clock: it draws what it gets at once), and queues the
//! sound to kura's audio output. The pictures' time is the graph's (dshow.rs: its
//! IMediaSeeking, IMediaPosition and EC_COMPLETE go by this timeline).
//!
//! Each filter is one guest block: word 0 is its IBaseFilter, word k its pin k (1..=3), word 4
//! the splitter's IAMStreamSelect (krmovie asks for it after connecting). The
//! samples and the allocator the decoder hands the renderer are ours (NotifyAllocator), so no
//! CLSID_MemoryAllocator is needed.

#![allow(non_snake_case)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use waits::calls::Calls;

use crate::api::{Ctx, Handler};
use crate::dshow::com;
use crate::dshow_play::{now, Timeline, SEC};
use crate::mem::{self, ps, put32, putp, r32, rp, w16, w32, w64, wp};

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_UNEXPECTED: u32 = 0x8000_ffff;
const S_FALSE: u32 = 1;
const VFW_E_ALREADY_CONNECTED: u32 = 0x8004_0204;
const VFW_E_NOT_CONNECTED: u32 = 0x8004_0209;
const VFW_E_INVALID_DIRECTION: u32 = 0x8004_0208;
const VFW_E_TYPE_NOT_ACCEPTED: u32 = 0x8004_022a;
const VFW_E_NO_ACCEPTABLE_TYPES: u32 = 0x8004_022b;
const VFW_E_NOT_FOUND: u32 = 0x8004_0216;

pub const fn guid(d1: u32, d2: u16, d3: u16, d4: [u8; 8]) -> [u8; 16] {
    let a = d1.to_le_bytes();
    let b = d2.to_le_bytes();
    let c = d3.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d4[0], d4[1], d4[2], d4[3], d4[4], d4[5], d4[6], d4[7]]
}
const fn ds(d1: u32, d2: u16) -> [u8; 16] {
    // {xxxxxxxx-xxxx-11ce-...} of quartz's media types and formats
    guid(d1, d2, 0x11ce, [0x9f, 0x53, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70])
}
const fn fourcc(d1: u32) -> [u8; 16] {
    guid(d1, 0, 0x10, [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71])
}
const fn iface(d1: u32) -> [u8; 16] {
    // {56a86xxx-0ad4-11ce-b03a-0020af0ba770}
    guid(d1, 0x0ad4, 0x11ce, [0xb0, 0x3a, 0x00, 0x20, 0xaf, 0x0b, 0xa7, 0x70])
}

pub const CLSID_MPEG1_SPLITTER: [u8; 16] = guid(0x3364_75d0, 0x942a, 0x11ce, [0xa8, 0x70, 0x00, 0xaa, 0x00, 0x2f, 0xea, 0xb5]);
pub const CLSID_MPEG_VIDEO_CODEC: [u8; 16] = guid(0xfeb5_0740, 0x7bef, 0x11ce, [0x9b, 0xd9, 0x00, 0x00, 0xe2, 0x02, 0x59, 0x9c]);
pub const CLSID_MPEG_AUDIO_CODEC: [u8; 16] = guid(0x4a22_86e0, 0x7bef, 0x11ce, [0x9b, 0xd9, 0x00, 0x00, 0xe2, 0x02, 0x59, 0x9c]);
pub const CLSID_DSOUND_RENDER: [u8; 16] = guid(0x7937_6820, 0x07d0, 0x11cf, [0xa2, 0x4d, 0x00, 0x20, 0xaf, 0xd7, 0x97, 0x67]);

const MEDIATYPE_VIDEO: [u8; 16] = fourcc(0x7364_6976);
const MEDIATYPE_AUDIO: [u8; 16] = fourcc(0x7364_7561);
const MEDIATYPE_STREAM: [u8; 16] = ds(0xe436_eb83, 0x524f);
const SUBTYPE_RGB32: [u8; 16] = ds(0xe436_eb7e, 0x524f);
const SUBTYPE_MPEG1_PAYLOAD: [u8; 16] = ds(0xe436_eb81, 0x524f);
const SUBTYPE_MPEG1_AUDIO_PAYLOAD: [u8; 16] = fourcc(0x50);
const SUBTYPE_PCM: [u8; 16] = fourcc(1);
const FORMAT_VIDEO_INFO: [u8; 16] = guid(0x0558_9f80, 0xc356, 0x11ce, [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a]);
const FORMAT_WAVE_FORMAT_EX: [u8; 16] = guid(0x0558_9f81, 0xc356, 0x11ce, [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a]);
const FORMAT_MPEG_VIDEO: [u8; 16] = guid(0x0558_9f82, 0xc356, 0x11ce, [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a]);

const IID_IUNKNOWN: [u8; 16] = guid(0, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_IPERSIST: [u8; 16] = guid(0x10c, 0, 0, [0xc0, 0, 0, 0, 0, 0, 0, 0x46]);
const IID_IBASEFILTER: [u8; 16] = iface(0x56a8_6895);
const IID_IMEDIAFILTER: [u8; 16] = iface(0x56a8_6899);
const IID_IPIN: [u8; 16] = iface(0x56a8_6891);
const IID_IENUMPINS: [u8; 16] = iface(0x56a8_6892);
const IID_IMEMINPUTPIN: [u8; 16] = iface(0x56a8_689d);
const IID_IMEMALLOCATOR: [u8; 16] = iface(0x56a8_689c);
const IID_IMEDIASAMPLE: [u8; 16] = iface(0x56a8_689a);
const IID_IASYNCREADER: [u8; 16] = iface(0x56a8_68aa);
const IID_IAMSTREAMSELECT: [u8; 16] = guid(0xc196_0960, 0x17f5, 0x11d1, [0xab, 0xe1, 0x00, 0xa0, 0xc9, 0x05, 0xf3, 0x75]);
/// the splitter's IAMStreamSelect: word SS of its block
const SS: u32 = 4;

fn is(p: u32, g: &[u8; 16]) -> bool {
    p != 0 && mem::slice(p, 16) == g
}
fn put_guid(p: u32, g: &[u8; 16]) {
    mem::slice_mut(p, 16).copy_from_slice(g);
}

// ---------------------------------------------------------------------------------------------
// the filters

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Split,
    VDec,
    ADec,
    Sound,
}

#[derive(Clone, Default)]
struct Pin {
    out: bool,
    name: &'static str,
    /// it is there (the splitter's outputs once its input knows the stream)
    on: bool,
    /// the pin it is connected to (a reference held)
    peer: u32,
    /// the connection's AM_MEDIA_TYPE (ours, guest memory)
    mt: u32,
    /// a guest input pin's IMemInputPin (a reference held)
    mem: u32,
}

struct Filter {
    kind: Kind,
    refs: u32,
    /// the graph it joined (not held: the graph holds the filter)
    graph: u32,
    name: Vec<u16>,
    /// 0 stopped, 1 paused, 2 running
    state: u32,
    pins: [Pin; 3],
}

static FILTERS: Mutex<Option<HashMap<u32, Filter>>> = Mutex::new(None);

fn fwith<R>(b: u32, f: impl FnOnce(&mut Filter) -> R) -> Option<R> {
    FILTERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

/// The filter and pin number of an interface pointer of ours (k 0: the filter).
fn ours(p: u32) -> Option<(u32, usize)> {
    let g = FILTERS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.as_ref()?;
    (0..=SS).find_map(|k| p.checked_sub(ps() * k).filter(|b| m.contains_key(b)).map(|b| (b, k as usize)))
}
fn pin_of(p: u32) -> Option<(u32, usize)> {
    ours(p).filter(|x| (1..=3).contains(&x.1))
}
fn kind(b: u32) -> Option<Kind> {
    fwith(b, |f| f.kind)
}
fn pin(b: u32, k: usize) -> Pin {
    fwith(b, |f| f.pins[k - 1].clone()).unwrap_or_default()
}

/// The filter of kind `k`, its IBaseFilter (or one of its interfaces `iid` asks for).
pub fn create_instance(clsid: u32, iid: u32) -> Option<u32> {
    let k = [(CLSID_MPEG1_SPLITTER, Kind::Split), (CLSID_MPEG_VIDEO_CODEC, Kind::VDec), (CLSID_MPEG_AUDIO_CODEC, Kind::ADec), (CLSID_DSOUND_RENDER, Kind::Sound)]
        .into_iter()
        .find(|(c, _)| is(clsid, c))?
        .1;
    if !filter_iid(iid) {
        return Some(0);
    }
    let b = mem::heap_alloc((SS + 1) * 8, true);
    let v = vtables();
    wp(b, v.filter);
    for i in 1..4 {
        wp(b + ps() * i, v.pin);
    }
    wp(b + ps() * SS, v.stream_select);
    let p = |out: bool, name: &'static str, on: bool| Pin { out, name, on, ..Pin::default() };
    let pins = match k {
        Kind::Split => [p(false, "Input", true), p(true, "Video", false), p(true, "Audio", false)],
        Kind::VDec | Kind::ADec => [p(false, "Input", true), p(true, "Output", true), Pin::default()],
        Kind::Sound => [p(false, "Audio Input pin (rendered)", true), Pin::default(), Pin::default()],
    };
    FILTERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Filter { kind: k, refs: 1, graph: 0, name: Vec::new(), state: 0, pins });
    Some(b)
}

fn filter_iid(iid: u32) -> bool {
    [IID_IUNKNOWN, IID_IPERSIST, IID_IMEDIAFILTER, IID_IBASEFILTER].iter().any(|g| is(iid, g))
}

fn add_ref(b: u32) -> u32 {
    fwith(b, |f| {
        f.refs += 1;
        f.refs
    })
    .unwrap_or(1)
}
fn release(c: &mut Ctx, b: u32) -> u32 {
    let n = fwith(b, |f| {
        f.refs = f.refs.saturating_sub(1);
        f.refs
    })
    .unwrap_or(0);
    if n == 0 {
        // (its connections go, and the splitter's thread with them)
        let pins = fwith(b, |f| std::mem::take(&mut f.pins)).unwrap_or_default();
        for p in pins {
            for x in [p.peer, p.mem] {
                if x != 0 && pin_of(x).is_none() {
                    com(c, x, 2, &[]);
                }
            }
        }
        if let Some(r) = PIPES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b) {
            r.dead = true;
        }
        FILTERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
    }
    n
}

fn f_qi(c: &mut Ctx) -> u32 {
    let (b, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if filter_iid(iid) {
        add_ref(b);
        putp(out, b);
        return 0;
    }
    if is(iid, &IID_IAMSTREAMSELECT) && kind(b) == Some(Kind::Split) {
        add_ref(b);
        putp(out, b + ps() * SS);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn f_add_ref(c: &mut Ctx) -> u32 {
    add_ref(c.a(0))
}
fn f_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    release(c, b)
}
fn f_class(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let g = match kind(c.a(0)) {
        Some(Kind::Split) => CLSID_MPEG1_SPLITTER,
        Some(Kind::VDec) => CLSID_MPEG_VIDEO_CODEC,
        Some(Kind::ADec) => CLSID_MPEG_AUDIO_CODEC,
        _ => CLSID_DSOUND_RENDER,
    };
    put_guid(out, &g);
    0
}
fn set_state(c: &mut Ctx, s: u32) -> u32 {
    let b = c.a(0);
    let Some(k) = fwith(b, |f| {
        f.state = s;
        f.kind
    }) else {
        return E_UNEXPECTED;
    };
    if k == Kind::Split {
        streaming(b, s);
    }
    0
}
fn f_stop(c: &mut Ctx) -> u32 {
    set_state(c, 0)
}
fn f_pause(c: &mut Ctx) -> u32 {
    set_state(c, 1)
}
fn f_run(c: &mut Ctx) -> u32 {
    set_state(c, 2)
}
fn f_get_state(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    if out == 0 {
        return E_POINTER;
    }
    put32(out, fwith(c.a(0), |f| f.state).unwrap_or(0));
    0
}
fn ok(_: &mut Ctx) -> u32 {
    0
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn get_null(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), 0);
    0
}
/// The pins there are, in order.
fn pins_of(b: u32) -> Vec<u32> {
    fwith(b, |f| f.pins.iter().enumerate().filter(|(_, p)| p.on).map(|(i, _)| b + ps() * (i as u32 + 1)).collect()).unwrap_or_default()
}
fn f_enum_pins(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let pins = pins_of(c.a(0));
    putp(out, enum_new(pins));
    0
}
fn f_find_pin(c: &mut Ctx) -> u32 {
    let (b, id, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let want = if id == 0 { Vec::new() } else { mem::wstr(id) };
    let found = fwith(b, |f| f.pins.iter().position(|p| p.on && p.name.encode_utf16().eq(want.iter().copied()))).flatten();
    match found {
        Some(i) => {
            add_ref(b);
            putp(out, b + ps() * (i as u32 + 1));
            0
        }
        None => VFW_E_NOT_FOUND,
    }
}
/// FILTER_INFO { WCHAR achName[128]; IFilterGraph *pGraph }
fn f_info(c: &mut Ctx) -> u32 {
    let (b, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let (name, graph) = fwith(b, |f| (f.name.clone(), f.graph)).unwrap_or_default();
    mem::fill(out, 256 + ps(), 0);
    for (i, &w) in name.iter().take(127).enumerate() {
        w16(out + 2 * i as u32, w);
    }
    if graph != 0 {
        com(c, graph, 1, &[]);
    }
    wp(out + 256, graph);
    0
}
fn f_join(c: &mut Ctx) -> u32 {
    let (b, g, name) = (c.a(0), c.a(1), c.a(2));
    let n = if name == 0 { Vec::new() } else { mem::wstr(name) };
    fwith(b, |f| {
        f.graph = g;
        f.name = n;
    });
    0
}

// ---------------------------------------------------------------------------------------------
// pins

fn p_qi(c: &mut Ctx) -> u32 {
    let (p, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is(iid, &IID_IUNKNOWN) || is(iid, &IID_IPIN) {
        if let Some((b, _)) = pin_of(p) {
            add_ref(b);
        }
        putp(out, p);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn p_add_ref(c: &mut Ctx) -> u32 {
    pin_of(c.a(0)).map_or(1, |(b, _)| add_ref(b))
}
fn p_release(c: &mut Ctx) -> u32 {
    match pin_of(c.a(0)) {
        Some((b, _)) => release(c, b),
        None => 0,
    }
}

/// AM_MEDIA_TYPE: major, sub, bFixedSizeSamples, bTemporalCompression, lSampleSize,
/// formattype, pUnk, cbFormat, pbFormat (x64: pUnk on 64)
fn mt_size() -> u32 {
    if mem::x64() { 88 } else { 72 }
}
fn mt_fmt_at() -> (u32, u32) {
    if mem::x64() { (72, 80) } else { (64, 68) }
}
fn new_mt(major: &[u8; 16], sub: &[u8; 16], fixed: bool, sample: u32, fmt_type: &[u8; 16], fmt: &[u8]) -> u32 {
    let p = mem::heap_alloc(mt_size(), true);
    put_guid(p, major);
    put_guid(p + 16, sub);
    w32(p + 32, fixed as u32);
    w32(p + 36, !fixed as u32);
    w32(p + 40, sample);
    put_guid(p + 44, fmt_type);
    let (cb, pb) = mt_fmt_at();
    let f = mem::heap_alloc(fmt.len().max(1) as u32, true);
    mem::slice_mut(f, fmt.len() as u32).copy_from_slice(fmt);
    w32(p + cb, fmt.len() as u32);
    wp(p + pb, f);
    p
}
/// A copy of media type `src` into `dst` (its format block a new CoTaskMemAlloc one).
fn copy_mt(src: u32, dst: u32) {
    let n = mt_size();
    let bytes = mem::slice(src, n).to_vec();
    mem::slice_mut(dst, n).copy_from_slice(&bytes);
    let (cb, pb) = mt_fmt_at();
    let len = r32(src + cb);
    let f = mem::heap_alloc(len.max(1), false);
    let from = rp(src + pb);
    if from != 0 && len != 0 {
        let b = mem::slice(from, len).to_vec();
        mem::slice_mut(f, len).copy_from_slice(&b);
    }
    wp(dst + pb, f);
}

/// VIDEOINFOHEADER { rcSource, rcTarget, dwBitRate, dwBitErrorRate, AvgTimePerFrame,
/// BITMAPINFOHEADER }
fn video_info(w: u32, h: u32, bits: u16, compression: u32, rate: f64) -> Vec<u8> {
    let mut v = vec![0u8; 88];
    let put = |v: &mut Vec<u8>, at: usize, b: &[u8]| v[at..at + b.len()].copy_from_slice(b);
    for (i, x) in [0, 0, w, h, 0, 0, w, h].into_iter().enumerate() {
        put(&mut v, 4 * i, &x.to_le_bytes());
    }
    let frame = if rate > 0.0 { (SEC as f64 / rate) as u64 } else { 0 };
    put(&mut v, 32, &((w as u64 * h as u64 * bits as u64 * rate.max(0.0) as u64) as u32).to_le_bytes());
    put(&mut v, 40, &frame.to_le_bytes());
    put(&mut v, 48, &40u32.to_le_bytes());
    put(&mut v, 52, &w.to_le_bytes());
    put(&mut v, 56, &h.to_le_bytes());
    put(&mut v, 60, &1u16.to_le_bytes());
    put(&mut v, 62, &bits.to_le_bytes());
    put(&mut v, 64, &compression.to_le_bytes());
    put(&mut v, 68, &(w * h * bits as u32 / 8).to_le_bytes());
    v
}
/// WAVEFORMATEX (cbSize 0 for PCM)
fn wave_format(tag: u16, channels: u16, rate: u32, bits: u16, extra: &[u8]) -> Vec<u8> {
    let align = channels * bits.max(8) / 8;
    let mut v = Vec::new();
    v.extend(tag.to_le_bytes());
    v.extend(channels.to_le_bytes());
    v.extend(rate.to_le_bytes());
    v.extend((rate * align as u32).to_le_bytes());
    v.extend(align.max(1).to_le_bytes());
    v.extend(bits.to_le_bytes());
    v.extend((extra.len() as u16).to_le_bytes());
    v.extend_from_slice(extra);
    v
}

/// The media type output pin `k` of filter `b` gives (once its input knows the stream).
fn out_type(b: u32, k: usize) -> Option<u32> {
    let (kind, info) = (kind(b)?, info_of(b)?);
    let (w, h, r) = (info.width as u32, info.height as u32, info.rate);
    Some(match (kind, k) {
        (Kind::Split, 2) => {
            let mut f = video_info(w, h, 0, 0, r);
            f.extend([0u8; 12]);
            new_mt(&MEDIATYPE_VIDEO, &SUBTYPE_MPEG1_PAYLOAD, false, 0, &FORMAT_MPEG_VIDEO, &f)
        }
        (Kind::Split, 3) => {
            // MPEG1WAVEFORMAT: fwHeadLayer ACM_MPEG_LAYER2, the rest unknown
            let mut extra = [0u8; 22];
            extra[0] = 2;
            new_mt(&MEDIATYPE_AUDIO, &SUBTYPE_MPEG1_AUDIO_PAYLOAD, false, 0, &FORMAT_WAVE_FORMAT_EX, &wave_format(0x50, 2, info.audio_rate, 0, &extra))
        }
        (Kind::VDec, 2) => new_mt(&MEDIATYPE_VIDEO, &SUBTYPE_RGB32, true, w * h * 4, &FORMAT_VIDEO_INFO, &video_info(w, h, 32, 0, r)),
        (Kind::ADec, 2) => new_mt(&MEDIATYPE_AUDIO, &SUBTYPE_PCM, true, 4, &FORMAT_WAVE_FORMAT_EX, &wave_format(1, 2, info.audio_rate, 16, &[])),
        _ => return None,
    })
}

/// The stream's facts for any filter of the chain: the splitter's, upstream of it.
fn info_of(b: u32) -> Option<mpeg1::Info> {
    let s = splitter_of(b)?;
    PIPES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&s).map(|p| p.info)
}
/// The splitter feeding filter `b` (itself, for one).
fn splitter_of(b: u32) -> Option<u32> {
    match kind(b)? {
        Kind::Split => Some(b),
        _ => {
            let up = pin(b, 1).peer;
            let (ub, _) = pin_of(up)?;
            if ub == b { None } else { splitter_of(ub) }
        }
    }
}

/// Our output pin `out` to our input pin `inp`: the splitter's video to a video decoder, its
/// sound to a sound decoder, a sound decoder to the sound renderer.
fn connect_ours(out: (u32, usize), inp: (u32, usize)) -> u32 {
    let ok = match (kind(out.0), out.1, kind(inp.0), inp.1) {
        (Some(Kind::Split), 2, Some(Kind::VDec), 1) | (Some(Kind::Split), 3, Some(Kind::ADec), 1) | (Some(Kind::ADec), 2, Some(Kind::Sound), 1) => true,
        _ => false,
    };
    if !ok {
        return VFW_E_NO_ACCEPTABLE_TYPES;
    }
    if pin(inp.0, inp.1).peer != 0 {
        return VFW_E_ALREADY_CONNECTED;
    }
    let Some(mt) = out_type(out.0, out.1) else { return VFW_E_NO_ACCEPTABLE_TYPES };
    let (op, ip) = (out.0 + ps() * out.1 as u32, inp.0 + ps() * inp.1 as u32);
    fwith(out.0, |f| {
        f.pins[out.1 - 1].peer = ip;
        f.pins[out.1 - 1].mt = mt;
    });
    fwith(inp.0, |f| {
        f.pins[inp.1 - 1].peer = op;
        f.pins[inp.1 - 1].mt = mt;
    });
    // (each holds the other)
    add_ref(out.0);
    add_ref(inp.0);
    0
}

/// IPin::Connect(receive pin, media type): only outputs connect.
fn p_connect(c: &mut Ctx) -> u32 {
    let (p, recv) = (c.a(0), c.a(1));
    let Some((b, k)) = pin_of(p) else { return E_UNEXPECTED };
    if recv == 0 {
        return E_POINTER;
    }
    let me = pin(b, k);
    if !me.out {
        return VFW_E_INVALID_DIRECTION;
    }
    if me.peer != 0 {
        return VFW_E_ALREADY_CONNECTED;
    }
    if let Some(inp) = pin_of(recv) {
        return connect_ours((b, k), inp);
    }
    // a guest input pin: the video decoder's pictures to a game's renderer
    if kind(b) != Some(Kind::VDec) {
        return VFW_E_NO_ACCEPTABLE_TYPES;
    }
    let dir = mem::heap_alloc(4, true);
    let r = com(c, recv, 9, &[dir]);
    let d = r32(dir);
    mem::heap_free(dir);
    if r != 0 || d != 0 {
        return VFW_E_INVALID_DIRECTION;
    }
    let Some(mt) = out_type(b, k) else { return VFW_E_NOT_CONNECTED };
    let r = com(c, recv, 4, &[p, mt]);
    if r != 0 {
        return if r == VFW_E_TYPE_NOT_ACCEPTED { VFW_E_NO_ACCEPTABLE_TYPES } else { r };
    }
    let out = mem::heap_alloc(8 + 16, true);
    put_guid(out + 8, &IID_IMEMINPUTPIN);
    let r = com(c, recv, 0, &[out + 8, out]);
    let mem_pin = rp(out);
    mem::heap_free(out);
    if r != 0 || mem_pin == 0 {
        com(c, recv, 5, &[]);
        return E_NOINTERFACE;
    }
    com(c, recv, 1, &[]);
    fwith(b, |f| {
        f.pins[k - 1].peer = recv;
        f.pins[k - 1].mt = mt;
        f.pins[k - 1].mem = mem_pin;
    });
    // our allocator (not read-only)
    let a = allocator(b);
    com(c, mem_pin, 4, &[a, 0]);
    0
}

/// IPin::ReceiveConnection(connector, media type): the splitter's input from a game's source
/// (its IAsyncReader), or one of ours.
fn p_receive_connection(c: &mut Ctx) -> u32 {
    let (p, from, mt) = (c.a(0), c.a(1), c.a(2));
    let Some((b, k)) = pin_of(p) else { return E_UNEXPECTED };
    if from == 0 {
        return E_POINTER;
    }
    let me = pin(b, k);
    if me.out {
        return VFW_E_INVALID_DIRECTION;
    }
    if me.peer != 0 {
        return VFW_E_ALREADY_CONNECTED;
    }
    if let Some(o) = pin_of(from) {
        return connect_ours(o, (b, k));
    }
    if kind(b) != Some(Kind::Split) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    if mt != 0 && !is(mt, &MEDIATYPE_STREAM) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    let out = mem::heap_alloc(8, true);
    let iid = mem::heap_alloc(16, false);
    put_guid(iid, &IID_IASYNCREADER);
    let r = com(c, from, 0, &[iid, out]);
    let reader = rp(out);
    mem::heap_free(out);
    mem::heap_free(iid);
    if r != 0 || reader == 0 {
        return E_NOINTERFACE;
    }
    let Some((len, info)) = probe(c, reader) else {
        com(c, reader, 2, &[]);
        return VFW_E_TYPE_NOT_ACCEPTED;
    };
    com(c, from, 1, &[]);
    let copy = if mt != 0 {
        let m = mem::heap_alloc(mt_size(), true);
        copy_mt(mt, m);
        m
    } else {
        0
    };
    fwith(b, |f| {
        f.pins[0].peer = from;
        f.pins[0].mt = copy;
        f.pins[1].on = info.width > 0;
        f.pins[2].on = info.audio_rate > 0;
    });
    let mut tl = Timeline::new(info.duration_100ns());
    tl.stop = tl.duration;
    // (connected again: the thread there was goes on with the new stream)
    let mut pipes = PIPES.lock().unwrap_or_else(|e| e.into_inner());
    let m = pipes.get_or_insert_with(HashMap::new);
    let (thread, gen, calls) = m.get(&b).map_or((false, 0, None), |p| (p.thread, p.gen + 1, Some(p.calls.clone())));
    let calls = calls.unwrap_or_default();
    // (connected again while it runs: the calls go on)
    if fwith(b, |f| f.state).unwrap_or(0) != 0 {
        calls.run();
    }
    m.insert(b, Pipe { reader, len, info, tl, thread, dead: false, ended: false, gen, calls });
    drop(pipes);
    eprintln!("sake: MPEG-1 splitter: {}x{} {:.3} fps, sound {} Hz, {:.2} s, {} bytes", info.width, info.height, info.rate, info.audio_rate, info.duration, len);
    0
}

trait Dur {
    fn duration_100ns(&self) -> u64;
}
impl Dur for mpeg1::Info {
    fn duration_100ns(&self) -> u64 {
        (self.duration.max(0.0) * SEC as f64) as u64
    }
}

/// The stream's length (IAsyncReader::Length) and its facts, from its first and last bytes.
fn probe(c: &mut Ctx, reader: u32) -> Option<(u64, mpeg1::Info)> {
    let lens = mem::heap_alloc(16, true);
    let r = com(c, reader, 8, &[lens, lens + 8]);
    let len = mem::r64(lens);
    mem::heap_free(lens);
    if r & 0x8000_0000 != 0 || len == 0 {
        return None;
    }
    // (its end further back while it holds no picture: a stream may end in padding, #230)
    mpeg1::probe_stream(len, 256 << 10, |pos, n| read_at(c, reader, pos, n as u32)).map(|i| (len, i))
}

/// `n` bytes at `pos` of the stream (IAsyncReader::SyncRead into a buffer of ours).
fn read_at(c: &mut Ctx, reader: u32, pos: u64, n: u32) -> Option<Vec<u8>> {
    let buf = mem::heap_alloc(n.max(1), false);
    let args: Vec<u32> = if mem::x64() { vec![pos as u32, n, buf] } else { vec![pos as u32, (pos >> 32) as u32, n, buf] };
    let r = com(c, reader, 7, &args);
    let v = (r & 0x8000_0000 == 0).then(|| mem::slice(buf, n).to_vec());
    mem::heap_free(buf);
    v
}

fn p_disconnect(c: &mut Ctx) -> u32 {
    let Some((b, k)) = pin_of(c.a(0)) else { return E_UNEXPECTED };
    let me = pin(b, k);
    if me.peer == 0 {
        return S_FALSE;
    }
    fwith(b, |f| {
        let p = &mut f.pins[k - 1];
        p.peer = 0;
        p.mem = 0;
    });
    // (the splitter's source gone: no more reads from it)
    let reader = if kind(b) == Some(Kind::Split) && k == 1 { pwith(b, |p| std::mem::take(&mut p.reader)).unwrap_or(0) } else { 0 };
    if reader != 0 {
        // (a read in flight comes back first)
        stop_calls(b, false);
        com(c, reader, 2, &[]);
    }
    match pin_of(me.peer) {
        // (one of ours lets this one go too)
        Some((pb, pk)) => {
            fwith(pb, |f| {
                f.pins[pk - 1].peer = 0;
            });
            release(c, pb);
            release(c, b);
        }
        None => {
            for x in [me.mem, me.peer] {
                if x != 0 {
                    com(c, x, 2, &[]);
                }
            }
        }
    }
    0
}
fn p_connected_to(c: &mut Ctx) -> u32 {
    let (p, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let peer = pin_of(p).map_or(0, |(b, k)| pin(b, k).peer);
    putp(out, peer);
    if peer == 0 {
        return VFW_E_NOT_CONNECTED;
    }
    match pin_of(peer) {
        Some((pb, _)) => {
            add_ref(pb);
        }
        None => {
            com(c, peer, 1, &[]);
        }
    }
    0
}
fn p_connection_type(c: &mut Ctx) -> u32 {
    let (p, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let me = pin_of(p).map(|(b, k)| pin(b, k)).unwrap_or_default();
    if me.peer == 0 || me.mt == 0 {
        mem::fill(out, mt_size(), 0);
        return VFW_E_NOT_CONNECTED;
    }
    copy_mt(me.mt, out);
    0
}
/// PIN_INFO { IBaseFilter *pFilter; PIN_DIRECTION dir; WCHAR achName[128] }
fn p_info(c: &mut Ctx) -> u32 {
    let (p, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let Some((b, k)) = pin_of(p) else { return E_UNEXPECTED };
    let me = pin(b, k);
    add_ref(b);
    mem::fill(out, ps() + 4 + 256, 0);
    wp(out, b);
    w32(out + ps(), me.out as u32);
    for (i, w) in me.name.encode_utf16().take(127).enumerate() {
        w16(out + ps() + 4 + 2 * i as u32, w);
    }
    0
}
fn p_direction(c: &mut Ctx) -> u32 {
    let (p, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    put32(out, pin_of(p).is_some_and(|(b, k)| pin(b, k).out) as u32);
    0
}
fn p_id(c: &mut Ctx) -> u32 {
    let (p, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let name = pin_of(p).map_or("", |(b, k)| pin(b, k).name);
    let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
    let s = mem::heap_alloc(2 * w.len() as u32, false);
    for (i, v) in w.iter().enumerate() {
        w16(s + 2 * i as u32, *v);
    }
    putp(out, s);
    0
}
/// QueryAccept: the type it is connected with (S_FALSE: another)
fn p_accept(c: &mut Ctx) -> u32 {
    let (p, mt) = (c.a(0), c.a(1));
    if mt == 0 {
        return E_POINTER;
    }
    let me = pin_of(p).map(|(b, k)| pin(b, k)).unwrap_or_default();
    if me.mt != 0 && mem::slice(mt, 32) == mem::slice(me.mt, 32) { 0 } else { S_FALSE }
}

// ---------------------------------------------------------------------------------------------
// the splitter's IAMStreamSelect (krmovie asks for it): its streams, the video in group 0 and
// the sound in group 1, each enabled

/// The splitter's output pins there are: (pin number, group)
fn streams(b: u32) -> Vec<(usize, u32)> {
    [(2, 0), (3, 1)].into_iter().filter(|&(k, _)| fwith(b, |f| f.pins[k - 1].on).unwrap_or(false)).collect()
}
fn ss_qi(c: &mut Ctx) -> u32 {
    let b = c.a(0) - ps() * SS;
    let (iid, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is(iid, &IID_IUNKNOWN) || is(iid, &IID_IAMSTREAMSELECT) {
        add_ref(b);
        putp(out, c.a(0));
        return 0;
    }
    if filter_iid(iid) {
        add_ref(b);
        putp(out, b);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn ss_add_ref(c: &mut Ctx) -> u32 {
    add_ref(c.a(0) - ps() * SS)
}
fn ss_release(c: &mut Ctx) -> u32 {
    let b = c.a(0) - ps() * SS;
    release(c, b)
}
fn ss_count(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), streams(c.a(0) - ps() * SS).len() as u32);
    0
}
/// Info(index, &mt, &flags, &lcid, &group, &name, &object, &unk)
fn ss_info(c: &mut Ctx) -> u32 {
    let b = c.a(0) - ps() * SS;
    let Some(&(k, group)) = streams(b).get(c.a(1) as usize) else { return S_FALSE };
    let (mt, flags, lcid, grp, name, obj, unk) = (c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7), c.a(8));
    if mt != 0 {
        let t = out_type(b, k).unwrap_or(0);
        let m = mem::heap_alloc(mt_size(), true);
        if t != 0 {
            copy_mt(t, m);
        }
        putp(mt, m);
    }
    put32(flags, 1); // AMSTREAMSELECTINFO_ENABLED
    put32(lcid, 0);
    put32(grp, group);
    if name != 0 {
        let n = pin(b, k).name;
        let w: Vec<u16> = n.encode_utf16().chain([0]).collect();
        let p = mem::heap_alloc(2 * w.len() as u32, false);
        for (i, v) in w.iter().enumerate() {
            w16(p + 2 * i as u32, *v);
        }
        putp(name, p);
    }
    putp(obj, 0);
    putp(unk, 0);
    0
}
/// Enable(index, flags): the streams there are stay enabled
fn ss_enable(c: &mut Ctx) -> u32 {
    let b = c.a(0) - ps() * SS;
    if (c.a(1) as usize) < streams(b).len() { 0 } else { 0x8007_0057 }
}

// ---------------------------------------------------------------------------------------------
// IEnumPins

static ENUMS: Mutex<Option<HashMap<u32, (Vec<u32>, usize, u32)>>> = Mutex::new(None);

fn ewith<R>(e: u32, f: impl FnOnce(&mut (Vec<u32>, usize, u32)) -> R) -> Option<R> {
    ENUMS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).get_mut(&e).map(f)
}
fn enum_new(pins: Vec<u32>) -> u32 {
    let e = mem::heap_alloc(8, true);
    wp(e, vtables().enum_pins);
    ENUMS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).insert(e, (pins, 0, 1));
    e
}
fn e_qi(c: &mut Ctx) -> u32 {
    let (e, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is(iid, &IID_IUNKNOWN) || is(iid, &IID_IENUMPINS) {
        ewith(e, |x| x.2 += 1);
        putp(out, e);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn e_add_ref(c: &mut Ctx) -> u32 {
    ewith(c.a(0), |x| {
        x.2 += 1;
        x.2
    })
    .unwrap_or(1)
}
fn e_release(c: &mut Ctx) -> u32 {
    let e = c.a(0);
    let n = ewith(e, |x| {
        x.2 = x.2.saturating_sub(1);
        x.2
    })
    .unwrap_or(0);
    if n == 0 {
        ENUMS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).remove(&e);
        mem::heap_free(e);
    }
    n
}
/// Next(n, pins, fetched)
fn e_next(c: &mut Ctx) -> u32 {
    let (e, n, out, got) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 {
        return E_POINTER;
    }
    let take = ewith(e, |x| {
        let t: Vec<u32> = x.0.iter().skip(x.1).take(n as usize).copied().collect();
        x.1 += t.len();
        t
    })
    .unwrap_or_default();
    for (i, &p) in take.iter().enumerate() {
        if let Some((b, _)) = pin_of(p) {
            add_ref(b);
        }
        wp(out + ps() * i as u32, p);
    }
    if got != 0 {
        put32(got, take.len() as u32);
    }
    if take.len() == n as usize { 0 } else { S_FALSE }
}
fn e_skip(c: &mut Ctx) -> u32 {
    let n = c.a(1) as usize;
    ewith(c.a(0), |x| {
        let left = x.0.len() - x.1.min(x.0.len());
        x.1 = (x.1 + n).min(x.0.len());
        if left >= n { 0 } else { S_FALSE }
    })
    .unwrap_or(S_FALSE)
}
fn e_reset(c: &mut Ctx) -> u32 {
    ewith(c.a(0), |x| x.1 = 0);
    0
}
fn e_clone(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let Some((pins, at, _)) = ewith(c.a(0), |x| x.clone()) else { return E_UNEXPECTED };
    let e = enum_new(pins);
    ewith(e, |x| x.1 = at);
    putp(out, e);
    0
}

// ---------------------------------------------------------------------------------------------
// the samples the video decoder hands the renderer: IMemAllocator and IMediaSample

#[derive(Default)]
struct Sample {
    refs: u32,
    buf: u32,
    size: u32,
    len: u32,
    time: Option<(u64, u64)>,
    sync: bool,
    disc: bool,
}
static SAMPLES: Mutex<Option<HashMap<u32, Sample>>> = Mutex::new(None);
/// each decoder's allocator, and the samples it made
static ALLOCS: Mutex<Option<HashMap<u32, (u32, Vec<u32>)>>> = Mutex::new(None);

fn swith<R>(s: u32, f: impl FnOnce(&mut Sample) -> R) -> Option<R> {
    SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&s).map(f)
}
fn allocator(dec: u32) -> u32 {
    let mut g = ALLOCS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new)
        .entry(dec)
        .or_insert_with(|| {
            let a = mem::heap_alloc(8, true);
            wp(a, vtables().allocator);
            (a, Vec::new())
        })
        .0
}
/// A sample of `size` bytes no one else holds (one more made when the renderer keeps them all).
fn free_sample(dec: u32, size: u32) -> u32 {
    allocator(dec);
    let mine = ALLOCS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&dec).map(|a| a.1.clone()).unwrap_or_default();
    for s in mine {
        if swith(s, |x| x.refs == 1 && x.size >= size) == Some(true) {
            return s;
        }
    }
    let s = mem::heap_alloc(8, true);
    wp(s, vtables().sample);
    let buf = mem::heap_alloc(size.max(1), true);
    SAMPLES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(s, Sample { refs: 1, buf, size, ..Sample::default() });
    ALLOCS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&dec).unwrap().1.push(s);
    s
}

fn a_qi(c: &mut Ctx) -> u32 {
    let (a, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is(iid, &IID_IUNKNOWN) || is(iid, &IID_IMEMALLOCATOR) {
        putp(out, a);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn one(_: &mut Ctx) -> u32 {
    1
}
/// ALLOCATOR_PROPERTIES { cBuffers, cbBuffer, cbAlign, cbPrefix }
fn a_set_props(c: &mut Ctx) -> u32 {
    let (req, act) = (c.a(1), c.a(2));
    if req == 0 || act == 0 {
        return E_POINTER;
    }
    for i in 0..4 {
        w32(act + 4 * i, r32(req + 4 * i));
    }
    0
}
fn a_get_props(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let size = ALLOCS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).values().find(|a| a.0 == c.a(0)).and_then(|a| a.1.first().copied()).and_then(|s| swith(s, |x| x.size)).unwrap_or(0);
    for (i, v) in [1, size, 1, 0].into_iter().enumerate() {
        w32(out + 4 * i as u32, v);
    }
    0
}
/// GetBuffer(&sample, start, stop, flags): one of ours, held for the caller
fn a_get_buffer(c: &mut Ctx) -> u32 {
    let (a, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let dec = ALLOCS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).iter().find(|(_, x)| x.0 == a).map(|(&d, _)| d);
    let Some(dec) = dec else { return E_UNEXPECTED };
    let size = info_of(dec).map_or(4, |i| (i.width * i.height * 4) as u32);
    let s = free_sample(dec, size);
    swith(s, |x| x.refs += 1);
    putp(out, s);
    0
}

fn s_qi(c: &mut Ctx) -> u32 {
    let (s, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if is(iid, &IID_IUNKNOWN) || is(iid, &IID_IMEDIASAMPLE) {
        swith(s, |x| x.refs += 1);
        putp(out, s);
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn s_add_ref(c: &mut Ctx) -> u32 {
    swith(c.a(0), |x| {
        x.refs += 1;
        x.refs
    })
    .unwrap_or(1)
}
fn s_release(c: &mut Ctx) -> u32 {
    // (ours keep one reference: the pool's)
    swith(c.a(0), |x| {
        x.refs = x.refs.saturating_sub(1).max(1);
        x.refs - 1
    })
    .unwrap_or(0)
}
fn s_pointer(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), swith(c.a(0), |x| x.buf).unwrap_or(0));
    0
}
fn s_size(c: &mut Ctx) -> u32 {
    swith(c.a(0), |x| x.size).unwrap_or(0)
}
fn s_get_time(c: &mut Ctx) -> u32 {
    let (s, a, b) = (c.a(0), c.a(1), c.a(2));
    match swith(s, |x| x.time).flatten() {
        Some((t0, t1)) => {
            if a != 0 {
                w64(a, t0);
            }
            if b != 0 {
                w64(b, t1);
            }
            0
        }
        // VFW_E_SAMPLE_TIME_NOT_SET
        None => 0x8004_0249,
    }
}
fn s_set_time(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(1), c.a(2));
    let t = (a != 0).then(|| (mem::r64(a), if b != 0 { mem::r64(b) } else { mem::r64(a) + 1 }));
    swith(c.a(0), |x| x.time = t);
    0
}
fn s_is_sync(c: &mut Ctx) -> u32 {
    if swith(c.a(0), |x| x.sync) == Some(true) { 0 } else { S_FALSE }
}
fn s_set_sync(c: &mut Ctx) -> u32 {
    let v = c.a(1) != 0;
    swith(c.a(0), |x| x.sync = v);
    0
}
fn s_false(_: &mut Ctx) -> u32 {
    S_FALSE
}
fn s_len(c: &mut Ctx) -> u32 {
    swith(c.a(0), |x| x.len).unwrap_or(0)
}
fn s_set_len(c: &mut Ctx) -> u32 {
    let n = c.a(1);
    swith(c.a(0), |x| {
        if n > x.size {
            return 0x8007_0057;
        }
        x.len = n;
        0
    })
    .unwrap_or(E_UNEXPECTED)
}
/// GetMediaType: no change (S_FALSE, *NULL)
fn s_media_type(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), 0);
    S_FALSE
}
fn s_is_disc(c: &mut Ctx) -> u32 {
    if swith(c.a(0), |x| x.disc) == Some(true) { 0 } else { S_FALSE }
}
fn s_set_disc(c: &mut Ctx) -> u32 {
    let v = c.a(1) != 0;
    swith(c.a(0), |x| x.disc = v);
    0
}
/// GetMediaTime: VFW_E_MEDIA_TIME_NOT_SET
fn s_media_time(_: &mut Ctx) -> u32 {
    0x8004_0251
}

// ---------------------------------------------------------------------------------------------
// the stream: its clock and the thread that plays it

struct Pipe {
    reader: u32,
    len: u64,
    info: mpeg1::Info,
    tl: Timeline,
    thread: bool,
    /// the splitter went: the thread ends
    dead: bool,
    /// EC_COMPLETE went out (until a seek or a stop)
    ended: bool,
    /// stops and seeks so far: the thread starts the stream again from the position
    gen: u32,
    /// the thread's calls into the game's objects (the reader, the renderer's pin): a stop
    /// waits them out, as Windows' source filters do, before the game takes those apart
    calls: Arc<Calls>,
}
static PIPES: Mutex<Option<HashMap<u32, Pipe>>> = Mutex::new(None);

fn pwith<R>(b: u32, f: impl FnOnce(&mut Pipe) -> R) -> Option<R> {
    PIPES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

/// The splitter in graph `g` (the graph's IFilterGraph pointer, as JoinFilterGraph gave it).
fn in_graph(g: u32) -> Option<u32> {
    let f = FILTERS.lock().unwrap_or_else(|e| e.into_inner());
    let b = f.as_ref()?.iter().find(|(_, x)| x.kind == Kind::Split && x.graph == g).map(|(&b, _)| b)?;
    drop(f);
    pwith(b, |_| ()).map(|_| b)
}

/// The timeline of the MPEG-1 stream graph `g` plays (dshow.rs: IMediaSeeking and IMediaPosition
/// go by it).
pub fn timeline<R>(g: u32, f: impl FnOnce(&mut Timeline, u64) -> R) -> Option<R> {
    let b = in_graph(g)?;
    let t = now();
    let r = pwith(b, |p| f(&mut p.tl, t));
    WAKE.notify_all();
    r
}

/// Graph `g` was seeked: the stream again from the new position.
pub fn seeked(g: u32) {
    if let Some(b) = in_graph(g) {
        pwith(b, |p| {
            p.gen += 1;
            p.ended = false;
        });
        WAKE.notify_all();
    }
}

/// Graph `g` went: our filters in it let go of what they hold of the game's (its source's
/// reader, its renderer's pins), their connections and the splitter's thread go, as Windows'
/// graph disconnects every filter when it is released.
pub fn graph_released(c: &mut Ctx, g: u32) {
    let mine: Vec<u32> = FILTERS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).iter().filter(|(_, f)| f.graph == g).map(|(&b, _)| b).collect();
    // (the splitter thread's calls into the source and the renderer come back first)
    for &b in &mine {
        stop_calls(b, true);
    }
    for b in mine {
        let pins = fwith(b, |f| {
            f.graph = 0;
            f.state = 0;
            f.pins.iter_mut().map(|p| (std::mem::take(&mut p.peer), std::mem::take(&mut p.mem))).collect::<Vec<_>>()
        })
        .unwrap_or_default();
        for (peer, mem_pin) in pins {
            for x in [peer, mem_pin] {
                if x != 0 && pin_of(x).is_none() {
                    com(c, x, 2, &[]);
                }
            }
        }
        let reader = pwith(b, |p| {
            p.dead = true;
            std::mem::take(&mut p.reader)
        })
        .unwrap_or(0);
        if reader != 0 {
            com(c, reader, 2, &[]);
        }
    }
    WAKE.notify_all();
}

/// Whether `p` is a pin of ours.
pub fn is_pin(p: u32) -> bool {
    pin_of(p).is_some()
}

/// The size, duration and picture rate of graph `g`'s stream, if it plays one.
pub fn movie(g: u32) -> Option<mpeg1::Info> {
    let b = in_graph(g)?;
    pwith(b, |p| p.info)
}

static WAKE: std::sync::Condvar = std::sync::Condvar::new();
static WAKE_LOCK: Mutex<()> = Mutex::new(());

/// The splitter's new state: the clock runs, stands, or stops (the position stays: DirectShow
/// does not rewind); the thread starts with the first Pause or Run.
fn streaming(b: u32, s: u32) {
    let t = now();
    let start = pwith(b, |p| {
        match s {
            0 => {
                p.tl.stop(t);
                p.gen += 1;
                p.ended = false;
            }
            1 => p.tl.pause(t),
            _ => p.tl.run(t),
        }
        if s != 0 {
            p.calls.run();
        }
        s != 0 && !std::mem::replace(&mut p.thread, true)
    });
    WAKE.notify_all();
    if s == 0 {
        stop_calls(b, false);
    }
    if start == Some(true) {
        static ENTRY: OnceLock<u32> = OnceLock::new();
        let entry = *ENTRY.get_or_init(|| crate::api::host_fn("mpeg1_stream_thread", stream_thread, 4));
        crate::spawn_gated(entry, b, 0x10000, crate::obj::new_thread_obj(0), false);
    }
}

/// Bytes read from the source at once.
const READ: u32 = 64 << 10;
/// Frames of sound (at kura's rate) queued ahead of the clock.
const AHEAD: u64 = 4410;

/// What the thread works on (its own: no other thread touches it).
struct Dec {
    gen: u32,
    pos: u64,
    demux: mpeg1::Demux,
    video: Option<mpeg1::Video>,
    audio: Option<mpeg1::Audio>,
    /// the next picture, its number, as BGRX bottom-up
    next: Option<(u64, Vec<u8>)>,
    pictures: u64,
    /// the sound at kura's rate, from frame `pcm_at` on
    pcm: VecDeque<[f32; 2]>,
    pcm_at: u64,
    rs: Resampler,
    out: Option<(i32, u64)>,
    read_all: bool,
    /// the pictures went past the stop position
    past_stop: bool,
    eos_sent: bool,
    buf: u32,
}

impl Dec {
    fn new(gen: u32, video: bool, audio: bool, rate: u32) -> Dec {
        Dec {
            gen,
            pos: 0,
            demux: mpeg1::Demux::new(),
            video: video.then(mpeg1::Video::new),
            audio: audio.then(mpeg1::Audio::new),
            next: None,
            pictures: 0,
            pcm: VecDeque::new(),
            pcm_at: 0,
            rs: Resampler::new(rate, kura::audio::RATE),
            out: None,
            read_all: false,
            past_stop: false,
            eos_sent: false,
            buf: 0,
        }
    }
    fn close(self) {
        if let Some((a, _)) = self.out {
            kura::audio::close(a);
        }
        if self.buf != 0 {
            mem::heap_free(self.buf);
        }
    }
}

/// Linear resampling of stereo frames from one rate to another, a stream at a time.
#[derive(Clone, Debug)]
pub struct Resampler {
    from: u32,
    to: u32,
    /// the last input frame, and where the next output falls after it (in 1/to of an input frame)
    last: [f32; 2],
    at: u64,
    started: bool,
}

impl Resampler {
    pub fn new(from: u32, to: u32) -> Resampler {
        Resampler { from: from.max(1), to: to.max(1), last: [0.0; 2], at: 0, started: false }
    }
    /// The output frames of the input frames `inp` (interleaved stereo) onto `out`.
    pub fn push(&mut self, inp: &[f32], out: &mut VecDeque<[f32; 2]>) {
        if self.from == self.to {
            out.extend(inp.chunks_exact(2).map(|f| [f[0], f[1]]));
            return;
        }
        for f in inp.chunks_exact(2) {
            let cur = [f[0], f[1]];
            if !self.started {
                self.started = true;
                self.last = cur;
                continue;
            }
            // outputs between last (0) and cur (to): at, at + from, ...
            while self.at < self.to as u64 {
                let k = self.at as f32 / self.to as f32;
                out.push_back([self.last[0] + (cur[0] - self.last[0]) * k, self.last[1] + (cur[1] - self.last[1]) * k]);
                self.at += self.from as u64;
            }
            self.at -= self.to as u64;
            self.last = cur;
        }
    }
}

/// The chain the thread feeds: (the video decoder, the renderer's IMemInputPin, its pin), and
/// whether the sound reaches the sound renderer.
fn chain(b: u32) -> (Option<(u32, u32, u32)>, bool) {
    let v = pin_of(pin(b, 2).peer).map(|(d, _)| d).filter(|&d| kind(d) == Some(Kind::VDec)).and_then(|d| {
        let o = pin(d, 2);
        (o.mem != 0).then_some((d, o.mem, o.peer))
    });
    let a = pin_of(pin(b, 3).peer).map(|(d, _)| d).filter(|&d| kind(d) == Some(Kind::ADec)).is_some_and(|d| pin_of(pin(d, 2).peer).is_some());
    (v, a)
}

/// How long a stop waits for the splitter thread's call in flight (a call that waits for what
/// only the stopping thread would do could wait forever).
const STOP_WAIT: Duration = Duration::from_secs(2);

/// No more calls from the splitter thread of `b` into the game's objects until it pauses or
/// runs again (`close`: ever); the call in flight comes back first.
fn stop_calls(b: u32, close: bool) {
    let Some(calls) = pwith(b, |p| p.calls.clone()) else { return };
    WAKE.notify_all();
    let back = if close { calls.close(STOP_WAIT, &crate::k32::Pause) } else { calls.stop(STOP_WAIT, &crate::k32::Pause) };
    if !back {
        eprintln!("sake: MPEG-1 splitter: a call of its thread did not come back in {STOP_WAIT:?}");
    }
}

/// One call of the splitter thread into the game's objects, unless a stop came after `epoch`
/// (None then).
fn call_out<R>(calls: &Calls, epoch: u32, f: impl FnOnce() -> R) -> Option<R> {
    if !calls.enter(epoch) {
        return None;
    }
    let r = f();
    calls.leave();
    Some(r)
}

/// The splitter's thread (its parameter): reads, decodes, hands out the pictures when due,
/// queues the sound, tells the graph the end; until the splitter goes.
fn stream_thread(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let mut d: Option<Dec> = None;
    loop {
        // (the epoch before what it goes by: a stop or a new reader after this keeps its calls out)
        let Some(calls) = pwith(b, |p| p.calls.clone()) else { break };
        let epoch = calls.epoch();
        let t = now();
        let Some((dead, gen, pos, running, stop, reader, len, info)) = pwith(b, |p| (p.dead, p.gen, p.tl.position(t), p.tl.running(), p.tl.stop, p.reader, p.len, p.info)) else { break };
        if dead {
            break;
        }
        // (stopped: the game may take its source and renderer apart now; nothing of theirs is
        // touched until it pauses or runs again)
        if fwith(b, |f| f.state).unwrap_or(0) == 0 || reader == 0 {
            if let Some(s) = d.as_mut() {
                if let Some((a, _)) = s.out.take() {
                    kura::audio::close(a);
                }
            }
            pause(SEC / 50);
            continue;
        }
        let (video, audio) = chain(b);
        // (a seek or a stop: the stream again from the start, to the position)
        if d.as_ref().is_none_or(|d| d.gen != gen) {
            if let Some(old) = d.take() {
                old.close();
                if let Some((_, _, rpin)) = video {
                    call_out(&calls, epoch, || {
                        com(c, rpin, 15, &[]);
                        com(c, rpin, 16, &[]);
                    });
                }
            }
            d = Some(Dec::new(gen, video.is_some(), audio, info.audio_rate));
        }
        let s = d.as_mut().unwrap();
        let frame = if info.rate > 0.0 { (SEC as f64 / info.rate) as u64 } else { SEC / 30 };
        // the sound to kura, up to a little ahead of the clock
        let want = (pos as u128 * kura::audio::RATE as u128 / SEC as u128) as u64;
        let g = fwith(b, |f| f.graph).unwrap_or(0);
        sound(s, running, want, crate::dshow::volume(g));
        // (the sound keeps half a second decoded ahead, read on while a picture waits)
        let sound_low = s.audio.is_some() && s.pcm_at + (s.pcm.len() as u64) < want + kura::audio::RATE as u64 / 2;
        // the next picture: due, or wait for it
        if let Some((n, _)) = &s.next {
            let at = n * frame;
            if at >= stop && pos >= stop {
                // (past the stop position: no more pictures)
                s.next = None;
                s.past_stop = true;
                continue;
            }
            if !running || at > pos {
                if !(sound_low && feed(c, s, reader, len, &calls, epoch)) {
                    let wait = if running { (at - pos).min(SEC / 100) } else { SEC / 100 };
                    pause(wait);
                }
                continue;
            }
            // (stopped meanwhile: the picture waits for the next run)
            if video.is_some() && !calls.enter(epoch) {
                pause(SEC / 100);
                continue;
            }
            let (n, px) = s.next.take().unwrap();
            // (late by more than two pictures: dropped)
            if let Some((dec, mem_pin, _)) = video {
                if n * frame + 2 * frame >= pos {
                    let sample = free_sample(dec, px.len() as u32);
                    let buf = swith(sample, |x| {
                        x.len = px.len() as u32;
                        x.time = Some((n * frame, (n + 1) * frame));
                        x.sync = true;
                        x.disc = n == 0;
                        x.buf
                    })
                    .unwrap_or(0);
                    mem::slice_mut(buf, px.len() as u32).copy_from_slice(&px);
                    com(c, mem_pin, 6, &[sample]);
                }
                calls.leave();
            }
            continue;
        }
        // decode: a picture, or more of the stream
        if !s.past_stop {
            if let Some(v) = s.video.as_mut() {
                if let Some(f) = v.decode() {
                    let mut px = vec![0u8; f.width * f.height * 4];
                    mpeg1::video::to_bgrx(f, &mut px, f.width * 4, true);
                    s.next = Some((s.pictures, px));
                    s.pictures += 1;
                    continue;
                }
            }
        }
        // (only sound: decoded a second ahead)
        let sound_full = s.video.is_none() && s.pcm_at + (s.pcm.len() as u64) > want + kura::audio::RATE as u64;
        if !s.past_stop && !sound_full && feed(c, s, reader, len, &calls, epoch) {
            continue;
        }
        if !s.read_all && !s.past_stop {
            pause(SEC / 100);
            continue;
        }
        // the end: the renderer told, then the graph once the clock is there and the sound out
        if let Some((_, _, rpin)) = video {
            if !s.eos_sent && call_out(&calls, epoch, || com(c, rpin, 14, &[])).is_some() {
                s.eos_sent = true;
            }
        }
        let sound_end = if audio { (s.pcm_at + s.pcm.len() as u64) * SEC / kura::audio::RATE as u64 } else { 0 };
        let last = (s.pictures * frame).max(sound_end).min(stop);
        if running && pos >= last && (s.pcm.is_empty() || s.past_stop) {
            let tell = pwith(b, |p| p.gen == gen && !std::mem::replace(&mut p.ended, true)).unwrap_or(false);
            if tell && g != 0 {
                crate::dshow::completed(g);
            }
        }
        pause(SEC / 50);
    }
    if let Some(s) = d {
        s.close();
    }
    PIPES.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
    crate::k32::exit_thread(0)
}

/// One step on: a packet to its decoder (and the sound decoded), or more of the stream read.
/// False once all of it went to the decoders.
fn feed(c: &mut Ctx, s: &mut Dec, reader: u32, len: u64, calls: &Calls, epoch: u32) -> bool {
    if s.read_all {
        return false;
    }
    if let Some(p) = s.demux.next() {
        if mpeg1::ps::is_video(p.id) {
            if let Some(v) = s.video.as_mut() {
                v.push(&p.data);
            }
        } else if mpeg1::ps::is_audio(p.id) {
            if let Some(a) = s.audio.as_mut() {
                a.push(&p.data);
                while a.decode() {
                    s.rs.push(&a.out, &mut s.pcm);
                }
            }
        }
        return true;
    }
    if s.pos >= len {
        s.demux.end();
        if s.demux.next().is_none() {
            s.read_all = true;
            if let Some(v) = s.video.as_mut() {
                v.end();
            }
            if let Some(a) = s.audio.as_mut() {
                a.end();
                while a.decode() {
                    s.rs.push(&a.out, &mut s.pcm);
                }
            }
        }
        return true;
    }
    let n = (len - s.pos).min(READ as u64) as u32;
    if s.buf == 0 {
        s.buf = mem::heap_alloc(READ, false);
    }
    let args: Vec<u32> = if mem::x64() { vec![s.pos as u32, n, s.buf] } else { vec![s.pos as u32, (s.pos >> 32) as u32, n, s.buf] };
    // (stopped meanwhile: nothing read)
    let Some(r) = call_out(calls, epoch, || com(c, reader, 7, &args)) else { return false };
    if r & 0x8000_0000 != 0 {
        eprintln!("sake: MPEG-1 splitter: read at {} failed ({r:#x})", s.pos);
        s.pos = len;
        return true;
    }
    let bytes = mem::slice(s.buf, n).to_vec();
    s.demux.push(&bytes);
    s.pos += n as u64;
    true
}

/// Waits up to `t` (100 ns units), or less when told something changed.
fn pause(t: u64) {
    let g = WAKE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    drop(WAKE.wait_timeout(g, std::time::Duration::from_nanos(t * 100)));
    crate::snap::point();
}

/// The sound up to `want` + AHEAD to kura; what is before `want` (seeked past, or played while
/// the stream was read again) goes. None while the clock stands: what kura has queued is lost
/// on a pause, as the movie player's.
fn sound(s: &mut Dec, running: bool, want: u64, volume: i32) {
    if s.out.is_none() || !running {
        let n = want.saturating_sub(s.pcm_at).min(s.pcm.len() as u64) as usize;
        s.pcm.drain(..n);
        s.pcm_at += n as u64;
    }
    if !running || s.audio.is_none() {
        if let Some((a, _)) = s.out.take() {
            kura::audio::close(a);
        }
        return;
    }
    let (a, next) = match s.out {
        Some((a, next)) if (next as i128 - kura::audio::queued(a) as i128 - want as i128).abs() <= kura::audio::RATE as i128 * 60 / 1000 => (a, next),
        old => {
            if let Some((a, _)) = old {
                kura::audio::close(a);
            }
            (kura::audio::open(), want)
        }
    };
    // (what is before `next` is not wanted any more)
    let drop_n = next.saturating_sub(s.pcm_at).min(s.pcm.len() as u64) as usize;
    s.pcm.drain(..drop_n);
    s.pcm_at += drop_n as u64;
    let mut next = next.max(s.pcm_at);
    let end = want + AHEAD;
    if next < end && !s.pcm.is_empty() && s.pcm_at <= next {
        let n = ((end - next) as usize).min(s.pcm.len());
        let gain = if volume >= 0 { 1.0 } else { 10f32.powf(volume.max(-10000) as f32 / 2000.0) };
        let buf: Vec<f32> = s.pcm.drain(..n).flat_map(|f| [f[0] * gain, f[1] * gain]).collect();
        kura::audio::write(a, &buf);
        s.pcm_at += n as u64;
        next += n as u64;
    }
    s.out = Some((a, next));
}

// ---------------------------------------------------------------------------------------------

struct Vtables {
    filter: u32,
    stream_select: u32,
    pin: u32,
    enum_pins: u32,
    allocator: u32,
    sample: u32,
}

fn vtables() -> &'static Vtables {
    use crate::api::vtable;
    static V: OnceLock<Vtables> = OnceLock::new();
    V.get_or_init(|| {
        let filter: [(Handler, u32); 15] = [
            (f_qi, 12),
            (f_add_ref, 4),
            (f_release, 4),
            (f_class, 8),
            (f_stop, 4),
            (f_pause, 4),
            (f_run, 12),
            (f_get_state, 12),
            (ok, 8),       // SetSyncSource
            (get_null, 8), // GetSyncSource
            (f_enum_pins, 8),
            (f_find_pin, 12),
            (f_info, 8),
            (f_join, 12),
            (notimpl, 8), // QueryVendorInfo
        ];
        let pin: [(Handler, u32); 18] = [
            (p_qi, 12),
            (p_add_ref, 4),
            (p_release, 4),
            (p_connect, 12),
            (p_receive_connection, 12),
            (p_disconnect, 4),
            (p_connected_to, 8),
            (p_connection_type, 8),
            (p_info, 8),
            (p_direction, 8),
            (p_id, 8),
            (p_accept, 8),
            (notimpl, 8),  // EnumMediaTypes
            (notimpl, 12), // QueryInternalConnections
            (ok, 4),       // EndOfStream
            (ok, 4),       // BeginFlush
            (ok, 4),       // EndFlush
            (ok, 28),      // NewSegment
        ];
        let enum_pins: [(Handler, u32); 7] = [(e_qi, 12), (e_add_ref, 4), (e_release, 4), (e_next, 16), (e_skip, 8), (e_reset, 4), (e_clone, 8)];
        let allocator: [(Handler, u32); 9] = [(a_qi, 12), (one, 4), (one, 4), (a_set_props, 12), (a_get_props, 8), (ok, 4), (ok, 4), (a_get_buffer, 20), (ok, 8)];
        let sample: [(Handler, u32); 19] = [
            (s_qi, 12),
            (s_add_ref, 4),
            (s_release, 4),
            (s_pointer, 8),
            (s_size, 4),
            (s_get_time, 12),
            (s_set_time, 12),
            (s_is_sync, 4),
            (s_set_sync, 8),
            (s_false, 4), // IsPreroll
            (ok, 8),      // SetPreroll
            (s_len, 4),
            (s_set_len, 8),
            (s_media_type, 8),
            (ok, 8), // SetMediaType
            (s_is_disc, 4),
            (s_set_disc, 8),
            (s_media_time, 12),
            (ok, 12), // SetMediaTime
        ];
        let ss: [(Handler, u32); 6] = [(ss_qi, 12), (ss_add_ref, 4), (ss_release, 4), (ss_count, 8), (ss_info, 36), (ss_enable, 12)];
        Vtables {
            filter: vtable("MPEG-1 IBaseFilter", &filter),
            stream_select: vtable("MPEG-1 IAMStreamSelect", &ss),
            pin: vtable("MPEG-1 IPin", &pin),
            enum_pins: vtable("MPEG-1 IEnumPins", &enum_pins),
            allocator: vtable("MPEG-1 IMemAllocator", &allocator),
            sample: vtable("MPEG-1 IMediaSample", &sample),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn guids_are_windows_bytes() {
        // {336475d0-942a-11ce-a870-00aa002feab5}
        assert_eq!(CLSID_MPEG1_SPLITTER[..8], [0xd0, 0x75, 0x64, 0x33, 0x2a, 0x94, 0xce, 0x11]);
        // {73646976-0000-0010-8000-00aa00389b71}
        assert_eq!(MEDIATYPE_VIDEO, [0x76, 0x69, 0x64, 0x73, 0, 0, 0x10, 0, 0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71]);
        assert_eq!(IID_IPIN[..4], [0x91, 0x68, 0xa8, 0x56]);
    }

    #[test]
    fn video_info_is_a_bottom_up_dib() {
        let v = video_info(640, 480, 32, 0, 29.97);
        assert_eq!(v.len(), 88);
        assert_eq!(u64::from_le_bytes(v[40..48].try_into().unwrap()), 333_667);
        assert_eq!(i32::from_le_bytes(v[56..60].try_into().unwrap()), 480);
        assert_eq!(u32::from_le_bytes(v[68..72].try_into().unwrap()), 640 * 480 * 4);
    }

    proptest! {
        /// Resampling gives (to/from) times the frames, however the input is cut, and stays
        /// between neighbouring inputs.
        #[test]
        fn resampling_is_steady(from in prop::sample::select(vec![32000u32, 44100, 48000, 22050]), x in prop::collection::vec(-1f32..1.0, 2..4000), cuts in prop::collection::vec(1usize..300, 0..10)) {
            let n = x.len() / 2 * 2;
            let x = &x[..n];
            let mut whole = VecDeque::new();
            Resampler::new(from, 44100).push(x, &mut whole);
            let mut r = Resampler::new(from, 44100);
            let mut parts = VecDeque::new();
            let mut at = 0;
            for c in cuts {
                let e = (at + 2 * c).min(n);
                r.push(&x[at..e], &mut parts);
                at = e;
            }
            r.push(&x[at..], &mut parts);
            prop_assert_eq!(&whole, &parts);
            let frames = n / 2;
            if from != 44100 {
                let want = (frames.saturating_sub(1)) as f64 * 44100.0 / from as f64;
                prop_assert!((whole.len() as f64 - want).abs() <= 1.0, "{} vs {}", whole.len(), want);
            } else {
                prop_assert_eq!(whole.len(), frames);
            }
            let (lo, hi) = x.iter().fold((f32::MAX, f32::MIN), |a, &v| (a.0.min(v), a.1.max(v)));
            for f in &whole {
                prop_assert!(f[0] >= lo - 1e-6 && f[0] <= hi + 1e-6);
            }
        }
    }
}
