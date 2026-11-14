//! dxwgsl on any DXBC container (D3D10/11 shader model 4/5 bytecode from game files and
//! D3DCompile): no panic, and a shader it takes is WGSL naga parses and validates.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&k, code)) = data.split_first() else { return };
    let r = match k % 5 {
        0 => dxwgsl::translate(code, false, None),
        1 => dxwgsl::translate(code, true, None),
        2 => dxwgsl::translate_compute(code, &[]),
        // (D3D11 point lists: the vertex shader drawn as sprites, its PSIZE from the signature)
        3 => dxwgsl::translate_point_sprite(code, None, 64.0),
        _ => {
            // a vertex shader with the pixel shader it feeds (the second half)
            let (vs, ps) = code.split_at(code.len() / 2);
            dxwgsl::translate(vs, false, Some(ps))
        }
    };
    if let Ok(w) = r {
        ippai_fuzz::valid_wgsl(&w.code);
    }
});
