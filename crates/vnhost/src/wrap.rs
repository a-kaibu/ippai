//! libc entry points routed through vnhost with `-Wl,--wrap=<name>` (crates/sake/build.sh):
//! stdout/stderr into the crash log, clocks without BigInts, and the per-thread CPU count
//! (VNVM_LOAD) around futex waits. sched_yield is in c/shim.c.

use std::ffi::{c_int, c_void};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use libc::{iovec, size_t, ssize_t};

extern "C" {
    fn __real_write(fd: c_int, buf: *const c_void, len: size_t) -> ssize_t;
    fn __real_writev(fd: c_int, iov: *const iovec, cnt: c_int) -> ssize_t;
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_write(f: c_int, buf: *const c_void, len: size_t) -> ssize_t {
    if (f == 1 || f == 2) && !buf.is_null() {
        crate::crashlog::output(f, std::slice::from_raw_parts(buf as *const u8, len));
    }
    __real_write(f, buf, len)
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_writev(f: c_int, iov: *const iovec, cnt: c_int) -> ssize_t {
    if (f == 1 || f == 2) && !iov.is_null() {
        // (one record for the pieces of one line)
        let mut l = crate::crashlog::Line::<4096>::new();
        for v in std::slice::from_raw_parts(iov, cnt.max(0) as usize) {
            if !v.iov_base.is_null() {
                l.push(std::slice::from_raw_parts(v.iov_base as *const u8, v.iov_len));
            }
        }
        crate::crashlog::output(f, l.bytes());
    }
    __real_writev(f, iov, cnt)
}

// ---------------------------------------------------------------------------------------------
// clocks

extern "C" {
    fn __real_clock_gettime(clk: libc::clockid_t, ts: *mut libc::timespec) -> c_int;
    fn emscripten_get_now() -> f64;
    fn emscripten_date_now() -> f64;
}

/// The monotonic and real-time clocks straight from performance.now() / Date.now() as doubles:
/// emscripten's clock_gettime goes through WASI's clock_time_get, whose 64-bit arguments make
/// every call box BigInts in JS. The wineserver and every wait read the clock thousands of times
/// a second.
#[no_mangle]
pub unsafe extern "C" fn __wrap_clock_gettime(clk: libc::clockid_t, ts: *mut libc::timespec) -> c_int {
    let ms = match clk {
        libc::CLOCK_REALTIME => emscripten_date_now(),
        libc::CLOCK_MONOTONIC | libc::CLOCK_MONOTONIC_RAW | libc::CLOCK_BOOTTIME => emscripten_get_now(),
        _ => return __real_clock_gettime(clk, ts),
    };
    let ns = (ms * 1e6) as i64;
    (*ts).tv_sec = (ns / 1_000_000_000) as _;
    (*ts).tv_nsec = (ns % 1_000_000_000) as _;
    0
}

extern "C" {
    fn __real_emscripten_futex_wait(addr: *mut c_void, val: u32, max_ms: f64) -> c_int;
}

/// Per thread (with VNVM_LOAD set, for web/game.html): CPU time used up to its latest wait
/// (ms, by its own clock: performance.now() differs between workers), and when it last woke by
/// Date.now() (0: waiting), which all threads share; see [`vnhost_load_busy`].
struct Load {
    busy: AtomicU64,
    woke: AtomicU64,
    /// waits so far
    waits: AtomicU64,
}
const LOAD_THREADS: usize = 256;
static LOAD: [Load; LOAD_THREADS] = [const {
    Load { busy: AtomicU64::new(0), woke: AtomicU64::new(0), waits: AtomicU64::new(0) }
}; LOAD_THREADS];
static LOAD_N: AtomicUsize = AtomicUsize::new(0);
/// Longest run between waits counted (ms): some thread (in the page) blocks somewhere other
/// than emscripten_futex_wait and would otherwise look busy for good.
const LOAD_RUN_MAX: f64 = 3000.0;
/// CPU time (ms, f64 bits) of the threads that ended
static LOAD_RETIRED: AtomicU64 = AtomicU64::new(0);

fn add_f64(a: &AtomicU64, v: f64) {
    let _ = a.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |r| Some((f64::from_bits(r) + v).to_bits()));
}

/// A thread's slot and when it last woke by its own clock; its time goes to LOAD_RETIRED when
/// the thread ends (load_key).
struct LoadSlot(Option<&'static Load>, std::cell::Cell<f64>);
// No Drop: the first wait of a thread can come from inside malloc (its lock), where registering
// a thread-local destructor aborts ("the System allocator may not use TLS with destructors");
// load_key's pthread destructor retires the slot instead (it runs for every thread).

/// The pthread key whose destructor retires a thread's slot (value: slot + 1).
fn load_key() -> libc::pthread_key_t {
    static KEY: std::sync::OnceLock<libc::pthread_key_t> = std::sync::OnceLock::new();
    unsafe extern "C" fn retire(v: *mut c_void) {
        let Some(l) = LOAD.get((v as usize).wrapping_sub(1)) else { return };
        let woke = f64::from_bits(l.woke.swap(0, Ordering::Relaxed));
        let mut busy = f64::from_bits(l.busy.swap(0, Ordering::Relaxed));
        if woke != 0.0 {
            busy += emscripten_date_now() - woke;
        }
        add_f64(&LOAD_RETIRED, busy);
    }
    *KEY.get_or_init(|| {
        let mut k = 0;
        unsafe { libc::pthread_key_create(&mut k, Some(retire)) };
        k
    })
}

/// Every block of a thread here ends up in emscripten_futex_wait (pthread mutexes and condition
/// variables, Rust's too); with VNVM_LOAD set, the time outside it is counted per thread.
#[no_mangle]
pub unsafe extern "C" fn __wrap_emscripten_futex_wait(addr: *mut c_void, val: u32, max_ms: f64) -> c_int {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    extern "C" {
        fn emscripten_is_main_browser_thread() -> c_int;
    }
    // (not the page's own thread: it waits in its event loop, not here, so all of its time
    // would look busy)
    if !*ON.get_or_init(|| std::env::var_os("VNVM_LOAD").is_some()) || emscripten_is_main_browser_thread() != 0 || NO_LOAD.with(|c| c.get()) {
        return __real_emscripten_futex_wait(addr, val, max_ms);
    }
    thread_local!(static SLOT: LoadSlot = {
        let i = LOAD_N.fetch_add(1, Ordering::Relaxed);
        let l = LOAD.get(i);
        if let Some(l) = l {
            l.woke.store(unsafe { emscripten_date_now() }.to_bits(), Ordering::Relaxed);
            // Wine's threads end without running Rust's thread-local destructors; pthread key
            // destructors do run
            unsafe { libc::pthread_setspecific(load_key(), (i + 1) as *const c_void) };
        }
        LoadSlot(l, std::cell::Cell::new(unsafe { emscripten_get_now() }))
    });
    // VNVM_LOAD_LOG: every 5 s, the threads running for over a second (a check of the counting)
    static LOGGER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    static LOG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *LOG.get_or_init(|| std::env::var_os("VNVM_LOAD_LOG").is_some()) && !LOGGER.swap(true, Ordering::Relaxed) {
        std::thread::spawn(|| loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            let now = unsafe { emscripten_date_now() };
            let n = LOAD_N.load(Ordering::Relaxed).min(LOAD_THREADS);
            let long: Vec<String> = (0..n).filter_map(|k| {
                let w = f64::from_bits(LOAD[k].woke.load(Ordering::Relaxed));
                (w != 0.0 && now - w > 1000.0).then(|| format!("{k}:{:.0}s", (now - w) / 1000.0))
            }).collect();
            eprintln!("vnload: {n} threads, running for over 1 s: {}", long.join(" "));
        });
    }
    let Ok(Some((l, woke))) = SLOT.try_with(|s| s.0.map(|l| (l, s.1.get()))) else { return __real_emscripten_futex_wait(addr, val, max_ms) };
    let idle = LOAD_IDLE.with(|c| c.replace(0.0));
    add_f64(&l.busy, (emscripten_get_now() - woke - idle).clamp(0.0, LOAD_RUN_MAX));
    l.waits.fetch_add(1, Ordering::Relaxed);
    l.woke.store(0, Ordering::Relaxed);
    LOAD_SLOT.with(|c| c.set(Some(l)));
    let r = __real_emscripten_futex_wait(addr, val, max_ms);
    let _ = SLOT.try_with(|s| s.1.set(emscripten_get_now()));
    l.woke.store(emscripten_date_now().to_bits(), Ordering::Relaxed);
    r
}

thread_local!(static LOAD_SLOT: std::cell::Cell<Option<&'static Load>> = const { std::cell::Cell::new(None) });
thread_local!(static NO_LOAD: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// This thread waits in its event loop (as the page's does), not in futex waits: not counted.
pub fn load_exclude() {
    NO_LOAD.with(|c| c.set(true));
}

/// Runs `f` (a synchronous XHR) as a wait: the thread is blocked on the network, not busy.
pub fn load_idle<R>(f: impl FnOnce() -> R) -> R {
    let Some(l) = LOAD_SLOT.with(|c| c.get()) else { return f() };
    let woke = f64::from_bits(l.woke.swap(0, Ordering::Relaxed));
    let t = unsafe { emscripten_get_now() };
    let r = f();
    let blocked = unsafe { emscripten_get_now() } - t;
    // the wrapper counts from its own stamp: it takes off the time blocked
    LOAD_IDLE.with(|c| c.set(c.get() + blocked));
    if woke != 0.0 {
        l.woke.store((woke + blocked).to_bits(), Ordering::Relaxed);
    }
    r
}
thread_local!(static LOAD_IDLE: std::cell::Cell<f64> = const { std::cell::Cell::new(0.0) });

/// CPU time (ms) all threads that ever waited used so far.
#[no_mangle]
pub unsafe extern "C" fn vnhost_load_busy() -> f64 {
    let now = emscripten_date_now();
    let n = LOAD_N.load(Ordering::Relaxed).min(LOAD_THREADS);
    let mut busy = f64::from_bits(LOAD_RETIRED.load(Ordering::Relaxed));
    for l in &LOAD[..n] {
        busy += f64::from_bits(l.busy.load(Ordering::Relaxed));
        let woke = f64::from_bits(l.woke.load(Ordering::Relaxed));
        if woke != 0.0 {
            busy += (now - woke).min(LOAD_RUN_MAX);
        }
    }
    busy
}

/// Thread `i` of vnhost_load_busy's count (in order of their first wait): its CPU time (ms) and
/// waits so far to `out[0..2]` (`out[2..4]`: 0, once a process and spins of Wine's waits);
/// false past the last thread.
#[no_mangle]
pub unsafe extern "C" fn vnhost_load_slot(i: u32, out: *mut f64) -> bool {
    let n = LOAD_N.load(Ordering::Relaxed).min(LOAD_THREADS);
    let Some(l) = LOAD[..n].get(i as usize) else { return false };
    let mut busy = f64::from_bits(l.busy.load(Ordering::Relaxed));
    let woke = f64::from_bits(l.woke.load(Ordering::Relaxed));
    if woke != 0.0 {
        busy += (emscripten_date_now() - woke).min(LOAD_RUN_MAX);
    }
    let out = std::slice::from_raw_parts_mut(out, 4);
    out[0] = busy;
    out[1] = l.waits.load(Ordering::Relaxed) as f64;
    out[2] = 0.0;
    out[3] = 0.0;
    true
}

/// Threads counted by vnhost_load_busy * 1000 + those running now (a check of the counting).
#[no_mangle]
pub unsafe extern "C" fn vnhost_load_threads() -> u32 {
    let n = LOAD_N.load(Ordering::Relaxed).min(LOAD_THREADS);
    (n * 1000 + LOAD[..n].iter().filter(|l| l.woke.load(Ordering::Relaxed) != 0).count()) as u32
}
