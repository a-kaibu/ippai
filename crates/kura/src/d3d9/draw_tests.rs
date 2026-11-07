//! What a Direct3D 9 draw reads: random DrawPrimitive, DrawIndexedPrimitive, DrawPrimitiveUP
//! and DrawIndexedPrimitiveUP calls through the real entry point (`vnhost_d3d9_call`), with
//! vertex declarations and FVFs of one or two streams, stream offsets and strides, 16- and
//! 32-bit indices, base vertex indices (negative too), MinVertexIndex / NumVertices that do
//! not cover the indices used (games pass them wrong; Direct3D's hardware path reads
//! BaseVertexIndex + index all the same), every primitive type (fans: WebGPU has none). The
//! page runs each on the model of WebGPU (render/model.rs), which assembles the primitives
//! as WebGPU does (index buffer, baseVertex, topology, strip restart); they must be the
//! primitives Direct3D 9 assembles, vertex for vertex, attribute for attribute.

use super::trace_tests::{alloc, attach_page, Rng, CALLS, SERIAL};
use super::*;
use crate::render::model;

pub(super) fn call(iface: u32, m: u32, args: &[u32]) -> u32 {
    // (padded like the guest stack: the Direct3D 8 forwards read 10 words)
    let mut a = args.to_vec();
    a.resize(a.len().max(16), 0);
    unsafe { vnhost_d3d9_call(iface << 8 | m, a.as_ptr(), &CALLS) }
}
pub(super) fn out() -> u32 {
    unsafe { alloc(16) as u32 }
}
pub(super) fn rd(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}
pub(super) fn put_bytes(p: u32, b: &[u8]) {
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), p as *mut u8, b.len()) };
}

/// A layout: elements (stream, offset, type, usage, usage index) and the stream strides it
/// needs at least.
pub(super) struct Layout {
    pub(super) elems: Vec<(u32, u32, u32, u32, u32)>,
    pub(super) fvf: u32,
}

pub(super) fn type_bytes(t: u32) -> u32 {
    match t {
        0 => 4,
        1 => 8,
        2 => 12,
        3 => 16,
        4 | 5 | 6 | 8 => 4,
        _ => 8,
    }
}

pub(super) fn layouts() -> Vec<Layout> {
    vec![
        // one stream: position, colour, a texture coordinate
        Layout {
            elems: vec![(0, 0, 2, 0, 0), (0, 12, 4, 10, 0), (0, 16, 1, 5, 0)],
            fvf: 0,
        },
        // two streams: position in 0; colour and coordinates in 1
        Layout {
            elems: vec![(0, 0, 2, 0, 0), (1, 0, 4, 10, 0), (1, 4, 1, 5, 0)],
            fvf: 0,
        },
        // position and a normal, coordinates in the other order
        Layout {
            elems: vec![(0, 16, 2, 0, 0), (0, 0, 2, 3, 0), (0, 28, 1, 5, 0)],
            fvf: 0,
        },
        // a specular colour and two coordinate sets (usage indices 1)
        Layout {
            elems: vec![
                (0, 0, 2, 0, 0),
                (0, 12, 4, 10, 1),
                (0, 16, 1, 5, 1),
                (0, 24, 1, 5, 0),
            ],
            fvf: 0,
        },
        // FVF XYZ | DIFFUSE | TEX1
        Layout {
            elems: vec![(0, 0, 2, 0, 0), (0, 12, 4, 10, 0), (0, 16, 1, 5, 0)],
            fvf: 0x142,
        },
    ]
}

/// D3D9's primitives of `prim` over vertex numbers `v` (fans: (v0, v[i+1], v[i+2])).
pub(super) fn d3d_prims(prim: u32, v: &[i64], n: u32) -> Vec<Vec<i64>> {
    let n = n as usize;
    match prim {
        1 => v.iter().take(n).map(|&x| vec![x]).collect(),
        2 => v.chunks_exact(2).take(n).map(|c| c.to_vec()).collect(),
        3 => v.windows(2).take(n).map(|c| c.to_vec()).collect(),
        4 => v.chunks_exact(3).take(n).map(|c| c.to_vec()).collect(),
        5 => (0..n)
            .map(|i| {
                if i % 2 == 0 {
                    vec![v[i], v[i + 1], v[i + 2]]
                } else {
                    vec![v[i + 1], v[i], v[i + 2]]
                }
            })
            .collect(),
        _ => (0..n).map(|i| vec![v[0], v[i + 1], v[i + 2]]).collect(),
    }
}

/// (a triangle's vertices from its smallest: the same triangle, the same winding)
pub(super) fn rotated<T: Ord + Clone>(p: &[T]) -> Vec<T> {
    if p.len() != 3 {
        return p.to_vec();
    }
    let k = (0..3).min_by_key(|&i| &p[i]).unwrap();
    (0..3).map(|i| p[(k + i) % 3].clone()).collect()
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 5);
    attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0]
        .into_iter()
        .enumerate()
    {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o);
    // two vertex buffers of random bytes (the position floats kept finite), two index buffers
    const VB: u32 = 8192;
    let mut vbs = Vec::new();
    for _ in 0..2 {
        let o = out();
        assert_eq!(call(IF_DEVICE, 26, &[dev, VB, 0, 0, 1, o, 0]), 0);
        let vb = rd(o);
        let p = out();
        assert_eq!(call(IF_VB, 11, &[vb, 0, 0, p, 0]), 0);
        let bytes: Vec<u8> = (0..VB / 4)
            .flat_map(|_| (((r.next() as u32) & 0x3fff_ffff) | 0x3000_0000).to_le_bytes())
            .collect();
        put_bytes(rd(p), &bytes);
        assert_eq!(call(IF_VB, 12, &[vb]), 0);
        vbs.push((vb, bytes));
    }
    let mut ibs = Vec::new();
    for (fmt, isz) in [(101u32, 2u32), (102, 4)] {
        let o = out();
        assert_eq!(call(IF_DEVICE, 27, &[dev, 1024, 0, fmt, 1, o, 0]), 0);
        ibs.push((rd(o), isz));
    }
    let decls: Vec<u32> = layouts()
        .iter()
        .map(|l| {
            if l.fvf != 0 {
                return 0;
            }
            let p = unsafe { alloc(8 * (l.elems.len() as u32 + 1)) as u32 };
            for (i, &(s, off, t, u, ui)) in l.elems.iter().enumerate() {
                let w = [(s | off << 16), (t | 0 << 8 | u << 16 | ui << 24)];
                put_bytes(
                    p + 8 * i as u32,
                    &[w[0].to_le_bytes(), w[1].to_le_bytes()].concat(),
                );
            }
            put_bytes(p + 8 * l.elems.len() as u32, &[0xff, 0, 0, 0, 17, 0, 0, 0]);
            let o = out();
            assert_eq!(call(IF_DEVICE, 86, &[dev, p, o]), 0);
            rd(o)
        })
        .collect();
    // no culling, no lighting, no depth: every primitive is drawn
    for (rs, v) in [(22, 1), (137, 0), (7, 0)] {
        call(IF_DEVICE, 57, &[dev, rs, v]);
    }
    call(IF_DEVICE, 41, &[dev]);
    let up = unsafe { alloc(VB) as u32 };
    let upi = unsafe { alloc(1024) as u32 };
    let lays = layouts();
    let mut drawn = 0;
    for step in 0..120 {
        let li = r.below(lays.len());
        let l = &lays[li];
        if l.fvf != 0 {
            call(IF_DEVICE, 89, &[dev, l.fvf]);
        } else {
            call(IF_DEVICE, 87, &[dev, decls[li]]);
        }
        let streams: Vec<u32> = {
            let mut s: Vec<u32> = l.elems.iter().map(|e| e.0).collect();
            s.dedup();
            s
        };
        // (strides: what the layout needs, and some padding)
        let mut stride = [0u32; 2];
        let mut offset = [0u32; 2];
        for &s in &streams {
            let need = l
                .elems
                .iter()
                .filter(|e| e.0 == s)
                .map(|e| e.1 + type_bytes(e.2))
                .max()
                .unwrap();
            stride[s as usize] = need + 4 * r.below(3) as u32;
            offset[s as usize] = 4 * r.below(16) as u32;
        }
        let prim = 1 + r.below(6) as u32;
        let n = 1 + r.below(6) as u32;
        let count = [0, n, 2 * n, n + 1, 3 * n, n + 2, n + 2][prim as usize];
        let kind = r.below(4);
        let isz = if r.chance(50) { 2 } else { 4 };
        // the index values, and where the vertex numbers start
        let mut base: i64 = r.below(12) as i64 - 4;
        let mut ix: Vec<u32> = (0..count).map(|_| r.below(24) as u32 + 4).collect();
        if kind == 1 && isz == 2 && prim == 5 && r.chance(30) {
            // 0xffff a vertex (with a negative base), not a strip restart
            base = -0xffff + 6;
            ix = (0..count)
                .map(|_| {
                    if r.chance(40) {
                        0xffff
                    } else {
                        0xffff - r.below(6) as u32
                    }
                })
                .collect();
        }
        let start = r.below(8) as u32;
        let (min, num) = {
            let lo = ix.iter().min().unwrap();
            let hi = ix.iter().max().unwrap();
            if r.chance(60) {
                (*lo, hi - lo + 1)
            } else {
                (r.below(6) as u32, 1 + r.below(4) as u32)
            }
        };
        if kind == 3 {
            base = 0;
        }
        let first_vertex = r.below(10) as u32;
        // the vertex numbers D3D9 reads, by invocation
        let verts: Vec<i64> = match kind {
            0 | 2 => (0..count as i64)
                .map(|k| first_vertex as i64 * (kind == 0) as i64 + k)
                .collect(),
            _ => ix.iter().map(|&i| base + i as i64).collect(),
        };
        if verts.iter().any(|&v| v < 0) {
            continue;
        }
        // geometry instancing: indexed draws of the two-stream layout, stream 1 per instance
        let (instances, divider) = if kind == 1 && streams.len() == 2 && r.chance(50) {
            (1 + r.below(4) as u32, 1 + r.below(3) as u32)
        } else {
            (1, 0)
        };
        if divider != 0 {
            call(IF_DEVICE, 102, &[dev, 0, 0x4000_0000 | instances]);
            call(IF_DEVICE, 102, &[dev, 1, 0x8000_0000 | divider]);
        }
        let res = match kind {
            0 | 1 => {
                for &s in &streams {
                    call(
                        IF_DEVICE,
                        100,
                        &[
                            dev,
                            s,
                            vbs[s as usize].0,
                            offset[s as usize],
                            stride[s as usize],
                        ],
                    );
                }
                if kind == 0 {
                    call(IF_DEVICE, 81, &[dev, prim, first_vertex, n])
                } else {
                    let (ib, _) = ibs[(isz == 4) as usize];
                    let p = out();
                    assert_eq!(call(IF_IB, 11, &[ib, 0, 0, p, 0]), 0);
                    let bytes: Vec<u8> = ix
                        .iter()
                        .flat_map(|&v| {
                            if isz == 4 {
                                v.to_le_bytes().to_vec()
                            } else {
                                (v as u16).to_le_bytes().to_vec()
                            }
                        })
                        .collect();
                    put_bytes(rd(p) + start * isz, &bytes);
                    call(IF_IB, 12, &[ib]);
                    call(IF_DEVICE, 104, &[dev, ib]);
                    call(IF_DEVICE, 82, &[dev, prim, base as u32, min, num, start, n])
                }
            }
            _ => {
                // UP: one stream only
                if streams.len() > 1 {
                    continue;
                }
                offset[0] = 0;
                put_bytes(up, &vbs[0].1);
                if kind == 2 {
                    call(IF_DEVICE, 83, &[dev, prim, n, up, stride[0]])
                } else {
                    let bytes: Vec<u8> = ix
                        .iter()
                        .flat_map(|&v| {
                            if isz == 4 {
                                v.to_le_bytes().to_vec()
                            } else {
                                (v as u16).to_le_bytes().to_vec()
                            }
                        })
                        .collect();
                    put_bytes(upi, &bytes);
                    call(
                        IF_DEVICE,
                        84,
                        &[
                            dev,
                            prim,
                            min,
                            num,
                            n,
                            upi,
                            if isz == 4 { 102 } else { 101 },
                            up,
                            stride[0],
                        ],
                    )
                }
            }
        };
        assert_eq!(res, 0, "seed {seed} step {step}");
        if divider != 0 {
            call(IF_DEVICE, 102, &[dev, 0, 1]);
            call(IF_DEVICE, 102, &[dev, 1, 1]);
        }
        gpu::flush();
        gpu::pump();
        // Direct3D 9's primitives: each vertex the elements' bytes, by element
        let vertex = |v: i64, inst: u32| -> Vec<Vec<u8>> {
            let mut a: Vec<Vec<u8>> = l
                .elems
                .iter()
                .map(|&(s, off, t, _, _)| {
                    let v = if divider != 0 && s == 1 {
                        (inst / divider) as i64
                    } else {
                        v
                    };
                    let at = (offset[s as usize] as i64
                        + v * stride[s as usize] as i64
                        + off as i64) as usize;
                    vbs[s as usize]
                        .1
                        .get(at..at + type_bytes(t) as usize)
                        .map(|b| b.to_vec())
                        .unwrap_or_default()
                })
                .collect();
            a.sort();
            a
        };
        let want: Vec<Vec<Vec<Vec<u8>>>> = (0..instances)
            .flat_map(|i| d3d_prims(prim, &verts, n).into_iter().map(move |p| (i, p)))
            .map(|(i, p)| rotated(&p.iter().map(|&v| vertex(v, i)).collect::<Vec<_>>()))
            .collect();
        let got = model::with(|m| m.prims.as_mut().unwrap().drain(..).collect::<Vec<_>>());
        assert_eq!(
            got.len(),
            1,
            "seed {seed} step {step}: {} draws on the page for one",
            got.len()
        );
        let got: Vec<Vec<Vec<Vec<u8>>>> = got[0]
            .iter()
            .map(|p| {
                rotated(
                    &p.iter()
                        .map(|v| {
                            let mut a: Vec<Vec<u8>> = v.iter().map(|x| x.1.clone()).collect();
                            a.sort();
                            a
                        })
                        .collect::<Vec<_>>(),
                )
            })
            .collect();
        let what = [
            "DrawPrimitive",
            "DrawIndexedPrimitive",
            "DrawPrimitiveUP",
            "DrawIndexedPrimitiveUP",
        ][kind];
        assert_eq!(got.len(), want.len(), "seed {seed} step {step}: {what} prim {prim} x{n} layout {li} base {base} min {min} num {num} isz {isz}: primitives");
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(g == w, "seed {seed} step {step}: {what} prim {prim} x{n} layout {li} base {base} min {min} num {num} start {start} isz {isz} index {ix:?}: primitive {k} differs\n got  {g:?}\n want {w:?}");
        }
        drawn += 1;
    }
    assert!(drawn > 60, "seed {seed}: {drawn} draws checked");
    call(IF_DEVICE, 42, &[dev]);
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(
        errors.is_empty(),
        "seed {seed}: WebGPU errors {:?}",
        &errors[..errors.len().min(10)]
    );
    model::with(|m| m.prims = None);
}

#[test]
fn d3d9_draws_read_what_direct3d_reads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    for seed in 0..8 {
        run(seed);
    }
}
