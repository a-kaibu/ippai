//! The Vorbis decoder sake swaps in for a game's own libvorbis (crates/vorbis): Ogg pages to
//! packets, the identification and setup headers, then every audio packet, on any bytes. No
//! panic, no hang, and the output length is what the blocks give (crates/vorbis/tests/robust.rs
//! checks random mutations of one stream; this follows coverage into the setup parser,
//! codebooks, floors and residues).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // (an Ogg stream, or with a leading 0xff the raw packets: u16 lengths then bytes, so the
    // fuzzer reaches the header parsers without having to get page CRCs right)
    let packets: Vec<Vec<u8>> = if data.first() == Some(&0xff) {
        let mut v = Vec::new();
        let mut d = &data[1..];
        while d.len() >= 2 {
            let n = (u16::from_le_bytes([d[0], d[1]]) as usize).min(d.len() - 2);
            v.push(d[2..2 + n].to_vec());
            d = &d[2 + n..];
        }
        v
    } else {
        vorbis::ogg::packets(data).0
    };
    if packets.len() < 3 {
        return;
    }
    let Ok(mut dec) = vorbis::Decoder::new(&packets[0], &packets[2]) else { return };
    let ch = vorbis::Ident::read(&packets[0]).map(|i| i.channels).unwrap_or(1);
    let mut out = Vec::new();
    for p in &packets[3..] {
        let before = out.len();
        dec.packet(p, &mut out);
        assert_eq!((out.len() - before) % ch, 0, "a partial frame");
    }
});
