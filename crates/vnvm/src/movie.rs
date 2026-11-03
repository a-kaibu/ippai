//! Siglus's movies (`.omv`: a header, then Ogg Theora) made playable by the browser's hardware
//! decoder: `vnvm pack` writes `<name>.omv.vnv` next to each one (H.264, see `Vnv`), which sake
//! decodes with WebCodecs in place of the game's own Theora decoder (crates/sake/src/movie.rs).
//!
//! An `.omv` whose Theora picture is taller than the movie (1920x1440 for 1920x1080) carries an
//! alpha channel below it: the movie's rows of alpha are split in three, stacked in the Y, U and
//! V planes (4:4:4) of rows `h..h + (h + 2) / 3`. That does not survive 4:2:0, so it becomes a
//! second H.264 track of its own (the alpha in the luma).
//!
//! ```text
//! .vnv: "VNV1" w h tracks                      (u32 little endian)
//!       per track: codec_len codec(utf-8) frames, then per frame: offset size key
//!       the H.264 access units (Annex B), at their offsets from the start of the file
//! ```
//! A track's frame k is the movie's frame k (gaps in the Theora stream are repeated frames).

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Whether `ffmpeg` (with libx264) can be run.
pub fn available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// The `.vnv` for the `.omv` at `omv`, from the store's cache (`movies/`, by content) or made.
pub fn vnv_for(store: &Path, omv: &Path) -> Result<std::path::PathBuf> {
    let h = blake3::hash(&fs::read(omv)?).to_hex()[..32].to_string();
    let dir = store.join("movies");
    let path = dir.join(format!("{h}.vnv"));
    if !path.exists() {
        fs::create_dir_all(&dir)?;
        let tmp = dir.join(format!(".{h}.part"));
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
        // (H.264 4:2:0 wants even sizes: padded, the reader takes w x h)
        let pad = "pad=ceil(iw/2)*2:ceil(ih/2)*2";
        let rate = format!("1000000/{}", if frame_us == 0 { 33333 } else { frame_us });
        let mut tracks = vec![encode(&src, &tmp.join("c.h264"), &format!("crop={w}:{h}:0:0,{pad},format=yuv420p"), &rate)?];
        if ph > h {
            let a = h.div_ceil(3);
            let last = h - 2 * a;
            let f = format!(
                "crop={w}:{a}:0:{h},extractplanes=y+u+v[y][u][v];[y]null[y1];[u]null[u1];[v]crop={w}:{last}:0:0[v1];\
                 [y1][u1][v1]vstack=3,format=gray,{pad},format=yuv420p"
            );
            tracks.push(encode(&src, &tmp.join("a.h264"), &f, &rate)?);
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
/// codec string and the access units (with whether each is a key frame).
fn encode(src: &Path, out: &Path, filter: &str, rate: &str) -> Result<(String, Vec<(Vec<u8>, bool)>)> {
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-i"])
        .arg(src)
        .args(["-filter_complex", filter, "-fps_mode", "cfr", "-r", rate])
        // no B frames (decode order = movie order), a key frame a second (seeking, loops)
        .args(["-c:v", "libx264", "-preset", "medium", "-crf", "16", "-bf", "0", "-g", "30"])
        .args(["-x264-params", "aud=1", "-f", "h264"])
        .arg(out)
        .status()?;
    if !st.success() {
        bail!("ffmpeg failed ({filter})");
    }
    let es = fs::read(out)?;
    // NAL units: (start of the start code, type)
    let mut nals = Vec::new();
    let mut i = 0;
    while i + 3 < es.len() {
        if es[i] == 0 && es[i + 1] == 0 && es[i + 2] == 1 {
            let s = if i > 0 && es[i - 1] == 0 { i - 1 } else { i };
            nals.push((s, i + 3));
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut codec = None;
    let mut aus: Vec<(Vec<u8>, bool)> = Vec::new();
    for (k, &(s, p)) in nals.iter().enumerate() {
        let end = nals.get(k + 1).map_or(es.len(), |n| n.0);
        let t = es[p] & 31;
        if t == 7 && codec.is_none() && p + 3 < es.len() {
            codec = Some(format!("avc1.{:02x}{:02x}{:02x}", es[p + 1], es[p + 2], es[p + 3]));
        }
        if t == 9 || aus.is_empty() {
            aus.push((Vec::new(), false));
        }
        let au = aus.last_mut().unwrap();
        au.0.extend_from_slice(&es[s..end]);
        au.1 |= t == 5;
    }
    Ok((codec.context("no SPS")?, aus))
}
