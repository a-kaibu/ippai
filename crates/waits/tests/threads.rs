//! The protocols on real threads (what loom checks exhaustively on small cases, tests/loom.rs,
//! here at scale): counts kept under each lock come out exact, nobody waits for ever.
#![cfg(not(loom))]

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use std::sync::Arc;
use waits::{Cs, Entered, NoPause, Once, Sync as Waits};

const THREADS: u32 = 6;
const N: u64 = 20_000;

#[test]
fn critical_sections_count_exactly() {
    let cs = Arc::new(Cs::new());
    let (own, waiters) = (Arc::new(AtomicU32::new(0)), Arc::new(AtomicU32::new(0)));
    // (a plain read-modify-write the lock alone keeps whole)
    let count = Arc::new(AtomicU64::new(0));
    let t: Vec<_> = (1..=THREADS)
        .map(|me| {
            let (cs, own, waiters, count) = (cs.clone(), own.clone(), waiters.clone(), count.clone());
            std::thread::spawn(move || {
                for i in 0..N {
                    assert_eq!(cs.enter(&own, &waiters, me, &NoPause), Entered::First);
                    if i % 7 == 0 {
                        assert_eq!(cs.enter(&own, &waiters, me, &NoPause), Entered::Again);
                    }
                    let v = count.load(Relaxed);
                    count.store(v + 1, Relaxed);
                    cs.leave(&own, &waiters);
                }
            })
        })
        .collect();
    t.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(count.load(SeqCst), THREADS as u64 * N);
    assert_eq!((own.load(SeqCst), waiters.load(SeqCst)), (0, 0));
}

#[test]
fn srw_locks_count_exactly_and_readers_see_whole_writes() {
    let s = Arc::new(Waits::new());
    let word = Arc::new(AtomicU32::new(0));
    let pair = Arc::new((AtomicU64::new(0), AtomicU64::new(0)));
    let t: Vec<_> = (0..THREADS)
        .map(|k| {
            let (s, word, pair) = (s.clone(), word.clone(), pair.clone());
            std::thread::spawn(move || {
                for _ in 0..N {
                    if k % 2 == 0 {
                        s.srw_acquire(&word, false, &NoPause);
                        let v = pair.0.load(Relaxed);
                        pair.0.store(v + 1, Relaxed);
                        pair.1.store(v + 1, Relaxed);
                        s.srw_release(&word, false);
                    } else {
                        s.srw_acquire(&word, true, &NoPause);
                        assert_eq!(pair.0.load(Relaxed), pair.1.load(Relaxed));
                        s.srw_release(&word, true);
                    }
                }
            })
        })
        .collect();
    t.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(pair.0.load(SeqCst), (THREADS as u64).div_ceil(2) * N);
    assert_eq!(word.load(SeqCst), 0);
}

#[test]
fn condition_variables_hand_every_item_over() {
    let s = Arc::new(Waits::new());
    let word = Arc::new(AtomicU32::new(0));
    let queue = Arc::new(AtomicU64::new(0));
    let taken = Arc::new(AtomicU64::new(0));
    let consumers: Vec<_> = (0..2)
        .map(|_| {
            let (s, word, queue, taken) = (s.clone(), word.clone(), queue.clone(), taken.clone());
            std::thread::spawn(move || loop {
                s.srw_acquire(&word, false, &NoPause);
                while queue.load(Relaxed) == 0 && taken.load(Relaxed) < N {
                    s.cv_wait(1, u32::MAX, || s.srw_release(&word, false), || s.srw_acquire(&word, false, &NoPause), &NoPause);
                }
                if taken.load(Relaxed) >= N {
                    s.srw_release(&word, false);
                    s.cv_wake(1);
                    return;
                }
                queue.store(queue.load(Relaxed) - 1, Relaxed);
                taken.store(taken.load(Relaxed) + 1, Relaxed);
                s.srw_release(&word, false);
                s.cv_wake(2);
            })
        })
        .collect();
    for _ in 0..N {
        s.srw_acquire(&word, false, &NoPause);
        while queue.load(Relaxed) >= 4 {
            s.cv_wait(2, u32::MAX, || s.srw_release(&word, false), || s.srw_acquire(&word, false, &NoPause), &NoPause);
        }
        queue.store(queue.load(Relaxed) + 1, Relaxed);
        s.srw_release(&word, false);
        s.cv_wake(1);
    }
    consumers.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(taken.load(SeqCst), N);
}

#[test]
fn timed_waits_time_out() {
    let s = Waits::new();
    let t = std::time::Instant::now();
    assert!(!s.cv_wait(9, 30, || {}, || {}, &NoPause));
    assert!(t.elapsed() >= std::time::Duration::from_millis(30));
    let a = waits::Addr::new();
    assert!(!a.wait(0x1000, || true, 20, &NoPause));
    assert!(a.wait(0x1000, || false, 20, &NoPause), "a value that already differs returns at once");
}

#[test]
fn init_once_runs_once_on_many_threads() {
    let s = Arc::new(Waits::new());
    let word = Arc::new(AtomicU32::new(0));
    let runs = Arc::new(AtomicU32::new(0));
    let t: Vec<_> = (0..THREADS)
        .map(|_| {
            let (s, word, runs) = (s.clone(), word.clone(), runs.clone());
            std::thread::spawn(move || {
                match s.once_execute(&word, || {
                    runs.fetch_add(1, SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    (1, 0x100 | 2)
                }) {
                    Once::Ran(r) => assert_eq!(r, 1),
                    Once::Done(w) => assert_eq!(w, 0x102),
                }
            })
        })
        .collect();
    t.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(runs.load(SeqCst), 1);
}

/// A writer waiting behind a reader is preferred: new readers (TryAcquireSRWLockShared, and
/// AcquireSRWLockShared, which waits) stand back until it had the lock (Wine's kernel32:sync
/// test_srwlock_base, seq 17-18 and 24), and the word is 0 again at the end.
#[test]
fn srw_waiting_writer_is_preferred_over_new_readers() {
    let s = Arc::new(Waits::new());
    let word = Arc::new(AtomicU32::new(0));
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    s.srw_acquire(&word, true, &NoPause);
    let w = {
        let (s, word, order) = (s.clone(), word.clone(), order.clone());
        std::thread::spawn(move || {
            s.srw_acquire(&word, false, &NoPause);
            order.lock().unwrap().push("writer");
            std::thread::sleep(std::time::Duration::from_millis(20));
            s.srw_release(&word, false);
        })
    };
    while word.load(SeqCst) < waits::SRW_WRITER_WAITS {
        std::thread::yield_now();
    }
    assert!(!Waits::srw_try(&word, true), "a new reader waits behind the writer");
    assert!(!Waits::srw_try(&word, false), "the reader still holds it");
    let r = {
        let (s, word, order) = (s.clone(), word.clone(), order.clone());
        std::thread::spawn(move || {
            s.srw_acquire(&word, true, &NoPause);
            order.lock().unwrap().push("reader");
            s.srw_release(&word, true);
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(order.lock().unwrap().is_empty());
    s.srw_release(&word, true);
    w.join().unwrap();
    r.join().unwrap();
    assert_eq!(*order.lock().unwrap(), vec!["writer", "reader"]);
    assert_eq!(word.load(SeqCst), 0);
}

/// review b11: a condition variable's entry goes with its last waiter, and a wake of one
/// nobody waits on adds none (C++ runtimes put CVs in short-lived heap objects: the map grew
/// for the whole session).
#[test]
fn condition_variables_leave_no_entries() {
    let s = Arc::new(Waits::new());
    for cv in (0x10_0000u32..0x10_0000 + 4096 * 4).step_by(4) {
        s.cv_wake(cv);
        assert!(!s.cv_wait(cv, 0, || {}, || {}, &NoPause));
    }
    assert_eq!(s.cv_entries(), 0);
    // waiters on many CVs at once, each woken: none left either
    let word = Arc::new(AtomicU32::new(0));
    let done = Arc::new(AtomicU64::new(0));
    let t: Vec<_> = (0..THREADS as u32)
        .map(|i| {
            let (s, word, done) = (s.clone(), word.clone(), done.clone());
            std::thread::spawn(move || {
                s.srw_acquire(&word, false, &NoPause);
                while done.load(SeqCst) & (1 << i) == 0 {
                    s.cv_wait(0x2000 + 4 * i, u32::MAX, || s.srw_release(&word, false), || s.srw_acquire(&word, false, &NoPause), &NoPause);
                }
                s.srw_release(&word, false);
            })
        })
        .collect();
    for i in 0..THREADS as u32 {
        s.srw_acquire(&word, false, &NoPause);
        done.fetch_or(1 << i, SeqCst);
        s.srw_release(&word, false);
        s.cv_wake(0x2000 + 4 * i);
    }
    t.into_iter().for_each(|t| t.join().unwrap());
    assert_eq!(s.cv_entries(), 0);
}

/// review b11: an idle waiter sleeps (the untimed waits polled every 50 ms: 20 wakeups a
/// second per parked worker), and a wake of another condition variable or address in another
/// bucket does not end its wait early.
#[test]
fn waiters_of_other_words_sleep_on() {
    let s = Arc::new(Waits::new());
    // two CVs in different buckets
    let a = 0x3000u32;
    let b = (0x3004u32..).step_by(4).find(|&b| waits::bucket(b as usize) != waits::bucket(a as usize)).unwrap();
    let t0 = std::time::Instant::now();
    let w = {
        let s = s.clone();
        std::thread::spawn(move || s.cv_wait(a, 300, || {}, || {}, &NoPause))
    };
    for _ in 0..2000 {
        s.cv_wake(b);
    }
    assert!(!w.join().unwrap(), "a wake of another CV ends no wait");
    assert!(t0.elapsed() >= std::time::Duration::from_millis(300));
    let addr = waits::Addr::new();
    let other = (0x5004usize..).step_by(4).find(|&x| waits::bucket(x) != waits::bucket(0x5000)).unwrap();
    std::thread::scope(|sc| {
        let h = sc.spawn(|| addr.wait(0x5000, || true, 200, &NoPause));
        for _ in 0..1000 {
            addr.wake(other);
        }
        assert!(!h.join().unwrap(), "a wake of an address in another bucket ends no wait");
    });
}
