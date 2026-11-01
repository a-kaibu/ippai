//! gdi32: DCs, bitmaps (DDBs and DIB sections in guest memory), brushes, pens, fonts,
//! palettes, regions, blits with raster ops, and text through TrueType fonts (ab_glyph).
//!
//! Every pixel operation runs on a `View`: rows of some bit depth at a raw address, either a
//! bitmap's bits in guest memory or a top-level window's surface (user.rs). One lock holds all
//! GDI objects; a draw op takes it, then the surface's lock, draws, and sends what changed to
//! the display after letting go of both.

#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _};

use crate::api::{implement, Ctx};
use crate::mem::{self, cstr, put32, r16, r32, r8, w16, w32, w8, wstr};
use crate::text::{a2s, a2w, mb2w};
use crate::user::{self, Rect};

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

// ---------------------------------------------------------------------------------------------
// objects

#[derive(Clone)]
pub struct Bitmap {
    pub w: i32,
    pub h: i32,
    pub bpp: u16,
    pub bits: u32,
    pub stride: u32,
    pub bottom_up: bool,
    pub pal: Vec<u32>,
    pub r565: bool,
    pub dib: bool,
    /// bits from mem::alloc (DIB sections) rather than the heap; 0 = not ours
    pub own: u8,
}

#[derive(Clone, Copy)]
pub struct Brush {
    pub style: u32,
    pub color: u32,
    pub hatch: u32,
    pub pattern: u32,
}

#[derive(Clone, Copy)]
pub struct Pen {
    pub style: u32,
    pub width: i32,
    pub color: u32,
}

#[derive(Clone)]
pub struct LogFont {
    pub height: i32,
    pub width: i32,
    pub esc: i32,
    pub orient: i32,
    pub weight: i32,
    pub italic: u8,
    pub underline: u8,
    pub strike: u8,
    pub charset: u8,
    pub out_prec: u8,
    pub clip_prec: u8,
    pub quality: u8,
    pub pitch: u8,
    pub face: String,
}

pub enum G {
    Bitmap(Bitmap),
    Brush(Brush),
    Pen(Pen),
    Font(LogFont),
    Palette(Vec<u32>),
    Region(Vec<Rect>),
    Dc(Box<Dc>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Kind {
    Mem,
    Window(u32),
    Screen,
}

#[derive(Clone, Copy)]
pub struct DcState {
    pub bitmap: u32,
    pub brush: u32,
    pub pen: u32,
    pub font: u32,
    pub palette: u32,
    pub text: u32,
    pub bk: u32,
    pub bk_mode: u32,
    pub rop2: u32,
    pub stretch: u32,
    pub map_mode: u32,
    pub align: u32,
    pub vp: (i32, i32),
    pub win: (i32, i32),
    pub pos: (i32, i32),
    pub brush_org: (i32, i32),
    /// clip box in device coordinates
    pub clip: Option<Rect>,
    pub char_extra: i32,
    pub dc_brush: u32,
    pub dc_pen: u32,
}

pub struct Dc {
    pub kind: Kind,
    /// for BeginPaint DCs: the update rectangle (client coordinates)
    pub paint: Option<Rect>,
    pub s: DcState,
    pub saved: Vec<DcState>,
}

struct Objs {
    m: HashMap<u32, G>,
    next: u32,
    solid: HashMap<u32, u32>,
}

static OBJS: Mutex<Option<Objs>> = Mutex::new(None);

fn objs() -> MutexGuard<'static, Option<Objs>> {
    OBJS.lock().unwrap_or_else(|e| e.into_inner())
}

fn with<R>(f: impl FnOnce(&mut Objs) -> R) -> R {
    let mut g = objs();
    f(g.get_or_insert_with(|| Objs { m: HashMap::new(), next: 0x0a00_0100, solid: HashMap::new() }))
}

fn add(o: G) -> u32 {
    with(|t| {
        let h = t.next;
        t.next += 4;
        t.m.insert(h, o);
        h
    })
}

/// Stock objects: GetStockObject(i) is STOCK + 4*i.
const STOCK: u32 = 0x0a00_0000;
const STOCK_N: u32 = 20;
const DEFAULT_BITMAP: u32 = STOCK + 4 * 21;

fn is_stock(h: u32) -> bool {
    (STOCK..=DEFAULT_BITMAP).contains(&h)
}

fn rgb(cr: u32) -> u32 {
    (cr & 0xff) << 16 | (cr & 0xff00) | (cr >> 16 & 0xff)
}

fn cref(c: u32) -> u32 {
    rgb(c)
}

pub const SYS_FACE: &str = "ＭＳ Ｐゴシック";

fn stock_font(face: &str, height: i32) -> LogFont {
    LogFont {
        height,
        width: 0,
        esc: 0,
        orient: 0,
        weight: 400,
        italic: 0,
        underline: 0,
        strike: 0,
        charset: 128,
        out_prec: 0,
        clip_prec: 0,
        quality: 0,
        pitch: 0,
        face: face.into(),
    }
}

pub fn init() {
    with(|t| {
        let solid = |c: u32| G::Brush(Brush { style: 0, color: c, hatch: 0, pattern: 0 });
        let stock: Vec<G> = vec![
            solid(0xffffff),
            solid(0xc0c0c0),
            solid(0x808080),
            solid(0x404040),
            solid(0),
            G::Brush(Brush { style: 1, color: 0, hatch: 0, pattern: 0 }),
            G::Pen(Pen { style: 0, width: 1, color: 0xffffff }),
            G::Pen(Pen { style: 0, width: 1, color: 0 }),
            G::Pen(Pen { style: 5, width: 1, color: 0 }),
            G::Font(stock_font("Terminal", 12)),
            G::Font(stock_font("ＭＳ ゴシック", 12)),
            G::Font(stock_font("ＭＳ ゴシック", 12)),
            G::Font(stock_font("ＭＳ ゴシック", 12)),
            G::Font(stock_font("System", 16)),
            G::Font(stock_font("System", 16)),
            G::Palette(default_palette()),
            G::Font(stock_font("ＭＳ ゴシック", 16)),
            G::Font(stock_font(SYS_FACE, -12)),
            solid(0xffffff),
            G::Pen(Pen { style: 0, width: 1, color: 0 }),
        ];
        for (i, o) in stock.into_iter().enumerate() {
            t.m.insert(STOCK + 4 * i as u32, o);
        }
        t.m.insert(DEFAULT_BITMAP, G::Bitmap(new_bitmap_raw(1, 1, 1, 0)));
    });
    register();
}

fn default_palette() -> Vec<u32> {
    let mut p = vec![
        0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0xc0dcc0, 0xa6caf0,
    ];
    p.extend([0xfffbf0, 0xa0a0a4, 0x808080, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff]);
    // (RGB values here are 0xRRGGBB already, in "device" order)
    p
}

fn halftone_palette() -> Vec<u32> {
    let mut p = Vec::with_capacity(256);
    for r in 0..6u32 {
        for g in 0..6u32 {
            for b in 0..6u32 {
                p.push((r * 51) << 16 | (g * 51) << 8 | b * 51);
            }
        }
    }
    while p.len() < 256 {
        let v = (p.len() as u32 - 216) * 255 / 39;
        p.push(v << 16 | v << 8 | v);
    }
    p
}

/// A bitmap with fresh (zeroed) bits on the guest heap; DDB rows are WORD-aligned.
fn new_bitmap_raw(w: i32, h: i32, bpp: u16, init: u32) -> Bitmap {
    let stride = (((w as u32 * bpp as u32) + 15) / 16) * 2;
    let size = (stride * h as u32).max(4);
    let bits = mem::heap_alloc(size, true);
    if init != 0 {
        mem::copy(bits, init, size);
    }
    let pal = if bpp == 1 { vec![0, 0xffffff] } else { Vec::new() };
    Bitmap { w, h, bpp, bits, stride, bottom_up: false, pal, r565: false, dib: false, own: 1 }
}

/// A new zeroed DDB (for user32: icon and cursor masks).
pub fn new_bitmap(w: i32, h: i32, bpp: u16) -> u32 {
    add(G::Bitmap(new_bitmap_raw(w, h, bpp, 0)))
}

fn free_bitmap(b: &Bitmap) {
    match b.own {
        1 => {
            mem::heap_free(b.bits);
        }
        2 => {
            mem::release(b.bits);
        }
        _ => {}
    }
}

pub fn solid_brush(color: u32) -> u32 {
    if let Some(h) = with(|t| t.solid.get(&color).copied()) {
        return h;
    }
    let h = add(G::Brush(Brush { style: 0, color, hatch: 0, pattern: 0 }));
    with(|t| t.solid.insert(color, h));
    h
}

// ---------------------------------------------------------------------------------------------
// views: pixels at an address

#[derive(Clone)]
struct View {
    /// address of row 0 (the top row)
    top: *mut u8,
    stride: isize,
    w: i32,
    h: i32,
    bpp: u16,
    r565: bool,
    pal: Vec<u32>,
    /// a monochrome DDB: colors map through the DC's text/background colors
    mono: bool,
}

unsafe impl Send for View {}

impl View {
    fn of(b: &Bitmap) -> View {
        let base = b.bits as usize as *mut u8;
        let (top, stride) = if b.bottom_up && b.h > 1 {
            (unsafe { base.add(((b.h - 1) as u32 * b.stride) as usize) }, -(b.stride as isize))
        } else {
            (base, b.stride as isize)
        };
        View { top, stride, w: b.w, h: b.h, bpp: b.bpp, r565: b.r565, pal: b.pal.clone(), mono: b.bpp == 1 && !b.dib }
    }
    fn surface(px: &mut [u32], w: i32, h: i32) -> View {
        View { top: px.as_mut_ptr() as *mut u8, stride: w as isize * 4, w, h, bpp: 32, r565: false, pal: Vec::new(), mono: false }
    }
    #[inline]
    fn row(&self, y: i32) -> *mut u8 {
        unsafe { self.top.offset(y as isize * self.stride) }
    }
    fn bounds(&self) -> Rect {
        Rect { l: 0, t: 0, r: self.w, b: self.h }
    }
    /// The pixel as 0xRRGGBB (32 bpp: all four bytes).
    #[inline]
    fn get(&self, x: i32, y: i32) -> u32 {
        let r = self.row(y);
        unsafe {
            match self.bpp {
                32 => (r as *const u32).add(x as usize).read_unaligned(),
                24 => {
                    let p = r.add(3 * x as usize);
                    (*p.add(2) as u32) << 16 | (*p.add(1) as u32) << 8 | *p as u32
                }
                16 => {
                    let v = (r as *const u16).add(x as usize).read_unaligned() as u32;
                    if self.r565 {
                        let (r5, g6, b5) = (v >> 11 & 31, v >> 5 & 63, v & 31);
                        (r5 << 3 | r5 >> 2) << 16 | (g6 << 2 | g6 >> 4) << 8 | (b5 << 3 | b5 >> 2)
                    } else {
                        let (r5, g5, b5) = (v >> 10 & 31, v >> 5 & 31, v & 31);
                        (r5 << 3 | r5 >> 2) << 16 | (g5 << 3 | g5 >> 2) << 8 | (b5 << 3 | b5 >> 2)
                    }
                }
                8 => self.pal_at(*r.add(x as usize) as usize),
                4 => {
                    let b = *r.add(x as usize / 2);
                    self.pal_at(if x & 1 == 0 { b >> 4 } else { b & 15 } as usize)
                }
                1 => {
                    let b = *r.add(x as usize / 8);
                    self.pal_at((b >> (7 - (x & 7)) & 1) as usize)
                }
                _ => 0,
            }
        }
    }
    fn pal_at(&self, i: usize) -> u32 {
        self.pal.get(i).copied().unwrap_or(if i == 0 { 0 } else { 0xffffff })
    }
    fn index_of(&self, c: u32) -> u32 {
        let c = c & 0xffffff;
        if self.bpp == 1 && self.pal.len() == 2 && self.pal[0] == 0 && self.pal[1] == 0xffffff {
            return (c != 0) as u32;
        }
        let mut best = (u32::MAX, 0);
        for (i, &p) in self.pal.iter().enumerate() {
            let d = |s: u32| ((c >> s & 255) as i32 - (p >> s & 255) as i32).pow(2) as u32;
            let e = d(16) + d(8) + d(0);
            if e < best.0 {
                best = (e, i as u32);
                if e == 0 {
                    break;
                }
            }
        }
        best.1
    }
    #[inline]
    fn put(&self, x: i32, y: i32, c: u32) {
        let r = self.row(y);
        unsafe {
            match self.bpp {
                32 => (r as *mut u32).add(x as usize).write_unaligned(c),
                24 => {
                    let p = r.add(3 * x as usize);
                    *p = c as u8;
                    *p.add(1) = (c >> 8) as u8;
                    *p.add(2) = (c >> 16) as u8;
                }
                16 => {
                    let v = if self.r565 {
                        (c >> 19 & 31) << 11 | (c >> 10 & 63) << 5 | (c >> 3 & 31)
                    } else {
                        (c >> 19 & 31) << 10 | (c >> 11 & 31) << 5 | (c >> 3 & 31)
                    };
                    (r as *mut u16).add(x as usize).write_unaligned(v as u16)
                }
                8 => *r.add(x as usize) = self.index_of(c) as u8,
                4 => {
                    let i = self.index_of(c) as u8;
                    let p = r.add(x as usize / 2);
                    *p = if x & 1 == 0 { (*p & 0x0f) | i << 4 } else { (*p & 0xf0) | i };
                }
                1 => {
                    let i = self.index_of(c) as u8;
                    let p = r.add(x as usize / 8);
                    let m = 0x80u8 >> (x & 7);
                    *p = if i != 0 { *p | m } else { *p & !m };
                }
                _ => {}
            }
        }
    }
}

/// A DC's drawing surface: a view, the DC origin in it and the clip box (view coordinates).
struct Canvas {
    v: View,
    ox: i32,
    oy: i32,
    clip: Rect,
}

impl Canvas {
    fn dev(&self, x: i32, y: i32) -> (i32, i32) {
        (x + self.ox, y + self.oy)
    }
    fn rect(&self, r: Rect) -> Rect {
        r.off(self.ox, self.oy)
    }
}

// ---------------------------------------------------------------------------------------------
// DCs

fn new_dc(kind: Kind, paint: Option<Rect>) -> u32 {
    let s = DcState {
        bitmap: DEFAULT_BITMAP,
        brush: STOCK,
        pen: STOCK + 4 * 7,
        font: STOCK + 4 * 13,
        palette: STOCK + 4 * 15,
        text: 0,
        bk: 0xffffff,
        bk_mode: 2,
        rop2: 13,
        stretch: 1,
        map_mode: 1,
        align: 0,
        vp: (0, 0),
        win: (0, 0),
        pos: (0, 0),
        brush_org: (0, 0),
        clip: None,
        char_extra: 0,
        dc_brush: 0xffffff,
        dc_pen: 0,
    };
    add(G::Dc(Box::new(Dc { kind, paint, s, saved: Vec::new() })))
}

pub fn window_dc(hwnd: u32, paint: Option<Rect>) -> u32 {
    let paint = paint.filter(|r| !r.empty());
    new_dc(Kind::Window(hwnd), paint)
}

pub fn screen_dc() -> u32 {
    new_dc(Kind::Screen, None)
}

pub fn release_dc(dc: u32) {
    with(|t| {
        if matches!(t.m.get(&dc), Some(G::Dc(d)) if d.kind != Kind::Mem) {
            t.m.remove(&dc);
        }
    });
}

pub fn dc_window(dc: u32) -> u32 {
    with(|t| match t.m.get(&dc) {
        Some(G::Dc(d)) => match d.kind {
            Kind::Window(h) => h,
            _ => 0,
        },
        _ => 0,
    })
}

fn dc_state(dc: u32) -> Option<DcState> {
    with(|t| match t.m.get(&dc) {
        Some(G::Dc(d)) => Some(d.s),
        _ => None,
    })
}

fn with_dc<R>(dc: u32, f: impl FnOnce(&mut Dc) -> R) -> Option<R> {
    with(|t| match t.m.get_mut(&dc) {
        Some(G::Dc(d)) => Some(f(d)),
        _ => None,
    })
}

/// What a DC draws on, with the locks it needs.
enum Tgt {
    Bmp(View),
    Win { surf: Arc<Mutex<user::Surface>>, top: u32, x: i32, y: i32, clip: Rect },
    None,
}

fn target_of(t: &Objs, d: &Dc) -> Tgt {
    match d.kind {
        Kind::Mem => match t.m.get(&d.s.bitmap) {
            Some(G::Bitmap(b)) => Tgt::Bmp(View::of(b)),
            _ => Tgt::None,
        },
        Kind::Window(h) => match user::target(h) {
            Some(tg) => {
                let mut clip = tg.clip;
                if let Some(p) = d.paint {
                    clip = clip.and(&p.off(tg.x, tg.y));
                }
                Tgt::Win { surf: tg.surface, top: tg.top, x: tg.x, y: tg.y, clip }
            }
            None => Tgt::None,
        },
        Kind::Screen => Tgt::None,
    }
}

fn canvas_clip(s: &DcState, v: &View, ox: i32, oy: i32, extra: Rect) -> Rect {
    let mut c = v.bounds().and(&extra);
    if let Some(cl) = s.clip {
        c = c.and(&cl.off(ox - (s.vp.0 - s.win.0), oy - (s.vp.1 - s.win.1)));
    }
    c
}

/// Runs a draw op on DC `dc`; `f` returns the changed rectangle (view coordinates).
fn draw(dc: u32, f: impl FnOnce(&Objs, &DcState, &mut Canvas) -> Rect) -> bool {
    let mut flush = None;
    {
        let mut g = objs();
        let Some(t) = g.as_mut() else { return false };
        let Some(G::Dc(d)) = t.m.get(&dc) else { return false };
        let s = d.s;
        let tg = target_of(t, d);
        let org = (s.vp.0 - s.win.0, s.vp.1 - s.win.1);
        match tg {
            Tgt::Bmp(v) => {
                let clip = canvas_clip(&s, &v, org.0, org.1, v.bounds());
                let mut c = Canvas { v, ox: org.0, oy: org.1, clip };
                f(t, &s, &mut c);
            }
            Tgt::Win { surf, top, x, y, clip } => {
                let mut sg = surf.lock().unwrap();
                let (w, h) = (sg.w, sg.h);
                let v = View::surface(&mut sg.px, w, h);
                let (ox, oy) = (org.0 + x, org.1 + y);
                let clip = canvas_clip(&s, &v, ox, oy, clip);
                let mut c = Canvas { v, ox, oy, clip };
                let r = f(t, &s, &mut c).and(&c.clip);
                if !r.empty() {
                    flush = Some((top, r));
                }
            }
            Tgt::None => return true,
        }
    }
    if let Some((top, r)) = flush {
        user::flush(top, r);
    }
    true
}

/// A copy of a DC's pixels in `r` (device coordinates) as a source for blits; with the
/// offset of the copy. Memory DCs are used in place.
fn source(t: &Objs, dc: u32) -> Option<(View, i32, i32, DcState)> {
    let Some(G::Dc(d)) = t.m.get(&dc) else { return None };
    let s = d.s;
    let org = (s.vp.0 - s.win.0, s.vp.1 - s.win.1);
    match target_of(t, d) {
        Tgt::Bmp(v) => Some((v, org.0, org.1, s)),
        Tgt::Win { surf, x, y, .. } => {
            let g = surf.lock().unwrap();
            let mut px = g.px.clone();
            let (w, h) = (g.w, g.h);
            drop(g);
            let v = View::surface(&mut px, w, h);
            // the copy lives as long as the process: sources from windows are rare
            std::mem::forget(px);
            Some((v, org.0 + x, org.1 + y, s))
        }
        Tgt::None => {
            let mut px = vec![0u32; 1];
            let v = View::surface(&mut px, 1, 1);
            std::mem::forget(px);
            Some((View { w: 0, h: 0, ..v }, 0, 0, s))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// raster ops

#[inline]
fn rop3(op: u8, p: u32, s: u32, d: u32) -> u32 {
    match op {
        0xcc => s,
        0xf0 => p,
        0x00 => 0,
        0xff => 0xffff_ffff,
        0x88 => s & d,
        0xee => s | d,
        0x66 => s ^ d,
        0x55 => !d,
        0x5a => p ^ d,
        0x33 => !s,
        0xc0 => p & s,
        0xaa => d,
        0xbb => !s | d,
        0x44 => s & !d,
        0x11 => !(s | d),
        0xb8 => (s & p) | (!s & d),
        0xe2 => (s & d) | (!s & p),
        _ => {
            let mut r = 0;
            for i in 0..8 {
                if op >> i & 1 != 0 {
                    let pp = if i & 4 != 0 { p } else { !p };
                    let ss = if i & 2 != 0 { s } else { !s };
                    let dd = if i & 1 != 0 { d } else { !d };
                    r |= pp & ss & dd;
                }
            }
            r
        }
    }
}

fn uses_src(op: u8) -> bool {
    (op >> 2 ^ op) & 0x33 != 0
}

fn uses_pat(op: u8) -> bool {
    (op >> 4 ^ op) & 0x0f != 0
}

/// A brush as a function of the device pixel.
enum Pat {
    Solid(u32),
    None,
    Tile(View, i32, i32),
}

impl Pat {
    fn at(&self, x: i32, y: i32) -> u32 {
        match self {
            Pat::Solid(c) => *c,
            Pat::None => 0,
            Pat::Tile(v, ox, oy) => {
                if v.w <= 0 || v.h <= 0 {
                    return 0;
                }
                v.get((x - ox).rem_euclid(v.w), (y - oy).rem_euclid(v.h)) & 0xffffff
            }
        }
    }
}

fn pattern(t: &Objs, s: &DcState, brush: u32) -> Pat {
    let brush = if brush == STOCK + 4 * 18 { return Pat::Solid(rgb(s.dc_brush)) } else { brush };
    if (1..=31).contains(&brush) {
        return Pat::Solid(rgb(sys_color(brush - 1)));
    }
    match t.m.get(&brush) {
        Some(G::Brush(b)) => match b.style {
            1 => Pat::None,
            3 | 5 => match t.m.get(&b.pattern) {
                Some(G::Bitmap(bm)) => Pat::Tile(View::of(bm), s.brush_org.0, s.brush_org.1),
                _ => Pat::Solid(rgb(b.color)),
            },
            _ => Pat::Solid(rgb(b.color)),
        },
        _ => Pat::Solid(0xffffff),
    }
}

fn sys_color(i: u32) -> u32 {
    match i {
        0 => 0xc8c8c8,
        1 => 0,
        2 => 0xd1b499,
        3 => 0xdbcdbf,
        4 => 0xf0f0f0,
        5 => 0xffffff,
        6 => 0x646464,
        7 | 8 | 9 => 0,
        10 | 11 => 0xb4b4b4,
        12 => 0xababab,
        13 => 0xd77800,
        14 => 0xffffff,
        15 => 0xf0f0f0,
        16 => 0xa0a0a0,
        17 => 0x6d6d6d,
        18 => 0,
        20 => 0xffffff,
        21 => 0x696969,
        _ => 0xf0f0f0,
    }
}

/// Fills `r` (device coordinates) with a raster op of the pattern.
fn pat_blt(c: &mut Canvas, r: Rect, pat: &Pat, op: u8) -> Rect {
    let r = r.and(&c.clip);
    if r.empty() {
        return r;
    }
    if let (Pat::Solid(col), 0xf0, 32) = (pat, op, c.v.bpp) {
        for y in r.t..r.b {
            let row = c.v.row(y) as *mut u32;
            for x in r.l..r.r {
                unsafe { row.add(x as usize).write_unaligned(*col) };
            }
        }
        return r;
    }
    for y in r.t..r.b {
        for x in r.l..r.r {
            let d = c.v.get(x, y);
            c.v.put(x, y, rop3(op, pat.at(x, y), 0, d));
        }
    }
    r
}

pub struct Src<'a> {
    v: &'a View,
    /// source rectangle (view coordinates); w/h may be negative (mirrored)
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// the source DC's background color (color -> mono)
    bk: u32,
}

/// The blit at the heart of BitBlt/StretchBlt/StretchDIBits: dst rect (device coordinates,
/// w/h may be negative), source, raster op. Nearest-neighbor stretching.
fn blit(c: &mut Canvas, dst: (i32, i32, i32, i32), src: Option<Src>, op: u8, pat: &Pat, text: u32, bk: u32) -> Rect {
    let (mut dx, mut dy, mut dw, mut dh) = dst;
    let mut flip = (false, false);
    if dw < 0 {
        dx += dw;
        dw = -dw;
        flip.0 = true;
    }
    if dh < 0 {
        dy += dh;
        dh = -dh;
        flip.1 = true;
    }
    let full = Rect { l: dx, t: dy, r: dx + dw, b: dy + dh };
    let r = full.and(&c.clip);
    if r.empty() {
        return r;
    }
    let Some(s) = src.filter(|_| uses_src(op)) else {
        return pat_blt(c, r, pat, op);
    };
    let (mut sx, mut sy, mut sw, mut sh) = (s.x, s.y, s.w, s.h);
    if sw < 0 {
        sx += sw;
        sw = -sw;
        flip.0 = !flip.0;
    }
    if sh < 0 {
        sy += sh;
        sh = -sh;
        flip.1 = !flip.1;
    }
    if sw == 0 || sh == 0 {
        return Rect::default();
    }
    let map_x = |x: i32| -> i32 {
        let i = x - dx;
        let k = if sw == dw { i } else { ((i as i64 * sw as i64) / dw as i64) as i32 };
        if flip.0 {
            sx + sw - 1 - k
        } else {
            sx + k
        }
    };
    let map_y = |y: i32| -> i32 {
        let i = y - dy;
        let k = if sh == dh { i } else { ((i as i64 * sh as i64) / dh as i64) as i32 };
        if flip.1 {
            sy + sh - 1 - k
        } else {
            sy + k
        }
    };
    let sv = s.v;
    let (sw_, sh_) = (sv.w, sv.h);
    let in_src = |x: i32, y: i32| x >= 0 && y >= 0 && x < sw_ && y < sh_;
    let d = &c.v;
    // fast path: same depth (32), copy, no stretch or flip
    if op == 0xcc && d.bpp == 32 && sv.bpp == 32 && sw == dw && sh == dh && !flip.0 && !flip.1 {
        let x0 = r.l.max(dx - sx);
        let x1 = r.r.min(dx - sx + sw_);
        if x0 >= x1 {
            return Rect::default();
        }
        for y in r.t..r.b {
            let syy = sy + (y - dy);
            if syy < 0 || syy >= sh_ {
                continue;
            }
            unsafe {
                let sp = (sv.row(syy) as *const u32).add((sx + (x0 - dx)) as usize);
                let dp = (d.row(y) as *mut u32).add(x0 as usize);
                std::ptr::copy(sp, dp, (x1 - x0) as usize);
            }
        }
        return Rect { l: x0, r: x1, ..r };
    }
    let xs: Vec<i32> = (r.l..r.r).map(map_x).collect();
    let mono_to_color = sv.mono && !d.mono;
    let color_to_mono = d.mono && !sv.mono;
    let need_dst = (op >> 1 ^ op) & 0x55 != 0;
    let need_pat = uses_pat(op);
    for y in r.t..r.b {
        let syy = map_y(y);
        for (k, x) in (r.l..r.r).enumerate() {
            let sxx = xs[k];
            if !in_src(sxx, syy) {
                continue;
            }
            let mut sc = sv.get(sxx, syy);
            if mono_to_color {
                sc = if sc & 0xffffff == 0 { text } else { bk };
            } else if color_to_mono {
                sc = if sc & 0xffffff == s.bk { 0xffffff } else { 0 };
            }
            if op == 0xcc {
                if d.bpp != 32 || sv.bpp != 32 {
                    sc &= 0xffffff;
                }
                d.put(x, y, sc);
                continue;
            }
            let dc = if need_dst { d.get(x, y) } else { 0 };
            let p = if need_pat { pat.at(x, y) } else { 0 };
            d.put(x, y, rop3(op, p, sc, dc));
        }
    }
    r
}

pub fn fill_rect_brush(dc: u32, r: Rect, brush: u32) {
    draw(dc, |t, s, c| {
        let pat = pattern(t, s, brush);
        if matches!(pat, Pat::None) {
            return Rect::default();
        }
        let r = c.rect(r);
        pat_blt(c, r, &pat, 0xf0)
    });
}

// ---------------------------------------------------------------------------------------------
// DIBs described by a BITMAPINFO in guest memory

#[derive(Clone)]
struct DibInfo {
    w: i32,
    h: i32,
    bpp: u16,
    compression: u32,
    stride: u32,
    pal: Vec<u32>,
    r565: bool,
    top_down: bool,
}

fn dib_info(bmi: u32, usage: u32, dc_pal: &[u32]) -> Option<DibInfo> {
    let size = r32(bmi);
    let (w, h, bpp, comp, clr_used, ctab, entry) = if size == 12 {
        (r16(bmi + 4) as i32, r16(bmi + 6) as i16 as i32, r16(bmi + 10), 0, 0, bmi + 12, 3)
    } else {
        (r32(bmi + 4) as i32, r32(bmi + 8) as i32, r16(bmi + 14), r32(bmi + 16), r32(bmi + 32), bmi + size, 4)
    };
    if w <= 0 || h == 0 || bpp == 0 {
        return None;
    }
    let mut pal = Vec::new();
    let mut r565 = false;
    if bpp <= 8 {
        let n = if clr_used != 0 { clr_used.min(1 << bpp) } else { 1 << bpp };
        for i in 0..n {
            if usage == 1 {
                let k = r16(ctab + 2 * i) as usize;
                pal.push(dc_pal.get(k).copied().unwrap_or(0));
            } else {
                let p = ctab + entry * i;
                pal.push((r8(p + 2) as u32) << 16 | (r8(p + 1) as u32) << 8 | r8(p) as u32);
            }
        }
    } else if comp == 3 && bpp == 16 {
        let gmask = if size >= 52 { r32(bmi + 44) } else { r32(bmi + size + 4) };
        r565 = gmask == 0x7e0;
    }
    let stride = ((w as u32 * bpp as u32 + 31) / 32) * 4;
    Some(DibInfo { w, h: h.abs(), bpp, compression: comp, stride, pal, r565, top_down: h < 0 })
}

fn dib_view(i: &DibInfo, bits: u32) -> View {
    let b = Bitmap {
        w: i.w,
        h: i.h,
        bpp: i.bpp,
        bits,
        stride: i.stride,
        bottom_up: !i.top_down,
        pal: i.pal.clone(),
        r565: i.r565,
        dib: true,
        own: 0,
    };
    View::of(&b)
}

fn dc_palette(t: &Objs, s: &DcState) -> Vec<u32> {
    match t.m.get(&s.palette) {
        Some(G::Palette(p)) => p.clone(),
        _ => default_palette(),
    }
}

// ---------------------------------------------------------------------------------------------
// fonts

struct Face {
    names: &'static [&'static str],
    file: &'static str,
    fixed: bool,
    roman: bool,
}

const FACES: &[Face] = &[
    Face { names: &["ＭＳ ゴシック", "MS Gothic"], file: "msgothic.ttf", fixed: true, roman: false },
    Face { names: &["ＭＳ Ｐゴシック", "MS PGothic"], file: "mspgothic.ttf", fixed: false, roman: false },
    Face { names: &["ＭＳ 明朝", "MS Mincho"], file: "msmincho.ttf", fixed: true, roman: true },
    Face { names: &["ＭＳ Ｐ明朝", "MS PMincho"], file: "mspmincho.ttf", fixed: false, roman: true },
    Face { names: &["MS UI Gothic"], file: "msuigothic.ttf", fixed: false, roman: false },
];

static FILES: OnceLock<Mutex<HashMap<usize, Option<Arc<FontVec>>>>> = OnceLock::new();

fn font_file(i: usize) -> Option<Arc<FontVec>> {
    let m = FILES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut g = m.lock().unwrap();
    g.entry(i)
        .or_insert_with(|| {
            let dirs = [std::env::var("VNWIN_FONTS").ok(), crate::files::resolve("C:\\windows\\Fonts\\")];
            for d in dirs.into_iter().flatten() {
                let p = format!("{}/{}", d.trim_end_matches('/'), FACES[i].file);
                if let Ok(data) = std::fs::read(&p) {
                    if let Ok(f) = FontVec::try_from_vec(data) {
                        return Some(Arc::new(f));
                    }
                }
            }
            eprintln!("vnwin: font {} not found", FACES[i].file);
            None
        })
        .clone()
}

/// The face for a LOGFONT face name (the Japanese and English names, '@' for vertical).
fn face_index(lf: &LogFont) -> usize {
    let name = lf.face.trim_start_matches('@');
    for (i, f) in FACES.iter().enumerate() {
        if f.names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            return i;
        }
    }
    let family = lf.pitch & 0xf0;
    let variable = lf.pitch & 3 == 2;
    match (family == 0x10 || family == 0x40, variable || family == 0x20) {
        (true, true) => 3,
        (true, false) => 2,
        (false, _) if lf.pitch & 3 == 1 || family == 0x30 => 0,
        _ => 1,
    }
}

#[derive(Clone)]
struct Real {
    face: usize,
    font: Arc<FontVec>,
    /// em size in pixels
    em: f32,
    ascent: i32,
    descent: i32,
    bold: bool,
}

fn realize(lf: &LogFont) -> Option<Real> {
    let face = face_index(lf);
    let font = font_file(face)?;
    let upem = font.units_per_em().unwrap_or(1000.0);
    let (asc, desc) = (font.ascent_unscaled(), -font.descent_unscaled());
    let em = if lf.height < 0 {
        -lf.height as f32
    } else if lf.height > 0 {
        lf.height as f32 * upem / (asc + desc)
    } else {
        12.0
    };
    let ascent = (asc * em / upem).round() as i32;
    let descent = (desc * em / upem).round() as i32;
    Some(Real { face, font, em, ascent, descent, bold: lf.weight >= 600 })
}

pub struct Glyph {
    /// left of the black box from the pen
    x0: i32,
    /// top of the black box above the baseline
    top: i32,
    w: i32,
    h: i32,
    cov: Vec<u8>,
    adv: i32,
}

type GlyphKey = (usize, u32, u32, bool);
static GLYPHS: OnceLock<Mutex<HashMap<GlyphKey, Arc<Glyph>>>> = OnceLock::new();

fn glyph(r: &Real, ch: u32) -> Arc<Glyph> {
    let key = (r.face, r.em.to_bits(), ch, r.bold);
    let m = GLYPHS.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(g) = m.lock().unwrap().get(&key) {
        return g.clone();
    }
    let g = Arc::new(raster(r, ch));
    m.lock().unwrap().insert(key, g.clone());
    g
}

fn raster(r: &Real, ch: u32) -> Glyph {
    let f = &*r.font;
    let upem = f.units_per_em().unwrap_or(1000.0);
    let height = f.ascent_unscaled() - f.descent_unscaled();
    let scale = PxScale::from(r.em * height / upem);
    let c = char::from_u32(ch).unwrap_or('?');
    let id = f.glyph_id(c);
    let adv = f.as_scaled(scale).h_advance(id).round() as i32;
    let mut g = Glyph { x0: 0, top: 0, w: 0, h: 0, cov: Vec::new(), adv };
    if let Some(o) = f.outline_glyph(id.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0))) {
        let b = o.px_bounds();
        let (w, h) = (b.width() as i32, b.height() as i32);
        if w > 0 && h > 0 {
            let mut cov = vec![0u8; (w * h) as usize];
            o.draw(|x, y, v| {
                let i = (y as i32 * w + x as i32) as usize;
                if i < cov.len() {
                    cov[i] = (v * 255.0).round().min(255.0) as u8;
                }
            });
            g = Glyph { x0: b.min.x as i32, top: -(b.min.y as i32), w, h, cov, adv };
        }
    }
    if r.bold && g.w > 0 {
        // the emboldening Windows does for fonts without a bold face: one pixel wider
        let w2 = g.w + 1;
        let mut cov = vec![0u8; (w2 * g.h) as usize];
        for y in 0..g.h {
            for x in 0..w2 {
                let a = if x < g.w { g.cov[(y * g.w + x) as usize] } else { 0 };
                let b = if x > 0 { g.cov[(y * g.w + x - 1) as usize] } else { 0 };
                cov[(y * w2 + x) as usize] = a.max(b);
            }
        }
        g.cov = cov;
        g.w = w2;
        g.adv += 1;
    }
    g
}

fn dc_font(t: &Objs, s: &DcState) -> LogFont {
    match t.m.get(&s.font) {
        Some(G::Font(f)) => f.clone(),
        _ => stock_font(SYS_FACE, -12),
    }
}

fn real_of(dc: u32) -> Option<(Real, LogFont, DcState)> {
    let (lf, s) = with(|t| {
        let Some(G::Dc(d)) = t.m.get(&dc) else { return None };
        Some((dc_font(t, &d.s), d.s))
    })?;
    Some((realize(&lf)?, lf, s))
}

fn read_logfont(p: u32, wide: bool) -> LogFont {
    let face = if wide {
        let v: Vec<u16> = (0..32).map(|i| r16(p + 28 + 2 * i)).take_while(|&c| c != 0).collect();
        String::from_utf16_lossy(&v)
    } else {
        a2s(&mem::cstrn(p + 28, 32))
    };
    LogFont {
        height: r32(p) as i32,
        width: r32(p + 4) as i32,
        esc: r32(p + 8) as i32,
        orient: r32(p + 12) as i32,
        weight: r32(p + 16) as i32,
        italic: r8(p + 20),
        underline: r8(p + 21),
        strike: r8(p + 22),
        charset: r8(p + 23),
        out_prec: r8(p + 24),
        clip_prec: r8(p + 25),
        quality: r8(p + 26),
        pitch: r8(p + 27),
        face,
    }
}

fn write_logfont(p: u32, lf: &LogFont, wide: bool) {
    for (i, v) in [lf.height, lf.width, lf.esc, lf.orient, lf.weight].iter().enumerate() {
        w32(p + 4 * i as u32, *v as u32);
    }
    for (i, v) in [lf.italic, lf.underline, lf.strike, lf.charset, lf.out_prec, lf.clip_prec, lf.quality, lf.pitch].iter().enumerate() {
        w8(p + 20 + i as u32, *v);
    }
    if wide {
        let u: Vec<u16> = lf.face.encode_utf16().collect();
        mem::put_wstr(p + 28, 32, &u);
    } else {
        mem::put_cstr(p + 28, 32, &crate::text::s2a(&lf.face));
    }
}

pub fn default_logfont(p: u32, wide: bool) {
    let lf = stock_font(SYS_FACE, -12);
    mem::fill(p, if wide { 92 } else { 60 }, 0);
    write_logfont(p, &lf, wide);
}

/// TEXTMETRIC (A: 56 bytes, W: 60).
fn write_tm(p: u32, r: &Real, lf: &LogFont, wide: bool) {
    let f = FACES.get(r.face);
    let height = r.ascent + r.descent;
    let full = r.em.round() as i32;
    let ave = (full + 1) / 2;
    let vals = [height, r.ascent, r.descent, (height - full).max(0), 0, ave, full, if r.bold { 700 } else { 400 }, 0, 96, 96];
    for (i, v) in vals.iter().enumerate() {
        w32(p + 4 * i as u32, *v as u32);
    }
    let fixed = f.is_some_and(|f| f.fixed);
    let family = if f.is_some_and(|f| f.roman) { 0x10 } else { 0x30 };
    let pitch = family | 6 | (!fixed) as u8;
    if wide {
        w16(p + 44, 0x20);
        w16(p + 46, 0xfffc);
        w16(p + 48, 0x1f);
        w16(p + 50, 0x20);
        for (i, v) in [lf.italic, lf.underline, lf.strike, pitch, 128].iter().enumerate() {
            w8(p + 52 + i as u32, *v);
        }
    } else {
        for (i, v) in [0x20u8, 0xfc, 0x1f, 0x20, lf.italic, lf.underline, lf.strike, pitch, 128].iter().enumerate() {
            w8(p + 44 + i as u32, *v);
        }
    }
}

fn text_width(r: &Real, s: &[u16], extra: i32) -> i32 {
    s.iter().map(|&c| glyph(r, c as u32).adv + extra).sum()
}

/// Draws text with the DC's font: `x`,`y` logical, `dx` explicit advances.
fn text_out(dc: u32, x: i32, y: i32, opts: u32, rect: Option<Rect>, s: &[u16], dx: Option<Vec<i32>>) -> bool {
    let Some((r, lf, st)) = real_of(dc) else { return false };
    let glyphs: Vec<Arc<Glyph>> = s.iter().map(|&c| glyph(&r, c as u32)).collect();
    let advs: Vec<i32> = match &dx {
        Some(d) => d.clone(),
        None => glyphs.iter().map(|g| g.adv + st.char_extra).collect(),
    };
    let total: i32 = advs.iter().sum();
    let (mut x, mut y) = (x, y);
    if st.align & 1 != 0 {
        x = st.pos.0;
        y = st.pos.1;
    }
    let x0 = match st.align & 6 {
        2 => x - total,
        6 => x - total / 2,
        _ => x,
    };
    let base = match st.align & 24 {
        8 => y - r.descent,
        24 => y,
        _ => y + r.ascent,
    };
    if st.align & 1 != 0 {
        with_dc(dc, |d| d.s.pos.0 = x0 + total);
    }
    let text = rgb(st.text);
    let bk = rgb(st.bk);
    let underline = lf.underline != 0;
    draw(dc, |_, s2, c| {
        let mut dirty = Rect::default();
        let saved = c.clip;
        if let Some(rc) = rect {
            let rd = c.rect(rc);
            if opts & 2 != 0 {
                dirty = dirty.or(&pat_blt(c, rd, &Pat::Solid(bk), 0xf0));
            }
            if opts & 4 != 0 {
                c.clip = c.clip.and(&rd);
            }
        }
        if s2.bk_mode == 2 && opts & 2 == 0 {
            let cell = Rect { l: x0, t: base - r.ascent, r: x0 + total, b: base + r.descent };
            dirty = dirty.or(&pat_blt(c, c.rect(cell), &Pat::Solid(bk), 0xf0));
        }
        let mut pen = x0;
        for (g, adv) in glyphs.iter().zip(&advs) {
            let (gx, gy) = c.dev(pen + g.x0, base - g.top);
            let gr = Rect { l: gx, t: gy, r: gx + g.w, b: gy + g.h }.and(&c.clip);
            for yy in gr.t..gr.b {
                for xx in gr.l..gr.r {
                    let a = g.cov[((yy - gy) * g.w + (xx - gx)) as usize] as u32;
                    if a == 0 {
                        continue;
                    }
                    let col = if a >= 255 { text } else { blend(c.v.get(xx, yy), text, a) };
                    c.v.put(xx, yy, col);
                }
            }
            dirty = dirty.or(&gr);
            pen += adv;
        }
        if underline {
            let (ux, uy) = c.dev(x0, base + 1);
            dirty = dirty.or(&pat_blt(c, Rect { l: ux, t: uy, r: ux + total, b: uy + 1 }, &Pat::Solid(text), 0xf0));
        }
        c.clip = saved;
        dirty
    })
}

fn blend(d: u32, s: u32, a: u32) -> u32 {
    let ch = |sh: u32| (((s >> sh & 255) * a + (d >> sh & 255) * (255 - a) + 127) / 255) << sh;
    ch(16) | ch(8) | ch(0)
}

/// DrawText (user32): returns the text height.
pub fn draw_text(dc: u32, s: &[u16], rp: u32, flags: u32) -> u32 {
    let Some((r, _, st)) = real_of(dc) else { return 0 };
    let mut rc = Rect::read(rp);
    let lh = r.ascent + r.descent;
    let prefix = flags & 0x800 == 0;
    let mut chars: Vec<u16> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if prefix && s[i] == b'&' as u16 && i + 1 < s.len() {
            i += 1;
        }
        chars.push(s[i]);
        i += 1;
    }
    let maxw = rc.w();
    let mut lines: Vec<Vec<u16>> = Vec::new();
    if flags & 0x20 != 0 {
        lines.push(chars.iter().copied().filter(|&c| c != 10 && c != 13).collect());
    } else {
        for l in chars.split(|&c| c == 10) {
            let l: Vec<u16> = l.iter().copied().filter(|&c| c != 13).collect();
            if flags & 0x10 != 0 && maxw > 0 {
                let mut cur = Vec::new();
                let mut w = 0;
                for ch in l {
                    let a = glyph(&r, ch as u32).adv + st.char_extra;
                    if w + a > maxw && !cur.is_empty() {
                        lines.push(std::mem::take(&mut cur));
                        w = 0;
                    }
                    cur.push(ch);
                    w += a;
                }
                lines.push(cur);
            } else {
                lines.push(l);
            }
        }
    }
    let height = lh * lines.len() as i32;
    if flags & 0x400 != 0 {
        let w = lines.iter().map(|l| text_width(&r, l, st.char_extra)).max().unwrap_or(0);
        rc.r = rc.l + w;
        rc.b = rc.t + height;
        rc.write(rp);
        return height as u32;
    }
    let mut y = rc.t;
    if flags & 0x20 != 0 {
        if flags & 4 != 0 {
            y = rc.t + (rc.h() - lh) / 2;
        } else if flags & 8 != 0 {
            y = rc.b - lh;
        }
    }
    let old_align = st.align;
    with_dc(dc, |d| d.s.align = 0);
    for l in &lines {
        let w = text_width(&r, l, st.char_extra);
        let x = if flags & 1 != 0 {
            rc.l + (rc.w() - w) / 2
        } else if flags & 2 != 0 {
            rc.r - w
        } else {
            rc.l
        };
        let clip = if flags & 0x100 != 0 { None } else { Some(rc) };
        text_out(dc, x, y, if clip.is_some() { 4 } else { 0 }, clip, l, None);
        y += lh;
    }
    with_dc(dc, |d| d.s.align = old_align);
    height as u32
}

// ---------------------------------------------------------------------------------------------
// bitmaps from resources

pub fn load_bitmap(inst: u32, name: u32, wide: bool) -> u32 {
    let module = if inst == 0 { 0 } else { inst };
    let ent = if module == 0 {
        None
    } else {
        crate::pe::find_resource(module, &crate::pe::ResId::Int(2), &crate::pe::ResId::from_arg(name, wide))
    };
    let Some(e) = ent else {
        return add(G::Bitmap(new_bitmap_raw(1, 1, 1, 0)));
    };
    let (data, _) = crate::pe::resource_data(module, e);
    let Some(info) = dib_info(data, 0, &default_palette()) else { return 0 };
    let hdr = r32(data);
    let ncol = if info.bpp <= 8 { info.pal.len() as u32 } else if info.compression == 3 { 3 } else { 0 };
    let bits = data + hdr + ncol * if hdr == 12 { 3 } else { 4 };
    let src = dib_view(&info, bits);
    let bpp = if info.bpp == 1 { 1 } else { 32 };
    let b = new_bitmap_raw(info.w, info.h, bpp, 0);
    let dst = View::of(&b);
    for y in 0..info.h {
        for x in 0..info.w {
            dst.put(x, y, src.get(x, y) & 0xffffff);
        }
    }
    add(G::Bitmap(b))
}

// ---------------------------------------------------------------------------------------------
// the API

fn dc_of(c: &Ctx, i: u32) -> u32 {
    c.a(i)
}

fn GetStockObject(c: &mut Ctx) -> u32 {
    let i = c.a(0);
    if i < STOCK_N {
        STOCK + 4 * i
    } else {
        0
    }
}

fn CreateCompatibleDC(_: &mut Ctx) -> u32 {
    new_dc(Kind::Mem, None)
}
fn CreateDCA(_: &mut Ctx) -> u32 {
    screen_dc()
}
fn CreateDCW(_: &mut Ctx) -> u32 {
    screen_dc()
}
fn CreateICA(_: &mut Ctx) -> u32 {
    screen_dc()
}
fn CreateICW(_: &mut Ctx) -> u32 {
    screen_dc()
}
fn DeleteDC(c: &mut Ctx) -> u32 {
    with(|t| matches!(t.m.remove(&c.a(0)), Some(G::Dc(_)))) as u32
}

fn GetObjectType(c: &mut Ctx) -> u32 {
    with(|t| match t.m.get(&c.a(0)) {
        Some(G::Pen(_)) => 1,
        Some(G::Brush(_)) => 2,
        Some(G::Dc(d)) => {
            if d.kind == Kind::Mem {
                10
            } else {
                3
            }
        }
        Some(G::Palette(_)) => 5,
        Some(G::Font(_)) => 6,
        Some(G::Bitmap(_)) => 7,
        Some(G::Region(_)) => 8,
        None => 0,
    })
}

fn SelectObject(c: &mut Ctx) -> u32 {
    let (dc, h) = (c.a(0), c.a(1));
    let region = with(|t| matches!(t.m.get(&h), Some(G::Region(_))));
    if region {
        return select_clip(dc, h, 5);
    }
    with(|t| {
        let kind = match t.m.get(&h) {
            Some(G::Bitmap(_)) => 0,
            Some(G::Brush(_)) => 1,
            Some(G::Pen(_)) => 2,
            Some(G::Font(_)) => 3,
            _ => return 0,
        };
        let Some(G::Dc(d)) = t.m.get_mut(&dc) else { return 0 };
        let slot = match kind {
            0 => {
                if d.kind != Kind::Mem {
                    return 0;
                }
                &mut d.s.bitmap
            }
            1 => &mut d.s.brush,
            2 => &mut d.s.pen,
            _ => &mut d.s.font,
        };
        std::mem::replace(slot, h)
    })
}

fn DeleteObject(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if is_stock(h) {
        return 1;
    }
    with(|t| {
        match t.m.get(&h) {
            Some(G::Dc(_)) | None => return 0,
            _ => {}
        }
        if t.solid.values().any(|&v| v == h) {
            return 1;
        }
        if let Some(G::Bitmap(b)) = t.m.remove(&h) {
            free_bitmap(&b);
        }
        1
    })
}

fn GetObjectA(c: &mut Ctx) -> u32 {
    get_object(c, false)
}
fn GetObjectW(c: &mut Ctx) -> u32 {
    get_object(c, true)
}
fn get_object(c: &mut Ctx, wide: bool) -> u32 {
    let (h, n, p) = (c.a(0), c.a(1), c.a(2));
    with(|t| match t.m.get(&h) {
        Some(G::Bitmap(b)) => {
            let need = if b.dib && n >= 84 { 84 } else { 24 };
            if p == 0 {
                return need;
            }
            let mut buf = [0u8; 84];
            let put = |buf: &mut [u8], o: usize, v: u32| buf[o..o + 4].copy_from_slice(&v.to_le_bytes());
            put(&mut buf, 4, b.w as u32);
            put(&mut buf, 8, b.h as u32);
            put(&mut buf, 12, if b.dib { b.stride } else { b.stride });
            buf[16..18].copy_from_slice(&1u16.to_le_bytes());
            buf[18..20].copy_from_slice(&b.bpp.to_le_bytes());
            put(&mut buf, 20, if b.dib { b.bits } else { 0 });
            if need == 84 {
                put(&mut buf, 24, 40);
                put(&mut buf, 28, b.w as u32);
                put(&mut buf, 32, if b.bottom_up { b.h as u32 } else { (-b.h) as u32 });
                buf[36..38].copy_from_slice(&1u16.to_le_bytes());
                buf[38..40].copy_from_slice(&b.bpp.to_le_bytes());
                put(&mut buf, 44, b.stride * b.h as u32);
                put(&mut buf, 56, if b.bpp <= 8 { b.pal.len() as u32 } else { 0 });
                if b.bpp == 16 {
                    put(&mut buf, 40, 3);
                    let m = if b.r565 { [0xf800, 0x7e0, 0x1f] } else { [0x7c00, 0x3e0, 0x1f] };
                    for (i, v) in m.iter().enumerate() {
                        put(&mut buf, 64 + 4 * i, *v);
                    }
                }
            }
            let k = need.min(n);
            mem::write(p, &buf[..k as usize]);
            k
        }
        Some(G::Pen(pn)) => {
            if p != 0 && n >= 16 {
                w32(p, pn.style);
                w32(p + 4, pn.width as u32);
                w32(p + 8, 0);
                w32(p + 12, pn.color);
            }
            16
        }
        Some(G::Brush(b)) => {
            if p != 0 && n >= 12 {
                w32(p, b.style);
                w32(p + 4, b.color);
                w32(p + 8, b.hatch);
            }
            12
        }
        Some(G::Font(f)) => {
            let need = if wide { 92 } else { 60 };
            if p != 0 && n >= need {
                write_logfont(p, f, wide);
            }
            need
        }
        Some(G::Palette(pl)) => {
            if p != 0 && n >= 2 {
                w16(p, pl.len() as u16);
            }
            2
        }
        _ => 0,
    })
}

fn GetCurrentObject(c: &mut Ctx) -> u32 {
    let (dc, ty) = (c.a(0), c.a(1));
    dc_state(dc).map_or(0, |s| match ty {
        1 => s.pen,
        2 => s.brush,
        5 => s.palette,
        6 => s.font,
        7 => s.bitmap,
        _ => 0,
    })
}

// --- brushes, pens

fn CreateSolidBrush(c: &mut Ctx) -> u32 {
    add(G::Brush(Brush { style: 0, color: c.a(0), hatch: 0, pattern: 0 }))
}
fn CreateBrushIndirect(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let (style, color, hatch) = (r32(p), r32(p + 4), r32(p + 8));
    add(G::Brush(Brush { style, color, hatch, pattern: if style == 3 { hatch } else { 0 } }))
}
fn CreateHatchBrush(c: &mut Ctx) -> u32 {
    add(G::Brush(Brush { style: 0, color: c.a(1), hatch: c.a(0), pattern: 0 }))
}
fn CreatePatternBrush(c: &mut Ctx) -> u32 {
    add(G::Brush(Brush { style: 3, color: 0, hatch: 0, pattern: c.a(0) }))
}
fn CreatePen(c: &mut Ctx) -> u32 {
    add(G::Pen(Pen { style: c.a(0), width: c.i(1).max(1), color: c.a(2) }))
}
fn CreatePenIndirect(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    add(G::Pen(Pen { style: r32(p), width: (r32(p + 4) as i32).max(1), color: r32(p + 12) }))
}
fn ExtCreatePen(c: &mut Ctx) -> u32 {
    let lb = c.a(2);
    add(G::Pen(Pen { style: c.a(0) & 0xf, width: c.i(1).max(1), color: if lb != 0 { r32(lb + 4) } else { 0 } }))
}
fn SetDCBrushColor(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    with_dc(c.a(0), |d| std::mem::replace(&mut d.s.dc_brush, v)).unwrap_or(0xffff_ffff)
}
fn SetDCPenColor(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    with_dc(c.a(0), |d| std::mem::replace(&mut d.s.dc_pen, v)).unwrap_or(0xffff_ffff)
}
fn UnrealizeObject(_: &mut Ctx) -> u32 {
    1
}

// --- DC attributes

macro_rules! setter {
    ($name:ident, $field:ident) => {
        fn $name(c: &mut Ctx) -> u32 {
            let v = c.a(1);
            with_dc(c.a(0), |d| std::mem::replace(&mut d.s.$field, v)).unwrap_or(0)
        }
    };
}
setter!(SetTextColor, text);
setter!(SetBkColor, bk);
setter!(SetBkMode, bk_mode);
setter!(SetROP2, rop2);
setter!(SetStretchBltMode, stretch);
setter!(SetMapMode, map_mode);
setter!(SetTextAlign, align);

macro_rules! getter {
    ($name:ident, $field:ident) => {
        fn $name(c: &mut Ctx) -> u32 {
            dc_state(c.a(0)).map_or(0, |s| s.$field)
        }
    };
}
getter!(GetTextColor, text);
getter!(GetBkColor, bk);
getter!(GetBkMode, bk_mode);
getter!(GetROP2, rop2);
getter!(GetStretchBltMode, stretch);
getter!(GetMapMode, map_mode);
getter!(GetTextAlign, align);

fn SetTextCharacterExtra(c: &mut Ctx) -> u32 {
    let v = c.i(1);
    with_dc(c.a(0), |d| std::mem::replace(&mut d.s.char_extra, v) as u32).unwrap_or(0x8000_0000)
}
fn GetTextCharacterExtra(c: &mut Ctx) -> u32 {
    dc_state(c.a(0)).map_or(0, |s| s.char_extra as u32)
}

fn put_pt(p: u32, v: (i32, i32)) {
    if p != 0 {
        w32(p, v.0 as u32);
        w32(p + 4, v.1 as u32);
    }
}

fn org_op(c: &mut Ctx, vp: bool, set: bool, offset: bool) -> u32 {
    let (dc, x, y, p) = (c.a(0), c.i(1), c.i(2), c.a(3));
    let old = with_dc(dc, |d| {
        let f = if vp { &mut d.s.vp } else { &mut d.s.win };
        let old = *f;
        if set {
            *f = if offset { (old.0 + x, old.1 + y) } else { (x, y) };
        }
        old
    });
    match old {
        Some(o) => {
            put_pt(if set { p } else { c.a(1) }, o);
            1
        }
        None => 0,
    }
}
fn SetViewportOrgEx(c: &mut Ctx) -> u32 {
    org_op(c, true, true, false)
}
fn SetWindowOrgEx(c: &mut Ctx) -> u32 {
    org_op(c, false, true, false)
}
fn OffsetViewportOrgEx(c: &mut Ctx) -> u32 {
    org_op(c, true, true, true)
}
fn OffsetWindowOrgEx(c: &mut Ctx) -> u32 {
    org_op(c, false, true, true)
}
fn GetViewportOrgEx(c: &mut Ctx) -> u32 {
    org_op(c, true, false, false)
}
fn GetWindowOrgEx(c: &mut Ctx) -> u32 {
    org_op(c, false, false, false)
}
fn SetBrushOrgEx(c: &mut Ctx) -> u32 {
    let (dc, x, y, p) = (c.a(0), c.i(1), c.i(2), c.a(3));
    with_dc(dc, |d| put_pt(p, std::mem::replace(&mut d.s.brush_org, (x, y)))).is_some() as u32
}
fn GetBrushOrgEx(c: &mut Ctx) -> u32 {
    dc_state(c.a(0)).map(|s| put_pt(c.a(1), s.brush_org)).is_some() as u32
}
fn GetCurrentPositionEx(c: &mut Ctx) -> u32 {
    dc_state(c.a(0)).map(|s| put_pt(c.a(1), s.pos)).is_some() as u32
}
fn MoveToEx(c: &mut Ctx) -> u32 {
    let (dc, x, y, p) = (c.a(0), c.i(1), c.i(2), c.a(3));
    with_dc(dc, |d| put_pt(p, std::mem::replace(&mut d.s.pos, (x, y)))).is_some() as u32
}
fn GetDCOrgEx(c: &mut Ctx) -> u32 {
    let h = dc_window(c.a(0));
    let r = if h != 0 { user::with(|s| user::client_screen(s, h)) } else { Rect::default() };
    put_pt(c.a(1), (r.l, r.t));
    1
}
fn SetGraphicsMode(_: &mut Ctx) -> u32 {
    1
}
fn GetGraphicsMode(_: &mut Ctx) -> u32 {
    1
}
fn SetWorldTransform(_: &mut Ctx) -> u32 {
    1
}
fn ModifyWorldTransform(_: &mut Ctx) -> u32 {
    1
}
fn SetLayout(_: &mut Ctx) -> u32 {
    0
}
fn GetLayout(_: &mut Ctx) -> u32 {
    0
}
fn SetViewportExtEx(_: &mut Ctx) -> u32 {
    1
}
fn SetWindowExtEx(_: &mut Ctx) -> u32 {
    1
}
fn GetViewportExtEx(c: &mut Ctx) -> u32 {
    put_pt(c.a(1), (1, 1));
    1
}
fn GetWindowExtEx(c: &mut Ctx) -> u32 {
    put_pt(c.a(1), (1, 1));
    1
}
fn LPtoDP(_: &mut Ctx) -> u32 {
    1
}
fn DPtoLP(_: &mut Ctx) -> u32 {
    1
}

fn SaveDC(c: &mut Ctx) -> u32 {
    with_dc(c.a(0), |d| {
        d.saved.push(d.s);
        d.saved.len() as u32
    })
    .unwrap_or(0)
}
fn RestoreDC(c: &mut Ctx) -> u32 {
    let n = c.i(1);
    with_dc(c.a(0), |d| {
        let k = if n < 0 { d.saved.len() as i32 + n } else { n - 1 };
        if k < 0 || k as usize >= d.saved.len() {
            return 0;
        }
        d.s = d.saved[k as usize];
        d.saved.truncate(k as usize);
        1
    })
    .unwrap_or(0)
}

fn GetDeviceCaps(c: &mut Ctx) -> u32 {
    let (w, h) = user::screen();
    let bpp = with(|t| {
        let Some(G::Dc(d)) = t.m.get(&c.a(0)) else { return 32 };
        if d.kind != Kind::Mem {
            return 32;
        }
        match t.m.get(&d.s.bitmap) {
            Some(G::Bitmap(b)) if b.bpp == 1 => 1,
            _ => 32,
        }
    });
    (match c.a(1) {
        0 => 0x400,
        2 => 1,
        4 => w * 254 / 960,
        6 => h * 254 / 960,
        8 => w,
        10 => h,
        12 => bpp,
        14 => 1,
        16 => 0,
        18 => 0,
        20 => 0,
        24 => -1,
        26 => 0,
        28 => 0,
        30 => 0,
        32 => 0,
        34 => 0,
        36 => 0x7fff,
        38 => 0x7e99,
        40 => 36,
        42 => 36,
        44 => 51,
        88 | 90 => 96,
        104 => 0,
        106 => 0,
        108 => 24,
        110 => w,
        111 => h,
        116 => 60,
        117 => h,
        118 => w,
        119 => 1,
        120 => 0,
        121 => 0,
        _ => 0,
    }) as u32
}

// --- bitmaps

fn CreateCompatibleBitmap(c: &mut Ctx) -> u32 {
    let (dc, w, h) = (c.a(0), c.i(1), c.i(2));
    if w <= 0 || h <= 0 {
        return DEFAULT_BITMAP;
    }
    let mono = with(|t| {
        let Some(G::Dc(d)) = t.m.get(&dc) else { return false };
        d.kind == Kind::Mem && matches!(t.m.get(&d.s.bitmap), Some(G::Bitmap(b)) if b.bpp == 1)
    });
    add(G::Bitmap(new_bitmap_raw(w, h, if mono { 1 } else { 32 }, 0)))
}
fn CreateDiscardableBitmap(c: &mut Ctx) -> u32 {
    CreateCompatibleBitmap(c)
}
fn CreateBitmap(c: &mut Ctx) -> u32 {
    let (w, h, planes, bpp, bits) = (c.i(0), c.i(1), c.a(2), c.a(3), c.a(4));
    if w <= 0 || h <= 0 {
        return DEFAULT_BITMAP;
    }
    let bpp = (planes * bpp) as u16;
    let bpp = match bpp {
        1 => 1,
        2..=4 => 4,
        5..=8 => 8,
        9..=16 => 16,
        17..=24 => 24,
        _ => 32,
    };
    let mut b = new_bitmap_raw(w, h, bpp, bits);
    if bpp == 8 {
        b.pal = default_palette();
        b.pal.resize(256, 0);
    }
    add(G::Bitmap(b))
}
fn CreateBitmapIndirect(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let (w, h, bpp, bits) = (r32(p + 4) as i32, r32(p + 8) as i32, r16(p + 16) * r16(p + 18).max(1), r32(p + 20));
    if w <= 0 || h <= 0 {
        return DEFAULT_BITMAP;
    }
    add(G::Bitmap(new_bitmap_raw(w, h, if bpp == 1 { 1 } else { 32 }, bits)))
}

fn CreateDIBSection(c: &mut Ctx) -> u32 {
    let (dc, bmi, usage, pbits, section, offset) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let pal = with(|t| match t.m.get(&dc) {
        Some(G::Dc(d)) => dc_palette(t, &d.s),
        _ => default_palette(),
    });
    let Some(i) = dib_info(bmi, usage, &pal) else {
        put32(pbits, 0);
        return 0;
    };
    let size = i.stride * i.h as u32;
    // (file mapping sections are not shared with the bits: the bits are fresh memory)
    let _ = (section, offset);
    let (bits, own) = (mem::alloc(size).unwrap_or(0), 2);
    if bits == 0 {
        return 0;
    }
    put32(pbits, bits);
    add(G::Bitmap(Bitmap {
        w: i.w,
        h: i.h,
        bpp: i.bpp,
        bits,
        stride: i.stride,
        bottom_up: !i.top_down,
        pal: i.pal,
        r565: i.r565,
        dib: true,
        own,
    }))
}

/// Copies between a bitmap and a DIB in guest memory: rows [start, start+lines) counted
/// from the DIB's bottom (bottom-up) or top.
fn dib_copy(b: &Bitmap, start: u32, lines: u32, bits: u32, i: &DibInfo, to_dib: bool) -> u32 {
    let bv = View::of(b);
    let dv = dib_view(i, bits);
    let lines = lines.min(i.h as u32 - start.min(i.h as u32)).min(b.h as u32);
    let w = i.w.min(b.w);
    for k in 0..lines as i32 {
        // row in top-down terms of both images
        let row = if i.top_down { start as i32 + k } else { b.h - 1 - (start as i32 + k) };
        let drow = if i.top_down { start as i32 + k } else { i.h - 1 - (start as i32 + k) };
        if row < 0 || row >= b.h || drow < 0 || drow >= i.h {
            continue;
        }
        if bv.bpp == dv.bpp && bv.bpp >= 16 && bv.r565 == dv.r565 {
            let n = (w as usize * bv.bpp as usize).div_ceil(8);
            unsafe {
                if to_dib {
                    std::ptr::copy(bv.row(row), dv.row(drow), n);
                } else {
                    std::ptr::copy(dv.row(drow), bv.row(row), n);
                }
            }
            continue;
        }
        for x in 0..w {
            if to_dib {
                dv.put(x, drow, bv.get(x, row) & if dv.bpp == 32 { 0xffff_ffff } else { 0xffffff });
            } else {
                bv.put(x, row, dv.get(x, drow) & if bv.bpp == 32 { 0xffff_ffff } else { 0xffffff });
            }
        }
    }
    lines
}


fn GetDIBits(c: &mut Ctx) -> u32 {
    let (dc, hbm, start, lines, bits, bmi, usage) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let _ = dc;
    let Some(b) = with(|t| match t.m.get(&hbm) {
        Some(G::Bitmap(b)) => Some(b.clone()),
        _ => None,
    }) else {
        return 0;
    };
    if r16(bmi + 14) == 0 {
        // fill in the header
        w32(bmi + 4, b.w as u32);
        w32(bmi + 8, b.h as u32);
        w16(bmi + 12, 1);
        w16(bmi + 14, b.bpp);
        w32(bmi + 16, if b.bpp == 16 { 3 } else { 0 });
        w32(bmi + 20, ((b.w as u32 * b.bpp as u32 + 31) / 32) * 4 * b.h as u32);
        w32(bmi + 32, 0);
        if b.bpp == 16 {
            let m = if b.r565 { [0xf800, 0x7e0, 0x1f] } else { [0x7c00, 0x3e0, 0x1f] };
            for (k, v) in m.iter().enumerate() {
                w32(bmi + 40 + 4 * k as u32, *v);
            }
        }
        return 1;
    }
    let bpp = r16(bmi + 14);
    let hdr = r32(bmi);
    // the color table the result uses
    let pal: Vec<u32> = if bpp <= 8 {
        if b.bpp == bpp && !b.pal.is_empty() {
            b.pal.clone()
        } else if bpp == 1 {
            vec![0, 0xffffff]
        } else if bpp == 8 {
            halftone_palette()
        } else {
            default_palette()[..16].to_vec()
        }
    } else {
        Vec::new()
    };
    if bpp <= 8 && usage == 0 {
        for (k, p) in pal.iter().enumerate() {
            let a = bmi + hdr + 4 * k as u32;
            w8(a, *p as u8);
            w8(a + 1, (*p >> 8) as u8);
            w8(a + 2, (*p >> 16) as u8);
            w8(a + 3, 0);
        }
    }
    let height = r32(bmi + 8) as i32;
    let stride = ((r32(bmi + 4) * bpp as u32 + 31) / 32) * 4;
    w32(bmi + 20, stride * height.unsigned_abs());
    if bits == 0 {
        return b.h as u32;
    }
    let Some(mut i) = dib_info(bmi, 0, &pal) else { return 0 };
    i.pal = pal;
    dib_copy(&b, start, lines, bits, &i, true)
}

fn SetDIBits(c: &mut Ctx) -> u32 {
    let (dc, hbm, start, lines, bits, bmi, usage) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let pal = with(|t| match t.m.get(&dc) {
        Some(G::Dc(d)) => dc_palette(t, &d.s),
        _ => default_palette(),
    });
    let Some(b) = with(|t| match t.m.get(&hbm) {
        Some(G::Bitmap(b)) => Some(b.clone()),
        _ => None,
    }) else {
        return 0;
    };
    let Some(i) = dib_info(bmi, usage, &pal) else { return 0 };
    dib_copy(&b, start, lines, bits, &i, false)
}

fn CreateDIBitmap(c: &mut Ctx) -> u32 {
    let (dc, bih, init, bits, bmi, usage) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let (w, h) = if r32(bih) == 12 { (r16(bih + 4) as i32, r16(bih + 6) as i16 as i32) } else { (r32(bih + 4) as i32, r32(bih + 8) as i32) };
    if w <= 0 || h == 0 {
        return 0;
    }
    let bpp = if r32(bih) == 12 { r16(bih + 10) } else { r16(bih + 14) };
    let mono = bpp == 1 && dc == 0;
    let b = new_bitmap_raw(w, h.abs(), if mono { 1 } else { 32 }, 0);
    if init & 4 != 0 && bits != 0 && bmi != 0 {
        let pal = with(|t| match t.m.get(&dc) {
            Some(G::Dc(d)) => dc_palette(t, &d.s),
            _ => default_palette(),
        });
        if let Some(i) = dib_info(bmi, usage, &pal) {
            dib_copy(&b, 0, i.h as u32, bits, &i, false);
        }
    }
    add(G::Bitmap(b))
}

fn GetBitmapBits(c: &mut Ctx) -> u32 {
    let (h, n, p) = (c.a(0), c.a(1), c.a(2));
    with(|t| match t.m.get(&h) {
        Some(G::Bitmap(b)) => {
            let size = b.stride * b.h as u32;
            if p == 0 {
                return size;
            }
            let k = size.min(n);
            mem::copy(p, b.bits, k);
            k
        }
        _ => 0,
    })
}
fn SetBitmapBits(c: &mut Ctx) -> u32 {
    let (h, n, p) = (c.a(0), c.a(1), c.a(2));
    with(|t| match t.m.get(&h) {
        Some(G::Bitmap(b)) => {
            let k = (b.stride * b.h as u32).min(n);
            mem::copy(b.bits, p, k);
            k
        }
        _ => 0,
    })
}
fn GetBitmapDimensionEx(c: &mut Ctx) -> u32 {
    put_pt(c.a(1), (0, 0));
    1
}

fn dib_color_table(c: &mut Ctx, set: bool) -> u32 {
    let (dc, start, n, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    with(|t| {
        let Some(G::Dc(d)) = t.m.get(&dc) else { return 0 };
        let bm = d.s.bitmap;
        let Some(G::Bitmap(b)) = t.m.get_mut(&bm) else { return 0 };
        let mut k = 0;
        for i in start..(start + n).min(b.pal.len() as u32) {
            let a = p + 4 * (i - start);
            if set {
                b.pal[i as usize] = (r8(a + 2) as u32) << 16 | (r8(a + 1) as u32) << 8 | r8(a) as u32;
            } else {
                let v = b.pal[i as usize];
                w32(a, v & 0xffffff);
            }
            k += 1;
        }
        k
    })
}
fn GetDIBColorTable(c: &mut Ctx) -> u32 {
    dib_color_table(c, false)
}
fn SetDIBColorTable(c: &mut Ctx) -> u32 {
    dib_color_table(c, true)
}

// --- blits

fn bit_blt(dst: u32, d: (i32, i32, i32, i32), src: u32, s: (i32, i32, i32, i32), rop: u32) -> u32 {
    let op = (rop >> 16) as u8;
    let has_src = uses_src(op) && src != 0;
    draw(dst, |t, ds, c| {
        let pat = pattern(t, ds, ds.brush);
        let dr = c.dev(d.0, d.1);
        let dst_r = (dr.0, dr.1, d.2, d.3);
        if !has_src {
            return blit(c, dst_r, None, op, &pat, 0, 0);
        }
        let Some((sv, sox, soy, ss)) = (if src == dst { None } else { source(t, src) }) else {
            // the same DC: blit from a copy of its own pixels
            let copy = copy_view(&c.v);
            let (sx, sy) = c.dev(s.0, s.1);
            let r = blit(c, dst_r, Some(Src { v: &copy.0, x: sx, y: sy, w: s.2, h: s.3, bk: rgb(ds.bk) }), op, &pat, rgb(ds.text), rgb(ds.bk));
            return r;
        };
        let src_ = Src { v: &sv, x: s.0 + sox, y: s.1 + soy, w: s.2, h: s.3, bk: rgb(ss.bk) };
        blit(c, dst_r, Some(src_), op, &pat, rgb(ds.text), rgb(ds.bk))
    }) as u32
}

/// A copy of a view's pixels (blits within one bitmap).
fn copy_view(v: &View) -> (View, Vec<u8>) {
    let n = (v.stride.unsigned_abs()) * v.h as usize;
    let mut buf = vec![0u8; n];
    for y in 0..v.h {
        unsafe { std::ptr::copy_nonoverlapping(v.row(y), buf.as_mut_ptr().add(y as usize * v.stride.unsigned_abs()), v.stride.unsigned_abs()) };
    }
    let nv = View { top: buf.as_mut_ptr(), stride: v.stride.abs(), ..v.clone() };
    (nv, buf)
}

fn BitBlt(c: &mut Ctx) -> u32 {
    let (w, h) = (c.i(3), c.i(4));
    bit_blt(c.a(0), (c.i(1), c.i(2), w, h), c.a(5), (c.i(6), c.i(7), w, h), c.a(8))
}
fn StretchBlt(c: &mut Ctx) -> u32 {
    bit_blt(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), c.a(5), (c.i(6), c.i(7), c.i(8), c.i(9)), c.a(10))
}
fn PatBlt(c: &mut Ctx) -> u32 {
    bit_blt(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), 0, (0, 0, 0, 0), c.a(5))
}
fn MaskBlt(c: &mut Ctx) -> u32 {
    let (w, h) = (c.i(3), c.i(4));
    bit_blt(c.a(0), (c.i(1), c.i(2), w, h), c.a(5), (c.i(6), c.i(7), w, h), c.a(11) & 0x00ff_ffff)
}
fn TransparentBlt(c: &mut Ctx) -> u32 {
    // (dst, x, y, w, h, src, x, y, w, h, transparent color)
    let (dst, src, key) = (c.a(0), c.a(5), rgb(c.a(10)));
    let d = (c.i(1), c.i(2), c.i(3), c.i(4));
    let s = (c.i(6), c.i(7), c.i(8), c.i(9));
    draw(dst, |t, _, cv| {
        let Some((sv, sox, soy, _)) = source(t, src) else { return Rect::default() };
        let (dx, dy) = cv.dev(d.0, d.1);
        let r = Rect { l: dx, t: dy, r: dx + d.2, b: dy + d.3 }.and(&cv.clip);
        for y in r.t..r.b {
            for x in r.l..r.r {
                let sx = s.0 + sox + (x - dx) * s.2 / d.2.max(1);
                let sy = s.1 + soy + (y - dy) * s.3 / d.3.max(1);
                if sx < 0 || sy < 0 || sx >= sv.w || sy >= sv.h {
                    continue;
                }
                let p = sv.get(sx, sy) & 0xffffff;
                if p != key {
                    cv.v.put(x, y, p);
                }
            }
        }
        r
    }) as u32
}
fn GdiTransparentBlt(c: &mut Ctx) -> u32 {
    TransparentBlt(c)
}
fn AlphaBlend(c: &mut Ctx) -> u32 {
    // (dst, x, y, w, h, src, x, y, w, h, BLENDFUNCTION)
    let (dst, src, bf) = (c.a(0), c.a(5), c.a(10));
    let d = (c.i(1), c.i(2), c.i(3), c.i(4));
    let s = (c.i(6), c.i(7), c.i(8), c.i(9));
    let (k, per_pixel) = (bf >> 16 & 255, bf >> 24 & 1 != 0);
    draw(dst, |t, _, cv| {
        let Some((sv, sox, soy, _)) = source(t, src) else { return Rect::default() };
        let (dx, dy) = cv.dev(d.0, d.1);
        let r = Rect { l: dx, t: dy, r: dx + d.2, b: dy + d.3 }.and(&cv.clip);
        for y in r.t..r.b {
            for x in r.l..r.r {
                let sx = s.0 + sox + (x - dx) * s.2 / d.2.max(1);
                let sy = s.1 + soy + (y - dy) * s.3 / d.3.max(1);
                if sx < 0 || sy < 0 || sx >= sv.w || sy >= sv.h {
                    continue;
                }
                let p = sv.get(sx, sy);
                let dp = cv.v.get(x, y);
                let ch = |sh: u32| {
                    let sc = (p >> sh & 255) * k / 255;
                    let a = if per_pixel { (p >> 24) * k / 255 } else { k };
                    ((sc + (dp >> sh & 255) * (255 - a) / 255).min(255)) << sh
                };
                cv.v.put(x, y, ch(24) | ch(16) | ch(8) | ch(0));
            }
        }
        r
    }) as u32
}
fn GdiAlphaBlend(c: &mut Ctx) -> u32 {
    AlphaBlend(c)
}

fn dib_to_device(dc: u32, d: (i32, i32, i32, i32), s: (i32, i32, i32, i32), bits: u32, bmi: u32, usage: u32, rop: u32, strip: Option<(u32, u32)>) -> u32 {
    let op = (rop >> 16) as u8;
    let pal = with(|t| match t.m.get(&dc) {
        Some(G::Dc(dd)) => dc_palette(t, &dd.s),
        _ => default_palette(),
    });
    let Some(mut i) = dib_info(bmi, usage, &pal) else { return 0 };
    if i.compression != 0 && i.compression != 3 {
        return 0;
    }
    let full_h = i.h;
    // the rows the DIB is a strip of (SetDIBitsToDevice)
    let (first, n) = strip.unwrap_or((0, full_h as u32));
    let n = n.min(full_h as u32) as i32;
    i.h = n;
    let v = dib_view(&i, bits);
    // source rect top in the strip's top-down rows
    let sy = if i.top_down { s.1 - first as i32 } else { first as i32 + n - (s.1 + s.3) };
    let ok = draw(dc, |t, ds, c| {
        let pat = pattern(t, ds, ds.brush);
        let (dx, dy) = c.dev(d.0, d.1);
        blit(c, (dx, dy, d.2, d.3), Some(Src { v: &v, x: s.0, y: sy, w: s.2, h: s.3, bk: rgb(ds.bk) }), op, &pat, rgb(ds.text), rgb(ds.bk))
    });
    if ok {
        n.max(1) as u32
    } else {
        0
    }
}

fn StretchDIBits(c: &mut Ctx) -> u32 {
    // (dc, xd, yd, wd, hd, xs, ys, ws, hs, bits, bmi, usage, rop)
    let mut hs = c.i(8);
    let ws = c.i(7);
    let (mut yd, mut hd) = (c.i(2), c.i(4));
    // a bottom-up source with negative heights mirrors the same way
    if hs < 0 && hd < 0 {
        hs = -hs;
        yd += hd;
        hd = -hd;
    }
    dib_to_device(c.a(0), (c.i(1), yd, c.i(3), hd), (c.i(5), c.i(6), ws, hs), c.a(9), c.a(10), c.a(11), c.a(12), None)
}
fn SetDIBitsToDevice(c: &mut Ctx) -> u32 {
    // (dc, xd, yd, w, h, xs, ys, start, lines, bits, bmi, usage)
    let (w, h) = (c.i(3), c.i(4));
    dib_to_device(c.a(0), (c.i(1), c.i(2), w, h), (c.i(5), c.i(6), w, h), c.a(9), c.a(10), c.a(11), 0xcc0020, Some((c.a(7), c.a(8))))
}

fn GetPixel(c: &mut Ctx) -> u32 {
    let (dc, x, y) = (c.a(0), c.i(1), c.i(2));
    let mut out = 0xffff_ffff;
    with(|t| {
        if let Some((v, ox, oy, _)) = source(t, dc) {
            let (x, y) = (x + ox, y + oy);
            if x >= 0 && y >= 0 && x < v.w && y < v.h {
                let p = v.get(x, y);
                out = (p & 0xff) << 16 | (p & 0xff00) | (p >> 16 & 0xff);
            }
        }
    });
    out
}
fn SetPixel(c: &mut Ctx) -> u32 {
    let (dc, x, y, col) = (c.a(0), c.i(1), c.i(2), c.a(3));
    draw(dc, |_, _, cv| {
        let (x, y) = cv.dev(x, y);
        if cv.clip.has(x, y) {
            cv.v.put(x, y, cref(col));
            return Rect { l: x, t: y, r: x + 1, b: y + 1 };
        }
        Rect::default()
    });
    col
}
fn SetPixelV(c: &mut Ctx) -> u32 {
    SetPixel(c);
    1
}

// --- lines and shapes

fn pen_of(t: &Objs, s: &DcState) -> Option<(u32, i32)> {
    if s.pen == STOCK + 4 * 19 {
        return Some((rgb(s.dc_pen), 1));
    }
    match t.m.get(&s.pen) {
        Some(G::Pen(p)) if p.style != 5 => Some((rgb(p.color), p.width)),
        _ => None,
    }
}

fn line(c: &mut Canvas, a: (i32, i32), b: (i32, i32), col: u32, rop2: u32, last: bool) -> Rect {
    let (mut x, mut y) = c.dev(a.0, a.1);
    let (x1, y1) = c.dev(b.0, b.1);
    let (dx, dy) = ((x1 - x).abs(), -(y1 - y).abs());
    let (sx, sy) = (if x < x1 { 1 } else { -1 }, if y < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    let mut r = Rect::default();
    loop {
        let end = x == x1 && y == y1;
        if end && !last {
            break;
        }
        if c.clip.has(x, y) {
            let d = c.v.get(x, y);
            let v = match rop2 {
                1 => 0,
                6 => !d,
                7 => d ^ col,
                16 => 0xffffff,
                _ => col,
            };
            c.v.put(x, y, v & 0xffffff);
            r = r.or(&Rect { l: x, t: y, r: x + 1, b: y + 1 });
        }
        if end {
            break;
        }
        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
    r
}

fn LineTo(c: &mut Ctx) -> u32 {
    let (dc, x, y) = (c.a(0), c.i(1), c.i(2));
    let Some(s) = dc_state(dc) else { return 0 };
    draw(dc, |t, s, cv| match pen_of(t, s) {
        Some((col, _)) => line(cv, s.pos, (x, y), col, s.rop2, false),
        None => Rect::default(),
    });
    let _ = s;
    with_dc(dc, |d| d.s.pos = (x, y));
    1
}
fn Polyline(c: &mut Ctx) -> u32 {
    let (dc, p, n) = (c.a(0), c.a(1), c.a(2));
    let pts: Vec<(i32, i32)> = (0..n).map(|i| (r32(p + 8 * i) as i32, r32(p + 8 * i + 4) as i32)).collect();
    draw(dc, |t, s, cv| {
        let Some((col, _)) = pen_of(t, s) else { return Rect::default() };
        let mut r = Rect::default();
        for w in pts.windows(2) {
            r = r.or(&line(cv, w[0], w[1], col, s.rop2, false));
        }
        r
    }) as u32
}
fn PolylineTo(c: &mut Ctx) -> u32 {
    let (dc, p, n) = (c.a(0), c.a(1), c.a(2));
    let Some(s) = dc_state(dc) else { return 0 };
    let mut pts = vec![s.pos];
    pts.extend((0..n).map(|i| (r32(p + 8 * i) as i32, r32(p + 8 * i + 4) as i32)));
    let last = *pts.last().unwrap();
    draw(dc, |t, s, cv| {
        let Some((col, _)) = pen_of(t, s) else { return Rect::default() };
        let mut r = Rect::default();
        for w in pts.windows(2) {
            r = r.or(&line(cv, w[0], w[1], col, s.rop2, false));
        }
        r
    });
    with_dc(dc, |d| d.s.pos = last);
    1
}
fn Rectangle(c: &mut Ctx) -> u32 {
    let (dc, l, tp, r, b) = (c.a(0), c.i(1), c.i(2), c.i(3), c.i(4));
    draw(dc, |t, s, cv| {
        let pen = pen_of(t, s);
        let pat = pattern(t, s, s.brush);
        let inset = pen.is_some() as i32;
        let mut d = Rect::default();
        if !matches!(pat, Pat::None) {
            d = pat_blt(cv, cv.rect(Rect { l: l + inset, t: tp + inset, r: r - inset, b: b - inset }), &pat, 0xf0);
        }
        if let Some((col, _)) = pen {
            let pc = Pat::Solid(col);
            for e in [Rect { l, t: tp, r, b: tp + 1 }, Rect { l, t: b - 1, r, b }, Rect { l, t: tp, r: l + 1, b }, Rect { l: r - 1, t: tp, r, b }] {
                d = d.or(&pat_blt(cv, cv.rect(e), &pc, 0xf0));
            }
        }
        d
    }) as u32
}
fn Ellipse(c: &mut Ctx) -> u32 {
    Rectangle(c)
}
fn RoundRect(c: &mut Ctx) -> u32 {
    Rectangle(c)
}
fn Polygon(c: &mut Ctx) -> u32 {
    let (dc, p, n) = (c.a(0), c.a(1), c.a(2));
    let mut pts: Vec<(i32, i32)> = (0..n).map(|i| (r32(p + 8 * i) as i32, r32(p + 8 * i + 4) as i32)).collect();
    if let Some(&f) = pts.first() {
        pts.push(f);
    }
    draw(dc, |t, s, cv| {
        let Some((col, _)) = pen_of(t, s) else { return Rect::default() };
        let mut r = Rect::default();
        for w in pts.windows(2) {
            r = r.or(&line(cv, w[0], w[1], col, s.rop2, false));
        }
        r
    }) as u32
}

// --- regions and clipping

fn region_kind(r: &[Rect]) -> u32 {
    match r.iter().filter(|x| !x.empty()).count() {
        0 => 1,
        1 => 2,
        _ => 3,
    }
}
fn bound(r: &[Rect]) -> Rect {
    r.iter().fold(Rect::default(), |a, b| a.or(b))
}
fn sub_rect(a: Rect, b: Rect) -> Vec<Rect> {
    let i = a.and(&b);
    if i.empty() {
        return vec![a];
    }
    let mut v = Vec::new();
    if a.t < i.t {
        v.push(Rect { b: i.t, ..a });
    }
    if i.b < a.b {
        v.push(Rect { t: i.b, ..a });
    }
    if a.l < i.l {
        v.push(Rect { l: a.l, t: i.t, r: i.l, b: i.b });
    }
    if i.r < a.r {
        v.push(Rect { l: i.r, t: i.t, r: a.r, b: i.b });
    }
    v
}
fn sub(a: &[Rect], b: &[Rect]) -> Vec<Rect> {
    let mut cur: Vec<Rect> = a.to_vec();
    for &x in b {
        cur = cur.into_iter().flat_map(|r| sub_rect(r, x)).collect();
    }
    cur
}

fn region(h: u32) -> Option<Vec<Rect>> {
    with(|t| match t.m.get(&h) {
        Some(G::Region(r)) => Some(r.clone()),
        _ => None,
    })
}
fn set_region(h: u32, r: Vec<Rect>) -> u32 {
    let k = region_kind(&r);
    with(|t| {
        if let Some(G::Region(x)) = t.m.get_mut(&h) {
            *x = r.into_iter().filter(|x| !x.empty()).collect();
        }
    });
    k
}

fn CreateRectRgn(c: &mut Ctx) -> u32 {
    add(G::Region(vec![Rect { l: c.i(0), t: c.i(1), r: c.i(2), b: c.i(3) }].into_iter().filter(|r| !r.empty()).collect()))
}
fn CreateRectRgnIndirect(c: &mut Ctx) -> u32 {
    add(G::Region(vec![Rect::read(c.a(0))].into_iter().filter(|r| !r.empty()).collect()))
}
fn CreateRoundRectRgn(c: &mut Ctx) -> u32 {
    CreateRectRgn(c)
}
fn CreateEllipticRgn(c: &mut Ctx) -> u32 {
    CreateRectRgn(c)
}
fn CreatePolygonRgn(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let mut r = Rect { l: i32::MAX, t: i32::MAX, r: i32::MIN, b: i32::MIN };
    for i in 0..n {
        let (x, y) = (r32(p + 8 * i) as i32, r32(p + 8 * i + 4) as i32);
        r = Rect { l: r.l.min(x), t: r.t.min(y), r: r.r.max(x), b: r.b.max(y) };
    }
    add(G::Region(if n > 0 { vec![r] } else { vec![] }))
}
fn ExtCreateRegion(c: &mut Ctx) -> u32 {
    let p = c.a(2);
    let n = r32(p + 8);
    let at = p + r32(p);
    add(G::Region((0..n).map(|i| Rect::read(at + 16 * i)).filter(|r| !r.empty()).collect()))
}
fn SetRectRgn(c: &mut Ctx) -> u32 {
    set_region(c.a(0), vec![Rect { l: c.i(1), t: c.i(2), r: c.i(3), b: c.i(4) }]);
    1
}
fn CombineRgn(c: &mut Ctx) -> u32 {
    let (d, a, b, mode) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(ra) = region(a) else { return 0 };
    let rb = region(b).unwrap_or_default();
    let out = match mode {
        1 => ra.iter().flat_map(|x| rb.iter().map(move |y| x.and(y))).filter(|r| !r.empty()).collect(),
        2 => {
            let mut v = ra.clone();
            v.extend(sub(&rb, &ra));
            v
        }
        3 => {
            let mut v = sub(&ra, &rb);
            v.extend(sub(&rb, &ra));
            v
        }
        4 => sub(&ra, &rb),
        _ => ra.clone(),
    };
    set_region(d, out)
}
fn OffsetRgn(c: &mut Ctx) -> u32 {
    let (h, x, y) = (c.a(0), c.i(1), c.i(2));
    let Some(r) = region(h) else { return 0 };
    set_region(h, r.iter().map(|q| q.off(x, y)).collect())
}
fn GetRgnBox(c: &mut Ctx) -> u32 {
    let Some(r) = region(c.a(0)) else { return 0 };
    bound(&r).write(c.a(1));
    region_kind(&r)
}
fn PtInRegion(c: &mut Ctx) -> u32 {
    region(c.a(0)).is_some_and(|r| r.iter().any(|q| q.has(c.i(1), c.i(2)))) as u32
}
fn RectInRegion(c: &mut Ctx) -> u32 {
    let q = Rect::read(c.a(1));
    region(c.a(0)).is_some_and(|r| r.iter().any(|x| !x.and(&q).empty())) as u32
}
fn EqualRgn(c: &mut Ctx) -> u32 {
    (region(c.a(0)).map(|r| bound(&r)) == region(c.a(1)).map(|r| bound(&r))) as u32
}
fn GetRegionData(c: &mut Ctx) -> u32 {
    let (h, n, p) = (c.a(0), c.a(1), c.a(2));
    let Some(r) = region(h) else { return 0 };
    let need = 32 + 16 * r.len() as u32;
    if p == 0 || n < need {
        return need;
    }
    w32(p, 32);
    w32(p + 4, 1);
    w32(p + 8, r.len() as u32);
    w32(p + 12, 16 * r.len() as u32);
    bound(&r).write(p + 16);
    for (i, q) in r.iter().enumerate() {
        q.write(p + 32 + 16 * i as u32);
    }
    need
}
fn FillRgn(c: &mut Ctx) -> u32 {
    let (dc, h, b) = (c.a(0), c.a(1), c.a(2));
    for r in region(h).unwrap_or_default() {
        fill_rect_brush(dc, r, b);
    }
    1
}
fn PaintRgn(c: &mut Ctx) -> u32 {
    let (dc, h) = (c.a(0), c.a(1));
    let b = dc_state(dc).map_or(STOCK, |s| s.brush);
    for r in region(h).unwrap_or_default() {
        fill_rect_brush(dc, r, b);
    }
    1
}
fn FrameRgn(_: &mut Ctx) -> u32 {
    1
}
fn InvertRgn(_: &mut Ctx) -> u32 {
    1
}

/// Clip box from region `h` (device coordinates), combined by `mode`.
fn select_clip(dc: u32, h: u32, mode: u32) -> u32 {
    let r = if h == 0 { None } else { region(h).map(|r| bound(&r)) };
    with_dc(dc, |d| {
        d.s.clip = match (mode, r, d.s.clip) {
            (5, r, _) => r,
            (_, None, _) if h == 0 => None,
            (1, Some(r), Some(c)) => Some(r.and(&c)),
            (1, Some(r), None) => Some(r),
            (2, Some(r), Some(c)) => Some(r.or(&c)),
            (2, _, None) => None,
            (_, _, c) => c,
        };
        match d.s.clip {
            Some(c) if c.empty() => 1,
            _ => 2,
        }
    })
    .unwrap_or(0)
}
fn SelectClipRgn(c: &mut Ctx) -> u32 {
    select_clip(c.a(0), c.a(1), 5)
}
fn ExtSelectClipRgn(c: &mut Ctx) -> u32 {
    select_clip(c.a(0), c.a(1), c.a(2))
}
fn IntersectClipRect(c: &mut Ctx) -> u32 {
    let dc = c.a(0);
    let r = Rect { l: c.i(1), t: c.i(2), r: c.i(3), b: c.i(4) };
    with_dc(dc, |d| {
        let rd = r.off(d.s.vp.0 - d.s.win.0, d.s.vp.1 - d.s.win.1);
        let n = match d.s.clip {
            Some(c) => c.and(&rd),
            None => rd,
        };
        d.s.clip = Some(n);
        if n.empty() {
            1
        } else {
            2
        }
    })
    .unwrap_or(0)
}
fn ExcludeClipRect(c: &mut Ctx) -> u32 {
    let dc = c.a(0);
    let r = Rect { l: c.i(1), t: c.i(2), r: c.i(3), b: c.i(4) };
    let size = with(|t| {
        let Some(G::Dc(d)) = t.m.get(&dc) else { return None };
        match target_of(t, d) {
            Tgt::Bmp(v) => Some(v.bounds()),
            Tgt::Win { clip, x, y, .. } => Some(clip.off(-x, -y)),
            Tgt::None => None,
        }
    });
    with_dc(dc, |d| {
        let rd = r.off(d.s.vp.0 - d.s.win.0, d.s.vp.1 - d.s.win.1);
        let cur = d.s.clip.or(size).unwrap_or_default();
        // only exclusions that cut a side off are kept exactly; others are ignored
        let pieces = sub_rect(cur, rd);
        if pieces.len() == 1 {
            d.s.clip = Some(pieces[0]);
        } else if pieces.is_empty() {
            d.s.clip = Some(Rect::default());
        }
        if pieces.is_empty() {
            1
        } else {
            2
        }
    })
    .unwrap_or(0)
}
fn OffsetClipRgn(c: &mut Ctx) -> u32 {
    let (dc, x, y) = (c.a(0), c.i(1), c.i(2));
    with_dc(dc, |d| {
        if let Some(cl) = d.s.clip {
            d.s.clip = Some(cl.off(x, y));
        }
        2
    })
    .unwrap_or(0)
}
fn GetClipBox(c: &mut Ctx) -> u32 {
    let (dc, p) = (c.a(0), c.a(1));
    let r = with(|t| {
        let Some(G::Dc(d)) = t.m.get(&dc) else { return None };
        let org = (d.s.vp.0 - d.s.win.0, d.s.vp.1 - d.s.win.1);
        let full = match target_of(t, d) {
            Tgt::Bmp(v) => v.bounds(),
            Tgt::Win { clip, x, y, .. } => clip.off(-x, -y),
            Tgt::None => {
                let (w, h) = user::screen();
                Rect { l: 0, t: 0, r: w, b: h }
            }
        };
        let r = match d.s.clip {
            Some(c) => c.and(&full),
            None => full,
        };
        Some(r.off(-org.0, -org.1))
    });
    match r {
        Some(r) => {
            r.write(p);
            if r.empty() {
                1
            } else {
                2
            }
        }
        None => 0,
    }
}
fn GetClipRgn(c: &mut Ctx) -> u32 {
    let (dc, h) = (c.a(0), c.a(1));
    match dc_state(dc).and_then(|s| s.clip) {
        Some(r) => {
            set_region(h, vec![r]);
            1
        }
        None => 0,
    }
}
fn RectVisible(c: &mut Ctx) -> u32 {
    let dc = c.a(0);
    let r = Rect::read(c.a(1));
    dc_state(dc).is_some_and(|s| s.clip.is_none_or(|cl| !cl.and(&r.off(s.vp.0 - s.win.0, s.vp.1 - s.win.1)).empty())) as u32
}
fn PtVisible(c: &mut Ctx) -> u32 {
    let (dc, x, y) = (c.a(0), c.i(1), c.i(2));
    dc_state(dc).is_some_and(|s| s.clip.is_none_or(|cl| cl.has(x + s.vp.0 - s.win.0, y + s.vp.1 - s.win.1))) as u32
}

// --- palettes

fn CreatePalette(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let n = r16(p + 2) as u32;
    add(G::Palette((0..n).map(|i| {
        let a = p + 4 + 4 * i;
        (r8(a) as u32) << 16 | (r8(a + 1) as u32) << 8 | r8(a + 2) as u32
    }).collect()))
}
fn CreateHalftonePalette(_: &mut Ctx) -> u32 {
    add(G::Palette(halftone_palette()))
}
fn SelectPalette(c: &mut Ctx) -> u32 {
    let (dc, h) = (c.a(0), c.a(1));
    with_dc(dc, |d| std::mem::replace(&mut d.s.palette, h)).unwrap_or(0)
}
fn RealizePalette(_: &mut Ctx) -> u32 {
    0
}
fn pal_entries(c: &mut Ctx, set: bool) -> u32 {
    let (h, start, n, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    with(|t| {
        let Some(G::Palette(pl)) = t.m.get_mut(&h) else { return 0 };
        if p == 0 && !set {
            return pl.len() as u32;
        }
        let mut k = 0;
        for i in start..(start + n).min(pl.len() as u32) {
            let a = p + 4 * (i - start);
            if set {
                pl[i as usize] = (r8(a) as u32) << 16 | (r8(a + 1) as u32) << 8 | r8(a + 2) as u32;
            } else {
                let v = pl[i as usize];
                w8(a, (v >> 16) as u8);
                w8(a + 1, (v >> 8) as u8);
                w8(a + 2, v as u8);
                w8(a + 3, 0);
            }
            k += 1;
        }
        k
    })
}
fn GetPaletteEntries(c: &mut Ctx) -> u32 {
    pal_entries(c, false)
}
fn SetPaletteEntries(c: &mut Ctx) -> u32 {
    pal_entries(c, true)
}
fn AnimatePalette(_: &mut Ctx) -> u32 {
    1
}
fn GetSystemPaletteEntries(c: &mut Ctx) -> u32 {
    let (start, n, p) = (c.a(1), c.a(2), c.a(3));
    if p == 0 {
        return 256;
    }
    let pal = halftone_palette();
    for i in start..(start + n).min(256) {
        let v = pal[i as usize];
        let a = p + 4 * (i - start);
        w8(a, (v >> 16) as u8);
        w8(a + 1, (v >> 8) as u8);
        w8(a + 2, v as u8);
        w8(a + 3, 0);
    }
    n
}
fn GetNearestPaletteIndex(_: &mut Ctx) -> u32 {
    0
}
fn GetNearestColor(c: &mut Ctx) -> u32 {
    c.a(1)
}
fn GetSystemPaletteUse(_: &mut Ctx) -> u32 {
    1
}
fn SetSystemPaletteUse(_: &mut Ctx) -> u32 {
    1
}

// --- fonts and text

fn CreateFontIndirectA(c: &mut Ctx) -> u32 {
    add(G::Font(read_logfont(c.a(0), false)))
}
fn CreateFontIndirectW(c: &mut Ctx) -> u32 {
    add(G::Font(read_logfont(c.a(0), true)))
}
fn CreateFontIndirectExW(c: &mut Ctx) -> u32 {
    add(G::Font(read_logfont(c.a(0), true)))
}
fn create_font(c: &mut Ctx, wide: bool) -> u32 {
    let f = c.a(13);
    let face = if f == 0 { String::new() } else { crate::k32::arg_str(f, wide) };
    add(G::Font(LogFont {
        height: c.i(0),
        width: c.i(1),
        esc: c.i(2),
        orient: c.i(3),
        weight: c.i(4),
        italic: c.a(5) as u8,
        underline: c.a(6) as u8,
        strike: c.a(7) as u8,
        charset: c.a(8) as u8,
        out_prec: c.a(9) as u8,
        clip_prec: c.a(10) as u8,
        quality: c.a(11) as u8,
        pitch: c.a(12) as u8,
        face,
    }))
}
fn CreateFontA(c: &mut Ctx) -> u32 {
    create_font(c, false)
}
fn CreateFontW(c: &mut Ctx) -> u32 {
    create_font(c, true)
}

fn GetTextMetricsA(c: &mut Ctx) -> u32 {
    let Some((r, lf, _)) = real_of(c.a(0)) else { return 0 };
    write_tm(c.a(1), &r, &lf, false);
    1
}
fn GetTextMetricsW(c: &mut Ctx) -> u32 {
    let Some((r, lf, _)) = real_of(c.a(0)) else { return 0 };
    write_tm(c.a(1), &r, &lf, true);
    1
}
fn GetTextFaceA(c: &mut Ctx) -> u32 {
    let (dc, n, p) = (c.a(0), c.a(1), c.a(2));
    let Some((r, _, _)) = real_of(dc) else { return 0 };
    let name = crate::text::s2a(FACES[r.face].names[0]);
    if p == 0 {
        return name.len() as u32 + 1;
    }
    mem::put_cstr(p, n, &name)
}
fn GetTextFaceW(c: &mut Ctx) -> u32 {
    let (dc, n, p) = (c.a(0), c.a(1), c.a(2));
    let Some((r, _, _)) = real_of(dc) else { return 0 };
    let name: Vec<u16> = FACES[r.face].names[0].encode_utf16().collect();
    if p == 0 {
        return name.len() as u32 + 1;
    }
    mem::put_wstr(p, n, &name)
}
fn GetTextCharset(_: &mut Ctx) -> u32 {
    128
}
fn GetTextCharsetInfo(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        mem::fill(c.a(1), 24, 0);
        w32(c.a(1), 1 << 17);
    }
    128
}

fn ansi_chars(p: u32, n: u32) -> Vec<u16> {
    a2w(&mem::cstrn(p, n))
}
fn wide_chars(p: u32, n: u32) -> Vec<u16> {
    (0..n).map(|i| r16(p + 2 * i)).collect()
}

fn extent(c: &mut Ctx, s: &[u16], out: u32) -> u32 {
    let Some((r, _, st)) = real_of(c.a(0)) else { return 0 };
    let w = text_width(&r, s, st.char_extra);
    put_pt(out, (w, r.ascent + r.descent));
    1
}
fn GetTextExtentPoint32A(c: &mut Ctx) -> u32 {
    let s = ansi_chars(c.a(1), c.a(2));
    extent(c, &s, c.a(3))
}
fn GetTextExtentPointA(c: &mut Ctx) -> u32 {
    GetTextExtentPoint32A(c)
}
fn GetTextExtentPoint32W(c: &mut Ctx) -> u32 {
    let s = wide_chars(c.a(1), c.a(2));
    extent(c, &s, c.a(3))
}
fn GetTextExtentPointW(c: &mut Ctx) -> u32 {
    GetTextExtentPoint32W(c)
}
fn ext_ex(c: &mut Ctx, s: &[u16]) -> u32 {
    // (dc, str, n, max, fit*, dx*, size*)
    let Some((r, _, st)) = real_of(c.a(0)) else { return 0 };
    let (max, fit, dx, size) = (c.i(3), c.a(4), c.a(5), c.a(6));
    let mut x = 0;
    let mut k = 0;
    for (i, &ch) in s.iter().enumerate() {
        x += glyph(&r, ch as u32).adv + st.char_extra;
        if fit == 0 || x <= max {
            k = i + 1;
        }
        if dx != 0 {
            w32(dx + 4 * i as u32, x as u32);
        }
    }
    put32(fit, k as u32);
    put_pt(size, (x, r.ascent + r.descent));
    1
}
fn GetTextExtentExPointA(c: &mut Ctx) -> u32 {
    let s = ansi_chars(c.a(1), c.a(2));
    ext_ex(c, &s)
}
fn GetTextExtentExPointW(c: &mut Ctx) -> u32 {
    let s = wide_chars(c.a(1), c.a(2));
    ext_ex(c, &s)
}
fn char_widths(c: &mut Ctx, wide: bool, abc: bool) -> u32 {
    let (dc, first, last, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((r, _, _)) = real_of(dc) else { return 0 };
    for (i, code) in (first..=last).enumerate() {
        let ch = if wide { code } else { mbcs_char(code) };
        let g = glyph(&r, ch);
        if abc {
            let a = p + 12 * i as u32;
            w32(a, g.x0 as u32);
            w32(a + 4, g.w.max(0) as u32);
            w32(a + 8, (g.adv - g.x0 - g.w) as u32);
        } else {
            w32(p + 4 * i as u32, g.adv as u32);
        }
    }
    1
}
fn GetCharWidthA(c: &mut Ctx) -> u32 {
    char_widths(c, false, false)
}
fn GetCharWidthW(c: &mut Ctx) -> u32 {
    char_widths(c, true, false)
}
fn GetCharWidth32A(c: &mut Ctx) -> u32 {
    char_widths(c, false, false)
}
fn GetCharWidth32W(c: &mut Ctx) -> u32 {
    char_widths(c, true, false)
}
fn GetCharABCWidthsA(c: &mut Ctx) -> u32 {
    char_widths(c, false, true)
}
fn GetCharABCWidthsW(c: &mut Ctx) -> u32 {
    char_widths(c, true, true)
}

/// A code from the A functions (one byte, or lead<<8|trail) as a UTF-16 unit.
fn mbcs_char(code: u32) -> u32 {
    let b: Vec<u8> = if code > 0xff { vec![(code >> 8) as u8, code as u8] } else { vec![code as u8] };
    mb2w(0, &b).first().copied().unwrap_or(b'?' as u16) as u32
}

fn glyph_outline(c: &mut Ctx, wide: bool) -> u32 {
    let (dc, code, fmt, gm, size, buf) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let Some((r, _, _)) = real_of(dc) else { return 0xffff_ffff };
    let ch = if fmt & 0x80 != 0 || wide { code } else { mbcs_char(code) };
    let g = glyph(&r, ch);
    let blank = g.w == 0 || g.h == 0;
    if gm != 0 {
        w32(gm, if blank { 1 } else { g.w as u32 });
        w32(gm + 4, if blank { 1 } else { g.h as u32 });
        w32(gm + 8, g.x0 as u32);
        w32(gm + 12, if blank { 0 } else { g.top as u32 });
        w16(gm + 16, g.adv as u16);
        w16(gm + 18, 0);
    }
    let f = fmt & 0x7f;
    let (stride, levels) = match f {
        0 => return 1,
        1 => ((g.w as u32).div_ceil(32) * 4, 1),
        4 => ((g.w as u32 + 3) & !3, 4),
        5 => ((g.w as u32 + 3) & !3, 16),
        6 => ((g.w as u32 + 3) & !3, 64),
        _ => return 0xffff_ffff,
    };
    if blank {
        return 0;
    }
    let need = stride * g.h as u32;
    if buf == 0 || size == 0 {
        return need;
    }
    if size < need {
        return 0xffff_ffff;
    }
    let out = mem::slice_mut(buf, need);
    out.fill(0);
    for y in 0..g.h {
        for x in 0..g.w {
            let v = g.cov[(y * g.w + x) as usize] as u32;
            let o = (y as u32 * stride) as usize;
            if f == 1 {
                if v >= 128 {
                    out[o + x as usize / 8] |= 0x80 >> (x & 7);
                }
            } else {
                out[o + x as usize] = ((v * levels + 127) / 255) as u8;
            }
        }
    }
    need
}
fn GetGlyphOutlineA(c: &mut Ctx) -> u32 {
    glyph_outline(c, false)
}
fn GetGlyphOutlineW(c: &mut Ctx) -> u32 {
    glyph_outline(c, true)
}
fn GetGlyphIndicesW(c: &mut Ctx) -> u32 {
    let (dc, s, n, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((r, _, _)) = real_of(dc) else { return 0xffff_ffff };
    for i in 0..n {
        let ch = char::from_u32(r16(s + 2 * i) as u32).unwrap_or('?');
        w16(out + 2 * i, r.font.glyph_id(ch).0);
    }
    n
}
fn GetFontData(_: &mut Ctx) -> u32 {
    0xffff_ffff
}
fn GetKerningPairsA(_: &mut Ctx) -> u32 {
    0
}
fn GetKerningPairsW(_: &mut Ctx) -> u32 {
    0
}
fn GetFontLanguageInfo(_: &mut Ctx) -> u32 {
    0
}
fn GetOutlineTextMetricsA(_: &mut Ctx) -> u32 {
    0
}
fn GetOutlineTextMetricsW(_: &mut Ctx) -> u32 {
    0
}
fn AddFontResourceA(_: &mut Ctx) -> u32 {
    1
}
fn AddFontResourceW(_: &mut Ctx) -> u32 {
    1
}
fn AddFontResourceExA(_: &mut Ctx) -> u32 {
    1
}
fn AddFontResourceExW(_: &mut Ctx) -> u32 {
    1
}
fn RemoveFontResourceA(_: &mut Ctx) -> u32 {
    1
}
fn RemoveFontResourceExA(_: &mut Ctx) -> u32 {
    1
}
fn AddFontMemResourceEx(c: &mut Ctx) -> u32 {
    put32(c.a(3), 1);
    0x0a7f_0000
}
fn RemoveFontMemResourceEx(_: &mut Ctx) -> u32 {
    1
}

fn TextOutA(c: &mut Ctx) -> u32 {
    let s = ansi_chars(c.a(3), c.a(4));
    text_out(c.a(0), c.i(1), c.i(2), 0, None, &s, None) as u32
}
fn TextOutW(c: &mut Ctx) -> u32 {
    let s = wide_chars(c.a(3), c.a(4));
    text_out(c.a(0), c.i(1), c.i(2), 0, None, &s, None) as u32
}
fn ext_text_out(c: &mut Ctx, wide: bool) -> u32 {
    let (dc, x, y, opts, rp, sp, n, dxp) = (c.a(0), c.i(1), c.i(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    let rect = (rp != 0).then(|| Rect::read(rp));
    let (s, units): (Vec<u16>, Vec<usize>) = if wide || opts & 0x10 != 0 {
        (wide_chars(sp, n), (0..n as usize).collect())
    } else {
        // dx has one entry per byte: map the bytes of DBCS characters to their character
        let bytes = mem::cstrn(sp, n);
        let mut s = Vec::new();
        let mut units = Vec::new();
        let mut i = 0;
        while i < bytes.len() {
            let k = if crate::text::is_lead(bytes[i]) && i + 1 < bytes.len() { 2 } else { 1 };
            s.extend(a2w(&bytes[i..i + k]));
            units.push(i);
            i += k;
        }
        (s, units)
    };
    let dx = (dxp != 0).then(|| {
        let raw: Vec<i32> = (0..n).map(|i| r32(dxp + 4 * i) as i32).collect();
        units
            .iter()
            .enumerate()
            .map(|(k, &u)| {
                let end = units.get(k + 1).copied().unwrap_or(n as usize);
                raw[u..end].iter().sum()
            })
            .collect()
    });
    if s.is_empty() {
        if let Some(r) = rect.filter(|_| opts & 2 != 0) {
            let bk = dc_state(dc).map_or(0xffffff, |s| s.bk);
            let b = solid_brush(bk);
            fill_rect_brush(dc, r, b);
        }
        return 1;
    }
    text_out(dc, x, y, opts, rect, &s, dx) as u32
}
fn ExtTextOutA(c: &mut Ctx) -> u32 {
    ext_text_out(c, false)
}
fn ExtTextOutW(c: &mut Ctx) -> u32 {
    ext_text_out(c, true)
}

/// Calls an EnumFonts-style callback for each face: (LOGFONT, TEXTMETRIC, type, lparam).
fn enum_fonts(c: &mut Ctx, filter: Option<String>, proc_: u32, lp: u32, wide: bool, ex: bool) -> u32 {
    let lfsize = if wide { 92 } else { 60 };
    let buf = mem::heap_alloc(1024, true);
    let tm = buf + 512;
    let mut ret = 1;
    let mut names: Vec<String> = Vec::new();
    for f in FACES {
        for n in f.names {
            names.push((*n).to_string());
        }
        if f.names[0].starts_with('Ｍ') {
            names.push(format!("@{}", f.names[0]));
        }
    }
    for name in names {
        if let Some(fl) = &filter {
            if !fl.is_empty() && !fl.eq_ignore_ascii_case(&name) {
                continue;
            }
        }
        let lf = LogFont { height: 16, weight: 400, pitch: 0, ..stock_font(&name, 16) };
        mem::fill(buf, 1024, 0);
        write_logfont(buf, &lf, wide);
        if ex {
            // full name and style after the LOGFONT
            if wide {
                let u: Vec<u16> = name.encode_utf16().collect();
                mem::put_wstr(buf + lfsize, 64, &u);
                mem::put_wstr(buf + lfsize + 128, 32, &"Regular".encode_utf16().collect::<Vec<u16>>());
            } else {
                mem::put_cstr(buf + lfsize, 64, &crate::text::s2a(&name));
                mem::put_cstr(buf + lfsize + 64, 32, b"Regular");
            }
        }
        if let Some(r) = realize(&lf) {
            write_tm(tm, &r, &lf, wide);
            let tmsize = if wide { 60 } else { 56 };
            w32(tm + tmsize + 4, 2048);
            w32(tm + tmsize + 8, (r.ascent + r.descent) as u32);
            w32(tm + tmsize + 12, (r.em / 2.0) as u32);
        }
        ret = c.call(proc_, &[buf, tm, 4, lp]);
        if ret == 0 {
            break;
        }
    }
    mem::heap_free(buf);
    ret
}
fn EnumFontsA(c: &mut Ctx) -> u32 {
    let (f, p, lp) = (c.a(1), c.a(2), c.a(3));
    let filter = (f != 0).then(|| a2s(&cstr(f)));
    enum_fonts(c, filter, p, lp, false, false)
}
fn EnumFontsW(c: &mut Ctx) -> u32 {
    let (f, p, lp) = (c.a(1), c.a(2), c.a(3));
    let filter = (f != 0).then(|| String::from_utf16_lossy(&wstr(f)));
    enum_fonts(c, filter, p, lp, true, false)
}
fn EnumFontFamiliesA(c: &mut Ctx) -> u32 {
    let (f, p, lp) = (c.a(1), c.a(2), c.a(3));
    let filter = (f != 0).then(|| a2s(&cstr(f)));
    enum_fonts(c, filter, p, lp, false, true)
}
fn EnumFontFamiliesW(c: &mut Ctx) -> u32 {
    let (f, p, lp) = (c.a(1), c.a(2), c.a(3));
    let filter = (f != 0).then(|| String::from_utf16_lossy(&wstr(f)));
    enum_fonts(c, filter, p, lp, true, true)
}
fn EnumFontFamiliesExA(c: &mut Ctx) -> u32 {
    let (lf, p, lp) = (c.a(1), c.a(2), c.a(3));
    let filter = Some(read_logfont(lf, false).face);
    enum_fonts(c, filter, p, lp, false, true)
}
fn EnumFontFamiliesExW(c: &mut Ctx) -> u32 {
    let (lf, p, lp) = (c.a(1), c.a(2), c.a(3));
    let filter = Some(read_logfont(lf, true).face);
    enum_fonts(c, filter, p, lp, true, true)
}

// --- msvfw32 DrawDib: a StretchDIBits

fn DrawDibOpen(_: &mut Ctx) -> u32 {
    0x0a7d_0000
}
fn DrawDibClose(_: &mut Ctx) -> u32 {
    1
}
fn DrawDibBegin(_: &mut Ctx) -> u32 {
    1
}
fn DrawDibEnd(_: &mut Ctx) -> u32 {
    1
}
fn DrawDibRealize(_: &mut Ctx) -> u32 {
    0
}
fn DrawDibSetPalette(_: &mut Ctx) -> u32 {
    1
}
fn DrawDibStart(_: &mut Ctx) -> u32 {
    1
}
fn DrawDibStop(_: &mut Ctx) -> u32 {
    1
}
fn DrawDibDraw(c: &mut Ctx) -> u32 {
    // (hdd, dc, xd, yd, wd, hd, bi, bits, xs, ys, ws, hs, flags)
    let (dc, bi, mut bits) = (c.a(1), c.a(6), c.a(7));
    if bi == 0 {
        return 1;
    }
    let (w, h) = (r32(bi + 4) as i32, (r32(bi + 8) as i32).abs());
    let (mut ws, mut hs) = (c.i(10), c.i(11));
    if ws < 0 {
        ws = w;
    }
    if hs < 0 {
        hs = h;
    }
    let (mut wd, mut hd) = (c.i(4), c.i(5));
    if wd < 0 {
        wd = ws;
    }
    if hd < 0 {
        hd = hs;
    }
    if bits == 0 {
        let n = r32(bi + 32);
        let bpp = r16(bi + 14) as u32;
        let ncol = if bpp <= 8 { if n != 0 { n } else { 1 << bpp } } else if r32(bi + 16) == 3 { 3 } else { 0 };
        bits = bi + r32(bi) + 4 * ncol;
    }
    // DrawDib source coordinates are top-down
    let ys = if (r32(bi + 8) as i32) > 0 { h - c.i(9) - hs } else { c.i(9) };
    dib_to_device(dc, (c.i(2), c.i(3), wd, hd), (c.i(8), ys, ws, hs), bits, bi, 0, 0xcc0020, None);
    1
}

fn GdiFlush(_: &mut Ctx) -> u32 {
    1
}
fn GdiSetBatchLimit(_: &mut Ctx) -> u32 {
    1
}
fn GetPixelFormat(_: &mut Ctx) -> u32 {
    0
}
fn SetICMMode(_: &mut Ctx) -> u32 {
    1
}
fn GetObjectTypeDummy(_: &mut Ctx) -> u32 {
    0
}

fn register() {
    let _ = (dc_of, GetObjectTypeDummy);
    reg!("gdi32";
        GetStockObject, CreateCompatibleDC, CreateDCA, CreateDCW, CreateICA, CreateICW, DeleteDC, GetObjectType,
        SelectObject, DeleteObject, GetObjectA, GetObjectW, GetCurrentObject,
        CreateSolidBrush, CreateBrushIndirect, CreateHatchBrush, CreatePatternBrush, CreatePen, CreatePenIndirect,
        ExtCreatePen, SetDCBrushColor, SetDCPenColor, UnrealizeObject,
        SetTextColor, SetBkColor, SetBkMode, SetROP2, SetStretchBltMode, SetMapMode, SetTextAlign,
        GetTextColor, GetBkColor, GetBkMode, GetROP2, GetStretchBltMode, GetMapMode, GetTextAlign,
        SetTextCharacterExtra, GetTextCharacterExtra,
        SetViewportOrgEx, SetWindowOrgEx, OffsetViewportOrgEx, OffsetWindowOrgEx, GetViewportOrgEx, GetWindowOrgEx,
        SetBrushOrgEx, GetBrushOrgEx, GetCurrentPositionEx, MoveToEx, GetDCOrgEx, SetGraphicsMode, GetGraphicsMode,
        SetWorldTransform, ModifyWorldTransform, SetLayout, GetLayout, SetViewportExtEx, SetWindowExtEx,
        GetViewportExtEx, GetWindowExtEx, LPtoDP, DPtoLP, SaveDC, RestoreDC, GetDeviceCaps,
        CreateCompatibleBitmap, CreateDiscardableBitmap, CreateBitmap, CreateBitmapIndirect, CreateDIBSection,
        GetDIBits, SetDIBits, CreateDIBitmap, GetBitmapBits, SetBitmapBits, GetBitmapDimensionEx,
        GetDIBColorTable, SetDIBColorTable,
        BitBlt, StretchBlt, PatBlt, MaskBlt, GdiTransparentBlt, GdiAlphaBlend, StretchDIBits, SetDIBitsToDevice,
        GetPixel, SetPixel, SetPixelV, LineTo, Polyline, PolylineTo, Rectangle, Ellipse, RoundRect, Polygon,
        CreateRectRgn, CreateRectRgnIndirect, CreateRoundRectRgn, CreateEllipticRgn, CreatePolygonRgn,
        ExtCreateRegion, SetRectRgn, CombineRgn, OffsetRgn, GetRgnBox, PtInRegion, RectInRegion, EqualRgn,
        GetRegionData, FillRgn, PaintRgn, FrameRgn, InvertRgn,
        SelectClipRgn, ExtSelectClipRgn, IntersectClipRect, ExcludeClipRect, OffsetClipRgn, GetClipBox, GetClipRgn,
        RectVisible, PtVisible,
        CreatePalette, CreateHalftonePalette, SelectPalette, RealizePalette, GetPaletteEntries, SetPaletteEntries,
        AnimatePalette, GetSystemPaletteEntries, GetNearestPaletteIndex, GetNearestColor, GetSystemPaletteUse,
        SetSystemPaletteUse,
        CreateFontIndirectA, CreateFontIndirectW, CreateFontIndirectExW, CreateFontA, CreateFontW,
        GetTextMetricsA, GetTextMetricsW, GetTextFaceA, GetTextFaceW, GetTextCharset, GetTextCharsetInfo,
        GetTextExtentPoint32A, GetTextExtentPointA, GetTextExtentPoint32W, GetTextExtentPointW,
        GetTextExtentExPointA, GetTextExtentExPointW, GetCharWidthA, GetCharWidthW, GetCharWidth32A,
        GetCharWidth32W, GetCharABCWidthsA, GetCharABCWidthsW, GetGlyphOutlineA, GetGlyphOutlineW,
        GetGlyphIndicesW, GetFontData, GetKerningPairsA, GetKerningPairsW, GetFontLanguageInfo,
        GetOutlineTextMetricsA, GetOutlineTextMetricsW, AddFontResourceA, AddFontResourceW, AddFontResourceExA,
        AddFontResourceExW, RemoveFontResourceA, RemoveFontResourceExA, AddFontMemResourceEx,
        RemoveFontMemResourceEx, TextOutA, TextOutW, ExtTextOutA, ExtTextOutW,
        EnumFontsA, EnumFontsW, EnumFontFamiliesA, EnumFontFamiliesW, EnumFontFamiliesExA, EnumFontFamiliesExW,
        GdiFlush, GdiSetBatchLimit, GetPixelFormat, SetICMMode,
    );
    reg!("msimg32"; TransparentBlt, AlphaBlend);
    reg!("msvfw32"; DrawDibOpen, DrawDibClose, DrawDibBegin, DrawDibEnd, DrawDibRealize, DrawDibSetPalette,
        DrawDibStart, DrawDibStop, DrawDibDraw);
}
