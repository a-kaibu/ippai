//! Pictures the page decodes (web/src/idec.ts): JPEG files for sake's WIC, GDI+, D3DX and OLE
//! (crates/sake/src/picture.rs). The browser's decoder (ImageDecoder: libjpeg-turbo's islow
//! IDCT in native SIMD code) hands over the Y, Cb and Cr planes; libjpeg's fancy upsampling and
//! YCbCr -> RGB (`Picture::bgr24`, here in wasm SIMD128) then give Windows' own bytes
//! (tests/winref imaging jpeg_*, where jpeg-decoder is up to 2 off) in a fraction of the wasm
//! decoder's time. (The browser's own RGB, a canvas read back, is exact too, but slower than
//! decoding in wasm.)
//!
//! The guest posts a file and waits (`jpeg`); the page lists the files waiting when the doorbell
//! rings (`vnhost_idec_list`), decodes each, asks for room for its planes in the layout they
//! came in (`vnhost_idec_begin`, `Layout`), writes them and says it is done
//! (`vnhost_idec_done`). Without a page (node), when the page cannot decode a file or does not
//! answer in time: None, and the caller decodes it itself. A page that once did not answer in
//! time is not asked again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Req {
    /// the file (stays put while listed: the page reads it right after listing)
    data: Vec<u8>,
    listed: bool,
    /// the page's room for the planes, from `vnhost_idec_begin` on
    px: Vec<u8>,
    size: (u32, u32),
    layout: Option<Layout>,
    /// written by the page: Some(true) done, Some(false) failed
    done: Option<bool>,
    /// the guest stopped waiting while the page writes: dropped by `vnhost_idec_done`
    abandoned: bool,
}

static REQS: Mutex<Option<HashMap<u32, Req>>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();
/// The guest adds 1 and wakes the page (Atomics.waitAsync) when a file waits.
static DOORBELL: AtomicI32 = AtomicI32::new(0);
/// A page has listed (after checking its decoder gives Windows' bytes: web/src/idec.ts).
static PAGE: AtomicBool = AtomicBool::new(false);
/// The page once did not answer in time.
static SLOW: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicU32 = AtomicU32::new(1);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Req>) -> R) -> R {
    let mut g = REQS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

fn ring() {
    DOORBELL.fetch_add(1, Ordering::SeqCst);
    #[cfg(target_os = "emscripten")]
    unsafe {
        extern "C" {
            fn emscripten_futex_wake(addr: *const std::ffi::c_void, count: i32) -> i32;
        }
        emscripten_futex_wake(DOORBELL.as_ptr() as *const std::ffi::c_void, i32::MAX);
    }
}

/// Whether a page decodes (MASU_IDEC_OFF: never).
pub fn available() -> bool {
    PAGE.load(Ordering::Relaxed) && !SLOW.load(Ordering::Relaxed) && !crate::env_on!("MASU_IDEC_OFF")
}

/// Pictures smaller than this are decoded by the guest (the page's round trip would cost more);
/// MASU_IDEC_ALL=1 sends every one (tests/winref's small probe pictures, in the browser).
pub const PIXELS_MIN: u64 = 128 * 128;
static ALL: AtomicBool = AtomicBool::new(false);

/// Whether the page is to decode a picture of `pixels` pixels.
pub fn wants(pixels: u64) -> bool {
    available() && (pixels >= PIXELS_MIN || ALL.load(Ordering::Relaxed) || crate::env_on!("MASU_IDEC_ALL"))
}

/// How long the guest waits for a picture (a 4K JPEG takes the page ~30 ms on a phone).
pub const TIMEOUT: Duration = Duration::from_secs(2);

/// The page's decoding of JPEG file `data`; None without a page, or when it failed or was late.
pub fn jpeg(data: &[u8], timeout: Duration) -> Option<Picture> {
    if !available() {
        return None;
    }
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    with(|m| m.insert(id, Req { data: data.to_vec(), ..Req::default() }));
    ring();
    let end = Instant::now() + timeout;
    let mut g = REQS.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let m = g.get_or_insert_with(HashMap::new);
        let r = m.get_mut(&id)?;
        match r.done {
            Some(true) => {
                let r = m.remove(&id)?;
                let layout = r.layout?;
                return (r.px.len() == layout.size(r.size.0, r.size.1)).then_some(Picture { w: r.size.0, h: r.size.1, layout, bytes: r.px });
            }
            Some(false) => {
                m.remove(&id);
                return None;
            }
            None => {}
        }
        let now = Instant::now();
        if now >= end {
            SLOW.store(true, Ordering::Relaxed);
            eprintln!("kura: idec: the page did not decode a picture in {timeout:?}: decoding pictures here from now on");
            // (a listed file the page may still be reading, or writing the pixels of: the page
            // drops it when done)
            if !r.listed {
                m.remove(&id);
            } else {
                r.abandoned = true;
            }
            return None;
        }
        // (in slices: a page woken late still gets its doorbell)
        g = WAKE.wait_timeout(g, (end - now).min(Duration::from_millis(5))).unwrap_or_else(|e| e.into_inner()).0;
    }
}

// ---------------------------------------------------------------------------------------------
// the planes

/// How the page lays out a picture's bytes (`vnhost_idec_begin`'s number; web/src/idec.ts
/// copies the browser's VideoFrame so): Y (w x h), then Cb and Cr planes (`chroma`), each row
/// right after the one before; or one plane of 4-byte pixels (gray files: the browser's BGRX).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum Layout {
    I420 = 1,
    I422 = 2,
    I444 = 3,
    Bgrx = 4,
    Rgbx = 5,
}

impl Layout {
    pub fn from_u32(n: u32) -> Option<Layout> {
        Some(match n {
            1 => Layout::I420,
            2 => Layout::I422,
            3 => Layout::I444,
            4 => Layout::Bgrx,
            5 => Layout::Rgbx,
            _ => return None,
        })
    }

    /// The width and height of a chroma plane (0 x 0 for 4-byte pixels).
    pub fn chroma(self, w: u32, h: u32) -> (usize, usize) {
        let (w, h) = (w as usize, h as usize);
        match self {
            Layout::I420 => (w.div_ceil(2), h.div_ceil(2)),
            Layout::I422 => (w.div_ceil(2), h),
            Layout::I444 => (w, h),
            Layout::Bgrx | Layout::Rgbx => (0, 0),
        }
    }

    /// The bytes of a `w` x `h` picture.
    pub fn size(self, w: u32, h: u32) -> usize {
        let (cw, ch) = self.chroma(w, h);
        match self {
            Layout::Bgrx | Layout::Rgbx => w as usize * h as usize * 4,
            _ => w as usize * h as usize + 2 * cw * ch,
        }
    }
}

/// A picture as the page decoded it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Picture {
    pub w: u32,
    pub h: u32,
    pub layout: Layout,
    /// `layout.size(w, h)` bytes
    pub bytes: Vec<u8>,
}

/// libjpeg's jdcolor.c tables as products (FIX: 16-bit fixed point, rounded).
const fn fix(x: f64) -> i32 {
    (x * 65536.0 + 0.5) as i32
}
const CR_R: i32 = fix(1.40200);
const CB_B: i32 = fix(1.77200);
const CR_G: i32 = fix(0.71414);
const CB_G: i32 = fix(0.34414);
const HALF: i32 = 1 << 15;

/// One pixel as libjpeg's ycc_rgb_convert: B, G, R.
#[inline(always)]
fn ycc_bgr(y: u8, u: u8, v: u8) -> [u8; 3] {
    let (y, u, v) = (y as i32, u as i32 - 128, v as i32 - 128);
    let c = |x: i32| x.clamp(0, 255) as u8;
    [c(y + ((CB_B * u + HALF) >> 16)), c(y + ((-CB_G * u - CR_G * v + HALF) >> 16)), c(y + ((CR_R * v + HALF) >> 16))]
}

/// Chroma row `y` of plane `c` (`cw` x `ch`) at full width as libjpeg upsamples it: fancy
/// (triangle filter: h2v1_fancy_upsample, h2v2_fancy_upsample, the rows above the top and
/// below the bottom their edge rows) for planes wider than 2, else each sample repeated
/// (jdsample.c: fancy only `if downsampled_width > 2`). `cs`: room for cw + 2 sums; `out`: for
/// `out_len(w, cw)` samples (the first w are the row's).
fn upsample(layout: Layout, c: &[u8], cw: usize, ch: usize, y: usize, cs: &mut [u16], out: &mut [u8]) {
    let row = |r: usize| &c[r * cw..r * cw + cw];
    match layout {
        Layout::I444 => out[..cw].copy_from_slice(row(y)),
        _ if cw <= 2 => {
            let r = row(if layout == Layout::I420 { y / 2 } else { y });
            for (x, o) in out[..2 * cw].iter_mut().enumerate() {
                *o = r[x / 2];
            }
        }
        _ => {
            // (the sums padded with their edge ones: cs[i + 1] is sample i's)
            if layout == Layout::I422 {
                for (s, &a) in cs[1..cw + 1].iter_mut().zip(row(y)) {
                    *s = a as u16;
                }
            } else {
                let r = y / 2;
                let nb = if y % 2 == 0 { r.saturating_sub(1) } else { (r + 1).min(ch - 1) };
                for ((s, &a), &b) in cs[1..cw + 1].iter_mut().zip(row(r)).zip(row(nb)) {
                    *s = a as u16 * 3 + b as u16;
                }
            }
            cs[0] = cs[1];
            cs[cw + 1] = cs[cw];
            let (e, o, sh) = if layout == Layout::I422 { (1, 2, 2) } else { (8, 7, 4) };
            fancy(cs, cw, e, o, sh, out);
        }
    }
}

/// Samples of a row upsampled from `cw` (pairs for 2 x 1 and 2 x 2 planes).
fn out_len(w: usize, cw: usize) -> usize {
    w.max(2 * cw)
}

/// fancy_scalar's out[..2 * cw] from cs[..cw + 2] (SIMD where it can).
#[doc(hidden)]
pub fn fancy(cs: &[u16], cw: usize, e: u16, o: u16, sh: u32, out: &mut [u8]) {
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    let done = unsafe { simd::fancy(cs, cw, e, o, sh, out) };
    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    let done = 0;
    fancy_scalar(&cs[done..cw + 2], e, o, sh, &mut out[2 * done..2 * cw]);
}

/// out[2i], out[2i + 1] from sums cs[i] (left), cs[i + 1] (the sample's), cs[i + 2] (right):
/// (3 x own + left + e) >> sh, (3 x own + right + o) >> sh.
#[doc(hidden)]
pub fn fancy_scalar(cs: &[u16], e: u16, o: u16, sh: u32, out: &mut [u8]) {
    for (i, p) in out.chunks_exact_mut(2).enumerate() {
        let a = cs[i + 1] * 3;
        p[0] = ((a + cs[i] + e) >> sh) as u8;
        p[1] = ((a + cs[i + 2] + o) >> sh) as u8;
    }
}

/// One row: Y, upsampled Cb and Cr -> B, G, R bytes.
pub fn ycc_row(yr: &[u8], ur: &[u8], vr: &[u8], out: &mut [u8]) {
    let n = yr.len();
    assert!(ur.len() >= n && vr.len() >= n && out.len() >= n * 3);
    #[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
    let done = unsafe { simd::ycc_row(yr, ur, vr, out) };
    #[cfg(not(all(target_arch = "wasm32", target_feature = "simd128")))]
    let done = 0;
    ycc_row_scalar(&yr[done..], &ur[done..], &vr[done..], &mut out[done * 3..]);
}

#[doc(hidden)]
pub fn ycc_row_scalar(yr: &[u8], ur: &[u8], vr: &[u8], out: &mut [u8]) {
    for (x, &y) in yr.iter().enumerate() {
        out[x * 3..x * 3 + 3].copy_from_slice(&ycc_bgr(y, ur[x], vr[x]));
    }
}

impl Picture {
    /// The picture as libjpeg (and Windows) decodes it to BGR, rows `w * 3` bytes apart; None
    /// when its bytes do not fit its layout.
    pub fn bgr24(&self) -> Option<Vec<u8>> {
        self.bgr24_with(ycc_row)
    }

    #[doc(hidden)]
    pub fn bgr24_with(&self, row: fn(&[u8], &[u8], &[u8], &mut [u8])) -> Option<Vec<u8>> {
        let (w, h) = (self.w as usize, self.h as usize);
        if self.bytes.len() != self.layout.size(self.w, self.h) {
            return None;
        }
        let mut out = zeroed(w * h * 3)?;
        match self.layout {
            Layout::Bgrx | Layout::Rgbx => {
                let (b, r) = if self.layout == Layout::Bgrx { (0, 2) } else { (2, 0) };
                for (o, p) in out.chunks_exact_mut(3).zip(self.bytes.chunks_exact(4)) {
                    o.copy_from_slice(&[p[b], p[1], p[r]]);
                }
            }
            _ => {
                let (cw, ch) = self.layout.chroma(self.w, self.h);
                let (yp, rest) = self.bytes.split_at(w * h);
                let (up, vp) = rest.split_at(cw * ch);
                let (mut ur, mut vr) = (vec![0; out_len(w, cw)], vec![0; out_len(w, cw)]);
                let mut cs = vec![0; cw + 2];
                for y in 0..h {
                    upsample(self.layout, up, cw, ch, y, &mut cs, &mut ur);
                    upsample(self.layout, vp, cw, ch, y, &mut cs, &mut vr);
                    row(&yp[y * w..y * w + w], &ur[..w], &vr[..w], &mut out[y * w * 3..y * w * 3 + w * 3]);
                }
            }
        }
        Some(out)
    }

    /// A gray file's samples: its Y plane, or the B of its 4-byte pixels.
    pub fn gray(&self) -> Option<Vec<u8>> {
        let n = self.w as usize * self.h as usize;
        if self.bytes.len() != self.layout.size(self.w, self.h) {
            return None;
        }
        Some(match self.layout {
            Layout::Bgrx | Layout::Rgbx => self.bytes.chunks_exact(4).map(|p| p[0]).collect(),
            _ => self.bytes[..n].to_vec(),
        })
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod simd {
    //! ycc_row 16 pixels at a time: libjpeg's sums exactly (one product a term, as its tables
    //! hold them, in i32 lanes), clamped by saturating narrowing.
    use super::{CB_B, CB_G, CR_G, CR_R, HALF};
    use core::arch::wasm32::*;

    /// fancy_scalar for i in 0..8 * k (as many as fit in cw): 8 sums a step, out 16 bytes; how
    /// many i done.
    pub unsafe fn fancy(cs: &[u16], cw: usize, e: u16, o: u16, sh: u32, out: &mut [u8]) -> usize {
        assert!(cs.len() >= cw + 2 && out.len() >= 2 * cw);
        let (p, q) = (cs.as_ptr(), out.as_mut_ptr());
        let (e, o) = (u16x8_splat(e), u16x8_splat(o));
        let mut i = 0;
        while i + 8 <= cw {
            let (l, a, n) = (v128_load(p.add(i) as *const v128), v128_load(p.add(i + 1) as *const v128), v128_load(p.add(i + 2) as *const v128));
            let a3 = u16x8_add(u16x8_add(a, a), a);
            let ev = u16x8_shr(u16x8_add(u16x8_add(a3, l), e), sh);
            let od = u16x8_shr(u16x8_add(u16x8_add(a3, n), o), sh);
            // (the sums are 10 bits at most: <= 255 after the shift)
            let v = u8x16_shuffle::<0, 16, 2, 18, 4, 20, 6, 22, 8, 24, 10, 26, 12, 28, 14, 30>(ev, od);
            v128_store(q.add(2 * i) as *mut v128, v);
            i += 8;
        }
        i
    }

    /// Pixels 0..16 * k of the row (k as many as fit): how many written.
    pub unsafe fn ycc_row(yr: &[u8], ur: &[u8], vr: &[u8], out: &mut [u8]) -> usize {
        let n = yr.len() / 16 * 16;
        let (yp, up, vp, op) = (yr.as_ptr(), ur.as_ptr(), vr.as_ptr(), out.as_mut_ptr());
        let mut x = 0;
        while x < n {
            let (y, u, v) = (v128_load(yp.add(x) as *const v128), v128_load(up.add(x) as *const v128), v128_load(vp.add(x) as *const v128));
            let quarter = |b: v128, q: usize| {
                let h = if q < 2 { u16x8_extend_low_u8x16(b) } else { u16x8_extend_high_u8x16(b) };
                if q % 2 == 0 { u32x4_extend_low_u16x8(h) } else { u32x4_extend_high_u16x8(h) }
            };
            let mut bs = [i32x4_splat(0); 4];
            let mut gs = bs;
            let mut rs = bs;
            for q in 0..4 {
                let yq = quarter(y, q);
                let uq = i32x4_sub(quarter(u, q), i32x4_splat(128));
                let vq = i32x4_sub(quarter(v, q), i32x4_splat(128));
                let half = i32x4_splat(HALF);
                bs[q] = i32x4_add(yq, i32x4_shr(i32x4_add(i32x4_mul(uq, i32x4_splat(CB_B)), half), 16));
                gs[q] = i32x4_add(yq, i32x4_shr(i32x4_sub(half, i32x4_add(i32x4_mul(uq, i32x4_splat(CB_G)), i32x4_mul(vq, i32x4_splat(CR_G)))), 16));
                rs[q] = i32x4_add(yq, i32x4_shr(i32x4_add(i32x4_mul(vq, i32x4_splat(CR_R)), half), 16));
            }
            let pack = |a: [v128; 4]| u8x16_narrow_i16x8(i16x8_narrow_i32x4(a[0], a[1]), i16x8_narrow_i32x4(a[2], a[3]));
            let (b, g, r) = (pack(bs), pack(gs), pack(rs));
            // B G R interleaved: B, G pairs first, then R into every third byte
            let lo = u8x16_shuffle::<0, 16, 1, 17, 2, 18, 3, 19, 4, 20, 5, 21, 6, 22, 7, 23>(b, g);
            let hi = u8x16_shuffle::<8, 24, 9, 25, 10, 26, 11, 27, 12, 28, 13, 29, 14, 30, 15, 31>(b, g);
            let mid = u8x16_shuffle::<8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23>(lo, hi);
            let o0 = u8x16_shuffle::<0, 1, 16, 2, 3, 17, 4, 5, 18, 6, 7, 19, 8, 9, 20, 10>(lo, r);
            let o1 = u8x16_shuffle::<3, 21, 4, 5, 22, 6, 7, 23, 8, 9, 24, 10, 11, 25, 12, 13>(mid, r);
            let o2 = u8x16_shuffle::<26, 6, 7, 27, 8, 9, 28, 10, 11, 29, 12, 13, 30, 14, 15, 31>(hi, r);
            let o = op.add(x * 3);
            v128_store(o as *mut v128, o0);
            v128_store(o.add(16) as *mut v128, o1);
            v128_store(o.add(32) as *mut v128, o2);
            x += 16;
        }
        n
    }
}

// ---------------------------------------------------------------------------------------------
// the page

/// The doorbell's address (the page waits on it with Atomics.waitAsync).
#[no_mangle]
pub extern "C" fn vnhost_idec_doorbell() -> u32 {
    DOORBELL.as_ptr() as u32
}

/// Lists up to `max` files not listed before into `out`, 3 words each: id, address, bytes. The
/// first call also says there is a page.
///
/// # Safety
/// `out` must hold `max * 3` words.
#[no_mangle]
pub unsafe extern "C" fn vnhost_idec_list(out: *mut u32, max: u32) -> u32 {
    let out = std::slice::from_raw_parts_mut(out, max as usize * 3);
    let rows = list(max as usize);
    for (n, r) in rows.iter().enumerate() {
        out[n * 3..n * 3 + 3].copy_from_slice(&[r.0, r.1 as u32, r.2 as u32]);
    }
    rows.len() as u32
}

fn list(max: usize) -> Vec<(u32, usize, usize)> {
    PAGE.store(true, Ordering::Relaxed);
    with(|m| {
        m.iter_mut()
            .filter(|(_, r)| !r.listed)
            .take(max)
            .map(|(&id, r)| {
                r.listed = true;
                (id, r.data.as_ptr() as usize, r.data.len())
            })
            .collect()
    })
}

/// Room for the planes of file `id`, `w` x `h` pixels in `layout` (`Layout` as a number; stays
/// put until `vnhost_idec_done`); 0 when the guest no longer waits for it, or the layout is not
/// one of ours or the size is out of reach.
#[no_mangle]
pub extern "C" fn vnhost_idec_begin(id: u32, w: u32, h: u32, layout: u32) -> u32 {
    begin(id, w, h, layout).unwrap_or(0) as u32
}

fn begin(id: u32, w: u32, h: u32, layout: u32) -> Option<usize> {
    let layout = Layout::from_u32(layout)?;
    if w == 0 || h == 0 || w as u64 * h as u64 > 1 << 28 {
        return None;
    }
    let n = layout.size(w, h);
    with(|m| match m.get_mut(&id) {
        Some(r) if r.done.is_none() && r.px.is_empty() && !r.abandoned => {
            // (a picture the heap has no room for fails on its own: an abort would end the VM)
            r.px = zeroed(n)?;
            r.size = (w, h);
            r.layout = Some(layout);
            Some(r.px.as_ptr() as usize)
        }
        _ => None,
    })
}

/// `n` zero bytes, None when the heap cannot give them.
fn zeroed(n: usize) -> Option<Vec<u8>> {
    #[cfg(test)]
    if n > tests::HEAP.load(Ordering::Relaxed) {
        return None;
    }
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, 0);
    Some(v)
}

/// The page is done with file `id`: its pixels written (`ok`), or it could not decode it.
#[no_mangle]
pub extern "C" fn vnhost_idec_done(id: u32, ok: u32) {
    with(|m| {
        if m.get(&id).is_some_and(|r| r.abandoned) {
            m.remove(&id);
        } else if let Some(r) = m.get_mut(&id) {
            r.done = Some(ok != 0 && !r.px.is_empty());
        }
    });
    WAKE.notify_all();
}

/// Tests: says there is a page (or not) for pictures of any size, and forgets that one was late.
#[doc(hidden)]
pub fn test_page(on: bool) {
    PAGE.store(on, Ordering::Relaxed);
    ALL.store(on, Ordering::Relaxed);
    SLOW.store(false, Ordering::Relaxed);
}

/// Tests: a page pass in this process (`dec`: the page's decoding of a file, None for a
/// failure), as web/src/idec.ts does with ImageDecoder.
#[doc(hidden)]
pub fn test_page_pass(dec: &mut dyn FnMut(&[u8]) -> Option<Picture>) {
    for (id, p, n) in list(16) {
        let data = unsafe { std::slice::from_raw_parts(p as *const u8, n) }.to_vec();
        match dec(&data) {
            Some(pic) => {
                let at = begin(id, pic.w, pic.h, pic.layout as u32);
                if let Some(at) = at {
                    unsafe { std::ptr::copy_nonoverlapping(pic.bytes.as_ptr(), at as *mut u8, pic.bytes.len().min(pic.layout.size(pic.w, pic.h))) };
                }
                vnhost_idec_done(id, at.is_some() as u32);
            }
            None => vnhost_idec_done(id, 0),
        }
    }
}

/// Tests: the cases of testdata/idec/planes.bin (make.sh): name, the browser's planes of the
/// file, and libjpeg-turbo's (Windows' for tests/winref's jpeg_*) RGB of it.
#[doc(hidden)]
pub fn test_cases(bin: &[u8]) -> Vec<(String, Picture, Vec<u8>)> {
    let mut out = Vec::new();
    let mut i = 0;
    let word = |i: &mut usize| {
        *i += 4;
        u32::from_le_bytes(bin[*i - 4..*i].try_into().unwrap())
    };
    while i < bin.len() {
        let n = word(&mut i) as usize;
        let name = String::from_utf8(bin[i..i + n].to_vec()).unwrap();
        i += n;
        let (w, h, layout, n) = (word(&mut i), word(&mut i), Layout::from_u32(word(&mut i)).unwrap(), word(&mut i) as usize);
        let bytes = bin[i..i + n].to_vec();
        i += n;
        let rgb = bin[i..i + w as usize * h as usize * 3].to_vec();
        i += rgb.len();
        out.push((name, Picture { w, h, layout, bytes }, rgb));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Tests: the most one allocation of `zeroed` gets (a heap that is full past it)
    pub(super) static HEAP: AtomicUsize = AtomicUsize::new(usize::MAX);
    /// (tests that share the page and HEAP)
    static ONE: Mutex<()> = Mutex::new(());

    /// A picture bigger than the heap can hold fails on its own (the guest falls back to its own
    /// decoder), and the next one that fits decodes: a full heap no longer aborts the VM.
    #[test]
    fn a_picture_past_the_heap_fails_alone() {
        let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
        test_page(true);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let page = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                test_page_pass(&mut |d| match d[0] {
                    // (16384 x 16384 I420: 384 MiB the heap does not have; never written)
                    b'b' => Some(Picture { w: 16384, h: 16384, layout: Layout::I420, bytes: Vec::new() }),
                    _ => Some(Picture { w: 1, h: 1, layout: Layout::Bgrx, bytes: vec![1, 2, 3, 255] }),
                });
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        HEAP.store(1 << 20, Ordering::Relaxed);
        let t = std::time::Instant::now();
        assert_eq!(jpeg(b"big!", TIMEOUT), None);
        assert!(t.elapsed() < TIMEOUT, "answered, not timed out");
        assert!(available(), "one picture failing keeps the page");
        assert_eq!(jpeg(b"tiny", TIMEOUT).map(|p| p.bytes), Some(vec![1, 2, 3, 255]));
        let p = Picture { w: 1024, h: 1024, layout: Layout::Bgrx, bytes: vec![0; 4 << 20] };
        assert_eq!(p.bgr24(), None, "3 MiB out of a 1 MiB heap");
        HEAP.store(usize::MAX, Ordering::Relaxed);
        assert_eq!(p.bgr24().map(|v| v.len()), Some(3 << 20));
        assert!(with(|m| m.is_empty()));
        stop.store(true, Ordering::Relaxed);
        page.join().unwrap();
    }

    /// A guest thread waits while a page thread answers: done, failed, and late (the guest is not
    /// asked again after that).
    #[test]
    fn guest_and_page() {
        let _g = ONE.lock().unwrap_or_else(|e| e.into_inner());
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        PAGE.store(true, Ordering::Relaxed);
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let s2 = stop.clone();
        let page = std::thread::spawn(move || {
            while !s2.load(Ordering::Relaxed) {
                test_page_pass(&mut |d| {
                    CALLS.fetch_add(1, Ordering::Relaxed);
                    match d[0] {
                        b'f' => None,
                        b's' => {
                            std::thread::sleep(Duration::from_millis(300));
                            Some(Picture { w: 1, h: 1, layout: Layout::Bgrx, bytes: vec![9, 9, 9, 255] })
                        }
                        _ => Some(Picture { w: 2, h: 1, layout: Layout::Bgrx, bytes: d.iter().cycle().take(8).copied().collect() }),
                    }
                });
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        assert_eq!(jpeg(b"abcd", TIMEOUT), Some(Picture { w: 2, h: 1, layout: Layout::Bgrx, bytes: b"abcdabcd".to_vec() }));
        assert_eq!(jpeg(b"fail", TIMEOUT), None);
        assert!(available());
        assert_eq!(jpeg(b"slow", Duration::from_millis(50)), None);
        assert!(!available());
        let n = CALLS.load(Ordering::Relaxed);
        assert_eq!(jpeg(b"abcd", TIMEOUT), None);
        std::thread::sleep(Duration::from_millis(400));
        // (the late one was dropped once the page was done with it; nothing new asked)
        assert_eq!(CALLS.load(Ordering::Relaxed), n);
        assert!(with(|m| m.is_empty()));
        stop.store(true, Ordering::Relaxed);
        page.join().unwrap();
    }

    /// The browser's planes of JPEG files (Chrome's ImageDecoder) -> libjpeg-turbo's RGB of them,
    /// and Windows' for tests/winref's: 4:2:0, 4:2:2, 4:4:4, progressive, restart markers, gray,
    /// and sizes from 1 x 1 up, odd and even (planes 1 and 2 wide: libjpeg's plain upsampling).
    #[test]
    fn planes_as_libjpeg() {
        let cases = test_cases(include_bytes!("../testdata/idec/planes.bin"));
        assert!(cases.len() >= 39);
        for (name, p, rgb) in &cases {
            let bgr: Vec<u8> = rgb.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0]]).collect();
            if name.contains("gray") {
                assert_eq!(p.gray().unwrap(), rgb.chunks_exact(3).map(|p| p[0]).collect::<Vec<_>>(), "{name}");
            }
            assert_eq!(p.bgr24().unwrap(), bgr, "{name}");
        }
        assert!(cases.iter().any(|c| c.1.layout == Layout::Bgrx));
    }

    /// Bytes that do not fit the layout: None, no panic.
    #[test]
    fn planes_cut_short() {
        for l in [Layout::I420, Layout::I422, Layout::I444, Layout::Bgrx, Layout::Rgbx] {
            for (w, h) in [(1, 1), (3, 5), (8, 2)] {
                let p = Picture { w, h, layout: l, bytes: vec![7; l.size(w, h) - 1] };
                assert_eq!((p.bgr24(), p.gray()), (None, None));
                let p = Picture { bytes: vec![7; l.size(w, h)], ..p };
                assert_eq!(p.bgr24().unwrap().len(), w as usize * h as usize * 3);
            }
        }
        let px = |layout| Picture { w: 2, h: 1, layout, bytes: vec![1, 2, 3, 0, 4, 5, 6, 0] };
        assert_eq!(px(Layout::Bgrx).bgr24().unwrap(), [1, 2, 3, 4, 5, 6]);
        assert_eq!(px(Layout::Rgbx).bgr24().unwrap(), [3, 2, 1, 6, 5, 4]);
        assert_eq!(begin(1, 4, 4, 0), None);
        assert_eq!(begin(1, 4, 4, 6), None);
    }

    proptest::proptest! {
        /// Constant chroma upsamples to itself (the filters' weights sum to one, with the
        /// rounding biases below one), and a gray picture (Cb = Cr = 128) comes out as its Y.
        #[test]
        fn upsampling_keeps_flat_chroma(w in 1u32..40, h in 1u32..40, l in 1u32..=3, c in proptest::prelude::any::<u8>(), y in proptest::collection::vec(proptest::prelude::any::<u8>(), 1600)) {
            let l = Layout::from_u32(l).unwrap();
            let (cw, ch) = l.chroma(w, h);
            let (n, ww) = (w as usize * h as usize, w as usize);
            let mut out = vec![0; out_len(ww, cw)];
            let mut cs = vec![0; cw + 2];
            for row in 0..h as usize {
                upsample(l, &vec![c; cw * ch], cw, ch, row, &mut cs, &mut out);
                proptest::prop_assert!(out[..ww].iter().all(|&o| o == c));
            }
            let mut bytes = y[..n].to_vec();
            bytes.resize(l.size(w, h), 128);
            let p = Picture { w, h, layout: l, bytes };
            let bgr = p.bgr24().unwrap();
            proptest::prop_assert!(bgr.chunks_exact(3).zip(&y[..n]).all(|(p, &y)| p == [y, y, y]));
        }
    }
}
