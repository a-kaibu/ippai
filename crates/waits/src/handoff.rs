//! The moments right after a thread signals an event, as Windows schedules them: the signaling
//! thread runs on (the waiter it readied starts microseconds later, on another core) so what
//! it writes just after SetEvent is there when the waiter looks. Games depend on it without
//! knowing: EntisGLS (#158) runs a command on its window thread through an object on the
//! poster's stack, `SetEvent(done); this->signaled = 1;`, and by the time the poster has woken
//! and returned, that store lands in its reused stack (a saved ebx = 1, a crash 30 ms later).
//! Under sake the signaler can stall right there for milliseconds (a block to translate, a
//! busy host), so a waiter that was blocked and woke because another thread signaled waits for
//! that thread to make its next API call or to run on a while without one (it has then passed
//! the stores after SetEvent), or for [`CAP`], whichever comes first.
//!
//! Each guest thread has a counter its API calls advance ([`Handoff::entered`]), and so does
//! running a slice of guest code without one ([`Handoff::progressed`]: kura's run loop, when the
//! JIT's budget runs out); signaling records the signaler's counter ([`Handoff::mark`]); the
//! woken waiter waits until it moves ([`Handoff::wait_passed`]). The cap is only for a
//! signaler that does not run at all (a busy host): 30 ms was not enough with three games
//! starting at once, the signaler stalled 32 ms (up to 220 ms seen) and #158 crashed again.
//! Under `--cfg loom` the wait has no cap: `tests/loom.rs` checks that what the signaler stored
//! before its next call or slice is what the waiter reads.
//!
//! A second counter per thread counts only the calls that do something ([`Handoff::worked`]:
//! not a bare yield, Sleep(0) or SwitchToThread, and the thread's end counts too). SendMessage
//! uses it (#82, #209-5): of the senders a busy owner answers one after the other, the next
//! goes on only once the one before has reached such a call ([`Handoff::wait_worked`]), so it
//! has finished what it does right after SendMessage without a lock; a fixed head start was
//! not enough on a loaded machine (1 run in 4 of host test sendrace).

#[cfg(loom)]
use loom::sync::atomic::AtomicU32;
#[cfg(not(loom))]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::{Acquire, Release};
use std::time::Duration;

/// The longest a woken waiter waits for a signaler that makes no progress at all (in the
/// browser a signaler stalls 2-8 ms now and then, over 30 ms on a busy machine; one spinning
/// without API calls progresses: it doesn't hold the waiter this long)
pub const CAP: Duration = Duration::from_millis(1000);

/// A signal: who signaled, and their API counter then
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub tid: u32,
    pub seq: u32,
}

/// The guest threads' API counters (thread ids share a slot modulo the table's size: a shared
/// slot only ends a wait early)
pub struct Handoff {
    seq: Vec<AtomicU32>,
    work: Vec<AtomicU32>,
}

/// The longest a sender waits for the one answered before it to reach a call that does
/// something ([`Handoff::wait_worked`]; one that spins without calls only delays it this much)
pub const WORK_CAP: Duration = Duration::from_millis(250);

impl Handoff {
    pub fn new(slots: usize) -> Self {
        let n = slots.max(1);
        Handoff { seq: (0..n).map(|_| AtomicU32::new(0)).collect(), work: (0..n).map(|_| AtomicU32::new(0)).collect() }
    }
    fn slot(&self, tid: u32) -> &AtomicU32 {
        // (Windows thread ids are multiples of 4)
        &self.seq[(tid as usize >> 2) % self.seq.len()]
    }
    fn work_slot(&self, tid: u32) -> &AtomicU32 {
        &self.work[(tid as usize >> 2) % self.work.len()]
    }
    /// Thread `tid` enters a call that does something (or ends): what it stored before is
    /// visible to a sender that sees the counter move
    pub fn worked(&self, tid: u32) {
        self.work_slot(tid).fetch_add(1, Release);
    }
    /// Thread `tid`'s work counter now (before it goes on from a SendMessage)
    pub fn work_mark(&self, tid: u32) -> Mark {
        Mark { tid, seq: self.work_slot(tid).load(Acquire) }
    }
    /// Whether the thread of `m` has entered a call that does something since
    pub fn worked_since(&self, m: Mark) -> bool {
        self.work_slot(m.tid).load(Acquire) != m.seq
    }
    /// Thread `me` waits for the thread of `m` to enter a call that does something, at most
    /// `cap` (no wait for itself). Whether it got there.
    pub fn wait_worked(&self, m: Mark, me: u32, cap: Duration) -> bool {
        if m.tid == me {
            return true;
        }
        #[cfg(loom)]
        let _ = cap;
        #[cfg(not(loom))]
        let start = std::time::Instant::now();
        loop {
            if self.worked_since(m) {
                return true;
            }
            #[cfg(loom)]
            loom::thread::yield_now();
            #[cfg(not(loom))]
            {
                if start.elapsed() >= cap {
                    return false;
                }
                std::thread::sleep(Duration::from_micros(50));
            }
        }
    }
    /// Thread `tid` enters an API call: what it stored before is visible to a waiter that sees
    /// the counter move
    pub fn entered(&self, tid: u32) {
        self.slot(tid).fetch_add(1, Release);
    }
    /// Thread `tid` ran a slice of guest code since its last API call or slice: what it stored
    /// is visible as after [`Self::entered`]
    pub fn progressed(&self, tid: u32) {
        self.slot(tid).fetch_add(1, Release);
    }
    /// Thread `tid` signals something (in an API call: its counter already counts that call)
    pub fn mark(&self, tid: u32) -> Mark {
        Mark { tid, seq: self.slot(tid).load(Acquire) }
    }
    /// Whether the signaler of `m` has made another API call since
    pub fn passed(&self, m: Mark) -> bool {
        self.slot(m.tid).load(Acquire) != m.seq
    }
    /// Thread `me`, woken by the signal `m`, waits for the signaler to pass its next API call
    /// (at most [`CAP`]; no wait for its own signal). Whether the signaler got there.
    pub fn wait_passed(&self, m: Mark, me: u32) -> bool {
        if m.tid == me {
            return true;
        }
        #[cfg(not(loom))]
        let start = std::time::Instant::now();
        loop {
            if self.passed(m) {
                return true;
            }
            #[cfg(loom)]
            loom::thread::yield_now();
            #[cfg(not(loom))]
            {
                if start.elapsed() >= CAP {
                    return false;
                }
                std::thread::sleep(Duration::from_micros(20));
            }
        }
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
    use std::sync::Arc;

    #[test]
    fn own_signal_needs_no_wait() {
        let h = Handoff::new(8);
        h.entered(0x20);
        assert!(h.wait_passed(h.mark(0x20), 0x20));
    }

    #[test]
    fn a_signaler_that_never_calls_again_costs_the_cap() {
        let h = Handoff::new(8);
        h.entered(0x24);
        let t = std::time::Instant::now();
        assert!(!h.wait_passed(h.mark(0x24), 0x20));
        assert!(t.elapsed() >= CAP);
    }

    /// the #158 shape with real threads: SetEvent, a slow store, the next API call; the waiter
    /// always reads the store
    #[test]
    fn the_store_after_the_signal_is_seen() {
        for _ in 0..50 {
            let h = Arc::new(Handoff::new(64));
            let flag = Arc::new(AtomicBool::new(false));
            let signaled = Arc::new(std::sync::Mutex::new(None::<Mark>));
            let (h2, f2, s2) = (h.clone(), flag.clone(), signaled.clone());
            let t = std::thread::spawn(move || {
                h2.entered(0x24); // SetEvent
                *s2.lock().unwrap() = Some(h2.mark(0x24));
                std::thread::sleep(Duration::from_micros(300)); // stalls before its store
                f2.store(true, Relaxed);
                h2.entered(0x24); // its next call
            });
            let m = loop {
                if let Some(m) = *signaled.lock().unwrap() {
                    break m;
                }
                std::thread::yield_now();
            };
            assert!(h.wait_passed(m, 0x20));
            assert!(flag.load(Relaxed));
            t.join().unwrap();
        }
    }

    #[test]
    fn a_yield_is_not_work_but_a_call_is() {
        let h = Handoff::new(8);
        let m = h.work_mark(0x24);
        h.entered(0x24); // Sleep(0)
        assert!(!h.worked_since(m));
        let t = std::time::Instant::now();
        assert!(!h.wait_worked(m, 0x20, Duration::from_millis(20)));
        assert!(t.elapsed() >= Duration::from_millis(20));
        h.worked(0x24);
        assert!(h.wait_worked(m, 0x20, WORK_CAP));
        assert!(h.wait_worked(m, 0x24, Duration::ZERO));
    }

    /// The sendrace shape (#209-5) with real threads on a loaded machine: two senders answered
    /// one after the other each register into a table without a lock right after their
    /// SendMessage, one stalls before its store (descheduled: a busy host); with the second
    /// waiting for the first to reach a call that does something, the second always finds the
    /// first one's entry.
    #[test]
    fn the_next_sender_sees_what_the_one_before_registered() {
        for round in 0..40 {
            let h = Arc::new(Handoff::new(64));
            let table = Arc::new(std::sync::Mutex::new(Vec::<u32>::new()));
            let (h1, t1) = (h.clone(), table.clone());
            let m = h.work_mark(0x24);
            let a = std::thread::spawn(move || {
                h1.entered(0x24); // Sleep(0): not work
                std::thread::sleep(Duration::from_micros(50 + 37 * (round % 7)));
                let mut t = t1.lock().unwrap();
                if !t.contains(&1) {
                    t.push(1);
                }
                drop(t);
                h1.entered(0x24);
                h1.worked(0x24); // its next call
            });
            assert!(h.wait_worked(m, 0x28, WORK_CAP));
            // (without the wait it looks before the first one stored and takes slot 0 itself;
            // the first then finds that entry, as two Windows senders never do)
            assert_eq!(table.lock().unwrap().iter().position(|&x| x == 1), Some(0), "round {round}");
            a.join().unwrap();
        }
    }

    /// #158 again: with three games starting at once the signaler stalled 32 ms between
    /// SetEvent and its store, past the old 30 ms cap, and the game crashed. A stall of 100 ms
    /// with no progress at all is waited out.
    #[test]
    fn a_long_stall_is_waited_out() {
        let h = Arc::new(Handoff::new(64));
        let flag = Arc::new(AtomicBool::new(false));
        h.entered(0x24);
        let m = h.mark(0x24);
        let (h2, f2) = (h.clone(), flag.clone());
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            f2.store(true, Relaxed);
            h2.entered(0x24);
        });
        assert!(h.wait_passed(m, 0x20));
        assert!(flag.load(Relaxed));
        t.join().unwrap();
    }

    /// A signaler that runs on without API calls (a long loop after SetEvent) reports slices:
    /// its waiter goes after the first, not after the cap
    #[test]
    fn a_signaler_that_spins_without_calls_does_not_hold_its_waiter() {
        let h = Arc::new(Handoff::new(64));
        let stop = Arc::new(AtomicBool::new(false));
        h.entered(0x24);
        let m = h.mark(0x24);
        let (h2, s2) = (h.clone(), stop.clone());
        let t = std::thread::spawn(move || {
            while !s2.load(Relaxed) {
                std::thread::sleep(Duration::from_millis(1)); // a slice of guest code
                h2.progressed(0x24);
            }
        });
        let start = std::time::Instant::now();
        assert!(h.wait_passed(m, 0x20));
        assert!(start.elapsed() < CAP / 4, "{:?}", start.elapsed());
        stop.store(true, Relaxed);
        t.join().unwrap();
    }

    proptest! {
        /// the work counter moves only with worked(), never with entered(), and per slot
        #[test]
        fn work_tracks_only_work(slots in 1usize..64, tid in (8u32..4096).prop_map(|t| t * 4), other in (8u32..4096).prop_map(|t| t * 4), yields in 0u32..5, calls in 0u32..5) {
            let h = Handoff::new(slots);
            let m = h.work_mark(tid);
            for _ in 0..yields {
                h.entered(tid);
            }
            prop_assert!(!h.worked_since(m));
            for _ in 0..calls {
                h.worked(other);
            }
            let shared = (other as usize >> 2) % slots == (tid as usize >> 2) % slots;
            prop_assert_eq!(h.worked_since(m), calls > 0 && shared);
            h.worked(tid);
            prop_assert!(h.worked_since(m));
        }

        /// a mark is passed exactly when its thread (or one sharing its slot) has entered a
        /// call or run a slice since; other threads' calls and slices don't count, nor its own
        /// before the mark
        #[test]
        fn passed_tracks_the_signalers_calls(slots in 1usize..64, tid in (8u32..4096).prop_map(|t| t * 4), other in (8u32..4096).prop_map(|t| t * 4),
                                             before in proptest::collection::vec(any::<bool>(), 0..5), others in proptest::collection::vec(any::<bool>(), 0..5), slice in any::<bool>()) {
            let h = Handoff::new(slots);
            for &call in &before {
                if call { h.entered(tid) } else { h.progressed(tid) }
            }
            h.entered(tid);
            let m = h.mark(tid);
            prop_assert!(!h.passed(m));
            for &call in &others {
                if call { h.entered(other) } else { h.progressed(other) }
            }
            let shared = (other as usize >> 2) % slots == (tid as usize >> 2) % slots;
            prop_assert_eq!(h.passed(m), !others.is_empty() && shared);
            if slice { h.progressed(tid) } else { h.entered(tid) }
            prop_assert!(h.passed(m));
        }
    }
}
