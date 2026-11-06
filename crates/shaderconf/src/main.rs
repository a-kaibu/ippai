//! Writes shader conformance cases as JSON (stdout): each case is a pixel shader that ippai's
//! translators made WGSL of (from HLSL through hlslc and dxwgsl, from D3D9 assembly, from GLSL
//! through glwgsl), the uniform data of each pixel it is drawn at, and what the Direct3D or
//! OpenGL rules say each pixel must be. tests/shaders.mjs draws them with WebGPU (Chromium's
//! Dawn) into an rgba32float target and compares.
//!
//!   cargo run -p shaderconf > cases.json         (shaderconf --list: the case names)
//!
//! The references are written from the specs, independent of the translators: the Direct3D 11.3
//! functional spec (float, integer and conversion rules), the D3D9 instruction reference
//! (ps_3_0), the HLSL constant packing rules, the GLSL 1.20 spec and std140 for GL.

mod cases;
mod json;

/// How a pixel's channel compares.
#[derive(Clone, Copy, Debug)]
pub enum Tol {
    /// the same value (+0 and -0 alike; NaN wanted: any NaN)
    Exact,
    /// within n units in the last place
    Ulp(u32),
    /// within an absolute error
    Abs(f32),
    /// within a relative error (or the absolute one near 0)
    Rel(f32, f32),
}

/// One expected channel: any of these values (implementation-defined results list each allowed
/// one), or None for not compared.
pub type Want = Option<Vec<f32>>;

pub struct Case {
    pub name: String,
    /// which translator: "dxbc", "d3d9", "glsl"
    pub api: &'static str,
    pub wgsl: String,
    /// the uniform buffer the shader reads: group, binding, bytes
    pub group: u32,
    pub binding: u32,
    /// per pixel: the uniform buffer's bytes, and its 4 expected channels
    pub pixels: Vec<(Vec<u8>, [Want; 4])>,
    pub tol: Tol,
    /// the spec rule the case checks
    pub rule: &'static str,
}

fn main() {
    let list = std::env::args().any(|a| a == "--list");
    let mut out = Vec::new();
    let mut errors = Vec::new();
    cases::all(&mut out, &mut errors);
    if list {
        for c in &out {
            println!("{} ({} pixels): {}", c.name, c.pixels.len(), c.rule);
        }
    } else {
        let mut s = String::from("[\n");
        for (i, c) in out.iter().enumerate() {
            if i > 0 {
                s.push_str(",\n");
            }
            json::case(&mut s, c);
        }
        s.push_str("\n]\n");
        print!("{s}");
    }
    for e in &errors {
        eprintln!("{e}");
    }
    eprintln!("shaderconf: {} cases, {} pixels, {} that did not translate", out.len(), out.iter().map(|c| c.pixels.len()).sum::<usize>(), errors.len());
    if !errors.is_empty() {
        std::process::exit(1);
    }
}
