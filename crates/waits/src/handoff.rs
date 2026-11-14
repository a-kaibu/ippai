//! The moments right after a thread signals an event, as Windows schedules them: the signaling
//! thread runs on (the waiter it readied starts microseconds later, on another core) so what
//! it writes just after SetEvent is there when the waiter looks. Games depend on it without
//! knowing: EntisGLS (#158) runs a command on its window thread through an object on the
//! poster's stack, `SetEvent(done); this->signaled = 1;`, and by the time the poster has woken
//! and returned, that store lands in its reused stack (a saved ebx = 1, a crash 30 ms later).
//! Under sake the signaler can stall right there for milliseconds (a block to translate, a
//! busy host), so a waiter that was blocked and woke because another thread signaled waits for
//! that thread to make its next API call (it has then passed the stores after SetEvent) or for
//! [`CAP`], whichever comes first.
//!
//! Each guest thread has a counter its API calls advance ([`Handoff::entered`]); signaling
//! records the signaler's counter ([`Handoff::mark`]); the woken waiter waits until it moves
//! ([`Handoff::wait_passed`]). Under `--cfg loom` the wait has no cap: `tests/loom.rs` checks
//! that what the signaler stored before its next call is what the waiter reads.

#[cfg(loom)]
use loom::sync::atomic::AtomicU32;
#[cfg(not(loom))]
use std::sync::atomic::AtomicU32;
use std::sync::atomic::Ordering::{Acquire, Release};
use std::time::Duration;

/// The longest a woken waiter waits for its signaler (a signaler spinning without API calls
/// only delays the waiter this much; in the browser a signaler stalls 2-8 ms now and then)
pub const CAP: Duration = Duration::from_millis(30);

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
}

impl Handoff {
    pub fn new(slots: usize) -> Self {
        Handoff { seq: (0..slots.max(1)).map(|_| AtomicU32::new(0)).collect() }
    }
    fn slot(&self, tid: u32) -> &AtomicU32 {
        // (Windows thread ids are multiples of 4)
        &self.seq[(tid as usize >> 2) % self.seq.len()]
    }
    /// Thread `tid` enters an API call: what it stored before is visible to a waiter that sees
    /// the counter move
    pub fn entered(&self, tid: u32) {
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

    proptest! {
        /// a mark is passed exactly when its thread (or one sharing its slot) has entered a
        /// call since; other threads' calls don't count
        #[test]
        fn passed_tracks_the_signalers_calls(slots in 1usize..64, tid in (8u32..4096).prop_map(|t| t * 4), other in (8u32..4096).prop_map(|t| t * 4), calls in 0u32..5) {
            let h = Handoff::new(slots);
            h.entered(tid);
            let m = h.mark(tid);
            prop_assert!(!h.passed(m));
            for _ in 0..calls {
                h.entered(other);
            }
            let shared = (other as usize >> 2) % slots == (tid as usize >> 2) % slots;
            prop_assert_eq!(h.passed(m), calls > 0 && shared);
            h.entered(tid);
            prop_assert!(h.passed(m));
        }
    }
}
