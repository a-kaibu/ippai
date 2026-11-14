//! Draws joined to the one before them (#120: device::draw, `merging`) against the same draws
//! one by one: random frames of 2D sprites (DrawPrimitiveUP strips and lists, pre-transformed,
//! overlapping, their colors' alpha blended in several ways), with the texture, the blend, the
//! write mask, the sampler's filter, the clip, a stage's operation and the texture's texels
//! changed between them, the screen read back (GetRenderTargetData) after each frame. The page
//! is the model of WebGPU, which assembles each draw's primitives (vertex for vertex, attribute
//! for attribute) and says what each draws with (pipeline, bind groups' contents, viewport, clip,
//! blend constant): the primitives the page draws, in order, each with what it is drawn with,
//! must be the same (the same triangle, the same facing), and no WebGPU rule broken. (The model's
//! texels are a draw's signature, not colors: they cannot be compared across a join.)

use super::device::{set_merge_mode, JOINED_HERE};
use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, attach_page, CALLS, SERIAL};
use super::*;
use crate::render::model;
use proptest::prelude::*;

const W: u32 = 48;

#[derive(Clone, Debug)]
enum Op {
    /// a sprite: x, y, size, its color, strip (else list), how many quads in the call
    Sprite(f32, f32, f32, u32, bool, u32),
    Tex(usize),
    Blend(u32, u32),
    BlendOn(bool),
    Mask(u32),
    Filter(u32),
    Scissor(bool, u32, u32),
    ColorOp(u32),
    /// a texel of texture k rewritten (LockRect)
    Texel(usize, u32, u32),
    /// a draw not of a sprite: a triangle fan
    Fan(u32),
    /// the frame shown, read back
    Present,
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        30 => (0f32..40.0, 0f32..40.0, 2f32..24.0, any::<u32>(), any::<bool>(), 1u32..4).prop_map(|(x, y, s, c, st, n)| Op::Sprite(x, y, s, c, st, n)),
        4 => (0usize..2).prop_map(Op::Tex),
        2 => prop::sample::select(vec![(5u32, 6u32), (2, 2), (5, 2), (1, 3), (9, 4)]).prop_map(|(a, b)| Op::Blend(a, b)),
        1 => any::<bool>().prop_map(Op::BlendOn),
        1 => prop::sample::select(vec![0xfu32, 0x7, 0x9, 0x2]).prop_map(Op::Mask),
        1 => prop::sample::select(vec![1u32, 2]).prop_map(Op::Filter),
        1 => (any::<bool>(), 0u32..24, 0u32..24).prop_map(|(on, a, b)| Op::Scissor(on, a, b)),
        1 => prop::sample::select(vec![4u32, 2, 3]).prop_map(Op::ColorOp),
        2 => (0usize..2, 0u32..8, any::<u32>()).prop_map(|(k, at, v)| Op::Texel(k, at, v)),
        1 => (0u32..40).prop_map(Op::Fan),
        2 => Just(Op::Present),
    ]
}

struct Scene {
    dev: u32,
    tex: [u32; 2],
    rt: u32,
    sys: u32,
    up: u32,
}

fn scene() -> Scene {
    attach_page();
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [W, W, 21, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    let dev = rd(o);
    let mut tex = [0; 2];
    for (k, t) in tex.iter_mut().enumerate() {
        let o = out();
        assert_eq!(call(IF_DEVICE, 23, &[dev, 4, 4, 1, 0, 21, 1, o, 0]), 0);
        *t = rd(o);
        let lr = out();
        assert_eq!(call(IF_TEXTURE, 19, &[*t, 0, lr, 0, 0]), 0);
        let (pitch, bits) = (rd(lr), rd(lr + 4));
        for y in 0..4u32 {
            let row: Vec<u8> = (0..4u32).flat_map(|x| (0x8000_0000u32 | (x * 60) << 16 | (y * 60) << 8 | (k as u32 * 200 + 30)).to_le_bytes()).collect();
            put_bytes(bits + y * pitch, &row);
        }
        assert_eq!(call(IF_TEXTURE, 20, &[*t, 0]), 0);
    }
    let o = out();
    assert_eq!(call(IF_DEVICE, 28, &[dev, W, W, 21, 0, 0, 0, o, 0]), 0);
    let rt = rd(o);
    let o = out();
    assert_eq!(call(IF_DEVICE, 36, &[dev, W, W, 21, 2, o, 0]), 0);
    let sys = rd(o);
    assert_eq!(call(IF_DEVICE, 37, &[dev, 0, rt]), 0);
    call(IF_DEVICE, 89, &[dev, 0x144]); // XYZRHW | DIFFUSE | TEX1
    for (rs, v) in [(22, 1), (137, 0), (7, 0), (27, 1), (19, 5), (20, 6)] {
        call(IF_DEVICE, 57, &[dev, rs, v]);
    }
    call(IF_DEVICE, 65, &[dev, 0, tex[0]]);
    let up = unsafe { alloc(64 * 28 * 6) as u32 };
    Scene { dev, tex, rt, sys, up }
}

/// What the page draws: each primitive with what it is drawn with; and how many draws
fn play(ops: &[Op]) -> (Vec<(u64, Vec<model::Vertex>)>, u64) {
    let s = scene();
    model::with(|m| {
        m.prims = Some(Vec::new());
        m.prim_states = Some(Vec::new());
    });
    let dev = s.dev;
    let draws0 = model::with(|m| m.calls.get("draw").copied().unwrap_or(0) + m.calls.get("draw_indexed").copied().unwrap_or(0));
    let mut shots = Vec::new();
    call(IF_DEVICE, 43, &[dev, 0, 0, 1, 0xff20_4060, 0, 0]);
    call(IF_DEVICE, 41, &[dev]);
    let vertex = |b: &mut Vec<u8>, x: f32, y: f32, c: u32, u: f32, v: f32| {
        for f in [x, y, 0.5f32, 1.0] {
            b.extend_from_slice(&f.to_le_bytes());
        }
        b.extend_from_slice(&c.to_le_bytes());
        b.extend_from_slice(&u.to_le_bytes());
        b.extend_from_slice(&v.to_le_bytes());
    };
    let present = |shots: &mut Vec<Vec<u8>>| {
        call(IF_DEVICE, 42, &[dev]);
        assert_eq!(call(IF_DEVICE, 32, &[dev, s.rt, s.sys]), 0);
        let lr = out();
        assert_eq!(call(IF_SURFACE, 13, &[s.sys, lr, 0, 0x10]), 0);
        let (pitch, bits) = (rd(lr), rd(lr + 4));
        let px: Vec<u8> = (0..W * pitch).map(|i| unsafe { *((bits + i) as *const u8) }).collect();
        call(IF_SURFACE, 14, &[s.sys]);
        shots.push(px);
        call(IF_DEVICE, 41, &[dev]);
    };
    for o in ops {
        match *o {
            Op::Sprite(x, y, size, c, strip, n) => {
                let mut b = Vec::new();
                for q in 0..n {
                    let (x, y) = (x + q as f32 * 3.0, y + q as f32 * 2.0);
                    let corners = [(x, y, 0.0, 0.0), (x + size, y, 1.0, 0.0), (x, y + size, 0.0, 1.0), (x + size, y + size, 1.0, 1.0)];
                    let order: &[usize] = if strip { &[0, 1, 2, 3] } else { &[0, 1, 2, 2, 1, 3] };
                    // (a strip of several quads: joined by two degenerate triangles)
                    if strip && q > 0 {
                        let last = b[b.len() - 28..].to_vec();
                        b.extend_from_slice(&last);
                        let (cx, cy, u, v) = corners[0];
                        vertex(&mut b, cx, cy, c, u, v);
                    }
                    for &k in order {
                        let (cx, cy, u, v) = corners[k];
                        vertex(&mut b, cx, cy, c.rotate_left(q * 8), u, v);
                    }
                }
                put_bytes(s.up, &b);
                let nv = b.len() as u32 / 28;
                let (prim, prims) = if strip { (5, nv - 2) } else { (4, nv / 3) };
                assert_eq!(call(IF_DEVICE, 83, &[dev, prim, prims, s.up, 28]), 0);
            }
            Op::Tex(k) => {
                call(IF_DEVICE, 65, &[dev, 0, s.tex[k]]);
            }
            Op::Blend(a, b) => {
                call(IF_DEVICE, 57, &[dev, 19, a]);
                call(IF_DEVICE, 57, &[dev, 20, b]);
            }
            Op::BlendOn(on) => {
                call(IF_DEVICE, 57, &[dev, 27, on as u32]);
            }
            Op::Mask(m) => {
                call(IF_DEVICE, 57, &[dev, 168, m]);
            }
            Op::Filter(f) => {
                call(IF_DEVICE, 69, &[dev, 0, 5, f]);
                call(IF_DEVICE, 69, &[dev, 0, 6, f]);
            }
            Op::Scissor(on, a, b) => {
                let r = unsafe { alloc(16) as u32 };
                put_bytes(r, &[a, b, a + 24, b + 24].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
                call(IF_DEVICE, 75, &[dev, r]);
                call(IF_DEVICE, 57, &[dev, 174, on as u32]);
            }
            Op::ColorOp(op) => {
                call(IF_DEVICE, 67, &[dev, 0, 1, op]);
            }
            Op::Texel(k, at, v) => {
                let lr = out();
                let rect = unsafe { alloc(16) as u32 };
                let (x, y) = (at % 4, at / 4 % 4);
                put_bytes(rect, &[x, y, x + 1, y + 1].iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
                assert_eq!(call(IF_TEXTURE, 19, &[s.tex[k], 0, lr, rect, 0]), 0);
                put_bytes(rd(lr + 4), &v.to_le_bytes());
                assert_eq!(call(IF_TEXTURE, 20, &[s.tex[k], 0]), 0);
            }
            Op::Fan(x) => {
                let mut b = Vec::new();
                let x = x as f32;
                for (px, py) in [(x, 4.0), (x + 8.0, 4.0), (x + 8.0, 12.0), (x, 12.0)] {
                    vertex(&mut b, px, py, 0x80ff_ffff, 0.5, 0.5);
                }
                put_bytes(s.up, &b);
                assert_eq!(call(IF_DEVICE, 83, &[dev, 6, 2, s.up, 28]), 0);
            }
            Op::Present => present(&mut shots),
        }
    }
    present(&mut shots);
    call(IF_DEVICE, 42, &[dev]);
    gpu::flush();
    gpu::pump();
    let draws = model::with(|m| m.calls.get("draw").copied().unwrap_or(0) + m.calls.get("draw_indexed").copied().unwrap_or(0)) - draws0;
    assert!(shots.len() > 0);
    let (prims, states) = model::with(|m| (m.prims.take().unwrap(), m.prim_states.take().unwrap()));
    assert_eq!(prims.len(), states.len());
    let drawn = prims.into_iter().zip(states).flat_map(|(ps, st)| ps.into_iter().map(move |p| (st, super::draw_tests::rotated(&p)))).collect();
    (drawn, draws)
}

fn check(ops: &[Op]) -> Result<(), TestCaseError> {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    set_merge_mode(1);
    let (want, draws_one) = play(ops);
    let e = model::with(|m| std::mem::take(&mut m.errors));
    set_merge_mode(0);
    JOINED_HERE.with(|j| j.set(0));
    let (got, draws_joined) = play(ops);
    let joined = JOINED_HERE.with(|j| j.get());
    let e2 = model::with(|m| std::mem::take(&mut m.errors));
    set_merge_mode(0xff);
    gpu::set_pump(None);
    prop_assert!(e.is_empty() && e2.is_empty(), "WebGPU errors: {:?} {:?}", &e[..e.len().min(4)], &e2[..e2.len().min(4)]);
    prop_assert_eq!(want.len(), got.len(), "primitives ({} draws joined)", joined);
    for (k, (w, g)) in want.iter().zip(&got).enumerate() {
        prop_assert!(w == g, "primitive {} differs ({} draws joined)\n one by one {:?}\n joined     {:?}", k, joined, w, g);
    }
    // (the page draws fewer, by as many as were joined)
    if draws_one > 0 {
        prop_assert!(draws_joined + joined == draws_one, "{} draws joined, {} draws one by one, {} with joins", joined, draws_one, draws_joined);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 128, ..ProptestConfig::default() })]
    #[test]
    fn d3d9_joined_draws_draw_the_same_pictures(ops in prop::collection::vec(op(), 1..60)) {
        check(&ops)?;
    }
}

/// Sprites of one state in a row are joined (strips too, but the first of a run), and each
/// change between them ends a run: a texture, a blend, a texel rewritten, a draw of another
/// kind, a clip, a write mask, a filter, a stage's operation, a frame's end.
#[test]
fn d3d9_what_ends_a_run_of_joined_draws() {
    let sp = |x: f32| Op::Sprite(x, 4.0, 10.0, 0x80ff_4020, true, 1);
    let run = |between: Op| {
        let ops = [sp(0.0), sp(4.0), sp(8.0), between, sp(12.0), sp(16.0)];
        check(&ops).unwrap();
        JOINED_HERE.with(|j| j.get())
    };
    // (the first of a run stays a strip, the next becomes a list the others join: nothing
    // between, two draws for five)
    assert_eq!(run(Op::Tex(0)), 3, "the same texture set again");
    for between in [Op::Tex(1), Op::Blend(2, 2), Op::Texel(0, 1, 0xff00_ff00), Op::Fan(20), Op::Scissor(true, 2, 2), Op::Mask(7), Op::ColorOp(2), Op::Present] {
        let what = format!("{between:?}");
        assert_eq!(run(between), 1, "{what}");
    }
    // (a filter changes no generation: the sprite after it becomes a list but does not join
    // the one before, the next joins it)
    assert_eq!(run(Op::Filter(2)), 2, "a filter");
    // (an opaque sprite, a strip: not a list, not joined)
    assert_eq!(run(Op::BlendOn(false)), 1, "blending off: strips stay strips");
}

