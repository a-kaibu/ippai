//! Synchronization barriers (InitializeSynchronizationBarrier, EnterSynchronizationBarrier,
//! DeleteSynchronizationBarrier), as Windows 10 1607 and later do them (Wine's kernel32:sync
//! test_barrier):
//! - a barrier of N threads lets its waiters go once the Nth enters; that one gets TRUE, the
//!   others FALSE, and the next N start a new phase (a barrier of 1: every call is TRUE);
//! - DeleteSynchronizationBarrier waits until the threads waiting without
//!   SYNCHRONIZATION_BARRIER_FLAGS_NO_DELETE have left (else it returns at once), and leaves
//!   the barrier usable;
//! - initializing a barrier again lets the threads waiting on it go;
//! - a flag other than SPIN_ONLY, BLOCK_ONLY, NO_DELETE raises STATUS_INVALID_PARAMETER.
//! The state is the host's, by the barrier's address (nothing reads the guest's bytes).

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::Duration;

pub const SPIN_ONLY: u32 = 1;
pub const BLOCK_ONLY: u32 = 2;
pub const NO_DELETE: u32 = 4;
pub const VALID: u32 = SPIN_ONLY | BLOCK_ONLY | NO_DELETE;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Bar {
    total: u32,
    count: u32,
    /// which initialization, and which phase of it
    epoch: u64,
    phase: u64,
    /// waiters of this epoch without NO_DELETE
    deletable: u32,
}

/// A waiter's ticket: it waits until its barrier's phase or epoch moves on
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Ticket {
    epoch: u64,
    phase: u64,
    no_delete: bool,
}

#[derive(Debug, PartialEq)]
pub enum Enter {
    /// the last of the phase: TRUE
    Last,
    Wait(Ticket),
}

/// Every barrier (the pure part)
#[derive(Default)]
pub struct Bars {
    map: HashMap<u32, Bar>,
    epochs: u64,
}

impl Bars {
    pub fn init(&mut self, b: u32, total: u32) {
        self.epochs += 1;
        self.map.insert(b, Bar { total, epoch: self.epochs, ..Default::default() });
    }
    fn bar(&mut self, b: u32) -> &mut Bar {
        let epochs = &mut self.epochs;
        self.map.entry(b).or_insert_with(|| {
            // (never initialized: a barrier of one)
            *epochs += 1;
            Bar { total: 1, epoch: *epochs, ..Default::default() }
        })
    }
    pub fn enter(&mut self, b: u32, no_delete: bool) -> Enter {
        let bar = self.bar(b);
        bar.count += 1;
        if bar.count >= bar.total {
            bar.count = 0;
            bar.phase += 1;
            return Enter::Last;
        }
        if !no_delete {
            bar.deletable += 1;
        }
        Enter::Wait(Ticket { epoch: bar.epoch, phase: bar.phase, no_delete })
    }
    /// Whether the waiter with ticket `t` may go
    pub fn released(&self, b: u32, t: Ticket) -> bool {
        self.map.get(&b).is_none_or(|bar| bar.epoch != t.epoch || bar.phase != t.phase)
    }
    /// The waiter has gone
    pub fn leave(&mut self, b: u32, t: Ticket) {
        if let Some(bar) = self.map.get_mut(&b) {
            if bar.epoch == t.epoch && !t.no_delete {
                bar.deletable = bar.deletable.saturating_sub(1);
            }
        }
    }
    /// Whether DeleteSynchronizationBarrier may return
    pub fn deletable(&self, b: u32) -> bool {
        self.map.get(&b).is_none_or(|bar| bar.deletable == 0)
    }
}

static BARS: Mutex<Option<Bars>> = Mutex::new(None);
static CV: Condvar = Condvar::new();

fn with<R>(f: impl FnOnce(&mut Bars) -> R) -> R {
    f(BARS.lock().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(Bars::default))
}

/// Waits until `done` (rechecked on every change, and now and then for a pause or a
/// TerminateThread)
fn wait_until(done: impl Fn(&Bars) -> bool) {
    let mut g = BARS.lock().unwrap_or_else(|e| e.into_inner());
    while !done(g.get_or_insert_with(Bars::default)) {
        g = CV.wait_timeout(g, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
        drop(g);
        crate::snap::point();
        crate::k32::check_terminate();
        g = BARS.lock().unwrap_or_else(|e| e.into_inner());
    }
}

fn InitializeSynchronizationBarrier(c: &mut Ctx) -> u32 {
    let (b, total, spin) = (c.a(0), c.a(1) as i32, c.a(2) as i32);
    if b == 0 || total < 1 || spin < -1 {
        crate::k32::last_error(87);
        return 0;
    }
    with(|s| s.init(b, total as u32));
    CV.notify_all();
    1
}

fn EnterSynchronizationBarrier(c: &mut Ctx) -> u32 {
    let (b, flags) = (c.a(0), c.a(1));
    if flags & !VALID != 0 {
        crate::k32::raise_from_api(c, 0xC000_000D, 1, 2);
        return 0;
    }
    match with(|s| s.enter(b, flags & NO_DELETE != 0)) {
        Enter::Last => {
            CV.notify_all();
            1
        }
        Enter::Wait(t) => {
            wait_until(|s| s.released(b, t));
            with(|s| s.leave(b, t));
            CV.notify_all();
            0
        }
    }
}

fn DeleteSynchronizationBarrier(c: &mut Ctx) -> u32 {
    let b = c.a(0);
    wait_until(|s| s.deletable(b));
    1
}

pub fn init() {
    for dll in ["kernel32", "kernelbase"] {
        implement(dll, "InitializeSynchronizationBarrier", InitializeSynchronizationBarrier);
        implement(dll, "EnterSynchronizationBarrier", EnterSynchronizationBarrier);
        implement(dll, "DeleteSynchronizationBarrier", DeleteSynchronizationBarrier);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn the_last_of_a_phase_gets_true_and_lets_the_others_go() {
        let mut s = Bars::default();
        s.init(0x100, 3);
        let Enter::Wait(a) = s.enter(0x100, false) else { panic!() };
        let Enter::Wait(b) = s.enter(0x100, true) else { panic!() };
        assert!(!s.released(0x100, a) && !s.deletable(0x100));
        assert_eq!(s.enter(0x100, false), Enter::Last);
        assert!(s.released(0x100, a) && s.released(0x100, b));
        assert!(!s.deletable(0x100), "until the waiter without NO_DELETE left");
        s.leave(0x100, b);
        assert!(!s.deletable(0x100));
        s.leave(0x100, a);
        assert!(s.deletable(0x100));
        // the next phase starts over
        assert!(matches!(s.enter(0x100, false), Enter::Wait(_)));
    }

    #[test]
    fn a_barrier_of_one_and_an_unknown_one_always_return_true() {
        let mut s = Bars::default();
        s.init(0x10, 1);
        for _ in 0..3 {
            assert_eq!(s.enter(0x10, false), Enter::Last);
            assert_eq!(s.enter(0x20, true), Enter::Last);
        }
    }

    #[test]
    fn initializing_again_releases_the_waiters() {
        let mut s = Bars::default();
        s.init(0x10, 5);
        let Enter::Wait(t) = s.enter(0x10, false) else { panic!() };
        s.init(0x10, 5);
        assert!(s.released(0x10, t));
        s.leave(0x10, t);
        assert!(s.deletable(0x10), "an old waiter does not count against the new barrier");
    }

    proptest! {
        /// Over any number of phases of a barrier of n: exactly one TRUE per n entries, every
        /// waiter of a finished phase may go, and once all left Delete does not wait.
        #[test]
        fn one_true_per_phase(n in 1u32..8, phases in 1u32..6, no_delete in proptest::collection::vec(any::<bool>(), 48)) {
            let mut s = Bars::default();
            s.init(0x40, n);
            let mut waiting = vec![];
            let mut trues = 0;
            for i in 0..n * phases {
                match s.enter(0x40, no_delete[i as usize % 48]) {
                    Enter::Last => {
                        trues += 1;
                        for t in waiting.drain(..) {
                            prop_assert!(s.released(0x40, t));
                            s.leave(0x40, t);
                        }
                    }
                    Enter::Wait(t) => {
                        prop_assert!(!s.released(0x40, t));
                        waiting.push(t);
                    }
                }
            }
            prop_assert_eq!(trues, phases);
            prop_assert!(waiting.is_empty());
            prop_assert!(s.deletable(0x40));
        }
    }
}
