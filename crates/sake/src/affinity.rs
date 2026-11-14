//! Processor affinity: the process's mask and each thread's (SetThreadAffinityMask,
//! Get/SetThreadGroupAffinity, Get/SetProcessAffinityMask, SetThreadIdealProcessor), as
//! Windows keeps them (Wine's kernel32:thread test_thread_processor):
//! - a thread starts with the process's mask; SetProcessAffinityMask sets every thread's too;
//! - SetThreadAffinityMask takes a mask within the process's (all bits: the process's), not 0;
//! - SetThreadGroupAffinity takes any mask of the processors there are (group 0), 0 for the
//!   process's; one beyond the process's widens the process's to it;
//! - the ideal processor is below MAXIMUM_PROCESSORS (that value asks for it).
//! The host schedules the threads, except that a process that pins itself to a single
//! processor runs them one at a time (crate::uni).

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::HashMap;
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct Masks {
    /// the processors there are
    pub all: u32,
    pub process: u32,
    threads: HashMap<u32, u32>,
    ideal: HashMap<u32, u32>,
}

impl Masks {
    pub fn new(all: u32) -> Self {
        Masks { all, process: all, threads: HashMap::default(), ideal: HashMap::default() }
    }
    pub fn created(&mut self, tid: u32) {
        self.threads.insert(tid, self.process);
    }
    pub fn thread(&self, tid: u32) -> u32 {
        self.threads.get(&tid).copied().unwrap_or(self.process)
    }
    /// SetThreadAffinityMask: the old mask, or ERROR_INVALID_PARAMETER
    pub fn set_thread(&mut self, tid: u32, mask: u32) -> Result<u32, u32> {
        let mask = if mask == u32::MAX { self.process } else { mask };
        if mask == 0 || mask & !self.process != 0 {
            return Err(87);
        }
        Ok(self.threads.insert(tid, mask).unwrap_or(self.process))
    }
    /// SetThreadGroupAffinity (group 0): the old mask, or ERROR_INVALID_PARAMETER
    pub fn set_group(&mut self, tid: u32, mask: u32) -> Result<u32, u32> {
        if mask & !self.all != 0 {
            return Err(87);
        }
        let mask = if mask == 0 { self.process } else { mask };
        self.process |= mask;
        Ok(self.threads.insert(tid, mask).unwrap_or(self.process))
    }
    pub fn set_process(&mut self, mask: u32) -> Result<(), u32> {
        if mask == 0 || mask & !self.all != 0 {
            return Err(87);
        }
        self.process = mask;
        self.threads.values_mut().for_each(|m| *m = mask);
        Ok(())
    }
    /// SetThreadIdealProcessor: the old one (MAXIMUM_PROCESSORS: only asks), or
    /// ERROR_INVALID_PARAMETER
    pub fn set_ideal(&mut self, tid: u32, n: u32, max: u32) -> Result<u32, u32> {
        if n > max {
            return Err(87);
        }
        let old = self.ideal.get(&tid).copied().unwrap_or(0);
        if n < max {
            self.ideal.insert(tid, n);
        }
        Ok(old)
    }
    pub fn gone(&mut self, tid: u32) {
        self.threads.remove(&tid);
        self.ideal.remove(&tid);
    }
}

static MASKS: Mutex<Option<Masks>> = Mutex::new(None);

pub fn with<R>(f: impl FnOnce(&mut Masks) -> R) -> R {
    let mut g = MASKS.lock().unwrap_or_else(|e| e.into_inner());
    f(g.get_or_insert_with(|| Masks::new(crate::k32::cpu_mask())))
}

fn fail(e: u32) -> u32 {
    crate::k32::last_error(e);
    0
}

fn SetThreadAffinityMask(c: &mut Ctx) -> u32 {
    let Some(tid) = crate::k32::any_thread_tid(c.a(0)) else { return fail(6) };
    with(|m| m.set_thread(tid, c.a(1))).unwrap_or_else(fail)
}

fn SetProcessAffinityMask(c: &mut Ctx) -> u32 {
    match with(|m| m.set_process(c.a(1))) {
        Ok(()) => {
            // (a single processor: the threads run one at a time, as there; #199)
            crate::uni::pinned(c.a(1).count_ones() == 1);
            1
        }
        Err(e) => fail(e),
    }
}

fn GetProcessAffinityMask(c: &mut Ctx) -> u32 {
    let (p, all) = with(|m| (m.process, m.all));
    crate::mem::putp(c.a(1), p);
    crate::mem::putp(c.a(2), all);
    1
}

/// GROUP_AFFINITY: Mask (pointer-sized), Group (WORD), Reserved[3]
fn put_group(p: u32, mask: u32) {
    crate::mem::fill(p, crate::mem::ps() + 8, 0);
    crate::mem::wp(p, mask);
}

fn GetThreadGroupAffinity(c: &mut Ctx) -> u32 {
    let Some(tid) = crate::k32::any_thread_tid(c.a(0)) else { return fail(6) };
    if c.a(1) == 0 {
        return fail(87);
    }
    put_group(c.a(1), with(|m| m.thread(tid)));
    1
}

fn SetThreadGroupAffinity(c: &mut Ctx) -> u32 {
    let Some(tid) = crate::k32::any_thread_tid(c.a(0)) else { return fail(6) };
    let new = c.a(1);
    if new == 0 || crate::mem::accessible(new, crate::mem::ps() + 8, false, false) == 0 {
        return fail(998); // ERROR_NOACCESS
    }
    let ps = crate::mem::ps();
    let wide = if ps == 8 { crate::mem::r64(new) } else { crate::mem::r32(new) as u64 };
    if crate::mem::r16(new + ps) != 0 || wide > u32::MAX as u64 {
        return fail(87);
    }
    match with(|m| m.set_group(tid, wide as u32)) {
        Ok(old) => {
            if c.a(2) != 0 {
                put_group(c.a(2), old);
            }
            1
        }
        Err(e) => fail(e),
    }
}

fn SetThreadIdealProcessor(c: &mut Ctx) -> u32 {
    let Some(tid) = crate::k32::any_thread_tid(c.a(0)) else {
        crate::k32::last_error(6);
        return u32::MAX;
    };
    let max = if crate::mem::x64() { 64 } else { 32 };
    with(|m| m.set_ideal(tid, c.a(1), max)).unwrap_or_else(|e| {
        crate::k32::last_error(e);
        u32::MAX
    })
}

pub fn init() {
    for dll in ["kernel32", "kernelbase"] {
        implement(dll, "SetThreadAffinityMask", SetThreadAffinityMask);
        implement(dll, "SetProcessAffinityMask", SetProcessAffinityMask);
        implement(dll, "GetProcessAffinityMask", GetProcessAffinityMask);
        implement(dll, "GetThreadGroupAffinity", GetThreadGroupAffinity);
        implement(dll, "SetThreadGroupAffinity", SetThreadGroupAffinity);
        implement(dll, "SetThreadIdealProcessor", SetThreadIdealProcessor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// kernel32:thread test_thread_processor's steps, on 4 processors
    #[test]
    fn windows_steps() {
        let mut m = Masks::new(0xf);
        m.created(1);
        assert_eq!(m.set_thread(1, 0xf), Ok(0xf));
        assert_eq!(m.set_thread(1, 0x10), Err(87));
        assert_eq!(m.set_thread(1, u32::MAX), Ok(0xf));
        assert_eq!(m.set_thread(1, 0), Err(87));
        assert_eq!(m.set_group(1, 0xe), Ok(0xf));
        assert_eq!(m.set_group(1, 0), Ok(0xe));
        assert_eq!(m.set_group(1, 0xf), Ok(0xf));
        m.set_process(0x1).unwrap();
        m.created(2);
        assert_eq!((m.thread(1), m.thread(2)), (1, 1));
        assert_eq!(m.set_group(1, 0), Ok(1));
        assert_eq!(m.set_group(1, 0xe), Ok(1));
        assert_eq!(m.process, 0xf, "a group mask beyond the process's widens it");
        m.created(3);
        assert_eq!((m.thread(2), m.thread(3), m.thread(1)), (1, 0xf, 0xe));
        assert_eq!(m.set_group(1, 0), Ok(0xe));
        assert_eq!(m.thread(1), 0xf);
        assert_eq!(m.set_group(1, u32::MAX), Err(87));
        assert_eq!(m.set_ideal(1, 33, 32), Err(87));
        assert_eq!(m.set_ideal(1, 3, 32), Ok(0));
        assert_eq!(m.set_ideal(1, 32, 32), Ok(3));
        assert_eq!(m.set_ideal(1, 32, 32), Ok(3));
    }

    proptest! {
        /// Whatever is asked: every thread's mask is nonzero, within the processors there are,
        /// and the process's mask is too; SetThreadAffinityMask keeps threads within it.
        #[test]
        fn masks_stay_valid(ops in proptest::collection::vec((0u8..4, 0u32..4, any::<u32>()), 0..40)) {
            let mut m = Masks::new(0x3f);
            for t in 0..4 {
                m.created(t);
            }
            for (op, t, v) in ops {
                let r = match op {
                    0 => m.set_thread(t, v).map(|_| ()),
                    1 => m.set_group(t, v).map(|_| ()),
                    2 => m.set_process(v),
                    _ => m.set_ideal(t, v % 70, 64).map(|_| ()),
                };
                if op == 0 && r.is_ok() {
                    prop_assert_eq!(m.thread(t) & !m.process, 0);
                }
                prop_assert!(m.process != 0 && m.process & !m.all == 0);
                for t in 0..4 {
                    prop_assert!(m.thread(t) != 0 && m.thread(t) & !m.all == 0);
                }
            }
        }
    }
}
