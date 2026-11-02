//! Wine processes as instances inside the one wasm module.
//!
//! Every Wine process gets its own instance of the unix-side module (its own statics) and its
//! own slice of the guest address space; all of them share the linear memory, wineserver and
//! the emulated fds. A thread belongs to the process that created it (see the pthread_create
//! wrapper in c/shim.c). Threads of the host itself (main, wineserver) have pid `HOST_PID`.
//!
//! Per process state kept here: the address partition, the environment (getenv/setenv see
//! per-process overrides on top of the host environment), the current directory, the thread
//! list and exit status.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, CStr, CString};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::mem::GUEST_END;

pub const HOST_PID: i32 = 42;
const FIRST_PID: i32 = 100;

/// Guest addresses everybody may map: the window where wineserver's shared mappings live
/// (see `mem`), up to KUSER_SHARED_DATA and the page above it.
pub const SHARED_LO: usize = 0x7c00_0000;
pub const USER_SHARED_DATA: usize = 0x7ffe_0000;
const AREA_LO: usize = 0x1_0000;
const APP_SIZE: usize = 0x5000_0000;
const HELPER_SIZE: usize = 0x0800_0000;
const MIN_SIZE: usize = 0x0200_0000;

struct Proc {
    lo: usize,
    hi: usize,
    root: bool,
    dead: bool,
    tids: Vec<i32>,
    /// None = unset
    env: BTreeMap<String, Option<CString>>,
    cwd: String,
    /// stopped while a child it started has its range (see `create`); `parked` of its threads
    /// have stopped so far
    frozen: bool,
    parked: usize,
    /// a child took its range: it freezes once its CreateProcess has let the child go
    /// (`vnhost_proc_freeze`)
    freeze_pending: bool,
    /// its memory while frozen
    saved: Option<crate::mem::Saved>,
    /// (the child) the process whose range it took, and whether that one is swapped out yet
    swap_from: Option<i32>,
    swapped: bool,
}

#[derive(Default)]
struct Procs {
    list: BTreeMap<i32, Proc>,
    next: i32,
}

static PROCS: Mutex<Option<Procs>> = Mutex::new(None);
/// Signalled when a process freezes, a thread parks, or a process thaws.
static PARK: Condvar = Condvar::new();
static ANY_FROZEN: AtomicBool = AtomicBool::new(false);

thread_local! {
    static PID: Cell<i32> = const { Cell::new(HOST_PID) };
}

fn with<R>(f: impl FnOnce(&mut Procs) -> R) -> R {
    let mut g = PROCS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Procs { next: FIRST_PID, ..Default::default() }))
}

pub fn current() -> i32 {
    PID.with(|p| p.get())
}

/// A free range of `size` bytes: apps take the lowest one (exes want 0x400000), helpers the
/// highest one.
fn find_area(p: &Procs, size: usize, low: bool) -> Option<(usize, usize)> {
    let mut used: Vec<(usize, usize)> = p.list.values().map(|q| (q.lo, q.hi)).collect();
    used.sort();
    let mut holes = Vec::new();
    let mut at = AREA_LO;
    for (lo, hi) in used.iter().copied().chain([(SHARED_LO, SHARED_LO)]) {
        if lo >= at + size {
            holes.push((at, lo));
        }
        at = at.max(hi);
    }
    if low {
        holes.first().map(|&(lo, _)| (lo, lo + size))
    } else {
        holes.last().map(|&(_, hi)| (hi - size, hi))
    }
}

/// Creates a process slot; `app` asks for the big low partition.
///
/// Processes share one address space, so a launcher (or installer) holding the low partition
/// can't start the game next to it: the game's exe wants 0x400000 too. Then the child gets the
/// launcher's partition and the launcher is swapped out until the child is gone: its threads
/// stop at their next wait or guest entry (`park_if_frozen`), the child's first thread saves
/// its memory and clears the range (`swap_out_parent`), and the child's end puts it back.
pub fn create(app: bool, root: bool) -> Option<i32> {
    let parent_env = current_env();
    let parent_cwd = cwd();
    let me = current();
    with(|p| {
        let mut size = if app { APP_SIZE } else { HELPER_SIZE };
        let low_free = find_area(p, APP_SIZE, true).is_some_and(|a| a.0 == AREA_LO);
        let swap = app && !low_free && p.list.get(&me).is_some_and(|q| q.lo == AREA_LO && !q.frozen && !q.dead);
        if swap {
            let q = p.list.get_mut(&me).unwrap();
            q.freeze_pending = true;
            let area = (q.lo, q.hi);
            let pid = p.next;
            p.next += 1;
            trace(format_args!("process {pid}: {:#x}-{:#x}, taken from {me}", area.0, area.1));
            crate::crashlog::event(format_args!("process {pid} created: {:#x}-{:#x}, taken from {me}", area.0, area.1));
            let mut c = new_proc(area, root, parent_env.clone(), parent_cwd.clone());
            c.swap_from = Some(me);
            p.list.insert(pid, c);
            return Some(pid);
        }
        let area = loop {
            if let Some(a) = find_area(p, size, app) {
                break a;
            }
            size /= 2;
            if size < MIN_SIZE {
                return None;
            }
        };
        let pid = p.next;
        p.next += 1;
        trace(format_args!("process {pid}: {:#x}-{:#x}{}", area.0, area.1, if root { " (root)" } else { "" }));
        crate::crashlog::event(format_args!("process {pid} created: {:#x}-{:#x}{}", area.0, area.1, if root { " (root)" } else { "" }));
        p.list.insert(pid, new_proc(area, root, parent_env, parent_cwd));
        Some(pid)
    })
}

/// Wine's process creation, after the spawn: whether the new process took the caller's range
/// (the caller then resumes the child itself and calls `vnhost_proc_freeze`).
#[no_mangle]
pub extern "C" fn vnhost_proc_swap_pending() -> c_int {
    let pid = current();
    with(|p| p.list.get(&pid).is_some_and(|q| q.freeze_pending)) as c_int
}

/// Freezes the calling process: its threads stop at their next wait or guest entry, and the
/// child that took its range starts once they all have.
#[no_mangle]
pub extern "C" fn vnhost_proc_freeze() {
    let pid = current();
    let kick = with(|p| match p.list.get_mut(&pid) {
        Some(q) if q.freeze_pending => {
            q.freeze_pending = false;
            q.frozen = true;
            q.parked = 0;
            ANY_FROZEN.store(true, Ordering::SeqCst);
            q.tids.clone()
        }
        _ => Vec::new(),
    });
    trace(format_args!("process {pid}: freezing"));
    PARK.notify_all();
    for t in kick {
        // (out of guest code and waits, to `park_if_frozen`)
        crate::cpu::vnhost_tkill(t, libc::SIGSTOP);
    }
}

fn new_proc(area: (usize, usize), root: bool, env: BTreeMap<String, Option<CString>>, cwd: String) -> Proc {
    Proc {
        lo: area.0,
        hi: area.1,
        root,
        dead: false,
        tids: Vec::new(),
        env,
        cwd,
        frozen: false,
        parked: 0,
        freeze_pending: false,
        saved: None,
        swap_from: None,
        swapped: false,
    }
}

/// Whether the calling thread's process is frozen (`park_if_frozen` would block).
pub fn self_frozen() -> bool {
    if !ANY_FROZEN.load(Ordering::Relaxed) {
        return false;
    }
    let pid = current();
    pid != HOST_PID && with(|p| p.list.get(&pid).is_some_and(|q| q.frozen && !q.dead))
}

/// Blocks the calling thread while its process is frozen (a no-op otherwise). Called where a
/// thread touches no guest memory: entering guest code, entering or leaving a wait.
pub fn park_if_frozen() {
    if !ANY_FROZEN.load(Ordering::Relaxed) {
        return;
    }
    let pid = current();
    if pid == HOST_PID {
        return;
    }
    let mut g = PROCS.lock().unwrap_or_else(|e| e.into_inner());
    let mut counted = false;
    loop {
        match g.as_mut().and_then(|p| p.list.get_mut(&pid)) {
            Some(q) if q.frozen && !q.dead => {
                if !counted {
                    q.parked += 1;
                    counted = true;
                    PARK.notify_all();
                }
            }
            _ => break,
        }
        g = PARK.wait_timeout(g, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        // a new process's dlopen waits until every thread has loaded it
        #[cfg(target_os = "emscripten")]
        {
            drop(g);
            unsafe { _emscripten_process_dlopen_queue() };
            g = PROCS.lock().unwrap_or_else(|e| e.into_inner());
        }
    }
    if counted {
        if let Some(q) = g.as_mut().and_then(|p| p.list.get_mut(&pid)) {
            q.parked = q.parked.saturating_sub(1);
        }
    }
    drop(g);
    check_alive();
}

#[cfg(target_os = "emscripten")]
extern "C" {
    fn _emscripten_process_dlopen_queue();
}

/// The first thread of a child that took its parent's range: waits until the parent's threads
/// have stopped, then saves the parent's memory and clears the range.
fn swap_out_parent(pid: i32) {
    let Some(parent) = with(|p| p.list.get(&pid).filter(|q| !q.swapped).and_then(|q| q.swap_from)) else {
        return;
    };
    let start = Instant::now();
    let mut warned = false;
    let mut g = PROCS.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        let q = g.as_ref().and_then(|p| p.list.get(&parent));
        match q {
            Some(q) if (q.freeze_pending || q.frozen && q.parked < q.tids.len()) && !q.dead => {}
            _ => break,
        }
        if !warned && start.elapsed() > Duration::from_secs(10) {
            warned = true;
            eprintln!("vnhost: process {parent}: still not stopped after 10 s (process {pid} waits for its range)");
        }
        g = PARK.wait_timeout(g, Duration::from_millis(20)).unwrap_or_else(|e| e.into_inner()).0;
    }
    let range = g.as_ref().and_then(|p| p.list.get(&parent)).filter(|q| q.frozen && !q.dead).map(|q| (q.lo, q.hi));
    drop(g);
    let saved = range.map(|(lo, hi)| crate::mem::swap_out(lo, hi, parent));
    with(|p| {
        if let Some(c) = p.list.get_mut(&pid) {
            c.swapped = true;
        }
        match p.list.get_mut(&parent) {
            Some(q) if q.frozen => q.saved = saved,
            // (the parent died meanwhile: what was saved goes)
            _ => {}
        }
    });
}

/// After child `child` (which held `parent`'s range) is gone: puts the parent back and lets it
/// run again.
fn thaw(p: &mut Procs, parent: i32) {
    if let Some(q) = p.list.get_mut(&parent) {
        if let Some(saved) = q.saved.take() {
            crate::mem::swap_in(q.lo, q.hi, saved);
        }
        q.frozen = false;
        trace(format_args!("process {parent}: thawed"));
    }
    ANY_FROZEN.store(p.list.values().any(|q| q.frozen), Ordering::SeqCst);
    PARK.notify_all();
}

pub(crate) fn trace(args: std::fmt::Arguments) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("VNVM_TRACE_PROC").is_some()) {
        eprintln!("vnhost: {args}");
    }
}

/// The host threads' current directory (Wine processes have their own).
static HOST_CWD: Mutex<Option<String>> = Mutex::new(None);

/// The calling thread's process's current directory.
pub fn cwd() -> String {
    let pid = current();
    if let Some(c) = with(|p| p.list.get(&pid).map(|q| q.cwd.clone())) {
        return c;
    }
    let mut g = HOST_CWD.lock().unwrap_or_else(|e| e.into_inner());
    g.get_or_insert_with(|| crate::wrap::real_getcwd().unwrap_or_else(|| "/".into())).clone()
}

pub fn set_cwd(path: String) {
    let pid = current();
    let done = with(|p| match p.list.get_mut(&pid) {
        Some(q) => {
            q.cwd = path.clone();
            true
        }
        None => false,
    });
    if !done {
        *HOST_CWD.lock().unwrap_or_else(|e| e.into_inner()) = Some(path);
    }
}

fn current_env() -> BTreeMap<String, Option<CString>> {
    let pid = current();
    with(|p| p.list.get(&pid).map(|q| q.env.clone()).unwrap_or_default())
}

pub fn exists(pid: i32) -> bool {
    with(|p| p.list.get(&pid).is_some_and(|q| !q.dead))
}

pub fn is_dead(pid: i32) -> bool {
    pid != HOST_PID && with(|p| p.list.get(&pid).is_none_or(|q| q.dead))
}

/// The guest range the calling thread's process may map.
pub fn area() -> (usize, usize) {
    let pid = current();
    with(|p| p.list.get(&pid).map_or((AREA_LO, GUEST_END), |q| (q.lo, q.hi)))
}

/// Whether the calling process may map guest range [lo, hi).
pub fn may_map(lo: usize, hi: usize) -> bool {
    if lo >= SHARED_LO {
        return true;
    }
    let pid = current();
    if pid == HOST_PID {
        return true;
    }
    let (a, b) = area();
    lo >= a && hi <= b
}

/// Marks the process dead: its fds close and its threads stop at their next chance.
/// Set when the first process (the program the page started) has ended.
static ROOT_EXITED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The page: whether the program it started has ended (it then closes the game).
#[no_mangle]
pub extern "C" fn vnhost_root_exited() -> c_int {
    ROOT_EXITED.load(std::sync::atomic::Ordering::Relaxed) as c_int
}

pub fn kill(pid: i32) {
    let tids = with(|p| match p.list.get_mut(&pid) {
        Some(q) if !q.dead => {
            q.dead = true;
            if q.root {
                ROOT_EXITED.store(true, std::sync::atomic::Ordering::Relaxed);
            }
            q.tids.clone()
        }
        _ => Vec::new(),
    });
    trace(format_args!("process {pid}: killed, threads {tids:?}"));
    crate::crashlog::event(format_args!("process {pid} ended ({} threads left)", tids.len()));
    crate::fd::close_owned(pid);
    for t in tids {
        crate::cpu::vnhost_tkill(t, libc::SIGKILL);
    }
    reap(pid);
}

fn reap(pid: i32) {
    with(|p| {
        if p.list.get(&pid).is_some_and(|q| q.dead && q.tids.is_empty()) {
            let q = p.list.remove(&pid).unwrap();
            crate::mem::forget_process(pid);
            trace(format_args!("process {pid}: gone"));
            if let Some(parent) = q.swap_from {
                thaw(p, parent);
            }
        }
    });
}

/// Ends the calling thread if its process was killed.
pub fn check_alive() {
    let pid = current();
    if is_dead(pid) {
        unsafe { libc::pthread_exit(std::ptr::null_mut()) };
    }
}

// ---------------------------------------------------------------------------------------------
// C interface (vnvm-wine hangover/main.c, c/shim.c, Wine's process creation)

#[no_mangle]
pub extern "C" fn vnhost_getpid() -> i32 {
    current()
}

/// New process slot; returns its pid or -1.
#[no_mangle]
pub extern "C" fn vnhost_proc_create(app: c_int, root: c_int) -> i32 {
    create(app != 0, root != 0).unwrap_or(-1)
}

/// Sets a variable in a (not yet started) process's environment; `val` NULL unsets it.
#[no_mangle]
pub unsafe extern "C" fn vnhost_proc_setenv(pid: i32, name: *const c_char, val: *const c_char) {
    let name = CStr::from_ptr(name).to_string_lossy().into_owned();
    let val = (!val.is_null()).then(|| CStr::from_ptr(val).to_owned());
    with(|p| {
        if let Some(q) = p.list.get_mut(&pid) {
            q.env.insert(name, val);
        }
    });
}

#[no_mangle]
pub extern "C" fn vnhost_fd_give(fd: c_int, pid: i32) {
    crate::fd::give(fd, pid);
}

/// The calling thread joins process `pid`.
#[no_mangle]
pub extern "C" fn vnhost_proc_enter(pid: i32) {
    PID.with(|p| p.set(pid));
    if pid == HOST_PID {
        return;
    }
    let tid = crate::cpu::vnhost_gettid();
    let first = with(|p| match p.list.get_mut(&pid) {
        Some(q) => {
            q.tids.push(tid);
            q.tids.len() == 1
        }
        None => false,
    });
    if first {
        swap_out_parent(pid);
    }
    // (a thread its frozen process started on the way to stopping)
    park_if_frozen();
}

/// The calling thread is going away.
#[no_mangle]
pub extern "C" fn vnhost_proc_leave() {
    let pid = current();
    if pid == HOST_PID {
        return;
    }
    let tid = crate::cpu::vnhost_gettid();
    let last = with(|p| match p.list.get_mut(&pid) {
        Some(q) => {
            q.tids.retain(|&t| t != tid);
            q.tids.is_empty()
        }
        None => false,
    });
    crate::cpu::forget_thread(tid);
    crate::crashlog::forget_tid(tid);
    if last {
        // a process whose threads all ended without calling exit()
        kill(pid);
    }
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_proc_area(lo: *mut usize, hi: *mut usize) {
    let (a, b) = area();
    *lo = a;
    *hi = b;
}

#[no_mangle]
pub extern "C" fn vnhost_proc_is_root() -> c_int {
    let pid = current();
    with(|p| p.list.get(&pid).is_some_and(|q| q.root)) as c_int
}

/// Kills the calling thread's process; returns only on host threads.
#[no_mangle]
pub extern "C" fn vnhost_proc_exit() {
    let pid = current();
    if pid == HOST_PID {
        return;
    }
    kill(pid);
    unsafe { libc::pthread_exit(std::ptr::null_mut()) };
}

// ---------------------------------------------------------------------------------------------
// environment

extern "C" {
    fn __real_getenv(name: *const c_char) -> *mut c_char;
    fn __real_setenv(name: *const c_char, val: *const c_char, overwrite: c_int) -> c_int;
    fn __real_unsetenv(name: *const c_char) -> c_int;
    fn __real_putenv(s: *mut c_char) -> c_int;
}

/// The process override for `name`: Some(None) if unset there, None if not overridden.
fn env_get(name: &str) -> Option<Option<*mut c_char>> {
    let pid = current();
    with(|p| {
        let v = p.list.get(&pid)?.env.get(name)?;
        Some(v.as_ref().map(|s| s.as_ptr() as *mut c_char))
    })
}

fn env_set(name: String, val: Option<CString>) -> bool {
    let pid = current();
    with(|p| match p.list.get_mut(&pid) {
        Some(q) => {
            q.env.insert(name, val);
            true
        }
        None => false,
    })
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_getenv(name: *const c_char) -> *mut c_char {
    if current() != HOST_PID {
        if let Some(v) = env_get(&CStr::from_ptr(name).to_string_lossy()) {
            return v.unwrap_or(std::ptr::null_mut());
        }
    }
    __real_getenv(name)
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_setenv(name: *const c_char, val: *const c_char, overwrite: c_int) -> c_int {
    let n = CStr::from_ptr(name).to_string_lossy().into_owned();
    if current() != HOST_PID {
        if overwrite == 0 && !__wrap_getenv(name).is_null() {
            return 0;
        }
        if env_set(n, Some(CStr::from_ptr(val).to_owned())) {
            return 0;
        }
    }
    __real_setenv(name, val, overwrite)
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_unsetenv(name: *const c_char) -> c_int {
    if current() != HOST_PID && env_set(CStr::from_ptr(name).to_string_lossy().into_owned(), None) {
        return 0;
    }
    __real_unsetenv(name)
}

#[no_mangle]
pub unsafe extern "C" fn __wrap_putenv(s: *mut c_char) -> c_int {
    if current() != HOST_PID {
        let kv = CStr::from_ptr(s).to_string_lossy().into_owned();
        let ok = match kv.split_once('=') {
            Some((k, v)) => env_set(k.to_owned(), CString::new(v).ok()),
            None => env_set(kv, None),
        };
        if ok {
            return 0;
        }
    }
    __real_putenv(s)
}
