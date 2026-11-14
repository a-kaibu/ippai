//! The screen behind sake's windows.
//!
//! Top-level windows hand their pixels here when sake flushes their surface, and the screen is composed from them in z-order (bottom to top).
//! The page shows `vnhost_display_frame` (one BGRA buffer of the whole screen) and sends input
//! with `vnhost_input_push`; the program's input thread takes them with `input_take`.
//!
//! Under node, `IPPAI_SCREENSHOT=<file.ppm>` writes the screen out (see `start_screenshots`) and
//! `IPPAI_INPUT="<seconds>:<kind>,<x>,<y>,<data>;..."` sends input that many seconds after the
//! first window shows up (see `start_scripted_input`).

use std::collections::{HashMap, VecDeque};
use std::ffi::c_int;
use std::sync::Mutex;

#[derive(Clone, Copy, Default, PartialEq, Debug)]
struct Rect {
    l: i32,
    t: i32,
    r: i32,
    b: i32,
}

impl Rect {
    fn w(&self) -> i32 {
        self.r - self.l
    }
    fn h(&self) -> i32 {
        self.b - self.t
    }
    fn empty(&self) -> bool {
        self.l >= self.r || self.t >= self.b
    }
    fn and(&self, o: &Rect) -> Rect {
        Rect { l: self.l.max(o.l), t: self.t.max(o.t), r: self.r.min(o.r), b: self.b.min(o.b) }
    }
    fn or(&self, o: &Rect) -> Rect {
        if self.empty() {
            return *o;
        }
        if o.empty() {
            return *self;
        }
        Rect { l: self.l.min(o.l), t: self.t.min(o.t), r: self.r.max(o.r), b: self.b.max(o.b) }
    }
}

struct Win {
    /// screen position of the window's surface
    rect: Rect,
    /// its client area on the screen
    client: Rect,
    visible: bool,
    /// its pixels; while it shows a `shared` surface, what of it the screen was last told
    /// (`vnhost_window_draw` compares new pixels with them: only what changed is redrawn)
    pixels: Vec<u32>,
    /// sake: the surface the window shows instead of `pixels` (rect-sized rows, owned by the
    /// caller; see `vnhost_window_draw_shared`), 0 for none
    shared: usize,
}

impl Win {
    /// the window's pixels, rect-sized rows
    fn px(&self) -> &[u32] {
        if self.shared != 0 {
            let n = self.rect.w().max(0) as usize * self.rect.h().max(0) as usize;
            unsafe { std::slice::from_raw_parts(self.shared as *const u32, n) }
        } else {
            &self.pixels
        }
    }
}

struct Display {
    w: i32,
    h: i32,
    wins: HashMap<u32, Win>,
    /// bottom to top
    order: Vec<u32>,
    screen: Vec<u32>,
    serial: u32,
    /// the area each of the last few serials redrew, newest last, and the window whose own
    /// pixels they are when `screen` was not updated (see `direct`)
    bands: std::collections::VecDeque<(Rect, u32)>,
    /// where `screen` is behind its windows: the areas drawn `direct`ly since it was last made
    /// whole (see `sync`)
    stale: Rect,
    /// what `screen` holds, in screen coordinates: the screen and every window shown on it,
    /// whole (a game's window partly off the screen is shown whole: `extent_of`)
    ext: Rect,
}

static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Display) -> R) -> R {
    let mut g = DISPLAY.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| {
        let (w, h) = std::env::var("IPPAI_SCREEN")
            .ok()
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some((w.parse().ok()?, h.parse().ok()?))
            })
            // the commonest laptop screen: games that need a 1280x720 window with its frame
            // (AdvHD: "screen smaller than 1280x720") fit; Siglus takes 75% of the width
            .unwrap_or((1366, 768));
        Display {
            w,
            h,
            wins: HashMap::new(),
            order: Vec::new(),
            screen: vec![0xff00_0000; (w * h) as usize],
            serial: 0,
            bands: Default::default(),
            stale: Rect::default(),
            ext: Rect { l: 0, t: 0, r: w, b: h },
        }
    }))
}

/// `IPPAI_SCREENSHOT`: a thread writes the screen out whenever it has changed, once a second.
fn start_screenshots() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    let Ok(path) = std::env::var("IPPAI_SCREENSHOT") else { return };
    STARTED.call_once(|| {
        std::thread::spawn(move || {
            let mut written = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let shot = with(|d| {
                    d.sync();
                    (d.serial != written).then(|| {
                        written = d.serial;
                        let mut out = format!("P6\n{} {}\n255\n", d.ext.w(), d.ext.h()).into_bytes();
                        out.reserve(d.screen.len() * 3);
                        for p in &d.screen {
                            out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
                        }
                        out
                    })
                });
                if let Some(out) = shot {
                    let tmp = format!("{path}.tmp");
                    if std::fs::write(&tmp, &out).is_ok() {
                        let _ = std::fs::rename(&tmp, &path);
                    }
                }
            }
        });
    });
}

/// `IPPAI_INPUT`: input records sent at given times (for tests without a page).
fn start_scripted_input() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    let Ok(script) = std::env::var("IPPAI_INPUT") else { return };
    STARTED.call_once(|| {
        std::thread::spawn(move || {
            let t0 = std::time::Instant::now();
            for ev in script.split(';') {
                let Some((at, rec)) = ev.split_once(':') else { continue };
                let v: Vec<i64> = rec.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                let (Ok(at), &[kind, x, y, data]) = (at.parse::<f64>(), v.as_slice()) else { continue };
                let wait = std::time::Duration::from_secs_f64(at).saturating_sub(t0.elapsed());
                std::thread::sleep(wait);
                vnhost_input_push(kind as u32, x as i32, y as i32, data as u32);
                if crate::env_on!("IPPAI_INPUT_TRACE") {
                    let n = input_queued() * 16;
                    // and the windows there, top first
                    let under: Vec<String> = with(|d| {
                        d.order
                            .iter()
                            .rev()
                            .filter(|id| {
                                let r = d.wins[id].rect;
                                d.wins[id].visible && x >= r.l as i64 && x < r.r as i64 && y >= r.t as i64 && y < r.b as i64
                            })
                            .map(|id| format!("{id:x}{:?}", d.wins[id].rect))
                            .collect()
                    });
                    eprintln!("vnvm input {at}: {kind},{x},{y},{data} ({n} bytes unread) over {}", under.join(" "));
                }
            }
        });
    });
}

impl Display {
    /// What the page shows: the client area of the game's window and nothing else (no title
    /// bar, frame, desktop or other windows around it; windows over it show where they are over
    /// it): the shown window with the biggest client area on the screen, whole, also what of it
    /// is off the screen (a window placed where a bigger screen had it: AMBITIOUS MISSION's at
    /// x 850 of 1366). The whole screen without windows.
    fn view(&self) -> Rect {
        let screen = self.screen_rect();
        let shown = |id: &u32| self.wins[id].visible && !self.wins[id].px().is_empty();
        let area = |id: &u32| {
            let c = self.wins[id].client.and(&screen);
            if c.empty() { 0 } else { c.w() as i64 * c.h() as i64 }
        };
        let Some(main) = self.order.iter().filter(|id| shown(id)).max_by_key(|id| area(id)) else { return screen };
        if area(main) == 0 {
            return screen;
        }
        self.wins[main].client.and(&self.ext)
    }

    /// What `screen` holds: the screen, widened to the whole of every window shown with a part
    /// on it (one wholly off the screen is hidden there: never shown), within `MAX_EXTENT`
    /// pixels each way.
    fn extent_of(&self) -> Rect {
        let screen = self.screen_rect();
        let mut e = screen;
        for w in self.wins.values() {
            if w.visible && !w.rect.empty() && !w.rect.and(&screen).empty() {
                e = e.or(&w.rect);
            }
        }
        // (around the screen: as much on each side as it allows)
        let (mw, mh) = ((MAX_EXTENT - screen.w()).max(0), (MAX_EXTENT - screen.h()).max(0));
        let l = e.l.max(-(mw / 2));
        let t = e.t.max(-(mh / 2));
        Rect { l, t, r: e.r.min(l + MAX_EXTENT).max(screen.r), b: e.b.min(t + MAX_EXTENT).max(screen.b) }
    }

    /// `screen`'s index of screen pixel (x, y) (inside `ext`)
    fn at_index(&self, x: i32, y: i32) -> usize {
        (y - self.ext.t) as usize * self.ext.w() as usize + (x - self.ext.l) as usize
    }

    /// `screen` made to hold `extent_of` when that changed (all of it composed again); whether
    /// it did.
    fn fit_extent(&mut self) -> bool {
        let e = self.extent_of();
        if e == self.ext {
            return false;
        }
        self.ext = e;
        self.screen = vec![0xff00_0000; (e.w() * e.h()) as usize];
        self.bands.clear();
        self.stale = Rect::default();
        self.compose(e);
        true
    }

    fn screen_rect(&self) -> Rect {
        Rect { l: 0, t: 0, r: self.w, b: self.h }
    }

    /// Redraws part of the screen from the windows.
    /// The window that shows all of `area` with nothing above it there, when its pixels are its
    /// surface (sake): the page uploads `area` from that surface itself.
    fn direct(&self, area: &Rect) -> Option<u32> {
        let shown = |w: &Win| w.visible && !w.px().is_empty();
        let &top = self.order.iter().rev().find(|id| shown(&self.wins[id]) && !self.wins[id].rect.and(area).empty())?;
        let w = &self.wins[&top];
        (w.shared != 0 && w.rect.and(area) == *area).then_some(top)
    }

    /// Makes `screen` whole where direct draws left it behind.
    fn sync(&mut self) {
        let stale = std::mem::take(&mut self.stale);
        if !stale.empty() {
            self.copy_in(stale);
        }
    }

    fn compose(&mut self, area: Rect) {
        let area = area.and(&self.ext);
        if area.empty() {
            return;
        }
        // the common case, a game window redrawing itself: no copy into `screen`
        if let Some(id) = self.direct(&area) {
            self.stale = self.stale.or(&area);
            self.band(area, id);
            return;
        }
        self.sync();
        self.copy_in(area);
        self.band(area, 0);
    }

    fn band(&mut self, area: Rect, direct: u32) {
        self.serial = self.serial.wrapping_add(1);
        if self.bands.len() == 64 {
            self.bands.pop_front();
        }
        self.bands.push_back((area, direct));
    }

    /// Composes `area` of `screen` from the windows.
    fn copy_in(&mut self, area: Rect) {
        // from the topmost window that covers the whole area (the black background otherwise)
        let shown = |w: &Win| w.visible && !w.px().is_empty();
        let covers = |w: &Win| shown(w) && w.rect.and(&area) == area;
        let first = self.order.iter().rposition(|id| covers(&self.wins[id]));
        if first.is_none() {
            for y in area.t..area.b {
                let row = self.at_index(area.l, y);
                self.screen[row..row + area.w() as usize].fill(0xff00_0000);
            }
        }
        for id in &self.order[first.unwrap_or(0)..] {
            let win = &self.wins[id];
            let r = win.rect.and(&area);
            if !win.visible || r.empty() || win.px().is_empty() {
                continue;
            }
            let ww = win.rect.w() as usize;
            for y in r.t..r.b {
                let src = (y - win.rect.t) as usize * ww + (r.l - win.rect.l) as usize;
                let dst = (y - self.ext.t) as usize * self.ext.w() as usize + (r.l - self.ext.l) as usize;
                let n = r.w() as usize;
                self.screen[dst..dst + n].copy_from_slice(&win.px()[src..src + n]);
            }
        }
    }
}

/// How big `screen` may get each way (it holds the screen and the windows on it).
const MAX_EXTENT: i32 = 8192;

/// The one monitor's HMONITOR, as user32 (MonitorFromWindow, EnumDisplayMonitors), DXGI
/// (IDXGIOutput::GetDesc) and d3d9 (GetAdapterMonitor) all answer it: Unity 6 matches the
/// window's monitor against the DXGI outputs to number the display its pointer is on, and with
/// no match uGUI dropped every click and hover as being on another display. 0x10001 as Windows'
/// first monitor.
pub const HMONITOR: u32 = 0x10001;

/// A display mode change (ChangeDisplaySettings): the screen takes the new size.
#[no_mangle]
pub extern "C" fn vnhost_display_set_size(w: c_int, h: c_int) {
    with(|d| d.set_size(w, h))
}

/// The part of the screen the page shows (see `Display::view`): `out` gets l, t, r, b.
#[no_mangle]
pub unsafe extern "C" fn vnhost_display_view(out: *mut i32) {
    let v = with(|d| d.view());
    std::slice::from_raw_parts_mut(out, 4).copy_from_slice(&[v.l, v.t, v.r, v.b]);
}

/// The visible top-level windows, topmost first: `out` gets l, t, r, b of up to `max` of them;
/// returns how many. (The page draws the ones above a Direct3D window over its frames.)
#[no_mangle]
pub unsafe extern "C" fn vnhost_display_windows(out: *mut i32, max: c_int) -> c_int {
    with(|d| d.windows(std::slice::from_raw_parts_mut(out, 4 * max.max(0) as usize)) as c_int)
}

/// [`vnhost_display_windows`] of the windows above window `hwnd` (the one a Direct3D frame is
/// shown in: a movie's window of the same size over it is above it); None when `hwnd` is not a
/// visible window of the display.
pub fn windows_above(hwnd: u32, out: &mut [i32]) -> Option<usize> {
    with(|d| d.windows_above(hwnd, out))
}

/// What `vnhost_display_frame`'s buffer holds, in screen coordinates (l t r b): the screen and
/// the windows shown on it, whole (a game's window partly off the screen too).
#[no_mangle]
pub unsafe extern "C" fn vnhost_display_extent(out: *mut i32) {
    let e = with(|d| d.ext);
    std::slice::from_raw_parts_mut(out, 4).copy_from_slice(&[e.l, e.t, e.r, e.b]);
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_display_size(w: *mut c_int, h: *mut c_int) {
    with(|d| {
        *w = d.w;
        *h = d.h;
    })
}

/// A top-level window moved, resized, was shown/hidden or changed its z-order. `rect` is where
/// its surface is on the screen, then its client area; with `restack`, it goes just below
/// `above` (0: on top).
#[no_mangle]
pub unsafe extern "C" fn vnhost_window_pos(hwnd: u32, rect: *const i32, visible: c_int, restack: c_int, above: u32) {
    start_screenshots();
    start_scripted_input();
    let r = std::slice::from_raw_parts(rect, 8);
    let rect = Rect { l: r[0], t: r[1], r: r[2], b: r[3] };
    let client = Rect { l: r[4], t: r[5], r: r[6], b: r[7] };
    with(|d| d.pos(hwnd, rect, client, visible != 0, restack != 0, above))
}

impl Display {
    fn set_size(&mut self, w: i32, h: i32) {
        if w <= 0 || h <= 0 || w > 8192 || h > 8192 || (self.w, self.h) == (w, h) {
            return;
        }
        self.w = w;
        self.h = h;
        // (made anew even when it holds the same: a whole frame)
        self.ext = Rect::default();
        self.fit_extent();
    }

    /// The visible windows' rects, topmost first, as many as `out` holds; how many.
    fn windows(&self, out: &mut [i32]) -> usize {
        let mut n = 0;
        for id in self.order.iter().rev() {
            let w = &self.wins[id];
            if !w.visible || w.rect.empty() {
                continue;
            }
            if 4 * n == out.len() {
                break;
            }
            out[4 * n..4 * n + 4].copy_from_slice(&[w.rect.l, w.rect.t, w.rect.r, w.rect.b]);
            n += 1;
        }
        n
    }

    /// The visible windows above window `hwnd`, topmost first (as `windows`); None when `hwnd` is
    /// not a visible window.
    fn windows_above(&self, hwnd: u32, out: &mut [i32]) -> Option<usize> {
        if !self.wins.get(&hwnd).is_some_and(|w| w.visible && !w.rect.empty()) {
            return None;
        }
        let mut n = 0;
        for id in self.order.iter().rev() {
            if *id == hwnd {
                return Some(n);
            }
            let w = &self.wins[id];
            if !w.visible || w.rect.empty() || 4 * n == out.len() {
                continue;
            }
            out[4 * n..4 * n + 4].copy_from_slice(&[w.rect.l, w.rect.t, w.rect.r, w.rect.b]);
            n += 1;
        }
        Some(n)
    }

    fn pos(&mut self, hwnd: u32, rect: Rect, client: Rect, visible: bool, restack: bool, above: u32) {
        let d = self;
        let old = d.wins.get(&hwnd).map(|w| if w.visible { w.rect } else { Rect::default() }).unwrap_or_default();
        let win = d.wins.entry(hwnd).or_insert_with(|| Win { rect: Rect::default(), client, visible: false, pixels: Vec::new(), shared: 0 });
        if win.rect.w() != rect.w() || win.rect.h() != rect.h() {
            win.shared = 0;
            win.pixels = vec![0xff00_0000; (rect.w().max(0) * rect.h().max(0)) as usize];
        }
        win.rect = rect;
        win.client = client;
        win.visible = visible;
        if restack || !d.order.contains(&hwnd) {
            d.order.retain(|&h| h != hwnd);
            let at = d.order.iter().position(|&h| h == above).unwrap_or(d.order.len());
            d.order.insert(at, hwnd);
        }
        let new = if visible { rect } else { Rect::default() };
        if !d.fit_extent() {
            d.compose(old.or(&new));
        }
    }
}

/// New pixels for part of a window: `dirty` in window coordinates, `bits` (BGRA rows of
/// `stride` pixels) start at window coordinates (`ox`, `oy`).
/// Frames with big redraws end at most this often. KiriKiri and the like redraw in a busy loop,
/// which composes frames nobody sees at a few hundred per second; held to the display's rate, as
/// a vsync would, the guest spends that CPU elsewhere. Animations go by the clock, so they last
/// as long as before. The wait is at the end of a frame (`pace_frame`, from the message loop),
/// not at each draw: a game that blits its background and its message window separately would
/// get half the rate.
const FRAME: std::time::Duration = std::time::Duration::from_micros(16_667);
const BIG: i32 = 256 * 256;

/// Whether a draw of `r` is a big redraw (see `FRAME`).
fn big(r: &Rect) -> bool {
    r.w() * r.h() >= BIG
}

static DREW_BIG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The end of a frame (a message loop turn): after big redraws, waits for the display's rate.
pub fn pace_frame() {
    if DREW_BIG.swap(false, std::sync::atomic::Ordering::Relaxed) {
        pace();
    }
}

fn pace() {
    if crate::nopace() {
        return; // (benchmarks: as many frames as the guest can make, tests/bench)
    }
    #[cfg(target_os = "emscripten")]
    {
        extern "C" {
            fn emscripten_is_main_browser_thread() -> c_int;
        }
        if unsafe { emscripten_is_main_browser_thread() } != 0 {
            return; // (can't block)
        }
    }
    static NEXT: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let now = std::time::Instant::now();
    let wait = {
        let mut next = NEXT.lock().unwrap();
        let at = next.map_or(now, |n| n.max(now));
        *next = Some(at + FRAME);
        at - now
    };
    if !wait.is_zero() {
        std::thread::sleep(wait);
    }
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_window_draw(hwnd: u32, dirty: *const i32, bits: *const u32, stride: c_int, ox: c_int, oy: c_int) {
    let r = std::slice::from_raw_parts(dirty, 4);
    let dirty = Rect { l: r[0], t: r[1], r: r[2], b: r[3] };
    with(|d| d.draw(hwnd, dirty, bits, stride, ox, oy))
}

impl Display {
    /// # Safety
    /// `bits` must hold the rows of `dirty` (see `vnhost_window_draw`).
    unsafe fn draw(&mut self, hwnd: u32, dirty: Rect, bits: *const u32, stride: c_int, ox: c_int, oy: c_int) {
        if big(&dirty) {
            DREW_BIG.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        let d = self;
        let Some(win) = d.wins.get_mut(&hwnd) else { return };
        let dirty = dirty.and(&Rect { l: 0, t: 0, r: win.rect.w(), b: win.rect.h() });
        if dirty.empty() || win.px().is_empty() {
            return;
        }
        if win.shared != 0 && win.shared != bits as usize {
            win.pixels = win.px().to_vec();
            win.shared = 0;
        }
        let ww = win.rect.w() as usize;
        // Games redraw all of their window every frame, mostly the same pixels (a still screen,
        // a message waiting for a click): only the box that changed goes to the screen, the
        // page uploads and redraws only that, and nothing at all when nothing changed. The new
        // pixels are compared with `pixels` (what the screen has of the window) as they are
        // copied there; a whole new frame costs a row's compare more than the copy.
        // (the alpha byte is whatever the surface has: the page draws the frame opaque)
        let src = |y: i32| std::slice::from_raw_parts(bits.offset(((y - oy) * stride + (dirty.l - ox)) as isize), dirty.w() as usize);
        let changed = if nodiff() {
            for y in dirty.t..dirty.b {
                let dst = y as usize * ww + dirty.l as usize;
                win.pixels[dst..dst + dirty.w() as usize].copy_from_slice(src(y));
            }
            dirty
        } else {
            update_changed(&mut win.pixels, ww, dirty, src)
        };
        if changed.empty() {
            return;
        }
        let dirty = changed;
        let at = Rect { l: dirty.l + win.rect.l, t: dirty.t + win.rect.t, r: dirty.r + win.rect.l, b: dirty.b + win.rect.t };
        if win.visible {
            d.compose(at);
            gdi_drew([at.l, at.t, at.r, at.b]);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// GDI over Direct3D: on Windows a window's GDI drawing and its Direct3D / OpenGL presents land
// on the same surface, the later one showing. The page shows the last present over the window
// until the next one, so what GDI changed on the screen since then (a game that drew its logo
// with Direct3D and then its warning screen with DrawDibDraw: カスタムレイド4 体験版) is drawn
// from the screen on top of it. (Its own lock: taken alone, or after the display's.)

static GDI_SINCE_PRESENT: Mutex<Vec<[i32; 4]>> = Mutex::new(Vec::new());
/// at most this many rects; more: their bounding box
pub const GDI_RECTS: usize = 4;

fn gdi_drew(r: [i32; 4]) {
    add_rect(&mut GDI_SINCE_PRESENT.lock().unwrap_or_else(|e| e.into_inner()), r);
}

/// A present (d3d9/gpu.rs, from the program's thread): the frame is over everything again.
pub fn presented() {
    GDI_SINCE_PRESENT.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// What GDI changed on the screen since the last present (l t r b, screen coordinates).
pub fn gdi_since_present() -> Vec<[i32; 4]> {
    GDI_SINCE_PRESENT.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Adds `r` to the rects `v`, which cover the same pixels as the rects added (more only once
/// they are over GDI_RECTS: then their bounding box): a rect inside another is dropped, two
/// that make one rect (the same columns and touching rows, or the same rows and touching
/// columns: strips drawn one after another) become it.
pub fn add_rect(v: &mut Vec<[i32; 4]>, r: [i32; 4]) {
    let inside = |a: &[i32; 4], b: &[i32; 4]| a[0] >= b[0] && a[1] >= b[1] && a[2] <= b[2] && a[3] <= b[3];
    if r[0] >= r[2] || r[1] >= r[3] || v.iter().any(|e| inside(&r, e)) {
        return;
    }
    v.retain(|e| !inside(e, &r));
    let mut r = r;
    while let Some(i) = v.iter().position(|e| {
        (e[0] == r[0] && e[2] == r[2] && e[1] <= r[3] && r[1] <= e[3]) || (e[1] == r[1] && e[3] == r[3] && e[0] <= r[2] && r[0] <= e[2])
    }) {
        let e = v.swap_remove(i);
        r = [e[0].min(r[0]), e[1].min(r[1]), e[2].max(r[2]), e[3].max(r[3])];
        v.retain(|x| !inside(x, &r));
    }
    v.push(r);
    if v.len() > GDI_RECTS {
        let b = v.iter().fold(v[0], |a, e| [a[0].min(e[0]), a[1].min(e[1]), a[2].max(e[2]), a[3].max(e[3])]);
        v.clear();
        v.push(b);
    }
}

/// `IPPAI_NODIFF`: every draw goes to the screen as it is (no compare; for measuring).
fn nodiff() -> bool {
    crate::env_on!("IPPAI_NODIFF")
}

/// Where `a` and `b` first differ (None: the same).
fn first_diff(a: &[u32], b: &[u32]) -> Option<usize> {
    // (blocks of 16 or'ed together: the compiler keeps it to a load and xor per pixel)
    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 16 <= n {
        let (x, y) = (&a[i..i + 16], &b[i..i + 16]);
        if x.iter().zip(y).fold(0, |acc, (p, q)| acc | (p ^ q)) != 0 {
            break;
        }
        i += 16;
    }
    (i..n).find(|&k| a[k] != b[k])
}

/// Where `a` and `b` last differ, plus one (None: the same).
fn last_diff(a: &[u32], b: &[u32]) -> Option<usize> {
    let n = a.len().min(b.len());
    let mut i = n;
    while i >= 16 {
        let (x, y) = (&a[i - 16..i], &b[i - 16..i]);
        if x.iter().zip(y).fold(0, |acc, (p, q)| acc | (p ^ q)) != 0 {
            break;
        }
        i -= 16;
    }
    (0..i).rev().find(|&k| a[k] != b[k]).map(|k| k + 1)
}

/// Copies rows `src(y)` (y in `dirty.t..dirty.b`, columns `dirty.l..dirty.r`) into `px` (rows
/// of `ww`) and returns the box of what was different there (empty: nothing).
fn update_changed<'a>(px: &mut [u32], ww: usize, dirty: Rect, src: impl Fn(i32) -> &'a [u32]) -> Rect {
    let (l, w) = (dirty.l as usize, dirty.w() as usize);
    let row = |y: i32| y as usize * ww + l..y as usize * ww + l + w;
    // the first and the last row that changed
    let Some(t) = (dirty.t..dirty.b).find(|&y| first_diff(&px[row(y)], src(y)).is_some()) else { return Rect::default() };
    let b = (t..dirty.b).rev().find(|&y| first_diff(&px[row(y)], src(y)).is_some()).unwrap() + 1;
    // the columns, from both ends of each row: only what lies outside the box so far is
    // looked at
    let (mut x0, mut x1) = (w, 0);
    for y in t..b {
        let (p, s) = (&px[row(y)], src(y));
        if x0 > 0 {
            if let Some(k) = first_diff(&p[..x0], &s[..x0]) {
                x0 = k;
            }
        }
        if x1 < w {
            if let Some(k) = last_diff(&p[x1..], &s[x1..]) {
                x1 += k;
            }
        }
        if x0 == 0 && x1 == w {
            break;
        }
    }
    let x1 = x1.max(x0 + 1).min(w);
    for y in t..b {
        let r = row(y);
        px[r.start + x0..r.start + x1].copy_from_slice(&src(y)[x0..x1]);
    }
    Rect { l: dirty.l + x0 as i32, t, r: dirty.l + x1 as i32, b }
}

/// sake: like `vnhost_window_draw`, but `bits` is the window's whole surface (`w`*`h` pixels),
/// which the window then shows directly instead of copying it (one copy of each frame less).
/// The caller keeps `bits` alive and unmoved until `vnhost_window_unshare`.
#[no_mangle]
pub unsafe extern "C" fn vnhost_window_draw_shared(hwnd: u32, dirty: *const i32, bits: *const u32, w: c_int, h: c_int) {
    let r = std::slice::from_raw_parts(dirty, 4);
    with(|d| d.draw_shared(hwnd, Rect { l: r[0], t: r[1], r: r[2], b: r[3] }, bits, w, h))
}

/// sake: the window stops showing the surface `vnhost_window_draw_shared` gave it (about to be
/// freed or moved): it keeps a copy of it. When this returns, nothing reads the surface.
#[no_mangle]
pub extern "C" fn vnhost_window_unshare(hwnd: u32) {
    with(|d| d.unshare(hwnd))
}

#[no_mangle]
pub extern "C" fn vnhost_window_destroy(hwnd: u32) {
    with(|d| d.destroy(hwnd))
}

impl Display {
    /// # Safety
    /// See `vnhost_window_draw_shared`.
    unsafe fn draw_shared(&mut self, hwnd: u32, dirty: Rect, bits: *const u32, w: c_int, h: c_int) {
        if let Some(win) = self.wins.get_mut(&hwnd) {
            if win.rect.w() == w && win.rect.h() == h {
                win.shared = bits as usize;
            }
        }
        self.draw(hwnd, dirty, bits, w, 0, 0)
    }

    fn unshare(&mut self, hwnd: u32) {
        if let Some(win) = self.wins.get_mut(&hwnd) {
            if win.shared != 0 {
                let px = win.px().to_vec();
                win.pixels = px;
                win.shared = 0;
            }
        }
    }

    fn destroy(&mut self, hwnd: u32) {
        if let Some(win) = self.wins.remove(&hwnd) {
            self.order.retain(|&h| h != hwnd);
            if win.visible && !self.fit_extent() {
                self.compose(win.rect);
            }
        }
    }
}

/// The screen for the page: returns the BGRA buffer of `vnhost_display_extent` (its width *
/// height pixels; rows and columns below count from its corner). `io[0]` is the
/// serial of the frame the caller shows (anything else, such as -1, for none); stores the
/// current serial there, which changes whenever the screen does, and the box that changed since
/// (so a text cursor's blink doesn't upload the whole screen): rows `io[1]..io[2]`, columns
/// `io[10]..io[11]`. `io` has 12 words.
///
/// With `io[3]` = 1 the caller can take the changed rows from a window's own surface: when all
/// of them are one window's (a game redrawing its window; `screen` was not updated for them),
/// `io[3]` stays 1 and `io[4..10]` = the surface's address, its row length in pixels and the
/// window's rect on the screen (l t r b). Otherwise `io[3]` = 0 and `screen` has them.
#[no_mangle]
pub unsafe extern "C" fn vnhost_display_frame(io: *mut u32) -> *const u32 {
    let direct_ok = *io.add(3) == 1;
    let io = std::slice::from_raw_parts_mut(io, 12);
    with(|d| d.frame(io.try_into().unwrap(), direct_ok))
}

impl Display {
    fn frame(&mut self, io: &mut [u32; 12], direct_ok: bool) -> *const u32 {
        let d = self;
        let n = d.serial.wrapping_sub(io[0]) as usize;
        let mut one = None;
        let all = d.ext;
        let area = if n <= d.bands.len() {
            one = d.bands.iter().rev().take(n).next().map(|x| x.1).filter(|&id| id != 0);
            if d.bands.iter().rev().take(n).any(|x| Some(x.1) != one) {
                one = None;
            }
            d.bands.iter().rev().take(n).fold(Rect::default(), |a, x| a.or(&x.0)).and(&all)
        } else {
            all
        };
        // (rows and columns of `screen`: from the extent's corner)
        let (el, et) = (d.ext.l, d.ext.t);
        io[0] = d.serial;
        io[1] = (area.t - et).max(0) as u32;
        io[2] = (area.b.max(area.t) - et).max(0) as u32;
        io[10] = (area.l - el).max(0) as u32;
        io[11] = (area.r.max(area.l) - el).max(0) as u32;
        let win = one.filter(|_| direct_ok).and_then(|id| d.wins.get(&id)).filter(|w| w.shared != 0);
        match win {
            Some(w) => {
                io[3] = 1;
                io[4] = w.shared as u32;
                io[5] = w.rect.w() as u32;
                io[6..10].copy_from_slice(&[(w.rect.l - el) as u32, (w.rect.t - et) as u32, (w.rect.r - el) as u32, (w.rect.b - et) as u32]);
            }
            None => {
                // (rows a direct draw changed may be among them)
                d.sync();
                if direct_ok {
                    io[3] = 0;
                }
            }
        }
        d.screen.as_ptr()
    }
}

// ---------------------------------------------------------------------------------------------
// input: records { kind, x, y, data } queued for the program's input thread

/// Input record kinds, shared with the driver.
pub const INPUT_MOUSE_MOVE: u32 = 1;
pub const INPUT_MOUSE_DOWN: u32 = 2; // data: button 0 left, 1 right, 2 middle
pub const INPUT_MOUSE_UP: u32 = 3;
pub const INPUT_WHEEL: u32 = 4; // data: delta (120 per notch)
pub const INPUT_KEY_DOWN: u32 = 5; // data: vk | scan << 16
pub const INPUT_KEY_UP: u32 = 6;

/// Input records from the page, waiting for the program's input thread.
static INPUT: Mutex<VecDeque<[u32; 4]>> = Mutex::new(VecDeque::new());
static INPUT_READY: std::sync::Condvar = std::sync::Condvar::new();

/// The input records so far, waiting up to `ms` for one.
/// A state is being taken: the input thread comes out of its wait (and stands still).
pub(crate) fn snap_wake() {
    INPUT_READY.notify_all();
}

pub fn input_take(ms: u64) -> Vec<[u32; 4]> {
    let g = INPUT.lock().unwrap_or_else(|e| e.into_inner());
    let (mut g, _) = INPUT_READY
        .wait_timeout_while(g, std::time::Duration::from_millis(ms), |q| q.is_empty())
        .unwrap_or_else(|e| e.into_inner());
    g.drain(..).collect()
}

fn input_queued() -> usize {
    INPUT.lock().unwrap_or_else(|e| e.into_inner()).len()
}

#[no_mangle]
pub extern "C" fn vnhost_input_push(kind: u32, x: i32, y: i32, data: u32) -> c_int {
    // (nothing changes while a save state is taken: what comes meanwhile is dropped)
    if crate::snap::pausing() {
        return 0;
    }
    INPUT.lock().unwrap_or_else(|e| e.into_inner()).push_back([kind, x as u32, y as u32, data]);
    INPUT_READY.notify_one();
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cover(v: &[[i32; 4]], x: i32, y: i32) -> bool {
        v.iter().any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
    }

    #[test]
    fn strips_drawn_one_after_another_make_one_rect() {
        // カスタムレイド4: DrawDibDraw in 1024x8 strips down the window
        let mut v = Vec::new();
        for y in (0..768).step_by(8) {
            add_rect(&mut v, [0, y, 1024, y + 8]);
        }
        assert_eq!(v, [[0, 0, 1024, 768]]);
        add_rect(&mut v, [10, 10, 20, 20]);
        assert_eq!(v, [[0, 0, 1024, 768]]);
        add_rect(&mut v, [5, 5, 5, 9]);
        assert_eq!(v.len(), 1);
    }

    proptest::proptest! {
        /// Any rects in any order: at most GDI_RECTS, and the same pixels as the rects added
        /// while they fit; once not, everything added stays covered (the bounding box).
        #[test]
        fn add_rect_covers_what_was_added(rs in proptest::collection::vec((0i32..12, 0i32..12, 0i32..6, 0i32..6), 0..10)) {
            let mut v = Vec::new();
            let mut added: Vec<[i32; 4]> = Vec::new();
            let mut overflowed = false;
            for (l, t, w, h) in rs {
                let r = [l, t, l + w, t + h];
                add_rect(&mut v, r);
                if w > 0 && h > 0 { added.push(r); }
                proptest::prop_assert!(v.len() <= GDI_RECTS);
                proptest::prop_assert!(v.iter().all(|e| e[0] < e[2] && e[1] < e[3]));
                for y in 0..18 {
                    for x in 0..18 {
                        let want = cover(&added, x, y);
                        let got = cover(&v, x, y);
                        proptest::prop_assert!(!want || got, "({}, {}) lost: {:?} from {:?}", x, y, v, added);
                    }
                }
                let exact = (0..18).all(|y| (0..18).all(|x| cover(&added, x, y) == cover(&v, x, y)));
                if !exact && !overflowed {
                    // only by going over GDI_RECTS: one rect, the bounding box of all added
                    overflowed = true;
                    let bb = added.iter().fold(added[0], |a, e| [a[0].min(e[0]), a[1].min(e[1]), a[2].max(e[2]), a[3].max(e[3])]);
                    proptest::prop_assert_eq!(&v, &vec![bb]);
                }
            }
        }
    }

    fn run(old: &[u32], new: &[u32], w: usize, dirty: Rect) -> (Rect, Vec<u32>) {
        let mut px = old.to_vec();
        let r = update_changed(&mut px, w, dirty, |y| &new[y as usize * w + dirty.l as usize..y as usize * w + dirty.r as usize]);
        (r, px)
    }

    #[test]
    fn same_pixels_change_nothing() {
        let a: Vec<u32> = (0..64 * 48).collect();
        let (r, px) = run(&a, &a.clone(), 64, Rect { l: 0, t: 0, r: 64, b: 48 });
        assert!(r.empty());
        assert_eq!(px, a);
    }

    #[test]
    fn box_of_the_change() {
        let (w, h) = (100usize, 70usize);
        let a: Vec<u32> = (0..(w * h) as u32).map(|i| i * 7).collect();
        let mut b = a.clone();
        // two spots: (13, 20) and (81, 33)
        b[20 * w + 13] ^= 1;
        b[33 * w + 81] ^= 0xff00;
        let (r, px) = run(&a, &b, w, Rect { l: 0, t: 0, r: w as i32, b: h as i32 });
        assert_eq!(r, Rect { l: 13, t: 20, r: 82, b: 34 });
        assert_eq!(px, b);
    }

    #[test]
    fn only_inside_dirty() {
        let (w, h) = (40usize, 30usize);
        let a = vec![5u32; w * h];
        let mut b = a.clone();
        b[3 * w + 3] = 9; // outside dirty: not looked at
        b[10 * w + 20] = 9;
        let dirty = Rect { l: 10, t: 5, r: 35, b: 25 };
        let (r, px) = run(&a, &b, w, dirty);
        assert_eq!(r, Rect { l: 20, t: 10, r: 21, b: 11 });
        assert_eq!(px[3 * w + 3], 5);
        assert_eq!(px[10 * w + 20], 9);
    }

    #[test]
    fn every_single_pixel() {
        let (w, h) = (37usize, 9usize);
        let a: Vec<u32> = (0..(w * h) as u32).collect();
        for k in 0..w * h {
            let mut b = a.clone();
            b[k] = u32::MAX;
            let (r, px) = run(&a, &b, w, Rect { l: 0, t: 0, r: w as i32, b: h as i32 });
            let (x, y) = ((k % w) as i32, (k / w) as i32);
            assert_eq!(r, Rect { l: x, t: y, r: x + 1, b: y + 1 });
            assert_eq!(px, b);
        }
    }

    #[test]
    fn diff_ends() {
        let a = vec![1u32; 50];
        for k in 0..50 {
            let mut b = a.clone();
            b[k] = 2;
            assert_eq!(first_diff(&a, &b), Some(k));
            assert_eq!(last_diff(&a, &b), Some(k + 1));
        }
        assert_eq!(first_diff(&a, &a), None);
        assert_eq!(last_diff(&a, &a), None);
    }

    fn rect(l: i32, t: i32, r: i32, b: i32) -> Rect {
        Rect { l, t, r, b }
    }

    fn disp(w: i32, h: i32) -> Display {
        Display { w, h, wins: HashMap::new(), order: Vec::new(), screen: vec![0xff00_0000; (w * h) as usize], serial: 0, bands: Default::default(), stale: Rect::default(), ext: rect(0, 0, w, h) }
    }

    const BLACK: u32 = 0xff00_0000;

    impl Display {
        fn at(&self, x: i32, y: i32) -> u32 {
            self.screen[self.at_index(x, y)]
        }
        /// fills window `id` with `c` (a plain draw)
        fn fill(&mut self, id: u32, c: u32) {
            let r = self.wins[&id].rect;
            let px = vec![c; (r.w() * r.h()) as usize];
            unsafe { self.draw(id, rect(0, 0, r.w(), r.h()), px.as_ptr(), r.w(), 0, 0) };
        }
        /// frame(): (io after, the changed box as l t r b)
        fn take(&mut self, seen: u32, direct_ok: bool) -> ([u32; 12], Rect) {
            let mut io = [0u32; 12];
            io[0] = seen;
            io[3] = direct_ok as u32;
            let p = self.frame(&mut io, direct_ok);
            assert_eq!(p, self.screen.as_ptr());
            (io, rect(io[10] as i32, io[1] as i32, io[11] as i32, io[2] as i32))
        }
    }

    #[test]
    fn rect_ops() {
        let a = rect(1, 2, 5, 9);
        assert_eq!((a.w(), a.h()), (4, 7));
        assert!(!a.empty() && rect(3, 0, 3, 5).empty() && rect(0, 4, 5, 4).empty() && rect(4, 0, 3, 5).empty() && rect(0, 5, 5, 4).empty());
        assert!(!rect(0, 0, 1, 1).empty());
        assert_eq!(a.and(&rect(3, 0, 9, 4)), rect(3, 2, 5, 4));
        assert_eq!(a.or(&rect(3, 0, 9, 4)), rect(1, 0, 9, 9));
        assert_eq!(a.or(&rect(7, 7, 7, 9)), a);
        assert_eq!(rect(7, 7, 7, 9).or(&a), a);
    }

    /// タイムリープ (#192): the D3D9 window was shown top-level, then made the child of a
    /// bigger dialog shown after it. sake hides it from the screen then (SetParent: a zero rect,
    /// not visible); it is no window of the screen's, and the windows over its frame are the ones
    /// above the dialog that holds it, none, not the dialog itself.
    #[test]
    fn a_window_made_a_child_leaves_the_screen() {
        let mut d = disp(64, 48);
        d.pos(1, rect(0, 0, 64, 48), rect(0, 0, 64, 48), true, false, 0);
        d.pos(2, rect(0, -2, 64, 50), rect(0, 0, 64, 48), true, false, 0);
        let mut out = [0; 32];
        assert_eq!(d.windows_above(1, &mut out), Some(1), "under the dialog while top-level");
        d.pos(1, rect(0, 0, 0, 0), rect(0, 0, 0, 0), false, false, 0);
        assert_eq!(d.windows_above(1, &mut out), None, "a child: by the topmost window over it");
        d.fill(2, 0xff12_3456);
        assert_eq!(d.at(10, 10), 0xff12_3456, "the dialog's pixels are the screen's");
    }

    /// The windows above a Direct3D window, by the window: a movie's window exactly over the
    /// game's client area is above it (it was taken for the game's own before, and hidden).
    #[test]
    fn windows_above_a_window() {
        let mut d = disp(40, 30);
        d.pos(1, rect(0, 0, 40, 30), rect(0, 0, 40, 30), true, false, 0);
        d.pos(2, rect(0, 0, 40, 30), rect(0, 0, 40, 30), true, false, 0);
        d.pos(3, rect(5, 5, 10, 10), rect(5, 5, 10, 10), true, false, 0);
        let mut out = [0; 32];
        assert_eq!(d.windows_above(1, &mut out), Some(2));
        assert_eq!(out[..8], [5, 5, 10, 10, 0, 0, 40, 30]);
        assert_eq!(d.windows_above(3, &mut out), Some(0));
        assert_eq!(d.windows_above(9, &mut out), None, "not a window of the display");
        d.pos(2, rect(0, 0, 40, 30), rect(0, 0, 40, 30), false, false, 0);
        assert_eq!(d.windows_above(1, &mut out), Some(1), "hidden ones are not above");
        assert_eq!(d.windows_above(2, &mut out), None, "a hidden window shows nothing");
        // (as many as `out` holds)
        assert_eq!(d.windows_above(1, &mut out[..0]), Some(0));
    }

    proptest::proptest! {
        /// Any moves, hides and restacks of a few windows: windows_above is, topmost first, the
        /// visible windows over the given one in a model of the z-order (a window restacked
        /// goes under the one named, or on top).
        #[test]
        fn windows_above_as_a_model(ops in proptest::collection::vec((1u32..5, 0i32..30, 0i32..20, 0i32..20, proptest::prelude::any::<bool>(), proptest::prelude::any::<bool>(), 0u32..6), 1..30)) {
            let mut d = disp(40, 30);
            // (bottom first: hwnd, rect, visible)
            let mut model: Vec<(u32, [i32; 4], bool)> = Vec::new();
            for (h, x, y, w, vis, restack, above) in ops {
                let r = [x, y, x + w, y + w / 2];
                d.pos(h, rect(r[0], r[1], r[2], r[3]), rect(r[0], r[1], r[2], r[3]), vis, restack, above);
                let known = model.iter().any(|m| m.0 == h);
                if restack || !known {
                    model.retain(|m| m.0 != h);
                    let at = model.iter().position(|m| m.0 == above).unwrap_or(model.len());
                    model.insert(at, (h, r, vis));
                } else {
                    model.iter_mut().find(|m| m.0 == h).unwrap().1 = r;
                    model.iter_mut().find(|m| m.0 == h).unwrap().2 = vis;
                }
                let shown = |m: &(u32, [i32; 4], bool)| m.2 && m.1[2] > m.1[0] && m.1[3] > m.1[1];
                for q in 1..6 {
                    let mut out = [0; 32];
                    let got = d.windows_above(q, &mut out).map(|n| out[..4 * n].to_vec());
                    let want = model.iter().position(|m| m.0 == q && shown(m)).map(|i| model[i + 1..].iter().rev().filter(|m| shown(m)).flat_map(|m| m.1).collect::<Vec<_>>());
                    proptest::prop_assert_eq!(got, want, "window {}", q);
                }
            }
        }
    }

    #[test]
    fn windows_compose_in_z_order() {
        let mut d = disp(40, 30);
        d.pos(1, rect(10, 10, 30, 20), rect(12, 14, 28, 18), true, false, 0);
        assert_eq!(d.at(10, 10), BLACK, "a new window is black");
        d.fill(1, 0xff11_1111);
        assert_eq!((d.at(10, 10), d.at(29, 19), d.at(9, 10), d.at(30, 19), d.at(10, 20)), (0xff11_1111, 0xff11_1111, BLACK, BLACK, BLACK));
        // a second one on top, overlapping
        d.pos(2, rect(25, 15, 35, 25), rect(25, 15, 35, 25), true, false, 0);
        d.fill(2, 0xff22_2222);
        assert_eq!((d.at(26, 16), d.at(24, 16), d.at(34, 24)), (0xff22_2222, 0xff11_1111, 0xff22_2222));
        // window 1 restacked on top (above 0: the top)
        d.pos(1, rect(10, 10, 30, 20), rect(12, 14, 28, 18), true, true, 0);
        assert_eq!(d.order, [2, 1]);
        assert_eq!(d.at(26, 16), 0xff11_1111);
        // and below 2 again
        d.pos(1, rect(10, 10, 30, 20), rect(12, 14, 28, 18), true, true, 2);
        assert_eq!(d.order, [1, 2]);
        assert_eq!(d.at(26, 16), 0xff22_2222);
        // hidden: what was under it shows
        d.pos(2, rect(25, 15, 35, 25), rect(25, 15, 35, 25), false, false, 0);
        assert_eq!((d.at(26, 16), d.at(34, 24), d.at(20, 16)), (0xff11_1111, BLACK, 0xff11_1111));
        // a draw into a hidden window changes nothing on the screen
        d.fill(2, 0xff33_3333);
        assert_eq!(d.at(34, 24), BLACK);
        d.pos(2, rect(25, 15, 35, 25), rect(25, 15, 35, 25), true, false, 0);
        assert_eq!(d.at(34, 24), 0xff33_3333, "it kept its pixels");
        // moved (same size: pixels kept), then resized (black again)
        d.pos(1, rect(0, 0, 20, 10), rect(0, 0, 20, 10), true, false, 0);
        assert_eq!((d.at(0, 0), d.at(10, 10), d.at(20, 12)), (0xff11_1111, BLACK, BLACK));
        d.pos(1, rect(0, 0, 21, 10), rect(0, 0, 21, 10), true, false, 0);
        assert_eq!(d.at(0, 0), BLACK);
        d.destroy(2);
        assert_eq!(d.at(34, 24), BLACK);
        assert_eq!(d.order, [1]);
        d.destroy(7);
    }

    #[test]
    fn partial_draws() {
        let mut d = disp(20, 20);
        d.pos(1, rect(2, 3, 12, 13), rect(2, 3, 12, 13), true, false, 0);
        // a 3x2 block at (4,5) of the window from a source of stride 8 starting at (4,5)
        let src: Vec<u32> = (0..16).map(|i| 0xff00_0000 | i).collect();
        unsafe { d.draw(1, rect(4, 5, 7, 7), src.as_ptr(), 8, 4, 5) };
        assert_eq!((d.at(6, 8), d.at(8, 8), d.at(6, 9), d.at(8, 9)), (0xff00_0000, 0xff00_0002, 0xff00_0008, 0xff00_000a));
        assert_eq!((d.at(9, 8), d.at(6, 10), d.at(5, 8)), (BLACK, BLACK, BLACK));
        // drawing outside the window is clipped to it
        let big = vec![0xff44_4444u32; 400];
        unsafe { d.draw(1, rect(-5, -5, 50, 50), big.as_ptr().add(5 * 20 + 5), 20, 0, 0) };
        assert_eq!((d.at(2, 3), d.at(11, 12), d.at(12, 12), d.at(1, 3)), (0xff44_4444, 0xff44_4444, BLACK, BLACK));
        // the same pixels again: no new frame
        let s = d.serial;
        unsafe { d.draw(1, rect(0, 0, 10, 10), big.as_ptr(), 20, 0, 0) };
        assert_eq!(d.serial, s);
        unsafe { d.draw(9, rect(0, 0, 10, 10), big.as_ptr(), 20, 0, 0) };
        assert_eq!(d.serial, s);
    }

    #[test]
    fn view_is_the_game_window() {
        let mut d = disp(100, 80);
        assert_eq!(d.view(), rect(0, 0, 100, 80), "no windows: the screen");
        d.pos(1, rect(5, 5, 60, 50), rect(8, 10, 58, 48), true, false, 0);
        d.pos(2, rect(70, 70, 75, 75), rect(70, 70, 75, 75), true, true, 1);
        assert_eq!(d.order, [2, 1]);
        assert_eq!(d.view(), rect(8, 10, 58, 48), "the biggest client area, without the frame; one below does not widen it");
        // a dialog on top that sticks out: still only the game's window
        d.pos(3, rect(50, 40, 90, 70), rect(52, 42, 88, 68), true, false, 0);
        assert_eq!(d.view(), rect(8, 10, 58, 48));
        // hidden ones do not count
        d.pos(3, rect(50, 40, 90, 70), rect(52, 42, 88, 68), false, false, 0);
        assert_eq!(d.view(), rect(8, 10, 58, 48));
        // partly off the screen: all of it
        d.pos(1, rect(-10, -10, 60, 50), rect(-5, -5, 58, 48), true, false, 0);
        assert_eq!(d.view(), rect(-5, -5, 58, 48));
        // client areas off the screen: the screen
        d.pos(1, rect(200, 200, 260, 250), rect(200, 200, 260, 250), true, false, 0);
        d.pos(2, rect(70, 70, 75, 75), rect(170, 170, 175, 175), true, false, 0);
        assert_eq!(d.view(), rect(0, 0, 100, 80));
        // a window above that is off the screen does not widen it
        let mut d = disp(100, 80);
        d.pos(1, rect(5, 5, 60, 50), rect(8, 10, 58, 48), true, false, 0);
        d.pos(2, rect(120, 0, 130, 10), rect(120, 0, 130, 10), true, false, 0);
        assert_eq!(d.view(), rect(8, 10, 58, 48));
        // a smaller window above: the main one is still the biggest
        d.pos(4, rect(10, 12, 20, 22), rect(10, 12, 20, 22), true, false, 0);
        assert_eq!(d.view(), rect(8, 10, 58, 48));
        // the biggest by area: 10x10 over 2x30
        let mut e = disp(100, 80);
        e.pos(1, rect(0, 0, 10, 10), rect(0, 0, 10, 10), true, false, 0);
        e.pos(2, rect(50, 0, 52, 30), rect(50, 0, 52, 30), true, true, 1);
        assert_eq!(e.view(), rect(0, 0, 10, 10));
        // zero-sized windows are not shown
        d.pos(5, rect(0, 0, 0, 0), rect(0, 0, 0, 0), true, false, 0);
        assert_eq!(d.view(), rect(8, 10, 58, 48));
    }

    /// AMBITIOUS MISSION after episode 1 on a phone: its 976x599 client at (850, 154), where a
    /// bigger screen had it, on the 1366x768 screen. The page showed the 516x599 on the screen;
    /// now all of it, its pixels off the screen too.
    #[test]
    fn a_window_partly_off_the_screen_is_shown_whole() {
        let mut d = disp(1366, 768);
        let (win, client) = (rect(842, 123, 1834, 761), rect(850, 154, 1826, 753));
        d.pos(1, win, client, true, false, 0);
        assert_eq!(d.ext, rect(0, 0, 1834, 768), "the screen and the window");
        d.fill(1, 0xff12_3456);
        assert_eq!(d.view(), client);
        assert_eq!((d.at(1825, 752), d.at(850, 154), d.at(849, 100)), (0xff12_3456, 0xff12_3456, BLACK));
        let (io, r) = d.take(u32::MAX, false);
        assert_eq!(r, rect(0, 0, 1834, 768), "rows and columns of the buffer");
        assert_eq!(io[3], 0);
        // moved back onto the screen: the buffer is the screen again
        d.pos(1, rect(0, 0, 992, 638), rect(8, 31, 984, 630), true, false, 0);
        assert_eq!((d.ext, d.screen.len()), (rect(0, 0, 1366, 768), 1366 * 768));
        assert_eq!((d.view(), d.at(8, 31)), (rect(8, 31, 984, 630), 0xff12_3456));
        // up and left of the screen: the buffer starts there
        d.pos(1, rect(-100, -50, 892, 588), rect(-92, -19, 884, 580), true, false, 0);
        assert_eq!(d.ext, rect(-100, -50, 1366, 768));
        assert_eq!((d.view(), d.at(-92, -19), d.at(883, 579)), (rect(-92, -19, 884, 580), 0xff12_3456, 0xff12_3456));
        let (_, r) = d.take(u32::MAX, false);
        assert_eq!(r, rect(0, 0, 1466, 818));
        // a window with nothing on the screen is not shown (hidden there): the screen
        d.pos(1, rect(2000, 0, 2100, 100), rect(2000, 0, 2100, 100), true, false, 0);
        assert_eq!((d.ext, d.view()), (rect(0, 0, 1366, 768), rect(0, 0, 1366, 768)));
        // windows far off are held to MAX_EXTENT around the screen
        d.pos(1, rect(-9000, -9000, 9000, 9000), rect(-9000, -9000, 9000, 9000), true, false, 0);
        assert!(d.ext.w() <= MAX_EXTENT && d.ext.h() <= MAX_EXTENT && d.ext.and(&d.screen_rect()) == d.screen_rect(), "{:?}", d.ext);
    }

    /// A game drawing its own surface (sake's shared surfaces) partly off the screen: the page
    /// is told where it is in the buffer, from the buffer's corner.
    #[test]
    fn direct_frames_off_the_screen() {
        let mut d = disp(40, 30);
        d.pos(1, rect(-4, 20, 6, 36), rect(-4, 20, 6, 36), true, false, 0);
        assert_eq!(d.ext, rect(-4, 0, 40, 36));
        let s0 = d.serial;
        let surf: Vec<u32> = (0..160).map(|i| 0xff00_0000 | i).collect();
        unsafe { d.draw_shared(1, rect(0, 0, 10, 16), surf.as_ptr(), 10, 16) };
        let (io, r) = d.take(s0, true);
        assert_eq!(r, rect(0, 20, 10, 36));
        assert_eq!(io[3..10], [1, surf.as_ptr() as usize as u32, 10, 0, 20, 10, 36]);
        // composed instead: the pixel under the window's corner
        let (_, _) = d.take(s0, false);
        assert_eq!((d.at(-4, 20), d.at(5, 35)), (0xff00_0000, 0xff00_0000 | 159));
    }

    proptest::proptest! {
        /// Any windows moved anywhere (on, partly off and wholly off the screen), shown, hidden,
        /// restacked and drawn: the buffer holds the screen and every shown window with a part on
        /// the screen, whole, each pixel the topmost window's there (black where none), and the
        /// view is inside it.
        #[test]
        fn the_buffer_holds_the_windows_on_the_screen(ops in proptest::collection::vec((1u32..5, -50i32..70, -40i32..60, 1i32..40, 1i32..30, proptest::prelude::any::<bool>(), proptest::prelude::any::<bool>(), 0u32..6), 1..25)) {
            let mut d = disp(40, 30);
            // (bottom first: hwnd, rect, visible, colour)
            let mut model: Vec<(u32, Rect, bool, u32)> = Vec::new();
            for (k, (h, x, y, w, ht, vis, restack, above)) in ops.into_iter().enumerate() {
                let r = rect(x, y, x + w, y + ht);
                d.pos(h, r, r, vis, restack, above);
                let colour = 0xff00_0000 | (k as u32 + 1) << 8 | h;
                d.fill(h, colour);
                let known = model.iter().any(|m| m.0 == h);
                if restack || !known {
                    model.retain(|m| m.0 != h);
                    let at = model.iter().position(|m| m.0 == above).unwrap_or(model.len());
                    model.insert(at, (h, r, vis, colour));
                } else {
                    let m = model.iter_mut().find(|m| m.0 == h).unwrap();
                    (m.1, m.2, m.3) = (r, vis, colour);
                }
                d.sync();
                let screen = rect(0, 0, 40, 30);
                let mut want = screen;
                for m in model.iter().filter(|m| m.2 && !m.1.and(&screen).empty()) {
                    want = want.or(&m.1);
                }
                proptest::prop_assert_eq!(d.ext, want);
                proptest::prop_assert_eq!(d.screen.len(), (want.w() * want.h()) as usize);
                for y in want.t..want.b {
                    for x in want.l..want.r {
                        let top = model.iter().rev().find(|m| m.2 && x >= m.1.l && x < m.1.r && y >= m.1.t && y < m.1.b);
                        // (one wholly off the screen is not composed)
                        let top = top.filter(|m| !m.1.and(&screen).empty());
                        let expect = top.map_or(BLACK, |m| m.3);
                        let hidden_under = model.iter().rev().find(|m| m.2 && x >= m.1.l && x < m.1.r && y >= m.1.t && y < m.1.b).is_some_and(|m| m.1.and(&screen).empty());
                        if !hidden_under {
                            proptest::prop_assert_eq!(d.at(x, y), expect, "pixel {},{}", x, y);
                        }
                    }
                }
                let v = d.view();
                proptest::prop_assert!(v.and(&d.ext) == v, "view {:?} in {:?}", v, d.ext);
            }
        }
    }

    #[test]
    fn window_list_topmost_first() {
        let mut d = disp(100, 80);
        d.pos(1, rect(0, 0, 10, 10), rect(0, 0, 10, 10), true, false, 0);
        d.pos(2, rect(1, 1, 5, 5), rect(1, 1, 5, 5), false, false, 0);
        d.pos(3, rect(2, 2, 2, 9), rect(2, 2, 2, 9), true, false, 0);
        d.pos(4, rect(3, 4, 5, 6), rect(3, 4, 5, 6), true, false, 0);
        let mut out = [0i32; 12];
        assert_eq!(d.windows(&mut out), 2);
        assert_eq!(out[..8], [3, 4, 5, 6, 0, 0, 10, 10]);
        let mut one = [0i32; 4];
        assert_eq!(d.windows(&mut one), 1);
        assert_eq!(one, [3, 4, 5, 6]);
        assert_eq!(d.windows(&mut []), 0);
    }

    #[test]
    fn frames_say_what_changed() {
        let mut d = disp(40, 30);
        let (io, r) = d.take(u32::MAX, false);
        assert_eq!(r, rect(0, 0, 40, 30), "a caller with no frame gets all of it");
        let s0 = io[0];
        assert_eq!(s0, d.serial);
        d.pos(1, rect(10, 10, 30, 20), rect(10, 10, 30, 20), true, false, 0);
        let (io, r) = d.take(s0, false);
        assert_eq!(r, rect(10, 10, 30, 20));
        let s1 = io[0];
        assert_ne!(s1, s0);
        // nothing since: an empty box
        let (io, r) = d.take(s1, false);
        assert!(r.empty() && io[0] == s1, "{r:?}");
        // two draws: their union
        unsafe {
            d.draw(1, rect(0, 0, 2, 2), [0xff00_0001u32; 4].as_ptr(), 2, 0, 0);
            d.draw(1, rect(5, 6, 7, 8), [0xff00_0002u32; 4].as_ptr(), 2, 5, 6);
        }
        let (_, r) = d.take(s1, false);
        assert_eq!(r, rect(10, 10, 17, 18));
        let (_, r) = d.take(s1.wrapping_add(1), false);
        assert_eq!(r, rect(15, 16, 17, 18), "only the newest");
        // a caller further behind than the bands kept gets the whole screen
        for i in 0..70u32 {
            unsafe { d.draw(1, rect(0, 0, 1, 1), [0xff10_0000 | i].as_ptr(), 1, 0, 0) };
        }
        let (_, r) = d.take(s1, false);
        assert_eq!(r, rect(0, 0, 40, 30));
        assert_eq!(d.bands.len(), 64);
        let (io, r) = d.take(d.serial.wrapping_sub(64), false);
        assert_eq!((r, io[3]), (rect(10, 10, 11, 11), 0));
    }

    #[test]
    fn shared_surfaces_go_direct() {
        let mut d = disp(40, 30);
        d.pos(1, rect(10, 10, 14, 13), rect(10, 10, 14, 13), true, false, 0);
        let s0 = d.serial;
        let surf: Vec<u32> = (0..12).map(|i| 0xff00_0100 | i).collect();
        unsafe { d.draw_shared(1, rect(0, 0, 4, 3), surf.as_ptr(), 4, 3) };
        assert_eq!(d.wins[&1].shared, surf.as_ptr() as usize);
        assert_eq!(d.wins[&1].px(), &surf[..]);
        // the screen was not written: the page takes the window's surface
        assert_eq!(d.at(10, 10), BLACK);
        assert_eq!(d.stale, rect(10, 10, 14, 13));
        let (io, r) = d.take(s0, true);
        assert_eq!(r, rect(10, 10, 14, 13));
        assert_eq!(io[3..10], [1, surf.as_ptr() as usize as u32, 4, 10, 10, 14, 13]);
        assert_eq!(d.at(10, 10), BLACK, "still behind");
        // windows on top that are hidden, or elsewhere, leave it direct
        d.pos(8, rect(30, 0, 40, 5), rect(30, 0, 40, 5), true, false, 0);
        d.pos(9, rect(9, 9, 15, 15), rect(9, 9, 15, 15), false, false, 0);
        let s9 = d.serial;
        unsafe { d.draw_shared(1, rect(0, 0, 4, 3), surf.as_ptr(), 4, 3) };
        let mut moved = surf.clone();
        moved[0] = 0xff00_0f00;
        unsafe { d.draw_shared(1, rect(0, 0, 4, 3), moved.as_ptr(), 4, 3) };
        assert_eq!((d.take(s9, true).0[3], d.at(10, 10)), (1, 0xff00_0100), "the screen keeps the older pixels");
        d.destroy(8);
        d.destroy(9);
        unsafe { d.draw_shared(1, rect(0, 0, 4, 3), surf.as_ptr(), 4, 3) };
        let s0 = d.serial.wrapping_sub(1);
        // a caller that cannot take it: the screen is brought up to date
        let (io, _) = d.take(s0, false);
        assert_eq!(io[3], 0);
        assert_eq!((d.at(10, 10), d.at(13, 12)), (0xff00_0100, 0xff00_010b));
        assert!(d.stale.empty());
        // a window on top of part of it: no longer direct
        d.pos(2, rect(12, 11, 20, 20), rect(12, 11, 20, 20), true, false, 0);
        let s1 = d.serial;
        let surf2: Vec<u32> = (0..12).map(|i| 0xff00_0200 | i).collect();
        unsafe { d.draw_shared(1, rect(0, 0, 4, 3), surf2.as_ptr(), 4, 3) };
        assert_eq!(d.at(10, 10), 0xff00_0200);
        let (io, _) = d.take(s1, true);
        assert_eq!(io[3], 0);
        // a mixed run of bands (direct then composed) is not direct either
        d.destroy(2);
        let s2 = d.serial;
        let surf3: Vec<u32> = (0..12).map(|i| 0xff00_0300 | i).collect();
        unsafe { d.draw_shared(1, rect(0, 0, 4, 3), surf3.as_ptr(), 4, 3) };
        let (io, _) = d.take(s2.wrapping_sub(1), true);
        assert_eq!(io[3], 0);
        assert_eq!(d.at(11, 10), 0xff00_0301);
        // a different surface: the old one is copied first
        let surf4: Vec<u32> = (0..12).map(|i| 0xff00_0400 | i).collect();
        unsafe { d.draw(1, rect(0, 0, 1, 1), surf4.as_ptr(), 4, 0, 0) };
        assert_eq!(d.wins[&1].shared, 0);
        assert_eq!(d.wins[&1].px()[..3], [0xff00_0400, 0xff00_0301, 0xff00_0302]);
        // unshare keeps a copy
        unsafe { d.draw_shared(1, rect(0, 0, 4, 3), surf4.as_ptr(), 4, 3) };
        d.unshare(1);
        assert_eq!(d.wins[&1].shared, 0);
        assert_eq!(d.wins[&1].px(), &surf4[..]);
        d.unshare(1);
        d.unshare(5);
        // a surface of another size is not taken
        unsafe { d.draw_shared(1, rect(0, 0, 2, 2), surf4.as_ptr(), 2, 6) };
        assert_eq!(d.wins[&1].shared, 0);
    }

    #[test]
    fn display_size() {
        let mut d = disp(40, 30);
        d.pos(1, rect(30, 20, 50, 40), rect(30, 20, 50, 40), true, false, 0);
        d.fill(1, 0xff55_5555);
        let s = d.serial;
        for (w, h) in [(40, 30), (0, 30), (40, 0), (-1, 30), (40, -1), (8193, 30), (40, 8193)] {
            d.set_size(w, h);
            assert_eq!((d.serial, d.w, d.h), (s, 40, 30), "{w}x{h}");
        }
        d.set_size(8192, 1);
        assert_eq!((d.w, d.h), (8192, 1));
        d.set_size(1, 8192);
        assert_eq!((d.w, d.h), (1, 8192));
        d.set_size(60, 50);
        assert_eq!((d.screen.len(), d.at(45, 35), d.at(0, 0)), (3000, 0xff55_5555, BLACK));
        assert_eq!(d.bands.len(), 1);
        let (_, r) = d.take(s, false);
        assert_eq!(r, rect(0, 0, 60, 50));
    }

    #[test]
    fn input_queue() {
        assert_eq!(vnhost_input_push(INPUT_KEY_DOWN, 1, 2, 3), 0);
        vnhost_input_push(INPUT_MOUSE_MOVE, -1, 4, 0);
        assert_eq!(input_queued(), 2);
        assert_eq!(input_take(0), [[INPUT_KEY_DOWN, 1, 2, 3], [INPUT_MOUSE_MOVE, u32::MAX, 4, 0]]);
        assert!(input_take(1).is_empty());
    }

    #[test]
    fn big_redraws() {
        assert!(big(&rect(0, 0, 256, 256)) && big(&rect(10, 10, 522, 138)));
        assert!(!big(&rect(0, 0, 255, 256)) && !big(&rect(0, 0, 256, 255)) && !big(&rect(0, 0, 30, 30)));
    }

    #[test]
    fn diffs_of_zero_pixels() {
        let a = vec![0u32; 40];
        for k in [0, 15, 16, 17, 39] {
            let mut b = a.clone();
            b[k] = 5;
            assert_eq!((first_diff(&a, &b), last_diff(&a, &b)), (Some(k), Some(k + 1)), "{k}");
        }
    }

    #[test]
    fn change_boxes_across_rows() {
        let (w, h) = (20usize, 4usize);
        let a = vec![0u32; w * h];
        // the first row reaches the right edge, a later one the left: both count
        let mut b = a.clone();
        b[w + 5..2 * w].fill(1);
        b[2 * w + 1] = 1;
        assert_eq!(run(&a, &b, w, Rect { l: 0, t: 0, r: w as i32, b: h as i32 }).0, Rect { l: 1, t: 1, r: w as i32, b: 3 });
        // the first row reaches the left edge, a later one the right
        let mut b = a.clone();
        b[w] = 1;
        b[3 * w - 1] = 1;
        assert_eq!(run(&a, &b, w, Rect { l: 0, t: 0, r: w as i32, b: h as i32 }).0, Rect { l: 0, t: 1, r: w as i32, b: 3 });
    }

    /// The C API on the one global display (the only test that uses it).
    #[test]
    fn host_calls() {
        unsafe {
            vnhost_display_set_size(64, 48);
            vnhost_display_set_size(0, 48);
            let (mut w, mut h) = (0, 0);
            vnhost_display_size(&mut w, &mut h);
            assert_eq!((w, h), (64, 48));
            vnhost_window_pos(7, [4, 4, 12, 10, 5, 6, 11, 9].as_ptr(), 1, 0, 0);
            vnhost_window_pos(8, [40, 30, 44, 34, 40, 30, 44, 34].as_ptr(), 1, 0, 0);
            let mut v = [0i32; 4];
            vnhost_display_view(v.as_mut_ptr());
            assert_eq!(v, [5, 6, 11, 9], "the client area only, not the window above");
            let mut list = [0i32; 8];
            assert_eq!(vnhost_display_windows(list.as_mut_ptr(), 2), 2);
            assert_eq!(list, [40, 30, 44, 34, 4, 4, 12, 10]);
            let px = vec![0xff12_3456u32; 48];
            vnhost_window_draw(7, [0, 0, 8, 6].as_ptr(), px.as_ptr(), 8, 0, 0);
            let mut io = [u32::MAX; 12];
            io[3] = 0;
            let screen = std::slice::from_raw_parts(vnhost_display_frame(io.as_mut_ptr()), 64 * 48);
            assert_eq!((screen[4 * 64 + 4], screen[3 * 64 + 4]), (0xff12_3456, BLACK));
            let surf = vec![0xff65_4321u32; 48];
            vnhost_window_draw_shared(7, [0, 0, 8, 6].as_ptr(), surf.as_ptr(), 8, 6);
            vnhost_window_unshare(7);
            with(|d| assert_eq!((d.wins[&7].shared, d.wins[&7].px()[0]), (0, 0xff65_4321)));
            vnhost_window_destroy(8);
            assert_eq!(vnhost_display_windows(list.as_mut_ptr(), 2), 1);
            vnhost_window_destroy(7);
            with(|d| assert!(d.wins.is_empty() && d.order.is_empty()));
        }
    }

}

/// Kani proofs (formal/kani.sh): the rectangle algebra windows are composed with.
#[cfg(kani)]
mod proofs {
    use super::Rect;

    fn any_rect() -> Rect {
        let v = || kani::any_where(|&v: &i32| (-100_000..=100_000).contains(&v));
        Rect { l: v(), t: v(), r: v(), b: v() }
    }
    fn inside(p: (i32, i32), r: &Rect) -> bool {
        !r.empty() && r.l <= p.0 && p.0 < r.r && r.t <= p.1 && p.1 < r.b
    }

    /// A point is in a ∩ b exactly when it is in both; a ∪ b holds both.
    #[kani::proof]
    fn and_or() {
        let (a, b) = (any_rect(), any_rect());
        let p = (kani::any(), kani::any());
        assert_eq!(inside(p, &a.and(&b)), inside(p, &a) && inside(p, &b));
        let u = a.or(&b);
        assert!(!inside(p, &a) || inside(p, &u));
        assert!(!inside(p, &b) || inside(p, &u));
        assert_eq!(a.and(&b).empty(), b.and(&a).empty());
    }

}
