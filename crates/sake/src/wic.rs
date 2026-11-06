//! Windows Imaging Component: the imaging factory with streams, decoders (PNG, JPEG, BMP through
//! ole::decode), bitmaps, format converters, scalers and clippers. Pixels live on the host as
//! straight BGRA rows; each object keeps the WIC pixel format it reports, and CopyPixels/Lock
//! convert on the way out. Sources made by the game itself are read through their vtables
//! (GetSize, GetPixelFormat, CopyPixels).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::api::Ctx;
use crate::com::{self, read_guid, Guid};
use crate::mem;

const E_FAIL: u32 = 0x8000_4005;
const E_INVALIDARG: u32 = 0x8007_0057;
const E_POINTER: u32 = 0x8000_4003;
const WINCODEC_ERR_PALETTEUNAVAILABLE: u32 = 0x8898_2f45;
const WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT: u32 = 0x8898_2f80;
const WINCODEC_ERR_COMPONENTNOTFOUND: u32 = 0x8898_2f50;
const WINCODEC_ERR_INSUFFICIENTBUFFER: u32 = 0x8898_2f8c;

/// The pixel formats sake converts between.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Fmt {
    Bgra,
    Pbgra,
    Bgr32,
    Rgba,
    Prgba,
    Rgb32,
    Bgr24,
    Rgb24,
    Gray8,
}

const WIC_BASE: (u32, u16, u16, [u8; 7]) = (0x6fdd_c324, 0x4e03, 0x4bfe, [0xb1, 0x85, 0x3d, 0x77, 0x76, 0x8d, 0xc9]);

fn fmt_guid(f: Fmt) -> Guid {
    let base = |last: u8| {
        let mut b = [0u8; 8];
        b[..7].copy_from_slice(&WIC_BASE.3);
        b[7] = last;
        (WIC_BASE.0, WIC_BASE.1, WIC_BASE.2, b)
    };
    match f {
        Fmt::Bgra => base(0x0f),
        Fmt::Pbgra => base(0x10),
        Fmt::Bgr32 => base(0x0e),
        Fmt::Bgr24 => base(0x0c),
        Fmt::Rgb24 => base(0x0d),
        Fmt::Gray8 => base(0x08),
        Fmt::Rgba => (0xf5c7_ad2d, 0x6a8d, 0x43dd, [0xa7, 0xa8, 0xa2, 0x99, 0x35, 0x26, 0x1a, 0xe9]),
        Fmt::Prgba => (0x3cc4_a650, 0xa527, 0x4d37, [0xa9, 0x16, 0x31, 0x42, 0xc7, 0xeb, 0xed, 0xba]),
        Fmt::Rgb32 => (0xd98c_6b95, 0x3efe, 0x47d6, [0xbb, 0x25, 0xeb, 0x17, 0x48, 0xab, 0x0c, 0xf1]),
    }
}

const ALL: [Fmt; 9] = [Fmt::Bgra, Fmt::Pbgra, Fmt::Bgr32, Fmt::Rgba, Fmt::Prgba, Fmt::Rgb32, Fmt::Bgr24, Fmt::Rgb24, Fmt::Gray8];

fn fmt_of(g: Guid) -> Option<Fmt> {
    ALL.into_iter().find(|&f| fmt_guid(f) == g)
}

fn bpp(f: Fmt) -> u32 {
    match f {
        Fmt::Bgr24 | Fmt::Rgb24 => 3,
        Fmt::Gray8 => 1,
        _ => 4,
    }
}

fn write_guid(p: u32, g: Guid) {
    mem::w32(p, g.0);
    mem::w16(p + 4, g.1);
    mem::w16(p + 6, g.2);
    mem::write(p + 8, &g.3);
}

/// Straight BGRA, `w * 4` bytes a row.
struct Img {
    w: u32,
    h: u32,
    px: Vec<u8>,
}

/// Pixels of `img` in `f`, rows of `stride` bytes (rectangle x, y, w, h).
fn encode(img: &Img, f: Fmt, (x, y, w, h): (u32, u32, u32, u32), stride: u32, out: &mut [u8]) {
    let n = bpp(f) as usize;
    for row in 0..h {
        let src = &img.px[(((y + row) * img.w + x) * 4) as usize..][..(w * 4) as usize];
        let dst = &mut out[(row * stride) as usize..][..w as usize * n];
        for (s, d) in src.chunks_exact(4).zip(dst.chunks_exact_mut(n)) {
            let (b, g, r, a) = (s[0], s[1], s[2], s[3]);
            let pm = |c: u8| ((c as u32 * a as u32 + 127) / 255) as u8;
            match f {
                Fmt::Bgra => d.copy_from_slice(&[b, g, r, a]),
                Fmt::Pbgra => d.copy_from_slice(&[pm(b), pm(g), pm(r), a]),
                Fmt::Bgr32 => d.copy_from_slice(&[b, g, r, 255]),
                Fmt::Rgba => d.copy_from_slice(&[r, g, b, a]),
                Fmt::Prgba => d.copy_from_slice(&[pm(r), pm(g), pm(b), a]),
                Fmt::Rgb32 => d.copy_from_slice(&[r, g, b, 255]),
                Fmt::Bgr24 => d.copy_from_slice(&[b, g, r]),
                Fmt::Rgb24 => d.copy_from_slice(&[r, g, b]),
                Fmt::Gray8 => d[0] = ((r as u32 * 77 + g as u32 * 150 + b as u32 * 29) >> 8) as u8,
            }
        }
    }
}

/// `w x h` pixels in `f` (rows of `stride` bytes) as straight BGRA.
fn decode(f: Fmt, w: u32, h: u32, stride: u32, data: &[u8]) -> Img {
    let n = bpp(f) as usize;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for row in 0..h as usize {
        let Some(src) = data.get(row * stride as usize..row * stride as usize + w as usize * n) else { break };
        let dst = &mut px[row * w as usize * 4..][..w as usize * 4];
        for (s, d) in src.chunks_exact(n).zip(dst.chunks_exact_mut(4)) {
            let un = |c: u8, a: u8| if a == 0 { 0 } else { (c as u32 * 255 / a as u32).min(255) as u8 };
            let v = match f {
                Fmt::Bgra => [s[0], s[1], s[2], s[3]],
                Fmt::Pbgra => [un(s[0], s[3]), un(s[1], s[3]), un(s[2], s[3]), s[3]],
                Fmt::Bgr32 => [s[0], s[1], s[2], 255],
                Fmt::Rgba => [s[2], s[1], s[0], s[3]],
                Fmt::Prgba => [un(s[2], s[3]), un(s[1], s[3]), un(s[0], s[3]), s[3]],
                Fmt::Rgb32 => [s[2], s[1], s[0], 255],
                Fmt::Bgr24 => [s[0], s[1], s[2], 255],
                Fmt::Rgb24 => [s[2], s[1], s[0], 255],
                Fmt::Gray8 => [s[0], s[0], s[0], 255],
            };
            d.copy_from_slice(&v);
        }
    }
    Img { w, h, px }
}

/// What each object of this module is.
enum Obj {
    Stream { data: Vec<u8>, pos: u64 },
    /// a decoder, a frame, a converter, a scaler or clipper: an image and its format
    Source { img: Arc<Img>, fmt: Fmt, container: Guid },
    /// IWICBitmap (pixels changed through Lock)
    Bitmap { img: Arc<Mutex<Img>>, fmt: Fmt },
    /// IWICBitmapLock: the bitmap, the rectangle and the guest copy of its pixels
    Lock { bitmap: Arc<Mutex<Img>>, fmt: Fmt, rect: (u32, u32, u32, u32), buf: u32, stride: u32, write: bool },
}

static OBJS: Mutex<Option<HashMap<u32, Obj>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Obj>) -> R) -> R {
    f(OBJS.lock().unwrap().get_or_insert_with(HashMap::new))
}

fn new(iface: &'static str, impls: &[(&str, crate::api::Handler)], o: Obj) -> u32 {
    let p = com::object(iface, impls, 8);
    with(|m| m.insert(p, o));
    p
}

fn release(c: &mut Ctx) -> u32 {
    let this = c.a(0);
    let n = com::release_obj(this);
    if n == 0 {
        if let Some(Obj::Lock { bitmap, fmt, rect, buf, stride, write }) = with(|m| m.remove(&this)) {
            if write {
                let data = mem::slice(buf, stride * rect.3).to_vec();
                let part = decode(fmt, rect.2, rect.3, stride, &data);
                let mut img = bitmap.lock().unwrap();
                for row in 0..rect.3 {
                    let d = (((rect.1 + row) * img.w + rect.0) * 4) as usize;
                    let s = (row * rect.2 * 4) as usize;
                    img.px[d..d + (rect.2 * 4) as usize].copy_from_slice(&part.px[s..s + (rect.2 * 4) as usize]);
                }
            }
            mem::heap_free(buf);
        }
    }
    n
}

// ---- streams ----

fn stream_impls() -> Vec<(&'static str, crate::api::Handler)> {
    vec![
        ("Release", release as crate::api::Handler),
        ("Read", s_read),
        ("Write", s_write),
        ("Seek", s_seek),
        ("SetSize", s_set_size),
        ("Stat", s_stat),
        ("Commit", ok),
        ("InitializeFromMemory", s_init_memory),
        ("InitializeFromFilename", s_init_filename),
        ("InitializeFromIStream", s_init_istream),
    ]
}

fn ok(_: &mut Ctx) -> u32 {
    0
}

fn stream<R>(this: u32, f: impl FnOnce(&mut Vec<u8>, &mut u64) -> R) -> Option<R> {
    with(|m| match m.get_mut(&this) {
        Some(Obj::Stream { data, pos }) => Some(f(data, pos)),
        _ => None,
    })
}

fn s_read(c: &mut Ctx) -> u32 {
    let (this, pv, cb, got) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let n = stream(this, |d, pos| {
        let start = (*pos as usize).min(d.len());
        let n = (cb as usize).min(d.len() - start);
        mem::write(pv, &d[start..start + n]);
        *pos += n as u64;
        n as u32
    })
    .unwrap_or(0);
    if got != 0 {
        mem::w32(got, n);
    }
    (n < cb) as u32
}

fn s_write(c: &mut Ctx) -> u32 {
    let (this, pv, cb, done) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let src = mem::slice(pv, cb).to_vec();
    stream(this, |d, pos| {
        let end = *pos as usize + src.len();
        if d.len() < end {
            d.resize(end, 0);
        }
        d[*pos as usize..end].copy_from_slice(&src);
        *pos = end as u64;
    });
    if done != 0 {
        mem::w32(done, cb);
    }
    0
}

fn s_seek(c: &mut Ctx) -> u32 {
    // (this, LARGE_INTEGER move, origin, ULARGE_INTEGER* new); x64: move is one register
    let (mv, origin, out) = if mem::x64() {
        (c.a64(1) as i64, c.a(2), c.a(3))
    } else {
        ((c.a(1) as u64 | (c.a(2) as u64) << 32) as i64, c.a(3), c.a(4))
    };
    let this = c.a(0);
    let p = stream(this, |d, pos| {
        let base = match origin {
            1 => *pos as i64,
            2 => d.len() as i64,
            _ => 0,
        };
        *pos = (base + mv).max(0) as u64;
        *pos
    });
    match p {
        Some(p) => {
            if out != 0 {
                mem::w64(out, p);
            }
            0
        }
        None => E_FAIL,
    }
}

fn s_set_size(c: &mut Ctx) -> u32 {
    let (this, n) = (c.a(0), c.a(1));
    stream(this, |d, _| d.resize(n as usize, 0));
    0
}

fn s_stat(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    let len = stream(this, |d, _| d.len() as u64).unwrap_or(0);
    // STATSTG: pwcsName, type, cbSize (8-aligned), times, ...
    let k = mem::ps();
    mem::fill(p, if mem::x64() { 80 } else { 72 }, 0);
    mem::w32(p + k, 2); // STGTY_STREAM
    mem::w64(p + if mem::x64() { 16 } else { 8 }, len);
    0
}

fn s_init_memory(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a(2));
    let src = mem::slice(p, n).to_vec();
    stream(this, |d, pos| {
        *d = src;
        *pos = 0;
    });
    0
}

fn s_init_filename(c: &mut Ctx) -> u32 {
    let (this, name) = (c.a(0), c.a(1));
    let name = String::from_utf16_lossy(&mem::wstr(name));
    let Some(data) = crate::d3dcompiler::read_file(&name) else { return 0x8007_0002 };
    stream(this, |d, pos| {
        *d = data;
        *pos = 0;
    });
    0
}

fn s_init_istream(c: &mut Ctx) -> u32 {
    let (this, src) = (c.a(0), c.a(1));
    let data = read_stream(c, src);
    stream(this, |d, pos| {
        *d = data;
        *pos = 0;
    });
    0
}

/// The rest of a stream's bytes, through its Read (whoever made it).
fn read_stream(c: &mut Ctx, stm: u32) -> Vec<u8> {
    if let Some(v) = stream(stm, |d, pos| d[(*pos as usize).min(d.len())..].to_vec()) {
        return v;
    }
    let read = mem::rp(mem::rp(stm) + 3 * mem::ps());
    let buf = mem::heap_alloc(0x10000, false);
    let got = mem::heap_alloc(4, true);
    let mut data = Vec::new();
    loop {
        mem::w32(got, 0);
        c.call(read, &[stm, buf, 0x10000, got]);
        let n = mem::r32(got).min(0x10000);
        data.extend_from_slice(mem::slice(buf, n));
        if n < 0x10000 {
            break;
        }
    }
    mem::heap_free(buf);
    mem::heap_free(got);
    data
}

// ---- image sources ----

const PNG: Guid = (0x1b7c_faf4, 0x713f, 0x473c, [0xbb, 0xcd, 0x61, 0x37, 0x42, 0x5f, 0xae, 0xaf]);
const JPEG: Guid = (0x19e4_a5aa, 0x5662, 0x4fc5, [0xa0, 0xc0, 0x17, 0x58, 0x02, 0x8e, 0x10, 0x57]);
const BMP: Guid = (0x0af1_d87e, 0xfcfe, 0x4188, [0xbd, 0xeb, 0xa7, 0x90, 0x64, 0x71, 0xcb, 0xe3]);

fn source_impls() -> Vec<(&'static str, crate::api::Handler)> {
    vec![
        ("Release", release as crate::api::Handler),
        ("GetSize", src_size),
        ("GetPixelFormat", src_format),
        ("GetResolution", src_resolution),
        ("CopyPalette", no_palette),
        ("CopyPixels", src_copy),
    ]
}

/// A source object of `iface`.
fn new_source(iface: &'static str, img: Arc<Img>, fmt: Fmt, extra: &[(&'static str, crate::api::Handler)]) -> u32 {
    let mut impls = source_impls();
    impls.extend_from_slice(extra);
    new(iface, &impls, Obj::Source { img, fmt, container: (0, 0, 0, [0; 8]) })
}

/// The image of one of this module's sources or bitmaps, and its format.
fn image_of(this: u32) -> Option<(Arc<Img>, Fmt)> {
    with(|m| match m.get(&this)? {
        Obj::Source { img, fmt, .. } => Some((img.clone(), *fmt)),
        Obj::Bitmap { img, fmt } => {
            let i = img.lock().unwrap();
            Some((Arc::new(Img { w: i.w, h: i.h, px: i.px.clone() }), *fmt))
        }
        _ => None,
    })
}

/// Any IWICBitmapSource's pixels: ours directly, the game's through its methods.
fn read_source(c: &mut Ctx, src: u32) -> Option<(Arc<Img>, Fmt)> {
    if src == 0 {
        return None;
    }
    if let Some(r) = image_of(src) {
        return Some(r);
    }
    let vt = mem::rp(src);
    let k = mem::ps();
    let tmp = mem::heap_alloc(16, true);
    c.call(mem::rp(vt + 3 * k), &[src, tmp, tmp + 4]);
    let (w, h) = (mem::r32(tmp), mem::r32(tmp + 4));
    c.call(mem::rp(vt + 4 * k), &[src, tmp]);
    let f = fmt_of(read_guid(tmp));
    mem::heap_free(tmp);
    let f = f?;
    let stride = w * bpp(f);
    let buf = mem::heap_alloc((stride * h).max(4), false);
    let r = c.call(mem::rp(vt + 7 * k), &[src, 0, stride, stride * h, buf]);
    let data = mem::slice(buf, stride * h).to_vec();
    mem::heap_free(buf);
    (r == 0).then(|| (Arc::new(decode(f, w, h, stride, &data)), f))
}

fn src_size(c: &mut Ctx) -> u32 {
    let (this, pw, ph) = (c.a(0), c.a(1), c.a(2));
    let Some((img, _)) = image_of(this) else { return E_FAIL };
    if pw == 0 || ph == 0 {
        return E_INVALIDARG;
    }
    mem::w32(pw, img.w);
    mem::w32(ph, img.h);
    0
}

fn src_format(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let Some((_, f)) = image_of(this) else { return E_FAIL };
    if out == 0 {
        return E_INVALIDARG;
    }
    write_guid(out, fmt_guid(f));
    0
}

fn src_resolution(c: &mut Ctx) -> u32 {
    let (x, y) = (c.a(1), c.a(2));
    if x != 0 {
        mem::w64(x, 96f64.to_bits());
    }
    if y != 0 {
        mem::w64(y, 96f64.to_bits());
    }
    0
}

fn no_palette(_: &mut Ctx) -> u32 {
    WINCODEC_ERR_PALETTEUNAVAILABLE
}

/// The rectangle a WICRect* asks for (NULL: everything), if inside the image.
fn rect(p: u32, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    if p == 0 {
        return Some((0, 0, w, h));
    }
    let (x, y, rw, rh) = (mem::r32(p) as i32, mem::r32(p + 4) as i32, mem::r32(p + 8) as i32, mem::r32(p + 12) as i32);
    if x < 0 || y < 0 || rw < 0 || rh < 0 || (x + rw) as u32 > w || (y + rh) as u32 > h {
        return None;
    }
    Some((x as u32, y as u32, rw as u32, rh as u32))
}

fn src_copy(c: &mut Ctx) -> u32 {
    let (this, prc, stride, size, buf) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((img, f)) = image_of(this) else { return E_FAIL };
    let Some(r) = rect(prc, img.w, img.h) else { return E_INVALIDARG };
    if buf == 0 {
        return E_POINTER;
    }
    if r.2 == 0 || r.3 == 0 {
        return 0;
    }
    let row = r.2 * bpp(f);
    if stride < row || (size as u64) < stride as u64 * (r.3 - 1) as u64 + row as u64 {
        return WINCODEC_ERR_INSUFFICIENTBUFFER;
    }
    let mut out = mem::slice(buf, stride * (r.3 - 1) + row).to_vec();
    encode(&img, f, r, stride, &mut out);
    mem::write(buf, &out);
    0
}

// ---- decoders ----

fn decoder(data: &[u8]) -> Option<u32> {
    let container = if data.starts_with(b"\x89PNG") {
        PNG
    } else if data.starts_with(&[0xff, 0xd8]) {
        JPEG
    } else if data.starts_with(b"BM") {
        BMP
    } else {
        return None;
    };
    let (w, h, px) = crate::ole::decode(data)?;
    let img = Arc::new(Img { w, h, px });
    let fmt = if container == PNG { Fmt::Bgra } else { Fmt::Bgr32 };
    let d = new(
        "IWICBitmapDecoder",
        &[
            ("Release", release),
            ("GetContainerFormat", dec_container),
            ("GetFrameCount", dec_frame_count),
            ("GetFrame", dec_frame),
            ("CopyPalette", no_palette),
            ("Initialize", ok),
        ],
        Obj::Source { img, fmt, container },
    );
    Some(d)
}

fn dec_container(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let g = with(|m| match m.get(&this) {
        Some(Obj::Source { container, .. }) => Some(*container),
        _ => None,
    });
    match g {
        Some(g) if out != 0 => {
            write_guid(out, g);
            0
        }
        _ => E_INVALIDARG,
    }
}

fn dec_frame_count(c: &mut Ctx) -> u32 {
    mem::w32(c.a(1), 1);
    0
}

fn dec_frame(c: &mut Ctx) -> u32 {
    let (this, index, out) = (c.a(0), c.a(1), c.a(2));
    let Some((img, f)) = image_of(this) else { return E_FAIL };
    if index != 0 {
        return E_INVALIDARG;
    }
    mem::putp(out, new_source("IWICBitmapFrameDecode", img, f, &[]));
    0
}

fn create_decoder(data: &[u8], out: u32) -> u32 {
    match decoder(data) {
        Some(d) => {
            mem::putp(out, d);
            0
        }
        None => {
            eprintln!("sake: WIC: no decoder for {} bytes ({:02x?})", data.len(), &data[..data.len().min(4)]);
            mem::putp(out, 0);
            WINCODEC_ERR_COMPONENTNOTFOUND
        }
    }
}

// ---- the factory ----

fn f_decoder_from_filename(c: &mut Ctx) -> u32 {
    // (this, name, vendor, access, options, out)
    let (name, out) = (c.a(1), c.a(5));
    let name = String::from_utf16_lossy(&mem::wstr(name));
    match crate::d3dcompiler::read_file(&name) {
        Some(d) => create_decoder(&d, out),
        None => {
            mem::putp(out, 0);
            0x8007_0002
        }
    }
}

fn f_decoder_from_stream(c: &mut Ctx) -> u32 {
    // (this, stream, vendor, options, out)
    let (stm, out) = (c.a(1), c.a(4));
    if stm == 0 {
        return E_INVALIDARG;
    }
    let data = read_stream(c, stm);
    create_decoder(&data, out)
}

fn f_create_stream(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    mem::putp(out, new("IWICStream", &stream_impls(), Obj::Stream { data: Vec::new(), pos: 0 }));
    0
}

fn f_create_converter(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let empty = Arc::new(Img { w: 0, h: 0, px: Vec::new() });
    mem::putp(out, new_source("IWICFormatConverter", empty, Fmt::Bgra, &[("Initialize", conv_init), ("CanConvert", conv_can)]));
    0
}

fn set_source(this: u32, img: Arc<Img>, fmt: Fmt) {
    with(|m| {
        if let Some(Obj::Source { img: i, fmt: f, .. }) = m.get_mut(&this) {
            *i = img;
            *f = fmt;
        }
    });
}

fn conv_init(c: &mut Ctx) -> u32 {
    // (this, source, dst format, dither, palette, double alpha threshold, palette type)
    let (this, src, fmt) = (c.a(0), c.a(1), c.a(2));
    let Some(f) = fmt_of(read_guid(fmt)) else {
        eprintln!("sake: WIC converter: unsupported format {:x?}", read_guid(fmt));
        return WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT;
    };
    let Some((img, _)) = read_source(c, src) else { return E_INVALIDARG };
    set_source(this, img, f);
    0
}

fn conv_can(c: &mut Ctx) -> u32 {
    let (from, to, out) = (c.a(1), c.a(2), c.a(3));
    let yes = fmt_of(read_guid(from)).is_some() && fmt_of(read_guid(to)).is_some();
    mem::w32(out, yes as u32);
    0
}

fn f_create_scaler(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let empty = Arc::new(Img { w: 0, h: 0, px: Vec::new() });
    mem::putp(out, new_source("IWICBitmapScaler", empty, Fmt::Bgra, &[("Initialize", scaler_init)]));
    0
}

fn scaler_init(c: &mut Ctx) -> u32 {
    // (this, source, width, height, mode): bilinear, or nearest for mode 0
    let (this, src, w, h, mode) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((img, f)) = read_source(c, src) else { return E_INVALIDARG };
    if w == 0 || h == 0 {
        return E_INVALIDARG;
    }
    set_source(this, Arc::new(scale(&img, w, h, mode == 0)), f);
    0
}

fn scale(img: &Img, w: u32, h: u32, nearest: bool) -> Img {
    let mut px = vec![0u8; (w * h * 4) as usize];
    let (sw, sh) = (img.w.max(1), img.h.max(1));
    let at = |x: u32, y: u32, k: usize| img.px[((y.min(sh - 1) * img.w + x.min(sw - 1)) * 4) as usize + k] as f32;
    for y in 0..h {
        for x in 0..w {
            let fx = ((x as f32 + 0.5) * sw as f32 / w as f32 - 0.5).max(0.0);
            let fy = ((y as f32 + 0.5) * sh as f32 / h as f32 - 0.5).max(0.0);
            let d = ((y * w + x) * 4) as usize;
            if nearest || img.px.is_empty() {
                let (sx, sy) = ((x * sw / w).min(sw - 1), (y * sh / h).min(sh - 1));
                if !img.px.is_empty() {
                    px[d..d + 4].copy_from_slice(&img.px[((sy * img.w + sx) * 4) as usize..][..4]);
                }
                continue;
            }
            let (x0, y0) = (fx as u32, fy as u32);
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            for k in 0..4 {
                let top = at(x0, y0, k) * (1.0 - tx) + at(x0 + 1, y0, k) * tx;
                let bot = at(x0, y0 + 1, k) * (1.0 - tx) + at(x0 + 1, y0 + 1, k) * tx;
                px[d + k] = (top * (1.0 - ty) + bot * ty + 0.5) as u8;
            }
        }
    }
    Img { w, h, px }
}

fn crop(img: &Img, (x, y, w, h): (u32, u32, u32, u32)) -> Img {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        px.extend_from_slice(&img.px[((row * img.w + x) * 4) as usize..][..(w * 4) as usize]);
    }
    Img { w, h, px }
}

fn f_create_clipper(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let empty = Arc::new(Img { w: 0, h: 0, px: Vec::new() });
    mem::putp(out, new_source("IWICBitmapClipper", empty, Fmt::Bgra, &[("Initialize", clipper_init)]));
    0
}

fn clipper_init(c: &mut Ctx) -> u32 {
    let (this, src, prc) = (c.a(0), c.a(1), c.a(2));
    let Some((img, f)) = read_source(c, src) else { return E_INVALIDARG };
    let Some(r) = rect(prc, img.w, img.h) else { return E_INVALIDARG };
    set_source(this, Arc::new(crop(&img, r)), f);
    0
}

fn f_create_flip(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let empty = Arc::new(Img { w: 0, h: 0, px: Vec::new() });
    mem::putp(out, new_source("IWICBitmapFlipRotator", empty, Fmt::Bgra, &[("Initialize", flip_init)]));
    0
}

fn flip_init(c: &mut Ctx) -> u32 {
    // (this, source, options): rotate 90/180/270 (1..3), flip horizontal 8, vertical 16
    let (this, src, opt) = (c.a(0), c.a(1), c.a(2));
    let Some((img, f)) = read_source(c, src) else { return E_INVALIDARG };
    let rot = opt & 3;
    let (w, h) = if rot & 1 == 1 { (img.h, img.w) } else { (img.w, img.h) };
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (mut dx, mut dy) = (x, y);
            if opt & 8 != 0 {
                dx = w - 1 - dx;
            }
            if opt & 16 != 0 {
                dy = h - 1 - dy;
            }
            // the source pixel this destination pixel shows
            let (sx, sy) = match rot {
                1 => (dy, img.h - 1 - dx),
                2 => (img.w - 1 - dx, img.h - 1 - dy),
                3 => (img.w - 1 - dy, dx),
                _ => (dx, dy),
            };
            let d = ((y * w + x) * 4) as usize;
            px[d..d + 4].copy_from_slice(&img.px[((sy * img.w + sx) * 4) as usize..][..4]);
        }
    }
    set_source(this, Arc::new(Img { w, h, px }), f);
    0
}

fn new_bitmap(img: Img, fmt: Fmt) -> u32 {
    let mut impls = source_impls();
    impls.extend_from_slice(&[("Lock", bm_lock as crate::api::Handler), ("SetResolution", ok), ("SetPalette", ok)]);
    new("IWICBitmap", &impls, Obj::Bitmap { img: Arc::new(Mutex::new(img)), fmt })
}

fn f_create_bitmap(c: &mut Ctx) -> u32 {
    // (this, width, height, format, option, out)
    let (w, h, fmt, out) = (c.a(1), c.a(2), c.a(3), c.a(5));
    let Some(f) = fmt_of(read_guid(fmt)) else { return WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT };
    mem::putp(out, new_bitmap(Img { w, h, px: vec![0; (w * h * 4) as usize] }, f));
    0
}

fn f_bitmap_from_memory(c: &mut Ctx) -> u32 {
    // (this, width, height, format, stride, size, buffer, out)
    let (w, h, fmt, stride, size, buf, out) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    let Some(f) = fmt_of(read_guid(fmt)) else { return WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT };
    let data = mem::slice(buf, size).to_vec();
    mem::putp(out, new_bitmap(decode(f, w, h, stride, &data), f));
    0
}

fn f_bitmap_from_source(c: &mut Ctx) -> u32 {
    // (this, source, option, out)
    let (src, out) = (c.a(1), c.a(3));
    let Some((img, f)) = read_source(c, src) else { return E_INVALIDARG };
    mem::putp(out, new_bitmap(Img { w: img.w, h: img.h, px: img.px.clone() }, f));
    0
}

fn f_bitmap_from_source_rect(c: &mut Ctx) -> u32 {
    // (this, source, x, y, width, height, out)
    let (src, x, y, w, h, out) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let Some((img, f)) = read_source(c, src) else { return E_INVALIDARG };
    if x + w > img.w || y + h > img.h {
        return E_INVALIDARG;
    }
    mem::putp(out, new_bitmap(crop(&img, (x, y, w, h)), f));
    0
}

fn bm_lock(c: &mut Ctx) -> u32 {
    // (this, rect, flags (1 read, 2 write), out)
    let (this, prc, flags, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((bitmap, fmt)) = with(|m| match m.get(&this) {
        Some(Obj::Bitmap { img, fmt }) => Some((img.clone(), *fmt)),
        _ => None,
    }) else {
        return E_FAIL;
    };
    let (w, h) = {
        let i = bitmap.lock().unwrap();
        (i.w, i.h)
    };
    let Some(r) = rect(prc, w, h) else { return E_INVALIDARG };
    let stride = (r.2 * bpp(fmt)).div_ceil(4) * 4;
    let buf = mem::heap_alloc((stride * r.3).max(4), true);
    let mut data = vec![0u8; (stride * r.3) as usize];
    encode(&bitmap.lock().unwrap(), fmt, r, stride, &mut data);
    mem::write(buf, &data);
    let l = new(
        "IWICBitmapLock",
        &[("Release", release), ("GetSize", lock_size), ("GetStride", lock_stride), ("GetDataPointer", lock_data), ("GetPixelFormat", lock_format)],
        Obj::Lock { bitmap, fmt, rect: r, buf, stride, write: flags & 2 != 0 },
    );
    mem::putp(out, l);
    0
}

fn lock<R>(this: u32, f: impl FnOnce(Fmt, (u32, u32, u32, u32), u32, u32) -> R) -> Option<R> {
    with(|m| match m.get(&this) {
        Some(Obj::Lock { fmt, rect, buf, stride, .. }) => Some(f(*fmt, *rect, *buf, *stride)),
        _ => None,
    })
}

fn lock_size(c: &mut Ctx) -> u32 {
    let (this, pw, ph) = (c.a(0), c.a(1), c.a(2));
    lock(this, |_, r, _, _| {
        mem::w32(pw, r.2);
        mem::w32(ph, r.3);
    })
    .map_or(E_FAIL, |_| 0)
}

fn lock_stride(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    lock(this, |_, _, _, s| mem::w32(out, s)).map_or(E_FAIL, |_| 0)
}

fn lock_data(c: &mut Ctx) -> u32 {
    let (this, psize, pptr) = (c.a(0), c.a(1), c.a(2));
    lock(this, |_, r, buf, s| {
        mem::w32(psize, s * r.3);
        mem::putp(pptr, buf);
    })
    .map_or(E_FAIL, |_| 0)
}

fn lock_format(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    lock(this, |f, _, _, _| write_guid(out, fmt_guid(f))).map_or(E_FAIL, |_| 0)
}

/// CoCreateInstance of the imaging factory (IWICImagingFactory and IWICImagingFactory2).
pub fn factory() -> u32 {
    com::object(
        "IWICImagingFactory2",
        &[
            ("CreateDecoderFromFilename", f_decoder_from_filename),
            ("CreateDecoderFromStream", f_decoder_from_stream),
            ("CreateFormatConverter", f_create_converter),
            ("CreateBitmapScaler", f_create_scaler),
            ("CreateBitmapClipper", f_create_clipper),
            ("CreateBitmapFlipRotator", f_create_flip),
            ("CreateStream", f_create_stream),
            ("CreateBitmap", f_create_bitmap),
            ("CreateBitmapFromSource", f_bitmap_from_source),
            ("CreateBitmapFromSourceRect", f_bitmap_from_source_rect),
            ("CreateBitmapFromMemory", f_bitmap_from_memory),
        ],
        8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_round_trip() {
        let img = Img { w: 2, h: 1, px: vec![10, 20, 30, 128, 1, 2, 3, 255] };
        for f in ALL {
            let stride = 2 * bpp(f) + 1;
            let mut out = vec![0u8; stride as usize];
            encode(&img, f, (0, 0, 2, 1), stride, &mut out);
            let back = decode(f, 2, 1, stride, &out);
            match f {
                Fmt::Bgra | Fmt::Rgba => assert_eq!(back.px, img.px, "{f:?}"),
                Fmt::Bgr32 | Fmt::Rgb32 | Fmt::Bgr24 | Fmt::Rgb24 => assert_eq!(back.px, vec![10, 20, 30, 255, 1, 2, 3, 255], "{f:?}"),
                Fmt::Pbgra | Fmt::Prgba => {
                    // premultiplied and back: within rounding
                    for (a, b) in back.px.iter().zip(&img.px) {
                        assert!((*a as i32 - *b as i32).abs() <= 2, "{f:?} {:?}", back.px);
                    }
                }
                Fmt::Gray8 => assert_eq!(back.px[3], 255),
            }
        }
    }

    #[test]
    fn premultiplied_order() {
        let img = Img { w: 1, h: 1, px: vec![255, 0, 0, 128] }; // blue, half alpha
        let mut out = [0u8; 4];
        encode(&img, Fmt::Pbgra, (0, 0, 1, 1), 4, &mut out);
        assert_eq!(out, [128, 0, 0, 128]);
        encode(&img, Fmt::Prgba, (0, 0, 1, 1), 4, &mut out);
        assert_eq!(out, [0, 0, 128, 128]);
    }

    #[test]
    fn scale_and_flip() {
        let img = Img { w: 2, h: 1, px: vec![0, 0, 0, 255, 200, 200, 200, 255] };
        let s = scale(&img, 4, 2, true);
        assert_eq!((s.w, s.h), (4, 2));
        assert_eq!(&s.px[0..4], &[0, 0, 0, 255]);
        assert_eq!(&s.px[12..16], &[200, 200, 200, 255]);
        let c = crop(&img, (1, 0, 1, 1));
        assert_eq!(c.px, vec![200, 200, 200, 255]);
    }

    #[test]
    fn guids() {
        for f in ALL {
            assert_eq!(fmt_of(fmt_guid(f)), Some(f));
        }
    }
}
