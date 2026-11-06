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

/// An n-bit UNORM value as 8 bits: round(v * 255 / (2^n - 1)), what a GPU samples it as (D3D's
/// UNORM rules: v / (2^n - 1) as a float, then rounded to the nearest 8-bit value). (Bit
/// replication, (v << 3) | (v >> 2), is off by one for 14 of the 5- and 6-bit values.)
fn ext(v: u32, bits: u32) -> u32 {
    let max = (1 << bits) - 1;
    (v * 255 + max / 2) / max
}

fn rgb565(c: u16) -> [u8; 3] {
    let c = c as u32;
    [ext(c & 31, 5) as u8, ext((c >> 5) & 63, 6) as u8, ext(c >> 11, 5) as u8]
}

/// One w x h region of `fmt` (rows `pitch` bytes apart; for block formats, block rows and a
/// region aligned to blocks) as BGRA8 (or the float formats as they are), `w*bpp` bytes a row.
/// Formats whose pixels the page takes as they are (convert() only copies them).
pub fn same_on_page(fmt: u32) -> bool {
    matches!(fmt, A8R8G8B8 | X8R8G8B8 | A16B16G16R16F)
}

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

#[cfg(test)]
mod tests {
    use super::*;

    // References written from the Direct3D docs ("D3DFORMAT", "Block Compression"), in floats,
    // independent of the code above: a channel of n bits is c / (2^n - 1), 8-bit outputs are
    // rounded to nearest; the page wants B, G, R, A bytes.

    fn unorm(c: u32, bits: u32) -> f64 {
        c as f64 / ((1u32 << bits) - 1) as f64
    }
    fn byte(f: f64) -> u8 {
        (f.clamp(0.0, 1.0) * 255.0).round() as u8
    }
    fn conv(fmt: u32, src: &[u8], w: u32, h: u32) -> Vec<u8> {
        let (pitch, _) = pitch_rows(fmt, w, h);
        let mut padded = Vec::new();
        let (bpb, bs) = layout(fmt).unwrap();
        let row = (w.div_ceil(bs) * bpb) as usize;
        for r in src.chunks(row) {
            padded.extend_from_slice(r);
            padded.resize(padded.len().next_multiple_of(pitch as usize), 0xcd);
        }
        let mut out = Vec::new();
        unsafe { convert(fmt, padded.as_ptr(), pitch as usize, w, h, &mut out) };
        out
    }

    /// Every value of every 16-bit format.
    #[test]
    fn every_16_bit_texel() {
        // (format, [(shift, bits) of B, G, R, A] (bits 0: none, alpha then 1.0))
        let fmts: [(u32, [(u32, u32); 4]); 6] = [
            (R5G6B5, [(0, 5), (5, 6), (11, 5), (0, 0)]),
            (X1R5G5B5, [(0, 5), (5, 5), (10, 5), (0, 0)]),
            (A1R5G5B5, [(0, 5), (5, 5), (10, 5), (15, 1)]),
            (A4R4G4B4, [(0, 4), (4, 4), (8, 4), (12, 4)]),
            (X4R4G4B4, [(0, 4), (4, 4), (8, 4), (0, 0)]),
            (A8L8, [(0, 8), (0, 8), (0, 8), (8, 8)]),
        ];
        for (fmt, ch) in fmts {
            let src: Vec<u8> = (0..=u16::MAX).flat_map(|v| v.to_le_bytes()).collect();
            let out = conv(fmt, &src, 256, 256);
            for v in 0..=u16::MAX as u32 {
                let want: Vec<u8> = ch.iter().map(|&(sh, bits)| if bits == 0 { 255 } else { byte(unorm((v >> sh) & ((1 << bits) - 1), bits)) }).collect();
                assert_eq!(&out[v as usize * 4..][..4], &want[..], "format {fmt} texel {v:#06x}");
            }
        }
    }

    /// Every value of every 8-bit format, and the byte orders of the 24- and 32-bit ones.
    #[test]
    fn every_8_bit_texel_and_byte_orders() {
        let src: Vec<u8> = (0..=255).collect();
        for v in 0..=255u32 {
            let i = v as usize * 4;
            assert_eq!(conv(A8, &src, 256, 1)[i..i + 4], [0, 0, 0, v as u8]);
            assert_eq!(conv(L8, &src, 256, 1)[i..i + 4], [v as u8, v as u8, v as u8, 255]);
            let (l, a) = (byte(unorm(v & 15, 4)), byte(unorm(v >> 4, 4)));
            assert_eq!(conv(A4L4, &src, 256, 1)[i..i + 4], [l, l, l, a]);
        }
        // D3DFMT_R8G8B8: B, G, R in memory; A8B8G8R8: R, G, B, A
        assert_eq!(conv(R8G8B8, &[1, 2, 3, 4, 5, 6], 2, 1), [1, 2, 3, 255, 4, 5, 6, 255]);
        assert_eq!(conv(A8B8G8R8, &[1, 2, 3, 4], 1, 1), [3, 2, 1, 4]);
        assert_eq!(conv(X8B8G8R8, &[1, 2, 3, 4], 1, 1), [3, 2, 1, 4]);
        assert_eq!(conv(A8R8G8B8, &[1, 2, 3, 4], 1, 1), [1, 2, 3, 4]);
    }

    /// The reference BC1-3 decoder ("Block Compression (Direct3D 10)"): 16 B, G, R, A pixels in floats.
    fn ref_dxt(fmt: u32, b: &[u8]) -> [[f64; 4]; 16] {
        let col = if fmt == DXT1 { b } else { &b[8..] };
        let c0 = u16::from_le_bytes([col[0], col[1]]) as u32;
        let c1 = u16::from_le_bytes([col[2], col[3]]) as u32;
        let e = |c: u32| [unorm(c & 31, 5), unorm((c >> 5) & 63, 6), unorm(c >> 11, 5), 1.0];
        let (p0, p1) = (e(c0), e(c1));
        let lerp = |a: [f64; 4], b: [f64; 4], t: f64| [0, 1, 2, 3].map(|i| a[i] + (b[i] - a[i]) * t);
        let pal = if c0 > c1 || fmt != DXT1 {
            [p0, p1, lerp(p0, p1, 1.0 / 3.0), lerp(p0, p1, 2.0 / 3.0)]
        } else {
            [p0, p1, lerp(p0, p1, 0.5), [0.0; 4]]
        };
        let bits = u32::from_le_bytes([col[4], col[5], col[6], col[7]]);
        let mut out = [[0.0; 4]; 16];
        for (i, o) in out.iter_mut().enumerate() {
            *o = pal[((bits >> (2 * i)) & 3) as usize];
            match fmt {
                DXT2 | DXT3 => o[3] = unorm(((b[i / 2] >> (4 * (i & 1))) & 15) as u32, 4),
                DXT4 | DXT5 => {
                    let (a0, a1) = (b[0] as f64 / 255.0, b[1] as f64 / 255.0);
                    let code = ((u64::from_le_bytes([b[2], b[3], b[4], b[5], b[6], b[7], 0, 0]) >> (3 * i)) & 7) as usize;
                    o[3] = match code {
                        0 => a0,
                        1 => a1,
                        k if b[0] > b[1] => ((8 - k) as f64 * a0 + (k - 1) as f64 * a1) / 7.0,
                        6 => 0.0,
                        7 => 1.0,
                        k => ((6 - k) as f64 * a0 + (k - 1) as f64 * a1) / 5.0,
                    }
                }
                _ => {}
            }
        }
        out
    }

    /// Our 8-bit pixels against the reference: interpolated values may be off by one (the
    /// block formats' tolerance), the endpoints and alpha codes 0/1 must be exact.
    fn check_block(fmt: u32, b: &[u8]) {
        let got = decode_dxt(fmt, b);
        for (i, (g, r)) in got.iter().zip(ref_dxt(fmt, b)).enumerate() {
            for c in 0..4 {
                let want = byte(r[c]) as i32;
                assert!((g[c] as i32 - want).abs() <= 1, "format {fmt:#x} block {b:02x?} pixel {i} channel {c}: {} want {want}", g[c]);
            }
        }
    }

    /// Every pair of endpoint values of each channel, in both modes, every palette index; every
    /// pair of DXT5 alpha endpoints with every code; every DXT3 alpha.
    #[test]
    fn dxt_blocks_against_the_reference() {
        // indices 0, 1, 2, 3 in pixels 0-3 (and again)
        let idx = 0b11_10_01_00_11_10_01_00_11_10_01_00_11_10_01_00u32.to_le_bytes();
        for (sh, bits) in [(0, 5), (5, 6), (11, 5)] {
            for a in 0..1u32 << bits {
                for b in 0..1u32 << bits {
                    // the other channels pick the mode: c0 > c1, then c0 <= c1 (and c0 == c1
                    // when a == b: the 3-color mode too)
                    for (oa, ob) in [(0xffffu32, 0u32), (0, 0xffff), (0, 0)] {
                        let m = !(((1 << bits) - 1) << sh) & 0xffff;
                        let c0 = (a << sh) | (oa & m);
                        let c1 = (b << sh) | (ob & m);
                        let mut blk = [0u8; 8];
                        blk[..2].copy_from_slice(&(c0 as u16).to_le_bytes());
                        blk[2..4].copy_from_slice(&(c1 as u16).to_le_bytes());
                        blk[4..].copy_from_slice(&idx);
                        check_block(DXT1, &blk);
                        let mut b16 = [0u8; 16];
                        b16[8..].copy_from_slice(&blk);
                        check_block(DXT3, &b16);
                        check_block(DXT5, &b16);
                    }
                }
            }
        }
        for a0 in 0..=255u8 {
            for a1 in 0..=255u8 {
                // codes 0..7 in pixels 0..7 and 8..15
                let codes: u64 = (0..16).map(|i| ((i % 8) as u64) << (3 * i)).sum();
                let mut b = [0u8; 16];
                b[0] = a0;
                b[1] = a1;
                b[2..8].copy_from_slice(&codes.to_le_bytes()[..6]);
                check_block(DXT5, &b);
            }
        }
        let mut b = [0u8; 16];
        for (i, v) in b[..8].iter_mut().enumerate() {
            *v = (i as u8 * 2) | ((i as u8 * 2 + 1) << 4);
        }
        check_block(DXT3, &b);
    }


    /// What each format is, as the D3D9 docs list them.
    #[test]
    fn format_table() {
        use super::super::gpu::{FMT_BGRA8, FMT_DEPTH24S8, FMT_RGBA16F, FMT_RGBA32F};
        // (format, depth, render target, no alpha, kept as it is on the page)
        let all = [
            (R8G8B8, false, false, true, false),
            (A8R8G8B8, false, true, false, true),
            (X8R8G8B8, false, true, true, true),
            (R5G6B5, false, true, true, false),
            (X1R5G5B5, false, true, true, false),
            (A1R5G5B5, false, true, false, false),
            (A4R4G4B4, false, false, false, false),
            (A8, false, false, false, false),
            (X4R4G4B4, false, false, true, false),
            (A8B8G8R8, false, true, false, false),
            (X8B8G8R8, false, true, true, false),
            (L8, false, false, true, false),
            (A8L8, false, false, false, false),
            (A4L4, false, false, false, false),
            (P8, false, false, false, false),
            (D16_LOCKABLE, true, false, false, false),
            (D32, true, false, false, false),
            (D15S1, true, false, false, false),
            (D24S8, true, false, false, false),
            (D24X8, true, false, false, false),
            (D24X4S4, true, false, false, false),
            (D16, true, false, false, false),
            (D32F_LOCKABLE, true, false, false, false),
            (D24FS8, true, false, false, false),
            (A16B16G16R16F, false, true, false, true),
            (A32B32G32R32F, false, true, false, false),
            (DXT1, false, false, true, false),
            (DXT2, false, false, false, false),
            (DXT3, false, false, false, false),
            (DXT4, false, false, false, false),
            (DXT5, false, false, false, false),
        ];
        for (f, depth, rt, no_a, same) in all {
            assert!(supported(f), "{f}");
            assert_eq!((is_depth(f), renderable(f), no_alpha(f), same_on_page(f)), (depth, rt, no_a, same), "{f}");
            let (page, bpp) = match f {
                _ if depth => (FMT_DEPTH24S8, 4),
                A16B16G16R16F => (FMT_RGBA16F, 8),
                A32B32G32R32F => (FMT_RGBA32F, 16),
                _ => (FMT_BGRA8, 4),
            };
            assert_eq!((page_format(f), page_bpp(f)), (page, bpp), "{f}");
        }
        for f in [0, 1, 27, 40, 100, INDEX16, INDEX32, 114, u32::MAX] {
            assert!(!supported(f) && !is_depth(f) && !renderable(f) && !no_alpha(f) && !same_on_page(f), "{f}");
            assert_eq!(page_format(f), FMT_BGRA8);
        }
    }

    /// A region that ends inside a block: only its pixels are written, row by row.
    #[test]
    fn dxt_partial_blocks() {
        let mut blk = [0u8; 8];
        blk[..2].copy_from_slice(&0xf800u16.to_le_bytes()); // red
        let src: Vec<u8> = blk.iter().chain(blk.iter()).copied().collect();
        let out = conv(DXT1, &src, 5, 3);
        assert_eq!(out.len(), 5 * 3 * 4);
        assert!(out.chunks(4).all(|p| p == [0, 0, 255, 255]));
    }

    /// Every format on every region up to 9 x 9 from a source exactly as long as the region
    /// needs (the last row only as long as its texels): run under Miri (formal/miri.sh), a read
    /// past it or a write outside the pixels added is an error.
    #[test]
    fn every_format_on_exact_buffers() {
        let fmts = [
            R8G8B8, A8R8G8B8, X8R8G8B8, R5G6B5, X1R5G5B5, A1R5G5B5, A4R4G4B4, A8, X4R4G4B4, A8B8G8R8,
            X8B8G8R8, L8, A8L8, A4L4, P8, A16B16G16R16F, A32B32G32R32F, DXT1, DXT2, DXT3, DXT4, DXT5,
        ];
        for fmt in fmts {
            let (bpb, bs) = layout(fmt).unwrap();
            for w in 1..=9 {
                for h in 1..=9 {
                    let (pitch, rows) = pitch_rows(fmt, w, h);
                    let len = (pitch * (rows - 1) + w.div_ceil(bs) * bpb) as usize;
                    let src: Vec<u8> = (0..len).map(|i| (i * 37) as u8).collect();
                    let mut out = vec![7u8; 3];
                    unsafe { convert(fmt, src.as_ptr(), pitch as usize, w, h, &mut out) };
                    assert_eq!(out.len(), 3 + (w * h * page_bpp(fmt)) as usize);
                    assert_eq!(out[..3], [7, 7, 7]);
                }
            }
        }
    }

    #[test]
    fn pitches_and_rows() {
        assert_eq!(pitch_rows(A8R8G8B8, 3, 2), (12, 2));
        assert_eq!(pitch_rows(R8G8B8, 3, 1), (12, 1)); // (9 -> 12: rows 4-byte aligned)
        assert_eq!(pitch_rows(L8, 1, 1), (4, 1));
        assert_eq!(pitch_rows(DXT1, 1, 1), (8, 1)); // a whole block for a 1x1 level
        assert_eq!(pitch_rows(DXT5, 9, 5), (48, 2));
        for f in [A8R8G8B8, R5G6B5, A8, DXT1, DXT3, A16B16G16R16F, A32B32G32R32F] {
            let (bpb, bs) = layout(f).unwrap();
            for w in 1..20 {
                let (p, _) = pitch_rows(f, w, 1);
                assert!(p >= w.div_ceil(bs) * bpb && p % 4 == 0);
            }
        }
    }
}

/// Kani proofs (formal/kani.sh): the UNORM expansion and the pitch arithmetic for every input.
#[cfg(kani)]
mod proofs {
    use super::*;

    const FORMATS: [u32; 22] = [
        R8G8B8, A8R8G8B8, X8R8G8B8, R5G6B5, X1R5G5B5, A1R5G5B5, A4R4G4B4, A8, X4R4G4B4, A8B8G8R8,
        X8B8G8R8, L8, A8L8, A4L4, P8, A16B16G16R16F, A32B32G32R32F, DXT1, DXT2, DXT3, DXT4, DXT5,
    ];

    #[kani::proof]
    fn ext_is_rounded_unorm() {
        let bits: u32 = *[1u32, 4, 5, 6].get(kani::any_where(|&i: &usize| i < 4)).unwrap();
        let v: u32 = kani::any_where(|&v| v < 1 << bits);
        let max = (1 << bits) - 1;
        let e = ext(v, bits);
        // |e - v * 255 / max| <= 1/2, in integers
        assert!(e <= 255);
        assert!((2 * e * max).abs_diff(2 * v * 255) <= max);
    }

    /// No overflow for any level Direct3D 9 allows (8192 x 8192 at most), and the pitch covers a row.
    #[kani::proof]
    fn pitch_rows_no_overflow() {
        let fmt = FORMATS[kani::any_where(|&i: &usize| i < FORMATS.len())];
        let w: u32 = kani::any_where(|&w| (1..=8192).contains(&w));
        let h: u32 = kani::any_where(|&h| (1..=8192).contains(&h));
        let (bpb, bs) = layout(fmt).unwrap();
        let (pitch, rows) = pitch_rows(fmt, w, h);
        assert!(pitch >= w.div_ceil(bs) * bpb && pitch % 4 == 0 && pitch < w * bpb + 4);
        assert!(rows == h.div_ceil(bs));
    }
}
