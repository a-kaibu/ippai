//! ID3DXSprite (D3DXCreateSprite): textured quads queued by Draw and drawn by Flush/End with
//! DrawPrimitiveUP, in the states D3DX sets (as Wine's d3dx9 sprite.c: alpha blending and
//! test with ALPHABLEND, linear clamped sampling, a pixel-centred orthographic projection of the
//! viewport unless OBJECTSPACE). Begin saves the device state in a state block, End restores it.

#![allow(non_snake_case)]

use crate::api::{vtable, Ctx, Handler};
use crate::d3dx9::{add_ref, com_call, qi_no, zero};
use crate::mem::{self, r32, w32};
use crate::HashMap;
use std::sync::Mutex;

// object: vtable, refs, device, Begin flags, state block, transform (16 floats at 20)
const DEV: u32 = 8;
const FLAGS: u32 = 12;
const SB: u32 = 16;
const XF: u32 = 20;

const DONOTSAVESTATE: u32 = 1;
const DONOTMODIFY_RENDERSTATE: u32 = 2;
const OBJECTSPACE: u32 = 4;
const ALPHABLEND: u32 = 0x10;

struct Quad {
    tex: u32,
    v: [[f32; 5]; 4], // x y z u v
    color: u32,
}

static QUEUE: Mutex<Option<HashMap<u32, Vec<Quad>>>> = Mutex::new(None);

fn rf(p: u32, i: u32) -> f32 {
    f32::from_bits(r32(p + 4 * i))
}

/// D3DXCreateSprite(device, &sprite)
pub fn D3DXCreateSprite(c: &mut Ctx) -> u32 {
    let (dev, out) = (c.a(0), c.a(1));
    if dev == 0 || out == 0 {
        return 0x8876_086c; // D3DERR_INVALIDCALL
    }
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let vt = *V.get_or_init(|| {
        let m: &[(Handler, u32)] = &[
            (qi_no, 12),
            (add_ref, 4),
            (release, 4),
            (get_device, 8),
            (get_transform, 8),
            (set_transform, 8),
            (zero, 12),
            (zero, 12),
            (begin, 8),
            (draw, 24),
            (flush, 4),
            (end, 4),
            (zero, 4),
            (zero, 4),
        ];
        vtable("ID3DXSprite", m)
    });
    let o = mem::heap_alloc(XF + 64, true);
    w32(o, vt);
    w32(o + 4, 1);
    w32(o + DEV, dev);
    for i in 0..4 {
        w32(o + XF + 4 * (i * 5), 1f32.to_bits());
    }
    com_call(c, dev, 1, &[]);
    mem::putp(out, o);
    0
}

fn release(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let n = r32(o + 4).saturating_sub(1);
    w32(o + 4, n);
    if n == 0 {
        QUEUE
            .lock()
            .unwrap()
            .get_or_insert_with(HashMap::default)
            .remove(&o);
        com_call(c, r32(o + DEV), 2, &[]);
        mem::heap_free(o);
    }
    n
}
fn get_device(c: &mut Ctx) -> u32 {
    let d = r32(c.a(0) + DEV);
    com_call(c, d, 1, &[]);
    mem::putp(c.a(1), d);
    0
}
fn get_transform(c: &mut Ctx) -> u32 {
    mem::copy(c.a(1), c.a(0) + XF, 64);
    0
}
fn set_transform(c: &mut Ctx) -> u32 {
    mem::copy(c.a(0) + XF, c.a(1), 64);
    0
}

fn begin(c: &mut Ctx) -> u32 {
    let (o, flags) = (c.a(0), c.a(1));
    w32(o + FLAGS, flags);
    if flags & DONOTSAVESTATE == 0 {
        let sb = mem::heap_alloc(8, true);
        // CreateStateBlock(D3DSBT_ALL): it holds the state of now
        if com_call(c, r32(o + DEV), 59, &[1, sb]) == 0 {
            w32(o + SB, r32(sb));
        }
        mem::heap_free(sb);
    }
    0
}

/// Draw(texture, src RECT*, center*, position*, color)
fn draw(c: &mut Ctx) -> u32 {
    let (o, tex, rect, center, pos, color) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if tex == 0 {
        return 0x8876_086c;
    }
    // the texture's size: GetLevelDesc(0) (D3DSURFACE_DESC: width at 24, height at 28)
    let desc = mem::heap_alloc(32, true);
    com_call(c, tex, 17, &[0, desc]);
    let (tw, th) = (r32(desc + 24).max(1) as f32, r32(desc + 28).max(1) as f32);
    mem::heap_free(desc);
    let (l, t, r, b) = if rect != 0 {
        (
            r32(rect) as i32 as f32,
            r32(rect + 4) as i32 as f32,
            r32(rect + 8) as i32 as f32,
            r32(rect + 12) as i32 as f32,
        )
    } else {
        (0.0, 0.0, tw, th)
    };
    let v3 = |p: u32| {
        if p != 0 {
            [rf(p, 0), rf(p, 1), rf(p, 2)]
        } else {
            [0.0; 3]
        }
    };
    let (ce, po) = (v3(center), v3(pos));
    let (x0, y0, z) = (po[0] - ce[0], po[1] - ce[1], po[2] - ce[2]);
    let (w, h) = (r - l, b - t);
    let m: [f32; 16] = std::array::from_fn(|i| rf(o + XF, i as u32));
    let xf = |x: f32, y: f32| {
        let v = [x, y, z, 1.0];
        let r: [f32; 4] = std::array::from_fn(|j| (0..4).map(|i| v[i] * m[i * 4 + j]).sum());
        let wd = if r[3] != 0.0 { r[3] } else { 1.0 };
        [r[0] / wd, r[1] / wd, r[2] / wd]
    };
    let corner = |x: f32, y: f32, u: f32, v: f32| {
        let p = xf(x, y);
        [p[0], p[1], p[2], u / tw, v / th]
    };
    let q = Quad {
        tex,
        v: [
            corner(x0, y0, l, t),
            corner(x0 + w, y0, r, t),
            corner(x0 + w, y0 + h, r, b),
            corner(x0, y0 + h, l, b),
        ],
        color,
    };
    com_call(c, tex, 1, &[]);
    QUEUE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::default)
        .entry(o)
        .or_default()
        .push(q);
    0
}

fn set_states(c: &mut Ctx, o: u32) {
    let (d, flags) = (r32(o + DEV), r32(o + FLAGS));
    com_call(c, d, 92, &[0]); // SetVertexShader(NULL)
    com_call(c, d, 107, &[0]); // SetPixelShader(NULL)
    com_call(c, d, 89, &[0x142]); // SetFVF(XYZ | DIFFUSE | TEX1)
    if flags & DONOTMODIFY_RENDERSTATE == 0 {
        let ab = (flags & ALPHABLEND != 0) as u32;
        // (render state, value): alpha test/blend, no culling, lighting, fog, stencil, clipping planes
        for (s, v) in [
            (15, ab),
            (25, 5),
            (24, 0),
            (27, ab),
            (136, 1),
            (152, 0),
            (168, 0xf),
            (22, 1),
            (20, 6),
            (145, 1),
            (184, 0),
            (8, 3),
            (28, 0),
            (167, 0),
            (137, 0),
            (48, 0),
            (206, 0),
            (9, 2),
            (29, 0),
            (19, 5),
            (194, 0),
            (52, 0),
            (151, 0),
            (128, 0),
        ] {
            com_call(c, d, 57, &[s, v]);
        }
        // stage 0: texture x diffuse; stage 1 off
        for (s, v) in [
            (5, 2),
            (6, 0),
            (4, 4),
            (2, 2),
            (3, 0),
            (1, 4),
            (11, 0),
            (24, 0),
        ] {
            com_call(c, d, 67, &[0, s, v]);
        }
        com_call(c, d, 67, &[1, 4, 1]);
        com_call(c, d, 67, &[1, 1, 1]);
        for (s, v) in [
            (1, 3),
            (2, 3),
            (5, 2),
            (6, 2),
            (7, 2),
            (8, 0),
            (9, 0),
            (11, 0),
        ] {
            com_call(c, d, 69, &[0, s, v]);
        }
    }
    let m = mem::heap_alloc(64, true);
    let ident = |m: u32| {
        mem::fill(m, 64, 0);
        for i in 0..4 {
            w32(m + 4 * (i * 5), 1f32.to_bits());
        }
    };
    ident(m);
    com_call(c, d, 44, &[256, m]); // WORLD
    if flags & OBJECTSPACE == 0 {
        com_call(c, d, 44, &[2, m]); // VIEW
                                     // GetViewport: X Y Width Height MinZ MaxZ
        let vp = mem::heap_alloc(24, true);
        com_call(c, d, 48, &[vp]);
        let (x, y, w, h) = (
            r32(vp) as f32,
            r32(vp + 4) as f32,
            r32(vp + 8) as f32,
            r32(vp + 12) as f32,
        );
        let (zn, zf) = (rf(vp, 4), rf(vp, 5));
        mem::heap_free(vp);
        // D3DXMatrixOrthoOffCenterLH(x + .5, x + w + .5, y + h + .5, y + .5, zn, zf)
        let (l, r, b, t) = (x + 0.5, x + w + 0.5, y + h + 0.5, y + 0.5);
        let dz = if zf != zn { zf - zn } else { 1.0 };
        let p = [
            2.0 / (r - l),
            0.0,
            0.0,
            0.0,
            0.0,
            2.0 / (t - b),
            0.0,
            0.0,
            0.0,
            0.0,
            1.0 / dz,
            0.0,
            (l + r) / (l - r),
            (t + b) / (b - t),
            zn / (zn - zf),
            1.0,
        ];
        for (i, v) in p.iter().enumerate() {
            w32(m + 4 * i as u32, v.to_bits());
        }
        com_call(c, d, 44, &[3, m]); // PROJECTION
    }
    mem::heap_free(m);
}

fn flush(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let qs = QUEUE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::default)
        .remove(&o)
        .unwrap_or_default();
    if qs.is_empty() {
        return 0;
    }
    set_states(c, o);
    let d = r32(o + DEV);
    // runs of one texture: two triangles a quad, 24-byte vertices (xyz, colour, uv)
    let mut i = 0;
    while i < qs.len() {
        let tex = qs[i].tex;
        let n = qs[i..].iter().take_while(|q| q.tex == tex).count();
        let buf = mem::heap_alloc(n as u32 * 6 * 24, false);
        let mut p = buf;
        for q in &qs[i..i + n] {
            for k in [0, 1, 2, 0, 2, 3] {
                let v = q.v[k];
                for (j, f) in [v[0], v[1], v[2]].iter().enumerate() {
                    w32(p + 4 * j as u32, f.to_bits());
                }
                w32(p + 12, q.color);
                w32(p + 16, v[3].to_bits());
                w32(p + 20, v[4].to_bits());
                p += 24;
            }
        }
        com_call(c, d, 65, &[0, tex]); // SetTexture
        com_call(c, d, 83, &[4, 2 * n as u32, buf, 24]); // DrawPrimitiveUP(TRIANGLELIST)
        mem::heap_free(buf);
        i += n;
    }
    for q in &qs {
        com_call(c, q.tex, 2, &[]);
    }
    0
}

fn end(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    flush(c);
    let sb = r32(o + SB);
    if sb != 0 {
        com_call(c, sb, 5, &[]); // Apply
        com_call(c, sb, 2, &[]);
        w32(o + SB, 0);
    }
    0
}
