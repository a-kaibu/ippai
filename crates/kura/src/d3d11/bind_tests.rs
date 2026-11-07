//! Random input and output binds of views of the same resources through the real entry point
//! (`d3d11::call`): shader resource views (pixel, vertex, compute shader), render targets and
//! depth views (writable and read-only), the compute shader's UAV, a buffer as vertex buffer
//! and stream output target; and what the slots hold after each. Recorded as a trace for TLC
//! to check against formal/tla/Binds11.tla (formal/trace.sh).
//!
//! Views: 1 SRV of texture A's mip 0, 2 of its mip 1, 3 SRV of depth texture Z; 4 / 5 render
//! targets of A's mip 0 / 1; 6 / 7 depth views of Z, writable / read-only; 8 UAV of A's mip 0;
//! 9 the buffer.
//!
//! `IPPAI_TRACE_DIR=dir` writes `dir/d3d11bind-<seed>/`; `IPPAI_TRACE_SEEDS=n` (default 20).

use super::*;
use crate::d3d9::trace_tests::{alloc, seeds, write_module, Rng, CALLS, SERIAL};
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
fn release(a: u32) {
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
}

/// The slots: the SRV slots (pixel 0 and 1, vertex 0, compute 0), the render target, the
/// depth view, the compute UAV, vertex buffer 0, stream output target 0.
const SLOTS: [&str; 9] = ["ps0", "ps1", "vs0", "cs0", "rt", "ds", "uav", "vb", "so"];

#[derive(Default)]
struct Step {
    op: &'static str,
    /// the slot set or read (index in SLOTS); OM: the render target and the depth view
    s: u32,
    v: u32,
    w: u32,
    got: u32,
}

fn run(seed: u64) -> Vec<Step> {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 9);
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
    let o = mem(2);
    let tex = |w: u32, mips: u32, fmt: u32, bind: u32| {
        let d = mem(11);
        put(d, &[w, w, mips, 1, fmt, 1, 0, 0, bind, 0, 0]);
        assert_eq!(call1(m(IF_DEVICE, 5), &[dev, d, 0, o]), 0);
        rd1(o)
    };
    let a = tex(16, 2, 28, 0x8 | 0x20 | 0x80);
    let z = tex(16, 1, 44, 0x8 | 0x40);
    let bd = mem(6);
    put(bd, &[256, 0, 0x1 | 0x10, 0, 0, 0]);
    assert_eq!(call1(m(IF_DEVICE, 3), &[dev, bd, 0, o]), 0);
    let buf = rd1(o);
    // views: (method, resource, description words)
    let view = |kind: u32, res: u32, words: &[u32]| {
        let d = mem(8);
        put(d, words);
        assert_eq!(
            call1(m(IF_DEVICE, kind), &[dev, res, d, o]),
            0,
            "view {kind} {words:?}"
        );
        rd1(o)
    };
    let mut views = vec![0u32];
    views.push(view(7, a, &[28, 4, 0, 1, 0, 0]));
    views.push(view(7, a, &[28, 4, 1, 1, 0, 0]));
    views.push(view(7, z, &[46, 4, 0, 1, 0, 0]));
    views.push(view(9, a, &[28, 4, 0, 0, 0]));
    views.push(view(9, a, &[28, 4, 1, 0, 0]));
    views.push(view(10, z, &[45, 3, 0, 0, 0, 0]));
    views.push(view(10, z, &[45, 3, 3, 0, 0, 0]));
    views.push(view(8, a, &[28, 4, 0, 0, 0]));
    views.push(buf);
    let which = |x: u32| views.iter().position(|&v| v == x).map_or(99, |i| i as u32);
    let arr = mem(2);
    let mut steps = Vec::new();
    for _ in 0..250 {
        let mut t = Step::default();
        match r.below(10) {
            0..=2 => {
                // an SRV slot
                let s = r.below(4) as u32;
                let v = [0, 1, 2, 3][r.below(4)];
                put(arr, &[views[v]]);
                let (meth, slot) = [(8, 0), (8, 1), (25, 0), (67, 0)][s as usize];
                call1(m(IF_CONTEXT, meth), &[ctx, slot, 1, arr]);
                (t.op, t.s, t.v) = ("SetSRV", s, v as u32);
            }
            3 | 4 => {
                let (rt, ds) = ([0, 4, 5][r.below(3)], [0, 6, 7][r.below(3)]);
                put(arr, &[views[rt]]);
                call1(m(IF_CONTEXT, 33), &[ctx, 1, arr, views[ds]]);
                (t.op, t.v, t.w) = ("SetOM", rt as u32, ds as u32);
            }
            5 => {
                let v = [0, 8][r.below(2)];
                put(arr, &[views[v]]);
                call1(m(IF_CONTEXT, 68), &[ctx, 0, 1, arr, 0]);
                (t.op, t.v) = ("SetUAV", v as u32);
            }
            6 => {
                let v = [0, 9][r.below(2)];
                put(arr, &[views[v]]);
                let so = mem(2);
                if r.chance(50) {
                    call1(m(IF_CONTEXT, 18), &[ctx, 0, 1, arr, so, so]);
                    (t.op, t.v) = ("SetVB", v as u32);
                } else {
                    call1(m(IF_CONTEXT, 37), &[ctx, 1, arr, so]);
                    (t.op, t.v) = ("SetSO", v as u32);
                }
            }
            _ => {
                let s = r.below(SLOTS.len()) as u32;
                put(arr, &[0, 0]);
                let st = mem(2);
                match SLOTS[s as usize] {
                    "ps0" | "ps1" => call1(m(IF_CONTEXT, 73), &[ctx, s, 1, arr]),
                    "vs0" => call1(m(IF_CONTEXT, 84), &[ctx, 0, 1, arr]),
                    "cs0" => call1(m(IF_CONTEXT, 105), &[ctx, 0, 1, arr]),
                    "rt" => call1(m(IF_CONTEXT, 89), &[ctx, 1, arr, 0]),
                    "ds" => call1(m(IF_CONTEXT, 89), &[ctx, 0, 0, arr]),
                    "uav" => call1(m(IF_CONTEXT, 106), &[ctx, 0, 1, arr]),
                    "vb" => call1(m(IF_CONTEXT, 79), &[ctx, 0, 1, arr, st, st]),
                    _ => call1(m(IF_CONTEXT, 93), &[ctx, 1, arr]),
                };
                let got = rd1(arr);
                if got != 0 {
                    release(got);
                }
                (t.op, t.s, t.got) = ("Get", s, which(got));
            }
        }
        steps.push(t);
    }
    call1(m(IF_CONTEXT, 110), &[ctx]);
    for v in views.into_iter().skip(1) {
        release(v);
    }
    for x in [a, z, sc, ctx, dev] {
        release(x);
    }
    steps
}

/// Binds11.tla in Rust (the test fails here too, with the step, when TLC is not run).
fn check(steps: &[Step], seed: u64) {
    // per view: (resource, mip); 1..3 inputs, 4..8 outputs (7 read-only), 9 the buffer
    let span = |v: u32| {
        [
            (0, 0),
            (1, 0),
            (1, 1),
            (2, 0),
            (1, 0),
            (1, 1),
            (2, 0),
            (2, 0),
            (1, 0),
            (3, 0),
        ][v as usize]
    };
    let mut slots = [0u32; 9];
    for (n, t) in steps.iter().enumerate() {
        let outs = |slots: &[u32; 9]| -> Vec<(u32, u32)> {
            [slots[4], if slots[5] == 7 { 0 } else { slots[5] }, slots[6]]
                .into_iter()
                .filter(|&v| v != 0)
                .map(span)
                .collect()
        };
        let reads = |v: u32, slots: &[u32; 9]| v != 0 && outs(slots).contains(&span(v));
        match t.op {
            "SetSRV" => slots[t.s as usize] = if reads(t.v, &slots) { 0 } else { t.v },
            "SetOM" | "SetUAV" => {
                if t.op == "SetOM" {
                    (slots[4], slots[5]) = (t.v, t.w);
                } else {
                    slots[6] = t.v;
                }
                for s in 0..4 {
                    if reads(slots[s], &slots) {
                        slots[s] = 0;
                    }
                }
            }
            "SetVB" => slots[7] = if t.v != 0 && slots[8] == t.v { 0 } else { t.v },
            "SetSO" => {
                slots[8] = t.v;
                if t.v != 0 && slots[7] == t.v {
                    slots[7] = 0;
                }
            }
            _ => assert_eq!(
                t.got, slots[t.s as usize],
                "seed {seed} step {n}: Get {}",
                SLOTS[t.s as usize]
            ),
        }
    }
}

#[test]
fn d3d11_bind_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
    for seed in 0..seeds() {
        let steps = run(seed);
        let mut s = String::from("---- MODULE BindData ----\nTrace == <<\n");
        for (i, t) in steps.iter().enumerate() {
            let _ = writeln!(
                s,
                "  [op |-> \"{}\", s |-> {}, v |-> {}, w |-> {}, got |-> {}]{}",
                t.op,
                t.s,
                t.v,
                t.w,
                t.got,
                if i + 1 < steps.len() { "," } else { "" }
            );
        }
        s.push_str(">>\n====\n");
        write_module(&format!("d3d11bind-{seed}"), "Binds11", "BindData", &s);
        check(&steps, seed);
    }
}
