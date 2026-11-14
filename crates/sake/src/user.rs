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

use crate::api::{implement, Ctx, Handler};
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
    /// the answer, when the sender goes on, and the sender let go before it that it waits for
    /// (release_gate)
    result: Arc<(Mutex<Option<(u32, Instant, Option<waits::handoff::Mark>)>>, std::sync::Condvar)>,
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
    /// TrackMouseEvent's watch for this thread
    track: Option<crate::mousetrack::Track>,
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
    /// hbmpItem: MIIM_BITMAP's, or an MF_BITMAP item's handle (where the string would be)
    pub bmp: u32,
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
    /// the display mode (as set: ChangeDisplaySettings), which `screen` may have grown past
    pub mode: (i32, i32),
    pub cursor_handle: u32,
    pub cursor_count: i32,
    pub msgnames: HashMap<String, u32>,
    pub last_msg_pos: (i32, i32),
    pub last_msg_time: u32,
    /// where a maximized window goes back to when restored (its rect before)
    pub normal: HashMap<u32, Rect>,
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
            mode: (s, h),
            cursor_handle: 0,
            cursor_count: 0,
            msgnames: HashMap::default(),
            last_msg_pos: (0, 0),
            last_msg_time: 0,
            normal: HashMap::default(),
        }
    }))
}

pub const DESKTOP: u32 = 0x10010;
/// HWND_MESSAGE: the parent of message-only windows (kept as their owner)
const HWND_MESSAGE: u32 = 0xffff_fffd;
const WS_MAXIMIZE: u32 = 0x0100_0000;

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
        59 => w + 12,
        60 => h + 12,
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

/// Whether a window draws a menu bar: a top-level (not WS_CHILD) window whose menu has an item.
/// Windows draws none for an empty menu (俺の姿が､透明に!'s TVPMainWindow has one: its client is
/// the whole 1920x1080 inside its 1936x1119 window). A handle sake does not know counts as a bar.
fn menu_bar(menus: &HashMap<u32, Menu>, menu: u32, style: u32) -> bool {
    menu != 0 && style & 0x4000_0000 == 0 && menus.get(&menu).map_or(true, |m| !m.items.is_empty())
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

/// comctl32's animation control (SysAnimate32), shown still: ACM_OPEN(A/W) of a clip, ACM_PLAY
/// and ACM_STOP succeed and nothing is drawn; ACM_ISPLAYING is FALSE. (Delphi's TAnimate opens
/// its clip when created and raises 「AVI を開くことができません」 on FALSE: YU-RIS's installer)
fn animate_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("animate_proc", |c| match animate_answer(c.a(1)) {
        Some(r) => r,
        None => def_proc(c, msg_wide()),
    }, 16))
}
pub(crate) fn animate_answer(msg: u32) -> Option<u32> {
    match msg {
        // ACM_OPENA, ACM_PLAY, ACM_STOP, ACM_OPENW
        0x464..=0x467 => Some(1),
        // ACM_ISPLAYING
        0x468 => Some(0),
        _ => None,
    }
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
    /// a static's picture (STM_SETIMAGE / STM_SETICON): SS_BITMAP's HBITMAP, SS_ICON's HICON
    image: u32,
}

static CTLS: Mutex<Option<HashMap<u32, Ctl>>> = Mutex::new(None);

fn ctl(h: u32) -> Ctl {
    CTLS.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).copied().unwrap_or_default()
}
/// A button's check state (BM_GETCHECK): BST_CHECKED or indeterminate (SAKE_AUTO looks).
pub(crate) fn button_checked(h: u32) -> bool {
    ctl(h).check != 0
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
#[path = "listbox.rs"]
mod listbox;
fn listbox_proc() -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| crate::api::host_fn("listbox_proc", listbox::proc_fn, 16))
}
fn key_down(vk: usize) -> bool {
    with(|s| s.keys[vk & 0xff] & 0x80 != 0)
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
    if !button && matches!(style & 0x1f, 3 | 0xe) {
        if st.image != 0 {
            static_picture(c, dc, style, st.image, w, hh);
        }
        gcall(c, "user32", "EndPaint", &[h, ps]);
        mem::heap_free(ps);
        return;
    }
    let mut tr = full;
    let mut flags;
    let edit = with(|s| s.wins.get(&h).is_some_and(|w| edit_class(&s.classes[w.class].name)));
    if edit {
        fill(full, 0xffffff);
        frame(full, 0x7a7a7a);
        (tr, flags) = edit_text_box(style, w, hh);
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
        if matches!(kind, 4 | 9) {
            // a radio button's is a circle (タイムリープ's window/fullscreen choice, #192)
            for (y, l, r, col) in radio_spans(st.check == 1) {
                fill(Rect { l, t: by + y, r, b: by + y + 1 }, col);
            }
        } else {
            fill(b, 0xffffff);
            frame(b, 0x333333);
            if st.check == 1 {
                fill(Rect { l: 3, t: by + 3, r: 10, b: by + 10 }, 0x202020);
            }
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
    let text = match password_char(h) { Some(ch) if edit => vec![ch; text.len()], _ => text };
    tr.write(rc);
    crate::gdi::draw_text(dc, &text, rc, flags);
    gcall(c, "user32", "EndPaint", &[h, ps]);
    mem::heap_free(ps);
}

/// A radio button's 13x13 circle as row spans (row, left, right, colour): a dark ring, white
/// inside, and with `checked` a dark dot in the middle. The parent's background shows outside.
fn radio_spans(checked: bool) -> Vec<(i32, i32, i32, u32)> {
    let mut v = Vec::new();
    for y in 0..13 {
        let mut run: Option<(i32, u32)> = None;
        for x in 0..=13 {
            let col = (x < 13).then(|| {
                let (dx, dy) = (x as f32 - 6.0, y as f32 - 6.0);
                let d = (dx * dx + dy * dy).sqrt();
                if d > 6.4 {
                    None
                } else if d > 5.4 {
                    Some(0x333333)
                } else if checked && d < 3.0 {
                    Some(0x202020)
                } else {
                    Some(0xffffff)
                }
            }).flatten();
            match (run, col) {
                (Some((_, a)), Some(b)) if a == b => {}
                _ => {
                    if let Some((l, a)) = run {
                        v.push((y, l, x, a));
                    }
                    run = col.map(|c| (x, c));
                }
            }
        }
    }
    v
}

/// A static's picture (as Wine's static.c): SS_BITMAP (0xe) at its own size from the top left,
/// stretched over the control with SS_REALSIZECONTROL (0x40), centred with SS_CENTERIMAGE
/// (0x200); SS_ICON (3) drawn at its size from the top left (centred with SS_CENTERIMAGE).
fn static_picture(c: &mut Ctx, dc: u32, style: u32, image: u32, w: i32, hh: i32) {
    if style & 0x1f == 3 {
        let (x, y) = if style & 0x200 != 0 { ((w - 32) / 2, (hh - 32) / 2) } else { (0, 0) };
        gcall(c, "user32", "DrawIconEx", &[dc, x as u32, y as u32, image, 0, 0, 0, 0, 0x3 | 0x8]);
        return;
    }
    let info = mem::heap_alloc(32, true);
    gcall(c, "gdi32", "GetObjectW", &[image, 24, info]);
    let (bw, bh) = (r32(info + 4) as i32, r32(info + 8) as i32);
    mem::heap_free(info);
    let (x, y, dw, dh) = static_bitmap_rect(style, w, hh, bw, bh);
    let mdc = gcall(c, "gdi32", "CreateCompatibleDC", &[dc]);
    let old = gcall(c, "gdi32", "SelectObject", &[mdc, image]);
    gcall(c, "gdi32", "StretchBlt", &[dc, x as u32, y as u32, dw as u32, dh as u32, mdc, 0, 0, bw as u32, bh as u32, 0x00cc_0020]);
    gcall(c, "gdi32", "SelectObject", &[mdc, old]);
    gcall(c, "gdi32", "DeleteDC", &[mdc]);
}

/// Where an SS_BITMAP static draws its bitmap (bw x bh) in its client area (w x hh): x y cx cy.
pub fn static_bitmap_rect(style: u32, w: i32, hh: i32, bw: i32, bh: i32) -> (i32, i32, i32, i32) {
    if style & 0x40 != 0 {
        (0, 0, w, hh)
    } else if style & 0x200 != 0 {
        ((w - bw) / 2, (hh - bh) / 2, bw, bh)
    } else {
        (0, 0, bw, bh)
    }
}

/// A picture static made from a template whose text is a resource (a dialog item's ordinal,
/// or a name): its bitmap or icon from `inst`, as the static's WM_CREATE does on Windows.
fn static_load_picture(c: &mut Ctx, h: u32, inst: u32, res: u32) {
    let style = with(|s| s.wins.get(&h).map_or(0, |w| w.style));
    let (load, kind) = match style & 0x1f {
        0xe => ("LoadBitmapW", 0),
        3 => ("LoadIconW", 1),
        _ => return,
    };
    let img = gcall(c, "user32", load, &[inst, res]);
    if img != 0 {
        send(c, h, 0x172, kind, img);
    }
}

/// Where an edit control's text goes and the DrawText flags for it. Never a prefix (an edit
/// shows its "&" as typed). One line (the default) is centred vertically; ES_MULTILINE (4) starts
/// at the top and breaks at its CRLFs and at the right edge (YU-RIS's error dialog, a read-only
/// multiline edit, showed all its lines run together in one centred line). ES_CENTER (1) and
/// ES_RIGHT (2) align each line.
fn edit_text_box(style: u32, w: i32, hh: i32) -> (Rect, u32) {
    let align = match style & 3 {
        1 => 0x1,
        2 => 0x2,
        _ => 0,
    };
    if style & 4 != 0 {
        (Rect { l: 4, t: 2, r: w - 4, b: hh - 2 }, align | 0x10 | 0x800 | 0x2000)
    } else {
        (Rect { l: 4, t: 0, r: w - 4, b: hh }, align | 0x4 | 0x20 | 0x800)
    }
}

fn rich_stream_in(c: &mut Ctx, h: u32, format: u32, es: u32) -> u32 {
    if es == 0 {
        return 0;
    }
    // EDITSTREAM { DWORD_PTR dwCookie; DWORD dwError; EDITSTREAMCALLBACK pfnCallback; }, packed
    // to 4 (richedit.h's pshpack4): the callback at 12 on x64
    let (err_at, cb_at) = if mem::x64() { (8, 12) } else { (4, 8) };
    let (cookie, cb) = (mem::rp(es), mem::rp(es + cb_at));
    if crate::env_on!("SAKE_TRACE_INPUT") {
        eprintln!("sake: EM_STREAMIN {h:x} format {format:x} es {es:x} cookie {cookie:x} callback {cb:x}");
    }
    if cb == 0 {
        return 0;
    }
    const N: u32 = 4096;
    let buf = mem::heap_alloc(N + 4, true);
    let got = buf + N;
    let mut data = Vec::new();
    let mut err = 0;
    // (a callback that keeps giving: a license is not 64 MB)
    while data.len() < 64 << 20 {
        mem::w32(got, 0);
        err = c.call(cb, &[cookie, buf, N, got]);
        let n = r32(got).min(N);
        if err != 0 || n == 0 {
            break;
        }
        data.extend_from_slice(mem::slice(buf, n));
    }
    mem::heap_free(buf);
    mem::w32(es + err_at, err);
    let text: Vec<u16> = if format & 2 != 0 {
        crate::rtf::to_text(&data, 0)
    } else if format & 0x10 != 0 {
        data.chunks_exact(2).map(|p| u16::from_le_bytes([p[0], p[1]])).collect()
    } else {
        crate::text::mb2w(0, &data)
    };
    let n = text.len() as u32;
    with(|s| {
        if let Some(w) = s.wins.get_mut(&h) {
            if format & 0x8000 != 0 {
                w.text.extend(text);
            } else {
                w.text = text;
            }
        }
    });
    invalidate(h, None, true);
    n
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
        // BM_SETSTYLE: the button type (BS_TYPEMASK), as Wine's button.c (a push button made
        // the default one: Enter in IsDialogMessage then takes it, winref dlgenter)
        0xf4 if button => {
            with(|s| s.wins.get_mut(&h).map(|w| w.style = w.style & !0xf | wp & 0xf));
            if lp != 0 {
                invalidate(h, None, true);
            }
            0
        }
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
        // EM_STREAMIN (rich edit): the program's EDITSTREAM callback gives the text, RTF (SF_RTF 2)
        // or plain (SF_TEXT 1, SF_UNICODE 0x10: UTF-16); it replaces the text (or the selection,
        // SFF_SELECTION: sake has none, so it is appended). Returns the characters read in.
        0x449 if is_rich(h) => rich_stream_in(c, h, wp, lp),
        // EM_EXLIMITTEXT, EM_SETBKGNDCOLOR, EM_SETEVENTMASK, EM_AUTOURLDETECT: taken, nothing to do
        0x435 | 0x443 | 0x445 | 0x45b if is_rich(h) => 0,
        // the edit control: typed characters (no caret, no selection)
        0x7 | 0x8 | 0x102 | 0x201 | 0x87 if with(|s| s.wins.get(&h).is_some_and(|w| edit_class(&s.classes[w.class].name))) => {
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
        // STM_SETICON / STM_SETIMAGE: the old picture back; a bitmap static takes the bitmap's
        // size unless SS_CENTERIMAGE or SS_REALSIZECONTROL (Wine's static.c)
        0x170 | 0x172 if !button => {
            let style = with(|s| s.wins.get(&h).map_or(0, |w| w.style));
            let (kind, img) = if m == 0x170 { (1, wp) } else { (wp, lp) };
            let want = match style & 0x1f {
                0xe => 0,
                3 => 1,
                _ => return 0,
            };
            if kind != want && !(want == 1 && kind == 2) {
                return 0;
            }
            let old = ctl(h).image;
            set_ctl(h, |t| t.image = img);
            if want == 0 && img != 0 && style & 0x240 == 0 {
                let info = mem::heap_alloc(32, true);
                if gcall(c, "gdi32", "GetObjectW", &[img, 24, info]) != 0 {
                    let (bw, bh) = (r32(info + 4), r32(info + 8));
                    gcall(c, "user32", "SetWindowPos", &[h, 0, 0, 0, bw, bh, 0x2 | 0x4 | 0x10]);
                }
                mem::heap_free(info);
            }
            invalidate(h, None, true);
            old
        }
        // STM_GETICON / STM_GETIMAGE: only of the static's own kind
        0x171 | 0x173 if !button => {
            let style = with(|s| s.wins.get(&h).map_or(0, |w| w.style));
            let kind = if m == 0x171 { 1 } else { wp };
            match (style & 0x1f, kind) {
                (0xe, 0) | (3, 1 | 2) => ctl(h).image,
                _ => 0,
            }
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
            // (a single-line edit tells its parent of a WM_SETTEXT: EN_UPDATE, EN_CHANGE)
            if m == 0xc && is_edit(h) {
                let (id, parent, style) = with(|s| s.wins.get(&h).map_or((0, 0, 0), |w| (w.id, w.parent, w.style)));
                if parent != 0 && style & 4 == 0 {
                    send(c, parent, 0x111, 0x400 << 16 | (id & 0xffff), h);
                    send(c, parent, 0x111, 0x300 << 16 | (id & 0xffff), h);
                }
            }
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
    for n in ["Button", "Edit", "Static", "ListBox", "ComboBox", "ScrollBar", "#32770", "MDIClient", "ComboLBox", "#32768", "msctls_progress32", "SysListView32", "SysTreeView32", "msctls_statusbar32", "ToolbarWindow32", "tooltips_class32", "RichEdit20A", "RichEdit20W", "RICHEDIT50W", "IME", "SysTabControl32", "msctls_trackbar32", "msctls_updown32", "SysAnimate32"] {
        let atom = crate::k32::atom(n);
        s.classes.push(Class {
            name: n.into(),
            style: 0,
            proc_: match n {
                "Button" | "Static" | "Edit" | "RichEdit20A" | "RichEdit20W" | "RICHEDIT50W" => control_proc(),
                "SysTabControl32" => tab_proc(),
                "ComboBox" => combo_proc(),
                "ListBox" => listbox_proc(),
                "SysAnimate32" => animate_proc(),
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
    send_to(cpu, hwnd, msg, wp, lp, true)
}
/// A message to `hwnd`'s procedure: sent (`hooks`: past the WH_CALLWNDPROC hooks), or a posted
/// one DispatchMessage hands it (no hooks)
fn send_to(cpu: &mut koji::Cpu, hwnd: u32, msg: u32, wp: u32, lp: u32, hooks: bool) -> u32 {
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
    if !hooks {
        return call_proc(cpu, proc_, hwnd, msg, wp, lp);
    }
    call_sent(cpu, proc_, hwnd, msg, wp, lp, true)
}

/// Calls a window procedure. A window whose procedure was set to NULL (SetWindowLongPtr
/// GWLP_WNDPROC 0) answers 0 without a call, as in Wine: the Flash projector does that before
/// DestroyWindow, whose WM_DESTROY then called address 0 (オヤジ狩り少女 体験版).
fn call_proc(cpu: &mut koji::Cpu, proc_: u32, hwnd: u32, msg: u32, wp: u32, lp: u32) -> u32 {
    if proc_ == 0 {
        return 0;
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
            if let Some((v, at, gate)) = *g {
                drop(g);
                if let Some(d) = at.checked_duration_since(Instant::now()) {
                    std::thread::sleep(d);
                }
                if let Some(m) = gate {
                    let passed = crate::obj::HANDOFF.wait_worked(m, crate::cur().tid, waits::handoff::WORK_CAP);
                    if crate::env_on!("SAKE_TRACE_HANDOFF") {
                        eprintln!("sake: SendMessage of {:04x} waited for {:04x}{}", crate::cur().tid, m.tid, if passed { "" } else { " (ran out)" });
                    }
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
/// thread's window, and the second one got slot 1, which it takes for an error. 4 ms was
/// enough on an idle machine but not on a loaded one (the mirror trial sweep: アパタイト's
/// KiriKiri trials failed 1 start in 8 alone and up to half with two games running; sendrace
/// failed 1-2 rounds in 12): the first thread has to be scheduled and run its next few calls.
const HEAD_START: Duration = Duration::from_millis(25);

/// Whom the sender `from` waits for besides its time: the sender let go before it, `last`
/// (its work counter as this thread answered it), when that is another thread of this process.
/// That one has to reach a call that does something first (waits::handoff, #209-5): a head
/// start alone was not enough on a loaded machine, where the first may not even be scheduled
/// in 25 ms.
fn release_gate(last: Option<(u32, Instant, waits::handoff::Mark)>, from: u32) -> Option<waits::handoff::Mark> {
    match last {
        Some((t, _, m)) if t != from && t != 0 && from != 0 => Some(m),
        _ => None,
    }
}

/// When the sender `from` of a message answered `now` goes on, `last` being the sender this
/// thread let go before it (and when): another thread waits a head start after that one.
fn release_at(last: Option<(u32, Instant)>, from: u32, now: Instant) -> Instant {
    match last {
        Some((t, at)) if t != from => (at + HEAD_START).max(now),
        // (never ahead of the one before)
        Some((_, at)) => at.max(now),
        None => now,
    }
}

thread_local! {
    /// The sender this thread let go last, and when. Kept across pumps and nested ones: a busy
    /// owner that peeks between bits of work (KiriKiri's main loop) answers two waiting senders
    /// in two PeekMessage calls microseconds apart, and a window procedure that pumps answers
    /// the second inside the first one's call; both must still go on a head start apart (#82).
    static LAST_RELEASE: std::cell::Cell<Option<(u32, Instant, waits::handoff::Mark)>> = const { std::cell::Cell::new(None) };
}

/// Runs messages other threads sent to this one.
fn process_sent(cpu: &mut koji::Cpu) {
    let me = crate::cur().tid;
    loop {
        let s = with(|s| s.queues.get_mut(&me).and_then(|q| q.sent.pop_front()));
        let Some(sent) = s else { return };
        let m = sent.msg;
        let proc_ = with(|s| s.wins.get(&m.hwnd).map(|w| w.proc_));
        let r = match proc_ {
            Some(p) => call_sent(cpu, p, m.hwnd, m.msg, m.w, m.l, false),
            None => 0,
        };
        let last = LAST_RELEASE.get();
        let at = release_at(last.map(|(t, at, _)| (t, at)), sent.from, Instant::now());
        let gate = release_gate(last, sent.from);
        // (marked before it can go on: its next call that does something comes after)
        LAST_RELEASE.set(Some((sent.from, at, crate::obj::HANDOFF.work_mark(sent.from))));
        *sent.result.0.lock().unwrap() = Some((r, at, gate));
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
    let (parent, mut menu, inst, param) = (c.a(8), c.a(9), c.a(10), c.a(11));
    let tid = crate::cur().tid;
    let child = style & 0x4000_0000 != 0;
    // the class menu (WNDCLASS lpszMenuName): a top-level window without its own gets a copy,
    // loaded from the window's instance (as Wine's WIN_CreateWindowEx; a missing resource
    // leaves it without one). The engine of エピデミック 〜感染拡大〜 inserts its items into it.
    if !child && menu == 0 {
        let cm = with(|s| {
            system_classes(s);
            class_index(s, cls_p, wide).map(|ci| (s.classes[ci].menu, s.classes[ci].inst, s.classes[ci].unicode))
        });
        if let Some((name, kinst, uni)) = cm.filter(|k| k.0 != 0) {
            menu = resource(if inst != 0 { inst } else { kinst }, name, 4, uni).map_or(0, menu_from_resource);
        }
    }
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
        let has_menu = !child && menu_bar(&s.menus, menu, 0);
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
    // MINMAXINFO for WM_GETMINMAXINFO: the size kept within its tracking sizes (Wine's
    // CreateWindowEx: top-level windows only)
    let mmi = cs + if x64 { 0x60 } else { 0x40 };
    if !child {
        grow_screen_for(style, (x, y), (cx, cy));
        minmax_defaults(mmi);
        send(c, h, 0x24, 0, mmi);
        let v = |k: u32| r32(mmi + 4 * k) as i32;
        let (ncx, ncy) = track_clamp(style, (cx, cy), (v(6), v(7)), (v(8), v(9)));
        if (ncx, ncy) != (cx, cy) {
            if crate::env_on!("SAKE_TRACE_WIN") {
                eprintln!("sake: CreateWindowEx {h:x}: {cx}x{cy} kept to {ncx}x{ncy}");
            }
            (cx, cy) = (ncx, ncy);
            with(|s| {
                if let Some(w) = s.wins.get_mut(&h) {
                    w.rect = Rect { l: x, t: y, r: x + cx, b: y + cy };
                    let has_menu = menu_bar(&s.menus, w.menu, w.style);
                    w.client = client_of(&w.rect, w.style, w.exstyle, has_menu);
                    resize_surface(w);
                }
            });
            // (CREATESTRUCT's cx, cy: what the window got)
            if x64 {
                w32(cs + 36, cx as u32);
                w32(cs + 32, cy as u32);
            } else {
                w32(cs + 20, cx as u32);
                w32(cs + 16, cy as u32);
            }
        }
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
    // The compositor takes ownership of a caption's non-client rendering even
    // for a hidden window. Windows posts WM_DWMNCRENDERINGCHANGED here (wParam
    // TRUE), leaving input available to MsgWaitForMultipleObjects before ShowWindow.
    // Reference: Windows startup probe, actions/runs/42493375958; the notification
    // describes our composed window frame, independently of any guest wait.
    if !child && style & 0x00c0_0000 == 0x00c0_0000 {
        post(h, 0x31f, 1, 0);
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
        s.normal.remove(&h);
        if let Some(w) = s.wins.remove(&h) {
            // (a top-level window's menu goes with it, as DestroyWindow does on Windows)
            if w.parent == 0 && w.style & 0x4000_0000 == 0 && w.menu != 0 {
                s.menus.remove(&w.menu);
            }
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
    let top = with(|s| s.wins.get(&h).filter(|w| w.parent == 0 && w.style & 0x4000_0000 == 0).map(|w| (w.style, w.exstyle, menu_bar(&s.menus, w.menu, w.style))));
    // (WS_MAXIMIZE set, the rect before kept: SW_RESTORE / SW_SHOWNORMAL / SC_RESTORE put it
    // back; WM_SIZE says SIZE_MAXIMIZED)
    if let (3, Some((style, ex, menu))) = (cmd, top) {
        let (l, t, r, b) = frame(style, ex, menu);
        let (sw, sh) = screen();
        with(|s| {
            if let Some(w) = s.wins.get_mut(&h) {
                if w.style & WS_MAXIMIZE == 0 {
                    let r = w.rect;
                    w.style |= WS_MAXIMIZE;
                    s.normal.insert(h, r);
                }
            }
        });
        set_pos(c, h, 0, -l, -t, sw + l + r, sh + t + b, 0x14);
    }
    if matches!(cmd, 1 | 9) && top.is_some_and(|t| t.0 & WS_MAXIMIZE != 0) {
        let back = with(|s| {
            s.wins.get_mut(&h).map(|w| w.style &= !WS_MAXIMIZE);
            s.normal.remove(&h)
        });
        if let Some(r) = back {
            set_pos(c, h, 0, r.l, r.t, r.w(), r.h(), 0x14);
        }
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
        // (WM_NCACTIVATE first, then WM_ACTIVATE, as Wine's set_active_window and its message
        // sequence tests have it)
        if old != 0 {
            send(c, old, 0x86, 0, h);
            send(c, old, 0x6, 0, h);
        }
        // WM_ACTIVATEAPP when the activation moves to another thread (or comes from no window):
        // to every top-level window of both threads, as on Windows (games run only while the
        // app is active; Delphi's Application.OnActivate needs it at TApplication's own hidden
        // window: crate::activation)
        let (old_tid, new_tid, tops) = with(|s| {
            let tid = |h: u32| s.wins.get(&h).map(|w| w.tid);
            let mut tops: Vec<crate::activation::Top> =
                s.wins.iter().filter(|(&k, w)| k != DESKTOP && w.parent == 0).map(|(&k, w)| (k, w.tid, w.owner == HWND_MESSAGE)).collect();
            // (Z order approximated as newest first, as FindWindow lists them)
            tops.sort_unstable_by(|a, b| b.0.cmp(&a.0));
            (if old != 0 { tid(old) } else { None }, tid(h), tops)
        });
        if let Some(new_tid) = new_tid {
            let me = crate::cur().tid;
            for (w, wp, lp) in crate::activation::app_messages(old_tid, new_tid, &tops) {
                match with(|s| s.wins.get(&w).map(|x| x.tid)) {
                    Some(t) if t == me => {
                        send(c, w, 0x1c, wp, lp);
                    }
                    // (another thread's windows: queued like SendNotifyMessage, not waited for)
                    Some(t) => notify_thread(t, Msg { hwnd: w, msg: 0x1c, w: wp, l: lp, time: now_ms(), ..Default::default() }),
                    None => {}
                }
            }
        }
        send(c, h, 0x86, 1, old);
        send(c, h, 0x6, 1, old);
        // (the focus moves to the window unless it is already inside it, as Wine's
        // set_active_window: WM_KILLFOCUS at the old one's window, WM_SETFOCUS here)
        let f = with(|s| s.focus);
        if f == 0 || !is_window(f) || with(|s| root_of(s, f)) != h {
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
    // (Windows paints only what is on screen: a hidden window, or one inside a hidden parent,
    // gets no WM_PAINT however invalid it is; Malie's twitter.dll creates its tweet window
    // hidden, calls UpdateWindow, and its WM_PAINT needs a picture loaded only when shown)
    let dirty = with(|s| paint_due(s, h));
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

/// Whether Windows keeps window `style`'s size within its WM_GETMINMAXINFO tracking sizes
/// (CreateWindowEx, DefWindowProc's WM_WINDOWPOSCHANGING): a sizing frame, or an overlapped
/// window (neither WS_POPUP nor WS_CHILD).
fn tracks(style: u32) -> bool {
    style & 0x0004_0000 != 0 || style & 0xc000_0000 == 0
}

/// `(cx, cy)` kept within the tracking sizes `min` and `max`: at most `max`, then (unless
/// minimized) at least `min`, as Wine's WINPOS_HandleWindowPosChanging. A window asked bigger
/// than the screen (KiriKiri's 1920x1080 on sake's 1366x768) is made to fit, as on Windows,
/// instead of reaching past the screen's edge (the page then showed only part of it).
fn track_clamp(style: u32, (cx, cy): (i32, i32), min: (i32, i32), max: (i32, i32)) -> (i32, i32) {
    if !tracks(style) {
        return (cx, cy);
    }
    let (mut cx, mut cy) = (cx.min(max.0), cy.min(max.1));
    if style & 0x2000_0000 == 0 {
        cx = cx.max(min.0);
        cy = cy.max(min.1);
    }
    (cx, cy)
}

/// The screen a top-level window at (`x`, `y`) asking `cx` x `cy` needs, when that is more than
/// the display mode's tracking size (`mode` and 12 pixels): `screen` (the mode, or what it grew
/// to) grown to hold it where it is (8192 at most). A big window moved partly off the screen
/// grows it too (俺の姿が､透明に! puts its window back where the real PC's desktop had it, from
/// its saved settings: (391, 239), then only the top-left 1577x869 of its 1920x1080 showed);
/// a small one does not (a window moved off the screen to hide it). sake's screen is no monitor: a game that asks for a window bigger than it (KiriKiri
/// Z's 1920x1080 window with its frame, 俺の姿が､透明に!) gets a screen it fits on, not a window
/// kept to the screen and drawn into at full size (the page then showed its top-left part
/// enlarged). None: it fits, it does not track (a popup), or the screen was given
/// (IPPAI_SCREEN: winref's probes keep Windows' clamp).
fn grown_screen(mode: (i32, i32), screen: (i32, i32), style: u32, (x, y): (i32, i32), (cx, cy): (i32, i32)) -> Option<(i32, i32)> {
    let (sw, sh) = screen;
    if !tracks(style) || (cx <= mode.0 + 12 && cy <= mode.1 + 12) {
        return None;
    }
    let w = sw.max(x.max(0).saturating_add(cx)).min(8192);
    let h = sh.max(y.max(0).saturating_add(cy)).min(8192);
    Some((w, h)).filter(|&g| g != screen)
}

/// Grows the screen for a top-level window asking `cx` x `cy` at (`x`, `y`) (see
/// [`grown_screen`]); no WM_DISPLAYCHANGE: the screen was this big all along.
fn grow_screen_for(style: u32, xy: (i32, i32), size: (i32, i32)) {
    if std::env::var_os("IPPAI_SCREEN").is_some() {
        return;
    }
    if let Some((w, h)) = grown_screen(with(|s| s.mode), screen(), style, xy, size) {
        if crate::env_on!("SAKE_TRACE_WIN") {
            eprintln!("sake: screen grown to {w}x{h} for a {}x{} window", size.0, size.1);
        }
        kura::display::vnhost_display_set_size(w, h);
        with(|s| s.screen = (w, h));
    }
}

/// MINMAXINFO's defaults for a top-level window on this screen, written at `p` (40 bytes).
fn minmax_defaults(p: u32) {
    let (sw, sh) = screen();
    for (k, v) in [0, 0, sw, sh, 0, 0, metric(34), metric(35), metric(59), metric(60)].iter().enumerate() {
        w32(p + 4 * k as u32, *v as u32);
    }
}

/// WM_GETMINMAXINFO to `h`: its (min, max) tracking sizes.
fn minmax(c: &mut Ctx, h: u32) -> ((i32, i32), (i32, i32)) {
    let p = mem::heap_alloc(40, true);
    minmax_defaults(p);
    send(c, h, 0x24, 0, p);
    let v = |k: u32| r32(p + 4 * k) as i32;
    let r = ((v(6), v(7)), (v(8), v(9)));
    mem::heap_free(p);
    r
}

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
        let has_menu = menu_bar(&s.menus, w.menu, w.style);
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
        // (SIZE_MAXIMIZED for a maximized window)
        let how = with(|s| s.wins.get(&h).is_some_and(|w| w.style & WS_MAXIMIZE != 0)) as u32 * 2;
        send(c, h, 0x5, how, (cl.h() as u32) << 16 | (cl.w() as u32 & 0xffff));
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
/// WINDOWINFO (60 bytes, the same on x64): the window's and client's rectangles in screen
/// coordinates, styles, WS_ACTIVECAPTION for the active window, the border widths as the
/// gaps between the two rectangles (left and bottom, as Windows works them out), the class
/// atom and creator version 0x400. cbSize is not checked (Windows fills it whatever it was).
pub fn window_info(win: Rect, client: Rect, style: u32, exstyle: u32, active: bool, atom: u16) -> [u8; 60] {
    let mut b = [0u8; 60];
    let mut put = |o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(0, 60);
    for (o, v) in [(4, win.l), (8, win.t), (12, win.r), (16, win.b), (20, client.l), (24, client.t), (28, client.r), (32, client.b)] {
        put(o, v as u32);
    }
    put(36, style);
    put(40, exstyle);
    put(44, active as u32);
    put(48, (client.l - win.l) as u32);
    put(52, (win.b - client.b) as u32);
    put(56, atom as u32 | 0x0400 << 16);
    b
}
fn GetWindowInfo(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    let info = with(|s| {
        let w = s.wins.get(&h)?;
        let (x, y) = window_origin(s, h);
        let win = Rect { l: x, t: y, r: x + w.rect.w(), b: y + w.rect.h() };
        let atom = s.classes[w.class].atom as u16;
        Some((win, client_screen(s, h), w.exstyle, s.active == h, atom))
    });
    let Some((win, client, exstyle, active, atom)) = info else {
        w32(crate::cur().teb + crate::teb_last_error(), 1400);
        return 0;
    };
    if p == 0 {
        w32(crate::cur().teb + crate::teb_last_error(), 998);
        return 0;
    }
    // (the style as GetWindowLong reports it: WS_VISIBLE follows the window's visibility)
    let style = get_long(h, -16, true);
    for (i, v) in window_info(win, client, style, exstyle, active, atom).iter().enumerate() {
        crate::mem::w8(p + i as u32, *v);
    }
    1
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
        } else if old == 0 || old == DESKTOP {
            // a top-level window made a child leaves the screen's windows: it is drawn in its
            // parent's now (タイムリープ's D3D9 window, shown, then put in its menu dialog: the
            // screen kept it under the dialog and the frames under the dialog's gray, #192)
            unsafe { kura::display::vnhost_window_pos(h, [0i32; 8].as_ptr(), 0, 0, 0) };
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
    const MESSAGE: u32 = HWND_MESSAGE;
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
                let has_menu = menu_bar(&s.menus, w.menu, w.style);
                w.client = client_of(&w.rect, w.style, w.exstyle, has_menu);
                resize_surface(w);
                update_display(s, h);
                old
            }
            -20 => {
                let old = w.exstyle;
                w.exstyle = v;
                let has_menu = menu_bar(&s.menus, w.menu, w.style);
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
/// ERROR_INVALID_INDEX. Success leaves the last error as it was (winref user: Windows keeps
/// the caller's 0xdead after a style change too).
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

const PASSWORD_CHAR: &str = "sake:passwordchar";
/// The edit control and the rich edit controls (RichEdit20A/W, RICHEDIT50W): sake draws a rich
/// edit as a plain edit of its text (EM_STREAMIN's RTF reduced by crate::rtf, #164).
fn edit_class(name: &str) -> bool {
    ["edit", "richedit20a", "richedit20w", "richedit50w"].iter().any(|n| name.eq_ignore_ascii_case(n))
}
fn is_rich(h: u32) -> bool {
    with(|s| s.wins.get(&h).is_some_and(|w| s.classes[w.class].name.to_ascii_lowercase().starts_with("richedit")))
}
fn is_edit(h: u32) -> bool {
    with(|s| s.wins.get(&h).is_some_and(|w| edit_class(&s.classes[w.class].name)))
}
/// The character an edit control draws for each one of its text (None: drawn as typed).
fn password_char(h: u32) -> Option<u16> {
    with(|s| {
        let w = s.wins.get(&h)?;
        if w.style & 0x20 == 0 { return None; }
        Some(w.props.iter().find(|p| p.0 == PASSWORD_CHAR).map_or(b'*' as u16, |p| p.1 as u16))
    })
}
fn def_proc(c: &mut Ctx, proc_wide: bool) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    // text parameters are in the sender's encoding (sake translates no messages), whichever of
    // DefWindowProcA/W the window procedure calls
    let wide = msg_wide();
    match m {
        // EM_SETPASSWORDCHAR / EM_GETPASSWORDCHAR (0: shown as typed; ES_PASSWORD's own is '*',
        // as comctl32 5's)
        0xcc | 0xd2 if is_edit(h) => {
            if m == 0xd2 {
                return password_char(h).unwrap_or(0) as u32;
            }
            with(|s| {
                if let Some(w) = s.wins.get_mut(&h) {
                    w.props.retain(|p| p.0 != PASSWORD_CHAR);
                    w.props.push((PASSWORD_CHAR.into(), wp & 0xffff));
                    if wp & 0xffff == 0 { w.style &= !0x20 } else { w.style |= 0x20 }
                }
            });
            invalidate(h, None, true);
            0
        }
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
            if let Some((parent, id)) = with(|s| s.wins.get(&h).map(|w| (w.parent, w.id))) {
                sdrt_login::status(parent, id, &t);
            }
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
        // WM_PRINT(dc, flags): PRF_NONCLIENT (2) draws the non-client edge sake has
        // (client_edge) into the DC at the window's own coordinates; PRF_CHECKVISIBLE (1)
        // draws nothing for a hidden window. The client area is not drawn (no WM_PRINTCLIENT).
        0x317 => {
            let Some((rect, style, ex, visible)) = with(|s| s.wins.get(&h).map(|w| (w.rect, w.style, w.exstyle, w.visible))) else { return 0 };
            if lp & 1 != 0 && !visible {
                return 0;
            }
            if lp & 2 != 0 {
                for (r, color) in client_edge(style, ex, rect.w(), rect.h()) {
                    crate::gdi::fill_rect_brush(wp, r, crate::gdi::solid_brush(sys_color(color)));
                }
            }
            0
        }
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
        0x46 => {
            // (the size kept within the window's tracking sizes, as Windows')
            let (x, y, cx, cy, f) = windowpos_read(lp);
            let style = with(|s| s.wins.get(&h).map(|w| w.style));
            // (a move alone too: a big window moved partly off the screen grows it)
            if let Some(style) = style.filter(|&st| f & 3 != 3 && tracks(st)) {
                let (at, size) = with(|s| s.wins.get(&h).map(|w| ((w.rect.l, w.rect.t), (w.rect.r - w.rect.l, w.rect.b - w.rect.t)))).unwrap_or_default();
                grow_screen_for(style, if f & 2 == 0 { (x, y) } else { at }, if f & 1 == 0 { (cx, cy) } else { size });
            }
            if let Some(style) = style.filter(|&st| f & 1 == 0 && tracks(st)) {
                let (min, max) = minmax(c, h);
                let (ncx, ncy) = track_clamp(style, (cx, cy), min, max);
                if (ncx, ncy) != (cx, cy) {
                    let ps = mem::ps();
                    let (hw, after) = (mem::rp(lp), mem::rp(lp + ps));
                    windowpos_write(lp, hw, after, x, y, ncx, ncy, f);
                }
            }
            0
        }
        0x47 => {
            let (.., f) = windowpos_read(lp);
            send_move_size(c, h, f & SWP_NOCLIENTMOVE == 0, f & SWP_NOCLIENTSIZE == 0);
            0
        }
        0x24 => 0,
        0x83 => 0,
        // WM_SYSCOMMAND: SC_CLOSE closes; SC_MAXIMIZE and SC_RESTORE as ShowWindow's
        // (KiriKiri's windowEx plugin restores a saved maximized window with a posted
        // SC_MAXIMIZE: AMBITIOUS MISSION after episode 1, whose swap chain then takes the
        // maximized size); SC_MINIMIZE is not done (the page shows the game)
        0x112 => {
            match wp & 0xfff0 {
                0xf060 => {
                    send(c, h, 0x10, 0, 0);
                }
                0xf030 => {
                    show(c, h, 3);
                }
                0xf120 => {
                    show(c, h, 9);
                }
                _ => {}
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
/// SendNotifyMessage(hwnd, msg, wp, lp): to a window of this thread as SendMessage; to another
/// thread's as a sent message (handled before its posted ones) without waiting for it; to
/// another process's window posted; HWND_BROADCAST to every top-level window
fn send_notify(c: &mut Ctx, wide: bool) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let me = crate::cur().tid;
    let targets: Vec<u32> = if h == 0xffff { with(|s| s.wins.iter().filter(|(_, w)| w.parent == 0).map(|(&h, _)| h).collect()) } else { vec![h] };
    for t in targets {
        match with(|s| s.wins.get(&t).map(|w| w.tid)) {
            Some(tid) if tid == me => {
                as_wide(wide, || send(c, t, m, wp, lp));
            }
            Some(tid) => notify_thread(tid, Msg { hwnd: t, msg: m, w: wp, l: lp, time: now_ms(), ..Default::default() }),
            None => {
                if !post(t, m, wp, lp) {
                    crate::k32::last_error(1400);
                    return 0;
                }
            }
        }
    }
    1
}
/// A message sent to thread `tid`'s window without waiting for it (SendNotifyMessage).
fn notify_thread(tid: u32, msg: Msg) {
    let result = Arc::new((Mutex::new(None), std::sync::Condvar::new()));
    with(|s| {
        let q = s.queues.entry(tid).or_default();
        q.sent.push_back(Sent { msg, result, from: 0, remote: None });
        q.fresh |= 0x40;
    });
    crate::obj::signal();
}
fn SendNotifyMessageA(c: &mut Ctx) -> u32 {
    send_notify(c, false)
}
fn SendNotifyMessageW(c: &mut Ctx) -> u32 {
    send_notify(c, true)
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
        // TrackMouseEvent's hover (generated like a timer's message, then posted)
        if remove {
            let q = s.queues.get_mut(&me).unwrap();
            let (t, e) = crate::mousetrack::tick(q.track, track_ms());
            q.track = t;
            if let Some(e) = e {
                track_post(s, e);
                let q = s.queues.get_mut(&me).unwrap();
                if let Some(i) = q.posted.iter().position(|m| matches(m, hwnd, lo, hi)) {
                    return q.posted.remove(i);
                }
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
        let hover = crate::mousetrack::next_due(q.track).map(|d| Duration::from_millis(d.saturating_sub(track_ms())));
        q.timers.iter().map(|t| t.due.saturating_duration_since(now)).chain(hover).min()
    })
}

fn has_work() -> bool {
    has_work_of(QS_ALL)
}
const QS_ALL: u32 = 0x4ff;
/// Whether thread `me` has messages of the QS_* kinds in `mask`: input (key, mouse, raw input,
/// pointer 0x1407), posted (0x108), paint (0x20), sent (0x40). (Delphi's TThread.WaitFor waits for
/// QS_SENDMESSAGE only and peeks without removing: a pending paint must not wake it.)
fn queued(s: &State, me: u32, mask: u32) -> bool {
    s.queues.get(&me).is_some_and(|q| {
        mask & 0x108 != 0 && (!q.posted.is_empty() || q.quit.is_some())
            || mask & 0x1407 != 0 && !q.input.is_empty()
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
            getmsg_hooks(c.cpu, p, true);
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
            getmsg_hooks(c.cpu, p, flags & 1 != 0);
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
    with(|s| s.queues.get(&me).is_some_and(|q| q.timers.iter().any(|t| t.due <= now) || crate::mousetrack::next_due(q.track).is_some_and(|d| d <= track_ms())))
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
        // Round remaining fractional milliseconds up: truncation made a 1 ms
        // wait return immediately and turned startup waits into busy loops.
        let left = deadline.map_or(0xffff_ffff, |d| d.saturating_duration_since(Instant::now()).as_nanos().div_ceil(1_000_000) as u32);
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
                0x245..=0x24f => 0x1000, // QS_POINTER
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
    if ((0x200..=0x20a).contains(&m) && m != 0x200 || (0x100..=0x109).contains(&m) || m == 0xff) && crate::env_on!("SAKE_TRACE_INPUT") {
        eprintln!("sake input: dispatch {h:x} msg {m:x} {wp:x} {lp:x}");
    }
    let r = send_to(c.cpu, h, m, wp, lp, false);
    if ((0x200..=0x20a).contains(&m) && m != 0x200 || (0x100..=0x109).contains(&m) || m == 0xff) && crate::env_on!("SAKE_TRACE_INPUT") {
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
fn next_dialog_tab(dlg: u32, control: u32, previous: bool) -> u32 {
    with(|s| {
        fn visit(s: &State, parent: u32, list: &mut Vec<(u32, bool)>, depth: u32) {
            if depth > 64 { return; }
            let Some(parent) = s.wins.get(&parent) else { return };
            for &h in &parent.children {
                let Some(w) = s.wins.get(&h) else { continue };
                let enabled = w.visible && w.enabled;
                list.push((h, enabled && w.style & 0x10000 != 0));
                if enabled && w.exstyle & 0x10000 != 0 { visit(s, h, list, depth + 1); }
            }
        }
        let mut list = Vec::new(); visit(s, dlg, &mut list, 0);
        if list.is_empty() { return 0; }
        let n = list.len();
        let start = if control == 0 { if previous { 0 } else { n - 1 } }
            else { match list.iter().position(|x| x.0 == control) { Some(i) => i, None => return 0 } };
        for offset in 1..=n {
            let i = if previous { (start + n - offset) % n } else { (start + offset) % n };
            if list[i].1 { return list[i].0; }
        }
        0
    })
}
/// GetNextDlgTabItem(dlg, ctrl, previous): the dialog itself as ctrl is no control; no control
/// searching backwards finds nothing (winref host_dialogtab: Windows answers NULL, as Wine's
/// tests on Windows 2000 found), forwards the first tab stop.
fn GetNextDlgTabItem(c: &mut Ctx) -> u32 {
    let (dlg, ctrl, previous) = (c.a(0), c.a(1), c.a(2) != 0);
    let ctrl = if ctrl == dlg { 0 } else { ctrl };
    if ctrl == 0 && previous {
        return 0;
    }
    next_dialog_tab(dlg, ctrl, previous)
}

/// IsDialogMessage(dlg, msg): Enter / Escape pressed in the dialog become IDOK / IDCANCEL; its
/// other messages are translated and dispatched (TRUE); messages of other windows are FALSE.
fn is_dialog_message(c: &mut Ctx, wide: bool) -> u32 {
    let (dlg, msg) = (c.a(0), c.a(1));
    let (h, m, key, _) = msg_at(msg);
    if (0x100..=0x109).contains(&m) && crate::env_on!("SAKE_TRACE_INPUT") {
        eprintln!("sake input: IsDialogMessage({dlg:x}) {h:x} msg {m:x} key {key:x}");
    }
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
    if m == 0x100 && key == 9 {
        let code = send(c, h, 0x87, key, msg); // WM_GETDLGCODE
        if code & 6 == 0 {
            let previous = key_states()[0x10] & 0x80 != 0;
            let next = next_dialog_tab(dlg, h, previous);
            if next != 0 {
                set_focus(c, next);
                if send(c, next, 0x87, 0, 0) & 8 != 0 { send(c, next, 0xb1, 0, u32::MAX); }
            }
            return 1;
        }
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
    let (id, b) = if key == 0x0d { default_button(c, dlg) } else { (2, dlg_item(dlg, 2)) };
    let f = crate::api::export("user32", if wide { "SendMessageW" } else { "SendMessageA" });
    let r = c.call(f, &[dlg, 0x111, id, b]);
    if crate::env_on!("SAKE_TRACE_INPUT") {
        eprintln!("sake input: IsDialogMessage({dlg:x}) WM_COMMAND {id:x} {b:x} -> {r:x}");
    }
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
fn shown_tree(s: &State, h: u32) -> bool {
    shown_chain(|h| s.wins.get(&h).map(|w| (w.visible, w.parent)), h)
}

/// Whether window `h` and every parent up to its top-level window are visible (`get`: a
/// window's visible flag and parent). At most 4096 levels: a parent cycle is not shown.
fn shown_chain(get: impl Fn(u32) -> Option<(bool, u32)>, mut h: u32) -> bool {
    for _ in 0..4096 {
        match get(h) {
            Some((true, parent)) => {
                if parent == 0 || parent == DESKTOP {
                    return true;
                }
                h = parent;
            }
            _ => return false,
        }
    }
    false
}

/// A WM_PAINT is due: the window has an update region and is on screen (UpdateWindow,
/// RedrawWindow's RDW_UPDATENOW and the queue's generated WM_PAINT all ask this).
fn paint_due(s: &State, h: u32) -> bool {
    s.wins.get(&h).is_some_and(|w| !w.update.empty()) && shown_tree(s, h)
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
        let dirty = with(|s| paint_due(s, h));
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
/// The 3D edge of a window's non-client area that sake draws (in the window's coordinates,
/// width x height): WS_EX_CLIENTEDGE's sunken edge, 2 pixels just outside the client area
/// (inside any border), as DrawEdge(EDGE_SUNKEN, BF_RECT) draws it: shadow and dark shadow at
/// the top and left, highlight and light at the bottom and right. Empty without the style.
pub fn client_edge(style: u32, exstyle: u32, w: i32, h: i32) -> Vec<(Rect, u32)> {
    if exstyle & 0x200 == 0 {
        return Vec::new();
    }
    let (l, t, r, b) = frame(style, exstyle, false);
    let rc = Rect { l: l - 2, t: t - 2, r: w - (r - 2), b: h - (b - 2) };
    if rc.empty() {
        return Vec::new();
    }
    edge_parts(rc, 0xa, 0xf).0
}

/// A child window's non-client edge (client_edge), drawn in its parent's client area where
/// the window is: Windows' WM_NCPAINT of it. Top-level windows' frames are not on their
/// surfaces. The old KiriKiri 2's and アトリエさくら's drawing child has WS_EX_CLIENTEDGE: a
/// 2-pixel black frame round the game without this (#215).
fn paint_client_edge(h: u32) {
    let Some((parent, rect, style, ex)) = with(|s| s.wins.get(&h).map(|w| (w.parent, w.rect, w.style, w.exstyle))) else { return };
    if parent == 0 || parent == DESKTOP {
        return;
    }
    let parts = client_edge(style, ex, rect.w(), rect.h());
    if parts.is_empty() {
        return;
    }
    let dc = crate::gdi::get_dce(parent, dc_info(parent), 0, crate::gdi::DCX_CACHE);
    if dc == 0 {
        return;
    }
    for (r, color) in parts {
        crate::gdi::fill_rect_brush(dc, r.off(rect.l, rect.t), crate::gdi::solid_brush(sys_color(color)));
    }
    crate::gdi::release_dce(dc);
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
    paint_client_edge(h);
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
/// SetFocus(window): as Windows (and Wine's NtUserSetFocus): a minimized or disabled window (or
/// one inside such a parent) takes no focus; a window whose top-level window is not the active
/// one activates it first, so WM_ACTIVATE comes before WM_SETFOCUS (Unity 6 sets the focus on its
/// hidden window before showing it: sake sent WM_SETFOCUS alone, the activation at ShowWindow)
fn SetFocus(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h != 0 {
        if !is_window(h) {
            crate::k32::last_error(1400);
            return 0;
        }
        let Some(top) = with(|s| focus_top(s, h)) else { return 0 };
        if with(|s| s.focus) == h {
            return h;
        }
        if with(|s| s.active) != top {
            activate(c, top);
            if !is_window(h) || with(|s| s.active) != top {
                return 0;
            }
        }
    }
    set_focus(c, h)
}
/// The top-level window `h` is in (GetAncestor GA_ROOT)
fn root_of(s: &State, h: u32) -> u32 {
    let mut t = h;
    while let Some(w) = s.wins.get(&t) {
        if w.parent == 0 || w.parent == DESKTOP || !s.wins.contains_key(&w.parent) {
            break;
        }
        t = w.parent;
    }
    t
}
/// The top-level window SetFocus(h) activates, or None when h can't take the focus (it or a
/// parent minimized or disabled)
fn focus_top(s: &State, h: u32) -> Option<u32> {
    let mut t = h;
    loop {
        let w = s.wins.get(&t)?;
        if w.style & (0x2000_0000 | 0x0800_0000) != 0 {
            return None;
        }
        if w.parent == 0 || w.parent == DESKTOP || !s.wins.contains_key(&w.parent) {
            return Some(t);
        }
        t = w.parent;
    }
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
            0 => mouse_inject(r32(u) as i32, r32(u + 4) as i32, r32(u + 8), r32(u + 12)),
            1 => {
                let (vk, scan, flags) = (r16(u) as u32, r16(u + 2) as u32, r32(u + 4));
                if flags & 4 != 0 {
                    continue; // KEYEVENTF_UNICODE: no key
                }
                key_inject(vk, scan, flags);
            }
            _ => {}
        }
    }
    crate::obj::signal();
    n
}
/// A mouse event as SendInput's MOUSEINPUT and mouse_event give it: MOUSEEVENTF_MOVE (dx, dy
/// relative, or with MOUSEEVENTF_ABSOLUTE 0..65535 over the screen), the buttons' down/up
/// bits, MOUSEEVENTF_WHEEL (data)
fn mouse_inject(dx: i32, dy: i32, data: u32, flags: u32) {
    if flags & 1 != 0 {
        let (cx, cy) = with(|s| s.cursor);
        let (x, y) = if flags & 0x8000 != 0 {
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
/// A key as SendInput's KEYBDINPUT and keybd_event give it (KEYEVENTF_KEYUP 2)
fn key_inject(vk: u32, scan: u32, flags: u32) {
    input(if flags & 2 != 0 { 6 } else { 5 }, 0, 0, vk & 0xff | (scan & 0xff) << 16);
}
/// keybd_event(vk, scan, flags, extra): SendInput's keyboard event. 催●学級 (ggs) lets go of
/// every key GetKeyboardState shows down with it after its launcher dialog.
fn keybd_event(c: &mut Ctx) -> u32 {
    key_inject(c.a(0) & 0xff, c.a(1) & 0xff, c.a(2));
    crate::obj::signal();
    0
}
/// mouse_event(flags, dx, dy, data, extra): SendInput's mouse event
fn mouse_event(c: &mut Ctx) -> u32 {
    mouse_inject(c.a(1) as i32, c.a(2) as i32, c.a(3), c.a(0));
    crate::obj::signal();
    0
}
/// Milliseconds for TrackMouseEvent's hover timing.
fn track_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}
/// Posts what mouse tracking answered (WM_MOUSELEAVE / WM_MOUSEHOVER, or their WM_NC kinds).
fn track_post(s: &mut State, e: crate::mousetrack::Event) {
    use crate::mousetrack::Event;
    let (hwnd, msg, w, l) = match e {
        Event::Leave(h, nc) => (h, if nc { 0x2a2 } else { 0x2a3 }, 0, 0),
        Event::Hover(h, nc, (x, y)) => {
            let mut fl = 0u32;
            for (vk, bit) in [(1usize, 1u32), (2, 2), (0x10, 4), (0x11, 8), (4, 0x10)] {
                if s.keys[vk] & 0x80 != 0 {
                    fl |= bit;
                }
            }
            // (WM_NCMOUSEHOVER: the hit-test code and screen coordinates; HTCAPTION)
            let (w, (px, py)) = if nc { (2, (x, y)) } else {
                let c = client_screen(s, h);
                (fl, (x - c.l, y - c.t))
            };
            (h, if nc { 0x2a0 } else { 0x2a1 }, w, ((py as u32) << 16) | (px as u32 & 0xffff))
        }
    };
    let Some(tid) = s.wins.get(&hwnd).map(|w| w.tid) else { return };
    let (x, y) = s.cursor;
    s.queues.entry(tid).or_default().posted.push_back(Msg { hwnd, msg, w, l, time: now_ms(), x, y });
}
/// TrackMouseEvent (comctl32's _TrackMouseEvent too): WM_MOUSELEAVE when the cursor leaves the
/// window, WM_MOUSEHOVER when it rests there for the hover time; TME_QUERY / TME_CANCEL.
fn TrackMouseEvent(c: &mut Ctx) -> u32 {
    use crate::mousetrack::*;
    let p = c.a(0);
    // (x64: the HWND at 8 is 8 bytes, the hover time at 16, 24 bytes in all)
    let (size, ho) = if crate::mem::x64() { (24, 16) } else { (16, 12) };
    if p == 0 || r32(p) != size {
        crate::k32::last_error(87);
        return 0;
    }
    let (flags, hwnd, ms) = (r32(p + 4), r32(p + 8), r32(p + ho));
    let me = crate::cur().tid;
    if flags & TME_QUERY != 0 {
        let (f, h, t) = with(|s| query(s.queues.get(&me).and_then(|q| q.track)));
        w32(p + 4, f);
        mem::putp(p + 8, h);
        w32(p + ho, t);
        return 1;
    }
    if !is_window(hwnd) {
        crate::k32::last_error(1400);
        return 0;
    }
    with(|s| {
        let (x, y) = s.cursor;
        let under = window_at(s, x, y);
        let q = s.queues.entry(me).or_default();
        let (t, e) = request(q.track, hwnd, flags, ms, under, (x, y), track_ms());
        q.track = t;
        if let Some(e) = e {
            track_post(s, e);
        }
    });
    1
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
/// The key's scan code (no E0 prefix) in the keyboard layout (keyboard.rs).
pub fn vk_scan(vk: u32) -> u32 {
    crate::keyboard::vk_scan(crate::keyboard::layout_of(0), vk).0
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
pub(crate) fn keyboard_layout() -> u32 {
    if crate::text::acp() == 932 { 0x0411_0411 } else { 0x0409_0409 }
}
fn GetKeyboardLayout(_: &mut Ctx) -> u32 {
    keyboard_layout()
}
/// WaitForInputIdle(process, ms): 0, the process is ready (DEMO.EXE of 妄想ぷろとこる！ waits so for
/// the game it starts; sake's processes take input from their first message loop on)
fn WaitForInputIdle(_: &mut Ctx) -> u32 {
    0
}
fn GetKeyboardLayoutList(c: &mut Ctx) -> u32 {
    if c.a(0) > 0 {
        crate::mem::wp(c.a(1), keyboard_layout());
    }
    1
}
fn ActivateKeyboardLayout(_: &mut Ctx) -> u32 {
    0x0411_0411
}
fn LoadKeyboardLayoutA(_: &mut Ctx) -> u32 {
    0x0411_0411
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
/// The button a message box "presses" so the program goes on (sake shows none): OK, Ignore,
/// Yes, Cancel (Retry would ask again), Continue; by the low 4 bits of uType (MB_OK ..
/// MB_CANCELTRYCONTINUE, else OK).
pub(crate) fn box_answer(style: u32) -> u32 {
    match style & 0xf {
        2 => 5,      // MB_ABORTRETRYIGNORE: IDIGNORE
        3 | 4 => 6,  // MB_YESNOCANCEL, MB_YESNO: IDYES
        5 => 2,      // MB_RETRYCANCEL: IDCANCEL
        6 => 11,     // MB_CANCELTRYCONTINUE: IDCONTINUE
        _ => 1,      // MB_OK, MB_OKCANCEL: IDOK
    }
}
fn show_box(text: &str, cap: &str, style: u32) -> u32 {
    eprintln!("sake: MessageBox [{cap}] {text}");
    if crate::autopilot::on() {
        if let Some(a) = crate::autopilot::box_answer(text, style) {
            return a;
        }
    }
    box_answer(style)
}
fn message_box(c: &mut Ctx, wide: bool) -> u32 {
    let t = arg_str(c.a(1), wide);
    let cap = arg_str(c.a(2), wide);
    show_box(&t, &cap, c.a(3))
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
fn MessageBoxExW(c: &mut Ctx) -> u32 {
    message_box(c, true)
}
/// MSGBOXPARAMS' lpszText, lpszCaption and dwStyle offsets for a pointer size (cbSize and
/// hwndOwner, hInstance pointers first; x64 pads cbSize to 8).
pub(crate) fn msgboxparams(ps: u32) -> (u32, u32, u32) {
    let first = if ps == 8 { 8 } else { 4 };
    (first + 2 * ps, first + 3 * ps, first + 4 * ps)
}
fn message_box_indirect(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return 0;
    }
    let (t, cap, style) = msgboxparams(mem::ps());
    // (lpszText / lpszCaption may be resource ids with hInstance: shown as numbers)
    let s = |o: u32| {
        let v = mem::rp(p + o);
        if v < 0x10000 { format!("#{v}") } else { arg_str(v, wide) }
    };
    show_box(&s(t), &s(cap), r32(p + style))
}
fn MessageBoxIndirectA(c: &mut Ctx) -> u32 {
    message_box_indirect(c, false)
}
fn MessageBoxIndirectW(c: &mut Ctx) -> u32 {
    message_box_indirect(c, true)
}
/// Hooks the thread's code calls (handle, WH_* id, procedure, thread; 0 = every thread):
/// WH_CBT (MFC's CWnd::CreateEx attaches the window and subclasses it in HCBT_CREATEWND),
/// WH_CALLWNDPROC and WH_CALLWNDPROCRET around every sent message (Softhouse-chara's pgsv.dll
/// answers its own message 0x7771 to the game's window in one), WH_GETMESSAGE on every message
/// GetMessage / PeekMessage hands out (LiveMaker's live.dll). Other hooks are accepted and
/// never called. CallNextHookEx does nothing: every hook of a chain is called, newest first.
static HOOKS: Mutex<Vec<(u32, u32, u32, u32)>> = Mutex::new(Vec::new());
/// WH_CALLWNDPROC(RET) hooks set: sending skips the hook lookup when none are
static SEND_HOOKS: AtomicU32 = AtomicU32::new(0);
static NEXT_HOOK: AtomicU32 = AtomicU32::new(0x5000);
/// WH_GETMESSAGE hooks set: GetMessage / PeekMessage skip the lookup when none are
static GETMSG_HOOKS: AtomicU32 = AtomicU32::new(0);
const WH_GETMESSAGE: u32 = 3;
const WH_CALLWNDPROC: u32 = 4;
const WH_CBT: u32 = 5;
const WH_CALLWNDPROCRET: u32 = 12;
fn set_hook(c: &mut Ctx) -> u32 {
    let h = NEXT_HOOK.fetch_add(4, Ordering::Relaxed);
    let id = c.a(0);
    if matches!(id, WH_CBT | WH_CALLWNDPROC | WH_CALLWNDPROCRET | WH_GETMESSAGE) {
        // (a hook for this thread when it names none and is not a DLL's global one; CBT
        // hooks always this thread's, as before)
        let tid = if c.a(3) != 0 { c.a(3) } else if id == WH_CBT || c.a(2) == 0 { crate::cur().tid } else { 0 };
        HOOKS.lock().unwrap().push((h, id, c.a(1), tid));
        match id {
            WH_CBT => {}
            WH_GETMESSAGE => {
                GETMSG_HOOKS.fetch_add(1, Ordering::Relaxed);
            }
            _ => {
                SEND_HOOKS.fetch_add(1, Ordering::Relaxed);
            }
        }
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
    let mut g = HOOKS.lock().unwrap();
    if let Some(i) = g.iter().position(|x| x.0 == c.a(0)) {
        match g.remove(i).1 {
            WH_CBT => {}
            WH_GETMESSAGE => {
                GETMSG_HOOKS.fetch_sub(1, Ordering::Relaxed);
            }
            _ => {
                SEND_HOOKS.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }
    1
}
/// The procedures of thread `tid`'s hooks of kind `id`, newest first
fn hooks_of(id: u32, tid: u32) -> Vec<u32> {
    HOOKS.lock().unwrap().iter().rev().filter(|x| x.1 == id && (x.3 == tid || x.3 == 0)).map(|x| x.2).collect()
}
/// The thread's WH_GETMESSAGE hooks for the message GetMessage / PeekMessage just wrote at `p`
/// (HC_ACTION, PM_REMOVE or PM_NOREMOVE, the MSG: a hook may change it before the program sees
/// it), newest first.
fn getmsg_hooks(cpu: &mut koji::Cpu, p: u32, removed: bool) {
    if GETMSG_HOOKS.load(Ordering::Relaxed) == 0 {
        return;
    }
    for h in hooks_of(WH_GETMESSAGE, crate::cur().tid) {
        crate::call(cpu, h, &[0, removed as u32, p]);
    }
}
/// The thread's CBT hooks with (code, wparam, lparam); true when one refuses.
fn cbt(c: &mut Ctx, code: u32, wp: u32, lp: u32) -> bool {
    let tid = crate::cur().tid;
    hooks_of(WH_CBT, tid).into_iter().any(|p| c.call(p, &[code, wp, lp]) != 0)
}
/// A sent message to `proc_` with the thread's WH_CALLWNDPROC hooks before it (HC_ACTION,
/// sent by this thread, &CWPSTRUCT {lParam, wParam, message, hwnd}) and WH_CALLWNDPROCRET
/// hooks after it (&CWPRETSTRUCT {lResult, lParam, wParam, message, hwnd}).
fn call_sent(cpu: &mut koji::Cpu, proc_: u32, hwnd: u32, msg: u32, wp: u32, lp: u32, own: bool) -> u32 {
    if SEND_HOOKS.load(Ordering::Relaxed) == 0 {
        return call_proc(cpu, proc_, hwnd, msg, wp, lp);
    }
    let tid = crate::cur().tid;
    let (before, after) = (hooks_of(WH_CALLWNDPROC, tid), hooks_of(WH_CALLWNDPROCRET, tid));
    let ps = if mem::x64() { 8 } else { 4 };
    if !before.is_empty() {
        let cwp = mem::heap_alloc(4 * ps, true);
        mem::wp(cwp, lp);
        mem::wp(cwp + ps, wp);
        w32(cwp + 2 * ps, msg);
        mem::wp(cwp + 3 * ps, hwnd);
        for p in before {
            crate::call(cpu, p, &[0, own as u32, cwp]);
        }
        mem::heap_free(cwp);
    }
    let r = call_proc(cpu, proc_, hwnd, msg, wp, lp);
    if !after.is_empty() {
        let cwp = mem::heap_alloc(5 * ps, true);
        mem::wp(cwp, r);
        mem::wp(cwp + ps, lp);
        mem::wp(cwp + 2 * ps, wp);
        w32(cwp + 3 * ps, msg);
        mem::wp(cwp + 4 * ps, hwnd);
        for p in after {
            crate::call(cpu, p, &[0, own as u32, cwp]);
        }
        mem::heap_free(cwp);
    }
    r
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
        // SPI_GETDEFAULTINPUTLANG: the HKL of the default input language, as GetKeyboardLayout
        // (Triangle's 妄想ぷろとこる！ ANDs it with 0x411 among its "Japan only" checks)
        0x59 => {
            if p != 0 {
                crate::mem::wp(p, keyboard_layout());
            }
        }
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
mod window_info_tests {
    use super::{window_info, Rect};
    use proptest::prelude::*;
    fn at(b: &[u8; 60], o: usize) -> u32 {
        u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
    }
    /// winref windowinfo's answer for an overlapped 300x200 window at 100,100 (frame 8, caption 31)
    #[test]
    fn overlapped() {
        let win = Rect { l: 100, t: 100, r: 400, b: 300 };
        let cl = Rect { l: 108, t: 131, r: 392, b: 292 };
        let b = window_info(win, cl, 0x14cf_0000, 0x100, true, 0xc123);
        assert_eq!(at(&b, 0), 60);
        assert_eq!((at(&b, 4), at(&b, 8), at(&b, 12), at(&b, 16)), (100, 100, 400, 300));
        assert_eq!((at(&b, 20), at(&b, 24), at(&b, 28), at(&b, 32)), (108, 131, 392, 292));
        assert_eq!((at(&b, 36), at(&b, 40), at(&b, 44)), (0x14cf_0000, 0x100, 1));
        assert_eq!((at(&b, 48), at(&b, 52)), (8, 8));
        assert_eq!(at(&b, 56), 0x0400_c123);
    }
    proptest! {
        /// any rectangles (negative screen positions too): every field lands where WINDOWINFO
        /// has it, the borders are the left and bottom gaps, nothing else is written
        #[test]
        fn fields(l in -5000i32..5000, t in -5000i32..5000, w in 0i32..4000, h in 0i32..4000,
                  bl in 0i32..50, bt in 0i32..80, br in 0i32..50, bb in 0i32..50,
                  style: u32, ex: u32, active: bool, atom: u16) {
            let win = Rect { l, t, r: l + w, b: t + h };
            let cl = Rect { l: l + bl, t: t + bt, r: l + w - br, b: t + h - bb };
            let b = window_info(win, cl, style, ex, active, atom);
            prop_assert_eq!(at(&b, 0), 60);
            prop_assert_eq!(at(&b, 4) as i32, l);
            prop_assert_eq!(at(&b, 16) as i32, t + h);
            prop_assert_eq!(at(&b, 20) as i32, l + bl);
            prop_assert_eq!(at(&b, 32) as i32, t + h - bb);
            prop_assert_eq!(at(&b, 36), style);
            prop_assert_eq!(at(&b, 40), ex);
            prop_assert_eq!(at(&b, 44), active as u32);
            prop_assert_eq!(at(&b, 48) as i32, bl);
            prop_assert_eq!(at(&b, 52) as i32, bb);
            prop_assert_eq!(at(&b, 56) & 0xffff, atom as u32);
            prop_assert_eq!(at(&b, 56) >> 16, 0x400);
        }
    }
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

/// The screen switches to `w` x `h` (DirectDraw's SetDisplayMode / RestoreDisplayMode)
pub fn set_screen(c: &mut Ctx, w: i32, h: i32) {
    set_display(c, w, h);
}
/// A new screen size; top-level windows get WM_DISPLAYCHANGE (CatSystem2 goes on from there)
fn set_display(c: &mut Ctx, w: i32, h: i32) {
    kura::display::vnhost_display_set_size(w, h);
    if with(|s| {
        s.mode = (w, h);
        std::mem::replace(&mut s.screen, (w, h))
    }) != (w, h)
    {
        let f = crate::api::export("user32", "SendMessageA");
        c.call(f, &[0xffff, 0x7e, 32, (h as u32) << 16 | (w as u32 & 0xffff)]);
    }
}
/// DEVMODE of mode `n` (ENUM_CURRENT_SETTINGS/ENUM_REGISTRY_SETTINGS: the screen's). The W
/// layout has a 64-byte device name and a 64-byte form name: fields before dmFormName are 32
/// bytes later, those after it 64. Windows writes the Windows 95 DEVMODE (124 bytes, W 188)
/// whatever dmSize says, and sets dmSize to that (tests/winref/probes/sizes.c): RealLive passes
/// that short one on its stack, and the rest is its return address (e170d29).
/// The display modes EnumDisplaySettings lists for the screen `(w, h)`: the usual sizes that fit
/// on it and the screen's own, smallest first. Windows always lists the current mode: SiglusEngine
/// looks the current size up in this list to know the desktop, and with the 1920x1080 screen
/// missing it took the first entry (640x480), found its 1024x768 window too big and opened at
/// half size (牝アイドル 体験版, 512x384).
fn display_modes((w, h): (i32, i32)) -> Vec<(i32, i32)> {
    const USUAL: [(i32, i32); 12] = [(640, 480), (800, 600), (1024, 768), (1280, 720), (1280, 960), (1280, 1024),
        (1366, 768), (1600, 900), (1680, 1050), (1920, 1080), (2560, 1440), (3840, 2160)];
    let mut m: Vec<(i32, i32)> = USUAL.iter().copied().filter(|&(x, y)| x <= w && y <= h).collect();
    if !m.contains(&(w, h)) {
        m.push((w, h));
    }
    m.sort();
    m
}
fn display_mode(n: u32, p: u32, wide: bool) -> u32 {
    let modes = display_modes(screen());
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
    let (name, desc) = if device == 0 { ("\\\\.\\DISPLAY1", "ippai display") } else { ("\\\\.\\DISPLAY1\\Monitor0", "Generic PnP Monitor") };
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
    kura::display::HMONITOR
}
fn MonitorFromPoint(_: &mut Ctx) -> u32 {
    kura::display::HMONITOR
}
fn MonitorFromRect(_: &mut Ctx) -> u32 {
    kura::display::HMONITOR
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
    c.call(f, &[kura::display::HMONITOR, 0, r, lp]);
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
    let (parts, inner, ok) = edge_parts(*rc, edge, flags);
    for (r, color) in parts {
        crate::gdi::fill_rect_brush(dc, r, crate::gdi::solid_brush(sys_color(color)));
    }
    if flags & 0x2000 != 0 && flags & 0x10 == 0 {
        *rc = inner;
    }
    ok
}

/// What DrawEdge draws: the rectangles in order with their COLOR_* indices, the rectangle
/// inside the edge (BF_ADJUST's), and its answer
pub(crate) fn edge_parts(rc: Rect, edge: u32, flags: u32) -> (Vec<(Rect, u32)>, Rect, bool) {
    let mut parts = Vec::new();
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
        return (parts, rc, ok); // BF_DIAGONAL
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
    let mut fill = |r: Rect, color: i8| {
        if color != N {
            parts.push((r, color as u32));
        }
    };
    let r = rc;
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
    let mut inner = rc;
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
        inner = ir;
    }
    (parts, inner, ok)
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
/// (DRAWTEXTPARAMS' margins and tab length are not applied, as in DrawTextExW)
fn DrawTextExA(c: &mut Ctx) -> u32 {
    DrawTextA(c)
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
fn IsMenu(c: &mut Ctx) -> u32 {
    with(|s| s.menus.contains_key(&c.a(0))) as u32
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
        let mut i = if pos == 0xffff_ffff { m.items.len() } else { item_index(m, pos, by_pos).unwrap_or(m.items.len()) };
        // (an item appended goes in front of a last HBMMENU_SYSTEM bitmap item, which stays
        // last: user32:menu test_InsertMenu, as Windows does)
        if i == m.items.len() && m.items.last().is_some_and(|l| l.bmp == 1) {
            i -= 1;
        }
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
    insert_menu(c, false)
}
fn InsertMenuW(c: &mut Ctx) -> u32 {
    insert_menu(c, true)
}
fn insert_menu(c: &mut Ctx, wide: bool) -> u32 {
    let (h, pos, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let sub = if flags & 0x10 != 0 { id } else { 0 };
    insert_item(h, pos, flags & 0x400 != 0, MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, wide), sub, data: 0, bmp: if flags & 0x904 == 4 { p } else { 0 } })
}
fn AppendMenuA(c: &mut Ctx) -> u32 {
    let (h, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let sub = if flags & 0x10 != 0 { id } else { 0 };
    insert_item(h, 0xffff_ffff, true, MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, false), sub, data: 0, bmp: if flags & 0x904 == 4 { p } else { 0 } })
}
fn AppendMenuW(c: &mut Ctx) -> u32 {
    let (h, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let sub = if flags & 0x10 != 0 { id } else { 0 };
    insert_item(h, 0xffff_ffff, true, MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, true), sub, data: 0, bmp: if flags & 0x904 == 4 { p } else { 0 } })
}
/// MENUITEMINFO offsets: (wID, hSubMenu, dwItemData, dwTypeData, cch); handles are 8 bytes on x64.
fn mii() -> (u32, u32, u32, u32, u32) {
    if mem::x64() { (16, 24, 48, 56, 64) } else { (16, 20, 32, 36, 40) }
}
/// MENUITEMINFO's hbmpItem (after cch)
fn mii_bmp() -> u32 {
    if mem::x64() { 72 } else { 44 }
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
    if mask & 0x80 != 0 {
        it.bmp = mem::rp(p + mii_bmp());
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
        if mask & 0x20 != 0 {
            x.data = it.data;
        }
        if mask & 0x80 != 0 {
            x.bmp = it.bmp;
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
    if mask & 0x80 != 0 {
        mem::wp(p + mii_bmp(), it.bmp);
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
fn GetMenuStringW(c: &mut Ctx) -> u32 {
    let (h, item, buf, cap, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let t = with(|s| locate(&s.menus, h, item, flags & 0x400 != 0).map(|(m, i)| s.menus[&m].items[i].text.clone())).unwrap_or_default();
    if buf == 0 {
        return t.len() as u32;
    }
    mem::put_wstr(buf, cap, &t)
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
    modify_menu(c, false)
}
fn ModifyMenuW(c: &mut Ctx) -> u32 {
    modify_menu(c, true)
}
fn modify_menu(c: &mut Ctx, wide: bool) -> u32 {
    let (h, pos, flags, id, p) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    with(|s| {
        let Some(m) = s.menus.get_mut(&h) else { return 0 };
        let Some(i) = item_index(m, pos, flags & 0x400 != 0) else { return 0 };
        let sub = if flags & 0x10 != 0 { id } else { 0 };
        m.items[i] = MenuItem { id: if sub != 0 { 0xffff_ffff } else { id }, flags, text: menu_text(flags, p, wide), sub, data: 0, bmp: if flags & 0x904 == 4 { p } else { 0 } };
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
        let had = menu_bar(&s.menus, w.menu, w.style);
        w.menu = m;
        let has = menu_bar(&s.menus, m, w.style);
        if had != has {
            w.client = client_of(&w.rect, w.style, w.exstyle, has);
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
/// The menu bar comes or goes with the menu's first item or its last (Windows recomputes the
/// frame here: SetWindowPos with SWP_FRAMECHANGED).
fn DrawMenuBar(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let changed = with(|s| {
        let has = s.wins.get(&h).map(|w| menu_bar(&s.menus, w.menu, w.style))?;
        let w = s.wins.get_mut(&h)?;
        let client = client_of(&w.rect, w.style, w.exstyle, has);
        if client == w.client {
            return None;
        }
        w.client = client;
        resize_surface(w);
        update_display(s, h);
        Some((client.w(), client.h()))
    });
    if let Some((cw, ch)) = changed {
        send(c, h, 0x5, 0, (ch as u32) << 16 | (cw as u32 & 0xffff));
    }
    with(|s| s.wins.contains_key(&h)) as u32
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
/// LoadImage(inst, name, type, cx, cy, flags). LR_LOADFROMFILE (0x10): `name` is a file and
/// `inst` is not looked at (Key's StartMenu.exe passes its module); a bitmap is read from it
/// (gdi::load_bitmap_file, LR_CREATEDIBSECTION 0x2000 for a DIB section), an icon or cursor is a
/// handle when the file is there. Else a bitmap resource of `inst`; icons and cursors are
/// handles whose pictures are not used.
fn load_image(c: &mut Ctx, wide: bool) -> u32 {
    let (inst, name, kind, flags) = (c.a(0), c.a(1), c.a(2), c.a(5));
    if flags & 0x10 != 0 {
        if name == 0 {
            crate::k32::last_error(87);
            return 0;
        }
        return match kind {
            0 => crate::gdi::load_bitmap_file(&arg_str(name, wide), flags & 0x2000 != 0),
            _ => cursor_from_file(name, wide),
        };
    }
    if kind == 0 {
        return crate::gdi::load_bitmap(inst, name, wide);
    }
    fake_handle()
}
fn LoadImageA(c: &mut Ctx) -> u32 {
    load_image(c, false)
}
fn LoadImageW(c: &mut Ctx) -> u32 {
    load_image(c, true)
}
fn CreateIcon(_: &mut Ctx) -> u32 {
    fake_handle()
}
/// CreateIconFromResource(Ex)(bits, size, icon, version, cx, cy, flags): an icon or cursor from
/// an RT_ICON / RT_CURSOR resource's data (a cursor's starts with its hotspot, two WORDs), a
/// BITMAPINFOHEADER (or BITMAPCOREHEADER) DIB or a PNG; 0 for anything else (as Windows: a
/// version outside 2.0..=3.0, data too short for its header). The pictures are not used, as
/// LoadIcon's are not; a cursor's hotspot is kept for GetIconInfo. (SDL 1.2 sets Ren'Py's window
/// icon so.)
pub(crate) fn icon_from_resource(bits: &[u8], icon: bool, version: u32) -> Option<Option<(u16, u16)>> {
    if !(0x2_0000..=0x3_0000).contains(&version) {
        return None;
    }
    let (hot, dib) = if icon {
        (None, bits)
    } else {
        let w = |o: usize| u16::from_le_bytes([bits[o], bits[o + 1]]);
        if bits.len() < 4 {
            return None;
        }
        (Some((w(0), w(2))), &bits[4..])
    };
    if dib.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(hot);
    }
    let hs = u32::from_le_bytes(dib.get(..4)?.try_into().ok()?) as usize;
    (matches!(hs, 12 | 40 | 52 | 56 | 108 | 124) && dib.len() >= hs).then_some(hot)
}
fn icon_from_resource_call(c: &mut Ctx, version: u32) -> u32 {
    let (p, size, icon) = (c.a(0), c.a(1), c.a(2) != 0);
    if p == 0 || size == 0 {
        crate::k32::last_error(87);
        return 0;
    }
    let bits = mem::slice(p, size.min(1 << 24)).to_vec();
    let Some(hot) = icon_from_resource(&bits, icon, version) else { return 0 };
    let h = fake_handle();
    if let Some(hot) = hot {
        HOTSPOTS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, hot);
    }
    h
}
fn CreateIconFromResource(c: &mut Ctx) -> u32 {
    let v = c.a(3);
    icon_from_resource_call(c, v)
}
fn CreateIconFromResourceEx(c: &mut Ctx) -> u32 {
    let v = c.a(3);
    icon_from_resource_call(c, v)
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
/// How deep popups in a menu template may nest: the parsers recurse per level, and a template
/// is the program's (or a game's damaged resource), so a deeper one must not overflow sake's own
/// stack; the deeper popups are left empty. (Windows' menus nest a handful deep.)
const MENU_DEPTH: u32 = 64;
#[cfg(test)]
fn menu_items(h: u32, p: u32) -> u32 {
    menu_items_at(h, p, u32::MAX, 0)
}
/// (`end`: where the template's bytes end, a resource's size; u32::MAX for LoadMenuIndirect,
/// which trusts the program as Windows does. A damaged resource without its last item stops there.)
fn menu_items_at(h: u32, mut p: u32, end: u32, depth: u32) -> u32 {
    loop {
        if p.saturating_add(4) > end {
            return end;
        }
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
            p = if depth < MENU_DEPTH { menu_items_at(sub, p, end, depth + 1) } else { skip_menu_items(p, end) };
            // (a popup's id is its menu handle)
            insert_item(h, 0xffff_ffff, true, MenuItem { id: sub, flags, text, sub, data: 0, bmp: 0 });
        } else {
            insert_item(h, 0xffff_ffff, true, MenuItem { id, flags, text, sub: 0, data: 0, bmp: 0 });
        }
        if f & 0x80 != 0 {
            return p;
        }
    }
}
/// MENUEX items: type, state, id (dwords), flags word (1 popup, 0x80 last), text, dword aligned;
/// a popup has a help id and then its items
#[cfg(test)]
fn menuex_items(h: u32, p: u32) -> u32 {
    menuex_items_at(h, p, u32::MAX, 0)
}
/// The end of a MENU template's item list, without making menus (and without recursion).
fn skip_menu_items(mut p: u32, end: u32) -> u32 {
    let mut open = 1u32;
    while open > 0 {
        if p.saturating_add(4) > end {
            return end;
        }
        let f = r16(p) as u32;
        p += if f & 0x10 == 0 { 4 } else { 2 };
        p += 2 * wstr(p).len() as u32 + 2;
        if f & 0x10 != 0 {
            open += 1;
        }
        // (a popup that is the last item: its own list still follows, so the count stays open)
        if f & 0x80 != 0 {
            open -= 1;
        }
    }
    p
}
fn skip_menuex_items(mut p: u32, end: u32) -> u32 {
    let mut open = 1u32;
    while open > 0 {
        if p.saturating_add(16) > end {
            return end;
        }
        let res = r16(p + 12) as u32;
        let text = wstr(p + 14);
        p = (p + 14 + 2 * text.len() as u32 + 2 + 3) & !3;
        if res & 1 != 0 {
            p += 4;
            open += 1;
        }
        if res & 0x80 != 0 {
            open -= 1;
        }
    }
    p
}
fn menuex_items_at(h: u32, mut p: u32, end: u32, depth: u32) -> u32 {
    loop {
        if p.saturating_add(16) > end {
            return end;
        }
        let (ty, state, id, res) = (r32(p), r32(p + 4), r32(p + 8), r16(p + 12) as u32);
        let text = wstr(p + 14);
        p = (p + 14 + 2 * text.len() as u32 + 2 + 3) & !3;
        let flags = ty | state | if res & 1 != 0 { 0x10 } else { 0 };
        if res & 1 != 0 {
            let sub = new_menu();
            p = if depth < MENU_DEPTH { menuex_items_at(sub, p + 4, end, depth + 1) } else { skip_menuex_items(p + 4, end) };
            insert_item(h, 0xffff_ffff, true, MenuItem { id, flags, text, sub, data: 0, bmp: 0 });
        } else {
            insert_item(h, 0xffff_ffff, true, MenuItem { id, flags, text, sub: 0, data: 0, bmp: 0 });
        }
        if res & 0x80 != 0 {
            return p;
        }
    }
}
fn menu_from_template(p: u32) -> u32 {
    menu_from_bytes(p, u32::MAX)
}
/// A menu from a RT_MENU resource of `size` bytes: never read past its end
fn menu_from_resource((p, size): (u32, u32)) -> u32 {
    menu_from_bytes(p, p.saturating_add(size))
}
fn menu_from_bytes(p: u32, end: u32) -> u32 {
    let h = new_menu();
    if p.saturating_add(4) > end {
        return h;
    }
    match r16(p) {
        0 => {
            menu_items_at(h, p + 4 + r16(p + 2) as u32, end, 0);
        }
        1 => {
            menuex_items_at(h, p + 4 + r16(p + 2) as u32, end, 0);
        }
        _ => {}
    }
    h
}
fn LoadMenuA(c: &mut Ctx) -> u32 {
    resource(c.a(0), c.a(1), 4, false).map_or(0, menu_from_resource)
}
fn LoadMenuW(c: &mut Ctx) -> u32 {
    resource(c.a(0), c.a(1), 4, true).map_or(0, menu_from_resource)
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
/// CharPrevW(start, current): the character before (a UTF-16 unit; start itself at the start)
fn CharPrevW(c: &mut Ctx) -> u32 {
    let (start, p) = (c.a(0), c.a(1));
    if p <= start { start } else { p - 2 }
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
    WINSTA
}
/// The window station GetProcessWindowStation gives; any other handle is taken for the desktop.
const WINSTA: u32 = 0x50;

/// What GetUserObjectInformation returns for `index` of object `h`: UOI_FLAGS (1) a
/// USEROBJECTFLAGS (the window station visible, WSF_VISIBLE: GDI+ and others check it for an
/// interactive session), UOI_NAME (2) / UOI_TYPE (3) the name, NUL-terminated. None: an index
/// Windows answers ERROR_INVALID_PARAMETER for.
fn user_object_info(h: u32, index: u32, wide: bool) -> Option<Vec<u8>> {
    let text = |t: &str| -> Vec<u8> {
        if wide {
            t.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect()
        } else {
            t.bytes().chain([0]).collect()
        }
    };
    let winsta = h == WINSTA;
    match index {
        1 => Some([0u32, 0, winsta as u32].iter().flat_map(|v| v.to_le_bytes()).collect()),
        2 => Some(text(if winsta { "WinSta0" } else { "Default" })),
        3 => Some(text(if winsta { "WindowStation" } else { "Desktop" })),
        _ => None,
    }
}
/// GetUserObjectInformation(object, index, buffer, size, needed)
fn user_object_information(c: &mut Ctx, wide: bool) -> u32 {
    let (h, index, buf, size, needed) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some(data) = user_object_info(h, index, wide) else {
        crate::k32::last_error(87);
        return 0;
    };
    let n = data.len() as u32;
    if needed != 0 {
        mem::w32(needed, n);
    }
    if buf == 0 || size < n {
        crate::k32::last_error(122); // ERROR_INSUFFICIENT_BUFFER
        return 0;
    }
    for (i, b) in data.iter().enumerate() {
        mem::w8(buf + i as u32, *b);
    }
    1
}
fn GetUserObjectInformationA(c: &mut Ctx) -> u32 {
    user_object_information(c, false)
}
fn GetUserObjectInformationW(c: &mut Ctx) -> u32 {
    user_object_information(c, true)
}
/// SetWinEventHook: a handle, no events (sake raises no accessibility events); GDI+ fails its
/// startup without one.
fn SetWinEventHook(_: &mut Ctx) -> u32 {
    static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x7e00_0001);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
fn UnhookWinEvent(c: &mut Ctx) -> u32 {
    (c.a(0) != 0) as u32
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
/// The pointer messages' frame ids (POINTER_INFO.frameId)
static POINTER_FRAME: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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

/// A key going down (`down`) or up, `data` as the page sends it (vk | scan << 16; the page's vk
/// for Shift, Ctrl and Alt is the side's: VK_LSHIFT..VK_RMENU), into the keyboard state as
/// Windows keeps it: the side's entry and the generic VK_SHIFT / VK_CONTROL / VK_MENU, down while
/// either side is (GetKeyState(VK_CONTROL) and GetKeyState(VK_LCONTROL) both); a key's toggle bit
/// flips when it goes down, not on its repeats. Returns the vk of the message (WM_KEYDOWN's
/// wParam is the generic one, never the side's) and its lParam bits: 24, the right Ctrl / Alt
/// (extended); 30 (a key down), the key was down already (a repeat).
fn key_state(keys: &mut [u8; 256], pressed: &mut [bool; 256], data: u32, down: bool) -> (u32, u32) {
    let side = (data & 0xff) as usize;
    let (generic, other) = match side {
        0xa0..=0xa5 => (0x10 + (side - 0xa0) / 2, side ^ 1),
        _ => (side, side),
    };
    let was = keys[side] & 0x80 != 0;
    let mut flags = if matches!(side, 0xa3 | 0xa5) { 1 << 24 } else { 0 };
    for k in if generic == side { vec![side] } else { vec![side, generic] } {
        if down {
            pressed[k] = true;
            // (the generic key flips when it goes down from neither side down)
            let first = if k == generic && generic != side { keys[other] & 0x80 == 0 && !was } else { !was };
            keys[k] = 0x80 | if first { (keys[k] ^ 1) & 1 } else { keys[k] & 1 };
        } else if k == side || keys[other] & 0x80 == 0 {
            keys[k] &= 1;
        }
    }
    if down && was {
        flags |= 1 << 30;
    }
    (generic as u32, flags)
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
                let under = window_at(s, x, y);
                {
                    // TrackMouseEvent: leaving (the window under the cursor, capture or not)
                    let now = track_ms();
                    let mut events = Vec::new();
                    for q in s.queues.values_mut() {
                        if q.track.is_some() {
                            let (t, e) = crate::mousetrack::moved(q.track, under, (x, y), now);
                            q.track = t;
                            events.extend(e);
                        }
                    }
                    for e in events {
                        track_post(s, e);
                    }
                }
                let target = if s.capture != 0 { s.capture } else { under };
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
                let held = |keys: &[u8; 256]| [1usize, 2, 4].iter().enumerate().fold(0u8, |b, (i, vk)| b | (((keys[*vk] >> 7) & 1) << i));
                let before = held(&s.keys);
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
                // the mouse as a pointer (EnableMouseInPointer): WM_POINTER* ahead of the mouse
                // message, at the screen position
                let pointer = if crate::pointer::on() {
                    crate::pointer::event(kind, data, data as u16 as i16 as i32, before, held(&s.keys)).map(|e| {
                        let mods = if s.keys[0x10] & 0x80 != 0 { 4 } else { 0 } | if s.keys[0x11] & 0x80 != 0 { 8 } else { 0 };
                        let frame = POINTER_FRAME.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        crate::pointer::set_last(crate::pointer::Info { event: e, frame, hwnd: target, x, y, time: t, keys: mods, qpc: crate::k32::qpc() });
                        Msg { hwnd: target, msg: e.msg, w: e.wparam, l: ((y as u32) << 16) | (x as u32 & 0xffff), time: t, x, y }
                    })
                } else {
                    None
                };
                let q = s.queues.entry(tid).or_default();
                // coalesce moves (and their pointer updates: [WM_POINTERUPDATE,] WM_SETCURSOR,
                // WM_MOUSEMOVE)
                if msg == 0x200 {
                    let n = q.input.len();
                    if q.input.back().is_some_and(|last| last.msg == 0x200 && last.hwnd == target) {
                        let pu = n >= 3 && q.input[n - 3].msg == crate::pointer::WM_POINTERUPDATE && q.input[n - 3].hwnd == target;
                        if pu == pointer.is_some() {
                            q.input[n - 1] = Msg { hwnd: target, msg, w: wp, l: lp, time: t, x, y };
                            if let Some(p) = pointer {
                                q.input[n - 3] = p;
                            }
                            return;
                        }
                    }
                    q.input.extend(pointer);
                    q.input.push_back(Msg { hwnd: target, msg: 0x20, w: target, l: 0x200 << 16 | 1, time: t, x, y });
                } else {
                    q.input.extend(pointer);
                }
                q.input.push_back(Msg { hwnd: target, msg, w: wp, l: lp, time: t, x, y });
            }
            5 | 6 => {
                let (vk, flags) = key_state(&mut s.keys, &mut s.pressed, data, kind == 5);
                let vk = vk as usize;
                let scan = (data >> 16) & 0xff;
                let target = if s.focus != 0 { s.focus } else { s.active };
                if crate::env_on!("SAKE_TRACE_INPUT") {
                    eprintln!("sake input: key {vk:x} kind {kind} -> {target:x} (focus {:x} active {:x})", s.focus, s.active);
                    for (h, w) in s.wins.iter() {
                        eprintln!("  win {h:x} parent {:x} id {} vis {} style {:x} ex {:x} tid {} rect {:?}", w.parent, w.id, w.visible, w.style, w.exstyle, w.tid, w.rect);
                    }
                }
                let Some(w) = s.wins.get(&target) else { return };
                let tid = w.tid;
                let alt = s.keys[0x12] & 0x80 != 0;
                let msg = match (kind, alt || vk == 0x12) {
                    (5, false) => 0x100,
                    (6, false) => 0x101,
                    (5, true) => 0x104,
                    _ => 0x105,
                };
                let lp = 1 | scan << 16 | flags | if kind == 6 { 0xc000_0000 } else { 0 } | if alt { 0x2000_0000 } else { 0 };
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
        // DM_GETDEFID: MAKELONG(id, DC_HASDEFID) of the BS_DEFPUSHBUTTON child, also one in a
        // visible, enabled WS_EX_CONTROLPARENT child (a wizard's page dialog holding the
        // buttons: 姦淫特急『松葉』体験版's installer; Wine's DEFDLG_FindDefButton)
        0x400 => find_def_button(h).map_or(0, |(id, _)| 0x534b_0000 | id),
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
fn default_button(c: &mut Ctx, dlg: u32) -> (u32, u32) {
    // (Wine's IsDialogMessage: the focused control if it is a default push button, else
    // DM_GETDEFID's, else IDOK; winref msgwait: a plain push button with the focus is not taken)
    let focused = with(|s| {
        let w = s.wins.get(&s.focus)?;
        let button = s.classes.get(w.class).is_some_and(|c| c.name.eq_ignore_ascii_case("Button"));
        (descends(s, dlg, s.focus) && button && w.style & 0xf == 1).then(|| (w.id & 0xffff, s.focus))
    });
    if let Some(f) = focused {
        return f;
    }
    // DM_GETDEFID: a dialog's procedure answers it, a game's own window (DefWindowProc) does
    // not, and then it is IDOK even beside a BS_DEFPUSHBUTTON (host/tests/dlgenter)
    let r = send(c, dlg, 0x400, 0, 0);
    let id = if r >> 16 == 0x534b { r & 0xffff } else { 1 };
    (id, dlg_item(dlg, id))
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

/// h is a child window (WS_CHILD all the way) inside dlg, at any depth: IsChild
fn descends(s: &State, dlg: u32, mut h: u32) -> bool {
    for _ in 0..64 {
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
    false
}

/// DM_GETDEFID's button: the first BS_DEFPUSHBUTTON among the children in z-order, looking
/// into visible, enabled WS_EX_CONTROLPARENT children as it goes
fn find_def_button(dlg: u32) -> Option<(u32, u32)> {
    with(|s| def_in(s, dlg, 0))
}

fn def_in(s: &State, h: u32, depth: u32) -> Option<(u32, u32)> {
    let node = |k: u32| {
        let w = s.wins.get(&k)?;
        let button = s.classes.get(w.class).is_some_and(|c| c.name.eq_ignore_ascii_case("Button")) && w.style & 0xf == 1;
        let into = w.exstyle & 0x1_0000 != 0 && w.visible && w.style & 0x0800_0000 == 0;
        Some(DefNode { id: w.id & 0xffff, button, into, children: if into { w.children.clone() } else { Vec::new() } })
    };
    let top = s.wins.get(&h)?.children.clone();
    def_search(&top, &node, depth)
}

/// A window as DM_GETDEFID's search sees it.
struct DefNode {
    id: u32,
    /// a BS_DEFPUSHBUTTON Button
    button: bool,
    /// WS_EX_CONTROLPARENT, visible and enabled: searched into
    into: bool,
    children: Vec<u32>,
}

/// The first default push button among `kids` in order, descending into the windows to search
/// into before going on (Wine's DEFDLG_FindDefButton): (id, window)
fn def_search(kids: &[u32], node: &dyn Fn(u32) -> Option<DefNode>, depth: u32) -> Option<(u32, u32)> {
    if depth > 16 {
        return None;
    }
    for &k in kids {
        let Some(n) = node(k) else { continue };
        if n.button {
            return Some((n.id, k));
        }
        if n.into {
            if let Some(r) = def_search(&n.children, node, depth + 1) {
                return Some(r);
            }
        }
    }
    None
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

#[path = "sdrt_login.rs"]
mod sdrt_login;

/// A dialog's window size for its template's `w`x`h` (pixels) client area: the frame and the
/// menu bar (`bar`) around it, so that the client area is the template's.
fn dialog_window_size(style: u32, exstyle: u32, w: i32, h: i32, bar: bool) -> (i32, i32) {
    let (fl, ft, fr, fb) = frame(style, exstyle, bar);
    (w + fl + fr, h + ft + fb)
}

fn create_dialog(c: &mut Ctx, inst: u32, t: u32, owner: u32, proc_: u32, param: u32, modal: bool) -> u32 {
    let ex = r16(t) == 1 && r16(t + 2) == 0xffff;
    let (style, exstyle, n, mut p) = if ex { (r32(t + 12), r32(t + 8), r16(t + 16) as u32, t + 18) } else { (r32(t), r32(t + 4), r16(t + 8) as u32, t + 10) };
    let (x, y, cx, cy) = (r16(p) as i16 as i32, r16(p + 2) as i16 as i32, r16(p + 4) as i16 as i32, r16(p + 6) as i16 as i32);
    p += 8;
    // the menu: a dialog with one gets its menu bar on top of the template's size (タイムリープ
    // moves its menu bar off the screen, SM_CYMENU above, and its game window fills the client)
    let (menu_s, menu_o, q) = sz_or_ord(p);
    let menu = match (menu_s, menu_o) {
        (Some(s), _) => resource(inst, s, 4, true).map_or(0, menu_from_resource),
        (None, o) if o != 0 => resource(inst, o, 4, true).map_or(0, menu_from_resource),
        _ => 0,
    };
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
    let (ww, wh) = dialog_window_size(style, exstyle, w, h, with(|s| menu_bar(&s.menus, menu, style)));
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
    let dlg = gcall(c, "user32", "CreateWindowExW", &[exstyle, cls, 0, style & !0x1000_0000, ox as u32, oy as u32, ww as u32, wh as u32, owner, menu, inst, param]);
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
        // (a picture static's text is its bitmap or icon: Bishop's startup.exe banner)
        let is_static = with(|stt| stt.wins.get(&ctl).is_some_and(|w| stt.classes[w.class].name.eq_ignore_ascii_case("static")));
        if is_static {
            match sz_or_ord(r2) {
                (None, o, _) if o != 0 => static_load_picture(c, ctl, inst, o),
                (Some(t), _, _) if r16(t) != 0 => static_load_picture(c, ctl, inst, t),
                _ => {}
            }
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
    sdrt_login::fill(c, dlg, inst);
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
    send_to(c.cpu, m.hwnd, m.msg, m.w, m.l, false);
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
        // (a message another thread sent may end it: EndDialog in its handling)
        process_sent(c.cpu);
        if let Some(r) = DLG_END.lock().unwrap().get_or_insert_with(HashMap::default).remove(&dlg) {
            break r;
        }
        if !is_window(dlg) {
            break 0;
        }
        match next_msg(0, 0, 0, true) {
            Some(m) if m.msg == 0x12 => {
                with(|s| s.queues.entry(crate::cur().tid).or_default().quit = Some(m.w));
                break 0;
            }
            Some(m) => {
                // (the dialog or any control inside it, also in a page dialog: IsDialogMessage's IsChild)
                let mine = m.hwnd == dlg || with(|s| descends(s, dlg, m.hwnd));
                if m.msg == 0x100 && mine && (m.w == 0x0d || m.w == 0x1b) {
                    let (id, b) = if m.w == 0x0d { default_button(c, dlg) } else { (2, dlg_item(dlg, 2)) };
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

/// A modal dialog without controls that is never shown (WS_POPUP | WS_CAPTION | WS_SYSMENU |
/// DS_MODALFRAME, not WS_VISIBLE), run by the dialog procedure `proc_` as DialogBoxParam runs
/// one: its own message loop, EndDialog's result. shell32's SHBrowseForFolder: the program's
/// callback gets a real dialog window to send BFFM_ messages to.
pub(crate) fn hidden_dialog(c: &mut Ctx, owner: u32, proc_: u32, param: u32) -> u32 {
    // DLGTEMPLATE: style, extended style, no items, 0,0 200x150, no menu, class or title
    let t = mem::heap_alloc(32, true);
    w32(t, 0x80c8_0080);
    w16(t + 14, 200);
    w16(t + 16, 150);
    let r = dialog_box(c, 0, t, owner, proc_, param);
    mem::heap_free(t);
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
    sdrt_login::status(h, id, &t);
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

// ---------------------------------------------------------------------------------------------
// imm32's default IME window, user32's WINNLS switch of the IME

/// Each thread's default IME window ("IME" class, hidden), made the first time it is asked for
/// once the thread has a window of its own: Windows makes it with the thread's first window,
/// and ImmGetDefaultIMEWnd(NULL) is NULL before that (Wine's imm32 tests). SDRT asks for it.
static IME_WNDS: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);

/// ImmGetDefaultIMEWnd(hwnd): the IME window of hwnd's thread (NULL: this thread's)
fn ImmGetDefaultIMEWnd(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let me = crate::cur().tid;
    let tid = if h == 0 { me } else {
        match with(|s| s.wins.get(&h).map(|w| w.tid)) {
            Some(t) => t,
            None => return 0,
        }
    };
    let known = IME_WNDS.lock().unwrap().get_or_insert_with(HashMap::default).get(&tid).copied();
    if let Some(w) = known.filter(|&w| is_window(w)) {
        return w;
    }
    // (only this thread can make its own; a window of the thread besides an IME one)
    if tid != me || !with(|s| s.wins.values().any(|w| w.tid == tid && !s.classes[w.class].name.eq_ignore_ascii_case("IME"))) {
        return 0;
    }
    let strs = mem::heap_alloc(16, true);
    mem::write(strs, b"IME\0Default IME\0");
    // WS_POPUP | WS_DISABLED, as Windows' is
    let w = gcall(c, "user32", "CreateWindowExA", &[0, strs, strs + 4, 0x8800_0000, 0, 0, 1, 1, 0, 0, 0, 0]);
    mem::heap_free(strs);
    if w != 0 {
        IME_WNDS.lock().unwrap().get_or_insert_with(HashMap::default).insert(tid, w);
    }
    w
}

/// Whether the IME is on (WINNLSEnableIME): it starts on, as on a Japanese Windows
static IME_ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);
/// WINNLSEnableIME(hwnd, enable): the state before. sake has no IME window to show or hide.
fn WINNLSEnableIME(c: &mut Ctx) -> u32 {
    IME_ENABLED.swap(c.a(1) != 0, Ordering::SeqCst) as u32
}
fn WINNLSGetEnableStatus(_: &mut Ctx) -> u32 {
    IME_ENABLED.load(Ordering::SeqCst) as u32
}

pub fn init() {
    reg!("user32"; WINNLSEnableIME, WINNLSGetEnableStatus);
    reg!("imm32"; ImmGetDefaultIMEWnd);
    reg!("user32";
        RegisterClassA, RegisterClassW, RegisterClassExA, RegisterClassExW, UnregisterClassA, UnregisterClassW,
        GetClassInfoA, GetClassInfoW, GetClassInfoExA, GetClassInfoExW, CreateWindowExA, CreateWindowExW,
        DestroyWindow, ShowWindow, ShowWindowAsync, ShowOwnedPopups, UpdateWindow, IsWindow, IsWindowVisible,
        IsWindowEnabled, EnableWindow, IsIconic, IsZoomed, IsChild, SetWindowPos, MoveWindow, BeginDeferWindowPos,
        DeferWindowPos, EndDeferWindowPos, GetWindowRect, GetClientRect, GetWindowInfo, GetWindowPlacement, SetWindowPlacement,
        ClientToScreen, ScreenToClient, MapWindowPoints, AdjustWindowRect, AdjustWindowRectEx, GetParent,
        GetAncestor, SetParent, GetDesktopWindow, GetShellWindow, GetWindow, GetTopWindow, GetLastActivePopup,
        GetDlgItem, GetNextDlgTabItem, GetDlgCtrlID, SendDlgItemMessageA, GetWindowThreadProcessId, EnumWindows, EnumThreadWindows,
        EnumChildWindows, FindWindowA, FindWindowW, FindWindowExA, FindWindowExW, WindowFromPoint, ChildWindowFromPoint,
        GetWindowLongA, GetWindowLongW, SetWindowLongA, SetWindowLongW, GetClassLongA, GetClassLongW,
        GetWindowLongPtrA, GetWindowLongPtrW, SetWindowLongPtrA, SetWindowLongPtrW, GetClassLongPtrA, GetClassLongPtrW,
        SetClassLongPtrA, SetClassLongPtrW,
        SetClassLongA, SetClassLongW, GetClassNameA, GetClassNameW, SetPropA, SetPropW, GetPropA, GetPropW,
        RemovePropA, RemovePropW, SetWindowTextA, SetWindowTextW, GetWindowTextA, GetWindowTextW,
        GetWindowTextLengthA, GetWindowTextLengthW, SetWindowRgn, DefWindowProcA, DefWindowProcW, DefDlgProcA,
        DefFrameProcA, DefMDIChildProcA, CallWindowProcA, CallWindowProcW,
        PostMessageA, PostMessageW, PostThreadMessageA, PostThreadMessageW, PostQuitMessage, SendMessageA,
        SendMessageW, SendMessageTimeoutA, SendNotifyMessageA, SendNotifyMessageW, InSendMessage, ReplyMessage, GetMessageA,
        GetMessageW, PeekMessageA, PeekMessageW, WaitMessage, MsgWaitForMultipleObjects,
        MsgWaitForMultipleObjectsEx, GetQueueStatus, GetInputState, TranslateMessage, DispatchMessageA,
        DispatchMessageW, IsDialogMessageA, IsDialogMessageW, TranslateMDISysAccel, TranslateAcceleratorA, TranslateAcceleratorW, CopyAcceleratorTableA, CopyAcceleratorTableW,
        CreateAcceleratorTableA, CreateAcceleratorTableW, DestroyAcceleratorTable, LoadAcceleratorsW, LoadMenuIndirectA, LoadMenuIndirectW,
        GetMessagePos, GetMessageTime, GetMessageExtraInfo, RegisterWindowMessageA, RegisterWindowMessageW,
        SetTimer, KillTimer, InvalidateRect, InvalidateRgn, ValidateRect, ValidateRgn, RedrawWindow,
        GetUpdateRect, GetUpdateRgn, BeginPaint, EndPaint, GetDC, GetDCEx, GetWindowDC, ReleaseDC, WindowFromDC,
        ScrollWindow, ScrollWindowEx, LockWindowUpdate, SetFocus, GetFocus, SetActiveWindow, GetActiveWindow,
        GetForegroundWindow, SetForegroundWindow, BringWindowToTop, SetCapture, ReleaseCapture, GetCapture,
        SendInput, keybd_event, mouse_event, TrackMouseEvent, GetCursorPos, SetCursorPos, SetCursor, GetCursor, ShowCursor, ClipCursor, GetKeyState, GetAsyncKeyState,
        GetKeyboardState, SetKeyboardState, GetKeyboardType, GetKeyboardLayout, GetKeyboardLayoutList, WaitForInputIdle,
        ActivateKeyboardLayout, LoadKeyboardLayoutA, GetKeyNameTextA, GetKeyNameTextW,
        GetDoubleClickTime, MessageBeep, MessageBoxA, MessageBoxW, MessageBoxExA, MessageBoxExW, MessageBoxIndirectA, MessageBoxIndirectW,
        SetWindowsHookExA, SetWindowsHookExW, UnhookWindowsHookEx, CallNextHookEx, WinHelpA, GetSysColor,
        GetSysColorBrush, GetSystemMetrics, SystemParametersInfoA, SystemParametersInfoW, ChangeDisplaySettingsA,
        ChangeDisplaySettingsW, ChangeDisplaySettingsExA, ChangeDisplaySettingsExW, EnumDisplaySettingsA, EnumDisplaySettingsW, EnumDisplaySettingsExA, EnumDisplaySettingsExW, EnumDisplayDevicesA, EnumDisplayDevicesW, GetDisplayConfigBufferSizes, QueryDisplayConfig, DisplayConfigGetDeviceInfo, MonitorFromWindow, MonitorFromPoint,
        MonitorFromRect, GetMonitorInfoA, GetMonitorInfoW, EnumDisplayMonitors,
        SetRect, SetRectEmpty, CopyRect, IsRectEmpty, EqualRect, PtInRect, OffsetRect, InflateRect, IntersectRect,
        UnionRect, SubtractRect, FillRect, FrameRect, InvertRect, DrawFocusRect, DrawEdge, DrawFrameControl,
        DrawIcon, DrawIconEx, DrawTextA, DrawTextW, DrawTextExW, DrawTextExA, GrayStringA,
        CreateCaret, DestroyCaret, SetCaretPos, ShowCaret, HideCaret, GetScrollInfo, SetScrollInfo, GetScrollPos,
        SetScrollPos, GetScrollRange, SetScrollRange, ShowScrollBar, EnableScrollBar, OpenClipboard,
        CloseClipboard, EmptyClipboard, GetClipboardData, SetClipboardData, IsClipboardFormatAvailable,
        EnumClipboardFormats, GetPriorityClipboardFormat, RegisterClipboardFormatA, RegisterClipboardFormatW,
        CreateMenu, CreatePopupMenu, DestroyMenu, InsertMenuA, InsertMenuW, ModifyMenuW, GetMenuStringW, AppendMenuA, AppendMenuW, InsertMenuItemA,
        InsertMenuItemW, SetMenuItemInfoA, SetMenuItemInfoW, GetMenuItemInfoA, GetMenuItemInfoW, GetMenuItemCount, GetMenuItemID, GetSubMenu,
        GetMenuState, GetMenuStringA, CheckMenuItem, CheckMenuRadioItem, EnableMenuItem, IsMenu, DeleteMenu, RemoveMenu, ModifyMenuA, GetMenu,
        SetMenu, GetSystemMenu, DrawMenuBar, TrackPopupMenu, TrackPopupMenuEx, GetMenuItemRect,
        SetMenuItemBitmaps, GetMenuCheckMarkDimensions,
        LoadStringA, LoadStringW, LoadCursorA, LoadCursorW, LoadCursorFromFileA, LoadCursorFromFileW, LoadIconA, LoadIconW, LoadImageA,
        LoadImageW, CreateIcon, CreateIconFromResource, CreateIconFromResourceEx, CreateIconIndirect, CreateCursor, CopyIcon, DestroyCursor, DestroyIcon,
        GetIconInfo, LoadBitmapA, LoadBitmapW, LoadAcceleratorsA, LoadMenuA, LoadMenuW,
        CharLowerA, CharUpperA, CharLowerW, CharUpperW, CharLowerBuffA, CharUpperBuffA, CharLowerBuffW, CharUpperBuffW, CharNextA, CharNextExA, CharNextW,
        CharPrevA, CharPrevW, CharPrevExA, CharToOemA, OemToCharA, OemToCharBuffA, CharToOemBuffA, IsCharAlphaA, IsCharAlphaNumericA,
        IsCharUpperA, IsCharLowerA, IsCharAlphaW, wsprintfA, wvsprintfA, wsprintfW, wvsprintfW, GetGUIThreadInfo,
        AttachThreadInput, FlashWindow, GetProcessWindowStation, GetUserObjectInformationA, GetUserObjectInformationW, SetWinEventHook, UnhookWinEvent, SetProcessDPIAware,
        IsWindowUnicode, RegisterHotKey, UnregisterHotKey,
    );
    {
        use crate::keyboard::*;
        reg!("user32"; MapVirtualKeyA, MapVirtualKeyExA, ToUnicode, ToUnicodeEx, ToAscii, ToAsciiEx, VkKeyScanA, VkKeyScanW,
            VkKeyScanExA, VkKeyScanExW);
        implement("user32", "MapVirtualKeyW", MapVirtualKeyA);
        implement("user32", "MapVirtualKeyExW", MapVirtualKeyExA);
    }
    reg!("user32"; DdeInitializeA, DdeCreateStringHandleA, DdeCreateStringHandleW, DdeConnect, DdeGetLastError,
        DdeQueryStringA, DdeQueryStringW, DdeCmpStringHandles);
    implement("user32", "DdeInitializeW", DdeInitializeA);
    implement("comctl32", "_TrackMouseEvent", TrackMouseEvent);
    // per-monitor DPI: everything is 96 dpi
    implement("user32", "AdjustWindowRectExForDpi", AdjustWindowRectEx);
    // comctl32's flat scroll bars draw as the standard ones here: InitializeFlatSB succeeds and
    // each FlatSB_ call is its user32 counterpart (same arguments), as Windows does for a
    // window whose flat bars are not initialized (Delphi launchers call them on every list)
    implement("comctl32", "InitializeFlatSB", |_| 1);
    implement("comctl32", "UninitializeFlatSB", |_| 0);
    implement("comctl32", "FlatSB_SetScrollProp", |c| (c.a(1) & !0xfff == 0 && c.a(1) != 0) as u32);
    for (name, f) in [("FlatSB_GetScrollInfo", GetScrollInfo as Handler), ("FlatSB_SetScrollInfo", SetScrollInfo), ("FlatSB_GetScrollPos", GetScrollPos),
        ("FlatSB_SetScrollPos", SetScrollPos), ("FlatSB_GetScrollRange", GetScrollRange), ("FlatSB_SetScrollRange", SetScrollRange),
        ("FlatSB_ShowScrollBar", ShowScrollBar), ("FlatSB_EnableScrollBar", EnableScrollBar)] {
        implement("comctl32", name, f);
    }
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
mod message_box_tests {
    use super::{box_answer, msgboxparams};
    use proptest::prelude::*;

    /// MSGBOXPARAMSA/W as the SDK lays it out (winuser.h; x64 pads cbSize).
    #[test]
    fn msgboxparams_offsets() {
        assert_eq!(msgboxparams(4), (12, 16, 20));
        assert_eq!(msgboxparams(8), (24, 32, 40));
    }

    proptest! {
        /// Whatever the other uType bits (icons, default button, modality, flags), the answer is
        /// one of the box's own buttons and one that goes on (never IDABORT, IDRETRY, IDNO,
        /// IDTRYAGAIN), and IDOK for the types that have no other.
        #[test]
        fn answer_is_a_button_that_goes_on(style in any::<u32>()) {
            let buttons: &[u32] = match style & 0xf {
                1 => &[1, 2],
                2 => &[3, 4, 5],
                3 => &[6, 7, 2],
                4 => &[6, 7],
                5 => &[4, 2],
                6 => &[2, 10, 11],
                _ => &[1],
            };
            let a = box_answer(style);
            prop_assert!(buttons.contains(&a), "{style:#x}: {a}");
            prop_assert!(![3, 4, 7, 10].contains(&a));
            prop_assert_eq!(a, box_answer(style & 0xf));
        }
    }
}

#[cfg(test)]
mod radio_tests {
    use super::radio_spans;
    fn grid(checked: bool) -> [[u32; 13]; 13] {
        let mut g = [[0u32; 13]; 13];
        for (y, l, r, c) in radio_spans(checked) {
            for x in l..r {
                assert_eq!(g[y as usize][x as usize], 0, "spans overlap at {x},{y}");
                g[y as usize][x as usize] = c;
            }
        }
        g
    }
    /// A circle, not the check box's square: the corners are the background, the middle of each
    /// side the ring; checked only adds the dot, symmetric both ways.
    #[test]
    fn a_round_ring_with_a_dot() {
        for checked in [false, true] {
            let g = grid(checked);
            for (x, y) in [(0, 0), (12, 0), (0, 12), (12, 12), (1, 1)] {
                assert_eq!(g[y][x], 0, "corner {x},{y}");
            }
            for (x, y) in [(6, 0), (0, 6), (12, 6), (6, 12)] {
                assert_eq!(g[y][x], 0x333333, "ring {x},{y}");
            }
            assert_eq!(g[6][6], if checked { 0x202020 } else { 0xffffff });
            assert_eq!(g[2][6], 0xffffff);
            for y in 0..13 {
                for x in 0..13 {
                    assert_eq!(g[y][x], g[y][12 - x]);
                    assert_eq!(g[y][x], g[12 - y][x]);
                    assert_eq!(g[y][x], g[x][y]);
                }
            }
        }
        let (a, b) = (grid(false), grid(true));
        for y in 0..13 {
            for x in 0..13 {
                if a[y][x] != b[y][x] {
                    assert_eq!((a[y][x], b[y][x]), (0xffffff, 0x202020));
                }
            }
        }
    }
}

#[cfg(test)]
mod menu_bar_tests {
    use super::{client_of, dialog_window_size, menu_bar, HashMap, Menu, MenuItem};

    /// 俺の姿が､透明に!: GetMenu(TVPMainWindow) is a menu with no items on Windows, and its
    /// client is the whole window inside the frame: an empty menu draws no bar.
    #[test]
    fn only_menus_with_items_draw_a_bar() {
        let mut menus = HashMap::default();
        menus.insert(0x10, Menu { items: vec![] });
        menus.insert(0x20, Menu { items: vec![MenuItem::default()] });
        assert!(!menu_bar(&menus, 0, 0));
        assert!(!menu_bar(&menus, 0x10, 0x00cf_0000));
        assert!(menu_bar(&menus, 0x20, 0x00cf_0000));
        // a child's "menu" is its id
        assert!(!menu_bar(&menus, 0x20, 0x4000_0000));
        // (a handle sake does not know: a bar, as before)
        assert!(menu_bar(&menus, 0x30, 0));
    }

    /// タイムリープ (#192): a dialog template with a menu is sized so that its client area is
    /// the template's, below the bar; a captionless popup's client begins 20 below its top.
    #[test]
    fn a_dialog_menu_bar_sits_above_the_template_client() {
        use super::Rect;
        let style = 0x8600_0044;
        let (w, h) = dialog_window_size(style, 0x10000, 640, 480, true);
        assert_eq!((w, h), (640, 500));
        let c = client_of(&Rect { l: 0, t: -20, r: 640, b: 480 }, style, 0x10000, true);
        assert_eq!((c.l, c.t, c.w(), c.h()), (0, 20, 640, 480));
        assert_eq!(dialog_window_size(style, 0x10000, 640, 480, false), (640, 480));
    }

    proptest::proptest! {
        /// Any top-level dialog style, with or without a bar: the window CreateDialog makes has the
        /// template's client size, and the bar only moves the client down.
        #[test]
        fn dialog_client_is_the_template(style in proptest::num::u32::ANY, ex in proptest::num::u32::ANY, w in 0i32..4000, h in 0i32..4000, bar: bool) {
            let style = style & !0x4000_0000;
            let (ww, wh) = dialog_window_size(style, ex, w, h, bar);
            let c = client_of(&super::Rect { l: 0, t: 0, r: ww, b: wh }, style, ex, bar);
            proptest::prop_assert_eq!((c.w(), c.h()), (w, h));
            let c0 = client_of(&super::Rect { l: 0, t: 0, r: ww, b: wh }, style, ex, false);
            proptest::prop_assert_eq!(c.t - c0.t, if bar { 20 } else { 0 });
        }
    }
}

#[cfg(test)]
mod track_tests {
    use super::{track_clamp, tracks, Scroll};
    use proptest::prelude::*;

    /// 俺の姿が､透明に!: KiriKiri Z's 1920x1080 client with its frame on the 1366x768 screen
    /// grows the screen to hold the window, which then keeps its size.
    #[test]
    fn big_windows_grow_the_screen() {
        let s = (1366, 768);
        let grown_screen = |s, st, xy, size| super::grown_screen(s, s, st, xy, size);
        assert_eq!(grown_screen(s, OVERLAPPED, (0, 0), (1926, 1109)), Some((1926, 1109)));
        // (CW_USEDEFAULT's x: from the left edge)
        assert_eq!(grown_screen(s, OVERLAPPED, (i32::MIN, i32::MIN), (1926, 1109)), Some((1926, 1109)));
        assert_eq!(grown_screen(s, OVERLAPPED, (100, 50), (1926, 1109)), Some((2026, 1159)));
        // only the width too big: the height as tall as the window needs
        assert_eq!(grown_screen(s, OVERLAPPED, (0, 0), (2000, 700)), Some((2000, 768)));
        // what fits (AdvHD's 1280x720 with its frame, up to the tracking size) leaves it
        assert_eq!(grown_screen(s, OVERLAPPED, (0, 0), (1286, 749)), None);
        assert_eq!(grown_screen(s, OVERLAPPED, (500, 500), (1378, 780)), None);
        // popups are never kept to the screen, so never grow it
        assert_eq!(grown_screen(s, POPUP, (0, 0), (5000, 5000)), None);
        assert_eq!(grown_screen(s, OVERLAPPED, (0, 0), (90000, 90000)), Some((8192, 8192)));
    }

    /// 俺の姿が､透明に!: its 1936x1119 window grew the screen to 1976x1159 at (40, 40), then went
    /// to (391, 239), where its saved settings from a real PC put it: the screen holds it there
    /// too. A window that fits the display mode moved off the screen leaves the screen as it is.
    #[test]
    fn big_windows_moved_off_the_screen_grow_it() {
        let (mode, grown) = ((1366, 768), (1976, 1159));
        assert_eq!(super::grown_screen(mode, grown, OVERLAPPED, (391, 239), (1936, 1119)), Some((2327, 1358)));
        assert_eq!(super::grown_screen(mode, grown, OVERLAPPED, (40, 40), (1936, 1119)), None);
        assert_eq!(super::grown_screen(mode, grown, OVERLAPPED, (3000, 3000), (800, 600)), None);
        assert_eq!(super::grown_screen(mode, mode, OVERLAPPED, (3000, 3000), (1378, 780)), None);
        // (the screen never shrinks back when a big window moves left)
        assert_eq!(super::grown_screen(mode, (2327, 1358), OVERLAPPED, (0, 0), (1936, 1119)), None);
    }

    proptest! {
        /// A screen already grown past the display mode: it grows exactly when a window bigger
        /// than the mode reaches past it, then holds the window, and growing again does nothing.
        #[test]
        fn grown_screens_hold_moved_windows(mw in 640i32..2000, mh in 480i32..1500, gw in 0i32..1500, gh in 0i32..1000, x in -3000i32..4000, y in -3000i32..4000, cx in 0i32..4000, cy in 0i32..3000) {
            // (a screen grown past the mode before; any window, anywhere)
            let (mode, screen) = ((mw, mh), (mw + gw, mh + gh));
            let got = super::grown_screen(mode, screen, OVERLAPPED, (x, y), (cx, cy));
            let big = cx > mw + 12 || cy > mh + 12;
            let out = x.max(0) + cx > screen.0 || y.max(0) + cy > screen.1;
            prop_assert_eq!(got.is_some(), big && out);
            if let Some((w, h)) = got {
                prop_assert!(w >= screen.0 && h >= screen.1);
                prop_assert!(x.max(0) + cx <= w && y.max(0) + cy <= h);
                prop_assert_eq!(super::grown_screen(mode, (w, h), OVERLAPPED, (x, y), (cx, cy)), None);
            }
        }

        /// Grown, the screen holds the window: its tracking size no longer changes it (up to
        /// 8192), the screen never shrinks, and growing again does nothing.
        #[test]
        fn grown_screens_hold_the_window(sw in 640i32..3000, sh in 480i32..2000, x in -3000i32..3000, y in -3000i32..3000, cx in 0i32..9000, cy in 0i32..9000, style in proptest::prop_oneof![Just(OVERLAPPED), Just(0x00cf_0000), Just(POPUP), Just(POPUP | 0x0004_0000), Just(0x4000_0000)]) {
            let grown_screen = |s, st, xy, size| super::grown_screen(s, s, st, xy, size);
            match grown_screen((sw, sh), style, (x, y), (cx, cy)) {
                None => prop_assert!(!tracks(style) || (cx <= sw + 12 && cy <= sh + 12)),
                Some((w, h)) => {
                    prop_assert!(tracks(style));
                    prop_assert!(w >= sw && h >= sh && (w, h) != (sw, sh));
                    let max = (w + 12, h + 12);
                    let kept = track_clamp(style, (cx, cy), (0, 0), max);
                    prop_assert_eq!(kept, (cx.min(max.0), cy.min(max.1)));
                    if cx + x.max(0) <= 8192 && cy + y.max(0) <= 8192 {
                        prop_assert_eq!(kept, (cx, cy));
                        prop_assert!(x.max(0) + cx <= w && y.max(0) + cy <= h);
                    }
                    prop_assert_eq!(grown_screen((w, h), style, (x, y), kept), None);
                }
            }
        }
    }

    const OVERLAPPED: u32 = 0x00cb_0000; // WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX
    const POPUP: u32 = 0x8000_0000;

    /// winref user's "big" answers, with sake's 1366x768 screen (tracking size 1378x780).
    #[test]
    fn big_windows_as_windows_keeps_them() {
        let (min, max) = ((112, 27), (1378, 780));
        // the 俺の姿が､透明に! window: KiriKiri's 1920x1080 (and frame) made to fit
        assert_eq!(track_clamp(OVERLAPPED, (1926, 1109), min, max), (1378, 780));
        assert_eq!(track_clamp(0x00cf_0000, (5000, 5000), min, max), (1378, 780));
        assert_eq!(track_clamp(POPUP, (5000, 5000), min, max), (5000, 5000));
        assert_eq!(track_clamp(POPUP | 0x0004_0000, (5000, 5000), min, max), (1378, 780));
        assert_eq!(track_clamp(POPUP | 0x00c0_0000, (5000, 5000), min, max), (5000, 5000));
        assert_eq!(track_clamp(0x00cf_0000, (5000, 5000), min, (6000, 6000)), (5000, 5000));
        assert_eq!(track_clamp(0x00cf_0000, (200, 900), min, (300, 300)), (200, 300));
        // what fits is left as it is (AdvHD's 1280x720 with its frame)
        assert_eq!(track_clamp(OVERLAPPED, (1286, 749), min, max), (1286, 749));
        // grown to the least, unless minimized
        assert_eq!(track_clamp(OVERLAPPED, (10, 5), min, max), (112, 27));
        assert_eq!(track_clamp(OVERLAPPED | 0x2000_0000, (10, 5), min, max), (10, 5));
    }

    proptest! {
        /// A scroll bar's kept state (SetScrollInfo, SetScrollPos, SetScrollRange, and the
        /// FlatSB_ calls that are them): the page never exceeds the range, the position is in
        /// [min, max - (page - 1)] (min when that is empty), a position already there is kept,
        /// and clamping twice changes nothing.
        #[test]
        fn scroll_clamp_model(min in -300i32..300, len in -50i32..400, page in 0u32..600, pos in -1000i32..1000) {
            let max = min + len;
            let mut s = Scroll { min, max, page, pos, track: 0 };
            s.clamp();
            let span = (max as i64 - min as i64 + 1).max(0);
            prop_assert_eq!(s.page as i64, (page as i64).min(span));
            let top = (max as i64 - (s.page as i64 - 1).max(0)).max(min as i64);
            prop_assert!(s.pos as i64 >= min as i64 && s.pos as i64 <= top);
            if pos as i64 >= min as i64 && pos as i64 <= top {
                prop_assert_eq!(s.pos, pos);
            }
            let again = { let mut t = s; t.clamp(); t };
            prop_assert_eq!((again.page, again.pos), (s.page, s.pos));
        }
    }

    #[test]
    fn which_styles_are_kept() {
        assert!(tracks(0) && tracks(OVERLAPPED) && tracks(POPUP | 0x0004_0000) && tracks(0x4004_0000));
        assert!(!tracks(POPUP) && !tracks(0x4000_0000) && !tracks(POPUP | 0x00c0_0000));
    }

    proptest! {
        /// A kept size is within [min, max] (max winning where they cross, minimized windows
        /// only capped), never changes what already fits, and is the request for the others.
        #[test]
        fn clamp_model(style: u32, cx in -100i32..9000, cy in -100i32..9000, minx in 0i32..400, miny in 0i32..400, maxx in 0i32..9000, maxy in 0i32..9000) {
            let (rx, ry) = track_clamp(style, (cx, cy), (minx, miny), (maxx, maxy));
            if !tracks(style) {
                prop_assert_eq!((rx, ry), (cx, cy));
            } else {
                let mini = style & 0x2000_0000 != 0;
                let want = |v: i32, lo: i32, hi: i32| if mini { v.min(hi) } else { v.min(hi).max(lo) };
                prop_assert_eq!((rx, ry), (want(cx, minx, maxx), want(cy, miny, maxy)));
                if cx >= minx && cx <= maxx && cy >= miny && cy <= maxy {
                    prop_assert_eq!((rx, ry), (cx, cy));
                }
                // idempotent
                prop_assert_eq!(track_clamp(style, (rx, ry), (minx, miny), (maxx, maxy)), (rx, ry));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn shown_chain_cases() {
        // 1 top-level visible, 2 its visible child, 3 a hidden top-level, 4 a visible child of 3
        let w = |h: u32| match h {
            1 => Some((true, 0)),
            2 => Some((true, 1)),
            3 => Some((false, DESKTOP)),
            4 => Some((true, 3)),
            5 => Some((true, 6)),
            6 => Some((true, 5)),
            _ => None,
        };
        assert!(shown_chain(w, 1) && shown_chain(w, 2));
        assert!(!shown_chain(w, 3), "created without WS_VISIBLE");
        assert!(!shown_chain(w, 4), "visible inside a hidden parent");
        assert!(!shown_chain(w, 9), "no such window");
        assert!(!shown_chain(w, 5), "a parent cycle ends");
    }

    proptest! {
        /// Any forest of windows (parents earlier in the list or none): shown_chain agrees with
        /// the recursive definition: visible, and the parent (if any) shown.
        #[test]
        fn shown_chain_forest(ws in proptest::collection::vec((any::<bool>(), any::<u16>(), any::<bool>()), 1..30)) {
            // window i+1: (visible, parent index below i, top-level?)
            let parent = |i: usize| -> u32 {
                let (_, p, top) = ws[i];
                if top || i == 0 { 0 } else { (p as usize % i) as u32 + 1 }
            };
            let get = |h: u32| (h >= 1 && h as usize <= ws.len()).then(|| (ws[h as usize - 1].0, parent(h as usize - 1)));
            fn rec(get: &dyn Fn(u32) -> Option<(bool, u32)>, h: u32) -> bool {
                match get(h) {
                    Some((true, 0)) => true,
                    Some((true, p)) => rec(get, p),
                    _ => false,
                }
            }
            for h in 1..=ws.len() as u32 {
                prop_assert_eq!(shown_chain(get, h), rec(&get, h));
            }
        }
    }

    fn u(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn key_state_sides_and_generic() {
        let (mut k, mut p) = ([0u8; 256], [false; 256]);
        let lctrl = 0xa2 | 29 << 16;
        let rctrl = 0xa3 | 29 << 16;
        // WM_KEYDOWN VK_CONTROL (not VK_LCONTROL), both entries down
        assert_eq!(key_state(&mut k, &mut p, lctrl, true), (0x11, 0));
        assert_eq!((k[0x11] & 0x80, k[0xa2] & 0x80, k[0xa3] & 0x80), (0x80, 0x80, 0));
        assert!(p[0x11] && p[0xa2]);
        // held: a repeat (lParam bit 30), the toggle bits stay
        let toggles = (k[0x11] & 1, k[0xa2] & 1);
        assert_eq!(key_state(&mut k, &mut p, lctrl, true), (0x11, 1 << 30));
        assert_eq!((k[0x11] & 1, k[0xa2] & 1), toggles);
        // the right one too (extended), then the left let go: Ctrl is still down
        assert_eq!(key_state(&mut k, &mut p, rctrl, true), (0x11, 1 << 24));
        assert_eq!(key_state(&mut k, &mut p, lctrl, false), (0x11, 0));
        assert_eq!((k[0x11] & 0x80, k[0xa2] & 0x80, k[0xa3] & 0x80), (0x80, 0, 0x80));
        assert_eq!(key_state(&mut k, &mut p, rctrl, false), (0x11, 1 << 24));
        assert_eq!(k[0x11] & 0x80, 0);
        // Shift and Alt the same; other keys as they are
        assert_eq!(key_state(&mut k, &mut p, 0xa1 | 54 << 16, true).0, 0x10);
        assert_eq!(key_state(&mut k, &mut p, 0xa4 | 56 << 16, true), (0x12, 0));
        assert_eq!(key_state(&mut k, &mut p, 0xa5 | 56 << 16, true), (0x12, 1 << 24));
        assert_eq!(key_state(&mut k, &mut p, 0x0d | 28 << 16, true), (0x0d, 0));
        assert_eq!(key_state(&mut k, &mut p, 0x0d | 28 << 16, true), (0x0d, 1 << 30));
        // CapsLock's toggle flips per press, not per repeat
        key_state(&mut k, &mut p, 0x14, true);
        key_state(&mut k, &mut p, 0x14, true);
        key_state(&mut k, &mut p, 0x14, false);
        assert_eq!(k[0x14], 1);
        key_state(&mut k, &mut p, 0x14, true);
        key_state(&mut k, &mut p, 0x14, false);
        assert_eq!(k[0x14], 0);
    }

    proptest! {
        /// 催●学級's way of letting go of the keyboard (keybd_event KEYUP for every key that
        /// GetKeyboardState shows down, from 0 to 255): whatever was pressed before, no key is
        /// down afterwards, Shift/Ctrl/Alt and their sides included, and no toggle bit changed.
        #[test]
        fn releasing_every_down_key_clears_the_keyboard(presses in proptest::collection::vec((0u32..256, any::<bool>()), 0..40)) {
            let (mut k, mut p) = ([0u8; 256], [false; 256]);
            for (vk, down) in presses {
                key_state(&mut k, &mut p, vk, down);
            }
            let toggles: Vec<u8> = k.iter().map(|x| x & 1).collect();
            for vk in 0..256u32 {
                if k[vk as usize] & 0x80 != 0 {
                    key_state(&mut k, &mut p, vk, false);
                }
            }
            prop_assert!(k.iter().all(|x| x & 0x80 == 0), "{:?}", k.iter().enumerate().filter(|x| x.1 & 0x80 != 0).collect::<Vec<_>>());
            prop_assert_eq!(k.iter().map(|x| x & 1).collect::<Vec<u8>>(), toggles);
        }
    }

    proptest! {
        /// An SS_BITMAP static's picture: over the whole control with SS_REALSIZECONTROL (which
        /// wins over SS_CENTERIMAGE), else at the bitmap's own size, centred (each side within a
        /// pixel of the other) with SS_CENTERIMAGE, else from the top left. Other style bits
        /// change nothing.
        #[test]
        fn static_bitmap_placement(w in 0i32..2000, h in 0i32..2000, bw in 1i32..2000, bh in 1i32..2000, style in any::<u32>()) {
            let (x, y, cx, cy) = static_bitmap_rect(style, w, h, bw, bh);
            if style & 0x40 != 0 {
                prop_assert_eq!((x, y, cx, cy), (0, 0, w, h));
            } else {
                prop_assert_eq!((cx, cy), (bw, bh));
                if style & 0x200 != 0 {
                    prop_assert!((x - (w - (x + cx))).abs() <= 1 && (y - (h - (y + cy))).abs() <= 1);
                } else {
                    prop_assert_eq!((x, y), (0, 0));
                }
            }
            prop_assert_eq!(static_bitmap_rect(style & 0x240, w, h, bw, bh), (x, y, cx, cy));
        }
    }

    proptest! {
        /// Any run of the six side keys and a few others going down and up: the generic Shift /
        /// Ctrl / Alt is down exactly while one of its sides is, a message never names a side, and
        /// the repeat bit is set exactly on a down of a key already down.
        #[test]
        fn key_state_any_run(ops in proptest::collection::vec((0usize..9, any::<bool>()), 0..200)) {
            const KEYS: [u32; 9] = [0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5, 0x0d, 0x41, 0x14];
            let (mut k, mut p) = ([0u8; 256], [false; 256]);
            for (i, down) in ops {
                let vk = KEYS[i] as usize;
                let was = k[vk] & 0x80 != 0;
                let (w, flags) = key_state(&mut k, &mut p, KEYS[i] | 30 << 16, down);
                prop_assert!(!(0xa0..=0xa5).contains(&w));
                prop_assert_eq!(flags & (1 << 30) != 0, down && was);
                prop_assert_eq!(flags & (1 << 24) != 0, vk == 0xa3 || vk == 0xa5);
                prop_assert_eq!(k[vk] & 0x80 != 0, down);
                for g in 0..3 {
                    let either = k[0xa0 + 2 * g] & 0x80 != 0 || k[0xa1 + 2 * g] & 0x80 != 0;
                    prop_assert_eq!(k[0x10 + g] & 0x80 != 0, either);
                }
            }
        }
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

    #[test]
    fn release_gate_waits_for_another_sender_only() {
        let t0 = Instant::now();
        let m = |tid| waits::handoff::Mark { tid, seq: 3 };
        assert_eq!(release_gate(None, 8), None);
        // the same thread again, or another process (0) on either side: no one to wait for
        assert_eq!(release_gate(Some((8, t0, m(8))), 8), None);
        assert_eq!(release_gate(Some((0, t0, m(0))), 8), None);
        assert_eq!(release_gate(Some((8, t0, m(8))), 0), None);
        assert_eq!(release_gate(Some((8, t0, m(8))), 12), Some(m(8)));
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

#[cfg(test)]
mod menu_template_tests {
    use super::*;
    use crate::test_guest::enter;
    use proptest::prelude::*;

    /// A menu as a tree: (text, id, popup items)
    #[derive(Clone, Debug)]
    enum Item {
        Cmd(String, u16),
        Popup(String, Vec<Item>),
    }

    fn items() -> impl Strategy<Value = Vec<Item>> {
        let leaf = ("[a-zあ-ん]{0,6}", 1u16..0x7fff).prop_map(|(t, id)| Item::Cmd(t, id));
        let item = leaf.prop_recursive(4, 40, 5, |inner| {
            prop_oneof![
                ("[a-z]{0,6}", 1u16..0x7fff).prop_map(|(t, id)| Item::Cmd(t, id)),
                ("[a-z]{1,6}", proptest::collection::vec(inner, 1..5)).prop_map(|(t, v)| Item::Popup(t, v)),
            ]
        });
        proptest::collection::vec(item, 1..6)
    }

    fn text(out: &mut Vec<u8>, t: &str) {
        for c in t.encode_utf16().chain([0]) {
            out.extend_from_slice(&c.to_le_bytes());
        }
    }
    /// MENU (MENUITEMTEMPLATE) items as RC writes them: flags (0x10 popup, 0x80 last), id unless
    /// a popup, text; a popup's items follow it
    fn menu_bytes(out: &mut Vec<u8>, v: &[Item]) {
        for (i, it) in v.iter().enumerate() {
            let last = if i + 1 == v.len() { 0x80u16 } else { 0 };
            match it {
                Item::Cmd(t, id) => {
                    out.extend_from_slice(&last.to_le_bytes());
                    out.extend_from_slice(&id.to_le_bytes());
                    text(out, t);
                }
                Item::Popup(t, sub) => {
                    out.extend_from_slice(&(0x10 | last).to_le_bytes());
                    text(out, t);
                    menu_bytes(out, sub);
                }
            }
        }
    }
    /// MENUEX items: type, state, id, flags word (1 popup, 0x80 last), text, dword aligned; a
    /// popup's help id, then its items
    fn menuex_bytes(out: &mut Vec<u8>, v: &[Item]) {
        for (i, it) in v.iter().enumerate() {
            let last = if i + 1 == v.len() { 0x80u16 } else { 0 };
            let (t, id, sub) = match it {
                Item::Cmd(t, id) => (t, *id as u32, None),
                Item::Popup(t, s) => (t, 0, Some(s)),
            };
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&0u32.to_le_bytes());
            out.extend_from_slice(&id.to_le_bytes());
            out.extend_from_slice(&(last | if sub.is_some() { 1 } else { 0 }).to_le_bytes());
            text(out, t);
            while out.len() % 4 != 0 {
                out.push(0);
            }
            if let Some(s) = sub {
                out.extend_from_slice(&0u32.to_le_bytes());
                menuex_bytes(out, s);
            }
        }
    }
    fn template(v: &[Item], ex: bool) -> Vec<u8> {
        let mut out = Vec::new();
        if ex {
            out.extend_from_slice(&[1, 0, 4, 0, 0, 0, 0, 0]); // version 1, offset 4, help id
            menuex_bytes(&mut out, v);
        } else {
            out.extend_from_slice(&[0, 0, 0, 0]);
            menu_bytes(&mut out, v);
        }
        out
    }
    /// The menu `h` made, as the tree it came from (`depth` levels deep at most)
    fn check(h: u32, v: &[Item], depth: u32) -> Result<(), TestCaseError> {
        let got = with(|s| s.menus.get(&h).map(|m| m.items.clone())).expect("menu");
        if depth > MENU_DEPTH + 1 {
            prop_assert!(got.is_empty());
            return Ok(());
        }
        prop_assert_eq!(got.len(), v.len());
        for (g, want) in got.iter().zip(v) {
            match want {
                Item::Cmd(t, id) => {
                    prop_assert_eq!(&g.text, &t.encode_utf16().collect::<Vec<_>>());
                    prop_assert_eq!(g.sub, 0);
                    if !t.is_empty() {
                        prop_assert_eq!(g.id, *id as u32);
                    }
                }
                Item::Popup(t, sub) => {
                    prop_assert_eq!(&g.text, &t.encode_utf16().collect::<Vec<_>>());
                    prop_assert!(g.sub != 0);
                    prop_assert_eq!(g.flags & 0x10, 0x10);
                    check(g.sub, sub, depth + 1)?;
                }
            }
        }
        Ok(())
    }
    fn load(bytes: &[u8]) -> u32 {
        let p = mem::heap_alloc(bytes.len() as u32 + 64, true);
        mem::write(p, bytes);
        let h = menu_from_template(p);
        mem::heap_free(p);
        h
    }

    proptest! {
        /// Any menu tree written as a MENU or MENUEX template loads back as the same menus
        /// (items, texts, ids, popups in order); the class menu of CreateWindow and LoadMenu
        /// both come through menu_from_template
        #[test]
        fn templates_load_as_written(v in items(), ex in any::<bool>()) {
            let _g = enter();
            let h = load(&template(&v, ex));
            check(h, &v, 1)?;
        }

        /// Arbitrary bytes after a valid header, as a resource of that size (a damaged game
        /// resource: no last item, popups never closed, nesting past MENU_DEPTH) never read past
        /// the resource nor take sake down
        #[test]
        fn template_garbage_stays_inside(mut b in proptest::collection::vec(any::<u8>(), 0..512), ex in any::<bool>()) {
            let _g = enter();
            let head: &[u8] = if ex { &[1, 0, 4, 0, 0, 0, 0, 0] } else { &[0, 0, 0, 0] };
            let mut t = head.to_vec();
            t.append(&mut b);
            // (texts end inside: a zero word at the end, as any resource's padding)
            t.extend_from_slice(&[0, 0]);
            let p = mem::heap_alloc(t.len() as u32, true);
            mem::write(p, &t);
            let end = p + t.len() as u32;
            let h = menu_from_resource((p, t.len() as u32));
            prop_assert!(with(|s| s.menus.contains_key(&h)));
            let at = if ex { menuex_items_at(new_menu(), p + 8, end, 0) } else { menu_items_at(new_menu(), p + 4, end, 0) };
            prop_assert!(at <= end + 2 * 256, "parsing stopped at {at:#x}, the resource ends at {end:#x}");
            mem::heap_free(p);
        }
    }

    const DEEP: usize = 5000;
    /// A template nesting popups far deeper than any menu (a damaged or hostile resource) loads
    /// without overflowing sake's stack: the levels below MENU_DEPTH are empty, and parsing
    /// still ends where the template does
    #[test]
    fn deep_nesting_is_bounded() {
        let _g = enter();
        for ex in [false, true] {
            let mut v = vec![Item::Cmd("leaf".into(), 7)];
            for i in 0..DEEP {
                v = vec![Item::Popup(format!("p{}", i % 10), v), Item::Cmd("after".into(), 9)];
            }
            let b = template(&v, ex);
            let p = mem::heap_alloc(b.len() as u32 + 64, true);
            mem::write(p, &b);
            let end = if ex { menuex_items(new_menu(), p + 8) } else { menu_items(new_menu(), p + 4) };
            assert_eq!(end, p + b.len() as u32, "ex {ex}: parsing ends at the template's end");
            mem::heap_free(p);
            let h = load(&b);
            let mut cur = h;
            let mut depth = 1;
            loop {
                let items = with(|s| s.menus[&cur].items.clone());
                if depth > MENU_DEPTH + 1 {
                    assert!(items.is_empty());
                    break;
                }
                assert_eq!(items.len(), 2);
                assert_eq!(items[1].text, "after".encode_utf16().collect::<Vec<_>>());
                cur = items[0].sub;
                depth += 1;
            }
        }
    }
}

#[cfg(test)]
mod edit_text_tests {
    use super::edit_text_box;
    use proptest::prelude::*;

    /// YU-RIS's error dialog: a read-only multiline edit (0x50a01804) with several CRLF lines
    /// drew them all on one line, centred vertically. Multiline starts at the top and breaks;
    /// one line stays centred.
    #[test]
    fn multiline_edits_break_from_the_top() {
        let (r, f) = edit_text_box(0x50a0_1804, 300, 120);
        assert_eq!((r.l, r.t, r.r, r.b), (4, 2, 296, 118));
        assert_eq!(f & 0x20, 0, "not DT_SINGLELINE");
        assert_ne!(f & 0x10, 0, "DT_WORDBREAK");
        assert_eq!(f & 0x4, 0, "not DT_VCENTER");
        let (r, f) = edit_text_box(0x5081_0080, 300, 24);
        assert_eq!((r.l, r.t, r.r, r.b), (4, 0, 296, 24));
        assert_eq!(f & (0x20 | 0x4), 0x20 | 0x4);
    }

    proptest! {
        /// Any style: never a prefix, the alignment from ES_CENTER/ES_RIGHT, single-line
        /// exactly when ES_MULTILINE is clear, and the text box inside the control.
        #[test]
        fn edit_layout_follows_the_style(style in any::<u32>(), w in 0i32..4000, h in 0i32..4000) {
            let (r, f) = edit_text_box(style, w, h);
            prop_assert_ne!(f & 0x800, 0);
            prop_assert_eq!(f & 3, match style & 3 { 1 => 1, 2 => 2, _ => 0 });
            prop_assert_eq!(f & 0x20 != 0, style & 4 == 0);
            prop_assert_eq!(f & 0x10 != 0, style & 4 != 0);
            prop_assert!(r.l >= 0 && r.t >= 0 && r.r <= w.max(r.l) + 4 && r.b <= h);
            prop_assert_eq!((r.l, r.r), (4, w - 4));
        }
    }
}

#[cfg(test)]
mod display_mode_tests {
    use super::display_modes;
    use proptest::prelude::*;

    #[test]
    fn usual_screens() {
        let m = display_modes((1920, 1080));
        assert_eq!(m.first(), Some(&(640, 480)));
        assert_eq!(m.last(), Some(&(1920, 1080)));
        assert!(m.contains(&(1024, 768)) && !m.contains(&(2560, 1440)));
        assert_eq!(display_modes((1366, 768)).last(), Some(&(1366, 768)));
        assert_eq!(display_modes((800, 600)), vec![(640, 480), (800, 600)]);
        // (a screen smaller than every usual size: just its own)
        assert_eq!(display_modes((320, 240)), vec![(320, 240)]);
    }

    proptest! {
        /// for any screen the list holds the screen's own size exactly once, holds no mode larger
        /// than the screen in either direction, and is sorted without repeats
        #[test]
        fn current_mode_is_listed(w in 1i32..8000, h in 1i32..5000) {
            let m = display_modes((w, h));
            prop_assert_eq!(m.iter().filter(|&&x| x == (w, h)).count(), 1);
            prop_assert!(m.iter().all(|&(x, y)| x <= w && y <= h));
            prop_assert!(m.windows(2).all(|p| p[0] < p[1]));
        }
    }
}

#[cfg(test)]
mod user_object_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    #[test]
    fn flags_and_names() {
        // the window station is visible (WSF_VISIBLE), the desktop has no WSF flag
        assert_eq!(user_object_info(WINSTA, 1, false).unwrap(), [0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(user_object_info(0x60, 1, true).unwrap(), [0; 12]);
        assert_eq!(user_object_info(WINSTA, 2, false).unwrap(), b"WinSta0\0");
        assert_eq!(user_object_info(0x60, 3, true).unwrap(), "Desktop\0".encode_utf16().flat_map(|u| u.to_le_bytes()).collect::<Vec<_>>());
        assert!(user_object_info(WINSTA, 4, false).is_none());
    }

    proptest! {
        /// whatever the buffer size: the needed size is always told, the call succeeds exactly
        /// when the buffer holds the answer, and nothing is written past `size` or on failure
        #[test]
        fn buffer_sizes(index in 0u32..6, wide in any::<bool>(), size in 0u32..40, desk in any::<bool>()) {
            let _g = enter();
            crate::test_guest::guest_thread();
            let h = if desk { 0x60 } else { WINSTA };
            let (buf, needed) = (mem::heap_alloc(64, true), mem::heap_alloc(4, true));
            for i in 0..64 { mem::w8(buf + i, 0xcc); }
            mem::w32(needed, 0xdead);
            let f = if wide { GetUserObjectInformationW } else { GetUserObjectInformationA };
            let r = call(f, &[h, index, buf, size, needed]);
            match user_object_info(h, index, wide) {
                None => {
                    prop_assert_eq!(r, 0);
                    prop_assert_eq!(mem::r32(needed), 0xdead);
                    prop_assert_eq!(crate::k32::get_last_error(), 87);
                }
                Some(d) => {
                    prop_assert_eq!(mem::r32(needed), d.len() as u32);
                    prop_assert_eq!(r, (size as usize >= d.len()) as u32);
                    if r == 0 {
                        prop_assert_eq!(crate::k32::get_last_error(), 122);
                    }
                    for i in 0..64u32 {
                        let want = if r == 1 && (i as usize) < d.len() { d[i as usize] } else { 0xcc };
                        prop_assert_eq!(mem::r8(buf + i), want);
                    }
                }
            }
            mem::heap_free(buf);
            mem::heap_free(needed);
        }
    }

    #[test]
    fn win_event_hooks() {
        let _g = enter();
        let (a, b) = (call(SetWinEventHook, &[3, 3, 0, 0x1000, 0, 0, 0]), call(SetWinEventHook, &[3, 3, 0, 0x1000, 0, 0, 0]));
        assert!(a != 0 && b != 0 && a != b);
        assert_eq!(call(UnhookWinEvent, &[a]), 1);
        assert_eq!(call(UnhookWinEvent, &[0]), 0);
    }
}

#[cfg(test)]
mod def_button_search {
    use super::*;
    use proptest::prelude::*;

    /// A random window tree: per window (parent index or none, default button, searched into).
    fn tree() -> impl Strategy<Value = Vec<(Option<usize>, bool, bool)>> {
        prop::collection::vec((any::<prop::sample::Index>(), any::<bool>(), any::<bool>(), any::<bool>()), 1..40).prop_map(|v| {
            v.into_iter()
                .enumerate()
                .map(|(i, (p, top, b, into))| (if i == 0 || top { None } else { Some(p.index(i)) }, b, into))
                .collect()
        })
    }

    proptest! {
        /// DM_GETDEFID's search is the first default button of a preorder walk (explicit
        /// stack) that enters only the windows to search into.
        #[test]
        fn first_in_preorder(t in tree()) {
            let kids = |p: Option<usize>| -> Vec<u32> { t.iter().enumerate().filter(|x| x.1 .0 == p).map(|x| x.0 as u32 + 1).collect() };
            let node = |k: u32| {
                let (_, button, into) = t[k as usize - 1];
                Some(DefNode { id: k + 100, button, into, children: if into { kids(Some(k as usize - 1)) } else { Vec::new() } })
            };
            let got = def_search(&kids(None), &node, 0);
            // reference: walk, stack of pending windows (reversed so the first child pops first)
            let mut stack: Vec<(u32, u32)> = kids(None).into_iter().rev().map(|k| (k, 0)).collect();
            let mut want = None;
            while let Some((k, d)) = stack.pop() {
                let (_, button, into) = t[k as usize - 1];
                if button { want = Some((k + 100, k)); break; }
                if into && d < 16 {
                    for c in kids(Some(k as usize - 1)).into_iter().rev() { stack.push((c, d + 1)); }
                }
            }
            prop_assert_eq!(got, want);
            if let Some((_, k)) = got {
                // (found through windows to search into only)
                let mut p = t[k as usize - 1].0;
                while let Some(i) = p { prop_assert!(t[i].2); p = t[i].0; }
            }
        }
    }
}

#[cfg(test)]
mod icon_resource_tests {
    use super::icon_from_resource;
    use proptest::prelude::*;

    fn dib(header: u32, extra: usize) -> Vec<u8> {
        let mut v = header.to_le_bytes().to_vec();
        v.resize(header as usize + extra, 0);
        v
    }

    #[test]
    fn icons_and_cursors() {
        assert_eq!(icon_from_resource(&dib(40, 64), true, 0x3_0000), Some(None));
        let mut cur = vec![3, 0, 7, 0];
        cur.extend(dib(40, 64));
        assert_eq!(icon_from_resource(&cur, false, 0x3_0000), Some(Some((3, 7))), "a cursor: its hotspot first");
        assert_eq!(icon_from_resource(b"\x89PNG\r\n\x1a\nrest", true, 0x3_0000), Some(None));
        assert_eq!(icon_from_resource(&dib(40, 64), true, 0x1_0000), None, "version");
        assert_eq!(icon_from_resource(&dib(40, 64), true, 0x4_0000), None);
        assert_eq!(icon_from_resource(&dib(40, 0)[..20], true, 0x3_0000), None, "shorter than its header");
        assert_eq!(icon_from_resource(&[1, 2], false, 0x3_0000), None);
    }

    proptest! {
        /// Any bytes: no panic; accepted only with a header (or PNG signature) where one belongs.
        #[test]
        fn any_bytes(bits in proptest::collection::vec(any::<u8>(), 0..200), icon in any::<bool>(), v in 0u32..0x5_0000) {
            if let Some(hot) = icon_from_resource(&bits, icon, v) {
                prop_assert!((0x2_0000..=0x3_0000).contains(&v));
                prop_assert_eq!(hot.is_some(), !icon);
                let d = if icon { &bits[..] } else { &bits[4..] };
                prop_assert!(d.starts_with(b"\x89PNG\r\n\x1a\n") || d.len() >= 4 && d.len() >= u32::from_le_bytes([d[0], d[1], d[2], d[3]]) as usize);
            }
        }
    }
}

#[cfg(test)]
mod client_edge_tests {
    use super::{client_edge, client_of, Rect};
    use proptest::prelude::*;

    /// a 40x30 child with WS_EX_CLIENTEDGE: the 2-pixel ring, shadow (16) and dark shadow (21)
    /// at the top and left, highlight (20) and light (22) at the bottom and right
    #[test]
    fn sunken_edge_of_a_child() {
        let p = client_edge(0x5000_0000, 0x200, 40, 30);
        let at = |x: i32, y: i32| p.iter().rev().find(|(r, _)| x >= r.l && x < r.r && y >= r.t && y < r.b).map(|e| e.1);
        assert_eq!((at(0, 0), at(5, 0), at(0, 5)), (Some(16), Some(16), Some(16)));
        assert_eq!((at(39, 5), at(5, 29), at(39, 29)), (Some(20), Some(20), Some(20)));
        assert_eq!((at(1, 1), at(5, 1), at(1, 5)), (Some(21), Some(21), Some(21)));
        assert_eq!((at(38, 5), at(5, 28)), (Some(22), Some(22)));
        assert_eq!(at(2, 2), None);
        assert!(client_edge(0x5000_0000, 0, 40, 30).is_empty());
    }

    proptest! {
        /// the edge is exactly the ring between the window and its client area: every pixel
        /// of it is drawn, nothing inside the client area or outside the window is
        #[test]
        fn the_edge_fills_the_ring(w in 1i32..40, h in 1i32..40, border: bool) {
            let style = 0x5000_0000 | if border { 0x0080_0000 } else { 0 };
            let parts = client_edge(style, 0x200, w, h);
            let win = Rect { l: 0, t: 0, r: w, b: h };
            let client = client_of(&win, style, 0x200, false);
            let off = if border { 1 } else { 0 };
            for y in 0..h {
                for x in 0..w {
                    let drawn = parts.iter().any(|(r, _)| x >= r.l && x < r.r && y >= r.t && y < r.b);
                    let in_client = x >= client.l && x < client.r && y >= client.t && y < client.b;
                    let in_ring = x >= off && y >= off && x < w - off && y < h - off && !in_client;
                    if in_client { prop_assert!(!drawn, "client pixel {},{} drawn", x, y); }
                    if drawn { prop_assert!(in_ring || w - 2 * off < 4 || h - 2 * off < 4, "{},{} outside the ring", x, y); }
                    if in_ring && w - 2 * off >= 4 && h - 2 * off >= 4 { prop_assert!(drawn, "ring pixel {},{} not drawn", x, y); }
                }
            }
        }
    }
}
