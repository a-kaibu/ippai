#![allow(non_snake_case)]
//! User APCs (QueueUserAPC): queued for a thread and run when that thread next waits
//! alertably (SleepEx, WaitFor*ObjectsEx, MsgWaitForMultipleObjectsEx with MWMO_ALERTABLE),
//! which then returns WAIT_IO_COMPLETION. mono queues one to interrupt a thread that is
//! blocked in such a wait (thread abort, GC handshakes).
use crate::api::Ctx;
use crate::obj::{self, Obj};
use std::sync::Mutex;

pub const WAIT_IO_COMPLETION: u32 = 0xc0;

/// (thread id, function, arguments, argument count), oldest first
static QUEUE: Mutex<Vec<(u32, u32, [u32; 4], usize)>> = Mutex::new(Vec::new());

/// Queues `f(arg)` for thread `tid` and wakes it
pub fn queue(tid: u32, f: u32, arg: u32) {
    queue_args(tid, f, &[arg]);
}

/// Queues `f(args...)` (up to four arguments: an IO completion routine's) for thread `tid`
pub fn queue_args(tid: u32, f: u32, args: &[u32]) {
    let mut a = [0; 4];
    a[..args.len()].copy_from_slice(args);
    QUEUE.lock().unwrap().push((tid, f, a, args.len()));
    obj::signal();
}

/// Whether `tid` has an APC waiting (safe to call with the object table locked: the queue's
/// lock is never held while taking that one)
pub fn pending(tid: u32) -> bool {
    QUEUE.lock().unwrap().iter().any(|&(t, ..)| t == tid)
}

fn take(tid: u32) -> Option<(u32, [u32; 4], usize)> {
    let mut q = QUEUE.lock().unwrap();
    let i = q.iter().position(|&(t, ..)| t == tid)?;
    let (_, f, a, n) = q.remove(i);
    Some((f, a, n))
}

/// Runs this thread's APCs, in order (including ones they queue); whether there were any
pub fn run(c: &mut Ctx) -> bool {
    let me = crate::cur().tid;
    let mut ran = false;
    while let Some((f, a, n)) = take(me) {
        // (NtQueueApcThread with no routine: only wakes the wait)
        if f != 0 {
            c.call(f, &a[..n]);
            ran = true;
        }
    }
    ran
}

/// A thread handle's state: Some(thread id, ended) for a thread, None for anything else
fn thread_of(h: u32) -> Option<(u32, bool)> {
    if h == obj::CURRENT_THREAD {
        return Some((crate::cur().tid, false));
    }
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Thread { tid, done, .. }, _)) => Some((*tid, *done || *tid == 0)),
        _ => None,
    })
}

/// An alertable WaitForMultipleObjects
pub fn wait(c: &mut Ctx, handles: &[u32], all: bool, ms: u32) -> u32 {
    if run(c) {
        return WAIT_IO_COMPLETION;
    }
    let me = crate::cur().tid;
    let r = obj::wait(handles, all, ms, Some(&|| pending(me)));
    if r == handles.len() as u32 && run(c) { WAIT_IO_COMPLETION } else { r }
}

/// QueueUserAPC(f, thread, arg): an ended thread is ERROR_GEN_FAILURE
pub fn QueueUserAPC(c: &mut Ctx) -> u32 {
    let (f, h, arg) = (c.a(0), c.a(1), c.a(2));
    match thread_of(h) {
        Some((tid, false)) if f != 0 => {
            queue(tid, f, arg);
            1
        }
        Some((_, true)) => {
            crate::k32::last_error(31);
            0
        }
        _ => {
            crate::k32::last_error(6); // ERROR_INVALID_HANDLE
            0
        }
    }
}

/// QueueUserAPC2(f, thread, arg, flags): QUEUE_USER_APC_FLAGS_SPECIAL_USER_APC (64-bit only)
/// runs it at once on the calling thread, as the thread would asynchronously
pub fn QueueUserAPC2(c: &mut Ctx) -> u32 {
    let (f, h, arg, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    match flags {
        0 => QueueUserAPC(c),
        1 if !crate::mem::x64() => {
            crate::k32::last_error(50); // ERROR_NOT_SUPPORTED
            0
        }
        1 => match thread_of(h) {
            Some((tid, false)) if tid == crate::cur().tid && f != 0 => {
                c.call(f, &[arg]);
                1
            }
            Some((tid, false)) if f != 0 => {
                queue(tid, f, arg);
                1
            }
            Some((_, true)) => {
                crate::k32::last_error(31);
                0
            }
            _ => {
                crate::k32::last_error(6);
                0
            }
        },
        _ => {
            crate::k32::last_error(87);
            0
        }
    }
}

/// NtQueueApcThread(thread, routine, arg1, arg2, arg3): STATUS_UNSUCCESSFUL for a thread that
/// ended, STATUS_INVALID_HANDLE for no thread
pub fn NtQueueApcThread(c: &mut Ctx) -> u32 {
    let (h, f) = (c.a(0), c.a(1));
    let args = [c.a(2), c.a(3), c.a(4)];
    match thread_of(h) {
        Some((_, true)) => 0xC000_0001,
        Some((tid, false)) => {
            queue_args(tid, f, &args);
            0
        }
        None => 0xC000_0008,
    }
}

/// NtTestAlert: the calling thread's APCs run now
pub fn NtTestAlert(c: &mut Ctx) -> u32 {
    run(c);
    0
}

/// Drops the APCs of a thread that ended
pub fn forget(tid: u32) {
    QUEUE.lock().unwrap().retain(|&(t, ..)| t != tid);
}
