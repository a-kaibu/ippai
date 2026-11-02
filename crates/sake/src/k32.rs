//! kernel32 (and the ntdll/kernelbase names that are the same functions).

#![allow(non_snake_case)]

use crate::HashMap;
use std::io::SeekFrom;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

use vnx86::{EAX, ESP};

use crate::api::{implement, Ctx};
use crate::files;
use crate::mem::{self, cstr, put32, put_cstr, put_wstr, r16, r32, w16, w32, w8, wstr};
use crate::obj::{self, Obj};
use crate::text::{a2s, a2w, mb2w, w2mb};

pub const PROCESS_HEAP: u32 = 0x0015_0000;

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

fn last_error(e: u32) {
    w32(crate::cur().teb + crate::teb_last_error(), e);
}

fn ok(b: bool) -> u32 {
    b as u32
}

/// A guest string argument as Rust text: ANSI (`wide` false) or UTF-16.
pub fn arg_str(p: u32, wide: bool) -> String {
    if wide {
        String::from_utf16_lossy(&wstr(p))
    } else {
        a2s(&cstr(p))
    }
}

/// Writes a string result as the A or W function would: returns the length written, or the
/// size needed (with NUL) when it does not fit.
pub fn ret_str(dst: u32, cap: u32, s: &str, wide: bool) -> u32 {
    if wide {
        let v: Vec<u16> = s.encode_utf16().collect();
        if (v.len() as u32) >= cap {
            return v.len() as u32 + 1;
        }
        put_wstr(dst, cap, &v)
    } else {
        let v = crate::text::s2a(s);
        if (v.len() as u32) >= cap {
            return v.len() as u32 + 1;
        }
        put_cstr(dst, cap, &v)
    }
}

// ---------------------------------------------------------------------------------------------
// process, threads

/// Nanoseconds since the first call, by the wall clock: in the browser every worker has its own
/// performance.now() (Instant), so a time taken on one thread (an input message's) and compared
/// on another (GetTickCount) would disagree by seconds; Date.now() is shared.
pub fn clock_ns() -> u64 {
    static START: OnceLock<u128> = OnceLock::new();
    let now = || std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let t0 = *START.get_or_init(now);
    now().saturating_sub(t0) as u64
}

pub fn ticks_ms() -> u32 {
    (clock_ns() / 1_000_000 + 60_000) as u32
}

static CMDLINE: Mutex<(u32, u32)> = Mutex::new((0, 0));

pub fn set_command_line(exe: &str, args: &[String]) {
    let mut s = format!("\"{exe}\"");
    for a in args {
        s += " ";
        s += a;
    }
    let a = crate::text::s2a(&s);
    let w: Vec<u16> = s.encode_utf16().collect();
    let pa = mem::heap_alloc(a.len() as u32 + 1, true);
    mem::write(pa, &a);
    let pw = mem::heap_alloc(2 * w.len() as u32 + 2, true);
    put_wstr(pw, w.len() as u32 + 1, &w);
    *CMDLINE.lock().unwrap() = (pa, pw);
}

/// (ANSI, UTF-16) command line
pub fn cmdline() -> (u32, u32) {
    *CMDLINE.lock().unwrap()
}

fn GetCommandLineA(_: &mut Ctx) -> u32 {
    CMDLINE.lock().unwrap().0
}
fn GetCommandLineW(_: &mut Ctx) -> u32 {
    CMDLINE.lock().unwrap().1
}

fn env_block(wide: bool) -> u32 {
    let vars = ["PATH=C:\\windows\\system32;C:\\windows", "TEMP=C:\\windows\\temp", "TMP=C:\\windows\\temp", "windir=C:\\windows", "SystemRoot=C:\\windows", "OS=Windows_NT", "COMPUTERNAME=SAKE", "USERNAME=user"];
    let s = vars.join("\0") + "\0\0";
    if wide {
        let w: Vec<u16> = s.encode_utf16().collect();
        let p = mem::heap_alloc(2 * w.len() as u32, true);
        for (i, c) in w.iter().enumerate() {
            w16(p + 2 * i as u32, *c);
        }
        p
    } else {
        let p = mem::heap_alloc(s.len() as u32, true);
        mem::write(p, s.as_bytes());
        p
    }
}
fn GetEnvironmentStrings(_: &mut Ctx) -> u32 {
    env_block(false)
}
fn GetEnvironmentStringsA(_: &mut Ctx) -> u32 {
    env_block(false)
}
fn GetEnvironmentStringsW(_: &mut Ctx) -> u32 {
    env_block(true)
}
fn FreeEnvironmentStringsA(c: &mut Ctx) -> u32 {
    mem::heap_free(c.a(0));
    1
}
fn FreeEnvironmentStringsW(c: &mut Ctx) -> u32 {
    mem::heap_free(c.a(0));
    1
}
fn env_get(name: &str) -> Option<String> {
    let v = match name.to_ascii_uppercase().as_str() {
        "TEMP" | "TMP" => "C:\\windows\\temp",
        "WINDIR" | "SYSTEMROOT" => "C:\\windows",
        "PATH" => "C:\\windows\\system32;C:\\windows",
        "OS" => "Windows_NT",
        _ => return None,
    };
    Some(v.into())
}
fn GetEnvironmentVariableA(c: &mut Ctx) -> u32 {
    match env_get(&arg_str(c.a(0), false)) {
        Some(v) => ret_str(c.a(1), c.a(2), &v, false),
        None => {
            last_error(203);
            0
        }
    }
}
fn GetEnvironmentVariableW(c: &mut Ctx) -> u32 {
    match env_get(&arg_str(c.a(0), true)) {
        Some(v) => ret_str(c.a(1), c.a(2), &v, true),
        None => {
            last_error(203);
            0
        }
    }
}
fn SetEnvironmentVariableA(_: &mut Ctx) -> u32 {
    1
}
fn SetEnvironmentVariableW(_: &mut Ctx) -> u32 {
    1
}
/// %NAME% replaced by its value (unknown names stay as written)
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out += &rest[..i];
        let after = &rest[i + 1..];
        match after.find('%').and_then(|j| env_get(&after[..j]).map(|v| (j, v))) {
            Some((j, v)) => {
                out += &v;
                rest = &after[j + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out + rest
}
/// ExpandEnvironmentStrings(src, dst, size): characters with the terminator, needed or written
fn expand_env_strings(c: &mut Ctx, wide: bool) -> u32 {
    let s = expand_env(&arg_str(c.a(0), wide));
    let n = ret_str(c.a(1), c.a(2), &s, wide);
    if n >= c.a(2) { n } else { n + 1 }
}
fn ExpandEnvironmentStringsA(c: &mut Ctx) -> u32 {
    expand_env_strings(c, false)
}
fn ExpandEnvironmentStringsW(c: &mut Ctx) -> u32 {
    expand_env_strings(c, true)
}

fn GetCurrentProcess(_: &mut Ctx) -> u32 {
    obj::CURRENT_PROCESS
}
fn GetCurrentProcessId(_: &mut Ctx) -> u32 {
    crate::PID
}
fn GetCurrentThread(_: &mut Ctx) -> u32 {
    obj::CURRENT_THREAD
}
fn GetCurrentThreadId(_: &mut Ctx) -> u32 {
    crate::cur().tid
}

fn ExitProcess(c: &mut Ctx) -> u32 {
    let code = c.a(0);
    if std::env::var_os("SAKE_TRACE_EXIT").is_some() {
        eprintln!("sake: ExitProcess({code}) from {:#x}", r32(c.esp));
    }
    unsafe { libc::exit(code as i32) }
}
fn TerminateProcess(c: &mut Ctx) -> u32 {
    if c.a(0) == obj::CURRENT_PROCESS {
        unsafe { libc::exit(c.a(1) as i32) }
    }
    0
}
fn FatalAppExitA(c: &mut Ctx) -> u32 {
    eprintln!("sake: FatalAppExit: {}", arg_str(c.a(1), false));
    unsafe { libc::exit(1) }
}

/// The calling thread ends (from the thread's own code: ExitThread or a return).
pub fn exit_thread(code: u32) -> ! {
    let t = crate::cur();
    let h = t.obj;
    obj::with(|tb| {
        if let Some((Obj::Thread { done, code: c, .. }, _)) = tb.objs.get_mut(&h) {
            *done = true;
            *c = code;
        }
    });
    obj::signal();
    if t.tid == 0x20 {
        // the main thread: the process
        unsafe { libc::exit(code as i32) }
    }
    // the guest stack and TEB go back to the address space (a game starting a thread per
    // file load ran out of it after ~600)
    mem::release(t.stack);
    mem::release(t.teb);
    unsafe { libc::pthread_exit(std::ptr::null_mut()) }
}
fn ExitThread(c: &mut Ctx) -> u32 {
    exit_thread(c.a(0))
}
/// _beginthread(start, stack, arg): a CRT thread (start is cdecl, returning ends it)
fn _beginthread(c: &mut Ctx) -> u32 {
    let (start, stack, arg) = (c.a(0), c.a(1), c.a(2));
    let h = obj::add(Obj::Thread { tid: 0, done: false, code: 0x103 });
    let tid = crate::spawn_gated(start, arg, if stack == 0 { 0x10_0000 } else { stack.max(0x10000) }, h, false);
    obj::with(|t| {
        if let Some((Obj::Thread { tid: x, .. }, _)) = t.objs.get_mut(&h) {
            *x = tid;
        }
    });
    h
}
fn _endthreadex(c: &mut Ctx) -> u32 {
    exit_thread(c.a(0))
}
fn SetThreadExecutionState(_: &mut Ctx) -> u32 {
    0x8000_0000 // ES_CONTINUOUS
}
fn _endthread(_: &mut Ctx) -> u32 {
    exit_thread(0)
}
fn FreeLibraryAndExitThread(c: &mut Ctx) -> u32 {
    exit_thread(c.a(1))
}
/// PROCESS_MEMORY_COUNTERS: a modest working set
fn K32GetProcessMemoryInfo(c: &mut Ctx) -> u32 {
    let (p, cb) = (c.a(1), c.a(2));
    mem::fill(p, cb.min(44), 0);
    w32(p, cb.min(44));
    for off in [8, 12, 16, 20, 32, 36, 40] {
        if off + 4 <= cb {
            w32(p + off, 64 << 20);
        }
    }
    1
}

/// Threads created suspended wait here: tid -> suspend count.
static GATE: Mutex<Vec<(u32, u32)>> = Mutex::new(Vec::new());
static GATE_CV: Condvar = Condvar::new();

fn CreateThread(c: &mut Ctx) -> u32 {
    let (stack, start, param, flags, ptid) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let h = obj::add(Obj::Thread { tid: 0, done: false, code: 0x103 });
    let suspended = flags & 4 != 0;
    // a suspended thread is started by its first ResumeThread
    let stack = if stack == 0 { 0x10_0000 } else { stack.max(0x10000) };
    if suspended {
        GATE.lock().unwrap().push((h, 1));
    }
    let tid = crate::spawn_gated(start, param, stack, h, suspended);
    obj::with(|t| {
        if let Some((Obj::Thread { tid: x, .. }, _)) = t.objs.get_mut(&h) {
            *x = tid;
        }
    });
    put32(ptid, tid);
    h
}

/// Blocks a new thread created suspended until ResumeThread.
pub fn wait_gate(h: u32) {
    let mut g = GATE.lock().unwrap();
    while g.iter().any(|&(x, n)| x == h && n > 0) {
        g = GATE_CV.wait(g).unwrap();
    }
    g.retain(|&(x, _)| x != h);
}

fn ResumeThread(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let mut g = GATE.lock().unwrap();
    let r = match g.iter_mut().find(|(x, _)| *x == h) {
        Some((_, n)) => {
            let old = *n;
            *n = n.saturating_sub(1);
            old
        }
        None => 0,
    };
    GATE_CV.notify_all();
    r
}
/// SuspendThread: a thread suspending itself (WOLF RPG's workers) waits for ResumeThread; other
/// threads can't be stopped from outside here and go on (0)
fn SuspendThread(c: &mut Ctx) -> u32 {
    let tid = crate::cur().tid;
    let h = match c.a(0) {
        0xffff_fffe => obj::with(|t| {
            t.objs.iter().find_map(|(&h, (o, _))| matches!(o, Obj::Thread { tid: x, .. } if *x == tid).then_some(h))
        }),
        h => obj::with(|t| matches!(t.objs.get(&h), Some((Obj::Thread { tid: x, .. }, _)) if *x == tid).then_some(h)),
    };
    let Some(h) = h else { return 0 };
    let mut g = GATE.lock().unwrap();
    match g.iter_mut().find(|(x, _)| *x == h) {
        Some((_, n)) => *n += 1,
        None => g.push((h, 1)),
    }
    while g.iter().any(|&(x, n)| x == h && n > 0) {
        g = GATE_CV.wait(g).unwrap();
    }
    g.retain(|&(x, _)| x != h);
    0
}
fn GetExitCodeThread(c: &mut Ctx) -> u32 {
    let code = obj::with(|t| match t.objs.get(&c.a(0)) {
        Some((Obj::Thread { code, .. }, _)) => Some(*code),
        _ => None,
    });
    match code {
        Some(v) => {
            put32(c.a(1), v);
            1
        }
        None => 0,
    }
}
fn GetExitCodeProcess(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0x103);
    1
}
fn TerminateThread(_: &mut Ctx) -> u32 {
    1
}
fn GetThreadPriority(_: &mut Ctx) -> u32 {
    0
}
fn SetThreadPriority(_: &mut Ctx) -> u32 {
    1
}
fn SetPriorityClass(_: &mut Ctx) -> u32 {
    1
}
fn GetPriorityClass(_: &mut Ctx) -> u32 {
    0x20
}
fn SetThreadAffinityMask(_: &mut Ctx) -> u32 {
    1
}
fn SetProcessAffinityMask(_: &mut Ctx) -> u32 {
    1
}
fn ncpu() -> u32 {
    std::thread::available_parallelism().map_or(4, |n| n.get() as u32).clamp(1, 8)
}
fn GetProcessAffinityMask(c: &mut Ctx) -> u32 {
    let m = (1u32 << ncpu()) - 1;
    put32(c.a(1), m);
    put32(c.a(2), m);
    1
}
fn SetThreadIdealProcessor(_: &mut Ctx) -> u32 {
    0
}
fn Sleep(c: &mut Ctx) -> u32 {
    let ms = c.a(0);
    if ms == 0 {
        std::thread::yield_now();
    } else {
        std::thread::sleep(std::time::Duration::from_millis(ms as u64));
    }
    0
}
fn SleepEx(c: &mut Ctx) -> u32 {
    Sleep(c);
    0
}
fn SwitchToThread(_: &mut Ctx) -> u32 {
    std::thread::yield_now();
    1
}

// ---------------------------------------------------------------------------------------------
// synchronization

fn named(name: u32, wide: bool) -> Option<String> {
    (name != 0).then(|| arg_str(name, wide))
}

fn create_named(name: Option<String>, o: Obj) -> u32 {
    if let Some(n) = &name {
        let existing = obj::with(|t| t.names.get(n).copied());
        if let Some(h) = existing {
            last_error(files::ERROR_ALREADY_EXISTS);
            return obj::dup(h);
        }
    }
    let h = obj::add(o);
    if let Some(n) = name {
        obj::with(|t| t.names.insert(n, h));
    }
    last_error(0);
    h
}
fn open_named(name: u32, wide: bool) -> u32 {
    let n = arg_str(name, wide);
    match obj::with(|t| t.names.get(&n).copied()) {
        Some(h) => obj::dup(h),
        None => {
            last_error(2);
            0
        }
    }
}
fn CreateEventA(c: &mut Ctx) -> u32 {
    create_named(named(c.a(3), false), Obj::Event { manual: c.a(1) != 0, set: c.a(2) != 0 })
}
fn CreateEventW(c: &mut Ctx) -> u32 {
    create_named(named(c.a(3), true), Obj::Event { manual: c.a(1) != 0, set: c.a(2) != 0 })
}
fn CreateEventExW(c: &mut Ctx) -> u32 {
    create_named(named(c.a(1), true), Obj::Event { manual: c.a(2) & 1 != 0, set: c.a(2) & 2 != 0 })
}
fn new_timer(manual: bool) -> Obj {
    Obj::Timer { manual, set: false, due: None, period: std::time::Duration::ZERO }
}
fn CreateWaitableTimerA(c: &mut Ctx) -> u32 {
    create_named(named(c.a(2), false), new_timer(c.a(1) != 0))
}
fn CreateWaitableTimerW(c: &mut Ctx) -> u32 {
    create_named(named(c.a(2), true), new_timer(c.a(1) != 0))
}
fn CreateWaitableTimerExW(c: &mut Ctx) -> u32 {
    create_named(named(c.a(1), true), new_timer(c.a(2) & 1 != 0))
}
fn OpenWaitableTimerA(c: &mut Ctx) -> u32 {
    open_named(c.a(2), false)
}
fn OpenWaitableTimerW(c: &mut Ctx) -> u32 {
    open_named(c.a(2), true)
}
/// SetWaitableTimer(h, due, period ms, apc, arg, resume): `due` in 100 ns, negative = from now,
/// positive = an absolute FILETIME. (The completion routine is not called.)
fn SetWaitableTimer(c: &mut Ctx) -> u32 {
    use std::time::{Duration, Instant, SystemTime};
    let v = get64(c.a(1)) as i64;
    let after = if v < 0 {
        Duration::from_nanos(v.unsigned_abs() * 100)
    } else {
        let now = files::filetime(SystemTime::now()) as i64;
        Duration::from_nanos((v - now).max(0) as u64 * 100)
    };
    let period = Duration::from_millis(c.a(2) as u64);
    let r = obj::with(|t| match t.objs.get_mut(&c.a(0)) {
        Some((Obj::Timer { set, due, period: p, .. }, _)) => {
            *set = false;
            *due = Some(Instant::now() + after);
            *p = period;
            true
        }
        _ => false,
    });
    obj::signal();
    ok(r)
}
fn CancelWaitableTimer(c: &mut Ctx) -> u32 {
    ok(obj::with(|t| match t.objs.get_mut(&c.a(0)) {
        Some((Obj::Timer { due, .. }, _)) => {
            *due = None;
            true
        }
        _ => false,
    }))
}
fn OpenEventA(c: &mut Ctx) -> u32 {
    open_named(c.a(2), false)
}
fn OpenEventW(c: &mut Ctx) -> u32 {
    open_named(c.a(2), true)
}
pub fn event_set(h: u32, v: bool) -> u32 {
    let r = obj::with(|t| match t.objs.get_mut(&h) {
        Some((Obj::Event { set, .. }, _)) => {
            *set = v;
            true
        }
        _ => false,
    });
    if r && v {
        obj::signal();
    }
    ok(r)
}
fn SetEvent(c: &mut Ctx) -> u32 {
    event_set(c.a(0), true)
}
fn ResetEvent(c: &mut Ctx) -> u32 {
    event_set(c.a(0), false)
}
fn PulseEvent(c: &mut Ctx) -> u32 {
    event_set(c.a(0), true);
    std::thread::yield_now();
    event_set(c.a(0), false)
}
fn CreateMutexA(c: &mut Ctx) -> u32 {
    let tid = crate::cur().tid;
    let own = c.a(1) != 0;
    create_named(named(c.a(2), false), Obj::Mutex { owner: if own { tid } else { 0 }, count: own as u32 })
}
fn CreateMutexW(c: &mut Ctx) -> u32 {
    let tid = crate::cur().tid;
    let own = c.a(1) != 0;
    create_named(named(c.a(2), true), Obj::Mutex { owner: if own { tid } else { 0 }, count: own as u32 })
}
fn OpenMutexA(c: &mut Ctx) -> u32 {
    open_named(c.a(2), false)
}
fn ReleaseMutex(c: &mut Ctx) -> u32 {
    let tid = crate::cur().tid;
    let r = obj::with(|t| match t.objs.get_mut(&c.a(0)) {
        Some((Obj::Mutex { owner, count }, _)) if *owner == tid => {
            *count -= 1;
            if *count == 0 {
                *owner = 0;
            }
            true
        }
        _ => false,
    });
    obj::signal();
    ok(r)
}
fn CreateSemaphoreA(c: &mut Ctx) -> u32 {
    create_named(named(c.a(3), false), Obj::Semaphore { count: c.i(1), max: c.i(2) })
}
fn CreateSemaphoreW(c: &mut Ctx) -> u32 {
    create_named(named(c.a(3), true), Obj::Semaphore { count: c.i(1), max: c.i(2) })
}
fn ReleaseSemaphore(c: &mut Ctx) -> u32 {
    let r = obj::with(|t| match t.objs.get_mut(&c.a(0)) {
        Some((Obj::Semaphore { count, max }, _)) => {
            let prev = *count;
            if *count + c.i(1) > *max {
                return None;
            }
            *count += c.i(1);
            Some(prev)
        }
        _ => None,
    });
    obj::signal();
    match r {
        Some(p) => {
            put32(c.a(2), p as u32);
            1
        }
        None => 0,
    }
}
fn WaitForSingleObject(c: &mut Ctx) -> u32 {
    obj::wait(&[c.a(0)], false, c.a(1), None)
}
fn WaitForSingleObjectEx(c: &mut Ctx) -> u32 {
    WaitForSingleObject(c)
}
fn WaitForMultipleObjects(c: &mut Ctx) -> u32 {
    let hs: Vec<u32> = (0..c.a(0).min(64)).map(|i| r32(c.a(1) + 4 * i)).collect();
    obj::wait(&hs, c.a(2) != 0, c.a(3), None)
}
fn WaitForMultipleObjectsEx(c: &mut Ctx) -> u32 {
    WaitForMultipleObjects(c)
}
fn CloseHandle(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    ok(obj::close(h) || h == obj::CURRENT_PROCESS || h == obj::CURRENT_THREAD || h < 0x10)
}
fn DuplicateHandle(c: &mut Ctx) -> u32 {
    let src = c.a(1);
    let h = if src == obj::CURRENT_THREAD {
        obj::dup(crate::cur().obj)
    } else if src == obj::CURRENT_PROCESS {
        obj::add(Obj::Process)
    } else {
        obj::dup(src)
    };
    put32(c.a(3), h);
    ok(h != 0)
}

// critical sections: OwningThread is the lock word (CAS), LockSemaphore counts waiters
static CS_LOCK: Mutex<()> = Mutex::new(());
static CS_CV: Condvar = Condvar::new();

fn atomic(a: u32) -> &'static AtomicU32 {
    unsafe { &*(a as usize as *const AtomicU32) }
}

/// An RTL_CRITICAL_SECTION_DEBUG for `cs` (Wine's msvcr90 names its locks in DebugInfo->Spare).
fn cs_debug_info(cs: u32) {
    let d = mem::heap_alloc(32, true);
    w32(d + 4, cs);
    w32(cs, d);
}
fn cs_init(cs: u32) {
    w32(cs, 0);
    w32(cs + 4, 0xffff_ffff);
    w32(cs + 8, 0);
    w32(cs + 12, 0);
    w32(cs + 16, 0);
    w32(cs + 20, 0);
}
pub fn cs_enter(cs: u32) {
    let me = crate::cur().tid;
    let own = atomic(cs + 12);
    if own.load(Ordering::Acquire) == me {
        w32(cs + 8, r32(cs + 8) + 1);
        return;
    }
    let mut spins = 0;
    loop {
        if own.compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            w32(cs + 8, 1);
            w32(cs + 4, 0);
            return;
        }
        spins += 1;
        if spins < 50 {
            std::hint::spin_loop();
            continue;
        }
        let waiters = atomic(cs + 16);
        let mut g = CS_LOCK.lock().unwrap();
        waiters.fetch_add(1, Ordering::SeqCst);
        while own.load(Ordering::SeqCst) != 0 {
            g = CS_CV.wait_timeout(g, std::time::Duration::from_millis(50)).unwrap().0;
        }
        waiters.fetch_sub(1, Ordering::SeqCst);
        drop(g);
    }
}
pub fn cs_leave(cs: u32) {
    let n = r32(cs + 8);
    if n > 1 {
        w32(cs + 8, n - 1);
        return;
    }
    w32(cs + 8, 0);
    w32(cs + 4, 0xffff_ffff);
    atomic(cs + 12).store(0, Ordering::SeqCst);
    if atomic(cs + 16).load(Ordering::SeqCst) != 0 {
        let _g = CS_LOCK.lock().unwrap();
        CS_CV.notify_all();
    }
}
/// Wakes the waiters of `cs` (fast::leave_cs found some after releasing it).
pub fn cs_wake(_cs: u32) {
    let _g = CS_LOCK.lock().unwrap();
    CS_CV.notify_all();
}
fn InitializeCriticalSection(c: &mut Ctx) -> u32 {
    cs_init(c.a(0));
    cs_debug_info(c.a(0));
    0
}
fn InitializeCriticalSectionAndSpinCount(c: &mut Ctx) -> u32 {
    cs_init(c.a(0));
    cs_debug_info(c.a(0));
    1
}
fn InitializeCriticalSectionEx(c: &mut Ctx) -> u32 {
    cs_init(c.a(0));
    // (unless RTL_CRITICAL_SECTION_FLAG_NO_DEBUG_INFO)
    if c.a(2) & 0x0100_0000 == 0 {
        cs_debug_info(c.a(0));
    }
    1
}
fn DeleteCriticalSection(c: &mut Ctx) -> u32 {
    let d = r32(c.a(0));
    if d != 0 && d != u32::MAX && r32(d + 4) == c.a(0) {
        w32(c.a(0), 0);
        mem::heap_free(d);
    }
    0
}
fn EnterCriticalSection(c: &mut Ctx) -> u32 {
    cs_enter(c.a(0));
    0
}
fn LeaveCriticalSection(c: &mut Ctx) -> u32 {
    cs_leave(c.a(0));
    0
}
fn TryEnterCriticalSection(c: &mut Ctx) -> u32 {
    let cs = c.a(0);
    let me = crate::cur().tid;
    let own = atomic(cs + 12);
    if own.load(Ordering::Acquire) == me {
        w32(cs + 8, r32(cs + 8) + 1);
        return 1;
    }
    if own.compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        w32(cs + 8, 1);
        return 1;
    }
    0
}
fn RtlEnterCriticalSection(c: &mut Ctx) -> u32 {
    cs_enter(c.a(0));
    0
}
fn RtlLeaveCriticalSection(c: &mut Ctx) -> u32 {
    cs_leave(c.a(0));
    0
}

// slim reader/writer locks (word: bit 0 exclusive, the rest 2 x readers), condition variables
// (a generation per address) and one-time init, all waiting on one Condvar
static SYNC: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
static SYNC_CV: Condvar = Condvar::new();

fn srw_try(l: u32, shared: bool) -> bool {
    let a = atomic(l);
    let v = a.load(Ordering::SeqCst);
    if shared {
        v & 1 == 0 && a.compare_exchange(v, v + 2, Ordering::SeqCst, Ordering::Relaxed).is_ok()
    } else {
        a.compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed).is_ok()
    }
}
fn srw_acquire(l: u32, shared: bool) {
    for _ in 0..50 {
        if srw_try(l, shared) {
            return;
        }
        std::hint::spin_loop();
    }
    let mut g = SYNC.lock().unwrap();
    while !srw_try(l, shared) {
        g = SYNC_CV.wait_timeout(g, std::time::Duration::from_millis(20)).unwrap().0;
    }
}
fn srw_release(l: u32, shared: bool) {
    let a = atomic(l);
    if shared {
        a.fetch_sub(2, Ordering::SeqCst);
    } else {
        a.store(0, Ordering::SeqCst);
    }
    let _g = SYNC.lock().unwrap();
    SYNC_CV.notify_all();
}
fn InitializeSRWLock(c: &mut Ctx) -> u32 {
    w32(c.a(0), 0);
    0
}
fn AcquireSRWLockExclusive(c: &mut Ctx) -> u32 {
    srw_acquire(c.a(0), false);
    0
}
fn AcquireSRWLockShared(c: &mut Ctx) -> u32 {
    srw_acquire(c.a(0), true);
    0
}
fn ReleaseSRWLockExclusive(c: &mut Ctx) -> u32 {
    srw_release(c.a(0), false);
    0
}
fn ReleaseSRWLockShared(c: &mut Ctx) -> u32 {
    srw_release(c.a(0), true);
    0
}
fn TryAcquireSRWLockExclusive(c: &mut Ctx) -> u32 {
    srw_try(c.a(0), false) as u32
}
fn TryAcquireSRWLockShared(c: &mut Ctx) -> u32 {
    srw_try(c.a(0), true) as u32
}
fn InitializeConditionVariable(c: &mut Ctx) -> u32 {
    w32(c.a(0), 0);
    0
}
/// Waits for a wake of `cv` (after `release`, before `reacquire`); false on timeout.
fn cv_wait(cv: u32, ms: u32, release: impl FnOnce(), reacquire: impl FnOnce()) -> bool {
    // the generation is read before the lock is released (a wake in between is seen), but SYNC is
    // not held across `release` (srw_release takes it to notify: holding it deadlocked)
    let gen0 = *SYNC.lock().unwrap().get_or_insert_with(HashMap::default).entry(cv).or_insert(0);
    release();
    let mut g = SYNC.lock().unwrap();
    let end = (ms != u32::MAX).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms as u64));
    let mut woke = true;
    while g.as_ref().unwrap().get(&cv).copied() == Some(gen0) {
        let left = match end {
            Some(e) => match e.checked_duration_since(std::time::Instant::now()) {
                Some(d) => d.min(std::time::Duration::from_millis(50)),
                None => {
                    woke = false;
                    break;
                }
            },
            None => std::time::Duration::from_millis(50),
        };
        g = SYNC_CV.wait_timeout(g, left).unwrap().0;
    }
    drop(g);
    reacquire();
    if !woke {
        last_error(1460);
    }
    woke
}
fn cv_wake(cv: u32) {
    let mut g = SYNC.lock().unwrap();
    let e = g.get_or_insert_with(HashMap::default).entry(cv).or_insert(0);
    *e = e.wrapping_add(1);
    SYNC_CV.notify_all();
}
fn SleepConditionVariableSRW(c: &mut Ctx) -> u32 {
    let (cv, l, ms, shared) = (c.a(0), c.a(1), c.a(2), c.a(3) & 1 != 0);
    cv_wait(cv, ms, || srw_release(l, shared), || srw_acquire(l, shared)) as u32
}
fn SleepConditionVariableCS(c: &mut Ctx) -> u32 {
    let (cv, cs, ms) = (c.a(0), c.a(1), c.a(2));
    // a recursively entered section is left and entered again as a whole
    let depth = r32(cs + 8).max(1);
    cv_wait(
        cv,
        ms,
        || {
            w32(cs + 8, 1);
            cs_leave(cs)
        },
        || {
            cs_enter(cs);
            w32(cs + 8, depth)
        },
    ) as u32
}
fn WakeConditionVariable(c: &mut Ctx) -> u32 {
    cv_wake(c.a(0));
    0
}
fn WakeAllConditionVariable(c: &mut Ctx) -> u32 {
    cv_wake(c.a(0));
    0
}
/// InitOnceExecuteOnce(once, fn, param, context): the word is 0 (new), 1 (running), 2 (done).
fn InitOnceExecuteOnce(c: &mut Ctx) -> u32 {
    let (once, f, param, ctx) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let a = atomic(once);
    loop {
        match a.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => {
                let r = c.call(f, &[once, param, ctx]);
                a.store(if r != 0 { 2 } else { 0 }, Ordering::SeqCst);
                let _g = SYNC.lock().unwrap();
                SYNC_CV.notify_all();
                return r;
            }
            Err(2) => return 1,
            Err(_) => {
                let g = SYNC.lock().unwrap();
                if a.load(Ordering::SeqCst) == 1 {
                    let _ = SYNC_CV.wait_timeout(g, std::time::Duration::from_millis(20));
                }
            }
        }
    }
}
/// No debugger: the debug registers read as zero (games check them against hooks).
fn GetThreadContext(c: &mut Ctx) -> u32 {
    let ctx = c.a(1);
    for i in 1..7 {
        w32(ctx + 4 * i, 0);
    }
    1
}
fn SetThreadContext(_: &mut Ctx) -> u32 {
    1
}
fn InitOnceInitialize(c: &mut Ctx) -> u32 {
    w32(c.a(0), 0);
    0
}

fn InterlockedIncrement(c: &mut Ctx) -> u32 {
    atomic(c.a(0)).fetch_add(1, Ordering::SeqCst).wrapping_add(1)
}
fn InterlockedDecrement(c: &mut Ctx) -> u32 {
    atomic(c.a(0)).fetch_sub(1, Ordering::SeqCst).wrapping_sub(1)
}
fn InterlockedExchange(c: &mut Ctx) -> u32 {
    atomic(c.a(0)).swap(c.a(1), Ordering::SeqCst)
}
fn InterlockedExchangeAdd(c: &mut Ctx) -> u32 {
    atomic(c.a(0)).fetch_add(c.a(1), Ordering::SeqCst)
}
fn InterlockedCompareExchange(c: &mut Ctx) -> u32 {
    atomic(c.a(0)).compare_exchange(c.a(2), c.a(1), Ordering::SeqCst, Ordering::SeqCst).unwrap_or_else(|v| v)
}

// TLS slots: TEB+0xE10 (64), then TEB+0xF94 -> 1024 more
static TLS_USED: Mutex<Vec<bool>> = Mutex::new(Vec::new());

pub fn tls_thread_init(_teb: u32) {}

fn tls_slot(i: u32) -> u32 {
    let teb = crate::cur().teb;
    let (slots, exp, k) = if mem::x64() { (0x1480, 0x1780, 8) } else { (0xe10, 0xf94, 4) };
    if i < 64 {
        return teb + slots + k * i;
    }
    let mut ex = r32(teb + exp);
    if ex == 0 {
        ex = mem::heap_alloc(1024 * k, true);
        mem::wp(teb + exp, ex);
    }
    ex + k * (i - 64)
}
fn TlsAlloc(_: &mut Ctx) -> u32 {
    let mut u = TLS_USED.lock().unwrap();
    if u.is_empty() {
        u.resize(1088, false);
    }
    match u.iter().position(|x| !x) {
        Some(i) => {
            u[i] = true;
            i as u32
        }
        None => 0xffff_ffff,
    }
}
fn TlsFree(c: &mut Ctx) -> u32 {
    if let Some(x) = TLS_USED.lock().unwrap().get_mut(c.a(0) as usize) {
        *x = false;
    }
    1
}
fn TlsGetValue(c: &mut Ctx) -> u32 {
    let i = c.a(0);
    if i >= 1088 {
        last_error(87);
        return 0;
    }
    last_error(0);
    r32(tls_slot(i))
}
fn TlsSetValue(c: &mut Ctx) -> u32 {
    let i = c.a(0);
    if i >= 1088 {
        return 0;
    }
    mem::wp(tls_slot(i), c.a(1));
    1
}
fn FlsAlloc(c: &mut Ctx) -> u32 {
    TlsAlloc(c)
}
fn FlsFree(c: &mut Ctx) -> u32 {
    TlsFree(c)
}
fn FlsGetValue(c: &mut Ctx) -> u32 {
    TlsGetValue(c)
}
fn FlsSetValue(c: &mut Ctx) -> u32 {
    TlsSetValue(c)
}

fn GetLastError(_: &mut Ctx) -> u32 {
    r32(crate::cur().teb + crate::teb_last_error())
}
fn SetLastError(c: &mut Ctx) -> u32 {
    last_error(c.a(0));
    0
}
fn SetErrorMode(_: &mut Ctx) -> u32 {
    0
}
fn GetErrorMode(_: &mut Ctx) -> u32 {
    0
}

fn RtlUnwind(c: &mut Ctx) -> u32 {
    crate::seh::rtl_unwind(c)
}
fn RaiseException(c: &mut Ctx) -> u32 {
    crate::seh::raise_exception(c)
}
fn SetUnhandledExceptionFilter(c: &mut Ctx) -> u32 {
    crate::seh::TOP_FILTER.swap(c.a(0), Ordering::Relaxed)
}
/// UnhandledExceptionFilter(EXCEPTION_POINTERS*): the program's top-level filter decides (the
/// CRT's _XcptFilter reaches here); none: execute the handler (terminate)
fn UnhandledExceptionFilter(c: &mut Ctx) -> u32 {
    match crate::seh::TOP_FILTER.load(Ordering::Relaxed) {
        0 => 1,
        f => c.call(f, &[c.a(0)]),
    }
}
fn IsDebuggerPresent(_: &mut Ctx) -> u32 {
    0
}
fn OutputDebugStringA(c: &mut Ctx) -> u32 {
    if std::env::var_os("SAKE_DEBUGSTR").is_some() {
        eprintln!("sake: debug: {}", arg_str(c.a(0), false).trim_end());
    }
    0
}
fn OutputDebugStringW(c: &mut Ctx) -> u32 {
    if std::env::var_os("SAKE_DEBUGSTR").is_some() {
        eprintln!("sake: debug: {}", arg_str(c.a(0), true).trim_end());
    }
    0
}
fn SetConsoleCtrlHandler(_: &mut Ctx) -> u32 {
    1
}
fn DebugBreak(_: &mut Ctx) -> u32 {
    0
}
fn EncodePointer(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn DecodePointer(c: &mut Ctx) -> u32 {
    c.a(0)
}

// ---------------------------------------------------------------------------------------------
// memory

fn VirtualAlloc(c: &mut Ctx) -> u32 {
    let (addr, size, ty, prot) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let reserve = ty & 0x2000 != 0;
    let commit = ty & 0x1000 != 0;
    if addr != 0 && !reserve {
        // commit inside an existing reservation
        if commit && mem::commit_range(addr, size, prot) {
            return addr & !(mem::PAGE - 1);
        }
        if ty & 0x80000 != 0 && mem::find(addr).is_some() {
            return addr & !(mem::PAGE - 1); // MEM_RESET
        }
        last_error(487);
        return 0;
    }
    if !reserve && !commit {
        last_error(87);
        return 0;
    }
    let Some(b) = mem::reserve(addr, size, prot, 0x20000, ty & 0x10_0000 != 0) else {
        last_error(if addr != 0 { 487 } else { 8 });
        return 0;
    };
    if commit {
        mem::commit(b, prot);
    }
    b
}
fn VirtualFree(c: &mut Ctx) -> u32 {
    let (addr, size, ty) = (c.a(0), c.a(1), c.a(2));
    if ty & 0x8000 != 0 {
        if size != 0 || mem::find(addr).is_none_or(|(b, _)| b != addr) {
            last_error(87);
            return 0;
        }
        return ok(mem::release(addr));
    }
    if ty & 0x4000 != 0 {
        if !mem::decommit_range(addr, size) {
            last_error(487);
            return 0;
        }
        return 1;
    }
    last_error(87);
    0
}
fn VirtualQuery(c: &mut Ctx) -> u32 {
    let (a, mbi, len) = (c.a(0), c.a(1), c.a(2));
    if len < 28 {
        return 0;
    }
    let page = a & !(mem::PAGE - 1);
    match mem::query(a) {
        Some((b, r, start, n, state, prot)) => {
            w32(mbi, start.max(page));
            w32(mbi + 4, b);
            w32(mbi + 8, r.alloc_prot);
            w32(mbi + 12, n - (page - start));
            w32(mbi + 16, state);
            w32(mbi + 20, prot);
            w32(mbi + 24, r.kind);
        }
        None => {
            let (lo, n) = mem::free_at(a);
            w32(mbi, lo.max(page));
            w32(mbi + 4, 0);
            w32(mbi + 8, 0);
            w32(mbi + 12, n - (page.max(lo) - lo));
            w32(mbi + 16, 0x10000);
            w32(mbi + 20, 1);
            w32(mbi + 24, 0);
        }
    }
    28
}
fn VirtualProtect(c: &mut Ctx) -> u32 {
    match mem::protect(c.a(0), c.a(1), c.a(2)) {
        Some(old) => {
            put32(c.a(3), old);
            1
        }
        None => {
            last_error(487);
            0
        }
    }
}
fn VirtualLock(_: &mut Ctx) -> u32 {
    1
}
fn VirtualUnlock(_: &mut Ctx) -> u32 {
    1
}
fn FlushInstructionCache(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        vnhost::jit::code_changed(c.a(1) as usize, c.a(2).max(1) as usize);
    }
    1
}
fn is_bad(p: u32, n: u32) -> bool {
    if n == 0 {
        return false;
    }
    if p < mem::LO {
        return true;
    }
    match mem::find(p) {
        Some((b, r)) => r.state != 0x1000 || p as u64 + n as u64 > b as u64 + r.size as u64,
        None => true,
    }
}
fn IsBadReadPtr(c: &mut Ctx) -> u32 {
    ok(is_bad(c.a(0), c.a(1)))
}
fn IsBadWritePtr(c: &mut Ctx) -> u32 {
    ok(is_bad(c.a(0), c.a(1)))
}
fn IsBadCodePtr(c: &mut Ctx) -> u32 {
    ok(is_bad(c.a(0), 1))
}
fn IsBadStringPtrA(c: &mut Ctx) -> u32 {
    ok(is_bad(c.a(0), 1))
}
fn IsBadStringPtrW(c: &mut Ctx) -> u32 {
    ok(is_bad(c.a(0), 2))
}
fn NtQuerySystemTime(c: &mut Ctx) -> u32 {
    put64(c.a(0), files::filetime(std::time::SystemTime::now()));
    0
}
fn RtlTimeToSecondsSince1970(c: &mut Ctx) -> u32 {
    let s = (get64(c.a(0)) / 10_000_000) as i64 - 11_644_473_600;
    if !(0..=u32::MAX as i64).contains(&s) {
        return 0;
    }
    put32(c.a(1), s as u32);
    1
}
fn locale_name() -> &'static str {
    if crate::text::acp() == 932 { "ja-JP" } else { "en-US" }
}
fn GetUserDefaultLocaleName(c: &mut Ctx) -> u32 {
    let (out, n) = (c.a(0), c.a(1));
    let w: Vec<u16> = locale_name().encode_utf16().chain([0]).collect();
    if (n as usize) < w.len() {
        last_error(122);
        return 0;
    }
    for (i, &u) in w.iter().enumerate() {
        w16(out + 2 * i as u32, u);
    }
    w.len() as u32
}
const LOCALES: [(u32, &str); 16] = [
    (0x409, "en-US"), (0x411, "ja-JP"), (0x804, "zh-CN"), (0x404, "zh-TW"), (0x412, "ko-KR"), (0x809, "en-GB"),
    (0x407, "de-DE"), (0x40c, "fr-FR"), (0x410, "it-IT"), (0xc0a, "es-ES"), (0x419, "ru-RU"), (0x416, "pt-BR"),
    (0x816, "pt-PT"), (0x413, "nl-NL"), (0x41d, "sv-SE"), (0x7f, ""),
];
fn LocaleNameToLCID(c: &mut Ctx) -> u32 {
    let n = if c.a(0) == 0 { locale_name().to_string() } else { String::from_utf16_lossy(&wstr(c.a(0))) };
    let n = n.to_ascii_lowercase();
    match n.as_str() {
        "" | "!x-sys-default-locale" => if crate::text::acp() == 932 { 0x411 } else { 0x409 },
        _ => LOCALES.iter().find(|l| l.1.eq_ignore_ascii_case(&n) || l.1.split('-').next() == Some(n.as_str()) && !n.is_empty()).map_or(0x409, |l| l.0),
    }
}
/// GetLocaleInfoEx(name, type, buffer, count): GetLocaleInfoW of the name's LCID
fn GetLocaleInfoEx(c: &mut Ctx) -> u32 {
    let f = crate::api::export("kernel32", "LocaleNameToLCID");
    let lcid = c.call(f, &[c.a(0), 0]);
    let f = crate::api::export("kernel32", "GetLocaleInfoW");
    c.call(f, &[lcid, c.a(1), c.a(2), c.a(3)])
}
/// (lcid, name, count, flags)
fn LCIDToLocaleName(c: &mut Ctx) -> u32 {
    let lcid = match c.a(0) & 0xffff {
        0x400 | 0x800 | 0 => if crate::text::acp() == 932 { 0x411 } else { 0x409 },
        l => l,
    };
    let Some(&(_, name)) = LOCALES.iter().find(|l| l.0 == lcid) else {
        last_error(87);
        return 0;
    };
    let w: Vec<u16> = name.encode_utf16().chain([0]).collect();
    if c.a(2) == 0 {
        return w.len() as u32;
    }
    if (c.a(2) as usize) < w.len() {
        last_error(122);
        return 0;
    }
    for (i, &u) in w.iter().enumerate() {
        w16(c.a(1) + 2 * i as u32, u);
    }
    w.len() as u32
}
fn wine_dbg_strdup(c: &mut Ctx) -> u32 {
    c.a(0)
}
/// LdrGetDllHandle(path, flags, UNICODE_STRING *name, HMODULE *out)
fn LdrGetDllHandle(c: &mut Ctx) -> u32 {
    let us = c.a(2);
    let h = if us == 0 { 0 } else { module_handle(r32(us + 4), true) };
    put32(c.a(3), h);
    if h == 0 { 0xc000_0135 } else { 0 }
}
fn RtlFindExportedRoutineByName(c: &mut Ctx) -> u32 {
    let n = a2s(&cstr(c.a(1)));
    if n == "__wine_unix_call_dispatcher" {
        return crate::unixlib::dispatcher_cell();
    }
    crate::pe::proc_address(c.a(0), crate::pe::Import::Name(&n)).unwrap_or(0)
}

/// (process, address, class, info, length, *returned): basic information, or the handle of a
/// Wine DLL's Unix side (MemoryWineUnixFuncs)
fn NtQueryVirtualMemory(c: &mut Ctx) -> u32 {
    let (a, class, info, len, ret) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    match class {
        0 => {
            let f = crate::api::export("kernel32", "VirtualQuery");
            let n = c.call(f, &[a, info, len]);
            if ret != 0 {
                w32(ret, n);
            }
            if n == 0 { 0xc000_0004 } else { 0 }
        }
        1000 => {
            let base = crate::pe::containing(a).unwrap_or(a);
            match crate::unixlib::handle_for(base) {
                Ok(h) if len >= 8 => {
                    w32(info, h as u32);
                    w32(info + 4, (h >> 32) as u32);
                    0
                }
                Ok(_) => 0xc000_0004,
                Err(e) => e,
            }
        }
        _ => 0xc000_0003,
    }
}

fn GetProcessHeap(_: &mut Ctx) -> u32 {
    PROCESS_HEAP
}
static NEXT_HEAP: AtomicU32 = AtomicU32::new(0x0016_0000);
fn HeapCreate(_: &mut Ctx) -> u32 {
    NEXT_HEAP.fetch_add(0x10000, Ordering::Relaxed)
}
fn HeapDestroy(_: &mut Ctx) -> u32 {
    1
}
fn HeapAlloc(c: &mut Ctx) -> u32 {
    let p = mem::heap_alloc_small(c.a(2), c.a(1) & 8 != 0);
    if p == 0 && c.a(1) & 4 != 0 {
        let ret = r32(c.esp);
        c.cpu.eip = ret;
        crate::seh::raise(c.cpu, 0xc000_0017, 0, ret, &[]);
        c.jumped = true;
    }
    p
}
fn HeapFree(c: &mut Ctx) -> u32 {
    let p = c.a(2);
    ok(p == 0 || mem::heap_free_small(p))
}
fn HeapReAlloc(c: &mut Ctx) -> u32 {
    let (flags, p, size) = (c.a(1), c.a(2), c.a(3));
    if flags & 0x10 != 0 && size > mem::heap_size(p) {
        return 0;
    }
    if flags & 0x10 != 0 {
        return p;
    }
    mem::heap_realloc(p, size, flags & 8 != 0)
}
fn HeapSize(c: &mut Ctx) -> u32 {
    mem::heap_size(c.a(2))
}
fn HeapValidate(_: &mut Ctx) -> u32 {
    1
}
fn HeapCompact(_: &mut Ctx) -> u32 {
    0x10_0000
}
fn HeapSetInformation(_: &mut Ctx) -> u32 {
    1
}
fn HeapLock(_: &mut Ctx) -> u32 {
    1
}
fn HeapUnlock(_: &mut Ctx) -> u32 {
    1
}
fn RtlAllocateHeap(c: &mut Ctx) -> u32 {
    mem::heap_alloc(c.a(2), c.a(1) & 8 != 0)
}
fn RtlFreeHeap(c: &mut Ctx) -> u32 {
    ok(mem::heap_free(c.a(2)))
}
fn GlobalAlloc(c: &mut Ctx) -> u32 {
    mem::heap_alloc(c.a(1).max(1), c.a(0) & 0x40 != 0)
}
fn GlobalFree(c: &mut Ctx) -> u32 {
    mem::heap_free(c.a(0));
    0
}
fn GlobalReAlloc(c: &mut Ctx) -> u32 {
    if c.a(2) & 0x80 != 0 {
        // GMEM_MODIFY
        return c.a(0);
    }
    mem::heap_realloc(c.a(0), c.a(1).max(1), c.a(2) & 0x40 != 0)
}
fn GlobalLock(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn GlobalUnlock(_: &mut Ctx) -> u32 {
    1
}
fn GlobalHandle(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn GlobalSize(c: &mut Ctx) -> u32 {
    mem::heap_size(c.a(0))
}
fn GlobalFlags(_: &mut Ctx) -> u32 {
    0
}
fn LocalAlloc(c: &mut Ctx) -> u32 {
    mem::heap_alloc(c.a(1).max(1), c.a(0) & 0x40 != 0)
}
fn LocalFree(c: &mut Ctx) -> u32 {
    mem::heap_free(c.a(0));
    0
}
fn LocalReAlloc(c: &mut Ctx) -> u32 {
    mem::heap_realloc(c.a(0), c.a(1).max(1), c.a(2) & 0x40 != 0)
}
fn LocalLock(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn LocalUnlock(_: &mut Ctx) -> u32 {
    1
}
fn LocalSize(c: &mut Ctx) -> u32 {
    mem::heap_size(c.a(0))
}
fn LocalHandle(c: &mut Ctx) -> u32 {
    c.a(0)
}

const PHYS: u32 = 256 << 20;
fn GlobalMemoryStatus(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    w32(p, 32);
    w32(p + 4, 30);
    w32(p + 8, PHYS);
    w32(p + 12, PHYS / 4 * 3);
    w32(p + 16, 2 * PHYS);
    w32(p + 20, 2 * PHYS);
    w32(p + 24, 0x7ffe_0000);
    w32(p + 28, 0x6000_0000);
    0
}
fn GlobalMemoryStatusEx(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    w32(p, 64);
    w32(p + 4, 30);
    let q = |o: u32, v: u64| {
        w32(p + o, v as u32);
        w32(p + o + 4, (v >> 32) as u32);
    };
    q(8, PHYS as u64);
    q(16, PHYS as u64 / 4 * 3);
    q(24, 2 * PHYS as u64);
    q(32, 2 * PHYS as u64);
    q(40, 0x7ffe_0000);
    q(48, 0x6000_0000);
    q(56, 0);
    1
}

fn GetSystemInfo(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    mem::fill(p, 36, 0);
    w32(p + 4, 0x1000);
    w32(p + 8, 0x10000);
    w32(p + 12, 0x7ffe_ffff);
    w32(p + 16, (1u32 << ncpu()) - 1);
    w32(p + 20, ncpu());
    w32(p + 24, 586);
    w32(p + 28, 0x10000);
    w16(p + 32, 6);
    w16(p + 34, 0x0f01);
    0
}
// CPU topology: one group, one package, one NUMA node, one core per processor (the MSVC
// concurrency runtime asks before it starts; without it the game throws at startup)
fn cpu_mask() -> u32 {
    (1u32 << ncpu()) - 1
}
fn group_affinity(p: u32, mask: u32) {
    mem::fill(p, 12, 0);
    w32(p, mask);
}
/// GetLogicalProcessorInformationEx(relation, buffer, length*)
fn GetLogicalProcessorInformationEx(c: &mut Ctx) -> u32 {
    let (rel, buf, lenp) = (c.a(0), c.a(1), c.a(2));
    let all = rel == 0xffff;
    // (relationship, size, filler)
    let mut items: Vec<(u32, u32, Box<dyn Fn(u32)>)> = Vec::new();
    if all || rel == 0 {
        for i in 0..ncpu() {
            items.push((0, 44, Box::new(move |p| {
                w16(p + 8 + 22, 1);
                group_affinity(p + 8 + 24, 1 << i);
            })));
        }
    }
    if all || rel == 1 {
        items.push((1, 44, Box::new(|p| {
            w16(p + 8 + 22, 1);
            group_affinity(p + 8 + 24, cpu_mask());
        })));
    }
    if all || rel == 3 {
        items.push((3, 44, Box::new(|p| {
            w16(p + 8 + 22, 1);
            group_affinity(p + 8 + 24, cpu_mask());
        })));
    }
    if all || rel == 4 {
        items.push((4, 76, Box::new(|p| {
            w16(p + 8, 1);
            w16(p + 10, 1);
            w8(p + 8 + 24, ncpu() as u8);
            w8(p + 8 + 25, ncpu() as u8);
            w32(p + 8 + 24 + 40, cpu_mask());
        })));
    }
    let need: u32 = items.iter().map(|x| x.1).sum();
    if lenp == 0 || r32(lenp) < need || buf == 0 {
        if lenp != 0 {
            w32(lenp, need);
        }
        last_error(122);
        return 0;
    }
    let mut p = buf;
    for (r, size, fill) in &items {
        mem::fill(p, *size, 0);
        w32(p, *r);
        w32(p + 4, *size);
        fill(p);
        p += size;
    }
    w32(lenp, need);
    1
}
/// GetLogicalProcessorInformation(buffer, length*): 24-byte entries
fn GetLogicalProcessorInformation(c: &mut Ctx) -> u32 {
    let (buf, lenp) = (c.a(0), c.a(1));
    let n = ncpu() + 2;
    let need = 24 * n;
    if r32(lenp) < need || buf == 0 {
        w32(lenp, need);
        last_error(122);
        return 0;
    }
    mem::fill(buf, need, 0);
    for i in 0..ncpu() {
        w32(buf + 24 * i, 1 << i);
    }
    let q = buf + 24 * ncpu();
    w32(q, cpu_mask());
    w32(q + 4, 1);
    w32(q + 24, cpu_mask());
    w32(q + 28, 3);
    w32(lenp, need);
    1
}
fn GetNumaHighestNodeNumber(c: &mut Ctx) -> u32 {
    put32(c.a(0), 0);
    1
}
fn GetCurrentProcessorNumber(_: &mut Ctx) -> u32 {
    0
}
fn GetCurrentProcessorNumberEx(c: &mut Ctx) -> u32 {
    put32(c.a(0), 0);
    0
}
fn GetNumaNodeProcessorMaskEx(c: &mut Ctx) -> u32 {
    group_affinity(c.a(1), if c.a(0) & 0xffff == 0 { cpu_mask() } else { 0 });
    1
}
fn GetNumaNodeProcessorMask(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    w32(p, if c.a(0) & 0xff == 0 { cpu_mask() } else { 0 });
    w32(p + 4, 0);
    1
}
fn GetActiveProcessorCount(c: &mut Ctx) -> u32 {
    if matches!(c.a(0) & 0xffff, 0 | 0xffff) { ncpu() } else { 0 }
}
fn GetMaximumProcessorCount(c: &mut Ctx) -> u32 {
    GetActiveProcessorCount(c)
}
fn GetActiveProcessorGroupCount(_: &mut Ctx) -> u32 {
    1
}
fn GetMaximumProcessorGroupCount(_: &mut Ctx) -> u32 {
    1
}
fn GetThreadGroupAffinity(c: &mut Ctx) -> u32 {
    group_affinity(c.a(1), cpu_mask());
    1
}
fn SetThreadGroupAffinity(c: &mut Ctx) -> u32 {
    if c.a(2) != 0 {
        group_affinity(c.a(2), cpu_mask());
    }
    1
}
// ---------------------------------------------------------------------------------------------
// thread pool timers and waits (the MSVC concurrency runtime looks them up and calls them
// unchecked): each Set starts a guest thread that runs the callback once due / signalled

struct Tp {
    cb: u32,
    ctx: u32,
    /// callbacks started and not finished (WaitForThreadpoolWorkCallbacks)
    running: u32,
    /// bumped by every Set / cancel / close: a thread whose generation is stale calls nothing
    gen: u32,
}
static TPS: Mutex<Option<HashMap<u32, Tp>>> = Mutex::new(None);

fn tp_with<R>(f: impl FnOnce(&mut HashMap<u32, Tp>) -> R) -> R {
    f(TPS.lock().unwrap().get_or_insert_with(HashMap::default))
}
fn tp_new(c: &mut Ctx) -> u32 {
    let h = mem::heap_alloc(16, true);
    let (cb, ctx) = (c.a(0), c.a(1));
    tp_with(|m| m.insert(h, Tp { cb, ctx, gen: 0, running: 0 }));
    h
}
fn tp_bump(h: u32) -> Option<(u32, u32, u32)> {
    tp_with(|m| {
        m.get_mut(&h).map(|t| {
            t.gen = t.gen.wrapping_add(1);
            (t.cb, t.ctx, t.gen)
        })
    })
}
fn tp_current(h: u32, gen: u32) -> bool {
    tp_with(|m| m.get(&h).is_some_and(|t| t.gen == gen))
}
/// FILETIME* due time -> ms from now (negative: relative 100 ns units)
fn due_ms(p: u32) -> u64 {
    let v = get64(p) as i64;
    if v < 0 {
        (-v / 10_000) as u64
    } else {
        let now = files::filetime(std::time::SystemTime::now()) as i64;
        ((v - now).max(0) / 10_000) as u64
    }
}
fn tp_spawn(timer: bool, h: u32, gen: u32, arg: (u32, u32, u32)) {
    static T: OnceLock<u32> = OnceLock::new();
    static W: OnceLock<u32> = OnceLock::new();
    TP_ARGS.lock().unwrap().push((h, gen, arg));
    let entry = if timer {
        *T.get_or_init(|| crate::api::host_fn("tp_timer_thread", tp_timer_thread, 4))
    } else {
        *W.get_or_init(|| crate::api::host_fn("tp_wait_thread", tp_wait_thread, 4))
    };
    crate::spawn_gated(entry, h, 0x10000, obj::new_thread_obj(0), false);
}
static TP_ARGS: Mutex<Vec<(u32, u32, (u32, u32, u32))>> = Mutex::new(Vec::new());
fn tp_take(h: u32) -> Option<(u32, (u32, u32, u32))> {
    let mut v = TP_ARGS.lock().unwrap();
    let i = v.iter().position(|x| x.0 == h)?;
    let (_, gen, a) = v.remove(i);
    Some((gen, a))
}
fn tp_timer_thread(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if let Some((gen, (delay, period, mode))) = tp_take(h) {
        let mut next = std::time::Instant::now() + std::time::Duration::from_millis(delay as u64);
        loop {
            let now = std::time::Instant::now();
            if next > now {
                std::thread::sleep((next - now).min(std::time::Duration::from_millis(50)));
                if tp_current(h, gen) {
                    continue;
                }
            }
            if !tp_current(h, gen) {
                break;
            }
            let Some((cb, ctx)) = tp_with(|m| m.get(&h).map(|t| (t.cb, t.ctx))) else { break };
            tp_with(|m| m.get_mut(&h).map(|t| t.running += 1));
            match mode {
                0 => c.call(cb, &[0, ctx, h]),
                1 => c.call(cb, &[ctx, 1]),
                3 => c.call(cb, &[0, ctx]),
                _ => c.call(cb, &[ctx]),
            };
            tp_with(|m| m.get_mut(&h).map(|t| t.running -= 1));
            if mode >= 2 {
                // QueueUserWorkItem's one-off entry
                tp_with(|m| m.remove(&h));
            }
            if period == 0 {
                break;
            }
            next += std::time::Duration::from_millis(period as u64);
        }
    }
    exit_thread(0)
}
/// mode 0: a thread pool wait (one call: instance, ctx, wait, result); otherwise
/// RegisterWaitForSingleObject's flags (callback(ctx, timed out), again unless WT_EXECUTEONLYONCE)
fn tp_wait_thread(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if let Some((gen, (obj_h, ms, mode))) = tp_take(h) {
        loop {
            let r = obj::wait(&[obj_h], false, ms, None);
            if !tp_current(h, gen) {
                break;
            }
            let Some((cb, ctx)) = tp_with(|m| m.get(&h).map(|t| (t.cb, t.ctx))) else { break };
            if mode == 0 {
                c.call(cb, &[0, ctx, h, if r == 0 { 0 } else { 0x102 }]);
                break;
            }
            c.call(cb, &[ctx, (r != 0) as u32]);
            if mode & 8 != 0 {
                break;
            }
        }
    }
    exit_thread(0)
}
/// RegisterWaitForSingleObject(phandle*, object, callback, ctx, ms, flags)
fn RegisterWaitForSingleObject(c: &mut Ctx) -> u32 {
    let (out, obj_h, cb, ctx, ms, flags) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let h = mem::heap_alloc(16, true);
    tp_with(|m| m.insert(h, Tp { cb, ctx, gen: 0, running: 0 }));
    put32(out, h);
    tp_spawn(false, h, 0, (obj_h, ms, flags | 0x8000_0000));
    1
}
// timer queues: one implicit queue, each timer a thread pool timer calling callback(param, TRUE)
fn CreateTimerQueue(_: &mut Ctx) -> u32 {
    mem::heap_alloc(16, true)
}
fn DeleteTimerQueueEx(_: &mut Ctx) -> u32 {
    1
}
fn DeleteTimerQueue(_: &mut Ctx) -> u32 {
    1
}
/// CreateTimerQueueTimer(phandle*, queue, callback, param, due ms, period ms, flags)
fn CreateTimerQueueTimer(c: &mut Ctx) -> u32 {
    let (out, cb, param, due, period) = (c.a(0), c.a(2), c.a(3), c.a(4), c.a(5));
    let h = mem::heap_alloc(16, true);
    tp_with(|m| m.insert(h, Tp { cb, ctx: param, gen: 0, running: 0 }));
    put32(out, h);
    tp_spawn(true, h, 0, (due, period, 1));
    1
}
/// ChangeTimerQueueTimer(queue, timer, due ms, period ms)
fn ChangeTimerQueueTimer(c: &mut Ctx) -> u32 {
    let (h, due, period) = (c.a(1), c.a(2), c.a(3));
    let Some((_, _, gen)) = tp_bump(h) else { return 0 };
    tp_spawn(true, h, gen, (due, period, 1));
    1
}
fn DeleteTimerQueueTimer(c: &mut Ctx) -> u32 {
    tp_with(|m| m.remove(&c.a(1)));
    let ev = c.a(2);
    if ev != 0 && ev != u32::MAX {
        set_event(ev);
    }
    1
}
/// QueueUserWorkItem(function, context, flags): function(context) on a new guest thread
fn QueueUserWorkItem(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(0), c.a(1));
    let h = mem::heap_alloc(16, true);
    tp_with(|m| m.insert(h, Tp { cb, ctx, gen: 0, running: 0 }));
    tp_spawn(true, h, 0, (0, 0, 2));
    1
}
fn CreateThreadpoolWork(c: &mut Ctx) -> u32 {
    tp_new(c)
}
fn SubmitThreadpoolWork(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if let Some(gen) = tp_with(|m| m.get(&h).map(|t| t.gen)) {
        tp_spawn(true, h, gen, (0, 0, 0));
    }
    0
}
/// TrySubmitThreadpoolCallback(callback(instance, ctx), ctx, env)
fn TrySubmitThreadpoolCallback(c: &mut Ctx) -> u32 {
    let (cb, ctx) = (c.a(0), c.a(1));
    let h = mem::heap_alloc(16, true);
    tp_with(|m| m.insert(h, Tp { cb, ctx, gen: 0, running: 0 }));
    tp_spawn(true, h, 0, (0, 0, 3));
    1
}
/// WaitForThreadpoolWorkCallbacks(work, cancel pending)
fn WaitForThreadpoolWorkCallbacks(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if c.a(1) != 0 {
        TP_ARGS.lock().unwrap().retain(|x| x.0 != h);
    }
    while TP_ARGS.lock().unwrap().iter().any(|x| x.0 == h) || tp_with(|m| m.get(&h).is_some_and(|t| t.running > 0)) {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    0
}
fn CloseThreadpoolWork(c: &mut Ctx) -> u32 {
    tp_with(|m| m.remove(&c.a(0)));
    0
}
fn UnregisterWait(c: &mut Ctx) -> u32 {
    tp_with(|m| m.remove(&c.a(0)));
    1
}
fn UnregisterWaitEx(c: &mut Ctx) -> u32 {
    tp_with(|m| m.remove(&c.a(0)));
    if c.a(1) != 0 && c.a(1) != u32::MAX {
        set_event(c.a(1));
    }
    1
}
fn CreateThreadpoolTimer(c: &mut Ctx) -> u32 {
    tp_new(c)
}
/// SetThreadpoolTimer(timer, due FILETIME* or NULL (cancel), period ms, window)
fn SetThreadpoolTimer(c: &mut Ctx) -> u32 {
    let (h, due, period) = (c.a(0), c.a(1), c.a(2));
    let Some((_, _, gen)) = tp_bump(h) else { return 0 };
    if due != 0 {
        tp_spawn(true, h, gen, (due_ms(due) as u32, period, 0));
    }
    0
}
fn IsThreadpoolTimerSet(c: &mut Ctx) -> u32 {
    TP_ARGS.lock().unwrap().iter().any(|x| x.0 == c.a(0)) as u32
}
fn WaitForThreadpoolTimerCallbacks(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        tp_bump(c.a(0));
    }
    0
}
fn CloseThreadpoolTimer(c: &mut Ctx) -> u32 {
    tp_with(|m| m.remove(&c.a(0)));
    0
}
fn CreateThreadpoolWait(c: &mut Ctx) -> u32 {
    tp_new(c)
}
/// SetThreadpoolWait(wait, handle or NULL (cancel), timeout FILETIME* or NULL (infinite))
fn SetThreadpoolWait(c: &mut Ctx) -> u32 {
    let (h, obj_h, due) = (c.a(0), c.a(1), c.a(2));
    let Some((_, _, gen)) = tp_bump(h) else { return 0 };
    if obj_h != 0 {
        let ms = if due == 0 { u32::MAX } else { due_ms(due).min(u32::MAX as u64 - 1) as u32 };
        tp_spawn(false, h, gen, (obj_h, ms, 0));
    }
    0
}
fn WaitForThreadpoolWaitCallbacks(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        tp_bump(c.a(0));
    }
    0
}
fn CloseThreadpoolWait(c: &mut Ctx) -> u32 {
    tp_with(|m| m.remove(&c.a(0)));
    0
}
fn FreeLibraryWhenCallbackReturns(_: &mut Ctx) -> u32 {
    0
}
fn CreateSemaphoreExW(c: &mut Ctx) -> u32 {
    create_named(named(c.a(3), true), Obj::Semaphore { count: c.i(1), max: c.i(2) })
}
fn SetThreadStackGuarantee(_: &mut Ctx) -> u32 {
    1
}
fn FlushProcessWriteBuffers(_: &mut Ctx) -> u32 {
    std::sync::atomic::fence(Ordering::SeqCst);
    0
}
// event tracing: nobody listens
fn RegisterTraceGuidsW(c: &mut Ctx) -> u32 {
    put32(c.a(5), 0);
    0
}
fn UnregisterTraceGuids(_: &mut Ctx) -> u32 {
    0
}
fn TraceEvent(_: &mut Ctx) -> u32 {
    0
}
fn GetTraceLoggerHandle(_: &mut Ctx) -> u32 {
    last_error(6);
    0
}
fn GetTraceEnableFlags(_: &mut Ctx) -> u32 {
    0
}
fn GetTraceEnableLevel(_: &mut Ctx) -> u32 {
    0
}
fn LCMapStringEx(c: &mut Ctx) -> u32 {
    // (locale name, flags, src, n, dst, cap, ..): the same slots as LCMapStringW's
    LCMapStringW(c)
}
fn GetNativeSystemInfo(c: &mut Ctx) -> u32 {
    GetSystemInfo(c)
}
fn IsProcessorFeaturePresent(c: &mut Ctx) -> u32 {
    // MMX 3, XMMI 6, RDTSC 8, XMMI64 10, NX 12, SSE3 13
    ok(matches!(c.a(0), 3 | 8) || (matches!(c.a(0), 6 | 10) && std::env::var_os("SAKE_SSE").is_some()))
}
fn IsWow64Process(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    1
}

// ---------------------------------------------------------------------------------------------
// version, locale

fn GetVersion(_: &mut Ctx) -> u32 {
    0x1db1_0106
}
fn version_info(p: u32, wide: bool) -> u32 {
    let size = r32(p);
    w32(p + 4, 6);
    w32(p + 8, 1);
    w32(p + 12, 7601);
    w32(p + 16, 2);
    if wide {
        put_wstr(p + 20, 128, &"Service Pack 1".encode_utf16().collect::<Vec<_>>());
    } else {
        put_cstr(p + 20, 128, b"Service Pack 1");
    }
    let ext = if wide { 284 } else { 156 };
    if size >= ext {
        let o = if wide { 276 } else { 148 };
        w16(p + o, 1);
        w16(p + o + 2, 0);
        w16(p + o + 4, 0x100);
        w8(p + o + 6, 1);
    }
    1
}
fn GetVersionExA(c: &mut Ctx) -> u32 {
    version_info(c.a(0), false)
}
fn GetVersionExW(c: &mut Ctx) -> u32 {
    version_info(c.a(0), true)
}
/// RtlGetVersion(RTL_OSVERSIONINFOW*): Artemis asks ntdll directly (GetVersionEx lies on 8.1+)
fn RtlGetVersion(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    version_info(p, true);
    // the real version, as Windows 10 gives it to programs without a manifest
    w32(p + 4, 10);
    w32(p + 8, 0);
    w32(p + 12, 19045);
    put_wstr(p + 20, 128, &[]);
    0
}
fn RtlGetNtVersionNumbers(c: &mut Ctx) -> u32 {
    put32(c.a(0), 10);
    put32(c.a(1), 0);
    put32(c.a(2), 0xf000_0000 | 19045);
    0
}
fn VerifyVersionInfoW(_: &mut Ctx) -> u32 {
    1
}
fn VerifyVersionInfoA(_: &mut Ctx) -> u32 {
    1
}
fn VerSetConditionMask(c: &mut Ctx) -> u32 {
    let v = if mem::x64() { c.a64(0) } else { c.a(0) as u64 | (c.a(1) as u64) << 32 };
    c.ret64(v)
}

fn GetACP(_: &mut Ctx) -> u32 {
    crate::text::acp()
}
fn GetOEMCP(_: &mut Ctx) -> u32 {
    crate::text::acp()
}
fn IsValidCodePage(c: &mut Ctx) -> u32 {
    ok(matches!(c.a(0), 932 | 936 | 949 | 950 | 1250 | 1251 | 1252 | 65001 | 437 | 850))
}
fn lcid() -> u32 {
    match crate::text::acp() {
        932 => 0x411,
        936 => 0x804,
        949 => 0x412,
        950 => 0x404,
        _ => 0x409,
    }
}
fn GetThreadLocale(_: &mut Ctx) -> u32 {
    lcid()
}
fn SetThreadLocale(_: &mut Ctx) -> u32 {
    1
}
fn GetUserDefaultLCID(_: &mut Ctx) -> u32 {
    lcid()
}
fn GetSystemDefaultLCID(_: &mut Ctx) -> u32 {
    lcid()
}
/// EnumSystemLocales: the one locale we have, as "%08x".
fn enum_locales(c: &mut Ctx, wide: bool) -> u32 {
    let s = format!("{:08x}", lcid());
    let p = mem::heap_alloc(32, true);
    if wide {
        put_wstr(p, 16, &s.encode_utf16().collect::<Vec<u16>>());
    } else {
        mem::write(p, s.as_bytes());
    }
    c.call(c.a(0), &[p]);
    mem::heap_free(p);
    1
}
fn EnumSystemLocalesA(c: &mut Ctx) -> u32 {
    enum_locales(c, false)
}
fn EnumSystemLocalesW(c: &mut Ctx) -> u32 {
    enum_locales(c, true)
}
/// EnumSystemLocalesEx(proc(name, flags, lparam), flags, lparam, reserved): ja-JP
fn EnumSystemLocalesEx(c: &mut Ctx) -> u32 {
    let p = mem::heap_alloc(16, true);
    put_wstr(p, 8, &"ja-JP".encode_utf16().collect::<Vec<u16>>());
    c.call(c.a(0), &[p, 2, c.a(2)]);
    mem::heap_free(p);
    1
}
fn GetUserDefaultLangID(_: &mut Ctx) -> u32 {
    lcid()
}
fn GetSystemDefaultLangID(_: &mut Ctx) -> u32 {
    lcid()
}
fn GetUserDefaultUILanguage(_: &mut Ctx) -> u32 {
    lcid()
}
fn GetSystemDefaultUILanguage(_: &mut Ctx) -> u32 {
    lcid()
}
fn IsValidLocale(_: &mut Ctx) -> u32 {
    1
}
fn locale_info(ty: u32) -> String {
    let ja = crate::text::acp() == 932;
    let pick = |j: &str, e: &str| if ja { j } else { e }.to_string();
    match ty & 0xffff {
        0x1 | 0x9 => format!("{:04x}", lcid()),
        0x2 | 0x73 => pick("日本語 (日本)", "English (United States)"),
        0x3 => pick("JPN", "ENU"),
        0x4 => pick("日本語", "English"),
        0x5 | 0xa => pick("81", "1"),
        0x6 | 0x8 => pick("日本", "United States"),
        0x7 | 0x68 => pick("JPN", "USA"),
        0x59 | 0x6d => pick("ja", "en"),
        0x5a => pick("JP", "US"),
        0x5c => pick("ja-JP", "en-US"),
        0x67 => pick("jpn", "eng"),
        0x72 => pick("Japanese (Japan)", "English (United States)"),
        0xb => pick("932", "437"),
        0x1004 => crate::text::acp().to_string(),
        0x1001 => pick("Japanese", "English"),
        0x1002 => pick("Japan", "United States"),
        0x0c => ",".into(),
        0x0d => "1".into(),
        0x0e => ".".into(),
        0x0f => ",".into(),
        0x10 => "3;0".into(),
        0x11 => "2".into(),
        0x12 => "1".into(),
        0x13 => "0123456789".into(),
        0x14 => if ja { "¥" } else { "$" }.into(),
        0x15 => if ja { "JPY" } else { "USD" }.into(),
        0x16 => ".".into(),
        0x17 => ",".into(),
        0x18 => "3;0".into(),
        0x19 => if ja { "0" } else { "2" }.into(),
        0x1a | 0x1b => "0".into(),
        0x1c => "1".into(),
        0x1d => "/".into(),
        0x1e => ":".into(),
        0x1f => if ja { "yyyy/MM/dd" } else { "M/d/yyyy" }.into(),
        0x20 => if ja { "yyyy'年'M'月'd'日'" } else { "dddd, MMMM dd, yyyy" }.into(),
        0x21 => if ja { "2" } else { "0" }.into(),
        0x22 => "0".into(),
        0x23 => "1".into(),
        0x24 => "0".into(),
        0x25 => "1".into(),
        0x26 => "0".into(),
        0x27 => "0".into(),
        0x28 => if ja { "午前" } else { "AM" }.into(),
        0x29 => if ja { "午後" } else { "PM" }.into(),
        0x1003 => "H:mm:ss".into(),
        0x100c => "0".into(),
        0x100d => "0".into(),
        0x1009 => "1".into(),
        0x100b => "1".into(),
        0x1014 => if ja { "¥" } else { "$" }.into(),
        0x2a..=0x30 => ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"][(ty & 0xffff) as usize - 0x2a].into(),
        0x31..=0x37 => ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"][(ty & 0xffff) as usize - 0x31].into(),
        0x38..=0x43 => ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][(ty & 0xffff) as usize - 0x38].into(),
        0x44..=0x4f => ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][(ty & 0xffff) as usize - 0x44].into(),
        0x50 => "".into(),
        0x51 => "-".into(),
        _ => "".into(),
    }
}
/// LOCALE_RETURN_NUMBER: the value as a DWORD (A: 4 bytes, W: 2 WCHARs)
fn locale_number(c: &mut Ctx, wide: bool) -> Option<u32> {
    if c.a(1) & 0x2000_0000 == 0 {
        return None;
    }
    let n = if wide { 2 } else { 4 };
    let v = u32::from_str_radix(&locale_info(c.a(1)), if c.a(1) & 0xffff == 1 { 16 } else { 10 }).unwrap_or(0);
    if c.a(3) == 0 {
        return Some(n);
    }
    if c.a(3) < n {
        last_error(122);
        return Some(0);
    }
    w32(c.a(2), v);
    Some(n)
}
fn GetLocaleInfoA(c: &mut Ctx) -> u32 {
    if let Some(n) = locale_number(c, false) {
        return n;
    }
    let s = locale_info(c.a(1));
    if c.a(3) == 0 {
        return crate::text::s2a(&s).len() as u32 + 1;
    }
    ret_str(c.a(2), c.a(3), &s, false) + 1
}
fn GetLocaleInfoW(c: &mut Ctx) -> u32 {
    if let Some(n) = locale_number(c, true) {
        return n;
    }
    let s = locale_info(c.a(1));
    if c.a(3) == 0 {
        return s.encode_utf16().count() as u32 + 1;
    }
    ret_str(c.a(2), c.a(3), &s, true) + 1
}
fn EnumCalendarInfoA(_: &mut Ctx) -> u32 {
    1
}
fn GetCPInfo(c: &mut Ctx) -> u32 {
    let p = c.a(1);
    mem::fill(p, 20, 0);
    let dbcs = crate::text::is_dbcs() && matches!(c.a(0), 0 | 1 | 3 | 932 | 936 | 949 | 950);
    w32(p, if c.a(0) == 65001 { 4 } else if dbcs { 2 } else { 1 });
    w8(p + 4, b'?');
    if dbcs && crate::text::acp() == 932 {
        mem::write(p + 6, &[0x81, 0x9f, 0xe0, 0xfc]);
    } else if dbcs {
        mem::write(p + 6, &[0x81, 0xfe]);
    }
    1
}
fn GetCPInfoExA(c: &mut Ctx) -> u32 {
    GetCPInfo(c)
}
fn IsDBCSLeadByte(c: &mut Ctx) -> u32 {
    ok(crate::text::is_lead(c.a(0) as u8))
}
fn IsDBCSLeadByteEx(c: &mut Ctx) -> u32 {
    ok(crate::text::is_lead(c.a(1) as u8))
}

fn MultiByteToWideChar(c: &mut Ctx) -> u32 {
    let (cp, _flags, src, n, dst, cap) = (c.a(0), c.a(1), c.a(2), c.i(3), c.a(4), c.a(5));
    let s = if n < 0 { let mut v = cstr(src); v.push(0); v } else { mem::slice(src, n as u32).to_vec() };
    let w = mb2w(cp, &s);
    if cap == 0 {
        return w.len() as u32;
    }
    if w.len() as u32 > cap {
        last_error(122);
        return 0;
    }
    for (i, ch) in w.iter().enumerate() {
        w16(dst + 2 * i as u32, *ch);
    }
    w.len() as u32
}
fn WideCharToMultiByte(c: &mut Ctx) -> u32 {
    let (cp, src, n, dst, cap) = (c.a(0), c.a(2), c.i(3), c.a(4), c.a(5));
    let s: Vec<u16> = if n < 0 {
        let mut v = wstr(src);
        v.push(0);
        v
    } else {
        (0..n as u32).map(|i| r16(src + 2 * i)).collect()
    };
    let a = w2mb(cp, &s);
    put32(c.a(7), 0);
    if cap == 0 {
        return a.len() as u32;
    }
    if a.len() as u32 > cap {
        last_error(122);
        return 0;
    }
    mem::write(dst, &a);
    a.len() as u32
}

fn ctype1(ch: u16) -> u16 {
    let c = ch as u32;
    let mut t = 0u16;
    if let Some(x) = char::from_u32(c) {
        if x.is_uppercase() {
            t |= 1;
        }
        if x.is_lowercase() {
            t |= 2;
        }
        // (full-width digits are digits too)
        if x.is_ascii_digit() || (0xff10..=0xff19).contains(&c) {
            t |= 4;
        }
        if x.is_whitespace() {
            t |= 8;
        }
        // (and the non-ASCII marks: 、。「」・ー and full-width ！？)
        if x.is_ascii_punctuation() || (c >= 0x80 && !x.is_alphanumeric() && !x.is_whitespace() && !x.is_control()) {
            t |= 0x10;
        }
        if x.is_control() {
            t |= 0x20;
        }
        if x == ' ' || x == '\t' {
            t |= 0x40;
        }
        if x.is_ascii_hexdigit() || matches!(c, 0xff10..=0xff19 | 0xff21..=0xff26 | 0xff41..=0xff46) {
            t |= 0x80;
        }
        if x.is_alphabetic() {
            t |= 0x100;
        }
    }
    t
}
/// CT_CTYPE2: the bidi class (left to right for letters, CJK included)
fn ctype2(ch: u16) -> u16 {
    let t = ctype1(ch);
    match ch {
        _ if t & 0x100 != 0 => 1,             // C2_LEFTTORIGHT
        _ if t & 4 != 0 => 3,                 // C2_EUROPENUMBER
        0x2b | 0x2d => 4,                     // C2_EUROPESEPARATOR
        0x23..=0x25 => 5,                     // C2_EUROPETERMINATOR
        0x2c | 0x2e | 0x2f | 0x3a => 7,       // C2_COMMONSEPARATOR
        0x0a | 0x0d | 0x1c..=0x1e => 8,       // C2_BLOCKSEPARATOR
        0x09 | 0x0b | 0x1f => 9,              // C2_SEGMENTSEPARATOR
        _ if t & 8 != 0 => 10,                // C2_WHITESPACE
        _ if t & 0x20 != 0 => 0,              // C2_NOTAPPLICABLE
        _ => 11,                              // C2_OTHERNEUTRAL
    }
}

/// CT_CTYPE3: Japanese text layout reads C3_FULLWIDTH / C3_HALFWIDTH (SiglusEngine spaces a
/// character by half the font size without C3_FULLWIDTH), C3_HIRAGANA, C3_KATAKANA, C3_IDEOGRAPH.
fn ctype3(ch: u16) -> u16 {
    const NONSPACING: u16 = 1;
    const DIACRITIC: u16 = 2;
    const SYMBOL: u16 = 8;
    const KATAKANA: u16 = 0x10;
    const HIRAGANA: u16 = 0x20;
    const HALFWIDTH: u16 = 0x40;
    const FULLWIDTH: u16 = 0x80;
    const IDEOGRAPH: u16 = 0x100;
    const ALPHA: u16 = 0x8000;
    let c = ch as u32;
    let t1 = ctype1(ch);
    let alpha = if t1 & 0x100 != 0 { ALPHA } else { 0 };
    let symbol = if t1 & 0x10 != 0 { SYMBOL } else { 0 };
    match c {
        0x3099..=0x309a => NONSPACING | DIACRITIC | HIRAGANA | FULLWIDTH,
        0x309b..=0x309c => DIACRITIC | SYMBOL | FULLWIDTH,
        0x3041..=0x309f => HIRAGANA | FULLWIDTH | ALPHA,
        0x30fb => SYMBOL | FULLWIDTH,
        0x30a0..=0x30ff | 0x31f0..=0x31ff => KATAKANA | FULLWIDTH | ALPHA,
        0x3005..=0x3007 | 0x3021..=0x3029 | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff => IDEOGRAPH | FULLWIDTH | ALPHA,
        0xd800..=0xdfff => FULLWIDTH, // (supplementary ideographs come as surrogate pairs)
        0x3000..=0x303f | 0x3200..=0x33ff | 0xfe30..=0xfe4f => symbol | FULLWIDTH,
        0xff61..=0xff65 => SYMBOL | HALFWIDTH,
        0xff9e..=0xff9f => DIACRITIC | SYMBOL | KATAKANA | HALFWIDTH,
        0xff66..=0xff9d => KATAKANA | HALFWIDTH | ALPHA,
        0xffa0..=0xffdc | 0xffe8..=0xffee => alpha | symbol | HALFWIDTH,
        0xff01..=0xff60 | 0xffe0..=0xffe6 => alpha | symbol | FULLWIDTH,
        0x1100..=0x115f | 0xac00..=0xd7a3 | 0x3130..=0x318f => FULLWIDTH | ALPHA,
        // (box drawing, arrows, math, Greek and Cyrillic are full width in the Japanese fonts,
        // but Windows marks them by script: only symbols, letters)
        0x0300..=0x036f => NONSPACING | DIACRITIC,
        0..=0x7f => alpha | symbol | HALFWIDTH,
        _ => alpha | symbol,
    }
}

fn string_type(ty: u32, w: &[u16], out: u32) {
    for (i, ch) in w.iter().enumerate() {
        let v = match ty {
            1 => ctype1(*ch),
            2 => ctype2(*ch),
            4 => ctype3(*ch),
            _ => 0,
        };
        w16(out + 2 * i as u32, v);
    }
}
fn GetStringTypeA(c: &mut Ctx) -> u32 {
    let (ty, src, n, out) = (c.a(1), c.a(2), c.i(3), c.a(4));
    let s = if n < 0 { cstr(src) } else { mem::slice(src, n as u32).to_vec() };
    // one result per byte
    let mut w = Vec::new();
    let mut i = 0;
    while i < s.len() {
        if crate::text::is_lead(s[i]) && i + 1 < s.len() {
            let x = a2w(&s[i..i + 2]);
            w.push(x.first().copied().unwrap_or(0));
            w.push(x.first().copied().unwrap_or(0));
            i += 2;
        } else {
            w.push(a2w(&s[i..i + 1]).first().copied().unwrap_or(0));
            i += 1;
        }
    }
    string_type(ty, &w, out);
    1
}
fn GetStringTypeExA(c: &mut Ctx) -> u32 {
    GetStringTypeA(c)
}
fn GetStringTypeW(c: &mut Ctx) -> u32 {
    let (ty, src, n, out) = (c.a(0), c.a(1), c.i(2), c.a(3));
    let w: Vec<u16> = if n < 0 { wstr(src) } else { (0..n as u32).map(|i| r16(src + 2 * i)).collect() };
    string_type(ty, &w, out);
    1
}
fn GetStringTypeExW(c: &mut Ctx) -> u32 {
    let (ty, src, n, out) = (c.a(1), c.a(2), c.i(3), c.a(4));
    let w: Vec<u16> = if n < 0 { wstr(src) } else { (0..n as u32).map(|i| r16(src + 2 * i)).collect() };
    string_type(ty, &w, out);
    1
}
fn LCMapStringA(c: &mut Ctx) -> u32 {
    let (flags, src, n, dst, cap) = (c.a(1), c.a(2), c.i(3), c.a(4), c.a(5));
    let mut s = if n < 0 { let mut v = cstr(src); v.push(0); v } else { mem::slice(src, n as u32).to_vec() };
    if flags & 0x100 != 0 {
        crate::text::lower_a(&mut s);
    }
    if flags & 0x200 != 0 {
        crate::text::upper_a(&mut s);
    }
    if cap == 0 {
        return s.len() as u32;
    }
    let k = s.len().min(cap as usize);
    mem::write(dst, &s[..k]);
    k as u32
}
fn LCMapStringW(c: &mut Ctx) -> u32 {
    let (flags, src, n, dst, cap) = (c.a(1), c.a(2), c.i(3), c.a(4), c.a(5));
    let mut w: Vec<u16> = if n < 0 { let mut v = wstr(src); v.push(0); v } else { (0..n as u32).map(|i| r16(src + 2 * i)).collect() };
    for ch in w.iter_mut() {
        if flags & 0x100 != 0 {
            *ch = crate::text::lower16(*ch);
        }
        if flags & 0x200 != 0 {
            *ch = crate::text::upper16(*ch);
        }
    }
    if cap == 0 {
        return w.len() as u32;
    }
    let k = w.len().min(cap as usize);
    for (i, ch) in w[..k].iter().enumerate() {
        w16(dst + 2 * i as u32, *ch);
    }
    k as u32
}
fn cmp_w(a: &[u16], b: &[u16], nocase: bool) -> std::cmp::Ordering {
    let f = |c: u16| if nocase { crate::text::lower16(c) } else { c };
    a.iter().map(|&c| f(c)).cmp(b.iter().map(|&c| f(c)))
}
fn CompareStringA(c: &mut Ctx) -> u32 {
    let (flags, s1, n1, s2, n2) = (c.a(1), c.a(2), c.i(3), c.a(4), c.i(5));
    let a = if n1 < 0 { cstr(s1) } else { mem::cstrn(s1, n1 as u32) };
    let b = if n2 < 0 { cstr(s2) } else { mem::cstrn(s2, n2 as u32) };
    cmp_w(&a2w(&a), &a2w(&b), flags & 1 != 0) as i32 as u32 + 2
}
fn CompareStringW(c: &mut Ctx) -> u32 {
    let (flags, s1, n1, s2, n2) = (c.a(1), c.a(2), c.i(3), c.a(4), c.i(5));
    let rd = |p: u32, n: i32| -> Vec<u16> { if n < 0 { wstr(p) } else { (0..n as u32).map(|i| r16(p + 2 * i)).take_while(|&x| x != 0).collect() } };
    cmp_w(&rd(s1, n1), &rd(s2, n2), flags & 1 != 0) as i32 as u32 + 2
}
fn GetDateFormatA(c: &mut Ctx) -> u32 {
    ret_str(c.a(4), c.a(5), "2026/11/01", false) + 1
}
fn GetTimeFormatA(c: &mut Ctx) -> u32 {
    ret_str(c.a(4), c.a(5), "12:00:00", false) + 1
}
fn MulDiv(c: &mut Ctx) -> u32 {
    let (a, b, d) = (c.i(0) as i64, c.i(1) as i64, c.i(2) as i64);
    if d == 0 {
        return 0xffff_ffff;
    }
    let n = a * b;
    let r = if (n < 0) != (d < 0) { (n - d / 2) / d } else { (n + d / 2) / d };
    r as i32 as u32
}

// lstr*
fn lstrlenA(c: &mut Ctx) -> u32 {
    cstr(c.a(0)).len() as u32
}
fn lstrlenW(c: &mut Ctx) -> u32 {
    wstr(c.a(0)).len() as u32
}
fn lstrcpyA(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(1));
    mem::write(c.a(0), &s);
    w8(c.a(0) + s.len() as u32, 0);
    c.a(0)
}
fn lstrcpyW(c: &mut Ctx) -> u32 {
    let s = wstr(c.a(1));
    put_wstr(c.a(0), s.len() as u32 + 1, &s);
    c.a(0)
}
fn lstrcpynA(c: &mut Ctx) -> u32 {
    let s = mem::cstrn(c.a(1), c.a(2).saturating_sub(1));
    put_cstr(c.a(0), c.a(2), &s);
    c.a(0)
}
fn lstrcpynW(c: &mut Ctx) -> u32 {
    let s = wstr(c.a(1));
    put_wstr(c.a(0), c.a(2), &s);
    c.a(0)
}
fn lstrcatA(c: &mut Ctx) -> u32 {
    let d = cstr(c.a(0));
    let s = cstr(c.a(1));
    mem::write(c.a(0) + d.len() as u32, &s);
    w8(c.a(0) + (d.len() + s.len()) as u32, 0);
    c.a(0)
}
fn lstrcatW(c: &mut Ctx) -> u32 {
    let d = wstr(c.a(0));
    let s = wstr(c.a(1));
    put_wstr(c.a(0) + 2 * d.len() as u32, s.len() as u32 + 1, &s);
    c.a(0)
}
fn lstrcmpA(c: &mut Ctx) -> u32 {
    cmp_w(&a2w(&cstr(c.a(0))), &a2w(&cstr(c.a(1))), false) as i32 as u32
}
fn lstrcmpiA(c: &mut Ctx) -> u32 {
    cmp_w(&a2w(&cstr(c.a(0))), &a2w(&cstr(c.a(1))), true) as i32 as u32
}
fn lstrcmpW(c: &mut Ctx) -> u32 {
    cmp_w(&wstr(c.a(0)), &wstr(c.a(1)), false) as i32 as u32
}
fn lstrcmpiW(c: &mut Ctx) -> u32 {
    cmp_w(&wstr(c.a(0)), &wstr(c.a(1)), true) as i32 as u32
}

// atoms
static ATOMS: Mutex<Vec<String>> = Mutex::new(Vec::new());
fn add_atom(s: String) -> u32 {
    if let Some(n) = s.strip_prefix('#').and_then(|x| x.parse::<u32>().ok()) {
        return n;
    }
    let mut a = ATOMS.lock().unwrap();
    let low = s.to_lowercase();
    if let Some(i) = a.iter().position(|x| x.to_lowercase() == low) {
        return 0xc000 + i as u32;
    }
    a.push(s);
    0xc000 + a.len() as u32 - 1
}
pub fn atom_name(a: u32) -> Option<String> {
    ATOMS.lock().unwrap().get((a.checked_sub(0xc000)?) as usize).cloned()
}
pub fn atom(s: &str) -> u32 {
    add_atom(s.to_string())
}
fn GlobalAddAtomA(c: &mut Ctx) -> u32 {
    add_atom(arg_str(c.a(0), false))
}
fn GlobalAddAtomW(c: &mut Ctx) -> u32 {
    add_atom(arg_str(c.a(0), true))
}
fn AddAtomA(c: &mut Ctx) -> u32 {
    add_atom(arg_str(c.a(0), false))
}
fn GlobalFindAtomA(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(0), false).to_lowercase();
    ATOMS.lock().unwrap().iter().position(|x| x.to_lowercase() == s).map_or(0, |i| 0xc000 + i as u32)
}
fn FindAtomA(c: &mut Ctx) -> u32 {
    GlobalFindAtomA(c)
}
fn GlobalDeleteAtom(_: &mut Ctx) -> u32 {
    0
}
fn DeleteAtom(_: &mut Ctx) -> u32 {
    0
}
fn GlobalGetAtomNameA(c: &mut Ctx) -> u32 {
    match atom_name(c.a(0)) {
        Some(s) => ret_str(c.a(1), c.a(2), &s, false),
        None => 0,
    }
}

// ---------------------------------------------------------------------------------------------
// time

fn system_time(p: u32, local: bool) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as i64 + if local { tz_bias() } else { 0 };
    write_systemtime(p, secs, now.subsec_millis());
}
fn tz_bias() -> i64 {
    if crate::text::acp() == 932 { 9 * 3600 } else { 0 }
}
fn write_systemtime(p: u32, secs: i64, ms: u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    // civil from days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    w16(p, y as u16);
    w16(p + 2, m as u16);
    w16(p + 4, ((days + 4).rem_euclid(7)) as u16);
    w16(p + 6, d as u16);
    w16(p + 8, (rem / 3600) as u16);
    w16(p + 10, (rem / 60 % 60) as u16);
    w16(p + 12, (rem % 60) as u16);
    w16(p + 14, ms as u16);
}
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}
fn GetSystemTime(c: &mut Ctx) -> u32 {
    system_time(c.a(0), false);
    0
}
fn GetLocalTime(c: &mut Ctx) -> u32 {
    system_time(c.a(0), true);
    0
}
fn put64(p: u32, v: u64) {
    w32(p, v as u32);
    w32(p + 4, (v >> 32) as u32);
}
fn get64(p: u32) -> u64 {
    r32(p) as u64 | (r32(p + 4) as u64) << 32
}
fn GetSystemTimeAsFileTime(c: &mut Ctx) -> u32 {
    put64(c.a(0), files::filetime(std::time::SystemTime::now()));
    0
}
fn GetSystemTimePreciseAsFileTime(c: &mut Ctx) -> u32 {
    GetSystemTimeAsFileTime(c)
}
fn FileTimeToSystemTime(c: &mut Ctx) -> u32 {
    let ft = get64(c.a(0));
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    write_systemtime(c.a(1), secs, ((ft / 10_000) % 1000) as u32);
    1
}
fn SystemTimeToFileTime(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let days = days_from_civil(r16(p) as i64, r16(p + 2) as i64, r16(p + 6) as i64);
    let secs = days * 86400 + r16(p + 8) as i64 * 3600 + r16(p + 10) as i64 * 60 + r16(p + 12) as i64;
    let ft = ((secs + 11_644_473_600) as u64) * 10_000_000 + r16(p + 14) as u64 * 10_000;
    put64(c.a(1), ft);
    1
}
fn FileTimeToLocalFileTime(c: &mut Ctx) -> u32 {
    put64(c.a(1), get64(c.a(0)).wrapping_add((tz_bias() * 10_000_000) as u64));
    1
}
fn LocalFileTimeToFileTime(c: &mut Ctx) -> u32 {
    put64(c.a(1), get64(c.a(0)).wrapping_sub((tz_bias() * 10_000_000) as u64));
    1
}
fn FileTimeToDosDateTime(c: &mut Ctx) -> u32 {
    let ft = get64(c.a(0));
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    let tmp = mem::heap_alloc(16, true);
    write_systemtime(tmp, secs, 0);
    let (y, m, d, h, mi, s) = (r16(tmp), r16(tmp + 2), r16(tmp + 6), r16(tmp + 8), r16(tmp + 10), r16(tmp + 12));
    mem::heap_free(tmp);
    if c.a(1) != 0 {
        w16(c.a(1), (y.saturating_sub(1980)) << 9 | m << 5 | d);
    }
    if c.a(2) != 0 {
        w16(c.a(2), h << 11 | mi << 5 | s / 2);
    }
    1
}
fn CompareFileTime(c: &mut Ctx) -> u32 {
    get64(c.a(0)).cmp(&get64(c.a(1))) as i32 as u32
}
fn GetTimeZoneInformation(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    mem::fill(p, 172, 0);
    w32(p, (-tz_bias() / 60) as i32 as u32);
    // (Japanese Windows' names: RealLive and Siglus refuse to run elsewhere)
    if tz_bias() == 9 * 3600 {
        for (off, name) in [(4, "東京 (標準時)"), (88, "東京 (夏時間)")] {
            let w: Vec<u16> = name.encode_utf16().collect();
            mem::put_wstr(p + off, 32, &w);
        }
    }
    0
}
fn GetTickCount(_: &mut Ctx) -> u32 {
    ticks_ms()
}
fn GetTickCount64(c: &mut Ctx) -> u32 {
    let v = ticks_ms() as u64;
    c.ret64(v)
}
fn qpc() -> u64 {
    clock_ns() / 100 + 600_000_000
}
fn QueryPerformanceCounter(c: &mut Ctx) -> u32 {
    put64(c.a(0), qpc());
    1
}
fn QueryPerformanceFrequency(c: &mut Ctx) -> u32 {
    put64(c.a(0), 10_000_000);
    1
}
fn GetProcessTimes(c: &mut Ctx) -> u32 {
    for i in 1..5 {
        put64(c.a(i), 0);
    }
    1
}
fn GetThreadTimes(c: &mut Ctx) -> u32 {
    GetProcessTimes(c)
}

// ---------------------------------------------------------------------------------------------
// modules, resources

fn module_name(h: u32, wide: bool, dst: u32, cap: u32) -> u32 {
    let base = if h == 0 { crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.base).unwrap_or(0) } else { h };
    let Some(path) = crate::pe::with(base, |m| m.path.clone()) else {
        last_error(126);
        return 0;
    };
    let n = if wide { path.encode_utf16().count() } else { crate::text::s2a(&path).len() } as u32;
    if n >= cap {
        ret_str(dst, cap, &path, wide);
        last_error(122);
        return cap;
    }
    ret_str(dst, cap, &path, wide)
}
fn GetModuleFileNameA(c: &mut Ctx) -> u32 {
    module_name(c.a(0), false, c.a(1), c.a(2))
}
fn GetModuleFileNameW(c: &mut Ctx) -> u32 {
    module_name(c.a(0), true, c.a(1), c.a(2))
}
pub fn module_handle(name: u32, wide: bool) -> u32 {
    if name == 0 {
        return crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.base).unwrap_or(0);
    }
    let n = arg_str(name, wide);
    if let Some(b) = crate::pe::by_name(&n) {
        return b;
    }
    let k = crate::api::dll_key(&n);
    if matches!(k.as_str(), "kernel32" | "ntdll" | "kernelbase" | "user32" | "gdi32" | "advapi32") {
        return crate::pe::hle_module(&k);
    }
    last_error(126);
    0
}
fn GetModuleHandleA(c: &mut Ctx) -> u32 {
    module_handle(c.a(0), false)
}
fn GetModuleHandleW(c: &mut Ctx) -> u32 {
    module_handle(c.a(0), true)
}
fn GetModuleHandleExW(c: &mut Ctx) -> u32 {
    let flags = c.a(0);
    let h = if flags & 4 != 0 { crate::pe::containing(c.a(1)).unwrap_or(0) } else { module_handle(c.a(1), true) };
    put32(c.a(2), h);
    ok(h != 0)
}
fn GetModuleHandleExA(c: &mut Ctx) -> u32 {
    let flags = c.a(0);
    let h = if flags & 4 != 0 { crate::pe::containing(c.a(1)).unwrap_or(0) } else { module_handle(c.a(1), false) };
    put32(c.a(2), h);
    ok(h != 0)
}
fn LoadLibraryA(c: &mut Ctx) -> u32 {
    let n = arg_str(c.a(0), false);
    let r = crate::pe::load_library(c.cpu, &n);
    if r == 0 {
        last_error(126);
    }
    r
}
fn LoadLibraryW(c: &mut Ctx) -> u32 {
    let n = arg_str(c.a(0), true);
    let r = crate::pe::load_library(c.cpu, &n);
    if r == 0 {
        last_error(126);
    }
    r
}
fn LoadLibraryExA(c: &mut Ctx) -> u32 {
    LoadLibraryA(c)
}
fn LoadLibraryExW(c: &mut Ctx) -> u32 {
    LoadLibraryW(c)
}
fn FreeLibrary(_: &mut Ctx) -> u32 {
    1
}
fn DisableThreadLibraryCalls(_: &mut Ctx) -> u32 {
    1
}
fn GetProcAddress(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    let imp = if p < 0x10000 { crate::pe::Import::Ordinal(p) } else { crate::pe::Import::Name(Box::leak(a2s(&cstr(p)).into_boxed_str())) };
    let r = crate::pe::proc_address(h, imp).unwrap_or(0);
    if std::env::var_os("SAKE_TRACE_LOAD").is_some() {
        eprintln!("sake: GetProcAddress({h:#x}, {}) = {r:#x}", if p < 0x10000 { format!("#{p}") } else { a2s(&cstr(p)) });
    }
    if r == 0 {
        last_error(127);
    }
    r
}
/// ResolveDelayLoadedAPI(base, IMAGE_DELAYLOAD_DESCRIPTOR, hooks.., thunk, flags): loads the
/// DLL on first use and binds one delay-load IAT slot.
fn ResolveDelayLoadedAPI(c: &mut Ctx) -> u32 {
    let (base, d, thunk) = (c.a(0), c.a(1), c.a(4));
    let rva = |i: u32| if r32(d) & 1 != 0 { base + r32(d + 4 * i) } else { r32(d + 4 * i) };
    let name = a2s(&cstr(rva(1)));
    let hmod_at = rva(2);
    let mut h = r32(hmod_at);
    if h == 0 {
        h = crate::pe::load_library(c.cpu, &name);
        w32(hmod_at, h);
    }
    let i = (thunk - rva(3)) / 4;
    let e = r32(rva(4) + 4 * i);
    let what = if e & 0x8000_0000 != 0 { format!("#{}", e & 0xffff) } else { a2s(&cstr(base + e + 2)) };
    let r = if h == 0 {
        0
    } else if e & 0x8000_0000 != 0 {
        crate::pe::proc_address(h, crate::pe::Import::Ordinal(e & 0xffff)).unwrap_or(0)
    } else {
        crate::pe::proc_address(h, crate::pe::Import::Name(&what)).unwrap_or(0)
    };
    // a system DLL's function we know but don't implement: bound like a static import (its
    // trap returns 0), as Windows would have it
    let r = if r == 0 && h != 0 && e & 0x8000_0000 == 0 && crate::api::is_system(&name) {
        crate::fast::bind(&name, &what, crate::api::export(&name, &what))
    } else {
        r
    };
    if r == 0 {
        eprintln!("sake: delay-load import {name}!{what} failed ({})", if h == 0 { "no DLL" } else { "no export" });
    }
    w32(thunk, r);
    r
}
fn find_res(c: &mut Ctx, m: u32, name: u32, ty: u32, wide: bool) -> u32 {
    let _ = c;
    let m = if m == 0 { module_handle(0, false) } else { m };
    let ty = crate::pe::ResId::from_arg(ty, wide);
    let name = crate::pe::ResId::from_arg(name, wide);
    match crate::pe::find_resource(m, &ty, &name) {
        Some(e) => e,
        None => {
            last_error(1813);
            0
        }
    }
}
fn FindResourceA(c: &mut Ctx) -> u32 {
    let (m, n, t) = (c.a(0), c.a(1), c.a(2));
    find_res(c, m, n, t, false)
}
fn FindResourceW(c: &mut Ctx) -> u32 {
    let (m, n, t) = (c.a(0), c.a(1), c.a(2));
    find_res(c, m, n, t, true)
}
fn FindResourceExA(c: &mut Ctx) -> u32 {
    let (m, t, n) = (c.a(0), c.a(1), c.a(2));
    find_res(c, m, n, t, false)
}
fn FindResourceExW(c: &mut Ctx) -> u32 {
    let (m, t, n) = (c.a(0), c.a(1), c.a(2));
    find_res(c, m, n, t, true)
}
fn LoadResource(c: &mut Ctx) -> u32 {
    let m = if c.a(0) == 0 { module_handle(0, false) } else { c.a(0) };
    if c.a(1) == 0 {
        return 0;
    }
    crate::pe::resource_data(m, c.a(1)).0
}
fn LockResource(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn FreeResource(_: &mut Ctx) -> u32 {
    0
}
fn SizeofResource(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        return 0;
    }
    r32(c.a(1) + 4)
}

// ---------------------------------------------------------------------------------------------
// files

fn GetStdHandle(c: &mut Ctx) -> u32 {
    static STD: OnceLock<[u32; 3]> = OnceLock::new();
    let s = STD.get_or_init(|| [obj::add(Obj::Other("stdin")), obj::add(Obj::Other("stdout")), obj::add(Obj::Other("stderr"))]);
    match c.a(0) {
        0xffff_fff6 => s[0],
        0xffff_fff5 => s[1],
        0xffff_fff4 => s[2],
        _ => 0xffff_ffff,
    }
}
fn SetStdHandle(_: &mut Ctx) -> u32 {
    1
}
fn GetFileType(c: &mut Ctx) -> u32 {
    match obj::with(|t| t.objs.get(&c.a(0)).map(|(o, _)| matches!(o, Obj::Other(_)))) {
        Some(true) => 2,
        Some(false) => 1,
        None => 0,
    }
}
fn GetConsoleMode(_: &mut Ctx) -> u32 {
    0
}
fn create_file(c: &mut Ctx, wide: bool) -> u32 {
    let name = arg_str(c.a(0), wide);
    let (access, disp) = (c.a(1), c.a(4));
    if std::env::var_os("SAKE_TRACE_FILES").is_some() {
        eprintln!("sake: CreateFile({name}, {access:#x}, {disp})");
    }
    if name.eq_ignore_ascii_case("CONOUT$") || name.eq_ignore_ascii_case("CONIN$") {
        return obj::add(Obj::Other("console"));
    }
    match files::open(&name, access, disp) {
        // a directory opens only with FILE_FLAG_BACKUP_SEMANTICS
        Ok(f) if f.dir && c.a(5) & 0x0200_0000 == 0 => {
            last_error(5);
            0xffff_ffff
        }
        Ok(f) => {
            let existed = disp == 2 || disp == 4;
            last_error(if existed { 0 } else { 0 });
            obj::add(Obj::File(std::sync::Arc::new(f)))
        }
        Err(e) => {
            last_error(e);
            0xffff_ffff
        }
    }
}
fn CreateFileA(c: &mut Ctx) -> u32 {
    create_file(c, false)
}
fn CreateFileW(c: &mut Ctx) -> u32 {
    create_file(c, true)
}
fn _lopen(c: &mut Ctx) -> u32 {
    let name = arg_str(c.a(0), false);
    match files::open(&name, if c.a(1) & 3 != 0 { 0xc000_0000 } else { 0x8000_0000 }, 3) {
        Ok(f) => obj::add(Obj::File(std::sync::Arc::new(f))),
        Err(_) => 0xffff_ffff,
    }
}
fn _lclose(c: &mut Ctx) -> u32 {
    obj::close(c.a(0));
    0
}
fn _lread(c: &mut Ctx) -> u32 {
    obj::file(c.a(0)).map_or(0xffff_ffff, |f| f.read_into(c.a(1), c.a(2)))
}
fn _llseek(c: &mut Ctx) -> u32 {
    let Some(f) = obj::file(c.a(0)) else { return 0xffff_ffff };
    let pos = match c.a(2) {
        0 => SeekFrom::Start(c.a(1) as u64),
        1 => SeekFrom::Current(c.i(1) as i64),
        _ => SeekFrom::End(c.i(1) as i64),
    };
    f.seek(pos).map_or(0xffff_ffff, |p| p as u32)
}
fn ReadFile(c: &mut Ctx) -> u32 {
    let (h, buf, n, pread, ov) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some(f) = obj::file(h) else {
        last_error(6);
        return 0;
    };
    // OVERLAPPED: Internal, InternalHigh (ULONG_PTRs), Offset, OffsetHigh, hEvent
    let k = mem::ps();
    if ov != 0 {
        f.seek(SeekFrom::Start(r32(ov + 2 * k) as u64 | (r32(ov + 2 * k + 4) as u64) << 32));
    }
    let got = f.read_into(buf, n);
    put32(pread, got);
    if ov != 0 {
        mem::wp(ov, 0);
        mem::wp(ov + k, got);
        if r32(ov + 2 * k + 8) != 0 {
            event_set(r32(ov + 2 * k + 8), true);
        }
    }
    1
}
fn WriteFile(c: &mut Ctx) -> u32 {
    let (h, buf, n, pw) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let kind = obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Other(k), _)) => Some(*k),
        _ => None,
    });
    if let Some(_k) = kind {
        eprint!("{}", a2s(mem::slice(buf, n)));
        put32(pw, n);
        return 1;
    }
    let Some(f) = obj::file(h) else {
        last_error(6);
        return 0;
    };
    let w = f.write_from(buf, n);
    put32(pw, w);
    ok(w == n)
}
fn SetFilePointer(c: &mut Ctx) -> u32 {
    let (h, lo, phi, how) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(f) = obj::file(h) else {
        last_error(6);
        return 0xffff_ffff;
    };
    let off = if phi != 0 { (lo as u64 | (r32(phi) as u64) << 32) as i64 } else { lo as i32 as i64 };
    let pos = match how {
        0 => SeekFrom::Start(off as u64),
        1 => SeekFrom::Current(off),
        _ => SeekFrom::End(off),
    };
    match f.seek(pos) {
        Some(p) => {
            put32(phi, (p >> 32) as u32);
            p as u32
        }
        None => {
            last_error(131);
            0xffff_ffff
        }
    }
}
fn SetFilePointerEx(c: &mut Ctx) -> u32 {
    // (x64: the LARGE_INTEGER is one argument)
    let (h, off, out, how) = if mem::x64() {
        (c.a(0), c.a64(1) as i64, c.a(2), c.a(3))
    } else {
        (c.a(0), (c.a(1) as u64 | (c.a(2) as u64) << 32) as i64, c.a(3), c.a(4))
    };
    let Some(f) = obj::file(h) else { return 0 };
    let pos = match how {
        0 => SeekFrom::Start(off as u64),
        1 => SeekFrom::Current(off),
        _ => SeekFrom::End(off),
    };
    match f.seek(pos) {
        Some(p) => {
            if out != 0 {
                put64(out, p);
            }
            1
        }
        None => 0,
    }
}
fn GetFileSize(c: &mut Ctx) -> u32 {
    let Some(f) = obj::file(c.a(0)) else {
        last_error(6);
        return 0xffff_ffff;
    };
    let s = f.size();
    put32(c.a(1), (s >> 32) as u32);
    s as u32
}
fn GetFileSizeEx(c: &mut Ctx) -> u32 {
    let Some(f) = obj::file(c.a(0)) else { return 0 };
    put64(c.a(1), f.size());
    1
}
fn SetEndOfFile(c: &mut Ctx) -> u32 {
    let Some(f) = obj::file(c.a(0)) else { return 0 };
    let p = f.seek(SeekFrom::Current(0)).unwrap_or(0);
    let r = f.f.lock().unwrap().set_len(p).is_ok();
    ok(r)
}
fn FlushFileBuffers(_: &mut Ctx) -> u32 {
    1
}
fn LockFile(_: &mut Ctx) -> u32 {
    1
}
fn UnlockFile(_: &mut Ctx) -> u32 {
    1
}
fn GetFileTime(c: &mut Ctx) -> u32 {
    let Some(f) = obj::file(c.a(0)) else { return 0 };
    let m = f.f.lock().unwrap().metadata().ok();
    let t = m.and_then(|m| m.modified().ok()).map_or(0, files::filetime);
    for i in 1..4 {
        if c.a(i) != 0 {
            put64(c.a(i), t);
        }
    }
    1
}
fn SetFileTime(_: &mut Ctx) -> u32 {
    1
}
fn GetFileInformationByHandle(c: &mut Ctx) -> u32 {
    let Some(f) = obj::file(c.a(0)) else { return 0 };
    let Ok(m) = f.f.lock().unwrap().metadata() else { return 0 };
    let p = c.a(1);
    mem::fill(p, 52, 0);
    w32(p, files::attrs(&m));
    let t = m.modified().map_or(0, files::filetime);
    for o in [4, 12, 20] {
        put64(p + o, t);
    }
    w32(p + 28, 0x1234);
    w32(p + 32, (m.len() >> 32) as u32);
    w32(p + 36, m.len() as u32);
    w32(p + 40, 1);
    1
}

fn write_find_data(p: u32, name: &str, m: &std::fs::Metadata, wide: bool) {
    let size = if wide { 592 } else { 320 };
    mem::fill(p, size, 0);
    w32(p, files::attrs(m));
    let t = m.modified().map_or(0, files::filetime);
    for o in [4, 12, 20] {
        put64(p + o, t);
    }
    w32(p + 28, (m.len() >> 32) as u32);
    w32(p + 32, m.len() as u32);
    if wide {
        put_wstr(p + 44, 260, &name.encode_utf16().collect::<Vec<_>>());
    } else {
        put_cstr(p + 44, 260, &crate::text::s2a(name));
    }
}
fn find_first(c: &mut Ctx, wide: bool) -> u32 {
    let pat = arg_str(c.a(0), wide);
    if std::env::var_os("SAKE_TRACE_FILES").is_some() {
        let r = files::find(&pat).map(|f| f.entries.iter().map(|e| e.0.clone()).collect::<Vec<_>>().join(" "));
        eprintln!("sake: FindFirstFile({pat}) -> {r:?}");
    }
    match files::find(&pat) {
        Some(f) => {
            let (n, m) = &f.entries[0];
            write_find_data(c.a(1), n, m, wide);
            let mut f = f;
            f.at = 1;
            obj::add(Obj::Find(std::sync::Arc::new(Mutex::new(f))))
        }
        None => {
            last_error(2);
            0xffff_ffff
        }
    }
}
fn find_next(c: &mut Ctx, wide: bool) -> u32 {
    let f = obj::with(|t| match t.objs.get(&c.a(0)) {
        Some((Obj::Find(f), _)) => Some(f.clone()),
        _ => None,
    });
    let Some(f) = f else { return 0 };
    let mut f = f.lock().unwrap();
    if f.at >= f.entries.len() {
        last_error(18);
        return 0;
    }
    let (n, m) = &f.entries[f.at];
    write_find_data(c.a(1), n, m, wide);
    f.at += 1;
    1
}
fn FindFirstFileA(c: &mut Ctx) -> u32 {
    find_first(c, false)
}
fn FindFirstFileW(c: &mut Ctx) -> u32 {
    find_first(c, true)
}
fn FindFirstFileExA(c: &mut Ctx) -> u32 {
    let (p, d) = (c.a(0), c.a(2));
    w32(c.esp + 8, d);
    let _ = p;
    find_first(c, false)
}
fn FindNextFileA(c: &mut Ctx) -> u32 {
    find_next(c, false)
}
fn FindNextFileW(c: &mut Ctx) -> u32 {
    find_next(c, true)
}
fn FindClose(c: &mut Ctx) -> u32 {
    ok(obj::close(c.a(0)))
}
fn file_attrs(name: &str) -> u32 {
    if std::env::var_os("SAKE_TRACE_FILES").is_some() {
        eprintln!("sake: GetFileAttributes({name})");
    }
    match files::resolve(name).and_then(|h| std::fs::metadata(h).ok()) {
        Some(m) => files::attrs(&m),
        None => {
            last_error(2);
            0xffff_ffff
        }
    }
}
fn GetFileAttributesA(c: &mut Ctx) -> u32 {
    file_attrs(&arg_str(c.a(0), false))
}
fn GetFileAttributesW(c: &mut Ctx) -> u32 {
    file_attrs(&arg_str(c.a(0), true))
}
fn attrs_ex(c: &mut Ctx, wide: bool) -> u32 {
    let name = arg_str(c.a(0), wide);
    let Some(m) = files::resolve(&name).and_then(|h| std::fs::metadata(h).ok()) else {
        last_error(2);
        return 0;
    };
    let p = c.a(2);
    w32(p, files::attrs(&m));
    let t = m.modified().map_or(0, files::filetime);
    for o in [4, 12, 20] {
        put64(p + o, t);
    }
    w32(p + 28, (m.len() >> 32) as u32);
    w32(p + 32, m.len() as u32);
    1
}
fn GetFileAttributesExA(c: &mut Ctx) -> u32 {
    attrs_ex(c, false)
}
fn GetFileAttributesExW(c: &mut Ctx) -> u32 {
    attrs_ex(c, true)
}
fn SetFileAttributesA(_: &mut Ctx) -> u32 {
    1
}
fn SetFileAttributesW(_: &mut Ctx) -> u32 {
    1
}
pub fn mkdir(name: &str) -> u32 {
    let h = files::resolve_new(name);
    match std::fs::create_dir(&h) {
        Ok(()) => 1,
        Err(e) => {
            last_error(if e.kind() == std::io::ErrorKind::AlreadyExists { 183 } else { 3 });
            0
        }
    }
}
fn CreateDirectoryA(c: &mut Ctx) -> u32 {
    mkdir(&arg_str(c.a(0), false))
}
fn CreateDirectoryW(c: &mut Ctx) -> u32 {
    mkdir(&arg_str(c.a(0), true))
}
fn rmdir(name: &str) -> u32 {
    ok(files::resolve(name).is_some_and(|h| std::fs::remove_dir(h).is_ok()))
}
fn RemoveDirectoryA(c: &mut Ctx) -> u32 {
    rmdir(&arg_str(c.a(0), false))
}
fn RemoveDirectoryW(c: &mut Ctx) -> u32 {
    rmdir(&arg_str(c.a(0), true))
}
fn del(name: &str) -> u32 {
    match files::resolve(name) {
        Some(h) => ok(std::fs::remove_file(h).is_ok()),
        None => {
            last_error(2);
            0
        }
    }
}
fn DeleteFileA(c: &mut Ctx) -> u32 {
    del(&arg_str(c.a(0), false))
}
fn DeleteFileW(c: &mut Ctx) -> u32 {
    del(&arg_str(c.a(0), true))
}
fn mv(from: &str, to: &str, replace: bool) -> u32 {
    let Some(f) = files::resolve(from) else {
        last_error(2);
        return 0;
    };
    let t = files::resolve(to);
    if t.is_some() && !replace {
        last_error(183);
        return 0;
    }
    ok(std::fs::rename(f, t.unwrap_or_else(|| files::resolve_new(to))).is_ok())
}
fn MoveFileA(c: &mut Ctx) -> u32 {
    mv(&arg_str(c.a(0), false), &arg_str(c.a(1), false), false)
}
fn MoveFileW(c: &mut Ctx) -> u32 {
    mv(&arg_str(c.a(0), true), &arg_str(c.a(1), true), false)
}
fn MoveFileExA(c: &mut Ctx) -> u32 {
    mv(&arg_str(c.a(0), false), &arg_str(c.a(1), false), c.a(2) & 1 != 0)
}
fn MoveFileExW(c: &mut Ctx) -> u32 {
    mv(&arg_str(c.a(0), true), &arg_str(c.a(1), true), c.a(2) & 1 != 0)
}
fn copy_file(from: &str, to: &str, fail: bool) -> u32 {
    let Some(f) = files::resolve(from) else {
        last_error(2);
        return 0;
    };
    let t = files::resolve(to);
    if t.is_some() && fail {
        last_error(80);
        return 0;
    }
    ok(std::fs::copy(f, t.unwrap_or_else(|| files::resolve_new(to))).is_ok())
}
fn CopyFileA(c: &mut Ctx) -> u32 {
    copy_file(&arg_str(c.a(0), false), &arg_str(c.a(1), false), c.a(2) != 0)
}
fn CopyFileW(c: &mut Ctx) -> u32 {
    copy_file(&arg_str(c.a(0), true), &arg_str(c.a(1), true), c.a(2) != 0)
}
fn full_path_name(c: &mut Ctx, wide: bool) -> u32 {
    let name = arg_str(c.a(0), wide);
    let full = files::full_path(&name);
    let r = ret_str(c.a(2), c.a(1), &full, wide);
    if c.a(3) != 0 && r < c.a(1) {
        let pos = full.rfind('\\').map_or(0, |i| i + 1);
        let off = if wide { full[..pos].encode_utf16().count() * 2 } else { crate::text::s2a(&full[..pos]).len() };
        w32(c.a(3), c.a(2) + off as u32);
    }
    r
}
fn GetFullPathNameA(c: &mut Ctx) -> u32 {
    full_path_name(c, false)
}
fn GetFullPathNameW(c: &mut Ctx) -> u32 {
    full_path_name(c, true)
}
fn GetShortPathNameA(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(0), false);
    ret_str(c.a(1), c.a(2), &s, false)
}
fn GetShortPathNameW(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(0), true);
    ret_str(c.a(1), c.a(2), &s, true)
}
fn GetLongPathNameA(c: &mut Ctx) -> u32 {
    GetShortPathNameA(c)
}
fn GetLongPathNameW(c: &mut Ctx) -> u32 {
    GetShortPathNameW(c)
}
fn cur_dir(c: &mut Ctx, wide: bool) -> u32 {
    let d = files::cwd();
    let d = if d.len() > 3 { d.trim_end_matches('\\').to_string() } else { d };
    ret_str(c.a(1), c.a(0), &d, wide)
}
fn GetCurrentDirectoryA(c: &mut Ctx) -> u32 {
    cur_dir(c, false)
}
fn GetCurrentDirectoryW(c: &mut Ctx) -> u32 {
    cur_dir(c, true)
}
fn set_dir(d: &str) -> u32 {
    let full = files::full_path(d);
    if files::resolve(&full).is_none() {
        last_error(3);
        return 0;
    }
    files::set_cwd(&full);
    1
}
fn SetCurrentDirectoryA(c: &mut Ctx) -> u32 {
    set_dir(&arg_str(c.a(0), false))
}
fn SetCurrentDirectoryW(c: &mut Ctx) -> u32 {
    set_dir(&arg_str(c.a(0), true))
}
fn ensure_dir(w: &str) {
    let _ = std::fs::create_dir_all(files::to_host(w));
}
fn GetTempPathA(c: &mut Ctx) -> u32 {
    ensure_dir("C:\\windows\\temp");
    ret_str(c.a(1), c.a(0), "C:\\windows\\temp\\", false)
}
fn GetTempPathW(c: &mut Ctx) -> u32 {
    ensure_dir("C:\\windows\\temp");
    ret_str(c.a(1), c.a(0), "C:\\windows\\temp\\", true)
}
static TEMP_N: AtomicU32 = AtomicU32::new(0x100);
fn temp_name(c: &mut Ctx, wide: bool) -> u32 {
    let dir = arg_str(c.a(0), wide);
    let pre = arg_str(c.a(1), wide);
    let n = if c.a(2) != 0 { c.a(2) } else { TEMP_N.fetch_add(1, Ordering::Relaxed) };
    let p = format!("{}\\{}{:04X}.tmp", dir.trim_end_matches('\\'), pre.chars().take(3).collect::<String>(), n);
    if c.a(2) == 0 {
        let _ = std::fs::File::create(files::resolve_new(&p));
    }
    ret_str(c.a(3), 260, &p, wide);
    n
}
fn GetTempFileNameA(c: &mut Ctx) -> u32 {
    temp_name(c, false)
}
fn GetTempFileNameW(c: &mut Ctx) -> u32 {
    temp_name(c, true)
}
fn GetWindowsDirectoryA(c: &mut Ctx) -> u32 {
    ret_str(c.a(0), c.a(1), "C:\\windows", false)
}
fn GetWindowsDirectoryW(c: &mut Ctx) -> u32 {
    ret_str(c.a(0), c.a(1), "C:\\windows", true)
}
fn GetSystemDirectoryA(c: &mut Ctx) -> u32 {
    ret_str(c.a(0), c.a(1), "C:\\windows\\system32", false)
}
fn GetSystemDirectoryW(c: &mut Ctx) -> u32 {
    ret_str(c.a(0), c.a(1), "C:\\windows\\system32", true)
}
fn GetSystemWindowsDirectoryA(c: &mut Ctx) -> u32 {
    GetWindowsDirectoryA(c)
}
fn GetDriveTypeA(c: &mut Ctx) -> u32 {
    let d = arg_str(c.a(0), false).to_ascii_uppercase();
    if d.starts_with('C') || d.starts_with('Z') || c.a(0) == 0 { 3 } else { 1 }
}
fn GetDriveTypeW(c: &mut Ctx) -> u32 {
    let d = arg_str(c.a(0), true).to_ascii_uppercase();
    if d.starts_with('C') || d.starts_with('Z') || c.a(0) == 0 { 3 } else { 1 }
}
fn GetLogicalDrives(_: &mut Ctx) -> u32 {
    (1 << 2) | (1 << 25)
}
fn GetDiskFreeSpaceA(c: &mut Ctx) -> u32 {
    put32(c.a(1), 8);
    put32(c.a(2), 512);
    put32(c.a(3), 0x0100_0000);
    put32(c.a(4), 0x0200_0000);
    1
}
fn GetDiskFreeSpaceExA(c: &mut Ctx) -> u32 {
    for i in 1..4 {
        if c.a(i) != 0 {
            put64(c.a(i), 8 << 30);
        }
    }
    1
}
fn GetDiskFreeSpaceExW(c: &mut Ctx) -> u32 {
    GetDiskFreeSpaceExA(c)
}
fn GetVolumeInformationA(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        put_cstr(c.a(1), c.a(2), b"SAKE");
    }
    put32(c.a(3), 0x1234_5678);
    put32(c.a(4), 255);
    put32(c.a(5), 0x3);
    if c.a(6) != 0 {
        put_cstr(c.a(6), c.a(7), b"NTFS");
    }
    1
}
fn GetVolumeInformationW(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        put_wstr(c.a(1), c.a(2), &"SAKE".encode_utf16().collect::<Vec<_>>());
    }
    put32(c.a(3), 0x1234_5678);
    put32(c.a(4), 255);
    put32(c.a(5), 0x3);
    if c.a(6) != 0 {
        put_wstr(c.a(6), c.a(7), &"NTFS".encode_utf16().collect::<Vec<_>>());
    }
    1
}
fn SearchPathA(c: &mut Ctx) -> u32 {
    let (path, name, ext, cap, buf, part) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let mut n = arg_str(name, false);
    if ext != 0 && !n.contains('.') {
        n += &arg_str(ext, false);
    }
    let dirs: Vec<String> = if path != 0 { arg_str(path, false).split(';').map(String::from).collect() } else { vec![files::cwd()] };
    for d in dirs {
        let w = files::full_path(&format!("{}\\{n}", d.trim_end_matches('\\')));
        if files::resolve(&w).is_some() {
            let r = ret_str(buf, cap, &w, false);
            if part != 0 {
                w32(part, buf + w.rfind('\\').map_or(0, |i| i + 1) as u32);
            }
            return r;
        }
    }
    last_error(2);
    0
}
// file mappings: views are copies (read-only use: archives)
fn CreateFileMappingA(c: &mut Ctx) -> u32 {
    let f = obj::file(c.a(0));
    let size = c.a(4);
    obj::add(Obj::Mapping { file: f, size, view: 0 })
}
fn CreateFileMappingW(c: &mut Ctx) -> u32 {
    CreateFileMappingA(c)
}
fn MapViewOfFile(c: &mut Ctx) -> u32 {
    let (h, off_hi, off_lo, n) = (c.a(0), c.a(2), c.a(3), c.a(4));
    let info = obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Mapping { file, size, view }, _)) => Some((file.clone(), *size, *view)),
        _ => None,
    });
    let Some((file, size, view)) = info else { return 0 };
    if file.is_none() && view != 0 {
        return view;
    }
    let off = off_lo as u64 | (off_hi as u64) << 32;
    let total = match &file {
        Some(f) => f.size().saturating_sub(off) as u32,
        None => size,
    };
    let n = if n == 0 { total } else { n.min(total.max(n)) };
    let Some(b) = mem::reserve(0, n.max(1), 4, 0x40000, false) else { return 0 };
    mem::commit(b, 4);
    if let Some(f) = &file {
        let pos = f.seek(SeekFrom::Current(0)).unwrap_or(0);
        f.seek(SeekFrom::Start(off));
        f.read_into(b, n.min(total));
        f.seek(SeekFrom::Start(pos));
    } else {
        obj::with(|t| {
            if let Some((Obj::Mapping { view, .. }, _)) = t.objs.get_mut(&h) {
                *view = b;
            }
        });
    }
    b
}
fn MapViewOfFileEx(c: &mut Ctx) -> u32 {
    MapViewOfFile(c)
}
fn UnmapViewOfFile(c: &mut Ctx) -> u32 {
    let a = c.a(0);
    let named_view = obj::with(|t| t.objs.values().any(|(o, _)| matches!(o, Obj::Mapping { file: None, view, .. } if *view == a)));
    if !named_view {
        mem::release(a);
    }
    1
}

fn CreatePipe(_: &mut Ctx) -> u32 {
    0
}
fn CreateProcessA(c: &mut Ctx) -> u32 {
    eprintln!("sake: CreateProcess({}, {}) refused", arg_str(c.a(0), false), arg_str(c.a(1), false));
    last_error(2);
    0
}
fn CreateProcessW(c: &mut Ctx) -> u32 {
    eprintln!("sake: CreateProcess({}, {}) refused", arg_str(c.a(0), true), arg_str(c.a(1), true));
    last_error(2);
    0
}
fn WinExec(_: &mut Ctx) -> u32 {
    2
}
fn OpenProcess(_: &mut Ctx) -> u32 {
    0
}
fn GetStartupInfoA(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    mem::fill(p, 68, 0);
    w32(p, 68);
    w32(p + 44, 0);
    0
}
fn GetStartupInfoW(c: &mut Ctx) -> u32 {
    GetStartupInfoA(c)
}
fn SetHandleCount(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn FormatMessageA(c: &mut Ctx) -> u32 {
    let flags = c.a(0);
    let msg = format!("error {}\r\n", c.a(2));
    if flags & 0x100 != 0 {
        let a = crate::text::s2a(&msg);
        let p = mem::heap_alloc(a.len() as u32 + 1, true);
        mem::write(p, &a);
        w32(c.a(4), p);
        return a.len() as u32;
    }
    ret_str(c.a(4), c.a(5), &msg, false)
}
fn FormatMessageW(c: &mut Ctx) -> u32 {
    let flags = c.a(0);
    let msg = format!("error {}\r\n", c.a(2));
    if flags & 0x100 != 0 {
        let w: Vec<u16> = msg.encode_utf16().collect();
        let p = mem::heap_alloc(2 * w.len() as u32 + 2, true);
        put_wstr(p, w.len() as u32 + 1, &w);
        w32(c.a(4), p);
        return w.len() as u32;
    }
    ret_str(c.a(4), c.a(5), &msg, true)
}
fn Beep(_: &mut Ctx) -> u32 {
    1
}
fn GetComputerNameA(c: &mut Ctx) -> u32 {
    put_cstr(c.a(0), r32(c.a(1)), b"SAKE");
    w32(c.a(1), 5);
    1
}
fn GetUserNameA(c: &mut Ctx) -> u32 {
    put_cstr(c.a(0), r32(c.a(1)), b"user");
    w32(c.a(1), 5);
    1
}
fn RtlZeroMemory(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), c.a(1), 0);
    0
}
fn RtlMoveMemory(c: &mut Ctx) -> u32 {
    mem::copy(c.a(0), c.a(1), c.a(2));
    0
}
fn RtlFillMemory(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), c.a(1), c.a(2) as u8);
    0
}
fn InitializeSListHead(c: &mut Ctx) -> u32 {
    mem::fill(c.a(0), 8, 0);
    0
}
// interlocked singly linked lists (SLIST_HEADER: next, depth u16, sequence u16), under one lock
static SLIST: Mutex<()> = Mutex::new(());
fn slist_set(h: u32, next: u32, depth: u32) {
    w32(h, next);
    w16(h + 4, depth as u16);
    w16(h + 6, r16(h + 6).wrapping_add(1));
}
fn InterlockedPushEntrySList(c: &mut Ctx) -> u32 {
    let (h, e) = (c.a(0), c.a(1));
    let _g = SLIST.lock().unwrap();
    let old = r32(h);
    w32(e, old);
    slist_set(h, e, r16(h + 4) as u32 + 1);
    old
}
fn InterlockedPopEntrySList(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let _g = SLIST.lock().unwrap();
    let e = r32(h);
    if e != 0 {
        slist_set(h, r32(e), (r16(h + 4) as u32).saturating_sub(1));
    }
    e
}
fn InterlockedFlushSList(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    let _g = SLIST.lock().unwrap();
    let e = r32(h);
    slist_set(h, 0, 0);
    e
}
fn QueryDepthSList(c: &mut Ctx) -> u32 {
    r16(c.a(0) + 4) as u32
}
/// InterlockedPushListSListEx(head, first, last, count)
fn InterlockedPushListSListEx(c: &mut Ctx) -> u32 {
    let (h, first, last, n) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let _g = SLIST.lock().unwrap();
    let old = r32(h);
    w32(last, old);
    slist_set(h, first, r16(h + 4) as u32 + n);
    old
}
fn RtlFirstEntrySList(c: &mut Ctx) -> u32 {
    r32(c.a(0))
}
// tool help snapshots: the process list holds this process only (single-instance checks)
fn CreateToolhelp32Snapshot(_: &mut Ctx) -> u32 {
    create_named(None, Obj::Semaphore { count: 0, max: 1 })
}
fn process_entry(c: &mut Ctx, wide: bool) -> u32 {
    let p = c.a(1);
    let exe = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.name.clone()).unwrap_or_default();
    let size = if wide { 36 + 520 } else { 36 + 260 };
    mem::fill(p + 4, size - 4, 0);
    w32(p + 8, crate::PID);
    w32(p + 20, 1);
    w32(p + 28, 8);
    ret_str(p + 36, 260, &exe, wide);
    1
}
fn Process32First(c: &mut Ctx) -> u32 {
    process_entry(c, false)
}
fn Process32FirstW(c: &mut Ctx) -> u32 {
    process_entry(c, true)
}
fn no_more(_: &mut Ctx) -> u32 {
    last_error(18);
    0
}
/// ConvertDefaultLocale(lcid): the default locales become the user's
fn ConvertDefaultLocale(c: &mut Ctx) -> u32 {
    match c.a(0) {
        0 | 0x400 | 0x800 => lcid(),
        l => l,
    }
}
fn MakeSureDirectoryPathExists(c: &mut Ctx) -> u32 {
    let s = arg_str(c.a(0), false);
    let dir = &s[..s.rfind('\\').unwrap_or(0)];
    let mut at = 0;
    while let Some(i) = dir[at..].find('\\').map(|i| i + at) {
        if i > 2 {
            mkdir(&dir[..i]);
        }
        at = i + 1;
    }
    if !dir.is_empty() {
        mkdir(dir);
    }
    ok(dir.is_empty() || files::resolve(dir).is_some_and(|h| std::path::Path::new(&h).is_dir()))
}
fn WNetGetUniversalNameA(_: &mut Ctx) -> u32 {
    2250 // ERROR_NOT_CONNECTED: a local drive
}
fn GetCurrentPackageId(_: &mut Ctx) -> u32 {
    15700
}
fn AreFileApisANSI(_: &mut Ctx) -> u32 {
    1
}
fn SetFileApisToANSI(_: &mut Ctx) -> u32 {
    0
}

// Wine's PE DLLs (msvcr90, d3dx9...) log through these: their ERR lines go to stderr.
// SAKE_WINEDEBUG=<flags>[:channel,...] shows more (FIXME 1, ERR 2, WARN 4, TRACE 8), for
// the listed channels only when given ("15:dwrite,d2d").
fn wine_dbg_flags(ch: u32) -> u8 {
    static CFG: OnceLock<(u8, Vec<String>)> = OnceLock::new();
    let (flags, chans) = CFG.get_or_init(|| {
        let v = std::env::var("SAKE_WINEDEBUG").unwrap_or_default();
        let (f, c) = v.split_once(':').unwrap_or((&v, ""));
        let chans = c.split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        (f.parse().unwrap_or(0), chans)
    });
    let name = if ch != 0 { crate::text::a2s(&cstr(ch + 1)) } else { String::new() };
    if chans.is_empty() || chans.contains(&name) { *flags | 2 } else { 2 }
}
fn wine_dbg_header(c: &mut Ctx) -> u32 {
    // -1 makes the caller skip the message (their channels default to everything on)
    let ch = c.a(1);
    let flags = wine_dbg_flags(ch);
    if ch != 0 {
        w8(ch, flags);
    }
    if flags & 1 << c.a(0).min(7) == 0 {
        return u32::MAX;
    }
    let name = if ch != 0 { crate::text::a2s(&cstr(ch + 1)) } else { String::new() };
    let class = ["fixme", "err", "warn", "trace"].get(c.a(0) as usize).copied().unwrap_or("?");
    eprint!("sake wine {class}:{name}:{} ", crate::text::a2s(&cstr(c.a(2))));
    0
}
/// Stored in the channel too: the DLLs check its flags inline before every message (they
/// start as 0xff, "everything, not looked up yet").
fn wine_dbg_channel_flags(c: &mut Ctx) -> u32 {
    let flags = wine_dbg_flags(c.a(0));
    if c.a(0) != 0 {
        w8(c.a(0), flags);
    }
    flags as u32
}
fn wine_dbg_output(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0));
    eprint!("{}", crate::text::a2s(&s));
    s.len() as u32
}

pub fn init() {
    implement("msvcrt", "_beginthreadex", CreateThread);
    implement("msvcrt", "_beginthread", _beginthread);
    implement("msvcrt", "_endthreadex", _endthreadex);
    implement("msvcrt", "_endthread", _endthread);
    implement("kernel32", "SetThreadExecutionState", SetThreadExecutionState);
    implement("psapi", "GetProcessMemoryInfo", K32GetProcessMemoryInfo);
    implement("ntdll", "__wine_dbg_header", wine_dbg_header);
    implement("kernel32", "ResolveDelayLoadedAPI", ResolveDelayLoadedAPI);
    implement("kernel32", "IsBadStringPtrW", IsBadStringPtrW);
    implement("kernel32", "GetUserDefaultLocaleName", GetUserDefaultLocaleName);
    implement("kernel32", "LocaleNameToLCID", LocaleNameToLCID);
    implement("ntdll", "NtQuerySystemTime", NtQuerySystemTime);
    implement("ntdll", "RtlTimeToSecondsSince1970", RtlTimeToSecondsSince1970);
    implement("ntdll", "__wine_dbg_strdup", wine_dbg_strdup);
    implement("ntdll", "LdrGetDllHandle", LdrGetDllHandle);
    implement("ntdll", "RtlFindExportedRoutineByName", RtlFindExportedRoutineByName);
    implement("ntdll", "NtQueryVirtualMemory", NtQueryVirtualMemory);
    implement("kernel32", "LCIDToLocaleName", LCIDToLocaleName);
    implement("kernel32", "GetLocaleInfoEx", GetLocaleInfoEx);
    implement("kernel32", "GetSystemDefaultLocaleName", GetUserDefaultLocaleName);
    implement("ntdll", "__wine_dbg_get_channel_flags", wine_dbg_channel_flags);
    implement("ntdll", "__wine_dbg_output", wine_dbg_output);
    reg!("kernel32";
        GetCommandLineA, GetCommandLineW, GetEnvironmentStrings, GetEnvironmentStringsA, GetEnvironmentStringsW,
        FreeEnvironmentStringsA, FreeEnvironmentStringsW, GetEnvironmentVariableA, GetEnvironmentVariableW,
        SetEnvironmentVariableA, SetEnvironmentVariableW, ExpandEnvironmentStringsA, ExpandEnvironmentStringsW,
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThread, GetCurrentThreadId, ExitProcess, TerminateProcess,
        FatalAppExitA, ExitThread, CreateThread, ResumeThread, SuspendThread, GetExitCodeThread, GetExitCodeProcess,
        TerminateThread, GetThreadPriority, SetThreadPriority, SetPriorityClass, GetPriorityClass, SetThreadAffinityMask,
        SetProcessAffinityMask, GetProcessAffinityMask, SetThreadIdealProcessor, Sleep, SleepEx, SwitchToThread,
        CreateEventA, CreateEventW, CreateEventExW, CreateWaitableTimerA, CreateWaitableTimerW, CreateWaitableTimerExW,
        OpenWaitableTimerA, OpenWaitableTimerW, SetWaitableTimer, CancelWaitableTimer, OpenEventA, OpenEventW, SetEvent, ResetEvent, PulseEvent,
        CreateMutexA, CreateMutexW, OpenMutexA, ReleaseMutex, CreateSemaphoreA, CreateSemaphoreW, ReleaseSemaphore,
        WaitForSingleObject, WaitForSingleObjectEx, WaitForMultipleObjects, WaitForMultipleObjectsEx, CloseHandle,
        DuplicateHandle, InitializeCriticalSection, InitializeCriticalSectionAndSpinCount, InitializeCriticalSectionEx,
        DeleteCriticalSection, EnterCriticalSection, LeaveCriticalSection, TryEnterCriticalSection,
        InterlockedIncrement, InterlockedDecrement, InterlockedExchange, InterlockedExchangeAdd, InterlockedCompareExchange,
        TlsAlloc, TlsFree, TlsGetValue, TlsSetValue, FlsAlloc, FlsFree, FlsGetValue, FlsSetValue,
        GetLastError, SetLastError, SetErrorMode, GetErrorMode, RtlUnwind, RaiseException, SetUnhandledExceptionFilter,
        UnhandledExceptionFilter, IsDebuggerPresent, OutputDebugStringA, OutputDebugStringW, SetConsoleCtrlHandler,
        DebugBreak, EncodePointer, DecodePointer,
        VirtualAlloc, VirtualFree, VirtualQuery, VirtualProtect, VirtualLock, VirtualUnlock, FlushInstructionCache,
        IsBadReadPtr, IsBadWritePtr, IsBadCodePtr, IsBadStringPtrA, GetProcessHeap, HeapCreate, HeapDestroy, HeapAlloc,
        HeapFree, HeapReAlloc, HeapSize, HeapValidate, HeapCompact, HeapSetInformation, HeapLock, HeapUnlock,
        GlobalAlloc, GlobalFree, GlobalReAlloc, GlobalLock, GlobalUnlock, GlobalHandle, GlobalSize, GlobalFlags,
        LocalAlloc, LocalFree, LocalReAlloc, LocalLock, LocalUnlock, LocalSize, LocalHandle,
        GlobalMemoryStatus, GlobalMemoryStatusEx, GetSystemInfo, GetNativeSystemInfo, IsProcessorFeaturePresent,
        GetLogicalProcessorInformationEx, GetLogicalProcessorInformation, GetNumaHighestNodeNumber, GetCurrentProcessorNumber,
        GetCurrentProcessorNumberEx, GetNumaNodeProcessorMaskEx, GetNumaNodeProcessorMask, GetActiveProcessorCount,
        GetMaximumProcessorCount, GetActiveProcessorGroupCount, GetMaximumProcessorGroupCount, GetThreadGroupAffinity,
        SetThreadGroupAffinity, LCMapStringEx, CreateThreadpoolTimer, SetThreadpoolTimer, IsThreadpoolTimerSet,
        WaitForThreadpoolTimerCallbacks, CloseThreadpoolTimer, CreateThreadpoolWait, SetThreadpoolWait,
        WaitForThreadpoolWaitCallbacks, CloseThreadpoolWait, FreeLibraryWhenCallbackReturns, CreateSemaphoreExW,
        SetThreadStackGuarantee, FlushProcessWriteBuffers, RegisterWaitForSingleObject, UnregisterWait, UnregisterWaitEx,
        QueueUserWorkItem, CreateThreadpoolWork, SubmitThreadpoolWork, TrySubmitThreadpoolCallback,
        WaitForThreadpoolWorkCallbacks, CloseThreadpoolWork,
        CreateTimerQueue, DeleteTimerQueueEx, DeleteTimerQueue, CreateTimerQueueTimer, ChangeTimerQueueTimer, DeleteTimerQueueTimer,
        IsWow64Process, GetVersion, GetVersionExA, GetVersionExW, VerifyVersionInfoW, VerifyVersionInfoA, VerSetConditionMask,
        GetACP, GetOEMCP, IsValidCodePage, GetThreadLocale, SetThreadLocale, GetUserDefaultLCID, GetSystemDefaultLCID, EnumSystemLocalesA, EnumSystemLocalesW, EnumSystemLocalesEx,
        GetUserDefaultLangID, GetSystemDefaultLangID, GetUserDefaultUILanguage, GetSystemDefaultUILanguage,
        IsValidLocale, GetLocaleInfoA, GetLocaleInfoW, EnumCalendarInfoA, GetCPInfo, GetCPInfoExA, IsDBCSLeadByte,
        IsDBCSLeadByteEx, MultiByteToWideChar, WideCharToMultiByte, GetStringTypeA, GetStringTypeExA, GetStringTypeW,
        GetStringTypeExW, LCMapStringA, LCMapStringW, CompareStringA, CompareStringW, GetDateFormatA, GetTimeFormatA,
        MulDiv, lstrlenA, lstrlenW, lstrcpyA, lstrcpyW, lstrcpynA, lstrcpynW, lstrcatA, lstrcatW, lstrcmpA, lstrcmpiA,
        lstrcmpW, lstrcmpiW, GlobalAddAtomA, GlobalAddAtomW, AddAtomA, GlobalFindAtomA, FindAtomA, GlobalDeleteAtom,
        DeleteAtom, GlobalGetAtomNameA,
        GetSystemTime, GetLocalTime, GetSystemTimeAsFileTime, GetSystemTimePreciseAsFileTime, FileTimeToSystemTime,
        SystemTimeToFileTime, FileTimeToLocalFileTime, LocalFileTimeToFileTime, FileTimeToDosDateTime, CompareFileTime,
        GetTimeZoneInformation, GetTickCount, GetTickCount64, QueryPerformanceCounter, QueryPerformanceFrequency,
        GetProcessTimes, GetThreadTimes,
        GetModuleFileNameA, GetModuleFileNameW, GetModuleHandleA, GetModuleHandleW, GetModuleHandleExA,
        GetModuleHandleExW, LoadLibraryA, LoadLibraryW, LoadLibraryExA, LoadLibraryExW, FreeLibrary,
        DisableThreadLibraryCalls, GetProcAddress, FindResourceA, FindResourceW, FindResourceExA, FindResourceExW,
        LoadResource, LockResource, FreeResource, SizeofResource,
        GetStdHandle, SetStdHandle, GetFileType, GetConsoleMode, CreateFileA, CreateFileW, _lopen, _lclose, _lread,
        _llseek, ReadFile, WriteFile, SetFilePointer, SetFilePointerEx, GetFileSize, GetFileSizeEx, SetEndOfFile,
        FlushFileBuffers, LockFile, UnlockFile, GetFileTime, SetFileTime, GetFileInformationByHandle,
        FindFirstFileA, FindFirstFileW, FindFirstFileExA, FindNextFileA, FindNextFileW, FindClose, GetFileAttributesA,
        GetFileAttributesW, GetFileAttributesExA, GetFileAttributesExW, SetFileAttributesA, SetFileAttributesW,
        CreateDirectoryA, CreateDirectoryW, RemoveDirectoryA, RemoveDirectoryW, DeleteFileA, DeleteFileW, MoveFileA,
        MoveFileW, MoveFileExA, MoveFileExW, CopyFileA, CopyFileW, GetFullPathNameA, GetFullPathNameW,
        GetShortPathNameA, GetShortPathNameW, GetLongPathNameA, GetLongPathNameW, GetCurrentDirectoryA,
        GetCurrentDirectoryW, SetCurrentDirectoryA, SetCurrentDirectoryW, GetTempPathA, GetTempPathW,
        GetTempFileNameA, GetTempFileNameW, GetWindowsDirectoryA, GetWindowsDirectoryW, GetSystemDirectoryA,
        GetSystemDirectoryW, GetSystemWindowsDirectoryA, GetDriveTypeA, GetDriveTypeW, GetLogicalDrives,
        GetDiskFreeSpaceA, GetDiskFreeSpaceExA, GetDiskFreeSpaceExW, GetVolumeInformationA, GetVolumeInformationW,
        SearchPathA,
        CreateFileMappingA, CreateFileMappingW, MapViewOfFile, MapViewOfFileEx, UnmapViewOfFile, CreatePipe,
        CreateProcessA, CreateProcessW, WinExec, OpenProcess, GetStartupInfoA, GetStartupInfoW, SetHandleCount,
        FormatMessageA, FormatMessageW, Beep, GetComputerNameA, InitializeSListHead, GetCurrentPackageId,
        CreateToolhelp32Snapshot, Process32First, Process32FirstW, ConvertDefaultLocale,
        InterlockedPushEntrySList, InterlockedPopEntrySList, InterlockedFlushSList, QueryDepthSList, InterlockedPushListSListEx,
        AreFileApisANSI, SetFileApisToANSI,
    );
    reg!("advapi32"; GetUserNameA);
    for n in ["Process32Next", "Process32NextW", "Module32First", "Module32FirstW", "Module32Next", "Module32NextW", "Thread32First", "Thread32Next"] {
        implement("kernel32", n, no_more);
    }
    reg!("dbghelp"; MakeSureDirectoryPathExists);
    reg!("mpr"; WNetGetUniversalNameA);
    implement("mpr", "WNetGetUniversalNameW", WNetGetUniversalNameA);
    reg!("advapi32"; RegisterTraceGuidsW, UnregisterTraceGuids, TraceEvent, GetTraceLoggerHandle, GetTraceEnableFlags, GetTraceEnableLevel);
    reg!("ntdll"; RtlGetVersion, RtlGetNtVersionNumbers, RtlFirstEntrySList);
    implement("ntdll", "RtlInterlockedPushEntrySList", InterlockedPushEntrySList);
    implement("ntdll", "RtlInterlockedPopEntrySList", InterlockedPopEntrySList);
    implement("ntdll", "RtlInterlockedFlushSList", InterlockedFlushSList);
    implement("ntdll", "RtlQueryDepthSList", QueryDepthSList);
    implement("ntdll", "RtlInitializeSListHead", InitializeSListHead);
    reg!("ntdll"; RtlZeroMemory, RtlMoveMemory, RtlFillMemory, RtlAllocateHeap, RtlFreeHeap, RtlEnterCriticalSection,
        RtlLeaveCriticalSection, InitializeSRWLock, AcquireSRWLockExclusive, AcquireSRWLockShared,
        ReleaseSRWLockExclusive, ReleaseSRWLockShared, TryAcquireSRWLockExclusive, TryAcquireSRWLockShared,
        InitializeConditionVariable, SleepConditionVariableSRW, SleepConditionVariableCS, WakeConditionVariable,
        WakeAllConditionVariable, InitOnceExecuteOnce, InitOnceInitialize, GetThreadContext, SetThreadContext, FreeLibraryAndExitThread,
        K32GetProcessMemoryInfo);
    let _ = (EAX, ESP);
}

pub fn set_event(h: u32) {
    event_set(h, true);
}

pub fn pulse_event(h: u32) {
    event_set(h, true);
    std::thread::yield_now();
    event_set(h, false);
}
