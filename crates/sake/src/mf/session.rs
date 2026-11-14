//! The Media Session (MFCreateMediaSession), as games play a movie through it: a topology from
//! the source resolver's media source to the renderers' activates (topology.rs), SetTopology,
//! Start, the events (GetEvent, or BeginGetEvent / EndGetEvent on a thread of the session's),
//! IMFVideoDisplayControl through MFGetService, the volume, the presentation clock.
//!
//! The movie plays on dshow_play's movie thread (mf/movie.rs: the page's decoders), its picture
//! in the window the video renderer was made for, placed as an EVR of dshow.rs is (the display
//! control is that EVR's). What the session tells of, in order, is `Model`, kept free of the
//! guest and tested here; winref's mfsession probe has Windows' answers.

use super::source::{self, Data};
use super::topology::{self, Act};
use super::*;
use std::sync::{Arc, Condvar};

const MF_E_TOPO_UNSUPPORTED: u32 = 0xc00d_5214;
const MF_E_NO_EVENTS_AVAILABLE: u32 = 0xc00d_3e80;
const MF_E_MULTIPLE_SUBSCRIBERS: u32 = 0xc00d_36e3;
const MF_E_UNSUPPORTED_SERVICE: u32 = 0xc00d_36ba;
const MF_E_NOT_FOUND: u32 = 0xc00d_36d5;
const MF_E_CLOCK_NO_TIME_SOURCE: u32 = 0xc00d_9c41;

/// MediaEventType
pub mod met {
    pub const ERROR: u32 = 1;
    pub const TOPOLOGY_SET: u32 = 101;
    pub const TOPOLOGIES_CLEARED: u32 = 102;
    pub const STARTED: u32 = 103;
    pub const PAUSED: u32 = 104;
    pub const STOPPED: u32 = 105;
    pub const CLOSED: u32 = 106;
    pub const ENDED: u32 = 107;
    pub const CAPABILITIES_CHANGED: u32 = 110;
    pub const TOPOLOGY_STATUS: u32 = 111;
    pub const END_OF_PRESENTATION: u32 = 211;
}
/// MF_TOPOSTATUS
pub const READY: u32 = 100;
pub const STARTED_SOURCE: u32 = 200;
pub const TOPO_ENDED: u32 = 500;
/// MFSESSIONCAP_*: START, SEEK, PAUSE, RATE_FORWARD, RATE_REVERSE, DOES_NOT_USE_NETWORK
pub const CAPS_EMPTY: u32 = 0x1 | 0x2 | 0x10 | 0x20 | 0x40;
pub const CAPS_MOVIE: u32 = CAPS_EMPTY | 0x4;

// ---------------------------------------------------------------------------------------------
// what the session tells of

/// An event: its type, status, MF_EVENT_TOPOLOGY_STATUS, MF_EVENT_SESSIONCAPS(_DELTA), and a
/// value that is an object (the topology; the event holds the reference).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct E {
    pub met: u32,
    pub hr: u32,
    pub status: Option<u32>,
    pub caps: Option<(u32, u32)>,
    pub unk: u32,
    /// a value of the program's own (QueueEvent)
    pub val: Option<Val>,
}
fn e(met: u32, hr: u32) -> E {
    E { met, hr, ..E::default() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum St {
    Stopped,
    Started,
    Paused,
    Closed,
}

/// The session's state and the events its calls queue. Each call is answered by an event of
/// its own (its status says whether it was done: the calls themselves succeed).
#[derive(Debug)]
pub struct Model {
    pub st: St,
    pub topo: bool,
    /// the topology's source started (MF_TOPOSTATUS_STARTED_SOURCE told) since it last stopped
    sourced: bool,
    pub caps: u32,
    pub out: VecDeque<E>,
}

impl Default for Model {
    fn default() -> Model {
        Model { st: St::Stopped, topo: false, sourced: false, caps: CAPS_EMPTY, out: VecDeque::new() }
    }
}

impl Model {
    /// SetTopology: `ok` whether it can play; `unk` the topology (the TopologySet event's value).
    /// Whether the event took `unk` (it was set).
    pub fn set_topology(&mut self, ok: bool, unk: u32) -> bool {
        if self.st == St::Closed {
            self.out.push_back(e(met::TOPOLOGY_SET, MF_E_INVALIDREQUEST));
            return false;
        }
        if !ok {
            self.out.push_back(e(met::TOPOLOGY_SET, MF_E_TOPO_UNSUPPORTED));
            return false;
        }
        self.topo = true;
        self.out.push_back(E { unk, ..e(met::TOPOLOGY_SET, 0) });
        if self.caps != CAPS_MOVIE {
            let old = std::mem::replace(&mut self.caps, CAPS_MOVIE);
            self.out.push_back(E { caps: Some((CAPS_MOVIE, CAPS_MOVIE ^ old)), ..e(met::CAPABILITIES_CHANGED, 0) });
        }
        self.out.push_back(E { status: Some(READY), ..e(met::TOPOLOGY_STATUS, 0) });
        true
    }
    pub fn clear_topologies(&mut self) {
        let hr = if self.st == St::Closed { MF_E_INVALIDREQUEST } else { 0 };
        self.out.push_back(e(met::TOPOLOGIES_CLEARED, hr));
    }
    /// Start: whether the movie runs now.
    pub fn start(&mut self) -> bool {
        if self.st == St::Closed || !self.topo {
            self.out.push_back(e(met::STARTED, MF_E_INVALIDREQUEST));
            return false;
        }
        if !self.sourced {
            self.sourced = true;
            self.out.push_back(E { status: Some(STARTED_SOURCE), ..e(met::TOPOLOGY_STATUS, 0) });
        }
        self.st = St::Started;
        self.out.push_back(e(met::STARTED, 0));
        true
    }
    /// Pause: whether it paused.
    pub fn pause(&mut self) -> bool {
        if self.st != St::Started {
            self.out.push_back(e(met::PAUSED, MF_E_INVALIDREQUEST));
            return false;
        }
        self.st = St::Paused;
        self.out.push_back(e(met::PAUSED, 0));
        true
    }
    /// Stop: whether it stopped (back to the start).
    pub fn stop(&mut self) -> bool {
        if self.st == St::Closed {
            self.out.push_back(e(met::STOPPED, MF_E_INVALIDREQUEST));
            return false;
        }
        self.st = St::Stopped;
        self.sourced = false;
        self.out.push_back(e(met::STOPPED, 0));
        true
    }
    /// Close: whether it closed (the movie goes).
    pub fn close(&mut self) -> bool {
        if self.st == St::Closed {
            self.out.push_back(e(met::CLOSED, MF_E_INVALIDREQUEST));
            return false;
        }
        self.st = St::Closed;
        self.topo = false;
        self.out.push_back(e(met::CLOSED, 0));
        true
    }
    /// The movie reached its end: whether that was told (it ran).
    pub fn end(&mut self) -> bool {
        if self.st != St::Started {
            return false;
        }
        self.st = St::Stopped;
        self.sourced = false;
        self.out.push_back(e(met::END_OF_PRESENTATION, 0));
        self.out.push_back(E { status: Some(TOPO_ENDED), ..e(met::TOPOLOGY_STATUS, 0) });
        self.out.push_back(e(met::ENDED, 0));
        true
    }
}

// ---------------------------------------------------------------------------------------------
// IMFMediaEvent: an attribute store (the status and caps as its attributes) with more methods

fn k_topology_status() -> Guid {
    source::g("30c5018d-9a53-454b-ad9e-6d5f8fa7c43b")
}
fn k_caps() -> Guid {
    source::g("7e5ebcd0-11b8-4abe-afad-10f6599a7f42")
}
fn k_caps_delta() -> Guid {
    source::g("7e5ebcd1-11b8-4abe-afad-10f6599a7f42")
}

/// The events' type, extended type, status and value, by block.
static EVENTS: Mutex<Option<HashMap<u32, (u32, Guid, u32, Option<Val>)>>> = Mutex::new(None);
fn event<R>(b: u32, f: impl FnOnce(&(u32, Guid, u32, Option<Val>)) -> R) -> Option<R> {
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get(&b).map(f)
}

fn me_qi(c: &mut Ctx) -> u32 {
    // IMFMediaEvent {df598932-f10c-4e39-bba2-c308f101daa3}
    reader::typed_qi(c, 0xdf59_8932, 0xf10c, 0x4e39)
}
fn me_release(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    if attrs(b, |a| a.0) == Some(1) {
        if let Some((_, _, _, Some(Val::Unk(u)))) = EVENTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).remove(&b) {
            release(c, u);
        }
    }
    at_release(c)
}
fn me_type(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), event(c.a(0), |e| e.0).unwrap_or(0));
    0
}
fn me_ext(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    mem::write(c.a(1), &event(c.a(0), |e| e.1).unwrap_or_default());
    0
}
fn me_status(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), event(c.a(0), |e| e.2).unwrap_or(0));
    0
}
/// GetValue(PROPVARIANT *): VT_EMPTY when it has none
fn me_value(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return E_POINTER;
    }
    match event(c.a(0), |e| e.3.clone()).flatten() {
        Some(v) => put_prop(c, p, &v),
        None => mem::fill(p, 8 + 2 * ps(), 0),
    }
    0
}
fn event_vtable() -> u32 {
    static VT: OnceLock<u32> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut m = attrs_methods();
        m[0] = (me_qi, 12);
        m[2] = (me_release, 4);
        m.extend([(me_type as Handler, 8), (me_ext, 8), (me_status, 8), (me_value, 8)]);
        vtable("IMFMediaEvent", &m)
    })
}
/// A new event of `x` (its object's reference handed over).
fn new_event(x: E) -> u32 {
    let b = object(event_vtable(), 1);
    let mut s = Store::default();
    if let Some(st) = x.status {
        s.set(k_topology_status(), Val::U32(st));
    }
    if let Some((caps, delta)) = x.caps {
        s.set(k_caps(), Val::U32(caps));
        s.set(k_caps_delta(), Val::U32(delta));
    }
    ATTRS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (1, s));
    let val = if x.unk != 0 { Some(Val::Unk(x.unk)) } else { x.val };
    EVENTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, (x.met, [0; 16], x.hr, val));
    b
}
/// MFCreateMediaEvent(MediaEventType, REFGUID extended, HRESULT, const PROPVARIANT *, IMFMediaEvent **)
fn MFCreateMediaEvent(c: &mut Ctx) -> u32 {
    let (t, ext, hr, pv, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if out == 0 {
        return E_POINTER;
    }
    let val = if pv == 0 || r16(pv) == 0 { None } else { Some(get_prop(pv).unwrap_or(Val::U32(0))) };
    if let Some(Val::Unk(u)) = val {
        add_ref(c, u);
    }
    let b = new_event(E { met: t, hr, val, ..E::default() });
    if ext != 0 {
        let g = guid_at(ext);
        EVENTS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).get_mut(&b).map(|e| e.1 = g);
    }
    putp(out, b);
    0
}

// ---------------------------------------------------------------------------------------------
// the session: a block of 4 interfaces (IMFMediaSession, IMFGetService, IMFPresentationClock,
// IMFSimpleAudioVolume), one count of references; its state by block in SESSIONS

struct Session {
    refs: u32,
    m: Model,
    shut: bool,
    /// BeginGetEvent's result waiting for the next event, and whether the events thread runs
    waiter: u32,
    thread: bool,
    topology: u32,
    /// the movie set up by SetTopology (handed to dshow_play at the first Start), its size
    movie: Option<crate::dshow_play::Movie>,
    video: (i32, i32),
    playing: bool,
    evr: u32,
    volume: f32,
    muted: bool,
}

static SESSIONS: Mutex<Option<HashMap<u32, Session>>> = Mutex::new(None);
static CV: Condvar = Condvar::new();

fn session<R>(b: u32, f: impl FnOnce(&mut Session) -> R) -> Option<R> {
    let mut g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    let s = g.get_or_insert_with(HashMap::new).get_mut(&b)?;
    let r = f(s);
    if s.shut || !s.m.out.is_empty() {
        CV.notify_all();
    }
    Some(r)
}
/// The session of interface `this` at word `k` of its block.
fn base(c: &Ctx, k: u32) -> u32 {
    c.a(0) - ps() * k
}

pub(crate) fn snap_wake() {
    drop(SESSIONS.lock());
    CV.notify_all();
}

/// Where the movie of session `b` is drawn, and how loud: to dshow_play.
fn sync(b: u32) {
    let Some((evr, video, st, vol)) = session(b, |s| (s.evr, s.video, s.m.st, centibels(s.volume as f64, s.muted))) else { return };
    let place = if evr != 0 { crate::dshow::evr_place(evr, video, matches!(st, St::Started | St::Paused)) } else { None };
    play::with(|m| {
        if let Some(p) = m.get_mut(&b) {
            if let Some(pl) = place {
                p.place = pl;
            }
            p.volume = vol;
        }
    });
}
/// EVR `e`'s display control changed: the sessions it draws for.
pub(crate) fn evr_changed(e: u32) {
    let bs: Vec<u32> = SESSIONS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).iter().filter(|(_, s)| s.evr == e).map(|(&b, _)| b).collect();
    for b in bs {
        sync(b);
    }
}
/// The picture size of a session's movie drawn by EVR `e`.
pub(crate) fn native(e: u32) -> Option<(i32, i32)> {
    SESSIONS.lock().unwrap_or_else(|x| x.into_inner()).get_or_insert_with(HashMap::new).values().find(|s| s.evr == e && s.m.topo).map(|s| s.video)
}

/// The movie of session `k` reached its end (dshow_play's movie thread): whether `k` is one.
pub fn ended(k: u32) -> bool {
    let Some(told) = session(k, |s| s.m.end()) else { return false };
    if told {
        // (the clock stays at the end; the next Start goes from the beginning)
        play::timeline(k, now(), |tl, t| tl.pause(t));
        sync(k);
    }
    true
}

// --- the events thread ----------------------------------------------------------------------

enum Next {
    /// BeginGetEvent's result and the event for it
    Event(u32, E),
    Idle,
    Gone,
}
fn next(b: u32, wait: std::time::Duration) -> Next {
    let mut g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
    let mut waited = false;
    loop {
        let Some(s) = g.get_or_insert_with(HashMap::new).get_mut(&b) else { return Next::Gone };
        if s.shut {
            return Next::Gone;
        }
        if s.waiter != 0 && !s.m.out.is_empty() {
            let x = s.m.out.pop_front().unwrap();
            return Next::Event(std::mem::take(&mut s.waiter), x);
        }
        if waited || kura::snap::pausing() {
            return Next::Idle;
        }
        g = CV.wait_timeout(g, wait).unwrap_or_else(|e| e.into_inner()).0;
        waited = true;
    }
}
/// The thread that answers BeginGetEvent: the callback's Invoke when the next event is there
/// (it is the result's object for EndGetEvent).
fn events_thread(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    loop {
        match next(b, std::time::Duration::from_millis(250)) {
            Next::Event(r, x) => {
                plat::set_object(c, r, new_event(x));
                let cb = plat::result_callback(r);
                if cb != 0 {
                    add_ref(c, cb);
                    com(c, cb, 4, &[r]);
                    release(c, cb);
                }
                release(c, r);
            }
            Next::Idle => {
                crate::snap::point();
            }
            Next::Gone => break,
        }
    }
    session(b, |s| s.thread = false);
    retire(b);
    crate::k32::exit_thread(0)
}
fn start_events(b: u32) {
    if session(b, |s| !std::mem::replace(&mut s.thread, true)) != Some(true) {
        return;
    }
    static ENTRY: OnceLock<u32> = OnceLock::new();
    let entry = *ENTRY.get_or_init(|| crate::api::host_fn("mf_session_events", events_thread, 4));
    if crate::spawn_gated(entry, b, 0x10000, crate::obj::new_thread_obj(0), false) == 0 {
        session(b, |s| s.thread = false);
    }
}
/// Session `b` goes once nothing holds it (the program's references, its events thread).
fn retire(b: u32) {
    let gone = {
        let mut g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
        let m = g.get_or_insert_with(HashMap::new);
        match m.get(&b) {
            Some(s) if s.refs == 0 && !s.thread && s.shut => m.remove(&b).is_some(),
            _ => false,
        }
    };
    if gone {
        crate::com::free_later(b);
    }
}

// --- IUnknown ---------------------------------------------------------------------------------

/// The word of the block for an interface (None: not one of the session's).
fn slot(g: &Guid) -> Option<u32> {
    if is_unknown(g) {
        return Some(0);
    }
    Some(match u32::from_le_bytes(g[0..4].try_into().unwrap()) {
        // IMFMediaSession {90377834-21d0-4dee-8214-ba2e3e6c1127}, IMFMediaEventGenerator
        0x9037_7834 if is(g, 0x9037_7834, 0x21d0, 0x4dee) => 0,
        0x2cd0_bd52 if is(g, 0x2cd0_bd52, 0xbcd5, 0x4b89) => 0,
        // IMFGetService {fa993888-4383-415a-a930-dd472a8cf6f7}
        0xfa99_3888 if is(g, 0xfa99_3888, 0x4383, 0x415a) => 1,
        // IMFClock {2eb1e945-18b8-4139-9b1a-d5d584818530}, IMFPresentationClock {868ce85c-8ea9-4f55-ab82-b009a910a805}
        0x2eb1_e945 if is(g, 0x2eb1_e945, 0x18b8, 0x4139) => 2,
        0x868c_e85c if is(g, 0x868c_e85c, 0x8ea9, 0x4f55) => 2,
        // IMFSimpleAudioVolume {089edf13-cf71-4338-8d13-9e569dbdc319}
        0x089e_df13 if is(g, 0x089e_df13, 0xcf71, 0x4338) => 3,
        _ => return None,
    })
}
fn qi(c: &mut Ctx, k: u32) -> u32 {
    let (b, out) = (base(c, k), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    let g = guid_at(c.a(1));
    // (the clock and volume answer for themselves; the session for all but those)
    let s = slot(&g).filter(|&s| k < 2 || s == k || s == 0 && is_unknown(&g));
    match s {
        Some(s) => {
            session(b, |x| x.refs += 1);
            putp(out, b + ps() * s);
            0
        }
        None => {
            putp(out, 0);
            E_NOINTERFACE
        }
    }
}
fn add_ref_k(c: &mut Ctx, k: u32) -> u32 {
    session(base(c, k), |s| {
        s.refs += 1;
        s.refs
    })
    .unwrap_or(1)
}
fn release_k(c: &mut Ctx, k: u32) -> u32 {
    let b = base(c, k);
    let n = session(b, |s| {
        s.refs = s.refs.saturating_sub(1);
        s.refs
    })
    .unwrap_or(0);
    if n == 0 {
        shutdown(c, b);
        retire(b);
    }
    n
}
macro_rules! unknown {
    ($k:expr) => {{
        fn q(c: &mut Ctx) -> u32 {
            qi(c, $k)
        }
        fn a(c: &mut Ctx) -> u32 {
            add_ref_k(c, $k)
        }
        fn r(c: &mut Ctx) -> u32 {
            release_k(c, $k)
        }
        vec![(q as Handler, 12), (a as Handler, 4), (r as Handler, 4)]
    }};
}

fn live(b: u32) -> bool {
    session(b, |s| !s.shut).unwrap_or(false)
}

// --- IMFMediaEventGenerator ------------------------------------------------------------------

/// GetEvent(DWORD flags, IMFMediaEvent **): MF_EVENT_FLAG_NO_WAIT (1) or it waits
fn ms_get_event(c: &mut Ctx) -> u32 {
    let (b, flags, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return E_POINTER;
    }
    loop {
        let got = {
            let mut g = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
            let Some(s) = g.get_or_insert_with(HashMap::new).get_mut(&b) else { return E_INVALIDARG };
            if s.shut {
                return MF_E_SHUTDOWN;
            }
            if s.waiter != 0 {
                return MF_E_MULTIPLE_SUBSCRIBERS;
            }
            match s.m.out.pop_front() {
                Some(x) => Some(x),
                None if flags & 1 != 0 => return MF_E_NO_EVENTS_AVAILABLE,
                None => {
                    let _ = CV.wait_timeout(g, std::time::Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner());
                    None
                }
            }
        };
        match got {
            Some(x) => {
                putp(out, new_event(x));
                return 0;
            }
            // (a save state: the call is made again after it)
            None if kura::snap::pausing() => {
                crate::snap::point();
            }
            None => {}
        }
    }
}
/// BeginGetEvent(IMFAsyncCallback *, IUnknown *state)
fn ms_begin_event(c: &mut Ctx) -> u32 {
    let (b, cb, st) = (c.a(0), c.a(1), c.a(2));
    if cb == 0 {
        return E_INVALIDARG;
    }
    match session(b, |s| (s.shut, s.waiter != 0)) {
        Some((true, _)) | None => return MF_E_SHUTDOWN,
        Some((_, true)) => return MF_E_MULTIPLE_SUBSCRIBERS,
        _ => {}
    }
    let r = plat::new_result(c, 0, cb, st);
    session(b, |s| s.waiter = r);
    start_events(b);
    0
}
/// EndGetEvent(IMFAsyncResult *, IMFMediaEvent **)
fn ms_end_event(c: &mut Ctx) -> u32 {
    let (r, out) = (c.a(1), c.a(2));
    if r == 0 || out == 0 {
        return E_POINTER;
    }
    let o = plat::object_of(r);
    putp(out, o);
    if o == 0 {
        return plat::done_of(r).map(|d| d.1).filter(|&h| h != 0).unwrap_or(E_FAIL);
    }
    add_ref(c, o);
    0
}
/// QueueEvent(MediaEventType, REFGUID, HRESULT, const PROPVARIANT *)
fn ms_queue_event(c: &mut Ctx) -> u32 {
    let (b, t, hr, pv) = (c.a(0), c.a(1), c.a(3), c.a(4));
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    let val = if pv == 0 || r16(pv) == 0 { None } else { Some(get_prop(pv).unwrap_or(Val::U32(0))) };
    if let Some(Val::Unk(u)) = val {
        add_ref(c, u);
    }
    session(b, |s| s.m.out.push_back(E { met: t, hr, val, ..E::default() }));
    0
}

// --- IMFMediaSession --------------------------------------------------------------------------

/// The tracks a topology plays: the source's bytes and media, the stream of each source node
/// that reaches a video or audio renderer of ours, the video window.
struct Plan {
    data: Arc<Data>,
    media: Arc<source::Media>,
    video: Option<usize>,
    audio: Option<usize>,
    hwnd: u32,
}
/// The renderer node `n` leads to (through transforms and tees).
fn renderer(n: u32, depth: u32) -> Option<Act> {
    let (kind, object, outs) = topology::node(n, |x| (x.kind, x.object, x.outputs.clone()))?;
    if kind == topology::OUTPUT {
        return topology::act(object);
    }
    if depth > 16 {
        return None;
    }
    outs.into_iter().flatten().find_map(|(d, _)| renderer(d, depth + 1))
}
fn plan(t: u32) -> Option<Plan> {
    let mut p: Option<Plan> = None;
    for n in topology::nodes_of(t, topology::SOURCESTREAM) {
        let Some(st) = store_of(n) else { continue };
        let Some(Val::Unk(src)) = st.get(&source::g("835c58ec-e075-4bc7-bcba-4de000df9ae6")) else { continue };
        let Some(Val::Unk(sd)) = st.get(&source::g("835c58ee-e075-4bc7-bcba-4de000df9ae6")) else { continue };
        let (Some((data, media)), Some((_, i))) = (source::source_of(*src), source::sd_stream(*sd)) else { continue };
        let Some(r) = renderer(n, 0) else { continue };
        let q = p.get_or_insert_with(|| Plan { data, media, video: None, audio: None, hwnd: 0 });
        match (r, media_kind(&q.media, i)) {
            (Act::Video(h), Some(true)) => {
                q.video = Some(i);
                q.hwnd = h;
            }
            (Act::Audio, Some(false)) => q.audio = Some(i),
            _ => {}
        }
    }
    p.filter(|p| p.video.is_some() || p.audio.is_some())
}
/// Whether track `i` is video (Some(false): audio).
fn media_kind(m: &source::Media, i: usize) -> Option<bool> {
    Some(matches!(m.tracks.get(i)?, mp4::Track::Video(_)))
}

/// SetTopology(DWORD flags, IMFTopology *)
fn ms_set_topology(c: &mut Ctx) -> u32 {
    let (b, t) = (c.a(0), c.a(2));
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    if t == 0 || topology::topo(t, |_| ()).is_none() {
        return E_INVALIDARG;
    }
    let movie = plan(t).and_then(|p| {
        // (a program's own byte stream is read here, on its thread: the movie thread has none)
        let data = match p.data.src {
            source::Src::Guest(_) if p.data.len <= 1 << 30 => Arc::new(Data::mem_arc(Arc::new(p.data.read(Some(c), 0, p.data.len as usize)?))),
            source::Src::Guest(_) => return None,
            _ => p.data.clone(),
        };
        let m = movie::open(data, &p.media, p.video, p.audio)?;
        Some((m, p.hwnd))
    });
    // (the movie played before goes)
    play::end(b);
    let ok = movie.is_some();
    add_ref(c, t);
    add_ref(c, t);
    let (old, old_evr, took) = session(b, |s| {
        s.playing = false;
        let old_evr = std::mem::take(&mut s.evr);
        if let Some((m, hwnd)) = movie {
            eprintln!("sake: movie {}x{} {} frames (Media Session)", m.w, m.h, m.frames);
            s.video = (m.w as i32, m.h as i32);
            s.movie = Some(m);
            if hwnd != 0 {
                s.evr = crate::dshow::session_evr(hwnd);
            }
        }
        let took = s.m.set_topology(ok, t);
        (std::mem::replace(&mut s.topology, t), old_evr, took)
    })
    .unwrap_or((0, 0, false));
    if !took {
        // (no event holds it)
        release(c, t);
    }
    if !ok {
        eprintln!("sake: Media Session: a topology of nothing sake plays");
    }
    if old_evr != 0 {
        crate::dshow::evr_drop(old_evr);
    }
    release(c, old);
    0
}
fn ms_clear_topologies(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    session(b, |s| s.m.clear_topologies());
    0
}
/// Start(const GUID *time format, const PROPVARIANT *start): VT_EMPTY from where it is, VT_I8
/// from that time
fn ms_start(c: &mut Ctx) -> u32 {
    let (b, pv) = (c.a(0), c.a(2));
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    let to = (pv != 0 && r16(pv) == 20).then(|| mem::r64(pv + 8));
    let Some((run, movie)) = session(b, |s| {
        let was = s.m.st;
        let run = s.m.start();
        let movie = if run && !s.playing { s.movie.take() } else { None };
        if run {
            s.playing = true;
        }
        (run.then_some(was), movie)
    }) else {
        return E_INVALIDARG;
    };
    if let Some(m) = movie {
        let mut p = Play::new(m);
        p.place.visible = false;
        play::start(b, p);
    }
    if let Some(was) = run {
        play::timeline(b, now(), |tl, t| {
            if let Some(to) = to {
                tl.seek(t, to as u64);
            } else if was == St::Stopped {
                tl.seek(t, 0);
            }
            tl.run(t);
        });
        sync(b);
    }
    0
}
fn ms_pause(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    if session(b, |s| s.m.pause()) == Some(true) {
        play::timeline(b, now(), |tl, t| tl.pause(t));
    }
    0
}
fn ms_stop(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    if session(b, |s| s.m.stop()) == Some(true) {
        play::timeline(b, now(), |tl, t| {
            tl.stop(t);
            tl.seek(t, 0);
        });
        sync(b);
    }
    0
}
fn ms_close(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    let closed = session(b, |s| {
        let ok = s.m.close();
        if ok {
            s.movie = None;
            s.playing = false;
        }
        (ok, std::mem::take(&mut s.evr), if ok { std::mem::take(&mut s.topology) } else { 0 })
    });
    if let Some((true, evr, t)) = closed {
        play::end(b);
        if evr != 0 {
            crate::dshow::evr_drop(evr);
        }
        release(c, t);
    }
    0
}
/// Shutdown: the events (and their objects), the movie, the topology go
fn shutdown(c: &mut Ctx, b: u32) {
    let gone = session(b, |s| {
        if std::mem::replace(&mut s.shut, true) {
            return None;
        }
        let ev: Vec<E> = s.m.out.drain(..).collect();
        s.movie = None;
        Some((ev, std::mem::take(&mut s.waiter), std::mem::take(&mut s.topology), std::mem::take(&mut s.evr)))
    })
    .flatten();
    let Some((ev, waiter, t, evr)) = gone else { return };
    play::end(b);
    for x in ev {
        release(c, x.unk);
        if let Some(Val::Unk(u)) = x.val {
            release(c, u);
        }
    }
    release(c, waiter);
    release(c, t);
    if evr != 0 {
        crate::dshow::evr_drop(evr);
    }
}
fn ms_shutdown(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    shutdown(c, b);
    0
}
fn ms_get_clock(c: &mut Ctx) -> u32 {
    let (b, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    if !live(b) {
        putp(out, 0);
        return MF_E_SHUTDOWN;
    }
    session(b, |s| s.refs += 1);
    putp(out, b + 2 * ps());
    0
}
fn ms_caps(c: &mut Ctx) -> u32 {
    let (b, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_POINTER;
    }
    match session(b, |s| (!s.shut).then_some(s.m.caps)).flatten() {
        Some(v) => {
            w32(out, v);
            0
        }
        None => MF_E_SHUTDOWN,
    }
}
/// GetFullTopology(DWORD flags, TOPOID, IMFTopology **): the current one (MFSESSION_GETFULLTOPOLOGY_CURRENT) or by id
fn ms_full_topology(c: &mut Ctx) -> u32 {
    let (b, flags) = (c.a(0), c.a(1));
    let id = u64_arg(c, 2, 2);
    let out = if mem::x64() { c.a(3) } else { c.a(4) };
    if out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let Some(t) = session(b, |s| (!s.shut).then_some(s.topology)).flatten() else { return MF_E_SHUTDOWN };
    if t == 0 || (flags & 1 == 0 && topology::topo(t, |x| x.id) != Some(id)) {
        return MF_E_NOT_FOUND;
    }
    add_ref(c, t);
    putp(out, t);
    0
}

// --- IMFGetService ----------------------------------------------------------------------------

/// GetService(REFGUID service, REFIID, void **): the video display control (MR_VIDEO_RENDER_SERVICE),
/// the volume (MR_POLICY_VOLUME_SERVICE)
fn gs_get_service(c: &mut Ctx) -> u32 {
    let (b, sv, iid, out) = (base(c, 1), c.a(1), c.a(2), c.a(3));
    if sv == 0 || iid == 0 || out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let (s, i) = (guid_at(sv), guid_at(iid));
    if !live(b) {
        return MF_E_SHUTDOWN;
    }
    // MR_VIDEO_RENDER_SERVICE {1092a86c-ab1a-459a-a336-831fbc4d11ff}, IMFVideoDisplayControl {a490b1e4-ab84-4d31-...}
    if is(&s, 0x1092_a86c, 0xab1a, 0x459a) {
        if !is(&i, 0xa490_b1e4, 0xab84, 0x4d31) {
            return E_NOINTERFACE;
        }
        let evr = session(b, |x| x.evr).unwrap_or(0);
        let dc = if evr != 0 { crate::dshow::evr_display_control(evr) } else { 0 };
        if dc == 0 {
            return MF_E_UNSUPPORTED_SERVICE;
        }
        putp(out, dc);
        return 0;
    }
    // MR_POLICY_VOLUME_SERVICE {1abaa2ac-9d3b-47c6-ab48-c59506de784d}
    if is(&s, 0x1aba_a2ac, 0x9d3b, 0x47c6) {
        if !is(&i, 0x089e_df13, 0xcf71, 0x4338) {
            return E_NOINTERFACE;
        }
        session(b, |x| x.refs += 1);
        putp(out, b + 3 * ps());
        return 0;
    }
    MF_E_UNSUPPORTED_SERVICE
}

/// MFGetService(IUnknown *, REFGUID service, REFIID, void **): its IMFGetService's
fn MFGetService(c: &mut Ctx) -> u32 {
    let (o, sv, iid, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if o == 0 || out == 0 {
        return E_POINTER;
    }
    putp(out, 0);
    let g = mem::heap_alloc(16 + ps(), true);
    mem::write(g, &source::g("fa993888-4383-415a-a930-dd472a8cf6f7"));
    let hr = com(c, o, 0, &[g, g + 16]);
    let gs = rp(g + 16);
    mem::heap_free(g);
    if hr != 0 || gs == 0 {
        return MF_E_UNSUPPORTED_SERVICE;
    }
    let hr = com(c, gs, 3, &[sv, iid, out]);
    release(c, gs);
    hr
}

// --- IMFPresentationClock ---------------------------------------------------------------------

fn clock_state(b: u32) -> u32 {
    // MFCLOCK_STATE: INVALID, RUNNING, STOPPED, PAUSED
    match session(b, |s| (s.m.st, s.m.topo)) {
        Some((St::Started, _)) => 1,
        Some((St::Paused, _)) => 3,
        Some((_, true)) => 2,
        _ => 0,
    }
}
fn position(b: u32) -> u64 {
    let t = now();
    play::read(|m| m.get(&b).map(|p| p.tl.position(t))).unwrap_or(0)
}
fn ck_chars(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    // MFCLOCK_CHARACTERISTICS_FLAG_FREQUENCY_10MHZ
    w32(c.a(1), 2);
    0
}
/// GetCorrelatedTime(DWORD, LONGLONG *clock, MFTIME *system)
fn ck_correlated(c: &mut Ctx) -> u32 {
    let (b, ct, st) = (base(c, 2), c.a(2), c.a(3));
    if ct == 0 || st == 0 {
        return E_POINTER;
    }
    w64(ct, position(b));
    w64(st, now());
    0
}
fn ck_state(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    if out == 0 {
        return E_POINTER;
    }
    w32(out, clock_state(base(c, 2)));
    0
}
/// GetProperties(MFCLOCK_PROPERTIES *): 10 MHz
fn ck_props(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return E_POINTER;
    }
    mem::fill(p, 48, 0);
    w64(p, 10_000_000);
    w64(p + 32, 10_000_000);
    0
}
fn ck_time(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    w64(out, position(base(c, 2)));
    0
}

// --- IMFSimpleAudioVolume ---------------------------------------------------------------------

/// A float argument: x86 its stack slot, x64 XMM1.
fn float_arg(c: &Ctx) -> f32 {
    f32::from_bits(if mem::x64() { c.xmm(1) as u32 } else { c.a(1) })
}
fn sv_set(c: &mut Ctx) -> u32 {
    let (b, v) = (base(c, 3), float_arg(c));
    if !(0.0..=1.0).contains(&v) {
        return E_INVALIDARG;
    }
    session(b, |s| s.volume = v);
    sync(b);
    0
}
fn sv_get(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), session(base(c, 3), |s| s.volume).unwrap_or(1.0).to_bits());
    0
}
fn sv_set_mute(c: &mut Ctx) -> u32 {
    let (b, m) = (base(c, 3), c.a(1) != 0);
    session(b, |s| s.muted = m);
    sync(b);
    0
}
fn sv_get_mute(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return E_POINTER;
    }
    w32(c.a(1), session(base(c, 3), |s| s.muted).unwrap_or(false) as u32);
    0
}

fn vtables() -> [u32; 4] {
    static VT: OnceLock<[u32; 4]> = OnceLock::new();
    *VT.get_or_init(|| {
        let mut ms = unknown!(0);
        ms.extend([
            (ms_get_event as Handler, 12),
            (ms_begin_event, 12),
            (ms_end_event, 12),
            (ms_queue_event, 20),
            (ms_set_topology, 12),
            (ms_clear_topologies, 4),
            (ms_start, 12),
            (ms_pause, 4),
            (ms_stop, 4),
            (ms_close, 4),
            (ms_shutdown, 4),
            (ms_get_clock, 8),
            (ms_caps, 8),
            (ms_full_topology, 20),
        ]);
        let mut gs = unknown!(1);
        gs.push((gs_get_service, 16));
        let mut ck = unknown!(2);
        ck.extend([
            (ck_chars as Handler, 8),
            (ck_correlated, 16),
            (|c: &mut Ctx| {
                if c.a(1) == 0 {
                    return E_POINTER;
                }
                w32(c.a(1), 0);
                0
            }, 8), // GetContinuityKey
            (ck_state, 12),
            (ck_props, 8),
            (ok, 8), // SetTimeSource
            (|c: &mut Ctx| {
                if c.a(1) != 0 {
                    putp(c.a(1), 0);
                }
                MF_E_CLOCK_NO_TIME_SOURCE
            }, 8), // GetTimeSource
            (ck_time, 8),
            (ok, 8),  // AddClockStateSink
            (ok, 8),  // RemoveClockStateSink
            (ok, 12), // Start
            (ok, 4),  // Stop
            (ok, 4),  // Pause
        ]);
        let mut sv = unknown!(3);
        sv.extend([(sv_set as Handler, 8), (sv_get, 8), (sv_set_mute, 8), (sv_get_mute, 8)]);
        [vtable("IMFMediaSession", &ms), vtable("MediaSession::IMFGetService", &gs), vtable("IMFPresentationClock", &ck), vtable("MediaSession::IMFSimpleAudioVolume", &sv)]
    })
}

/// MFCreateMediaSession(IMFAttributes *config, IMFMediaSession **)
fn MFCreateMediaSession(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_POINTER;
    }
    let b = mem::heap_alloc(4 * ps(), true);
    for (k, v) in vtables().into_iter().enumerate() {
        wp(b + ps() * k as u32, v);
    }
    let s = Session { refs: 1, m: Model::default(), shut: false, waiter: 0, thread: false, topology: 0, movie: None, video: (0, 0), playing: false, evr: 0, volume: 1.0, muted: false };
    SESSIONS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new).insert(b, s);
    putp(out, b);
    0
}

pub fn init() {
    implement("mf", "MFCreateMediaSession", MFCreateMediaSession);
    implement("mf", "MFGetService", MFGetService);
    implement("mfplat", "MFCreateMediaEvent", MFCreateMediaEvent);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The events of a movie played to its end and closed, as a program sees them.
    #[test]
    fn a_movie_to_its_end() {
        let mut m = Model::default();
        m.set_topology(true, 7);
        assert!(m.start());
        assert!(m.end());
        assert!(m.close());
        let got: Vec<(u32, u32, Option<u32>)> = m.out.iter().map(|x| (x.met, x.hr, x.status)).collect();
        assert_eq!(
            got,
            vec![
                (met::TOPOLOGY_SET, 0, None),
                (met::CAPABILITIES_CHANGED, 0, None),
                (met::TOPOLOGY_STATUS, 0, Some(READY)),
                (met::TOPOLOGY_STATUS, 0, Some(STARTED_SOURCE)),
                (met::STARTED, 0, None),
                (met::END_OF_PRESENTATION, 0, None),
                (met::TOPOLOGY_STATUS, 0, Some(TOPO_ENDED)),
                (met::ENDED, 0, None),
                (met::CLOSED, 0, None),
            ]
        );
        assert_eq!(m.out[0].unk, 7);
        assert_eq!(m.out[1].caps, Some((CAPS_MOVIE, 4)));
    }

    #[derive(Clone, Copy, Debug)]
    enum Op {
        Set(bool),
        Clear,
        Start,
        Pause,
        Stop,
        Close,
        End,
    }
    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![any::<bool>().prop_map(Op::Set), Just(Op::Clear), Just(Op::Start), Just(Op::Pause), Just(Op::Stop), Just(Op::Close), Just(Op::End)]
    }

    proptest! {
        /// Any calls in any order: each is answered by one event of its own type (status 0 when
        /// it was done), in the order made; the end is told only of a movie running; nothing
        /// is done after Close.
        #[test]
        fn every_call_is_answered(ops in prop::collection::vec(op(), 0..40)) {
            let mut m = Model::default();
            let mut answers = Vec::new();
            for &o in &ops {
                let before = m.st;
                let n = m.out.len();
                match o {
                    Op::Set(ok) => { m.set_topology(ok, 1); }
                    Op::Clear => m.clear_topologies(),
                    Op::Start => { m.start(); }
                    Op::Pause => { m.pause(); }
                    Op::Stop => { m.stop(); }
                    Op::Close => { m.close(); }
                    Op::End => {
                        let told = m.end();
                        prop_assert_eq!(told, before == St::Started);
                        prop_assert_eq!(m.out.len() - n, if told { 3 } else { 0 });
                        if told {
                            prop_assert_eq!(m.out.back().unwrap().met, met::ENDED);
                            prop_assert_eq!(m.st, St::Stopped);
                        }
                        continue;
                    }
                }
                let new: Vec<E> = m.out.iter().skip(n).cloned().collect();
                let want = match o {
                    Op::Set(_) => met::TOPOLOGY_SET,
                    Op::Clear => met::TOPOLOGIES_CLEARED,
                    Op::Start => met::STARTED,
                    Op::Pause => met::PAUSED,
                    Op::Stop => met::STOPPED,
                    Op::Close => met::CLOSED,
                    Op::End => unreachable!(),
                };
                let answer: Vec<&E> = new.iter().filter(|x| x.met == want).collect();
                prop_assert_eq!(answer.len(), 1, "{:?} -> {:?}", o, new);
                if before == St::Closed {
                    prop_assert_ne!(answer[0].hr, 0, "{:?} after Close", o);
                    prop_assert_eq!(m.st, St::Closed);
                }
                // (only these come with the answers)
                prop_assert!(new.iter().all(|x| x.met == want || x.met == met::TOPOLOGY_STATUS || x.met == met::CAPABILITIES_CHANGED));
                answers.push(want);
            }
            // (the answers in the order the calls were made)
            let kinds = [met::TOPOLOGY_SET, met::TOPOLOGIES_CLEARED, met::STARTED, met::PAUSED, met::STOPPED, met::CLOSED];
            let told: Vec<u32> = m.out.iter().map(|x| x.met).filter(|t| kinds.contains(t)).collect();
            prop_assert_eq!(told, answers);
            // the caps change once, to those of a movie
            prop_assert!(m.out.iter().filter(|x| x.met == met::CAPABILITIES_CHANGED).count() <= 1);
        }
    }
}
