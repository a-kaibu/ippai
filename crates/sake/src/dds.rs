//! DirectDraw Surface files (.dds), as D3DX reads them: the header's size, mip levels, cube
//! faces or volume depth and the D3DFORMAT of the pixel format, and where each face's levels
//! are in the file. The pixels are copied as they are (the device has the same formats).
//!
//! Layout (DDS_HEADER after the "DDS " magic): height at 12, width 16, depth 24, mip count 28,
//! the pixel format at 76 (flags 80, FourCC 84, bit count 88, masks 92..108), caps2 at 112;
//! the data from 128, face by face (+X -X +Y -Y +Z -Z, those caps2 names), each face's levels
//! largest first, rows tightly packed (no alignment), blocks of 4x4 for DXTn.

use kura::d3d9::format as f;

const DDPF_ALPHAPIXELS: u32 = 0x1;
const DDPF_ALPHA: u32 = 0x2;
const DDPF_FOURCC: u32 = 0x4;
const DDPF_RGB: u32 = 0x40;
const DDPF_LUMINANCE: u32 = 0x2_0000;
const DDPF_BUMPDUDV: u32 = 0x8_0000;
pub const CUBEMAP: u32 = 0x200;
/// caps2 bits of the faces a cube map file has, in file order
pub const FACES: [u32; 6] = [0x400, 0x800, 0x1000, 0x2000, 0x4000, 0x8000];
pub const VOLUME: u32 = 0x20_0000;
const HEADER: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dds {
    pub w: u32,
    pub h: u32,
    /// slices of a volume texture (1 otherwise)
    pub depth: u32,
    pub levels: u32,
    pub fmt: u32,
    /// bits of the cube faces present (bit k: face k of D3DCUBEMAP_FACES); 0: not a cube map
    pub faces: u32,
}

/// The D3DFORMAT of a DDS pixel format (the 32 bytes at 76), None for one not handled.
pub fn pixel_format(pf: &[u8]) -> Option<u32> {
    let g = |o: usize| u32::from_le_bytes(pf[o..o + 4].try_into().unwrap());
    let (flags, cc, bits, r, gm, b, a) = (g(4), g(8), g(12), g(16), g(20), g(24), g(28));
    if flags & DDPF_FOURCC != 0 {
        // (FourCC DXTn, or a D3DFORMAT number: the float and 16-bit formats)
        return match cc {
            f::DXT1 | f::DXT2 | f::DXT3 | f::DXT4 | f::DXT5 => Some(cc),
            36 | 110..=116 | 81 => Some(cc),
            _ => None,
        };
    }
    let a = if flags & (DDPF_ALPHAPIXELS | DDPF_ALPHA) != 0 { a } else { 0 };
    Some(if flags & DDPF_BUMPDUDV != 0 {
        match (bits, r, gm, b, a) {
            (16, 0xff, 0xff00, 0, 0) => f::V8U8,
            (32, 0xffff, 0xffff_0000, 0, 0) => f::V16U16,
            (32, 0xff, 0xff00, 0xff_0000, 0xff00_0000) => f::Q8W8V8U8,
            _ => return None,
        }
    } else if flags & DDPF_LUMINANCE != 0 {
        match (bits, r, a) {
            (8, 0xff, 0) => f::L8,
            (16, 0xff, 0xff00) => f::A8L8,
            (16, 0xffff, 0) => f::L16,
            (8, 0xf, 0xf0) => f::A4L4,
            _ => return None,
        }
    } else if flags & DDPF_RGB != 0 {
        match (bits, r, gm, b, a) {
            (32, 0xff_0000, 0xff00, 0xff, 0xff00_0000) => f::A8R8G8B8,
            (32, 0xff_0000, 0xff00, 0xff, 0) => f::X8R8G8B8,
            (32, 0xff, 0xff00, 0xff_0000, 0xff00_0000) => f::A8B8G8R8,
            (32, 0xff, 0xff00, 0xff_0000, 0) => f::X8B8G8R8,
            (32, 0x3ff00000, 0xffc00, 0x3ff, 0xc000_0000) => f::A2R10G10B10,
            (32, 0x3ff, 0xffc00, 0x3ff00000, 0xc000_0000) => f::A2B10G10R10,
            (32, 0xffff, 0xffff_0000, 0, 0) => f::G16R16,
            (24, 0xff_0000, 0xff00, 0xff, 0) => f::R8G8B8,
            (16, 0xf800, 0x7e0, 0x1f, 0) => f::R5G6B5,
            (16, 0x7c00, 0x3e0, 0x1f, 0x8000) => f::A1R5G5B5,
            (16, 0x7c00, 0x3e0, 0x1f, 0) => f::X1R5G5B5,
            (16, 0xf00, 0xf0, 0xf, 0xf000) => f::A4R4G4B4,
            (16, 0xf00, 0xf0, 0xf, 0) => f::X4R4G4B4,
            _ => return None,
        }
    } else if flags & DDPF_ALPHA != 0 && bits == 8 && a == 0xff {
        f::A8
    } else {
        return None;
    })
}

/// The header of a DDS file, None if it is not one (or of a format not handled, or its data is
/// shorter than its levels).
pub fn parse(data: &[u8]) -> Option<Dds> {
    if data.len() < HEADER || &data[..4] != b"DDS " {
        return None;
    }
    let g = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let (h, w) = (g(12), g(16));
    let caps2 = g(112);
    let fmt = pixel_format(&data[76..108])?;
    let depth = if caps2 & VOLUME != 0 { g(24).max(1) } else { 1 };
    // (a mip count of 0: one level; never more than the chain down to 1x1)
    let max = 32 - w.max(h).max(depth).leading_zeros();
    let levels = g(28).clamp(1, max.max(1));
    let faces = if caps2 & CUBEMAP != 0 {
        FACES.iter().enumerate().filter(|(_, &b)| caps2 & b != 0).fold(0, |m, (k, _)| m | 1 << k)
    } else {
        0
    };
    if w == 0 || h == 0 || w > 16384 || h > 16384 || depth > 2048 || caps2 & CUBEMAP != 0 && (faces == 0 || w != h) {
        return None;
    }
    let d = Dds { w, h, depth, levels, fmt, faces };
    let end = HEADER as u64 + d.face_size() * d.face_count() as u64;
    (end <= data.len() as u64).then_some(d)
}

impl Dds {
    /// Bytes of one row of blocks and the rows of blocks of a level (one slice).
    pub fn level_rows(&self, level: u32) -> (u32, u32) {
        let (bpb, bs) = f::layout(self.fmt).unwrap_or((4, 1));
        let (lw, lh) = ((self.w >> level).max(1), (self.h >> level).max(1));
        (lw.div_ceil(bs) * bpb, lh.div_ceil(bs))
    }
    fn level_size(&self, level: u32) -> u64 {
        let (row, rows) = self.level_rows(level);
        row as u64 * rows as u64 * (self.depth >> level).max(1) as u64
    }
    fn face_size(&self) -> u64 {
        (0..self.levels).map(|l| self.level_size(l)).sum()
    }
    /// The faces in the file (1 for a 2D or volume texture).
    pub fn face_count(&self) -> u32 {
        if self.faces == 0 { 1 } else { self.faces.count_ones() }
    }
    /// Where level `level` of face `face` (0..6; 0 for a 2D texture) starts in the file and its
    /// length; None for a face the file doesn't have.
    pub fn level_at(&self, face: u32, level: u32) -> Option<(usize, usize)> {
        if level >= self.levels {
            return None;
        }
        let k = if self.faces == 0 {
            (face == 0).then_some(0)?
        } else {
            if face >= 6 || self.faces & 1 << face == 0 {
                return None;
            }
            (self.faces & ((1 << face) - 1)).count_ones()
        };
        let at = HEADER as u64 + k as u64 * self.face_size() + (0..level).map(|l| self.level_size(l)).sum::<u64>();
        Some((at as usize, self.level_size(level) as usize))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A DDS file: header for the format's pixel format bytes, then `faces` x levels of bytes
    /// counting up (so each level's bytes say where they came from).
    pub fn file(w: u32, h: u32, depth: u32, mips: u32, pf: [u32; 8], caps2: u32, extra: usize) -> Vec<u8> {
        let mut v = vec![0u8; HEADER];
        v[..4].copy_from_slice(b"DDS ");
        let mut put = |o: usize, x: u32| v[o..o + 4].copy_from_slice(&x.to_le_bytes());
        put(4, 124);
        put(12, h);
        put(16, w);
        put(24, depth);
        put(28, mips);
        for (k, x) in pf.iter().enumerate() {
            put(76 + 4 * k, *x);
        }
        put(112, caps2);
        v.resize(HEADER + extra, 0);
        for (i, b) in v[HEADER..].iter_mut().enumerate() {
            *b = (i % 251) as u8;
        }
        v
    }
    const ARGB: [u32; 8] = [32, DDPF_RGB | DDPF_ALPHAPIXELS, 0, 32, 0xff_0000, 0xff00, 0xff, 0xff00_0000];
    const DXT1: [u32; 8] = [32, DDPF_FOURCC, f::DXT1, 0, 0, 0, 0, 0];
    const DXT5: [u32; 8] = [32, DDPF_FOURCC, f::DXT5, 0, 0, 0, 0, 0];

    #[test]
    fn cube_map_of_argb_levels() {
        // 4x4 A8R8G8B8, 3 levels (64 + 16 + 4 bytes per face), all faces
        let caps2 = CUBEMAP | FACES.iter().fold(0, |m, b| m | b);
        let d = parse(&file(4, 4, 0, 3, ARGB, caps2, 6 * 84)).unwrap();
        assert_eq!(d, Dds { w: 4, h: 4, depth: 1, levels: 3, fmt: f::A8R8G8B8, faces: 0x3f });
        assert_eq!(d.level_at(0, 0), Some((128, 64)));
        assert_eq!(d.level_at(0, 2), Some((128 + 80, 4)));
        assert_eq!(d.level_at(5, 1), Some((128 + 5 * 84 + 64, 16)));
        assert_eq!(d.level_at(6, 0), None);
        assert_eq!(d.level_at(0, 3), None);
        // (one byte short: not read)
        assert_eq!(parse(&file(4, 4, 0, 3, ARGB, caps2, 6 * 84 - 1)), None);
    }

    #[test]
    fn partial_cube_and_dxt_blocks() {
        // +Y and -Z only: -Z is the second in the file; DXT1 8x8 (4 blocks of 8 bytes, then
        // 1 block for 4x4, 2x2 and 1x1)
        let d = parse(&file(8, 8, 0, 4, DXT1, CUBEMAP | 0x1000 | 0x8000, 2 * 56)).unwrap();
        assert_eq!(d.faces, 1 << 2 | 1 << 5);
        assert_eq!(d.level_at(2, 0), Some((128, 32)));
        assert_eq!(d.level_at(5, 0), Some((128 + 56, 32)));
        assert_eq!(d.level_at(5, 3), Some((128 + 56 + 48, 8)));
        assert_eq!(d.level_at(0, 0), None);
        // (DXT5: 16-byte blocks; a 2D file has face 0 only; mip count 0 is one level)
        let d = parse(&file(5, 3, 0, 0, DXT5, 0, 32)).unwrap();
        assert_eq!((d.levels, d.faces, d.level_at(0, 0), d.level_at(1, 0)), (1, 0, Some((128, 32)), None));
    }

    #[test]
    fn not_handled() {
        assert_eq!(parse(b"DDS "), None);
        assert_eq!(parse(&file(4, 4, 0, 1, [32, DDPF_FOURCC, u32::from_le_bytes(*b"DX10"), 0, 0, 0, 0, 0], 0, 1 << 10)), None);
        // (a cube map that isn't square, or has no faces)
        assert_eq!(parse(&file(4, 8, 0, 1, ARGB, CUBEMAP | 0xfc00, 1 << 12)), None);
        assert_eq!(parse(&file(4, 4, 0, 1, ARGB, CUBEMAP, 1 << 12)), None);
        let mut png = file(4, 4, 0, 1, ARGB, 0, 64);
        png[..4].copy_from_slice(b"\x89PNG");
        assert_eq!(parse(&png), None);
    }

    #[test]
    fn rgb_masks() {
        let rgb = |bits, r, g, b, a| {
            let mut pf = [0u8; 32];
            for (k, x) in [32, DDPF_RGB | if a != 0 { DDPF_ALPHAPIXELS } else { 0 }, 0, bits, r, g, b, a].iter().enumerate() {
                pf[4 * k..4 * k + 4].copy_from_slice(&x.to_le_bytes());
            }
            pixel_format(&pf)
        };
        assert_eq!(rgb(32, 0xff0000, 0xff00, 0xff, 0), Some(f::X8R8G8B8));
        assert_eq!(rgb(16, 0xf800, 0x7e0, 0x1f, 0), Some(f::R5G6B5));
        assert_eq!(rgb(16, 0x7c00, 0x3e0, 0x1f, 0x8000), Some(f::A1R5G5B5));
        assert_eq!(rgb(16, 0xf00, 0xf0, 0xf, 0xf000), Some(f::A4R4G4B4));
        assert_eq!(rgb(24, 0xff0000, 0xff00, 0xff, 0), Some(f::R8G8B8));
        assert_eq!(rgb(32, 0xff, 0xff00, 0xff0000, 0xff000000), Some(f::A8B8G8R8));
        assert_eq!(rgb(16, 0x1f, 0x7e0, 0xf800, 0), None);
    }

    proptest! {
        /// Any parsed file's levels lie inside it, one after another without overlap, face by
        /// face in file order, and each is rows x row bytes x slices of its level.
        #[test]
        fn levels_tile_the_file(w in 1u32..70, h in 1u32..70, depth in 0u32..5, mips in 0u32..9, pf in 0usize..3, caps2 in any::<u32>(), slack in 0usize..64) {
            let pf = [ARGB, DXT1, DXT5][pf];
            // (the size the header asks for, from a file long enough for anything)
            if let Some(d) = parse(&file(w, h, depth, mips, pf, caps2, 1 << 22)) {
                let mut at = 128;
                for face in 0..6 {
                    for l in 0..d.levels {
                        let Some((o, n)) = d.level_at(face, l) else { continue };
                        prop_assert_eq!(o, at);
                        let (row, rows) = d.level_rows(l);
                        prop_assert_eq!(n as u64, row as u64 * rows as u64 * (d.depth >> l).max(1) as u64);
                        at += n;
                    }
                }
                let need = at - 128;
                prop_assert!(d.levels >= 1 && d.levels <= 32 - d.w.max(d.h).max(d.depth).leading_zeros());
                // exactly long enough parses, one byte short doesn't
                prop_assert!(parse(&file(w, h, depth, mips, pf, caps2, need + slack)).is_some());
                if need > 0 {
                    prop_assert!(parse(&file(w, h, depth, mips, pf, caps2, need - 1)).is_none());
                }
            }
        }

        /// Arbitrary bytes never panic, and what parses has its levels inside the data.
        #[test]
        fn any_bytes(mut data in proptest::collection::vec(any::<u8>(), 0..400), dds in any::<bool>()) {
            if dds && data.len() >= 4 {
                data[..4].copy_from_slice(b"DDS ");
            }
            if let Some(d) = parse(&data) {
                for face in 0..6 {
                    for l in 0..d.levels {
                        if let Some((o, n)) = d.level_at(face, l) {
                            prop_assert!(o + n <= data.len());
                        }
                    }
                }
            }
        }
    }
}
