//! What CreateTexture1D/2D/3D and CreateBuffer accept, as Windows decides it (the d3d11api
//! winref probe's golden output; per-format answers in `winfmt`).

use super::winfmt::{self, FORMATS};

pub const E_INVALIDARG: u32 = 0x8007_0057;

const USAGE_DEFAULT: u32 = 0;
const USAGE_IMMUTABLE: u32 = 1;
const USAGE_DYNAMIC: u32 = 2;
const USAGE_STAGING: u32 = 3;

const CPU_WRITE: u32 = 0x10000;
const CPU_READ: u32 = 0x20000;

const BIND_VB: u32 = 0x1;
const BIND_IB: u32 = 0x2;
const BIND_CB: u32 = 0x4;
const BIND_SRV: u32 = 0x8;
const BIND_SO: u32 = 0x10;
const BIND_RT: u32 = 0x20;
const BIND_DS: u32 = 0x40;
const BIND_UAV: u32 = 0x80;

const MISC_GENMIPS: u32 = 0x1;
const MISC_SHARED: u32 = 0x2;
const MISC_CUBE: u32 = 0x4;
const MISC_INDIRECT: u32 = 0x10;
const MISC_RAW: u32 = 0x20;
const MISC_STRUCTURED: u32 = 0x40;
const MISC_KEYEDMUTEX: u32 = 0x100;
const MISC_GDI: u32 = 0x200;

/// Windows' row for a DXGI format (None: no such format). The video formats of planes or of
/// pixel pairs (P010..IA44, 104-112: optional, a device without video support answers E_FAIL for
/// them) as that device has them: the page keeps none of them, and a texture said to work but
/// kept nowhere would draw black. (AYUV, Y410, Y416, P8, A8P8 and B4G4R4A4 it keeps: texels of
/// one size, their bytes kept for Map and copies.) NV12 it keeps (its bytes, the luma plane as
/// an R8 texture and the chroma one as R8G8 for views of them, see mod.rs `Nv12`): Windows' row
/// less multisampling and the video decoder's, processor's and encoder's bits.
pub fn format(fmt: u32) -> Option<(bool, u32, bool, u32, [u8; 3], u32)> {
    FORMATS.get(fmt as usize).copied().filter(|_| fmt != 0).map(|f| {
        if fmt == NV12 {
            (true, f.1 & NV12_SUPPORT, false, 0, [0; 3], f.5)
        } else if UNKEPT.contains(&fmt) {
            (false, 0, false, 0, [0; 3], f.5)
        } else {
            f
        }
    })
}

/// The video formats the page keeps no texture of (see `format`)
pub const UNKEPT: std::ops::RangeInclusive<u32> = 104..=112;
pub const NV12: u32 = 103;
/// D3D11_FORMAT_SUPPORT_TEXTURE2D, SHADER_LOAD, SHADER_SAMPLE, RENDER_TARGET, BLENDABLE,
/// CPU_LOCKABLE, SHADER_GATHER, TYPED_UNORDERED_ACCESS_VIEW (render targets and UAVs: of a plane,
/// as R8 or R8G8)
const NV12_SUPPORT: u32 = 0x20 | 0x100 | 0x200 | 0x4000 | 0x8000 | 0x2_0000 | 0x80_0000 | 0x200_0000;

/// CheckFormatSupport: the bits, or None (E_FAIL)
pub fn support(fmt: u32) -> Option<u32> {
    format(fmt).filter(|f| f.0).map(|f| f.1)
}

/// CheckFeatureSupport(FORMAT_SUPPORT2): the bits, or None (E_FAIL)
pub fn support2(fmt: u32) -> Option<u32> {
    format(fmt).filter(|f| f.2).map(|f| f.3)
}

/// CheckMultisampleQualityLevels: levels at `count` samples (0: not supported)
pub fn quality_levels(fmt: u32, count: u32) -> u32 {
    let Some(f) = format(fmt).filter(|f| f.0) else { return 0 };
    match count {
        1 => 1,
        2 => f.4[0] as u32,
        4 => f.4[1] as u32,
        // (16: what the probe's runner took for 8 bits of RGBA, the most a format has)
        8 | 16 => f.4[2] as u32,
        _ => 0,
    }
}

/// A texture's description: dimension 1, 2 or 3; `layers` is the depth of a 3D one.
#[derive(Clone, Copy, Debug, Default)]
pub struct Tex {
    pub dim: u32,
    pub w: u32,
    pub h: u32,
    pub layers: u32,
    pub mips: u32,
    pub fmt: u32,
    pub samples: u32,
    pub quality: u32,
    pub usage: u32,
    pub bind: u32,
    pub cpu: u32,
    pub misc: u32,
}

/// Levels of a full chain for these sizes.
pub fn full_chain(w: u32, h: u32, d: u32) -> u32 {
    32 - w.max(h).max(d).max(1).leading_zeros()
}

/// Whether CreateTexture* takes `t` (`init`: initial data given); its mip count if it does.
pub fn texture(t: &Tex, init: bool) -> Result<u32, u32> {
    let bad = Err(E_INVALIDARG);
    let Some((ok, sup, _, _, _, cases)) = format(t.fmt) else { return bad };
    if !ok || t.w == 0 || t.h == 0 || t.layers == 0 || t.samples == 0 {
        return bad;
    }
    if t.dim != 3 && t.layers > 2048 || t.dim == 3 && t.layers > 2048 {
        return bad;
    }
    let max = full_chain(t.w, t.h, if t.dim == 3 { t.layers } else { 1 });
    if t.mips > max {
        return bad;
    }
    let mips = if t.mips == 0 { max } else { t.mips };
    // (NV12: 2D, even sizes, one level, one sample, as Windows takes it)
    if t.fmt == NV12 && (t.dim != 2 || t.w % 2 != 0 || t.h % 2 != 0 || mips != 1 || t.samples != 1) {
        return bad;
    }
    let need = |case: u32| cases & case != 0;
    // (the probe asked for mips, cubes, 1D and 3D textures with a shader resource bind, and
    // for multisampling with a render target one: without that bind, what the format supports)
    let with = |case: u32, bind: u32, support: u32| need(case) || t.bind & bind == 0 && sup & support != 0;
    // the bind flags a texture takes, each as the format allows it
    if t.bind & !(BIND_VB | BIND_IB | BIND_CB | BIND_SRV | BIND_SO | BIND_RT | BIND_DS | BIND_UAV) != 0 {
        return bad;
    }
    let per_bind =
        [(BIND_VB, winfmt::VB), (BIND_IB, winfmt::IB), (BIND_CB, winfmt::CB), (BIND_SRV, winfmt::SRV), (BIND_SO, winfmt::SO), (BIND_RT, winfmt::RT), (BIND_DS, winfmt::DS), (BIND_UAV, winfmt::UAV)];
    if per_bind.iter().any(|&(b, case)| t.bind & b != 0 && !need(case)) || !need(winfmt::NONE) {
        return bad;
    }
    match t.usage {
        USAGE_DEFAULT => {}
        USAGE_IMMUTABLE => {
            if !init || t.cpu != 0 || t.bind & (BIND_RT | BIND_DS | BIND_UAV | BIND_SO) != 0 {
                return bad;
            }
        }
        USAGE_DYNAMIC => {
            if t.cpu != CPU_WRITE || mips != 1 || t.layers != 1 && t.dim != 3 || t.bind & (BIND_RT | BIND_DS | BIND_UAV | BIND_SO) != 0 || !need(winfmt::DYNAMIC) {
                return bad;
            }
        }
        USAGE_STAGING => {
            if t.bind != 0 || t.cpu & (CPU_READ | CPU_WRITE) == 0 || t.samples > 1 || !need(winfmt::STAGING) {
                return bad;
            }
        }
        _ => return bad,
    }
    if t.cpu & !(CPU_READ | CPU_WRITE) != 0 {
        return bad;
    }
    // multisampling: 2D, one level, a sample count with quality levels (or the standard and
    // center patterns, 0xffffffff and 0xfffffffe)
    // (a typeless format casts within its bit layout and has no multisample bits of its own:
    // Windows takes it when the format has quality levels, as Unity 6's 2x MSAA depth texture,
    // R32G8X24_TYPELESS with DEPTH_STENCIL | SHADER_RESOURCE, in キメセク!地雷ちゃん 体験版)
    if t.samples > 1 || t.quality > 0 {
        let levels = quality_levels(t.fmt, t.samples);
        if t.dim != 2 || mips != 1 || levels == 0 || t.quality >= levels && !(t.samples > 1 && t.quality >= 0xffff_fffe) || t.misc & (MISC_CUBE | MISC_GENMIPS) != 0 || t.samples > 1 && !(with(winfmt::MS4, BIND_RT, 0x20_0000) || t.bind & (BIND_RT | BIND_DS) == BIND_DS && sup & 0x10_0000 != 0) {
            return bad;
        }
    }
    if mips > 1 && !with(winfmt::MIPS, BIND_SRV, 0x1000) {
        return bad;
    }
    if t.misc & (MISC_INDIRECT | MISC_RAW | MISC_STRUCTURED) != 0 || t.misc & MISC_SHARED != 0 && t.misc & MISC_KEYEDMUTEX != 0 {
        return bad;
    }
    if t.misc & MISC_CUBE != 0 && (t.dim != 2 || t.layers < 6 || t.w != t.h || !with(winfmt::CUBE, BIND_SRV, 0x80)) {
        return bad;
    }
    if t.misc & MISC_GENMIPS != 0 && (t.bind & (BIND_RT | BIND_SRV) != BIND_RT | BIND_SRV || !need(winfmt::GENMIPS)) {
        return bad;
    }
    if t.misc & MISC_GDI != 0 && (!matches!(t.fmt, 87 | 90 | 91) || t.bind & BIND_RT == 0) {
        return bad;
    }
    if t.dim == 1 && !with(winfmt::TEX1D, BIND_SRV, 0x10) || t.dim == 3 && !with(winfmt::TEX3D, BIND_SRV, 0x40) {
        return bad;
    }
    Ok(mips)
}

/// Whether CreateBuffer takes a buffer of this description; the stride it keeps if it does
/// (only a structured buffer keeps one).
pub fn buffer(size: u32, usage: u32, bind: u32, cpu: u32, misc: u32, stride: u32, init: bool) -> Result<u32, u32> {
    let bad = Err(E_INVALIDARG);
    if size == 0 || bind & !(BIND_VB | BIND_IB | BIND_CB | BIND_SRV | BIND_SO | BIND_RT | BIND_UAV) != 0 || cpu & !(CPU_READ | CPU_WRITE) != 0 {
        return bad;
    }
    // a constant buffer: 16-byte multiples, no other binding
    if bind & BIND_CB != 0 && (size % 16 != 0 || bind != BIND_CB) {
        return bad;
    }
    match usage {
        USAGE_DEFAULT => {
            if cpu != 0 {
                return bad;
            }
        }
        USAGE_IMMUTABLE => {
            if !init || cpu != 0 || bind & (BIND_SO | BIND_RT | BIND_UAV) != 0 {
                return bad;
            }
        }
        USAGE_DYNAMIC => {
            if cpu != CPU_WRITE || bind & (BIND_SO | BIND_RT | BIND_UAV) != 0 {
                return bad;
            }
        }
        USAGE_STAGING => {
            if bind != 0 || cpu == 0 {
                return bad;
            }
        }
        _ => return bad,
    }
    // (a keyed mutex alone is taken: Wine's test_create_buffer)
    if misc & (MISC_GENMIPS | MISC_CUBE | MISC_GDI) != 0 || misc & MISC_SHARED != 0 && misc & MISC_KEYEDMUTEX != 0 {
        return bad;
    }
    if misc & MISC_STRUCTURED != 0 {
        if misc & MISC_RAW != 0 || stride == 0 || stride % 4 != 0 || stride > 2048 || size % stride != 0 || bind & (BIND_VB | BIND_IB | BIND_CB) != 0 {
            return bad;
        }
        return Ok(stride);
    }
    if misc & MISC_RAW != 0 && bind & (BIND_SRV | BIND_UAV) == 0 {
        return bad;
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgba(w: u32, h: u32, mips: u32, layers: u32, bind: u32, usage: u32, cpu: u32, misc: u32, samples: u32) -> Result<u32, u32> {
        texture(&Tex { dim: 2, w, h, layers, mips, fmt: 28, samples, quality: 0, usage, bind, cpu, misc }, false)
    }

    /// sake's stand-in for ANGLE's point sprite geometry shader is a geometry shader (ANGLE
    /// compiles it again on every draw when its creation fails), and only that
    #[test]
    fn point_sprite_stand_in() {
        let mut tag = dxwgsl::POINT_SPRITE_GS.to_vec();
        tag.extend_from_slice(&64f32.to_le_bytes());
        assert_eq!(shader(&tag, 13, 0xb000), Ok(()));
        assert_eq!(shader(&tag, 12, 0xb000), Err(E_INVALIDARG));
        assert_eq!(shader(&tag, 15, 0xb000), Err(E_INVALIDARG));
        assert_eq!(shader(&tag[..8], 13, 0xb000), Err(E_INVALIDARG));
    }

    /// the probe's texture cases, with Windows' answers
    #[test]
    fn texture_cases() {
        let ok = |r: Result<u32, u32>| r.is_ok();
        assert!(!ok(rgba(0, 16, 1, 1, 8, 0, 0, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 8, 0, 0, 0, 0)));
        assert!(ok(rgba(16385, 1, 1, 1, 8, 0, 0, 0, 1)));
        assert!(ok(rgba(4, 4, 1, 2048, 8, 0, 0, 0, 1)));
        assert!(!ok(rgba(4, 4, 1, 2049, 8, 0, 0, 0, 1)));
        assert_eq!(rgba(16, 16, 5, 1, 8, 0, 0, 0, 1), Ok(5));
        assert!(!ok(rgba(16, 16, 6, 1, 8, 0, 0, 0, 1)));
        assert!(!ok(rgba(17, 3, 6, 1, 8, 0, 0, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 8, 1, 0, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 8, 2, 0, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 8, 2, CPU_READ, 0, 1)));
        assert!(ok(rgba(16, 16, 1, 1, 8, 2, CPU_WRITE, 0, 1)));
        assert!(!ok(rgba(16, 16, 2, 1, 8, 2, CPU_WRITE, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 2, 8, 2, CPU_WRITE, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 0x28, 2, CPU_WRITE, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 8, 3, CPU_READ, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 0, 3, 0, 0, 1)));
        assert!(ok(rgba(16, 16, 1, 1, 0, 3, CPU_READ | CPU_WRITE, 0, 1)));
        assert!(ok(rgba(16, 16, 1, 1, 8, 0, CPU_WRITE, 0, 1)));
        assert!(!ok(rgba(16, 16, 1, 5, 8, 0, 0, 4, 1)));
        assert!(ok(rgba(16, 16, 1, 12, 8, 0, 0, 4, 1)));
        assert!(!ok(rgba(16, 8, 1, 6, 8, 0, 0, 4, 1)));
        assert!(!ok(rgba(16, 16, 0, 1, 8, 0, 0, 1, 1)));
        assert!(!ok(rgba(16, 16, 0, 1, 0x20, 0, 0, 1, 1)));
        assert!(ok(rgba(16, 16, 0, 1, 0x28, 0, 0, 1, 1)));
        assert!(ok(rgba(16, 16, 1, 1, 0x28, 0, 0, 2, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 0x28, 0, 0, 0x102, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 0x28, 0, 0, 0x200, 1)));
        assert!(!ok(rgba(16, 16, 1, 1, 8, 0, 0, 0x20, 1)));
        assert!(!ok(rgba(16, 16, 2, 1, 0x20, 0, 0, 0, 4)));
        assert!(!ok(rgba(16, 16, 1, 1, 0x20, 0, 0, 0, 3)));
        assert!(ok(rgba(16, 16, 1, 1, 0x20, 0, 0, 0, 8)));
        assert!(ok(rgba(16, 16, 1, 1, 0x20, 0, 0, 0, 16)));
        assert!(!ok(rgba(16, 16, 1, 1, 0, 3, CPU_READ, 0, 4)));
        assert!(!ok(rgba(16, 16, 1, 1, 0x200, 0, 0, 0, 1)));
        // (Wine's d3d11 test_create_depthstencil_view: a mip chain of a depth format)
        assert!(texture(&Tex { dim: 2, w: 512, h: 512, layers: 1, mips: 10, fmt: 45, samples: 1, bind: 0x40, ..Tex::default() }, false).is_ok());
        assert!(texture(&Tex { dim: 2, w: 512, h: 512, layers: 6, mips: 10, fmt: 45, samples: 1, bind: 0x40, misc: 4, ..Tex::default() }, false).is_ok());
        assert!(texture(&Tex { dim: 2, w: 512, h: 512, layers: 1, mips: 1, fmt: 45, samples: 4, bind: 0x40, ..Tex::default() }, false).is_ok());
        // (Wine's d3d11 test_standard_pattern)
        assert!(texture(&Tex { dim: 2, w: 32, h: 32, layers: 1, mips: 1, fmt: 28, samples: 4, quality: 0xffff_ffff, bind: 0x28, ..Tex::default() }, false).is_ok());
        assert!(texture(&Tex { dim: 2, w: 32, h: 32, layers: 1, mips: 1, fmt: 28, samples: 1, quality: 0xffff_ffff, bind: 0x28, ..Tex::default() }, false).is_err());
        assert!(!ok(rgba(16, 16, 1, 1, 0x1000, 0, 0, 0, 1)));
        assert!(texture(&Tex { dim: 2, w: 4, h: 4, layers: 1, mips: 1, fmt: 28, samples: 1, usage: 1, bind: 8, ..Tex::default() }, true).is_ok());
    }

    /// Windows' answers (winref d3d11tex): a shader resource view of a video format's texture
    /// in that format (NULL) is refused; in the formats of its texels it is made
    #[test]
    fn video_format_views() {
        for (fmt, ok) in [(100u32, [28u32, 30]), (101, [24, 25]), (102, [11, 12])] {
            let r = Res { dim: 2, fmt, mips: 1, layers: 1, bind: 8, cube: false, samples: 1, backbuffer: false };
            assert!(view(SRV, &r, None).is_err(), "{fmt} NULL");
            assert!(view(SRV, &r, Some(&[fmt, 4, 0, 1])).is_err(), "{fmt} as itself");
            for v in ok {
                assert!(view(SRV, &r, Some(&[v, 4, 0, 1])).is_ok(), "{fmt} as {v}");
            }
            assert!(view(SRV, &r, Some(&[87, 4, 0, 1])).is_err(), "{fmt} as B8G8R8A8");
        }
    }

    /// the probe's buffer cases
    #[test]
    fn cube_views() {
        // a cube texture of 6 or more layers; NULL views are one cube below 12 layers
        let cube = |mips, layers| Res { dim: 2, fmt: 28, mips, layers, bind: 8, cube: true, samples: 1, backbuffer: false };
        let srv = |r: &Res, d: Option<&[u32]>| view(SRV, r, d).ok().map(|v| v.desc[..6].to_vec());
        for (layers, want) in [(6, [28, 9, 0, 2, 0, 0]), (9, [28, 9, 0, 2, 0, 0]), (11, [28, 9, 0, 2, 0, 0]), (12, [28, 10, 0, 2, 0, 2]), (13, [28, 10, 0, 2, 0, 2]), (18, [28, 10, 0, 2, 0, 3])] {
            let got = srv(&cube(2, layers), None).map(|mut g| {
                if g[1] == 9 {
                    g[4..].fill(0);
                }
                g
            });
            assert_eq!(got.as_deref(), Some(&want[..]), "{layers} layers");
        }
        // cube arrays: all (~0) of 8 layers is one cube
        let m = u32::MAX;
        assert_eq!(srv(&cube(1, 8), Some(&[0, 10, 0, m, 0, m])).as_deref(), Some(&[28, 10, 0, 1, 0, 1][..]));
        assert_eq!(srv(&cube(1, 12), Some(&[0, 10, 0, m, 0, 1])).as_deref(), Some(&[28, 10, 0, 1, 0, 1][..]));
        assert!(srv(&cube(1, 6), Some(&[28, 10, 0, 1, 1, 1])).is_none());
        assert!(srv(&cube(1, 7), Some(&[28, 10, 0, 1, 0, 2])).is_none());
        assert!(texture(&Tex { dim: 2, w: 16, h: 16, layers: 9, mips: 2, fmt: 28, samples: 1, bind: 8, misc: 4, ..Tex::default() }, false).is_ok());
        assert!(texture(&Tex { dim: 2, w: 16, h: 16, layers: 5, mips: 2, fmt: 28, samples: 1, bind: 8, misc: 4, ..Tex::default() }, false).is_err());
    }

    #[test]
    fn wine_create_buffer() {
        // Wine's d3d11 test_create_buffer: 1024 bytes, DEFAULT, (bind, misc, stride, taken)
        const RAW: u32 = 0x20;
        const ST: u32 = 0x40;
        #[rustfmt::skip]
        let t: &[(u32, u32, u32, bool)] = &[
            (1, 0, 0, true), (2, 0, 0, true), (4, 0, 0, true), (8, 0, 0, true), (0x10, 0, 0, true), (0x20, 0, 0, true), (0x80, 0, 0, true),
            (0, 2, 0, true), (0, 0x10, 0, true),
            (1, RAW, 0, false), (2, RAW, 0, false), (4, RAW, 0, false), (8, RAW, 0, true), (0x10, RAW, 0, false), (0x20, RAW, 0, false), (0x80, RAW, 0, true), (0, RAW, 0, false),
            (0, ST, 16, true), (8, ST, 16, true), (8, ST, !0, false), (8, ST, 0, false), (8, ST, 1, false), (8, ST, 2, false), (8, ST, 3, false), (8, ST, 4, true),
            (8, ST, 5, false), (8, ST, 8, true), (8, ST, 512, true), (8, ST, 513, false), (8, ST, 1024, true),
            (0, 0, 513, true), (4, 0, 513, true), (8, 0, 513, true), (0x80, 0, 513, true),
            (0, RAW | ST, 16, false), (8, RAW | ST, 16, false), (0, 0x100, 0, true),
        ];
        for (i, &(bind, misc, stride, ok)) in t.iter().enumerate() {
            assert_eq!(buffer(1024, 0, bind, 0, misc, stride, false).is_ok(), ok, "test {i}");
        }
    }

    #[test]
    fn buffer_cases() {
        let b = |size, usage, bind, cpu, misc, stride| buffer(size, usage, bind, cpu, misc, stride, false);
        let cases: &[(&str, u32, u32, u32, u32, u32, u32, Option<u32>)] = &[
            ("size0", 0, 0, 1, 0, 0, 0, None),
            ("vb1", 1, 0, 1, 0, 0, 0, Some(0)),
            ("cb1", 1, 0, 4, 0, 0, 0, None),
            ("cb15", 15, 0, 4, 0, 0, 0, None),
            ("cb16", 16, 0, 4, 0, 0, 0, Some(0)),
            ("cb17", 17, 0, 4, 0, 0, 0, None),
            ("cb65552", 65552, 0, 4, 0, 0, 0, Some(0)),
            ("cb_vb", 16, 0, 5, 0, 0, 0, None),
            ("cb_srv", 16, 0, 0xc, 0, 0, 0, None),
            ("vb_ib_srv", 16, 0, 0xb, 0, 0, 0, Some(0)),
            ("vb_rt", 16, 0, 0x21, 0, 0, 0, Some(0)),
            ("vb_ds", 16, 0, 0x41, 0, 0, 0, None),
            ("so", 16, 0, 0x10, 0, 0, 0, Some(0)),
            ("uav", 16, 0, 0x80, 0, 0, 0, Some(0)),
            ("nobind", 16, 0, 0, 0, 0, 0, Some(0)),
            ("dynamic_vb", 16, 2, 1, CPU_WRITE, 0, 0, Some(0)),
            ("dynamic_nocpu", 16, 2, 1, 0, 0, 0, None),
            ("dynamic_read", 16, 2, 1, CPU_READ | CPU_WRITE, 0, 0, None),
            ("dynamic_uav", 16, 2, 0x80, CPU_WRITE, 0, 0, None),
            ("dynamic_so", 16, 2, 0x10, CPU_WRITE, 0, 0, None),
            ("immutable", 16, 1, 1, 0, 0, 0, None),
            ("staging", 16, 3, 0, CPU_READ, 0, 0, Some(0)),
            ("staging_bind", 16, 3, 1, CPU_READ, 0, 0, None),
            ("staging_nocpu", 16, 3, 0, 0, 0, 0, None),
            ("default_cpu", 16, 0, 1, CPU_WRITE, 0, 0, None),
            ("structured", 64, 0, 8, 0, 0x40, 16, Some(16)),
            ("structured_s0", 64, 0, 8, 0, 0x40, 0, None),
            ("structured_s3", 64, 0, 8, 0, 0x40, 3, None),
            ("structured_s6", 66, 0, 8, 0, 0x40, 6, None),
            ("structured_s2052", 2052, 0, 8, 0, 0x40, 2052, None),
            ("structured_s2056", 2056, 0, 8, 0, 0x40, 2056, None),
            ("structured_odd", 60, 0, 8, 0, 0x40, 16, None),
            ("structured_vb", 64, 0, 1, 0, 0x40, 16, None),
            ("structured_cb", 64, 0, 4, 0, 0x40, 16, None),
            ("raw", 64, 0, 8, 0, 0x20, 0, Some(0)),
            ("raw_vb", 64, 0, 1, 0, 0x20, 0, None),
            ("raw_structured", 64, 0, 8, 0, 0x60, 16, None),
            ("indirect", 64, 0, 0, 0, 0x10, 0, Some(0)),
            ("indirect_vb", 64, 0, 1, 0, 0x10, 0, Some(0)),
            ("cube", 64, 0, 1, 0, 4, 0, None),
            ("genmips", 64, 0, 0x28, 0, 1, 0, None),
            ("stride_unused", 64, 0, 1, 0, 0, 7, Some(0)),
        ];
        for &(name, size, usage, bind, cpu, misc, stride, want) in cases {
            assert_eq!(b(size, usage, bind, cpu, misc, stride).ok(), want, "{name}");
        }
    }

    /// every format's row of texture cases (the table and the rules agree)
    #[test]
    fn format_rows() {
        for f in 1..=132u32 {
            let (_, _, _, _, _, cases) = format(f).unwrap();
            // (the video formats the page keeps none of: a device without them makes no such texture)
            let cases = if UNKEPT.contains(&f) { 0 } else { cases };
            let t = |bind, usage, cpu, misc, mips, layers, samples| texture(&Tex { dim: 2, w: 16, h: 16, layers, mips, fmt: f, samples, quality: 0, usage, bind, cpu, misc }, false).is_ok();
            let got = [
                t(0, 0, 0, 0, 1, 1, 1),
                t(1, 0, 0, 0, 1, 1, 1),
                t(2, 0, 0, 0, 1, 1, 1),
                t(4, 0, 0, 0, 1, 1, 1),
                t(8, 0, 0, 0, 1, 1, 1),
                t(0x10, 0, 0, 0, 1, 1, 1),
                t(0x20, 0, 0, 0, 1, 1, 1),
                t(0x40, 0, 0, 0, 1, 1, 1),
                t(0x80, 0, 0, 0, 1, 1, 1),
                t(8, 0, 0, 0, 0, 1, 1),
                t(0, 3, CPU_READ, 0, 1, 1, 1),
                t(8, 2, CPU_WRITE, 0, 1, 1, 1),
                t(8, 0, 0, 4, 1, 6, 1),
                t(0x28, 0, 0, 1, 0, 1, 1),
                t(0x20, 0, 0, 0, 1, 1, 4),
                texture(&Tex { dim: 1, w: 16, h: 1, layers: 1, mips: 1, fmt: f, samples: 1, bind: 8, ..Tex::default() }, false).is_ok(),
                texture(&Tex { dim: 3, w: 8, h: 8, layers: 8, mips: 1, fmt: f, samples: 1, bind: 8, ..Tex::default() }, false).is_ok(),
            ];
            for (i, g) in got.iter().enumerate() {
                assert_eq!(*g, cases & (1 << i) != 0, "format {f} case {i}");
            }
        }
    }

    /// the probe's view cases: (name, texture format, mips, layers, bind, kind, view format,
    /// dimension, a, b, c, d (DSV: flags)), Windows' answer (GetDesc's format, dimension, fields)
    #[test]
    fn view_cases() {
        const M: u32 = u32::MAX;
        #[rustfmt::skip]
        let cases: &[(&str, u32, u32, u32, u32, u32, u32, u32, [u32; 4], Option<&[u32]>)] = &[
            ("srv_null", 28, 3, 1, 8, SRV, 0, 0, [0; 4], Some(&[28, 4, 0, 3])),
            ("srv_same", 28, 3, 1, 8, SRV, 28, 4, [0, 3, 0, 0], Some(&[28, 4, 0, 3])),
            ("srv_fmt0", 28, 3, 1, 8, SRV, 0, 4, [0, 3, 0, 0], Some(&[28, 4, 0, 3])),
            ("srv_srgb", 28, 3, 1, 8, SRV, 29, 4, [0, 3, 0, 0], None),
            ("srv_typeless_unorm", 27, 3, 1, 8, SRV, 28, 4, [0, 3, 0, 0], Some(&[28, 4, 0, 3])),
            ("srv_typeless_uint", 27, 3, 1, 8, SRV, 30, 4, [0, 3, 0, 0], Some(&[30, 4, 0, 3])),
            ("srv_typeless_null", 27, 3, 1, 8, SRV, 0, 0, [0; 4], None),
            ("srv_typeless_typeless", 27, 3, 1, 8, SRV, 27, 4, [0, 3, 0, 0], None),
            ("srv_other_family", 27, 3, 1, 8, SRV, 87, 4, [0, 3, 0, 0], None),
            ("srv_bgra_typeless", 90, 3, 1, 8, SRV, 91, 4, [0, 3, 0, 0], Some(&[91, 4, 0, 3])),
            ("srv_r32_float_of_d32", 39, 1, 1, 0x48, SRV, 41, 4, [0, 1, 0, 0], Some(&[41, 4, 0, 1])),
            ("srv_d32_of_d32", 39, 1, 1, 0x48, SRV, 40, 4, [0, 1, 0, 0], None),
            ("srv_r24x8", 44, 1, 1, 0x48, SRV, 46, 4, [0, 1, 0, 0], Some(&[46, 4, 0, 1])),
            ("srv_x24g8", 44, 1, 1, 0x48, SRV, 47, 4, [0, 1, 0, 0], Some(&[47, 4, 0, 1])),
            ("srv_mips_all", 28, 3, 1, 8, SRV, 28, 4, [1, M, 0, 0], Some(&[28, 4, 1, 2])),
            ("srv_mips_over", 28, 3, 1, 8, SRV, 28, 4, [1, 3, 0, 0], None),
            ("srv_mips0", 28, 3, 1, 8, SRV, 28, 4, [0, 0, 0, 0], None),
            ("srv_mip3", 28, 3, 1, 8, SRV, 28, 4, [3, 1, 0, 0], None),
            ("srv_array_of_single", 28, 1, 1, 8, SRV, 28, 5, [0, 1, 0, 1], Some(&[28, 5, 0, 1, 0, 1])),
            ("srv_2d_of_array", 28, 1, 4, 8, SRV, 28, 4, [0, 1, 0, 0], Some(&[28, 4, 0, 1])),
            ("srv_array_range", 28, 1, 4, 8, SRV, 28, 5, [0, 1, 1, 3], Some(&[28, 5, 0, 1, 1, 3])),
            ("srv_array_over", 28, 1, 4, 8, SRV, 28, 5, [0, 1, 2, 3], None),
            ("srv_array_all", 28, 1, 4, 8, SRV, 28, 5, [0, 1, 0, M], Some(&[28, 5, 0, 1, 0, 4])),
            ("srv_array_none", 28, 1, 4, 8, SRV, 28, 5, [0, 1, 0, 0], None),
            ("srv_cube_of_2d", 28, 1, 6, 8, SRV, 28, 9, [0, 1, 0, 0], None),
            ("srv_3d_of_2d", 28, 1, 1, 8, SRV, 28, 8, [0, 1, 0, 0], None),
            ("srv_1d_of_2d", 28, 1, 1, 8, SRV, 28, 2, [0, 1, 0, 0], None),
            ("srv_ms_of_2d", 28, 1, 1, 8, SRV, 28, 6, [0; 4], Some(&[28, 6])),
            ("srv_dim0", 28, 1, 1, 8, SRV, 28, 0, [0; 4], None),
            ("srv_nobind", 28, 1, 1, 0x20, SRV, 0, 0, [0; 4], None),
            ("rtv_null", 28, 3, 1, 0x20, RTV, 0, 0, [0; 4], Some(&[28, 4, 0])),
            ("rtv_mip2", 28, 3, 1, 0x20, RTV, 28, 4, [2, 0, 0, 0], Some(&[28, 4, 2])),
            ("rtv_mip3", 28, 3, 1, 0x20, RTV, 28, 4, [3, 0, 0, 0], None),
            ("rtv_fmt0", 28, 1, 1, 0x20, RTV, 0, 4, [0; 4], Some(&[28, 4, 0])),
            ("rtv_typeless_null", 27, 1, 1, 0x20, RTV, 0, 0, [0; 4], None),
            ("rtv_typeless_srgb", 27, 1, 1, 0x20, RTV, 29, 4, [0; 4], Some(&[29, 4, 0])),
            ("rtv_typeless_snorm", 27, 1, 1, 0x20, RTV, 31, 4, [0; 4], Some(&[31, 4, 0])),
            ("rtv_unorm_as_srgb", 28, 1, 1, 0x20, RTV, 29, 4, [0; 4], None),
            ("rtv_array_slice", 28, 1, 4, 0x20, RTV, 28, 5, [0, 3, 1, 0], Some(&[28, 5, 0, 3, 1])),
            ("rtv_array_over", 28, 1, 4, 0x20, RTV, 28, 5, [0, 3, 2, 0], None),
            ("rtv_array_all", 28, 1, 4, 0x20, RTV, 28, 5, [0, 0, M, 0], Some(&[28, 5, 0, 0, 4])),
            ("rtv_of_srv_only", 28, 1, 1, 8, RTV, 0, 0, [0; 4], None),
            ("rtv_d24", 44, 1, 1, 0x48, RTV, 45, 4, [0; 4], None),
            ("dsv_null", 45, 1, 1, 0x40, DSV, 0, 0, [0; 4], Some(&[45, 3, 0, 0])),
            ("dsv_typeless_null", 44, 1, 1, 0x40, DSV, 0, 0, [0; 4], None),
            ("dsv_typeless_d24", 44, 1, 1, 0x40, DSV, 45, 3, [0; 4], Some(&[45, 3, 0, 0])),
            ("dsv_r32_typeless_d32", 39, 1, 1, 0x40, DSV, 40, 3, [0; 4], Some(&[40, 3, 0, 0])),
            ("dsv_r32_float", 39, 1, 1, 0x40, DSV, 41, 3, [0; 4], None),
            ("dsv_fmt0", 40, 1, 1, 0x40, DSV, 0, 3, [0; 4], Some(&[40, 3, 0, 0])),
            ("dsv_flags_ro", 45, 1, 1, 0x40, DSV, 45, 3, [0, 0, 0, 3], Some(&[45, 3, 3, 0])),
            ("dsv_flags_bad", 45, 1, 1, 0x40, DSV, 45, 3, [0, 0, 0, 4], None),
            ("dsv_d32_stencil_ro", 40, 1, 1, 0x40, DSV, 40, 3, [0, 0, 0, 2], None),
            ("dsv_of_rt", 28, 1, 1, 0x20, DSV, 0, 0, [0; 4], None),
            ("uav_null", 41, 1, 1, 0x80, UAV, 0, 0, [0; 4], Some(&[41, 4, 0])),
            ("uav_typeless_r32_uint", 39, 1, 1, 0x80, UAV, 42, 4, [0; 4], Some(&[42, 4, 0])),
            ("uav_rgba8_typeless_uint", 27, 1, 1, 0x80, UAV, 30, 4, [0; 4], Some(&[30, 4, 0])),
            ("uav_rgba8_typeless_r32", 27, 1, 1, 0x80, UAV, 42, 4, [0; 4], Some(&[42, 4, 0])),
            ("uav_of_srv", 41, 1, 1, 8, UAV, 0, 0, [0; 4], None),
        ];
        for &(name, tf, mips, layers, bind, kind, vf, dim, [a, b, c, dd], want) in cases {
            let r = Res { dim: 2, fmt: tf, mips, layers, bind, cube: false, samples: 1, backbuffer: false };
            // (the probe's union: SRV (a, b, c, d), the others (a, b, c); a DSV's flags in d)
            let words: Vec<u32> = match kind {
                SRV => vec![vf, dim, a, b, c, dd],
                DSV => vec![vf, dim, dd, a, b, c],
                _ => vec![vf, dim, a, b, c],
            };
            let desc = if vf == 0 && dim == 0 { None } else { Some(&words[..]) };
            let got = view(kind, &r, desc).ok().map(|v| v.desc);
            // (Windows' GetDesc fields past those the dimension has are left out of `want`)
            let got = got.map(|g| {
                let n = want.map_or(0, |w| w.len());
                g[..n.min(g.len())].to_vec()
            });
            assert_eq!(got.as_deref(), want, "{name}");
        }
    }
}

/// The typeless format a format belongs to (itself when it has no family).
pub fn family(fmt: u32) -> u32 {
    match fmt {
        1..=4 => 1,
        5..=8 => 5,
        9..=14 => 9,
        15..=18 => 15,
        19..=22 => 19,
        23..=25 => 23,
        27..=32 => 27,
        33..=38 => 33,
        39..=43 => 39,
        44..=47 => 44,
        48..=52 => 48,
        53..=59 => 53,
        60..=64 => 60,
        70..=72 => 70,
        73..=75 => 73,
        76..=78 => 76,
        79..=81 => 79,
        82..=84 => 82,
        90 | 87 | 91 => 90,
        92 | 88 | 93 => 92,
        94..=96 => 94,
        97..=99 => 97,
        _ => fmt,
    }
}

pub fn typeless(fmt: u32) -> bool {
    matches!(fmt, 1 | 5 | 9 | 15 | 19 | 23 | 27 | 33 | 39 | 44 | 48 | 53 | 60 | 70 | 73 | 76 | 79 | 82 | 90 | 92 | 94 | 97)
}

/// bytes per texel of the uncompressed formats (0 for the others)
fn texel_bytes(fmt: u32) -> u32 {
    match family(fmt) {
        1 => 16,
        5 => 12,
        9 | 15 | 19 => 8,
        23 | 26 | 27 | 33 | 39 | 44 | 67 | 90 | 92 => 4,
        48 | 53 | 85 | 86 | 115 => 2,
        60 | 65 => 1,
        _ => 0,
    }
}

/// The texture a view is made of.
#[derive(Clone, Copy, Debug, Default)]
pub struct Res {
    pub dim: u32,
    pub fmt: u32,
    pub mips: u32,
    /// array size, depth of a 3D texture
    pub layers: u32,
    pub bind: u32,
    pub cube: bool,
    pub samples: u32,
    /// a swap chain's buffer: viewed in its format's sRGB twin too (BACK_BUFFER_CAST)
    pub backbuffer: bool,
}

/// A view as Windows keeps it: the description GetDesc gives back (format, dimension, the DSV's
/// flags, then the union, unused fields 0) and what it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    pub desc: Vec<u32>,
    pub mip: u32,
    pub mips: u32,
    pub layer: u32,
    pub layers: u32,
}

/// The formats a view of a texture of video (or palette) format `fmt` takes (None: not one).
pub fn video_views(fmt: u32) -> Option<&'static [u32]> {
    Some(match fmt {
        100 => &[28, 30],
        101 => &[24, 25],
        102 => &[11, 12],
        // (NV12: the luma plane as R8, the chroma one as R8G8, UNORM: the page's textures of
        // them; Windows also takes R8_UINT and R8G8_UINT)
        103 => &[61, 49],
        104..=114 => &[],
        _ => return None,
    })
}

pub const SRV: u32 = 0;
pub const RTV: u32 = 1;
pub const DSV: u32 = 2;
pub const UAV: u32 = 3;

/// Whether Create{ShaderResource, RenderTarget, DepthStencil, UnorderedAccess}View (`kind`)
/// takes `desc` (its words; None: NULL) for a view of texture `r`, and the view if it does.
pub fn view(kind: u32, r: &Res, desc: Option<&[u32]>) -> Result<View, u32> {
    let bad = Err(E_INVALIDARG);
    let need = [BIND_SRV, BIND_RT, BIND_DS, BIND_UAV][kind as usize & 3];
    if r.bind & need == 0 {
        return bad;
    }
    let ms = r.samples > 1;
    let arr = r.layers > 1;
    // NULL: the whole texture, in its own format
    let (fmt, dim, flags, u): (u32, u32, u32, [u32; 4]) = match desc {
        None => {
            let (m, n) = (r.mips, r.layers);
            let (dim, u) = match (kind, r.dim) {
                (SRV, 1) => (if arr { 3 } else { 2 }, [0, m, 0, n]),
                (SRV, 3) => (8, [0, m, 0, 0]),
                (SRV, _) if ms => (if arr { 7 } else { 6 }, [0, n, 0, 0]),
                (SRV, _) if r.cube => (if n / 6 == 1 { 9 } else { 10 }, [0, m, 0, n / 6]),
                (SRV, _) => (if arr { 5 } else { 4 }, [0, m, 0, n]),
                (DSV, 3) => return bad,
                (DSV, 1) => (if arr { 2 } else { 1 }, [0, 0, n, 0]),
                (DSV, _) if ms => (if arr { 6 } else { 5 }, [0, n, 0, 0]),
                (DSV, _) => (if arr { 4 } else { 3 }, [0, 0, n, 0]),
                (UAV, _) if ms => return bad,
                (_, 1) => (if arr { 3 } else { 2 }, [0, 0, n, 0]),
                (_, 3) => (8, [0, 0, n, 0]),
                (_, _) if ms => (if arr { 7 } else { 6 }, [0, n, 0, 0]),
                (_, _) => (if arr { 5 } else { 4 }, [0, 0, n, 0]),
            };
            (r.fmt, dim, 0, u)
        }
        Some(w) => {
            let at = |i: usize| w.get(i).copied().unwrap_or(0);
            let base = if kind == DSV { 3 } else { 2 };
            (at(0), at(1), if kind == DSV { at(2) } else { 0 }, [at(base), at(base + 1), at(base + 2), at(base + 3)])
        }
    };
    // the format: the texture's own (a typeless one needs a typed format of its family)
    let fmt = if fmt == 0 { r.fmt } else { fmt };
    if typeless(fmt) {
        return bad;
    }
    let same_family = family(fmt) == family(r.fmt) && typeless(r.fmt);
    let r32_of_32 = kind == UAV && typeless(r.fmt) && texel_bytes(r.fmt) == 4 && matches!(fmt, 41..=43);
    let srgb_twin = |a: u32, b: u32| matches!((a.min(b), a.max(b)), (28, 29) | (87, 91) | (88, 93));
    let cast = r.backbuffer && srgb_twin(fmt, r.fmt);
    // (views of video formats: of the formats of their texels, AYUV as R8G8B8A8, Y410 as
    // R10G10B10A2, Y416 as R16G16B16A16, UNORM or UINT; none in the format itself)
    // (NV12: views of a plane, no depth)
    if r.fmt == NV12 && kind == DSV {
        return bad;
    }
    if let Some(ok) = video_views(r.fmt) {
        if !ok.contains(&fmt) {
            return bad;
        }
    } else if fmt != r.fmt && !same_family && !r32_of_32 && !cast {
        return bad;
    }
    let cap = [0x100, 0x4000, 0x10000, 0x2000000][kind as usize & 3];
    if support(fmt).is_none_or(|s| s & cap == 0) {
        return bad;
    }
    if kind == DSV && (flags & !3 != 0 || flags & 2 != 0 && !matches!(fmt, 20 | 45)) {
        return bad;
    }
    let dims: &[u32] = match (kind, r.dim) {
        (SRV, 1) => &[2, 3],
        (SRV, 2) => &[4, 5, 6, 7, 9, 10],
        (SRV, 3) => &[8],
        (RTV, 1) | (UAV, 1) => &[2, 3],
        (RTV, 2) => &[4, 5, 6, 7],
        (UAV, 2) => &[4, 5],
        (RTV, 3) | (UAV, 3) => &[8],
        (DSV, 1) => &[1, 2],
        (DSV, 2) => &[3, 4, 5, 6],
        _ => &[],
    };
    if !dims.contains(&dim) || matches!((kind, dim), (SRV, 9 | 10)) && !r.cube {
        return bad;
    }
    // what the union says, checked against the texture
    let all = u32::MAX;
    let levels = |first: u32, n: u32| -> Option<u32> {
        let n = if n == all { r.mips.checked_sub(first)? } else { n };
        (first < r.mips && n > 0 && first.checked_add(n)? <= r.mips).then_some(n)
    };
    let slices = |first: u32, n: u32, total: u32| -> Option<u32> {
        let n = if n == all { total.checked_sub(first)? } else { n };
        (first < total && n > 0 && first.checked_add(n)? <= total).then_some(n)
    };
    let mip_ok = |m: u32| m < r.mips;
    let v = |desc: Vec<u32>, mip, mips, layer, layers| Ok(View { desc, mip, mips, layer, layers });
    let head = |rest: &[u32], n: usize| -> Vec<u32> {
        let mut d = Vec::from([fmt, dim]);
        if kind == DSV {
            d.push(flags);
        }
        d.extend_from_slice(rest);
        d.resize(d.len() + n - rest.len(), 0);
        d
    };
    match (kind, dim) {
        (SRV, 2 | 4 | 8 | 9) => {
            let Some(n) = levels(u[0], u[1]) else { return bad };
            let layers = if dim == 9 { 6 } else { 1 };
            v(head(&[u[0], n], 4), u[0], n, 0, layers)
        }
        (SRV, 3 | 5) => {
            let (Some(n), Some(k)) = (levels(u[0], u[1]), slices(u[2], u[3], r.layers)) else { return bad };
            v(head(&[u[0], n, u[2], k], 4), u[0], n, u[2], k)
        }
        (SRV, 10) => {
            let cubes = if u[3] == all { r.layers.saturating_sub(u[2]) / 6 } else { u[3] };
            let Some(n) = levels(u[0], u[1]) else { return bad };
            if cubes == 0 || cubes.checked_mul(6).and_then(|c| c.checked_add(u[2])).is_none_or(|end| end > r.layers) {
                return bad;
            }
            v(head(&[u[0], n, u[2], cubes], 4), u[0], n, u[2], cubes * 6)
        }
        (SRV, 6) | (RTV, 6) | (DSV, 5) => v(head(&[], if kind == SRV { 4 } else { 3 }), 0, 1, 0, 1),
        (SRV, 7) | (RTV, 7) | (DSV, 6) => {
            let Some(k) = slices(u[0], u[1], r.layers) else { return bad };
            v(head(&[u[0], k], if kind == SRV { 4 } else { 3 }), 0, 1, u[0], k)
        }
        (RTV | UAV, 2 | 4) | (DSV, 1 | 3) => {
            if !mip_ok(u[0]) {
                return bad;
            }
            v(head(&[u[0]], 3), u[0], 1, 0, 1)
        }
        (RTV | UAV, 3 | 5) | (DSV, 2 | 4) => {
            let Some(k) = slices(u[1], u[2], r.layers) else { return bad };
            if !mip_ok(u[0]) {
                return bad;
            }
            v(head(&[u[0], u[1], k], 3), u[0], 1, u[1], k)
        }
        (RTV | UAV, 8) => {
            if !mip_ok(u[0]) {
                return bad;
            }
            let depth = (r.layers >> u[0]).max(1);
            let Some(k) = slices(u[1], u[2], depth) else { return bad };
            v(head(&[u[0], u[1], k], 3), u[0], 1, u[1], k)
        }
        _ => bad,
    }
}

#[cfg(test)]
mod coverage {
    /// (not a check: lists the formats Windows takes that the page has no texels for)
    #[test]
    #[ignore]
    fn page_formats() {
        let missing: Vec<u32> = (1..=132).filter(|&f| super::support(f).is_some() && !super::typeless(f) && super::super::format::info(f).is_none()).collect();
        println!("missing {missing:?}");
    }
}

/// A buffer view: its format (R32_UINT's 4 bytes for raw and structured views), the first
/// byte and the bytes it covers, and the description GetDesc gives back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufView {
    pub fmt: u32,
    pub offset: u32,
    pub bytes: u32,
    pub desc: Vec<u32>,
}

/// Whether a view (`kind`) of a buffer (`size` bytes, its bind and misc flags and structure
/// stride) with description `w` is taken. SRV: BUFFER (first, n) or BUFFEREX (first, n,
/// flags); UAV: BUFFER (first, n, flags); RTV: BUFFER (first, n).
pub fn buffer_view(kind: u32, size: u32, bind: u32, misc: u32, stride: u32, w: Option<&[u32]>) -> Result<BufView, u32> {
    let bad = Err(E_INVALIDARG);
    let need = [BIND_SRV, BIND_RT, BIND_DS, BIND_UAV][kind as usize & 3];
    if bind & need == 0 || kind == DSV {
        return bad;
    }
    let structured = misc & MISC_STRUCTURED != 0;
    // NULL: only for a structured buffer, all of it
    let Some(w) = w else {
        if !structured || kind == RTV {
            return bad;
        }
        let n = size / stride;
        let desc = if kind == SRV { vec![0, 1, 0, n, 0, 0] } else { vec![0, 1, 0, n, 0] };
        return Ok(BufView { fmt: 0, offset: 0, bytes: n * stride, desc });
    };
    let at = |i: usize| w.get(i).copied().unwrap_or(0);
    let (fmt, dim, first, n) = (at(0), at(1), at(2), at(3));
    let flags = match (kind, dim) {
        (SRV, 1) | (RTV, 1) => 0,
        (SRV, 11) => at(4),
        (UAV, 1) => at(4),
        _ => return bad,
    };
    // SRV BUFFEREX flags: RAW; UAV: RAW, APPEND, COUNTER
    let raw = flags & 1 != 0;
    if flags & !if kind == UAV { 7 } else { 1 } != 0 || n == 0 {
        return bad;
    }
    let elem = if raw {
        if fmt != 39 || misc & MISC_RAW == 0 || flags & 6 != 0 {
            return bad;
        }
        4
    } else if structured {
        if fmt != 0 {
            return bad;
        }
        stride
    } else {
        if flags & 6 != 0 {
            return bad;
        }
        let cap = [0x1, 0x4000, 0, 0x2000000][kind as usize & 3];
        let ok = if kind == SRV { format(fmt).is_some_and(|f| f.5 & winfmt::BSRV != 0) } else { support(fmt).is_some_and(|s| s & cap != 0) };
        let bytes = texel_bytes(fmt);
        if !ok || bytes == 0 {
            return bad;
        }
        bytes
    };
    if first.checked_add(n).is_none_or(|end| end as u64 * elem as u64 > size as u64) {
        return bad;
    }
    let mut desc = vec![fmt, dim, first, n];
    if kind != RTV {
        desc.push(flags);
    }
    if kind == SRV {
        desc.resize(6, 0);
    }
    Ok(BufView { fmt, offset: first * elem, bytes: n * elem, desc })
}

#[cfg(test)]
mod buffer_view_tests {
    use super::*;

    #[test]
    fn raw_structured_typed() {
        // (Wine's d3d11 test_unaligned_raw_buffer_access, test_buffer_srv)
        assert!(buffer_view(SRV, 64, BIND_SRV, MISC_RAW, 0, Some(&[39, 11, 0, 16, 1])).is_ok());
        assert!(buffer_view(SRV, 64, BIND_SRV, 0, 0, Some(&[39, 11, 0, 16, 1])).is_err());
        assert!(buffer_view(SRV, 64, BIND_SRV, 0, 0, Some(&[28, 1, 0, 16])).is_ok());
        assert!(buffer_view(SRV, 64, BIND_SRV, 0, 0, Some(&[28, 1, 1, 16])).is_err());
        assert!(buffer_view(SRV, 64, BIND_SRV, MISC_STRUCTURED, 16, Some(&[0, 1, 0, 4])).is_ok());
        assert!(buffer_view(SRV, 64, BIND_SRV, MISC_STRUCTURED, 16, Some(&[28, 1, 0, 4])).is_err());
        assert_eq!(buffer_view(UAV, 64, BIND_UAV, MISC_STRUCTURED, 16, None).map(|v| v.desc), Ok(vec![0, 1, 0, 4, 0]));
        assert!(buffer_view(UAV, 64, BIND_UAV, MISC_RAW, 0, Some(&[39, 1, 0, 16, 1])).is_ok());
        assert!(buffer_view(UAV, 64, BIND_SRV, MISC_RAW, 0, Some(&[39, 1, 0, 16, 1])).is_err());
    }

    /// the probe's typed buffer view of every format (4 elements of a 256-byte buffer)
    #[test]
    fn typed_rows() {
        for f in 1..=132u32 {
            let want = format(f).unwrap().5 & winfmt::BSRV != 0;
            assert_eq!(buffer_view(SRV, 256, BIND_SRV, 0, 0, Some(&[f, 1, 0, 4])).is_ok(), want, "format {f}");
        }
    }
}

/// Create*Shader's checks (`method` the device method: 12 VS, 13 GS, 15 PS, 16 HS, 17 DS, 18 CS):
/// DXBC whose program is that stage, at a shader model the feature level runs. Below 10_0 only
/// a 4_0_level_9_x vertex or pixel shader (one with an Aon9 chunk) is taken.
pub fn shader(code: &[u8], method: u32, level: u32) -> Result<(), u32> {
    let bad = Err(E_INVALIDARG);
    // (sake's D3DCompile stands ANGLE's point sprite geometry shader in with a tag: draws
    // expand the points themselves)
    if method == 13 && dxwgsl::point_sprite_gs(code).is_some() {
        return Ok(());
    }
    let Ok(c) = dxwgsl::dxbc::Container::parse(code) else { return bad };
    let Some(prog) = c.chunk(b"SHEX").or_else(|| c.chunk(b"SHDR")) else { return bad };
    let Some(v) = prog.get(..4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) else { return bad };
    let (kind, major, minor) = (v >> 16, v >> 4 & 0xf, v & 0xf);
    let want = match method {
        12 => 1,
        13 => 2,
        15 => 0,
        16 => 3,
        17 => 4,
        _ => 5,
    };
    if kind != want {
        return bad;
    }
    let ok = if level < 0xa000 {
        c.chunk(b"Aon9").is_some() && kind <= 1 && (major, minor) == (4, 0)
    } else if level < 0xa100 {
        (major, minor) == (4, 0) && kind <= 2 || kind == 5 && (major, minor) == (4, 0)
    } else if level < 0xb000 {
        major == 4 && minor <= 1 && (kind <= 2 || kind == 5)
    } else {
        major == 4 && minor <= 1 && (kind <= 2 || kind == 5) || (major, minor) == (5, 0)
    };
    if ok {
        Ok(())
    } else {
        bad
    }
}

#[cfg(test)]
mod shader_tests {
    use super::*;

    /// A DXBC container with a program chunk of version `v` (and an Aon9 chunk if `aon9`).
    fn dxbc(tag: &[u8; 4], v: u32, aon9: bool) -> Vec<u8> {
        let mut chunks: Vec<([u8; 4], Vec<u8>)> = vec![(*tag, [v.to_le_bytes(), 2u32.to_le_bytes()].concat())];
        if aon9 {
            chunks.push((*b"Aon9", vec![0; 8]));
        }
        let mut out = b"DXBC".to_vec();
        out.extend([0u8; 16]);
        out.extend(1u32.to_le_bytes());
        let head = 32 + 4 * chunks.len();
        let mut body = Vec::new();
        let mut offs = Vec::new();
        for (t, d) in &chunks {
            offs.push((head + body.len()) as u32);
            body.extend(t);
            body.extend((d.len() as u32).to_le_bytes());
            body.extend(d);
        }
        out.extend(((head + body.len()) as u32).to_le_bytes());
        out.extend((chunks.len() as u32).to_le_bytes());
        for o in offs {
            out.extend(o.to_le_bytes());
        }
        out.extend(body);
        out
    }

    fn ver(kind: u32, major: u32, minor: u32) -> u32 {
        kind << 16 | major << 4 | minor
    }

    /// The cases of Wine's test_create_shader (d3d11.c), for every feature level it runs.
    #[test]
    fn create_shader_cases() {
        let bad = Err(E_INVALIDARG);
        let levels = [0x9100, 0x9200, 0x9300, 0xa000, 0xa100, 0xb000];
        // SM1-3 bytecode (a version token, no container) is never taken
        let vs_2_0 = 0xfffe0200u32.to_le_bytes().to_vec();
        for &l in &levels {
            assert_eq!(shader(&vs_2_0, 12, l), bad);
            assert_eq!(shader(&vs_2_0, 15, l), bad);
            // ps_4_0_level_9_x everywhere
            assert_eq!(shader(&dxbc(b"SHDR", ver(0, 4, 0), true), 15, l), Ok(()));
            // ps_4_0 as a vertex shader, gs_4_0 as hull, domain and compute
            assert_eq!(shader(&dxbc(b"SHDR", ver(0, 4, 0), false), 12, l), bad);
            for m in [16, 17, 18] {
                assert_eq!(shader(&dxbc(b"SHDR", ver(2, 4, 0), false), m, l), bad);
            }
            let fl10 = l >= 0xa000;
            let fl101 = l >= 0xa100;
            let fl11 = l >= 0xb000;
            for (m, kind) in [(12, 1), (15, 0), (13, 2)] {
                assert_eq!(shader(&dxbc(b"SHDR", ver(kind, 4, 0), false), m, l).is_ok(), fl10, "{m} 4_0 at {l:#x}");
                assert_eq!(shader(&dxbc(b"SHDR", ver(kind, 4, 1), false), m, l).is_ok(), fl101, "{m} 4_1 at {l:#x}");
                assert_eq!(shader(&dxbc(b"SHEX", ver(kind, 5, 0), false), m, l).is_ok(), fl11, "{m} 5_0 at {l:#x}");
            }
            for (m, kind) in [(16, 3), (17, 4), (18, 5)] {
                assert_eq!(shader(&dxbc(b"SHEX", ver(kind, 5, 0), false), m, l).is_ok(), fl11);
            }
            assert_eq!(shader(&dxbc(b"SHDR", ver(5, 4, 0), false), 18, l).is_ok(), fl10);
        }
        // nothing, or a container without a program
        assert_eq!(shader(&[], 12, 0xb000), bad);
        assert_eq!(shader(&dxbc(b"RDEF", ver(1, 4, 0), false), 12, 0xb000), bad);
    }
}

/// CreateInputLayout's checks of each element (winref d3d11copy's layouts on Windows): an input
/// slot below 32, a format (not UNKNOWN, not block compressed), per vertex (0, step rate 0) or
/// per instance (1), and no semantic (name without case, index) twice. Elements are (name,
/// index, format, slot, class, step rate).
pub fn input_elements(elems: &[(&str, u32, u32, u32, u32, u32)]) -> Result<(), u32> {
    for (k, &(name, index, fmt, slot, class, step)) in elems.iter().enumerate() {
        let bc = matches!(fmt, 70..=84 | 94..=99);
        if slot >= 32 || fmt == 0 || fmt > 132 || bc || class > 1 || (class == 0 && step != 0) {
            return Err(E_INVALIDARG);
        }
        if elems[..k].iter().any(|e| e.1 == index && e.0.eq_ignore_ascii_case(name)) {
            return Err(E_INVALIDARG);
        }
    }
    Ok(())
}

/// CreateInputLayout's checks (Windows' answers in Wine's test_create_input_layout): a
/// description array is needed even for no elements, at most 32 elements, and every input of
/// the shader's signature (name without case, index) has an element but the system values
/// the input assembler makes (SV_VertexID 6, SV_InstanceID 8). SV_Position and the others
/// are ordinary inputs to a vertex shader, and are needed. Elements the shader does not read
/// are fine. `slots`: each element's (input slot, per instance).
pub fn input_layout(have_descs: bool, elems: &[(&str, u32)], slots: &[(u32, bool)], code: &[u8]) -> Result<(), u32> {
    if !have_descs || elems.len() > 32 {
        return Err(E_INVALIDARG);
    }
    // the elements of one input slot are all per vertex or all per instance
    if slots.iter().any(|&(s, inst)| slots.iter().any(|&(t, i2)| s == t && inst != i2)) {
        return Err(E_INVALIDARG);
    }
    for s in dxwgsl::signature(code, false) {
        // (by the system value or by the name: a pixel shader's SV_InstanceID input carries
        // no system value of its own)
        if matches!(s.sysval, 6 | 8) || s.name.eq_ignore_ascii_case("SV_VertexID") || s.name.eq_ignore_ascii_case("SV_InstanceID") {
            continue;
        }
        if !elems.iter().any(|&(n, i)| i == s.index && n.eq_ignore_ascii_case(&s.name)) {
            return Err(E_INVALIDARG);
        }
    }
    Ok(())
}

#[cfg(test)]
mod input_layout_tests {
    use super::*;

    /// A DXBC container with only an ISGN of (name, index, sysval).
    fn isgn(inputs: &[(&str, u32, u32)]) -> Vec<u8> {
        let mut c = Vec::new();
        c.extend((inputs.len() as u32).to_le_bytes());
        c.extend(8u32.to_le_bytes());
        let mut names = Vec::new();
        let base = 8 + 24 * inputs.len();
        for (r, &(n, i, sv)) in inputs.iter().enumerate() {
            for x in [(base + names.len()) as u32, i, sv, 3, r as u32, 0x0f0f] {
                c.extend(x.to_le_bytes());
            }
            names.extend(n.bytes().chain([0]));
        }
        c.extend(names);
        let mut b = b"DXBC".to_vec();
        b.extend([0; 20]);
        b.extend([0; 4]); // total size (unchecked)
        b.extend(1u32.to_le_bytes());
        b.extend(36u32.to_le_bytes());
        b.extend(b"ISGN");
        b.extend((c.len() as u32).to_le_bytes());
        b.extend(c);
        b
    }

    #[test]
    fn elements_as_windows() {
        // (winref d3d11copy-32: the layouts of shader 0)
        let pos = |fmt, slot, class, step| ("POSITION", 0, fmt, slot, class, step);
        assert_eq!(input_elements(&[pos(2, 0, 0, 0)]), Ok(()));
        assert_eq!(input_elements(&[pos(2, 15, 0, 0)]), Ok(()));
        assert_eq!(input_elements(&[pos(2, 31, 1, 7)]), Ok(()));
        assert_eq!(input_elements(&[pos(2, 0, 0, 1)]), Err(E_INVALIDARG)); // pos_vert_step1
        assert_eq!(input_elements(&[pos(2, 32, 0, 0)]), Err(E_INVALIDARG)); // pos_slot32
        assert_eq!(input_elements(&[pos(0, 0, 0, 0)]), Err(E_INVALIDARG)); // pos_unknown_format
        assert_eq!(input_elements(&[pos(71, 0, 0, 0)]), Err(E_INVALIDARG)); // pos_bc1
        assert_eq!(input_elements(&[pos(2, 0, 0, 0), ("position", 0, 2, 0, 0, 0)]), Err(E_INVALIDARG)); // pos_twice
        assert_eq!(input_elements(&[pos(2, 0, 0, 0), ("POSITION", 1, 2, 0, 0, 0)]), Ok(())); // pos_pos1
        assert_eq!(input_elements(&[pos(2, 0, 2, 0)]), Err(E_INVALIDARG));
        assert_eq!(input_elements(&[]), Ok(()));
    }

    #[test]
    fn typeless_multisampled_depth() {
        let t = |fmt, bind, samples| texture(&Tex { dim: 2, w: 540, h: 960, layers: 1, mips: 1, fmt, samples, bind, ..Tex::default() }, false);
        // (キメセク!地雷ちゃん 体験版, Unity 6: 540x960 R32G8X24_TYPELESS at 2x, DS | SRV)
        assert!(t(19, 0x48, 2).is_ok());
        assert!(t(44, 0x48, 4).is_ok()); // R24G8_TYPELESS
        assert!(t(39, 0x48, 8).is_ok()); // R32_TYPELESS
        // a shader resource alone or a render target bind stays as the format says
        assert!(t(19, 0x8, 2).is_err());
        assert!(t(19, 0x20, 2).is_err());
        assert!(t(19, 0x48, 3).is_err());
    }

    proptest::proptest! {
        /// A multisampled texture is taken only at a sample count with quality levels, and a
        /// depth bind on a typeless format with levels is always taken (Windows casts it).
        #[test]
        fn multisample_needs_levels(fmt in 1u32..133, samples in 2u32..17, bind in proptest::sample::select(vec![0x8u32, 0x20, 0x28, 0x40, 0x48, 0x80])) {
            let r = texture(&Tex { dim: 2, w: 64, h: 64, layers: 1, mips: 1, fmt, samples, bind, ..Tex::default() }, false);
            if quality_levels(fmt, samples) == 0 {
                proptest::prop_assert!(r.is_err());
            }
            let f = format(fmt).unwrap();
            if bind == 0x48 && quality_levels(fmt, samples) > 0 && f.1 & 0x10_0000 != 0 && f.5 & (winfmt::DS | winfmt::SRV | winfmt::NONE) == winfmt::DS | winfmt::SRV | winfmt::NONE {
                proptest::prop_assert!(r.is_ok());
            }
        }
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(2048))]
        /// Any view description, words near u32::MAX too: a view taken covers only levels and
        /// slices the texture has (first + count wrapped: 2 + 0xfffffffe levels passed).
        #[test]
        fn views_stay_inside_the_texture(
            kind in 0u32..4, dim in proptest::sample::select(vec![3u32, 4, 5, 7, 9, 10]), mips in 1u32..8, layers in 1u32..16, cube in proptest::bool::ANY,
            u in proptest::collection::vec(proptest::prop_oneof![0u32..8, u32::MAX - 8..=u32::MAX, proptest::prelude::any::<u32>()], 4),
        ) {
            let r = Res { dim: 2, fmt: 28, mips, layers: if cube { layers.max(1) * 6 } else { layers }, bind: 8 | 0x20 | 0x80, cube, samples: 1, backbuffer: false };
            let desc = [28, dim, u[0], u[1], u[2], u[3]];
            let (d, rest) = if kind == 2 { (vec![desc[0], desc[1], 0, desc[2], desc[3], desc[4], desc[5]], ()) } else { (desc.to_vec(), ()) };
            let _ = rest;
            if let Ok(v) = view(kind, &r, Some(&d)) {
                proptest::prop_assert!(v.mips >= 1 && v.mip as u64 + v.mips as u64 <= r.mips as u64, "{:?} of {:?}", v, r);
                proptest::prop_assert!(v.layer as u64 + v.layers as u64 <= r.layers.max(6) as u64, "{:?} of {:?}", v, r);
            }
        }

        /// What a texture takes in bytes: no overflow for any size (saturating), the sum of its
        /// levels' pitch x rows where those fit u32 (format::pitch_rows), and refused past
        /// MAX_TEXTURE_BYTES before anything multiplies its sizes in u32.
        #[test]
        fn texture_bytes_any_size(fmt in 1u32..133, w in proptest::prelude::any::<u32>(), h in proptest::prelude::any::<u32>(), d in 1u32..4, layers in 1u32..2049, mips in 1u32..33, samples in 1u32..9, small in 1u32..300) {
            use super::super::format::{pitch_rows, texture_bytes, MAX_TEXTURE_BYTES};
            let _ = texture_bytes(fmt, w, h, d, layers, mips, samples);
            let (sw, sh) = (small, small / 2 + 1);
            let want: u64 = (0..3u32).map(|l| { let (p, r) = pitch_rows(fmt, (sw >> l).max(1), (sh >> l).max(1)); p as u64 * r as u64 }).sum();
            proptest::prop_assert_eq!(texture_bytes(fmt, sw, sh, 1, 1, 3, 1), want);
            proptest::prop_assert!(texture_bytes(fmt, 1 << 16, 1 << 16, 1, 1, 1, 1) > MAX_TEXTURE_BYTES || super::super::format::info(fmt).is_some_and(|f| f.block > 1));
        }

        /// What CreateTexture* takes (both sizes inside MAX_TEXTURE_BYTES) has every level's
        /// decoded texels (w x h x slices x the page's bytes a texel: what the converters
        /// allocate in usize, 32 bits on wasm) inside u32, whatever the format; the page's size
        /// is at least the guest's for the decoded (block) formats' whole blocks.
        #[test]
        fn accepted_textures_decode_in_32_bits(fmt in 1u32..133, lw in 0u32..17, lh in 0u32..17, d in 1u32..9, layers in 1u32..2049, mips in 1u32..16) {
            use super::super::format::{info, page_bpp, page_texture_bytes, texture_bytes, MAX_TEXTURE_BYTES};
            let (w, h) = (1u32 << lw, 1u32 << lh);
            let ok = texture_bytes(fmt, w, h, d, layers, mips, 1).max(page_texture_bytes(fmt, w, h, d, layers, mips, 1)) <= MAX_TEXTURE_BYTES;
            if let (true, Some(f)) = (ok, info(fmt)) {
                let texel = page_bpp(f.page).max(1) as u64;
                proptest::prop_assert!(w as u64 * h as u64 * d as u64 * texel * layers as u64 <= u32::MAX as u64);
            }
            // (whole blocks at every level: a 1x1 BC level is a block of 8 or 16 bytes)
            if info(fmt).is_some_and(|f| f.block > 1) && lw >= 2 + mips && lh >= 2 + mips {
                proptest::prop_assert!(page_texture_bytes(fmt, w, h, d, layers, mips, 1) >= texture_bytes(fmt, w, h, d, layers, mips, 1));
            }
        }
    }

    proptest::proptest! {
        /// Any element valid alone stays valid with others of other semantics, and an
        /// invalid one makes any layout holding it invalid.
        #[test]
        fn one_bad_element_spoils_the_layout(fmt in 0u32..140, slot in 0u32..40, class in 0u32..3, step in 0u32..3, n in 0usize..4) {
            let names = ["A", "B", "C", "D"];
            let mut v: Vec<(&str, u32, u32, u32, u32, u32)> = (0..n).map(|i| (names[i], 0, 2, 0, 0, 0)).collect();
            let e = ("E", 0, fmt, slot, class, step);
            let alone = input_elements(&[e]);
            v.push(e);
            proptest::prop_assert_eq!(input_elements(&v), alone);
        }
    }

    #[test]
    fn wine_create_input_layout() {
        let vs = isgn(&[("POSITION", 0, 0)]);
        let ok = |e: &[(&str, u32)], code: &[u8]| input_layout(true, e, &[], code);
        assert_eq!(ok(&[("POSITION", 0), ("COLOR", 0)], &vs), Ok(()));
        // names without case; an element the shader does not read is fine
        assert_eq!(ok(&[("position", 0)], &vs), Ok(()));
        assert_eq!(ok(&[], &vs), Err(E_INVALIDARG));
        assert_eq!(ok(&[("POSITION", 1), ("COLOR", 0)], &vs), Err(E_INVALIDARG));
        assert_eq!(ok(&[("SV_POSITION", 0), ("COLOR", 0)], &vs), Err(E_INVALIDARG));
        assert_eq!(ok(&[("POSITION2", 0), ("COLOR", 0)], &vs), Err(E_INVALIDARG));
        // no inputs: no elements needed, but a description array is
        let vs3 = isgn(&[]);
        assert_eq!(ok(&[], &vs3), Ok(()));
        assert_eq!(input_layout(false, &[], &[], &vs3), Err(E_INVALIDARG));
        // POSITION per instance and COLOR per vertex in slot 0; one element alone is fine,
        // and so are the two in slots of their own
        let two = [("POSITION", 0), ("COLOR", 0)];
        assert_eq!(input_layout(true, &two, &[(0, true), (0, false)], &vs), Err(E_INVALIDARG));
        assert_eq!(input_layout(true, &two[..1], &[(0, true)], &vs), Ok(()));
        assert_eq!(input_layout(true, &two, &[(0, true), (1, false)], &vs), Ok(()));
        assert_eq!(ok(&[("A", 0); 33], &vs3), Err(E_INVALIDARG));
        assert_eq!(ok(&[("A", 0); 32], &vs3), Ok(()));
        // every input is needed but SV_VertexID / SV_InstanceID (vs2 and ps of the test)
        let vs2 = [("CUSTOMDATA", 0, 0), ("POSITION", 0, 0), ("SV_Position", 0, 0), ("COLOR", 0, 0), ("SV_ClipDistance", 0, 0), ("SV_InstanceID", 0, 8), ("SV_VertexID", 0, 6)];
        let ps = [("CUSTOMDATA", 0, 0), ("POSITION", 0, 0), ("SV_Position", 0, 1), ("COLOR", 0, 0), ("SV_InstanceID", 0, 0)];
        for sig in [&vs2[..], &ps[..]] {
            let code = isgn(sig);
            let all: Vec<(&str, u32)> = sig.iter().map(|s| (s.0, s.1)).collect();
            assert_eq!(ok(&all, &code), Ok(()));
            for i in 0..all.len() {
                let mut less = all.clone();
                less.remove(i);
                let want = if sig[i].0.starts_with("SV_InstanceID") || sig[i].0 == "SV_VertexID" { Ok(()) } else { Err(E_INVALIDARG) };
                assert_eq!(ok(&less, &code), want, "{} excluded", sig[i].0);
            }
        }
        // a compute shader (no ISGN) or no bytecode: nothing needed
        assert_eq!(ok(&[], b"DXBC"), Ok(()));
        assert_eq!(ok(&[], &[]), Ok(()));
    }
}

#[cfg(test)]
mod page_coverage {
    /// Every format Windows says a texture can have (D3D11_FORMAT_SUPPORT_TEXTURE1D/2D/3D/CUBE)
    /// has a page format to upload it to: a format said to work but kept nowhere draws black
    /// (BC6H/BC7 after 105584b, Unity 2019's title screen).
    #[test]
    fn texture_formats_reach_the_page() {
        let missing: Vec<u32> = (1..200).filter(|&f| super::support(f).is_some_and(|b| b & 0xf0 != 0) && crate::d3d11::format::info(f).is_none()).collect();
        assert!(missing.is_empty(), "texture formats without a page format: {missing:?}");
    }
}
