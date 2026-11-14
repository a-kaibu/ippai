//! IStream over an HGLOBAL (CreateStreamOnHGlobal) and IPicture (OleLoadPicture: JPEG and
//! BMP decoded in Rust into a 32-bit bitmap). COM objects are guest blocks whose first word
//! is a vtable of host_fn traps.

#![allow(non_snake_case)]

use std::sync::OnceLock;

use crate::api::{implement, Ctx};
use crate::mem::{self, put32, r32, w32};

const E_NOTIMPL: u32 = 0x8000_4001;
const E_FAIL: u32 = 0x8000_4005;
const E_OUTOFMEMORY: u32 = 0x8007_000e;

use crate::api::vtable;

fn gcall(c: &mut Ctx, dll: &str, name: &str, args: &[u32]) -> u32 {
    let f = crate::api::export(dll, name);
    c.call(f, args)
}

fn qi(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(2));
    w32(this + 4 * 7, r32(this + 4 * 7) + 1);
    put32(out, this);
    0
}
fn add_ref(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = r32(this + 4 * 7) + 1;
    w32(this + 4 * 7, n);
    n
}
fn notimpl(_: &mut Ctx) -> u32 {
    E_NOTIMPL
}
fn ok(_: &mut Ctx) -> u32 {
    0
}
/// Lock/UnlockRegion of an HGLOBAL stream: STG_E_INVALIDFUNCTION, as Windows
fn no_locks(_: &mut Ctx) -> u32 {
    0x8003_0001
}

// ---------------------------------------------------------------------------------------------
// IStream: {vtbl, memory, -, pos, -, -, -, refs}; its memory, shared with its clones (as
// Windows' HGLOBAL streams share it: a write through one is seen by the others, the HGLOBAL
// goes with the last of them): {hglobal, size, delete on release, refs}

const STG_E_MEDIUMFULL: u32 = 0x8003_0070;

fn hg(this: u32) -> u32 {
    r32(r32(this + 4))
}
fn size_of(this: u32) -> u32 {
    r32(r32(this + 4) + 4)
}
fn set_size(this: u32, n: u32) {
    w32(r32(this + 4) + 4, n)
}

fn s_release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = r32(this + 28).saturating_sub(1);
    w32(this + 28, n);
    if n == 0 {
        let m = r32(this + 4);
        let left = r32(m + 12).saturating_sub(1);
        w32(m + 12, left);
        if left == 0 {
            if r32(m + 8) != 0 && r32(m) != 0 {
                crate::k32::global_free(r32(m));
            }
            mem::heap_free(m);
        }
        mem::heap_free(this);
    }
    n
}
fn s_read(c: &mut Ctx) -> u32 {
    let (this, pv, cb, got) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (h, size, pos) = (hg(this), size_of(this), r32(this + 12));
    let n = cb.min(size.saturating_sub(pos));
    if n > 0 {
        mem::copy(pv, crate::k32::global_ptr(h) + pos, n);
    }
    w32(this + 12, pos + n);
    put32(got, n);
    // (S_OK also for a short read, as Windows' HGLOBAL stream)
    0
}
fn s_write(c: &mut Ctx) -> u32 {
    let (this, pv, cb, done) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (size, pos) = (size_of(this), r32(this + 12));
    // (past 4 GiB: a position near the end and a large count wrapped to a small end, and the
    // copy went past the HGLOBAL)
    let Some(end) = pos.checked_add(cb) else { return STG_E_MEDIUMFULL };
    if end > size {
        // (room for twice as much: writes in small pieces stay linear)
        let h = hg(this);
        if (crate::k32::global_ptr(h) == 0 || end > crate::k32::global_size(h)) && !grow(this, end.saturating_mul(2).max(64)) && !grow(this, end) {
            return E_OUTOFMEMORY;
        }
        set_size(this, end);
    }
    mem::copy(crate::k32::global_ptr(hg(this)) + pos, pv, cb);
    w32(this + 12, end);
    put32(done, cb);
    0
}
fn s_seek(c: &mut Ctx) -> u32 {
    // (this, LARGE_INTEGER move, origin, ULARGE_INTEGER* new): the 8-byte argument is one on x64
    let (this, mv, origin, out) =
        if mem::x64() { (c.a(0), c.a64(1), c.a(2), c.a(3)) } else { (c.a(0), (c.a(2) as u64) << 32 | c.a(1) as u64, c.a(3), c.a(4)) };
    let base = match origin {
        0 => 0,
        1 => r32(this + 12) as i64,
        2 => size_of(this) as i64,
        _ => -1,
    };
    let p = base + mv as i64;
    // a bad origin or a position before the start: STG_E_SEEKERROR, the position stays and
    // is what *new gets
    let (p, hr) = if base < 0 || p < 0 || p > u32::MAX as i64 { (r32(this + 12), 0x8003_0019) } else { (p as u32, 0) };
    w32(this + 12, p);
    if out != 0 {
        w32(out, p);
        w32(out + 4, 0);
    }
    hr
}
fn s_set_size(c: &mut Ctx) -> u32 {
    let (this, n) = (c.a(0), c.a(1));
    if !grow(this, n) {
        return E_OUTOFMEMORY;
    }
    set_size(this, n);
    0
}
/// Makes the stream's HGLOBAL hold at least `n` bytes (GlobalReAlloc, GMEM_MOVEABLE: a
/// moveable handle stays, a fixed block may move); false: no memory
fn grow(this: u32, n: u32) -> bool {
    let h = hg(this);
    if crate::k32::global_ptr(h) != 0 && n <= crate::k32::global_size(h) {
        return true;
    }
    let g = crate::k32::global_realloc(h, n, 2 | 0x40, true);
    if g != 0 {
        w32(r32(this + 4), g);
    }
    g != 0
}
fn s_stat(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    // STATSTG {pwcsName, type, cbSize (8-aligned), ...}: 72 bytes, 80 on x64
    let ps = mem::ps();
    mem::fill(p, 64 + 2 * ps, 0);
    w32(p + ps, 2); // STGTY_STREAM
    w32(p + 2 * ps, size_of(this));
    0
}
/// CopyTo(this, dest, ULARGE_INTEGER cb, read*, written*): through the destination's Write
fn s_copy_to(c: &mut Ctx) -> u32 {
    let (this, dst, cb, rd, wr) =
        if mem::x64() { (c.a(0), c.a(1), c.a64(2), c.a(3), c.a(4)) } else { (c.a(0), c.a(1), (c.a(3) as u64) << 32 | c.a(2) as u64, c.a(4), c.a(5)) };
    let (h, size, pos) = (hg(this), size_of(this), r32(this + 12));
    let n = (cb.min(size.saturating_sub(pos) as u64)) as u32;
    let done = mem::heap_alloc(4, true);
    // (IStream::Write is the 5th method)
    let f = mem::rp(mem::rp(dst) + 4 * mem::ps());
    let hr = c.call(f, &[dst, crate::k32::global_ptr(h) + pos, n, done]);
    let w = r32(done);
    mem::heap_free(done);
    w32(this + 12, pos + n);
    for (p, v) in [(rd, n), (wr, w)] {
        if p != 0 {
            w32(p, v);
            w32(p + 4, 0);
        }
    }
    hr
}
fn s_clone(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let m = r32(this + 4);
    let o = mem::heap_alloc(32, true);
    if o == 0 {
        put32(out, 0);
        return E_OUTOFMEMORY;
    }
    w32(o, stream_vtbl());
    w32(o + 4, m);
    w32(o + 12, r32(this + 12));
    w32(o + 28, 1);
    w32(m + 12, r32(m + 12) + 1);
    put32(out, o);
    0
}

fn stream_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IStream",
            &[
                (qi, 12),
                (add_ref, 4),
                (s_release, 4),
                (s_read, 16),
                (s_write, 16),
                (s_seek, 20),
                (s_set_size, 12),
                (s_copy_to, 24),
                (ok, 8),
                (ok, 4),
                (no_locks, 24),
                (no_locks, 24),
                (s_stat, 12),
                (s_clone, 8),
            ],
        )
    })
}

/// A stream over HGLOBAL `h` holding `size` bytes; 0 when the heap is full.
fn new_stream(h: u32, size: u32, del: bool) -> u32 {
    let (o, m) = (mem::heap_alloc(32, true), mem::heap_alloc(16, true));
    if o == 0 || m == 0 {
        mem::heap_free(o);
        mem::heap_free(m);
        return 0;
    }
    w32(m, h);
    w32(m + 4, size);
    w32(m + 8, del as u32);
    w32(m + 12, 1);
    w32(o, stream_vtbl());
    w32(o + 4, m);
    w32(o + 28, 1);
    o
}

fn CreateStreamOnHGlobal(c: &mut Ctx) -> u32 {
    let (h, del, out) = (c.a(0), c.a(1), c.a(2));
    if out == 0 {
        return 0x8007_0057; // E_INVALIDARG
    }
    // (no HGLOBAL: a new moveable one, empty, as Windows)
    let (h, size) = if h == 0 { (crate::k32::global_alloc(2 | 0x2000, 0), 0) } else { (h, crate::k32::global_size(h)) };
    let s = if h != 0 { new_stream(h, size, del != 0 || c.a(0) == 0) } else { 0 };
    mem::wp(out, s);
    if s == 0 {
        if c.a(0) == 0 && h != 0 {
            crate::k32::global_free(h);
        }
        return E_OUTOFMEMORY;
    }
    0
}
/// SHCreateMemStream(data, size): an IStream over a copy of the data.
fn SHCreateMemStream(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let h = crate::k32::global_alloc(0x40, n.max(64));
    // (no memory: NULL, as Windows; the copy went to address 0 and the stream over handle 0)
    if h == 0 {
        return 0;
    }
    if p != 0 && n != 0 {
        mem::copy(h, p, n);
    }
    let s = new_stream(h, if p != 0 { n } else { 0 }, true);
    if s == 0 {
        crate::k32::global_free(h);
    }
    s
}
fn GetHGlobalFromStream(c: &mut Ctx) -> u32 {
    let (s, out) = (c.a(0), c.a(1));
    if r32(s) != stream_vtbl() {
        return E_FAIL;
    }
    mem::wp(out, hg(s));
    0
}

// ---------------------------------------------------------------------------------------------
// IPicture: {vtbl, hbitmap, width, height, bits, -, -, refs}

fn himetric(px: u32) -> u32 {
    (px * 2540 + 48) / 96
}

fn p_release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = r32(this + 28).saturating_sub(1);
    w32(this + 28, n);
    if n == 0 {
        gcall(c, "gdi32", "DeleteObject", &[r32(this + 4)]);
        mem::release(r32(this + 16));
        mem::heap_free(this);
    }
    n
}
fn p_handle(c: &mut Ctx) -> u32 {
    put32(c.a(1), r32(c.a(0) + 4));
    0
}
fn p_zero(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    0
}
fn p_type(c: &mut Ctx) -> u32 {
    put32(c.a(1), 1); // PICTYPE_BITMAP
    0
}
fn p_width(c: &mut Ctx) -> u32 {
    put32(c.a(1), himetric(r32(c.a(0) + 8)));
    0
}
fn p_height(c: &mut Ctx) -> u32 {
    put32(c.a(1), himetric(r32(c.a(0) + 12)));
    0
}
fn p_render(c: &mut Ctx) -> u32 {
    // (this, hdc, x, y, cx, cy, xSrc, ySrc, cxSrc, cySrc, bounds); source in HIMETRIC, y up
    let this = c.a(0);
    let (hdc, x, y, cx, cy) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let (xs, ys, cxs, cys) = (c.i(6) as i64, c.i(7) as i64, c.i(8) as i64, c.i(9) as i64);
    let (hbm, w, h) = (r32(this + 4), r32(this + 8) as i64, r32(this + 12) as i64);
    let (hw, hh) = (himetric(w as u32) as i64, himetric(h as u32) as i64);
    let sx = xs * w / hw.max(1);
    let sw = cxs * w / hw.max(1);
    let sy = h - ys * h / hh.max(1);
    let sh = -cys * h / hh.max(1);
    let dc = gcall(c, "gdi32", "CreateCompatibleDC", &[hdc]);
    let old = gcall(c, "gdi32", "SelectObject", &[dc, hbm]);
    gcall(c, "gdi32", "StretchBlt", &[hdc, x, y, cx, cy, dc, sx as u32, sy as u32, sw as u32, sh as u32, 0x00cc_0020]);
    gcall(c, "gdi32", "SelectObject", &[dc, old]);
    gcall(c, "gdi32", "DeleteDC", &[dc]);
    0
}
fn p_select(c: &mut Ctx) -> u32 {
    let (this, hdc, pdc, pbm) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let old = gcall(c, "gdi32", "SelectObject", &[hdc, r32(this + 4)]);
    put32(pdc, 0);
    put32(pbm, old);
    0
}

fn picture_vtbl() -> u32 {
    static V: OnceLock<u32> = OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "IPicture",
            &[
                (qi, 12),
                (add_ref, 4),
                (p_release, 4),
                (p_handle, 8),
                (p_zero, 8),
                (p_type, 8),
                (p_width, 8),
                (p_height, 8),
                (p_render, 44),
                (ok, 8),
                (p_zero, 8),
                (p_select, 16),
                (p_zero, 8),
                (ok, 8),
                (ok, 4),
                (notimpl, 16),
                (p_zero, 8),
            ],
        )
    })
}

/// BGRA rows, top first (JPEG, BMP, PNG with its alpha).
pub fn decode(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    crate::picture::decode_bgra(data)
}

/// The most bytes OleLoadPicture reads from a stream of unknown size.
const PICTURE_BYTES_MAX: usize = 256 << 20;

fn OleLoadPicture(c: &mut Ctx) -> u32 {
    let (stm, size, out) = (c.a(0), c.a(1), c.a(4));
    put32(out, 0);
    // the stream's bytes (through its Read, whoever made it)
    let read = mem::rp(mem::rp(stm) + 3 * mem::ps());
    let buf = mem::heap_alloc(0x10000, false);
    let got = mem::heap_alloc(4, true);
    let mut data = Vec::new();
    loop {
        let want = if size != 0 { (size - data.len() as u32).min(0x10000) } else { 0x10000 };
        if want == 0 {
            break;
        }
        w32(got, 0);
        c.call(read, &[stm, buf, want, got]);
        // (a Read claiming more than it was asked for read no more than the buffer; a stream
        // that never ends stops at what a picture can be)
        let n = r32(got).min(want);
        data.extend_from_slice(mem::slice(buf, n));
        if n < want || data.len() >= PICTURE_BYTES_MAX {
            break;
        }
    }
    mem::heap_free(buf);
    mem::heap_free(got);
    let Some((w, h, px)) = decode(&data) else {
        eprintln!("sake: OleLoadPicture: can't decode {} bytes ({:02x?})", data.len(), &data[..data.len().min(4)]);
        return E_FAIL;
    };
    let Some(bits) = mem::alloc(w * h * 4) else { return E_FAIL };
    mem::write(bits, &px);
    let hbm = crate::gdi::bitmap_over(w as i32, h as i32, 32, bits, w * 4, false);
    let o = mem::heap_alloc(32, true);
    w32(o, picture_vtbl());
    w32(o + 4, hbm);
    w32(o + 8, w);
    w32(o + 12, h);
    w32(o + 16, bits);
    w32(o + 28, 1);
    put32(out, o);
    0
}
fn OleLoadPictureEx(c: &mut Ctx) -> u32 {
    // (stream, size, runmode, riid, x, y, flags, out): as OleLoadPicture
    let (stm, size, out) = (c.a(0), c.a(1), c.a(7));
    let tmp = mem::heap_alloc(8, true);
    let r = gcall(c, "oleaut32", "OleLoadPicture", &[stm, size, 0, 0, tmp]);
    mem::putp(out, mem::rp(tmp));
    mem::heap_free(tmp);
    r
}

pub fn init() {
    implement("ole32", "CreateStreamOnHGlobal", CreateStreamOnHGlobal);
    implement("shlwapi", "SHCreateMemStream", SHCreateMemStream);
    implement("ole32", "GetHGlobalFromStream", GetHGlobalFromStream);
    implement("oleaut32", "OleLoadPicture", OleLoadPicture);
    implement("oleaut32", "OleLoadPictureEx", OleLoadPictureEx);
}

#[cfg(test)]
mod proptests {
    use super::decode;
    use proptest::collection::vec;
    use proptest::prelude::*;

    fn bmp(w: u32, h: u32, bpp: u16, top_down: bool, px: &[u8], pal: &[u8]) -> Vec<u8> {
        let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
        let off = 54 + pal.len() as u32;
        let mut f = b"BM".to_vec();
        f.extend((off + stride as u32 * h).to_le_bytes());
        f.extend([0; 4]);
        f.extend(off.to_le_bytes());
        f.extend(40u32.to_le_bytes());
        f.extend(w.to_le_bytes());
        f.extend((if top_down { -(h as i32) } else { h as i32 }).to_le_bytes());
        f.extend(1u16.to_le_bytes());
        f.extend(bpp.to_le_bytes());
        f.extend([0; 24]);
        f.extend(pal);
        let n = bpp as usize / 8;
        for y in 0..h as usize {
            let sy = if top_down { y } else { h as usize - 1 - y };
            let row = &px[sy * w as usize * n..][..w as usize * n];
            f.extend(row);
            f.extend(vec![0; stride - row.len()]);
        }
        f
    }

    fn png(w: u32, h: u32, color: png::ColorType, px: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(color);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(px).unwrap();
        out
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// 24- and 32-bit BMPs, bottom-up or top-down: the pixels back as BGRA, top row first.
        #[test]
        fn bmp_round_trip(w in 1u32..24, h in 1u32..24, wide in any::<bool>(), top_down in any::<bool>(), seed in any::<u64>()) {
            let n = if wide { 4 } else { 3 };
            let px: Vec<u8> = (0..(w * h) as u64 * n).map(|i| (i.wrapping_mul(seed | 1) >> 7) as u8).collect();
            let (gw, gh, got) = decode(&bmp(w, h, 8 * n as u16, top_down, &px, &[])).unwrap();
            prop_assert_eq!((gw, gh), (w, h));
            for i in 0..(w * h) as usize {
                let s = &px[i * n as usize..];
                prop_assert_eq!(&got[4 * i..4 * i + 4], &[s[0], s[1], s[2], 255][..]);
            }
        }

        /// 8-bit BMPs through their palette.
        #[test]
        fn bmp_palette(w in 1u32..24, h in 1u32..12, idx in vec(any::<u8>(), 24 * 12), pal in vec(any::<u8>(), 1024)) {
            let px = &idx[..(w * h) as usize];
            let (_, _, got) = decode(&bmp(w, h, 8, false, px, &pal)).unwrap();
            for (i, &k) in px.iter().enumerate() {
                let p = &pal[4 * k as usize..];
                prop_assert_eq!(&got[4 * i..4 * i + 4], &[p[0], p[1], p[2], 255][..]);
            }
        }

        /// PNG (gray, gray+alpha, RGB, RGBA): BGRA with the image's alpha.
        #[test]
        fn png_round_trip(w in 1u32..20, h in 1u32..20, kind in 0usize..4, seed in any::<u64>()) {
            let (color, n) = [(png::ColorType::Grayscale, 1), (png::ColorType::GrayscaleAlpha, 2), (png::ColorType::Rgb, 3), (png::ColorType::Rgba, 4)][kind];
            let px: Vec<u8> = (0..(w * h) as u64 * n).map(|i| (i.wrapping_mul(seed | 1) >> 5) as u8).collect();
            let (gw, gh, got) = decode(&png(w, h, color, &px)).unwrap();
            prop_assert_eq!((gw, gh), (w, h));
            for i in 0..(w * h) as usize {
                let s = &px[i * n as usize..][..n as usize];
                let want = match n { 1 => [s[0], s[0], s[0], 255], 2 => [s[0], s[0], s[0], s[1]], 3 => [s[2], s[1], s[0], 255], _ => [s[2], s[1], s[0], s[3]] };
                prop_assert_eq!(&got[4 * i..4 * i + 4], &want[..]);
            }
        }

        /// Any bytes after a signature: None or w x h BGRA, never a panic.
        #[test]
        fn any_bytes(sig in 0usize..3, rest in vec(any::<u8>(), 0..400)) {
            let mut d = [&b"BM"[..], b"\x89PNG\r\n\x1a\n", &[0xff, 0xd8]][sig].to_vec();
            d.extend(rest);
            if let Some((w, h, px)) = decode(&d) {
                prop_assert_eq!(px.len(), w as usize * h as usize * 4);
            }
        }
    }
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::test_guest::{call, enter};
    use proptest::prelude::*;

    fn create() -> u32 {
        let out = mem::heap_alloc(4, true);
        assert_eq!(call(CreateStreamOnHGlobal, &[0, 1, out]), 0);
        let s = r32(out);
        mem::heap_free(out);
        s
    }
    fn write(s: u32, data: &[u8]) -> u32 {
        let (p, done) = (mem::heap_alloc(data.len() as u32 + 1, false), mem::heap_alloc(4, true));
        mem::write(p, data);
        let hr = call(s_write, &[s, p, data.len() as u32, done]);
        mem::heap_free(p);
        mem::heap_free(done);
        hr
    }
    fn read(s: u32, n: u32) -> Vec<u8> {
        let (p, got) = (mem::heap_alloc(n + 1, true), mem::heap_alloc(4, true));
        assert_eq!(call(s_read, &[s, p, n, got]), 0);
        let v = mem::slice(p, r32(got)).to_vec();
        mem::heap_free(p);
        mem::heap_free(got);
        v
    }
    fn seek(s: u32, to: u32) {
        assert_eq!(call(s_seek, &[s, to, 0, 0, 0]), 0);
    }
    fn clone(s: u32) -> u32 {
        let out = mem::heap_alloc(4, true);
        assert_eq!(call(s_clone, &[s, out]), 0);
        let c = r32(out);
        mem::heap_free(out);
        c
    }

    /// A write at a position near 4 GiB is STG_E_MEDIUMFULL: `pos + cb` wrapped below the
    /// size and the bytes went past the HGLOBAL into the heap.
    #[test]
    fn write_past_4g_is_medium_full() {
        let _g = enter();
        let s = create();
        assert_eq!(write(s, b"abcd"), 0);
        seek(s, 0xffff_fffe);
        assert_eq!(write(s, b"wxyz"), STG_E_MEDIUMFULL);
        assert_eq!(size_of(s), 4);
        assert_eq!(call(s_release, &[s]), 0);
    }

    /// The original released first (it owns the HGLOBAL): its clone still reads the bytes,
    /// and a write through the clone that moves the block is seen by... the clone alone left.
    #[test]
    fn clone_outlives_its_original() {
        let _g = enter();
        let s = create();
        assert_eq!(write(s, b"hello"), 0);
        let c = clone(s);
        assert_eq!(call(s_release, &[s]), 0);
        seek(c, 0);
        assert_eq!(read(c, 16), b"hello");
        assert_eq!(write(c, &[7; 5000]), 0);
        seek(c, 0);
        assert_eq!(&read(c, 5)[..], b"hello");
        assert_eq!(call(s_release, &[c]), 0);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        /// Against a model (one byte vector shared by every clone, a position each): any
        /// writes, reads, seeks, SetSize, clones and releases, in any order, read back the
        /// model's bytes and sizes, and the HGLOBAL goes with the last stream.
        #[test]
        fn clones_share_one_memory(ops in proptest::collection::vec((0u8..6, 0usize..4, 0u32..300, any::<u8>()), 1..60)) {
            let _g = enter();
            crate::test_guest::guest_thread();
            let first = create();
            let h = hg(first);
            let mut streams = vec![(first, 0u32)];
            let mut model: Vec<u8> = Vec::new();
            for (op, which, n, b) in ops {
                if streams.is_empty() { break; }
                let i = which % streams.len();
                let (s, pos) = streams[i];
                match op {
                    0 => {
                        let data: Vec<u8> = (0..n).map(|k| b.wrapping_add(k as u8)).collect();
                        prop_assert_eq!(write(s, &data), 0);
                        let end = pos as usize + data.len();
                        if end > model.len() {
                            // (a gap a write past the end opens holds what the memory held: the
                            // API leaves it undefined)
                            let old = model.len();
                            model.resize(end, 0);
                            let p = crate::k32::global_ptr(hg(s));
                            if pos as usize > old { model[old..pos as usize].copy_from_slice(mem::slice(p + old as u32, pos - old as u32)); }
                        }
                        model[pos as usize..end].copy_from_slice(&data);
                        streams[i].1 = end as u32;
                    }
                    1 => {
                        let got = read(s, n);
                        let lo = (pos as usize).min(model.len());
                        let want = &model[lo..(lo + n as usize).min(model.len())];
                        prop_assert_eq!(&got[..], want);
                        streams[i].1 = pos + want.len() as u32;
                    }
                    2 => { seek(s, n); streams[i].1 = n; }
                    3 => {
                        prop_assert_eq!(call(s_set_size, &[s, n, 0]), 0);
                        model.resize(n as usize, 0);
                        // (the bytes past a shrink are not the model's to say: written as zeros)
                        let p = crate::k32::global_ptr(hg(s));
                        mem::write(p, &model);
                    }
                    4 => { let c = clone(s); streams.push((c, pos)); }
                    _ => { call(s_release, &[s]); streams.remove(i); }
                }
                for &(s, _) in &streams { prop_assert_eq!(size_of(s) as usize, model.len()); }
            }
            for (s, _) in streams { call(s_release, &[s]); }
            // (the HGLOBAL went with the last stream)
            prop_assert_eq!(crate::k32::global_free(h), h, "the HGLOBAL is still a handle");
        }
    }
}
