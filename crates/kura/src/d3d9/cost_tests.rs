//! What a Direct3D 9 draw costs the guest thread (`MASU_D3D_COST=1 cargo test --release -p kura
//! d3d9_draw_cost -- --ignored --nocapture`): frames of the shapes games draw, timed per draw on
//! the guest side (the page is the model of WebGPU: its time is printed apart and is not the
//! browser's). Not a pass/fail test: it prints, for comparing builds.

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, CALLS, SERIAL};
use super::*;
use std::time::Instant;

fn device() -> u32 {
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [256, 256, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    rd(o)
}

fn texture(dev: u32) -> u32 {
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, 64, 64, 1, 0, 21, 1, o, 0]), 0);
    rd(o)
}

/// XYZRHW | DIFFUSE | TEX1 quad at (x, y), 8x8
fn quad(p: u32, x: f32, y: f32) {
    let mut b = Vec::new();
    for (vx, vy, u, v) in [(x, y, 0.0, 0.0), (x + 8.0, y, 1.0, 0.0), (x, y + 8.0, 0.0, 1.0), (x + 8.0, y + 8.0, 1.0, 1.0)] {
        for f in [vx, vy, 0.0f32, 1.0] {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b.extend_from_slice(&0xff80_8080u32.to_le_bytes());
        b.extend_from_slice(&(u as f32).to_le_bytes());
        b.extend_from_slice(&(v as f32).to_le_bytes());
    }
    put_bytes(p, &b);
}

const STRIDE: u32 = 28;

/// One case: `draws` draws a frame by `frame`, over several frames; prints ns per draw.
fn case(name: &str, frames: u32, draws: u32, mut frame: impl FnMut(u32)) {
    // warm-up
    frame(0);
    device::DRAWS_JOINED.store(0, std::sync::atomic::Ordering::Relaxed);
    let t = Instant::now();
    for f in 1..=frames {
        frame(f);
    }
    let el = t.elapsed();
    let joined = device::DRAWS_JOINED.swap(0, std::sync::atomic::Ordering::Relaxed);
    let per = el.as_nanos() as f64 / (frames * draws) as f64;
    println!("cost {name}: {per:.0} ns/draw ({frames} frames of {draws}, {joined} joined to the draw before)");
}

#[test]
#[ignore]
fn d3d9_draw_cost() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let page = std::env::var_os("MASU_D3D_COST_PAGE").is_some();
    if page {
        attach_page();
    }
    let dev = device();
    let tex = [texture(dev), texture(dev)];
    for (rs, v) in [(22, 1), (137, 0), (7, 0), (27, 1), (19, 5), (20, 6)] {
        call(IF_DEVICE, 57, &[dev, rs, v]);
    }
    call(IF_DEVICE, 89, &[dev, 0x144]);
    const N: u32 = 2000;
    let up = unsafe { alloc(N * 4 * STRIDE) as u32 };
    for k in 0..N {
        quad(up + k * 4 * STRIDE, (k % 30) as f32 * 8.0, (k / 30 % 30) as f32 * 8.0);
    }
    let present = || {
        call(IF_DEVICE, 42, &[dev]);
        call(IF_DEVICE, 17, &[dev, 0, 0, 0, 0]);
        if page {
            gpu::pump();
        }
        call(IF_DEVICE, 41, &[dev]);
    };
    call(IF_DEVICE, 41, &[dev]);
    // sprites: one texture, DrawPrimitiveUP each
    case("up-same", 20, N, |_| {
        call(IF_DEVICE, 65, &[dev, 0, tex[0]]);
        for k in 0..N {
            call(IF_DEVICE, 83, &[dev, 5, 2, up + k * 4 * STRIDE, STRIDE]);
        }
        present();
    });
    // sprites as engines draw them: the texture and the blend states set again before each
    case("up-engine", 20, N, |_| {
        for k in 0..N {
            call(IF_DEVICE, 65, &[dev, 0, tex[(k / 8 % 2) as usize]]);
            call(IF_DEVICE, 57, &[dev, 27, 1]);
            call(IF_DEVICE, 57, &[dev, 19, 5]);
            call(IF_DEVICE, 57, &[dev, 20, 6]);
            call(IF_DEVICE, 67, &[dev, 0, 1, 4]);
            call(IF_DEVICE, 83, &[dev, 5, 2, up + k * 4 * STRIDE, STRIDE]);
        }
        present();
    });
    // state changes before each draw (tests/bench/src/d3d9.c "state"): every draw prepares
    case("up-state", 20, N, |f| {
        for k in 0..N {
            call(IF_DEVICE, 57, &[dev, 27, (k % 3 != 0) as u32]);
            call(IF_DEVICE, 57, &[dev, 20, if k % 2 == 1 { 6 } else { 2 }]);
            call(IF_DEVICE, 69, &[dev, 0, 5, if k % 4 != 0 { 2 } else { 1 }]);
            call(IF_DEVICE, 67, &[dev, 0, 1, if k % 5 != 0 { 4 } else { 2 }]);
            call(IF_DEVICE, 65, &[dev, 0, tex[((k + f) % 2) as usize]]);
            call(IF_DEVICE, 83, &[dev, 5, 2, up + k * 4 * STRIDE, STRIDE]);
        }
        present();
    });
    // a static vertex buffer: DrawPrimitive of its quads
    let o = out();
    assert_eq!(call(IF_DEVICE, 26, &[dev, N * 4 * STRIDE, 8, 0x144, 1, o, 0]), 0);
    let vb = rd(o);
    let p = out();
    assert_eq!(call(IF_VB, 11, &[vb, 0, 0, p, 0]), 0);
    unsafe { std::ptr::copy_nonoverlapping(up as *const u8, rd(p) as *mut u8, (N * 4 * STRIDE) as usize) };
    assert_eq!(call(IF_VB, 12, &[vb]), 0);
    call(IF_DEVICE, 100, &[dev, 0, vb, 0, STRIDE]);
    case("vb-static", 20, N, |_| {
        call(IF_DEVICE, 65, &[dev, 0, tex[0]]);
        for k in 0..N {
            call(IF_DEVICE, 81, &[dev, 5, k * 4, 2]);
        }
        present();
    });
    call(IF_DEVICE, 42, &[dev]);
    gpu::set_pump(None);
}
