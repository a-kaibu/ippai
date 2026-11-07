//! glu32: the GLU 1.2 functions games call, on sake's opengl32 (kura's GL): the matrix helpers
//! (gluOrtho2D, gluPerspective, gluLookAt, gluPickMatrix multiply the current matrix with
//! glMultMatrixd), gluProject / gluUnProject, gluScaleImage and gluBuild1D/2DMipmaps (box
//! filter, as SGI's GLU), the quadrics (sphere, cylinder, disk with glBegin/glEnd), the strings
//! and gluCheckExtension. The tessellator and NURBS are not there (their calls do nothing).
//!
//! The math is pure (tested below on the host); the guest side reads arguments and calls GL.

#![allow(non_snake_case)]

use std::f64::consts::PI;
use std::sync::{Mutex, OnceLock};

use crate::api::{implement, Ctx};
use crate::mem::{self, r32, w32};
use crate::HashMap;

pub type M4 = [f64; 16];

/// a * b, column-major (OpenGL's order)
pub fn mul(a: &M4, b: &M4) -> M4 {
    let mut r = [0.0; 16];
    for c in 0..4 {
        for row in 0..4 {
            r[c * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[c * 4 + k]).sum();
        }
    }
    r
}

pub fn identity() -> M4 {
    let mut m = [0.0; 16];
    m[0] = 1.0;
    m[5] = 1.0;
    m[10] = 1.0;
    m[15] = 1.0;
    m
}

/// gluPerspective's matrix (None when it does nothing: zNear == zFar, sin(fovy/2) == 0,
/// aspect == 0)
pub fn perspective(fovy: f64, aspect: f64, near: f64, far: f64) -> Option<M4> {
    let r = fovy / 2.0 * PI / 180.0;
    let dz = far - near;
    let s = r.sin();
    if dz == 0.0 || s == 0.0 || aspect == 0.0 {
        return None;
    }
    let cot = r.cos() / s;
    let mut m = identity();
    m[0] = cot / aspect;
    m[5] = cot;
    m[10] = -(far + near) / dz;
    m[11] = -1.0;
    m[14] = -2.0 * near * far / dz;
    m[15] = 0.0;
    Some(m)
}

fn norm(v: [f64; 3]) -> [f64; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l == 0.0 {
        v
    } else {
        [v[0] / l, v[1] / l, v[2] / l]
    }
}
fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// gluLookAt's matrix (rotation, then the translation by -eye)
pub fn look_at(eye: [f64; 3], center: [f64; 3], up: [f64; 3]) -> M4 {
    let f = norm([center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]]);
    let s = norm(cross(f, up));
    let u = cross(s, f);
    let mut m = identity();
    for i in 0..3 {
        m[i * 4] = s[i];
        m[i * 4 + 1] = u[i];
        m[i * 4 + 2] = -f[i];
    }
    let mut t = identity();
    t[12] = -eye[0];
    t[13] = -eye[1];
    t[14] = -eye[2];
    mul(&m, &t)
}

/// gluPickMatrix's matrix (None for an empty region)
pub fn pick(x: f64, y: f64, dx: f64, dy: f64, vp: [i32; 4]) -> Option<M4> {
    if dx <= 0.0 || dy <= 0.0 {
        return None;
    }
    let mut m = identity();
    m[0] = vp[2] as f64 / dx;
    m[5] = vp[3] as f64 / dy;
    m[12] = (vp[2] as f64 - 2.0 * (x - vp[0] as f64)) / dx;
    m[13] = (vp[3] as f64 - 2.0 * (y - vp[1] as f64)) / dy;
    Some(m)
}

fn transform(m: &M4, v: [f64; 4]) -> [f64; 4] {
    let mut r = [0.0; 4];
    for row in 0..4 {
        r[row] = (0..4).map(|k| m[k * 4 + row] * v[k]).sum();
    }
    r
}

/// The inverse of a 4x4 matrix (None when singular)
pub fn invert(m: &M4) -> Option<M4> {
    let mut a = [[0.0f64; 8]; 4];
    for r in 0..4 {
        for c in 0..4 {
            a[r][c] = m[c * 4 + r];
        }
        a[r][4 + r] = 1.0;
    }
    for c in 0..4 {
        let p = (c..4).max_by(|&x, &y| a[x][c].abs().total_cmp(&a[y][c].abs()))?;
        if a[p][c] == 0.0 {
            return None;
        }
        a.swap(c, p);
        let d = a[c][c];
        for k in 0..8 {
            a[c][k] /= d;
        }
        for r in 0..4 {
            if r != c {
                let f = a[r][c];
                for k in 0..8 {
                    a[r][k] -= f * a[c][k];
                }
            }
        }
    }
    let mut out = [0.0; 16];
    for r in 0..4 {
        for c in 0..4 {
            out[c * 4 + r] = a[r][4 + c];
        }
    }
    Some(out)
}

/// gluProject: window coordinates of an object point (None when w is 0)
pub fn project(obj: [f64; 3], model: &M4, proj: &M4, vp: [i32; 4]) -> Option<[f64; 3]> {
    let e = transform(model, [obj[0], obj[1], obj[2], 1.0]);
    let c = transform(proj, e);
    if c[3] == 0.0 {
        return None;
    }
    let n = [c[0] / c[3], c[1] / c[3], c[2] / c[3]];
    Some([
        vp[0] as f64 + (n[0] * 0.5 + 0.5) * vp[2] as f64,
        vp[1] as f64 + (n[1] * 0.5 + 0.5) * vp[3] as f64,
        n[2] * 0.5 + 0.5,
    ])
}

/// gluUnProject: the object point of window coordinates (None for a singular matrix or w 0)
pub fn unproject(win: [f64; 3], model: &M4, proj: &M4, vp: [i32; 4]) -> Option<[f64; 3]> {
    let inv = invert(&mul(proj, model))?;
    if vp[2] == 0 || vp[3] == 0 {
        return None;
    }
    let n = [
        (win[0] - vp[0] as f64) / vp[2] as f64 * 2.0 - 1.0,
        (win[1] - vp[1] as f64) / vp[3] as f64 * 2.0 - 1.0,
        win[2] * 2.0 - 1.0,
        1.0,
    ];
    let o = transform(&inv, n);
    if o[3] == 0.0 {
        return None;
    }
    Some([o[0] / o[3], o[1] / o[3], o[2] / o[3]])
}

/// Bytes per pixel of a GL format with GL_UNSIGNED_BYTE (None: not one GLU scales)
pub fn components(format: u32) -> Option<usize> {
    Some(match format {
        0x1900 | 0x1903..=0x1906 | 0x1909 => 1, // COLOR_INDEX, RED, GREEN, BLUE, ALPHA, LUMINANCE
        0x190a => 2,                            // LUMINANCE_ALPHA
        0x1907 | 0x80e0 => 3,                   // RGB, BGR_EXT
        0x1908 | 0x80e1 => 4,                   // RGBA, BGRA_EXT
        _ => return None,
    })
}

fn row_bytes(w: usize, comps: usize, align: usize) -> usize {
    let n = w * comps;
    let a = align.max(1);
    n.div_ceil(a) * a
}

/// gluScaleImage on bytes, as SGI's GLU (Mesa's libGLU) does it: the bytes widened to 16 bits
/// (x 257), each output pixel the box filter of the input over its footprint (the footprint is
/// one input pixel wide when growing; a box past an edge wraps around to the other side), in
/// f32, then the top byte. An exact halving averages 2x2 blocks with rounding. Rows are
/// `align`-byte aligned in and out (GL_UNPACK/PACK_ALIGNMENT).
pub fn scale_image(comps: usize, wi: usize, hi: usize, src: &[u8], wo: usize, ho: usize, align: usize) -> Vec<u8> {
    let (si, so) = (row_bytes(wi, comps, align), row_bytes(wo, comps, align));
    let mut out = vec![0u8; so * ho];
    if wi == 0 || hi == 0 || wo == 0 || ho == 0 {
        return out;
    }
    let px = |x: usize, y: usize, k: usize| src.get(y * si + x * comps + k).copied().unwrap_or(0) as u32 * 257;
    if wi == wo * 2 && hi == ho * 2 {
        for y in 0..ho {
            for x in 0..wo {
                for k in 0..comps {
                    let s = px(2 * x, 2 * y, k) + px(2 * x + 1, 2 * y, k) + px(2 * x, 2 * y + 1, k) + px(2 * x + 1, 2 * y + 1, k);
                    out[y * so + x * comps + k] = (((s + 2) / 4) >> 8) as u8;
                }
            }
        }
        return out;
    }
    let convy = hi as f32 / ho as f32;
    let convx = wi as f32 / wo as f32;
    let (halfx, halfy) = (convx / 2.0, convy / 2.0);
    for i in 0..ho {
        let y = convy * (i as f32 + 0.5);
        let (lowy, highy) = if hi > ho { (y - halfy, y + halfy) } else { (y - 0.5, y + 0.5) };
        for j in 0..wo {
            let x = convx * (j as f32 + 0.5);
            let (lowx, highx) = if wi > wo { (x - halfx, x + halfx) } else { (x - 0.5, x + 0.5) };
            let mut totals = [0f32; 4];
            let mut area = 0f32;
            let mut y = lowy;
            let mut yint = y.floor() as i64;
            while y < highy {
                let yi = (yint + hi as i64).rem_euclid(hi as i64) as usize;
                let yp = if highy < (yint + 1) as f32 { highy - y } else { (yint + 1) as f32 - y };
                let mut x = lowx;
                let mut xint = x.floor() as i64;
                while x < highx {
                    let xi = (xint + wi as i64).rem_euclid(wi as i64) as usize;
                    let xp = if highx < (xint + 1) as f32 { highx - x } else { (xint + 1) as f32 - x };
                    let p = xp * yp;
                    area += p;
                    for (k, t) in totals.iter_mut().enumerate().take(comps) {
                        *t += px(xi, yi, k) as f32 * p;
                    }
                    xint += 1;
                    x = xint as f32;
                }
                yint += 1;
                y = yint as f32;
            }
            for k in 0..comps {
                let v = ((totals[k] + 0.5) / area) as u32;
                out[i * so + j * comps + k] = (v.min(65535) >> 8) as u8;
            }
        }
    }
    out
}

/// The power of two nearest `n`, as libGLU's nearestPower picks it (up when the bit below the top
/// one is set: 3 -> 4, 6 -> 8, 5 -> 4), capped at `max`
pub fn nearest_pot(n: usize, max: usize) -> usize {
    let (mut v, mut i) = (n.max(1), 1usize);
    let p = loop {
        if v == 1 {
            break i;
        }
        if v == 3 {
            break i * 4;
        }
        v >>= 1;
        i *= 2;
    };
    p.min(max.max(1))
}

/// libGLU's halveImage_ubyte: each 2x2 block's rounded mean; a single row or column (1D) halves
/// pairs, truncating. `si` is the input's row size in bytes; the output is tightly packed.
pub fn halve_ubyte(comps: usize, w: usize, h: usize, src: &[u8], si: usize) -> Vec<u8> {
    let at = |o: usize| src.get(o).copied().unwrap_or(0) as u32;
    let mut out = Vec::new();
    if w == 1 || h == 1 {
        if h == 1 {
            for j in 0..w / 2 {
                for k in 0..comps {
                    out.push(((at(2 * j * comps + k) + at((2 * j + 1) * comps + k)) / 2) as u8);
                }
            }
        } else {
            for j in 0..h / 2 {
                for k in 0..comps {
                    out.push(((at(2 * j * si + k) + at((2 * j + 1) * si + k)) / 2) as u8);
                }
            }
        }
        return out;
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            let t = 2 * y * si + 2 * x * comps;
            for k in 0..comps {
                out.push(((at(t + k) + at(t + comps + k) + at(t + si + k) + at(t + si + comps + k) + 2) / 4) as u8);
            }
        }
    }
    out
}

/// libGLU's scale_internal_ubyte, which gluBuild2DMipmaps resizes level 0 with (not gluScaleImage's
/// filter): boxes of conv = in/out pixels stepped with integer and fraction parts, the edge
/// pixels weighted by their coverage, the sum divided by the box area and truncated. An exact
/// halving is halve_ubyte. Tightly packed output.
pub fn scale_ubyte(comps: usize, wi: usize, hi: usize, src: &[u8], si: usize, wo: usize, ho: usize) -> Vec<u8> {
    if wi == wo * 2 && hi == ho * 2 {
        return halve_ubyte(comps, wi, hi, src, si);
    }
    let at = |o: isize| if o < 0 { 0.0 } else { src.get(o as usize).copied().unwrap_or(0) as f32 };
    let g = comps as isize;
    let ys = si as isize;
    let convy = hi as f32 / ho as f32;
    let convx = wi as f32 / wo as f32;
    let (convy_int, convx_int) = (convy.floor() as isize, convx.floor() as isize);
    let (convy_f, convx_f) = (convy - convy_int as f32, convx - convx_int as f32);
    let area = convx * convy;
    let mut out = vec![0u8; wo * ho * comps];
    let (mut lowy_int, mut lowy_f, mut highy_int, mut highy_f) = (0isize, 0f32, convy_int, convy_f);
    for i in 0..ho {
        if highy_int >= hi as isize {
            highy_int = hi as isize - 1;
        }
        let (mut lowx_int, mut lowx_f, mut highx_int, mut highx_f) = (0isize, 0f32, convx_int, convx_f);
        for j in 0..wo {
            let mut totals = [0f32; 4];
            let xindex = lowx_int * g;
            if highy_int > lowy_int && highx_int > lowx_int {
                let mut yp = 1.0 - lowy_f;
                let mut temp = xindex + lowy_int * ys;
                let mut percent = yp * (1.0 - lowx_f);
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
                let mut left = temp;
                for _ in lowx_int + 1..highx_int {
                    temp += g;
                    for (k, t) in totals.iter_mut().enumerate().take(comps) {
                        *t += at(temp + k as isize) * yp;
                    }
                }
                temp += g;
                let mut right = temp;
                percent = yp * highx_f;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
                // the last row
                yp = highy_f;
                percent = yp * (1.0 - lowx_f);
                temp = xindex + highy_int * ys;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
                for _ in lowx_int + 1..highx_int {
                    temp += g;
                    for (k, t) in totals.iter_mut().enumerate().take(comps) {
                        *t += at(temp + k as isize) * yp;
                    }
                }
                temp += g;
                percent = yp * highx_f;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
                // the first and last columns (left and right walk on by a pixel each row, as
                // libGLU's do)
                for _ in lowy_int + 1..highy_int {
                    left += ys;
                    right += ys;
                    for t in totals.iter_mut().take(comps) {
                        *t += at(left) * (1.0 - lowx_f) + at(right) * highx_f;
                        left += 1;
                        right += 1;
                    }
                }
            } else if highy_int > lowy_int {
                let xp = highx_f - lowx_f;
                let mut percent = (1.0 - lowy_f) * xp;
                let mut temp = xindex + lowy_int * ys;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
                for _ in lowy_int + 1..highy_int {
                    temp += ys;
                    for (k, t) in totals.iter_mut().enumerate().take(comps) {
                        *t += at(temp + k as isize) * xp;
                    }
                }
                percent = xp * highy_f;
                temp += ys;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
            } else if highx_int > lowx_int {
                let yp = highy_f - lowy_f;
                let mut percent = (1.0 - lowx_f) * yp;
                let mut temp = xindex + lowy_int * ys;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
                for _ in lowx_int + 1..highx_int {
                    temp += g;
                    for (k, t) in totals.iter_mut().enumerate().take(comps) {
                        *t += at(temp + k as isize) * yp;
                    }
                }
                temp += g;
                percent = yp * highx_f;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
            } else {
                let percent = (highy_f - lowy_f) * (highx_f - lowx_f);
                let temp = xindex + lowy_int * ys;
                for (k, t) in totals.iter_mut().enumerate().take(comps) {
                    *t += at(temp + k as isize) * percent;
                }
            }
            // the body
            let mut temp0 = xindex + g + (lowy_int + 1) * ys;
            for _ in lowy_int + 1..highy_int {
                let mut temp = temp0;
                for _ in lowx_int + 1..highx_int {
                    for (k, t) in totals.iter_mut().enumerate().take(comps) {
                        *t += at(temp + k as isize);
                    }
                    temp += g;
                }
                temp0 += ys;
            }
            for k in 0..comps {
                out[(j + i * wo) * comps + k] = (totals[k] / area).clamp(0.0, 255.0) as u8;
            }
            lowx_int = highx_int;
            lowx_f = highx_f;
            highx_int += convx_int;
            highx_f += convx_f;
            if highx_f > 1.0 {
                highx_f -= 1.0;
                highx_int += 1;
            }
            // (no clamp at the right edge, as in libGLU 9.0.2: the last box may take a
            // weight-0 or partial pixel from past the row, the next row's first)
        }
        lowy_int = highy_int;
        lowy_f = highy_f;
        highy_int += convy_int;
        highy_f += convy_f;
        if highy_f > 1.0 {
            highy_f -= 1.0;
            highy_int += 1;
        }
    }
    out
}

/// The mipmap chain gluBuild2DMipmaps uploads, as libGLU makes it: (width, height, pixels
/// tightly packed) from level 0 (the image resized to the nearest powers of two with
/// scale_ubyte) down to 1x1, each level halve_ubyte of the one above. `align` is the input
/// rows' alignment.
pub fn mipmaps(comps: usize, w: usize, h: usize, src: &[u8], max: usize, align: usize) -> Vec<(usize, usize, Vec<u8>)> {
    let si = row_bytes(w, comps, align);
    let (mut cw, mut ch) = (nearest_pot(w.max(1), max), nearest_pot(h.max(1), max));
    let mut cur = if (cw, ch) == (w, h) {
        (0..h).flat_map(|y| (0..w * comps).map(move |i| y * si + i)).map(|o| src.get(o).copied().unwrap_or(0)).collect()
    } else {
        scale_ubyte(comps, w, h, src, si, cw, ch)
    };
    let mut v = Vec::new();
    loop {
        let next = (cw > 1 || ch > 1).then(|| ((cw / 2).max(1), (ch / 2).max(1), halve_ubyte(comps, cw, ch, &cur, cw * comps)));
        v.push((cw, ch, std::mem::take(&mut cur)));
        let Some((nw, nh, n)) = next else { break };
        (cw, ch, cur) = (nw, nh, n);
    }
    v
}

// ---------------------------------------------------------------------------------------------
// the guest side

use crate::gl::call_gl;

fn dbl(v: f64) -> [u32; 2] {
    let b = v.to_bits();
    [b as u32, (b >> 32) as u32]
}

/// glMultMatrixd(m) from a guest copy of `m`
fn mult(m: &M4) {
    let p = mem::heap_alloc(128, false);
    for (i, v) in m.iter().enumerate() {
        mem::w64(p + 8 * i as u32, v.to_bits());
    }
    call_gl("glMultMatrixd", &[p]);
    mem::heap_free(p);
}

/// Argument at x86 stack slot `slot` (after doubles: 2 slots each) / x64 position `pos`
fn arg(c: &Ctx, slot: u32, pos: u32) -> u32 {
    if mem::x64() {
        c.a64(pos) as u32
    } else {
        c.a(slot)
    }
}
/// The double at x86 slot `slot` / x64 position `pos`
fn darg(c: &Ctx, slot: u32, pos: u32) -> f64 {
    if mem::x64() {
        f64::from_bits(c.xmm(pos))
    } else {
        f64::from_bits(c.a(slot) as u64 | (c.a(slot + 1) as u64) << 32)
    }
}
fn read_m4(p: u32) -> M4 {
    let mut m = [0.0; 16];
    for (i, v) in m.iter_mut().enumerate() {
        *v = f64::from_bits(mem::r64(p + 8 * i as u32));
    }
    m
}
fn read_vp(p: u32) -> [i32; 4] {
    [0, 1, 2, 3].map(|i| r32(p + 4 * i) as i32)
}

fn gluOrtho2D(c: &mut Ctx) -> u32 {
    let v: Vec<u32> = [darg(c, 0, 0), darg(c, 2, 1), darg(c, 4, 2), darg(c, 6, 3), -1.0, 1.0].iter().flat_map(|&x| dbl(x)).collect();
    call_gl("glOrtho", &v);
    0
}
fn gluPerspective(c: &mut Ctx) -> u32 {
    if let Some(m) = perspective(darg(c, 0, 0), darg(c, 2, 1), darg(c, 4, 2), darg(c, 6, 3)) {
        mult(&m);
    }
    0
}
fn gluLookAt(c: &mut Ctx) -> u32 {
    let d: Vec<f64> = (0..9).map(|i| darg(c, 2 * i, i)).collect();
    mult(&look_at([d[0], d[1], d[2]], [d[3], d[4], d[5]], [d[6], d[7], d[8]]));
    0
}
fn gluPickMatrix(c: &mut Ctx) -> u32 {
    let vp = arg(c, 8, 4);
    if vp == 0 {
        return 0;
    }
    if let Some(m) = pick(darg(c, 0, 0), darg(c, 2, 1), darg(c, 4, 2), darg(c, 6, 3), read_vp(vp)) {
        mult(&m);
    }
    0
}
/// gluProject(objx, objy, objz, model, proj, viewport, winx, winy, winz)
fn gluProject(c: &mut Ctx) -> u32 {
    let o = [darg(c, 0, 0), darg(c, 2, 1), darg(c, 4, 2)];
    let p: Vec<u32> = (0..6).map(|i| arg(c, 6 + i, 3 + i)).collect();
    if p.contains(&0) {
        return 0;
    }
    match project(o, &read_m4(p[0]), &read_m4(p[1]), read_vp(p[2])) {
        Some(w) => {
            for i in 0..3 {
                mem::w64(p[3 + i], w[i].to_bits());
            }
            1
        }
        None => 0,
    }
}
fn gluUnProject(c: &mut Ctx) -> u32 {
    let w = [darg(c, 0, 0), darg(c, 2, 1), darg(c, 4, 2)];
    let p: Vec<u32> = (0..6).map(|i| arg(c, 6 + i, 3 + i)).collect();
    if p.contains(&0) {
        return 0;
    }
    match unproject(w, &read_m4(p[0]), &read_m4(p[1]), read_vp(p[2])) {
        Some(o) => {
            for i in 0..3 {
                mem::w64(p[3 + i], o[i].to_bits());
            }
            1
        }
        None => 0,
    }
}

const GLU_INVALID_ENUM: u32 = 100900;
const GLU_INVALID_VALUE: u32 = 100901;
const GL_UNSIGNED_BYTE: u32 = 0x1401;

fn gl_int(pname: u32) -> i32 {
    let p = mem::heap_alloc(64, true);
    call_gl("glGetIntegerv", &[pname, p]);
    let v = r32(p) as i32;
    mem::heap_free(p);
    v
}

/// gluScaleImage(format, wIn, hIn, typeIn, dataIn, wOut, hOut, typeOut, dataOut)
fn gluScaleImage(c: &mut Ctx) -> u32 {
    let a: Vec<u32> = (0..9).map(|i| c.a(i)).collect();
    let Some(comps) = components(a[0]) else { return GLU_INVALID_ENUM };
    if a[3] != GL_UNSIGNED_BYTE || a[7] != GL_UNSIGNED_BYTE {
        return GLU_INVALID_ENUM;
    }
    let (wi, hi, wo, ho) = (a[1] as i32, a[2] as i32, a[5] as i32, a[6] as i32);
    if wi < 0 || hi < 0 || wo < 0 || ho < 0 {
        return GLU_INVALID_VALUE;
    }
    let (ua, pa) = (gl_int(0x0cf5).clamp(1, 8) as usize, gl_int(0x0d05).clamp(1, 8) as usize);
    let src = mem::slice(a[4], (row_bytes(wi as usize, comps, ua) * hi as usize) as u32).to_vec();
    // (rows unpacked at the unpack alignment, packed at the pack alignment)
    let tight = scale_image(comps, wi as usize, hi as usize, &src, wo as usize, ho as usize, ua);
    let (si, so) = (row_bytes(wo as usize, comps, ua), row_bytes(wo as usize, comps, pa));
    for y in 0..ho as usize {
        mem::write(a[8] + (y * so) as u32, &tight[y * si..y * si + wo as usize * comps]);
    }
    0
}

/// gluBuild2DMipmaps(target, internalFormat, width, height, format, type, data)
fn gluBuild2DMipmaps(c: &mut Ctx) -> u32 {
    let a: Vec<u32> = (0..7).map(|i| c.a(i)).collect();
    build_mipmaps(a[0], a[1], a[2] as i32, a[3] as i32, a[4], a[5], a[6], false)
}
/// gluBuild1DMipmaps(target, internalFormat, width, format, type, data)
fn gluBuild1DMipmaps(c: &mut Ctx) -> u32 {
    let a: Vec<u32> = (0..6).map(|i| c.a(i)).collect();
    build_mipmaps(a[0], a[1], a[2] as i32, 1, a[3], a[4], a[5], true)
}
#[allow(clippy::too_many_arguments)]
fn build_mipmaps(target: u32, internal: u32, w: i32, h: i32, format: u32, ty: u32, data: u32, one_d: bool) -> u32 {
    let Some(comps) = components(format) else { return GLU_INVALID_ENUM };
    if ty != GL_UNSIGNED_BYTE {
        return GLU_INVALID_ENUM;
    }
    if w < 1 || h < 1 {
        return GLU_INVALID_VALUE;
    }
    let align = gl_int(0x0cf5).clamp(1, 8) as usize;
    let max = gl_int(0x0d33).max(64) as usize; // GL_MAX_TEXTURE_SIZE
    let src = mem::slice(data, (row_bytes(w as usize, comps, align) * h as usize) as u32).to_vec();
    // (the levels are tightly packed: unpack them at alignment 1, as libGLU does)
    call_gl("glPixelStorei", &[0x0cf5, 1]);
    for (level, (lw, lh, px)) in mipmaps(comps, w as usize, h as usize, &src, max, align).into_iter().enumerate() {
        if one_d && lh > 1 {
            continue;
        }
        let p = mem::heap_alloc(px.len().max(4) as u32, false);
        mem::write(p, &px);
        if one_d {
            call_gl("glTexImage1D", &[target, level as u32, internal, lw as u32, 0, format, ty, p]);
        } else {
            call_gl("glTexImage2D", &[target, level as u32, internal, lw as u32, lh as u32, 0, format, ty, p]);
        }
        mem::heap_free(p);
    }
    call_gl("glPixelStorei", &[0x0cf5, align as u32]);
    0
}

/// guest copies of strings (once each)
fn gstr(s: &str, wide: bool) -> u32 {
    static S: Mutex<Option<HashMap<(String, bool), u32>>> = Mutex::new(None);
    *S.lock().unwrap().get_or_insert_with(HashMap::default).entry((s.into(), wide)).or_insert_with(|| {
        let b: Vec<u8> = if wide { s.encode_utf16().chain([0]).flat_map(|u| u.to_le_bytes()).collect() } else { s.bytes().chain([0]).collect() };
        let p = mem::heap_alloc(b.len() as u32, false);
        mem::write(p, &b);
        p
    })
}

/// gluErrorString's text for GLU and GL error codes (None: not one)
pub fn error_text(e: u32) -> Option<&'static str> {
    Some(match e {
        0 => "no error",
        0x0500 => "invalid enumerant",
        0x0501 => "invalid value",
        0x0502 => "invalid operation",
        0x0503 => "stack overflow",
        0x0504 => "stack underflow",
        0x0505 => "out of memory",
        100900 => "invalid enumerant",
        100901 => "invalid value",
        100902 => "out of memory",
        100903 => "incompatible gl version",
        _ => return None,
    })
}
fn gluErrorString(c: &mut Ctx) -> u32 {
    error_text(c.a(0)).map_or(0, |s| gstr(s, false))
}
fn gluErrorUnicodeStringEXT(c: &mut Ctx) -> u32 {
    error_text(c.a(0)).map_or(0, |s| gstr(s, true))
}
/// GLU_VERSION, GLU_EXTENSIONS (Windows' glu32: GLU 1.2)
fn gluGetString(c: &mut Ctx) -> u32 {
    match c.a(0) {
        100800 => gstr("1.2.2.0 Microsoft Corporation", false),
        100801 => gstr("", false),
        _ => 0,
    }
}
/// whole-word match of `name` in the space-separated `list`
pub fn has_extension(name: &[u8], list: &[u8]) -> bool {
    !name.is_empty() && list.split(|&b| b == b' ').any(|w| w == name)
}
fn gluCheckExtension(c: &mut Ctx) -> u32 {
    let (n, l) = (c.a(0), c.a(1));
    if n == 0 || l == 0 {
        return 0;
    }
    has_extension(&mem::cstr(n), &mem::cstr(l)) as u32
}

// ---- quadrics: { draw style, normals, orientation, texture } -----------------------------------

const GLU_FILL: u32 = 100012;
const GLU_LINE: u32 = 100011;
const GLU_POINT: u32 = 100010;
const GLU_SILHOUETTE: u32 = 100013;
const GLU_NONE: u32 = 100002;
const GLU_INSIDE: u32 = 100021;

fn gluNewQuadric(_: &mut Ctx) -> u32 {
    let q = mem::heap_alloc(16, true);
    w32(q, GLU_FILL);
    w32(q + 4, 100000); // GLU_SMOOTH
    w32(q + 8, 100020); // GLU_OUTSIDE
    w32(q + 12, 0);
    q
}
fn gluDeleteQuadric(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        mem::heap_free(c.a(0));
    }
    0
}
fn set_field(c: &Ctx, o: u32) -> u32 {
    if c.a(0) != 0 {
        w32(c.a(0) + o, c.a(1));
    }
    0
}

/// one vertex: normal (unless GLU_NONE), texture coordinate (when on), position
fn vtx(q: [u32; 4], n: [f64; 3], t: (f64, f64), p: [f64; 3]) {
    let f = |v: f64| (v as f32).to_bits();
    let s = if q[2] == GLU_INSIDE { -1.0 } else { 1.0 };
    if q[1] != GLU_NONE {
        call_gl("glNormal3f", &[f(n[0] * s), f(n[1] * s), f(n[2] * s)]);
    }
    if q[3] != 0 {
        call_gl("glTexCoord2f", &[f(t.0), f(t.1)]);
    }
    call_gl("glVertex3f", &[f(p[0]), f(p[1]), f(p[2])]);
}
fn quad(p: u32) -> [u32; 4] {
    [r32(p), r32(p + 4), r32(p + 8), r32(p + 12)]
}
/// GL primitive for a strip in the quadric's style
fn strip_mode(style: u32) -> u32 {
    match style {
        GLU_POINT => 0,                    // GL_POINTS
        GLU_LINE | GLU_SILHOUETTE => 3,    // GL_LINE_STRIP
        _ => 5,                            // GL_TRIANGLE_STRIP
    }
}

/// gluCylinder(q, base, top, height, slices, stacks)
fn gluCylinder(c: &mut Ctx) -> u32 {
    let qp = c.a(0);
    if qp == 0 {
        return 0;
    }
    let q = quad(qp);
    let (base, top, height) = (darg(c, 1, 1), darg(c, 3, 2), darg(c, 5, 3));
    let (slices, stacks) = (arg(c, 7, 4) as i32, arg(c, 8, 5) as i32);
    if slices < 2 || stacks < 1 || base < 0.0 || top < 0.0 || height < 0.0 {
        return 0;
    }
    let nz = (base - top) / height.max(f64::MIN_POSITIVE);
    for j in 0..stacks {
        let (z0, z1) = (height * j as f64 / stacks as f64, height * (j + 1) as f64 / stacks as f64);
        let (r0, r1) = (base + (top - base) * j as f64 / stacks as f64, base + (top - base) * (j + 1) as f64 / stacks as f64);
        call_gl("glBegin", &[strip_mode(q[0])]);
        for i in 0..=slices {
            let a = 2.0 * PI * i as f64 / slices as f64;
            let (s, co) = (a.sin(), a.cos());
            let n = norm([s, co, nz]);
            let u = i as f64 / slices as f64;
            vtx(q, n, (u, j as f64 / stacks as f64), [r0 * s, r0 * co, z0]);
            vtx(q, n, (u, (j + 1) as f64 / stacks as f64), [r1 * s, r1 * co, z1]);
        }
        call_gl("glEnd", &[]);
    }
    0
}
/// gluSphere(q, radius, slices, stacks)
fn gluSphere(c: &mut Ctx) -> u32 {
    let qp = c.a(0);
    if qp == 0 {
        return 0;
    }
    let q = quad(qp);
    let r = darg(c, 1, 1);
    let (slices, stacks) = (arg(c, 3, 2) as i32, arg(c, 4, 3) as i32);
    if slices < 2 || stacks < 1 || r < 0.0 {
        return 0;
    }
    for j in 0..stacks {
        let (p0, p1) = (PI * j as f64 / stacks as f64, PI * (j + 1) as f64 / stacks as f64);
        call_gl("glBegin", &[strip_mode(q[0])]);
        for i in 0..=slices {
            let a = 2.0 * PI * i as f64 / slices as f64;
            let u = 1.0 - i as f64 / slices as f64;
            for p in [p0, p1] {
                let n = [-a.sin() * p.sin(), a.cos() * p.sin(), p.cos()];
                vtx(q, n, (u, 1.0 - p / PI), [n[0] * r, n[1] * r, n[2] * r]);
            }
        }
        call_gl("glEnd", &[]);
    }
    0
}
/// gluPartialDisk(q, inner, outer, slices, loops, start, sweep) (degrees, clockwise from +y)
fn partial_disk(q: [u32; 4], inner: f64, outer: f64, slices: i32, loops: i32, start: f64, sweep: f64) {
    if slices < 2 || loops < 1 || outer <= 0.0 || inner < 0.0 || inner > outer {
        return;
    }
    let sweep = sweep.clamp(-360.0, 360.0);
    for l in 0..loops {
        let (r0, r1) = (inner + (outer - inner) * l as f64 / loops as f64, inner + (outer - inner) * (l + 1) as f64 / loops as f64);
        call_gl("glBegin", &[strip_mode(q[0])]);
        for i in 0..=slices {
            let a = (start + sweep * i as f64 / slices as f64) * PI / 180.0;
            let (s, co) = (a.sin(), a.cos());
            for r in [r1, r0] {
                vtx(q, [0.0, 0.0, 1.0], (0.5 + s * r / outer / 2.0, 0.5 + co * r / outer / 2.0), [r * s, r * co, 0.0]);
            }
        }
        call_gl("glEnd", &[]);
    }
}
/// gluDisk(q, inner, outer, slices, loops)
fn gluDisk(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        partial_disk(quad(c.a(0)), darg(c, 1, 1), darg(c, 3, 2), arg(c, 5, 3) as i32, arg(c, 6, 4) as i32, 0.0, 360.0);
    }
    0
}
fn gluPartialDisk(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        partial_disk(quad(c.a(0)), darg(c, 1, 1), darg(c, 3, 2), arg(c, 5, 3) as i32, arg(c, 6, 4) as i32, darg(c, 7, 5), darg(c, 9, 6));
    }
    0
}

pub fn init() {
    static DONE: OnceLock<()> = OnceLock::new();
    DONE.get_or_init(|| ());
    for (n, f) in [
        ("gluOrtho2D", gluOrtho2D as crate::api::Handler),
        ("gluPerspective", gluPerspective),
        ("gluLookAt", gluLookAt),
        ("gluPickMatrix", gluPickMatrix),
        ("gluProject", gluProject),
        ("gluUnProject", gluUnProject),
        ("gluScaleImage", gluScaleImage),
        ("gluBuild1DMipmaps", gluBuild1DMipmaps),
        ("gluBuild2DMipmaps", gluBuild2DMipmaps),
        ("gluErrorString", gluErrorString),
        ("gluErrorUnicodeStringEXT", gluErrorUnicodeStringEXT),
        ("gluGetString", gluGetString),
        ("gluCheckExtension", gluCheckExtension),
        ("gluNewQuadric", gluNewQuadric),
        ("gluDeleteQuadric", gluDeleteQuadric),
        ("gluQuadricDrawStyle", |c| set_field(c, 0)),
        ("gluQuadricNormals", |c| set_field(c, 4)),
        ("gluQuadricOrientation", |c| set_field(c, 8)),
        ("gluQuadricTexture", |c| set_field(c, 12)),
        ("gluQuadricCallback", |_| 0),
        ("gluCylinder", gluCylinder),
        ("gluSphere", gluSphere),
        ("gluDisk", gluDisk),
        ("gluPartialDisk", gluPartialDisk),
    ] {
        implement("glu32", n, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn f(&mut self, lo: f64, hi: f64) -> f64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            lo + (hi - lo) * ((self.0 >> 11) as f64 / (1u64 << 53) as f64)
        }
    }

    fn close(a: &[f64], b: &[f64], eps: f64) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps * (1.0 + y.abs()))
    }

    // Differential tests against Mesa's libGLU 9.0.2 (on OSMesa): testdata/glu/gen.c wrote what it
    // answered. Its matrices went through GL's float matrix stack, so they agree to float precision.
    fn nums(s: &str) -> Vec<f64> {
        s.split_whitespace().filter_map(|w| w.parse().ok()).collect()
    }

    #[test]
    fn matrices_match_libglu() {
        let mut n = [0; 3];
        for l in include_str!("../../../testdata/glu/matrix.txt").lines() {
            let (kind, rest) = l.split_once(' ').unwrap();
            let v = nums(rest);
            let (args, want) = v.split_at(v.len() - 16);
            let got = match kind {
                "persp" => perspective(args[0], args[1], args[2], args[3]).unwrap_or_else(identity),
                "lookat" => look_at([args[0], args[1], args[2]], [args[3], args[4], args[5]], [args[6], args[7], args[8]]),
                "pick" => pick(args[0], args[1], args[2], args[3], [args[4] as i32, args[5] as i32, args[6] as i32, args[7] as i32]).unwrap_or_else(identity),
                _ => unreachable!(),
            };
            let ok = got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 2e-6 * (1.0 + w.abs()));
            assert!(ok, "{l}\n got {got:?}");
            n[["persp", "lookat", "pick"].iter().position(|k| *k == kind).unwrap()] += 1;
        }
        assert_eq!(n, [200, 200, 100]);
    }

    #[test]
    fn project_unproject_match_libglu() {
        let mut checked = 0;
        for l in include_str!("../../../testdata/glu/project.txt").lines() {
            let v = nums(&l.replace(['m', 'p', 'v', 'o', '=', 'u', 'n'], " "));
            let (m, p) = (std::array::from_fn::<f64, 16, _>(|i| v[i]), std::array::from_fn::<f64, 16, _>(|i| v[16 + i]));
            let vp = [v[32] as i32, v[33] as i32, v[34] as i32, v[35] as i32];
            let o = [v[36], v[37], v[38]];
            let (r, w, r2, u) = (v[39], [v[40], v[41], v[42]], v[43], [v[44], v[45], v[46]]);
            let got = project(o, &m, &p, vp);
            assert_eq!(got.is_some(), r == 1.0, "{l}");
            if let Some(g) = got {
                assert!(close(&g, &w, 1e-9), "{l}\n got {g:?}");
            }
            let got = unproject([o[0] * 10.0, o[1] * 10.0, o[2] / 10.0], &m, &p, vp);
            assert_eq!(got.is_some(), r2 == 1.0, "{l}");
            if let Some(g) = got {
                // (libGLU inverts with its own Gaussian elimination: the same to rounding,
                // relative to the size of what is solved for)
                let scale = u.iter().fold(1.0f64, |a, x| a.max(x.abs()));
                assert!(g.iter().zip(&u).all(|(a, b)| (a - b).abs() <= 1e-7 * scale), "{l}\n got {g:?}");
            }
            checked += 1;
        }
        assert_eq!(checked, 200);
    }

    #[test]
    fn scale_image_matches_libglu() {
        let hex = |s: &str| (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let mut n = 0;
        for l in include_str!("../../../testdata/glu/scale.txt").lines() {
            let mut parts = l.split('|');
            let h: Vec<usize> = parts.next().unwrap().split_whitespace().map(|x| x.parse().unwrap()).collect();
            let (src, want) = (hex(parts.next().unwrap()), hex(parts.next().unwrap()));
            let (comps, wi, hi, wo, ho, ua) = (h[0], h[1], h[2], h[3], h[4], h[5]);
            let out = scale_image(comps, wi, hi, &src, wo, ho, ua);
            let so = row_bytes(wo, comps, ua);
            let got: Vec<u8> = (0..ho).flat_map(|y| out[y * so..y * so + wo * comps].to_vec()).collect();
            assert_eq!(got, want, "{}", &l[..l.find('|').unwrap()]);
            n += 1;
        }
        assert_eq!(n, 400);
    }

    #[test]
    fn eye_maps_to_origin_and_center_ahead() {
        let mut r = Rng(7);
        for _ in 0..2000 {
            let eye = [r.f(-9.0, 9.0), r.f(-9.0, 9.0), r.f(-9.0, 9.0)];
            let ctr = [r.f(-9.0, 9.0), r.f(-9.0, 9.0), r.f(-9.0, 9.0)];
            let m = look_at(eye, ctr, [0.0, 1.0, 0.0]);
            let e = transform(&m, [eye[0], eye[1], eye[2], 1.0]);
            assert!(e[..3].iter().all(|v| v.abs() < 1e-9));
            let c = transform(&m, [ctr[0], ctr[1], ctr[2], 1.0]);
            let d = ((ctr[0] - eye[0]).powi(2) + (ctr[1] - eye[1]).powi(2) + (ctr[2] - eye[2]).powi(2)).sqrt();
            if d > 1e-3 && (ctr[0] - eye[0]).abs() + (ctr[2] - eye[2]).abs() > 1e-3 {
                assert!(c[0].abs() < 1e-8 && c[1].abs() < 1e-8 && (c[2] + d).abs() < 1e-8, "{c:?} {d}");
            }
        }
    }

    /// gluUnProject(gluProject(p)) == p over random scenes
    #[test]
    fn project_roundtrip() {
        let mut r = Rng(0x1234_5678);
        let vp = [10, 20, 640, 480];
        for _ in 0..5000 {
            let proj = perspective(r.f(20.0, 120.0), r.f(0.5, 2.0), r.f(0.1, 2.0), r.f(50.0, 500.0)).unwrap();
            let model = look_at([r.f(-5.0, 5.0), r.f(-5.0, 5.0), r.f(5.0, 20.0)], [0.0; 3], [0.0, 1.0, 0.0]);
            let p = [r.f(-3.0, 3.0), r.f(-3.0, 3.0), r.f(-3.0, 3.0)];
            let Some(w) = project(p, &model, &proj, vp) else { continue };
            let back = unproject(w, &model, &proj, vp).unwrap();
            assert!(close(&back, &p, 1e-6), "{p:?} -> {w:?} -> {back:?}");
        }
    }

    #[test]
    fn invert_inverts() {
        let mut r = Rng(99);
        for _ in 0..2000 {
            let m: M4 = std::array::from_fn(|_| r.f(-4.0, 4.0));
            if let Some(i) = invert(&m) {
                let p = mul(&m, &i);
                assert!(close(&p, &identity(), 1e-6) || p.iter().any(|x| !x.is_finite()), "{p:?}");
            }
        }
        assert!(invert(&[0.0; 16]).is_none());
    }

    #[test]
    fn pick_maps_the_region_to_the_unit_square() {
        let m = pick(50.0, 60.0, 10.0, 20.0, [0, 0, 200, 100]).unwrap();
        // the region's centre goes to NDC (0, 0), its corners to +-1
        let c = transform(&m, [50.0 / 100.0 - 1.0, 60.0 / 50.0 - 1.0, 0.0, 1.0]);
        assert!(c[0].abs() < 1e-12 && c[1].abs() < 1e-12);
        let k = transform(&m, [55.0 / 100.0 - 1.0, 70.0 / 50.0 - 1.0, 0.0, 1.0]);
        assert!((k[0] - 1.0).abs() < 1e-12 && (k[1] - 1.0).abs() < 1e-12);
        assert!(pick(0.0, 0.0, 0.0, 1.0, [0, 0, 1, 1]).is_none());
    }

    #[test]
    fn scale_keeps_flat_images_flat_and_identity() {
        let mut r = Rng(5);
        for _ in 0..300 {
            let (comps, wi, hi) = (1 + (r.f(0.0, 4.0) as usize).min(3), 1 + r.f(0.0, 40.0) as usize, 1 + r.f(0.0, 40.0) as usize);
            let (wo, ho) = (1 + r.f(0.0, 40.0) as usize, 1 + r.f(0.0, 40.0) as usize);
            let align = [1, 2, 4, 8][r.f(0.0, 4.0) as usize % 4];
            let color: Vec<u8> = (0..comps).map(|_| r.f(0.0, 256.0) as u8).collect();
            let si = row_bytes(wi, comps, align);
            let mut src = vec![0u8; si * hi];
            for y in 0..hi {
                for x in 0..wi {
                    src[y * si + x * comps..][..comps].copy_from_slice(&color);
                }
            }
            let out = scale_image(comps, wi, hi, &src, wo, ho, align);
            let so = row_bytes(wo, comps, align);
            for y in 0..ho {
                for x in 0..wo {
                    assert_eq!(&out[y * so + x * comps..][..comps], &color[..]);
                }
            }
            let same = scale_image(comps, wi, hi, &src, wi, hi, align);
            for y in 0..hi {
                assert_eq!(&same[y * si..y * si + wi * comps], &src[y * si..y * si + wi * comps]);
            }
        }
    }

    #[test]
    fn mipmaps_match_libglu() {
        let hex = |s: &str| (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect::<Vec<u8>>();
        let (mut n, mut bad) = (0, Vec::new());
        for l in include_str!("../../../testdata/glu/mipmap.txt").lines() {
            let mut parts = l.split('|');
            let h: Vec<usize> = parts.next().unwrap().split_whitespace().map(|x| x.parse().unwrap()).collect();
            let src = hex(parts.next().unwrap());
            let (comps, w, hgt, align) = (h[0], h[1], h[2], h[3]);
            let got = mipmaps(comps, w, hgt, &src, 2048, align);
            let want: Vec<(usize, usize, Vec<u8>)> = parts
                .map(|p| {
                    let (sz, px) = p.split_once(':').unwrap();
                    let (a, b) = sz.split_once('x').unwrap();
                    (a.parse().unwrap(), b.parse().unwrap(), hex(px))
                })
                .collect();
            // (libGLU 9.0.2 uploads levels past 0 tightly packed but leaves the caller's unpack
            // alignment on: rows that are not a multiple of it read past each row, so those levels
            // hold whatever followed; they are not compared, nor is anything but their size)
            // (level 0 too when it was resized)
            let resized = want.first().is_some_and(|l| (l.0, l.1) != (w, hgt));
            let ill = |lv: usize, lw: usize, lh: usize| (lv > 0 || resized) && lh > 1 && lw * comps % align != 0;
            let mask = |v: &[(usize, usize, Vec<u8>)]| -> Vec<(usize, usize, Vec<u8>)> {
                v.iter().enumerate().map(|(lv, (lw, lh, px))| (*lw, *lh, if ill(lv, *lw, *lh) { Vec::new() } else { px.clone() })).collect()
            };
            let (tight, want) = (mask(&got), mask(&want));
            if tight != want {
                bad.push(format!("{} {:?} vs {:?}", &l[..l.find('|').unwrap()], tight.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>(), want.iter().map(|x| (x.0, x.1)).collect::<Vec<_>>()));
            }
            n += 1;
        }
        assert_eq!(n, 150);
        assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad.join("\n"));
    }

    #[test]
    fn halving_averages_2x2_blocks() {
        let src = [0u8, 100, 50, 150, 10, 20, 30, 40, 200, 0, 255, 255, 1, 2, 3, 4];
        let out = scale_image(1, 4, 4, &src, 2, 2, 1);
        // (16-bit sums rounded, then the top byte, as libGLU's halveImage)
        assert_eq!(out, vec![32, 67, 50, 129]);
    }

    #[test]
    fn mipmap_chain() {
        let src = vec![128u8; 4 * 100 * 60];
        let m = mipmaps(4, 100, 60, &src, 2048, 4);
        let sizes: Vec<(usize, usize)> = m.iter().map(|l| (l.0, l.1)).collect();
        assert_eq!(sizes, vec![(128, 64), (64, 32), (32, 16), (16, 8), (8, 4), (4, 2), (2, 1), (1, 1)]);
        assert!(m.iter().all(|l| l.2.len() == l.0 * l.1 * 4 && l.2.iter().all(|&b| b == 128)));
        assert_eq!(nearest_pot(3, 1024), 4);
        assert_eq!(nearest_pot(5, 1024), 4);
        assert_eq!(nearest_pot(6, 1024), 8);
        assert_eq!(nearest_pot(4000, 1024), 1024);
    }

    #[test]
    fn extensions_match_whole_words() {
        assert!(has_extension(b"GL_EXT_bgra", b"GL_ARB_multitexture GL_EXT_bgra"));
        assert!(!has_extension(b"GL_EXT", b"GL_EXT_bgra"));
        assert!(!has_extension(b"", b"GL_EXT_bgra"));
        assert_eq!(error_text(100900), Some("invalid enumerant"));
    }
}
