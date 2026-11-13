//! sake's binary formats from files and the registry: shell links (.lnk), event log records
//! and .evt files, PE resource trees (UpdateResource), type libraries, CryptStringToBinary /
//! CryptBinaryToString. No panic on any bytes; what is decoded encodes back to the same.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::cryptstr;

fuzz_target!(|data: &[u8]| {
    let Some((&k, b)) = data.split_first() else { return };
    match k % 6 {
        0 => {
            if let Some(l) = sake::shelllink::decode(b) {
                assert_eq!(sake::shelllink::decode(&sake::shelllink::encode(&l)), Some(l));
            }
        }
        1 => {
            if let Some(r) = sake::eventlog::Rec::decode(b) {
                let _ = r.encode(true);
            }
            if let Some(log) = sake::eventlog::Log::from_evt(b) {
                assert_eq!(sake::eventlog::Log::from_evt(&log.evt()), Some(log));
            }
        }
        2 => {
            if let Some(tree) = sake::resupdate::read(b) {
                if let Some(out) = sake::resupdate::write(b, &tree) {
                    assert_eq!(sake::resupdate::read(&out), Some(tree));
                }
            }
        }
        3 => {
            let _ = sake::typelib::parse(b);
        }
        4 => {
            // any text: no panic
            let s: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            for fmt in [cryptstr::BASE64HEADER, cryptstr::BASE64, cryptstr::BINARY, cryptstr::BASE64_ANY, cryptstr::ANY, cryptstr::BASE64REQUESTHEADER, cryptstr::BASE64X509CRLHEADER] {
                let _ = cryptstr::decode(&s, b, fmt);
            }
        }
        _ => {
            // any data: what an encoding gives decodes back to it
            let flags = [0, cryptstr::NOCR, cryptstr::NOCRLF][(b.len() % 3) as usize];
            for fmt in [cryptstr::BASE64, cryptstr::BASE64HEADER, cryptstr::BASE64REQUESTHEADER, cryptstr::BASE64X509CRLHEADER] {
                let s: Vec<u16> = cryptstr::encode(b, fmt | flags).unwrap().encode_utf16().collect();
                assert_eq!(cryptstr::decode(&s, &[], fmt).map(|r| r.0), Ok(b.to_vec()), "format {fmt:#x}");
            }
        }
    }
});
