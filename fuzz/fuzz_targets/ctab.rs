//! ID3DXConstantTable's reading of a shader's CTAB comment (sake::d3dx9shader, from a game's
//! shader bytecode) on any bytes: no panic, at most MAX_CONSTANTS entries however the members
//! and elements nest or refer to themselves, and Set*Array's layout of any number of values
//! joined into register runs without losing a write (review b05).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&k, d)) = data.split_first() else { return };
    sake::d3dx9shader::check_ctab(d, k as usize * 17);
});
