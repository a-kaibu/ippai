//! SuspendThread / ResumeThread of guest threads that are running, for sake (threads created
//! suspended that have not started yet wait in sake's k32 GATE instead).
//!
//! Windows stops the target wherever it is; here the target stops at the top of its run loop
//! (kura::cpu, which the JIT returns to at least every jit_budget backward jumps) and stays
//! there until its count is back to 0. SuspendThread is asynchronous on Windows too ("the thread
//! may not be suspended when the function returns": GetThreadContext waits). A thread inside an
//! API call (a wait, Sleep) stops when that call returns to its code.
//!
//! A Delphi engine's multi-core renderer (TThread.Suspend from the main thread, then Resume) let
//! its workers run on while the main thread thought them stopped: they ended, and only the
//! main thread's strip of the screen was drawn (巨乳人妻３Ｐ催眠, はにデビ！ 体験版).
//!
//! Built with `--cfg loom` the types are loom's: tests/loom.rs checks every interleaving.

#[cfg(loom)]
use loom::sync::{atomic::AtomicU32, Condvar, Mutex};
#[cfg(not(loom))]
use std::sync::{atomic::AtomicU32, Condvar, Mutex};
use std::sync::atomic::Ordering;

/// Windows' MAXIMUM_SUSPEND_COUNT: one more SuspendThread fails.
pub const MAX_COUNT: u32 = 127;

pub struct Suspends {
    /// (thread id, suspend count), counts above 0 only
    list: Mutex<Vec<(u32, u32)>>,
    cv: Condvar,
    /// list.len(), read without the lock by every run loop
    any: AtomicU32,
}

impl Default for Suspends {
    fn default() -> Self {
        Self::new()
    }
}

impl Suspends {
    pub fn new() -> Self {
        Suspends { list: Mutex::new(Vec::new()), cv: Condvar::new(), any: AtomicU32::new(0) }
    }

    /// SuspendThread: the previous count, None when it is already MAX_COUNT.
    pub fn suspend(&self, tid: u32) -> Option<u32> {
        let mut l = self.list.lock().unwrap();
        match l.iter_mut().find(|e| e.0 == tid) {
            Some(e) if e.1 >= MAX_COUNT => None,
            Some(e) => {
                e.1 += 1;
                Some(e.1 - 1)
            }
            None => {
                l.push((tid, 1));
                self.any.store(l.len() as u32, Ordering::Release);
                Some(0)
            }
        }
    }

    /// ResumeThread: the previous count; at 0 the thread goes on.
    pub fn resume(&self, tid: u32) -> u32 {
        let mut l = self.list.lock().unwrap();
        let Some(i) = l.iter().position(|e| e.0 == tid) else { return 0 };
        let old = l[i].1;
        if old == 1 {
            l.swap_remove(i);
            self.any.store(l.len() as u32, Ordering::Release);
            drop(l);
            self.cv.notify_all();
        } else {
            l[i].1 -= 1;
        }
        old
    }

    pub fn count(&self, tid: u32) -> u32 {
        self.list.lock().unwrap().iter().find(|e| e.0 == tid).map_or(0, |e| e.1)
    }

    /// Whether some thread is suspended (cheap: the run loops ask before every slice).
    #[inline(always)]
    pub fn any(&self) -> bool {
        self.any.load(Ordering::Acquire) != 0
    }

    /// Thread `tid` at a safe point: waits while its count is above 0. `leave` is asked after
    /// every wake-up (`wake`): true returns at once (a save state wants the thread at the top of
    /// its run loop; it comes back here afterwards). Returns whether it waited.
    pub fn park(&self, tid: u32, leave: impl Fn() -> bool) -> bool {
        let mut l = self.list.lock().unwrap();
        let mut waited = false;
        while l.iter().any(|e| e.0 == tid) {
            if leave() {
                break;
            }
            waited = true;
            l = self.cv.wait(l).unwrap();
        }
        waited
    }

    /// Wakes every parked thread to ask its `leave` again.
    pub fn wake(&self) {
        drop(self.list.lock().unwrap());
        self.cv.notify_all();
    }

    /// A thread that ended (TerminateThread of a suspended thread): its entry goes.
    pub fn forget(&self, tid: u32) {
        let mut l = self.list.lock().unwrap();
        if let Some(i) = l.iter().position(|e| e.0 == tid) {
            l.swap_remove(i);
            self.any.store(l.len() as u32, Ordering::Release);
        }
    }
}

/// Property: any sequence of SuspendThread / ResumeThread / thread ends over a few threads
/// returns what Windows' per-thread suspend counts give (a map of counts, saturating at
/// MAX_COUNT for suspend and at 0 for resume), and `any` is true exactly while some count is
/// above 0.
#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    #[derive(Debug, Clone)]
    enum Op {
        Suspend(u32),
        Resume(u32),
        Forget(u32),
        /// 130 suspensions at once: past the limit
        Burst(u32),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            4 => (0u32..3).prop_map(Op::Suspend),
            4 => (0u32..3).prop_map(Op::Resume),
            1 => (0u32..3).prop_map(Op::Forget),
            1 => (0u32..3).prop_map(Op::Burst),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(300))]
        #[test]
        fn counts_match_a_model(ops in proptest::collection::vec(op(), 0..300)) {
            let s = Suspends::new();
            {
                let mut m: HashMap<u32, u32> = HashMap::new();
                for o in &ops {
                    match *o {
                        Op::Suspend(t) => {
                            let c = m.entry(t).or_insert(0);
                            let want = if *c >= MAX_COUNT { None } else { *c += 1; Some(*c - 1) };
                            assert_eq!(s.suspend(t), want);
                        }
                        Op::Resume(t) => {
                            let c = m.entry(t).or_insert(0);
                            let want = *c;
                            *c = c.saturating_sub(1);
                            assert_eq!(s.resume(t), want);
                        }
                        Op::Forget(t) => {
                            m.remove(&t);
                            s.forget(t);
                        }
                        Op::Burst(t) => {
                            for _ in 0..130 {
                                let c = m.entry(t).or_insert(0);
                                let want = if *c >= MAX_COUNT { None } else { *c += 1; Some(*c - 1) };
                                assert_eq!(s.suspend(t), want);
                            }
                        }
                    }
                    for t in 0..3 {
                        assert_eq!(s.count(t), m.get(&t).copied().unwrap_or(0));
                    }
                    assert_eq!(s.any(), m.values().any(|&c| c > 0));
                }
            }
        }
    }
}
