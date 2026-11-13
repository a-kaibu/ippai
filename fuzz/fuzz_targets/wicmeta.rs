//! WIC frame metadata from a game's PNG (sake::wicmeta::png, IWICMetadataQueryReader): no
//! panic on any bytes, and every item it answers is found again by its name in any case.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut f = b"\x89PNG\r\n\x1a\n".to_vec();
    f.extend_from_slice(data);
    for d in [data, &f[..]] {
        if let Some(m) = sake::wicmeta::png(d) {
            for (n, v) in &m.items {
                assert!(m.blocks.iter().any(|b| n.starts_with(&format!("{b}/"))));
                let got = m.get(&n.to_ascii_uppercase()).expect("item by its name");
                // (the first of repeated names wins)
                assert_eq!(got.vt(), m.items.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).unwrap().1.vt());
                let _ = v;
            }
        }
    }
});
