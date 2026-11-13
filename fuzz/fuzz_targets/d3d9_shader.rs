//! dxwgsl's Direct3D 9 shader translation (vs/ps 1.x-3.0 token streams) on any tokens: no
//! panic, and a shader it parses becomes WGSL naga parses and validates.
#![no_main]

use dxwgsl::d3d9::{Gen, Options, Shader};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&k, rest)) = data.split_first() else { return };
    let words: Vec<u32> = rest.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
    let Some(s) = Shader::parse(&words) else { return };
    let opts = Options { link: 0x3ff, vs_inputs: Vec::new(), alpha_test: (k & 1 != 0).then_some(0x0003_0001), fog: k & 2 != 0, addr: Vec::new(), targets: 1 + (k >> 2) as u32 % 4 };
    let code = if s.pixel { Gen::new(&s).pixel(&opts) } else { Gen::new(&s).vertex(&opts) };
    ippai_fuzz::valid_wgsl(&code);
});
