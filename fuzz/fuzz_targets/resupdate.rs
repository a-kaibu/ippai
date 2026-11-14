//! BeginUpdateResource/EndUpdateResource's PE rewrite (sake::resupdate::read and write) on any
//! bytes: no panic, no more resource data read than the file holds, and a file it wrote reads
//! back as the tree written.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some(t) = sake::resupdate::read(data) else { return };
    let total: usize = t.values().flat_map(|n| n.values()).flat_map(|l| l.values()).map(|(_, d)| d.len()).sum();
    assert!(total <= data.len());
    if let Some(out) = sake::resupdate::write(data, &t) {
        assert_eq!(sake::resupdate::read(&out).as_ref(), Some(&t));
    }
});
