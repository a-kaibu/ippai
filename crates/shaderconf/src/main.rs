//! Writes shader conformance cases as JSON (stdout): each case is a pixel shader that ippai's
//! translators made WGSL of (from HLSL through hlslc and dxwgsl, from D3D9 assembly, from GLSL
//! through glwgsl), the uniform data of each pixel it is drawn at, and what the Direct3D or
//! OpenGL rules say each pixel must be. tests/shaders.mjs draws them with WebGPU (Chromium's
//! Dawn) into an rgba32float target and compares.
//!
//!   cargo run -p shaderconf > cases.json         (shaderconf --list: the case names)
//!   cargo run -p shaderconf -- --compute > compute.json   (tests/compute.mjs: compute shaders)
//!
//! The references are written from the specs, independent of the translators: the Direct3D 11.3
//! functional spec (float, integer and conversion rules), the D3D9 instruction reference
//! (ps_3_0), the HLSL constant packing rules, the GLSL 1.20 spec and std140 for GL.

mod cases;
mod compute;
mod fuzz;
mod json;
mod oracle;
mod random;
mod ref_d3d9;
mod ref_dxbc;
#[cfg(test)]
mod ref_tests;

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
    /// what it was translated from (HLSL, assembly or GLSL)
    pub src: String,
    /// the bytecode dxwgsl translated (DXBC, D3D9 tokens; empty for GLSL)
    pub code: Vec<u8>,
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
    if std::env::args().any(|a| a == "--compute") {
        match compute::cases() {
            Ok(s) => print!("{s}"),
            Err(e) => {
                eprintln!("shaderconf: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    let list = std::env::args().any(|a| a == "--list");
    let mut out = Vec::new();
    let mut errors = Vec::new();
    cases::all(&mut out, &mut errors);
    if let Some(n) = std::env::args().skip_while(|a| a != "--src").nth(1) {
        for c in out.iter().filter(|c| c.name == n) {
            println!("{}\n----\n{}", c.src, c.wgsl);
        }
        return;
    }
    let mut a = std::env::args().skip_while(|a| a != "--fuzz").skip(1);
    if let Some(n) = a.next() {
        let n = n.parse().expect("--fuzz N [seed]");
        let seed = a.next().map_or(1, |s| s.parse().expect("--fuzz N [seed]"));
        // (the inputs that broke a translator before, then new ones)
        let bad = fuzz::regressions(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("regress"));
        for b in &bad {
            println!("REGRESSION {b}");
        }
        std::process::exit(if fuzz::run(&out, n, seed) && bad.is_empty() { 0 } else { 1 });
    }
    let mut a = std::env::args().skip_while(|a| a != "--oracle").skip(1);
    if let Some(n) = a.next() {
        let n = n.parse().expect("--oracle N [seed]");
        let seed = a.next().map_or(1, |s| s.parse().expect("--oracle N [seed]"));
        let m = oracle::cases(&out, n, seed);
        eprintln!("shaderconf: {} mutants of {n} with an interpreted result", m.len());
        out = m;
    }
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

#[cfg(test)]
mod tests {
    /// Every case translates, to WGSL naga takes, and asks for something (cargo test, and cargo
    /// mutants on the translators: a mutant that breaks their output shows here without a browser).
    #[test]
    fn every_case_is_valid_wgsl() {
        let (mut out, mut errors) = (Vec::new(), Vec::new());
        super::cases::all(&mut out, &mut errors);
        assert!(errors.is_empty(), "{}", errors.join("\n"));
        assert!(out.len() > 2000, "{} cases", out.len());
        for c in &out {
            if let Err(e) = super::fuzz::valid(&c.wgsl) {
                panic!("{}: {e}\n{}", c.name, c.wgsl);
            }
            assert!(!c.pixels.is_empty() && c.pixels.iter().any(|p| p.1.iter().any(|w| w.is_some())), "{}: nothing compared", c.name);
        }
    }
}
