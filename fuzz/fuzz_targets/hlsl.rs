//! hlslc (D3DXCompileShader / D3DCompile / D3DXCreateEffect from a game's HLSL text, and
//! D3DXAssembleShader's assembly) on any text: no panic, and what it compiles goes on to the
//! translators (dxwgsl) without panicking either. The first byte picks the target and the job.
#![no_main]

use libfuzzer_sys::fuzz_target;

const TARGETS: [&str; 8] = ["vs_2_0", "ps_2_0", "vs_3_0", "ps_3_0", "vs_4_0", "ps_4_0", "ps_5_0", "cs_5_0"];

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else { return };
    let src = String::from_utf8_lossy(rest);
    let mut none = |_: &str, _: bool| None;
    let target = TARGETS[(k & 7) as usize];
    match k >> 3 & 3 {
        0 | 1 => {
            let Ok(code) = hlslc::compile(&src, &[], &mut none, "main", target, (k >> 5) as u32 & 1) else { return };
            if target.ends_with("_4_0") || target.ends_with("_5_0") {
                let _ = if target.starts_with("cs") { dxwgsl::translate_compute(&code, &[]) } else { dxwgsl::translate(&code, target.starts_with("ps"), None) };
            } else {
                let words: Vec<u32> = code.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
                assert!(dxwgsl::d3d9::Shader::parse(&words).is_some(), "hlslc's {target} bytecode does not parse");
            }
        }
        2 => {
            if let Ok(fx) = hlslc::compile_effect(&src, &[], &mut none, 0) {
                sake::d3dx9fx::check(&fx).expect("hlslc's effect reads back");
            }
        }
        _ => {
            if let Ok(words) = hlslc::d3d9asm::assemble(&src) {
                let _ = dxwgsl::d3d9::Shader::parse(&words);
            }
        }
    }
});
