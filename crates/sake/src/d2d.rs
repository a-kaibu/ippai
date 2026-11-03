//! Direct2D on D3D11 textures: device contexts draw on the CPU (text with dwrite.rs's layout and
//! gdi.rs's glyphs, solid rectangles) into a premultiplied BGRA layer of the target's size; at
//! EndDraw (or Flush) the changed rectangle goes over the target texture on the GPU
//! (vnhost::d3d11::d2d::composite). Clear clears the texture itself.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx, Handler};
use crate::com;
use crate::dwrite;
use crate::mem::{self, r32, w32};
use std::collections::HashMap;
use std::sync::Mutex;
use vnhost::d3d11::d2d as gpu2d;

type Xf = [f32; 6];
const IDENTITY: Xf = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Default)]
struct Dc {
    device: u32,
    /// the target bitmap object and its texture (surface address), size in pixels
    target: u32,
    surface: u32,
    w: u32,
    h: u32,
    /// premultiplied BGRA, w * h * 4 (empty until drawn on)
    layer: Vec<u8>,
    /// the changed pixels [x0, y0, x1, y1)
    dirty: Option<[u32; 4]>,
    xf: Option<Xf>,
    clips: Vec<[f32; 4]>,
}

struct Brush {
    color: [f32; 4],
    opacity: f32,
    /// a linear gradient: start, end, stops (position, color)
    linear: Option<([f32; 2], [f32; 2], Vec<(f32, [f32; 4])>)>,
}

/// What a brush paints at a device pixel center (straight RGBA, opacity applied).
#[derive(Clone)]
enum Paint {
    Solid([f32; 4]),
    Linear { a: [f32; 2], d: [f32; 2], len2: f32, stops: Vec<(f32, [f32; 4])>, opacity: f32 },
}

impl Paint {
    fn at(&self, x: f32, y: f32) -> [f32; 4] {
        match self {
            Paint::Solid(c) => *c,
            Paint::Linear { a, d, len2, stops, opacity } => {
                let t = if *len2 > 0.0 { ((x - a[0]) * d[0] + (y - a[1]) * d[1]) / len2 } else { 0.0 };
                let mut c = stops.first().map_or([0.0; 4], |s| s.1);
                for w in stops.windows(2) {
                    let ((p0, c0), (p1, c1)) = (w[0], w[1]);
                    if t >= p0 {
                        c = if t >= p1 { c1 } else {
                            let k = (t - p0) / (p1 - p0).max(1e-6);
                            [0, 1, 2, 3].map(|i| c0[i] + (c1[i] - c0[i]) * k)
                        };
                    }
                }
                [c[0], c[1], c[2], c[3] * opacity]
            }
        }
    }
}

#[derive(Default)]
struct State {
    dcs: HashMap<u32, Dc>,
    brushes: HashMap<u32, Brush>,
    /// bitmap object -> (surface, w, h)
    bitmaps: HashMap<u32, (u32, u32, u32)>,
    stops: HashMap<u32, Vec<(f32, [f32; 4])>>,
    /// device -> factory, context/device -> device
    parents: HashMap<u32, u32>,
}

static S: Mutex<Option<State>> = Mutex::new(None);

fn st<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(S.lock().unwrap().get_or_insert_with(State::default))
}

fn fl(c: &Ctx, i: u32) -> f32 {
    f32::from_bits(c.a(i))
}
fn rf(p: u32, i: u32) -> f32 {
    f32::from_bits(r32(p + 4 * i))
}
fn ok(_: &mut Ctx) -> u32 {
    0
}

// ---- factory, device ----

const FACTORY: &str = "ID2D1Factory7";
const DEVICE: &str = "ID2D1Device6";
const CONTEXT: &str = "ID2D1DeviceContext6";

fn factory() -> u32 {
    com::object(
        FACTORY,
        &[
            ("CreateDevice", CreateDevice as Handler),
            ("CreateDxgiSurfaceRenderTarget", CreateDxgiSurfaceRenderTarget),
            ("GetDesktopDpi", GetDpi),
            ("ReloadSystemMetrics", ok),
        ],
        8,
    )
}

fn D2D1CreateFactory(c: &mut Ctx) -> u32 {
    mem::putp(c.a(3), factory());
    0
}

fn new_device(factory: u32, out: u32) -> u32 {
    let d = com::object(
        DEVICE,
        &[("CreateDeviceContext", CreateDeviceContext as Handler), ("GetFactory", GetFactory), ("ClearResources", ok), ("SetMaximumTextureMemory", ok)],
        8,
    );
    st(|s| s.parents.insert(d, factory));
    mem::putp(out, d);
    0
}

fn CreateDevice(c: &mut Ctx) -> u32 {
    new_device(c.a(0), c.a(2))
}

fn D2D1CreateDevice(c: &mut Ctx) -> u32 {
    new_device(factory(), c.a(2))
}

fn GetFactory(c: &mut Ctx) -> u32 {
    let mut f = st(|s| s.parents.get(&c.a(0)).copied().unwrap_or(0));
    // (a context's factory is its device's)
    if com::is(f, DEVICE) {
        f = st(|s| s.parents.get(&f).copied().unwrap_or(0));
    }
    if f != 0 {
        com::add_ref_obj(f);
    }
    mem::putp(c.a(1), f);
    0
}

fn context_methods() -> Vec<(&'static str, Handler)> {
    vec![
        ("CreateSolidColorBrush", CreateSolidColorBrush),
        ("CreateBitmapFromDxgiSurface", CreateBitmapFromDxgiSurface),
        ("SetTarget", SetTarget),
        ("GetTarget", GetTarget),
        ("BeginDraw", ok),
        ("EndDraw", EndDraw),
        ("Flush", EndDraw),
        ("Clear", Clear),
        ("SetTransform", SetTransform),
        ("GetTransform", GetTransform),
        ("FillRectangle", FillRectangle),
        ("FillEllipse", FillEllipse),
        ("DrawEllipse", DrawEllipse),
        ("CreateGradientStopCollection", CreateGradientStopCollection),
        ("CreateLinearGradientBrush", CreateLinearGradientBrush),
        ("DrawRectangle", DrawRectangle),
        ("DrawText", DrawText),
        ("DrawTextLayout", DrawTextLayout),
        ("PushAxisAlignedClip", PushAxisAlignedClip),
        ("PopAxisAlignedClip", PopAxisAlignedClip),
        ("SetAntialiasMode", ok),
        ("SetTextAntialiasMode", ok),
        ("SetTextRenderingParams", ok),
        ("SetDpi", ok),
        ("SetUnitMode", ok),
        ("SetPrimitiveBlend", ok),
        ("SetTags", ok),
        ("GetAntialiasMode", zero),
        ("GetTextAntialiasMode", zero),
        ("GetDpi", GetDpi),
        ("GetSize", GetSize),
        ("GetPixelSize", GetPixelSize),
        ("GetDevice", GetDevice),
        ("GetFactory", GetFactory),
    ]
}

fn zero(_: &mut Ctx) -> u32 {
    0
}

fn CreateDeviceContext(c: &mut Ctx) -> u32 {
    let o = com::object(CONTEXT, &context_methods(), 8);
    let device = c.a(0);
    st(|s| {
        s.parents.insert(o, device);
        s.dcs.insert(o, Dc { device, ..Default::default() })
    });
    mem::putp(c.a(2), o);
    0
}

fn D2D1CreateDeviceContext(c: &mut Ctx) -> u32 {
    // (dxgi surface, properties, out): a device and a context targeting the surface
    let dev_out = mem::heap_alloc(8, true);
    new_device(factory(), dev_out);
    let device = mem::rp(dev_out);
    mem::heap_free(dev_out);
    let o = com::object(CONTEXT, &context_methods(), 8);
    let surface = c.a(0);
    let bmp = new_bitmap(surface);
    st(|s| {
        s.parents.insert(o, device);
        s.dcs.insert(o, Dc { device, ..Default::default() })
    });
    set_target(o, bmp);
    mem::putp(c.a(2), o);
    0
}

fn CreateDxgiSurfaceRenderTarget(c: &mut Ctx) -> u32 {
    // (this, surface, properties, out): a context of a device of this factory
    let dev_out = mem::heap_alloc(8, true);
    new_device(c.a(0), dev_out);
    let device = mem::rp(dev_out);
    mem::heap_free(dev_out);
    let o = com::object(CONTEXT, &context_methods(), 8);
    let bmp = new_bitmap(c.a(1));
    st(|s| {
        s.parents.insert(o, device);
        s.dcs.insert(o, Dc { device, ..Default::default() })
    });
    set_target(o, bmp);
    mem::putp(c.a(3), o);
    0
}

fn GetDevice(c: &mut Ctx) -> u32 {
    let d = st(|s| s.dcs.get(&c.a(0)).map_or(0, |d| d.device));
    if d != 0 {
        com::add_ref_obj(d);
    }
    mem::putp(c.a(1), d);
    0
}

fn GetDpi(c: &mut Ctx) -> u32 {
    for p in [c.a(1), c.a(2)] {
        if p != 0 {
            w32(p, 96f32.to_bits());
        }
    }
    0
}

// ---- resources ----

fn CreateSolidColorBrush(c: &mut Ctx) -> u32 {
    // (this, color, properties, out)
    let (col, props) = (c.a(1), c.a(2));
    let color = [rf(col, 0), rf(col, 1), rf(col, 2), rf(col, 3)];
    let opacity = if props != 0 { rf(props, 0) } else { 1.0 };
    let o = com::object(
        "ID2D1SolidColorBrush",
        &[("SetColor", SetColor as Handler), ("GetColor", GetColor), ("SetOpacity", SetOpacity), ("GetOpacity", GetOpacity), ("SetTransform", ok), ("GetTransform", GetIdentity)],
        8,
    );
    st(|s| s.brushes.insert(o, Brush { color, opacity, linear: None }));
    mem::putp(c.a(3), o);
    0
}
fn SetColor(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let color = [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3)];
    st(|s| s.brushes.get_mut(&c.a(0)).map(|b| b.color = color));
    0
}
fn GetColor(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let color = st(|s| s.brushes.get(&c.a(0)).map_or([0.0; 4], |b| b.color));
    for (i, v) in color.iter().enumerate() {
        w32(out + 4 * i as u32, v.to_bits());
    }
    out
}
fn SetOpacity(c: &mut Ctx) -> u32 {
    let v = fl(c, 1);
    st(|s| s.brushes.get_mut(&c.a(0)).map(|b| b.opacity = v));
    0
}
fn GetOpacity(c: &mut Ctx) -> u32 {
    c.st0 = Some(st(|s| s.brushes.get(&c.a(0)).map_or(1.0, |b| b.opacity)) as f64);
    0
}
fn GetIdentity(c: &mut Ctx) -> u32 {
    for (i, v) in IDENTITY.iter().enumerate() {
        w32(c.a(1) + 4 * i as u32, v.to_bits());
    }
    0
}

fn new_bitmap(surface: u32) -> u32 {
    let (w, h) = gpu2d::size(surface).unwrap_or((0, 0));
    let o = com::object(
        "ID2D1Bitmap1",
        &[("GetSize", BitmapSize as Handler), ("GetPixelSize", BitmapPixelSize), ("GetDpi", GetDpi)],
        8,
    );
    st(|s| s.bitmaps.insert(o, (surface, w, h)));
    o
}

fn CreateBitmapFromDxgiSurface(c: &mut Ctx) -> u32 {
    // (this, surface, properties, out)
    if gpu2d::size(c.a(1)).is_none() {
        eprintln!("sake: d2d CreateBitmapFromDxgiSurface {:#x}: not a texture", c.a(1));
    }
    mem::putp(c.a(3), new_bitmap(c.a(1)));
    0
}
fn BitmapSize(c: &mut Ctx) -> u32 {
    let (_, w, h) = st(|s| s.bitmaps.get(&c.a(0)).copied().unwrap_or_default());
    w32(c.a(1), (w as f32).to_bits());
    w32(c.a(1) + 4, (h as f32).to_bits());
    c.a(1)
}
fn BitmapPixelSize(c: &mut Ctx) -> u32 {
    let (_, w, h) = st(|s| s.bitmaps.get(&c.a(0)).copied().unwrap_or_default());
    w32(c.a(1), w);
    w32(c.a(1) + 4, h);
    c.a(1)
}

fn set_target(dc: u32, bmp: u32) {
    flush(dc);
    st(|s| {
        let (surface, w, h) = s.bitmaps.get(&bmp).copied().unwrap_or_default();
        if let Some(d) = s.dcs.get_mut(&dc) {
            (d.target, d.surface, d.w, d.h) = (bmp, surface, w, h);
            d.layer = Vec::new();
            d.dirty = None;
        }
    });
}
fn SetTarget(c: &mut Ctx) -> u32 {
    set_target(c.a(0), c.a(1));
    0
}
fn GetTarget(c: &mut Ctx) -> u32 {
    let t = st(|s| s.dcs.get(&c.a(0)).map_or(0, |d| d.target));
    if t != 0 {
        com::add_ref_obj(t);
    }
    mem::putp(c.a(1), t);
    0
}
fn GetSize(c: &mut Ctx) -> u32 {
    let (w, h) = st(|s| s.dcs.get(&c.a(0)).map_or((0, 0), |d| (d.w, d.h)));
    w32(c.a(1), (w as f32).to_bits());
    w32(c.a(1) + 4, (h as f32).to_bits());
    c.a(1)
}
fn GetPixelSize(c: &mut Ctx) -> u32 {
    let (w, h) = st(|s| s.dcs.get(&c.a(0)).map_or((0, 0), |d| (d.w, d.h)));
    w32(c.a(1), w);
    w32(c.a(1) + 4, h);
    c.a(1)
}

// ---- drawing ----

/// Sends the changed pixels to the target texture.
fn flush(dc: u32) {
    let job = st(|s| {
        let d = s.dcs.get_mut(&dc)?;
        let [x0, y0, x1, y1] = d.dirty.take()?;
        let (w, h) = (x1 - x0, y1 - y0);
        let mut px = Vec::with_capacity((w * h * 4) as usize);
        for y in y0..y1 {
            let a = ((y * d.w + x0) * 4) as usize;
            px.extend_from_slice(&d.layer[a..a + (w * 4) as usize]);
            d.layer[a..a + (w * 4) as usize].fill(0);
        }
        Some((d.surface, x0, y0, w, h, px))
    });
    if let Some((surface, x, y, w, h, px)) = job {
        gpu2d::composite(surface, x, y, w, h, &px);
    }
}

fn EndDraw(c: &mut Ctx) -> u32 {
    flush(c.a(0));
    for p in [c.a(1), c.a(2)] {
        if p != 0 {
            w32(p, 0);
            w32(p + 4, 0);
        }
    }
    0
}

fn Clear(c: &mut Ctx) -> u32 {
    let (dc, p) = (c.a(0), c.a(1));
    let color = if p != 0 { [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3)] } else { [0.0; 4] };
    let clipped = st(|s| s.dcs.get(&dc).is_some_and(|d| !d.clips.is_empty()));
    if clipped {
        // (inside a clip: painted over what is there, an approximation of replacing it)
        let r = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, [0.0, 0.0, d.w as f32, d.h as f32])));
        if let Some(r) = r {
            fill(dc, r, &Paint::Solid(color));
        }
        return 0;
    }
    // (what was drawn before the clear is gone)
    let surface = st(|s| {
        let d = s.dcs.get_mut(&dc)?;
        d.dirty = None;
        d.layer.iter_mut().for_each(|b| *b = 0);
        Some(d.surface)
    });
    if let Some(surface) = surface {
        gpu2d::clear(surface, color);
    }
    0
}

fn SetTransform(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let m = [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3), rf(p, 4), rf(p, 5)];
    st(|s| s.dcs.get_mut(&c.a(0)).map(|d| d.xf = if m == IDENTITY { None } else { Some(m) }));
    0
}
fn GetTransform(c: &mut Ctx) -> u32 {
    let m = st(|s| s.dcs.get(&c.a(0)).and_then(|d| d.xf)).unwrap_or(IDENTITY);
    for (i, v) in m.iter().enumerate() {
        w32(c.a(1) + 4 * i as u32, v.to_bits());
    }
    0
}

fn apply(xf: Option<Xf>, x: f32, y: f32) -> (f32, f32) {
    match xf {
        Some(m) => (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]),
        None => (x, y),
    }
}

/// A rectangle (l, t, r, b) through the transform: its bounding box.
fn xf_rect(xf: Option<Xf>, r: [f32; 4]) -> [f32; 4] {
    let pts = [apply(xf, r[0], r[1]), apply(xf, r[2], r[1]), apply(xf, r[0], r[3]), apply(xf, r[2], r[3])];
    let (mut a, mut b) = ((f32::MAX, f32::MAX), (f32::MIN, f32::MIN));
    for p in pts {
        a = (a.0.min(p.0), a.1.min(p.1));
        b = (b.0.max(p.0), b.1.max(p.1));
    }
    [a.0, a.1, b.0, b.1]
}

/// Device pixels a rectangle covers inside the target and the clips.
fn clip_rect(d: &Dc, r: [f32; 4]) -> [f32; 4] {
    let mut r = [r[0].max(0.0), r[1].max(0.0), r[2].min(d.w as f32), r[3].min(d.h as f32)];
    for c in &d.clips {
        r = [r[0].max(c[0]), r[1].max(c[1]), r[2].min(c[2]), r[3].min(c[3])];
    }
    r
}

fn mark(d: &mut Dc, x0: u32, y0: u32, x1: u32, y1: u32) {
    if d.layer.len() != (d.w * d.h * 4) as usize {
        d.layer = vec![0; (d.w * d.h * 4) as usize];
    }
    d.dirty = Some(match d.dirty {
        Some([a, b, c, e]) => [a.min(x0), b.min(y0), c.max(x1), e.max(y1)],
        None => [x0, y0, x1, y1],
    });
}

/// Source-over of `color` (straight RGBA) at coverage `cov` (0..=1) onto a layer pixel.
#[inline]
fn blend(px: &mut [u8], color: [f32; 4], cov: f32) {
    let a = color[3] * cov;
    if a <= 0.0 {
        return;
    }
    let inv = 1.0 - a;
    let s = [color[2] * a, color[1] * a, color[0] * a, a];
    for k in 0..4 {
        px[k] = ((s[k] * 255.0) + px[k] as f32 * inv).round().clamp(0.0, 255.0) as u8;
    }
}

/// Fills device rectangle `r` (already clipped) with `color` at `opacity`; partial pixels at
/// the edges get partial coverage.
fn fill(dc: u32, r: [f32; 4], paint: &Paint) {
    if r[2] <= r[0] || r[3] <= r[1] {
        return;
    }
    st(|s| {
        let Some(d) = s.dcs.get_mut(&dc) else { return };
        let (x0, y0, x1, y1) = (r[0].floor() as u32, r[1].floor() as u32, (r[2].ceil() as u32).min(d.w), (r[3].ceil() as u32).min(d.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        mark(d, x0, y0, x1, y1);
        for y in y0..y1 {
            let cy = (r[3].min(y as f32 + 1.0) - r[1].max(y as f32)).clamp(0.0, 1.0);
            for x in x0..x1 {
                let cx = (r[2].min(x as f32 + 1.0) - r[0].max(x as f32)).clamp(0.0, 1.0);
                let i = ((y * d.w + x) * 4) as usize;
                blend(&mut d.layer[i..i + 4], paint.at(x as f32 + 0.5, y as f32 + 0.5), cx * cy);
            }
        }
    });
}

fn brush(dc: u32, b: u32) -> Paint {
    let xf = st(|s| s.dcs.get(&dc).and_then(|d| d.xf));
    st(|s| {
        s.brushes.get(&b).map(|b| match &b.linear {
            None => Paint::Solid([b.color[0], b.color[1], b.color[2], b.color[3] * b.opacity]),
            Some((p0, p1, stops)) => {
                let (a0, a1) = apply(xf, p0[0], p0[1]);
                let (b0, b1) = apply(xf, p1[0], p1[1]);
                let d = [b0 - a0, b1 - a1];
                Paint::Linear { a: [a0, a1], d, len2: d[0] * d[0] + d[1] * d[1], stops: stops.clone(), opacity: b.opacity }
            }
        })
    })
    .unwrap_or_else(|| {
        if b != 0 {
            eprintln!("sake: d2d brush {b:#x} is not one of ours");
        }
        Paint::Solid([0.0, 0.0, 0.0, 1.0])
    })
}

fn CreateGradientStopCollection(c: &mut Ctx) -> u32 {
    // (this, stops, count, gamma, extend mode, out)
    let (p, n) = (c.a(1), c.a(2));
    let stops: Vec<(f32, [f32; 4])> = (0..n).map(|i| {
        let q = p + 20 * i;
        (rf(q, 0), [rf(q, 1), rf(q, 2), rf(q, 3), rf(q, 4)])
    }).collect();
    let o = com::object("ID2D1GradientStopCollection", &[("GetGradientStopCount", StopCount as Handler)], 8);
    st(|s| s.stops.insert(o, stops));
    mem::putp(c.a(5), o);
    0
}
fn StopCount(c: &mut Ctx) -> u32 {
    st(|s| s.stops.get(&c.a(0)).map_or(0, |v| v.len() as u32))
}

fn CreateLinearGradientBrush(c: &mut Ctx) -> u32 {
    // (this, linear properties {start, end}, brush properties, stops, out)
    let (lp, bp, sc) = (c.a(1), c.a(2), c.a(3));
    let opacity = if bp != 0 { rf(bp, 0) } else { 1.0 };
    let stops = st(|s| s.stops.get(&sc).cloned()).unwrap_or_default();
    let o = com::object("ID2D1LinearGradientBrush", &[("SetOpacity", SetOpacity as Handler), ("GetOpacity", GetOpacity)], 8);
    let linear = Some(([rf(lp, 0), rf(lp, 1)], [rf(lp, 2), rf(lp, 3)], stops));
    st(|s| s.brushes.insert(o, Brush { color: [0.0; 4], opacity, linear }));
    mem::putp(c.a(4), o);
    0
}

/// Paints device pixels in `bbox` (clipped) with `paint` where `inside(x, y)` holds, 4x4
/// samples a pixel.
fn shape(dc: u32, bbox: [f32; 4], paint: &Paint, inside: impl Fn(f32, f32) -> bool) {
    st(|s| {
        let Some(d) = s.dcs.get_mut(&dc) else { return };
        let r = clip_rect(d, bbox);
        let (x0, y0, x1, y1) = (r[0].floor().max(0.0) as u32, r[1].floor().max(0.0) as u32, (r[2].ceil().max(0.0) as u32).min(d.w), (r[3].ceil().max(0.0) as u32).min(d.h));
        if x1 <= x0 || y1 <= y0 {
            return;
        }
        mark(d, x0, y0, x1, y1);
        for y in y0..y1 {
            for x in x0..x1 {
                let mut n = 0;
                for k in 0..16 {
                    let (sx, sy) = (x as f32 + (k % 4) as f32 * 0.25 + 0.125, y as f32 + (k / 4) as f32 * 0.25 + 0.125);
                    if sx >= r[0] && sx < r[2] && sy >= r[1] && sy < r[3] && inside(sx, sy) {
                        n += 1;
                    }
                }
                if n > 0 {
                    let i = ((y * d.w + x) * 4) as usize;
                    blend(&mut d.layer[i..i + 4], paint.at(x as f32 + 0.5, y as f32 + 0.5), n as f32 / 16.0);
                }
            }
        }
    });
}

/// An ellipse's (center, radii) through the transform (axis-aligned scale and translation).
fn xf_ellipse(dc: u32, p: u32) -> ([f32; 2], [f32; 2]) {
    let xf = st(|s| s.dcs.get(&dc).and_then(|d| d.xf));
    let (cx, cy) = apply(xf, rf(p, 0), rf(p, 1));
    let (sx, sy) = xf.map_or((1.0, 1.0), |m| (m[0].hypot(m[1]), m[2].hypot(m[3])));
    ([cx, cy], [rf(p, 2) * sx, rf(p, 3) * sy])
}

fn ellipse(dc: u32, c: [f32; 2], r: [f32; 2], paint: &Paint, ring: Option<f32>) {
    let h = ring.map_or(0.0, |w| w / 2.0);
    let (ox, oy) = (r[0] + h, r[1] + h);
    let (ix, iy) = (r[0] - h, r[1] - h);
    let bbox = [c[0] - ox, c[1] - oy, c[0] + ox, c[1] + oy];
    shape(dc, bbox, paint, |x, y| {
        let (dx, dy) = (x - c[0], y - c[1]);
        let out = (dx / ox).powi(2) + (dy / oy).powi(2) <= 1.0;
        out && (ring.is_none() || ix <= 0.0 || iy <= 0.0 || (dx / ix).powi(2) + (dy / iy).powi(2) > 1.0)
    });
}

fn FillEllipse(c: &mut Ctx) -> u32 {
    let (dc, p, b) = (c.a(0), c.a(1), c.a(2));
    let (ctr, r) = xf_ellipse(dc, p);
    ellipse(dc, ctr, r, &brush(dc, b), None);
    0
}
fn DrawEllipse(c: &mut Ctx) -> u32 {
    let (dc, p, b, w) = (c.a(0), c.a(1), c.a(2), fl(c, 3));
    let (ctr, r) = xf_ellipse(dc, p);
    ellipse(dc, ctr, r, &brush(dc, b), Some(w));
    0
}

fn FillRectangle(c: &mut Ctx) -> u32 {
    let (dc, p, b) = (c.a(0), c.a(1), c.a(2));
    let r = [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3)];
    let Some(r) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, xf_rect(d.xf, r)))) else { return 0 };
    fill(dc, r, &brush(dc, b));
    0
}

fn DrawRectangle(c: &mut Ctx) -> u32 {
    let (dc, p, b, w) = (c.a(0), c.a(1), c.a(2), fl(c, 3));
    let r = [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3)];
    let h = w / 2.0;
    let paint = brush(dc, b);
    // (four bands centred on the edges)
    for e in [
        [r[0] - h, r[1] - h, r[2] + h, r[1] + h],
        [r[0] - h, r[3] - h, r[2] + h, r[3] + h],
        [r[0] - h, r[1] + h, r[0] + h, r[3] - h],
        [r[2] - h, r[1] + h, r[2] + h, r[3] - h],
    ] {
        if let Some(e) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, xf_rect(d.xf, e)))) {
            fill(dc, e, &paint);
        }
    }
    0
}

fn PushAxisAlignedClip(c: &mut Ctx) -> u32 {
    let (dc, p) = (c.a(0), c.a(1));
    let r = [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3)];
    st(|s| {
        if let Some(d) = s.dcs.get_mut(&dc) {
            let r = xf_rect(d.xf, r);
            d.clips.push(r);
        }
    });
    0
}
fn PopAxisAlignedClip(c: &mut Ctx) -> u32 {
    st(|s| s.dcs.get_mut(&c.a(0)).map(|d| d.clips.pop()));
    0
}

/// Draws laid-out text with its box's top left at (x, y) (before the transform).
fn draw_text(dc: u32, text: &[u16], fmt: &dwrite::Format, w: f32, h: f32, x: f32, y: f32, b: u32, clip_box: bool) {
    let paint = brush(dc, b);
    let xf = st(|s| s.dcs.get(&dc).and_then(|d| d.xf));
    // (a scaling transform scales the font; glyphs stay upright)
    let scale = xf.map_or(1.0, |m| (m[0] * m[3] - m[1] * m[2]).abs().sqrt());
    let mut f = fmt.clone();
    f.size *= scale;
    let laid = dwrite::layout(text, fmt, w, h);
    let Some(font) = dwrite::layout(&[], &f, 0.0, 0.0).font else { return };
    let chars = dwrite::chars(text);
    let clip_extra = if clip_box { Some(xf_rect(xf, [x, y, x + w, y + h])) } else { None };
    st(|s| {
        let Some(d) = s.dcs.get_mut(&dc) else { return };
        let mut lim = clip_rect(d, [0.0, 0.0, d.w as f32, d.h as f32]);
        if let Some(c) = clip_extra {
            lim = [lim[0].max(c[0]), lim[1].max(c[1]), lim[2].min(c[2]), lim[3].min(c[3])];
        }
        for l in &laid.lines {
            for i in l.from..l.to {
                let ch = chars[i];
                if ch < 32 {
                    continue;
                }
                let (px, py) = apply(xf, x + l.x + laid.pen[i], y + l.baseline);
                let (px, py) = (px.round() as i32, py.round() as i32);
                font.glyph(ch, |gx, top, gw, gh, cov| {
                    let (ox, oy) = (px + gx, py - top);
                    let (x0, y0) = (ox.max(lim[0].ceil() as i32), oy.max(lim[1].ceil() as i32));
                    let (x1, y1) = ((ox + gw).min(lim[2].floor() as i32), (oy + gh).min(lim[3].floor() as i32));
                    if x1 <= x0 || y1 <= y0 {
                        return;
                    }
                    mark(d, x0 as u32, y0 as u32, x1 as u32, y1 as u32);
                    for yy in y0..y1 {
                        for xx in x0..x1 {
                            let k = cov[((yy - oy) * gw + (xx - ox)) as usize];
                            if k != 0 {
                                let i = ((yy as u32 * d.w + xx as u32) * 4) as usize;
                                blend(&mut d.layer[i..i + 4], paint.at(xx as f32 + 0.5, yy as f32 + 0.5), k as f32 / 255.0);
                            }
                        }
                    }
                });
            }
        }
    });
}

fn DrawText(c: &mut Ctx) -> u32 {
    // (this, string, length, format, layout rect, brush, options, measuring mode)
    let (dc, s, n, fmt, r, b, opts) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let Some(f) = dwrite::format_of(fmt) else { return 0 };
    let text: Vec<u16> = (0..n).map(|i| mem::r16(s + 2 * i)).collect();
    let (l, t, rr, bb) = (rf(r, 0), rf(r, 1), rf(r, 2), rf(r, 3));
    // D2D1_DRAW_TEXT_OPTIONS_CLIP (2)
    draw_text(dc, &text, &f, rr - l, bb - t, l, t, b, opts & 2 != 0);
    0
}

fn DrawTextLayout(c: &mut Ctx) -> u32 {
    // (this, origin x, origin y, layout, brush, options)
    let (dc, x, y, lay, b, opts) = (c.a(0), fl(c, 1), fl(c, 2), c.a(3), c.a(4), c.a(5));
    let Some((text, f, w, h)) = dwrite::layout_of(lay) else { return 0 };
    draw_text(dc, &text, &f, w, h, x, y, b, opts & 2 != 0);
    0
}

pub fn init() {
    implement("d2d1", "D2D1CreateFactory", D2D1CreateFactory);
    implement("d2d1", "D2D1CreateDevice", D2D1CreateDevice);
    implement("d2d1", "D2D1CreateDeviceContext", D2D1CreateDeviceContext);
}
