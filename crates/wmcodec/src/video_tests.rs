//! Differential tests of the MS-MPEG4 / WMV 1 / WMV 2 decoders against FFmpeg's own (its C
//! code, `-cpuflags 0`): streams its encoders make must decode to the same pictures, bit for bit.

use crate::msmpeg4::{Decoder, Picture, Version};
use crate::tests::{have_ffmpeg, run, Tmp};
use std::path::Path;

/// Ours: every picture of the file's video stream, as yuv420p of its visible size.
pub fn ours(file: &Path) -> (Vec<Vec<u8>>, usize, usize) {
    let mut r = asf::Reader::new(std::fs::read(file).unwrap()).unwrap();
    let s = r.header.streams.iter().find(|s| matches!(s.kind, asf::Kind::Video { .. })).unwrap().clone();
    let f = &s.format;
    let rd32 = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap());
    let (w, h) = (rd32(4) as usize, rd32(8) as usize);
    let ver = match &f[16..20] {
        b"WMV1" => Version::Wmv1,
        b"WMV2" => Version::Wmv2,
        b"MP43" | b"DIV3" => Version::V3,
        x => panic!("codec {x:?}"),
    };
    let mut d = Decoder::new(ver, w, h, &f[40..]).unwrap();
    let mut out = Vec::new();
    let mut last: Option<Picture> = None;
    while let Some(x) = r.next_sample() {
        if x.stream != s.number {
            continue;
        }
        match d.decode(&x.data) {
            Ok(Some(p)) => last = Some(p),
            Ok(None) => {}
            Err(e) => {
                eprintln!("frame {}: {e:?}", out.len());
                continue;
            }
        }
        if let Some(p) = &last {
            out.push(yuv(p));
        }
    }
    (out, w, h)
}

fn yuv(p: &Picture) -> Vec<u8> {
    let mut v = Vec::new();
    for (k, pl) in p.planes.iter().enumerate() {
        let (w, h, s) = if k == 0 { (p.width, p.height, p.stride[0]) } else { (p.width.div_ceil(2), p.height.div_ceil(2), p.stride[1]) };
        for y in 0..h {
            v.extend_from_slice(&pl[y * s..y * s + w]);
        }
    }
    v
}

/// FFmpeg's pictures of the file.
fn theirs(file: &Path, t: &Tmp) -> Vec<u8> {
    let raw = t.path("v.yuv");
    run(&["-cpuflags", "0", "-i", file.to_str().unwrap(), "-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-fps_mode", "passthrough", raw.to_str().unwrap()]);
    std::fs::read(raw).unwrap()
}

fn check(file: &Path, t: &Tmp, what: &str) {
    let want = theirs(file, t);
    let (got, w, h) = ours(file);
    let size = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
    assert_eq!(want.len() % size, 0);
    let n = want.len() / size;
    assert!(n > 0 && got.len() == n, "{what}: ours {} pictures, ffmpeg {n}", got.len());
    for (i, g) in got.iter().enumerate() {
        let wnt = &want[i * size..(i + 1) * size];
        if g[..] != wnt[..] {
            let k = g.iter().zip(wnt).position(|(a, b)| a != b).unwrap();
            let bad = g.iter().zip(wnt).filter(|(a, b)| a != b).count();
            panic!("{what}: picture {i} differs at byte {k} (x {} y {}), {bad} bytes", k % w, k / w);
        }
    }
    eprintln!("{what}: {n} pictures equal");
}

fn compare(codec: &str, w: u32, h: u32, args: &[&str]) {
    if !have_ffmpeg() {
        eprintln!("no ffmpeg: skipped");
        return;
    }
    let t = Tmp::new(&format!("{codec}{w}x{h}{}", args.join("").replace(['-', ':', '='], "")));
    let f = t.path("v.wmv");
    let src = format!("testsrc2=size={w}x{h}:rate=25:duration=3,noise=alls=12:allf=t");
    let mut a = vec!["-f", "lavfi", "-i", &src, "-c:v", codec];
    a.extend_from_slice(args);
    a.push(f.to_str().unwrap());
    run(&a);
    check(&f, &t, &format!("{codec} {w}x{h} {args:?}"));
}

#[test]
fn wmv2_qcif() {
    compare("wmv2", 176, 144, &["-b:v", "200k"]);
}
#[test]
fn wmv2_odd_size_high_q() {
    compare("wmv2", 330, 250, &["-q:v", "2", "-g", "10"]);
}
#[test]
fn wmv2_low_rate() {
    compare("wmv2", 320, 240, &["-b:v", "60k", "-g", "50"]);
}
#[test]
fn wmv1_qcif() {
    compare("wmv1", 176, 144, &["-b:v", "200k"]);
}
#[test]
fn wmv1_vga() {
    compare("wmv1", 640, 480, &["-b:v", "800k", "-g", "12"]);
}
#[test]
fn wmv1_odd_size_high_q() {
    compare("wmv1", 330, 250, &["-q:v", "3"]);
}

/// A file of one's own (WMV_FILE=path, e.g. a game's .wmv): ours against FFmpeg.
#[test]
fn file_from_env() {
    let Ok(f) = std::env::var("WMV_FILE") else { return };
    let t = Tmp::new("envv");
    check(Path::new(&f), &t, &f);
}
#[test]
fn msmpeg4v3_qcif() {
    compare("msmpeg4", 176, 144, &["-b:v", "200k"]);
}
#[test]
fn msmpeg4v3_cif_high_q() {
    compare("msmpeg4", 352, 288, &["-q:v", "2", "-g", "8"]);
}

mod robust {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #![proptest_config(ProptestConfig { cases: 400, ..ProptestConfig::default() })]
        /// Any bytes, as any picture of any version and size: an error or a picture, never a
        /// panic, and pictures keep their size.
        #[test]
        fn any_bytes(ver in 0u8..3, w in 1usize..80, h in 1usize..80, extra in any::<[u8; 4]>(), pics in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..600), 1..6)) {
            let ver = [Version::V3, Version::Wmv1, Version::Wmv2][ver as usize];
            let Ok(mut d) = Decoder::new(ver, w, h, &extra) else { return Ok(()) };
            for p in &pics {
                if let Ok(Some(p)) = d.decode(p) {
                    prop_assert_eq!((p.width, p.height), (w, h));
                    prop_assert_eq!(p.planes[0].len(), w.div_ceil(16) * 16 * h.div_ceil(16) * 16);
                }
            }
        }

        /// Real pictures with bits flipped: never a panic.
        #[test]
        fn damaged_pictures(flips in prop::collection::vec((any::<usize>(), 0u8..8), 1..12), skip in 0usize..3) {
            let (ver, w, h, extra, pics) = sample();
            let mut d = Decoder::new(ver, w, h, &extra).unwrap();
            for (i, p) in pics.iter().enumerate() {
                let mut p = p.clone();
                if i >= skip && !p.is_empty() {
                    for &(at, bit) in &flips {
                        let k = at % p.len();
                        p[k] ^= 1 << bit;
                    }
                }
                let _ = d.decode(&p);
            }
        }
    }

    /// A WMV2 stream made once for the tests (QCIF, I and P pictures), or a fixed one.
    fn sample() -> (Version, usize, usize, Vec<u8>, Vec<Vec<u8>>) {
        static S: std::sync::OnceLock<(Version, usize, usize, Vec<u8>, Vec<Vec<u8>>)> = std::sync::OnceLock::new();
        S.get_or_init(|| {
            if !have_ffmpeg() {
                return (Version::Wmv2, 32, 32, vec![0x2c, 0x83, 0x60, 0x02], vec![vec![0x55; 64], vec![0xaa; 64]]);
            }
            let t = Tmp::new("sample");
            let f = t.path("s.wmv");
            run(&["-f", "lavfi", "-i", "testsrc2=size=96x64:rate=25:duration=1", "-c:v", "wmv2", "-g", "6", f.to_str().unwrap()]);
            let mut r = asf::Reader::new(std::fs::read(&f).unwrap()).unwrap();
            let s = r.header.streams.iter().find(|s| matches!(s.kind, asf::Kind::Video { .. })).unwrap().clone();
            let mut pics = Vec::new();
            while let Some(x) = r.next_sample() {
                if x.stream == s.number {
                    pics.push(x.data);
                }
            }
            (Version::Wmv2, 96, 64, s.format[40..].to_vec(), pics)
        })
        .clone()
    }
}
