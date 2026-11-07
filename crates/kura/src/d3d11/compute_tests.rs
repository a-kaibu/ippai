//! Direct3D 11 compute shaders (Dispatch, DispatchIndirect, typed UAVs) through the real entry
//! point, compiled by hlslc, the page's executor running on the model of WebGPU: no WebGPU
//! validation error, and what a dispatch writes depends on what it reads (constants, shader
//! resources, the UAV's old texels it reads back) and on nothing else.

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
fn rd1(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}
fn call1(id: u32, args: &[u32]) -> u32 {
    unsafe { call(id, args.as_ptr(), &CALLS) }
}
fn m(iface: u32, m: u32) -> u32 {
    iface << 8 | m
}

const CS: &str = "Texture2D<float4> t : register(t0); SamplerState s : register(s0);
cbuffer C : register(b0) { float4 k; };
RWTexture2D<float4> u : register(u0);
[numthreads(4, 4, 1)] void main(uint3 id : SV_DispatchThreadID) { u[id.xy] = t[id.xy] * k + t.SampleLevel(s, id.xy / 8.0, 0); }";
// (reads its UAV: ld_uav_typed)
const RMW: &str = "RWTexture2D<unorm float4> u : register(u0);
[numthreads(1, 1, 1)] void main(uint3 id : SV_DispatchThreadID) { u[id.xy] = u[id.xy].yzwx * 0.5; }";
const SHARED: &str = "RWTexture2D<float4> u : register(u1); groupshared float4 g[16];
[numthreads(16, 1, 1)] void main(uint3 id : SV_DispatchThreadID, uint i : SV_GroupIndex) {
  g[i] = float4(id, 1); GroupMemoryBarrierWithGroupSync(); u[id.xy] = g[15 - i]; }";

struct Dev {
    dev: u32,
    ctx: u32,
    sc: u32,
    req: u32,
}

impl Dev {
    fn new() -> Dev {
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
        Dev {
            dev: rd1(dev_out),
            ctx: rd1(ctx_out),
            sc: rd1(sc_out),
            req: 0x5200_0000,
        }
    }
    fn dc(&self, mm: u32, a: &[u32]) -> u32 {
        call1(m(IF_DEVICE, mm), &[&[self.dev][..], a].concat())
    }
    fn cc(&self, mm: u32, a: &[u32]) -> u32 {
        call1(m(IF_CONTEXT, mm), &[&[self.ctx][..], a].concat())
    }
    /// an 8x8 texture of `fmt` with `bind` flags and texels `data`
    fn tex(&self, fmt: u32, bind: u32, data: &[u32]) -> u32 {
        let d = mem(11);
        put(d, &[8, 8, 1, 1, fmt, 1, 0, 0, bind, 0, 0]);
        let p = mem(64);
        put(p, data);
        let init = mem(3);
        put(init, &[p, 32, 0]);
        let o = mem(2);
        assert_eq!(self.dc(5, &[d, init, o]), 0, "CreateTexture2D {fmt}");
        rd1(o)
    }
    fn srv(&self, t: u32) -> u32 {
        let o = mem(2);
        assert_eq!(self.dc(7, &[t, 0, o]), 0);
        rd1(o)
    }
    fn uav(&self, t: u32) -> u32 {
        let o = mem(2);
        assert_eq!(self.dc(8, &[t, 0, o]), 0, "CreateUnorderedAccessView");
        rd1(o)
    }
    fn cs(&self, src: &str) -> u32 {
        let code = hlslc::compile(src, &[], &mut |_, _| None, "main", "cs_5_0", 0)
            .unwrap_or_else(|e| panic!("cs: {e}"));
        let p = unsafe { alloc(code.len() as u32) as u32 };
        unsafe { std::ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len()) };
        let o = mem(2);
        assert_eq!(
            self.dc(18, &[p, code.len() as u32, 0, o]),
            0,
            "CreateComputeShader"
        );
        rd1(o)
    }
    fn cbuf(&self, v: [f32; 4]) -> u32 {
        let d = mem(6);
        put(d, &[16, 0, 4, 0, 0, 0]);
        let p = mem(4);
        put(p, &v.map(f32::to_bits));
        let init = mem(3);
        put(init, &[p, 0, 0]);
        let o = mem(2);
        assert_eq!(self.dc(3, &[d, init, o]), 0);
        rd1(o)
    }
    fn set(&self, mm: u32, slot: u32, obj: u32) {
        let a = mem(1);
        put(a, &[obj]);
        if mm == 69 {
            self.cc(69, &[obj, 0, 0]);
        } else if mm == 68 {
            self.cc(68, &[slot, 1, a, 0]);
        } else {
            self.cc(mm, &[slot, 1, a]);
        }
    }
    fn read(&mut self, t: u32) -> Vec<u32> {
        let g = {
            let g = D11S.lock().unwrap();
            let d = g.as_ref().unwrap();
            match &d.objs[&d.id_of(t)].data {
                Data::Texture(x) => x.gpu,
                _ => 0,
            }
        };
        gpu::pump();
        self.req += 1;
        let rb = mem(64);
        gpu::record(|b| b.cmd(&[gpu::READBACK, self.req, g, 0, 0, 0, 8, 8, rb, 32]));
        assert!(gpu::wait_readback(self.req));
        (0..64).map(|i| rd1(rb + 4 * i)).collect()
    }
    fn done(self) {
        self.cc(110, &[]);
        call1(m(IF_SWAPCHAIN, 2), &[self.sc]);
        call1(m(IF_CONTEXT, 2), &[self.ctx]);
        call1(m(IF_DEVICE, 2), &[self.dev]);
        gpu::set_pump(None);
        let errors = model::with(|m| std::mem::take(&mut m.errors));
        assert!(
            errors.is_empty(),
            "WebGPU errors {:?}",
            &errors[..errors.len().min(10)]
        );
    }
}

const RGBA8: u32 = 28;
const RGBA32F: u32 = 2;
const PATTERN: fn(u32) -> Vec<u32> = |s| {
    (0..64)
        .map(|i| (i * 0x01030507u32).wrapping_add(s))
        .collect()
};

#[test]
fn dispatch_writes_by_what_it_reads() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut d = Dev::new();
    let cs = d.cs(CS);
    let input = d.tex(RGBA8, 8, &PATTERN(1));
    let input2 = d.tex(RGBA8, 8, &PATTERN(2));
    let (srv, srv2) = (d.srv(input), d.srv(input2));
    let (k1, k2) = (d.cbuf([1.0, 0.5, 0.25, 1.0]), d.cbuf([2.0, 0.5, 0.25, 1.0]));
    // a run of the shader into a fresh UAV texture: what it holds after
    let run =
        |d: &mut Dev, srv: u32, k: u32, groups: [u32; 3], indirect: bool| -> (Vec<u32>, Vec<u32>) {
            let out = d.tex(RGBA8, 0x88, &PATTERN(9));
            let before = d.read(out);
            let uav = d.uav(out);
            d.set(69, 0, cs);
            d.set(67, 0, srv);
            d.set(71, 0, k);
            d.set(68, 0, uav);
            if indirect {
                let bd = mem(6);
                // (D3D11_RESOURCE_MISC_DRAWINDIRECT_ARGS)
                put(bd, &[16, 0, 0, 0, 0x10, 0]);
                let p = mem(4);
                put(p, &[0, groups[0], groups[1], groups[2]]);
                let init = mem(3);
                put(init, &[p, 0, 0]);
                let o = mem(2);
                assert_eq!(d.dc(3, &[bd, init, o]), 0);
                d.cc(42, &[rd1(o), 4]);
            } else {
                d.cc(41, &groups.to_vec());
            }
            d.set(68, 0, 0);
            (before, d.read(out))
        };
    let (before, a) = run(&mut d, srv, k1, [2, 2, 1], false);
    assert_ne!(a, before, "Dispatch wrote nothing");
    assert_eq!(
        run(&mut d, srv, k1, [2, 2, 1], false).1,
        a,
        "the same dispatch wrote something else"
    );
    assert_eq!(
        run(&mut d, srv, k1, [2, 2, 1], true).1,
        a,
        "DispatchIndirect with the same counts"
    );
    assert_ne!(
        run(&mut d, srv, k2, [2, 2, 1], false).1,
        a,
        "other constants"
    );
    assert_ne!(
        run(&mut d, srv2, k1, [2, 2, 1], false).1,
        a,
        "another shader resource"
    );
    assert_ne!(
        run(&mut d, srv, k1, [1, 2, 1], false).1,
        a,
        "other group counts"
    );
    // no groups, or more than 65535 in a dimension: nothing runs
    let (before, after) = run(&mut d, srv, k1, [0, 2, 1], false);
    assert_eq!(after, before);
    let (before, after) = run(&mut d, srv, k1, [65536, 1, 1], false);
    assert_eq!(after, before);
    d.done();
}

#[test]
fn dispatch_reads_its_uav_and_groupshared() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut d = Dev::new();
    // a UAV the shader reads back (ld_uav_typed: a copy of what it held, on the page)
    let rmw = d.cs(RMW);
    let a = d.tex(RGBA8, 0x88, &PATTERN(3));
    let b = d.tex(RGBA8, 0x88, &PATTERN(4));
    let (ua, ub) = (d.uav(a), d.uav(b));
    d.set(69, 0, rmw);
    d.set(68, 0, ua);
    d.cc(41, &[8, 8, 1]);
    d.set(68, 0, ub);
    d.cc(41, &[8, 8, 1]);
    d.set(68, 0, 0);
    let (ra, rb) = (d.read(a), d.read(b));
    assert_ne!(ra, PATTERN(3));
    assert_ne!(ra, rb, "what the UAV held went in");
    // groupshared memory and a barrier, the UAV in slot 1 of a float format
    let sh = d.cs(SHARED);
    let f = d.tex(RGBA32F, 0x88, &[0; 64]);
    let before = d.read(f);
    let uf = d.uav(f);
    d.set(69, 0, sh);
    d.set(68, 1, uf);
    d.cc(41, &[1, 8, 1]);
    d.set(68, 1, 0);
    assert_ne!(d.read(f), before);
    d.done();
}

#[test]
fn dispatch_without_a_storage_texture_runs_nothing() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut d = Dev::new();
    let rmw = d.cs(RMW);
    d.set(69, 0, rmw);
    // no UAV bound: writes go nowhere
    d.cc(41, &[8, 8, 1]);
    // R8_UNORM: a typed UAV Direct3D 11 has, WebGPU has no such storage texture (no dispatch
    // rather than an invalid one)
    let t = d.tex(61, 0x88, &[0x11223344; 64]);
    let before = d.read(t);
    let u = d.uav(t);
    d.set(68, 0, u);
    d.cc(41, &[8, 8, 1]);
    d.set(68, 0, 0);
    assert_eq!(d.read(t), before);
    d.done();
}

#[test]
fn a_uav_unbinds_the_compute_shaders_srv_of_it() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let d = Dev::new();
    let t = d.tex(RGBA8, 0x88, &PATTERN(5));
    let (s, u) = (d.srv(t), d.uav(t));
    d.set(67, 0, s);
    d.set(68, 0, u);
    let o = mem(1);
    d.cc(105, &[0, 1, o]);
    assert_eq!(
        rd1(o),
        0,
        "CSGetShaderResources: the SRV of the UAV's texture stays bound"
    );
    // (and bound again while the UAV is: NULL)
    d.set(67, 0, s);
    d.cc(105, &[0, 1, o]);
    assert_eq!(rd1(o), 0);
    d.set(68, 0, 0);
    d.done();
}
