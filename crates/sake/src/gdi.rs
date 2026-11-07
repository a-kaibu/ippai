//! gdi32: DCs, bitmaps (DDBs and DIB sections in guest memory), brushes, pens, fonts,
//! palettes, regions, blits with raster ops, and text through TrueType fonts (ab_glyph).
//!
//! Every pixel operation runs on a `View`: rows of some bit depth at a raw address, either a
//! bitmap's bits in guest memory or a top-level window's surface (user.rs). One lock holds all
//! GDI objects; a draw op takes it, then the surface's lock, draws, and sends what changed to
//! the display after letting go of both.

#![allow(non_snake_case)]

use crate::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use ab_glyph::{Font as _, FontVec, PxScale, ScaleFont as _};

use crate::api::{implement, Ctx};
use crate::mem::{self, cstr, put32, r16, r32, r8, w16, w32, w8, wstr};
use crate::text::{a2s, a2w, mb2w};
use crate::gdi_geom;
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
    /// ALTERNATE 1, WINDING 2
    pub fill_mode: u32,
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
    f(g.get_or_insert_with(|| Objs { m: HashMap::default(), next: 0x0a00_0100, solid: HashMap::default() }))
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

/// A top-down bitmap over pixels someone else owns (ddraw surfaces).
pub fn bitmap_over(w: i32, h: i32, bpp: u16, bits: u32, stride: u32, r565: bool) -> u32 {
    let pal = if bpp <= 8 { halftone_palette() } else { Vec::new() };
    add(G::Bitmap(Bitmap { w, h, bpp, bits, stride, bottom_up: false, pal, r565, dib: true, own: 0 }))
}

/// Replaces a bitmap's color table (0xRRGGBB entries).
pub fn set_bitmap_palette(h: u32, pal: &[u32]) {
    with(|t| {
        if let Some(G::Bitmap(b)) = t.m.get_mut(&h) {
            b.pal = pal.to_vec();
        }
    });
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
    /// The pixel as stored (32/24 bpp: the color, 16: the 16-bit value, palettes: the index).
    /// Raster ops work on these, as in Windows (an indexed DIB's ops combine indices).
    #[inline]
    fn raw(&self, x: i32, y: i32) -> u32 {
        let r = self.row(y);
        unsafe {
            match self.bpp {
                32 => (r as *const u32).add(x as usize).read_unaligned(),
                24 => {
                    let p = r.add(3 * x as usize);
                    (*p.add(2) as u32) << 16 | (*p.add(1) as u32) << 8 | *p as u32
                }
                16 => (r as *const u16).add(x as usize).read_unaligned() as u32,
                8 => *r.add(x as usize) as u32,
                4 => (*r.add(x as usize / 2) >> if x & 1 == 0 { 4 } else { 0 } & 15) as u32,
                1 => (*r.add(x as usize / 8) >> (7 - (x & 7)) & 1) as u32,
                _ => 0,
            }
        }
    }
    #[inline]
    fn set_raw(&self, x: i32, y: i32, v: u32) {
        let r = self.row(y);
        unsafe {
            match self.bpp {
                32 => (r as *mut u32).add(x as usize).write_unaligned(v),
                24 => {
                    let p = r.add(3 * x as usize);
                    *p = v as u8;
                    *p.add(1) = (v >> 8) as u8;
                    *p.add(2) = (v >> 16) as u8;
                }
                16 => (r as *mut u16).add(x as usize).write_unaligned(v as u16),
                8 => *r.add(x as usize) = v as u8,
                4 => {
                    let p = r.add(x as usize / 2);
                    let i = (v & 15) as u8;
                    *p = if x & 1 == 0 { (*p & 0x0f) | i << 4 } else { (*p & 0xf0) | i };
                }
                1 => {
                    let p = r.add(x as usize / 8);
                    let m = 0x80u8 >> (x & 7);
                    *p = if v & 1 != 0 { *p | m } else { *p & !m };
                }
                _ => {}
            }
        }
    }
    /// A solid color (0xRRGGBB) as a stored pixel: palettes take the nearest entry.
    fn pixel(&self, c: u32) -> u32 {
        match self.bpp {
            32 | 24 => c & 0xffffff,
            16 if self.r565 => (c >> 19 & 31) << 11 | (c >> 10 & 63) << 5 | (c >> 3 & 31),
            16 => (c >> 19 & 31) << 10 | (c >> 11 & 31) << 5 | (c >> 3 & 31),
            _ => self.index_of(c),
        }
    }
    /// The COLORREF (0xBBGGRR) a stored pixel shows
    fn color_of(&self, px: u32) -> u32 {
        let c = match self.bpp {
            32 | 24 => px & 0xffffff,
            16 => {
                let x5 = |v: u32| v << 3 | v >> 2;
                let (r, g, b) = if self.r565 {
                    let g6 = px >> 5 & 63;
                    (px >> 11 & 31, g6 << 2 | g6 >> 4, px & 31)
                } else {
                    (px >> 10 & 31, x5(px >> 5 & 31), px & 31)
                };
                x5(r) << 16 | g << 8 | x5(b)
            }
            _ => self.pal_at(px as usize),
        };
        (c & 0xff) << 16 | (c & 0xff00) | (c >> 16 & 0xff)
    }
    /// The bits a stored pixel has
    fn raw_mask(&self) -> u32 {
        if self.bpp >= 32 {
            u32::MAX
        } else {
            (1u32 << self.bpp) - 1
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
        fill_mode: 1,
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

thread_local! {
    static WIN_SOURCES: std::cell::RefCell<Vec<Vec<u32>>> = const { std::cell::RefCell::new(Vec::new()) };
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
            let (w, h) = (g.w, g.h);
            // a copy (the blit may draw into the same surface), kept until this thread's
            // source after next: one blit reads two sources at most
            let v = WIN_SOURCES.with(|k| {
                let mut k = k.borrow_mut();
                let mut px = if k.len() >= 2 { k.remove(0) } else { Vec::new() };
                px.clear();
                px.extend_from_slice(&g.px);
                k.push(px);
                View::surface(k.last_mut().unwrap(), w, h)
            });
            drop(g);
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
        0xb8 => (s & d) | (!s & p),
        0xe2 => (s & p) | (!s & d),
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
    /// 8x8 bits (bit 7 = left), set bits fg, clear bits bg; origin
    Hatch([u8; 8], u32, u32, i32, i32),
    /// a monochrome bitmap: 0 bits the text color, 1 bits the background color
    Mono(View, u32, u32, i32, i32),
}

const HATCHES: [[u8; 8]; 6] = [
    [0, 0, 0, 0xff, 0, 0, 0, 0],
    [8; 8],
    [0x80, 0x40, 0x20, 0x10, 0x08, 0x04, 0x02, 0x01],
    [0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80],
    [8, 8, 8, 0xff, 8, 8, 8, 8],
    [0x81, 0x42, 0x24, 0x18, 0x18, 0x24, 0x42, 0x81],
];

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
            Pat::Hatch(b, fg, bg, ox, oy) => {
                let (u, w) = ((x - ox).rem_euclid(8), (y - oy).rem_euclid(8));
                if b[w as usize] >> (7 - u) & 1 != 0 {
                    *fg
                } else {
                    *bg
                }
            }
            Pat::Mono(v, fg, bg, ox, oy) => {
                if v.w <= 0 || v.h <= 0 {
                    return 0;
                }
                if v.get((x - ox).rem_euclid(v.w), (y - oy).rem_euclid(v.h)) & 0xffffff == 0 {
                    *fg
                } else {
                    *bg
                }
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
            2 => Pat::Hatch(HATCHES[b.hatch.min(5) as usize], rgb(b.color), rgb(s.bk), s.brush_org.0, s.brush_org.1),
            3 | 5 => match t.m.get(&b.pattern) {
                Some(G::Bitmap(bm)) if bm.bpp == 1 && !bm.dib => Pat::Mono(View::of(bm), rgb(s.text), rgb(s.bk), s.brush_org.0, s.brush_org.1),
                Some(G::Bitmap(bm)) => Pat::Tile(View::of(bm), s.brush_org.0, s.brush_org.1),
                _ => Pat::Solid(rgb(b.color)),
            },
            _ => Pat::Solid(rgb(b.color)),
        },
        _ => Pat::Solid(0xffffff),
    }
}

fn sys_color(i: u32) -> u32 {
    crate::user::sys_color(i)
}

/// Fills `r` (device coordinates) with a raster op of the pattern.
fn pat_blt(c: &mut Canvas, r: Rect, pat: &Pat, op: u8) -> Rect {
    let r = r.and(&c.clip);
    if r.empty() || matches!(pat, Pat::None) && uses_pat(op) {
        // (a raster op of the null brush draws nothing)
        return Rect::default();
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
    let mask = c.v.raw_mask();
    for y in r.t..r.b {
        for x in r.l..r.r {
            let p = c.v.pixel(pat.at(x, y));
            c.v.set_raw(x, y, rop3(op, p, 0, c.v.raw(x, y)) & mask);
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
    /// HALFTONE stretch mode: Windows picks floor(i * sw / dw), not the pixel centre
    half: bool,
}

/// The blit at the heart of BitBlt/StretchBlt/StretchDIBits: dst rect (device coordinates,
/// w/h may be negative), source, raster op. Nearest-neighbor stretching.
fn blit(c: &mut Canvas, dst: (i32, i32, i32, i32), src: Option<Src>, op: u8, pat: &Pat, text: u32, bk: u32) -> Rect {
    // (a negative extent covers x+w+1 ..= x, as in Windows)
    let (mut dx, mut dy, mut dw, mut dh) = dst;
    let mut flip = (false, false);
    // a mirrored stretch covers x+w+1..=x (pixel x is drawn), PatBlt covers x+w..x-1
    let one = (src.is_some() && uses_src(op)) as i32;
    if dw < 0 {
        dx += dw + one;
        dw = -dw;
        flip.0 = true;
    }
    if dh < 0 {
        dy += dh + one;
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
        sx += sw + 1;
        sw = -sw;
        flip.0 = !flip.0;
    }
    if sh < 0 {
        sy += sh + 1;
        sh = -sh;
        flip.1 = !flip.1;
    }
    if sw == 0 || sh == 0 {
        return Rect::default();
    }
    let map_x = |x: i32| -> i32 {
        let i = x - dx;
        // pixel centres: Windows samples at (i + 0.5) * sw / dw
        let k = if sw == dw { i } else if s.half { ((i as i64 * sw as i64) / dw as i64) as i32 } else { (((2 * i as i64 + 1) * sw as i64) / (2 * dw as i64)) as i32 };
        if flip.0 {
            sx + sw - 1 - k
        } else {
            sx + k
        }
    };
    let map_y = |y: i32| -> i32 {
        let i = y - dy;
        let k = if sh == dh { i } else if s.half { ((i as i64 * sh as i64) / dh as i64) as i32 } else { (((2 * i as i64 + 1) * sh as i64) / (2 * dh as i64)) as i32 };
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
    // raster ops combine stored pixels (an indexed destination's: indices); a source of
    // another format is converted first, to an index by the 5 high bits of each component
    // as Windows does
    let same = sv.bpp == d.bpp && !mono_to_color && !color_to_mono && if d.bpp <= 8 { sv.pal == d.pal } else { sv.r565 == d.r565 };
    let mask = d.raw_mask();
    let (text_px, bk_px) = (d.pixel(text), d.pixel(bk));
    for y in r.t..r.b {
        let syy = map_y(y);
        for (k, x) in (r.l..r.r).enumerate() {
            let sxx = xs[k];
            if !in_src(sxx, syy) {
                continue;
            }
            let spx = if same {
                sv.raw(sxx, syy)
            } else {
                let sc = sv.get(sxx, syy) & 0xffffff;
                if mono_to_color {
                    if sc == 0 { text_px } else { bk_px }
                } else if color_to_mono {
                    (sc != s.bk) as u32 ^ 1
                } else if d.bpp <= 8 {
                    d.index_of(sc & 0xf8f8f8 | 0x040404)
                } else {
                    d.pixel(sc)
                }
            };
            if op == 0xcc {
                d.set_raw(x, y, spx);
                continue;
            }
            let dpx = if need_dst { d.raw(x, y) } else { 0 };
            let p = if need_pat { d.pixel(pat.at(x, y)) } else { 0 };
            d.set_raw(x, y, rop3(op, p, spx, dpx) & mask);
        }
    }
    r
}

pub fn fill_rect_brush(dc: u32, r: Rect, brush: u32) {
    pat_rect(dc, r, Some(brush), 0xf0);
}

/// PatBlt of a rectangle (user32's FillRect/InvertRect/FrameRect are made of these): with
/// `brush` instead of the DC's. An upside-down rectangle is turned the right way as PatBlt's
/// negative extents are: x+w+1 ..= x.
pub fn pat_rect(dc: u32, r: Rect, brush: Option<u32>, op: u8) {
    let flip = |a: i32, b: i32| if a > b { (b + 1, a + 1) } else { (a, b) };
    let ((l, rr), (t, b)) = (flip(r.l, r.r), flip(r.t, r.b));
    let r = Rect { l, t, r: rr, b };
    draw(dc, |t, s, c| {
        let pat = pattern(t, s, brush.unwrap_or(s.brush));
        if matches!(pat, Pat::None) && uses_pat(op) {
            return Rect::default();
        }
        let r = c.rect(r);
        pat_blt(c, r, &pat, op)
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
    /// the collection and the face in it as Windows has them (msgothic.ttc: MS Gothic, MS UI
    /// Gothic, MS PGothic; winref dwfont), else the font's own file. `ippai ttc` makes the
    /// collections from the separate files (a third of their size); a face is found in one by
    /// its family name, so a collection in another order works too.
    ttc: (&'static str, u32),
    file: &'static str,
    fixed: bool,
    roman: bool,
}

const FACES: &[Face] = &[
    Face { names: &["ＭＳ ゴシック", "MS Gothic"], ttc: ("msgothic.ttc", 0), file: "msgothic.ttf", fixed: true, roman: false },
    Face { names: &["ＭＳ Ｐゴシック", "MS PGothic"], ttc: ("msgothic.ttc", 2), file: "mspgothic.ttf", fixed: false, roman: false },
    Face { names: &["ＭＳ 明朝", "MS Mincho"], ttc: ("msmincho.ttc", 0), file: "msmincho.ttf", fixed: true, roman: true },
    Face { names: &["ＭＳ Ｐ明朝", "MS PMincho"], ttc: ("msmincho.ttc", 1), file: "mspmincho.ttf", fixed: false, roman: true },
    Face { names: &["MS UI Gothic"], ttc: ("msgothic.ttc", 1), file: "msuigothic.ttf", fixed: false, roman: false },
];

/// Face `index` of a TrueType collection as a font file of its own (its tables copied out),
/// so the rest reads every font as face 0.
fn ttc_face(d: &[u8], index: u32) -> Option<Vec<u8>> {
    let be16 = |o: usize| d.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let be32 = |o: usize| d.get(o..o + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()));
    if d.get(..4)? != b"ttcf" || index >= be32(8)? {
        return None;
    }
    let dir = be32(12 + 4 * index as usize)? as usize;
    let n = be16(dir + 4)? as usize;
    let mut out = d.get(dir..dir + 12 + 16 * n)?.to_vec();
    for i in 0..n {
        let r = 12 + 16 * i;
        let (off, len) = (be32(dir + r + 8)? as usize, be32(dir + r + 12)? as usize);
        let at = out.len() as u32;
        out[r + 8..r + 12].copy_from_slice(&at.to_be_bytes());
        out.extend_from_slice(d.get(off..off + len)?);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    Some(out)
}

static FILES: OnceLock<Mutex<HashMap<usize, Option<Arc<FontVec>>>>> = OnceLock::new();

/// Fonts the game added (AddFontResource*, AddFontMemResourceEx): (family names, font).
/// Their face indices follow FACES'.
static ADDED: Mutex<Vec<(Vec<String>, Arc<FontVec>)>> = Mutex::new(Vec::new());

/// Registers a TrueType font file's data; returns how many fonts it held (0 if none).
fn add_font(data: Vec<u8>) -> u32 {
    let mut names: Vec<String> = Vec::new();
    if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
        for n in face.names() {
            if matches!(n.name_id, 1 | 4 | 16) {
                if let Some(s) = n.to_string() {
                    if !s.is_empty() && !names.contains(&s) {
                        names.push(s);
                    }
                }
            }
        }
    }
    // the Japanese family name first: what a Japanese Windows enumerates (Siglus looks for it)
    if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
        let ja = face.names().into_iter().find(|n| n.name_id == 1 && n.platform_id == ttf_parser::PlatformId::Windows && n.language_id == 0x411);
        if let Some(j) = ja.and_then(|n| n.to_string()).filter(|j| !j.is_empty()) {
            names.retain(|n| n != &j);
            names.insert(0, j);
        }
    }
    let Ok(f) = FontVec::try_from_vec(data) else { return 0 };
    if names.is_empty() {
        return 0;
    }
    let mut g = ADDED.lock().unwrap();
    if !g.iter().any(|(n, _)| n == &names) {
        g.push((names, Arc::new(f)));
    }
    1
}

/// A face's name (as GetTextFace reports it).
fn face_name(i: usize) -> String {
    match FACES.get(i) {
        Some(f) => f.names[0].to_string(),
        None => ADDED.lock().unwrap().get(i - FACES.len()).map_or_else(String::new, |(n, _)| n[0].clone()),
    }
}

fn font_file(i: usize) -> Option<Arc<FontVec>> {
    if i >= FACES.len() {
        return ADDED.lock().unwrap().get(i - FACES.len()).map(|(_, f)| f.clone());
    }
    let m = FILES.get_or_init(|| Mutex::new(HashMap::default()));
    let mut g = m.lock().unwrap();
    g.entry(i)
        .or_insert_with(|| {
            let dirs = [std::env::var("SAKE_FONTS").ok(), crate::files::resolve("C:\\windows\\Fonts\\")];
            let (ttc, index) = FACES[i].ttc;
            for d in dirs.into_iter().flatten() {
                let d = d.trim_end_matches('/');
                let data = match kura::fs::read(format!("{d}/{ttc}")) {
                    Ok(c) => ttc_face(&c, ttc_index(&c, FACES[i].names).unwrap_or(index)),
                    Err(_) => kura::fs::read(format!("{d}/{}", FACES[i].file)).ok(),
                };
                if let Some(f) = data.and_then(|v| FontVec::try_from_vec(v).ok()) {
                    return Some(Arc::new(f));
                }
            }
            eprintln!("sake: font {} not found", FACES[i].file);
            None
        })
        .clone()
}

/// For DirectWrite's system font collection, the built-in faces: their family names (Japanese
/// first), the guest path of the file that holds one and its index in that file.
pub fn sys_faces() -> usize {
    FACES.len()
}
pub fn sys_face_names(i: usize) -> &'static [&'static str] {
    FACES[i].names
}
pub fn sys_face_file(i: usize) -> (String, u32) {
    static KNOWN: Mutex<Vec<Option<(String, u32)>>> = Mutex::new(Vec::new());
    if let Some(Some(k)) = KNOWN.lock().unwrap().get(i) {
        return k.clone();
    }
    let (ttc, index) = FACES[i].ttc;
    let p = format!("C:\\Windows\\Fonts\\{ttc}");
    let k = match crate::files::resolve(&p).and_then(|h| kura::fs::read(h).ok()) {
        Some(d) => (p, ttc_index(&d, FACES[i].names).unwrap_or(index)),
        None => (format!("C:\\Windows\\Fonts\\{}", FACES[i].file), 0),
    };
    let mut g = KNOWN.lock().unwrap();
    if g.len() <= i {
        g.resize(i + 1, None);
    }
    g[i] = Some(k.clone());
    k
}

/// The face of collection `d` whose family name is one of `names`.
fn ttc_index(d: &[u8], names: &[&str]) -> Option<u32> {
    (0..ttf_parser::fonts_in_collection(d)?).find(|&k| {
        ttf_parser::Face::parse(d, k)
            .is_ok_and(|f| f.names().into_iter().any(|n| n.name_id == 1 && n.to_string().is_some_and(|s| names.iter().any(|m| *m == s))))
    })
}
/// DWRITE_FONT_METRICS of built-in face `i` in design units: em, ascent, descent, line gap,
/// cap height, x height, underline position and thickness, strikethrough position and
/// thickness (the Windows ascent and descent; the gap is what hhea's line height adds).
pub fn sys_face_design_metrics(i: usize) -> Option<[u16; 10]> {
    let f = font_file(i)?;
    let t = ttf_parser::Face::parse(f.as_slice(), 0).ok()?;
    let m = font_metrics(i, &f);
    let gap = (m.hhea_asc + m.hhea_desc + m.line_gap - m.win_asc - m.win_desc).max(0.0);
    let os2 = t.tables().os2;
    let cap = os2.and_then(|o| o.capital_height()).unwrap_or(0);
    let x = os2.and_then(|o| o.x_height()).unwrap_or(0);
    let u = t.underline_metrics().map_or((0, 0), |u| (u.position, u.thickness));
    let st = t.strikeout_metrics().map_or((0, 0), |u| (u.position, u.thickness));
    Some([m.upem as u16, m.win_asc as u16, m.win_desc as u16, gap as u16, cap as u16, x as u16, u.0 as u16, u.1 as u16, st.0 as u16, st.1 as u16])
}
/// Whether built-in face `i` has a glyph for `ch`.
pub fn sys_face_has(i: usize, ch: u32) -> bool {
    font_file(i).is_some_and(|f| char::from_u32(ch).is_some_and(|c| f.glyph_id(c).0 != 0))
}

/// The face for a LOGFONT face name (the Japanese and English names, '@' for vertical).
fn face_index(lf: &LogFont) -> usize {
    let name = lf.face.trim_start_matches('@');
    for (i, (names, _)) in ADDED.lock().unwrap().iter().enumerate() {
        if names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
            return FACES.len() + i;
        }
    }
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
    /// TEXTMETRIC's average and maximum widths and external leading
    ave: i32,
    max_w: i32,
    ext_leading: i32,
}

/// A font's metrics as Windows reads them (font units): the cell from OS/2's usWinAscent and
/// usWinDescent, the average width from xAvgCharWidth (half the em in MS Gothic, nearly all
/// of it in Noto Sans CJK: SiglusEngine spaces text by it), hhea for the rest.
#[derive(Clone, Copy)]
struct FontMetrics {
    upem: f32,
    win_asc: f32,
    win_desc: f32,
    hhea_asc: f32,
    hhea_desc: f32,
    line_gap: f32,
    avg: f32,
    max_adv: f32,
}

fn font_metrics(face: usize, font: &FontVec) -> FontMetrics {
    static M: Mutex<Vec<Option<FontMetrics>>> = Mutex::new(Vec::new());
    if let Some(Some(m)) = M.lock().unwrap().get(face) {
        return *m;
    }
    let upem = font.units_per_em().unwrap_or(1000.0);
    let (asc, desc) = (font.ascent_unscaled(), -font.descent_unscaled());
    let mut m = FontMetrics { upem, win_asc: asc, win_desc: desc, hhea_asc: asc, hhea_desc: desc, line_gap: font.line_gap_unscaled(), avg: upem / 2.0, max_adv: upem };
    if let Ok(t) = ttf_parser::Face::parse(font.as_slice(), 0) {
        let hhea = t.tables().hhea;
        (m.hhea_asc, m.hhea_desc, m.line_gap) = (hhea.ascender as f32, -(hhea.descender as f32), hhea.line_gap as f32);
        let raw = |tag: &[u8; 4]| t.raw_face().table(ttf_parser::Tag::from_bytes(tag));
        if let Some(h) = raw(b"hhea").filter(|h| h.len() >= 12) {
            m.max_adv = u16::from_be_bytes([h[10], h[11]]) as f32;
        }
        let avg = raw(b"OS/2").filter(|o| o.len() >= 4).map_or(0, |o| i16::from_be_bytes([o[2], o[3]]));
        if let Some(os2) = t.tables().os2 {
            let (wa, wd) = (os2.windows_ascender() as f32, os2.windows_descender().unsigned_abs() as f32);
            if wa + wd > 0.0 {
                (m.win_asc, m.win_desc) = (wa, wd);
            }
            if avg > 0 {
                m.avg = avg as f32;
            }
        }
    }
    let mut g = M.lock().unwrap();
    if g.len() <= face {
        g.resize(face + 1, None);
    }
    g[face] = Some(m);
    m
}

fn realize(lf: &LogFont) -> Option<Real> {
    let face = face_index(lf);
    let font = font_file(face)?;
    let m = font_metrics(face, &font);
    let em = if lf.height < 0 {
        -lf.height as f32
    } else if lf.height > 0 {
        lf.height as f32 * m.upem / (m.win_asc + m.win_desc)
    } else {
        12.0
    };
    let px = |v: f32| (v * em / m.upem).round() as i32;
    let (ascent, descent) = (px(m.win_asc), px(m.win_desc));
    // (Windows: the hhea line gap less what the OS/2 cell already adds over hhea's)
    let ext_leading = px((m.line_gap - ((m.win_asc + m.win_desc) - (m.hhea_asc + m.hhea_desc))).max(0.0));
    Some(Real { face, font, em, ascent, descent, bold: lf.weight >= 600, ave: px(m.avg).max(1), max_w: px(m.max_adv).max(1), ext_leading })
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

/// Rasterized glyphs, at most [`GLYPH_CACHE_BYTES`] of them: past that the least recently
/// used half goes (a long story in many sizes would otherwise keep every glyph it drew).
#[derive(Default)]
struct Glyphs {
    map: HashMap<GlyphKey, (Arc<Glyph>, u64)>,
    tick: u64,
    bytes: usize,
}

const GLYPH_CACHE_BYTES: usize = 16 << 20;

impl Glyphs {
    fn size(g: &Glyph) -> usize {
        g.cov.len() + 64
    }
    fn get(&mut self, k: &GlyphKey) -> Option<Arc<Glyph>> {
        self.tick += 1;
        let e = self.map.get_mut(k)?;
        e.1 = self.tick;
        Some(e.0.clone())
    }
    fn put(&mut self, k: GlyphKey, g: Arc<Glyph>, limit: usize) {
        self.tick += 1;
        self.bytes += Self::size(&g);
        if let Some((old, _)) = self.map.insert(k, (g, self.tick)) {
            self.bytes -= Self::size(&old);
        }
        while self.bytes > limit {
            let mut ticks: Vec<u64> = self.map.values().map(|e| e.1).collect();
            ticks.sort_unstable();
            // (the newest half; nothing once only one bigger than the limit is left)
            let keep_from = if ticks.len() > 1 { ticks[ticks.len() / 2] } else { u64::MAX };
            let mut bytes = 0;
            self.map.retain(|_, e| {
                let keep = e.1 >= keep_from;
                if keep {
                    bytes += Self::size(&e.0);
                }
                keep
            });
            self.bytes = bytes;
        }
    }
}

static GLYPHS: Mutex<Option<Glyphs>> = Mutex::new(None);

const GLYPH_INDEX: u32 = 0x8000_0000;

fn glyph(r: &Real, ch: u32) -> Arc<Glyph> {
    let key = (r.face, r.em.to_bits(), ch, r.bold);
    if let Some(g) = GLYPHS.lock().unwrap().get_or_insert_with(Glyphs::default).get(&key) {
        return g;
    }
    let g = Arc::new(raster(r, ch));
    GLYPHS.lock().unwrap().get_or_insert_with(Glyphs::default).put(key, g.clone(), GLYPH_CACHE_BYTES);
    g
}

/// A glyph's outline even when ttf-parser rejects the charstring after drawing it: CFF glyphs
/// with bytes after `endchar` (FOT-Chiaro's う and a, as Windows and FreeType draw them).
fn lenient_outline(f: &FontVec, id: ab_glyph::GlyphId) -> Option<ab_glyph::Outline> {
    #[derive(Default)]
    struct B {
        last: ab_glyph::Point,
        start: Option<ab_glyph::Point>,
        curves: Vec<ab_glyph::OutlineCurve>,
    }
    impl ttf_parser::OutlineBuilder for B {
        fn move_to(&mut self, x: f32, y: f32) {
            self.close();
            self.last = ab_glyph::point(x, y);
            self.start = Some(self.last);
        }
        fn line_to(&mut self, x: f32, y: f32) {
            let p = ab_glyph::point(x, y);
            self.curves.push(ab_glyph::OutlineCurve::Line(self.last, p));
            self.last = p;
        }
        fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
            let p = ab_glyph::point(x, y);
            self.curves.push(ab_glyph::OutlineCurve::Quad(self.last, ab_glyph::point(x1, y1), p));
            self.last = p;
        }
        fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
            let p = ab_glyph::point(x, y);
            self.curves.push(ab_glyph::OutlineCurve::Cubic(self.last, ab_glyph::point(x1, y1), ab_glyph::point(x2, y2), p));
            self.last = p;
        }
        fn close(&mut self) {
            if let Some(s) = self.start.take() {
                if s != self.last {
                    self.curves.push(ab_glyph::OutlineCurve::Line(self.last, s));
                }
                self.last = s;
            }
        }
    }
    let face = ttf_parser::Face::parse(f.as_slice(), 0).ok()?;
    let mut b = B::default();
    face.outline_glyph(ttf_parser::GlyphId(id.0), &mut b);
    ttf_parser::OutlineBuilder::close(&mut b);
    if b.curves.is_empty() {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for c in &b.curves {
        let pts: &[ab_glyph::Point] = match c {
            ab_glyph::OutlineCurve::Line(a, b) => &[*a, *b],
            ab_glyph::OutlineCurve::Quad(a, b, c) => &[*a, *b, *c],
            ab_glyph::OutlineCurve::Cubic(a, b, c, d) => &[*a, *b, *c, *d],
        };
        for p in pts {
            (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
        }
    }
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    Some(ab_glyph::Outline { bounds: ab_glyph::Rect { min: ab_glyph::point(x0, y1), max: ab_glyph::point(x1, y0) }, curves: b.curves })
}

fn raster(r: &Real, ch: u32) -> Glyph {
    let f = &*r.font;
    let upem = f.units_per_em().unwrap_or(1000.0);
    let height = f.ascent_unscaled() - f.descent_unscaled();
    let scale = PxScale::from(r.em * height / upem);
    // (GLYPH_INDEX marks a glyph index, as from GetGlyphIndices)
    let id = if ch & GLYPH_INDEX != 0 { ab_glyph::GlyphId(ch as u16) } else { f.glyph_id(char::from_u32(ch).unwrap_or('?')) };
    let adv = f.as_scaled(scale).h_advance(id).round() as i32;
    let mut g = Glyph { x0: 0, top: 0, w: 0, h: 0, cov: Vec::new(), adv };
    let glyph = id.with_scale_and_position(scale, ab_glyph::point(0.0, 0.0));
    let outlined = f.outline_glyph(glyph.clone()).or_else(|| {
        lenient_outline(f, id).map(|o| ab_glyph::OutlinedGlyph::new(glyph, o, f.as_scaled(scale).scale_factor()))
    });
    if let Some(o) = outlined {
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

/// For DirectWrite: a font by family name at an em size in pixels.
pub struct TextFont {
    r: Real,
    asc: f32,
    desc: f32,
    gap: f32,
}

pub fn text_font(family: &str, em: f32, weight: i32) -> Option<TextFont> {
    let lf = LogFont {
        height: -(em.round().max(1.0) as i32),
        width: 0,
        esc: 0,
        orient: 0,
        weight,
        italic: 0,
        underline: 0,
        strike: 0,
        charset: 128,
        out_prec: 0,
        clip_prec: 0,
        quality: 0,
        pitch: 0,
        face: family.to_string(),
    };
    let mut r = realize(&lf)?;
    r.em = em;
    let m = font_metrics(r.face, &r.font);
    let k = em / m.upem;
    // (DirectWrite's line metrics: the hhea ascent/descent/gap, as for fonts without typo
    // metrics in use)
    Some(TextFont { asc: m.win_asc * k, desc: m.win_desc * k, gap: (m.line_gap * k).max(0.0), r })
}

/// For DirectWrite glyph runs: built-in face `face` at an em size in pixels.
pub fn face_font(face: usize, em: f32) -> Option<TextFont> {
    let names = sys_face_names(face);
    text_font(names[names.len() - 1], em, 400)
}

impl TextFont {
    /// ascent, descent and line gap in pixels
    pub fn metrics(&self) -> (f32, f32, f32) {
        (self.asc, self.desc, self.gap)
    }
    pub fn advance(&self, ch: u32) -> f32 {
        glyph(&self.r, ch).adv as f32
    }
    /// What identifies `ch`'s glyph in this font at this size (for caches of its bitmap).
    pub fn glyph_key(&self, ch: u32) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.r.face, self.r.em.to_bits(), ch, self.r.bold).hash(&mut h);
        h.finish()
    }
    /// The built-in face this font draws with (sys_face_names' index).
    pub fn face(&self) -> usize {
        self.r.face
    }
    /// The glyph index of `ch` in the face (0: none).
    pub fn glyph_index(&self, ch: u32) -> u16 {
        char::from_u32(ch).map_or(0, |c| self.r.font.glyph_id(c).0)
    }
    /// glyph and glyph_key of a glyph index
    pub fn index_glyph(&self, gi: u16, f: impl FnMut(i32, i32, i32, i32, &[u8])) {
        self.glyph(gi as u32 | GLYPH_INDEX, f)
    }
    pub fn index_advance(&self, gi: u16) -> f32 {
        glyph(&self.r, gi as u32 | GLYPH_INDEX).adv as f32
    }
    pub fn index_key(&self, gi: u16) -> u64 {
        self.glyph_key(gi as u32 | GLYPH_INDEX)
    }
    /// Calls `f(left, top above the baseline, w, h, coverage)` with the glyph's black box.
    pub fn glyph(&self, ch: u32, mut f: impl FnMut(i32, i32, i32, i32, &[u8])) {
        let g = glyph(&self.r, ch);
        if g.w > 0 && g.h > 0 {
            f(g.x0, g.top, g.w, g.h, &g.cov);
        }
    }
}

/// For Uniscribe (usp.rs): the font selected into `dc`.
pub fn dc_font_handle(dc: u32) -> u32 {
    with(|t| match t.m.get(&dc) {
        Some(G::Dc(d)) => d.s.font,
        _ => 0,
    })
}
/// For Uniscribe: the glyph indices of `s` and each glyph's ABC widths in font `hfont` (the
/// DC's when 0).
pub fn font_glyphs(dc: u32, hfont: u32, s: &[u16]) -> Option<Vec<u16>> {
    let r = font_real(dc, hfont)?;
    Some(s.iter().map(|&c| r.font.glyph_id(char::from_u32(c as u32).unwrap_or('?')).0).collect())
}
pub fn font_abc(dc: u32, hfont: u32, glyphs: &[u16]) -> Option<Vec<(i32, i32, i32)>> {
    let r = font_real(dc, hfont)?;
    Some(
        glyphs
            .iter()
            .map(|&gi| {
                let g = glyph(&r, gi as u32 | GLYPH_INDEX);
                (g.x0, g.w.max(0), g.adv - g.x0 - g.w.max(0))
            })
            .collect(),
    )
}
fn font_real(dc: u32, hfont: u32) -> Option<Real> {
    if hfont == 0 {
        return real_of(dc).map(|x| x.0);
    }
    let lf = with(|t| match t.m.get(&hfont) {
        Some(G::Font(f)) => Some(f.clone()),
        _ => None,
    })?;
    realize(&lf)
}

fn real_of(dc: u32) -> Option<(Real, LogFont, DcState)> {
    let (lf, s) = with(|t| {
        let Some(G::Dc(d)) = t.m.get(&dc) else { return None };
        Some((dc_font(t, &d.s), d.s))
    })?;
    Some((realize(&lf)?, lf, s))
}

pub(crate) fn read_logfont(p: u32, wide: bool) -> LogFont {
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

pub(crate) fn write_logfont(p: u32, lf: &LogFont, wide: bool) {
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
    let vals = [height, r.ascent, r.descent, (height - full).max(0), r.ext_leading, r.ave, r.max_w, if r.bold { 700 } else { 400 }, 0, 96, 96];
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
    // (ETO_GLYPH_INDEX: glyph indices, not characters)
    let gi = if opts & 0x10 != 0 { GLYPH_INDEX } else { 0 };
    let glyphs: Vec<Arc<Glyph>> = s.iter().map(|&c| glyph(&r, c as u32 | gi)).collect();
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
            // BITMAP, then DIBSECTION's BITMAPINFOHEADER, bit fields, section and offset; x64:
            // bmBits 8 bytes (BITMAP 32), the section 8 aligned (DIBSECTION 104)
            let x64 = mem::x64();
            let (small, full, d) = if x64 { (32, 104, 8) } else { (24, 84, 0) };
            let need = if b.dib && n >= full { full } else { small };
            if p == 0 {
                return need;
            }
            let mut buf = [0u8; 104];
            let put = |buf: &mut [u8], o: usize, v: u32| buf[o..o + 4].copy_from_slice(&v.to_le_bytes());
            put(&mut buf, 4, b.w as u32);
            put(&mut buf, 8, b.h as u32);
            put(&mut buf, 12, if b.dib { b.stride } else { b.stride });
            buf[16..18].copy_from_slice(&1u16.to_le_bytes());
            buf[18..20].copy_from_slice(&b.bpp.to_le_bytes());
            put(&mut buf, if x64 { 24 } else { 20 }, if b.dib { b.bits } else { 0 });
            if need == full {
                put(&mut buf, 24 + d, 40);
                put(&mut buf, 28 + d, b.w as u32);
                put(&mut buf, 32 + d, if b.bottom_up { b.h as u32 } else { (-b.h) as u32 });
                buf[36 + d..38 + d].copy_from_slice(&1u16.to_le_bytes());
                buf[38 + d..40 + d].copy_from_slice(&b.bpp.to_le_bytes());
                put(&mut buf, 44 + d, b.stride * b.h as u32);
                put(&mut buf, 56 + d, if b.bpp <= 8 { b.pal.len() as u32 } else { 0 });
                if b.bpp == 16 {
                    put(&mut buf, 40 + d, 3);
                    let m = if b.r565 { [0xf800, 0x7e0, 0x1f] } else { [0x7c00, 0x3e0, 0x1f] };
                    for (i, v) in m.iter().enumerate() {
                        put(&mut buf, 64 + d + 4 * i, *v);
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
    add(G::Brush(Brush { style: 2, color: c.a(1), hatch: c.a(0), pattern: 0 }))
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
setter!(SetPolyFillMode, fill_mode);
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
getter!(GetPolyFillMode, fill_mode);
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

/// A 32-bit top-down DIB section holding `px` (BGRA rows): GDI+'s HBITMAP of a bitmap.
pub(crate) fn dib_section_bgra(w: u32, h: u32, px: &[u8]) -> u32 {
    let Some(bits) = mem::alloc((w * h * 4).max(4)) else { return 0 };
    mem::write(bits, &px[..(w * h * 4) as usize]);
    add(G::Bitmap(Bitmap { w: w as i32, h: h as i32, bpp: 32, bits, stride: w * 4, bottom_up: false, pal: Vec::new(), r565: false, dib: true, own: 2 }))
}

/// Copies between a bitmap and a DIB in guest memory: rows [start, start+lines) counted
/// from the DIB's bottom (bottom-up) or top.
fn dib_copy(b: &Bitmap, start: u32, lines: u32, bits: u32, i: &DibInfo, to_dib: bool) -> u32 {
    let bv = View::of(b);
    let dv = dib_view(i, bits);
    let lines = lines.min(i.h as u32 - start.min(i.h as u32)).min(b.h as u32);
    let w = i.w.min(b.w);
    for k in 0..lines as i32 {
        // row in top-down terms of both images: scan lines count from the bottom of the DIB,
        // and the DIB's rows lie over the bitmap's top rows (a 4-line DIB is the top 4 rows)
        let drow = if i.top_down { start as i32 + k } else { i.h - 1 - (start as i32 + k) };
        let row = drow;
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
            let r = blit(c, dst_r, Some(Src { v: &copy.0, x: sx, y: sy, w: s.2, h: s.3, bk: rgb(ds.bk), half: ds.stretch == 4 }), op, &pat, rgb(ds.text), rgb(ds.bk));
            return r;
        };
        let src_ = Src { v: &sv, x: s.0 + sox, y: s.1 + soy, w: s.2, h: s.3, bk: rgb(ss.bk), half: ds.stretch == 4 };
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
                // pixel centres, as StretchBlt
                let sx = s.0 + sox + ((2 * (x - dx) + 1) * s.2) / (2 * d.2.max(1));
                let sy = s.1 + soy + ((2 * (y - dy) + 1) * s.3) / (2 * d.3.max(1));
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
        if s.2 == d.2 && s.3 == d.3 && sv.bpp == 32 && cv.v.bpp == 32 {
            alpha_rows(&sv, &cv.v, &r, (s.0 + sox - dx, s.1 + soy - dy), k, per_pixel);
            return r;
        }
        for y in r.t..r.b {
            for x in r.l..r.r {
                // pixel centres, as StretchBlt
                let sx = s.0 + sox + ((2 * (x - dx) + 1) * s.2) / (2 * d.2.max(1));
                let sy = s.1 + soy + ((2 * (y - dy) + 1) * s.3) / (2 * d.3.max(1));
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
/// x / 255 rounded down, for x up to 255 * 255.
#[inline(always)]
fn div255(x: u32) -> u32 {
    (x + 1 + (x >> 8)) >> 8
}

/// AlphaBlend without stretching, 32 bpp to 32 bpp, row by row (the same results as the
/// pixel loop): source pixel (x + o.0, y + o.1) over destination pixel (x, y) in `r`. The
/// source may be the destination (each pixel is read before it is written, in order).
fn alpha_rows(sv: &View, dv: &View, r: &Rect, o: (i32, i32), k: u32, per_pixel: bool) {
    let (l, rr) = (r.l.max(-o.0), r.r.min(sv.w - o.0));
    if l >= rr {
        return;
    }
    for y in r.t.max(-o.1)..r.b.min(sv.h - o.1) {
        let sp = unsafe { (sv.row(y + o.1) as *const u32).add((l + o.0) as usize) };
        let dp = unsafe { (dv.row(y) as *mut u32).add(l as usize) };
        for i in 0..(rr - l) as usize {
            let (ps, pd) = unsafe { (sp.add(i), dp.add(i)) };
            let p = unsafe { ps.read_unaligned() };
            let a = if !per_pixel { k } else if k == 255 { p >> 24 } else { div255((p >> 24) * k) };
            if k == 255 && a == 255 {
                // (an opaque source pixel, the common case: sc + 0)
                unsafe { pd.write_unaligned(p) };
                continue;
            }
            if k == 255 && p == 0 {
                continue; // (sc 0 and the destination times 255 / 255)
            }
            let q = unsafe { pd.read_unaligned() };
            let ia = 255 - a;
            let ch = |sh: u32| {
                let sc = if k == 255 { p >> sh & 255 } else { div255((p >> sh & 255) * k) };
                (sc + div255((q >> sh & 255) * ia)).min(255) << sh
            };
            unsafe { pd.write_unaligned(ch(24) | ch(16) | ch(8) | ch(0)) };
        }
    }
}

/// GradientFill: rectangles (GRADIENT_FILL_RECT_H 0 / _V 1) or triangles (2), colors
/// interpolated per pixel from the TRIVERTEXes (16-bit channels).
fn GradientFill(c: &mut Ctx) -> u32 {
    let (dc, pv, nv, pm, nm, mode) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let vert = |i: u32| -> Option<((i32, i32), [f64; 3])> {
        if i >= nv {
            return None;
        }
        let a = pv + 16 * i;
        Some(((r32(a) as i32, r32(a + 4) as i32), [r16(a + 8) as f64, r16(a + 10) as f64, r16(a + 12) as f64]))
    };
    let pack = |c: [f64; 3]| -> u32 { c.iter().fold(0, |acc, &v| acc << 8 | ((v / 256.0) as u32).min(255)) };
    draw(dc, |_, _, cv| {
        let mut d = Rect::default();
        for m in 0..nm {
            if mode < 2 {
                let (Some(a), Some(b)) = (vert(r32(pm + 8 * m)), vert(r32(pm + 8 * m + 4))) else { continue };
                let (l, t, r, bo) = (a.0 .0.min(b.0 .0), a.0 .1.min(b.0 .1), a.0 .0.max(b.0 .0), a.0 .1.max(b.0 .1));
                let (c0, c1) = if (mode == 0 && a.0 .0 <= b.0 .0) || (mode == 1 && a.0 .1 <= b.0 .1) { (a.1, b.1) } else { (b.1, a.1) };
                let rr = cv.rect(Rect { l, t, r, b: bo }).and(&cv.clip);
                for y in rr.t..rr.b {
                    for x in rr.l..rr.r {
                        let (k, n) = if mode == 0 { (x - cv.ox - l, r - l) } else { (y - cv.oy - t, bo - t) };
                        let f = k as f64 / n.max(1) as f64;
                        let col = [0, 1, 2].map(|i| c0[i] + (c1[i] - c0[i]) * f);
                        cv.v.put(x, y, pack(col));
                    }
                }
                d = d.or(&rr);
            } else {
                let vs: Vec<_> = (0..3).filter_map(|k| vert(r32(pm + 12 * m + 4 * k))).collect();
                if vs.len() < 3 {
                    continue;
                }
                let p: Vec<(i32, i32)> = vs.iter().map(|v| cv.dev(v.0 .0, v.0 .1)).collect();
                let det = ((p[1].1 - p[2].1) * (p[0].0 - p[2].0) + (p[2].0 - p[1].0) * (p[0].1 - p[2].1)) as f64;
                if det == 0.0 {
                    continue;
                }
                for (y, x0, x1) in gdi_geom::poly_spans(&[p.clone()], false) {
                    for x in x0..x1 {
                        if !cv.clip.has(x, y) {
                            continue;
                        }
                        let w0 = ((p[1].1 - p[2].1) * (x - p[2].0) + (p[2].0 - p[1].0) * (y - p[2].1)) as f64 / det;
                        let w1 = ((p[2].1 - p[0].1) * (x - p[2].0) + (p[0].0 - p[2].0) * (y - p[2].1)) as f64 / det;
                        let w2 = 1.0 - w0 - w1;
                        let col = [0, 1, 2].map(|i| (vs[0].1[i] * w0 + vs[1].1[i] * w1 + vs[2].1[i] * w2).clamp(0.0, 65535.0));
                        cv.v.put(x, y, pack(col));
                        d = d.or(&Rect { l: x, t: y, r: x + 1, b: y + 1 });
                    }
                }
            }
        }
        d
    }) as u32
}
fn GdiGradientFill(c: &mut Ctx) -> u32 {
    GradientFill(c)
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
        blit(c, (dx, dy, d.2, d.3), Some(Src { v: &v, x: s.0, y: sy, w: s.2, h: s.3, bk: rgb(ds.bk), half: ds.stretch == 4 }), op, &pat, rgb(ds.text), rgb(ds.bk))
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
    let mut out = u32::MAX;
    draw(dc, |_, s, cv| {
        let (x, y) = cv.dev(x, y);
        // (SetPixel follows the DC's ROP2 too, and returns the color it could store, also
        // outside the clip)
        let px = cv.v.pixel(cref(col));
        out = cv.v.color_of(px);
        if cv.clip.has(x, y) {
            cv.v.set_raw(x, y, rop2(s.rop2, px, cv.v.raw(x, y)) & cv.v.raw_mask());
            return Rect { l: x, t: y, r: x + 1, b: y + 1 };
        }
        Rect::default()
    });
    out
}
fn SetPixelV(c: &mut Ctx) -> u32 {
    SetPixel(c);
    1
}

// --- lines and shapes

#[derive(Clone, Copy)]
struct PenInfo {
    col: u32,
    width: i32,
    /// PS_SOLID 0, DASH 1, DOT 2, DASHDOT 3, DASHDOTDOT 4
    style: u32,
}

fn pen_of(t: &Objs, s: &DcState) -> Option<PenInfo> {
    if s.pen == STOCK + 4 * 19 {
        return Some(PenInfo { col: rgb(s.dc_pen), width: 1, style: 0 });
    }
    match t.m.get(&s.pen) {
        // (a wide pen made by CreatePen is solid whatever its style)
        Some(G::Pen(p)) if p.style & 15 != 5 => Some(PenInfo { col: rgb(p.color), width: p.width, style: if p.width > 1 || p.style & 15 > 4 { 0 } else { p.style & 15 } }),
        _ => None,
    }
}

/// A binary raster op (R2_BLACK 1 ..= R2_WHITE 16) of pen and destination pixel values: op-1 is
/// the truth table (bit 2*pen+dst); callers mask the result to the pixel's bits.
#[inline]
fn rop2(op: u32, p: u32, d: u32) -> u32 {
    let t = op.wrapping_sub(1) & 15;
    let m = |b: u32| if t >> b & 1 != 0 { u32::MAX } else { 0 };
    (!p & !d & m(0)) | (!p & d & m(1)) | (p & !d & m(2)) | (p & d & m(3))
}

fn plot(c: &mut Canvas, x: i32, y: i32, col: u32, op: u32) -> bool {
    if !c.clip.has(x, y) {
        return false;
    }
    // raster ops act on the stored pixel values (palette indices of an indexed DIB)
    let px = c.v.pixel(col);
    let d = if op == 13 { 0 } else { c.v.raw(x, y) };
    c.v.set_raw(x, y, rop2(op, px, d) & c.v.raw_mask());
    true
}

fn pts_rect(pts: &[(i32, i32)]) -> Rect {
    pts.iter().fold(Rect::default(), |a, &(x, y)| a.or(&Rect { l: x, t: y, r: x + 1, b: y + 1 }))
}

/// Draws paths (logical coordinates) with the DC's pen; `closed` joins each path's ends.
/// A cosmetic pen leaves out each path's last point (unless closed), a wide pen covers it.
fn stroke(c: &mut Canvas, s: &DcState, pen: PenInfo, paths: &[Vec<(i32, i32)>], closed: bool) -> Rect {
    let mut done: Vec<(i32, i32)> = Vec::new();
    if pen.width > 1 {
        let nib = gdi_geom::nib(pen.width);
        let mut set = std::collections::HashSet::new();
        for p in paths {
            let mut q: Vec<(i32, i32)> = p.iter().map(|&(x, y)| c.dev(x, y)).collect();
            if closed && q.len() > 1 {
                q.push(q[0]);
            }
            if q.len() == 1 {
                q.push(q[0]);
            }
            for w in q.windows(2) {
                for (x, y) in gdi_geom::line_pts(w[0], w[1], true) {
                    for &(u, v) in &nib {
                        set.insert((x + u, y + v));
                    }
                }
            }
        }
        for (x, y) in set {
            if plot(c, x, y, pen.col, s.rop2) {
                done.push((x, y));
            }
        }
        return pts_rect(&done);
    }
    // dash patterns of cosmetic pens (on, off, ...)
    let dashes: &[u32] = match pen.style {
        1 => &[18, 6],
        2 => &[3, 3],
        3 => &[9, 6, 3, 6],
        4 => &[9, 3, 3, 3, 3, 3],
        _ => &[],
    };
    let period: u32 = dashes.iter().sum();
    let mut pos = 0u32;
    for p in paths {
        let mut q: Vec<(i32, i32)> = p.iter().map(|&(x, y)| c.dev(x, y)).collect();
        if closed && q.len() > 1 {
            q.push(q[0]);
        }
        for w in q.windows(2) {
            for (x, y) in gdi_geom::line_pts(w[0], w[1], false) {
                let mut on = true;
                if period > 0 {
                    let mut k = pos % period;
                    for (i, &d) in dashes.iter().enumerate() {
                        if k < d {
                            on = i % 2 == 0;
                            break;
                        }
                        k -= d;
                    }
                    pos += 1;
                }
                let hit = if on {
                    plot(c, x, y, pen.col, s.rop2)
                } else if s.bk_mode == 2 {
                    plot(c, x, y, rgb(s.bk), s.rop2)
                } else {
                    false
                };
                if hit {
                    done.push((x, y));
                }
            }
        }
    }
    pts_rect(&done)
}

/// Fills spans (device coordinates) with a brush pattern through ROP2.
fn fill_spans(c: &mut Canvas, spans: &[gdi_geom::Span], pat: &Pat, op: u32) -> Rect {
    if matches!(pat, Pat::None) {
        return Rect::default();
    }
    let mut d = Rect::default();
    for &(y, x0, x1) in spans {
        let r = Rect { l: x0, t: y, r: x1, b: y + 1 }.and(&c.clip);
        if r.empty() {
            continue;
        }
        for x in r.l..r.r {
            plot(c, x, y, pat.at(x, y), op);
        }
        d = d.or(&r);
    }
    d
}

fn dev_spans(c: &Canvas, s: Vec<gdi_geom::Span>) -> Vec<gdi_geom::Span> {
    s.into_iter().map(|(y, a, b)| (y + c.oy, a + c.ox, b + c.ox)).collect()
}

fn line(c: &mut Canvas, s: &DcState, pen: PenInfo, a: (i32, i32), b: (i32, i32)) -> Rect {
    stroke(c, s, pen, &[vec![a, b]], false)
}

fn LineTo(c: &mut Ctx) -> u32 {
    let (dc, x, y) = (c.a(0), c.i(1), c.i(2));
    draw(dc, |t, s, cv| match pen_of(t, s) {
        Some(pen) => line(cv, s, pen, s.pos, (x, y)),
        None => Rect::default(),
    });
    with_dc(dc, |d| d.s.pos = (x, y));
    1
}
fn read_pts(p: u32, n: u32) -> Vec<(i32, i32)> {
    (0..n).map(|i| (r32(p + 8 * i) as i32, r32(p + 8 * i + 4) as i32)).collect()
}
fn polyline(dc: u32, pts: Vec<(i32, i32)>) -> u32 {
    draw(dc, |t, s, cv| match pen_of(t, s) {
        Some(pen) => stroke(cv, s, pen, &[pts], false),
        None => Rect::default(),
    }) as u32
}
fn Polyline(c: &mut Ctx) -> u32 {
    polyline(c.a(0), read_pts(c.a(1), c.a(2)))
}
fn PolylineTo(c: &mut Ctx) -> u32 {
    let (dc, p, n) = (c.a(0), c.a(1), c.a(2));
    let Some(s) = dc_state(dc) else { return 0 };
    let mut pts = vec![s.pos];
    pts.extend(read_pts(p, n));
    let last = *pts.last().unwrap();
    polyline(dc, pts);
    with_dc(dc, |d| d.s.pos = last);
    1
}
fn PolyPolyline(c: &mut Ctx) -> u32 {
    let (dc, p, counts, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut at = p;
    let mut paths = Vec::new();
    for i in 0..n {
        let k = r32(counts + 4 * i);
        paths.push(read_pts(at, k));
        at += 8 * k;
    }
    draw(dc, |t, s, cv| match pen_of(t, s) {
        Some(pen) => stroke(cv, s, pen, &paths, false),
        None => Rect::default(),
    }) as u32
}
fn bezier_path(pts: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let mut v = vec![];
    if let Some(&f) = pts.first() {
        v.push(f);
    }
    for k in (1..pts.len().saturating_sub(2)).step_by(3) {
        let q = gdi_geom::bezier([pts[k - 1], pts[k], pts[k + 1], pts[k + 2]]);
        v.extend(q.into_iter().skip(1));
    }
    v
}
fn PolyBezier(c: &mut Ctx) -> u32 {
    let pts = read_pts(c.a(1), c.a(2));
    polyline(c.a(0), bezier_path(&pts))
}
fn PolyBezierTo(c: &mut Ctx) -> u32 {
    let (dc, p, n) = (c.a(0), c.a(1), c.a(2));
    let Some(s) = dc_state(dc) else { return 0 };
    let mut pts = vec![s.pos];
    pts.extend(read_pts(p, n));
    let last = *pts.last().unwrap();
    polyline(dc, bezier_path(&pts));
    with_dc(dc, |d| d.s.pos = last);
    1
}

/// A closed shape: `area(l, t, r, b)` gives its spans for a box. With a pen the box is the
/// whole shape and its border is drawn with the pen, without one the box is a pixel smaller.
fn shape(dc: u32, b: (i32, i32, i32, i32), area: impl Fn(i32, i32, i32, i32) -> Vec<gdi_geom::Span>) -> u32 {
    let (mut l, mut t, mut r, mut bo) = b;
    if l > r {
        std::mem::swap(&mut l, &mut r);
    }
    if t > bo {
        std::mem::swap(&mut t, &mut bo);
    }
    draw(dc, |tt, s, cv| {
        let pen = pen_of(tt, s);
        let pat = pattern(tt, s, s.brush);
        match pen {
            None => {
                let sp = dev_spans(cv, area(l, t, r - 1, bo - 1));
                fill_spans(cv, &sp, &pat, s.rop2)
            }
            Some(pen) => {
                let sp = dev_spans(cv, area(l, t, r, bo));
                let (edge, inner) = gdi_geom::outline(&sp);
                let mut d = fill_spans(cv, &inner, &pat, s.rop2);
                if pen.width > 1 {
                    // a wide pen: the nib along the border
                    let nib = gdi_geom::nib(pen.width);
                    let set: std::collections::HashSet<(i32, i32)> = edge.iter().flat_map(|&(x, y)| nib.iter().map(move |&(u, v)| (x + u, y + v))).collect();
                    for (x, y) in set {
                        plot(cv, x, y, pen.col, s.rop2);
                    }
                } else {
                    for &(x, y) in &edge {
                        plot(cv, x, y, pen.col, s.rop2);
                    }
                }
                d = d.or(&pts_rect(&edge));
                d
            }
        }
    }) as u32
}
fn Rectangle(c: &mut Ctx) -> u32 {
    shape(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), |l, t, r, b| (t..b).filter(|_| r > l).map(|y| (y, l, r)).collect())
}
fn Ellipse(c: &mut Ctx) -> u32 {
    shape(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), gdi_geom::ellipse_spans)
}
fn RoundRect(c: &mut Ctx) -> u32 {
    let (ew, eh) = (c.i(5), c.i(6));
    shape(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), move |l, t, r, b| gdi_geom::round_rect_spans(l, t, r, b, ew, eh))
}
/// Pie (pie: true) or chord of the ellipse in the box, counter-clockwise from radial s to e.
fn arc_area(l: i32, t: i32, r: i32, b: i32, s: (i32, i32), e: (i32, i32), pie: bool) -> Vec<gdi_geom::Span> {
    let (cx, cy) = ((l + r) as f64 / 2.0, (t + b) as f64 / 2.0);
    let (a0, a1) = (gdi_geom::angle(cx, cy, s.0 as f64, s.1 as f64), gdi_geom::angle(cx, cy, e.0 as f64, e.1 as f64));
    // the chord's end points on the ellipse
    let (rx, ry) = ((r - l) as f64 / 2.0, (b - t) as f64 / 2.0);
    let on = |a: f64| (cx + rx * a.cos(), cy - ry * a.sin());
    let (p0, p1) = (on(a0), on(a1));
    let mut out = Vec::new();
    for (y, x0, x1) in gdi_geom::ellipse_spans(l, t, r, b) {
        let mut run: Option<i32> = None;
        for x in x0..=x1 {
            let (px, py) = (x as f64 + 0.5, y as f64 + 0.5);
            let inside = x < x1
                && if pie {
                    gdi_geom::on_arc(gdi_geom::angle(cx, cy, px, py), a0, a1)
                } else {
                    // left of the chord p1 -> p0 (y down): the side the arc is on
                    (p0.0 - p1.0) * (py - p1.1) - (p0.1 - p1.1) * (px - p1.0) <= 0.0
                };
            match (inside, run) {
                (true, None) => run = Some(x),
                (false, Some(a)) => {
                    out.push((y, a, x));
                    run = None;
                }
                _ => {}
            }
        }
    }
    out
}
fn Pie(c: &mut Ctx) -> u32 {
    let (s, e) = ((c.i(5), c.i(6)), (c.i(7), c.i(8)));
    shape(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), move |l, t, r, b| arc_area(l, t, r, b, s, e, true))
}
fn Chord(c: &mut Ctx) -> u32 {
    let (s, e) = ((c.i(5), c.i(6)), (c.i(7), c.i(8)));
    shape(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), move |l, t, r, b| arc_area(l, t, r, b, s, e, false))
}
fn arc(dc: u32, b: (i32, i32, i32, i32), s: (i32, i32), e: (i32, i32)) -> u32 {
    let (l, t, r, bo) = b;
    draw(dc, |tt, st, cv| {
        let Some(pen) = pen_of(tt, st) else { return Rect::default() };
        let (cx, cy) = ((l + r) as f64 / 2.0, (t + bo) as f64 / 2.0);
        let (a0, a1) = (gdi_geom::angle(cx, cy, s.0 as f64, s.1 as f64), gdi_geom::angle(cx, cy, e.0 as f64, e.1 as f64));
        let (edge, _) = gdi_geom::outline(&gdi_geom::ellipse_spans(l, t, r, bo));
        let pts: Vec<(i32, i32)> = edge.into_iter().filter(|&(x, y)| gdi_geom::on_arc(gdi_geom::angle(cx, cy, x as f64 + 0.5, y as f64 + 0.5), a0, a1)).collect();
        let paths: Vec<Vec<(i32, i32)>> = pts.iter().map(|&p| vec![p]).collect();
        if pen.width > 1 {
            return stroke(cv, st, pen, &paths, false);
        }
        let mut d = Vec::new();
        for (x, y) in pts {
            let (x, y) = cv.dev(x, y);
            if plot(cv, x, y, pen.col, st.rop2) {
                d.push((x, y));
            }
        }
        pts_rect(&d)
    }) as u32
}
fn Arc(c: &mut Ctx) -> u32 {
    arc(c.a(0), (c.i(1), c.i(2), c.i(3), c.i(4)), (c.i(5), c.i(6)), (c.i(7), c.i(8)))
}
fn ArcTo(c: &mut Ctx) -> u32 {
    let dc = c.a(0);
    let r = arc(dc, (c.i(1), c.i(2), c.i(3), c.i(4)), (c.i(5), c.i(6)), (c.i(7), c.i(8)));
    with_dc(dc, |d| d.s.pos = (c.i(7), c.i(8)));
    r
}
fn polygons(dc: u32, polys: Vec<Vec<(i32, i32)>>) -> u32 {
    draw(dc, |t, s, cv| {
        let pat = pattern(t, s, s.brush);
        let dev: Vec<Vec<(i32, i32)>> = polys.iter().map(|p| p.iter().map(|&(x, y)| cv.dev(x, y)).collect()).collect();
        let sp = gdi_geom::poly_spans(&dev, s.fill_mode == 2);
        let mut d = fill_spans(cv, &sp, &pat, s.rop2);
        if let Some(pen) = pen_of(t, s) {
            d = d.or(&stroke(cv, s, pen, &polys, true));
        }
        d
    }) as u32
}
fn Polygon(c: &mut Ctx) -> u32 {
    polygons(c.a(0), vec![read_pts(c.a(1), c.a(2))])
}
fn PolyPolygon(c: &mut Ctx) -> u32 {
    let (dc, p, counts, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut at = p;
    let mut v = Vec::new();
    for i in 0..n {
        let k = r32(counts + 4 * i);
        v.push(read_pts(at, k));
        at += 8 * k;
    }
    polygons(dc, v)
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
            *x = gdi_geom::normalize(&r);
        }
    });
    k
}

fn CreateRectRgn(c: &mut Ctx) -> u32 {
    let (l, t, r, b) = (c.i(0).min(c.i(2)), c.i(1).min(c.i(3)), c.i(0).max(c.i(2)), c.i(1).max(c.i(3)));
    add(G::Region(vec![Rect { l, t, r, b }].into_iter().filter(|r| !r.empty()).collect()))
}
fn CreateRectRgnIndirect(c: &mut Ctx) -> u32 {
    add(G::Region(vec![Rect::read(c.a(0))].into_iter().filter(|r| !r.empty()).collect()))
}
/// Elliptic and round regions cover the box less its right column and bottom row.
fn CreateRoundRectRgn(c: &mut Ctx) -> u32 {
    let (l, t, r, b) = (c.i(0).min(c.i(2)), c.i(1).min(c.i(3)), c.i(0).max(c.i(2)), c.i(1).max(c.i(3)));
    add(G::Region(gdi_geom::spans_region(&gdi_geom::round_rect_spans(l, t, r - 1, b - 1, c.i(4), c.i(5)))))
}
fn CreateEllipticRgn(c: &mut Ctx) -> u32 {
    let (l, t, r, b) = (c.i(0).min(c.i(2)), c.i(1).min(c.i(3)), c.i(0).max(c.i(2)), c.i(1).max(c.i(3)));
    add(G::Region(gdi_geom::spans_region(&gdi_geom::ellipse_spans(l, t, r - 1, b - 1))))
}
fn CreateEllipticRgnIndirect(c: &mut Ctx) -> u32 {
    let r = Rect::read(c.a(0));
    let (l, t, rr, b) = (r.l.min(r.r), r.t.min(r.b), r.l.max(r.r), r.t.max(r.b));
    add(G::Region(gdi_geom::spans_region(&gdi_geom::ellipse_spans(l, t, rr - 1, b - 1))))
}
fn CreatePolygonRgn(c: &mut Ctx) -> u32 {
    let pts = read_pts(c.a(0), c.a(1));
    add(G::Region(gdi_geom::spans_region(&gdi_geom::poly_spans(&[pts], c.a(2) == 2))))
}
fn CreatePolyPolygonRgn(c: &mut Ctx) -> u32 {
    let (p, counts, n) = (c.a(0), c.a(1), c.a(2));
    let mut at = p;
    let mut v = Vec::new();
    for i in 0..n {
        let k = r32(counts + 4 * i);
        v.push(read_pts(at, k));
        at += 8 * k;
    }
    add(G::Region(gdi_geom::spans_region(&gdi_geom::poly_spans(&v, c.a(3) == 2))))
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
        1..=4 => gdi_geom::combine(&ra, &rb, mode),
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
    match (region(c.a(0)), region(c.a(1))) {
        (Some(a), Some(b)) => (gdi_geom::normalize(&a) == gdi_geom::normalize(&b)) as u32,
        _ => 0,
    }
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
/// The region's border w x h pixels thick: the region less itself shifted by +-w, +-h.
fn FrameRgn(c: &mut Ctx) -> u32 {
    let (dc, h, b, w, hh) = (c.a(0), c.a(1), c.a(2), c.i(3), c.i(4));
    let Some(r) = region(h) else { return 0 };
    let mut inner = r.clone();
    for (dx, dy) in [(w, 0), (-w, 0), (0, hh), (0, -hh)] {
        let moved: Vec<Rect> = r.iter().map(|q| q.off(dx, dy)).collect();
        inner = gdi_geom::combine(&inner, &moved, 1);
    }
    for q in gdi_geom::combine(&r, &inner, 4) {
        fill_rect_brush(dc, q, b);
    }
    1
}
fn InvertRgn(c: &mut Ctx) -> u32 {
    let (dc, h) = (c.a(0), c.a(1));
    let Some(r) = region(h) else { return 0 };
    draw(dc, |_, _, cv| {
        let mut d = Rect::default();
        for q in &r {
            d = d.or(&pat_blt(cv, cv.rect(*q), &Pat::Solid(0), 0x55));
        }
        d
    }) as u32
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
    let lf = read_logfont(c.a(0), true);
    if crate::env_on!("SAKE_TRACE_FONT") {
        eprintln!("sake font: {} h {} w {} weight {} q {}", lf.face, lf.height, lf.width, lf.weight, lf.quality);
    }
    add(G::Font(lf))
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
    if crate::env_on!("SAKE_TRACE_FONT") {
        eprintln!("sake font: tm face {} h {} asc {} em {}", face_name(r.face), r32(c.a(1)), r32(c.a(1) + 4), r.em);
    }
    1
}
fn GetTextFaceA(c: &mut Ctx) -> u32 {
    let (dc, n, p) = (c.a(0), c.a(1), c.a(2));
    let Some((r, _, _)) = real_of(dc) else { return 0 };
    let name = crate::text::s2a(&face_name(r.face));
    if p == 0 {
        return name.len() as u32 + 1;
    }
    mem::put_cstr(p, n, &name)
}
fn GetTextFaceW(c: &mut Ctx) -> u32 {
    let (dc, n, p) = (c.a(0), c.a(1), c.a(2));
    let Some((r, _, _)) = real_of(dc) else { return 0 };
    let name: Vec<u16> = face_name(r.face).encode_utf16().collect();
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

/// A negative character count: the text functions fail (ERROR_INVALID_PARAMETER)
/// A text count no call can have: negative, or more than the guest's memory holds.
fn bad_count(n: u32) -> bool {
    if (n as i32) < 0 || n > 1 << 28 {
        crate::k32::last_error(87);
        return true;
    }
    false
}
fn ansi_chars(p: u32, n: u32) -> Vec<u16> {
    a2w(&mem::cstrn(p, n))
}
fn wide_chars(p: u32, n: u32) -> Vec<u16> {
    (0..if bad_count(n) { 0 } else { n }).map(|i| r16(p + 2 * i)).collect()
}

fn extent(c: &mut Ctx, s: &[u16], out: u32) -> u32 {
    let Some((r, _, st)) = real_of(c.a(0)) else { return 0 };
    let w = text_width(&r, s, st.char_extra);
    put_pt(out, (w, r.ascent + r.descent));
    if crate::env_on!("SAKE_TRACE_FONT") {
        eprintln!("sake font: extent {:?} em {} -> {w}x{}", String::from_utf16_lossy(s), r.em, r.ascent + r.descent);
    }
    1
}
fn GetTextExtentPoint32A(c: &mut Ctx) -> u32 {
    if bad_count(c.a(2)) {
        return 0;
    }
    let s = ansi_chars(c.a(1), c.a(2));
    extent(c, &s, c.a(3))
}
fn GetTextExtentPointA(c: &mut Ctx) -> u32 {
    GetTextExtentPoint32A(c)
}
fn GetTextExtentPoint32W(c: &mut Ctx) -> u32 {
    if bad_count(c.a(2)) {
        return 0;
    }
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
    if bad_count(c.a(2)) {
        return 0;
    }
    let s = ansi_chars(c.a(1), c.a(2));
    ext_ex(c, &s)
}
fn GetTextExtentExPointW(c: &mut Ctx) -> u32 {
    if bad_count(c.a(2)) {
        return 0;
    }
    let s = wide_chars(c.a(1), c.a(2));
    ext_ex(c, &s)
}
/// GetCharWidth / GetCharABCWidths (`float`: the Float variants, FLOATs instead of ints)
fn char_widths(c: &mut Ctx, wide: bool, abc: bool) -> u32 {
    char_widths_as(c, wide, abc, false)
}
fn char_widths_as(c: &mut Ctx, wide: bool, abc: bool, float: bool) -> u32 {
    let (dc, first, last, p) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((r, _, _)) = real_of(dc) else { return 0 };
    for (i, code) in (first..=last).enumerate() {
        let ch = if wide { code } else { mbcs_char(code) };
        let g = glyph(&r, ch);
        if float {
            let f = |v: i32| (v as f32).to_bits();
            if abc {
                let a = p + 12 * i as u32;
                w32(a, f(g.x0));
                w32(a + 4, f(g.w.max(0)));
                w32(a + 8, f(g.adv - g.x0 - g.w));
            } else {
                w32(p + 4 * i as u32, f(g.adv));
            }
        } else if abc {
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
fn GetCharABCWidthsFloatA(c: &mut Ctx) -> u32 {
    char_widths_as(c, false, true, true)
}
fn GetCharABCWidthsFloatW(c: &mut Ctx) -> u32 {
    char_widths_as(c, true, true, true)
}
fn GetCharWidthFloatA(c: &mut Ctx) -> u32 {
    char_widths_as(c, false, false, true)
}
fn GetCharWidthFloatW(c: &mut Ctx) -> u32 {
    char_widths_as(c, true, false, true)
}

/// A code from the A functions (one byte, or lead<<8|trail) as a UTF-16 unit.
fn mbcs_char(code: u32) -> u32 {
    let b: Vec<u8> = if code > 0xff { vec![(code >> 8) as u8, code as u8] } else { vec![code as u8] };
    mb2w(0, &b).first().copied().unwrap_or(b'?' as u16) as u32
}

fn glyph_outline(c: &mut Ctx, wide: bool) -> u32 {
    let (dc, code, fmt, gm, size, buf) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let Some((mut r, _, _)) = real_of(dc) else { return 0xffff_ffff };
    // MAT2: scaling only (eM11, eM22 as 16.16 fixed; shear and rotation are not done)
    let mat = c.a(6);
    if mat != 0 {
        let fixed = |p: u32| r32(p) as i32 as f32 / 65536.0;
        let (sx, sy) = (fixed(mat), fixed(mat + 12));
        let s = if sy != 0.0 { sy.abs() } else { sx.abs() };
        if s > 0.0 && (s - 1.0).abs() > 1e-3 {
            r.em *= s;
            r.ascent = (r.ascent as f32 * s).round() as i32;
            r.descent = (r.descent as f32 * s).round() as i32;
        }
    }
    let ch = if fmt & 0x80 != 0 { code & 0xffff | GLYPH_INDEX } else if wide { code } else { mbcs_char(code) };
    let g = glyph(&r, ch);
    if crate::env_on!("SAKE_TRACE_FONT") {
        eprintln!("sake font: glyph {ch:#x} fmt {fmt} em {} mat {mat:#x} -> {}x{} at {},{} adv {}", r.em, g.w, g.h, g.x0, g.top, g.adv);
    }
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
/// GetCharABCWidthsI / GetCharWidthI: by glyph index (`pgi`, or `first`.. when null)
fn widths_i(c: &mut Ctx, abc: bool) -> u32 {
    let (dc, first, n, pgi, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((r, _, _)) = real_of(dc) else { return 0 };
    for i in 0..n {
        let gi = if pgi != 0 { r16(pgi + 2 * i) as u32 } else { first + i };
        let g = glyph(&r, gi & 0xffff | GLYPH_INDEX);
        if abc {
            let a = out + 12 * i;
            w32(a, g.x0 as u32);
            w32(a + 4, g.w.max(0) as u32);
            w32(a + 8, (g.adv - g.x0 - g.w) as u32);
        } else {
            w32(out + 4 * i, g.adv as u32);
        }
    }
    1
}
/// GLYPHSET: one range over the whole BMP (the font's real coverage is not looked up)
fn GetFontUnicodeRanges(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    if p != 0 {
        w32(p, 20);
        w32(p + 4, 0);
        w32(p + 8, 0xffff);
        w32(p + 12, 1);
        w16(p + 16, 0);
        w16(p + 18, 0xffff);
    }
    20
}
fn GetCharABCWidthsI(c: &mut Ctx) -> u32 {
    widths_i(c, true)
}
fn GetCharWidthI(c: &mut Ctx) -> u32 {
    widths_i(c, false)
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
/// GetFontData(dc, table, offset, buf, size): the selected font's file (table 0, or 'ttcf': the
/// font as one face) or one of its tables (the tag's bytes as a DWORD); size asked with no buffer.
/// (NonColor's text renderer feeds it to its own FreeType.)
fn GetFontData(c: &mut Ctx) -> u32 {
    let (dc, table, off, buf, cap) = (c.a(0), c.a(1), c.a(2) as usize, c.a(3), c.a(4) as usize);
    let Some((r, _, _)) = real_of(dc) else { return 0xffff_ffff };
    let d = r.font.as_slice();
    let be32 = |o: usize| d.get(o..o + 4).map(|b| u32::from_be_bytes(b.try_into().unwrap()));
    let part = if table == 0 || table == u32::from_le_bytes(*b"ttcf") {
        Some(d)
    } else {
        let n = d.get(4..6).map_or(0, |b| u16::from_be_bytes([b[0], b[1]]) as usize);
        let tag = u32::from_be_bytes(table.to_le_bytes());
        (0..n).find(|i| be32(12 + 16 * i) == Some(tag)).and_then(|i| {
            let (o, l) = (be32(12 + 16 * i + 8)? as usize, be32(12 + 16 * i + 12)? as usize);
            d.get(o..o + l)
        })
    };
    let Some(part) = part else { return 0xffff_ffff };
    let Some(rest) = part.get(off..) else { return 0xffff_ffff };
    if buf == 0 || cap == 0 {
        return rest.len() as u32;
    }
    let n = rest.len().min(cap);
    mem::write(buf, &rest[..n]);
    n as u32
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
/// OUTLINETEXTMETRIC: the TEXTMETRIC, then the outline numbers, then the family, face, style
/// and full names (their offsets from the struct in the otmp* fields). Size asked with p == 0.
fn outline_metrics(c: &mut Ctx, wide: bool) -> u32 {
    let (dc, cap, p) = (c.a(0), c.a(1), c.a(2));
    let Some((r, lf, _)) = real_of(dc) else { return 0 };
    let face = face_name(r.face);
    let enc = |s: &str| -> Vec<u8> {
        if wide {
            s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect()
        } else {
            let mut v = crate::text::s2a(s);
            v.push(0);
            v
        }
    };
    let names = [enc(&face), enc(&face), enc(if r.bold { "Bold" } else { "Regular" }), enc(&face)];
    // (TEXTMETRICA is 4 bytes shorter: its characters are bytes)
    let d = if wide { 0 } else { 4u32 };
    let base = 216 - d;
    let size = base + names.iter().map(|n| n.len() as u32).sum::<u32>();
    if p == 0 {
        return size;
    }
    if cap < base {
        return 0;
    }
    let mut buf = vec![0u8; size as usize];
    let put = |b: &mut Vec<u8>, off: u32, v: i32| b[(off - d) as usize..(off - d + 4) as usize].copy_from_slice(&v.to_le_bytes());
    buf[0..4].copy_from_slice(&size.to_le_bytes());
    let em = r.em.round() as i32;
    let (asc, desc) = (r.ascent, r.descent);
    put(&mut buf, 84, 1); // slope rise
    put(&mut buf, 96, em); // em square (in pixels: the font is realized at this size)
    put(&mut buf, 100, asc);
    put(&mut buf, 104, -desc);
    put(&mut buf, 108, (asc + desc - em).max(0));
    put(&mut buf, 112, asc * 7 / 10);
    put(&mut buf, 116, asc / 2);
    for (i, v) in [0, asc, em, -desc].into_iter().enumerate() {
        put(&mut buf, 120 + 4 * i as u32, v);
    }
    put(&mut buf, 136, asc);
    put(&mut buf, 140, -desc);
    put(&mut buf, 148, 9);
    for (o, v) in [(152, em / 2), (156, em / 2), (164, desc / 2), (168, em / 2), (172, em / 2), (180, asc / 2)] {
        put(&mut buf, o, v);
    }
    put(&mut buf, 184, (em / 20).max(1));
    put(&mut buf, 188, asc / 3);
    put(&mut buf, 192, -(em / 20).max(1));
    put(&mut buf, 196, -(desc / 2).max(1));
    let mut off = base;
    for (i, n) in names.iter().enumerate() {
        put(&mut buf, 200 + 4 * i as u32, off as i32);
        buf[off as usize..off as usize + n.len()].copy_from_slice(n);
        off += n.len() as u32;
    }
    let n = size.min(cap);
    mem::write(p, &buf[..n as usize]);
    write_tm(p + 4, &r, &lf, wide);
    n
}
fn GetOutlineTextMetricsA(c: &mut Ctx) -> u32 {
    outline_metrics(c, false)
}
fn GetOutlineTextMetricsW(c: &mut Ctx) -> u32 {
    outline_metrics(c, true)
}
fn add_font_file(name: &str) -> u32 {
    let n = match crate::files::resolve(name).and_then(|h| kura::fs::read(h).ok()) {
        // a collection: each face
        Some(data) if data.starts_with(b"ttcf") => (0..).map_while(|i| ttc_face(&data, i)).map(add_font).sum(),
        Some(data) => add_font(data),
        None => 0,
    };
    if crate::env_on!("SAKE_TRACE_FONT") {
        eprintln!("sake: AddFontResource({name}) = {n}");
    }
    n
}
fn AddFontResourceA(c: &mut Ctx) -> u32 {
    add_font_file(&crate::k32::arg_str(c.a(0), false))
}
fn AddFontResourceW(c: &mut Ctx) -> u32 {
    add_font_file(&crate::k32::arg_str(c.a(0), true))
}
fn AddFontResourceExA(c: &mut Ctx) -> u32 {
    add_font_file(&crate::k32::arg_str(c.a(0), false))
}
fn AddFontResourceExW(c: &mut Ctx) -> u32 {
    add_font_file(&crate::k32::arg_str(c.a(0), true))
}
fn RemoveFontResourceA(_: &mut Ctx) -> u32 {
    1
}
fn RemoveFontResourceExA(_: &mut Ctx) -> u32 {
    1
}
fn RemoveFontResourceW(_: &mut Ctx) -> u32 {
    1
}
fn RemoveFontResourceExW(_: &mut Ctx) -> u32 {
    1
}
fn AddFontMemResourceEx(c: &mut Ctx) -> u32 {
    add_font(mem::slice(c.a(0), c.a(1)).to_vec());
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
    if bad_count(n) {
        return 0;
    }
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

/// lfPitchAndFamily of an enumerated face: FIXED_PITCH or VARIABLE_PITCH, with FF_ROMAN for
/// the serif ones, else FF_MODERN (monospaced) or FF_SWISS, so face_index picks it back.
fn face_pitch(fixed: bool, roman: bool) -> u8 {
    match (fixed, roman) {
        (true, true) => 0x11,
        (true, false) => 0x31,
        (false, true) => 0x12,
        (false, false) => 0x22,
    }
}

/// Calls an EnumFonts-style callback for each face: (LOGFONT, TEXTMETRIC, type, lparam).
fn enum_fonts(c: &mut Ctx, filter: Option<String>, proc_: u32, lp: u32, wide: bool, ex: bool) -> u32 {
    let lfsize = if wide { 92 } else { 60 };
    let buf = mem::heap_alloc(1024, true);
    let tm = buf + 512;
    let mut ret = 1;
    // (name, lfPitchAndFamily as Windows reports it: games filter on FIXED_PITCH)
    let mut names: Vec<(String, u8)> = Vec::new();
    for f in FACES {
        let pitch = face_pitch(f.fixed, f.roman);
        for n in f.names {
            names.push(((*n).to_string(), pitch));
        }
        if f.names[0].starts_with('Ｍ') {
            names.push((format!("@{}", f.names[0]), pitch));
        }
    }
    // (a font the game added under any of its names: Siglus asks for the Japanese one)
    for (n, _) in ADDED.lock().unwrap().iter() {
        let hit = filter.as_ref().and_then(|fl| n.iter().find(|x| x.eq_ignore_ascii_case(fl)));
        names.push((hit.unwrap_or(&n[0]).clone(), face_pitch(false, false)));
    }
    for (name, pitch) in names {
        if let Some(fl) = &filter {
            if !fl.is_empty() && !fl.eq_ignore_ascii_case(&name) {
                continue;
            }
        }
        let lf = LogFont { height: 16, weight: 400, pitch, ..stock_font(&name, 16) };
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
            w32(tm + tmsize + 12, r.ave as u32);
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
        BitBlt, StretchBlt, PatBlt, MaskBlt, GdiTransparentBlt, GdiAlphaBlend, GdiGradientFill, StretchDIBits, SetDIBitsToDevice,
        GetPixel, SetPixel, SetPixelV, LineTo, Polyline, PolylineTo, Rectangle, Ellipse, RoundRect, Polygon,
        PolyPolyline, PolyBezier, PolyBezierTo, Pie, Chord, Arc, ArcTo, PolyPolygon, SetPolyFillMode, GetPolyFillMode,
        CreateRectRgn, CreateRectRgnIndirect, CreateRoundRectRgn, CreateEllipticRgn, CreatePolygonRgn,
        CreateEllipticRgnIndirect, CreatePolyPolygonRgn,
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
        GetCharWidth32W, GetCharABCWidthsA, GetCharABCWidthsW, GetCharABCWidthsFloatA, GetCharABCWidthsFloatW, GetCharABCWidthsI, GetCharWidthI, GetFontUnicodeRanges, GetCharWidthFloatA, GetCharWidthFloatW, GetGlyphOutlineA, GetGlyphOutlineW,
        GetGlyphIndicesW, GetFontData, GetKerningPairsA, GetKerningPairsW, GetFontLanguageInfo,
        GetOutlineTextMetricsA, GetOutlineTextMetricsW, AddFontResourceA, AddFontResourceW, AddFontResourceExA,
        AddFontResourceExW, RemoveFontResourceA, RemoveFontResourceExA, RemoveFontResourceW, RemoveFontResourceExW, AddFontMemResourceEx,
        RemoveFontMemResourceEx, TextOutA, TextOutW, ExtTextOutA, ExtTextOutW,
        EnumFontsA, EnumFontsW, EnumFontFamiliesA, EnumFontFamiliesW, EnumFontFamiliesExA, EnumFontFamiliesExW,
        GdiFlush, GdiSetBatchLimit, GetPixelFormat, SetICMMode,
    );
    reg!("msimg32"; TransparentBlt, AlphaBlend, GradientFill);
    reg!("msvfw32"; DrawDibOpen, DrawDibClose, DrawDibBegin, DrawDibEnd, DrawDibRealize, DrawDibSetPalette,
        DrawDibStart, DrawDibStop, DrawDibDraw);
}

#[cfg(test)]
mod glyph_cache_tests {
    use super::*;

    /// enumerated faces carry Windows' lfPitchAndFamily: games keep only FIXED_PITCH ones for
    /// some lists (NonColor's font menu), and the pitch picks the same face back in face_index
    #[test]
    fn enumerated_pitch() {
        for (i, f) in FACES.iter().enumerate() {
            let p = face_pitch(f.fixed, f.roman);
            assert_eq!(p & 3, if f.fixed { 1 } else { 2 }, "{}", f.names[1.min(f.names.len() - 1)]);
            let lf = LogFont { pitch: p, ..stock_font("no such face", 16) };
            if i < 4 {
                assert_eq!(face_index(&lf), i);
            }
        }
    }

    fn g(n: usize) -> Arc<Glyph> {
        Arc::new(Glyph { x0: 0, top: 0, w: 0, h: 0, cov: vec![0; n], adv: 0 })
    }

    /// Random gets and puts (sizes, keys again): `bytes` is what the map holds, the cache
    /// stays within its limit after a trim, and a glyph used since the last put of the others
    /// survives the trim.
    #[test]
    fn glyph_cache_is_bounded() {
        let mut seed = 0x2545f4914f6cdd1du64;
        let mut rnd = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        for _ in 0..200 {
            let limit = 2000 + rnd(20000) as usize;
            let mut c = Glyphs::default();
            for _ in 0..500 {
                let k: GlyphKey = (0, 0, rnd(60) as u32, false);
                if rnd(2) == 0 {
                    c.get(&k);
                } else {
                    let hot: GlyphKey = (1, 0, 0, false);
                    if c.map.contains_key(&hot) {
                        c.get(&hot);
                    }
                    c.put(k, g(rnd(800) as usize), limit);
                    if k != hot && c.map.len() > 2 && rnd(4) == 0 {
                        c.put(hot, g(10), limit);
                    }
                }
                let sum: usize = c.map.values().map(|e| Glyphs::size(&e.0)).sum();
                assert_eq!(c.bytes, sum);
                assert!(c.bytes <= limit);
            }
        }
    }

    #[test]
    fn glyph_cache_keeps_recently_used() {
        let mut c = Glyphs::default();
        for i in 0..10 {
            c.put((0, 0, i, false), g(100), 10_000);
        }
        c.get(&(0, 0, 0, false));
        c.put((0, 0, 99, false), g(100), 1640);
        assert!(c.map.contains_key(&(0, 0, 0, false)));
        assert!(c.map.contains_key(&(0, 0, 99, false)));
        assert!(!c.map.contains_key(&(0, 0, 1, false)));
        assert!(c.bytes <= 1640);
    }
}
