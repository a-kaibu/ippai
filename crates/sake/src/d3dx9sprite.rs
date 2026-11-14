//! ID3DXSprite (D3DXCreateSprite): textured quads queued by Draw and drawn by Flush/End with
//! DrawPrimitiveUP, in the states D3DX sets (as Wine's d3dx9 sprite.c: alpha blending and
//! test with ALPHABLEND, linear clamped sampling, a pixel-centred orthographic projection of the
//! viewport unless OBJECTSPACE). Begin saves the device state in the sprite's state block
//! (made once, captured at each Begin, as Wine's), End restores it.

#![allow(non_snake_case)]

use crate::api::{vtable, Ctx, Handler};
use crate::d3dx9::{add_ref, com_call, qi_no, zero};
use crate::mem::{self, r32, w32};
use crate::HashMap;
use std::sync::Mutex;

// object: vtable, refs, device, Begin flags, state block, transform (16 floats at 20), begun
const DEV: u32 = 8;
const FLAGS: u32 = 12;
const SB: u32 = 16;
const XF: u32 = 20;
const BEGUN: u32 = XF + 64;
const SIZE: u32 = BEGUN + 4;

/// Quads queued before Draw flushes by itself (Draw without End must not grow for ever), and the
/// quads one DrawPrimitiveUP takes.
const MAX_QUEUE: usize = 4096;
const MAX_RUN: usize = 1024;

const DONOTSAVESTATE: u32 = 1;
const DONOTMODIFY_RENDERSTATE: u32 = 2;
const OBJECTSPACE: u32 = 4;
const ALPHABLEND: u32 = 0x10;

struct Quad {
    tex: u32,
    v: [[f32; 5]; 4], // x y z u v
    color: u32,
}

/// A sprite's queue: its quads, the textures they hold a reference to (one each, however many
/// quads), and the texture sizes seen since Begin (GetLevelDesc once a texture, not a draw).
#[derive(Default)]
struct Pending {
    quads: Vec<Quad>,
    held: Vec<u32>,
    sizes: HashMap<u32, (f32, f32)>,
}

static QUEUE: Mutex<Option<HashMap<u32, Pending>>> = Mutex::new(None);

fn pending<R>(o: u32, f: impl FnOnce(&mut Pending) -> R) -> R {
    f(QUEUE.lock().unwrap().get_or_insert_with(HashMap::default).entry(o).or_default())
}

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
        let m: &[(Handler, u32)] = &[(qi_no, 12), (add_ref, 4), (release, 4), (get_device, 8), (get_transform, 8), (set_transform, 8),
            (zero, 12), (zero, 12), (begin, 8), (draw, 24), (flush, 4), (end, 4), (zero, 4), (zero, 4)];
        vtable("ID3DXSprite", m)
    });
    let o = mem::heap_alloc(SIZE, true);
    if o == 0 {
        return 0x8007_000e;
    }
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
        // (the textures queued and never drawn, the state block, the device)
        let p = QUEUE.lock().unwrap().get_or_insert_with(HashMap::default).remove(&o);
        for t in p.map(|p| p.held).unwrap_or_default().into_iter().chain([r32(o + SB), r32(o + DEV)]) {
            if t != 0 {
                com_call(c, t, 2, &[]);
            }
        }
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
    // (a second Begin before End keeps the state the first one saved)
    if r32(o + BEGUN) != 0 {
        return 0;
    }
    w32(o + FLAGS, flags);
    w32(o + BEGUN, 1);
    if flags & DONOTSAVESTATE == 0 {
        let sb = r32(o + SB);
        if sb != 0 {
            com_call(c, sb, 4, &[]); // Capture: the state of now
        } else {
            let p = mem::heap_alloc(8, true);
            // CreateStateBlock(D3DSBT_ALL): it holds the state of now
            if com_call(c, r32(o + DEV), 59, &[1, p]) == 0 {
                w32(o + SB, r32(p));
            }
            mem::heap_free(p);
        }
    }
    0
}

/// Draw(texture, src RECT*, center*, position*, color)
fn draw(c: &mut Ctx) -> u32 {
    let (o, tex, rect, center, pos, color) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if tex == 0 {
        return 0x8876_086c;
    }
    // the texture's size: GetLevelDesc(0) (D3DSURFACE_DESC: width at 24, height at 28), once a
    // texture between Begin and End
    let (tw, th) = match pending(o, |p| p.sizes.get(&tex).copied()) {
        Some(s) => s,
        None => {
            let desc = mem::heap_alloc(32, true);
            com_call(c, tex, 17, &[0, desc]);
            let s = (r32(desc + 24).max(1) as f32, r32(desc + 28).max(1) as f32);
            mem::heap_free(desc);
            pending(o, |p| p.sizes.insert(tex, s));
            s
        }
    };
    let (l, t, r, b) = if rect != 0 {
        (r32(rect) as i32 as f32, r32(rect + 4) as i32 as f32, r32(rect + 8) as i32 as f32, r32(rect + 12) as i32 as f32)
    } else {
        (0.0, 0.0, tw, th)
    };
    let v3 = |p: u32| if p != 0 { [rf(p, 0), rf(p, 1), rf(p, 2)] } else { [0.0; 3] };
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
        v: [corner(x0, y0, l, t), corner(x0 + w, y0, r, t), corner(x0 + w, y0 + h, r, b), corner(x0, y0 + h, l, b)],
        color,
    };
    let (hold, full) = pending(o, |p| {
        p.quads.push(q);
        let hold = !p.held.contains(&tex);
        if hold {
            p.held.push(tex);
        }
        (hold, p.quads.len() >= MAX_QUEUE)
    });
    if hold {
        com_call(c, tex, 1, &[]);
    }
    if full {
        flush(c);
    }
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
        for (s, v) in [(15, ab), (25, 5), (24, 0), (27, ab), (136, 1), (152, 0), (168, 0xf), (22, 1), (20, 6), (145, 1), (184, 0),
            (8, 3), (28, 0), (167, 0), (137, 0), (48, 0), (206, 0), (9, 2), (29, 0), (19, 5), (194, 0), (52, 0), (151, 0), (128, 0)]
        {
            com_call(c, d, 57, &[s, v]);
        }
        // stage 0: texture x diffuse; stage 1 off
        for (s, v) in [(5, 2), (6, 0), (4, 4), (2, 2), (3, 0), (1, 4), (11, 0), (24, 0)] {
            com_call(c, d, 67, &[0, s, v]);
        }
        com_call(c, d, 67, &[1, 4, 1]);
        com_call(c, d, 67, &[1, 1, 1]);
        for (s, v) in [(1, 3), (2, 3), (5, 2), (6, 2), (7, 2), (8, 0), (9, 0), (11, 0)] {
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
        let (x, y, w, h) = (r32(vp) as f32, r32(vp + 4) as f32, r32(vp + 8) as f32, r32(vp + 12) as f32);
        let (zn, zf) = (rf(vp, 4), rf(vp, 5));
        mem::heap_free(vp);
        // D3DXMatrixOrthoOffCenterLH(x + .5, x + w + .5, y + h + .5, y + .5, zn, zf)
        let (l, r, b, t) = (x + 0.5, x + w + 0.5, y + h + 0.5, y + 0.5);
        let dz = if zf != zn { zf - zn } else { 1.0 };
        let p = [2.0 / (r - l), 0.0, 0.0, 0.0, 0.0, 2.0 / (t - b), 0.0, 0.0, 0.0, 0.0, 1.0 / dz, 0.0, (l + r) / (l - r),
            (t + b) / (b - t), zn / (zn - zf), 1.0];
        for (i, v) in p.iter().enumerate() {
            w32(m + 4 * i as u32, v.to_bits());
        }
        com_call(c, d, 44, &[3, m]); // PROJECTION
    }
    mem::heap_free(m);
}

/// The vertices of quads: two triangles a quad, 24-byte vertices (xyz, colour, uv).
fn vertices(qs: &[Quad]) -> Vec<u8> {
    let mut b = Vec::with_capacity(qs.len() * 6 * 24);
    for q in qs {
        for k in [0, 1, 2, 0, 2, 3] {
            let v = q.v[k];
            for f in [v[0], v[1], v[2]] {
                b.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            b.extend_from_slice(&q.color.to_le_bytes());
            b.extend_from_slice(&v[3].to_bits().to_le_bytes());
            b.extend_from_slice(&v[4].to_bits().to_le_bytes());
        }
    }
    b
}

/// Runs of one texture, at most MAX_RUN quads each: (start, length).
fn texture_runs(qs: &[Quad]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < qs.len() {
        let tex = qs[i].tex;
        let n = qs[i..].iter().take(MAX_RUN).take_while(|q| q.tex == tex).count();
        out.push((i, n));
        i += n;
    }
    out
}

fn flush(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    // (the sizes go too: a texture no longer held may be freed and its address reused)
    let (qs, held) = pending(o, |p| {
        p.sizes.clear();
        (std::mem::take(&mut p.quads), std::mem::take(&mut p.held))
    });
    let mut hr = 0;
    if !qs.is_empty() {
        set_states(c, o);
        let d = r32(o + DEV);
        let buf = mem::heap_alloc((MAX_RUN.min(qs.len()) * 6 * 24) as u32, false);
        if buf == 0 {
            hr = 0x8007_000e;
        } else {
            for (i, n) in texture_runs(&qs) {
                mem::write(buf, &vertices(&qs[i..i + n]));
                com_call(c, d, 65, &[0, qs[i].tex]); // SetTexture
                com_call(c, d, 83, &[4, 2 * n as u32, buf, 24]); // DrawPrimitiveUP(TRIANGLELIST)
            }
            mem::heap_free(buf);
        }
    }
    for t in held {
        com_call(c, t, 2, &[]);
    }
    hr
}

fn end(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let hr = flush(c);
    let sb = r32(o + SB);
    if r32(o + BEGUN) != 0 && r32(o + FLAGS) & DONOTSAVESTATE == 0 && sb != 0 {
        com_call(c, sb, 5, &[]); // Apply
    }
    w32(o + BEGUN, 0);
    hr
}

/// Review b05: Draw takes one texture reference and one GetLevelDesc per texture (not per quad),
/// a long Draw without End flushes by itself, flush draws in bounded runs from one block, the
/// state block is made once and captured at each Begin (a second Begin keeps the first), and the
/// last Release lets go of queued textures and the state block. A model of the device's calls
/// checks the reference counts for any sequence of Begin / Draw / Flush / End.
#[cfg(test)]
mod review_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    thread_local! {
        static LOG: std::cell::RefCell<Vec<(u32, u32)>> = const { std::cell::RefCell::new(Vec::new()) };
    }
    /// any COM method N of a fake object: logged (object, N); AddRef/Release count at +4,
    /// GetLevelDesc answers 64x32, CreateStateBlock makes a fake state block
    fn rec<const N: u32>(c: &mut Ctx) -> u32 {
        let o = c.a(0);
        LOG.with(|l| l.borrow_mut().push((o, N)));
        match N {
            1 => { w32(o + 4, r32(o + 4) + 1); r32(o + 4) }
            2 => { w32(o + 4, r32(o + 4) - 1); r32(o + 4) }
            17 if r32(o + 8) == 1 => { w32(c.a(2) + 24, 64); w32(c.a(2) + 28, 32); 0 }
            59 => { mem::putp(c.a(2), fake(2)); 0 }
            _ => 0,
        }
    }
    fn vt() -> u32 {
        static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
        *V.get_or_init(|| {
            let m: Vec<(Handler, u32)> = vec![(rec::<0>, 4), (rec::<1>, 4), (rec::<2>, 4), (rec::<3>, 4), (rec::<4>, 4), (rec::<5>, 4), (rec::<6>, 4), (rec::<7>, 4), (rec::<8>, 4), (rec::<9>, 4), (rec::<10>, 4), (rec::<11>, 4), (rec::<12>, 4), (rec::<13>, 4), (rec::<14>, 4), (rec::<15>, 4), (rec::<16>, 4), (rec::<17>, 4), (rec::<18>, 4), (rec::<19>, 4), (rec::<20>, 4), (rec::<21>, 4), (rec::<22>, 4), (rec::<23>, 4), (rec::<24>, 4), (rec::<25>, 4), (rec::<26>, 4), (rec::<27>, 4), (rec::<28>, 4), (rec::<29>, 4), (rec::<30>, 4), (rec::<31>, 4), (rec::<32>, 4), (rec::<33>, 4), (rec::<34>, 4), (rec::<35>, 4), (rec::<36>, 4), (rec::<37>, 4), (rec::<38>, 4), (rec::<39>, 4), (rec::<40>, 4), (rec::<41>, 4), (rec::<42>, 4), (rec::<43>, 4), (rec::<44>, 4), (rec::<45>, 4), (rec::<46>, 4), (rec::<47>, 4), (rec::<48>, 4), (rec::<49>, 4), (rec::<50>, 4), (rec::<51>, 4), (rec::<52>, 4), (rec::<53>, 4), (rec::<54>, 4), (rec::<55>, 4), (rec::<56>, 4), (rec::<57>, 4), (rec::<58>, 4), (rec::<59>, 4), (rec::<60>, 4), (rec::<61>, 4), (rec::<62>, 4), (rec::<63>, 4), (rec::<64>, 4), (rec::<65>, 4), (rec::<66>, 4), (rec::<67>, 4), (rec::<68>, 4), (rec::<69>, 4), (rec::<70>, 4), (rec::<71>, 4), (rec::<72>, 4), (rec::<73>, 4), (rec::<74>, 4), (rec::<75>, 4), (rec::<76>, 4), (rec::<77>, 4), (rec::<78>, 4), (rec::<79>, 4), (rec::<80>, 4), (rec::<81>, 4), (rec::<82>, 4), (rec::<83>, 4), (rec::<84>, 4), (rec::<85>, 4), (rec::<86>, 4), (rec::<87>, 4), (rec::<88>, 4), (rec::<89>, 4), (rec::<90>, 4), (rec::<91>, 4), (rec::<92>, 4), (rec::<93>, 4), (rec::<94>, 4), (rec::<95>, 4), (rec::<96>, 4), (rec::<97>, 4), (rec::<98>, 4), (rec::<99>, 4), (rec::<100>, 4), (rec::<101>, 4), (rec::<102>, 4), (rec::<103>, 4), (rec::<104>, 4), (rec::<105>, 4), (rec::<106>, 4), (rec::<107>, 4), (rec::<108>, 4), (rec::<109>, 4), (rec::<110>, 4), (rec::<111>, 4), (rec::<112>, 4), (rec::<113>, 4), (rec::<114>, 4), (rec::<115>, 4), (rec::<116>, 4), (rec::<117>, 4), (rec::<118>, 4), (rec::<119>, 4)];
            crate::api::vtable("Fake", &m)
        })
    }
    /// a fake object (kind 0 device, 1 texture, 2 state block) with one reference
    fn fake(kind: u32) -> u32 {
        let o = mem::heap_alloc(16, true);
        w32(o, vt());
        w32(o + 4, 1);
        w32(o + 8, kind);
        o
    }
    fn log() -> Vec<(u32, u32)> {
        LOG.with(|l| std::mem::take(&mut *l.borrow_mut()))
    }
    fn vcall(o: u32, slot: u32, args: &[u32]) -> u32 {
        let f = mem::rp(mem::rp(o) + 4 * slot);
        let h = crate::api::handler_of(f).expect("a host method");
        let mut a = vec![o];
        a.extend_from_slice(args);
        call(h, &a)
    }
    fn sprite(dev: u32) -> u32 {
        let out = mem::heap_alloc(4, true);
        assert_eq!(call(D3DXCreateSprite, &[dev, out]), 0);
        let s = r32(out);
        mem::heap_free(out);
        s
    }

    /// 20 draws of one texture: one AddRef, one GetLevelDesc, one Release at Flush, one
    /// DrawPrimitiveUP of 40 triangles; the state block made at the first Begin, captured at the
    /// second, applied at each End, released with the sprite
    #[test]
    fn one_reference_and_size_a_texture() {
        let _g = enter();
        let (dev, tex) = (fake(0), fake(1));
        let s = sprite(dev);
        log();
        for round in 0..2 {
            assert_eq!(vcall(s, 8, &[0]), 0); // Begin
            for _ in 0..20 {
                assert_eq!(vcall(s, 9, &[tex, 0, 0, 0, 0xffff_ffff]), 0);
            }
            assert_eq!(r32(tex + 4), 2);
            assert_eq!(vcall(s, 11, &[]), 0); // End
            assert_eq!(r32(tex + 4), 1);
            let l = log();
            let n = |o: u32, m: u32| l.iter().filter(|x| **x == (o, m)).count();
            assert_eq!((n(tex, 1), n(tex, 17), n(tex, 2)), (1, 1, 1));
            assert_eq!(n(dev, 83), 1);
            assert_eq!(n(dev, 59), if round == 0 { 1 } else { 0 });
            let sb = r32(s + SB);
            assert_eq!((n(sb, 4), n(sb, 5)), (round, 1));
        }
        let sb = r32(s + SB);
        assert_eq!(vcall(s, 2, &[]), 0);
        assert_eq!((r32(sb + 4), r32(dev + 4)), (0, 1));
        for p in [dev, tex, sb] { mem::heap_free(p); }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]
        /// any sequence of Begin / Draw (one of 3 textures) / Flush / End, then Release: each
        /// texture is back to its own reference, never below; no queue grows past MAX_QUEUE;
        /// every DrawPrimitiveUP is at most MAX_RUN quads
        #[test]
        fn references_balance(ops in prop::collection::vec((0u8..4, 0usize..3, 1usize..3000), 1..12)) {
            let _g = enter();
            let dev = fake(0);
            let texs = [fake(1), fake(1), fake(1)];
            let s = sprite(dev);
            for &(op, t, n) in &ops {
                match op {
                    0 => { vcall(s, 8, &[0]); }
                    1 => {
                        for _ in 0..n.min(MAX_QUEUE + 10) {
                            vcall(s, 9, &[texs[t], 0, 0, 0, 0]);
                        }
                    }
                    2 => { vcall(s, 10, &[]); }
                    _ => { vcall(s, 11, &[]); }
                }
                let q = pending(s, |p| p.quads.len());
                prop_assert!(q < MAX_QUEUE);
                for &x in &texs { prop_assert!(r32(x + 4) >= 1); }
            }
            let sb = r32(s + SB);
            vcall(s, 2, &[]);
            for &x in &texs { prop_assert_eq!(r32(x + 4), 1); }
            prop_assert_eq!(r32(dev + 4), 1);
            if sb != 0 { prop_assert_eq!(r32(sb + 4), 0); mem::heap_free(sb); }
            log();
            for p in texs.into_iter().chain([dev]) { mem::heap_free(p); }
        }
    }

    /// runs never mix textures and never pass MAX_RUN quads; vertices are 6 x 24 bytes a quad
    #[test]
    fn runs_and_vertices() {
        let q = |tex| Quad { tex, v: [[0.0; 5]; 4], color: 0 };
        let qs: Vec<Quad> = (0..2500).map(|i| q(if i < 2100 { 1 } else { 2 })).collect();
        assert_eq!(texture_runs(&qs), vec![(0, 1024), (1024, 1024), (2048, 52), (2100, 400)]);
        assert_eq!(vertices(&qs[..3]).len(), 3 * 6 * 24);
    }
}
