//! GDI+ images (gdiplus.dll): bitmaps decoded from a file or an IStream (PNG, JPEG, BMP via
//! ole::decode), their size, pixel format, pixels (LockBits, GetPixel) and an HBITMAP of them.
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

struct Bitmap {
    w: u32,
    h: u32,
    px: Vec<u8>, // BGRA, top row first
    locked: Option<(u32, u32, u32, u32, u32)>, // buffer, x, y, w, format (to write back)
}

static IMAGES: Mutex<Option<HashMap<u32, Bitmap>>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Bitmap>) -> R) -> R {
    f(IMAGES.lock().unwrap().get_or_insert_with(HashMap::default))
}

fn new_image(data: &[u8], out: u32) -> u32 {
    let Some((w, h, px)) = crate::ole::decode(data) else { return UNKNOWN_IMAGE_FORMAT };
    let o = mem::heap_alloc(16, true);
    with(|m| m.insert(o, Bitmap { w, h, px, locked: None }));
    mem::putp(out, o);
    OK
}

fn GdiplusStartup(c: &mut Ctx) -> u32 {
    put32(c.a(0), 1);
    if c.a(2) != 0 {
        mem::fill(c.a(2), 8, 0);
    }
    OK
}
fn ok(_: &mut Ctx) -> u32 {
    OK
}

/// GdipCreateBitmapFromStream / GdipLoadImageFromStream (+ICM)(stream, &image)
fn from_stream(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 || c.a(1) == 0 {
        return INVALID_PARAMETER;
    }
    let data = crate::wic::read_stream(c, c.a(0));
    new_image(&data, c.a(1))
}
/// GdipCreateBitmapFromFile / GdipLoadImageFromFile (+ICM)(wide path, &image)
fn from_file(c: &mut Ctx) -> u32 {
    let name = String::from_utf16_lossy(&mem::wstr(c.a(0)));
    let Some(data) = crate::files::resolve(&name).and_then(|h| kura::fs::read(h).ok()) else { return FILE_NOT_FOUND };
    new_image(&data, c.a(1))
}

fn size_of(img: u32) -> Option<(u32, u32)> {
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
    put32(c.a(1), FMT_32ARGB);
    OK
}
fn GdipGetImageType(c: &mut Ctx) -> u32 {
    put32(c.a(1), 1); // ImageTypeBitmap
    OK
}
/// (a ColorPalette with no entries: flags, count, one ARGB)
fn GdipGetImagePaletteSize(c: &mut Ctx) -> u32 {
    put32(c.a(1), 12);
    OK
}
fn GdipGetImagePalette(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 || c.a(2) < 8 {
        return INVALID_PARAMETER;
    }
    mem::fill(c.a(1), c.a(2).min(12), 0);
    OK
}
fn GdipGetImageDecodersSize(c: &mut Ctx) -> u32 {
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
    let Some((w, h, px)) = with(|m| m.get(&c.a(0)).map(|b| (b.w, b.h, b.px.clone()))) else { return INVALID_PARAMETER };
    let o = mem::heap_alloc(16, true);
    with(|m| m.insert(o, Bitmap { w, h, px, locked: None }));
    mem::putp(c.a(1), o);
    OK
}

/// GdipBitmapGetPixel(bitmap, x, y, &argb)
fn GdipBitmapGetPixel(c: &mut Ctx) -> u32 {
    let (x, y) = (c.a(1), c.a(2));
    let p = with(|m| {
        let b = m.get(&c.a(0))?;
        (x < b.w && y < b.h).then(|| {
            let i = ((y * b.w + x) * 4) as usize;
            u32::from_le_bytes(b.px[i..i + 4].try_into().unwrap())
        })
    });
    let Some(p) = p else { return INVALID_PARAMETER };
    put32(c.a(3), p);
    OK
}

/// GdipBitmapLockBits(bitmap, GpRect*, flags, format, BitmapData*): BitmapData = width, height,
/// stride, format, scan0, reserved
fn GdipBitmapLockBits(c: &mut Ctx) -> u32 {
    let (bmp, rect, flags, mut fmt, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let r = with(|m| {
        let b = m.get_mut(&bmp)?;
        let (x, y, w, h) = if rect != 0 { (r32(rect), r32(rect + 4), r32(rect + 8), r32(rect + 12)) } else { (0, 0, b.w, b.h) };
        if b.locked.is_some() || x.checked_add(w)? > b.w || y.checked_add(h)? > b.h {
            return None;
        }
        if ![FMT_24RGB, FMT_32RGB, FMT_32ARGB, FMT_32PARGB].contains(&fmt) {
            fmt = FMT_32ARGB;
        }
        let bpp = if fmt == FMT_24RGB { 3 } else { 4 };
        let stride = (w * bpp + 3) & !3;
        let buf = mem::heap_alloc(stride * h.max(1), true);
        let mut row = vec![0u8; stride as usize];
        for j in 0..h {
            for i in 0..w {
                let s = (((y + j) * b.w + x + i) * 4) as usize;
                let [bl, g, rd, a] = [b.px[s], b.px[s + 1], b.px[s + 2], b.px[s + 3]];
                let d = (i * bpp) as usize;
                match fmt {
                    FMT_24RGB => row[d..d + 3].copy_from_slice(&[bl, g, rd]),
                    FMT_32RGB => row[d..d + 4].copy_from_slice(&[bl, g, rd, 0xff]),
                    FMT_32PARGB => {
                        let pm = |v: u8| (v as u32 * a as u32 / 255) as u8;
                        row[d..d + 4].copy_from_slice(&[pm(bl), pm(g), pm(rd), a]);
                    }
                    _ => row[d..d + 4].copy_from_slice(&[bl, g, rd, a]),
                }
            }
            mem::write(buf + j * stride, &row);
        }
        // (ImageLockModeWrite: copied back by UnlockBits)
        b.locked = Some((buf, x, y, if flags & 2 != 0 { w } else { 0 }, fmt));
        Some((w, h, stride, buf))
    });
    let Some((w, h, stride, buf)) = r else { return INVALID_PARAMETER };
    for (i, v) in [w, h, stride, fmt, buf, 0].into_iter().enumerate() {
        w32(out + 4 * i as u32, v);
    }
    OK
}
fn GdipBitmapUnlockBits(c: &mut Ctx) -> u32 {
    let (bmp, data) = (c.a(0), c.a(1));
    with(|m| {
        let Some(b) = m.get_mut(&bmp) else { return INVALID_PARAMETER };
        let Some((buf, x, y, w, fmt)) = b.locked.take() else { return INVALID_PARAMETER };
        let (h, stride) = (r32(data + 4), r32(data + 8));
        let bpp = if fmt == FMT_24RGB { 3 } else { 4 };
        for j in 0..h.min(b.h - y) {
            let row = mem::slice(buf + j * stride, w * bpp);
            for i in 0..w {
                let s = (i * bpp) as usize;
                let d = (((y + j) * b.w + x + i) * 4) as usize;
                let a = if bpp == 4 && fmt != FMT_32RGB { row[s + 3] } else { 0xff };
                let un = |v: u8| if fmt == FMT_32PARGB && a != 0 { (v as u32 * 255 / a as u32).min(255) as u8 } else { v };
                b.px[d..d + 4].copy_from_slice(&[un(row[s]), un(row[s + 1]), un(row[s + 2]), a]);
            }
        }
        mem::heap_free(buf);
        OK
    })
}

/// GdipCreateHBITMAPFromBitmap(bitmap, &hbitmap, background): a 32-bit top-down DIB section
fn GdipCreateHBITMAPFromBitmap(c: &mut Ctx) -> u32 {
    let Some((w, h, px)) = with(|m| m.get(&c.a(0)).map(|b| (b.w, b.h, b.px.clone()))) else { return INVALID_PARAMETER };
    let hb = crate::gdi::dib_section_bgra(w, h, &px);
    put32(c.a(1), hb);
    if hb != 0 { OK } else { 1 }
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("GdiplusStartup", GdiplusStartup),
        ("GdiplusShutdown", ok),
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
        ("GdipGetImageEncodersSize", GdipGetImageDecodersSize),
        ("GdipDisposeImage", GdipDisposeImage),
        ("GdipCloneImage", GdipCloneImage),
        ("GdipBitmapGetPixel", GdipBitmapGetPixel),
        ("GdipBitmapLockBits", GdipBitmapLockBits),
        ("GdipBitmapUnlockBits", GdipBitmapUnlockBits),
        ("GdipCreateHBITMAPFromBitmap", GdipCreateHBITMAPFromBitmap),
    ];
    for (n, f) in fns {
        implement("gdiplus", n, *f);
    }
}
