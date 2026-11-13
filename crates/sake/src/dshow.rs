//! DirectShow's filter graph (CLSID_FilterGraph) and the EVR, for games that play their movies
//! with RenderFile and Run.
//!
//! A movie with a `.vnv` next to it (`ippai pack` makes one of every mpg / wmv / avi / mp4 ...,
//! crates/ippai/src/movie.rs) really plays (dshow_play.rs): its clock, seeking, EC_COMPLETE at
//! its end, its picture where the video window (IVideoWindow / IBasicVideo) or the EVR's
//! rectangle (IMFVideoDisplayControl) is, its sound. Any other "plays" to its end shortly after
//! Run (EC_COMPLETE after PLAY), so games go on past their opening or logo movie.
//!
//! One guest block holds every interface of a graph: pointer-sized word k is interface k's
//! vtable (so an interface pointer is block + k * pointer size); the graph's state is in
//! `GRAPHS`, by block.

#![allow(non_snake_case)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use crate::api::{Ctx, Handler};
use crate::dshow_play::{self as play, now, Place, Play, SEC};
use crate::mem::{self, ps, put32, put64, putp, r32, rp, w32, w64, wp};

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_POINTER: u32 = 0x8000_4003;
const VFW_E_NOT_STOPPED: u32 = 0x8004_0224;
const E_NOTIMPL: u32 = 0x8000_4001;
const E_ABORT: u32 = 0x8000_4004;
const E_FAIL: u32 = 0x8000_4005;
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
// (the two as the graph hands them out when its renderer is windowless: every call
// E_NOINTERFACE, as no filter of it has a window)
const VW_NONE: u32 = 8;
const BV_NONE: u32 = 9;
const GC: u32 = 10; // IGraphConfig
const ES: u32 = 11; // IMediaEventSink (what filters tell the graph)
const NI: u32 = 12;

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
        // IGraphConfig {03a1eb8e-32bf-4245-8502-114d08a9cb88}
        0x03a1_eb8e => GC,
        // IMediaEventSink {56a868a2-0ad4-11ce-b03a-0020af0ba770}
        0x56a8_68a2 => ES,
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
    /// Runs and Stops so far: an end scheduled without a movie (ENDS) is for this one
    runs: u32,
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
    /// the filters added (each joined to the graph, one reference held)
    filters: Vec<u32>,
    /// the names they were added by (FindFilterByName)
    named: Vec<(Vec<u16>, u32)>,
    /// pins were connected through it (ConnectDirect): its state changes go to the filters
    /// from then on (a graph a game only renders a file in keeps them to itself)
    wired: bool,
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
            let windowless = with(b, |g| {
                g.refs += 1;
                g.evr != 0 && evr_with(g.evr, |e| e.windowless).unwrap_or(false)
            })
            .unwrap_or(false);
            let s = match s {
                VW if windowless => VW_NONE,
                BV if windowless => BV_NONE,
                s => s,
            };
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
fn nointerface(_: &mut Ctx) -> u32 {
    E_NOINTERFACE
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

/// IFilterGraph::EnumFilters: an IEnumFilters of the renderer added to it (ours: its AddRef and
/// Release count nothing), if any (there are no other filters). The enumerator is a block of
/// its vtable, the renderer and where it is (0 or 1).
fn enum_filters(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    static E: OnceLock<u32> = OnceLock::new();
    let v = *E.get_or_init(|| {
        fn qi(c: &mut Ctx) -> u32 {
            let ok = r32(c.a(1)) == 0x56a8_6893 || r32(c.a(1)) == 0;
            putp(c.a(2), if ok { c.a(0) } else { 0 });
            if ok { 0 } else { E_NOINTERFACE }
        }
        fn one(_: &mut Ctx) -> u32 {
            1
        }
        /// Next(n, filters, fetched)
        fn next(c: &mut Ctx) -> u32 {
            let (e, n) = (c.a(0), c.a(1));
            let (f, at) = (r32(e + ps()), r32(e + 2 * ps()));
            let got = (n > 0 && f != 0 && at == 0) as u32;
            if got == 1 {
                putp(c.a(2), f);
                w32(e + 2 * ps(), 1);
            }
            if c.a(3) != 0 {
                put32(c.a(3), got);
            }
            if got == n { 0 } else { S_FALSE }
        }
        /// Skip(n)
        fn skip(c: &mut Ctx) -> u32 {
            let e = c.a(0);
            let left = (r32(e + ps()) != 0 && r32(e + 2 * ps()) == 0) as u32;
            if c.a(1) > 0 {
                w32(e + 2 * ps(), 1);
            }
            if c.a(1) <= left { 0 } else { S_FALSE }
        }
        fn reset(c: &mut Ctx) -> u32 {
            w32(c.a(0) + 2 * ps(), 0);
            0
        }
        vtable("IEnumFilters", &[(qi, 12), (one, 4), (one, 4), (next, 16), (skip, 8), (reset, 4), (notimpl, 8)])
    });
    let f = with(base(c, GB), |g| g.evr).unwrap_or(0);
    let e = mem::heap_alloc(3 * ps(), true);
    wp(e, v);
    wp(e + ps(), f);
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
/// RemoveFilter(filter) (and IGraphConfig::RemoveFilterEx(filter, flags), whose
/// REMFILTERF_LEAVECONNECTED only keeps pins this graph doesn't track): out of the graph, told so
/// (JoinFilterGraph(NULL, NULL)) and released; E_FAIL for one not in it (Wine's answer). A
/// game's teardown (薔薇ノ木ニ薔薇ノ花咲ク: enumerate, RemoveFilterEx each) relies on it.
/// A graph's filter list taking filter `f` in: false for one already in it (added once).
fn put_filter(filters: &mut Vec<u32>, evr: &mut u32, f: u32, is_evr: bool) -> bool {
    if filters.contains(&f) {
        return false;
    }
    filters.push(f);
    if is_evr {
        *evr = f;
    }
    true
}
/// ... and letting it go: false for one not in it. The renderer it was goes with it.
fn take_filter(filters: &mut Vec<u32>, evr: &mut u32, f: u32) -> bool {
    let n = filters.len();
    filters.retain(|&x| x != f);
    if *evr == f {
        *evr = 0;
    }
    filters.len() != n
}
fn remove_filter_at(c: &mut Ctx, b: u32, f: u32) -> u32 {
    if f == 0 {
        return E_POINTER;
    }
    let had = with(b, |g| {
        g.named.retain(|x| x.1 != f);
        take_filter(&mut g.filters, &mut g.evr, f)
    })
    .unwrap_or(false);
    if !had {
        return E_FAIL;
    }
    com(c, f, 13, &[0, 0]);
    com(c, f, 2, &[]);
    0
}
fn remove_filter(c: &mut Ctx) -> u32 {
    let (b, f) = (base(c, GB), c.a(1));
    remove_filter_at(c, b, f)
}
fn remove_filter_ex(c: &mut Ctx) -> u32 {
    let (b, f) = (base(c, GC), c.a(1));
    remove_filter_at(c, b, f)
}

fn add_filter(c: &mut Ctx) -> u32 {
    let f = c.a(1);
    if f == 0 {
        return E_POINTER;
    }
    let b = base(c, GB);
    // (one added again stays added once)
    let is_evr = evr_with(f, |_| ()).is_some();
    if !with(b, |g| put_filter(&mut g.filters, &mut g.evr, f, is_evr)).unwrap_or(false) {
        return 0;
    }
    com(c, f, 1, &[]);
    if c.a(2) != 0 {
        let n = mem::wstr(c.a(2));
        with(b, |g| g.named.push((n, f)));
    }
    // IBaseFilter::JoinFilterGraph(graph, name)
    com(c, f, 13, &[b + ps() * GB, c.a(2)]);
    0
}

/// COM method `slot` of `obj` (`this` first)
pub(crate) fn com(c: &mut Ctx, obj: u32, slot: u32, args: &[u32]) -> u32 {
    let f = rp(rp(obj) + ps() * slot);
    let mut a = vec![obj];
    a.extend_from_slice(args);
    c.call(f, &a)
}

/// ConnectDirect(out, in, mt): out->Connect(in, mt), only while stopped
/// FindFilterByName(LPCWSTR, IBaseFilter **): a filter AddFilter added by that name (one
/// reference added), else *out NULL and VFW_E_NOT_FOUND (the graph's own renderers are no
/// filters a game can reach)
fn find_filter(c: &mut Ctx) -> u32 {
    let (name, out) = (c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let want = if name == 0 { Vec::new() } else { mem::wstr(name) };
    if let Some(f) = with(base(c, GB), |g| g.named.iter().find(|(n, _)| *n == want).map(|x| x.1)).flatten() {
        com(c, f, 1, &[]);
        putp(out, f);
        return 0;
    }
    if crate::env_on!("SAKE_TRACE_COM") && name != 0 {
        eprintln!("sake: FilterGraph FindFilterByName {:?}: none", String::from_utf16_lossy(&mem::wstr(name)));
    }
    NOT_FOUND
}
fn connect_direct(c: &mut Ctx) -> u32 {
    let b = base(c, GB);
    let (out, inp, mt) = (c.a(1), c.a(2), c.a(3));
    if out == 0 || inp == 0 {
        return E_POINTER;
    }
    if with(b, |g| g.state).unwrap_or(0) != 0 {
        return VFW_E_NOT_STOPPED;
    }
    let r = com(c, out, 3, &[inp, mt]);
    if r == 0 {
        with(b, |g| g.wired = true);
    }
    r
}
/// Connect(out, in): ConnectDirect when a pin is one of dshow_mpeg.rs's filters (no filters in
/// between: those are the ones games put there themselves); others, as before, succeed with
/// nothing done (sake's graph renders files itself)
fn connect(c: &mut Ctx) -> u32 {
    let (out, inp) = (c.a(1), c.a(2));
    if out == 0 || inp == 0 {
        return E_POINTER;
    }
    if !crate::dshow_mpeg::is_pin(out) && !crate::dshow_mpeg::is_pin(inp) {
        return 0;
    }
    let b = base(c, GB);
    if with(b, |g| g.state).unwrap_or(0) != 0 {
        return VFW_E_NOT_STOPPED;
    }
    let r = com(c, out, 3, &[inp, 0]);
    if r == 0 {
        with(b, |g| g.wired = true);
    }
    // (VFW_E_CANNOT_CONNECT, as Windows says when no filter would do)
    if r & 0x8000_0000 != 0 { 0x8004_0217 } else { r }
}
/// Disconnect(pin): pin->Disconnect
fn disconnect(c: &mut Ctx) -> u32 {
    let pin = c.a(1);
    if pin == 0 {
        return E_POINTER;
    }
    com(c, pin, 5, &[])
}
/// Stop/Pause/Run (IBaseFilter slots 4, 5, 6) of each filter of a wired graph
fn filters_to(c: &mut Ctx, b: u32, slot: u32) {
    let fs = with(b, |g| if g.wired { g.filters.clone() } else { Vec::new() }).unwrap_or_default();
    for f in fs {
        // (Run's start time: 0, a REFERENCE_TIME that is two words on x86)
        let start: &[u32] = if slot != 6 { &[] } else if ps() == 4 { &[0, 0] } else { &[0] };
        com(c, f, slot, start);
    }
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

/// Without a movie, it ends PLAY after it runs: EC_COMPLETE queued, the event signaled, the
/// window notified then (once a movie: see Graph::done).
/// From stopped it answers S_FALSE (Windows: the change is still under way), else S_OK.
fn run(c: &mut Ctx) -> u32 {
    let b = base(c, MC);
    filters_to(c, b, 6);
    let was = set_state(b, 2);
    // (an MPEG-1 stream of dshow_mpeg.rs plays: its own end)
    let mpeg = crate::dshow_mpeg::movie(b).is_some();
    let run = with(b, |g| {
        // (a Run again, after a Pause, keeps the end to come)
        if g.playing || g.done || mpeg {
            return None;
        }
        g.runs += 1;
        g.done = true;
        Some(g.runs)
    });
    if let Some(Some(n)) = run {
        end_later(b, n);
    }
    if was == 0 { S_FALSE } else { 0 }
}

/// How long a movie with nothing to play "plays". Not 0: CatSystem2 (ここなつ #48) stores its
/// loop count after Run returns, and its movie thread, given EC_COMPLETE before that, rewound
/// and ran the movie again, on without end (a black screen instead of the title).
const PLAY: std::time::Duration = std::time::Duration::from_millis(500);

/// Ends to come of movies with nothing to play: (when, graph, its run).
static ENDS: Mutex<Vec<(std::time::Instant, u32, u32)>> = Mutex::new(Vec::new());
static ENDS_WAKE: std::sync::Condvar = std::sync::Condvar::new();

/// The ends of `v` due at `t`, earliest first, taken out.
fn due(v: &mut Vec<(std::time::Instant, u32, u32)>, t: std::time::Instant) -> Vec<(u32, u32)> {
    let mut out: Vec<_> = v.iter().filter(|e| e.0 <= t).copied().collect();
    v.retain(|e| e.0 > t);
    out.sort_by_key(|e| e.0);
    out.into_iter().map(|e| (e.1, e.2)).collect()
}

/// Graph `b`'s movie (nothing to play), run `n`, ends PLAY from now: unless a Stop or another
/// Run came in between, or a real movie plays by then.
fn end_later(b: u32, n: u32) {
    ENDS.lock().unwrap_or_else(|e| e.into_inner()).push((std::time::Instant::now() + PLAY, b, n));
    ENDS_WAKE.notify_all();
    static T: OnceLock<()> = OnceLock::new();
    T.get_or_init(|| {
        std::thread::Builder::new()
            .name("movie ends".into())
            .spawn(|| {
                let mut v = ENDS.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let t = std::time::Instant::now();
                    let now_due = due(&mut v, t);
                    let wait = v.iter().map(|e| e.0.saturating_duration_since(t)).min().unwrap_or(std::time::Duration::from_secs(3600));
                    drop(v);
                    for (b, n) in now_due {
                        if with(b, |g| g.runs == n && g.done && !g.playing) == Some(true) {
                            completed(b);
                        }
                    }
                    v = ENDS.lock().unwrap_or_else(|e| e.into_inner());
                    if v.iter().all(|e| e.0 > std::time::Instant::now()) {
                        v = ENDS_WAKE.wait_timeout(v, wait).unwrap_or_else(|e| e.into_inner()).0;
                    }
                }
            })
            .expect("movie ends thread");
    });
}
/// Queues EC_PAUSED; S_FALSE from stopped, as Run.
fn pause(c: &mut Ctx) -> u32 {
    let b = base(c, MC);
    filters_to(c, b, 5);
    let was = set_state(b, 1);
    queue(b, EC_PAUSED, 0);
    if was == 0 { S_FALSE } else { 0 }
}
fn stop(c: &mut Ctx) -> u32 {
    let b = base(c, MC);
    filters_to(c, b, 4);
    set_state(b, 0);
    // (without a movie, one run again after a stop plays to its end again; one ending now
    // doesn't)
    with(b, |g| {
        g.done = false;
        g.ended = false;
        g.runs += 1;
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
    queue2(b, code, p1, 0)
}
fn queue2(b: u32, code: u32, p1: u32, p2: u32) {
    let Some((ev, (hwnd, msg, lp), flags)) = with(b, |g| {
        g.events.push_back((code, p1, p2));
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

/// IMediaEventSink::Notify(code, p1, p2) from a filter (krmovie's buffer renderer tells its
/// frames with a code of its own): to the application, as Windows' graph passes what it does not
/// handle itself. A renderer's EC_COMPLETE is not passed on (the graph sends its own when the
/// movie ends), nor the ones Windows handles by default: EC_REPAINT, EC_CLOCK_CHANGED,
/// EC_ACTIVATE, EC_NEED_RESTART, EC_DISPLAY_CHANGED, EC_STARVATION.
fn es_notify(c: &mut Ctx) -> u32 {
    let (code, p1, p2) = (c.a(1), c.a(2), c.a(3));
    if !matches!(code, EC_COMPLETE | 0x05 | 0x0d | 0x13 | 0x14 | 0x16 | 0x17) {
        queue2(base(c, ES), code, p1, p2);
    }
    0
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
    let mut f = Some(f);
    play::with(|m| m.get_mut(&b).map(|p| (f.take().unwrap())(&mut p.tl, t))).or_else(|| crate::dshow_mpeg::timeline(b, f.take()?))
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
/// A graph's movie's duration (its `.vnv`'s, or the MPEG-1 stream's of dshow_mpeg.rs).
fn duration(b: u32) -> u64 {
    match crate::dshow_mpeg::movie(b) {
        Some(i) => (i.duration.max(0.0) * SEC as f64) as u64,
        None => with(b, |g| g.duration).unwrap_or(0),
    }
}
/// The picture size and rate (pictures, seconds) of a graph's MPEG-1 stream, if it has one.
fn mpeg_video(b: u32) -> Option<((i32, i32), (u32, u32))> {
    crate::dshow_mpeg::movie(b).map(|i| ((i.width as i32, i.height as i32), ((i.rate * 1000.0).round() as u32, 1000)))
}
/// The volume IBasicAudio set on graph `b` (hundredths of a dB).
pub(crate) fn volume(b: u32) -> i32 {
    with(b, |g| g.volume).unwrap_or(0)
}
fn ms_duration(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w64(c.a(1), duration(base(c, MS)));
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
    let r = timeline(b, |tl, t| {
        let cur = tl.position(t);
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
            tl.set_stop(t, s.max(0) as u64);
        }
        if let Some(to) = to {
            tl.seek(t, to.max(0) as u64);
        }
        (tl.position(t), tl.stop)
    });
    if let Some((cur, stop)) = r {
        if fc & 3 != 0 {
            crate::dshow_mpeg::seeked(b);
        }
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
    let d = duration(base(c, MS));
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
            if let Some(e) = evr_with(g.evr, |e| *e).filter(|e| e.windowless) {
                // (a VMR-9 shows nothing until it is given a destination; the EVR fills the window)
                let dst = if (e.dst.2 > 0 && e.dst.3 > 0) || e.vmr { e.dst } else { crate::user::client_rect_screen(e.hwnd).map_or((0, 0, 0, 0), |r| (0, 0, r.r - r.l, r.b - r.t)) };
                // (the EVR's source as fractions of the picture)
                let s = e.src;
                let src = if e.vmr {
                    e.src_px.unwrap_or((0, 0, video.0, video.1))
                } else {
                    ((s[0] * video.0 as f32) as i32, (s[1] * video.1 as f32) as i32, ((s[2] - s[0]) * video.0 as f32) as i32, ((s[3] - s[1]) * video.1 as f32) as i32)
                };
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
    let b = base(c, BV);
    let r = mpeg_video(b).map(|v| v.1).or_else(|| with(b, |g| if g.playing { g.rate } else { (0, 1) })).unwrap_or((0, 1));
    put_dbl(c.a(1), if r.0 == 0 { 0.0 } else { r.1 as f64 / r.0 as f64 })
}
fn bv_width(c: &mut Ctx) -> u32 {
    let b = base(c, BV);
    put32(c.a(1), mpeg_video(b).map(|v| v.0).or_else(|| with(b, |g| g.video)).unwrap_or((0, 0)).0 as u32);
    0
}
fn bv_height(c: &mut Ctx) -> u32 {
    let b = base(c, BV);
    put32(c.a(1), mpeg_video(b).map(|v| v.0).or_else(|| with(b, |g| g.video)).unwrap_or((0, 0)).1 as u32);
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
    let Some((w, h)) = play::with(|m| m.get(&b).map(|p| (p.movie.w, p.movie.h))) else {
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
    if !put_dib(b, img) {
        // E_UNEXPECTED
        return 0x8000_ffff;
    }
    put32(psize, need);
    0
}
/// The frame graph `b` shows as a 32-bit bottom-up DIB at `img` (a BITMAPINFOHEADER, then
/// 4 * w * h bytes).
fn put_dib(b: u32, img: u32) -> bool {
    let t = now();
    let Some((stream, n, w, h)) = play::with(|m| m.get(&b).map(|p| (p.movie.stream, play::frame_at(p.tl.position(t), p.movie.rate, p.movie.frames), p.movie.w, p.movie.h))) else {
        return false;
    };
    let mut px = vec![0u8; (4 * w * h) as usize];
    if !unsafe { kura::video::frame(stream, n as i32, px.as_mut_ptr(), 4 * w as usize, w, h, kura::video::Out::Rgb601) } {
        return false;
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
    true
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
                (remove_filter, 8),          // RemoveFilter
                (enum_filters, 8),           // EnumFilters
                (find_filter, 12),           // FindFilterByName
                (connect_direct, 16),        // ConnectDirect
                (ok, 8),                     // Reconnect
                (disconnect, 8),             // Disconnect
                (ok, 4),                     // SetDefaultSyncSource
                (connect, 12),               // Connect
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
        let bv_tail = [
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
        ];
        bv.extend(bv_tail);
        // (the same calls, each E_NOINTERFACE)
        let none = |unk: [(Handler, u32); 3], v: &[(Handler, u32)]| -> Vec<(Handler, u32)> { unk.into_iter().chain(v[3..].iter().map(|&(_, n)| (nointerface as Handler, n))).collect() };
        let vw_none = none(iunknown!(VW_NONE), &vw);
        let gc: Vec<(Handler, u32)> = iunknown!(GC)
            .into_iter()
            .chain([
                (notimpl as Handler, 28), // Reconnect
                (notimpl, 20),            // Reconfigure
                (notimpl, 8),             // AddFilterToCache
                (notimpl, 8),             // EnumCacheFilter
                (notimpl, 8),             // RemoveFilterFromCache
                (notimpl, 8),             // GetStartTime
                (notimpl, 16),            // PushThroughData
                (notimpl, 12),            // SetFilterFlags
                (notimpl, 12),            // GetFilterFlags
                (remove_filter_ex, 12),   // RemoveFilterEx
            ])
            .collect();
        let bv_none = none(iunknown!(BV_NONE), &bv);
        let es: Vec<(Handler, u32)> = iunknown!(ES).into_iter().chain([(es_notify as Handler, 16)]).collect();
        [
            vtable("IGraphBuilder", &gb),
            vtable("IMediaControl", &mc),
            vtable("IMediaEventEx", &me),
            vtable("IMediaSeeking", &ms),
            vtable("IMediaPosition", &mp),
            vtable("IBasicAudio", &ba),
            vtable("IVideoWindow", &vw),
            vtable("IBasicVideo", &bv),
            vtable("IVideoWindow(windowless)", &vw_none),
            vtable("IBasicVideo(windowless)", &bv_none),
            vtable("IGraphConfig", &gc),
            vtable("IMediaEventSink", &es),
        ]
    })
}

/// CoCreateInstance(CLSID_FilterGraph, ...)
pub fn create_instance(clsid: u32, iid: u32) -> Option<u32> {
    // CLSID_EnhancedVideoRenderer {fa10746c-9b63-4b6c-bc49-fc300ea5f256}
    if r32(clsid) == 0xfa10_746c && r32(clsid + 4) == 0x4b6c_9b63 {
        return evr(iid);
    }
    // CLSID_VideoMixingRenderer {b87beb7b-8d29-423f-ae4d-6582c10175ac} (the VMR-9 is vmr9.rs's)
    if r32(clsid) == 0xb87b_eb7b && r32(clsid + 4) == 0x423f_8d29 {
        return vmr(iid);
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
        runs: 0,
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
        filters: Vec::new(),
        named: Vec::new(),
        wired: false,
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
    /// MFVideoARMode / VMR9AspectRatioMode (1: keep the picture's shape)
    mode: u32,
    /// a VMR-9 (not an EVR): its source rectangle is in pixels (`src_px`, None: all of it)
    vmr: bool,
    src_px: Option<(i32, i32, i32, i32)>,
    /// it draws into `hwnd` itself (the EVR always; a VMR-9 once in VMR9Mode_Windowless): the
    /// graph's video window is not used
    windowless: bool,
    border: u32,
    key: u32,
    /// the graph it joined (JoinFilterGraph; not held: the graph holds the filter)
    graph: u32,
}

static EVRS: Mutex<Option<HashMap<u32, Evr>>> = Mutex::new(None);

fn evr_with<R>(b: u32, f: impl FnOnce(&mut Evr) -> R) -> Option<R> {
    EVRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(f)
}
/// The graphs an EVR renders for take its new settings.
pub(crate) fn evr_changed(e: u32) {
    let gs: Vec<u32> = GRAPHS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).iter().filter(|(_, g)| g.evr == e).map(|(&b, _)| b).collect();
    for b in gs {
        sync(b);
    }
}

/// A VMR-9 of vmr9.rs at `key` (its IBaseFilter): drawn as the old ones were; `windowless`
/// once it is in VMR9Mode_Windowless.
pub(crate) fn vmr_evr(key: u32, windowless: Option<bool>) {
    let mut g = EVRS.lock().unwrap_or_else(|e| e.into_inner());
    let m = g.get_or_insert_with(HashMap::new);
    let e = m.entry(key).or_insert(Evr { hwnd: 0, src: [0.0, 0.0, 1.0, 1.0], dst: (0, 0, 0, 0), mode: 0, vmr: true, src_px: None, windowless: false, border: 0, key: 0, graph: 0 });
    if let Some(w) = windowless {
        e.windowless = w;
    }
    drop(g);
    evr_changed(key);
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
/// IBaseFilter::QueryFilterInfo(&info): FILTER_INFO { WCHAR achName[128], IFilterGraph *pGraph }
fn evr_query_info(c: &mut Ctx) -> u32 {
    let (e, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    let graph = evr_with(e, |v| v.graph).unwrap_or(0);
    mem::fill(out, 128 * 2 + ps(), 0);
    if graph != 0 {
        com(c, graph, 1, &[]);
    }
    wp(out + 256, graph);
    0
}
/// IBaseFilter::JoinFilterGraph(graph, name)
fn evr_join(c: &mut Ctx) -> u32 {
    let (e, g) = (c.a(0), c.a(1));
    evr_with(e, |v| v.graph = g);
    0
}
fn dc_block(c: &Ctx) -> u32 {
    c.a(0) - 2 * ps()
}
/// The picture size of the movie a graph renders into renderer `e` (0x0: none).
fn native(e: u32) -> (i32, i32) {
    GRAPHS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).values().find(|g| g.evr == e && g.playing).map(|g| g.video).unwrap_or((0, 0))
}
/// GetNativeVideoSize(size, aspect): the movie's of a graph it renders for
fn dc_native(c: &mut Ctx) -> u32 {
    let v = native(dc_block(c));
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
        bf.extend([(notimpl as Handler, 8), (notimpl, 12), (evr_query_info, 8), (evr_join, 12), (notimpl, 8)]);
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
    EVRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Evr { hwnd: 0, src: [0.0, 0.0, 1.0, 1.0], dst: (0, 0, 0, 0), mode: 1, vmr: false, src_px: None, windowless: true, border: 0, key: 0, graph: 0 });
    Some(b + ps() * s)
}

// ---------------------------------------------------------------------------------------------
// The VMR-7 and VMR-9 (CLSID_VideoMixingRenderer(9)), as games add one by hand before
// RenderFile (Artemis: the VMR-7, windowless): IBaseFilter, IVMRFilterConfig(9) and, once
// windowless, IVMRWindowlessControl(9) (one word of the block each, its state an `Evr` with
// `vmr`; the two VMRs' interfaces are the same calls). Windowed (their default), the graph's
// video window shows the picture as without one.

const VMR_CONFIG: u32 = 1;
const VMR_WINDOWLESS: u32 = 2;
/// VMR9Mode_Windowless
const VMR9_WINDOWLESS: u32 = 2;

fn vmr_qi(c: &mut Ctx, k: u32) -> u32 {
    let b = c.a(0) - ps() * k;
    if c.a(2) == 0 {
        return E_POINTER;
    }
    let windowless = evr_with(b, |e| e.windowless).unwrap_or(false);
    let s = match r32(c.a(1)) {
        0 | 0x56a8_6895 | 0x0000_010c | 0x56a8_6899 => Some(0), // IBaseFilter, IPersist, IMediaFilter
        0x9e55_30c5 | 0x5a80_4648 => Some(VMR_CONFIG),                    // IVMRFilterConfig(9)
        0x0eb1_088c | 0x8f53_7d09 if windowless => Some(VMR_WINDOWLESS), // IVMRWindowlessControl(9)
        _ => None,
    };
    match s {
        Some(s) => {
            putp(c.a(2), b + ps() * s);
            0
        }
        None => {
            if crate::env_on!("SAKE_TRACE_COM") {
                eprintln!("sake: VMR-9 QueryInterface {:08x}: no", r32(c.a(1)));
            }
            putp(c.a(2), 0);
            E_NOINTERFACE
        }
    }
}
fn vmr_block(c: &Ctx, k: u32) -> u32 {
    c.a(0) - ps() * k
}
/// IVMRFilterConfig9::SetRenderingMode(mode)
fn vmr_set_mode(c: &mut Ctx) -> u32 {
    let (b, m) = (vmr_block(c, VMR_CONFIG), c.a(1));
    if !(1..=4).contains(&m) || m == 3 {
        return E_INVALIDARG;
    }
    evr_with(b, |e| e.windowless = m == VMR9_WINDOWLESS);
    evr_changed(b);
    0
}
fn vmr_get_mode(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let w = evr_with(vmr_block(c, VMR_CONFIG), |e| e.windowless).unwrap_or(false);
    put32(c.a(1), if w { VMR9_WINDOWLESS } else { 1 });
    0
}
fn wl_block(c: &Ctx) -> u32 {
    vmr_block(c, VMR_WINDOWLESS)
}
/// GetNativeVideoSize(w, h, aspect w, aspect h)
pub(crate) fn wl_native(c: &mut Ctx) -> u32 {
    let v = native(wl_block(c));
    for (i, x) in [v.0, v.1, v.0, v.1].into_iter().enumerate() {
        if c.a(1 + i as u32) != 0 {
            put32(c.a(1 + i as u32), x as u32);
        }
    }
    0
}
/// GetMinIdealVideoSize(w, h): 1x1, as Windows answers (tests/winref dshow)
pub(crate) fn wl_min_ideal(c: &mut Ctx) -> u32 {
    put32(c.a(1), 1);
    put32(c.a(2), 1);
    0
}
/// GetMaxIdealVideoSize(w, h): one more than the screen each way (Windows on a 1024x768 screen:
/// 1025x769)
pub(crate) fn wl_max_ideal(c: &mut Ctx) -> u32 {
    let (w, h) = crate::user::screen();
    put32(c.a(1), (w + 1) as u32);
    put32(c.a(2), (h + 1) as u32);
    0
}
fn rect_at(p: u32) -> (i32, i32, i32, i32) {
    let (l, t, r, b) = (r32(p) as i32, r32(p + 4) as i32, r32(p + 8) as i32, r32(p + 12) as i32);
    (l, t, r - l, b - t)
}
fn put_rect(p: u32, r: (i32, i32, i32, i32)) {
    for (i, v) in [r.0, r.1, r.0 + r.2, r.1 + r.3].into_iter().enumerate() {
        w32(p + 4 * i as u32, v as u32);
    }
}
/// SetVideoPosition(const RECT *src, const RECT *dst)
pub(crate) fn wl_set_pos(c: &mut Ctx) -> u32 {
    let e = wl_block(c);
    let (s, d) = (c.a(1), c.a(2));
    if s == 0 && d == 0 {
        return E_POINTER;
    }
    if s != 0 {
        let r = rect_at(s);
        let v = native(e);
        if r.0 < 0 || r.1 < 0 || r.2 <= 0 || r.3 <= 0 || r.0 + r.2 > v.0 || r.1 + r.3 > v.1 {
            return E_INVALIDARG;
        }
    }
    evr_with(e, |v| {
        if s != 0 {
            v.src_px = Some(rect_at(s));
        }
        if d != 0 {
            v.dst = rect_at(d);
        }
    });
    evr_changed(e);
    0
}
pub(crate) fn wl_get_pos(c: &mut Ctx) -> u32 {
    let b = wl_block(c);
    let Some(e) = evr_with(b, |v| *v) else { return E_POINTER };
    let v = native(b);
    if c.a(1) != 0 {
        put_rect(c.a(1), e.src_px.unwrap_or((0, 0, v.0, v.1)));
    }
    if c.a(2) != 0 {
        put_rect(c.a(2), e.dst);
    }
    0
}
pub(crate) fn wl_get_mode(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), evr_with(wl_block(c), |v| v.mode).unwrap_or(0));
    0
}
pub(crate) fn wl_set_mode(c: &mut Ctx) -> u32 {
    let (e, m) = (wl_block(c), c.a(1));
    evr_with(e, |v| v.mode = m);
    evr_changed(e);
    0
}
pub(crate) fn wl_set_window(c: &mut Ctx) -> u32 {
    let (e, h) = (wl_block(c), c.a(1));
    evr_with(e, |v| v.hwnd = h);
    evr_changed(e);
    0
}
/// GetCurrentImage(BYTE **dib): the frame shown, a CoTaskMemAlloc'd 32-bit DIB
pub(crate) fn wl_image(c: &mut Ctx) -> u32 {
    let e = wl_block(c);
    if c.a(1) == 0 {
        return E_POINTER;
    }
    let g = GRAPHS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).iter().find(|(_, g)| g.evr == e && g.playing).map(|(&b, g)| (b, g.video));
    let Some((b, (w, h))) = g else { return E_NOTIMPL };
    let img = mem::heap_alloc(40 + 4 * (w * h) as u32, false);
    if !put_dib(b, img) {
        mem::heap_free(img);
        return 0x8000_ffff;
    }
    putp(c.a(1), img);
    0
}
pub(crate) fn wl_set_border(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    evr_with(wl_block(c), |e| e.border = v);
    0
}
pub(crate) fn wl_get_border(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), evr_with(wl_block(c), |e| e.border).unwrap_or(0));
    0
}
/// (the VMR-7's) SetColorKey / GetColorKey
fn wl_set_key(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    evr_with(wl_block(c), |e| e.key = v);
    0
}
fn wl_get_key(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    put32(c.a(1), evr_with(wl_block(c), |e| e.key).unwrap_or(0));
    0
}

fn vmr(iid: u32) -> Option<u32> {
    static V: OnceLock<[u32; 3]> = OnceLock::new();
    let v = V.get_or_init(|| {
        fn q0(c: &mut Ctx) -> u32 {
            vmr_qi(c, 0)
        }
        fn q1(c: &mut Ctx) -> u32 {
            vmr_qi(c, VMR_CONFIG)
        }
        fn q2(c: &mut Ctx) -> u32 {
            vmr_qi(c, VMR_WINDOWLESS)
        }
        let unk = |q: Handler| -> Vec<(Handler, u32)> { vec![(q, 12), (one as Handler, 4), (one as Handler, 4)] };
        let mut bf = unk(q0);
        bf.extend([(notimpl as Handler, 8), (ok, 4), (ok, 4), (ok, 12), (notimpl, 12), (ok, 8), (notimpl, 8)]);
        bf.extend([(notimpl as Handler, 8), (notimpl, 12), (evr_query_info, 8), (evr_join, 12), (notimpl, 8)]);
        let mut fc = unk(q1);
        fc.extend([
            (ok as Handler, 8), // SetImageCompositor
            (ok, 8),            // SetNumberOfStreams
            (get0, 8),          // GetNumberOfStreams
            (ok, 8),            // SetRenderingPrefs
            (get0, 8),          // GetRenderingPrefs
            (vmr_set_mode, 8),
            (vmr_get_mode, 8),
        ]);
        let mut wl = unk(q2);
        wl.extend([
            (wl_native as Handler, 20),
            (wl_min_ideal, 12),
            (wl_max_ideal, 12),
            (wl_set_pos, 12),
            (wl_get_pos, 12),
            (wl_get_mode, 8),
            (wl_set_mode, 8),
            (wl_set_window, 8),
            (ok, 12), // RepaintVideo
            (ok, 4),  // DisplayModeChanged
            (wl_image, 8),
            (wl_set_border, 8),
            (wl_get_border, 8),
            (wl_set_key, 8), // (the VMR-7's)
            (wl_get_key, 8),
        ]);
        [vtable("VMR::IBaseFilter", &bf), vtable("VMR::IVMRFilterConfig", &fc), vtable("VMR::IVMRWindowlessControl", &wl)]
    });
    let s = match r32(iid) {
        0 | 0x56a8_6895 | 0x0000_010c | 0x56a8_6899 => 0,
        0x9e55_30c5 | 0x5a80_4648 => VMR_CONFIG,
        _ => return None,
    };
    let b = mem::heap_alloc(8 * 3, true);
    for (k, t) in v.iter().enumerate() {
        wp(b + ps() * k as u32, *t);
    }
    EVRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, Evr { hwnd: 0, src: [0.0, 0.0, 1.0, 1.0], dst: (0, 0, 0, 0), mode: 0, vmr: true, src_px: None, windowless: false, border: 0, key: 0, graph: 0 });
    Some(b + ps() * s)
}

#[cfg(test)]
mod ends_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn ends_are_taken_when_due_earliest_first() {
        let t = Instant::now();
        let mut v = vec![(t + Duration::from_secs(9), 1, 1), (t, 2, 5), (t - Duration::from_millis(3), 3, 7)];
        assert_eq!(due(&mut v, t), [(3, 7), (2, 5)]);
        assert_eq!(v.len(), 1);
        assert!(due(&mut v, t).is_empty());
        assert_eq!(due(&mut v, t + Duration::from_secs(9)), [(1, 1)]);
        assert!(v.is_empty());
    }
}

#[cfg(test)]
mod filter_list_tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn removed_once_then_fails() {
        let (mut v, mut e) = (Vec::new(), 0);
        assert!(put_filter(&mut v, &mut e, 7, true));
        assert!(!put_filter(&mut v, &mut e, 7, true));
        assert_eq!((v.as_slice(), e), (&[7][..], 7));
        assert!(take_filter(&mut v, &mut e, 7));
        assert_eq!((v.len(), e), (0, 0));
        assert!(!take_filter(&mut v, &mut e, 7));
    }

    proptest! {
        /// Any run of adds and removes keeps the list a set in the order filters came in, the
        /// renderer one of it (or none), and each call's answer what a set model says.
        #[test]
        fn adds_and_removes_match_a_set(ops in proptest::collection::vec((any::<bool>(), 1u32..6, any::<bool>()), 0..64)) {
            let (mut v, mut e) = (Vec::new(), 0u32);
            let mut model: Vec<u32> = Vec::new();
            let mut renderer = 0u32;
            for (add, f, is_evr) in ops {
                if add {
                    let fresh = !model.contains(&f);
                    prop_assert_eq!(put_filter(&mut v, &mut e, f, is_evr), fresh);
                    if fresh {
                        model.push(f);
                        if is_evr { renderer = f; }
                    }
                } else {
                    let had = model.contains(&f);
                    prop_assert_eq!(take_filter(&mut v, &mut e, f), had);
                    model.retain(|&x| x != f);
                    if renderer == f { renderer = 0; }
                }
                prop_assert_eq!(&v, &model);
                prop_assert_eq!(e, renderer);
                prop_assert!(e == 0 || v.contains(&e));
            }
        }
    }
}
