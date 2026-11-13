//! Differential tests against FFmpeg itself: streams its encoders make, decoded by its
//! decoders (the ones ported here) and by ours, must give the same samples.

use std::path::{Path, PathBuf};
use std::process::Command;

pub fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

pub struct Tmp(pub PathBuf);
impl Tmp {
    pub fn new(tag: &str) -> Tmp {
        let d = std::env::temp_dir().join(format!("wmcodec-{tag}-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(&d).unwrap();
        Tmp(d)
    }
    pub fn path(&self, n: &str) -> PathBuf {
        self.0.join(n)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn run(args: &[&str]) {
    let o = Command::new("ffmpeg").args(["-y", "-v", "error"]).args(args).output().unwrap();
    assert!(o.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn f32s(p: &Path) -> Vec<f32> {
    std::fs::read(p).unwrap().chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
}

/// Ours: the planar output of every packet, interleaved, the delay taken off.
fn ours(file: &Path) -> (Vec<f32>, usize) {
    let mut r = asf::Reader::new(std::fs::read(file).unwrap()).unwrap();
    let s = r.header.streams.iter().find(|s| s.kind == asf::Kind::Audio).unwrap().clone();
    let w = &s.format;
    let rd16 = |o: usize| u16::from_le_bytes([w[o], w[o + 1]]);
    let rd32 = |o: usize| u32::from_le_bytes(w[o..o + 4].try_into().unwrap());
    let cb = rd16(16) as usize;
    let mut d = crate::wma::Decoder::new(rd16(0), rd16(2), rd32(4), rd32(8) * 8, rd16(12), &w[18..18 + cb]).unwrap();
    let align = rd16(12) as usize;
    let ch = d.channels;
    let mut out = Vec::new();
    let mut skip = d.frame_len * 2;
    while let Some(x) = r.next_sample() {
        if x.stream != s.number {
            continue;
        }
        for p in x.data.chunks(align) {
            if p.len() < align {
                continue;
            }
            let planes = match d.decode_raw(p) { Ok(p) => p, Err(e) => { if std::env::var("WMA_DEBUG").is_ok() { eprintln!("decode: {e:?}"); } continue } };
            let n = planes[0].len();
            for i in 0..n {
                if skip > 0 {
                    skip -= 1;
                    continue;
                }
                for c in &planes {
                    out.push(c[i]);
                }
            }
        }
    }
    (out, ch)
}

fn compare(codec: &str, rate: u32, channels: u32, bitrate: &str) {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let t = Tmp::new(&format!("{codec}{rate}{channels}{bitrate}"));
    let src = format!("sine=frequency=440:sample_rate={rate}:duration=2,aformat=channel_layouts={}", if channels == 2 { "stereo" } else { "mono" });
    let wma = t.path("a.wma");
    // (a tone plus noise: the noise coding and the high bands get used too)
    let input = format!("{src}[a];anoisesrc=d=2:c=pink:r={rate}:a=0.1,aformat=channel_layouts={}[b];[a][b]amix=inputs=2", if channels == 2 { "stereo" } else { "mono" });
    run(&["-f", "lavfi", "-i", &input, "-c:a", codec, "-b:a", bitrate, wma.to_str().unwrap()]);
    let raw = t.path("a.f32");
    run(&["-i", wma.to_str().unwrap(), "-f", "f32le", "-c:a", "pcm_f32le", raw.to_str().unwrap()]);
    let want = f32s(&raw);
    let (got, ch) = ours(&wma);
    assert_eq!(ch as u32, channels);
    // (FFmpeg drops the last superframe's samples past the stream's end: compare what both have)
    let n = want.len().min(got.len());
    assert!(n > rate as usize, "{codec} {rate} {channels}: only {n} samples (ours {}, ffmpeg {})", got.len(), want.len());
    assert!(got.len() + 4096 * channels as usize >= want.len(), "ours {} ffmpeg {}", got.len(), want.len());
    let err = got[..n].iter().zip(&want[..n]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    assert!(err < 1e-4, "{codec} {rate} Hz {channels} ch {bitrate}: max error {err}");
}

#[test]
fn wmav2_44100_stereo() {
    compare("wmav2", 44100, 2, "128k");
}
#[test]
fn wmav2_44100_stereo_low() {
    compare("wmav2", 44100, 2, "48k");
}
#[test]
fn wmav2_22050_mono() {
    compare("wmav2", 22050, 1, "32k");
}
#[test]
fn wmav2_32000_stereo() {
    compare("wmav2", 32000, 2, "64k");
}
#[test]
fn wmav2_16000_mono() {
    compare("wmav2", 16000, 1, "24k");
}
#[test]
fn wmav2_8000_mono() {
    compare("wmav2", 8000, 1, "24k");
}
#[test]
fn wmav1_44100_stereo() {
    compare("wmav1", 44100, 2, "128k");
}
#[test]
fn wmav1_22050_stereo() {
    compare("wmav1", 22050, 2, "48k");
}
#[test]
fn wmav2_48000_stereo_high() {
    compare("wmav2", 48000, 2, "192k");
}

/// A file of one's own (WMA_FILE=path, e.g. a game's .wma / .wmv): ours against FFmpeg.
#[test]
fn file_from_env() {
    let Ok(f) = std::env::var("WMA_FILE") else { return };
    let t = Tmp::new("env");
    let raw = t.path("a.f32");
    run(&["-i", &f, "-map", "0:a:0", "-f", "f32le", "-c:a", "pcm_f32le", raw.to_str().unwrap()]);
    let want = f32s(&raw);
    let (got, _) = ours(Path::new(&f));
    let n = want.len().min(got.len());
    let err = got[..n].iter().zip(&want[..n]).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    eprintln!("{f}: ours {} ffmpeg {} max error {err}", got.len(), want.len());
    assert!(err < 1e-4 && n > 0);
}
