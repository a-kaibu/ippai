//! Random Set*/Get* calls on a few device states of every kind a state block may hold or not,
//! CreateStateBlock of each type, BeginStateBlock / EndStateBlock, Capture and Apply, through
//! the real entry point (`vnhost_d3d9_call`), recorded as a trace for TLC to check against
//! formal/tla/StateBlocks.tla (formal/trace.sh): what every call returned and every Get read.
//!
//! The states (a key each): ALPHAREF (pixel), CULLMODE (vertex), SHADEMODE (both), stage 0's
//! COLOROP (pixel) and TEXCOORDINDEX (both), sampler 0's ADDRESSU (pixel) and DMAPOFFSET
//! (D3DSBT_ALL's only), texture 0, the vertex and pixel shaders, constant 0 of each, the view transform,
//! the viewport, light 2. A value is 0..3 (objects: 0 none, 1 or 2 one of two).
//!
//! `IPPAI_TRACE_DIR=dir` writes `dir/d3d9sb-<seed>/`; `IPPAI_TRACE_SEEDS=n` (default 20).

use super::trace_tests::{alloc, seeds, write_module, Rng, CALLS, SERIAL};
use super::*;
use std::fmt::Write as _;

pub(crate) const KEYS: [&str; 15] = ["rsP", "rsV", "rsB", "tssP", "tssB", "ssP", "ssV", "tex", "vs", "ps", "vsf", "psf", "xf", "vp", "light"];

fn call(iface: u32, m: u32, args: &[u32]) -> u32 {
    // (padded like the guest stack: the Direct3D 8 forwards read 10 words)
    let mut a = args.to_vec();
    a.resize(a.len().max(16), 0);
    unsafe { vnhost_d3d9_call(iface << 8 | m, a.as_ptr(), &CALLS) }
}
fn mem(n: u32) -> u32 {
    unsafe { alloc(4 * n) as u32 }
}
fn rd(p: u32, i: u32) -> u32 {
    unsafe { *((p + 4 * i) as *const u32) }
}
fn wr(p: u32, i: u32, v: u32) {
    unsafe { *((p + 4 * i) as *mut u32) = v }
}

struct Dev {
    dev: u32,
    tex: [u32; 2],
    vs: [u32; 2],
    ps: [u32; 2],
    buf: u32,
}

impl Dev {
    /// (an object value: 0 none, 1 / 2 the first / second; what an object reads as)
    fn obj(&self, set: &[u32; 2], v: u32) -> u32 {
        if v == 0 { 0 } else { set[(v - 1) as usize % 2] }
    }
    fn which(&self, set: &[u32; 2], o: u32) -> i64 {
        if o == 0 { 0 } else { set.iter().position(|&x| x == o).map_or(-1, |i| i as i64 + 1) }
    }
    fn set(&self, k: usize, v: u32) {
        let (d, b) = (self.dev, self.buf);
        let r = match KEYS[k] {
            "rsP" => call(IF_DEVICE, 57, &[d, 24, v]),
            "rsV" => call(IF_DEVICE, 57, &[d, 22, v + 1]),
            "rsB" => call(IF_DEVICE, 57, &[d, 9, v + 1]),
            "tssP" => call(IF_DEVICE, 67, &[d, 0, 1, v + 1]),
            "tssB" => call(IF_DEVICE, 67, &[d, 0, 11, v]),
            "ssP" => call(IF_DEVICE, 69, &[d, 0, 1, v + 1]),
            "ssV" => call(IF_DEVICE, 69, &[d, 0, 13, v]),
            "tex" => call(IF_DEVICE, 65, &[d, 0, self.obj(&self.tex, v)]),
            "vs" => call(IF_DEVICE, 92, &[d, self.obj(&self.vs, v)]),
            "ps" => call(IF_DEVICE, 107, &[d, self.obj(&self.ps, v)]),
            "vsf" | "psf" => {
                wr(b, 0, (v as f32).to_bits());
                call(IF_DEVICE, if KEYS[k] == "vsf" { 94 } else { 109 }, &[d, 0, b, 1])
            }
            "xf" => {
                for i in 0..16 {
                    wr(b, i, if i == 0 { (v as f32).to_bits() } else { 0 });
                }
                call(IF_DEVICE, 44, &[d, 2, b])
            }
            "vp" => {
                for (i, x) in [v, 0, 32, 32, 0, 1f32.to_bits()].into_iter().enumerate() {
                    wr(b, i as u32, x);
                }
                call(IF_DEVICE, 47, &[d, b])
            }
            _ => {
                for i in 0..26 {
                    wr(b, i, 0);
                }
                wr(b, 0, 1);
                wr(b, 19, (v as f32 + 1.0).to_bits());
                call(IF_DEVICE, 51, &[d, 2, b])
            }
        };
        assert_eq!(r, 0, "Set {}", KEYS[k]);
    }
    fn get(&self, k: usize) -> i64 {
        let (d, b) = (self.dev, self.buf);
        let obj = |m: u32, args: &[u32]| {
            call(IF_DEVICE, m, args);
            let o = rd(b, 0);
            if o != 0 {
                let id = D3D.lock().unwrap().as_ref().unwrap().id_of(o);
                let iface = D3D.lock().unwrap().as_ref().unwrap().objs[&id].iface;
                call(iface, 2, &[o]);
            }
            o
        };
        match KEYS[k] {
            "rsP" | "rsV" | "rsB" => {
                let s = [24, 22, 9][k];
                call(IF_DEVICE, 58, &[d, s, b]);
                rd(b, 0) as i64 - (k != 0) as i64
            }
            "tssP" | "tssB" => {
                call(IF_DEVICE, 66, &[d, 0, if k == 3 { 1 } else { 11 }, b]);
                rd(b, 0) as i64 - (k == 3) as i64
            }
            "ssP" | "ssV" => {
                call(IF_DEVICE, 68, &[d, 0, if k == 5 { 1 } else { 13 }, b]);
                rd(b, 0) as i64 - (k == 5) as i64
            }
            "tex" => self.which(&self.tex, obj(64, &[d, 0, b])),
            "vs" => self.which(&self.vs, obj(93, &[d, b])),
            "ps" => self.which(&self.ps, obj(108, &[d, b])),
            "vsf" | "psf" => {
                call(IF_DEVICE, if KEYS[k] == "vsf" { 95 } else { 110 }, &[d, 0, b, 1]);
                f32::from_bits(rd(b, 0)) as i64
            }
            "xf" => {
                call(IF_DEVICE, 45, &[d, 2, b]);
                f32::from_bits(rd(b, 0)) as i64
            }
            "vp" => {
                call(IF_DEVICE, 48, &[d, b]);
                rd(b, 0) as i64
            }
            _ => {
                call(IF_DEVICE, 52, &[d, 2, b]);
                f32::from_bits(rd(b, 19)) as i64 - 1
            }
        }
    }
}

#[derive(Default, Clone)]
pub(crate) struct Step {
    pub op: &'static str,
    /// the state (Set, Get), the block (Capture, Apply), the type (Create)
    pub k: i64,
    pub v: i64,
    pub ret: &'static str,
    /// the block made (Create, End), what Get read
    pub got: i64,
}

fn code(r: u32) -> &'static str {
    match r {
        0 => "ok",
        D3DERR_INVALIDCALL => "invalid",
        _ => "other",
    }
}

fn run(seed: u64) -> Vec<Step> {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 3);
    let o = mem(4);
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o, 0);
    let pp = mem(14);
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        wr(pp, i as u32, x);
    }
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o, 0);
    let mut d = Dev { dev, tex: [0; 2], vs: [0; 2], ps: [0; 2], buf: mem(32) };
    let vs_code = [0xfffe0200u32, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0xc00f0000, 0x90e40000, 0x0000ffff];
    let ps_code = [0xffff0200u32, 0x02000001, 0x800f0800, 0xa0e40000, 0x0000ffff];
    let code_at = |c: &[u32]| {
        let p = mem(c.len() as u32);
        for (i, x) in c.iter().enumerate() {
            wr(p, i as u32, *x);
        }
        p
    };
    let (vc, pc) = (code_at(&vs_code), code_at(&ps_code));
    for i in 0..2 {
        assert_eq!(call(IF_DEVICE, 23, &[dev, 4, 4, 1, 0, 21, 1, o, 0]), 0);
        d.tex[i] = rd(o, 0);
        assert_eq!(call(IF_DEVICE, 91, &[dev, vc, o]), 0);
        d.vs[i] = rd(o, 0);
        assert_eq!(call(IF_DEVICE, 106, &[dev, pc, o]), 0);
        d.ps[i] = rd(o, 0);
    }
    let mut steps = Vec::new();
    // (every state known first)
    for k in 0..KEYS.len() {
        d.set(k, 0);
        steps.push(Step { op: "Set", k: k as i64, v: 0, ret: "ok", ..Default::default() });
    }
    let mut blocks: Vec<u32> = Vec::new();
    let mut recording = false;
    for _ in 0..250 {
        let mut t = Step::default();
        let roll = r.below(100);
        match roll {
            0..=29 => {
                let (k, v) = (r.below(KEYS.len()), r.below(3) as u32);
                d.set(k, v);
                (t.op, t.k, t.v, t.ret) = ("Set", k as i64, v as i64, "ok");
            }
            30..=49 => {
                let k = r.below(KEYS.len());
                (t.op, t.k, t.ret, t.got) = ("Get", k as i64, "ok", d.get(k));
            }
            50..=57 => {
                // (types 1..3; 0 and 4 now and then)
                let ty = if r.chance(10) { [0, 4][r.below(2)] } else { 1 + r.below(3) as u32 };
                wr(o, 0, 0);
                let res = call(IF_DEVICE, 59, &[dev, ty, o]);
                if res == 0 {
                    blocks.push(rd(o, 0));
                }
                (t.op, t.k, t.ret, t.got) = ("Create", ty as i64, code(res), if res == 0 { blocks.len() as i64 } else { 0 });
            }
            58..=61 => {
                let res = call(IF_DEVICE, 60, &[dev]);
                recording |= res == 0;
                (t.op, t.ret) = ("Begin", code(res));
            }
            62..=72 => {
                wr(o, 0, 0);
                let res = call(IF_DEVICE, 61, &[dev, o]);
                if res == 0 {
                    blocks.push(rd(o, 0));
                    recording = false;
                }
                (t.op, t.ret, t.got) = ("End", code(res), if res == 0 { blocks.len() as i64 } else { 0 });
            }
            _ if blocks.is_empty() || recording => continue,
            73..=84 => {
                let b = r.below(blocks.len());
                (t.op, t.k, t.ret) = ("Capture", b as i64 + 1, code(call(IF_STATEBLOCK, 4, &[blocks[b]])));
            }
            _ => {
                let b = r.below(blocks.len());
                (t.op, t.k, t.ret) = ("Apply", b as i64 + 1, code(call(IF_STATEBLOCK, 5, &[blocks[b]])));
            }
        }
        steps.push(t);
    }
    if recording {
        call(IF_DEVICE, 61, &[dev, o]);
        blocks.push(rd(o, 0));
    }
    for b in blocks {
        call(IF_STATEBLOCK, 2, &[b]);
    }
    for i in 0..2 {
        call(IF_DEVICE, 65, &[dev, 0, 0]);
        call(IF_DEVICE, 92, &[dev, 0]);
        call(IF_DEVICE, 107, &[dev, 0]);
        call(IF_TEXTURE, 2, &[d.tex[i]]);
        call(IF_VS, 2, &[d.vs[i]]);
        call(IF_PS, 2, &[d.ps[i]]);
    }
    call(IF_DEVICE, 2, &[dev]);
    call(IF_D3D9, 2, &[d3d]);
    steps
}

/// Which keys a type of block holds (StateBlocks.tla's Pixel / Vertex).
pub(crate) fn holds(ty: i64, k: usize) -> bool {
    match ty {
        0 => false,
        1 => true,
        2 => matches!(KEYS[k], "rsP" | "rsB" | "tssP" | "tssB" | "ssP" | "ps" | "psf"),
        _ => matches!(KEYS[k], "rsV" | "rsB" | "tssB" | "vs" | "vsf" | "light"),
    }
}

/// StateBlocks.tla in Rust (the test fails here too, with the step, when TLC is not run).
fn check(steps: &[Step], seed: u64) {
    let n = KEYS.len();
    let mut dev = vec![-1i64; n];
    let mut rec: Option<(Vec<i64>, Vec<bool>)> = None;
    let mut blocks: Vec<(Vec<i64>, Vec<bool>)> = Vec::new();
    for (i, t) in steps.iter().enumerate() {
        let fail = |what: String| panic!("seed {seed} step {i}: {} {}: {what}", t.op, t.k);
        let (ret, got) = match t.op {
            "Set" => {
                let k = t.k as usize;
                match &mut rec {
                    Some((st, m)) => {
                        st[k] = t.v;
                        m[k] = true;
                    }
                    None => dev[k] = t.v,
                }
                ("ok", 0)
            }
            "Get" => ("ok", dev[t.k as usize]),
            "Create" => {
                if rec.is_some() || !(0..=3).contains(&t.k) {
                    ("invalid", 0)
                } else {
                    let m: Vec<bool> = (0..n).map(|k| holds(t.k, k)).collect();
                    blocks.push((dev.iter().zip(&m).map(|(v, h)| if *h { *v } else { -1 }).collect(), m));
                    ("ok", blocks.len() as i64)
                }
            }
            "Begin" => {
                if rec.is_some() {
                    ("invalid", 0)
                } else {
                    rec = Some((dev.clone(), vec![false; n]));
                    ("ok", 0)
                }
            }
            "End" => match rec.take() {
                None => ("invalid", 0),
                Some((st, m)) => {
                    blocks.push((st.iter().zip(&m).map(|(v, h)| if *h { *v } else { -1 }).collect(), m));
                    ("ok", blocks.len() as i64)
                }
            },
            "Capture" => {
                let b = &mut blocks[t.k as usize - 1];
                for k in 0..n {
                    if b.1[k] {
                        b.0[k] = dev[k];
                    }
                }
                ("ok", 0)
            }
            _ => {
                let b = &blocks[t.k as usize - 1];
                for k in 0..n {
                    if b.1[k] {
                        dev[k] = b.0[k];
                    }
                }
                ("ok", 0)
            }
        };
        if t.ret != ret || t.got != got {
            fail(format!("gave {} {}, want {ret} {got}", t.ret, t.got));
        }
    }
}

#[test]
fn d3d9_state_block_trace() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    for seed in 0..seeds() {
        let steps = run(seed);
        let mut s = String::from("---- MODULE SbData ----\nEXTENDS Integers\nTrace == <<\n");
        for (i, t) in steps.iter().enumerate() {
            let _ = writeln!(
                s,
                "  [op |-> \"{}\", k |-> {}, v |-> {}, ret |-> \"{}\", got |-> {}]{}",
                t.op, t.k, t.v, t.ret, t.got,
                if i + 1 < steps.len() { "," } else { "" }
            );
        }
        s.push_str(">>\n====\n");
        write_module(&format!("d3d9sb-{seed}"), "StateBlocks", "SbData", &s);
        check(&steps, seed);
    }
}

/// Every float constant through a block of each type on a software vertex processing device,
/// as winref d3d9sb's runs on Windows (golden 140122a): D3DSBT_ALL and D3DSBT_VERTEXSTATE hold
/// all 8192 vertex shader constants, D3DSBT_ALL and D3DSBT_PIXELSTATE all 224 pixel shader
/// ones, and neither holds the other's.
#[test]
fn d3d9_state_block_constants_as_windows() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let o = mem(4);
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o, 0);
    let pp = mem(14);
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        wr(pp, i as u32, x);
    }
    // (D3DCREATE_SOFTWARE_VERTEXPROCESSING)
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    let dev = rd(o, 0);
    let b = mem(4);
    let set = |m: u32, i: u32, v: f32| {
        wr(b, 0, v.to_bits());
        assert_eq!(call(IF_DEVICE, m, &[dev, i, b, 1]), 0);
    };
    let get = |m: u32, i: u32| {
        wr(b, 0, 0);
        assert_eq!(call(IF_DEVICE, m + 1, &[dev, i, b, 1]), 0);
        f32::from_bits(rd(b, 0))
    };
    // (SetVertexShaderConstantF 94, SetPixelShaderConstantF 109; types ALL 1, PIXEL 2, VERTEX 3)
    for (m, n, held_by) in [(94u32, 8192u32, [true, false, true]), (109, 224, [true, true, false])] {
        for i in 0..n {
            for (t, &want) in held_by.iter().enumerate() {
                set(m, i, 3.0);
                assert_eq!(call(IF_DEVICE, 59, &[dev, t as u32 + 1, o]), 0);
                let sb = rd(o, 0);
                set(m, i, 4.0);
                assert_eq!(call(IF_STATEBLOCK, 5, &[sb]), 0);
                assert_eq!(get(m, i) == 3.0, want, "method {m} register {i} type {}", t + 1);
                call(IF_STATEBLOCK, 2, &[sb]);
            }
        }
        // (past the last register: refused)
        wr(b, 0, 0);
        assert_ne!(call(IF_DEVICE, m, &[dev, n, b, 1]), 0);
    }
    call(IF_DEVICE, 2, &[dev]);
    call(IF_D3D9, 2, &[d3d]);
}

/// Recorded blocks on a software vertex processing device (8192 float, 2048 integer and
/// boolean vertex constants) hold every register their Set* calls named, far ones too: a
/// model of random recorded ranges (anywhere in the registers, up to the end), then other values
/// set everywhere, Apply; each register read back is the recorded value where the block holds
/// it, the later one elsewhere. And a D3DSBT_VERTEXSTATE block captures all of them.
#[test]
fn recorded_blocks_hold_far_constants() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let o = mem(4);
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o, 0);
    let pp = mem(14);
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        wr(pp, i as u32, x);
    }
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    let dev = rd(o, 0);
    // (method, registers, words per register)
    const KINDS: [(u32, u32, u32); 3] = [(94, 8192, 4), (96, 2048, 4), (98, 2048, 1)];
    let buf = mem(8192 * 4);
    let set = |m: u32, start: u32, n: u32, per: u32, v: &dyn Fn(u32) -> u32| {
        for i in 0..n * per {
            wr(buf, i, v(start * per + i));
        }
        assert_eq!(call(IF_DEVICE, m, &[dev, start, buf, n]), 0, "Set {m} {start}+{n}");
    };
    let get_all = |m: u32, n: u32, per: u32| -> Vec<u32> {
        assert_eq!(call(IF_DEVICE, m + 1, &[dev, 0, buf, n]), 0);
        (0..n * per).map(|i| rd(buf, i)).collect()
    };
    // (values: floats and ints any bits; booleans 0 or 1)
    let val = |m: u32, tag: u32, i: u32| if m == 98 { (tag ^ i) & 1 } else { tag.wrapping_mul(0x9e37_79b9) ^ i.wrapping_mul(0x85eb_ca6b) & 0x3fff_ffff };
    let mut r = Rng(0xc0_57a7e);
    for round in 0..12 {
        let (t1, t2) = (2 * round + 1, 2 * round + 2);
        let mut held: Vec<Vec<bool>> = KINDS.iter().map(|k| vec![false; k.1 as usize]).collect();
        assert_eq!(call(IF_DEVICE, 60, &[dev]), 0);
        for _ in 0..1 + r.below(6) {
            let k = r.below(3);
            let (m, n, per) = KINDS[k];
            let start = match r.below(3) {
                0 => r.below(16) as u32,
                1 => n - 1 - r.below(8) as u32,
                _ => r.below(n as usize) as u32,
            };
            let count = (1 + r.below(300) as u32).min(n - start);
            set(m, start, count, per, &|i| val(m, t1, i));
            held[k][start as usize..(start + count) as usize].fill(true);
        }
        assert_eq!(call(IF_DEVICE, 61, &[dev, o]), 0);
        let sb = rd(o, 0);
        for (m, n, per) in KINDS {
            set(m, 0, n, per, &|i| val(m, t2, i));
        }
        assert_eq!(call(IF_STATEBLOCK, 5, &[sb]), 0);
        for (k, (m, n, per)) in KINDS.into_iter().enumerate() {
            let got = get_all(m, n, per);
            for reg in 0..n {
                let tag = if held[k][reg as usize] { t1 } else { t2 };
                for w in 0..per {
                    let i = reg * per + w;
                    assert_eq!(got[i as usize], val(m, tag, i), "round {round}: method {m} register {reg} (held {})", held[k][reg as usize]);
                }
            }
        }
        call(IF_STATEBLOCK, 2, &[sb]);
    }
    // (D3DSBT_VERTEXSTATE: every vertex constant)
    for (m, n, per) in KINDS {
        set(m, 0, n, per, &|i| val(m, 100, i));
    }
    assert_eq!(call(IF_DEVICE, 59, &[dev, 3, o]), 0);
    let sb = rd(o, 0);
    for (m, n, per) in KINDS {
        set(m, 0, n, per, &|i| val(m, 101, i));
    }
    assert_eq!(call(IF_STATEBLOCK, 5, &[sb]), 0);
    for (m, n, per) in KINDS {
        let got = get_all(m, n, per);
        assert!((0..n * per).all(|i| got[i as usize] == val(m, 100, i)), "method {m}");
    }
    call(IF_STATEBLOCK, 2, &[sb]);
    call(IF_DEVICE, 2, &[dev]);
    call(IF_D3D9, 2, &[d3d]);
}
