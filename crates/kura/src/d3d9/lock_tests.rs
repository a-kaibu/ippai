//! Random LockRect / UnlockRect, writes through the pointer a lock gave, ColorFill,
//! GetRenderTargetData, UpdateSurface and StretchRect through the real entry point
//! (`vnhost_d3d9_call`), with the page's executor (render/d3d.rs) running in the same thread on
//! the model of WebGPU (render/model.rs) whenever the guest would wait for it, or when a step
//! says so. Recorded as a trace for TLC to check against formal/tla/Locks.tla
//! (formal/trace.sh): each call's result, what a lock lets the application read, and what a
//! readback of the page's texture gives.
//!
//! Surfaces are 4x4 cells; every write and fill paints whole cells with bytes that name it, so
//! what is read is a cell's name: the write or fill it came from.
//!
//! `IPPAI_TRACE_DIR=dir` writes `dir/d3d9lock-<seed>/`; `IPPAI_TRACE_SEEDS=n` (default 20).

use super::trace_tests::{alloc, attach_page, hr, seeds, write_module, Rng, CALLS, SERIAL};
use super::*;
use crate::render::model;
use std::fmt::Write as _;

fn call(iface: u32, m: u32, args: &[u32]) -> u32 {
    // (padded like the guest stack: the Direct3D 8 forwards read 10 words)
    let mut a = args.to_vec();
    a.resize(a.len().max(16), 0);
    unsafe { vnhost_d3d9_call(iface << 8 | m, a.as_ptr(), &CALLS) }
}

fn out() -> u32 {
    unsafe { alloc(16) as u32 }
}

fn rd(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}

fn with<R>(f: impl FnOnce(&mut D3d) -> R) -> R {
    f(D3D.lock().unwrap().as_mut().unwrap())
}

/// A RECT in guest memory (pixels).
fn rect(r: [u32; 4]) -> u32 {
    let p = unsafe { alloc(16) as u32 };
    for (i, v) in r.into_iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = v };
    }
    p
}

/// The name of a cell's bytes: a write k (k), a fill k (100000 + k); -2 none of them, -3 a cell
/// whose pixels differ.
pub(crate) fn name_of(px: &[[u8; 4]]) -> i64 {
    let p = px[0];
    if px.iter().any(|&q| q != p) {
        return -3;
    }
    let k = p[1] as i64 | (p[2] as i64) << 8;
    match (p[0], p[3]) {
        (0x5a, 0x11) => k,
        (0xa5, 0x22) => 100000 + k,
        _ => -2,
    }
}

pub(crate) fn write_bytes(k: u32) -> [u8; 4] {
    [0x5a, k as u8, (k >> 8) as u8, 0x11]
}

/// The D3DCOLOR of fill k (A8R8G8B8 in memory: B G R A).
pub(crate) fn fill_color(k: u32) -> u32 {
    0x2200_00a5 | (k & 0xffff) << 8
}

#[derive(Clone, Copy)]
struct Surf {
    /// (texture level 0 of a managed texture: locked through the texture)
    tex: u32,
    addr: u32,
    w: u32,
    h: u32,
    /// on the page (0: system memory)
    gpu: u32,
}

/// One trace step, as a Locks.tla record.
#[derive(Default)]
struct Step {
    op: &'static str,
    s: u32,
    d: u32,
    r: [u32; 4],
    p: [u32; 2],
    kind: &'static str,
    k: u32,
    ret: String,
    obs: Vec<i64>,
}

/// Names of the cells of `s` (rows `pitch` apart from `bits`) inside `r` (cells); -9 outside.
fn cells(s: &Surf, bits: u32, pitch: u32, r: [u32; 4]) -> Vec<i64> {
    let (cw, ch) = (s.w / 4, s.h / 4);
    let mut v = vec![-9; 16];
    for cy in r[1]..r[3] {
        for cx in r[0]..r[2] {
            let mut px = Vec::new();
            for y in 0..ch {
                for x in 0..cw {
                    // (bits points at the lock rect's top left)
                    let at = bits + ((cy - r[1]) * ch + y) * pitch + ((cx - r[0]) * cw + x) * 4;
                    px.push(unsafe { *(at as *const [u8; 4]) });
                }
            }
            v[(cy * 4 + cx) as usize] = name_of(&px);
        }
    }
    v
}

/// What a readback of the page's texture of `s` gives now.
fn gpu_cells(s: &Surf, n: &mut u32) -> Vec<i64> {
    let pitch = s.w * 4;
    // (one buffer for every readback: the test arena is never given back)
    static BUF: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let dst = *BUF.get_or_init(|| unsafe { alloc(256 * 256 * 4) as u32 });
    *n += 1;
    let req = 0x4000_0000 + *n;
    gpu::record(|b| b.cmd(&[gpu::READBACK, req, s.gpu, 0, 0, 0, s.w, s.h, dst, pitch]));
    assert!(gpu::wait_readback(req));
    if std::env::var_os("IPPAI_LOCK_DUMP").is_some() {
        for y in 0..s.h.min(16) {
            let row: Vec<String> = (0..s.w.min(16)).map(|x| format!("{:08x}", unsafe { *((dst + y * pitch + x * 4) as *const u32) })).collect();
            eprintln!("  {}", row.join(" "));
        }
    }
    cells(s, dst, pitch, [0, 0, 4, 4])
}

pub(crate) fn random_rect(r: &mut Rng) -> [u32; 4] {
    let (x0, y0) = (r.below(4) as u32, r.below(4) as u32);
    let (x1, y1) = (x0 + 1 + r.below(4 - x0 as usize) as u32, y0 + 1 + r.below(4 - y0 as usize) as u32);
    [x0, y0, x1, y1]
}

fn px(s: &Surf, r: [u32; 4]) -> [u32; 4] {
    [r[0] * s.w / 4, r[1] * s.h / 4, r[2] * s.w / 4, r[3] * s.h / 4]
}

/// `script`: (op, surface, cells or [0; 4] for all, a write's bytes: those of write k, 0 new)
/// steps to take instead of random ones, with locks that write.
fn run(seed: u64, script: Option<&[(u32, usize, [u32; 4], u32)]>) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    // the page: runs what the guest sent, finishes the readbacks it asked for
    attach_page();
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let o = out();
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o);
    let surf = |a: u32, tex: u32| {
        let s = with(|d| d.id_of(a));
        with(|d| match &d.objs[&s].data {
            Data::Surface(x) => Surf { tex, addr: a, w: x.w, h: x.h, gpu: x.gpu },
            _ => unreachable!(),
        })
    };
    // 0: level 0 of a managed texture; 1: a 256x256 off-screen plain surface (big enough for
    // the changed-rows upload); 2, 4: render targets; 3: system memory
    let mut ss = Vec::new();
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, 16, 16, 1, 0, 21, 1, o, 0]), 0);
    let tex = rd(o);
    let o = out();
    assert_eq!(call(IF_TEXTURE, 18, &[tex, 0, o]), 0);
    ss.push(surf(rd(o), tex));
    let o = out();
    assert_eq!(call(IF_DEVICE, 36, &[dev, 256, 256, 21, 0, o, 0]), 0);
    ss.push(surf(rd(o), 0));
    for k in 0..3 {
        let o = out();
        if k == 1 {
            assert_eq!(call(IF_DEVICE, 36, &[dev, 16, 16, 21, 2, o, 0]), 0);
        } else {
            assert_eq!(call(IF_DEVICE, 28, &[dev, 16, 16, 21, 0, 0, 1, o, 0]), 0);
        }
        ss.push(surf(rd(o), 0));
    }
    let mut steps = Vec::new();
    // (what each surface's lock lets us write, and where it points)
    let mut locks: Vec<Option<(bool, [u32; 4], u32, u32)>> = vec![None; ss.len()];
    let mut n = 0u32;
    let mut reads = 0u32;
    for step in 0..script.map_or(300, |s| s.len()) {
        let mut i = r.below(ss.len());
        if let Some(sc) = script {
            i = sc[step].1;
        }
        let s = ss[i];
        let mut t = Step { s: i as u32, ..Default::default() };
        // (locked: mostly writes, then the unlock; else locks, fills, copies, reads)
        let op = if locks[i].is_some() {
            [3, 3, 3, 4, 4, 2, 2, 2, 0, 9][r.below(10)]
        } else {
            [0, 0, 0, 1, 5, 5, 6, 7, 8, 9, 9, 9, 2][r.below(13)]
        };
        let op = script.map_or(op, |sc| sc[step].0);
        let full = script.is_some();
        let given = script.map(|sc| sc[step].2).filter(|r| r[2] > 0);
        let reuse = script.map_or(0, |sc| sc[step].3);
        match op {
            0 | 1 => {
                let whole = if full { given.is_none() } else { r.chance(30) };
                let cr = if whole { [0, 0, 4, 4] } else { given.unwrap_or_else(|| random_rect(&mut r)) };
                let (kind, flags) = match if full { 2 } else { r.below(4) } {
                    0 => ("ro", 0x10),
                    1 => ("discard", 0x2000),
                    _ => ("rw", 0),
                };
                let o = unsafe { alloc(8) as u32 };
                let rp = if whole { 0 } else { rect(px(&s, cr)) };
                let res = if s.tex != 0 { call(IF_TEXTURE, 19, &[s.tex, 0, o, rp, flags]) } else { call(IF_SURFACE, 13, &[s.addr, o, rp, flags]) };
                (t.op, t.r, t.kind, t.ret) = ("Lock", cr, kind, hr(res));
                if res == 0 {
                    let (pitch, bits) = (rd(o), rd(o + 4));
                    locks[i] = Some((kind != "ro", cr, bits, pitch));
                    t.obs = cells(&s, bits, pitch, cr);
                }
            }
            2 => {
                let res = if s.tex != 0 { call(IF_TEXTURE, 20, &[s.tex, 0]) } else { call(IF_SURFACE, 14, &[s.addr]) };
                (t.op, t.ret) = ("Unlock", hr(res));
                if res == 0 {
                    locks[i] = None;
                }
            }
            3 | 4 => {
                // (through the pointer of a lock that may write)
                let Some((true, lr, bits, pitch)) = locks[i] else { continue };
                let x0 = lr[0] + r.below((lr[2] - lr[0]) as usize) as u32;
                let y0 = lr[1] + r.below((lr[3] - lr[1]) as usize) as u32;
                let x1 = x0 + 1 + r.below((lr[2] - x0) as usize) as u32;
                let y1 = y0 + 1 + r.below((lr[3] - y0) as usize) as u32;
                let [x0, y0, x1, y1] = if full { given.unwrap_or(lr) } else { [x0, y0, x1, y1] };
                // (sometimes the bytes of an earlier write: unchanged pixels must still reach
                // the page when it has others)
                let k = if reuse != 0 {
                    reuse
                } else if !full && n > 0 && r.chance(30) {
                    n - r.below(n.min(3) as usize) as u32
                } else {
                    n += 1;
                    n
                };
                let b = write_bytes(k);
                let (cw, ch) = (s.w / 4, s.h / 4);
                for y in (y0 - lr[1]) * ch..(y1 - lr[1]) * ch {
                    for x in (x0 - lr[0]) * cw..(x1 - lr[0]) * cw {
                        unsafe { *((bits + y * pitch + x * 4) as *mut [u8; 4]) = b };
                    }
                }
                (t.op, t.r, t.k, t.ret) = ("Write", [x0, y0, x1, y1], k, hr(0));
            }
            5 => {
                if locks[i].is_some() {
                    continue;
                }
                let whole = r.chance(30);
                let cr = if whole { [0, 0, 4, 4] } else { random_rect(&mut r) };
                n += 1;
                let res = call(IF_DEVICE, 35, &[dev, s.addr, if whole { 0 } else { rect(px(&s, cr)) }, fill_color(n)]);
                (t.op, t.r, t.k, t.ret) = ("Fill", cr, n, hr(res));
            }
            6 => {
                // render target 2 or 4 into system memory 3
                let src = if r.chance(50) { 2 } else { 4 };
                if locks[src].is_some() || locks[3].is_some() {
                    continue;
                }
                let res = call(IF_DEVICE, 32, &[dev, ss[src].addr, ss[3].addr]);
                (t.op, t.s, t.d, t.ret) = ("GetRTData", src as u32, 3, hr(res));
            }
            7 => {
                // system memory 3 into render target 2 or 4, cells to cells
                let dst = if r.chance(50) { 2 } else { 4 };
                if locks[dst].is_some() || locks[3].is_some() {
                    continue;
                }
                let cr = random_rect(&mut r);
                let p = [r.below(5 - (cr[2] - cr[0]) as usize) as u32, r.below(5 - (cr[3] - cr[1]) as usize) as u32];
                let pt = rect([p[0] * 4, p[1] * 4, 0, 0]);
                let res = call(IF_DEVICE, 30, &[dev, ss[3].addr, rect(px(&ss[3], cr)), ss[dst].addr, pt]);
                (t.op, t.s, t.d, t.r, t.p, t.ret) = ("Copy", 3, dst as u32, cr, p, hr(res));
            }
            8 => {
                // between the render targets, the same size
                let (src, dst) = if full { (i, 6 - i) } else if r.chance(50) { (2, 4) } else { (4, 2) };
                if locks[dst].is_some() || locks[src].is_some() {
                    continue;
                }
                let cr = if full { [0, 0, 4, 4] } else { random_rect(&mut r) };
                let p = if full { [0, 0] } else { [r.below(5 - (cr[2] - cr[0]) as usize) as u32, r.below(5 - (cr[3] - cr[1]) as usize) as u32] };
                let dr = [p[0], p[1], p[0] + cr[2] - cr[0], p[1] + cr[3] - cr[1]];
                let res = call(IF_DEVICE, 34, &[dev, ss[src].addr, rect(px(&ss[src], cr)), ss[dst].addr, rect(px(&ss[dst], dr)), 1]);
                (t.op, t.s, t.d, t.r, t.p, t.ret) = ("Copy", src as u32, dst as u32, cr, p, hr(res));
            }
            _ => {
                if script.is_none() && r.chance(50) {
                    gpu::pump();
                    t.op = "Page";
                } else {
                    if s.gpu == 0 || locks[i].is_some() {
                        continue;
                    }
                    t.op = "GpuRead";
                    t.obs = gpu_cells(&s, &mut reads);
                }
                t.ret = hr(0);
            }
        }
        steps.push(t);
    }
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(errors.is_empty(), "seed {seed}: WebGPU errors {:?}", &errors[..errors.len().min(10)]);
    let mut s = String::from("---- MODULE LockData ----\nEXTENDS Integers\nTrace == <<\n");
    for (i, t) in steps.iter().enumerate() {
        let obs = t.obs.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ");
        let _ = write!(
            s,
            "  [op |-> \"{}\", s |-> {}, d |-> {}, r |-> <<{}, {}, {}, {}>>, p |-> <<{}, {}>>, kind |-> \"{}\", k |-> {}, ret |-> {}, obs |-> <<{obs}>>]{}\n",
            t.op, t.s, t.d, t.r[0], t.r[1], t.r[2], t.r[3], t.p[0], t.p[1], t.kind, t.k, t.ret,
            if i + 1 < steps.len() { "," } else { "" }
        );
    }
    s.push_str(">>\n====\n");
    write_module(&format!("d3d9lock-{seed}"), "Locks", "LockData", &s);
    check(&steps, seed);
}

/// Locks.tla in Rust (the test fails here too, with the step, when TLC is not run).
fn check(steps: &[Step], seed: u64) {
    // surfaces: managed, default, render target, system memory, render target
    let fillable = [false, true, true, false, true];
    let mut content = vec![vec![-1i64; 16]; 5];
    let mut lock: Vec<Option<(&str, [u32; 4])>> = vec![None; 5];
    for (n, t) in steps.iter().enumerate() {
        let s = t.s as usize;
        let cells_of = |r: [u32; 4]| (r[1]..r[3]).flat_map(move |y| (r[0]..r[2]).map(move |x| (y * 4 + x) as usize));
        let seen = |obs: &[i64], c: &[i64], r: [u32; 4]| cells_of(r).all(|i| c[i] == -1 || obs[i] == c[i]);
        let want = match t.op {
            "Lock" => {
                if lock[s].is_some() {
                    "\"invalid\""
                } else {
                    if t.kind != "discard" {
                        assert!(seen(&t.obs, &content[s], t.r), "seed {seed} step {n}: lock read {:?}, content {:?}", t.obs, content[s]);
                    } else {
                        for i in cells_of(t.r) {
                            content[s][i] = -1;
                        }
                    }
                    lock[s] = Some((t.kind, t.r));
                    "\"ok\""
                }
            }
            "Unlock" => {
                if lock[s].take().is_some() {
                    "\"ok\""
                } else {
                    "\"invalid\""
                }
            }
            "Write" => {
                for i in cells_of(t.r) {
                    content[s][i] = t.k as i64;
                }
                "\"ok\""
            }
            "Fill" => {
                if fillable[s] {
                    for i in cells_of(t.r) {
                        content[s][i] = 100000 + t.k as i64;
                    }
                    "\"ok\""
                } else {
                    "\"invalid\""
                }
            }
            "GetRTData" => {
                content[t.d as usize] = content[s].clone();
                "\"ok\""
            }
            "Copy" => {
                let src = content[s].clone();
                for y in 0..t.r[3] - t.r[1] {
                    for x in 0..t.r[2] - t.r[0] {
                        content[t.d as usize][((t.p[1] + y) * 4 + t.p[0] + x) as usize] = src[((t.r[1] + y) * 4 + t.r[0] + x) as usize];
                    }
                }
                "\"ok\""
            }
            "GpuRead" => {
                assert!(seen(&t.obs, &content[s], [0, 0, 4, 4]), "seed {seed} step {n}: page read {:?}, content {:?}", t.obs, content[s]);
                "\"ok\""
            }
            _ => "\"ok\"",
        };
        assert_eq!(t.ret, want, "seed {seed} step {n}: {} of surface {s}", t.op);
    }
}

#[test]
fn d3d9_lock_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    for seed in 0..seeds() {
        run(seed, None);
    }
}

/// A copy on the page between two uploads of the same surface in one batch must copy the first
/// (the second write must not reach the copy through guest memory the first upload reads).
#[test]
fn d3d9_lock_copy_between_uploads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    // lock 2, write, unlock; copy 2 to 4; lock 2, write, unlock; read 4 on the page
    let all = [0; 4];
    run(0, Some(&[(0, 2, all, 0), (3, 2, all, 0), (2, 2, all, 0), (8, 2, all, 0), (0, 2, all, 0), (3, 2, all, 0), (2, 2, all, 0), (9, 4, all, 0)]));
}

/// The big surface's whole-surface locks upload only the rows that changed since the last one
/// (render/d3d.rs keeps a copy): a lock of part of it in between changes the page's pixels too.
#[test]
fn d3d9_lock_changed_rows_after_part() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let (all, part) = ([0; 4], [1, 1, 2, 2]);
    // whole: write 1 (twice: the copy is made); part: write 2; whole: write 1 again where 2 is
    run(0, Some(&[
        (0, 1, all, 0), (3, 1, all, 0), (2, 1, all, 0),
        (0, 1, all, 0), (3, 1, all, 1), (2, 1, all, 0),
        (0, 1, part, 0), (3, 1, part, 0), (2, 1, all, 0),
        (0, 1, all, 0), (3, 1, part, 1), (2, 1, all, 0),
        (9, 1, all, 0),
    ]));
}

/// The lock traces again with no room for whole-lock copies, and with room for none but the
/// smallest: what a lock reads and what the page holds are checked as before (a surface whose
/// copy went is uploaded whole).
#[test]
fn d3d9_lock_trace_with_small_copy_budgets() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let budget = &device::SHADOW_BUDGET;
    let was = budget.load(std::sync::atomic::Ordering::Relaxed);
    for b in [0, 1 << 18, 2 << 20] {
        budget.store(b, std::sync::atomic::Ordering::Relaxed);
        // (copies made under another budget, by the runs before: gone)
        with(|d| {
            for o in d.objs.values_mut() {
                if let Data::Surface(s) = &mut o.data {
                    s.shadow = None;
                }
            }
        });
        for seed in 0..seeds().min(12) {
            run(seed, None);
        }
        let all = [0; 4];
        run(0, Some(&[(0, 1, all, 0), (3, 1, all, 0), (2, 1, all, 0), (0, 1, all, 0), (3, 1, all, 1), (2, 1, all, 0), (0, 1, all, 0), (3, 1, all, 0), (2, 1, all, 0), (9, 1, all, 0)]));
        // (what is kept is within the budget)
        let kept: usize = with(|d| d.objs.values().filter_map(|o| match &o.data { Data::Surface(s) => s.shadow.as_ref().map(|x| x.len()), _ => None }).sum());
        assert!(kept <= b, "{kept} bytes of copies with a budget of {b}");
    }
    budget.store(was, std::sync::atomic::Ordering::Relaxed);
}
