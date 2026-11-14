//! Save states, sake's part (the mechanism is crates/kura/src/snap.rs, docs/savestate.md).
//!
//! A guest thread stands still for a state only where its host stack holds nothing that a
//! restored copy could not build again:
//!
//! * at the top level of its run loop, between blocks (`run` with no host call under it):
//!   it goes on there ([`CONTINUE`]);
//! * waiting inside an API called from that top level, if the API may simply be called again
//!   (the waits of [`RESTARTABLE`]: nothing done yet that a second call would do twice): a
//!   restored copy calls it again ([`RESTART`]);
//! * in a condition variable's wait (a wake without a signal is allowed): a restored copy takes
//!   the lock again and returns as woken ([`RETURN`]);
//! * created suspended and not started yet ([`START`]).
//!
//! Anywhere else (a window procedure waiting for something, a dialog's loop...) the thread
//! just goes on, and the save waits for it (and fails after a while, saying where it was).

use std::cell::Cell;
use std::sync::atomic::Ordering;

use koji::{Cpu, EAX, ESP};
use kura::snap::Part;

use crate::mem::{self, r32};
use crate::Thread;

pub const CONTINUE: u64 = 0;
pub const RESTART: u64 = 1;
pub const RETURN: u64 = 2;
pub const START: u64 = 3;

thread_local! {
    /// `run` calls nested on this thread (1: the top level)
    static DEPTH: Cell<u32> = const { Cell::new(0) };
    /// the API called from the top level now: trap address, ESP at the call (0: none)
    static API: Cell<(u32, u32)> = const { Cell::new((0, 0)) };
}

/// APIs that may be called again from the start when a restored copy finds a thread waiting
/// in them (their waits come before anything they do; a wait's time starts over).
pub const RESTARTABLE: &[&str] = &[
    "kernel32!Sleep",
    "kernel32!SleepEx",
    "kernelbase!Sleep",
    "kernelbase!SleepEx",
    "kernel32!WaitForSingleObject",
    "kernel32!WaitForSingleObjectEx",
    "kernel32!WaitForMultipleObjects",
    "kernel32!WaitForMultipleObjectsEx",
    "kernelbase!WaitForSingleObject",
    "kernelbase!WaitForSingleObjectEx",
    "kernelbase!WaitForMultipleObjects",
    "kernelbase!WaitForMultipleObjectsEx",
    "kernel32!EnterCriticalSection",
    "ntdll!RtlEnterCriticalSection",
    "kernel32!WaitOnAddress",
    "kernelbase!WaitOnAddress",
    "kernel32!SleepConditionVariableCS",
    "kernel32!SleepConditionVariableSRW",
    "kernelbase!SleepConditionVariableCS",
    "kernelbase!SleepConditionVariableSRW",
    "kernel32!AcquireSRWLockExclusive",
    "kernel32!AcquireSRWLockShared",
    "ntdll!RtlAcquireSRWLockExclusive",
    "ntdll!RtlAcquireSRWLockShared",
    "user32!GetMessageA",
    "user32!GetMessageW",
    "user32!WaitMessage",
    "user32!MsgWaitForMultipleObjects",
    "user32!MsgWaitForMultipleObjectsEx",
    "ntdll!NtDelayExecution",
    "ntdll!NtWaitForSingleObject",
    "ntdll!NtWaitForMultipleObjects",
    // the threads behind timers, sound devices and thread pools: their trap is the thread's
    // whole life, a loop that starts over as well as it goes on
    "sake!mm_timer_thread",
    "sake!tp_timer_thread",
    "sake!tp_wait_thread",
    "sake!wave_thread",
    "sake!xaudio2_thread",
];

fn restartable(trap: u32) -> bool {
    let Some(n) = crate::api::name_of(trap) else { return false };
    let n = n.to_ascii_lowercase();
    RESTARTABLE.iter().any(|r| r.eq_ignore_ascii_case(&n))
        // (msvcp's condition variables: their waits are cv_wait's)
        || n.contains("!_cnd_wait")
        || n.contains("!_cnd_timedwait")
}

/// A thread's outermost run that the host called to return to it (`run` with `until`: a
/// DLL's init or a TLS callback before the program's entry): its depth is this much more, so
/// that nothing in it is the top level (a restored copy has no host to return to).
const HOST_CALL: u32 = 1 << 16;

/// `run` begins (`base`: the thread's own run, which never returns to the host) / ends; the
/// depth (1: the top level).
#[inline]
pub fn enter_run(base: bool) -> u32 {
    DEPTH.with(|d| {
        let n = d.get() + if d.get() == 0 && !base { HOST_CALL + 1 } else { 1 };
        d.set(n);
        n
    })
}
#[inline]
pub fn leave_run() {
    DEPTH.with(|d| d.set(if d.get() == HOST_CALL + 1 { 0 } else { d.get().saturating_sub(1) }));
}

/// Before each run of guest code: whether the run loop may stop there for a state.
#[inline]
pub fn before_guest(cpu: &Cpu, depth: u32) {
    kura::snap::set_parkable(depth == 1 && cpu.mode16 == 0 && (!kura::snap::pausing() || clean()));
}

/// No exception being handled on this thread (its record is on the host's side).
fn clean() -> bool {
    crate::cxx::idle() && crate::cxx64::idle() && crate::seh64::idle()
}

/// An API is called (`dispatch`): the top level's is remembered.
#[inline]
pub fn api_enter(trap: u32, esp: u32) -> bool {
    let top = DEPTH.with(|d| d.get()) == 1;
    if top {
        API.with(|a| a.set((trap, esp)));
        if let Some(p) = kura::snap::me() {
            p.doing.store(trap, Ordering::Relaxed);
        }
    }
    top
}
#[inline]
pub fn api_leave() {
    API.with(|a| a.set((0, 0)));
    if let Some(p) = kura::snap::me() {
        p.doing.store(0, Ordering::Relaxed);
    }
}

/// The thread's record.
fn me() -> Option<(&'static Part, &'static mut Thread)> {
    let p = kura::snap::me()?;
    let t = p.user.load(Ordering::Relaxed) as *mut Thread;
    (!t.is_null()).then(|| (p, unsafe { &mut *t }))
}

/// The top-level API this thread waits in, if a restored copy may call it again.
fn restart_point() -> Option<(u32, u32)> {
    if !kura::snap::pausing() || DEPTH.with(|d| d.get()) != 1 || !clean() {
        return None;
    }
    let (trap, esp) = API.with(|a| a.get());
    (trap != 0 && restartable(trap)).then_some((trap, esp))
}

fn park_as(kind: u64, a: u64, b: u64, c: u64) {
    let Some((p, t)) = me() else { return };
    save_tls(t);
    for (i, v) in [kind, a, b, c].into_iter().enumerate() {
        p.resume[i].store(v, Ordering::SeqCst);
    }
    kura::snap::park();
    p.resume[0].store(CONTINUE, Ordering::SeqCst);
}

/// A restartable wait, holding no lock: stands still if a state is being taken (a restored
/// copy calls the API again). Returns whether it did (the caller looks again at what it waits for).
pub fn point() -> bool {
    let Some((trap, esp)) = restart_point() else { return false };
    park_as(RESTART, trap as u64, esp as u64, 0);
    true
}

/// A condition variable's wait (the lock released): stands still if a state is being taken;
/// a restored copy calls `reacquire(a, b)` and returns `value` from the API.
pub fn point_return(value: u32, reacquire: fn(u64, u64), a: u64, b: u64) -> bool {
    let Some((trap, esp)) = restart_point() else { return false };
    // (the action and its arguments: in the record's extra words)
    let extra = Box::leak(Box::new([reacquire as usize as u64, a, b]));
    park_as(RETURN, trap as u64 | (esp as u64) << 32, value as u64, extra.as_ptr() as usize as u64);
    drop(unsafe { Box::from_raw(extra) });
    true
}

/// The run loop's top level stopped for a state (kura's Stop::Pause).
pub fn park_here() {
    park_as(CONTINUE, 0, 0, 0);
}

/// A thread created suspended, waiting for its first ResumeThread: stands still if asked
/// (a restored copy waits for it again, then starts).
pub fn point_start(start: u32, param: u32) -> bool {
    if !kura::snap::pausing() {
        return false;
    }
    park_as(START, start as u64, param as u64, 1);
    true
}

// ---------------------------------------------------------------------------------------------
// the thread-local state a guest thread keeps on the host side

fn save_tls(t: &mut Thread) {
    t.tls = [
        crate::gl::current_pair().0,
        crate::gl::current_pair().1,
        kura::gl::current(),
        crate::k32::thread_error_mode(),
        crate::k32::fs_redirection_off(),
        0,
        0,
        0,
    ];
}

fn load_tls(t: &Thread) {
    crate::gl::set_current_pair((t.tls[0], t.tls[1]));
    kura::gl::set_current(t.tls[2]);
    crate::k32::set_thread_error_mode(t.tls[3]);
    crate::k32::set_fs_redirection_off(t.tls[4]);
}

// ---------------------------------------------------------------------------------------------
// waking sleepers when a pause starts, and what a state holds

/// Once per module (sake's main and restore): the wakers and the guest's memory ranges.
pub fn init() {
    kura::snap::on_pause(crate::obj::signal);
    kura::snap::on_pause(crate::k32::snap_wake);
    kura::snap::on_pause(crate::sound::snap_wake);
    kura::snap::on_pause(crate::xaudio2::snap_wake);
    kura::snap::on_pause(crate::wasapi::snap_wake);
    kura::snap::on_pause(crate::dshow_play::snap_wake);
    kura::snap::on_pause(crate::mf::snap_wake);
    kura::snap::set_ranges(crate::mem::snap_ranges);
    // (the kernel other processes share is outside this memory, host/vnkern.js)
    kura::snap::set_refuse(|| crate::kern::host::shared().then(|| "other processes run with this one".into()));
    kura::snap::set_namer(|trap| crate::api::name_of(trap).unwrap_or_else(|| format!("{trap:#x}")));
    kura::snap::start();
}

// ---------------------------------------------------------------------------------------------
// restoring

/// sake's main in a restored module: every guest thread starts again where it stood.
pub fn restore() -> ! {
    let parts = kura::snap::restore_begin();
    init();
    eprintln!("snap: restoring {} threads", parts.len());
    crate::sound::snap_restore();
    crate::wasapi::snap_restore();
    crate::user::snap_restore();
    crate::dshow_play::snap_restore();
    let mut main = None;
    for p in parts {
        let t = p.user.load(Ordering::SeqCst) as *mut Thread;
        if t.is_null() {
            continue;
        }
        let resume: [u64; 4] = std::array::from_fn(|i| p.resume[i].load(Ordering::SeqCst));
        let t: &'static mut Thread = unsafe { &mut *t };
        if t.tid == 0x20 {
            main = Some((t, resume));
            continue;
        }
        let (tt, r) = (t as *mut Thread as usize, resume);
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || go_on(unsafe { &mut *(tt as *mut Thread) }, r))
            .expect("thread");
    }
    match main {
        Some((t, r)) => go_on(t, r),
        // (the main thread had ended: the others go on, the process with them)
        None => loop {
            std::thread::park();
        },
    }
}

/// A restored guest thread goes on from its record.
fn go_on(t: &'static mut Thread, r: [u64; 4]) -> ! {
    crate::adopt(t as *mut Thread);
    load_tls(t);
    let cpu = unsafe { &mut *t.cpu };
    match r[0] {
        RESTART => {
            cpu.eip = r[1] as u32;
            cpu.rip_hi = 0;
            set_sp(cpu, r[2] as u32);
        }
        RETURN => {
            let (trap, esp) = (r[1] as u32, (r[1] >> 32) as u32);
            let extra = unsafe { *(r[3] as usize as *const [u64; 3]) };
            let f: fn(u64, u64) = unsafe { std::mem::transmute(extra[0] as usize) };
            f(extra[1], extra[2]);
            return_from(cpu, trap, esp, r[2] as u32);
        }
        START => {
            crate::k32::wait_gate(t.obj);
            crate::thread_start(t, r[1] as u32, r[2] as u32);
        }
        _ => {}
    }
    crate::run(cpu, 0);
    unsafe { libc::exit(0) }
}

fn set_sp(cpu: &mut Cpu, sp: u32) {
    if mem::x64() {
        cpu.set_gpr(ESP, sp as u64);
    } else {
        cpu.regs[ESP] = sp;
    }
}

/// Returns from the API at `trap` called with ESP `esp` with `value`, as dispatch does.
fn return_from(cpu: &mut Cpu, trap: u32, esp: u32, value: u32) {
    if mem::x64() {
        cpu.set_gpr(EAX, value as u64);
        let ret = mem::r64(esp);
        cpu.eip = ret as u32;
        cpu.rip_hi = (ret >> 32) as u32;
        cpu.set_gpr(ESP, esp as u64 + 8);
        return;
    }
    let (cdecl, bytes) = crate::api::args_of(trap);
    cpu.regs[EAX] = value;
    cpu.eip = r32(esp);
    cpu.regs[ESP] = esp + 4 + if cdecl { 0 } else { bytes };
}
