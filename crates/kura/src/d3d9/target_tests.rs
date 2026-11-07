//! Draws to more render targets and with the stencil test, on the model of WebGPU
//! (render/model.rs): the pipelines and passes they make must be valid WebGPU (formats, write
//! masks, depth-stencil state) and have the targets Direct3D 9 draws to.

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, CALLS, SERIAL};
use super::*;
use crate::render::model;

/// A 64x64 device with a D24S8 depth buffer (software vertex processing).
fn device() -> u32 {
    attach_page();
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 1, 75, 0, 0, 0]
        .into_iter()
        .enumerate()
    {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    rd(o)
}

/// A pre-transformed quad over the target (XYZRHW | DIFFUSE).
fn quad(dev: u32) {
    let v = unsafe { alloc(80) as u32 };
    let mut b = Vec::new();
    for (x, y) in [(0f32, 0f32), (64.0, 0.0), (0.0, 64.0), (64.0, 64.0)] {
        for f in [x - 0.5, y - 0.5, 0.5, 1.0] {
            b.extend(f.to_le_bytes());
        }
        b.extend(0xff80_4020u32.to_le_bytes());
    }
    put_bytes(v, &b);
    call(IF_DEVICE, 89, &[dev, 0x44]);
    call(IF_DEVICE, 41, &[dev]);
    assert_eq!(call(IF_DEVICE, 83, &[dev, 5, 2, v, 20]), 0);
    call(IF_DEVICE, 42, &[dev]);
    gpu::flush();
    gpu::pump();
}

fn errors() -> Vec<String> {
    model::with(|m| std::mem::take(&mut m.errors))
}

/// The color targets (format, write mask) of the pipelines made since `from`.
fn targets(from: usize) -> Vec<Vec<(String, u32)>> {
    model::with(|m| {
        m.objs[from..]
            .iter()
            .filter_map(|o| match o {
                model::Obj::Pipeline(p) if !p.clear_shader && !p.buffers.is_empty() => {
                    Some(p.targets.iter().map(|t| (t.0.clone(), t.1)).collect())
                }
                _ => None,
            })
            .collect()
    })
}

#[test]
fn stencil_and_more_render_targets_are_valid_webgpu() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let dev = device();
    errors();
    // the stencil test, one- and two-sided, every function and operation; with the depth
    // test off too (stencil still tests)
    for (k, (func, op)) in (1..=8).zip([1, 2, 3, 4, 5, 6, 7, 8]).enumerate() {
        for (rs, v) in [
            (52, 1),
            (56, func),
            (55, op),
            (53, 9 - op),
            (54, op),
            (57, k as u32),
            (58, 0x0f),
            (59, 0xf0),
            (185, (k & 1) as u32),
            (189, 9 - func),
            (188, op),
            (7, (k & 2 != 0) as u32),
        ] {
            call(IF_DEVICE, 57, &[dev, rs, v]);
        }
        // (a stencil clear in a part, then the whole)
        let r = unsafe { alloc(16) as u32 };
        put_bytes(
            r,
            &[4u32, 4, 20, 20]
                .iter()
                .flat_map(|v| v.to_le_bytes())
                .collect::<Vec<_>>(),
        );
        assert_eq!(
            call(IF_DEVICE, 43, &[dev, 1, r, 4, 0, 1f32.to_bits(), 3]),
            0
        );
        quad(dev);
        assert_eq!(
            call(IF_DEVICE, 43, &[dev, 0, 0, 7, 0, 1f32.to_bits(), 1]),
            0
        );
        quad(dev);
    }
    call(IF_DEVICE, 57, &[dev, 52, 0]);
    let e = errors();
    assert!(e.is_empty(), "stencil: {:?}", &e[..e.len().min(8)]);
    // a second render target (A8R8G8B8 texture), a pixel shader writing oC0 and oC1, then
    // fixed function with both set (oC1 not written: no writes to it)
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, 64, 64, 1, 1, 21, 0, o, 0]), 0);
    let o2 = out();
    assert_eq!(call(IF_TEXTURE, 18, &[rd(o), 0, o2]), 0);
    let s1 = rd(o2);
    assert_eq!(call(IF_DEVICE, 37, &[dev, 1, s1]), 0);
    let code: [u32; 8] = [
        0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0x02000001, 0x800f0801, 0xa0e40001,
        0x0000ffff,
    ];
    let c = unsafe { alloc(32) as u32 };
    put_bytes(
        c,
        &code
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<_>>(),
    );
    let o = out();
    assert_eq!(call(IF_DEVICE, 106, &[dev, c, o]), 0);
    let ps = rd(o);
    let from = model::with(|m| m.objs.len());
    assert_eq!(
        call(
            IF_DEVICE,
            43,
            &[dev, 0, 0, 7, 0x00ff_0000, 1f32.to_bits(), 0]
        ),
        0
    );
    call(IF_DEVICE, 107, &[dev, ps]);
    call(IF_DEVICE, 57, &[dev, 190, 1]);
    quad(dev);
    call(IF_DEVICE, 107, &[dev, 0]);
    quad(dev);
    // and with the stencil test
    call(IF_DEVICE, 57, &[dev, 52, 1]);
    call(IF_DEVICE, 107, &[dev, ps]);
    quad(dev);
    let e = errors();
    assert!(e.is_empty(), "two targets: {:?}", &e[..e.len().min(8)]);
    let t = targets(from);
    assert!(t.len() >= 3, "{t:?}");
    for p in &t {
        assert_eq!(p.len(), 2, "{t:?}");
    }
    assert_eq!(t[0][1].1, 1, "COLORWRITEENABLE1 red: {t:?}");
    assert_eq!(
        t[1][1].1, 0,
        "fixed function writes no render target 1: {t:?}"
    );
    // render target 1 unset: one target again
    call(IF_DEVICE, 37, &[dev, 1, 0]);
    let from = model::with(|m| m.objs.len());
    quad(dev);
    let t = targets(from);
    assert!(t.iter().all(|p| p.len() == 1), "{t:?}");
    let e = errors();
    assert!(e.is_empty(), "one target again: {:?}", &e[..e.len().min(8)]);
    gpu::set_pump(None);
}

/// Every D3D9 texture format reaches its page texture (format::convert's texels; 32-bit floats as
/// halves on a page without float32-filterable).
#[test]
fn every_texture_format_reaches_the_page() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let dev = device();
    errors();
    use super::format as f;
    let fmts = [
        f::A8R8G8B8,
        f::X8R8G8B8,
        f::R5G6B5,
        f::A1R5G5B5,
        f::A4R4G4B4,
        f::A8,
        f::L8,
        f::A8L8,
        f::A8B8G8R8,
        f::A16B16G16R16F,
        f::A32B32G32R32F,
        f::A2R10G10B10,
        f::A2B10G10R10,
        f::G16R16,
        f::A16B16G16R16,
        f::L16,
        f::V8U8,
        f::Q8W8V8U8,
        f::V16U16,
        f::Q16W16V16U16,
        f::R16F,
        f::G16R16F,
        f::R32F,
        f::G32R32F,
    ];
    // (those Windows has no textures of)
    for fmt in [f::X8L8V8U8, f::L6V5U5, f::A2W10V10U10] {
        assert_eq!(
            call(IF_DEVICE, 23, &[dev, 4, 4, 1, 0, fmt, 1, out(), 0]),
            0x8876086c,
            "format {fmt}"
        );
    }
    for fmt in fmts {
        let from = model::with(|m| m.objs.len());
        let o = out();
        assert_eq!(
            call(IF_DEVICE, 23, &[dev, 4, 4, 1, 0, fmt, 1, o, 0]),
            0,
            "format {fmt}"
        );
        let tex = rd(o);
        let lr = out();
        assert_eq!(call(IF_TEXTURE, 19, &[tex, 0, lr, 0, 0]), 0, "format {fmt}");
        let (pitch, bits) = (rd(lr), rd(lr + 4));
        let (bpp, _) = f::layout(fmt).unwrap();
        // (values that are normal floats of every width, 32-bit ones within reach of halves:
        // the top byte of each 4 stays 0x3c)
        let mut src = Vec::new();
        for y in 0..4u32 {
            let row: Vec<u8> = (0..4 * bpp)
                .map(|i| {
                    [0x05, 0x21, 0x3e, 0x3c][(i % 4) as usize]
                        ^ if i % 4 == 3 {
                            0
                        } else {
                            (y * 4 * bpp + i) as u8 & 0x11
                        }
                })
                .collect();
            put_bytes(bits + y * pitch, &row);
            src.extend(row);
        }
        assert_eq!(call(IF_TEXTURE, 20, &[tex, 0]), 0);
        call(IF_DEVICE, 65, &[dev, 0, tex]);
        quad(dev);
        call(IF_DEVICE, 65, &[dev, 0, 0]);
        let mut want = Vec::new();
        unsafe { f::convert(fmt, src.as_ptr(), (4 * bpp) as usize, 4, 4, &mut want) };
        let got = model::with(|m| {
            m.objs[from..].iter().find_map(|o| match o {
                model::Obj::Texture(t) if t.w == 4 && t.h == 4 => {
                    Some((t.format.clone(), t.texels[0].clone()))
                }
                _ => None,
            })
        });
        let Some((pf, texels)) = got else {
            panic!("format {fmt}: no page texture")
        };
        assert!(
            texels.iter().all(|&t| t != 0),
            "format {fmt} ({pf}): texels not written {texels:x?}"
        );
        let page = f::page_bpp(fmt) as usize;
        let want: Vec<u64> = match pf.as_str() {
            // (halves of the 32-bit floats when the page has no float32-filterable)
            "rgba16float" if page == 16 => want
                .chunks_exact(16)
                .map(|p| {
                    let h: Vec<u8> = p
                        .chunks_exact(4)
                        .flat_map(|f| {
                            crate::d3d11::format::f16_bits(f32::from_le_bytes(
                                f.try_into().unwrap(),
                            ))
                            .to_le_bytes()
                        })
                        .collect();
                    u64::from_le_bytes(h.try_into().unwrap())
                })
                .collect(),
            _ if page <= 8 => want
                .chunks_exact(page)
                .map(|p| {
                    let mut w = [0u8; 8];
                    w[..page].copy_from_slice(p);
                    u64::from_le_bytes(w)
                })
                .collect(),
            _ => texels.clone(), // (16-byte texels are hashed in the model)
        };
        assert_eq!(texels, want, "format {fmt} ({pf})");
        call(IF_TEXTURE, 2, &[tex]);
    }
    let e = errors();
    assert!(e.is_empty(), "{:?}", &e[..e.len().min(8)]);
    gpu::set_pump(None);
}

/// Clear as Windows answers it (tests/winref/golden/d3d9clear-32), and a depth buffer smaller
/// than the target in use (the pipelines test depth, the passes are valid WebGPU).
#[test]
fn clear_answers_and_a_smaller_depth_buffer() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let dev = device();
    errors();
    const INVALIDCALL: u32 = 0x8876086c;
    let clear = |count: u32, rects: u32, flags: u32| {
        call(
            IF_DEVICE,
            43,
            &[dev, count, rects, flags, 0x102030, 1f32.to_bits(), 0x105],
        )
    };
    let r = unsafe { alloc(16) as u32 };
    put_bytes(
        r,
        &[4u32, 4, 8, 8]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect::<Vec<u8>>(),
    );
    assert_eq!(clear(0, 0, 0), INVALIDCALL, "no flags");
    assert_eq!(clear(0, 0, 0x10), INVALIDCALL, "no known flags");
    assert_eq!(clear(0, 0, 0x11), 0);
    assert_eq!(clear(1, 0, 1), 0, "a count without a list");
    assert_eq!(clear(0, r, 1), 0, "a list with a count of 0");
    assert_eq!(clear(1, r, 7), 0, "D24S8");
    // (each with its own depth function: a pipeline of its own)
    for (fmt, w, stencil, func) in [
        (format::D24X8, 64, false, 4),
        (format::D16, 32, false, 2),
        (format::D24S8, 32, true, 8),
    ] {
        let o = out();
        assert_eq!(call(IF_DEVICE, 29, &[dev, w, w, fmt, 0, 0, 0, o, 0]), 0);
        let z = rd(o);
        assert_eq!(call(IF_DEVICE, 39, &[dev, z]), 0);
        assert_eq!(clear(0, 0, 2), 0, "z {fmt}");
        assert_eq!(
            clear(0, 0, 4),
            if stencil { 0 } else { INVALIDCALL },
            "stencil {fmt}"
        );
        assert_eq!(
            clear(0, 0, 5),
            if stencil { 0 } else { INVALIDCALL },
            "target and stencil {fmt}"
        );
        let from = model::with(|m| m.objs.len());
        call(IF_DEVICE, 57, &[dev, 7, 1]);
        call(IF_DEVICE, 57, &[dev, 23, func]);
        quad(dev);
        let depth = model::with(|m| {
            m.objs[from..].iter().any(
                |o| matches!(o, model::Obj::Pipeline(p) if p.depth.is_some() && !p.clear_shader),
            )
        });
        assert!(depth, "{fmt} {w}x{w}: the draw tests depth");
        let e = errors();
        assert!(e.is_empty(), "{fmt} {w}x{w}: {:?}", &e[..e.len().min(8)]);
        call(IF_DEVICE, 39, &[dev, 0]);
        call(IF_SURFACE, 2, &[z]);
    }
    gpu::set_pump(None);
}

/// Render targets of the formats Windows also renders to (A4R4G4B4, A8, L8, A8L8, A2R10G10B10,
/// R32F, V8U8, and R5G6B5's kind): valid WebGPU pipelines writing only the channels the format
/// has, a clear that leaves the others as the format reads them, and GetRenderTargetData giving
/// the format's texels (format::pack of the page's).
#[test]
fn render_targets_of_fewer_channels() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let dev = device();
    errors();
    use super::format as f;
    let o = out();
    assert_eq!(call(IF_DEVICE, 18, &[dev, 0, 0, 0, o]), 0); // GetBackBuffer
    let back = rd(o);
    // (format, its channels' write mask, the clear of 0x80406020 read back)
    let cases: [(u32, u32, &[u8]); 11] = [
        (f::A8R8G8B8, 0xf, &[0x20, 0x60, 0x40, 0x80]),
        (f::R5G6B5, 0xf, &(8u16 << 11 | 24 << 5 | 4).to_le_bytes()),
        (
            f::A1R5G5B5,
            0xf,
            &(1u16 << 15 | 8 << 10 | 12 << 5 | 4).to_le_bytes(),
        ),
        (f::A4R4G4B4, 0xf, &0x8462u16.to_le_bytes()),
        (f::A8, 8, &[0x80]),
        (f::L8, 1, &[0x40]),
        (f::A8L8, 9, &[0x40, 0x80]),
        (f::A8B8G8R8, 0xf, &[0x40, 0x60, 0x20, 0x80]),
        (
            f::A2R10G10B10,
            0xf,
            &(2u32 << 30 | 257 << 20 | 385 << 10 | 128).to_le_bytes(),
        ),
        (f::R32F, 1, &[]),
        (f::V8U8, 3, &[0x20, 0x30]),
    ];
    call(IF_DEVICE, 57, &[dev, 27, 1]); // ALPHABLENDENABLE
    for (i, (fmt, mask, want)) in cases.into_iter().enumerate() {
        let o = out();
        assert_eq!(
            call(IF_DEVICE, 23, &[dev, 64, 64, 1, 1, fmt, 0, o, 0]),
            0,
            "format {fmt}"
        );
        let tex = rd(o);
        let o = out();
        assert_eq!(call(IF_TEXTURE, 18, &[tex, 0, o]), 0);
        let rt = rd(o);
        assert_eq!(call(IF_DEVICE, 37, &[dev, 0, rt]), 0, "format {fmt}");
        assert_eq!(
            call(
                IF_DEVICE,
                43,
                &[dev, 0, 0, 1, 0x8040_6020, 1f32.to_bits(), 0]
            ),
            0
        );
        let from = model::with(|m| m.objs.len());
        // (each with its own blend: a pipeline of its own)
        call(IF_DEVICE, 57, &[dev, 19, 1 + i as u32]);
        quad(dev);
        let t = targets(from);
        assert!(
            !t.is_empty() && t.iter().all(|p| p[0].1 == mask),
            "format {fmt}: {t:?}"
        );
        // the clear read back
        let o = out();
        assert_eq!(call(IF_DEVICE, 36, &[dev, 64, 64, fmt, 2, o, 0]), 0);
        let sys = rd(o);
        assert_eq!(call(IF_DEVICE, 37, &[dev, 0, back]), 0);
        assert_eq!(
            call(
                IF_DEVICE,
                43,
                &[dev, 0, 0, 1, 0x8040_6020, 1f32.to_bits(), 0]
            ),
            0
        );
        assert_eq!(call(IF_DEVICE, 37, &[dev, 0, rt]), 0);
        assert_eq!(
            call(
                IF_DEVICE,
                43,
                &[dev, 0, 0, 1, 0x8040_6020, 1f32.to_bits(), 0]
            ),
            0
        );
        assert_eq!(
            call(IF_DEVICE, 32, &[dev, rt, sys]),
            0,
            "format {fmt}: GetRenderTargetData"
        );
        let lr = out();
        assert_eq!(call(IF_SURFACE, 13, &[sys, lr, 0, 0x10]), 0);
        let bits = rd(lr + 4);
        let got: Vec<u8> = (0..want.len().max(4))
            .map(|i| unsafe { *((bits as usize + i) as *const u8) })
            .collect();
        if fmt == f::R32F {
            let v = f32::from_le_bytes(got[..4].try_into().unwrap());
            assert!((v - 64.0 / 255.0).abs() < 1e-3, "format {fmt}: {v}");
        } else {
            assert_eq!(&got[..want.len()], want, "format {fmt}");
        }
        call(IF_SURFACE, 14, &[sys]);
        assert_eq!(call(IF_DEVICE, 37, &[dev, 0, back]), 0);
        for x in [sys, rt, tex] {
            call(IF_SURFACE, 2, &[x]);
        }
        let e = errors();
        assert!(e.is_empty(), "format {fmt}: {:?}", &e[..e.len().min(8)]);
    }
    gpu::set_pump(None);
}
