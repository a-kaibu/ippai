//! DDS files as D3DX reads them (sake::dds, #126): no panic, and every level of every face a
//! parsed header names lies inside the data, the levels one after another without overlap.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Some(d) = sake::dds::parse(data) {
        let mut at = 128;
        for face in 0..6 {
            for l in 0..d.levels {
                if let Some((o, n)) = d.level_at(face, l) {
                    assert_eq!(o, at);
                    assert!(o + n <= data.len());
                    at += n;
                }
            }
        }
        assert!(d.levels >= 1 && d.w >= 1 && d.h >= 1);
    }
});
