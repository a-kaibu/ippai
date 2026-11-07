//! Movies the page decodes with WebCodecs (web/src/video.ts), for guest code that would decode
//! them itself in x86 (Siglus's Theora: crates/sake/src/movie.rs, crates/ippai/src/movie.rs).
//!
//! A stream holds a `.vnv` file (VP9 tracks: color, and alpha when the movie has one). The
//! guest asks for frame n with `frame`; the page, which lists the streams every frame
//! (`vnhost_video_list`) and wakes at once on a request (`vnhost_video_doorbell`), decodes ahead
//! of the newest request and copies decoded frames into the stream's slots
//! (`vnhost_video_begin` / `vnhost_video_ready`); `frame` turns a slot into the guest's pixels.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Frames decoded ahead of the newest request (and so slots per stream).
const SLOTS: usize = 3;

/// Plane formats of a slot's track (VideoFrame.format).
const I420: u32 = 0;
const NV12: u32 = 1;
const RGBA: u32 = 2;

struct Slot {
    buf: Vec<u8>,
    /// the frame it holds (-1: none), whether the page is writing it, and how many guest
    /// threads read it (a count: one reader done must not free the slot under another,
    /// formal/tla/VideoSlots.tla)
    frame: i32,
    writing: bool,
    reading: u32,
    /// per track: format, then offset and stride of planes 0..3 in `buf`
    layout: [[u32; 7]; 2],
}

/// Where a stream's bytes are.
enum Source {
    /// all of the file
    Mem(Vec<u8>),
    /// read as its frames are wanted (`fill`): the header and tables (what the page parses),
    /// and per track the bytes of a run of its frames (offset in the file, bytes)
    File { file: Arc<Mutex<crate::fs::File>>, head: Vec<u8>, wins: Vec<(u64, Vec<u8>)> },
}

impl Source {
    fn head(&self) -> &[u8] {
        match self {
            Source::Mem(d) => d,
            Source::File { head, .. } => head,
        }
    }
    /// Bytes `off..off + size` of the file, when they are here.
    fn bytes(&self, off: u64, size: usize) -> Option<&[u8]> {
        let at = |o: u64, b: &'_ [u8]| -> Option<std::ops::Range<usize>> {
            let s = off.checked_sub(o)? as usize;
            let e = s.checked_add(size)?;
            (e <= b.len()).then_some(s..e)
        };
        match self {
            Source::Mem(d) => d.get(at(0, d)?),
            Source::File { wins, .. } => wins.iter().find_map(|(o, b)| Some(&b[at(*o, b)?])),
        }
    }
}

struct Stream {
    src: Source,
    /// per track: its frames (offset, size, key frame)
    index: Vec<crate::vnv::Frames>,
    w: u32,
    h: u32,
    tracks: u32,
    want: i32,
    slots: Vec<Slot>,
    /// closed while the page wrote a slot or the guest read one: dropped when the last of them
    /// is done (see `release`), and no new reads or writes start
    closed: bool,
    /// the page cannot decode it (the guest gets no frames, without waiting)
    failed: bool,
    /// the frame handed out last (-1: none yet, -2: none in time)
    shown: i32,
    opened: Instant,
    /// frames of uncompressed I420 ("raw-i420", for tests under node): `frame` reads them
    /// itself, no page involved
    raw: bool,
}

static STREAMS: Mutex<Option<HashMap<u32, Stream>>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();
/// The page bumps nothing here: the guest adds 1 and notifies after each new request.
static DOORBELL: AtomicI32 = AtomicI32::new(0);
/// The page lists streams (a page is there to decode).
static PAGE: AtomicBool = AtomicBool::new(false);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, Stream>) -> R) -> R {
    let mut g = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(HashMap::new))
}

/// Whether a page decodes (no page under node: the guest keeps its own decoder).
pub fn available() -> bool {
    PAGE.load(Ordering::Relaxed)
}

pub use crate::vnv::{info, raw_size, Info};

/// Opens the `.vnv` in `data`; None when it is not one.
pub fn open(data: Vec<u8>) -> Option<u32> {
    let i = info(&data)?;
    add(Source::Mem(data), i)
}

/// Opens the `.vnv` file at `path`, read as its frames are wanted (a movie can be hundreds of
/// MiB: the store's chunks of it are fetched a few seconds ahead of the frame shown); the
/// stream, and what the file says of itself. None when it is not one.
pub fn open_file(path: impl AsRef<std::path::Path>) -> Option<(u32, Info)> {
    let mut f = crate::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let (i, head_len) = crate::vnv::read_info(len, &mut |o, b| read_at(&mut f, o, b))?;
    let mut head = vec![0; head_len];
    if !read_at(&mut f, 0, &mut head) {
        return None;
    }
    let wins = vec![(0, Vec::new()); i.tracks.len()];
    let id = add(Source::File { file: Arc::new(Mutex::new(f)), head, wins }, i.clone())?;
    Some((id, i))
}

/// Reads `buf.len()` bytes at `off`.
pub fn read_at(f: &mut crate::fs::File, off: u64, buf: &mut [u8]) -> bool {
    use std::io::{Read, Seek, SeekFrom};
    f.seek(SeekFrom::Start(off)).is_ok() && f.read_exact(buf).is_ok()
}

fn add(src: Source, i: Info) -> Option<u32> {
    let (w, h, tracks) = (i.w, i.h, i.tracks.len() as u32);
    // (raw: one track of whole frames)
    let raw = match &i.tracks[..] {
        [(c, f)] if c == "raw-i420" => {
            if f.iter().any(|&(_, size, _)| (size as usize) < raw_size(w, h)) {
                return None;
            }
            true
        }
        t if t.iter().any(|(c, _)| c == "raw-i420") => return None,
        _ => false,
    };
    let index = i.tracks.into_iter().map(|t| t.1).collect();
    static NEXT: AtomicI32 = AtomicI32::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed) as u32;
    let slots = (0..SLOTS)
        .map(|_| Slot { buf: Vec::new(), frame: -1, writing: false, reading: 0, layout: [[0; 7]; 2] })
        .collect();
    // (IPPAI_VIDEO_FAIL: every stream fails, as on a phone without decoders: tests the fallback)
    let failed = std::env::var_os("IPPAI_VIDEO_FAIL").is_some();
    with(|m| m.insert(id, Stream { src, index, w, h, tracks, want: -1, slots, closed: false, failed, shown: -1, opened: Instant::now(), raw }));
    Some(id)
}

/// Frames past the one asked for that `fill` keeps read, and how many it reads ahead once
/// fewer are (seconds at a game's 30 fps); the store's chunks for twice as many are fetched
/// meanwhile (not waited for), so a read rarely waits on the network.
const READ_LOW: usize = 30;
const READ_HIGH: usize = 120;
/// At most this much of a track read at once (a bad table: no huge allocation).
const READ_MAX: u64 = 64 << 20;

/// Has the bytes of frame `n` of stream `id` read (a file's), from each track's key frame
/// before it (where its decoder starts) to a few seconds past it; the bytes before go. Reads
/// without holding the streams' lock (the page takes it every frame).
fn fill(id: u32, n: i32) {
    let plan = with(|m| {
        let s = m.get(&id).filter(|s| !s.closed)?;
        let Source::File { file, wins, .. } = &s.src else { return None };
        let mut ahead = (u64::MAX, 0u64);
        let reads: Vec<Option<(u64, u64, u64)>> = s
            .index
            .iter()
            .zip(wins)
            .map(|(frames, (o, b))| {
                let last = frames.len().checked_sub(1)?;
                let n = (n.max(0) as usize).min(last);
                let k = (0..=n).rev().find(|&k| frames[k].2).unwrap_or(0);
                let start = frames[k].0 as u64;
                let end_of = |j: usize| {
                    let f = frames[j.min(last)];
                    f.0 as u64 + f.1 as u64
                };
                let far = end_of(n + 2 * READ_HIGH).max(start);
                ahead = (ahead.0.min(start), ahead.1.max(far));
                let have = (*o, *o + b.len() as u64);
                if have.0 <= start && end_of(n + READ_LOW) <= have.1 {
                    return None;
                }
                let end = end_of(n + READ_HIGH).clamp(start, start + READ_MAX);
                // (what is here of it stays: only the rest is read)
                let from = if have.0 <= start && start <= have.1 && have.1 <= end { have.1 } else { start };
                Some((start, from, end))
            })
            .collect();
        Some((file.clone(), reads, ahead))
    });
    let Some((file, reads, ahead)) = plan else { return };
    let mut f = file.lock().unwrap_or_else(|e| e.into_inner());
    if ahead.0 < ahead.1 {
        f.prefetch(ahead.0, ahead.1 - ahead.0);
    }
    if reads.iter().all(Option::is_none) {
        return;
    }
    let got: Vec<_> = reads
        .into_iter()
        .map(|r| {
            let (start, from, end) = r?;
            let mut b = vec![0; (end - from) as usize];
            read_at(&mut f, from, &mut b).then_some((start, from, b))
        })
        .collect();
    drop(f);
    with(|m| {
        let Some(Source::File { wins, .. }) = m.get_mut(&id).map(|s| &mut s.src) else { return };
        for ((o, b), g) in wins.iter_mut().zip(got) {
            let Some((start, from, tail)) = g else { continue };
            if from == start || from != *o + b.len() as u64 || start < *o {
                // (another thread changed it meanwhile: what was read alone)
                (*o, *b) = (from, tail);
            } else {
                b.drain(..(start - *o) as usize);
                b.extend_from_slice(&tail);
                *o = start;
            }
        }
    });
}

/// Whether stream `id`'s frames can be had: a page decodes, or it is uncompressed.
pub fn decodable(id: u32) -> bool {
    available() || with(|m| m.get(&id).is_some_and(|s| s.raw))
}

/// Whether the page gave up on stream `id` (no decoder for it, or the decoder failed: Android
/// takes hardware decoders back when several movies play at once).
pub fn failed(id: u32) -> bool {
    with(|m| m.get(&id).is_some_and(|s| s.failed))
}

pub fn close(id: u32) {
    // (a slot the page writes into or the guest reads stays until it is done: see release)
    with(|m| {
        if let Some(s) = m.get_mut(&id) {
            s.closed = true;
        }
        release(m, id);
    });
    WAKE.notify_all();
}

/// Drops stream `id` when it is closed and no slot of it is written or read any more.
fn release(m: &mut HashMap<u32, Stream>, id: u32) {
    if m.get(&id).is_some_and(|s| s.closed && !s.slots.iter().any(|sl| sl.writing || sl.reading > 0)) {
        m.remove(&id);
    }
}

fn ring() {
    DOORBELL.fetch_add(1, Ordering::SeqCst);
    #[cfg(target_os = "emscripten")]
    unsafe {
        extern "C" {
            fn emscripten_futex_wake(addr: *const std::ffi::c_void, count: i32) -> i32;
        }
        emscripten_futex_wake(DOORBELL.as_ptr() as *const std::ffi::c_void, i32::MAX);
    }
}

/// How `frame` lays out the guest's pixels (B, G, R, A bytes per pixel).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Out {
    /// Y, Cb, Cr, alpha (255 without an alpha track): the game converts on the GPU
    Ycc,
    /// RGB by BT.601 (Theora's), alpha 255
    Rgb,
    /// RGB by BT.601 from video range (Y 16-235, Cb Cr 16-240: what `ippai pack` makes of other
    /// movies), alpha 255
    Rgb601,
}

/// How long the guest waits for the frame it asks for once the movie shows, before it takes the
/// newest decoded one before it (or keeps its pixels): the game paces movies by the clock, so a
/// late frame is better skipped than waited for (a 30 fps movie has 33 ms a frame).
const WAIT: Duration = Duration::from_millis(10);
/// For the first frame, from the stream's opening (the decoder starting up).
const FIRST: Duration = Duration::from_millis(500);

/// Writes frame `n` (or the newest decoded frame before it, when `n` is late) into `dst` (`w` x
/// `h` 32-bit pixels, rows `pitch` bytes apart); false when the page delivered nothing.
///
/// # Safety
/// `dst` must hold `h` rows of `pitch` bytes.
pub unsafe fn frame(id: u32, n: i32, dst: *mut u8, pitch: usize, w: u32, h: u32, out: Out) -> bool {
    let start = Instant::now();
    fill(id, n);
    let mut g = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
    // uncompressed: straight from the file (frames past the end: the last one)
    if let Some(s) = g.get_or_insert_with(HashMap::new).get_mut(&id).filter(|s| !s.closed && !s.failed && s.raw) {
        let f = &s.index[0];
        let Some(&(off, _, _)) = f.get((n.max(0) as usize).min(f.len().saturating_sub(1))) else { return false };
        let (sw, sh) = (s.w, s.h);
        s.shown = n;
        let Some(bytes) = s.src.bytes(off as u64, raw_size(sw, sh)) else { return false };
        let (pw, ph) = (sw.next_multiple_of(2), sh.next_multiple_of(2));
        let (y, c) = (pw * ph, (pw / 2) * (ph / 2));
        let layout = [[I420, 0, pw, y, pw / 2, y + c, pw / 2], [0; 7]];
        // (the stream stays while this reads it: the lock is held)
        convert(bytes.as_ptr(), &layout, 1, sw, sh, dst, pitch, w.min(sw), h.min(sh), out);
        return true;
    }
    loop {
        let Some(s) = g.get_or_insert_with(HashMap::new).get_mut(&id).filter(|s| !s.closed && !s.failed) else { return false };
        if s.want != n {
            s.want = n;
            ring();
        }
        let late = start.elapsed() > WAIT && (s.shown >= 0 || s.opened.elapsed() > FIRST);
        let ready = |sl: &Slot| !sl.writing && sl.frame >= 0 && if late { sl.frame <= n && sl.frame > s.shown } else { sl.frame == n };
        let k = (0..s.slots.len()).filter(|&k| ready(&s.slots[k])).max_by_key(|&k| s.slots[k].frame);
        if let Some(k) = k {
            stat(start.elapsed(), n, s.slots[k].frame != n);
            s.shown = s.slots[k].frame;
            s.slots[k].reading += 1;
            let (sw, sh, tracks) = (s.w, s.h, s.tracks);
            let (buf, layout) = (s.slots[k].buf.as_ptr(), s.slots[k].layout);
            drop(g);
            convert(buf, &layout, tracks, sw, sh, dst, pitch, w.min(sw), h.min(sh), out);
            let mut g = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
            let m = g.get_or_insert_with(HashMap::new);
            if let Some(s) = m.get_mut(&id) {
                s.slots[k].reading -= 1;
            }
            release(m, id);
            return true;
        }
        if late {
            // nothing newer: the old pixels stay
            if s.shown == -1 {
                eprintln!("kura: video {id}: no frame decoded {FIRST:?} after it opened");
                s.shown = -2;
            }
            stat(start.elapsed(), n, true);
            return s.shown >= 0;
        }
        g = WAKE.wait_timeout(g, Duration::from_millis(2)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// IPPAI_VIDEO_STATS: frames handed out and the guest's wait for them, every 5 s.
fn stat(waited: Duration, n: i32, late: bool) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ON.get_or_init(|| std::env::var_os("IPPAI_VIDEO_STATS").is_some()) {
        return;
    }
    static S: Mutex<(Option<Instant>, u32, Duration, Duration, i32, u32)> = Mutex::new((None, 0, Duration::ZERO, Duration::ZERO, -1, 0));
    let mut g = S.lock().unwrap();
    let t0 = *g.0.get_or_insert_with(Instant::now);
    g.1 += 1;
    g.2 += waited;
    g.3 = g.3.max(waited);
    if g.4 >= 0 && n != g.4 + 1 {
        g.5 += 1;
    }
    static LATE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    if late {
        LATE.fetch_add(1, Ordering::Relaxed);
    }
    g.4 = n;
    if t0.elapsed() >= Duration::from_secs(5) {
        eprintln!(
            "kura: video {} frames ({} late), waited {:?} (max {:?}), {} jumps",
            g.1,
            LATE.swap(0, Ordering::Relaxed),
            g.2,
            g.3,
            g.5
        );
        *g = (Some(Instant::now()), 0, Duration::ZERO, Duration::ZERO, n, 0);
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn convert(buf: *const u8, layout: &[[u32; 7]; 2], tracks: u32, sw: u32, sh: u32, dst: *mut u8, pitch: usize, w: u32, h: u32, out: Out) {
    let c = &layout[0];
    if tracks == 1 && matches!(c[0], I420 | NV12) && out == Out::Rgb601 {
        return to_rgb(buf, c, dst, pitch, w, h, out);
    }
    convert_any(buf, layout, tracks, sw, sh, dst, pitch, w, h, out)
}

/// The terms of ycc_to_rgb / ycc601_to_rgb by byte, in fixed point (FRAC bits): Y, Cr's in
/// red, Cb's and Cr's in green, Cb's in blue. Rounded (601) or truncated as those functions
/// are, they give the same bytes for every Y, Cb, Cr (terms_as_the_functions checks all).
struct Terms {
    y: [i32; 256],
    rv: [i32; 256],
    gu: [i32; 256],
    gv: [i32; 256],
    bu: [i32; 256],
}
const FRAC: u32 = 21;
fn terms(out: Out) -> &'static Terms {
    static T: OnceLock<[Terms; 2]> = OnceLock::new();
    let t = T.get_or_init(|| {
        let make = |video: bool| {
            let mut t = Terms { y: [0; 256], rv: [0; 256], gu: [0; 256], gv: [0; 256], bu: [0; 256] };
            let f = |x: f64| (x * (1 << FRAC) as f64).round() as i32;
            for i in 0..256 {
                let x = i as f64;
                let (y, u) = if video { ((x - 16.0) * 255.0 / 219.0, (x - 128.0) * 255.0 / 224.0) } else { (x, x - 128.0) };
                let (gu, gv) = if video { (0.344136, 0.714136) } else { (0.34414, 0.71414) };
                (t.y[i], t.rv[i], t.gu[i], t.gv[i], t.bu[i]) = (f(y), f(1.402 * u), f(gu * u), f(gv * u), f(1.772 * u));
            }
            t
        };
        [make(false), make(true)]
    });
    &t[(out == Out::Rgb601) as usize]
}

/// [`convert_any`] of one I420 / NV12 track to BGRX: the same bytes, in fixed point from
/// tables (a movie frame's conversion, every frame).
unsafe fn to_rgb(buf: *const u8, c: &[u32; 7], dst: *mut u8, pitch: usize, w: u32, h: u32, out: Out) {
    let t = terms(out);
    let p = |i: usize| buf.add(c[1 + 2 * i] as usize);
    let st = |i: usize| c[2 + 2 * i] as usize;
    let nv12 = c[0] == NV12;
    // (rounding: half added; truncating: none, values below 0 clamp either way)
    let half = if out == Out::Rgb601 { 1 << (FRAC - 1) } else { 0 };
    let q = |x: i32| ((x + half) >> FRAC).clamp(0, 255) as u8;
    for y in 0..h as usize {
        let row = dst.add(y * pitch);
        let yr = p(0).add(y * st(0));
        let (ur, vr, step) = if nv12 {
            let uv = p(1).add((y / 2) * st(1));
            (uv, uv.add(1), 2)
        } else {
            (p(1).add((y / 2) * st(1)), p(2).add((y / 2) * st(2)), 1)
        };
        for x in 0..w as usize {
            let k = (x / 2) * step;
            let (u, v) = (*ur.add(k) as usize, *vr.add(k) as usize);
            let yy = t.y[*yr.add(x) as usize];
            let o = row.add(x * 4);
            *o = q(yy + t.bu[u]);
            *o.add(1) = q(yy - t.gu[u] - t.gv[v]);
            *o.add(2) = q(yy + t.rv[v]);
            *o.add(3) = 255;
        }
    }
}

unsafe fn convert_any(buf: *const u8, layout: &[[u32; 7]; 2], tracks: u32, sw: u32, _sh: u32, dst: *mut u8, pitch: usize, w: u32, h: u32, out: Out) {
    let _ = sw;
    let c = &layout[0];
    let a = (tracks > 1).then_some(&layout[1]);
    let p = |l: &[u32; 7], i: usize| buf.add(l[1 + 2 * i] as usize);
    let st = |l: &[u32; 7], i: usize| l[2 + 2 * i] as usize;
    for y in 0..h as usize {
        let row = dst.add(y * pitch);
        // color: Y, Cb, Cr of each pixel
        let (yr, ur, vr, uvx): (*const u8, *const u8, *const u8, usize) = match c[0] {
            NV12 => {
                let uv = p(c, 1).add((y / 2) * st(c, 1));
                (p(c, 0).add(y * st(c, 0)), uv, uv.add(1), 2)
            }
            I420 => (p(c, 0).add(y * st(c, 0)), p(c, 1).add((y / 2) * st(c, 1)), p(c, 2).add((y / 2) * st(c, 2)), 1),
            _ => (p(c, 0).add(y * st(c, 0)), std::ptr::null(), std::ptr::null(), 0),
        };
        let ar: *const u8 = match a {
            Some(l) => p(l, 0).add(y * st(l, 0)),
            None => std::ptr::null(),
        };
        let rgba_alpha = a.is_some_and(|l| l[0] == RGBA);
        for x in 0..w as usize {
            let (yy, cb, cr) = if c[0] == RGBA {
                let q = yr.add(x * 4);
                rgb_to_ycc(*q, *q.add(1), *q.add(2))
            } else {
                // (uvx: bytes between chroma samples, 2 for NV12's interleaved Cb Cr)
                let k = (x / 2) * uvx;
                (*yr.add(x), *ur.add(k), *vr.add(k))
            };
            let al = if ar.is_null() {
                255
            } else if rgba_alpha {
                *ar.add(x * 4)
            } else {
                *ar.add(x)
            };
            let o = row.add(x * 4);
            match out {
                Out::Ycc => {
                    *o = yy;
                    *o.add(1) = cb;
                    *o.add(2) = cr;
                    *o.add(3) = al;
                }
                Out::Rgb | Out::Rgb601 => {
                    let (r, g, b) = if out == Out::Rgb { ycc_to_rgb(yy, cb, cr) } else { ycc601_to_rgb(yy, cb, cr) };
                    *o = b;
                    *o.add(1) = g;
                    *o.add(2) = r;
                    *o.add(3) = 255;
                }
            }
        }
    }
}

/// BT.601 full range, as Siglus's table (truncated, clamped).
fn ycc_to_rgb(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let (y, u, v) = (y as f64, cb as f64 - 128.0, cr as f64 - 128.0);
    let c = |x: f64| (x as i32).clamp(0, 255) as u8;
    (c(y + 1.402 * v), c(y - 0.34414 * u - 0.71414 * v), c(y + 1.772 * u))
}

/// BT.601 from video range (as Windows' decoders give RGB of such a movie), rounded.
pub fn ycc601_to_rgb(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    let (y, u, v) = ((y as f64 - 16.0) * 255.0 / 219.0, (cb as f64 - 128.0) * 255.0 / 224.0, (cr as f64 - 128.0) * 255.0 / 224.0);
    let c = |x: f64| x.round().clamp(0.0, 255.0) as u8;
    (c(y + 1.402 * v), c(y - 0.344136 * u - 0.714136 * v), c(y + 1.772 * u))
}

fn rgb_to_ycc(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let (r, g, b) = (r as f64, g as f64, b as f64);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    let c = |x: f64| x.round().clamp(0.0, 255.0) as u8;
    (c(y), c(128.0 + (b - y) / 1.772), c(128.0 + (r - y) / 1.402))
}

// ---------------------------------------------------------------------------------------------
// the page

/// The doorbell's address (the page waits on it with Atomics.waitAsync).
#[no_mangle]
pub extern "C" fn vnhost_video_doorbell() -> u32 {
    DOORBELL.as_ptr() as u32
}

/// Lists up to `max` streams into `out`, 6 words each: id, header ptr and bytes (the `.vnv`'s
/// header and frame tables, `vnv::read_info`), newest
/// request (-1: none yet), first frame held in a slot, last frame held (-1, -1: none).
/// Returns how many.
///
/// # Safety
/// `out` must hold `max * 6` words.
#[no_mangle]
pub unsafe extern "C" fn vnhost_video_list(out: *mut u32, max: u32) -> u32 {
    PAGE.store(true, Ordering::Relaxed);
    let out = std::slice::from_raw_parts_mut(out, max as usize * 6);
    with(|m| {
        let mut n = 0;
        for (&id, s) in m.iter().filter(|(_, s)| !s.closed) {
            if n == max as usize {
                break;
            }
            let held: Vec<i32> = s.slots.iter().filter(|sl| sl.frame >= 0).map(|sl| sl.frame).collect();
            out[n * 6..n * 6 + 6].copy_from_slice(&[
                id,
                s.src.head().as_ptr() as u32,
                s.src.head().len() as u32,
                s.want as u32,
                held.iter().min().copied().unwrap_or(-1) as u32,
                held.iter().max().copied().unwrap_or(-1) as u32,
            ]);
            n += 1;
        }
        n as u32
    })
}

/// The page's copy of a frame's bytes (`vnhost_video_chunk`): only the page's thread uses it,
/// so it stays as it is while the page reads it, whatever the guest does to the stream.
static CHUNK: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// The bytes of frame `n` of track `t` of stream `id`, copied for the page (and their size in
/// `*size`); 0 when they are not read yet (`fill`: the page tries again on its next frame).
///
/// # Safety
/// `size` must be writable.
#[no_mangle]
pub unsafe extern "C" fn vnhost_video_chunk(id: u32, t: u32, n: u32, size: *mut u32) -> u32 {
    let mut c = CHUNK.lock().unwrap_or_else(|e| e.into_inner());
    let ok = with(|m| {
        let s = m.get(&id)?;
        let &(off, sz, _) = s.index.get(t as usize)?.get(n as usize)?;
        let b = s.src.bytes(off as u64, sz as usize)?;
        c.clear();
        c.extend_from_slice(b);
        Some(())
    });
    if ok.is_none() {
        return 0;
    }
    *size = c.len() as u32;
    // (an empty frame still has an address the page can read nothing from)
    c.reserve(1);
    c.as_ptr() as u32
}

/// The page cannot decode stream `id`.
#[no_mangle]
pub extern "C" fn vnhost_video_fail(id: u32) {
    with(|m| {
        if let Some(s) = m.get_mut(&id) {
            s.failed = true;
        }
    });
    WAKE.notify_all();
}

/// Whether stream `id` holds frame `n` in a slot.
#[no_mangle]
pub extern "C" fn vnhost_video_has(id: u32, n: i32) -> u32 {
    with(|m| m.get(&id).is_some_and(|s| s.slots.iter().any(|sl| sl.frame == n)) as u32)
}

/// The page is about to copy frame `n` of stream `id`: a slot's buffer (0: none free now, or
/// the stream is gone), of at least `*size` bytes (what the page copies). The slot reused is the one with the oldest frame
/// (none the guest reads, none it may still want: frames before its newest request go first).
///
/// # Safety
/// `size` must be writable.
#[no_mangle]
pub unsafe extern "C" fn vnhost_video_begin(id: u32, n: i32, size: *mut u32) -> u32 {
    with(|m| {
        let Some(s) = m.get_mut(&id).filter(|s| !s.closed) else { return 0 };
        let want = s.want;
        let free = s
            .slots
            .iter()
            .enumerate()
            .filter(|(_, sl)| sl.reading == 0 && !sl.writing)
            .min_by_key(|(_, sl)| if sl.frame < 0 || sl.frame < want { i32::MIN + sl.frame.max(0) } else { sl.frame })
            .map(|(k, _)| k);
        let Some(k) = free else { return 0 };
        let sl = &mut s.slots[k];
        if sl.frame >= want && sl.frame >= 0 && sl.frame < n {
            // every slot holds a frame still to come before this one
            return 0;
        }
        sl.writing = true;
        sl.frame = n;
        // (sized by the page's first copy: YUV, or RGBA when frames are not readable as YUV)
        if sl.buf.len() < *size as usize {
            sl.buf = vec![0; *size as usize];
        }
        sl.buf.as_mut_ptr() as u32
    })
}

/// The page copied the frame into the slot at `buf`: `layout` is 14 words, per track the format
/// (0 I420, 1 NV12, 2 RGBA) and offset and stride of three planes. `ok` = 0: it failed.
///
/// # Safety
/// `layout` must hold 14 words.
#[no_mangle]
pub unsafe extern "C" fn vnhost_video_ready(id: u32, buf: u32, layout: *const u32, ok: u32) {
    let l = std::slice::from_raw_parts(layout, 14);
    with(|m| {
        let Some(s) = m.get_mut(&id) else { return };
        if let Some(sl) = s.slots.iter_mut().find(|sl| sl.buf.as_ptr() as u32 == buf) {
            sl.writing = false;
            if ok == 0 {
                sl.frame = -1;
            } else {
                sl.layout[0].copy_from_slice(&l[..7]);
                sl.layout[1].copy_from_slice(&l[7..]);
            }
        }
        release(m, id);
    });
    WAKE.notify_all();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vnv() -> Vec<u8> {
        let mut d = b"VNV1".to_vec();
        for v in [16u32, 16, 1, 0, 0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d
    }

    fn exists(id: u32) -> bool {
        with(|m| m.contains_key(&id))
    }

    #[test]
    fn close_waits_for_the_page_writing() {
        let id = open(vnv()).unwrap();
        let mut size = 16 * 16 * 4;
        let buf = unsafe { vnhost_video_begin(id, 0, &mut size) };
        assert_ne!(buf, 0);
        close(id);
        assert!(exists(id), "dropped while the page writes into it");
        assert_eq!(unsafe { vnhost_video_begin(id, 1, &mut size) }, 0, "a closed stream takes no new writes");
        let layout = [RGBA, 0, 64, 0, 0, 0, 0, RGBA, 0, 64, 0, 0, 0, 0];
        unsafe { vnhost_video_ready(id, buf, layout.as_ptr(), 1) };
        assert!(!exists(id));
    }

    #[test]
    fn close_waits_for_the_guest_reading() {
        let id = open(vnv()).unwrap();
        with(|m| m.get_mut(&id).unwrap().slots[0].reading += 1);
        close(id);
        assert!(exists(id), "dropped while the guest reads a slot");
        // the page is done writing some other slot: the read still holds the stream
        with(|m| release(m, id));
        assert!(exists(id));
        let mut px = [0u8; 4];
        assert!(!unsafe { frame(id, 0, px.as_mut_ptr(), 4, 1, 1, Out::Rgb) }, "a closed stream gives no frames");
        with(|m| {
            m.get_mut(&id).unwrap().slots[0].reading -= 1;
            release(m, id);
        });
        assert!(!exists(id));
    }

    #[test]
    fn two_readers_of_one_slot() {
        // one guest thread converts slot 0 while another reads the same frame and finishes:
        // the slot stays taken until the first one is done too (VideoSlotsFlag.cfg's trace)
        let id = open(vnv()).unwrap();
        let mut size = 16 * 16 * 4;
        let buf = unsafe { vnhost_video_begin(id, 0, &mut size) };
        let layout = [RGBA, 0, 64, 0, 0, 0, 0, RGBA, 0, 64, 0, 0, 0, 0];
        unsafe { vnhost_video_ready(id, buf, layout.as_ptr(), 1) };
        with(|m| m.get_mut(&id).unwrap().slots[0].reading += 1);
        let mut px = vec![0u8; 16 * 16 * 4];
        assert!(unsafe { frame(id, 0, px.as_mut_ptr(), 64, 16, 16, Out::Rgb) });
        assert_eq!(with(|m| m.get(&id).unwrap().slots[0].reading), 1, "the first reader still holds it");
        for n in 1..4 {
            let b = unsafe { vnhost_video_begin(id, n, &mut size) };
            assert_ne!(b, buf, "the page must not write the slot a guest reads");
            if b != 0 {
                unsafe { vnhost_video_ready(id, b, layout.as_ptr(), 1) };
            }
        }
        with(|m| m.get_mut(&id).unwrap().slots[0].reading -= 1);
        close(id);
        assert!(!exists(id));
    }

    #[test]
    fn reads_and_closes_from_threads() {
        // the guest reads frames while another thread closes the stream: every read finishes on
        // a live buffer (under Miri or ASan a use after free shows here)
        for _ in 0..50 {
            let id = open(vnv()).unwrap();
            let mut size = 16 * 16 * 4;
            let buf = unsafe { vnhost_video_begin(id, 0, &mut size) };
            let layout = [RGBA, 0, 64, 0, 0, 0, 0, RGBA, 0, 64, 0, 0, 0, 0];
            unsafe { vnhost_video_ready(id, buf, layout.as_ptr(), 1) };
            let reader = std::thread::spawn(move || {
                let mut px = vec![0u8; 16 * 16 * 4];
                for _ in 0..20 {
                    unsafe { frame(id, 0, px.as_mut_ptr(), 64, 16, 16, Out::Rgb) };
                }
            });
            close(id);
            reader.join().unwrap();
            assert!(!exists(id));
        }
    }

    fn header(w: u32, h: u32, tracks: u32, len: usize) -> Vec<u8> {
        let mut d = b"VNV1".to_vec();
        for v in [w, h, tracks, 0] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.resize(len, 0);
        d
    }

    /// A raw I420 movie of `n` frames, frame k all Y = 16 + k, Cb Cr given.
    fn raw(w: u32, h: u32, n: u32, cb: u8, cr: u8) -> Vec<u8> {
        let (pw, ph) = (w.next_multiple_of(2) as usize, h.next_multiple_of(2) as usize);
        let frames = (0..n)
            .map(|k| {
                let mut f = vec![16u8.wrapping_add(k as u8); pw * ph];
                f.extend(std::iter::repeat_n(cb, pw / 2 * (ph / 2)));
                f.extend(std::iter::repeat_n(cr, pw / 2 * (ph / 2)));
                (f, true)
            })
            .collect();
        crate::vnv::write(w, h, &[("raw-i420".into(), frames)], None)
    }

    #[test]
    fn raw_frames_need_no_page() {
        let d = raw(5, 3, 4, 128, 128);
        let id = open(d.clone()).unwrap();
        assert!(decodable(id));
        // (the page's copy of a frame: from memory as from a file)
        let mut size = 0;
        assert_ne!(unsafe { vnhost_video_chunk(id, 0, 1, &mut size) }, 0);
        let (off, sz, _) = info(&d).unwrap().tracks[0].1[1];
        assert_eq!(&CHUNK.lock().unwrap()[..], &d[off as usize..(off + sz) as usize]);
        assert_eq!(unsafe { vnhost_video_chunk(id, 0, 4, &mut size) }, 0, "no frame 4");
        assert_eq!(unsafe { vnhost_video_chunk(id, 1, 0, &mut size) }, 0, "no track 1");
        let mut px = vec![0xeeu8; 6 * 4 * 3];
        for n in [0, 3, 2, 9, -1] {
            assert!(unsafe { frame(id, n, px.as_mut_ptr(), 24, 5, 3, Out::Ycc) });
            let want = 16 + n.clamp(0, 3) as u8;
            for y in 0..3 {
                for x in 0..5 {
                    assert_eq!(px[y * 24 + 4 * x..][..4], [want, 128, 128, 255], "frame {n} at {x},{y}");
                }
                assert_eq!(px[y * 24 + 20..y * 24 + 24], [0xee; 4], "past the row");
            }
        }
        close(id);
        assert!(!unsafe { frame(id, 0, px.as_mut_ptr(), 24, 5, 3, Out::Ycc) });
        // short frames, or raw mixed with another track: refused
        let mut d = raw(4, 4, 1, 128, 128);
        let i = info(&d).unwrap();
        let (off, _, _) = i.tracks[0].1[0];
        d = crate::vnv::write(4, 4, &[("raw-i420".into(), vec![(d[off as usize..off as usize + 20].to_vec(), true)])], None);
        assert!(open(d).is_none());
        let two = crate::vnv::write(4, 4, &[("raw-i420".into(), vec![(vec![0; 24], true)]), ("vp09".into(), vec![(vec![0; 3], true)])], None);
        assert!(open(two).is_none());
    }

    /// A `.vnv` written to a file of its own (removed when dropped).
    struct TempVnv(std::path::PathBuf);
    impl TempVnv {
        fn new(d: &[u8]) -> TempVnv {
            static N: AtomicI32 = AtomicI32::new(0);
            let p = std::env::temp_dir().join(format!("kura-video-{}-{}.vnv", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            std::fs::write(&p, d).unwrap();
            TempVnv(p)
        }
    }
    impl Drop for TempVnv {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[test]
    fn raw_frames_from_a_file() {
        let d = raw(5, 3, 300, 128, 128);
        let f = TempVnv::new(&d);
        let (id, i) = open_file(&f.0).unwrap();
        assert_eq!(i, info(&d).unwrap());
        let mut px = vec![0u8; 6 * 4 * 3];
        // (in order, a jump far ahead, back to the start, past the end)
        for n in [0, 1, 2, 40, 41, 250, 3, 0, 299, 400, -1] {
            assert!(unsafe { frame(id, n, px.as_mut_ptr(), 24, 5, 3, Out::Ycc) }, "frame {n}");
            assert_eq!(px[..4], [16u8.wrapping_add(n.clamp(0, 299) as u8), 128, 128, 255], "frame {n}");
        }
        close(id);
        assert!(open_file(std::env::temp_dir().join("kura-video-none.vnv")).is_none());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(32))]
        /// A file's stream has, after any request, every frame its page may decode next (each
        /// track's from its key frame before the request to READ_LOW frames past it), byte for
        /// byte the file's, and holds little more than READ_HIGH frames past it.
        #[test]
        fn file_frames_around_the_request(
            sizes in proptest::collection::vec((1usize..60, proptest::bool::weighted(0.05)), 1..500),
            alpha: bool,
            asks in proptest::collection::vec(0usize..600, 1..12),
        ) {
            let track = |seed: u8| -> Vec<(Vec<u8>, bool)> {
                sizes.iter().enumerate().map(|(k, &(n, key))| ((0..n).map(|j| (k * 7 + j) as u8 ^ seed).collect(), key || k == 0)).collect()
            };
            let mut tracks = vec![("vp09.00.31.08".to_string(), track(0))];
            if alpha {
                tracks.push(("vp09.00.31.08".to_string(), track(0x5a)));
            }
            let d = crate::vnv::write(32, 16, &tracks, None);
            let f = TempVnv::new(&d);
            let (id, i) = open_file(&f.0).unwrap();
            // (closed however the case ends: a failing case must not leave streams to the others)
            struct Closing(u32);
            impl Drop for Closing {
                fn drop(&mut self) {
                    close(self.0);
                }
            }
            let _c = Closing(id);
            let last = sizes.len() - 1;
            let mut most = 0usize;
            for &ask in &asks {
                fill(id, ask as i32);
                let n = ask.min(last);
                for (t, (_, frames)) in i.tracks.iter().enumerate() {
                    let k = (0..=n).rev().find(|&k| frames[k].2).unwrap();
                    for j in k..=(n + READ_LOW).min(last) {
                        let mut size = 0u32;
                        let p = unsafe { vnhost_video_chunk(id, t as u32, j as u32, &mut size) };
                        proptest::prop_assert!(p != 0, "track {} frame {} after asking {}", t, j, ask);
                        // (the pointer is a wasm address: the copy itself is looked at)
                        let got = CHUNK.lock().unwrap().clone();
                        proptest::prop_assert_eq!(size as usize, got.len());
                        proptest::prop_assert_eq!(&got[..], &tracks[t].1[j].0[..]);
                    }
                }
                let held: usize = with(|m| match &m[&id].src {
                    Source::File { wins, .. } => wins.iter().map(|w| w.1.len()).sum(),
                    Source::Mem(_) => unreachable!(),
                });
                // (a track's run is one read: from a key frame to READ_HIGH frames past a request,
                // this one's or an earlier one's that still covers this)
                most = most.max(i.tracks.iter().map(|(_, fr)| {
                    let k = (0..=n).rev().find(|&k| fr[k].2).unwrap();
                    fr[k..=(n + READ_HIGH).min(last)].iter().map(|f| f.1 as usize).sum::<usize>()
                }).sum());
                proptest::prop_assert!(held <= most, "{} bytes held, at most {}", held, most);
            }
        }
    }

    #[test]
    fn video_range_bt601() {
        // black, white and the 75% bars (ITU-R BT.601 video levels -> RGB)
        assert_eq!(ycc601_to_rgb(16, 128, 128), (0, 0, 0));
        assert_eq!(ycc601_to_rgb(235, 128, 128), (255, 255, 255));
        assert_eq!(ycc601_to_rgb(0, 128, 128), (0, 0, 0));
        assert_eq!(ycc601_to_rgb(255, 128, 128), (255, 255, 255));
        for (ycc, rgb) in [((65, 100, 212), (191, 0, 0)), ((112, 72, 58), (0, 191, 0)), ((35, 212, 114), (0, 0, 191)), ((162, 44, 142), (191, 191, 0))] {
            let got = ycc601_to_rgb(ycc.0, ycc.1, ycc.2);
            let d = |a: u8, b: u8| (a as i32 - b as i32).abs();
            assert!(d(got.0, rgb.0) <= 2 && d(got.1, rgb.1) <= 2 && d(got.2, rgb.2) <= 2, "{ycc:?} -> {got:?}, want {rgb:?}");
        }
        let id = open(raw(2, 2, 1, 128, 128)).unwrap();
        let mut px = [0u8; 16];
        assert!(unsafe { frame(id, 0, px.as_mut_ptr(), 8, 2, 2, Out::Rgb601) });
        assert_eq!(px[..4], [0, 0, 0, 255]);
        close(id);
    }

    #[test]
    fn open_checks_the_header() {
        // (each track: an empty codec name and no frames)
        for (w, h, t, len) in [(16, 16, 1, 24), (8192, 8192, 2, 32), (1, 1, 1, 24)] {
            let id = open(header(w, h, t, len)).unwrap_or_else(|| panic!("{w}x{h} {t}"));
            close(id);
        }
        for (w, h, t, len) in [
            (16, 16, 1, 15),
            (16, 16, 1, 16),
            (16, 16, 1, 20),
            (16, 16, 2, 24),
            (0, 16, 1, 16),
            (16, 0, 1, 16),
            (8193, 16, 1, 16),
            (16, 8193, 1, 16),
            (16, 16, 0, 16),
            (16, 16, 3, 16),
        ] {
            assert!(open(header(w, h, t, len)).is_none(), "{w}x{h} {t} {len}");
        }
        let mut d = vnv();
        d[0] = b'X';
        assert!(open(d).is_none());
    }

    #[test]
    fn a_fresh_stream_holds_nothing() {
        // (the only test that lists streams)
        assert!(!available(), "no page has listed streams yet");
        let id = open(vnv()).unwrap();
        for n in [0, 1] {
            assert_eq!(vnhost_video_has(id, n), 0, "frame {n}");
        }
        assert!(!failed(id));
        // the page sees no request yet; frame() rings the doorbell with one
        let row = |id: u32| {
            let mut out = vec![0u32; 6 * 64];
            let n = unsafe { vnhost_video_list(out.as_mut_ptr(), 64) } as usize;
            out[..n * 6]
                .chunks(6)
                .find(|r| r[0] == id)
                .map(|r| r.to_vec())
                .unwrap()
        };
        assert_eq!(row(id)[3..], [u32::MAX; 3]);
        assert!(available(), "listing streams is the page being there");
        let rung = DOORBELL.load(Ordering::SeqCst);
        let mut px = [0u8; 4];
        // nothing decoded: after the first frame's wait the guest goes on without one
        assert!(!unsafe { frame(id, 2, px.as_mut_ptr(), 4, 1, 1, Out::Rgb) });
        assert!(
            DOORBELL.load(Ordering::SeqCst) > rung,
            "a new request rings the page"
        );
        assert_eq!(row(id)[3], 2);
        close(id);
    }

    #[test]
    fn a_failed_stream_gives_no_frames() {
        let id = open(vnv()).unwrap();
        let mut size = 16 * 16 * 4;
        let buf = unsafe { vnhost_video_begin(id, 0, &mut size) };
        let layout = [RGBA, 0, 64, 0, 0, 0, 0, RGBA, 0, 64, 0, 0, 0, 0];
        unsafe { vnhost_video_ready(id, buf, layout.as_ptr(), 1) };
        assert_eq!((vnhost_video_has(id, 0), vnhost_video_has(id, 1)), (1, 0));
        assert_eq!(vnhost_video_doorbell(), DOORBELL.as_ptr() as u32);
        vnhost_video_fail(id);
        assert!(failed(id) && !failed(u32::MAX));
        let mut px = vec![0u8; 16 * 16 * 4];
        assert!(!unsafe { frame(id, 0, px.as_mut_ptr(), 64, 16, 16, Out::Rgb) });
        close(id);
    }
    #[test]
    fn bt601_both_ways() {
        assert_eq!(ycc_to_rgb(100, 200, 50), (0, 130, 227));
        assert_eq!(ycc_to_rgb(50, 60, 200), (150, 21, 0));
        assert_eq!(ycc_to_rgb(16, 128, 128), (16, 16, 16));
        assert_eq!(ycc_to_rgb(200, 90, 170), (255, 183, 132));
        assert_eq!(rgb_to_ycc(255, 0, 0), (76, 85, 255));
        assert_eq!(rgb_to_ycc(10, 200, 30), (124, 75, 47));
        assert_eq!(rgb_to_ycc(0, 0, 255), (29, 255, 107));
    }

    /// A 4x4 slot in `fmt` (planes padded: strides wider than the rows) with an alpha track
    /// in `alpha` (None: no alpha track) and the guest's pixels of it (pitch 24).
    fn slot_pixels(fmt: u32, alpha: Option<u32>, out: Out) -> (Vec<u8>, Vec<u8>) {
        let mut buf: Vec<u8> = (0..200u32).map(|i| (i * 37 % 251) as u8).collect();
        let color = match fmt {
            I420 => [I420, 0, 5, 20, 3, 30, 3],
            NV12 => [NV12, 0, 5, 20, 6, 0, 0],
            _ => [RGBA, 0, 18, 0, 0, 0, 0],
        };
        let a = match alpha {
            Some(RGBA) => [RGBA, 100, 17, 0, 0, 0, 0],
            Some(f) => [f, 100, 6, 0, 0, 0, 0],
            None => [0; 7],
        };
        buf[0] = 0; // (a pixel that clamps)
        let mut dst = vec![0xeeu8; 24 * 4];
        unsafe {
            convert(
                buf.as_ptr(),
                &[color, a],
                1 + alpha.is_some() as u32,
                4,
                4,
                dst.as_mut_ptr(),
                24,
                4,
                4,
                out,
            )
        };
        (buf, dst)
    }

    proptest::proptest! {
        /// The table path gives the general loop's bytes: any planes, sizes (odd ones too),
        /// strides, I420 / NV12, both RGB outputs (the truncating one takes the general loop).
        #[test]
        fn to_rgb_as_any(seed in proptest::prelude::any::<u64>(), w in 1u32..40, h in 1u32..12, pad in 0u32..5, nv12 in proptest::prelude::any::<bool>(), r601 in proptest::prelude::any::<bool>()) {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let ys = w + pad;
            let layout = if nv12 { [NV12, 0, ys, ys * h, 2 * cw + pad, 0, 0] } else { [I420, 0, ys, ys * h, cw + pad, ys * h + (cw + pad) * ch, cw + pad] };
            let n = (ys * h + 2 * (cw + pad) * ch) as usize;
            let mut x = seed | 1;
            let buf: Vec<u8> = (0..n).map(|_| { x ^= x << 13; x ^= x >> 7; x ^= x << 17; x as u8 }).collect();
            let out = if r601 { Out::Rgb601 } else { Out::Rgb };
            let pitch = 4 * w as usize + 8;
            let (mut a, mut b) = (vec![0xeeu8; pitch * h as usize], vec![0xeeu8; pitch * h as usize]);
            unsafe {
                convert(buf.as_ptr(), &[layout, [0; 7]], 1, w, h, a.as_mut_ptr(), pitch, w, h, out);
                convert_any(buf.as_ptr(), &[layout, [0; 7]], 1, w, h, b.as_mut_ptr(), pitch, w, h, out);
            }
            // (within 1 where a sum is a hair from a half: terms_as_the_function)
            proptest::prop_assert!(a.iter().zip(&b).all(|(x, y)| x.abs_diff(*y) <= 1));
        }
    }

    /// Every Y, Cb, Cr: the fixed-point tables as ycc601_to_rgb, but for a sum within a hair of
    /// a half (where f64's own rounding of the terms decides): those within 1, and few.
    #[test]
    fn terms_as_the_function() {
        let mut off = 0;
        for cb in 0..=255u8 {
            for cr in 0..=255u8 {
                // (a 256x1 NV12 picture of every Y, its chroma row that pair repeated)
                let mut b: Vec<u8> = (0..=255u8).collect();
                b.extend((0..128).flat_map(|_| [cb, cr]));
                let mut d = vec![0u8; 1024];
                unsafe { to_rgb(b.as_ptr(), &[NV12, 0, 256, 256, 256, 0, 0], d.as_mut_ptr(), 1024, 256, 1, Out::Rgb601) };
                for y in 0..256 {
                    let (r, g, bl) = ycc601_to_rgb(y as u8, cb, cr);
                    for (got, want) in d[4 * y..4 * y + 3].iter().zip([bl, g, r]) {
                        assert!(got.abs_diff(want) <= 1, "{y} {cb} {cr}: {got} for {want}");
                        off += (*got != want) as u32;
                    }
                }
            }
        }
        eprintln!("{off} of {} bytes off by 1", 3 << 24);
        assert!(off < 3 << 14, "{off}");
    }

    /// (cargo test -p kura --release -- --ignored --nocapture rgb_speed)
    #[test]
    #[ignore]
    fn rgb_speed() {
        let (w, h) = (1280u32, 720u32);
        let buf: Vec<u8> = (0..w * h * 3 / 2).map(|i| (i * 37 % 251) as u8).collect();
        let layout = [[I420, 0, w, w * h, w / 2, w * h * 5 / 4, w / 2], [0; 7]];
        let mut d = vec![0u8; (4 * w * h) as usize];
        for (name, f) in [("table", convert as unsafe fn(_, _, _, _, _, _, _, _, _, _)), ("general", convert_any)] {
            let t = Instant::now();
            for _ in 0..20 {
                unsafe { f(buf.as_ptr(), &layout, 1, w, h, d.as_mut_ptr(), 4 * w as usize, w, h, Out::Rgb601) };
            }
            eprintln!("{name}: {:.2} ms a 1280x720 frame", t.elapsed().as_secs_f64() * 1000.0 / 20.0);
        }
    }

    #[test]
    fn convert_planes() {
        for fmt in [I420, NV12, RGBA] {
            for alpha in [None, Some(I420), Some(RGBA)] {
                for out in [Out::Ycc, Out::Rgb, Out::Rgb601] {
                    let (b, dst) = slot_pixels(fmt, alpha, out);
                    for y in 0..4 {
                        for x in 0..4 {
                            let ycc = match fmt {
                                I420 => (
                                    b[y * 5 + x],
                                    b[20 + y / 2 * 3 + x / 2],
                                    b[30 + y / 2 * 3 + x / 2],
                                ),
                                NV12 => (
                                    b[y * 5 + x],
                                    b[20 + y / 2 * 6 + x / 2 * 2],
                                    b[21 + y / 2 * 6 + x / 2 * 2],
                                ),
                                _ => {
                                    let q = y * 18 + x * 4;
                                    rgb_to_ycc(b[q], b[q + 1], b[q + 2])
                                }
                            };
                            let al = match alpha {
                                None => 255,
                                Some(RGBA) => b[100 + y * 17 + x * 4],
                                Some(_) => b[100 + y * 6 + x],
                            };
                            let want = match out {
                                Out::Ycc => [ycc.0, ycc.1, ycc.2, al],
                                Out::Rgb => {
                                    let (r, g, bl) = ycc_to_rgb(ycc.0, ycc.1, ycc.2);
                                    [bl, g, r, 255]
                                }
                                Out::Rgb601 => {
                                    let (r, g, bl) = ycc601_to_rgb(ycc.0, ycc.1, ycc.2);
                                    [bl, g, r, 255]
                                }
                            };
                            let o = y * 24 + x * 4;
                            assert_eq!(dst[o..o + 4], want, "fmt {fmt} alpha {alpha:?} at {x},{y}");
                        }
                        assert_eq!(dst[y * 24 + 16..y * 24 + 24], [0xee; 8], "past the row");
                    }
                }
            }
        }
    }
}
