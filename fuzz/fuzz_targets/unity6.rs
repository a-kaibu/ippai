//! Unity 6's allocator patch (sake::unity6::plan) on arbitrary code: no panic, and every change
//! it plans is inside the code, apart from the others and keeps its instruction (a constant
//! changes, nothing else). The first byte picks real allocator pieces to splice in at offsets
//! the next bytes give, so the fuzzer gets past the recognizer too.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::unity6::{plan, same_instructions, samples::*};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fuzz_target!(|data: &[u8]| {
    let Some((&pick, rest)) = data.split_first() else { return };
    let mut code = rest.to_vec();
    for (k, p) in [REG, LOOKUP, RESERVE, CTOR, ROUND, FIELD, FLAG].iter().enumerate() {
        if pick & (1 << k) != 0 {
            let at = code.get(k).map_or(0, |&b| b as usize * code.len() / 256);
            code.splice(at..at, hex(p));
        }
    }
    let ip = 0x1_8000_1000;
    if let Some((edits, f)) = plan(&code, ip) {
        assert!(same_instructions(&code, ip, &edits));
        assert_eq!(edits.len(), f.shifts + f.sizes + f.masks + f.rounds);
        assert!(f.anchor >= ip && f.anchor < ip + code.len() as u64);
    }
});
