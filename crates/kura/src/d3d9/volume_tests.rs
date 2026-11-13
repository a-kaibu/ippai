//! Direct3D 9 volume textures (#107) on the model of WebGPU (render/model.rs): creation and
//! its descriptions, LockBox's pitches and boxes, the slices reaching the page's 3D texture,
//! UpdateTexture from system memory, and draws of a ps_2_0 shader with dcl_volume being valid
//! WebGPU (a texture_3d binding, a 3D view; white when a 2D texture is set instead). The
//! Direct3D 8 interfaces read the same through d3d8.rs (D3DVOLUME_DESC8 with Size).

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, on_low_stack, CALLS, SERIAL};
use super::*;
use crate::render::model;

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

/// A pre-transformed quad over the target with 3D texture coordinates (XYZRHW | TEX1 |
/// TEXCOORDSIZE3(0)): z of the coordinates `w`.
fn quad(dev: u32, w: f32) {
    let v = unsafe { alloc(112) as u32 };
    let mut b = Vec::new();
    for (x, y) in [(0f32, 0f32), (64.0, 0.0), (0.0, 64.0), (64.0, 64.0)] {
        for f in [x - 0.5, y - 0.5, 0.5, 1.0, x / 64.0, y / 64.0, w] {
            b.extend(f.to_le_bytes());
        }
    }
    put_bytes(v, &b);
    call(IF_DEVICE, 89, &[dev, 0x4 | 0x100 | 0x10000]);
    call(IF_DEVICE, 41, &[dev]);
    assert_eq!(call(IF_DEVICE, 83, &[dev, 5, 2, v, 28]), 0);
    call(IF_DEVICE, 42, &[dev]);
    gpu::flush();
    gpu::pump();
}

fn errors() -> Vec<String> {
    model::with(|m| std::mem::take(&mut m.errors))
}

fn words(w: &[u32]) -> u32 {
    let p = unsafe { alloc(4 * w.len() as u32) as u32 };
    put_bytes(p, &w.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    p
}

/// The page's 3D textures of size `w`x`h`x`d` made since `from`: their texels per (mip, z).
fn page_3d(from: usize, w: u32, h: u32, d: u32) -> Vec<Vec<Vec<u64>>> {
    model::with(|m| {
        m.objs[from..]
            .iter()
            .filter_map(|o| match o {
                model::Obj::Texture(t) if t.dim == "3d" && t.w == w && t.h == h && t.layers == d => Some(t.texels.clone()),
                _ => None,
            })
            .collect()
    })
}

/// texel (x, y, z) of the test's volumes: A8R8G8B8 with z in red, y in green, x in blue
fn texel(x: u32, y: u32, z: u32) -> u32 {
    0xff00_0000 | (z * 40 + 7) << 16 | (y * 20 + 3) << 8 | (x * 20 + 1)
}

/// Fills box (left, top, front)..(right, bottom, back) of a locked level.
fn fill(lb: u32, b: [u32; 6]) {
    let (row, slice, bits) = (rd(lb), rd(lb + 4), rd(lb + 8));
    for z in b[2]..b[5] {
        for y in b[1]..b[4] {
            let px: Vec<u8> = (b[0]..b[3]).flat_map(|x| texel(x, y, z).to_le_bytes()).collect();
            put_bytes(bits + (z - b[2]) * slice + (y - b[1]) * row, &px);
        }
    }
}

/// What the page's texels of A8R8G8B8 texel `t` are (bgra8unorm: the same bytes).
fn page(t: u32) -> u64 {
    t as u64
}

#[test]
fn volume_textures_reach_the_page_and_draw() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let dev = device();
    errors();
    use super::format as f;
    // the caps say so (D3DPTEXTURECAPS_VOLUMEMAP)
    let caps = unsafe { alloc(304) as u32 };
    assert_eq!(call(IF_DEVICE, 7, &[dev, caps]), 0);
    assert_ne!(rd(caps + 15 * 4) & 0x2000, 0, "TextureCaps VOLUMEMAP");
    // invalid: zero sizes, render targets, depth formats
    for (w, h, dd, usage, fmt) in [(0, 4, 4, 0, f::A8R8G8B8), (4, 4, 0, 0, f::A8R8G8B8), (4, 4, 4, 1, f::A8R8G8B8), (4, 4, 4, 0, 80)] {
        let o = out();
        assert_eq!(call(IF_DEVICE, 24, &[dev, w, h, dd, 1, usage, fmt, 1, o, 0]), 0x8876086c, "{w}x{h}x{dd} usage {usage} format {fmt}");
        assert_eq!(rd(o), 0);
    }
    // 8x8x4, all levels (4: 8x8x4, 4x4x2, 2x2x1, 1x1x1), managed
    let from = model::with(|m| m.objs.len());
    let o = out();
    assert_eq!(call(IF_DEVICE, 24, &[dev, 8, 8, 4, 0, 0, f::A8R8G8B8, 1, o, 0]), 0);
    let vt = rd(o);
    assert_eq!(call(IF_VOLUMETEX, 13, &[vt]), 4, "GetLevelCount");
    assert_eq!(call(IF_VOLUMETEX, 10, &[vt]), 4, "GetType: D3DRTYPE_VOLUMETEXTURE");
    for (l, (w, h, dd)) in [(8, 8, 4), (4, 4, 2), (2, 2, 1), (1, 1, 1)].into_iter().enumerate() {
        let desc = unsafe { alloc(32) as u32 };
        assert_eq!(call(IF_VOLUMETEX, 17, &[vt, l as u32, desc]), 0);
        let got: Vec<u32> = (0..7).map(|i| rd(desc + 4 * i)).collect();
        assert_eq!(got, [f::A8R8G8B8, 2, 0, 1, w, h, dd], "level {l} desc");
    }
    assert_eq!(call(IF_VOLUMETEX, 17, &[vt, 4, out()]), 0x8876086c, "level past the last");
    // the level's volume: its container, LockBox's pitches
    let o = out();
    assert_eq!(call(IF_VOLUMETEX, 18, &[vt, 0, o]), 0);
    let vol = rd(o);
    let iid_vt = words(&[0x2518526c, 0x4789793f, 0x8a7b0884, 0x6c66a6a0]);
    let o = out();
    assert_eq!(call(IF_VOLUME, 7, &[vol, iid_vt, o]), 0, "GetContainer");
    assert_eq!(rd(o), vt);
    call(IF_VOLUMETEX, 2, &[vt]);
    let lb = unsafe { alloc(12) as u32 };
    assert_eq!(call(IF_VOLUME, 9, &[vol, lb, 0, 0]), 0);
    assert_eq!((rd(lb), rd(lb + 4)), (32, 256), "RowPitch, SlicePitch");
    assert_eq!(call(IF_VOLUME, 9, &[vol, lb, 0, 0]), 0x8876086c, "locked twice");
    fill(lb, [0, 0, 0, 8, 8, 4]);
    assert_eq!(call(IF_VOLUME, 10, &[vol]), 0);
    assert_eq!(call(IF_VOLUME, 10, &[vol]), 0x8876086c, "unlocked twice");
    gpu::flush();
    gpu::pump();
    let p = page_3d(from, 8, 8, 4);
    assert_eq!(p.len(), 1, "one 8x8x4 page texture");
    for z in 0..4 {
        let want: Vec<u64> = (0..64).map(|i| page(texel(i % 8, i / 8, z))).collect();
        assert_eq!(p[0][z as usize], want, "slice {z}");
    }
    // a box: Left 2, Top 1, Right 6, Bottom 3, Front 1, Back 3 (pBits at its corner)
    let bx = words(&[2, 1, 6, 3, 1, 3]);
    let full = unsafe { alloc(12) as u32 };
    assert_eq!(call(IF_VOLUME, 9, &[vol, full, 0, 0x10]), 0);
    let base = rd(full + 8);
    assert_eq!(call(IF_VOLUME, 10, &[vol]), 0);
    assert_eq!(call(IF_VOLUME, 9, &[vol, lb, bx, 0]), 0);
    assert_eq!(rd(lb + 8), base + 256 + 32 + 8, "pBits of the box");
    let (row, slice, bits) = (rd(lb), rd(lb + 4), rd(lb + 8));
    for z in 0..2 {
        for y in 0..2 {
            put_bytes(bits + z * slice + y * row, &[0x55u8; 16]);
        }
    }
    assert_eq!(call(IF_VOLUME, 10, &[vol]), 0);
    gpu::flush();
    gpu::pump();
    let p = page_3d(from, 8, 8, 4);
    for z in 0..4u32 {
        let want: Vec<u64> = (0..64u32)
            .map(|i| {
                let (x, y) = (i % 8, i / 8);
                if (2..6).contains(&x) && (1..3).contains(&y) && (1..3).contains(&z) { 0x5555_5555 } else { page(texel(x, y, z)) }
            })
            .collect();
        assert_eq!(p[0][z as usize], want, "slice {z} after the box");
    }
    // boxes outside the level, empty ones
    for b in [[0, 0, 9, 8, 0, 4], [0, 0, 8, 9, 0, 4], [0, 0, 8, 8, 0, 5], [4, 0, 4, 8, 0, 4], [0, 0, 8, 8, 2, 2]] {
        assert_eq!(call(IF_VOLUME, 9, &[vol, lb, words(&b), 0]), 0x8876086c, "box {b:?}");
    }
    let desc = unsafe { alloc(28) as u32 };
    assert_eq!(call(IF_VOLUME, 8, &[vol, desc]), 0);
    assert_eq!((0..7).map(|i| rd(desc + 4 * i)).collect::<Vec<_>>(), [f::A8R8G8B8, 2, 0, 1, 8, 8, 4], "GetDesc: D3DRTYPE_VOLUME");
    // UpdateTexture: a system memory volume texture into a default pool one
    let o = out();
    assert_eq!(call(IF_DEVICE, 24, &[dev, 4, 4, 2, 1, 0, f::A8R8G8B8, 2, o, 0]), 0);
    let sys = rd(o);
    let from = model::with(|m| m.objs.len());
    let o = out();
    assert_eq!(call(IF_DEVICE, 24, &[dev, 4, 4, 2, 1, 0, f::A8R8G8B8, 0, o, 0]), 0);
    let def = rd(o);
    gpu::flush();
    gpu::pump();
    assert_eq!(page_3d(from, 4, 4, 2).len(), 1, "the default pool texture is on the page");
    assert_eq!(call(IF_VOLUMETEX, 19, &[sys, 0, lb, 0, 0]), 0);
    fill(lb, [0, 0, 0, 4, 4, 2]);
    assert_eq!(call(IF_VOLUMETEX, 20, &[sys, 0]), 0);
    assert_eq!(call(IF_DEVICE, 31, &[dev, sys, def]), 0);
    gpu::flush();
    gpu::pump();
    let p = page_3d(from, 4, 4, 2);
    for z in 0..2 {
        let want: Vec<u64> = (0..16).map(|i| page(texel(i % 4, i / 4, z))).collect();
        assert_eq!(p[0][z as usize], want, "UpdateTexture slice {z}");
    }
    assert_eq!(call(IF_DEVICE, 31, &[dev, def, sys]), 0x8876086c, "default pool into system memory");
    assert_eq!(call(IF_DEVICE, 31, &[dev, sys, vt]), 0x8876086c, "sizes differ");
    let e = errors();
    assert!(e.is_empty(), "volumes: {:?}", &e[..e.len().min(8)]);
    // ps_2_0: dcl t0.xyz; dcl_volume s0; texld r0, t0, s0; mov oC0, r0
    let code = words(&[0xffff0200, 0x0200001f, 0x80000000, 0xb0070000, 0x0200001f, 0xa0000000, 0xa00f0800, 0x03000042, 0x800f0000, 0xb0e40000, 0xa0e40800, 0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff]);
    let o = out();
    assert_eq!(call(IF_DEVICE, 106, &[dev, code, o]), 0);
    let ps = rd(o);
    call(IF_DEVICE, 107, &[dev, ps]);
    // (every address mode in W, linear filtering, mips)
    let from = model::with(|m| m.objs.len());
    for (am, filt) in [(1, 2), (2, 1), (3, 2), (4, 2), (5, 1)] {
        for (ss, v) in [(3, am), (5, filt), (6, filt), (7, filt)] {
            call(IF_DEVICE, 69, &[dev, 0, ss, v]);
        }
        call(IF_DEVICE, 65, &[dev, 0, vt]);
        quad(dev, 0.4);
    }
    // the bind groups made: binding 0 a 3D view of the 8x8x4 texture (not the white stand-in)
    let views = model::with(|m| {
        let mut views = Vec::new();
        for o in &m.objs[from..] {
            if let model::Obj::Group(_, entries) = o {
                for (b, e) in entries {
                    if let (0, model::Entry::View(v)) = (b, e) {
                        let model::Obj::View(v) = &m.objs[*v] else { continue };
                        let model::Obj::Texture(t) = &m.objs[v.tex] else { continue };
                        views.push((v.dim.clone(), t.w, t.h, t.layers));
                    }
                }
            }
        }
        views
    });
    assert!(!views.is_empty() && views.iter().all(|v| *v == ("3d".to_string(), 8, 8, 4)), "{views:?}");
    // a 2D texture where the shader samples a volume, nothing set: white, still valid
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, 4, 4, 1, 0, f::A8R8G8B8, 1, o, 0]), 0);
    let t2 = rd(o);
    call(IF_DEVICE, 65, &[dev, 0, t2]);
    quad(dev, 0.0);
    call(IF_DEVICE, 65, &[dev, 0, 0]);
    quad(dev, 0.0);
    // and the volume where a 2D texture is sampled (fixed function)
    call(IF_DEVICE, 107, &[dev, 0]);
    call(IF_DEVICE, 65, &[dev, 0, vt]);
    quad(dev, 0.0);
    call(IF_DEVICE, 65, &[dev, 0, 0]);
    let e = errors();
    assert!(e.is_empty(), "draws: {:?}", &e[..e.len().min(8)]);
    // the pipelines' group 1 layouts: a 3D binding 0 for the shader
    let dims = model::with(|m| {
        m.objs
            .iter()
            .filter_map(|o| match o {
                model::Obj::Layout(entries) => entries.iter().find(|e| e.binding == 0 && e.kind == 't').map(|e| e.view_dim.clone()),
                _ => None,
            })
            .filter(|d| d == "3d")
            .count()
    });
    assert!(dims >= 1, "a group layout with a 3D texture binding");
    call(IF_VOLUMETEX, 2, &[sys]);
    call(IF_VOLUMETEX, 2, &[def]);
    call(IF_VOLUMETEX, 2, &[vt]);
    call(IF_TEXTURE, 2, &[t2]);
    gpu::set_pump(None);
}

#[test]
fn direct3d8_volume_textures() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    // (d3d8.rs passes its Direct3D 9 structures on the stack: one guest pointers reach)
    on_low_stack(direct3d8_volumes);
}

fn direct3d8_volumes() {
    const V8: u32 = 16;
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
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
    // CreateVolumeTexture(8, 8, 4, 2 levels, 0, A8R8G8B8, MANAGED)
    let o = out();
    assert_eq!(call(IF_DEVICE + V8, 21, &[dev, 8, 8, 4, 2, 0, super::format::A8R8G8B8, 1, o]), 0);
    let vt = rd(o);
    // D3DVOLUME_DESC8: Format, Type, Usage, Pool, Size, Width, Height, Depth
    for (l, want) in [(0, [21, 2, 0, 1, 1024, 8, 8, 4]), (1, [21, 2, 0, 1, 128, 4, 4, 2])] {
        let desc = unsafe { alloc(32) as u32 };
        assert_eq!(call(IF_VOLUMETEX + V8, 14, &[vt, l, desc]), 0);
        assert_eq!((0..8).map(|i| rd(desc + 4 * i)).collect::<Vec<_>>(), want, "level {l}");
    }
    let o = out();
    assert_eq!(call(IF_VOLUMETEX + V8, 15, &[vt, 1, o]), 0, "GetVolumeLevel");
    let vol = rd(o);
    let desc = unsafe { alloc(32) as u32 };
    assert_eq!(call(IF_VOLUME + V8, 8, &[vol, desc]), 0);
    assert_eq!((0..8).map(|i| rd(desc + 4 * i)).collect::<Vec<_>>(), [21, 2, 0, 1, 128, 4, 4, 2], "IDirect3DVolume8::GetDesc");
    let lb = unsafe { alloc(12) as u32 };
    assert_eq!(call(IF_VOLUMETEX + V8, 16, &[vt, 1, lb, 0, 0]), 0, "LockBox");
    assert_eq!((rd(lb), rd(lb + 4)), (16, 64));
    assert_eq!(call(IF_VOLUMETEX + V8, 17, &[vt, 1]), 0, "UnlockBox");
    assert_eq!(call(IF_VOLUME + V8, 9, &[vol, lb, 0, 0]), 0, "IDirect3DVolume8::LockBox");
    assert_eq!(call(IF_VOLUME + V8, 10, &[vol]), 0);
    call(IF_DEVICE + V8, 61, &[dev, 0, vt]);
    let e = errors();
    assert!(e.is_empty(), "{:?}", &e[..e.len().min(8)]);
    call(IF_VOLUMETEX + V8, 2, &[vt]);
    gpu::set_pump(None);
}
