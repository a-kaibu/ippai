//! The WMA 1 / 2 decoder (crates/wmcodec::wma, FFmpeg's ported): any stream parameters and any
//! packets: no panic, and a packet gives whole frames of samples per channel.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() < 12 {
        return;
    }
    let tag = if data[0] & 1 == 0 { 0x160 } else { 0x161 };
    let channels = (data[1] % 2 + 1) as u16;
    let rate = [8000, 11025, 16000, 22050, 32000, 44100, 48000][data[2] as usize % 7];
    let bitrate = (u16::from_le_bytes([data[3], data[4]]) as u32 + 1) * 64;
    let align = (u16::from_le_bytes([data[5], data[6]]) % 4096 + 1) as u16;
    let extra = &data[7..13.min(data.len())];
    let Ok(mut d) = wmcodec::wma::Decoder::new(tag, channels, rate, bitrate, align, extra) else { return };
    for p in data[13.min(data.len())..].chunks(align as usize) {
        if let Ok(planes) = d.decode_raw(p) {
            assert_eq!(planes.len(), channels as usize);
            for c in &planes {
                assert_eq!(c.len() % d.frame_len, 0);
                assert_eq!(c.len(), planes[0].len());
            }
        }
    }
    d.flush();
    let _ = d.drain();
});
