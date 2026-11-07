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
