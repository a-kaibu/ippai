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

/// srgb_to_linear of every byte, worked out once (three powf a pixel were most of a gray
/// conversion).
fn linear(c: u8) -> f64 {
    static T: std::sync::OnceLock<[f64; 256]> = std::sync::OnceLock::new();
    T.get_or_init(|| std::array::from_fn(|i| srgb_to_linear(i as u8)))[c as usize]
}

/// WIC's gray of an sRGB color: Rec. 709 luminance of the linear light, back to sRGB.
pub fn gray(b: u8, g: u8, r: u8) -> u8 {
    let y = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
    (linear_to_srgb(y) * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// 16bppGray of an sRGB color: the same luminance, to 16 bits.
fn gray16(b: u8, g: u8, r: u8) -> u16 {
    let y = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b);
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
    let Some(mut px) = zeroed(w as usize * h as usize * 4) else { return Vec::new() };
    // (no pixels: nothing to convert, and no zero-sized rows to cut the buffer into)
    if w == 0 || h == 0 {
        return px;
    }
    // (the formats pictures decode to, a row at a time: the per-pixel dispatch below was most
    // of an image load)
    let fast = matches!(f, Fmt::Bgra | Fmt::Bgr32 | Fmt::Bgr24 | Fmt::Rgba | Fmt::Gray8 | Fmt::Idx8);
    if fast {
        let rb = f.row_bytes(w) as usize;
        for (y, dst) in px.chunks_exact_mut(w as usize * 4).enumerate() {
            let start = y * stride as usize;
            let Some(row) = data.get(start..start + rb) else { break };
            match f {
                Fmt::Bgra => dst.copy_from_slice(row),
                Fmt::Bgr32 => {
                    for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                        d.copy_from_slice(&[s[0], s[1], s[2], 255]);
                    }
                }
                Fmt::Bgr24 => {
                    for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(3)) {
                        d.copy_from_slice(&[s[0], s[1], s[2], 255]);
                    }
                }
                Fmt::Rgba => {
                    for (d, s) in dst.chunks_exact_mut(4).zip(row.chunks_exact(4)) {
                        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                    }
                }
                Fmt::Gray8 => {
                    for (d, &g) in dst.chunks_exact_mut(4).zip(row) {
                        d.copy_from_slice(&[g, g, g, 255]);
                    }
                }
                _ => {
                    for (d, &v) in dst.chunks_exact_mut(4).zip(row) {
                        d.copy_from_slice(&palette.get(v as usize).copied().unwrap_or(0xff00_0000).to_le_bytes());
                    }
                }
            }
        }
        return px;
    }
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
    // (the last color's entry: runs of one color look the palette up once)
    let mut last: Option<([u8; 3], u32)> = None;
    for row in 0..h {
        // (rows outside either buffer are not converted: in 64 bits, as a guest rectangle near
        // 4 G wrapped the offset back into the buffer)
        let at = (y as u128 + row as u128) * sw as u128 * 4 + x as u128 * 4;
        let Some(s) = usize::try_from(at).ok().and_then(|at| src.get(at..)?.get(..w as usize * 4)) else { return };
        let Some(d) = usize::try_from(row as u64 * stride as u64).ok().and_then(|o| out.get_mut(o..)?.get_mut(..f.row_bytes(w) as usize)) else { return };
        for (i, p) in s.chunks_exact(4).enumerate() {
            let (b, g, r, a) = (p[0], p[1], p[2], p[3]);
            let w16 = |d: &mut [u8], o: usize, v: u16| d[o..o + 2].copy_from_slice(&v.to_le_bytes());
            let v = match f {
                Fmt::Idx1 | Fmt::Idx2 | Fmt::Idx4 | Fmt::Idx8 => match last {
                    Some((c, v)) if c == [b, g, r] => v,
                    _ => {
                        let v = nearest(palette, bits, b, g, r);
                        last = Some(([b, g, r], v));
                        v
                    }
                },
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
        let Some(s) = usize::try_from((y as u64 + row as u64) * src_stride as u64).ok().and_then(|o| src.get(o..)) else { return };
        let Some(d) = usize::try_from(row as u64 * stride as u64).ok().and_then(|o| out.get_mut(o..)) else { return };
        if bits % 8 == 0 {
            let n = (bits / 8) as usize;
            let (Some(d), Some(s)) = (d.get_mut(..w as usize * n), s.get(x as usize * n..(x as usize + w as usize) * n)) else { return };
            d.copy_from_slice(s);
        } else {
            if (x as u64 + w as u64) * bits as u64 > s.len() as u64 * 8 || w as u64 * bits as u64 > d.len() as u64 * 8 {
                return;
            }
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

/// The most pixels a picture may have: 64 M (256 MiB as BGRA), what sake's heap can still
/// hand out; a header claiming more is no picture (2^28 asked for 1-2 GiB, and a failed
/// allocation ends the VM).
const PIXELS_MAX: u64 = 1 << 26;
/// The pixels all of a GIF's frames may have together (a small file of many maximal frames).
const GIF_PIXELS_MAX: u64 = 1 << 27;

/// `n` zero bytes, or None when sake's heap cannot give them (instead of an abort).
fn zeroed(n: usize) -> Option<Vec<u8>> {
    let mut v = Vec::new();
    v.try_reserve_exact(n).ok()?;
    v.resize(n, 0);
    Some(v)
}

/// The first `rb` bytes of each of `h` rows `line` bytes apart in `buf` (moved when that is
/// all of it), with `px` applied to each `n`-byte pixel in place.
fn rows_of(mut buf: Vec<u8>, line: usize, rb: usize, h: usize, n: usize, px: impl Fn(&mut [u8])) -> Vec<u8> {
    if line != rb {
        for y in 1..h {
            buf.copy_within(y * line..y * line + rb, y * rb);
        }
    }
    buf.truncate(rb * h);
    if n > 0 {
        for p in buf.chunks_exact_mut(n) {
            px(p);
        }
    }
    buf
}

fn png_frame(data: &[u8]) -> Option<Frame> {
    use png::{BitDepth, ColorType};
    let mut d = png::Decoder::new(data);
    d.set_transformations(png::Transformations::IDENTITY);
    let mut r = d.read_info().ok()?;
    let (w, h) = (r.info().width, r.info().height);
    if w as u64 * h as u64 > PIXELS_MAX {
        return None;
    }
    let mut buf = zeroed(r.output_buffer_size())?;
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
            (f, rows_of(buf, line, rb, h as usize, 0, |_| {}))
        }
        (ColorType::Grayscale, _) if trns.is_none() => match out.bit_depth {
            BitDepth::Sixteen => (Fmt::Gray16, rows_of(buf, line, w as usize * 2, h as usize, 2, |p| p.swap(0, 1))),
            _ => {
                let f = match depth {
                    1 => Fmt::BlackWhite,
                    2 => Fmt::Gray2,
                    4 => Fmt::Gray4,
                    _ => Fmt::Gray8,
                };
                let rb = f.row_bytes(w) as usize;
                (f, rows_of(buf, line, rb, h as usize, 0, |_| {}))
            }
        },
        (ColorType::Rgb, BitDepth::Eight) if trns.is_none() => (Fmt::Bgr24, rows_of(buf, line, w as usize * 3, h as usize, 3, |p| p.swap(0, 2))),
        (ColorType::Rgb, BitDepth::Sixteen) if trns.is_none() => (Fmt::Rgb48, rows_of(buf, line, w as usize * 6, h as usize, 2, |p| p.swap(0, 1))),
        (ct, BitDepth::Sixteen) => {
            // gray+alpha, RGBA, or gray/RGB with a tRNS color: 64bppRGBA
            let key: Option<Vec<u16>> = trns.as_ref().map(|t| t.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect());
            let mut v = Vec::new();
            v.try_reserve_exact(w as usize * h as usize * 8).ok()?;
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
            let mut v = Vec::new();
            v.try_reserve_exact(w as usize * h as usize * 4).ok()?;
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
    let (w, h) = (info.width as u32, info.height as u32);
    if let Some(f) = jpeg_page(data, w, h, info.pixel_format) {
        return Some(f);
    }
    let px = d.decode().ok()?;
    let (fmt, bits) = match info.pixel_format {
        PixelFormat::L8 => (Fmt::Gray8, px),
        PixelFormat::L16 => (Fmt::Gray8, px.chunks_exact(2).map(|c| c[0]).collect()),
        PixelFormat::RGB24 => {
            let mut px = px;
            for p in px.chunks_exact_mut(3) {
                p.swap(0, 2);
            }
            (Fmt::Bgr24, px)
        }
        PixelFormat::CMYK32 => (Fmt::Cmyk, px),
    };
    Some(Frame { w, h, fmt, bits, palette: Vec::new(), dpi: jfif_dpi(data) })
}

/// A JPEG decoded by the page (kura::idec: the browser's libjpeg-turbo planes, then libjpeg's
/// upsampling and colors: Windows' own bytes), for gray and YCbCr files big enough
/// (kura::idec::wants); None to decode it here.
fn jpeg_page(data: &[u8], w: u32, h: u32, f: jpeg_decoder::PixelFormat) -> Option<Frame> {
    use jpeg_decoder::PixelFormat;
    if !matches!(f, PixelFormat::L8 | PixelFormat::RGB24) || !kura::idec::wants(w as u64 * h as u64) {
        return None;
    }
    let file = jpeg_for_page(data)?;
    let p = kura::idec::jpeg(&file, kura::idec::TIMEOUT)?;
    if (p.w, p.h) != (w, h) {
        return None;
    }
    let (fmt, bits) = if f == PixelFormat::L8 { (Fmt::Gray8, p.gray()?) } else { (Fmt::Bgr24, p.bgr24()?) };
    Some(Frame { w, h, fmt, bits, palette: Vec::new(), dpi: jfif_dpi(data) })
}

/// `data` for the browser to decode as Windows does: without its APP1 (Exif: the browser would
/// turn the picture by its orientation) and APP2 (ICC: color managed) segments. None for a file
/// with an Adobe APP14 segment (its color transform: left to jpeg-decoder) or segments past
/// the end.
pub fn jpeg_for_page(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = data.get(..2)?.to_vec();
    let mut i = 2;
    loop {
        if *data.get(i)? != 0xff {
            return None;
        }
        let m = *data.get(i + 1)?;
        if m == 0xff {
            // (fill bytes)
            i += 1;
            continue;
        }
        if m == 0xda {
            out.extend_from_slice(&data[i..]);
            return Some(out);
        }
        let len = u16::from_be_bytes([*data.get(i + 2)?, *data.get(i + 3)?]) as usize;
        let seg = data.get(i..i + 2 + len)?;
        if len < 2 || m == 0xee {
            return None;
        }
        if m != 0xe1 && m != 0xe2 {
            out.extend_from_slice(seg);
        }
        i += 2 + len;
    }
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
        let bits = rle(data.get(off..)?, w, h, if comp == 1 { 8 } else { 4 }, &palette, top_down)?;
        return Some(Frame { w, h, fmt: Fmt::Bgr32, bits, palette, dpi });
    }
    // (in 64 bits: 2^27 pixels of 32 bits wrapped a 32-bit usize to a stride of 0)
    let stride = (w as u64 * bpp as u64).div_ceil(32) * 4;
    if off as u64 + stride * h as u64 > data.len() as u64 {
        return None;
    }
    let stride = stride as usize;
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
            let mut bits = Vec::new();
            bits.try_reserve_exact(w as usize * h as usize * 4).ok()?;
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
    let mut bits = Vec::new();
    bits.try_reserve_exact(rb * h as usize).ok()?;
    for y in 0..h {
        bits.extend_from_slice(&row(y)[..rb]);
    }
    Some(Frame { w, h, fmt, bits, palette, dpi })
}

/// RLE8/RLE4 rows as 32bppBGR (pixels the runs skip stay black).
fn rle(src: &[u8], w: u32, h: u32, bits: u32, palette: &[u32], top_down: bool) -> Option<Vec<u8>> {
    let mut out = zeroed(w as usize * h as usize * 4)?;
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
    Some(out)
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
    let mut pixels = 0u64;
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
                // (a 30-byte file can claim 65535 x 65535: 4 GiB asked for each frame)
                pixels += w as u64 * h as u64;
                if w as u64 * h as u64 > PIXELS_MAX || pixels > GIF_PIXELS_MAX {
                    break;
                }
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

    /// kura::idec's page is one a process: one test at a time talks to it.
    static PAGE: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The probe's JPEG files (tests/winref/probes/imaging_data.h), by name.
    fn probe_jpegs() -> Vec<(String, Vec<u8>)> {
        let h = include_str!("../../../tests/winref/probes/imaging_data.h");
        let mut out = Vec::new();
        for part in h.split("static const unsigned char ").skip(1) {
            let name = part.split('[').next().unwrap().to_string();
            if !name.starts_with("jpeg_") {
                continue;
            }
            let body = &part[part.find('{').unwrap() + 1..part.find('}').unwrap()];
            let bytes = body.split(',').map(str::trim).filter(|x| !x.is_empty()).map(|x| u8::from_str_radix(x.trim_start_matches("0x"), 16).unwrap()).collect();
            out.push((name, bytes));
        }
        assert!(out.len() >= 6);
        out
    }

    /// A segment (marker, payload) as a JPEG holds it.
    fn seg(m: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0xff, m];
        v.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// The file for the page decodes to the same pixels; Exif / ICC segments go (also among fill
    /// bytes), others stay; an Adobe segment keeps the file here.
    #[test]
    fn jpeg_for_the_page() {
        for (name, f) in probe_jpegs() {
            let pf = jpeg_for_page(&f);
            if name == "jpeg_cmyk" {
                assert!(pf.is_none(), "{name}");
                continue;
            }
            let pf = pf.unwrap_or_else(|| panic!("{name}"));
            assert_eq!(jpeg_decoder::Decoder::new(&pf[..]).decode().unwrap(), jpeg_decoder::Decoder::new(&f[..]).decode().unwrap(), "{name}");
            let mut with = f[..2].to_vec();
            with.extend(seg(0xe1, b"Exif\0\0MM\0*"));
            with.extend([0xff, 0xff]);
            with.extend(seg(0xe2, b"ICC_PROFILE\0"));
            with.extend(seg(0xfe, b"a comment"));
            with.extend_from_slice(&f[2..]);
            let got = jpeg_for_page(&with).unwrap();
            let mut want = f[..2].to_vec();
            want.extend(seg(0xfe, b"a comment"));
            want.extend_from_slice(&f[2..]);
            assert_eq!(got, want, "{name}");
        }
        let mut adobe = vec![0xff, 0xd8];
        adobe.extend(seg(0xee, b"Adobe\0\x64\0\0\0\0\x01"));
        adobe.extend([0xff, 0xda, 0, 2]);
        assert_eq!(jpeg_for_page(&adobe), None);
    }

    proptest::proptest! {
        /// Any bytes after SOI: no panic, and what goes to the page is the file without some of
        /// its segments (SOI first, the rest from SOS on as it is).
        #[test]
        fn jpeg_for_page_any(tail in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..200), segs in proptest::collection::vec((0xc0u8..=0xfe, proptest::collection::vec(proptest::prelude::any::<u8>(), 0..20)), 0..6)) {
            let mut f = vec![0xff, 0xd8];
            for (m, p) in &segs {
                f.extend(seg(*m, p));
            }
            f.extend([0xff, 0xda]);
            f.extend(&tail);
            if let Some(out) = jpeg_for_page(&f) {
                proptest::prop_assert!(out.starts_with(&[0xff, 0xd8]));
                proptest::prop_assert!(out.len() <= f.len());
                let sos = out.windows(2).position(|w| w == [0xff, 0xda]).unwrap();
                proptest::prop_assert!(f.ends_with(&out[sos..]));
            }
            let _ = jpeg_for_page(&tail[..tail.len().min(3)].iter().copied().chain([0xff]).collect::<Vec<_>>());
        }
    }

    /// Through a page (kura::idec, here a thread decoding with jpeg-decoder, handing over BGRX
    /// as a browser does for gray files): the same frames as decoded here, gray and YCbCr alike.
    #[test]
    fn jpeg_through_the_page() {
        let _one = PAGE.lock().unwrap_or_else(|e| e.into_inner());
        let here: Vec<_> = probe_jpegs().into_iter().map(|(n, f)| (n, decode(&f).map(|p| (p.frames[0].fmt, p.frames[0].bits.clone())))).collect();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let served = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (s2, n2) = (stop.clone(), served.clone());
        let page = std::thread::spawn(move || {
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                kura::idec::test_page_pass(&mut |d| {
                    n2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let mut j = jpeg_decoder::Decoder::new(d);
                    let px = j.decode().ok()?;
                    let i = j.info()?;
                    let bytes = match i.pixel_format {
                        jpeg_decoder::PixelFormat::L8 => px.iter().flat_map(|&g| [g, g, g, 255]).collect(),
                        _ => px.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0], 255]).collect(),
                    };
                    Some(kura::idec::Picture { w: i.width as u32, h: i.height as u32, layout: kura::idec::Layout::Bgrx, bytes })
                });
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        kura::idec::test_page(true);
        for (name, f) in probe_jpegs() {
            let got = decode(&f).map(|p| (p.frames[0].fmt, p.frames[0].bits.clone()));
            assert_eq!(&got, &here.iter().find(|h| h.0 == name).unwrap().1, "{name}");
        }
        kura::idec::test_page(false);
        std::thread::sleep(std::time::Duration::from_millis(30));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        page.join().unwrap();
        // (every gray and YCbCr one went to the page: not CMYK, not 12-bit)
        assert!(served.load(std::sync::atomic::Ordering::Relaxed) >= 6, "{}", served.load(std::sync::atomic::Ordering::Relaxed));
    }

    /// Through a page that answers with Chrome's own planes of the probe's files
    /// (crates/kura/testdata/idec): Windows' pixels (tests/winref/golden/imaging-32), where
    /// jpeg-decoder alone is up to 2 off.
    #[test]
    fn jpeg_through_the_page_as_windows() {
        let _one = PAGE.lock().unwrap_or_else(|e| e.into_inner());
        let planes = kura::idec::test_cases(include_bytes!("../../kura/testdata/idec/planes.bin"));
        let files: Vec<_> = probe_jpegs().into_iter().filter(|(n, _)| n != "jpeg_cmyk").collect();
        let answers: Vec<_> = files.iter().map(|(n, f)| (jpeg_for_page(f).unwrap(), planes.iter().find(|p| &p.0 == n).unwrap().1.clone())).collect();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s2 = stop.clone();
        let page = std::thread::spawn(move || {
            while !s2.load(std::sync::atomic::Ordering::Relaxed) {
                kura::idec::test_page_pass(&mut |d| answers.iter().find(|a| a.0 == d).map(|a| a.1.clone()));
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
        });
        kura::idec::test_page(true);
        let mut off_here = 0;
        for (name, f) in &files {
            let png = png::Decoder::new(std::fs::File::open(format!("{}/../../tests/winref/golden/imaging-32/{name}.png", env!("CARGO_MANIFEST_DIR"))).unwrap());
            let mut r = png::Decoder::read_info(png).unwrap();
            let mut rgb = vec![0; r.output_buffer_size()];
            r.next_frame(&mut rgb).unwrap();
            let want: Vec<u8> = if name == "jpeg_gray" { rgb.chunks_exact(3).map(|p| p[0]).collect() } else { rgb.chunks_exact(3).flat_map(|p| [p[2], p[1], p[0]]).collect() };
            let got = decode(f).unwrap();
            assert_eq!(got.frames[0].bits, want, "{name}");
            kura::idec::test_page(false);
            off_here += (decode(f).unwrap().frames[0].bits != want) as usize;
            kura::idec::test_page(true);
        }
        kura::idec::test_page(false);
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        page.join().unwrap();
        // (what the page is for: decoded here, some are not Windows' bytes)
        assert!(off_here > 0);
    }

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

#[cfg(test)]
mod review_tests {
    use super::*;

    /// fuzz (image): a zero-width picture converts to no pixels in every fast format, not a
    /// panic on zero-sized rows
    #[test]
    fn zero_width_pictures_convert_to_nothing() {
        for f in [Fmt::Bgra, Fmt::Bgr32, Fmt::Bgr24, Fmt::Rgba, Fmt::Gray8, Fmt::Idx8, Fmt::Rgb24] {
            assert!(to_bgra(f, 0, 5, 4, &[0; 64], &[0; 256]).is_empty());
            assert!(to_bgra(f, 5, 0, 20, &[0; 64], &[0; 256]).is_empty());
        }
    }
    use proptest::prelude::*;

    /// to_bgra without its row fast paths: every pixel through pixel_to_bgra (as it was).
    fn generic(f: Fmt, w: u32, h: u32, stride: u32, data: &[u8], palette: &[u32]) -> Vec<u8> {
        let mut px = vec![0u8; (w * h * 4) as usize];
        let bits = f.bits();
        let n = (bits / 8) as usize;
        for y in 0..h as usize {
            let start = y * stride as usize;
            let Some(row) = data.get(start..start + f.row_bytes(w) as usize) else { break };
            for x in 0..w as usize {
                let v = if bits < 8 { get_bits(row, x as u32, bits) } else if bits == 8 { row[x] as u32 } else { 0 };
                let s = if bits >= 8 { &row[x * n..x * n + n] } else { &row[..0] };
                px[(y * w as usize + x) * 4..][..4].copy_from_slice(&pixel_to_bgra(f, s, v, palette));
            }
        }
        px
    }

    fn png(ct: png::ColorType, depth: png::BitDepth, w: u32, h: u32, data: &[u8], palette: Option<&[u8]>) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(ct);
            e.set_depth(depth);
            if let Some(p) = palette {
                e.set_palette(p.to_vec());
            }
            e.write_header().unwrap().write_image_data(data).unwrap();
        }
        out
    }

    /// srgb_to_linear's table is the function, for every byte (so gray and gray16 are too).
    #[test]
    fn linear_table_is_exact() {
        for c in 0..=255u8 {
            assert_eq!(linear(c).to_bits(), srgb_to_linear(c).to_bits(), "{c}");
        }
    }

    /// Headers claiming pictures sake's heap cannot hold are no pictures, without asking for
    /// the memory (each was an allocation of 1-4 GiB, a failure ending the VM): a 30-byte GIF
    /// of 65535 x 65535, an RLE8 BMP of 16384 x 16384, a 32-bit BMP 2^27 pixels wide (its
    /// stride wrapped to 0 in 32 bits).
    #[test]
    fn huge_headers_are_refused() {
        let mut gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec();
        gif.extend([0x2c, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0, 2, 2, 0x4c, 0x01, 0, 0x3b]);
        assert!(gif.len() <= 30);
        assert!(decode(&gif).is_none());
        // (one frame of 4096 x 4096 is fine; 9 such frames are past the file's budget)
        let mut gif = b"GIF89a\x01\x00\x01\x00\x00\x00\x00".to_vec();
        for _ in 0..9 {
            gif.extend([0x2c, 0, 0, 0, 0, 0, 0x10, 0, 0x10, 0, 2, 2, 0x4c, 0x01, 0]);
        }
        let p = decode(&gif).unwrap();
        assert_eq!(p.frames.len(), 8);
        let bmp = |w: i32, h: i32, bpp: u16, comp: u32| {
            let mut b = b"BM".to_vec();
            b.extend(0u32.to_le_bytes());
            b.extend([0; 4]);
            b.extend(54u32.to_le_bytes());
            b.extend(40u32.to_le_bytes());
            b.extend(w.to_le_bytes());
            b.extend(h.to_le_bytes());
            b.extend(1u16.to_le_bytes());
            b.extend(bpp.to_le_bytes());
            b.extend(comp.to_le_bytes());
            b.extend([0; 20]);
            b.extend([0, 1]);
            b
        };
        assert!(decode(&bmp(16384, 16384, 8, 1)).is_none());
        assert!(decode(&bmp(1 << 27, 1, 32, 0)).is_none());
        assert!(decode(&bmp(1 << 26, 1, 32, 0)).is_none(), "(no pixels in the file)");
        // the largest RLE picture still decodes
        let (_, _, px) = decode_bgra(&bmp(8192, 8192, 8, 1)).unwrap();
        assert_eq!(px.len(), 8192 * 8192 * 4);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        /// Every format, any bytes, rows padded or cut short: the row fast paths give what the
        /// per-pixel conversion gives.
        #[test]
        fn fast_paths_match_the_per_pixel_conversion(w in 1u32..24, h in 1u32..8, pad in 0u32..9, cut in 0usize..64, seed in any::<u64>(), npal in 0usize..300) {
            let palette: Vec<u32> = (0..npal as u64).map(|i| (i.wrapping_mul(seed | 1) >> 7) as u32).collect();
            for f in ALL {
                let stride = f.row_bytes(w) + pad;
                let len = (stride * h) as usize - cut.min((stride * h) as usize);
                let data: Vec<u8> = (0..len as u64).map(|i| (i.wrapping_mul(seed | 3) >> 11) as u8).collect();
                prop_assert_eq!(to_bgra(f, w, h, stride, &data, &palette), generic(f, w, h, stride, &data, &palette), "{:?}", f);
            }
        }

        /// PNG files of every kind the moved-row paths take (RGB, gray, 16-bit, indexed): the
        /// frame's BGRA is the source pixels'.
        #[test]
        fn png_rows_are_the_source_pixels(w in 1u32..40, h in 1u32..12, seed in any::<u64>()) {
            use png::{BitDepth as D, ColorType as C};
            let bytes = |n: u32| -> Vec<u8> { (0..n as u64).map(|i| (i.wrapping_mul(seed | 5) >> 13) as u8).collect() };
            let (wn, hn) = (w as usize, h as usize);
            // RGB 8: BGR24 then BGRA
            let src = bytes(w * h * 3);
            let (_, _, px) = decode_bgra(&png(C::Rgb, D::Eight, w, h, &src, None)).unwrap();
            for i in 0..wn * hn { prop_assert_eq!(&px[4 * i..4 * i + 4], &[src[3 * i + 2], src[3 * i + 1], src[3 * i], 255][..]); }
            let f = &decode(&png(C::Rgb, D::Eight, w, h, &src, None)).unwrap().frames[0];
            prop_assert_eq!((f.fmt, f.bits.len()), (Fmt::Bgr24, wn * hn * 3));
            // RGB 16: RGB48 little-endian
            let src = bytes(w * h * 6);
            let f = &decode(&png(C::Rgb, D::Sixteen, w, h, &src, None)).unwrap().frames[0];
            prop_assert_eq!(f.fmt, Fmt::Rgb48);
            for i in 0..wn * hn * 3 { prop_assert_eq!(&f.bits[2 * i..2 * i + 2], &[src[2 * i + 1], src[2 * i]][..]); }
            // gray 16 and 8, 1-bit gray (rows of whole bytes, the padding bits kept)
            let src = bytes(w * h * 2);
            let f = &decode(&png(C::Grayscale, D::Sixteen, w, h, &src, None)).unwrap().frames[0];
            prop_assert_eq!(f.fmt, Fmt::Gray16);
            for i in 0..wn * hn { prop_assert_eq!(&f.bits[2 * i..2 * i + 2], &[src[2 * i + 1], src[2 * i]][..]); }
            let src = bytes(w * h);
            let (_, _, px) = decode_bgra(&png(C::Grayscale, D::Eight, w, h, &src, None)).unwrap();
            for i in 0..wn * hn { prop_assert_eq!(&px[4 * i..4 * i + 4], &[src[i], src[i], src[i], 255][..]); }
            let rb = wn.div_ceil(8);
            let src = bytes((rb * hn) as u32);
            let f = &decode(&png(C::Grayscale, D::One, w, h, &src, None)).unwrap().frames[0];
            prop_assert_eq!((f.fmt, f.stride() as usize), (Fmt::BlackWhite, rb));
            for y in 0..hn { for x in 0..wn { prop_assert_eq!(get_bits(&f.bits[y * rb..], x as u32, 1), get_bits(&src[y * rb..], x as u32, 1)); } }
            // indexed 8 with a 256-color palette
            let pal = bytes(256 * 3);
            let src = bytes(w * h);
            let (_, _, px) = decode_bgra(&png(C::Indexed, D::Eight, w, h, &src, Some(&pal))).unwrap();
            for i in 0..wn * hn {
                let c = &pal[3 * src[i] as usize..][..3];
                prop_assert_eq!(&px[4 * i..4 * i + 4], &[c[2], c[1], c[0], 255][..]);
            }
        }

        /// Any rectangle, stride and buffer sizes (a guest's, near 4 G too): no panic.
        #[test]
        fn rect_copies_never_index_out(src_len in 0usize..600, out_len in 0usize..600, sw in any::<u32>(), stride in prop_oneof![0u32..80, any::<u32>()], rect in (prop_oneof![0u32..16, any::<u32>()], prop_oneof![0u32..16, any::<u32>()], 0u32..20, 0u32..6), fi in 0usize..23) {
            let f = ALL[fi];
            let src = vec![7u8; src_len];
            let mut out = vec![0u8; out_len];
            from_bgra(&src, sw, f, rect, stride, &mut out, &[0xff00_0000, 0xffff_ffff]);
            copy_rect(f, &src, sw, rect, stride, &mut out);
        }

        /// Indexed output: the one-entry cache picks what the palette search picks.
        #[test]
        fn nearest_cache_is_the_search(px in proptest::collection::vec(prop_oneof![Just([1u8, 2, 3, 255]), Just([200, 9, 9, 0]), any::<[u8; 4]>()], 1..64), pal in proptest::collection::vec(any::<u32>(), 1..20)) {
            let pal: Vec<u32> = pal.into_iter().map(|c| c | 0xff00_0000).collect();
            let w = px.len() as u32;
            let src: Vec<u8> = px.concat();
            let mut out = vec![0u8; w as usize];
            from_bgra(&src, w, Fmt::Idx8, (0, 0, w, 1), w, &mut out, &pal);
            for (i, p) in px.iter().enumerate() { prop_assert_eq!(out[i] as u32, nearest(&pal, 8, p[0], p[1], p[2])); }
        }
    }
}
