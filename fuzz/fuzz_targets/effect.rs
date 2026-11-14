//! D3DXCreateEffect's reading of a compiled fx_2_0 effect (sake::d3dx9fx, from a game's .fxo
//! or resource) on any bytes: no panic, no allocation beyond the file's own size, and every
//! parameter's value inside its top-level parameter's bytes.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut fx = 0xfeff0901u32.to_le_bytes().to_vec();
    fx.extend_from_slice(data);
    for d in [data, &fx[..]] {
        let _ = sake::d3dx9fx::check(d);
    }
});
