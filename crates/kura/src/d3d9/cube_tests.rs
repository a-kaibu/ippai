//! Direct3D 9 cube textures (#126) on the model of WebGPU (render/model.rs): CreateCubeTexture
//! and its descriptions, each face's levels as surfaces (GetCubeMapSurface, LockRect with
//! rectangles), the faces reaching their layers of the page's six-layer texture in
//! D3DCUBEMAP_FACES order, UpdateTexture from system memory, and draws of a ps_2_0 shader with
//! dcl_cube being valid WebGPU (a texture_cube binding through a cube view). A model (PBT)
//! checks any sequence of face/level locks against what the page holds. Direct3D 8 reads the
//! same through d3d8.rs.

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, on_low_stack, CALLS, SERIAL};
use super::*;
use crate::render::model;
use proptest::prelude::*;

/// A 64x64 device (software vertex processing).
fn device() -> u32 {
    attach_page();
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    rd(o)
}

fn init() {
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
}

fn errors() -> Vec<String> {
    model::with(|m| std::mem::take(&mut m.errors))
}

fn pump() {
    gpu::flush();
    gpu::pump();
}

/// The page's 2D textures of `e`x`e` with six layers made since `from`: texels per (mip, layer).
fn page_cubes(from: usize, e: u32) -> Vec<Vec<Vec<u64>>> {
    model::with(|m| {
        m.objs[from..]
            .iter()
            .filter_map(|o| match o {
                model::Obj::Texture(t) if t.dim == "2d" && t.w == e && t.h == e && t.layers == 6 => Some(t.texels.clone()),
                _ => None,
            })
            .collect()
    })
}

/// texel (x, y) of face `f` level `l` in the tests: A8R8G8B8, face in red, level in alpha
fn texel(f: u32, l: u32, x: u32, y: u32) -> u32 {
    (0x80 + l) << 24 | (f * 40 + 7) << 16 | (y * 9 + 3) << 8 | (x * 9 + 1)
}

/// Fills rect (left, top, right, bottom) of a locked rect (pitch, bits) with `px(x, y)`.
fn fill(lr: u32, r: [u32; 4], px: impl Fn(u32, u32) -> u32) {
    let (pitch, bits) = (rd(lr), rd(lr + 4));
    for y in r[1]..r[3] {
        let row: Vec<u8> = (r[0]..r[2]).flat_map(|x| px(x, y).to_le_bytes()).collect();
        put_bytes(bits + (y - r[1]) * pitch, &row);
    }
}

fn words(w: &[u32]) -> u32 {
    let p = unsafe { alloc(4 * w.len() as u32) as u32 };
    put_bytes(p, &w.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    p
}

#[test]
fn cube_textures_reach_the_page_and_draw() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let dev = device();
    errors();
    use super::format as f;
    let caps = unsafe { alloc(304) as u32 };
    assert_eq!(call(IF_DEVICE, 7, &[dev, caps]), 0);
    assert_ne!(rd(caps + 15 * 4) & 0x800, 0, "TextureCaps CUBEMAP");
    // invalid: no size, a depth format, no out pointer; a render target cube is not available
    // yet (an error the game sees, not a NULL)
    for (e, usage, fmt) in [(0, 0, f::A8R8G8B8), (4, 0, 80), (4, 2, f::D24S8)] {
        let o = out();
        assert_eq!(call(IF_DEVICE, 25, &[dev, e, 1, usage, fmt, 1, o, 0]), 0x8876086c, "edge {e} usage {usage} format {fmt}");
        assert_eq!(rd(o), 0);
    }
    let o = out();
    assert_eq!(call(IF_DEVICE, 25, &[dev, 4, 1, 1, f::A8R8G8B8, 0, o, 0]), 0x8876086a, "render target cube: NOTAVAILABLE");
    assert_eq!(rd(o), 0);
    // 8x8, all levels (4), managed
    let from = model::with(|m| m.objs.len());
    let o = out();
    assert_eq!(call(IF_DEVICE, 25, &[dev, 8, 0, 0, f::A8R8G8B8, 1, o, 0]), 0);
    let cube = rd(o);
    assert_eq!(call(IF_CUBE, 13, &[cube]), 4, "GetLevelCount (of a face)");
    assert_eq!(call(IF_CUBE, 10, &[cube]), 5, "GetType: D3DRTYPE_CUBETEXTURE");
    for l in 0..4u32 {
        let desc = unsafe { alloc(32) as u32 };
        assert_eq!(call(IF_CUBE, 17, &[cube, l, desc]), 0);
        let e = 8 >> l;
        assert_eq!((0..8).map(|i| rd(desc + 4 * i)).collect::<Vec<_>>(), [f::A8R8G8B8, 1, 0, 1, 0, 0, e, e], "level {l} desc");
    }
    assert_eq!(call(IF_CUBE, 17, &[cube, 4, out()]), 0x8876086c, "level past the last");
    for (face, level) in [(6, 0), (0, 4)] {
        assert_eq!(call(IF_CUBE, 18, &[cube, face, level, out()]), 0x8876086c, "face {face} level {level}");
        assert_eq!(call(IF_CUBE, 19, &[cube, face, level, out(), 0, 0]), 0x8876086c);
    }
    // every face's every level, through LockRect(face, level) and through the surface
    let lr = unsafe { alloc(8) as u32 };
    for face in 0..6 {
        for l in 0..4 {
            let e = 8 >> l;
            if face % 2 == 0 {
                assert_eq!(call(IF_CUBE, 19, &[cube, face, l, lr, 0, 0]), 0);
                assert_eq!(rd(lr), 4 * e.max(1), "pitch");
                fill(lr, [0, 0, e, e], |x, y| texel(face, l, x, y));
                assert_eq!(call(IF_CUBE, 20, &[cube, face, l]), 0);
            } else {
                let o = out();
                assert_eq!(call(IF_CUBE, 18, &[cube, face, l, o]), 0);
                let s = rd(o);
                let iid_cube = words(&[0xfff32f81, 0x473ad953, 0xd6932392, 0x3fa9ab52]);
                let o = out();
                assert_eq!(call(IF_SURFACE, 11, &[s, iid_cube, o]), 0, "GetContainer");
                assert_eq!(rd(o), cube);
                call(IF_CUBE, 2, &[cube]);
                assert_eq!(call(IF_SURFACE, 13, &[s, lr, 0, 0]), 0);
                fill(lr, [0, 0, e, e], |x, y| texel(face, l, x, y));
                assert_eq!(call(IF_SURFACE, 14, &[s]), 0);
                call(IF_SURFACE, 2, &[s]);
            }
        }
    }
    pump();
    let p = page_cubes(from, 8);
    assert_eq!(p.len(), 1, "one 8x8 six-layer page texture");
    for l in 0..4u32 {
        let e = 8 >> l;
        for face in 0..6u32 {
            let want: Vec<u64> = (0..e * e).map(|i| texel(face, l, i % e, i / e) as u64).collect();
            assert_eq!(p[0][(l * 6 + face) as usize], want, "level {l} face {face}");
        }
    }
    // a rectangle of one face (+Z): the others stay
    assert_eq!(call(IF_CUBE, 19, &[cube, 4, 0, lr, words(&[2, 1, 6, 3]), 0]), 0);
    fill(lr, [0, 0, 4, 2], |_, _| 0x5555_5555);
    assert_eq!(call(IF_CUBE, 20, &[cube, 4, 0]), 0);
    assert_eq!(call(IF_CUBE, 21, &[cube, 4, 0]), 0, "AddDirtyRect");
    pump();
    let p = page_cubes(from, 8);
    for face in 0..6u32 {
        let want: Vec<u64> = (0..64u32)
            .map(|i| {
                let (x, y) = (i % 8, i / 8);
                if face == 4 && (2..6).contains(&x) && (1..3).contains(&y) { 0x5555_5555 } else { texel(face, 0, x, y) as u64 }
            })
            .collect();
        assert_eq!(p[0][face as usize], want, "face {face} after the rect");
    }
    // UpdateTexture: system memory cube into a default pool one (a 2D texture or a cube of
    // other levels: refused)
    let o = out();
    assert_eq!(call(IF_DEVICE, 25, &[dev, 4, 0, 0, f::A8R8G8B8, 2, o, 0]), 0);
    let sys = rd(o);
    let from = model::with(|m| m.objs.len());
    let o = out();
    assert_eq!(call(IF_DEVICE, 25, &[dev, 4, 2, 0, f::A8R8G8B8, 0, o, 0]), 0);
    let def = rd(o);
    for face in 0..6 {
        for l in 0..3 {
            assert_eq!(call(IF_CUBE, 19, &[sys, face, l, lr, 0, 0]), 0);
            let e = 4 >> l;
            fill(lr, [0, 0, e, e], |x, y| texel(face, l, x, y));
            assert_eq!(call(IF_CUBE, 20, &[sys, face, l]), 0);
        }
    }
    assert_eq!(call(IF_DEVICE, 31, &[dev, sys, def]), 0);
    pump();
    let p = page_cubes(from, 4);
    assert_eq!(p.len(), 1);
    // (each face's levels from the top: the source's 1x1 level has no partner)
    for l in 0..2u32 {
        let e = 4 >> l;
        for face in 0..6u32 {
            let want: Vec<u64> = (0..e * e).map(|i| texel(face, l, i % e, i / e) as u64).collect();
            assert_eq!(p[0][(l * 6 + face) as usize], want, "UpdateTexture level {l} face {face}");
        }
    }
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, 4, 4, 1, 0, f::A8R8G8B8, 0, o, 0]), 0);
    let t2 = rd(o);
    assert_eq!(call(IF_DEVICE, 31, &[dev, sys, t2]), 0x8876086c, "a cube into a 2D texture");
    let e = errors();
    assert!(e.is_empty(), "cubes: {:?}", &e[..e.len().min(8)]);
    // ps_2_0: dcl t0.xyz; dcl_cube s0; texld r0, t0, s0; mov oC0, r0
    let code = words(&[0xffff0200, 0x0200001f, 0x80000000, 0xb0070000, 0x0200001f, 0x98000000, 0xa00f0800, 0x03000042, 0x800f0000, 0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff]);
    let o = out();
    assert_eq!(call(IF_DEVICE, 106, &[dev, code, o]), 0);
    let ps = rd(o);
    call(IF_DEVICE, 107, &[dev, ps]);
    let from = model::with(|m| m.objs.len());
    call(IF_DEVICE, 65, &[dev, 0, cube]);
    quad(dev);
    // a 2D texture where a cube is sampled, and the cube under fixed function: valid, white
    call(IF_DEVICE, 65, &[dev, 0, t2]);
    quad(dev);
    call(IF_DEVICE, 107, &[dev, 0]);
    call(IF_DEVICE, 65, &[dev, 0, cube]);
    quad(dev);
    call(IF_DEVICE, 65, &[dev, 0, 0]);
    let e = errors();
    assert!(e.is_empty(), "draws: {:?}", &e[..e.len().min(8)]);
    let views = model::with(|m| {
        let mut views = Vec::new();
        for o in &m.objs[from..] {
            if let model::Obj::Group(_, entries) = o {
                for (b, e) in entries {
                    if let (0, model::Entry::View(v)) = (b, e) {
                        let model::Obj::View(v) = &m.objs[*v] else { continue };
                        let model::Obj::Texture(t) = &m.objs[v.tex] else { continue };
                        views.push((v.dim.clone(), t.w, t.layers));
                    }
                }
            }
        }
        views
    });
    assert!(views.contains(&("cube".to_string(), 8, 6)), "the cube's cube view sampled: {views:?}");
    for t in [sys, def, cube] {
        call(IF_CUBE, 2, &[t]);
    }
    call(IF_TEXTURE, 2, &[t2]);
    gpu::set_pump(None);
}

/// A pre-transformed quad over the target with 3D texture coordinates (XYZRHW | TEX1 |
/// TEXCOORDSIZE3(0)) pointing out of the cube's +Z face.
fn quad(dev: u32) {
    let v = unsafe { alloc(112) as u32 };
    let mut b = Vec::new();
    for (x, y) in [(0f32, 0f32), (64.0, 0.0), (0.0, 64.0), (64.0, 64.0)] {
        for f in [x - 0.5, y - 0.5, 0.5, 1.0, x / 32.0 - 1.0, 1.0 - y / 32.0, 1.0] {
            b.extend(f.to_le_bytes());
        }
    }
    put_bytes(v, &b);
    call(IF_DEVICE, 89, &[dev, 0x4 | 0x100 | 0x10000]);
    call(IF_DEVICE, 41, &[dev]);
    assert_eq!(call(IF_DEVICE, 83, &[dev, 5, 2, v, 28]), 0);
    call(IF_DEVICE, 42, &[dev]);
    pump();
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]
    /// Any sequence of locks of a cube's faces and levels (whole or a rectangle, through the
    /// cube or its surface, some read only) leaves the page's layer (level * 6 + face) holding
    /// what a plain model of six chains of images holds.
    #[test]
    fn face_locks_match_a_model(edge in 1u32..9, ops in proptest::collection::vec((0u32..6, 0u32..4, any::<[u8; 4]>(), any::<bool>(), any::<bool>(), any::<u32>()), 1..12)) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        init();
        let dev = device();
        errors();
        let from = model::with(|m| m.objs.len());
        let o = out();
        prop_assert_eq!(call(IF_DEVICE, 25, &[dev, edge, 0, 0, super::format::A8R8G8B8, 1, o, 0]), 0);
        let cube = rd(o);
        let levels = 32 - edge.leading_zeros();
        prop_assert_eq!(call(IF_CUBE, 13, &[cube]), levels);
        // the model: zeros, as a new texture reads
        let mut want: Vec<Vec<Vec<u32>>> = (0..6).map(|_| (0..levels).map(|l| vec![0; ((edge >> l).max(1).pow(2)) as usize]).collect()).collect();
        let lr = unsafe { alloc(8) as u32 };
        for (face, level, r, via_surface, read_only, seed) in ops {
            let level = level % levels;
            let e = (edge >> level).max(1);
            let (x0, y0) = (r[0] as u32 % e, r[1] as u32 % e);
            let (x1, y1) = (x0 + 1 + r[2] as u32 % (e - x0), y0 + 1 + r[3] as u32 % (e - y0));
            let rect = words(&[x0, y0, x1, y1]);
            let flags = if read_only { 0x10 } else { 0 };
            let px = |x: u32, y: u32| seed.wrapping_mul(2654435761).wrapping_add(x * 977 + y * 131 + face * 7 + level);
            if via_surface {
                let o = out();
                prop_assert_eq!(call(IF_CUBE, 18, &[cube, face, level, o]), 0);
                let s = rd(o);
                prop_assert_eq!(call(IF_SURFACE, 13, &[s, lr, rect, flags]), 0);
                fill(lr, [x0, y0, x1, y1], px);
                prop_assert_eq!(call(IF_SURFACE, 14, &[s]), 0);
                call(IF_SURFACE, 2, &[s]);
            } else {
                prop_assert_eq!(call(IF_CUBE, 19, &[cube, face, level, lr, rect, flags]), 0);
                fill(lr, [x0, y0, x1, y1], px);
                prop_assert_eq!(call(IF_CUBE, 20, &[cube, face, level]), 0);
            }
            // (a read-only lock's writes stay in the guest copy: the page doesn't get them
            // until a later lock uploads that copy, so the model follows the copy and checks
            // the page only after a writable lock of the same level)
            for y in y0..y1 {
                for x in x0..x1 {
                    want[face as usize][level as usize][(y * e + x) as usize] = px(x, y);
                }
            }
            if !read_only {
                pump();
                let p = page_cubes(from, edge);
                prop_assert_eq!(p.len(), 1);
                let got: Vec<u32> = p[0][(level * 6 + face) as usize].iter().map(|&v| v as u32).collect();
                let w = &want[face as usize][level as usize];
                // the locked rectangle is on the page as written
                for y in y0..y1 {
                    for x in x0..x1 {
                        prop_assert_eq!(got[(y * e + x) as usize], w[(y * e + x) as usize], "face {} level {} ({}, {})", face, level, x, y);
                    }
                }
            }
        }
        prop_assert!(errors().is_empty());
        call(IF_CUBE, 2, &[cube]);
        gpu::set_pump(None);
    }
}

#[test]
fn direct3d8_cube_textures() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    on_low_stack(direct3d8_cubes);
}

fn direct3d8_cubes() {
    const V8: u32 = 16;
    init();
    attach_page();
    let o = out();
    assert_eq!(unsafe { vnhost_d3d9_call(d3d8::CREATE, [o].as_ptr(), &CALLS) }, 0);
    let d3d = rd(o);
    let pp = unsafe { alloc(52) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9 + V8, 15, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o);
    errors();
    // CreateCubeTexture(8, 2 levels, 0, A8R8G8B8, MANAGED)
    let o = out();
    assert_eq!(call(IF_DEVICE + V8, 22, &[dev, 8, 2, 0, super::format::A8R8G8B8, 1, o]), 0);
    let cube = rd(o);
    // D3DSURFACE_DESC8: Format, Type, Usage, Pool, Size, MultiSampleType, Width, Height
    let desc = unsafe { alloc(32) as u32 };
    assert_eq!(call(IF_CUBE + V8, 14, &[cube, 1, desc]), 0);
    assert_eq!((0..8).map(|i| rd(desc + 4 * i)).collect::<Vec<_>>(), [21, 1, 0, 1, 64, 0, 4, 4], "level 1");
    let o = out();
    assert_eq!(call(IF_CUBE + V8, 15, &[cube, 3, 1, o]), 0, "GetCubeMapSurface");
    assert_ne!(rd(o), 0);
    let lr = unsafe { alloc(8) as u32 };
    assert_eq!(call(IF_CUBE + V8, 16, &[cube, 5, 0, lr, 0, 0]), 0, "LockRect");
    assert_eq!(rd(lr), 32);
    assert_eq!(call(IF_CUBE + V8, 17, &[cube, 5, 0]), 0, "UnlockRect");
    call(IF_DEVICE + V8, 61, &[dev, 0, cube]);
    let e = errors();
    assert!(e.is_empty(), "{:?}", &e[..e.len().min(8)]);
    call(IF_CUBE + V8, 2, &[cube]);
    gpu::set_pump(None);
}
