//! The commands the page's WebGPU executor (web/src/d3d9.ts) runs, in batches.
//!
//! A batch is a list of u32 words (the commands) and a data block the commands point into
//! (vertices, indices, shader constants: the page writes it to one GPU buffer before it runs the
//! batch's draws; texture pixels and shader text). Guest threads append to the open batch; a
//! Present closes it. The page takes closed batches with `vnhost_d3d9_poll` every frame.
//!
//! Without a page (node) nothing ever polls, and batches are dropped at each Present.

use std::collections::VecDeque;
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
pub const DRAW: u32 = 8; // see Device::draw (ends with: hash of what it reads, x0 y0 x1 y1 it draws to or u32::MAX)
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
    f(g.get_or_insert_with(|| Queue {
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
    }))
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
    flush();
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

fn close_if_big(q: &mut Queue) {
    if q.open.blob.len() < BLOB_MAX || q.open.cmds.is_empty() {
        return;
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
}

/// Records into the open batch.
pub fn record<R>(f: impl FnOnce(&mut Batch) -> R) -> R {
    with(|q| {
        let r = f(&mut q.open);
        close_if_big(q);
        r
    })
}

/// Records into the open batch; also returns its generation (see `wait_done`).
pub fn record_gen<R>(f: impl FnOnce(&mut Batch) -> R) -> (R, u64) {
    with(|q| {
        let r = (f(&mut q.open), q.gen);
        close_if_big(q);
        r
    })
}

fn page_attached(q: &Queue) -> bool {
    q.last_poll.is_some_and(|t| now() - t < 3000.0)
}

/// Changes whenever a new batch opens (what the page knows of draw state is per batch).
pub fn batch_gen() -> u64 {
    with(|q| q.gen)
}

/// Closes the open batch (if it has anything).
pub fn flush() {
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
/// frames behind (as a vsync would; without a page, drops what nobody will draw).
pub fn present() {
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    {
        let q = g.as_mut().unwrap();
        q.open.presents += 1;
        q.presented += 1;
        let mut b = std::mem::take(&mut q.open);
        b.gen = q.gen;
        q.gen += 1;
        q.closed.push_back(b);
        if !page_attached(q) {
            if q.last_poll.is_none() {
                // node: nobody draws
                q.closed.clear();
                q.shown = q.presented;
                q.done = q.gen;
            }
            return;
        }
    }
    // a display's rate at most (games that present in a busy loop would spend the CPU on
    // frames nobody sees)
    drop(g);
    pace();
    let mut g = QUEUE.lock().unwrap_or_else(|e| e.into_inner());
    let deadline = now() + 250.0;
    loop {
        let q = g.as_ref().unwrap();
        if q.presented <= q.shown + 2 || now() >= deadline {
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
        let q = g.as_mut().unwrap();
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
        let q = g.as_mut().unwrap();
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

fn pace() {
    if crate::nopace() {
        return; // (benchmarks: the page's rate only, tests/bench)
    }
    static NEXT: Mutex<f64> = Mutex::new(0.0);
    const FRAME: f64 = 1000.0 / 60.0;
    let t = now();
    let wait = {
        let mut next = NEXT.lock().unwrap_or_else(|e| e.into_inner());
        // (late frames don't make the next ones early)
        let at = if *next + FRAME < t { t } else { next.max(t) };
        *next = at + FRAME;
        at - t
    };
    if wait > 0.0 {
        std::thread::sleep(Duration::from_secs_f64(wait / 1000.0));
    }
}
