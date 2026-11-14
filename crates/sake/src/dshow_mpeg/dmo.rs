//! The DMO Wrapper Filter (CLSID_DMOWrapperFilter) around Windows Media's video decoder DMO,
//! for games that build their graph by hand: KiriKiri's krmovie (#60) pushes the compressed
//! objects of its own WMV source (wmvcore.rs's sync reader, SetReadStreamSamples) into the
//! wrapper's input pin (IMemInputPin::Receive, on the source's thread), and the wrapper hands
//! the pictures, RGB32 bottom-up as Windows' decoder gives them to an RGB32 renderer, to the
//! game's renderer when they are due (the renderer draws what it gets at once: it has no clock).
//! The decoding is wmdec.rs's (crates/wmcodec, FFmpeg's decoders ported).
//!
//! The filter is one of dshow_mpeg.rs's (Kind::Dmo): word DW of its block is its
//! IDMOWrapperFilter, word MI its input pin's IMemInputPin.

use super::*;
use crate::wmdec::{self, Frame};

/// IDMOWrapperFilter {52d6f586-9f0f-4824-8fc8-e32ca04930c2}
pub(super) const IID_IDMOWRAPPERFILTER: [u8; 16] = guid(0x52d6_f586, 0x9f0f, 0x4824, [0x8f, 0xc8, 0xe3, 0x2c, 0xa0, 0x49, 0x30, 0xc2]);
/// CLSID_DMOWrapperFilter {94297043-bd82-4dfd-b0de-8177739c6d20}
pub const CLSID_DMO_WRAPPER: [u8; 16] = guid(0x9429_7043, 0xbd82, 0x4dfd, [0xb0, 0xde, 0x81, 0x77, 0x73, 0x9c, 0x6d, 0x20]);
const VFW_E_WRONG_STATE: u32 = 0x8004_0227;

struct Dmo {
    /// the DMO Init asked for
    clsid: [u8; 16],
    codec: Option<Box<dyn wmdec::Codec>>,
    w: u32,
    h: u32,
    /// AvgTimePerFrame of the input (100 ns)
    frame: u64,
    tl: Timeline,
    flushing: bool,
    /// stops and flushes so far: a picture waiting goes
    gen: u32,
    /// a picture went to the renderer since the last stop or flush (the paused graph's one)
    shown: bool,
}
static DMOS: Mutex<Option<HashMap<u32, Dmo>>> = Mutex::new(None);

fn dwith<R>(b: u32, f: impl FnOnce(&mut Dmo) -> R) -> Option<R> {
    DMOS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

pub(super) fn created(b: u32) {
    let d = Dmo { clsid: [0; 16], codec: None, w: 0, h: 0, frame: SEC / 30, tl: Timeline::new(u64::MAX / 4), flushing: false, gen: 0, shown: false };
    DMOS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, d);
}
pub(super) fn gone(b: u32) {
    DMOS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b);
    WAKE.notify_all();
}

/// Whether graph `g` (its IFilterGraph pointer) has a wrapper that plays: the graph's own end
/// does not come then (the input's EndOfStream tells it).
pub fn in_graph(g: u32) -> bool {
    let f = FILTERS.lock().unwrap_or_else(|e| e.into_inner());
    f.as_ref().is_some_and(|m| m.iter().any(|(b, x)| x.kind == Kind::Dmo && x.graph == g && x.pins[0].peer != 0 && DMOS.lock().unwrap_or_else(|e| e.into_inner()).as_ref().is_some_and(|d| d.get(b).is_some_and(|d| d.codec.is_some()))))
}

/// The input's type (from a game's source): video a codec of wmdec.rs decodes.
pub(super) fn receive_connection(c: &mut Ctx, b: u32, from: u32, mt: u32) -> u32 {
    if mt == 0 || !is(mt, &MEDIATYPE_VIDEO) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    let (cb, pb) = mt_fmt_at();
    let (len, fmt) = (r32(mt + cb), rp(mt + pb));
    if !is(mt + 44, &FORMAT_VIDEO_INFO) || len < 88 || fmt == 0 {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    let f = mem::slice(fmt, len).to_vec();
    let Some(info) = wmdec::Info::from_bih(&f[48..]) else { return VFW_E_TYPE_NOT_ACCEPTED };
    let Some(codec) = wmdec::codec(&info) else {
        eprintln!("sake: DMO wrapper: no decoder for {:?} (format {} bytes)", info, len);
        return VFW_E_TYPE_NOT_ACCEPTED;
    };
    let wmdec::Info::Video { width, height, .. } = info else { return VFW_E_TYPE_NOT_ACCEPTED };
    let frame = u64::from_le_bytes(f[40..48].try_into().unwrap());
    let h = (height as i32).unsigned_abs();
    dwith(b, |d| {
        d.codec = Some(codec);
        d.w = width;
        d.h = h;
        d.frame = if frame > 0 { frame } else { SEC / 30 };
    });
    com(c, from, 1, &[]);
    let copy = mem::heap_alloc(mt_size(), true);
    copy_mt(mt, copy);
    fwith(b, |x| {
        x.pins[0].peer = from;
        x.pins[0].mt = copy;
    });
    eprintln!("sake: DMO wrapper: {}x{} {:.3} fps", width, h, SEC as f64 / frame.max(1) as f64);
    0
}

/// The output's type: RGB32 of the input's size.
pub(super) fn out_type(b: u32) -> Option<u32> {
    let (w, h, frame) = dwith(b, |d| (d.w, d.h, d.frame)).filter(|x| x.0 > 0)?;
    Some(new_mt(&MEDIATYPE_VIDEO, &SUBTYPE_RGB32, true, w * h * 4, &FORMAT_VIDEO_INFO, &video_info(w, h, 32, 0, SEC as f64 / frame as f64)))
}
pub(super) fn size(b: u32) -> Option<u32> {
    dwith(b, |d| d.w * d.h * 4).filter(|&s| s > 0)
}

/// Stop / Pause / Run of the filter: its clock (stream time from Run's start).
pub(super) fn state(b: u32, s: u32) {
    let t = now();
    dwith(b, |d| match s {
        0 => {
            d.tl.stop(t);
            d.tl.seek(t, 0);
            d.gen += 1;
            d.shown = false;
        }
        1 => d.tl.pause(t),
        _ => d.tl.run(t),
    });
    WAKE.notify_all();
}

/// The input pin's IPin::EndOfStream / BeginFlush / EndFlush / NewSegment: on to the renderer
/// (`slot`), the graph told of the end.
pub(super) fn downstream(c: &mut Ctx, b: u32, slot: u32) -> u32 {
    match slot {
        15 => {
            dwith(b, |d| {
                d.flushing = true;
                d.gen += 1;
            });
            WAKE.notify_all();
        }
        16 => {
            dwith(b, |d| {
                d.flushing = false;
                d.shown = false;
                if let Some(x) = d.codec.as_mut() {
                    x.flush();
                }
            });
        }
        _ => {}
    }
    let out = pin(b, 2).peer;
    if out == 0 {
        return 0;
    }
    let args: Vec<u32> = (1..if slot == 17 { if ps() == 4 { 7 } else { 4 } } else { 1 }).map(|i| c.a(i)).collect();
    com(c, out, slot, &args);
    if slot == 14 {
        let g = fwith(b, |f| f.graph).unwrap_or(0);
        if g != 0 {
            crate::dshow::completed(g);
        }
    }
    0
}

fn base_of(mi: u32) -> u32 {
    mi - ps() * MI
}

fn mi_qi(c: &mut Ctx) -> u32 {
    let (mi, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let b = base_of(mi);
    if is(iid, &IID_IMEMINPUTPIN) {
        add_ref(b);
        putp(out, mi);
        return 0;
    }
    if is(iid, &IID_IUNKNOWN) || is(iid, &IID_IPIN) {
        add_ref(b);
        putp(out, b + ps());
        return 0;
    }
    putp(out, 0);
    E_NOINTERFACE
}
fn mi_add_ref(c: &mut Ctx) -> u32 {
    add_ref(base_of(c.a(0)))
}
fn mi_release(c: &mut Ctx) -> u32 {
    let b = base_of(c.a(0));
    release(c, b)
}
/// GetAllocator: none of its own (VFW_E_NO_ALLOCATOR: the source's is used)
fn mi_get_allocator(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        putp(c.a(1), 0);
    }
    0x8004_020a
}

/// Receive(sample): decoded, its pictures to the renderer when due.
fn mi_receive(c: &mut Ctx) -> u32 {
    let (b, sample) = (base_of(c.a(0)), c.a(1));
    receive(c, b, sample)
}
fn receive(c: &mut Ctx, b: u32, sample: u32) -> u32 {
    if sample == 0 {
        return E_POINTER;
    }
    if fwith(b, |f| f.state).unwrap_or(0) == 0 {
        return VFW_E_WRONG_STATE;
    }
    if dwith(b, |d| d.flushing).unwrap_or(true) {
        return S_FALSE;
    }
    let tmp = mem::heap_alloc(16, true);
    com(c, sample, 3, &[tmp]);
    let ptr = rp(tmp);
    let len = com(c, sample, 11, &[]);
    let time = if com(c, sample, 5, &[tmp, tmp + 8]) == 0 { mem::r64(tmp) } else { u64::MAX };
    mem::heap_free(tmp);
    let key = com(c, sample, 7, &[]) == 0;
    if ptr == 0 {
        return E_POINTER;
    }
    let data = mem::slice(ptr, len).to_vec();
    let Some((mut codec, gen)) = dwith(b, |d| (d.codec.take(), d.gen)) else { return E_UNEXPECTED };
    let t0 = std::time::Instant::now();
    let frames = codec.as_mut().map(|x| x.decode(&data, if time == u64::MAX { 0 } else { time }, key)).unwrap_or_default();
    stats(Stat::Decoded(t0.elapsed()));
    dwith(b, |d| d.codec = codec);
    for f in frames {
        if let Frame::Picture { time, w, h, bgra } = f {
            if !show(c, b, gen, time, w, h, &bgra) {
                break;
            }
        }
    }
    0
}

/// One picture to the renderer once the clock is there (false: stopped or flushed meanwhile).
fn show(c: &mut Ctx, b: u32, gen: u32, time: u64, w: u32, h: u32, bgra: &[u8]) -> bool {
    loop {
        let t = now();
        let Some((g, flushing, pos, running, shown, frame)) = dwith(b, |d| (d.gen, d.flushing, d.tl.position(t), d.tl.running(), d.shown, d.frame)) else { return false };
        let state = fwith(b, |f| f.state).unwrap_or(0);
        if g != gen || flushing || state == 0 {
            return false;
        }
        if running && time <= pos {
            // (late by more than two pictures: dropped)
            if time + 2 * frame < pos && shown {
                stats(Stat::Dropped);
                return true;
            }
            break;
        }
        // (paused: the first picture shown, then it waits)
        if !running && !shown {
            break;
        }
        pause(if running { (time - pos).min(SEC / 100) } else { SEC / 100 });
    }
    let mem_pin = pin(b, 2).mem;
    let Some((ow, oh, frame)) = dwith(b, |d| (d.w, d.h, d.frame)) else { return false };
    if mem_pin == 0 {
        return true;
    }
    let px = wmdec::to_rgb(bgra, w, h, ow, oh, 32);
    let s = free_sample(b, px.len() as u32);
    let buf = swith(s, |x| {
        x.len = px.len() as u32;
        x.time = Some((time, time + frame));
        x.sync = true;
        x.disc = false;
        x.buf
    })
    .unwrap_or(0);
    mem::slice_mut(buf, px.len() as u32).copy_from_slice(&px);
    dwith(b, |d| d.shown = true);
    com(c, mem_pin, 6, &[s]);
    stats(Stat::Shown);
    true
}

enum Stat {
    Decoded(std::time::Duration),
    Shown,
    Dropped,
}

/// SAKE_MOVIE_STATS: every 5 s, the samples decoded (and the time per sample), the pictures
/// handed to the renderer and the ones dropped as late.
fn stats(s: Stat) {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};
    static ON: OnceLock<bool> = OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("SAKE_MOVIE_STATS").is_some()) {
        return;
    }
    static S: Mutex<Option<(Instant, u32, Duration, Duration, u32, u32)>> = Mutex::new(None);
    let mut g = S.lock().unwrap_or_else(|e| e.into_inner());
    let (since, n, sum, max, shown, dropped) = g.get_or_insert_with(|| (Instant::now(), 0, Duration::ZERO, Duration::ZERO, 0, 0));
    match s {
        Stat::Decoded(d) => {
            *n += 1;
            *sum += d;
            *max = (*max).max(d);
        }
        Stat::Shown => *shown += 1,
        Stat::Dropped => *dropped += 1,
    }
    if since.elapsed() >= Duration::from_secs(5) {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        eprintln!("sake: DMO wrapper: {} samples decoded, ms avg/max {:.2}/{:.2}; {} pictures shown, {} dropped", n, ms(*sum) / (*n).max(1) as f64, ms(*max), shown, dropped);
        *g = None;
    }
}

fn mi_receive_multiple(c: &mut Ctx) -> u32 {
    let (mi, arr, n, done) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let b = base_of(mi);
    let mut k = 0;
    for i in 0..n {
        let s = rp(arr + ps() * i);
        let r = receive(c, b, s);
        if r != 0 {
            break;
        }
        k += 1;
    }
    if done != 0 {
        w32(done, k);
    }
    0
}

/// IDMOWrapperFilter: QI / AddRef / Release of the filter, Init(clsidDMO, catDMO)
fn dw_qi(c: &mut Ctx) -> u32 {
    let b = c.a(0) - ps() * DW;
    com(c, b, 0, &[c.a(1), c.a(2)])
}
fn dw_add_ref(c: &mut Ctx) -> u32 {
    add_ref(c.a(0) - ps() * DW)
}
fn dw_release(c: &mut Ctx) -> u32 {
    let b = c.a(0) - ps() * DW;
    release(c, b)
}
fn dw_init(c: &mut Ctx) -> u32 {
    let (b, clsid) = (c.a(0) - ps() * DW, c.a(1));
    if clsid == 0 {
        return E_POINTER;
    }
    let g: [u8; 16] = mem::slice(clsid, 16).try_into().unwrap();
    dwith(b, |d| d.clsid = g);
    0
}

pub(super) struct Vt {
    pub wrapper: u32,
    pub mem_input: u32,
}

pub(super) fn vt() -> &'static Vt {
    use crate::api::vtable;
    static V: OnceLock<Vt> = OnceLock::new();
    V.get_or_init(|| {
        let dw: [(Handler, u32); 4] = [(dw_qi, 12), (dw_add_ref, 4), (dw_release, 4), (dw_init, 12)];
        let mi: [(Handler, u32); 9] = [
            (mi_qi, 12),
            (mi_add_ref, 4),
            (mi_release, 4),
            (mi_get_allocator, 8),
            (ok, 12), // NotifyAllocator
            (notimpl, 8), // GetAllocatorRequirements
            (mi_receive, 8),
            (mi_receive_multiple, 16),
            (ok, 4), // ReceiveCanBlock: S_OK, it blocks
        ];
        Vt { wrapper: vtable("IDMOWrapperFilter", &dw), mem_input: vtable("DMO wrapper IMemInputPin", &mi) }
    })
}
