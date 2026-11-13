//! The fixed-function texture stages' temporary register: D3DPMISCCAPS_TSSARGTEMP is reported
//! (ダンジョンズ＆シネステジア 体験版 quits with 「テクスチャステージでテンポラリレジスタが使用
//! できません」 without it), and draws with D3DTSS_RESULTARG = D3DTA_TEMP, D3DTA_TEMP arguments and
//! the texture-alpha blend ops in the alpha stage give valid WebGPU (render/model.rs validates
//! every shader module with naga). A seeded sweep draws random stage setups the same way.

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, CALLS, SERIAL};
use super::*;
use crate::render::model;

const TSSARGTEMP: u32 = 0x400;

fn d3d() -> u32 {
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    attach_page();
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    rd(o)
}

fn device(d3d: u32) -> u32 {
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 0, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    rd(o)
}

/// A pre-transformed quad (XYZRHW | DIFFUSE | TEX1) over the 64x64 target.
fn quad(dev: u32) {
    let v = unsafe { alloc(4 * 28) as u32 };
    let mut b = Vec::new();
    for (x, y) in [(0f32, 0f32), (64.0, 0.0), (0.0, 64.0), (64.0, 64.0)] {
        for f in [x - 0.5, y - 0.5, 0.5, 1.0] {
            b.extend(f.to_le_bytes());
        }
        b.extend(0xff80_4020u32.to_le_bytes());
        b.extend((x / 64.0).to_le_bytes());
        b.extend((y / 64.0).to_le_bytes());
    }
    put_bytes(v, &b);
    call(IF_DEVICE, 89, &[dev, 0x4 | 0x40 | 0x100]);
    call(IF_DEVICE, 41, &[dev]);
    assert_eq!(call(IF_DEVICE, 83, &[dev, 5, 2, v, 28]), 0);
    call(IF_DEVICE, 42, &[dev]);
    gpu::flush();
    gpu::pump();
}

fn errors() -> Vec<String> {
    model::with(|m| std::mem::take(&mut m.errors))
}

/// SetTextureStageState(stage, type, value)
fn tss(dev: u32, s: u32, t: u32, v: u32) {
    assert_eq!(call(IF_DEVICE, 67, &[dev, s, t, v]), 0);
}

#[test]
fn caps_report_tssargtemp() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let d3d = d3d();
    let caps = unsafe { alloc(304) as u32 };
    // IDirect3D9::GetDeviceCaps(adapter 0, HAL)
    assert_eq!(call(IF_D3D9, 14, &[d3d, 0, 1, caps]), 0);
    assert_ne!(rd(caps + 32) & TSSARGTEMP, 0, "PrimitiveMiscCaps {:#x}", rd(caps + 32));
    // IDirect3DDevice9::GetDeviceCaps says the same
    let dev = device(d3d);
    let c2 = unsafe { alloc(304) as u32 };
    assert_eq!(call(IF_DEVICE, 7, &[dev, c2]), 0);
    assert_ne!(rd(c2 + 32) & TSSARGTEMP, 0);
}

#[test]
fn resultarg_temp_and_alpha_blendtexturealpha_draw() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dev = device(d3d());
    errors();
    // stage 0: temp = texture (none bound: white), alpha BLENDTEXTUREALPHA (13) of its own
    // texture; stage 1: current = diffuse * temp; stage 2 off
    for (s, t, v) in [(0, 1, 2), (0, 2, 2), (0, 4, 13), (0, 5, 2), (0, 6, 0), (0, 28, 5), (1, 1, 4), (1, 2, 0), (1, 3, 5), (1, 4, 2), (1, 5, 0), (1, 28, 1), (2, 1, 1)] {
        tss(dev, s, t, v);
    }
    quad(dev);
    assert_eq!(errors(), Vec::<String>::new());
}

#[test]
fn random_stage_setups_draw() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let dev = device(d3d());
    errors();
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |n: u32| {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        (x % n as u64) as u32
    };
    for case in 0..200 {
        let stages = 1 + next(8);
        for s in 0..8 {
            if s >= stages {
                tss(dev, s, 1, 1);
                tss(dev, s, 4, 1);
                continue;
            }
            let arg = |r: u32| r | [0, 0x10, 0x20, 0x30][(r >> 3 & 3) as usize];
            tss(dev, s, 1, 2 + next(25));
            tss(dev, s, 2, arg(next(7) | next(4) << 3) & 0x3f);
            tss(dev, s, 3, arg(next(7) | next(4) << 3) & 0x3f);
            tss(dev, s, 26, next(7));
            tss(dev, s, 4, 2 + next(25));
            tss(dev, s, 5, next(7) | next(2) << 4);
            tss(dev, s, 6, next(7) | next(2) << 4);
            tss(dev, s, 27, next(7));
            tss(dev, s, 28, if next(2) == 0 { 1 } else { 5 });
        }
        quad(dev);
        let e = errors();
        assert!(e.is_empty(), "case {case}: {e:?}");
    }
}
