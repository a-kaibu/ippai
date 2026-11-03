//! Siglus's movies (`.omv`: a header, then Ogg Theora) made playable by the browser's hardware
//! decoder: `vnvm pack` writes `<name>.omv.vnv` next to each one (VP9, see below), which sake
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
//! ```text
//! .vnv: "VNV1" w h tracks                      (u32 little endian)
//!       per track: codec_len codec(utf-8) frames, then per frame: offset size key
//!       the frames (VP9, one shown frame each), at their offsets from the start of the file
//! ```
//! A track's frame k is the movie's frame k (gaps in the Theora stream are repeated frames).

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Whether `ffmpeg` (with libvpx) can be run.
pub fn available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// The `.vnv` for the `.omv` at `omv`, from the store's cache (`movies/`, by content) or made.
pub fn vnv_for(store: &Path, omv: &Path) -> Result<std::path::PathBuf> {
    let h = blake3::hash(&fs::read(omv)?).to_hex()[..32].to_string();
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

/// Writes `out` (.vnv) for the `.omv` at `omv`.
pub fn transcode(omv: &Path, out: &Path) -> Result<()> {
    let data = fs::read(omv)?;
    let ogg = data.windows(4).position(|w| w == b"OggS").context("no Ogg stream")?;
    let u32_at = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let (w, h, frame_us) = (u32_at(0x2c), u32_at(0x30), u32_at(0x3c));
    if w == 0 || h == 0 || w > 8192 || h > 8192 {
        bail!("unexpected header ({w}x{h})");
    }
    let tmp = out.with_extension("vnv-tmp");
    fs::create_dir_all(&tmp)?;
    let res = (|| {
        let src = tmp.join("in.ogv");
        fs::write(&src, &data[ogg..])?;
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
        let n = tracks.iter().map(|t| t.1.len()).min().unwrap_or(0);
        let mut head = Vec::new();
        for v in [*b"VNV1", w.to_le_bytes(), h.to_le_bytes(), (tracks.len() as u32).to_le_bytes()] {
            head.extend(v);
        }
        let table_len: usize = tracks.iter().map(|t| 8 + t.0.len() + 12 * n).sum();
        let mut at = head.len() + table_len;
        let mut body = Vec::new();
        for (codec, aus) in &tracks {
            head.extend((codec.len() as u32).to_le_bytes());
            head.extend(codec.as_bytes());
            head.extend((n as u32).to_le_bytes());
            for (au, key) in &aus[..n] {
                for v in [at as u32, au.len() as u32, *key as u32] {
                    head.extend(v.to_le_bytes());
                }
                at += au.len();
            }
        }
        for (_, aus) in &tracks {
            for (au, _) in &aus[..n] {
                body.extend_from_slice(au);
            }
        }
        head.extend(body);
        fs::write(out, head)?;
        Ok(())
    })();
    let _ = fs::remove_dir_all(&tmp);
    res
}

/// Encodes the frames `filter` makes of `src` (one a movie frame, gaps repeated); returns the
/// codec string and the frames (with whether each is a key frame).
fn encode(src: &Path, out: &Path, filter: &str, rate: &str) -> Result<(String, Vec<(Vec<u8>, bool)>)> {
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
    let ivf = fs::read(out)?;
    if ivf.len() < 32 || &ivf[..4] != b"DKIF" {
        bail!("not IVF");
    }
    let (w, h) = (u16::from_le_bytes([ivf[12], ivf[13]]) as u32, u16::from_le_bytes([ivf[14], ivf[15]]) as u32);
    let mut frames = Vec::new();
    let mut i = u16::from_le_bytes([ivf[6], ivf[7]]) as usize;
    while i + 12 <= ivf.len() {
        let size = u32::from_le_bytes(ivf[i..i + 4].try_into().unwrap()) as usize;
        let f = ivf.get(i + 12..i + 12 + size).context("IVF cut short")?;
        // the uncompressed header (profile 0), from the top bit: frame_marker(2) profile(2)
        // show_existing_frame(1) frame_type(1, 0 = key)
        frames.push((f.to_vec(), f.first().is_some_and(|b| b & 0x0c == 0)));
        i += 12 + size;
    }
    // profile 0, level (3.1 up to 720p, 4.1 up to 1080p, 5.1 above), 8 bits
    let level = if w * h <= 1280 * 720 { 31 } else if w * h <= 2048 * 1088 { 41 } else { 51 };
    Ok((format!("vp09.00.{level}.08"), frames))
}
