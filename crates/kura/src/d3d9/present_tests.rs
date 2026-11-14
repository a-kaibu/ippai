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
    device_swap(w, h, windowed, 1)
}

/// `device` with swap effect `swap` (1 DISCARD, 2 FLIP, 3 COPY)
fn device_swap(w: u32, h: u32, windowed: u32, swap: u32) -> u32 {
    let o = out();
    unsafe { vnhost_d3d9_create(0x20, o as *mut u32, &CALLS) };
    let d3d = rd(o);
    let pp = unsafe { alloc(56) as u32 };
    for (i, x) in [w, h, 22, 1, 0, 0, swap, 1, windowed, 0, 0, 0, 0, 0].into_iter().enumerate() {
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
    // (into a part of the client area: the window's pixels with that part new, #235)
    assert_eq!(present(&page, dev, 0, rect([8, 4, 40, 36])), ([0, 0, 64, 64], [0.0, 0.0, 1.0, 1.0]), "into a part of the client area");
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

/// Clears the back buffer of `dev` to `color` (XRGB), all of it or box `r`.
fn clear(dev: u32, r: Option<[i32; 4]>, color: u32) {
    let (n, p) = match r {
        Some(r) => (1, rect(r)),
        None => (0, 0),
    };
    assert_eq!(call(IF_DEVICE, 43, &[dev, n, p, 1, 0xff00_0000 | color, 0, 0]), 0);
}

/// Present(src, dst) (`None`: NULL) of `dev`.
fn present_rects(dev: u32, src: Option<[i32; 4]>, dst: Option<[i32; 4]>) {
    assert_eq!(call(IF_DEVICE, 17, &[dev, src.map_or(0, rect), dst.map_or(0, rect), 0, 0]), 0);
}

/// GetFrontBufferData of `dev` into a 64x64 system memory A8R8G8B8 surface: the screen's
/// pixels (RGB) where the 64x64 client area is.
fn front(dev: u32) -> Vec<u32> {
    let o = out();
    assert_eq!(call(IF_DEVICE, 36, &[dev, 64, 64, 21, 2, o, 0]), 0);
    let sys = rd(o);
    assert_eq!(call(IF_DEVICE, 33, &[dev, 0, sys]), 0, "GetFrontBufferData");
    let lr = out();
    assert_eq!(call(IF_SURFACE, 13, &[sys, lr, 0, 0x10]), 0);
    let (pitch, bits) = (rd(lr), rd(lr + 4));
    let px = (0..64 * 64).map(|i| unsafe { *((bits + (i / 64) * pitch + (i % 64) * 4) as *const u32) } & 0xff_ffff).collect();
    call(IF_SURFACE, 14, &[sys]);
    call(IF_SURFACE, 2, &[sys]);
    px
}

/// The window as Windows shows it: `w` with box `r` (clipped to 64x64) set to `color`.
fn paint(w: &mut [u32], r: [i32; 4], color: u32) {
    for y in r[1].max(0)..r[3].min(64) {
        for x in r[0].max(0)..r[2].min(64) {
            w[(y * 64 + x) as usize] = color;
        }
    }
}

fn boxes(px: &[u32]) -> String {
    let mut runs: Vec<(u32, usize)> = Vec::new();
    for &p in px {
        match runs.last_mut() {
            Some((c, n)) if *c == p => *n += 1,
            _ => runs.push((p, 1)),
        }
    }
    format!("{} runs: {:x?}", runs.len(), &runs[..runs.len().min(12)])
}

/// 果つることなき未来ヨリ (CatSystem2) draws the whole frame and presents only the box that
/// changed, Present(&box, &box): Windows updates that part of the window and the rest keeps
/// what was there. The page showed that box alone, black around it (#235).
#[test]
fn a_present_into_part_of_the_window_keeps_the_rest() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let page = page();
    let dev = device(64, 64, 1);
    let mut want = vec![0u32; 64 * 64];
    // a whole frame
    clear(dev, None, 0xff0000);
    present_rects(dev, None, None);
    paint(&mut want, [0, 0, 64, 64], 0xff0000);
    assert_eq!(front(dev), want, "a whole frame");
    // the next frame: all of it drawn, a box presented (the first: over the back buffer as it
    // is, the game's whole frame)
    clear(dev, Some([8, 8, 24, 24]), 0x00ff00);
    present_rects(dev, Some([8, 8, 24, 24]), Some([8, 8, 24, 24]));
    paint(&mut want, [8, 8, 24, 24], 0x00ff00);
    assert_eq!(front(dev), want, "the box over the frame: {}", boxes(&front(dev)));
    let o = page.borrow().overlay.unwrap();
    assert_eq!((o.rect, o.src), ([0, 0, 64, 64], [0.0, 0.0, 1.0, 1.0]), "the screen shows the whole client area");
    // all of the back buffer blue, only a box of it presented: the rest stays as shown
    clear(dev, None, 0x0000ff);
    present_rects(dev, Some([40, 30, 64, 64]), Some([40, 30, 64, 64]));
    paint(&mut want, [40, 30, 64, 64], 0x0000ff);
    assert_eq!(front(dev), want, "the second box: {}", boxes(&front(dev)));
    // a box of the back buffer stretched into another place
    clear(dev, Some([0, 0, 4, 4]), 0xffff00);
    present_rects(dev, Some([0, 0, 4, 4]), Some([0, 40, 16, 56]));
    paint(&mut want, [0, 40, 16, 56], 0xffff00);
    assert_eq!(front(dev), want, "stretched: {}", boxes(&front(dev)));
    // (past the client area: what lands in it)
    clear(dev, None, 0x00ffff);
    present_rects(dev, Some([0, 0, 32, 32]), Some([48, 48, 80, 80]));
    paint(&mut want, [48, 48, 80, 80], 0x00ffff);
    assert_eq!(front(dev), want, "clipped: {}", boxes(&front(dev)));
    // a whole frame again: all of it, and the parts after it go over it
    clear(dev, None, 0x808080);
    present_rects(dev, None, None);
    paint(&mut want, [0, 0, 64, 64], 0x808080);
    assert_eq!(front(dev), want);
    clear(dev, None, 0x102030);
    present_rects(dev, Some([16, 0, 32, 8]), Some([16, 0, 32, 8]));
    paint(&mut want, [16, 0, 32, 8], 0x102030);
    assert_eq!(front(dev), want, "a box after a whole frame: {}", boxes(&front(dev)));
    // (a GetFrontBufferData with no box ever presented: the back buffer as it was presented)
    let dev2 = device(64, 64, 1);
    clear(dev2, None, 0x405060);
    present_rects(dev2, None, None);
    assert!(front(dev2).iter().all(|&p| p == 0x405060));
    gpu::set_pump(None);
}

/// What GetFrontBufferData asks of its surface (Windows: system memory A8R8G8B8).
#[test]
fn front_buffer_data_takes_a_system_memory_argb_surface() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let _page = page();
    let dev = device(64, 64, 1);
    for (fmt, pool, ok) in [(21, 2, true), (22, 2, false), (21, 0, false), (21, 3, false)] {
        let o = out();
        assert_eq!(call(IF_DEVICE, 36, &[dev, 64, 64, fmt, pool, o, 0]), 0);
        let s = rd(o);
        let r = call(IF_DEVICE, 33, &[dev, 0, s]);
        assert_eq!(r == 0, ok, "format {fmt} pool {pool}: {r:#x}");
        call(IF_SURFACE, 2, &[s]);
    }
    let o = out();
    assert_eq!(call(IF_DEVICE, 36, &[dev, 64, 64, 21, 2, o, 0]), 0);
    assert_ne!(call(IF_DEVICE, 33, &[dev, 1, rd(o)]), 0, "swap chain 1 of a device with one");
    gpu::set_pump(None);
}

/// D3DSWAPEFFECT_COPY: the window keeps what each Present showed, so a box presented after
/// whole frames goes over the last whole frame, not over the back buffer drawn since
/// (tests/winref/probes/d3d9part.c's "box").
#[test]
fn copy_swap_effect_keeps_the_last_whole_frame() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    unsafe {
        let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
        vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
    }
    let _page = page();
    let dev = device_swap(64, 64, 1, 3);
    clear(dev, None, 0xff0000);
    present_rects(dev, None, None);
    clear(dev, None, 0x00ff00);
    present_rects(dev, Some([8, 4, 40, 36]), Some([8, 4, 40, 36]));
    let mut want = vec![0xff0000; 64 * 64];
    paint(&mut want, [8, 4, 40, 36], 0x00ff00);
    assert_eq!(front(dev), want, "{}", boxes(&front(dev)));
    // (and GetFrontBufferData after whole frames only: the last one, not the back buffer since)
    let dev2 = device_swap(64, 64, 1, 3);
    clear(dev2, None, 0x405060);
    present_rects(dev2, None, None);
    clear(dev2, None, 0x000000);
    assert!(front(dev2).iter().all(|&p| p == 0x405060));
    gpu::set_pump(None);
}

/// A step of `presents_show_what_windows_shows`: a box of the back buffer cleared, then a
/// Present of a box (into the same box: the game's way) or of all of it.
#[derive(Clone, Debug)]
enum Step {
    Clear([i32; 4], u32),
    Part([i32; 4]),
    Whole,
}

fn boxed() -> impl Strategy<Value = [i32; 4]> {
    (0i32..64, 0i32..64, 1i32..64, 1i32..64).prop_map(|(x, y, w, h)| [x, y, (x + w).min(64), (y + h).min(64)])
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (boxed(), 0u32..0x100_0000).prop_map(|(r, c)| Step::Clear(r, c)),
        boxed().prop_map(Step::Part),
        Just(Step::Whole),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Any run of clears and presents of the whole back buffer or of boxes of it: the screen
    /// (GetFrontBufferData, and so what the page shows) is what Windows shows, the boxes
    /// presented over the frames before. (D3DSWAPEFFECT_DISCARD: the first box goes over the
    /// back buffer as it is, here, as the game does, the whole frame presented just before.)
    #[test]
    fn presents_show_what_windows_shows(c0 in 0u32..0x100_0000, copy in any::<bool>(), steps in proptest::collection::vec(step(), 1..8)) {
        let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            let vt: Vec<u32> = (1..=64).map(|i| 0x1000 * i).collect();
            vnhost_d3d9_init(vt.as_ptr(), vt.len() as u32);
        }
        let _page = page();
        let dev = device_swap(64, 64, 1, if copy { 3 } else { 1 });
        let (mut bb, mut win) = (vec![c0; 64 * 64], vec![c0; 64 * 64]);
        clear(dev, None, c0);
        present_rects(dev, None, None);
        if !copy {
            present_rects(dev, Some([0, 0, 1, 1]), Some([0, 0, 1, 1]));
        }
        for s in &steps {
            match *s {
                Step::Clear(r, c) => {
                    clear(dev, Some(r), c);
                    paint(&mut bb, r, c);
                }
                Step::Part(r) => {
                    present_rects(dev, Some(r), Some(r));
                    for y in r[1]..r[3] {
                        for x in r[0]..r[2] {
                            win[(y * 64 + x) as usize] = bb[(y * 64 + x) as usize];
                        }
                    }
                }
                Step::Whole => {
                    present_rects(dev, None, None);
                    win.copy_from_slice(&bb);
                }
            }
            let got = front(dev);
            prop_assert!(got == win, "after {:?}: {} want {}", s, boxes(&got), boxes(&win));
        }
        gpu::set_pump(None);
    }
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
