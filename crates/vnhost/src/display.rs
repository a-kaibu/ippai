//! The screen behind Wine's browser display driver (dlls/winebrowser.drv in the Wine patch).
//!
//! Every Wine process shares this state: top-level windows hand their pixels here when Wine
//! flushes their surface, and the screen is composed from them in z-order (bottom to top).
//! The page shows `vnhost_display_frame` (one BGRA buffer of the whole screen) and sends input
//! with `vnhost_input_push`; the driver's desktop thread reads it from `vnhost_input_fd`.
//!
//! Under node, `VNVM_SCREENSHOT=<file.ppm>` writes the screen out (see `start_screenshots`) and
//! `VNVM_INPUT="<seconds>:<kind>,<x>,<y>,<data>;..."` sends input that many seconds after the
//! first window shows up (see `start_scripted_input`).

use std::collections::HashMap;
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
    /// the rows each of the last few serials redrew, newest last, and the window whose own
    /// pixels they are when `screen` was not updated (see `direct`)
    bands: std::collections::VecDeque<(i32, i32, u32)>,
    /// where `screen` is behind its windows: the areas drawn `direct`ly since it was last made
    /// whole (see `sync`)
    stale: Rect,
}

static DISPLAY: Mutex<Option<Display>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut Display) -> R) -> R {
    let mut g = DISPLAY.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| {
        let (w, h) = std::env::var("VNVM_SCREEN")
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
        }
    }))
}

/// `VNVM_SCREENSHOT`: a thread writes the screen out whenever it has changed, once a second.
fn start_screenshots() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    let Ok(path) = std::env::var("VNVM_SCREENSHOT") else { return };
    STARTED.call_once(|| {
        std::thread::spawn(move || {
            let mut written = 0;
            loop {
                std::thread::sleep(std::time::Duration::from_secs(1));
                let shot = with(|d| {
                    d.sync();
                    (d.serial != written).then(|| {
                        written = d.serial;
                        let mut out = format!("P6\n{} {}\n255\n", d.w, d.h).into_bytes();
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

/// `VNVM_INPUT`: input records sent at given times (for tests without a page).
fn start_scripted_input() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    let Ok(script) = std::env::var("VNVM_INPUT") else { return };
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
                if crate::env_on!("VNVM_INPUT_TRACE") {
                    let mut n: c_int = 0;
                    unsafe { libc::ioctl(vnhost_input_fd(), libc::FIONREAD, &mut n) };
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
    /// What the page shows: the client area of the game's window (the shown window with the
    /// biggest one: no title bar, frame or desktop around it), widened by the windows above it
    /// that stick out (dialogs, menus); the whole screen without windows.
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
        let at = self.order.iter().position(|id| id == main).unwrap();
        let mut v = self.wins[main].client.and(&screen);
        for id in self.order[at + 1..].iter().filter(|id| shown(id)) {
            let r = self.wins[id].rect.and(&screen);
            if !r.empty() {
                v = v.or(&r);
            }
        }
        v
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
        let area = area.and(&self.screen_rect());
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
        self.bands.push_back((area.t, area.b, direct));
    }

    /// Composes `area` of `screen` from the windows.
    fn copy_in(&mut self, area: Rect) {
        let sw = self.w as usize;
        // from the topmost window that covers the whole area (the black background otherwise)
        let shown = |w: &Win| w.visible && !w.px().is_empty();
        let covers = |w: &Win| shown(w) && w.rect.and(&area) == area;
        let first = self.order.iter().rposition(|id| covers(&self.wins[id]));
        if first.is_none() {
            for y in area.t..area.b {
                let row = y as usize * sw;
                self.screen[row + area.l as usize..row + area.r as usize].fill(0xff00_0000);
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
                let dst = y as usize * sw + r.l as usize;
                let n = r.w() as usize;
                self.screen[dst..dst + n].copy_from_slice(&win.px()[src..src + n]);
            }
        }
    }
}

/// A display mode change (ChangeDisplaySettings): the screen takes the new size.
#[no_mangle]
pub extern "C" fn vnhost_display_set_size(w: c_int, h: c_int) {
    if w <= 0 || h <= 0 || w > 8192 || h > 8192 {
        return;
    }
    with(|d| {
        if (d.w, d.h) == (w, h) {
            return;
        }
        d.w = w;
        d.h = h;
        d.screen = vec![0xff00_0000; (w * h) as usize];
        d.bands.clear();
        d.stale = Rect::default();
        let all = d.screen_rect();
        d.compose(all);
    })
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
    with(|d| {
        let out = std::slice::from_raw_parts_mut(out, 4 * max.max(0) as usize);
        let mut n = 0;
        for id in d.order.iter().rev() {
            let w = &d.wins[id];
            if !w.visible || w.rect.empty() {
                continue;
            }
            if n as c_int == max {
                break;
            }
            out[4 * n..4 * n + 4].copy_from_slice(&[w.rect.l, w.rect.t, w.rect.r, w.rect.b]);
            n += 1;
        }
        n as c_int
    })
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
    with(|d| {
        let old = d.wins.get(&hwnd).map(|w| if w.visible { w.rect } else { Rect::default() }).unwrap_or_default();
        let win = d.wins.entry(hwnd).or_insert_with(|| Win { rect: Rect::default(), client, visible: false, pixels: Vec::new(), shared: 0 });
        if win.rect.w() != rect.w() || win.rect.h() != rect.h() {
            win.shared = 0;
            win.pixels = vec![0xff00_0000; (rect.w().max(0) * rect.h().max(0)) as usize];
        }
        win.rect = rect;
        win.client = client;
        win.visible = visible != 0;
        if restack != 0 || !d.order.contains(&hwnd) {
            d.order.retain(|&h| h != hwnd);
            let at = d.order.iter().position(|&h| h == above).unwrap_or(d.order.len());
            d.order.insert(at, hwnd);
        }
        let new = if visible != 0 { rect } else { Rect::default() };
        d.compose(old.or(&new));
    })
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

static DREW_BIG: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The end of a frame (a message loop turn): after big redraws, waits for the display's rate.
pub fn pace_frame() {
    if DREW_BIG.swap(false, std::sync::atomic::Ordering::Relaxed) {
        pace();
    }
}

fn pace() {
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
    if dirty.w() * dirty.h() >= BIG {
        DREW_BIG.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    with(|d| {
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
        // (nothing to copy when the window shows these very pixels)
        for y in (dirty.t..dirty.b).filter(|_| win.shared == 0) {
            let src = bits.offset(((y - oy) * stride + (dirty.l - ox)) as isize);
            let dst = y as usize * ww + dirty.l as usize;
            let n = dirty.w() as usize;
            // (the alpha byte is whatever the surface has: the page draws the frame opaque)
            win.pixels[dst..dst + n].copy_from_slice(std::slice::from_raw_parts(src, n));
        }
        let at = Rect { l: dirty.l + win.rect.l, t: dirty.t + win.rect.t, r: dirty.r + win.rect.l, b: dirty.b + win.rect.t };
        if win.visible {
            d.compose(at);
        }
    })
}

/// sake: like `vnhost_window_draw`, but `bits` is the window's whole surface (`w`*`h` pixels),
/// which the window then shows directly instead of copying it (one copy of each frame less).
/// The caller keeps `bits` alive and unmoved until `vnhost_window_unshare`.
#[no_mangle]
pub unsafe extern "C" fn vnhost_window_draw_shared(hwnd: u32, dirty: *const i32, bits: *const u32, w: c_int, h: c_int) {
    with(|d| {
        if let Some(win) = d.wins.get_mut(&hwnd) {
            if win.rect.w() == w && win.rect.h() == h {
                win.shared = bits as usize;
            }
        }
    });
    vnhost_window_draw(hwnd, dirty, bits, w, 0, 0)
}

/// sake: the window stops showing the surface `vnhost_window_draw_shared` gave it (about to be
/// freed or moved): it keeps a copy of it. When this returns, nothing reads the surface.
#[no_mangle]
pub extern "C" fn vnhost_window_unshare(hwnd: u32) {
    with(|d| {
        if let Some(win) = d.wins.get_mut(&hwnd) {
            if win.shared != 0 {
                let px = win.px().to_vec();
                win.pixels = px;
                win.shared = 0;
            }
        }
    })
}

#[no_mangle]
pub extern "C" fn vnhost_window_destroy(hwnd: u32) {
    with(|d| {
        if let Some(win) = d.wins.remove(&hwnd) {
            d.order.retain(|&h| h != hwnd);
            if win.visible {
                d.compose(win.rect);
            }
        }
    })
}

/// The screen for the page: returns the BGRA buffer (width*height pixels). `io[0]` is the
/// serial of the frame the caller shows (anything else, such as -1, for none); stores the
/// current serial there, which changes whenever the screen does, and in `io[1]..io[2]` the
/// rows that changed since (so a text cursor's blink doesn't upload the whole screen).
///
/// With `io[3]` = 1 the caller can take the changed rows from a window's own surface: when all
/// of them are one window's (a game redrawing its window; `screen` was not updated for them),
/// `io[3]` stays 1 and `io[4..10]` = the surface's address, its row length in pixels and the
/// window's rect on the screen (l t r b). Otherwise `io[3]` = 0 and `screen` has them.
#[no_mangle]
pub unsafe extern "C" fn vnhost_display_frame(io: *mut u32) -> *const u32 {
    let direct_ok = *io.add(3) == 1;
    let io = std::slice::from_raw_parts_mut(io, if direct_ok { 10 } else { 3 });
    with(|d| {
        let n = d.serial.wrapping_sub(io[0]) as usize;
        let mut one = None;
        let (t, b) = if n <= d.bands.len() {
            one = d.bands.iter().rev().take(n).next().map(|x| x.2).filter(|&id| id != 0);
            if d.bands.iter().rev().take(n).any(|x| Some(x.2) != one) {
                one = None;
            }
            d.bands.iter().rev().take(n).fold((d.h, 0), |(t, b), &(t2, b2, _)| (t.min(t2), b.max(b2)))
        } else {
            (0, d.h)
        };
        io[0] = d.serial;
        io[1] = t.max(0) as u32;
        io[2] = b.max(t).min(d.h) as u32;
        let win = one.filter(|_| direct_ok).and_then(|id| d.wins.get(&id)).filter(|w| w.shared != 0);
        match win {
            Some(w) => {
                io[3] = 1;
                io[4] = w.shared as u32;
                io[5] = w.rect.w() as u32;
                io[6..10].copy_from_slice(&[w.rect.l as u32, w.rect.t as u32, w.rect.r as u32, w.rect.b as u32]);
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
    })
}

// ---------------------------------------------------------------------------------------------
// input: 16-byte records { kind, x, y, data } through a pipe the driver's desktop thread polls

/// Input record kinds, shared with the driver.
pub const INPUT_MOUSE_MOVE: u32 = 1;
pub const INPUT_MOUSE_DOWN: u32 = 2; // data: button 0 left, 1 right, 2 middle
pub const INPUT_MOUSE_UP: u32 = 3;
pub const INPUT_WHEEL: u32 = 4; // data: delta (120 per notch)
pub const INPUT_KEY_DOWN: u32 = 5; // data: vk | scan << 16
pub const INPUT_KEY_UP: u32 = 6;

static INPUT: Mutex<Option<(c_int, c_int)>> = Mutex::new(None);

fn input_pipe() -> Option<(c_int, c_int)> {
    let mut g = INPUT.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        let mut fds = [0; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return None;
        }
        unsafe { libc::fcntl(fds[0], libc::F_SETFL, libc::O_NONBLOCK) };
        unsafe { libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK) };
        // not the calling process's: they outlive it
        crate::fd::give(fds[0], crate::proc::HOST_PID);
        crate::fd::give(fds[1], crate::proc::HOST_PID);
        *g = Some((fds[0], fds[1]));
    }
    *g
}

/// The read end of the input pipe (for the desktop process's driver).
#[no_mangle]
pub extern "C" fn vnhost_input_fd() -> c_int {
    input_pipe().map_or(-1, |p| p.0)
}

#[no_mangle]
pub extern "C" fn vnhost_input_push(kind: u32, x: i32, y: i32, data: u32) -> c_int {
    let Some((_, w)) = input_pipe() else { return -1 };
    let rec = [kind, x as u32, y as u32, data];
    let n = unsafe { libc::write(w, rec.as_ptr() as *const libc::c_void, 16) };
    (n == 16) as c_int - 1
}
