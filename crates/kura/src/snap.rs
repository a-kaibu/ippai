//! Save states ("どこでもセーブ", docs/savestate.md): the whole running program at one moment,
//! restored later in a fresh page.
//!
//! A state is the linear memory itself (guest memory below 2 GiB, sake's and kura's heap,
//! statics and stacks above it), taken while every thread that changes it stands still, plus
//! what lives outside it, which the page cannot copy: the GPU's textures and buffers (the
//! drawing thread reads them back into memory first, `render::snap_dump`), and the threads'
//! wasm call stacks, which no one can copy. So threads stop only where nothing of theirs is
//! on the host's stack but what can be built again:
//!
//! * guest threads at the top level of their run loop (between translated blocks, or in an
//!   API call that waits and may be started again: sake's `snap` module), parked: [`park`];
//! * host threads (the mixer, input, movies, fetchers, the drawing thread) between two pieces
//!   of work: [`busy`] / [`idle`] around them;
//! * work handed to other threads (the JIT's compiler, saves) done: [`drain`].
//!
//! The page asks with [`Ctl::cmd`] = SAVE; this module's coordinator thread pauses everything,
//! has the GPU read back, lists the memory to copy ([`Ctl::ranges`]) and says READY; the page
//! copies (sake.js's memory, untouched while everything stands still) and says RESUME.
//!
//! Restoring: the page fills a new memory with the image, sets [`Ctl::restoring`] and starts
//! the module on it (emscripten's data initialization sees its flag set and leaves the memory
//! alone). sake's main sees `restoring` and starts every guest thread again where it parked
//! ([`restore_begin`] gives them); services started lazily see [`epoch`] change and start
//! again. The clocks the program reads continue from the moment of the save ([`Ctl::offset`]).

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering::*};
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// What the page and the restored module read and write (fixed offsets: web/src/savestate.ts).
#[repr(C)]
pub struct Ctl {
    /// "SNAP"
    pub magic: u32,
    /// layout version
    pub version: u32,
    /// page -> coordinator: CMD_*
    pub cmd: AtomicU32,
    /// coordinator -> page: ST_*
    pub state: AtomicU32,
    /// nonzero while threads are asked to stand still (the hot check of guest code)
    pub pause: AtomicU32,
    /// +1 at every restore
    pub epoch: AtomicU32,
    /// the page sets it in the image it restores
    pub restoring: AtomicU32,
    /// READY: [addr, len] pairs to copy (u32 each), and how many
    pub ranges: AtomicU32,
    pub nranges: AtomicU32,
    /// FAILED: why (UTF-8)
    pub msg: AtomicU32,
    pub msg_len: AtomicU32,
    /// the GPU's part: GPU_*
    pub gpu: AtomicU32,
    /// f64 bits: the raw monotonic and real-time clocks (ms) when the state was taken
    pub clock: [AtomicU64; 2],
    /// f64 bits: what the program's clocks add to the raw ones (ms)
    pub offset: [AtomicU64; 2],
    /// saves so far, restores so far (stats)
    pub saves: AtomicU32,
    pub restores: AtomicU32,
}

pub const CMD_NONE: u32 = 0;
pub const CMD_SAVE: u32 = 1;
pub const CMD_RESUME: u32 = 2;

pub const ST_RUNNING: u32 = 0;
pub const ST_PAUSING: u32 = 1;
pub const ST_READY: u32 = 2;
pub const ST_FAILED: u32 = 3;

pub const GPU_NONE: u32 = 0;
pub const GPU_DUMP: u32 = 1;
pub const GPU_DONE: u32 = 2;
pub const GPU_FAILED: u32 = 3;

#[no_mangle]
pub static VNHOST_SNAP: Ctl = Ctl {
    magic: u32::from_le_bytes(*b"SNAP"),
    version: 1,
    cmd: AtomicU32::new(0),
    state: AtomicU32::new(0),
    pause: AtomicU32::new(0),
    epoch: AtomicU32::new(0),
    restoring: AtomicU32::new(0),
    ranges: AtomicU32::new(0),
    nranges: AtomicU32::new(0),
    msg: AtomicU32::new(0),
    msg_len: AtomicU32::new(0),
    gpu: AtomicU32::new(0),
    clock: [AtomicU64::new(0), AtomicU64::new(0)],
    offset: [AtomicU64::new(0), AtomicU64::new(0)],
    saves: AtomicU32::new(0),
    restores: AtomicU32::new(0),
};

/// The page: where [`Ctl`] is.
#[no_mangle]
pub extern "C" fn vnhost_snap_ctl() -> *const Ctl {
    &VNHOST_SNAP
}

/// Whether threads are asked to stand still.
#[inline(always)]
pub fn pausing() -> bool {
    VNHOST_SNAP.pause.load(Relaxed) != 0
}

/// The restore count of this memory: a service started for an older one starts again.
pub fn epoch() -> u32 {
    VNHOST_SNAP.epoch.load(Relaxed)
}

/// Whether this module was started on a saved image (sake's main restores then).
pub fn restoring() -> bool {
    VNHOST_SNAP.restoring.load(Relaxed) != 0
}

/// Starts a service once per epoch: true for the caller that should (`at` remembers the epoch
/// it was started for, plus one).
pub fn once(at: &AtomicU32) -> bool {
    let e = epoch() + 1;
    let old = at.load(Acquire);
    old != e && at.compare_exchange(old, e, AcqRel, Acquire).is_ok()
}

// ---------------------------------------------------------------------------------------------
// clocks (crates/kura/src/wrap.rs adds the offsets)

/// What the program's clock `i` (0 monotonic, 1 real time) adds to the raw one, ms.
#[inline(always)]
pub fn offset(i: usize) -> f64 {
    f64::from_bits(VNHOST_SNAP.offset[i].load(Relaxed))
}

fn add_offset(i: usize, ms: f64) {
    let o = &VNHOST_SNAP.offset[i];
    o.store((f64::from_bits(o.load(Relaxed)) + ms).to_bits(), Relaxed);
}

fn raw_clocks() -> [f64; 2] {
    #[cfg(target_os = "emscripten")]
    unsafe {
        extern "C" {
            fn emscripten_get_now() -> f64;
            fn emscripten_date_now() -> f64;
        }
        [emscripten_get_now(), emscripten_date_now()]
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64() * 1000.0);
        [t, t]
    }
}

// ---------------------------------------------------------------------------------------------
// participants

pub const BUSY: u32 = 0;
pub const IDLE: u32 = 1;
pub const PARKED: u32 = 2;

/// A thread that must stand still for a state: a guest thread (`guest`, with sake's record of
/// it in `user`) or one of the host's own.
pub struct Part {
    pub name: &'static str,
    pub guest: bool,
    pub state: AtomicU32,
    /// sake's `Thread`
    pub user: AtomicUsize,
    /// what the thread did when it parked (sake: how to go on after a restore)
    pub resume: [AtomicU64; 4],
    /// what it does now (sake: the trap address of the API it is in), for the message of a
    /// save that could not stop it
    pub doing: AtomicU32,
}

static PARTS: Mutex<Vec<&'static Part>> = Mutex::new(Vec::new());

thread_local! {
    static ME: std::cell::Cell<Option<&'static Part>> = const { std::cell::Cell::new(None) };
    /// guest threads: at the top level of the run loop (crates/kura/src/cpu.rs may stop there)
    static PARKABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Makes the calling thread a participant (busy). A host thread calls it at its start.
pub fn register(name: &'static str, guest: bool, user: usize) -> &'static Part {
    let p: &'static Part = Box::leak(Box::new(Part {
        name,
        guest,
        state: AtomicU32::new(BUSY),
        user: AtomicUsize::new(user),
        resume: [const { AtomicU64::new(0) }; 4],
        doing: AtomicU32::new(0),
    }));
    PARTS.lock().unwrap_or_else(|e| e.into_inner()).push(p);
    ME.with(|m| m.set(Some(p)));
    p
}

/// The calling thread ends (or no longer takes part).
pub fn unregister() {
    if let Some(p) = ME.with(|m| m.take()) {
        PARTS.lock().unwrap_or_else(|e| e.into_inner()).retain(|&x| !std::ptr::eq(x, p));
        COORD_CV.notify_all();
    }
}

pub fn me() -> Option<&'static Part> {
    ME.with(|m| m.get())
}

/// Guest threads: whether the run loop is at its top level now.
#[inline]
pub fn set_parkable(on: bool) {
    PARKABLE.with(|p| p.set(on));
}
#[inline]
pub fn parkable() -> bool {
    PARKABLE.with(|p| p.get())
}

static GATE: Mutex<()> = Mutex::new(());
static GATE_CV: Condvar = Condvar::new();
static COORD_CV: Condvar = Condvar::new();

/// Stands still while a state is taken (a guest thread at a safe point; its `resume` says
/// how it goes on in a restored copy). Returns at once when nothing is asked.
pub fn park() {
    let Some(p) = me() else { return };
    let mut g = GATE.lock().unwrap_or_else(|e| e.into_inner());
    p.state.store(PARKED, SeqCst);
    COORD_CV.notify_all();
    while pausing() {
        g = GATE_CV.wait_timeout(g, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
    }
    p.state.store(BUSY, SeqCst);
}

/// Host threads: about to wait for work (nothing of theirs changes until [`busy`]).
pub fn idle() {
    if let Some(p) = me() {
        p.state.store(IDLE, SeqCst);
        COORD_CV.notify_all();
    }
}

/// Host threads: work to do; waits while a state is taken.
pub fn busy() {
    let Some(p) = me() else { return };
    loop {
        p.state.store(BUSY, SeqCst);
        if !pausing() {
            return;
        }
        p.state.store(IDLE, SeqCst);
        let mut g = GATE.lock().unwrap_or_else(|e| e.into_inner());
        COORD_CV.notify_all();
        while pausing() {
            g = GATE_CV.wait_timeout(g, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

/// Work handed to another thread (counted until done): a state waits for none to be left.
pub static DRAIN: AtomicU32 = AtomicU32::new(0);

pub fn drain_add() {
    DRAIN.fetch_add(1, SeqCst);
}
pub fn drain_done() {
    DRAIN.fetch_sub(1, SeqCst);
    COORD_CV.notify_all();
}

/// Wakes a sleeping thread so that it notices a pause (sake: its condition variables).
static WAKERS: Mutex<Vec<fn()>> = Mutex::new(Vec::new());

pub fn on_pause(f: fn()) {
    WAKERS.lock().unwrap_or_else(|e| e.into_inner()).push(f);
}

fn wake_all() {
    let w = WAKERS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    w.iter().for_each(|f| f());
}

/// Sleeps `d`, or less when a pause comes (the caller then parks and sleeps again).
pub fn sleep(d: Duration) {
    let g = GATE.lock().unwrap_or_else(|e| e.into_inner());
    if pausing() {
        return;
    }
    drop(GATE_CV.wait_timeout(g, d));
}

// ---------------------------------------------------------------------------------------------
// what to copy

/// Memory ranges a state holds besides the host's own (sake: the guest's reservations).
static RANGES: Mutex<Option<fn(&mut Vec<(u32, u32)>)>> = Mutex::new(None);

pub fn set_ranges(f: fn(&mut Vec<(u32, u32)>)) {
    *RANGES.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

/// The host's part of memory: statics, heap and stacks, from GLOBAL_BASE to the heap's top.
fn host_range() -> (u32, u32) {
    #[cfg(target_os = "emscripten")]
    {
        extern "C" {
            fn sbrk(n: isize) -> *mut std::ffi::c_void;
        }
        let top = unsafe { sbrk(0) } as usize as u64;
        let lo = 0x8000_0000u64;
        (lo as u32, (top.saturating_sub(lo)) as u32)
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        (0, 0)
    }
}

/// The list the page copies: merged, page-aligned (64 KiB) ranges.
fn build_ranges() -> Vec<u32> {
    let mut v = Vec::new();
    if let Some(f) = *RANGES.lock().unwrap_or_else(|e| e.into_inner()) {
        f(&mut v);
    }
    let (lo, n) = host_range();
    if n > 0 {
        v.push((lo, n));
    }
    merge_ranges(v)
}

/// Sorted, 64 KiB-aligned, overlapping and touching ones merged: [addr, len]*.
pub fn merge_ranges(mut v: Vec<(u32, u32)>) -> Vec<u32> {
    const G: u64 = 0x1_0000;
    let mut r: Vec<(u64, u64)> = v
        .drain(..)
        .filter(|&(_, n)| n > 0)
        .map(|(a, n)| {
            let lo = a as u64 & !(G - 1);
            let hi = ((a as u64 + n as u64 + G - 1) & !(G - 1)).min(1 << 32);
            (lo, hi)
        })
        .collect();
    r.sort();
    let mut out: Vec<(u64, u64)> = Vec::new();
    for (lo, hi) in r {
        match out.last_mut() {
            Some(l) if lo <= l.1 => l.1 = l.1.max(hi),
            _ => out.push((lo, hi)),
        }
    }
    out.iter().flat_map(|&(lo, hi)| [lo as u32, (hi - lo).min(u32::MAX as u64) as u32]).collect()
}

// ---------------------------------------------------------------------------------------------
// the coordinator

/// How long threads get to reach a safe point.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the drawing thread gets to read the GPU back.
const GPU_TIMEOUT: Duration = Duration::from_secs(20);

/// Whether a drawing thread takes part (render: it registers when it starts).
pub static GPU_THREAD: AtomicU32 = AtomicU32::new(0);

/// Starts the coordinator (once per epoch; sake's main).
pub fn start() {
    static AT: AtomicU32 = AtomicU32::new(0);
    if !once(&AT) {
        return;
    }
    let _ = std::thread::Builder::new().name("snap".into()).spawn(|| {
        crate::wrap::load_exclude();
        loop {
            wait_cmd(CMD_SAVE);
            take();
        }
    });
}

#[cfg(target_arch = "wasm32")]
extern "C" {
    fn emscripten_futex_wait(addr: *const std::ffi::c_void, val: u32, max_ms: f64) -> i32;
    fn emscripten_futex_wake(addr: *const std::ffi::c_void, count: i32) -> i32;
}

fn wait_cmd(want: u32) {
    let c = &VNHOST_SNAP.cmd;
    loop {
        let v = c.load(SeqCst);
        if v == want {
            c.store(CMD_NONE, SeqCst);
            return;
        }
        #[cfg(target_arch = "wasm32")]
        unsafe {
            emscripten_futex_wait(c.as_ptr() as *const std::ffi::c_void, v, 1000.0);
        }
        #[cfg(not(target_arch = "wasm32"))]
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Names what [`Part::doing`] holds (sake: API names).
static NAMER: Mutex<Option<fn(u32) -> String>> = Mutex::new(None);

pub fn set_namer(f: fn(u32) -> String) {
    *NAMER.lock().unwrap_or_else(|e| e.into_inner()) = Some(f);
}

/// Who keeps a state from being taken now (empty: no one).
fn blockers() -> Vec<String> {
    let parts = PARTS.lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<String> = parts
        .iter()
        .filter(|p| match p.state.load(SeqCst) {
            PARKED => false,
            IDLE => p.guest,
            _ => true,
        })
        .map(|p| {
            let d = p.doing.load(Relaxed);
            match (d, *NAMER.lock().unwrap_or_else(|e| e.into_inner())) {
                (0, _) | (_, None) => p.name.to_string(),
                (d, Some(f)) => format!("{} ({})", p.name, f(d)),
            }
        })
        .collect();
    let d = DRAIN.load(SeqCst);
    if d != 0 {
        v.push(format!("{d} jobs"));
    }
    v
}

fn fail(msg: String) {
    eprintln!("snap: no state: {msg}");
    let b = Box::leak(msg.into_boxed_str());
    VNHOST_SNAP.msg.store(b.as_ptr() as usize as u32, SeqCst);
    VNHOST_SNAP.msg_len.store(b.len() as u32, SeqCst);
    resume_all(None);
    VNHOST_SNAP.state.store(ST_FAILED, SeqCst);
    notify_page();
}

fn resume_all(paused_at: Option<f64>) {
    // (the time the program stood still is not its time)
    if let Some(t) = paused_at {
        let now = raw_clocks()[0];
        add_offset(0, t - now);
        add_offset(1, t - now);
    }
    VNHOST_SNAP.gpu.store(GPU_NONE, SeqCst);
    let _g = GATE.lock().unwrap_or_else(|e| e.into_inner());
    VNHOST_SNAP.pause.store(0, SeqCst);
    GATE_CV.notify_all();
}

fn notify_page() {
    #[cfg(target_arch = "wasm32")]
    unsafe {
        emscripten_futex_wake(VNHOST_SNAP.state.as_ptr() as *const std::ffi::c_void, i32::MAX);
    }
}

/// One save: pause, GPU, the list, READY, wait for RESUME.
fn take() {
    let t0 = raw_clocks()[0];
    VNHOST_SNAP.msg_len.store(0, SeqCst);
    VNHOST_SNAP.state.store(ST_PAUSING, SeqCst);
    VNHOST_SNAP.pause.store(1, SeqCst);
    let start = std::time::Instant::now();
    // every thread at a safe point
    let mut g = GATE.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        drop(g);
        wake_all();
        let b = blockers();
        if b.is_empty() {
            // (a thread that just finished its work may still be putting its things away:
            // once more after a moment)
            std::thread::sleep(Duration::from_millis(20));
            if blockers().is_empty() {
                break;
            }
            g = GATE.lock().unwrap_or_else(|e| e.into_inner());
            continue;
        }
        if start.elapsed() > STOP_TIMEOUT {
            return fail(format!("still running: {}", b.join(", ")));
        }
        g = GATE.lock().unwrap_or_else(|e| e.into_inner());
        g = COORD_CV.wait_timeout(g, Duration::from_millis(10)).unwrap_or_else(|e| e.into_inner()).0;
    }
    // the GPU's textures and buffers into memory
    if GPU_THREAD.load(SeqCst) != 0 {
        VNHOST_SNAP.gpu.store(GPU_DUMP, SeqCst);
        let t = std::time::Instant::now();
        loop {
            match VNHOST_SNAP.gpu.load(SeqCst) {
                GPU_DONE => break,
                GPU_FAILED => return fail("the GPU could not be read back".into()),
                _ if t.elapsed() > GPU_TIMEOUT => return fail("the GPU took too long to read back".into()),
                _ => std::thread::sleep(Duration::from_millis(2)),
            }
        }
        // (and the drawing thread back between frames)
        let t = std::time::Instant::now();
        while !blockers().is_empty() {
            if t.elapsed() > STOP_TIMEOUT {
                return fail(format!("still running: {}", blockers().join(", ")));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let list = Box::leak(build_ranges().into_boxed_slice());
    VNHOST_SNAP.ranges.store(list.as_ptr() as usize as u32, SeqCst);
    VNHOST_SNAP.nranges.store(list.len() as u32 / 2, SeqCst);
    let c = raw_clocks();
    for (i, &v) in c.iter().enumerate() {
        VNHOST_SNAP.clock[i].store((v + offset(i)).to_bits(), SeqCst);
    }
    VNHOST_SNAP.saves.fetch_add(1, SeqCst);
    eprintln!("snap: stopped in {:.0} ms, {} ranges", c[0] - t0, list.len() / 2);
    VNHOST_SNAP.state.store(ST_READY, SeqCst);
    notify_page();
    // (nothing of ours may change memory until the page has its copy)
    wait_cmd(CMD_RESUME);
    let _ = unsafe { Box::from_raw(list as *mut [u32]) };
    resume_all(Some(c[0]));
    VNHOST_SNAP.state.store(ST_RUNNING, SeqCst);
    notify_page();
}

// ---------------------------------------------------------------------------------------------
// restoring

/// In a restored module, before anything else runs: the clocks continue from the save, the
/// coordinator's state is clear, and the guest threads to start again (their parts are no
/// one's now: each new thread registers again).
pub fn restore_begin() -> Vec<&'static Part> {
    let c = raw_clocks();
    for (i, &now) in c.iter().enumerate() {
        let saved = f64::from_bits(VNHOST_SNAP.clock[i].load(SeqCst));
        // offset' such that now + offset' = saved
        VNHOST_SNAP.offset[i].store((saved - now).to_bits(), SeqCst);
    }
    VNHOST_SNAP.epoch.fetch_add(1, SeqCst);
    VNHOST_SNAP.restores.fetch_add(1, SeqCst);
    // (and `restoring` cleared, GPU_THREAD set when the module's drawing thread runs already)
    crate::render::snap_restore();
    VNHOST_SNAP.cmd.store(CMD_NONE, SeqCst);
    VNHOST_SNAP.gpu.store(GPU_NONE, SeqCst);
    VNHOST_SNAP.state.store(ST_RUNNING, SeqCst);
    DRAIN.store(0, SeqCst);
    WAKERS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    crate::vfs::snap_restore();
    let parts = std::mem::take(&mut *PARTS.lock().unwrap_or_else(|e| e.into_inner()));
    VNHOST_SNAP.pause.store(0, SeqCst);
    parts.into_iter().filter(|p| p.guest).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_merge_and_align() {
        assert_eq!(merge_ranges(vec![]), Vec::<u32>::new());
        assert_eq!(merge_ranges(vec![(0x30000, 0x1000)]), vec![0x30000, 0x10000]);
        assert_eq!(merge_ranges(vec![(0x31000, 0x1000), (0x30000, 0x100)]), vec![0x30000, 0x10000]);
        // touching ranges merge, gaps stay
        assert_eq!(merge_ranges(vec![(0x40000, 0x10000), (0x50000, 0x10)]), vec![0x40000, 0x20000]);
        assert_eq!(merge_ranges(vec![(0x40000, 0x10000), (0x70000, 0x10)]), vec![0x40000, 0x10000, 0x70000, 0x10000]);
        // up to the top of the address space
        assert_eq!(merge_ranges(vec![(0xffff_0000, 0xffff)]), vec![0xffff_0000, 0x10000]);
        assert_eq!(merge_ranges(vec![(0x10000, 0), (0x20000, 1)]), vec![0x20000, 0x10000]);
    }

    #[test]
    fn ranges_cover_every_input_byte() {
        use proptest::prelude::*;
        proptest!(|(v in proptest::collection::vec((0u32..0xffff_0000, 0u32..0x80000), 0..20))| {
            let r = merge_ranges(v.clone());
            let pairs: Vec<(u64, u64)> = r.chunks(2).map(|c| (c[0] as u64, c[0] as u64 + c[1] as u64)).collect();
            for w in pairs.windows(2) {
                prop_assert!(w[0].1 < w[1].0, "sorted and apart: {pairs:x?}");
            }
            for &(lo, hi) in &pairs {
                prop_assert!(lo % 0x10000 == 0 && hi % 0x10000 == 0);
            }
            for (a, n) in v {
                if n == 0 { continue; }
                let (a, e) = (a as u64, a as u64 + n as u64);
                prop_assert!(pairs.iter().any(|&(lo, hi)| lo <= a && e <= hi), "{a:x}+{n:x} in {pairs:x?}");
            }
        });
    }

    #[test]
    fn once_per_epoch() {
        let at = AtomicU32::new(0);
        assert!(once(&at));
        assert!(!once(&at));
        VNHOST_SNAP.epoch.fetch_add(1, SeqCst);
        assert!(once(&at));
        assert!(!once(&at));
        VNHOST_SNAP.epoch.fetch_sub(1, SeqCst);
    }
}
