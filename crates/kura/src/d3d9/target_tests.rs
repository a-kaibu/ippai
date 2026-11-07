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
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 1, 75, 0, 0, 0].into_iter().enumerate() {
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
                model::Obj::Pipeline(p) if !p.clear_shader && !p.buffers.is_empty() => Some(p.targets.iter().map(|t| (t.0.clone(), t.1)).collect()),
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
        for (rs, v) in [(52, 1), (56, func), (55, op), (53, 9 - op), (54, op), (57, k as u32), (58, 0x0f), (59, 0xf0), (185, (k & 1) as u32), (189, 9 - func), (188, op), (7, (k & 2 != 0) as u32)] {
            call(IF_DEVICE, 57, &[dev, rs, v]);
        }
        // (a stencil clear in a part, then the whole)
        let r = unsafe { alloc(16) as u32 };
        put_bytes(r, &[4u32, 4, 20, 20].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
        assert_eq!(call(IF_DEVICE, 43, &[dev, 1, r, 4, 0, 1f32.to_bits(), 3]), 0);
        quad(dev);
        assert_eq!(call(IF_DEVICE, 43, &[dev, 0, 0, 7, 0, 1f32.to_bits(), 1]), 0);
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
    let code: [u32; 8] = [0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0x02000001, 0x800f0801, 0xa0e40001, 0x0000ffff];
    let c = unsafe { alloc(32) as u32 };
    put_bytes(c, &code.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    let o = out();
    assert_eq!(call(IF_DEVICE, 106, &[dev, c, o]), 0);
    let ps = rd(o);
    let from = model::with(|m| m.objs.len());
    assert_eq!(call(IF_DEVICE, 43, &[dev, 0, 0, 7, 0x00ff_0000, 1f32.to_bits(), 0]), 0);
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
    assert_eq!(t[1][1].1, 0, "fixed function writes no render target 1: {t:?}");
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
