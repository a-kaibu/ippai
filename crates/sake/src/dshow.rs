//! DirectShow's filter graph (CLSID_FilterGraph) and the EVR, for games that play their movies
//! with RenderFile and Run.
//!
//! A movie with a `.vnv` next to it (`ippai pack` makes one of every mpg / wmv / avi / mp4 ...,
//! crates/ippai/src/movie.rs) really plays (dshow_play.rs): its clock, seeking, EC_COMPLETE at
//! its end, its picture where the video window (IVideoWindow / IBasicVideo) or the EVR's
//! rectangle (IMFVideoDisplayControl) is, its sound. Any other "plays" to its end at once (Run
//! posts EC_COMPLETE), so games go on past their opening or logo movie.
//!
//! One guest block holds every interface of a graph: pointer-sized word k is interface k's
//! vtable (so an interface pointer is block + k * pointer size); the graph's state is in
//! `GRAPHS`, by block.

#![allow(non_snake_case)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use crate::api::{Ctx, Handler};
use crate::dshow_play::{self as play, now, Place, Play, SEC};
use crate::mem::{self, ps, put32, put64, putp, r32, w32, w64, wp};

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_ABORT: u32 = 0x8000_4004;
const E_INVALIDARG: u32 = 0x8007_0057;
const S_FALSE: u32 = 1;
const EC_COMPLETE: u32 = 1;
/// VFW_E_NOT_FOUND (RenderFile of a file not there, as Windows answers: tests/winref dshow)
const NOT_FOUND: u32 = 0x8004_0216;
/// EC_PAUSED: a Pause done (p1 S_OK)
const EC_PAUSED: u32 = 0x0e;
/// VFW_E_WRONG_STATE
const WRONG_STATE: u32 = 0x8004_0227;
/// AM_MEDIAEVENT_NONOTIFY: the event handle is not signalled
const NONOTIFY: u32 = 1;
const OATRUE: u32 = 0xffff_ffff;
/// what a video window's style is until the game sets one (WS_OVERLAPPEDWINDOW |
/// WS_CLIPSIBLINGS | WS_CLIPCHILDREN)
const DEFAULT_STYLE: u32 = 0x06cf_0000;

// interfaces, by slot
const GB: u32 = 0; // IGraphBuilder / IFilterGraph(2)
const MC: u32 = 1; // IMediaControl
const ME: u32 = 2; // IMediaEventEx
const MS: u32 = 3; // IMediaSeeking
const MP: u32 = 4; // IMediaPosition
const BA: u32 = 5; // IBasicAudio
const VW: u32 = 6; // IVideoWindow
const BV: u32 = 7; // IBasicVideo(2)
const NI: u32 = 8;

fn slot_of(iid: u32) -> Option<u32> {
    Some(match r32(iid) {
        0 | 0x56a8_68a9 | 0x56a8_689f | 0x36b7_3882 => GB,
        0x56a8_68b1 => MC,
        0x56a8_68b6 | 0x56a8_68c0 => ME,
        0x36b7_3880 => MS,
        0x56a8_68b2 => MP,
        0x56a8_68b3 => BA,
        0x56a8_68b4 => VW,
        0x56a8_68b5 | 0x329b_b360 => BV,
        _ => return None,
    })
}

/// A graph's state.
struct Graph {
    refs: u32,
    event: u32,
    notify: (u32, u32, u32),
    flags: u32,
    events: VecDeque<(u32, u32, u32)>,
    /// 0 stopped, 1 paused, 2 running
    state: u32,
    /// no movie to play (none, or no `.vnv` for it): EC_COMPLETE went out since RenderFile (a
    /// Run again, of a looping movie rewound by SetPositions, then "plays" on without end)
    done: bool,
    /// EC_COMPLETE went out since it last ran from the start or was seeked (WaitForCompletion
    /// then answers VFW_E_WRONG_STATE, as Windows does)
    ended: bool,
    /// a movie plays (dshow_play's PLAYS holds it)
    playing: bool,
    /// the movie's picture size (1280x720 without a movie: CatSystem2 asks before it plays)
    video: (i32, i32),
    duration: u64,
    rate: (u32, u32),
    // the video window
    owner: u32,
    style: u32,
    style_ex: u32,
    window: (i32, i32, i32, i32),
    visible: bool,
    auto_show: bool,
    drain: u32,
    full_screen: u32,
    /// IBasicVideo's rectangles (None: the default, all of it)
    src: Option<(i32, i32, i32, i32)>,
    dst: Option<(i32, i32, i32, i32)>,
    volume: i32,
    balance: i32,
    /// the EVR added to it (its picture goes where the EVR says)
    evr: u32,
}

static GRAPHS: Mutex<Option<HashMap<u32, Graph>>> = Mutex::new(None);

fn with<R>(b: u32, f: impl FnOnce(&mut Graph) -> R) -> Option<R> {
    let mut g = GRAPHS.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}

/// The block an interface pointer of slot `k` belongs to.
fn base(c: &Ctx, k: u32) -> u32 {
    c.a(0) - ps() * k
}

fn qi_k(c: &mut Ctx, k: u32) -> u32 {
    let b = base(c, k);
    if c.a(2) == 0 {
        return E_POINTER;
    }
    match slot_of(c.a(1)) {
        Some(s) => {
            with(b, |g| g.refs += 1);
            putp(c.a(2), b + ps() * s);
            0
        }
        None => {
            if crate::env_on!("SAKE_TRACE_COM") {
                eprintln!("sake: FilterGraph QueryInterface {:08x}: no", r32(c.a(1)));
            }
            putp(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
fn add_ref_k(c: &mut Ctx, k: u32) -> u32 {
    with(base(c, k), |g| {
        g.refs += 1;
        g.refs
    })
    .unwrap_or(1)
}
fn release_k(c: &mut Ctx, k: u32) -> u32 {
    let b = base(c, k);
    let n = with(b, |g| {
        g.refs = g.refs.saturating_sub(1);
        g.refs
    })
    .unwrap_or(0);
    if n == 0 {
        // the movie stops and its window goes (the block and the graph's event stay: a game
        // may still hold the handle it got from us)
        play::end(b);
        with(b, |g| g.playing = false);
    }
    n
}

macro_rules! iunknown {
    ($k:expr) => {{
        fn q(c: &mut Ctx) -> u32 {
            qi_k(c, $k)
        }
        fn a(c: &mut Ctx) -> u32 {
            add_ref_k(c, $k)
        }
        fn r(c: &mut Ctx) -> u32 {
            release_k(c, $k)
        }
        [(q as Handler, 12), (a as Handler, 4), (r as Handler, 4)]
    }};
}

fn ok(_: &mut Ctx) -> u32 {
    0
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}

/// A double argument: x86 stack slots `slot`, `slot + 1`; x64 argument `arg` (its XMM register).
fn dbl(c: &Ctx, slot: u32, arg: u32) -> f64 {
    if mem::x64() { f64::from_bits(c.xmm(arg)) } else { c.f64(slot) }
}
fn put_dbl(p: u32, v: f64) -> u32 {
    if p == 0 {
        return E_POINTER;
    }
    w64(p, v.to_bits());
    0
}

/// IFilterGraph::EnumFilters: an IEnumFilters with nothing in it (there are no filters)
fn enum_filters(c: &mut Ctx) -> u32 {
    static E: OnceLock<u32> = OnceLock::new();
    let e = *E.get_or_init(|| {
        fn qi(c: &mut Ctx) -> u32 {
            let ok = r32(c.a(1)) == 0x56a8_6893 || r32(c.a(1)) == 0;
            putp(c.a(2), if ok { c.a(0) } else { 0 });
            if ok { 0 } else { E_NOINTERFACE }
        }
        fn one(_: &mut Ctx) -> u32 {
            1
        }
        fn next(c: &mut Ctx) -> u32 {
            put32(c.a(3), 0);
            1 // S_FALSE
        }
        let v = vtable("IEnumFilters", &[(qi, 12), (one, 4), (one, 4), (next, 16), (one, 8), (ok, 4), (notimpl, 8)]);
        let o = mem::heap_alloc(2 * ps(), true);
        wp(o, v);
        o
    });
    putp(c.a(1), e);
    0
}
/// a getter: zero its out parameter
fn get0(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    0
}
fn get0_64(c: &mut Ctx) -> u32 {
    put64(c.a(1), 0);
    0
}

// ---------------------------------------------------------------------------------------------
// IFilterGraph / IGraphBuilder

/// AddFilter(filter, name): an EVR added is where the picture goes
fn add_filter(c: &mut Ctx) -> u32 {
    let f = c.a(1);
    if f == 0 {
        return E_POINTER;
    }
    if evr_with(f, |_| ()).is_some() {
        with(base(c, GB), |g| g.evr = f);
    }
    0
}

/// A movie at guest path `w`: its `.vnv` played, or (no `.vnv`, or no page to decode it) none.
/// Err when the movie itself is not there either.
fn load(b: u32, w: &str) -> Result<(), u32> {
    play::end(b);
    let vnv = crate::files::resolve(&format!("{w}.vnv"));
    let movie = vnv.and_then(|p| play::Movie::open(&p)).filter(|m| kura::video::decodable(m.stream));
    if movie.is_none() && crate::files::resolve(w).is_none() {
        return Err(NOT_FOUND);
    }
    let Some(movie) = movie else {
        eprintln!("sake: movie {w}: not played (no .vnv to decode, see ippai pack)");
        with(b, |g| {
            g.done = false;
            g.playing = false;
            g.video = (1280, 720);
            g.duration = 0;
        });
        return Ok(());
    };
    eprintln!("sake: movie {w}: {}x{} {} frames", movie.w, movie.h, movie.frames);
    with(b, |g| {
        g.done = false;
        g.playing = true;
        g.video = (movie.w as i32, movie.h as i32);
        g.duration = movie.duration;
        g.rate = movie.rate;
        // (the video renderer's own window: its client area is at least 120 wide, Windows'
        // smallest overlapped window)
        g.window = (0, 0, (movie.w as i32).max(120), movie.h as i32);
        g.ended = false;
        g.src = None;
        g.dst = None;
        g.events.clear();
    });
    play::start(b, Play::new(movie));
    sync(b);
    Ok(())
}
fn render_file(b: u32, name: u32) -> u32 {
    if name == 0 {
        return E_POINTER;
    }
    let w = String::from_utf16_lossy(&mem::wstr(name));
    match load(b, &w) {
        Ok(()) => 0,
        Err(e) => e,
    }
}
fn render_file_gb(c: &mut Ctx) -> u32 {
    render_file(base(c, GB), c.a(1))
}
fn render_file_mc(c: &mut Ctx) -> u32 {
    render_file(base(c, MC), c.a(1))
}

// ---------------------------------------------------------------------------------------------
// IMediaControl

/// The video window shows itself when the graph pauses or runs (AutoShow). Returns the state
/// before.
fn set_state(b: u32, s: u32) -> u32 {
    let t = now();
    let (playing, was) = with(b, |g| {
        let was = std::mem::replace(&mut g.state, s);
        if s != 0 && g.auto_show {
            g.visible = true;
        }
        (g.playing, was)
    })
    .unwrap_or((false, 0));
    if playing {
        play::with(|m| {
            if let Some(p) = m.get_mut(&b) {
                match s {
                    0 => p.tl.stop(t),
                    1 => p.tl.pause(t),
                    _ => p.tl.run(t),
                }
            }
        });
        sync(b);
    }
    was
}

/// Without a movie, it ends as soon as it runs: queue EC_COMPLETE, signal the event, notify
/// the window (once a movie: see Graph::done).
/// From stopped it answers S_FALSE (Windows: the change is still under way), else S_OK.
fn run(c: &mut Ctx) -> u32 {
    let b = base(c, MC);
    let was = set_state(b, 2);
    let instant = with(b, |g| {
        if g.playing || g.done {
            return false;
        }
        g.done = true;
        true
    });
    if instant == Some(true) {
        completed(b);
    }
    if was == 0 { S_FALSE } else { 0 }
}
/// Queues EC_PAUSED; S_FALSE from stopped, as Run.
fn pause(c: &mut Ctx) -> u32 {
    let b = base(c, MC);
    let was = set_state(b, 1);
    queue(b, EC_PAUSED, 0);
    if was == 0 { S_FALSE } else { 0 }
}
fn stop(c: &mut Ctx) -> u32 {
    let b = base(c, MC);
    set_state(b, 0);
    // (without a movie, one run again after a stop plays to its end again)
    with(b, |g| {
        g.done = false;
        g.ended = false;
    });
    0
}
fn get_state(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return E_POINTER;
    }
    put32(c.a(2), with(base(c, MC), |g| g.state).unwrap_or(0));
    0
}

// ---------------------------------------------------------------------------------------------
// IMediaEventEx

/// The movie of graph `b` ended: EC_COMPLETE (from the movie thread too).
pub fn completed(b: u32) {
    with(b, |g| g.ended = true);
    queue(b, EC_COMPLETE, 0);
}

/// Queues an event: the event handle set, the notify window told.
fn queue(b: u32, code: u32, p1: u32) {
    let Some((ev, (hwnd, msg, lp), flags)) = with(b, |g| {
        g.events.push_back((code, p1, 0));
        (g.event, g.notify, g.flags)
    }) else {
        return;
    };
    if flags & NONOTIFY == 0 {
        crate::k32::event_set(ev, true);
    }
    if hwnd != 0 {
        crate::user::post(hwnd, msg, 0, lp);
    }
}

fn get_event_handle(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), with(base(c, ME), |g| g.event).unwrap_or(0));
    0
}
/// GetEvent(code, p1, p2, timeout ms): waits for one that long (INFINITE too)
fn get_event(c: &mut Ctx) -> u32 {
    let b = base(c, ME);
    let (pc, p1, p2, ms) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if pc == 0 || p1 == 0 || p2 == 0 {
        return E_POINTER;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_millis(if ms == u32::MAX { u64::MAX / 4 } else { ms as u64 });
    loop {
        let e = with(b, |g| {
            let e = g.events.pop_front();
            if g.events.is_empty() {
                crate::k32::event_set(g.event, false);
            }
            e
        })
        .flatten();
        if let Some((code, a, bb)) = e {
            put32(pc, code);
            putp(p1, a);
            putp(p2, bb);
            return 0;
        }
        if std::time::Instant::now() >= until {
            put32(pc, 0);
            putp(p1, 0);
            putp(p2, 0);
            return E_ABORT;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
/// WaitForCompletion(timeout, code): until EC_COMPLETE is queued (left in the queue); after one
/// was taken, VFW_E_WRONG_STATE (Windows)
fn wait_for_completion(c: &mut Ctx) -> u32 {
    let b = base(c, ME);
    let (ms, pc) = (c.a(1), c.a(2));
    if pc == 0 {
        return E_POINTER;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_millis(if ms == u32::MAX { u64::MAX / 4 } else { ms as u64 });
    loop {
        let (done, running) = with(b, |g| (g.events.iter().any(|e| e.0 == EC_COMPLETE), g.state == 2 && !g.ended)).unwrap_or((true, false));
        if done {
            put32(pc, EC_COMPLETE);
            return 0;
        }
        if !running {
            put32(pc, 0);
            return WRONG_STATE;
        }
        if std::time::Instant::now() >= until {
            put32(pc, 0);
            return E_ABORT;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
fn set_notify_window(c: &mut Ctx) -> u32 {
    let n = (c.a(1), c.a(2), c.a(3));
    with(base(c, ME), |g| g.notify = n);
    0
}
fn set_notify_flags(c: &mut Ctx) -> u32 {
    let f = c.a(1);
    if f > 1 {
        return E_INVALIDARG;
    }
    with(base(c, ME), |g| {
        g.flags = f;
        crate::k32::event_set(g.event, f == 0 && !g.events.is_empty());
    });
    0
}
fn get_notify_flags(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), with(base(c, ME), |g| g.flags).unwrap_or(0));
    0
}

// ---------------------------------------------------------------------------------------------
// IMediaSeeking (100 ns units: TIME_FORMAT_MEDIA_TIME only)

fn timeline<R>(b: u32, f: impl FnOnce(&mut play::Timeline, u64) -> R) -> Option<R> {
    let t = now();
    play::with(|m| m.get_mut(&b).map(|p| f(&mut p.tl, t)))
}
/// TIME_FORMAT_MEDIA_TIME {7b785574-8c82-11cf-bc0c-00aa00ac74f6}
fn media_time(p: u32) -> bool {
    p != 0 && r32(p) == 0x7b78_5574 && r32(p + 4) == 0x11cf_8c82 && r32(p + 8) == 0xaa00_0cbc && r32(p + 12) == 0xf674_ac00
}
fn write_media_time(p: u32) {
    for (i, v) in [0x7b78_5574u32, 0x11cf_8c82, 0xaa00_0cbc, 0xf674_ac00].into_iter().enumerate() {
        w32(p + 4 * i as u32, v);
    }
}
/// AM_SEEKING_CanSeekAbsolute | Forwards | Backwards | CanGetStopPos | CanGetDuration (not
/// CanGetCurrentPos: Windows' graph does not say so, though GetCurrentPosition works)
const CAPS: u32 = 0x37;
fn ms_caps(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), CAPS);
    0
}
fn ms_check_caps(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return E_POINTER;
    }
    let want = r32(p);
    put32(p, want & CAPS);
    if want & !CAPS == 0 {
        0
    } else if want & CAPS != 0 {
        S_FALSE
    } else {
        0x8000_4005
    }
}
fn ms_format_supported(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    if media_time(c.a(1)) { 0 } else { S_FALSE }
}
fn ms_get_format(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    write_media_time(c.a(1));
    0
}
fn ms_set_format(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    if media_time(c.a(1)) { 0 } else { E_INVALIDARG }
}
fn ms_duration(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w64(c.a(1), with(base(c, MS), |g| g.duration).unwrap_or(0));
    0
}
fn ms_stop_pos(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w64(c.a(1), timeline(base(c, MS), |tl, _| tl.stop).unwrap_or(0));
    0
}
fn ms_cur_pos(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w64(c.a(1), timeline(base(c, MS), |tl, t| tl.position(t)).unwrap_or(0));
    0
}
/// ConvertTimeFormat(target, target format, source (by value), source format): one format
fn ms_convert(c: &mut Ctx) -> u32 {
    let (out, tf) = (c.a(1), c.a(2));
    let (v, sf) = if mem::x64() { (c.a64(3), c.a(4)) } else { (c.a(3) as u64 | (c.a(4) as u64) << 32, c.a(5)) };
    if out == 0 {
        return E_POINTER;
    }
    if (tf != 0 && !media_time(tf)) || (sf != 0 && !media_time(sf)) {
        return E_INVALIDARG;
    }
    w64(out, v);
    0
}
/// SetPositions(cur, cur flags, stop, stop flags): absolute, relative or incremental (the
/// stop), AM_SEEKING_ReturnTime writes the times back
fn ms_set_positions(c: &mut Ctx) -> u32 {
    let b = base(c, MS);
    let (pc, fc, pst, fst) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let t = now();
    let r = play::with(|m| {
        let Some(p) = m.get_mut(&b) else { return None };
        let cur = p.tl.position(t);
        let rd = |a: u32| if a == 0 { 0 } else { mem::r64(a) as i64 };
        let to = match fc & 3 {
            1 => Some(rd(pc)),
            2 => Some(cur as i64 + rd(pc)),
            _ => None,
        };
        let stop = match fst & 3 {
            1 => Some(rd(pst)),
            2 => Some(cur as i64 + rd(pst)),
            3 => Some(to.unwrap_or(cur as i64) + rd(pst)),
            _ => None,
        };
        if let Some(s) = stop {
            p.tl.set_stop(t, s.max(0) as u64);
        }
        if let Some(to) = to {
            p.tl.seek(t, to.max(0) as u64);
        }
        Some((p.tl.position(t), p.tl.stop))
    });
    if let Some((cur, stop)) = r {
        // (a seek: an EC_COMPLETE of before is old news)
        if fc & 3 != 0 {
            with(b, |g| {
                g.ended = false;
                g.events.retain(|e| e.0 != EC_COMPLETE);
                if g.events.is_empty() {
                    crate::k32::event_set(g.event, false);
                }
            });
        }
        if fc & 8 != 0 && pc != 0 {
            w64(pc, cur);
        }
        if fst & 8 != 0 && pst != 0 {
            w64(pst, stop);
        }
    }
    0
}
fn ms_get_positions(c: &mut Ctx) -> u32 {
    let (cur, stop) = timeline(base(c, MS), |tl, t| (tl.position(t), tl.stop)).unwrap_or((0, 0));
    put64(c.a(1), cur);
    put64(c.a(2), stop);
    0
}
fn ms_available(c: &mut Ctx) -> u32 {
    let d = with(base(c, MS), |g| g.duration).unwrap_or(0);
    put64(c.a(1), 0);
    put64(c.a(2), d);
    0
}
fn set_rate(b: u32, r: f64) -> u32 {
    if r <= 0.0 || !r.is_finite() {
        return E_INVALIDARG;
    }
    timeline(b, |tl, t| tl.set_rate(t, r));
    0
}
fn ms_set_rate(c: &mut Ctx) -> u32 {
    let r = dbl(c, 1, 1);
    set_rate(base(c, MS), r)
}
fn ms_get_rate(c: &mut Ctx) -> u32 {
    put_dbl(c.a(1), timeline(base(c, MS), |tl, _| tl.rate).unwrap_or(1.0))
}

// ---------------------------------------------------------------------------------------------
// IMediaPosition (seconds as doubles)

fn secs(v: u64) -> f64 {
    v as f64 / SEC as f64
}
fn from_secs(s: f64) -> u64 {
    (s.max(0.0) * SEC as f64).round() as u64
}
fn mp_duration(c: &mut Ctx) -> u32 {
    put_dbl(c.a(1), secs(with(base(c, MP), |g| g.duration).unwrap_or(0)))
}
fn mp_put_cur(c: &mut Ctx) -> u32 {
    let b = base(c, MP);
    let s = dbl(c, 1, 1);
    timeline(b, |tl, t| tl.seek(t, from_secs(s)));
    with(b, |g| {
        g.ended = false;
        g.events.retain(|e| e.0 != EC_COMPLETE);
    });
    0
}
fn mp_get_cur(c: &mut Ctx) -> u32 {
    put_dbl(c.a(1), secs(timeline(base(c, MP), |tl, t| tl.position(t)).unwrap_or(0)))
}
fn mp_get_stop(c: &mut Ctx) -> u32 {
    put_dbl(c.a(1), secs(timeline(base(c, MP), |tl, _| tl.stop).unwrap_or(0)))
}
fn mp_put_stop(c: &mut Ctx) -> u32 {
    let s = dbl(c, 1, 1);
    timeline(base(c, MP), |tl, t| tl.set_stop(t, from_secs(s)));
    0
}
fn mp_get0(c: &mut Ctx) -> u32 {
    put_dbl(c.a(1), 0.0)
}
fn mp_put_rate(c: &mut Ctx) -> u32 {
    let r = dbl(c, 1, 1);
    set_rate(base(c, MP), r)
}
fn mp_get_rate(c: &mut Ctx) -> u32 {
    put_dbl(c.a(1), timeline(base(c, MP), |tl, _| tl.rate).unwrap_or(1.0))
}
fn mp_can_seek(c: &mut Ctx) -> u32 {
    put32(c.a(1), OATRUE);
    0
}

// ---------------------------------------------------------------------------------------------
// IBasicAudio

fn ba_put_volume(c: &mut Ctx) -> u32 {
    let v = c.i(1);
    if !(-10000..=0).contains(&v) {
        return E_INVALIDARG;
    }
    let b = base(c, BA);
    with(b, |g| g.volume = v);
    play::with(|m| {
        if let Some(p) = m.get_mut(&b) {
            p.volume = v;
        }
    });
    0
}
fn ba_get_volume(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), with(base(c, BA), |g| g.volume).unwrap_or(0) as u32);
    0
}
fn ba_put_balance(c: &mut Ctx) -> u32 {
    let v = c.i(1);
    if !(-10000..=10000).contains(&v) {
        return E_INVALIDARG;
    }
    with(base(c, BA), |g| g.balance = v);
    0
}
fn ba_get_balance(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), with(base(c, BA), |g| g.balance).unwrap_or(0) as u32);
    0
}

// ---------------------------------------------------------------------------------------------
// the picture's place: the video window (child of its owner, or on the screen), or the EVR's

/// Hands graph `b`'s video window (or EVR) settings to its movie.
fn sync(b: u32) {
    let Some((place, volume)) = with(b, |g| {
        let video = g.video;
        let src = g.src.unwrap_or((0, 0, video.0, video.1));
        if g.evr != 0 {
            if let Some(e) = evr_with(g.evr, |e| *e) {
                let dst = if e.dst.2 > 0 && e.dst.3 > 0 { e.dst } else { crate::user::client_rect_screen(e.hwnd).map_or((0, 0, 0, 0), |r| (0, 0, r.r - r.l, r.b - r.t)) };
                // (the source as fractions of the picture)
                let s = e.src;
                let src = ((s[0] * video.0 as f32) as i32, (s[1] * video.1 as f32) as i32, ((s[2] - s[0]) * video.0 as f32) as i32, ((s[3] - s[1]) * video.1 as f32) as i32);
                return (Place { hwnd: e.hwnd, dst, src, letterbox: e.mode & 1 != 0, visible: e.hwnd != 0 && (g.state != 0 || g.visible) }, g.volume);
            }
        }
        // the window's place (in its owner's client area, or on the screen), the picture in it
        let (x, y, w, h) = g.window;
        let d = g.dst.unwrap_or((0, 0, w, h));
        let dst = (x + d.0, y + d.1, d.2, d.3);
        let dst = play::clip(dst, (x, y, w, h));
        (Place { hwnd: g.owner, dst, src, letterbox: false, visible: g.visible }, g.volume)
    }) else {
        return;
    };
    play::with(|m| {
        if let Some(p) = m.get_mut(&b) {
            p.place = place;
            p.volume = volume;
        }
    });
}

/// IVideoWindow's properties: put_X(v) / get_X(*v), by index in the vtable's 15 pairs.
fn vw_prop(c: &mut Ctx, i: u32) -> u32 {
    let b = base(c, VW);
    let (put, prop) = if (26..30).contains(&i) { (i % 2 == 1, i / 2) } else { (i % 2 == 0, i / 2) };
    let v = c.a(1);
    if !put && v == 0 {
        return E_POINTER;
    }
    with(b, |g| {
        // (the u32 properties: a pointer-sized owner and drain)
        let field = match prop {
            1 => Some(&mut g.style),
            2 => Some(&mut g.style_ex),
            11 => Some(&mut g.owner),
            12 => Some(&mut g.drain),
            14 => Some(&mut g.full_screen),
            _ => None,
        };
        if let Some(f) = field {
            if put {
                *f = v;
            } else if prop == 11 || prop == 12 {
                putp(v, *f);
            } else {
                put32(v, *f);
            }
            return;
        }
        match prop {
            // Caption: a BSTR (none kept)
            0 => {
                if !put {
                    putp(v, 0);
                }
            }
            3 => {
                if put {
                    g.auto_show = v != 0;
                } else {
                    put32(v, if g.auto_show { OATRUE } else { 0 });
                }
            }
            // WindowState (SW_HIDE or a shown state)
            4 => {
                if put {
                    g.visible = v != 0;
                } else {
                    put32(v, g.visible as u32);
                }
            }
            6 => {
                if put {
                    g.visible = v != 0;
                } else {
                    put32(v, if g.visible { OATRUE } else { 0 });
                }
            }
            7..=10 => {
                let f = match prop {
                    7 => &mut g.window.0,
                    8 => &mut g.window.2,
                    9 => &mut g.window.1,
                    _ => &mut g.window.3,
                };
                if put {
                    *f = v as i32;
                } else {
                    put32(v, *f as u32);
                }
            }
            // BackgroundPalette, BorderColor
            _ => {
                if !put {
                    put32(v, 0);
                }
            }
        }
    });
    if put {
        sync(b);
    }
    0
}

fn vw_set_pos(c: &mut Ctx) -> u32 {
    let b = base(c, VW);
    let r = (c.i(1), c.i(2), c.i(3), c.i(4));
    with(b, |g| g.window = r);
    sync(b);
    0
}
fn vw_get_pos(c: &mut Ctx) -> u32 {
    let r = with(base(c, VW), |g| g.window).unwrap_or_default();
    for (i, v) in [r.0, r.1, r.2, r.3].into_iter().enumerate() {
        put32(c.a(1 + i as u32), v as u32);
    }
    0
}
fn vw_ideal(c: &mut Ctx) -> u32 {
    let v = with(base(c, VW), |g| g.video).unwrap_or_default();
    put32(c.a(1), v.0 as u32);
    put32(c.a(2), v.1 as u32);
    0
}
fn vw_owner_msg(_: &mut Ctx) -> u32 {
    0
}

// IBasicVideo

fn bv_avg_time(c: &mut Ctx) -> u32 {
    let r = with(base(c, BV), |g| if g.playing { g.rate } else { (0, 1) }).unwrap_or((0, 1));
    put_dbl(c.a(1), if r.0 == 0 { 0.0 } else { r.1 as f64 / r.0 as f64 })
}
fn bv_width(c: &mut Ctx) -> u32 {
    put32(c.a(1), with(base(c, BV), |g| g.video.0).unwrap_or(0) as u32);
    0
}
fn bv_height(c: &mut Ctx) -> u32 {
    put32(c.a(1), with(base(c, BV), |g| g.video.1).unwrap_or(0) as u32);
    0
}
/// put/get Source / Destination Left, Width, Top, Height (index 0..16)
fn bv_prop(c: &mut Ctx, i: u32) -> u32 {
    let b = base(c, BV);
    let (put, dest, f) = (i % 2 == 0, i >= 8, (i % 8) / 2);
    let v = c.a(1);
    if !put && v == 0 {
        return E_POINTER;
    }
    with(b, |g| {
        let full = if dest { (0, 0, g.window.2, g.window.3) } else { (0, 0, g.video.0, g.video.1) };
        let r = if dest { &mut g.dst } else { &mut g.src };
        let mut cur = r.unwrap_or(full);
        let field = match f {
            0 => &mut cur.0,
            1 => &mut cur.2,
            2 => &mut cur.1,
            _ => &mut cur.3,
        };
        if put {
            *field = v as i32;
            *r = Some(cur);
        } else {
            put32(v, *field as u32);
        }
    });
    if put {
        sync(b);
    }
    0
}
fn bv_set_rect(c: &mut Ctx, dest: bool) -> u32 {
    let b = base(c, BV);
    let r = (c.i(1), c.i(2), c.i(3), c.i(4));
    if r.2 <= 0 || r.3 <= 0 {
        return E_INVALIDARG;
    }
    with(b, |g| if dest { g.dst = Some(r) } else { g.src = Some(r) });
    sync(b);
    0
}
fn bv_get_rect(c: &mut Ctx, dest: bool) -> u32 {
    let r = with(base(c, BV), |g| if dest { g.dst.unwrap_or((0, 0, g.window.2, g.window.3)) } else { g.src.unwrap_or((0, 0, g.video.0, g.video.1)) }).unwrap_or_default();
    for (i, v) in [r.0, r.1, r.2, r.3].into_iter().enumerate() {
        put32(c.a(1 + i as u32), v as u32);
    }
    0
}
fn bv_default_rect(c: &mut Ctx, dest: bool) -> u32 {
    let b = base(c, BV);
    with(b, |g| if dest { g.dst = None } else { g.src = None });
    sync(b);
    0
}
fn bv_is_default(c: &mut Ctx, dest: bool) -> u32 {
    let d = with(base(c, BV), |g| if dest { g.dst.is_none() } else { g.src.is_none() }).unwrap_or(true);
    if d { 0 } else { S_FALSE }
}
fn bv_size(c: &mut Ctx) -> u32 {
    let v = with(base(c, BV), |g| g.video).unwrap_or_default();
    put32(c.a(1), v.0 as u32);
    put32(c.a(2), v.1 as u32);
    0
}
fn bv_aspect(c: &mut Ctx) -> u32 {
    let v = with(base(c, BV), |g| g.video).unwrap_or_default();
    put32(c.a(1), v.0 as u32);
    put32(c.a(2), v.1 as u32);
    0
}
/// GetCurrentImage(size, image): a BITMAPINFOHEADER and the frame shown, 32-bit bottom-up
fn bv_image(c: &mut Ctx) -> u32 {
    let b = base(c, BV);
    let (psize, img) = (c.a(1), c.a(2));
    if psize == 0 {
        return E_POINTER;
    }
    let t = now();
    let Some((stream, n, w, h)) = play::with(|m| m.get(&b).map(|p| (p.movie.stream, play::frame_at(p.tl.position(t), p.movie.rate, p.movie.frames), p.movie.w, p.movie.h))) else {
        return E_NOTIMPL;
    };
    let need = 40 + 4 * w * h;
    if img == 0 {
        put32(psize, need);
        return 0;
    }
    if r32(psize) < need {
        return E_INVALIDARG;
    }
    let mut px = vec![0u8; (4 * w * h) as usize];
    if !unsafe { kura::video::frame(stream, n as i32, px.as_mut_ptr(), 4 * w as usize, w, h, kura::video::Out::Rgb601) } {
        // E_UNEXPECTED
        return 0x8000_ffff;
    }
    mem::fill(img, 40, 0);
    w32(img, 40);
    w32(img + 4, w);
    w32(img + 8, h);
    mem::w16(img + 12, 1);
    mem::w16(img + 14, 32);
    w32(img + 20, 4 * w * h);
    for y in 0..h {
        let row = &px[(4 * w * (h - 1 - y)) as usize..][..(4 * w) as usize];
        mem::write(img + 40 + 4 * w * y, row);
    }
    put32(psize, need);
    0
}

const IDISPATCH: [(Handler, u32); 4] = [(notimpl, 8), (notimpl, 16), (notimpl, 24), (notimpl, 36)];

use crate::api::vtable;

macro_rules! vw_props {
    ($($i:literal)*) => {
        [$({
            fn f(c: &mut Ctx) -> u32 {
                vw_prop(c, $i)
            }
            (f as Handler, 8)
        }),*]
    };
}
macro_rules! bv_props {
    ($($i:literal)*) => {
        [$({
            fn f(c: &mut Ctx) -> u32 {
                bv_prop(c, $i)
            }
            (f as Handler, 8)
        }),*]
    };
}

fn vtables() -> &'static [u32; NI as usize] {
    static V: OnceLock<[u32; NI as usize]> = OnceLock::new();
    V.get_or_init(|| {
        let gb: Vec<(Handler, u32)> = iunknown!(GB)
            .into_iter()
            .chain([
                (add_filter as Handler, 12), // AddFilter
                (ok, 8),                     // RemoveFilter
                (enum_filters, 8),           // EnumFilters
                (notimpl, 12),               // FindFilterByName
                (ok, 16),                    // ConnectDirect
                (ok, 8),                     // Reconnect
                (ok, 8),                     // Disconnect
                (ok, 4),                     // SetDefaultSyncSource
                (ok, 12),                    // Connect
                (ok, 8),                     // Render
                (render_file_gb, 12),        // RenderFile
                (notimpl, 16),               // AddSourceFilter
                (ok, 8),                     // SetLogFile
                (ok, 4),                     // Abort
                (ok, 4),                     // ShouldOperationContinue
                // IFilterGraph2
                (notimpl, 20), // AddSourceFilterForMoniker
                (ok, 12),      // ReconnectEx
                (ok, 16),      // RenderEx
            ])
            .collect();
        let mc: Vec<(Handler, u32)> = iunknown!(MC)
            .into_iter()
            .chain(IDISPATCH)
            .chain([
                (run as Handler, 4),
                (pause, 4),
                (stop, 4),
                (get_state, 12),
                (render_file_mc, 8), // RenderFile
                (notimpl, 12),       // AddSourceFilter
                (notimpl, 8),        // get_FilterCollection
                (notimpl, 8),        // get_RegFilterCollection
                (pause, 4),          // StopWhenReady (paused, the picture shown)
            ])
            .collect();
        let me: Vec<(Handler, u32)> = iunknown!(ME)
            .into_iter()
            .chain(IDISPATCH)
            .chain([
                (get_event_handle as Handler, 8),
                (get_event, 20),
                (wait_for_completion, 12),
                (ok, 8),  // CancelDefaultHandling
                (ok, 8),  // RestoreDefaultHandling
                (ok, 16), // FreeEventParams
                (set_notify_window, 16),
                (set_notify_flags, 8),
                (get_notify_flags, 8),
            ])
            .collect();
        let ms: Vec<(Handler, u32)> = iunknown!(MS)
            .into_iter()
            .chain([
                (ms_caps as Handler, 8),
                (ms_check_caps, 8),
                (ms_format_supported, 8),
                (ms_get_format, 8), // QueryPreferredFormat
                (ms_get_format, 8), // GetTimeFormat
                (ms_format_supported, 8), // IsUsingTimeFormat
                (ms_set_format, 8),
                (ms_duration, 8),
                (ms_stop_pos, 8),
                (ms_cur_pos, 8),
                (ms_convert, 24),
                (ms_set_positions, 20),
                (ms_get_positions, 12),
                (ms_available, 12),
                (ms_set_rate, 12),
                (ms_get_rate, 8),
                (get0_64, 8), // GetPreroll
            ])
            .collect();
        let mp: Vec<(Handler, u32)> = iunknown!(MP)
            .into_iter()
            .chain(IDISPATCH)
            .chain([
                (mp_duration as Handler, 8),
                (mp_put_cur, 12),
                (mp_get_cur, 8),
                (mp_get_stop, 8),
                (mp_put_stop, 12),
                (mp_get0, 8), // get_PrerollTime
                (ok, 12),     // put_PrerollTime
                (mp_put_rate, 12),
                (mp_get_rate, 8),
                (mp_can_seek, 8),
                (mp_can_seek, 8),
            ])
            .collect();
        let ba: Vec<(Handler, u32)> =
            iunknown!(BA).into_iter().chain(IDISPATCH).chain([(ba_put_volume as Handler, 8), (ba_get_volume, 8), (ba_put_balance, 8), (ba_get_balance, 8)]).collect();
        let mut vw: Vec<(Handler, u32)> = iunknown!(VW).into_iter().chain(IDISPATCH).collect();
        vw.extend(vw_props!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29));
        vw.extend([
            (ok as Handler, 8), // SetWindowForeground
            (vw_owner_msg, 20), // NotifyOwnerMessage
            (vw_set_pos, 20),   // SetWindowPosition
            (vw_get_pos, 20),   // GetWindowPosition
            (vw_ideal, 12),     // GetMinIdealImageSize
            (vw_ideal, 12),     // GetMaxIdealImageSize
            (vw_get_pos, 20),   // GetRestorePosition
            (ok, 8),            // HideCursor
            (get0, 8),          // IsCursorHidden
        ]);
        let mut bv: Vec<(Handler, u32)> = iunknown!(BV).into_iter().chain(IDISPATCH).collect();
        bv.extend([(bv_avg_time as Handler, 8), (get0, 8), (get0, 8), (bv_width, 8), (bv_height, 8)]);
        bv.extend(bv_props!(0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15));
        fn set_src(c: &mut Ctx) -> u32 {
            bv_set_rect(c, false)
        }
        fn get_src(c: &mut Ctx) -> u32 {
            bv_get_rect(c, false)
        }
        fn def_src(c: &mut Ctx) -> u32 {
            bv_default_rect(c, false)
        }
        fn set_dst(c: &mut Ctx) -> u32 {
            bv_set_rect(c, true)
        }
        fn get_dst(c: &mut Ctx) -> u32 {
            bv_get_rect(c, true)
        }
        fn def_dst(c: &mut Ctx) -> u32 {
            bv_default_rect(c, true)
        }
        fn is_src(c: &mut Ctx) -> u32 {
            bv_is_default(c, false)
        }
        fn is_dst(c: &mut Ctx) -> u32 {
            bv_is_default(c, true)
        }
        bv.extend([
            (set_src as Handler, 20),
            (get_src, 20),
            (def_src, 4),
            (set_dst, 20),
            (get_dst, 20),
            (def_dst, 4),
            (bv_size, 12),
            (notimpl, 20), // GetVideoPaletteEntries
            (bv_image, 12),
            (is_src, 4),
            (is_dst, 4),
            (bv_aspect, 12), // IBasicVideo2::GetPreferredAspectRatio
        ]);
        [
            vtable("IGraphBuilder", &gb),
            vtable("IMediaControl", &mc),
            vtable("IMediaEventEx", &me),
            vtable("IMediaSeeking", &ms),
            vtable("IMediaPosition", &mp),
            vtable("IBasicAudio", &ba),
            vtable("IVideoWindow", &vw),
            vtable("IBasicVideo", &bv),
        ]
    })
}

/// CoCreateInstance(CLSID_FilterGraph, ...)
pub fn create_instance(clsid: u32, iid: u32) -> Option<u32> {
    // CLSID_EnhancedVideoRenderer {fa10746c-9b63-4b6c-bc49-fc300ea5f256}
    if r32(clsid) == 0xfa10_746c && r32(clsid + 4) == 0x4b6c_9b63 {
        return evr(iid);
    }
    // CLSID_FilterGraph {e436ebb3-524f-11ce-9f53-0020af0ba770}, FilterGraphNoThread {e436ebb8-...}
    if !matches!(r32(clsid), 0xe436_ebb3 | 0xe436_ebb8) || r32(clsid + 4) != 0x11ce_524f {
        return None;
    }
    let s = slot_of(iid)?;
    let b = mem::heap_alloc(NI * 8, true);
    for (k, v) in vtables().iter().enumerate() {
        wp(b + ps() * k as u32, *v);
    }
    let event = crate::obj::add(crate::obj::Obj::Event { manual: true, set: false });
    let g = Graph {
        refs: 1,
        event,
        notify: (0, 0, 0),
        flags: 0,
        events: VecDeque::new(),
        ended: false,
        state: 0,
        done: false,
        playing: false,
        video: (1280, 720),
        duration: 0,
        rate: (0, 1),
        owner: 0,
        style: DEFAULT_STYLE,
        style_ex: 0,
        window: (0, 0, 1280, 720),
        visible: false,
        auto_show: true,
        drain: 0,
        full_screen: 0,
        src: None,
        dst: None,
        volume: 0,
        balance: 0,
        evr: 0,
    };
    GRAPHS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, g);
    Some(b + ps() * s)
}

// ---------------------------------------------------------------------------------------------
// The EVR: IBaseFilter, IMFGetService and IMFVideoDisplayControl (what GetService hands out),
// each one word of the block like the graph's interfaces. The display control keeps the
// window and rectangles the graph's picture goes to; the rest succeeds and does nothing.

#[derive(Clone, Copy)]
struct Evr {
    hwnd: u32,
    /// the source as fractions (left top right bottom), the destination in the window
    src: [f32; 4],
    dst: (i32, i32, i32, i32),
    /// MFVideoARMode (1: keep the picture's shape)
    mode: u32,
}

static EVRS: Mutex<Option<HashMap<u32, Evr>>> = Mutex::new(None);

fn evr_with<R>(b: u32, f: impl FnOnce(&mut Evr) -> R) -> Option<R> {
    EVRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
/// The graphs an EVR renders for take its new settings.
fn evr_changed(e: u32) {
    let gs: Vec<u32> = GRAPHS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).iter().filter(|(_, g)| g.evr == e).map(|(&b, _)| b).collect();
    for b in gs {
        sync(b);
    }
}

fn evr_slot(iid: u32) -> Option<u32> {
    Some(match r32(iid) {
        0 | 0x56a8_6895 | 0x0000_010c | 0x56a8_6899 => 0, // IBaseFilter, IPersist, IMediaFilter
        0xfa99_3888 => 1,                                 // IMFGetService
        0xa490_b1e4 => 2,                                 // IMFVideoDisplayControl
        _ => return None,
    })
}
fn evr_qi(c: &mut Ctx, k: u32) -> u32 {
    let b = c.a(0) - ps() * k;
    match evr_slot(c.a(1)) {
        Some(s) => {
            putp(c.a(2), b + ps() * s);
            0
        }
        None => {
            putp(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
/// IMFGetService::GetService(service, iid, out)
fn evr_get_service(c: &mut Ctx) -> u32 {
    let b = c.a(0) - ps();
    match evr_slot(c.a(2)) {
        Some(s) => {
            putp(c.a(3), b + ps() * s);
            0
        }
        None => {
            putp(c.a(3), 0);
            E_NOINTERFACE
        }
    }
}
fn one(_: &mut Ctx) -> u32 {
    1
}
fn dc_block(c: &Ctx) -> u32 {
    c.a(0) - 2 * ps()
}
/// GetNativeVideoSize(size, aspect): the movie's of a graph it renders for
fn dc_native(c: &mut Ctx) -> u32 {
    let e = dc_block(c);
    let v = GRAPHS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).values().find(|g| g.evr == e).map(|g| g.video).unwrap_or((0, 0));
    for p in [c.a(1), c.a(2)] {
        if p != 0 {
            w32(p, v.0 as u32);
            w32(p + 4, v.1 as u32);
        }
    }
    0
}
/// SetVideoPosition(MFVideoNormalizedRect *src, RECT *dst)
fn dc_set_pos(c: &mut Ctx) -> u32 {
    let e = dc_block(c);
    let (s, d) = (c.a(1), c.a(2));
    if s == 0 && d == 0 {
        return E_POINTER;
    }
    evr_with(e, |v| {
        if s != 0 {
            v.src = [0, 1, 2, 3].map(|i| f32::from_bits(r32(s + 4 * i)));
        }
        if d != 0 {
            let (l, t, r, b) = (r32(d) as i32, r32(d + 4) as i32, r32(d + 8) as i32, r32(d + 12) as i32);
            v.dst = (l, t, r - l, b - t);
        }
    });
    evr_changed(e);
    0
}
fn dc_get_pos(c: &mut Ctx) -> u32 {
    let e = evr_with(dc_block(c), |v| *v);
    let Some(e) = e else { return E_POINTER };
    let (s, d) = (c.a(1), c.a(2));
    if s != 0 {
        for (i, v) in e.src.iter().enumerate() {
            w32(s + 4 * i as u32, v.to_bits());
        }
    }
    if d != 0 {
        for (i, v) in [e.dst.0, e.dst.1, e.dst.0 + e.dst.2, e.dst.1 + e.dst.3].into_iter().enumerate() {
            w32(d + 4 * i as u32, v as u32);
        }
    }
    0
}
fn dc_set_mode(c: &mut Ctx) -> u32 {
    let (e, m) = (dc_block(c), c.a(1));
    evr_with(e, |v| v.mode = m);
    evr_changed(e);
    0
}
fn dc_get_mode(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), evr_with(dc_block(c), |v| v.mode).unwrap_or(1));
    0
}
fn dc_set_window(c: &mut Ctx) -> u32 {
    let (e, h) = (dc_block(c), c.a(1));
    evr_with(e, |v| v.hwnd = h);
    evr_changed(e);
    0
}
fn dc_get_window(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    putp(c.a(1), evr_with(dc_block(c), |v| v.hwnd).unwrap_or(0));
    0
}

fn evr(iid: u32) -> Option<u32> {
    static V: OnceLock<[u32; 3]> = OnceLock::new();
    let s = evr_slot(iid)?;
    let v = V.get_or_init(|| {
        fn q0(c: &mut Ctx) -> u32 {
            evr_qi(c, 0)
        }
        fn q1(c: &mut Ctx) -> u32 {
            evr_qi(c, 1)
        }
        fn q2(c: &mut Ctx) -> u32 {
            evr_qi(c, 2)
        }
        let unk = |q: Handler| -> Vec<(Handler, u32)> { vec![(q, 12), (one as Handler, 4), (one as Handler, 4)] };
        let mut gs = unk(q1);
        gs.push((evr_get_service, 16));
        // IBaseFilter: GetClassID, Stop, Pause, Run, GetState, SetSyncSource, GetSyncSource,
        // EnumPins, FindPin, QueryFilterInfo, JoinFilterGraph, QueryVendorInfo
        let mut bf = unk(q0);
        bf.extend([(notimpl as Handler, 8), (ok, 4), (ok, 4), (ok, 12), (notimpl, 12), (ok, 8), (notimpl, 8)]);
        bf.extend([(notimpl as Handler, 8), (notimpl, 12), (notimpl, 8), (ok, 12), (notimpl, 8)]);
        // IMFVideoDisplayControl
        let mut dc = unk(q2);
        dc.extend([
            (dc_native as Handler, 12), // GetNativeVideoSize
            (dc_native, 12),            // GetIdealVideoSize
            (dc_set_pos, 12),           // SetVideoPosition
            (dc_get_pos, 12),           // GetVideoPosition
            (dc_set_mode, 8),           // SetAspectRatioMode
            (dc_get_mode, 8),           // GetAspectRatioMode
            (dc_set_window, 8),         // SetVideoWindow
            (dc_get_window, 8),         // GetVideoWindow
            (ok, 4),                    // RepaintVideo
            (notimpl, 20),              // GetCurrentImage
            (ok, 8),                    // SetBorderColor
            (get0, 8),                  // GetBorderColor
            (ok, 8),                    // SetRenderingPrefs
            (get0, 8),                  // GetRenderingPrefs
            (ok, 8),                    // SetFullscreen
            (get0, 8),                  // GetFullscreen
        ]);
        [vtable("EVR::IBaseFilter", &bf), vtable("EVR::IMFGetService", &gs), vtable("EVR::IMFVideoDisplayControl", &dc)]
    });
    let b = mem::heap_alloc(8 * 3, true);
    for (k, t) in v.iter().enumerate() {
        wp(b + ps() * k as u32, *t);
    }
    EVRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Evr { hwnd: 0, src: [0.0, 0.0, 1.0, 1.0], dst: (0, 0, 0, 0), mode: 1 });
    Some(b + ps() * s)
}
