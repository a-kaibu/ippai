//! Additional swap chains (IDirect3DDevice9::CreateAdditionalSwapChain). ANGLE, the GLES of Ren'Py
//! 6/7 on Windows, makes its device on a hidden 1x1 window and draws every EGL window surface
//! through one of these: without them its eglCreateWindowSurface failed and Ren'Py fell back to
//! software drawing with a "performance warning" screen (RJ112723, #146). What they must do:
//! the parameters filled in (size 0 = the window's client area, D3DFMT_UNKNOWN = the display's
//! format) and refused as Windows refuses them, a back buffer of their own (not the device's),
//! shown in their own window by their Present, living on while the program holds it.
//! (tests/winref/probes/d3d9swapchain.c makes the same calls on Windows and on sake.)

use super::draw_tests::{call, out, rd};
use super::trace_tests::{alloc, attach_page, on_low_stack, CALLS, SERIAL};
use super::*;
use crate::render::d3d::D3d as Page;
use proptest::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

fn init() {
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
}

/// D3DPRESENT_PARAMETERS: w, h, format, count, swap effect, window, windowed
fn params(w: u32, h: u32, f: u32, count: u32, effect: u32, hwnd: u32, windowed: u32) -> u32 {
    let p = unsafe { alloc(56) as u32 };
    for (i, x) in [w, h, f, count, 0, 0, effect, hwnd, windowed, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((p + 4 * i as u32) as *mut u32) = x };
    }
    p
}
fn word(p: u32, i: u32) -> u32 {
    rd(p + 4 * i)
}

/// ANGLE's device: 1x1 on its hidden window 0x100, no depth buffer
fn angle_device() -> u32 {
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 0x100, 0x852, params(1, 1, 22, 1, 1, 0x100, 1), o]), 0);
    rd(o)
}

fn page() -> Rc<RefCell<Page>> {
    crate::render::model::reset();
    let page = Rc::new(RefCell::new(Page::new()));
    let pg = page.clone();
    gpu::set_pump(Some(Box::new(move || {
        while gpu::run_next(|c, data, blob| {
            let mut d = pg.borrow_mut();
            d.run(c, data, blob);
            for t in crate::render::model::with(|m| m.finish_maps()) {
                d.mapped(t, true);
            }
        }) {}
    })));
    page
}

fn live(addr: u32) -> bool {
    D3D.lock().unwrap().as_ref().unwrap().objs.contains_key(&unsafe { *((addr + 4) as *const u32) })
}

fn desc(surface: u32) -> [u32; 8] {
    let d = unsafe { alloc(32) as u32 };
    assert_eq!(call(IF_SURFACE, 12, &[surface, d]), 0);
    std::array::from_fn(|i| word(d, i as u32))
}

#[test]
fn angle_draws_its_window_through_an_additional_swap_chain() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    let page = page();
    let dev = angle_device();
    // (Ren'Py's window 0x200: its client area is the test's 64x64)
    let pp = params(0, 0, 0, 1, 1, 0x200, 1);
    let o = out();
    assert_eq!(call(IF_DEVICE, 13, &[dev, pp, o]), 0);
    let sc = rd(o);
    assert_ne!(sc, 0);
    assert_eq!([word(pp, 0), word(pp, 1), word(pp, 2), word(pp, 3)], [64, 64, 22, 1], "filled in as CreateDevice fills its own");
    let got = unsafe { alloc(56) as u32 };
    assert_eq!(call(IF_SWAPCHAIN, 9, &[sc, got]), 0);
    assert_eq!((0..14).map(|i| word(got, i)).collect::<Vec<_>>(), (0..14).map(|i| word(pp, i)).collect::<Vec<_>>());
    // its own back buffer, not the device's
    let o = out();
    assert_eq!(call(IF_SWAPCHAIN, 5, &[sc, 0, 0, o]), 0);
    let bb = rd(o);
    let o = out();
    assert_eq!(call(IF_DEVICE, 18, &[dev, 0, 0, 0, o]), 0);
    let dev_bb = rd(o);
    assert_ne!(bb, dev_bb);
    let dd = desc(bb);
    assert_eq!((dd[0], dd[2], dd[6], dd[7]), (22, 1, 64, 64), "X8R8G8B8, a render target, 64x64");
    assert_eq!(desc(dev_bb)[6..8], [1, 1]);
    assert_eq!(call(IF_SWAPCHAIN, 5, &[sc, 1, 0, out()]), D3DERR_INVALIDCALL, "one back buffer");
    // the implicit swap chain is still the only one GetSwapChain knows
    assert_eq!(call(IF_DEVICE, 15, &[dev]), 1);
    assert_eq!(call(IF_DEVICE, 14, &[dev, 1, out()]), D3DERR_INVALIDCALL);
    // drawn into and presented: the page shows it in Ren'Py's window, the device's Present in ANGLE's
    assert_eq!(call(IF_DEVICE, 37, &[dev, 0, bb]), 0);
    assert_eq!(call(IF_DEVICE, 43, &[dev, 0, 0, 1, 0xff20_4060, 0, 0]), 0);
    assert_eq!(call(IF_SWAPCHAIN, 3, &[sc, 0, 0, 0, 0, 0]), 0);
    gpu::flush();
    gpu::pump();
    let ov = page.borrow().overlay.expect("shown");
    assert_eq!((ov.hwnd, ov.rect), (0x200, [0, 0, 64, 64]));
    assert_eq!(call(IF_DEVICE, 17, &[dev, 0, 0, 0, 0]), 0);
    gpu::flush();
    gpu::pump();
    assert_eq!(page.borrow().overlay.unwrap().hwnd, 0x100);
    // the chain's back buffer lives while the program holds it, the chain goes at its last Release
    assert_eq!(call(IF_DEVICE, 37, &[dev, 0, dev_bb]), 0);
    assert_eq!(call(IF_SWAPCHAIN, 2, &[sc]), 0);
    assert!(!live(sc));
    assert!(live(bb), "held by the program");
    assert_eq!(call(IF_SURFACE, 2, &[bb]), 0);
    assert!(!live(bb));
    gpu::set_pump(None);
}

#[test]
fn additional_swap_chains_refused_as_windows_refuses_them() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    attach_page();
    let dev = angle_device();
    let bad = [
        ("full screen", params(640, 480, 22, 1, 1, 0x200, 0)),
        ("4 back buffers", params(640, 480, 22, 4, 1, 0x200, 1)),
        ("no swap effect", params(640, 480, 22, 1, 0, 0x200, 1)),
        ("swap effect 6", params(640, 480, 22, 1, 6, 0x200, 1)),
        ("not a back buffer format (A8)", params(640, 480, 28, 1, 1, 0x200, 1)),
    ];
    for (what, pp) in bad {
        let o = out();
        unsafe { *(o as *mut u32) = 0xdead };
        assert_eq!(call(IF_DEVICE, 13, &[dev, pp, o]), D3DERR_INVALIDCALL, "{what}");
        assert_eq!(rd(o), 0, "{what}: the out pointer is cleared");
    }
    assert_eq!(call(IF_DEVICE, 13, &[dev, 0, out()]), D3DERR_INVALIDCALL);
    assert_eq!(call(IF_DEVICE, 13, &[dev, params(8, 8, 22, 1, 1, 0, 1), 0]), D3DERR_INVALIDCALL);
    // count 0 is 1; no window: the device's
    let pp = params(8, 8, 23, 0, 3, 0, 1);
    let o = out();
    assert_eq!(call(IF_DEVICE, 13, &[dev, pp, o]), 0);
    assert_eq!(word(pp, 3), 1);
    let b = out();
    assert_eq!(call(IF_SWAPCHAIN, 5, &[rd(o), 0, 0, b]), 0);
    assert_eq!(desc(rd(b))[0], 23, "R5G6B5");
    gpu::set_pump(None);
}

#[test]
fn direct3d8_additional_swap_chain() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    init();
    on_low_stack(|| {
        attach_page();
        const V8: u32 = 16;
        let o = out();
        assert_eq!(unsafe { vnhost_d3d9_call(d3d8::CREATE, [o].as_ptr(), &CALLS) }, 0);
        let d3d = rd(o);
        let pp8 = |w: u32, h: u32, windowed: u32| {
            let p = unsafe { alloc(52) as u32 };
            for (i, x) in [w, h, 22, 1, 0, 1, 0x300, windowed, 0, 0, 0, 0, 0].into_iter().enumerate() {
                unsafe { *((p + 4 * i as u32) as *mut u32) = x };
            }
            p
        };
        let o = out();
        assert_eq!(call(IF_D3D9 + V8, 15, &[d3d, 0, 1, 0x300, 0x40, pp8(64, 64, 1), o]), 0);
        let dev = rd(o);
        let pp = pp8(0, 0, 1);
        let o = out();
        assert_eq!(call(IF_DEVICE + V8, 13, &[dev, pp, o]), 0);
        let sc = rd(o);
        assert_eq!((word(pp, 0), word(pp, 1)), (64, 64));
        // IDirect3DSwapChain8::GetBackBuffer(index, type, out): a Direct3D 8 surface
        let b = out();
        assert_eq!(call(IF_SWAPCHAIN + V8, 4, &[sc, 0, 0, b]), 0);
        assert_ne!(rd(b), 0);
        assert_eq!(call(IF_SWAPCHAIN + V8, 3, &[sc, 0, 0, 0, 0]), 0);
        assert_eq!(call(IF_DEVICE + V8, 13, &[dev, pp8(64, 64, 0), out()]), D3DERR_INVALIDCALL, "full screen");
        gpu::set_pump(None);
    });
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    /// Any parameters: created exactly when Windows would (count <= 3, DISCARD/FLIP/COPY,
    /// windowed, a back buffer format or UNKNOWN), with the size and format filled in; and any
    /// order of releasing the chains and their back buffers leaves nothing behind.
    #[test]
    fn any_parameters_and_release_order(chains in proptest::collection::vec((0u32..300, 0u32..300, prop_oneof![Just(0u32), Just(21), Just(22), Just(23), Just(28), Just(35), Just(50), Just(77)], 0u32..6, 0u32..7, 0u32..2), 1..6), order in proptest::collection::vec(any::<prop::sample::Index>(), 0..12)) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        init();
        attach_page();
        let dev = angle_device();
        let base = D3D.lock().unwrap().as_ref().unwrap().objs.len();
        let mut held = Vec::new();
        for &(w, h, f, count, effect, windowed) in &chains {
            let pp = params(w, h, f, count, effect, 0x200, windowed);
            let o = out();
            let r = call(IF_DEVICE, 13, &[dev, pp, o]);
            let ok = count <= 3 && (1..=3).contains(&effect) && windowed == 1 && matches!(f, 0 | 21 | 22 | 23 | 35);
            prop_assert_eq!(r == 0, ok, "{:?}", (w, h, f, count, effect, windowed));
            if !ok {
                prop_assert_eq!(rd(o), 0);
                continue;
            }
            let want = [if w == 0 { 64 } else { w }, if h == 0 { 64 } else { h }, if f == 0 { 22 } else { f }, count.max(1)];
            prop_assert_eq!([word(pp, 0), word(pp, 1), word(pp, 2), word(pp, 3)], want);
            let sc = rd(o);
            let b = out();
            prop_assert_eq!(call(IF_SWAPCHAIN, 5, &[sc, 0, 0, b]), 0);
            let dd = desc(rd(b));
            prop_assert_eq!((dd[0], dd[6], dd[7]), (want[2], want[0], want[1]));
            held.push((IF_SWAPCHAIN, sc));
            held.push((IF_SURFACE, rd(b)));
        }
        for i in order {
            if held.is_empty() {
                break;
            }
            let (iface, a) = held.remove(i.index(held.len()));
            call(iface, 2, &[a]);
        }
        for (iface, a) in held.drain(..) {
            call(iface, 2, &[a]);
        }
        prop_assert_eq!(D3D.lock().unwrap().as_ref().unwrap().objs.len(), base, "every chain and back buffer gone");
        gpu::set_pump(None);
    }
}
