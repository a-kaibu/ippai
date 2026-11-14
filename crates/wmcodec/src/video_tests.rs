//! Differential tests of the MS-MPEG4 / WMV 1 / WMV 2 decoders against FFmpeg's own (its C
//! code, `-cpuflags 0`): streams its encoders make, and real files (WMV_FILE, the WMV3 ones
//! too), must decode to the same pictures, bit for bit.

use crate::msmpeg4::{Decoder, Picture, Version};
use crate::tests::{have_ffmpeg, run, Tmp};
use std::path::Path;

/// One picture of ours: yuv420p of the visible size, and whether it is a key picture and one
/// FFmpeg conceals part of (so it, and the pictures up to the next key one, are FFmpeg's guesses).
pub struct Out {
    pub yuv: Vec<u8>,
    pub key: bool,
    pub conceal: bool,
}

enum D {
    M(Decoder),
    V(crate::vc1::Decoder),
}

impl D {
    fn decode(&mut self, buf: &[u8], out: &mut Vec<Out>, last: &mut Option<Picture>) {
        let got = match self {
            D::M(d) => d.decode(buf),
            D::V(d) => d.decode(buf),
        };
        match got {
            Ok(Some(p)) => *last = Some(p),
            Ok(None) => {}
            // (not ported: the rest of the stream can't be compared)
            Err(e) if format!("{e:?}").contains("IntraX8") => panic!("frame {}: {e:?}", out.len()),
            Err(e) => eprintln!("frame {}: {e:?}", out.len()),
        }
        let conceal = matches!(self, D::V(d) if d.ffmpeg_conceals);
        if matches!(self, D::V(d) if d.ffmpeg_drops) {
            return;
        }
        if let Some(p) = last {
            out.push(Out { yuv: yuv(p), key: p.key, conceal });
        }
    }

    /// The end of the stream: the picture a decoder with B pictures still holds.
    fn flush(&mut self, out: &mut Vec<Out>) {
        if let D::V(d) = self {
            if let Some(p) = d.flush() {
                out.push(Out { yuv: yuv(&p), key: p.key, conceal: false });
            }
        }
    }
}

/// Ours: every picture of an RCV file (SMPTE VC-1 test vectors of Simple/Main profile, the
/// container of FFmpeg's vc1test demuxer: 24-bit count, 0xC5, sequence header, sizes, frames).
fn ours_rcv(file: &Path) -> (Vec<Out>, usize, usize) {
    let b = std::fs::read(file).unwrap();
    let rd32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    assert_eq!(b[3], 0xc5);
    let ext = rd32(4);
    let extra = b[8..12].to_vec();
    let mut o = 8 + ext;
    let (h, w) = (rd32(o), rd32(o + 4));
    o += 8;
    assert_eq!(rd32(o), 0xc);
    o += 16;
    let mut d = D::V(crate::vc1::Decoder::new(w, h, &extra).unwrap());
    let (mut out, mut last) = (Vec::new(), None);
    while o + 8 <= b.len() {
        let n = rd32(o) & 0xff_ffff;
        o += 8;
        d.decode(&b[o..(o + n).min(b.len())], &mut out, &mut last);
        o += n;
    }
    d.flush(&mut out);
    (out, w, h)
}

/// Ours: every picture of the file's video stream.
pub fn ours(file: &Path) -> (Vec<Out>, usize, usize) {
    if file.extension().is_some_and(|e| e.eq_ignore_ascii_case("rcv")) {
        return ours_rcv(file);
    }
    let mut r = asf::Reader::new(std::fs::read(file).unwrap()).unwrap();
    let s = r.header.streams.iter().find(|s| matches!(s.kind, asf::Kind::Video { .. })).unwrap().clone();
    let f = &s.format;
    let rd32 = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap());
    let (w, h) = (rd32(4) as usize, rd32(8) as usize);
    let ver = match &f[16..20] {
        b"WMV1" => Some(Version::Wmv1),
        b"WMV2" => Some(Version::Wmv2),
        b"MP43" | b"DIV3" => Some(Version::V3),
        b"WMV3" => None,
        x => panic!("codec {x:?}"),
    };
    let mut d = match ver {
        Some(v) => D::M(Decoder::new(v, w, h, &f[40..]).unwrap()),
        None => D::V(crate::vc1::Decoder::new(w, h, &f[40..]).unwrap()),
    };
    let (mut out, mut last) = (Vec::new(), None);
    while let Some(x) = r.next_sample() {
        if x.stream == s.number {
            d.decode(&x.data, &mut out, &mut last);
        }
    }
    d.flush(&mut out);
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

/// Ours and FFmpeg's pictures are the same, bit for bit, but for the ones from a picture FFmpeg
/// conceals to the next key picture. Returns (pictures equal, pictures left out).
fn check(file: &Path, t: &Tmp, what: &str) -> (usize, usize) {
    let want = theirs(file, t);
    let (got, w, h) = ours(file);
    let size = w * h + 2 * w.div_ceil(2) * h.div_ceil(2);
    assert_eq!(want.len() % size, 0);
    let n = want.len() / size;
    assert!(n > 0 && got.len() == n, "{what}: ours {} pictures, ffmpeg {n}", got.len());
    let (mut same, mut out, mut guessed) = (0, 0, false);
    for (i, g) in got.iter().enumerate() {
        guessed = (guessed && !g.key) || g.conceal;
        if guessed {
            out += 1;
            continue;
        }
        let (g, wnt) = (&g.yuv, &want[i * size..(i + 1) * size]);
        if g[..] != wnt[..] {
            let k = g.iter().zip(wnt).position(|(a, b)| a != b).unwrap();
            let bad = g.iter().zip(wnt).filter(|(a, b)| a != b).count();
            // the macroblocks whose luma differs, in raster order
            let mut mbs = Vec::new();
            for my in 0..h.div_ceil(16) {
                for mx in 0..w.div_ceil(16) {
                    let d = (0..16).flat_map(|y| (0..16).map(move |x| (mx * 16 + x, my * 16 + y))).filter(|&(x, y)| x < w && y < h && g[y * w + x] != wnt[y * w + x]).count();
                    if d > 0 && mbs.len() < 12 {
                        mbs.push((mx, my, d));
                    }
                }
            }
            panic!("{what}: picture {i} differs at byte {k} (x {} y {}), {bad} bytes; MBs (x, y, pixels) {mbs:?}", k % w, k / w);
        }
        same += 1;
    }
    eprintln!("{what}: {same} pictures equal, {out} after FFmpeg's concealment left out");
    (same, out)
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

/// Every .wmv / .rcv file of WMV_DIR (samples.sh fetches FFmpeg's WMV 9 ones): ours against
/// FFmpeg. A stream of what is not ported (IntraX8 pictures) is told of, not failed.
#[test]
fn files_from_dir() {
    let Ok(d) = std::env::var("WMV_DIR") else { return };
    let mut files: Vec<_> = std::fs::read_dir(&d).unwrap().filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wmv") || e.eq_ignore_ascii_case("rcv"))).collect();
    files.sort();
    assert!(!files.is_empty(), "no files in {d}");
    let t = Tmp::new("dirv");
    let (mut all, mut skipped) = (0, Vec::new());
    for f in &files {
        let name = f.display().to_string();
        match std::panic::catch_unwind(|| check(f, &t, &name)) {
            Ok((same, _)) => all += same,
            Err(e) => {
                let msg = e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default();
                assert!(msg.contains("IntraX8"), "{name}: {msg}");
                skipped.push(name);
            }
        }
    }
    eprintln!("{} files, {all} pictures equal; with IntraX8 pictures (not ported): {skipped:?}", files.len());
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
