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
use std::sync::{Condvar, Mutex};
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

struct Stream {
    data: Vec<u8>,
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

/// Opens the `.vnv` in `data`; None when it is not one.
pub fn open(data: Vec<u8>) -> Option<u32> {
    if data.len() < 16 || &data[..4] != b"VNV1" {
        return None;
    }
    let u = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap());
    let (w, h, tracks) = (u(4), u(8), u(12));
    if w == 0 || h == 0 || w > 8192 || h > 8192 || !(1..=2).contains(&tracks) {
        return None;
    }
    static NEXT: AtomicI32 = AtomicI32::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed) as u32;
    let slots = (0..SLOTS)
        .map(|_| Slot { buf: Vec::new(), frame: -1, writing: false, reading: 0, layout: [[0; 7]; 2] })
        .collect();
    // (IPPAI_VIDEO_FAIL: every stream fails, as on a phone without decoders: tests the fallback)
    let failed = std::env::var_os("IPPAI_VIDEO_FAIL").is_some();
    with(|m| m.insert(id, Stream { data, w, h, tracks, want: -1, slots, closed: false, failed, shown: -1, opened: Instant::now() }));
    Some(id)
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
#[derive(Clone, Copy, PartialEq)]
pub enum Out {
    /// Y, Cb, Cr, alpha (255 without an alpha track): the game converts on the GPU
    Ycc,
    /// RGB by BT.601 (Theora's), alpha 255
    Rgb,
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
    let mut g = STREAMS.lock().unwrap_or_else(|e| e.into_inner());
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
unsafe fn convert(buf: *const u8, layout: &[[u32; 7]; 2], tracks: u32, sw: u32, _sh: u32, dst: *mut u8, pitch: usize, w: u32, h: u32, out: Out) {
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
                Out::Rgb => {
                    let (r, g, b) = ycc_to_rgb(yy, cb, cr);
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

/// Lists up to `max` streams into `out`, 6 words each: id, data ptr, data bytes, newest
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
                s.data.as_ptr() as u32,
                s.data.len() as u32,
                s.want as u32,
                held.iter().min().copied().unwrap_or(-1) as u32,
                held.iter().max().copied().unwrap_or(-1) as u32,
            ]);
            n += 1;
        }
        n as u32
    })
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
        for v in [16u32, 16, 1, 0] {
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
}
