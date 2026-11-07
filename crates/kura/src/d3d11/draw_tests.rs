//! What a Direct3D 11 draw reads: random Draw, DrawIndexed, DrawInstanced and
//! DrawIndexedInstanced calls through the real entry point (`d3d11::call`), with input
//! layouts over one to three slots (per-vertex and per-instance data, D3D11_APPEND_ALIGNED_ELEMENT
//! offsets), vertex buffer offsets and strides, 16- and 32-bit indices with an index buffer
//! offset, base vertex locations, start instances, strip cuts, zero instances, every
//! topology. The vertex shader (compiled by hlslc) reads every input. The page runs each draw
//! on the model of WebGPU (render/model.rs); the primitives it assembles must be the ones
//! Direct3D 11 assembles, vertex for vertex and attribute for attribute. The same through
//! Direct3D 10's ID3D10Device (d3d10.rs: its methods become the D3D11 ones), on a stack and
//! heap below 4 GB as in wasm32.

use super::*;
use crate::d3d9::trace_tests::{alloc, attach_page, on_low_stack, Rng, CALLS, SERIAL};
use crate::render::model;
use d3d10::*;

fn mem(n: u32) -> u32 {
    unsafe { alloc(4 * n) as u32 }
}
fn put(p: u32, v: &[u32]) {
    for (i, &x) in v.iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = x };
    }
}
fn put_bytes(p: u32, b: &[u8]) {
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len()) };
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

const VS: &str = "
struct VI { float3 p : POSITION; float2 t : TEXCOORD0; float4 c : COLOR; float4 i : TEXCOORD1; };
struct VO { float4 p : SV_Position; float4 c : COLOR; };
VO main(VI v) { VO o; o.p = float4(v.p, 1) + v.i; o.c = v.c + float4(v.t, 0, 0); return o; }
";
const PS: &str = "float4 main(float4 p : SV_Position, float4 c : COLOR) : SV_Target { return c; }";

/// (semantic, index, DXGI format, bytes)
const ELEMS: [(&str, u32, u32, u32); 4] = [
    ("POSITION", 0, 6, 12),
    ("TEXCOORD", 0, 16, 8),
    ("COLOR", 0, 28, 4),
    ("TEXCOORD", 1, 2, 16),
];

/// D3D11's primitives of `topology` over vertex numbers (None: a strip cut).
fn d3d_prims(topology: u32, v: &[Option<i64>]) -> Vec<Vec<i64>> {
    let mut out = Vec::new();
    for run in v.split(|x| x.is_none()) {
        let run: Vec<i64> = run.iter().map(|x| x.unwrap()).collect();
        match topology {
            1 => out.extend(run.iter().map(|&x| vec![x])),
            2 => out.extend(run.chunks_exact(2).map(|c| c.to_vec())),
            3 => out.extend(run.windows(2).map(|c| c.to_vec())),
            4 => out.extend(run.chunks_exact(3).map(|c| c.to_vec())),
            _ => out.extend(run.windows(3).enumerate().map(|(i, w)| {
                if i % 2 == 0 {
                    w.to_vec()
                } else {
                    vec![w[1], w[0], w[2]]
                }
            })),
        }
    }
    out
}

fn run(seed: u64, vs: &[u8], ps: &[u8], ten: bool) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 3);
    attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
    let desc = mem(16);
    put(desc, &[64, 64, 60, 1, 28, 0, 0, 1, 0, 0x20, 1, 1, 1, 0, 0]);
    let (sc_out, dev_out, level, ctx_out) = (mem(2), mem(2), mem(2), mem(2));
    if ten {
        assert_eq!(
            call1(
                CREATE_DEVICE10,
                &[0, 1, 0, 0, 0xa100, 0, desc, sc_out, dev_out]
            ),
            0
        );
    } else {
        assert_eq!(
            call1(
                CREATE_DEVICE,
                &[0, 1, 0, 0, 0, 0, 7, desc, sc_out, dev_out, level, ctx_out]
            ),
            0
        );
    }
    let (sc, dev) = (rd1(sc_out), rd1(dev_out));
    let ctx = if ten { dev } else { rd1(ctx_out) };
    // (the D3D11 interfaces and methods, or D3D10's)
    let (idev, ictx, ibuf, ilayout) = if ten {
        (IF10_DEVICE, IF10_DEVICE, IF10_BUFFER, IF10_LAYOUT)
    } else {
        (IF_DEVICE, IF_CONTEXT, IF_BUFFER, IF_LAYOUT)
    };
    let dm = |m11: u32| {
        if !ten {
            m11
        } else {
            [(3, 71), (5, 73), (9, 76), (11, 78), (12, 79), (15, 82)]
                .iter()
                .find(|x| x.0 == m11)
                .unwrap()
                .1
        }
    };
    let cm = |m11: u32| {
        if !ten {
            m11
        } else {
            [
                (9, 5),
                (11, 7),
                (12, 8),
                (13, 9),
                (17, 11),
                (18, 12),
                (19, 13),
                (20, 14),
                (21, 15),
                (24, 18),
                (33, 24),
                (44, 30),
                (110, 69),
                (111, 70),
            ]
            .iter()
            .find(|x| x.0 == m11)
            .unwrap()
            .1
        }
    };
    let dc = |m11: u32, a: &[u32]| call1(m(idev, dm(m11)), a);
    let cc = |m11: u32, a: &[u32]| call1(m(ictx, cm(m11)), a);
    let o = mem(2);
    // a render target and its view, the viewport
    let d = mem(11);
    put(d, &[64, 64, 1, 1, 28, 1, 0, 0, 0x20, 0, 0]);
    assert_eq!(dc(5, &[dev, d, 0, o]), 0);
    let rt = rd1(o);
    let vd = mem(4);
    put(vd, &[28, 4, 0, 0]);
    assert_eq!(dc(9, &[dev, rt, vd, o]), 0);
    let rtv = rd1(o);
    let arr = mem(4);
    put(arr, &[rtv]);
    cc(33, &[ctx, 1, arr, 0]);
    let vp = mem(6);
    if ten {
        put(vp, &[0, 0, 64, 64, 0, 1f32.to_bits()]);
    } else {
        put(
            vp,
            &[0, 0, 64f32.to_bits(), 64f32.to_bits(), 0, 1f32.to_bits()],
        );
    }
    cc(44, &[ctx, 1, vp]);
    // the shaders
    let code = |c: &[u8]| {
        let p = unsafe { alloc(c.len() as u32) as u32 };
        put_bytes(p, c);
        p
    };
    let (vsc, psc) = (code(vs), code(ps));
    let linkage: &[u32] = if ten { &[] } else { &[0] };
    assert_eq!(
        dc(
            12,
            &[&[dev, vsc, vs.len() as u32][..], linkage, &[o]].concat()
        ),
        0
    );
    let vsh = rd1(o);
    assert_eq!(
        dc(
            15,
            &[&[dev, psc, ps.len() as u32][..], linkage, &[o]].concat()
        ),
        0
    );
    let psh = rd1(o);
    cc(11, &[ctx, vsh, 0, 0]);
    cc(9, &[ctx, psh, 0, 0]);
    // three vertex buffers of random bytes (finite floats), one index buffer per format
    const VB: u32 = 4096;
    let mut vbs = Vec::new();
    for _ in 0..3 {
        let bytes: Vec<u8> = (0..VB / 4)
            .flat_map(|_| (((r.next() as u32) & 0x3fff_ffff) | 0x3000_0000).to_le_bytes())
            .collect();
        let init = mem(3);
        let p = unsafe { alloc(VB) as u32 };
        put_bytes(p, &bytes);
        put(init, &[p, 0, 0]);
        // (D3D10's description has 5 dwords: what follows is not a StructureByteStride)
        let bd = mem(6);
        put(bd, &[VB, 0, 1, 0, 0, if ten { 0xdead } else { 0 }]);
        assert_eq!(dc(3, &[dev, bd, init, o]), 0);
        vbs.push((rd1(o), bytes));
    }
    let names: Vec<u32> = ELEMS
        .iter()
        .map(|e| {
            let p = unsafe { alloc(e.0.len() as u32 + 1) as u32 };
            put_bytes(p, e.0.as_bytes());
            put_bytes(p + e.0.len() as u32, &[0]);
            p
        })
        .collect();
    let mut drawn = 0;
    for step in 0..100 {
        // the layout: each element's slot (TEXCOORD1 per instance in slot 2 half the time),
        // offsets packed (APPEND_ALIGNED) or spread
        let inst = r.chance(50);
        let mut slots = [0u32; 4];
        for (k, s) in slots.iter_mut().enumerate() {
            *s = if k == 3 && inst { 2 } else { r.below(2) as u32 };
        }
        let mut next = [0u32; 3];
        let mut elems = Vec::new(); // (slot, offset, bytes)
        let ed = mem(7 * 4);
        for (k, e) in ELEMS.iter().enumerate() {
            let s = slots[k];
            let append = r.chance(50);
            let off = if append {
                next[s as usize]
            } else {
                next[s as usize] + 4 * r.below(3) as u32
            };
            next[s as usize] = off + e.3;
            elems.push((s, off, e.3));
            put(
                ed + 28 * k as u32,
                &[
                    names[k],
                    e.1,
                    e.2,
                    s,
                    if append { 0xffff_ffff } else { off },
                    (k == 3 && inst) as u32,
                    (k == 3 && inst) as u32,
                ],
            );
        }
        assert_eq!(
            dc(11, &[dev, ed, 4, vsc, vs.len() as u32, o]),
            0,
            "seed {seed} step {step}: CreateInputLayout"
        );
        let layout = rd1(o);
        cc(17, &[ctx, layout]);
        let mut stride = [0u32; 3];
        let mut offset = [0u32; 3];
        for s in 0..3 {
            stride[s] = next[s] + 4 * r.below(3) as u32;
            offset[s] = 4 * r.below(8) as u32;
        }
        let (bufs, strides, offs) = (mem(3), mem(3), mem(3));
        put(bufs, &[vbs[0].0, vbs[1].0, vbs[2].0]);
        put(strides, &stride);
        put(offs, &offset);
        cc(18, &[ctx, 0, 3, bufs, strides, offs]);
        let topology = 1 + r.below(5) as u32;
        cc(24, &[ctx, topology]);
        let count = r.below(10) as u32;
        let indexed = r.chance(50);
        let instanced = r.chance(50);
        let instances = if instanced { r.below(4) as u32 } else { 1 };
        let start_instance = if instanced { r.below(4) as u32 } else { 0 };
        let start = r.below(6) as u32;
        let base = r.below(10) as i32 - 3;
        let i32 = r.chance(50);
        let cut = if i32 { u32::MAX } else { 0xffff };
        let ix: Vec<u32> = (0..start + count)
            .map(|_| {
                if r.chance(10) {
                    cut
                } else {
                    r.below(30) as u32 + 3
                }
            })
            .collect();
        let ib_off = 4 * r.below(4) as u32;
        // the vertex numbers D3D11 reads (None: a cut, in strips)
        let verts: Vec<Option<i64>> = if indexed {
            ix[start as usize..]
                .iter()
                .map(|&i| {
                    if i == cut {
                        if topology == 3 || topology == 5 {
                            None
                        } else {
                            Some(-1)
                        }
                    } else {
                        Some(base as i64 + i as i64)
                    }
                })
                .collect()
        } else {
            (0..count).map(|k| Some(start as i64 + k as i64)).collect()
        };
        if verts.iter().any(|v| v.is_some_and(|v| v < 0)) {
            call1(m(ilayout, 2), &[layout]);
            continue;
        }
        if indexed {
            let bytes: Vec<u8> = ix
                .iter()
                .flat_map(|&v| {
                    if i32 {
                        v.to_le_bytes().to_vec()
                    } else {
                        (v as u16).to_le_bytes().to_vec()
                    }
                })
                .collect();
            let mut all = vec![0u8; ib_off as usize];
            all.extend_from_slice(&bytes);
            let init = mem(3);
            let p = unsafe { alloc(all.len() as u32 + 4) as u32 };
            put_bytes(p, &all);
            put(init, &[p, 0, 0]);
            let bd = mem(6);
            put(
                bd,
                &[(all.len() as u32).next_multiple_of(4).max(4), 0, 2, 0, 0, 0],
            );
            assert_eq!(dc(3, &[dev, bd, init, o]), 0);
            let ib = rd1(o);
            cc(19, &[ctx, ib, if i32 { 42 } else { 57 }, ib_off]);
            call1(m(ibuf, 2), &[ib]);
        }
        match (indexed, instanced) {
            (false, false) => cc(13, &[ctx, count, start]),
            (true, false) => cc(12, &[ctx, count, start, base as u32]),
            (false, true) => cc(21, &[ctx, count, instances, start, start_instance]),
            (true, true) => cc(
                20,
                &[ctx, count, instances, start, base as u32, start_instance],
            ),
        };
        cc(111, &[ctx]);
        gpu::pump();
        let vertex = |v: i64, instance: i64| -> Vec<Vec<u8>> {
            let mut a: Vec<Vec<u8>> = elems
                .iter()
                .enumerate()
                .map(|(k, &(s, off, n))| {
                    let v = if k == 3 && inst { instance } else { v };
                    let at = (offset[s as usize] as i64
                        + v * stride[s as usize] as i64
                        + off as i64) as usize;
                    vbs[s as usize]
                        .1
                        .get(at..at + n as usize)
                        .map(|b| b.to_vec())
                        .unwrap_or_default()
                })
                .collect();
            a.sort();
            a
        };
        let mut want = Vec::new();
        for i in 0..instances as i64 {
            for p in d3d_prims(topology, &verts) {
                want.push(
                    p.iter()
                        .map(|&v| vertex(v, start_instance as i64 + i))
                        .collect::<Vec<_>>(),
                );
            }
        }
        let got = model::with(|m| m.prims.as_mut().unwrap().drain(..).collect::<Vec<_>>());
        let got: Vec<Vec<Vec<Vec<u8>>>> = got
            .iter()
            .flatten()
            .map(|p| {
                p.iter()
                    .map(|v| {
                        let mut a: Vec<Vec<u8>> = v.iter().map(|x| x.1.clone()).collect();
                        a.sort();
                        a
                    })
                    .collect()
            })
            .collect();
        let api = if ten { "D3D10" } else { "D3D11" };
        let what = format!("{api} seed {seed} step {step}: topology {topology} count {count} indexed {indexed} (start {start} base {base} 32-bit {i32} offset {ib_off} {ix:?}) instances {instances} from {start_instance} per-instance {inst}");
        assert_eq!(got.len(), want.len(), "{what}: primitives");
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(
                g == w,
                "{what}: primitive {k} differs\n got  {g:?}\n want {w:?}"
            );
        }
        call1(m(ilayout, 2), &[layout]);
        drawn += 1;
    }
    assert!(drawn > 50, "seed {seed}: {drawn} draws checked");
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(
        errors.is_empty(),
        "seed {seed}: WebGPU errors {:?}",
        &errors[..errors.len().min(10)]
    );
    model::with(|m| m.prims = None);
    cc(110, &[ctx]);
    let _ = (sc, level);
}

#[test]
fn d3d11_draws_read_what_direct3d_reads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vs = hlslc::compile(VS, &[], &mut |_, _| None, "main", "vs_4_0", 0).expect("vs");
    let ps = hlslc::compile(PS, &[], &mut |_, _| None, "main", "ps_4_0", 0).expect("ps");
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
    for seed in 0..6 {
        run(seed, &vs, &ps, false);
    }
}

#[test]
fn d3d10_draws_read_what_direct3d_reads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    on_low_stack(|| {
        let vs = hlslc::compile(VS, &[], &mut |_, _| None, "main", "vs_4_0", 0).expect("vs");
        let ps = hlslc::compile(PS, &[], &mut |_, _| None, "main", "ps_4_0", 0).expect("ps");
        let vt = mem(256);
        put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
        call1(INIT, &[vt, 256]);
        for seed in 0..4 {
            run(seed, &vs, &ps, true);
        }
    });
}

/// D3D10's resource misc flags (SHARED_KEYEDMUTEX 0x10 and GDI_COMPATIBLE 0x20 are other bits in
/// D3D11) and its integer viewports, all of them, there and back.
#[test]
fn d3d10_descriptions_and_viewports() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    on_low_stack(|| {
        let vt = mem(256);
        put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
        call1(INIT, &[vt, 256]);
        attach_page();
        let desc = mem(16);
        put(desc, &[64, 64, 60, 1, 28, 0, 0, 1, 0, 0x20, 1, 1, 1, 0, 0]);
        let (sc_out, dev_out) = (mem(2), mem(2));
        assert_eq!(
            call1(
                CREATE_DEVICE10,
                &[0, 1, 0, 0, 0xa100, 0, desc, sc_out, dev_out]
            ),
            0
        );
        let dev = rd1(dev_out);
        let o = mem(2);
        // a GDI compatible render target (B8G8R8A8), a keyed mutex texture, a plain one
        for (fmt, misc, ok) in [
            (87, 0x20, true),
            (28, 0x20, false),
            (28, 0x10, true),
            (28, 0x12, false),
            (28, 0, true),
            (28, 0x4, false),
        ] {
            let d = mem(11);
            put(d, &[16, 16, 1, 1, fmt, 1, 0, 0, 0x28, 0, misc]);
            let r = call1(m(IF10_DEVICE, 73), &[dev, d, 0, o]);
            assert_eq!(r == 0, ok, "Texture2D format {fmt} misc {misc:#x}: {r:#x}");
            if ok {
                let t = rd1(o);
                let g = mem(11);
                assert_eq!(call1(m(IF10_TEX2D, 12), &[t, g]), 0);
                assert_eq!(
                    (0..11).map(|i| rd1(g + 4 * i)).collect::<Vec<_>>(),
                    vec![16, 16, 1, 1, fmt, 1, 0, 0, 0x28, 0, misc]
                );
                call1(m(IF10_TEX2D, 2), &[t]);
            }
        }
        let bd = mem(6);
        put(bd, &[64, 0, 1, 0, 0, 0xdead]);
        assert_eq!(call1(m(IF10_DEVICE, 71), &[dev, bd, 0, o]), 0);
        let b = rd1(o);
        let g = mem(6);
        put(g, &[7; 6]);
        assert_eq!(call1(m(IF10_BUFFER, 12), &[b, g]), 0);
        assert_eq!(
            (0..6).map(|i| rd1(g + 4 * i)).collect::<Vec<_>>(),
            vec![64, 0, 1, 0, 0, 7]
        );
        // three viewports: RSGetViewports gives each back
        let vp = mem(18);
        let want: Vec<u32> = (0..3)
            .flat_map(|k| [k, 2 * k, 10 + k, 20 + k, 0, 1f32.to_bits()])
            .collect();
        put(vp, &want);
        call1(m(IF10_DEVICE, 30), &[dev, 3, vp]);
        let (n, back) = (mem(1), mem(24));
        put(n, &[4]);
        call1(m(IF10_DEVICE, 61), &[dev, n, back]);
        assert_eq!(rd1(n), 3);
        assert_eq!((0..18).map(|i| rd1(back + 4 * i)).collect::<Vec<_>>(), want);
        assert_eq!(
            (18..24).map(|i| rd1(back + 4 * i)).collect::<Vec<_>>(),
            vec![0; 6]
        );
        gpu::set_pump(None);
    });
}

/// State descriptions as Windows keeps them: one object for descriptions that differ only in
/// what the state ignores (winref d3d11bind), GetDesc's normalized description.
#[test]
fn state_descriptions_normalized() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
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
    let dev = rd1(dev_out);
    let make = |m11: u32, d: &[u32]| {
        let p = mem(d.len() as u32);
        put(p, d);
        let o = mem(2);
        assert_eq!(call1(m(IF_DEVICE, m11), &[dev, p, o]), 0);
        rd1(o)
    };
    let get = |iface: u32, o: u32, n: u32| {
        let p = mem(n);
        call1(m(iface, 7), &[o, p]);
        (0..n).map(|i| rd1(p + 4 * i)).collect::<Vec<_>>()
    };
    // blend: a disabled target's factors; the targets after the first without IndependentBlendEnable
    let mut b = vec![0u32; 66];
    for t in 0..8 {
        b[2 + t * 8..2 + t * 8 + 8].copy_from_slice(&[0, 5, 6, 1, 2, 1, 1, 0xf]);
    }
    let b1 = make(20, &b);
    let mut b2 = b.clone();
    b2[3] = 2;
    b2[10..18].copy_from_slice(&[1, 4, 4, 3, 4, 4, 3, 1]);
    let b2 = make(20, &b2);
    assert_eq!(
        b1, b2,
        "blend: differs only in a disabled factor and an unused target"
    );
    let mut want = vec![0u32; 66];
    for t in 0..8 {
        want[2 + t * 8..2 + t * 8 + 8].copy_from_slice(&[0, 2, 1, 1, 2, 1, 1, 0xf]);
    }
    assert_eq!(get(IF_BLEND, b1, 66), want);
    b[1] = 1;
    b[10] = 1;
    let b3 = make(20, &b);
    assert_ne!(b3, b1);
    let got = get(IF_BLEND, b3, 66);
    assert_eq!(
        &got[10..18],
        &[1, 5, 6, 1, 2, 1, 1, 0xf],
        "an independent target that blends keeps its factors"
    );
    assert_eq!(&got[2..10], &[0, 2, 1, 1, 2, 1, 1, 0xf]);
    // depth stencil: no depth test, no stencil; the padding after the stencil masks
    let d1 = make(21, &[0, 0, 4, 0, 0xdead_0f0f, 2, 3, 4, 5, 6, 7, 8, 1]);
    let d2 = make(21, &[0, 1, 7, 0, 0x0000_1234, 1, 1, 1, 8, 1, 1, 1, 8]);
    assert_eq!(d1, d2);
    assert_eq!(
        get(IF_DEPTH, d1, 13),
        vec![0, 1, 2, 0, 0xffff, 1, 1, 1, 8, 1, 1, 1, 8]
    );
    let d3 = make(21, &[1, 0, 4, 1, 0xbeef_0f0f, 2, 3, 4, 5, 6, 7, 8, 1]);
    assert_eq!(
        get(IF_DEPTH, d3, 13),
        vec![1, 0, 4, 1, 0x0f0f, 2, 3, 4, 5, 6, 7, 8, 1]
    );
    gpu::set_pump(None);
}

/// ANGLE's streaming: each draw's vertices go into a dynamic buffer through a Map
/// NO_OVERWRITE after the last draw's (in place, only the new bytes uploaded), a Map DISCARD
/// when they don't fit (the buffer anew). Several draws per batch: each must read the
/// vertices written for it, not ones written after (the page writes the new bytes while the
/// pass with the earlier draws is still open), and not lose the ones before.
fn stream_run(seed: u64, vs: &[u8], ps: &[u8]) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 5);
    attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
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
    let (dev, ctx) = (rd1(dev_out), rd1(ctx_out));
    let dc = |m11: u32, a: &[u32]| call1(m(IF_DEVICE, m11), a);
    let cc = |m11: u32, a: &[u32]| call1(m(IF_CONTEXT, m11), a);
    let o = mem(2);
    let d = mem(11);
    put(d, &[64, 64, 1, 1, 28, 1, 0, 0, 0x20, 0, 0]);
    assert_eq!(dc(5, &[dev, d, 0, o]), 0);
    let rt = rd1(o);
    let vd = mem(4);
    put(vd, &[28, 4, 0, 0]);
    assert_eq!(dc(9, &[dev, rt, vd, o]), 0);
    let arr = mem(4);
    put(arr, &[rd1(o)]);
    cc(33, &[ctx, 1, arr, 0]);
    let vp = mem(6);
    put(
        vp,
        &[0, 0, 64f32.to_bits(), 64f32.to_bits(), 0, 1f32.to_bits()],
    );
    cc(44, &[ctx, 1, vp]);
    let code = |c: &[u8]| {
        let p = unsafe { alloc(c.len() as u32) as u32 };
        put_bytes(p, c);
        p
    };
    let (vsc, psc) = (code(vs), code(ps));
    assert_eq!(dc(12, &[dev, vsc, vs.len() as u32, 0, o]), 0);
    cc(11, &[ctx, rd1(o), 0, 0]);
    assert_eq!(dc(15, &[dev, psc, ps.len() as u32, 0, o]), 0);
    cc(9, &[ctx, rd1(o), 0, 0]);
    // every element in slot 0, packed: 40 bytes a vertex
    const STRIDE: u32 = 40;
    let ed = mem(7 * 4);
    let mut at = 0;
    for (k, e) in ELEMS.iter().enumerate() {
        let p = unsafe { alloc(e.0.len() as u32 + 1) as u32 };
        put_bytes(p, e.0.as_bytes());
        put_bytes(p + e.0.len() as u32, &[0]);
        put(ed + 28 * k as u32, &[p, e.1, e.2, 0, at, 0, 0]);
        at += e.3;
    }
    assert_eq!(dc(11, &[dev, ed, 4, vsc, vs.len() as u32, o]), 0);
    cc(17, &[ctx, rd1(o)]);
    cc(24, &[ctx, 1]);
    // the dynamic vertex buffer (usage dynamic, vertex binding, CPU write)
    const SIZE: u32 = 1200;
    let bd = mem(6);
    put(bd, &[SIZE, 2, 1, 0x10000, 0, 0]);
    assert_eq!(dc(3, &[dev, bd, 0, o]), 0);
    let vb = rd1(o);
    let (bufs, strides, offs, mapped) = (mem(1), mem(1), mem(1), mem(4));
    put(bufs, &[vb]);
    put(strides, &[STRIDE]);
    let mut cursor = SIZE;
    let mut checked = 0;
    for batch in 0..60 {
        let mut want: Vec<Vec<Vec<Vec<u8>>>> = Vec::new();
        for _ in 0..1 + r.below(6) {
            let n = 1 + r.below(8) as u32;
            // (DISCARD when the vertices don't fit, now and then anyway; the first Map too)
            let discard = cursor + n * STRIDE > SIZE || r.chance(10);
            if discard {
                cursor = 0;
            }
            assert_eq!(
                cc(14, &[ctx, vb, 0, if discard { 4 } else { 5 }, 0, mapped]),
                0
            );
            let p = rd1(mapped);
            // (some vertices the same bytes as the buffer held there: no change to upload)
            let same = r.chance(20);
            let bytes: Vec<u8> = (0..n * STRIDE / 4)
                .flat_map(|_| (((r.next() as u32) & 0x3fff_ffff) | 0x3000_0000).to_le_bytes())
                .collect();
            if !same {
                put_bytes(p + cursor, &bytes);
            }
            let bytes = unsafe {
                std::slice::from_raw_parts((p + cursor) as *const u8, (n * STRIDE) as usize)
            }
            .to_vec();
            cc(15, &[ctx, vb, 0]);
            put(offs, &[cursor]);
            cc(18, &[ctx, 0, 1, bufs, strides, offs]);
            cc(13, &[ctx, n, 0]);
            for v in bytes.chunks(STRIDE as usize) {
                let mut a: Vec<Vec<u8>> = ELEMS
                    .iter()
                    .scan(0usize, |o, e| {
                        let s = v[*o..*o + e.3 as usize].to_vec();
                        *o += e.3 as usize;
                        Some(s)
                    })
                    .collect();
                a.sort();
                want.push(vec![a]);
            }
            cursor += n * STRIDE;
        }
        cc(111, &[ctx]);
        gpu::pump();
        let got = model::with(|m| m.prims.as_mut().unwrap().drain(..).collect::<Vec<_>>());
        let got: Vec<Vec<Vec<Vec<u8>>>> = got
            .iter()
            .flatten()
            .map(|p| {
                p.iter()
                    .map(|v| {
                        let mut a: Vec<Vec<u8>> = v.iter().map(|x| x.1.clone()).collect();
                        a.sort();
                        a
                    })
                    .collect()
            })
            .collect();
        assert_eq!(got.len(), want.len(), "seed {seed} batch {batch}: points");
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(
                g == w,
                "seed {seed} batch {batch}: point {k} differs\n got  {g:?}\n want {w:?}"
            );
        }
        checked += want.len();
    }
    assert!(checked > 200, "seed {seed}: {checked} points");
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(
        errors.is_empty(),
        "seed {seed}: WebGPU errors {:?}",
        &errors[..errors.len().min(10)]
    );
    model::with(|m| m.prims = None);
    cc(110, &[ctx]);
}

#[test]
fn d3d11_streamed_vertices_in_one_batch() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vs = hlslc::compile(VS, &[], &mut |_, _| None, "main", "vs_4_0", 0).expect("vs");
    let ps = hlslc::compile(PS, &[], &mut |_, _| None, "main", "ps_4_0", 0).expect("ps");
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
    for seed in 0..8 {
        stream_run(seed, &vs, &ps);
    }
}
