//! koji's saved translations index (what `ippai serve` hands the page back: any bytes may come
//! from the server or a damaged file): no panic, a root in every entry; a whole index is the one decode_all reads.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let all = koji::jit::saved::decode_all(data);
    // (every entry read has a root: kura takes roots[0])
    for e in all.iter().flatten() {
        assert!(!e.roots.is_empty());
        let _ = e.name();
    }
    if let Some(one) = koji::jit::saved::decode(data) {
        assert_eq!(all.map(|a| a.len()), Some(one.len()));
    }
});
