//! user32: windows, message queues, input, timers, menus (kept, not drawn), resources.
//!
//! All USER state is behind one lock, never held while guest code runs. Top-level windows
//! have a surface (their client area, BGRA) that GDI draws into and that goes to vnhost's
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
            vnhost::display::vnhost_window_unshare(self.shown_by);
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
        w32(p, self.hwnd);
        w32(p + 4, self.msg);
        w32(p + 8, self.w);
        w32(p + 12, self.l);
        w32(p + 16, self.time);
        w32(p + 20, self.x as u32);
        w32(p + 24, self.y as u32);
    }
}

struct Timer {
    hwnd: u32,
    id: u32,
    ms: u32,
    proc_: u32,
    due: Instant,
}

/// A SendMessage from another thread, waiting for its result.
struct Sent {
    msg: Msg,
    result: Arc<(Mutex<Option<u32>>, std::sync::Condvar)>,
}

#[derive(Default)]
struct Queue {
    posted: VecDeque<Msg>,
    input: VecDeque<Msg>,
    sent: VecDeque<Sent>,
    timers: Vec<Timer>,
    quit: Option<u32>,
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
        unsafe { vnhost::display::vnhost_display_size(&mut s, &mut h) };
        State {
            classes: Vec::new(),
            wins: HashMap::default(),
            next: 0x10020,
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
        4 => 19,
        5 | 6 => 1,
        7 | 8 => 3,
        11 | 12 | 13 | 14 => 32,
        15 => 19,
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
        _ => 0,
    }
}

/// The non-client margins of a window style: (left, top, right, bottom).
fn frame(style: u32, exstyle: u32, menu: bool) -> (i32, i32, i32, i32) {
    let child = style & 0x4000_0000 != 0;
    let mut b = 0;
    if style & 0x0004_0000 != 0 {
        b += 4;
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
        top += if exstyle & 0x80 != 0 { 15 } else { 19 };
    }
    if menu && !child {
        top += 19;
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
        let clip = Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() }.off(x, y);
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
    unsafe { vnhost::display::vnhost_window_draw_shared(top, d.as_ptr(), g.px.as_ptr(), g.w, g.h) };
}

/// Tells the display where a top-level window is (its client area is what it shows).
fn update_display(s: &State, h: u32) {
    let Some(w) = s.wins.get(&h) else { return };
    if w.parent != 0 && w.parent != DESKTOP {
        return;
    }
    let c = w.client.off(w.rect.l, w.rect.t);
    let r = [c.l, c.t, c.r, c.b, c.l, c.t, c.r, c.b];
    let shown = w.visible && w.style & 0x4000_0000 == 0 && c.w() > 0 && c.h() > 0;
    unsafe { vnhost::display::vnhost_window_pos(h, r.as_ptr(), shown as i32, 0, 0) };
    // owned windows (dialogs) stay above their owner, as on Windows: RealLive's fullscreen
    // settings dialog is shown before its topmost fullscreen owner
    if shown {
        for (&o, ow) in s.wins.iter().filter(|(_, ow)| ow.owner == h && ow.visible && (ow.parent == 0 || ow.parent == DESKTOP)) {
            let c = ow.client.off(ow.rect.l, ow.rect.t);
            let r = [c.l, c.t, c.r, c.b, c.l, c.t, c.r, c.b];
            unsafe { vnhost::display::vnhost_window_pos(o, r.as_ptr(), 1, 1, 0) };
        }
    }
}

fn resize_surface(w: &mut Window) {
    if w.parent != 0 && w.parent != DESKTOP {
        return;
    }
    let (cw, ch) = (w.client.w().max(1), w.client.h().max(1));
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
        return s.classes.iter().position(|c| c.atom == name);
    }
    let n = arg_str(name, wide).to_lowercase();
    s.classes.iter().position(|c| c.name.to_lowercase() == n)
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

fn control_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("control_proc", control_proc_fn, 16))
}

/// Tab controls: hwnd -> (each item's lParam, selection). Delphi's TPageControl checks what
/// TCM_INSERTITEM / TCM_DELETEITEM return and reads its page objects back with TCM_GETITEM;
/// nothing is drawn.
static TABS: Mutex<Option<HashMap<u32, (Vec<u32>, i32)>>> = Mutex::new(None);
fn tab_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("tab_proc", tab_proc_fn, 16))
}
fn tab_proc_fn(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut g = TABS.lock().unwrap();
    let t = g.get_or_insert_with(HashMap::default).entry(h).or_insert((Vec::new(), -1));
    let n = t.0.len() as u32;
    // TCITEM: mask, state, state mask, text, text max, image, lParam (TCIF_PARAM = 8)
    let param = |lp: u32| if lp != 0 && mem::r32(lp) & 8 != 0 { mem::r32(lp + 24) } else { 0 };
    match m {
        0x1304 => n,
        // TCM_INSERTITEMA/W: at min(index, count)
        0x1307 | 0x133e => {
            let i = wp.min(n);
            t.0.insert(i as usize, param(lp));
            if t.1 < 0 {
                t.1 = 0;
            }
            i
        }
        0x1308 => {
            if wp >= n {
                return 0;
            }
            t.0.remove(wp as usize);
            if t.1 >= t.0.len() as i32 {
                t.1 = t.0.len() as i32 - 1;
            }
            1
        }
        0x1309 => {
            *t = (Vec::new(), -1);
            1
        }
        0x130b => t.1 as u32,
        0x130c => {
            let old = t.1;
            if wp < n {
                t.1 = wp as i32;
            }
            old as u32
        }
        0x132c => 1,
        // TCM_GETITEMA/W: lParam; the text is not kept (an empty string)
        0x1305 | 0x133c => {
            if wp >= n || lp == 0 {
                return 0;
            }
            let mask = mem::r32(lp);
            if mask & 8 != 0 {
                w32(lp + 24, t.0[wp as usize]);
            }
            if mask & 1 != 0 && mem::r32(lp + 12) != 0 && mem::r32(lp + 16) != 0 {
                if m == 0x133c { mem::w16(mem::r32(lp + 12), 0) } else { mem::w8(mem::r32(lp + 12), 0) }
            }
            1
        }
        // TCM_SETITEMA/W
        0x1306 | 0x133d => {
            if wp >= n {
                return 0;
            }
            if lp != 0 && mem::r32(lp) & 8 != 0 {
                t.0[wp as usize] = mem::r32(lp + 24);
            }
            1
        }
        0x130a | 0x1328 | 0x1303 | 0x1302 => 0,
        0x2 => {
            g.as_mut().unwrap().remove(&h);
            drop(g);
            def_proc(c, msg_wide())
        }
        _ => {
            drop(g);
            def_proc(c, msg_wide())
        }
    }
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
    crate::gdi::fill_rect_brush(dc, full, bg);
    let kind = style & 0xf;
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
    } else if kind == 7 {
        // group box
        frame(Rect { l: 0, t: 7, r: w, b: hh }, 0xdcdcdc);
        tr = Rect { l: 8, t: 0, r: w - 8, b: 16 };
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
        0x102 | 0x201 | 0x87 if with(|s| s.wins.get(&h).is_some_and(|w| s.classes[w.class].name.eq_ignore_ascii_case("edit"))) => {
            match m {
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
                // (the class's own: a CBT hook subclassing a new dialog keeps it as the super proc)
                "#32770" => dlg_wndproc(),
                _ => builtin_proc(),
            },
            cls_extra: 0,
            wnd_extra: 32,
            inst: 0,
            icon: 0,
            cursor: 0,
            brush: 0,
            menu: 0,
            unicode: false,
            atom,
            extra: Vec::new(),
        });
    }
}

fn register_class(c: &mut Ctx, p: u32, wide: bool, ex: bool) -> u32 {
    let o = if ex { 4 } else { 0 };
    let name_p = r32(p + o + 36);
    let name = if name_p < 0x10000 { format!("#{name_p}") } else { arg_str(name_p, wide) };
    let atom = crate::k32::atom(&name);
    let cls = Class {
        name: name.clone(),
        style: r32(p + o),
        proc_: r32(p + o + 4),
        cls_extra: r32(p + o + 8),
        wnd_extra: r32(p + o + 12),
        inst: r32(p + o + 16),
        icon: r32(p + o + 20),
        cursor: r32(p + o + 24),
        brush: r32(p + o + 28),
        menu: r32(p + o + 32),
        unicode: wide,
        atom,
        extra: vec![0; r32(p + o + 8) as usize],
    };
    let _ = c;
    with(|s| {
        system_classes(s);
        let low = name.to_lowercase();
        if let Some(i) = s.classes.iter().position(|k| k.name.to_lowercase() == low) {
            s.classes[i] = cls;
        } else {
            s.classes.push(cls);
        }
    });
    atom
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
fn UnregisterClassA(_: &mut Ctx) -> u32 {
    1
}
fn UnregisterClassW(_: &mut Ctx) -> u32 {
    1
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
        w32(crate::cur().teb + 0x34, 1411);
        return 0;
    };
    let o = if ex { 4 } else { 0 };
    if ex {
        w32(out, 48);
    }
    for (k, v) in [style, proc_, ce, we, inst, icon, cursor, brush, menu, if name < 0x10000 { name } else { 0 }].iter().enumerate() {
        w32(out + o + 4 * k as u32, *v);
    }
    if name >= 0x10000 {
        w32(out + o + 36, name);
    }
    if ex {
        w32(out + 44, 0);
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

pub fn send_cpu(cpu: &mut vnx86::Cpu, hwnd: u32, msg: u32, wp: u32, lp: u32) -> u32 {
    let info = with(|s| s.wins.get(&hwnd).map(|w| (w.proc_, w.tid)));
    let Some((proc_, tid)) = info else { return 0 };
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

fn send_other_thread(cpu: &mut vnx86::Cpu, tid: u32, msg: Msg) -> u32 {
    let result = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    with(|s| s.queues.entry(tid).or_default().sent.push_back(Sent { msg, result: result.clone() }));
    crate::obj::signal();
    // wait, handling messages sent to us meanwhile
    loop {
        {
            let g = result.0.lock().unwrap();
            if let Some(v) = *g {
                return v;
            }
            let (g, _) = result.1.wait_timeout(g, Duration::from_millis(5)).unwrap();
            if let Some(v) = *g {
                return v;
            }
        }
        process_sent(cpu);
        if !with(|s| s.wins.values().any(|w| w.tid == tid)) && !with(|s| s.queues.contains_key(&tid)) {
            return 0;
        }
    }
}

/// Runs messages other threads sent to this one.
fn process_sent(cpu: &mut vnx86::Cpu) {
    let me = crate::cur().tid;
    loop {
        let s = with(|s| s.queues.get_mut(&me).and_then(|q| q.sent.pop_front()));
        let Some(sent) = s else { return };
        let m = sent.msg;
        let proc_ = with(|s| s.wins.get(&m.hwnd).map(|w| w.proc_));
        let r = match proc_ {
            Some(p) => crate::call(cpu, p, &[m.hwnd, m.msg, m.w, m.l]),
            None => 0,
        };
        *sent.result.0.lock().unwrap() = Some(r);
        sent.result.1.notify_all();
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
    if !child && style & 0x8000_0000 == 0 {
        // overlapped: a caption
        style |= 0x00c0_0000;
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
        w32(crate::cur().teb + 0x34, 1407);
        return 0;
    };
    if std::env::var_os("SAKE_TRACE_WIN").is_some() {
        eprintln!("sake: CreateWindowEx {h:x} class {cname} style {style:x} ex {ex:x} {x},{y} {cx}x{cy} parent {parent:x}");
    }
    // CREATESTRUCT on the guest stack (below the caller's)
    let save = c.cpu.regs[vnx86::ESP];
    let cs = (save - 0x80) & !15;
    c.cpu.regs[vnx86::ESP] = cs - 16;
    for (k, v) in [param, inst, menu, parent, cy as u32, cx as u32, y as u32, x as u32, style, name_p, cls_p, ex].iter().enumerate() {
        w32(cs + 4 * k as u32, *v);
    }
    // HCBT_CREATEWND (CBT_CREATEWND: CREATESTRUCT, insert after)
    let cbtw = cs + 0x70;
    w32(cbtw, cs);
    w32(cbtw + 4, 0);
    if cbt(c, 3, h, cbtw) {
        c.cpu.regs[vnx86::ESP] = save;
        destroy(c, h);
        return 0;
    }
    // MINMAXINFO for WM_GETMINMAXINFO
    let mmi = cs + 0x40;
    if !child {
        let (sw, sh) = screen();
        for (k, v) in [0, 0, sw, sh, 0, 0, 112, 27, sw + 12, sh + 12].iter().enumerate() {
            w32(mmi + 4 * k as u32, *v as u32);
        }
        send(c, h, 0x24, 0, mmi);
    }
    let ok = send(c, h, 0x81, 0, cs);
    if ok == 0 {
        c.cpu.regs[vnx86::ESP] = save;
        destroy(c, h);
        return 0;
    }
    if send(c, h, 0x1, 0, cs) == 0xffff_ffff {
        c.cpu.regs[vnx86::ESP] = save;
        destroy(c, h);
        return 0;
    }
    c.cpu.regs[vnx86::ESP] = save;
    if !is_window(h) {
        return 0;
    }
    // top-level windows get WM_SIZE / WM_MOVE when first shown (games keep the window in a
    // global only once CreateWindowEx returns), child windows now
    if child {
        let (cw, ch) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.client.w(), w.client.h())));
        send(c, h, 0x5, 0, (ch as u32) << 16 | (cw as u32 & 0xffff));
        send(c, h, 0x3, 0, (y as u32) << 16 | (x as u32 & 0xffff));
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
                vnhost::display::vnhost_window_destroy(h);
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
    true
}
fn DestroyWindow(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    destroy(c, h) as u32
}

fn show(c: &mut Ctx, h: u32, cmd: u32) -> u32 {
    let was = with(|s| s.wins.get(&h).map(|w| w.visible));
    let Some(was) = was else { return 0 };
    let vis = !matches!(cmd, 0);
    if vis != was {
        send(c, h, 0x18, vis as u32, 0);
        with(|s| {
            if let Some(w) = s.wins.get_mut(&h) {
                w.visible = vis;
                if vis {
                    w.update = Rect { l: 0, t: 0, r: w.client.w(), b: w.client.h() };
                    w.erase = true;
                }
            }
            update_display(s, h);
        });
        let (cw, ch) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.client.w(), w.client.h())));
        if vis {
            send(c, h, 0x5, 0, (ch as u32) << 16 | (cw as u32 & 0xffff));
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
    is_window(c.a(0)) as u32
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

fn set_pos(c: &mut Ctx, h: u32, after: u32, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> u32 {
    let _ = after;
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
        let (cw, ch) = (w.client.w(), w.client.h());
        update_display(s, h);
        Some((resized, moved, cw, ch, r))
    });
    let Some((resized, moved, cw, ch, r)) = r else { return 0 };
    // WINDOWPOS for WM_WINDOWPOSCHANGED
    let save = c.cpu.regs[vnx86::ESP];
    let wp = (save - 0x40) & !15;
    c.cpu.regs[vnx86::ESP] = wp - 16;
    for (k, v) in [h, after, r.l as u32, r.t as u32, r.w() as u32, r.h() as u32, flags].iter().enumerate() {
        w32(wp + 4 * k as u32, *v);
    }
    send(c, h, 0x47, 0, wp);
    c.cpu.regs[vnx86::ESP] = save;
    if resized {
        send(c, h, 0x5, 0, (ch as u32) << 16 | (cw as u32 & 0xffff));
    }
    if moved {
        send(c, h, 0x3, 0, (r.t as u32) << 16 | (r.l as u32 & 0xffff));
    }
    if flags & 0x40 != 0 {
        send(c, h, 0x18, 1, 0);
    }
    crate::obj::signal();
    1
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
    put32(c.a(1), crate::PID);
    with(|s| s.wins.get(&c.a(0)).map_or(0, |w| w.tid))
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
fn find_window(cls: u32, title: u32, wide: bool) -> u32 {
    let cls_name = (cls != 0).then(|| if cls < 0x10000 { crate::k32::atom_name(cls).unwrap_or_default() } else { arg_str(cls, wide) }.to_lowercase());
    let t = (title != 0).then(|| arg_str(title, wide));
    with(|s| {
        s.wins
            .iter()
            .find(|(_, w)| {
                w.parent == 0
                    && cls_name.as_ref().is_none_or(|n| s.classes[w.class].name.to_lowercase() == *n)
                    && t.as_ref().is_none_or(|t| String::from_utf16_lossy(&w.text) == *t)
            })
            .map_or(0, |(&h, _)| h)
    })
}
fn FindWindowA(c: &mut Ctx) -> u32 {
    find_window(c.a(0), c.a(1), false)
}
fn FindWindowW(c: &mut Ctx) -> u32 {
    find_window(c.a(0), c.a(1), true)
}
fn FindWindowExA(c: &mut Ctx) -> u32 {
    find_window(c.a(2), c.a(3), false)
}
fn FindWindowExW(c: &mut Ctx) -> u32 {
    find_window(c.a(2), c.a(3), true)
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
fn WindowFromPoint(c: &mut Ctx) -> u32 {
    let (x, y) = (c.i(0), c.i(1));
    with(|s| window_at(s, x, y))
}
fn ChildWindowFromPoint(c: &mut Ctx) -> u32 {
    let (h, x, y) = (c.a(0), c.i(1), c.i(2));
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
fn SetWindowLongA(c: &mut Ctx) -> u32 {
    set_long(c.a(0), c.i(1), c.a(2))
}
fn SetWindowLongW(c: &mut Ctx) -> u32 {
    set_long(c.a(0), c.i(1), c.a(2))
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
    let key = if k < 0x10000 { format!("#{k}") } else { arg_str(k, false) };
    with(|s| {
        let Some(w) = s.wins.get_mut(&h) else { return 0 };
        w.props.retain(|(x, _)| *x != key);
        w.props.push((key, v));
        1
    })
}
fn SetPropW(c: &mut Ctx) -> u32 {
    let (h, k, v) = (c.a(0), c.a(1), c.a(2));
    let key = if k < 0x10000 { format!("#{k}") } else { arg_str(k, true) };
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
        arg_str(k, wide)
    }
}
fn GetPropA(c: &mut Ctx) -> u32 {
    let key = prop_key(c.a(1), false);
    with(|s| s.wins.get(&c.a(0)).and_then(|w| w.props.iter().find(|(x, _)| *x == key).map(|p| p.1)).unwrap_or(0))
}
fn GetPropW(c: &mut Ctx) -> u32 {
    let key = prop_key(c.a(1), true);
    with(|s| s.wins.get(&c.a(0)).and_then(|w| w.props.iter().find(|(x, _)| *x == key).map(|p| p.1)).unwrap_or(0))
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
            with(|s| {
                if let Some(w) = s.wins.get_mut(&h) {
                    w.text = t;
                }
            });
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
        0x47 => 0,
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
            None => return false,
        }
    };
    post_thread(tid, Msg { hwnd, msg, w, l, time: now_ms(), x: 0, y: 0 });
    true
}
fn post_thread(tid: u32, mut m: Msg) {
    with(|s| {
        m.x = s.cursor.0;
        m.y = s.cursor.1;
        s.queues.entry(tid).or_default().posted.push_back(m)
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
fn dde_string(c: &mut Ctx, wide: bool) -> u32 {
    let h = 0x100 + DDE_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let p = c.a(1);
    let t = if p == 0 { Vec::new() } else if wide { wstr(p) } else { a2w(&cstr(p)) };
    DDE_STR.lock().unwrap().push((h, t));
    h
}
fn DdeCreateStringHandleA(c: &mut Ctx) -> u32 {
    dde_string(c, false)
}
fn DdeCreateStringHandleW(c: &mut Ctx) -> u32 {
    dde_string(c, true)
}
/// DdeQueryString(inst, hsz, buffer, cch, codepage): the length without the NUL
fn dde_query(c: &mut Ctx, wide: bool) -> u32 {
    let (h, buf, cch) = (c.a(1), c.a(2), c.a(3));
    let Some(t) = DDE_STR.lock().unwrap().iter().find(|x| x.0 == h).map(|x| x.1.clone()) else { return 0 };
    if wide {
        if buf != 0 && cch != 0 {
            mem::put_wstr(buf, cch, &t);
        }
        t.len() as u32
    } else {
        let a = w2a(&t);
        if buf != 0 && cch != 0 {
            mem::put_cstr(buf, cch, &a);
        }
        a.len() as u32
    }
}
fn DdeQueryStringA(c: &mut Ctx) -> u32 {
    dde_query(c, false)
}
fn DdeQueryStringW(c: &mut Ctx) -> u32 {
    dde_query(c, true)
}
fn DdeCmpStringHandles(c: &mut Ctx) -> u32 {
    let g = DDE_STR.lock().unwrap();
    let get = |h: u32| g.iter().find(|x| x.0 == h).map(|x| x.1.iter().map(|&ch| crate::text::lower16(ch)).collect::<Vec<_>>());
    match get(c.a(0)).cmp(&get(c.a(1))) {
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
                .filter(|(&h, w)| w.tid == me && w.visible && !w.update.empty() && (hwnd == 0 || hwnd == h))
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
    let me = crate::cur().tid;
    with(|s| {
        let Some(q) = s.queues.get(&me) else { return false };
        !q.posted.is_empty() || !q.input.is_empty() || !q.sent.is_empty() || q.quit.is_some()
            || s.wins.iter().any(|(_, w)| w.tid == me && w.visible && !w.update.empty())
    })
}

/// Blocks until this thread has a message, a handle is signaled or `ms` passes.
fn wait_msgs(handles: &[u32], all: bool, ms: u32) -> u32 {
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
        match s {
            Ok(g) => g.as_ref().is_some_and(|s| {
                s.queues.get(&me).is_some_and(|q| !q.posted.is_empty() || !q.input.is_empty() || !q.sent.is_empty() || q.quit.is_some())
                    || s.wins.iter().any(|(_, w)| w.tid == me && w.visible && !w.update.empty())
            }),
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
    let (n, hs, all, ms) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    let hs: Vec<u32> = (0..n.min(64)).map(|i| r32(hs + 4 * i)).collect();
    process_sent(c.cpu);
    if has_work() {
        return n;
    }
    wait_msgs(&hs, all, ms)
}
fn MsgWaitForMultipleObjectsEx(c: &mut Ctx) -> u32 {
    let (n, hs, ms, flags) = (c.a(0), c.a(1), c.a(2), c.a(4));
    let hs: Vec<u32> = (0..n.min(64)).map(|i| r32(hs + 4 * i)).collect();
    process_sent(c.cpu);
    if has_work() {
        return n;
    }
    wait_msgs(&hs, flags & 1 != 0, ms)
}
fn GetQueueStatus(_: &mut Ctx) -> u32 {
    if has_work() { 0x00ff_00ff } else { 0 }
}
fn GetInputState(_: &mut Ctx) -> u32 {
    let me = crate::cur().tid;
    with(|s| s.queues.get(&me).is_some_and(|q| !q.input.is_empty())) as u32
}
fn TranslateMessage(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let (h, m, wp, lp) = (r32(p), r32(p + 4), r32(p + 8), r32(p + 12));
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
    let (h, m, wp, lp) = (r32(p), r32(p + 4), r32(p + 8), r32(p + 12));
    if m == 0x113 && lp != 0 {
        return c.call(lp, &[h, m, wp, now_ms()]);
    }
    if h == 0 {
        return 0;
    }
    if (0x200..=0x20a).contains(&m) && m != 0x200 && std::env::var_os("SAKE_TRACE_INPUT").is_some() {
        eprintln!("sake input: dispatch {h:x} msg {m:x} {wp:x} {lp:x}");
    }
    let r = send(c, h, m, wp, lp);
    if (0x200..=0x20a).contains(&m) && m != 0x200 && std::env::var_os("SAKE_TRACE_INPUT").is_some() {
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
/// IsDialogMessage(dlg, msg): Enter / Escape pressed in the dialog become IDOK / IDCANCEL; the
/// rest is left to the caller's TranslateMessage / DispatchMessage.
fn is_dialog_message(c: &mut Ctx, wide: bool) -> u32 {
    let (dlg, msg) = (c.a(0), c.a(1));
    let (h, m, key) = (r32(msg), r32(msg + 4), r32(msg + 8));
    if m != 0x100 || !(key == 0x0d || key == 0x1b) || dlg == 0 {
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
fn TranslateAcceleratorA(_: &mut Ctx) -> u32 {
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
    with(|s| {
        for q in s.queues.values_mut() {
            q.timers.retain(|t| !(t.hwnd == h && t.id == id));
        }
    });
    1
}

// painting
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
    invalidate(c.a(0), None, c.a(2) != 0);
    1
}
fn ValidateRect(c: &mut Ctx) -> u32 {
    with(|s| {
        if let Some(w) = s.wins.get_mut(&c.a(0)) {
            w.update = Rect::default();
        }
    });
    1
}
fn ValidateRgn(c: &mut Ctx) -> u32 {
    ValidateRect(c)
}
fn RedrawWindow(c: &mut Ctx) -> u32 {
    let (h, r, flags) = (c.a(0), c.a(1), c.a(3));
    if flags & 1 != 0 {
        invalidate(h, (r != 0).then(|| Rect::read(r)), flags & 4 != 0);
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
    let dc = crate::gdi::window_dc(h, Some(upd));
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
    mem::fill(ps, 64, 0);
    w32(ps, dc);
    w32(ps + 4, 0);
    upd.write(ps + 8);
    dc
}
fn EndPaint(c: &mut Ctx) -> u32 {
    let ps = c.a(1);
    crate::gdi::release_dc(r32(ps));
    1
}
fn GetDC(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h == 0 || h == DESKTOP {
        return crate::gdi::screen_dc();
    }
    crate::gdi::window_dc(h, None)
}
fn GetDCEx(c: &mut Ctx) -> u32 {
    GetDC(c)
}
fn GetWindowDC(c: &mut Ctx) -> u32 {
    GetDC(c)
}
fn ReleaseDC(c: &mut Ctx) -> u32 {
    crate::gdi::release_dc(c.a(1));
    1
}
fn WindowFromDC(c: &mut Ctx) -> u32 {
    crate::gdi::dc_window(c.a(0))
}
fn ScrollWindow(_: &mut Ctx) -> u32 {
    1
}
fn ScrollWindowEx(_: &mut Ctx) -> u32 {
    1
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
        w32(c.a(1), if crate::text::acp() == 932 { 0x0411_0411 } else { 0x0409_0409 });
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
    match c.a(3) & 0xf {
        1 | 3 => 1,
        4 => 6,
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
    match c.a(0) {
        0 => 0xc8c8c8,
        1 => 0x000000,
        2 => 0xd1b499,
        3 => 0xdbcdbf,
        4 => 0xf0f0f0,
        5 => 0xffffff,
        6 => 0x646464,
        7 | 8 => 0x000000,
        9 => 0x000000,
        10 | 11 => 0xb4b4b4,
        12 => 0xababab,
        13 => 0xff9933,
        14 => 0xffffff,
        15 => 0xf0f0f0,
        16 => 0xa0a0a0,
        17 => 0x6d6d6d,
        18 => 0x000000,
        19 => 0x000000,
        20 => 0xffffff,
        21 => 0x696969,
        22 => 0xe3e3e3,
        23 => 0x000000,
        24 => 0xe1ffff,
        _ => 0xf0f0f0,
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
            // NONCLIENTMETRICS: sizes and fonts
            let lf = if wide { 92 } else { 60 };
            let size = r32(p);
            mem::fill(p + 4, size.saturating_sub(4), 0);
            for (k, v) in [1u32, 16, 16, 18, 19].iter().enumerate() {
                w32(p + 4 + 4 * k as u32, *v);
            }
            let fonts = [24, 24 + lf + 8, 24 + 2 * lf + 16, 24 + 3 * lf + 16, 24 + 4 * lf + 16];
            for o in fonts {
                if o + lf <= size {
                    crate::gdi::default_logfont(p + o, wide);
                }
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
    let p = c.a(0);
    if p != 0 {
        let fields = r32(p + 40);
        if fields & 0x180000 == 0x180000 {
            let (w, h) = (r32(p + 108) as i32, r32(p + 112) as i32);
            set_display(c, w, h);
        }
    }
    0
}

/// A new screen size; top-level windows get WM_DISPLAYCHANGE (CatSystem2 goes on from there)
fn set_display(c: &mut Ctx, w: i32, h: i32) {
    vnhost::display::vnhost_display_set_size(w, h);
    if with(|s| std::mem::replace(&mut s.screen, (w, h))) != (w, h) {
        let f = crate::api::export("user32", "SendMessageA");
        c.call(f, &[0xffff, 0x7e, 32, (h as u32) << 16 | (w as u32 & 0xffff)]);
    }
}
fn ChangeDisplaySettingsW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p != 0 {
        let fields = r32(p + 72);
        if fields & 0x180000 == 0x180000 {
            let (w, h) = (r32(p + 172) as i32, r32(p + 176) as i32);
            set_display(c, w, h);
        }
    }
    0
}
/// DEVMODE of mode `n` (ENUM_CURRENT_SETTINGS/ENUM_REGISTRY_SETTINGS: the screen's). The W
/// layout has a 64-byte device name and a 64-byte form name: fields before dmFormName are 32
/// bytes later, those after it 64.
fn display_mode(n: u32, p: u32, wide: bool) -> u32 {
    let modes = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 960)];
    let (w, h) = if n == 0xffff_ffff || n == 0xffff_fffe { screen() } else if (n as usize) < modes.len() { modes[n as usize] } else { return 0 };
    let (d, e) = if wide { (32, 64) } else { (0, 0) };
    mem::fill(p + 36 + d, 120 + e - d, 0);
    w16(p + 36 + d, 156 + e as u16);
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
    if n != 0 || p == 0 {
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
fn MonitorFromWindow(_: &mut Ctx) -> u32 {
    0x10001
}
fn MonitorFromPoint(_: &mut Ctx) -> u32 {
    0x10001
}
fn MonitorFromRect(_: &mut Ctx) -> u32 {
    0x10001
}
fn GetMonitorInfoA(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let (w, h) = screen();
    let r = Rect { l: 0, t: 0, r: w, b: h };
    r.write(p + 4);
    r.write(p + 20);
    w32(p + 36, 1);
    if r32(p) >= 72 {
        mem::put_cstr(p + 40, 32, b"\\\\.\\DISPLAY1");
    }
    1
}
fn GetMonitorInfoW(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let (w, h) = screen();
    let r = Rect { l: 0, t: 0, r: w, b: h };
    r.write(p + 4);
    r.write(p + 20);
    w32(p + 36, 1);
    1
}
fn EnumDisplayMonitors(c: &mut Ctx) -> u32 {
    let (f, lp) = (c.a(2), c.a(3));
    let save = c.cpu.regs[vnx86::ESP];
    let r = (save - 0x20) & !15;
    let (w, h) = screen();
    Rect { l: 0, t: 0, r: w, b: h }.write(r);
    c.cpu.regs[vnx86::ESP] = r - 16;
    c.call(f, &[0x10001, 0, r, lp]);
    c.cpu.regs[vnx86::ESP] = save;
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
    Rect::read(c.a(0)).has(c.i(1), c.i(2)) as u32
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
    r.write(c.a(0));
    (!r.empty()) as u32
}
fn SubtractRect(c: &mut Ctx) -> u32 {
    Rect::read(c.a(1)).write(c.a(0));
    1
}
fn FillRect(c: &mut Ctx) -> u32 {
    crate::gdi::fill_rect_brush(c.a(0), Rect::read(c.a(1)), c.a(2));
    1
}
fn FrameRect(c: &mut Ctx) -> u32 {
    let r = Rect::read(c.a(1));
    let (dc, b) = (c.a(0), c.a(2));
    for e in [Rect { b: r.t + 1, ..r }, Rect { t: r.b - 1, ..r }, Rect { r: r.l + 1, ..r }, Rect { l: r.r - 1, ..r }] {
        crate::gdi::fill_rect_brush(dc, e, b);
    }
    1
}
fn InvertRect(_: &mut Ctx) -> u32 {
    1
}
fn DrawFocusRect(_: &mut Ctx) -> u32 {
    1
}
fn DrawEdge(_: &mut Ctx) -> u32 {
    1
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
fn GetScrollInfo(c: &mut Ctx) -> u32 {
    let p = c.a(2);
    let size = r32(p);
    mem::fill(p + 8, size.saturating_sub(8), 0);
    0
}
fn SetScrollInfo(_: &mut Ctx) -> u32 {
    0
}
fn GetScrollPos(_: &mut Ctx) -> u32 {
    0
}
fn SetScrollPos(_: &mut Ctx) -> u32 {
    0
}
fn GetScrollRange(c: &mut Ctx) -> u32 {
    put32(c.a(2), 0);
    put32(c.a(3), 0);
    1
}
fn SetScrollRange(_: &mut Ctx) -> u32 {
    1
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
fn read_mii(p: u32, wide: bool) -> (u32, MenuItem) {
    let mask = r32(p + 4);
    let mut it = MenuItem::default();
    if mask & 0x100 != 0 || mask & 1 != 0 {
        it.flags = r32(p + 8) | r32(p + 12);
    }
    if mask & 2 != 0 {
        it.id = r32(p + 16);
    }
    if mask & 4 != 0 {
        it.sub = r32(p + 20);
    }
    if mask & 0x20 != 0 {
        it.data = r32(p + 32);
    }
    if mask & 0x40 != 0 || (mask & 0x10 != 0 && r32(p + 8) == 0) {
        let t = r32(p + 36);
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
    let (h, item, by_pos, p) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    let (mask, it) = read_mii(p, false);
    with(|s| {
        let Some(m) = s.menus.get_mut(&h) else { return 0 };
        let Some(i) = item_index(m, item, by_pos) else { return 0 };
        let x = &mut m.items[i];
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
    let (h, item, by_pos, p) = (c.a(0), c.a(1), c.a(2) != 0, c.a(3));
    let it = with(|s| s.menus.get(&h).and_then(|m| item_index(m, item, by_pos).map(|i| m.items[i].clone())));
    let Some(it) = it else { return 0 };
    let mask = r32(p + 4);
    if mask & 0x101 != 0 {
        w32(p + 8, it.flags & 0x6b00);
        w32(p + 12, it.flags & 0x108b);
    }
    if mask & 2 != 0 {
        w32(p + 16, it.id);
    }
    if mask & 4 != 0 {
        w32(p + 20, it.sub);
    }
    if mask & 0x20 != 0 {
        w32(p + 32, it.data);
    }
    if mask & 0x50 != 0 {
        let a = w2a(&it.text);
        let cap = r32(p + 40);
        if r32(p + 36) != 0 && cap > 0 {
            mem::put_cstr(r32(p + 36), cap, &a);
        }
        w32(p + 40, a.len() as u32);
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
    with(|s| s.menus.get(&h).and_then(|m| item_index(m, item, flags & 0x400 != 0).map(|i| m.items[i].flags & 0xffff)).unwrap_or(0xffff_ffff))
}
fn GetMenuStringA(c: &mut Ctx) -> u32 {
    let (h, item, buf, cap, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let t = with(|s| s.menus.get(&h).and_then(|m| item_index(m, item, flags & 0x400 != 0).map(|i| m.items[i].text.clone()))).unwrap_or_default();
    let a = w2a(&t);
    if buf == 0 {
        return a.len() as u32;
    }
    mem::put_cstr(buf, cap, &a)
}
fn modify_flags(h: u32, item: u32, flags: u32, set: u32, clear: u32) -> u32 {
    with(|s| {
        let Some(m) = s.menus.get_mut(&h) else { return 0xffff_ffff };
        let Some(i) = item_index(m, item, flags & 0x400 != 0) else { return 0xffff_ffff };
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
        let Some(m) = s.menus.get_mut(&h) else { return 0 };
        match item_index(m, item, f & 0x400 != 0) {
            Some(i) => {
                m.items.remove(i);
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
    let inst = if inst == 0 { crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map_or(0, |m| m.base) } else { inst };
    let Some(e) = crate::pe::find_resource(inst, &crate::pe::ResId::Int(6), &crate::pe::ResId::Int(id / 16 + 1)) else { return 0 };
    let (mut p, _) = crate::pe::resource_data(inst, e);
    for _ in 0..id % 16 {
        p += 2 + 2 * r16(p) as u32;
    }
    let n = r16(p) as u32;
    let s: Vec<u16> = (0..n).map(|i| r16(p + 2 + 2 * i)).collect();
    if wide {
        if cap == 0 {
            w32(buf, p + 2);
            return n;
        }
        mem::put_wstr(buf, cap, &s)
    } else {
        mem::put_cstr(buf, cap, &w2a(&s))
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
fn LoadCursorFromFileA(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn LoadCursorFromFileW(_: &mut Ctx) -> u32 {
    fake_handle()
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
    // a 32x32 monochrome cursor: AND mask over XOR mask, no color bitmap
    let p = c.a(1);
    mem::fill(p, 20, 0);
    w32(p, 0);
    w32(p + 12, crate::gdi::new_bitmap(32, 64, 1));
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
fn LoadAcceleratorsA(_: &mut Ctx) -> u32 {
    fake_handle()
}
fn LoadMenuA(_: &mut Ctx) -> u32 {
    new_menu()
}
fn LoadMenuW(_: &mut Ctx) -> u32 {
    new_menu()
}

// strings
fn char_case(c: &mut Ctx, upper: bool) -> u32 {
    let p = c.a(0);
    if p < 0x10000 {
        let b = p as u8;
        return if upper { b.to_ascii_uppercase() } else { b.to_ascii_lowercase() } as u32;
    }
    let mut s = cstr(p);
    if upper {
        crate::text::upper_a(&mut s);
    } else {
        crate::text::lower_a(&mut s);
    }
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
    crate::text::lower_a(s);
    n
}
fn CharUpperBuffA(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let s = mem::slice_mut(p, n);
    crate::text::upper_a(s);
    n
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
fn CharNextW(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if r16(p) == 0 { p } else { p + 2 }
}
fn CharPrevA(c: &mut Ctx) -> u32 {
    let (start, p) = (c.a(0), c.a(1));
    if p <= start {
        return start;
    }
    let mut q = start;
    loop {
        let n = if crate::text::is_lead(mem::r8(q)) { q + 2 } else { q + 1 };
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
fn wsprintfA(c: &mut Ctx) -> u32 {
    let (dst, fmt) = (c.a(0), c.a(1));
    let out = crate::misc::format(&cstr(fmt), &mut crate::misc::VaArgs::stack(c.esp + 12), false);
    let out: Vec<u8> = out.into_iter().take(1023).collect();
    mem::write(dst, &out);
    mem::w8(dst + out.len() as u32, 0);
    out.len() as u32
}
fn wvsprintfA(c: &mut Ctx) -> u32 {
    let (dst, fmt, va) = (c.a(0), c.a(1), c.a(2));
    let out = crate::misc::format(&cstr(fmt), &mut crate::misc::VaArgs::stack(va), false);
    let out: Vec<u8> = out.into_iter().take(1023).collect();
    mem::write(dst, &out);
    mem::w8(dst + out.len() as u32, 0);
    out.len() as u32
}
fn wsprintfW(c: &mut Ctx) -> u32 {
    let (dst, fmt) = (c.a(0), c.a(1));
    let f: Vec<u16> = wstr(fmt);
    let out = crate::misc::format_w(&f, &mut crate::misc::VaArgs::stack(c.esp + 12));
    mem::put_wstr(dst, 1024, &out)
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
// input from the page (vnhost's input pipe)

fn start_input() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::thread::spawn(|| {
            let fd = vnhost::display::vnhost_input_fd();
            if fd < 0 {
                return;
            }
            let mut buf = [0u32; 64];
            let trace = std::env::var_os("SAKE_TRACE_INPUT").is_some();
            loop {
                let mut p = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
                let r = unsafe { libc::poll(&mut p, 1, 100) };
                let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, 256) };
                if trace && n != -1 {
                    eprintln!("sake input: poll {r} read {n}");
                }
                if n <= 0 {
                    continue;
                }
                for r in buf[..n as usize / 4].chunks(4) {
                    if r.len() == 4 {
                        input(r[0], r[1] as i32, r[2] as i32, r[3]);
                        if trace {
                            eprintln!("sake input: {r:?} queued");
                        }
                    }
                }
                crate::obj::signal();
                if trace {
                    eprintln!("sake input: signaled");
                }
            }
        });
    });
}

fn input(kind: u32, x: i32, y: i32, data: u32) {
    with(|s| {
        let t = now_ms();
        match kind {
            1..=4 => {
                s.cursor = (x, y);
                let target = if s.capture != 0 { s.capture } else { window_at(s, x, y) };
                let Some(w) = s.wins.get(&target) else { return };
                let tid = w.tid;
                let c = client_screen(s, target);
                let (cx, cy) = (x - c.l, y - c.t);
                if std::env::var_os("SAKE_TRACE_INPUT").is_some() {
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
                s.queues.entry(tid).or_default().input.push_back(Msg { hwnd: target, msg, w: vk as u32, l: lp, time: t, x: s.cursor.0, y: s.cursor.1 });
            }
            _ => {}
        }
    });
}

// ---------------------------------------------------------------------------------------------
// dialogs: windows built from DLGTEMPLATE(EX) resources, a guest DLGPROC called from our window
// procedure (DWL_DLGPROC = window extra +4), and DialogBox's modal loop

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
    let p = extra32(h, 4);
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
        0x10 => {
            // WM_CLOSE: as the Cancel button
            let b = dlg_item(h, 2);
            send(c, h, 0x111, 2, b);
            0
        }
        _ => def_proc(c, false),
    }
}

/// The control Enter presses in a dialog: a focused push button, else the default push button
/// (BS_DEFPUSHBUTTON), else IDOK. (id, its window)
fn default_button(dlg: u32) -> (u32, u32) {
    let found = with(|s| {
        let push = |h: u32, def: bool| {
            s.wins.get(&h).filter(|w| s.classes.get(w.class).is_some_and(|c| c.name.eq_ignore_ascii_case("Button")) && w.visible && w.enabled).is_some_and(|w| {
                let k = w.style & 0xf;
                if def { k == 1 } else { k <= 1 }
            })
        };
        let kids = s.wins.get(&dlg).map(|w| w.children.clone()).unwrap_or_default();
        if kids.contains(&s.focus) && push(s.focus, false) {
            return Some((s.wins[&s.focus].id & 0xffff, s.focus));
        }
        kids.iter().find(|&&k| push(k, true)).map(|&k| (s.wins[&k].id & 0xffff, k))
    });
    found.unwrap_or_else(|| (1, dlg_item(dlg, 1)))
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
    set_extra32(dlg, 4, proc_);
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
        SetClassLongA, SetClassLongW, GetClassNameA, GetClassNameW, SetPropA, SetPropW, GetPropA, GetPropW,
        RemovePropA, RemovePropW, SetWindowTextA, SetWindowTextW, GetWindowTextA, GetWindowTextW,
        GetWindowTextLengthA, GetWindowTextLengthW, SetWindowRgn, DefWindowProcA, DefWindowProcW, DefDlgProcA,
        DefFrameProcA, DefMDIChildProcA, CallWindowProcA, CallWindowProcW,
        PostMessageA, PostMessageW, PostThreadMessageA, PostThreadMessageW, PostQuitMessage, SendMessageA,
        SendMessageW, SendMessageTimeoutA, SendNotifyMessageA, InSendMessage, ReplyMessage, GetMessageA,
        GetMessageW, PeekMessageA, PeekMessageW, WaitMessage, MsgWaitForMultipleObjects,
        MsgWaitForMultipleObjectsEx, GetQueueStatus, GetInputState, TranslateMessage, DispatchMessageA,
        DispatchMessageW, IsDialogMessageA, IsDialogMessageW, TranslateMDISysAccel, TranslateAcceleratorA,
        GetMessagePos, GetMessageTime, GetMessageExtraInfo, RegisterWindowMessageA, RegisterWindowMessageW,
        SetTimer, KillTimer, InvalidateRect, InvalidateRgn, ValidateRect, ValidateRgn, RedrawWindow,
        GetUpdateRect, GetUpdateRgn, BeginPaint, EndPaint, GetDC, GetDCEx, GetWindowDC, ReleaseDC, WindowFromDC,
        ScrollWindow, ScrollWindowEx, LockWindowUpdate, SetFocus, GetFocus, SetActiveWindow, GetActiveWindow,
        GetForegroundWindow, SetForegroundWindow, BringWindowToTop, SetCapture, ReleaseCapture, GetCapture,
        GetCursorPos, SetCursorPos, SetCursor, GetCursor, ShowCursor, ClipCursor, GetKeyState, GetAsyncKeyState,
        GetKeyboardState, SetKeyboardState, GetKeyboardType, GetKeyboardLayout, GetKeyboardLayoutList,
        ActivateKeyboardLayout, LoadKeyboardLayoutA, MapVirtualKeyA, MapVirtualKeyW, GetKeyNameTextA,
        GetDoubleClickTime, MessageBeep, MessageBoxA, MessageBoxW, MessageBoxExA, MessageBoxIndirectA,
        SetWindowsHookExA, SetWindowsHookExW, UnhookWindowsHookEx, CallNextHookEx, WinHelpA, GetSysColor,
        GetSysColorBrush, GetSystemMetrics, SystemParametersInfoA, SystemParametersInfoW, ChangeDisplaySettingsA,
        ChangeDisplaySettingsW, EnumDisplaySettingsA, EnumDisplaySettingsW, EnumDisplaySettingsExA, EnumDisplaySettingsExW, EnumDisplayDevicesA, EnumDisplayDevicesW, MonitorFromWindow, MonitorFromPoint,
        MonitorFromRect, GetMonitorInfoA, GetMonitorInfoW, EnumDisplayMonitors,
        SetRect, SetRectEmpty, CopyRect, IsRectEmpty, EqualRect, PtInRect, OffsetRect, InflateRect, IntersectRect,
        UnionRect, SubtractRect, FillRect, FrameRect, InvertRect, DrawFocusRect, DrawEdge, DrawFrameControl,
        DrawIcon, DrawIconEx, DrawTextA, DrawTextW, DrawTextExW, GrayStringA,
        CreateCaret, DestroyCaret, SetCaretPos, ShowCaret, HideCaret, GetScrollInfo, SetScrollInfo, GetScrollPos,
        SetScrollPos, GetScrollRange, SetScrollRange, ShowScrollBar, EnableScrollBar, OpenClipboard,
        CloseClipboard, EmptyClipboard, GetClipboardData, SetClipboardData, IsClipboardFormatAvailable,
        EnumClipboardFormats, GetPriorityClipboardFormat, RegisterClipboardFormatA, RegisterClipboardFormatW,
        CreateMenu, CreatePopupMenu, DestroyMenu, InsertMenuA, AppendMenuA, AppendMenuW, InsertMenuItemA,
        InsertMenuItemW, SetMenuItemInfoA, GetMenuItemInfoA, GetMenuItemCount, GetMenuItemID, GetSubMenu,
        GetMenuState, GetMenuStringA, CheckMenuItem, CheckMenuRadioItem, EnableMenuItem, DeleteMenu, RemoveMenu, ModifyMenuA, GetMenu,
        SetMenu, GetSystemMenu, DrawMenuBar, TrackPopupMenu, TrackPopupMenuEx, GetMenuItemRect,
        SetMenuItemBitmaps, GetMenuCheckMarkDimensions,
        LoadStringA, LoadStringW, LoadCursorA, LoadCursorW, LoadCursorFromFileA, LoadCursorFromFileW, LoadIconA, LoadIconW, LoadImageA,
        LoadImageW, CreateIcon, CreateIconIndirect, CreateCursor, CopyIcon, DestroyCursor, DestroyIcon,
        GetIconInfo, LoadBitmapA, LoadBitmapW, LoadAcceleratorsA, LoadMenuA, LoadMenuW,
        CharLowerA, CharUpperA, CharLowerW, CharUpperW, CharLowerBuffA, CharUpperBuffA, CharNextA, CharNextW,
        CharPrevA, CharToOemA, OemToCharA, OemToCharBuffA, CharToOemBuffA, IsCharAlphaA, IsCharAlphaNumericA,
        IsCharUpperA, IsCharLowerA, IsCharAlphaW, wsprintfA, wvsprintfA, wsprintfW, GetGUIThreadInfo,
        AttachThreadInput, FlashWindow, GetProcessWindowStation, GetUserObjectInformationA, SetProcessDPIAware,
        IsWindowUnicode, RegisterHotKey, UnregisterHotKey,
    );
    reg!("user32"; DdeInitializeA, DdeCreateStringHandleA, DdeCreateStringHandleW, DdeConnect, DdeGetLastError,
        DdeQueryStringA, DdeQueryStringW, DdeCmpStringHandles);
    implement("user32", "DdeInitializeW", DdeInitializeA);
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
