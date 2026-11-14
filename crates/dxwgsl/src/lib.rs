//! Direct3D shader bytecode to WGSL.
//!
//! - Direct3D 10/11 (DXBC, shader model 4/5): [`translate`] and [`signature`] here; dxbc.rs reads
//!   the container, tpf.rs the shader model 4/5 program, wgsl.rs writes the WGSL.
//! - Direct3D 8/9 (vs/ps 1.x-3.0 bytecode): [`d3d9`]; the fixed-function pipeline: [`d3d9ff`].
//!
//! Bytes in, WGSL text and what it binds out; no GPU or OS calls, std only.

pub mod addr;
pub mod d3d9;
pub mod d3d9ff;
pub mod dxbc;
pub mod tpf;
mod wgsl;

/// A finite f32 as WGSL literal text (no suffix): Rust's shortest decimal, unless that decimal
/// read as a double lies past f32's range. Browsers' WGSL compiler (Tint) reads a literal as a
/// double and rejects it when it exceeds f32's largest value ("value cannot be represented as
/// 'f32'"): the shortest text of f32::MAX, 3.4028235e38, does (Unity's shaders use FLT_MAX as
/// 3.402823466e+38); the double's own shortest text (3.4028234663852886e38) is exact.
pub fn f32_text(f: f32) -> String {
    let s = format!("{f:?}");
    match s.parse::<f64>() {
        Ok(d) if d.abs() <= f32::MAX as f64 => s,
        _ => format!("{:?}", f as f64),
    }
}

/// A shader's input (`output` false) or output signature.
pub fn signature(code: &[u8], output: bool) -> Vec<crate::dxbc::SigElement> {
    crate::dxbc::Container::parse(code).map_or(Vec::new(), |c| c.signature(output))
}

/// What a shader binds (group = stage)
#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    /// constant buffer: size the shader reads
    Uniform(u32),
    /// texture: view dimension (0 1d, 1 2d, 2 2d array, 3 cube, 4 cube array, 5 3d, 6
    /// multisampled 2d), sample
    /// type (0 float, 1 uint, 2 sint, 3 depth)
    Texture(u32, u32),
    Sampler(bool),
    /// compute shaders: a write-only storage texture (a typed UAV): view dimension as for
    /// textures, its WebGPU format
    Storage(u32, String),
}

pub struct Wgsl {
    pub code: String,
    /// (binding, what)
    pub bindings: Vec<(u32, Binding)>,
}

/// DXBC to WGSL; `ps`: the pixel shader a vertex shader feeds (its outputs go to that shader's
/// input registers).
pub fn translate(code: &[u8], pixel: bool, ps: Option<&[u8]>) -> Result<Wgsl, String> {
    crate::wgsl::translate(code, pixel, ps)
}

pub use crate::wgsl::{translate_addr, translate_compute, translate_point_sprite, uav_slots};

/// The "geometry shader" sake's D3DCompile makes of ANGLE's point sprite geometry shader: this
/// tag, then the largest point size (f32). The D3D11 device draws points with it as sprites
/// (translate_point_sprite).
pub const POINT_SPRITE_GS: &[u8; 8] = b"SAKEPSGS";

/// The largest point size of a POINT_SPRITE_GS blob.
pub fn point_sprite_gs(code: &[u8]) -> Option<f32> {
    let rest = code.strip_prefix(POINT_SPRITE_GS.as_slice())?;
    Some(f32::from_le_bytes(rest.get(..4)?.try_into().ok()?))
}

#[cfg(test)]
mod f32_text_tests {
    use super::f32_text;
    use proptest::prelude::*;

    /// Tint's rule: the text as a double must not exceed f32's range, and must still name the
    /// same f32
    fn tint_accepts(t: &str, f: f32) -> bool {
        let d: f64 = t.parse().unwrap();
        d.abs() <= f32::MAX as f64 && d as f32 == f && (d as f32).to_bits() == f.to_bits()
    }

    #[test]
    fn largest_values() {
        assert_eq!(f32_text(f32::MAX), "3.4028234663852886e38");
        assert_eq!(f32_text(f32::MIN), "-3.4028234663852886e38");
        assert_eq!(f32_text(1.5), "1.5");
        assert_eq!(f32_text(-0.0), "-0.0");
        for f in [f32::MAX, f32::MIN, f32::from_bits(0x7f7f_fffe), f32::MIN_POSITIVE, f32::from_bits(1), 3.402823466e38] {
            assert!(tint_accepts(&f32_text(f), f), "{f:?} -> {}", f32_text(f));
        }
    }

    proptest! {
        #[test]
        fn every_finite_value_reads_back(bits in any::<u32>()) {
            let f = f32::from_bits(bits);
            prop_assume!(f.is_finite());
            let t = f32_text(f);
            prop_assert!(tint_accepts(&t, f), "{:?} -> {}", f, t);
        }
        // the top of the range, where shortest decimals round up past f32::MAX
        #[test]
        fn top_of_the_range_reads_back(k in 0u32..0x10000) {
            for sign in [0u32, 0x8000_0000] {
                let f = f32::from_bits((0x7f7f_ffff - k) | sign);
                let t = f32_text(f);
                prop_assert!(tint_accepts(&t, f), "{:?} -> {}", f, t);
            }
        }
    }
}
