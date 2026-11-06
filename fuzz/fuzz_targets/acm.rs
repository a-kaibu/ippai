//! The ACM ADPCM decoders (IMA and Microsoft ADPCM blocks from a game's WAVs): no panic for
//! any block, channel count or samples-per-block the format header claims.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 3 {
        return;
    }
    let ch = (data[0] & 7) as usize + 1;
    let spb = u16::from_le_bytes([data[1], data[2]]) as usize;
    let block = &data[3..];
    let mut out = vec![0i16; 3];
    if data[0] & 0x80 != 0 {
        sake::acm::ima_block(block, ch, spb, &mut out);
    } else {
        sake::acm::ms_block(block, ch, spb, &mut out);
    }
    assert!(out.len() == 3 || out.len() == 3 + spb * ch, "{} samples", out.len());
});
