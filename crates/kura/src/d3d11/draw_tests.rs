//! What a Direct3D 11 draw reads: random Draw, DrawIndexed, DrawInstanced and
//! DrawIndexedInstanced calls through the real entry point (`d3d11::call`), with input
//! layouts over one to three slots (per-vertex and per-instance data, D3D11_APPEND_ALIGNED_ELEMENT
//! offsets), vertex buffer offsets and strides, 16- and 32-bit indices with an index buffer
//! offset, base vertex locations, start instances, strip cuts, zero instances, every
//! topology. The vertex shader (compiled by hlslc) reads every input. The page runs each draw
//! on the model of WebGPU (render/model.rs); the primitives it assembles must be the ones
//! Direct3D 11 assembles, vertex for vertex and attribute for attribute.

use super::*;
use crate::d3d9::trace_tests::{alloc, attach_page, Rng, CALLS, SERIAL};
use crate::render::model;

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
const ELEMS: [(&str, u32, u32, u32); 4] = [("POSITION", 0, 6, 12), ("TEXCOORD", 0, 16, 8), ("COLOR", 0, 28, 4), ("TEXCOORD", 1, 2, 16)];

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
            _ => out.extend(run.windows(3).enumerate().map(|(i, w)| if i % 2 == 0 { w.to_vec() } else { vec![w[1], w[0], w[2]] })),
        }
    }
    out
}

fn run(seed: u64, vs: &[u8], ps: &[u8]) {
    let mut r = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 3);
    attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
    let desc = mem(16);
    put(desc, &[64, 64, 60, 1, 28, 0, 0, 1, 0, 0x20, 1, 1, 1, 0, 0]);
    let (sc_out, dev_out, level, ctx_out) = (mem(2), mem(2), mem(2), mem(2));
    assert_eq!(call1(CREATE_DEVICE, &[0, 1, 0, 0, 0, 0, 7, desc, sc_out, dev_out, level, ctx_out]), 0);
    let (sc, dev, ctx) = (rd1(sc_out), rd1(dev_out), rd1(ctx_out));
    let o = mem(2);
    // a render target and its view, the viewport
    let d = mem(11);
    put(d, &[64, 64, 1, 1, 28, 1, 0, 0, 0x20, 0, 0]);
    assert_eq!(call1(m(IF_DEVICE, 5), &[dev, d, 0, o]), 0);
    let rt = rd1(o);
    let vd = mem(4);
    put(vd, &[28, 4, 0, 0]);
    assert_eq!(call1(m(IF_DEVICE, 9), &[dev, rt, vd, o]), 0);
    let rtv = rd1(o);
    let arr = mem(4);
    put(arr, &[rtv]);
    call1(m(IF_CONTEXT, 33), &[ctx, 1, arr, 0]);
    let vp = mem(6);
    put(vp, &[0, 0, 64f32.to_bits(), 64f32.to_bits(), 0, 1f32.to_bits()]);
    call1(m(IF_CONTEXT, 44), &[ctx, 1, vp]);
    // the shaders
    let code = |c: &[u8]| {
        let p = unsafe { alloc(c.len() as u32) as u32 };
        put_bytes(p, c);
        p
    };
    let (vsc, psc) = (code(vs), code(ps));
    assert_eq!(call1(m(IF_DEVICE, 12), &[dev, vsc, vs.len() as u32, 0, o]), 0);
    let vsh = rd1(o);
    assert_eq!(call1(m(IF_DEVICE, 15), &[dev, psc, ps.len() as u32, 0, o]), 0);
    let psh = rd1(o);
    call1(m(IF_CONTEXT, 11), &[ctx, vsh, 0, 0]);
    call1(m(IF_CONTEXT, 9), &[ctx, psh, 0, 0]);
    // three vertex buffers of random bytes (finite floats), one index buffer per format
    const VB: u32 = 4096;
    let mut vbs = Vec::new();
    for _ in 0..3 {
        let bytes: Vec<u8> = (0..VB / 4).flat_map(|_| (((r.next() as u32) & 0x3fff_ffff) | 0x3000_0000).to_le_bytes()).collect();
        let init = mem(3);
        let p = unsafe { alloc(VB) as u32 };
        put_bytes(p, &bytes);
        put(init, &[p, 0, 0]);
        let bd = mem(6);
        put(bd, &[VB, 0, 1, 0, 0, 0]);
        assert_eq!(call1(m(IF_DEVICE, 3), &[dev, bd, init, o]), 0);
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
            let off = if append { next[s as usize] } else { next[s as usize] + 4 * r.below(3) as u32 };
            next[s as usize] = off + e.3;
            elems.push((s, off, e.3));
            put(ed + 28 * k as u32, &[names[k], e.1, e.2, s, if append { 0xffff_ffff } else { off }, (k == 3 && inst) as u32, (k == 3 && inst) as u32]);
        }
        assert_eq!(call1(m(IF_DEVICE, 11), &[dev, ed, 4, vsc, vs.len() as u32, o]), 0, "seed {seed} step {step}: CreateInputLayout");
        let layout = rd1(o);
        call1(m(IF_CONTEXT, 17), &[ctx, layout]);
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
        call1(m(IF_CONTEXT, 18), &[ctx, 0, 3, bufs, strides, offs]);
        let topology = 1 + r.below(5) as u32;
        call1(m(IF_CONTEXT, 24), &[ctx, topology]);
        let count = r.below(10) as u32;
        let indexed = r.chance(50);
        let instanced = r.chance(50);
        let instances = if instanced { r.below(4) as u32 } else { 1 };
        let start_instance = if instanced { r.below(4) as u32 } else { 0 };
        let start = r.below(6) as u32;
        let base = r.below(10) as i32 - 3;
        let i32 = r.chance(50);
        let cut = if i32 { u32::MAX } else { 0xffff };
        let ix: Vec<u32> = (0..start + count).map(|_| if r.chance(10) { cut } else { r.below(30) as u32 + 3 }).collect();
        let ib_off = 4 * r.below(4) as u32;
        // the vertex numbers D3D11 reads (None: a cut, in strips)
        let verts: Vec<Option<i64>> = if indexed {
            ix[start as usize..].iter().map(|&i| if i == cut { if topology == 3 || topology == 5 { None } else { Some(-1) } } else { Some(base as i64 + i as i64) }).collect()
        } else {
            (0..count).map(|k| Some(start as i64 + k as i64)).collect()
        };
        if verts.iter().any(|v| v.is_some_and(|v| v < 0)) {
            call1(m(IF_LAYOUT, 2), &[layout]);
            continue;
        }
        if indexed {
            let bytes: Vec<u8> = ix.iter().flat_map(|&v| if i32 { v.to_le_bytes().to_vec() } else { (v as u16).to_le_bytes().to_vec() }).collect();
            let mut all = vec![0u8; ib_off as usize];
            all.extend_from_slice(&bytes);
            let init = mem(3);
            let p = unsafe { alloc(all.len() as u32 + 4) as u32 };
            put_bytes(p, &all);
            put(init, &[p, 0, 0]);
            let bd = mem(6);
            put(bd, &[(all.len() as u32).next_multiple_of(4).max(4), 0, 2, 0, 0, 0]);
            assert_eq!(call1(m(IF_DEVICE, 3), &[dev, bd, init, o]), 0);
            let ib = rd1(o);
            call1(m(IF_CONTEXT, 19), &[ctx, ib, if i32 { 42 } else { 57 }, ib_off]);
            call1(m(IF_BUFFER, 2), &[ib]);
        }
        match (indexed, instanced) {
            (false, false) => call1(m(IF_CONTEXT, 13), &[ctx, count, start]),
            (true, false) => call1(m(IF_CONTEXT, 12), &[ctx, count, start, base as u32]),
            (false, true) => call1(m(IF_CONTEXT, 21), &[ctx, count, instances, start, start_instance]),
            (true, true) => call1(m(IF_CONTEXT, 20), &[ctx, count, instances, start, base as u32, start_instance]),
        };
        call1(m(IF_CONTEXT, 111), &[ctx]);
        gpu::pump();
        let vertex = |v: i64, instance: i64| -> Vec<Vec<u8>> {
            let mut a: Vec<Vec<u8>> = elems
                .iter()
                .enumerate()
                .map(|(k, &(s, off, n))| {
                    let v = if k == 3 && inst { instance } else { v };
                    let at = (offset[s as usize] as i64 + v * stride[s as usize] as i64 + off as i64) as usize;
                    vbs[s as usize].1.get(at..at + n as usize).map(|b| b.to_vec()).unwrap_or_default()
                })
                .collect();
            a.sort();
            a
        };
        let mut want = Vec::new();
        for i in 0..instances as i64 {
            for p in d3d_prims(topology, &verts) {
                want.push(p.iter().map(|&v| vertex(v, start_instance as i64 + i)).collect::<Vec<_>>());
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
        let what = format!("seed {seed} step {step}: topology {topology} count {count} indexed {indexed} (start {start} base {base} 32-bit {i32} offset {ib_off} {ix:?}) instances {instances} from {start_instance} per-instance {inst}");
        assert_eq!(got.len(), want.len(), "{what}: primitives");
        for (k, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(g == w, "{what}: primitive {k} differs\n got  {g:?}\n want {w:?}");
        }
        call1(m(IF_LAYOUT, 2), &[layout]);
        drawn += 1;
    }
    assert!(drawn > 50, "seed {seed}: {drawn} draws checked");
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(errors.is_empty(), "seed {seed}: WebGPU errors {:?}", &errors[..errors.len().min(10)]);
    model::with(|m| m.prims = None);
    call1(m(IF_CONTEXT, 110), &[ctx]);
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
        run(seed, &vs, &ps);
    }
}
