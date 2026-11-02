//! IStream over an HGLOBAL (CreateStreamOnHGlobal) and IPicture (OleLoadPicture: JPEG and
//! BMP decoded in Rust into a 32-bit bitmap). COM objects are guest blocks whose first word
//! is a vtable of host_fn traps.

#![allow(non_snake_case)]

use std::sync::OnceLock;

use crate::api::{host_fn, implement, Ctx, Handler};
use crate::mem::{self, put32, r32, w32};

const E_NOTIMPL: u32 = 0x8000_4001;
const E_FAIL: u32 = 0x8000_4005;

fn vtable(name: &str, m: &[(Handler, u32)]) -> u32 {
    let v = mem::heap_alloc(4 * m.len() as u32, true);
    for (i, (f, n)) in m.iter().enumerate() {
        w32(v + 4 * i as u32, host_fn(&format!("{name}::{i}"), *f, *n));
    }
    v
}

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

// ---------------------------------------------------------------------------------------------
// IStream: {vtbl, hglobal, size, pos, delete on release, -, -, refs}

fn s_release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = r32(this + 28).saturating_sub(1);
    w32(this + 28, n);
    if n == 0 {
        if r32(this + 16) != 0 && r32(this + 4) != 0 {
            mem::heap_free(r32(this + 4));
        }
        mem::heap_free(this);
    }
    n
}
fn s_read(c: &mut Ctx) -> u32 {
    let (this, pv, cb, got) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (h, size, pos) = (r32(this + 4), r32(this + 8), r32(this + 12));
    let n = cb.min(size.saturating_sub(pos));
    if n > 0 {
        mem::copy(pv, h + pos, n);
    }
    w32(this + 12, pos + n);
    put32(got, n);
    if n < cb { 1 } else { 0 }
}
fn s_write(c: &mut Ctx) -> u32 {
    let (this, pv, cb, done) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let (mut h, size, pos) = (r32(this + 4), r32(this + 8), r32(this + 12));
    let end = pos + cb;
    if end > size {
        if h == 0 || end > mem::heap_size(h) {
            h = if h == 0 { mem::heap_alloc(end.max(64), true) } else { mem::heap_realloc(h, end * 2, true) };
            w32(this + 4, h);
        }
        w32(this + 8, end);
    }
    mem::copy(h + pos, pv, cb);
    w32(this + 12, end);
    put32(done, cb);
    0
}
fn s_seek(c: &mut Ctx) -> u32 {
    // (this, LARGE_INTEGER move, origin, ULARGE_INTEGER* new)
    let (this, lo, hi, origin, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let mv = (hi as u64) << 32 | lo as u64;
    let base = match origin {
        1 => r32(this + 12) as i64,
        2 => r32(this + 8) as i64,
        _ => 0,
    };
    let p = (base + mv as i64).max(0) as u32;
    w32(this + 12, p);
    if out != 0 {
        w32(out, p);
        w32(out + 4, 0);
    }
    0
}
fn s_set_size(c: &mut Ctx) -> u32 {
    let (this, n) = (c.a(0), c.a(1));
    let h = r32(this + 4);
    if n > mem::heap_size(h) || h == 0 {
        let h = if h == 0 { mem::heap_alloc(n.max(64), true) } else { mem::heap_realloc(h, n, true) };
        w32(this + 4, h);
    }
    w32(this + 8, n);
    0
}
fn s_stat(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    mem::fill(p, 72, 0);
    w32(p + 4, 2); // STGTY_STREAM
    w32(p + 8, r32(this + 8));
    0
}
fn s_clone(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let o = new_stream(r32(this + 4), r32(this + 8), false);
    w32(o + 12, r32(this + 12));
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
                (notimpl, 24),
                (ok, 8),
                (ok, 4),
                (notimpl, 24),
                (notimpl, 24),
                (s_stat, 12),
                (s_clone, 8),
            ],
        )
    })
}

fn new_stream(h: u32, size: u32, del: bool) -> u32 {
    let o = mem::heap_alloc(32, true);
    w32(o, stream_vtbl());
    w32(o + 4, h);
    w32(o + 8, size);
    w32(o + 16, del as u32);
    w32(o + 28, 1);
    o
}

fn CreateStreamOnHGlobal(c: &mut Ctx) -> u32 {
    let (h, del, out) = (c.a(0), c.a(1), c.a(2));
    let (h, size) = if h == 0 { (mem::heap_alloc(64, true), 0) } else { (h, mem::heap_size(h)) };
    put32(out, new_stream(h, size, del != 0 || c.a(0) == 0));
    0
}
fn GetHGlobalFromStream(c: &mut Ctx) -> u32 {
    let (s, out) = (c.a(0), c.a(1));
    if r32(s) != stream_vtbl() {
        return E_FAIL;
    }
    put32(out, r32(s + 4));
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

/// BGRA rows, top first.
fn decode(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if data.starts_with(&[0xff, 0xd8]) {
        let mut d = jpeg_decoder::Decoder::new(data);
        let px = d.decode().ok()?;
        let info = d.info()?;
        let (w, h) = (info.width as u32, info.height as u32);
        let n = (w * h) as usize;
        let mut out = vec![0xffu8; n * 4];
        for i in 0..n {
            let (r, g, b) = match info.pixel_format {
                jpeg_decoder::PixelFormat::L8 => (px[i], px[i], px[i]),
                jpeg_decoder::PixelFormat::L16 => (px[2 * i], px[2 * i], px[2 * i]),
                jpeg_decoder::PixelFormat::RGB24 => (px[3 * i], px[3 * i + 1], px[3 * i + 2]),
                jpeg_decoder::PixelFormat::CMYK32 => {
                    let (cc, m, y, k) = (px[4 * i] as u32, px[4 * i + 1] as u32, px[4 * i + 2] as u32, px[4 * i + 3] as u32);
                    ((cc * k / 255) as u8, (m * k / 255) as u8, (y * k / 255) as u8)
                }
            };
            out[4 * i] = b;
            out[4 * i + 1] = g;
            out[4 * i + 2] = r;
        }
        return Some((w, h, out));
    }
    if data.starts_with(b"BM") && data.len() > 54 {
        let u32le = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        let off = u32le(10) as usize;
        let w = u32le(18) as i32;
        let hraw = u32le(22) as i32;
        let bpp = u16::from_le_bytes([data[28], data[29]]) as usize;
        let (h, top_down) = (hraw.unsigned_abs(), hraw < 0);
        let w = w.unsigned_abs();
        let stride = (w as usize * bpp).div_ceil(32) * 4;
        let hdr = u32le(14) as usize;
        let pal = &data[14 + hdr..off.min(data.len())];
        let mut out = vec![0xffu8; (w * h) as usize * 4];
        for y in 0..h as usize {
            let sy = if top_down { y } else { h as usize - 1 - y };
            let row = data.get(off + sy * stride..off + sy * stride + stride)?;
            for x in 0..w as usize {
                let (b, g, r) = match bpp {
                    32 => (row[4 * x], row[4 * x + 1], row[4 * x + 2]),
                    24 => (row[3 * x], row[3 * x + 1], row[3 * x + 2]),
                    8 => {
                        let i = row[x] as usize * 4;
                        (pal.get(i).copied()?, pal.get(i + 1).copied()?, pal.get(i + 2).copied()?)
                    }
                    _ => return None,
                };
                let d = (y * w as usize + x) * 4;
                out[d] = b;
                out[d + 1] = g;
                out[d + 2] = r;
            }
        }
        return Some((w, h, out));
    }
    None
}

fn OleLoadPicture(c: &mut Ctx) -> u32 {
    let (stm, size, out) = (c.a(0), c.a(1), c.a(4));
    put32(out, 0);
    // the stream's bytes (through its Read, whoever made it)
    let read = r32(r32(stm) + 12);
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
        let n = r32(got);
        data.extend_from_slice(mem::slice(buf, n));
        if n < want {
            break;
        }
    }
    mem::heap_free(buf);
    mem::heap_free(got);
    let Some((w, h, px)) = decode(&data) else {
        eprintln!("vnwin: OleLoadPicture: can't decode {} bytes ({:02x?})", data.len(), &data[..data.len().min(4)]);
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
    let tmp = mem::heap_alloc(4, true);
    let r = gcall(c, "oleaut32", "OleLoadPicture", &[stm, size, 0, 0, tmp]);
    put32(out, r32(tmp));
    mem::heap_free(tmp);
    r
}

pub fn init() {
    implement("ole32", "CreateStreamOnHGlobal", CreateStreamOnHGlobal);
    implement("ole32", "GetHGlobalFromStream", GetHGlobalFromStream);
    implement("oleaut32", "OleLoadPicture", OleLoadPicture);
    implement("oleaut32", "OleLoadPictureEx", OleLoadPictureEx);
}
