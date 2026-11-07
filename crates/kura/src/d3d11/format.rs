//! DXGI formats: what the page keeps them as, and how texels get there.

/// page texture formats (d3d9.ts TEX_FORMATS)
/// Format::conv for 32-bit float texels stored as halves (d3d9's converters copy those as they are)
pub const F32_TO_F16: u32 = 0xf32;

/// Format::conv codes of texels converted here (to 32-bit floats): 16-bit UNORM/SNORM channels
/// (WebGPU has no 16-bit normalized formats) and BC4/BC5 blocks (decoded as the hardware does,
/// in floats)
pub const U16N: u32 = 0x1601;
pub const S16N: u32 = 0x1602;
pub const BC4U: u32 = 0xbc41;
pub const BC4S: u32 = 0xbc42;
pub const BC5U: u32 = 0xbc51;
pub const BC5S: u32 = 0xbc52;

/// Texels of a w x h box in the guest's layout (rows `pitch` apart; rows of 4x4 blocks for BC)
/// as 32-bit floats, `n` channels a texel (conversions above). None: not one of them.
///
/// # Safety
/// `src` must hold the box.
pub unsafe fn to_floats(conv: u32, n: u32, src: *const u8, pitch: usize, w: u32, h: u32) -> Option<Vec<u8>> {
    let (w, h) = (w as usize, h as usize);
    let n = n as usize;
    let mut out = vec![0f32; w * h * n];
    match conv {
        U16N | S16N => {
            for y in 0..h {
                let row = src.add(y * pitch);
                for i in 0..w * n {
                    let v = u16::from_le_bytes([*row.add(2 * i), *row.add(2 * i + 1)]);
                    out[y * w * n + i] = if conv == U16N { v as f32 / 65535.0 } else { ((v as i16) as f32 / 32767.0).max(-1.0) };
                }
            }
        }
        BC4U | BC4S | BC5U | BC5S => {
            let signed = conv == BC4S || conv == BC5S;
            let chans = if conv == BC4U || conv == BC4S { 1 } else { 2 };
            let block = 8 * chans;
            for by in 0..h.div_ceil(4) {
                for bx in 0..w.div_ceil(4) {
                    let b = std::slice::from_raw_parts(src.add(by * pitch + bx * block), block);
                    for c in 0..chans {
                        let v = bc4_block(&b[c * 8..c * 8 + 8], signed);
                        for (i, &f) in v.iter().enumerate() {
                            let (x, y) = (bx * 4 + i % 4, by * 4 + i / 4);
                            if x < w && y < h {
                                out[(y * w + x) * n + c] = f;
                            }
                        }
                    }
                }
            }
        }
        _ => return None,
    }
    Some(out.iter().flat_map(|f| f.to_le_bytes()).collect())
}

/// The 16 values of a BC4 block (BC5 is two of them), as the Direct3D spec interpolates.
fn bc4_block(b: &[u8], signed: bool) -> [f32; 16] {
    let (e0, e1, scale) = if signed {
        ((b[0] as i8).max(-127) as f32, (b[1] as i8).max(-127) as f32, 127.0)
    } else {
        (b[0] as f32, b[1] as f32, 255.0)
    };
    let mut pal = [0f32; 8];
    pal[0] = e0;
    pal[1] = e1;
    if e0 > e1 {
        for i in 1..7 {
            pal[i + 1] = ((7 - i) as f32 * e0 + i as f32 * e1) / 7.0;
        }
    } else {
        for i in 1..5 {
            pal[i + 1] = ((5 - i) as f32 * e0 + i as f32 * e1) / 5.0;
        }
        pal[6] = if signed { -127.0 } else { 0.0 };
        pal[7] = scale;
    }
    let bits = u64::from_le_bytes([b[2], b[3], b[4], b[5], b[6], b[7], 0, 0]);
    std::array::from_fn(|i| pal[((bits >> (i * 3)) & 7) as usize] / scale)
}

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

/// f16 bits to f32 (exact)
pub fn f16_to_f32(h: u16) -> f32 {
    let (s, e, m) = ((h as u32 >> 15) << 31, (h as u32 >> 10) & 31, h as u32 & 1023);
    let bits = match e {
        0 if m == 0 => 0,
        // subnormal: normalize
        0 => {
            let k = m.leading_zeros() - 21; // shifts until bit 10 is set
            (113 - k) << 23 | ((m << k) & 1023) << 13
        }
        31 => 0xff << 23 | m << 13,
        _ => (e + 112) << 23 | m << 13,
    };
    f32::from_bits(s | bits)
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
pub const P_RGBA16UI: u32 = 30;
pub const P_RGBA16SI: u32 = 31;
pub const P_RGB10A2UI: u32 = 32;
pub const P_RG16UI: u32 = 33;
pub const P_RG16SI: u32 = 34;
pub const P_RG8UI: u32 = 35;
pub const P_RG8SI: u32 = 36;
pub const P_R16UI: u32 = 37;
pub const P_R16SI: u32 = 38;
pub const P_R8UI: u32 = 39;
pub const P_R8SI: u32 = 40;

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
        // (the integer ones: the same bytes on the page)
        12 => f(P_RGBA16UI, 8),
        14 => f(P_RGBA16SI, 8),
        25 => f(P_RGB10A2UI, 4),
        36 => f(P_RG16UI, 4),
        38 => f(P_RG16SI, 4),
        50 => f(P_RG8UI, 2),
        52 => f(P_RG8SI, 2),
        57 => f(P_R16UI, 2),
        59 => f(P_R16SI, 2),
        62 => f(P_R8UI, 1),
        64 => f(P_R8SI, 1),
        31 => f(P_RGBA8SN, 4),
        32 => f(P_RGBA8SI, 4),
        33 | 34 => f(P_RG16F, 4),
        39 | 41 => f(P_R32F, 4),
        40 => f(P_D32F, 4),
        42 => f(P_R32UI, 4),
        43 => f(P_R32SI, 4),
        44 | 45 | 46 => f(P_D24S8, 4),
        // D32_FLOAT_S8X24 and its typeless / view formats (Unity's default depth buffer): the
        // page has no 32-bit float depth with stencil everywhere (WebGPU's depth32float-stencil8
        // is optional), so 24-bit depth + stencil; 8 bytes a texel in the guest's layout
        19..=22 => f(P_D24S8, 8),
        48 | 49 => f(P_RG8, 2),
        53 | 54 => f(P_R16F, 2),
        55 => f(P_D16, 2),
        51 => f(P_RG8SN, 2),
        11 => Format { page: P_RGBA32F, bpp: 8, block: 1, conv: U16N, no_alpha: false },
        13 => Format { page: P_RGBA32F, bpp: 8, block: 1, conv: S16N, no_alpha: false },
        35 => Format { page: P_RG32F, bpp: 4, block: 1, conv: U16N, no_alpha: false },
        37 => Format { page: P_RG32F, bpp: 4, block: 1, conv: S16N, no_alpha: false },
        56 => Format { page: P_R32F, bpp: 2, block: 1, conv: U16N, no_alpha: false },
        58 => Format { page: P_R32F, bpp: 2, block: 1, conv: S16N, no_alpha: false },
        79 | 80 => Format { page: P_R32F, bpp: 8, block: 4, conv: BC4U, no_alpha: false },
        81 => Format { page: P_R32F, bpp: 8, block: 4, conv: BC4S, no_alpha: false },
        82 | 83 => Format { page: P_RG32F, bpp: 16, block: 4, conv: BC5U, no_alpha: false },
        84 => Format { page: P_RG32F, bpp: 16, block: 4, conv: BC5S, no_alpha: false },
        60 | 61 => f(P_R8, 1),
        63 => f(P_R8SN, 1),
        67 => f(P_RGB9E5, 4),
        65 => c(1, 1, f9::A8),
        70 | 71 => c(8, 4, f9::DXT1),
        73 | 74 => c(16, 4, f9::DXT3),
        76 | 77 => c(16, 4, f9::DXT5),
        // (the _SRGB ones: decoded the same, sampled through sRGB)
        72 => Format { page: P_BGRA8_SRGB, ..c(8, 4, f9::DXT1) },
        75 => Format { page: P_BGRA8_SRGB, ..c(16, 4, f9::DXT3) },
        78 => Format { page: P_BGRA8_SRGB, ..c(16, 4, f9::DXT5) },
        85 => c(2, 1, f9::R5G6B5),
        86 => c(2, 1, f9::A1R5G5B5),
        87 | 90 => f(P_BGRA8, 4),
        88 | 92 => Format { page: P_BGRA8, bpp: 4, block: 1, conv: 0, no_alpha: true },
        91 | 93 => f(P_BGRA8_SRGB, 4),
        115 => c(2, 1, f9::A4R4G4B4),
        _ => return None,
    })
}

/// D3D11_FORMAT_SUPPORT bits of a DXGI format: what the page does with it.
pub fn support(dxgi: u32) -> u32 {
    const VB: u32 = 0x2;
    const IB: u32 = 0x4;
    const TEX: u32 = 0x10 | 0x20 | 0x40 | 0x80; // 1D, 2D, 3D, cube
    const LOAD: u32 = 0x100;
    const SAMPLE: u32 = 0x200;
    const COMPARISON: u32 = 0x400;
    const MIP: u32 = 0x1000;
    const AUTOGEN: u32 = 0x2000;
    const RT: u32 = 0x4000;
    const BLEND: u32 = 0x8000;
    const DS: u32 = 0x10000;
    const LOCK: u32 = 0x20000;
    const DISPLAY: u32 = 0x80000;
    let mut s = if vertex_format(dxgi).is_some() { VB } else { 0 };
    if matches!(dxgi, 42 | 57) {
        s |= IB;
    }
    let Some(f) = info(dxgi) else { return s };
    if is_depth(f.page) {
        return s | 0x10 | 0x20 | 0x80 | MIP | DS | COMPARISON;
    }
    s |= TEX | LOAD | MIP | LOCK;
    let int = matches!(f.page, P_RGBA8UI | P_RGBA8SI | P_R32UI | P_R32SI | P_RGBA32UI | P_RGBA32SI | P_RG32UI | P_RG32SI) || (P_RGBA16UI..=P_R8SI).contains(&f.page);
    if !int {
        s |= SAMPLE | 0x80_0000; // (and SHADER_GATHER)
    }
    // (what the page draws to: not block-compressed, converted, SNORM or shared-exponent texels)
    let renderable = f.block == 1 && f.conv == 0 && !matches!(f.page, P_RGBA8SN | P_R8SN | P_RG8SN | P_RGB9E5);
    if renderable {
        s |= RT;
        if !int {
            s |= BLEND | AUTOGEN;
        }
    }
    if matches!(dxgi, 28 | 29 | 87 | 91 | 24) {
        s |= DISPLAY;
    }
    s
}

pub fn is_depth(page: u32) -> bool {
    matches!(page, P_D24S8 | P_D32F | P_D16)
}

/// bytes per texel on the page
pub fn page_bpp(page: u32) -> u32 {
    match page {
        P_R8 | P_R8SN | P_R8UI | P_R8SI => 1,
        P_RG8 | P_RG8SN | P_R16F | P_D16 | P_RG8UI | P_RG8SI | P_R16UI | P_R16SI => 2,
        P_RGBA16F | P_RG32F | P_RG32UI | P_RG32SI | P_RGBA16UI | P_RGBA16SI => 8,
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
    fn f16_to_f32_round_trips_and_is_exact() {
        for h in 0..=u16::MAX {
            let f = super::f16_to_f32(h);
            if f.is_nan() {
                assert!(h & 0x7c00 == 0x7c00 && h & 1023 != 0);
                continue;
            }
            assert_eq!(super::f16_bits(f), h, "{h:#06x} -> {f}");
            // against the definition: (-1)^s 2^(e-15) (1 + m/1024), subnormals 2^-14 m/1024
            let (e, m) = ((h >> 10) & 31, (h & 1023) as f64);
            let v = if e == 0 { m / 1024.0 * 2f64.powi(-14) } else if e == 31 { f64::INFINITY } else { (1.0 + m / 1024.0) * 2f64.powi(e as i32 - 15) };
            assert_eq!(f.abs() as f64, v, "{h:#06x}");
            assert_eq!(f.is_sign_negative(), h >> 15 != 0);
        }
    }

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
            if f.conv == 0 && !(19..=22).contains(&dxgi) {
                // copied as is: the page format has the guest's texel size (but D32_FLOAT_S8X24,
                // a depth buffer the page keeps as D24S8: never uploaded)
                assert!(f.block == 1 && page_bpp(f.page) == f.bpp);
            }
        }
    }
}

#[cfg(test)]
mod conv_tests {
    use super::*;

    #[test]
    fn bc4_as_the_spec_interpolates() {
        // 8 values: e0 > e1; index 2 is (6 e0 + e1) / 7
        let v = bc4_block(&[200, 10, 0b010_001_000, 0, 0, 0, 0, 0], false);
        assert_eq!(v[0], 200.0 / 255.0);
        assert_eq!(v[1], 10.0 / 255.0);
        assert_eq!(v[2], (6.0 * 200.0 + 10.0) / 7.0 / 255.0);
        // 6 values plus 0 and 1: e0 <= e1, indices 6 and 7
        let v = bc4_block(&[10, 200, 0b110 | 0b111 << 3, 0, 0, 0, 0, 0], false);
        assert_eq!((v[0], v[1]), (0.0, 1.0));
        // signed: -128 reads as -127, the extremes are -1 and 1
        let v = bc4_block(&[0x80, 0x7f, 0b110 | 0b111 << 3, 0, 0, 0, 0, 0], true);
        assert_eq!((v[0], v[1], v[2]), (-1.0, 1.0, -1.0));
    }

    /// Unity's D32_FLOAT_S8X24 depth buffer and the views of it are depth-stencil formats
    #[test]
    fn d32s8_family_is_depth() {
        for dxgi in 19..=22 {
            let f = info(dxgi).unwrap();
            assert!(is_depth(f.page), "{dxgi}");
            assert_ne!(support(dxgi) & 0x10000, 0, "{dxgi}: DEPTH_STENCIL");
            assert_eq!(pitch_rows(dxgi, 1280, 720), (1280 * 8, 720));
        }
    }

    #[test]
    fn unorm16_to_floats() {
        let px: [u16; 4] = [0, 65535, 32768, 1];
        let b: Vec<u8> = px.iter().flat_map(|v| v.to_le_bytes()).collect();
        let f = unsafe { to_floats(U16N, 4, b.as_ptr(), 8, 1, 1) }.unwrap();
        let f: Vec<f32> = f.chunks(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        assert_eq!(f, [0.0, 1.0, 32768.0 / 65535.0, 1.0 / 65535.0]);
    }
}
