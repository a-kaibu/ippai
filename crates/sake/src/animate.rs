//! comctl32's animation control (SysAnimate32): ACM_OPEN(A/W) of a clip (a RIFF AVI from a
//! module's "AVI" resources, or a file), ACM_PLAY / ACM_STOP / ACM_ISPLAYING, ACS_AUTOPLAY,
//! ACS_CENTER (else the control takes the clip's size), ACS_TRANSPARENT (the first frame's
//! top-left colour shows the parent's WM_CTLCOLORSTATIC background), and ACN_START / ACN_STOP
//! to the parent. (A clip it cannot find or read opens as nothing to show: ACM_OPEN is TRUE,
//! as programs take FALSE for an error.) Clips of uncompressed 8-bit (BI_RGB, as sake's own shell32 clips, sysres.rs)
//! or BI_RLE8 frames, as comctl32 itself plays without a codec (Wine's animate.c); others open
//! and play with nothing drawn. Frames advance on a timer of the clip's frame time (comctl32
//! uses a thread): while the program takes its messages. Delphi's TAnimate (せきさば！'s
//! installer's copy page plays shell32's clip 150 with frames 0-28, #222).

use super::*;

/// A clip read from its AVI: its frames as palette indices (bottom-up rows as stored), the
/// palette (0x00RRGGBB), the size and the frame time.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Clip {
    pub w: u32,
    pub h: u32,
    pub us_per_frame: u32,
    pub palette: Vec<u32>,
    /// None: frames of a format sake does not draw
    pub frames: Option<Vec<Vec<u8>>>,
    pub count: u32,
}

fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// The chunks of a RIFF list body: (id, data); a size past the end ends the walk.
fn chunks(b: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= b.len() {
        let id: [u8; 4] = b[at..at + 4].try_into().unwrap();
        let n = le32(b, at + 4).unwrap() as usize;
        let Some(data) = b.get(at + 8..(at + 8).saturating_add(n)) else { break };
        out.push((id, data));
        at = at + 8 + n + n % 2;
    }
    out
}

/// A LIST chunk's type and body.
fn list(data: &[u8]) -> Option<([u8; 4], &[u8])> {
    Some((data.get(..4)?.try_into().ok()?, &data[4..]))
}

/// Applies one BI_RLE8 frame onto `px` (w x h indices, bottom-up): runs, absolute runs, line
/// ends, deltas (the pixels they skip keep the previous frame's) and the end of the bitmap.
pub(crate) fn rle8_onto(px: &mut [u8], w: u32, h: u32, src: &[u8]) {
    let (w, h) = (w as usize, h as usize);
    let (mut x, mut y, mut i) = (0usize, 0usize, 0usize);
    let mut put = |x: usize, y: usize, v: u8| {
        if x < w && y < h {
            px[y * w + x] = v;
        }
    };
    while i + 1 < src.len() && y < h {
        let (n, v) = (src[i], src[i + 1]);
        i += 2;
        if n > 0 {
            for _ in 0..n {
                put(x, y, v);
                x += 1;
            }
            continue;
        }
        match v {
            0 => {
                x = 0;
                y += 1;
            }
            1 => break,
            2 => {
                let (Some(&dx), Some(&dy)) = (src.get(i), src.get(i + 1)) else { break };
                x += dx as usize;
                y += dy as usize;
                i += 2;
            }
            n => {
                for k in 0..n as usize {
                    let Some(&b) = src.get(i + k) else { break };
                    put(x, y, b);
                    x += 1;
                }
                // (absolute runs are padded to a word)
                i += n as usize + (n as usize & 1);
            }
        }
    }
}

/// Reads a RIFF AVI: its main header, its first video stream's format, and the frames of its
/// movi list ('##db' / '##dc' chunks, also inside 'rec ' lists). None: not an AVI.
pub(crate) fn parse_avi(b: &[u8]) -> Option<Clip> {
    if b.get(..4)? != b"RIFF" || b.get(8..12)? != b"AVI " {
        return None;
    }
    let size = (le32(b, 4)? as usize).min(b.len() - 8);
    let body = &b[12..8 + size.max(4)];
    let (mut avih, mut strf, mut movi) = (None, None, None);
    for (id, data) in chunks(body) {
        if &id != b"LIST" {
            continue;
        }
        let Some((ty, inner)) = list(data) else { continue };
        match &ty {
            b"hdrl" => {
                for (id, data) in chunks(inner) {
                    match &id {
                        b"avih" => avih = Some(data),
                        b"LIST" => {
                            if let Some((_, s)) = list(data).filter(|(t, _)| t == b"strl") {
                                let hs = chunks(s);
                                let vids = hs.iter().any(|(i, d)| i == b"strh" && d.get(..4) == Some(b"vids"));
                                if vids && strf.is_none() {
                                    strf = hs.iter().find(|(i, _)| i == b"strf").map(|(_, d)| *d);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            b"movi" => movi = Some(inner),
            _ => {}
        }
    }
    let avih = avih?;
    let us_per_frame = le32(avih, 0)?;
    let count = le32(avih, 16)?;
    let bmi = strf?;
    let (w, hh) = (le32(bmi, 4)?, le32(bmi, 8)? as i32);
    let (bpp, comp) = (u16::from_le_bytes(bmi.get(14..16)?.try_into().ok()?), le32(bmi, 16)?);
    let h = hh.unsigned_abs();
    let hdr = le32(bmi, 0)? as usize;
    let used = le32(bmi, 32).unwrap_or(0) as usize;
    let ncol = if bpp <= 8 { if used == 0 { 1usize << bpp } else { used.min(256) } } else { 0 };
    let palette: Vec<u32> = (0..ncol).filter_map(|k| le32(bmi, hdr + 4 * k)).map(|c| c & 0xff_ffff).collect();
    let drawable = bpp == 8 && matches!(comp, 0 | 1) && w > 0 && h > 0 && (w as u64 * h as u64) <= 4096 * 4096;
    let frames = drawable.then(|| {
        let n = (w * h) as usize;
        let stride = (w as usize).div_ceil(4) * 4;
        let mut cur = vec![0u8; n];
        let mut out = Vec::new();
        let mut take = |data: &[u8], out: &mut Vec<Vec<u8>>| {
            if comp == 1 {
                rle8_onto(&mut cur, w, h, data);
            } else {
                for y in 0..h as usize {
                    // (a top-down clip's rows turned bottom-up)
                    let row = if hh < 0 { h as usize - 1 - y } else { y };
                    let src = data.get(row * stride..row * stride + w as usize);
                    if let Some(src) = src {
                        cur[y * w as usize..(y + 1) * w as usize].copy_from_slice(src);
                    }
                }
            }
            out.push(cur.clone());
        };
        let mut walk = |data: &[u8], out: &mut Vec<Vec<u8>>| {
            for (id, d) in chunks(data) {
                if &id[2..] == b"db" || &id[2..] == b"dc" {
                    take(d, out);
                } else if &id == b"LIST" {
                    if let Some((_, inner)) = list(d).filter(|(t, _)| t == b"rec ") {
                        for (id, d) in chunks(inner) {
                            if &id[2..] == b"db" || &id[2..] == b"dc" {
                                take(d, out);
                            }
                        }
                    }
                }
            }
        };
        if let Some(m) = movi {
            walk(m, &mut out);
        }
        out
    });
    let frames = frames.filter(|f| !f.is_empty());
    let count = frames.as_ref().map_or(count, |f| f.len() as u32);
    Some(Clip { w, h, us_per_frame, palette, frames, count })
}

/// A frame as 32-bit top-down BGRX pixels; `transparent`: the colour that index takes instead
/// (ACS_TRANSPARENT: the top-left pixel's index of the first frame, as comctl32 takes it).
pub(crate) fn frame_pixels(clip: &Clip, k: usize, transparent: Option<(u8, u32)>) -> Option<Vec<u32>> {
    let f = clip.frames.as_ref()?.get(k)?;
    let (w, h) = (clip.w as usize, clip.h as usize);
    let mut out = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let i = f[(h - 1 - y) * w + x];
            let c = match transparent {
                Some((t, bg)) if t == i => bg,
                _ => clip.palette.get(i as usize).copied().unwrap_or(0),
            };
            out[y * w + x] = c;
        }
    }
    Some(out)
}

/// ACM_PLAY's range on a clip of `count` frames: (from, to) with -1 (0xffff) the last frame;
/// a stop past the clip's end is its last frame (sake's shell32 clips have fewer frames than
/// Windows', whose frame numbers programs keep: TAnimate's StopFrame). None: nothing to play.
pub(crate) fn play_range(from: u32, to: u32, count: u32) -> Option<(u32, u32)> {
    if count == 0 {
        return None;
    }
    let last = count - 1;
    let to = if to == 0xffff { last } else { to.min(last) };
    let from = from.min(last);
    (from <= to).then_some((from, to))
}

struct Anim {
    clip: Clip,
    frame: u32,
    from: u32,
    to: u32,
    /// repeats left (-1 forever)
    repeat: i32,
    playing: bool,
}

static ANIMS: Mutex<Option<HashMap<u32, Anim>>> = Mutex::new(None);
fn with_anim<R>(h: u32, f: impl FnOnce(&mut Anim) -> R) -> Option<R> {
    ANIMS.lock().unwrap().get_or_insert_with(HashMap::default).get_mut(&h).map(f)
}
const TIMER_ID: u32 = 0x414e;

fn notify(c: &mut Ctx, h: u32, code: u32) {
    let (id, parent) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.id, w.parent)));
    if parent != 0 {
        send(c, parent, 0x111, code << 16 | (id & 0xffff), h);
    }
}

fn stop(c: &mut Ctx, h: u32) -> bool {
    let was = with_anim(h, |a| std::mem::replace(&mut a.playing, false)).unwrap_or(false);
    if was {
        gcall(c, "user32", "KillTimer", &[h, TIMER_ID]);
        notify(c, h, 2);
    }
    was
}

fn play(c: &mut Ctx, h: u32, repeat: i32, from: u32, to: u32) -> u32 {
    let Some((count, us)) = with_anim(h, |a| (a.clip.count, a.clip.us_per_frame)) else { return 0 };
    let Some((from, to)) = play_range(from, to, count) else { return 0 };
    stop(c, h);
    if from == to {
        // (one frame: shown, nothing plays)
        with_anim(h, |a| a.frame = from);
        invalidate(h, None, true);
        return 1;
    }
    with_anim(h, |a| {
        a.frame = from;
        a.from = from;
        a.to = to;
        a.repeat = repeat;
        a.playing = true;
    });
    invalidate(h, None, true);
    notify(c, h, 1);
    gcall(c, "user32", "SetTimer", &[h, TIMER_ID, (us / 1000).clamp(10, 1000), 0]);
    1
}

fn open(c: &mut Ctx, h: u32, inst: u32, name: u32, wide: bool) -> u32 {
    stop(c, h);
    ANIMS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
    if name == 0 {
        invalidate(h, None, true);
        return 1;
    }
    // a module's "AVI" resource, else (no module, a name) a file
    let res = (inst != 0).then(|| crate::pe::find_resource(inst, &crate::pe::ResId::Name("AVI".encode_utf16().collect()), &crate::pe::ResId::from_arg(name, wide)).map(|e| crate::pe::resource_data(inst, e))).flatten();
    // (a clip sake cannot find or read opens too, shown as nothing: programs take a FALSE as
    // an error, and sake's shell32 has only some of Windows' clips)
    let data = match res {
        Some((p, n)) => mem::slice(p, n).to_vec(),
        None if name >= 0x10000 => {
            let path = if wide { String::from_utf16_lossy(&wstr(name)) } else { String::from_utf16_lossy(&a2w(&cstr(name))) };
            crate::files::resolve(&path).and_then(|f| kura::fs::read(f).ok()).unwrap_or_default()
        }
        None => Vec::new(),
    };
    let Some(clip) = parse_avi(&data) else { return 1 };
    let (style, parent) = with(|s| s.wins.get(&h).map_or((0, 0), |w| (w.style, w.parent)));
    if style & 1 == 0 {
        // (without ACS_CENTER the control takes the clip's size)
        gcall(c, "user32", "SetWindowPos", &[h, 0, 0, 0, clip.w, clip.h, 0x16]);
    }
    let _ = parent;
    let count = clip.count;
    ANIMS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, Anim { clip, frame: 0, from: 0, to: 0, repeat: 0, playing: false });
    invalidate(h, None, true);
    if style & 4 != 0 && count > 1 {
        play(c, h, -1, 0, 0xffff);
    }
    1
}

fn paint(c: &mut Ctx, h: u32) {
    let (style, parent, (w, hh)) = with(|s| s.wins.get(&h).map_or((0, 0, (0, 0)), |w| (w.style, w.parent, (w.client.w(), w.client.h()))));
    let ps = mem::heap_alloc(64, true);
    let dc = gcall(c, "user32", "BeginPaint", &[h, ps]);
    // the background: the parent's WM_CTLCOLORSTATIC brush (else the dialog face)
    let mut brush = if parent != 0 { send(c, parent, 0x138, dc, h) } else { 0 };
    if brush == 0 {
        brush = crate::gdi::solid_brush(0xf0f0f0);
    }
    crate::gdi::fill_rect_brush(dc, Rect { l: 0, t: 0, r: w, b: hh }, brush);
    let shown = ANIMS.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).and_then(|a| {
        let t = (style & 2 != 0).then(|| a.clip.frames.as_ref().and_then(|f| f.first()).map(|f0| f0[(a.clip.h as usize - 1) * a.clip.w as usize])).flatten();
        let bg = t.map(|t| (t, crate::gdi::brush_color(brush).unwrap_or(0xf0f0f0)));
        Some((frame_pixels(&a.clip, a.frame as usize, bg)?, a.clip.w, a.clip.h))
    });
    if let Some((px, fw, fh)) = shown {
        let (x, y) = if style & 1 != 0 { ((w - fw as i32) / 2, (hh - fh as i32) / 2) } else { (0, 0) };
        // a 32-bit top-down DIB of the frame
        let bmi = mem::heap_alloc(40 + px.len() as u32 * 4, true);
        for (k, v) in [40u32, fw, (-(fh as i32)) as u32].iter().enumerate() {
            mem::w32(bmi + 4 * k as u32, *v);
        }
        mem::w16(bmi + 12, 1);
        mem::w16(bmi + 14, 32);
        let bits = bmi + 40;
        for (k, p) in px.iter().enumerate() {
            // (0x00RRGGBB as B, G, R, 0)
            mem::w32(bits + 4 * k as u32, *p);
        }
        gcall(c, "gdi32", "SetDIBitsToDevice", &[dc, x as u32, y as u32, fw, fh, 0, 0, 0, fh, bits, bmi, 0]);
        mem::heap_free(bmi);
    }
    gcall(c, "user32", "EndPaint", &[h, ps]);
    mem::heap_free(ps);
}

pub(super) fn proc_fn(c: &mut Ctx) -> u32 {
    let (h, m, wp, lp) = (c.a(0), c.a(1), c.a(2), c.a(3));
    match m {
        // ACM_OPENA / ACM_OPENW (hInstance, name or id; NULL closes)
        0x464 | 0x467 => open(c, h, wp, lp, m == 0x467),
        // ACM_PLAY(repeat, MAKELONG(from, to))
        0x465 => play(c, h, wp as i32, lp & 0xffff, lp >> 16),
        // ACM_STOP
        0x466 => {
            stop(c, h);
            1
        }
        // ACM_ISPLAYING
        0x468 => with_anim(h, |a| a.playing as u32).unwrap_or(0),
        0x113 if wp == TIMER_ID => {
            let done = with_anim(h, |a| {
                if !a.playing {
                    return false;
                }
                if a.frame < a.to {
                    a.frame += 1;
                    return false;
                }
                if a.repeat == 0 || a.repeat == 1 {
                    return true;
                }
                if a.repeat > 0 {
                    a.repeat -= 1;
                }
                a.frame = a.from;
                false
            });
            if done == Some(true) {
                stop(c, h);
            }
            invalidate(h, None, false);
            0
        }
        0xf => {
            paint(c, h);
            0
        }
        0x14 => 1,
        0x2 => {
            stop(c, h);
            ANIMS.lock().unwrap().get_or_insert_with(HashMap::default).remove(&h);
            def_proc(c, msg_wide())
        }
        _ => def_proc(c, msg_wide()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// sake's own shell32 clip opens as an 8-frame drawable clip of its size, each frame
    /// different from the one before (the page moves), index 0 (the face colour) at the
    /// top-left as the transparent one.
    #[test]
    fn shell32_clip() {
        let clip = parse_avi(&crate::sysres::copy_clip()).expect("an AVI");
        let frames = clip.frames.as_ref().expect("drawable");
        assert_eq!(clip.count as usize, frames.len());
        assert!(clip.count >= 2);
        assert_eq!(clip.us_per_frame, 100_000);
        for k in 1..frames.len() {
            assert_ne!(frames[k], frames[k - 1], "frame {k}");
        }
        assert_eq!(frames[0][(clip.h as usize - 1) * clip.w as usize], 0);
        let px = frame_pixels(&clip, 0, Some((0, 0x123456))).unwrap();
        assert_eq!(px[0], 0x123456);
        assert_eq!(frame_pixels(&clip, 0, None).unwrap()[0], 0xf0f0f0);
    }

    /// An RLE8 frame: a run, a line end, an absolute run (padded), a delta that keeps the
    /// pixels it skips, the bitmap's end.
    #[test]
    fn rle8_frame() {
        let mut px = vec![9u8; 4 * 3];
        rle8_onto(&mut px, 4, 3, &[3, 1, 0, 0, 0, 3, 5, 6, 7, 0, 0, 0, 0, 2, 1, 0, 1, 2, 0, 1]);
        assert_eq!(px, [1, 1, 1, 9, 5, 6, 7, 9, 9, 2, 9, 9]);
    }

    proptest! {
        /// Any bytes: no panic, and whatever opens has frames of its size.
        #[test]
        fn any_bytes(b in prop::collection::vec(any::<u8>(), 0..400)) {
            if let Some(c) = parse_avi(&b) {
                for f in c.frames.iter().flatten() {
                    prop_assert_eq!(f.len(), (c.w * c.h) as usize);
                }
            }
        }

        /// The shell32 clip cut short or with bytes changed: no panic.
        #[test]
        fn damaged_clip(cut in 0usize..4000, at in 0usize..4000, v in any::<u8>()) {
            let mut b = crate::sysres::copy_clip();
            if at < b.len() { b[at] = v; }
            b.truncate(cut.max(1).min(b.len()));
            let _ = parse_avi(&b);
        }

        /// Any RLE8 data on any small frame: no panic, the size kept.
        #[test]
        fn rle8_any(w in 1u32..20, h in 1u32..20, src in prop::collection::vec(any::<u8>(), 0..200)) {
            let mut px = vec![0u8; (w * h) as usize];
            rle8_onto(&mut px, w, h, &src);
            prop_assert_eq!(px.len(), (w * h) as usize);
        }

        /// ACM_PLAY's range: inside the clip, from <= to, -1 the last frame, a stop past the
        /// end the last frame, nothing on an empty clip or a start after the stop.
        #[test]
        fn ranges(from in 0u32..0x10000, to in 0u32..0x10000, count in 0u32..100) {
            match play_range(from, to, count) {
                Some((f, t)) => {
                    prop_assert!(f <= t && t < count);
                    if to == 0xffff || to >= count { prop_assert_eq!(t, count - 1); } else { prop_assert_eq!(t, to); }
                }
                None => {
                    let stop = if to == 0xffff { count.saturating_sub(1) } else { to.min(count.saturating_sub(1)) };
                    prop_assert!(count == 0 || from.min(count - 1) > stop);
                }
            }
        }
    }
}
