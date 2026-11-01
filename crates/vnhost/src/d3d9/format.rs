//! D3DFORMATs: their memory layout, and what the page keeps them as.
//!
//! Color textures are all BGRA8 on the page (converted when uploaded), except float formats.

use super::gpu::{FMT_BGRA8, FMT_DEPTH24S8, FMT_RGBA16F, FMT_RGBA32F};

pub const R8G8B8: u32 = 20;
pub const A8R8G8B8: u32 = 21;
pub const X8R8G8B8: u32 = 22;
pub const R5G6B5: u32 = 23;
pub const X1R5G5B5: u32 = 24;
pub const A1R5G5B5: u32 = 25;
pub const A4R4G4B4: u32 = 26;
pub const A8: u32 = 28;
pub const X4R4G4B4: u32 = 30;
pub const A8B8G8R8: u32 = 32;
pub const X8B8G8R8: u32 = 33;
pub const L8: u32 = 50;
pub const A8L8: u32 = 51;
pub const A4L4: u32 = 52;
pub const P8: u32 = 41;
pub const D16_LOCKABLE: u32 = 70;
pub const D32: u32 = 71;
pub const D15S1: u32 = 73;
pub const D24S8: u32 = 75;
pub const D24X8: u32 = 77;
pub const D24X4S4: u32 = 79;
pub const D16: u32 = 80;
pub const D32F_LOCKABLE: u32 = 82;
pub const D24FS8: u32 = 83;
pub const INDEX16: u32 = 101;
pub const INDEX32: u32 = 102;
pub const A16B16G16R16F: u32 = 113;
pub const A32B32G32R32F: u32 = 116;
pub const DXT1: u32 = u32::from_le_bytes(*b"DXT1");
pub const DXT2: u32 = u32::from_le_bytes(*b"DXT2");
pub const DXT3: u32 = u32::from_le_bytes(*b"DXT3");
pub const DXT4: u32 = u32::from_le_bytes(*b"DXT4");
pub const DXT5: u32 = u32::from_le_bytes(*b"DXT5");

pub fn supported(fmt: u32) -> bool {
    layout(fmt).is_some()
}

pub fn is_depth(fmt: u32) -> bool {
    matches!(fmt, D16_LOCKABLE | D32 | D15S1 | D24S8 | D24X8 | D24X4S4 | D16 | D32F_LOCKABLE | D24FS8)
}

/// Formats a render target can have.
pub fn renderable(fmt: u32) -> bool {
    matches!(fmt, A8R8G8B8 | X8R8G8B8 | A8B8G8R8 | X8B8G8R8 | R5G6B5 | X1R5G5B5 | A1R5G5B5 | A16B16G16R16F | A32B32G32R32F)
}

/// (bytes per block, block size in pixels)
pub fn layout(fmt: u32) -> Option<(u32, u32)> {
    Some(match fmt {
        A8R8G8B8 | X8R8G8B8 | A8B8G8R8 | X8B8G8R8 | D32 | D24S8 | D24X8 | D24X4S4 | D32F_LOCKABLE | D24FS8 => (4, 1),
        R8G8B8 => (3, 1),
        R5G6B5 | X1R5G5B5 | A1R5G5B5 | A4R4G4B4 | X4R4G4B4 | A8L8 | D16 | D16_LOCKABLE | D15S1 => (2, 1),
        A8 | L8 | A4L4 | P8 => (1, 1),
        A16B16G16R16F => (8, 1),
        A32B32G32R32F => (16, 1),
        DXT1 => (8, 4),
        DXT2 | DXT3 | DXT4 | DXT5 => (16, 4),
        _ => return None,
    })
}

/// Bytes per row (of blocks) and rows (of blocks) of a w x h image.
pub fn pitch_rows(fmt: u32, w: u32, h: u32) -> (u32, u32) {
    let (bpb, bs) = layout(fmt).unwrap_or((4, 1));
    let bw = w.div_ceil(bs).max(1);
    let bh = h.div_ceil(bs).max(1);
    // rows 4-byte aligned, as drivers do
    ((bw * bpb).next_multiple_of(4), bh)
}

pub fn page_format(fmt: u32) -> u32 {
    match fmt {
        _ if is_depth(fmt) => FMT_DEPTH24S8,
        A16B16G16R16F => FMT_RGBA16F,
        A32B32G32R32F => FMT_RGBA32F,
        _ => FMT_BGRA8,
    }
}

/// Bytes per pixel on the page.
pub fn page_bpp(fmt: u32) -> u32 {
    match page_format(fmt) {
        FMT_RGBA16F => 8,
        FMT_RGBA32F => 16,
        _ => 4,
    }
}

/// Whether the format has no alpha (samplers then read alpha as 1).
pub fn no_alpha(fmt: u32) -> bool {
    matches!(fmt, X8R8G8B8 | X8B8G8R8 | R5G6B5 | X1R5G5B5 | X4R4G4B4 | R8G8B8 | L8 | DXT1)
}

fn ext(v: u32, bits: u32) -> u32 {
    // n-bit value to 8 bits
    match bits {
        1 => v * 255,
        4 => v * 17,
        5 => (v << 3) | (v >> 2),
        6 => (v << 2) | (v >> 4),
        _ => v,
    }
}

fn rgb565(c: u16) -> [u8; 3] {
    let c = c as u32;
    [ext(c & 31, 5) as u8, ext((c >> 5) & 63, 6) as u8, ext(c >> 11, 5) as u8]
}

/// One w x h region of `fmt` (rows `pitch` bytes apart; for block formats, block rows and a
/// region aligned to blocks) as BGRA8 (or the float formats as they are), `w*bpp` bytes a row.
pub unsafe fn convert(fmt: u32, src: *const u8, pitch: usize, w: u32, h: u32, out: &mut Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    let page = page_bpp(fmt) as usize;
    let at = out.len();
    out.resize(at + w * h * page, 0);
    let dst = &mut out[at..];
    let row = |y: usize| src.add(y * pitch);
    match fmt {
        DXT1 | DXT2 | DXT3 | DXT4 | DXT5 => {
            for by in 0..h.div_ceil(4) {
                for bx in 0..w.div_ceil(4) {
                    let size = if fmt == DXT1 { 8 } else { 16 };
                    let b = std::slice::from_raw_parts(row(by).add(bx * size), size);
                    let px = decode_dxt(fmt, b);
                    for (i, p) in px.iter().enumerate() {
                        let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                        if x < w && y < h {
                            dst[(y * w + x) * 4..][..4].copy_from_slice(p);
                        }
                    }
                }
            }
        }
        _ => {
            for y in 0..h {
                let s = row(y);
                let d = &mut dst[y * w * page..][..w * page];
                match fmt {
                    A8R8G8B8 | X8R8G8B8 | A16B16G16R16F | A32B32G32R32F => {
                        std::ptr::copy_nonoverlapping(s, d.as_mut_ptr(), w * page)
                    }
                    A8B8G8R8 | X8B8G8R8 => {
                        for x in 0..w {
                            let p = std::slice::from_raw_parts(s.add(x * 4), 4);
                            d[x * 4..][..4].copy_from_slice(&[p[2], p[1], p[0], p[3]]);
                        }
                    }
                    R8G8B8 => {
                        for x in 0..w {
                            let p = std::slice::from_raw_parts(s.add(x * 3), 3);
                            d[x * 4..][..4].copy_from_slice(&[p[0], p[1], p[2], 255]);
                        }
                    }
                    R5G6B5 => {
                        for x in 0..w {
                            let c = (s.add(x * 2) as *const u16).read_unaligned();
                            let [b, g, r] = rgb565(c);
                            d[x * 4..][..4].copy_from_slice(&[b, g, r, 255]);
                        }
                    }
                    X1R5G5B5 | A1R5G5B5 => {
                        for x in 0..w {
                            let c = (s.add(x * 2) as *const u16).read_unaligned() as u32;
                            let a = if fmt == A1R5G5B5 { ext(c >> 15, 1) } else { 255 };
                            d[x * 4..][..4].copy_from_slice(&[
                                ext(c & 31, 5) as u8,
                                ext((c >> 5) & 31, 5) as u8,
                                ext((c >> 10) & 31, 5) as u8,
                                a as u8,
                            ]);
                        }
                    }
                    A4R4G4B4 | X4R4G4B4 => {
                        for x in 0..w {
                            let c = (s.add(x * 2) as *const u16).read_unaligned() as u32;
                            let a = if fmt == A4R4G4B4 { ext(c >> 12, 4) } else { 255 };
                            d[x * 4..][..4].copy_from_slice(&[
                                ext(c & 15, 4) as u8,
                                ext((c >> 4) & 15, 4) as u8,
                                ext((c >> 8) & 15, 4) as u8,
                                a as u8,
                            ]);
                        }
                    }
                    A8 => {
                        for x in 0..w {
                            d[x * 4..][..4].copy_from_slice(&[0, 0, 0, *s.add(x)]);
                        }
                    }
                    L8 | P8 => {
                        for x in 0..w {
                            let l = *s.add(x);
                            d[x * 4..][..4].copy_from_slice(&[l, l, l, 255]);
                        }
                    }
                    A8L8 => {
                        for x in 0..w {
                            let (l, a) = (*s.add(x * 2), *s.add(x * 2 + 1));
                            d[x * 4..][..4].copy_from_slice(&[l, l, l, a]);
                        }
                    }
                    A4L4 => {
                        for x in 0..w {
                            let v = *s.add(x) as u32;
                            let l = ext(v & 15, 4) as u8;
                            d[x * 4..][..4].copy_from_slice(&[l, l, l, ext(v >> 4, 4) as u8]);
                        }
                    }
                    _ => d.fill(0),
                }
            }
        }
    }
}

/// 16 BGRA pixels of one DXT block.
fn decode_dxt(fmt: u32, b: &[u8]) -> [[u8; 4]; 16] {
    let col = if fmt == DXT1 { b } else { &b[8..] };
    let c0 = u16::from_le_bytes([col[0], col[1]]);
    let c1 = u16::from_le_bytes([col[2], col[3]]);
    let (p0, p1) = (rgb565(c0), rgb565(c1));
    let mix = |a: u8, b: u8, wa: u32, wb: u32, d: u32| ((a as u32 * wa + b as u32 * wb) / d) as u8;
    let mut pal = [[0u8; 4]; 4];
    pal[0] = [p0[0], p0[1], p0[2], 255];
    pal[1] = [p1[0], p1[1], p1[2], 255];
    if c0 > c1 || fmt != DXT1 {
        for i in 0..3 {
            pal[2][i] = mix(p0[i], p1[i], 2, 1, 3);
            pal[3][i] = mix(p0[i], p1[i], 1, 2, 3);
        }
        pal[2][3] = 255;
        pal[3][3] = 255;
    } else {
        for i in 0..3 {
            pal[2][i] = mix(p0[i], p1[i], 1, 1, 2);
        }
        pal[2][3] = 255;
        pal[3] = [0, 0, 0, 0];
    }
    let bits = u32::from_le_bytes([col[4], col[5], col[6], col[7]]);
    let mut out = [[0u8; 4]; 16];
    for (i, o) in out.iter_mut().enumerate() {
        *o = pal[((bits >> (i * 2)) & 3) as usize];
    }
    match fmt {
        DXT2 | DXT3 => {
            for (i, o) in out.iter_mut().enumerate() {
                let v = (b[i / 2] >> ((i & 1) * 4)) & 15;
                o[3] = v * 17;
            }
        }
        DXT4 | DXT5 => {
            let (a0, a1) = (b[0] as u32, b[1] as u32);
            let mut ap = [0u32; 8];
            ap[0] = a0;
            ap[1] = a1;
            if a0 > a1 {
                for i in 1..7 {
                    ap[i + 1] = ((7 - i as u32) * a0 + i as u32 * a1) / 7;
                }
            } else {
                for i in 1..5 {
                    ap[i + 1] = ((5 - i as u32) * a0 + i as u32 * a1) / 5;
                }
                ap[6] = 0;
                ap[7] = 255;
            }
            let bits = u64::from_le_bytes([b[2], b[3], b[4], b[5], b[6], b[7], 0, 0]);
            for (i, o) in out.iter_mut().enumerate() {
                o[3] = ap[((bits >> (i * 3)) & 7) as usize] as u8;
            }
        }
        _ => {}
    }
    out
}
