//! Multisampling through the real entry point, the page's executor running on the model of
//! WebGPU (render/model.rs, which knows WebGPU's multisampling rules): 4x render targets and
//! depth buffers, ResolveSubresource (the GPU's resolve, the shader one between formats of a
//! typeless group, the copy where the page could not multisample), CopyResource between 4x
//! textures, Texture2DMS read by a pixel shader, a 4x swap chain presented, the same recorded
//! on a deferred context, and the calls Windows ignores (sizes, formats, sample counts that do
//! not match). No WebGPU error anywhere; the values the model gives a resolve are the ones a
//! clear of a single-sample texture of the same color gives.

use super::*;
use crate::d3d9::trace_tests::{alloc, attach_page, CALLS, SERIAL};
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

const VS: &str = "float4 main(uint id : SV_VertexID) : SV_Position { float2 p = float2((id << 1) & 2, id & 2); return float4(p * 2 - 1, 0.5, 1); }";
const PS: &str =
    "float4 main(float4 p : SV_Position) : SV_Target { return float4(p.xy / 8, 0.5, 1); }";
const PS_MS: &str = "Texture2DMS<float4> t : register(t0);
float4 main(float4 p : SV_Position) : SV_Target { uint w, h, n; t.GetDimensions(w, h, n); return t.Load(int2(p.xy), n - 1) + t.Load(int2(p.xy), 0) * w; }";

#[test]
fn msaa() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let vt = mem(256);
    put(vt, &(1..=256).map(|i| 0x1000 * i).collect::<Vec<_>>());
    call1(INIT, &[vt, 256]);
    attach_page();
    model::with(|m| m.errors.clear());
    // a device with a 4x swap chain (BitBlt model: DXGI_SWAP_EFFECT_DISCARD)
    let desc = mem(16);
    put(desc, &[8, 8, 60, 1, 28, 0, 0, 4, 0, 0x20, 1, 1, 1, 0, 0]);
    let (sc_out, dev_out, level, ctx_out) = (mem(2), mem(2), mem(2), mem(2));
    assert_eq!(
        call1(
            CREATE_DEVICE,
            &[0, 1, 0, 0, 0, 0, 7, desc, sc_out, dev_out, level, ctx_out]
        ),
        0
    );
    let (sc, dev, ctx) = (rd1(sc_out), rd1(dev_out), rd1(ctx_out));
    let o = mem(2);
    let dc = |mm: u32, a: &[u32]| call1(m(IF_DEVICE, mm), &[&[dev][..], a].concat());
    let cc = |mm: u32, a: &[u32]| call1(m(IF_CONTEXT, mm), &[&[ctx][..], a].concat());
    // (count, quality) of a 2D texture, its bind flags
    let tex =
        |w: u32, h: u32, fmt: u32, samples: u32, quality: u32, bind: u32| -> Result<u32, u32> {
            let d = mem(11);
            put(d, &[w, h, 1, 1, fmt, samples, quality, 0, bind, 0, 0]);
            let hr = dc(5, &[d, 0, o]);
            if hr == 0 {
                Ok(rd1(o))
            } else {
                Err(hr)
            }
        };
    let rtv = |t: u32, fmt: Option<u32>| -> u32 {
        let vd = fmt.map(|f| {
            let p = mem(4);
            put(p, &[f, 6, 0, 0]);
            p
        });
        assert_eq!(dc(9, &[t, vd.unwrap_or(0), o]), 0, "RTV");
        rd1(o)
    };
    let gpu_of = |a: u32| {
        let g = D11S.lock().unwrap();
        let d = g.as_ref().unwrap();
        match &d.objs[&d.id_of(a)].data {
            Data::Texture(t) => t.gpu,
            _ => 0,
        }
    };
    let rb = mem(16 * 64);
    let mut req = 0x5300_0000;
    let mut read = |t: u32, w: u32, h: u32, bpp: u32| -> Vec<u8> {
        gpu::pump();
        req += 1;
        unsafe { std::ptr::write_bytes(rb as *mut u8, 0xcd, (w * h * bpp) as usize) };
        gpu::record(|b| b.cmd(&[gpu::READBACK, req, gpu_of(t), 0, 0, 0, w, h, rb, w * bpp]));
        assert!(gpu::wait_readback(req));
        (0..w * h * bpp)
            .map(|i| unsafe { *((rb + i) as *const u8) })
            .collect()
    };
    let color = mem(4);
    let clear = |v: u32, c: [f32; 4]| {
        put(color, &c.map(f32::to_bits));
        cc(50, &[v, color]);
    };
    let release = |o: u32| {
        call1(m(IF_TEX2D, 2), &[o]);
    };

    // quality levels (WARP's table) and the textures they allow
    let q = mem(1);
    assert_eq!(dc(30, &[28, 4, q]), 0);
    assert_eq!(rd1(q), 1);
    assert_eq!(dc(30, &[28, 3, q]), 0);
    assert_eq!(rd1(q), 0);
    assert!(
        tex(8, 8, 28, 4, 1, 0x20).is_err(),
        "quality past the levels"
    );
    assert!(tex(8, 8, 28, 3, 0, 0x20).is_err(), "3 samples");
    let ms = tex(8, 8, 28, 4, 0, 0x28).unwrap();
    let ms_std = tex(8, 8, 28, 4, 0xffff_ffff, 0x20).unwrap();
    // GetDesc keeps the sample description
    let td = mem(11);
    call1(m(IF_TEX2D, 10), &[ms_std, td]);
    assert_eq!((rd1(td + 20), rd1(td + 24)), (4, 0xffff_ffff));

    // a clear of a 4x target resolves to what the clear of a 1x one gives
    let c = [1.0, 0.5, 0.25, 1.0];
    let ms_rtv = rtv(ms, None);
    clear(ms_rtv, c);
    let dst = tex(8, 8, 28, 1, 0, 0x28).unwrap();
    cc(57, &[dst, 0, ms, 0, 28]);
    let want_tex = tex(8, 8, 28, 1, 0, 0x28).unwrap();
    clear(rtv(want_tex, None), c);
    let want = read(want_tex, 8, 8, 4);
    assert_eq!(read(dst, 8, 8, 4), want, "resolve of a clear");
    // ... and so does a copy of the 4x texture resolved
    let ms2 = tex(8, 8, 28, 4, 0, 0x20).unwrap();
    cc(47, &[ms2, ms]);
    let dst2 = tex(8, 8, 28, 1, 0, 0x28).unwrap();
    cc(57, &[dst2, 0, ms2, 0, 28]);
    assert_eq!(read(dst2, 8, 8, 4), want, "CopyResource of 4x, resolved");

    // what Windows ignores: another size, a 1x source, a 4x destination, another typeless
    // group, a mip or layer past the texture's
    let zero = read(tex(8, 8, 28, 1, 0, 0x28).unwrap(), 8, 8, 4);
    let small = tex(4, 4, 28, 1, 0, 0x28).unwrap();
    let before_small = read(small, 4, 4, 4);
    cc(57, &[small, 0, ms, 0, 28]);
    assert_eq!(read(small, 4, 4, 4), before_small, "another size");
    let fresh = tex(8, 8, 28, 1, 0, 0x28).unwrap();
    cc(57, &[fresh, 0, want_tex, 0, 28]);
    cc(57, &[ms2, 0, ms, 0, 28]);
    cc(57, &[fresh, 1, ms, 0, 28]);
    cc(57, &[fresh, 0, ms, 1, 28]);
    let bgra = tex(8, 8, 87, 1, 0, 0x28).unwrap();
    cc(57, &[bgra, 0, ms, 0, 87]);
    cc(57, &[fresh, 0, ms, 0, 87]);
    assert_eq!(read(fresh, 8, 8, 4), zero, "ignored resolves");
    assert_eq!(
        read(bgra, 8, 8, 4),
        zero,
        "a resolve across typeless groups"
    );

    // a typeless 4x texture drawn as UNORM, resolved into its sRGB twin (the shader resolve)
    let ms_tl = tex(8, 8, 27, 4, 0, 0x20).unwrap();
    clear(rtv(ms_tl, Some(28)), c);
    let srgb = tex(8, 8, 29, 1, 0, 0x28).unwrap();
    cc(57, &[srgb, 0, ms_tl, 0, 29]);
    assert_ne!(read(srgb, 8, 8, 4), zero, "shader resolve");

    // a format WebGPU does not multisample (R32G32_FLOAT): 1x on the page, the resolve a copy
    let ms_rg = tex(8, 8, 16, 4, 0, 0x20).unwrap();
    clear(rtv(ms_rg, None), c);
    let rg = tex(8, 8, 16, 1, 0, 0x28).unwrap();
    cc(57, &[rg, 0, ms_rg, 0, 16]);
    let rg_want = tex(8, 8, 16, 1, 0, 0x28).unwrap();
    clear(rtv(rg_want, None), c);
    assert_eq!(read(rg, 8, 8, 8), read(rg_want, 8, 8, 8), "copy resolve");

    // draws into the 4x target with a 4x depth buffer, resolved; a draw that reads its samples
    let code = |src: &str, target: &str| {
        let c = hlslc::compile(src, &[], &mut |_, _| None, "main", target, 0)
            .unwrap_or_else(|e| panic!("{target}: {e}"));
        let p = unsafe { alloc(c.len() as u32) as u32 };
        put_bytes(p, &c);
        (p, c.len() as u32)
    };
    let (vsc, vsn) = code(VS, "vs_4_0");
    let (psc, psn) = code(PS, "ps_4_0");
    let (pmc, pmn) = code(PS_MS, "ps_4_1");
    assert_eq!(dc(12, &[vsc, vsn, 0, o]), 0);
    let vs = rd1(o);
    assert_eq!(dc(15, &[psc, psn, 0, o]), 0);
    let ps = rd1(o);
    assert_eq!(dc(15, &[pmc, pmn, 0, o]), 0);
    let ps_ms = rd1(o);
    cc(11, &[vs, 0, 0]);
    cc(24, &[4]);
    let vp = mem(6);
    put(
        vp,
        &[0, 0, 8f32.to_bits(), 8f32.to_bits(), 0, 1f32.to_bits()],
    );
    cc(44, &[1, vp]);
    let z4 = tex(8, 8, 45, 4, 0, 0x40).unwrap();
    assert_eq!(dc(10, &[z4, 0, o]), 0);
    let dsv4 = rd1(o);
    let z1 = tex(8, 8, 45, 1, 0, 0x40).unwrap();
    assert_eq!(dc(10, &[z1, 0, o]), 0);
    let dsv1 = rd1(o);
    let targets = mem(1);
    put(targets, &[ms_rtv]);
    let draw = |ps: u32, dsv: u32| {
        cc(9, &[ps, 0, 0]);
        cc(33, &[1, targets, dsv]);
        cc(13, &[3, 0]);
    };
    clear(ms_rtv, c);
    draw(ps, dsv4);
    // (a 1x depth buffer with a 4x target: nothing drawn, no invalid pass)
    draw(ps, dsv1);
    cc(33, &[0, 0, 0]);
    cc(57, &[dst, 0, ms, 0, 28]);
    let drawn = read(dst, 8, 8, 4);
    assert_ne!(drawn, want, "a draw resolved");
    // Texture2DMS: the 4x texture's view, read into a 1x target
    assert_eq!(dc(7, &[ms, 0, o]), 0);
    let srv = rd1(o);
    let vdesc = mem(10);
    call1(m(IF_SRV, 8), &[srv, vdesc]);
    assert_eq!(
        rd1(vdesc + 4),
        6,
        "a NULL desc of a 4x texture views it as TEXTURE2DMS"
    );
    let out = tex(8, 8, 28, 1, 0, 0x28).unwrap();
    put(targets, &[rtv(out, None)]);
    let srvs = mem(1);
    put(srvs, &[srv]);
    cc(8, &[0, 1, srvs]);
    draw(ps_ms, 0);
    put(srvs, &[0]);
    cc(8, &[0, 1, srvs]);
    assert_ne!(read(out, 8, 8, 4), zero, "drawn from the samples");

    // the same clear and resolve recorded on a deferred context
    assert_eq!(dc(27, &[0, o]), 0);
    let def = rd1(o);
    let dfc = |mm: u32, a: &[u32]| call1(m(IF_CONTEXT, mm), &[&[def][..], a].concat());
    let c2 = [0.0, 1.0, 0.0, 1.0];
    put(color, &c2.map(f32::to_bits));
    dfc(50, &[ms_rtv, color]);
    let dst3 = tex(8, 8, 28, 1, 0, 0x28).unwrap();
    dfc(57, &[dst3, 0, ms, 0, 28]);
    assert_eq!(dfc(114, &[0, o]), 0);
    let list = rd1(o);
    assert_eq!(read(dst3, 8, 8, 4), zero, "not before it is executed");
    cc(58, &[list, 0]);
    let want3 = tex(8, 8, 28, 1, 0, 0x28).unwrap();
    clear(rtv(want3, None), c2);
    assert_eq!(
        read(dst3, 8, 8, 4),
        read(want3, 8, 8, 4),
        "deferred resolve"
    );

    // the 4x swap chain: its buffer and description, cleared and presented
    let iid = mem(4);
    put(iid, &[0x6f15aaf2, 0x4e89d208, 0x9548b49a, 0x9c4fd335]);
    assert_eq!(call1(m(IF_SWAPCHAIN, 9), &[sc, 0, iid, o]), 0);
    let bb = rd1(o);
    call1(m(IF_TEX2D, 10), &[bb, td]);
    assert_eq!((rd1(td + 20), rd1(td + 24)), (4, 0));
    let sd = mem(16);
    call1(m(IF_SWAPCHAIN, 12), &[sc, sd]);
    assert_eq!((rd1(sd + 28), rd1(sd + 32)), (4, 0));
    clear(rtv(bb, None), c);
    assert_eq!(call1(m(IF_SWAPCHAIN, 8), &[sc, 0, 0]), 0);
    release(bb);

    // a 3D render target cleared (the page draws to none: no invalid pass either)
    let d3 = mem(9);
    put(d3, &[4, 4, 4, 1, 28, 0, 0x20, 0, 0]);
    assert_eq!(dc(6, &[d3, 0, o]), 0);
    let t3 = rd1(o);
    let v3 = mem(5);
    put(v3, &[28, 8, 0, 0, 4]);
    assert_eq!(dc(9, &[t3, v3, o]), 0);
    clear(rd1(o), c);

    gpu::pump();
    call1(m(IF_SWAPCHAIN, 2), &[sc]);
    call1(m(IF_CONTEXT, 2), &[ctx]);
    call1(m(IF_DEVICE, 2), &[dev]);
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(
        errors.is_empty(),
        "WebGPU errors {:?}",
        &errors[..errors.len().min(10)]
    );
}
