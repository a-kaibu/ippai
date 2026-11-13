//! Whole streams against FFmpeg's own MPEG-1 video and MP2 decoders (independent
//! implementations): testdata/testsrc.mpg (FFmpeg's mpeg1video with B pictures, GOP 12, and
//! mp2 stereo 44.1 kHz) decoded by FFmpeg to yuv420p (testsrc.yuv) and s16 (testsrc.s16);
//! mono32k.mp2 (mono 32 kHz) to mono32k.s16. The IDCTs differ (pl_mpeg's integer one against
//! FFmpeg's), so pictures must be close, not equal: every picture at 40 dB PSNR or better.
//! Audio: pl_mpeg's integer dequantizer leaves noise around 60 dB below FFmpeg's float one;
//! every stream at 55 dB SNR or better, no sample more than 16 steps (of 16-bit PCM) off. (pl_mpeg
//! itself was 4.8 % too quiet, 27 dB: see audio.rs.)

use mpeg1::{ps, Audio, Video};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/testdata/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let mse = a.iter().zip(b).map(|(&x, &y)| (x as f64 - y as f64).powi(2)).sum::<f64>() / a.len() as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

#[test]
fn video_matches_ffmpeg() {
    let s = data("testsrc.mpg");
    let want = data("testsrc.yuv");
    let mut v = Video::new();
    for p in ps::packets(&s) {
        if ps::is_video(p.id) {
            v.push(&p.data);
        }
    }
    v.end();
    let (w, h) = (64, 48);
    let size = w * h * 3 / 2;
    let mut n = 0;
    let mut worst = f64::INFINITY;
    while let Some(f) = v.decode() {
        assert_eq!((f.width, f.height), (w, h));
        let mut ours = Vec::with_capacity(size);
        for y in 0..h {
            ours.extend_from_slice(&f.y[y * f.y_stride..y * f.y_stride + w]);
        }
        for plane in [&f.cb, &f.cr] {
            for y in 0..h / 2 {
                ours.extend_from_slice(&plane[y * f.c_stride..y * f.c_stride + w / 2]);
            }
        }
        let theirs = &want[n * size..(n + 1) * size];
        let p = psnr(&ours, theirs);
        assert!(p >= 40.0, "picture {n}: {p:.1} dB");
        worst = worst.min(p);
        n += 1;
    }
    assert_eq!(n, want.len() / size, "pictures");
    eprintln!("{n} pictures, worst {worst:.1} dB");
}

fn audio_check(es: &[u8], want: &[u8], channels: usize) {
    let want: Vec<i16> = want.chunks(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect();
    let mut a = Audio::new();
    a.push(es);
    a.end();
    let mut ours = Vec::new();
    while a.decode() {
        for f in a.out.chunks(2) {
            for &x in &f[..channels] {
                ours.push((x * 32768.0).round().clamp(-32768.0, 32767.0) as i16);
            }
        }
    }
    // (FFmpeg may drop the end of the last frame)
    assert!(ours.len() >= want.len() && ours.len() < want.len() + 1152 * channels, "{} vs {}", ours.len(), want.len());
    let (mut sig, mut err, mut worst) = (0f64, 0f64, 0);
    for (&x, &y) in ours.iter().zip(&want) {
        let e = (x as i32 - y as i32).abs();
        sig += (y as f64).powi(2);
        err += (e as f64).powi(2);
        worst = worst.max(e);
    }
    let snr = 10.0 * (sig / err.max(1.0)).log10();
    assert!(snr >= 55.0 && worst <= 16, "{snr:.1} dB, worst {worst}");
    eprintln!("{} samples, {snr:.1} dB, worst {worst}", want.len());
}

#[test]
fn stereo_audio_matches_ffmpeg() {
    let s = data("testsrc.mpg");
    let es: Vec<u8> = ps::packets(&s).into_iter().filter(|p| ps::is_audio(p.id)).flat_map(|p| p.data).collect();
    audio_check(&es, &data("testsrc.s16"), 2);
}

#[test]
fn mono_audio_matches_ffmpeg() {
    audio_check(&data("mono32k.mp2"), &data("mono32k.s16"), 1);
}

#[test]
fn probe_reads_the_stream() {
    let s = data("testsrc.mpg");
    let i = mpeg1::probe(&s, &s, true).unwrap();
    assert_eq!((i.width, i.height, i.rate, i.audio_rate), (64, 48, 25.0, 44100));
    assert!((i.duration - 1.2).abs() < 0.05, "{}", i.duration);
}
