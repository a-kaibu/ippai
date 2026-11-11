//! kernel32 (and the ntdll/kernelbase names that are the same functions).

#![allow(non_snake_case)]

use crate::HashMap;
use std::io::SeekFrom;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Condvar, Mutex, OnceLock};

use koji::{EAX, ESP};

use crate::api::{implement, Ctx};
use crate::files;
use crate::mem::{self, cstr, put32, put_cstr, put_wstr, r16, r32, w16, w32, w8, wstr};
use crate::obj::{self, Obj};
use crate::text::{a2s, a2w};

pub const PROCESS_HEAP: u32 = 0x0015_0000;

macro_rules! reg {
    ($dll:literal; $($f:ident),* $(,)?) => { $( implement($dll, stringify!($f), $f); )* };
}

pub fn last_error(e: u32) {
    w32(crate::cur().teb + crate::teb_last_error(), e);
}
pub fn get_last_error() -> u32 {
    r32(crate::cur().teb + crate::teb_last_error())
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
/// `s` in `cap` characters, cut short if it must be, ended with a 0
fn put_name(dst: u32, cap: u32, s: &str, wide: bool) -> u32 {
    if wide {
        put_wstr(dst, cap, &s.encode_utf16().collect::<Vec<_>>())
    } else {
        put_cstr(dst, cap, &crate::text::s2a(s))
    }
}
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
    set_command_line_raw(&s);
}
/// The command line as it is (a process another one started: CreateProcess's)
pub fn set_command_line_raw(s: &str) {
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

/// The process environment, in the order Windows keeps it (sorted, case-insensitively).
/// Besides the defaults, the guest sees the host's WINETEST_* variables (what Wine passes its
/// tests) and the NAME=VALUE pairs of SAKE_GUEST_ENV (`;` between them).
static ENV: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);

fn with_env<R>(f: impl FnOnce(&mut Vec<(String, String)>) -> R) -> R {
    let mut g = ENV.lock().unwrap();
    let e = g.get_or_insert_with(|| {
        let mut e: Vec<(String, String)> = [
            ("PATH", "C:\\windows\\system32;C:\\windows"),
            ("TEMP", "C:\\windows\\temp"),
            ("TMP", "C:\\windows\\temp"),
            ("windir", "C:\\windows"),
            ("SystemRoot", "C:\\windows"),
            ("OS", "Windows_NT"),
            ("COMPUTERNAME", "SAKE"),
            ("USERNAME", "user"),
            // (where SHGetFolderPath's folders are)
            ("USERPROFILE", "C:\\users\\user"),
            ("HOMEDRIVE", "C:"),
            ("HOMEPATH", "\\users\\user"),
            ("APPDATA", "C:\\users\\user\\AppData\\Roaming"),
            ("LOCALAPPDATA", "C:\\users\\user\\AppData\\Local"),
            ("PUBLIC", "C:\\users\\Public"),
            ("ProgramData", "C:\\ProgramData"),
            ("ALLUSERSPROFILE", "C:\\ProgramData"),
            ("ProgramFiles", "C:\\Program Files"),
            ("CommonProgramFiles", "C:\\Program Files\\Common Files"),
            ("SystemDrive", "C:"),
        ]
        .iter()
        .map(|&(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let mut extra: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("WINETEST_")).collect();
        if let Ok(list) = std::env::var("SAKE_GUEST_ENV") {
            extra.extend(list.split(';').filter_map(|kv| kv.split_once('=')).filter(|(k, _)| !k.is_empty()).map(|(k, v)| (k.to_string(), v.to_string())));
        }
        for (k, v) in extra {
            env_put(&mut e, &k, Some(&v));
        }
        e.sort_by_key(|(k, _)| k.to_ascii_uppercase());
        e
    });
    f(e)
}

/// Sets (or with None removes) a variable, keeping the order.
fn env_put(e: &mut Vec<(String, String)>, name: &str, value: Option<&str>) {
    let key = name.to_ascii_uppercase();
    let at = e.iter().position(|(k, _)| k.to_ascii_uppercase() == key);
    match (at, value) {
        (Some(i), Some(v)) => e[i].1 = v.to_string(),
        (Some(i), None) => {
            e.remove(i);
        }
        (None, Some(v)) => {
            let i = e.iter().position(|(k, _)| k.to_ascii_uppercase() > key).unwrap_or(e.len());
            e.insert(i, (name.to_string(), v.to_string()));
        }
        (None, None) => {}
    }
}

fn env_block(wide: bool) -> u32 {
    let s: String = with_env(|e| e.iter().map(|(k, v)| format!("{k}={v}\0")).collect::<String>()) + "\0";
    if wide {
        let w: Vec<u16> = s.encode_utf16().collect();
        let p = mem::heap_alloc(2 * w.len() as u32, true);
        for (i, c) in w.iter().enumerate() {
            w16(p + 2 * i as u32, *c);
        }
        p
    } else {
        let a = crate::text::s2a(&s);
        let p = mem::heap_alloc(a.len() as u32, true);
        mem::write(p, &a);
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
/// The environment as the C runtime sees it: get, set (None removes), all variables.
pub(crate) fn env_set(name: &str, value: Option<&str>) {
    with_env(|e| env_put(e, name, value));
}
pub(crate) fn env_vars() -> Vec<(String, String)> {
    with_env(|e| e.clone())
}
pub fn env_get(name: &str) -> Option<String> {
    let key = name.to_ascii_uppercase();
    with_env(|e| e.iter().find(|(k, _)| k.to_ascii_uppercase() == key).map(|(_, v)| v.clone()))
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
        // (a buffer too short gets an empty string, as Windows' W one: winref envvars)
        Some(v) if v.encode_utf16().count() as u32 >= c.a(2) && c.a(1) != 0 && c.a(2) != 0 => {
            w16(c.a(1), 0);
            v.encode_utf16().count() as u32 + 1
        }
        Some(v) => ret_str(c.a(1), c.a(2), &v, true),
        None => {
            last_error(203);
            0
        }
    }
}
/// SetEnvironmentVariable(name, value): a NULL value removes the variable
fn set_env(c: &mut Ctx, wide: bool) -> u32 {
    let name = arg_str(c.a(0), wide);
    if c.a(0) == 0 || name.is_empty() || name.contains('=') {
        last_error(87);
        return 0;
    }
    let value = (c.a(1) != 0).then(|| arg_str(c.a(1), wide));
    with_env(|e| env_put(e, &name, value.as_deref()));
    1
}
fn SetEnvironmentVariableA(c: &mut Ctx) -> u32 {
    set_env(c, false)
}
fn SetEnvironmentVariableW(c: &mut Ctx) -> u32 {
    set_env(c, true)
}
/// %NAME% replaced by its value (unknown names stay as written)
pub(crate) fn expand_env(s: &str) -> String {
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
    match (n >= c.a(2), wide) {
        // (the ANSI one asks for a character more than it needs when the buffer is short, as
        // Windows' does: winref envvars)
        (true, false) => n + 1,
        (true, true) => n,
        (false, _) => n + 1,
    }
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
    crate::pid()
}
fn GetCurrentThread(_: &mut Ctx) -> u32 {
    obj::CURRENT_THREAD
}
/// GetThreadId(h): 0 (ERROR_INVALID_HANDLE) for what is not a thread
fn GetThreadId(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h == obj::CURRENT_THREAD {
        return crate::cur().tid;
    }
    match obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Thread { tid, .. } | Obj::RemoteThread { tid, .. }, _)) => Some(*tid),
        _ => None,
    }) {
        Some(tid) => tid,
        None => {
            last_error(6);
            0
        }
    }
}
fn GetCurrentThreadId(_: &mut Ctx) -> u32 {
    crate::cur().tid
}

fn ExitProcess(c: &mut Ctx) -> u32 {
    let code = c.a(0);
    if crate::env_on!("SAKE_TRACE_EXIT") {
        eprintln!("sake: ExitProcess({code}) from {:#x}", r32(c.esp));
    }
    unsafe { libc::exit(code as i32) }
}
use crate::process::PROCESS_ALL_ACCESS;
/// The access a handle to this process has (GetCurrentProcess's: all), None for another handle
pub fn self_process(h: u32) -> Option<u32> {
    crate::process::of_handle(h).filter(|&(pid, _)| crate::process::is_self(pid)).map(|(_, a)| a)
}
fn TerminateProcess(c: &mut Ctx) -> u32 {
    match crate::process::of_handle(c.a(0)) {
        Some((_, a)) if a & 1 == 0 => {
            last_error(5);
            0
        }
        Some((pid, _)) if crate::process::is_self(pid) => {
            if crate::env_on!("SAKE_TRACE_EXIT") {
                eprintln!("sake: TerminateProcess({}) of itself", c.a(1));
            }
            unsafe { libc::exit(c.a(1) as i32) }
        }
        Some((pid, _)) => crate::process::terminate(pid, c.a(1)) as u32,
        None => {
            last_error(6);
            0
        }
    }
}
fn FatalAppExitA(c: &mut Ctx) -> u32 {
    eprintln!("sake: FatalAppExit: {}", arg_str(c.a(1), false));
    unsafe { libc::exit(1) }
}

/// The calling thread ends (from the thread's own code: ExitThread or a return).
pub fn exit_thread(code: u32) -> ! {
    kura::snap::unregister();
    let t = crate::cur();
    let h = t.obj;
    obj::with(|tb| {
        if let Some((Obj::Thread { done, code: c, .. }, _)) = tb.objs.get_mut(&h) {
            *done = true;
            *c = code;
        }
        // mutexes it still holds are abandoned: the next wait gets WAIT_ABANDONED
        for (o, _) in tb.objs.values_mut() {
            if let Obj::Mutex { owner, count } = o {
                if *owner == t.tid {
                    *owner = obj::ABANDONED;
                    *count = 0;
                }
            }
        }
    });
    crate::kern::with(|k| k.abandon(t.tid));
    obj::signal();
    obj::close(h);
    crate::apc::forget(t.tid);
    if t.tid == crate::process::first_tid(crate::pid()) {
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
/// _beginthreadex(security, stack, start, arg, flags, tid*): no start routine is EINVAL and 0,
/// no thread (Wine's ucrtbase:thread; the thread at address 0 crashed the test meanwhile)
fn _beginthreadex(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        crate::crt::set_errno(22); // EINVAL
        return 0;
    }
    CreateThread(c)
}
/// _beginthread(start, stack, arg): no start routine is EINVAL and -1
fn _beginthread(c: &mut Ctx) -> u32 {
    let (start, stack, arg) = (c.a(0), c.a(1), c.a(2));
    if start == 0 {
        crate::crt::set_errno(22); // EINVAL
        return u32::MAX;
    }
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
/// GetProcessMemoryInfo(process, PROCESS_MEMORY_COUNTERS(_EX)*, bytes): a modest working set
/// (64 MB); needs query access
fn K32GetProcessMemoryInfo(c: &mut Ctx) -> u32 {
    let (h, p, cb) = (c.a(0), c.a(1), c.a(2));
    if !psapi_access(h, false) {
        return 0;
    }
    let ps = mem::ps();
    let (size, ex) = (8 + 8 * ps, 8 + 9 * ps);
    if cb < size {
        last_error(122);
        return 0;
    }
    let n = if cb >= ex { ex } else { size };
    mem::fill(p, n, 0);
    w32(p, n);
    w32(p + 4, 20_000); // page faults
    for i in [0, 1, 6, 7, 8] {
        if 8 + (i + 1) * ps <= n {
            mem::wp(p + 8 + i * ps, 64 << 20);
        }
    }
    1
}

/// Memory in pages as GlobalMemoryStatusEx tells it, for GetPerformanceInfo and
/// NtQuerySystemInformation alike: (physical, available, commit total, commit limit, peak,
/// paged pool, nonpaged pool)
fn perf_pages() -> [u32; 7] {
    let pages = (PHYS / 4096) as u32;
    [pages, pages / 4 * 3, pages / 4, 2 * pages, pages / 4, 0x6000, 0x2000]
}
/// The threads that run (by id) and the handles open
fn counts() -> (Vec<u32>, u32) {
    obj::with(|t| {
        let mut tids: Vec<u32> = t.objs.iter().filter_map(|(_, (o, _))| match o {
            Obj::Thread { tid, done: false, .. } => Some(*tid),
            _ => None,
        }).collect();
        tids.sort();
        tids.dedup();
        (tids, t.objs.iter().count() as u32)
    })
}
/// GetPerformanceInfo(PERFORMANCE_INFORMATION*, bytes): in pages, one process with its threads
/// and handles
fn K32GetPerformanceInfo(c: &mut Ctx) -> u32 {
    let (p, cb) = (c.a(0), c.a(1));
    let ps = mem::ps();
    let size = if mem::x64() { 104 } else { 56 };
    if cb < size {
        last_error(24); // ERROR_BAD_LENGTH
        return 0;
    }
    mem::fill(p, size, 0);
    w32(p, size);
    let [phys, avail, commit, limit, peak, paged, nonpaged] = perf_pages();
    let at = |i: u32| p + ps + i * ps; // the SIZE_T fields after cb
    for (i, v) in [commit, limit, peak, phys, avail, phys / 8, paged + nonpaged, paged, nonpaged, 4096].into_iter().enumerate() {
        mem::wp(at(i as u32), v);
    }
    let (tids, handles) = counts();
    w32(at(10), handles);
    w32(at(10) + 4, 1);
    w32(at(10) + 8, tids.len().max(1) as u32);
    1
}
/// NtQuerySystemInformation's classes sysinfo.rs leaves to us (class, buffer, bytes, &needed):
/// SystemPerformanceInformation, SystemProcessInformation (this process, its threads); others
/// STATUS_INVALID_INFO_CLASS
pub(crate) fn query_system_processes(c: &mut Ctx) -> u32 {
    let (class, p, len, pr) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let ps = mem::ps();
    let x64 = mem::x64();
    let [_, avail, commit, limit, peak, paged, nonpaged] = perf_pages();
    let (need, exact) = match class {
        2 => (312, false),
        5 => {
            let (tids, _) = counts();
            let name = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.rsplit('\\').next().unwrap_or("").to_string()).unwrap_or_default();
            let (fixed, ti) = if x64 { (0x100, 0x50) } else { (0xb8, 0x40) };
            (fixed + ti * tids.len().max(1) as u32 + 2 * (name.encode_utf16().count() as u32 + 1), false)
        }
        _ => return 0xc000_0003,
    };
    if pr != 0 {
        w32(pr, need);
    }
    if (exact && len != need) || len < need {
        return 0xc000_0004; // STATUS_INFO_LENGTH_MISMATCH
    }
    mem::fill(p, need, 0);
    match class {
        2 => {
            for (o, v) in [(44, avail), (48, commit), (52, limit), (56, peak), (112, paged), (116, nonpaged)] {
                w32(p + o, v);
            }
        }
        _ => {
            let (tids, handles) = counts();
            let tids = if tids.is_empty() { vec![crate::cur().tid] } else { tids };
            let name: Vec<u16> = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.rsplit('\\').next().unwrap_or("").to_string()).unwrap_or_default().encode_utf16().collect();
            let (fixed, ti) = if x64 { (0x100, 0x50) } else { (0xb8, 0x40) };
            w32(p + 4, tids.len() as u32);
            let s = p + fixed + ti * tids.len() as u32;
            for (i, &u) in name.iter().enumerate() {
                mem::w16(s + 2 * i as u32, u);
            }
            mem::w16(p + 0x38, 2 * name.len() as u16);
            mem::w16(p + 0x3a, 2 * name.len() as u16 + 2);
            mem::wp(p + 0x38 + ps, s);
            let (pid_at, hc_at) = if x64 { (0x50, 0x60) } else { (0x44, 0x4c) };
            w32(p + pid_at - ps, 8); // base priority: normal
            mem::wp(p + pid_at, crate::pid());
            w32(p + hc_at, handles);
            w32(p + hc_at + 4, 1); // session
            for (i, &tid) in tids.iter().enumerate() {
                let t = p + fixed + ti * i as u32;
                let cid = if x64 { 0x28 } else { 0x20 };
                mem::wp(t + cid, crate::pid());
                mem::wp(t + cid + ps, tid);
                w32(t + cid + 2 * ps, 8);
                w32(t + cid + 2 * ps + 4, 8);
                w32(t + cid + 2 * ps + 12, 5); // Waiting
            }
        }
    }
    0
}
/// The process's modules in load order, as EnumProcessModules lists them: the exe, then ntdll,
/// kernel32 and kernelbase (always there on Windows), then every DLL loaded (mono finds a
/// P/Invoke into "__Internal" or an unmapped DLL by GetProcAddress on each)
fn process_modules() -> Vec<u32> {
    let exe = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.base);
    let mut v: Vec<u32> = exe.into_iter().collect();
    for d in ["ntdll", "kernel32", "kernelbase"] {
        let b = crate::pe::hle_module(d);
        if !v.contains(&b) {
            v.push(b);
        }
    }
    for m in crate::pe::MODULES.read().unwrap().iter() {
        if !v.contains(&m.base) {
            v.push(m.base);
        }
    }
    v
}
/// EnumProcessModules(process, modules, bytes, &needed): as many handles as fit, and the bytes
/// all of them take
fn K32EnumProcessModules(c: &mut Ctx) -> u32 {
    let (h, p, cb, pn) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if !psapi_access(h, true) {
        return 0;
    }
    let all = process_modules();
    let ps = mem::ps();
    // (the list is written before the count: a NULL count fails after it)
    if p == 0 && cb >= ps {
        last_error(998); // ERROR_NOACCESS
        return 0;
    }
    for (i, &b) in all.iter().enumerate().take((cb / ps) as usize) {
        mem::wp(p + i as u32 * ps, b);
    }
    if pn == 0 {
        last_error(998);
        return 0;
    }
    w32(pn, all.len() as u32 * ps);
    1
}
/// EnumProcessModulesEx(.., filter): a process of one kind has its own modules only
fn K32EnumProcessModulesEx(c: &mut Ctx) -> u32 {
    if c.a(4) > 3 {
        last_error(87);
        return 0;
    }
    K32EnumProcessModules(c)
}
/// Whether psapi may read this process's modules through `h`: PROCESS_QUERY_INFORMATION (or
/// its limited form) and PROCESS_VM_READ; ERROR_INVALID_HANDLE / ERROR_ACCESS_DENIED if not
fn psapi_access(h: u32, vm_read: bool) -> bool {
    match self_process(h) {
        None => last_error(6),
        Some(a) if a & 0x1400 == 0 || (vm_read && a & 0x10 == 0) => last_error(5),
        Some(_) => return true,
    }
    false
}
/// The module a psapi call names (0: the exe), or ERROR_INVALID_HANDLE
fn psapi_module(process: u32, h: u32) -> Option<u32> {
    if !psapi_access(process, true) {
        return None;
    }
    let base = if h == 0 { crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.base) } else { crate::pe::by_base(h).map(|_| h) };
    if base.is_none() {
        last_error(6);
    }
    base
}
/// GetModuleInformation(process, module, MODULEINFO*, bytes): base, size, entry point
/// (module 0: TRUE with a NULL base, yet the size and entry point not 0: the exe's, as the
/// names have it; winref modules)
fn K32GetModuleInformation(c: &mut Ctx) -> u32 {
    let (p, cb) = (c.a(2), c.a(3));
    let Some(b) = psapi_module(c.a(0), c.a(1)) else { return 0 };
    let ps = mem::ps();
    if cb < 3 * ps {
        last_error(122);
        return 0;
    }
    let (size, entry) = crate::pe::with(b, |m| (m.size, m.entry)).unwrap();
    let (b, size, entry) = module_info(c.a(1), b, size, entry);
    mem::wp(p, b);
    w32(p + ps, size);
    if mem::x64() {
        w32(p + 12, 0);
    }
    mem::wp(p + 2 * ps, entry);
    1
}
/// MODULEINFO for the module asked (0: the exe, `base` its base): the base as asked, the
/// size and entry point the module's
fn module_info(asked: u32, base: u32, size: u32, entry: u32) -> (u32, u32, u32) {
    (if asked == 0 { 0 } else { base }, size, entry)
}
#[cfg(test)]
#[test]
fn module_info_of_module_0() {
    // (winref modules: TRUE, base NULL, size and entry point not 0)
    assert_eq!(module_info(0, 0x40_0000, 0x5000, 0x40_1000), (0, 0x5000, 0x40_1000));
    assert_eq!(module_info(0x40_0000, 0x40_0000, 0x5000, 0x40_1000), (0x40_0000, 0x5000, 0x40_1000));
}
/// GetModuleBaseName / GetModuleFileNameEx(process, module, buffer, chars): as much of the name
/// as fits, ended by a 0 when there is room for one; the characters copied. GetModuleFileNameEx
/// cut short ends the buffer with a 0 as GetModuleFileName does (Wine's psapi test, Windows 10)
fn psapi_name(c: &mut Ctx, base_only: bool, wide: bool) -> u32 {
    let (dst, cap) = (c.a(2), c.a(3));
    let Some(b) = psapi_module(c.a(0), c.a(1)) else { return 0 };
    let path = crate::pe::with(b, |m| m.path.clone()).unwrap();
    let name = if base_only { path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string() } else { path };
    if cap == 0 {
        last_error(122);
        return 0;
    }
    if !base_only {
        let n = if wide { name.encode_utf16().count() } else { crate::text::s2a(&name).len() } as u32;
        put_name(dst, cap, &name, wide);
        // (the ANSI one cut short counts what it kept before its 0, as Windows 10)
        return if !wide && n >= cap { cap - 1 } else { n.min(cap) };
    }
    if wide {
        let v: Vec<u16> = name.encode_utf16().collect();
        let n = v.len().min(cap as usize);
        for (i, &u) in v[..n].iter().enumerate() {
            mem::w16(dst + 2 * i as u32, u);
        }
        if n < cap as usize {
            mem::w16(dst + 2 * n as u32, 0);
        }
        n as u32
    } else {
        // (cut short: what fits before a 0, yet the count of characters asked for, as Windows 10
        // answers; the wide name is converted into the caller's buffer)
        let v = crate::text::s2a(&name);
        if v.len() >= cap as usize {
            mem::write(dst, &v[..cap as usize - 1]);
            mem::w8(dst + cap - 1, 0);
            return cap;
        }
        mem::write(dst, &v);
        mem::w8(dst + v.len() as u32, 0);
        v.len() as u32
    }
}
fn K32GetModuleBaseNameA(c: &mut Ctx) -> u32 {
    psapi_name(c, true, false)
}
fn K32GetModuleBaseNameW(c: &mut Ctx) -> u32 {
    psapi_name(c, true, true)
}
fn K32GetModuleFileNameExA(c: &mut Ctx) -> u32 {
    psapi_name(c, false, false)
}
fn K32GetModuleFileNameExW(c: &mut Ctx) -> u32 {
    psapi_name(c, false, true)
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

/// wait_gate for a new thread that will run `start(param)`: it may stand still for a save
/// state meanwhile (snap.rs START).
pub fn wait_gate_start(h: u32, start: u32, param: u32) {
    let mut g = GATE.lock().unwrap();
    while g.iter().any(|&(x, n)| x == h && n > 0) {
        if kura::snap::pausing() {
            drop(g);
            crate::snap::point_start(start, param);
            g = GATE.lock().unwrap();
            continue;
        }
        g = GATE_CV.wait(g).unwrap();
    }
    g.retain(|&(x, _)| x != h);
}

/// Save states: wakes the waits here so that they see the pause.
pub(crate) fn snap_wake() {
    drop(GATE.lock());
    GATE_CV.notify_all();
    drop(CS_LOCK.lock());
    CS_CV.notify_all();
    drop(SYNC.lock());
    SYNC_CV.notify_all();
    drop(ADDR_GEN.lock());
    ADDR_CV.notify_all();
}

fn ResumeThread(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if let Some(pid) = obj::with(|t| match t.objs.get(&h) {
        Some((Obj::RemoteThread { pid, .. }, _)) => Some(*pid),
        _ => None,
    }) {
        return crate::process::resume(pid);
    }
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
    // a thread that has not started yet (created suspended): one more suspension
    if let Some((_, n)) = GATE.lock().unwrap().iter_mut().find(|(x, n)| *x == c.a(0) && *n > 0) {
        *n += 1;
        return *n - 1;
    }
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
    match crate::process::of_handle(c.a(0)) {
        Some((pid, _)) => {
            put32(c.a(1), crate::process::exit_code(pid));
            1
        }
        None => {
            last_error(6);
            0
        }
    }
}
fn TerminateThread(_: &mut Ctx) -> u32 {
    1
}
// thread priorities are kept (and answered) but change nothing: by thread handle
static PRIORITY: Mutex<Option<HashMap<u32, i32>>> = Mutex::new(None);
fn thread_key(h: u32) -> u32 {
    if h == obj::CURRENT_THREAD { crate::cur().obj } else { h }
}
fn GetThreadPriority(c: &mut Ctx) -> u32 {
    PRIORITY.lock().unwrap().get_or_insert_with(HashMap::default).get(&thread_key(c.a(0))).copied().unwrap_or(0) as u32
}
fn SetThreadPriority(c: &mut Ctx) -> u32 {
    let p = c.i(1);
    if !matches!(p, -15 | -2..=2 | 15) {
        last_error(87);
        return 0;
    }
    PRIORITY.lock().unwrap().get_or_insert_with(HashMap::default).insert(thread_key(c.a(0)), p);
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
pub(crate) fn ncpu() -> u32 {
    if let Some(n) = std::env::var("SAKE_NCPU").ok().and_then(|v| v.parse::<u32>().ok()) {
        return n.clamp(1, 64);
    }
    std::thread::available_parallelism().map_or(4, |n| n.get() as u32).clamp(1, 8)
}
fn GetProcessAffinityMask(c: &mut Ctx) -> u32 {
    let m = (1u32 << ncpu()) - 1;
    crate::mem::putp(c.a(1), m);
    crate::mem::putp(c.a(2), m);
    1
}
fn SetThreadIdealProcessor(_: &mut Ctx) -> u32 {
    0
}
fn Sleep(c: &mut Ctx) -> u32 {
    let ms = c.a(0);
    if ms == 0 {
        std::thread::yield_now();
        return 0;
    }
    sleep_ms(ms);
    0
}

/// Sleeps `ms` (INFINITE: for good); a save state may be taken meanwhile (the call starts over
/// in a restored copy).
pub(crate) fn sleep_ms(ms: u32) {
    let end = (ms != u32::MAX).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms as u64));
    loop {
        let left = match end {
            Some(e) => match e.checked_duration_since(std::time::Instant::now()) {
                Some(d) if !d.is_zero() => d,
                _ => return,
            },
            None => std::time::Duration::from_secs(3600),
        };
        kura::snap::sleep(left);
        crate::snap::point();
    }
}
// WaitOnAddress: one generation counter for all addresses; any wake wakes every waiter, which
// returns TRUE (Windows allows that: callers check the value again)
static ADDR_GEN: Mutex<u64> = Mutex::new(0);
static ADDR_CV: Condvar = Condvar::new();

/// The `size`-byte value at `a` (1, 2, 4, 8; else None)
fn value_at(a: u32, size: u32) -> Option<u64> {
    Some(match size {
        1 => mem::r8(a) as u64,
        2 => r16(a) as u64,
        4 => r32(a) as u64,
        8 => mem::r64(a),
        _ => return None,
    })
}
/// WaitOnAddress(address, &compare, size, ms): TRUE once woken (or the value already differs),
/// FALSE with ERROR_TIMEOUT
fn WaitOnAddress(c: &mut Ctx) -> u32 {
    match wait_on_address(c.a(0), c.a(1), c.a(2), c.a(3)) {
        Ok(()) => 1,
        Err(e) => {
            last_error(e);
            0
        }
    }
}
/// WaitOnAddress's wait: Err(ERROR_INVALID_PARAMETER for a bad size, ERROR_TIMEOUT)
pub(crate) fn wait_on_address(a: u32, cmp: u32, size: u32, ms: u32) -> Result<(), u32> {
    let (Some(_), Some(want)) = (value_at(a, size), value_at(cmp, size)) else {
        return Err(87);
    };
    let g = ADDR_GEN.lock().unwrap_or_else(|e| e.into_inner());
    // (checked under the lock: a waker changes the value, then takes it to wake)
    if value_at(a, size) != Some(want) {
        return Ok(());
    }
    let start = *g;
    let end = (ms != 0xffff_ffff).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms as u64));
    let mut g = g;
    while *g == start {
        if kura::snap::pausing() {
            drop(g);
            crate::snap::point();
            g = ADDR_GEN.lock().unwrap_or_else(|e| e.into_inner());
            continue;
        }
        let left = match end {
            Some(e) => match e.checked_duration_since(std::time::Instant::now()) {
                Some(d) if !d.is_zero() => d,
                _ => return Err(1460),
            },
            None => std::time::Duration::from_secs(3600),
        };
        g = ADDR_CV.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
    }
    Ok(())
}
fn WakeByAddressAll(_: &mut Ctx) -> u32 {
    *ADDR_GEN.lock().unwrap_or_else(|e| e.into_inner()) += 1;
    ADDR_CV.notify_all();
    0
}

fn SleepEx(c: &mut Ctx) -> u32 {
    if c.a(1) == 0 {
        Sleep(c);
        return 0;
    }
    let r = crate::apc::wait(c, &[], false, c.a(0));
    if r == crate::apc::WAIT_IO_COMPLETION { r } else { 0 }
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

/// The kind of a named object (a name is one namespace: another kind under it is an error)
fn kind(o: &Obj) -> u8 {
    match o {
        Obj::Event { .. } => 1,
        Obj::Mutex { .. } => 2,
        Obj::Semaphore { .. } => 3,
        Obj::Timer { .. } => 4,
        Obj::Mapping { .. } => 5,
        _ => 0,
    }
}
/// A kernel object name: "Local\\" / "Global\\" prefixes, no other backslash (ERROR_PATH_NOT_FOUND)
fn name_ok(n: &str) -> bool {
    let rest = n.strip_prefix("Local\\").or_else(|| n.strip_prefix("Global\\")).unwrap_or(n);
    !rest.contains('\\')
}
/// A named event, mutex or semaphore as the kernel keeps it (other processes open it by name)
fn kern_sync(o: &Obj) -> Option<crate::kern::Sync> {
    use crate::kern::Sync;
    Some(match *o {
        Obj::Event { manual, set } => Sync::Event { manual, set },
        Obj::Mutex { owner, count } => Sync::Mutex { owner, pid: crate::pid(), count, abandoned: false },
        Obj::Semaphore { count, max } => Sync::Semaphore { count, max },
        _ => return None,
    })
}
pub(crate) fn create_named(name: Option<String>, o: Obj) -> u32 {
    if let Some(n) = &name {
        if !name_ok(n) {
            last_error(3);
            return 0;
        }
        let k = kind(&o);
        let local = obj::with(|t| t.names.contains_key(n));
        if let (Some(s), false) = (kern_sync(&o), local) {
            return match crate::kern::with(|kk| kk.create_sync(n, s, crate::pid())).0 {
                Ok((id, existed)) => {
                    let h = obj::add(Obj::Shared(id));
                    last_error(if existed { files::ERROR_ALREADY_EXISTS } else { 0 });
                    h
                }
                Err(_) => {
                    last_error(6);
                    0
                }
            };
        }
        if crate::kern::with(|kk| kk.sync_kind(n)).0.is_some() {
            last_error(6);
            return 0;
        }
        let existing = obj::with(|t| t.names.get(n).map(|h| (*h, t.objs.get(h).map_or(0, |(o, _)| kind(o)))));
        if let Some((h, hk)) = existing {
            if hk != k {
                last_error(6); // ERROR_INVALID_HANDLE: another kind of object has the name
                return 0;
            }
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
fn open_named(name: u32, wide: bool, want: u8) -> u32 {
    open_sync(arg_str(name, wide), want)
}
/// Opens a named event (1), mutex (2), semaphore (3) or timer (4); 0 and the last error
/// (ERROR_FILE_NOT_FOUND, ERROR_INVALID_HANDLE for another kind)
pub(crate) fn open_sync(n: String, want: u8) -> u32 {
    if let Some(k) = crate::kern::with(|kk| kk.sync_kind(&n)).0 {
        if k != want {
            last_error(6);
            return 0;
        }
        return match crate::kern::with(|kk| kk.open_sync(&n, want, crate::pid())).0 {
            Ok(id) => obj::add(Obj::Shared(id)),
            Err(e) => {
                last_error(e);
                0
            }
        };
    }
    match obj::with(|t| t.names.get(&n).map(|h| (*h, t.objs.get(h).map_or(0, |(o, _)| kind(o))))) {
        Some((_, k)) if k != want => {
            last_error(6);
            0
        }
        Some((h, _)) => obj::dup(h),
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
    open_named(c.a(2), false, 4)
}
fn OpenWaitableTimerW(c: &mut Ctx) -> u32 {
    open_named(c.a(2), true, 4)
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
    if r {
        last_error(0);
    }
    ok(r)
}
/// SetWaitableTimerEx(h, due, period, apc, arg, wake context, tolerable delay): as SetWaitableTimer
/// (Python's time.sleep waits on a high-resolution timer set this way)
fn SetWaitableTimerEx(c: &mut Ctx) -> u32 {
    SetWaitableTimer(c)
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
    open_named(c.a(2), false, 1)
}
fn OpenEventW(c: &mut Ctx) -> u32 {
    open_named(c.a(2), true, 1)
}
// I/O completion ports: a manual-reset event (set while packets wait) and its packets. Only
// PostQueuedCompletionStatus queues them (file I/O here completes synchronously).
static PORTS: Mutex<Option<HashMap<u32, std::collections::VecDeque<(u32, u32, u32)>>>> = Mutex::new(None);
/// CreateIoCompletionPort(file, port, key, threads): a new port, or `port` with the file joined
fn CreateIoCompletionPort(c: &mut Ctx) -> u32 {
    let (file, port) = (c.a(0), c.a(1));
    if port != 0 {
        if !PORTS.lock().unwrap().get_or_insert_with(HashMap::default).contains_key(&port) {
            last_error(87);
            return 0;
        }
        return port;
    }
    let _ = file;
    let h = obj::add(Obj::Event { manual: true, set: false });
    PORTS.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, Default::default());
    h
}
fn PostQueuedCompletionStatus(c: &mut Ctx) -> u32 {
    let (port, bytes, key, ov) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let ok = PORTS.lock().unwrap().get_or_insert_with(HashMap::default).get_mut(&port).map(|q| q.push_back((bytes, key, ov))).is_some();
    if !ok {
        last_error(6);
        return 0;
    }
    event_set(port, true);
    1
}
/// GetQueuedCompletionStatus(port, &bytes, &key, &overlapped, ms): the first packet; on a timeout
/// FALSE, WAIT_TIMEOUT and a NULL overlapped (key and bytes as they were)
fn GetQueuedCompletionStatus(c: &mut Ctx) -> u32 {
    let (port, pbytes, pkey, pov, ms) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let deadline = (ms != 0xffff_ffff).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms as u64));
    loop {
        let got = {
            let mut g = PORTS.lock().unwrap();
            let Some(q) = g.get_or_insert_with(HashMap::default).get_mut(&port) else {
                drop(g);
                last_error(6);
                return 0;
            };
            let p = q.pop_front();
            if q.is_empty() {
                event_set(port, false);
            }
            p
        };
        if let Some((bytes, key, ov)) = got {
            put32(pbytes, bytes);
            mem::putp(pkey, key);
            mem::putp(pov, ov);
            return 1;
        }
        let left = deadline.map_or(0xffff_ffff, |d| d.saturating_duration_since(std::time::Instant::now()).as_millis() as u32);
        if deadline.is_some() && left == 0 || obj::wait(&[port], false, left, None) != 0 {
            mem::putp(pov, 0);
            last_error(258);
            return 0;
        }
    }
}
pub fn event_set(h: u32, v: bool) -> u32 {
    if let Some(id) = shared_id(h) {
        let r = crate::kern::with(|k| k.set_event(id, v)).0;
        if !r {
            last_error(6);
        }
        obj::signal();
        return ok(r);
    }
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
    open_named(c.a(2), false, 2)
}
fn OpenMutexW(c: &mut Ctx) -> u32 {
    open_named(c.a(2), true, 2)
}
/// The kernel's id of a named event, mutex or semaphore handle
pub(crate) fn shared_id(h: u32) -> Option<u32> {
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Shared(id), _)) => Some(*id),
        _ => None,
    })
}
fn ReleaseMutex(c: &mut Ctx) -> u32 {
    match release_mutex(c.a(0)) {
        Ok(()) => 1,
        Err(e) => {
            last_error(e);
            0
        }
    }
}
/// Releases a mutex the thread owns: Err(ERROR_NOT_OWNER / ERROR_INVALID_HANDLE)
pub(crate) fn release_mutex(h: u32) -> Result<(), u32> {
    let tid = crate::cur().tid;
    if let Some(id) = shared_id(h) {
        let r = crate::kern::with(|k| k.release_mutex(id, tid)).0;
        obj::signal();
        return r;
    }
    let r = obj::with(|t| match t.objs.get_mut(&h) {
        Some((Obj::Mutex { owner, count }, _)) if *owner == tid => {
            *count -= 1;
            if *count == 0 {
                *owner = 0;
            }
            Ok(())
        }
        Some((Obj::Mutex { .. }, _)) => Err(288), // ERROR_NOT_OWNER
        _ => Err(6),
    });
    obj::signal();
    r
}
fn CreateSemaphoreA(c: &mut Ctx) -> u32 {
    if !semaphore_counts(c.i(1), c.i(2)) {
        return 0;
    }
    create_named(named(c.a(3), false), Obj::Semaphore { count: c.i(1), max: c.i(2) })
}
/// CreateSemaphore's counts: 0 <= initial <= maximum, maximum > 0 (else ERROR_INVALID_PARAMETER).
fn semaphore_counts(init: i32, max: i32) -> bool {
    let good = max > 0 && (0..=max).contains(&init);
    if !good {
        last_error(87);
    }
    good
}
fn CreateSemaphoreW(c: &mut Ctx) -> u32 {
    if !semaphore_counts(c.i(1), c.i(2)) {
        return 0;
    }
    create_named(named(c.a(3), true), Obj::Semaphore { count: c.i(1), max: c.i(2) })
}
fn ReleaseSemaphore(c: &mut Ctx) -> u32 {
    match release_semaphore(c.a(0), c.i(1)) {
        Ok(p) => {
            put32(c.a(2), p as u32);
            1
        }
        Err(e) => {
            last_error(e);
            0
        }
    }
}
/// Adds `n` to a semaphore: Ok(the previous count), Err(ERROR_INVALID_PARAMETER,
/// ERROR_TOO_MANY_POSTS, ERROR_INVALID_HANDLE)
pub(crate) fn release_semaphore(h: u32, n: i32) -> Result<i32, u32> {
    if let Some(id) = shared_id(h) {
        let r = if n <= 0 { Err(87) } else { crate::kern::with(|k| k.release_semaphore(id, n)).0 };
        obj::signal();
        return r.map(|p| p as i32);
    }
    let r = obj::with(|t| match t.objs.get_mut(&h) {
        Some(_) if n <= 0 => Err(87),
        Some((Obj::Semaphore { count, max }, _)) => {
            let prev = *count;
            if *count as i64 + n as i64 > *max as i64 {
                return Err(298); // ERROR_TOO_MANY_POSTS
            }
            *count += n;
            Ok(prev)
        }
        _ => Err(6),
    });
    obj::signal();
    r
}
fn WaitForSingleObject(c: &mut Ctx) -> u32 {
    obj::wait(&[c.a(0)], false, c.a(1), None)
}
fn WaitForSingleObjectEx(c: &mut Ctx) -> u32 {
    if c.a(2) == 0 {
        return WaitForSingleObject(c);
    }
    crate::apc::wait(c, &[c.a(0)], false, c.a(1))
}
fn WaitForMultipleObjects(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 || c.a(0) > 64 {
        last_error(87);
        return obj::WAIT_FAILED;
    }
    let hs: Vec<u32> = (0..c.a(0)).map(|i| mem::rp(c.a(1) + mem::ps() * i)).collect();
    // (wait-all on the same object twice: ERROR_INVALID_PARAMETER)
    if c.a(2) != 0 && (1..hs.len()).any(|i| hs[..i].contains(&hs[i])) {
        last_error(87);
        return obj::WAIT_FAILED;
    }
    obj::wait(&hs, c.a(2) != 0, c.a(3), None)
}
fn WaitForMultipleObjectsEx(c: &mut Ctx) -> u32 {
    if c.a(4) == 0 {
        return WaitForMultipleObjects(c);
    }
    if c.a(0) == 0 || c.a(0) > 64 {
        last_error(87);
        return obj::WAIT_FAILED;
    }
    let hs: Vec<u32> = (0..c.a(0)).map(|i| r32(c.a(1) + mem::ps() * i)).collect();
    crate::apc::wait(c, &hs, c.a(2) != 0, c.a(3))
}
fn CloseHandle(c: &mut Ctx) -> u32 {
    if let Some(r) = crate::nt::via::close(c) {
        return r;
    }
    let h = c.a(0);
    if crate::ws2::is_socket(h) {
        return ok(crate::ws2::close_socket(h) == 0);
    }
    let r = obj::close(h) || h == obj::CURRENT_PROCESS || h == obj::CURRENT_THREAD;
    if !r {
        last_error(6);
    }
    ok(r)
}
fn DuplicateHandle(c: &mut Ctx) -> u32 {
    let src = c.a(1);
    let h = if src == obj::CURRENT_THREAD {
        obj::dup(crate::cur().obj)
    } else if src == obj::CURRENT_PROCESS {
        obj::add(Obj::Process { access: PROCESS_ALL_ACCESS, pid: crate::pid() })
    } else if crate::ws2::is_socket(src) {
        let h = crate::ws2::dup_socket(src).unwrap_or(0);
        if c.a(6) & 1 != 0 {
            crate::ws2::close_socket(src);
        }
        h
    } else if c.a(6) & 1 != 0 {
        // DUPLICATE_CLOSE_SOURCE: Windows hands out the number it just freed (the old handle
        // keeps working through the new one)
        if obj::with(|t| t.objs.contains_key(&src)) { src } else { 0 }
    } else {
        obj::dup(src)
    };
    // (a HANDLE: 8 bytes on x64)
    mem::putp(c.a(3), h);
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
// RTL_CRITICAL_SECTION fields: DebugInfo, LockCount, RecursionCount, OwningThread, LockSemaphore,
// SpinCount (pointer-sized ones are 8 bytes on x64)
const LOCK: usize = 1;
const REC: usize = 2;
const OWNER: usize = 3;
const SEM: usize = 4;
const SPIN: usize = 5;
fn cs_off(f: usize) -> u32 {
    if mem::x64() { [0, 8, 12, 16, 24, 32][f] } else { [0, 4, 8, 12, 16, 20][f] }
}
/// LockCount while owned (Vista and later: bit 0 clear = locked, no waiters counted)
const CS_LOCKED: u32 = 0xffff_fffe;
fn cs_init(cs: u32) {
    mem::fill(cs, if mem::x64() { 40 } else { 24 }, 0);
    w32(cs + cs_off(LOCK), 0xffff_ffff);
}
pub fn cs_enter(cs: u32) {
    let me = crate::cur().tid;
    let own = atomic(cs + cs_off(OWNER));
    if own.load(Ordering::Acquire) == me {
        w32(cs + cs_off(REC), r32(cs + cs_off(REC)) + 1);
        return;
    }
    let mut spins = 0;
    loop {
        if own.compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            w32(cs + cs_off(REC), 1);
            w32(cs + cs_off(LOCK), CS_LOCKED);
            return;
        }
        spins += 1;
        if spins < 50 {
            std::hint::spin_loop();
            continue;
        }
        let waiters = atomic(cs + cs_off(SEM));
        let mut g = CS_LOCK.lock().unwrap();
        waiters.fetch_add(1, Ordering::SeqCst);
        while own.load(Ordering::SeqCst) != 0 {
            if kura::snap::pausing() {
                waiters.fetch_sub(1, Ordering::SeqCst);
                drop(g);
                crate::snap::point();
                g = CS_LOCK.lock().unwrap();
                waiters.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            g = CS_CV.wait_timeout(g, std::time::Duration::from_millis(50)).unwrap().0;
        }
        waiters.fetch_sub(1, Ordering::SeqCst);
        drop(g);
    }
}
pub fn cs_leave(cs: u32) {
    let n = r32(cs + cs_off(REC));
    if n > 1 {
        w32(cs + cs_off(REC), n - 1);
        return;
    }
    w32(cs + cs_off(REC), 0);
    w32(cs + cs_off(LOCK), 0xffff_ffff);
    atomic(cs + cs_off(OWNER)).store(0, Ordering::SeqCst);
    if atomic(cs + cs_off(SEM)).load(Ordering::SeqCst) != 0 {
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
    w32(c.a(0) + cs_off(SPIN), c.a(1) & 0x00ff_ffff);
    1
}
fn SetCriticalSectionSpinCount(c: &mut Ctx) -> u32 {
    let old = r32(c.a(0) + cs_off(SPIN));
    w32(c.a(0) + cs_off(SPIN), c.a(1) & 0x00ff_ffff);
    old
}
fn GetHandleInformation(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if !obj::with(|t| t.objs.contains_key(&h)) {
        last_error(6);
        return 0;
    }
    put32(c.a(1), obj::inherits(h) as u32);
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
    let own = atomic(cs + cs_off(OWNER));
    if own.load(Ordering::Acquire) == me {
        w32(cs + cs_off(REC), r32(cs + cs_off(REC)) + 1);
        return 1;
    }
    if own.compare_exchange(0, me, Ordering::Acquire, Ordering::Relaxed).is_ok() {
        w32(cs + cs_off(REC), 1);
        w32(cs + cs_off(LOCK), CS_LOCKED);
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

pub(crate) fn srw_try(l: u32, shared: bool) -> bool {
    let a = atomic(l);
    let v = a.load(Ordering::SeqCst);
    if shared {
        v & 1 == 0 && a.compare_exchange(v, v + 2, Ordering::SeqCst, Ordering::Relaxed).is_ok()
    } else {
        a.compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed).is_ok()
    }
}
pub(crate) fn srw_acquire(l: u32, shared: bool) {
    for _ in 0..50 {
        if srw_try(l, shared) {
            return;
        }
        std::hint::spin_loop();
    }
    let mut g = SYNC.lock().unwrap();
    // (counted before the retry: a release that misses the count frees the lock before it, so
    // the retry takes it; one that sees it notifies under SYNC, after this thread waits)
    SRW_WAITERS.fetch_add(1, Ordering::SeqCst);
    while !srw_try(l, shared) {
        if kura::snap::pausing() {
            drop(g);
            crate::snap::point();
            g = SYNC.lock().unwrap();
            continue;
        }
        g = SYNC_CV.wait_timeout(g, std::time::Duration::from_millis(20)).unwrap().0;
    }
    SRW_WAITERS.fetch_sub(1, Ordering::SeqCst);
}
/// Threads in srw_acquire's slow path (any lock): a release with none skips SYNC and the wake,
/// which cost Godot's loading thread seconds (one futex wake per release).
static SRW_WAITERS: AtomicU32 = AtomicU32::new(0);
pub(crate) fn srw_release(l: u32, shared: bool) {
    let a = atomic(l);
    if shared {
        a.fetch_sub(2, Ordering::SeqCst);
    } else {
        a.store(0, Ordering::SeqCst);
    }
    if SRW_WAITERS.load(Ordering::SeqCst) == 0 {
        return;
    }
    let _g = SYNC.lock().unwrap();
    SYNC_CV.notify_all();
}
fn InitializeSRWLock(c: &mut Ctx) -> u32 {
    crate::mem::wp(c.a(0), 0);
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
    crate::mem::wp(c.a(0), 0);
    0
}
/// Waits for a wake of `cv` (after `release`, before `reacquire`); false on timeout.
/// A condition variable's wait: `release(a, b)` lets go of its lock, `reacquire(a, b)` takes
/// it again (plain functions: a save state taken meanwhile has a restored copy call
/// `reacquire` and return `woken` from the API, a wake without a signal).
#[allow(clippy::too_many_arguments)]
pub(crate) fn cv_wait(cv: u32, ms: u32, release: fn(u64, u64), reacquire: fn(u64, u64), a: u64, b: u64, woken: u32) -> bool {
    // the generation is read before the lock is released (a wake in between is seen), but SYNC is
    // not held across `release` (srw_release takes it to notify: holding it deadlocked)
    let gen0 = *SYNC.lock().unwrap().get_or_insert_with(HashMap::default).entry(cv).or_insert(0);
    release(a, b);
    let mut g = SYNC.lock().unwrap();
    let end = (ms != u32::MAX).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms as u64));
    let mut woke = true;
    while g.as_ref().unwrap().get(&cv).copied() == Some(gen0) {
        if kura::snap::pausing() {
            drop(g);
            crate::snap::point_return(woken, reacquire, a, b);
            g = SYNC.lock().unwrap();
            continue;
        }
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
    reacquire(a, b);
    if !woke {
        last_error(1460);
    }
    woke
}
pub(crate) fn cv_wake(cv: u32) {
    let mut g = SYNC.lock().unwrap();
    let e = g.get_or_insert_with(HashMap::default).entry(cv).or_insert(0);
    *e = e.wrapping_add(1);
    SYNC_CV.notify_all();
}
fn SleepConditionVariableSRW(c: &mut Ctx) -> u32 {
    let (cv, l, ms, shared) = (c.a(0), c.a(1), c.a(2), c.a(3) & 1 != 0);
    cv_wait(cv, ms, |l, s| srw_release(l as u32, s != 0), |l, s| srw_acquire(l as u32, s != 0), l as u64, shared as u64, 1) as u32
}
fn SleepConditionVariableCS(c: &mut Ctx) -> u32 {
    let (cv, cs, ms) = (c.a(0), c.a(1), c.a(2));
    // a recursively entered section is left and entered again as a whole
    let depth = r32(cs + cs_off(REC)).max(1);
    cv_wait(
        cv,
        ms,
        |cs, _| {
            w32(cs as u32 + cs_off(REC), 1);
            cs_leave(cs as u32)
        },
        |cs, depth| {
            cs_enter(cs as u32);
            w32(cs as u32 + cs_off(REC), depth as u32)
        },
        cs as u64,
        depth as u64,
        1,
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
                // (done: the context the function gave, with 2 in its low bits)
                let got = if ctx != 0 { r32(ctx) & !3 } else { 0 };
                a.store(if r != 0 { got | 2 } else { 0 }, Ordering::SeqCst);
                let _g = SYNC.lock().unwrap();
                SYNC_CV.notify_all();
                return r;
            }
            Err(v) if v & 3 == 2 => {
                if ctx != 0 {
                    mem::putp(ctx, v & !3);
                }
                return 1;
            }
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
/// InitOnceBeginInitialize(once, flags, &pending, &context). The INIT_ONCE: 0, 1 while one
/// thread initializes, then the context with 2 in its low bits (as Windows keeps it).
fn InitOnceBeginInitialize(c: &mut Ctx) -> u32 {
    let (once, flags, pending, ctx) = (c.a(0), c.a(1), c.a(2), c.a(3));
    const CHECK_ONLY: u32 = 1;
    let a = atomic(once);
    loop {
        let v = a.load(Ordering::SeqCst);
        if v & 3 == 2 {
            put32(pending, 0);
            if ctx != 0 {
                mem::putp(ctx, v & !3);
            }
            return 1;
        }
        if flags & CHECK_ONLY != 0 {
            last_error(31); // ERROR_GEN_FAILURE: not done yet
            return 0;
        }
        if v == 0 && a.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
            put32(pending, 1);
            return 1;
        }
        let g = SYNC.lock().unwrap();
        if a.load(Ordering::SeqCst) == 1 {
            let _ = SYNC_CV.wait_timeout(g, std::time::Duration::from_millis(20));
        }
    }
}
/// InitOnceComplete(once, flags, context): INIT_ONCE_INIT_FAILED (4) lets the next caller try
fn InitOnceComplete(c: &mut Ctx) -> u32 {
    let (once, flags, ctx) = (c.a(0), c.a(1), c.a(2));
    let a = atomic(once);
    if a.load(Ordering::SeqCst) != 1 || ctx & 3 != 0 {
        last_error(87);
        return 0;
    }
    a.store(if flags & 4 != 0 { 0 } else { ctx | 2 }, Ordering::SeqCst);
    let _g = SYNC.lock().unwrap();
    SYNC_CV.notify_all();
    1
}
fn InitOnceInitialize(c: &mut Ctx) -> u32 {
    crate::mem::wp(c.a(0), 0);
    0
}

fn InterlockedIncrement(c: &mut Ctx) -> u32 {
    mem::atomic_rmw32(c.a(0), |v| v.wrapping_add(1)).wrapping_add(1)
}
fn InterlockedDecrement(c: &mut Ctx) -> u32 {
    mem::atomic_rmw32(c.a(0), |v| v.wrapping_sub(1)).wrapping_sub(1)
}
fn InterlockedExchange(c: &mut Ctx) -> u32 {
    let v = c.a(1);
    mem::atomic_rmw32(c.a(0), |_| v)
}
fn InterlockedExchangeAdd(c: &mut Ctx) -> u32 {
    let d = c.a(1);
    mem::atomic_rmw32(c.a(0), |v| v.wrapping_add(d))
}
fn InterlockedCompareExchange(c: &mut Ctx) -> u32 {
    let (new, cmp) = (c.a(1), c.a(2));
    mem::atomic_rmw32(c.a(0), |v| if v == cmp { new } else { v })
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
    match TLS_USED.lock().unwrap().get_mut(c.a(0) as usize) {
        Some(x) => {
            *x = false;
            1
        }
        None => {
            last_error(87);
            0
        }
    }
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
        last_error(87);
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
thread_local!(static THREAD_ERROR_MODE: std::cell::Cell<u32> = const { std::cell::Cell::new(0) });
/// SetThreadErrorMode(mode, &old): only SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX and
/// SEM_NOOPENFILEERRORBOX are taken
fn SetThreadErrorMode(c: &mut Ctx) -> u32 {
    let (m, old) = (c.a(0), c.a(1));
    if m & !0x8003 != 0 {
        last_error(87);
        return 0;
    }
    let prev = THREAD_ERROR_MODE.with(|t| t.replace(m));
    if old != 0 {
        w32(old, prev);
    }
    1
}
fn GetThreadErrorMode(_: &mut Ctx) -> u32 {
    THREAD_ERROR_MODE.with(|t| t.get())
}
pub(crate) fn thread_error_mode() -> u32 {
    THREAD_ERROR_MODE.with(|t| t.get())
}
pub(crate) fn set_thread_error_mode(m: u32) {
    THREAD_ERROR_MODE.with(|t| t.set(m))
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
    if crate::env_on!("SAKE_DEBUGSTR") {
        eprintln!("sake: debug: {}", arg_str(c.a(0), false).trim_end());
    }
    0
}
fn OutputDebugStringW(c: &mut Ctx) -> u32 {
    if crate::env_on!("SAKE_DEBUGSTR") {
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
    // the guest has 2 GB of address space, not 128 TB: a reservation that only wants an aligned
    // block inside (twice the block less a page) gets that block, aligned; else Unity 6, which
    // reserves twice each allocator region, runs out after a few (its regions are 64 MB here,
    // unity6.rs)
    let over = mem::over_reserved_block(size).filter(|_| addr == 0 && !commit);
    let b = over.and_then(|s| mem::reserve_aligned(s, s, prot, 0x20000)).or_else(|| mem::reserve(addr, size, prot, 0x20000, ty & 0x10_0000 != 0));
    let Some(b) = b else {
        if crate::env_on!("SAKE_TRACE_MEM") {
            eprintln!("sake: VirtualAlloc({addr:#x}, {size:#x}, {ty:#x}) failed; the address space:\n{}", mem::dump());
        }
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
        // an address in no allocation is ERROR_INVALID_ADDRESS, one inside but not its base (or
        // a size with MEM_RELEASE) ERROR_INVALID_PARAMETER
        if mem::find(addr).is_none() {
            last_error(487);
            return 0;
        }
        if size != 0 || mem::find(addr).is_some_and(|(b, _)| b != addr) {
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
/// VirtualQueryEx: this process's memory as VirtualQuery tells it (another process's is
/// not readable here: ERROR_ACCESS_DENIED).
fn VirtualQueryEx(c: &mut Ctx) -> u32 {
    if self_process(c.a(0)).is_none() {
        last_error(5);
        return 0;
    }
    virtual_query(c.a(1), c.a(2), c.a(3))
}
fn VirtualQuery(c: &mut Ctx) -> u32 {
    virtual_query(c.a(0), c.a(1), c.a(2))
}
fn virtual_query(a: u32, mbi: u32, len: u32) -> u32 {
    // MEMORY_BASIC_INFORMATION: (base, allocation base, allocation protection, region size,
    // state, protection, type); x64: pointers and the size 8 bytes, 48 in all
    let x64 = mem::x64();
    let size = if x64 { 48 } else { 28 };
    if len < size {
        return 0;
    }
    let page = a & !(mem::PAGE - 1);
    let (base, alloc, aprot, n, state, prot, kind) = match mem::query(a) {
        Some((b, r, start, n, state, prot)) => (start.max(page), b, r.alloc_prot, n - (page - start), state, prot, r.kind),
        None => {
            let (lo, n) = mem::free_at(a);
            (lo.max(page), 0, 0, n - (page.max(lo) - lo), 0x10000, 1, 0)
        }
    };
    if x64 {
        mem::fill(mbi, 48, 0);
        mem::w64(mbi, mem::widen(base));
        mem::w64(mbi + 8, if alloc != 0 { mem::widen(alloc) } else { 0 });
        w32(mbi + 16, aprot);
        mem::w64(mbi + 24, n as u64);
        w32(mbi + 32, state);
        w32(mbi + 36, prot);
        w32(mbi + 40, kind);
    } else {
        w32(mbi, base);
        w32(mbi + 4, alloc);
        w32(mbi + 8, aprot);
        w32(mbi + 12, n);
        w32(mbi + 16, state);
        w32(mbi + 20, prot);
        w32(mbi + 24, kind);
    }
    size
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
/// (nothing is paged out; a guard page in the range fails it once, with the guard gone, as
/// touching it would: STATUS_GUARD_PAGE_VIOLATION)
fn VirtualLock(c: &mut Ctx) -> u32 {
    let (a, n) = (c.a(0) & !0xfff, c.a(1).saturating_add(c.a(0) & 0xfff));
    let mut hit = false;
    for p in (0..n.div_ceil(0x1000)).map(|i| a.wrapping_add(i * 0x1000)) {
        hit |= mem::guard_hit(p);
    }
    if hit {
        last_error(0x8000_0001);
        return 0;
    }
    1
}
fn VirtualUnlock(_: &mut Ctx) -> u32 {
    1
}
fn FlushInstructionCache(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        kura::jit::code_changed(c.a(1) as usize, c.a(2).max(1) as usize);
    }
    1
}
/// IsBad*Ptr: whether any of the `n` bytes at `p` cannot be read (or written): reserved,
/// decommitted, PAGE_NOACCESS / guard pages, or read-only ones for a write
fn is_bad_for(p: u32, n: u32, write: bool) -> bool {
    n != 0 && mem::accessible(p, n, write, false) < n
}
fn is_bad(p: u32, n: u32) -> bool {
    is_bad_for(p, n, false)
}
/// Read/WriteProcessMemory(process, addr, buffer, n, done): the one process; a write may patch
/// code (Delphi programs hook their own functions this way: QLIE). As on Windows
/// (tests/winref/probes/procmem.c): a source that is not all readable copies nothing (count 0,
/// ERROR_PARTIAL_COPY); a destination that ends early takes what fits (that count, 299); a
/// write whose first byte cannot be written is ERROR_NOACCESS with the count left alone.
/// Executable pages take writes, read-only data does not. The count is a SIZE_T.
fn ReadProcessMemory(c: &mut Ctx) -> u32 {
    let (a, b, n, done) = (c.a(1), c.a(2), c.a(3), c.a(4));
    if mem::accessible(a, n, false, false) < n {
        mem::putp(done, 0);
        last_error(299);
        return 0;
    }
    let k = mem::accessible(b, n, true, false);
    mem::copy(b, a, k);
    mem::putp(done, k);
    if k < n {
        last_error(299);
        return 0;
    }
    1
}
fn WriteProcessMemory(c: &mut Ctx) -> u32 {
    let (a, b, n, done) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let k = mem::accessible(a, n, true, true).min(mem::accessible(b, n, false, false));
    if k == 0 && n != 0 {
        last_error(998);
        return 0;
    }
    mem::copy(a, b, k);
    kura::jit::code_changed(a as usize, k.max(1) as usize);
    mem::putp(done, k);
    if k < n {
        last_error(299);
        return 0;
    }
    1
}
fn IsBadReadPtr(c: &mut Ctx) -> u32 {
    ok(is_bad(c.a(0), c.a(1)))
}
fn IsBadWritePtr(c: &mut Ctx) -> u32 {
    ok(is_bad_for(c.a(0), c.a(1), true))
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
/// LdrGetDllHandle(path, flags, UNICODE_STRING *name, HMODULE *out)
fn LdrGetDllHandle(c: &mut Ctx) -> u32 {
    let us = c.a(2);
    let h = if us == 0 { 0 } else { module_handle(mem::rp(us + mem::ps()), true) };
    if c.a(3) != 0 {
        mem::wp(c.a(3), h);
    }
    if h == 0 { 0xc000_0135 } else { 0 }
}
// ntdll's counted strings: UNICODE_STRING / ANSI_STRING = { u16 Length, u16 MaximumLength, PTR Buffer }
fn put_counted(us: u32, len: u32, max: u32, buf: u32) {
    w16(us, len as u16);
    w16(us + 2, max as u16);
    // (UNICODE_STRING / ANSI_STRING: Buffer after the two lengths, pointer-aligned)
    mem::wp(us + mem::ps(), buf);
}
fn RtlInitUnicodeString(c: &mut Ctx) -> u32 {
    let (us, s) = (c.a(0), c.a(1));
    let n = if s == 0 { 0 } else { 2 * wstr(s).len() as u32 };
    put_counted(us, n, if s == 0 { 0 } else { n + 2 }, s);
    0
}
fn RtlInitAnsiString(c: &mut Ctx) -> u32 {
    let (us, s) = (c.a(0), c.a(1));
    let n = if s == 0 { 0 } else { cstr(s).len() as u32 };
    put_counted(us, n, if s == 0 { 0 } else { n + 1 }, s);
    0
}
fn RtlFreeUnicodeString(c: &mut Ctx) -> u32 {
    let us = c.a(0);
    if us != 0 && mem::rp(us + mem::ps()) != 0 {
        mem::heap_free(mem::rp(us + mem::ps()));
        put_counted(us, 0, 0, 0);
    }
    0
}
/// Stores `v` (code units of `unit` bytes) in counted string `us`, allocating if asked.
fn counted_out(us: u32, v: &[u16], unit: u32, alloc: bool) -> u32 {
    let n = unit * v.len() as u32;
    let buf = if alloc {
        let b = mem::heap_alloc(n + unit, true);
        put_counted(us, n, n + unit, b);
        b
    } else {
        if n + unit > r16(us + 2) as u32 {
            return 0x8000_0005; // STATUS_BUFFER_OVERFLOW
        }
        w16(us, n as u16);
        mem::rp(us + mem::ps())
    };
    for (i, &x) in v.iter().enumerate() {
        if unit == 2 { w16(buf + 2 * i as u32, x) } else { w8(buf + i as u32, x as u8) }
    }
    if unit == 2 { w16(buf + n, 0) } else { w8(buf + n, 0) }
    0
}
fn counted_bytes(us: u32) -> Vec<u8> {
    (0..r16(us) as u32).map(|i| mem::r8(mem::rp(us + mem::ps()) + i)).collect()
}
fn counted_wide(us: u32) -> Vec<u16> {
    (0..r16(us) as u32 / 2).map(|i| r16(mem::rp(us + mem::ps()) + 2 * i)).collect()
}
fn RtlAnsiStringToUnicodeString(c: &mut Ctx) -> u32 {
    let w = a2w(&counted_bytes(c.a(1)));
    counted_out(c.a(0), &w, 2, c.a(2) != 0)
}
fn RtlUnicodeStringToAnsiString(c: &mut Ctx) -> u32 {
    let a: Vec<u16> = crate::text::w2a(&counted_wide(c.a(1))).into_iter().map(|b| b as u16).collect();
    counted_out(c.a(0), &a, 1, c.a(2) != 0)
}
/// RtlMultiByteToUnicodeN(dst, dst bytes, &written bytes, src, src bytes): the ANSI code page's
/// text as UTF-16, cut to what fits (whole characters)
fn RtlMultiByteToUnicodeN(c: &mut Ctx) -> u32 {
    let (dst, cap, pw, src, n) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let b: Vec<u8> = (0..n).map(|i| mem::r8(src + i)).collect();
    let w = a2w(&b);
    let k = w.len().min(cap as usize / 2);
    for (i, &u) in w[..k].iter().enumerate() {
        w16(dst + 2 * i as u32, u);
    }
    if pw != 0 {
        w32(pw, 2 * k as u32);
    }
    0
}
/// RtlUnicodeToMultiByteN(dst, dst bytes, &written bytes, src, src bytes)
fn RtlUnicodeToMultiByteN(c: &mut Ctx) -> u32 {
    let (dst, cap, pw, src, n) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let w: Vec<u16> = (0..n / 2).map(|i| r16(src + 2 * i)).collect();
    let mut out = Vec::new();
    // (whole characters: a double-byte one that does not fit is left out)
    for &u in &w {
        let b = crate::text::w2a(&[u]);
        if out.len() + b.len() > cap as usize {
            break;
        }
        out.extend(b);
    }
    mem::write(dst, &out);
    if pw != 0 {
        w32(pw, out.len() as u32);
    }
    0
}
fn RtlMultiByteToUnicodeSize(c: &mut Ctx) -> u32 {
    let (pw, src, n) = (c.a(0), c.a(1), c.a(2));
    let b: Vec<u8> = (0..n).map(|i| mem::r8(src + i)).collect();
    w32(pw, 2 * a2w(&b).len() as u32);
    0
}
fn RtlUnicodeToMultiByteSize(c: &mut Ctx) -> u32 {
    let (pw, src, n) = (c.a(0), c.a(1), c.a(2));
    let w: Vec<u16> = (0..n / 2).map(|i| r16(src + 2 * i)).collect();
    w32(pw, crate::text::w2a(&w).len() as u32);
    0
}
/// RtlUpcaseUnicodeChar(c): Windows' upper case of one UTF-16 unit
fn RtlUpcaseUnicodeChar(c: &mut Ctx) -> u32 {
    crate::nt::up(c.a(0) as u16) as u32
}
/// RtlCompareUnicodeString(a, b, case insensitive): the difference of the first units that
/// differ (in upper case when asked), else of the lengths
fn RtlCompareUnicodeString(c: &mut Ctx) -> u32 {
    let (a, b, ci) = (counted_wide(c.a(0)), counted_wide(c.a(1)), c.a(2) & 0xff != 0);
    let f = |u: u16| if ci { crate::nt::up(u) } else { u };
    for (x, y) in a.iter().zip(&b) {
        let (x, y) = (f(*x), f(*y));
        if x != y {
            return (x as i32 - y as i32) as u32;
        }
    }
    (a.len() as i32 - b.len() as i32) as u32
}
/// RtlEqualUnicodeString(a, b, case insensitive)
fn RtlEqualUnicodeString(c: &mut Ctx) -> u32 {
    let (a, b) = (counted_wide(c.a(0)), counted_wide(c.a(1)));
    (a.len() == b.len() && RtlCompareUnicodeString(c) == 0) as u32
}
/// RtlDosPathNameToNtPathName_U(dos, &nt, &filepart, reserved): "\??\" + the full path
fn RtlDosPathNameToNtPathName_U(c: &mut Ctx) -> u32 {
    let (dos, nt, part) = (c.a(0), c.a(1), c.a(2));
    let full = files::full_path(&String::from_utf16_lossy(&wstr(dos)));
    let w: Vec<u16> = format!("\\??\\{full}").encode_utf16().collect();
    counted_out(nt, &w, 2, true);
    if part != 0 {
        let at = w.iter().rposition(|&x| x == b'\\' as u16).map_or(0, |i| i + 1);
        mem::wp(part, if at < w.len() { mem::rp(nt + mem::ps()) + 2 * at as u32 } else { 0 });
    }
    1
}
fn RtlFormatCurrentUserKeyPath(c: &mut Ctx) -> u32 {
    let w: Vec<u16> = "\\REGISTRY\\USER\\S-1-5-21-0-0-0-1000".encode_utf16().collect();
    counted_out(c.a(0), &w, 2, true)
}
/// LdrGetProcedureAddress(module, ANSI_STRING* name, ordinal, &address)
fn LdrGetProcedureAddress(c: &mut Ctx) -> u32 {
    let (h, name, ord, out) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let n;
    let imp = if name != 0 {
        n = a2s(&counted_bytes(name));
        crate::pe::Import::Name(&n)
    } else {
        crate::pe::Import::Ordinal(ord)
    };
    let r = crate::pe::proc_address_static(h, imp).unwrap_or(0);
    if out != 0 {
        mem::wp(out, r);
    }
    if r == 0 { 0xc000_0139 } else { 0 }
}
/// NtProtectVirtualMemory(process, &base, &size, protect, &old)
fn NtProtectVirtualMemory(c: &mut Ctx) -> u32 {
    // (*base and *size are pointer-sized, and come back as the whole pages changed)
    let (pb, ps) = (c.a(1), c.a(2));
    let (base, size) = (mem::rp(pb), mem::rp(ps));
    match mem::protect(base, size, c.a(3)) {
        Some(old) => {
            let start = base & !0xfff;
            let end = (base as u64 + size.max(1) as u64 + 0xfff) & !0xfff;
            mem::wp(pb, start);
            mem::wp(ps, (end - start as u64) as u32);
            put32(c.a(4), old);
            0
        }
        None => 0xc000_0045,
    }
}
fn RtlFindExportedRoutineByName(c: &mut Ctx) -> u32 {
    let n = a2s(&cstr(c.a(1)));
    crate::pe::proc_address(c.a(0), crate::pe::Import::Name(&n)).unwrap_or(0)
}

/// (process, address, class, info, length, *returned): basic information only
fn NtQueryVirtualMemory(c: &mut Ctx) -> u32 {
    let (a, class, info, len, ret) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    match class {
        0 => {
            let f = crate::api::export("kernel32", "VirtualQuery");
            let n = c.call(f, &[a, info, len]);
            if ret != 0 {
                mem::wp(ret, n);
            }
            if n == 0 { 0xc000_0004 } else { 0 }
        }
        2 => {
            // MemoryMappedFilenameInformation: MEMORY_SECTION_NAME, the name right after it
            let name = match mapped_file(a) {
                Ok(p) => device_path(&p),
                Err(1006) => return 0xc000_0098, // STATUS_FILE_INVALID
                Err(_) => return 0xc000_0141,    // STATUS_INVALID_ADDRESS
            };
            let w: Vec<u16> = name.encode_utf16().collect();
            let us = 2 * mem::ps();
            let need = us + 2 * w.len() as u32 + 2;
            if ret != 0 {
                mem::wp(ret, need);
            }
            if len < need {
                return 0x8000_0005; // STATUS_BUFFER_OVERFLOW
            }
            mem::w16(info, 2 * w.len() as u16);
            mem::w16(info + 2, 2 * w.len() as u16 + 2);
            mem::wp(info + mem::ps(), info + us);
            put_wstr(info + us, w.len() as u32 + 1, &w);
            0
        }
        _ => 0xc000_0003,
    }
}

fn GetProcessHeap(_: &mut Ctx) -> u32 {
    PROCESS_HEAP
}
static NEXT_HEAP: AtomicU32 = AtomicU32::new(0x0016_0000);
/// The maximum size of each fixed-size heap (HeapCreate with dwMaximumSize != 0)
static HEAP_MAX: Mutex<Option<HashMap<u32, u32>>> = Mutex::new(None);
fn HeapCreate(c: &mut Ctx) -> u32 {
    let h = NEXT_HEAP.fetch_add(0x10000, Ordering::Relaxed);
    if c.a(2) != 0 {
        HEAP_MAX.lock().unwrap().get_or_insert_with(HashMap::default).insert(h, c.a(2));
    }
    h
}
fn HeapDestroy(c: &mut Ctx) -> u32 {
    HEAP_MAX.lock().unwrap().get_or_insert_with(HashMap::default).remove(&c.a(0));
    1
}
/// Whether heap `h` can never hold a block of `size` bytes: a fixed heap's blocks fit in its
/// maximum size, and no block reaches the top of the address space
fn heap_refuses(h: u32, size: u32) -> bool {
    let max = if mem::x64() { 0x7fff_0000 } else { 0x7ffd_ffff };
    size > max || HEAP_MAX.lock().unwrap().get_or_insert_with(HashMap::default).get(&h).is_some_and(|&m| size > m)
}
fn HeapAlloc(c: &mut Ctx) -> u32 {
    let p = if heap_refuses(c.a(0), c.a(2)) { 0 } else { mem::heap_alloc_small(c.a(2), c.a(1) & 8 != 0) };
    if p == 0 {
        // (the error is ERROR_NOT_ENOUGH_MEMORY, which RtlAllocateHeap's caller sets)
        last_error(8);
    }
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
    // HEAP_REALLOC_IN_PLACE_ONLY: the block keeps its address and takes the new size, or the
    // call fails
    let r = if heap_refuses(c.a(0), size) || flags & 0x10 != 0 && !mem::heap_fits(p, size) {
        0
    } else {
        mem::heap_realloc(p, size, flags & 8 != 0)
    };
    if r == 0 {
        last_error(8);
    }
    r
}
fn HeapSize(c: &mut Ctx) -> u32 {
    mem::heap_size(c.a(2))
}
/// HeapValidate(heap, flags, block): a whole heap is valid; a block is one the heap gave
fn HeapValidate(c: &mut Ctx) -> u32 {
    let p = c.a(2);
    (p == 0 || mem::heap_owns(p)) as u32
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
/// Moveable Global/Local blocks as Windows keeps them: a handle is the address of the `ptr` of
/// an entry {flags, ptr} (two pointers, aligned to their size: `handle & (2 * ps - 1) == ps`,
/// which no heap pointer is) in a table of 0x10000. The flags word: 1 used, 2 moveable,
/// 4 discardable, 8 discarded (no block), 0x8000 DDE share, the lock count in bits 16-23; a
/// free entry's is the next free one. Programs look at these (Wine's heap test does).
mod handles {
    use super::*;
    use crate::mem::rp;
    pub const MAX: u32 = 0x1_0000;
    pub const USED: u32 = 1;
    pub const MOVEABLE: u32 = 2;
    pub const DISCARDABLE: u32 = 4;
    pub const DISCARDED: u32 = 8;
    pub const DDESHARE: u32 = 0x8000;

    #[derive(Default)]
    struct Table {
        base: u32,
        /// entries handed out so far, then the free ones (the last freed first)
        used: u32,
        free: Vec<u32>,
        /// a moveable block's handle, by its pointer (GlobalHandle)
        owner: HashMap<u32, u32>,
    }
    static T: Mutex<Option<Table>> = Mutex::new(None);
    fn with<R>(f: impl FnOnce(&mut Table) -> R) -> R {
        f(T.lock().unwrap().get_or_insert_with(Table::default))
    }
    fn size() -> u32 {
        2 * mem::ps()
    }
    /// A new entry's handle (0: the table is full)
    pub fn new(flags: u32) -> u32 {
        with(|t| {
            if t.base == 0 {
                t.base = mem::alloc(MAX * size()).unwrap_or(0);
            }
            let e = match t.free.pop() {
                Some(e) => e,
                None if t.base != 0 && t.used < MAX => {
                    t.used += 1;
                    t.base + (t.used - 1) * size()
                }
                None => return 0,
            };
            mem::wp(e, flags);
            mem::wp(e + mem::ps(), 0);
            e + mem::ps()
        })
    }
    /// The entry of handle `h` in use
    pub fn entry(h: u32) -> Option<u32> {
        let ps = mem::ps();
        if h & (2 * ps - 1) != ps {
            return None;
        }
        let e = h - ps;
        let base = with(|t| t.base);
        (base != 0 && e >= base && e < base + MAX * size() && rp(e) & USED != 0).then_some(e)
    }
    pub fn flags(e: u32) -> u32 {
        rp(e)
    }
    pub fn set_flags(e: u32, f: u32) {
        mem::wp(e, f);
    }
    pub fn ptr(e: u32) -> u32 {
        rp(e + mem::ps())
    }
    pub fn set_ptr(e: u32, p: u32) {
        let old = ptr(e);
        mem::wp(e + mem::ps(), p);
        with(|t| {
            t.owner.remove(&old);
            if p != 0 {
                t.owner.insert(p, e + mem::ps());
            }
        });
    }
    pub fn release(e: u32) {
        set_ptr(e, 0);
        // (a free entry's flags word links the free ones: the next, or the table itself)
        with(|t| {
            mem::wp(e, t.free.last().copied().unwrap_or(t.base));
            t.free.push(e);
        });
    }
    /// The handle of a moveable block's pointer
    pub fn owner(p: u32) -> Option<u32> {
        with(|t| t.owner.get(&p).copied())
    }
}

/// a block of the heap: a fixed Global/Local block is its pointer
fn is_block(h: u32) -> bool {
    h & (2 * mem::ps() - 1) == 0 && mem::heap_owns(h)
}

/// The memory of an HGLOBAL: a fixed block's pointer, a moveable one's block (0: discarded or
/// no handle). For sake's own users of HGLOBALs (OLE streams).
pub(crate) fn global_ptr(h: u32) -> u32 {
    match handles::entry(h) {
        Some(e) => handles::ptr(e),
        None => h,
    }
}
pub(crate) fn global_size(h: u32) -> u32 {
    if is_block(h) {
        return mem::heap_size(h);
    }
    match handles::entry(h) {
        Some(e) if handles::ptr(e) != 0 => mem::heap_size(handles::ptr(e)),
        Some(_) => 0,
        None => {
            last_error(6);
            0
        }
    }
}
/// GlobalAlloc / LocalAlloc (GMEM_MOVEABLE 2, GMEM_ZEROINIT 0x40, GMEM_DISCARDABLE 0x100,
/// GMEM_DDESHARE 0x2000): a moveable block of size 0 has no memory (discarded)
pub(crate) fn global_alloc(flags: u32, size: u32) -> u32 {
    let zero = flags & 0x40 != 0;
    if flags & 2 == 0 {
        let p = mem::heap_alloc(size, zero);
        if p == 0 {
            last_error(8);
        }
        return p;
    }
    let mut f = handles::USED | handles::MOVEABLE;
    if flags & 0x100 != 0 {
        f |= handles::DISCARDABLE;
    }
    if flags & 0x2000 != 0 {
        f |= handles::DDESHARE;
    }
    if size == 0 {
        f |= handles::DISCARDED;
    }
    let h = handles::new(f);
    let Some(e) = handles::entry(h) else {
        last_error(8);
        return 0;
    };
    if size != 0 {
        let p = mem::heap_alloc(size, zero);
        if p == 0 {
            handles::release(e);
            last_error(8);
            return 0;
        }
        handles::set_ptr(e, p);
    }
    h
}
/// 0, or the handle when it is none (ERROR_INVALID_HANDLE)
pub(crate) fn global_free(h: u32) -> u32 {
    if h == 0 {
        return 0;
    }
    if is_block(h) {
        mem::heap_free(h);
        return 0;
    }
    match handles::entry(h) {
        Some(e) => {
            mem::heap_free(handles::ptr(e));
            handles::release(e);
            0
        }
        None => {
            // (a pointer of another heap: freed as before, quietly)
            if mem::heap_free(h) {
                return 0;
            }
            last_error(6);
            h
        }
    }
}
/// GlobalReAlloc / LocalReAlloc (`global`: GlobalReAlloc refuses a locked block's discard)
pub(crate) fn global_realloc(h: u32, size: u32, flags: u32, global: bool) -> u32 {
    let (zero, moveable, modify, discardable) = (flags & 0x40 != 0, flags & 2 != 0, flags & 0x80 != 0, flags & 0x100 != 0);
    if is_block(h) {
        if modify {
            if global && moveable {
                // (GMEM_MODIFY | GMEM_MOVEABLE: the fixed block gets a handle)
                let n = handles::new(handles::USED | handles::MOVEABLE | if discardable { handles::DISCARDABLE } else { 0 });
                let Some(e) = handles::entry(n) else {
                    last_error(8);
                    return 0;
                };
                handles::set_ptr(e, h);
                return n;
            }
            return h;
        }
        if discardable {
            last_error(87);
            return 0;
        }
        let n = if moveable || mem::heap_fits(h, size) { mem::heap_realloc(h, size, zero) } else { 0 };
        if n == 0 {
            last_error(8);
        }
        return n;
    }
    let Some(e) = handles::entry(h) else {
        last_error(6);
        return 0;
    };
    let f = handles::flags(e);
    let lock = f >> 16 & 0xff;
    if modify {
        if discardable {
            handles::set_flags(e, f | handles::DISCARDABLE);
        }
        return h;
    }
    if global && lock != 0 && (size == 0 || discardable) {
        return 0;
    }
    if discardable {
        last_error(87);
        return 0;
    }
    let p = handles::ptr(e);
    if size != 0 {
        let n = if p == 0 {
            mem::heap_alloc(size, zero)
        } else if lock != 0 && !moveable && !mem::heap_fits(p, size) {
            0
        } else {
            mem::heap_realloc(p, size, zero)
        };
        if n == 0 {
            last_error(8);
            return 0;
        }
        handles::set_ptr(e, n);
        handles::set_flags(e, handles::flags(e) & !handles::DISCARDED);
        return h;
    }
    if moveable && lock == 0 {
        mem::heap_free(p);
        handles::set_ptr(e, 0);
        handles::set_flags(e, f | handles::DISCARDED);
        return h;
    }
    last_error(87);
    0
}
/// GlobalLock: a fixed block is its own memory; a moveable one counts the lock (up to 255) and
/// gives its block, or nothing when discarded (ERROR_DISCARDED)
pub(crate) fn global_lock(h: u32) -> u32 {
    if h == 0 {
        return 0;
    }
    match handles::entry(h) {
        Some(e) => {
            let (f, p) = (handles::flags(e), handles::ptr(e));
            if p == 0 {
                last_error(157);
                return 0;
            }
            if f >> 16 & 0xff != 0xff {
                handles::set_flags(e, f + 0x1_0000);
            }
            p
        }
        None if h & (2 * mem::ps() - 1) == 0 => h,
        None => {
            last_error(6);
            0
        }
    }
}
/// Whether the block stays locked: unlocking one that is not locked is ERROR_NOT_LOCKED, the
/// last unlock clears the error. A fixed block: GlobalUnlock says TRUE, LocalUnlock FALSE
/// with ERROR_NOT_LOCKED.
fn global_unlock(h: u32, global: bool) -> u32 {
    if h & (2 * mem::ps() - 1) == 0 {
        if global {
            return 1;
        }
        last_error(158);
        return 0;
    }
    let Some(e) = handles::entry(h) else {
        last_error(6);
        return 0;
    };
    let f = handles::flags(e);
    if f >> 16 & 0xff == 0 {
        last_error(158);
        return 0;
    }
    handles::set_flags(e, f - 0x1_0000);
    if (f >> 16 & 0xff) == 1 {
        last_error(0);
        return 0;
    }
    1
}
/// GMEM_* flags: the lock count, GMEM_DISCARDABLE 0x100, GMEM_DISCARDED 0x4000, GMEM_DDESHARE
/// 0x2000; a fixed block 0; no handle GMEM_INVALID_HANDLE 0x8000
fn global_flags(h: u32) -> u32 {
    if h & (2 * mem::ps() - 1) == 0 {
        return 0;
    }
    let Some(e) = handles::entry(h) else {
        last_error(6);
        return 0x8000;
    };
    let f = handles::flags(e);
    let mut r = f >> 16 & 0xff;
    for (bit, g) in [(handles::DISCARDABLE, 0x100), (handles::DISCARDED, 0x4000), (handles::DDESHARE, 0x2000)] {
        if f & bit != 0 {
            r |= g;
        }
    }
    r
}
/// GlobalHandle(pointer): a moveable block's handle, a fixed block itself
fn global_handle(p: u32) -> u32 {
    if p == 0 {
        last_error(87);
        return 0;
    }
    if let Some(h) = handles::owner(p) {
        return h;
    }
    if handles::entry(p).is_some() || mem::heap_owns(p) {
        return p;
    }
    last_error(6);
    0
}
fn GlobalAlloc(c: &mut Ctx) -> u32 {
    // (a fixed block of 0 bytes takes 1)
    let (f, n) = (c.a(0), c.a(1));
    global_alloc(f, if f & 2 == 0 { n.max(1) } else { n })
}
fn GlobalFree(c: &mut Ctx) -> u32 {
    global_free(c.a(0))
}
fn GlobalReAlloc(c: &mut Ctx) -> u32 {
    global_realloc(c.a(0), c.a(1), c.a(2), true)
}
fn GlobalLock(c: &mut Ctx) -> u32 {
    global_lock(c.a(0))
}
fn GlobalUnlock(c: &mut Ctx) -> u32 {
    global_unlock(c.a(0), true)
}
fn GlobalHandle(c: &mut Ctx) -> u32 {
    global_handle(c.a(0))
}
fn GlobalSize(c: &mut Ctx) -> u32 {
    global_size(c.a(0))
}
fn GlobalFlags(c: &mut Ctx) -> u32 {
    global_flags(c.a(0))
}
fn LocalAlloc(c: &mut Ctx) -> u32 {
    global_alloc(c.a(0), c.a(1))
}
fn LocalFree(c: &mut Ctx) -> u32 {
    global_free(c.a(0))
}
fn LocalReAlloc(c: &mut Ctx) -> u32 {
    global_realloc(c.a(0), c.a(1), c.a(2), false)
}
fn LocalLock(c: &mut Ctx) -> u32 {
    global_lock(c.a(0))
}
fn LocalUnlock(c: &mut Ctx) -> u32 {
    global_unlock(c.a(0), false)
}
fn LocalSize(c: &mut Ctx) -> u32 {
    global_size(c.a(0))
}
/// LocalFlags: GlobalFlags with LMEM_DISCARDABLE (0xf00) for a discardable block
fn LocalFlags(c: &mut Ctx) -> u32 {
    let f = global_flags(c.a(0));
    if f != 0x8000 && f & 0x100 != 0 { f | 0xf00 } else { f }
}
fn LocalHandle(c: &mut Ctx) -> u32 {
    global_handle(c.a(0))
}

/// Physical memory reported to the game: 4 GB, like a modest PC (games check a minimum: 256 MB
/// made Debonosu's 神楽黎明記 quit at startup). GlobalMemoryStatus clamps to 2 GB as Windows
/// does for a 32-bit process.
pub(crate) const PHYS: u64 = 4 << 30;
fn GlobalMemoryStatus(c: &mut Ctx) -> u32 {
    // MEMORYSTATUS: two DWORDs, then six SIZE_Ts (x64: 8 bytes, 56 in all)
    let p = c.a(0);
    let x64 = mem::x64();
    let clamp = |v: u64| if x64 { v } else { v.min(0x7fff_ffff) };
    let ps = mem::ps();
    w32(p, 8 + 6 * ps);
    w32(p + 4, 30);
    for (i, v) in [PHYS, PHYS / 4 * 3, 2 * PHYS, 2 * PHYS, 0x7ffe_0000, 0x6000_0000].into_iter().enumerate() {
        let at = p + 8 + ps * i as u32;
        if x64 { mem::w64(at, clamp(v)) } else { w32(at, clamp(v) as u32) }
    }
    0
}
/// GlobalMemoryStatusEx: dwLength must be the structure's 64 (else ERROR_INVALID_PARAMETER,
/// nothing written)
fn GlobalMemoryStatusEx(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if r32(p) != 64 {
        last_error(87);
        return 0;
    }
    w32(p + 4, 30);
    let q = |o: u32, v: u64| {
        w32(p + o, v as u32);
        w32(p + o + 4, (v >> 32) as u32);
    };
    q(8, PHYS);
    q(16, PHYS / 4 * 3);
    q(24, 2 * PHYS);
    q(32, 2 * PHYS);
    q(40, 0x7ffe_0000);
    q(48, 0x6000_0000);
    q(56, 0);
    1
}

fn GetSystemInfo(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if mem::x64() {
        // (x64: the addresses and the mask 8 bytes; PROCESSOR_ARCHITECTURE_AMD64, 8664)
        mem::fill(p, 48, 0);
        w16(p, 9);
        w32(p + 4, 0x1000);
        mem::w64(p + 8, 0x10000);
        mem::w64(p + 16, 0x7fff_fffe_ffff);
        mem::w64(p + 24, ((1u64 << ncpu()) - 1) as u64);
        w32(p + 32, ncpu());
        w32(p + 36, 8664);
        w32(p + 40, 0x10000);
        w16(p + 44, 6);
        w16(p + 46, 0x0f01);
        return 0;
    }
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
/// A pool thread's arguments: taken the first time, then kept in its record (a thread started
/// over by a restored save state finds them there).
fn tp_args(h: u32) -> Option<(u32, (u32, u32, u32))> {
    let t = crate::cur();
    if t.tp.is_none() {
        t.tp = tp_take(h);
    }
    t.tp
}
fn tp_take(h: u32) -> Option<(u32, (u32, u32, u32))> {
    let mut v = TP_ARGS.lock().unwrap();
    let i = v.iter().position(|x| x.0 == h)?;
    let (_, gen, a) = v.remove(i);
    Some((gen, a))
}
fn tp_timer_thread(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if let Some((gen, (delay, period, mode))) = tp_args(h) {
        let mut next = std::time::Instant::now() + std::time::Duration::from_millis(delay as u64);
        loop {
            let now = std::time::Instant::now();
            if next > now {
                kura::snap::sleep((next - now).min(std::time::Duration::from_millis(50)));
                crate::snap::point();
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
    if let Some((gen, (obj_h, ms, mode))) = tp_args(h) {
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
    mem::putp(out, h);
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
    mem::putp(out, h);
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
    ok(matches!(c.a(0), 3 | 8) || (matches!(c.a(0), 6 | 10) && crate::env_on!("SAKE_SSE")))
}
fn IsWow64Process(c: &mut Ctx) -> u32 {
    put32(c.a(1), 0);
    1
}

thread_local! {
    /// The thread's file system redirection setting (nonzero: off). We are not WoW64, so it
    /// redirects nothing; programs only read back what they set.
    static FS_REDIR_OFF: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}
/// RtlWow64EnableFsRedirectionEx(disable, &old): any value is kept as given, also when `old`
/// is a bad pointer (STATUS_ACCESS_VIOLATION then: Windows sets it before writing the old one).
/// A 64-bit process is not WoW64: STATUS_NOT_IMPLEMENTED, nothing kept.
fn fs_redirection(disable: u32, old: u32) -> u32 {
    if mem::x64() {
        return 0xc000_0002;
    }
    let was = FS_REDIR_OFF.with(|r| r.replace(disable));
    if old == 0 || is_bad(old, 4) {
        return 0xc000_0005;
    }
    put32(old, was);
    0
}
pub(crate) fn fs_redirection_off() -> u32 {
    FS_REDIR_OFF.with(|r| r.get())
}
pub(crate) fn set_fs_redirection_off(v: u32) {
    FS_REDIR_OFF.with(|r| r.set(v))
}
fn RtlWow64EnableFsRedirectionEx(c: &mut Ctx) -> u32 {
    fs_redirection(c.a(0), c.a(1))
}
/// RtlWow64EnableFsRedirection(enable)
fn RtlWow64EnableFsRedirection(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return 0xc000_0002;
    }
    FS_REDIR_OFF.with(|r| r.set((c.a(0) == 0) as u32));
    0
}
/// kernel32's switches in a 64-bit process (not WoW64): FALSE, ERROR_INVALID_FUNCTION
fn not_wow64() -> u32 {
    last_error(1);
    0
}
/// Wow64DisableWow64FsRedirection(PVOID* old)
fn Wow64DisableWow64FsRedirection(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return not_wow64();
    }
    let old = c.a(0);
    if old == 0 || is_bad(old, mem::ps()) {
        last_error(998);
        return 0;
    }
    mem::wp(old, FS_REDIR_OFF.with(|r| r.replace(1)));
    1
}
/// Wow64RevertWow64FsRedirection(old)
fn Wow64RevertWow64FsRedirection(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return not_wow64();
    }
    FS_REDIR_OFF.with(|r| r.set(c.a(0)));
    1
}
/// Wow64EnableWow64FsRedirection(enable): BOOLEAN
fn Wow64EnableWow64FsRedirection(c: &mut Ctx) -> u32 {
    if mem::x64() {
        return not_wow64();
    }
    FS_REDIR_OFF.with(|r| r.set((c.a(0) & 0xff == 0) as u32));
    1
}

// ---------------------------------------------------------------------------------------------
// version, locale

/// GetVersion: build << 16 | minor << 8 | major (Windows 7: 0x1db10106)
fn GetVersion(_: &mut Ctx) -> u32 {
    let v = os_version();
    v.build << 16 | v.minor << 8 | v.major
}
/// The Windows version programs see: GetVersionEx's (Windows 7 SP1, what Windows 10 tells a
/// program without a manifest for 8.1+... and what games of that era expect), or the real one
/// (RtlGetVersion: 10.0.19045).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OsVer {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    pub platform: u32,
    pub sp_major: u16,
    pub sp_minor: u16,
    pub suite: u16,
    pub product: u8,
}
pub const OS_VERSION: OsVer = OsVer { major: 6, minor: 1, build: 7601, platform: 2, sp_major: 1, sp_minor: 0, suite: 0x100, product: 1 };
pub const OS_VERSION_REAL: OsVer = OsVer { major: 10, minor: 0, build: 19045, ..OS_VERSION };

/// The version GetVersionEx and VerifyVersionInfo answer with: Windows 10's to a program whose
/// manifest declares it supports Windows 10 (supportedOS {8e0f7a12-...}; Ren'Py's launcher
/// refuses to start otherwise), Windows 7's to the others, as Windows 10 does
pub fn os_version() -> OsVer {
    static WIN10: OnceLock<bool> = OnceLock::new();
    let exe = module_handle(0, false);
    let win10 = |exe: u32| {
        let Some(e) = crate::pe::find_resource(exe, &crate::pe::ResId::Int(24), &crate::pe::ResId::Int(1)) else { return false };
        let (p, n) = crate::pe::resource_data(exe, e);
        String::from_utf8_lossy(mem::slice(p, n.min(0x10000))).to_ascii_lowercase().contains("8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a")
    };
    // (decided once the program is loaded)
    let w = if exe == 0 { false } else { *WIN10.get_or_init(|| win10(exe)) };
    if w { OS_VERSION_REAL } else { OS_VERSION }
}

/// OSVERSIONINFO(EX): only its two sizes (A 148/156, W 276/284); another is
/// ERROR_INSUFFICIENT_BUFFER and nothing written (W from 300 bytes on: no error set, as Windows)
fn version_info(p: u32, wide: bool) -> u32 {
    let v = os_version();
    let size = r32(p);
    let (base, ext) = if wide { (276, 284) } else { (148, 156) };
    if size != base && size != ext {
        // Windows 11 (winref sizes) leaves the error alone for W at exactly 300 bytes
        if !(wide && size == 300) {
            last_error(122);
        }
        return 0;
    }
    fill_version_info(p, wide, &v, size == ext);
    1
}
fn fill_version_info(p: u32, wide: bool, v: &OsVer, ext: bool) {
    w32(p + 4, v.major);
    w32(p + 8, v.minor);
    w32(p + 12, v.build);
    w32(p + 16, v.platform);
    if wide {
        put_wstr(p + 20, 128, &"Service Pack 1".encode_utf16().collect::<Vec<_>>());
    } else {
        put_cstr(p + 20, 128, b"Service Pack 1");
    }
    if ext {
        let o = if wide { 276 } else { 148 };
        w16(p + o, v.sp_major);
        w16(p + o + 2, v.sp_minor);
        w16(p + o + 4, v.suite);
        w8(p + o + 6, v.product);
    }
}
fn GetVersionExA(c: &mut Ctx) -> u32 {
    version_info(c.a(0), false)
}
fn GetVersionExW(c: &mut Ctx) -> u32 {
    version_info(c.a(0), true)
}
/// RtlGetVersion(RTL_OSVERSIONINFOW*): Artemis asks ntdll directly (GetVersionEx lies on 8.1+)
/// GetProductInfo(major, minor, spmajor, spminor, &type): Windows 10 Pro
/// DeviceIoControl: no devices (ASProtect looks it up and quits when it is missing).
fn DeviceIoControl(_: &mut Ctx) -> u32 {
    last_error(1); // ERROR_INVALID_FUNCTION
    0
}
fn GetProductInfo(c: &mut Ctx) -> u32 {
    put32(c.a(4), 0x30); // PRODUCT_PROFESSIONAL
    1
}
fn RtlGetVersion(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let size = r32(p);
    if size != 276 && size != 284 {
        return 0xc000_000d; // STATUS_INVALID_PARAMETER
    }
    fill_version_info(p, true, &os_version(), size == 284);
    // the real version, as Windows 10 gives it to programs without a manifest
    w32(p + 4, OS_VERSION_REAL.major);
    w32(p + 8, OS_VERSION_REAL.minor);
    w32(p + 12, OS_VERSION_REAL.build);
    put_wstr(p + 20, 128, &[]);
    0
}
fn RtlGetNtVersionNumbers(c: &mut Ctx) -> u32 {
    put32(c.a(0), 10);
    put32(c.a(1), 0);
    put32(c.a(2), 0xf000_0000 | 19045);
    0
}
// ---- VerifyVersionInfo (as Wine's kernel32 and ntdll, which its tests check against Windows)

pub const VER_EQUAL: u8 = 1;
pub const VER_LESS_EQUAL: u8 = 5;
pub const VER_AND: u8 = 6;
pub const VER_OR: u8 = 7;

#[derive(Debug, PartialEq, Eq)]
pub enum VerFail {
    /// ERROR_OLD_WIN_VERSION / STATUS_REVISION_MISMATCH
    Mismatch,
    /// ERROR_BAD_ARGUMENTS / STATUS_INVALID_PARAMETER
    BadArgs,
}

/// The condition for one of major, minor, service pack major and minor, which are compared as
/// one number: the first one sets the direction, later EQUALs follow it.
fn ver_update_condition(last: &mut u8, c: u8) -> u8 {
    match *last {
        0 => *last = c,
        1 => {
            if (VER_EQUAL..=VER_LESS_EQUAL).contains(&c) {
                *last = c;
                return c;
            }
        }
        2 | 3 => {
            if (1..=3).contains(&c) {
                return c;
            }
        }
        4 | 5 => {
            if c == 1 || (4..=5).contains(&c) {
                return c;
            }
        }
        _ => {}
    }
    if c == 0 {
        *last |= 0x10;
    }
    *last & 0xf
}

fn ver_compare(left: u32, right: u32, c: u8) -> bool {
    match c {
        1 => left == right,
        2 => left > right,
        3 => left >= right,
        4 => left < right,
        5 => left <= right,
        _ => false,
    }
}

/// Whether the running version `cur` meets `want` for the fields in `types` (VER_MINORVERSION
/// 1 ... VER_PRODUCT_TYPE 0x80) under the conditions `cond` (3 bits per field,
/// VerSetConditionMask).
pub fn verify_version(cur: &OsVer, want: &OsVer, types: u32, cond: u64) -> Result<(), VerFail> {
    if types == 0 || cond == 0 {
        return Err(VerFail::BadArgs);
    }
    let c = |k: u32| (cond >> (3 * k) & 7) as u8;
    let check = |ok: bool| if ok { Ok(()) } else { Err(VerFail::Mismatch) };
    if types & 0x80 != 0 {
        check(ver_compare(cur.product as u32, want.product as u32, c(7)))?;
    }
    if types & 0x40 != 0 {
        match c(6) {
            VER_AND => check(want.suite & cur.suite == want.suite)?,
            VER_OR => check(want.suite & cur.suite != 0 || want.suite == 0)?,
            _ => return Err(VerFail::BadArgs),
        }
    }
    if types & 8 != 0 {
        check(ver_compare(cur.platform, want.platform, c(3)))?;
    }
    if types & 4 != 0 {
        check(ver_compare(cur.build, want.build, c(2)))?;
    }
    if types & (1 | 2 | 0x10 | 0x20) != 0 {
        let mut last = 0;
        let (mut ok, mut next) = (true, true);
        // (type bit, condition slot, current, wanted) from the most significant
        let parts = [
            (2, 1, cur.major, want.major),
            (1, 0, cur.minor, want.minor),
            (0x20, 5, cur.sp_major as u32, want.sp_major as u32),
            (0x10, 4, cur.sp_minor as u32, want.sp_minor as u32),
        ];
        for (bit, slot, l, r) in parts {
            if types & bit != 0 && next {
                let cnd = ver_update_condition(&mut last, c(slot));
                ok = ver_compare(l, r, cnd);
                next = l == r && (VER_EQUAL..=VER_LESS_EQUAL).contains(&cnd);
            }
        }
        check(ok)?;
    }
    Ok(())
}

/// VerSetConditionMask(mask, type, condition): the condition goes to the slot of the type's
/// highest bit among those it checks first (product type, suite, service pack, ...).
pub fn ver_set_condition_mask(mask: u64, types: u32, cond: u8) -> u64 {
    let c = (cond & 7) as u64;
    let slot = [(0x80, 7), (0x40, 6), (0x20, 5), (0x10, 4), (8, 3), (4, 2), (2, 1), (1, 0)].into_iter().find(|&(b, _)| types & b != 0);
    slot.map_or(mask, |(_, k)| mask | c << (3 * k))
}

/// OSVERSIONINFOEX(A/W) at `p`: the fields compared
fn read_osver(p: u32, wide: bool) -> OsVer {
    let o = if wide { 276 } else { 148 };
    OsVer {
        major: r32(p + 4),
        minor: r32(p + 8),
        build: r32(p + 12),
        platform: r32(p + 16),
        sp_major: r16(p + o),
        sp_minor: r16(p + o + 2),
        suite: r16(p + o + 4),
        product: crate::mem::r8(p + o + 6),
    }
}
/// The condition mask argument (a 64-bit value: two stack slots on x86, after two 32-bit ones)
fn cond_mask_arg(c: &Ctx) -> u64 {
    if mem::x64() { c.a64(2) } else { c.a(2) as u64 | (c.a(3) as u64) << 32 }
}
fn verify_version_info(c: &mut Ctx, wide: bool) -> u32 {
    match verify_version(&os_version(), &read_osver(c.a(0), wide), c.a(1), cond_mask_arg(c)) {
        Ok(()) => 1,
        Err(e) => {
            last_error(if e == VerFail::BadArgs { 160 } else { 1150 });
            0
        }
    }
}
fn VerifyVersionInfoW(c: &mut Ctx) -> u32 {
    verify_version_info(c, true)
}
fn VerifyVersionInfoA(c: &mut Ctx) -> u32 {
    verify_version_info(c, false)
}
/// (against the real version; libcurl takes a missing one for Windows 9x and loads
/// security.dll)
fn RtlVerifyVersionInfo(c: &mut Ctx) -> u32 {
    match verify_version(&OS_VERSION_REAL, &read_osver(c.a(0), true), c.a(1), cond_mask_arg(c)) {
        Ok(()) => 0,
        Err(VerFail::Mismatch) => 0xc000_0059, // STATUS_REVISION_MISMATCH
        Err(VerFail::BadArgs) => 0xc000_000d, // STATUS_INVALID_PARAMETER
    }
}
fn VerSetConditionMask(c: &mut Ctx) -> u32 {
    let (m, t, k) = if mem::x64() { (c.a64(0), c.a(1), c.a(2)) } else { (c.a(0) as u64 | (c.a(1) as u64) << 32, c.a(2), c.a(3)) };
    let v = ver_set_condition_mask(m, t, k as u8);
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
/// Get{Thread,User,Process,System}PreferredUILanguages(flags, count, buffer, size): the one UI
/// language, as an id (MUI_LANGUAGE_ID 4: "0411") or a name ("ja-JP"), in a double-NUL list.
/// (Delphi's startup calls GetThreadPreferredUILanguages on Vista+ without checking it exists.)
/// The process list is empty unless SetProcessPreferredUILanguages set one: only the double NUL,
/// and the count left as it was (winref strapis)
fn preferred_ui_languages(c: &mut Ctx) -> u32 {
    ui_languages(c, false)
}
fn process_ui_languages(c: &mut Ctx) -> u32 {
    ui_languages(c, true)
}
fn ui_languages(c: &mut Ctx, process: bool) -> u32 {
    let l = lcid();
    let s = if process {
        String::new()
    } else if c.a(0) & 4 != 0 {
        format!("{l:04X}")
    } else {
        LOCALES.iter().find(|x| x.0 == l).map_or("en-US", |x| x.1).to_string()
    };
    let w: Vec<u16> = if process { vec![0, 0] } else { s.encode_utf16().chain([0, 0]).collect() };
    if !process {
        w32(c.a(1), 1);
    }
    let (buf, cap) = (c.a(2), r32(c.a(3)));
    w32(c.a(3), w.len() as u32);
    if buf == 0 {
        return 1;
    }
    if (cap as usize) < w.len() {
        last_error(122);
        return 0;
    }
    for (i, &u) in w.iter().enumerate() {
        w16(buf + 2 * i as u32, u);
    }
    1
}
/// VerLanguageName(lang, buf, chars): the language's name as a Japanese Windows writes it
/// (神楽黎明記 used the unwritten buffer)
fn ver_language_name(c: &mut Ctx, wide: bool) -> u32 {
    let name = match c.a(0) & 0x3ff {
        0x11 => "日本語",
        0x09 => "英語 (米国)",
        0x04 => "中国語 (簡体字、中国)",
        0x12 => "韓国語",
        _ => "言語ニュートラル",
    };
    // (winref strapis: a short buffer gets what fits and a NUL, and the count copied; no buffer
    // gives 0, not the length)
    let (buf, cap) = (c.a(1), c.a(2));
    if buf == 0 || cap == 0 {
        return 0;
    }
    if wide {
        mem::put_wstr(buf, cap, &name.encode_utf16().collect::<Vec<_>>())
    } else {
        mem::put_cstr(buf, cap, &crate::text::s2a(name))
    }
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
fn EnumCalendarInfoW(_: &mut Ctx) -> u32 {
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
    ok(crate::text::is_lead_cp(c.a(0), c.a(1) as u8))
}

fn MultiByteToWideChar(c: &mut Ctx) -> u32 {
    let (cp, flags, src, n, dst, cap) = (c.a(0), c.a(1), c.a(2), c.i(3), c.a(4), c.a(5));
    // (Windows: any negative length means NUL-terminated; no output buffer with a size is
    // ERROR_INVALID_PARAMETER)
    if src == 0 || n == 0 || cap > i32::MAX as u32 || src == dst || cap != 0 && dst == 0 {
        last_error(87);
        return 0;
    }
    let s = if n < 0 { let mut v = cstr(src); v.push(0); v } else { mem::slice(src, n as u32).to_vec() };
    let (w, bad) = crate::text::mb2w_checked(cp, &s);
    // MB_ERR_INVALID_CHARS: bytes that are no character (a lone lead byte) fail with
    // ERROR_NO_UNICODE_TRANSLATION (mingw's btowc relies on it, winref crt_io)
    if flags & 8 != 0 && bad {
        last_error(1113);
        return 0;
    }
    if cap == 0 {
        return w.len() as u32;
    }
    // Conversion can fill the supplied buffer before discovering that the
    // terminating NUL does not fit. MFC's ANSI CString -> CStringW conversion
    // requests exactly the non-NUL length, ignores ERROR_INSUFFICIENT_BUFFER,
    // and supplies its own terminator in ReleaseBuffer. Leaving the entire
    // buffer untouched here silently turned its Automation strings into empty
    // BSTRs. Preserve the failure result while retaining the converted prefix.
    let mut written = w.len().min(cap as usize);
    if written < w.len() && written > 0 && (0xd800..=0xdbff).contains(&w[written - 1]) && (0xdc00..=0xdfff).contains(&w[written]) {
        written -= 1;
    }
    for (i, ch) in w[..written].iter().enumerate() {
        w16(dst + 2 * i as u32, *ch);
    }
    if w.len() as u32 > cap {
        last_error(122);
        return 0;
    }
    w.len() as u32
}
fn WideCharToMultiByte(c: &mut Ctx) -> u32 {
    let (cp, src, n, dst, cap) = (c.a(0), c.a(2), c.i(3), c.a(4), c.a(5));
    // (Windows: any negative length means NUL-terminated; no output buffer with a size is
    // ERROR_INVALID_PARAMETER)
    if src == 0 || n == 0 || cap > i32::MAX as u32 || src == dst || cap != 0 && dst == 0 {
        last_error(87);
        return 0;
    }
    let s: Vec<u16> = if n < 0 {
        let mut v = wstr(src);
        v.push(0);
        v
    } else {
        (0..n as u32).map(|i| r16(src + 2 * i)).collect()
    };
    // (lpDefaultChar / lpUsedDefaultChar with UTF-7/8: documented as ERROR_INVALID_PARAMETER,
    // but Windows Server 2025 converts and leaves the flag alone, winref crt_io)
    let (def, pused) = (c.a(6), c.a(7));
    let utf = matches!(cp, 65000 | 65001);
    let (def, pused) = if utf { (0, 0) } else { (def, pused) };
    let d = if def != 0 { vec![mem::r8(def)] } else { b"?".to_vec() };
    // (WC_NO_BEST_FIT_CHARS: a character the code page lacks is the default one, never a
    // look-alike)
    let (a, used) = crate::text::w2mb_flags(cp, &s, &d, c.a(1) & 0x400 == 0);
    put32(pused, used as u32);
    if cap == 0 {
        return a.len() as u32;
    }
    if a.len() as u32 > cap {
        // (as MultiByteToWideChar: the whole characters that fit are written before the failure)
        let utf8 = cp == 65001 || matches!(cp, 0 | 1 | 3) && crate::text::acp() == 65001;
        let mut fit = 0;
        let mut at = 0;
        while at < a.len() {
            let n = if utf8 { 1 + a[at + 1..].iter().take_while(|b| **b & 0xc0 == 0x80).count() }
                else if crate::text::is_lead_cp(cp, a[at]) { 2 } else { 1 };
            if at + n > cap as usize { break; }
            at += n;
            fit = at;
        }
        mem::write(dst, &a[..fit]);
        last_error(122);
        return 0;
    }
    mem::write(dst, &a);
    a.len() as u32
}

/// CT_CTYPE1, with C1_DEFINED (0x200) on every character but the surrogates
pub(crate) fn ctype1(ch: u16) -> u16 {
    let c = ch as u32;
    let mut t = 0u16;
    if let Some(x) = char::from_u32(c) {
        t |= 0x200;
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

/// CT_CTYPE3: C3_FULLWIDTH only for the East Asian "fullwidth" forms (U+3000, U+FF01..FF60,
/// U+FFE0..FFE6), C3_HALFWIDTH for ASCII and the half-width forms; kana and ideographs are
/// neither (Windows Server 2025, winref nls: あ 0x8020, 一 0x8100), C3_HIRAGANA, C3_KATAKANA,
/// C3_IDEOGRAPH. The space is a symbol.
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
    let symbol = if t1 & 0x10 != 0 || c == 0x20 || c == 0x3000 { SYMBOL } else { 0 };
    match c {
        0x3099..=0x309a => NONSPACING | DIACRITIC | HIRAGANA,
        0x309b..=0x309c => DIACRITIC | SYMBOL,
        0x3041..=0x309f => HIRAGANA | ALPHA,
        0x30fb => SYMBOL,
        0x30a0..=0x30ff | 0x31f0..=0x31ff => KATAKANA | ALPHA,
        0x3005..=0x3007 | 0x3021..=0x3029 | 0x3400..=0x4dbf | 0x4e00..=0x9fff | 0xf900..=0xfaff => IDEOGRAPH | ALPHA,
        0xd800..=0xdfff => 0,
        0x3000 => SYMBOL | FULLWIDTH,
        0x3001..=0x303f | 0x3200..=0x33ff | 0xfe30..=0xfe4f => symbol,
        0xff61..=0xff65 => SYMBOL | HALFWIDTH,
        0xff9e..=0xff9f => DIACRITIC | SYMBOL | KATAKANA | HALFWIDTH,
        0xff66..=0xff9d => KATAKANA | HALFWIDTH | ALPHA,
        0xffa0..=0xffdc | 0xffe8..=0xffee => alpha | symbol | HALFWIDTH,
        0xff01..=0xff60 | 0xffe0..=0xffe6 => alpha | symbol | FULLWIDTH,
        0x1100..=0x115f | 0xac00..=0xd7a3 | 0x3130..=0x318f => ALPHA,
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
/// LCMapString's case, kana and width maps (no sort keys)
fn lcmap(w: &[u16], flags: u32) -> Vec<u16> {
    let mut w = if flags & 0xf00000 != 0 { crate::text::map_kana_width(w, flags) } else { w.to_vec() };
    for ch in w.iter_mut() {
        if flags & 0x100 != 0 {
            *ch = crate::text::lower16(*ch);
        }
        if flags & 0x200 != 0 {
            *ch = crate::text::upper16(*ch);
        }
    }
    w
}
/// LCMapStringA: through Unicode in the ANSI code page (CharUpperA's rules for case)
fn LCMapStringA(c: &mut Ctx) -> u32 {
    let (flags, src, n, dst, cap) = (c.a(1), c.a(2), c.i(3), c.a(4), c.a(5));
    let mut s = if n < 0 { let mut v = cstr(src); v.push(0); v } else { mem::slice(src, n as u32).to_vec() };
    if flags & 0xf00000 != 0 {
        s = crate::text::w2a(&lcmap(&a2w(&s), flags));
    } else if flags & 0x300 != 0 {
        crate::text::case_a(&mut s, flags & 0x200 != 0);
    }
    if cap == 0 {
        return s.len() as u32;
    }
    let k = s.len().min(cap as usize);
    mem::write(dst, &s[..k]);
    k as u32
}
pub(crate) fn LCMapStringW(c: &mut Ctx) -> u32 {
    let (flags, src, n, dst, cap) = (c.a(1), c.a(2), c.i(3), c.a(4), c.a(5));
    let w: Vec<u16> = if n < 0 { let mut v = wstr(src); v.push(0); v } else { (0..n as u32).map(|i| r16(src + 2 * i)).collect() };
    let w = lcmap(&w, flags);
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
    crate::collate::compare(a, b, if nocase { crate::collate::NORM_IGNORECASE } else { 0 })
}
fn CompareStringA(c: &mut Ctx) -> u32 {
    let (flags, s1, n1, s2, n2) = (c.a(1), c.a(2), c.i(3), c.a(4), c.i(5));
    let a = if n1 < 0 { cstr(s1) } else { mem::cstrn(s1, n1 as u32) };
    let b = if n2 < 0 { cstr(s2) } else { mem::cstrn(s2, n2 as u32) };
    crate::collate::compare(&a2w(&a), &a2w(&b), flags) as i32 as u32 + 2
}
/// CompareStringOrdinal(s1, n1, s2, n2, ignore case): code unit order (with the case
/// ignored, of the upper case letters), CSTR_LESS_THAN 1 / EQUAL 2 / GREATER_THAN 3.
fn CompareStringOrdinal(c: &mut Ctx) -> u32 {
    let (s1, n1, s2, n2, nocase) = (c.a(0), c.i(1), c.a(2), c.i(3), c.a(4) != 0);
    if s1 == 0 || s2 == 0 {
        last_error(87);
        return 0;
    }
    let rd = |p: u32, n: i32| -> Vec<u16> { if n < 0 { wstr(p) } else { (0..n as u32).map(|i| r16(p + 2 * i)).collect() } };
    let up = |v: Vec<u16>| -> Vec<u16> {
        if !nocase {
            return v;
        }
        v.into_iter().map(|u| char::from_u32(u as u32).filter(|ch| !ch.is_ascii() || ch.is_ascii_lowercase()).and_then(|ch| {
            let mut it = ch.to_uppercase();
            match (it.next(), it.next()) {
                (Some(x), None) if (x as u32) < 0x10000 => Some(x as u16),
                _ => None,
            }
        }).unwrap_or(u)).collect()
    };
    up(rd(s1, n1)).cmp(&up(rd(s2, n2))) as i32 as u32 + 2
}
fn CompareStringW(c: &mut Ctx) -> u32 {
    let (flags, s1, n1, s2, n2) = (c.a(1), c.a(2), c.i(3), c.a(4), c.i(5));
    let rd = |p: u32, n: i32| -> Vec<u16> { if n < 0 { wstr(p) } else { (0..n as u32).map(|i| r16(p + 2 * i)).take_while(|&x| x != 0).collect() } };
    crate::collate::compare(&rd(s1, n1), &rd(s2, n2), flags) as i32 as u32 + 2
}
/// A date/time picture (GetDateFormat/GetTimeFormat) into text: fields are runs of one letter,
/// '...' is literal text ('' a quote). `skip` drops fields of those letters with the literal
/// text before them (TIME_NOSECONDS and the like).
fn format_picture(pic: &str, st: [u16; 8], ja: bool, skip: &str, h24: bool) -> String {
    let (y, mo, dow, d, h, mi, sec) = (st[0] as u32, st[1] as u32, st[2] as usize % 7, st[3] as u32, st[4] as u32, st[5] as u32, st[6] as u32);
    enum Item {
        Lit(String),
        Field(char, usize),
    }
    let mut items = Vec::new();
    let cs: Vec<char> = pic.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let ch = cs[i];
        if ch == '\'' {
            let mut lit = String::new();
            i += 1;
            while i < cs.len() {
                if cs[i] == '\'' {
                    if cs.get(i + 1) == Some(&'\'') {
                        lit.push('\'');
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                lit.push(cs[i]);
                i += 1;
            }
            items.push(Item::Lit(lit));
        } else if "yMdgHhmst".contains(ch) {
            let n = cs[i..].iter().take_while(|&&x| x == ch).count();
            items.push(Item::Field(ch, n));
            i += n;
        } else {
            items.push(Item::Lit(ch.to_string()));
            i += 1;
        }
    }
    // skipped fields take the literal text before them along (after them when first)
    let mut keep = vec![true; items.len()];
    for k in 0..items.len() {
        if let Item::Field(ch, _) = items[k] {
            let ch = if h24 && ch == 'h' { 'H' } else { ch };
            if skip.contains(ch) {
                keep[k] = false;
                let mut j = k;
                while j > 0 && matches!(items[j - 1], Item::Lit(_)) {
                    j -= 1;
                    keep[j] = false;
                }
                if j == 0 {
                    let mut j = k + 1;
                    while j < items.len() && matches!(items[j], Item::Lit(_)) {
                        keep[j] = false;
                        j += 1;
                    }
                }
            }
        }
    }
    const JA_DAYS: [&str; 7] = ["日", "月", "火", "水", "木", "金", "土"];
    const EN_DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
    const EN_MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    let mut out = String::new();
    for (k, it) in items.iter().enumerate() {
        if !keep[k] {
            continue;
        }
        match *it {
            Item::Lit(ref l) => out.push_str(l),
            Item::Field(ch, n) => {
                let ch = if h24 && ch == 'h' { 'H' } else { ch };
                let num = |v: u32| if n >= 2 { format!("{v:02}") } else { v.to_string() };
                let am = h < 12;
                let s = match ch {
                    'd' if n >= 4 => if ja { format!("{}曜日", JA_DAYS[dow]) } else { EN_DAYS[dow].into() },
                    'd' if n == 3 => if ja { JA_DAYS[dow].into() } else { EN_DAYS[dow][..3].into() },
                    'd' => num(d),
                    'M' if n >= 4 => if ja { format!("{mo}月") } else { EN_MONTHS[(mo as usize + 11) % 12].into() },
                    'M' if n == 3 => if ja { mo.to_string() } else { EN_MONTHS[(mo as usize + 11) % 12][..3].into() },
                    'M' => num(mo),
                    'y' if n == 1 => (y % 100).to_string(),
                    'y' if n == 2 => format!("{:02}", y % 100),
                    'y' => y.to_string(),
                    'g' => if ja { "西暦".into() } else { "A.D.".into() },
                    'H' => num(h),
                    'h' => num(if h % 12 == 0 { 12 } else { h % 12 }),
                    'm' => num(mi),
                    's' => num(sec),
                    _ => {
                        let m = if ja { if am { "午前" } else { "午後" } } else if am { "AM" } else { "PM" };
                        if n == 1 { m.chars().next().unwrap().to_string() } else { m.into() }
                    }
                };
                out.push_str(&s);
            }
        }
    }
    out
}
/// The SYSTEMTIME at `p` (the local time when 0), None when `date`/time fields are invalid.
fn read_systemtime(p: u32, date: bool) -> Option<[u16; 8]> {
    let tmp = (p == 0).then(|| mem::heap_alloc(16, true));
    if let Some(t) = tmp {
        system_time(t, true);
    }
    let q = tmp.unwrap_or(p);
    let st: [u16; 8] = std::array::from_fn(|i| r16(q + 2 * i as u32));
    if let Some(t) = tmp {
        mem::heap_free(t);
    }
    let (y, m, d) = (st[0] as i64, st[1] as i64, st[3] as i64);
    let good = if date { (1601..=30827).contains(&y) && (1..=12).contains(&m) && d >= 1 && d <= days_in_month(y, m) } else { st[4] < 24 && st[5] < 60 && st[6] < 60 && st[7] < 1000 };
    let mut st = st;
    if date {
        st[2] = (days_from_civil(y, m, d) + 4).rem_euclid(7) as u16;
    }
    good.then_some(st)
}
/// GetDateFormat(A/W) and GetTimeFormat(A/W): (lcid, flags, time, picture, out, cap)
fn date_time_format(c: &mut Ctx, date: bool, wide: bool) -> u32 {
    let (lcid, flags, t, pic, out, cap) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let Some(st) = read_systemtime(t, date) else {
        last_error(87);
        return 0;
    };
    let ja = lcid & 0x3ff == 0x11 || matches!(lcid, 0 | 0x400 | 0x800) && crate::text::acp() == 932;
    let picture = if pic != 0 {
        arg_str(pic, wide)
    } else if date {
        let long = flags & 2 != 0;
        match (ja, long) {
            (true, false) => "yyyy/MM/dd",
            (true, true) => "yyyy'年'M'月'd'日'",
            (false, false) => "M/d/yyyy",
            (false, true) => "dddd, MMMM d, yyyy",
        }
        .into()
    } else if ja {
        "H:mm:ss".into()
    } else {
        "h:mm:ss tt".into()
    };
    let mut skip = String::new();
    if !date {
        if flags & 1 != 0 {
            skip.push_str("ms");
        }
        if flags & 2 != 0 {
            skip.push('s');
        }
        if flags & 4 != 0 {
            skip.push('t');
        }
    }
    let text = format_picture(&picture, st, ja, &skip, !date && flags & 8 != 0);
    let n = if wide { text.encode_utf16().count() } else { crate::text::s2a(&text).len() } as u32 + 1;
    if cap == 0 {
        return n;
    }
    if n > cap {
        last_error(122);
        return 0;
    }
    ret_str(out, cap, &text, wide);
    n
}
fn GetDateFormatA(c: &mut Ctx) -> u32 {
    date_time_format(c, true, false)
}
fn GetDateFormatW(c: &mut Ctx) -> u32 {
    date_time_format(c, true, true)
}
fn GetTimeFormatA(c: &mut Ctx) -> u32 {
    date_time_format(c, false, false)
}
fn GetTimeFormatW(c: &mut Ctx) -> u32 {
    date_time_format(c, false, true)
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
/// An atom table: string atoms 0xc000 + index, each with its reference count (integer atoms,
/// 1..=0xbfff, are their own value and are not stored).
struct Atoms(Vec<Option<(String, u32)>>);
/// GlobalAddAtom's and AddAtom's tables.
static GLOBAL_ATOMS: Mutex<Atoms> = Mutex::new(Atoms(Vec::new()));
static LOCAL_ATOMS: Mutex<Atoms> = Mutex::new(Atoms(Vec::new()));

/// What an atom function was given: an integer atom (MAKEINTATOM, or "#1234"), a name, or the
/// Win32 error (0: none, as for MAKEINTATOM(0))
#[derive(Debug, PartialEq)]
enum AtomArg {
    Int(u32),
    Name(String),
    Bad(u32),
}
fn atom_arg(p: u32, wide: bool) -> AtomArg {
    if p < 0x10000 {
        return match p {
            0 => AtomArg::Bad(0),
            1..=0xbfff => AtomArg::Int(p),
            _ => AtomArg::Bad(87),
        };
    }
    atom_text(arg_str(p, wide))
}
fn atom_text(s: String) -> AtomArg {
    if let Some(d) = s.strip_prefix('#').filter(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())) {
        return match d.parse::<u32>() {
            Ok(n @ 1..=0xbfff) => AtomArg::Int(n),
            _ => AtomArg::Bad(87),
        };
    }
    // (at most 255 characters)
    if s.encode_utf16().count() > 255 {
        return AtomArg::Bad(87);
    }
    AtomArg::Name(s)
}
impl Atoms {
    fn find(&self, s: &str) -> Option<usize> {
        let low = s.to_lowercase();
        self.0.iter().position(|e| e.as_ref().is_some_and(|(n, _)| n.to_lowercase() == low))
    }
    fn add(&mut self, s: String) -> u32 {
        if let Some(i) = self.find(&s) {
            self.0[i].as_mut().unwrap().1 += 1;
            return 0xc000 + i as u32;
        }
        let i = self.0.iter().position(|e| e.is_none()).unwrap_or(self.0.len());
        if i >= 0x4000 {
            return 0;
        }
        if i == self.0.len() {
            self.0.push(None);
        }
        self.0[i] = Some((s, 1));
        0xc000 + i as u32
    }
    /// 0 once deleted (or an integer atom), else the atom (not one)
    fn delete(&mut self, a: u32) -> u32 {
        if (1..0xc000).contains(&a) {
            return 0;
        }
        let Some(e) = a.checked_sub(0xc000).and_then(|i| self.0.get_mut(i as usize)) else { return a };
        match e {
            Some((_, n)) if *n > 1 => *n -= 1,
            Some(_) => *e = None,
            None => return a,
        }
        0
    }
    fn name(&self, a: u32) -> Option<String> {
        match a {
            1..=0xbfff => Some(format!("#{a}")),
            _ => self.0.get(a.checked_sub(0xc000)? as usize)?.as_ref().map(|(n, _)| n.clone()),
        }
    }
}
fn atoms(global: bool) -> std::sync::MutexGuard<'static, Atoms> {
    if global { GLOBAL_ATOMS.lock().unwrap() } else { LOCAL_ATOMS.lock().unwrap() }
}
fn add_atom(c: &mut Ctx, wide: bool, global: bool) -> u32 {
    match atom_arg(c.a(0), wide) {
        AtomArg::Int(n) => n,
        AtomArg::Name(s) => match atoms(global).add(s) {
            0 => {
                last_error(8);
                0
            }
            a => a,
        },
        AtomArg::Bad(e) => {
            if e != 0 {
                last_error(e);
            }
            0
        }
    }
}
fn find_atom(c: &mut Ctx, wide: bool, global: bool) -> u32 {
    match atom_arg(c.a(0), wide) {
        AtomArg::Int(n) => n,
        AtomArg::Name(s) => match atoms(global).find(&s) {
            Some(i) => 0xc000 + i as u32,
            None => {
                last_error(2);
                0
            }
        },
        AtomArg::Bad(e) => {
            last_error(if e != 0 { e } else { 87 });
            0
        }
    }
}
/// The name of atom `a` into `buf` (`size` characters), as Windows' Get(Global)AtomName: a local
/// atom's name is cut to size - 1 characters and a NUL (the count without it; none fitting:
/// ERROR_INSUFFICIENT_BUFFER); GlobalGetAtomNameA cuts the same way but answers 0 with
/// ERROR_MORE_DATA, GlobalGetAtomNameW copies up to `size` characters and adds the NUL only where
/// it fits.
fn atom_name_into(a: u32, buf: u32, size: u32, wide: bool, global: bool) -> u32 {
    if a == 0 {
        last_error(87);
        return 0;
    }
    let Some(name) = atoms(global).name(a) else {
        last_error(6);
        return 0;
    };
    if size == 0 {
        last_error(if global { 234 } else { 122 });
        return 0;
    }
    let text: Vec<u16> = if wide { name.encode_utf16().collect() } else { crate::text::s2a(&name).into_iter().map(u16::from).collect() };
    let put = |n: usize, nul: bool| {
        for (i, &ch) in text[..n].iter().enumerate() {
            if wide { mem::w16(buf + 2 * i as u32, ch) } else { w8(buf + i as u32, ch as u8) }
        }
        if nul {
            if wide { mem::w16(buf + 2 * n as u32, 0) } else { w8(buf + n as u32, 0) }
        }
    };
    let len = text.len();
    if global && wide {
        let n = len.min(size as usize);
        put(n, n < size as usize);
        return n as u32;
    }
    let n = len.min(size as usize - 1);
    put(n, true);
    if global && n < len {
        last_error(234);
        return 0;
    }
    if n == 0 && len != 0 {
        last_error(122);
    }
    n as u32
}
/// The name of a global string atom (a window class's, a registered message's...).
pub fn atom_name(a: u32) -> Option<String> {
    if a < 0xc000 {
        return None;
    }
    atoms(true).name(a)
}
/// A global atom for `s` ("#n": the integer atom n).
pub fn atom(s: &str) -> u32 {
    match atom_text(s.to_string()) {
        AtomArg::Int(n) => n,
        AtomArg::Name(s) => atoms(true).add(s),
        AtomArg::Bad(_) => 0,
    }
}
fn GlobalAddAtomA(c: &mut Ctx) -> u32 {
    add_atom(c, false, true)
}
fn GlobalAddAtomW(c: &mut Ctx) -> u32 {
    add_atom(c, true, true)
}
fn AddAtomA(c: &mut Ctx) -> u32 {
    add_atom(c, false, false)
}
fn AddAtomW(c: &mut Ctx) -> u32 {
    add_atom(c, true, false)
}
fn GlobalFindAtomA(c: &mut Ctx) -> u32 {
    find_atom(c, false, true)
}
fn GlobalFindAtomW(c: &mut Ctx) -> u32 {
    find_atom(c, true, true)
}
fn FindAtomA(c: &mut Ctx) -> u32 {
    find_atom(c, false, false)
}
fn FindAtomW(c: &mut Ctx) -> u32 {
    find_atom(c, true, false)
}
fn GlobalDeleteAtom(c: &mut Ctx) -> u32 {
    atoms(true).delete(c.a(0) & 0xffff)
}
fn DeleteAtom(c: &mut Ctx) -> u32 {
    atoms(false).delete(c.a(0) & 0xffff)
}
fn GlobalGetAtomNameA(c: &mut Ctx) -> u32 {
    atom_name_into(c.a(0) & 0xffff, c.a(1), c.a(2), false, true)
}
fn GlobalGetAtomNameW(c: &mut Ctx) -> u32 {
    atom_name_into(c.a(0) & 0xffff, c.a(1), c.a(2), true, true)
}
fn GetAtomNameA(c: &mut Ctx) -> u32 {
    atom_name_into(c.a(0) & 0xffff, c.a(1), c.a(2), false, false)
}
fn GetAtomNameW(c: &mut Ctx) -> u32 {
    atom_name_into(c.a(0) & 0xffff, c.a(1), c.a(2), true, false)
}

// ---------------------------------------------------------------------------------------------
// time

fn system_time(p: u32, local: bool) {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs() as i64 + if local { tz_bias() } else { 0 };
    write_systemtime(p, secs, now.subsec_millis());
}
pub fn tz_bias() -> i64 {
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
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
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
    if ft > i64::MAX as u64 {
        last_error(87);
        return 0;
    }
    let secs = (ft / 10_000_000) as i64 - 11_644_473_600;
    write_systemtime(c.a(1), secs, ((ft / 10_000) % 1000) as u32);
    1
}
fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if y % 4 == 0 && (y % 100 != 0 || y % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}
/// A SYSTEMTIME's FILETIME (day of week ignored), None for fields Windows rejects.
fn systemtime_ft(p: u32) -> Option<u64> {
    let f = |o| r16(p + o) as i64;
    let (y, m, d, h, mi, s, ms) = (f(0), f(2), f(6), f(8), f(10), f(12), f(14));
    if !(1601..=30827).contains(&y) || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) || h > 23 || mi > 59 || s > 59 || ms > 999 {
        return None;
    }
    let secs = days_from_civil(y, m, d) * 86400 + h * 3600 + mi * 60 + s;
    Some(((secs + 11_644_473_600) as u64) * 10_000_000 + ms as u64 * 10_000)
}
fn SystemTimeToFileTime(c: &mut Ctx) -> u32 {
    let Some(ft) = systemtime_ft(c.a(0)) else {
        last_error(87);
        return 0;
    };
    put64(c.a(1), ft);
    1
}
/// The start of a TIME_ZONE_INFORMATION transition (StandardDate/DaylightDate at `p`) in `year`,
/// as local FILETIME: wDay is the week (5: the last) of wDayOfWeek in wMonth; a nonzero wYear is
/// an absolute date.
fn tz_transition(p: u32, year: i64) -> Option<i64> {
    let f = |o| r16(p + o) as i64;
    let (m, dow, wk, h, mi, s, ms) = (f(2), f(4), f(6), f(8), f(10), f(12), f(14));
    if m == 0 {
        return None;
    }
    let day = if f(0) != 0 {
        wk
    } else {
        let first = (days_from_civil(year, m, 1) + 4).rem_euclid(7);
        let mut d = 1 + (dow - first).rem_euclid(7) + (wk - 1) * 7;
        while d > days_in_month(year, m) {
            d -= 7;
        }
        d
    };
    Some(((days_from_civil(year, m, day) * 86400 + h * 3600 + mi * 60 + s + 11_644_473_600) * 1000 + ms) * 10_000)
}
/// The bias (minutes, UTC = local + bias) in effect at `t` (a FILETIME, UTC or local).
fn tz_bias_at(z: u32, t: i64, utc: bool) -> i64 {
    let bias = r32(z) as i32 as i64;
    let (std_b, dst_b) = (r32(z + 84) as i32 as i64, r32(z + 168) as i32 as i64);
    let st = mem::heap_alloc(16, true);
    write_systemtime(st, t.div_euclid(10_000_000) - 11_644_473_600, 0);
    let year = r16(st) as i64;
    mem::heap_free(st);
    let (Some(dst_start), Some(std_start)) = (tz_transition(z + 152, year), tz_transition(z + 68, year)) else {
        return bias + std_b;
    };
    // the transitions are in local time of the period before each
    let (ds, ss) = if utc { (dst_start + (bias + std_b) * 600_000_000, std_start + (bias + dst_b) * 600_000_000) } else { (dst_start, std_start) };
    let in_dst = if ds < ss { t >= ds && t < ss } else { !(t >= ss && t < ds) };
    bias + if in_dst { dst_b } else { std_b }
}
fn tz_info(c: &mut Ctx) -> u32 {
    let z = c.a(0);
    if z != 0 {
        return z;
    }
    let tmp = mem::heap_alloc(172, true);
    w32(tmp, (-tz_bias() / 60) as i32 as u32);
    tmp
}
fn SystemTimeToTzSpecificLocalTime(c: &mut Ctx) -> u32 {
    let Some(ft) = systemtime_ft(c.a(1)) else {
        last_error(87);
        return 0;
    };
    let z = tz_info(c);
    let t = ft as i64 - tz_bias_at(z, ft as i64, true) * 600_000_000;
    if c.a(0) == 0 {
        mem::heap_free(z);
    }
    if t < 0 {
        last_error(87);
        return 0;
    }
    write_systemtime(c.a(2), t.div_euclid(10_000_000) - 11_644_473_600, (t / 10_000 % 1000) as u32);
    1
}
fn TzSpecificLocalTimeToSystemTime(c: &mut Ctx) -> u32 {
    let Some(ft) = systemtime_ft(c.a(1)) else {
        last_error(87);
        return 0;
    };
    let z = tz_info(c);
    let t = ft as i64 + tz_bias_at(z, ft as i64, false) * 600_000_000;
    if c.a(0) == 0 {
        mem::heap_free(z);
    }
    if t < 0 {
        last_error(87);
        return 0;
    }
    write_systemtime(c.a(2), t.div_euclid(10_000_000) - 11_644_473_600, (t / 10_000 % 1000) as u32);
    1
}
fn DosDateTimeToFileTime(c: &mut Ctx) -> u32 {
    let (d, t) = (c.a(0) as i64 & 0xffff, c.a(1) as i64 & 0xffff);
    let (y, m, dd, h, mi, s) = (1980 + (d >> 9), (d >> 5) & 15, d & 31, t >> 11, (t >> 5) & 63, (t & 31) * 2);
    if !(1..=12).contains(&m) || dd < 1 || dd > days_in_month(y, m) || h > 23 || mi > 59 || s > 59 {
        last_error(87);
        return 0;
    }
    let secs = days_from_civil(y, m, dd) * 86400 + h * 3600 + mi * 60 + s;
    put64(c.a(2), ((secs + 11_644_473_600) as u64) * 10_000_000);
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
    if !(1980..=2107).contains(&y) || ft > i64::MAX as u64 {
        last_error(87);
        return 0;
    }
    if c.a(1) != 0 {
        w16(c.a(1), (y - 1980) << 9 | m << 5 | d);
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
pub(crate) fn qpc() -> u64 {
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
        // cut short: cap-1 characters and a 0, and cap (Windows Server 2025, winref fmt;
        // Windows XP left A without the 0)
        if wide {
            put_name(dst, cap, &path, true);
        } else {
            mem::put_cstr(dst, cap, &crate::text::s2a(&path));
        }
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
    mem::put_addr(c.a(2), h);
    ok(h != 0)
}
fn GetModuleHandleExA(c: &mut Ctx) -> u32 {
    let flags = c.a(0);
    let h = if flags & 4 != 0 { crate::pe::containing(c.a(1)).unwrap_or(0) } else { module_handle(c.a(1), false) };
    mem::put_addr(c.a(2), h);
    ok(h != 0)
}
/// LoadLibrary: "" is ERROR_INVALID_PARAMETER, a module not found ERROR_MOD_NOT_FOUND (winref fmt)
fn load_library_named(c: &mut Ctx, n: &str) -> u32 {
    if n.is_empty() {
        last_error(87);
        return 0;
    }
    let r = crate::pe::load_library(c.cpu, n);
    if r == 0 {
        last_error(126);
    }
    r
}
fn LoadLibraryA(c: &mut Ctx) -> u32 {
    let n = arg_str(c.a(0), false);
    load_library_named(c, &n)
}
fn LoadLibraryW(c: &mut Ctx) -> u32 {
    let n = arg_str(c.a(0), true);
    load_library_named(c, &n)
}
/// LoadLibraryEx(name, file, flags): LOAD_LIBRARY_AS_DATAFILE (2), its _EXCLUSIVE (0x40) and
/// LOAD_LIBRARY_AS_IMAGE_RESOURCE (0x20) map the file for its resources only (a module already
/// loaded as code answers unless exclusive); the handle is the base | 1 (| 2 for the image
/// resource)
fn load_library_ex(c: &mut Ctx, wide: bool) -> u32 {
    let (name, flags) = (c.a(0), c.a(2));
    if flags & 0x62 == 0 {
        return if wide { LoadLibraryW(c) } else { LoadLibraryA(c) };
    }
    let n = arg_str(name, wide);
    if n.is_empty() {
        last_error(87);
        return 0;
    }
    let full = files::full_path(&n);
    if flags & 0x40 == 0 {
        let loaded = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.name.is_empty() && m.path.eq_ignore_ascii_case(&full)).map(|m| m.base);
        if let Some(b) = loaded {
            return b;
        }
    }
    let Some(host) = files::resolve(&full).filter(|h| !files::is_dir(h)) else {
        last_error(2);
        return 0;
    };
    let Ok(data) = kura::fs::read(&host) else {
        last_error(5);
        return 0;
    };
    match crate::pe::load_datafile(&data, &full) {
        Some(b) => b | if flags & 0x20 != 0 && flags & 0x42 == 0 { 2 } else { 1 },
        None => {
            last_error(193);
            0
        }
    }
}
fn LoadLibraryExA(c: &mut Ctx) -> u32 {
    load_library_ex(c, false)
}
/// SetDllDirectory(path): one folder searched after the exe's (NULL or "": none again)
fn set_dll_directory(c: &mut Ctx, wide: bool) -> u32 {
    let mut dirs = crate::pe::DLL_DIRS.lock().unwrap();
    dirs.clear();
    if c.a(0) != 0 {
        let d = crate::files::full_path(arg_str(c.a(0), wide).trim_end_matches(['\\', '/']));
        if !d.is_empty() {
            dirs.push(d);
        }
    }
    1
}
fn SetDllDirectoryA(c: &mut Ctx) -> u32 {
    set_dll_directory(c, false)
}
fn SetDllDirectoryW(c: &mut Ctx) -> u32 {
    set_dll_directory(c, true)
}
fn LoadLibraryExW(c: &mut Ctx) -> u32 {
    load_library_ex(c, true)
}
/// (modules stay loaded; a handle that is no module is ERROR_MOD_NOT_FOUND, winref fmt)
fn FreeLibrary(c: &mut Ctx) -> u32 {
    // (a data-file module is its base | 1 or | 2)
    if crate::pe::with(c.a(0) & !3, |_| ()).is_none() {
        last_error(126);
        return 0;
    }
    if c.a(0) & 3 != 0 {
        crate::pe::free_datafile(c.a(0) & !3);
    }
    1
}
fn DisableThreadLibraryCalls(_: &mut Ctx) -> u32 {
    1
}
fn GetProcAddress(c: &mut Ctx) -> u32 {
    let (h, p) = (c.a(0), c.a(1));
    let imp = if p < 0x10000 { crate::pe::Import::Ordinal(p) } else { crate::pe::Import::Name(Box::leak(a2s(&cstr(p)).into_boxed_str())) };
    let mut r = crate::pe::proc_address(h, imp).unwrap_or(0);
    // SAKE_PROC_STUBS (tests/winetest): a system DLL's function we know but don't implement
    // is found, as on Windows, and its trap returns 0; programs (games) otherwise see NULL
    // and take the way they have for older Windows
    if r == 0 && p >= 0x10000 && crate::env_on!("SAKE_PROC_STUBS") {
        if let Some(dll) = crate::pe::with(h, |m| m.name.clone()).filter(|d| crate::api::is_system(d)) {
            let name = a2s(&cstr(p));
            let trap = crate::api::export(&dll, &name);
            if crate::api::is_known(trap) {
                r = crate::fast::bind(&dll, &name, trap);
            }
        }
    }
    if crate::env_on!("SAKE_TRACE_LOAD") {
        eprintln!("sake: GetProcAddress({h:#x}, {}) = {r:#x}", if p < 0x10000 { format!("#{p}") } else { a2s(&cstr(p)) });
    }
    if r == 0 {
        // (an ordinal not exported: ERROR_INVALID_ORDINAL, winref fmt)
        last_error(if p < 0x10000 && h != 0 { 182 } else { 127 });
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
fn find_res(m: u32, name: u32, ty: u32, lang: Option<u32>, wide: bool) -> u32 {
    let m = if m == 0 { module_handle(0, false) } else { m };
    let ty = crate::pe::ResId::from_arg(ty, wide);
    let name = crate::pe::ResId::from_arg(name, wide);
    match crate::pe::find_resource_lang(m, &ty, &name, lang) {
        Ok(e) => e,
        Err(e) => {
            last_error(e);
            0
        }
    }
}
fn FindResourceA(c: &mut Ctx) -> u32 {
    find_res(c.a(0), c.a(1), c.a(2), None, false)
}
fn FindResourceW(c: &mut Ctx) -> u32 {
    find_res(c.a(0), c.a(1), c.a(2), None, true)
}
/// FindResourceEx(module, type, name, language): that language only (LANG_NEUTRAL: any)
fn FindResourceExA(c: &mut Ctx) -> u32 {
    find_res(c.a(0), c.a(2), c.a(1), Some(c.a(3) & 0xffff), false)
}
fn FindResourceExW(c: &mut Ctx) -> u32 {
    find_res(c.a(0), c.a(2), c.a(1), Some(c.a(3) & 0xffff), true)
}
/// EnumResourceTypes/Names/Languages: the callback gets each id (a name as a temporary string)
/// until it returns FALSE; ERROR_RESOURCE_DATA/TYPE/NAME_NOT_FOUND when there is nothing.
fn enum_resources(c: &mut Ctx, level: usize, wide: bool) -> u32 {
    let m = if c.a(0) == 0 { module_handle(0, false) } else { c.a(0) };
    let path: Vec<crate::pe::ResId> = (0..level).map(|i| crate::pe::ResId::from_arg(c.a(1 + i as u32), wide)).collect();
    let (cb, param) = (c.a(1 + level as u32), c.a(2 + level as u32));
    // (read before the callbacks: on x64 the arguments are registers a call clobbers)
    let given: Vec<u32> = (0..level).map(|i| c.a(1 + i as u32)).collect();
    let Some(ids) = crate::pe::resource_ids(m, &path) else {
        last_error([1812, 1813, 1814][level.min(2)]);
        return 0;
    };
    if ids.is_empty() {
        last_error([1812, 1813, 1815][level.min(2)]);
        return 0;
    }
    for id in ids {
        let (arg, tmp) = match id {
            crate::pe::ResId::Int(n) => (n, 0),
            crate::pe::ResId::Name(w) => {
                let bytes: Vec<u8> = if wide {
                    w.iter().chain(&[0]).flat_map(|u| u.to_le_bytes()).collect()
                } else {
                    let mut a = crate::text::w2a(&w);
                    a.push(0);
                    a
                };
                let p = mem::heap_alloc(bytes.len() as u32, false);
                mem::write(p, &bytes);
                (p, p)
            }
        };
        // the callback's arguments: module, then the path so far, then this id, then the param
        let mut args = vec![m];
        args.extend(&given);
        if level == 2 {
            // EnumResourceLanguages' id is a WORD language
            args.push(arg & 0xffff);
        } else {
            args.push(arg);
        }
        args.push(param);
        let go = c.call(cb, &args);
        if tmp != 0 {
            mem::heap_free(tmp);
        }
        if go == 0 {
            break;
        }
    }
    1
}
fn EnumResourceTypesA(c: &mut Ctx) -> u32 {
    enum_resources(c, 0, false)
}
fn EnumResourceTypesW(c: &mut Ctx) -> u32 {
    enum_resources(c, 0, true)
}
fn EnumResourceNamesA(c: &mut Ctx) -> u32 {
    enum_resources(c, 1, false)
}
fn EnumResourceNamesW(c: &mut Ctx) -> u32 {
    enum_resources(c, 1, true)
}
fn EnumResourceLanguagesA(c: &mut Ctx) -> u32 {
    enum_resources(c, 2, false)
}
fn EnumResourceLanguagesW(c: &mut Ctx) -> u32 {
    enum_resources(c, 2, true)
}
fn LoadResource(c: &mut Ctx) -> u32 {
    let m = if c.a(0) == 0 { module_handle(0, false) } else { c.a(0) };
    if c.a(1) == 0 {
        last_error(87);
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
        last_error(87);
        return 0;
    }
    r32(c.a(1) + 4)
}

// ---------------------------------------------------------------------------------------------
// files

/// The handle of standard stream `i` (0 in, 1 out, 2 error).
pub(crate) fn std_handle(i: usize) -> u32 {
    STD.get_or_init(|| [obj::add(Obj::Other("stdin")), obj::add(Obj::Other("stdout")), obj::add(Obj::Other("stderr"))])[i]
}
static STD: OnceLock<[u32; 3]> = OnceLock::new();
static STD_GIVEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// STARTF_USESTDHANDLES's handles, for a process another one started with them
pub fn set_std_handles(h: Option<(u32, u32, u32)>) {
    if let Some((a, b, c)) = h {
        let _ = STD.set([a, b, c]);
        STD_GIVEN.store(true, Ordering::Relaxed);
    }
}
fn GetStdHandle(c: &mut Ctx) -> u32 {
    match c.a(0) {
        0xffff_fff6 => std_handle(0),
        0xffff_fff5 => std_handle(1),
        0xffff_fff4 => std_handle(2),
        _ => 0xffff_ffff,
    }
}
fn SetStdHandle(_: &mut Ctx) -> u32 {
    1
}
fn GetFileType(c: &mut Ctx) -> u32 {
    // FILE_TYPE_DISK, CHAR (the console), PIPE
    match obj::with(|t| t.objs.get(&c.a(0)).map(|(o, _)| match o {
        Obj::Other(_) => 2,
        Obj::Pipe(_) => 3,
        _ => 1,
    })) {
        Some(k) => k,
        None => 0,
    }
}
fn GetConsoleMode(_: &mut Ctx) -> u32 {
    0
}
/// GetConsoleCP / GetConsoleOutputCP: the ANSI code page
fn GetConsoleCP(_: &mut Ctx) -> u32 {
    crate::text::acp()
}
fn GetConsoleOutputCP(_: &mut Ctx) -> u32 {
    crate::text::acp()
}
/// WriteConsoleA/W(h, buf, n, &written, _): to stderr (no console; the CRT imports them)
fn WriteConsoleA(c: &mut Ctx) -> u32 {
    let (buf, n) = (c.a(1), c.a(2).min(0x10000));
    console_write(&String::from_utf8_lossy(mem::slice(buf, n)));
    put32(c.a(3), n);
    1
}
fn WriteConsoleW(c: &mut Ctx) -> u32 {
    let (buf, n) = (c.a(1), c.a(2).min(0x10000));
    let w: Vec<u16> = (0..n).map(|i| mem::r16(buf + 2 * i)).collect();
    console_write(&String::from_utf16_lossy(&w));
    put32(c.a(3), n);
    1
}
fn create_file(c: &mut Ctx, wide: bool) -> u32 {
    let name = arg_str(c.a(0), wide);
    if let Some(h) = crate::nt::via::create_file(c, &name) {
        return h;
    }
    open_file(name, c.a(1), c.a(2), c.a(4), c.a(5))
}

/// CreateFileW's work: `attrs` is dwFlagsAndAttributes.
fn open_file(name: String, access: u32, share: u32, disp: u32, attrs: u32) -> u32 {
    if crate::env_on!("SAKE_TRACE_FILES") {
        eprintln!("sake: CreateFile({name}, {access:#x}, {disp})");
    }
    if name.eq_ignore_ascii_case("CONOUT$") || name.eq_ignore_ascii_case("CONIN$") {
        return obj::add(Obj::Other("console"));
    }
    if let Some(n) = crate::kern::pipe_name(&name) {
        return crate::pipe::open(&n, access, attrs);
    }
    match files::open_shared(&name, access, disp, Some(share)) {
        // a directory opens only with FILE_FLAG_BACKUP_SEMANTICS
        Ok((f, _)) if f.dir && attrs & 0x0200_0000 == 0 => {
            last_error(5);
            0xffff_ffff
        }
        Ok((f, existed)) => {
            // CREATE_ALWAYS / OPEN_ALWAYS of an existing file: ERROR_ALREADY_EXISTS
            last_error(if existed && (disp == 2 || disp == 4) { 183 } else { 0 });
            // a file made (or remade) here takes the read-only / hidden / system attributes asked,
            // and the archive bit every new file has (winref files: CreateFile2 hidden is 0x22)
            if !f.dir && (!existed || disp == 2) && attrs & 7 != 0 {
                files::set_attrs(&f.path, attrs & 7 | 0x20);
            }
            if attrs & 0x0400_0000 != 0 {
                f.delete_on_close();
            }
            obj::add(Obj::File(std::sync::Arc::new(f)))
        }
        Err(e) => {
            note_failed_open(&name, e);
            last_error(e);
            0xffff_ffff
        }
    }
}
/// A file that did not open, in the run's log (the first 64 names, each once): what a game
/// looked for and did not find says much of why it stopped.
fn note_failed_open(name: &str, error: u32) {
    static SEEN: Mutex<Vec<String>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap_or_else(|e| e.into_inner());
    if seen.len() < 64 && !seen.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        seen.push(name.to_string());
        eprintln!("sake: CreateFile {name}: error {error}{}", if seen.len() == 64 { " (no more of these)" } else { "" });
    }
}
fn CreateFileA(c: &mut Ctx) -> u32 {
    create_file(c, false)
}
fn CreateFileW(c: &mut Ctx) -> u32 {
    create_file(c, true)
}
/// CreateFile2(name, access, share, disposition, CREATEFILE2_EXTENDED_PARAMETERS *): dwSize,
/// dwFileAttributes, dwFileFlags, ... (no parameters: no attributes or flags)
fn CreateFile2(c: &mut Ctx) -> u32 {
    let p = c.a(4);
    let attrs = if p == 0 { 0 } else { r32(p + 4) | r32(p + 8) };
    open_file(arg_str(c.a(0), true), c.a(1), c.a(2), c.a(3), attrs)
}
fn _lopen(c: &mut Ctx) -> u32 {
    let name = arg_str(c.a(0), false);
    match files::open(&name, if c.a(1) & 3 != 0 { 0xc000_0000 } else { 0x8000_0000 }, 3) {
        Ok(f) => obj::add(Obj::File(std::sync::Arc::new(f))),
        Err(_) => 0xffff_ffff,
    }
}
/// _lcreat(name, attributes): a new empty file (or the old one emptied), open to read and write
fn _lcreat(c: &mut Ctx) -> u32 {
    let name = arg_str(c.a(0), false);
    match files::open(&name, 0xc000_0000, 2) {
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
/// ReadFile / WriteFile on a socket: a recv / send, completed at once (with an IOCP packet)
fn socket_io(h: u32, buf: u32, n: u32, done: u32, ov: u32, write: bool) -> u32 {
    match crate::ws2::file_io(h, buf, n, write) {
        Ok(got) => {
            put32(done, got);
            crate::iocp::complete(h, ov, 0, got, true);
            1
        }
        Err(e) => {
            put32(done, 0);
            last_error(e);
            0
        }
    }
}
fn ReadFile(c: &mut Ctx) -> u32 {
    if let Some(r) = crate::nt::via::io(c, false) {
        return r;
    }
    let (h, buf, n, pread, ov) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if let Some(p) = crate::pipe::handle(h) {
        return crate::pipe::read(c, h, p);
    }
    if crate::ws2::is_socket(h) {
        return socket_io(h, buf, n, pread, ov, false);
    }
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
    if let Some(r) = crate::nt::via::io(c, true) {
        return r;
    }
    let (h, buf, n, pw) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if crate::ws2::is_socket(h) {
        return socket_io(h, buf, n, pw, c.a(4), true);
    }
    // (a length of -1: krkrz's console log passes what failed to convert; Windows can't read
    // that much and fails with ERROR_NOACCESS)
    if n >= 0x8000_0000 {
        put32(pw, 0);
        last_error(998);
        return 0;
    }
    if crate::env_on!("SAKE_TRACE_WRITES") {
        let b = mem::slice(buf, n.min(400));
        eprintln!("sake: WriteFile({h:x}, {n}): {:?}", String::from_utf16_lossy(&if b.len() > 2 && b[1] == 0 {
            b.chunks(2).map(|x| u16::from_le_bytes([x[0], *x.get(1).unwrap_or(&0)])).collect::<Vec<_>>()
        } else {
            a2s(b).encode_utf16().collect()
        }));
    }
    if let Some(p) = crate::pipe::handle(h) {
        return crate::pipe::write(c, h, p);
    }
    let kind = obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Other(k), _)) => Some(*k),
        _ => None,
    });
    if let Some(_k) = kind {
        console_write(&a2s(mem::slice(buf, n)));
        put32(pw, n);
        return 1;
    }
    let Some(f) = obj::file(h) else {
        last_error(6);
        return 0;
    };
    if !f.write {
        put32(pw, 0);
        last_error(5);
        return 0;
    }
    echo_write(&f.path, buf, n);
    let w = f.write_from(buf, n);
    put32(pw, w);
    ok(w == n)
}
/// Console output a line at a time (the page logs each write as its own line: 吉里吉里2 writes its
/// log a character at a time)
pub(crate) fn console_write(t: &str) {
    static PENDING: Mutex<String> = Mutex::new(String::new());
    let mut p = PENDING.lock().unwrap();
    p.push_str(t);
    while let Some(i) = p.find('\n') {
        let line: String = p.drain(..=i).collect();
        eprintln!("{}", line.trim_end_matches(['\r', '\n']));
    }
}
/// SAKE_ECHO_FILE=name: what the program writes to files of that name also goes to the log, a
/// line at a time (a game's own log file: 吉里吉里2 writes Debug.message only to krkr.console.log)
pub(crate) fn echo_write(path: &str, buf: u32, n: u32) {
    static NAME: OnceLock<Option<String>> = OnceLock::new();
    let Some(name) = NAME.get_or_init(|| std::env::var("SAKE_ECHO_FILE").ok().map(|s| s.to_ascii_lowercase())) else { return };
    if !path.to_ascii_lowercase().ends_with(name.as_str()) {
        return;
    }
    let b = mem::slice(buf, n);
    // (UTF-16 once the file started with a BOM, or when most odd bytes are 0; else the ANSI code page)
    static WIDE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if b.starts_with(&[0xff, 0xfe]) {
        WIDE.store(true, Ordering::Relaxed);
    }
    let zeros = b.iter().skip(1).step_by(2).filter(|&&x| x == 0).count();
    let t = if WIDE.load(Ordering::Relaxed) || (b.len() >= 2 && zeros * 2 >= b.len() / 2) {
        let u: Vec<u16> = b.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).filter(|&c| c != 0xfeff).collect();
        String::from_utf16_lossy(&u)
    } else {
        a2s(b)
    };
    console_write(&t);
}
fn SetFilePointer(c: &mut Ctx) -> u32 {
    if let Some(r) = crate::nt::via::set_file_pointer(c) {
        return r;
    }
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
    if let Some(r) = crate::nt::via::set_file_pointer_ex(c) {
        return r;
    }
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
    if let Some(r) = crate::nt::via::file_size(c, false) {
        return r;
    }
    // (a pipe: what there is to read)
    if let Some(p) = crate::pipe::handle(c.a(0)) {
        put32(c.a(1), 0);
        return crate::kern::with(|k| k.avail(p.end)).0;
    }
    let Some(f) = obj::file(c.a(0)) else {
        last_error(6);
        return 0xffff_ffff;
    };
    let s = f.size();
    put32(c.a(1), (s >> 32) as u32);
    s as u32
}
fn GetFileSizeEx(c: &mut Ctx) -> u32 {
    if let Some(r) = crate::nt::via::file_size(c, true) {
        return r;
    }
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

/// GetFileInformationByHandleEx(file, class, buffer, size): the classes Python's os.stat and
/// os.path ask for (basic, standard, attribute tag, id)
fn GetFileInformationByHandleEx(c: &mut Ctx) -> u32 {
    let Some(f) = obj::file(c.a(0)) else {
        last_error(6);
        return 0;
    };
    let Ok(m) = f.f.lock().unwrap().metadata() else { return 0 };
    let (class, p, size) = (c.a(1), c.a(2), c.a(3));
    let need = match class {
        0 => 40,
        1 => 24,
        9 => 8,
        18 => 24,
        _ => {
            last_error(87); // ERROR_INVALID_PARAMETER: "not this kind of file" to Python
            return 0;
        }
    };
    if size < need {
        last_error(24);
        return 0;
    }
    mem::fill(p, need, 0);
    let t = m.modified().map_or(0, files::filetime);
    match class {
        0 => {
            for o in [0, 8, 16, 24] {
                put64(p + o, t);
            }
            w32(p + 32, files::attrs(&m));
        }
        1 => {
            put64(p, m.len().next_multiple_of(4096));
            put64(p + 8, m.len());
            w32(p + 16, 1);
            mem::write(p + 21, &[m.is_dir() as u8]);
        }
        9 => w32(p, files::attrs(&m)),
        _ => {
            put64(p, 0x1234);
            // (a file id from the path: os.path.samefile compares them)
            let h = f.path.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b.to_ascii_lowercase() as u64).wrapping_mul(0x100_0000_01b3));
            put64(p + 8, h);
        }
    }
    1
}
/// PathCchSkipRoot(path, *root end): past "C:\", "C:", "\\server\share\" or "\"
fn PathCchSkipRoot(c: &mut Ctx) -> u32 {
    let (p, out) = (c.a(0), c.a(1));
    if p == 0 || out == 0 {
        return 0x8007_0057;
    }
    let w = mem::wstr(p);
    let sep = |i: usize| w.get(i).is_some_and(|&ch| ch == b'\\' as u16 || ch == b'/' as u16);
    let n = if w.len() >= 2 && w[1] == b':' as u16 {
        if sep(2) { 3 } else { 2 }
    } else if sep(0) && sep(1) {
        // \\server\share\ (or \\?\...)
        let mut i = 2;
        for _ in 0..2 {
            while i < w.len() && !sep(i) {
                i += 1;
            }
            if i < w.len() {
                i += 1;
            }
        }
        i
    } else if sep(0) {
        1
    } else {
        return 0x8007_0057;
    };
    mem::wp(out, p + 2 * n as u32);
    0
}
fn VerifyConsoleIoHandle(c: &mut Ctx) -> u32 {
    obj::file(c.a(0)).is_none() as u32
}
/// SetHandleInformation(h, mask, flags): HANDLE_FLAG_INHERIT (protection from close is not kept)
fn SetHandleInformation(c: &mut Ctx) -> u32 {
    let (h, mask, flags) = (c.a(0), c.a(1), c.a(2));
    if mask & 1 != 0 && !obj::set_inherit(h, flags & 1 != 0) {
        last_error(6);
        return 0;
    }
    1
}

fn write_find_data(p: u32, name: &str, m: &kura::fs::Metadata, wide: bool) {
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
    let (pat, data) = (c.a(0), c.a(1));
    find_first_at(pat, data, wide)
}
/// FindFirstFile(Ex): the pattern and where its WIN32_FIND_DATA goes (FindFirstFileEx's is its
/// third argument: in R8 on x64, so it cannot be moved into the second's stack slot)
fn find_first_at(pat: u32, data: u32, wide: bool) -> u32 {
    let pat = arg_str(pat, wide);
    if crate::env_on!("SAKE_TRACE_FILES") {
        let r = files::find(&pat).map(|f| f.entries.iter().map(|e| e.0.clone()).collect::<Vec<_>>().join(" "));
        eprintln!("sake: FindFirstFile({pat}) -> {r:?}");
    }
    match files::find(&pat) {
        Some(f) => {
            let (n, m) = &f.entries[0];
            write_find_data(data, n, m, wide);
            let mut f = f;
            f.at = 1;
            obj::add(Obj::Find(std::sync::Arc::new(Mutex::new(f))))
        }
        None => {
            // ("dir\": ERROR_INVALID_PARAMETER; "" and a missing directory: ERROR_PATH_NOT_FOUND)
            last_error(if pat.is_empty() { 3 } else if pat.ends_with(['\\', '/']) { 87 } else { files::missing(&pat) });
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
/// FindFirstFileEx(pattern, level, data, search op, filter, flags): FindExInfoStandard (0) and
/// FindExInfoBasic (1, no short name: sake has none anyway); the search op only advises
fn find_first_ex(c: &mut Ctx, wide: bool) -> u32 {
    if c.a(1) > 1 || c.a(3) > 2 || c.a(4) != 0 {
        last_error(87);
        return u32::MAX;
    }
    find_first_at(c.a(0), c.a(2), wide)
}
fn FindFirstFileExA(c: &mut Ctx) -> u32 {
    find_first_ex(c, false)
}
fn FindFirstFileExW(c: &mut Ctx) -> u32 {
    find_first_ex(c, true)
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
    if crate::env_on!("SAKE_TRACE_FILES") {
        eprintln!("sake: GetFileAttributes({name})");
    }
    match files::resolve(name).and_then(|h| kura::fs::metadata(&h).ok().map(|m| (h, m))) {
        Some((h, m)) => files::attrs_of(&h, &m),
        // (system32's DLLs that we implement are there: Siglus checks that kernel32.dll is,
        // then its language)
        None if is_system_dll_path(name) => 0x20,
        None => {
            last_error(files::missing(name));
            0xffff_ffff
        }
    }
}
/// C:\windows\system32\<a DLL we implement>
fn is_system_dll_path(name: &str) -> bool {
    let full = files::full_path(name).to_ascii_lowercase();
    full.strip_prefix("c:\\windows\\system32\\")
        .and_then(|n| n.strip_suffix(".dll"))
        .is_some_and(|d| !d.contains('\\') && crate::api::is_system(d))
}
fn GetFileAttributesA(c: &mut Ctx) -> u32 {
    let name = arg_str(c.a(0), false);
    crate::nt::via::attributes(c, &name).unwrap_or_else(|| file_attrs(&name))
}
fn GetFileAttributesW(c: &mut Ctx) -> u32 {
    let name = arg_str(c.a(0), true);
    crate::nt::via::attributes(c, &name).unwrap_or_else(|| file_attrs(&name))
}
fn attrs_ex(c: &mut Ctx, wide: bool) -> u32 {
    let name = arg_str(c.a(0), wide);
    if let Some(r) = crate::nt::via::attributes_ex(c, &name) {
        return r;
    }
    let Some((h, m)) = files::resolve(&name).and_then(|h| kura::fs::metadata(&h).ok().map(|m| (h, m))) else {
        last_error(files::missing(&name));
        return 0;
    };
    let p = c.a(2);
    w32(p, files::attrs_of(&h, &m));
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
fn set_file_attrs(name: &str, a: u32) -> u32 {
    match files::resolve(name) {
        Some(h) => {
            files::set_attrs(&h, a);
            1
        }
        None => {
            last_error(files::missing(name));
            0
        }
    }
}
fn SetFileAttributesA(c: &mut Ctx) -> u32 {
    set_file_attrs(&arg_str(c.a(0), false), c.a(1))
}
fn SetFileAttributesW(c: &mut Ctx) -> u32 {
    set_file_attrs(&arg_str(c.a(0), true), c.a(1))
}
/// CreateDirectory's and RemoveDirectory's name errors: none (ERROR_PATH_NOT_FOUND), wildcards
/// or other characters a name can't have (ERROR_INVALID_NAME)
fn bad_dir_name(name: &str) -> Option<u32> {
    if name.is_empty() {
        return Some(3);
    }
    let last = name.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or("");
    last.contains(['?', '*', '<', '>', '|', '"']).then_some(123)
}
pub fn mkdir(name: &str) -> u32 {
    if let Some(e) = bad_dir_name(name) {
        last_error(e);
        return 0;
    }
    let h = files::resolve_new(name);
    match kura::fs::create_dir(&h) {
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
pub(crate) fn rmdir(name: &str) -> u32 {
    if let Some(e) = bad_dir_name(name) {
        last_error(e);
        return 0;
    }
    let Some(h) = files::resolve(name) else {
        last_error(files::missing(name));
        return 0;
    };
    if !files::is_dir(&h) {
        last_error(267); // ERROR_DIRECTORY
        return 0;
    }
    if kura::fs::read_dir(&h).is_ok_and(|mut d| d.next().is_some()) {
        last_error(145); // ERROR_DIR_NOT_EMPTY
        return 0;
    }
    ok(kura::fs::remove_dir(h).is_ok())
}
fn RemoveDirectoryA(c: &mut Ctx) -> u32 {
    rmdir(&arg_str(c.a(0), false))
}
fn RemoveDirectoryW(c: &mut Ctx) -> u32 {
    rmdir(&arg_str(c.a(0), true))
}
fn del(name: &str) -> u32 {
    match files::resolve(name) {
        Some(h) if files::extra_attrs(&h) & 1 != 0 || files::is_dir(&h) => {
            last_error(5);
            0
        }
        Some(h) if files::delete_blocked(&h) => {
            last_error(32);
            0
        }
        Some(h) => {
            files::set_attrs(&h, 0);
            ok(kura::fs::remove_file(h).is_ok())
        }
        None => {
            last_error(files::missing(name));
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
        last_error(files::missing(from));
        return 0;
    };
    let t = files::resolve(to);
    if t.is_some() && !replace {
        last_error(183);
        return 0;
    }
    if files::delete_blocked(&f) || t.as_ref().is_some_and(|t| files::delete_blocked(t)) {
        last_error(32);
        return 0;
    }
    ok(kura::fs::rename(f, t.unwrap_or_else(|| files::resolve_new(to))).is_ok())
}
fn MoveFileA(c: &mut Ctx) -> u32 {
    mv(&arg_str(c.a(0), false), &arg_str(c.a(1), false), false)
}
fn MoveFileW(c: &mut Ctx) -> u32 {
    mv(&arg_str(c.a(0), true), &arg_str(c.a(1), true), false)
}
/// MoveFileEx: MOVEFILE_DELAY_UNTIL_REBOOT (4) only records the move for the next boot, which
/// never comes here (installers use it on files in use); NULL as the new name deletes.
fn move_ex(c: &mut Ctx, wide: bool) -> u32 {
    let (from, to, flags) = (c.a(0), c.a(1), c.a(2));
    if from == 0 {
        last_error(87);
        return 0;
    }
    if flags & 4 != 0 {
        return 1;
    }
    if to == 0 {
        return del(&arg_str(from, wide));
    }
    mv(&arg_str(from, wide), &arg_str(to, wide), flags & 1 != 0)
}
fn MoveFileExA(c: &mut Ctx) -> u32 {
    move_ex(c, false)
}
fn MoveFileExW(c: &mut Ctx) -> u32 {
    move_ex(c, true)
}
fn copy_file(from: &str, to: &str, fail: bool) -> u32 {
    let Some(f) = files::resolve(from) else {
        last_error(files::missing(from));
        return 0;
    };
    let t = files::resolve(to);
    if t.is_some() && fail {
        last_error(80);
        return 0;
    }
    let r = kura::fs::copy(f, t.unwrap_or_else(|| files::resolve_new(to))).is_ok();
    last_error(if r { 0 } else { 5 });
    ok(r)
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
    let unc_root = full.strip_prefix("\\\\").is_some_and(|u| u.matches('\\').count() <= 1);
    if c.a(3) != 0 && r < c.a(1) && (full.ends_with('\\') || unc_root) {
        // (no file part in "C:\" or "dir\")
        mem::wp(c.a(3), 0);
    } else if c.a(3) != 0 && r < c.a(1) {
        let pos = full.rfind('\\').map_or(0, |i| i + 1);
        let off = if wide { full[..pos].encode_utf16().count() * 2 } else { crate::text::s2a(&full[..pos]).len() };
        mem::wp(c.a(3), c.a(2) + off as u32);
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
pub(crate) fn set_dir(d: &str) -> u32 {
    let full = files::full_path(d);
    if crate::env_on!("SAKE_TRACE_FILES") {
        eprintln!("sake: SetCurrentDirectory({d}) -> {full}");
    }
    match files::resolve(&full).map(|h| kura::fs::metadata(h).map(|m| m.is_dir())) {
        Some(Ok(true)) => {}
        // (a file: Majiro sets its archive's name and expects that to fail)
        Some(_) => {
            last_error(267);
            return 0;
        }
        None => {
            last_error(3);
            return 0;
        }
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
    let _ = kura::fs::create_dir_all(files::to_host(w));
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
        let _ = kura::fs::File::create(files::resolve_new(&p));
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
/// The NT device of drive C: or Z: (QueryDosDevice), and back (for the Nt* file functions).
pub fn drive_device(d: char) -> Option<&'static str> {
    match d.to_ascii_uppercase() {
        'C' => Some("\\Device\\HarddiskVolume1"),
        'Z' => Some("\\Device\\HarddiskVolume2"),
        _ => None,
    }
}
fn query_dos_device(c: &mut Ctx, wide: bool) -> u32 {
    let (name, out, max) = (c.a(0), c.a(1), c.a(2));
    // the list of names: our two drives
    let v: Vec<String> = if name == 0 {
        vec!["C:".into(), "Z:".into()]
    } else {
        let n = arg_str(name, wide);
        let dev = n.chars().next().filter(|_| n.len() == 2 && n.ends_with(':')).and_then(drive_device);
        match dev {
            Some(d) => vec![d.into()],
            None => {
                last_error(2);
                return 0;
            }
        }
    };
    let mut units: Vec<u16> = Vec::new();
    for x in &v {
        if wide { units.extend(x.encode_utf16()) } else { units.extend(x.bytes().map(|b| b as u16)) }
        units.push(0);
    }
    units.push(0);
    if units.len() as u32 > max {
        last_error(122);
        return 0;
    }
    for (i, &u) in units.iter().enumerate() {
        if wide { w16(out + 2 * i as u32, u) } else { w8(out + i as u32, u as u8) }
    }
    units.len() as u32
}
fn QueryDosDeviceA(c: &mut Ctx) -> u32 {
    query_dos_device(c, false)
}
fn QueryDosDeviceW(c: &mut Ctx) -> u32 {
    query_dos_device(c, true)
}
fn logical_drive_strings(c: &mut Ctx, wide: bool) -> u32 {
    let (len, out) = (c.a(0), c.a(1));
    let s: Vec<u8> = b"C:\\\0Z:\\\0\0".to_vec();
    if len < s.len() as u32 {
        return s.len() as u32;
    }
    for (i, &b) in s.iter().enumerate() {
        if wide { w16(out + 2 * i as u32, b as u16) } else { w8(out + i as u32, b) }
    }
    s.len() as u32 - 1
}
fn GetLogicalDriveStringsA(c: &mut Ctx) -> u32 {
    logical_drive_strings(c, false)
}
fn GetLogicalDriveStringsW(c: &mut Ctx) -> u32 {
    logical_drive_strings(c, true)
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
/// SearchPath(path, name, ext, cap, buf, &file part): without a path, the exe's folder, the
/// current one, system32 (where the DLLs we implement are), the Windows folder and PATH
fn search_path(c: &mut Ctx, wide: bool) -> u32 {
    let (path, name, ext, cap, buf, part) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    let given = if name == 0 { String::new() } else { arg_str(name, wide) };
    if given.trim_start_matches(' ').is_empty() {
        last_error(87);
        return 0;
    }
    let mut n = given.clone();
    if ext != 0 && !n.contains('.') {
        n += &arg_str(ext, wide);
    }
    let path = if path != 0 && arg_str(path, wide).is_empty() { 0 } else { path };
    // a name with a path of its own ("C:\x", "\\x", "\x", "C:x", ".\x", "..\x") is looked
    // for there only, as given and then with the extension
    let b = given.as_bytes();
    let sep = |ch: u8| ch == b'\\' || ch == b'/';
    let own_path = sep(b[0]) || (b.len() > 1 && b[1] == b':') || (b[0] == b'.' && b.len() > 1 && (sep(b[1]) || (b[1] == b'.' && b.len() > 2 && sep(b[2]))));
    let dirs: Vec<String> = if own_path {
        vec![String::new()]
    } else if path != 0 {
        arg_str(path, wide).split(';').map(String::from).collect()
    } else {
        let exe = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.clone()).unwrap_or_default();
        let exe_dir = exe.rfind('\\').map_or(String::new(), |i| exe[..i].to_string());
        let mut v = vec![exe_dir, files::cwd(), "C:\\windows\\system32".into(), "C:\\windows".into()];
        v.extend(env_get("PATH").unwrap_or_default().split(';').filter(|d| !d.is_empty()).map(String::from));
        v
    };
    let names: Vec<String> = if own_path { vec![given, n] } else { vec![n] };
    for (d, n) in dirs.iter().filter(|d| own_path || !d.is_empty()).flat_map(|d| names.iter().map(move |n| (d, n))) {
        let w = files::full_path(&if own_path { n.clone() } else { format!("{}\\{n}", d.trim_end_matches('\\')) });
        if files::resolve(&w).is_some() || is_system_dll_path(&w) {
            let r = ret_str(buf, cap, &w, wide);
            if part != 0 {
                let dir = &w[..w.rfind('\\').map_or(0, |i| i + 1)];
                let off = if wide { 2 * dir.encode_utf16().count() } else { crate::text::s2a(dir).len() } as u32;
                // (pointer-sized; only when the name fitted)
                if r < cap {
                    mem::putp(part, buf + off);
                }
            }
            return r;
        }
    }
    last_error(2);
    0
}
fn SearchPathA(c: &mut Ctx) -> u32 {
    search_path(c, false)
}
fn SearchPathW(c: &mut Ctx) -> u32 {
    search_path(c, true)
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
    VIEWS.lock().unwrap().push((b, n.max(1), file.as_ref().map(|f| files::host_to_win(&f.path))));
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
        VIEWS.lock().unwrap().retain(|v| v.0 != a);
        mem::release(a);
    }
    1
}
/// Each view MapViewOfFile made: (base, size, the file's Windows path; None: pagefile-backed)
static VIEWS: Mutex<Vec<(u32, u32, Option<String>)>> = Mutex::new(Vec::new());
/// "C:\x\y" as the NT device path GetMappedFileName gives ("\Device\HarddiskVolume1\x\y")
fn device_path(win: &str) -> String {
    match win.chars().next().and_then(drive_device) {
        Some(d) if win.get(1..2) == Some(":") => format!("{d}{}", &win[2..]),
        _ => win.to_string(),
    }
}
/// The file behind an address (a module's image or a file view), as GetMappedFileName finds it:
/// Err(ERROR_FILE_INVALID) for a pagefile view, Err(ERROR_UNEXP_NET_ERR) for no mapping
fn mapped_file(a: u32) -> Result<String, u32> {
    if let Some(b) = crate::pe::containing(a) {
        return Ok(crate::pe::with(b, |m| m.path.clone()).unwrap());
    }
    match VIEWS.lock().unwrap().iter().find(|v| a >= v.0 && a - v.0 < v.1) {
        Some((_, _, Some(p))) => Ok(p.clone()),
        Some(_) => Err(1006),
        None => {
            // (a pagefile section a program shares by name)
            let named = obj::with(|t| t.objs.values().any(|(o, _)| matches!(o, Obj::Mapping { file: None, view, size } if *view != 0 && a >= *view && a - *view < *size)));
            Err(if named { 1006 } else { 59 })
        }
    }
}
/// GetMappedFileName(process, address, buffer, chars): the device path of the file mapped there
fn mapped_file_name(c: &mut Ctx, wide: bool) -> u32 {
    let (h, a, dst, cap) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if !psapi_access(h, false) {
        return 0;
    }
    if dst == 0 || cap == 0 {
        last_error(87);
        return 0;
    }
    match mapped_file(a) {
        Ok(p) => {
            let p = device_path(&p);
            let n = if wide { p.encode_utf16().count() } else { crate::text::s2a(&p).len() } as u32;
            put_name(dst, cap, &p, wide);
            n.min(cap)
        }
        Err(e) => {
            last_error(e);
            0
        }
    }
}
fn K32GetMappedFileNameA(c: &mut Ctx) -> u32 {
    mapped_file_name(c, false)
}
fn K32GetMappedFileNameW(c: &mut Ctx) -> u32 {
    mapped_file_name(c, true)
}
/// GetProcessImageFileName(process, buffer, chars): the exe's device path, which must fit with
/// its 0
fn image_file_name(c: &mut Ctx, wide: bool) -> u32 {
    let (h, dst, cap) = (c.a(0), c.a(1), c.a(2));
    if !psapi_access(h, false) {
        return 0;
    }
    let exe = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.clone()).unwrap_or_default();
    let p = device_path(&exe);
    let n = if wide { p.encode_utf16().count() } else { crate::text::s2a(&p).len() } as u32;
    if n >= cap || dst == 0 {
        last_error(122);
        return 0;
    }
    put_name(dst, cap, &p, wide)
}
/// The exe's path: its Windows path, or (`native`) the NT device path
pub(crate) fn exe_path(native: bool) -> String {
    let exe = crate::pe::MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.clone()).unwrap_or_default();
    if native { device_path(&exe) } else { exe }
}
/// QueryFullProcessImageName(process, flags, buffer, size in/out): flags 0 the Windows path,
/// PROCESS_NAME_NATIVE (1) the device path; *size the characters written, without the NUL
fn full_image_name(c: &mut Ctx, wide: bool) -> u32 {
    let (h, flags, dst, psize) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if flags > 1 || psize == 0 {
        last_error(87);
        return 0;
    }
    if !psapi_access(h, false) {
        return 0;
    }
    let p = exe_path(flags == 1);
    let n = if wide { p.encode_utf16().count() } else { crate::text::s2a(&p).len() } as u32;
    if n >= r32(psize) || dst == 0 {
        last_error(122);
        return 0;
    }
    put_name(dst, r32(psize), &p, wide);
    w32(psize, n);
    1
}
fn QueryFullProcessImageNameA(c: &mut Ctx) -> u32 {
    full_image_name(c, false)
}
fn QueryFullProcessImageNameW(c: &mut Ctx) -> u32 {
    full_image_name(c, true)
}
fn K32GetProcessImageFileNameA(c: &mut Ctx) -> u32 {
    image_file_name(c, false)
}
fn K32GetProcessImageFileNameW(c: &mut Ctx) -> u32 {
    image_file_name(c, true)
}

use crate::process::split_command;

/// Starting another program: sake runs one process, so a program of the game's own (an .exe
/// that exists) takes this one's place. The page reads the line and starts it once we exit
/// (play.ts); launchers end themselves right after anyway. False for anything else.
pub fn exec(file: &str, args: &str, dir: Option<&str>) -> bool {
    // (SAKE_NO_EXEC: tests/winetest, whose programs start copies of themselves as children)
    if std::env::var_os("SAKE_NO_EXEC").is_some() {
        return false;
    }
    let mut f = file.trim().to_string();
    if !f.to_ascii_lowercase().ends_with(".exe") {
        if files::resolve(&files::full_path(&f)).is_none() && files::resolve(&files::full_path(&format!("{f}.exe"))).is_some() {
            f += ".exe";
        } else {
            return false;
        }
    }
    let rel = !(f.starts_with('\\') || f.as_bytes().get(1) == Some(&b':'));
    let full = match dir.filter(|d| rel && !d.is_empty()) {
        Some(d) => files::full_path(&format!("{}\\{f}", files::full_path(d).trim_end_matches('\\'))),
        None => files::full_path(&f),
    };
    // (the file's own spelling: KiriKiri restarts やきゅられ体験版.eXe as ….exe, and the page
    // opens the path as given)
    let Some(h) = files::resolve(&full) else {
        eprintln!("sake: exec {full}: not found");
        return false;
    };
    let full = files::host_to_win(&h);
    eprintln!("sake: exec {full}\t{args}");
    unsafe { libc::exit(0) }
}
fn CreateProcessA(c: &mut Ctx) -> u32 {
    crate::process::create_process(c, false)
}
fn CreateProcessW(c: &mut Ctx) -> u32 {
    crate::process::create_process(c, true)
}
fn WinExec(c: &mut Ctx) -> u32 {
    let (prog, args) = split_command(&arg_str(c.a(0), false));
    exec(&prog, &args, None);
    2
}
/// OpenProcess(access, inherit, id): this process or one the kernel knows
fn OpenProcess(c: &mut Ctx) -> u32 {
    let pid = c.a(2);
    if !crate::process::is_self(pid) && !crate::process::exists(pid) {
        last_error(87);
        return 0;
    }
    let h = obj::add(Obj::Process { access: c.a(0) & PROCESS_ALL_ACCESS, pid });
    if c.a(1) != 0 {
        obj::set_inherit(h, true);
    }
    h
}
/// GetProcessId(h)
fn GetProcessId(c: &mut Ctx) -> u32 {
    match crate::process::of_handle(c.a(0)) {
        Some((pid, _)) => pid,
        None => {
            last_error(6);
            0
        }
    }
}
/// EnumProcesses(ids, bytes, &used): the processes running
fn K32EnumProcesses(c: &mut Ctx) -> u32 {
    let (p, cb, pu) = (c.a(0), c.a(1), c.a(2));
    let ids = crate::process::running();
    let n = ((cb / 4) as usize).min(ids.len());
    for (i, &id) in ids[..n].iter().enumerate() {
        w32(p + 4 * i as u32, id);
    }
    w32(pu, 4 * n as u32);
    1
}
/// OpenThread(access, inherit, id): a handle of the thread with that id (mono opens its own
/// threads by id)
fn OpenThread(c: &mut Ctx) -> u32 {
    let tid = c.a(2);
    let h = obj::with(|t| {
        t.objs.iter().filter(|(_, (o, _))| matches!(o, Obj::Thread { tid: x, .. } if *x == tid && tid != 0)).map(|(&h, (o, _))| (matches!(o, Obj::Thread { done: true, .. }), h)).min()
    });
    match h {
        Some((_, h)) => obj::dup(h),
        None => {
            last_error(87);
            0
        }
    }
}
/// GetStartupInfo: STARTF_USESTDHANDLES and the handles when the process was started with them
fn GetStartupInfoA(c: &mut Ctx) -> u32 {
    startup_info(c.a(0), false)
}
/// STARTUPINFO: the desktop is "Winsta0\Default" as for every interactive program (winref fmt)
fn startup_info(p: u32, wide: bool) -> u32 {
    static DESKTOP: std::sync::OnceLock<(u32, u32)> = std::sync::OnceLock::new();
    let (da, dw) = *DESKTOP.get_or_init(|| {
        let t = "Winsta0\\Default";
        let a = mem::heap_alloc(t.len() as u32 + 1, true);
        mem::write(a, t.as_bytes());
        let w = mem::heap_alloc(2 * t.len() as u32 + 2, true);
        for (i, u) in t.encode_utf16().enumerate() {
            w16(w + 2 * i as u32, u);
        }
        (a, w)
    });
    let (size, flags, std) = if mem::x64() { (104, 60, 80) } else { (68, 44, 56) };
    mem::fill(p, size, 0);
    w32(p, size);
    mem::wp(p + 2 * mem::ps(), if wide { dw } else { da });
    if STD_GIVEN.load(Ordering::Relaxed) {
        w32(p + flags, 0x100);
        for i in 0..3 {
            mem::wp(p + std + i * mem::ps(), std_handle(i as usize));
        }
    }
    0
}
fn GetStartupInfoW(c: &mut Ctx) -> u32 {
    startup_info(c.a(0), true)
}
fn SetHandleCount(c: &mut Ctx) -> u32 {
    c.a(0)
}
fn FormatMessageA(c: &mut Ctx) -> u32 {
    crate::fmtmsg::format_message(c, false)
}
fn FormatMessageW(c: &mut Ctx) -> u32 {
    crate::fmtmsg::format_message(c, true)
}
fn Beep(_: &mut Ctx) -> u32 {
    1
}
/// Copies `name` into (buf, *size chars); a short buffer fails with `short_err` and the size
/// needed (with the NUL), success leaves the length without it
fn put_sized_name(buf: u32, sizep: u32, name: &str, wide: bool, short_err: u32) -> u32 {
    if sizep == 0 {
        last_error(87);
        return 0;
    }
    let n = name.len() as u32;
    if r32(sizep) <= n || buf == 0 {
        w32(sizep, n + 1);
        last_error(short_err);
        return 0;
    }
    if wide {
        put_wstr(buf, n + 1, &a2w(name.as_bytes()));
    } else {
        put_cstr(buf, n + 1, name.as_bytes());
    }
    w32(sizep, n);
    1
}
fn GetComputerNameA(c: &mut Ctx) -> u32 {
    put_sized_name(c.a(0), c.a(1), "SAKE", false, 111)
}
fn GetComputerNameW(c: &mut Ctx) -> u32 {
    put_sized_name(c.a(0), c.a(1), "SAKE", true, 111)
}
/// GetComputerNameEx(type, buf, &size): the NetBIOS and DNS host names are "SAKE", with no
/// DNS domain
fn computer_name_ex(c: &mut Ctx, wide: bool) -> u32 {
    let name = match c.a(0) {
        0 | 1 | 3 | 4 | 5 | 7 => "SAKE",
        2 | 6 => "",
        _ => {
            last_error(87);
            return 0;
        }
    };
    put_sized_name(c.a(1), c.a(2), name, wide, 234)
}
fn GetUserNameA(c: &mut Ctx) -> u32 {
    put_cstr(c.a(0), r32(c.a(1)), b"user");
    w32(c.a(1), 5);
    1
}
/// RtlGenRandom (advapi32!SystemFunction036): the browser's random bytes.
fn SystemFunction036(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(0), c.a(1));
    let mut buf = vec![0u8; n as usize];
    for chunk in buf.chunks_mut(256) {
        if unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) } != 0 {
            return 0;
        }
    }
    mem::write(p, &buf);
    1
}
/// BCryptOpenAlgorithmProvider(&alg, id, implementation, flags): any algorithm "opens"; only
/// BCryptGenRandom does something with it.
/// BCryptGenRandom(alg, buffer, length, flags)
fn BCryptGenRandom(c: &mut Ctx) -> u32 {
    let mut buf = vec![0u8; c.a(2) as usize];
    for chunk in buf.chunks_mut(256) {
        unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) };
    }
    mem::write(c.a(1), &buf);
    0
}
fn BCryptGetProperty(_: &mut Ctx) -> u32 {
    0xc000_00bb // STATUS_NOT_SUPPORTED
}
/// avrt (MMCSS): AvSetMmThreadCharacteristics(task, &index) -> a handle; the rest succeed.
fn AvSetMmThreadCharacteristics(c: &mut Ctx) -> u32 {
    let max = crate::api::name_of(c.trap).unwrap_or_default().contains("Max");
    put32(c.a(if max { 2 } else { 1 }), 1);
    0x4d4d_4353
}
fn av_true(_: &mut Ctx) -> u32 {
    1
}
/// pdh: a query whose counters all read 0 (LiveMaker watches the CPU load).
fn PdhOpenQuery(c: &mut Ctx) -> u32 {
    put32(c.a(2), mem::heap_alloc(16, true));
    0
}
/// PdhAddCounter(query, path, user, &counter)
fn PdhAddCounter(c: &mut Ctx) -> u32 {
    put32(c.a(3), mem::heap_alloc(16, true));
    0
}
fn pdh_ok(_: &mut Ctx) -> u32 {
    0
}
/// PdhGetFormattedCounterValue(counter, format, &type, PDH_FMT_COUNTERVALUE*)
fn PdhGetFormattedCounterValue(c: &mut Ctx) -> u32 {
    put32(c.a(2), 0);
    if c.a(3) != 0 {
        mem::fill(c.a(3), 16, 0);
    }
    0
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
    w32(p + 8, crate::pid());
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
    ok(dir.is_empty() || files::resolve(dir).is_some_and(|h| crate::files::is_dir(&h)))
}
/// SearchTreeForFile(root, name, out): the first file called `name` under `root`, depth first.
fn SearchTreeForFile(c: &mut Ctx) -> u32 {
    let (root, name, out) = (arg_str(c.a(0), false), arg_str(c.a(1), false), c.a(2));
    let base = name.rsplit(['\\', '/']).next().unwrap_or(&name).to_lowercase();
    fn walk(host: &std::path::Path, guest: &str, base: &str, depth: u32) -> Option<String> {
        let mut dirs = Vec::new();
        for e in kura::fs::read_dir(host).ok()?.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            let g = format!("{}\\{n}", guest.trim_end_matches('\\'));
            if crate::files::is_dir(e.path()) {
                dirs.push((e.path(), g));
            } else if n.to_lowercase() == base {
                return Some(g);
            }
        }
        if depth < 16 {
            for (h, g) in dirs {
                if let Some(r) = walk(&h, &g, base, depth + 1) {
                    return Some(r);
                }
            }
        }
        None
    }
    let found = files::resolve(&root).and_then(|h| walk(std::path::Path::new(&h), &root, &base, 0));
    match found {
        Some(g) => {
            let mut b = crate::text::s2a(&g);
            b.truncate(259);
            b.push(0);
            crate::mem::write(out, &b);
            1
        }
        None => {
            if out != 0 {
                crate::mem::w8(out, 0);
            }
            0
        }
    }
}
fn WNetGetUniversalNameA(_: &mut Ctx) -> u32 {
    2250 // ERROR_NOT_CONNECTED: a local drive
}
/// GetProcessMitigationPolicy(process, policy, buffer, length): no mitigation is on (ChakraCore
/// fails fast without the function)
fn GetProcessMitigationPolicy(c: &mut Ctx) -> u32 {
    if c.a(2) != 0 {
        mem::fill(c.a(2), c.a(3), 0);
    }
    1
}
fn true_(_: &mut Ctx) -> u32 {
    1
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

pub fn init() {
    implement("kernel32", "SetThreadDescription", |_| 0);
    implement("msvcrt", "_beginthreadex", _beginthreadex);
    implement("msvcrt", "_beginthread", _beginthread);
    implement("msvcrt", "_endthreadex", _endthreadex);
    implement("msvcrt", "_endthread", _endthread);
    implement("kernel32", "SetThreadExecutionState", SetThreadExecutionState);
    implement("psapi", "GetProcessMemoryInfo", K32GetProcessMemoryInfo);
    for d in ["kernel32", "kernelbase"] {
        implement(d, "QueryFullProcessImageNameA", QueryFullProcessImageNameA);
        implement(d, "QueryFullProcessImageNameW", QueryFullProcessImageNameW);
    }
    for (n, f) in [
        ("EnumProcessModules", K32EnumProcessModules as crate::api::Handler),
        ("EnumProcessModulesEx", K32EnumProcessModulesEx),
        ("EnumProcesses", K32EnumProcesses),
        ("GetPerformanceInfo", K32GetPerformanceInfo),
        ("GetMappedFileNameA", K32GetMappedFileNameA),
        ("GetMappedFileNameW", K32GetMappedFileNameW),
        ("GetProcessImageFileNameA", K32GetProcessImageFileNameA),
        ("GetProcessImageFileNameW", K32GetProcessImageFileNameW),
        ("GetModuleInformation", K32GetModuleInformation),
        ("GetModuleBaseNameA", K32GetModuleBaseNameA),
        ("GetModuleBaseNameW", K32GetModuleBaseNameW),
        ("GetModuleFileNameExA", K32GetModuleFileNameExA),
        ("GetModuleFileNameExW", K32GetModuleFileNameExW),
    ] {
        implement("psapi", n, f);
        implement("kernel32", &format!("K32{n}"), f);
        implement("kernelbase", &format!("K32{n}"), f);
    }
    implement("kernel32", "ResolveDelayLoadedAPI", ResolveDelayLoadedAPI);
    implement("kernel32", "IsBadStringPtrW", IsBadStringPtrW);
    reg!("ntdll"; RtlWow64EnableFsRedirection, RtlWow64EnableFsRedirectionEx);
    for d in ["kernel32", "kernelbase"] {
        implement(d, "Wow64DisableWow64FsRedirection", Wow64DisableWow64FsRedirection);
        implement(d, "Wow64RevertWow64FsRedirection", Wow64RevertWow64FsRedirection);
        implement(d, "Wow64EnableWow64FsRedirection", Wow64EnableWow64FsRedirection);
    }
    implement("kernel32", "GetUserDefaultLocaleName", GetUserDefaultLocaleName);
    implement("kernel32", "LocaleNameToLCID", LocaleNameToLCID);
    implement("ntdll", "NtQuerySystemTime", NtQuerySystemTime);
    implement("ntdll", "RtlTimeToSecondsSince1970", RtlTimeToSecondsSince1970);
    // heap walks find no entries (sake's heaps keep no per-block list a program could walk)
    implement("ntdll", "RtlWalkHeap", |_| 0x8000_001a); // STATUS_NO_MORE_ENTRIES
    implement("kernel32", "HeapWalk", |_| {
        last_error(259); // ERROR_NO_MORE_ITEMS
        0
    });
    implement("ntdll", "LdrGetDllHandle", LdrGetDllHandle);
    implement("ntdll", "RtlFindExportedRoutineByName", RtlFindExportedRoutineByName);
    implement("ntdll", "NtQueryVirtualMemory", NtQueryVirtualMemory);
    implement("kernel32", "LCIDToLocaleName", LCIDToLocaleName);
    for d in ["kernel32", "kernelbase"] {
        for n in ["GetThreadPreferredUILanguages", "GetUserPreferredUILanguages", "GetSystemPreferredUILanguages"] {
            implement(d, n, preferred_ui_languages);
        }
        implement(d, "GetProcessPreferredUILanguages", process_ui_languages);
        implement(d, "SetThreadPreferredUILanguages", |_| 1);
        implement(d, "SetProcessPreferredUILanguages", |_| 1);
        implement(d, "GetThreadUILanguage", |_| lcid());
    }
    for d in ["kernel32", "kernelbase", "version"] {
        implement(d, "VerLanguageNameA", |c| ver_language_name(c, false));
        implement(d, "VerLanguageNameW", |c| ver_language_name(c, true));
    }
    implement("kernel32", "GetLocaleInfoEx", GetLocaleInfoEx);
    implement("kernel32", "GetSystemDefaultLocaleName", GetUserDefaultLocaleName);
    reg!("kernel32";
        GetCommandLineA, GetCommandLineW, GetEnvironmentStrings, GetEnvironmentStringsA, GetEnvironmentStringsW,
        FreeEnvironmentStringsA, FreeEnvironmentStringsW, GetEnvironmentVariableA, GetEnvironmentVariableW,
        SetEnvironmentVariableA, SetEnvironmentVariableW, ExpandEnvironmentStringsA, ExpandEnvironmentStringsW,
        GetCurrentProcess, GetCurrentProcessId, GetProcessId, GetCurrentThread, GetCurrentThreadId, GetThreadId, ExitProcess, TerminateProcess,
        FatalAppExitA, ExitThread, CreateThread, ResumeThread, SuspendThread, GetExitCodeThread, GetExitCodeProcess,
        TerminateThread, GetThreadPriority, SetThreadPriority, SetPriorityClass, GetPriorityClass, SetThreadAffinityMask,
        SetProcessAffinityMask, GetProcessAffinityMask, SetThreadIdealProcessor, Sleep, SleepEx, SwitchToThread,
        CreateEventA, CreateEventW, CreateEventExW, CreateWaitableTimerA, CreateWaitableTimerW, CreateWaitableTimerExW,
        OpenWaitableTimerA, OpenWaitableTimerW, SetWaitableTimer, SetWaitableTimerEx, CancelWaitableTimer, OpenEventA, OpenEventW, SetEvent, ResetEvent, PulseEvent,
        CreateMutexA, CreateMutexW, OpenMutexA, OpenMutexW, ReleaseMutex, CreateSemaphoreA, CreateSemaphoreW, ReleaseSemaphore,
        WaitForSingleObject, WaitForSingleObjectEx, WaitForMultipleObjects, WaitForMultipleObjectsEx, CloseHandle,
        DuplicateHandle, InitializeCriticalSection, InitializeCriticalSectionAndSpinCount, SetCriticalSectionSpinCount, GetHandleInformation,
        InitializeCriticalSectionEx,
        DeleteCriticalSection, EnterCriticalSection, LeaveCriticalSection, TryEnterCriticalSection,
        InterlockedIncrement, InterlockedDecrement, InterlockedExchange, InterlockedExchangeAdd, InterlockedCompareExchange,
        TlsAlloc, TlsFree, TlsGetValue, TlsSetValue, FlsAlloc, FlsFree, FlsGetValue, FlsSetValue,
        GetLastError, SetLastError, SetErrorMode, GetErrorMode, SetThreadErrorMode, GetThreadErrorMode, RtlUnwind, RaiseException, SetUnhandledExceptionFilter,
        UnhandledExceptionFilter, IsDebuggerPresent, OutputDebugStringA, OutputDebugStringW, SetConsoleCtrlHandler,
        DebugBreak, EncodePointer, DecodePointer,
        VirtualAlloc, VirtualFree, VirtualQuery, VirtualProtect, VirtualLock, VirtualUnlock, FlushInstructionCache,
        IsBadReadPtr, ReadProcessMemory, WriteProcessMemory, IsBadWritePtr, IsBadCodePtr, IsBadStringPtrA, GetProcessHeap, HeapCreate, HeapDestroy, HeapAlloc,
        HeapFree, HeapReAlloc, HeapSize, HeapValidate, HeapCompact, HeapSetInformation, HeapLock, HeapUnlock,
        GlobalAlloc, GlobalFree, GlobalReAlloc, GlobalLock, GlobalUnlock, GlobalHandle, GlobalSize, GlobalFlags,
        LocalAlloc, LocalFree, LocalReAlloc, LocalLock, LocalUnlock, LocalSize, LocalHandle, LocalFlags,
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
        IsValidLocale, GetLocaleInfoA, GetLocaleInfoW, EnumCalendarInfoA, EnumCalendarInfoW, CompareStringOrdinal, VirtualQueryEx, GetCPInfo, GetCPInfoExA, IsDBCSLeadByte,
        IsDBCSLeadByteEx, MultiByteToWideChar, WideCharToMultiByte, GetStringTypeA, GetStringTypeExA, GetStringTypeW,
        GetStringTypeExW, LCMapStringA, LCMapStringW, CompareStringA, CompareStringW, GetDateFormatA, GetTimeFormatA, GetDateFormatW, GetTimeFormatW,
        MulDiv, lstrlenA, lstrlenW, lstrcpyA, lstrcpyW, lstrcpynA, lstrcpynW, lstrcatA, lstrcatW, lstrcmpA, lstrcmpiA,
        lstrcmpW, lstrcmpiW, GlobalAddAtomA, GlobalAddAtomW, AddAtomA, AddAtomW, GlobalFindAtomA, FindAtomA, GlobalFindAtomW, FindAtomW, GlobalDeleteAtom,
        DeleteAtom, GlobalGetAtomNameA, GlobalGetAtomNameW, GetAtomNameA, GetAtomNameW,
        GetSystemTime, GetLocalTime, GetSystemTimeAsFileTime, GetSystemTimePreciseAsFileTime, FileTimeToSystemTime,
        SystemTimeToFileTime, FileTimeToLocalFileTime, LocalFileTimeToFileTime, FileTimeToDosDateTime, DosDateTimeToFileTime, SystemTimeToTzSpecificLocalTime,
        TzSpecificLocalTimeToSystemTime, CompareFileTime,
        GetTimeZoneInformation, GetTickCount, GetTickCount64, QueryPerformanceCounter, QueryPerformanceFrequency,
        GetProcessTimes, GetThreadTimes,
        GetModuleFileNameA, GetModuleFileNameW, GetModuleHandleA, GetModuleHandleW, GetModuleHandleExA,
        GetModuleHandleExW, LoadLibraryA, LoadLibraryW, LoadLibraryExA, LoadLibraryExW, FreeLibrary,
        DisableThreadLibraryCalls, GetProcAddress, FindResourceA, FindResourceW, FindResourceExA, EnumResourceTypesA, EnumResourceTypesW, EnumResourceNamesA, EnumResourceNamesW, EnumResourceLanguagesA, EnumResourceLanguagesW, FindResourceExW,
        LoadResource, LockResource, FreeResource, SizeofResource,
        GetStdHandle, SetStdHandle, SetDllDirectoryA, SetDllDirectoryW, GetFileInformationByHandleEx, PathCchSkipRoot, VerifyConsoleIoHandle, SetHandleInformation, GetFileType, GetConsoleMode, WriteConsoleA, WriteConsoleW, GetConsoleCP, GetConsoleOutputCP, CreateFileA, CreateFileW, CreateFile2, _lopen, _lcreat, _lclose, _lread,
        _llseek, ReadFile, WriteFile, SetFilePointer, SetFilePointerEx, GetFileSize, GetFileSizeEx, SetEndOfFile,
        FlushFileBuffers, LockFile, UnlockFile, GetFileTime, SetFileTime, GetFileInformationByHandle,
        FindFirstFileA, FindFirstFileW, FindFirstFileExA, FindFirstFileExW, FindNextFileA, FindNextFileW, FindClose, GetFileAttributesA,
        GetFileAttributesW, GetFileAttributesExA, GetFileAttributesExW, SetFileAttributesA, SetFileAttributesW,
        CreateDirectoryA, CreateDirectoryW, RemoveDirectoryA, RemoveDirectoryW, DeleteFileA, DeleteFileW, MoveFileA,
        MoveFileW, MoveFileExA, MoveFileExW, CopyFileA, CopyFileW, GetFullPathNameA, GetFullPathNameW,
        GetShortPathNameA, GetShortPathNameW, GetLongPathNameA, GetLongPathNameW, GetCurrentDirectoryA,
        GetCurrentDirectoryW, SetCurrentDirectoryA, SetCurrentDirectoryW, GetTempPathA, GetTempPathW,
        GetTempFileNameA, GetTempFileNameW, GetWindowsDirectoryA, GetWindowsDirectoryW, GetSystemDirectoryA,
        GetSystemDirectoryW, GetSystemWindowsDirectoryA, GetDriveTypeA, GetDriveTypeW, GetLogicalDrives,
        GetDiskFreeSpaceA, GetDiskFreeSpaceExA, GetDiskFreeSpaceExW, GetVolumeInformationA, GetVolumeInformationW,
        SearchPathA, SearchPathW, CreateIoCompletionPort, PostQueuedCompletionStatus, GetQueuedCompletionStatus,
        CreateFileMappingA, CreateFileMappingW, MapViewOfFile, MapViewOfFileEx, UnmapViewOfFile,
        CreateProcessA, CreateProcessW, WinExec, OpenProcess, OpenThread, GetStartupInfoA, GetStartupInfoW, SetHandleCount,
        FormatMessageA, FormatMessageW, Beep, GetComputerNameA, GetComputerNameW, InitializeSListHead, GetCurrentPackageId,
        CreateToolhelp32Snapshot, Process32First, Process32FirstW, ConvertDefaultLocale,
        InterlockedPushEntrySList, InterlockedPopEntrySList, InterlockedFlushSList, QueryDepthSList, InterlockedPushListSListEx,
        AreFileApisANSI, SetFileApisToANSI,
    );
    reg!("advapi32"; GetUserNameA, SystemFunction036);
    for d in ["kernel32", "kernelbase"] {
        implement(d, "GetProcessMitigationPolicy", GetProcessMitigationPolicy);
        implement(d, "SetProcessMitigationPolicy", true_);
        implement(d, "SetThreadInformation", true_);
    }
    implement("kernelbase", "VirtualAllocFromApp", VirtualAlloc);
    implement("kernelbase", "WaitOnAddress", WaitOnAddress);
    for d in ["kernel32", "kernelbase"] {
        implement(d, "InitOnceBeginInitialize", InitOnceBeginInitialize);
        implement(d, "InitOnceComplete", InitOnceComplete);
        implement(d, "QueueUserAPC", crate::apc::QueueUserAPC);
    }
    implement("kernelbase", "WakeByAddressAll", WakeByAddressAll);
    implement("kernelbase", "WakeByAddressSingle", WakeByAddressAll);
    implement("ntdll", "RtlWakeAddressAll", WakeByAddressAll);
    implement("ntdll", "RtlWakeAddressSingle", WakeByAddressAll);
    // RtlWaitOnAddress(address, &compare, size, timeout): STATUS_SUCCESS / TIMEOUT / INVALID_PARAMETER
    implement("ntdll", "RtlWaitOnAddress", |c| match wait_on_address(c.a(0), c.a(1), c.a(2), crate::ntsync::timeout_ms(c.a(3))) {
        Ok(()) => 0,
        Err(87) => 0xc000_000d,
        Err(_) => 0x102,
    });
    implement("ntdll", "RtlWakeAddressSingle", WakeByAddressAll);
    implement("kernelbase", "VirtualProtectFromApp", VirtualProtect);
    reg!("bcrypt"; BCryptGenRandom, BCryptGetProperty);
    for n in ["PdhOpenQueryA", "PdhOpenQueryW", "PdhOpenQuery"] {
        implement("pdh", n, PdhOpenQuery);
    }
    for n in ["PdhAddCounterA", "PdhAddCounterW", "PdhAddEnglishCounterA", "PdhAddEnglishCounterW"] {
        implement("pdh", n, PdhAddCounter);
    }
    for n in ["PdhCollectQueryData", "PdhRemoveCounter", "PdhCloseQuery"] {
        implement("pdh", n, pdh_ok);
    }
    reg!("pdh"; PdhGetFormattedCounterValue);
    for n in ["AvSetMmThreadCharacteristicsA", "AvSetMmThreadCharacteristicsW", "AvSetMmMaxThreadCharacteristicsA", "AvSetMmMaxThreadCharacteristicsW"] {
        implement("avrt", n, AvSetMmThreadCharacteristics);
    }
    implement("avrt", "AvRevertMmThreadCharacteristics", av_true);
    implement("avrt", "AvSetMmThreadPriority", av_true);
    for n in ["Process32Next", "Process32NextW", "Module32First", "Module32FirstW", "Module32Next", "Module32NextW", "Thread32First", "Thread32Next"] {
        implement("kernel32", n, no_more);
    }
    reg!("dbghelp"; MakeSureDirectoryPathExists, SearchTreeForFile);
    reg!("imagehlp"; MakeSureDirectoryPathExists, SearchTreeForFile);
    reg!("mpr"; WNetGetUniversalNameA);
    implement("mpr", "WNetGetUniversalNameW", WNetGetUniversalNameA);
    reg!("advapi32"; RegisterTraceGuidsW, UnregisterTraceGuids, TraceEvent, GetTraceLoggerHandle, GetTraceEnableFlags, GetTraceEnableLevel);
    reg!("ntdll"; RtlGetVersion, RtlGetNtVersionNumbers, RtlVerifyVersionInfo, RtlFirstEntrySList);
    for d in ["kernel32", "kernelbase"] {
        implement(d, "GetComputerNameExA", |c| computer_name_ex(c, false));
        implement(d, "GetComputerNameExW", |c| computer_name_ex(c, true));
    }
    reg!("kernel32"; DeviceIoControl, QueryDosDeviceA, QueryDosDeviceW, GetLogicalDriveStringsA, GetLogicalDriveStringsW, GetProductInfo);
    reg!("ntdll"; RtlMultiByteToUnicodeN, RtlUnicodeToMultiByteN, RtlMultiByteToUnicodeSize, RtlUnicodeToMultiByteSize,
        RtlUpcaseUnicodeChar, RtlCompareUnicodeString, RtlEqualUnicodeString);
    reg!("ntdll"; RtlInitUnicodeString, RtlInitAnsiString, RtlFreeUnicodeString, RtlAnsiStringToUnicodeString,
        RtlUnicodeStringToAnsiString, RtlDosPathNameToNtPathName_U, RtlFormatCurrentUserKeyPath, LdrGetProcedureAddress,
        NtProtectVirtualMemory);
    implement("ntdll", "RtlInitString", RtlInitAnsiString);
    implement("ntdll", "RtlFreeAnsiString", RtlFreeUnicodeString);
    implement("ntdll", "ZwProtectVirtualMemory", NtProtectVirtualMemory);
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

#[cfg(test)]
mod version_tests {
    use super::*;
    use proptest::prelude::*;

    /// Wine's kernel32 tests/version.c test_VerifyVersionInfo table (checked on Windows by Wine):
    /// (VerifyVersionInfo type mask, how the asked version differs from GetVersionEx's: 0 all zero,
    /// 1 same, 2 minor+1, 3 SP minor+1, 4 SP major+1, 5 SP major-1, 6 major-1, 7 build+1,
    /// expected error (0: TRUE), VerSetConditionMask calls)
    const WINE: &[(u32, u32, u32, &[(u32, u8)])] = &[
    // 49 cases
        (0x33, 2, 0, &[(0x2, 1), (0x1, 4)]),
        (0x33, 2, 1150, &[(0x2, 3), (0x1, 4)]),
        (0x33, 1, 0, &[(0x2, 3), (0x1, 4)]),
        (0x33, 1, 0, &[(0x2, 3), (0x1, 6)]),
        (0x33, 2, 0, &[(0x2, 5), (0x1, 4)]),
        (0x33, 2, 1150, &[(0x2, 6), (0x1, 4)]),
        (0x33, 2, 1150, &[(0x2, 7), (0x1, 4)]),
        (0x33, 3, 1150, &[(0x1, 1), (0x10, 4)]),
        (0x33, 3, 1150, &[(0x2, 1), (0x10, 4)]),
        (0x33, 4, 1150, &[(0x2, 1), (0x20, 1)]),
        (0x30, 3, 0, &[(0x20, 1), (0x10, 4)]),
        (0x33, 3, 1150, &[(0x20, 1), (0x10, 4)]),
        (0x31, 3, 0, &[(0x1, 1), (0x20, 1), (0x10, 4)]),
        (0x33, 3, 1150, &[(0x1, 1), (0x20, 1), (0x10, 4)]),
        (0x33, 3, 0, &[(0x2, 1), (0x1, 1), (0x20, 1), (0x10, 4)]),
        (0x33, 3, 1150, &[(0x2, 1), (0x1, 3), (0x20, 1), (0x10, 4)]),
        (0x33, 4, 0, &[(0x2, 5), (0x20, 2)]),
        (0x31, 4, 1150, &[(0x2, 1), (0x20, 4)]),
        (0x33, 4, 1150, &[(0x1, 1), (0x20, 4)]),
        (0x32, 4, 0, &[(0x2, 1), (0x20, 4)]),
        (0x33, 4, 1150, &[(0x2, 1), (0x20, 4)]),
        (0x33, 4, 0, &[(0x2, 1), (0x1, 1), (0x20, 4)]),
        (0x33, 4, 1150, &[(0x2, 3), (0x20, 4)]),
        (0x33, 6, 0, &[(0x2, 3), (0x20, 4)]),
        (0x33, 1, 0, &[(0x2, 3), (0x20, 4)]),
        (0x33, 4, 1150, &[(0x2, 3), (0x1, 1), (0x20, 4)]),
        (0x33, 4, 1150, &[(0x2, 3), (0x1, 3), (0x20, 5)]),
        (0x33, 4, 1150, &[(0x2, 3), (0x20, 6)]),
        (0x3, 0, 0, &[(0x2, 3)]),
        (0x7, 0, 1150, &[(0x2, 3)]),
        (0x40, 0, 0, &[(0x40, 6)]),
        (0x40, 0, 0, &[(0x40, 7)]),
        (0x31, 3, 1150, &[(0x1, 3)]),
        (0x31, 4, 0, &[(0x1, 4)]),
        (0x31, 4, 0, &[(0x1, 5)]),
        (0x31, 4, 1150, &[(0x1, 1)]),
        (0x31, 4, 1150, &[(0x1, 3)]),
        (0x31, 2, 1150, &[(0x1, 3)]),
        (0x33, 2, 1150, &[(0x1, 3)]),
        (0x31, 1, 0, &[(0x1, 3)]),
        (0x33, 7, 1150, &[(0x1, 3)]),
        (0x31, 7, 0, &[(0x1, 3)]),
        (0x31, 5, 0, &[(0x1, 2)]),
        (0x31, 5, 0, &[(0x1, 3)]),
        (0x33, 5, 1150, &[(0x2, 1), (0x20, 2)]),
        (0x33, 5, 0, &[(0x2, 3), (0x1, 1), (0x20, 2)]),
        (0x33, 5, 0, &[(0x2, 3), (0x1, 5), (0x20, 2)]),
        (0x33, 5, 0, &[(0x2, 3), (0x1, 6), (0x20, 2)]),
        (0x33, 5, 1150, &[(0x20, 2), (0x10, 1)]),
    ];

    #[test]
    fn wine_verify_version_table() {
        for (i, &(types, src, err, conds)) in WINE.iter().enumerate() {
            let mut v = OS_VERSION;
            match src {
                0 => v = OsVer::default(),
                2 => v.minor += 1,
                3 => v.sp_minor += 1,
                4 => v.sp_major += 1,
                5 => v.sp_major -= 1,
                6 => v.major -= 1,
                7 => v.build += 1,
                _ => {}
            }
            let mask = conds.iter().fold(0, |m, &(t, c)| ver_set_condition_mask(m, t, c));
            let got = verify_version(&OS_VERSION, &v, types, mask);
            let want = if err == 0 { Ok(()) } else { Err(VerFail::Mismatch) };
            assert_eq!(got, want, "case {i}");
        }
        // (the checks after the table)
        let mask = ver_set_condition_mask(ver_set_condition_mask(0, 1, 3), 2, 3);
        let mut v = OS_VERSION;
        (v.major, v.minor) = (3, 10);
        assert_eq!(verify_version(&OS_VERSION, &v, 0x33, mask), Ok(()));
        (v.minor, v.sp_major) = (0, 10);
        assert_eq!(verify_version(&OS_VERSION, &v, 0x33, mask), Ok(()));
        (v.sp_major, v.sp_minor) = (0, 10);
        assert_eq!(verify_version(&OS_VERSION, &v, 0x31, mask), Ok(()));
        assert_eq!(verify_version(&OS_VERSION, &v, 0, mask), Err(VerFail::BadArgs));
        assert_eq!(verify_version(&OS_VERSION, &v, 1, 0), Err(VerFail::BadArgs));
        assert_eq!(verify_version(&OS_VERSION, &v, 0x40, ver_set_condition_mask(0, 0x40, 1)), Err(VerFail::BadArgs));
    }


    #[test]
    fn condition_mask_slots() {
        // (VerSetConditionMask: the highest-priority type bit picks the slot; conditions are 3 bits)
        assert_eq!(ver_set_condition_mask(0, 2, 3), 3 << 3);
        assert_eq!(ver_set_condition_mask(0, 1, 0xff), 7);
        assert_eq!(ver_set_condition_mask(0, 0x80 | 1, 1), 1 << 21);
        assert_eq!(ver_set_condition_mask(5, 0, 1), 5);
        assert_eq!(ver_set_condition_mask(0, 0x20 | 0x10, 2), 2 << 15);
    }

    fn ver() -> impl Strategy<Value = OsVer> {
        (0u32..4, 0u32..4, 0u16..3, 0u16..3).prop_map(|(major, minor, sp_major, sp_minor)| OsVer { major, minor, sp_major, sp_minor, ..OS_VERSION })
    }

    proptest! {
        /// One condition for major, minor and the service pack numbers compares them as one
        /// number (Windows: "v3.10 is less than v4.x").
        #[test]
        fn same_condition_is_a_version_compare(cur in ver(), want in ver(), c in 1u8..=5) {
            let mask = [2u32, 1, 0x20, 0x10].iter().fold(0, |m, &t| ver_set_condition_mask(m, t, c));
            let key = |v: &OsVer| (v.major, v.minor, v.sp_major, v.sp_minor);
            let (a, b) = (key(&cur), key(&want));
            let want_ok = match c { 1 => a == b, 2 => a > b, 3 => a >= b, 4 => a < b, _ => a <= b };
            prop_assert_eq!(verify_version(&cur, &want, 0x33, mask).is_ok(), want_ok);
        }

        /// The other fields are compared on their own.
        #[test]
        fn single_fields(cur in 0u32..5, want in 0u32..5, c in 0u8..8, field in 0usize..3) {
            let (bit, set): (u32, fn(&mut OsVer, u32)) = [(4u32, (|v: &mut OsVer, x| v.build = x) as fn(&mut OsVer, u32)), (8, |v, x| v.platform = x), (0x80, |v, x| v.product = x as u8)][field];
            let (mut a, mut b) = (OS_VERSION, OS_VERSION);
            set(&mut a, cur);
            set(&mut b, want);
            let mask = ver_set_condition_mask(0, bit, c);
            let r = verify_version(&a, &b, bit, mask);
            if c == 0 {
                prop_assert_eq!(r, Err(VerFail::BadArgs));
            } else {
                let ok = match c { 1 => cur == want, 2 => cur > want, 3 => cur >= want, 4 => cur < want, 5 => cur <= want, _ => false };
                prop_assert_eq!(r.is_ok(), ok);
            }
        }
    }
}

#[cfg(test)]
mod atom_tests {
    use super::{atom_text, AtomArg, Atoms};

    #[test]
    fn names_integers_and_limits() {
        assert_eq!(atom_text("#1".into()), AtomArg::Int(1));
        assert_eq!(atom_text("#49151".into()), AtomArg::Int(0xbfff));
        assert_eq!(atom_text("#32770".into()), AtomArg::Int(32770));
        assert_eq!(atom_text("#49152".into()), AtomArg::Bad(87));
        assert_eq!(atom_text("#0".into()), AtomArg::Bad(87));
        assert_eq!(atom_text("#12a".into()), AtomArg::Name("#12a".into()));
        assert_eq!(atom_text("#".into()), AtomArg::Name("#".into()));
        assert_eq!(atom_text("a".repeat(255)), AtomArg::Name("a".repeat(255)));
        assert_eq!(atom_text("a".repeat(256)), AtomArg::Bad(87));
    }

    #[test]
    fn counted_case_blind_and_reused() {
        let mut t = Atoms(Vec::new());
        let a = t.add("foobar".into());
        assert_eq!(a, 0xc000);
        assert_eq!(t.add("FOOBAR".into()), a);
        assert_eq!(t.find("FooBar"), Some(0));
        assert_eq!(t.name(a).as_deref(), Some("foobar"));
        assert_eq!(t.name(7).as_deref(), Some("#7"));
        // (two references: the first delete keeps it)
        assert_eq!(t.delete(a), 0);
        assert_eq!(t.find("foobar"), Some(0));
        assert_eq!(t.delete(a), 0);
        assert_eq!(t.find("foobar"), None);
        assert_eq!(t.name(a), None);
        assert_eq!(t.delete(a), a);
        assert_eq!(t.delete(5), 0);
        // (the freed slot goes to the next name)
        assert_eq!(t.add("other".into()), 0xc000);
    }
}
