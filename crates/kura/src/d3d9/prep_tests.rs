//! What a draw reuses from the last one (#118: `device::Prep`, its uniform blocks) against what
//! it prepares from the device's state from scratch: random sequences of the calls that change
//! the state (render, stage and sampler states set again or changed, textures of other kinds,
//! FVFs and declarations, streams of other strides, instancing, shaders, constants, transforms,
//! lights, targets and depth buffers, state blocks recorded, captured and applied, Reset,
//! textures released and made again) and every kind of draw, run with MASU_PREP_CHECK (each
//! draw that reuses prepares again and compares): nothing may differ. The page is the model of
//! WebGPU: no WebGPU rule may be broken either.

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, CALLS, SERIAL};
use super::*;
use super::device::{set_prep_mode, PREP_DIFFS};
use proptest::prelude::*;
use std::sync::atomic::Ordering::Relaxed;

#[derive(Clone, Debug)]
enum Op {
    Rs(u32, u32),
    Tss(u32, u32, u32),
    Ss(u32, u32, u32),
    Tex(u32, usize),
    Fvf(usize),
    Decl(usize),
    Stream(u32, usize, u32, u32),
    Freq(u32, u32),
    Indices(usize),
    Vs(usize),
    Ps(usize),
    Transform(u32, f32),
    Viewport(u32, u32, u32, u32),
    Material(f32),
    Light(u32, u32),
    LightEnable(u32, bool),
    Const(bool, u32, f32),
    Scissor(i32, i32),
    Target(usize),
    Depth(usize),
    Block(u32),
    Apply(usize, bool),
    Record(bool),
    Remake,
    Reset,
    Draw(u32, u32, u32, u32),
    Present,
}

const RS: [u32; 24] = [7, 9, 14, 15, 19, 20, 22, 23, 24, 25, 27, 29, 52, 60, 137, 139, 141, 145, 168, 171, 174, 193, 206, 207];
const TSS: [u32; 10] = [1, 2, 3, 4, 5, 6, 11, 24, 26, 32];
const SS: [u32; 7] = [1, 2, 3, 4, 5, 6, 7];
const FVFS: [u32; 5] = [0x144, 0x142, 0x104, 0x152, 0x244];

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        10 => (prop::sample::select(RS.to_vec()), 0u32..9).prop_map(|(s, v)| Op::Rs(s, v)),
        3 => (0u32..3, prop::sample::select(TSS.to_vec()), 0u32..9).prop_map(|(a, s, v)| Op::Tss(a, s, v)),
        2 => (0u32..3, prop::sample::select(SS.to_vec()), 0u32..5).prop_map(|(a, s, v)| Op::Ss(a, s, v)),
        4 => (0u32..3, 0usize..5).prop_map(|(s, t)| Op::Tex(s, t)),
        2 => (0usize..FVFS.len()).prop_map(Op::Fvf),
        2 => (0usize..3).prop_map(Op::Decl),
        3 => (0u32..2, 0usize..3, 0u32..4, prop::sample::select(vec![0u32, 24, 28, 32, 36, 40])).prop_map(|(s, b, o, st)| Op::Stream(s, b, 4 * o, st)),
        1 => (0u32..2, prop::sample::select(vec![1u32, 0x4000_0002, 0x8000_0001, 0x8000_0002])).prop_map(|(s, v)| Op::Freq(s, v)),
        1 => (0usize..3).prop_map(Op::Indices),
        1 => (0usize..2).prop_map(Op::Vs),
        1 => (0usize..2).prop_map(Op::Ps),
        2 => (prop::sample::select(vec![2u32, 3, 256, 16]), -2.0f32..2.0).prop_map(|(t, f)| Op::Transform(t, f)),
        1 => (0u32..8, 0u32..8, 8u32..64, 8u32..64).prop_map(|(x, y, w, h)| Op::Viewport(x, y, w, h)),
        1 => (0.0f32..1.0).prop_map(Op::Material),
        1 => (0u32..3, 1u32..4).prop_map(|(i, t)| Op::Light(i, t)),
        1 => (0u32..3, any::<bool>()).prop_map(|(i, on)| Op::LightEnable(i, on)),
        2 => (any::<bool>(), 0u32..4, -1.0f32..1.0).prop_map(|(v, r, f)| Op::Const(v, r, f)),
        1 => (0i32..32, 0i32..32).prop_map(|(x, y)| Op::Scissor(x, y)),
        1 => (0usize..2).prop_map(Op::Target),
        1 => (0usize..3).prop_map(Op::Depth),
        1 => (1u32..4).prop_map(Op::Block),
        1 => (0usize..4, any::<bool>()).prop_map(|(b, a)| Op::Apply(b, a)),
        1 => any::<bool>().prop_map(Op::Record),
        1 => Just(Op::Remake),
        1 => Just(Op::Reset),
        24 => (0u32..4, 1u32..7, 1u32..4, 0u32..6).prop_map(|(k, p, n, s)| Op::Draw(k, p, n, s)),
        1 => Just(Op::Present),
    ]
}

fn f32s(p: u32, v: &[f32]) {
    put_bytes(p, &v.iter().flat_map(|x| x.to_le_bytes()).collect::<Vec<_>>());
}

fn words(w: &[u32]) -> u32 {
    let p = unsafe { alloc(4 * w.len() as u32 + 4) as u32 };
    put_bytes(p, &w.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    p
}

struct World {
    dev: u32,
    tex: Vec<u32>,
    vbs: Vec<u32>,
    ibs: Vec<u32>,
    decls: Vec<u32>,
    vs: u32,
    ps: u32,
    targets: Vec<u32>,
    depths: Vec<u32>,
    blocks: Vec<u32>,
    up: u32,
    upi: u32,
    m: u32,
}

fn texture(dev: u32, fmt: u32) -> u32 {
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, 4, 4, 1, 0, fmt, 1, o, 0]), 0);
    rd(o)
}

fn world() -> World {
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 1, 75, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o);
    // 2D textures with alpha and without, a cube texture
    let mut tex = vec![0, texture(dev, 21), texture(dev, 21), texture(dev, 22)];
    let o = out();
    assert_eq!(call(IF_DEVICE, 25, &[dev, 4, 1, 0, 21, 1, o, 0]), 0);
    tex.push(rd(o));
    // vertex buffers of finite floats, index buffers of indices below 24
    let mut vbs = vec![0];
    for k in 0..2u32 {
        let o = out();
        assert_eq!(call(IF_DEVICE, 26, &[dev, 4096, 0, 0, 1, o, 0]), 0);
        let vb = rd(o);
        let p = out();
        assert_eq!(call(IF_VB, 11, &[vb, 0, 0, p, 0]), 0);
        f32s(rd(p), &(0..1024).map(|i| ((i * 7 + k) % 61) as f32 * 0.5 - 4.0).collect::<Vec<_>>());
        assert_eq!(call(IF_VB, 12, &[vb]), 0);
        vbs.push(vb);
    }
    let mut ibs = vec![0];
    for (fmt, isz) in [(101u32, 2u32), (102, 4)] {
        let o = out();
        assert_eq!(call(IF_DEVICE, 27, &[dev, 1024, 0, fmt, 1, o, 0]), 0);
        let ib = rd(o);
        let p = out();
        assert_eq!(call(IF_IB, 11, &[ib, 0, 0, p, 0]), 0);
        let ix: Vec<u8> = (0..1024 / isz).flat_map(|i| if isz == 2 { ((i * 5 % 24) as u16).to_le_bytes().to_vec() } else { (i * 5 % 24).to_le_bytes().to_vec() }).collect();
        put_bytes(rd(p), &ix);
        assert_eq!(call(IF_IB, 12, &[ib]), 0);
        ibs.push(ib);
    }
    // declarations: position + colour + coordinates in one stream; position in 0, the rest in
    // 1; pretransformed
    let layouts: [&[(u32, u32, u32, u32, u32)]; 3] = [
        &[(0, 0, 2, 0, 0), (0, 12, 4, 10, 0), (0, 16, 1, 5, 0)],
        &[(0, 0, 2, 0, 0), (1, 0, 4, 10, 0), (1, 4, 1, 5, 0)],
        &[(0, 0, 3, 9, 0), (0, 16, 4, 10, 0), (0, 20, 1, 5, 0)],
    ];
    let decls = layouts
        .iter()
        .map(|l| {
            let p = unsafe { alloc(8 * (l.len() as u32 + 1)) as u32 };
            for (i, &(s, off, t, u, ui)) in l.iter().enumerate() {
                put_bytes(p + 8 * i as u32, &[(s | off << 16).to_le_bytes(), (t | u << 16 | ui << 24).to_le_bytes()].concat());
            }
            put_bytes(p + 8 * l.len() as u32, &[0xff, 0, 0, 0, 17, 0, 0, 0]);
            let o = out();
            assert_eq!(call(IF_DEVICE, 86, &[dev, p, o]), 0);
            rd(o)
        })
        .collect();
    let o = out();
    assert_eq!(call(IF_DEVICE, 91, &[dev, words(&[0xfffe0200, 0x0200001f, 0x80000000, 0x900f0000, 0x02000001, 0xc00f0000, 0x90e40000, 0x0000ffff]), o]), 0);
    let vs = rd(o);
    assert_eq!(call(IF_DEVICE, 106, &[dev, words(&[0xffff0200, 0x02000001, 0x800f0800, 0xa0e40000, 0x0000ffff]), o]), 0);
    let ps = rd(o);
    // the back buffer and a render target; the automatic depth buffer and one of another size
    assert_eq!(call(IF_DEVICE, 18, &[dev, 0, 0, 0, o]), 0);
    let bb = rd(o);
    assert_eq!(call(IF_DEVICE, 28, &[dev, 64, 64, 21, 0, 0, 0, o, 0]), 0);
    let targets = vec![bb, rd(o)];
    assert_eq!(call(IF_DEVICE, 40, &[dev, o]), 0);
    let auto = rd(o);
    assert_eq!(call(IF_DEVICE, 29, &[dev, 32, 32, 75, 0, 0, 0, o, 0]), 0);
    let depths = vec![0, auto, rd(o)];
    World { dev, tex, vbs, ibs, decls, vs, ps, targets, depths, blocks: Vec::new(), up: unsafe { alloc(4096) as u32 }, upi: unsafe { alloc(256) as u32 }, m: unsafe { alloc(256) as u32 } }
}

fn apply(w: &mut World, op: &Op) {
    let dev = w.dev;
    match *op {
        Op::Rs(s, v) => {
            // (blend factors and operations of their own ranges)
            let v = match s {
                19 | 20 | 207 => 1 + v % 13,
                171 => 1 + v % 5,
                23 | 25 => 1 + v % 8,
                168 => v * 0x22 % 16,
                24 | 60 | 193 | 139 => v.wrapping_mul(0x1f3a_5c7d),
                _ => v % 3,
            };
            call(IF_DEVICE, 57, &[dev, s, v]);
        }
        Op::Tss(st, s, v) => {
            call(IF_DEVICE, 67, &[dev, st, s, if s == 32 { v.wrapping_mul(0x0101_0101) } else { v }]);
        }
        Op::Ss(st, s, v) => {
            call(IF_DEVICE, 69, &[dev, st, s, v]);
        }
        Op::Tex(st, t) => {
            call(IF_DEVICE, 65, &[dev, st, w.tex[t]]);
        }
        Op::Fvf(k) => {
            call(IF_DEVICE, 89, &[dev, FVFS[k]]);
        }
        Op::Decl(k) => {
            call(IF_DEVICE, 87, &[dev, w.decls[k]]);
        }
        Op::Stream(s, b, off, stride) => {
            call(IF_DEVICE, 100, &[dev, s, w.vbs[b], off, stride]);
        }
        Op::Freq(s, v) => {
            call(IF_DEVICE, 102, &[dev, s, v]);
        }
        Op::Indices(k) => {
            call(IF_DEVICE, 104, &[dev, w.ibs[k]]);
        }
        Op::Vs(k) => {
            call(IF_DEVICE, 92, &[dev, [0, w.vs][k]]);
        }
        Op::Ps(k) => {
            call(IF_DEVICE, 107, &[dev, [0, w.ps][k]]);
        }
        Op::Transform(t, f) => {
            f32s(w.m, &[1.0, f, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, f, 0.0, 0.5, 1.0]);
            call(IF_DEVICE, 44, &[dev, t, w.m]);
        }
        Op::Viewport(x, y, ww, h) => {
            put_bytes(w.m, &[x, y, ww, h, 0, 1f32.to_bits()].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
            call(IF_DEVICE, 47, &[dev, w.m]);
        }
        Op::Material(f) => {
            f32s(w.m, &[f; 17]);
            call(IF_DEVICE, 49, &[dev, w.m]);
        }
        Op::Light(i, t) => {
            let mut l = vec![0u8; 104];
            l[..4].copy_from_slice(&t.to_le_bytes());
            for k in 1..26 {
                l[4 * k..4 * k + 4].copy_from_slice(&(0.25f32 * k as f32).to_le_bytes());
            }
            put_bytes(w.m, &l);
            call(IF_DEVICE, 51, &[dev, i, w.m]);
        }
        Op::LightEnable(i, on) => {
            call(IF_DEVICE, 53, &[dev, i, on as u32]);
        }
        Op::Const(v, r, f) => {
            f32s(w.m, &[f, -f, 0.5, 1.0]);
            call(IF_DEVICE, if v { 94 } else { 109 }, &[dev, r, w.m, 1]);
        }
        Op::Scissor(x, y) => {
            put_bytes(w.m, &[x, y, x + 24, y + 24].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
            call(IF_DEVICE, 75, &[dev, w.m]);
        }
        Op::Target(k) => {
            call(IF_DEVICE, 37, &[dev, 0, w.targets[k]]);
        }
        Op::Depth(k) => {
            call(IF_DEVICE, 39, &[dev, w.depths[k]]);
        }
        Op::Block(ty) => {
            let o = out();
            if call(IF_DEVICE, 59, &[dev, ty, o]) == 0 {
                w.blocks.push(rd(o));
            }
        }
        Op::Apply(b, a) => {
            if let Some(&sb) = w.blocks.get(b % w.blocks.len().max(1)) {
                call(IF_STATEBLOCK, if a { 5 } else { 4 }, &[sb]);
            }
        }
        Op::Record(begin) => {
            if begin {
                call(IF_DEVICE, 60, &[dev]);
            } else {
                let o = out();
                if call(IF_DEVICE, 61, &[dev, o]) == 0 {
                    w.blocks.push(rd(o));
                }
            }
        }
        Op::Remake => {
            // a texture let go (unbound first: the device keeps a reference) and another made
            for s in 0..3 {
                call(IF_DEVICE, 65, &[dev, s, 0]);
            }
            call(IF_TEXTURE, 2, &[w.tex[2]]);
            w.tex[2] = texture(dev, [21, 22][w.tex.len() % 2]);
        }
        Op::Reset => {
            // (Reset with resources of the default pool alive fails: as on Windows)
            let pp = unsafe { alloc(56) as u32 };
            for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 1, 75, 0, 0, 0].into_iter().enumerate() {
                unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
            }
            call(IF_DEVICE, 16, &[dev, pp]);
        }
        Op::Draw(kind, prim, n, start) => {
            let count = [0, n, 2 * n, n + 1, 3 * n, n + 2, n + 2][prim as usize];
            match kind {
                0 => {
                    call(IF_DEVICE, 81, &[dev, prim, start, n]);
                }
                1 => {
                    call(IF_DEVICE, 82, &[dev, prim, start % 3, 0, 24, start, n]);
                }
                2 => {
                    f32s(w.up, &(0..(count * 10)).map(|i| (i % 13) as f32 - 3.0).collect::<Vec<_>>());
                    call(IF_DEVICE, 83, &[dev, prim, n, w.up, 28 + 4 * (start % 3)]);
                }
                _ => {
                    put_bytes(w.upi, &(0..count).flat_map(|i| (((i * 3 + start) % 8) as u16).to_le_bytes()).collect::<Vec<_>>());
                    f32s(w.up, &(0..100).map(|i| (i % 11) as f32 - 2.0).collect::<Vec<_>>());
                    call(IF_DEVICE, 84, &[dev, prim, 0, 8, n, w.upi, 101, w.up, 28]);
                }
            }
        }
        Op::Present => {
            call(IF_DEVICE, 42, &[dev]);
            call(IF_DEVICE, 17, &[dev, 0, 0, 0, 0]);
            gpu::pump();
            call(IF_DEVICE, 41, &[dev]);
        }
    }
}

fn run(ops: &[Op]) -> u64 {
    PREP_DIFFS.store(0, Relaxed);
    let mut w = world();
    call(IF_DEVICE, 41, &[w.dev]);
    for op in ops {
        apply(&mut w, op);
    }
    call(IF_DEVICE, 42, &[w.dev]);
    call(IF_DEVICE, 17, &[w.dev, 0, 0, 0, 0]);
    gpu::pump();
    PREP_DIFFS.load(Relaxed)
}

fn setup() -> std::sync::MutexGuard<'static, ()> {
    let g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    g
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 256, ..ProptestConfig::default() })]

    #[test]
    fn d3d9_reused_preparation_is_the_states(ops in prop::collection::vec(op(), 1..160)) {
        let _g = setup();
        attach_page();
        set_prep_mode(2);
        let diffs = run(&ops);
        set_prep_mode(0xff);
        gpu::set_pump(None);
        let errors = crate::render::model::with(|m| std::mem::take(&mut m.errors));
        prop_assert_eq!(diffs, 0, "the reused preparation differs (see stderr)");
        prop_assert!(errors.is_empty(), "WebGPU errors {:?}", &errors[..errors.len().min(5)]);
    }
}

/// Each kind of call `stale` lets by (the same value again, a uniform's state, another buffer of
/// the same stride, another texture of the same kind) between two draws, and each kind it must
/// not (a value changed, another kind of texture, another stride, a state block applied): the
/// second draw prepares as from scratch.
#[test]
fn d3d9_each_state_change_is_seen() {
    let _g = setup();
    attach_page();
    set_prep_mode(2);
    let ops: Vec<Op> = vec![
        Op::Fvf(0),
        Op::Stream(0, 1, 0, 28),
        Op::Tex(0, 1),
        Op::Draw(0, 5, 2, 0),
        Op::Rs(27, 1),
        Op::Draw(0, 5, 2, 0),
        Op::Rs(19, 4),
        Op::Draw(0, 5, 2, 0),
        Op::Rs(60, 3),
        Op::Draw(0, 5, 2, 0),
        Op::Tex(0, 3),
        Op::Draw(0, 5, 2, 0),
        Op::Tex(0, 4),
        Op::Draw(0, 5, 2, 0),
        Op::Tex(0, 0),
        Op::Draw(0, 5, 2, 0),
        Op::Stream(0, 2, 4, 28),
        Op::Draw(0, 5, 2, 0),
        Op::Stream(0, 2, 4, 32),
        Op::Draw(0, 5, 2, 0),
        Op::Tss(0, 32, 5),
        Op::Draw(0, 5, 2, 0),
        Op::Transform(3, 0.5),
        Op::Draw(0, 5, 2, 0),
        Op::Viewport(2, 2, 40, 40),
        Op::Draw(0, 5, 2, 0),
        Op::Const(false, 0, 0.25),
        Op::Draw(0, 5, 2, 0),
        Op::Tss(0, 1, 2),
        Op::Draw(0, 5, 2, 0),
        Op::Ss(0, 1, 3),
        Op::Draw(0, 5, 2, 0),
        Op::Block(1),
        Op::Rs(27, 0),
        Op::Fvf(2),
        Op::Draw(0, 5, 2, 0),
        Op::Apply(0, true),
        Op::Draw(0, 5, 2, 0),
        Op::Decl(1),
        Op::Stream(1, 1, 0, 24),
        Op::Draw(1, 4, 2, 0),
        Op::Freq(0, 0x4000_0002),
        Op::Freq(1, 0x8000_0001),
        Op::Draw(1, 4, 2, 0),
        Op::Target(1),
        Op::Draw(1, 4, 2, 0),
        Op::Depth(0),
        Op::Rs(7, 1),
        Op::Draw(1, 4, 2, 0),
        Op::Remake,
        Op::Tex(0, 2),
        Op::Draw(2, 5, 2, 0),
        Op::Draw(2, 5, 2, 1),
        Op::Draw(3, 4, 2, 0),
        Op::Reset,
        Op::Draw(2, 5, 2, 0),
    ];
    let diffs = run(&ops);
    set_prep_mode(0xff);
    gpu::set_pump(None);
    assert_eq!(diffs, 0);
}
