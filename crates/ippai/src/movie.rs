//! Siglus's movies (`.omv`: a header, then Ogg Theora) made playable by the browser's hardware
//! decoder: `ippai pack` writes `<name>.omv.vnv` next to each one (VP9, see below), which sake
//! decodes with WebCodecs in place of the game's own Theora decoder (crates/sake/src/movie.rs).
//!
//! An `.omv` whose Theora picture is taller than the movie (1920x1440 for 1920x1080) carries an
//! alpha channel below it: the movie's rows of alpha are split in three, stacked in the Y, U and
//! V planes (4:4:4) of rows `h..h + (h + 2) / 3`. That does not survive 4:2:0, so it becomes a
//! second track of its own (the alpha in the luma).
//!
//! VP9 rather than H.264: the same picture (SSIM 0.986 on the aurora) in 18 MB instead of 22.6,
//! as fast to decode in software, and decoded in hardware by phones since about 2015.
//!
//! The `.vnv` format is crates/kura/src/vnv.rs (included here). A track's frame k is the movie's
//! frame k (gaps in the Theora stream are repeated frames).
//!
//! Other games' movies (mpg, wmv, avi, mp4 ... next to the game, played through DirectShow or
//! Media Foundation: crates/sake/src/dshow.rs) get a `<name>.vnv` of the same kind (`movie_vnv_for`),
//! with the trailer: frame rate, length and the sound as Ogg Vorbis (sake decodes it,
//! crates/vorbis).

use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "../../kura/src/vnv.rs"]
#[allow(dead_code)]
pub mod vnv;

/// Whether `ffmpeg` (with libvpx) can be run.
pub fn available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// The `.vnv` for the `.omv` at `omv`, from the store's cache (`movies/`, by content) or made.
pub fn vnv_for(store: &Path, omv: &Path) -> Result<std::path::PathBuf> {
    let h = hash_file(omv)?;
    let dir = store.join("movies");
    let path = dir.join(format!("{h}.vp9.vnv"));
    if !path.exists() {
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".{h}.vp9.part"));
        transcode(omv, &tmp)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

/// The store's name of a file's content (read through, not held: movies are big).
fn hash_file(p: &Path) -> Result<String> {
    let mut h = blake3::Hasher::new();
    h.update_reader(fs::File::open(p)?)?;
    Ok(h.finalize().to_hex()[..32].to_string())
}

/// Writes `out` (.vnv) for the `.omv` at `omv`.
pub fn transcode(omv: &Path, out: &Path) -> Result<()> {
    // (the header is small: the Ogg stream starts in the first KiB)
    let mut f = fs::File::open(omv)?;
    let mut data = Vec::new();
    (&mut f).take(64 << 10).read_to_end(&mut data)?;
    let ogg = data.windows(4).position(|w| w == b"OggS").context("no Ogg stream")?;
    if data.len() < 0x40 {
        bail!("not an .omv");
    }
    let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let (w, h, frame_us) = (u32_at(0x2c), u32_at(0x30), u32_at(0x3c));
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        bail!("unexpected header ({w}x{h})");
    }
    let tmp = out.with_extension("vnv-tmp");
    fs::create_dir_all(&tmp)?;
    let res = (|| {
        let src = tmp.join("in.ogv");
        f.seek(SeekFrom::Start(ogg as u64))?;
        std::io::copy(&mut f, &mut BufWriter::new(fs::File::create(&src)?))?;
        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=height", "-of", "csv=p=0"])
            .arg(&src)
            .output()?;
        let ph: u32 = String::from_utf8_lossy(&probe.stdout).trim().parse().context("ffprobe")?;
        // (4:2:0 wants even sizes: padded, the reader takes w x h)
        let pad = "pad=ceil(iw/2)*2:ceil(ih/2)*2";
        let rate = format!("1000000/{}", if frame_us == 0 { 33333 } else { frame_us });
        let mut tracks = vec![encode(&src, &tmp.join("c.ivf"), &format!("crop={w}:{h}:0:0,{pad},format=yuv420p"), &rate)?];
        if ph > h {
            let a = h.div_ceil(3);
            let last = h - 2 * a;
            let f = format!(
                "crop={w}:{a}:0:{h},extractplanes=y+u+v[y][u][v];[y]null[y1];[u]null[u1];[v]crop={w}:{last}:0:0[v1];\
                 [y1][u1][v1]vstack=3,format=gray,{pad},format=yuv420p"
            );
            tracks.push(encode(&src, &tmp.join("a.ivf"), &f, &rate)?);
        }
        write_vnv(out, w, h, &tracks, None)
    })();
    let _ = fs::remove_dir_all(&tmp);
    res
}

/// Encodes the frames `filter` makes of `src` (one a movie frame, gaps repeated) into the IVF
/// file `out`; returns it as a track.
fn encode(src: &Path, out: &Path, filter: &str, rate: &str) -> Result<Src> {
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-filter_complex", filter, "-fps_mode", "cfr", "-r", rate])
        // no alt-ref frames or lag (one packet = one shown frame, in movie order), a key frame
        // at least a second (seeking, loops)
        .args(["-c:v", "libvpx-vp9", "-crf", "24", "-b:v", "0", "-g", "30", "-row-mt", "1"])
        .args(["-deadline", "good", "-cpu-used", "3", "-lag-in-frames", "0", "-auto-alt-ref", "0"])
        .args(["-f", "ivf"])
        .arg(out)
        .status()?;
    if !st.success() {
        bail!("ffmpeg failed ({filter})");
    }
    let (w, h, frames) = ivf_frames(out)?;
    // profile 0, level (3.1 up to 720p, 4.1 up to 1080p, 5.1 above), 8 bits
    let level = if w * h <= 1280 * 720 { 31 } else if w * h <= 2048 * 1088 { 41 } else { 51 };
    Ok(Src { codec: format!("vp09.00.{level}.08"), file: out.to_path_buf(), frames })
}

/// A track for `write_vnv`: its codec, and its frames in `file` (offset, size, key frame).
pub struct Src {
    codec: String,
    file: PathBuf,
    frames: Vec<(u64, u32, bool)>,
}

/// An IVF file's picture size and frames (offset, size, key frame), its headers read alone.
fn ivf_frames(p: &Path) -> Result<(u32, u32, Vec<(u64, u32, bool)>)> {
    let len = fs::metadata(p)?.len();
    let mut r = BufReader::new(fs::File::open(p)?);
    let mut head = [0u8; 32];
    r.read_exact(&mut head).context("not IVF")?;
    if &head[..4] != b"DKIF" {
        bail!("not IVF");
    }
    let (w, h) = (u16::from_le_bytes([head[12], head[13]]) as u32, u16::from_le_bytes([head[14], head[15]]) as u32);
    let mut at = u16::from_le_bytes([head[6], head[7]]) as u64;
    r.seek(SeekFrom::Start(at))?;
    let mut frames = Vec::new();
    while at + 12 <= len {
        let mut fh = [0u8; 13];
        let size = {
            r.read_exact(&mut fh[..12])?;
            u32::from_le_bytes(fh[..4].try_into().unwrap())
        };
        if at + 12 + size as u64 > len {
            bail!("IVF cut short");
        }
        // the uncompressed header (profile 0), from the top bit: frame_marker(2) profile(2)
        // show_existing_frame(1) frame_type(1, 0 = key)
        let key = size > 0 && {
            r.read_exact(&mut fh[12..])?;
            r.seek_relative(size as i64 - 1)?;
            fh[12] & 0x0c == 0
        };
        frames.push((at + 12, size, key));
        at += 12 + size as u64;
    }
    Ok((w, h, frames))
}

/// Writes the `.vnv` of `tracks` at `out` (with the trailer when there is one: frame rate,
/// length, the file with the sound), copying the frames from their files: no movie is held in
/// memory whole.
fn write_vnv(out: &Path, w: u32, h: u32, tracks: &[Src], trailer: Option<((u32, u32), u64, Option<&Path>)>) -> Result<()> {
    let mut o = BufWriter::new(fs::File::create(out)?);
    let sizes: Vec<Vec<(u32, bool)>> = tracks.iter().map(|t| t.frames.iter().map(|f| (f.1, f.2)).collect()).collect();
    let index: Vec<(&str, &[(u32, bool)])> = tracks.iter().zip(&sizes).map(|(t, s)| (t.codec.as_str(), &s[..])).collect();
    let end = vnv::write_head(&mut o, w, h, &index)?;
    let n = sizes.iter().map(Vec::len).min().unwrap_or(0);
    for t in tracks {
        let mut r = BufReader::new(fs::File::open(&t.file)?);
        let mut pos = 0u64;
        for &(off, size, _) in &t.frames[..n] {
            r.seek_relative(off as i64 - pos as i64)?;
            let copied = std::io::copy(&mut (&mut r).take(size as u64), &mut o)?;
            if copied != size as u64 {
                bail!("{} cut short", t.file.display());
            }
            pos = off + size as u64;
        }
    }
    if let Some((rate, duration, audio)) = trailer {
        let len = match audio {
            Some(a) => std::io::copy(&mut fs::File::open(a)?, &mut o)?,
            None => 0,
        };
        vnv::write_tail(&mut o, rate, duration, end, len)?;
    }
    o.into_inner().map_err(|e| e.into_error())?.sync_all()?;
    Ok(())
}

/// Movie files games play through DirectShow / Media Foundation (by extension; ffprobe decides).
pub fn is_movie(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    let ext = l.rsplit_once('.').map_or("", |e| e.1);
    matches!(ext, "mpg" | "mpeg" | "mpe" | "m1v" | "m2v" | "wmv" | "asf" | "avi" | "mp4" | "m4v" | "mov" | "ogv" | "webm" | "mkv" | "flv")
}

/// Whether a file's first bytes are a movie container's (ASF, AVI, MP4/MOV, MPEG program or
/// video stream, Matroska/WebM, Ogg Theora, FLV): games name them anything (Artemis: `.dat`).
pub fn sniff(head: &[u8]) -> bool {
    let at = |o: usize, m: &[u8]| head.get(o..o + m.len()) == Some(m);
    at(0, &[0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11])
        || (at(0, b"RIFF") && at(8, b"AVI "))
        || at(4, b"ftyp")
        || at(0, &[0, 0, 1, 0xba])
        || at(0, &[0, 0, 1, 0xb3])
        || at(0, &[0x1a, 0x45, 0xdf, 0xa3])
        || (at(0, b"OggS") && at(28, b"\x80theora"))
        || at(0, b"FLV\x01")
}

/// How a movie's frames are stored.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Codec {
    /// VP9, decoded by the page (WebCodecs)
    Vp9,
    /// uncompressed I420, read by sake itself (tests under node)
    #[cfg_attr(not(test), allow(dead_code))]
    Raw,
}

/// The `.vnv` for the movie at `src`, from the store's cache or made (VP9).
pub fn movie_vnv_for(store: &Path, src: &Path) -> Result<std::path::PathBuf> {
    let h = hash_file(src)?;
    let dir = store.join("movies");
    let path = dir.join(format!("{h}.movie1.vnv"));
    if !path.exists() {
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".{h}.movie1.part"));
        transcode_movie(src, &tmp, Codec::Vp9)?;
        fs::rename(&tmp, &path)?;
    }
    Ok(path)
}

/// What ffprobe says of a movie: picture size, frame rate, whether it has sound, length (s).
#[derive(Debug, PartialEq)]
pub struct Probe {
    pub w: u32,
    pub h: u32,
    pub rate: (u32, u32),
    pub audio: bool,
    pub seconds: f64,
}

/// A rate "num/den" as ffprobe prints it, when it is a frame rate (0 < fps <= 240).
fn parse_rate(s: &str) -> Option<(u32, u32)> {
    let (n, d) = s.trim().split_once('/')?;
    let (n, d): (u32, u32) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0 && d > 0 && n as u64 <= 240 * d as u64).then_some((n, d))
}

/// ffprobe's `-show_streams -show_format` (flat, `-of default`) output read.
pub fn parse_probe(out: &str) -> Option<Probe> {
    #[derive(Default)]
    struct Stream {
        kind: String,
        w: u32,
        h: u32,
        avg: Option<(u32, u32)>,
        r: Option<(u32, u32)>,
    }
    let (mut video, mut audio, mut seconds, mut cur) = (None::<Stream>, false, 0.0, Stream::default());
    for line in out.lines() {
        let line = line.trim();
        if line == "[/STREAM]" {
            let s = std::mem::take(&mut cur);
            match s.kind.as_str() {
                "video" if video.is_none() && s.w > 0 && s.h > 0 => video = Some(s),
                "audio" => audio = true,
                _ => {}
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        match k {
            "codec_type" => cur.kind = v.to_string(),
            "width" => cur.w = v.parse().unwrap_or(0),
            "height" => cur.h = v.parse().unwrap_or(0),
            "avg_frame_rate" => cur.avg = parse_rate(v),
            "r_frame_rate" => cur.r = parse_rate(v),
            "duration" if seconds == 0.0 => seconds = v.parse().unwrap_or(0.0),
            _ => {}
        }
    }
    let v = video?;
    if v.w > 8192 || v.h > 8192 {
        return None;
    }
    // (avg is the movie's real rate; r_frame_rate can be a timebase's)
    Some(Probe { w: v.w, h: v.h, rate: v.avg.or(v.r).unwrap_or((30, 1)), audio, seconds })
}

/// Writes `out` (.vnv with the trailer) for the movie at `src`.
pub fn transcode_movie(src: &Path, out: &Path, codec: Codec) -> Result<()> {
    let o = Command::new("ffprobe").args(["-v", "error", "-show_streams", "-show_format"]).arg(src).output()?;
    let p = parse_probe(&String::from_utf8_lossy(&o.stdout)).context("no video stream")?;
    let tmp = out.with_extension("vnv-tmp");
    fs::create_dir_all(&tmp)?;
    let res = (|| {
        let rate = format!("{}/{}", p.rate.0, p.rate.1);
        // one colour model for every movie: BT.601, video range, 4:2:0 at even sizes (sake turns
        // it into RGB as Windows' decoders do, kura::video::Out::Rgb601)
        let filter = "scale=out_color_matrix=bt601:out_range=tv,pad=ceil(iw/2)*2:ceil(ih/2)*2,format=yuv420p";
        let track = match codec {
            Codec::Vp9 => encode(src, &tmp.join("c.ivf"), filter, &rate)?,
            Codec::Raw => {
                let f = tmp.join("c.yuv");
                let st = Command::new("ffmpeg")
                    .args(["-v", "error", "-y", "-i"])
                    .arg(src)
                    .args(["-vf", filter, "-fps_mode", "cfr", "-r", &rate, "-f", "rawvideo", "-pix_fmt", "yuv420p"])
                    .arg(&f)
                    .status()?;
                if !st.success() {
                    bail!("ffmpeg failed (raw)");
                }
                let n = vnv::raw_size(p.w, p.h) as u64;
                let count = fs::metadata(&f)?.len() / n;
                Src { codec: "raw-i420".into(), file: f, frames: (0..count).map(|k| (k * n, n as u32, true)).collect() }
            }
        };
        let audio = if p.audio {
            let a = tmp.join("a.ogg");
            let st = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-i"])
                .arg(src)
                .args(["-vn", "-ac", "2", "-ar", "44100", "-c:a", "libvorbis", "-q:a", "5", "-f", "ogg"])
                .arg(&a)
                .status()?;
            if !st.success() {
                bail!("ffmpeg failed (sound)");
            }
            Some(a)
        } else {
            None
        };
        let frames = track.frames.len() as u64;
        // (the container's length; else the frames')
        let from_frames = frames * 10_000_000 * p.rate.1 as u64 / p.rate.0 as u64;
        let duration = if p.seconds > 0.0 { (p.seconds * 1e7).round() as u64 } else { from_frames };
        write_vnv(out, p.w, p.h, &[track], Some((p.rate, duration, audio.as_deref())))
    })();
    let _ = fs::remove_dir_all(&tmp);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn movie_names() {
        for n in ["logo.mpg", "OP.WMV", "a/b/movie.Mp4", "x.avi", "e.webm", "m.m1v"] {
            assert!(is_movie(n), "{n}");
        }
        for n in ["a.omv", "mpg", "x.mpg.vnv", "data.xp3", "b.ogg", "c.wav"] {
            assert!(!is_movie(n), "{n}");
        }
    }

    #[test]
    fn sniffing() {
        let mut ogg = b"OggS".to_vec();
        ogg.resize(28, 0);
        let (mut theora, mut vorbis) = (ogg.clone(), ogg);
        theora.extend(b"\x80theora");
        vorbis.extend(b"\x01vorbis");
        let asf = [0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9];
        for (h, want) in [
            (&asf[..], true),
            (b"RIFF\0\0\0\0AVI LIST", true),
            (b"RIFF\0\0\0\0WAVEfmt ", false),
            (b"\0\0\0\x20ftypisom", true),
            (&[0, 0, 1, 0xba, 0x44], true),
            (&[0, 0, 1, 0xb3, 0x40], true),
            (&[0x1a, 0x45, 0xdf, 0xa3, 1], true),
            (&theora[..], true),
            (&vorbis[..], false),
            (b"FLV\x01\x05", true),
            (b"PNG", false),
            (b"", false),
            (&asf[..4], false),
        ] {
            assert_eq!(sniff(h), want, "{h:?}");
        }
    }

    #[test]
    fn probe_output() {
        let out = "[STREAM]\ncodec_type=audio\nduration=10.5\n[/STREAM]\n[STREAM]\ncodec_type=video\nwidth=1024\nheight=600\nr_frame_rate=60000/1001\navg_frame_rate=0/0\n[/STREAM]\n[FORMAT]\nduration=12.000000\n[/FORMAT]\n";
        let p = parse_probe(out).unwrap();
        // (a stream's duration comes first: kept)
        assert_eq!(p, Probe { w: 1024, h: 600, rate: (60000, 1001), audio: true, seconds: 10.5 });
        assert!(parse_probe("[STREAM]\ncodec_type=audio\n[/STREAM]\n").is_none());
        assert_eq!(parse_rate("1000/1"), None);
        assert_eq!(parse_rate("30000/1001"), Some((30000, 1001)));
        assert_eq!(parse_rate("0/0"), None);
    }

    /// With ffmpeg: a generated movie (test pattern, a tone) comes out as its frames at its
    /// rate, its length and its sound, in both codecs.
    /// A handmade IVF (frames of any sizes, key or not) through `ivf_frames` and `write_vnv`:
    /// each frame's bytes and key flag where the `.vnv` says, the sound after them.
    #[test]
    fn ivf_to_vnv() {
        let dir = std::env::temp_dir().join(format!("ippai-ivf-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut rng = 0x1234_5678u32;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 17;
            rng ^= rng << 5;
            rng
        };
        for case in 0..20 {
            let n = (next() % 40) as usize;
            let frames: Vec<Vec<u8>> = (0..n)
                .map(|k| {
                    let len = (next() % 300) as usize + 1;
                    // (frame_type bit 0x04 clear: a key frame; frame 0 always one)
                    let first = if k == 0 || next() % 4 == 0 { 0x82 } else { 0x86 };
                    std::iter::once(first).chain((1..len).map(|_| next() as u8)).collect()
                })
                .collect();
            let mut ivf = b"DKIF\0\0\x20\0VP90".to_vec();
            ivf.extend(64u16.to_le_bytes());
            ivf.extend(48u16.to_le_bytes());
            ivf.extend([0; 16]);
            for (k, f) in frames.iter().enumerate() {
                ivf.extend((f.len() as u32).to_le_bytes());
                ivf.extend((k as u64).to_le_bytes());
                ivf.extend(f);
            }
            let (ip, sp, op) = (dir.join("c.ivf"), dir.join("a.ogg"), dir.join("o.vnv"));
            fs::write(&ip, &ivf).unwrap();
            let sound: Vec<u8> = (0..next() % 100).map(|_| next() as u8).collect();
            fs::write(&sp, &sound).unwrap();
            let (w, h, idx) = ivf_frames(&ip).unwrap();
            assert_eq!((w, h, idx.len()), (64, 48, n));
            let t = Src { codec: "vp09.00.31.08".into(), file: ip.clone(), frames: idx };
            write_vnv(&op, w, h, &[t], Some(((30, 1), 12345, Some(&sp)))).unwrap();
            let d = fs::read(&op).unwrap();
            let i = vnv::info(&d).unwrap();
            assert_eq!(i.frames() as usize, n, "case {case}");
            for (k, &(off, size, key)) in i.tracks[0].1.iter().enumerate() {
                assert_eq!(&d[off as usize..(off + size) as usize], &frames[k][..], "case {case} frame {k}");
                assert_eq!(key, frames[k][0] == 0x82, "case {case} frame {k}");
            }
            assert_eq!((i.rate, i.duration), (Some((30, 1)), 12345));
            match i.audio {
                Some((o, l)) => assert_eq!(&d[o..o + l], &sound[..]),
                None => assert!(sound.is_empty()),
            }
            // cut short: refused
            fs::write(&ip, &ivf[..ivf.len().saturating_sub(1)]).unwrap();
            if n > 0 {
                assert!(ivf_frames(&ip).is_err(), "case {case}");
            }
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    /// A Siglus `.omv` (header, then Ogg Theora) made here, streamed through `transcode`.
    #[test]
    fn transcodes_an_omv() {
        if !available() {
            eprintln!("no ffmpeg: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ippai-omv-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        // A taller 4:4:4 source carries alpha in the lower Y/U/V planes. Exercise
        // the real ffmpeg path, not just the container writer, for both layouts.
        for source_height in [48, 64] {
            let ogv = dir.join("t.ogv");
            let st = Command::new("ffmpeg")
                .args(["-v", "error", "-y", "-f", "lavfi", "-i", &format!("testsrc=size=64x{source_height}:rate=30:duration=1"), "-pix_fmt", "yuv444p", "-c:v", "libtheora", "-q:v", "7"])
                .arg(&ogv)
                .status()
                .unwrap();
            assert!(st.success());
            let mut omv = vec![0u8; 0x60];
            omv[..4].copy_from_slice(b"TFTD");
            omv[0x2c..0x30].copy_from_slice(&64u32.to_le_bytes());
            omv[0x30..0x34].copy_from_slice(&48u32.to_le_bytes());
            omv[0x3c..0x40].copy_from_slice(&33333u32.to_le_bytes());
            omv.extend(fs::read(&ogv).unwrap());
            let (src, out) = (dir.join("t.omv"), dir.join("t.vnv"));
            fs::write(&src, &omv).unwrap();
            transcode(&src, &out).unwrap();
            let i = vnv::info(&fs::read(&out).unwrap()).unwrap();
            assert_eq!((i.w, i.h, i.tracks.len(), i.rate), (64, 48, if source_height == 48 { 1 } else { 2 }, None));
            assert!((29..=31).contains(&i.frames()), "{} frames", i.frames());
            assert!(i.tracks[0].1[0].2, "a key frame first");
            for (codec, frames) in &i.tracks {
                assert_eq!(codec, "vp09.00.31.08");
                assert_eq!(frames.len(), i.frames() as usize);
                assert!(frames[0].2, "each track starts at a key frame");
            }
        }
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcodes_a_movie() {
        if !available() {
            eprintln!("no ffmpeg: skipped");
            return;
        }
        let dir = std::env::temp_dir().join(format!("ippai-movie-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let src = dir.join("t.mpg");
        let st = Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x48:rate=25:duration=2", "-f", "lavfi", "-i", "sine=frequency=440:duration=2"])
            .args(["-c:v", "mpeg1video", "-q:v", "2", "-c:a", "mp2", "-shortest"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(st.success());
        for codec in [Codec::Raw, Codec::Vp9] {
            let out = dir.join(format!("{codec:?}.vnv"));
            transcode_movie(&src, &out, codec).unwrap();
            let d = fs::read(&out).unwrap();
            let i = vnv::info(&d).unwrap();
            assert_eq!((i.w, i.h, i.rate), (64, 48, Some((25, 1))), "{codec:?}");
            assert!((49..=51).contains(&i.frames()), "{codec:?}: {} frames", i.frames());
            assert!((19_000_000..=21_500_000).contains(&i.duration), "{codec:?}: {}", i.duration);
            let (ao, al) = i.audio.unwrap();
            assert_eq!(&d[ao..ao + 4], b"OggS");
            assert!(al > 1000);
            let c = &i.tracks[0].0;
            assert_eq!(c, if codec == Codec::Raw { "raw-i420" } else { "vp09.00.31.08" });
            if codec == Codec::Raw {
                assert!(i.tracks[0].1.iter().all(|f| f.1 as usize == vnv::raw_size(64, 48)));
            }
        }
        fs::remove_dir_all(&dir).unwrap();
    }
}
