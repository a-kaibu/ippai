//! The lock-word protocols of Win32's user-mode synchronization, as sake runs them on words in
//! guest memory: critical sections (OwningThread is the lock word, LockSemaphore counts
//! waiters), slim reader/writer locks (bit 0 exclusive, the rest 2 x readers), condition
//! variables (a generation per address), one-time initialization (0 new, 1 running, the context
//! with 2 in its low bits when done) and WaitOnAddress (one generation for every address).
//!
//! A thread that cannot take a word at once waits on a host lock and condition variable; the
//! timed waits there are only a safety net. Built with `--cfg loom` the types are loom's and
//! those waits have no timeout, so `tests/loom.rs` checks every interleaving (and every
//! weak-memory outcome loom models) for mutual exclusion, data races on what the locks guard,
//! lost wakeups and deadlocks.
//!
//! What a save state needs (sake's `snap`): every wait here asks [`Host::pausing`] while it
//! waits and stands still in [`Host::point`] holding none of its locks.


pub mod calls;
pub mod suspend;

#[cfg(loom)]
pub use loom::sync::atomic::AtomicU32;
#[cfg(loom)]
use loom::sync::{Condvar, Mutex, MutexGuard};
#[cfg(not(loom))]
pub use std::sync::atomic::AtomicU32;
#[cfg(not(loom))]
use std::sync::{Condvar, Mutex, MutexGuard};

use std::collections::BTreeMap;
use std::sync::atomic::Ordering::{Acquire, Relaxed, SeqCst};
use std::time::{Duration, Instant};

/// What a waiting thread does for a save state.
pub trait Host {
    /// Whether threads are asked to stand still now.
    fn pausing(&self) -> bool {
        false
    }
    /// Stands still (called holding none of this crate's locks).
    fn point(&self) {}
}

/// No save states.
pub struct NoPause;
impl Host for NoPause {}

/// How many times a lock word is tried before the thread waits.
#[cfg(not(loom))]
const SPINS: u32 = 50;
#[cfg(loom)]
const SPINS: u32 = 1;

fn relock<'a, T>(m: &'a Mutex<T>) -> MutexGuard<'a, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A wait whose timeout is only a safety net (loom: none, so a lost wakeup is a deadlock).
fn nap<'a, T>(cv: &Condvar, g: MutexGuard<'a, T>, ms: u64) -> MutexGuard<'a, T> {
    cv.wait_timeout(g, Duration::from_millis(ms)).unwrap_or_else(|e| e.into_inner()).0
}

/// Between the store and the load of the two sides of a Dekker handshake (a leave clears the
/// word, then looks at the waiters; a waiter counts itself, then looks at the word): one of the
/// two sees the other. SeqCst accesses alone order that too; loom models only the fence.
fn handshake() {
    #[cfg(loom)]
    loom::sync::atomic::fence(SeqCst);
    #[cfg(not(loom))]
    std::sync::atomic::fence(SeqCst);
}

fn spin() {
    #[cfg(loom)]
    loom::thread::yield_now();
    #[cfg(not(loom))]
    std::hint::spin_loop();
}

// ---------------------------------------------------------------------------------------------
// critical sections

/// The waits of every critical section (one lock and condition variable for all of them).
pub struct Cs {
    lock: Mutex<()>,
    cv: Condvar,
}

/// What TryEnterCriticalSection found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entered {
    /// the section was free and is this thread's now
    First,
    /// this thread already held it (one more level)
    Again,
    /// another thread holds it
    Busy,
}

impl Cs {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Cs { lock: Mutex::new(()), cv: Condvar::new() }
    }
    #[cfg(loom)]
    pub fn new() -> Self {
        Cs { lock: Mutex::new(()), cv: Condvar::new() }
    }

    /// TryEnterCriticalSection's part: `own` is the lock word, `me` the (nonzero) thread id.
    pub fn try_enter(own: &AtomicU32, me: u32) -> Entered {
        if own.load(Acquire) == me {
            return Entered::Again;
        }
        if own.compare_exchange(0, me, Acquire, Relaxed).is_ok() {
            Entered::First
        } else {
            Entered::Busy
        }
    }

    /// EnterCriticalSection's part: takes `own` for `me`, waiting while another thread holds
    /// it (counted in `waiters` meanwhile). Entered::Again when `me` held it already.
    pub fn enter(&self, own: &AtomicU32, waiters: &AtomicU32, me: u32, h: &impl Host) -> Entered {
        if own.load(Acquire) == me {
            return Entered::Again;
        }
        let mut spins = 0;
        loop {
            if own.compare_exchange(0, me, Acquire, Relaxed).is_ok() {
                return Entered::First;
            }
            spins += 1;
            if spins < SPINS {
                spin();
                continue;
            }
            // (counted under the lock before the word is looked at again: a leave that misses
            // the count cleared the word before, so this sees it free; one that sees it
            // notifies under the lock, after this thread waits)
            let mut g = relock(&self.lock);
            waiters.fetch_add(1, SeqCst);
            handshake();
            while own.load(SeqCst) != 0 {
                if h.pausing() {
                    waiters.fetch_sub(1, SeqCst);
                    drop(g);
                    h.point();
                    g = relock(&self.lock);
                    waiters.fetch_add(1, SeqCst);
                    handshake();
                    continue;
                }
                g = nap(&self.cv, g, 50);
            }
            waiters.fetch_sub(1, SeqCst);
            drop(g);
        }
    }

    /// LeaveCriticalSection's part, at the last level: frees `own`, wakes the waiters if any.
    pub fn leave(&self, own: &AtomicU32, waiters: &AtomicU32) {
        own.store(0, SeqCst);
        handshake();
        if waiters.load(SeqCst) != 0 {
            self.wake();
        }
    }

    /// Wakes every waiter (the guest's own leave, sake's `fast`, found some after freeing it;
    /// a save state).
    pub fn wake(&self) {
        let _g = relock(&self.lock);
        self.cv.notify_all();
    }
}

impl Default for Cs {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// SRW locks, condition variables, one-time init

/// The waits of every SRW lock, condition variable and INIT_ONCE: one lock (which also holds
/// the condition variables' generations) and one condition variable.
/// One writer waiting for an SRW lock, in its word (Sync::srw_try)
pub const SRW_WRITER_WAITS: u32 = 0x1_0000;

pub struct Sync {
    gens: Mutex<BTreeMap<u32, u32>>,
    cv: Condvar,
    /// threads in srw_acquire's slow path (any lock): a release with none skips the lock and
    /// the wake, which cost Godot's loading thread seconds (one futex wake per release)
    srw_waiters: AtomicU32,
}

/// What InitOnceBeginInitialize found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Begin {
    /// done: the word (the context with 2 in its low bits)
    Done(u32),
    /// not done, and the caller only asked
    NotYet,
    /// the caller initializes now (InitOnceComplete ends it)
    Pending,
    /// a synchronous initialization runs and this one is asynchronous, or the other way round
    Invalid,
}

/// Why InitOnceComplete failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Complete {
    /// ERROR_INVALID_PARAMETER: another kind of initialization runs
    Invalid,
    /// ERROR_GEN_FAILURE: none runs
    NotPending,
}

/// What InitOnceExecuteOnce did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Once {
    /// ran the function, which returned this
    Ran(u32),
    /// done before: the word
    Done(u32),
}

impl Sync {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Sync { gens: Mutex::new(BTreeMap::new()), cv: Condvar::new(), srw_waiters: AtomicU32::new(0) }
    }
    #[cfg(loom)]
    pub fn new() -> Self {
        Sync { gens: Mutex::new(BTreeMap::new()), cv: Condvar::new(), srw_waiters: AtomicU32::new(0) }
    }

    /// TryAcquireSRWLockShared / Exclusive. The word: bit 0 a writer holds it, bits 1-15 twice
    /// the readers holding it, bits 16 up the writers waiting for it (SRW_WRITER_WAITS each).
    /// A reader fails while a writer holds the lock or waits for it (a waiting writer is
    /// preferred, as on Windows: Wine's kernel32:sync test_srwlock_base); another reader's
    /// count landing between its load and its exchange makes it try again (it gave up there
    /// once, and waited for a release that the readers holding the lock might never make:
    /// loom's srw_readers_share_the_lock). A writer takes a lock nobody holds.
    pub fn srw_try(a: &AtomicU32, shared: bool) -> bool {
        Self::srw_take(a, shared, false)
    }

    /// The exchange: `waiting` for a writer counted among the waiting ones (it takes its count
    /// off as it takes the lock).
    fn srw_take(a: &AtomicU32, shared: bool, waiting: bool) -> bool {
        let mut v = a.load(SeqCst);
        loop {
            let new = if shared {
                if v & 1 != 0 || v >= SRW_WRITER_WAITS {
                    return false;
                }
                v + 2
            } else {
                if v & (SRW_WRITER_WAITS - 1) != 0 {
                    return false;
                }
                (v | 1) - if waiting { SRW_WRITER_WAITS } else { 0 }
            };
            match a.compare_exchange_weak(v, new, SeqCst, SeqCst) {
                Ok(_) => return true,
                Err(now) => v = now,
            }
        }
    }

    /// AcquireSRWLockShared / Exclusive.
    pub fn srw_acquire(&self, a: &AtomicU32, shared: bool, h: &impl Host) {
        for _ in 0..SPINS {
            if Self::srw_try(a, shared) {
                return;
            }
            spin();
        }
        let mut g = relock(&self.gens);
        // (counted before the retry: a release that misses the count frees the lock before it,
        // so the retry takes it; one that sees it notifies under the lock, after this thread waits)
        self.srw_waiters.fetch_add(1, SeqCst);
        // (a writer shows in the word that it waits: new readers stand back)
        let mark = |on: bool| {
            if !shared {
                if on {
                    a.fetch_add(SRW_WRITER_WAITS, SeqCst);
                } else {
                    a.fetch_sub(SRW_WRITER_WAITS, SeqCst);
                }
            }
        };
        mark(true);
        handshake();
        while !Self::srw_take(a, shared, !shared) {
            if h.pausing() {
                // (not marked while paused: a save state taken then has no waiter in the word)
                mark(false);
                self.cv.notify_all();
                drop(g);
                h.point();
                g = relock(&self.gens);
                mark(true);
                continue;
            }
            g = nap(&self.cv, g, 20);
        }
        self.srw_waiters.fetch_sub(1, SeqCst);
    }

    /// ReleaseSRWLockShared / Exclusive.
    pub fn srw_release(&self, a: &AtomicU32, shared: bool) {
        if shared {
            a.fetch_sub(2, SeqCst);
        } else {
            a.fetch_and(!1, SeqCst);
        }
        handshake();
        if self.srw_waiters.load(SeqCst) == 0 {
            return;
        }
        let _g = relock(&self.gens);
        self.cv.notify_all();
    }

    /// A condition variable's wait: `release` lets go of its lock, `reacquire` takes it again.
    /// `ms`: u32::MAX for ever. False: timed out.
    pub fn cv_wait(&self, cv: u32, ms: u32, release: impl FnOnce(), reacquire: impl FnOnce(), h: &impl Host) -> bool {
        // the generation is read before the lock is released (a wake in between is seen), but
        // this lock is not held across `release` (srw_release takes it to notify: holding it
        // deadlocked)
        let gen0 = *relock(&self.gens).entry(cv).or_insert(0);
        release();
        let mut g = relock(&self.gens);
        let end = (ms != u32::MAX).then(|| Instant::now() + Duration::from_millis(ms as u64));
        let mut woke = true;
        while g.get(&cv).copied() == Some(gen0) {
            if h.pausing() {
                drop(g);
                h.point();
                g = relock(&self.gens);
                continue;
            }
            let left = match end {
                Some(e) => match e.checked_duration_since(Instant::now()) {
                    Some(d) => d.min(Duration::from_millis(50)),
                    None => {
                        woke = false;
                        break;
                    }
                },
                None => Duration::from_millis(50),
            };
            g = self.cv.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        drop(g);
        reacquire();
        woke
    }

    /// WakeConditionVariable / WakeAllConditionVariable (both wake every waiter: callers check
    /// their condition again).
    pub fn cv_wake(&self, cv: u32) {
        let mut g = relock(&self.gens);
        let e = g.entry(cv).or_insert(0);
        *e = e.wrapping_add(1);
        self.cv.notify_all();
    }

    /// InitOnceExecuteOnce: runs `f` if no one did (or every run so far failed); `f` gives the
    /// function's result and the word to leave (0 to let the next caller try).
    pub fn once_execute(&self, a: &AtomicU32, f: impl FnOnce() -> (u32, u32)) -> Once {
        let mut f = Some(f);
        loop {
            match a.compare_exchange(0, 1, SeqCst, SeqCst) {
                Ok(_) => {
                    let (r, word) = (f.take().unwrap())();
                    a.store(word, SeqCst);
                    self.wake_all();
                    return Once::Ran(r);
                }
                Err(v) if v & 3 == 2 => return Once::Done(v),
                Err(_) => self.wait_running(a),
            }
        }
    }

    /// InitOnceBeginInitialize.
    pub fn once_begin(&self, a: &AtomicU32, check_only: bool) -> Begin {
        self.once_begin_ex(a, check_only, false)
    }

    /// InitOnceBeginInitialize with INIT_ONCE_ASYNC as `asynchronous`: the word is 3 while
    /// asynchronous initializers run (any number of them, none waits), 1 while one synchronous
    /// one does (the others wait); mixing the two is Begin::Invalid.
    pub fn once_begin_ex(&self, a: &AtomicU32, check_only: bool, asynchronous: bool) -> Begin {
        loop {
            let v = a.load(SeqCst);
            if v & 3 == 2 {
                return Begin::Done(v);
            }
            if check_only {
                return Begin::NotYet;
            }
            if asynchronous {
                match v {
                    3 => return Begin::Pending,
                    1 => return Begin::Invalid,
                    _ => {
                        if a.compare_exchange(0, 3, SeqCst, SeqCst).is_ok() {
                            return Begin::Pending;
                        }
                        continue;
                    }
                }
            }
            if v == 3 {
                return Begin::Invalid;
            }
            if v == 0 && a.compare_exchange(0, 1, SeqCst, SeqCst).is_ok() {
                return Begin::Pending;
            }
            self.wait_running(a);
        }
    }

    /// InitOnceComplete: `word` is what the INIT_ONCE becomes (0: failed, the next caller
    /// tries). False: nobody was initializing it.
    pub fn once_complete(&self, a: &AtomicU32, word: u32) -> bool {
        self.once_complete_ex(a, false, word).is_ok()
    }

    /// InitOnceComplete with INIT_ONCE_ASYNC as `asynchronous` (the first asynchronous
    /// initializer to complete wins; failing is not allowed then): Err(Complete::Invalid) for
    /// the other kind of initialization (or an asynchronous one already done),
    /// Err(Complete::NotPending) when nobody initializes it.
    pub fn once_complete_ex(&self, a: &AtomicU32, asynchronous: bool, word: u32) -> Result<(), Complete> {
        let v = a.load(SeqCst);
        if asynchronous {
            if v != 3 || word == 0 || a.compare_exchange(3, word, SeqCst, SeqCst).is_err() {
                return Err(Complete::Invalid);
            }
        } else {
            match v {
                1 => a.store(word, SeqCst),
                3 => return Err(Complete::Invalid),
                _ => return Err(Complete::NotPending),
            }
        }
        self.wake_all();
        Ok(())
    }

    /// Waits (once) while `a` is 1.
    fn wait_running(&self, a: &AtomicU32) {
        let g = relock(&self.gens);
        if a.load(SeqCst) == 1 {
            drop(nap(&self.cv, g, 20));
        }
    }

    /// Wakes every waiter (a save state).
    pub fn wake_all(&self) {
        let _g = relock(&self.gens);
        self.cv.notify_all();
    }
}

impl Default for Sync {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------------------------
// WaitOnAddress

/// WaitOnAddress's waits: one generation for every address; any wake wakes every waiter, which
/// returns TRUE (Windows allows that: callers check the value again).
pub struct Addr {
    gen: Mutex<u64>,
    cv: Condvar,
}

impl Addr {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Addr { gen: Mutex::new(0), cv: Condvar::new() }
    }
    #[cfg(loom)]
    pub fn new() -> Self {
        Addr { gen: Mutex::new(0), cv: Condvar::new() }
    }

    /// Waits while `same()` (the value still equals the compare value: checked once, under the
    /// lock, as a waker changes the value before it takes the lock to wake) until a wake.
    /// False: timed out (`ms` u32::MAX: never).
    pub fn wait(&self, same: impl FnOnce() -> bool, ms: u32, h: &impl Host) -> bool {
        let mut g = relock(&self.gen);
        if !same() {
            return true;
        }
        let start = *g;
        let end = (ms != u32::MAX).then(|| Instant::now() + Duration::from_millis(ms as u64));
        while *g == start {
            if h.pausing() {
                drop(g);
                h.point();
                g = relock(&self.gen);
                continue;
            }
            let left = match end {
                Some(e) => match e.checked_duration_since(Instant::now()) {
                    Some(d) if !d.is_zero() => d,
                    _ => return false,
                },
                None => Duration::from_secs(3600),
            };
            g = self.cv.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner()).0;
        }
        true
    }

    /// WakeByAddressAll / WakeByAddressSingle.
    pub fn wake_all(&self) {
        *relock(&self.gen) += 1;
        self.cv.notify_all();
    }

    /// Wakes every waiter without a wake (a save state: they look at the pause).
    pub fn stir(&self) {
        drop(relock(&self.gen));
        self.cv.notify_all();
    }
}

impl Default for Addr {
    fn default() -> Self {
        Self::new()
    }
}
