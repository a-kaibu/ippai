//! bcrypt/ncrypt key blobs (sake::cng) on any bytes after a header of each RSA / ECC magic: no
//! panic (the header's sizes are the guest's: their sum must not wrap, review b05), and an
//! accepted RSA blob is exactly as long as its header says, counted without wrapping. A PSS
//! salt from the guest (any u32) signs or fails without allocating it.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::cng::{blob_need, Blob, Ecc, Hash, Pad, Rsa};
use std::sync::OnceLock;

fuzz_target!(|data: &[u8]| {
    let Some((&k, body)) = data.split_first() else { return };
    let kind = [Blob::Public, Blob::Private, Blob::FullPrivate][k as usize % 3];
    let magic: u32 = [0x3141_5352, 0x3241_5352, 0x3341_5352][k as usize % 3];
    let mut b = magic.to_le_bytes().to_vec();
    b.extend_from_slice(body);
    if let Ok(_) = Rsa::from_blob(kind, k & 0x80 != 0, &b) {
        let f = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(blob_need(kind, f(8), f(12), f(16), f(20)), b.len() as u64);
    }
    let _ = Ecc::from_blob(body, None);
    let _ = Ecc::from_blob(body, Some(k & 1 != 0));
    if body.len() >= 4 {
        static KEY: OnceLock<Rsa> = OnceLock::new();
        let key = KEY.get_or_init(|| Rsa::generate(512).unwrap());
        let salt = u32::from_le_bytes(body[..4].try_into().unwrap()) as usize;
        let d = vec![0x11; 32];
        if let Ok(s) = key.sign(Pad::Pss(Hash::Sha256, salt), &d) {
            assert!(key.verify(Pad::Pss(Hash::Sha256, salt), &d, &s).is_ok());
        }
    }
});
