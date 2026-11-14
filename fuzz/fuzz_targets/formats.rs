//! sake's binary formats from files and the registry: shell links (.lnk), event log records
//! and .evt files, PE resource trees (UpdateResource), type libraries, CryptStringToBinary /
//! CryptBinaryToString; DirectShow source filters' file sniffing (ASF headers, check bytes) and
//! RGB rows for a game's video renderer (#200, #215). No panic on any bytes; what is decoded
//! encodes back to the same.
#![no_main]

use libfuzzer_sys::fuzz_target;
use sake::cryptstr;

fuzz_target!(|data: &[u8]| {
    let Some((&k, b)) = data.split_first() else { return };
    match k % 7 {
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
        5 => {
            // a movie file's first bytes: the source filter, its ASF streams (at most as many
            // as stream properties objects fit), RGB rows of any size and depth
            let asf = sake::dshow_mpeg::source_is_asf("movie.dat", b);
            if let Some(st) = sake::dshow_mpeg::asf_streams(b) {
                assert!(asf);
                assert!(st.len() * 78 <= b.len());
                assert!(sake::dshow_mpeg::asf_pin_names(&st).len() <= 3);
            }
            if let [w, h, d, px @ ..] = b {
                let (w, h) = (*w as u32 % 64 + 1, *h as u32 % 16 + 1);
                let bits = sake::dshow_mpeg::RGB_BITS[*d as usize % 4];
                let mut src = px.to_vec();
                src.resize((4 * w * h) as usize, 0x5a);
                let out = sake::dshow_mpeg::pack_bgrx(&src, w, h, bits);
                assert_eq!(out.len() as u32, if bits == 32 { 4 * w * h } else { sake::dshow_mpeg::dib_stride(w, bits) * h });
            }
            let len = u64::from_le_bytes(b.get(..8).map_or([0; 8], |x| x.try_into().unwrap()));
            let (at, n) = sake::dshow_mpeg::read_span(len / 3, b.len() as u32, len);
            assert!(at + n as u64 <= len);
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
