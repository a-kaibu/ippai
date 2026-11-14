//! The queues behind sake's thread pool, as Windows has one per process: a few long-lived worker
//! threads take jobs from one queue ([`Workers`]: QueueUserWorkItem, thread pool work, the
//! callbacks of timers, waits and bound files), and one thread runs every timer when it is due
//! ([`Timers`]: thread pool timers, timer queues, the ends MCI waits for). Each of those used to
//! start a guest thread of its own (1 MiB of guest stack, a TEB, a Web Worker on the page).
//!
//! A job queued while no worker waits for it asks the caller for one more worker, up to a few
//! at once; beyond those the caller looks again a moment later ([`Workers::stalled`]) and adds
//! one only if no job was taken meanwhile, as Windows' pool grows (a burst of short jobs is run
//! by the few, a job waiting for another job still gets a thread). A worker that had nothing to
//! do for a while (beyond the few that stay) ends. Built with `--cfg loom`, `tests/loom.rs` checks that every job is
//! taken exactly once and none is left queued with no worker to take it.

#[cfg(loom)]
use loom::sync::{Condvar, Mutex};
#[cfg(not(loom))]
use std::sync::{Condvar, Mutex};

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

use crate::{relock, Host};

struct Queue<J> {
    jobs: VecDeque<J>,
    /// workers waiting for a job now
    idle: usize,
    /// workers started and not retired (counting ones the caller is starting)
    live: usize,
    /// jobs taken so far (whether the queue moved)
    taken: u64,
}

/// The jobs and the workers waiting for them.
pub struct Workers<J> {
    st: Mutex<Queue<J>>,
    cv: Condvar,
}

/// What [`Workers::submit`] asks of its caller.
#[derive(Debug, PartialEq)]
pub enum Need {
    /// a worker waits for it
    No,
    /// start one more worker (then [`Workers::not_started`] if it could not)
    Worker,
    /// enough workers run: look again a moment later with [`Workers::stalled`] (this is
    /// [`Workers::taken`] now)
    Check(u64),
}

/// What a worker got from [`Workers::take`].
pub enum Take<J> {
    Job(J),
    /// it idled long enough and others stay: it ends
    Retire,
}

impl<J> Default for Workers<J> {
    fn default() -> Self {
        Self::new()
    }
}

impl<J> Workers<J> {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Workers { st: Mutex::new(Queue { jobs: VecDeque::new(), idle: 0, live: 0, taken: 0 }), cv: Condvar::new() }
    }
    #[cfg(loom)]
    pub fn new() -> Self {
        Workers { st: Mutex::new(Queue { jobs: VecDeque::new(), idle: 0, live: 0, taken: 0 }), cv: Condvar::new() }
    }

    /// Queues `j`; whether the caller is to start one more worker (`soft`: how many may run
    /// before it only looks again later).
    pub fn submit(&self, j: J, soft: usize) -> Need {
        let mut g = relock(&self.st);
        g.jobs.push_back(j);
        let need = if g.jobs.len() <= g.idle {
            Need::No
        } else if g.live < soft {
            g.live += 1;
            Need::Worker
        } else {
            Need::Check(g.taken)
        };
        drop(g);
        self.cv.notify_one();
        need
    }

    /// A moment after [`Need::Check`]`(since)`: true (start one more worker) when jobs wait, no
    /// worker is free and none was taken since.
    pub fn stalled(&self, since: u64) -> bool {
        let mut g = relock(&self.st);
        if g.jobs.len() > g.idle && g.idle == 0 && g.taken == since {
            g.live += 1;
            return true;
        }
        false
    }

    /// Jobs taken so far.
    pub fn taken(&self) -> u64 {
        relock(&self.st).taken
    }

    /// Whether jobs wait with no worker free for them.
    pub fn backlog(&self) -> bool {
        let g = relock(&self.st);
        g.jobs.len() > g.idle
    }

    /// The worker [`Workers::submit`] asked for could not be started. With no worker left at
    /// all, the queued jobs come back (nobody would ever run them).
    pub fn not_started(&self) -> Vec<J> {
        let mut g = relock(&self.st);
        g.live -= 1;
        if g.live == 0 { g.jobs.drain(..).collect() } else { Vec::new() }
    }

    /// A worker's wait for its next job. A worker that waited `idle_for` with nothing to do
    /// retires while more than `keep` are left. While a state is taken the worker stands still
    /// in [`Host::point`], holding nothing and counted as busy (a restored copy waits again).
    pub fn take(&self, host: &impl Host, keep: usize, idle_for: Duration) -> Take<J> {
        let mut g = relock(&self.st);
        loop {
            if let Some(j) = g.jobs.pop_front() {
                g.taken += 1;
                return Take::Job(j);
            }
            if host.pausing() {
                drop(g);
                host.point();
                g = relock(&self.st);
                continue;
            }
            g.idle += 1;
            #[cfg(loom)]
            let timed_out = {
                let _ = idle_for;
                g = self.cv.wait(g).unwrap();
                false
            };
            #[cfg(not(loom))]
            let timed_out = {
                let (ng, t) = self.cv.wait_timeout(g, idle_for).unwrap_or_else(|e| e.into_inner());
                g = ng;
                t.timed_out()
            };
            g.idle -= 1;
            if timed_out && g.jobs.is_empty() && g.live > keep {
                g.live -= 1;
                return Take::Retire;
            }
        }
    }

    /// Wakes every waiting worker (a save state starts: they stand still).
    pub fn wake_all(&self) {
        drop(relock(&self.st));
        self.cv.notify_all();
    }

    /// (workers started and not retired, workers waiting, jobs queued)
    pub fn counts(&self) -> (usize, usize, usize) {
        let g = relock(&self.st);
        (g.live, g.idle, g.jobs.len())
    }
}

// ---------------------------------------------------------------------------------------------
// timers

/// When each timer is due (no lock: the rules alone). A key has one entry at most: scheduling
/// it again replaces the one before; among timers due at the same moment the one scheduled
/// first runs first.
pub struct Schedule<J> {
    order: BTreeMap<(Instant, u64), u64>,
    jobs: HashMap<u64, (Instant, u64, J)>,
    seq: u64,
}

impl<J> Default for Schedule<J> {
    fn default() -> Self {
        Schedule { order: BTreeMap::new(), jobs: HashMap::new(), seq: 0 }
    }
}

impl<J> Schedule<J> {
    /// `key` runs `job` at `at`; true if it replaced an entry of that key.
    pub fn set(&mut self, key: u64, at: Instant, job: J) -> bool {
        let old = self.cancel(key);
        self.seq += 1;
        self.order.insert((at, self.seq), key);
        self.jobs.insert(key, (at, self.seq, job));
        old
    }

    /// `key`'s entry goes (true if it had one).
    pub fn cancel(&mut self, key: u64) -> bool {
        match self.jobs.remove(&key) {
            Some((at, seq, _)) => {
                self.order.remove(&(at, seq));
                true
            }
            None => false,
        }
    }

    pub fn pending(&self, key: u64) -> bool {
        self.jobs.contains_key(&key)
    }

    /// When the first entry is due.
    pub fn next_due(&self) -> Option<Instant> {
        self.order.keys().next().map(|k| k.0)
    }

    /// The first entry due at `now` (it goes).
    pub fn pop_due(&mut self, now: Instant) -> Option<(u64, J)> {
        let (&(at, seq), &key) = self.order.iter().next()?;
        if at > now {
            return None;
        }
        self.order.remove(&(at, seq));
        self.jobs.remove(&key).map(|e| (key, e.2))
    }

    pub fn len(&self) -> usize {
        self.jobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.jobs.is_empty()
    }
}

/// A [`Schedule`] that one thread serves ([`Timers::next`]) and any thread changes.
pub struct Timers<J> {
    st: Mutex<Option<Schedule<J>>>,
    cv: Condvar,
}

impl<J> Default for Timers<J> {
    fn default() -> Self {
        Self::new()
    }
}

impl<J> Timers<J> {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Timers { st: Mutex::new(None), cv: Condvar::new() }
    }
    #[cfg(loom)]
    pub fn new() -> Self {
        Timers { st: Mutex::new(None), cv: Condvar::new() }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Schedule<J>) -> R) -> R {
        f(relock(&self.st).get_or_insert_with(Schedule::default))
    }

    /// `key` runs `job` at `at` (replacing its entry before).
    pub fn set(&self, key: u64, at: Instant, job: J) -> bool {
        let r = self.with(|s| s.set(key, at, job));
        self.cv.notify_all();
        r
    }

    pub fn cancel(&self, key: u64) -> bool {
        self.with(|s| s.cancel(key))
    }

    pub fn pending(&self, key: u64) -> bool {
        self.with(|s| s.pending(key))
    }

    /// The timer thread's wait for its next entry due (by `now`); stands still in
    /// [`Host::point`] while a state is taken.
    pub fn next(&self, host: &impl Host, now: impl Fn() -> Instant) -> (u64, J) {
        let mut g = relock(&self.st);
        loop {
            let s = g.get_or_insert_with(Schedule::default);
            let t = now();
            if let Some(e) = s.pop_due(t) {
                return e;
            }
            if host.pausing() {
                drop(g);
                host.point();
                g = relock(&self.st);
                continue;
            }
            // (an hour at most: only a safety net, a change wakes it)
            let wait = s.next_due().map_or(Duration::from_secs(3600), |d| d.saturating_duration_since(t));
            #[cfg(loom)]
            {
                let _ = wait;
                g = self.cv.wait(g).unwrap();
            }
            #[cfg(not(loom))]
            {
                g = self.cv.wait_timeout(g, wait).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }

    /// Wakes the timer thread (a save state starts).
    pub fn wake(&self) {
        drop(relock(&self.st));
        self.cv.notify_all();
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::Arc;

    #[derive(Debug, Clone)]
    enum Op {
        Set(u64, u64),
        Cancel(u64),
        Pop(u64),
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0..6u64, 0..50u64).prop_map(|(k, t)| Op::Set(k, t)),
            (0..6u64).prop_map(Op::Cancel),
            (0..60u64).prop_map(Op::Pop),
        ]
    }

    proptest! {
        /// Against a plain list of (key, due, order): one entry per key, entries come out due
        /// first, then in the order they were set, never before they are due.
        #[test]
        fn schedule_is_a_list_of_one_entry_per_key(ops in proptest::collection::vec(op(), 0..80)) {
            let base = Instant::now();
            let mut s = Schedule::<u32>::default();
            let mut model: Vec<(u64, u64, u64, u32)> = Vec::new();
            let mut seq = 0;
            for (i, o) in ops.iter().enumerate() {
                match *o {
                    Op::Set(k, t) => {
                        let had = model.iter().any(|e| e.0 == k);
                        model.retain(|e| e.0 != k);
                        seq += 1;
                        model.push((k, t, seq, i as u32));
                        prop_assert_eq!(s.set(k, base + Duration::from_millis(t), i as u32), had);
                    }
                    Op::Cancel(k) => {
                        let had = model.iter().any(|e| e.0 == k);
                        model.retain(|e| e.0 != k);
                        prop_assert_eq!(s.cancel(k), had);
                    }
                    Op::Pop(now) => {
                        let want = model.iter().filter(|e| e.1 <= now).min_by_key(|e| (e.1, e.2)).copied();
                        if let Some(w) = want {
                            model.retain(|e| e.0 != w.0);
                        }
                        prop_assert_eq!(s.pop_due(base + Duration::from_millis(now)), want.map(|w| (w.0, w.3)));
                    }
                }
                prop_assert_eq!(s.len(), model.len());
                prop_assert_eq!(s.next_due(), model.iter().map(|e| base + Duration::from_millis(e.1)).min());
                for k in 0..6 {
                    prop_assert_eq!(s.pending(k), model.iter().any(|e| e.0 == k));
                }
            }
        }
    }

    /// Workers that block in their jobs: every job is taken once, the pool grows to the jobs
    /// that ran at once and not beyond, and idle workers retire.
    #[test]
    fn workers_grow_to_what_runs_at_once_and_retire() {
        let w = Arc::new(Workers::<u32>::new());
        let gate = Arc::new(std::sync::Barrier::new(5));
        let done = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut threads = Vec::new();
        let start = |threads: &mut Vec<std::thread::JoinHandle<()>>| {
            let (w, gate, done) = (w.clone(), gate.clone(), done.clone());
            threads.push(std::thread::spawn(move || loop {
                match w.take(&crate::NoPause, 0, Duration::from_millis(100)) {
                    Take::Job(j) => {
                        if j < 4 {
                            gate.wait();
                        }
                        done.lock().unwrap().push(j);
                    }
                    Take::Retire => return,
                }
            }));
        };
        // four jobs that wait for each other: four workers
        for j in 0..4 {
            if w.submit(j, 8) == Need::Worker {
                start(&mut threads);
            }
        }
        gate.wait();
        while done.lock().unwrap().len() < 4 {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(w.counts().0, 4);
        // then 100 short ones, one after the other: no more workers
        for j in 4..104 {
            if w.submit(j, 8) == Need::Worker {
                start(&mut threads);
            }
            while done.lock().unwrap().len() < j as usize + 1 {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        assert_eq!(threads.len(), 4);
        // idle (none to keep): they retire
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(w.counts(), (0, 0, 0));
        let mut d = done.lock().unwrap().clone();
        d.sort();
        assert_eq!(d, (0..104).collect::<Vec<_>>());
    }

    #[test]
    fn a_worker_not_started_gives_the_jobs_back_when_none_is_left() {
        let w = Workers::<u32>::new();
        assert_eq!(w.submit(1, 8), Need::Worker);
        assert_eq!(w.not_started(), vec![1]);
        assert_eq!(w.counts(), (0, 0, 0));
    }

    /// Beyond `soft` workers a queued job asks for a look later; one more worker comes only if
    /// the queue did not move meanwhile.
    #[test]
    fn beyond_the_few_a_worker_comes_only_for_a_stalled_queue() {
        let w = Workers::<u32>::new();
        assert_eq!(w.submit(1, 1), Need::Worker);
        let Need::Check(s) = w.submit(2, 1) else { panic!() };
        // (nothing taken: stalled)
        assert!(w.stalled(s));
        assert_eq!(w.counts().0, 2);
        // a job taken since: not stalled
        let Need::Check(s) = w.submit(3, 1) else { panic!() };
        assert!(matches!(w.take(&crate::NoPause, 9, Duration::from_millis(1)), Take::Job(1)));
        assert!(!w.stalled(s));
        assert!(w.backlog());
    }

    /// The timer thread runs entries when due, in due order, and a change (an earlier entry)
    /// wakes it.
    #[test]
    fn timers_run_when_due_and_wake_for_an_earlier_one() {
        let t = Arc::new(Timers::<u32>::new());
        let base = Instant::now();
        t.set(1, base + Duration::from_millis(300), 1);
        let th = {
            let t = t.clone();
            std::thread::spawn(move || {
                let a = t.next(&crate::NoPause, Instant::now);
                let at = Instant::now();
                let b = t.next(&crate::NoPause, Instant::now);
                (a, at, b, Instant::now())
            })
        };
        std::thread::sleep(Duration::from_millis(30));
        t.set(2, base + Duration::from_millis(60), 2);
        let (a, at, b, bt) = th.join().unwrap();
        assert_eq!(a, (2, 2));
        assert!(at >= base + Duration::from_millis(60) && at < base + Duration::from_millis(250), "{:?}", at - base);
        assert_eq!(b, (1, 1));
        assert!(bt >= base + Duration::from_millis(300));
    }
}
