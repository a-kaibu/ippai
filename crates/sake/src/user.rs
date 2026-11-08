//! user32: windows, message queues, input, timers, menus (kept, not drawn), resources.
//!
//! All USER state is behind one lock, never held while guest code runs. Top-level windows
//! have a surface (their client area, BGRA) that GDI draws into and that goes to kura's
//! display; non-client areas are accounted for (AdjustWindowRect and friends) but not drawn.

#![allow(non_snake_case)]

use crate::HashMap;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use crate::api::{implement, Ctx};
use crate::k32::{arg_str, ret_str};
use crate::mem::{self, cstr, put32, r16, r32, w16, w32, wstr};
use crate::text::{a2s, a2w, w2a};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub struct Rect {
    pub l: i32,
    pub t: i32,
    pub r: i32,
    pub b: i32,
}

impl Rect {
    pub fn read(p: u32) -> Rect {
        Rect { l: r32(p) as i32, t: r32(p + 4) as i32, r: r32(p + 8) as i32, b: r32(p + 12) as i32 }
    }
    pub fn write(&self, p: u32) {
        if p == 0 {
            return;
        }
        w32(p, self.l as u32);
        w32(p + 4, self.t as u32);
        w32(p + 8, self.r as u32);
        w32(p + 12, self.b as u32);
    }
    pub fn w(&self) -> i32 {
        self.r - self.l
    }
    pub fn h(&self) -> i32 {
        self.b - self.t
    }
    pub fn empty(&self) -> bool {
        self.l >= self.r || self.t >= self.b
    }
    pub fn and(&self, o: &Rect) -> Rect {
        Rect { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) }
    }
    pub fn or(&self, o: &Rect) -> Rect {
        if self.empty() {
            return *o;
        }
        if o.empty() {
            return *self;
        }
        Rect { l: self.l.min(o.l), t: self.t.min(o.t), r: self.r.max(o.r), b: self.b.max(o.b) }
    }
    pub fn off(&self, x: i32, y: i32) -> Rect {
        Rect { l: self.l + x, t: self.t + y, r: self.r + x, b: self.b + y }
    }
    pub fn has(&self, x: i32, y: i32) -> bool {
        x >= self.l && x < self.r && y >= self.t && y < self.b
    }
    /// The bounding box of what is left of self once o is taken away
    pub fn minus(&self, o: &Rect) -> Rect {
        let mut r = *self;
        if r.empty() || o.l <= r.l && o.t <= r.t && o.r >= r.r && o.b >= r.b {
            return Rect::default();
        }
        if o.l <= r.l && o.r >= r.r {
            if o.t <= r.t && o.b > r.t {
                r.t = o.b;
            } else if o.b >= r.b && o.t < r.b {
                r.b = o.t;
            }
        } else if o.t <= r.t && o.b >= r.b {
            if o.l <= r.l && o.r > r.l {
                r.l = o.r;
            } else if o.r >= r.r && o.l < r.r {
                r.r = o.l;
            }
        }
        r
    }
}

pub struct Surface {
    pub w: i32,
    pub h: i32,
    pub px: Vec<u32>,
    /// the top-level window whose display shows `px` directly (`flush`), 0 for none
    pub shown_by: u32,
}

impl Surface {
    fn new(w: i32, h: i32) -> Surface {
        Surface { w, h, px: vec![0xff00_0000; (w * h) as usize], shown_by: 0 }
    }
    /// before `px` is freed or moved: the display stops reading it
    fn unshare(&mut self) {
        if self.shown_by != 0 {
            kura::display::vnhost_window_unshare(self.shown_by);
            self.shown_by = 0;
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.unshare();
    }
}

pub struct Class {
    pub name: String,
    pub style: u32,
    pub proc_: u32,
    pub cls_extra: u32,
    pub wnd_extra: u32,
    pub inst: u32,
    pub icon: u32,
    pub cursor: u32,
    pub brush: u32,
    pub menu: u32,
    pub unicode: bool,
    pub atom: u32,
    pub extra: Vec<u8>,
    /// UnregisterClass'd (kept: windows refer to classes by index)
    pub unregistered: bool,
}

pub struct Window {
    pub class: usize,
    pub proc_: u32,
    pub style: u32,
    pub exstyle: u32,
    pub parent: u32,
    pub owner: u32,
    pub id: u32,
    pub inst: u32,
    pub user: u32,
    /// window rect: screen coordinates for top-level windows, the parent's client
    /// coordinates for child windows
    pub rect: Rect,
    /// the client area inside `rect` (window coordinates: 0,0 = rect's corner)
    pub client: Rect,
    pub text: Vec<u16>,
    pub visible: bool,
    pub enabled: bool,
    pub tid: u32,
    pub extra: Vec<u8>,
    pub props: Vec<(String, u32)>,
    pub update: Rect,
    pub erase: bool,
    pub menu: u32,
    pub unicode: bool,
    pub surface: Option<Arc<Mutex<Surface>>>,
    pub children: Vec<u32>,
    /// shown at least once (the first ShowWindow sends WM_SIZE and WM_MOVE)
    pub shown: bool,
    /// SB_HORZ, SB_VERT and SB_CTL's range, page and position
    pub scroll: [Scroll; 3],
}

/// A scroll bar's state (SCROLLINFO): (min, max, page, pos, track pos)
#[derive(Clone, Copy, Default)]
pub struct Scroll {
    pub min: i32,
    pub max: i32,
    pub page: u32,
    pub pos: i32,
    pub track: i32,
}
impl Scroll {
    /// the position within [min, max - max(page - 1, 0)], as Windows keeps it
    fn clamp(&mut self) {
        let span = (self.max as i64 - self.min as i64 + 1).max(0) as u64;
        self.page = (self.page as u64).min(span) as u32;
        let top = (self.max as i64 - (self.page as i64 - 1).max(0)).max(self.min as i64);
        self.pos = (self.pos as i64).clamp(self.min as i64, top) as i32;
    }
}

#[derive(Clone, Copy, Default)]
pub struct Msg {
    pub hwnd: u32,
    pub msg: u32,
    pub w: u32,
    pub l: u32,
    pub time: u32,
    pub x: i32,
    pub y: i32,
}

impl Msg {
    fn write(&self, p: u32) {
        if mem::x64() {
            // hwnd, message, wParam, lParam (8-byte aligned), time, pt, lPrivate: 48 bytes
            mem::wp(p, self.hwnd);
            mem::w64(p + 8, self.msg as u64);
            mem::w64(p + 16, self.w as u64);
            mem::w64(p + 24, self.l as i32 as i64 as u64);
            w32(p + 32, self.time);
            w32(p + 36, self.x as u32);
            w32(p + 40, self.y as u32);
            w32(p + 44, 0);
            return;
        }
        w32(p, self.hwnd);
        w32(p + 4, self.msg);
        w32(p + 8, self.w);
        w32(p + 12, self.l);
        w32(p + 16, self.time);
        w32(p + 20, self.x as u32);
        w32(p + 24, self.y as u32);
    }
}

/// A MSG's (hwnd, message, wParam, lParam).
fn msg_at(p: u32) -> (u32, u32, u32, u32) {
    if mem::x64() { (r32(p), r32(p + 8), r32(p + 16), r32(p + 24)) } else { (r32(p), r32(p + 4), r32(p + 8), r32(p + 12)) }
}

struct Timer {
    hwnd: u32,
    id: u32,
    ms: u32,
    proc_: u32,
    due: Instant,
}

/// A SendMessage from another thread, waiting for its result (and the moment it may go on).
struct Sent {
    msg: Msg,
    result: Arc<(Mutex<Option<(u32, Instant)>>, std::sync::Condvar)>,
    /// the sending thread (0: another process)
    from: u32,
    /// from another process: its message's id and the copy of WM_COPYDATA's data (freed after)
    remote: Option<(u32, u32)>,
}

#[derive(Default)]
struct Queue {
    posted: VecDeque<Msg>,
    input: VecDeque<Msg>,
    sent: VecDeque<Sent>,
    timers: Vec<Timer>,
    quit: Option<u32>,
    /// QS_* bits of messages that came since the last GetQueueStatus (posted, sent)
    fresh: u32,
}

pub struct Menu {
    pub items: Vec<MenuItem>,
}

#[derive(Clone, Default)]
pub struct MenuItem {
    pub id: u32,
    pub flags: u32,
    pub text: Vec<u16>,
    pub sub: u32,
    pub data: u32,
}

pub struct State {
    pub classes: Vec<Class>,
    pub wins: HashMap<u32, Window>,
    next: u32,
    queues: HashMap<u32, Queue>,
    pub menus: HashMap<u32, Menu>,
    pub focus: u32,
    pub active: u32,
    pub capture: u32,
    pub cursor: (i32, i32),
    pub keys: [u8; 256],
    /// pressed since GetAsyncKeyState last asked (a game polling once a frame still sees a
    /// click shorter than its frame)
    pub pressed: [bool; 256],
    pub screen: (i32, i32),
    pub cursor_handle: u32,
    pub cursor_count: i32,
    pub msgnames: HashMap<String, u32>,
    pub last_msg_pos: (i32, i32),
    pub last_msg_time: u32,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);

pub fn st() -> MutexGuard<'static, Option<State>> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

pub fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    let mut g = st();
    f(g.get_or_insert_with(|| {
        let mut s = 0i32;
        let mut h = 0i32;
        unsafe { kura::display::vnhost_display_size(&mut s, &mut h) };
        State {
            classes: Vec::new(),
            wins: HashMap::default(),
            next: first_hwnd(),
            queues: HashMap::default(),
            menus: HashMap::default(),
            focus: 0,
            active: 0,
            capture: 0,
            cursor: (s / 2, h / 2),
            keys: [0; 256],
            pressed: [false; 256],
            screen: (s, h),
            cursor_handle: 0,
            cursor_count: 0,
            msgnames: HashMap::default(),
            last_msg_pos: (0, 0),
            last_msg_time: 0,
        }
    }))
}

pub const DESKTOP: u32 = 0x10010;

pub fn screen() -> (i32, i32) {
    with(|s| s.screen)
}

// ---------------------------------------------------------------------------------------------
// metrics

pub fn metric(i: u32) -> i32 {
    let (w, h) = screen();
    match i {
        0 | 16 | 61 | 78 => w,
        1 | 62 | 79 => h,
        17 => h - 19,
        2 | 3 | 9 | 10 | 20 | 21 => 16,
        4 => 23,
        5 | 6 => 1,
        7 | 8 => 3,
        11 | 12 | 13 | 14 => 32,
        15 => 20,
        19 => 1,
        28 => 112,
        29 => 27,
        30 | 31 => 18,
        32 | 33 => 4,
        34 => 112,
        35 => 27,
        36 | 37 => 4,
        38 | 39 => 75,
        42 => (crate::text::is_dbcs()) as i32,
        43 => 3,
        45 | 46 => 2,
        49 | 50 => 16,
        51 => 15,
        52 | 53 => 13,
        54 | 55 => 18,
        59 | 60 => w + 12,
        63 => 3,
        71 | 72 => 16,
        75 => 1,
        80 => 1,
        81 => 1,
        82 => (crate::text::is_dbcs()) as i32,
        92 => 4,
        _ => 0,
    }
}

/// The non-client margins of a window style: (left, top, right, bottom), with Windows 10's
/// metrics at 96 dpi (caption 23, menu 20, a sizing frame 4 plus the padded border 4).
fn frame(style: u32, exstyle: u32, menu: bool) -> (i32, i32, i32, i32) {
    let child = style & 0x4000_0000 != 0;
    let mut b = 0;
    if style & 0x0004_0000 != 0 {
        b += 8;
    } else if style & 0x0040_0000 != 0 || exstyle & 1 != 0 {
        b += 3;
    } else if style & 0x0080_0000 != 0 {
        b += 1;
    }
    if exstyle & 0x200 != 0 {
        b += 2;
    }
    let mut top = b;
    if style & 0x00c0_0000 == 0x00c0_0000 {
        top += if exstyle & 0x80 != 0 { 15 } else { 23 };
    }
    if menu && !child {
        top += 20;
    }
    (b, top, b, b)
}

fn client_of(rect: &Rect, style: u32, exstyle: u32, menu: bool) -> Rect {
    let (l, t, r, b) = frame(style, exstyle, menu);
    let c = Rect { l, t, r: rect.w() - r, b: rect.h() - b };
    if c.empty() {
        Rect { l, t, r: l.max(c.r), b: t.max(c.b) }
    } else {
        c
    }
}

// ---------------------------------------------------------------------------------------------
// windows

pub fn is_window(h: u32) -> bool {
    h == DESKTOP || with(|s| s.wins.contains_key(&h))
}

/// The top-level ancestor of `h` and the offset of `h`'s client area in its client area.
fn toplevel_offset(s: &State, mut h: u32) -> Option<(u32, i32, i32)> {
    let w = s.wins.get(&h)?;
    let (mut x, mut y) = (w.client.l, w.client.t);
    loop {
        let w = s.wins.get(&h)?;
        if w.parent == 0 || w.parent == DESKTOP {
            // the client area of the top-level window is the surface origin
            return Some((h, x - w.client.l, y - w.client.t));
        }
        let p = s.wins.get(&w.parent)?;
        x += w.rect.l + p.client.l;
        y += w.rect.t + p.client.t;
        h = w.parent;
    }
}

/// Where a window's client area is in screen coordinates.
/// A window's client area in screen coordinates (None: no such window).
pub fn client_rect_screen(h: u32) -> Option<Rect> {
    with(|s| s.wins.contains_key(&h).then(|| client_screen(s, h)))
}

pub fn client_screen(s: &State, h: u32) -> Rect {
    if h == DESKTOP || h == 0 {
        return Rect { l: 0, t: 0, r: s.screen.0, b: s.screen.1 };
    }
    let Some(w) = s.wins.get(&h) else { return Rect::default() };
    let (x, y) = window_origin(s, h);
    w.client.off(x, y)
}

/// Screen position of a window's top-left corner.
fn window_origin(s: &State, h: u32) -> (i32, i32) {
    let Some(w) = s.wins.get(&h) else { return (0, 0) };
    if w.parent == 0 || w.parent == DESKTOP {
        return (w.rect.l, w.rect.t);
    }
    let pc = client_screen(s, w.parent);
    (pc.l + w.rect.l, pc.t + w.rect.t)
}

pub struct Target {
    pub surface: Arc<Mutex<Surface>>,
    pub top: u32,
    /// the window's client area in the surface
    pub x: i32,
    pub y: i32,
    pub clip: Rect,
}

/// Where a window's client area is drawn (for its DCs).
pub fn target(h: u32) -> Option<Target> {
    with(|s| {
        let (top, x, y) = toplevel_offset(s, h)?;
        let surface = s.wins.get(&top)?.surface.clone()?;
        let w = s.wins.get(&h)?;
        let mut clip = Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() }.off(x, y);
        // a CS_PARENTDC child draws anywhere in its parent's client area
        let child = w.parent != 0 && w.parent != DESKTOP;
        if child && s.classes[w.class].style & 0x80 != 0 {
            let (_, px, py) = toplevel_offset(s, w.parent)?;
            let p = s.wins.get(&w.parent)?;
            clip = Rect { l: 0, t: 0, r: p.client.w(), b: p.client.h() }.off(px, py);
        }
        // clipped by the client areas of the windows it is in
        let mut p = w.parent;
        while p != 0 && p != DESKTOP {
            let (_, px, py) = toplevel_offset(s, p)?;
            let pw = s.wins.get(&p)?;
            clip = clip.and(&Rect { l: 0, t: 0, r: pw.client.w(), b: pw.client.h() }.off(px, py));
            p = pw.parent;
        }
        Some(Target { surface, top, x, y, clip })
    })
}

/// Sends part of a top-level window's surface to the display.
pub fn flush(top: u32, r: Rect) {
    let Some(surf) = with(|s| s.wins.get(&top).and_then(|w| w.surface.clone())) else { return };
    let mut g = surf.lock().unwrap();
    let r = r.and(&Rect { l: 0, t: 0, r: g.w, b: g.h });
    if r.empty() {
        return;
    }
    let d = [r.l, r.t, r.r, r.b];
    // the display shows the surface itself (no copy per frame)
    if g.shown_by != top {
        g.unshare();
        g.shown_by = top;
    }
    unsafe { kura::display::vnhost_window_draw_shared(top, d.as_ptr(), g.px.as_ptr(), g.w, g.h) };
}

/// What the display gets of a client area (screen coordinates): at most the 8192x8192 its
/// surface has (resize_surface), whatever size a program gave the window.
fn shown_rect(c: Rect) -> [i32; 8] {
    let (r, b) = (c.l.saturating_add(c.w().clamp(0, 8192)), c.t.saturating_add(c.h().clamp(0, 8192)));
    [c.l, c.t, r, b, c.l, c.t, r, b]
}

/// Tells the display where a top-level window is (its client area is what it shows).
fn update_display(s: &State, h: u32) {
    let Some(w) = s.wins.get(&h) else { return };
    if w.parent != 0 && w.parent != DESKTOP {
        return;
    }
    let r = shown_rect(w.client.off(w.rect.l, w.rect.t));
    let c = Rect { l: r[0], t: r[1], r: r[2], b: r[3] };
    let shown = w.visible && w.style & 0x4000_0000 == 0 && c.w() > 0 && c.h() > 0;
    unsafe { kura::display::vnhost_window_pos(h, r.as_ptr(), shown as i32, 0, 0) };
    // owned windows (dialogs) stay above their owner, as on Windows: RealLive's fullscreen
    // settings dialog is shown before its topmost fullscreen owner
    if shown {
        for (&o, ow) in s.wins.iter().filter(|(_, ow)| ow.owner == h && ow.visible && (ow.parent == 0 || ow.parent == DESKTOP)) {
            let r = shown_rect(ow.client.off(ow.rect.l, ow.rect.t));
            unsafe { kura::display::vnhost_window_pos(o, r.as_ptr(), 1, 1, 0) };
        }
    }
}

fn resize_surface(w: &mut Window) {
    if w.parent != 0 && w.parent != DESKTOP {
        return;
    }
    // (the pixels of a huge window: its first 8192x8192, drawing is clipped to the surface)
    let (cw, ch) = (w.client.w().clamp(1, 8192), w.client.h().clamp(1, 8192));
    match &w.surface {
        Some(s) => {
            let mut g = s.lock().unwrap();
            if g.w != cw || g.h != ch {
                g.unshare();
                let mut px = vec![0xff00_0000u32; (cw * ch) as usize];
                for y in 0..g.h.min(ch) {
                    let n = g.w.min(cw) as usize;
                    px[(y * cw) as usize..(y * cw) as usize + n].copy_from_slice(&g.px[(y * g.w) as usize..(y * g.w) as usize + n]);
                }
                g.px = px;
                g.w = cw;
                g.h = ch;
            }
        }
        None => w.surface = Some(Arc::new(Mutex::new(Surface::new(cw, ch)))),
    }
}

fn class_index(s: &State, name: u32, wide: bool) -> Option<usize> {
    if name < 0x10000 {
        return s.classes.iter().position(|c| c.atom == name && !c.unregistered);
    }
    let n = arg_str(name, wide).to_lowercase();
    s.classes.iter().position(|c| c.name.to_lowercase() == n && !c.unregistered)
}

fn builtin_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("builtin_proc", |c| def_proc(c, msg_wide()), 16))
}

// whether the message being sent came through a W function (SendMessageW, CreateWindowExW): the
// built-in classes' procedures read text parameters with it (no A/W message translation)
thread_local!(static WIDE_MSG: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
fn msg_wide() -> bool {
    WIDE_MSG.with(|w| w.get())
}
fn as_wide<R>(wide: bool, f: impl FnOnce() -> R) -> R {
    let old = WIDE_MSG.with(|w| w.replace(wide));
    let r = f();
    WIDE_MSG.with(|w| w.set(old));
    r
}

// ---------------------------------------------------------------------------------------------
// the Button and Static controls (launcher and settings dialogs): drawn with GDI, clicks
// reach the parent as WM_COMMAND

#[derive(Default, Clone, Copy)]
struct Ctl {
    font: u32,
    check: u32,
    pressed: bool,
}

static CTLS: Mutex<Option<HashMap<u32, Ctl>>> = Mutex::new(None);

fn ctl(h: u32) -> Ctl {
    CTLS.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).copied().unwrap_or_default()
}
fn set_ctl(h: u32, f: impl FnOnce(&mut Ctl)) {
    f(CTLS.lock().unwrap().get_or_insert_with(HashMap::default).entry(h).or_default());
}

/// Combo boxes (settings dialogs): their items and selection, drawn as a box with the selected
/// item; a click selects the next item (no drop-down list) and tells the parent CBN_SELCHANGE.
#[derive(Default)]
struct Combo {
    items: Vec<(Vec<u16>, u32)>,
    sel: i32,
}
static COMBOS: Mutex<Option<HashMap<u32, Combo>>> = Mutex::new(None);
fn combo_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("combo_proc", combo_proc_fn, 16))
}
fn combo<R>(h: u32, f: impl FnOnce(&mut Combo) -> R) -> R {
    let mut g = COMBOS.lock().unwrap();
    let e = g.get_or_insert_with(HashMap::default).entry(h).or_insert_with(|| Combo { items: Vec::new(), sel: -1 });
    f(e)
}
fn combo_proc_fn(c: &mut Ctx) -> u32 {
    const CB_ERR: u32 = u32::MAX;
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let wide = msg_wide();
    let text = |p: u32| -> Vec<u16> { if p == 0 { Vec::new() } else { crate::k32::arg_str(p, wide).encode_utf16().collect() } };
    let find = |items: &[(Vec<u16>, u32)], from: u32, s: &[u16], exact: bool| -> u32 {
        let lower = |v: &[u16]| String::from_utf16_lossy(v).to_lowercase();
        let want = lower(s);
        let n = items.len();
        let start = if from as i32 >= 0 && (from as usize) < n { from as usize + 1 } else { 0 };
        (0..n).map(|k| (start + k) % n.max(1)).find(|&i| if exact { lower(&items[i].0) == want } else { lower(&items[i].0).starts_with(&want) }).map_or(CB_ERR, |i| i as u32)
    };
    let changed = |c: &mut Ctx| {
        invalidate(h, None, true);
        let (id, parent) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.id, w.parent)));
        if parent != 0 {
            send(c, parent, 0x111, 1 << 16 | (id & 0xffff), h);
        }
    };
    match m {
        // CB_ADDSTRING, CB_INSERTSTRING
        0x143 | 0x14a => {
            let t = text(lp);
            let r = combo(h, |cb| {
                let at = if m == 0x14a && (wp as usize) < cb.items.len() { wp as usize } else { cb.items.len() };
                cb.items.insert(at, (t, 0));
                at as u32
            });
            invalidate(h, None, true);
            r
        }
        // CB_DELETESTRING
        0x144 => combo(h, |cb| {
            if (wp as usize) < cb.items.len() {
                cb.items.remove(wp as usize);
                if cb.sel >= cb.items.len() as i32 {
                    cb.sel = -1;
                }
                cb.items.len() as u32
            } else {
                CB_ERR
            }
        }),
        0x146 => combo(h, |cb| cb.items.len() as u32),
        0x147 => combo(h, |cb| cb.sel as u32),
        // CB_GETLBTEXT, CB_GETLBTEXTLEN
        0x148 | 0x149 => {
            let Some(t) = combo(h, |cb| cb.items.get(wp as usize).map(|i| i.0.clone())) else { return CB_ERR };
            let bytes = if wide { t.clone() } else { Vec::new() };
            let a = if wide { Vec::new() } else { crate::text::s2a(&String::from_utf16_lossy(&t)) };
            if m == 0x149 {
                return if wide { bytes.len() as u32 } else { a.len() as u32 };
            }
            if wide {
                mem::put_wstr(lp, bytes.len() as u32 + 1, &bytes);
                bytes.len() as u32
            } else {
                mem::put_cstr(lp, a.len() as u32 + 1, &a);
                a.len() as u32
            }
        }
        0x14b => {
            combo(h, |cb| {
                cb.items.clear();
                cb.sel = -1;
            });
            invalidate(h, None, true);
            0
        }
        // CB_FINDSTRING, CB_FINDSTRINGEXACT
        0x14c | 0x158 => {
            let t = text(lp);
            combo(h, |cb| find(&cb.items, wp, &t, m == 0x158))
        }
        // CB_SELECTSTRING
        0x14d => {
            let t = text(lp);
            let r = combo(h, |cb| {
                let i = find(&cb.items, wp, &t, false);
                if i != CB_ERR {
                    cb.sel = i as i32;
                }
                i
            });
            invalidate(h, None, true);
            r
        }
        // CB_SETCURSEL
        0x14e => {
            let r = combo(h, |cb| {
                if (wp as usize) < cb.items.len() {
                    cb.sel = wp as i32;
                    wp
                } else {
                    cb.sel = -1;
                    CB_ERR
                }
            });
            invalidate(h, None, true);
            r
        }
        0x150 => combo(h, |cb| cb.items.get(wp as usize).map_or(CB_ERR, |i| i.1)),
        0x151 => combo(h, |cb| match cb.items.get_mut(wp as usize) {
            Some(i) => {
                i.1 = lp;
                0
            }
            None => CB_ERR,
        }),
        // CB_GETDROPPEDSTATE, CB_SHOWDROPDOWN, CB_SETEXTENDEDUI, CB_LIMITTEXT...
        0x157 | 0x14f | 0x155 | 0x141 => 0,
        0xf => {
            combo_paint(c, h);
            0
        }
        0x14 => 1,
        0x30 => {
            set_ctl(h, |t| t.font = wp);
            0
        }
        0x31 => ctl(h).font,
        // a click: the next item
        0x201 => {
            let moved = combo(h, |cb| {
                if cb.items.is_empty() {
                    return false;
                }
                cb.sel = (cb.sel + 1).rem_euclid(cb.items.len() as i32);
                true
            });
            if moved {
                changed(c);
            }
            0
        }
        0x2 => {
            COMBOS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
            def_proc(c, wide)
        }
        _ => def_proc(c, wide),
    }
}
fn combo_paint(c: &mut Ctx, h: u32) {
    let (w, hh, enabled) = with(|s| s.wins.get(&h).map_or((0, 0, true), |w| (w.client.w(), w.client.h(), w.enabled)));
    let text = combo(h, |cb| cb.items.get(cb.sel.max(0) as usize).filter(|_| cb.sel >= 0).map(|i| i.0.clone()).unwrap_or_default());
    let ps = mem::heap_alloc(64 + 16, true);
    let rc = ps + 64;
    let dc = gcall(c, "user32", "BeginPaint", &[h, ps]);
    let font = ctl(h).font;
    if font != 0 {
        gcall(c, "gdi32", "SelectObject", &[dc, font]);
    }
    gcall(c, "gdi32", "SetBkMode", &[dc, 1]);
    gcall(c, "gdi32", "SetTextColor", &[dc, if enabled { 0 } else { 0x6d6d6d }]);
    // (the box: the top of the window, whose height includes the list's)
    let bh = hh.min(22);
    let fill = |r: Rect, color: u32| crate::gdi::fill_rect_brush(dc, r, crate::gdi::solid_brush(color));
    fill(Rect { l: 0, t: 0, r: w, b: bh }, 0x7a7a7a);
    fill(Rect { l: 1, t: 1, r: w - 1, b: bh - 1 }, if enabled { 0xffffff } else { 0xf0f0f0 });
    // the arrow
    let ax = w - 12;
    for k in 0..4 {
        fill(Rect { l: ax - 3 + k, t: bh / 2 - 2 + k, r: ax + 4 - k, b: bh / 2 - 1 + k }, 0x404040);
    }
    Rect { l: 4, t: 0, r: w - 20, b: bh }.write(rc);
    crate::gdi::draw_text(dc, &text, rc, 0x4 | 0x20);
    gcall(c, "user32", "EndPaint", &[h, ps]);
    mem::heap_free(ps);
}

fn control_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("control_proc", control_proc_fn, 16))
}

#[path = "tabctl.rs"]
mod tabctl;
fn tab_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("tab_proc", tabctl::proc_fn, 16))
}
fn gcall(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let f = crate::api::export(dll, name);
    c.call(f, args)
}

fn ctl_paint(c: &mut Ctx, h: u32, button: bool) {
    let (style, text, id, parent, (w, hh), enabled) = with(|s| {
        let w = &s.wins[&h];
        (w.style, w.text.clone(), w.id, w.parent, (w.client.w(), w.client.h()), w.enabled)
    });
    let st = ctl(h);
    let ps = mem::heap_alloc(64 + 16, true);
    let rc = ps + 64;
    let dc = gcall(c, "user32", "BeginPaint", &[h, ps]);
    if st.font != 0 {
        gcall(c, "gdi32", "SelectObject", &[dc, st.font]);
    }
    gcall(c, "gdi32", "SetBkMode", &[dc, 1]);
    gcall(c, "gdi32", "SetTextColor", &[dc, if enabled { 0 } else { 0x6d6d6d }]);
    // the parent may give the background (WM_CTLCOLORSTATIC / WM_CTLCOLORBTN)
    let mut bg = if parent != 0 { send(c, parent, if button { 0x135 } else { 0x138 }, dc, h) } else { 0 };
    if bg == 0 {
        bg = crate::gdi::solid_brush(0xf0f0f0);
    }
    let full = Rect { l: 0, t: 0, r: w, b: hh };
    let kind = style & 0xf;
    // (a group box leaves its inside alone: the controls in it may be under it)
    let group = button && kind == 7;
    if !group {
        crate::gdi::fill_rect_brush(dc, full, bg);
    }
    let fill = |r: Rect, color: u32| crate::gdi::fill_rect_brush(dc, r, crate::gdi::solid_brush(color));
    let frame = |r: Rect, color: u32| {
        fill(Rect { l: r.l, t: r.t, r: r.r, b: r.t + 1 }, color);
        fill(Rect { l: r.l, t: r.b - 1, r: r.r, b: r.b }, color);
        fill(Rect { l: r.l, t: r.t, r: r.l + 1, b: r.b }, color);
        fill(Rect { l: r.r - 1, t: r.t, r: r.r, b: r.b }, color);
    };
    let mut tr = full;
    let mut flags;
    let edit = with(|s| s.wins.get(&h).is_some_and(|w| s.classes[w.class].name.eq_ignore_ascii_case("edit")));
    if edit {
        // a white box, the text left and centred vertically
        fill(full, 0xffffff);
        frame(full, 0x7a7a7a);
        tr.l = 4;
        flags = 0x4 | 0x20;
    } else if !button {
        if style & 0x1000 != 0 {
            // SS_SUNKEN: a one-pixel sunken edge (WS_EX_STATICEDGE)
            let mut e = full;
            draw_edge(dc, &mut e, 2, 0xf);
            tr = Rect { l: 1, t: 1, r: w - 1, b: hh - 1 };
        }
        flags = match style & 3 {
            1 => 0x1,
            2 => 0x2,
            _ => 0,
        } | 0x10;
        if style & 0x200 != 0 {
            flags |= 0x4 | 0x20;
        }
    } else if matches!(kind, 2 | 3 | 4 | 5 | 6 | 9) && kind != 7 {
        // check box / radio button: a box, then the text
        let by = (hh - 13) / 2;
        let b = Rect { l: 0, t: by, r: 13, b: by + 13 };
        fill(b, 0xffffff);
        frame(b, 0x333333);
        if st.check == 1 {
            fill(Rect { l: 3, t: by + 3, r: 10, b: by + 10 }, 0x202020);
        }
        tr.l = 17;
        flags = 0x4 | 0x20;
    } else if group {
        // an etched frame from half the text height down, the label over it at 7 pixels in on
        // the parent's background (a pixel wider each side)
        let tm = mem::heap_alloc(64, true);
        gcall(c, "gdi32", "GetTextMetricsW", &[dc, tm]);
        let th = r32(tm) as i32;
        mem::heap_free(tm);
        let mut f = Rect { t: th / 2 - 1, ..full };
        draw_edge(dc, &mut f, 6, 0xf | if style & 0x8000 != 0 { 0x4000 } else { 0 });
        if !text.is_empty() {
            let mut ext = Rect { l: 7, t: 0, r: w - 7, b: hh }; // DT_CALCRECT of the label
            ext.write(rc);
            crate::gdi::draw_text(dc, &text, rc, 0x20 | 0x400);
            ext = Rect::read(rc);
            crate::gdi::fill_rect_brush(dc, Rect { l: ext.l - 1, t: ext.t, r: ext.r + 1, b: ext.b + 1 }, bg);
            tr = ext;
        }
        flags = 0x20;
    } else {
        fill(Rect { l: 1, t: 1, r: w - 1, b: hh - 1 }, if st.pressed { 0xf7e4cc } else { 0xe1e1e1 });
        frame(full, if st.pressed { 0xd77800 } else { 0xadadad });
        flags = 0x1 | 0x4 | 0x20;
    }
    let _ = id;
    tr.write(rc);
    crate::gdi::draw_text(dc, &text, rc, flags);
    gcall(c, "user32", "EndPaint", &[h, ps]);
    mem::heap_free(ps);
}

fn ctl_click(c: &mut Ctx, h: u32) {
    let (style, id, parent) = with(|s| s.wins.get(&h).map_or((0, 0, 0), |w| (w.style, w.id, w.parent)));
    match style & 0xf {
        3 | 6 => set_ctl(h, |t| t.check = if t.check == 1 { 0 } else { 1 }),
        5 => set_ctl(h, |t| t.check = (t.check + 1) % 3),
        9 => {
            // auto radio: this one on, its siblings off
            let sibs: Vec<u32> = with(|s| s.wins.get(&parent).map(|p| p.children.clone()).unwrap_or_default());
            for k in sibs {
                let radio = with(|s| s.wins.get(&k).is_some_and(|w| w.style & 0xf == 9));
                if radio && k != h {
                    set_ctl(k, |t| t.check = 0);
                    invalidate(k, None, true);
                }
            }
            set_ctl(h, |t| t.check = 1);
        }
        _ => {}
    }
    invalidate(h, None, true);
    if parent != 0 {
        send(c, parent, 0x111, id & 0xffff, h);
    }
}

fn control_proc_fn(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let button = with(|s| s.wins.get(&h).is_some_and(|w| s.classes[w.class].name.eq_ignore_ascii_case("button")));
    match m {
        0xf => {
            ctl_paint(c, h, button);
            0
        }
        0x14 => 1,
        0x30 => {
            set_ctl(h, |t| t.font = wp);
            if lp != 0 {
                invalidate(h, None, true);
            }
            0
        }
        0x31 => ctl(h).font,
        0x87 if button => 0x2000,
        0x201 | 0x203 if button => {
            set_ctl(h, |t| t.pressed = true);
            with(|s| s.capture = h);
            invalidate(h, None, true);
            0
        }
        0x202 if button => {
            let was = ctl(h).pressed;
            set_ctl(h, |t| t.pressed = false);
            with(|s| {
                if s.capture == h {
                    s.capture = 0
                }
            });
            invalidate(h, None, true);
            let (x, y) = ((lp & 0xffff) as i16 as i32, (lp >> 16) as i16 as i32);
            let inside = with(|s| s.wins.get(&h).is_some_and(|w| x >= 0 && y >= 0 && x < w.client.w() && y < w.client.h()));
            if was && inside {
                ctl_click(c, h);
            }
            0
        }
        // the edit control: typed characters (no caret, no selection)
        0x7 | 0x8 | 0x102 | 0x201 | 0x87 if with(|s| s.wins.get(&h).is_some_and(|w| s.classes[w.class].name.eq_ignore_ascii_case("edit"))) => {
            match m {
                // EN_SETFOCUS / EN_KILLFOCUS to the parent (winref msgwait)
                0x7 | 0x8 => {
                    let (id, parent) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.id, w.parent)));
                    if parent != 0 {
                        let code = if m == 7 { 0x100 } else { 0x200 };
                        send(c, parent, 0x111, code << 16 | (id & 0xffff), h);
                    }
                }
                0x201 => {
                    set_focus(c, h);
                }
                0x87 => return 0x89,
                _ => {
                    let changed = with(|s| {
                        let w = s.wins.get_mut(&h).unwrap();
                        match wp {
                            8 => w.text.pop().is_some(),
                            0..=0x1f => false,
                            ch => {
                                w.text.push(ch as u16);
                                true
                            }
                        }
                    });
                    if changed {
                        invalidate(h, None, true);
                        let (id, parent) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.id, w.parent)));
                        if parent != 0 {
                            send(c, parent, 0x111, 0x300 << 16 | (id & 0xffff), h);
                        }
                    }
                }
            }
            0
        }
        0x87 => 0x100,
        0xf0 => ctl(h).check,
        0xf1 => {
            set_ctl(h, |t| t.check = wp);
            invalidate(h, None, true);
            0
        }
        0xf5 => {
            ctl_click(c, h);
            0
        }
        0xc | 0xa => {
            let r = def_proc(c, msg_wide());
            invalidate(h, None, true);
            r
        }
        0x2 => {
            CTLS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
            def_proc(c, msg_wide())
        }
        _ => def_proc(c, msg_wide()),
    }
}

/// Classes every program finds registered.
fn system_classes(s: &mut State) {
    if !s.classes.is_empty() {
        return;
    }
    for n in ["Button", "Edit", "Static", "ListBox", "ComboBox", "ScrollBar", "#32770", "MDIClient", "ComboLBox", "#32768", "msctls_progress32", "SysListView32", "SysTreeView32", "msctls_statusbar32", "ToolbarWindow32", "tooltips_class32", "RichEdit20A", "RichEdit20W", "RICHEDIT50W", "IME", "SysTabControl32", "msctls_trackbar32", "msctls_updown32"] {
        let atom = crate::k32::atom(n);
        s.classes.push(Class {
            name: n.into(),
            style: 0,
            proc_: match n {
                "Button" | "Static" | "Edit" => control_proc(),
                "SysTabControl32" => tab_proc(),
                "ComboBox" => combo_proc(),
                // (the class's own: a CBT hook subclassing a new dialog keeps it as the super proc)
                "#32770" => dlg_wndproc(),
                _ => builtin_proc(),
            },
            cls_extra: 0,
            wnd_extra: 32,
            inst: 0,
            icon: 0,
            cursor: 0,
            // (COLOR_BTNFACE + 1)
            brush: if n == "SysTabControl32" { 16 } else { 0 },
            menu: 0,
            unicode: false,
            atom,
            extra: Vec::new(),
            unregistered: false,
        });
    }
}

/// Offsets of WNDCLASS(EX)'s style, lpfnWndProc, cbClsExtra, cbWndExtra, hInstance, hIcon,
/// hCursor, hbrBackground, lpszMenuName, lpszClassName (and hIconSm after them in EX).
fn wndclass_offsets(ex: bool) -> [u32; 11] {
    if mem::x64() {
        [if ex { 4 } else { 0 }, 8, 16, 20, 24, 32, 40, 48, 56, 64, 72]
    } else {
        let o = if ex { 4 } else { 0 };
        std::array::from_fn(|i| o + 4 * i as u32)
    }
}

fn register_class(c: &mut Ctx, p: u32, wide: bool, ex: bool) -> u32 {
    let f = wndclass_offsets(ex);
    let name_p = r32(p + f[9]);
    let name = if name_p < 0x10000 { format!("#{name_p}") } else { arg_str(name_p, wide) };
    let atom = crate::k32::atom(&name);
    let cls = Class {
        name: name.clone(),
        style: r32(p + f[0]),
        proc_: r32(p + f[1]),
        cls_extra: r32(p + f[2]),
        wnd_extra: r32(p + f[3]),
        inst: r32(p + f[4]),
        icon: r32(p + f[5]),
        cursor: r32(p + f[6]),
        brush: r32(p + f[7]),
        menu: r32(p + f[8]),
        unicode: wide,
        atom,
        extra: vec![0; r32(p + f[2]) as usize],
        unregistered: false,
    };
    let _ = c;
    with(|s| {
        system_classes(s);
        let low = name.to_lowercase();
        // (an application class of the same module again: ERROR_CLASS_ALREADY_EXISTS; the
        // system classes may be replaced)
        match s.classes.iter().position(|k| k.name.to_lowercase() == low) {
            Some(i) if s.classes[i].inst == cls.inst && s.classes[i].inst != 0 && !s.classes[i].unregistered => {
                w32(crate::cur().teb + crate::teb_last_error(), 1410);
                return 0;
            }
            Some(i) => s.classes[i] = cls,
            None => s.classes.push(cls),
        }
        atom
    })
}
fn RegisterClassA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    register_class(c, p, false, false)
}
fn RegisterClassW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    register_class(c, p, true, false)
}
fn RegisterClassExA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    register_class(c, p, false, true)
}
fn RegisterClassExW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    register_class(c, p, true, true)
}
/// UnregisterClass(name, instance): ERROR_CLASS_DOES_NOT_EXIST, ERROR_CLASS_HAS_WINDOWS
fn unregister_class(name: u32, wide: bool) -> u32 {
    let r = with(|s| {
        system_classes(s);
        let i = class_index(s, name, wide).filter(|&i| s.classes[i].inst != 0).ok_or(1411u32)?;
        if s.wins.values().any(|w| w.class == i) {
            return Err(1412);
        }
        s.classes[i].unregistered = true;
        Ok(())
    });
    match r {
        Ok(()) => 1,
        Err(e) => {
            w32(crate::cur().teb + crate::teb_last_error(), e);
            0
        }
    }
}
fn UnregisterClassA(c: &mut Ctx) -> u32 {
    unregister_class(c.a(0), false)
}
fn UnregisterClassW(c: &mut Ctx) -> u32 {
    unregister_class(c.a(0), true)
}
fn class_info(c: &mut Ctx, wide: bool, ex: bool) -> u32 {
    let (name, out) = (c.a(1), c.a(2));
    let r = with(|s| {
        system_classes(s);
        let i = class_index(s, name, wide)?;
        let k = &s.classes[i];
        Some((k.style, k.proc_, k.cls_extra, k.wnd_extra, k.inst, k.icon, k.cursor, k.brush, k.menu, k.atom))
    });
    let Some((style, proc_, ce, we, inst, icon, cursor, brush, menu, atom)) = r else {
        w32(crate::cur().teb + crate::teb_last_error(), 1411);
        return 0;
    };
    let f = wndclass_offsets(ex);
    if ex {
        w32(out, if mem::x64() { 80 } else { 48 });
    }
    for (k, v) in [style, proc_, ce, we, inst, icon, cursor, brush, menu, if name < 0x10000 { name } else { 0 }].iter().enumerate() {
        if k == 0 || k == 2 || k == 3 {
            w32(out + f[k], *v);
        } else {
            mem::wp(out + f[k], *v);
        }
    }
    if name >= 0x10000 {
        mem::wp(out + f[9], name);
    }
    if ex {
        mem::wp(out + f[10], 0);
    }
    atom
}
fn GetClassInfoA(c: &mut Ctx) -> u32 {
    class_info(c, false, false)
}
fn GetClassInfoW(c: &mut Ctx) -> u32 {
    class_info(c, true, false)
}
fn GetClassInfoExA(c: &mut Ctx) -> u32 {
    class_info(c, false, true)
}
fn GetClassInfoExW(c: &mut Ctx) -> u32 {
    class_info(c, true, true)
}

pub fn send(c: &mut Ctx, hwnd: u32, msg: u32, wp: u32, lp: u32) -> u32 {
    send_cpu(c.cpu, hwnd, msg, wp, lp)
}

pub fn send_cpu(cpu: &mut koji::Cpu, hwnd: u32, msg: u32, wp: u32, lp: u32) -> u32 {
    let info = with(|s| s.wins.get(&hwnd).map(|w| (w.proc_, w.tid)));
    let Some((proc_, tid)) = info else {
        return remote(hwnd).map_or(0, |w| to_remote(&w, msg, wp, lp, true));
    };
    let me = crate::cur().tid;
    if tid != me {
        return send_other_thread(cpu, tid, Msg { hwnd, msg, w: wp, l: lp, ..Default::default() });
    }
    if trace_msgs() {
        eprintln!("sake msg {hwnd:x} {msg:04x} {wp:x} {lp:x}");
    }
    crate::call(cpu, proc_, &[hwnd, msg, wp, lp])
}

fn trace_msgs() -> bool {
    static T: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *T.get_or_init(|| std::env::var_os("SAKE_TRACE_MSG").is_some())
}

fn send_other_thread(cpu: &mut koji::Cpu, tid: u32, msg: Msg) -> u32 {
    let result = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    with(|s| {
        let q = s.queues.entry(tid).or_default();
        q.sent.push_back(Sent { msg, result: result.clone(), from: crate::cur().tid, remote: None });
        q.fresh |= 0x40;
    });
    crate::obj::signal();
    // wait, handling messages sent to us meanwhile
    loop {
        {
            let mut g = result.0.lock().unwrap();
            if g.is_none() {
                g = result.1.wait_timeout(g, Duration::from_millis(5)).unwrap().0;
            }
            if let Some((v, at)) = *g {
                drop(g);
                if let Some(d) = at.checked_duration_since(Instant::now()) {
                    std::thread::sleep(d);
                }
                return v;
            }
        }
        process_sent(cpu);
        if !with(|s| s.wins.values().any(|w| w.tid == tid)) && !with(|s| s.queues.contains_key(&tid)) {
            return 0;
        }
    }
}

/// How long a thread whose SendMessage this thread answered runs on before the next waiting
/// thread is let go too. Windows answers them one after the other, the next only once this
/// thread has handled its message too, at native speed. Here the senders' own code is much
/// slower than that next window procedure (it runs for the first time: interpreted, then
/// translated), so threads that waited together for a busy owner went on in lockstep and hit
/// races native timing never shows. KiriKiri's sigcheck.dll (#82) is one: its two checking
/// threads each register SHA-256 without a lock right after a SetWindowPos to the main
/// thread's window, and the second one got slot 1, which it takes for an error.
const HEAD_START: Duration = Duration::from_millis(4);

/// When the sender `from` of a message answered `now` goes on, `last` being the sender let go
/// before it in the same turn (and when): another thread waits a head start after that one.
fn release_at(last: Option<(u32, Instant)>, from: u32, now: Instant) -> Instant {
    match last {
        Some((t, at)) if t != from => (at + HEAD_START).max(now),
        // (never ahead of the one before)
        Some((_, at)) => at.max(now),
        None => now,
    }
}

/// Runs messages other threads sent to this one.
fn process_sent(cpu: &mut koji::Cpu) {
    let me = crate::cur().tid;
    // the thread let go last here, and when it was
    let mut last: Option<(u32, Instant)> = None;
    loop {
        let s = with(|s| s.queues.get_mut(&me).and_then(|q| q.sent.pop_front()));
        let Some(sent) = s else { return };
        let m = sent.msg;
        let proc_ = with(|s| s.wins.get(&m.hwnd).map(|w| w.proc_));
        let r = match proc_ {
            Some(p) => crate::call(cpu, p, &[m.hwnd, m.msg, m.w, m.l]),
            None => 0,
        };
        let at = release_at(last, sent.from, Instant::now());
        last = Some((sent.from, at));
        *sent.result.0.lock().unwrap() = Some((r, at));
        sent.result.1.notify_all();
        if let Some((id, buf)) = sent.remote {
            if buf != 0 {
                mem::heap_free(buf);
            }
            crate::kern::with(|k| k.reply(id, r));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// windows of other processes (crate::kern has their top-level windows and the messages to them)

/// This process's first window handle (the processes number their windows apart).
fn first_hwnd() -> u32 {
    0x10020 + crate::pid().saturating_sub(crate::kern::ROOT_PID) / crate::kern::PID_STEP * 0x10_0000
}

/// Tells the other processes this one's top-level windows (once the kernel is shared).
pub fn publish() {
    if !crate::kern::host::shared() {
        return;
    }
    let me = crate::pid();
    let mut list: Vec<crate::kern::KWin> = with(|s| {
        s.wins
            .iter()
            .filter(|(_, w)| w.parent == 0)
            .map(|(&h, w)| crate::kern::KWin { hwnd: h, pid: me, tid: w.tid, class: s.classes[w.class].name.to_lowercase(), title: String::from_utf16_lossy(&w.text) })
            .collect()
    });
    list.sort_by_key(|w| w.hwnd);
    crate::kern::with(|k| k.set_wins(me, list));
}

/// Another process's window `hwnd`.
fn remote(hwnd: u32) -> Option<crate::kern::KWin> {
    if hwnd == 0 || !crate::kern::host::shared() {
        return None;
    }
    crate::kern::with(|k| k.win(hwnd).filter(|w| w.pid != crate::pid()).cloned()).0
}

/// PostMessage / SendMessage to another process's window: SendMessage waits for its result
/// (0 if that process ends first). WM_COPYDATA's COPYDATASTRUCT and data go along.
fn to_remote(w: &crate::kern::KWin, msg: u32, wp: u32, lp: u32, send: bool) -> u32 {
    let data = (msg == 0x4a && lp != 0).then(|| {
        let ps = mem::ps();
        let d = if mem::x64() { mem::r64(lp) } else { r32(lp) as u64 };
        (d, mem::slice(mem::rp(lp + 2 * ps), r32(lp + ps)).to_vec())
    });
    let to = w.pid;
    let id = crate::kern::with(|k| k.put_msg(crate::kern::XMsg { to, hwnd: w.hwnd, msg, w: wp, l: lp, data, send, ..Default::default() })).0;
    if !send {
        return 1;
    }
    crate::kern::block(None, &|| false, |k| match k.take_reply(id) {
        Err(crate::kern::PENDING) if k.exit_code(to).is_some() => Ok(0),
        r => r,
    })
    .unwrap_or(0)
}

/// The messages other processes posted or sent to this one's windows go to their threads'
/// queues (the watcher thread, crate::process::watch, after each change).
pub fn deliver_remote() {
    let msgs = crate::kern::with(|k| k.take_msgs(crate::pid())).0;
    for x in msgs {
        let Some(tid) = with(|s| s.wins.get(&x.hwnd).map(|w| w.tid)) else {
            if x.send {
                crate::kern::with(|k| k.reply(x.id, 0));
            }
            continue;
        };
        let (mut l, mut buf) = (x.l, 0);
        if let Some((d, bytes)) = &x.data {
            let ps = mem::ps();
            buf = mem::heap_alloc(3 * ps + bytes.len() as u32, true);
            if mem::x64() {
                mem::w64(buf, *d);
            } else {
                w32(buf, *d as u32);
            }
            w32(buf + ps, bytes.len() as u32);
            mem::wp(buf + 2 * ps, if bytes.is_empty() { 0 } else { buf + 3 * ps });
            mem::write(buf + 3 * ps, bytes);
            l = buf;
        }
        let m = Msg { hwnd: x.hwnd, msg: x.msg, w: x.w, l, time: now_ms(), ..Default::default() };
        if !x.send {
            post_thread(tid, m);
            continue;
        }
        let result = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
        with(|s| {
            let q = s.queues.entry(tid).or_default();
            q.sent.push_back(Sent { msg: m, result, from: 0, remote: Some((x.id, buf)) });
            q.fresh |= 0x40;
        });
        crate::obj::signal();
    }
}

fn create_window(c: &mut Ctx, wide: bool) -> u32 {
    let (ex, cls_p, name_p, mut style) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (mut x, mut y, mut cx, mut cy) = (c.i(4), c.i(5), c.i(6), c.i(7));
    let (parent, menu, inst, param) = (c.a(8), c.a(9), c.a(10), c.a(11));
    let tid = crate::cur().tid;
    let child = style & 0x4000_0000 != 0;
    let (sw, sh) = screen();
    // (as Wine's fix_cs_coordinates: popups and children take 0, overlapped windows a default)
    let def = |v: i32| v == 0x8000_0000u32 as i32 || v == 0x8000;
    let plain = child || style & 0x8000_0000 != 0;
    if def(x) {
        x = if plain { 0 } else { 40 };
        y = if plain { 0 } else { 40 };
    }
    if def(cx) {
        cx = if plain { 0 } else { sw * 3 / 4 };
        cy = if plain { 0 } else { sh * 3 / 4 };
    }
    // (negative sizes are empty windows, as on Windows)
    cx = cx.max(0);
    cy = cy.max(0);
    let mut ex = ex;
    if !child && style & 0x8000_0000 == 0 {
        // overlapped: a caption, WS_CLIPSIBLINGS and WS_EX_WINDOWEDGE
        style |= 0x04c0_0000;
        ex |= 0x100;
    }
    let made = with(|s| {
        system_classes(s);
        let ci = class_index(s, cls_p, wide)?;
        let k = &s.classes[ci];
        let h = s.next;
        s.next += 0x10;
        let has_menu = !child && menu != 0;
        let rect = Rect { l: x, t: y, r: x + cx, b: y + cy };
        let w = Window {
            class: ci,
            proc_: k.proc_,
            style: style & !0x1000_0000,
            exstyle: ex,
            parent: if child { parent } else { 0 },
            owner: if child { 0 } else { parent },
            id: if child { menu } else { 0 },
            inst,
            user: 0,
            rect,
            client: client_of(&rect, style, ex, has_menu),
            text: if name_p >= 0x10000 { if wide { wstr(name_p) } else { a2w(&cstr(name_p)) } } else { Vec::new() },
            visible: false,
            enabled: style & 0x0800_0000 == 0,
            tid,
            extra: vec![0; k.wnd_extra as usize],
            props: Vec::new(),
            update: Rect::default(),
            erase: false,
            menu: if child { 0 } else { menu },
            unicode: wide,
            surface: None,
            children: Vec::new(),
            shown: false,
            scroll: Default::default(),
        };
        s.wins.insert(h, w);
        if child {
            if let Some(p) = s.wins.get_mut(&parent) {
                p.children.push(h);
            }
        }
        resize_surface(s.wins.get_mut(&h).unwrap());
        Some((h, s.classes[ci].name.clone()))
    });
    let Some((h, cname)) = made else {
        eprintln!("sake: CreateWindowEx: no class {}", if cls_p < 0x10000 { format!("#{cls_p}") } else { arg_str(cls_p, wide) });
        w32(crate::cur().teb + crate::teb_last_error(), 1407);
        return 0;
    };
    if !child {
        publish();
    }
    if crate::env_on!("SAKE_TRACE_WIN") {
        eprintln!("sake: CreateWindowEx {h:x} class {cname} style {style:x} ex {ex:x} {x},{y} {cx}x{cy} parent {parent:x}");
    }
    // CREATESTRUCT on the guest stack (below the caller's)
    let save = c.cpu.regs[koji::ESP];
    let x64 = mem::x64();
    let cs = (save - if x64 { 0x100 } else { 0x80 }) & !15;
    c.cpu.regs[koji::ESP] = cs - 16;
    let fields = [param, inst, menu, parent, cy as u32, cx as u32, y as u32, x as u32, style, name_p, cls_p, ex];
    if x64 {
        // (lpCreateParams, hInstance, hMenu, hwndParent: 8 bytes; cy cx y x style; name and
        // class: 8 bytes; dwExStyle)
        for (k, o) in [0u32, 8, 16, 24, 32, 36, 40, 44, 48, 56, 64, 72].iter().enumerate() {
            if matches!(k, 0..=3 | 9 | 10) { mem::wp(cs + o, fields[k]) } else { w32(cs + o, fields[k]) }
        }
    } else {
        for (k, v) in fields.iter().enumerate() {
            w32(cs + 4 * k as u32, *v);
        }
    }
    // HCBT_CREATEWND (CBT_CREATEWND: CREATESTRUCT, insert after)
    let cbtw = cs + if x64 { 0xb0 } else { 0x70 };
    mem::wp(cbtw, cs);
    mem::wp(cbtw + mem::ps(), 0);
    if cbt(c, 3, h, cbtw) {
        c.cpu.regs[koji::ESP] = save;
        destroy(c, h);
        return 0;
    }
    // MINMAXINFO for WM_GETMINMAXINFO
    let mmi = cs + if x64 { 0x60 } else { 0x40 };
    if !child {
        let (sw, sh) = screen();
        for (k, v) in [0, 0, sw, sh, 0, 0, 112, 27, sw + 12, sh + 12].iter().enumerate() {
            w32(mmi + 4 * k as u32, *v as u32);
        }
        send(c, h, 0x24, 0, mmi);
    }
    let ok = send(c, h, 0x81, 0, cs);
    if ok == 0 {
        c.cpu.regs[koji::ESP] = save;
        destroy(c, h);
        return 0;
    }
    // WM_NCCALCSIZE (wParam FALSE: the window rectangle in, the client rectangle out)
    let rc = cs + if x64 { 0xd0 } else { 0x90 };
    let wr = with(|s| s.wins.get(&h).map(|w| w.rect)).unwrap_or_default();
    wr.write(rc);
    send(c, h, 0x83, 0, rc);
    if send(c, h, 0x1, 0, cs) == 0xffff_ffff {
        c.cpu.regs[koji::ESP] = save;
        destroy(c, h);
        return 0;
    }
    c.cpu.regs[koji::ESP] = save;
    if !is_window(h) {
        return 0;
    }
    // top-level windows get WM_SIZE / WM_MOVE when first shown (games keep the window in a
    // global only once CreateWindowEx returns), child windows now
    if child {
        let (cw, ch) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.client.w(), w.client.h())));
        send(c, h, 0x5, 0, (ch as u32) << 16 | (cw as u32 & 0xffff));
        send(c, h, 0x3, 0, (y as u32) << 16 | (x as u32 & 0xffff));
        with(|s| s.wins.get_mut(&h).map(|w| w.shown = true));
    }
    if child {
        send(c, parent, 0x210, 1 | (menu & 0xffff) << 16, h); // WM_PARENTNOTIFY
    }
    if style & 0x1000_0000 != 0 {
        show(c, h, 1);
    }
    h
}
fn CreateWindowExA(c: &mut Ctx) -> u32 {
    as_wide(false, || create_window(c, false))
}
fn CreateWindowExW(c: &mut Ctx) -> u32 {
    as_wide(true, || create_window(c, true))
}

fn destroy(c: &mut Ctx, h: u32) -> bool {
    if !is_window(h) || h == DESKTOP {
        return false;
    }
    let children = with(|s| s.wins.get(&h).map(|w| w.children.clone()).unwrap_or_default());
    for ch in children {
        destroy(c, ch);
    }
    send(c, h, 0x2, 0, 0);
    send(c, h, 0x82, 0, 0);
    with(|s| {
        if let Some(w) = s.wins.remove(&h) {
            if let Some(p) = s.wins.get_mut(&w.parent) {
                p.children.retain(|&x| x != h);
            }
            if let Some(q) = s.queues.get_mut(&w.tid) {
                q.timers.retain(|t| t.hwnd != h);
                q.posted.retain(|m| m.hwnd != h);
            }
            if w.surface.is_some() {
                kura::display::vnhost_window_destroy(h);
            }
        }
        if s.focus == h {
            s.focus = 0;
        }
        if s.active == h {
            s.active = 0;
        }
        if s.capture == h {
            s.capture = 0;
        }
    });
    crate::gdi::window_destroyed(h);
    true
}
fn DestroyWindow(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let was_active = with(|s| s.active == h);
    let ok = destroy(c, h);
    w32(crate::cur().teb + crate::teb_last_error(), if ok { 0 } else { 1400 });
    if ok {
        publish();
    }
    // the active window gone: another visible top-level window takes over, as on Windows
    if ok && was_active {
        let next = with(|s| s.wins.iter().filter(|(&k, w)| k != DESKTOP && w.parent == 0 && w.visible).map(|(&k, _)| k).max());
        if let Some(n) = next {
            activate(c, n);
        }
    }
    ok as u32
}

fn show(c: &mut Ctx, h: u32, cmd: u32) -> u32 {
    let was = with(|s| s.wins.get(&h).map(|w| w.visible));
    let Some(was) = was else { return 0 };
    // SW_(SHOW)MAXIMIZE: a top-level window's client area covers the screen (its frame
    // outside it)
    let top = with(|s| s.wins.get(&h).filter(|w| w.parent == 0 && w.style & 0x4000_0000 == 0).map(|w| (w.style, w.exstyle, w.menu != 0)));
    if let (3, Some((style, ex, menu))) = (cmd, top) {
        let (l, t, r, b) = frame(style, ex, menu);
        let (sw, sh) = screen();
        set_pos(c, h, 0, -l, -t, sw + l + r, sh + t + b, 0x14);
    }
    let vis = !matches!(cmd, 0);
    if vis != was {
        send(c, h, 0x18, vis as u32, 0);
        // SetWindowPos(SWP_SHOWWINDOW or SWP_HIDEWINDOW, nothing else), then for the first show
        // WM_SIZE and WM_MOVE
        let f = 0x17 | if vis { 0x40 | SWP_BY_SHOWWINDOW } else { 0x80 | SWP_BY_HIDEWINDOW };
        set_pos(c, h, 0, 0, 0, 0, 0, f);
        let first = with(|s| s.wins.get_mut(&h).is_some_and(|w| vis && !std::mem::replace(&mut w.shown, true)));
        if first {
            let Some((r, cl)) = with(|s| s.wins.get(&h).map(|w| (w.rect, w.client))) else { return 0 };
            send(c, h, 0x5, 0, (cl.h() as u32) << 16 | (cl.w() as u32 & 0xffff));
            let (x, y) = (r.l + cl.l, r.t + cl.t);
            send(c, h, 0x3, 0, (y as u32 & 0xffff) << 16 | (x as u32 & 0xffff));
        }
        let top = with(|s| s.wins.get(&h).is_some_and(|w| w.parent == 0));
        if vis && top && !matches!(cmd, 4 | 8) {
            activate(c, h);
        }
        crate::obj::signal();
    }
    was as u32
}
fn activate(c: &mut Ctx, h: u32) {
    let old = with(|s| std::mem::replace(&mut s.active, h));
    if old != h {
        if old != 0 {
            send(c, old, 0x6, 0, h);
            send(c, old, 0x86, 0, h);
        }
        // the app becomes active with its first active window (games run only while it is)
        if old == 0 || !is_window(old) {
            send(c, h, 0x1c, 1, 0);
        }
        send(c, h, 0x86, 1, old);
        send(c, h, 0x6, 1, old);
        let f = with(|s| s.focus);
        if f == 0 || !is_window(f) {
            set_focus(c, h);
        }
    }
}
fn set_focus(c: &mut Ctx, h: u32) -> u32 {
    let old = with(|s| std::mem::replace(&mut s.focus, h));
    if old != h {
        if old != 0 && is_window(old) {
            send(c, old, 0x8, h, 0);
        }
        if h != 0 {
            send(c, h, 0x7, old, 0);
        }
    }
    old
}
fn ShowWindow(c: &mut Ctx) -> u32 {
    let (h, cmd) = (c.a(0), c.a(1));
    show(c, h, cmd)
}
fn ShowWindowAsync(c: &mut Ctx) -> u32 {
    ShowWindow(c)
}
fn ShowOwnedPopups(_: &mut Ctx) -> u32 {
    1
}
fn UpdateWindow(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let dirty = with(|s| s.wins.get(&h).is_some_and(|w| !w.update.empty()));
    if dirty {
        send(c, h, 0xf, 0, 0);
    }
    1
}
fn IsWindow(c: &mut Ctx) -> u32 {
    (is_window(c.a(0)) || remote(c.a(0)).is_some()) as u32
}
fn IsWindowVisible(c: &mut Ctx) -> u32 {
    with(|s| {
        let mut h = c.a(0);
        loop {
            match s.wins.get(&h) {
                Some(w) if w.visible => {
                    if w.parent == 0 || w.parent == DESKTOP {
                        return 1;
                    }
                    h = w.parent;
                }
                _ => return (h == DESKTOP) as u32,
            }
        }
    })
}
fn IsWindowEnabled(c: &mut Ctx) -> u32 {
    with(|s| s.wins.get(&c.a(0)).is_some_and(|w| w.enabled) as u32)
}
fn EnableWindow(c: &mut Ctx) -> u32 {
    let (h, en) = (c.a(0), c.a(1) != 0);
    let was = with(|s| {
        let w = s.wins.get_mut(&h)?;
        let was = w.enabled;
        w.enabled = en;
        if en {
            w.style &= !0x0800_0000;
        } else {
            w.style |= 0x0800_0000;
        }
        Some(was)
    });
    if was.is_some_and(|w| w != en) {
        send(c, h, 0xa, en as u32, 0);
    }
    (!was.unwrap_or(true)) as u32
}
fn IsIconic(c: &mut Ctx) -> u32 {
    with(|s| s.wins.get(&c.a(0)).is_some_and(|w| w.style & 0x2000_0000 != 0) as u32)
}
fn IsZoomed(c: &mut Ctx) -> u32 {
    with(|s| s.wins.get(&c.a(0)).is_some_and(|w| w.style & 0x0100_0000 != 0) as u32)
}
fn IsChild(c: &mut Ctx) -> u32 {
    let (p, mut h) = (c.a(0), c.a(1));
    with(|s| loop {
        match s.wins.get(&h) {
            Some(w) if w.style & 0x4000_0000 != 0 => {
                if w.parent == p {
                    return 1;
                }
                h = w.parent;
            }
            _ => return 0,
        }
    })
}

/// WINDOWPOS at `p`: hwnd, hwndInsertAfter (pointer sized), x, y, cx, cy, flags
fn windowpos_write(p: u32, h: u32, after: u32, x: i32, y: i32, cx: i32, cy: i32, flags: u32) {
    let ps = mem::ps();
    mem::wp(p, h);
    mem::wp(p + ps, after);
    for (k, v) in [x as u32, y as u32, cx as u32, cy as u32, flags].iter().enumerate() {
        w32(p + 2 * ps + 4 * k as u32, *v);
    }
}
fn windowpos_read(p: u32) -> (i32, i32, i32, i32, u32) {
    let o = p + 2 * mem::ps();
    (r32(o) as i32, r32(o + 4) as i32, r32(o + 8) as i32, r32(o + 12) as i32, r32(o + 16))
}
// SWP_ flags Windows keeps for itself in WM_WINDOWPOSCHANGED: the client area did not move /
// change size (DefWindowProc sends WM_MOVE / WM_SIZE otherwise), shown or hidden by ShowWindow
const SWP_NOCLIENTSIZE: u32 = 0x800;
const SWP_NOCLIENTMOVE: u32 = 0x1000;
const SWP_BY_SHOWWINDOW: u32 = 0x1000_0000;
const SWP_BY_HIDEWINDOW: u32 = 0x2000_0000;

/// SetWindowPos: WM_WINDOWPOSCHANGING (which may change the request), WM_NCCALCSIZE when the
/// size changes, WM_WINDOWPOSCHANGED unless nothing changed (DefWindowProc turns it into
/// WM_MOVE and WM_SIZE), as Windows sends them.
fn set_pos(c: &mut Ctx, h: u32, after: u32, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> u32 {
    if !is_window(h) {
        w32(crate::cur().teb + crate::teb_last_error(), 1400);
        return 0;
    }
    let save = c.cpu.regs[koji::ESP];
    let wp = (save - 0x80) & !15;
    c.cpu.regs[koji::ESP] = wp - 16;
    let internal = flags & (SWP_BY_SHOWWINDOW | SWP_BY_HIDEWINDOW);
    windowpos_write(wp, h, after, x, y, cx, cy, flags & !internal);
    send(c, h, 0x46, 0, wp);
    let (x, y, cx, cy, flags) = windowpos_read(wp);
    let flags = flags | internal;
    let r = with(|s| {
        let w = s.wins.get_mut(&h)?;
        let old = w.rect;
        let mut r = w.rect;
        if flags & 2 == 0 {
            r = Rect { l: x, t: y, r: x + r.w(), b: y + r.h() };
        }
        if flags & 1 == 0 {
            r.r = r.l + cx.max(0);
            r.b = r.t + cy.max(0);
        }
        w.rect = r;
        let has_menu = w.menu != 0 && w.style & 0x4000_0000 == 0;
        w.client = client_of(&r, w.style, w.exstyle, has_menu);
        let vis = w.visible;
        if flags & 0x40 != 0 {
            w.visible = true;
        }
        if flags & 0x80 != 0 {
            w.visible = false;
        }
        let resized = old.w() != r.w() || old.h() != r.h();
        if resized || (w.visible && !vis) {
            w.update = Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() };
            w.erase = true;
        }
        resize_surface(w);
        let moved = old.l != r.l || old.t != r.t;
        let shown = vis != w.visible;
        let hidden_all_along = !vis && !w.visible;
        let (parent, now_visible) = (w.parent, w.visible);
        if now_visible && !vis {
            invalidate_tree(s, h);
        }
        // a child hidden: its parent paints where it was (the rectangles are parent-relative)
        if vis && !now_visible && parent != 0 && parent != DESKTOP {
            if let Some(pw) = s.wins.get_mut(&parent) {
                pw.update = pw.update.or(&old);
                pw.erase = true;
            }
        }
        update_display(s, h);
        Some((resized, moved, shown, hidden_all_along, r))
    });
    let Some((resized, moved, shown, hidden, r)) = r else {
        c.cpu.regs[koji::ESP] = save;
        return 0;
    };
    if resized {
        crate::gdi::window_resized(h);
    }
    let changed = resized || moved || shown || flags & 0x24 != 0x4;
    if changed {
        if resized || flags & 0x20 != 0 {
            let rc = wp + 0x40;
            r.write(rc);
            send(c, h, 0x83, 0, rc);
        }
        let mut f = flags;
        if !resized {
            f |= SWP_NOCLIENTSIZE;
        }
        if !moved {
            f |= SWP_NOCLIENTMOVE;
        }
        if hidden {
            f |= 8; // (SWP_NOREDRAW: nothing to draw)
        }
        windowpos_write(wp, h, after, r.l, r.t, r.w(), r.h(), f);
        send(c, h, 0x47, 0, wp);
    }
    c.cpu.regs[koji::ESP] = save;
    if flags & 0x40 != 0 && internal == 0 {
        send(c, h, 0x18, 1, 0);
    }
    // without SWP_NOACTIVATE a shown top-level window becomes the active one (NonColor shows
    // its main window this way after the startup dialog and only takes mouse input while active)
    let top_vis = with(|s| s.wins.get(&h).is_some_and(|w| w.parent == 0 && w.visible));
    if flags & 0x10 == 0 && top_vis && (flags & 0x40 != 0 || with(|s| s.active == 0)) {
        activate(c, h);
    }
    crate::obj::signal();
    1
}
/// WM_MOVE (the client area's position) and WM_SIZE, as DefWindowProc sends them for
/// WM_WINDOWPOSCHANGED
fn send_move_size(c: &mut Ctx, h: u32, mv: bool, size: bool) {
    let Some((r, cl, child)) = with(|s| s.wins.get(&h).map(|w| (w.rect, w.client, w.parent != 0))) else { return };
    let _ = child;
    if mv {
        let (x, y) = (r.l + cl.l, r.t + cl.t);
        send(c, h, 0x3, 0, (y as u32 & 0xffff) << 16 | (x as u32 & 0xffff));
    }
    if size {
        send(c, h, 0x5, 0, (cl.h() as u32) << 16 | (cl.w() as u32 & 0xffff));
    }
}
fn SetWindowPos(c: &mut Ctx) -> u32 {
    let (h, after, x, y, cx, cy, f) = (c.a(0), c.a(1), c.i(2), c.i(3), c.i(4), c.i(5), c.a(6));
    set_pos(c, h, after, x, y, cx, cy, f)
}
fn MoveWindow(c: &mut Ctx) -> u32 {
    let (h, x, y, cx, cy) = (c.a(0), c.i(1), c.i(2), c.i(3), c.i(4));
    set_pos(c, h, 0, x, y, cx, cy, 4)
}
fn BeginDeferWindowPos(_: &mut Ctx) -> u32 {
    crate::mem::heap_alloc(4, true)
}
fn DeferWindowPos(c: &mut Ctx) -> u32 {
    let (hdwp, h, after, x, y, cx, cy, f) = (c.a(0), c.a(1), c.a(2), c.i(3), c.i(4), c.i(5), c.i(6), c.a(7));
    set_pos(c, h, after, x, y, cx, cy, f);
    hdwp
}
fn EndDeferWindowPos(_: &mut Ctx) -> u32 {
    1
}
fn GetWindowRect(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let r = with(|s| {
        if h == DESKTOP {
            return Some(Rect { l: 0, t: 0, r: s.screen.0, b: s.screen.1 });
        }
        let w = s.wins.get(&h)?;
        let (x, y) = window_origin(s, h);
        Some(Rect { l: x, t: y, r: x + w.rect.w(), b: y + w.rect.h() })
    });
    match r {
        Some(r) => {
            r.write(c.a(1));
            1
        }
        None => 0,
    }
}
fn GetClientRect(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let r = with(|s| {
        if h == DESKTOP || h == 0 {
            return Some(Rect { l: 0, t: 0, r: s.screen.0, b: s.screen.1 });
        }
        s.wins.get(&h).map(|w| Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() })
    });
    match r {
        Some(r) => {
            r.write(c.a(1));
            1
        }
        None => 0,
    }
}
fn GetWindowPlacement(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    let r = with(|s| s.wins.get(&h).map(|w| (w.rect, w.visible)));
    let Some((r, vis)) = r else { return 0 };
    w32(p, 44);
    w32(p + 4, 0);
    w32(p + 8, if vis { 1 } else { 0 });
    for o in [12, 16, 20, 24] {
        w32(p + o, 0xffff_ffff);
    }
    r.write(p + 28);
    1
}
fn SetWindowPlacement(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    let r = Rect::read(p + 28);
    set_pos(c, h, 0, r.l, r.t, r.w(), r.h(), 4);
    1
}
fn ClientToScreen(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    let o = with(|s| client_screen(s, h));
    w32(p, (r32(p) as i32 + o.l) as u32);
    w32(p + 4, (r32(p + 4) as i32 + o.t) as u32);
    1
}
fn ScreenToClient(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    let o = with(|s| client_screen(s, h));
    w32(p, (r32(p) as i32 - o.l) as u32);
    w32(p + 4, (r32(p + 4) as i32 - o.t) as u32);
    1
}
fn MapWindowPoints(c: &mut Ctx) -> u32 {
    let (from, to, p, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (a, b) = with(|s| (client_screen(s, from), client_screen(s, to)));
    let (dx, dy) = (a.l - b.l, a.t - b.t);
    for i in 0..n {
        w32(p + 8 * i, (r32(p + 8 * i) as i32 + dx) as u32);
        w32(p + 8 * i + 4, (r32(p + 8 * i + 4) as i32 + dy) as u32);
    }
    ((dy as u32) << 16) | (dx as u32 & 0xffff)
}
fn AdjustWindowRect(c: &mut Ctx) -> u32 {
    let (p, style, menu) = (c.a(0), c.a(1), c.a(2) != 0);
    adjust(p, style, 0, menu)
}
fn AdjustWindowRectEx(c: &mut Ctx) -> u32 {
    let (p, style, menu, ex) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    adjust(p, style, ex, menu)
}
fn adjust(p: u32, style: u32, ex: u32, menu: bool) -> u32 {
    let (l, t, r, b) = frame(style, ex, menu);
    let mut x = Rect::read(p);
    x.l -= l;
    x.t -= t;
    x.r += r;
    x.b += b;
    x.write(p);
    1
}
fn GetParent(c: &mut Ctx) -> u32 {
    with(|s| s.wins.get(&c.a(0)).map_or(0, |w| if w.style & 0x4000_0000 != 0 { w.parent } else { w.owner }))
}
fn GetAncestor(c: &mut Ctx) -> u32 {
    let (mut h, flags) = (c.a(0), c.a(1));
    with(|s| {
        if flags == 1 {
            return s.wins.get(&h).map_or(0, |w| if w.parent == 0 { DESKTOP } else { w.parent });
        }
        loop {
            match s.wins.get(&h) {
                Some(w) if w.parent != 0 && w.parent != DESKTOP => h = w.parent,
                Some(_) => return h,
                None => return 0,
            }
        }
    })
}
fn SetParent(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    with(|s| {
        let Some(w) = s.wins.get_mut(&h) else { return 0 };
        let old = w.parent;
        w.parent = if p == DESKTOP { 0 } else { p };
        if let Some(o) = s.wins.get_mut(&old) {
            o.children.retain(|&x| x != h);
        }
        if let Some(n) = s.wins.get_mut(&p) {
            n.children.push(h);
        }
        if p == 0 || p == DESKTOP {
            resize_surface(s.wins.get_mut(&h).unwrap());
            update_display(s, h);
        }
        if old == 0 { DESKTOP } else { old }
    })
}
fn GetDesktopWindow(_: &mut Ctx) -> u32 {
    DESKTOP
}
fn GetShellWindow(_: &mut Ctx) -> u32 {
    DESKTOP
}
fn GetWindow(c: &mut Ctx) -> u32 {
    let (h, cmd) = (c.a(0), c.a(1));
    with(|s| {
        let tops: Vec<u32> = {
            let mut v: Vec<u32> = s.wins.iter().filter(|(_, w)| w.parent == 0).map(|(&h, _)| h).collect();
            v.sort();
            v.reverse();
            v
        };
        let siblings = |h: u32| -> Vec<u32> {
            match s.wins.get(&h) {
                Some(w) if w.parent != 0 => s.wins.get(&w.parent).map(|p| p.children.clone()).unwrap_or_default(),
                _ => tops.clone(),
            }
        };
        match cmd {
            0 => siblings(h).first().copied().unwrap_or(0),
            1 => siblings(h).last().copied().unwrap_or(0),
            2 | 3 => {
                let v = siblings(h);
                let Some(i) = v.iter().position(|&x| x == h) else { return 0 };
                let j = if cmd == 2 { i + 1 } else { i.wrapping_sub(1) };
                v.get(j).copied().unwrap_or(0)
            }
            4 => s.wins.get(&h).map_or(0, |w| w.owner),
            5 => {
                if h == DESKTOP {
                    tops.first().copied().unwrap_or(0)
                } else {
                    s.wins.get(&h).and_then(|w| w.children.first().copied()).unwrap_or(0)
                }
            }
            _ => 0,
        }
    })
}
fn GetTopWindow(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    with(|s| {
        if h == 0 || h == DESKTOP {
            s.wins.iter().filter(|(_, w)| w.parent == 0 && w.visible).map(|(&h, _)| h).max().unwrap_or(0)
        } else {
            s.wins.get(&h).and_then(|w| w.children.first().copied()).unwrap_or(0)
        }
    })
}
fn GetLastActivePopup(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn GetDlgItem(c: &mut Ctx) -> u32 {
    let (h, id) = (c.a(0), c.a(1));
    with(|s| s.wins.get(&h).and_then(|w| w.children.iter().copied().find(|ch| s.wins.get(ch).is_some_and(|x| x.id == id))).unwrap_or(0))
}
fn GetDlgCtrlID(c: &mut Ctx) -> u32 {
    with(|s| s.wins.get(&c.a(0)).map_or(0, |w| w.id))
}
fn SendDlgItemMessageA(c: &mut Ctx) -> u32 {
    let (h, id, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let ch = with(|s| s.wins.get(&h).and_then(|w| w.children.iter().copied().find(|ch| s.wins.get(ch).is_some_and(|x| x.id == id))));
    ch.map_or(0, |ch| send(c, ch, m, wp, lp))
}
fn GetWindowThreadProcessId(c: &mut Ctx) -> u32 {
    if let Some(tid) = with(|s| s.wins.get(&c.a(0)).map(|w| w.tid)) {
        put32(c.a(1), crate::pid());
        return tid;
    }
    match remote(c.a(0)) {
        Some(w) => {
            put32(c.a(1), w.pid);
            w.tid
        }
        None => 0,
    }
}
fn enum_windows(c: &mut Ctx, list: Vec<u32>, f: u32, lp: u32) -> u32 {
    for h in list {
        if is_window(h) && c.call(f, &[h, lp]) == 0 {
            break;
        }
    }
    1
}
fn EnumWindows(c: &mut Ctx) -> u32 {
    let mut v: Vec<u32> = with(|s| s.wins.iter().filter(|(_, w)| w.parent == 0).map(|(&h, _)| h).collect());
    v.sort();
    let (f, lp) = (c.a(0), c.a(1));
    enum_windows(c, v, f, lp)
}
fn EnumThreadWindows(c: &mut Ctx) -> u32 {
    let tid = c.a(0);
    let mut v: Vec<u32> = with(|s| s.wins.iter().filter(|(_, w)| w.parent == 0 && w.tid == tid).map(|(&h, _)| h).collect());
    v.sort();
    let (f, lp) = (c.a(1), c.a(2));
    enum_windows(c, v, f, lp)
}
fn EnumChildWindows(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let v = with(|s| s.wins.get(&p).map(|w| w.children.clone()).unwrap_or_default());
    let (f, lp) = (c.a(1), c.a(2));
    enum_windows(c, v, f, lp)
}
/// FindWindow(Ex): the first window under `parent` (0 or the desktop: top-level windows, the
/// newest first; HWND_MESSAGE: message-only windows; a window: its children in their order)
/// after `after` whose class and title match (the class without case, the title exactly)
fn find_window(parent: u32, after: u32, cls: u32, title: u32, wide: bool) -> u32 {
    let cls_name = (cls != 0).then(|| if cls < 0x10000 { crate::k32::atom_name(cls).unwrap_or_default() } else { arg_str(cls, wide) }.to_lowercase());
    let t = (title != 0).then(|| arg_str(title, wide));
    const MESSAGE: u32 = 0xffff_fffd;
    let top = parent == 0 || parent == DESKTOP || parent == MESSAGE;
    let found = with(|s| {
        let list: Vec<u32> = if top {
            let msg_only = parent == MESSAGE;
            let mut v: Vec<u32> = s.wins.iter().filter(|(_, w)| w.parent == 0 && (w.owner == MESSAGE) == msg_only).map(|(&h, _)| h).collect();
            v.sort_unstable_by(|a, b| b.cmp(a));
            v
        } else {
            match s.wins.get(&parent) {
                Some(p) => p.children.clone(),
                None => return Err(()),
            }
        };
        let from = if after == 0 { 0 } else { list.iter().position(|&h| h == after).map_or(list.len(), |i| i + 1) };
        Ok(list[from..]
            .iter()
            .copied()
            .find(|h| {
                let w = &s.wins[h];
                cls_name.as_ref().is_none_or(|n| s.classes[w.class].name.to_lowercase() == *n) && t.as_ref().is_none_or(|t| String::from_utf16_lossy(&w.text) == *t)
            }))
    });
    let found = match found {
        Ok(f) => f,
        Err(()) => {
            crate::k32::last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
            return 0;
        }
    };
    found
        .or_else(|| {
            if parent != 0 && parent != DESKTOP || after != 0 || !crate::kern::host::shared() {
                return None;
            }
            crate::kern::with(|k| k.find_win(crate::pid(), cls_name.as_deref(), t.as_deref()).map(|w| w.hwnd)).0
        })
        .unwrap_or(0)
}
fn FindWindowA(c: &mut Ctx) -> u32 {
    find_window(0, 0, c.a(0), c.a(1), false)
}
fn FindWindowW(c: &mut Ctx) -> u32 {
    find_window(0, 0, c.a(0), c.a(1), true)
}
fn FindWindowExA(c: &mut Ctx) -> u32 {
    find_window(c.a(0), c.a(1), c.a(2), c.a(3), false)
}
fn FindWindowExW(c: &mut Ctx) -> u32 {
    find_window(c.a(0), c.a(1), c.a(2), c.a(3), true)
}
fn window_at(s: &State, x: i32, y: i32) -> u32 {
    let mut tops: Vec<u32> = s.wins.iter().filter(|(_, w)| w.parent == 0 && w.visible).map(|(&h, _)| h).collect();
    // the most recently activated first (with the windows it owns), owned windows above their
    // owner, then newest
    let owners = |h: u32| std::iter::successors(Some(h), |o| s.wins.get(o).map(|w| w.owner).filter(|&o| o != 0)).take(8);
    tops.sort_by_key(|&h| (owners(h).any(|o| o == s.active), owners(h).count(), h));
    let Some(&top) = tops.iter().rev().find(|&&h| {
        let (ox, oy) = window_origin(s, h);
        let w = &s.wins[&h];
        Rect { l: ox, t: oy, r: ox + w.rect.w(), b: oy + w.rect.h() }.has(x, y)
    }) else {
        return DESKTOP;
    };
    // the deepest visible child
    let mut h = top;
    'down: loop {
        let w = &s.wins[&h];
        for &ch in w.children.iter().rev() {
            let Some(cw) = s.wins.get(&ch) else { continue };
            if !cw.visible || cw.style & 0x0800_0000 != 0 {
                continue;
            }
            let (ox, oy) = window_origin(s, ch);
            if (Rect { l: ox, t: oy, r: ox + cw.rect.w(), b: oy + cw.rect.h() }).has(x, y) {
                h = ch;
                continue 'down;
            }
        }
        return h;
    }
}
/// The window under screen point (x, y) holding property `key` (lowercase), or its nearest
/// ancestor that does: (window, value). For OLE drag and drop ("oledroptargetinterface").
pub(crate) fn prop_at(x: i32, y: i32, key: &str) -> Option<(u32, u32)> {
    with(|s| {
        let mut h = window_at(s, x, y);
        for _ in 0..64 {
            let w = s.wins.get(&h)?;
            if let Some(&(_, v)) = w.props.iter().find(|(k, _)| k == key) {
                return Some((h, v));
            }
            h = w.parent;
        }
        None
    })
}
/// A window's property `key` (lowercase): set (Some) or removed (None); the old value
pub(crate) fn set_prop(h: u32, key: &str, v: Option<u32>) -> Option<u32> {
    with(|s| {
        let w = s.wins.get_mut(&h)?;
        let old = w.props.iter().find(|(k, _)| k == key).map(|p| p.1);
        w.props.retain(|(k, _)| k != key);
        if let Some(v) = v {
            w.props.push((key.to_string(), v));
        }
        old
    })
}
pub(crate) fn prop(h: u32, key: &str) -> Option<u32> {
    with(|s| s.wins.get(&h)?.props.iter().find(|(k, _)| k == key).map(|p| p.1))
}
pub(crate) fn window_from_point(x: i32, y: i32) -> u32 {
    with(|s| window_at(s, x, y))
}
pub(crate) fn capture(h: u32) -> u32 {
    with(|s| std::mem::replace(&mut s.capture, h))
}
fn WindowFromPoint(c: &mut Ctx) -> u32 {
    let (x, y) = c.pt(0);
    with(|s| window_at(s, x, y))
}
fn ChildWindowFromPoint(c: &mut Ctx) -> u32 {
    let (h, (x, y)) = (c.a(0), c.pt(1));
    with(|s| {
        let o = client_screen(s, h);
        let (sx, sy) = (x + o.l, y + o.t);
        let Some(w) = s.wins.get(&h) else { return 0 };
        for &ch in w.children.iter().rev() {
            if let Some(cw) = s.wins.get(&ch) {
                let (ox, oy) = window_origin(s, ch);
                if cw.visible && (Rect { l: ox, t: oy, r: ox + cw.rect.w(), b: oy + cw.rect.h() }).has(sx, sy) {
                    return ch;
                }
            }
        }
        h
    })
}

// window longs
fn get_long(h: u32, i: i32, wide: bool) -> u32 {
    let _ = wide;
    with(|s| {
        let Some(w) = s.wins.get(&h) else { return 0 };
        match i {
            -4 => w.proc_,
            -6 => w.inst,
            -8 => w.parent,
            -12 => w.id,
            -16 => w.style | if w.visible { 0x1000_0000 } else { 0 },
            -20 => w.exstyle,
            -21 => w.user,
            i if i >= 0 && (i as usize + 4) <= w.extra.len() => {
                let o = i as usize;
                u32::from_le_bytes([w.extra[o], w.extra[o + 1], w.extra[o + 2], w.extra[o + 3]])
            }
            _ => 0,
        }
    })
}
fn set_long(h: u32, i: i32, v: u32) -> u32 {
    let changed = with(|s| {
        let w = s.wins.get_mut(&h)?;
        let old = match i {
            -4 => std::mem::replace(&mut w.proc_, v),
            -6 => std::mem::replace(&mut w.inst, v),
            -8 => std::mem::replace(&mut w.owner, v),
            -12 => std::mem::replace(&mut w.id, v),
            -16 => {
                let old = w.style;
                w.style = v & !0x1000_0000;
                w.visible = v & 0x1000_0000 != 0;
                let has_menu = w.menu != 0 && w.style & 0x4000_0000 == 0;
                w.client = client_of(&w.rect, w.style, w.exstyle, has_menu);
                resize_surface(w);
                update_display(s, h);
                old
            }
            -20 => {
                let old = w.exstyle;
                w.exstyle = v;
                let has_menu = w.menu != 0 && w.style & 0x4000_0000 == 0;
                w.client = client_of(&w.rect, w.style, w.exstyle, has_menu);
                resize_surface(w);
                update_display(s, h);
                old
            }
            -21 => std::mem::replace(&mut w.user, v),
            i if i >= 0 && (i as usize + 4) <= w.extra.len() => {
                let o = i as usize;
                let old = u32::from_le_bytes([w.extra[o], w.extra[o + 1], w.extra[o + 2], w.extra[o + 3]]);
                w.extra[o..o + 4].copy_from_slice(&v.to_le_bytes());
                old
            }
            _ => 0,
        };
        Some(old)
    });
    changed.unwrap_or(0)
}
fn GetWindowLongA(c: &mut Ctx) -> u32 {
    get_long(c.a(0), c.i(1), false)
}
fn GetWindowLongW(c: &mut Ctx) -> u32 {
    get_long(c.a(0), c.i(1), true)
}
/// SetWindowLong: GWL_STYLE / GWL_EXSTYLE go through WM_STYLECHANGING (which may change the new
/// style) and WM_STYLECHANGED; a top-level window keeps WS_CLIPSIBLINGS; a bad index is
/// ERROR_INVALID_INDEX. A style change clears the last error (other fields leave it).
fn set_long_msg(c: &mut Ctx, h: u32, i: i32, v: u32) -> u32 {
    let err = |e: u32| w32(crate::cur().teb + crate::teb_last_error(), e);
    let Some((extra, top)) = with(|s| s.wins.get(&h).map(|w| (w.extra.len(), w.parent == 0 && w.style & 0x4000_0000 == 0))) else {
        err(1400);
        return 0;
    };
    let named = matches!(i, -4 | -6 | -8 | -12 | -16 | -20 | -21);
    if !named && !(i >= 0 && i as usize + 4 <= extra) {
        err(1413);
        return 0;
    }
    if !matches!(i, -16 | -20) {
        return set_long(h, i, v);
    }
    err(0);
    let old = get_long(h, i, false);
    let save = c.cpu.regs[koji::ESP];
    let ss = (save - 0x40) & !15;
    c.cpu.regs[koji::ESP] = ss - 16;
    w32(ss, old);
    w32(ss + 4, v);
    send(c, h, 0x7c, i as u32, ss);
    let mut new = r32(ss + 4);
    if i == -16 && top {
        new |= 0x0400_0000;
    }
    set_long(h, i, new);
    w32(ss, old);
    w32(ss + 4, new);
    send(c, h, 0x7d, i as u32, ss);
    c.cpu.regs[koji::ESP] = save;
    old
}
fn SetWindowLongA(c: &mut Ctx) -> u32 {
    let (h, i, v) = (c.a(0), c.i(1), c.a(2));
    set_long_msg(c, h, i, v)
}
fn SetWindowLongW(c: &mut Ctx) -> u32 {
    let (h, i, v) = (c.a(0), c.i(1), c.a(2));
    set_long_msg(c, h, i, v)
}
fn GetClassLongA(c: &mut Ctx) -> u32 {
    let (h, i) = (c.a(0), c.i(1));
    with(|s| {
        let Some(w) = s.wins.get(&h) else { return 0 };
        let k = &s.classes[w.class];
        match i {
            -10 => k.brush,
            -12 => k.cursor,
            -14 => k.icon,
            -16 => k.inst,
            -18 => k.wnd_extra,
            -20 => k.cls_extra,
            -24 => k.proc_,
            -26 => k.style,
            -32 => k.atom,
            _ => 0,
        }
    })
}
fn GetClassLongW(c: &mut Ctx) -> u32 {
    GetClassLongA(c)
}
fn SetClassLongA(c: &mut Ctx) -> u32 {
    let (h, i, v) = (c.a(0), c.i(1), c.a(2));
    with(|s| {
        let Some(w) = s.wins.get(&h) else { return 0 };
        let k = &mut s.classes[w.class];
        match i {
            -10 => std::mem::replace(&mut k.brush, v),
            -12 => std::mem::replace(&mut k.cursor, v),
            -14 => std::mem::replace(&mut k.icon, v),
            -24 => std::mem::replace(&mut k.proc_, v),
            -26 => std::mem::replace(&mut k.style, v),
            _ => 0,
        }
    })
}
fn SetClassLongW(c: &mut Ctx) -> u32 {
    SetClassLongA(c)
}
// (64-bit programs' names for them)
fn GetWindowLongPtrA(c: &mut Ctx) -> u32 {
    GetWindowLongA(c)
}
fn GetWindowLongPtrW(c: &mut Ctx) -> u32 {
    GetWindowLongW(c)
}
fn SetWindowLongPtrA(c: &mut Ctx) -> u32 {
    SetWindowLongA(c)
}
fn SetWindowLongPtrW(c: &mut Ctx) -> u32 {
    SetWindowLongW(c)
}
fn GetClassLongPtrA(c: &mut Ctx) -> u32 {
    GetClassLongA(c)
}
fn GetClassLongPtrW(c: &mut Ctx) -> u32 {
    GetClassLongW(c)
}
fn SetClassLongPtrA(c: &mut Ctx) -> u32 {
    SetClassLongA(c)
}
fn SetClassLongPtrW(c: &mut Ctx) -> u32 {
    SetClassLongW(c)
}
fn GetClassNameA(c: &mut Ctx) -> u32 {
    let n = with(|s| s.wins.get(&c.a(0)).map(|w| s.classes[w.class].name.clone())).unwrap_or_default();
    let r = ret_str(c.a(1), c.a(2), &n, false);
    r.min(c.a(2).saturating_sub(1))
}
fn GetClassNameW(c: &mut Ctx) -> u32 {
    let n = with(|s| s.wins.get(&c.a(0)).map(|w| s.classes[w.class].name.clone())).unwrap_or_default();
    let r = ret_str(c.a(1), c.a(2), &n, true);
    r.min(c.a(2).saturating_sub(1))
}
fn SetPropA(c: &mut Ctx) -> u32 {
    let (h, k, v) = (c.a(0), c.a(1), c.a(2));
    let key = prop_key(k, false);
    with(|s| {
        let Some(w) = s.wins.get_mut(&h) else { return 0 };
        w.props.retain(|(x, _)| *x != key);
        w.props.push((key, v));
        1
    })
}
fn SetPropW(c: &mut Ctx) -> u32 {
    let (h, k, v) = (c.a(0), c.a(1), c.a(2));
    let key = prop_key(k, true);
    with(|s| {
        let Some(w) = s.wins.get_mut(&h) else { return 0 };
        w.props.retain(|(x, _)| *x != key);
        w.props.push((key, v));
        1
    })
}
fn prop_key(k: u32, wide: bool) -> String {
    if k < 0x10000 {
        format!("#{k}")
    } else {
        // (property names are atoms: case-insensitive)
        arg_str(k, wide).to_lowercase()
    }
}
/// GetProp: a name that is no property is ERROR_FILE_NOT_FOUND (its atom is not there)
fn get_prop(h: u32, key: &str) -> u32 {
    let v = with(|s| s.wins.get(&h).and_then(|w| w.props.iter().find(|(x, _)| x == key).map(|p| p.1)));
    if v.is_none() && !key.starts_with('#') {
        w32(crate::cur().teb + crate::teb_last_error(), 2);
    }
    v.unwrap_or(0)
}
fn GetPropA(c: &mut Ctx) -> u32 {
    let key = prop_key(c.a(1), false);
    get_prop(c.a(0), &key)
}
fn GetPropW(c: &mut Ctx) -> u32 {
    let key = prop_key(c.a(1), true);
    get_prop(c.a(0), &key)
}
fn RemovePropA(c: &mut Ctx) -> u32 {
    let key = prop_key(c.a(1), false);
    with(|s| {
        let Some(w) = s.wins.get_mut(&c.a(0)) else { return 0 };
        let v = w.props.iter().find(|(x, _)| *x == key).map_or(0, |p| p.1);
        w.props.retain(|(x, _)| *x != key);
        v
    })
}
fn RemovePropW(c: &mut Ctx) -> u32 {
    let key = prop_key(c.a(1), true);
    with(|s| {
        let Some(w) = s.wins.get_mut(&c.a(0)) else { return 0 };
        let v = w.props.iter().find(|(x, _)| *x == key).map_or(0, |p| p.1);
        w.props.retain(|(x, _)| *x != key);
        v
    })
}
fn SetWindowTextA(c: &mut Ctx) -> u32 {
    let (h, t) = (c.a(0), c.a(1));
    as_wide(false, || send(c, h, 0xc, 0, t));
    1
}
fn SetWindowTextW(c: &mut Ctx) -> u32 {
    let (h, t) = (c.a(0), c.a(1));
    as_wide(true, || send(c, h, 0xc, 0, t));
    1
}
fn GetWindowTextA(c: &mut Ctx) -> u32 {
    let (h, b, n) = (c.a(0), c.a(1), c.a(2));
    as_wide(false, || send(c, h, 0xd, n, b))
}
fn GetWindowTextW(c: &mut Ctx) -> u32 {
    let (h, b, n) = (c.a(0), c.a(1), c.a(2));
    as_wide(true, || send(c, h, 0xd, n, b))
}
fn GetWindowTextLengthA(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    as_wide(false, || send(c, h, 0xe, 0, 0))
}
fn GetWindowTextLengthW(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    as_wide(true, || send(c, h, 0xe, 0, 0))
}
fn SetWindowRgn(_: &mut Ctx) -> u32 {
    1
}

// ---------------------------------------------------------------------------------------------
// DefWindowProc

fn def_proc(c: &mut Ctx, proc_wide: bool) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    // text parameters are in the sender's encoding (sake translates no messages), whichever of
    // DefWindowProcA/W the window procedure calls
    let wide = msg_wide();
    match m {
        0x81 => {
            // WM_NCCREATE: the title
            let name = r32(lp + 36);
            if name >= 0x10000 {
                let t = if wide { wstr(name) } else { a2w(&cstr(name)) };
                with(|s| {
                    if let Some(w) = s.wins.get_mut(&h) {
                        w.text = t;
                    }
                });
            }
            1
        }
        0xc => {
            let t = if lp == 0 { Vec::new() } else if wide { wstr(lp) } else { a2w(&cstr(lp)) };
            let top = with(|s| {
                s.wins.get_mut(&h).is_some_and(|w| {
                    w.text = t;
                    w.parent == 0
                })
            });
            if top {
                publish();
            }
            1
        }
        0xd => {
            let t = with(|s| s.wins.get(&h).map(|w| w.text.clone())).unwrap_or_default();
            if wp == 0 {
                return 0;
            }
            if wide {
                mem::put_wstr(lp, wp, &t)
            } else {
                mem::put_cstr(lp, wp, &w2a(&t))
            }
        }
        0xe => {
            let t = with(|s| s.wins.get(&h).map(|w| w.text.clone())).unwrap_or_default();
            if wide { t.len() as u32 } else { w2a(&t).len() as u32 }
        }
        0x10 => {
            destroy(c, h);
            0
        }
        0x11 | 0x16 => 1,
        0xf => {
            // BeginPaint/EndPaint: validates, and erases if asked to
            if with(|s| s.wins.get(&h).is_some_and(|w| w.erase)) {
                let ps = mem::heap_alloc(64, true);
                gcall(c, "user32", "BeginPaint", &[h, ps]);
                gcall(c, "user32", "EndPaint", &[h, ps]);
                mem::heap_free(ps);
            }
            with(|s| {
                if let Some(w) = s.wins.get_mut(&h) {
                    w.update = Rect::default();
                    w.erase = false;
                }
            });
            0
        }
        0x14 => {
            let brush = with(|s| s.wins.get(&h).map_or(0, |w| s.classes[w.class].brush));
            if brush != 0 {
                let r = with(|s| s.wins.get(&h).map(|w| Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() })).unwrap_or_default();
                crate::gdi::fill_rect_brush(wp, r, brush);
                return 1;
            }
            0
        }
        0x84 => 1,
        0x20 => {
            // WM_SETCURSOR: the class cursor in the client area
            if lp & 0xffff == 1 {
                let cur = with(|s| s.wins.get(&h).map_or(0, |w| s.classes[w.class].cursor));
                if cur != 0 {
                    with(|s| s.cursor_handle = cur);
                }
                return 1;
            }
            0
        }
        0x21 => 1,
        0x46 => 0,
        0x47 => {
            let (.., f) = windowpos_read(lp);
            send_move_size(c, h, f & SWP_NOCLIENTMOVE == 0, f & SWP_NOCLIENTSIZE == 0);
            0
        }
        0x24 => 0,
        0x83 => 0,
        0x112 => {
            if wp & 0xfff0 == 0xf060 {
                send(c, h, 0x10, 0, 0);
            }
            0
        }
        0x104 => {
            // Alt+F4
            if wp == 0x73 {
                send(c, h, 0x112, 0xf060, 0);
            }
            0
        }
        0x6 => {
            if wp & 0xffff != 0 {
                set_focus(c, h);
            }
            0
        }
        0x7f => 0,
        0x55 => if proc_wide { 2 } else { 1 },
        0x7b | 0x205 => {
            let p = GetParent_raw(h);
            if p != 0 && m == 0x7b {
                return send(c, p, m, wp, lp);
            }
            0
        }
        0x2b | 0x2c | 0x2e | 0x39 => 0,
        0x11a | 0x284 => 0,
        _ => 0,
    }
}
fn GetParent_raw(h: u32) -> u32 {
    with(|s| s.wins.get(&h).map_or(0, |w| w.parent))
}
fn DefWindowProcA(c: &mut Ctx) -> u32 {
    def_proc(c, false)
}
fn DefWindowProcW(c: &mut Ctx) -> u32 {
    def_proc(c, true)
}
fn DefDlgProcA(c: &mut Ctx) -> u32 {
    let (h, m, wp) = (c.a(0), c.a(1), c.a(2));
    if m == 0x7 {
        // WM_SETFOCUS: on to the control that had it (or the first tab stop)
        let to = with(|s| {
            let kids = s.wins.get(&h).map(|w| w.children.clone()).unwrap_or_default();
            if kids.contains(&wp) {
                return wp;
            }
            kids.into_iter().find(|k| s.wins.get(k).is_some_and(|w| w.visible && w.enabled && w.style & 0x1_0000 != 0)).unwrap_or(0)
        });
        if to != 0 {
            set_focus(c, to);
        }
        return 0;
    }
    if m == 0x14 {
        // dialogs erase with the 3D face colour
        let r = with(|s| s.wins.get(&h).map(|w| Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() })).unwrap_or_default();
        crate::gdi::fill_rect_brush(wp, r, crate::gdi::solid_brush(0xf0f0f0));
        return 1;
    }
    def_proc(c, false)
}
fn DefFrameProcA(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(2), c.a(3), c.a(4));
    let save = c.esp;
    w32(c.esp + 8, h);
    w32(c.esp + 12, m);
    w32(c.esp + 16, wp);
    w32(c.esp + 20, lp);
    c.esp = save + 4;
    let r = def_proc(c, false);
    c.esp = save;
    r
}
fn DefMDIChildProcA(c: &mut Ctx) -> u32 {
    def_proc(c, false)
}
fn CallWindowProcA(c: &mut Ctx) -> u32 {
    let (f, h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if f == 0 {
        return 0;
    }
    c.call(f, &[h, m, wp, lp])
}
fn CallWindowProcW(c: &mut Ctx) -> u32 {
    CallWindowProcA(c)
}

// ---------------------------------------------------------------------------------------------
// messages

fn now_ms() -> u32 {
    crate::k32::ticks_ms()
}

pub fn post(hwnd: u32, msg: u32, w: u32, l: u32) -> bool {
    let tid = if hwnd == 0 { crate::cur().tid } else {
        match with(|s| s.wins.get(&hwnd).map(|w| w.tid)) {
            Some(t) => t,
            None => return remote(hwnd).is_some_and(|rw| to_remote(&rw, msg, w, l, false) != 0),
        }
    };
    post_thread(tid, Msg { hwnd, msg, w, l, time: now_ms(), x: 0, y: 0 });
    true
}
fn post_thread(tid: u32, mut m: Msg) {
    with(|s| {
        m.x = s.cursor.0;
        m.y = s.cursor.1;
        let q = s.queues.entry(tid).or_default();
        q.posted.push_back(m);
        q.fresh |= 0x8;
    });
    crate::obj::signal();
}
fn PostMessageA(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h == 0xffff {
        // HWND_BROADCAST: the top-level windows
        let v: Vec<u32> = with(|s| s.wins.iter().filter(|(_, w)| w.parent == 0).map(|(&h, _)| h).collect());
        for h in v {
            post(h, c.a(1), c.a(2), c.a(3));
        }
        return 1;
    }
    post(h, c.a(1), c.a(2), c.a(3)) as u32
}
fn PostMessageW(c: &mut Ctx) -> u32 {
    PostMessageA(c)
}
// DDE: an instance and string handles, no other program to talk to (single-instance checks
// connect, find nobody and go on as the server)
static DDE_NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);
fn DdeInitializeA(c: &mut Ctx) -> u32 {
    put32(c.a(0), DDE_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
    0
}
static DDE_STR: Mutex<Vec<(u32, Vec<u16>)>> = Mutex::new(Vec::new());
/// String handles are atoms: the same text in any case is the same handle (with the first text's case)
fn dde_string(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(1);
    let t = if p == 0 { Vec::new() } else if wide { wstr(p) } else { a2w(&cstr(p)) };
    let low = |v: &[u16]| v.iter().map(|&ch| crate::text::lower16(ch)).collect::<Vec<_>>();
    let mut g = DDE_STR.lock().unwrap();
    if let Some(x) = g.iter().find(|x| low(&x.1) == low(&t)) {
        return x.0;
    }
    let h = 0xc000 + g.len() as u32;
    g.push((h, t));
    h
}
fn DdeCreateStringHandleA(c: &mut Ctx) -> u32 {
    dde_string(c, false)
}
fn DdeCreateStringHandleW(c: &mut Ctx) -> u32 {
    dde_string(c, true)
}
/// DdeQueryString(inst, hsz, buffer, cch, codepage): with no buffer the length without the NUL;
/// else the characters copied (at most cch-1, and a NUL)
fn dde_query(c: &mut Ctx, wide: bool) -> u32 {
    let (h, buf, cch) = (c.a(1), c.a(2), c.a(3));
    let Some(t) = DDE_STR.lock().unwrap().iter().find(|x| x.0 == h).map(|x| x.1.clone()) else { return 0 };
    let len = if wide { t.len() } else { w2a(&t).len() } as u32;
    if buf == 0 || cch == 0 {
        return len;
    }
    if wide {
        mem::put_wstr(buf, cch, &t);
    } else {
        mem::put_cstr(buf, cch, &w2a(&t));
    }
    len.min(cch - 1)
}
fn DdeQueryStringA(c: &mut Ctx) -> u32 {
    dde_query(c, false)
}
fn DdeQueryStringW(c: &mut Ctx) -> u32 {
    dde_query(c, true)
}
/// The handles' values (the same text is the same handle)
fn DdeCmpStringHandles(c: &mut Ctx) -> u32 {
    match c.a(0).cmp(&c.a(1)) {
        std::cmp::Ordering::Less => u32::MAX,
        std::cmp::Ordering::Equal => 0,
        _ => 1,
    }
}
fn dde_true(_: &mut Ctx) -> u32 {
    1
}
fn DdeConnect(_: &mut Ctx) -> u32 {
    0
}
fn DdeGetLastError(_: &mut Ctx) -> u32 {
    0x400a // DMLERR_NO_CONV_ESTABLISHED
}
pub fn post_thread_msg(tid: u32, msg: u32, w: u32, l: u32) {
    post_thread(tid, Msg { hwnd: 0, msg, w, l, time: now_ms(), x: 0, y: 0 });
}
fn PostThreadMessageA(c: &mut Ctx) -> u32 {
    post_thread(c.a(0), Msg { hwnd: 0, msg: c.a(1), w: c.a(2), l: c.a(3), time: now_ms(), x: 0, y: 0 });
    1
}
fn PostThreadMessageW(c: &mut Ctx) -> u32 {
    PostThreadMessageA(c)
}
fn PostQuitMessage(c: &mut Ctx) -> u32 {
    let tid = crate::cur().tid;
    with(|s| s.queues.entry(tid).or_default().quit = Some(c.a(0)));
    crate::obj::signal();
    0
}
fn send_message(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if h == 0xffff {
        let v: Vec<u32> = with(|s| s.wins.iter().filter(|(_, w)| w.parent == 0).map(|(&h, _)| h).collect());
        for h in v {
            send(c, h, m, wp, lp);
        }
        return 1;
    }
    send(c, h, m, wp, lp)
}
fn SendMessageA(c: &mut Ctx) -> u32 {
    as_wide(false, || send_message(c))
}
fn SendMessageW(c: &mut Ctx) -> u32 {
    as_wide(true, || send_message(c))
}
fn SendMessageTimeoutA(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp, res) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(6));
    let r = send(c, h, m, wp, lp);
    put32(res, r);
    1
}
fn SendNotifyMessageA(c: &mut Ctx) -> u32 {
    post(c.a(0), c.a(1), c.a(2), c.a(3)) as u32
}
fn InSendMessage(_: &mut Ctx) -> u32 {
    0
}
fn ReplyMessage(_: &mut Ctx) -> u32 {
    0
}

fn matches(m: &Msg, hwnd: u32, lo: u32, hi: u32) -> bool {
    (hwnd == 0 || m.hwnd == hwnd || hwnd == 0xffff_ffff && m.hwnd == 0) && (lo == 0 && hi == 0 || m.msg >= lo && m.msg <= hi)
}

/// The next message for this thread (PeekMessage); `remove` takes it off the queue.
fn next_msg(hwnd: u32, lo: u32, hi: u32, remove: bool) -> Option<Msg> {
    let me = crate::cur().tid;
    with(|s| {
        let q = s.queues.entry(me).or_default();
        if let Some(i) = q.posted.iter().position(|m| matches(m, hwnd, lo, hi)) {
            return Some(if remove { q.posted.remove(i).unwrap() } else { q.posted[i] });
        }
        if let Some(i) = q.input.iter().position(|m| matches(m, hwnd, lo, hi)) {
            let m = if remove { q.input.remove(i).unwrap() } else { q.input[i] };
            return Some(m);
        }
        if let Some(code) = q.quit {
            if lo == 0 && hi == 0 || (lo..=hi).contains(&0x12) {
                if remove {
                    q.quit = None;
                }
                return Some(Msg { hwnd: 0, msg: 0x12, w: code, l: 0, time: now_ms(), x: 0, y: 0 });
            }
        }
        // WM_PAINT for a window of ours with an update region
        if lo == 0 && hi == 0 || (lo..=hi).contains(&0xf) {
            let mut ws: Vec<u32> = s
                .wins
                .iter()
                .filter(|(&h, w)| w.tid == me && !w.update.empty() && (hwnd == 0 || hwnd == h) && shown_tree(s, h))
                .map(|(&h, _)| h)
                .collect();
            ws.sort();
            if let Some(&h) = ws.first() {
                return Some(Msg { hwnd: h, msg: 0xf, w: 0, l: 0, time: now_ms(), x: 0, y: 0 });
            }
        }
        // timers
        if lo == 0 && hi == 0 || (lo..=hi).contains(&0x113) {
            let now = Instant::now();
            let q = s.queues.get_mut(&me).unwrap();
            if let Some(t) = q.timers.iter_mut().filter(|t| t.due <= now && (hwnd == 0 || t.hwnd == hwnd)).min_by_key(|t| t.due) {
                let m = Msg { hwnd: t.hwnd, msg: 0x113, w: t.id, l: t.proc_, time: now_ms(), x: 0, y: 0 };
                if remove {
                    t.due = now + Duration::from_millis(t.ms.max(10) as u64);
                }
                return Some(m);
            }
        }
        None
    })
}

/// Until the next timer of this thread is due.
fn next_timer() -> Option<Duration> {
    let me = crate::cur().tid;
    with(|s| {
        let q = s.queues.get(&me)?;
        let now = Instant::now();
        q.timers.iter().map(|t| t.due.saturating_duration_since(now)).min()
    })
}

fn has_work() -> bool {
    has_work_of(QS_ALL)
}
const QS_ALL: u32 = 0x4ff;
/// Whether thread `me` has messages of the QS_* kinds in `mask`: input (key, mouse, raw input
/// 0x407), posted (0x108), paint (0x20), sent (0x40). (Delphi's TThread.WaitFor waits for
/// QS_SENDMESSAGE only and peeks without removing: a pending paint must not wake it.)
fn queued(s: &State, me: u32, mask: u32) -> bool {
    s.queues.get(&me).is_some_and(|q| {
        mask & 0x108 != 0 && (!q.posted.is_empty() || q.quit.is_some())
            || mask & 0x407 != 0 && !q.input.is_empty()
            || mask & 0x40 != 0 && !q.sent.is_empty()
    }) || mask & 0x20 != 0 && s.wins.iter().any(|(&h, w)| w.tid == me && !w.update.empty() && shown_tree(s, h))
}
fn has_work_of(mask: u32) -> bool {
    let me = crate::cur().tid;
    with(|s| queued(s, me, mask))
}

/// Blocks until this thread has a message, a handle is signaled or `ms` passes.
fn wait_msgs(handles: &[u32], all: bool, ms: u32) -> u32 {
    wait_msgs_of(handles, all, ms, QS_ALL, false)
}
/// (`apc`: an APC queued for the thread ends the wait too)
fn wait_msgs_of(handles: &[u32], all: bool, ms: u32, mask: u32, apc: bool) -> u32 {
    start_input();
    let timer = next_timer();
    let ms = match timer {
        Some(t) => ms.min(t.as_millis() as u32 + 1),
        None => ms,
    };
    let me = crate::cur().tid;
    let check = move || {
        // (called with the object table locked: the USER lock is taken inside, never the other way)
        let s = STATE.try_lock();
        apc && crate::apc::pending(me)
            || match s {
                Ok(g) => g.as_ref().is_some_and(|s| queued(s, me, mask)),
                Err(_) => true,
            }
    };
    crate::obj::wait(handles, all, ms, Some(&check))
}

fn get_message(c: &mut Ctx, p: u32, hwnd: u32, lo: u32, hi: u32) -> u32 {
    loop {
        process_sent(c.cpu);
        if let Some(m) = next_msg(hwnd, lo, hi, true) {
            m.write(p);
            with(|s| {
                s.last_msg_pos = (m.x, m.y);
                s.last_msg_time = m.time;
            });
            return (m.msg != 0x12) as u32;
        }
        wait_msgs(&[], false, 0xffff_ffff);
    }
}
fn GetMessageA(c: &mut Ctx) -> u32 {
    let (p, h, lo, hi) = (c.a(0), c.a(1), c.a(2), c.a(3));
    get_message(c, p, h, lo, hi)
}
fn GetMessageW(c: &mut Ctx) -> u32 {
    GetMessageA(c)
}
fn peek(c: &mut Ctx) -> u32 {
    let (p, h, lo, hi, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    start_input();
    // a turn of the game loop: the frame's big redraws wait for the display here
    kura::display::pace_frame();
    process_sent(c.cpu);
    match next_msg(h, lo, hi, flags & 1 != 0) {
        Some(m) => {
            m.write(p);
            with(|s| {
                s.last_msg_pos = (m.x, m.y);
                s.last_msg_time = m.time;
            });
            1
        }
        None => {
            idle_yield();
            0
        }
    }
}
/// A PeekMessage loop with nothing to do gives the CPU away now and then (Windows' would
/// sleep in the scheduler on a busy system; here other guest threads need the core).
fn idle_yield() {
    thread_local!(static N: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });
    N.with(|n| {
        let v = n.get() + 1;
        n.set(v);
        if v % 64 == 0 {
            std::thread::yield_now();
        }
    });
}
fn PeekMessageA(c: &mut Ctx) -> u32 {
    peek(c)
}
fn PeekMessageW(c: &mut Ctx) -> u32 {
    peek(c)
}
fn WaitMessage(_: &mut Ctx) -> u32 {
    if !has_work() {
        wait_msgs(&[], false, 0xffff_ffff);
    }
    1
}
fn MsgWaitForMultipleObjects(c: &mut Ctx) -> u32 {
    let (n, hs, all, ms, mask) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3), c.a(4));
    let hs: Vec<u32> = (0..n.min(64)).map(|i| mem::rp(hs + mem::ps() * i)).collect();
    msg_wait(c, &hs, all, ms, mask, false)
}
fn MsgWaitForMultipleObjectsEx(c: &mut Ctx) -> u32 {
    let (n, hs, ms, mask, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let hs: Vec<u32> = (0..n.min(64)).map(|i| mem::rp(hs + mem::ps() * i)).collect();
    let alertable = flags & 2 != 0; // MWMO_ALERTABLE
    if alertable && crate::apc::run(c) {
        return crate::apc::WAIT_IO_COMPLETION;
    }
    let r = msg_wait(c, &hs, flags & 1 != 0, ms, mask, alertable);
    if alertable && r == n && crate::apc::run(c) { crate::apc::WAIT_IO_COMPLETION } else { r }
}
/// A due timer of this thread's (QS_TIMER: its WM_TIMER is as good as posted)
fn timer_due() -> bool {
    let me = crate::cur().tid;
    let now = Instant::now();
    with(|s| s.queues.get(&me).is_some_and(|q| q.timers.iter().any(|t| t.due <= now)))
}
/// MsgWaitForMultipleObjects(Ex): input of the `mask` kinds (a message ends the wait before a
/// signaled handle, as on Windows) or a handle; with `all` (MWMO_WAITALL), every handle AND
/// input, the handles taken only then (tests/winref/probes/msgwait.c). WAIT_OBJECT_0 + n for
/// input; an APC (`apc`) also answers n, for the caller to run it.
fn msg_wait(c: &mut Ctx, hs: &[u32], all: bool, ms: u32, mask: u32, apc: bool) -> u32 {
    const TIMEOUT: u32 = 0x102;
    let n = hs.len() as u32;
    let deadline = (ms != 0xffff_ffff).then(|| Instant::now() + Duration::from_millis(ms as u64));
    let me = crate::cur().tid;
    if !crate::obj::waitable(hs) {
        crate::k32::last_error(6);
        return 0xffff_ffff;
    }
    loop {
        process_sent(c.cpu);
        if apc && crate::apc::pending(me) {
            return n;
        }
        let input = has_work_of(mask) || mask & 0x10 != 0 && timer_due();
        if input {
            if !all {
                return n;
            }
            let r = crate::obj::wait(hs, true, 0, None);
            if r != TIMEOUT {
                return r;
            }
        }
        let left = deadline.map_or(0xffff_ffff, |d| d.saturating_duration_since(Instant::now()).as_millis() as u32);
        if left == 0 {
            return TIMEOUT;
        }
        // (with `all` the handles are looked at between short waits for input)
        let r = if all { wait_msgs_of(&[], false, left.min(10), mask, apc) } else { wait_msgs_of(hs, false, left, mask, apc) };
        if !all && r != TIMEOUT && r != n {
            return r;
        }
    }
}
/// GetQueueStatus(flags): high word, the QS_* kinds in the queue; low word, those that came since
/// the last call (posted and sent messages; input, timers and paint count while there)
fn GetQueueStatus(c: &mut Ctx) -> u32 {
    let flags = c.a(0);
    let me = crate::cur().tid;
    let now = Instant::now();
    with(|s| {
        let paint = s.wins.iter().any(|(&h, w)| w.tid == me && !w.update.empty() && shown_tree(s, h));
        let q = s.queues.entry(me).or_default();
        let mut st = 0;
        if !q.posted.is_empty() || q.quit.is_some() {
            st |= 0x8;
        }
        if !q.sent.is_empty() {
            st |= 0x40;
        }
        if q.timers.iter().any(|t| t.due <= now) {
            st |= 0x10;
        }
        for m in &q.input {
            st |= match m.msg {
                0x100..=0x109 => 0x1,
                0x200 => 0x2,
                0x201..=0x20e => 0x4,
                _ => 0x400,
            };
        }
        if paint {
            st |= 0x20;
        }
        let fresh = (q.fresh | st & !0x48) & flags;
        q.fresh &= !flags;
        (st & flags) << 16 | fresh
    })
}
fn GetInputState(_: &mut Ctx) -> u32 {
    let me = crate::cur().tid;
    with(|s| s.queues.get(&me).is_some_and(|q| !q.input.is_empty())) as u32
}
fn TranslateMessage(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let (h, m, wp, lp) = msg_at(p);
    if m == 0x100 || m == 0x104 {
        let shift = with(|s| s.keys[0x10] & 0x80 != 0);
        let ch = match wp {
            0x30..=0x39 => Some(wp),
            0x41..=0x5a => Some(if shift { wp } else { wp + 32 }),
            0x20 | 0x0d | 0x08 | 0x1b | 0x09 => Some(wp),
            _ => None,
        };
        if let Some(ch) = ch {
            post(h, if m == 0x100 { 0x102 } else { 0x106 }, ch, lp);
            return 1;
        }
    }
    0
}
fn DispatchMessageA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let (h, m, wp, lp) = msg_at(p);
    if m == 0x113 && lp != 0 {
        return c.call(lp, &[h, m, wp, now_ms()]);
    }
    if h == 0 {
        return 0;
    }
    if (0x200..=0x20a).contains(&m) && m != 0x200 && crate::env_on!("SAKE_TRACE_INPUT") {
        eprintln!("sake input: dispatch {h:x} msg {m:x} {wp:x} {lp:x}");
    }
    let r = send(c, h, m, wp, lp);
    if (0x200..=0x20a).contains(&m) && m != 0x200 && crate::env_on!("SAKE_TRACE_INPUT") {
        eprintln!("sake input: dispatched {m:x} -> {r:x}");
    }
    if m == 0xf {
        // a WM_PAINT nobody validated must not come back forever
        with(|s| {
            if let Some(w) = s.wins.get_mut(&h) {
                w.update = Rect::default();
            }
        });
    }
    r
}
fn DispatchMessageW(c: &mut Ctx) -> u32 {
    DispatchMessageA(c)
}
/// IsDialogMessage(dlg, msg): Enter / Escape pressed in the dialog become IDOK / IDCANCEL; its
/// other messages are translated and dispatched (TRUE); messages of other windows are FALSE.
fn is_dialog_message(c: &mut Ctx, wide: bool) -> u32 {
    let (dlg, msg) = (c.a(0), c.a(1));
    let (h, m, key, _) = msg_at(msg);
    if dlg == 0 || h == 0 {
        return 0;
    }
    let inside = h == dlg
        || with(|s| {
            let mut h = h;
            loop {
                match s.wins.get(&h) {
                    Some(w) if w.style & 0x4000_0000 != 0 => {
                        if w.parent == dlg {
                            return true;
                        }
                        h = w.parent;
                    }
                    _ => return false,
                }
            }
        });
    if !inside {
        return 0;
    }
    if m != 0x100 || !(key == 0x0d || key == 0x1b) {
        // the dialog's other messages: translated and dispatched here, as Windows does (the
        // caller's loop skips its own TranslateMessage/DispatchMessage for them)
        let t = crate::api::export("user32", "TranslateMessage");
        c.call(t, &[msg]);
        let d = crate::api::export("user32", if wide { "DispatchMessageW" } else { "DispatchMessageA" });
        c.call(d, &[msg]);
        return 1;
    }
    let (id, b) = if key == 0x0d { default_button(dlg) } else { (2, dlg_item(dlg, 2)) };
    let f = crate::api::export("user32", if wide { "SendMessageW" } else { "SendMessageA" });
    c.call(f, &[dlg, 0x111, id, b]);
    1
}
fn IsDialogMessageA(c: &mut Ctx) -> u32 {
    is_dialog_message(c, false)
}
fn IsDialogMessageW(c: &mut Ctx) -> u32 {
    is_dialog_message(c, true)
}
fn TranslateMDISysAccel(_: &mut Ctx) -> u32 {
    0
}
fn GetMessagePos(_: &mut Ctx) -> u32 {
    with(|s| ((s.last_msg_pos.1 as u32) << 16) | (s.last_msg_pos.0 as u32 & 0xffff))
}
fn GetMessageTime(_: &mut Ctx) -> u32 {
    with(|s| s.last_msg_time)
}
fn GetMessageExtraInfo(_: &mut Ctx) -> u32 {
    0
}
fn RegisterWindowMessageA(c: &mut Ctx) -> u32 {
    let n = arg_str(c.a(0), false);
    with(|s| {
        let k = s.msgnames.len() as u32;
        *s.msgnames.entry(n.to_lowercase()).or_insert(0xc000 + k)
    })
}
fn RegisterWindowMessageW(c: &mut Ctx) -> u32 {
    let n = arg_str(c.a(0), true);
    with(|s| {
        let k = s.msgnames.len() as u32;
        *s.msgnames.entry(n.to_lowercase()).or_insert(0xc000 + k)
    })
}

fn SetTimer(c: &mut Ctx) -> u32 {
    let (h, id, ms, f) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let me = crate::cur().tid;
    let tid = if h == 0 { me } else { with(|s| s.wins.get(&h).map_or(me, |w| w.tid)) };
    let id = with(|s| {
        let q = s.queues.entry(tid).or_default();
        let id = if h == 0 && id == 0 { 0x7f00 + q.timers.len() as u32 } else { id };
        q.timers.retain(|t| !(t.hwnd == h && t.id == id));
        q.timers.push(Timer { hwnd: h, id, ms, proc_: f, due: Instant::now() + Duration::from_millis(ms.max(10) as u64) });
        id
    });
    crate::obj::signal();
    if h == 0 { id } else { id.max(1) }
}
fn KillTimer(c: &mut Ctx) -> u32 {
    let (h, id) = (c.a(0), c.a(1));
    let found = with(|s| {
        let mut found = false;
        for q in s.queues.values_mut() {
            let n = q.timers.len();
            q.timers.retain(|t| !(t.hwnd == h && t.id == id));
            found |= q.timers.len() != n;
        }
        found
    });
    found as u32
}

// painting
/// Visible with all its ancestors (IsWindowVisible): a child of a hidden window is not
/// painted (property sheet pages hidden behind the shown one).
fn shown_tree(s: &State, mut h: u32) -> bool {
    loop {
        match s.wins.get(&h) {
            Some(w) if w.visible => {
                if w.parent == 0 || w.parent == DESKTOP {
                    return true;
                }
                h = w.parent;
            }
            _ => return false,
        }
    }
}

/// A window shown: it and everything in it is to be painted.
fn invalidate_tree(s: &mut State, h: u32) {
    let mut todo = vec![h];
    while let Some(h) = todo.pop() {
        if let Some(w) = s.wins.get_mut(&h) {
            w.update = Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() };
            w.erase = true;
            todo.extend(w.children.iter().copied());
        }
    }
}

fn invalidate(h: u32, r: Option<Rect>, erase: bool) {
    with(|s| {
        let all = h == 0;
        let hs: Vec<u32> = if all { s.wins.keys().copied().collect() } else { vec![h] };
        for h in hs {
            if let Some(w) = s.wins.get_mut(&h) {
                let full = Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() };
                let r = r.map_or(full, |r| r.and(&full));
                w.update = w.update.or(&r);
                w.erase |= erase;
            }
        }
    });
    crate::obj::signal();
}
fn InvalidateRect(c: &mut Ctx) -> u32 {
    let r = (c.a(1) != 0).then(|| Rect::read(c.a(1)));
    invalidate(c.a(0), r, c.a(2) != 0);
    1
}
fn InvalidateRgn(c: &mut Ctx) -> u32 {
    let r = (c.a(1) != 0).then(|| crate::gdi::region_bounds(c.a(1))).flatten();
    invalidate(c.a(0), r, c.a(2) != 0);
    1
}
/// Takes `r` (None: all) out of the update region (its bounding box)
fn validate(h: u32, r: Option<Rect>) {
    with(|s| {
        if let Some(w) = s.wins.get_mut(&h) {
            w.update = r.map_or(Rect::default(), |r| w.update.minus(&r));
            if w.update.empty() {
                w.erase = false;
            }
        }
    });
}
fn ValidateRect(c: &mut Ctx) -> u32 {
    validate(c.a(0), (c.a(1) != 0).then(|| Rect::read(c.a(1))));
    1
}
fn ValidateRgn(c: &mut Ctx) -> u32 {
    validate(c.a(0), (c.a(1) != 0).then(|| crate::gdi::region_bounds(c.a(1))).flatten());
    1
}
/// RedrawWindow(h, rect, region, flags): RDW_INVALIDATE 1, RDW_ERASE 4, RDW_VALIDATE 8,
/// RDW_NOERASE 0x20, RDW_UPDATENOW 0x100
fn RedrawWindow(c: &mut Ctx) -> u32 {
    let (h, r, rgn, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let area = if r != 0 { Some(Rect::read(r)) } else if rgn != 0 { crate::gdi::region_bounds(rgn) } else { None };
    if flags & 1 != 0 {
        invalidate(h, area, flags & 4 != 0);
    } else if flags & 8 != 0 {
        validate(h, area);
    }
    if flags & 0x20 != 0 {
        with(|s| {
            if let Some(w) = s.wins.get_mut(&h) {
                w.erase = false;
            }
        });
    }
    if flags & 0x100 != 0 {
        let dirty = with(|s| s.wins.get(&h).is_some_and(|w| !w.update.empty()));
        if dirty {
            send(c, h, 0xf, 0, 0);
        }
    }
    1
}
fn GetUpdateRect(c: &mut Ctx) -> u32 {
    let r = with(|s| s.wins.get(&c.a(0)).map(|w| w.update)).unwrap_or_default();
    r.write(c.a(1));
    (!r.empty()) as u32
}
fn GetUpdateRgn(c: &mut Ctx) -> u32 {
    let e = with(|s| s.wins.get(&c.a(0)).is_none_or(|w| w.update.empty()));
    if e { 1 } else { 2 }
}
fn BeginPaint(c: &mut Ctx) -> u32 {
    let (h, ps) = (c.a(0), c.a(1));
    let (upd, erase) = with(|s| {
        let Some(w) = s.wins.get_mut(&h) else { return (Rect::default(), false) };
        let u = if w.update.empty() { Rect::default() } else { w.update };
        let e = w.erase;
        w.update = Rect::default();
        w.erase = false;
        (u, e)
    });
    // (a CS_PARENTDC child paints with its parent's visible region)
    let info = dc_info(h);
    let dc = crate::gdi::get_dce(h, info, 0, DCX_USESTYLE);
    if !is_parent_dc(h) {
        crate::gdi::set_paint(dc, Some(upd));
    }
    if erase {
        send(c, h, 0x14, dc, 0);
    }
    // without WS_CLIPCHILDREN the parent paints over its children: they paint again after it
    let kids: Vec<u32> = with(|s| {
        let Some(w) = s.wins.get(&h).filter(|w| w.style & 0x0200_0000 == 0) else { return Vec::new() };
        w.children.iter().copied().filter(|k| s.wins.get(k).is_some_and(|x| x.visible && !x.rect.and(&upd).empty())).collect()
    });
    for k in kids {
        invalidate(k, None, true);
    }
    // PAINTSTRUCT: (hdc, fErase, rcPaint, ...); x64: hdc 8 bytes, 72 in all
    let at = if mem::x64() { 8 } else { 4 };
    mem::fill(ps, if mem::x64() { 72 } else { 64 }, 0);
    mem::wp(ps, dc);
    upd.write(ps + at + 4);
    dc
}
fn EndPaint(c: &mut Ctx) -> u32 {
    let ps = c.a(1);
    crate::gdi::end_paint(mem::rp(ps));
    1
}
const DCX_USESTYLE: u32 = 0x1_0000;

/// A window's class style and class (for its DCs)
pub fn dc_info(h: u32) -> Option<(u32, usize)> {
    with(|s| s.wins.get(&h).map(|w| (s.classes[w.class].style, w.class)))
}

fn is_parent_dc(h: u32) -> bool {
    with(|s| s.wins.get(&h).is_some_and(|w| w.parent != 0 && w.parent != DESKTOP && s.classes[w.class].style & 0x80 != 0))
}

fn get_dc_ex(h: u32, rgn: u32, flags: u32) -> u32 {
    if h == 0 || h == DESKTOP {
        return crate::gdi::get_dce(0, None, rgn, flags | crate::gdi::DCX_CACHE);
    }
    crate::gdi::get_dce(h, dc_info(h), rgn, flags)
}
fn GetDC(c: &mut Ctx) -> u32 {
    get_dc_ex(c.a(0), 0, DCX_USESTYLE)
}
fn GetDCEx(c: &mut Ctx) -> u32 {
    get_dc_ex(c.a(0), c.a(1), c.a(2))
}
fn GetWindowDC(c: &mut Ctx) -> u32 {
    get_dc_ex(c.a(0), 0, DCX_USESTYLE)
}
fn ReleaseDC(c: &mut Ctx) -> u32 {
    crate::gdi::release_dce(c.a(1))
}
fn WindowFromDC(c: &mut Ctx) -> u32 {
    crate::gdi::dc_window(c.a(0))
}
/// What a scroll leaves to paint: the scrolled area (client coordinates) inside the clip
/// rectangle. The pixels are not moved: the whole area is painted again (the program draws its
/// content where it now is), a superset of the strip Windows asks for.
fn scrolled_area(h: u32, scroll: u32, clip: u32, org: (i32, i32)) -> Rect {
    let client = with(|s| s.wins.get(&h).map(|w| Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() })).unwrap_or_default();
    let mut a = client;
    if scroll != 0 {
        a = a.and(&Rect::read(scroll).off(org.0, org.1));
    }
    if clip != 0 {
        a = a.and(&Rect::read(clip).off(org.0, org.1));
    }
    a
}
/// ScrollWindow(h, dx, dy, scroll, clip): the rectangles are in the logical coordinates of the
/// window's own DC (CS_OWNDC / CS_CLASSDC), which it scrolls with
fn ScrollWindow(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let a = scrolled_area(h, c.a(3), c.a(4), crate::gdi::own_dc_origin(h));
    invalidate(h, Some(a), true);
    1
}
/// ScrollWindowEx(h, dx, dy, scroll, clip, update rgn, update rect, flags): SW_INVALIDATE 2,
/// SW_ERASE 4; client coordinates
fn ScrollWindowEx(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if !is_window(h) {
        return 0;
    }
    let a = scrolled_area(h, c.a(3), c.a(4), (0, 0));
    let flags = c.a(7);
    if c.a(5) != 0 {
        crate::gdi::set_region_rect(c.a(5), a);
    }
    a.write(c.a(6));
    if flags & 2 != 0 {
        invalidate(h, Some(a), flags & 4 != 0);
    }
    // NULLREGION 1, SIMPLEREGION 2, COMPLEXREGION 3: a scroll both ways that keeps part of
    // the area uncovers an L (winref dce)
    let (dx, dy) = (c.i(1), c.i(2));
    if a.empty() {
        1
    } else if dx != 0 && dy != 0 && dx.abs() < a.r - a.l && dy.abs() < a.b - a.t {
        3
    } else {
        2
    }
}
fn LockWindowUpdate(_: &mut Ctx) -> u32 {
    1
}

// focus and the like
fn SetFocus(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    set_focus(c, h)
}
fn GetFocus(_: &mut Ctx) -> u32 {
    with(|s| s.focus)
}
fn SetActiveWindow(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let old = with(|s| s.active);
    activate(c, h);
    old
}
fn GetActiveWindow(_: &mut Ctx) -> u32 {
    with(|s| s.active)
}
fn GetForegroundWindow(_: &mut Ctx) -> u32 {
    with(|s| s.active)
}
fn SetForegroundWindow(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    activate(c, h);
    1
}
fn BringWindowToTop(c: &mut Ctx) -> u32 {
    SetForegroundWindow(c)
}
fn SetCapture(c: &mut Ctx) -> u32 {
    with(|s| std::mem::replace(&mut s.capture, c.a(0)))
}
fn ReleaseCapture(_: &mut Ctx) -> u32 {
    with(|s| s.capture = 0);
    1
}
fn GetCapture(_: &mut Ctx) -> u32 {
    with(|s| s.capture)
}
/// SendInput(n, inputs, size): mouse and keyboard events, as if from the devices (hardware
/// events are not simulated)
fn SendInput(c: &mut Ctx) -> u32 {
    let (n, p, size) = (c.a(0), c.a(1), c.a(2));
    let k = mem::ps();
    if size != if mem::x64() { 40 } else { 28 } {
        crate::k32::last_error(87);
        return 0;
    }
    for i in 0..n {
        let e = p + i * size;
        let u = e + k; // the union, pointer-aligned
        match r32(e) {
            0 => {
                let (dx, dy, data, flags) = (r32(u) as i32, r32(u + 4) as i32, r32(u + 8), r32(u + 12));
                if flags & 1 != 0 {
                    let (cx, cy) = with(|s| s.cursor);
                    let (x, y) = if flags & 0x8000 != 0 {
                        // MOUSEEVENTF_ABSOLUTE: 0..65535 over the screen
                        let (w, h) = screen();
                        ((dx as i64 * w as i64 / 65536) as i32, (dy as i64 * h as i64 / 65536) as i32)
                    } else {
                        (cx + dx, cy + dy)
                    };
                    input(1, x, y, 0);
                }
                let (x, y) = with(|s| s.cursor);
                for (bit, kind, b) in [(2, 2, 0), (4, 3, 0), (8, 2, 1), (0x10, 3, 1), (0x20, 2, 2), (0x40, 3, 2)] {
                    if flags & bit != 0 {
                        input(kind, x, y, b);
                    }
                }
                if flags & 0x800 != 0 {
                    input(4, x, y, data & 0xffff);
                }
            }
            1 => {
                let (vk, scan, flags) = (r16(u) as u32, r16(u + 2) as u32, r32(u + 4));
                if flags & 4 != 0 {
                    continue; // KEYEVENTF_UNICODE: no key
                }
                input(if flags & 2 != 0 { 6 } else { 5 }, 0, 0, vk & 0xff | (scan & 0xff) << 16);
            }
            _ => {}
        }
    }
    crate::obj::signal();
    n
}
fn GetCursorPos(c: &mut Ctx) -> u32 {
    let (x, y) = with(|s| s.cursor);
    w32(c.a(0), x as u32);
    w32(c.a(0) + 4, y as u32);
    1
}
fn SetCursorPos(c: &mut Ctx) -> u32 {
    with(|s| s.cursor = (c.i(0), c.i(1)));
    1
}
fn SetCursor(c: &mut Ctx) -> u32 {
    with(|s| std::mem::replace(&mut s.cursor_handle, c.a(0)))
}
fn GetCursor(_: &mut Ctx) -> u32 {
    with(|s| s.cursor_handle)
}
fn ShowCursor(c: &mut Ctx) -> u32 {
    with(|s| {
        s.cursor_count += if c.a(0) != 0 { 1 } else { -1 };
        s.cursor_count as u32
    })
}
fn ClipCursor(_: &mut Ctx) -> u32 {
    1
}
/// The key states (GetKeyboardState's), for DirectInput.
pub fn key_states() -> [u8; 256] {
    with(|s| s.keys)
}
pub fn cursor_pos() -> (i32, i32) {
    with(|s| s.cursor)
}
pub fn vk_scan(vk: u32) -> u32 {
    vk_to_scan(vk)
}
fn GetKeyState(c: &mut Ctx) -> u32 {
    let k = c.a(0) as usize & 0xff;
    let v = with(|s| s.keys[k]);
    (if v & 0x80 != 0 { 0xff80u32 } else { 0 }) | (v & 1) as u32
}
fn GetAsyncKeyState(c: &mut Ctx) -> u32 {
    let k = c.a(0) as usize & 0xff;
    with(|s| {
        let down = s.keys[k] & 0x80 != 0;
        let pressed = std::mem::take(&mut s.pressed[k]);
        // (a press already released counts as down once)
        if down || pressed { 0x8000 | pressed as u32 } else { 0 }
    })
}
fn GetKeyboardState(c: &mut Ctx) -> u32 {
    let k = with(|s| s.keys);
    mem::write(c.a(0), &k);
    1
}
fn SetKeyboardState(c: &mut Ctx) -> u32 {
    let mut k = [0u8; 256];
    k.copy_from_slice(mem::slice(c.a(0), 256));
    with(|s| s.keys = k);
    1
}
fn GetKeyboardType(c: &mut Ctx) -> u32 {
    let ja = crate::text::acp() == 932;
    match c.a(0) {
        0 => if ja { 7 } else { 4 },
        1 => if ja { 2 } else { 0 },
        2 => 12,
        _ => 0,
    }
}
fn GetKeyboardLayout(_: &mut Ctx) -> u32 {
    if crate::text::acp() == 932 { 0x0411_0411 } else { 0x0409_0409 }
}
fn GetKeyboardLayoutList(c: &mut Ctx) -> u32 {
    if c.a(0) > 0 {
        crate::mem::wp(c.a(1), if crate::text::acp() == 932 { 0x0411_0411 } else { 0x0409_0409 });
    }
    1
}
fn ActivateKeyboardLayout(_: &mut Ctx) -> u32 {
    0x0411_0411
}
fn LoadKeyboardLayoutA(_: &mut Ctx) -> u32 {
    0x0411_0411
}
fn MapVirtualKeyA(c: &mut Ctx) -> u32 {
    let (code, ty) = (c.a(0), c.a(1));
    match ty {
        0 | 3 => vk_to_scan(code),
        1 | 2 => {
            if ty == 2 {
                match code {
                    0x30..=0x39 | 0x41..=0x5a => code,
                    _ => 0,
                }
            } else {
                (0..256).find(|&v| vk_to_scan(v) == code).unwrap_or(0)
            }
        }
        _ => 0,
    }
}
fn MapVirtualKeyW(c: &mut Ctx) -> u32 {
    MapVirtualKeyA(c)
}
fn vk_to_scan(vk: u32) -> u32 {
    const T: &[(u32, u32)] = &[(0x1b, 1), (0x08, 14), (0x09, 15), (0x0d, 28), (0x11, 29), (0x10, 42), (0x12, 56), (0x20, 57), (0x25, 75), (0x26, 72), (0x27, 77), (0x28, 80), (0x2d, 82), (0x2e, 83), (0x24, 71), (0x23, 79), (0x21, 73), (0x22, 81)];
    if let Some(&(_, s)) = T.iter().find(|(v, _)| *v == vk) {
        return s;
    }
    const ROW: &str = "1234567890";
    if let Some(i) = ROW.find(char::from_u32(vk).unwrap_or('\0')) {
        return 2 + i as u32;
    }
    const L: &str = "QWERTYUIOPASDFGHJKLZXCVBNM";
    const S: [u32; 26] = [16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 30, 31, 32, 33, 34, 35, 36, 37, 38, 44, 45, 46, 47, 48, 49, 50];
    if let Some(i) = L.find(char::from_u32(vk).unwrap_or('\0')) {
        return S[i];
    }
    if (0x70..0x7c).contains(&vk) {
        return 59 + vk - 0x70;
    }
    0
}
fn GetKeyNameTextA(c: &mut Ctx) -> u32 {
    let s = format!("Key{}", (c.a(0) >> 16) & 0xff);
    ret_str(c.a(1), c.a(2), &s, false)
}
fn GetKeyNameTextW(c: &mut Ctx) -> u32 {
    let s = format!("Key{}", (c.a(0) >> 16) & 0xff);
    ret_str(c.a(1), c.a(2), &s, true)
}
fn GetDoubleClickTime(_: &mut Ctx) -> u32 {
    500
}
fn MessageBeep(_: &mut Ctx) -> u32 {
    1
}
fn message_box(c: &mut Ctx, wide: bool) -> u32 {
    let t = arg_str(c.a(1), wide);
    let cap = arg_str(c.a(2), wide);
    eprintln!("sake: MessageBox [{cap}] {t}");
    // the answer that goes on: OK, Ignore, Yes, Cancel (Retry would ask again), Continue
    match c.a(3) & 0xf {
        2 => 5,
        3 | 4 => 6,
        5 => 2,
        6 => 11,
        _ => 1,
    }
}
fn MessageBoxA(c: &mut Ctx) -> u32 {
    message_box(c, false)
}
fn MessageBoxW(c: &mut Ctx) -> u32 {
    message_box(c, true)
}
fn MessageBoxExA(c: &mut Ctx) -> u32 {
    message_box(c, false)
}
fn MessageBoxIndirectA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    eprintln!("sake: MessageBox [{}] {}", arg_str(r32(p + 12), false), arg_str(r32(p + 8), false));
    1
}
/// CBT hooks (MFC's CWnd::CreateEx attaches the window and subclasses it in HCBT_CREATEWND):
/// (handle, procedure, thread). Other hooks are accepted and never called.
static CBT_HOOKS: Mutex<Vec<(u32, u32, u32)>> = Mutex::new(Vec::new());
static NEXT_HOOK: AtomicU32 = AtomicU32::new(0x5000);
fn set_hook(c: &mut Ctx) -> u32 {
    let h = NEXT_HOOK.fetch_add(4, Ordering::Relaxed);
    if c.a(0) == 5 {
        let tid = if c.a(3) != 0 { c.a(3) } else { crate::cur().tid };
        CBT_HOOKS.lock().unwrap().push((h, c.a(1), tid));
    }
    h
}
fn SetWindowsHookExA(c: &mut Ctx) -> u32 {
    set_hook(c)
}
fn SetWindowsHookExW(c: &mut Ctx) -> u32 {
    set_hook(c)
}
fn UnhookWindowsHookEx(c: &mut Ctx) -> u32 {
    CBT_HOOKS.lock().unwrap().retain(|x| x.0 != c.a(0));
    1
}
/// The thread's CBT hooks with (code, wparam, lparam); true when one refuses.
fn cbt(c: &mut Ctx, code: u32, wp: u32, lp: u32) -> bool {
    let tid = crate::cur().tid;
    let procs: Vec<u32> = CBT_HOOKS.lock().unwrap().iter().filter(|x| x.2 == tid).map(|x| x.1).collect();
    procs.into_iter().rev().any(|p| c.call(p, &[code, wp, lp]) != 0)
}
fn CallNextHookEx(_: &mut Ctx) -> u32 {
    0
}
fn WinHelpA(_: &mut Ctx) -> u32 {
    1
}
fn GetSysColor(c: &mut Ctx) -> u32 {
    sys_color(c.a(0))
}
/// The system colours (COLORREF) of Windows 10's default scheme.
pub(crate) fn sys_color(i: u32) -> u32 {
    match i {
        0 => 0xc8c8c8,
        1 | 7 | 8 | 9 | 18 | 19 | 23 | 25 => 0x000000,
        2 => 0xd1b499,
        3 => 0xdbcdbf,
        4 | 15 | 30 => 0xf0f0f0,
        5 | 14 | 20 => 0xffffff,
        6 => 0x646464,
        10 => 0xb4b4b4,
        11 => 0xfcf7f4,
        12 => 0xababab,
        13 => 0xd47800,
        16 => 0xa0a0a0,
        17 => 0x6d6d6d,
        21 => 0x696969,
        22 => 0xe3e3e3,
        24 => 0xe1ffff,
        26 => 0xcc6600,
        27 => 0xead1b9,
        28 => 0xf2e4d7,
        29 => 0xd47800,
        _ => 0,
    }
}
fn GetSysColorBrush(c: &mut Ctx) -> u32 {
    let col = GetSysColor(c);
    crate::gdi::solid_brush(col)
}
fn GetSystemMetrics(c: &mut Ctx) -> u32 {
    metric(c.a(0)) as u32
}
fn SystemParametersInfoA(c: &mut Ctx) -> u32 {
    spi(c, false)
}
fn SystemParametersInfoW(c: &mut Ctx) -> u32 {
    spi(c, true)
}
fn spi(c: &mut Ctx, wide: bool) -> u32 {
    let (a, wp, p) = (c.a(0), c.a(1), c.a(2));
    let _ = wp;
    match a {
        0x30 => {
            let (w, h) = screen();
            Rect { l: 0, t: 0, r: w, b: h }.write(p);
        }
        0x29 => {
            // NONCLIENTMETRICS: sizes and fonts. cbSize must be the pre-Vista size (A 340,
            // W 500) or the one with iPaddedBorderWidth (+4); else FALSE and no error set
            let lf = if wide { 92 } else { 60 };
            let old = 40 + 5 * lf;
            let size = r32(p);
            if size != old && size != old + 4 {
                return 0;
            }
            mem::fill(p + 4, size - 4, 0);
            // borders, scroll bars, caption buttons; small caption; menu (as GetSystemMetrics'
            // SM_CXVSCROLL.., SM_CXSIZE/CYSIZE, SM_CXSMSIZE/CYSMSIZE, SM_CXMENUSIZE/CYMENUSIZE)
            for (k, v) in [1u32, 16, 16, 18, 19].iter().enumerate() {
                w32(p + 4 + 4 * k as u32, *v);
            }
            w32(p + 24 + lf, metric(52) as u32);
            w32(p + 28 + lf, metric(53) as u32);
            w32(p + 32 + 2 * lf, metric(54) as u32);
            w32(p + 36 + 2 * lf, metric(55) as u32);
            for o in [24, 32 + lf, 40 + 2 * lf, 40 + 3 * lf, 40 + 4 * lf] {
                crate::gdi::default_logfont(p + o, wide);
            }
        }
        0x1f => crate::gdi::default_logfont(p, wide),
        0x26 | 0x10 | 0x0e | 0x16 | 0x0a | 0x68 | 0x1006 | 0x100a | 0x2000 | 0x2002 | 0x1002 | 0x1012 => {
            if p != 0 {
                w32(p, 0);
            }
        }
        _ => {
            if p != 0 && wp == 0 {
                w32(p, 0);
            }
        }
    }
    1
}
fn ChangeDisplaySettingsA(c: &mut Ctx) -> u32 {
    let (p, flags) = (c.a(0), c.a(1));
    change_display(c, p, 40, 108, flags)
}
fn ChangeDisplaySettingsW(c: &mut Ctx) -> u32 {
    let (p, flags) = (c.a(0), c.a(1));
    change_display(c, p, 72, 172, flags)
}
/// ChangeDisplaySettingsEx{A,W}(device, mode, hwnd, flags, param): the primary display's
fn ChangeDisplaySettingsExA(c: &mut Ctx) -> u32 {
    let (p, flags) = (c.a(1), c.a(3));
    change_display(c, p, 40, 108, flags)
}
fn ChangeDisplaySettingsExW(c: &mut Ctx) -> u32 {
    let (p, flags) = (c.a(1), c.a(3));
    change_display(c, p, 72, 172, flags)
}
/// The ChangeDisplaySettings answer for DEVMODE `p` (its dmFields at `fields`, dmPelsWidth and
/// dmPelsHeight at `size`): a size no display has, DISP_CHANGE_BADMODE (-2); CDS_TEST (2) only
/// answers, the screen as it was (winref dispmode; LiveMaker tests every mode it lists, #81)
fn change_display(c: &mut Ctx, p: u32, fields: u32, size: u32, flags: u32) -> u32 {
    if p == 0 || r32(p + fields) & 0x180000 != 0x180000 {
        return 0;
    }
    let (w, h) = (r32(p + size), r32(p + size + 4));
    match display_change(w, h, flags) {
        Some((w, h)) => {
            set_display(c, w, h);
            0
        }
        None if mode_exists(w, h) => 0,
        None => (-2i32) as u32,
    }
}
/// A size the display can take (kura's screen: 1..=8192 each way)
fn mode_exists(w: u32, h: u32) -> bool {
    (1..=8192).contains(&w) && (1..=8192).contains(&h)
}
/// The screen size a ChangeDisplaySettings of `w` x `h` with `flags` switches to (None: none)
fn display_change(w: u32, h: u32, flags: u32) -> Option<(i32, i32)> {
    (mode_exists(w, h) && flags & 2 == 0).then_some((w as i32, h as i32))
}
#[cfg(test)]
mod display_change_tests {
    use super::{display_change, mode_exists};
    /// winref dispmode's answers: CDS_TEST never switches, a size no display has is refused
    #[test]
    fn as_windows() {
        assert_eq!(display_change(640, 480, 2), None);
        assert!(mode_exists(640, 480));
        assert_eq!(display_change(640, 480, 0), Some((640, 480)));
        assert_eq!(display_change(1280, 720, 4), Some((1280, 720)));
        assert_eq!(display_change(12345, 77, 0), None);
        assert!(!mode_exists(12345, 77));
        assert!(!mode_exists(0, 480));
    }
    /// every flag combination with CDS_TEST, any size: never a switch
    #[test]
    fn test_never_switches() {
        let mut x = 0x9e37_79b9u32;
        for _ in 0..100_000 {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let (w, h, f) = (x % 9000, x.rotate_left(11) % 9000, x.rotate_left(22) | 2);
            assert_eq!(display_change(w, h, f), None);
            assert_eq!(display_change(w, h, f & !2).is_some(), mode_exists(w, h));
        }
    }
}

/// A new screen size; top-level windows get WM_DISPLAYCHANGE (CatSystem2 goes on from there)
fn set_display(c: &mut Ctx, w: i32, h: i32) {
    kura::display::vnhost_display_set_size(w, h);
    if with(|s| std::mem::replace(&mut s.screen, (w, h))) != (w, h) {
        let f = crate::api::export("user32", "SendMessageA");
        c.call(f, &[0xffff, 0x7e, 32, (h as u32) << 16 | (w as u32 & 0xffff)]);
    }
}
/// DEVMODE of mode `n` (ENUM_CURRENT_SETTINGS/ENUM_REGISTRY_SETTINGS: the screen's). The W
/// layout has a 64-byte device name and a 64-byte form name: fields before dmFormName are 32
/// bytes later, those after it 64. Windows writes the Windows 95 DEVMODE (124 bytes, W 188)
/// whatever dmSize says, and sets dmSize to that (tests/winref/probes/sizes.c): RealLive passes
/// that short one on its stack, and the rest is its return address (e170d29).
fn display_mode(n: u32, p: u32, wide: bool) -> u32 {
    let modes = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 960)];
    let (w, h) = if n == 0xffff_ffff || n == 0xffff_fffe { screen() } else if (n as usize) < modes.len() { modes[n as usize] } else { return 0 };
    let (d, e) = if wide { (32, 64) } else { (0, 0) };
    let size = 124 + e;
    mem::fill(p, size, 0);
    if wide {
        mem::put_wstr(p, 32, &"sake".encode_utf16().collect::<Vec<_>>());
    } else {
        mem::put_cstr(p, 32, b"sake");
    }
    w16(p + 36 + d, size as u16);
    w32(p + 40 + d, 0x5c0000);
    w32(p + 104 + e, 32);
    w32(p + 108 + e, w as u32);
    w32(p + 112 + e, h as u32);
    w32(p + 120 + e, 60);
    1
}
fn EnumDisplaySettingsA(c: &mut Ctx) -> u32 {
    display_mode(c.a(1), c.a(2), false)
}
fn EnumDisplaySettingsW(c: &mut Ctx) -> u32 {
    display_mode(c.a(1), c.a(2), true)
}
fn EnumDisplaySettingsExA(c: &mut Ctx) -> u32 {
    display_mode(c.a(1), c.a(2), false)
}
fn EnumDisplaySettingsExW(c: &mut Ctx) -> u32 {
    display_mode(c.a(1), c.a(2), true)
}
/// One display adapter (and one monitor on it, when `device` names the adapter). Only the
/// first `cb` bytes are written: programs pass shorter (older) DISPLAY_DEVICE layouts.
fn display_device(device: u32, n: u32, p: u32, wide: bool) -> u32 {
    if p == 0 {
        return 0;
    }
    if n != 0 {
        // (past the last device: the caller's bytes after cb are cleared, as on Windows)
        let cb = r32(p);
        if cb > 4 {
            mem::fill(p + 4, cb - 4, 0);
        }
        return 0;
    }
    let (name, desc) = if device == 0 { ("\\\\.\\DISPLAY1", "vnvm display") } else { ("\\\\.\\DISPLAY1\\Monitor0", "Generic PnP Monitor") };
    let u = if wide { 2 } else { 1 };
    let mut b = vec![0u8; ((32 + 128 + 128 + 128) * u + 4) as usize];
    let put = |b: &mut [u8], at: usize, s: &str, cap: usize| {
        if wide {
            for (i, ch) in s.encode_utf16().take(cap - 1).enumerate() {
                b[at + 2 * i..at + 2 * i + 2].copy_from_slice(&ch.to_le_bytes());
            }
        } else {
            let s = &s.as_bytes()[..s.len().min(cap - 1)];
            b[at..at + s.len()].copy_from_slice(s);
        }
    };
    put(&mut b, 0, name, 32);
    put(&mut b, (32 * u) as usize, desc, 128);
    let flags_at = (160 * u) as usize;
    b[flags_at..flags_at + 4].copy_from_slice(&(if device == 0 { 5u32 } else { 3 }).to_le_bytes()); // attached (+ primary) / active
    let cb = (r32(p) as usize).clamp(4, b.len() + 4);
    mem::write(p + 4, &b[..cb - 4]);
    1
}
fn EnumDisplayDevicesA(c: &mut Ctx) -> u32 {
    display_device(c.a(0), c.a(1), c.a(2), false)
}
fn EnumDisplayDevicesW(c: &mut Ctx) -> u32 {
    display_device(c.a(0), c.a(1), c.a(2), true)
}
/// The display configuration (Windows 7+): one active path, display 1's source (mode 0) on its
/// monitor (mode 1), at the screen's size and 60 Hz.
const DISPLAY_LUID: u32 = 0x1c82;
fn GetDisplayConfigBufferSizes(c: &mut Ctx) -> u32 {
    let (flags, paths, modes) = (c.a(0), c.a(1), c.a(2));
    if paths == 0 || modes == 0 {
        return 87;
    }
    if flags & 7 == 0 {
        return 87; // QDC_ALL_PATHS / QDC_ONLY_ACTIVE_PATHS / QDC_DATABASE_CURRENT
    }
    w32(paths, 1);
    w32(modes, 2);
    0
}
fn QueryDisplayConfig(c: &mut Ctx) -> u32 {
    let (flags, np, paths, nm, modes, topology) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if np == 0 || nm == 0 || paths == 0 || modes == 0 || flags & 7 == 0 {
        return 87;
    }
    // (only QDC_DATABASE_CURRENT takes a topology id, and wants one)
    if (flags & 4 != 0) != (topology != 0) {
        return 87;
    }
    if r32(np) < 1 || r32(nm) < 2 {
        return 122; // ERROR_INSUFFICIENT_BUFFER
    }
    let (w, h) = screen();
    let mut path = [0u32; 18];
    // source: adapter, id 0, mode 0, in use
    path[0] = DISPLAY_LUID;
    path[4] = 1;
    // target: adapter, id 0, mode 1, HDMI, no rotation or scaling, 60/1 Hz, progressive,
    // available, in use; the path is active
    path[5] = DISPLAY_LUID;
    path[7] = 0;
    path[8] = 1;
    path[9] = 5;
    path[10] = 1;
    path[11] = 1;
    path[12] = 60;
    path[13] = 1;
    path[14] = 1;
    path[15] = 1;
    path[16] = 1;
    path[17] = 1;
    let mut m = [0u32; 32];
    // source mode: width, height, 32 bpp, at (0, 0)
    m[0] = 1;
    m[2] = DISPLAY_LUID;
    m[4] = w as u32;
    m[5] = h as u32;
    m[6] = 4;
    // target mode: the video signal (pixel rate, h/v sync, active and total size, standard, progressive)
    let (tw, th) = (w as u32 + 160, h as u32 + 45);
    let rate = tw as u64 * th as u64 * 60;
    m[16] = 2;
    m[18] = DISPLAY_LUID;
    m[20] = rate as u32;
    m[21] = (rate >> 32) as u32;
    m[22] = (rate / tw as u64) as u32;
    m[23] = 1;
    m[24] = 60;
    m[25] = 1;
    m[26] = w as u32;
    m[27] = h as u32;
    m[28] = tw;
    m[29] = th;
    m[30] = 255; // D3DKMDT_VSS_OTHER
    m[31] = 1;
    let b = |v: &[u32]| v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<u8>>();
    mem::write(paths, &b(&path));
    mem::write(modes, &b(&m));
    w32(np, 1);
    w32(nm, 2);
    if topology != 0 {
        w32(topology, 1); // DISPLAYCONFIG_TOPOLOGY_INTERNAL
    }
    0
}
/// DisplayConfigGetDeviceInfo(header): the names of display 1's source and monitor
fn DisplayConfigGetDeviceInfo(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return 87;
    }
    let (ty, size) = (r32(p), r32(p + 4));
    let wide = |at: u32, cap: u32, s: &str| {
        mem::put_wstr(at, cap, &s.encode_utf16().collect::<Vec<_>>());
    };
    match ty {
        1 if size >= 84 => wide(p + 20, 32, "\\\\.\\DISPLAY1"), // DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME
        2 if size >= 420 => {
            // ..._GET_TARGET_NAME: flags (friendly name from EDID), HDMI, EDID ids, connector 1
            w32(p + 20, 1);
            w32(p + 24, 5);
            w32(p + 28, 0x1c82_10ac);
            w32(p + 32, 1);
            wide(p + 36, 64, "Generic PnP Monitor");
            wide(p + 164, 128, "\\\\?\\DISPLAY#VNV0001#1&0&UID0#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}");
        }
        3 if size >= 32 => {
            // ..._GET_TARGET_PREFERRED_MODE: the screen's size
            let (w, h) = screen();
            w32(p + 20, w as u32);
            w32(p + 24, h as u32);
        }
        4 if size >= 276 => wide(p + 20, 128, "\\\\?\\PCI#VEN_10DE&DEV_1C82#0#{5b45201d-f2f2-4f3b-85bb-30ff1f953599}"), // ..._GET_ADAPTER_NAME
        1..=4 => return 87,
        _ => return 50, // ERROR_NOT_SUPPORTED
    }
    0
}
fn MonitorFromWindow(_: &mut Ctx) -> u32 {
    0x10001
}
fn MonitorFromPoint(_: &mut Ctx) -> u32 {
    0x10001
}
fn MonitorFromRect(_: &mut Ctx) -> u32 {
    0x10001
}
/// GetMonitorInfo(monitor, MONITORINFO(EX)*): cbSize must be MONITORINFO's 40 or MONITORINFOEX's
/// (A 72, W 104), else ERROR_INVALID_PARAMETER with nothing written
fn monitor_info(p: u32, wide: bool) -> u32 {
    let ex = if wide { 104 } else { 72 };
    let size = r32(p);
    if size != 40 && size != ex {
        crate::k32::last_error(87);
        return 0;
    }
    let (w, h) = screen();
    let r = Rect { l: 0, t: 0, r: w, b: h };
    r.write(p + 4);
    r.write(p + 20);
    w32(p + 36, 1);
    if size == ex {
        if wide {
            mem::put_wstr(p + 40, 32, &"\\\\.\\DISPLAY1".encode_utf16().collect::<Vec<_>>());
        } else {
            mem::put_cstr(p + 40, 32, b"\\\\.\\DISPLAY1");
        }
    }
    1
}
fn GetMonitorInfoA(c: &mut Ctx) -> u32 {
    monitor_info(c.a(1), false)
}
fn GetMonitorInfoW(c: &mut Ctx) -> u32 {
    monitor_info(c.a(1), true)
}
fn EnumDisplayMonitors(c: &mut Ctx) -> u32 {
    let (f, lp) = (c.a(2), c.a(3));
    let save = c.cpu.regs[koji::ESP];
    let r = (save - 0x20) & !15;
    let (w, h) = screen();
    Rect { l: 0, t: 0, r: w, b: h }.write(r);
    c.cpu.regs[koji::ESP] = r - 16;
    c.call(f, &[0x10001, 0, r, lp]);
    c.cpu.regs[koji::ESP] = save;
    1
}

// rectangles
fn SetRect(c: &mut Ctx) -> u32 {
    Rect { l: c.i(1), t: c.i(2), r: c.i(3), b: c.i(4) }.write(c.a(0));
    1
}
fn SetRectEmpty(c: &mut Ctx) -> u32 {
    Rect::default().write(c.a(0));
    1
}
fn CopyRect(c: &mut Ctx) -> u32 {
    Rect::read(c.a(1)).write(c.a(0));
    1
}
fn IsRectEmpty(c: &mut Ctx) -> u32 {
    Rect::read(c.a(0)).empty() as u32
}
fn EqualRect(c: &mut Ctx) -> u32 {
    (Rect::read(c.a(0)) == Rect::read(c.a(1))) as u32
}
fn PtInRect(c: &mut Ctx) -> u32 {
    let (x, y) = c.pt(1);
    Rect::read(c.a(0)).has(x, y) as u32
}
fn OffsetRect(c: &mut Ctx) -> u32 {
    Rect::read(c.a(0)).off(c.i(1), c.i(2)).write(c.a(0));
    1
}
fn InflateRect(c: &mut Ctx) -> u32 {
    let mut r = Rect::read(c.a(0));
    r.l -= c.i(1);
    r.r += c.i(1);
    r.t -= c.i(2);
    r.b += c.i(2);
    r.write(c.a(0));
    1
}
fn IntersectRect(c: &mut Ctx) -> u32 {
    let r = Rect::read(c.a(1)).and(&Rect::read(c.a(2)));
    let r = if r.empty() { Rect::default() } else { r };
    r.write(c.a(0));
    (!r.empty()) as u32
}
fn UnionRect(c: &mut Ctx) -> u32 {
    let r = Rect::read(c.a(1)).or(&Rect::read(c.a(2)));
    let r = if r.empty() { Rect::default() } else { r };
    r.write(c.a(0));
    (!r.empty()) as u32
}
/// SubtractRect: src1 less src2 when src2 covers src1 entirely or one whole side of it (the rest
/// would not be a rectangle: src1 unchanged).
fn SubtractRect(c: &mut Ctx) -> u32 {
    let (a, b) = (Rect::read(c.a(1)), Rect::read(c.a(2)));
    let mut d = a;
    let x = a.and(&b);
    if a.empty() || !x.empty() && x.l == a.l && x.t == a.t && x.r == a.r && x.b == a.b {
        Rect::default().write(c.a(0));
        return 0;
    }
    if !x.empty() {
        if x.t == a.t && x.b == a.b {
            if x.l == a.l {
                d.l = x.r;
            } else if x.r == a.r {
                d.r = x.l;
            }
        } else if x.l == a.l && x.r == a.r {
            if x.t == a.t {
                d.t = x.b;
            } else if x.b == a.b {
                d.b = x.t;
            }
        }
    }
    d.write(c.a(0));
    1
}
fn FillRect(c: &mut Ctx) -> u32 {
    crate::gdi::fill_rect_brush(c.a(0), Rect::read(c.a(1)), c.a(2));
    1
}
fn FrameRect(c: &mut Ctx) -> u32 {
    let r = Rect::read(c.a(1));
    let (dc, b) = (c.a(0), c.a(2));
    if r.empty() {
        return 0;
    }
    for e in [Rect { b: r.t + 1, ..r }, Rect { t: r.b - 1, ..r }, Rect { r: r.l + 1, ..r }, Rect { l: r.r - 1, ..r }] {
        crate::gdi::fill_rect_brush(dc, e, b);
    }
    1
}
fn InvertRect(c: &mut Ctx) -> u32 {
    crate::gdi::pat_rect(c.a(0), Rect::read(c.a(1)), None, 0x55);
    1
}
fn DrawFocusRect(_: &mut Ctx) -> u32 {
    1
}
fn DrawEdge(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return 0;
    }
    let mut r = Rect::read(p);
    let (edge, flags) = (c.a(2), c.a(3));
    let ok = draw_edge(c.a(0), &mut r, edge, flags);
    if flags & 0x2000 != 0 && flags & 0x10 == 0 {
        r.write(p); // BF_ADJUST
    }
    ok as u32
}

/// DrawEdge's rectangle form (Windows' 3D edges: sunken/raised outer and inner lines in the
/// system colours, BF_SOFT / BF_FLAT / BF_MONO, BF_MIDDLE, BF_ADJUST). A diagonal (BF_DIAGONAL)
/// draws nothing yet: the 2-pixel corners of tabs and the like.
pub(crate) fn draw_edge(dc: u32, rc: &mut Rect, edge: u32, flags: u32) -> bool {
    // index: edge & 0xf (BDR_RAISEDOUTER 1, SUNKENOUTER 2, RAISEDINNER 4, SUNKENINNER 8) -> COLOR_*
    const N: i8 = -1;
    const LT_INNER_NORMAL: [i8; 16] = [N, N, N, N, N, 20, 20, N, N, 21, 21, N, N, N, N, N];
    const LT_OUTER_NORMAL: [i8; 16] = [N, 22, 16, N, 20, 22, 16, N, 21, 22, 16, N, N, 22, 16, N];
    const RB_INNER_NORMAL: [i8; 16] = [N, N, N, N, N, 16, 16, N, N, 22, 22, N, N, N, N, N];
    const RB_OUTER_NORMAL: [i8; 16] = [N, 21, 20, N, 16, 21, 20, N, 22, 21, 20, N, N, 21, 20, N];
    const LT_INNER_SOFT: [i8; 16] = [N, N, N, N, N, 22, 22, N, N, 16, 16, N, N, N, N, N];
    const LT_OUTER_SOFT: [i8; 16] = [N, 20, 21, N, 22, 20, 21, N, 16, 20, 21, N, N, 20, 21, N];
    const OUTER_MONO: [i8; 16] = [N, 6, 6, 6, 5, 6, 6, 6, 5, 6, 6, 6, 5, 6, 6, 6];
    const INNER_MONO: [i8; 16] = [N, N, N, N, N, 5, 5, 5, N, 5, 5, 5, N, 5, 5, 5];
    const OUTER_FLAT: [i8; 16] = [N, 16, 16, 16, 15, 16, 16, 16, 15, 16, 16, 16, 15, 16, 16, 16];
    const INNER_FLAT: [i8; 16] = [N, N, N, N, N, 15, 15, 15, N, 15, 15, 15, N, 15, 15, 15];
    let (left, top, right, bottom) = (flags & 1 != 0, flags & 2 != 0, flags & 4 != 0, flags & 8 != 0);
    let e = (edge & 0xf) as usize;
    let (inner, outer) = (edge & 0xc, edge & 3);
    let (soft, flat, mono) = (flags & 0x1000 != 0, flags & 0x4000 != 0, flags & 0x8000 != 0);
    let ok = !((inner == 0xc || outer == 3) && !(flat || mono));
    if flags & 0x10 != 0 {
        return ok; // BF_DIAGONAL
    }
    let (lti, lto, rbi, rbo) = if mono {
        (INNER_MONO[e], OUTER_MONO[e], INNER_MONO[e], OUTER_MONO[e])
    } else if flat {
        let i = if INNER_FLAT[e] != N { 15 } else { N };
        (i, OUTER_FLAT[e], i, OUTER_FLAT[e])
    } else if soft {
        (LT_INNER_SOFT[e], LT_OUTER_SOFT[e], RB_INNER_NORMAL[e], RB_OUTER_NORMAL[e])
    } else {
        (LT_INNER_NORMAL[e], LT_OUTER_NORMAL[e], RB_INNER_NORMAL[e], RB_OUTER_NORMAL[e])
    };
    let fill = |r: Rect, color: i8| {
        if color != N {
            crate::gdi::fill_rect_brush(dc, r, crate::gdi::solid_brush(sys_color(color as u32)));
        }
    };
    let r = *rc;
    let off = |a: bool, b: bool| if a && b { 1 } else { 0 };
    let (lbi, rti, rbi_o, lti_o) = (off(bottom, left), off(top, right), off(bottom, right), off(top, left));
    if top {
        fill(Rect { b: r.t + 1, ..r }, lto);
    }
    if left {
        fill(Rect { r: r.l + 1, ..r }, lto);
    }
    if bottom {
        fill(Rect { t: r.b - 1, ..r }, rbo);
    }
    if right {
        fill(Rect { l: r.r - 1, ..r }, rbo);
    }
    if top {
        fill(Rect { l: r.l + lti_o, t: r.t + 1, r: r.r - rti, b: r.t + 2 }, lti);
    }
    if left {
        fill(Rect { l: r.l + 1, t: r.t + lti_o, r: r.l + 2, b: r.b - lbi }, lti);
    }
    if bottom {
        fill(Rect { l: r.l + lbi, t: r.b - 2, r: r.r - rbi_o, b: r.b - 1 }, rbi);
    }
    if right {
        fill(Rect { l: r.r - 2, t: r.t + rti, r: r.r - 1, b: r.b - rbi_o }, rbi);
    }
    let (middle, adjust) = (flags & 0x800 != 0, flags & 0x2000 != 0);
    if (middle && ok) || adjust {
        let add = (INNER_MONO[e] != N) as i32 + (OUTER_MONO[e] != N) as i32;
        let mut ir = r;
        if left {
            ir.l += add;
        }
        if right {
            ir.r -= add;
        }
        if top {
            ir.t += add;
        }
        if bottom {
            ir.b -= add;
        }
        if middle && ok {
            fill(ir, if mono { 5 } else { 15 });
        }
        if adjust {
            *rc = ir;
        }
    }
    ok
}
fn DrawFrameControl(_: &mut Ctx) -> u32 {
    1
}
fn DrawIcon(_: &mut Ctx) -> u32 {
    1
}
fn DrawIconEx(_: &mut Ctx) -> u32 {
    1
}
fn DrawTextA(c: &mut Ctx) -> u32 {
    let (dc, s, n, r, f) = (c.a(0), c.a(1), c.i(2), c.a(3), c.a(4));
    let t = if n < 0 { cstr(s) } else { mem::cstrn(s, n as u32) };
    crate::gdi::draw_text(dc, &a2w(&t), r, f)
}
fn DrawTextW(c: &mut Ctx) -> u32 {
    let (dc, s, n, r, f) = (c.a(0), c.a(1), c.i(2), c.a(3), c.a(4));
    let t: Vec<u16> = if n < 0 { wstr(s) } else { (0..n as u32).map(|i| r16(s + 2 * i)).collect() };
    crate::gdi::draw_text(dc, &t, r, f)
}
fn DrawTextExW(c: &mut Ctx) -> u32 {
    DrawTextW(c)
}
fn GrayStringA(_: &mut Ctx) -> u32 {
    1
}

// carets, scroll bars, clipboard: accepted, no effect
fn CreateCaret(_: &mut Ctx) -> u32 {
    1
}
fn DestroyCaret(_: &mut Ctx) -> u32 {
    1
}
fn SetCaretPos(_: &mut Ctx) -> u32 {
    1
}
fn ShowCaret(_: &mut Ctx) -> u32 {
    1
}
fn HideCaret(_: &mut Ctx) -> u32 {
    1
}
/// A window's scroll bar `bar` (SB_HORZ 0, SB_VERT 1, SB_CTL 2: the control's own)
fn with_scroll<R>(h: u32, bar: u32, f: impl FnOnce(&mut Scroll) -> R) -> Option<R> {
    let i = if bar == 2 { 2 } else { bar as usize };
    with(|s| s.wins.get_mut(&h).and_then(|w| w.scroll.get_mut(i)).map(f))
}
/// GetScrollInfo(hwnd, bar, SCROLLINFO*): the fields fMask asks for (cbSize is not checked,
/// tests/winref/probes/sizes.c)
fn GetScrollInfo(c: &mut Ctx) -> u32 {
    let (h, bar, p) = (c.a(0), c.a(1), c.a(2));
    let mask = r32(p + 4);
    let Some(sc) = with_scroll(h, bar, |x| *x) else {
        crate::k32::last_error(1400);
        return 0;
    };
    if mask & 1 != 0 {
        w32(p + 8, sc.min as u32);
        w32(p + 12, sc.max as u32);
    }
    if mask & 2 != 0 {
        w32(p + 16, sc.page);
    }
    if mask & 4 != 0 {
        w32(p + 20, sc.pos as u32);
    }
    if mask & 0x10 != 0 {
        w32(p + 24, sc.track as u32);
    }
    1
}
/// SetScrollInfo(hwnd, bar, SCROLLINFO*, redraw): the new position
fn SetScrollInfo(c: &mut Ctx) -> u32 {
    let (h, bar, p) = (c.a(0), c.a(1), c.a(2));
    let mask = r32(p + 4);
    with_scroll(h, bar, |x| {
        if mask & 1 != 0 {
            x.min = r32(p + 8) as i32;
            x.max = r32(p + 12) as i32;
        }
        if mask & 2 != 0 {
            x.page = r32(p + 16);
        }
        if mask & 4 != 0 {
            x.pos = r32(p + 20) as i32;
        }
        x.clamp();
        x.pos as u32
    })
    .unwrap_or(0)
}
fn GetScrollPos(c: &mut Ctx) -> u32 {
    with_scroll(c.a(0), c.a(1), |x| x.pos as u32).unwrap_or(0)
}
/// SetScrollPos(hwnd, bar, pos, redraw): the old position
fn SetScrollPos(c: &mut Ctx) -> u32 {
    let pos = c.a(2) as i32;
    with_scroll(c.a(0), c.a(1), |x| {
        let old = x.pos;
        x.pos = pos;
        x.clamp();
        old as u32
    })
    .unwrap_or(0)
}
fn GetScrollRange(c: &mut Ctx) -> u32 {
    let (lo, hi) = with_scroll(c.a(0), c.a(1), |x| (x.min, x.max)).unwrap_or((0, 0));
    put32(c.a(2), lo as u32);
    put32(c.a(3), hi as u32);
    1
}
fn SetScrollRange(c: &mut Ctx) -> u32 {
    let (lo, hi) = (c.a(2) as i32, c.a(3) as i32);
    with_scroll(c.a(0), c.a(1), |x| {
        x.min = lo;
        x.max = hi;
        x.clamp();
    })
    .is_some() as u32
}
fn ShowScrollBar(_: &mut Ctx) -> u32 {
    1
}
fn EnableScrollBar(_: &mut Ctx) -> u32 {
    1
}
fn OpenClipboard(_: &mut Ctx) -> u32 {
    1
}
fn CloseClipboard(_: &mut Ctx) -> u32 {
    1
}
fn EmptyClipboard(_: &mut Ctx) -> u32 {
    1
}
fn GetClipboardData(_: &mut Ctx) -> u32 {
    0
}
fn SetClipboardData(c: &mut Ctx) -> u32 {
    c.a(1)
}
fn IsClipboardFormatAvailable(_: &mut Ctx) -> u32 {
    0
}
fn EnumClipboardFormats(_: &mut Ctx) -> u32 {
    0
}
fn GetPriorityClipboardFormat(_: &mut Ctx) -> u32 {
    0xffff_ffff
}
fn RegisterClipboardFormatA(c: &mut Ctx) -> u32 {
    RegisterWindowMessageA(c)
}
fn RegisterClipboardFormatW(c: &mut Ctx) -> u32 {
    RegisterWindowMessageW(c)
}

// menus
static NEXT_MENU: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x30010);
fn new_menu() -> u32 {
    let h = NEXT_MENU.fetch_add(4, std::sync::atomic::Ordering::Relaxed);
    with(|s| s.menus.insert(h, Menu { items: Vec::new() }));
    h
}
fn CreateMenu(_: &mut Ctx) -> u32 {
    new_menu()
}
fn CreatePopupMenu(_: &mut Ctx) -> u32 {
    new_menu()
}
fn DestroyMenu(c: &mut Ctx) -> u32 {
    with(|s| s.menus.remove(&c.a(0)));
    1
}
/// The menu and index of `item` in menu `h`: by position, or by command in `h` and then in
/// its popups, depth first (as Windows: GetMenuState(menu bar, id in a popup) finds it).
fn locate(menus: &HashMap<u32, Menu>, h: u32, item: u32, by_pos: bool) -> Option<(u32, usize)> {
    fn walk(menus: &HashMap<u32, Menu>, h: u32, item: u32, depth: u32) -> Option<(u32, usize)> {
        let m = menus.get(&h)?;
        for (i, it) in m.items.iter().enumerate() {
            if it.sub == 0 && it.id == item {
                return Some((h, i));
            }
            if it.sub != 0 && depth < 16 {
                if let Some(r) = walk(menus, it.sub, item, depth + 1) {
                    return Some(r);
                }
            }
        }
        None
    }
    if by_pos {
        let m = menus.get(&h)?;
        return item_index(m, item, true).map(|i| (h, i));
    }
    walk(menus, h, item, 0)
}
fn item_index(m: &Menu, item: u32, by_pos: bool) -> Option<usize> {
    if by_pos {
        ((item as usize) < m.items.len()).then_some(item as usize)
    } else {
        m.items.iter().position(|i| i.id == item && i.sub == 0)
    }
}
fn insert_item(h: u32, pos: u32, by_pos: bool, it: MenuItem) -> u32 {
    with(|s| {
        let Some(m) = s.menus.get_mut(&h) else { return 0 };
        let i = if pos == 0xffff_ffff { m.items.len() } else { item_index(m, pos, by_pos).unwrap_or(m.items.len()) };
        m.items.insert(i, it);
        1
    })
}
fn menu_text(flags: u32, p: u32, wide: bool) -> Vec<u16> {
    if flags & 0x904 != 0 || p < 0x10000 {
        Vec::new()
    } else if wide {
        wstr(p)
    } else {
        a2w(&cstr(p))
    }
}
fn InsertMenuA(c: &mut Ctx) -> u32 {
    let (h, pos, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let sub = if flags & 0x10 != 0 { id } else { 0 };
    insert_item(h, pos, flags & 0x400 != 0, MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, false), sub, data: 0 })
}
fn AppendMenuA(c: &mut Ctx) -> u32 {
    let (h, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let sub = if flags & 0x10 != 0 { id } else { 0 };
    insert_item(h, 0xffff_ffff, true, MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, false), sub, data: 0 })
}
fn AppendMenuW(c: &mut Ctx) -> u32 {
    let (h, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let sub = if flags & 0x10 != 0 { id } else { 0 };
    insert_item(h, 0xffff_ffff, true, MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, true), sub, data: 0 })
}
/// MENUITEMINFO offsets: (wID, hSubMenu, dwItemData, dwTypeData, cch); handles are 8 bytes on x64.
fn mii() -> (u32, u32, u32, u32, u32) {
    if mem::x64() { (16, 24, 48, 56, 64) } else { (16, 20, 32, 36, 40) }
}
fn read_mii(p: u32, wide: bool) -> (u32, MenuItem) {
    let (o_id, o_sub, o_data, o_text, _) = mii();
    let mask = r32(p + 4);
    let mut it = MenuItem::default();
    if mask & 0x100 != 0 || mask & 1 != 0 {
        it.flags = r32(p + 8) | r32(p + 12);
    }
    if mask & 2 != 0 {
        it.id = r32(p + o_id);
    }
    if mask & 4 != 0 {
        it.sub = r32(p + o_sub);
    }
    if mask & 0x20 != 0 {
        it.data = r32(p + o_data);
    }
    if mask & 0x40 != 0 || (mask & 0x10 != 0 && r32(p + 8) == 0) {
        let t = mem::rp(p + o_text);
        if t >= 0x10000 {
            it.text = if wide { wstr(t) } else { a2w(&cstr(t)) };
        }
    }
    (mask, it)
}
fn InsertMenuItemA(c: &mut Ctx) -> u32 {
    let (h, pos, by_pos, p) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    let (_, it) = read_mii(p, false);
    insert_item(h, pos, by_pos, it)
}
fn InsertMenuItemW(c: &mut Ctx) -> u32 {
    let (h, pos, by_pos, p) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    let (_, it) = read_mii(p, true);
    insert_item(h, pos, by_pos, it)
}
fn SetMenuItemInfoA(c: &mut Ctx) -> u32 {
    set_menu_item_info(c, false)
}
fn SetMenuItemInfoW(c: &mut Ctx) -> u32 {
    set_menu_item_info(c, true)
}
fn set_menu_item_info(c: &mut Ctx, wide: bool) -> u32 {
    let (h, item, by_pos, p) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    let (mask, it) = read_mii(p, wide);
    with(|s| {
        let Some((h, i)) = locate(&s.menus, h, item, by_pos) else { return 0 };
        let x = &mut s.menus.get_mut(&h).unwrap().items[i];
        if mask & 0x101 != 0 {
            x.flags = it.flags;
        }
        if mask & 2 != 0 {
            x.id = it.id;
        }
        if mask & 4 != 0 {
            x.sub = it.sub;
        }
        if mask & 0x50 != 0 {
            x.text = it.text;
        }
        1
    })
}
fn GetMenuItemInfoA(c: &mut Ctx) -> u32 {
    get_menu_item_info(c, false)
}
fn GetMenuItemInfoW(c: &mut Ctx) -> u32 {
    get_menu_item_info(c, true)
}
fn get_menu_item_info(c: &mut Ctx, wide: bool) -> u32 {
    let (h, item, by_pos, p) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    let it = with(|s| locate(&s.menus, h, item, by_pos).map(|(m, i)| s.menus[&m].items[i].clone()));
    let Some(it) = it else { return 0 };
    let (o_id, o_sub, o_data, o_text, o_cch) = mii();
    let mask = r32(p + 4);
    if mask & 0x101 != 0 {
        w32(p + 8, it.flags & 0x6b00);
        w32(p + 12, it.flags & 0x108b);
    }
    if mask & 2 != 0 {
        w32(p + o_id, it.id);
    }
    if mask & 4 != 0 {
        mem::wp(p + o_sub, it.sub);
    }
    if mask & 0x20 != 0 {
        mem::wp(p + o_data, it.data);
    }
    if mask & 0x50 != 0 {
        let cap = r32(p + o_cch);
        let t = mem::rp(p + o_text);
        let n = if wide {
            if t != 0 && cap > 0 {
                mem::put_wstr(t, cap, &it.text);
            }
            it.text.len()
        } else {
            let a = w2a(&it.text);
            if t != 0 && cap > 0 {
                mem::put_cstr(t, cap, &a);
            }
            a.len()
        };
        w32(p + o_cch, n as u32);
    }
    1
}
fn GetMenuItemCount(c: &mut Ctx) -> u32 {
    with(|s| s.menus.get(&c.a(0)).map_or(0xffff_ffff, |m| m.items.len() as u32))
}
fn GetMenuItemID(c: &mut Ctx) -> u32 {
    with(|s| s.menus.get(&c.a(0)).and_then(|m| m.items.get(c.a(1) as usize)).map_or(0xffff_ffff, |i| if i.sub != 0 { 0xffff_ffff } else { i.id }))
}
fn GetSubMenu(c: &mut Ctx) -> u32 {
    with(|s| s.menus.get(&c.a(0)).and_then(|m| m.items.get(c.a(1) as usize)).map_or(0, |i| i.sub))
}
fn GetMenuState(c: &mut Ctx) -> u32 {
    let (h, item, flags) = (c.a(0), c.a(1), c.a(2));
    with(|s| locate(&s.menus, h, item, flags & 0x400 != 0).map(|(m, i)| s.menus[&m].items[i].flags & 0xffff)).unwrap_or(0xffff_ffff)
}
fn GetMenuStringA(c: &mut Ctx) -> u32 {
    let (h, item, buf, cap, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let t = with(|s| locate(&s.menus, h, item, flags & 0x400 != 0).map(|(m, i)| s.menus[&m].items[i].text.clone())).unwrap_or_default();
    let a = w2a(&t);
    if buf == 0 {
        return a.len() as u32;
    }
    mem::put_cstr(buf, cap, &a)
}
fn modify_flags(h: u32, item: u32, flags: u32, set: u32, clear: u32) -> u32 {
    with(|s| {
        let Some((h, i)) = locate(&s.menus, h, item, flags & 0x400 != 0) else { return 0xffff_ffff };
        let m = s.menus.get_mut(&h).unwrap();
        let old = m.items[i].flags;
        m.items[i].flags = (old & !clear) | set;
        old & (set | clear)
    })
}
fn CheckMenuItem(c: &mut Ctx) -> u32 {
    let (h, item, f) = (c.a(0), c.a(1), c.a(2));
    modify_flags(h, item, f, f & 8, 8 & !f)
}
/// CheckMenuRadioItem(menu, first, last, check, flags): checks `check`, unchecks the rest
fn CheckMenuRadioItem(c: &mut Ctx) -> u32 {
    let (h, first, last, check, f) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4) & 0x400);
    for i in first..=last.min(first + 256) {
        if i == check {
            modify_flags(h, i, f, 8, 0);
        } else {
            modify_flags(h, i, f, 0, 8);
        }
    }
    1
}
fn EnableMenuItem(c: &mut Ctx) -> u32 {
    let (h, item, f) = (c.a(0), c.a(1), c.a(2));
    modify_flags(h, item, f, f & 3, 3 & !f)
}
fn DeleteMenu(c: &mut Ctx) -> u32 {
    let (h, item, f) = (c.a(0), c.a(1), c.a(2));
    with(|s| {
        match locate(&s.menus, h, item, f & 0x400 != 0) {
            Some((h, i)) => {
                s.menus.get_mut(&h).unwrap().items.remove(i);
                1
            }
            None => 0,
        }
    })
}
fn RemoveMenu(c: &mut Ctx) -> u32 {
    DeleteMenu(c)
}
fn ModifyMenuA(c: &mut Ctx) -> u32 {
    let (h, pos, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    with(|s| {
        let Some(m) = s.menus.get_mut(&h) else { return 0 };
        let Some(i) = item_index(m, pos, flags & 0x400 != 0) else { return 0 };
        let sub = if flags & 0x10 != 0 { id } else { 0 };
        m.items[i] = MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, false), sub, data: 0 };
        1
    })
}
fn GetMenu(c: &mut Ctx) -> u32 {
    with(|s| s.wins.get(&c.a(0)).map_or(0, |w| w.menu))
}
fn SetMenu(c: &mut Ctx) -> u32 {
    let (h, m) = (c.a(0), c.a(1));
    let changed = with(|s| {
        let Some(w) = s.wins.get_mut(&h) else { return None };
        let had = w.menu != 0;
        w.menu = m;
        if had != (m != 0) {
            w.client = client_of(&w.rect, w.style, w.exstyle, m != 0);
            resize_surface(w);
            let (cw, ch) = (w.client.w(), w.client.h());
            update_display(s, h);
            return Some((cw, ch));
        }
        None
    });
    if let Some((cw, ch)) = changed {
        send(c, h, 0x5, 0, (ch as u32) << 16 | (cw as u32 & 0xffff));
    }
    1
}
fn GetSystemMenu(_: &mut Ctx) -> u32 {
    static M: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *M.get_or_init(new_menu)
}
fn DrawMenuBar(_: &mut Ctx) -> u32 {
    1
}
fn TrackPopupMenu(_: &mut Ctx) -> u32 {
    0
}
fn TrackPopupMenuEx(_: &mut Ctx) -> u32 {
    0
}
fn GetMenuItemRect(_: &mut Ctx) -> u32 {
    0
}
fn SetMenuItemBitmaps(_: &mut Ctx) -> u32 {
    1
}
fn GetMenuCheckMarkDimensions(_: &mut Ctx) -> u32 {
    0x000d_000d
}

// resources: strings, cursors, icons, bitmaps
fn load_string(c: &mut Ctx, wide: bool) -> u32 {
    let (inst, id, buf, cap) = (c.a(0), c.a(1), c.a(2), c.a(3));
    // LoadStringA with no room answers -1 before looking
    if !wide && cap == 0 {
        return 0xffff_ffff;
    }
    let inst = if inst == 0 { crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map_or(0, |m| m.base) } else { inst };
    let Some(e) = crate::pe::find_resource(inst, &crate::pe::ResId::Int(6), &crate::pe::ResId::Int(id / 16 + 1)) else {
        // no block: ERROR_RESOURCE_NAME_NOT_FOUND and an empty string
        crate::k32::last_error(1814);
        if cap > 0 {
            if wide { mem::w16(buf, 0) } else { mem::w8(buf, 0) }
        }
        return 0;
    };
    let (mut p, _) = crate::pe::resource_data(inst, e);
    for _ in 0..id % 16 {
        p += 2 + 2 * r16(p) as u32;
    }
    let n = r16(p) as u32;
    let s: Vec<u16> = (0..n).map(|i| r16(p + 2 + 2 * i)).collect();
    if wide {
        if cap == 0 {
            if n > 0 {
                mem::wp(buf, p + 2);
            }
            return n;
        }
        mem::put_wstr(buf, cap, &s)
    } else {
        // as many whole characters as fit in cap - 1 bytes (a double-byte one is not split)
        let mut a = Vec::new();
        for &ch in &s {
            let b = w2a(&[ch]);
            if a.len() + b.len() > cap as usize - 1 {
                break;
            }
            a.extend(b);
        }
        mem::put_cstr(buf, cap, &a)
    }
}
fn LoadStringA(c: &mut Ctx) -> u32 {
    load_string(c, false)
}
fn LoadStringW(c: &mut Ctx) -> u32 {
    load_string(c, true)
}
static NEXT_ICON: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x40010);
fn fake_handle() -> u32 {
    NEXT_ICON.fetch_add(4, std::sync::atomic::Ordering::Relaxed)
}
fn LoadCursorA(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn LoadCursorW(_: &mut Ctx) -> u32 {
    fake_handle()
}
/// LoadCursorFromFile(name): a cursor when the file is there (its pictures are not used)
/// Cursors' hotspots (from a .cur file's first entry; GetIconInfo gives them back)
static HOTSPOTS: Mutex<Option<HashMap<u32, (u16, u16)>>> = Mutex::new(None);
fn cursor_from_file(name: u32, wide: bool) -> u32 {
    let n = arg_str(name, wide);
    let data = if name == 0 { None } else { crate::files::resolve(&n).and_then(|h| kura::fs::read(h).ok()) };
    let Some(data) = data else {
        crate::k32::last_error(if name == 0 { 87 } else { crate::files::missing(&n) });
        return 0;
    };
    let h = fake_handle();
    if let Some(hot) = cur_hotspot(&data) {
        HOTSPOTS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, hot);
    }
    h
}
/// A .cur file's hotspot: ICONDIR (reserved, type 2, count) then the first 16-byte entry, whose
/// planes/bit-count words are the hotspot's x and y
pub fn cur_hotspot(d: &[u8]) -> Option<(u16, u16)> {
    let w = |o: usize| d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    (w(0)? == 0 && w(2)? == 2 && w(4)? > 0).then_some(())?;
    Some((w(10)?, w(12)?))
}
fn LoadCursorFromFileA(c: &mut Ctx) -> u32 {
    cursor_from_file(c.a(0), false)
}
fn LoadCursorFromFileW(c: &mut Ctx) -> u32 {
    cursor_from_file(c.a(0), true)
}
fn LoadIconA(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn LoadIconW(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn LoadImageA(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        let (inst, name) = (c.a(0), c.a(1));
        return crate::gdi::load_bitmap(inst, name, false);
    }
    fake_handle()
}
fn LoadImageW(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        let (inst, name) = (c.a(0), c.a(1));
        return crate::gdi::load_bitmap(inst, name, true);
    }
    fake_handle()
}
fn CreateIcon(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn CreateIconIndirect(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn CreateCursor(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn CopyIcon(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn DestroyCursor(_: &mut Ctx) -> u32 {
    1
}
fn DestroyIcon(_: &mut Ctx) -> u32 {
    1
}
fn GetIconInfo(c: &mut Ctx) -> u32 {
    // a 32x32 monochrome cursor: AND mask over XOR mask, no color bitmap. ICONINFO: fIcon,
    // the hotspot, then the two bitmap handles (pointer-sized and 8-aligned on x64: 32 bytes)
    let p = c.a(1);
    let (size, mask) = if mem::x64() { (32, 16) } else { (20, 12) };
    mem::fill(p, size, 0);
    if let Some((x, y)) = HOTSPOTS.lock().unwrap().get_or_insert_with(HashMap::default).get(&c.a(0)) {
        w32(p + 4, *x as u32);
        w32(p + 8, *y as u32);
    }
    mem::wp(p + mask, crate::gdi::new_bitmap(32, 64, 1));
    1
}
fn LoadBitmapA(c: &mut Ctx) -> u32 {
    let (inst, name) = (c.a(0), c.a(1));
    crate::gdi::load_bitmap(inst, name, false)
}
fn LoadBitmapW(c: &mut Ctx) -> u32 {
    let (inst, name) = (c.a(0), c.a(1));
    crate::gdi::load_bitmap(inst, name, true)
}
/// A resource's data (module 0: the exe), or None with ERROR_RESOURCE_NAME_NOT_FOUND.
fn resource(inst: u32, name: u32, ty: u32, wide: bool) -> Option<(u32, u32)> {
    let m = if inst == 0 { crate::k32::module_handle(0, false) } else { inst };
    let r = crate::pe::find_resource(m, &crate::pe::ResId::Int(ty), &crate::pe::ResId::from_arg(name, wide));
    match r {
        Some(e) => Some(crate::pe::resource_data(m, e)),
        None => {
            crate::k32::last_error(1814);
            None
        }
    }
}

// accelerator tables: (fVirt, key, cmd) entries
static ACCELS: Mutex<Option<HashMap<u32, Vec<(u8, u16, u16)>>>> = Mutex::new(None);
fn new_accel(t: Vec<(u8, u16, u16)>) -> u32 {
    let h = fake_handle();
    ACCELS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, t);
    h
}
/// LoadAccelerators: RT_ACCELERATOR entries are 8 bytes (flags, key, id, pad); 0x80 ends them
fn load_accelerators(c: &mut Ctx, wide: bool) -> u32 {
    let Some((p, size)) = resource(c.a(0), c.a(1), 9, wide) else { return 0 };
    let mut t = Vec::new();
    for i in 0..size / 8 {
        let e = p + 8 * i;
        let f = r16(e);
        t.push(((f & 0x7f) as u8, r16(e + 2), r16(e + 4)));
        if f & 0x80 != 0 {
            break;
        }
    }
    new_accel(t)
}
fn LoadAcceleratorsA(c: &mut Ctx) -> u32 {
    load_accelerators(c, false)
}
fn LoadAcceleratorsW(c: &mut Ctx) -> u32 {
    load_accelerators(c, true)
}
/// CreateAcceleratorTable(ACCEL*, n): ACCEL is fVirt (byte), key, cmd (6 bytes)
fn CreateAcceleratorTableA(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    if p == 0 || n == 0 {
        crate::k32::last_error(87);
        return 0;
    }
    new_accel((0..n).map(|i| (crate::mem::r8(p + 6 * i), r16(p + 6 * i + 2), r16(p + 6 * i + 4))).collect())
}
fn CreateAcceleratorTableW(c: &mut Ctx) -> u32 {
    CreateAcceleratorTableA(c)
}
fn DestroyAcceleratorTable(c: &mut Ctx) -> u32 {
    ACCELS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0)).is_some() as u32
}
/// CopyAcceleratorTable(h, ACCEL*, n): the entry count with a null buffer, else how many
/// were copied
fn CopyAcceleratorTableA(c: &mut Ctx) -> u32 {
    let (h, p, n) = (c.a(0), c.a(1), c.a(2));
    let Some(t) = ACCELS.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).cloned() else { return 0 };
    if p == 0 {
        return t.len() as u32;
    }
    let k = t.len().min(n as usize);
    for (i, &(f, key, cmd)) in t[..k].iter().enumerate() {
        let e = p + 6 * i as u32;
        crate::mem::w8(e, f);
        crate::mem::w8(e + 1, 0);
        w16(e + 2, key);
        w16(e + 4, cmd);
    }
    k as u32
}
fn CopyAcceleratorTableW(c: &mut Ctx) -> u32 {
    CopyAcceleratorTableA(c)
}
/// TranslateAccelerator(hwnd, haccel, msg): a key down matching an entry (virtual key with
/// its Shift/Ctrl/Alt, or the character for WM_CHAR) becomes WM_COMMAND (1, cmd)
fn TranslateAcceleratorA(c: &mut Ctx) -> u32 {
    let (hwnd, h, m) = (c.a(0), c.a(1), c.a(2));
    if m == 0 {
        return 0;
    }
    let (msg, wp) = (r32(m + 4), r32(m + 8));
    let Some(t) = ACCELS.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).cloned() else { return 0 };
    let down = |k: usize| with(|s| s.keys[k] & 0x80 != 0);
    let mods = (if down(0x10) { 4 } else { 0 }) | (if down(0x11) { 8 } else { 0 }) | (if down(0x12) { 16 } else { 0 });
    let hit = t.iter().find(|&&(f, key, _)| {
        if f & 1 != 0 {
            matches!(msg, 0x100 | 0x104) && key as u32 == wp & 0xffff && (f & 0x1c) == mods
        } else {
            matches!(msg, 0x102 | 0x106) && key as u32 == wp & 0xffff
        }
    });
    let Some(&(_, _, cmd)) = hit else { return 0 };
    send(c, hwnd, 0x111, 0x1_0000 | cmd as u32, 0);
    1
}
fn TranslateAcceleratorW(c: &mut Ctx) -> u32 {
    TranslateAcceleratorA(c)
}

/// The items of a menu template at `p` into menu `h` (MENUITEMTEMPLATE: flags, id unless a
/// popup, text; MF_END on the last of each level); returns where the level ended.
fn menu_items(h: u32, mut p: u32) -> u32 {
    loop {
        let f = r16(p) as u32;
        p += 2;
        let id = if f & 0x10 == 0 {
            p += 2;
            r16(p - 2) as u32
        } else {
            0
        };
        let text = wstr(p);
        p += 2 * text.len() as u32 + 2;
        // (an item without text or id is a separator, as RC's MENUITEM SEPARATOR writes it)
        let mut flags = f & !0x80;
        if f & 0x10 == 0 && id == 0 && text.is_empty() {
            flags |= 0x803; // (disabled and grayed, as Windows loads it)
        }
        if f & 0x10 != 0 {
            let sub = new_menu();
            p = menu_items(sub, p);
            // (a popup's id is its menu handle)
            insert_item(h, 0xffff_ffff, true, MenuItem { id: sub, flags, text, sub, data: 0 });
        } else {
            insert_item(h, 0xffff_ffff, true, MenuItem { id, flags, text, sub: 0, data: 0 });
        }
        if f & 0x80 != 0 {
            return p;
        }
    }
}
/// MENUEX items: type, state, id (dwords), flags word (1 popup, 0x80 last), text, dword aligned;
/// a popup has a help id and then its items
fn menuex_items(h: u32, mut p: u32) -> u32 {
    loop {
        let (ty, state, id, res) = (r32(p), r32(p + 4), r32(p + 8), r16(p + 12) as u32);
        let text = wstr(p + 14);
        p = (p + 14 + 2 * text.len() as u32 + 2 + 3) & !3;
        let flags = ty | state | if res & 1 != 0 { 0x10 } else { 0 };
        if res & 1 != 0 {
            let sub = new_menu();
            p = menuex_items(sub, p + 4);
            insert_item(h, 0xffff_ffff, true, MenuItem { id, flags, text, sub, data: 0 });
        } else {
            insert_item(h, 0xffff_ffff, true, MenuItem { id, flags, text, sub: 0, data: 0 });
        }
        if res & 0x80 != 0 {
            return p;
        }
    }
}
fn menu_from_template(p: u32) -> u32 {
    let h = new_menu();
    match r16(p) {
        0 => {
            menu_items(h, p + 4 + r16(p + 2) as u32);
        }
        1 => {
            menuex_items(h, p + 4 + r16(p + 2) as u32);
        }
        _ => {}
    }
    h
}
fn LoadMenuA(c: &mut Ctx) -> u32 {
    resource(c.a(0), c.a(1), 4, false).map_or(0, |(p, _)| menu_from_template(p))
}
fn LoadMenuW(c: &mut Ctx) -> u32 {
    resource(c.a(0), c.a(1), 4, true).map_or(0, |(p, _)| menu_from_template(p))
}
fn LoadMenuIndirectA(c: &mut Ctx) -> u32 {
    menu_from_template(c.a(0))
}
fn LoadMenuIndirectW(c: &mut Ctx) -> u32 {
    menu_from_template(c.a(0))
}

// strings
fn char_case(c: &mut Ctx, upper: bool) -> u32 {
    let p = c.a(0);
    if p < 0x10000 {
        let b = p as u8;
        return if upper { b.to_ascii_uppercase() } else { b.to_ascii_lowercase() } as u32;
    }
    let mut s = cstr(p);
    crate::text::case_a(&mut s, upper);
    mem::write(p, &s);
    p
}
fn CharLowerA(c: &mut Ctx) -> u32 {
    char_case(c, false)
}
fn CharUpperA(c: &mut Ctx) -> u32 {
    char_case(c, true)
}
fn char_case_w(c: &mut Ctx, upper: bool) -> u32 {
    let p = c.a(0);
    let f = |ch: u16| if upper { crate::text::upper16(ch) } else { crate::text::lower16(ch) };
    if p < 0x10000 {
        return f(p as u16) as u32;
    }
    let mut a = p;
    loop {
        let ch = r16(a);
        if ch == 0 {
            break;
        }
        w16(a, f(ch));
        a += 2;
    }
    p
}
fn CharLowerW(c: &mut Ctx) -> u32 {
    char_case_w(c, false)
}
fn CharUpperW(c: &mut Ctx) -> u32 {
    char_case_w(c, true)
}
fn CharLowerBuffA(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let s = mem::slice_mut(p, n);
    crate::text::case_a(s, false);
    n
}
fn CharUpperBuffA(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let s = mem::slice_mut(p, n);
    crate::text::case_a(s, true);
    n
}
fn char_buff_w(c: &mut Ctx, upper: bool) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    for i in 0..n {
        let ch = r16(p + 2 * i);
        w16(p + 2 * i, if upper { crate::text::upper16(ch) } else { crate::text::lower16(ch) });
    }
    n
}
fn CharLowerBuffW(c: &mut Ctx) -> u32 {
    char_buff_w(c, false)
}
fn CharUpperBuffW(c: &mut Ctx) -> u32 {
    char_buff_w(c, true)
}
fn CharNextA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let b = mem::r8(p);
    if b == 0 {
        p
    } else if crate::text::is_lead(b) && mem::r8(p + 1) != 0 {
        p + 2
    } else {
        p + 1
    }
}
/// CharNextExA(codepage, p, flags): CharNextA with the system code page (932)
fn CharNextExA(c: &mut Ctx) -> u32 {
    let (cp, p) = (c.a(0), c.a(1));
    let b = mem::r8(p);
    if b == 0 {
        p
    } else if crate::text::is_lead_cp(cp, b) && mem::r8(p + 1) != 0 {
        p + 2
    } else {
        p + 1
    }
}
fn CharNextW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if r16(p) == 0 { p } else { p + 2 }
}
fn CharPrevA(c: &mut Ctx) -> u32 {
    char_prev(0, c.a(0), c.a(1))
}
/// CharPrevExA(code page, start, current, flags)
fn CharPrevExA(c: &mut Ctx) -> u32 {
    char_prev(c.a(0) & 0xffff, c.a(1), c.a(2))
}
fn char_prev(cp: u32, start: u32, p: u32) -> u32 {
    if p <= start {
        return start;
    }
    let mut q = start;
    loop {
        let n = if crate::text::is_lead_cp(cp, mem::r8(q)) { q + 2 } else { q + 1 };
        if n >= p {
            return q;
        }
        q = n;
    }
}
fn CharToOemA(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0));
    mem::write(c.a(1), &s);
    mem::w8(c.a(1) + s.len() as u32, 0);
    1
}
fn OemToCharA(c: &mut Ctx) -> u32 {
    CharToOemA(c)
}
fn OemToCharBuffA(c: &mut Ctx) -> u32 {
    mem::copy(c.a(1), c.a(0), c.a(2));
    1
}
fn CharToOemBuffA(c: &mut Ctx) -> u32 {
    mem::copy(c.a(1), c.a(0), c.a(2));
    1
}
fn IsCharAlphaA(c: &mut Ctx) -> u32 {
    (c.a(0) as u8).is_ascii_alphabetic() as u32
}
fn IsCharAlphaNumericA(c: &mut Ctx) -> u32 {
    (c.a(0) as u8).is_ascii_alphanumeric() as u32
}
fn IsCharUpperA(c: &mut Ctx) -> u32 {
    (c.a(0) as u8).is_ascii_uppercase() as u32
}
fn IsCharLowerA(c: &mut Ctx) -> u32 {
    (c.a(0) as u8).is_ascii_lowercase() as u32
}
fn IsCharAlphaW(c: &mut Ctx) -> u32 {
    char::from_u32(c.a(0) & 0xffff).is_some_and(|x| x.is_alphabetic()) as u32
}
/// wsprintf's own dialect over the CRT's printf: a conversion it does not know (%f, %e, %g, %n..)
/// prints its letter and takes no argument (winref strapis: "%f|%e" gives "f|e")
pub fn wsprintf_format(f: &[u16]) -> Vec<u16> {
    let mut out = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        if f[i] != b'%' as u16 {
            out.push(f[i]);
            i += 1;
            continue;
        }
        let start = i;
        i += 1;
        let is = |i: usize, set: &[u8]| f.get(i).is_some_and(|&ch| ch < 0x80 && set.contains(&(ch as u8)));
        while is(i, b"-#0 +") {
            i += 1;
        }
        while is(i, b"0123456789") {
            i += 1;
        }
        if is(i, b".") {
            i += 1;
            while is(i, b"0123456789") {
                i += 1;
            }
        }
        if f[i..].starts_with(&[b'I' as u16, b'6' as u16, b'4' as u16]) {
            i += 3;
        } else if is(i, b"lhw") {
            i += 1;
        }
        match f.get(i) {
            None => {
                out.extend_from_slice(&f[start..]);
            }
            Some(&t) if t < 0x80 && b"%cCdisSuxXp".contains(&(t as u8)) => out.extend_from_slice(&f[start..=i]),
            Some(&t) => out.push(t),
        }
        i += 1;
    }
    out
}
/// The output: 1024 characters at most, and a NUL
const WSPRINTF_MAX: usize = 1024;
fn wsprintf_a(dst: u32, fmt: u32, va: &mut crate::misc::VaArgs) -> u32 {
    let f: Vec<u8> = wsprintf_format(&cstr(fmt).iter().map(|&b| b as u16).collect::<Vec<_>>()).iter().map(|&u| u as u8).collect();
    let out = crate::misc::format(&f, va, false);
    let out = &out[..out.len().min(WSPRINTF_MAX)];
    mem::write(dst, out);
    mem::w8(dst + out.len() as u32, 0);
    out.len() as u32
}
fn wsprintf_w(dst: u32, fmt: u32, va: &mut crate::misc::VaArgs) -> u32 {
    let out = crate::misc::format_w(&wsprintf_format(&wstr(fmt)), va);
    mem::put_wstr(dst, WSPRINTF_MAX as u32 + 1, &out)
}
fn wsprintfA(c: &mut Ctx) -> u32 {
    let (dst, fmt) = (c.a(0), c.a(1));
    wsprintf_a(dst, fmt, &mut crate::misc::VaArgs::arg(c, 2))
}
fn wvsprintfA(c: &mut Ctx) -> u32 {
    let (dst, fmt, va) = (c.a(0), c.a(1), c.a(2));
    wsprintf_a(dst, fmt, &mut crate::misc::VaArgs::stack(va))
}
fn wsprintfW(c: &mut Ctx) -> u32 {
    let (dst, fmt) = (c.a(0), c.a(1));
    wsprintf_w(dst, fmt, &mut crate::misc::VaArgs::arg(c, 2))
}
fn wvsprintfW(c: &mut Ctx) -> u32 {
    let (dst, fmt, va) = (c.a(0), c.a(1), c.a(2));
    wsprintf_w(dst, fmt, &mut crate::misc::VaArgs::stack(va))
}
fn GetGUIThreadInfo(_: &mut Ctx) -> u32 {
    0
}
fn AttachThreadInput(_: &mut Ctx) -> u32 {
    1
}
fn FlashWindow(_: &mut Ctx) -> u32 {
    0
}
fn GetProcessWindowStation(_: &mut Ctx) -> u32 {
    0x50
}
fn GetUserObjectInformationA(_: &mut Ctx) -> u32 {
    0
}
fn SetProcessDPIAware(_: &mut Ctx) -> u32 {
    1
}
fn IsWindowUnicode(c: &mut Ctx) -> u32 {
    with(|s| s.wins.get(&c.a(0)).is_some_and(|w| w.unicode)) as u32
}
fn RegisterHotKey(_: &mut Ctx) -> u32 {
    1
}
fn UnregisterHotKey(_: &mut Ctx) -> u32 {
    1
}

// ---------------------------------------------------------------------------------------------
// input from the page (kura's input queue)

static INPUT_AT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// A restored save state: the input thread again, if it ran.
pub(crate) fn snap_restore() {
    if INPUT_AT.load(std::sync::atomic::Ordering::Relaxed) != 0 {
        start_input();
    }
}

fn start_input() {
    // (once per save-state epoch: a restored copy starts it again)
    if kura::snap::once(&INPUT_AT) {
        std::thread::spawn(|| {
            kura::snap::register("input", false, 0);
            let trace = crate::env_on!("SAKE_TRACE_INPUT");
            loop {
                kura::snap::idle();
                let recs = kura::display::input_take(100);
                kura::snap::busy();
                if recs.is_empty() {
                    continue;
                }
                for r in &recs {
                    input(r[0], r[1] as i32, r[2] as i32, r[3]);
                    if trace {
                        eprintln!("sake input: {r:?} queued");
                    }
                }
                crate::obj::signal();
            }
        });
    }
}

/// WM_INPUT with raw input event `h`, to `hwnd` (0: the window with the focus)
fn raw_post(s: &mut State, hwnd: u32, h: u32, t: u32) {
    let hwnd = if hwnd != 0 { hwnd } else if s.focus != 0 { s.focus } else { s.active };
    let Some(tid) = s.wins.get(&hwnd).map(|w| w.tid) else { return };
    let (x, y) = s.cursor;
    s.queues.entry(tid).or_default().input.push_back(Msg { hwnd, msg: crate::rawinput::WM_INPUT, w: 0, l: h, time: t, x, y });
}

fn input(kind: u32, x: i32, y: i32, data: u32) {
    crate::dinput::input(kind, x, y, data);
    with(|s| {
        let t = now_ms();
        match kind {
            1..=4 => {
                let (dx, dy) = (x - s.cursor.0, y - s.cursor.1);
                s.cursor = (x, y);
                if let Some((rw, nolegacy)) = crate::rawinput::registered(true) {
                    let (buttons, wheel) = match kind {
                        2 | 3 => ((if kind == 2 { 1u16 } else { 2 }) << (2 * data.min(2)), 0),
                        4 => (0x400, data as u16 as i16), // RI_MOUSE_WHEEL
                        _ => (0, 0),
                    };
                    raw_post(s, rw, crate::rawinput::mouse_event(buttons, wheel, dx, dy), t);
                    if nolegacy {
                        // (the button state still changes)
                        match (kind, data) {
                            (2, b) => s.keys[[1, 2, 4][b.min(2) as usize]] |= 0x80,
                            (3, b) => s.keys[[1, 2, 4][b.min(2) as usize]] &= 1,
                            _ => {}
                        }
                        return;
                    }
                }
                let target = if s.capture != 0 { s.capture } else { window_at(s, x, y) };
                let Some(w) = s.wins.get(&target) else { return };
                let tid = w.tid;
                let c = client_screen(s, target);
                let (cx, cy) = (x - c.l, y - c.t);
                if crate::env_on!("SAKE_TRACE_INPUT") {
                    eprintln!("sake input: {kind} at {x},{y} -> {target:x} client {cx},{cy} (client {c:?}, window {:?})", w.rect);
                    for (h, w) in s.wins.iter() {
                        eprintln!("  win {h:x} parent {:x} vis {} style {:x} ex {:x} rect {:?} active {}", w.parent, w.visible, w.style, w.exstyle, w.rect, s.active == *h);
                    }
                }
                let mut flags = 0u32;
                for (vk, bit) in [(1usize, 1u32), (2, 2), (0x10, 4), (0x11, 8), (4, 0x10)] {
                    if s.keys[vk] & 0x80 != 0 {
                        flags |= bit;
                    }
                }
                let (msg, wp) = match kind {
                    1 => (0x200, flags),
                    2 | 3 => {
                        let (vk, base) = match data {
                            0 => (1usize, 0x201u32),
                            1 => (2, 0x204),
                            _ => (4, 0x207),
                        };
                        if kind == 2 {
                            s.pressed[vk] = true;
                            s.keys[vk] = 0x80 | (s.keys[vk] ^ 1) & 1;
                        } else {
                            s.keys[vk] &= 1;
                        }
                        let mut fl = 0u32;
                        for (vk, bit) in [(1usize, 1u32), (2, 2), (0x10, 4), (0x11, 8), (4, 0x10)] {
                            if s.keys[vk] & 0x80 != 0 {
                                fl |= bit;
                            }
                        }
                        (if kind == 2 { base } else { base + 1 }, fl)
                    }
                    _ => (0x20a, flags | (data << 16)),
                };
                let lp = if msg == 0x20a { ((y as u32) << 16) | (x as u32 & 0xffff) } else { ((cy as u32) << 16) | (cx as u32 & 0xffff) };
                let q = s.queues.entry(tid).or_default();
                // coalesce moves
                if msg == 0x200 {
                    if let Some(last) = q.input.back_mut() {
                        if last.msg == 0x200 && last.hwnd == target {
                            *last = Msg { hwnd: target, msg, w: wp, l: lp, time: t, x, y };
                            return;
                        }
                    }
                    q.input.push_back(Msg { hwnd: target, msg: 0x20, w: target, l: 0x200 << 16 | 1, time: t, x, y });
                }
                q.input.push_back(Msg { hwnd: target, msg, w: wp, l: lp, time: t, x, y });
            }
            5 | 6 => {
                let vk = (data & 0xff) as usize;
                let scan = (data >> 16) & 0xff;
                if kind == 5 {
                    s.pressed[vk] = true;
                    s.keys[vk] = 0x80 | (s.keys[vk] ^ 1) & 1;
                } else {
                    s.keys[vk] &= 1;
                }
                let target = if s.focus != 0 { s.focus } else { s.active };
                let Some(w) = s.wins.get(&target) else { return };
                let tid = w.tid;
                let alt = s.keys[0x12] & 0x80 != 0;
                let msg = match (kind, alt || vk == 0x12) {
                    (5, false) => 0x100,
                    (6, false) => 0x101,
                    (5, true) => 0x104,
                    _ => 0x105,
                };
                let lp = 1 | scan << 16 | if kind == 6 { 0xc000_0000 } else { 0 } | if alt { 0x2000_0000 } else { 0 };
                if let Some((rw, nolegacy)) = crate::rawinput::registered(false) {
                    let h = crate::rawinput::key_event(scan as u16, (kind == 6) as u16, vk as u16, msg);
                    raw_post(s, rw, h, t);
                    if nolegacy {
                        return;
                    }
                }
                s.queues.entry(tid).or_default().input.push_back(Msg { hwnd: target, msg, w: vk as u32, l: lp, time: t, x: s.cursor.0, y: s.cursor.1 });
            }
            _ => {}
        }
    });
}

// ---------------------------------------------------------------------------------------------
// dialogs: windows built from DLGTEMPLATE(EX) resources, a guest DLGPROC called from our window
// procedure (DWLP_DLGPROC = window extra +4, +8 on x64: after the pointer-sized DWLP_MSGRESULT),
// and DialogBox's modal loop

/// DWLP_DLGPROC
fn dwlp_dlgproc() -> usize {
    mem::ps() as usize
}

/// Return values of ended modal dialogs (EndDialog).
static DLG_END: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);

fn dlg_wndproc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("dialog_proc", dlg_wndproc_fn, 16))
}

fn extra32(h: u32, off: usize) -> u32 {
    with(|s| s.wins.get(&h).and_then(|w| w.extra.get(off..off + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())))).unwrap_or(0)
}
fn set_extra32(h: u32, off: usize, v: u32) {
    with(|s| {
        if let Some(b) = s.wins.get_mut(&h).and_then(|w| w.extra.get_mut(off..off + 4)) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    });
}

fn dlg_wndproc_fn(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let p = extra32(h, dwlp_dlgproc());
    if p != 0 {
        let r = c.call(p, &[h, m, wp, lp]);
        if r != 0 {
            // (these return their value, the rest in DWL_MSGRESULT)
            return if matches!(m, 0x110 | 0x19 | 0x2b | 0x133..=0x138 | 0x87 | 0x56 | 0x37 | 0x39) { r } else { extra32(h, 0) };
        }
    }
    match m {
        0x14 => DefDlgProcA(c),
        0x110 => 1,
        // DM_GETDEFID: MAKELONG(id, DC_HASDEFID) of the BS_DEFPUSHBUTTON child
        0x400 => def_button(h).map_or(0, |(id, _)| 0x534b_0000 | id),
        // DM_SETDEFID(id): that button takes BS_DEFPUSHBUTTON from the old one
        0x401 => {
            let old = def_button(h);
            let new = dlg_item(h, wp);
            with(|s| {
                if let Some(w) = old.and_then(|(_, b)| s.wins.get_mut(&b)) {
                    w.style = w.style & !0xf;
                }
                if let Some(w) = s.wins.get_mut(&new).filter(|w| w.style & 0xf == 0) {
                    w.style |= 1;
                }
            });
            1
        }
        0x10 => {
            // WM_CLOSE: as the Cancel button
            let b = dlg_item(h, 2);
            send(c, h, 0x111, 2, b);
            0
        }
        _ => def_proc(c, false),
    }
}

/// The control Enter presses in a dialog: a focused default push button, else the default push button
/// (BS_DEFPUSHBUTTON), else IDOK. (id, its window)
fn default_button(dlg: u32) -> (u32, u32) {
    // (Wine's IsDialogMessage: the focused control if it is a default push button, else
    // DM_GETDEFID's, else IDOK; winref msgwait: a plain push button with the focus is not taken)
    let focused = with(|s| {
        let w = s.wins.get(&s.focus)?;
        let button = s.classes.get(w.class).is_some_and(|c| c.name.eq_ignore_ascii_case("Button"));
        (w.parent == dlg && button && w.style & 0xf == 1).then(|| (w.id & 0xffff, s.focus))
    });
    focused.or_else(|| def_button(dlg)).unwrap_or_else(|| (1, dlg_item(dlg, 1)))
}

/// The dialog's BS_DEFPUSHBUTTON child: (id, window)
fn def_button(dlg: u32) -> Option<(u32, u32)> {
    with(|s| {
        let kids = s.wins.get(&dlg)?.children.clone();
        kids.into_iter()
            .find(|k| s.wins.get(k).is_some_and(|w| s.classes.get(w.class).is_some_and(|c| c.name.eq_ignore_ascii_case("Button")) && w.style & 0xf == 1))
            .map(|k| (s.wins[&k].id & 0xffff, k))
    })
}

fn dlg_item(h: u32, id: u32) -> u32 {
    with(|s| s.wins.get(&h).and_then(|w| w.children.iter().copied().find(|ch| s.wins.get(ch).is_some_and(|x| x.id & 0xffff == id & 0xffff))).unwrap_or(0))
}

/// A string or ordinal in a template: (Some(string address) or None, ordinal) and the next field.
fn sz_or_ord(p: u32) -> (Option<u32>, u32, u32) {
    match r16(p) {
        0 => (None, 0, p + 2),
        0xffff => (None, r16(p + 2) as u32, p + 4),
        _ => {
            let mut q = p;
            while r16(q) != 0 {
                q += 2;
            }
            (Some(p), 0, q + 2)
        }
    }
}

/// Dialog units to pixels (the base units of a 9-point UI font).
fn du(x: i32, y: i32) -> (i32, i32) {
    ((x * 6 + 2) / 4, (y * 13 + 4) / 8)
}

fn create_dialog(c: &mut Ctx, inst: u32, t: u32, owner: u32, proc_: u32, param: u32, modal: bool) -> u32 {
    let ex = r16(t) == 1 && r16(t + 2) == 0xffff;
    let (style, exstyle, n, mut p) = if ex { (r32(t + 12), r32(t + 8), r16(t + 16) as u32, t + 18) } else { (r32(t), r32(t + 4), r16(t + 8) as u32, t + 10) };
    let (x, y, cx, cy) = (r16(p) as i16 as i32, r16(p + 2) as i16 as i32, r16(p + 4) as i16 as i32, r16(p + 6) as i16 as i32);
    p += 8;
    let (_, _, q) = sz_or_ord(p); // menu
    let (cls_s, cls_o, q) = sz_or_ord(q);
    let (title, _, mut q) = sz_or_ord(q);
    let mut font = 0;
    if style & 0x40 != 0 {
        let pt = r16(q) as i32;
        let (weight, italic, charset, face) = if ex { (r16(q + 2) as u32, r16(q + 4) as u32 & 0xff, r16(q + 4) as u32 >> 8, q + 6) } else { (400, 0, 1, q + 2) };
        let (_, _, next) = sz_or_ord(face);
        q = next;
        font = gcall(c, "gdi32", "CreateFontW", &[(-(pt * 96 + 36) / 72) as u32, 0, 0, 0, weight, italic, 0, 0, charset, 0, 0, 0, 0, face]);
    }
    let (w, h) = du(cx, cy);
    let (fl, ft, fr, fb) = frame(style, exstyle, false);
    let (ww, wh) = (w + fl + fr, h + ft + fb);
    let (ox, oy) = if style & 0x4000_0000 != 0 {
        du(x, y)
    } else {
        // centred on the owner (or the screen)
        let r = with(|s| s.wins.get(&owner).filter(|_| owner != 0).map(|w| w.rect)).unwrap_or_else(|| {
            let (sw, sh) = screen();
            Rect { l: 0, t: 0, r: sw, b: sh }
        });
        (r.l + (r.w() - ww) / 2, r.t + (r.h() - wh) / 2)
    };
    let cls = match (cls_s, cls_o) {
        (Some(s), _) => s,
        (None, 0) => crate::k32::atom("#32770"),
        (None, o) => o,
    };
    let dlg = gcall(c, "user32", "CreateWindowExW", &[exstyle, cls, 0, style & !0x1000_0000, ox as u32, oy as u32, ww as u32, wh as u32, owner, 0, inst, param]);
    if dlg == 0 {
        return 0;
    }
    if let Some(t) = title {
        let s = wstr(t);
        with(|st| st.wins.get_mut(&dlg).map(|w| w.text = s));
    }
    set_extra32(dlg, dwlp_dlgproc(), proc_);
    if font != 0 {
        send(c, dlg, 0x30, font, 0);
    }
    // the controls
    let mut first = 0;
    for _ in 0..n {
        q = (q + 3) & !3;
        let (st, ex2, id, mut r) = if ex { (r32(q + 8), r32(q + 4), r32(q + 20), q + 24) } else { (r32(q), r32(q + 4), r16(q + 16) as u32, q + 18) };
        let o = if ex { q + 12 } else { q + 8 };
        let (ix, iy) = du(r16(o) as i16 as i32, r16(o + 2) as i16 as i32);
        let (iw, ih) = du(r16(o + 4) as i16 as i32, r16(o + 6) as i16 as i32);
        let (cs, co, r2) = sz_or_ord(r);
        let (ts, _, r3) = sz_or_ord(r2);
        let extra = r16(r3) as u32;
        r = r3 + 2 + extra;
        q = r;
        let cls = match (cs, co) {
            (Some(s), _) => s,
            (None, o) => crate::k32::atom(match o {
                0x80 => "Button",
                0x81 => "Edit",
                0x82 => "Static",
                0x83 => "ListBox",
                0x84 => "ScrollBar",
                _ => "ComboBox",
            }),
        };
        let ctl = gcall(c, "user32", "CreateWindowExW", &[ex2, cls, 0, st | 0x4000_0000, ix as u32, iy as u32, iw as u32, ih as u32, dlg, id, inst, if extra != 0 { r3 + 2 } else { 0 }]);
        if ctl == 0 {
            continue;
        }
        if let Some(t) = ts {
            let s = wstr(t);
            with(|stt| stt.wins.get_mut(&ctl).map(|w| w.text = s));
        }
        if font != 0 {
            send(c, ctl, 0x30, font, 0);
        }
        if first == 0 && st & 0x0001_0000 != 0 && st & 0x0800_0000 == 0 {
            first = ctl;
        }
    }
    if send(c, dlg, 0x110, first, param) != 0 && first != 0 {
        set_focus(c, first);
    }
    if is_window(dlg) && (modal || style & 0x1000_0000 != 0) && !DLG_END.lock().unwrap().get_or_insert_with(HashMap::default).contains_key(&dlg) {
        show(c, dlg, 5);
    }
    dlg
}

fn dispatch_msg(c: &mut Ctx, m: &Msg) {
    if m.msg == 0x113 && m.l != 0 {
        c.call(m.l, &[m.hwnd, m.msg, m.w, now_ms()]);
        return;
    }
    if m.hwnd == 0 {
        return;
    }
    send(c, m.hwnd, m.msg, m.w, m.l);
    if m.msg == 0xf {
        with(|s| s.wins.get_mut(&m.hwnd).map(|w| w.update = Rect::default()));
    }
}

fn dialog_box(c: &mut Ctx, inst: u32, t: u32, owner: u32, proc_: u32, param: u32) -> u32 {
    // (the owner is the top-level window)
    let owner = with(|s| {
        let mut h = owner;
        while let Some(w) = s.wins.get(&h) {
            if w.parent == 0 {
                break;
            }
            h = w.parent;
        }
        h
    });
    let dlg = create_dialog(c, inst, t, owner, proc_, param, true);
    if dlg == 0 {
        return u32::MAX;
    }
    let was_enabled = with(|s| s.wins.get_mut(&owner).map(|w| std::mem::replace(&mut w.enabled, false))).unwrap_or(false);
    let r = loop {
        if let Some(r) = DLG_END.lock().unwrap().get_or_insert_with(HashMap::default).remove(&dlg) {
            break r;
        }
        if !is_window(dlg) {
            break 0;
        }
        process_sent(c.cpu);
        match next_msg(0, 0, 0, true) {
            Some(m) if m.msg == 0x12 => {
                with(|s| s.queues.entry(crate::cur().tid).or_default().quit = Some(m.w));
                break 0;
            }
            Some(m) => {
                let mine = m.hwnd == dlg || with(|s| s.wins.get(&m.hwnd).is_some_and(|w| w.parent == dlg));
                if m.msg == 0x100 && mine && (m.w == 0x0d || m.w == 0x1b) {
                    let (id, b) = if m.w == 0x0d { default_button(dlg) } else { (2, dlg_item(dlg, 2)) };
                    send(c, dlg, 0x111, id, b);
                } else {
                    dispatch_msg(c, &m);
                }
            }
            None => {
                wait_msgs(&[], false, 0xffff_ffff);
            }
        }
    };
    if was_enabled {
        with(|s| s.wins.get_mut(&owner).map(|w| w.enabled = true));
    }
    if is_window(dlg) {
        destroy(c, dlg);
    }
    if owner != 0 && is_window(owner) {
        activate(c, owner);
    }
    r
}

fn dlg_template(c: &mut Ctx, inst: u32, name: u32, wide: bool) -> Option<u32> {
    let _ = c;
    let m = if inst == 0 { crate::k32::module_handle(0, false) } else { inst };
    let e = crate::pe::find_resource(m, &crate::pe::ResId::Int(5), &crate::pe::ResId::from_arg(name, wide))?;
    Some(crate::pe::resource_data(m, e).0)
}

fn DialogBoxParamA(c: &mut Ctx) -> u32 {
    let (inst, name, owner, proc_, param) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    match dlg_template(c, inst, name, false) {
        Some(t) => dialog_box(c, inst, t, owner, proc_, param),
        None => u32::MAX,
    }
}
fn DialogBoxParamW(c: &mut Ctx) -> u32 {
    let (inst, name, owner, proc_, param) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    match dlg_template(c, inst, name, true) {
        Some(t) => dialog_box(c, inst, t, owner, proc_, param),
        None => u32::MAX,
    }
}
fn DialogBoxIndirectParamA(c: &mut Ctx) -> u32 {
    let (inst, t, owner, proc_, param) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    dialog_box(c, inst, t, owner, proc_, param)
}
fn DialogBoxIndirectParamW(c: &mut Ctx) -> u32 {
    DialogBoxIndirectParamA(c)
}
fn CreateDialogParamA(c: &mut Ctx) -> u32 {
    let (inst, name, owner, proc_, param) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    match dlg_template(c, inst, name, false) {
        Some(t) => create_dialog(c, inst, t, owner, proc_, param, false),
        None => 0,
    }
}
fn CreateDialogParamW(c: &mut Ctx) -> u32 {
    let (inst, name, owner, proc_, param) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    match dlg_template(c, inst, name, true) {
        Some(t) => create_dialog(c, inst, t, owner, proc_, param, false),
        None => 0,
    }
}
fn CreateDialogIndirectParamA(c: &mut Ctx) -> u32 {
    let (inst, t, owner, proc_, param) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    create_dialog(c, inst, t, owner, proc_, param, false)
}
fn CreateDialogIndirectParamW(c: &mut Ctx) -> u32 {
    CreateDialogIndirectParamA(c)
}
fn EndDialog(c: &mut Ctx) -> u32 {
    let (h, r) = (c.a(0), c.a(1));
    DLG_END.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, r);
    show(c, h, 0);
    1
}
fn SendDlgItemMessageW(c: &mut Ctx) -> u32 {
    as_wide(true, || SendDlgItemMessageA(c))
}
fn set_item_text(c: &mut Ctx, h: u32, id: u32, t: Vec<u16>) -> u32 {
    let ch = dlg_item(h, id);
    if ch == 0 {
        return 0;
    }
    with(|s| s.wins.get_mut(&ch).map(|w| w.text = t));
    invalidate(ch, None, true);
    let _ = c;
    1
}
fn SetDlgItemTextA(c: &mut Ctx) -> u32 {
    let (h, id, t) = (c.a(0), c.a(1), c.a(2));
    set_item_text(c, h, id, if t == 0 { Vec::new() } else { a2w(&cstr(t)) })
}
fn SetDlgItemTextW(c: &mut Ctx) -> u32 {
    let (h, id, t) = (c.a(0), c.a(1), c.a(2));
    set_item_text(c, h, id, if t == 0 { Vec::new() } else { wstr(t) })
}
fn item_text(h: u32, id: u32) -> Vec<u16> {
    let ch = dlg_item(h, id);
    with(|s| s.wins.get(&ch).map(|w| w.text.clone())).unwrap_or_default()
}
fn GetDlgItemTextA(c: &mut Ctx) -> u32 {
    let (h, id, b, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if n == 0 {
        return 0;
    }
    mem::put_cstr(b, n, &w2a(&item_text(h, id)))
}
fn GetDlgItemTextW(c: &mut Ctx) -> u32 {
    let (h, id, b, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if n == 0 {
        return 0;
    }
    mem::put_wstr(b, n, &item_text(h, id))
}
fn GetDlgItemInt(c: &mut Ctx) -> u32 {
    let (h, id, ok, signed) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let s = String::from_utf16_lossy(&item_text(h, id));
    let v = s.trim().parse::<i64>().ok().filter(|v| signed != 0 || *v >= 0);
    if ok != 0 {
        w32(ok, v.is_some() as u32);
    }
    v.unwrap_or(0) as u32
}
fn SetDlgItemInt(c: &mut Ctx) -> u32 {
    let (h, id, v, signed) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let s = if signed != 0 { (v as i32).to_string() } else { v.to_string() };
    set_item_text(c, h, id, s.encode_utf16().collect())
}
fn CheckDlgButton(c: &mut Ctx) -> u32 {
    let (h, id, v) = (c.a(0), c.a(1), c.a(2));
    let ch = dlg_item(h, id);
    if ch != 0 {
        send(c, ch, 0xf1, v, 0);
    }
    (ch != 0) as u32
}
fn IsDlgButtonChecked(c: &mut Ctx) -> u32 {
    let (h, id) = (c.a(0), c.a(1));
    let ch = dlg_item(h, id);
    if ch == 0 { 0 } else { send(c, ch, 0xf0, 0, 0) }
}
fn CheckRadioButton(c: &mut Ctx) -> u32 {
    let (h, first, last, on) = (c.a(0), c.a(1), c.a(2), c.a(3));
    for id in first..=last.min(first + 256) {
        let ch = dlg_item(h, id);
        if ch != 0 {
            send(c, ch, 0xf1, (id == on) as u32, 0);
        }
    }
    1
}
fn GetDialogBaseUnits(_: &mut Ctx) -> u32 {
    13 << 16 | 6
}
fn MapDialogRect(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let (l, t) = du(r32(p) as i32, r32(p + 4) as i32);
    let (r, b) = du(r32(p + 8) as i32, r32(p + 12) as i32);
    for (k, v) in [l, t, r, b].iter().enumerate() {
        w32(p + 4 * k as u32, *v as u32);
    }
    1
}

// ---------------------------------------------------------------------------------------------

pub fn init() {
    reg!("user32";
        RegisterClassA, RegisterClassW, RegisterClassExA, RegisterClassExW, UnregisterClassA, UnregisterClassW,
        GetClassInfoA, GetClassInfoW, GetClassInfoExA, GetClassInfoExW, CreateWindowExA, CreateWindowExW,
        DestroyWindow, ShowWindow, ShowWindowAsync, ShowOwnedPopups, UpdateWindow, IsWindow, IsWindowVisible,
        IsWindowEnabled, EnableWindow, IsIconic, IsZoomed, IsChild, SetWindowPos, MoveWindow, BeginDeferWindowPos,
        DeferWindowPos, EndDeferWindowPos, GetWindowRect, GetClientRect, GetWindowPlacement, SetWindowPlacement,
        ClientToScreen, ScreenToClient, MapWindowPoints, AdjustWindowRect, AdjustWindowRectEx, GetParent,
        GetAncestor, SetParent, GetDesktopWindow, GetShellWindow, GetWindow, GetTopWindow, GetLastActivePopup,
        GetDlgItem, GetDlgCtrlID, SendDlgItemMessageA, GetWindowThreadProcessId, EnumWindows, EnumThreadWindows,
        EnumChildWindows, FindWindowA, FindWindowW, FindWindowExA, FindWindowExW, WindowFromPoint, ChildWindowFromPoint,
        GetWindowLongA, GetWindowLongW, SetWindowLongA, SetWindowLongW, GetClassLongA, GetClassLongW,
        GetWindowLongPtrA, GetWindowLongPtrW, SetWindowLongPtrA, SetWindowLongPtrW, GetClassLongPtrA, GetClassLongPtrW,
        SetClassLongPtrA, SetClassLongPtrW,
        SetClassLongA, SetClassLongW, GetClassNameA, GetClassNameW, SetPropA, SetPropW, GetPropA, GetPropW,
        RemovePropA, RemovePropW, SetWindowTextA, SetWindowTextW, GetWindowTextA, GetWindowTextW,
        GetWindowTextLengthA, GetWindowTextLengthW, SetWindowRgn, DefWindowProcA, DefWindowProcW, DefDlgProcA,
        DefFrameProcA, DefMDIChildProcA, CallWindowProcA, CallWindowProcW,
        PostMessageA, PostMessageW, PostThreadMessageA, PostThreadMessageW, PostQuitMessage, SendMessageA,
        SendMessageW, SendMessageTimeoutA, SendNotifyMessageA, InSendMessage, ReplyMessage, GetMessageA,
        GetMessageW, PeekMessageA, PeekMessageW, WaitMessage, MsgWaitForMultipleObjects,
        MsgWaitForMultipleObjectsEx, GetQueueStatus, GetInputState, TranslateMessage, DispatchMessageA,
        DispatchMessageW, IsDialogMessageA, IsDialogMessageW, TranslateMDISysAccel, TranslateAcceleratorA, TranslateAcceleratorW, CopyAcceleratorTableA, CopyAcceleratorTableW,
        CreateAcceleratorTableA, CreateAcceleratorTableW, DestroyAcceleratorTable, LoadAcceleratorsW, LoadMenuIndirectA, LoadMenuIndirectW,
        GetMessagePos, GetMessageTime, GetMessageExtraInfo, RegisterWindowMessageA, RegisterWindowMessageW,
        SetTimer, KillTimer, InvalidateRect, InvalidateRgn, ValidateRect, ValidateRgn, RedrawWindow,
        GetUpdateRect, GetUpdateRgn, BeginPaint, EndPaint, GetDC, GetDCEx, GetWindowDC, ReleaseDC, WindowFromDC,
        ScrollWindow, ScrollWindowEx, LockWindowUpdate, SetFocus, GetFocus, SetActiveWindow, GetActiveWindow,
        GetForegroundWindow, SetForegroundWindow, BringWindowToTop, SetCapture, ReleaseCapture, GetCapture,
        SendInput, GetCursorPos, SetCursorPos, SetCursor, GetCursor, ShowCursor, ClipCursor, GetKeyState, GetAsyncKeyState,
        GetKeyboardState, SetKeyboardState, GetKeyboardType, GetKeyboardLayout, GetKeyboardLayoutList,
        ActivateKeyboardLayout, LoadKeyboardLayoutA, MapVirtualKeyA, MapVirtualKeyW, GetKeyNameTextA, GetKeyNameTextW,
        GetDoubleClickTime, MessageBeep, MessageBoxA, MessageBoxW, MessageBoxExA, MessageBoxIndirectA,
        SetWindowsHookExA, SetWindowsHookExW, UnhookWindowsHookEx, CallNextHookEx, WinHelpA, GetSysColor,
        GetSysColorBrush, GetSystemMetrics, SystemParametersInfoA, SystemParametersInfoW, ChangeDisplaySettingsA,
        ChangeDisplaySettingsW, ChangeDisplaySettingsExA, ChangeDisplaySettingsExW, EnumDisplaySettingsA, EnumDisplaySettingsW, EnumDisplaySettingsExA, EnumDisplaySettingsExW, EnumDisplayDevicesA, EnumDisplayDevicesW, GetDisplayConfigBufferSizes, QueryDisplayConfig, DisplayConfigGetDeviceInfo, MonitorFromWindow, MonitorFromPoint,
        MonitorFromRect, GetMonitorInfoA, GetMonitorInfoW, EnumDisplayMonitors,
        SetRect, SetRectEmpty, CopyRect, IsRectEmpty, EqualRect, PtInRect, OffsetRect, InflateRect, IntersectRect,
        UnionRect, SubtractRect, FillRect, FrameRect, InvertRect, DrawFocusRect, DrawEdge, DrawFrameControl,
        DrawIcon, DrawIconEx, DrawTextA, DrawTextW, DrawTextExW, GrayStringA,
        CreateCaret, DestroyCaret, SetCaretPos, ShowCaret, HideCaret, GetScrollInfo, SetScrollInfo, GetScrollPos,
        SetScrollPos, GetScrollRange, SetScrollRange, ShowScrollBar, EnableScrollBar, OpenClipboard,
        CloseClipboard, EmptyClipboard, GetClipboardData, SetClipboardData, IsClipboardFormatAvailable,
        EnumClipboardFormats, GetPriorityClipboardFormat, RegisterClipboardFormatA, RegisterClipboardFormatW,
        CreateMenu, CreatePopupMenu, DestroyMenu, InsertMenuA, AppendMenuA, AppendMenuW, InsertMenuItemA,
        InsertMenuItemW, SetMenuItemInfoA, SetMenuItemInfoW, GetMenuItemInfoA, GetMenuItemInfoW, GetMenuItemCount, GetMenuItemID, GetSubMenu,
        GetMenuState, GetMenuStringA, CheckMenuItem, CheckMenuRadioItem, EnableMenuItem, DeleteMenu, RemoveMenu, ModifyMenuA, GetMenu,
        SetMenu, GetSystemMenu, DrawMenuBar, TrackPopupMenu, TrackPopupMenuEx, GetMenuItemRect,
        SetMenuItemBitmaps, GetMenuCheckMarkDimensions,
        LoadStringA, LoadStringW, LoadCursorA, LoadCursorW, LoadCursorFromFileA, LoadCursorFromFileW, LoadIconA, LoadIconW, LoadImageA,
        LoadImageW, CreateIcon, CreateIconIndirect, CreateCursor, CopyIcon, DestroyCursor, DestroyIcon,
        GetIconInfo, LoadBitmapA, LoadBitmapW, LoadAcceleratorsA, LoadMenuA, LoadMenuW,
        CharLowerA, CharUpperA, CharLowerW, CharUpperW, CharLowerBuffA, CharUpperBuffA, CharLowerBuffW, CharUpperBuffW, CharNextA, CharNextExA, CharNextW,
        CharPrevA, CharPrevExA, CharToOemA, OemToCharA, OemToCharBuffA, CharToOemBuffA, IsCharAlphaA, IsCharAlphaNumericA,
        IsCharUpperA, IsCharLowerA, IsCharAlphaW, wsprintfA, wvsprintfA, wsprintfW, wvsprintfW, GetGUIThreadInfo,
        AttachThreadInput, FlashWindow, GetProcessWindowStation, GetUserObjectInformationA, SetProcessDPIAware,
        IsWindowUnicode, RegisterHotKey, UnregisterHotKey,
    );
    reg!("user32"; DdeInitializeA, DdeCreateStringHandleA, DdeCreateStringHandleW, DdeConnect, DdeGetLastError,
        DdeQueryStringA, DdeQueryStringW, DdeCmpStringHandles);
    implement("user32", "DdeInitializeW", DdeInitializeA);
    // per-monitor DPI: everything is 96 dpi
    implement("user32", "AdjustWindowRectExForDpi", AdjustWindowRectEx);
    for n in ["GetDpiForSystem", "GetDpiForWindow", "GetSystemDpiForProcess"] {
        implement("user32", n, |_| 96);
    }
    for n in ["SetProcessDpiAwarenessContext", "EnableNonClientDpiScaling"] {
        implement("user32", n, |_| 1);
    }
    // shcore (api-ms-win-shcore-scaling-*: Eushully's 天冥のコンキスタ)
    implement("shcore", "SetProcessDpiAwareness", |_| 0);
    implement("shcore", "GetProcessDpiAwareness", |c| {
        w32(c.a(1), 0);
        0
    });
    implement("shcore", "GetDpiForMonitor", |c| {
        w32(c.a(2), 96);
        w32(c.a(3), 96);
        0
    });
    implement("shcore", "GetScaleFactorForMonitor", |c| {
        w32(c.a(1), 100);
        0
    });
    // the AppUserModelID is the process parameters' WindowTitle with STARTF_TITLEISAPPID
    // (0x1000), as on Windows; no process references are kept (the getter finds none, E_FAIL)
    implement("shcore", "SetCurrentProcessExplicitAppUserModelID", |c| {
        let id = crate::mem::wstr(c.a(0));
        if c.a(0) == 0 || id.len() >= 128 {
            return 0x8007_0057;
        }
        let (flags, title) = app_id_params();
        let buf = crate::mem::heap_alloc(2 * id.len() as u32 + 2, true);
        for (i, &u) in id.iter().enumerate() {
            crate::mem::w16(buf + 2 * i as u32, u);
        }
        w32(flags, r32(flags) & !0x800 | 0x1000 | 0x4000);
        crate::mem::w16(title, 2 * id.len() as u16);
        crate::mem::w16(title + 2, 2 * id.len() as u16 + 2);
        crate::mem::wp(title + crate::mem::ps(), buf);
        0
    });
    implement("shcore", "GetCurrentProcessExplicitAppUserModelID", |c| {
        let (flags, title) = app_id_params();
        let (n, buf) = (crate::mem::r16(title) as u32 / 2, crate::mem::rp(title + crate::mem::ps()));
        if c.a(0) == 0 {
            return 0x8007_0057;
        }
        if r32(flags) & 0x1000 == 0 || buf == 0 {
            crate::mem::wp(c.a(0), 0);
            return 0x8000_4005;
        }
        let out = crate::mem::heap_alloc(2 * n + 2, true);
        for i in 0..n {
            crate::mem::w16(out + 2 * i, crate::mem::r16(buf + 2 * i));
        }
        crate::mem::wp(c.a(0), out);
        0
    });
    implement("shcore", "SetProcessReference", |_| 0);
    implement("shcore", "GetProcessReference", |c| {
        if c.a(0) != 0 {
            crate::mem::wp(c.a(0), 0);
        }
        0x8000_4005
    });
    for n in ["DdeNameService", "DdeFreeStringHandle", "DdeUninitialize", "DdeKeepStringHandle", "DdeDisconnect", "DdeFreeDataHandle", "DdeEnableCallback"] {
        implement("user32", n, dde_true);
    }
    reg!("user32";
        DialogBoxParamA, DialogBoxParamW, DialogBoxIndirectParamA, DialogBoxIndirectParamW, CreateDialogParamA,
        CreateDialogParamW, CreateDialogIndirectParamA, CreateDialogIndirectParamW, EndDialog, SendDlgItemMessageW,
        SetDlgItemTextA, SetDlgItemTextW, GetDlgItemTextA, GetDlgItemTextW, GetDlgItemInt, SetDlgItemInt,
        CheckDlgButton, IsDlgButtonChecked, CheckRadioButton, GetDialogBaseUnits, MapDialogRect,
    );
    let _ = a2s;
}

/// The process parameters' dwFlags and WindowTitle (UNICODE_STRING) addresses.
fn app_id_params() -> (u32, u32) {
    let peb = crate::PEB.load(std::sync::atomic::Ordering::Relaxed);
    if crate::mem::x64() {
        let pp = crate::mem::rp(peb + 0x20);
        (pp + 0xa4, pp + 0xb0)
    } else {
        let pp = r32(peb + 0x10);
        (pp + 0x68, pp + 0x70)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn rect() -> impl Strategy<Value = Rect> {
        (-20i32..20, -20i32..20, 0i32..30, 0i32..30).prop_map(|(l, t, w, h)| Rect { l, t, r: l + w, b: t + h })
    }

    #[test]
    fn release_at_staggers_other_threads() {
        let t0 = Instant::now();
        // the first one answered goes on at once, and so does the same thread again
        assert_eq!(release_at(None, 7, t0), t0);
        assert_eq!(release_at(Some((7, t0)), 7, t0), t0);
        // another thread: a head start after the one before
        assert_eq!(release_at(Some((7, t0)), 8, t0), t0 + HEAD_START);
        // (no later than needed: its message took longer than that to handle)
        let late = t0 + HEAD_START * 3;
        assert_eq!(release_at(Some((7, t0)), 8, late), late);
    }

    proptest! {
        /// However the messages of a turn come from the senders, two different threads that
        /// follow each other are let go at least HEAD_START apart, in the order answered, and
        /// none before its answer
        #[test]
        fn release_at_keeps_threads_apart(from in proptest::collection::vec(0u32..4, 1..12), took in proptest::collection::vec(0u64..10_000, 12)) {
            let t0 = Instant::now();
            let (mut now, mut last) = (t0, None::<(u32, Instant)>);
            for (i, &f) in from.iter().enumerate() {
                now += Duration::from_micros(took[i]);
                let at = release_at(last, f, now);
                prop_assert!(at >= now);
                if let Some((t, prev)) = last {
                    prop_assert!(at >= prev);
                    if t != f {
                        prop_assert!(at >= prev + HEAD_START);
                    }
                }
                last = Some((f, at));
            }
        }

        /// What validating leaves: inside the old update area, and every point not validated
        /// is still in it
        #[test]
        fn minus_keeps_what_is_left(a in rect(), o in rect(), x in -25i32..55, y in -25i32..55) {
            let m = a.minus(&o);
            if !m.empty() {
                prop_assert_eq!(m.and(&a), m);
            }
            if a.has(x, y) && !o.has(x, y) {
                prop_assert!(m.has(x, y));
            }
        }
    }

    #[test]
    fn minus_trims_edges() {
        let a = Rect { l: 0, t: 0, r: 100, b: 100 };
        assert!(a.minus(&Rect { l: -5, t: -5, r: 200, b: 200 }).empty());
        assert_eq!(a.minus(&Rect { l: 0, t: 0, r: 100, b: 30 }), Rect { l: 0, t: 30, r: 100, b: 100 });
        assert_eq!(a.minus(&Rect { l: 60, t: -1, r: 101, b: 101 }), Rect { l: 0, t: 0, r: 60, b: 100 });
        assert_eq!(a.minus(&Rect { l: 10, t: 10, r: 20, b: 20 }), a);
    }

    #[test]
    fn wsprintf_drops_unknown_conversions() {
        // (winref strapis: "%f|%e|%I64d" prints "f|e|" and the first double's bits)
        assert_eq!(wsprintf_format(&u("%f|%e|%I64d")), u("f|e|%I64d"));
        assert_eq!(wsprintf_format(&u("%-08.3ls %5lu %hc %% %ws %p")), u("%-08.3ls %5lu %hc %% %ws %p"));
        assert_eq!(wsprintf_format(&u("%5.2g%n%q")), u("gnq"));
        assert_eq!(wsprintf_format(&u("tail %")), u("tail %"));
        assert_eq!(wsprintf_format(&u("%05")), u("%05"));
    }

    #[test]
    fn cur_hotspots() {
        let mut d = vec![0u8, 0, 2, 0, 1, 0, 32, 32, 0, 0, 3, 0, 4, 0];
        d.extend([0; 8]);
        assert_eq!(cur_hotspot(&d), Some((3, 4)));
        d[2] = 1; // an icon
        assert_eq!(cur_hotspot(&d), None);
        assert_eq!(cur_hotspot(&d[..5]), None);
    }

    proptest! {
        /// Text with no '%' passes as it is; the result never has more '%' than the format
        #[test]
        fn wsprintf_format_keeps_plain_text(s in "[^%]{0,40}", f in "[%a-zA-Z0-9.#+ -]{0,40}") {
            prop_assert_eq!(wsprintf_format(&u(&s)), u(&s));
            let count = |v: &[u16]| v.iter().filter(|&&c| c == b'%' as u16).count();
            prop_assert!(count(&wsprintf_format(&u(&f))) <= count(&u(&f)));
        }
        /// Any bytes: no panic
        #[test]
        fn cur_hotspot_any(d in proptest::collection::vec(any::<u8>(), 0..32)) {
            let _ = cur_hotspot(&d);
        }
    }
}
