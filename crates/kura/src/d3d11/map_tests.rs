//! Random Map / Unmap, writes through the pointer a Map gave, UpdateSubresource,
//! ClearRenderTargetView, CopyResource and CopySubresourceRegion through the real entry point
//! (`d3d11::call`), with the page's executor running in the same thread on the model of WebGPU
//! (as d3d9/lock_tests.rs). Recorded as a trace for TLC to check against formal/tla/Maps.tla
//! (formal/trace.sh): each Map's result, what a Map for reading shows, and what a readback of
//! the page's texture gives.
//!
//! Textures are 16x16 B8G8R8A8 in 4x4 cells, buffers 64 bytes in 16 cells of 4 bytes; every
//! write paints whole cells with bytes that name it (d3d9/lock_tests.rs).

use super::*;
use crate::d3d9::lock_tests::{fill_color, name_of, random_rect, write_bytes};
use crate::d3d9::trace_tests::{alloc, attach_page, hr, seeds, write_module, Rng, CALLS, SERIAL};
use crate::render::model;
use std::fmt::Write as _;

fn mem(n: u32) -> u32 {
    unsafe { alloc(4 * n) as u32 }
}

fn put(p: u32, v: &[u32]) {
    for (i, &x) in v.iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = x };
    }
}

fn rd1(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}

fn call1(id: u32, args: &[u32]) -> u32 {
    unsafe { call(id, args.as_ptr(), &CALLS) }
}

fn m(iface: u32, m: u32) -> u32 {
    iface << 8 | m
}

/// Resources: 0 a default texture (with a render target view), 1 a staging texture, 2 a
/// dynamic texture, 3 a staging buffer, 4 a default buffer.
const TEX: [bool; 5] = [true, true, true, false, false];

/// Names of the cells inside `r` of a resource whose memory starts at `bits` (rows `pitch` apart).
fn cells(tex: bool, bits: u32, pitch: u32, r: [u32; 4]) -> Vec<i64> {
    let mut v = vec![-9; 16];
    for cy in r[1]..r[3] {
        for cx in r[0]..r[2] {
            let mut px = Vec::new();
            if tex {
                for y in 0..4 {
                    for x in 0..4 {
                        px.push(unsafe {
                            *((bits + (cy * 4 + y) * pitch + (cx * 4 + x) * 4) as *const [u8; 4])
                        });
                    }
                }
            } else {
                px.push(unsafe { *((bits + (cy * 4 + cx) * 4) as *const [u8; 4]) });
            }
            v[(cy * 4 + cx) as usize] = name_of(&px);
        }
    }
    v
}

/// Paints cells `r` with the bytes of write `k`.
fn paint(tex: bool, bits: u32, pitch: u32, r: [u32; 4], k: u32) {
    let b = write_bytes(k);
    for cy in r[1]..r[3] {
        for cx in r[0]..r[2] {
            if tex {
                for y in 0..4 {
                    for x in 0..4 {
                        unsafe {
                            *((bits + (cy * 4 + y) * pitch + (cx * 4 + x) * 4) as *mut [u8; 4]) = b
                        };
                    }
                }
            } else {
                unsafe { *((bits + (cy * 4 + cx) * 4) as *mut [u8; 4]) = b };
            }
        }
    }
}

/// (buffers: cells x0..x1 of row 0 only, so that a box is a byte range)
fn random_cells(r: &mut Rng, tex: bool) -> [u32; 4] {
    if tex {
        random_rect(r)
    } else {
        let x0 = r.below(16) as u32;
        [x0, 0, x0 + 1 + r.below(16 - x0 as usize) as u32, 1]
    }
}

#[derive(Default)]
struct Step {
    op: &'static str,
    s: u32,
    d: u32,
    r: [u32; 4],
    p: [u32; 2],
    ty: u32,
    k: u32,
    ret: String,
    obs: Vec<i64>,
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x2545_f491_4f6c_dd1d) | 3);
    attach_page();
    let desc = mem(16);
    put(desc, &[64, 64, 60, 1, 28, 0, 0, 1, 0, 0x20, 1, 1, 1, 0, 0]);
    let (sc_out, dev_out, level, ctx_out) = (mem(2), mem(2), mem(2), mem(2));
    assert_eq!(
        call1(
            CREATE_DEVICE,
            &[0, 1, 0, 0, 0, 0, 7, desc, sc_out, dev_out, level, ctx_out]
        ),
        0
    );
    let (sc, dev, ctx) = (rd1(sc_out), rd1(dev_out), rd1(ctx_out));
    let mut res = Vec::new();
    // textures: (usage, bind, cpu access)
    for (usage, bind, cpu) in [(0, 0x28, 0), (3, 0, 0x30000), (2, 0x8, 0x10000)] {
        let d = mem(11);
        put(d, &[16, 16, 1, 1, 87, 1, 0, usage, bind, cpu, 0]);
        let o = mem(2);
        assert_eq!(call1(m(IF_DEVICE, 5), &[dev, d, 0, o]), 0);
        res.push(rd1(o));
    }
    for (usage, cpu) in [(3, 0x30000), (0, 0)] {
        let d = mem(6);
        put(d, &[64, usage, 0, cpu, 0, 0]);
        let o = mem(2);
        assert_eq!(call1(m(IF_DEVICE, 3), &[dev, d, 0, o]), 0);
        res.push(rd1(o));
    }
    let o = mem(2);
    assert_eq!(call1(m(IF_DEVICE, 9), &[dev, res[0], 0, o]), 0);
    let rtv = rd1(o);
    let gpu_of = |a: u32| {
        let g = D11S.lock().unwrap();
        let d = g.as_ref().unwrap();
        match &d.objs[&d.id_of(a)].data {
            Data::Texture(t) => t.gpu,
            _ => 0,
        }
    };
    let gpus: Vec<u32> = res.iter().map(|&a| gpu_of(a)).collect();
    let mut maps: Vec<Option<(bool, u32, u32)>> = vec![None; 5];
    let mut steps = Vec::new();
    let mut n = 0u32;
    let mut reads = 0u32;
    let rb = unsafe { alloc(16 * 64) as u32 };
    for _ in 0..300 {
        let i = r.below(5);
        let tex = TEX[i];
        let mut t = Step {
            s: i as u32,
            ..Default::default()
        };
        let op = if maps[i].is_some() {
            [3, 3, 3, 2, 2][r.below(5)]
        } else {
            [0, 0, 0, 1, 4, 5, 6, 7, 9, 9][r.below(10)]
        };
        match op {
            0 => {
                // Map(resource, 0, type, 0, out)
                let ty = 1 + r.below(5) as u32;
                let o = mem(4);
                let ret = call1(m(IF_CONTEXT, 14), &[ctx, res[i], 0, ty, 0, o]);
                (t.op, t.ty, t.ret) = ("Map", ty, hr(ret));
                if ret == 0 {
                    let (bits, pitch) = (rd1(o), rd1(o + 4));
                    maps[i] = Some((ty != 1, bits, pitch));
                    t.obs = cells(
                        tex,
                        bits,
                        pitch,
                        if tex { [0, 0, 4, 4] } else { [0, 0, 16, 1] },
                    );
                }
            }
            2 => {
                call1(m(IF_CONTEXT, 15), &[ctx, res[i], 0]);
                maps[i] = None;
                (t.op, t.ret) = ("Unmap", hr(0));
            }
            3 => {
                let Some((true, bits, pitch)) = maps[i] else {
                    continue;
                };
                let c = random_cells(&mut r, tex);
                n += 1;
                paint(tex, bits, pitch, c, n);
                (t.op, t.r, t.k, t.ret) = ("Write", c, n, hr(0));
            }
            1 => {
                // UpdateSubresource of the default texture or buffer
                let i = if r.chance(50) { 0 } else { 4 };
                if maps[i].is_some() {
                    continue;
                }
                let tex = TEX[i];
                let c = random_cells(&mut r, tex);
                n += 1;
                let data = unsafe { alloc(16 * 64) as u32 };
                let (pitch, at) = (64, data);
                // (the box's first texel at `data`)
                let shifted = if tex {
                    [0, 0, c[2] - c[0], c[3] - c[1]]
                } else {
                    [0, 0, c[2] - c[0], 1]
                };
                paint(tex, at, pitch, shifted, n);
                let bx = mem(6);
                if tex {
                    put(bx, &[c[0] * 4, c[1] * 4, 0, c[2] * 4, c[3] * 4, 1]);
                } else {
                    put(bx, &[c[0] * 4, 0, 0, c[2] * 4, 1, 1]);
                }
                call1(m(IF_CONTEXT, 48), &[ctx, res[i], 0, bx, data, pitch, 0]);
                (t.op, t.s, t.r, t.k, t.ret) = ("Write", i as u32, c, n, hr(0));
            }
            4 => {
                if maps[0].is_some() {
                    continue;
                }
                n += 1;
                let b = fill_color(n).to_le_bytes();
                let color = mem(4);
                put(
                    color,
                    &[b[2], b[1], b[0], b[3]].map(|x| (x as f32 / 255.0).to_bits()),
                );
                call1(m(IF_CONTEXT, 50), &[ctx, rtv, color]);
                (t.op, t.s, t.r, t.k, t.ret) = ("Fill", 0, [0, 0, 4, 4], n, hr(0));
            }
            5 => {
                // CopyResource between textures (into the default or staging one) or buffers
                let (s, d) = [(0, 1), (1, 0), (2, 0), (2, 1), (3, 4), (4, 3)][r.below(6)];
                if maps[s].is_some() || maps[d].is_some() {
                    continue;
                }
                call1(m(IF_CONTEXT, 47), &[ctx, res[d], res[s]]);
                let all = if TEX[s] { [0, 0, 4, 4] } else { [0, 0, 16, 1] };
                (t.op, t.s, t.d, t.r, t.ret) = ("Copy", s as u32, d as u32, all, hr(0));
            }
            6 => {
                // CopySubresourceRegion: cells to cells
                let (s, d) = match r.below(4) {
                    0 => (0, 1),
                    1 => (1, 0),
                    2 => (3, 4),
                    _ => (4, 3),
                };
                if maps[s].is_some() || maps[d].is_some() {
                    continue;
                }
                let tex = TEX[s];
                let c = random_cells(&mut r, tex);
                let (wmax, hmax) = if tex { (4, 4) } else { (16, 1) };
                let p = [
                    r.below((wmax + 1 - (c[2] - c[0])) as usize) as u32,
                    if tex {
                        r.below((hmax + 1 - (c[3] - c[1])) as usize) as u32
                    } else {
                        0
                    },
                ];
                let bx = mem(6);
                if tex {
                    put(bx, &[c[0] * 4, c[1] * 4, 0, c[2] * 4, c[3] * 4, 1]);
                    call1(
                        m(IF_CONTEXT, 46),
                        &[ctx, res[d], 0, p[0] * 4, p[1] * 4, 0, res[s], 0, bx],
                    );
                } else {
                    put(bx, &[c[0] * 4, 0, 0, c[2] * 4, 1, 1]);
                    call1(
                        m(IF_CONTEXT, 46),
                        &[ctx, res[d], 0, p[0] * 4, 0, 0, res[s], 0, bx],
                    );
                }
                (t.op, t.s, t.d, t.r, t.p, t.ret) = ("Copy", s as u32, d as u32, c, p, hr(0));
            }
            7 => {
                gpu::pump();
                (t.op, t.ret) = ("Page", hr(0));
            }
            _ => {
                if !tex || maps[i].is_some() {
                    continue;
                }
                reads += 1;
                let req = 0x5000_0000 + reads;
                gpu::record(|b| b.cmd(&[gpu::READBACK, req, gpus[i], 0, 0, 0, 16, 16, rb, 64]));
                assert!(gpu::wait_readback(req));
                (t.op, t.ret, t.obs) = ("GpuRead", hr(0), cells(true, rb, 64, [0, 0, 4, 4]));
            }
        }
        steps.push(t);
    }
    // (everything back: the device and context live on, and the next test counts their
    // references from here)
    let release = |a: u32| {
        let iface = {
            let g = D11S.lock().unwrap();
            let d = g.as_ref().unwrap();
            d.objs[&d.id_of(a)]
                .faces
                .iter()
                .find(|f| f.1 == a)
                .unwrap()
                .0
        };
        call1(m(iface, 2), &[a]);
    };
    for a in res.iter().copied().chain([rtv, sc, ctx, dev]) {
        release(a);
    }
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(
        errors.is_empty(),
        "seed {seed}: WebGPU errors {:?}",
        &errors[..errors.len().min(10)]
    );
    let mut s = String::from("---- MODULE MapData ----\nEXTENDS Integers\nTrace == <<\n");
    for (i, t) in steps.iter().enumerate() {
        let obs = t
            .obs
            .iter()
            .map(|x| x.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            s,
            "  [op |-> \"{}\", s |-> {}, d |-> {}, r |-> <<{}, {}, {}, {}>>, p |-> <<{}, {}>>, ty |-> {}, k |-> {}, ret |-> {}, obs |-> <<{obs}>>]{}\n",
            t.op, t.s, t.d, t.r[0], t.r[1], t.r[2], t.r[3], t.p[0], t.p[1], t.ty, t.k, t.ret,
            if i + 1 < steps.len() { "," } else { "" }
        );
    }
    s.push_str(">>\n====\n");
    write_module(&format!("d3d11map-{seed}"), "Maps", "MapData", &s);
    check(&steps, seed);
}

/// Maps.tla in Rust (the test fails here too, with the step, when TLC is not run).
fn check(steps: &[Step], seed: u64) {
    if std::env::var_os("IPPAI_MAP_DUMP").is_some() {
        for (n, t) in steps.iter().enumerate() {
            eprintln!(
                "{n}: {} s{} d{} r{:?} p{:?} ty{} k{} {} {:?}",
                t.op, t.s, t.d, t.r, t.p, t.ty, t.k, t.ret, t.obs
            );
        }
    }
    // (usage of each resource: 0 default, 2 dynamic, 3 staging)
    let usage = [0, 3, 2, 3, 0];
    let mut content = vec![vec![-1i64; 16]; 5];
    for (n, t) in steps.iter().enumerate() {
        let s = t.s as usize;
        let cells_of = |r: [u32; 4]| {
            (r[1]..r[3]).flat_map(move |y| (r[0]..r[2]).map(move |x| (y * 4 + x) as usize))
        };
        let seen =
            |obs: &[i64], c: &[i64]| (0..16).all(|i| obs[i] == -9 || c[i] == -1 || obs[i] == c[i]);
        let want = match t.op {
            "Map" => {
                // (D3D11_MAP: 1 read, 2 write, 3 read and write, 4 write discard, 5 no overwrite)
                let ok = match usage[s] {
                    3 => (1..=3).contains(&t.ty),
                    2 => t.ty == 4 || (t.ty == 5 && !TEX[s]),
                    _ => false,
                };
                if !ok {
                    "\"invalid\""
                } else {
                    if t.ty == 4 {
                        content[s] = vec![-1; 16];
                    } else if t.ty != 2 && t.ty != 5 {
                        assert!(
                            seen(&t.obs, &content[s]),
                            "seed {seed} step {n}: map read {:?}, content {:?}",
                            t.obs,
                            content[s]
                        );
                    }
                    "\"ok\""
                }
            }
            "Write" => {
                for i in cells_of(t.r) {
                    content[s][i] = t.k as i64;
                }
                "\"ok\""
            }
            "Fill" => {
                content[s] = vec![100000 + t.k as i64; 16];
                "\"ok\""
            }
            "Copy" => {
                let src = content[s].clone();
                for y in 0..t.r[3] - t.r[1] {
                    for x in 0..t.r[2] - t.r[0] {
                        content[t.d as usize][((t.p[1] + y) * 4 + t.p[0] + x) as usize] =
                            src[((t.r[1] + y) * 4 + t.r[0] + x) as usize];
                    }
                }
                "\"ok\""
            }
            "GpuRead" => {
                assert!(
                    seen(&t.obs, &content[s]),
                    "seed {seed} step {n}: page read {:?}, content {:?}",
                    t.obs,
                    content[s]
                );
                "\"ok\""
            }
            _ => "\"ok\"",
        };
        assert_eq!(
            t.ret, want,
            "seed {seed} step {n}: {} of resource {s} (type {})",
            t.op, t.ty
        );
    }
}

#[test]
fn d3d11_map_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
    for seed in 0..seeds() {
        run(seed);
    }
}
