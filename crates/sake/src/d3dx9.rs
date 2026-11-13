//! D3DX 9 (d3dx9_24 .. d3dx9_43): the math helpers, ID3DXBuffer, a shader assembler
//! (D3DXAssembleShader: vs/ps 1.x-3.0 assembly to bytecode), D3DXFillTexture (the game's
//! callback for each texel) and a font object. Everything else in the export table is a logged
//! stub returning 0.

#![allow(non_snake_case)]

use crate::api::{implement, vtable, Ctx, Handler};
use crate::mem::{self, r32, w32};

type M = [f32; 16];

fn fl(c: &Ctx, i: u32) -> f32 {
    f32::from_bits(c.a(i))
}
fn rf(p: u32, i: u32) -> f32 {
    f32::from_bits(r32(p + 4 * i))
}
fn rm(p: u32) -> M {
    std::array::from_fn(|i| rf(p, i as u32))
}
fn wm(p: u32, m: &M) {
    for (i, v) in m.iter().enumerate() {
        w32(p + 4 * i as u32, v.to_bits());
    }
}
fn rv<const N: usize>(p: u32) -> [f32; N] {
    std::array::from_fn(|i| rf(p, i as u32))
}
fn wv(p: u32, v: &[f32]) {
    for (i, x) in v.iter().enumerate() {
        w32(p + 4 * i as u32, x.to_bits());
    }
}

const ID: M = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn mul(a: &M, b: &M) -> M {
    std::array::from_fn(|k| {
        let (i, j) = (k / 4, k % 4);
        (0..4).map(|n| a[i * 4 + n] * b[n * 4 + j]).sum()
    })
}

fn inverse(m: &M) -> Option<(M, f32)> {
    let mut inv = [0f32; 16];
    let a = |i: usize| m[i] as f64;
    let r = [
        a(5) * a(10) * a(15) - a(5) * a(11) * a(14) - a(9) * a(6) * a(15) + a(9) * a(7) * a(14) + a(13) * a(6) * a(11) - a(13) * a(7) * a(10),
        -a(1) * a(10) * a(15) + a(1) * a(11) * a(14) + a(9) * a(2) * a(15) - a(9) * a(3) * a(14) - a(13) * a(2) * a(11) + a(13) * a(3) * a(10),
        a(1) * a(6) * a(15) - a(1) * a(7) * a(14) - a(5) * a(2) * a(15) + a(5) * a(3) * a(14) + a(13) * a(2) * a(7) - a(13) * a(3) * a(6),
        -a(1) * a(6) * a(11) + a(1) * a(7) * a(10) + a(5) * a(2) * a(11) - a(5) * a(3) * a(10) - a(9) * a(2) * a(7) + a(9) * a(3) * a(6),
        -a(4) * a(10) * a(15) + a(4) * a(11) * a(14) + a(8) * a(6) * a(15) - a(8) * a(7) * a(14) - a(12) * a(6) * a(11) + a(12) * a(7) * a(10),
        a(0) * a(10) * a(15) - a(0) * a(11) * a(14) - a(8) * a(2) * a(15) + a(8) * a(3) * a(14) + a(12) * a(2) * a(11) - a(12) * a(3) * a(10),
        -a(0) * a(6) * a(15) + a(0) * a(7) * a(14) + a(4) * a(2) * a(15) - a(4) * a(3) * a(14) - a(12) * a(2) * a(7) + a(12) * a(3) * a(6),
        a(0) * a(6) * a(11) - a(0) * a(7) * a(10) - a(4) * a(2) * a(11) + a(4) * a(3) * a(10) + a(8) * a(2) * a(7) - a(8) * a(3) * a(6),
        a(4) * a(9) * a(15) - a(4) * a(11) * a(13) - a(8) * a(5) * a(15) + a(8) * a(7) * a(13) + a(12) * a(5) * a(11) - a(12) * a(7) * a(9),
        -a(0) * a(9) * a(15) + a(0) * a(11) * a(13) + a(8) * a(1) * a(15) - a(8) * a(3) * a(13) - a(12) * a(1) * a(11) + a(12) * a(3) * a(9),
        a(0) * a(5) * a(15) - a(0) * a(7) * a(13) - a(4) * a(1) * a(15) + a(4) * a(3) * a(13) + a(12) * a(1) * a(7) - a(12) * a(3) * a(5),
        -a(0) * a(5) * a(11) + a(0) * a(7) * a(9) + a(4) * a(1) * a(11) - a(4) * a(3) * a(9) - a(8) * a(1) * a(7) + a(8) * a(3) * a(5),
        -a(4) * a(9) * a(14) + a(4) * a(10) * a(13) + a(8) * a(5) * a(14) - a(8) * a(6) * a(13) - a(12) * a(5) * a(10) + a(12) * a(6) * a(9),
        a(0) * a(9) * a(14) - a(0) * a(10) * a(13) - a(8) * a(1) * a(14) + a(8) * a(2) * a(13) + a(12) * a(1) * a(10) - a(12) * a(2) * a(9),
        -a(0) * a(5) * a(14) + a(0) * a(6) * a(13) + a(4) * a(1) * a(14) - a(4) * a(2) * a(13) - a(12) * a(1) * a(6) + a(12) * a(2) * a(5),
        a(0) * a(5) * a(10) - a(0) * a(6) * a(9) - a(4) * a(1) * a(10) + a(4) * a(2) * a(9) + a(8) * a(1) * a(6) - a(8) * a(2) * a(5),
    ];
    let det = a(0) * r[0] + a(1) * r[4] + a(2) * r[8] + a(3) * r[12];
    if det == 0.0 {
        return None;
    }
    for i in 0..16 {
        inv[i] = (r[i] / det) as f32;
    }
    Some((inv, det as f32))
}

fn transform4(v: [f32; 4], m: &M) -> [f32; 4] {
    std::array::from_fn(|j| (0..4).map(|i| v[i] * m[i * 4 + j]).sum())
}

fn norm3(v: [f32; 3]) -> [f32; 3] {
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    if l == 0.0 { [0.0; 3] } else { v.map(|x| x / l) }
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn dot3(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn out_m(c: &Ctx, m: M) -> u32 {
    wm(c.a(0), &m);
    c.a(0)
}

fn D3DXMatrixMultiply(c: &mut Ctx) -> u32 {
    out_m(c, mul(&rm(c.a(1)), &rm(c.a(2))))
}
fn D3DXMatrixMultiplyTranspose(c: &mut Ctx) -> u32 {
    let m = mul(&rm(c.a(1)), &rm(c.a(2)));
    out_m(c, std::array::from_fn(|k| m[(k % 4) * 4 + k / 4]))
}
fn D3DXMatrixTranspose(c: &mut Ctx) -> u32 {
    let m = rm(c.a(1));
    out_m(c, std::array::from_fn(|k| m[(k % 4) * 4 + k / 4]))
}
fn D3DXMatrixInverse(c: &mut Ctx) -> u32 {
    match inverse(&rm(c.a(2))) {
        Some((m, det)) => {
            if c.a(1) != 0 {
                w32(c.a(1), det.to_bits());
            }
            out_m(c, m)
        }
        None => 0,
    }
}
fn D3DXMatrixDeterminant(c: &mut Ctx) -> u32 {
    c.st0 = Some(inverse(&rm(c.a(0))).map_or(0.0, |x| x.1) as f64);
    0
}
fn D3DXMatrixTranslation(c: &mut Ctx) -> u32 {
    let mut m = ID;
    (m[12], m[13], m[14]) = (fl(c, 1), fl(c, 2), fl(c, 3));
    out_m(c, m)
}
fn D3DXMatrixScaling(c: &mut Ctx) -> u32 {
    let mut m = ID;
    (m[0], m[5], m[10]) = (fl(c, 1), fl(c, 2), fl(c, 3));
    out_m(c, m)
}
fn rot(axis: usize, a: f32) -> M {
    let (s, co) = a.sin_cos();
    let mut m = ID;
    let (i, j) = match axis {
        0 => (1, 2),
        1 => (2, 0),
        _ => (0, 1),
    };
    m[i * 4 + i] = co;
    m[j * 4 + j] = co;
    m[i * 4 + j] = s;
    m[j * 4 + i] = -s;
    m
}
fn D3DXMatrixRotationX(c: &mut Ctx) -> u32 {
    out_m(c, rot(0, fl(c, 1)))
}
fn D3DXMatrixRotationY(c: &mut Ctx) -> u32 {
    out_m(c, rot(1, fl(c, 1)))
}
fn D3DXMatrixRotationZ(c: &mut Ctx) -> u32 {
    out_m(c, rot(2, fl(c, 1)))
}
fn D3DXMatrixRotationYawPitchRoll(c: &mut Ctx) -> u32 {
    let (y, p, r) = (fl(c, 1), fl(c, 2), fl(c, 3));
    out_m(c, mul(&mul(&rot(2, r), &rot(0, p)), &rot(1, y)))
}
fn look_at(eye: [f32; 3], at: [f32; 3], up: [f32; 3], lh: bool) -> M {
    let z = norm3(if lh { [at[0] - eye[0], at[1] - eye[1], at[2] - eye[2]] } else { [eye[0] - at[0], eye[1] - at[1], eye[2] - at[2]] });
    let x = norm3(cross(up, z));
    let y = cross(z, x);
    [x[0], y[0], z[0], 0.0, x[1], y[1], z[1], 0.0, x[2], y[2], z[2], 0.0, -dot3(x, eye), -dot3(y, eye), -dot3(z, eye), 1.0]
}
fn D3DXMatrixLookAtLH(c: &mut Ctx) -> u32 {
    out_m(c, look_at(rv(c.a(1)), rv(c.a(2)), rv(c.a(3)), true))
}
fn D3DXMatrixLookAtRH(c: &mut Ctx) -> u32 {
    out_m(c, look_at(rv(c.a(1)), rv(c.a(2)), rv(c.a(3)), false))
}
fn persp(fov: f32, aspect: f32, zn: f32, zf: f32, lh: bool) -> M {
    let ys = 1.0 / (fov / 2.0).tan();
    let mut m = [0f32; 16];
    m[0] = ys / aspect;
    m[5] = ys;
    let q = zf / (zf - zn);
    m[10] = if lh { q } else { -q };
    m[11] = if lh { 1.0 } else { -1.0 };
    m[14] = -zn * q;
    m
}
fn D3DXMatrixPerspectiveFovLH(c: &mut Ctx) -> u32 {
    out_m(c, persp(fl(c, 1), fl(c, 2), fl(c, 3), fl(c, 4), true))
}
fn D3DXMatrixPerspectiveFovRH(c: &mut Ctx) -> u32 {
    out_m(c, persp(fl(c, 1), fl(c, 2), fl(c, 3), fl(c, 4), false))
}
fn D3DXMatrixPerspectiveLH(c: &mut Ctx) -> u32 {
    let (w, h, zn, zf) = (fl(c, 1), fl(c, 2), fl(c, 3), fl(c, 4));
    let mut m = [0f32; 16];
    (m[0], m[5], m[10], m[11], m[14]) = (2.0 * zn / w, 2.0 * zn / h, zf / (zf - zn), 1.0, zn * zf / (zn - zf));
    out_m(c, m)
}
fn ortho_off_center(l: f32, r: f32, b: f32, t: f32, zn: f32, zf: f32, lh: bool) -> M {
    let mut m = ID;
    m[0] = 2.0 / (r - l);
    m[5] = 2.0 / (t - b);
    m[10] = if lh { 1.0 } else { -1.0 } / (zf - zn);
    m[12] = (l + r) / (l - r);
    m[13] = (t + b) / (b - t);
    m[14] = zn / (zn - zf);
    m
}
fn D3DXMatrixOrthoLH(c: &mut Ctx) -> u32 {
    let (w, h) = (fl(c, 1), fl(c, 2));
    out_m(c, ortho_off_center(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, fl(c, 3), fl(c, 4), true))
}
fn D3DXMatrixOrthoRH(c: &mut Ctx) -> u32 {
    let (w, h) = (fl(c, 1), fl(c, 2));
    out_m(c, ortho_off_center(-w / 2.0, w / 2.0, -h / 2.0, h / 2.0, fl(c, 3), fl(c, 4), false))
}
fn D3DXMatrixOrthoOffCenterLH(c: &mut Ctx) -> u32 {
    out_m(c, ortho_off_center(fl(c, 1), fl(c, 2), fl(c, 3), fl(c, 4), fl(c, 5), fl(c, 6), true))
}
fn D3DXMatrixOrthoOffCenterRH(c: &mut Ctx) -> u32 {
    out_m(c, ortho_off_center(fl(c, 1), fl(c, 2), fl(c, 3), fl(c, 4), fl(c, 5), fl(c, 6), false))
}

/// D3DXMatrixTransformation2D(out, scaling center, scaling rotation, scaling, rotation center,
/// rotation, translation)
fn D3DXMatrixTransformation2D(c: &mut Ctx) -> u32 {
    let v2 = |p: u32, d: [f32; 2]| if p != 0 { rv::<2>(p) } else { d };
    let (sc, sr, s, rc, r, t) = (v2(c.a(1), [0.0; 2]), fl(c, 2), v2(c.a(3), [1.0; 2]), v2(c.a(4), [0.0; 2]), fl(c, 5), v2(c.a(6), [0.0; 2]));
    let tr = |x: f32, y: f32| {
        let mut m = ID;
        (m[12], m[13]) = (x, y);
        m
    };
    let mut sm = ID;
    (sm[0], sm[5]) = (s[0], s[1]);
    // Msc^-1 * Msr^-1 * Ms * Msr * Msc * Mrc^-1 * Mr * Mrc * Mt
    let m = [tr(-sc[0], -sc[1]), rot(2, -sr), sm, rot(2, sr), tr(sc[0], sc[1]), tr(-rc[0], -rc[1]), rot(2, r), tr(rc[0], rc[1]), tr(t[0], t[1])]
        .iter()
        .fold(ID, |a, b| mul(&a, b));
    out_m(c, m)
}

fn D3DXVec3Normalize(c: &mut Ctx) -> u32 {
    wv(c.a(0), &norm3(rv(c.a(1))));
    c.a(0)
}
fn D3DXVec2Normalize(c: &mut Ctx) -> u32 {
    let v: [f32; 2] = rv(c.a(1));
    let l = v[0].hypot(v[1]);
    wv(c.a(0), &if l == 0.0 { [0.0; 2] } else { v.map(|x| x / l) });
    c.a(0)
}
fn D3DXVec4Normalize(c: &mut Ctx) -> u32 {
    let v: [f32; 4] = rv(c.a(1));
    let l = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    wv(c.a(0), &if l == 0.0 { [0.0; 4] } else { v.map(|x| x / l) });
    c.a(0)
}
fn D3DXVec3Transform(c: &mut Ctx) -> u32 {
    let v: [f32; 3] = rv(c.a(1));
    wv(c.a(0), &transform4([v[0], v[1], v[2], 1.0], &rm(c.a(2))));
    c.a(0)
}
fn coord(v: [f32; 3], m: &M) -> [f32; 3] {
    let r = transform4([v[0], v[1], v[2], 1.0], m);
    let w = if r[3] == 0.0 { 1.0 } else { r[3] };
    [r[0] / w, r[1] / w, r[2] / w]
}
fn D3DXVec3TransformCoord(c: &mut Ctx) -> u32 {
    wv(c.a(0), &coord(rv(c.a(1)), &rm(c.a(2))));
    c.a(0)
}
fn D3DXVec3TransformNormal(c: &mut Ctx) -> u32 {
    let v: [f32; 3] = rv(c.a(1));
    let r = transform4([v[0], v[1], v[2], 0.0], &rm(c.a(2)));
    wv(c.a(0), &r[..3]);
    c.a(0)
}
fn D3DXVec4Transform(c: &mut Ctx) -> u32 {
    wv(c.a(0), &transform4(rv(c.a(1)), &rm(c.a(2))));
    c.a(0)
}
fn D3DXVec2Transform(c: &mut Ctx) -> u32 {
    let v: [f32; 2] = rv(c.a(1));
    wv(c.a(0), &transform4([v[0], v[1], 0.0, 1.0], &rm(c.a(2))));
    c.a(0)
}
fn D3DXVec2TransformCoord(c: &mut Ctx) -> u32 {
    let v: [f32; 2] = rv(c.a(1));
    wv(c.a(0), &coord([v[0], v[1], 0.0], &rm(c.a(2)))[..2]);
    c.a(0)
}
/// The D3DXVec*Transform*Array forms (out, out stride, in, in stride, matrix, n): `f` maps one
/// N-float input to the output floats, written at each output stride (bytes between them
/// untouched, as d3dx9); returns out.
fn transform_array<const N: usize>(c: &mut Ctx, f: impl Fn([f32; N], &M) -> Vec<f32>) -> u32 {
    let (out, os, inp, is, n) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(5));
    let m = rm(c.a(4));
    for i in 0..n {
        let v: [f32; N] = rv(inp.wrapping_add(i.wrapping_mul(is)));
        wv(out.wrapping_add(i.wrapping_mul(os)), &f(v, &m));
    }
    out
}
fn D3DXVec3TransformCoordArray(c: &mut Ctx) -> u32 {
    transform_array::<3>(c, |v, m| coord(v, m).to_vec())
}
fn D3DXVec3TransformNormalArray(c: &mut Ctx) -> u32 {
    transform_array::<3>(c, |v, m| transform4([v[0], v[1], v[2], 0.0], m)[..3].to_vec())
}
fn D3DXVec3TransformArray(c: &mut Ctx) -> u32 {
    transform_array::<3>(c, |v, m| transform4([v[0], v[1], v[2], 1.0], m).to_vec())
}
fn D3DXVec2TransformCoordArray(c: &mut Ctx) -> u32 {
    transform_array::<2>(c, |v, m| coord([v[0], v[1], 0.0], m)[..2].to_vec())
}
fn D3DXVec2TransformNormal(c: &mut Ctx) -> u32 {
    let v: [f32; 2] = rv(c.a(1));
    wv(c.a(0), &transform4([v[0], v[1], 0.0, 0.0], &rm(c.a(2)))[..2]);
    c.a(0)
}
fn D3DXVec2TransformNormalArray(c: &mut Ctx) -> u32 {
    transform_array::<2>(c, |v, m| transform4([v[0], v[1], 0.0, 0.0], m)[..2].to_vec())
}
fn D3DXVec2TransformArray(c: &mut Ctx) -> u32 {
    transform_array::<2>(c, |v, m| transform4([v[0], v[1], 0.0, 1.0], m).to_vec())
}
fn D3DXVec4TransformArray(c: &mut Ctx) -> u32 {
    transform_array::<4>(c, |v, m| transform4(v, m).to_vec())
}
fn world_view_proj(proj: u32, view: u32, world: u32) -> M {
    let mut m = ID;
    for p in [world, view, proj] {
        if p != 0 {
            m = mul(&m, &rm(p));
        }
    }
    m
}
/// D3DXVec3Project(out, v, viewport, proj, view, world)
fn D3DXVec3Project(c: &mut Ctx) -> u32 {
    let m = world_view_proj(c.a(3), c.a(4), c.a(5));
    let mut v = coord(rv(c.a(1)), &m);
    let vp = c.a(2);
    if vp != 0 {
        let (x, y, w, h) = (r32(vp) as f32, r32(vp + 4) as f32, r32(vp + 8) as f32, r32(vp + 12) as f32);
        let (zn, zf) = (rf(vp, 4), rf(vp, 5));
        v = [x + (1.0 + v[0]) * w / 2.0, y + (1.0 - v[1]) * h / 2.0, zn + v[2] * (zf - zn)];
    }
    wv(c.a(0), &v);
    c.a(0)
}
/// D3DXVec3Unproject(out, v, viewport, proj, view, world)
fn D3DXVec3Unproject(c: &mut Ctx) -> u32 {
    let m = world_view_proj(c.a(3), c.a(4), c.a(5));
    let Some((inv, _)) = inverse(&m) else { return 0 };
    let mut v: [f32; 3] = rv(c.a(1));
    let vp = c.a(2);
    if vp != 0 {
        let (x, y, w, h) = (r32(vp) as f32, r32(vp + 4) as f32, r32(vp + 8) as f32, r32(vp + 12) as f32);
        let (zn, zf) = (rf(vp, 4), rf(vp, 5));
        v = [2.0 * (v[0] - x) / w - 1.0, 1.0 - 2.0 * (v[1] - y) / h, (v[2] - zn) / (zf - zn)];
    }
    wv(c.a(0), &coord(v, &inv));
    c.a(0)
}
fn D3DXVec3Length(c: &mut Ctx) -> u32 {
    let v: [f32; 3] = rv(c.a(0));
    c.st0 = Some(dot3(v, v).sqrt() as f64);
    0
}
fn D3DXVec3Cross(c: &mut Ctx) -> u32 {
    wv(c.a(0), &cross(rv(c.a(1)), rv(c.a(2))));
    c.a(0)
}
fn D3DXMatrixIdentity(c: &mut Ctx) -> u32 {
    out_m(c, ID)
}
fn D3DXMatrixIsIdentity(c: &mut Ctx) -> u32 {
    (rm(c.a(0)) == ID) as u32
}

// ---- ID3DXBuffer ----

fn buf_pointer(c: &mut Ctx) -> u32 {
    r32(c.a(0) + 8)
}
fn buf_size(c: &mut Ctx) -> u32 {
    r32(c.a(0) + 12)
}
pub(crate) fn qi_no(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), 0);
    0x8000_4002
}
pub(crate) fn add_ref(c: &mut Ctx) -> u32 {
    let n = r32(c.a(0) + 4) + 1;
    w32(c.a(0) + 4, n);
    n
}
pub(crate) fn release(c: &mut Ctx) -> u32 {
    let n = r32(c.a(0) + 4).saturating_sub(1);
    w32(c.a(0) + 4, n);
    n
}

/// A new ID3DXBuffer holding `data`.
pub fn buffer(data: &[u8]) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| vtable("ID3DXBuffer", &[(qi_no, 12), (add_ref, 4), (release, 4), (buf_pointer, 4), (buf_size, 4)]));
    let o = mem::heap_alloc(16, true);
    let p = mem::heap_alloc((data.len() as u32).max(4), true);
    mem::write(p, data);
    w32(o, vt);
    w32(o + 4, 1);
    w32(o + 8, p);
    w32(o + 12, data.len() as u32);
    o
}

/// A 32-bit BMP file of top-down BGRA rows.
pub(crate) fn bmp32(w: u32, h: u32, px: &[u8]) -> Vec<u8> {
    let size = 54 + w * h * 4;
    let mut f = Vec::with_capacity(size as usize);
    f.extend_from_slice(b"BM");
    for v in [size, 0, 54, 40, w] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    // (a negative height: top-down)
    f.extend_from_slice(&(-(h as i32)).to_le_bytes());
    f.extend_from_slice(&1u16.to_le_bytes());
    f.extend_from_slice(&32u16.to_le_bytes());
    for v in [0u32, w * h * 4, 2835, 2835, 0, 0] {
        f.extend_from_slice(&v.to_le_bytes());
    }
    f.extend_from_slice(&px[..(w * h * 4) as usize]);
    f
}

/// D3DXSave{Texture,Surface}ToFileInMemory(out buffer, format, source, palette): a BMP (any
/// format asked: the games read it back with D3DXCreateTextureFromFileInMemory, which takes
/// any), black when the source can't be read.
fn save_in_memory(c: &mut Ctx, surface: bool) -> u32 {
    let (out, src) = (c.a(0), c.a(2));
    let (w, h, px) = crate::d3d9::read_bgra(src, surface).unwrap_or((1, 1, vec![0, 0, 0, 255]));
    mem::putp(out, buffer(&bmp32(w, h, &px)));
    0
}
fn D3DXSaveTextureToFileInMemory(c: &mut Ctx) -> u32 {
    save_in_memory(c, false)
}
fn D3DXSaveSurfaceToFileInMemory(c: &mut Ctx) -> u32 {
    save_in_memory(c, true)
}

fn D3DXCreateBuffer(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), buffer(&vec![0; c.a(0) as usize]));
    0
}

/// D3DXCreateEffect(device, data, length, defines, include, flags, pool, effect, errors) and the
/// Ex form (.., include, skip constants, flags, ..): a compiled effect (crate::d3dx9fx) or effect
/// source, compiled to fx_2_0 first (hlslc::compile_effect)
#[allow(clippy::too_many_arguments)]
fn effect_from(c: &mut Ctx, dev: u32, data: &[u8], name: &str, defs: u32, inc: u32, flags: u32, out: u32, errs: u32) -> u32 {
    if errs != 0 {
        mem::putp(errs, 0);
    }
    let compiled;
    let data = if data.len() >= 4 && data[..4] == [1, 9, 0xff, 0xfe] {
        Ok(data)
    } else {
        let src = String::from_utf8_lossy(data).into_owned();
        let defines = crate::d3dcompiler::defines(defs);
        let base = crate::d3dcompiler::dir_of(name);
        let mut include = |n: &str, sys: bool| crate::d3dcompiler::open_include(c, inc, &base, n, sys);
        compiled = hlslc::compile_effect(src.trim_end_matches('\0'), &defines, &mut include, flags & hlslc::PACK_MATRIX_ROW_MAJOR);
        match &compiled {
            Ok(b) => Ok(&b[..]),
            Err(e) => Err(format!("{}: error X3000: {e}", if name.is_empty() { "memory" } else { name })),
        }
    };
    match data.and_then(|d| crate::d3dx9fx::create(c, dev, d)) {
        Ok(e) => {
            mem::putp(out, e);
            0
        }
        Err(m) => {
            eprintln!("sake: D3DXCreateEffect: {m}");
            if errs != 0 {
                mem::putp(errs, buffer(format!("{m}\0").as_bytes()));
            }
            mem::putp(out, 0);
            0x88760b59
        }
    }
}
fn D3DXCreateEffect(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(1), c.a(2)).to_vec();
    let (dev, defs, inc, flags, out, errs) = (c.a(0), c.a(3), c.a(4), c.a(5), c.a(7), c.a(8));
    effect_from(c, dev, &data, "", defs, inc, flags, out, errs)
}
fn D3DXCreateEffectEx(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(1), c.a(2)).to_vec();
    let (dev, defs, inc, flags, out, errs) = (c.a(0), c.a(3), c.a(4), c.a(6), c.a(8), c.a(9));
    effect_from(c, dev, &data, "", defs, inc, flags, out, errs)
}
/// D3DXCreateEffectFromFile{A,W}(device, file, defines, include, flags, pool, effect, errors)
/// and the Ex forms (.., include, skip, flags, pool, effect, errors)
fn effect_file(c: &mut Ctx, wide: bool, ex: bool) -> u32 {
    let name = crate::k32::arg_str(c.a(1), wide);
    let (flags, out, errs) = if ex { (c.a(5), c.a(7), c.a(8)) } else { (c.a(4), c.a(6), c.a(7)) };
    let Some(data) = crate::d3dcompiler::read_file(&name) else {
        mem::putp(out, 0);
        return 0x88760b59;
    };
    let (dev, defs, inc) = (c.a(0), c.a(2), c.a(3));
    effect_from(c, dev, &data, &name, defs, inc, flags, out, errs)
}
fn D3DXCreateEffectFromFileA(c: &mut Ctx) -> u32 {
    effect_file(c, false, false)
}
fn D3DXCreateEffectFromFileW(c: &mut Ctx) -> u32 {
    effect_file(c, true, false)
}
fn D3DXCreateEffectFromFileExA(c: &mut Ctx) -> u32 {
    effect_file(c, false, true)
}
fn D3DXCreateEffectFromFileExW(c: &mut Ctx) -> u32 {
    effect_file(c, true, true)
}

/// D3DXCreateEffectFromResource{A,W}(device, module, resource, defines, include, flags, pool,
/// effect, errors) and the Ex forms (.., include, skip, flags, pool, effect, errors): the
/// RT_RCDATA resource of that name in the module (0: the exe), as d3dx9 looks it up
fn effect_resource(c: &mut Ctx, wide: bool, ex: bool) -> u32 {
    let (flags, out, errs) = if ex { (c.a(6), c.a(8), c.a(9)) } else { (c.a(5), c.a(7), c.a(8)) };
    let module = if c.a(1) == 0 { crate::k32::module_handle(0, false) } else { c.a(1) };
    let name = crate::pe::ResId::from_arg(c.a(2), wide);
    let Some(e) = crate::pe::find_resource(module, &crate::pe::ResId::Int(10), &name) else {
        mem::putp(out, 0);
        return 0x8876_086c; // D3DERR_INVALIDCALL
    };
    let data = mem::slice(module.wrapping_add(r32(e)), r32(e + 4)).to_vec();
    let (dev, defs, inc) = (c.a(0), c.a(3), c.a(4));
    effect_from(c, dev, &data, "", defs, inc, flags, out, errs)
}
fn D3DXCreateEffectFromResourceA(c: &mut Ctx) -> u32 {
    effect_resource(c, false, false)
}
fn D3DXCreateEffectFromResourceW(c: &mut Ctx) -> u32 {
    effect_resource(c, true, false)
}
fn D3DXCreateEffectFromResourceExA(c: &mut Ctx) -> u32 {
    effect_resource(c, false, true)
}
fn D3DXCreateEffectFromResourceExW(c: &mut Ctx) -> u32 {
    effect_resource(c, true, true)
}

// ---- ID3DXMatrixStack: the top at object + 16 (GetTop's pointer), the rest here ----

static STACKS: std::sync::Mutex<Option<crate::HashMap<u32, Vec<M>>>> = std::sync::Mutex::new(None);

fn axis_rot(v: [f32; 3], a: f32) -> M {
    let [x, y, z] = norm3(v);
    let (s, co) = a.sin_cos();
    let t = 1.0 - co;
    [t * x * x + co, t * x * y + s * z, t * x * z - s * y, 0.0,
     t * x * y - s * z, t * y * y + co, t * y * z + s * x, 0.0,
     t * x * z + s * y, t * y * z - s * x, t * z * z + co, 0.0,
     0.0, 0.0, 0.0, 1.0]
}
fn ms_apply(c: &Ctx, m: M, local: bool) -> u32 {
    let o = c.a(0);
    let top = rm(o + 16);
    wm(o + 16, &if local { mul(&m, &top) } else { mul(&top, &m) });
    0
}
fn ms_pop(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let m = STACKS.lock().unwrap().get_or_insert_with(Default::default).get_mut(&o).and_then(|v| v.pop());
    if let Some(m) = m {
        wm(o + 16, &m);
    }
    0
}
fn ms_push(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    STACKS.lock().unwrap().get_or_insert_with(Default::default).entry(o).or_default().push(rm(o + 16));
    0
}
fn ms_identity(c: &mut Ctx) -> u32 {
    wm(c.a(0) + 16, &ID);
    0
}
fn ms_load(c: &mut Ctx) -> u32 {
    wm(c.a(0) + 16, &rm(c.a(1)));
    0
}
fn ms_mult(c: &mut Ctx) -> u32 {
    ms_apply(c, rm(c.a(1)), false)
}
fn ms_mult_local(c: &mut Ctx) -> u32 {
    ms_apply(c, rm(c.a(1)), true)
}
fn ms_rotate_axis(c: &mut Ctx) -> u32 {
    ms_apply(c, axis_rot(rv(c.a(1)), fl(c, 2)), false)
}
fn ms_rotate_axis_local(c: &mut Ctx) -> u32 {
    ms_apply(c, axis_rot(rv(c.a(1)), fl(c, 2)), true)
}
fn ypr(c: &Ctx) -> M {
    mul(&mul(&rot(2, fl(c, 3)), &rot(0, fl(c, 2))), &rot(1, fl(c, 1)))
}
fn ms_ypr(c: &mut Ctx) -> u32 {
    ms_apply(c, ypr(c), false)
}
fn ms_ypr_local(c: &mut Ctx) -> u32 {
    ms_apply(c, ypr(c), true)
}
fn scaling(c: &Ctx) -> M {
    let mut m = ID;
    (m[0], m[5], m[10]) = (fl(c, 1), fl(c, 2), fl(c, 3));
    m
}
fn translation(c: &Ctx) -> M {
    let mut m = ID;
    (m[12], m[13], m[14]) = (fl(c, 1), fl(c, 2), fl(c, 3));
    m
}
fn ms_scale(c: &mut Ctx) -> u32 {
    ms_apply(c, scaling(c), false)
}
fn ms_scale_local(c: &mut Ctx) -> u32 {
    ms_apply(c, scaling(c), true)
}
fn ms_translate(c: &mut Ctx) -> u32 {
    ms_apply(c, translation(c), false)
}
fn ms_translate_local(c: &mut Ctx) -> u32 {
    ms_apply(c, translation(c), true)
}
fn ms_top(c: &mut Ctx) -> u32 {
    c.a(0) + 16
}
/// D3DXIMAGE_INFO of a decoded image: (width, height, file format)
fn image_info(data: &[u8]) -> Option<(u32, u32, Vec<u8>, u32)> {
    let ff = if data.starts_with(b"\x89PNG") { 3 } else if data.starts_with(b"BM") { 0 } else { 1 };
    let (w, h, px) = crate::ole::decode(data)?;
    Some((w, h, px, ff))
}
fn write_info(p: u32, w: u32, h: u32, ff: u32) {
    if p != 0 {
        for (k, v) in [w, h, 1, 1, 21, 3, ff].iter().enumerate() {
            w32(p + 4 * k as u32, *v);
        }
    }
}
/// D3DXCheckTextureRequirements's sizes: D3DX_DEFAULT (and 0) -> 256, else as asked
/// (sake's devices take any size); levels D3DX_DEFAULT -> a full chain (0)
fn tex_req(w: u32, h: u32, mips: u32) -> (u32, u32, u32) {
    let size = |v: u32| if v == 0 || v >= 0xffff_fffd { 256 } else { v };
    (size(w), size(h), if mips == 0xffff_ffff { 0 } else { mips })
}
/// D3DXCreateTexture(device, width, height, mips, usage, format, pool, &texture)
fn D3DXCreateTexture(c: &mut Ctx) -> u32 {
    let (w, h, mips) = tex_req(c.a(1), c.a(2), c.a(3));
    let fmt = if c.a(5) == 0 { 21 } else { c.a(5) };
    let (dev, usage, pool, out) = (c.a(0), c.a(4), c.a(6), c.a(7));
    com_call(c, dev, 23, &[w, h, mips, usage, fmt, pool, out, 0])
}
/// D3DXCheckTextureRequirements(device, &width, &height, &mips, usage, &format, pool)
fn D3DXCheckTextureRequirements(c: &mut Ctx) -> u32 {
    let (pw, ph, pm, pf) = (c.a(1), c.a(2), c.a(3), c.a(5));
    let rd = |p: u32, d: u32| if p == 0 { d } else { r32(p) };
    let (w, h, m) = tex_req(rd(pw, 0), rd(ph, 0), rd(pm, 0xffff_ffff));
    for (p, v) in [(pw, w), (ph, h), (pm, if m == 0 { 1 + 31 - w.max(h).leading_zeros() } else { m })] {
        if p != 0 {
            w32(p, v);
        }
    }
    if pf != 0 && (r32(pf) == 0 || r32(pf) == 0xffff_fffd) {
        w32(pf, 21);
    }
    0
}
/// D3DXCreateTextureFromFileInMemoryEx(device, data, length, width, height, mips, usage, format,
/// pool, filter, mip filter, color key, &info, palette, &texture): PNG, JPEG, BMP
fn texture_from(c: &mut Ctx, dev: u32, data: &[u8], a: [u32; 12]) -> u32 {
    let [w, h, mips, usage, fmt, pool, _filter, _mipfilter, key, info, _pal, out] = a;
    mem::putp(out, 0);
    let Some((iw, ih, mut px, ff)) = image_info(data) else {
        eprintln!("sake: D3DXCreateTexture: image not decoded ({:02x?})", &data[..data.len().min(8)]);
        return 0x88760b59;
    };
    write_info(info, iw, ih, ff);
    // (D3DX_DEFAULT: the next power of two; 0, D3DX_DEFAULT_NONPOW2, D3DX_FROM_FILE: the image's)
    let size = |v: u32, i: u32| match v {
        0xffff_ffff => i.next_power_of_two(),
        0 | 0xffff_fffe | 0xffff_fffd => i,
        v => v,
    };
    let (tw, th) = (size(w, iw), size(h, ih));
    let fmt = if fmt == 0 || fmt == 0xffff_fffd { 21 } else { fmt };
    let levels = match mips {
        0xffff_fffd | 0xffff_fffe => 1,
        v => v,
    };
    if key != 0 {
        for p in px.chunks_mut(4) {
            let argb = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
            if argb == key {
                p.copy_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    let pt = mem::heap_alloc(8, true);
    let hr = com_call(c, dev, 23, &[tw, th, if levels == 0xffff_ffff { 0 } else { levels }, usage, fmt, pool, pt, 0]);
    let tex = r32(pt);
    mem::heap_free(pt);
    if hr != 0 || tex == 0 {
        return if hr != 0 { hr } else { 0x8876086c };
    }
    // every level: the image scaled to it (nearest), packed
    let n = com_call(c, tex, 13, &[]).max(1);
    let desc = mem::heap_alloc(32, true);
    let lr = mem::heap_alloc(8, true);
    for level in 0..n {
        com_call(c, tex, 17, &[level, desc]);
        let (lw, lh) = (r32(desc + 24), r32(desc + 28));
        if com_call(c, tex, 19, &[level, lr, 0, 0]) != 0 {
            continue;
        }
        let (pitch, bits) = (r32(lr), r32(lr + 4));
        for y in 0..lh {
            let sy = (y as u64 * ih as u64 / lh.max(1) as u64) as u32;
            for x in 0..lw {
                let sx = (x as u64 * iw as u64 / lw.max(1) as u64) as u32;
                let i = 4 * (sy * iw + sx) as usize;
                let (b, g, r, a) = (px[i], px[i + 1], px[i + 2], px[i + 3]);
                let f = |v: u8| v as f32 / 255.0;
                if let Some((v, bpp)) = pack(fmt, [f(r), f(g), f(b), f(a)]) {
                    let at = bits + y * pitch + x * bpp;
                    match bpp {
                        4 => w32(at, v),
                        2 => mem::w16(at, v as u16),
                        _ => mem::w8(at, v as u8),
                    }
                }
            }
        }
        com_call(c, tex, 20, &[level]);
    }
    mem::heap_free(desc);
    mem::heap_free(lr);
    mem::putp(out, tex);
    0
}
fn D3DXCreateTextureFromFileInMemoryEx(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(1), c.a(2)).to_vec();
    let a = std::array::from_fn(|k| c.a(3 + k as u32));
    let dev = c.a(0);
    texture_from(c, dev, &data, a)
}
fn D3DXCreateTextureFromFileInMemory(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(1), c.a(2)).to_vec();
    let (dev, out) = (c.a(0), c.a(3));
    texture_from(c, dev, &data, [u32::MAX, u32::MAX, u32::MAX, 0, 0, 1, 0, 0, 0, 0, 0, out])
}
/// D3DXCreateTextureFromFile{A,W}(device, file, &texture) and the Ex forms (device, file, then
/// the in-memory Ex arguments)
fn texture_file(c: &mut Ctx, wide: bool, ex: bool) -> u32 {
    let name = crate::k32::arg_str(c.a(1), wide);
    let Some(data) = crate::files::resolve(&name).and_then(|h| kura::fs::read(h).ok()) else {
        return 0x88760b59;
    };
    let dev = c.a(0);
    let a = if ex { std::array::from_fn(|k| c.a(2 + k as u32)) } else { [u32::MAX, u32::MAX, u32::MAX, 0, 0, 1, 0, 0, 0, 0, 0, c.a(2)] };
    texture_from(c, dev, &data, a)
}
fn D3DXCreateTextureFromFileA(c: &mut Ctx) -> u32 {
    texture_file(c, false, false)
}
fn D3DXCreateTextureFromFileW(c: &mut Ctx) -> u32 {
    texture_file(c, true, false)
}
fn D3DXCreateTextureFromFileExA(c: &mut Ctx) -> u32 {
    texture_file(c, false, true)
}
fn D3DXCreateTextureFromFileExW(c: &mut Ctx) -> u32 {
    texture_file(c, true, true)
}
/// A RECT* as (left, top, right, bottom), or the whole `w` x `h` for NULL.
fn rect_or(p: u32, w: u32, h: u32) -> (u32, u32, u32, u32) {
    if p == 0 {
        return (0, 0, w, h);
    }
    let g = |k: u32| (r32(p + 4 * k) as i32).max(0) as u32;
    (g(0).min(w), g(1).min(h), g(2).min(w), g(3).min(h))
}
/// D3DXLoadSurfaceFrom* for an image file: its `src` rectangle into the surface's `dst`
/// rectangle (scaled, nearest), color key pixels transparent.
#[allow(clippy::too_many_arguments)]
fn surface_from(c: &mut Ctx, surf: u32, dst: u32, data: &[u8], src: u32, key: u32, info: u32) -> u32 {
    let Some((iw, ih, px, ff)) = image_info(data) else {
        eprintln!("sake: D3DXLoadSurface: image not decoded ({:02x?})", &data[..data.len().min(8)]);
        return 0x88760b59;
    };
    write_info(info, iw, ih, ff);
    let desc = mem::heap_alloc(32, true);
    com_call(c, surf, 12, &[desc]);
    let (fmt, sw, sh) = (r32(desc), r32(desc + 24), r32(desc + 28));
    mem::heap_free(desc);
    let (sl, st, sr, sb) = rect_or(src, iw, ih);
    let (dl, dt, dr, db) = rect_or(dst, sw, sh);
    if sr <= sl || sb <= st || dr <= dl || db <= dt {
        return 0;
    }
    let lr = mem::heap_alloc(8, true);
    let hr = com_call(c, surf, 13, &[lr, dst, 0]);
    if hr != 0 {
        mem::heap_free(lr);
        return hr;
    }
    let (pitch, bits) = (r32(lr), r32(lr + 4));
    mem::heap_free(lr);
    let (dw, dh) = (dr - dl, db - dt);
    for y in 0..dh {
        let sy = st + (y as u64 * (sb - st) as u64 / dh as u64) as u32;
        for x in 0..dw {
            let sx = sl + (x as u64 * (sr - sl) as u64 / dw as u64) as u32;
            let i = 4 * (sy * iw + sx) as usize;
            let (b, g, r, mut a) = (px[i], px[i + 1], px[i + 2], px[i + 3]);
            if key != 0 && u32::from_le_bytes([b, g, r, a]) == key {
                a = 0;
            }
            let f = |v: u8| v as f32 / 255.0;
            let (r, g, b) = if a == 0 && key != 0 { (0, 0, 0) } else { (r, g, b) };
            if let Some((v, bpp)) = pack(fmt, [f(r), f(g), f(b), f(a)]) {
                // (the locked bits start at the rectangle's corner)
                let at = bits + y * pitch + x * bpp;
                match bpp {
                    4 => w32(at, v),
                    2 => mem::w16(at, v as u16),
                    _ => mem::w8(at, v as u8),
                }
            }
        }
    }
    com_call(c, surf, 14, &[]);
    0
}
/// D3DXLoadSurfaceFromFileInMemory(surface, palette, dst rect, data, length, src rect, filter,
/// color key, &info)
fn D3DXLoadSurfaceFromFileInMemory(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(3), c.a(4)).to_vec();
    let (surf, dst, src, key, info) = (c.a(0), c.a(2), c.a(5), c.a(7), c.a(8));
    surface_from(c, surf, dst, &data, src, key, info)
}
/// D3DXLoadSurfaceFromFile{A,W}(surface, palette, dst rect, file, src rect, filter, color key, &info)
fn surface_file(c: &mut Ctx, wide: bool) -> u32 {
    let name = crate::k32::arg_str(c.a(3), wide);
    let Some(data) = crate::files::resolve(&name).and_then(|h| kura::fs::read(h).ok()) else {
        return 0x88760b59;
    };
    let (surf, dst, src, key, info) = (c.a(0), c.a(2), c.a(4), c.a(6), c.a(7));
    surface_from(c, surf, dst, &data, src, key, info)
}
fn D3DXLoadSurfaceFromFileA(c: &mut Ctx) -> u32 {
    surface_file(c, false)
}
fn D3DXLoadSurfaceFromFileW(c: &mut Ctx) -> u32 {
    surface_file(c, true)
}
/// D3DXGetImageInfoFromFile{A,W}(file, &info)
fn image_info_file(c: &mut Ctx, wide: bool) -> u32 {
    let name = crate::k32::arg_str(c.a(0), wide);
    let Some(data) = crate::files::resolve(&name).and_then(|h| kura::fs::read(h).ok()) else {
        return 0x88760b59;
    };
    match image_info(&data) {
        Some((w, h, _, ff)) => {
            write_info(c.a(1), w, h, ff);
            0
        }
        None => 0x88760b59,
    }
}
fn D3DXGetImageInfoFromFileA(c: &mut Ctx) -> u32 {
    image_info_file(c, false)
}
fn D3DXGetImageInfoFromFileW(c: &mut Ctx) -> u32 {
    image_info_file(c, true)
}
/// D3DXGetImageInfoFromFileInMemory(data, length, &info)
fn D3DXGetImageInfoFromFileInMemory(c: &mut Ctx) -> u32 {
    let data = mem::slice(c.a(0), c.a(1)).to_vec();
    match image_info(&data) {
        Some((w, h, _, ff)) => {
            write_info(c.a(2), w, h, ff);
            0
        }
        None => 0x88760b59,
    }
}
/// D3DXCreateMatrixStack(flags, &stack)
fn D3DXCreateMatrixStack(c: &mut Ctx) -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        vtable("ID3DXMatrixStack", &[(qi_no, 12), (add_ref, 4), (release, 4), (ms_pop, 4), (ms_push, 4), (ms_identity, 4),
            (ms_load, 8), (ms_mult, 8), (ms_mult_local, 8), (ms_rotate_axis, 12), (ms_rotate_axis_local, 12),
            (ms_ypr, 16), (ms_ypr_local, 16), (ms_scale, 16), (ms_scale_local, 16), (ms_translate, 16),
            (ms_translate_local, 16), (ms_top, 4)])
    });
    let o = mem::heap_alloc(16 + 64, true);
    w32(o, vt);
    w32(o + 4, 1);
    wm(o + 16, &ID);
    mem::putp(c.a(1), o);
    0
}

// ---- the shader assembler ----

fn D3DXAssembleShader(c: &mut Ctx) -> u32 {
    // (data, length, defines, include, flags, shader, errors)
    let src = String::from_utf8_lossy(&mem::cstrn(c.a(0), c.a(1))).into_owned();
    let (out, errs) = (c.a(5), c.a(6));
    match hlslc::d3d9asm::assemble(&src) {
        Ok(words) => {
            let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
            if out != 0 {
                mem::putp(out, buffer(&bytes));
            }
            if errs != 0 {
                mem::putp(errs, 0);
            }
            0
        }
        Err(e) => {
            eprintln!("sake: D3DXAssembleShader: {e}\n{src}");
            if out != 0 {
                mem::putp(out, 0);
            }
            if errs != 0 {
                let mut b = e.into_bytes();
                b.push(0);
                mem::putp(errs, buffer(&b));
            }
            0x8876_086c // D3DXERR_INVALIDDATA
        }
    }
}

// ---- D3DXFillTexture ----

/// Calls a COM method of a guest object (slot `m`).
pub(crate) fn com_call(c: &mut Ctx, obj: u32, m: u32, args: &[u32]) -> u32 {
    let f = mem::rp(mem::rp(obj) + mem::ps() * m);
    let mut a = vec![obj];
    a.extend_from_slice(args);
    c.call(f, &a)
}

/// Packs straight RGBA floats into a texel of D3DFORMAT `fmt`; None for formats not handled.
fn pack(fmt: u32, v: [f32; 4]) -> Option<(u32, u32)> {
    let q = |x: f32, bits: u32| ((x.clamp(0.0, 1.0) * ((1u32 << bits) - 1) as f32) + 0.5) as u32;
    let [r, g, b, a] = v;
    Some(match fmt {
        21 | 22 => (q(a, 8) << 24 | q(r, 8) << 16 | q(g, 8) << 8 | q(b, 8), 4),
        32 => (q(a, 8) << 24 | q(b, 8) << 16 | q(g, 8) << 8 | q(r, 8), 4),
        23 => (q(r, 5) << 11 | q(g, 6) << 5 | q(b, 5), 2),
        24 => (q(r, 5) << 10 | q(g, 5) << 5 | q(b, 5), 2),
        25 => (q(a, 1) << 15 | q(r, 5) << 10 | q(g, 5) << 5 | q(b, 5), 2),
        26 => (q(a, 4) << 12 | q(r, 4) << 8 | q(g, 4) << 4 | q(b, 4), 2),
        28 => (q(a, 8), 1),
        50 => (q(r, 8), 1),
        51 => (q(a, 8) << 8 | q(r, 8), 2),
        _ => return None,
    })
}

/// D3DXFillTexture(texture, function, data): function(out, texcoord, texel size, data) for
/// every texel of every level.
fn D3DXFillTexture(c: &mut Ctx) -> u32 {
    let (tex, f, data) = (c.a(0), c.a(1), c.a(2));
    let levels = com_call(c, tex, 13, &[]);
    let scratch = mem::heap_alloc(64, true);
    let (out, tc, ts, desc, lr) = (scratch, scratch + 16, scratch + 24, scratch + 32, scratch + 32 + 32);
    let lr_mem = mem::heap_alloc(8, true);
    for level in 0..levels.max(1) {
        if com_call(c, tex, 17, &[level, desc]) != 0 {
            break;
        }
        let (fmt, w, h) = (r32(desc), r32(desc + 24), r32(desc + 28));
        if pack(fmt, [0.0; 4]).is_none() {
            eprintln!("sake: D3DXFillTexture: format {fmt} not handled");
            break;
        }
        if com_call(c, tex, 19, &[level, lr_mem, 0, 0]) != 0 {
            break;
        }
        let (pitch, bits) = (r32(lr_mem), r32(lr_mem + 4));
        wv(ts, &[1.0 / w as f32, 1.0 / h as f32]);
        for y in 0..h {
            for x in 0..w {
                wv(tc, &[(x as f32 + 0.5) / w as f32, (y as f32 + 0.5) / h as f32]);
                wv(out, &[0.0; 4]);
                c.call(f, &[out, tc, ts, data]);
                let (v, n) = pack(fmt, rv(out)).unwrap_or((0, 4));
                let p = bits + y * pitch + x * n;
                match n {
                    4 => w32(p, v),
                    2 => mem::w16(p, v as u16),
                    _ => mem::w8(p, v as u8),
                }
            }
        }
        com_call(c, tex, 20, &[level]);
    }
    let _ = lr;
    mem::heap_free(lr_mem);
    mem::heap_free(scratch);
    0
}

// ---- ID3DXFont (created; drawing is logged) ----

fn font_draw(c: &mut Ctx) -> u32 {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| eprintln!("sake: ID3DXFont::DrawText not implemented"));
    let _ = c;
    0
}
pub(crate) fn zero(_: &mut Ctx) -> u32 {
    0
}
fn font_device(c: &mut Ctx) -> u32 {
    let d = r32(c.a(0) + 8);
    if d != 0 {
        com_call(c, d, 1, &[]);
    }
    mem::putp(c.a(1), d);
    0
}

fn D3DXCreateFont(c: &mut Ctx) -> u32 {
    // D3DXCreateFontA/W(device, height, width, weight, mips, italic, charset, precision,
    // quality, pitch, face, out); the Indirect forms (device, desc, out)
    let n = crate::api::name_of(c.trap).unwrap_or_default();
    let (dev, out) = if n.contains("Indirect") { (c.a(0), c.a(2)) } else { (c.a(0), c.a(11)) };
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        let mut m: Vec<(Handler, u32)> = vec![(qi_no, 12), (add_ref, 4), (release, 4), (font_device, 8), (zero, 8), (zero, 8), (zero, 8), (zero, 8), (zero, 4)];
        m.extend_from_slice(&[(zero, 20), (zero, 12), (zero, 12), (zero, 12), (zero, 12), (font_draw, 28), (font_draw, 28), (zero, 4), (zero, 4)]);
        vtable("ID3DXFont", &m)
    });
    let o = mem::heap_alloc(16, true);
    w32(o, vt);
    w32(o + 4, 1);
    w32(o + 8, dev);
    if dev != 0 {
        com_call(c, dev, 1, &[]);
    }
    mem::putp(out, o);
    0
}

/// D3DXCheckVersion(D3D_SDK_VERSION, D3DX_SDK_VERSION): every version is ours.
fn D3DXCheckVersion(_: &mut Ctx) -> u32 {
    1
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("D3DXCreateEffect", D3DXCreateEffect),
        ("D3DXSaveTextureToFileInMemory", D3DXSaveTextureToFileInMemory),
        ("D3DXSaveSurfaceToFileInMemory", D3DXSaveSurfaceToFileInMemory),
        ("D3DXCreateMatrixStack", D3DXCreateMatrixStack),
        ("D3DXCreateTexture", D3DXCreateTexture),
        ("D3DXCheckTextureRequirements", D3DXCheckTextureRequirements),
        ("D3DXCreateTextureFromFileInMemoryEx", D3DXCreateTextureFromFileInMemoryEx),
        ("D3DXCreateTextureFromFileInMemory", D3DXCreateTextureFromFileInMemory),
        ("D3DXCreateTextureFromFileA", D3DXCreateTextureFromFileA),
        ("D3DXCreateTextureFromFileW", D3DXCreateTextureFromFileW),
        ("D3DXCreateTextureFromFileExA", D3DXCreateTextureFromFileExA),
        ("D3DXCreateTextureFromFileExW", D3DXCreateTextureFromFileExW),
        ("D3DXGetImageInfoFromFileInMemory", D3DXGetImageInfoFromFileInMemory),
        ("D3DXGetImageInfoFromFileA", D3DXGetImageInfoFromFileA),
        ("D3DXGetImageInfoFromFileW", D3DXGetImageInfoFromFileW),
        ("D3DXCreateEffectEx", D3DXCreateEffectEx),
        ("D3DXCreateEffectFromFileA", D3DXCreateEffectFromFileA),
        ("D3DXCreateEffectFromFileW", D3DXCreateEffectFromFileW),
        ("D3DXCreateEffectFromFileExA", D3DXCreateEffectFromFileExA),
        ("D3DXCreateEffectFromFileExW", D3DXCreateEffectFromFileExW),
        ("D3DXMatrixMultiply", D3DXMatrixMultiply),
        ("D3DXMatrixMultiplyTranspose", D3DXMatrixMultiplyTranspose),
        ("D3DXMatrixTranspose", D3DXMatrixTranspose),
        ("D3DXMatrixInverse", D3DXMatrixInverse),
        ("D3DXMatrixDeterminant", D3DXMatrixDeterminant),
        ("D3DXMatrixTranslation", D3DXMatrixTranslation),
        ("D3DXMatrixScaling", D3DXMatrixScaling),
        ("D3DXMatrixRotationX", D3DXMatrixRotationX),
        ("D3DXMatrixRotationY", D3DXMatrixRotationY),
        ("D3DXMatrixRotationZ", D3DXMatrixRotationZ),
        ("D3DXMatrixRotationYawPitchRoll", D3DXMatrixRotationYawPitchRoll),
        ("D3DXMatrixLookAtLH", D3DXMatrixLookAtLH),
        ("D3DXMatrixLookAtRH", D3DXMatrixLookAtRH),
        ("D3DXMatrixPerspectiveFovLH", D3DXMatrixPerspectiveFovLH),
        ("D3DXMatrixPerspectiveFovRH", D3DXMatrixPerspectiveFovRH),
        ("D3DXMatrixPerspectiveLH", D3DXMatrixPerspectiveLH),
        ("D3DXMatrixOrthoLH", D3DXMatrixOrthoLH),
        ("D3DXMatrixOrthoRH", D3DXMatrixOrthoRH),
        ("D3DXMatrixOrthoOffCenterLH", D3DXMatrixOrthoOffCenterLH),
        ("D3DXMatrixOrthoOffCenterRH", D3DXMatrixOrthoOffCenterRH),
        ("D3DXMatrixTransformation2D", D3DXMatrixTransformation2D),
        ("D3DXVec2Normalize", D3DXVec2Normalize),
        ("D3DXVec3Normalize", D3DXVec3Normalize),
        ("D3DXVec4Normalize", D3DXVec4Normalize),
        ("D3DXVec2Transform", D3DXVec2Transform),
        ("D3DXVec2TransformCoord", D3DXVec2TransformCoord),
        ("D3DXVec3Transform", D3DXVec3Transform),
        ("D3DXVec3TransformCoord", D3DXVec3TransformCoord),
        ("D3DXVec3TransformNormal", D3DXVec3TransformNormal),
        ("D3DXVec4Transform", D3DXVec4Transform),
        ("D3DXVec3TransformCoordArray", D3DXVec3TransformCoordArray),
        ("D3DXVec3TransformNormalArray", D3DXVec3TransformNormalArray),
        ("D3DXVec3TransformArray", D3DXVec3TransformArray),
        ("D3DXVec2TransformCoordArray", D3DXVec2TransformCoordArray),
        ("D3DXVec2TransformNormal", D3DXVec2TransformNormal),
        ("D3DXVec2TransformNormalArray", D3DXVec2TransformNormalArray),
        ("D3DXVec2TransformArray", D3DXVec2TransformArray),
        ("D3DXVec4TransformArray", D3DXVec4TransformArray),
        ("D3DXCreateEffectFromResourceA", D3DXCreateEffectFromResourceA),
        ("D3DXCreateEffectFromResourceW", D3DXCreateEffectFromResourceW),
        ("D3DXCreateEffectFromResourceExA", D3DXCreateEffectFromResourceExA),
        ("D3DXCreateEffectFromResourceExW", D3DXCreateEffectFromResourceExW),
        ("D3DXVec3Project", D3DXVec3Project),
        ("D3DXVec3Unproject", D3DXVec3Unproject),
        ("D3DXVec3Length", D3DXVec3Length),
        ("D3DXVec3Cross", D3DXVec3Cross),
        ("D3DXMatrixIdentity", D3DXMatrixIdentity),
        ("D3DXMatrixIsIdentity", D3DXMatrixIsIdentity),
        ("D3DXCreateBuffer", D3DXCreateBuffer),
        ("D3DXAssembleShader", D3DXAssembleShader),
        ("D3DXFillTexture", D3DXFillTexture),
        ("D3DXCreateFontA", D3DXCreateFont),
        ("D3DXCreateFontW", D3DXCreateFont),
        ("D3DXCreateFontIndirectA", D3DXCreateFont),
        ("D3DXCreateFontIndirectW", D3DXCreateFont),
        ("D3DXCheckVersion", D3DXCheckVersion),
        ("D3DXCreateSprite", crate::d3dx9sprite::D3DXCreateSprite),
        ("D3DXLoadSurfaceFromFileInMemory", D3DXLoadSurfaceFromFileInMemory),
        ("D3DXLoadSurfaceFromFileA", D3DXLoadSurfaceFromFileA),
        ("D3DXLoadSurfaceFromFileW", D3DXLoadSurfaceFromFileW),
    ];
    for v in 24..=43 {
        let dll = format!("d3dx9_{v}");
        for (n, f) in fns {
            implement(&dll, n, *f);
        }
    }
}

#[cfg(test)]
mod save_tests {
    use super::bmp32;
    use crate::d3d9::widen_bgra;
    use proptest::prelude::*;

    proptest! {
        /// what D3DXSaveTextureToFileInMemory writes reads back the same (a BI_RGB BMP: opaque)
        #[test]
        fn bmp32_round_trip(w in 1u32..20, h in 1u32..20, seed in any::<u64>()) {
            let px: Vec<u8> = (0..w * h * 4).map(|i| (seed.wrapping_mul(i as u64 + 7) >> 13) as u8).collect();
            let (rw, rh, back) = crate::picture::decode_bgra(&bmp32(w, h, &px)).unwrap();
            prop_assert_eq!((rw, rh), (w, h));
            let opaque: Vec<u8> = px.chunks(4).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
            prop_assert_eq!(back, opaque);
        }
    }

    #[test]
    fn widen_formats() {
        // X8R8G8B8 opaque, R5G6B5 white and pure red, A1R5G5B5 transparent black
        assert_eq!(widen_bgra(&[1, 2, 3, 0], 22), [1, 2, 3, 255]);
        assert_eq!(widen_bgra(&[0xff, 0xff], 23), [255, 255, 255, 255]);
        assert_eq!(widen_bgra(&[0x00, 0xf8], 23), [0, 0, 255, 255]);
        assert_eq!(widen_bgra(&[0x00, 0x00], 25), [0, 0, 0, 0]);
        assert_eq!(widen_bgra(&[0x1f, 0x80], 25), [255, 0, 0, 255]);
    }
}

#[cfg(test)]
mod array_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    fn put(p: u32, v: &[f32]) {
        for (i, x) in v.iter().enumerate() {
            w32(p + 4 * i as u32, x.to_bits());
        }
    }
    fn get(p: u32, n: usize) -> Vec<u32> {
        (0..n as u32).map(|i| r32(p + 4 * i)).collect()
    }

    /// (array handler, single handler, input floats, output floats)
    const FORMS: &[(Handler, Handler, usize, usize)] = &[
        (D3DXVec3TransformCoordArray, D3DXVec3TransformCoord, 3, 3),
        (D3DXVec3TransformNormalArray, D3DXVec3TransformNormal, 3, 3),
        (D3DXVec3TransformArray, D3DXVec3Transform, 3, 4),
        (D3DXVec2TransformCoordArray, D3DXVec2TransformCoord, 2, 2),
        (D3DXVec2TransformNormalArray, D3DXVec2TransformNormal, 2, 2),
        (D3DXVec2TransformArray, D3DXVec2Transform, 2, 4),
        (D3DXVec4TransformArray, D3DXVec4Transform, 4, 4),
    ];

    proptest! {
        /// each *Array form gives, element by element, the bits of its single-vector form, writes
        /// only its output floats at each stride (gap bytes keep their contents) and returns out
        #[test]
        fn arrays_match_single(form in 0..FORMS.len(), n in 0u32..6, in_gap in 0u32..3, out_gap in 0u32..3,
                               m in prop::array::uniform16(-8.0f32..8.0), vals in prop::collection::vec(-100.0f32..100.0, 24)) {
            let _g = enter();
            let (arr, one, ni, no) = FORMS[form];
            let (is, os) = (4 * (ni as u32 + in_gap), 4 * (no as u32 + out_gap));
            let (mp, ip, op) = (mem::heap_alloc(64, true), mem::heap_alloc(is * 6 + 16, true), mem::heap_alloc(os * 6 + 16, true));
            let (single_in, single_out) = (mem::heap_alloc(16, true), mem::heap_alloc(16, true));
            put(mp, &m);
            for i in 0..n {
                put(ip + i * is, &vals[4 * i as usize..4 * i as usize + ni]);
            }
            for k in 0..(os * 6 + 16) / 4 { w32(op + 4 * k, 0xdead_0000 | k); }
            prop_assert_eq!(call(arr, &[op, os, ip, is, mp, n]), op);
            for i in 0..n {
                w32(single_in, 0); w32(single_in + 4, 0); w32(single_in + 8, 0); w32(single_in + 12, 0);
                put(single_in, &vals[4 * i as usize..4 * i as usize + ni]);
                prop_assert_eq!(call(one, &[single_out, single_in, mp]), single_out);
                prop_assert_eq!(get(op + i * os, no), get(single_out, no));
                for k in no as u32..os / 4 {
                    prop_assert_eq!(r32(op + i * os + 4 * k), 0xdead_0000 | (i * os / 4 + k));
                }
            }
            for k in (n * os) / 4..(os * 6 + 16) / 4 {
                prop_assert_eq!(r32(op + 4 * k), 0xdead_0000 | k);
            }
            for p in [mp, ip, op, single_in, single_out] { mem::heap_free(p); }
        }
    }

    /// a module without that RT_RCDATA resource: D3DERR_INVALIDCALL and no effect
    #[test]
    fn effect_resource_missing() {
        let _g = enter();
        let out = mem::heap_alloc(4, true);
        let name = mem::heap_alloc(8, true);
        for (i, b) in b"FX\0".iter().enumerate() { mem::w8(name + i as u32, *b); }
        w32(out, 0x1234);
        // (a module handle nothing is loaded at)
        assert_eq!(call(D3DXCreateEffectFromResourceA, &[0, 0x6000_0000, name, 0, 0, 0, 0, out, 0]), 0x8876_086c);
        assert_eq!(r32(out), 0);
        mem::heap_free(out);
        mem::heap_free(name);
    }
}
