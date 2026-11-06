//! DXGI formats: what the page keeps them as, and how texels get there.

/// page texture formats (d3d9.ts TEX_FORMATS)
/// Format::conv for 32-bit float texels stored as halves (d3d9's converters copy those as they are)
pub const F32_TO_F16: u32 = 0xf32;

/// f32 to f16 bits (IEEE 754 round to nearest even, as D3D's float conversion rules ask; out of
/// range to infinity, NaN kept)
pub fn f16_bits(v: f32) -> u16 {
    let b = v.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let e = ((b >> 23) & 0xff) as i32;
    let m = b & 0x7f_ffff;
    if e == 0xff {
        return sign | 0x7c00 | if m != 0 { 0x200 } else { 0 };
    }
    let e16 = e - 127 + 15;
    if e16 >= 31 {
        return sign | 0x7c00;
    }
    // (h, the bits dropped): rounding up carries into the exponent, up to infinity
    let (h, shift) = if e16 <= 0 {
        if e16 < -10 {
            return sign;
        }
        let shift = (14 - e16) as u32;
        ((m | 0x80_0000) >> shift, shift)
    } else {
        ((e16 as u32) << 10 | m >> 13, 13)
    };
    let rest = (m | 0x80_0000) & ((1 << shift) - 1);
    let half = 1 << (shift - 1);
    let up = rest > half || (rest == half && h & 1 == 1);
    sign | (h + up as u32) as u16
}

pub const P_BGRA8: u32 = 0;
pub const P_D24S8: u32 = 1;
pub const P_RGBA16F: u32 = 2;
pub const P_RGBA8: u32 = 4;
pub const P_R8: u32 = 5;
pub const P_RG8: u32 = 6;
pub const P_D32F: u32 = 7;
pub const P_R16F: u32 = 8;
pub const P_RG16F: u32 = 9;
pub const P_RGB10A2: u32 = 10;
pub const P_RG11B10F: u32 = 11;
pub const P_RGBA8_SRGB: u32 = 12;
pub const P_BGRA8_SRGB: u32 = 13;
pub const P_D16: u32 = 14;
pub const P_R32F: u32 = 15;
pub const P_RG32F: u32 = 16;
pub const P_RGBA8UI: u32 = 17;
pub const P_RGBA8SN: u32 = 18;
pub const P_RGBA8SI: u32 = 19;
pub const P_R32UI: u32 = 20;
pub const P_R32SI: u32 = 21;
pub const P_RGBA32F: u32 = 22;
pub const P_RGBA32UI: u32 = 23;
pub const P_RGBA32SI: u32 = 24;
pub const P_RG32UI: u32 = 25;
pub const P_RG32SI: u32 = 26;
pub const P_R8SN: u32 = 27;
pub const P_RG8SN: u32 = 28;
pub const P_RGB9E5: u32 = 29;

pub struct Format {
    /// page format
    pub page: u32,
    /// bytes per texel (per 4x4 block for block formats) in the guest's layout
    pub bpp: u32,
    /// block size (1, or 4 for BC formats)
    pub block: u32,
    /// converted on upload with the Direct3D 9 converter as this D3DFORMAT (0: copied as is)
    pub conv: u32,
    /// B8G8R8X8: alpha set to 1 on upload
    pub no_alpha: bool,
}

const fn f(page: u32, bpp: u32) -> Format {
    Format { page, bpp, block: 1, conv: 0, no_alpha: false }
}

const fn c(bpp: u32, block: u32, conv: u32) -> Format {
    Format { page: P_BGRA8, bpp, block, conv, no_alpha: false }
}

pub fn info(dxgi: u32) -> Option<Format> {
    use crate::d3d9::format as f9;
    Some(match dxgi {
        // (the page keeps them as halves without float32-filterable: render/d3d.rs halved)
        1 | 2 => f(P_RGBA32F, 16),
        3 => f(P_RGBA32UI, 16),
        4 => f(P_RGBA32SI, 16),
        17 => f(P_RG32UI, 8),
        18 => f(P_RG32SI, 8),
        9 | 10 => f(P_RGBA16F, 8),
        15 | 16 => f(P_RG32F, 8),
        23 | 24 => f(P_RGB10A2, 4),
        26 => f(P_RG11B10F, 4),
        27 | 28 => f(P_RGBA8, 4),
        29 => f(P_RGBA8_SRGB, 4),
        30 => f(P_RGBA8UI, 4),
        31 => f(P_RGBA8SN, 4),
        32 => f(P_RGBA8SI, 4),
        33 | 34 => f(P_RG16F, 4),
        39 | 41 => f(P_R32F, 4),
        40 => f(P_D32F, 4),
        42 => f(P_R32UI, 4),
        43 => f(P_R32SI, 4),
        44 | 45 | 46 => f(P_D24S8, 4),
        48 | 49 => f(P_RG8, 2),
        53 | 54 => f(P_R16F, 2),
        55 => f(P_D16, 2),
        51 => f(P_RG8SN, 2),
        60 | 61 => f(P_R8, 1),
        63 => f(P_R8SN, 1),
        67 => f(P_RGB9E5, 4),
        65 => c(1, 1, f9::A8),
        70 | 71 | 72 => c(8, 4, f9::DXT1),
        73 | 74 | 75 => c(16, 4, f9::DXT3),
        76 | 77 | 78 => c(16, 4, f9::DXT5),
        85 => c(2, 1, f9::R5G6B5),
        86 => c(2, 1, f9::A1R5G5B5),
        87 | 90 => f(P_BGRA8, 4),
        88 | 92 => Format { page: P_BGRA8, bpp: 4, block: 1, conv: 0, no_alpha: true },
        91 | 93 => f(P_BGRA8_SRGB, 4),
        115 => c(2, 1, f9::A4R4G4B4),
        _ => return None,
    })
}

pub fn is_depth(page: u32) -> bool {
    matches!(page, P_D24S8 | P_D32F | P_D16)
}

/// bytes per texel on the page
pub fn page_bpp(page: u32) -> u32 {
    match page {
        P_R8 | P_R8SN => 1,
        P_RG8 | P_RG8SN | P_R16F | P_D16 => 2,
        P_RGBA16F | P_RG32F | P_RG32UI | P_RG32SI => 8,
        P_RGBA32F | P_RGBA32UI | P_RGBA32SI => 16,
        _ => 4,
    }
}

/// The guest's row pitch and row count of a w x h level.
pub fn pitch_rows(dxgi: u32, w: u32, h: u32) -> (u32, u32) {
    match info(dxgi) {
        Some(f) => (w.div_ceil(f.block) * f.bpp, h.div_ceil(f.block)),
        None => (w * 4, h),
    }
}

/// A vertex attribute format: (page code, d3d9.ts VERTEX_FORMATS2 index)
pub fn vertex_format(dxgi: u32) -> Option<u32> {
    // VERTEX_FORMATS2: float32 x1-4 (0-3), uint32 x1-4 (4-7), sint32 x1-4 (8-11), unorm8x4 12,
    // snorm8x4 13, uint8x4 14, sint8x4 15, unorm16x2 16, unorm16x4 17, snorm16x2 18,
    // snorm16x4 19, uint16x2 20, uint16x4 21, sint16x2 22, sint16x4 23, float16x2 24,
    // float16x4 25, unorm8x2 26, uint8x2 27, unorm10-10-10-2 28, unorm8x4-bgra 29, snorm8x2 30,
    // sint8x2 31
    Some(match dxgi {
        41 => 0,
        16 => 1,
        6 => 2,
        2 => 3,
        42 => 4,
        17 => 5,
        7 => 6,
        3 => 7,
        43 => 8,
        18 => 9,
        8 => 10,
        4 => 11,
        28 | 29 => 12,
        31 => 13,
        30 => 14,
        32 => 15,
        35 => 16,
        11 => 17,
        37 => 18,
        13 => 19,
        36 => 20,
        12 => 21,
        38 => 22,
        14 => 23,
        34 => 24,
        10 => 25,
        49 => 26,
        50 => 27,
        24 => 28,
        87 | 88 => 29,
        51 => 30,
        52 => 31,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every f32 against the `half` crate (IEEE 754 binary16, round to nearest even).
    #[test]
    fn f16_bits_matches_ieee_for_every_f32() {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as u64;
        let per = (1u64 << 32).div_ceil(threads);
        let bad: u64 = (0..threads)
            .map(|t| {
                std::thread::spawn(move || {
                    let mut bad = 0u64;
                    for b in t * per..((t + 1) * per).min(1 << 32) {
                        let v = f32::from_bits(b as u32);
                        let got = f16_bits(v);
                        let ok = if v.is_nan() { got & 0x7c00 == 0x7c00 && got & 0x3ff != 0 } else { got == half::f16::from_f32(v).to_bits() };
                        if !ok {
                            if bad == 0 {
                                eprintln!("f16_bits({v:e} = {b:#x}) = {got:#x}, want {:#x}", half::f16::from_f32(v).to_bits());
                            }
                            bad += 1;
                        }
                    }
                    bad
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .sum();
        assert_eq!(bad, 0);
    }
}

/// Kani proofs (formal/kani.sh): the layout arithmetic of every DXGI format.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// No overflow for any level Direct3D 11 allows (16384 x 16384), rows of whole blocks, and
    /// a known format's page texels fit what one guest texel (or block) converts to.
    #[kani::proof]
    fn pitch_rows_every_format() {
        let dxgi: u32 = kani::any_where(|&f| f <= 132);
        let w: u32 = kani::any_where(|&w| (1..=16384).contains(&w));
        let h: u32 = kani::any_where(|&h| (1..=16384).contains(&h));
        let (pitch, rows) = pitch_rows(dxgi, w, h);
        if let Some(f) = info(dxgi) {
            assert!(f.block == 1 || f.block == 4);
            assert!(pitch == w.div_ceil(f.block) * f.bpp && rows == h.div_ceil(f.block));
            assert!(pitch as u64 * rows as u64 <= 16384 * 16384 * 16);
            if f.conv == 0 {
                // copied as is: the page format has the guest's texel size
                assert!(f.block == 1 && page_bpp(f.page) == f.bpp);
            }
        }
    }
}
