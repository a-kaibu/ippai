//! DXGI formats: what the page keeps them as, and how texels get there.

/// page texture formats (d3d9.ts TEX_FORMATS)
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
        1 | 2 => Format { page: P_RGBA16F, bpp: 16, block: 1, conv: f9::A32B32G32R32F, no_alpha: false },
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
        60 | 61 => f(P_R8, 1),
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
        P_R8 => 1,
        P_RG8 | P_R16F | P_D16 => 2,
        P_RGBA16F | P_RG32F => 8,
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
