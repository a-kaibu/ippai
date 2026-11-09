//! Direct2D on D3D11 textures, drawn with WebGPU: device contexts work out on the CPU where each
//! shape goes (transforms, clips, text with dwrite.rs's layout and gdi.rs's glyphs) and collect
//! them as kura::d3d11::d2d items (rectangles, ellipses, glyphs from kura's atlas; solid colors
//! or gradient ramps); EndDraw (or Flush) draws them over the target texture in one instanced
//! draw. Clear clears the texture itself.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx, Handler};
use crate::com;
use crate::dwrite;
use crate::mem::{self, r32, w32};
use std::collections::HashMap;
use std::sync::Mutex;
use kura::d3d11::d2d::{self as gpu2d, Item};

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
    /// what was drawn since the last flush
    items: Vec<Item>,
    xf: Option<Xf>,
    /// dots per inch other than 96 (render target properties, SetDpi): DIPs scale by dpi / 96
    dpi: Option<(f32, f32)>,
    /// D2D1_ANTIALIAS_MODE_ALIASED
    aliased: bool,
    clips: Vec<[f32; 4]>,
    /// the bitmap texture the BITMAP items in `items` show
    bitmap: Option<u32>,
    drawing: Drawing,
}

pub(crate) const WRONG_STATE: u32 = 0x8899_0001;
pub(crate) const PUSH_POP_UNBALANCED: u32 = 0x8899_0016;

/// A context between BeginDraw and EndDraw (formal/tla/D2dDraw.tla): drawing outside them, a
/// second BeginDraw or a Pop with no Push puts the context in error and EndDraw (or Flush, which
/// then clears it) says so; an error from outside drawing stays for the next BeginDraw/EndDraw
/// (as Windows, winref probe d2d). EndDraw with clips still pushed fails with
/// D2DERR_PUSH_POP_UNBALANCED and drops them.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub(crate) struct Drawing {
    pub drawing: bool,
    /// the first error since BeginDraw (0: none)
    pub error: u32,
    /// axis-aligned clips pushed
    pub clips: u32,
}

impl Drawing {
    fn fail(&mut self, e: u32) {
        if self.error == 0 {
            self.error = e;
        }
    }
    pub fn begin(&mut self) {
        if self.drawing {
            self.fail(WRONG_STATE);
        } else {
            *self = Drawing { drawing: true, error: self.error, clips: 0 };
        }
    }
    /// Whether a drawing call may draw.
    pub fn draw(&mut self) -> bool {
        if !self.drawing {
            self.fail(WRONG_STATE);
        }
        self.drawing
    }
    pub fn push(&mut self) -> bool {
        let ok = self.draw();
        self.clips += ok as u32;
        ok
    }
    /// Whether a clip comes off.
    pub fn pop(&mut self) -> bool {
        if !self.draw() {
            return false;
        }
        if self.clips == 0 {
            self.fail(PUSH_POP_UNBALANCED);
            return false;
        }
        self.clips -= 1;
        true
    }
    pub fn flush(&mut self) -> u32 {
        if !self.drawing {
            return WRONG_STATE;
        }
        std::mem::take(&mut self.error)
    }
    pub fn end(&mut self) -> u32 {
        if !self.drawing {
            // (an error recorded outside drawing is lost with it)
            *self = Drawing::default();
            return WRONG_STATE;
        }
        let r = if self.error != 0 { self.error } else if self.clips != 0 { PUSH_POP_UNBALANCED } else { 0 };
        *self = Drawing::default();
        r
    }
}

/// `dc`'s drawing state after `f` (false for no such context).
fn drawing(dc: u32, f: impl FnOnce(&mut Drawing) -> bool) -> bool {
    st(|s| s.dcs.get_mut(&dc).is_some_and(|d| f(&mut d.drawing)))
}

/// A bitmap of its own (CreateBitmap, CreateBitmapFromWicBitmap): its texture, size in pixels,
/// and whether its alpha is ignored (D2D1_ALPHA_MODE_IGNORE).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Image {
    gpu: u32,
    w: u32,
    h: u32,
    opaque: bool,
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
    /// The item's paint and ramp words; None: no room for its ramp.
    fn gpu(&self) -> Option<([f32; 4], u32)> {
        match self {
            Paint::Solid(c) => Some((*c, 0)),
            Paint::Linear { a, d, len2, stops, opacity } => {
                let mut texels = [0u8; 1024];
                for k in 0..256 {
                    let t = k as f32 / 255.0;
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
                    c[3] *= opacity;
                    for i in 0..4 {
                        texels[k * 4 + i] = (c[i].clamp(0.0, 1.0) * 255.0).round() as u8;
                    }
                }
                let row = gpu2d::ramp(&texels)?;
                let dir = if *len2 > 0.0 { [d[0] / len2, d[1] / len2] } else { [0.0; 2] };
                Some(([a[0], a[1], dir[0], dir[1]], row + 1))
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
    images: HashMap<u32, Image>,
    /// surface bitmaps made with D2D1_ALPHA_MODE_IGNORE (or a format without alpha)
    opaque: std::collections::HashSet<u32>,
    stops: HashMap<u32, Vec<(f32, [f32; 4])>>,
    /// device -> factory, context/device -> device
    parents: HashMap<u32, u32>,
    /// HWND render target -> (its swap chain, window)
    hwnds: HashMap<u32, (u32, u32)>,
    /// HWND / compatible render target -> the D3D11 device its textures come from
    d3d: HashMap<u32, u32>,
}

static S: Mutex<Option<State>> = Mutex::new(None);

fn st<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(S.lock().unwrap().get_or_insert_with(State::default))
}

fn fl(c: &Ctx, i: u32) -> f32 {
    c.f(i)
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
            ("CreateHwndRenderTarget", CreateHwndRenderTarget),
            ("GetDesktopDpi", GetDpi),
            ("CreatePathGeometry", CreatePathGeometry),
            ("CreateTransformedGeometry", CreateTransformedGeometry),
            ("CreateStrokeStyle", CreateStrokeStyle),
            ("ReloadSystemMetrics", ok),
        ],
        8,
    )
}

// ---- HWND render targets: a D3D11 swap chain on the window, presented by EndDraw ----

/// The swap chain's back buffer (ID3D11Texture2D) as a D2D bitmap.
fn back_buffer(c: &mut Ctx, sc: u32) -> u32 {
    // IID_ID3D11Texture2D {6f15aaf2-d208-4e89-9ab4-489535d34f9c}
    let iid = mem::heap_alloc(24, true);
    mem::write(iid, &[0xf2, 0xaa, 0x15, 0x6f, 0x08, 0xd2, 0x89, 0x4e, 0x9a, 0xb4, 0x48, 0x95, 0x35, 0xd3, 0x4f, 0x9c]);
    c.call(mem::rp(mem::rp(sc) + mem::ps() * 9), &[sc, 0, iid, iid + 16]);
    let tex = r32(iid + 16);
    mem::heap_free(iid);
    new_bitmap(tex)
}

/// CreateHwndRenderTarget(render target properties, HWND properties, out)
fn CreateHwndRenderTarget(c: &mut Ctx) -> u32 {
    let (factory, hp, out) = (c.a(0), c.a(2), c.a(3));
    let hwnd = r32(hp);
    let (mut w, mut h) = (r32(hp + 4), r32(hp + 8));
    if w == 0 || h == 0 {
        if let Some(r) = crate::user::client_rect_screen(hwnd) {
            (w, h) = ((r.r - r.l).max(1) as u32, (r.b - r.t).max(1) as u32);
        }
    }
    // DXGI_SWAP_CHAIN_DESC: w, h, refresh, B8G8R8A8, ..., 1 sample, render target output, 1
    // buffer, the window, windowed, discard
    let desc = mem::heap_alloc(60 + 16, true);
    for (o, v) in [(0, w), (4, h), (16, 87), (28, 1), (36, 0x20), (40, 1), (44, hwnd), (48, 1)] {
        w32(desc + o, v);
    }
    let (sc_out, dev_out, ctx_out) = (desc + 60, desc + 64, desc + 68);
    let create = crate::api::export("d3d11", "D3D11CreateDeviceAndSwapChain");
    let hr = c.call(create, &[0, 1, 0, 0x20, 0, 0, 7, desc, sc_out, dev_out, 0, ctx_out]);
    let (sc, d3d) = (r32(sc_out), r32(dev_out));
    mem::heap_free(desc);
    if hr != 0 || sc == 0 {
        mem::putp(out, 0);
        return if hr != 0 { hr } else { 0x8000_4005 };
    }
    let dev_out = mem::heap_alloc(8, true);
    new_device(factory, dev_out);
    let device = mem::rp(dev_out);
    mem::heap_free(dev_out);
    // (first match wins: these before the device context's)
    let mut methods: Vec<(&str, Handler)> = vec![
        ("EndDraw", HwndEndDraw as Handler),
        ("Flush", Flush),
        ("CheckWindowState", zero),
        ("Resize", Resize),
        ("GetHwnd", GetHwnd),
        ("GetPixelFormat", GetPixelFormat),
        ("GetMaximumBitmapSize", GetMaximumBitmapSize),
        ("CreateCompatibleRenderTarget", CreateCompatibleRenderTarget),
    ];
    methods.extend(context_methods());
    let o = com::object("ID2D1HwndRenderTarget", &methods, 8);
    st(|s| {
        s.parents.insert(o, device);
        s.dcs.insert(o, Dc { device, ..Default::default() });
        s.hwnds.insert(o, (sc, hwnd));
        s.d3d.insert(o, d3d);
    });
    let bmp = back_buffer(c, sc);
    set_target(o, bmp);
    mem::putp(out, o);
    0
}

/// CreateCompatibleRenderTarget(size in DIPs *, size in pixels *, pixel format *, options, out):
/// a render target on a texture of its own, which GetBitmap hands out to draw elsewhere.
fn CreateCompatibleRenderTarget(c: &mut Ctx) -> u32 {
    let (parent, ps, pp, out) = (c.a(0), c.a(1), c.a(2), c.a(5));
    let (pw, ph, device, d3d) = st(|s| {
        let (w, h, dev) = s.dcs.get(&parent).map_or((1, 1, 0), |d| (d.w, d.h, d.device));
        (w, h, dev, s.d3d.get(&parent).copied().unwrap_or(0))
    });
    let (w, h) = if pp != 0 {
        (r32(pp), r32(pp + 4))
    } else if ps != 0 {
        (rf(ps, 0).ceil() as u32, rf(ps, 1).ceil() as u32)
    } else {
        (pw, ph)
    };
    let (w, h) = (w.max(1), h.max(1));
    if d3d == 0 {
        mem::putp(out, 0);
        return 0x8899_000c; // D2DERR_UNSUPPORTED_OPERATION
    }
    // ID3D11Device::CreateTexture2D(D3D11_TEXTURE2D_DESC: w, h, 1 mip, 1 layer, B8G8R8A8, 1
    // sample, default usage, render target | shader resource)
    let desc = mem::heap_alloc(44 + 8, true);
    for (o, v) in [(0, w), (4, h), (8, 1), (12, 1), (16, 87), (20, 1), (32, 0x28)] {
        w32(desc + o, v);
    }
    c.call(mem::rp(mem::rp(d3d) + mem::ps() * 5), &[d3d, desc, 0, desc + 44]);
    let tex = r32(desc + 44);
    mem::heap_free(desc);
    if tex == 0 {
        mem::putp(out, 0);
        return 0x8007_000e;
    }
    let bmp = new_bitmap(tex);
    if let Some(gpu) = gpu2d::texture_gpu(tex) {
        st(|s| s.images.insert(bmp, Image { gpu, w, h, opaque: false }));
    }
    gpu2d::clear(tex, [0.0; 4]);
    let mut methods: Vec<(&str, Handler)> = vec![("GetBitmap", GetBitmap as Handler), ("CreateCompatibleRenderTarget", CreateCompatibleRenderTarget)];
    methods.extend(context_methods());
    let o = com::object("ID2D1BitmapRenderTarget", &methods, 8);
    st(|s| {
        s.parents.insert(o, device);
        s.dcs.insert(o, Dc { device, ..Default::default() });
        s.d3d.insert(o, d3d);
    });
    set_target(o, bmp);
    mem::putp(out, o);
    0
}
fn GetBitmap(c: &mut Ctx) -> u32 {
    let t = st(|s| s.dcs.get(&c.a(0)).map_or(0, |d| d.target));
    if t != 0 {
        com::add_ref_obj(t);
    }
    mem::putp(c.a(1), t);
    0
}

fn HwndEndDraw(c: &mut Ctx) -> u32 {
    let r = EndDraw(c);
    if let Some((sc, _)) = st(|s| s.hwnds.get(&c.a(0)).copied()) {
        // IDXGISwapChain::Present(sync 0, flags 0)
        c.call(mem::rp(mem::rp(sc) + mem::ps() * 8), &[sc, 0, 0]);
    }
    r
}
/// Resize(D2D1_SIZE_U*)
fn Resize(c: &mut Ctx) -> u32 {
    let (o, p) = (c.a(0), c.a(1));
    let Some((sc, _)) = st(|s| s.hwnds.get(&o).copied()) else { return 0x8007_0057 };
    flush(o);
    // IDXGISwapChain::ResizeBuffers(0 = keep the count, w, h, keep the format, flags)
    c.call(mem::rp(mem::rp(sc) + mem::ps() * 13), &[sc, 0, r32(p), r32(p + 4), 0, 0]);
    let bmp = back_buffer(c, sc);
    set_target(o, bmp);
    0
}
fn GetHwnd(c: &mut Ctx) -> u32 {
    st(|s| s.hwnds.get(&c.a(0)).map_or(0, |x| x.1))
}
/// GetPixelFormat (a structure result: the hidden pointer): B8G8R8A8, premultiplied
fn GetPixelFormat(c: &mut Ctx) -> u32 {
    w32(c.a(1), 87);
    w32(c.a(1) + 4, 1);
    c.a(1)
}
fn GetMaximumBitmapSize(_: &mut Ctx) -> u32 {
    8192
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
        ("CreateBitmap", CreateBitmap),
        ("CreateBitmapFromWicBitmap", CreateBitmapFromWicBitmap),
        ("DrawBitmap", DrawBitmap),
        ("SetTarget", SetTarget),
        ("GetTarget", GetTarget),
        ("BeginDraw", BeginDraw),
        ("EndDraw", EndDraw),
        ("Flush", Flush),
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
        ("DrawGlyphRun", DrawGlyphRun),
        ("DrawGeometry", DrawGeometry),
        ("FillGeometry", FillGeometry),
        ("PushAxisAlignedClip", PushAxisAlignedClip),
        ("PopAxisAlignedClip", PopAxisAlignedClip),
        ("SetAntialiasMode", SetAntialiasMode),
        ("SetTextAntialiasMode", ok),
        ("SetTextRenderingParams", ok),
        ("SetDpi", SetDpi),
        ("SetUnitMode", ok),
        ("SetPrimitiveBlend", ok),
        ("SetTags", ok),
        ("GetAntialiasMode", GetAntialiasMode),
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

/// ID2D1Device::CreateDeviceContext(options, out): 96 DPI (there are no render target
/// properties: what lies after `out` was read as DPI, KiriKiri's DrawDeviceD2D then drew its
/// picture at 2/96 the size, AMBITIOUS MISSION's black screen; winref d2d device_context).
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
    // (D2D1_CREATION_PROPERTIES: threading, debug level, options; no DPI: 96)
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
    // (D2D1_RENDER_TARGET_PROPERTIES: dpiX, dpiY after the type and pixel format; 0 is 96)
    let props = c.a(2);
    let dpi = if props != 0 { (rf(props, 3), rf(props, 4)) } else { (0.0, 0.0) };
    let dpi = Some(dpi).filter(|&(x, y)| x > 0.0 && y > 0.0 && (x, y) != (96.0, 96.0));
    st(|s| {
        s.parents.insert(o, device);
        s.dcs.insert(o, Dc { device, dpi, ..Default::default() })
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
    let (x, y) = st(|s| s.dcs.get(&c.a(0)).and_then(|d| d.dpi)).unwrap_or((96.0, 96.0));
    for (p, v) in [(c.a(1), x), (c.a(2), y)] {
        if p != 0 {
            w32(p, v.to_bits());
        }
    }
    0
}
/// SetDpi(x, y): both 0 is the default (96); a negative or one 0 is ignored
fn SetDpi(c: &mut Ctx) -> u32 {
    let (x, y) = (fl(c, 1), fl(c, 2));
    let dpi = if x == 0.0 && y == 0.0 {
        None
    } else if x > 0.0 && y > 0.0 {
        Some((x, y))
    } else {
        return 0;
    };
    st(|s| s.dcs.get_mut(&c.a(0)).map(|d| d.dpi = dpi.filter(|&v| v != (96.0, 96.0))));
    0
}
/// The transform drawing goes by: the target's, then DIPs to pixels.
fn eff(d: &Dc) -> Option<Xf> {
    let Some((dx, dy)) = d.dpi else { return d.xf };
    let (sx, sy) = (dx / 96.0, dy / 96.0);
    let m = d.xf.unwrap_or(IDENTITY);
    Some([m[0] * sx, m[1] * sy, m[2] * sx, m[3] * sy, m[4] * sx, m[5] * sy])
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

/// An ID2D1Bitmap1 object (both kinds: a D3D11 surface, or an image of its own).
fn bitmap_object() -> u32 {
    com::object(
        "ID2D1Bitmap1",
        &[
            ("Release", BitmapRelease as Handler),
            ("GetSize", BitmapSize),
            ("GetPixelSize", BitmapPixelSize),
            ("GetDpi", GetDpi),
            ("GetPixelFormat", BitmapFormat),
            ("CopyFromMemory", CopyFromMemory),
        ],
        8,
    )
}

fn new_bitmap(surface: u32) -> u32 {
    let (w, h) = gpu2d::size(surface).unwrap_or((0, 0));
    let o = bitmap_object();
    st(|s| s.bitmaps.insert(o, (surface, w, h)));
    o
}

/// An image bitmap `w` x `h`, from premultiplied BGRA rows `px` (`pitch` apart) when given.
fn new_image(w: u32, h: u32, px: Option<(&[u8], u32)>, opaque: bool) -> u32 {
    let gpu = gpu2d::bitmap(w, h);
    if let Some((px, pitch)) = px {
        gpu2d::bitmap_write(gpu, (0, 0, w, h), px, pitch);
    }
    let o = bitmap_object();
    st(|s| s.images.insert(o, Image { gpu, w, h, opaque }));
    o
}

/// D2D1_ALPHA_MODE_IGNORE in D2D1_PIXEL_FORMAT `pf` (or a format without alpha).
fn alpha_ignored(pf: u32) -> bool {
    pf != 0 && (r32(pf + 4) == 3 || r32(pf) == 88) // (DXGI_FORMAT_B8G8R8X8_UNORM)
}

fn CreateBitmap(c: &mut Ctx) -> u32 {
    // (this, D2D1_SIZE_U by value, data, pitch, properties, out)
    let (w, h) = c.pt(1);
    let (w, h, o) = (w as u32, h as u32, if mem::x64() { 2 } else { 3 });
    let (data, pitch, props, out) = (c.a(o), c.a(o + 1), c.a(o + 2), c.a(o + 3));
    if w == 0 || h == 0 || w > 16384 || h > 16384 {
        return 0x8007_0057; // E_INVALIDARG
    }
    let px = (data != 0).then(|| mem::slice(data, pitch * (h - 1) + w * 4).to_vec());
    mem::putp(out, new_image(w, h, px.as_deref().map(|p| (p, pitch)), alpha_ignored(props)));
    0
}

fn CreateBitmapFromWicBitmap(c: &mut Ctx) -> u32 {
    // (this, source, properties or NULL, out)
    let (src, props, out) = (c.a(1), c.a(2), c.a(3));
    let Some((w, h, px)) = crate::wic::pbgra(c, src) else {
        mem::putp(out, 0);
        return 0x8000_4005; // E_FAIL
    };
    mem::putp(out, new_image(w, h, Some((&px, w * 4)), alpha_ignored(props)));
    0
}

fn CopyFromMemory(c: &mut Ctx) -> u32 {
    // (this, D2D1_RECT_U * or NULL, data, pitch)
    let (this, pr, data, pitch) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(im) = st(|s| s.images.get(&this).copied()) else { return 0x8000_4005 };
    let (x, y, r, b) = if pr != 0 { (r32(pr), r32(pr + 4), r32(pr + 8), r32(pr + 12)) } else { (0, 0, im.w, im.h) };
    let (r, b) = (r.min(im.w), b.min(im.h));
    if data == 0 || r <= x || b <= y {
        return 0x8007_0057;
    }
    let (w, h) = (r - x, b - y);
    let px = mem::slice(data, pitch * (h - 1) + w * 4).to_vec();
    gpu2d::bitmap_write(im.gpu, (x, y, w, h), &px, pitch);
    0
}

fn BitmapFormat(c: &mut Ctx) -> u32 {
    // (this, out): D2D1_PIXEL_FORMAT returned through a hidden pointer
    let out = c.a(1);
    let opaque = st(|s| s.images.get(&c.a(0)).is_some_and(|i| i.opaque) || s.opaque.contains(&c.a(0)));
    w32(out, 87); // DXGI_FORMAT_B8G8R8A8_UNORM
    w32(out + 4, if opaque { 3 } else { 1 });
    out
}

fn BitmapRelease(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = com::release_obj(this);
    if n == 0 {
        if let Some(im) = st(|s| {
            s.bitmaps.remove(&this);
            s.opaque.remove(&this);
            s.images.remove(&this)
        }) {
            // (drawn first if a context still has it in its items)
            let users: Vec<u32> = st(|s| s.dcs.iter().filter(|d| d.1.bitmap == Some(im.gpu)).map(|d| *d.0).collect());
            users.into_iter().for_each(flush);
            gpu2d::bitmap_free(im.gpu);
        }
    }
    n
}

/// What DrawBitmap of `bmp` on context `dc` shows.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Source {
    /// a bitmap of its own (or a compatible render target's)
    Image(Image),
    /// a bitmap of a D3D11 texture / DXGI surface
    Surface { surface: u32, w: u32, h: u32, opaque: bool },
    /// the texture `dc` draws into (D2D refuses that: nothing drawn)
    Target,
    /// no bitmap (released, or not one)
    None,
}

fn source(s: &State, dc: u32, bmp: u32) -> Source {
    let surface = s.bitmaps.get(&bmp).copied();
    let target = s.dcs.get(&dc).map(|d| d.surface);
    if let Some((t, ..)) = surface {
        if t != 0 && target == Some(t) {
            return Source::Target;
        }
    }
    match (s.images.get(&bmp), surface) {
        (Some(&im), _) => Source::Image(im),
        (None, Some((surface, w, h))) if surface != 0 => Source::Surface { surface, w, h, opaque: s.opaque.contains(&bmp) },
        _ => Source::None,
    }
}

fn DrawBitmap(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    // (this, bitmap, destination D2D1_RECT_F * or NULL, opacity, interpolation, source * or NULL)
    let (dc, bmp, pd, opacity, interp, ps) = (c.a(0), c.a(1), c.a(2), fl(c, 3), c.a(4), c.a(5));
    let im = match st(|s| source(s, dc, bmp)) {
        Source::Image(im) => im,
        // a bitmap of a D3D11 texture (CreateBitmapFromDxgiSurface, another context's target):
        // the texture itself, as it is when the items are drawn
        Source::Surface { surface, w, h, opaque } => match gpu2d::texture_gpu(surface) {
            Some(gpu) => Image { gpu, w, h, opaque },
            None => {
                com::warn_once("sake: d2d DrawBitmap of a surface that is not a texture: not drawn");
                return 0;
            }
        },
        Source::Target => {
            com::warn_once("sake: d2d DrawBitmap of the context's own target: not drawn");
            return 0;
        }
        Source::None => return 0,
    };
    // another context's target (a compatible render target's bitmap, a texture drawn by D2D):
    // what was drawn into it first
    let surface = st(|s| s.bitmaps.get(&bmp).map(|b| b.0));
    if let Some(owner) = st(|s| s.dcs.iter().find(|(&k, d)| k != dc && (d.target == bmp || surface.is_some_and(|t| t != 0 && d.surface == t)) && !d.items.is_empty()).map(|(&k, _)| k)) {
        flush(owner);
    }
    let src = if ps != 0 { [rf(ps, 0), rf(ps, 1), rf(ps, 2), rf(ps, 3)] } else { [0.0, 0.0, im.w as f32, im.h as f32] };
    let dst = if pd != 0 { [rf(pd, 0), rf(pd, 1), rf(pd, 2), rf(pd, 3)] } else { [0.0, 0.0, src[2] - src[0], src[3] - src[1]] };
    let xf = st(|s| s.dcs.get(&dc).and_then(eff));
    if xf.is_some_and(|m| m[1] != 0.0 || m[2] != 0.0) {
        com::warn_once("sake: d2d DrawBitmap under a rotation: drawn in its bounding box");
    }
    // (a mirroring transform: the corners swapped, and the source with them)
    let (a, b) = (apply(xf, dst[0], dst[1]), apply(xf, dst[2], dst[3]));
    let mut shape = [a.0, a.1, b.0, b.1];
    let mut tex = src;
    if shape[0] > shape[2] {
        shape.swap(0, 2);
        tex.swap(0, 2);
    }
    if shape[1] > shape[3] {
        shape.swap(1, 3);
        tex.swap(1, 3);
    }
    let Some(r) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, shape))) else { return 0 };
    if r[2] <= r[0] || r[3] <= r[1] || opacity <= 0.0 {
        return 0;
    }
    // one bitmap a draw: what shows another goes first
    if st(|s| s.dcs.get(&dc).is_some_and(|d| d.bitmap.is_some_and(|g| g != im.gpu))) {
        flush(dc);
    }
    let paint = [opacity.min(1.0), (interp != 0) as u32 as f32, im.opaque as u32 as f32, 0.0];
    st(|s| {
        if let Some(d) = s.dcs.get_mut(&dc) {
            d.bitmap = Some(im.gpu);
            d.items.push(Item { rect: r, shape, extra: tex, paint, kind: [gpu2d::BITMAP, 0, 0, 0] });
        }
    });
    0
}

fn CreateBitmapFromDxgiSurface(c: &mut Ctx) -> u32 {
    // (this, surface, properties, out)
    if gpu2d::size(c.a(1)).is_none() {
        eprintln!("sake: d2d CreateBitmapFromDxgiSurface {:#x}: not a texture", c.a(1));
    }
    let bmp = new_bitmap(c.a(1));
    if alpha_ignored(c.a(2)) {
        st(|s| s.opaque.insert(bmp));
    }
    // (always: a game's picture goes through these, in every run's log; the first 16)
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if N.fetch_add(1, std::sync::atomic::Ordering::Relaxed) < 16 {
        let (w, h) = bitmap_size(bmp);
        let opts = if c.a(2) != 0 { mem::r32(c.a(2) + 16) } else { 0 };
        eprintln!("sake: d2d bitmap {bmp:#x} of surface {:#x}: {w}x{h} options {opts:#x}", c.a(1));
    }
    mem::putp(c.a(3), bmp);
    0
}
/// A bitmap's size in pixels (either kind).
fn bitmap_size(b: u32) -> (u32, u32) {
    st(|s| s.images.get(&b).map(|i| (i.w, i.h)).or_else(|| s.bitmaps.get(&b).map(|v| (v.1, v.2))).unwrap_or_default())
}
fn BitmapSize(c: &mut Ctx) -> u32 {
    let (w, h) = bitmap_size(c.a(0));
    w32(c.a(1), (w as f32).to_bits());
    w32(c.a(1) + 4, (h as f32).to_bits());
    c.a(1)
}
fn BitmapPixelSize(c: &mut Ctx) -> u32 {
    let (w, h) = bitmap_size(c.a(0));
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
            d.items.clear();
            d.bitmap = None;
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
    // (in DIPs)
    let (w, h, (dx, dy)) = st(|s| s.dcs.get(&c.a(0)).map_or((0, 0, (96.0, 96.0)), |d| (d.w, d.h, d.dpi.unwrap_or((96.0, 96.0)))));
    w32(c.a(1), (w as f32 * 96.0 / dx).to_bits());
    w32(c.a(1) + 4, (h as f32 * 96.0 / dy).to_bits());
    c.a(1)
}
fn GetPixelSize(c: &mut Ctx) -> u32 {
    let (w, h) = st(|s| s.dcs.get(&c.a(0)).map_or((0, 0), |d| (d.w, d.h)));
    w32(c.a(1), w);
    w32(c.a(1) + 4, h);
    c.a(1)
}

// ---- drawing ----

/// Draws what was collected over the target texture.
fn flush(dc: u32) {
    let job = st(|s| s.dcs.get_mut(&dc).map(|d| (d.surface, std::mem::take(&mut d.items), d.bitmap.take())));
    if let Some((surface, items, bitmap)) = job {
        gpu2d::draw(surface, &items, bitmap);
    }
}

/// `f`'s result, after making room in kura's atlas and ramps if it found none (by drawing
/// what is pending first).
fn room<T>(dc: u32, mut f: impl FnMut() -> Option<T>) -> T {
    if let Some(v) = f() {
        return v;
    }
    flush(dc);
    gpu2d::reset();
    f().expect("d2d: an empty atlas has room")
}

/// Adds an item painted with `paint`.
fn push(dc: u32, kind: u32, rect: [f32; 4], shape: [f32; 4], extra: [f32; 4], paint: &Paint) {
    let (p, row) = room(dc, || paint.gpu());
    st(|s| s.dcs.get_mut(&dc).map(|d| d.items.push(Item { rect, shape, extra, paint: p, kind: [kind, row, 0, 0] })));
}

fn BeginDraw(c: &mut Ctx) -> u32 {
    drawing(c.a(0), |d| {
        d.begin();
        true
    });
    0
}

/// (EndDraw and Flush: this, tag1 *, tag2 *)
fn EndDraw(c: &mut Ctx) -> u32 {
    let dc = c.a(0);
    flush(dc);
    let r = st(|s| {
        let d = s.dcs.get_mut(&dc)?;
        let r = d.drawing.end();
        d.clips.clear();
        Some(r)
    });
    tags(c);
    r.unwrap_or(0)
}

fn Flush(c: &mut Ctx) -> u32 {
    let dc = c.a(0);
    flush(dc);
    tags(c);
    st(|s| s.dcs.get_mut(&dc).map_or(0, |d| d.drawing.flush()))
}

fn tags(c: &Ctx) {
    for p in [c.a(1), c.a(2)] {
        if p != 0 {
            w32(p, 0);
            w32(p + 4, 0);
        }
    }
}

fn Clear(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
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
        d.items.clear();
        d.bitmap = None;
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

/// Fills device rectangle `r` (already clipped) with `paint`; partial pixels at the edges get
/// partial coverage.
fn fill(dc: u32, r: [f32; 4], paint: &Paint) {
    let inside = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, r)));
    let Some(r) = inside else { return };
    if r[2] <= r[0] || r[3] <= r[1] {
        return;
    }
    push(dc, gpu2d::RECT, r, [0.0; 4], [0.0; 4], paint);
}

fn brush(dc: u32, b: u32) -> Paint {
    let xf = st(|s| s.dcs.get(&dc).and_then(eff));
    st(|s| {
        s.brushes.get(&b).map(|b| match &b.linear {
            // (Direct2D keeps a solid color in 8 bits a channel: an alpha of 0.5 is 128/255)
            None => Paint::Solid([b.color[0], b.color[1], b.color[2], b.color[3] * b.opacity].map(|v| (v.clamp(0.0, 1.0) * 255.0 + 0.5).floor() / 255.0)),
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

/// An ellipse's (center, radii) through the transform (axis-aligned scale and translation).
fn xf_ellipse(dc: u32, p: u32) -> ([f32; 2], [f32; 2]) {
    let xf = st(|s| s.dcs.get(&dc).and_then(eff));
    let (cx, cy) = apply(xf, rf(p, 0), rf(p, 1));
    let (sx, sy) = xf.map_or((1.0, 1.0), |m| (m[0].hypot(m[1]), m[2].hypot(m[3])));
    ([cx, cy], [rf(p, 2) * sx, rf(p, 3) * sy])
}

fn ellipse(dc: u32, c: [f32; 2], r: [f32; 2], paint: &Paint, ring: Option<f32>) {
    let h = ring.map_or(0.0, |w| w / 2.0);
    let (ox, oy) = (r[0] + h, r[1] + h);
    let (ix, iy) = (r[0] - h, r[1] - h);
    let bbox = [c[0] - ox, c[1] - oy, c[0] + ox, c[1] + oy];
    let Some(rr) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, bbox))) else { return };
    if rr[2] <= rr[0] || rr[3] <= rr[1] {
        return;
    }
    let aliased = st(|s| s.dcs.get(&dc).is_some_and(|d| d.aliased));
    push(dc, gpu2d::ELLIPSE, rr, [c[0], c[1], ox, oy], [ix, iy, ring.is_some() as u32 as f32, aliased as u32 as f32], paint);
}

fn SetAntialiasMode(c: &mut Ctx) -> u32 {
    let a = c.a(1) == 1;
    st(|s| s.dcs.get_mut(&c.a(0)).map(|d| d.aliased = a));
    0
}
fn GetAntialiasMode(c: &mut Ctx) -> u32 {
    st(|s| s.dcs.get(&c.a(0)).is_some_and(|d| d.aliased)) as u32
}

/// Fills rectangle `r` (l, t, r, b) under the target's transform as a QUAD item (rotated or
/// skewed, or aliased), or the circle inscribed in it: false when the transform is singular.
fn quad(dc: u32, r: [f32; 4], circle: bool, paint: &Paint) -> bool {
    let Some((m, aliased)) = st(|s| s.dcs.get(&dc).map(|d| (eff(d).unwrap_or(IDENTITY), d.aliased))) else { return true };
    let (w, h) = (r[2] - r[0], r[3] - r[1]);
    // unit square -> device: x = a u + c v + e, y = b u + d v + f
    let (a, b, cc, d) = (w * m[0], w * m[1], h * m[2], h * m[3]);
    let (e, f) = (r[0] * m[0] + r[1] * m[2] + m[4], r[0] * m[1] + r[1] * m[3] + m[5]);
    let det = a * d - b * cc;
    if det == 0.0 || !det.is_finite() {
        return false;
    }
    let (ia, ib, ic, id) = (d / det, -b / det, -cc / det, a / det);
    let (ie, iff) = (-(ia * e + ic * f), -(ib * e + id * f));
    let xs = [e, e + a, e + cc, e + a + cc];
    let ys = [f, f + b, f + d, f + b + d];
    let bbox = [xs.iter().copied().fold(f32::MAX, f32::min), ys.iter().copied().fold(f32::MAX, f32::min), xs.iter().copied().fold(f32::MIN, f32::max), ys.iter().copied().fold(f32::MIN, f32::max)];
    let Some(rr) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, bbox))) else { return true };
    if rr[2] > rr[0] && rr[3] > rr[1] {
        push(dc, gpu2d::QUAD, rr, [ia, ib, ic, id], [ie, iff, aliased as u32 as f32, circle as u32 as f32], paint);
    }
    true
}
/// Whether shapes need QUAD items: a transform that turns or skews them, or aliased drawing.
fn needs_quad(dc: u32) -> bool {
    st(|s| s.dcs.get(&dc).is_some_and(|d| d.aliased || eff(d).is_some_and(|m| m[1] != 0.0 || m[2] != 0.0)))
}

fn FillEllipse(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    let (dc, p, b) = (c.a(0), c.a(1), c.a(2));
    if needs_quad(dc) {
        let (x, y, rx, ry) = (rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3));
        if quad(dc, [x - rx, y - ry, x + rx, y + ry], true, &brush(dc, b)) {
            return 0;
        }
    }
    let (ctr, r) = xf_ellipse(dc, p);
    ellipse(dc, ctr, r, &brush(dc, b), None);
    0
}
fn DrawEllipse(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    let (dc, p, b, w) = (c.a(0), c.a(1), c.a(2), fl(c, 3));
    let (ctr, r) = xf_ellipse(dc, p);
    ellipse(dc, ctr, r, &brush(dc, b), Some(w));
    0
}

fn FillRectangle(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    let (dc, p, b) = (c.a(0), c.a(1), c.a(2));
    let r = [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3)];
    if needs_quad(dc) && quad(dc, r, false, &brush(dc, b)) {
        return 0;
    }
    let Some(r) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, xf_rect(eff(d), r)))) else { return 0 };
    fill(dc, r, &brush(dc, b));
    0
}

fn DrawRectangle(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
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
        if needs_quad(dc) && quad(dc, e, false, &paint) {
            continue;
        }
        if let Some(e) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, xf_rect(eff(d), e)))) {
            fill(dc, e, &paint);
        }
    }
    0
}

fn PushAxisAlignedClip(c: &mut Ctx) -> u32 {
    let (dc, p) = (c.a(0), c.a(1));
    if !drawing(dc, Drawing::push) {
        return 0;
    }
    let r = [rf(p, 0), rf(p, 1), rf(p, 2), rf(p, 3)];
    st(|s| {
        if let Some(d) = s.dcs.get_mut(&dc) {
            let r = xf_rect(eff(d), r);
            d.clips.push(r);
        }
    });
    0
}
fn PopAxisAlignedClip(c: &mut Ctx) -> u32 {
    if drawing(c.a(0), Drawing::pop) {
        st(|s| s.dcs.get_mut(&c.a(0)).map(|d| d.clips.pop()));
    }
    0
}

/// Draws laid-out text with its box's top left at (x, y) (before the transform).
fn draw_text(dc: u32, text: &[u16], fmt: &dwrite::Format, w: f32, h: f32, x: f32, y: f32, b: u32, clip_box: bool) {
    let paint = brush(dc, b);
    let xf = st(|s| s.dcs.get(&dc).and_then(eff));
    // (a scaling transform scales the font; glyphs stay upright)
    let scale = xf.map_or(1.0, |m| (m[0] * m[3] - m[1] * m[2]).abs().sqrt());
    let mut f = fmt.clone();
    f.size *= scale;
    let laid = dwrite::layout(text, fmt, w, h);
    let Some(font) = dwrite::layout(&[], &f, 0.0, 0.0).font else { return };
    let chars = dwrite::chars(text);
    let clip_extra = if clip_box { Some(xf_rect(xf, [x, y, x + w, y + h])) } else { None };
    let Some(mut lim) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, [0.0, 0.0, d.w as f32, d.h as f32]))) else { return };
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
                let key = font.glyph_key(ch);
                let at = room(dc, || gpu2d::glyph(key, gw as u32, gh as u32, || cov.to_vec()));
                let shape = [at[0] as f32 - ox as f32, at[1] as f32 - oy as f32, 0.0, 0.0];
                push(dc, gpu2d::GLYPH, [x0 as f32, y0 as f32, x1 as f32, y1 as f32], shape, [0.0; 4], &paint);
            });
        }
    }
}

/// ID2D1RenderTarget::DrawGlyphRun (this, baseline origin, DWRITE_GLYPH_RUN *, brush,
/// measuring mode) and ID2D1DeviceContext's (a glyph run description before the brush).
fn DrawGlyphRun(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    let ((x, y), o) = c.pt_f(1);
    let desc = com::method_dwords(c.trap) == Some(7);
    let (dc, run, b) = (c.a(0), c.a(o), c.a(o + 1 + desc as u32));
    let ps = mem::ps();
    let face = dwrite::face_index(mem::rp(run));
    let em = f32::from_bits(r32(run + ps));
    let n = r32(run + ps + 4);
    let (idx, adv, off) = (mem::rp(run + ps + 8), mem::rp(run + 2 * ps + 8), mem::rp(run + 3 * ps + 8));
    let mut pen = 0.0;
    let mut glyphs = Vec::with_capacity(n as usize);
    for i in 0..n {
        let (dx, dy) = if off != 0 { (rf(off, 2 * i), rf(off, 2 * i + 1)) } else { (0.0, 0.0) };
        glyphs.push((mem::r16(idx + 2 * i), x + pen + dx, y - dy));
        pen += if adv != 0 { rf(adv, i) } else { 0.0 };
    }
    draw_glyphs(dc, face, em, &glyphs, b, 0.0);
    0
}

/// Draws glyphs (index, baseline x, y before the transform) of built-in face `face`; `grow`
/// > 0: their outlines stroked that wide (in DIPs) as well, the shape grown by half of it.
fn draw_glyphs(dc: u32, face: usize, em: f32, glyphs: &[(u16, f32, f32)], b: u32, grow: f32) {
    let paint = brush(dc, b);
    let xf = st(|s| s.dcs.get(&dc).and_then(eff));
    let scale = xf.map_or(1.0, |m| (m[0] * m[3] - m[1] * m[2]).abs().sqrt());
    let Some(font) = crate::gdi::face_font(face, em * scale) else { return };
    let Some(lim) = st(|s| s.dcs.get(&dc).map(|d| clip_rect(d, [0.0, 0.0, d.w as f32, d.h as f32]))) else { return };
    for &(gi, gx0, gy0) in glyphs {
        let (px, py) = apply(xf, gx0, gy0);
        let (px, py) = (px.round() as i32, py.round() as i32);
        font.index_glyph(gi, |gx, top, gw, gh, cov| {
            let r = grow * scale / 2.0;
            let (gx, top, gw, gh, cov) = if r > 0.0 {
                let (w2, h2, d) = dilate(cov, gw, gh, r);
                let k = r.ceil() as i32;
                (gx - k, top + k, w2, h2, std::borrow::Cow::Owned(d))
            } else {
                (gx, top, gw, gh, std::borrow::Cow::Borrowed(cov))
            };
            let (ox, oy) = (px + gx, py - top);
            let (x0, y0) = (ox.max(lim[0].ceil() as i32), oy.max(lim[1].ceil() as i32));
            let (x1, y1) = ((ox + gw).min(lim[2].floor() as i32), (oy + gh).min(lim[3].floor() as i32));
            if x1 <= x0 || y1 <= y0 {
                return;
            }
            let key = font.index_key(gi) ^ (r.to_bits() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
            let at = room(dc, || gpu2d::glyph(key, gw as u32, gh as u32, || cov.to_vec()));
            let shape = [at[0] as f32 - ox as f32, at[1] as f32 - oy as f32, 0.0, 0.0];
            push(dc, gpu2d::GLYPH, [x0 as f32, y0 as f32, x1 as f32, y1 as f32], shape, [0.0; 4], &paint);
        });
    }
}

/// A coverage bitmap grown by `r` pixels all round (a round pen): each pixel the most of its
/// neighbours' coverage within `r`, fading over the last pixel. The bitmap gets ceil(r) more
/// on each side.
pub fn dilate(cov: &[u8], w: i32, h: i32, r: f32) -> (i32, i32, Vec<u8>) {
    let k = r.ceil() as i32;
    let (w2, h2) = (w + 2 * k, h + 2 * k);
    let mut out = vec![0u8; (w2 * h2) as usize];
    // the kernel: offsets with their weight
    let mut ker = Vec::new();
    for dy in -k..=k {
        for dx in -k..=k {
            // (the pixel itself keeps all its coverage, however thin the pen)
            let wgt = if dx == 0 && dy == 0 { 1.0 } else { (r + 0.5 - ((dx * dx + dy * dy) as f32).sqrt()).clamp(0.0, 1.0) };
            if wgt > 0.0 {
                ker.push((dx, dy, wgt));
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            let v = cov[(y * w + x) as usize];
            if v == 0 {
                continue;
            }
            for &(dx, dy, wgt) in &ker {
                let o = ((y + k + dy) * w2 + x + k + dx) as usize;
                let nv = (v as f32 * wgt).round() as u8;
                if nv > out[o] {
                    out[o] = nv;
                }
            }
        }
    }
    (w2, h2, out)
}

// ---- geometries: what glyph outlines (IDWriteFontFace::GetGlyphRunOutline) make ----

/// A path geometry holds the glyphs outlined into it (face, em, index, x, y), and whether
/// other figures went in (not drawn); a transformed geometry its source and matrix.
enum Geom {
    Path { glyphs: Vec<(usize, f32, u16, f32, f32)>, figures: bool },
    Xformed { src: u32, m: Xf },
}
static GEOMS: Mutex<Option<HashMap<u32, Geom>>> = Mutex::new(None);
fn geoms<R>(f: impl FnOnce(&mut HashMap<u32, Geom>) -> R) -> R {
    f(GEOMS.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn geom_methods() -> Vec<(&'static str, Handler)> {
    vec![("Open", GeomOpen as Handler), ("GetFactory", GetFactoryOfGeom)]
}
fn CreatePathGeometry(c: &mut Ctx) -> u32 {
    let o = com::object("ID2D1PathGeometry1", &geom_methods(), 16);
    w32(o + mem::ps(), c.a(0));
    geoms(|m| m.insert(o, Geom::Path { glyphs: Vec::new(), figures: false }));
    mem::putp(c.a(1), o);
    0
}
fn GetFactoryOfGeom(c: &mut Ctx) -> u32 {
    let f = r32(c.a(0) + mem::ps());
    com::add_ref_obj(f);
    mem::putp(c.a(1), f);
    0
}
fn CreateTransformedGeometry(c: &mut Ctx) -> u32 {
    // (this, source, matrix *, out)
    let (src, m, out) = (c.a(1), c.a(2), c.a(3));
    let xf = [rf(m, 0), rf(m, 1), rf(m, 2), rf(m, 3), rf(m, 4), rf(m, 5)];
    let o = com::object("ID2D1TransformedGeometry", &[("GetFactory", GetFactoryOfGeom as Handler)], 16);
    w32(o + mem::ps(), c.a(0));
    com::add_ref_obj(src);
    geoms(|g| g.insert(o, Geom::Xformed { src, m: xf }));
    mem::putp(out, o);
    0
}
fn CreateStrokeStyle(c: &mut Ctx) -> u32 {
    // (this, properties, dashes, dash count, out): drawn as a round pen whatever it says
    mem::putp(c.a(4), com::object("ID2D1StrokeStyle1", &[], 8));
    0
}
/// Open(this, out sink): the sink records into the geometry
fn GeomOpen(c: &mut Ctx) -> u32 {
    let m: [(&'static str, Handler); 12] = [
        ("SetFillMode", ok),
        ("SetSegmentFlags", ok),
        ("BeginFigure", SinkFigure),
        ("AddLines", SinkFigure),
        ("AddBeziers", SinkFigure),
        ("EndFigure", ok),
        ("Close", ok),
        ("AddLine", SinkFigure),
        ("AddBezier", SinkFigure),
        ("AddQuadraticBezier", SinkFigure),
        ("AddQuadraticBeziers", SinkFigure),
        ("AddArc", SinkFigure),
    ];
    let o = com::object("ID2D1GeometrySink", &m, 16);
    w32(o + mem::ps(), c.a(0));
    com::add_ref_obj(c.a(0));
    mem::putp(c.a(1), o);
    0
}
fn SinkFigure(c: &mut Ctx) -> u32 {
    let g = r32(c.a(0) + mem::ps());
    geoms(|m| {
        if let Some(Geom::Path { figures, .. }) = m.get_mut(&g) {
            *figures = true;
        }
    });
    0
}
/// IDWriteFontFace::GetGlyphRunOutline into a sink of ours: the glyphs go in its geometry
/// (false: not our sink).
pub fn sink_glyphs(sink: u32, glyphs: Vec<(usize, f32, u16, f32, f32)>) -> bool {
    if sink == 0 || com::method_name(mem::rp(mem::rp(sink) + 9 * mem::ps())) != Some("Close") {
        return false;
    }
    let g = r32(sink + mem::ps());
    geoms(|m| match m.get_mut(&g) {
        Some(Geom::Path { glyphs: v, .. }) => {
            v.extend(glyphs);
            true
        }
        _ => false,
    })
}
/// The glyphs of a geometry with the transforms between applied (x, y; em scaled), and
/// whether it holds figures that are not drawn.
fn geom_glyphs(g: u32) -> (Vec<(usize, f32, u16, f32, f32)>, bool) {
    let mut m = IDENTITY;
    let mut g = g;
    for _ in 0..16 {
        match geoms(|t| match t.get(&g) {
            Some(Geom::Path { glyphs, figures }) => Some(Err((glyphs.clone(), *figures))),
            Some(Geom::Xformed { src, m }) => Some(Ok((*src, *m))),
            None => None,
        }) {
            Some(Ok((src, xf))) => {
                m = mul(xf, m);
                g = src;
            }
            Some(Err((glyphs, figures))) => {
                let s = (m[0] * m[3] - m[1] * m[2]).abs().sqrt();
                let v = glyphs.into_iter().map(|(f, em, gi, x, y)| (f, em * s, gi, m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])).collect();
                return (v, figures);
            }
            None => break,
        }
    }
    (Vec::new(), false)
}
/// a then b
fn mul(a: Xf, b: Xf) -> Xf {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}
fn draw_geom(dc: u32, g: u32, b: u32, width: f32) {
    let (glyphs, figures) = geom_glyphs(g);
    if figures {
        com::warn_once("sake: d2d geometry with figures other than glyph outlines: not drawn");
    }
    // (runs of the same face and size)
    let mut i = 0;
    while i < glyphs.len() {
        let (f, em) = (glyphs[i].0, glyphs[i].1);
        let mut j = i;
        while j < glyphs.len() && glyphs[j].0 == f && glyphs[j].1 == em {
            j += 1;
        }
        let run: Vec<(u16, f32, f32)> = glyphs[i..j].iter().map(|g| (g.2, g.3, g.4)).collect();
        draw_glyphs(dc, f, em, &run, b, width);
        i = j;
    }
}
fn DrawGeometry(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    // (this, geometry, brush, stroke width, stroke style)
    draw_geom(c.a(0), c.a(1), c.a(2), fl(c, 3).max(0.001));
    0
}
fn FillGeometry(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    // (this, geometry, brush, opacity brush)
    draw_geom(c.a(0), c.a(1), c.a(2), 0.0);
    0
}

fn DrawText(c: &mut Ctx) -> u32 {
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
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
    if !drawing(c.a(0), Drawing::draw) {
        return 0;
    }
    // (this, origin (by value), layout, brush, options)
    let ((x, y), o) = c.pt_f(1);
    let (dc, lay, b, opts) = (c.a(0), c.a(o), c.a(o + 1), c.a(o + 2));
    let Some((text, f, w, h)) = dwrite::layout_of(lay) else { return 0 };
    draw_text(dc, &text, &f, w, h, x, y, b, opts & 2 != 0);
    0
}

// ---- matrix helpers (x86: floats on the stack) ----

fn put_matrix(out: u32, m: [f32; 6]) {
    for (i, v) in m.iter().enumerate() {
        w32(out + 4 * i as u32, v.to_bits());
    }
}
/// D2D1MakeRotateMatrix(angle in degrees, center, out)
fn D2D1MakeRotateMatrix(c: &mut Ctx) -> u32 {
    let (deg, ((cx, cy), o)) = (fl(c, 0), c.pt_f(1));
    let out = c.a(o);
    // (multiples of 90 degrees exactly, as Windows gives them)
    let (s, co) = if deg % 90.0 == 0.0 {
        [(0.0, 1.0), (1.0, 0.0), (0.0, -1.0), (-1.0, 0.0)][((deg / 90.0).rem_euclid(4.0)) as usize]
    } else {
        deg.to_radians().sin_cos()
    };
    put_matrix(out, [co, s, -s, co, cx - cx * co + cy * s, cy - cx * s - cy * co]);
    0
}
/// D2D1MakeSkewMatrix(angle x, angle y, center, out)
fn D2D1MakeSkewMatrix(c: &mut Ctx) -> u32 {
    let (tx, ty, ((cx, cy), o)) = (fl(c, 0).to_radians().tan(), fl(c, 1).to_radians().tan(), c.pt_f(2));
    let out = c.a(o);
    put_matrix(out, [1.0, ty, tx, 1.0, -cy * tx, -cx * ty]);
    0
}
fn det(m: u32) -> f32 {
    rf(m, 0) * rf(m, 3) - rf(m, 1) * rf(m, 2)
}
fn D2D1IsMatrixInvertible(c: &mut Ctx) -> u32 {
    (det(c.a(0)) != 0.0) as u32
}
fn D2D1InvertMatrix(c: &mut Ctx) -> u32 {
    let m = c.a(0);
    let d = det(m);
    if d == 0.0 {
        return 0;
    }
    let [a, b, cc, dd, x, y] = [rf(m, 0), rf(m, 1), rf(m, 2), rf(m, 3), rf(m, 4), rf(m, 5)];
    put_matrix(m, [dd / d, -b / d, -cc / d, a / d, (cc * y - dd * x) / d, (b * x - a * y) / d]);
    1
}

fn D2D1SinCos(c: &mut Ctx) -> u32 {
    let (s, co) = fl(c, 0).sin_cos();
    w32(c.a(1), s.to_bits());
    w32(c.a(2), co.to_bits());
    0
}
fn D2D1Tan(c: &mut Ctx) -> u32 {
    c.st0 = Some(fl(c, 0).tan() as f64);
    0
}

pub fn init() {
    implement("d2d1", "D2D1SinCos", D2D1SinCos);
    implement("d2d1", "D2D1Tan", D2D1Tan);
    implement("d2d1", "D2D1MakeRotateMatrix", D2D1MakeRotateMatrix);
    implement("d2d1", "D2D1MakeSkewMatrix", D2D1MakeSkewMatrix);
    implement("d2d1", "D2D1IsMatrixInvertible", D2D1IsMatrixInvertible);
    implement("d2d1", "D2D1InvertMatrix", D2D1InvertMatrix);
    implement("d2d1", "D2D1CreateFactory", D2D1CreateFactory);
    implement("d2d1", "D2D1CreateDevice", D2D1CreateDevice);
    implement("d2d1", "D2D1CreateDeviceContext", D2D1CreateDeviceContext);
}

#[cfg(test)]
mod draw_tests;

#[cfg(test)]
mod source_tests {
    use super::*;

    fn state() -> State {
        let mut s = State::default();
        // context 1 draws into texture 0x100 (bitmap 10), context 2 into 0x200 (bitmap 20,
        // a compatible render target's: an image too); bitmap 30 is texture 0x300 made with
        // D2D1_ALPHA_MODE_IGNORE, 40 a WIC image, 50 texture 0x100 again (another bitmap of
        // context 1's target)
        for (dc, bmp, t) in [(1, 10, 0x100), (2, 20, 0x200)] {
            s.dcs.insert(dc, Dc { target: bmp, surface: t, ..Default::default() });
            s.bitmaps.insert(bmp, (t, 64, 32));
        }
        s.images.insert(20, Image { gpu: 7, w: 64, h: 32, opaque: false });
        s.bitmaps.insert(30, (0x300, 16, 8));
        s.opaque.insert(30);
        s.images.insert(40, Image { gpu: 9, w: 4, h: 4, opaque: true });
        s.bitmaps.insert(50, (0x100, 64, 32));
        s
    }

    /// The black screen of KiriKiri's DrawDeviceD2D (AMBITIOUS MISSION after episode 1): the
    /// game draws its picture into a D3D11 texture and DrawBitmap's a bitmap of that texture
    /// onto the window's swap chain; sake drew nothing for such a bitmap.
    #[test]
    fn surface_bitmaps_are_drawn() {
        let s = state();
        assert_eq!(source(&s, 2, 30), Source::Surface { surface: 0x300, w: 16, h: 8, opaque: true });
        assert_eq!(source(&s, 2, 10), Source::Surface { surface: 0x100, w: 64, h: 32, opaque: false });
        assert_eq!(source(&s, 1, 20), Source::Image(Image { gpu: 7, w: 64, h: 32, opaque: false }));
        assert_eq!(source(&s, 1, 40), Source::Image(Image { gpu: 9, w: 4, h: 4, opaque: true }));
    }

    /// A context's own target, by any bitmap of its texture, is not drawn into itself.
    #[test]
    fn own_target_is_not_a_source() {
        let s = state();
        assert_eq!(source(&s, 1, 10), Source::Target);
        assert_eq!(source(&s, 1, 50), Source::Target);
        assert_eq!(source(&s, 2, 20), Source::Target);
    }

    #[test]
    fn released_or_unknown_bitmaps_draw_nothing() {
        let mut s = state();
        s.bitmaps.remove(&30);
        s.opaque.remove(&30);
        assert_eq!(source(&s, 2, 30), Source::None);
        assert_eq!(source(&s, 2, 0), Source::None);
        assert_eq!(source(&s, 99, 999), Source::None);
        // (a texture the device could not give: surface 0)
        s.bitmaps.insert(60, (0, 0, 0));
        assert_eq!(source(&s, 1, 60), Source::None);
    }

    proptest::proptest! {
        /// Any mix of contexts and bitmaps: Target exactly when the bitmap's texture is the
        /// context's, an image's own pixels otherwise when it has them, its texture when it has
        /// only that, and nothing for what is neither.
        #[test]
        fn source_model(dcs in proptest::collection::vec((1u32..4, 0u32..4), 0..4), bmps in proptest::collection::vec((1u32..8, 0u32..4, proptest::bool::ANY, proptest::bool::ANY), 0..8), dc in 0u32..5, bmp in 0u32..9) {
            let mut s = State::default();
            for &(k, t) in &dcs {
                s.dcs.insert(k, Dc { surface: t * 0x100, ..Default::default() });
            }
            for &(k, t, image, opaque) in &bmps {
                s.bitmaps.insert(k, (t * 0x100, k, k));
                if image {
                    s.images.insert(k, Image { gpu: k + 1000, w: k, h: k, opaque });
                }
                if opaque {
                    s.opaque.insert(k);
                }
            }
            let got = source(&s, dc, bmp);
            let t = s.bitmaps.get(&bmp).map(|b| b.0);
            let mine = s.dcs.get(&dc).map(|d| d.surface);
            let want = if t.is_some_and(|t| t != 0 && Some(t) == mine) {
                Source::Target
            } else if let Some(&im) = s.images.get(&bmp) {
                Source::Image(im)
            } else if let Some(t) = t.filter(|&t| t != 0) {
                Source::Surface { surface: t, w: bmp, h: bmp, opaque: s.opaque.contains(&bmp) }
            } else {
                Source::None
            };
            proptest::prop_assert_eq!(got, want);
        }
    }
}

#[cfg(test)]
mod geom_tests {
    use super::*;

    #[test]
    fn dilating_a_dot_makes_a_round_spot() {
        // one full pixel grown by 1: its 4 neighbours half covered, the corners barely
        let (w, h, d) = dilate(&[255], 1, 1, 1.0);
        assert_eq!((w, h), (3, 3));
        assert_eq!(d, vec![22, 128, 22, 128, 255, 128, 22, 128, 22]);
        // grown by 2: the direct neighbours full, 2 away half
        let (w, _, d) = dilate(&[255], 1, 1, 2.0);
        assert_eq!(w, 5);
        assert_eq!((d[12], d[13], d[14]), (255, 255, 128));
    }

    proptest::proptest! {
        /// A dilated bitmap keeps every pixel's coverage where it was (shifted by the border),
        /// grows by ceil(r) on each side, and a wider pen covers at least as much.
        #[test]
        fn dilation_only_adds(cov in proptest::collection::vec(proptest::prop_oneof![proptest::strategy::Just(0u8), 0u8..=255], 1..30), w in 1i32..6, r in 0.1f32..3.0) {
            let h = (cov.len() as i32 + w - 1) / w;
            let mut c = cov.clone();
            c.resize((w * h) as usize, 0);
            let (w2, h2, d) = dilate(&c, w, h, r);
            let k = r.ceil() as i32;
            proptest::prop_assert_eq!((w2, h2), (w + 2 * k, h + 2 * k));
            for y in 0..h {
                for x in 0..w {
                    proptest::prop_assert!(d[((y + k) * w2 + x + k) as usize] >= c[(y * w + x) as usize]);
                }
            }
            let (w3, _, d3) = dilate(&c, w, h, r + 1.0);
            let k3 = (r + 1.0).ceil() as i32;
            for y in 0..h2 {
                for x in 0..w2 {
                    proptest::prop_assert!(d3[((y + k3 - k) * w3 + x + k3 - k) as usize] >= d[(y * w2 + x) as usize]);
                }
            }
        }

        /// mul composes transforms: applying a then b to a point is applying mul(a, b).
        #[test]
        fn transforms_compose(a in proptest::array::uniform6(-4f32..4.0), b in proptest::array::uniform6(-4f32..4.0), x in -9f32..9.0, y in -9f32..9.0) {
            let ap = |m: Xf, x: f32, y: f32| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
            let (x1, y1) = ap(a, x, y);
            let (x2, y2) = ap(b, x1, y1);
            let (x3, y3) = ap(mul(a, b), x, y);
            proptest::prop_assert!((x2 - x3).abs() < 1e-2 && (y2 - y3).abs() < 1e-2);
        }
    }
}
