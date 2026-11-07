//! Windows' answers (tests/winref/golden, recorded by the winref workflow) checked here at
//! once, without the page: what a new Direct3D 9 device's GetRenderState,
//! GetTextureStageState and GetSamplerState give (d3d9state: every render state, stages 0, 1
//! and 7, samplers 0, 15, the displacement map and vertex textures), result and value.

use super::draw_tests::{call, out, rd};
use super::trace_tests::{alloc, attach_page, CALLS, SERIAL};
use super::*;

fn golden(name: &str) -> String {
    let p = format!(
        "{}/../../tests/winref/golden/{name}/stdout.txt",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

/// "rs 7 = 0x1", "rs 1: Get 0x8876086c", "tss 0 1: 0 = 0x4": (key, result, value)
fn parse(l: &str) -> Option<(Vec<u32>, u32, Option<u32>)> {
    let num = |s: &str| {
        if let Some(h) = s.strip_prefix("0x") {
            u32::from_str_radix(h, 16).ok()
        } else {
            s.parse().ok()
        }
    };
    let w: Vec<&str> = l.split_whitespace().collect();
    match w.first()? {
        &"rs" if w.get(2) == Some(&"=") => Some((vec![0, num(w[1])?], 0, num(w[3]))),
        &"rs" if w.get(2) == Some(&"Get") => {
            Some((vec![0, num(w[1].trim_end_matches(':'))?], num(w[3])?, None))
        }
        &"tss" | &"ss" => Some((
            vec![
                if w[0] == "tss" { 1 } else { 2 },
                num(w[1])?,
                num(w[2].trim_end_matches(':'))?,
            ],
            num(w[3])?,
            num(w[5]),
        )),
        _ => None,
    }
}

#[test]
fn d3d9_new_device_state_is_windows() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    attach_page();
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    // (the probe's device: 64x48 X8R8G8B8, auto D24S8, software vertex processing)
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 48, 22, 1, 0, 0, 1, 1, 1, 1, 75, 0, 0, 0]
        .into_iter()
        .enumerate()
    {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    let dev = rd(o);
    let text = golden("d3d9state-32");
    let mut n = 0;
    let mut bad = Vec::new();
    for l in text.lines() {
        let Some((k, hr, v)) = parse(l) else { continue };
        let p = out();
        unsafe { *(p as *mut u32) = 0xdead_beef };
        let r = match k[0] {
            0 => call(IF_DEVICE, 58, &[dev, k[1], p]),
            1 => call(IF_DEVICE, 66, &[dev, k[1], k[2], p]),
            _ => call(IF_DEVICE, 68, &[dev, k[1], k[2], p]),
        };
        // (sampler state 14 and stage 8 read past Windows' arrays: what they give is not an answer)
        if k[0] == 2 && k[2] == 14
            || k[0] == 1 && k[1] >= 8
            || k[0] == 2 && matches!(k[1], 16 | 261)
        {
            continue;
        }
        n += 1;
        let got_v = (r == 0).then(|| rd(p));
        if r != hr || v.is_some() && got_v != v {
            bad.push(format!("{l}: got {r:#x} {got_v:x?}"));
        }
    }
    assert!(n > 400, "{n} answers checked");
    assert!(
        bad.is_empty(),
        "{} of {n} differ from Windows:\n{}",
        bad.len(),
        bad.join("\n")
    );
    gpu::set_pump(None);
}
