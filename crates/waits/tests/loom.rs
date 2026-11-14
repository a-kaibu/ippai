//! Every interleaving of the lock-word protocols (RUSTFLAGS="--cfg loom" cargo test -p waits
//! --release --test loom; tools/ci/loom.sh). What the locks guard is a loom UnsafeCell: an
//! access that the protocol does not order after the last one is a data race loom reports.
//! The waits have no timeout under loom: a lost wakeup is a deadlock it reports.
#![cfg(loom)]

use loom::cell::UnsafeCell;
use loom::sync::atomic::AtomicU32;
use loom::sync::Arc;
use loom::thread;
use std::sync::atomic::Ordering::SeqCst;
use waits::{Begin, Complete, Cs, Entered, NoPause, Once, Sync as Waits};

fn model(f: impl Fn() + Sync + Send + 'static) {
    let mut b = loom::model::Builder::new();
    if b.preemption_bound.is_none() {
        b.preemption_bound = Some(3);
    }
    b.check(f);
}

struct Section {
    cs: Cs,
    own: AtomicU32,
    waiters: AtomicU32,
    data: UnsafeCell<u32>,
}
unsafe impl Sync for Section {}

impl Section {
    fn new() -> Arc<Self> {
        Arc::new(Section { cs: Cs::new(), own: AtomicU32::new(0), waiters: AtomicU32::new(0), data: UnsafeCell::new(0) })
    }
    fn add(&self, me: u32) {
        assert_eq!(self.cs.enter(&self.own, &self.waiters, me, &NoPause), Entered::First);
        self.data.with_mut(|d| unsafe { *d += 1 });
        self.cs.leave(&self.own, &self.waiters);
    }
}

#[test]
fn critical_sections_exclude_and_order_what_they_guard() {
    model(|| {
        let s = Section::new();
        let t: Vec<_> = (1..=2)
            .map(|me| {
                let s = s.clone();
                thread::spawn(move || s.add(me))
            })
            .collect();
        s.add(3);
        t.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(s.data.with(|d| unsafe { *d }), 3);
        assert_eq!(s.own.load(SeqCst), 0);
        assert_eq!(s.waiters.load(SeqCst), 0);
    });
}

#[test]
fn a_critical_section_entered_again_is_held_until_its_owner_leaves() {
    model(|| {
        let s = Section::new();
        let s2 = s.clone();
        let t = thread::spawn(move || {
            assert_eq!(s2.cs.enter(&s2.own, &s2.waiters, 1, &NoPause), Entered::First);
            assert_eq!(s2.cs.enter(&s2.own, &s2.waiters, 1, &NoPause), Entered::Again);
            s2.data.with_mut(|d| unsafe { *d += 1 });
            // (sake counts the levels: only the last leave frees the word)
            s2.cs.leave(&s2.own, &s2.waiters);
        });
        match Cs::try_enter(&s.own, 2) {
            Entered::First => {
                s.data.with_mut(|d| unsafe { *d += 1 });
                s.cs.leave(&s.own, &s.waiters);
            }
            Entered::Busy => {}
            Entered::Again => panic!("thread 2 never held it"),
        }
        t.join().unwrap();
    });
}

/// The guest's own LeaveCriticalSection (sake's `fast`: `lock xchg` of the owner, then a look
/// at the waiter count, the wake only if there are any) against the host's slow path.
#[test]
fn the_guest_leave_fast_path_loses_no_wakeup() {
    model(|| {
        let s = Section::new();
        let t: Vec<_> = (1..=2)
            .map(|me| {
                let s = s.clone();
                thread::spawn(move || {
                    assert_eq!(s.cs.enter(&s.own, &s.waiters, me, &NoPause), Entered::First);
                    s.data.with_mut(|d| unsafe { *d += 1 });
                    // (lock xchg: a full barrier)
                    s.own.swap(0, SeqCst);
                    loom::sync::atomic::fence(SeqCst);
                    if s.waiters.load(SeqCst) != 0 {
                        s.cs.wake();
                    }
                })
            })
            .collect();
        t.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(s.data.with(|d| unsafe { *d }), 2);
    });
}

struct Srw {
    sync: Waits,
    word: AtomicU32,
    data: UnsafeCell<u32>,
}
unsafe impl Sync for Srw {}

impl Srw {
    fn new() -> Arc<Self> {
        Arc::new(Srw { sync: Waits::new(), word: AtomicU32::new(0), data: UnsafeCell::new(0) })
    }
}

/// A writer that waits behind a reader marks the word; whatever the interleaving, it gets the
/// lock once the reader (who meanwhile tries to read again) lets go, and the word ends at 0.
#[test]
fn srw_waiting_writer_gets_the_lock_and_the_mark_goes() {
    model(|| {
        let l = Srw::new();
        l.sync.srw_acquire(&l.word, true, &NoPause);
        let w = {
            let l = l.clone();
            thread::spawn(move || {
                l.sync.srw_acquire(&l.word, false, &NoPause);
                l.data.with_mut(|d| unsafe { *d += 1 });
                l.sync.srw_release(&l.word, false);
            })
        };
        // (a second read: taken only while no writer shows in the word)
        let again = Waits::srw_try(&l.word, true);
        if again {
            l.sync.srw_release(&l.word, true);
        }
        l.sync.srw_release(&l.word, true);
        w.join().unwrap();
        assert_eq!(l.data.with(|d| unsafe { *d }), 1);
        assert_eq!(l.word.load(SeqCst), 0);
    });
}

#[test]
fn srw_writers_exclude_each_other() {
    model(|| {
        let l = Srw::new();
        let t: Vec<_> = (0..2)
            .map(|_| {
                let l = l.clone();
                thread::spawn(move || {
                    l.sync.srw_acquire(&l.word, false, &NoPause);
                    l.data.with_mut(|d| unsafe { *d += 1 });
                    l.sync.srw_release(&l.word, false);
                })
            })
            .collect();
        t.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(l.data.with(|d| unsafe { *d }), 2);
        assert_eq!(l.word.load(SeqCst), 0);
    });
}

#[test]
fn srw_readers_see_what_the_writer_wrote_or_nothing_of_it() {
    model(|| {
        let l = Srw::new();
        let w = {
            let l = l.clone();
            thread::spawn(move || {
                l.sync.srw_acquire(&l.word, false, &NoPause);
                l.data.with_mut(|d| unsafe { *d = 7 });
                l.sync.srw_release(&l.word, false);
            })
        };
        let r = {
            let l = l.clone();
            thread::spawn(move || {
                l.sync.srw_acquire(&l.word, true, &NoPause);
                let v = l.data.with(|d| unsafe { *d });
                assert!(v == 0 || v == 7);
                l.sync.srw_release(&l.word, true);
            })
        };
        l.sync.srw_acquire(&l.word, true, &NoPause);
        let v = l.data.with(|d| unsafe { *d });
        assert!(v == 0 || v == 7);
        l.sync.srw_release(&l.word, true);
        w.join().unwrap();
        r.join().unwrap();
        assert_eq!(l.word.load(SeqCst), 0);
    });
}

/// Readers hold the lock at once: none waits for another to let go (here each lets go only once
/// all have it: a wait would never end). Three, as a reader's try fails only when another
/// reader's count lands between its load and its exchange, and the slow path tries again.
#[test]
fn srw_readers_share_the_lock() {
    const N: u32 = 3;
    model(|| {
        let l = Srw::new();
        // (a barrier that blocks: a reader the lock keeps waiting is a deadlock loom reports)
        let got = Arc::new((loom::sync::Mutex::new(0), loom::sync::Condvar::new()));
        let read = |l: &Srw, got: &(loom::sync::Mutex<u32>, loom::sync::Condvar)| {
            l.sync.srw_acquire(&l.word, true, &NoPause);
            let mut n = got.0.lock().unwrap();
            *n += 1;
            got.1.notify_all();
            while *n < N {
                n = got.1.wait(n).unwrap();
            }
            drop(n);
            l.sync.srw_release(&l.word, true);
        };
        let t: Vec<_> = (1..N)
            .map(|_| {
                let (l, got) = (l.clone(), got.clone());
                thread::spawn(move || read(&l, &got))
            })
            .collect();
        read(&l, &got);
        t.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(l.word.load(SeqCst), 0);
    });
}

/// SleepConditionVariableSRW against WakeConditionVariable: a consumer waiting for a flag the
/// producer sets under the lock always sees it (no lost wakeup).
#[test]
fn condition_variables_lose_no_wakeup() {
    model(|| {
        let l = Srw::new();
        let p = {
            let l = l.clone();
            thread::spawn(move || {
                l.sync.srw_acquire(&l.word, false, &NoPause);
                l.data.with_mut(|d| unsafe { *d = 1 });
                l.sync.srw_release(&l.word, false);
                l.sync.cv_wake(0x40);
            })
        };
        l.sync.srw_acquire(&l.word, false, &NoPause);
        while l.data.with(|d| unsafe { *d }) == 0 {
            let woke = l.sync.cv_wait(0x40, u32::MAX, || l.sync.srw_release(&l.word, false), || l.sync.srw_acquire(&l.word, false, &NoPause), &NoPause);
            assert!(woke);
        }
        l.sync.srw_release(&l.word, false);
        p.join().unwrap();
    });
}

/// The same with a critical section as the condition variable's lock (SleepConditionVariableCS).
#[test]
fn condition_variables_over_critical_sections_lose_no_wakeup() {
    model(|| {
        let s = Section::new();
        let sync = Arc::new(Waits::new());
        let p = {
            let (s, sync) = (s.clone(), sync.clone());
            thread::spawn(move || {
                s.cs.enter(&s.own, &s.waiters, 1, &NoPause);
                s.data.with_mut(|d| unsafe { *d = 1 });
                s.cs.leave(&s.own, &s.waiters);
                sync.cv_wake(0x80);
            })
        };
        s.cs.enter(&s.own, &s.waiters, 2, &NoPause);
        while s.data.with(|d| unsafe { *d }) == 0 {
            sync.cv_wait(0x80, u32::MAX, || s.cs.leave(&s.own, &s.waiters), || assert_eq!(s.cs.enter(&s.own, &s.waiters, 2, &NoPause), Entered::First), &NoPause);
        }
        s.cs.leave(&s.own, &s.waiters);
        p.join().unwrap();
    });
}

struct Init {
    sync: Waits,
    word: AtomicU32,
    runs: UnsafeCell<u32>,
}
unsafe impl Sync for Init {}

/// InitOnceExecuteOnce: the function runs once; every caller returns after it, seeing its
/// context and what it wrote.
#[test]
fn init_once_runs_once_and_everyone_sees_it() {
    model(|| {
        let i = Arc::new(Init { sync: Waits::new(), word: AtomicU32::new(0), runs: UnsafeCell::new(0) });
        let go = |i: &Init| match i.sync.once_execute(&i.word, || {
            i.runs.with_mut(|r| unsafe { *r += 1 });
            (1, 0x1230 | 2)
        }) {
            Once::Ran(r) => assert_eq!(r, 1),
            Once::Done(w) => {
                assert_eq!(w, 0x1232);
                assert_eq!(i.runs.with(|r| unsafe { *r }), 1);
            }
        };
        let t = {
            let i = i.clone();
            thread::spawn(move || go(&i))
        };
        go(&i);
        t.join().unwrap();
        assert_eq!(i.runs.with(|r| unsafe { *r }), 1);
        assert_eq!(i.word.load(SeqCst), 0x1232);
    });
}

/// A function that fails leaves the INIT_ONCE new: the other caller runs it again.
#[test]
fn init_once_failure_lets_the_next_caller_try() {
    model(|| {
        let i = Arc::new(Init { sync: Waits::new(), word: AtomicU32::new(0), runs: UnsafeCell::new(0) });
        let t = {
            let i = i.clone();
            thread::spawn(move || {
                i.sync.once_execute(&i.word, || {
                    i.runs.with_mut(|r| unsafe { *r += 1 });
                    (0, 0)
                })
            })
        };
        let me = i.sync.once_execute(&i.word, || {
            i.runs.with_mut(|r| unsafe { *r += 1 });
            (1, 2)
        });
        let other = t.join().unwrap();
        let runs = i.runs.with(|r| unsafe { *r });
        match (me, other) {
            // mine first: the failing one finds it done
            (Once::Ran(1), Once::Done(2)) => assert_eq!(runs, 1),
            (Once::Ran(1), Once::Ran(0)) => assert_eq!(runs, 2),
            x => panic!("{x:?}"),
        }
        assert_eq!(i.word.load(SeqCst), 2);
    });
}

/// InitOnceBeginInitialize / InitOnceComplete: one caller initializes, the other waits for it.
#[test]
fn init_once_begin_and_complete() {
    model(|| {
        let i = Arc::new(Init { sync: Waits::new(), word: AtomicU32::new(0), runs: UnsafeCell::new(0) });
        let go = |i: &Init| match i.sync.once_begin(&i.word, false) {
            Begin::Pending => {
                i.runs.with_mut(|r| unsafe { *r += 1 });
                assert!(i.sync.once_complete(&i.word, 0x40 | 2));
            }
            Begin::Done(w) => {
                assert_eq!(w, 0x42);
                assert_eq!(i.runs.with(|r| unsafe { *r }), 1);
            }
            Begin::NotYet | Begin::Invalid => panic!("not only asked, synchronous only"),
        };
        let t = {
            let i = i.clone();
            thread::spawn(move || go(&i))
        };
        let c = {
            let i = i.clone();
            thread::spawn(move || match i.sync.once_begin(&i.word, true) {
                Begin::Done(w) => assert_eq!(w, 0x42),
                Begin::NotYet => {}
                Begin::Pending | Begin::Invalid => panic!("only asked"),
            })
        };
        go(&i);
        t.join().unwrap();
        c.join().unwrap();
        assert!(!i.sync.once_complete(&i.word, 2), "completed twice");
    });
}

/// INIT_ONCE_ASYNC: both initializers run (neither waits), the first to complete wins and the
/// other's completion fails; a synchronous caller meanwhile is refused or sees it done.
#[test]
fn init_once_async_first_completion_wins() {
    model(|| {
        let i = Arc::new(Init { sync: Waits::new(), word: AtomicU32::new(0), runs: UnsafeCell::new(0) });
        let go = |i: &Init, word: u32| -> bool {
            match i.sync.once_begin_ex(&i.word, false, true) {
                Begin::Pending => i.sync.once_complete_ex(&i.word, true, word).is_ok(),
                Begin::Done(w) => {
                    assert!(w == 0x42 || w == 0x82 || w == 0xc2);
                    false
                }
                // (the synchronous one runs)
                Begin::Invalid => false,
                Begin::NotYet => panic!("not only asked"),
            }
        };
        let t = {
            let i = i.clone();
            thread::spawn(move || go(&i, 0x40 | 2))
        };
        let s = {
            let i = i.clone();
            thread::spawn(move || match i.sync.once_begin_ex(&i.word, false, false) {
                // (an asynchronous one runs, or it is done; or none began yet and this one runs)
                Begin::Invalid => None,
                Begin::Done(w) => Some(w),
                Begin::Pending => {
                    i.sync.once_complete_ex(&i.word, false, 0xc0 | 2).unwrap();
                    Some(0xc2)
                }
                Begin::NotYet => panic!("not only asked"),
            })
        };
        let mine = go(&i, 0x80 | 2);
        let theirs = t.join().unwrap();
        let sync = s.join().unwrap();
        let w = i.word.load(SeqCst);
        if sync == Some(0xc2) {
            assert_eq!(w, 0xc2);
            assert!(!mine && !theirs);
        } else {
            // exactly one asynchronous completion took
            assert!(mine ^ theirs);
            assert_eq!(w, if mine { 0x82 } else { 0x42 });
        }
        assert_eq!(i.sync.once_complete_ex(&i.word, true, 0x102), Err(Complete::Invalid));
    });
}

/// WaitOnAddress against a waker that changes the value, then wakes.
#[test]
fn wait_on_address_loses_no_wakeup() {
    model(|| {
        let a = Arc::new(waits::Addr::new());
        let v = Arc::new(AtomicU32::new(0));
        let t = {
            let (a, v) = (a.clone(), v.clone());
            thread::spawn(move || {
                v.store(1, SeqCst);
                a.wake(0x10);
            })
        };
        while v.load(SeqCst) == 0 {
            assert!(a.wait(0x10, || v.load(SeqCst) == 0, u32::MAX, &NoPause));
        }
        t.join().unwrap();
    });
}

/// Two condition variables whose waiters park in the same bucket, or in different ones
/// (waits::bucket over loom's two): each waiter sees its own wake, none is lost to the other's,
/// and no entry stays once both are done.
#[test]
fn condition_variables_of_two_addresses_lose_no_wakeup() {
    let a = 0x40u32;
    let same = (0x44u32..).step_by(4).find(|&b| waits::bucket(b as usize) == waits::bucket(a as usize)).unwrap();
    let other = (0x44u32..).step_by(4).find(|&b| waits::bucket(b as usize) != waits::bucket(a as usize)).unwrap();
    for b in [same, other] {
        model(move || {
            let sync = Arc::new(Waits::new());
            let flags = Arc::new(AtomicU32::new(0));
            // (the flags are guarded by an SRW lock, the condition variables' lock)
            let word = Arc::new(AtomicU32::new(0));
            let w = |sync: Arc<Waits>, flags: Arc<AtomicU32>, word: Arc<AtomicU32>, cv: u32, bit: u32| {
                thread::spawn(move || {
                    sync.srw_acquire(&word, false, &NoPause);
                    while flags.load(SeqCst) & bit == 0 {
                        sync.cv_wait(cv, u32::MAX, || sync.srw_release(&word, false), || sync.srw_acquire(&word, false, &NoPause), &NoPause);
                    }
                    sync.srw_release(&word, false);
                })
            };
            let t1 = w(sync.clone(), flags.clone(), word.clone(), a, 1);
            let t2 = w(sync.clone(), flags.clone(), word.clone(), b, 2);
            for (bit, cv) in [(1, a), (2, b)] {
                sync.srw_acquire(&word, false, &NoPause);
                flags.fetch_or(bit, SeqCst);
                sync.srw_release(&word, false);
                sync.cv_wake(cv);
            }
            t1.join().unwrap();
            t2.join().unwrap();
            assert_eq!(sync.cv_entries(), 0);
        });
    }
}

/// WaitOnAddress at two addresses (one bucket or two): each waiter's wake reaches it.
#[test]
fn wait_on_two_addresses_loses_no_wakeup() {
    let same = (0x14usize..).step_by(4).find(|&b| waits::bucket(b) == waits::bucket(0x10)).unwrap();
    let other = (0x14usize..).step_by(4).find(|&b| waits::bucket(b) != waits::bucket(0x10)).unwrap();
    for b in [same, other] {
        model(move || {
            let a = Arc::new(waits::Addr::new());
            let v = Arc::new([AtomicU32::new(0), AtomicU32::new(0)]);
            let t = {
                let (a, v) = (a.clone(), v.clone());
                thread::spawn(move || {
                    while v[1].load(SeqCst) == 0 {
                        a.wait(b, || v[1].load(SeqCst) == 0, u32::MAX, &NoPause);
                    }
                })
            };
            let t2 = {
                let (a, v) = (a.clone(), v.clone());
                thread::spawn(move || {
                    v[0].store(1, SeqCst);
                    a.wake(0x10);
                    v[1].store(1, SeqCst);
                    a.wake(b);
                })
            };
            while v[0].load(SeqCst) == 0 {
                a.wait(0x10, || v[0].load(SeqCst) == 0, u32::MAX, &NoPause);
            }
            t.join().unwrap();
            t2.join().unwrap();
        });
    }
}

/// Two critical sections: a waiter of one is woken by its own leave whether or not the
/// other's waiters park in the same bucket (the leave wakes only its bucket now).
#[test]
fn critical_sections_in_buckets_lose_no_wakeup() {
    model(|| {
        let s1 = Section::new();
        let s2 = Section::new();
        let t: Vec<_> = [(s1.clone(), 1), (s1.clone(), 2), (s2.clone(), 3)].into_iter().map(|(s, me)| thread::spawn(move || s.add(me))).collect();
        s2.add(4);
        t.into_iter().for_each(|t| t.join().unwrap());
        assert_eq!(s1.data.with(|d| unsafe { *d }), 2);
        assert_eq!(s2.data.with(|d| unsafe { *d }), 2);
    });
}

/// A save state: a waiting thread stands still at the pause, holding none of the locks (the
/// coordinator takes them while it stands), then goes on.
#[test]
fn waits_stand_still_holding_nothing() {
    struct Pause {
        on: AtomicU32,
        stood: AtomicU32,
        sync: Arc<Waits>,
        cs: Arc<Cs>,
    }
    impl waits::Host for Pause {
        fn pausing(&self) -> bool {
            self.on.load(SeqCst) != 0
        }
        fn point(&self) {
            // (the copy: nothing may be held; the coordinator would take these)
            self.sync.wake_all();
            self.cs.wake();
            self.stood.fetch_add(1, SeqCst);
            while self.on.load(SeqCst) != 0 {
                thread::yield_now();
            }
        }
    }
    model(|| {
        let sync = Arc::new(Waits::new());
        let cs = Arc::new(Cs::new());
        let h = Arc::new(Pause { on: AtomicU32::new(0), stood: AtomicU32::new(0), sync: sync.clone(), cs: cs.clone() });
        let word = Arc::new(AtomicU32::new(1)); // held exclusively by the coordinator
        let t = {
            let (h, word, sync) = (h.clone(), word.clone(), sync.clone());
            thread::spawn(move || {
                sync.srw_acquire(&word, false, &*h);
                sync.srw_release(&word, false);
            })
        };
        h.on.store(1, SeqCst);
        sync.wake_all();
        h.on.store(0, SeqCst);
        sync.srw_release(&word, false);
        t.join().unwrap();
    });
}

// ---------------------------------------------------------------------------------------------
// SuspendThread / ResumeThread of running threads (src/suspend.rs, sake's k32)

mod suspend {
    use super::model;
    use loom::sync::atomic::AtomicU32;
    use loom::sync::Arc;
    use loom::thread;
    use std::sync::atomic::Ordering::SeqCst;
    use waits::suspend::{Suspends, MAX_COUNT};

    const T: u32 = 7;
    /// The target passes a safe point (park) and then runs a step. When the suspender's
    /// SuspendThread returned before the target came to the safe point, the target does not run its
    /// step until ResumeThread (phase 2 is written just before it); it is never left waiting.
    #[test]
    fn a_suspended_thread_runs_nothing_until_resumed() {
        model(|| {
            let s = Arc::new(Suspends::new());
            let phase = Arc::new(AtomicU32::new(0));
            let (s2, p2) = (s.clone(), phase.clone());
            let target = thread::spawn(move || {
                let before = p2.load(SeqCst);
                s2.park(T, || false);
                let after = p2.load(SeqCst);
                if before == 1 {
                    assert_eq!(after, 2, "ran while suspended");
                }
            });
            assert_eq!(s.suspend(T), Some(0));
            phase.store(1, SeqCst);
            phase.store(2, SeqCst);
            assert_eq!(s.resume(T), 1);
            target.join().unwrap();
            assert!(!s.any());
        });
    }

    /// GetThreadContext's wait (review b09): once the target is suspended, wait_stopped returns
    /// only after the target has reached its safe point, and in every interleaving it does
    /// return (no lost wake-up); the target then stays there until ResumeThread
    #[test]
    fn get_context_waits_for_the_safe_point() {
        model(|| {
            let s = Arc::new(Suspends::new());
            let at = Arc::new(AtomicU32::new(0));
            assert_eq!(s.suspend(T), Some(0));
            let (s2, a2) = (s.clone(), at.clone());
            let target = thread::spawn(move || {
                a2.store(1, SeqCst);
                s2.park(T, || false);
                a2.store(2, SeqCst);
            });
            assert!(s.wait_stopped(T, || false, || None));
            assert_eq!(at.load(SeqCst), 1, "stopped at the safe point, not past it");
            assert_eq!(s.resume(T), 1);
            target.join().unwrap();
            assert_eq!(at.load(SeqCst), 2);
            // not suspended: no wait
            assert!(!s.wait_stopped(T, || false, || None));
        });
    }

    /// Two threads suspend and resume the same target at once: the counts each call returns are
    /// those of some order of the calls (every previous count once), and the target, parked in
    /// between, goes on at the end.
    #[test]
    fn concurrent_suspends_and_resumes_count_like_one_order() {
        model(|| {
            let s = Arc::new(Suspends::new());
            let seen = Arc::new(AtomicU32::new(0));
            let mut hs = Vec::new();
            for _ in 0..2 {
                let (s, seen) = (s.clone(), seen.clone());
                hs.push(thread::spawn(move || {
                    let old = s.suspend(T).unwrap();
                    seen.fetch_or(1 << old, SeqCst);
                }));
            }
            let s3 = s.clone();
            let target = thread::spawn(move || {
                s3.park(T, || false);
            });
            for h in hs {
                h.join().unwrap();
            }
            assert_eq!(seen.load(SeqCst), 0b11, "previous counts 0 and 1, once each");
            assert_eq!(s.count(T), 2);
            let r1 = thread::spawn({
                let s = s.clone();
                move || s.resume(T)
            });
            let a = s.resume(T);
            let b = r1.join().unwrap();
            assert_eq!(a.min(b), 1);
            assert_eq!(a.max(b), 2);
            target.join().unwrap();
            assert_eq!(s.count(T), 0);
            assert!(!s.any());
        });
    }

    /// A parked thread asked to leave (a save state: `leave` turns true, then `wake`) returns
    /// without being resumed; its count stays.
    #[test]
    fn a_parked_thread_leaves_for_a_save_state() {
        model(|| {
            let s = Arc::new(Suspends::new());
            let pause = Arc::new(AtomicU32::new(0));
            assert_eq!(s.suspend(T), Some(0));
            let (s2, p2) = (s.clone(), pause.clone());
            let target = thread::spawn(move || {
                s2.park(T, || p2.load(SeqCst) != 0);
            });
            pause.store(1, SeqCst);
            s.wake();
            target.join().unwrap();
            assert_eq!(s.count(T), 1);
            assert_eq!(s.resume(T), 1);
        });
    }

    /// Another thread's suspension does not stop this one, and `any` tells the run loops that
    /// someone is suspended exactly while an entry exists.
    #[test]
    fn only_the_suspended_thread_waits() {
        model(|| {
            let s = Arc::new(Suspends::new());
            let s2 = s.clone();
            let other = thread::spawn(move || {
                assert!(!s2.park(T + 1, || false));
            });
            assert_eq!(s.suspend(T), Some(0));
            assert!(s.any());
            other.join().unwrap();
            s.forget(T);
            assert!(!s.any());
        });
    }

    /// The limit: MAX_COUNT suspensions, then None, as Windows' ERROR_SIGNAL_REFCOUNT_EXCEEDED.
    #[test]
    fn the_count_stops_at_the_limit() {
        model(|| {
            let s = Suspends::new();
            for i in 0..MAX_COUNT {
                assert_eq!(s.suspend(T), Some(i));
            }
            assert_eq!(s.suspend(T), None);
            assert_eq!(s.resume(T), MAX_COUNT);
            assert_eq!(s.resume(99), 0);
        });
    }
}

/// The streaming thread's calls and a Stop (calls.rs, #230): the thread looks at the epoch, enters
/// with it, touches the game's object and leaves; the game stops, then destroys the object (a
/// loom UnsafeCell: a touch the protocol doesn't order before the destroy is a data race, and
/// the `alive` flag it reads must still be set). A second run after that may call again.
mod calls {
    use super::model;
    use loom::cell::UnsafeCell;
    use loom::sync::Arc;
    use loom::thread;
    use waits::calls::Calls;
    use waits::NoPause;

    struct Obj {
        calls: Calls,
        alive: UnsafeCell<bool>,
    }
    unsafe impl Sync for Obj {}

    #[test]
    fn calls_no_call_after_a_stop() {
        model(|| {
            let o = Arc::new(Obj { calls: Calls::new(), alive: UnsafeCell::new(true) });
            o.calls.run();
            let o2 = o.clone();
            let t = thread::spawn(move || {
                for _ in 0..2 {
                    let e = o2.calls.epoch();
                    if o2.calls.enter(e) {
                        o2.alive.with(|a| assert!(unsafe { *a }, "a call into the destroyed object"));
                        o2.calls.leave();
                    }
                }
            });
            assert!(o.calls.stop(std::time::Duration::from_secs(1), &NoPause));
            o.alive.with_mut(|a| unsafe { *a = false });
            t.join().unwrap();
        });
    }

    #[test]
    fn calls_none_after_a_close() {
        model(|| {
            let o = Arc::new(Obj { calls: Calls::new(), alive: UnsafeCell::new(true) });
            o.calls.run();
            let o2 = o.clone();
            let t = thread::spawn(move || {
                let e = o2.calls.epoch();
                if o2.calls.enter(e) {
                    o2.alive.with(|a| assert!(unsafe { *a }));
                    o2.calls.leave();
                }
                // (a look after the close: never in)
                let e = o2.calls.epoch();
                if o2.calls.enter(e) {
                    o2.alive.with(|a| assert!(unsafe { *a }, "in after the close"));
                    o2.calls.leave();
                }
            });
            assert!(o.calls.close(std::time::Duration::from_secs(1), &NoPause));
            o.alive.with_mut(|a| unsafe { *a = false });
            t.join().unwrap();
        });
    }
}

/// handoff.rs: what a signaler stores between SetEvent and its next API call is what the waiter
/// it woke reads once `wait_passed` returns (EntisGLS #158: `SetEvent(done); this->signaled = 1`
/// on the poster's stack). The store is a plain cell: an unordered read is a race loom reports.
#[test]
fn a_woken_waiter_reads_what_its_signaler_stored_before_its_next_call() {
    use waits::handoff::{Handoff, Mark};
    struct S {
        h: Handoff,
        mark: loom::sync::Mutex<Option<Mark>>,
        flag: UnsafeCell<u32>,
    }
    unsafe impl Sync for S {}
    model(|| {
        let s = Arc::new(S { h: Handoff::new(4), mark: loom::sync::Mutex::new(None), flag: UnsafeCell::new(0) });
        let s2 = s.clone();
        let t = thread::spawn(move || {
            s2.h.entered(0x24); // SetEvent
            *s2.mark.lock().unwrap() = Some(s2.h.mark(0x24));
            s2.flag.with_mut(|f| unsafe { *f = 1 });
            s2.h.entered(0x24); // the next call
        });
        let m = loop {
            if let Some(m) = *s.mark.lock().unwrap() {
                break m;
            }
            thread::yield_now();
        };
        assert!(s.h.wait_passed(m, 0x20));
        assert_eq!(s.flag.with(|f| unsafe { *f }), 1);
        t.join().unwrap();
    });
}

/// handoff.rs, SendMessage (#82, #209-5): the owner answered sender 0x24 and then 0x28; 0x28 goes
/// on once 0x24 reached a call that does something, so in every interleaving 0x28 reads the
/// descriptor 0x24 registered without a lock right after its SendMessage (sigcheck.dll's
/// register_hash; host test sendrace), and a yield (Sleep(0)) in between does not let it go.
#[test]
fn the_next_sender_reads_what_the_one_before_registered() {
    use waits::handoff::{Handoff, WORK_CAP};
    struct S {
        h: Handoff,
        slot0: UnsafeCell<u32>,
    }
    unsafe impl Sync for S {}
    model(|| {
        let s = Arc::new(S { h: Handoff::new(4), slot0: UnsafeCell::new(0) });
        // (the owner marks the first sender as it answers it, before letting it go)
        let m = s.h.work_mark(0x24);
        let s2 = s.clone();
        let t = thread::spawn(move || {
            s2.h.entered(0x24); // Sleep(0)
            s2.slot0.with_mut(|f| unsafe { *f = 0x24 });
            s2.h.entered(0x24);
            s2.h.worked(0x24); // its next call
        });
        assert!(s.h.wait_worked(m, 0x28, WORK_CAP));
        assert_eq!(s.slot0.with(|f| unsafe { *f }), 0x24);
        t.join().unwrap();
    });
}

/// The same when the signaler makes no API call after its store but runs on (kura's run loop
/// reports the slice: `progressed`): the waiter goes once that slice is reported, and reads
/// the store; and a slice reported before the store (one already running when SetEvent came
/// is not one: the counter only moves after the mark) is never mistaken for it.
#[test]
fn a_signaler_running_on_without_calls_releases_its_waiter_after_its_store() {
    use waits::handoff::{Handoff, Mark};
    struct S {
        h: Handoff,
        mark: loom::sync::Mutex<Option<Mark>>,
        flag: UnsafeCell<u32>,
    }
    unsafe impl Sync for S {}
    model(|| {
        let s = Arc::new(S { h: Handoff::new(4), mark: loom::sync::Mutex::new(None), flag: UnsafeCell::new(0) });
        let s2 = s.clone();
        let t = thread::spawn(move || {
            s2.h.progressed(0x24); // a slice before the call
            s2.h.entered(0x24); // SetEvent
            *s2.mark.lock().unwrap() = Some(s2.h.mark(0x24));
            s2.flag.with_mut(|f| unsafe { *f = 1 });
            s2.h.progressed(0x24); // runs on: the budget runs out
        });
        let m = loop {
            if let Some(m) = *s.mark.lock().unwrap() {
                break m;
            }
            thread::yield_now();
        };
        assert!(s.h.wait_passed(m, 0x20));
        assert_eq!(s.flag.with(|f| unsafe { *f }), 1);
        t.join().unwrap();
    });
}

/// uni.rs (#199): two threads of a process pinned to one processor write one shared buffer
/// without a lock between their API calls (AIMS' LZSS ring buffer); in every interleaving the
/// code of the two never overlaps (an overlap is a race loom reports on the plain cell) and
/// each gets its turns (a lost wakeup is a deadlock loom reports).
#[test]
fn one_processor_runs_one_thread_at_a_time() {
    use waits::uni::OneCpu;
    struct S {
        cpu: OneCpu,
        ring: UnsafeCell<u32>,
    }
    unsafe impl Sync for S {}
    model(|| {
        let s = Arc::new(S { cpu: OneCpu::new(), ring: UnsafeCell::new(0) });
        let t: Vec<_> = (1..=2u32)
            .map(|id| {
                let s = s.clone();
                thread::spawn(move || {
                    for _ in 0..2 {
                        s.cpu.acquire();
                        s.ring.with_mut(|r| unsafe { *r = id });
                        assert_eq!(s.ring.with(|r| unsafe { *r }), id);
                        s.cpu.release();
                    }
                })
            })
            .collect();
        for t in t {
            t.join().unwrap();
        }
    });
}

/// The output queue (outq, #221): two threads write while the page's thread takes, in every
/// interleaving: every record comes out whole, each writer's in its order, none twice, and no
/// write waits for the taker (it only tries the lock).
#[test]
fn output_queue_keeps_records_whole() {
    use waits::outq::{records, OutQ};
    model(|| {
        let q = Arc::new(OutQ::<64>::new());
        let w: Vec<_> = (1..=2u8)
            .map(|fd| {
                let q = q.clone();
                thread::spawn(move || {
                    q.push(fd, &[fd, 10]);
                    q.push(fd, &[fd, 11, 12]);
                })
            })
            .collect();
        let mut got: Vec<(u8, Vec<u8>)> = vec![];
        let mut out = [0u8; 64];
        let n = q.take(&mut out);
        got.extend(records(&out[..n]).map(|(f, b)| (f, b.to_vec())));
        for t in w {
            t.join().unwrap();
        }
        loop {
            let n = q.take(&mut out);
            if n == 0 {
                break;
            }
            got.extend(records(&out[..n]).map(|(f, b)| (f, b.to_vec())));
        }
        for fd in 1..=2u8 {
            let mine: Vec<Vec<u8>> = got.iter().filter(|r| r.0 == fd).map(|r| r.1.clone()).collect();
            assert_eq!(mine, vec![vec![fd, 10], vec![fd, 11, 12]]);
        }
    });
}

/// sake's obj.rs waits (wake::WaitList): semaphores and auto-reset events under one lock, each
/// waiter with a condition variable of its own; a release wakes only as many waiters of that
/// object as it lets through, a waiter that leaves hands its wakeup on. Nobody sleeps for ever
/// (loom has no timeouts: a lost wakeup is a deadlock it reports), and no token is taken twice.
mod objects {
    use super::*;
    use loom::sync::{Condvar, Mutex};
    use waits::wake::WaitList;

    #[derive(Default)]
    struct State {
        count: Vec<u32>,
        list: WaitList<usize>,
        /// (waiter id, its condition variable's index)
        cvs: Vec<(u64, usize)>,
    }
    struct Table {
        s: Mutex<State>,
        cv: Vec<Condvar>,
    }
    impl Table {
        fn new(objects: usize, waiters: usize) -> Arc<Self> {
            let s = State { count: vec![0; objects], ..Default::default() };
            Arc::new(Table { s: Mutex::new(s), cv: (0..waiters).map(|_| Condvar::new()).collect() })
        }
        fn notify(&self, s: &State, ids: &[u64]) {
            for id in ids {
                let i = s.cvs.iter().find(|c| c.0 == *id).unwrap().1;
                self.cv[i].notify_one();
            }
        }
        /// ReleaseSemaphore(key, n) / SetEvent of an auto-reset event (n = 1)
        fn release(&self, key: usize, n: u32) {
            let mut s = self.s.lock().unwrap();
            s.count[key] += n;
            let mut out = Vec::new();
            s.list.wake(&key, n as usize, &mut out);
            self.notify(&s, &out);
        }
        /// WaitForMultipleObjects(keys, bWaitAll = all, INFINITE) by waiter `me`: what it took
        fn wait(&self, me: usize, keys: &[usize], all: bool) -> Vec<usize> {
            let mut s = self.s.lock().unwrap();
            let id = s.list.add(keys, all);
            s.cvs.push((id, me));
            loop {
                let took: Vec<usize> = if all {
                    if keys.iter().all(|&k| s.count[k] > 0) { keys.to_vec() } else { vec![] }
                } else {
                    keys.iter().copied().find(|&k| s.count[k] > 0).into_iter().collect()
                };
                if !took.is_empty() {
                    for &k in &took {
                        s.count[k] -= 1;
                    }
                    let (_, keys) = s.list.remove(id).unwrap();
                    s.cvs.retain(|c| c.0 != id);
                    // (the wakeup it may have taken from another waiter goes on)
                    let mut out = Vec::new();
                    for k in keys {
                        if s.count[k] > 0 {
                            s.list.wake(&k, 1, &mut out);
                        }
                    }
                    self.notify(&s, &out);
                    return took;
                }
                s.list.rearm(id);
                while !s.list.poked(id) {
                    s = self.cv[me].wait(s).unwrap();
                }
            }
        }
    }

    fn run(objects: usize, waits: Vec<(Vec<usize>, bool)>, releases: Vec<(usize, u32)>) {
        let n = waits.len();
        let mut given = vec![0u32; objects];
        for &(k, n) in &releases {
            given[k] += n;
        }
        let t = Table::new(objects, n);
        let w: Vec<_> = waits
            .into_iter()
            .enumerate()
            .map(|(me, (keys, all))| {
                let t = t.clone();
                thread::spawn(move || t.wait(me, &keys, all))
            })
            .collect();
        let r: Vec<_> = releases
            .into_iter()
            .map(|(k, n)| {
                let t = t.clone();
                thread::spawn(move || t.release(k, n))
            })
            .collect();
        for x in r {
            x.join().unwrap();
        }
        let mut took = vec![0u32; objects];
        for x in w {
            for k in x.join().unwrap() {
                took[k] += 1;
            }
        }
        let s = t.s.lock().unwrap();
        for k in 0..objects {
            assert_eq!(took[k] + s.count[k], given[k]);
        }
        assert!(s.list.is_empty());
    }

    /// Two waiters on one semaphore, one release each: neither is left asleep with a token there.
    #[test]
    fn one_semaphore_two_waiters() {
        model(|| run(1, vec![(vec![0], false), (vec![0], false)], vec![(0, 1), (0, 1)]));
    }

    /// A release of 2 at once lets both through.
    #[test]
    fn a_release_of_two_wakes_two() {
        model(|| run(1, vec![(vec![0], false), (vec![0], false)], vec![(0, 2)]));
    }

    /// The waiter a release woke takes another object of its list instead: the wakeup goes on to
    /// the waiter that has only that object. (Enough for every order: a waiter on [1, 0] may take
    /// 0 before 1 is released.)
    #[test]
    fn a_waiter_that_takes_another_object_hands_the_wakeup_on() {
        model(|| run(2, vec![(vec![1, 0], false), (vec![0], false)], vec![(0, 2), (1, 1)]));
    }

    /// The waiter woken for object 0 takes object 1 (released meanwhile, waking nobody new):
    /// it hands the wakeup on to the waiter that has only 0. Checked as the wakeup itself once
    /// the releases are done: no object that lets one through has a waiter of it asleep without
    /// another one of it awake (then everyone gets what is left over).
    #[test]
    fn the_wakeup_goes_on_when_the_woken_takes_another() {
        model(|| {
            let t = Table::new(2, 2);
            let w: Vec<_> = [vec![1, 0], vec![0]]
                .into_iter()
                .enumerate()
                .map(|(me, keys)| {
                    let t = t.clone();
                    thread::spawn(move || t.wait(me, &keys, false))
                })
                .collect();
            let r = {
                let t = t.clone();
                thread::spawn(move || {
                    t.release(0, 1);
                    t.release(1, 1);
                })
            };
            r.join().unwrap();
            {
                let s = t.s.lock().unwrap();
                for k in 0..2 {
                    if s.count[k] > 0 {
                        let asleep = s.cvs.iter().any(|c| !s.list.poked(c.0) && s.list.waits_on(c.0, &k));
                        let awake = s.cvs.iter().any(|c| s.list.poked(c.0) && s.list.waits_on(c.0, &k));
                        assert!(!asleep || awake, "object {k} is signaled, a waiter of it sleeps and none of them was woken");
                    }
                }
            }
            t.release(0, 2);
            t.release(1, 2);
            for x in w {
                x.join().unwrap();
            }
        });
    }

    /// A wait-all waiter beside a wait-any one on a shared object.
    #[test]
    fn wait_all_beside_wait_any() {
        model(|| run(2, vec![(vec![0, 1], true), (vec![0], false)], vec![(0, 2), (1, 1)]));
    }
}

/// A writer with a deadline behind a reader (_Mtx_timedlock): whether its time runs out
/// before or after the reader lets go, at any point, it either holds the lock alone or gave
/// up leaving no mark in the word (a reader after it gets in), and never misses the release.
#[test]
fn srw_timed_writer_gets_the_lock_or_leaves_no_mark() {
    model(|| {
        let l = Srw::new();
        let checks = Arc::new(AtomicU32::new(0));
        l.sync.srw_acquire(&l.word, true, &NoPause);
        let w = {
            let (l, checks) = (l.clone(), checks.clone());
            thread::spawn(move || {
                // (time runs out at its second look at the clock)
                let got = l.sync.srw_acquire_until(&l.word, false, &NoPause, || Some(if checks.fetch_add(1, SeqCst) >= 1 { 0 } else { 1 }));
                if got {
                    l.data.with_mut(|d| unsafe { *d += 1 });
                    l.sync.srw_release(&l.word, false);
                }
                got
            })
        };
        l.sync.srw_release(&l.word, true);
        let got = w.join().unwrap();
        // (a reader afterwards: the word is free whatever happened)
        assert!(Waits::srw_try(&l.word, true));
        l.sync.srw_release(&l.word, true);
        assert_eq!(l.data.with(|d| unsafe { *d }), got as u32);
        assert_eq!(l.word.load(SeqCst), 0);
    });
}

/// A writer with no time left takes a free lock still, and fails on a held one at once.
#[test]
fn srw_timed_writer_with_no_time_takes_only_a_free_lock() {
    model(|| {
        let l = Srw::new();
        assert!(l.sync.srw_acquire_until(&l.word, false, &NoPause, || Some(0)));
        let t = {
            let l = l.clone();
            thread::spawn(move || l.sync.srw_acquire_until(&l.word, true, &NoPause, || Some(0)))
        };
        let other = t.join().unwrap();
        l.sync.srw_release(&l.word, false);
        if other {
            l.sync.srw_release(&l.word, true);
        }
        assert!(!other, "a reader got in while the writer held it");
        assert_eq!(l.word.load(SeqCst), 0);
    });
}
/// Write-behind (`later.rs`): transactions changing the value, the page's flush_soon (which
/// never waits) and its last look
mod later {
    use super::*;
    use std::time::Duration;
    use waits::later::{Later, Store};

    /// The value: a count of changes, and the count each write wrote (the file's versions)
    #[derive(Default)]
    struct File {
        value: u32,
        written: Vec<u32>,
    }
    impl Store for File {
        fn write(&mut self) -> bool {
            self.written.push(self.value);
            true
        }
    }

    fn change(l: &Later<File>) {
        l.with(File::default, false, Duration::MAX, |h| {
            h.value.value += 1;
            ((), true)
        })
    }

    /// Two threads change the value (they say so after each change); the page reads how many
    /// changes were done, then flush_soon. Once everyone is done the file holds at least those:
    /// a change made before the page looked is never left unwritten. Writes never go back and
    /// none is made twice for one version.
    #[test]
    fn a_change_before_the_page_looks_is_written() {
        model(|| {
            let l = Arc::new(Later::<File>::new());
            let done = Arc::new(AtomicU32::new(0));
            let t: Vec<_> = (0..2)
                .map(|_| {
                    let (l, done) = (l.clone(), done.clone());
                    thread::spawn(move || {
                        change(&l);
                        done.fetch_add(1, SeqCst);
                    })
                })
                .collect();
            let seen = done.load(SeqCst);
            l.flush_soon();
            t.into_iter().for_each(|t| t.join().unwrap());
            l.peek(|h| {
                let w = h.map(|h| h.value.written.clone()).unwrap_or_default();
                assert!(w.last().copied().unwrap_or(0) >= seen, "{seen} changes before the look, file {w:?}");
                assert!(w.windows(2).all(|p| p[0] < p[1]), "{w:?}");
            });
        });
    }

    /// The page looks twice while a thread changes the value twice: after the thread is done
    /// and the page's second look began, everything is in the file
    #[test]
    fn the_last_look_after_the_changes_writes_them_all() {
        model(|| {
            let l = Arc::new(Later::<File>::new());
            let done = Arc::new(AtomicU32::new(0));
            let (l2, d2) = (l.clone(), done.clone());
            let t = thread::spawn(move || {
                change(&l2);
                change(&l2);
                d2.store(1, SeqCst);
            });
            l.flush_soon();
            let finished = done.load(SeqCst) == 1;
            l.flush_soon();
            t.join().unwrap();
            if finished {
                l.peek(|h| assert_eq!(h.unwrap().value.written.last(), Some(&2)));
            }
        });
    }
}

