//! A lost page device (render/mod.rs `vnhost_render_lost`): the page's D3d is made again on a
//! new device (render/d3d/snap.rs `renew`), everything on it empty, and the guest's next call
//! uploads again what it keeps (device.rs `after_lost`). Through the real entry point, with the
//! page's executor on the model of WebGPU (render/model.rs) in the same thread.

use super::lock_tests::{fill_color, name_of, write_bytes};
use super::trace_tests::{alloc, CALLS, SERIAL};
use super::*;
use crate::render::model;

fn call(iface: u32, m: u32, args: &[u32]) -> u32 {
    let mut a = args.to_vec();
    a.resize(a.len().max(16), 0);
    unsafe { vnhost_d3d9_call(iface << 8 | m, a.as_ptr(), &CALLS) }
}

fn out() -> u32 {
    unsafe { alloc(16) as u32 }
}

fn rd(p: u32) -> u32 {
    unsafe { *(p as *const u32) }
}

fn with<R>(f: impl FnOnce(&mut D3d) -> R) -> R {
    f(D3D.lock().unwrap().as_mut().unwrap())
}

type Page = std::rc::Rc<std::cell::RefCell<Option<crate::render::d3d::D3d>>>;

/// The page, run whenever the guest waits for it; the D3d is the test's to replace.
fn attach() -> Page {
    model::reset();
    let page: Page = std::rc::Rc::new(std::cell::RefCell::new(Some(crate::render::d3d::D3d::new())));
    let pg = page.clone();
    gpu::set_pump(Some(Box::new(move || {
        while gpu::run_next(|c, data, blob| {
            let mut d = pg.borrow_mut();
            let d = d.as_mut().unwrap();
            d.run(c, data, blob);
            for t in model::with(|m| m.finish_maps()) {
                d.mapped(t, true);
            }
        }) {}
    })));
    page
}

/// The names of the 4x4 cells (lock_tests `name_of`) of what the page's texture of surface
/// `s` (16x16 A8R8G8B8) holds now.
fn page_cells(s: u32, req: u32) -> Vec<i64> {
    let g = with(|d| match &d.objs[&d.id_of(s)].data {
        Data::Surface(x) => x.gpu,
        _ => unreachable!(),
    });
    let dst = unsafe { alloc(16 * 16 * 4) as u32 };
    gpu::record(|b| b.cmd(&[gpu::READBACK, req, g, 0, 0, 0, 16, 16, dst, 64]));
    assert!(gpu::wait_readback(req));
    (0..16)
        .map(|c| {
            let (cx, cy) = (c % 4, c / 4);
            let px: Vec<[u8; 4]> = (0..16).map(|k| unsafe { *((dst + (cy * 4 + k / 4) * 64 + (cx * 4 + k % 4) * 4) as *const [u8; 4]) }).collect();
            name_of(&px)
        })
        .collect()
}

/// Writes write k's bytes into every pixel of a locked 16x16 rectangle.
fn write_all(o: u32, k: u32) {
    let (pitch, bits) = (rd(o), rd(o + 4));
    for y in 0..16 {
        for x in 0..16 {
            unsafe { *((bits + y * pitch + x * 4) as *mut [u8; 4]) = write_bytes(k) };
        }
    }
}

#[test]
fn a_lost_device_gets_what_the_guest_keeps() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let page = attach();
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
    // a managed texture's level 0, a default pool off-screen plain surface, a render target
    let o = out();
    assert_eq!(call(IF_DEVICE, 23, &[dev, 16, 16, 1, 0, 21, 1, o, 0]), 0);
    let tex = rd(o);
    let o = out();
    assert_eq!(call(IF_TEXTURE, 18, &[tex, 0, o]), 0);
    let level = rd(o);
    let o = out();
    assert_eq!(call(IF_DEVICE, 36, &[dev, 16, 16, 21, 0, o, 0]), 0);
    let plain = rd(o);
    let o = out();
    assert_eq!(call(IF_DEVICE, 28, &[dev, 16, 16, 21, 0, 0, 1, o, 0]), 0);
    let target = rd(o);
    let o = out();
    assert_eq!(call(IF_TEXTURE, 19, &[tex, 0, o, 0, 0]), 0);
    write_all(o, 1);
    assert_eq!(call(IF_TEXTURE, 20, &[tex, 0]), 0);
    let o = out();
    assert_eq!(call(IF_SURFACE, 13, &[plain, o, 0, 0]), 0);
    write_all(o, 2);
    assert_eq!(call(IF_SURFACE, 14, &[plain]), 0);
    assert_eq!(call(IF_DEVICE, 35, &[dev, target, 0, fill_color(3)]), 0);
    gpu::pump();
    let all = |k: i64| vec![k; 16];
    assert_eq!(page_cells(level, 0x6c00_0001), all(1));
    assert_eq!(page_cells(plain, 0x6c00_0002), all(2));
    assert_eq!(page_cells(target, 0x6c00_0003), all(100003));

    // the device is lost: a new page D3d on a new device, everything on it zeros
    {
        let mut p = page.borrow_mut();
        let old = p.take().unwrap();
        *p = Some(old.renew());
    }
    assert_eq!(page_cells(level, 0x6c00_0004), all(-2));
    let gen = crate::render::lost_gen();
    crate::render::set_lost_gen(gen + 1);
    // (the guest's next call, whatever it is)
    assert_eq!(call(IF_DEVICE, 3, &[dev]), 0);
    gpu::pump();
    assert_eq!(page_cells(level, 0x6c00_0005), all(1), "a managed texture");
    assert_eq!(page_cells(plain, 0x6c00_0006), all(2), "a default pool surface written through a lock");
    // (a render target's pixels were on the GPU only: zeros until the game draws it again)
    assert_eq!(page_cells(target, 0x6c00_0007), all(-2), "a render target");
    // once
    let sent = model::with(|m| m.calls.get("write_texture").copied().unwrap_or(0));
    assert_eq!(call(IF_DEVICE, 3, &[dev]), 0);
    gpu::pump();
    assert_eq!(model::with(|m| m.calls.get("write_texture").copied().unwrap_or(0)), sent);

    gpu::set_pump(None);
    crate::render::set_lost_gen(gen);
    with(|d| d.lost_seen = gen);
    let errors = model::with(|m| std::mem::take(&mut m.errors));
    assert!(errors.is_empty(), "WebGPU errors {errors:?}");
}
