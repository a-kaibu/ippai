//! Debugging between processes (the shared kernel's part of crate::dbg): which process
//! debugs which, and the debug events on their way. A debuggee's thread puts an event and
//! waits for the debugger's ContinueDebugEvent; the debugger takes the events of its
//! debuggees one at a time per process (the next after the current one was continued), as
//! Windows hands them out. Events no thread waits for (those a debugger asked for when
//! attaching, a process's end) need continuing all the same.

use std::collections::BTreeMap;

pub const DBG_CONTINUE: u32 = 0x0001_0002;
pub const DBG_EXCEPTION_NOT_HANDLED: u32 = 0x8001_0001;
/// ContinueDebugEvent's "hand it to me again later"
pub const DBG_REPLY_LATER: u32 = 0x4001_0001;
pub const EXCEPTION_DEBUG_EVENT: u32 = 1;
pub const EXIT_PROCESS_DEBUG_EVENT: u32 = 5;
/// what a debuggee that outlives its debugger exits with (DebugSetProcessKillOnExit)
pub const STATUS_DEBUGGER_INACTIVE: u32 = 0xC000_0354;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ev {
    pub id: u32,
    /// the debugger it goes to
    pub to: u32,
    pub pid: u32,
    pub tid: u32,
    pub code: u32,
    pub vals: Vec<u64>,
    pub data: Vec<u8>,
    /// a thread of the debuggee waits for its continuing
    pub wait: bool,
    pub delivered: bool,
}

#[derive(Default, Debug, PartialEq)]
pub struct Debugs {
    /// debuggee -> (the debug object (port) it reports to, whether the processes it starts
    /// are debugged too)
    pub of: BTreeMap<u32, (u32, bool)>,
    /// debug objects (NtCreateDebugObject): id -> (the process that has it, whether its
    /// debuggees end when it is closed: DEBUG_KILL_ON_CLOSE)
    pub ports: BTreeMap<u32, (u32, bool)>,
    pub events: Vec<Ev>,
    /// the continue status each waiting event got
    pub conts: BTreeMap<u32, u32>,
    pub next: u32,
    /// the debuggers AeDebug started, by id (one of them crashing starts no other)
    pub ae: BTreeMap<u32, bool>,
    /// debuggee threads suspended now: their events wait until they are resumed
    pub held: BTreeMap<u32, bool>,
}

/// What a waiting event gets when its debugger is gone: an exception goes on unhandled
fn left(code: u32) -> u32 {
    if code == EXCEPTION_DEBUG_EVENT { DBG_EXCEPTION_NOT_HANDLED } else { DBG_CONTINUE }
}

impl Debugs {
    /// The debug object process `pid` reports to
    pub fn debugger(&self, pid: u32) -> Option<u32> {
        self.of.get(&pid).map(|d| d.0)
    }
    /// The process that has debug object `port`
    pub fn owner(&self, port: u32) -> Option<u32> {
        self.ports.get(&port).map(|p| p.0)
    }
    /// NtCreateDebugObject in process `owner`: the new object's id
    pub fn create_port(&mut self, owner: u32, kill: bool) -> u32 {
        self.next += 1;
        self.ports.insert(self.next, (owner, kill));
        self.next
    }
    /// DebugObjectKillProcessOnExitInformation: false for no such object
    pub fn set_kill(&mut self, port: u32, kill: bool) -> bool {
        self.ports.get_mut(&port).map(|p| p.1 = kill).is_some()
    }
    /// Process `child` was started by `parent` with CreateProcess `flags` and the creating
    /// thread's debug object `port`: DEBUG_PROCESS (1) makes it the child's debugger (and of
    /// its own children unless DEBUG_ONLY_THIS_PROCESS (2) too), DEBUG_ONLY_THIS_PROCESS alone
    /// of it only; else a debuggee's child is debugged when the debuggee's are.
    pub fn spawned(&mut self, port: Option<u32>, parent: u32, child: u32, flags: u32) {
        if flags & 3 != 0 {
            if let Some(p) = port.filter(|p| self.ports.contains_key(p)) {
                self.of.insert(child, (p, flags & 3 == 1));
            }
        } else if let Some(&(d, true)) = self.of.get(&parent) {
            self.of.insert(child, (d, true));
        }
    }
    /// DebugActiveProcess with debug object `port` of process `me`: ERROR_ACCESS_DENIED for
    /// itself or a process debugged already
    pub fn attach(&mut self, port: u32, me: u32, pid: u32) -> Result<(), u32> {
        if me == pid || self.of.contains_key(&pid) || !self.ports.contains_key(&port) {
            return Err(5);
        }
        self.of.insert(pid, (port, false));
        Ok(())
    }
    /// DebugActiveProcessStop: its events go on as if the debugger had gone; ERROR_ACCESS_DENIED
    /// if `port` is not its debugger
    pub fn detach(&mut self, port: u32, pid: u32) -> Result<(), u32> {
        if self.debugger(pid) != Some(port) {
            return Err(5);
        }
        self.of.remove(&pid);
        self.drop_events(|e| e.pid == pid);
        Ok(())
    }
    fn drop_events(&mut self, gone: impl Fn(&Ev) -> bool) {
        let (out, keep): (Vec<Ev>, Vec<Ev>) = std::mem::take(&mut self.events).into_iter().partition(|e| gone(e));
        self.events = keep;
        for e in out.into_iter().filter(|e| e.wait) {
            self.conts.insert(e.id, left(e.code));
        }
    }
    /// A debuggee's event: its id (what `take` answers once it is continued), None when no one
    /// debugs `pid`
    pub fn post(&mut self, mut e: Ev) -> Option<u32> {
        e.to = self.debugger(e.pid)?;
        self.next += 1;
        e.id = self.next;
        e.delivered = false;
        self.events.push(e);
        Some(self.next)
    }
    /// WaitForDebugEvent: the next event for debug object `port`, from a process none of whose
    /// events waits for continuing
    pub fn next(&mut self, port: u32) -> Option<Ev> {
        let busy: Vec<u32> = self.events.iter().filter(|e| e.delivered).map(|e| e.pid).collect();
        let held = &self.held;
        let e = self.events.iter_mut().find(|e| e.to == port && !e.delivered && !busy.contains(&e.pid) && !held.contains_key(&e.tid))?;
        e.delivered = true;
        Some(e.clone())
    }
    /// Thread `tid` of a debuggee was suspended (`on`) or resumed to count 0
    pub fn hold(&mut self, tid: u32, on: bool) {
        if on {
            self.held.insert(tid, true);
        } else {
            self.held.remove(&tid);
        }
    }
    /// ContinueDebugEvent: false if `port` has no event of that thread to continue
    pub fn cont(&mut self, port: u32, pid: u32, tid: u32, status: u32) -> bool {
        let Some(i) = self.events.iter().position(|e| e.delivered && e.to == port && e.pid == pid && e.tid == tid) else { return false };
        if status == DBG_REPLY_LATER {
            // (handed out again, before the process's others)
            self.events[i].delivered = false;
            let e = self.events.remove(i);
            let at = self.events.iter().position(|x| x.pid == e.pid).unwrap_or(self.events.len()).min(i);
            self.events.insert(at, e);
            return true;
        }
        let e = self.events.remove(i);
        if e.wait {
            self.conts.insert(e.id, status);
        }
        true
    }
    /// The status event `id` was continued with, once it was
    pub fn take(&mut self, id: u32) -> Option<u32> {
        self.conts.remove(&id)
    }
    /// Debug object `port` was closed: its debuggees are detached, and the ones it kills
    /// (DEBUG_KILL_ON_CLOSE) are returned
    pub fn close_port(&mut self, port: u32) -> Vec<u32> {
        let Some((_, kill)) = self.ports.remove(&port) else { return Vec::new() };
        let mine: Vec<u32> = self.of.iter().filter(|(_, d)| d.0 == port).map(|(&p, _)| p).collect();
        for &p in &mine {
            self.of.remove(&p);
        }
        self.drop_events(|e| e.to == port);
        if kill { mine } else { Vec::new() }
    }
    /// Process `pid` ended with `code` (its first thread `tid`): its debugger gets
    /// EXIT_PROCESS; its debug objects close (crate::kern kills the debuggees returned)
    pub fn exited(&mut self, pid: u32, tid: u32, code: u32) -> Vec<u32> {
        // (a process's thread ids run from its id up)
        self.held.retain(|&t, _| !(pid..pid.saturating_add(0x10000)).contains(&t));
        // (what its own threads waited for: no one waits any more)
        self.events.retain(|e| !(e.pid == pid && e.wait));
        if let Some((d, _)) = self.of.remove(&pid) {
            self.next += 1;
            self.events.push(Ev { id: self.next, to: d, pid, tid, code: EXIT_PROCESS_DEBUG_EVENT, vals: vec![code as u64], ..Ev::default() });
        }
        let ports: Vec<u32> = self.ports.iter().filter(|(_, p)| p.0 == pid).map(|(&id, _)| id).collect();
        self.ae.remove(&pid);
        ports.into_iter().flat_map(|p| self.close_port(p)).collect()
    }
}

crate::enc_struct!(Ev { id, to, pid, tid, code, vals, data, wait, delivered });
crate::enc_struct!(Debugs { of, ports, events, conts, next, ae, held });

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kern::enc::Enc;
    use proptest::prelude::*;

    fn ev(pid: u32, tid: u32, code: u32, wait: bool) -> Ev {
        Ev { pid, tid, code, wait, ..Ev::default() }
    }

    #[test]
    fn one_event_per_process_at_a_time_in_order() {
        let mut d = Debugs::default();
        let port = d.create_port(0x10, true);
        d.spawned(Some(port), 0x10, 0x20, 1);
        d.spawned(Some(port), 0x10, 0x30, 2);
        let a = d.post(ev(0x20, 0x30, 3, true)).unwrap();
        d.post(ev(0x20, 0x30, 6, false)).unwrap();
        d.post(ev(0x30, 0x40, 3, true)).unwrap();
        assert_eq!(d.post(ev(0x99, 1, 3, true)), None, "not debugged");
        let e = d.next(port).unwrap();
        assert_eq!((e.pid, e.code), (0x20, 3));
        let e2 = d.next(port).unwrap();
        assert_eq!(e2.pid, 0x30, "the other process's event, not the first's next");
        assert!(d.next(port).is_none());
        assert_eq!(d.take(a), None);
        assert!(!d.cont(port, 0x20, 0x31, DBG_CONTINUE), "no event of that thread");
        assert!(d.cont(port, 0x20, 0x30, DBG_CONTINUE));
        assert_eq!(d.take(a), Some(DBG_CONTINUE));
        assert_eq!(d.next(port).unwrap().code, 6);
        let other = d.create_port(0x10, true);
        assert!(d.next(other).is_none(), "another debug object sees none of them");
    }

    #[test]
    fn reply_later_hands_the_event_out_again() {
        let mut d = Debugs::default();
        let port = d.create_port(0x10, true);
        d.spawned(Some(port), 0x10, 0x20, 1);
        let a = d.post(ev(0x20, 0x30, 3, true)).unwrap();
        d.post(ev(0x20, 0x30, 6, true)).unwrap();
        let e = d.next(port).unwrap();
        assert!(d.cont(port, 0x20, 0x30, DBG_REPLY_LATER));
        assert_eq!(d.take(a), None, "the thread still waits");
        assert_eq!(d.next(port).unwrap().id, e.id);
        assert!(d.cont(port, 0x20, 0x30, DBG_CONTINUE));
        assert_eq!(d.take(a), Some(DBG_CONTINUE));
        assert_eq!(d.next(port).unwrap().code, 6);
    }

    #[test]
    fn children_follow_debug_process_but_not_only_this_process() {
        let mut d = Debugs::default();
        let port = d.create_port(0x10, true);
        d.spawned(Some(port), 0x10, 0x20, 1);
        d.spawned(None, 0x20, 0x30, 0);
        assert_eq!(d.debugger(0x30), Some(port));
        d.spawned(Some(port), 0x10, 0x40, 2);
        d.spawned(None, 0x40, 0x50, 0);
        d.spawned(Some(port), 0x10, 0x60, 3);
        d.spawned(None, 0x60, 0x70, 0);
        assert_eq!((d.debugger(0x50), d.debugger(0x70)), (None, None));
        let p30 = d.create_port(0x30, true);
        d.spawned(Some(p30), 0x30, 0x80, 2);
        assert_eq!(d.debugger(0x80), Some(p30), "a debuggee can debug its own child");
        d.spawned(None, 0x10, 0x90, 1);
        assert_eq!(d.debugger(0x90), None, "DEBUG_PROCESS without a debug object");
    }

    #[test]
    fn attach_detach_and_the_debuggers_end() {
        let mut d = Debugs::default();
        let port = d.create_port(0x10, true);
        assert_eq!(d.attach(port, 0x10, 0x10), Err(5));
        assert_eq!(d.attach(port + 1, 0x10, 0x20), Err(5), "no such debug object");
        d.attach(port, 0x10, 0x20).unwrap();
        let p30 = d.create_port(0x30, true);
        assert_eq!(d.attach(p30, 0x30, 0x20), Err(5));
        let id = d.post(ev(0x20, 0x21, EXCEPTION_DEBUG_EVENT, true)).unwrap();
        assert_eq!(d.detach(p30, 0x20), Err(5));
        d.detach(port, 0x20).unwrap();
        assert_eq!(d.take(id), Some(DBG_EXCEPTION_NOT_HANDLED), "a detached exception goes on unhandled");
        d.attach(port, 0x10, 0x20).unwrap();
        d.attach(port, 0x10, 0x30).unwrap();
        assert!(d.set_kill(port, false));
        assert!(d.exited(0x10, 0x11, 0).is_empty());
        assert_eq!(d.debugger(0x20), None);
        assert!(!d.set_kill(port, true), "the ended process's debug objects are gone");
        let p40 = d.create_port(0x40, true);
        d.attach(p40, 0x40, 0x20).unwrap();
        assert_eq!(d.exited(0x40, 0x41, 0), vec![0x20], "killed on exit by default");
    }

    #[test]
    fn closing_a_debug_object_kills_or_lets_go() {
        let mut d = Debugs::default();
        let a = d.create_port(0x10, false);
        let b = d.create_port(0x10, true);
        d.spawned(Some(a), 0x10, 0x20, 1);
        d.spawned(Some(b), 0x10, 0x30, 1);
        let w = d.post(ev(0x20, 0x21, 2, true)).unwrap();
        assert!(d.close_port(a).is_empty());
        assert_eq!(d.take(w), Some(DBG_CONTINUE), "the debuggee goes on");
        assert_eq!(d.debugger(0x20), None);
        assert!(d.close_port(a).is_empty(), "twice: nothing");
        assert_eq!(d.close_port(b), vec![0x30]);
    }

    #[test]
    fn a_debuggees_end_reaches_its_debugger() {
        let mut d = Debugs::default();
        let port = d.create_port(0x10, true);
        d.spawned(Some(port), 0x10, 0x20, 1);
        d.post(ev(0x20, 0x30, 2, true)).unwrap();
        d.exited(0x20, 0x30, 7);
        let e = d.next(port).unwrap();
        assert_eq!((e.code, e.vals.clone(), e.tid), (EXIT_PROCESS_DEBUG_EVENT, vec![7], 0x30));
        assert!(d.cont(port, 0x20, 0x30, DBG_CONTINUE));
        assert!(d.events.is_empty() && d.conts.is_empty());
    }

    #[test]
    fn a_suspended_threads_events_wait_for_it() {
        let mut d = Debugs::default();
        let port = d.create_port(0x10, true);
        d.spawned(Some(port), 0x10, 0x20, 1);
        d.post(ev(0x20, 0x30, 3, true)).unwrap();
        d.post(ev(0x20, 0x40, 6, true)).unwrap();
        d.hold(0x30, true);
        assert_eq!(d.next(port).unwrap().tid, 0x40, "the other thread's goes first");
        assert!(d.cont(port, 0x20, 0x40, DBG_CONTINUE));
        assert!(d.next(port).is_none());
        d.hold(0x30, false);
        assert_eq!(d.next(port).unwrap().tid, 0x30);
        d.hold(0x21, true);
        d.exited(0x20, 0x30, 0);
        assert!(d.held.is_empty(), "the ended process's threads are forgotten");
    }

    proptest! {
        /// Whatever happens: at most one delivered event per process, every waiting event that
        /// leaves the queue gets a status exactly once, no suspended thread's event goes out,
        /// no debuggee reports to a debug object that is gone, and the state survives the
        /// encoding
        #[test]
        fn waiting_events_always_get_an_answer(ops in proptest::collection::vec((0u8..11, 0u32..4, 0u32..3), 0..60)) {
            let mut d = Debugs::default();
            let pids = [0x100u32, 0x200, 0x300, 0x400];
            let mut ports: Vec<u32> = vec![];
            let mut waiting: Vec<u32> = vec![];
            for (op, p, t) in ops {
                let pid = pids[p as usize];
                let port = ports.get(t as usize % ports.len().max(1)).copied().unwrap_or(0);
                match op {
                    0 => d.spawned(Some(port), 0x10, pid, 1 + t % 2),
                    1 => { let _ = d.attach(port, 0x10 + t, pid); }
                    2 => if let Some(id) = d.post(ev(pid, pid + t, 1 + t, true)) { waiting.push(id) },
                    3 => if let Some(e) = d.next(port) { prop_assert!(!d.held.contains_key(&e.tid), "a suspended thread's event went out") },
                    4 => { let e = d.events.iter().find(|e| e.delivered && e.pid == pid).cloned(); if let Some(e) = e { prop_assert!(d.cont(e.to, e.pid, e.tid, DBG_CONTINUE)); } }
                    5 => { let _ = d.detach(port, pid); }
                    6 => { d.exited(if t == 0 { 0x10 } else { pid }, 0, 0); }
                    7 => d.hold(pid + t, true),
                    8 => d.hold(pid + t, false),
                    9 => ports.push(d.create_port(if t == 0 { 0x10 } else { pid }, t != 1)),
                    _ => { d.close_port(port); }
                }
                let mut seen = vec![];
                for e in d.events.iter().filter(|e| e.delivered) {
                    prop_assert!(!seen.contains(&e.pid));
                    seen.push(e.pid);
                }
                for (_, (port, _)) in &d.of {
                    prop_assert!(d.ports.contains_key(port), "a debuggee of a closed debug object");
                }
                waiting.retain(|id| d.take(*id).is_none());
                let mut b = vec![];
                Enc::put(&d, &mut b);
                prop_assert_eq!(<Debugs as Enc>::get(&mut &b[..]).unwrap(), d.clone_state());
            }
            // (each still waiting is queued, or its process ended without anyone waiting)
            for id in &waiting {
                prop_assert!(d.events.iter().any(|e| e.id == *id && e.wait) || !d.conts.contains_key(id));
            }
        }
    }

    impl Debugs {
        fn clone_state(&self) -> Debugs {
            Debugs { of: self.of.clone(), ports: self.ports.clone(), events: self.events.clone(), conts: self.conts.clone(), next: self.next, ae: self.ae.clone(), held: self.held.clone() }
        }
    }
}
