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
use waits::{Begin, Cs, Entered, NoPause, Once, Sync as Waits};

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
            Begin::NotYet => panic!("not only asked"),
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
                Begin::Pending => panic!("only asked"),
            })
        };
        go(&i);
        t.join().unwrap();
        c.join().unwrap();
        assert!(!i.sync.once_complete(&i.word, 2), "completed twice");
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
                a.wake_all();
            })
        };
        while v.load(SeqCst) == 0 {
            assert!(a.wait(|| v.load(SeqCst) == 0, u32::MAX, &NoPause));
        }
        t.join().unwrap();
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
