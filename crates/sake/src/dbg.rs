//! Debugging other processes (WaitForDebugEvent, ContinueDebugEvent, DebugActiveProcess, ...)
//! and being debugged, between the processes of the shared kernel (crate::kern::debug keeps
//! who debugs whom and the events on their way).
//!
//! A debugged process reports, as Windows does: its start (CREATE_PROCESS, a LOAD_DLL for each
//! module loaded with it, then a breakpoint), threads starting and ending, DLLs loading,
//! OutputDebugString, and each exception before its handlers run (first chance) and again
//! if none takes it (second chance); the reporting thread waits for ContinueDebugEvent. A
//! debugger attaching (DebugActiveProcess) gets the process, its threads and its modules
//! as events, then a breakpoint from a new thread at DbgBreakPoint.
//!
//! An unhandled exception in a process nobody debugs starts AeDebug's debugger (Auto "1"),
//! with the process's id and an event it sets once it is ready, and waits for it.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::kern::debug::{Ev, DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED, DBG_REPLY_LATER};
use crate::kern::{self, PENDING};
use crate::mem::{self, ps, r32, w16, w32, wp};
use crate::obj::{self, Obj};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const EXCEPTION: u32 = 1;
pub const CREATE_THREAD: u32 = 2;
pub const CREATE_PROCESS: u32 = 3;
pub const EXIT_THREAD: u32 = 4;
pub const LOAD_DLL: u32 = 6;
pub const UNLOAD_DLL: u32 = 7;
pub const OUTPUT_DEBUG_STRING: u32 = 8;
const BREAKPOINT: u32 = 0x8000_0003;
const DBG_EXCEPTION_HANDLED: u32 = 0x0001_0001;

/// The process has told its debugger of its start (later loads and threads are reported)
static STARTED: AtomicBool = AtomicBool::new(false);

fn shared() -> bool {
    kern::host::shared()
}

/// Whether a debugger debugs this process
pub fn debugged() -> bool {
    shared() && kern::with(|k| k.dbg.debugger(crate::pid()).is_some()).0
}

/// Thread `tid` of this process was suspended (`on`) or is running again: while it is,
/// the debugger gets none of its events (as no suspended thread reports one on Windows)
pub fn hold(tid: u32, on: bool) {
    if debugged() {
        kern::with(|k| {
            k.changed();
            k.dbg.hold(tid, on)
        });
    }
}

/// Posts event `code` of thread `tid` and waits for the debugger to continue it: the status
/// (DBG_CONTINUE when no one debugs the process). `cpu`: the thread's, which a
/// SetThreadContext meanwhile changes.
fn report(code: u32, vals: Vec<u64>, data: Vec<u8>, cpu: Option<&mut koji::Cpu>) -> u32 {
    if !shared() {
        return DBG_CONTINUE;
    }
    let t = crate::cur();
    let ev = Ev { pid: crate::pid(), tid: t.tid, code, vals, data, wait: true, ..Ev::default() };
    let Some(id) = kern::with(|k| { k.changed(); k.dbg.post(ev) }).0 else { return DBG_CONTINUE };
    // (GetThreadContext of this thread meanwhile: where it stands, not in an API call)
    let api = std::mem::take(&mut t.api);
    if crate::env_on!("SAKE_TRACE_DBG") {
        eprintln!("sake: thread {:#x} reports event {code} ({id})", crate::cur().tid);
    }
    let r = wait(|k| { let r = k.dbg.take(id).ok_or(PENDING); if r.is_ok() { k.changed() } r });
    if crate::env_on!("SAKE_TRACE_DBG") {
        eprintln!("sake: thread {:#x}: event {id} continued with {r:x?}", crate::cur().tid);
    }
    crate::cur().api = api;
    if let Some(cpu) = cpu {
        crate::tctx::apply_pending(cpu);
    }
    r.unwrap_or(DBG_CONTINUE)
}

/// kern::block, rechecked now and then: a process's end reaches the kernel only when someone
/// looks (its debugger's end answers the events it had)
fn wait<T>(mut f: impl FnMut(&mut kern::Kern) -> Result<T, u32>) -> Result<T, u32> {
    loop {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(100);
        match kern::block(Some(until), &|| false, &mut f) {
            Err(kern::IO_TIMEOUT) => {}
            r => return r,
        }
    }
}

/// Posts an event no thread waits for (the watcher's, for an attaching debugger)
fn post(tid: u32, code: u32, vals: Vec<u64>) -> Option<u32> {
    kern::with(|k| {
        k.changed();
        k.dbg.post(Ev { pid: crate::pid(), tid, code, vals, ..Ev::default() })
    }).0
}

/// The threads an attaching debugger was told of, suspended until their event is continued
/// (or gone: detached, the debugger ended): (thread, event)
static ATTACH: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());

/// Thread `tid`'s attach event: the thread waits suspended for it
fn post_suspended(tid: u32, code: u32, vals: Vec<u64>) {
    let _ = crate::k32::suspend_count(tid, true);
    match post(tid, code, vals) {
        Some(id) => ATTACH.lock().unwrap().push((tid, id)),
        None => {
            let _ = crate::k32::suspend_count(tid, false);
        }
    }
}

/// The watcher's part (before it serves other processes' requests, so that a debugger that
/// continued an event sees the thread running): the threads whose attach event is done go on
pub fn watch_attach() {
    let mut g = ATTACH.lock().unwrap();
    if g.is_empty() {
        return;
    }
    let live: Vec<u32> = kern::with(|k| k.dbg.events.iter().map(|e| e.id).collect()).0;
    g.retain(|&(tid, id)| {
        let keep = live.contains(&id);
        if !keep {
            let _ = crate::k32::suspend_count(tid, false);
        }
        keep
    });
}

/// PEB BeingDebugged
fn set_being_debugged(on: bool) {
    let peb = crate::PEB.load(Ordering::Relaxed);
    if peb != 0 {
        mem::w8(peb + 2, on as u8);
    }
}

/// `int3; ret`: the addresses of DbgBreakPoint and of the loader's own breakpoint
fn int3_stub(at: &AtomicU32, code: &[u8]) -> u32 {
    match at.load(Ordering::Relaxed) {
        0 => {
            let a = crate::api::stub(code);
            at.store(a, Ordering::Relaxed);
            a
        }
        a => a,
    }
}
static BREAK_POINT: AtomicU32 = AtomicU32::new(0);
static LOADER_BREAK: AtomicU32 = AtomicU32::new(0);
static BREAKIN: AtomicU32 = AtomicU32::new(0);
pub fn break_point() -> u32 {
    int3_stub(&BREAK_POINT, &[0xcc, 0xc3])
}
/// DbgUiRemoteBreakin: calls DbgBreakPoint and ends (a breakpoint nobody handles there does
/// not take the process down)
fn breakin() -> u32 {
    if let a @ 1.. = BREAKIN.load(Ordering::Relaxed) {
        return a;
    }
    let bp = break_point();
    let code: Vec<u8> = if mem::x64() {
        vec![0x48, 0x83, 0xec, 0x28, 0xe8, 0, 0, 0, 0, 0x48, 0x83, 0xc4, 0x28, 0xc3]
    } else {
        vec![0xe8, 0, 0, 0, 0, 0xc3]
    };
    let a = crate::api::stub(&code);
    let call_at = if mem::x64() { a + 4 } else { a };
    w32(call_at + 1, bp.wrapping_sub(call_at + 5));
    BREAKIN.store(a, Ordering::Relaxed);
    a
}
/// Where a breakpoint returns to in the break-in thread
fn breakin_return() -> u32 {
    breakin() + if mem::x64() { 9 } else { 5 }
}

/// The exports that are code of ours rather than API calls (ntdll's DbgBreakPoint)
pub fn export(dll: &str, name: &str) -> Option<u32> {
    match (crate::api::dll_key(dll).as_str(), name) {
        ("ntdll", "DbgBreakPoint" | "DbgUserBreakPoint") => Some(break_point()),
        ("ntdll", "DbgUiRemoteBreakin") => Some(breakin()),
        _ => None,
    }
}

/// An EXCEPTION_RECORD in this process's layout
fn record(code: u32, flags: u32, addr: u64, params: &[u64]) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(&code.to_le_bytes());
    r.extend_from_slice(&flags.to_le_bytes());
    if mem::x64() {
        r.extend_from_slice(&0u64.to_le_bytes());
        r.extend_from_slice(&addr.to_le_bytes());
        r.extend_from_slice(&(params.len().min(15) as u32).to_le_bytes());
        r.extend_from_slice(&0u32.to_le_bytes());
        for i in 0..15 {
            r.extend_from_slice(&params.get(i).copied().unwrap_or(0).to_le_bytes());
        }
    } else {
        r.extend_from_slice(&0u32.to_le_bytes());
        r.extend_from_slice(&(addr as u32).to_le_bytes());
        r.extend_from_slice(&(params.len().min(15) as u32).to_le_bytes());
        for i in 0..15 {
            r.extend_from_slice(&(params.get(i).copied().unwrap_or(0) as u32).to_le_bytes());
        }
    }
    r
}

/// The loaded modules in load order, ntdll first, without the program's own
fn dlls() -> Vec<u32> {
    let exe = crate::pe::exe_base();
    let mut v: Vec<u32> = crate::pe::ldr_bases().into_iter().filter(|&b| b != exe).collect();
    let nt = crate::pe::by_name("ntdll.dll");
    if let Some(i) = nt.and_then(|n| v.iter().position(|&b| b == n)) {
        let n = v.remove(i);
        v.insert(0, n);
    }
    v
}

/// The first thread, before the DLLs initialize: the process's start (`base` its image,
/// `entry` its entry point) and the loader's breakpoint
pub fn process_start(cpu: &mut koji::Cpu, base: u32, entry: u32) {
    // (made in the same order in every process: DbgBreakPoint is at the same address in all,
    // as ntdll's is on Windows, and the loader's breakpoint elsewhere)
    break_point();
    int3_stub(&LOADER_BREAK, &[0xcc, 0xc3, 0x90]);
    if !debugged() {
        STARTED.store(true, Ordering::Release);
        return;
    }
    set_being_debugged(true);
    let teb = crate::cur().teb as u64;
    report(CREATE_PROCESS, vec![base as u64, teb, entry as u64], vec![], Some(cpu));
    for b in dlls() {
        report(LOAD_DLL, vec![b as u64, 0], vec![], Some(cpu));
    }
    STARTED.store(true, Ordering::Release);
    // (the loader's own breakpoint: whatever the debugger says, the program starts)
    let at = int3_stub(&LOADER_BREAK, &[0xcc, 0xc3, 0x90]);
    report(EXCEPTION, vec![1], record(BREAKPOINT, 0, at as u64, &[0]), Some(cpu));
}

/// A new thread, before it runs (`start` its start address)
pub fn thread_created(cpu: &mut koji::Cpu, start: u32) {
    if STARTED.load(Ordering::Acquire) && debugged() {
        report(CREATE_THREAD, vec![crate::cur().teb as u64, start as u64], vec![], Some(cpu));
    }
}

/// A thread ends with `code`
pub fn thread_exiting(code: u32) {
    if STARTED.load(Ordering::Acquire) && debugged() {
        report(EXIT_THREAD, vec![code as u64], vec![], None);
    }
}

/// Module `base` loaded (after the start)
pub fn dll_loaded(base: u32) {
    if STARTED.load(Ordering::Acquire) && crate::cur_opt().is_some() && debugged() {
        report(LOAD_DLL, vec![base as u64, 0], vec![], None);
    }
}

/// A DLL went (crate::pe::free_hle)
pub fn dll_unloaded(base: u32) {
    if STARTED.load(Ordering::Acquire) && crate::cur_opt().is_some() && debugged() {
        report(UNLOAD_DLL, vec![base as u64], vec![], None);
    }
}

/// OutputDebugString: the ANSI string at `p`, `len` bytes with its NUL
pub fn output(p: u32, len: u32) {
    if debugged() {
        report(OUTPUT_DEBUG_STRING, vec![p as u64, 0, len as u64], vec![], None);
    }
}

thread_local! {
    /// the exception being raised is an int3's (crate::lib's Stop::Int(3)), whatever the byte
    /// there is by the time it is reported (a debugger may have written it back already)
    static INT3: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
/// The next exception raised on this thread is an int3's
pub fn int3_hit() {
    INT3.with(|f| f.set(true));
}

/// An exception is raised (`cpu` where it happened): Some(()) when the debugger handled it
/// (`cpu` goes on, past a breakpoint's int3), None for the handlers to run
pub fn first_chance(cpu: &mut koji::Cpu, code: u32, flags: u32, addr: u64, params: &[u64]) -> Option<()> {
    // (the break-in thread's breakpoint never reaches handlers: it ends either way)
    let breakin = code == BREAKPOINT && BREAKIN.load(Ordering::Relaxed) != 0 && mem::rp(cpu.gpr(4) as u32) == breakin_return();
    let int3 = INT3.with(|f| f.replace(false));
    if !STARTED.load(Ordering::Acquire) || !debugged() {
        return None.or(breakin.then(|| cpu.eip = cpu.eip.wrapping_add(1)));
    }
    // (the debugger sees an int3's EIP after it; handlers see it at the int3)
    let pc = cpu.eip as u64 | (cpu.rip_hi as u64) << 32;
    let bp = code == BREAKPOINT && pc == addr && int3;
    // (int 2d: a debugger that continues it skips the byte after it)
    let service = code == BREAKPOINT && addr >= 2 && mem::r8(addr as u32 - 2) == 0xcd && mem::r8(addr as u32 - 1) == 0x2d;
    if bp {
        cpu.eip = cpu.eip.wrapping_add(1);
    }
    let st = report(EXCEPTION, vec![1], record(code, flags, addr, params), Some(&mut *cpu));
    if st == DBG_EXCEPTION_NOT_HANDLED && !breakin {
        if bp && cpu.eip == (addr as u32).wrapping_add(1) {
            cpu.eip = cpu.eip.wrapping_sub(1);
        }
        return None;
    }
    if service {
        cpu.eip = (addr as u32).wrapping_add(1);
    }
    Some(())
}

/// No handler took the exception: the debugger's second chance (Some: it continued, the
/// context goes on), else AeDebug's debugger if there is one, then its second chance
pub fn last_chance(cpu: &mut koji::Cpu, code: u32, flags: u32, addr: u64, params: &[u64]) -> Option<()> {
    if !debugged() && !ae_debug() {
        return None;
    }
    let st = report(EXCEPTION, vec![0], record(code, flags, addr, params), Some(cpu));
    (st == DBG_CONTINUE && debugged()).then_some(())
}

/// AeDebug: starts the registered debugger (Auto "1") for this process and waits until it
/// set the event it was given or ended; whether a debugger debugs this process now
fn ae_debug() -> bool {
    const KEY: &str = "hklm\\software\\microsoft\\windows nt\\currentversion\\aedebug";
    let auto = crate::misc::registry_string(KEY, "Auto");
    if crate::env_on!("SAKE_TRACE_DBG") {
        eprintln!("sake: AeDebug Auto {auto:?}, debugger {:?}", crate::misc::registry_string(KEY, "Debugger"));
    }
    let me = crate::pid();
    // (a debugger AeDebug started that crashes itself starts no other: no chain of them)
    if !shared() || auto.as_deref() != Some("1") || kern::with(|k| k.dbg.ae.contains_key(&me)).0 {
        return false;
    }
    let Some(cmd) = crate::misc::registry_string(KEY, "Debugger") else { return false };
    let ev = obj::add(Obj::Event { manual: true, set: false });
    obj::set_inherit(ev, true);
    crate::process::share_handle(ev);
    let mut args = [crate::pid() as u64, ev as u64].into_iter();
    let cmd = fill_ld(&cmd, &mut args);
    let (prog, rest) = crate::process::split_command(&cmd);
    let Ok(dpid) = crate::process::spawn(&prog, false, &rest, Some(cmd.clone()), None, true, 0, None) else { return false };
    kern::with(|k| {
        k.changed();
        k.dbg.ae.insert(dpid, true)
    });
    // (the event, or the debugger's end)
    let Some(kern::KRef::Sync { id }) = crate::process::share_handle(ev) else { return false };
    let _ = wait(|k| {
        let set = matches!(k.sync_state(id), Some(kern::Sync::Event { set: true, .. }));
        if set || k.exit_code(dpid).is_some() { Ok(()) } else { Err(PENDING) }
    });
    debugged()
}

/// The command line with each %ld (or %lu, %d, %x...) given the next number
pub fn fill_ld(fmt: &str, args: &mut impl Iterator<Item = u64>) -> String {
    let mut out = String::new();
    let mut it = fmt.chars().peekable();
    while let Some(ch) = it.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        if it.peek() == Some(&'%') {
            it.next();
            out.push('%');
            continue;
        }
        let mut spec = String::new();
        while let Some(&n) = it.peek() {
            it.next();
            if n.is_ascii_alphabetic() && n != 'l' && n != 'h' && n != 'I' {
                spec.push(n);
                break;
            }
            spec.push(n);
        }
        match spec.chars().last() {
            Some('d' | 'u' | 'i') => out += &args.next().unwrap_or(0).to_string(),
            Some('x') => out += &format!("{:x}", args.next().unwrap_or(0)),
            Some('X') => out += &format!("{:X}", args.next().unwrap_or(0)),
            _ => {
                out.push('%');
                out += &spec;
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------------------------
// the debuggee's watcher: what an attaching debugger asks of it (crate::xmem)

/// A debugger attached: the process, its threads and modules as events, then a thread that
/// breaks in
pub fn attached() {
    if crate::env_on!("SAKE_TRACE_DBG") {
        eprintln!("sake: process {:#x}: a debugger attached", crate::pid());
    }
    set_being_debugged(true);
    let main = crate::process::first_tid(crate::pid());
    let threads = crate::k32::thread_list();
    let teb = |tid: u32| threads.iter().find(|t| t.0 == tid).map_or(0, |t| t.1) as u64;
    let exe = crate::pe::exe_base();
    let entry = crate::pe::with(exe, |m| m.entry).unwrap_or(0);
    post_suspended(main, CREATE_PROCESS, vec![exe as u64, teb(main), entry as u64]);
    let mut dlls = dlls().into_iter();
    if let Some(nt) = dlls.next() {
        post(main, LOAD_DLL, vec![nt as u64, 0]);
    }
    for &(tid, teb, _) in threads.iter().filter(|t| t.0 != main) {
        post_suspended(tid, CREATE_THREAD, vec![teb as u64, 0]);
    }
    for b in dlls {
        post(main, LOAD_DLL, vec![b as u64, 0]);
    }
    STARTED.store(true, Ordering::Release);
    break_in();
}
/// DebugBreakProcess's part here: a new thread breaks in
pub fn break_in() {
    crate::k32::thread_for_other(breakin(), 0, 0, 0);
}
pub fn detached() {
    set_being_debugged(false);
}

// ---------------------------------------------------------------------------------------------
// the debugger's APIs

fn process_of(h: u32) -> Option<u32> {
    crate::process::of_handle(h).map(|(pid, _)| pid)
}

/// DEBUG_EVENT from event `e` (handles to its process and thread made here)
fn fill(de: u32, e: &Ev) {
    let p = ps();
    let x64 = mem::x64();
    let u = de + if x64 { 16 } else { 12 };
    mem::fill(de, if x64 { 176 } else { 96 }, 0);
    w32(de, e.code);
    w32(de + 4, e.pid);
    w32(de + 8, e.tid);
    let v = |i: usize| e.vals.get(i).copied().unwrap_or(0) as u32;
    let thread = || obj::add(Obj::RemoteThread { pid: e.pid, tid: e.tid });
    match e.code {
        EXCEPTION => {
            mem::write(u, &e.data);
            w32(u + if x64 { 0x98 } else { 0x50 }, v(0));
        }
        CREATE_THREAD => {
            wp(u, thread());
            wp(u + p, v(0));
            wp(u + 2 * p, v(1));
        }
        CREATE_PROCESS => {
            wp(u + p, obj::add(Obj::Process { access: crate::process::PROCESS_ALL_ACCESS, pid: e.pid }));
            wp(u + 2 * p, thread());
            wp(u + 3 * p, v(0));
            let (tls, start, name, uni) = if x64 { (40, 48, 56, 64) } else { (24, 28, 32, 36) };
            wp(u + tls, v(1));
            wp(u + start, v(2));
            wp(u + name, 0);
            w16(u + uni, 1);
        }
        4 | 5 => w32(u, v(0)),
        LOAD_DLL => {
            wp(u + p, v(0));
            let (name, uni) = if x64 { (24, 32) } else { (16, 20) };
            wp(u + name, v(1));
            w16(u + uni, 1);
        }
        UNLOAD_DLL => wp(u, v(0)),
        OUTPUT_DEBUG_STRING => {
            wp(u, v(0));
            w16(u + p, v(1) as u16);
            w16(u + p + 2, v(2) as u16);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// debug objects: a thread's own is in TEB DbgSsReserved[1] (DbgUiGetThreadDebugObject)

fn teb_port_at() -> u32 {
    crate::cur().teb + if mem::x64() { 0x16a8 } else { 0xf20 }
}
/// The calling thread's debug object handle (0: none)
fn thread_port_handle() -> u32 {
    mem::rp(teb_port_at())
}
/// The debug port of handle `h`
fn port_of(h: u32) -> Option<u32> {
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::DebugObject(p), _)) => Some(*p),
        _ => None,
    })
}
/// The calling thread's debug port
fn thread_port() -> Option<u32> {
    port_of(thread_port_handle())
}
/// A new debug object of this process: its handle
fn create_port(kill: bool) -> u32 {
    crate::process::watch();
    let me = crate::pid();
    let port = kern::with(|k| {
        k.changed();
        k.dbg.create_port(me, kill)
    }).0;
    obj::add(Obj::DebugObject(port))
}
/// DbgUiConnectToDbg: the calling thread's debug port, made (DEBUG_KILL_ON_CLOSE) if it had none
pub fn connect() -> Option<u32> {
    if !kern::share() {
        return None;
    }
    if let Some(p) = thread_port() {
        return Some(p);
    }
    let h = create_port(true);
    mem::wp(teb_port_at(), h);
    port_of(h)
}
/// The last handle of debug object `port` was closed: its debuggees go on, or end
/// (DEBUG_KILL_ON_CLOSE)
pub fn port_closed(port: u32) {
    if !shared() {
        return;
    }
    kern::with(|k| {
        k.changed();
        for p in k.dbg.close_port(port) {
            k.terminate(p, crate::kern::debug::STATUS_DEBUGGER_INACTIVE);
        }
    });
}
/// A thread ends: its debug object handle closes (as Windows does)
pub fn thread_port_closes() {
    let h = thread_port_handle();
    if h != 0 && port_of(h).is_some() {
        mem::wp(teb_port_at(), 0);
        obj::close(h);
    }
}

fn DbgUiConnectToDbg(_: &mut Ctx) -> u32 {
    if connect().is_some() { 0 } else { 0xC000_0001 }
}
fn DbgUiGetThreadDebugObject(_: &mut Ctx) -> u32 {
    thread_port_handle()
}
fn DbgUiSetThreadDebugObject(c: &mut Ctx) -> u32 {
    mem::wp(teb_port_at(), c.a(0));
    0
}
const STATUS_INVALID_HANDLE: u32 = 0xC000_0008;
const STATUS_INVALID_PARAMETER: u32 = 0xC000_000D;
/// NtCreateDebugObject(&handle, access, attributes, flags): DEBUG_KILL_ON_CLOSE (1) only
fn NtCreateDebugObject(c: &mut Ctx) -> u32 {
    let (out, flags) = (c.a(0), c.a(3));
    if flags & !1 != 0 {
        return STATUS_INVALID_PARAMETER;
    }
    if !kern::share() {
        return 0xC000_0001;
    }
    mem::wp(out, create_port(flags & 1 != 0));
    0
}
/// NtSetInformationDebugObject(handle, DebugObjectKillProcessOnExitInformation (1), &flags,
/// 4, &length)
fn NtSetInformationDebugObject(c: &mut Ctx) -> u32 {
    let (h, class, buf, len, out) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    if class != 1 {
        return 0xC000_0003; // STATUS_INVALID_INFO_CLASS
    }
    if len != 4 {
        if out != 0 {
            w32(out, 4);
        }
        return 0xC000_0004; // STATUS_INFO_LENGTH_MISMATCH
    }
    let v = r32(buf);
    if v & !1 != 0 {
        return STATUS_INVALID_PARAMETER;
    }
    let Some(port) = port_of(h) else { return STATUS_INVALID_HANDLE };
    kern::with(|k| {
        k.changed();
        k.dbg.set_kill(port, v != 0)
    });
    if out != 0 {
        w32(out, 0);
    }
    0
}

/// WaitForDebugEvent(event, ms) / WaitForDebugEventEx: ERROR_SEM_TIMEOUT when none came in
/// time, ERROR_INVALID_HANDLE for a thread without a debug object
fn WaitForDebugEvent(c: &mut Ctx) -> u32 {
    let (de, ms) = (c.a(0), c.a(1));
    let me = crate::pid();
    let Some(port) = thread_port().filter(|_| shared()) else {
        last_error(6);
        return 0;
    };
    let until = (ms != u32::MAX).then(|| std::time::Instant::now() + std::time::Duration::from_millis(ms as u64));
    let got = loop {
        // (in slices: a debuggee's end reaches the kernel only when someone looks)
        let slice = std::time::Instant::now() + std::time::Duration::from_millis(100);
        let end = until.map_or(slice, |u| u.min(slice));
        match kern::block(Some(end), &|| false, |k| { let r = k.dbg.next(port).ok_or(PENDING); if r.is_ok() { k.changed() } r }) {
            Err(kern::IO_TIMEOUT) if until.is_none_or(|u| std::time::Instant::now() < u) => crate::k32::check_terminate(),
            r => break r,
        }
    };
    match got {
        Ok(e) => {
            if crate::env_on!("SAKE_TRACE_DBG") {
                eprintln!("sake: process {me:#x} gets event {} ({}) of {:#x}/{:#x}", e.code, e.id, e.pid, e.tid);
            }
            fill(de, &e);
            1
        }
        Err(_) => {
            last_error(121);
            0
        }
    }
}

fn ContinueDebugEvent(c: &mut Ctx) -> u32 {
    let (pid, tid, st) = (c.a(0), c.a(1), c.a(2));
    if ![DBG_CONTINUE, DBG_EXCEPTION_NOT_HANDLED, DBG_REPLY_LATER, DBG_EXCEPTION_HANDLED].contains(&st) {
        last_error(87);
        return 0;
    }
    let st = if st == DBG_EXCEPTION_HANDLED { DBG_CONTINUE } else { st };
    if crate::env_on!("SAKE_TRACE_DBG") {
        eprintln!("sake: process {:#x} continues {pid:#x}/{tid:#x} with {st:#x}", crate::pid());
    }
    let Some(port) = thread_port().filter(|_| shared()) else {
        last_error(6);
        return 0;
    };
    if !kern::with(|k| { k.changed(); k.dbg.cont(port, pid, tid, st) }).0 {
        last_error(87);
        return 0;
    }
    1
}

fn DebugActiveProcess(c: &mut Ctx) -> u32 {
    let pid = c.a(0);
    let me = crate::pid();
    if pid == me || !kern::share() {
        last_error(5);
        return 0;
    }
    let Some(port) = connect() else {
        last_error(5);
        return 0;
    };
    let r = kern::with(|k| if k.known(pid) && k.exit_code(pid).is_none() { k.changed(); k.dbg.attach(port, me, pid) } else { Err(87) }).0;
    if let Err(e) = r {
        last_error(e);
        return 0;
    }
    if crate::env_on!("SAKE_TRACE_DBG") {
        eprintln!("sake: process {me:#x} attaches to {pid:#x}");
    }
    if crate::xmem::call(pid, crate::xmem::DEBUG_ATTACH, vec![], vec![]).is_none() {
        let _ = kern::with(|k| { k.changed(); k.dbg.detach(port, pid) });
        last_error(87);
        return 0;
    }
    1
}

fn DebugActiveProcessStop(c: &mut Ctx) -> u32 {
    let pid = c.a(0);
    let Some(port) = thread_port().filter(|_| shared()) else {
        last_error(87);
        return 0;
    };
    if let Err(e) = kern::with(|k| { k.changed(); k.dbg.detach(port, pid) }).0 {
        last_error(e);
        return 0;
    }
    crate::xmem::call(pid, crate::xmem::DEBUG_DETACH, vec![], vec![]);
    1
}

/// DebugSetProcessKillOnExit: the calling thread's debug object's DEBUG_KILL_ON_CLOSE
fn DebugSetProcessKillOnExit(c: &mut Ctx) -> u32 {
    let kill = c.a(0) != 0;
    let Some(port) = thread_port().filter(|_| shared()) else {
        last_error(6);
        return 0;
    };
    kern::with(|k| { k.changed(); k.dbg.set_kill(port, kill) });
    1
}

/// Whether a debugger debugs process `pid`
pub fn debugged_pid(pid: u32) -> bool {
    shared() && kern::with(|k| k.dbg.debugger(pid).is_some()).0
}

fn CheckRemoteDebuggerPresent(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    let Some(pid) = process_of(h).filter(|_| out != 0) else {
        last_error(87);
        return 0;
    };
    w32(out, debugged_pid(pid) as u32);
    1
}

fn IsDebuggerPresent(_: &mut Ctx) -> u32 {
    let peb = crate::PEB.load(Ordering::Relaxed);
    if peb == 0 { 0 } else { mem::r8(peb + 2) as u32 }
}

fn DebugBreakProcess(c: &mut Ctx) -> u32 {
    let Some(pid) = process_of(c.a(0)) else {
        last_error(6);
        return 0;
    };
    if crate::process::is_self(pid) {
        break_in();
        return 1;
    }
    if crate::xmem::call(pid, crate::xmem::DEBUG_BREAK, vec![], vec![]).is_none() {
        last_error(5);
        return 0;
    }
    1
}

/// DebugBreak: a breakpoint at the call when debugged (else nothing)
fn DebugBreak(c: &mut Ctx) -> u32 {
    if debugged() {
        crate::k32::raise_from_api(c, BREAKPOINT, 0, 0);
    }
    0
}

pub fn init() {
    for dll in ["kernel32", "kernelbase"] {
        implement(dll, "WaitForDebugEvent", WaitForDebugEvent);
        implement(dll, "WaitForDebugEventEx", WaitForDebugEvent);
        implement(dll, "ContinueDebugEvent", ContinueDebugEvent);
        implement(dll, "DebugActiveProcess", DebugActiveProcess);
        implement(dll, "DebugActiveProcessStop", DebugActiveProcessStop);
        implement(dll, "DebugSetProcessKillOnExit", DebugSetProcessKillOnExit);
        implement(dll, "CheckRemoteDebuggerPresent", CheckRemoteDebuggerPresent);
        implement(dll, "IsDebuggerPresent", IsDebuggerPresent);
        implement(dll, "DebugBreakProcess", DebugBreakProcess);
        implement(dll, "DebugBreak", DebugBreak);
    }
    implement("ntdll", "DbgUiConnectToDbg", DbgUiConnectToDbg);
    implement("ntdll", "DbgUiGetThreadDebugObject", DbgUiGetThreadDebugObject);
    implement("ntdll", "DbgUiSetThreadDebugObject", DbgUiSetThreadDebugObject);
    for n in ["NtCreateDebugObject", "ZwCreateDebugObject"] {
        implement("ntdll", n, NtCreateDebugObject);
    }
    for n in ["NtSetInformationDebugObject", "ZwSetInformationDebugObject"] {
        implement("ntdll", n, NtSetInformationDebugObject);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn aedebug_command_lines() {
        let f = |s: &str| fill_ld(s, &mut [0x10010u64, 0x44].into_iter());
        assert_eq!(f("dbg.exe debugger dbg,event \"log\" %ld %ld"), "dbg.exe debugger dbg,event \"log\" 65552 68");
        assert_eq!(f("winedbg --auto %ld %ld"), "winedbg --auto 65552 68");
        assert_eq!(f("x -p %ld -e %ld -g"), "x -p 65552 -e 68 -g");
        assert_eq!(f("100%% %lx"), "100% 10010");
        assert_eq!(f("%s %ld"), "%s 65552");
    }

    proptest! {
        /// Text without '%' passes unchanged, and every %ld takes the next number in turn
        #[test]
        fn fill_ld_keeps_text_and_numbers(parts in proptest::collection::vec("[^%]{0,6}", 1..5), nums in proptest::collection::vec(any::<u32>(), 4)) {
            let fmt = parts.join("%ld");
            let mut it = nums.iter().map(|&n| n as u64);
            let got = fill_ld(&fmt, &mut it);
            let mut want = parts[0].clone();
            for (i, p) in parts.iter().enumerate().skip(1) {
                want += &nums[i - 1].to_string();
                want += p;
            }
            prop_assert_eq!(got, want);
        }
    }
}
