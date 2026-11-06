//! DirectShow's filter graph (CLSID_FilterGraph) without playback: a movie "plays" to its end
//! at once (Run posts EC_COMPLETE), so games that show an opening or logo movie go on past it.
//!
//! One guest block holds every interface: pointer-sized word k is interface k's vtable (so an
//! interface pointer is block + k * pointer size), then the refcount and the playback state.
//! CLSID_EnhancedVideoRenderer is a filter that does nothing, for games that add it to the graph.

#![allow(non_snake_case)]

use std::sync::OnceLock;

use crate::api::{Ctx, Handler};
use crate::mem::{self, ps, put32, putp, r32, w32, wp};

const E_NOINTERFACE: u32 = 0x8000_4002;
const E_ABORT: u32 = 0x8000_4004;
const EC_COMPLETE: u32 = 1;

// interfaces, by slot
const GB: u32 = 0; // IGraphBuilder / IFilterGraph(2)
const MC: u32 = 1; // IMediaControl
const ME: u32 = 2; // IMediaEventEx
const MS: u32 = 3; // IMediaSeeking
const MP: u32 = 4; // IMediaPosition
const BA: u32 = 5; // IBasicAudio
const VW: u32 = 6; // IVideoWindow
const BV: u32 = 7; // IBasicVideo
const NI: u32 = 8;
// fields after the vtables (room for 8-byte ones)
const REFS: u32 = 8 * NI;
const EVENT: u32 = REFS + 4;
const HWND: u32 = EVENT + 4;
const MSG: u32 = HWND + 4;
const LPARAM: u32 = MSG + 4;
const STATE: u32 = LPARAM + 4;
const PENDING: u32 = STATE + 4;
/// EC_COMPLETE went out since the last RenderFile: a Run again (a looping background movie,
/// rewound by SetPositions) "plays" on without end instead of completing at once forever
const DONE: u32 = PENDING + 4;
const SIZE: u32 = DONE + 4;

fn slot_of(iid: u32) -> Option<u32> {
    Some(match r32(iid) {
        0 | 0x56a8_68a9 | 0x56a8_689f | 0x36b7_3882 => GB,
        0x56a8_68b1 => MC,
        0x56a8_68b6 | 0x56a8_68c0 => ME,
        0x36b7_3880 => MS,
        0x56a8_68b2 => MP,
        0x56a8_68b3 => BA,
        0x56a8_68b4 => VW,
        0x56a8_68b5 | 0x329b_b360 => BV, // IBasicVideo(2)
        _ => return None,
    })
}

/// The block an interface pointer of slot `k` belongs to.
fn base(c: &Ctx, k: u32) -> u32 {
    c.a(0) - ps() * k
}

fn qi_k(c: &mut Ctx, k: u32) -> u32 {
    let b = base(c, k);
    match slot_of(c.a(1)) {
        Some(s) => {
            w32(b + REFS, r32(b + REFS) + 1);
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
    let b = base(c, k);
    let n = r32(b + REFS) + 1;
    w32(b + REFS, n);
    n
}
fn release_k(c: &mut Ctx, k: u32) -> u32 {
    let b = base(c, k);
    let n = r32(b + REFS).saturating_sub(1);
    w32(b + REFS, n);
    // (the block stays: a game may still hold the event handle it got from us)
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
    0x8000_4001
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
        let o = mem::heap_alloc(8, true);
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
/// a pointer-sized getter (a window handle, a BSTR)
fn get0p(c: &mut Ctx) -> u32 {
    putp(c.a(1), 0);
    0
}
fn get0_64(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    put32(c.a(1) + 4, 0);
    0
}
/// zero all four out parameters (GetWindowPosition and the like)
fn get0_4(c: &mut Ctx) -> u32 {
    for i in 1..5 {
        put32(c.a(i), 0);
    }
    0
}
fn get0_2(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    put32(c.a(2), 0);
    0
}
fn get0_2_64(c: &mut Ctx) -> u32 {
    for p in [c.a(1), c.a(2)] {
        put32(p, 0);
        if p != 0 {
            w32(p + 4, 0);
        }
    }
    0
}

const VIDEO: (u32, u32) = (1280, 720);
fn video_w(c: &mut Ctx) -> u32 {
    put32(c.a(1), VIDEO.0);
    0
}
fn video_h(c: &mut Ctx) -> u32 {
    put32(c.a(1), VIDEO.1);
    0
}
fn video_size(c: &mut Ctx) -> u32 {
    put32(c.a(1), VIDEO.0);
    put32(c.a(2), VIDEO.1);
    0
}
fn video_pos(c: &mut Ctx) -> u32 {
    for (i, v) in [0, 0, VIDEO.0, VIDEO.1].into_iter().enumerate() {
        put32(c.a(1 + i as u32), v);
    }
    0
}

const IDISPATCH: [(Handler, u32); 4] = [(notimpl, 8), (notimpl, 16), (notimpl, 24), (notimpl, 36)];

/// The movie ends as soon as it runs: queue EC_COMPLETE, signal the event, notify the window
/// (once a movie: see DONE).
fn run(c: &mut Ctx) -> u32 {
    let b = base(c, MC);
    w32(b + STATE, 2);
    if r32(b + DONE) != 0 {
        return 0;
    }
    w32(b + DONE, 1);
    w32(b + PENDING, 1);
    crate::k32::event_set(r32(b + EVENT), true);
    if r32(b + HWND) != 0 {
        let f = crate::api::export("user32", "PostMessageW");
        c.call(f, &[r32(b + HWND), r32(b + MSG), 0, r32(b + LPARAM)]);
    }
    0
}
/// RenderFile: a new movie, which completes when it runs
fn render_file_gb(c: &mut Ctx) -> u32 {
    w32(base(c, GB) + DONE, 0);
    0
}
fn render_file_mc(c: &mut Ctx) -> u32 {
    w32(base(c, MC) + DONE, 0);
    0
}
fn pause(c: &mut Ctx) -> u32 {
    w32(base(c, MC) + STATE, 1);
    0
}
fn stop(c: &mut Ctx) -> u32 {
    w32(base(c, MC) + STATE, 0);
    // (a movie run again after a stop plays to its end again)
    w32(base(c, MC) + DONE, 0);
    0
}
fn get_state(c: &mut Ctx) -> u32 {
    put32(c.a(2), r32(base(c, MC) + STATE));
    0
}

fn get_event_handle(c: &mut Ctx) -> u32 {
    putp(c.a(1), r32(base(c, ME) + EVENT));
    0
}
fn get_event(c: &mut Ctx) -> u32 {
    let b = base(c, ME);
    if r32(b + PENDING) == 0 {
        return E_ABORT;
    }
    w32(b + PENDING, 0);
    crate::k32::event_set(r32(b + EVENT), false);
    put32(c.a(1), EC_COMPLETE);
    putp(c.a(2), 0);
    putp(c.a(3), 0);
    0
}
fn wait_for_completion(c: &mut Ctx) -> u32 {
    put32(c.a(2), EC_COMPLETE);
    0
}
fn set_notify_window(c: &mut Ctx) -> u32 {
    let b = base(c, ME);
    w32(b + HWND, c.a(1));
    w32(b + MSG, c.a(2));
    w32(b + LPARAM, c.a(3));
    0
}

use crate::api::vtable;

fn vtables() -> &'static [u32; NI as usize] {
    static V: OnceLock<[u32; NI as usize]> = OnceLock::new();
    V.get_or_init(|| {
        let gb: Vec<(Handler, u32)> = iunknown!(GB)
            .into_iter()
            .chain([
                (ok as Handler, 12), // AddFilter
                (ok, 8),             // RemoveFilter
                (enum_filters, 8),   // EnumFilters
                (notimpl, 12),       // FindFilterByName
                (ok, 16),            // ConnectDirect
                (ok, 8),             // Reconnect
                (ok, 8),             // Disconnect
                (ok, 4),             // SetDefaultSyncSource
                (ok, 12),            // Connect
                (ok, 8),             // Render
                (render_file_gb, 12), // RenderFile
                (notimpl, 16),       // AddSourceFilter
                (ok, 8),             // SetLogFile
                (ok, 4),             // Abort
                (ok, 4),             // ShouldOperationContinue
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
                (notimpl, 12), // AddSourceFilter
                (notimpl, 8),
                (notimpl, 8),
                (stop, 4), // StopWhenReady
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
                (ok, 8),   // SetNotifyFlags
                (get0, 8), // GetNotifyFlags
            ])
            .collect();
        let ms: Vec<(Handler, u32)> = iunknown!(MS)
            .into_iter()
            .chain([
                (get0 as Handler, 8), // GetCapabilities
                (ok, 8),              // CheckCapabilities
                (ok, 8),              // IsFormatSupported
                (notimpl, 8),         // QueryPreferredFormat
                (notimpl, 8),         // GetTimeFormat
                (ok, 8),              // IsUsingTimeFormat
                (ok, 8),              // SetTimeFormat
                (get0_64, 8),         // GetDuration
                (get0_64, 8),         // GetStopPosition
                (get0_64, 8),         // GetCurrentPosition
                (notimpl, 24),        // ConvertTimeFormat
                (ok, 20),             // SetPositions
                (get0_2_64, 12),      // GetPositions
                (get0_2_64, 12),      // GetAvailable
                (ok, 12),             // SetRate
                (notimpl, 8),         // GetRate
                (get0_64, 8),         // GetPreroll
            ])
            .collect();
        let mp: Vec<(Handler, u32)> = iunknown!(MP)
            .into_iter()
            .chain(IDISPATCH)
            .chain([
                (get0_64 as Handler, 8), // get_Duration
                (ok, 12),                // put_CurrentPosition
                (get0_64, 8),            // get_CurrentPosition
                (get0_64, 8),            // get_StopTime
                (ok, 12),                // put_StopTime
                (get0_64, 8),            // get_PrerollTime
                (ok, 12),                // put_PrerollTime
                (ok, 12),                // put_Rate
                (notimpl, 8),            // get_Rate
                (get0, 8),               // CanSeekForward
                (get0, 8),               // CanSeekBackward
            ])
            .collect();
        let ba: Vec<(Handler, u32)> =
            iunknown!(BA).into_iter().chain(IDISPATCH).chain([(ok as Handler, 8), (get0, 8), (ok, 8), (get0, 8)]).collect();
        // IVideoWindow: 30 put/get pairs and friends, all taking one argument but these
        let mut vw: Vec<(Handler, u32)> = iunknown!(VW).into_iter().chain(IDISPATCH).collect();
        for i in 0..30 {
            // (put_* first in each pair, except BorderColor and FullScreenMode where get_ is first)
            let get = if (26..30).contains(&i) { i % 2 == 0 } else { i % 2 == 1 };
            // get_Caption, get_Owner and get_MessageDrain return pointer-sized values
            vw.push((if !get { ok } else if matches!(i, 1 | 23 | 25) { get0p } else { get0 }, 8));
        }
        vw.extend([
            (ok as Handler, 8), // SetWindowForeground
            (ok, 20),           // NotifyOwnerMessage
            (ok, 20),           // SetWindowPosition
            (get0_4, 20),       // GetWindowPosition
            (get0_2, 12),       // GetMinIdealImageSize
            (get0_2, 12),       // GetMaxIdealImageSize
            (get0_4, 20),       // GetRestorePosition
            (ok, 8),            // HideCursor
            (get0, 8),          // IsCursorHidden
        ]);
        // IBasicVideo: a 1280x720 picture, its source and destination the whole of it
        // (CatSystem2 asks for the size before it plays a movie)
        let mut bv: Vec<(Handler, u32)> = iunknown!(BV).into_iter().chain(IDISPATCH).collect();
        bv.extend([(get0_64 as Handler, 8), (get0, 8), (get0, 8), (video_w, 8), (video_h, 8)]);
        for i in 0..16 {
            // put/get Source and Destination Left, Width, Top, Height
            let get: Handler = match i {
                3 | 11 => video_w,
                7 | 15 => video_h,
                _ if i % 2 == 1 => get0,
                _ => ok,
            };
            bv.push((get, 8));
        }
        bv.extend([
            (ok as Handler, 20), // SetSourcePosition
            (video_pos, 20),     // GetSourcePosition
            (ok, 4),             // SetDefaultSourcePosition
            (ok, 20),            // SetDestinationPosition
            (video_pos, 20),     // GetDestinationPosition
            (ok, 4),             // SetDefaultDestinationPosition
            (video_size, 12),    // GetVideoSize
            (notimpl, 20),       // GetVideoPaletteEntries
            (notimpl, 12),       // GetCurrentImage
            (ok, 4),             // IsUsingDefaultSource
            (ok, 4),             // IsUsingDefaultDestination
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
    // CLSID_FilterGraph {e436ebb3-524f-11ce-9f53-0020af0ba770}, FilterGraphNoThread {e436ebb8-...}
    if r32(clsid) == 0xfa10_746c && r32(clsid + 4) == 0x4b6c_9b63 {
        return evr(iid);
    }
    if !matches!(r32(clsid), 0xe436_ebb3 | 0xe436_ebb8) || r32(clsid + 4) != 0x11ce_524f {
        return None;
    }
    let s = slot_of(iid)?;
    let b = mem::heap_alloc(SIZE, true);
    for (k, v) in vtables().iter().enumerate() {
        wp(b + ps() * k as u32, *v);
    }
    w32(b + REFS, 1);
    w32(b + EVENT, crate::obj::add(crate::obj::Obj::Event { manual: true, set: false }));
    Some(b + ps() * s)
}

// The EVR: IBaseFilter, IMFGetService and IMFVideoDisplayControl (what GetService hands out),
// each one word of the block like the graph's interfaces; every method succeeds and does nothing.
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
        let rest = |q: Handler, n: usize| -> Vec<(Handler, u32)> {
            [(q, 12), (one as Handler, 4), (one as Handler, 4)].into_iter().chain((0..n).map(|_| (ok as Handler, 4))).collect()
        };
        let mut gs = rest(q1, 0);
        gs.push((evr_get_service, 16));
        // IBaseFilter: GetClassID, Stop, Pause, Run, GetState, SetSyncSource, GetSyncSource,
        // EnumPins, FindPin, QueryFilterInfo, JoinFilterGraph, QueryVendorInfo
        let mut bf = rest(q0, 0);
        bf.extend([(notimpl as Handler, 8), (ok, 4), (ok, 4), (ok, 12), (notimpl, 12), (ok, 8), (notimpl, 8)]);
        bf.extend([(notimpl as Handler, 8), (notimpl, 12), (notimpl, 8), (ok, 12), (notimpl, 8)]);
        [vtable("EVR::IBaseFilter", &bf), vtable("EVR::IMFGetService", &gs), vtable("EVR::IMFVideoDisplayControl", &rest(q2, 20))]
    });
    let b = mem::heap_alloc(8 * 3, true);
    for (k, t) in v.iter().enumerate() {
        wp(b + ps() * k as u32, *t);
    }
    Some(b + ps() * s)
}
