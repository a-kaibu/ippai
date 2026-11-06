//! An HLSL compiler (what d3dcompiler_*.dll's D3DCompile does): preprocessor (pp.rs), parser
//! (parse.rs), lowering with every call inlined (lower.rs, intrinsics.rs, prelude.rs) and shader
//! model 4/5 DXBC output (sm4.rs), which kura's DXBC to WGSL translation reads, or shader model
//! 2/3 bytecode (sm3.rs) for Direct3D 9.

mod ast;
mod intrinsics;
mod lower;
mod parse;
pub mod pp;
mod prelude;
pub mod sm3;
pub mod sm4;

/// D3DCOMPILE_* flags used here
pub const PACK_MATRIX_ROW_MAJOR: u32 = 1 << 3;

pub struct Target {
    pub pixel: bool,
    pub major: u32,
    pub minor: u32,
}

/// "ps_4_0", "vs_5_0", "vs_4_0_level_9_3"...
pub fn parse_target(t: &str) -> Result<Target, String> {
    let v: Vec<&str> = t.split('_').collect();
    if v.len() < 3 {
        return Err(format!("unknown target {t}"));
    }
    let pixel = match v[0] {
        "ps" => true,
        "vs" => false,
        s => return Err(format!("{s} shaders are not supported")),
    };
    let major: u32 = v[1].parse().map_err(|_| format!("unknown target {t}"))?;
    let minor: u32 = if v[2] == "sw" {
        0
    } else {
        v[2].parse().unwrap_or(0)
    };
    Ok(Target {
        pixel,
        major,
        minor,
    })
}

/// Preprocessed tokens of `src`.
pub fn preprocess(
    src: &str,
    defines: &[(String, String)],
    include: &mut dyn FnMut(&str, bool) -> Option<String>,
) -> Result<Vec<pp::Token>, String> {
    pp::preprocess(src, defines, include)
}

/// HLSL source to shader model 2/3 assembly (for tests and dumps).
pub fn assembly(
    src: &str,
    defines: &[(String, String)],
    include: &mut dyn FnMut(&str, bool) -> Option<String>,
    entry: &str,
    target: &str,
    flags: u32,
) -> Result<String, String> {
    let t = parse_target(target)?;
    let shader = lowered(src, defines, include, entry, &t, flags)?;
    Ok(sm3::assembly(shader)?.0)
}

fn lowered(
    src: &str,
    defines: &[(String, String)],
    include: &mut dyn FnMut(&str, bool) -> Option<String>,
    entry: &str,
    t: &Target,
    flags: u32,
) -> Result<sm4::Shader, String> {
    let toks = pp::preprocess(src, defines, include)?;
    let mut p = parse::Parser::new(toks);
    p.program()?;
    let prog = p.prog;
    let mut none = |_: &str, _: bool| None;
    let pre_toks = pp::preprocess(&prelude::source(), &[], &mut none)?;
    let mut pp2 = parse::Parser::new(pre_toks);
    pp2.program().map_err(|e| format!("prelude: {e}"))?;
    let pre = pp2.prog;
    // (the prelude shares the program's struct table: it has no structs)
    let minor = if t.major >= 5 {
        0
    } else if t.major == 4 {
        t.minor.min(1)
    } else {
        t.minor
    };
    let o = lower::Opts {
        pixel: t.pixel,
        major: t.major.min(5),
        minor,
        row_major: flags & PACK_MATRIX_ROW_MAJOR != 0,
    };
    let l = lower::L::new(o, &prog, &pre);
    l.entry(entry)
}

/// HLSL source to a DXBC blob (or shader model 2/3 bytecode).
pub fn compile(
    src: &str,
    defines: &[(String, String)],
    include: &mut dyn FnMut(&str, bool) -> Option<String>,
    entry: &str,
    target: &str,
    flags: u32,
) -> Result<Vec<u8>, String> {
    let t = parse_target(target)?;
    let shader = lowered(src, defines, include, entry, &t, flags)?;
    if t.major < 4 {
        return sm3::finish(shader);
    }
    Ok(sm4::finish(shader))
}

#[cfg(test)]
mod tests;
