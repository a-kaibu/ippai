//! Page copies of static vertex buffers (device::page_buffer) against the guest's bytes, on the
//! model of WebGPU: random lock / write / unlock runs (ranges of any size and alignment, the
//! same bytes written again, buffers of lengths not a multiple of 4), the page copy brought up
//! to date whenever a draw would read it, must hold the guest's bytes (zeros past the end); and
//! a buffer filled once and drawn from keeps no third copy (review b03 #13).

use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, Rng, CALLS, SERIAL};
use super::*;
use crate::render::model;
use std::{cell::RefCell, rc::Rc};

#[test]
fn page_copies_hold_the_guests_bytes() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    model::reset();
    let page = Rc::new(RefCell::new(crate::render::d3d::D3d::new()));
    let pg = page.clone();
    gpu::set_pump(Some(Box::new(move || while gpu::run_next(|c, data, blob| pg.borrow_mut().run(c, data, blob)) {})));
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o);
    let with = |f: &mut dyn FnMut(&mut D3d)| f(D3D.lock().unwrap().as_mut().unwrap());
    let mut r = Rng(0xb0ff_e125);
    let (mut kept_once, mut relocked) = (0, 0);
    for round in 0..40 {
        let len = [1u32, 3, 4, 6, 64, 1023, 4096][r.below(7)] + r.below(3) as u32;
        let o = out();
        assert_eq!(call(IF_DEVICE, 26, &[dev, len, 0, 0, 1, o, 0]), 0);
        let vb = rd(o);
        let id = { let mut i = 0; with(&mut |d| i = d.id_of(vb)); i };
        let mem = { let mut m = 0; with(&mut |d| if let Data::Buffer(b) = &d.objs[&id].data { m = b.mem }); m };
        let guest = || unsafe { std::slice::from_raw_parts(mem as *const u8, len as usize) }.to_vec();
        let mut sent = false;
        let mut locks = 0;
        // (every third buffer filled before its first draw only: a static mesh)
        let once = round % 3 == 0;
        for step in 0..30 {
            if r.below(3) != 0 && !(once && sent) {
                // a lock of a range (size 0: to the end), written in part, or the same bytes again
                let off = r.below(len as usize) as u32;
                let size = if r.chance(30) { 0 } else { 1 + r.below((len - off) as usize) as u32 };
                let p = out();
                assert_eq!(call(IF_VB, 11, &[vb, off, size, p, 0]), 0);
                let at = rd(p);
                let n = if size == 0 { len - off } else { size };
                let (a, b) = { let a = r.below(n as usize + 1) as u32; (a, a + r.below((n - a) as usize + 1) as u32) };
                let bytes: Vec<u8> = if r.chance(25) { unsafe { std::slice::from_raw_parts((at + a) as *const u8, (b - a) as usize) }.to_vec() } else { (a..b).map(|_| r.next() as u8).collect() };
                put_bytes(at + a, &bytes);
                assert_eq!(call(IF_VB, 12, &[vb]), 0);
                if sent {
                    locks += 1;
                }
            } else {
                // a draw reads the page copy: made, or brought up to date
                let mut g = 0;
                with(&mut |d| g = device::page_buffer(&mut d.objs, id).0);
                gpu::pump();
                sent = true;
                let obj = page.borrow().buffer_obj(g).expect("a page buffer");
                let held = model::with(|m| m.buffer_bytes(obj)).unwrap();
                assert_eq!(&held[..len as usize], &guest()[..], "round {round} step {step}: the page copy of a {len}-byte buffer");
                assert!(held[len as usize..].iter().all(|&x| x == 0), "round {round}: past the end");
            }
        }
        let shadow = { let mut n = 0; with(&mut |d| if let Data::Buffer(b) = &d.objs[&id].data { n = b.shadow.len() }); n };
        if sent && locks == 0 {
            kept_once += 1;
            assert_eq!(shadow, 0, "round {round}: a buffer sent once keeps a copy");
        }
        relocked += (locks > 0) as u32;
    }
    assert!(kept_once > 0 && relocked > 0, "{kept_once} {relocked}");
    gpu::set_pump(None);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(errors.is_empty(), "{errors:?}");
}

/// Indexed draws from resident index buffers take the range their indices name from a cache
/// kept by (buffer, its lock count, start, count) instead of scanning the indices every draw
/// (review b03 #16): random relocks (16- and 32-bit, written in part, the same bytes again)
/// and random list and strip draws; every list draw's triangles are the current indices'
/// (the vertices' x is their number), and after every draw each cached range is what a scan
/// of the indices gives now.
#[test]
fn cached_index_ranges_follow_every_lock() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    super::trace_tests::attach_page();
    model::with(|m| m.prims = Some(Vec::new()));
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [64, 64, 22, 1, 0, 0, 1, 1, 1, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x40, pp, o]), 0);
    let dev = rd(o);
    const N: u32 = 64;
    let o = out();
    assert_eq!(call(IF_DEVICE, 26, &[dev, N * 16, 0, 0, 1, o, 0]), 0);
    let vb = rd(o);
    let p = out();
    assert_eq!(call(IF_VB, 11, &[vb, 0, 0, p, 0]), 0);
    let bytes: Vec<u8> = (0..N).flat_map(|i| [(i as f32).to_le_bytes(), 1f32.to_le_bytes(), 0.5f32.to_le_bytes(), (0xff00_0000 | i).to_le_bytes()].concat()).collect();
    put_bytes(rd(p), &bytes);
    assert_eq!(call(IF_VB, 12, &[vb]), 0);
    for (rs, v) in [(22, 1), (137, 0), (7, 0)] {
        call(IF_DEVICE, 57, &[dev, rs, v]);
    }
    call(IF_DEVICE, 89, &[dev, 0x42]);
    call(IF_DEVICE, 100, &[dev, 0, vb, 0, 16]);
    call(IF_DEVICE, 41, &[dev]);
    let with = |f: &mut dyn FnMut(&mut D3d)| f(D3D.lock().unwrap().as_mut().unwrap());
    let mut r = Rng(0x1d_c0de);
    let (mut lists, mut used) = (0, 0);
    for round in 0..12 {
        let (fmt, isz) = [(101u32, 2u32), (102, 4)][round % 2];
        const I: u32 = 96;
        let o = out();
        assert_eq!(call(IF_DEVICE, 27, &[dev, I * isz, 0, fmt, 1, o, 0]), 0);
        let ib = rd(o);
        let id = { let mut i = 0; with(&mut |d| i = d.id_of(ib)); i };
        let mut ix: Vec<u32> = (0..I).map(|_| r.below(N as usize) as u32).collect();
        let write = |ix: &[u32], from: u32, to: u32| {
            let p = out();
            assert_eq!(call(IF_IB, 11, &[ib, from * isz, (to - from) * isz, p, 0]), 0);
            let b: Vec<u8> = ix[from as usize..to as usize].iter().flat_map(|&v| if isz == 4 { v.to_le_bytes().to_vec() } else { (v as u16).to_le_bytes().to_vec() }).collect();
            put_bytes(rd(p), &b);
            call(IF_IB, 12, &[ib]);
        };
        write(&ix, 0, I);
        call(IF_DEVICE, 104, &[dev, ib]);
        for step in 0..60 {
            if r.chance(25) {
                let a = r.below(I as usize) as u32;
                let b = a + 1 + r.below((I - a) as usize) as u32;
                if !r.chance(20) {
                    for v in &mut ix[a as usize..b as usize] {
                        *v = r.below(N as usize) as u32;
                    }
                }
                write(&ix, a, b);
                continue;
            }
            let strip = r.chance(30);
            // (a few draws over and over, as a mesh's parts are: the cache is hit)
            let start = [0u32, 3, 12, 30][r.below(4)];
            let left = I - start;
            let prims = if strip { [1u32, 4, left - 2][r.below(3)] } else { [1u32, 5, left / 3][r.below(3)] };
            let prim = if strip { 5 } else { 4 };
            assert_eq!(call(IF_DEVICE, 82, &[dev, prim, 0, 0, N, start, prims]), 0);
            gpu::flush();
            gpu::pump();
            let got = model::with(|m| m.prims.as_mut().unwrap().drain(..).collect::<Vec<_>>());
            if !strip {
                lists += 1;
                let xs: Vec<Vec<f32>> = got.iter().flatten().map(|t| t.iter().map(|v| v.iter().find(|a| a.1.len() == 12).map(|a| f32::from_le_bytes(a.1[..4].try_into().unwrap())).unwrap_or(-1.0)).collect()).collect();
                let want: Vec<Vec<f32>> = ix[start as usize..(start + 3 * prims) as usize].chunks(3).map(|t| t.iter().map(|&v| v as f32).collect()).collect();
                assert_eq!(xs, want, "round {round} step {step}: start {start} prims {prims}");
            }
            // the range this draw used (left in its slot): what a scan of the indices gives now
            let nverts = if strip { prims + 2 } else { 3 * prims };
            with(&mut |d| {
                let Some(Data::Buffer(b)) = d.objs.get(&id).map(|o| &o.data) else { return };
                let mem = b.mem;
                let slot = (id ^ start.wrapping_mul(31) ^ nverts.wrapping_mul(131)) as usize & 63;
                let e = d.dev.as_ref().unwrap().ranges[slot].get();
                assert_eq!((e.key.0, e.key.2, e.key.3), (id, start, nverts), "round {round} step {step}: the draw's range cached");
                let (lo, n, ffff) = unsafe { device::scan_indices(mem + start * isz, isz, nverts) };
                assert!((e.lo, e.n, e.ffff) == (lo, n, ffff), "round {round} step {step}: the range of start {start} count {nverts}");
            });
            used += 1;
        }
        call(IF_DEVICE, 104, &[dev, 0]);
        call(IF_IB, 2, &[ib]);
    }
    assert!(lists > 100 && used > 200, "{lists} {used}");
    gpu::set_pump(None);
}
