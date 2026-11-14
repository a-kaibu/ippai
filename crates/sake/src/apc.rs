#![allow(non_snake_case)]
//! User APCs (QueueUserAPC): queued for a thread and run when that thread next waits
//! alertably (SleepEx, WaitFor*ObjectsEx, MsgWaitForMultipleObjectsEx with MWMO_ALERTABLE),
//! which then returns WAIT_IO_COMPLETION. mono queues one to interrupt a thread that is
//! blocked in such a wait (thread abort, GC handshakes).
use crate::api::Ctx;
use crate::obj::{self, Obj};
use std::sync::Mutex;

pub const WAIT_IO_COMPLETION: u32 = 0xc0;

/// (thread id, function, arguments, argument count, guest block freed once it ran or was
/// dropped: 0 none), oldest first
static QUEUE: Mutex<Vec<(u32, u32, [u32; 4], usize, u32)>> = Mutex::new(Vec::new());

/// Queues `f(arg)` for thread `tid` and wakes it
pub fn queue(tid: u32, f: u32, arg: u32) {
    queue_args(tid, f, &[arg]);
}

/// Queues `f(args...)` (up to four arguments: an IO completion routine's) for thread `tid`
pub fn queue_args(tid: u32, f: u32, args: &[u32]) {
    queue_args_freeing(tid, f, args, 0);
}

/// queue_args, and the guest heap block `free` (an IO_STATUS_BLOCK the APC is given) freed
/// after the APC returns, or when it never runs (its thread ended)
pub fn queue_args_freeing(tid: u32, f: u32, args: &[u32], free: u32) {
    let mut a = [0; 4];
    a[..args.len()].copy_from_slice(args);
    QUEUE.lock().unwrap().push((tid, f, a, args.len(), free));
    obj::signal();
}

/// Whether `tid` has an APC waiting (safe to call with the object table locked: the queue's
/// lock is never held while taking that one)
pub fn pending(tid: u32) -> bool {
    QUEUE.lock().unwrap().iter().any(|&(t, ..)| t == tid)
}

fn take(tid: u32) -> Option<(u32, [u32; 4], usize, u32)> {
    let mut q = QUEUE.lock().unwrap();
    let i = q.iter().position(|&(t, ..)| t == tid)?;
    let (_, f, a, n, free) = q.remove(i);
    Some((f, a, n, free))
}

/// Runs this thread's APCs, in order (including ones they queue); whether there were any
pub fn run(c: &mut Ctx) -> bool {
    let me = crate::cur().tid;
    let mut ran = false;
    while let Some((f, a, n, free)) = take(me) {
        // (NtQueueApcThread with no routine: only wakes the wait)
        if f != 0 {
            c.call(f, &a[..n]);
            ran = true;
        }
        if free != 0 {
            crate::mem::heap_free(free);
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
    let mut frees = Vec::new();
    QUEUE.lock().unwrap().retain(|&(t, .., free)| {
        if t == tid && free != 0 {
            frees.push(free);
        }
        t != tid
    });
    for p in frees {
        crate::mem::heap_free(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Every block handed to queue_args_freeing is freed exactly once, whether its APC
        /// runs or its thread ends first (an echo's IO_STATUS_BLOCK stayed for good)
        #[test]
        fn freed_once_run_or_dropped(ops in proptest::collection::vec((1u32..4, any::<bool>()), 1..30)) {
            let _g = crate::test_guest::enter();
            // (thread ids no real test thread has)
            let base = 0x7fff_0000u32;
            let mut blocks = Vec::new();
            for &(t, with_block) in &ops {
                let b = if with_block { crate::mem::heap_alloc(16, true) } else { 0 };
                queue_args_freeing(base + t, 0, &[1, 2, 3], b);
                if b != 0 {
                    blocks.push(b);
                }
            }
            for &b in &blocks {
                prop_assert!(crate::mem::heap_owns(b));
            }
            for t in 1..4 {
                forget(base + t);
            }
            prop_assert!(!QUEUE.lock().unwrap().iter().any(|&(t, ..)| t >= base));
            for &b in &blocks {
                prop_assert!(!crate::mem::heap_owns(b), "{:#x} kept", b);
            }
        }
    }
}
