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

use crate::picture::{self, Fmt};

fn fmt_guid(f: Fmt) -> Guid {
    f.guid()
}

fn fmt_of(g: Guid) -> Option<Fmt> {
    Fmt::of(g)
}

fn write_guid(p: u32, g: Guid) {
    mem::w32(p, g.0);
    mem::w16(p + 4, g.1);
    mem::w16(p + 6, g.2);
    mem::write(p + 8, &g.3);
}

/// Straight BGRA, `w * 4` bytes a row.
#[derive(Clone)]
struct Img {
    w: u32,
    h: u32,
    px: Vec<u8>,
    /// the palette of indexed formats (ARGB), as CopyPalette gives it
    palette: Vec<u32>,
    dpi: (f64, f64),
}

/// Pixels of `img` in `f`, rows of `stride` bytes (rectangle x, y, w, h).
fn encode(img: &Img, f: Fmt, r: (u32, u32, u32, u32), stride: u32, out: &mut [u8]) {
    picture::from_bgra(&img.px, img.w, f, r, stride, out, &img.palette);
}

/// `w x h` pixels in `f` (rows of `stride` bytes) as straight BGRA.
fn decode(f: Fmt, w: u32, h: u32, stride: u32, data: &[u8], palette: &[u32]) -> Img {
    Img {
        w,
        h,
        px: picture::to_bgra(f, w, h, stride, data, palette),
        palette: palette.to_vec(),
        dpi: (96.0, 96.0),
    }
}

impl Img {
    fn new(w: u32, h: u32, px: Vec<u8>) -> Img {
        Img {
            w,
            h,
            px,
            palette: Vec::new(),
            dpi: (96.0, 96.0),
        }
    }
}

/// What each object of this module is.
enum Obj {
    Stream {
        data: Vec<u8>,
        pos: u64,
    },
    /// a frame, a converter, a scaler or clipper: an image and its format, and for a frame in
    /// its own format (or a converter to it) the decoded pixels as they are
    Source {
        img: Arc<Img>,
        fmt: Fmt,
        raw: Option<Arc<picture::Frame>>,
    },
    /// a decoder: the picture and its container format
    Decoder {
        pic: Arc<picture::Picture>,
        container: Guid,
    },
    /// IWICPalette: ARGB colors and the WICBitmapPaletteType
    Palette {
        colors: Vec<u32>,
        kind: u32,
    },
    /// IWICBitmap (pixels changed through Lock)
    /// (and for an indexed one made from memory its bytes and stride, for SetPalette)
    Bitmap {
        img: Arc<Mutex<Img>>,
        fmt: Fmt,
        raw: Option<(Vec<u8>, u32)>,
    },
    /// IWICPixelFormatInfo2 of a pixel format (CreateComponentInfo)
    FormatInfo(Fmt),
    /// IWICBitmapLock: the bitmap, the rectangle and the guest copy of its pixels
    Lock {
        bitmap: Arc<Mutex<Img>>,
        fmt: Fmt,
        rect: (u32, u32, u32, u32),
        buf: u32,
        stride: u32,
        write: bool,
    },
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
        if let Some(Obj::Lock {
            bitmap,
            fmt,
            rect,
            buf,
            stride,
            write,
        }) = with(|m| m.remove(&this))
        {
            if write {
                let data = mem::slice(buf, stride * rect.3).to_vec();
                let mut img = bitmap.lock().unwrap();
                let part = decode(fmt, rect.2, rect.3, stride, &data, &img.palette);
                for row in 0..rect.3 {
                    let d = (((rect.1 + row) * img.w + rect.0) * 4) as usize;
                    let s = (row * rect.2 * 4) as usize;
                    img.px[d..d + (rect.2 * 4) as usize]
                        .copy_from_slice(&part.px[s..s + (rect.2 * 4) as usize]);
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
        (
            (c.a(1) as u64 | (c.a(2) as u64) << 32) as i64,
            c.a(3),
            c.a(4),
        )
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
    let (this, n, high) = (c.a(0), c.a(1), c.a(2));
    if high != 0 || n > 1 << 30 {
        return 0x8003_0070; // STG_E_MEDIUMFULL
    }
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
    let Some(data) = crate::d3dcompiler::read_file(&name) else {
        return 0x8007_0002;
    };
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
pub(crate) fn read_stream(c: &mut Ctx, stm: u32) -> Vec<u8> {
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

const PNG: Guid = (
    0x1b7c_faf4,
    0x713f,
    0x473c,
    [0xbb, 0xcd, 0x61, 0x37, 0x42, 0x5f, 0xae, 0xaf],
);
const JPEG: Guid = (
    0x19e4_a5aa,
    0x5662,
    0x4fc5,
    [0xa0, 0xc0, 0x17, 0x58, 0x02, 0x8e, 0x10, 0x57],
);
const BMP: Guid = (
    0x0af1_d87e,
    0xfcfe,
    0x4188,
    [0xbd, 0xeb, 0xa7, 0x90, 0x64, 0x71, 0xcb, 0xe3],
);
const GIF: Guid = (
    0x1f8a_5601,
    0x7d4d,
    0x4cbd,
    [0x9c, 0x82, 0x1b, 0xc8, 0xd4, 0xee, 0xb9, 0xa5],
);

fn source_impls() -> Vec<(&'static str, crate::api::Handler)> {
    vec![
        ("Release", release as crate::api::Handler),
        ("GetSize", src_size),
        ("GetPixelFormat", src_format),
        ("GetResolution", src_resolution),
        ("CopyPalette", src_palette),
        ("CopyPixels", src_copy),
    ]
}

/// A source object of `iface`.
fn new_source(
    iface: &'static str,
    img: Arc<Img>,
    fmt: Fmt,
    extra: &[(&'static str, crate::api::Handler)],
) -> u32 {
    let mut impls = source_impls();
    impls.extend_from_slice(extra);
    new(
        iface,
        &impls,
        Obj::Source {
            img,
            fmt,
            raw: None,
        },
    )
}

/// The image of one of this module's sources or bitmaps, and its format.
fn image_of(this: u32) -> Option<(Arc<Img>, Fmt)> {
    with(|m| match m.get(&this)? {
        Obj::Source { img, fmt, .. } => Some((img.clone(), *fmt)),
        Obj::Bitmap { img, fmt, .. } => {
            let i = img.lock().unwrap();
            Some((Arc::new(i.clone()), *fmt))
        }
        _ => None,
    })
}

/// A bitmap source's pixels as premultiplied BGRA rows (w * 4 bytes each), for Direct2D.
pub fn pbgra(c: &mut Ctx, src: u32) -> Option<(u32, u32, Vec<u8>)> {
    let (img, _) = read_source(c, src)?;
    let mut out = vec![0u8; (img.w * img.h * 4) as usize];
    encode(&img, Fmt::Pbgra, (0, 0, img.w, img.h), img.w * 4, &mut out);
    Some((img.w, img.h, out))
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
    let stride = f.row_bytes(w).div_ceil(4) * 4;
    let buf = mem::heap_alloc((stride * h).max(4), false);
    let r = c.call(mem::rp(vt + 7 * k), &[src, 0, stride, stride * h, buf]);
    let data = mem::slice(buf, stride * h).to_vec();
    mem::heap_free(buf);
    (r == 0).then(|| (Arc::new(decode(f, w, h, stride, &data, &[])), f))
}

fn src_size(c: &mut Ctx) -> u32 {
    let (this, pw, ph) = (c.a(0), c.a(1), c.a(2));
    let Some((img, _)) = image_of(this) else {
        return E_FAIL;
    };
    if pw == 0 || ph == 0 {
        return E_INVALIDARG;
    }
    mem::w32(pw, img.w);
    mem::w32(ph, img.h);
    0
}

fn src_format(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let Some((_, f)) = image_of(this) else {
        return E_FAIL;
    };
    if out == 0 {
        return E_INVALIDARG;
    }
    write_guid(out, fmt_guid(f));
    0
}

/// GetResolution(this, &x, &y): a frame's dots per inch (and of what was made from it), else 96
fn src_resolution(c: &mut Ctx) -> u32 {
    let (this, x, y) = (c.a(0), c.a(1), c.a(2));
    if x == 0 || y == 0 {
        return E_INVALIDARG;
    }
    let (dx, dy) = with(|m| match m.get(&this) {
        Some(Obj::Source { img, .. }) => img.dpi,
        Some(Obj::Bitmap { img, .. }) => img.lock().unwrap().dpi,
        _ => (96.0, 96.0),
    });
    mem::w64(x, dx.to_bits());
    mem::w64(y, dy.to_bits());
    0
}

/// CopyPalette(this, IWICPalette*): a source's palette, if it has one
fn src_palette(c: &mut Ctx) -> u32 {
    let (this, pal) = (c.a(0), c.a(1));
    let Some((img, _)) = image_of(this) else {
        return E_FAIL;
    };
    set_palette(pal, img.palette.clone(), 0)
}

fn set_palette(pal: u32, colors: Vec<u32>, kind: u32) -> u32 {
    if colors.is_empty() {
        return WINCODEC_ERR_PALETTEUNAVAILABLE;
    }
    with(|m| match m.get_mut(&pal) {
        Some(Obj::Palette { colors: c, kind: k }) => {
            *c = colors;
            *k = kind;
            0
        }
        _ => E_INVALIDARG,
    })
}

/// The rectangle a WICRect* asks for (NULL: everything), if inside the image.
fn rect(p: u32, w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    if p == 0 {
        return Some((0, 0, w, h));
    }
    let (x, y, rw, rh) = (
        mem::r32(p) as i32,
        mem::r32(p + 4) as i32,
        mem::r32(p + 8) as i32,
        mem::r32(p + 12) as i32,
    );
    if x < 0 || y < 0 || rw < 0 || rh < 0 || (x + rw) as u32 > w || (y + rh) as u32 > h {
        return None;
    }
    Some((x as u32, y as u32, rw as u32, rh as u32))
}

fn src_copy(c: &mut Ctx) -> u32 {
    let (this, prc, stride, size, buf) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((img, f)) = image_of(this) else {
        return E_FAIL;
    };
    let Some(r) = rect(prc, img.w, img.h) else {
        return E_INVALIDARG;
    };
    if buf == 0 {
        return E_POINTER;
    }
    if r.2 == 0 || r.3 == 0 {
        return 0;
    }
    let row = f.row_bytes(r.2);
    if stride < row || (size as u64) < stride as u64 * (r.3 - 1) as u64 + row as u64 {
        return WINCODEC_ERR_INSUFFICIENTBUFFER;
    }
    let mut out = mem::slice(buf, stride * (r.3 - 1) + row).to_vec();
    match raw_of(this) {
        Some(fr) if fr.fmt == f => {
            picture::copy_rect(f, &fr.bits, fr.stride(), r, stride, &mut out)
        }
        _ => encode(&img, f, r, stride, &mut out),
    }
    mem::write(buf, &out);
    0
}

// ---- decoders ----

fn decoder(data: &[u8]) -> Option<u32> {
    let pic = picture::decode(data)?;
    let container = match pic.kind {
        picture::Kind::Png => PNG,
        picture::Kind::Jpeg => JPEG,
        picture::Kind::Bmp => BMP,
        picture::Kind::Gif => GIF,
    };
    let d = new(
        "IWICBitmapDecoder",
        &[
            ("Release", release),
            ("GetContainerFormat", dec_container),
            ("GetFrameCount", dec_frame_count),
            ("GetFrame", dec_frame),
            ("CopyPalette", dec_palette),
            ("Initialize", ok),
        ],
        Obj::Decoder {
            pic: Arc::new(pic),
            container,
        },
    );
    Some(d)
}

fn picture_of(this: u32) -> Option<(Arc<picture::Picture>, Guid)> {
    with(|m| match m.get(&this) {
        Some(Obj::Decoder { pic, container }) => Some((pic.clone(), *container)),
        _ => None,
    })
}

fn dec_container(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    match picture_of(this) {
        Some((_, g)) if out != 0 => {
            write_guid(out, g);
            0
        }
        _ => E_INVALIDARG,
    }
}

fn dec_frame_count(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    let Some((pic, _)) = picture_of(this) else {
        return E_FAIL;
    };
    if out == 0 {
        return E_INVALIDARG;
    }
    mem::w32(out, pic.frames.len() as u32);
    0
}

fn dec_palette(c: &mut Ctx) -> u32 {
    let (this, pal) = (c.a(0), c.a(1));
    let Some((pic, _)) = picture_of(this) else {
        return E_FAIL;
    };
    set_palette(pal, pic.palette.clone(), 0)
}

/// GetFrame(this, index, out): a frame source in the frame's own pixel format
fn dec_frame(c: &mut Ctx) -> u32 {
    let (this, index, out) = (c.a(0), c.a(1), c.a(2));
    let Some((pic, _)) = picture_of(this) else {
        return E_FAIL;
    };
    let Some(f) = pic.frames.get(index as usize) else {
        return E_INVALIDARG;
    };
    let img = Img {
        w: f.w,
        h: f.h,
        px: f.bgra(),
        palette: f.palette.clone(),
        dpi: f.dpi,
    };
    let raw = picture::Frame {
        w: f.w,
        h: f.h,
        fmt: f.fmt,
        bits: f.bits.clone(),
        palette: f.palette.clone(),
        dpi: f.dpi,
    };
    let fmt = f.fmt;
    let p = new_source("IWICBitmapFrameDecode", Arc::new(img), fmt, &[]);
    with(|m| {
        if let Some(Obj::Source { raw: r, .. }) = m.get_mut(&p) {
            *r = Some(Arc::new(raw));
        }
    });
    mem::putp(out, p);
    0
}

fn raw_of(this: u32) -> Option<Arc<picture::Frame>> {
    with(|m| match m.get(&this) {
        Some(Obj::Source { raw, .. }) => raw.clone(),
        _ => None,
    })
}

fn create_decoder(data: &[u8], out: u32) -> u32 {
    match decoder(data) {
        Some(d) => {
            mem::putp(out, d);
            0
        }
        None => {
            eprintln!(
                "sake: WIC: no decoder for {} bytes ({:02x?})",
                data.len(),
                &data[..data.len().min(4)]
            );
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
    mem::putp(
        out,
        new(
            "IWICStream",
            &stream_impls(),
            Obj::Stream {
                data: Vec::new(),
                pos: 0,
            },
        ),
    );
    0
}

fn f_create_converter(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let empty = Arc::new(Img::new(0, 0, Vec::new()));
    mem::putp(
        out,
        new_source(
            "IWICFormatConverter",
            empty,
            Fmt::Bgra,
            &[("Initialize", conv_init), ("CanConvert", conv_can)],
        ),
    );
    0
}

fn set_source(this: u32, img: Arc<Img>, fmt: Fmt) {
    with(|m| {
        if let Some(Obj::Source {
            img: i,
            fmt: f,
            raw,
        }) = m.get_mut(&this)
        {
            *i = img;
            *f = fmt;
            *raw = None;
        }
    });
}

// ---- palettes ----

fn palette_colors(p: u32) -> Option<Vec<u32>> {
    with(|m| match m.get(&p) {
        Some(Obj::Palette { colors, .. }) => Some(colors.clone()),
        _ => None,
    })
}

fn f_create_palette(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return E_INVALIDARG;
    }
    let p = new(
        "IWICPalette",
        &[
            ("Release", release),
            ("InitializePredefined", pal_predefined),
            ("InitializeCustom", pal_custom),
            ("InitializeFromBitmap", pal_from_bitmap),
            ("InitializeFromPalette", pal_from_palette),
            ("GetType", pal_type),
            ("GetColorCount", pal_count),
            ("GetColors", pal_colors),
            ("IsBlackWhite", pal_is_bw),
            ("IsGrayscale", pal_is_gray),
            ("HasAlpha", pal_has_alpha),
        ],
        Obj::Palette {
            colors: Vec::new(),
            kind: 0,
        },
    );
    mem::putp(out, p);
    0
}

/// The colors of a WICBitmapPaletteType as Windows makes them: black and white (2), gray 4/16/256
/// (10, 11, 12), and the halftone color cubes (3..9: red slowest, blue fastest, levels rounded
/// half up) with the 16 system colors' extra 8 or the light gray after them; a transparent
/// black is added to the halftones if asked (replacing the last of 256).
fn predefined(kind: u32, transparent: bool) -> Option<Vec<u32>> {
    let level = |i: u32, n: u32| (i * 255 + (n - 1) / 2) / (n - 1);
    let gray = |n: u32| {
        (0..n)
            .map(|i| 0xff00_0000 | level(i, n) * 0x01_0101)
            .collect::<Vec<_>>()
    };
    let cube = |r: u32, g: u32, b: u32| {
        let mut v = Vec::new();
        for ri in 0..r {
            for gi in 0..g {
                for bi in 0..b {
                    v.push(0xff00_0000 | level(ri, r) << 16 | level(gi, g) << 8 | level(bi, b));
                }
            }
        }
        v
    };
    const EXTRA8: [u32; 8] = [
        0xffc0_c0c0,
        0xff80_8080,
        0xff80_0000,
        0xff00_8000,
        0xff00_0080,
        0xff80_8000,
        0xff80_0080,
        0xff00_8080,
    ];
    let with = |mut v: Vec<u32>, extra: &[u32]| {
        v.extend_from_slice(extra);
        v
    };
    let halftone = match kind {
        2 => return Some(vec![0xff00_0000, 0xffff_ffff]),
        10 => return Some(gray(4)),
        11 => return Some(gray(16)),
        12 => return Some(gray(256)),
        3 => with(cube(2, 2, 2), &EXTRA8),
        4 => with(cube(3, 3, 3), &EXTRA8[..1]),
        5 => with(cube(4, 4, 4), &EXTRA8),
        6 => with(cube(5, 5, 5), &EXTRA8[..1]),
        7 => with(cube(6, 6, 6), &EXTRA8),
        8 => cube(6, 7, 6),
        9 => cube(8, 8, 4),
        _ => return None,
    };
    let mut v = halftone;
    if transparent {
        if v.len() == 256 {
            v[255] = 0;
        } else {
            v.push(0);
        }
    }
    Some(v)
}

fn pal_set(this: u32, colors: Vec<u32>, kind: u32) -> u32 {
    with(|m| match m.get_mut(&this) {
        Some(Obj::Palette { colors: c, kind: k }) => {
            *c = colors;
            *k = kind;
            0
        }
        _ => E_INVALIDARG,
    })
}

fn pal_predefined(c: &mut Ctx) -> u32 {
    let (this, kind, transparent) = (c.a(0), c.a(1), c.a(2));
    let Some(v) = predefined(kind, transparent != 0) else {
        return E_INVALIDARG;
    };
    pal_set(this, v, kind)
}

fn pal_custom(c: &mut Ctx) -> u32 {
    let (this, p, n) = (c.a(0), c.a(1), c.a(2));
    if p == 0 && n != 0 || n > 256 {
        return E_INVALIDARG;
    }
    pal_set(this, (0..n).map(|i| mem::r32(p + 4 * i)).collect(), 0)
}

fn pal_from_palette(c: &mut Ctx) -> u32 {
    let (this, other) = (c.a(0), c.a(1));
    let Some((colors, kind)) = with(|m| match m.get(&other) {
        Some(Obj::Palette { colors, kind }) => Some((colors.clone(), *kind)),
        _ => None,
    }) else {
        return E_INVALIDARG;
    };
    pal_set(this, colors, kind)
}

/// InitializeFromBitmap(this, source, count, add transparent): the most used colors
fn pal_from_bitmap(c: &mut Ctx) -> u32 {
    let (this, src, n, transparent) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some((img, _)) = read_source(c, src) else {
        return E_INVALIDARG;
    };
    if !(2..=256).contains(&n) {
        return E_INVALIDARG;
    }
    let mut counts: HashMap<u32, u32> = HashMap::new();
    for p in img.px.chunks_exact(4) {
        *counts
            .entry(0xff00_0000 | (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32)
            .or_default() += 1;
    }
    let mut v: Vec<(u32, u32)> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let keep = n as usize - (transparent != 0) as usize;
    let mut colors: Vec<u32> = v.into_iter().take(keep).map(|(c, _)| c).collect();
    if transparent != 0 {
        colors.push(0);
    }
    pal_set(this, colors, 0)
}

fn pal_get<R>(this: u32, f: impl FnOnce(&[u32], u32) -> R) -> Option<R> {
    with(|m| match m.get(&this) {
        Some(Obj::Palette { colors, kind }) => Some(f(colors, *kind)),
        _ => None,
    })
}

fn pal_out(this: u32, out: u32, f: impl FnOnce(&[u32], u32) -> u32) -> u32 {
    if out == 0 {
        return E_INVALIDARG;
    }
    match pal_get(this, f) {
        Some(v) => {
            mem::w32(out, v);
            0
        }
        None => E_INVALIDARG,
    }
}

fn pal_type(c: &mut Ctx) -> u32 {
    pal_out(c.a(0), c.a(1), |_, k| k)
}

fn pal_count(c: &mut Ctx) -> u32 {
    pal_out(c.a(0), c.a(1), |v, _| v.len() as u32)
}

fn pal_colors(c: &mut Ctx) -> u32 {
    let (this, n, out, actual) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if out == 0 || actual == 0 {
        return E_INVALIDARG;
    }
    let Some(v) = pal_get(this, |v, _| v.to_vec()) else {
        return E_INVALIDARG;
    };
    let k = (n as usize).min(v.len());
    for (i, col) in v[..k].iter().enumerate() {
        mem::w32(out + 4 * i as u32, *col);
    }
    mem::w32(actual, k as u32);
    0
}

fn pal_is_bw(c: &mut Ctx) -> u32 {
    pal_out(c.a(0), c.a(1), |v, k| {
        (k == 2 || v == [0xff00_0000, 0xffff_ffff]) as u32
    })
}

fn pal_is_gray(c: &mut Ctx) -> u32 {
    pal_out(c.a(0), c.a(1), |v, k| {
        (matches!(k, 2 | 10 | 11 | 12)
            || k == 0
                && !v.is_empty()
                && v.iter().all(|c| {
                    let b = c.to_le_bytes();
                    b[0] == b[1] && b[1] == b[2]
                })) as u32
    })
}

fn pal_has_alpha(c: &mut Ctx) -> u32 {
    pal_out(c.a(0), c.a(1), |v, _| {
        v.iter().any(|c| c >> 24 != 255) as u32
    })
}

fn conv_init(c: &mut Ctx) -> u32 {
    // (this, source, dst format, dither, palette, double alpha threshold, palette type)
    let (this, src, fmt, pal) = (c.a(0), c.a(1), c.a(2), c.a(4));
    let Some(f) = fmt_of(read_guid(fmt)) else {
        eprintln!(
            "sake: WIC converter: unsupported format {:x?}",
            read_guid(fmt)
        );
        return WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT;
    };
    let Some((mut img, _)) = read_source(c, src) else {
        return E_INVALIDARG;
    };
    // (to an indexed format: with the palette given)
    if let Some(colors) = palette_colors(pal).filter(|p| !p.is_empty()) {
        let mut i = (*img).clone();
        i.palette = colors;
        img = Arc::new(i);
    } else if f.indexed() && img.palette.is_empty() {
        return E_INVALIDARG;
    }
    let raw = raw_of(src).filter(|r| r.fmt == f);
    set_source(this, img, f);
    with(|m| {
        if let Some(Obj::Source { raw: r, .. }) = m.get_mut(&this) {
            *r = raw;
        }
    });
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
    let empty = Arc::new(Img::new(0, 0, Vec::new()));
    mem::putp(
        out,
        new_source(
            "IWICBitmapScaler",
            empty,
            Fmt::Bgra,
            &[("Initialize", scaler_init)],
        ),
    );
    0
}

fn scaler_init(c: &mut Ctx) -> u32 {
    // (this, source, width, height, mode): bilinear, or nearest for mode 0
    let (this, src, w, h, mode) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some((img, f)) = read_source(c, src) else {
        return E_INVALIDARG;
    };
    if w == 0 || h == 0 {
        return E_INVALIDARG;
    }
    set_source(this, Arc::new(scale(&img, w, h, mode)), f);
    0
}

/// The source taps of each of `d` destination pixels from `s` for an interpolation mode:
/// linear (1) two taps at the pixel's center, cubic (2) Catmull-Rom's four, Fant (3) the
/// source area the pixel covers when shrinking (linear when growing).
fn taps(s: u32, d: u32, mode: u32) -> Vec<Vec<(u32, f32)>> {
    let ratio = s as f32 / d as f32;
    (0..d)
        .map(|i| {
            let mut t: Vec<(u32, f32)> = Vec::new();
            let mut add = |j: i64, w: f32| {
                let j = j.clamp(0, s as i64 - 1) as u32;
                match t.iter_mut().find(|e| e.0 == j) {
                    Some(e) => e.1 += w,
                    None => t.push((j, w)),
                }
            };
            if mode == 3 && s > d {
                let (lo, hi) = (i as f32 * ratio, (i + 1) as f32 * ratio);
                let mut j = lo.floor();
                while j < hi {
                    add(j as i64, ((j + 1.0).min(hi) - j.max(lo)) / ratio);
                    j += 1.0;
                }
                return t;
            }
            if mode == 1 {
                // Windows' bilinear: 16.16 positions, weights in 256ths (each pass floors)
                let step = ((s as u64) << 16) / d as u64;
                let p = (step / 2 + i as u64 * step).saturating_sub(0x8000);
                let x = ((p & 0xffff) >> 8) as f32;
                add((p >> 16) as i64, (256.0 - x) / 256.0);
                add((p >> 16) as i64 + 1, x / 256.0);
                return t;
            }
            let f = ((i as f32 + 0.5) * ratio - 0.5).max(0.0);
            let (j0, x) = (f.floor() as i64, f - f.floor());
            if mode == 2 {
                // Catmull-Rom
                let cr = |d: f32| {
                    let d = d.abs();
                    if d < 1.0 {
                        1.5 * d * d * d - 2.5 * d * d + 1.0
                    } else if d < 2.0 {
                        -0.5 * d * d * d + 2.5 * d * d - 4.0 * d + 2.0
                    } else {
                        0.0
                    }
                };
                for k in -1..=2 {
                    add(j0 + k, cr(x - k as f32));
                }
            } else {
                add(j0, 1.0 - x);
                add(j0 + 1, x);
            }
            t
        })
        .collect()
}

/// `img` scaled to `w x h` with a WICBitmapInterpolationMode, filtered premultiplied (as
/// Windows does: no color bleeds from transparent pixels) and straight again.
fn scale(img: &Img, w: u32, h: u32, mode: u32) -> Img {
    let mut px = vec![0u8; (w * h * 4) as usize];
    if img.px.is_empty() {
        return Img {
            w,
            h,
            px,
            palette: img.palette.clone(),
            dpi: img.dpi,
        };
    }
    let (sw, sh) = (img.w, img.h);
    let mut pm = vec![0u8; img.px.len()];
    picture::from_bgra(
        &img.px,
        img.w,
        Fmt::Pbgra,
        (0, 0, img.w, img.h),
        img.w * 4,
        &mut pm,
        &[],
    );
    if mode == 0 {
        // Windows' pick: (x * sw + sw - 1) / w
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (
                    ((x * sw + sw - 1) / w).min(sw - 1),
                    ((y * sh + sh - 1) / h).min(sh - 1),
                );
                let d = ((y * w + x) * 4) as usize;
                px[d..d + 4].copy_from_slice(&pm[((sy * sw + sx) * 4) as usize..][..4]);
            }
        }
    } else {
        // across, then down
        let (tx, ty) = (taps(sw, w, mode), taps(sh, h, mode));
        let mut mid = vec![0f32; (w * sh * 4) as usize];
        for y in 0..sh {
            for (x, t) in tx.iter().enumerate() {
                for k in 0..4 {
                    let v: f32 = t
                        .iter()
                        .map(|&(j, wt)| wt * pm[((y * sw + j) * 4) as usize + k] as f32)
                        .sum();
                    mid[((y * w + x as u32) * 4) as usize + k] =
                        if mode == 1 { v.floor() } else { v };
                }
            }
        }
        for (y, t) in ty.iter().enumerate() {
            for x in 0..w {
                let d = ((y as u32 * w + x) * 4) as usize;
                for k in 0..4 {
                    let v: f32 = t
                        .iter()
                        .map(|&(j, wt)| wt * mid[((j * w + x) * 4) as usize + k])
                        .sum();
                    px[d + k] = v.clamp(0.0, 255.0) as u8;
                }
                // (premultiplied: no channel above alpha)
                let a = px[d + 3];
                for k in 0..3 {
                    px[d + k] = px[d + k].min(a);
                }
            }
        }
    }
    let px = picture::to_bgra(Fmt::Pbgra, w, h, w * 4, &px, &[]);
    Img {
        w,
        h,
        px,
        palette: img.palette.clone(),
        dpi: img.dpi,
    }
}

fn crop(img: &Img, (x, y, w, h): (u32, u32, u32, u32)) -> Img {
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for row in y..y + h {
        px.extend_from_slice(&img.px[((row * img.w + x) * 4) as usize..][..(w * 4) as usize]);
    }
    Img {
        w,
        h,
        px,
        palette: img.palette.clone(),
        dpi: img.dpi,
    }
}

fn f_create_clipper(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let empty = Arc::new(Img::new(0, 0, Vec::new()));
    mem::putp(
        out,
        new_source(
            "IWICBitmapClipper",
            empty,
            Fmt::Bgra,
            &[("Initialize", clipper_init)],
        ),
    );
    0
}

fn clipper_init(c: &mut Ctx) -> u32 {
    let (this, src, prc) = (c.a(0), c.a(1), c.a(2));
    let Some((img, f)) = read_source(c, src) else {
        return E_INVALIDARG;
    };
    let Some(r) = rect(prc, img.w, img.h) else {
        return E_INVALIDARG;
    };
    set_source(this, Arc::new(crop(&img, r)), f);
    0
}

fn f_create_flip(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let empty = Arc::new(Img::new(0, 0, Vec::new()));
    mem::putp(
        out,
        new_source(
            "IWICBitmapFlipRotator",
            empty,
            Fmt::Bgra,
            &[("Initialize", flip_init)],
        ),
    );
    0
}

fn flip_init(c: &mut Ctx) -> u32 {
    // (this, source, options): rotate 90/180/270 (1..3), flip horizontal 8, vertical 16
    let (this, src, opt) = (c.a(0), c.a(1), c.a(2));
    let Some((img, f)) = read_source(c, src) else {
        return E_INVALIDARG;
    };
    let rot = opt & 3;
    let (w, h) = if rot & 1 == 1 {
        (img.h, img.w)
    } else {
        (img.w, img.h)
    };
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            // the source pixel this destination pixel shows: Windows flips the source, then
            // rotates (Rotate90 | FlipHorizontal puts the last source pixel first)
            let (mut sx, mut sy) = match rot {
                1 => (y, img.h - 1 - x),
                2 => (img.w - 1 - x, img.h - 1 - y),
                3 => (img.w - 1 - y, x),
                _ => (x, y),
            };
            if opt & 8 != 0 {
                sx = img.w - 1 - sx;
            }
            if opt & 16 != 0 {
                sy = img.h - 1 - sy;
            }
            let d = ((y * w + x) * 4) as usize;
            px[d..d + 4].copy_from_slice(&img.px[((sy * img.w + sx) * 4) as usize..][..4]);
        }
    }
    set_source(
        this,
        Arc::new(Img {
            w,
            h,
            px,
            palette: img.palette.clone(),
            dpi: img.dpi,
        }),
        f,
    );
    0
}

fn new_bitmap(img: Img, fmt: Fmt) -> u32 {
    let mut impls = source_impls();
    impls.extend_from_slice(&[
        ("Lock", bm_lock as crate::api::Handler),
        ("SetResolution", ok),
        ("SetPalette", bm_set_palette),
    ]);
    new(
        "IWICBitmap",
        &impls,
        Obj::Bitmap {
            img: Arc::new(Mutex::new(img)),
            fmt,
            raw: None,
        },
    )
}

fn bm_set_palette(c: &mut Ctx) -> u32 {
    let (this, p) = (c.a(0), c.a(1));
    let Some(colors) = palette_colors(p) else {
        return E_INVALIDARG;
    };
    with(|m| match m.get(&this) {
        Some(Obj::Bitmap { img, fmt, raw }) => {
            let mut i = img.lock().unwrap();
            // (an indexed bitmap made from memory: its pixels through the new palette)
            if let Some((data, stride)) = raw {
                let (w, h) = (i.w, i.h);
                i.px = picture::to_bgra(*fmt, w, h, *stride, data, &colors);
            }
            i.palette = colors;
            0
        }
        _ => E_FAIL,
    })
}

fn f_create_bitmap(c: &mut Ctx) -> u32 {
    // (this, width, height, format, option, out)
    let (w, h, fmt, out) = (c.a(1), c.a(2), c.a(3), c.a(5));
    let Some(f) = fmt_of(read_guid(fmt)) else {
        return WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT;
    };
    mem::putp(
        out,
        new_bitmap(Img::new(w, h, vec![0; (w * h * 4) as usize]), f),
    );
    0
}

fn f_bitmap_from_memory(c: &mut Ctx) -> u32 {
    // (this, width, height, format, stride, size, buffer, out)
    let (w, h, fmt, stride, size, buf, out) =
        (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6), c.a(7));
    let Some(f) = fmt_of(read_guid(fmt)) else {
        return WINCODEC_ERR_UNSUPPORTEDPIXELFORMAT;
    };
    let data = mem::slice(buf, size).to_vec();
    let b = new_bitmap(decode(f, w, h, stride, &data, &[]), f);
    if f.indexed() {
        with(|m| {
            if let Some(Obj::Bitmap { raw, .. }) = m.get_mut(&b) {
                *raw = Some((data, stride));
            }
        });
    }
    mem::putp(out, b);
    0
}

fn f_bitmap_from_source(c: &mut Ctx) -> u32 {
    // (this, source, option, out)
    let (src, out) = (c.a(1), c.a(3));
    let Some((img, f)) = read_source(c, src) else {
        return E_INVALIDARG;
    };
    mem::putp(out, new_bitmap(Img::new(img.w, img.h, img.px.clone()), f));
    0
}

fn f_bitmap_from_source_rect(c: &mut Ctx) -> u32 {
    // (this, source, x, y, width, height, out)
    let (src, x, y, w, h, out) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5), c.a(6));
    let Some((img, f)) = read_source(c, src) else {
        return E_INVALIDARG;
    };
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
        Some(Obj::Bitmap { img, fmt, .. }) => Some((img.clone(), *fmt)),
        _ => None,
    }) else {
        return E_FAIL;
    };
    let (w, h) = {
        let i = bitmap.lock().unwrap();
        (i.w, i.h)
    };
    let Some(r) = rect(prc, w, h) else {
        return E_INVALIDARG;
    };
    let stride = fmt.row_bytes(r.2).div_ceil(4) * 4;
    let buf = mem::heap_alloc((stride * r.3).max(4), true);
    let mut data = vec![0u8; (stride * r.3) as usize];
    encode(&bitmap.lock().unwrap(), fmt, r, stride, &mut data);
    mem::write(buf, &data);
    let l = new(
        "IWICBitmapLock",
        &[
            ("Release", release),
            ("GetSize", lock_size),
            ("GetStride", lock_stride),
            ("GetDataPointer", lock_data),
            ("GetPixelFormat", lock_format),
        ],
        Obj::Lock {
            bitmap,
            fmt,
            rect: r,
            buf,
            stride,
            write: flags & 2 != 0,
        },
    );
    mem::putp(out, l);
    0
}

fn lock<R>(this: u32, f: impl FnOnce(Fmt, (u32, u32, u32, u32), u32, u32) -> R) -> Option<R> {
    with(|m| match m.get(&this) {
        Some(Obj::Lock {
            fmt,
            rect,
            buf,
            stride,
            ..
        }) => Some(f(*fmt, *rect, *buf, *stride)),
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

fn info_fmt(this: u32) -> Option<Fmt> {
    with(|m| match m.get(&this) {
        Some(Obj::FormatInfo(f)) => Some(*f),
        _ => None,
    })
}
/// A pixel format's channels: indexed, gray and alpha-only formats have one, CMYK four.
fn channels(f: Fmt) -> u32 {
    use Fmt::*;
    match f {
        Idx1 | Idx2 | Idx4 | Idx8 | BlackWhite | Gray2 | Gray4 | Gray8 | Gray16 | Alpha8 => 1,
        Bgr555 | Bgr565 | Bgr24 | Rgb24 | Bgr32 | Rgb32 | Rgb48 => 3,
        Bgra | Pbgra | Rgba | Prgba | Rgba64 | Cmyk => 4,
    }
}
/// A u32 answer of a pixel format info (out at the second argument).
fn info_u32(c: &mut Ctx, v: impl FnOnce(Fmt) -> u32) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_INVALIDARG;
    }
    let Some(f) = info_fmt(this) else {
        return E_INVALIDARG;
    };
    mem::w32(out, v(f));
    0
}
fn pfi_type(c: &mut Ctx) -> u32 {
    info_u32(c, |_| 0x20) // WICPixelFormat (wincodec.h WICComponentType)
}
fn pfi_guid(c: &mut Ctx) -> u32 {
    let (this, out) = (c.a(0), c.a(1));
    if out == 0 {
        return E_INVALIDARG;
    }
    let Some(f) = info_fmt(this) else {
        return E_INVALIDARG;
    };
    write_guid(out, fmt_guid(f));
    0
}
fn pfi_bits(c: &mut Ctx) -> u32 {
    info_u32(c, |f| f.bits())
}
fn pfi_channels(c: &mut Ctx) -> u32 {
    info_u32(c, channels)
}
fn pfi_transparency(c: &mut Ctx) -> u32 {
    info_u32(c, transparency)
}
fn pfi_numeric(c: &mut Ctx) -> u32 {
    info_u32(c, numeric)
}
/// WICPixelFormatNumericRepresentation: Indexed 1, UnsignedInteger 2 (3 is SignedInteger);
/// Windows answers UnsignedInteger for every non-indexed format here, CMYK and alpha too
/// (winref wicconv info lines)
fn numeric(f: Fmt) -> u32 {
    if f.indexed() {
        1
    } else {
        2
    }
}
/// SupportsTransparency: formats with an alpha channel; the indexed ones answer FALSE on
/// Windows (a palette entry's alpha does not count: winref wicconv)
fn transparency(f: Fmt) -> u32 {
    use Fmt::*;
    matches!(f, Bgra | Pbgra | Rgba | Prgba | Rgba64 | Alpha8) as u32
}

/// CreateComponentInfo(clsid, out): pixel formats only (IWICPixelFormatInfo2); other components
/// (codecs, converters): WINCODEC_ERR_COMPONENTNOTFOUND.
fn f_component_info(c: &mut Ctx) -> u32 {
    let (clsid, out) = (c.a(1), c.a(2));
    if clsid == 0 || out == 0 {
        return E_INVALIDARG;
    }
    // (Windows leaves *out as it was for a component it does not know: winref wicconv)
    let Some(f) = fmt_of(read_guid(clsid)) else {
        return WINCODEC_ERR_COMPONENTNOTFOUND;
    };
    let p = new(
        "IWICPixelFormatInfo2",
        &[
            ("Release", release),
            ("GetComponentType", pfi_type),
            ("GetCLSID", pfi_guid),
            ("GetFormatGUID", pfi_guid),
            ("GetBitsPerPixel", pfi_bits),
            ("GetChannelCount", pfi_channels),
            ("SupportsTransparency", pfi_transparency),
            ("GetNumericRepresentation", pfi_numeric),
        ],
        Obj::FormatInfo(f),
    );
    mem::putp(out, p);
    0
}

/// CoCreateInstance of the imaging factory (IWICImagingFactory and IWICImagingFactory2).
pub fn factory() -> u32 {
    com::object(
        "IWICImagingFactory2",
        &[
            ("CreateDecoderFromFilename", f_decoder_from_filename),
            ("CreateDecoderFromStream", f_decoder_from_stream),
            ("CreateComponentInfo", f_component_info),
            ("CreateFormatConverter", f_create_converter),
            ("CreatePalette", f_create_palette),
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
    fn scale_and_crop() {
        let img = Img::new(2, 1, vec![0, 0, 0, 255, 200, 200, 200, 255]);
        let s = scale(&img, 4, 2, 0);
        assert_eq!((s.w, s.h), (4, 2));
        assert_eq!(&s.px[0..4], &[0, 0, 0, 255]);
        assert_eq!(&s.px[12..16], &[200, 200, 200, 255]);
        let c = crop(&img, (1, 0, 1, 1));
        assert_eq!(c.px, vec![200, 200, 200, 255]);
    }

    /// Every pixel format's IWICPixelFormatInfo2 answers (bits, channels, transparency,
    /// numeric representation) as Windows gave them to the winref wicconv probe.
    #[test]
    fn pixel_format_info_as_windows() {
        use Fmt::*;
        let names = [
            ("BlackWhite", BlackWhite),
            ("2bppGray", Gray2),
            ("4bppGray", Gray4),
            ("8bppGray", Gray8),
            ("16bppGray", Gray16),
            ("16bppBGR555", Bgr555),
            ("16bppBGR565", Bgr565),
            ("24bppBGR", Bgr24),
            ("24bppRGB", Rgb24),
            ("32bppBGR", Bgr32),
            ("32bppBGRA", Bgra),
            ("32bppPBGRA", Pbgra),
            ("32bppRGB", Rgb32),
            ("32bppRGBA", Rgba),
            ("32bppPRGBA", Prgba),
            ("48bppRGB", Rgb48),
            ("64bppRGBA", Rgba64),
            ("32bppCMYK", Cmyk),
            ("8bppAlpha", Alpha8),
            ("1bppIndexed", Idx1),
            ("2bppIndexed", Idx2),
            ("4bppIndexed", Idx4),
            ("8bppIndexed", Idx8),
        ];
        for golden in [
            include_str!("../../../tests/winref/golden/wicconv-32/stdout.txt"),
            include_str!("../../../tests/winref/golden/wicconv-64/stdout.txt"),
        ] {
            let mut seen = 0;
            for l in golden
                .lines()
                .filter(|l| l.starts_with("info ") && !l.starts_with("info unknown"))
            {
                let w: Vec<&str> = l.split_whitespace().collect();
                let f = names
                    .iter()
                    .find(|n| n.0 == w[1])
                    .unwrap_or_else(|| panic!("{l}"))
                    .1;
                let field = |k: &str| {
                    w[w.iter().position(|x| *x == k).unwrap() + 1]
                        .parse::<u32>()
                        .unwrap()
                };
                assert_eq!(field("bits"), f.bits(), "{l}");
                assert_eq!(field("channels"), channels(f), "{l}");
                assert_eq!(field("transparency"), transparency(f), "{l}");
                assert_eq!(field("numeric"), numeric(f), "{l}");
                seen += 1;
            }
            assert_eq!(seen, picture::ALL.len());
        }
    }

    #[test]
    fn predefined_palettes() {
        assert_eq!(predefined(2, false).unwrap(), [0xff00_0000, 0xffff_ffff]);
        assert_eq!(
            predefined(10, true).unwrap(),
            [0xff00_0000, 0xff55_5555, 0xffaa_aaaa, 0xffff_ffff]
        );
        assert_eq!(predefined(3, true).unwrap().len(), 17);
        assert_eq!(predefined(8, false).unwrap()[251], 0xffff_ffff);
        assert_eq!(predefined(12, false).unwrap().len(), 256);
        assert_eq!(predefined(9, false).unwrap().len(), 256);
    }
}
