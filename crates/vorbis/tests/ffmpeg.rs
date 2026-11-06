//! Whole streams against FFmpeg's own Vorbis decoder (an independent implementation):
//! testdata/<name>.ogg (made with FFmpeg's libvorbis encoder) decoded by `ffmpeg -c:a vorbis`
//! to 16-bit PCM (<name>.s16). Every sample must be within 2 steps of 16-bit PCM, from the
//! first one (the same start: no samples before the second packet).

use vorbis::{ogg, Decoder};

fn decode(name: &str) -> (Vec<f32>, usize, u64) {
    let data = std::fs::read(format!("{}/testdata/{name}.ogg", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let (packets, granule) = ogg::packets(&data);
    let mut d = Decoder::new(&packets[0], &packets[2]).unwrap();
    let ch = d.setup.ident.channels;
    let mut out = Vec::new();
    for p in &packets[3..] {
        d.packet(p, &mut out);
    }
    (out, ch, granule)
}

fn check(name: &str) {
    let (ours, ch, granule) = decode(name);
    let raw = std::fs::read(format!("{}/testdata/{name}.s16", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let want: Vec<f32> = raw.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    // FFmpeg trims the end to the last granule position
    let frames = (granule as usize).min(ours.len() / ch);
    // (FFmpeg's channel order for 5.1: FL FR FC LFE BL BR; Vorbis's: FL FC FR BL BR LFE)
    let order: Vec<usize> = if ch == 6 { vec![0, 2, 1, 5, 3, 4] } else { (0..ch).collect() };
    let ours: Vec<f32> = ours.chunks(ch).flat_map(|f| order.iter().map(|&k| f[k]).collect::<Vec<_>>()).collect();
    // (FFmpeg may stop a packet short of the granule position)
    let frames = frames.min(want.len() / ch);
    assert!(frames + 2048 >= granule as usize, "{name}: {frames} frames, granule {granule}");
    let mut worst = 0f32;
    for (i, (&a, &b)) in ours.iter().zip(&want).take(frames * ch).enumerate() {
        let a16 = (a * 32768.0).round().clamp(-32768.0, 32767.0) / 32768.0;
        let e = (a16 - b).abs();
        assert!(e <= 2.0 / 32768.0, "{name}: frame {} channel {}: {a} vs {b}", i / ch, i % ch);
        worst = worst.max(e);
    }
    eprintln!("{name}: {frames} frames x {ch}, worst {} steps", worst * 32768.0);
}

#[test]
fn stereo_q5() {
    check("stereo-q5");
}
#[test]
fn mono_q0() {
    check("mono-q0");
}
#[test]
fn surround_q3() {
    check("surround-q3");
}
#[test]
fn stereo_q10_silence() {
    check("stereo-q10-silence");
}
#[test]
fn stereo_48k() {
    check("stereo-48k");
}
