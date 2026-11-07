//! CopyResource / CopySubresourceRegion between formats through the real entry point, the
//! page's executor running on the model of WebGPU: Windows' answers in Wine's
//! test_format_compatibility (the bits between formats of one typeless group, nothing between
//! groups).

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

const BITS: [u32; 16] = [
    0xff0000ff, 0xff00ffff, 0xff00ff00, 0xffffff00, 0xffff0000, 0xffff00ff, 0xff000000, 0xff7f7f7f,
    0x3f800000, 0x40490fdb, 0x12345678, 0x80000001, 0x7f7fffff, 0x00000001, 0xdeadbeef, 0x01020304,
];

#[test]
fn format_copies() {
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
    let (sc, dev, ctx) = (rd1(sc_out), rd1(dev_out), rd1(ctx_out));
    let tex = |fmt: u32, data: &[u32]| -> u32 {
        let d = mem(11);
        put(d, &[4, 4, 1, 1, fmt, 1, 0, 0, 8, 0, 0]);
        let p = mem(16);
        put(p, data);
        let init = mem(3);
        put(init, &[p, 16, 0]);
        let o = mem(2);
        assert_eq!(
            call1(m(IF_DEVICE, 5), &[dev, d, init, o]),
            0,
            "format {fmt}"
        );
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
    let mut req = 0x5100_0000;
    let mut read = |t: u32| -> Vec<u32> {
        gpu::pump();
        req += 1;
        gpu::record(|b| b.cmd(&[gpu::READBACK, req, gpu_of(t), 0, 0, 0, 4, 4, rb, 16]));
        assert!(gpu::wait_readback(req));
        (0..16).map(|i| rd1(rb + 4 * i)).collect()
    };
    // (source, destination, whether the bits arrive): R8G8B8A8 UNORM / UINT / SRGB / SNORM /
    // SINT / TYPELESS, R32 FLOAT / UINT / SINT, R16G16 UINT / SINT, B8G8R8A8; across groups
    let cases = [
        (28, 30, true),
        (28, 29, true),
        (30, 32, true),
        (31, 28, true),
        (41, 42, true),
        (42, 43, true),
        (36, 38, true),
        (28, 87, false),
        (30, 36, false),
        (41, 28, false),
        (87, 91, true),
    ];
    for (s, d, copies) in cases {
        let src = tex(s, &BITS);
        let dst = tex(d, &[0; 16]);
        call1(m(IF_CONTEXT, 47), &[ctx, dst, src]);
        let want: Vec<u32> = if copies { BITS.to_vec() } else { vec![0; 16] };
        assert_eq!(read(dst), want, "CopyResource {s} -> {d}");
        // CopySubresourceRegion of a 3x3 box to (1, 1)
        let dst2 = tex(d, &[0; 16]);
        let bx = mem(6);
        put(bx, &[0, 0, 0, 3, 3, 1]);
        call1(m(IF_CONTEXT, 46), &[ctx, dst2, 0, 1, 1, 0, src, 0, bx]);
        let want: Vec<u32> = (0..16)
            .map(|j| {
                if copies && j % 4 >= 1 && j / 4 >= 1 {
                    BITS[j - 5]
                } else {
                    0
                }
            })
            .collect();
        assert_eq!(read(dst2), want, "CopySubresourceRegion {s} -> {d}");
        for t in [src, dst, dst2] {
            call1(m(IF_TEX2D, 2), &[t]);
        }
    }
    // (everything back: the next test counts the device's references from here)
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
