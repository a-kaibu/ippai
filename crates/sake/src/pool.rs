//! The threads Windows runs callbacks on, one set per process as Windows has them:
//!
//! * workers: a few long-lived guest threads taking jobs from one queue ([`submit`]): thread
//!   pool work (QueueUserWorkItem, TrySubmitThreadpoolCallback, SubmitThreadpoolWork), the
//!   callbacks of thread pool timers and waits, of timer-queue timers, of
//!   RegisterWaitForSingleObject and of BindIoCompletionCallback's files;
//! * the timer thread: every timer of the thread pool and the timer queues, and the ends MCI
//!   devices wait for ([`TIMERS`]); the multimedia timers have one of their own ([`MM_TIMERS`]),
//!   as winmm's single timer thread runs them one after the other;
//! * the wait thread: every registered wait ([`wait_add`]), in one wait on all of their objects.
//!
//! Each of those used to start a guest thread of its own per item (1 MiB of guest stack, a TEB,
//! a koji Cpu, a Web Worker on the page; review B-4). The queues and their rules are
//! `waits::pool` (loom and proptest there); this is the guest-thread side. Each thread's start is
//! a trap of `snap::RESTARTABLE`: it stands still for a save state only while it waits, holding
//! nothing, and a restored copy starts its loop over.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::api::Ctx;
use waits::pool::{Need, Take, Timers, Workers};

/// What a pool thread runs.
pub type Job = Box<dyn FnOnce(&mut Ctx) + Send>;

/// The guest side of a save state for the pool's waits.
struct Snap;
impl waits::Host for Snap {
    fn pausing(&self) -> bool {
        kura::snap::pausing()
    }
    fn point(&self) {
        if !crate::snap::point() {
            kura::snap::sleep(Duration::from_millis(10));
        }
    }
}

/// A pool thread of `name` running `f` (its start), its handle closed (the thread holds its own).
fn start(entry: &OnceLock<u32>, name: &str, f: crate::api::Handler) -> Option<u32> {
    let e = *entry.get_or_init(|| crate::api::host_fn(name, f, 4));
    crate::spawn_service(e, 0)
}

/// A thread of its own started once (again after a failed start).
fn once(started: &AtomicBool, entry: &OnceLock<u32>, name: &str, f: crate::api::Handler) {
    if !started.swap(true, Ordering::SeqCst) && start(entry, name, f).is_none() {
        started.store(false, Ordering::SeqCst);
    }
}

// ---------------------------------------------------------------------------------------------
// workers

static WORKERS: Workers<Job> = Workers::new();
/// Workers that stay however long they idle; others end after IDLE with nothing to do.
const KEEP: usize = 2;
const IDLE: Duration = Duration::from_secs(30);
static WORKER: OnceLock<u32> = OnceLock::new();

/// Workers started at once for a queue with none free; beyond them one more comes only when the
/// queue stood still for STALL (a burst of short jobs is run by these few).
const SOFT: usize = 8;
const STALL: Duration = Duration::from_millis(50);

/// Runs `job` on a pool worker (a new one if none is free). False: no worker could be started
/// and none runs (the job does not run).
pub fn submit(job: Job) -> bool {
    match WORKERS.submit(job, SOFT) {
        Need::No => true,
        Need::Worker => start(&WORKER, "pool_worker", worker).is_some() || WORKERS.not_started().is_empty(),
        Need::Check(since) => {
            watch(since);
            true
        }
    }
}

/// In STALL: one more worker if no job was taken since `since` and jobs still wait, then
/// again while they do (a job that waits for one queued behind it still runs).
fn watch(since: u64) {
    if TIMERS.pending(key::POOL) {
        return;
    }
    TIMERS.after(
        key::POOL,
        STALL,
        Box::new(move |_| {
            if WORKERS.stalled(since) && start(&WORKER, "pool_worker", worker).is_none() {
                WORKERS.not_started();
            }
            if WORKERS.backlog() {
                watch(WORKERS.taken());
            }
        }),
    );
}

fn worker(c: &mut Ctx) -> u32 {
    loop {
        match WORKERS.take(&Snap, KEEP, IDLE) {
            Take::Job(j) => j(c),
            Take::Retire => crate::k32::exit_thread(0),
        }
    }
}

/// (workers started and not retired, waiting, jobs queued)
pub fn counts() -> (usize, usize, usize) {
    WORKERS.counts()
}

// ---------------------------------------------------------------------------------------------
// timers

/// Timers served by one guest thread of their own.
pub struct TimerThread {
    timers: Timers<Job>,
    started: AtomicBool,
    entry: OnceLock<u32>,
    name: &'static str,
    run: crate::api::Handler,
}

/// The thread pool's and the timer queues' timers, MCI's ends: their jobs only queue the
/// callbacks for a worker (unless a timer asks to run in the timer thread).
pub static TIMERS: TimerThread = TimerThread::new("pool_timer_thread", |c| TIMERS.serve(c));
/// timeSetEvent's: run in that thread, one after the other.
pub static MM_TIMERS: TimerThread = TimerThread::new("mm_timer_thread", |c| MM_TIMERS.serve(c));

/// Keys of the timers' owners (a key has one entry at most).
pub mod key {
    pub const TP: u64 = 1 << 32;
    pub const TIMERQ: u64 = 2 << 32;
    pub const MCI: u64 = 3 << 32;
    pub const MM: u64 = 4 << 32;
    pub const WS2: u64 = 5 << 32;
    pub const POOL: u64 = 6 << 32;
}

impl TimerThread {
    const fn new(name: &'static str, run: crate::api::Handler) -> Self {
        TimerThread { timers: Timers::new(), started: AtomicBool::new(false), entry: OnceLock::new(), name, run }
    }

    /// `key` runs `job` (on this timer thread) at `at`, replacing its entry before.
    pub fn set(&self, key: u64, at: Instant, job: Job) {
        self.timers.set(key, at, job);
        once(&self.started, &self.entry, self.name, self.run);
    }

    /// In `d` from now.
    pub fn after(&self, key: u64, d: Duration, job: Job) {
        self.set(key, Instant::now() + d, job)
    }

    pub fn cancel(&self, key: u64) -> bool {
        self.timers.cancel(key)
    }

    pub fn pending(&self, key: u64) -> bool {
        self.timers.pending(key)
    }

    fn serve(&self, c: &mut Ctx) -> u32 {
        loop {
            let (_, j) = self.timers.next(&Snap, Instant::now);
            j(c);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// waits

struct Reg {
    obj: u32,
    deadline: Option<Instant>,
    /// called with 0 (signaled, or abandoned) or WAIT_TIMEOUT
    f: Job2,
}
type Job2 = Box<dyn FnOnce(&mut Ctx, u32) + Send>;

static REGS: Mutex<Vec<(u64, Reg)>> = Mutex::new(Vec::new());
/// bumped by every change of REGS: the wait thread looks again
static REG_GEN: AtomicU32 = AtomicU32::new(0);
static WAITING: AtomicBool = AtomicBool::new(false);
static WAITER: OnceLock<u32> = OnceLock::new();
const WAIT_TIMEOUT: u32 = 0x102;

fn regs() -> std::sync::MutexGuard<'static, Vec<(u64, Reg)>> {
    REGS.lock().unwrap_or_else(|e| e.into_inner())
}
fn changed() {
    REG_GEN.fetch_add(1, Ordering::SeqCst);
    crate::obj::signal();
}

/// Once `obj` is signaled, or `ms` passed (INFINITE: never), the wait thread calls `f` with 0 or
/// WAIT_TIMEOUT, once (a wait that goes on is added again). Replaces `key`'s wait before.
pub fn wait_add(key: u64, obj: u32, ms: u32, f: Job2) {
    let deadline = (ms != u32::MAX).then(|| Instant::now() + Duration::from_millis(ms as u64));
    {
        let mut r = regs();
        r.retain(|x| x.0 != key);
        r.push((key, Reg { obj, deadline, f }));
    }
    changed();
    once(&WAITING, &WAITER, "pool_wait_thread", wait_thread);
}

/// `key`'s wait goes (true if it had not fired).
pub fn wait_cancel(key: u64) -> bool {
    let had = {
        let mut r = regs();
        let n = r.len();
        r.retain(|x| x.0 != key);
        r.len() != n
    };
    if had {
        changed();
    }
    had
}

pub fn wait_pending(key: u64) -> bool {
    regs().iter().any(|x| x.0 == key)
}

/// The waits to fire for what one wait on all of them returned (`r`: the index of the object
/// that ended it, WAIT_TIMEOUT, or anything else), and the ones due at `now`; (key, result),
/// each once. A wait whose object could not be waited on (WAIT_FAILED) is `bad`.
fn fired(keys: &[(u64, Option<Instant>)], r: u32, now: Instant, bad: &dyn Fn(usize) -> bool) -> Vec<(u64, u32)> {
    let n = keys.len() as u32;
    let mut out: Vec<(u64, u32)> = Vec::new();
    let hit = match r {
        i if i < n => Some(i),
        i if (0x80..0x80 + n).contains(&i) => Some(i - 0x80),
        _ => None,
    };
    if let Some(i) = hit {
        out.push((keys[i as usize].0, 0));
    }
    if r == u32::MAX {
        // (WAIT_FAILED: an object that can't be waited on; its wait goes without a call)
        out.extend(keys.iter().enumerate().filter(|&(i, _)| bad(i)).map(|(_, k)| (k.0, u32::MAX)));
    }
    for k in keys {
        if k.1.is_some_and(|d| d <= now) && !out.iter().any(|o| o.0 == k.0) {
            out.push((k.0, WAIT_TIMEOUT));
        }
    }
    out
}

fn wait_thread(c: &mut Ctx) -> u32 {
    // (where the list starts this time: a signaled object first in it doesn't starve the rest)
    let mut turn = 0usize;
    loop {
        let gen = REG_GEN.load(Ordering::SeqCst);
        let (keys, handles, ms) = {
            let r = regs();
            let n = r.len().max(1);
            let order: Vec<&(u64, Reg)> = r.iter().cycle().skip(turn % n).take(r.len()).collect();
            let now = Instant::now();
            let ms = order
                .iter()
                .filter_map(|x| x.1.deadline)
                .min()
                .map_or(u32::MAX, |d| (d.saturating_duration_since(now).as_micros().div_ceil(1000)).min(u32::MAX as u128 - 1) as u32);
            (order.iter().map(|x| (x.0, x.1.deadline)).collect::<Vec<_>>(), order.iter().map(|x| x.1.obj).collect::<Vec<_>>(), ms)
        };
        turn = turn.wrapping_add(1);
        let r = crate::obj::wait(&handles, false, ms, Some(&|| REG_GEN.load(Ordering::SeqCst) != gen));
        let bad = |i: usize| crate::obj::wait(&handles[i..i + 1], false, 0, None) == u32::MAX;
        for (k, res) in fired(&keys, r, Instant::now(), &bad) {
            let reg = {
                let mut v = regs();
                v.iter().position(|x| x.0 == k).map(|i| v.remove(i).1)
            };
            if let Some(reg) = reg {
                REG_GEN.fetch_add(1, Ordering::SeqCst);
                if res != u32::MAX {
                    (reg.f)(c, res);
                }
            }
        }
    }
}

/// A save state starts: the pool's threads stand still.
pub(crate) fn snap_wake() {
    WORKERS.wake_all();
    TIMERS.timers.wake();
    MM_TIMERS.timers.wake();
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        /// Each wait fires at most once, the object that ended the wait among them, and every
        /// one due; nothing else.
        #[test]
        fn fired_is_each_wait_once(dues in proptest::collection::vec(proptest::option::of(0..20u64), 0..8), r in prop_oneof![0..10u32, 0x80..0x8au32, Just(0x102u32), Just(u32::MAX)], now in 0..20u64, badmask in 0..256u32) {
            let base = Instant::now();
            let keys: Vec<(u64, Option<Instant>)> = dues.iter().enumerate().map(|(i, d)| (i as u64 + 100, d.map(|d| base + Duration::from_millis(d)))).collect();
            let out = fired(&keys, r, base + Duration::from_millis(now), &|i| badmask >> i & 1 != 0);
            let mut ks: Vec<u64> = out.iter().map(|o| o.0).collect();
            ks.sort();
            ks.dedup();
            prop_assert_eq!(ks.len(), out.len());
            let n = keys.len() as u32;
            let hit = if r < n { Some(r) } else if (0x80..0x80 + n).contains(&r) { Some(r - 0x80) } else { None };
            for (i, k) in keys.iter().enumerate() {
                let due = k.1.is_some_and(|d| d <= base + Duration::from_millis(now));
                let failed = r == u32::MAX && badmask >> i & 1 != 0;
                let want = if hit == Some(i as u32) { Some(0) } else if failed { Some(u32::MAX) } else if due { Some(WAIT_TIMEOUT) } else { None };
                prop_assert_eq!(out.iter().find(|o| o.0 == k.0).map(|o| o.1), want);
            }
        }
    }
}
