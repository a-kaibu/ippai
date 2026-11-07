//! What a Direct3D 8 draw reads: random DrawPrimitive, DrawIndexedPrimitive (the base vertex
//! SetIndices gave), DrawPrimitiveUP and DrawIndexedPrimitiveUP through the Direct3D 8
//! interfaces (d3d8.rs turns them into Direct3D 9 calls), with vertex shader declarations
//! (STREAM, REG and SKIP tokens, fixed function: no shader function) and FVFs, of one or two
//! streams, strides with padding, 16- and 32-bit indices, MinIndex / NumVertices that do not
//! cover the indices used, every primitive type. As draw_tests does for Direct3D 9: the
//! primitives WebGPU's model assembles must be Direct3D 8's, vertex for vertex.

use super::draw_tests::{call, d3d_prims, layouts, out, put_bytes, rd, rotated, type_bytes};
use super::trace_tests::{alloc, attach_page, on_low_stack, Rng, CALLS, SERIAL};
use super::*;
use crate::render::model;

const V8: u32 = 16;

/// The declaration's register for a Direct3D 9 usage (D3DVSDE_*)
fn reg(usage: u32, index: u32) -> u32 {
    match usage {
        0 => 0,
        3 => 3,
        10 => 5 + index,
        5 => 7 + index,
        _ => unreachable!(),
    }
}

/// D3DVSD tokens of a layout: each stream's elements by offset, SKIPs for the gaps.
fn decl_tokens(elems: &[(u32, u32, u32, u32, u32)]) -> Vec<u32> {
    let mut t = Vec::new();
    let mut streams: Vec<u32> = elems.iter().map(|e| e.0).collect();
    streams.sort();
    streams.dedup();
    for s in streams {
        t.push(1 << 29 | s);
        let mut es: Vec<_> = elems.iter().filter(|e| e.0 == s).collect();
        es.sort_by_key(|e| e.1);
        let mut at = 0;
        for e in es {
            if e.1 > at {
                t.push(2 << 29 | 0x1000_0000 | ((e.1 - at) / 4) << 16);
            }
            t.push(2 << 29 | e.2 << 16 | reg(e.3, e.4));
            at = e.1 + type_bytes(e.2);
        }
    }
    t.push(0xffff_ffff);
    t
}

fn run(seed: u64) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 9);
    attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
    let o = out();
    let args = [o];
    assert_eq!(
        unsafe { vnhost_d3d9_call(d3d8::CREATE, args.as_ptr(), &CALLS) },
        0
    );
    let d3d = rd(o);
    let pp = unsafe { alloc(52) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 1, 1, 1, 0, 0, 0, 0, 0]
        .into_iter()
        .enumerate()
    {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9 + V8, 15, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o);
    let devc = |m: u32, a: &[u32]| -> u32 { call(IF_DEVICE + V8, m, &[&[dev][..], a].concat()) };
    const VB: u32 = 8192;
    let mut vbs = Vec::new();
    for _ in 0..2 {
        let o = out();
        assert_eq!(devc(23, &[VB, 0, 0, 1, o]), 0);
        let vb = rd(o);
        let p = out();
        assert_eq!(call(IF_VB + V8, 11, &[vb, 0, 0, p, 0]), 0);
        let bytes: Vec<u8> = (0..VB / 4)
            .flat_map(|_| (((r.next() as u32) & 0x3fff_ffff) | 0x3000_0000).to_le_bytes())
            .collect();
        put_bytes(rd(p), &bytes);
        assert_eq!(call(IF_VB + V8, 12, &[vb]), 0);
        vbs.push((vb, bytes));
    }
    let mut ibs = Vec::new();
    for fmt in [101u32, 102] {
        let o = out();
        assert_eq!(devc(24, &[1024, 0, fmt, 1, o]), 0);
        ibs.push(rd(o));
    }
    let lays = layouts();
    let shaders: Vec<u32> = lays
        .iter()
        .map(|l| {
            if l.fvf != 0 {
                return l.fvf;
            }
            let t = decl_tokens(&l.elems);
            let p = unsafe { alloc(4 * t.len() as u32) as u32 };
            put_bytes(
                p,
                &t.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>(),
            );
            let o = out();
            assert_eq!(devc(75, &[p, 0, o, 0]), 0);
            let h = rd(o);
            // (GetVertexShaderDeclaration gives the tokens back)
            let (data, size) = (unsafe { alloc(4 * t.len() as u32) as u32 }, out());
            unsafe { *(size as *mut u32) = 4 * t.len() as u32 };
            assert_eq!(devc(81, &[h, data, size]), 0);
            assert_eq!(rd(size), 4 * t.len() as u32);
            assert_eq!(
                (0..t.len())
                    .map(|i| rd(data + 4 * i as u32))
                    .collect::<Vec<_>>(),
                t
            );
            h
        })
        .collect();
    for (rs, v) in [(22, 1), (137, 0), (7, 0)] {
        devc(50, &[rs, v]);
    }
    devc(34, &[]);
    let up = unsafe { alloc(VB) as u32 };
    let upi = unsafe { alloc(1024) as u32 };
    let mut drawn = 0;
    for step in 0..120 {
        let li = r.below(lays.len());
        let l = &lays[li];
        assert_eq!(devc(76, &[shaders[li]]), 0);
        let mut streams: Vec<u32> = l.elems.iter().map(|e| e.0).collect();
        streams.dedup();
        let mut stride = [0u32; 2];
        for &s in &streams {
            let need = l
                .elems
                .iter()
                .filter(|e| e.0 == s)
                .map(|e| e.1 + type_bytes(e.2))
                .max()
                .unwrap();
            stride[s as usize] = need + 4 * r.below(3) as u32;
        }
        let prim = 1 + r.below(6) as u32;
        let n = 1 + r.below(6) as u32;
        let count = [0, n, 2 * n, n + 1, 3 * n, n + 2, n + 2][prim as usize];
        let kind = r.below(4);
        let isz = if r.chance(50) { 2 } else { 4 };
        let mut base: i64 = r.below(12) as i64;
        let ix: Vec<u32> = (0..count).map(|_| r.below(24) as u32).collect();
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
        let verts: Vec<i64> = match kind {
            0 | 2 => (0..count as i64)
                .map(|k| first_vertex as i64 * (kind == 0) as i64 + k)
                .collect(),
            _ => ix.iter().map(|&i| base + i as i64).collect(),
        };
        let ibytes: Vec<u8> = ix
            .iter()
            .flat_map(|&v| {
                if isz == 4 {
                    v.to_le_bytes().to_vec()
                } else {
                    (v as u16).to_le_bytes().to_vec()
                }
            })
            .collect();
        let res = match kind {
            0 | 1 => {
                for &s in &streams {
                    devc(83, &[s, vbs[s as usize].0, stride[s as usize]]);
                }
                if kind == 0 {
                    devc(70, &[prim, first_vertex, n])
                } else {
                    let ib = ibs[(isz == 4) as usize];
                    let p = out();
                    assert_eq!(call(IF_IB + V8, 11, &[ib, 0, 0, p, 0]), 0);
                    put_bytes(rd(p) + start * isz, &ibytes);
                    call(IF_IB + V8, 12, &[ib]);
                    devc(85, &[ib, base as u32]);
                    // (GetIndices: the buffer and the base vertex back)
                    let (o, b) = (out(), out());
                    assert_eq!(devc(86, &[o, b]), 0);
                    assert_eq!((rd(b), rd(o) == ib), (base as u32, true));
                    call(IF_IB + V8, 2, &[rd(o)]);
                    devc(71, &[prim, min, num, start, n])
                }
            }
            _ => {
                if streams.len() > 1 {
                    continue;
                }
                put_bytes(up, &vbs[0].1);
                if kind == 2 {
                    devc(72, &[prim, n, up, stride[0]])
                } else {
                    put_bytes(upi, &ibytes);
                    devc(
                        73,
                        &[
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
        gpu::flush();
        gpu::pump();
        let vertex = |v: i64| -> Vec<Vec<u8>> {
            let mut a: Vec<Vec<u8>> = l
                .elems
                .iter()
                .map(|&(s, off, t, _, _)| {
                    let at = (v * stride[s as usize] as i64 + off as i64) as usize;
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
        let want: Vec<Vec<Vec<Vec<u8>>>> = d3d_prims(prim, &verts, n)
            .into_iter()
            .map(|p| rotated(&p.iter().map(|&v| vertex(v)).collect::<Vec<_>>()))
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
        assert_eq!(
            got.len(),
            want.len(),
            "seed {seed} step {step}: {what} prim {prim} x{n} layout {li} base {base}: primitives"
        );
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(g == w, "seed {seed} step {step}: {what} prim {prim} x{n} layout {li} base {base} min {min} num {num} start {start} isz {isz} index {ix:?}: primitive {k} differs\n got  {g:?}\n want {w:?}");
        }
        drawn += 1;
    }
    assert!(drawn > 60, "seed {seed}: {drawn} draws checked");
    devc(35, &[]);
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
fn d3d8_draws_read_what_direct3d_reads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    on_low_stack(|| {
        unsafe {
            let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
            vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
        }
        for seed in 0..8 {
            run(seed);
        }
    });
}
