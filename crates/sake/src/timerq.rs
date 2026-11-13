//! Timer queues (CreateTimerQueue, CreateTimerQueueTimer, ChangeTimerQueueTimer,
//! DeleteTimerQueueTimer, DeleteTimerQueue(Ex)) as Windows runs them: each timer calls
//! callback(param, TRUE) on a pool thread when due, then every period; a once-only timer
//! fires once and then stays (ChangeTimerQueueTimer still succeeds on it, but it does not run
//! again). Deleting a timer whose callback runs pends (ERROR_IO_PENDING) unless the caller
//! waits for it (INVALID_HANDLE_VALUE); an event given is set once no callback runs. A NULL
//! event always pends, and a timer already due then still fires that once.
//!
//! The rules live in `Timer` (host-only state, tested below); each timer has a guest thread
//! that sleeps until it is due and makes the calls.

#![allow(non_snake_case)]

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem;
use crate::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

const ERROR_IO_PENDING: u32 = 997;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_PARAMETER: u32 = 87;
const INVALID_HANDLE_VALUE: u32 = u32::MAX;

/// A timer's schedule and state (no guest memory: the rules alone)
#[derive(Debug, Clone, PartialEq)]
pub struct Timer {
    pub queue: u32,
    pub cb: u32,
    pub param: u32,
    /// when it fires next (None: a once-only timer that fired)
    pub due: Option<Instant>,
    pub period: Duration,
    /// callbacks running now
    pub running: u32,
    pub deleted: bool,
    /// deleted with a NULL event while due: it fires that once still
    pub grace: bool,
    /// events to set once no callback runs (after the deletion)
    pub events: Vec<u32>,
}

/// What DeleteTimerQueueTimer does with its event argument
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Completion {
    /// NULL: return at once, always ERROR_IO_PENDING
    None,
    /// INVALID_HANDLE_VALUE: wait until no callback runs
    Wait,
    /// an event: set once no callback runs
    Event(u32),
}

impl Timer {
    pub fn new(queue: u32, cb: u32, param: u32, now: Instant, due_ms: u32, period_ms: u32) -> Timer {
        Timer {
            queue,
            cb,
            param,
            due: Some(now + Duration::from_millis(due_ms as u64)),
            period: Duration::from_millis(period_ms as u64),
            running: 0,
            deleted: false,
            grace: false,
            events: Vec::new(),
        }
    }

    /// Whether it fires at `now`; if so, the next due time is set (a once-only timer has none
    /// left) and the callback counted as running.
    pub fn fire(&mut self, now: Instant) -> bool {
        let Some(d) = self.due else { return false };
        if now < d || (self.deleted && !self.grace) {
            return false;
        }
        self.grace = false;
        self.due = if self.period.is_zero() { None } else { Some((d + self.period).max(now)) };
        self.running += 1;
        true
    }

    /// A callback returned; the events to set now (a deleted timer with none running)
    pub fn done(&mut self) -> Vec<u32> {
        self.running = self.running.saturating_sub(1);
        if self.deleted && self.running == 0 && !self.grace { std::mem::take(&mut self.events) } else { Vec::new() }
    }

    /// ChangeTimerQueueTimer: false for a deleted timer; a once-only timer that fired keeps
    /// its state (and true)
    pub fn change(&mut self, now: Instant, due_ms: u32, period_ms: u32) -> bool {
        if self.deleted {
            return false;
        }
        if self.due.is_none() && self.period.is_zero() {
            return true;
        }
        self.due = Some(now + Duration::from_millis(due_ms as u64));
        self.period = Duration::from_millis(period_ms as u64);
        true
    }

    /// DeleteTimerQueueTimer's first half: Ok(events to set now) when the call succeeds at
    /// once, Err(ERROR_IO_PENDING) when it pends; Completion::Wait callers then wait for
    /// `idle()`.
    pub fn delete(&mut self, now: Instant, how: Completion) -> Result<Vec<u32>, u32> {
        let due_now = self.due.is_some_and(|d| d <= now);
        self.deleted = true;
        match how {
            Completion::None => {
                self.grace = due_now && self.running == 0;
                Err(ERROR_IO_PENDING)
            }
            Completion::Wait => Ok(Vec::new()),
            Completion::Event(e) => {
                if self.running == 0 {
                    Ok(vec![e])
                } else {
                    self.events.push(e);
                    Err(ERROR_IO_PENDING)
                }
            }
        }
    }

    /// Nothing left to run: it can go
    pub fn idle(&self) -> bool {
        self.running == 0 && !self.grace
    }
}

#[derive(Default)]
struct State {
    timers: HashMap<u32, Timer>,
    /// queue -> (deleted, event to set once its timers are idle)
    queues: HashMap<u32, (bool, Vec<u32>)>,
}
static ST: Mutex<Option<State>> = Mutex::new(None);
static CV: Condvar = Condvar::new();

fn lock() -> std::sync::MutexGuard<'static, Option<State>> {
    ST.lock().unwrap_or_else(|e| e.into_inner())
}
fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    f(lock().get_or_insert_with(State::default))
}

fn set_events(evs: Vec<u32>) {
    for e in evs {
        if e != 0 && e != INVALID_HANDLE_VALUE {
            crate::k32::set_event(e);
        }
    }
}

/// The default queue (CreateTimerQueueTimer with no queue)
const DEFAULT: u32 = 0;

fn CreateTimerQueue(_: &mut Ctx) -> u32 {
    let q = mem::heap_alloc(16, true);
    with(|s| s.queues.insert(q, (false, Vec::new())));
    q
}

/// CreateTimerQueueTimer(phandle*, queue, callback, param, due ms, period ms, flags)
fn CreateTimerQueueTimer(c: &mut Ctx) -> u32 {
    let (out, q, cb, param, due, period) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
    if out == 0 || cb == 0 {
        last_error(ERROR_INVALID_PARAMETER);
        return 0;
    }
    if q != DEFAULT && !with(|s| s.queues.get(&q).is_some_and(|x| !x.0)) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    let t = mem::heap_alloc(16, true);
    with(|s| s.timers.insert(t, Timer::new(q, cb, param, Instant::now(), due, period)));
    mem::putp(out, t);
    static ENTRY: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    let entry = *ENTRY.get_or_init(|| crate::api::host_fn("timer_queue_thread", timer_thread, 4));
    crate::spawn_gated(entry, t, 0x10000, crate::obj::new_thread_obj(0), false);
    1
}

/// A timer's pool thread: sleeps until it is due, calls it, until it is deleted and idle
fn timer_thread(c: &mut Ctx) -> u32 {
    let t = c.a(0);
    loop {
        let mut g = lock();
        let s = g.get_or_insert_with(State::default);
        let Some(tm) = s.timers.get_mut(&t) else { break };
        let now = Instant::now();
        if tm.fire(now) {
            let (cb, param) = (tm.cb, tm.param);
            drop(g);
            c.call(cb, &[param, 1]);
            let evs = with(|s| s.timers.get_mut(&t).map(|tm| tm.done()).unwrap_or_default());
            set_events(evs);
            CV.notify_all();
            continue;
        }
        if tm.deleted && tm.idle() {
            let q = tm.queue;
            s.timers.remove(&t);
            let qevs = queue_done(s, q);
            drop(g);
            set_events(qevs);
            CV.notify_all();
            mem::heap_free(t);
            break;
        }
        // (asleep until due, a change or a deletion; in short steps for save states and
        // TerminateThread)
        let wait = tm.due.map_or(Duration::from_millis(50), |d| d.saturating_duration_since(now)).min(Duration::from_millis(50));
        let _ = CV.wait_timeout(g, wait);
        crate::snap::point();
    }
    crate::k32::exit_thread(0)
}

/// The events of a deleted queue whose timers have all gone
fn queue_done(s: &mut State, q: u32) -> Vec<u32> {
    if q == DEFAULT || s.timers.values().any(|t| t.queue == q) {
        return Vec::new();
    }
    match s.queues.get(&q) {
        Some((true, _)) => s.queues.remove(&q).map(|x| x.1).unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// ChangeTimerQueueTimer(queue, timer, due ms, period ms)
fn ChangeTimerQueueTimer(c: &mut Ctx) -> u32 {
    let (t, due, period) = (c.a(1), c.a(2), c.a(3));
    let r = with(|s| s.timers.get_mut(&t).map(|tm| tm.change(Instant::now(), due, period)));
    CV.notify_all();
    match r {
        Some(true) => 1,
        _ => {
            last_error(ERROR_INVALID_PARAMETER);
            0
        }
    }
}

fn completion(ev: u32) -> Completion {
    match ev {
        0 => Completion::None,
        INVALID_HANDLE_VALUE => Completion::Wait,
        e => Completion::Event(e),
    }
}

/// Waits until timer `t` has no callback left to run (it may be gone)
fn wait_idle(t: u32) {
    let mut g = lock();
    loop {
        let s = g.get_or_insert_with(State::default);
        if s.timers.get(&t).is_none_or(|tm| tm.idle()) {
            return;
        }
        g = CV.wait_timeout(g, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
    }
}

/// DeleteTimerQueueTimer(queue, timer, completion event)
fn DeleteTimerQueueTimer(c: &mut Ctx) -> u32 {
    let (t, ev) = (c.a(1), c.a(2));
    let how = completion(ev);
    let r = with(|s| s.timers.get_mut(&t).map(|tm| tm.delete(Instant::now(), how)));
    CV.notify_all();
    match r {
        None => {
            last_error(ERROR_INVALID_PARAMETER);
            0
        }
        Some(Ok(evs)) => {
            if how == Completion::Wait {
                wait_idle(t);
            }
            set_events(evs);
            1
        }
        Some(Err(e)) => {
            last_error(e);
            0
        }
    }
}

/// DeleteTimerQueueEx(queue, completion event): its timers deleted (INVALID_HANDLE_VALUE waits
/// for their callbacks; an event is set once they are done)
fn DeleteTimerQueueEx(c: &mut Ctx) -> u32 {
    let (q, ev) = (c.a(0), c.a(1));
    if q == DEFAULT || !with(|s| s.queues.get(&q).is_some_and(|x| !x.0)) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    let how = completion(ev);
    let now = Instant::now();
    let (ts, evs) = with(|s| {
        let ts: Vec<u32> = s.timers.iter().filter(|(_, t)| t.queue == q).map(|(&h, _)| h).collect();
        let mut evs = Vec::new();
        for h in &ts {
            if let Some(tm) = s.timers.get_mut(h) {
                // (each timer as a waited-for deletion: no grace run, no event of its own)
                let _ = tm.delete(now, Completion::Wait);
            }
        }
        let entry = s.queues.get_mut(&q).unwrap();
        entry.0 = true;
        if let Completion::Event(e) = how {
            entry.1.push(e);
        }
        evs.extend(queue_done(s, q));
        (ts, evs)
    });
    CV.notify_all();
    if how == Completion::Wait {
        for t in ts {
            wait_idle(t);
        }
    }
    set_events(evs);
    1
}

fn DeleteTimerQueue(c: &mut Ctx) -> u32 {
    let q = c.a(0);
    if q == DEFAULT || !with(|s| s.queues.contains_key(&q)) {
        last_error(ERROR_INVALID_HANDLE);
        return 0;
    }
    // (as DeleteTimerQueueEx(queue, NULL))
    with(|s| {
        let now = Instant::now();
        for t in s.timers.values_mut().filter(|t| t.queue == q) {
            let _ = t.delete(now, Completion::Wait);
        }
        if let Some(e) = s.queues.get_mut(&q) {
            e.0 = true;
        }
        queue_done(s, q);
    });
    CV.notify_all();
    1
}

pub fn init() {
    for dll in ["kernel32", "kernelbase"] {
        implement(dll, "CreateTimerQueue", CreateTimerQueue);
        implement(dll, "CreateTimerQueueTimer", CreateTimerQueueTimer);
        implement(dll, "ChangeTimerQueueTimer", ChangeTimerQueueTimer);
        implement(dll, "DeleteTimerQueueTimer", DeleteTimerQueueTimer);
        implement(dll, "DeleteTimerQueueEx", DeleteTimerQueueEx);
        implement(dll, "DeleteTimerQueue", DeleteTimerQueue);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn once_only_fires_once_and_change_keeps_it_spent() {
        let t0 = Instant::now();
        let mut t = Timer::new(1, 0x1000, 0, t0, 10, 0);
        assert!(!t.fire(t0));
        assert!(t.fire(t0 + ms(10)));
        assert!(t.done().is_empty());
        assert!(!t.fire(t0 + ms(1000)));
        // (cb4: changed from its own callback, it does not run again)
        assert!(t.change(t0 + ms(20), 50, 50));
        assert!(!t.fire(t0 + ms(1000)));
    }

    #[test]
    fn change_before_it_fires_moves_it() {
        let t0 = Instant::now();
        let mut t = Timer::new(1, 0x1000, 0, t0, 10_000, 0);
        assert!(t.change(t0, 0, 0));
        assert!(t.fire(t0));
    }

    #[test]
    fn deleting_pends_while_running_and_sets_the_event_after() {
        let t0 = Instant::now();
        let mut t = Timer::new(1, 0x1000, 0, t0, 0, 0);
        assert!(t.fire(t0));
        assert_eq!(t.delete(t0, Completion::Event(0x40)), Err(ERROR_IO_PENDING));
        assert!(!t.idle());
        assert_eq!(t.done(), vec![0x40]);
        assert!(t.idle());
        // idle: the event at once
        let mut u = Timer::new(1, 0x1000, 0, t0, 0, 0);
        assert!(u.fire(t0));
        u.done();
        assert_eq!(u.delete(t0, Completion::Event(0x44)), Ok(vec![0x44]));
    }

    #[test]
    fn a_null_delete_pends_and_a_due_timer_fires_once_more() {
        let t0 = Instant::now();
        let mut t = Timer::new(1, 0x1000, 0, t0, 0, 300);
        assert_eq!(t.delete(t0, Completion::None), Err(ERROR_IO_PENDING));
        assert!(!t.idle());
        assert!(t.fire(t0));
        t.done();
        assert!(t.idle());
        assert!(!t.fire(t0 + ms(600)));
        // (cb6: deleted from its own callback: no second run)
        let mut u = Timer::new(1, 0x1000, 0, t0, 100, 100);
        assert!(u.fire(t0 + ms(100)));
        assert_eq!(u.delete(t0 + ms(100), Completion::None), Err(ERROR_IO_PENDING));
        u.done();
        assert!(!u.fire(t0 + ms(1000)));
        assert!(!u.change(t0, 10, 10));
    }

    #[derive(Debug, Clone)]
    enum Op {
        Advance(u64),
        Fire,
        Done,
        Change(u32, u32),
        Delete(u8),
    }
    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u64..200).prop_map(Op::Advance),
            Just(Op::Fire),
            Just(Op::Done),
            (0u32..200, 0u32..100).prop_map(|(d, p)| Op::Change(d, p)),
            (0u8..3).prop_map(Op::Delete),
        ]
    }

    proptest! {
        /// Whatever happens: a timer never runs before it is due, a deleted one runs at most
        /// its one grace call, an event is handed out once and only when nothing runs, and
        /// running never goes negative
        #[test]
        fn timer_invariants(due in 0u32..200, period in 0u32..100, ops in proptest::collection::vec(op(), 1..40)) {
            let t0 = Instant::now();
            let mut now = t0;
            let mut t = Timer::new(1, 0x1000, 0, t0, due, period);
            let mut after_delete = 0;
            let mut events_seen = Vec::new();
            for o in ops {
                match o {
                    Op::Advance(n) => now += ms(n),
                    Op::Fire => {
                        let before = t.due;
                        let deleted = t.deleted;
                        if t.fire(now) {
                            prop_assert!(before.is_some_and(|d| d <= now));
                            if deleted {
                                after_delete += 1;
                            }
                        }
                    }
                    Op::Done => {
                        if t.running > 0 {
                            let evs = t.done();
                            if !evs.is_empty() {
                                prop_assert_eq!(t.running, 0);
                            }
                            events_seen.extend(evs);
                        }
                    }
                    Op::Change(d, p) => {
                        let was = t.clone();
                        let ok = t.change(now, d, p);
                        prop_assert_eq!(ok, !was.deleted);
                    }
                    Op::Delete(k) => {
                        if t.deleted {
                            continue;
                        }
                        let how = match k { 0 => Completion::None, 1 => Completion::Wait, _ => Completion::Event(0x40) };
                        let running = t.running;
                        match t.delete(now, how) {
                            Ok(evs) => {
                                prop_assert!(how != Completion::None);
                                if how == Completion::Event(0x40) {
                                    prop_assert_eq!(running, 0);
                                }
                                events_seen.extend(evs);
                            }
                            Err(e) => {
                                prop_assert_eq!(e, ERROR_IO_PENDING);
                                prop_assert!(how == Completion::None || running > 0);
                            }
                        }
                    }
                }
            }
            prop_assert!(after_delete <= 1);
            prop_assert!(events_seen.len() <= 1);
        }
    }
}
