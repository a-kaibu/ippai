//! Present's source and destination rectangles (windowed): what the page shows of the back
//! buffer, and where. 俺の姿が､透明に! made its device on a 2327x1358 client (the window was
//! that big for a moment) and presents (0,0)-(1920,1080) of it into its 1920x1080 client: the
//! page showed the whole back buffer squeezed into the window.

use super::device::{present_dest, present_src_words};
use super::draw_tests::{call, out, put_bytes, rd};
use super::trace_tests::{alloc, CALLS, SERIAL};
use super::*;
use crate::render::d3d::{present_src, D3d};
use proptest::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

/// The page's side, kept to look at its overlay.
fn page() -> Rc<RefCell<D3d>> {
    crate::render::model::reset();
    let page = Rc::new(RefCell::new(D3d::new()));
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

/// A `w`x`h` back buffer, windowed or not (the test's client area is (0,0)-(64,64)).
fn device(w: u32, h: u32, windowed: u32) -> u32 {
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [w, h, 22, 1, 0, 0, 1, 1, windowed, 0, 0, 0, 0, 0].into_iter().enumerate() {
        unsafe { *((pp + 4 * i as u32) as *mut u32) = x };
    }
    let o = out();
    assert_eq!(call(IF_D3D9, 16, &[d3d, 0, 1, 1, 0x20, pp, o]), 0);
    rd(o)
}

fn rect(r: [i32; 4]) -> u32 {
    let p = unsafe { alloc(16) as u32 };
    put_bytes(p, &r.iter().flat_map(|v| v.to_le_bytes()).collect::<Vec<_>>());
    p
}

/// Presents (IDirect3DDevice9::Present) and gives what the page then shows: rect, source uv.
fn present(page: &Rc<RefCell<D3d>>, dev: u32, src: u32, dst: u32) -> ([i32; 4], [f32; 4]) {
    assert_eq!(call(IF_DEVICE, 17, &[dev, src, dst, 0, 0]), 0);
    gpu::flush();
    gpu::pump();
    let o = page.borrow().overlay.expect("shown");
    (o.rect, o.src)
}

#[test]
fn present_rectangles_choose_what_is_shown_and_where() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let page = page();
    // (the test's client area is 64x64; the back buffer 128x96, as 俺の姿's was bigger)
    let dev = device(128, 96, 1);
    assert_eq!(present(&page, dev, 0, 0), ([0, 0, 64, 64], [0.0, 0.0, 1.0, 1.0]), "no rectangles: all of it, over the client area");
    assert_eq!(present(&page, dev, rect([0, 0, 64, 64]), rect([0, 0, 64, 64])), ([0, 0, 64, 64], [0.0, 0.0, 0.5, 64.0 / 96.0]), "俺の姿: the top left part");
    assert_eq!(present(&page, dev, rect([32, 48, 96, 96]), 0), ([0, 0, 64, 64], [0.25, 0.5, 0.75, 1.0]));
    assert_eq!(present(&page, dev, 0, rect([8, 4, 40, 36])), ([8, 4, 40, 36], [0.0, 0.0, 1.0, 1.0]), "into a part of the client area");
    // (past the back buffer: clipped to it; empty ones as none)
    assert_eq!(present(&page, dev, rect([-10, -10, 500, 500]), 0).1, [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(present(&page, dev, rect([10, 10, 10, 50]), rect([5, 5, 5, 5])), ([0, 0, 64, 64], [0.0, 0.0, 1.0, 1.0]));
    // the swap chain's Present (IDirect3DSwapChain9 3) the same
    let o = out();
    assert_eq!(call(IF_DEVICE, 14, &[dev, 0, o]), 0);
    let sc = rd(o);
    assert_eq!(call(IF_SWAPCHAIN, 3, &[sc, rect([0, 0, 64, 48]), 0, 0, 0, 0]), 0);
    gpu::flush();
    gpu::pump();
    assert_eq!(page.borrow().overlay.unwrap().src, [0.0, 0.0, 0.5, 0.5]);
    // full screen: Windows takes no rectangles
    let dev = device(128, 96, 0);
    assert_eq!(present(&page, dev, rect([0, 0, 64, 64]), rect([8, 8, 16, 16])), ([0, 0, 64, 64], [0.0, 0.0, 1.0, 1.0]));
    gpu::set_pump(None);
}

proptest! {
    /// Any source rectangle in a back buffer goes to the page and back as the same part of it;
    /// any rectangle at all gives a part inside the texture (or all of it), never an empty one.
    #[test]
    fn source_rectangles_round_trip(w in 1u32..4096, h in 1u32..4096, l in -100i32..5000, t in -100i32..5000, dw in -50i32..5000, dh in -50i32..5000) {
        let r = [l, t, l + dw, t + dh];
        let (a6, a7) = present_src_words(Some(r), w, h);
        let uv = present_src(a6, a7, w, h);
        prop_assert!(uv[0] >= 0.0 && uv[1] >= 0.0 && uv[2] <= 1.0 && uv[3] <= 1.0 && uv[0] < uv[2] && uv[1] < uv[3], "{:?}", uv);
        let inside = l >= 0 && t >= 0 && dw > 0 && dh > 0 && (l + dw) as u32 <= w && (t + dh) as u32 <= h;
        if inside {
            let want = [l as f32 / w as f32, t as f32 / h as f32, (l + dw) as f32 / w as f32, (t + dh) as f32 / h as f32];
            prop_assert_eq!(uv, want);
        }
        // no rectangle, and the texture's own size (Direct3D 11, OpenGL): all of it
        let (a6, a7) = present_src_words(None, w, h);
        prop_assert_eq!(present_src(a6, a7, w, h), [0.0, 0.0, 1.0, 1.0]);
    }

    /// A destination rectangle is that part of the client area wherever the window is; an
    /// empty one (or none) the whole client area.
    #[test]
    fn destination_rectangles_are_in_client_coordinates(cl in -3000i32..3000, ct in -3000i32..3000, cw in 1i32..4000, ch in 1i32..4000, d in proptest::option::of((-100i32..4000, -100i32..4000, -100i32..4000, -100i32..4000))) {
        let client = [cl, ct, cl + cw, ct + ch];
        let dst = d.map(|(l, t, r, b)| [l, t, r, b]);
        let got = present_dest(client, dst);
        match dst {
            Some([l, t, r, b]) if r > l && b > t => prop_assert_eq!(got, [cl + l, ct + t, cl + r, ct + b]),
            _ => prop_assert_eq!(got, client),
        }
    }
}
