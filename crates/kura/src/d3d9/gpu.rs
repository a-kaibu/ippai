//! The commands the page's WebGPU executor (web/src/d3d9.ts) runs, in batches.
//!
//! A batch is a list of u32 words (the commands) and a data block the commands point into
//! (vertices, indices, shader constants: the page writes it to one GPU buffer before it runs the
//! batch's draws; texture pixels and shader text). Guest threads append to the open batch; a
//! Present closes it. The page takes closed batches with `vnhost_d3d9_poll` every frame.
//!
//! Without a page (node) nothing ever polls, and batches are dropped at each Present.

use std::collections::{HashMap, VecDeque};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Milliseconds by Date.now(): the clock all threads share (performance.now() differs per worker).
fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0)
}

pub const CREATE_TEXTURE: u32 = 1; // id w h levels format usage
pub const DESTROY_TEXTURE: u32 = 2; // id
pub const WRITE_TEXTURE: u32 = 3; // id level x y w h bytes_per_row blob_offset
pub const CREATE_SHADER: u32 = 4; // id blob_offset len (WGSL, UTF-8)
pub const CREATE_PIPELINE: u32 = 5; // see Pipeline::encode
pub const SET_TARGET: u32 = 6; // color_tex color_level depth_tex (0: none)
pub const CLEAR: u32 = 7; // flags color(rgba f32 x4) z stencil x y w h (w=0: whole target)
pub const DRAW: u32 = 8; // see Device::draw (its streams as n words of (buffer, offset) pairs, buffer DATA the data block, INDEX_PAIR | buffer the indices'; ends with: hash of what it reads, x0 y0 x1 y1 it draws to or u32::MAX)
pub const PRESENT: u32 = 9; // tex l t r b (screen rect the frame goes to) src_w src_h, or the part shown: l|t<<16 r|b<<16 (render/d3d.rs present_src) hwnd
pub const COPY: u32 = 10; // src_tex src_level sx sy sw sh dst_tex dst_level dx dy dw dh filter
pub const READBACK: u32 = 11; // request tex level x y w h dst_ptr pitch
pub const FILL: u32 = 12; // tex level x y w h color(bgra u32)
pub const WRITE_TEXTURE_MEM: u32 = 13; // id level x y w h pitch ptr: rows straight from guest memory
// Direct3D 11 (crates/kura/src/d3d11)
pub const CREATE_BUFFER: u32 = 20; // id size
pub const DESTROY_BUFFER: u32 = 21; // id
pub const WRITE_BUFFER: u32 = 22; // id offset blob_offset len
/// In WRITE_BUFFER's offset: the bytes are ones no draw before reads (written without ending the pass).
pub const WRITE_UNUSED: u32 = 0x8000_0000;
pub const CREATE_PIPELINE2: u32 = 23; // id len key[len] (see d3d11::draw)
pub const SET_TARGETS2: u32 = 24; // n (tex level layer)*n dtex dlevel dlayer
pub const DRAW2: u32 = 25; // see d3d11::draw (ends with the hash of what it reads from its buffers, 0: unknown)
pub const CLEAR_VIEW: u32 = 26; // tex level layer flags c0 c1 c2 c3 depth stencil
pub const GEN_MIPS: u32 = 27; // tex
pub const CREATE_TEXTURE2: u32 = 28; // id w h layers_or_depth mips format dim (0 2d, 1 3d, 2 1d; | 0x100 UAV, | 0x200 multisampled)
pub const WRITE_TEXTURE2: u32 = 29; // id level layer_or_z x y w h bytes_per_row blob_offset
pub const PRESENT_FLIP: u32 = 30; // as PRESENT, the texture bottom row first (OpenGL)
pub const DISPATCH: u32 = 31; // shader x y z n entries[n*8] (see d3d11::dispatch)
pub const RESOLVE: u32 = 32; // dst dst_level dst_layer src src_layer: ResolveSubresource (src multisampled, its level 0)

/// Texture formats on the page.
pub const FMT_BGRA8: u32 = 0;
pub const FMT_DEPTH24S8: u32 = 1;
pub const FMT_RGBA16F: u32 = 2;
pub const FMT_RGBA32F: u32 = 22; // (the page format list's rgba32float: halves without float32-filterable)

#[derive(Default)]
pub struct Batch {
    pub cmds: Vec<u32>,
    /// what goes into the page's GPU buffer: vertices, indices, uniforms
    pub data: Vec<u8>,
    /// the rest (texture pixels, shader text)
    pub blob: Vec<u8>,
    presents: u32,
    gen: u64,
    /// where its WRITE_TEXTURE_MEM commands are in `cmds`
    mems: Vec<usize>,
}

impl Batch {
    pub fn cmd(&mut self, words: &[u32]) {
        if matches!(words.first(), Some(&PRESENT) | Some(&PRESENT_FLIP)) {
            crate::display::presented();
        }
        self.cmds.extend_from_slice(words);
    }

    /// WRITE_TEXTURE_MEM id level x y w h pitch ptr (see `detach_mem`).
    pub fn cmd_mem(&mut self, words: &[u32; 9]) {
        self.mems.push(self.cmds.len());
        self.cmds.extend_from_slice(words);
    }

    /// Guest memory [lo, hi) is about to change: the WRITE_TEXTURE_MEM commands that read it get
    /// a copy of their rows in the blob (WRITE_TEXTURE with the same pitch) instead.
    fn detach(&mut self, lo: u32, hi: u32) {
        let mut i = 0;
        while i < self.mems.len() {
            let at = self.mems[i];
            let c = &self.cmds[at..at + 9];
            let (h, pitch, ptr) = (c[6], c[7], c[8]);
            let len = pitch as usize * h as usize;
            if (ptr as u64) < hi as u64 && ptr as u64 + len as u64 > lo as u64 {
                let off = self.blob(unsafe { std::slice::from_raw_parts(ptr as usize as *const u8, len) });
                self.cmds[at] = WRITE_TEXTURE;
                self.cmds[at + 8] = off;
                self.mems.swap_remove(i);
            } else {
                i += 1;
            }
        }
    }

    fn reads(&self, lo: u32, hi: u32) -> bool {
        self.mems.iter().any(|&at| {
            let c = &self.cmds[at..at + 9];
            (c[8] as u64) < hi as u64 && c[8] as u64 + c[7] as u64 * c[6] as u64 > lo as u64
        })
    }

    /// Appends data aligned to `align` bytes; returns its offset.
    pub fn data(&mut self, bytes: &[u8], align: usize) -> u32 {
        let at = self.data.len().next_multiple_of(align);
        self.data.resize(at, 0);
        self.data.extend_from_slice(bytes);
        at as u32
    }

    /// Appends to the blob (4-byte aligned); returns the offset.
    pub fn blob(&mut self, bytes: &[u8]) -> u32 {
        let at = self.blob.len().next_multiple_of(4);
        self.blob.resize(at, 0);
        self.blob.extend_from_slice(bytes);
        at as u32
    }

    /// Room for `len` bytes aligned to `align`; returns the offset.
    pub fn reserve(&mut self, len: usize, align: usize) -> u32 {
        let at = self.data.len().next_multiple_of(align);
        self.data.resize(at + len, 0);
        at as u32
    }
}

struct Queue {
    open: Batch,
    closed: VecDeque<Batch>,
    /// the batch the page is reading (kept until its next poll)
    reading: Option<Batch>,
    presented: u64,
    shown: u64,
    last_poll: Option<f64>,
    readbacks_done: Vec<u32>,
    /// readbacks the page is copying into guest memory right now
    readbacks_writing: Vec<u32>,
    /// readbacks whose waiter gave up: the page must not write their pixels any more (the
    /// guest may have freed that memory already)
    readbacks_dropped: Vec<u32>,
    gen: u64,
    /// batches up to this one are done (the page ran them, or nobody will)
    done: u64,
}

static QUEUE: Mutex<Option<Queue>> = Mutex::new(None);
static WAKE: Condvar = Condvar::new();

fn with<R>(f: impl FnOnce(&mut Queue) -> R) -> R {
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    f(queue(&mut g))
}

/// The queue, made at its first use (a Present or a wait before any command was recorded:
/// a back buffer that could not be made, a game's first call a Present).
fn queue(g: &mut Option<Queue>) -> &mut Queue {
    g.get_or_insert_with(|| Queue {
        open: Batch::default(),
        closed: VecDeque::new(),
        reading: None,
        presented: 0,
        shown: 0,
        last_poll: None,
        readbacks_done: Vec::new(),
        readbacks_writing: Vec::new(),
        readbacks_dropped: Vec::new(),
        gen: 0,
        done: 0,
    })
}

/// Tests: whether readback `id` was said to be done (and not taken by its waiter yet).
#[cfg(test)]
pub fn readback_is_done(id: u32) -> bool {
    with(|q| q.readbacks_done.contains(&id))
}

// Tests: the page's executor in the same thread, run where the guest would wait for it.
#[cfg(test)]
thread_local! {
    static PUMP: std::cell::RefCell<Option<Box<dyn FnMut()>>> = std::cell::RefCell::new(None);
}

/// (tests) Runs `f` as the page whenever the guest waits for it; None: no page. Starts from an
/// empty queue (what other tests left is dropped).
#[cfg(test)]
pub fn set_pump(f: Option<Box<dyn FnMut()>>) {
    *QUEUE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    // (a page from the start: the guest then waits for it where it must)
    if f.is_some() {
        with(|q| q.last_poll = Some(now()));
    }
    PUMP.with(|p| *p.borrow_mut() = f);
}

/// (tests) Lets the page run what was recorded so far.
#[cfg(test)]
pub fn pump() {
    flush_now();
    pump_page();
}

// (tests) The page is away: it neither polls nor runs anything (`set_away`).
#[cfg(test)]
thread_local! {
    static AWAY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// (tests) The page goes away (its last poll long ago, nothing run) or comes back.
#[cfg(test)]
pub fn set_away(away: bool) {
    AWAY.with(|a| a.set(away));
    with(|q| q.last_poll = Some(if away { now() - 10_000.0 } else { now() }));
}

/// (tests) The page takes what is closed.
#[cfg(test)]
fn pump_page() {
    if AWAY.with(|a| a.get()) {
        return;
    }
    let f = PUMP.with(|p| p.borrow_mut().take());
    if let Some(mut f) = f {
        f();
        PUMP.with(|p| *p.borrow_mut() = Some(f));
    }
}

/// (tests) The page: takes the next closed batch and runs `f` on it (as vnhost_d3d9_poll and
/// vnhost_d3d9_done, with host pointers that need not fit 32 bits). False: there was none.
#[cfg(test)]
pub fn run_next(f: impl FnOnce(&[u32], &[u8], &[u8])) -> bool {
    let Some(b) = with(|q| {
        q.last_poll = Some(now());
        let b = q.closed.pop_front()?;
        q.shown += b.presents as u64;
        Some(b)
    }) else {
        return false;
    };
    f(&b.cmds, &b.data, &b.blob);
    with(|q| q.done = q.done.max(b.gen + 1));
    WAKE.notify_all();
    true
}

/// A batch whose blob grew past this is closed after the command that grew it: a game loading
/// its images before the first Present put them all in one batch (Artemis: a 1 GB blob, out of
/// memory).
const BLOB_MAX: usize = 64 << 20;

/// The most bytes of closed batches that wait for the page before the guest waits for it too.
/// Only a Present waited: a game loading its textures for seconds without one (a loading
/// screen, with the tab hidden or the page slow) queued 64 MiB batches without end, and what
/// the BLOB_MAX above kept out of one batch ran the 2 GB heap out through the queue.
pub(crate) static QUEUE_MAX: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(256 << 20);

/// The bytes of the closed batches.
fn queued(q: &Queue) -> usize {
    q.closed.iter().map(|b| b.blob.len() + b.data.len() + b.cmds.len() * 4).sum()
}

/// Waits while more than `QUEUE_MAX` bytes of closed batches wait for a page that has ever
/// polled (as `present` waits for a page two frames behind). Not while a state is taken.
fn wait_room() {
    let max = QUEUE_MAX.load(std::sync::atomic::Ordering::Relaxed);
    loop {
        #[cfg(test)]
        if with(|q| queued(q) > max && q.last_poll.is_some()) {
            pump_page();
        }
        let g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
        let Some(q) = g.as_ref() else { return };
        if queued(q) <= max || q.last_poll.is_none() || crate::snap::pausing() {
            return;
        }
        drop(WAKE.wait_timeout(g, Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner()));
    }
}

/// Whether it closed the open batch.
fn close_if_big(q: &mut Queue) -> bool {
    if q.open.blob.len() < BLOB_MAX || q.open.cmds.is_empty() {
        return false;
    }
    let mut b = std::mem::take(&mut q.open);
    b.gen = q.gen;
    q.gen += 1;
    if q.last_poll.is_none() {
        // node: nobody draws
        q.done = q.gen;
    } else {
        q.closed.push_back(b);
    }
    true
}

/// Records into the open batch.
pub fn record<R>(f: impl FnOnce(&mut Batch) -> R) -> R {
    let (r, closed) = with(|q| {
        let r = f(&mut q.open);
        (r, close_if_big(q))
    });
    if closed {
        wait_room();
    }
    r
}

/// Records into the open batch; also returns its generation (see `wait_done`).
pub fn record_gen<R>(f: impl FnOnce(&mut Batch) -> R) -> (R, u64) {
    let (r, closed) = with(|q| {
        let r = (f(&mut q.open), q.gen);
        (r, close_if_big(q))
    });
    if closed {
        wait_room();
    }
    r
}

fn page_attached(q: &Queue) -> bool {
    q.last_poll.is_some_and(|t| now() - t < 3000.0)
}

/// The open batch's generation and how many command words it has (where the next command goes)
pub fn batch_end() -> (u64, usize) {
    with(|q| (q.gen, q.open.cmds.len()))
}

/// Changes whenever a new batch opens (what the page knows of draw state is per batch).
pub fn batch_gen() -> u64 {
    with(|q| q.gen)
}

/// Closes the open batch (if it has anything), and waits for the page when too much is queued.
pub fn flush() {
    flush_now();
    wait_room();
}

fn flush_now() {
    with(|q| {
        if !q.open.cmds.is_empty() {
            let mut b = std::mem::take(&mut q.open);
            b.gen = q.gen;
            q.closed.push_back(b);
            q.gen += 1;
        }
    });
}

/// After a Present was recorded: closes the batch, then waits while the page is more than two
/// frames behind (as a vsync would; without a page, drops what nobody will draw). A page that
/// polls but is slow holds a frame at most 250 ms; a page that stopped polling (its tab hidden:
/// no animation frames) holds it until it polls again. Returning then, as before, let a game
/// in a background tab spin at a whole core and pile up frames nobody drew: 金恋 ran out of
/// memory after 6 minutes, and showed white for minutes after while the page caught up.
/// (`hwnd`: the window presented to; each window is paced on its own)
pub fn present(hwnd: u32) {
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    {
        let q = queue(&mut g);
        q.open.presents += 1;
        q.presented += 1;
        let mut b = std::mem::take(&mut q.open);
        b.gen = q.gen;
        q.gen += 1;
        q.closed.push_back(b);
        if q.last_poll.is_none() {
            // node: nobody draws
            q.closed.clear();
            q.shown = q.presented;
            q.done = q.gen;
            return;
        }
    }
    // a display's rate at most (games that present in a busy loop would spend the CPU on
    // frames nobody sees)
    drop(g);
    pace(hwnd);
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let deadline = now() + 250.0;
    loop {
        let q = queue(&mut g);
        // (a state being taken: the game must reach its pause, not wait here)
        if q.presented <= q.shown + 2 || (now() >= deadline && page_attached(q)) || crate::snap::pausing() {
            return;
        }
        g = WAKE.wait_timeout(g, Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// Waits until the batch recorded in generation `gen` (see `batch_gen`) is done: guest memory
/// its commands read (WRITE_TEXTURE_MEM) may change after that.
pub fn wait_done(gen: u64) {
    #[cfg(test)]
    pump();
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let deadline = now() + 1000.0;
    loop {
        let q = queue(&mut g);
        if q.done > gen || q.last_poll.is_none() || !page_attached(q) || now() >= deadline {
            if q.last_poll.is_none() {
                q.done = q.done.max(gen + 1);
            }
            return;
        }
        if q.gen == gen && !q.open.cmds.is_empty() {
            // still open: close it
            let mut b = std::mem::take(&mut q.open);
            b.gen = q.gen;
            q.closed.push_back(b);
            q.gen += 1;
        }
        g = WAKE.wait_timeout(g, Duration::from_millis(5)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// Guest memory [lo, hi) is about to change: the batches not taken by the page yet copy what
/// their WRITE_TEXTURE_MEM commands read of it (a copy instead of waiting up to a frame for the
/// page). Returns the generation to wait for when the page is running such a batch right now.
pub fn detach_mem(lo: u32, hi: u32) -> Option<u64> {
    with(|q| {
        for b in q.closed.iter_mut() {
            b.detach(lo, hi);
        }
        q.open.detach(lo, hi);
        let done = q.done;
        q.reading.as_ref().filter(|b| b.gen >= done && b.reads(lo, hi)).map(|b| b.gen)
    })
}

/// Whether the batch of generation `gen` is done (see `wait_done`).
pub fn is_done(gen: u64) -> bool {
    with(|q| q.done > gen || q.last_poll.is_none())
}

/// The page ran the batch it took last.
#[no_mangle]
pub extern "C" fn vnhost_d3d9_done() {
    with(|q| {
        if let Some(b) = &q.reading {
            q.done = q.done.max(b.gen + 1);
        }
    });
    WAKE.notify_all();
}

/// Waits until the page has done readback `id` (or gave up).
pub fn wait_readback(id: u32) -> bool {
    flush();
    #[cfg(test)]
    pump();
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    // (10 s without the page finishing a batch: a slow GPU with frames queued before the
    // readback, or shaders compiling, is still making progress)
    let mut deadline = now() + 10000.0;
    let mut done = g.as_ref().map_or(0, |q| q.done);
    loop {
        let q = queue(&mut g);
        if q.done != done {
            done = q.done;
            deadline = now() + 10000.0;
        }
        if let Some(i) = q.readbacks_done.iter().position(|&r| r == id) {
            q.readbacks_done.swap_remove(i);
            return true;
        }
        // giving up (the page is gone or stalled): the page must not write the pixels later,
        // into memory the guest may have freed and reused by then
        if (now() >= deadline || !page_attached(q)) && !q.readbacks_writing.contains(&id) {
            eprintln!("kura: readback {id:#x} given up ({})", if page_attached(q) { "timed out" } else { "page not polling" });
            q.readbacks_dropped.push(id);
            return false;
        }
        g = WAKE.wait_timeout(g, Duration::from_millis(5)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// The page: takes the next closed batch. `out` gets [cmds ptr, cmd words, data ptr, data bytes,
/// blob ptr, blob bytes];
/// returns 0 when there is none. The batch stays valid until the next call.
#[no_mangle]
pub unsafe extern "C" fn vnhost_d3d9_poll(out: *mut u32) -> u32 {
    let out = std::slice::from_raw_parts_mut(out, 6);
    let r = with(|q| {
        q.last_poll = Some(now());
        q.reading = q.closed.pop_front();
        let Some(b) = &q.reading else { return 0 };
        super::stats::batch(b.data.len(), b.blob.len());
        q.shown += b.presents as u64;
        out[0] = b.cmds.as_ptr() as u32;
        out[1] = b.cmds.len() as u32;
        out[2] = b.data.as_ptr() as u32;
        out[3] = b.data.len() as u32;
        out[4] = b.blob.as_ptr() as u32;
        out[5] = b.blob.len() as u32;
        1
    });
    WAKE.notify_all();
    r
}

/// The page wrote the pixels of readback `id`.
#[no_mangle]
pub extern "C" fn vnhost_d3d9_readback_done(id: u32) {
    with(|q| {
        q.readbacks_writing.retain(|&r| r != id);
        if let Some(i) = q.readbacks_dropped.iter().position(|&r| r == id) {
            q.readbacks_dropped.swap_remove(i);
        } else {
            q.readbacks_done.push(id);
        }
    });
    WAKE.notify_all();
}

/// The page: whether it may now write readback `id`'s pixels into guest memory (0: its waiter
/// gave up, skip the copy). Until readback_done, the waiter keeps waiting.
#[no_mangle]
pub extern "C" fn vnhost_d3d9_readback_begin(id: u32) -> u32 {
    with(|q| {
        if q.readbacks_dropped.contains(&id) {
            0
        } else {
            q.readbacks_writing.push(id);
            1
        }
    })
}

fn pace(hwnd: u32) {
    if crate::nopace() {
        return; // (benchmarks: the page's rate only, tests/bench)
    }
    // (per window: a game with two windows, or two devices, presents each at 60 a second;
    // one clock for all gave them 30 each)
    static NEXT: Mutex<Option<HashMap<u32, f64>>> = Mutex::new(None);
    const FRAME: f64 = 1000.0 / 60.0;
    let t = now();
    let wait = pace_window(NEXT.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(HashMap::new), hwnd, t, FRAME);
    if wait > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(wait / 1000.0));
    }
}

/// How long a Present at `t` waits for the frame after `next` (and `next` moved on): late
/// frames don't make the next ones early, and no wait is longer than a frame. The clock is
/// the wall clock: stepped back (NTP, a phone waking) by a minute, the next Present slept a
/// minute.
/// `pace_wait` for window `hwnd` of `next`'s; windows not presented to for a second are
/// forgotten (a game making and closing windows).
fn pace_window(next: &mut HashMap<u32, f64>, hwnd: u32, t: f64, frame: f64) -> f64 {
    if next.len() >= 16 {
        next.retain(|_, n| *n + 1000.0 > t);
    }
    pace_wait(next.entry(hwnd).or_insert(0.0), t, frame)
}

fn pace_wait(next: &mut f64, t: f64, frame: f64) -> f64 {
    let at = if *next + frame < t || *next > t + frame { t } else { next.max(t) };
    *next = at + frame;
    at - t
}

#[cfg(test)]
mod pace_tests {
    use proptest::prelude::*;
    proptest! {
        /// Any clock, stepped back or forward: every wait is between 0 and a frame, and with a
        /// steady clock presents a frame apart do not wait at all.
        #[test]
        fn a_wait_is_a_frame_at_most(steps in proptest::collection::vec(-100_000.0f64..100_000.0, 1..200)) {
            let (mut next, mut t) = (0.0, 1e12);
            for d in steps {
                t += d;
                let w = super::pace_wait(&mut next, t, 16.0);
                prop_assert!((0.0..=16.0).contains(&w), "{}", w);
            }
            for _ in 0..10 {
                t += 16.0;
                prop_assert_eq!(super::pace_wait(&mut next, t, 16.0), 0.0);
            }
        }

        /// Windows are paced apart: `n` windows each presenting once a frame, each at its own
        /// time in the frame, never wait (one clock for all made each wait for the others); a
        /// window presenting twice in a frame waits; the table stays small however many
        /// windows come and go.
        #[test]
        fn windows_are_paced_apart(n in 1usize..6, frames in 2usize..40, offsets in proptest::collection::vec(0.0f64..16.0, 6), others in proptest::collection::vec(1000u32..100_000, 0..200)) {
            let mut next = std::collections::HashMap::new();
            let mut t = 1e9;
            for f in 0..frames {
                for w in 1..=n as u32 {
                    let at = t + offsets[w as usize - 1];
                    let wait = super::pace_window(&mut next, w, at, 16.0);
                    prop_assert!(f == 0 || wait == 0.0, "frame {} window {} waited {}", f, w, wait);
                }
                t += 16.0;
            }
            // (window 1 again at once: a second present in the frame waits)
            prop_assert!(super::pace_window(&mut next, 1, t - 16.0 + offsets[0] + 1.0, 16.0) > 0.0);
            for (k, w) in others.into_iter().enumerate() {
                super::pace_window(&mut next, w, t + k as f64 * 100.0, 16.0);
                prop_assert!(next.len() <= 16 + 1, "{} windows kept", next.len());
            }
        }
    }
}

#[cfg(test)]
mod hidden_tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
    use std::sync::Arc;

    fn queued() -> (usize, usize) {
        with(|q| (q.closed.len(), q.closed.iter().map(|b| b.blob.len()).sum()))
    }

    /// A Present, a wait for a batch and a wait for a readback before anything was ever
    /// recorded (no queue yet) go on as without a page; they used to abort the VM.
    #[test]
    fn waits_before_the_first_command_go_on() {
        let _g = super::super::trace_tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        for first in 0..3 {
            set_pump(None);
            assert!(QUEUE.lock().unwrap().is_none());
            match first {
                0 => present(1),
                1 => wait_done(3),
                _ => assert!(!wait_readback(7)),
            }
            present(1);
            wait_done(0);
            assert!(is_done(0));
            assert_eq!(with(|q| q.presented), 1 + (first == 0) as u64);
        }
    }

    /// A game presenting 1 MiB frames into a page that stopped polling (a hidden tab) waits
    /// in Present with at most 3 frames queued, however long the page is away, and goes on
    /// when it polls again; a page that polls but is slow holds a frame 250 ms at most.
    #[test]
    fn a_page_away_holds_the_game_not_its_frames() {
        let _g = super::super::trace_tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        set_pump(None);
        with(|q| q.last_poll = Some(now() - 10_000.0));
        let done = Arc::new(AtomicU32::new(0));
        let d = done.clone();
        let game = std::thread::spawn(move || {
            for _ in 0..40 {
                record(|b| {
                    b.cmd(&[0]);
                    b.blob.extend(std::iter::repeat_n(7u8, 1 << 20));
                });
                present(1);
                d.fetch_add(1, SeqCst);
            }
        });
        for _ in 0..10 {
            std::thread::sleep(Duration::from_millis(100));
            let (n, bytes) = queued();
            assert!(n <= 3 && bytes <= 3 << 20, "{n} frames, {bytes} bytes queued for a page away");
        }
        assert!(done.load(SeqCst) <= 3, "{} presents went through", done.load(SeqCst));
        // (a state taken meanwhile: the game is let go to reach its pause)
        let before = done.load(SeqCst);
        crate::snap::VNHOST_SNAP.pause.store(1, SeqCst);
        std::thread::sleep(Duration::from_millis(100));
        crate::snap::VNHOST_SNAP.pause.store(0, SeqCst);
        assert!(done.load(SeqCst) > before, "a pause did not let the game out of Present");
        // (the page back: it takes the frames, the game goes on to the end)
        while !game.is_finished() {
            while run_next(|_, _, _| {}) {}
            std::thread::sleep(Duration::from_millis(2));
        }
        game.join().unwrap();
        assert_eq!(done.load(SeqCst), 40);
        // (a page polling but not keeping up: Present goes on after 250 ms)
        let t = now();
        for _ in 0..4 {
            with(|q| q.last_poll = Some(now()));
            record(|b| b.cmd(&[0]));
            present(1);
        }
        assert!(now() - t < 2000.0, "a slow page held the game {} ms", now() - t);
        set_pump(None);
    }

    /// A game loading without a Present (1 MiB of texture, a flush, again and again) into a
    /// page that is away queues no more than the limit and one batch, waits, and when the page
    /// polls again every batch comes to it once and in order. With no page at all (node)
    /// nothing waits.
    #[test]
    fn loading_without_a_present_waits_for_the_page_too() {
        let _g = super::super::trace_tests::SERIAL.lock().unwrap_or_else(|e| e.into_inner());
        let was = QUEUE_MAX.swap(8 << 20, SeqCst);
        let load = |n: u32, done: Arc<AtomicU32>| {
            for k in 0..n {
                record(|b| {
                    b.cmd(&[k]);
                    b.blob.extend(std::iter::repeat_n(k as u8, 1 << 20));
                });
                flush();
                done.fetch_add(1, SeqCst);
            }
        };
        // (no page: all at once)
        set_pump(None);
        let done = Arc::new(AtomicU32::new(0));
        let t = now();
        load(30, done.clone());
        assert!(now() - t < 5000.0 && done.load(SeqCst) == 30);
        // (a page that is away)
        set_pump(None);
        with(|q| q.last_poll = Some(now() - 10_000.0));
        let done = Arc::new(AtomicU32::new(0));
        let d = done.clone();
        let game = std::thread::spawn(move || load(60, d));
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(50));
            let (n, bytes) = queued();
            assert!(bytes <= (8 << 20) + (1 << 20) + 4096, "{n} batches, {bytes} bytes queued for a page away");
        }
        let held = done.load(SeqCst);
        assert!((6..=9).contains(&held), "{held} flushes went through");
        // (the page back, slowly: the queue stays within the limit, the batches come in order)
        let mut got = Vec::new();
        while !game.is_finished() || queued().0 > 0 {
            let (_, bytes) = queued();
            assert!(bytes <= (8 << 20) + (1 << 20) + 4096, "{bytes} bytes queued");
            run_next(|c, _, blob| {
                assert!(blob.len() == 1 << 20 && blob.iter().all(|&x| x == c[0] as u8));
                got.push(c[0]);
            });
            std::thread::sleep(Duration::from_millis(1));
        }
        game.join().unwrap();
        assert_eq!(got, (0..60).collect::<Vec<u32>>());
        QUEUE_MAX.store(was, SeqCst);
        set_pump(None);
    }
}
