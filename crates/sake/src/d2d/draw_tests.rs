//! Random BeginDraw, EndDraw, Flush, drawing calls and axis-aligned clips on a context's drawing
//! state, recorded as a trace for TLC to check against formal/tla/D2dDraw.tla (formal/trace.sh):
//! what each call gave back.
//!
//! `IPPAI_TRACE_DIR=dir` writes `dir/d2d-<seed>/`; `IPPAI_TRACE_SEEDS=n` (default 20).

use super::*;
use std::fmt::Write as _;

const OPS: [&str; 6] = ["Begin", "Draw", "Push", "Pop", "Flush", "End"];

/// (an HRESULT as the spec writes it: less 0x88990000)
fn code(hr: u32) -> i64 {
    if hr == 0 { 0 } else { (hr - 0x8899_0000) as i64 }
}

fn run(seed: u64) -> Vec<(usize, i64)> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut d = Drawing::default();
    (0..300)
        .map(|_| {
            // (mostly drawing between a Begin and an End, sometimes out of order)
            let op = match next() % 20 {
                0..=2 => 0,
                3..=7 => 1,
                8..=11 => 2,
                12..=15 => 3,
                16 => 4,
                _ => 5,
            };
            let ret = match op {
                0 => {
                    d.begin();
                    0
                }
                1 => d.draw() as i64,
                2 => d.push() as i64,
                3 => d.pop() as i64,
                4 => code(d.flush()),
                _ => code(d.end()),
            };
            (op, ret)
        })
        .collect()
}

/// D2dDraw.tla in Rust (the test fails here too, with the step, when TLC is not run).
fn check(steps: &[(usize, i64)], seed: u64) {
    let (mut drawing, mut error, mut clips) = (false, 0i64, 0i64);
    for (n, &(op, ret)) in steps.iter().enumerate() {
        let mut fail = |e: i64| {
            if error == 0 {
                error = e;
            }
        };
        let want = match OPS[op] {
            "Begin" => {
                if drawing {
                    fail(1);
                } else {
                    // (an error from outside drawing stays)
                    (drawing, clips) = (true, 0);
                }
                0
            }
            "Draw" | "Push" => {
                if !drawing {
                    fail(1);
                    0
                } else {
                    clips += (OPS[op] == "Push") as i64;
                    1
                }
            }
            "Pop" => {
                if !drawing {
                    fail(1);
                    0
                } else if clips == 0 {
                    fail(22);
                    0
                } else {
                    clips -= 1;
                    1
                }
            }
            "Flush" => {
                if drawing {
                    std::mem::take(&mut error)
                } else {
                    1
                }
            }
            _ => {
                let r = if !drawing { 1 } else if error != 0 { error } else if clips != 0 { 22 } else { 0 };
                (drawing, error, clips) = (false, 0, 0);
                r
            }
        };
        assert_eq!(ret, want, "seed {seed} step {n}: {}", OPS[op]);
    }
}

#[test]
fn d2d_draw_trace() {
    let seeds = std::env::var("IPPAI_TRACE_SEEDS").ok().and_then(|s| s.parse().ok()).unwrap_or(20u64);
    for seed in 0..seeds {
        let steps = run(seed);
        check(&steps, seed);
        let Some(dir) = std::env::var_os("IPPAI_TRACE_DIR") else { continue };
        let mut s = String::from("---- MODULE D2dData ----\nTrace == <<\n");
        for (i, (op, ret)) in steps.iter().enumerate() {
            let _ = writeln!(s, "  [op |-> \"{}\", ret |-> {ret}]{}", OPS[*op], if i + 1 < steps.len() { "," } else { "" });
        }
        s.push_str(">>\n====\n");
        let dir = std::path::Path::new(&dir).join(format!("d2d-{seed}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("D2dData.tla"), s).unwrap();
        std::fs::write(dir.join("SPEC"), "D2dDraw").unwrap();
    }
}
