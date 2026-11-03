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
fn qi_no(c: &mut Ctx) -> u32 {
    mem::putp(c.a(2), 0);
    0x8000_4002
}
fn add_ref(c: &mut Ctx) -> u32 {
    let n = r32(c.a(0) + 4) + 1;
    w32(c.a(0) + 4, n);
    n
}
fn release(c: &mut Ctx) -> u32 {
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

fn D3DXCreateBuffer(c: &mut Ctx) -> u32 {
    mem::putp(c.a(1), buffer(&vec![0; c.a(0) as usize]));
    0
}

// ---- the shader assembler ----

fn D3DXAssembleShader(c: &mut Ctx) -> u32 {
    // (data, length, defines, include, flags, shader, errors)
    let src = String::from_utf8_lossy(&mem::cstrn(c.a(0), c.a(1))).into_owned();
    let (out, errs) = (c.a(5), c.a(6));
    match crate::d3dx9asm::assemble(&src) {
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
fn com_call(c: &mut Ctx, obj: u32, m: u32, args: &[u32]) -> u32 {
    let f = r32(r32(obj) + 4 * m);
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
fn zero(_: &mut Ctx) -> u32 {
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

pub fn init() {
    let fns: &[(&str, Handler)] = &[
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
    ];
    for v in 24..=43 {
        let dll = format!("d3dx9_{v}");
        for (n, f) in fns {
            implement(&dll, n, *f);
        }
    }
}
