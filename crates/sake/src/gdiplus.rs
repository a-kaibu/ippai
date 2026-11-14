//! GDI+ images (gdiplus.dll): bitmaps decoded from a file or an IStream (PNG, JPEG, BMP via
//! picture.rs), their size, pixel format, pixels (LockBits, GetPixel) and an HBITMAP of them.
//! Enough for the games and launchers that load their pictures with GDI+; no drawing.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx, Handler};
use crate::mem::{self, put32, r32, w32};
use crate::HashMap;
use std::sync::Mutex;

const OK: u32 = 0;
const INVALID_PARAMETER: u32 = 2;
const FILE_NOT_FOUND: u32 = 10;
const UNKNOWN_IMAGE_FORMAT: u32 = 13;

const FMT_24RGB: u32 = 0x0002_1808;
const FMT_32RGB: u32 = 0x0002_2009;
const FMT_32ARGB: u32 = 0x0026_200a;
const FMT_32PARGB: u32 = 0x000e_200b;
const FMT_16RGB555: u32 = 0x0002_1005;
const FMT_16RGB565: u32 = 0x0002_1006;

fn bytes_per_pixel(fmt: u32) -> u32 {
    match fmt {
        FMT_24RGB => 3,
        FMT_16RGB555 | FMT_16RGB565 => 2,
        _ => 4,
    }
}

/// BGRA -> 16-bit pixel and back (5-bit channels widened by bit replication)
fn pack16(fmt: u32, [b, g, r]: [u8; 3]) -> u16 {
    let (b, r) = ((b >> 3) as u16, (r >> 3) as u16);
    if fmt == FMT_16RGB565 { r << 11 | ((g >> 2) as u16) << 5 | b } else { r << 10 | ((g >> 3) as u16) << 5 | b }
}
fn unpack16(fmt: u32, v: u16) -> [u8; 3] {
    let x5 = |c: u16| ((c << 3) | (c >> 2)) as u8;
    let b = x5(v & 31);
    if fmt == FMT_16RGB565 {
        let g = (v >> 5) & 63;
        [b, ((g << 2) | (g >> 4)) as u8, x5(v >> 11)]
    } else {
        [b, x5((v >> 5) & 31), x5((v >> 10) & 31)]
    }
}

pub(crate) fn is_image(h: u32) -> bool {
    with(|m| m.contains_key(&h))
}

/// Runs `f(width, height, bgra)` over an image's pixels (for gdiplus_text's Graphics).
pub(crate) fn with_pixels(h: u32, f: impl FnOnce(u32, u32, &mut [u8])) {
    with(|m| {
        if let Some(b) = m.get_mut(&h) {
            if b.fmt != FMT_32PARGB {
                return f(b.w, b.h, &mut b.px);
            }
            // (a PARGB bitmap: drawn on straight, kept premultiplied)
            let mut px: Vec<u8> = (0..b.px.len() / 4).flat_map(|i| b.straight(i)).collect();
            f(b.w, b.h, &mut px);
            for (i, p) in px.chunks(4).enumerate() {
                b.store(i, [p[0], p[1], p[2], p[3]]);
            }
        }
    })
}

/// An image's straight BGRA pixels, top row first (for gdiplus_brush's textures)
pub(crate) fn pixels(h: u32) -> Option<(u32, u32, Vec<u8>)> {
    with(|m| {
        let b = m.get(&h)?;
        Some((b.w, b.h, (0..(b.w * b.h) as usize).flat_map(|i| b.straight(i)).collect()))
    })
}

/// A new w x h bitmap of `fmt`, zeroed (gdiplus_text's GdipCreateBitmapFromGraphics)
pub(crate) fn new_bitmap(w: u32, h: u32, fmt: u32, out: u32) -> u32 {
    if w == 0 || h == 0 || out == 0 || w.checked_mul(h).is_none_or(|n| n > 1 << 26) {
        return INVALID_PARAMETER;
    }
    let alpha = if has_alpha(fmt) { 0 } else { 0xff };
    let mut px = vec![0u8; (w * h * 4) as usize];
    for p in px.chunks_mut(4) {
        p[3] = alpha;
    }
    let flags = if has_alpha(fmt) { FLAG_HAS_ALPHA } else { 0 };
    add(Bitmap { w, h, px, fmt, flags, palette: Vec::new(), idx: Vec::new(), locked: None }, out)
}

struct Bitmap {
    w: u32,
    h: u32,
    /// BGRA, top row first, straight alpha; but a PARGB bitmap keeps its premultiplied bytes
    /// (unpremultiplying and premultiplying again does not always give them back), a 32bppRGB
    /// one its fourth byte as stored (locks in 32bppRGB show it; GetPixel and the alpha
    /// formats say 255), a 16-bit one its colors as quantized.
    px: Vec<u8>,
    fmt: u32, // the PixelFormat GDI+ reports for it
    /// GdipGetImageFlags
    flags: u32,
    /// an indexed bitmap's colors and each pixel's index
    palette: Vec<u32>,
    idx: Vec<u8>,
    locked: Option<(u32, u32, u32, u32, u32)>, // buffer, x, y, w, format (to write back)
}

const FMT_1IDX: u32 = 0x0003_0101;
const FMT_4IDX: u32 = 0x0003_0402;
const FMT_8IDX: u32 = 0x0003_0803;
const WRONG_STATE: u32 = 8;
const WIN32_ERROR: u32 = 7;

/// ImageFlags
const FLAG_HAS_ALPHA: u32 = 2;
const FLAG_RGB: u32 = 0x10;
const FLAG_GRAY: u32 = 0x40;
const FLAG_YCBCR: u32 = 0x100;
const FLAG_REAL_DPI: u32 = 0x1000;
const FLAG_REAL_SIZE: u32 = 0x2000;
const FLAG_READ_ONLY: u32 = 0x10000;

fn has_alpha(fmt: u32) -> bool {
    fmt & 0x0004_0000 != 0
}
fn indexed(fmt: u32) -> bool {
    fmt & 0x0001_0000 != 0
}

/// GDI+'s premultiplication (winref gdip, every case of the probe): c * a / 255 rounded
pub(crate) fn premul(c: u8, a: u8) -> u8 {
    ((c as u32 * a as u32 + 127) / 255) as u8
}
/// and its inverse: c times the reciprocal 0xff00 / a, truncated (a = 127: 127 -> 254)
pub(crate) fn unpremul(c: u8, a: u8) -> u8 {
    if a == 0 {
        return 0;
    }
    ((c as u32 * (0xff00 / a as u32)) >> 8).min(255) as u8
}

static IMAGES: Mutex<Option<HashMap<u32, Bitmap>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Bitmap>) -> R) -> R {
    f(IMAGES.lock().unwrap().get_or_insert_with(HashMap::default))
}

fn add(b: Bitmap, out: u32) -> u32 {
    let o = mem::heap_alloc(16, true);
    with(|m| m.insert(o, b));
    mem::putp(out, o);
    OK
}

/// A decoded file's ImageFlags (winref gdip: BMP 0x13010, PNG with alpha 0x12012, 16-bit gray
/// 0x12042): read-only, its real pixel size, its DPI when the file says it (a BMP always, a
/// PNG with pHYs, a JPEG with JFIF density), the color space, alpha
fn decoded_flags(pic: &crate::picture::Picture, data: &[u8], fmt: u32) -> u32 {
    use crate::picture::{Fmt, Kind};
    let f = &pic.frames[0];
    let gray = matches!(f.fmt, Fmt::Gray2 | Fmt::Gray4 | Fmt::Gray8 | Fmt::Gray16);
    let dpi = match pic.kind {
        Kind::Bmp => true,
        Kind::Png => data.windows(4).any(|w| w == b"pHYs"),
        Kind::Jpeg => data.get(13).is_some_and(|&u| data.get(6..10) == Some(b"JFIF") && u != 0),
        Kind::Gif => false,
    };
    let space = match (pic.kind, gray) {
        (_, true) => FLAG_GRAY,
        (Kind::Jpeg, false) => FLAG_YCBCR,
        _ => FLAG_RGB,
    };
    FLAG_READ_ONLY | FLAG_REAL_SIZE | if dpi { FLAG_REAL_DPI } else { 0 } | space | if has_alpha(fmt) { FLAG_HAS_ALPHA } else { 0 }
}

fn new_image(data: &[u8], out: u32, bad: u32) -> u32 {
    let Some(pic) = crate::picture::decode(data) else { return bad };
    let fmt = pic.gdiplus_format();
    let flags = decoded_flags(&pic, data, fmt);
    let f = &pic.frames[0];
    let (w, h, mut px) = (f.w, f.h, f.bgra());
    // (a 32-bit BMP is 32bppRGB keeping each pixel's fourth byte: a lock in 32bppRGB shows it)
    if fmt == FMT_32RGB && f.fmt == crate::picture::Fmt::Bgr32 {
        let stride = f.stride() as usize;
        for y in 0..h as usize {
            for x in 0..w as usize {
                px[4 * (y * w as usize + x) + 3] = f.bits[y * stride + 4 * x + 3];
            }
        }
    }
    // (an indexed bitmap: its palette and indices, from a palette file)
    let (palette, idx) = if indexed(fmt) && matches!(f.fmt, crate::picture::Fmt::Idx1 | crate::picture::Fmt::Idx2 | crate::picture::Fmt::Idx4 | crate::picture::Fmt::Idx8) {
        let bits = f.fmt.bits();
        let stride = f.stride() as usize;
        let idx = (0..h as usize).flat_map(|y| (0..w).map(move |x| (y, x))).map(|(y, x)| get_bits(&f.bits[y * stride..], x, bits) as u8).collect();
        (f.palette.clone(), idx)
    } else {
        (Vec::new(), Vec::new())
    };
    add(Bitmap { w, h, px, fmt, flags, palette, idx, locked: None }, out)
}

fn get_bits(row: &[u8], x: u32, bits: u32) -> u32 {
    let bit = x * bits;
    let b = row.get((bit / 8) as usize).copied().unwrap_or(0) as u32;
    (b >> (8 - bits - bit % 8)) & ((1 << bits) - 1)
}

fn GdiplusStartup(c: &mut Ctx) -> u32 {
    crate::mem::putp(c.a(0), 1);
    if c.a(2) != 0 {
        mem::fill(c.a(2), 8, 0);
    }
    OK
}
fn ok(_: &mut Ctx) -> u32 {
    OK
}

/// GdipCreateBitmapFromStream / GdipLoadImageFromStream (+ICM)(stream, &image): a stream
/// that is no picture is InvalidParameter (winref gdip)
fn from_stream(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 || c.a(1) == 0 {
        return INVALID_PARAMETER;
    }
    // (the out pointer read first: on x64 calling the stream's Read clobbers the argument registers)
    let out = c.a(1);
    let data = crate::wic::read_stream(c, c.a(0));
    new_image(&data, out, INVALID_PARAMETER)
}
/// GdipCreateBitmapFromFile / GdipLoadImageFromFile (+ICM)(wide path, &image)
fn from_file(c: &mut Ctx) -> u32 {
    let name = String::from_utf16_lossy(&mem::wstr(c.a(0)));
    let Some(data) = crate::files::resolve(&name).and_then(|h| kura::fs::read(h).ok()) else { return FILE_NOT_FOUND };
    new_image(&data, c.a(1), UNKNOWN_IMAGE_FORMAT)
}

pub(crate) fn size_of(img: u32) -> Option<(u32, u32)> {
    with(|m| m.get(&img).map(|b| (b.w, b.h)))
}
fn GdipGetImageWidth(c: &mut Ctx) -> u32 {
    let Some((w, _)) = size_of(c.a(0)) else { return INVALID_PARAMETER };
    put32(c.a(1), w);
    OK
}
fn GdipGetImageHeight(c: &mut Ctx) -> u32 {
    let Some((_, h)) = size_of(c.a(0)) else { return INVALID_PARAMETER };
    put32(c.a(1), h);
    OK
}
fn GdipGetImageDimension(c: &mut Ctx) -> u32 {
    let Some((w, h)) = size_of(c.a(0)) else { return INVALID_PARAMETER };
    put32(c.a(1), (w as f32).to_bits());
    put32(c.a(2), (h as f32).to_bits());
    OK
}
fn GdipGetImagePixelFormat(c: &mut Ctx) -> u32 {
    let Some(f) = with(|m| m.get(&c.a(0)).map(|b| b.fmt)) else { return INVALID_PARAMETER };
    put32(c.a(1), f);
    OK
}
fn GdipGetImageType(c: &mut Ctx) -> u32 {
    put32(c.a(1), 1); // ImageTypeBitmap
    OK
}
fn GdipGetImageFlags(c: &mut Ctx) -> u32 {
    let Some(f) = with(|m| m.get(&c.a(0)).map(|b| b.flags)) else { return INVALID_PARAMETER };
    put32(c.a(1), f);
    OK
}
/// A ColorPalette: flags, count, the ARGB entries (none: room for one all the same)
fn palette_of(img: u32) -> Option<Vec<u32>> {
    with(|m| m.get(&img).map(|b| b.palette.clone()))
}
fn GdipGetImagePaletteSize(c: &mut Ctx) -> u32 {
    let Some(p) = palette_of(c.a(0)) else { return INVALID_PARAMETER };
    put32(c.a(1), 8 + 4 * (p.len() as u32).max(1));
    OK
}
fn GdipGetImagePalette(c: &mut Ctx) -> u32 {
    let Some(p) = palette_of(c.a(0)) else { return INVALID_PARAMETER };
    let (out, size) = (c.a(1), c.a(2));
    if out == 0 || size < 8 + 4 * (p.len() as u32).max(1) {
        return INVALID_PARAMETER;
    }
    mem::fill(out, 12, 0);
    w32(out + 4, p.len() as u32);
    for (i, &v) in p.iter().enumerate() {
        w32(out + 8 + 4 * i as u32, v);
    }
    OK
}

/// The decoders sake has (GdipGetImageDecoders): ImageCodecInfo = CLSID, format GUID, five
/// strings (name, DLL, description, extensions, MIME type), flags, version, signature count and
/// size, signature patterns and masks
const DECODERS: [(u32, &str, &str, &str, &str, &[u8]); 4] = [
    (0xb96b3cab, "Built-in BMP Codec", "BMP", "*.BMP;*.DIB;*.RLE", "image/bmp", b"BM"),
    (0xb96b3cae, "Built-in JPEG Codec", "JPEG", "*.JPG;*.JPEG;*.JPE;*.JFIF", "image/jpeg", b"\xff\xd8"),
    (0xb96b3cb0, "Built-in GIF Codec", "GIF", "*.GIF", "image/gif", b"GIF8"),
    (0xb96b3caf, "Built-in PNG Codec", "PNG", "*.PNG", "image/png", b"\x89PNG\r\n\x1a\n"),
];
/// The whole GdipGetImageDecoders answer for a buffer at `base` (pointer size `ps`): the
/// ImageCodecInfo structs, then each codec's five strings and its signature pattern and mask.
/// GdipGetImageDecodersSize reports its length, so the two can't disagree (#162: the DLL name
/// was written but not counted, and the 88 bytes past the game's buffer were its next object).
pub(crate) fn decoders_blob(base: u64, ps: u32) -> Vec<u8> {
    let one = (32 + 7 * ps + 16) as usize;
    let mut out = vec![0u8; one * DECODERS.len()];
    let ptr = |out: &mut Vec<u8>, at: usize, v: u64| out[at..at + ps as usize].copy_from_slice(&v.to_le_bytes()[..ps as usize]);
    for (i, d) in DECODERS.iter().enumerate() {
        let p = one * i;
        // the GUIDs' common tail: {xxxxxxxx-1a04-11d3-9a73-0000f81ef32e} (codec) / formats {b96b3c..-0728-11d3-9d7b-0000f81ef32e}
        let codec = d.0 - 0xb96b3cab + 0x557cf400;
        out[p..p + 4].copy_from_slice(&codec.to_le_bytes());
        out[p + 4..p + 16].copy_from_slice(&[0x04, 0x1a, 0xd3, 0x11, 0x9a, 0x73, 0x00, 0x00, 0xf8, 0x1e, 0xf3, 0x2e]);
        out[p + 16..p + 20].copy_from_slice(&d.0.to_le_bytes());
        out[p + 20..p + 32].copy_from_slice(&[0x28, 0x07, 0xd3, 0x11, 0x9d, 0x7b, 0x00, 0x00, 0xf8, 0x1e, 0xf3, 0x2e]);
        for (k, s) in [d.1, "gdiplus.dll", d.2, d.3, d.4].into_iter().enumerate() {
            let at = base + out.len() as u64;
            ptr(&mut out, p + 32 + ps as usize * k, at);
            out.extend(s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()));
        }
        let q = p + 32 + 5 * ps as usize;
        // flags: Decoder | SupportBitmap | Builtin; version 1; one signature
        for (k, v) in [0x0001_0002u32 | 0x0000_0004, 1, 1, d.5.len() as u32].into_iter().enumerate() {
            out[q + 4 * k..q + 4 * k + 4].copy_from_slice(&v.to_le_bytes());
        }
        let at = base + out.len() as u64;
        ptr(&mut out, q + 16, at);
        ptr(&mut out, q + 16 + ps as usize, at + d.5.len() as u64);
        out.extend_from_slice(d.5);
        out.extend(std::iter::repeat_n(0xff, d.5.len()));
    }
    out
}
fn GdipGetImageDecodersSize(c: &mut Ctx) -> u32 {
    put32(c.a(0), DECODERS.len() as u32);
    put32(c.a(1), decoders_blob(0, mem::ps()).len() as u32);
    OK
}
fn GdipGetImageDecoders(c: &mut Ctx) -> u32 {
    let (n, size, out) = (c.a(0), c.a(1), c.a(2));
    let blob = decoders_blob(out as u64, mem::ps());
    if n != DECODERS.len() as u32 || size != blob.len() as u32 || out == 0 {
        return INVALID_PARAMETER;
    }
    mem::write(out, &blob);
    OK
}
fn GdipGetImageEncodersSize(c: &mut Ctx) -> u32 {
    put32(c.a(0), 0);
    put32(c.a(1), 0);
    OK
}
fn GdipDisposeImage(c: &mut Ctx) -> u32 {
    if with(|m| m.remove(&c.a(0))).is_some() {
        mem::heap_free(c.a(0));
    }
    OK
}
fn GdipCloneImage(c: &mut Ctx) -> u32 {
    let Some(b) = with(|m| m.get(&c.a(0)).map(|b| Bitmap { w: b.w, h: b.h, px: b.px.clone(), fmt: b.fmt, flags: b.flags, palette: b.palette.clone(), idx: b.idx.clone(), locked: None })) else { return INVALID_PARAMETER };
    add(b, c.a(1))
}

impl Bitmap {
    /// pixel i as straight BGRA (32bppRGB: opaque)
    fn straight(&self, i: usize) -> [u8; 4] {
        let p = &self.px[4 * i..4 * i + 4];
        match self.fmt {
            FMT_32RGB => [p[0], p[1], p[2], 0xff],
            FMT_32PARGB => [unpremul(p[0], p[3]), unpremul(p[1], p[3]), unpremul(p[2], p[3]), p[3]],
            _ => [p[0], p[1], p[2], p[3]],
        }
    }
    /// stores straight BGRA as the bitmap's own format keeps it
    fn store(&mut self, i: usize, [b, g, r, a]: [u8; 4]) {
        let v = match self.fmt {
            FMT_32PARGB => [premul(b, a), premul(g, a), premul(r, a), a],
            FMT_24RGB => [b, g, r, 0xff],
            FMT_16RGB555 | FMT_16RGB565 => {
                let [b, g, r] = unpack16(self.fmt, pack16(self.fmt, [b, g, r]));
                [b, g, r, 0xff]
            }
            _ => [b, g, r, a],
        };
        self.px[4 * i..4 * i + 4].copy_from_slice(&v);
    }
    /// the nearest palette entry (an indexed bitmap written through another format)
    fn nearest(&self, [b, g, r, a]: [u8; 4]) -> u8 {
        let d = |c: u32| {
            let [cb, cg, cr, ca] = c.to_le_bytes();
            let e = |x: u8, y: u8| (x as i32 - y as i32).pow(2);
            e(cb, b) + e(cg, g) + e(cr, r) + e(ca, a)
        };
        (0..self.palette.len()).min_by_key(|&k| d(self.palette[k])).unwrap_or(0) as u8
    }
}

/// GdipBitmapGetPixel(bitmap, x, y, &argb)
fn GdipBitmapGetPixel(c: &mut Ctx) -> u32 {
    let (x, y) = (c.a(1), c.a(2));
    let p = with(|m| {
        let b = m.get(&c.a(0))?;
        (x < b.w && y < b.h).then(|| u32::from_le_bytes(b.straight((y * b.w + x) as usize)))
    });
    let Some(p) = p else { return INVALID_PARAMETER };
    put32(c.a(3), p);
    OK
}
/// GdipBitmapSetPixel(bitmap, x, y, argb): kept as the format keeps it (a PARGB bitmap reads
/// back 0x80ff8040 as 0x80ff7f3f, 555 quantizes; winref gdip)
fn GdipBitmapSetPixel(c: &mut Ctx) -> u32 {
    let (x, y, v) = (c.a(1), c.a(2), c.a(3));
    with(|m| {
        let Some(b) = m.get_mut(&c.a(0)) else { return INVALID_PARAMETER };
        if x >= b.w || y >= b.h || indexed(b.fmt) {
            return INVALID_PARAMETER;
        }
        b.store((y * b.w + x) as usize, v.to_le_bytes());
        OK
    })
}

fn lock_bits(fmt: u32) -> u32 {
    match fmt {
        FMT_1IDX => 1,
        FMT_4IDX => 4,
        FMT_8IDX => 8,
        _ => 8 * bytes_per_pixel(fmt),
    }
}

/// GdipBitmapLockBits(bitmap, GpRect*, flags, format, BitmapData*): BitmapData = width, height,
/// stride, format, scan0, reserved. As Windows converts (winref gdip): PARGB premultiplies
/// (rounded), 32bppRGB keeps the fourth byte, 24/16-bit drop alpha, an indexed bitmap in its
/// own format gives its indices; a second lock is WrongState.
fn GdipBitmapLockBits(c: &mut Ctx) -> u32 {
    let (bmp, rect, flags, mut fmt, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let r = with(|m| {
        let b = m.get_mut(&bmp).ok_or(INVALID_PARAMETER)?;
        let (x, y, w, h) = if rect != 0 { (r32(rect), r32(rect + 4), r32(rect + 8), r32(rect + 12)) } else { (0, 0, b.w, b.h) };
        if x.checked_add(w).is_none_or(|e| e > b.w) || y.checked_add(h).is_none_or(|e| e > b.h) {
            return Err(INVALID_PARAMETER);
        }
        if b.locked.is_some() {
            return Err(WRONG_STATE);
        }
        let own_index = indexed(fmt) && fmt == b.fmt && !b.idx.is_empty();
        if !own_index && ![FMT_24RGB, FMT_32RGB, FMT_32ARGB, FMT_32PARGB, FMT_16RGB555, FMT_16RGB565].contains(&fmt) {
            fmt = FMT_32ARGB;
        }
        let bits = lock_bits(fmt);
        let stride = ((w * bits).div_ceil(8) + 3) & !3;
        let buf = mem::heap_alloc(stride * h.max(1), true);
        let mut row = vec![0u8; stride as usize];
        for j in 0..h {
            row.fill(0);
            for i in 0..w {
                let s = ((y + j) * b.w + x + i) as usize;
                if own_index {
                    let v = b.idx[s] as u32;
                    let bit = i * bits;
                    row[(bit / 8) as usize] |= (v << (8 - bits - bit % 8)) as u8;
                    continue;
                }
                let [bl, g, rd, a] = b.straight(s);
                let d = (i * bits / 8) as usize;
                match fmt {
                    FMT_24RGB => row[d..d + 3].copy_from_slice(&[bl, g, rd]),
                    FMT_16RGB555 | FMT_16RGB565 => row[d..d + 2].copy_from_slice(&pack16(fmt, [bl, g, rd]).to_le_bytes()),
                    // (the fourth byte as the bitmap has it)
                    // (the fourth byte as the bitmap has it; a PARGB one's own bytes)
                    FMT_32RGB | FMT_32PARGB if b.fmt == fmt => row[d..d + 4].copy_from_slice(&b.px[4 * s..4 * s + 4]),
                    FMT_32RGB => row[d..d + 4].copy_from_slice(&b.px[4 * s..4 * s + 4]),
                    FMT_32PARGB => row[d..d + 4].copy_from_slice(&[premul(bl, a), premul(g, a), premul(rd, a), a]),
                    _ => row[d..d + 4].copy_from_slice(&[bl, g, rd, a]),
                }
            }
            mem::write(buf + j * stride, &row);
        }
        // (ImageLockModeWrite: copied back by UnlockBits)
        b.locked = Some((buf, x, y, if flags & 2 != 0 { w } else { 0 }, fmt));
        Ok((w, h, stride, buf))
    });
    let (w, h, stride, buf) = match r {
        Ok(v) => v,
        Err(e) => return e,
    };
    for (i, v) in [w, h, stride as u32, fmt].into_iter().enumerate() {
        w32(out + 4 * i as u32, v);
    }
    mem::wp(out + 16, buf);
    mem::wp(out + 16 + mem::ps(), 0);
    OK
}
/// GdipBitmapUnlockBits: a bitmap not locked is Win32Error (winref gdip)
fn GdipBitmapUnlockBits(c: &mut Ctx) -> u32 {
    let (bmp, data) = (c.a(0), c.a(1));
    with(|m| {
        let Some(b) = m.get_mut(&bmp) else { return INVALID_PARAMETER };
        let Some((buf, x, y, w, fmt)) = b.locked.take() else { return WIN32_ERROR };
        let (h, stride) = (r32(data + 4), r32(data + 8));
        let bits = lock_bits(fmt);
        for j in 0..h.min(b.h - y) {
            let row = mem::slice(buf + j * stride, (w * bits).div_ceil(8)).to_vec();
            for i in 0..w {
                let s = (i * bits / 8) as usize;
                let d = ((y + j) * b.w + x + i) as usize;
                if indexed(fmt) {
                    let v = get_bits(&row, i, bits) as u8;
                    b.idx[d] = v;
                    let col = b.palette.get(v as usize).copied().unwrap_or(0xff00_0000);
                    b.px[4 * d..4 * d + 4].copy_from_slice(&col.to_le_bytes());
                    continue;
                }
                let v = match bits {
                    16 => {
                        let [bl, g, rd] = unpack16(fmt, u16::from_le_bytes([row[s], row[s + 1]]));
                        [bl, g, rd, 0xff]
                    }
                    24 => [row[s], row[s + 1], row[s + 2], 0xff],
                    _ if fmt == b.fmt && (fmt == FMT_32RGB || fmt == FMT_32PARGB) => {
                        b.px[4 * d..4 * d + 4].copy_from_slice(&row[s..s + 4]);
                        continue;
                    }
                    _ if fmt == FMT_32PARGB => {
                        let a = row[s + 3];
                        [unpremul(row[s], a), unpremul(row[s + 1], a), unpremul(row[s + 2], a), a]
                    }
                    _ => [row[s], row[s + 1], row[s + 2], if fmt == FMT_32RGB { 0xff } else { row[s + 3] }],
                };
                if !b.idx.is_empty() {
                    let k = b.nearest(v);
                    b.idx[d] = k;
                    let col = b.palette[k as usize];
                    b.px[4 * d..4 * d + 4].copy_from_slice(&col.to_le_bytes());
                } else {
                    b.store(d, v);
                }
            }
        }
        mem::heap_free(buf);
        OK
    })
}

/// GdipCreateBitmapFromScan0(width, height, stride, format, scan0, &bitmap): scan0 = 0 means a
/// new zeroed bitmap; otherwise the caller's pixels are copied in (GDI+ would alias them). A
/// stride that is no multiple of 4 is InvalidParameter (winref gdip).
fn GdipCreateBitmapFromScan0(c: &mut Ctx) -> u32 {
    let (w, h, stride, fmt, scan0, out) = (c.a(0), c.a(1), c.a(2) as i32, c.a(3), c.a(4), c.a(5));
    if w == 0 || h == 0 || out == 0 || w.checked_mul(h).is_none_or(|n| n > 1 << 26) || scan0 != 0 && stride % 4 != 0 {
        return INVALID_PARAMETER;
    }
    let alpha = if has_alpha(fmt) { 0 } else { 0xff };
    let mut px = vec![0u8; (w * h * 4) as usize];
    for p in px.chunks_mut(4) {
        p[3] = alpha;
    }
    let mut b = Bitmap { w, h, px: Vec::new(), fmt, flags: if has_alpha(fmt) { FLAG_HAS_ALPHA } else { 0 }, palette: Vec::new(), idx: Vec::new(), locked: None };
    if scan0 != 0 {
        let bpp = bytes_per_pixel(fmt);
        for j in 0..h {
            let row = mem::slice((scan0 as i32 + j as i32 * stride) as u32, w * bpp);
            for i in 0..w {
                let s = (i * bpp) as usize;
                let d = ((j * w + i) * 4) as usize;
                let v = match bpp {
                    2 => {
                        let [b, g, r] = unpack16(fmt, u16::from_le_bytes([row[s], row[s + 1]]));
                        [b, g, r, 0xff]
                    }
                    3 => [row[s], row[s + 1], row[s + 2], 0xff],
                    _ => [row[s], row[s + 1], row[s + 2], row[s + 3]],
                };
                px[d..d + 4].copy_from_slice(&v);
            }
        }
    }
    b.px = px;
    add(b, out)
}

/// straight BGRA `p` over the background `bg`, keeping p's alpha: the pixel premultiplied plus
/// the background times 1 - alpha, each rounded (7ffe8000 over ff336699 is 7f99734d; winref gdip)
fn over(p: [u8; 4], bg: [u8; 4]) -> [u8; 4] {
    let a = p[3];
    let mix = |s: u8, k: u8| premul(s, a) + premul(k, 255 - a);
    [mix(p[0], bg[0]), mix(p[1], bg[1]), mix(p[2], bg[2]), a]
}

/// GdipCreateHBITMAPFromBitmap(bitmap, &hbitmap, background): a 32-bit top-down DIB section of
/// the pixels over the background colour, rounded, with the pixels' own alpha (winref gdip)
fn GdipCreateHBITMAPFromBitmap(c: &mut Ctx) -> u32 {
    let bg = c.a(2).to_le_bytes();
    let Some((w, h, px)) = with(|m| {
        m.get(&c.a(0)).map(|b| {
            let px: Vec<u8> = (0..(b.w * b.h) as usize)
                .flat_map(|i| {
                    over(b.straight(i), bg)
                })
                .collect();
            (b.w, b.h, px)
        })
    }) else {
        return INVALID_PARAMETER;
    };
    let hb = crate::gdi::dib_section_bgra(w, h, &px);
    crate::mem::putp(c.a(1), hb);
    if hb != 0 { OK } else { 1 }
}

/// GdipAlloc(size) / GdipFree(p): GDI+'s operator new and delete
fn GdipAlloc(c: &mut Ctx) -> u32 {
    crate::mem::heap_alloc(c.a(0).max(1), false)
}
fn GdipFree(c: &mut Ctx) -> u32 {
    if c.a(0) != 0 {
        crate::mem::heap_free(c.a(0));
    }
    0
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("GdiplusStartup", GdiplusStartup),
        ("GdiplusShutdown", ok),
        ("GdipAlloc", GdipAlloc),
        ("GdipFree", GdipFree),
        ("GdipCreateBitmapFromStream", from_stream),
        ("GdipCreateBitmapFromStreamICM", from_stream),
        ("GdipLoadImageFromStream", from_stream),
        ("GdipLoadImageFromStreamICM", from_stream),
        ("GdipCreateBitmapFromFile", from_file),
        ("GdipCreateBitmapFromFileICM", from_file),
        ("GdipLoadImageFromFile", from_file),
        ("GdipLoadImageFromFileICM", from_file),
        ("GdipGetImageWidth", GdipGetImageWidth),
        ("GdipGetImageHeight", GdipGetImageHeight),
        ("GdipGetImageDimension", GdipGetImageDimension),
        ("GdipGetImagePixelFormat", GdipGetImagePixelFormat),
        ("GdipGetImageType", GdipGetImageType),
        ("GdipGetImagePaletteSize", GdipGetImagePaletteSize),
        ("GdipGetImagePalette", GdipGetImagePalette),
        ("GdipGetImageDecodersSize", GdipGetImageDecodersSize),
        ("GdipGetImageDecoders", GdipGetImageDecoders),
        ("GdipGetImageEncodersSize", GdipGetImageEncodersSize),
        ("GdipGetImageFlags", GdipGetImageFlags),
        ("GdipBitmapSetPixel", GdipBitmapSetPixel),
        ("GdipDisposeImage", GdipDisposeImage),
        ("GdipCloneImage", GdipCloneImage),
        ("GdipBitmapGetPixel", GdipBitmapGetPixel),
        ("GdipBitmapLockBits", GdipBitmapLockBits),
        ("GdipBitmapUnlockBits", GdipBitmapUnlockBits),
        ("GdipCreateHBITMAPFromBitmap", GdipCreateHBITMAPFromBitmap),
        ("GdipCreateBitmapFromScan0", GdipCreateBitmapFromScan0),
    ];
    for (n, f) in fns {
        implement("gdiplus", n, *f);
    }
}

#[cfg(test)]
mod pack_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn pack16_round_trip(v in any::<u16>()) {
            // every 565 value survives unpack -> pack; 555 ignores the top bit
            prop_assert_eq!(pack16(FMT_16RGB565, unpack16(FMT_16RGB565, v)), v);
            prop_assert_eq!(pack16(FMT_16RGB555, unpack16(FMT_16RGB555, v)), v & 0x7fff);
        }
    }

    /// Every (color, alpha) and the probe's measured cases. The round trip loses (premul of
    /// unpremul can be 2 low), which is why a PARGB bitmap keeps its premultiplied bytes.
    #[test]
    fn premul_unpremul() {
        for a in 1..=255u8 {
            for c in 0..=255u8 {
                let p = premul(c, a);
                assert!(p <= a);
                assert_eq!(premul(c, 255), c);
                assert_eq!(unpremul(premul(c, 255), 255), c, "c {c}");
                // (the truncated reciprocal only ever reads back low, and in order)
                let back = unpremul(p, a);
                assert!(premul(back, a) <= p, "c {c} a {a}");
                assert!(p == 0 || unpremul(p - 1, a) <= back);
            }
        }
        // the probe's measured cases (winref gdip)
        assert_eq!([premul(1, 1), premul(128, 1), premul(255, 1)], [0, 1, 1]);
        assert_eq!([premul(128, 127), premul(255, 127), premul(200, 254)], [64, 127, 199]);
        assert_eq!([unpremul(127, 127), unpremul(64, 127), unpremul(64, 128), unpremul(1, 128)], [254, 128, 127, 1]);
    }

    /// the probe's pixels over ff336699, and every (color, alpha) over any background
    /// stays in range with the extremes exact
    #[test]
    fn over_background() {
        let bg = 0xff33_6699u32.to_le_bytes();
        let got: Vec<u32> = [0x80ff_0000u32, 0x01ff_ff00, 0x7ffe_8000, 0x80ff_7f01, 0xff09_63c7].iter().map(|&p| u32::from_le_bytes(over(p.to_le_bytes(), bg))).collect();
        assert_eq!(got, [0x8099_334c, 0x0134_6798, 0x7f99_734d, 0x8099_734d, 0xff09_63c7]);
        for a in 0..=255u8 {
            for c in 0..=255u8 {
                assert_eq!(over([c, 0, 255, a], [c, 255, 0, 0])[..3], [c, 255 - a, a], "c {c} a {a}");
            }
        }
    }

    #[test]
    fn pack16_extremes() {
        assert_eq!(unpack16(FMT_16RGB565, 0xffff), [255, 255, 255]);
        assert_eq!(unpack16(FMT_16RGB565, 0xf800), [0, 0, 255]);
        assert_eq!(pack16(FMT_16RGB555, [255, 0, 0]), 0x1f);
    }
}

#[cfg(test)]
mod decoder_tests {
    use super::*;
    use proptest::prelude::*;

    fn ptr(b: &[u8], at: usize, ps: u32) -> u64 {
        let mut v = [0u8; 8];
        v[..ps as usize].copy_from_slice(&b[at..at + ps as usize]);
        u64::from_le_bytes(v)
    }
    fn wstr(b: &[u8], off: usize) -> Option<String> {
        let w: Vec<u16> = b.get(off..)?.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let end = w.iter().position(|&c| c == 0)?;
        Some(String::from_utf16_lossy(&w[..end]))
    }

    /// #162: the size GdipGetImageDecodersSize reports is the bytes GdipGetImageDecoders writes
    /// (it used to leave the four "gdiplus.dll" names out: 726 reported, 814 written)
    #[test]
    fn size_counts_everything_written() {
        assert_eq!(decoders_blob(0x23aa848, 4).len(), 814);
        assert_eq!(decoders_blob(0, 8).len(), 814 + 4 * 28);
    }

    proptest! {
        /// Wherever the buffer is, its length is the same and every pointer in the structs
        /// lands inside it, past the structs, on what the codec says.
        #[test]
        fn pointers_stay_inside(base in 0u64..0xffff_0000, x64 in any::<bool>()) {
            let ps = if x64 { 8 } else { 4 };
            let base = base & !(ps as u64 - 1);
            let b = decoders_blob(base, ps);
            prop_assert_eq!(b.len(), decoders_blob(0, ps).len());
            let one = (32 + 7 * ps + 16) as usize;
            let structs = one * DECODERS.len();
            let mut ends = Vec::new();
            for (i, d) in DECODERS.iter().enumerate() {
                let p = one * i;
                for (k, want) in [d.1, "gdiplus.dll", d.2, d.3, d.4].into_iter().enumerate() {
                    let at = ptr(&b, p + 32 + ps as usize * k, ps);
                    prop_assert!(at >= base + structs as u64 && at < base + b.len() as u64);
                    let off = (at - base) as usize;
                    let got = wstr(&b, off);
                    prop_assert_eq!(got.as_deref(), Some(want));
                    ends.push(off + 2 * (want.len() + 1));
                }
                let q = p + 32 + 5 * ps as usize;
                let sig = u32::from_le_bytes(b[q + 12..q + 16].try_into().unwrap()) as usize;
                prop_assert_eq!(sig, d.5.len());
                let (pat, mask) = ((ptr(&b, q + 16, ps) - base) as usize, (ptr(&b, q + 16 + ps as usize, ps) - base) as usize);
                prop_assert_eq!(&b[pat..pat + sig], d.5);
                prop_assert!(b[mask..mask + sig].iter().all(|&m| m == 0xff));
                ends.push(mask + sig);
            }
            // (nothing past the last string or signature: the size is tight)
            prop_assert_eq!(ends.into_iter().max(), Some(b.len()));
        }
    }
}
