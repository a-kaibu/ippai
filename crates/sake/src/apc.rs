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

/// The thread id behind a thread handle (or the current-thread pseudo handle)
fn tid_of(h: u32) -> Option<u32> {
    if h == obj::CURRENT_THREAD {
        return Some(crate::cur().tid);
    }
    obj::with(|t| match t.objs.get(&h) {
        Some((Obj::Thread { tid, done: false, .. }, _)) if *tid != 0 => Some(*tid),
        _ => None,
    })
}

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
        c.call(f, &a[..n]);
        ran = true;
    }
    ran
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

/// QueueUserAPC(f, thread, arg)
pub fn QueueUserAPC(c: &mut Ctx) -> u32 {
    let (f, h, arg) = (c.a(0), c.a(1), c.a(2));
    match tid_of(h) {
        Some(tid) if f != 0 => {
            queue(tid, f, arg);
            1
        }
        _ => {
            crate::k32::last_error(6); // ERROR_INVALID_HANDLE
            0
        }
    }
}

/// Drops the APCs of a thread that ended
pub fn forget(tid: u32) {
    QUEUE.lock().unwrap().retain(|&(t, ..)| t != tid);
}
