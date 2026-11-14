//! amstream.dll's multimedia streaming (CLSID_AMMultiMediaStream): IAMMultiMediaStream, its
//! IDirectDrawMediaStream and IAudioMediaStream, and IDirectDrawStreamSample, for games that
//! take a movie's frames into a DirectDraw surface of their own and draw it themselves
//! (ShiinaRio's MDDAVIPlay, #49).
//!
//! A movie with a `.vnv` next to it, or an ASF one sake decodes (asfmovie.rs), plays as the
//! graph's do (dshow_play.rs, its sound too, with no picture of its own): a sample's Update
//! writes the frame of the moment into its surface.
//! Any other comes to its end at once (Update answers MS_S_ENDOFSTREAM), so games go on past
//! their movie.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::api::{vtable, Ctx, Handler};
use crate::dshow_play::{self as play, frame_at, now, Play, SEC};
use crate::mem::{self, ps, putp, r32, rp, w32, wp};

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_FAIL: u32 = 0x8000_4005;
const S_FALSE: u32 = 1;
const MS_S_PENDING: u32 = 0x0004_0001;
const MS_S_ENDOFSTREAM: u32 = 0x0004_0003;
const MS_E_PURPOSEID: u32 = 0x8004_0402;
const MS_E_NOSTREAM: u32 = 0x8004_0403;
const MS_E_NOTRUNNING: u32 = 0x8004_040a;
/// (OpenFile of a file not there: what RenderFile answers)
const VFW_E_NOT_FOUND: u32 = 0x8004_0216;

/// AddMediaStream: a renderer of its own (an audio stream the game takes no samples of)
const AMMSF_ADDDEFAULTRENDERER: u32 = 1;
/// OpenFile: every stream of the file renders (its sound too), and it runs once opened
const AMMSF_RENDERALLSTREAMS: u32 = 1;
const AMMSF_NORENDER: u32 = 2;
const AMMSF_RUN: u32 = 8;
/// Update: return at once, the event / CompletionStatus telling when it is done
const SSUPDATE_ASYNC: u32 = 1;

/// MSPID_PrimaryVideo {a35ff56a-9fda-11d0-8fdf-00c04fd9189d}, MSPID_PrimaryAudio (…56b)
const VIDEO: u32 = 0xa35f_f56a;
const AUDIO: u32 = 0xa35f_f56b;
const MSPID_TAIL: [u8; 12] = [0xda, 0x9f, 0xd0, 0x11, 0x8f, 0xdf, 0x00, 0xc0, 0x4f, 0xd9, 0x18, 0x9d];

/// How long Update waits at most for a frame the page has not decoded yet.
const FRAME_TRIES: u32 = 100;

struct Mms {
    refs: u32,
    /// the streams added, by purpose (VIDEO / AUDIO): their blocks
    video: u32,
    audio: u32,
    /// an audio stream rendered (AddMediaStream's AMMSF_ADDDEFAULTRENDERER, OpenFile's
    /// AMMSF_RENDERALLSTREAMS): the movie's sound is heard
    sound: bool,
    /// a file opened: a movie plays (dshow_play's PLAYS holds it, by this block), its size,
    /// frame rate and length
    open: bool,
    playing: bool,
    size: (i32, i32),
    rate: (u32, u32),
    duration: u64,
    /// STREAMSTATE_STOP 0, STREAMSTATE_RUN 1
    state: u32,
    /// the end-of-stream event (manual), set at the end
    eos: u32,
    /// its filter graph (Initialize's, else one made when asked for: a dshow.rs graph)
    graph: u32,
}

struct Stream {
    refs: u32,
    mms: u32,
    purpose: u32,
    /// IDirectDraw it makes surfaces with (0: one of its own when it needs one)
    dd: u32,
    /// SetFormat's pixel format (DDPIXELFORMAT's 32 bytes), else the display's
    pf: Option<[u8; 32]>,
}

struct Sample {
    refs: u32,
    stream: u32,
    surface: u32,
    rect: (i32, i32, i32, i32),
    /// the frame it holds, and what the last Update came to
    frame: Option<u32>,
    status: u32,
}

enum Obj {
    Mms(Mms),
    Stream(Stream),
    Sample(Sample),
}

static OBJS: Mutex<Option<HashMap<u32, Obj>>> = Mutex::new(None);

fn objs<R>(f: impl FnOnce(&mut HashMap<u32, Obj>) -> R) -> R {
    let mut g = OBJS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}
fn mms<R>(b: u32, f: impl FnOnce(&mut Mms) -> R) -> Option<R> {
    objs(|m| match m.get_mut(&b) {
        Some(Obj::Mms(x)) => Some(f(x)),
        _ => None,
    })
}
fn stream<R>(b: u32, f: impl FnOnce(&mut Stream) -> R) -> Option<R> {
    objs(|m| match m.get_mut(&b) {
        Some(Obj::Stream(x)) => Some(f(x)),
        _ => None,
    })
}
fn sample<R>(b: u32, f: impl FnOnce(&mut Sample) -> R) -> Option<R> {
    objs(|m| match m.get_mut(&b) {
        Some(Obj::Sample(x)) => Some(f(x)),
        _ => None,
    })
}
fn refs(b: u32, d: i32) -> u32 {
    objs(|m| {
        let r = match m.get_mut(&b) {
            Some(Obj::Mms(x)) => &mut x.refs,
            Some(Obj::Stream(x)) => &mut x.refs,
            Some(Obj::Sample(x)) => &mut x.refs,
            None => return 0,
        };
        *r = r.saturating_add_signed(d);
        *r
    })
}

fn com(c: &mut Ctx, obj: u32, slot: u32, args: &[u32]) -> u32 {
    let f = rp(rp(obj) + ps() * slot);
    let mut a = vec![obj];
    a.extend_from_slice(args);
    c.call(f, &a)
}
fn add_ref_guest(c: &mut Ctx, obj: u32) {
    if obj != 0 {
        com(c, obj, 1, &[]);
    }
}

/// A STREAM_TIME argument at `i` (two words on x86) and how many words it takes.
fn time_arg(c: &Ctx, i: u32) -> u64 {
    if ps() == 4 { c.a(i) as u64 | (c.a(i + 1) as u64) << 32 } else { c.a(i) as u64 }
}
fn put_time(p: u32, t: u64) {
    w32(p, t as u32);
    w32(p + 4, (t >> 32) as u32);
}

/// The interface a GUID at `iid` names, of the ones an object of `kind` has: its first word
/// (0 IUnknown, then theirs).
fn iid_ok(iid: u32, ids: &[u32]) -> bool {
    let first = r32(iid);
    first == 0 && r32(iid + 4) == 0 && r32(iid + 8) == 0xc0 && r32(iid + 12) == 0x4600_0000 || ids.contains(&first) && first != 0
}
fn qi(c: &mut Ctx, ids: &[u32]) -> u32 {
    let (b, iid, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    if iid == 0 || !iid_ok(iid, ids) {
        if crate::env_on!("SAKE_TRACE_COM") {
            eprintln!("sake: amstream QueryInterface {:08x}: no", r32(iid));
        }
        putp(out, 0);
        return E_NOINTERFACE;
    }
    refs(b, 1);
    putp(out, b);
    0
}
fn add_ref(c: &mut Ctx) -> u32 {
    refs(c.a(0), 1)
}
fn release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let n = refs(b, -1);
    if n == 0 {
        // (a multimedia stream's movie stops; the blocks stay, a game may hold them still)
        if mms(b, |x| x.playing = false).is_some() {
            play::end(b);
        }
    }
    n
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn ok(_: &mut Ctx) -> u32 {
    0
}

// ---------------------------------------------------------------------------------------------
// IAMMultiMediaStream

const MMS_IDS: [u32; 2] = [0xb502_d1bc, 0xbebe_595c];
const DDSTREAM_IDS: [u32; 2] = [0xb502_d1bd, 0xf410_4fce];
const AUDIOSTREAM_IDS: [u32; 2] = [0xb502_d1bd, 0xf753_7560];
const SAMPLE_IDS: [u32; 2] = [0xb502_d1be, 0xf410_4fcf];

fn mms_qi(c: &mut Ctx) -> u32 {
    qi(c, &MMS_IDS)
}
/// GetInformation(DWORD *flags, STREAM_TYPE *): a clock, seeking; read
fn mms_info(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        w32(c.a(1), 1 | 4); // MMSSF_HASCLOCK | MMSSF_SUPPORTSEEK
    }
    if c.a(2) != 0 {
        w32(c.a(2), 0); // STREAMTYPE_READ
    }
    0
}
/// The stream of purpose `id` (VIDEO / AUDIO) of multimedia stream `b`.
fn stream_of(b: u32, id: u32) -> u32 {
    mms(b, |x| match id {
        VIDEO => x.video,
        AUDIO => x.audio,
        _ => 0,
    })
    .unwrap_or(0)
}
fn purpose(p: u32) -> Option<u32> {
    let tail: [u8; 12] = mem::slice(p + 4, 12).try_into().unwrap();
    let id = r32(p);
    (tail == MSPID_TAIL && matches!(id, VIDEO | AUDIO)).then_some(id)
}
/// GetMediaStream(REFMSPID, IMediaStream **)
fn mms_get_stream(c: &mut Ctx) -> u32 {
    let (b, id, out) = (c.a(0), c.a(1), c.a(2));
    if id == 0 || out == 0 {
        return E_POINTER;
    }
    let s = purpose(id).map_or(0, |id| stream_of(b, id));
    if s == 0 {
        putp(out, 0);
        return MS_E_NOSTREAM;
    }
    refs(s, 1);
    putp(out, s);
    0
}
/// EnumMediaStreams(long index, IMediaStream **)
fn mms_enum(c: &mut Ctx) -> u32 {
    let (b, i, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let all: Vec<u32> = mms(b, |x| [x.video, x.audio].into_iter().filter(|&s| s != 0).collect()).unwrap_or_default();
    match all.get(i as usize) {
        Some(&s) => {
            refs(s, 1);
            putp(out, s);
            0
        }
        None => {
            putp(out, 0);
            S_FALSE
        }
    }
}
fn mms_get_state(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), mms(c.a(0), |x| x.state).unwrap_or(0));
    0
}
fn set_state(b: u32, s: u32) {
    let t = now();
    let Some(playing) = mms(b, |x| {
        x.state = s;
        x.playing
    }) else {
        return;
    };
    if playing {
        play::with(|m| {
            if let Some(p) = m.get_mut(&b) {
                if s == 1 { p.tl.run(t) } else { p.tl.pause(t) }
            }
        });
    }
}
/// SetState(STREAM_STATE)
fn mms_set_state(c: &mut Ctx) -> u32 {
    let s = c.a(1);
    if s > 1 {
        return E_INVALIDARG;
    }
    set_state(c.a(0), s);
    0
}
fn position(b: u32) -> u64 {
    let t = now();
    play::read(|m| m.get(&b).map(|p| p.tl.position(t))).unwrap_or(0)
}
/// GetTime(STREAM_TIME *)
fn mms_time(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put_time(c.a(1), position(c.a(0)));
    0
}
/// GetDuration(STREAM_TIME *): S_FALSE before a file is open
fn mms_duration(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let (open, d) = mms(c.a(0), |x| (x.open, x.duration)).unwrap_or((false, 0));
    put_time(c.a(1), d);
    if open { 0 } else { S_FALSE }
}
/// Seek(STREAM_TIME)
fn mms_seek(c: &mut Ctx) -> u32 {
    let (b, to) = (c.a(0), time_arg(c, 1));
    let t = now();
    if let Some(eos) = mms(b, |x| x.eos) {
        crate::k32::event_set(eos, false);
    }
    play::with(|m| m.get_mut(&b).map(|p| p.tl.seek(t, to)));
    objs(|m| {
        for o in m.values_mut() {
            if let Obj::Sample(s) = o {
                s.frame = None;
            }
        }
    });
    0
}
fn mms_eos_handle(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), mms(c.a(0), |x| x.eos).unwrap_or(0));
    0
}
/// GetFilter: none to hand out (games that only stream a file do not ask)
fn mms_get_null(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), 0);
    E_NOTIMPL
}
/// Initialize(STREAM_TYPE, DWORD flags, IGraphBuilder *): the graph given is the one it uses
fn mms_initialize(c: &mut Ctx) -> u32 {
    let (b, graph) = (c.a(0), c.a(3));
    if graph != 0 && mms(b, |x| x.graph).unwrap_or(0) == 0 {
        add_ref_guest(c, graph);
        mms(b, |x| x.graph = graph);
    }
    0
}
/// GetFilterGraph(IGraphBuilder **): the graph it streams with (ShiinaRio adds a renderer of
/// its own to it, through IFilterGraph)
fn mms_get_graph(c: &mut Ctx) -> u32 {
    let (b, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let mut g = mms(b, |x| x.graph).unwrap_or(0);
    if g == 0 {
        // CLSID_FilterGraph, IID_IGraphBuilder
        let ids = mem::heap_alloc(32, true);
        w32(ids, 0xe436_ebb3);
        w32(ids + 4, 0x11ce_524f);
        w32(ids + 16, 0x56a8_68a9);
        g = crate::dshow::create_instance(ids, ids + 16).unwrap_or(0);
        mem::heap_free(ids);
        mms(b, |x| x.graph = g);
    }
    add_ref_guest(c, g);
    putp(out, g);
    if g == 0 { E_NOTIMPL } else { 0 }
}

fn new_stream(c: &mut Ctx, b: u32, id: u32, dd: u32) -> u32 {
    let s = mem::heap_alloc(2 * ps(), true);
    wp(s, if id == VIDEO { vtables()[1] } else { vtables()[2] });
    add_ref_guest(c, dd);
    objs(|m| m.insert(s, Obj::Stream(Stream { refs: 1, mms: b, purpose: id, dd, pf: None })));
    s
}
/// AddMediaStream(IUnknown *stream_object, const MSPID *, DWORD flags, IMediaStream **)
fn mms_add_stream(c: &mut Ctx) -> u32 {
    let (b, obj, id, flags, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if out != 0 {
        putp(out, 0);
    }
    if id == 0 {
        return E_POINTER;
    }
    let Some(id) = purpose(id) else { return MS_E_PURPOSEID };
    if stream_of(b, id) != 0 {
        return MS_E_PURPOSEID;
    }
    if id == AUDIO && flags & AMMSF_ADDDEFAULTRENDERER != 0 {
        mms(b, |x| x.sound = true);
        return 0;
    }
    // (a video stream's object is the IDirectDraw its surfaces come from)
    let s = new_stream(c, b, id, if id == VIDEO { obj } else { 0 });
    mms(b, |x| if id == VIDEO { x.video = s } else { x.audio = s });
    if out != 0 {
        refs(s, 1);
        putp(out, s);
    }
    0
}
/// OpenFile(LPCWSTR, DWORD flags)
fn mms_open_file(c: &mut Ctx) -> u32 {
    let (b, name, flags) = (c.a(0), c.a(1), c.a(2));
    if name == 0 {
        return E_POINTER;
    }
    let w = String::from_utf16_lossy(&mem::wstr(name));
    play::end(b);
    let movie = play::Movie::find(&w);
    if movie.is_none() && crate::files::resolve(&w).is_none() {
        return VFW_E_NOT_FOUND;
    }
    let render = flags & 3;
    let sound = mms(b, |x| {
        if render == AMMSF_RENDERALLSTREAMS {
            x.sound = true;
        }
        x.sound
    })
    .unwrap_or(false);
    if let Some(eos) = mms(b, |x| x.eos) {
        crate::k32::event_set(eos, false);
    }
    match movie {
        Some(movie) => {
            eprintln!("sake: movie {w}: {}x{} {} frames (amstream)", movie.w, movie.h, movie.frames);
            mms(b, |x| {
                x.open = true;
                x.playing = true;
                x.size = (movie.w as i32, movie.h as i32);
                x.rate = movie.rate;
                x.duration = movie.duration;
            });
            let mut p = Play::new(movie);
            p.place.visible = false;
            if !sound || render == AMMSF_NORENDER {
                p.volume = -10000;
            }
            play::start(b, p);
        }
        None => {
            eprintln!("sake: movie {w}: not played (no .vnv to decode, see masu pack)");
            mms(b, |x| {
                x.open = true;
                x.playing = false;
                x.size = (640, 480);
                x.rate = (30, 1);
                x.duration = 0;
            });
        }
    }
    if flags & AMMSF_RUN != 0 {
        set_state(b, 1);
    }
    0
}

// ---------------------------------------------------------------------------------------------
// IMediaStream, IDirectDrawMediaStream

fn ddstream_qi(c: &mut Ctx) -> u32 {
    qi(c, &DDSTREAM_IDS)
}
fn audiostream_qi(c: &mut Ctx) -> u32 {
    qi(c, &AUDIOSTREAM_IDS)
}
/// GetMultiMediaStream(IMultiMediaStream **)
fn st_get_mms(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let b = stream(c.a(0), |s| s.mms).unwrap_or(0);
    refs(b, 1);
    putp(c.a(1), b);
    0
}
/// GetInformation(MSPID *, STREAM_TYPE *)
fn st_info(c: &mut Ctx) -> u32 {
    let id = stream(c.a(0), |s| s.purpose).unwrap_or(VIDEO);
    if c.a(1) != 0 {
        w32(c.a(1), id);
        mem::write(c.a(1) + 4, &MSPID_TAIL);
    }
    if c.a(2) != 0 {
        w32(c.a(2), 0);
    }
    0
}
/// The size of the stream's frames (its multimedia stream's movie).
fn frame_size(s: u32) -> (i32, i32) {
    let b = stream(s, |x| x.mms).unwrap_or(0);
    mms(b, |x| x.size).unwrap_or((0, 0))
}
/// A DDSURFACEDESC of the frames at `p`: their size, the pixel format set or the display's.
fn write_format(s: u32, p: u32) {
    let (w, h) = frame_size(s);
    let size = if r32(p) == 124 { 124 } else { 108 };
    mem::fill(p, size, 0);
    w32(p, size);
    w32(p + 4, 0x1 | 0x2 | 0x4 | 0x1000); // caps height width pixelformat
    w32(p + 8, h as u32);
    w32(p + 12, w as u32);
    match stream(s, |x| x.pf).flatten() {
        Some(pf) => mem::write(p + 72, &pf),
        None => crate::ddraw::write_display_pf(p + 72),
    }
    w32(p + 104, 0x40 | 0x800); // DDSCAPS_OFFSCREENPLAIN | DDSCAPS_SYSTEMMEMORY
}
/// GetFormat(DDSURFACEDESC *current, IDirectDrawPalette **, DDSURFACEDESC *desired, DWORD *flags)
fn dd_get_format(c: &mut Ctx) -> u32 {
    let s = c.a(0);
    let b = stream(s, |x| x.mms).unwrap_or(0);
    if !mms(b, |x| x.open).unwrap_or(false) {
        return MS_E_NOSTREAM;
    }
    for p in [c.a(1), c.a(3)] {
        if p != 0 {
            write_format(s, p);
        }
    }
    if c.a(2) != 0 {
        putp(c.a(2), 0);
    }
    if c.a(4) != 0 {
        w32(c.a(4), 0);
    }
    0
}
/// SetFormat(const DDSURFACEDESC *, IDirectDrawPalette *): its pixel format kept
fn dd_set_format(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return E_POINTER;
    }
    if r32(p + 4) & 0x1000 != 0 {
        let pf: [u8; 32] = mem::slice(p + 72, 32).try_into().unwrap();
        stream(c.a(0), |x| x.pf = Some(pf));
    }
    0
}
fn dd_of(s: u32) -> u32 {
    let dd = stream(s, |x| x.dd).unwrap_or(0);
    if dd != 0 {
        return dd;
    }
    let dd = crate::ddraw::new_direct_draw();
    stream(s, |x| x.dd = dd);
    dd
}
/// GetDirectDraw(IDirectDraw **)
fn dd_get_dd(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let dd = dd_of(c.a(0));
    add_ref_guest(c, dd);
    putp(c.a(1), dd);
    0
}
/// SetDirectDraw(IDirectDraw *)
fn dd_set_dd(c: &mut Ctx) -> u32 {
    let (s, dd) = (c.a(0), c.a(1));
    add_ref_guest(c, dd);
    stream(s, |x| x.dd = dd);
    0
}
/// CreateSample(IDirectDrawSurface *, const RECT *, DWORD flags, IDirectDrawStreamSample **)
/// (no surface: one made at the frames' size with the stream's DirectDraw)
fn dd_create_sample(c: &mut Ctx) -> u32 {
    let (s, mut surf, rect, out) = (c.a(0), c.a(1), c.a(2), c.a(4));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    if surf == 0 {
        let dd = dd_of(s);
        let desc = mem::heap_alloc(108 + 8, true);
        w32(desc, 108);
        write_format(s, desc);
        let slot = desc + 108;
        let hr = com(c, dd, 6, &[desc, slot, 0]); // IDirectDraw::CreateSurface
        surf = rp(slot);
        mem::heap_free(desc);
        if hr != 0 || surf == 0 {
            return if hr != 0 { hr } else { E_FAIL };
        }
    } else {
        add_ref_guest(c, surf);
    }
    let r = if rect != 0 {
        (r32(rect) as i32, r32(rect + 4) as i32, r32(rect + 8) as i32, r32(rect + 12) as i32)
    } else {
        let (w, h) = crate::ddraw::surface_memory(surf).map_or(frame_size(s), |m| (m.0, m.1));
        (0, 0, w, h)
    };
    let b = mem::heap_alloc(2 * ps(), true);
    wp(b, vtables()[3]);
    refs(s, 1);
    objs(|m| m.insert(b, Obj::Sample(Sample { refs: 1, stream: s, surface: surf, rect: r, frame: None, status: 0 })));
    putp(out, b);
    0
}
/// AllocateSample(DWORD flags, IStreamSample **)
fn dd_allocate(c: &mut Ctx) -> u32 {
    let (s, out) = (c.a(0), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let tmp = mem::heap_alloc(8, true);
    let mut args = vec![0, 0, 0, tmp];
    let f = rp(rp(s) + ps() * 13);
    args.insert(0, s);
    let hr = c.call(f, &args);
    putp(out, rp(tmp));
    mem::heap_free(tmp);
    hr
}
/// GetTimePerFrame(STREAM_TIME *)
fn dd_time_per_frame(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let b = stream(c.a(0), |x| x.mms).unwrap_or(0);
    let Some((open, (n, d))) = mms(b, |x| (x.open, x.rate)) else { return MS_E_NOSTREAM };
    if !open || n == 0 {
        return MS_E_NOSTREAM;
    }
    put_time(c.a(1), SEC * d as u64 / n as u64);
    0
}

// ---------------------------------------------------------------------------------------------
// IStreamSample, IDirectDrawStreamSample

fn sample_qi(c: &mut Ctx) -> u32 {
    qi(c, &SAMPLE_IDS)
}
fn sm_get_stream(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let s = sample(c.a(0), |x| x.stream).unwrap_or(0);
    refs(s, 1);
    putp(c.a(1), s);
    0
}
/// GetSampleTimes(STREAM_TIME *start, *end, *current)
fn sm_times(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    let Some((s, f)) = sample(b, |x| (x.stream, x.frame)) else { return E_FAIL };
    let m = stream(s, |x| x.mms).unwrap_or(0);
    let (n, d) = mms(m, |x| x.rate).unwrap_or((30, 1));
    let per = if n == 0 { 0 } else { SEC * d as u64 / n as u64 };
    let start = f.unwrap_or(0) as u64 * per;
    for (p, v) in [(c.a(1), start), (c.a(2), start + per), (c.a(3), position(m))] {
        if p != 0 {
            put_time(p, v);
        }
    }
    0
}

/// `rgb` (0x00rrggbb) as a pixel of a format with these red, green and blue masks.
pub fn pack(rgb: u32, masks: [u32; 3]) -> u32 {
    let mut v = 0;
    for (i, &m) in masks.iter().enumerate() {
        if m == 0 {
            continue;
        }
        let c = (rgb >> (16 - 8 * i)) & 0xff;
        let shift = m.trailing_zeros();
        let bits = (m >> shift).count_ones().min(32);
        let c = if bits >= 8 { c << (bits - 8) } else { c >> (8 - bits) };
        v |= (c << shift) & m;
    }
    v
}

/// Frame `px` (w x h, 0x00rrggbb) into the `dst` rectangle of a surface (stretched to it,
/// nearest), as pixels of `bpp` bits with these masks, rows `pitch` bytes apart.
pub fn draw(px: &[u32], w: u32, h: u32, out: &mut [u8], pitch: usize, bpp: u16, masks: [u32; 3], dst: (i32, i32, i32, i32), surf: (i32, i32)) {
    let (l, t, r, b) = (dst.0.max(0), dst.1.max(0), dst.2.min(surf.0), dst.3.min(surf.1));
    let (dw, dh) = (dst.2 - dst.0, dst.3 - dst.1);
    if r <= l || b <= t || dw <= 0 || dh <= 0 || w == 0 || h == 0 {
        return;
    }
    let bytes = (bpp / 8) as usize;
    for y in t..b {
        let sy = ((y - dst.1) as u64 * h as u64 / dh as u64) as usize;
        let row = &px[sy * w as usize..][..w as usize];
        let o = y as usize * pitch;
        for x in l..r {
            let sx = ((x - dst.0) as u64 * w as u64 / dw as u64) as usize;
            let v = pack(row[sx], masks);
            let at = o + x as usize * bytes;
            if let Some(d) = out.get_mut(at..at + bytes) {
                d.copy_from_slice(&v.to_le_bytes()[..bytes]);
            }
        }
    }
}

/// Where Update is: the frame to show now, or the end.
#[derive(Debug, PartialEq)]
pub enum Next {
    Frame(u32),
    /// a frame shown already: the next one's time is this far off
    Wait(u64),
    End,
}
/// What a sample holding `have` shows at `pos` of a movie of `frames` frames at `rate` that
/// stops at `stop`.
pub fn next(pos: u64, stop: u64, rate: (u32, u32), frames: u32, have: Option<u32>) -> Next {
    if pos >= stop || frames == 0 || rate.0 == 0 {
        return Next::End;
    }
    let n = frame_at(pos, rate, frames);
    if have != Some(n) {
        return Next::Frame(n);
    }
    if n + 1 >= frames {
        // (the last frame shown: the end is at the stop)
        return Next::Wait(stop - pos);
    }
    let at = ((n as u128 + 1) * rate.1 as u128 * SEC as u128).div_ceil(rate.0 as u128) as u64;
    Next::Wait(at.saturating_sub(pos).max(1))
}

/// Update(DWORD flags, HANDLE event, PAPCFUNC, DWORD_PTR): the next frame into the surface
/// (waiting for its time), MS_S_ENDOFSTREAM at the end. Asynchronous ones are done at once too
/// (MS_S_PENDING, the event set).
fn sm_update(c: &mut Ctx) -> u32 {
    let (b, flags, event) = (c.a(0), c.a(1), c.a(2));
    let Some((s, surf, rect)) = sample(b, |x| (x.stream, x.surface, x.rect)) else { return E_FAIL };
    let m = stream(s, |x| x.mms).unwrap_or(0);
    let Some((state, playing, eos)) = mms(m, |x| (x.state, x.playing, x.eos)) else { return E_FAIL };
    if state != 1 {
        return MS_E_NOTRUNNING;
    }
    let hr = if !playing { MS_S_ENDOFSTREAM } else { update(c, b, m, surf, rect) };
    if hr == MS_S_ENDOFSTREAM {
        crate::k32::event_set(eos, true);
    }
    sample(b, |x| x.status = hr);
    if flags & SSUPDATE_ASYNC != 0 {
        if event != 0 {
            crate::k32::event_set(event, true);
        }
        return MS_S_PENDING;
    }
    hr
}
fn update(c: &mut Ctx, b: u32, m: u32, surf: u32, rect: (i32, i32, i32, i32)) -> u32 {
    loop {
        let t = now();
        let Some((pos, stop, rate, frames, stream, w, h)) = play::read(|x| {
            x.get(&m).map(|p| (p.tl.position(t), p.tl.stop, p.movie.rate, p.movie.frames, p.movie.stream, p.movie.w, p.movie.h))
        }) else {
            return MS_S_ENDOFSTREAM;
        };
        let have = sample(b, |x| x.frame).flatten();
        match next(pos, stop, rate, frames, have) {
            Next::End => return MS_S_ENDOFSTREAM,
            Next::Wait(d) => kura::snap::sleep(std::time::Duration::from_nanos(d.min(SEC) * 100)),
            Next::Frame(n) => {
                let t0 = std::time::Instant::now();
                let mut px = vec![0u32; (w * h) as usize];
                let mut tries = 0;
                while !unsafe { play::frame(stream, n as i32, px.as_mut_ptr() as *mut u8, 4 * w as usize, w, h) } {
                    tries += 1;
                    if tries > FRAME_TRIES || play::failed(stream) {
                        return E_FAIL;
                    }
                    kura::snap::sleep(std::time::Duration::from_millis(5));
                }
                sample(b, |x| x.frame = Some(n));
                let t1 = std::time::Instant::now();
                let Some((sw, sh, bpp, pf, pitch, bits)) = crate::ddraw::surface_memory(surf) else { return 0 };
                if !matches!(bpp, 16 | 24 | 32) {
                    return 0;
                }
                // (kura's Rgb601 frames are bytes r, g, b, a: as 0x00rrggbb)
                let rgb: Vec<u32> = px.iter().map(|&p| {
                    let [r, g, bl, _] = p.to_le_bytes();
                    (r as u32) << 16 | (g as u32) << 8 | bl as u32
                }).collect();
                let len = pitch as usize * sh as usize;
                let mut out = mem::slice(bits, len as u32).to_vec();
                draw(&rgb, w, h, &mut out, pitch as usize, bpp, [pf[1], pf[2], pf[3]], rect, (sw, sh));
                mem::write(bits, &out);
                crate::ddraw::drawn(c, surf, crate::user::Rect { l: rect.0, t: rect.1, r: rect.2, b: rect.3 });
                play::stats(m, n, [t1 - t0, t1.elapsed(), std::time::Duration::ZERO], px.len());
                return 0;
            }
        }
    }
}
/// CompletionStatus(DWORD flags, DWORD milliseconds): what the last Update came to
fn sm_status(c: &mut Ctx) -> u32 {
    sample(c.a(0), |x| x.status).unwrap_or(E_FAIL)
}
/// GetSurface(IDirectDrawSurface **, RECT *)
fn sm_surface(c: &mut Ctx) -> u32 {
    let Some((surf, r)) = sample(c.a(0), |x| (x.surface, x.rect)) else { return E_FAIL };
    if c.a(1) != 0 {
        add_ref_guest(c, surf);
        putp(c.a(1), surf);
    }
    if c.a(2) != 0 {
        for (i, v) in [r.0, r.1, r.2, r.3].into_iter().enumerate() {
            w32(c.a(2) + 4 * i as u32, v as u32);
        }
    }
    0
}
/// SetRect(const RECT *)
fn sm_set_rect(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return E_POINTER;
    }
    let r = (r32(p) as i32, r32(p + 4) as i32, r32(p + 8) as i32, r32(p + 12) as i32);
    sample(c.a(0), |x| x.rect = r);
    0
}

/// The movie thread came to a multimedia stream's end: its end-of-stream event is set.
pub fn ended(b: u32) -> bool {
    match mms(b, |x| x.eos) {
        Some(eos) => {
            crate::k32::event_set(eos, true);
            true
        }
        None => false,
    }
}

fn vtables() -> &'static [u32; 4] {
    static V: OnceLock<[u32; 4]> = OnceLock::new();
    V.get_or_init(|| {
        let t = 2 * ps().min(4); // a STREAM_TIME on the stack (x86: 8 bytes)
        let mms: Vec<(Handler, u32)> = vec![
            (mms_qi as Handler, 12),
            (add_ref, 4),
            (release, 4),
            (mms_info, 12),
            (mms_get_stream, 12),
            (mms_enum, 12),
            (mms_get_state, 8),
            (mms_set_state, 8),
            (mms_time, 8),
            (mms_duration, 8),
            (mms_seek, 4 + t),
            (mms_eos_handle, 8),
            (mms_initialize, 16),
            (mms_get_graph, 8),
            (mms_get_null, 8), // GetFilter
            (mms_add_stream, 20),
            (mms_open_file, 12),
            (notimpl, 16), // OpenMoniker
            (ok, 8),       // Render
        ];
        let media_stream = |q: Handler, alloc: Handler| -> Vec<(Handler, u32)> {
            vec![
                (q, 12),
                (add_ref as Handler, 4),
                (release, 4),
                (st_get_mms, 8),
                (st_info, 12),
                (notimpl, 12), // SetSameFormat
                (alloc, 12),
                (notimpl, 16), // CreateSharedSample
                (notimpl, 8),  // SendEndOfStream
            ]
        };
        let mut dd = media_stream(ddstream_qi, dd_allocate);
        dd.extend([
            (dd_get_format as Handler, 20),
            (dd_set_format, 12),
            (dd_get_dd, 8),
            (dd_set_dd, 8),
            (dd_create_sample, 20),
            (dd_time_per_frame, 8),
        ]);
        let mut au = media_stream(audiostream_qi, notimpl);
        au.extend([(notimpl as Handler, 8), (notimpl, 8), (notimpl, 16)]); // GetFormat SetFormat CreateSample
        let sm: Vec<(Handler, u32)> = vec![
            (sample_qi as Handler, 12),
            (add_ref, 4),
            (release, 4),
            (sm_get_stream, 8),
            (sm_times, 16),
            (notimpl, 12), // SetSampleTimes
            (sm_update, 20),
            (sm_status, 12),
            (sm_surface, 12),
            (sm_set_rect, 8),
        ];
        [vtable("IAMMultiMediaStream", &mms), vtable("IDirectDrawMediaStream", &dd), vtable("IAudioMediaStream", &au), vtable("IDirectDrawStreamSample", &sm)]
    })
}

/// CoCreateInstance(CLSID_AMMultiMediaStream {49c47ce5-9ba4-11d0-8212-00c04fc32c45}, ...)
pub fn create_instance(clsid: u32, iid: u32) -> Option<u32> {
    if r32(clsid) != 0x49c4_7ce5 || r32(clsid + 4) != 0x11d0_9ba4 {
        return None;
    }
    if iid != 0 && !iid_ok(iid, &MMS_IDS) {
        return Some(0);
    }
    let b = mem::heap_alloc(2 * ps(), true);
    wp(b, vtables()[0]);
    let eos = crate::obj::add(crate::obj::Obj::Event { manual: true, set: false });
    let m = Mms { refs: 1, video: 0, audio: 0, sound: false, open: false, playing: false, size: (0, 0), rate: (0, 1), duration: 0, state: 0, eos, graph: 0 };
    objs(|o| o.insert(b, Obj::Mms(m)));
    Some(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn pack_formats() {
        assert_eq!(pack(0x12_34_56, [0xff0000, 0xff00, 0xff]), 0x12_34_56);
        // 565: 0xff -> 31 / 63 / 31
        assert_eq!(pack(0xff_ff_ff, [0xf800, 0x07e0, 0x001f]), 0xffff);
        assert_eq!(pack(0x80_40_20, [0xf800, 0x07e0, 0x001f]), (0x10 << 11) | (0x10 << 5) | 0x04);
        // 555
        assert_eq!(pack(0xff_00_00, [0x7c00, 0x03e0, 0x001f]), 0x7c00);
        // BGR order
        assert_eq!(pack(0x11_22_33, [0xff, 0xff00, 0xff0000]), 0x33_22_11);
    }

    proptest! {
        /// Each channel lands inside its mask and keeps its top bits.
        #[test]
        fn pack_keeps_top_bits(rgb in 0u32..0x100_0000, five in any::<bool>()) {
            let masks = if five { [0x7c00, 0x03e0, 0x001f] } else { [0xf800, 0x07e0, 0x001f] };
            let v = pack(rgb, masks);
            prop_assert_eq!(v & !(masks[0] | masks[1] | masks[2]), 0);
            for (i, m) in masks.iter().enumerate() {
                let bits = m.count_ones();
                let c = (rgb >> (16 - 8 * i)) & 0xff;
                prop_assert_eq!((v & m) >> m.trailing_zeros(), c >> (8 - bits));
            }
        }

        /// A frame drawn at its own size into 32-bit pixels is the frame.
        #[test]
        fn draw_same_size(w in 1u32..20, h in 1u32..20, seed in any::<u32>()) {
            let px: Vec<u32> = (0..w * h).map(|i| (i.wrapping_mul(2654435761) ^ seed) & 0xffffff).collect();
            let pitch = 4 * w as usize + 8;
            let mut out = vec![0u8; pitch * h as usize];
            draw(&px, w, h, &mut out, pitch, 32, [0xff0000, 0xff00, 0xff], (0, 0, w as i32, h as i32), (w as i32, h as i32));
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let o = y * pitch + 4 * x;
                    prop_assert_eq!(u32::from_le_bytes(out[o..o + 4].try_into().unwrap()), px[y * w as usize + x]);
                }
            }
        }

        /// Drawing never writes outside the rectangle, clipped to the surface.
        #[test]
        fn draw_stays_inside(l in -5i32..15, t in -5i32..15, dw in 1i32..20, dh in 1i32..20) {
            let (sw, sh) = (12, 10);
            let pitch = 2 * sw as usize;
            let mut out = vec![0u8; pitch * sh as usize];
            draw(&[0xffffff; 4], 2, 2, &mut out, pitch, 16, [0xf800, 0x07e0, 0x001f], (l, t, l + dw, t + dh), (sw, sh));
            for y in 0..sh {
                for x in 0..sw {
                    let o = y as usize * pitch + 2 * x as usize;
                    let inside = x >= l && x < l + dw && y >= t && y < t + dh;
                    prop_assert_eq!(out[o] != 0, inside);
                }
            }
        }

        /// Frames come in order, each once, and the end comes at the stop.
        #[test]
        fn next_in_order(frames in 1u32..50, fps in 1u32..60, step in 1u64..2_000_000) {
            let rate = (fps, 1);
            let stop = frames as u64 * SEC / fps as u64;
            let (mut pos, mut have, mut seen) = (0u64, None, Vec::new());
            loop {
                match next(pos, stop, rate, frames, have) {
                    Next::End => break,
                    Next::Frame(n) => { seen.push(n); have = Some(n); }
                    Next::Wait(d) => { prop_assert!(d > 0); pos += d.min(step); }
                }
            }
            prop_assert!(pos >= stop);
            prop_assert!(seen.windows(2).all(|w| w[0] < w[1]));
            prop_assert_eq!(seen[0], 0);
        }
    }

    #[test]
    fn next_waits_for_the_next_frame() {
        // 30 fps: frame 0 shown at 0, frame 1's time 333334 off
        assert_eq!(next(0, SEC, (30, 1), 30, None), Next::Frame(0));
        assert_eq!(next(0, SEC, (30, 1), 30, Some(0)), Next::Wait(333_334));
        assert_eq!(next(333_334, SEC, (30, 1), 30, Some(0)), Next::Frame(1));
        assert_eq!(next(SEC, SEC, (30, 1), 30, Some(29)), Next::End);
        assert_eq!(next(0, 0, (30, 1), 30, None), Next::End);
    }
}
