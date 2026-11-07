//! Pictures as Windows decodes them: PNG, JPEG, BMP and GIF into frames that keep the pixel
//! format, palette and resolution the WIC decoders report (an 8-bit PNG palette stays 8bppIndexed,
//! 16-bit PNG stays 48bppRGB, a 565 BMP stays 16bppBGR565, ...), and the WIC pixel formats
//! themselves: their GUIDs and their conversion to and from straight BGRA, with the rounding
//! Windows uses (tests/winref/probes/imaging.c records it). No guest memory here; wic.rs,
//! gdiplus.rs and ole.rs build on it.

/// A WIC pixel format.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fmt {
    Idx1,
    Idx2,
    Idx4,
    Idx8,
    BlackWhite,
    Gray2,
    Gray4,
    Gray8,
    Gray16,
    Bgr555,
    Bgr565,
    Bgr24,
    Rgb24,
    Bgr32,
    Bgra,
    Pbgra,
    Rgb32,
    Rgba,
    Prgba,
    Rgb48,
    Rgba64,
    Cmyk,
    Alpha8,
}

pub type Guid = (u32, u16, u16, [u8; 8]);

pub const ALL: [Fmt; 23] = [
    Fmt::Idx1,
    Fmt::Idx2,
    Fmt::Idx4,
    Fmt::Idx8,
    Fmt::BlackWhite,
    Fmt::Gray2,
    Fmt::Gray4,
    Fmt::Gray8,
    Fmt::Gray16,
    Fmt::Bgr555,
    Fmt::Bgr565,
    Fmt::Bgr24,
    Fmt::Rgb24,
    Fmt::Bgr32,
    Fmt::Bgra,
    Fmt::Pbgra,
    Fmt::Rgb32,
    Fmt::Rgba,
    Fmt::Prgba,
    Fmt::Rgb48,
    Fmt::Rgba64,
    Fmt::Cmyk,
    Fmt::Alpha8,
];

impl Fmt {
    pub fn guid(self) -> Guid {
        let base = |last: u8| (0x6fdd_c324, 0x4e03, 0x4bfe, [0xb1, 0x85, 0x3d, 0x77, 0x76, 0x8d, 0xc9, last]);
        match self {
            Fmt::Idx1 => base(0x01),
            Fmt::Idx2 => base(0x02),
            Fmt::Idx4 => base(0x03),
            Fmt::Idx8 => base(0x04),
            Fmt::BlackWhite => base(0x05),
            Fmt::Gray2 => base(0x06),
            Fmt::Gray4 => base(0x07),
            Fmt::Gray8 => base(0x08),
            Fmt::Bgr555 => base(0x09),
            Fmt::Bgr565 => base(0x0a),
            Fmt::Gray16 => base(0x0b),
            Fmt::Bgr24 => base(0x0c),
            Fmt::Rgb24 => base(0x0d),
            Fmt::Bgr32 => base(0x0e),
            Fmt::Bgra => base(0x0f),
            Fmt::Pbgra => base(0x10),
            Fmt::Rgb48 => base(0x15),
            Fmt::Rgba64 => base(0x16),
            Fmt::Cmyk => base(0x1c),
            Fmt::Rgba => (0xf5c7_ad2d, 0x6a8d, 0x43dd, [0xa7, 0xa8, 0xa2, 0x99, 0x35, 0x26, 0x1a, 0xe9]),
            Fmt::Prgba => (0x3cc4_a650, 0xa527, 0x4d37, [0xa9, 0x16, 0x31, 0x42, 0xc7, 0xeb, 0xed, 0xba]),
            Fmt::Rgb32 => (0xd98c_6b95, 0x3efe, 0x47d6, [0xbb, 0x25, 0xeb, 0x17, 0x48, 0xab, 0x0c, 0xf1]),
            Fmt::Alpha8 => (0xe6cd_0116, 0xeeba, 0x4161, [0xaa, 0x85, 0x27, 0xdd, 0x9f, 0xb3, 0xa8, 0x95]),
        }
    }

    pub fn of(g: Guid) -> Option<Fmt> {
        ALL.into_iter().find(|f| f.guid() == g)
    }

    pub fn bits(self) -> u32 {
        match self {
            Fmt::Idx1 | Fmt::BlackWhite => 1,
            Fmt::Idx2 | Fmt::Gray2 => 2,
            Fmt::Idx4 | Fmt::Gray4 => 4,
            Fmt::Idx8 | Fmt::Gray8 | Fmt::Alpha8 => 8,
            Fmt::Gray16 | Fmt::Bgr555 | Fmt::Bgr565 => 16,
            Fmt::Bgr24 | Fmt::Rgb24 => 24,
            Fmt::Rgb48 => 48,
            Fmt::Rgba64 => 64,
            _ => 32,
        }
    }

    pub fn indexed(self) -> bool {
        matches!(self, Fmt::Idx1 | Fmt::Idx2 | Fmt::Idx4 | Fmt::Idx8)
    }

    /// Bytes of a row of `w` pixels, unpadded.
    pub fn row_bytes(self, w: u32) -> u32 {
        ((w as u64 * self.bits() as u64).div_ceil(8)) as u32
    }
}

fn get_bits(row: &[u8], x: u32, bits: u32) -> u32 {
    let bit = x * bits;
    let b = row.get((bit / 8) as usize).copied().unwrap_or(0) as u32;
    (b >> (8 - bits - bit % 8)) & ((1 << bits) - 1)
}

fn put_bits(row: &mut [u8], x: u32, bits: u32, v: u32) {
    let bit = x * bits;
    let shift = 8 - bits - bit % 8;
    let m = (((1u32 << bits) - 1) << shift) as u8;
    let b = &mut row[(bit / 8) as usize];
    *b = (*b & !m) | ((v << shift) as u8 & m);
}

fn srgb_to_linear(c: u8) -> f64 {
    let c = c as f64 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(l: f64) -> f64 {
    if l <= 0.003_130_8 {
        12.92 * l
    } else {
        1.055 * l.powf(1.0 / 2.4) - 0.055
    }
}

/// WIC's gray of an sRGB color: Rec. 709 luminance of the linear light, back to sRGB.
pub fn gray(b: u8, g: u8, r: u8) -> u8 {
    let y = 0.2126 * srgb_to_linear(r) + 0.7152 * srgb_to_linear(g) + 0.0722 * srgb_to_linear(b);
    (linear_to_srgb(y) * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// 16bppGray of an sRGB color: the same luminance, to 16 bits.
fn gray16(b: u8, g: u8, r: u8) -> u16 {
    let y = 0.2126 * srgb_to_linear(r) + 0.7152 * srgb_to_linear(g) + 0.0722 * srgb_to_linear(b);
    (linear_to_srgb(y) * 65535.0 + 0.5).clamp(0.0, 65535.0) as u16
}

/// Premultiplied to straight: what Windows' converters give (truncating).
fn unpremultiply(c: u8, a: u8) -> u8 {
    if a == 0 {
        0
    } else {
        // (in floats, both scaled to 0..1 first: 57 of 171 is 84, not 85)
        ((c as f32 / 255.0) / (a as f32 / 255.0) * 255.0).min(255.0) as u8
    }
}

fn premultiply(c: u8, a: u8) -> u8 {
    ((c as u32 * a as u32 + 127) / 255) as u8
}

/// Windows' 32bppCMYK -> sRGB (through its CMYK color profile), measured on a grid of 9 levels
/// of each ink (tests/winref/golden/wicconv-32/cmyk_grid.png: RGB, cyan fastest, black slowest).
static CMYK_GRID: &[u8; 9 * 9 * 9 * 9 * 3] = include_bytes!("../data/cmyk_grid.rgb");

/// 32bppCMYK as Windows shows it: the measured grid, interpolated between its levels (within a
/// few levels of Windows' own conversion).
pub fn cmyk_to_bgr(c: u8, m: u8, y: u8, k: u8) -> [u8; 3] {
    let mut idx = [0usize; 4];
    let mut t = [0f32; 4];
    for (j, v) in [c, m, y, k].into_iter().enumerate() {
        let p = v as f32 * 8.0 / 255.0;
        idx[j] = (p as usize).min(7);
        t[j] = p - idx[j] as f32;
    }
    let mut rgb = [0f32; 3];
    for corner in 0..16 {
        let mut w = 1.0;
        let mut at = 0;
        for j in 0..4 {
            let bit = (corner >> j) & 1;
            w *= if bit == 1 { t[j] } else { 1.0 - t[j] };
            at += (idx[j] + bit) * [1, 9, 81, 729][j];
        }
        for (ch, out) in rgb.iter_mut().enumerate() {
            *out += w * CMYK_GRID[at * 3 + ch] as f32;
        }
    }
    let q = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u8;
    [q(rgb[2]), q(rgb[1]), q(rgb[0])]
}

/// The first 16bppGray level of each 8-bit gray 1..=255 in Windows' conversion (measured:
/// tests/winref/golden/wicconv-32/gray16_ramp.png).
const GRAY16_TO_8: [u16; 255] = [
    127, 379, 632, 884, 1137, 1389, 1641, 1894, 2146, 2399, 2649, 2892, 3122, 3551, 3752, 3946, 4315, 4490, 4827, 4988, 5298, 5595, 5737, 6015, 6281, 6538, 6786, 7026, 7373, 7596, 7812, 8127, 8330,
    8626, 8913, 9099, 9371, 9635, 9893, 10143, 10387, 10704, 10935, 11161, 11456, 11671, 11952, 12226, 12493, 12690, 12947, 13261, 13506, 13746, 13982, 14270, 14497, 14774, 15046, 15312, 15573,
    15829, 16081, 16328, 16571, 16810, 17091, 17368, 17594, 17862, 18124, 18383, 18637, 18887, 19133, 19416, 19655, 19929, 20160, 20426, 20688, 20946, 21201, 21452, 21734, 21977, 22218, 22489, 22755,
    22986, 23246, 23503, 23787, 24037, 24283, 24557, 24797, 25064, 25328, 25559, 25816, 26098, 26349, 26597, 26869, 27111, 27377, 27640, 27874, 28131, 28410, 28661, 28909, 29179, 29422, 29687, 29948,
    30207, 30462, 30716, 30966, 31237, 31482, 31747, 31988, 32248, 32505, 32759, 33032, 33282, 33529, 33795, 34057, 34317, 34575, 34830, 35083, 35334, 35601, 35847, 36110, 36370, 36627, 36883, 37136,
    37387, 37654, 37901, 38163, 38422, 38680, 38935, 39189, 39456, 39705, 39969, 40214, 40473, 40730, 40985, 41254, 41505, 41769, 42016, 42276, 42534, 42791, 43045, 43312, 43562, 43825, 44072, 44331,
    44588, 44844, 45097, 45363, 45612, 45874, 46134, 46392, 46648, 46902, 47154, 47418, 47680, 47927, 48185, 48442, 48697, 48962, 49214, 49476, 49724, 49982, 50239, 50506, 50760, 51011, 51273, 51534,
    51781, 52037, 52304, 52558, 52810, 53072, 53332, 53590, 53847, 54102, 54356, 54608, 54870, 55129, 55388, 55644, 55899, 56153, 56416, 56666, 56926, 57183, 57440, 57695, 57958, 58210, 58471, 58729,
    58977, 59243, 59497, 59751, 60012, 60262, 60520, 60777, 61042, 61297, 61549, 61810, 62069, 62327, 62583, 62838, 63092, 63353, 63604, 63862, 64119, 64375, 64638, 64891, 65152, 65411,
];

/// One pixel of `f` (its bytes or, below 8 bits, the value) as straight BGRA.
fn pixel_to_bgra(f: Fmt, s: &[u8], v: u32, palette: &[u32]) -> [u8; 4] {
    let le16 = |i: usize| u16::from_le_bytes([s[i], s[i + 1]]);
    let grayn = |v: u32, bits: u32| {
        let g = (v * 255 / ((1 << bits) - 1)) as u8;
        [g, g, g, 255]
    };
    match f {
        Fmt::Idx1 | Fmt::Idx2 | Fmt::Idx4 | Fmt::Idx8 => {
            let c = palette.get(v as usize).copied().unwrap_or(0xff00_0000);
            c.to_le_bytes()
        }
        Fmt::BlackWhite => grayn(v, 1),
        Fmt::Gray2 => grayn(v, 2),
        Fmt::Gray4 => grayn(v, 4),
        Fmt::Gray8 => [s[0], s[0], s[0], 255],
        Fmt::Gray16 => {
            let v = le16(0);
            let g = GRAY16_TO_8.partition_point(|&t| t <= v) as u8;
            [g, g, g, 255]
        }
        Fmt::Bgr555 => {
            let p = le16(0) as u32;
            let x = |v: u32| ((v << 3) | (v >> 2)) as u8;
            [x(p & 31), x((p >> 5) & 31), x((p >> 10) & 31), 255]
        }
        Fmt::Bgr565 => {
            let p = le16(0) as u32;
            let x5 = |v: u32| ((v << 3) | (v >> 2)) as u8;
            let x6 = |v: u32| ((v << 2) | (v >> 4)) as u8;
            [x5(p & 31), x6((p >> 5) & 63), x5(p >> 11), 255]
        }
        Fmt::Bgr24 => [s[0], s[1], s[2], 255],
        Fmt::Rgb24 => [s[2], s[1], s[0], 255],
        Fmt::Bgr32 => [s[0], s[1], s[2], 255],
        Fmt::Bgra => [s[0], s[1], s[2], s[3]],
        Fmt::Pbgra => [unpremultiply(s[0], s[3]), unpremultiply(s[1], s[3]), unpremultiply(s[2], s[3]), s[3]],
        Fmt::Rgb32 => [s[2], s[1], s[0], 255],
        Fmt::Rgba => [s[2], s[1], s[0], s[3]],
        Fmt::Prgba => [unpremultiply(s[2], s[3]), unpremultiply(s[1], s[3]), unpremultiply(s[0], s[3]), s[3]],
        Fmt::Rgb48 => [s[5], s[3], s[1], 255],
        Fmt::Rgba64 => [s[5], s[3], s[1], s[7]],
        Fmt::Cmyk => {
            let [b, g, r] = cmyk_to_bgr(s[0], s[1], s[2], s[3]);
            [b, g, r, 255]
        }
        Fmt::Alpha8 => [255, 255, 255, s[0]],
    }
}

/// `w x h` pixels of `f` in rows of `stride` bytes as straight BGRA (`w * 4` bytes a row).
pub fn to_bgra(f: Fmt, w: u32, h: u32, stride: u32, data: &[u8], palette: &[u32]) -> Vec<u8> {
    let mut px = vec![0u8; (w as usize) * (h as usize) * 4];
    let bits = f.bits();
    let n = (bits / 8) as usize;
    for y in 0..h as usize {
        let start = y * stride as usize;
        let Some(row) = data.get(start..(start + f.row_bytes(w) as usize).min(data.len())) else {
            break;
        };
        if row.len() < f.row_bytes(w) as usize {
            break;
        }
        let dst = &mut px[y * w as usize * 4..][..w as usize * 4];
        for x in 0..w as usize {
            let v = if bits < 8 {
                get_bits(row, x as u32, bits)
            } else if bits == 8 {
                row[x] as u32
            } else {
                0
            };
            let s = if bits >= 8 { &row[x * n..x * n + n] } else { &row[..0] };
            dst[x * 4..x * 4 + 4].copy_from_slice(&pixel_to_bgra(f, s, v, palette));
        }
    }
    px
}

/// The palette entry closest to a color (for converting to indexed formats).
/// Windows picks among the first 2^bits opaque entries by RGB distance, lowest index on ties.
fn nearest(palette: &[u32], bits: u32, b: u8, g: u8, r: u8) -> u32 {
    let mut best = (u32::MAX, 0);
    for (i, &c) in palette.iter().take(1 << bits).enumerate() {
        let e = c.to_le_bytes();
        if e[3] != 255 {
            continue;
        }
        let d = |x: u8, y: u8| (x as i32 - y as i32).pow(2) as u32;
        let dist = d(e[0], b) + d(e[1], g) + d(e[2], r);
        if dist < best.0 {
            best = (dist, i as u32);
        }
    }
    best.1
}

/// Straight BGRA pixels `src` (`sw` pixels a row) of rectangle (x, y, w, h) written in `f`
/// to `out`, rows of `stride` bytes. Indexed formats take the nearest entry of `palette`.
pub fn from_bgra(src: &[u8], sw: u32, f: Fmt, (x, y, w, h): (u32, u32, u32, u32), stride: u32, out: &mut [u8], palette: &[u32]) {
    let bits = f.bits();
    let n = (bits / 8) as usize;
    for row in 0..h {
        let s = &src[(((y + row) * sw + x) * 4) as usize..][..(w * 4) as usize];
        let d = &mut out[(row * stride) as usize..][..f.row_bytes(w) as usize];
        for (i, p) in s.chunks_exact(4).enumerate() {
            let (b, g, r, a) = (p[0], p[1], p[2], p[3]);
            let w16 = |d: &mut [u8], o: usize, v: u16| d[o..o + 2].copy_from_slice(&v.to_le_bytes());
            let v = match f {
                Fmt::Idx1 | Fmt::Idx2 | Fmt::Idx4 | Fmt::Idx8 => nearest(palette, bits, b, g, r),
                Fmt::BlackWhite => (gray(b, g, r) >= 128) as u32,
                Fmt::Gray2 => (gray(b, g, r) as u32 * 3 + 127) / 255,
                Fmt::Gray4 => gray(b, g, r) as u32 >> 4,
                _ => 0,
            };
            if bits < 8 {
                put_bits(d, i as u32, bits, v);
                continue;
            }
            let o = i * n;
            match f {
                Fmt::Idx8 => d[o] = v as u8,
                Fmt::Gray8 => d[o] = gray(b, g, r),
                Fmt::Alpha8 => d[o] = a,
                Fmt::Gray16 => w16(d, o, gray16(b, g, r)),
                Fmt::Bgr555 => w16(d, o, ((r as u16 >> 3) << 10) | ((g as u16 >> 3) << 5) | (b as u16 >> 3)),
                Fmt::Bgr565 => w16(d, o, ((r as u16 >> 3) << 11) | ((g as u16 >> 2) << 5) | (b as u16 >> 3)),
                Fmt::Bgr24 => d[o..o + 3].copy_from_slice(&[b, g, r]),
                Fmt::Rgb24 => d[o..o + 3].copy_from_slice(&[r, g, b]),
                // (the fourth byte is copied, not set: what Windows does)
                Fmt::Bgr32 | Fmt::Bgra => d[o..o + 4].copy_from_slice(&[b, g, r, a]),
                Fmt::Rgba => d[o..o + 4].copy_from_slice(&[r, g, b, a]),
                Fmt::Rgb32 => d[o..o + 4].copy_from_slice(&[r, g, b, 255]),
                Fmt::Pbgra => d[o..o + 4].copy_from_slice(&[premultiply(b, a), premultiply(g, a), premultiply(r, a), a]),
                Fmt::Prgba => d[o..o + 4].copy_from_slice(&[premultiply(r, a), premultiply(g, a), premultiply(b, a), a]),
                Fmt::Rgb48 => {
                    for (k, c) in [r, g, b].into_iter().enumerate() {
                        w16(d, o + 2 * k, c as u16 * 257);
                    }
                }
                Fmt::Rgba64 => {
                    for (k, c) in [r, g, b, a].into_iter().enumerate() {
                        w16(d, o + 2 * k, c as u16 * 257);
                    }
                }
                Fmt::Cmyk => {
                    let k = 255 - r.max(g).max(b);
                    let c = |v: u8| {
                        if k == 255 {
                            0
                        } else {
                            ((255 - v as u32 - k as u32) * 255 / (255 - k as u32)) as u8
                        }
                    };
                    d[o..o + 4].copy_from_slice(&[c(r), c(g), c(b), k]);
                }
                _ => {}
            }
        }
    }
}

/// Rectangle (x, y, w, h) of rows `src` (`src_stride` bytes each) of format `f` to `out`
/// (rows of `stride`), bit-exact (sub-byte formats shifted to start each row).
pub fn copy_rect(f: Fmt, src: &[u8], src_stride: u32, (x, y, w, h): (u32, u32, u32, u32), stride: u32, out: &mut [u8]) {
    let bits = f.bits();
    for row in 0..h {
        let s = &src[((y + row) * src_stride) as usize..];
        let d = &mut out[(row * stride) as usize..];
        if bits % 8 == 0 {
            let n = (bits / 8) as usize;
            d[..w as usize * n].copy_from_slice(&s[x as usize * n..(x + w) as usize * n]);
        } else {
            for i in 0..w {
                put_bits(d, i, bits, get_bits(s, x + i, bits));
            }
        }
    }
}

// ---- pictures ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Png,
    Jpeg,
    Bmp,
    Gif,
}

/// One frame: its pixels in their own format (tight rows of `fmt.row_bytes(w)`), palette as
/// ARGB colors (empty: none), dots per inch.
pub struct Frame {
    pub w: u32,
    pub h: u32,
    pub fmt: Fmt,
    pub bits: Vec<u8>,
    pub palette: Vec<u32>,
    pub dpi: (f64, f64),
}

impl Frame {
    pub fn stride(&self) -> u32 {
        self.fmt.row_bytes(self.w)
    }

    /// The pixels as straight BGRA.
    pub fn bgra(&self) -> Vec<u8> {
        to_bgra(self.fmt, self.w, self.h, self.stride(), &self.bits, &self.palette)
    }
}

pub struct Picture {
    pub kind: Kind,
    pub frames: Vec<Frame>,
    /// a GIF's global color table (the decoder's CopyPalette)
    pub palette: Vec<u32>,
}

impl Picture {
    /// GDI+'s pixel format of the picture (GdipGetImagePixelFormat).
    pub fn gdiplus_format(&self) -> u32 {
        const P1: u32 = 0x0003_0101;
        const P4: u32 = 0x0003_0402;
        const P8: u32 = 0x0003_0803;
        const RGB24: u32 = 0x0002_1808;
        const RGB32: u32 = 0x0002_2009;
        const ARGB: u32 = 0x0026_200a;
        const CMYK: u32 = 0x0000_200f;
        let f = &self.frames[0];
        let alpha = f.palette.iter().any(|c| c >> 24 != 255);
        match (self.kind, f.fmt) {
            (Kind::Gif, _) if self.frames.len() > 1 => ARGB,
            (Kind::Gif, _) => P8,
            (Kind::Jpeg, Fmt::Gray8) => P8,
            (Kind::Jpeg, Fmt::Cmyk) => CMYK,
            (Kind::Jpeg, _) => RGB24,
            (_, Fmt::Idx1 | Fmt::BlackWhite) if !alpha => P1,
            (_, Fmt::Idx4) if !alpha => P4,
            (Kind::Bmp, Fmt::Idx8) => P8,
            (_, Fmt::Bgr24) => RGB24,
            (Kind::Bmp, Fmt::Bgr32 | Fmt::Bgr555 | Fmt::Bgr565) => RGB32,
            _ => ARGB,
        }
    }
}

/// Every frame of a PNG, JPEG, BMP or GIF file, as Windows' decoders give them.
pub fn decode(data: &[u8]) -> Option<Picture> {
    let (kind, frames, palette) = if data.starts_with(b"\x89PNG") {
        (Kind::Png, vec![png_frame(data)?], Vec::new())
    } else if data.starts_with(&[0xff, 0xd8]) {
        (Kind::Jpeg, vec![jpeg_frame(data)?], Vec::new())
    } else if data.starts_with(b"BM") {
        (Kind::Bmp, vec![bmp_frame(data)?], Vec::new())
    } else if data.starts_with(b"GIF8") {
        let (frames, global) = gif(data)?;
        (Kind::Gif, frames, global)
    } else {
        return None;
    };
    Some(Picture { kind, frames, palette })
}

/// The first frame as straight BGRA (OleLoadPicture, D3DX, GDI+'s pixels).
pub fn decode_bgra(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let p = decode(data)?;
    let f = &p.frames[0];
    Some((f.w, f.h, f.bgra()))
}

const PIXELS_MAX: u64 = 1 << 28;

fn png_frame(data: &[u8]) -> Option<Frame> {
    use png::{BitDepth, ColorType};
    let mut d = png::Decoder::new(data);
    d.set_transformations(png::Transformations::IDENTITY);
    let mut r = d.read_info().ok()?;
    let (w, h) = (r.info().width, r.info().height);
    if w as u64 * h as u64 > PIXELS_MAX {
        return None;
    }
    let mut buf = vec![0; r.output_buffer_size()];
    let out = r.next_frame(&mut buf).ok()?;
    let line = out.line_size;
    let info = r.info();
    let depth = info.bit_depth as u8 as u32;
    let dpi = match info.pixel_dims {
        Some(png::PixelDimensions { xppu, yppu, unit: png::Unit::Meter }) => (xppu as f64 * 0.0254, yppu as f64 * 0.0254),
        _ => (96.0, 96.0),
    };
    let trns = info.trns.as_ref().map(|t| t.to_vec());
    let rows = |y: usize| &buf[y * line..(y + 1) * line];
    let be16 = |r: &[u8], i: usize| u16::from_be_bytes([r[2 * i], r[2 * i + 1]]);
    let mut palette = Vec::new();
    let (fmt, bits) = match (out.color_type, out.bit_depth) {
        (ColorType::Indexed, _) => {
            let p = info.palette.as_ref()?;
            let t = trns.clone().unwrap_or_default();
            palette = p.chunks_exact(3).enumerate().map(|(i, c)| (*t.get(i).unwrap_or(&255) as u32) << 24 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32).collect();
            let f = match depth {
                1 => Fmt::Idx1,
                2 => Fmt::Idx2,
                4 => Fmt::Idx4,
                _ => Fmt::Idx8,
            };
            let rb = f.row_bytes(w) as usize;
            (f, (0..h as usize).flat_map(|y| rows(y)[..rb].to_vec()).collect())
        }
        (ColorType::Grayscale, _) if trns.is_none() => match out.bit_depth {
            BitDepth::Sixteen => (Fmt::Gray16, (0..h as usize).flat_map(|y| (0..w as usize).flat_map(move |x| be16(rows(y), x).to_le_bytes())).collect()),
            _ => {
                let f = match depth {
                    1 => Fmt::BlackWhite,
                    2 => Fmt::Gray2,
                    4 => Fmt::Gray4,
                    _ => Fmt::Gray8,
                };
                let rb = f.row_bytes(w) as usize;
                (f, (0..h as usize).flat_map(|y| rows(y)[..rb].to_vec()).collect())
            }
        },
        (ColorType::Rgb, BitDepth::Eight) if trns.is_none() => {
            (Fmt::Bgr24, (0..h as usize).flat_map(|y| rows(y)[..w as usize * 3].chunks_exact(3).flat_map(|p| [p[2], p[1], p[0]]).collect::<Vec<_>>()).collect())
        }
        (ColorType::Rgb, BitDepth::Sixteen) if trns.is_none() => (Fmt::Rgb48, (0..h as usize).flat_map(|y| (0..w as usize * 3).flat_map(move |i| be16(rows(y), i).to_le_bytes())).collect()),
        (ct, BitDepth::Sixteen) => {
            // gray+alpha, RGBA, or gray/RGB with a tRNS color: 64bppRGBA
            let key: Option<Vec<u16>> = trns.as_ref().map(|t| t.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect());
            let mut v = Vec::with_capacity((w * h * 8) as usize);
            for y in 0..h as usize {
                let r = rows(y);
                for x in 0..w as usize {
                    let (c, a) = match ct {
                        ColorType::Grayscale => {
                            let g = be16(r, x);
                            ([g, g, g], if key.as_deref() == Some(&[g]) { 0 } else { 65535 })
                        }
                        ColorType::GrayscaleAlpha => {
                            let g = be16(r, 2 * x);
                            ([g, g, g], be16(r, 2 * x + 1))
                        }
                        ColorType::Rgb => {
                            let c = [be16(r, 3 * x), be16(r, 3 * x + 1), be16(r, 3 * x + 2)];
                            (c, if key.as_deref() == Some(&c[..]) { 0 } else { 65535 })
                        }
                        _ => ([be16(r, 4 * x), be16(r, 4 * x + 1), be16(r, 4 * x + 2)], be16(r, 4 * x + 3)),
                    };
                    for k in [c[0], c[1], c[2], a] {
                        v.extend_from_slice(&k.to_le_bytes());
                    }
                }
            }
            (Fmt::Rgba64, v)
        }
        (ct, _) => {
            // gray+alpha, RGBA, gray/RGB with tRNS (any depth below 16): 32bppBGRA
            let mut v = Vec::with_capacity((w * h * 4) as usize);
            let max = (1u32 << depth) - 1;
            for y in 0..h as usize {
                let r = rows(y);
                for x in 0..w as usize {
                    let px = match ct {
                        ColorType::Grayscale => {
                            let raw = if depth < 8 { get_bits(r, x as u32, depth) } else { r[x] as u32 };
                            let g = (raw * 255 / max) as u8;
                            let t = trns.as_ref().map(|t| if t.len() >= 2 { u16::from_be_bytes([t[0], t[1]]) as u32 } else { t.first().copied().unwrap_or(0) as u32 });
                            [g, g, g, if t == Some(raw) { 0 } else { 255 }]
                        }
                        ColorType::GrayscaleAlpha => [r[2 * x], r[2 * x], r[2 * x], r[2 * x + 1]],
                        ColorType::Rgb => {
                            let c = [r[3 * x], r[3 * x + 1], r[3 * x + 2]];
                            // (the crate keeps 8-bit keys as bytes, others as 16-bit values)
                            let t = trns.as_ref().and_then(|t| match t.len() {
                                6 => Some([t[1], t[3], t[5]]),
                                3 => Some([t[0], t[1], t[2]]),
                                _ => None,
                            });
                            [c[2], c[1], c[0], if t == Some(c) { 0 } else { 255 }]
                        }
                        _ => [r[4 * x + 2], r[4 * x + 1], r[4 * x], r[4 * x + 3]],
                    };
                    v.extend_from_slice(&px);
                }
            }
            (Fmt::Bgra, v)
        }
    };
    Some(Frame { w, h, fmt, bits, palette, dpi })
}

fn jpeg_frame(data: &[u8]) -> Option<Frame> {
    use jpeg_decoder::PixelFormat;
    let mut d = jpeg_decoder::Decoder::new(data);
    d.read_info().ok()?;
    let info = d.info()?;
    if info.width as u64 * info.height as u64 > PIXELS_MAX {
        return None;
    }
    let px = d.decode().ok()?;
    let (w, h) = (info.width as u32, info.height as u32);
    let (fmt, bits) = match info.pixel_format {
        PixelFormat::L8 => (Fmt::Gray8, px),
        PixelFormat::L16 => (Fmt::Gray8, px.chunks_exact(2).map(|c| c[0]).collect()),
        PixelFormat::RGB24 => (Fmt::Bgr24, px.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0]]).collect()),
        PixelFormat::CMYK32 => (Fmt::Cmyk, px),
    };
    Some(Frame { w, h, fmt, bits, palette: Vec::new(), dpi: jfif_dpi(data) })
}

/// The JFIF APP0 density (units 1: dpi, 2: dots per cm); 96 without one.
fn jfif_dpi(data: &[u8]) -> (f64, f64) {
    let mut i = 2;
    while i + 4 <= data.len() && data[i] == 0xff {
        let m = data[i + 1];
        let len = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if m == 0xe0 && data.get(i + 4..i + 9) == Some(b"JFIF\0") && i + 16 <= data.len() {
            let (u, x, y) = (data[i + 11], u16::from_be_bytes([data[i + 12], data[i + 13]]) as f64, u16::from_be_bytes([data[i + 14], data[i + 15]]) as f64);
            return match u {
                1 => (x, y),
                2 => (x * 2.54, y * 2.54),
                _ => (96.0, 96.0),
            };
        }
        if m == 0xda || len < 2 {
            break;
        }
        i += 2 + len;
    }
    (96.0, 96.0)
}

fn bmp_frame(data: &[u8]) -> Option<Frame> {
    let u16le = |o: usize| Some(u16::from_le_bytes(data.get(o..o + 2)?.try_into().ok()?));
    let u32le = |o: usize| Some(u32::from_le_bytes(data.get(o..o + 4)?.try_into().ok()?));
    let off = u32le(10)? as usize;
    let hdr = u32le(14)? as usize;
    let core = hdr == 12;
    let (w, hraw, bpp, comp, ppm, clr_used) = if core {
        (u16le(18)? as i32, u16le(20)? as i16 as i32, u16le(24)? as u32, 0, (0, 0), 0)
    } else {
        (u32le(18)? as i32, u32le(22)? as i32, u16le(28)? as u32, u32le(30)?, (u32le(38)? as i32, u32le(42)? as i32), u32le(46)?)
    };
    if w <= 0 || hraw == 0 || (w as u64) * (hraw.unsigned_abs() as u64) > PIXELS_MAX {
        return None;
    }
    let (w, h, top_down) = (w as u32, hraw.unsigned_abs(), hraw < 0);
    let dpi = |p: i32| if p > 0 { p as f64 * 0.0254 } else { 96.0 };
    let dpi = (dpi(ppm.0), dpi(ppm.1));
    // masks: after a 40-byte header for BI_BITFIELDS, or inside a V2+ header
    let masks: Option<[u32; 4]> = match comp {
        3 | 6 => {
            let at = 14 + 40;
            let m = |k: usize| u32le(at + 4 * k);
            Some([m(0)?, m(1)?, m(2)?, if hdr >= 56 || comp == 6 { m(3).unwrap_or(0) } else { 0 }])
        }
        _ => None,
    };
    // palette: after the header (and the 3 masks of a 40-byte BI_BITFIELDS header)
    let pal_at = 14
        + hdr
        + if hdr == 40 && comp == 3 {
            12
        } else if hdr == 40 && comp == 6 {
            16
        } else {
            0
        };
    let entry = if core { 3 } else { 4 };
    let max = if bpp <= 8 { 1usize << bpp } else { 0 };
    let n = if clr_used != 0 && bpp <= 8 { (clr_used as usize).min(256) } else { max };
    let n = n.min(off.saturating_sub(pal_at) / entry);
    // a file cut inside its palette has no pixels either (off lies past its end): not a picture
    if pal_at + n * entry > data.len() {
        return None;
    }
    let palette: Vec<u32> = (0..n)
        .map(|i| {
            let p = &data[pal_at + i * entry..];
            0xff00_0000 | (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32
        })
        .collect();
    if comp == 1 || comp == 2 {
        // RLE8 / RLE4: decoded to 32bppBGR
        let bits = rle(data.get(off..)?, w, h, if comp == 1 { 8 } else { 4 }, &palette, top_down);
        return Some(Frame { w, h, fmt: Fmt::Bgr32, bits, palette, dpi });
    }
    let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
    if off as u64 + stride as u64 * h as u64 > data.len() as u64 {
        return None;
    }
    let row = |y: u32| {
        let sy = if top_down { y } else { h - 1 - y } as usize;
        &data[off + sy * stride..off + sy * stride + stride]
    };
    let fmt = match (bpp, masks) {
        (1, _) => Fmt::Idx1,
        (2, _) => Fmt::Idx2,
        (4, _) => Fmt::Idx4,
        (8, _) => Fmt::Idx8,
        (16, None) | (16, Some([0x7c00, 0x3e0, 0x1f, 0])) => Fmt::Bgr555,
        (16, Some([0xf800, 0x7e0, 0x1f, 0])) => Fmt::Bgr565,
        (24, _) => Fmt::Bgr24,
        (32, None) | (32, Some([0xff0000, 0xff00, 0xff, 0])) => Fmt::Bgr32,
        (32, Some([0xff0000, 0xff00, 0xff, 0xff00_0000])) => Fmt::Bgra,
        (16 | 32, Some(m)) => {
            // other masks: each channel scaled to 8 bits
            let mut bits = Vec::with_capacity((w * h * 4) as usize);
            for y in 0..h {
                let r = row(y);
                for x in 0..w as usize {
                    let v = if bpp == 16 { u16::from_le_bytes([r[2 * x], r[2 * x + 1]]) as u32 } else { u32::from_le_bytes(r[4 * x..4 * x + 4].try_into().unwrap()) };
                    let ch = |m: u32| {
                        if m == 0 {
                            return None;
                        }
                        let (s, n) = (m.trailing_zeros(), (m >> m.trailing_zeros()).trailing_ones());
                        let c = (v & m) >> s;
                        Some(if n >= 8 { (c >> (n - 8)) as u8 } else { (c * 255 / ((1 << n) - 1)) as u8 })
                    };
                    bits.extend_from_slice(&[ch(m[2]).unwrap_or(0), ch(m[1]).unwrap_or(0), ch(m[0]).unwrap_or(0), ch(m[3]).unwrap_or(255)]);
                }
            }
            return Some(Frame { w, h, fmt: if m[3] != 0 { Fmt::Bgra } else { Fmt::Bgr32 }, bits, palette, dpi });
        }
        _ => return None,
    };
    let rb = fmt.row_bytes(w) as usize;
    let bits = (0..h).flat_map(|y| row(y)[..rb].to_vec()).collect();
    Some(Frame { w, h, fmt, bits, palette, dpi })
}

/// RLE8/RLE4 rows as 32bppBGR (pixels the runs skip stay black).
fn rle(src: &[u8], w: u32, h: u32, bits: u32, palette: &[u32], top_down: bool) -> Vec<u8> {
    let mut out = vec![0u8; (w * h * 4) as usize];
    let (mut x, mut y, mut i) = (0u32, 0u32, 0usize);
    let mut put = |x: u32, y: u32, v: u8| {
        if x < w && y < h {
            let row = if top_down { y } else { h - 1 - y };
            let c = palette.get(v as usize).copied().unwrap_or(0xff00_0000).to_le_bytes();
            out[((row * w + x) * 4) as usize..][..4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    };
    while i + 1 < src.len() && y < h {
        let (n, v) = (src[i], src[i + 1]);
        i += 2;
        if n > 0 {
            for k in 0..n as u32 {
                let p = if bits == 8 {
                    v
                } else if k % 2 == 0 {
                    v >> 4
                } else {
                    v & 15
                };
                put(x, y, p);
                x += 1;
            }
            continue;
        }
        match v {
            0 => {
                x = 0;
                y += 1;
            }
            1 => break,
            2 => {
                if i + 1 >= src.len() {
                    break;
                }
                x += src[i] as u32;
                y += src[i + 1] as u32;
                i += 2;
            }
            n => {
                let bytes = if bits == 8 { n as usize } else { (n as usize).div_ceil(2) };
                for k in 0..n as usize {
                    let Some(&b) = src.get(i + if bits == 8 { k } else { k / 2 }) else {
                        break;
                    };
                    let p = if bits == 8 {
                        b
                    } else if k % 2 == 0 {
                        b >> 4
                    } else {
                        b & 15
                    };
                    put(x, y, p);
                    x += 1;
                }
                i += bytes + (bytes & 1);
            }
        }
    }
    out
}

/// A GIF's frames (8bppIndexed, each its descriptor's size, deinterlaced, the transparent
/// index with alpha 0 in its palette) and its global color table.
fn gif(data: &[u8]) -> Option<(Vec<Frame>, Vec<u32>)> {
    let u16le = |o: usize| Some(u16::from_le_bytes(data.get(o..o + 2)?.try_into().ok()?));
    let flags = *data.get(10)?;
    let table = |at: usize, n: usize| -> Option<Vec<u32>> {
        let t = data.get(at..at + 3 * n)?;
        Some(t.chunks_exact(3).map(|c| 0xff00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32).collect())
    };
    let mut i = 13;
    let global = if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        let t = table(i, n)?;
        i += 3 * n;
        t
    } else {
        Vec::new()
    };
    let mut frames = Vec::new();
    let mut transparent: Option<u8> = None;
    let sub_blocks = |mut i: usize| -> (Vec<u8>, usize) {
        let mut v = Vec::new();
        while let Some(&n) = data.get(i) {
            i += 1;
            if n == 0 {
                break;
            }
            v.extend_from_slice(data.get(i..(i + n as usize).min(data.len())).unwrap_or(&[]));
            i += n as usize;
        }
        (v, i)
    };
    while let Some(&b) = data.get(i) {
        match b {
            0x21 => {
                let label = *data.get(i + 1)?;
                let (body, next) = sub_blocks(i + 2);
                if label == 0xf9 && body.len() >= 4 {
                    transparent = (body[0] & 1 != 0).then_some(body[3]);
                }
                i = next;
            }
            0x2c => {
                let (w, h) = (u16le(i + 5)? as u32, u16le(i + 7)? as u32);
                let f = *data.get(i + 9)?;
                i += 10;
                let mut palette = if f & 0x80 != 0 {
                    let n = 2usize << (f & 7);
                    let t = table(i, n)?;
                    i += 3 * n;
                    t
                } else {
                    global.clone()
                };
                if let Some(t) = transparent.take() {
                    if let Some(c) = palette.get_mut(t as usize) {
                        *c &= 0x00ff_ffff;
                    }
                }
                let min = *data.get(i)?;
                let (lzw_data, next) = sub_blocks(i + 1);
                i = next;
                let mut px = lzw(&lzw_data, min, (w * h) as usize);
                px.resize((w * h) as usize, 0);
                if f & 0x40 != 0 {
                    let mut rows = vec![0u8; px.len()];
                    let mut src = 0;
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        for y in (start..h).step_by(step) {
                            rows[(y * w) as usize..][..w as usize].copy_from_slice(&px[src..src + w as usize]);
                            src += w as usize;
                        }
                    }
                    px = rows;
                }
                frames.push(Frame { w, h, fmt: Fmt::Idx8, bits: px, palette, dpi: (96.0, 96.0) });
            }
            _ => break,
        }
    }
    (!frames.is_empty()).then_some((frames, global))
}

/// GIF's variable-width LZW, up to `max` pixels.
fn lzw(src: &[u8], min: u8, max: usize) -> Vec<u8> {
    let min = min.clamp(1, 11) as u32;
    let (clear, end) = (1u32 << min, (1u32 << min) + 1);
    // each code: (prefix code, its last byte, its length)
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut len = vec![0u16; 4096];
    for c in 0..clear {
        suffix[c as usize] = c as u8;
        len[c as usize] = 1;
    }
    let mut out = Vec::with_capacity(max);
    let (mut size, mut next, mut prev) = (min + 1, end + 1, None::<u32>);
    let (mut acc, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    let mut tmp = Vec::new();
    while out.len() < max {
        while nbits < size {
            let Some(&b) = src.get(pos) else { return out };
            acc |= (b as u32) << nbits;
            nbits += 8;
            pos += 1;
        }
        let code = acc & ((1 << size) - 1);
        acc >>= size;
        nbits -= size;
        if code == clear {
            size = min + 1;
            next = end + 1;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        let Some(p) = prev else {
            if code < clear {
                out.push(code as u8);
                prev = Some(code);
            }
            continue;
        };
        let (string_of, first) = if code < next {
            (code, None)
        } else if code == next {
            (p, Some(()))
        } else {
            break;
        };
        tmp.clear();
        let mut c = string_of;
        loop {
            tmp.push(suffix[c as usize]);
            if len[c as usize] <= 1 {
                break;
            }
            c = prefix[c as usize] as u32;
        }
        tmp.reverse();
        let head = tmp[0];
        if first.is_some() {
            tmp.push(head);
        }
        out.extend_from_slice(&tmp[..tmp.len().min(max - out.len())]);
        if next < 4096 {
            prefix[next as usize] = p as u16;
            suffix[next as usize] = head;
            len[next as usize] = len[p as usize] + 1;
            next += 1;
            if next == 1 << size && size < 12 {
                size += 1;
            }
        }
        prev = Some(code);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guids_unique() {
        for f in ALL {
            assert_eq!(Fmt::of(f.guid()), Some(f));
        }
    }

    #[test]
    fn windows_conversions() {
        // the probe's first pixels (tests/winref/golden/imaging-32): BGRA 05 11 ff 00, 2a 6c f7 87
        let px = [5u8, 17, 255, 0, 42, 108, 247, 135];
        let conv = |f: Fmt| {
            let mut out = vec![0u8; f.row_bytes(2) as usize];
            from_bgra(&px, 2, f, (0, 0, 2, 1), out.len() as u32, &mut out, &[]);
            out
        };
        assert_eq!(conv(Fmt::Pbgra), [0, 0, 0, 0, 0x16, 0x39, 0x83, 0x87]);
        assert_eq!(conv(Fmt::Bgr32), [5, 17, 255, 0, 42, 108, 247, 135]);
        assert_eq!(conv(Fmt::Bgr565), [0x80, 0xf8, 0x65, 0xf3]);
        assert_eq!(conv(Fmt::Bgr555), [0x40, 0x7c, 0xa5, 0x79]);
        assert_eq!(conv(Fmt::Gray8), [0x80, 0x96]);
        assert_eq!(conv(Fmt::Alpha8), [0x00, 0x87]);
        assert_eq!(conv(Fmt::Rgba64), [0xff, 0xff, 0x11, 0x11, 0x05, 0x05, 0, 0, 0xf7, 0xf7, 0x6c, 0x6c, 0x2a, 0x2a, 0x87, 0x87]);
        assert_eq!(conv(Fmt::BlackWhite), [0xc0]);
    }

    /// A 1 bpp BMP (40-byte header, 2-color palette, one 8x1 row): whole, it decodes.
    fn bmp_1bpp() -> Vec<u8> {
        let mut b = b"BM".to_vec();
        b.extend(66u32.to_le_bytes()); // file size
        b.extend([0; 4]);
        b.extend(62u32.to_le_bytes()); // pixels at 14 + 40 + 8
        b.extend(40u32.to_le_bytes());
        b.extend(8i32.to_le_bytes());
        b.extend(1i32.to_le_bytes());
        b.extend(1u16.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // bpp
        b.extend([0; 24]); // compression .. important colors
        b.extend([0, 0, 0, 0, 0xff, 0xff, 0xff, 0]); // palette
        b.extend([0b1010_0000, 0, 0, 0]); // the row
        b
    }

    #[test]
    fn bmp_cut_inside_its_palette() {
        // fuzz (image, crash-4d3b0e51...): a palette read past the end of the file panicked
        let whole = bmp_1bpp();
        let f = bmp_frame(&whole).expect("whole");
        assert_eq!((f.w, f.h, f.palette.len()), (8, 1, 2));
        for cut in 0..whole.len() {
            let _ = bmp_frame(&whole[..cut]);
        }
        assert!(bmp_frame(&whole[..14 + 40 + 6]).is_none());
        assert!(bmp_frame(&whole[..14 + 40 + 1]).is_none());
    }

    #[test]
    fn sub_byte_rows() {
        let row = [0b1011_0001u8, 0b1100_0000];
        assert_eq!((0..10).map(|x| get_bits(&row, x, 1)).collect::<Vec<_>>(), [1, 0, 1, 1, 0, 0, 0, 1, 1, 1]);
        let mut out = [0u8; 2];
        copy_rect(Fmt::Idx1, &row, 2, (3, 0, 7, 1), 2, &mut out);
        assert_eq!(out, [0b1000_1110, 0]);
    }

    #[test]
    fn lzw_round() {
        // 4 pixels 0 1 1 0, min code size 2: clear(4) 0 1 1 0 end(5)
        // (the table reaches 8 entries after the third code: 4-bit codes from then on)
        let codes = [(4u32, 3), (0, 3), (1, 3), (1, 3), (0, 4), (5, 4)];
        let (mut acc, mut n, mut bytes) = (0u32, 0, Vec::new());
        for (c, size) in codes {
            acc |= c << n;
            n += size;
            while n >= 8 {
                bytes.push(acc as u8);
                acc >>= 8;
                n -= 8;
            }
        }
        bytes.push(acc as u8);
        assert_eq!(lzw(&bytes, 2, 4), [0, 1, 1, 0]);
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Any BMP header (indexed, any palette size and pixel offset), cut anywhere: decoding
        /// never panics and a frame it returns has the pixels its size says.
        #[test]
        fn bmp_headers_cut_anywhere(bpp in prop::sample::select(vec![1u16, 4, 8, 16, 24, 32]), clr in 0u32..300, off in 0u32..2000, core in any::<bool>(), cut in 0usize..2200, w in 1i32..20, h in -8i32..8) {
            let mut b = b"BM".to_vec();
            b.extend(0u32.to_le_bytes());
            b.extend([0; 4]);
            b.extend(off.to_le_bytes());
            if core {
                b.extend(12u32.to_le_bytes());
                b.extend((w as u16).to_le_bytes());
                b.extend((h as i16).to_le_bytes());
                b.extend(1u16.to_le_bytes());
                b.extend(bpp.to_le_bytes());
            } else {
                b.extend(40u32.to_le_bytes());
                b.extend(w.to_le_bytes());
                b.extend(h.to_le_bytes());
                b.extend(1u16.to_le_bytes());
                b.extend(bpp.to_le_bytes());
                b.extend([0; 16]);
                b.extend(clr.to_le_bytes());
                b.extend([0; 4]);
            }
            b.extend((0..2000u32).map(|i| (i * 37) as u8));
            b.truncate(cut);
            if let Some((fw, fh, px)) = crate::ole::decode(&b) {
                prop_assert_eq!(px.len() as u64, fw as u64 * fh as u64 * 4);
            }
        }

        /// Every non-indexed format, any image and rectangle, rows padded: written from BGRA
        /// and read back, straight formats come back exact (their alpha 255 without one),
        /// premultiplied ones within their rounding, the others within their precision.
        #[test]
        fn formats_round_trip(w in 1u32..12, h in 1u32..8, rect in (0u32..12, 0u32..8, 1u32..12, 1u32..8), pad in 0u32..5, seed in any::<u64>()) {
            let px: Vec<u8> = (0..(w * h * 4) as u64).map(|i| (i.wrapping_mul(seed | 1) >> 9) as u8).collect();
            let (x, y) = (rect.0 % w, rect.1 % h);
            let (rw, rh) = (rect.2.min(w - x), rect.3.min(h - y));
            for f in ALL.into_iter().filter(|f| !f.indexed()) {
                let stride = f.row_bytes(rw) + pad;
                let mut out = vec![0u8; (stride * rh) as usize];
                from_bgra(&px, w, f, (x, y, rw, rh), stride, &mut out, &[]);
                let back = to_bgra(f, rw, rh, stride, &out, &[]);
                for r in 0..rh {
                    for c in 0..rw {
                        let s = &px[(((y + r) * w + x + c) * 4) as usize..][..4];
                        let g = &back[((r * rw + c) * 4) as usize..][..4];
                        let opaque = [s[0], s[1], s[2], 255];
                        match f {
                            Fmt::Bgra | Fmt::Rgba | Fmt::Rgba64 => prop_assert_eq!(g, s, "{:?}", f),
                            Fmt::Bgr32 | Fmt::Rgb32 | Fmt::Bgr24 | Fmt::Rgb24 | Fmt::Rgb48 => prop_assert_eq!(g, &opaque[..], "{:?}", f),
                            Fmt::Pbgra | Fmt::Prgba => {
                                let a = s[3] as i32;
                                prop_assert_eq!(g[3], s[3]);
                                for k in 0..3 {
                                    let err = (g[k] as i32 - s[k] as i32).abs();
                                    if a == 0 { prop_assert_eq!(g[k], 0) } else { prop_assert!(err <= 255 / a + 1, "{:?} {:?} -> {:?}", f, s, g) }
                                }
                            }
                            Fmt::Bgr555 | Fmt::Bgr565 => {
                                for k in 0..3 {
                                    prop_assert!((g[k] as i32 - s[k] as i32).abs() <= 7, "{:?} {:?} -> {:?}", f, s, g);
                                }
                            }
                            Fmt::Alpha8 => prop_assert_eq!(g[3], s[3]),
                            // (Windows' CMYK goes through a color profile both ways: not a round trip)
                            Fmt::Cmyk => prop_assert_eq!(g[3], 255),
                            _ => {
                                // grays: one level of the format, and gray between the darkest and brightest channel
                                let (lo, hi) = (*s[..3].iter().min().unwrap() as i32, *s[..3].iter().max().unwrap() as i32);
                                let step = 255 / ((1i32 << f.bits().min(8)) - 1);
                                prop_assert!(g[0] as i32 >= lo - step && g[0] as i32 <= hi + step, "{:?} {:?} -> {:?}", f, s, g);
                                prop_assert_eq!((g[1], g[2], g[3]), (g[0], g[0], 255));
                            }
                        }
                    }
                }
            }
        }
    }
}
