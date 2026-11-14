//! koji's saved translations index (what `masu serve` hands the page back: any bytes may come
//! from the server or a damaged file): no panic; a whole index is the one decode_all reads;
//! every entry read is consistent (a root, its code's bytes) and has its names.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let all = koji::jit::saved::decode_all(data);
    if let Some(es) = &all {
        assert!(es.iter().all(|e| e.consistent()));
        let _ = koji::jit::saved::func_names(es);
    }
    if let Some(one) = koji::jit::saved::decode(data) {
        assert_eq!(all.map(|a| a.len()), Some(one.len()));
    }
});
