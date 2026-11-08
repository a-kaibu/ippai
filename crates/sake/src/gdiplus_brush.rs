//! GDI+ brushes, matrices and paths (gdiplus.dll): solid, hatch, texture, linear gradient and
//! path gradient brushes with their properties (blends, preset blends, wrap modes,
//! transforms, surround colors), what each paints at a pixel (`paint`, for gdiplus_text's
//! FillRectangle), 3x2 matrices and paths of points (enough for ellipses and polygons).
//!
//! Matrices are GDI+'s row-vector ones: [m11 m12; m21 m22; dx dy], a point times the matrix;
//! "prepend" multiplies the operation in front (applied first), "append" behind.
//!
//! A linear gradient keeps its rectangle and a transform from that rectangle's space to the
//! world: the colors go left to right across the rectangle, and the transform turns that the
//! way of the gradient line (or angle) while keeping the rectangle's center (winref gdipbrush).

#![allow(non_snake_case)]

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, put32, r32, w32};
use crate::HashMap;
use std::sync::Mutex;

const OK: u32 = 0;
const GENERIC_ERROR: u32 = 1;
const INVALID_PARAMETER: u32 = 2;
const OUT_OF_MEMORY: u32 = 3;
const INSUFFICIENT_BUFFER: u32 = 5;
const NOT_IMPLEMENTED: u32 = 6;

/// WrapMode
const TILE: u32 = 0;
const FLIP_X: u32 = 1;
const FLIP_Y: u32 = 2;
const FLIP_XY: u32 = 3;
const CLAMP: u32 = 4;

pub(crate) type M = [f32; 6];
pub(crate) const IDENTITY: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// a × b: a's transformation, then b's
pub(crate) fn mul(a: &M, b: &M) -> M {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}
fn det(m: &M) -> f32 {
    m[0] * m[3] - m[1] * m[2]
}
pub(crate) fn invert(m: &M) -> Option<M> {
    let d = det(m);
    if d == 0.0 || !d.is_finite() {
        return None;
    }
    let (a, b, c, e) = (m[3] / d, -m[1] / d, -m[2] / d, m[0] / d);
    Some([a, b, c, e, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * e)])
}
pub(crate) fn apply(m: &M, x: f32, y: f32) -> (f32, f32) {
    (x * m[0] + y * m[2] + m[4], x * m[1] + y * m[3] + m[5])
}
fn rotation(deg: f32) -> M {
    let r = (deg as f64).to_radians();
    let (s, c) = (r.sin() as f32, r.cos() as f32);
    [c, s, -s, c, 0.0, 0.0]
}
/// `op` prepended (MatrixOrderPrepend 0) or appended (1) to `m`
fn combine(m: &mut M, op: &M, order: u32) -> u32 {
    match order {
        0 => *m = mul(op, m),
        1 => *m = mul(m, op),
        _ => return INVALID_PARAMETER,
    }
    OK
}

#[derive(Clone)]
struct Blend {
    /// (factor, position) pairs; empty: the default, a factor of 1 at the end (colors go
    /// straight from the first to the second)
    factors: Vec<(f32, f32)>,
    /// (color, position) pairs replacing the brush's colors; empty: none
    preset: Vec<(u32, f32)>,
}

impl Blend {
    fn new() -> Blend {
        Blend { factors: Vec::new(), preset: Vec::new() }
    }
    /// The color at `t` (0..=1) between `a` and `b`, premultiplied
    fn color_pm(&self, a: u32, b: u32, t: f32) -> u32 {
        if !self.preset.is_empty() {
            let p = &self.preset;
            let k = p.iter().position(|&(_, pos)| pos >= t).unwrap_or(p.len() - 1);
            if k == 0 {
                return premul_argb(p[0].0);
            }
            let ((c0, p0), (c1, p1)) = (p[k - 1], p[k]);
            return lerp(premul_argb(c0), premul_argb(c1), if p1 > p0 { (t - p0) / (p1 - p0) } else { 1.0 });
        }
        lerp(premul_argb(a), premul_argb(b), interp(&self.factors, t))
    }
    fn color(&self, a: u32, b: u32, t: f32) -> u32 {
        straight_argb(self.color_pm(a, b, t))
    }
}

/// The blend factor at `t` of (factor, position) pairs (straight lines between them)
fn interp(f: &[(f32, f32)], t: f32) -> f32 {
    if f.len() < 2 {
        return t * f.first().map_or(1.0, |p| p.0);
    }
    let k = f.iter().position(|&(_, pos)| pos >= t).unwrap_or(f.len() - 1);
    if k == 0 {
        return f[0].0;
    }
    let ((f0, p0), (f1, p1)) = (f[k - 1], f[k]);
    if p1 > p0 { f0 + (f1 - f0) * (t - p0) / (p1 - p0) } else { f1 }
}

/// Straight-alpha ARGB a..b at t, each channel rounded
pub(crate) fn lerp(a: u32, b: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |s: u32| {
        let (x, y) = ((a >> s & 255) as f32, (b >> s & 255) as f32);
        ((x + (y - x) * t).round() as u32).min(255) << s
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

/// a..b at coverage k of 255 as GDI+ mixes a hatch's edges: each premultiplied channel the
/// rounded parts of both, made straight again with GDI+'s reciprocal (winref gdipbrush:
/// 80ff0000 over 400000ff at 64 is 50660099, at 234 7af4000a)
pub(crate) fn mix_premul(a: u32, b: u32, k: u32) -> u32 {
    use crate::gdiplus::{premul, unpremul};
    let (k, aa, ba) = (k.min(255) as u8, (a >> 24) as u8, (b >> 24) as u8);
    let pm = |c: u32, s: u32, al: u8| premul((c >> s) as u8, al);
    let alpha = premul(aa, 255 - k) + premul(ba, k);
    let ch = |s: u32| {
        let v = premul(pm(a, s, aa), 255 - k) + premul(pm(b, s, ba), k);
        (unpremul(v, alpha) as u32) << s
    };
    (alpha as u32) << 24 | ch(16) | ch(8) | ch(0)
}

pub(crate) fn premul_argb(c: u32) -> u32 {
    let a = (c >> 24) as u8;
    let ch = |s: u32| (crate::gdiplus::premul((c >> s) as u8, a) as u32) << s;
    c & 0xff00_0000 | ch(16) | ch(8) | ch(0)
}
/// premultiplied back to straight with GDI+'s reciprocal (winref gdipbrush grad_alpha: a 0 to
/// white gradient's 112/112 is 254)
pub(crate) fn straight_argb(c: u32) -> u32 {
    let a = (c >> 24) as u8;
    let ch = |s: u32| (crate::gdiplus::unpremul((c >> s) as u8, a) as u32) << s;
    c & 0xff00_0000 | ch(16) | ch(8) | ch(0)
}

/// A linear gradient's colors as GDI+ paints them: worked out at each whole unit of the
/// brush rectangle's width (premultiplied, rounded), and the pixels between those mixed of
/// the two around them (winref gdipbrush: a 0.3 preset stop over 16 units shows at 4.5 units
/// only half; a 16 unit blend's 7.5 is the mean of 7's 147 and 8's 128, 138)
fn line_lut(rect: &[f32; 4], colors: &[u32; 2], blend: &Blend) -> Vec<u32> {
    let n = (rect[2].abs().round() as usize).clamp(1, 4096);
    (0..=n).map(|k| blend.color_pm(colors[0], colors[1], k as f32 / n as f32)).collect()
}
fn lut_at(lut: &[u32], t: f32) -> u32 {
    let n = lut.len() - 1;
    let s = t.clamp(0.0, 1.0) * n as f32;
    let k = (s.floor() as usize).min(n);
    if k == n {
        return lut[n];
    }
    lerp(lut[k], lut[k + 1], s - k as f32)
}

#[derive(Clone)]
enum Brush {
    Solid(u32),
    Hatch { style: u32, fore: u32, back: u32 },
    /// the image's straight BGRA pixels, copied when the brush is made
    Texture { w: u32, h: u32, px: Vec<u8>, wrap: u32, m: M },
    Line { rect: [f32; 4], colors: [u32; 2], wrap: u32, m: M, blend: Blend, gamma: bool },
    PathGrad { pts: Vec<[f32; 2]>, center: [f32; 2], cc: u32, surround: Vec<u32>, wrap: u32, m: M, blend: Blend, focus: [f32; 2], gamma: bool },
}

impl Brush {
    /// GdipGetBrushType: BrushTypeSolidColor 0, HatchFill 1, TextureFill 2, PathGradient 3,
    /// LinearGradient 4
    fn kind(&self) -> u32 {
        match self {
            Brush::Solid(_) => 0,
            Brush::Hatch { .. } => 1,
            Brush::Texture { .. } => 2,
            Brush::PathGrad { .. } => 3,
            Brush::Line { .. } => 4,
        }
    }
}

#[derive(Clone)]
enum Obj {
    Matrix(M),
    /// points and PathPointTypes (0 start, 1 line, 3 Bezier; 0x80 closes the figure)
    Path { pts: Vec<[f32; 2]>, types: Vec<u8>, fill: u32 },
    Brush(Brush),
    Cap(Cap),
}

/// A GpCustomLineCap, or a GpAdjustableArrowCap (`arrow`: height, width, middle inset, filled).
/// Its settings only: lines are drawn without the cap's path.
#[derive(Clone)]
struct Cap {
    base: u32,
    inset: f32,
    join: u32,
    scale: f32,
    caps: (u32, u32),
    arrow: Option<(f32, f32, f32, bool)>,
}

static OBJS: Mutex<Option<HashMap<u32, Obj>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Obj>) -> R) -> R {
    f(OBJS.lock().unwrap().get_or_insert_with(HashMap::default))
}
fn new_obj(o: Obj, out: u32) -> u32 {
    if out == 0 {
        return INVALID_PARAMETER;
    }
    let h = mem::heap_alloc(16, true);
    with(|m| m.insert(h, o));
    mem::putp(out, h);
    OK
}
fn delete_obj(h: u32, f: fn(&Obj) -> bool) -> u32 {
    let gone = with(|m| match m.get(&h) {
        Some(o) if f(o) => m.remove(&h).is_some(),
        _ => false,
    });
    if !gone {
        return INVALID_PARAMETER;
    }
    mem::heap_free(h);
    OK
}
fn matrix(h: u32) -> Option<M> {
    with(|m| match m.get(&h) {
        Some(Obj::Matrix(x)) => Some(*x),
        _ => None,
    })
}
/// `f` on matrix `h`'s elements
fn on_matrix(h: u32, f: impl FnOnce(&mut M) -> u32) -> u32 {
    with(|m| match m.get_mut(&h) {
        Some(Obj::Matrix(x)) => f(x),
        _ => INVALID_PARAMETER,
    })
}
/// `f` on brush `h`
fn on_brush(h: u32, f: impl FnOnce(&mut Brush) -> u32) -> u32 {
    with(|m| match m.get_mut(&h) {
        Some(Obj::Brush(b)) => f(b),
        _ => INVALID_PARAMETER,
    })
}
fn rf(p: u32) -> f32 {
    f32::from_bits(r32(p))
}
fn wf(p: u32, v: f32) {
    w32(p, v.to_bits())
}
fn read_rect(p: u32, int: bool) -> [f32; 4] {
    std::array::from_fn(|i| if int { r32(p + 4 * i as u32) as i32 as f32 } else { rf(p + 4 * i as u32) })
}
fn read_pts(p: u32, n: u32, int: bool) -> Vec<[f32; 2]> {
    (0..n).map(|i| [0, 1].map(|k| {
        let a = p + 8 * i + 4 * k;
        if int { r32(a) as i32 as f32 } else { rf(a) }
    })).collect()
}

// ---- matrices ----

fn GdipCreateMatrix(c: &mut Ctx) -> u32 {
    new_obj(Obj::Matrix(IDENTITY), c.a(0))
}
fn GdipCreateMatrix2(c: &mut Ctx) -> u32 {
    new_obj(Obj::Matrix(std::array::from_fn(|i| c.f(i as u32))), c.a(6))
}
/// GdipCreateMatrix3(rect, dstplg[3], &matrix): the rectangle onto the parallelogram whose
/// top-left, top-right and bottom-left corners are dstplg's
fn create_matrix3(c: &mut Ctx, int: bool) -> u32 {
    let (rp, pp, out) = (c.a(0), c.a(1), c.a(2));
    if rp == 0 || pp == 0 {
        return INVALID_PARAMETER;
    }
    let r = read_rect(rp, int);
    let p = read_pts(pp, 3, int);
    if r[2] == 0.0 || r[3] == 0.0 {
        return OUT_OF_MEMORY;
    }
    let (m11, m12) = ((p[1][0] - p[0][0]) / r[2], (p[1][1] - p[0][1]) / r[2]);
    let (m21, m22) = ((p[2][0] - p[0][0]) / r[3], (p[2][1] - p[0][1]) / r[3]);
    let m = [m11, m12, m21, m22, p[0][0] - m11 * r[0] - m21 * r[1], p[0][1] - m12 * r[0] - m22 * r[1]];
    new_obj(Obj::Matrix(m), out)
}
fn GdipCreateMatrix3(c: &mut Ctx) -> u32 {
    create_matrix3(c, false)
}
fn GdipCreateMatrix3I(c: &mut Ctx) -> u32 {
    create_matrix3(c, true)
}
fn GdipCloneMatrix(c: &mut Ctx) -> u32 {
    let Some(m) = matrix(c.a(0)) else { return INVALID_PARAMETER };
    new_obj(Obj::Matrix(m), c.a(1))
}
fn GdipDeleteMatrix(c: &mut Ctx) -> u32 {
    delete_obj(c.a(0), |o| matches!(o, Obj::Matrix(_)))
}
fn GdipSetMatrixElements(c: &mut Ctx) -> u32 {
    let e: M = std::array::from_fn(|i| c.f(i as u32 + 1));
    on_matrix(c.a(0), |m| {
        *m = e;
        OK
    })
}
fn GdipGetMatrixElements(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    let Some(m) = matrix(c.a(0)).filter(|_| p != 0) else { return INVALID_PARAMETER };
    for (i, v) in m.iter().enumerate() {
        wf(p + 4 * i as u32, *v);
    }
    OK
}
fn GdipMultiplyMatrix(c: &mut Ctx) -> u32 {
    let Some(b) = matrix(c.a(1)) else { return INVALID_PARAMETER };
    let order = c.a(2);
    on_matrix(c.a(0), |m| combine(m, &b, order))
}
fn GdipTranslateMatrix(c: &mut Ctx) -> u32 {
    let op = [1.0, 0.0, 0.0, 1.0, c.f(1), c.f(2)];
    let order = c.a(3);
    on_matrix(c.a(0), |m| combine(m, &op, order))
}
fn GdipScaleMatrix(c: &mut Ctx) -> u32 {
    let op = [c.f(1), 0.0, 0.0, c.f(2), 0.0, 0.0];
    let order = c.a(3);
    on_matrix(c.a(0), |m| combine(m, &op, order))
}
fn GdipRotateMatrix(c: &mut Ctx) -> u32 {
    let op = rotation(c.f(1));
    let order = c.a(2);
    on_matrix(c.a(0), |m| combine(m, &op, order))
}
/// GdipShearMatrix(m, shearX, shearY, order): x' = x + shearX y, y' = shearY x + y
fn GdipShearMatrix(c: &mut Ctx) -> u32 {
    let op = [1.0, c.f(2), c.f(1), 1.0, 0.0, 0.0];
    let order = c.a(3);
    on_matrix(c.a(0), |m| combine(m, &op, order))
}
fn GdipInvertMatrix(c: &mut Ctx) -> u32 {
    on_matrix(c.a(0), |m| match invert(m) {
        Some(i) => {
            *m = i;
            OK
        }
        None => INVALID_PARAMETER,
    })
}
fn GdipIsMatrixInvertible(c: &mut Ctx) -> u32 {
    let (Some(m), out) = (matrix(c.a(0)), c.a(1)) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    w32(out, invert(&m).is_some() as u32);
    OK
}
fn GdipIsMatrixIdentity(c: &mut Ctx) -> u32 {
    let (Some(m), out) = (matrix(c.a(0)), c.a(1)) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    w32(out, (m == IDENTITY) as u32);
    OK
}
fn GdipIsMatrixEqual(c: &mut Ctx) -> u32 {
    let (Some(a), Some(b), out) = (matrix(c.a(0)), matrix(c.a(1)), c.a(2)) else { return INVALID_PARAMETER };
    if out == 0 {
        return INVALID_PARAMETER;
    }
    w32(out, (a == b) as u32);
    OK
}
/// GdipTransformMatrixPoints / ..I / GdipVectorTransformMatrixPoints(..I)(m, pts, count)
fn transform_points(c: &mut Ctx, int: bool, vector: bool) -> u32 {
    let (p, n) = (c.a(1), c.i(2));
    let Some(mut m) = matrix(c.a(0)) else { return INVALID_PARAMETER };
    if p == 0 || n <= 0 {
        return INVALID_PARAMETER;
    }
    if vector {
        m[4] = 0.0;
        m[5] = 0.0;
    }
    for i in 0..n as u32 {
        let a = p + 8 * i;
        if int {
            let (x, y) = apply(&m, r32(a) as i32 as f32, r32(a + 4) as i32 as f32);
            w32(a, x.round() as i32 as u32);
            w32(a + 4, y.round() as i32 as u32);
        } else {
            let (x, y) = apply(&m, rf(a), rf(a + 4));
            wf(a, x);
            wf(a + 4, y);
        }
    }
    OK
}
fn GdipTransformMatrixPoints(c: &mut Ctx) -> u32 {
    transform_points(c, false, false)
}
fn GdipTransformMatrixPointsI(c: &mut Ctx) -> u32 {
    transform_points(c, true, false)
}
fn GdipVectorTransformMatrixPoints(c: &mut Ctx) -> u32 {
    transform_points(c, false, true)
}
fn GdipVectorTransformMatrixPointsI(c: &mut Ctx) -> u32 {
    transform_points(c, true, true)
}

// ---- paths ----

/// The 13 points (4 Beziers) GDI+ makes an ellipse of, starting at its right end
pub(crate) fn ellipse_points(x: f32, y: f32, w: f32, h: f32) -> Vec<[f32; 2]> {
    const K: f32 = 0.552_284_8; // 4 (sqrt 2 - 1) / 3
    let (rx, ry) = (w / 2.0, h / 2.0);
    let (cx, cy) = (x + rx, y + ry);
    let u: [(f32, f32); 13] = [
        (1.0, 0.0), (1.0, K), (K, 1.0), (0.0, 1.0), (-K, 1.0), (-1.0, K), (-1.0, 0.0),
        (-1.0, -K), (-K, -1.0), (0.0, -1.0), (K, -1.0), (1.0, -K), (1.0, 0.0),
    ];
    u.iter().map(|&(a, b)| [cx + a * rx, cy + b * ry]).collect()
}

fn on_path(h: u32, f: impl FnOnce(&mut Vec<[f32; 2]>, &mut Vec<u8>, &mut u32) -> u32) -> u32 {
    with(|m| match m.get_mut(&h) {
        Some(Obj::Path { pts, types, fill }) => f(pts, types, fill),
        _ => INVALID_PARAMETER,
    })
}
fn path_points(h: u32) -> Option<Vec<[f32; 2]>> {
    with(|m| match m.get(&h) {
        Some(Obj::Path { pts, .. }) => Some(pts.clone()),
        _ => None,
    })
}
fn GdipCreatePath(c: &mut Ctx) -> u32 {
    new_obj(Obj::Path { pts: Vec::new(), types: Vec::new(), fill: c.a(0) }, c.a(1))
}
/// GdipCreatePath2(points, types, count, fill, &path) / ..I
fn create_path2(c: &mut Ctx, int: bool) -> u32 {
    let (pp, tp, n) = (c.a(0), c.a(1), c.i(2));
    if pp == 0 || tp == 0 || n < 0 {
        return INVALID_PARAMETER;
    }
    let pts = read_pts(pp, n as u32, int);
    let types = mem::slice(tp, n as u32).to_vec();
    new_obj(Obj::Path { pts, types, fill: c.a(3) }, c.a(4))
}
fn GdipCreatePath2(c: &mut Ctx) -> u32 {
    create_path2(c, false)
}
fn GdipCreatePath2I(c: &mut Ctx) -> u32 {
    create_path2(c, true)
}
fn GdipClonePath(c: &mut Ctx) -> u32 {
    let Some(o) = with(|m| m.get(&c.a(0)).filter(|o| matches!(o, Obj::Path { .. })).cloned()) else { return INVALID_PARAMETER };
    new_obj(o, c.a(1))
}
fn GdipDeletePath(c: &mut Ctx) -> u32 {
    delete_obj(c.a(0), |o| matches!(o, Obj::Path { .. }))
}
fn GdipResetPath(c: &mut Ctx) -> u32 {
    on_path(c.a(0), |p, t, f| {
        p.clear();
        t.clear();
        *f = 0;
        OK
    })
}
/// appends a figure: its first point starts it, the others are of `kind`; `close` closes it
fn add_figure(h: u32, pts: Vec<[f32; 2]>, kind: u8, close: bool) -> u32 {
    on_path(h, |p, t, _| {
        let n = pts.len();
        for (i, q) in pts.into_iter().enumerate() {
            let mut ty = if i == 0 { 0 } else { kind };
            if close && i + 1 == n {
                ty |= 0x80;
            }
            p.push(q);
            t.push(ty);
        }
        OK
    })
}
fn GdipAddPathEllipse(c: &mut Ctx) -> u32 {
    add_figure(c.a(0), ellipse_points(c.f(1), c.f(2), c.f(3), c.f(4)), 3, true)
}
fn GdipAddPathEllipseI(c: &mut Ctx) -> u32 {
    let [x, y, w, h] = [1, 2, 3, 4].map(|i| c.i(i) as f32);
    add_figure(c.a(0), ellipse_points(x, y, w, h), 3, true)
}
fn rect_points(x: f32, y: f32, w: f32, h: f32) -> Vec<[f32; 2]> {
    vec![[x, y], [x + w, y], [x + w, y + h], [x, y + h]]
}
fn GdipAddPathRectangle(c: &mut Ctx) -> u32 {
    add_figure(c.a(0), rect_points(c.f(1), c.f(2), c.f(3), c.f(4)), 1, true)
}
fn GdipAddPathRectangleI(c: &mut Ctx) -> u32 {
    let [x, y, w, h] = [1, 2, 3, 4].map(|i| c.i(i) as f32);
    add_figure(c.a(0), rect_points(x, y, w, h), 1, true)
}
/// GdipAddPathPolygon(path, points, count) / ..I: at least 3 points
fn add_polygon(c: &mut Ctx, int: bool) -> u32 {
    let (pp, n) = (c.a(1), c.i(2));
    if pp == 0 || n < 3 {
        return INVALID_PARAMETER;
    }
    add_figure(c.a(0), read_pts(pp, n as u32, int), 1, true)
}
fn GdipAddPathPolygon(c: &mut Ctx) -> u32 {
    add_polygon(c, false)
}
fn GdipAddPathPolygonI(c: &mut Ctx) -> u32 {
    add_polygon(c, true)
}
fn GdipGetPointCount(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let Some(p) = path_points(c.a(0)).filter(|_| out != 0) else { return INVALID_PARAMETER };
    w32(out, p.len() as u32);
    OK
}
fn GdipGetPathPoints(c: &mut Ctx) -> u32 {
    let (out, n) = (c.a(1), c.i(2));
    let Some(p) = path_points(c.a(0)).filter(|_| out != 0) else { return INVALID_PARAMETER };
    if n < p.len() as i32 {
        return INSUFFICIENT_BUFFER;
    }
    for (i, q) in p.iter().enumerate() {
        wf(out + 8 * i as u32, q[0]);
        wf(out + 8 * i as u32 + 4, q[1]);
    }
    OK
}
fn GdipGetPathTypes(c: &mut Ctx) -> u32 {
    let (out, n) = (c.a(1), c.i(2));
    let t = with(|m| match m.get(&c.a(0)) {
        Some(Obj::Path { types, .. }) => Some(types.clone()),
        _ => None,
    });
    let Some(t) = t.filter(|_| out != 0) else { return INVALID_PARAMETER };
    if n < t.len() as i32 {
        return INSUFFICIENT_BUFFER;
    }
    mem::slice_mut(out, t.len() as u32).copy_from_slice(&t);
    OK
}
fn GdipGetPathFillMode(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let f = with(|m| match m.get(&c.a(0)) {
        Some(Obj::Path { fill, .. }) => Some(*fill),
        _ => None,
    });
    let Some(f) = f.filter(|_| out != 0) else { return INVALID_PARAMETER };
    w32(out, f);
    OK
}
fn GdipSetPathFillMode(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    on_path(c.a(0), |_, _, f| {
        *f = v;
        OK
    })
}

// ---- brushes in general ----

/// The color a brush stands for where one color is wanted (gdiplus_text's text and pens)
pub(crate) fn main_color(h: u32) -> Option<u32> {
    with(|m| match m.get(&h) {
        Some(Obj::Brush(b)) => Some(match b {
            Brush::Solid(c) => *c,
            Brush::Hatch { fore, .. } => *fore,
            Brush::Texture { .. } => 0xff00_0000,
            Brush::Line { colors, .. } => colors[0],
            Brush::PathGrad { cc, .. } => *cc,
        }),
        _ => None,
    })
}

fn GdipCreateSolidFill(c: &mut Ctx) -> u32 {
    new_obj(Obj::Brush(Brush::Solid(c.a(0))), c.a(1))
}
fn GdipGetSolidFillColor(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_brush(c.a(0), |b| match b {
        Brush::Solid(v) if out != 0 => {
            w32(out, *v);
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
fn GdipSetSolidFillColor(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    on_brush(c.a(0), |b| match b {
        Brush::Solid(x) => {
            *x = v;
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
fn GdipCloneBrush(c: &mut Ctx) -> u32 {
    let b = with(|m| match m.get(&c.a(0)) {
        Some(Obj::Brush(b)) => Some(b.clone()),
        _ => None,
    });
    let Some(b) = b else { return INVALID_PARAMETER };
    new_obj(Obj::Brush(b), c.a(1))
}
fn GdipDeleteBrush(c: &mut Ctx) -> u32 {
    delete_obj(c.a(0), |o| matches!(o, Obj::Brush(_)))
}
fn GdipGetBrushType(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_brush(c.a(0), |b| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, b.kind());
        OK
    })
}

// ---- hatches ----

/// HatchStyleMax
const HATCH_MAX: u32 = 52;

/// Each hatch style's 8x8 tile as Windows draws it, a row per u16 from the top, two bits a
/// pixel from the left (0 the background color .. 3 the foreground; 1 and 2 are the
/// antialiased edges of the diagonals): read off real GDI+'s pixels (winref gdipbrush)
static HATCHES: [[u16; 8]; 53] = include!("gdiplus_hatches.in");

/// how much of the foreground each level has, of 255
const LEVEL: [u32; 4] = [0, 64, 234, 255];

fn hatch_level(style: u32, x: i32, y: i32) -> usize {
    let row = HATCHES[style as usize][(y & 7) as usize];
    (row >> (2 * (7 - (x & 7))) & 3) as usize
}

fn GdipCreateHatchBrush(c: &mut Ctx) -> u32 {
    let (style, out) = (c.a(0), c.a(3));
    if out == 0 || style > HATCH_MAX {
        return INVALID_PARAMETER;
    }
    new_obj(Obj::Brush(Brush::Hatch { style, fore: c.a(1), back: c.a(2) }), out)
}
fn hatch_get(c: &mut Ctx, f: fn(u32, u32, u32) -> u32) -> u32 {
    let out = c.a(1);
    on_brush(c.a(0), |b| match b {
        Brush::Hatch { style, fore, back } if out != 0 => {
            w32(out, f(*style, *fore, *back));
            OK
        }
        _ => INVALID_PARAMETER,
    })
}
fn GdipGetHatchStyle(c: &mut Ctx) -> u32 {
    hatch_get(c, |s, _, _| s)
}
fn GdipGetHatchForegroundColor(c: &mut Ctx) -> u32 {
    hatch_get(c, |_, f, _| f)
}
fn GdipGetHatchBackgroundColor(c: &mut Ctx) -> u32 {
    hatch_get(c, |_, _, b| b)
}

// ---- textures ----

fn new_texture(img: u32, wrap: u32, crop: Option<[f32; 4]>, out: u32) -> u32 {
    if img == 0 || out == 0 {
        return INVALID_PARAMETER;
    }
    let Some((w, h, px)) = crate::gdiplus::pixels(img) else { return INVALID_PARAMETER };
    let (x0, y0, cw, ch) = match crop {
        None => (0, 0, w, h),
        Some(r) => {
            let [x, y, cw, ch] = r.map(|v| v.round() as i64);
            if x < 0 || y < 0 || cw <= 0 || ch <= 0 || x + cw > w as i64 || y + ch > h as i64 {
                return OUT_OF_MEMORY;
            }
            (x as u32, y as u32, cw as u32, ch as u32)
        }
    };
    let mut sub = Vec::with_capacity((cw * ch * 4) as usize);
    for y in y0..y0 + ch {
        let s = ((y * w + x0) * 4) as usize;
        sub.extend_from_slice(&px[s..s + (cw * 4) as usize]);
    }
    new_obj(Obj::Brush(Brush::Texture { w: cw, h: ch, px: sub, wrap, m: IDENTITY }), out)
}
fn GdipCreateTexture(c: &mut Ctx) -> u32 {
    let wrap = c.a(1);
    if wrap > CLAMP {
        return INVALID_PARAMETER;
    }
    new_texture(c.a(0), wrap, None, c.a(2))
}
/// GdipCreateTexture2(image, wrap, x, y, w, h, &texture)
fn GdipCreateTexture2(c: &mut Ctx) -> u32 {
    let r = [2, 3, 4, 5].map(|i| c.f(i));
    new_texture(c.a(0), c.a(1), Some(r), c.a(6))
}
fn GdipCreateTexture2I(c: &mut Ctx) -> u32 {
    let r = [2, 3, 4, 5].map(|i| c.i(i) as f32);
    new_texture(c.a(0), c.a(1), Some(r), c.a(6))
}
/// GdipCreateTextureIA(image, attributes, x, y, w, h, &texture): tiled (attributes ignored)
fn GdipCreateTextureIA(c: &mut Ctx) -> u32 {
    let r = [2, 3, 4, 5].map(|i| c.f(i));
    new_texture(c.a(0), TILE, Some(r), c.a(6))
}
fn GdipCreateTextureIAI(c: &mut Ctx) -> u32 {
    let r = [2, 3, 4, 5].map(|i| c.i(i) as f32);
    new_texture(c.a(0), TILE, Some(r), c.a(6))
}

/// The transform of a texture, linear or path gradient brush
fn brush_m(b: &mut Brush) -> Option<&mut M> {
    match b {
        Brush::Texture { m, .. } | Brush::Line { m, .. } | Brush::PathGrad { m, .. } => Some(m),
        _ => None,
    }
}
fn wrap_of(b: &mut Brush) -> Option<&mut u32> {
    match b {
        Brush::Texture { wrap, .. } | Brush::Line { wrap, .. } | Brush::PathGrad { wrap, .. } => Some(wrap),
        _ => None,
    }
}
/// Get..Transform(brush, matrix) of brushes of `kind`
fn get_transform(c: &mut Ctx, kind: u32) -> u32 {
    let (h, mh) = (c.a(0), c.a(1));
    let mut got = None;
    let s = on_brush(h, |b| {
        if !b_kind(kind, b) {
            return INVALID_PARAMETER;
        }
        got = brush_m(b).map(|m| *m);
        OK
    });
    let Some(m) = got.filter(|_| s == OK) else { return INVALID_PARAMETER };
    on_matrix(mh, |x| {
        *x = m;
        OK
    })
}
fn b_kind(kind: u32, b: &Brush) -> bool {
    b.kind() == kind
}
fn set_transform(c: &mut Ctx, kind: u32) -> u32 {
    let Some(m) = matrix(c.a(1)) else { return INVALID_PARAMETER };
    on_brush(c.a(0), |b| {
        if !b_kind(kind, b) {
            return INVALID_PARAMETER;
        }
        *brush_m(b).unwrap() = m;
        OK
    })
}
fn reset_transform(c: &mut Ctx, kind: u32) -> u32 {
    on_brush(c.a(0), |b| {
        if !b_kind(kind, b) {
            return INVALID_PARAMETER;
        }
        *brush_m(b).unwrap() = IDENTITY;
        OK
    })
}
/// Multiply..Transform(brush, matrix, order): a NULL matrix does nothing
fn multiply_transform(c: &mut Ctx, kind: u32) -> u32 {
    let (h, mh, order) = (c.a(0), c.a(1), c.a(2));
    let op = if mh == 0 { None } else {
        let Some(m) = matrix(mh) else { return INVALID_PARAMETER };
        Some(m)
    };
    on_brush(h, |b| {
        if !b_kind(kind, b) {
            return INVALID_PARAMETER;
        }
        match op {
            Some(op) => combine(brush_m(b).unwrap(), &op, order),
            None => OK,
        }
    })
}
fn op_transform(c: &mut Ctx, kind: u32, op: M, order: u32) -> u32 {
    on_brush(c.a(0), |b| {
        if !b_kind(kind, b) {
            return INVALID_PARAMETER;
        }
        combine(brush_m(b).unwrap(), &op, order)
    })
}
fn get_wrap(c: &mut Ctx, kind: u32) -> u32 {
    let out = c.a(1);
    on_brush(c.a(0), |b| {
        if out == 0 || !b_kind(kind, b) {
            return INVALID_PARAMETER;
        }
        w32(out, *wrap_of(b).unwrap());
        OK
    })
}
fn set_wrap(c: &mut Ctx, kind: u32) -> u32 {
    let v = c.a(1);
    on_brush(c.a(0), |b| {
        if !b_kind(kind, b) || v > CLAMP || kind == 4 && v == CLAMP {
            return INVALID_PARAMETER;
        }
        *wrap_of(b).unwrap() = v;
        OK
    })
}

fn GdipGetTextureTransform(c: &mut Ctx) -> u32 {
    get_transform(c, 2)
}
fn GdipSetTextureTransform(c: &mut Ctx) -> u32 {
    set_transform(c, 2)
}
fn GdipResetTextureTransform(c: &mut Ctx) -> u32 {
    reset_transform(c, 2)
}
fn GdipMultiplyTextureTransform(c: &mut Ctx) -> u32 {
    multiply_transform(c, 2)
}
fn GdipTranslateTextureTransform(c: &mut Ctx) -> u32 {
    let (op, o) = ([1.0, 0.0, 0.0, 1.0, c.f(1), c.f(2)], c.a(3));
    op_transform(c, 2, op, o)
}
fn GdipScaleTextureTransform(c: &mut Ctx) -> u32 {
    let (op, o) = ([c.f(1), 0.0, 0.0, c.f(2), 0.0, 0.0], c.a(3));
    op_transform(c, 2, op, o)
}
fn GdipRotateTextureTransform(c: &mut Ctx) -> u32 {
    let (op, o) = (rotation(c.f(1)), c.a(2));
    op_transform(c, 2, op, o)
}
fn GdipGetTextureWrapMode(c: &mut Ctx) -> u32 {
    get_wrap(c, 2)
}
fn GdipSetTextureWrapMode(c: &mut Ctx) -> u32 {
    set_wrap(c, 2)
}

// ---- linear gradients ----

/// The transform of a gradient across `rect` (x, y, w, h) turned to `deg` degrees: the
/// rectangle's x axis goes along the angle, scaled so that the gradient spans the
/// rectangle's projection on that direction, about the rectangle's center (winref
/// gdipbrush; Wine's tests: (1,1)-(100,100) gives 1 1 -1 1 50.5 -50.5)
pub(crate) fn line_transform(rect: [f32; 4], deg: f64) -> M {
    let (w, h) = (rect[2].abs() as f64, rect[3].abs() as f64);
    let r = deg.to_radians();
    let (s, c) = (r.sin(), r.cos());
    // (round off what is 0 at right angles)
    let snap = |v: f64| if v.abs() < 1e-12 { 0.0 } else { v };
    let (s, c) = (snap(s), snap(c));
    let p = w * c.abs() + h * s.abs();
    let q = w * s.abs() + h * c.abs();
    let (m11, m12) = (p / w * c, p / w * s);
    let (m21, m22) = (-q / h * s, q / h * c);
    let (cx, cy) = (rect[0] as f64 + rect[2] as f64 / 2.0, rect[1] as f64 + rect[3] as f64 / 2.0);
    [m11, m12, m21, m22, cx - (cx * m11 + cy * m21), cy - (cx * m12 + cy * m22)].map(|v| v as f32)
}

/// GdipCreateLineBrush(point1, point2, color1, color2, wrap, &brush) / ..I: the rectangle the
/// points span, a line along one axis making it a square about the line
fn line_brush(c: &mut Ctx, int: bool) -> u32 {
    let (p1, p2, c1, c2, wrap, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if p1 == 0 || p2 == 0 || out == 0 {
        return INVALID_PARAMETER;
    }
    if wrap == CLAMP {
        return INVALID_PARAMETER;
    }
    let p = read_pts(p1, 1, int)[0];
    let q = read_pts(p2, 1, int)[0];
    let Some((rect, deg)) = line_geometry(p, q) else { return OUT_OF_MEMORY };
    let m = line_transform(rect, deg);
    new_obj(Obj::Brush(Brush::Line { rect, colors: [c1, c2], wrap, m, blend: Blend::new(), gamma: false }), out)
}
/// The rectangle and angle of a gradient from p to q (None: the same point)
pub(crate) fn line_geometry(p: [f32; 2], q: [f32; 2]) -> Option<([f32; 4], f64)> {
    let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
    if dx == 0.0 && dy == 0.0 {
        return None;
    }
    let (x, y) = (p[0].min(q[0]), p[1].min(q[1]));
    let (mut w, mut h) = (dx.abs(), dy.abs());
    let (mut rx, mut ry) = (x, y);
    if w == 0.0 {
        w = h;
        rx = p[0] - w / 2.0;
    }
    if h == 0.0 {
        h = w;
        ry = p[1] - h / 2.0;
    }
    Some(([rx, ry, w, h], (dy as f64).atan2(dx as f64).to_degrees()))
}
fn GdipCreateLineBrush(c: &mut Ctx) -> u32 {
    line_brush(c, false)
}
fn GdipCreateLineBrushI(c: &mut Ctx) -> u32 {
    line_brush(c, true)
}
/// The transform of a gradient at `deg` that scales with the rectangle: the one of a square
/// stretched to the rectangle (the angle is the square's; winref gdipbrush: 30 degrees over
/// 100 x 10 is 1.183 0.068 -6.830 1.183)
pub(crate) fn scaled_transform(rect: [f32; 4], deg: f64) -> M {
    let sq = line_transform([0.0, 0.0, 1.0, 1.0], deg);
    let (w, h) = (rect[2].abs(), rect[3].abs());
    let (m11, m12, m21, m22) = (sq[0], sq[1] * h / w, sq[2] * w / h, sq[3]);
    let (cx, cy) = (rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
    [m11, m12, m21, m22, cx - (cx * m11 + cy * m21), cy - (cx * m12 + cy * m22)]
}
fn rect_brush(rect: [f32; 4], c1: u32, c2: u32, deg: f64, scalable: bool, wrap: u32, out: u32) -> u32 {
    if rect[2] == 0.0 || rect[3] == 0.0 {
        return OUT_OF_MEMORY;
    }
    let m = if deg.rem_euclid(360.0) == 0.0 {
        IDENTITY
    } else if scalable {
        scaled_transform(rect, deg)
    } else {
        line_transform(rect, deg)
    };
    new_obj(Obj::Brush(Brush::Line { rect, colors: [c1, c2], wrap, m, blend: Blend::new(), gamma: false }), out)
}
/// GdipCreateLineBrushFromRect(rect, color1, color2, mode, wrap, &brush) / ..I:
/// LinearGradientModeHorizontal 0, Vertical 1, ForwardDiagonal 2, BackwardDiagonal 3: the
/// angles 0, 90, 45 and 135 scaling with the rectangle
fn from_rect(c: &mut Ctx, int: bool) -> u32 {
    let (rp, c1, c2, mode, wrap, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if rp == 0 || out == 0 || wrap == CLAMP {
        return INVALID_PARAMETER;
    }
    // (an unknown mode: winref gdipbrush)
    if mode > 3 {
        return OUT_OF_MEMORY;
    }
    rect_brush(read_rect(rp, int), c1, c2, [0.0, 90.0, 45.0, 135.0][mode as usize], true, wrap, out)
}
fn GdipCreateLineBrushFromRect(c: &mut Ctx) -> u32 {
    from_rect(c, false)
}
fn GdipCreateLineBrushFromRectI(c: &mut Ctx) -> u32 {
    from_rect(c, true)
}
/// GdipCreateLineBrushFromRectWithAngle(rect, color1, color2, angle, isAngleScalable, wrap,
/// &brush) / ..I: a NULL rectangle or brush or WrapModeClamp is InvalidParameter, before an
/// empty rectangle's OutOfMemory (Wine's tests)
fn with_angle(c: &mut Ctx, int: bool) -> u32 {
    let (rp, c1, c2, deg, scalable, wrap, out) = (c.a(0), c.a(1), c.a(2), c.f(3), c.a(4) != 0, c.a(5), c.a(6));
    if rp == 0 || out == 0 || wrap == CLAMP {
        return INVALID_PARAMETER;
    }
    rect_brush(read_rect(rp, int), c1, c2, deg as f64, scalable, wrap, out)
}
fn GdipCreateLineBrushFromRectWithAngle(c: &mut Ctx) -> u32 {
    with_angle(c, false)
}
fn GdipCreateLineBrushFromRectWithAngleI(c: &mut Ctx) -> u32 {
    with_angle(c, true)
}

fn on_line(h: u32, f: impl FnOnce(&mut [f32; 4], &mut [u32; 2], &mut Blend, &mut bool) -> u32) -> u32 {
    on_brush(h, |b| match b {
        Brush::Line { rect, colors, blend, gamma, .. } => f(rect, colors, blend, gamma),
        _ => INVALID_PARAMETER,
    })
}
fn on_blend(h: u32, f: impl FnOnce(&mut Blend, bool) -> u32) -> u32 {
    on_brush(h, |b| match b {
        Brush::Line { blend, .. } => f(blend, false),
        Brush::PathGrad { blend, .. } => f(blend, true),
        _ => INVALID_PARAMETER,
    })
}
fn GdipGetLineRect(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_line(c.a(0), |r, _, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        for (i, v) in r.iter().enumerate() {
            wf(out + 4 * i as u32, *v);
        }
        OK
    })
}
fn GdipGetLineRectI(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_line(c.a(0), |r, _, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        for (i, v) in r.iter().enumerate() {
            w32(out + 4 * i as u32, v.round() as i32 as u32);
        }
        OK
    })
}
fn GdipGetLineColors(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_line(c.a(0), |_, k, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, k[0]);
        w32(out + 4, k[1]);
        OK
    })
}
fn GdipSetLineColors(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(1), c.a(2));
    on_line(c.a(0), |_, k, _, _| {
        *k = [a, b];
        OK
    })
}
fn GdipGetLineGammaCorrection(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_line(c.a(0), |_, _, _, g| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, *g as u32);
        OK
    })
}
fn GdipSetLineGammaCorrection(c: &mut Ctx) -> u32 {
    let v = c.a(1) != 0;
    on_line(c.a(0), |_, _, _, g| {
        *g = v;
        OK
    })
}
fn GdipGetLineWrapMode(c: &mut Ctx) -> u32 {
    get_wrap(c, 4)
}
fn GdipSetLineWrapMode(c: &mut Ctx) -> u32 {
    set_wrap(c, 4)
}
fn GdipGetLineTransform(c: &mut Ctx) -> u32 {
    get_transform(c, 4)
}
fn GdipSetLineTransform(c: &mut Ctx) -> u32 {
    set_transform(c, 4)
}
fn GdipResetLineTransform(c: &mut Ctx) -> u32 {
    reset_transform(c, 4)
}
fn GdipMultiplyLineTransform(c: &mut Ctx) -> u32 {
    multiply_transform(c, 4)
}
fn GdipTranslateLineTransform(c: &mut Ctx) -> u32 {
    let (op, o) = ([1.0, 0.0, 0.0, 1.0, c.f(1), c.f(2)], c.a(3));
    op_transform(c, 4, op, o)
}
fn GdipScaleLineTransform(c: &mut Ctx) -> u32 {
    let (op, o) = ([c.f(1), 0.0, 0.0, c.f(2), 0.0, 0.0], c.a(3));
    op_transform(c, 4, op, o)
}
fn GdipRotateLineTransform(c: &mut Ctx) -> u32 {
    let (op, o) = (rotation(c.f(1)), c.a(2));
    op_transform(c, 4, op, o)
}

// ---- blends (linear and path gradients alike) ----

/// Get..BlendCount(brush, &count): 1 for the default blend
fn blend_count(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_blend(c.a(0), |b, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, b.factors.len().max(1) as u32);
        OK
    })
}
/// Get..Blend(brush, factors, positions, count): the default writes a factor of 1 and leaves
/// the position as it was (Wine's tests)
fn get_blend(c: &mut Ctx) -> u32 {
    let (fp, pp, n) = (c.a(1), c.a(2), c.i(3));
    on_blend(c.a(0), |b, _| {
        if fp == 0 || pp == 0 || n <= 0 {
            return INVALID_PARAMETER;
        }
        if b.factors.is_empty() {
            wf(fp, 1.0);
            return OK;
        }
        if n < b.factors.len() as i32 {
            return INSUFFICIENT_BUFFER;
        }
        for (i, &(f, p)) in b.factors.iter().enumerate() {
            wf(fp + 4 * i as u32, f);
            wf(pp + 4 * i as u32, p);
        }
        OK
    })
}
/// positions from 0 to 1 (when there are more than one)
fn ends_ok(pos: &[f32]) -> bool {
    pos.len() < 2 || pos[0] == 0.0 && pos[pos.len() - 1] == 1.0
}
/// Set..Blend(brush, factors, positions, count): it replaces a preset blend
fn set_blend(c: &mut Ctx) -> u32 {
    let (fp, pp, n) = (c.a(1), c.a(2), c.i(3));
    if fp == 0 || pp == 0 || n <= 0 {
        return INVALID_PARAMETER;
    }
    let f: Vec<(f32, f32)> = (0..n as u32).map(|i| (rf(fp + 4 * i), rf(pp + 4 * i))).collect();
    if !ends_ok(&f.iter().map(|p| p.1).collect::<Vec<_>>()) {
        return INVALID_PARAMETER;
    }
    on_blend(c.a(0), |b, _| {
        b.factors = f;
        b.preset.clear();
        OK
    })
}
fn preset_count(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_blend(c.a(0), |b, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, b.preset.len() as u32);
        OK
    })
}
/// Get..PresetBlend(brush, colors, positions, count): fewer than 2 is InvalidParameter (a
/// path gradient's negative count OutOfMemory), none set GenericError (Wine's tests)
fn get_preset(c: &mut Ctx) -> u32 {
    let (cp, pp, n) = (c.a(1), c.a(2), c.i(3));
    on_blend(c.a(0), |b, path| {
        if cp == 0 || pp == 0 {
            return INVALID_PARAMETER;
        }
        if path && n < 0 {
            return OUT_OF_MEMORY;
        }
        if n < 2 {
            return INVALID_PARAMETER;
        }
        if b.preset.is_empty() {
            return GENERIC_ERROR;
        }
        if n < b.preset.len() as i32 {
            return INSUFFICIENT_BUFFER;
        }
        for (i, &(col, p)) in b.preset.iter().enumerate() {
            w32(cp + 4 * i as u32, col);
            wf(pp + 4 * i as u32, p);
        }
        OK
    })
}
fn set_preset(c: &mut Ctx) -> u32 {
    let (cp, pp, n) = (c.a(1), c.a(2), c.i(3));
    if cp == 0 || pp == 0 || n < 2 {
        return INVALID_PARAMETER;
    }
    let p: Vec<(u32, f32)> = (0..n as u32).map(|i| (r32(cp + 4 * i), rf(pp + 4 * i))).collect();
    if !ends_ok(&p.iter().map(|p| p.1).collect::<Vec<_>>()) {
        return INVALID_PARAMETER;
    }
    on_blend(c.a(0), |b, _| {
        b.preset = p;
        b.factors.clear();
        OK
    })
}
/// The blend of Set..LinearBlend(focus, scale): up to the scale at the focus and back to 0
pub(crate) fn linear_blend(focus: f32, scale: f32) -> Vec<(f32, f32)> {
    if focus == 0.0 {
        vec![(scale, 0.0), (0.0, 1.0)]
    } else if focus == 1.0 {
        vec![(0.0, 0.0), (scale, 1.0)]
    } else {
        vec![(0.0, 0.0), (scale, focus), (0.0, 1.0)]
    }
}
/// The blend of Set..SigmaBlend(focus, scale): a bell, the normal distribution's integral
/// over -2 to 2 sigmas rising to the focus and falling after it, at 511 positions
/// (winref gdipbrush: focus 0.5 has 0.139 at 0.124 and 0.997 at 0.494)
pub(crate) fn sigma_blend(focus: f32, scale: f32) -> Vec<(f32, f32)> {
    const N: usize = 511;
    let s = |u: f64| (erf(std::f64::consts::SQRT_2 * (2.0 * u - 1.0)) + erf(std::f64::consts::SQRT_2)) / (2.0 * erf(std::f64::consts::SQRT_2));
    (0..N)
        .map(|i| {
            let p = i as f64 / (N - 1) as f64;
            let f = focus as f64;
            let v = if f > 0.0 && p <= f {
                s(p / f)
            } else if f < 1.0 {
                1.0 - s((p - f) / (1.0 - f))
            } else {
                1.0
            };
            ((v as f32 * scale).clamp(0.0, 1.0), p as f32)
        })
        .collect()
}
/// erf (Abramowitz and Stegun 7.1.26, |error| < 1.5e-7)
fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.327_591_1 * x.abs());
    let y = 1.0 - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t - 0.284_496_736) * t + 0.254_829_592) * t * (-x * x).exp();
    if x < 0.0 { -y } else { y }
}
fn shaped_blend(c: &mut Ctx, f: fn(f32, f32) -> Vec<(f32, f32)>) -> u32 {
    let (focus, scale) = (c.f(1), c.f(2));
    if !(0.0..=1.0).contains(&focus) || !(0.0..=1.0).contains(&scale) {
        return INVALID_PARAMETER;
    }
    on_blend(c.a(0), |b, _| {
        b.factors = f(focus, scale);
        b.preset.clear();
        OK
    })
}
fn set_linear_blend(c: &mut Ctx) -> u32 {
    shaped_blend(c, linear_blend)
}
fn set_sigma_blend(c: &mut Ctx) -> u32 {
    shaped_blend(c, sigma_blend)
}

// ---- path gradients ----

fn path_grad(pts: Vec<[f32; 2]>, wrap: u32, out: u32) -> u32 {
    if flat(&pts) {
        return OUT_OF_MEMORY;
    }
    let n = pts.len() as f32;
    let center = [pts.iter().map(|p| p[0]).sum::<f32>() / n, pts.iter().map(|p| p[1]).sum::<f32>() / n];
    let surround = vec![0xffff_ffff; pts.len()];
    let b = Brush::PathGrad { pts, center, cc: 0xff00_0000, surround, wrap, m: IDENTITY, blend: Blend::new(), focus: [0.0, 0.0], gamma: false };
    new_obj(Obj::Brush(b), out)
}
/// GdipCreatePathGradient(points, count, wrap, &brush) / ..I
fn create_path_grad(c: &mut Ctx, int: bool) -> u32 {
    let (pp, n, wrap, out) = (c.a(0), c.i(1), c.a(2), c.a(3));
    if pp == 0 || out == 0 {
        return INVALID_PARAMETER;
    }
    if n < 2 {
        return OUT_OF_MEMORY;
    }
    path_grad(read_pts(pp, n as u32, int), wrap, out)
}
/// (points in a line, with no area to their bounds, are OutOfMemory: winref gdipbrush)
fn flat(pts: &[[f32; 2]]) -> bool {
    let b = bounds(pts);
    b[2] == 0.0 || b[3] == 0.0
}
fn GdipCreatePathGradient(c: &mut Ctx) -> u32 {
    create_path_grad(c, false)
}
fn GdipCreatePathGradientI(c: &mut Ctx) -> u32 {
    create_path_grad(c, true)
}
fn GdipCreatePathGradientFromPath(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let Some(p) = path_points(c.a(0)).filter(|_| out != 0) else { return INVALID_PARAMETER };
    if p.len() < 2 {
        return OUT_OF_MEMORY;
    }
    path_grad(p, CLAMP, out)
}
fn on_pg(h: u32, f: impl FnOnce(&mut Vec<[f32; 2]>, &mut [f32; 2], &mut u32, &mut Vec<u32>, &mut [f32; 2], &mut bool) -> u32) -> u32 {
    on_brush(h, |b| match b {
        Brush::PathGrad { pts, center, cc, surround, focus, gamma, .. } => f(pts, center, cc, surround, focus, gamma),
        _ => INVALID_PARAMETER,
    })
}
fn GdipGetPathGradientCenterPoint(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_pg(c.a(0), |_, ctr, _, _, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        wf(out, ctr[0]);
        wf(out + 4, ctr[1]);
        OK
    })
}
fn GdipGetPathGradientCenterPointI(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_pg(c.a(0), |_, ctr, _, _, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, ctr[0].round() as i32 as u32);
        w32(out + 4, ctr[1].round() as i32 as u32);
        OK
    })
}
fn set_center(c: &mut Ctx, int: bool) -> u32 {
    let p = c.a(1);
    if p == 0 {
        return INVALID_PARAMETER;
    }
    let q = read_pts(p, 1, int)[0];
    on_pg(c.a(0), |_, ctr, _, _, _, _| {
        *ctr = q;
        OK
    })
}
fn GdipSetPathGradientCenterPoint(c: &mut Ctx) -> u32 {
    set_center(c, false)
}
fn GdipSetPathGradientCenterPointI(c: &mut Ctx) -> u32 {
    set_center(c, true)
}
fn GdipGetPathGradientCenterColor(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_pg(c.a(0), |_, _, cc, _, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, *cc);
        OK
    })
}
fn GdipSetPathGradientCenterColor(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    on_pg(c.a(0), |_, _, cc, _, _, _| {
        *cc = v;
        OK
    })
}
fn bounds(pts: &[[f32; 2]]) -> [f32; 4] {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in pts {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    [x0, y0, x1 - x0, y1 - y0]
}
fn get_pg_rect(c: &mut Ctx, int: bool) -> u32 {
    let out = c.a(1);
    on_pg(c.a(0), |pts, _, _, _, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        for (i, v) in bounds(pts).iter().enumerate() {
            if int {
                w32(out + 4 * i as u32, v.round() as i32 as u32);
            } else {
                wf(out + 4 * i as u32, *v);
            }
        }
        OK
    })
}
fn GdipGetPathGradientRect(c: &mut Ctx) -> u32 {
    get_pg_rect(c, false)
}
fn GdipGetPathGradientRectI(c: &mut Ctx) -> u32 {
    get_pg_rect(c, true)
}
fn GdipGetPathGradientPointCount(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_pg(c.a(0), |pts, _, _, _, _, _| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, pts.len() as u32);
        OK
    })
}
/// GdipGetPathGradientSurroundColorCount: the number of points, whatever was set
fn GdipGetPathGradientSurroundColorCount(c: &mut Ctx) -> u32 {
    GdipGetPathGradientPointCount(c)
}
/// GdipGetPathGradientSurroundColorsWithCount(brush, colors, &count): room for a color per
/// point, all of them written; the count says 1 when they are all the same (Wine's tests)
fn GdipGetPathGradientSurroundColorsWithCount(c: &mut Ctx) -> u32 {
    let (out, np) = (c.a(1), c.a(2));
    on_pg(c.a(0), |_, _, _, s, _, _| {
        if out == 0 || np == 0 || (r32(np) as i32) < s.len() as i32 {
            return INVALID_PARAMETER;
        }
        for (i, v) in s.iter().enumerate() {
            w32(out + 4 * i as u32, *v);
        }
        w32(np, if s.iter().all(|&v| v == s[0]) { 1 } else { s.len() as u32 });
        OK
    })
}
/// GdipSetPathGradientSurroundColorsWithCount(brush, colors, &count): 1 up to a color per
/// point; the last one given goes on for the points after it
fn GdipSetPathGradientSurroundColorsWithCount(c: &mut Ctx) -> u32 {
    let (cp, np) = (c.a(1), c.a(2));
    if cp == 0 || np == 0 {
        return INVALID_PARAMETER;
    }
    let n = r32(np) as i32;
    on_pg(c.a(0), |_, _, _, s, _, _| {
        if n <= 0 || n > s.len() as i32 {
            return INVALID_PARAMETER;
        }
        for i in 0..s.len() {
            s[i] = r32(cp + 4 * (i as u32).min(n as u32 - 1));
        }
        OK
    })
}
fn GdipGetPathGradientPath(_: &mut Ctx) -> u32 {
    NOT_IMPLEMENTED
}
fn GdipGetPathGradientFocusScales(c: &mut Ctx) -> u32 {
    let (xp, yp) = (c.a(1), c.a(2));
    on_pg(c.a(0), |_, _, _, _, f, _| {
        if xp == 0 || yp == 0 {
            return INVALID_PARAMETER;
        }
        wf(xp, f[0]);
        wf(yp, f[1]);
        OK
    })
}
fn GdipSetPathGradientFocusScales(c: &mut Ctx) -> u32 {
    let v = [c.f(1), c.f(2)];
    on_pg(c.a(0), |_, _, _, _, f, _| {
        *f = v;
        OK
    })
}
fn GdipGetPathGradientGammaCorrection(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    on_pg(c.a(0), |_, _, _, _, _, g| {
        if out == 0 {
            return INVALID_PARAMETER;
        }
        w32(out, *g as u32);
        OK
    })
}
fn GdipSetPathGradientGammaCorrection(c: &mut Ctx) -> u32 {
    let v = c.a(1) != 0;
    on_pg(c.a(0), |_, _, _, _, _, g| {
        *g = v;
        OK
    })
}
fn GdipGetPathGradientWrapMode(c: &mut Ctx) -> u32 {
    get_wrap(c, 3)
}
fn GdipSetPathGradientWrapMode(c: &mut Ctx) -> u32 {
    set_wrap(c, 3)
}
fn GdipGetPathGradientTransform(c: &mut Ctx) -> u32 {
    get_transform(c, 3)
}
fn GdipSetPathGradientTransform(c: &mut Ctx) -> u32 {
    set_transform(c, 3)
}
fn GdipResetPathGradientTransform(c: &mut Ctx) -> u32 {
    reset_transform(c, 3)
}
fn GdipMultiplyPathGradientTransform(c: &mut Ctx) -> u32 {
    multiply_transform(c, 3)
}
fn GdipTranslatePathGradientTransform(c: &mut Ctx) -> u32 {
    let (op, o) = ([1.0, 0.0, 0.0, 1.0, c.f(1), c.f(2)], c.a(3));
    op_transform(c, 3, op, o)
}
fn GdipScalePathGradientTransform(c: &mut Ctx) -> u32 {
    let (op, o) = ([c.f(1), 0.0, 0.0, c.f(2), 0.0, 0.0], c.a(3));
    op_transform(c, 3, op, o)
}
fn GdipRotatePathGradientTransform(c: &mut Ctx) -> u32 {
    let (op, o) = (rotation(c.f(1)), c.a(2));
    op_transform(c, 3, op, o)
}

// ---- painting ----

/// position in a tile of `n`, mirrored on odd tiles when `flip`
fn wrap_coord(v: i64, n: i64, flip: bool) -> i64 {
    let k = v.rem_euclid(n);
    if flip && v.div_euclid(n) & 1 != 0 { n - 1 - k } else { k }
}
fn wrap_t(t: f32, wrap: u32) -> f32 {
    match wrap {
        FLIP_X | FLIP_XY => {
            let k = t.rem_euclid(2.0);
            if k > 1.0 { 2.0 - k } else { k }
        }
        _ => t.rem_euclid(1.0),
    }
}

/// What a brush is to paint: a snapshot to sample without the lock, the inverse of its
/// transform, a linear gradient's colors
pub(crate) struct Painter(Brush, Option<M>, Vec<u32>);

pub(crate) fn painter(h: u32) -> Option<Painter> {
    let b = with(|m| match m.get(&h) {
        Some(Obj::Brush(b)) => Some(b.clone()),
        _ => None,
    })?;
    let inv = match &b {
        Brush::Texture { m, .. } | Brush::Line { m, .. } | Brush::PathGrad { m, .. } => invert(m),
        _ => Some(IDENTITY),
    };
    let lut = match &b {
        Brush::Line { rect, colors, blend, .. } => line_lut(rect, colors, blend),
        _ => Vec::new(),
    };
    Some(Painter(b, inv, lut))
}

impl Painter {
    /// The straight ARGB color at device pixel (x, y) (sampled at the pixel's integer
    /// coordinates, GDI+'s default pixel offset), with the graphics' rendering origin
    /// (hatches start there); None: nothing (a clamped texture or path gradient outside)
    pub(crate) fn at(&self, x: i32, y: i32, origin: (i32, i32)) -> Option<u32> {
        let Painter(b, inv, lut) = self;
        let inv = (*inv)?;
        match b {
            Brush::Solid(c) => Some(*c),
            Brush::Hatch { style, fore, back } => {
                let l = LEVEL[hatch_level(*style, x - origin.0, y - origin.1)];
                Some(mix_premul(*back, *fore, l))
            }
            Brush::Texture { w, h, px, wrap, .. } => {
                // bilinear, texel centers at whole coordinates (winref gdipbrush: scaled by 2,
                // the pixel between texels 0 and 0x60 is 0x30)
                let (u, v) = apply(&inv, x as f32, y as f32);
                let split = |a: f32| {
                    let k = a.floor();
                    let f = a - k;
                    if f < 1e-3 { (k as i64, 0.0) } else if f > 1.0 - 1e-3 { (k as i64 + 1, 0.0) } else { (k as i64, f) }
                };
                let ((u0, fu), (v0, fv)) = (split(u), split(v));
                let (w, h) = (*w as i64, *h as i64);
                let texel = |u: i64, v: i64| -> u32 {
                    if *wrap == CLAMP && (u < 0 || v < 0 || u >= w || v >= h) {
                        return 0;
                    }
                    let u = wrap_coord(u, w, matches!(*wrap, FLIP_X | FLIP_XY));
                    let v = wrap_coord(v, h, matches!(*wrap, FLIP_Y | FLIP_XY));
                    let i = ((v * w + u) * 4) as usize;
                    premul_argb(u32::from_le_bytes([px[i], px[i + 1], px[i + 2], px[i + 3]]))
                };
                let c = if fu == 0.0 && fv == 0.0 {
                    texel(u0, v0)
                } else {
                    let top = lerp(texel(u0, v0), texel(u0 + 1, v0), fu);
                    let bottom = lerp(texel(u0, v0 + 1), texel(u0 + 1, v0 + 1), fu);
                    lerp(top, bottom, fv)
                };
                if *wrap == CLAMP && c >> 24 == 0 {
                    return None;
                }
                Some(straight_argb(c))
            }
            Brush::Line { rect, wrap, .. } => {
                let (u, _) = apply(&inv, x as f32, y as f32);
                let t = wrap_t((u - rect[0]) / rect[2], *wrap);
                Some(straight_argb(lut_at(lut, t)))
            }
            Brush::PathGrad { pts, center, cc, surround, blend, .. } => {
                let (u, v) = apply(&inv, x as f32, y as f32);
                let (t, k, s) = path_t(pts, *center, u, v)?;
                let edge = lerp(surround[k], surround[(k + 1) % surround.len()], s);
                // (position 0 is the boundary, 1 the center)
                Some(blend.color(edge, *cc, 1.0 - t))
            }
        }
    }
}

/// Where (u, v) is between the center (0) and the boundary of the polygon (1) along the ray
/// from the center, the edge that ray leaves by and how far along it; None: outside
fn path_t(pts: &[[f32; 2]], c: [f32; 2], u: f32, v: f32) -> Option<(f32, usize, f32)> {
    let (dx, dy) = (u - c[0], v - c[1]);
    if dx == 0.0 && dy == 0.0 {
        return Some((0.0, 0, 0.0));
    }
    let mut best: Option<(f32, usize, f32)> = None;
    for k in 0..pts.len() {
        let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
        let (ex, ey) = (b[0] - a[0], b[1] - a[1]);
        let d = dx * ey - dy * ex;
        if d.abs() < 1e-9 {
            continue;
        }
        // c + r (dx, dy) = a + s (ex, ey)
        let (ax, ay) = (a[0] - c[0], a[1] - c[1]);
        let r = (ax * ey - ay * ex) / d;
        let s = (ax * dy - ay * dx) / d;
        if r > 0.0 && (-1e-6..=1.0 + 1e-6).contains(&s) && best.is_none_or(|b| r < b.0) {
            best = Some((r, k, s.clamp(0.0, 1.0)));
        }
    }
    let (r, k, s) = best?;
    // (u, v) is at 1 / r of the way to the boundary
    let t = 1.0 / r;
    (t <= 1.0 + 1e-6).then_some((t.min(1.0), k, s))
}

// ---------------------------------------------------------------------------------------------
// custom line caps

/// LineCapFlat, Square, Round, Triangle: what a base cap or a cap's stroke caps may be (the
/// anchors are not)
fn plain_cap(v: u32) -> bool {
    v <= 3
}
fn on_cap(h: u32, f: impl FnOnce(&mut Cap) -> u32) -> u32 {
    with(|m| match m.get_mut(&h) {
        Some(Obj::Cap(c)) => f(c),
        _ => INVALID_PARAMETER,
    })
}
/// a getter: NULL cap or output is InvalidParameter
fn cap_get(h: u32, out: u32, f: impl FnOnce(&Cap) -> u32) -> u32 {
    if out == 0 {
        return INVALID_PARAMETER;
    }
    on_cap(h, |c| {
        w32(out, f(c));
        OK
    })
}
/// GdipCreateCustomLineCap(fillPath, strokePath, baseCap, baseInset, &cap)
fn GdipCreateCustomLineCap(c: &mut Ctx) -> u32 {
    let (fill, stroke, base, inset, out) = (c.a(0), c.a(1), c.a(2), c.f(3), c.a(4));
    let h = if stroke != 0 { stroke } else { fill };
    if h == 0 || out == 0 {
        return INVALID_PARAMETER;
    }
    if !with(|m| matches!(m.get(&h), Some(Obj::Path { .. }))) {
        return INVALID_PARAMETER;
    }
    new_obj(Obj::Cap(Cap { base, inset, join: 0, scale: 1.0, caps: (0, 0), arrow: None }), out)
}
/// GdipCreateAdjustableArrowCap(height, width, isFilled, &cap): a triangle base cap, inset
/// height / width
fn GdipCreateAdjustableArrowCap(c: &mut Ctx) -> u32 {
    let (hgt, wid, filled, out) = (c.f(0), c.f(1), c.a(2), c.a(3));
    let cap = Cap { base: 3, inset: hgt / wid, join: 0, scale: 1.0, caps: (0, 0), arrow: Some((hgt, wid, 0.0, filled != 0)) };
    new_obj(Obj::Cap(cap), out)
}
fn GdipDeleteCustomLineCap(c: &mut Ctx) -> u32 {
    delete_obj(c.a(0), |o| matches!(o, Obj::Cap(_)))
}
fn GdipCloneCustomLineCap(c: &mut Ctx) -> u32 {
    let Some(cap) = with(|m| match m.get(&c.a(0)) {
        Some(Obj::Cap(x)) => Some(x.clone()),
        _ => None,
    }) else {
        return INVALID_PARAMETER;
    };
    new_obj(Obj::Cap(cap), c.a(1))
}
/// CustomLineCapTypeDefault 0, CustomLineCapTypeAdjustableArrow 1
fn GdipGetCustomLineCapType(c: &mut Ctx) -> u32 {
    cap_get(c.a(0), c.a(1), |x| x.arrow.is_some() as u32)
}
fn GdipGetCustomLineCapBaseCap(c: &mut Ctx) -> u32 {
    cap_get(c.a(0), c.a(1), |x| x.base)
}
fn GdipSetCustomLineCapBaseCap(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    on_cap(c.a(0), |x| {
        if !plain_cap(v) {
            return INVALID_PARAMETER;
        }
        x.base = v;
        OK
    })
}
fn GdipGetCustomLineCapBaseInset(c: &mut Ctx) -> u32 {
    cap_get(c.a(0), c.a(1), |x| x.inset.to_bits())
}
fn GdipSetCustomLineCapBaseInset(c: &mut Ctx) -> u32 {
    let v = c.f(1);
    on_cap(c.a(0), |x| {
        x.inset = v;
        OK
    })
}
fn GdipGetCustomLineCapStrokeJoin(c: &mut Ctx) -> u32 {
    cap_get(c.a(0), c.a(1), |x| x.join)
}
fn GdipSetCustomLineCapStrokeJoin(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    on_cap(c.a(0), |x| {
        x.join = v;
        OK
    })
}
fn GdipGetCustomLineCapWidthScale(c: &mut Ctx) -> u32 {
    cap_get(c.a(0), c.a(1), |x| x.scale.to_bits())
}
fn GdipSetCustomLineCapWidthScale(c: &mut Ctx) -> u32 {
    let v = c.f(1);
    on_cap(c.a(0), |x| {
        x.scale = v;
        OK
    })
}
fn GdipGetCustomLineCapStrokeCaps(c: &mut Ctx) -> u32 {
    let (s, e) = (c.a(1), c.a(2));
    if s == 0 || e == 0 {
        return INVALID_PARAMETER;
    }
    on_cap(c.a(0), |x| {
        w32(s, x.caps.0);
        w32(e, x.caps.1);
        OK
    })
}
fn GdipSetCustomLineCapStrokeCaps(c: &mut Ctx) -> u32 {
    let (s, e) = (c.a(1), c.a(2));
    on_cap(c.a(0), |x| {
        if !plain_cap(s) || !plain_cap(e) {
            return INVALID_PARAMETER;
        }
        x.caps = (s, e);
        OK
    })
}
/// an adjustable arrow's field: `f` reads it (as bits), `g` writes it; the base inset follows
/// the height and the width
fn on_arrow(h: u32, f: impl FnOnce(&mut (f32, f32, f32, bool)) -> u32) -> u32 {
    on_cap(h, |x| match &mut x.arrow {
        Some(a) => {
            let r = f(a);
            x.inset = a.0 / a.1;
            r
        }
        None => INVALID_PARAMETER,
    })
}
fn arrow_get(h: u32, out: u32, f: impl FnOnce(&(f32, f32, f32, bool)) -> u32) -> u32 {
    if out == 0 {
        return INVALID_PARAMETER;
    }
    on_arrow(h, |a| {
        w32(out, f(a));
        OK
    })
}
fn GdipGetAdjustableArrowCapHeight(c: &mut Ctx) -> u32 {
    arrow_get(c.a(0), c.a(1), |a| a.0.to_bits())
}
fn GdipGetAdjustableArrowCapWidth(c: &mut Ctx) -> u32 {
    arrow_get(c.a(0), c.a(1), |a| a.1.to_bits())
}
fn GdipGetAdjustableArrowCapMiddleInset(c: &mut Ctx) -> u32 {
    arrow_get(c.a(0), c.a(1), |a| a.2.to_bits())
}
fn GdipGetAdjustableArrowCapFillState(c: &mut Ctx) -> u32 {
    arrow_get(c.a(0), c.a(1), |a| a.3 as u32)
}
fn GdipSetAdjustableArrowCapHeight(c: &mut Ctx) -> u32 {
    let v = c.f(1);
    on_arrow(c.a(0), |a| {
        a.0 = v;
        OK
    })
}
fn GdipSetAdjustableArrowCapWidth(c: &mut Ctx) -> u32 {
    let v = c.f(1);
    on_arrow(c.a(0), |a| {
        a.1 = v;
        OK
    })
}
fn GdipSetAdjustableArrowCapMiddleInset(c: &mut Ctx) -> u32 {
    let v = c.f(1);
    on_arrow(c.a(0), |a| {
        a.2 = v;
        OK
    })
}
fn GdipSetAdjustableArrowCapFillState(c: &mut Ctx) -> u32 {
    let v = c.a(1) != 0;
    on_arrow(c.a(0), |a| {
        a.3 = v;
        OK
    })
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("GdipCreateCustomLineCap", GdipCreateCustomLineCap),
        ("GdipCreateAdjustableArrowCap", GdipCreateAdjustableArrowCap),
        ("GdipDeleteCustomLineCap", GdipDeleteCustomLineCap),
        ("GdipCloneCustomLineCap", GdipCloneCustomLineCap),
        ("GdipGetCustomLineCapType", GdipGetCustomLineCapType),
        ("GdipGetCustomLineCapBaseCap", GdipGetCustomLineCapBaseCap),
        ("GdipSetCustomLineCapBaseCap", GdipSetCustomLineCapBaseCap),
        ("GdipGetCustomLineCapBaseInset", GdipGetCustomLineCapBaseInset),
        ("GdipSetCustomLineCapBaseInset", GdipSetCustomLineCapBaseInset),
        ("GdipGetCustomLineCapStrokeJoin", GdipGetCustomLineCapStrokeJoin),
        ("GdipSetCustomLineCapStrokeJoin", GdipSetCustomLineCapStrokeJoin),
        ("GdipGetCustomLineCapWidthScale", GdipGetCustomLineCapWidthScale),
        ("GdipSetCustomLineCapWidthScale", GdipSetCustomLineCapWidthScale),
        ("GdipGetCustomLineCapStrokeCaps", GdipGetCustomLineCapStrokeCaps),
        ("GdipSetCustomLineCapStrokeCaps", GdipSetCustomLineCapStrokeCaps),
        ("GdipGetAdjustableArrowCapHeight", GdipGetAdjustableArrowCapHeight),
        ("GdipGetAdjustableArrowCapWidth", GdipGetAdjustableArrowCapWidth),
        ("GdipGetAdjustableArrowCapMiddleInset", GdipGetAdjustableArrowCapMiddleInset),
        ("GdipGetAdjustableArrowCapFillState", GdipGetAdjustableArrowCapFillState),
        ("GdipSetAdjustableArrowCapHeight", GdipSetAdjustableArrowCapHeight),
        ("GdipSetAdjustableArrowCapWidth", GdipSetAdjustableArrowCapWidth),
        ("GdipSetAdjustableArrowCapMiddleInset", GdipSetAdjustableArrowCapMiddleInset),
        ("GdipSetAdjustableArrowCapFillState", GdipSetAdjustableArrowCapFillState),
        ("GdipCreateMatrix", GdipCreateMatrix),
        ("GdipCreateMatrix2", GdipCreateMatrix2),
        ("GdipCreateMatrix3", GdipCreateMatrix3),
        ("GdipCreateMatrix3I", GdipCreateMatrix3I),
        ("GdipCloneMatrix", GdipCloneMatrix),
        ("GdipDeleteMatrix", GdipDeleteMatrix),
        ("GdipSetMatrixElements", GdipSetMatrixElements),
        ("GdipGetMatrixElements", GdipGetMatrixElements),
        ("GdipMultiplyMatrix", GdipMultiplyMatrix),
        ("GdipTranslateMatrix", GdipTranslateMatrix),
        ("GdipScaleMatrix", GdipScaleMatrix),
        ("GdipRotateMatrix", GdipRotateMatrix),
        ("GdipShearMatrix", GdipShearMatrix),
        ("GdipInvertMatrix", GdipInvertMatrix),
        ("GdipIsMatrixInvertible", GdipIsMatrixInvertible),
        ("GdipIsMatrixIdentity", GdipIsMatrixIdentity),
        ("GdipIsMatrixEqual", GdipIsMatrixEqual),
        ("GdipTransformMatrixPoints", GdipTransformMatrixPoints),
        ("GdipTransformMatrixPointsI", GdipTransformMatrixPointsI),
        ("GdipVectorTransformMatrixPoints", GdipVectorTransformMatrixPoints),
        ("GdipVectorTransformMatrixPointsI", GdipVectorTransformMatrixPointsI),
        ("GdipCreatePath", GdipCreatePath),
        ("GdipCreatePath2", GdipCreatePath2),
        ("GdipCreatePath2I", GdipCreatePath2I),
        ("GdipClonePath", GdipClonePath),
        ("GdipDeletePath", GdipDeletePath),
        ("GdipResetPath", GdipResetPath),
        ("GdipAddPathEllipse", GdipAddPathEllipse),
        ("GdipAddPathEllipseI", GdipAddPathEllipseI),
        ("GdipAddPathRectangle", GdipAddPathRectangle),
        ("GdipAddPathRectangleI", GdipAddPathRectangleI),
        ("GdipAddPathPolygon", GdipAddPathPolygon),
        ("GdipAddPathPolygonI", GdipAddPathPolygonI),
        ("GdipGetPointCount", GdipGetPointCount),
        ("GdipGetPathPoints", GdipGetPathPoints),
        ("GdipGetPathTypes", GdipGetPathTypes),
        ("GdipGetPathFillMode", GdipGetPathFillMode),
        ("GdipSetPathFillMode", GdipSetPathFillMode),
        ("GdipCreateSolidFill", GdipCreateSolidFill),
        ("GdipGetSolidFillColor", GdipGetSolidFillColor),
        ("GdipSetSolidFillColor", GdipSetSolidFillColor),
        ("GdipCloneBrush", GdipCloneBrush),
        ("GdipDeleteBrush", GdipDeleteBrush),
        ("GdipGetBrushType", GdipGetBrushType),
        ("GdipCreateHatchBrush", GdipCreateHatchBrush),
        ("GdipGetHatchStyle", GdipGetHatchStyle),
        ("GdipGetHatchForegroundColor", GdipGetHatchForegroundColor),
        ("GdipGetHatchBackgroundColor", GdipGetHatchBackgroundColor),
        ("GdipCreateTexture", GdipCreateTexture),
        ("GdipCreateTexture2", GdipCreateTexture2),
        ("GdipCreateTexture2I", GdipCreateTexture2I),
        ("GdipCreateTextureIA", GdipCreateTextureIA),
        ("GdipCreateTextureIAI", GdipCreateTextureIAI),
        ("GdipGetTextureTransform", GdipGetTextureTransform),
        ("GdipSetTextureTransform", GdipSetTextureTransform),
        ("GdipResetTextureTransform", GdipResetTextureTransform),
        ("GdipMultiplyTextureTransform", GdipMultiplyTextureTransform),
        ("GdipTranslateTextureTransform", GdipTranslateTextureTransform),
        ("GdipScaleTextureTransform", GdipScaleTextureTransform),
        ("GdipRotateTextureTransform", GdipRotateTextureTransform),
        ("GdipGetTextureWrapMode", GdipGetTextureWrapMode),
        ("GdipSetTextureWrapMode", GdipSetTextureWrapMode),
        ("GdipCreateLineBrush", GdipCreateLineBrush),
        ("GdipCreateLineBrushI", GdipCreateLineBrushI),
        ("GdipCreateLineBrushFromRect", GdipCreateLineBrushFromRect),
        ("GdipCreateLineBrushFromRectI", GdipCreateLineBrushFromRectI),
        ("GdipCreateLineBrushFromRectWithAngle", GdipCreateLineBrushFromRectWithAngle),
        ("GdipCreateLineBrushFromRectWithAngleI", GdipCreateLineBrushFromRectWithAngleI),
        ("GdipGetLineRect", GdipGetLineRect),
        ("GdipGetLineRectI", GdipGetLineRectI),
        ("GdipGetLineColors", GdipGetLineColors),
        ("GdipSetLineColors", GdipSetLineColors),
        ("GdipGetLineGammaCorrection", GdipGetLineGammaCorrection),
        ("GdipSetLineGammaCorrection", GdipSetLineGammaCorrection),
        ("GdipGetLineWrapMode", GdipGetLineWrapMode),
        ("GdipSetLineWrapMode", GdipSetLineWrapMode),
        ("GdipGetLineTransform", GdipGetLineTransform),
        ("GdipSetLineTransform", GdipSetLineTransform),
        ("GdipResetLineTransform", GdipResetLineTransform),
        ("GdipMultiplyLineTransform", GdipMultiplyLineTransform),
        ("GdipTranslateLineTransform", GdipTranslateLineTransform),
        ("GdipScaleLineTransform", GdipScaleLineTransform),
        ("GdipRotateLineTransform", GdipRotateLineTransform),
        ("GdipGetLineBlendCount", blend_count),
        ("GdipGetLineBlend", get_blend),
        ("GdipSetLineBlend", set_blend),
        ("GdipGetLinePresetBlendCount", preset_count),
        ("GdipGetLinePresetBlend", get_preset),
        ("GdipSetLinePresetBlend", set_preset),
        ("GdipSetLineLinearBlend", set_linear_blend),
        ("GdipSetLineSigmaBlend", set_sigma_blend),
        ("GdipCreatePathGradient", GdipCreatePathGradient),
        ("GdipCreatePathGradientI", GdipCreatePathGradientI),
        ("GdipCreatePathGradientFromPath", GdipCreatePathGradientFromPath),
        ("GdipGetPathGradientCenterPoint", GdipGetPathGradientCenterPoint),
        ("GdipGetPathGradientCenterPointI", GdipGetPathGradientCenterPointI),
        ("GdipSetPathGradientCenterPoint", GdipSetPathGradientCenterPoint),
        ("GdipSetPathGradientCenterPointI", GdipSetPathGradientCenterPointI),
        ("GdipGetPathGradientCenterColor", GdipGetPathGradientCenterColor),
        ("GdipSetPathGradientCenterColor", GdipSetPathGradientCenterColor),
        ("GdipGetPathGradientRect", GdipGetPathGradientRect),
        ("GdipGetPathGradientRectI", GdipGetPathGradientRectI),
        ("GdipGetPathGradientPointCount", GdipGetPathGradientPointCount),
        ("GdipGetPathGradientSurroundColorCount", GdipGetPathGradientSurroundColorCount),
        ("GdipGetPathGradientSurroundColorsWithCount", GdipGetPathGradientSurroundColorsWithCount),
        ("GdipSetPathGradientSurroundColorsWithCount", GdipSetPathGradientSurroundColorsWithCount),
        ("GdipGetPathGradientPath", GdipGetPathGradientPath),
        ("GdipGetPathGradientFocusScales", GdipGetPathGradientFocusScales),
        ("GdipSetPathGradientFocusScales", GdipSetPathGradientFocusScales),
        ("GdipGetPathGradientGammaCorrection", GdipGetPathGradientGammaCorrection),
        ("GdipSetPathGradientGammaCorrection", GdipSetPathGradientGammaCorrection),
        ("GdipGetPathGradientWrapMode", GdipGetPathGradientWrapMode),
        ("GdipSetPathGradientWrapMode", GdipSetPathGradientWrapMode),
        ("GdipGetPathGradientTransform", GdipGetPathGradientTransform),
        ("GdipSetPathGradientTransform", GdipSetPathGradientTransform),
        ("GdipResetPathGradientTransform", GdipResetPathGradientTransform),
        ("GdipMultiplyPathGradientTransform", GdipMultiplyPathGradientTransform),
        ("GdipTranslatePathGradientTransform", GdipTranslatePathGradientTransform),
        ("GdipScalePathGradientTransform", GdipScalePathGradientTransform),
        ("GdipRotatePathGradientTransform", GdipRotatePathGradientTransform),
        ("GdipGetPathGradientBlendCount", blend_count),
        ("GdipGetPathGradientBlend", get_blend),
        ("GdipSetPathGradientBlend", set_blend),
        ("GdipGetPathGradientPresetBlendCount", preset_count),
        ("GdipGetPathGradientPresetBlend", get_preset),
        ("GdipSetPathGradientPresetBlend", set_preset),
        ("GdipSetPathGradientLinearBlend", set_linear_blend),
        ("GdipSetPathGradientSigmaBlend", set_sigma_blend),
    ];
    for (n, f) in fns {
        implement("gdiplus", n, *f);
    }
    let _ = put32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn close(a: &[f32], b: &[f32], eps: f32) -> bool {
        a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < eps)
    }

    /// The transforms Wine's tests list for two points, a rectangle and a mode, a rectangle
    /// and an angle (all of them hold on Windows)
    #[test]
    fn line_transforms() {
        let pts: &[([f32; 2], [f32; 2], [f32; 4], M)] = &[
            ([1.0, 1.0], [100.0, 100.0], [1.0, 1.0, 99.0, 99.0], [1.0, 1.0, -1.0, 1.0, 50.5, -50.5]),
            ([0.0, 0.0], [0.0, 10.0], [-5.0, 0.0, 10.0, 10.0], [0.0, 1.0, -1.0, 0.0, 5.0, 5.0]),
            ([0.0, 0.0], [10.0, 0.0], [0.0, -5.0, 10.0, 10.0], IDENTITY),
            ([0.0, 0.0], [20.0, -20.0], [0.0, -20.0, 20.0, 20.0], [1.0, -1.0, 1.0, 1.0, 10.0, 10.0]),
            ([0.0, 0.0], [100.0, 1.0], [0.0, 0.0, 100.0, 1.0], [1.0, 0.01, -0.02, 2.0, 0.01, -1.0]),
            ([10.0, 10.0], [-90.0, 10.0], [-90.0, -40.0, 100.0, 100.0], [-1.0, 0.0, 0.0, -1.0, -80.0, 20.0]),
        ];
        for (p, q, rect, m) in pts {
            let (r, deg) = line_geometry(*p, *q).unwrap();
            assert_eq!(r, *rect);
            assert!(close(&line_transform(r, deg), m, 1e-3), "{p:?} {q:?}: {:?}", line_transform(r, deg));
        }
        assert!(line_geometry([1.0, 1.0], [1.0, 1.0]).is_none());
        let rect = scaled_transform;
        assert!(close(&rect([0.0, 0.0, 100.0, 10.0], 90.0), &[0.0, 0.1, -10.0, 0.0, 100.0, 0.0], 1e-3));
        assert!(close(&rect([10.0, -12.0, 100.0, 105.0], 90.0), &[0.0, 1.05, -0.952, 0.0, 98.571, -22.5], 1e-3));
        assert!(close(&rect([0.0, 0.0, 100.0, 10.0], -90.0), &[0.0, -0.1, 10.0, 0.0, 0.0, 10.0], 1e-3));
        assert!(close(&line_transform([10.0, -12.0, 100.0, 105.0], -90.0), &[0.0, -1.05, 0.952, 0.0, 21.429, 103.5], 1e-3));
        // winref gdipbrush: scalable 30 and 120 degrees, ForwardDiagonal (45)
        assert!(close(&rect([0.0, 0.0, 100.0, 10.0], 30.0), &[1.183, 0.068, -6.830, 1.183, 25.0, -4.330], 1e-3));
        assert!(close(&rect([10.0, -12.0, 100.0, 105.0], 120.0), &[-0.683, 1.242, -1.127, -0.683, 146.611, -6.368], 1e-3));
        assert!(close(&rect([-5.0, 3.0, 40.0, 80.0], 45.0), &[1.0, 2.0, -0.5, 1.0, 21.5, -30.0], 1e-3));
        // and not scalable
        assert!(close(&line_transform([0.0, 0.0, 100.0, 10.0], 30.0), &[0.793, 0.458, -2.933, 5.080, 25.0, -43.301], 1e-3));
    }

    #[test]
    fn matrix_ops() {
        // Wine's test_transform: (8 0 0 2 10 -20) times rotate 45 then scale (.25, .5)
        let mut m = IDENTITY;
        combine(&mut m, &rotation(45.0), 1);
        combine(&mut m, &[0.25, 0.0, 0.0, 0.5, 0.0, 0.0], 1);
        let r = mul(&[8.0, 0.0, 0.0, 2.0, 10.0, -20.0], &m);
        assert!(close(&r, &[1.414214, 2.828427, -0.353553, 0.707107, 5.303300, -3.535534], 1e-4));
        assert!(invert(&[1.0, 2.0, 2.0, 4.0, 0.0, 0.0]).is_none());
        assert_eq!(combine(&mut m, &IDENTITY, 2), INVALID_PARAMETER);
    }

    #[test]
    fn blends() {
        assert_eq!(linear_blend(0.6, 0.8), vec![(0.0, 0.0), (0.8, 0.6), (0.0, 1.0)]);
        assert_eq!(linear_blend(0.0, 0.8), vec![(0.8, 0.0), (0.0, 1.0)]);
        assert_eq!(linear_blend(1.0, 0.8), vec![(0.0, 0.0), (0.8, 1.0)]);
        let b = Blend { factors: vec![(0.0, 0.0), (1.0, 0.5), (0.0, 1.0)], preset: Vec::new() };
        assert_eq!(b.color(0xff000000, 0xffffffff, 0.5), 0xffffffff);
        assert_eq!(b.color(0xff000000, 0xffffffff, 0.25), 0xff808080);
        let p = Blend { factors: Vec::new(), preset: vec![(0xffff0000, 0.0), (0xff0000ff, 1.0)] };
        assert_eq!(p.color(0, 0, 1.0), 0xff0000ff);
        assert_eq!(mix_premul(0x400000ff, 0x80ff0000, 64), 0x50660099);
        assert_eq!(mix_premul(0x400000ff, 0x80ff0000, 234), 0x7af4000a);
        assert_eq!(mix_premul(0xffffffff, 0xff000000, 64), 0xffbfbfbf);
        assert_eq!(mix_premul(0xffffffff, 0xff000000, 234), 0xff151515);
        let s = sigma_blend(0.5, 1.0);
        assert_eq!(s.len(), 511);
        for (i, f) in [(63, 0.139), (126, 0.490), (252, 0.997), (315, 0.872), (441, 0.164), (504, 0.006)] {
            assert!((s[i].0 - f).abs() < 2e-3, "{i}: {}", s[i].0);
        }
        assert!(ends_ok(&[0.5]) && ends_ok(&[0.0, 1.0]) && !ends_ok(&[0.0, 0.8]) && !ends_ok(&[0.2, 1.0]));
        assert_eq!(wrap_coord(-1, 5, false), 4);
        assert_eq!(wrap_coord(5, 5, true), 4);
    }

    /// Wine's test_pathgradientcenterpoint: an ellipse's center is the mean of its 13 points
    #[test]
    fn ellipse_center() {
        let p = ellipse_points(0.0, 0.0, 100.0, 50.0);
        assert_eq!(p.len(), 13);
        let x: f32 = p.iter().map(|q| q[0]).sum::<f32>() / 13.0;
        let y: f32 = p.iter().map(|q| q[1]).sum::<f32>() / 13.0;
        assert!((x - 700.0 / 13.0).abs() < 1e-3 && (y - 25.0).abs() < 1e-3);
    }

    #[test]
    fn path_positions() {
        let sq = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        assert_eq!(path_t(&sq, [5.0, 5.0], 5.0, 5.0).unwrap().0, 0.0);
        let (t, k, s) = path_t(&sq, [5.0, 5.0], 10.0, 5.0).unwrap();
        assert!((t - 1.0).abs() < 1e-5 && k == 1 && (s - 0.5).abs() < 1e-5);
        assert!((path_t(&sq, [5.0, 5.0], 7.5, 5.0).unwrap().0 - 0.5).abs() < 1e-5);
        assert!(path_t(&sq, [5.0, 5.0], 12.0, 5.0).is_none());
    }

    proptest! {
        /// a matrix times its inverse is the identity; prepending then appending is (op m op')
        #[test]
        fn inverse_roundtrip(m in prop::array::uniform6(-50.0f32..50.0)) {
            prop_assume!(det(&m).abs() > 0.5);
            let i = invert(&m).unwrap();
            let r = mul(&m, &i);
            prop_assert!(close(&r, &IDENTITY, 1e-2), "{:?}", r);
        }
        /// the transform of a gradient line maps the rectangle's center to itself, and the
        /// rectangle's left and right edges' midpoints on the line through the points
        #[test]
        fn line_ends(p in prop::array::uniform2(-100.0f32..100.0), q in prop::array::uniform2(-100.0f32..100.0)) {
            prop_assume!((p[0] - q[0]).abs() > 0.5 || (p[1] - q[1]).abs() > 0.5);
            let (r, deg) = line_geometry(p, q).unwrap();
            let m = line_transform(r, deg);
            let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
            let (x, y) = apply(&m, cx, cy);
            prop_assert!((x - cx).abs() < 1e-2 && (y - cy).abs() < 1e-2);
            // the ends of the gradient (the rectangle's left and right sides through its
            // center) lie on the line from p to q
            let (dx, dy) = (q[0] - p[0], q[1] - p[1]);
            for u in [r[0], r[0] + r[2]] {
                let (x, y) = apply(&m, u, cy);
                let cross = (x - p[0]) * dy - (y - p[1]) * dx;
                prop_assert!(cross.abs() < 1e-1 * (dx.abs() + dy.abs()).max(1.0), "{cross}");
            }
        }
        /// colors along a gradient stay between its two colors, channel by channel
        #[test]
        fn lerp_between(a in any::<u32>(), b in any::<u32>(), t in 0.0f32..=1.0) {
            let c = lerp(a, b, t);
            for s in [0, 8, 16, 24] {
                let (x, y, z) = (a >> s & 255, b >> s & 255, c >> s & 255);
                prop_assert!(x.min(y) <= z && z <= x.max(y));
            }
        }
        /// blends that end at 0 and 1 give factors inside their own range
        #[test]
        fn sigma_in_range(focus in 0.0f32..=1.0, scale in 0.0f32..=1.0) {
            let b = sigma_blend(focus, scale);
            prop_assert!(b.first().unwrap().1 == 0.0 && b.last().unwrap().1 == 1.0);
            prop_assert!(b.iter().all(|&(f, _)| (0.0..=scale + 1e-6).contains(&f)));
        }
    }
}
