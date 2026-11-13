//! A host thread's calls into the guest's objects, which a stop waits out: DirectShow's
//! IMediaFilter::Stop on a source filter returns once its streaming thread no longer calls
//! upstream (IAsyncReader::SyncRead) or downstream (IMemInputPin::Receive), so the game may
//! take those objects apart as soon as Stop returns. sake's MPEG-1 splitter thread kept reading
//! from KiriKiri krmovie's reader while the game destroyed it (アクアリウム 体験版, #230).
//!
//! The thread asks [`Calls::enter`] with the epoch it last looked at before each call and
//! [`Calls::leave`]s after it; a stop moves the epoch on and shuts the calls out until the next
//! [`Calls::run`] (no call of before gets in after that, nor a new one while stopped: the
//! thread need not look at the filter's state in any order), and waits for the call in flight
//! to leave. `tests/loom.rs` checks that no call runs
//! after a stop returned, in every interleaving.

#[cfg(loom)]
use loom::sync::{Condvar, Mutex};
#[cfg(not(loom))]
use std::sync::{Condvar, Mutex};

use crate::Host;
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    epoch: u32,
    inside: bool,
    /// between a run and a stop
    running: bool,
    closed: bool,
}

pub struct Calls {
    m: Mutex<State>,
    cv: Condvar,
}

impl Default for Calls {
    fn default() -> Self {
        Self::new()
    }
}

impl Calls {
    pub fn new() -> Self {
        Calls { m: Mutex::new(State::default()), cv: Condvar::new() }
    }

    /// The epoch now: what a call made later asks to enter with.
    pub fn epoch(&self) -> u32 {
        crate::relock(&self.m).epoch
    }

    /// Calls may go out (Pause or Run): until the next stop.
    pub fn run(&self) {
        crate::relock(&self.m).running = true;
    }

    /// Whether a call may go out now: running, no stop since `epoch`, no close.
    /// True: the call is in flight until [`Calls::leave`].
    pub fn enter(&self, epoch: u32) -> bool {
        let mut s = crate::relock(&self.m);
        if s.closed || !s.running || s.epoch != epoch || s.inside {
            return false;
        }
        s.inside = true;
        true
    }

    /// The call in flight came back.
    pub fn leave(&self) {
        let mut s = crate::relock(&self.m);
        s.inside = false;
        drop(s);
        self.cv.notify_all();
    }

    /// No call of an earlier epoch goes out from now on; waits up to `timeout` for the one in
    /// flight (a call that waits for something only this thread would do after the stop could
    /// wait forever). False: it was still in flight then.
    pub fn stop(&self, timeout: Duration, h: &impl Host) -> bool {
        self.stop_or_close(false, timeout, h)
    }

    /// No call ever again; waits as `stop`.
    pub fn close(&self, timeout: Duration, h: &impl Host) -> bool {
        self.stop_or_close(true, timeout, h)
    }

    fn stop_or_close(&self, close: bool, timeout: Duration, h: &impl Host) -> bool {
        let end = Instant::now() + timeout;
        let mut s = crate::relock(&self.m);
        s.epoch = s.epoch.wrapping_add(1);
        s.running = false;
        s.closed |= close;
        while s.inside {
            #[cfg(not(loom))]
            {
                let now = Instant::now();
                if now >= end {
                    return false;
                }
                if h.pausing() {
                    drop(s);
                    h.point();
                    s = crate::relock(&self.m);
                    continue;
                }
                s = self.cv.wait_timeout(s, (end - now).min(Duration::from_millis(50))).unwrap_or_else(|e| e.into_inner()).0;
            }
            #[cfg(loom)]
            {
                let _ = (end, h);
                s = self.cv.wait(s).unwrap_or_else(|e| e.into_inner());
            }
        }
        true
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use crate::NoPause;

    #[test]
    fn a_stop_shuts_out_the_epoch_before() {
        let c = Calls::new();
        let e = c.epoch();
        assert!(!c.enter(e), "not running yet");
        c.run();
        assert!(c.enter(e));
        assert!(!c.enter(e), "one call at a time");
        c.leave();
        assert!(c.stop(Duration::ZERO, &NoPause));
        assert!(!c.enter(e));
        let e2 = c.epoch();
        assert_ne!(e, e2);
        assert!(!c.enter(e2), "stopped: none until it runs again");
        c.run();
        assert!(!c.enter(e), "the epoch before the stop stays out");
        assert!(c.enter(e2));
        c.leave();
        assert!(c.close(Duration::ZERO, &NoPause));
        assert!(!c.enter(c.epoch()), "closed: none ever again");
    }

    #[test]
    fn a_stop_waits_for_the_call_in_flight() {
        let c = std::sync::Arc::new(Calls::new());
        c.run();
        assert!(c.enter(c.epoch()));
        let c2 = c.clone();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c2.leave();
        });
        let t0 = Instant::now();
        assert!(c.stop(Duration::from_secs(5), &NoPause));
        assert!(t0.elapsed() >= Duration::from_millis(40));
        t.join().unwrap();
    }

    #[test]
    fn a_call_that_never_comes_back_times_out() {
        let c = Calls::new();
        c.run();
        assert!(c.enter(c.epoch()));
        assert!(!c.stop(Duration::from_millis(20), &NoPause));
        // (it came back after all: the next stop finds none)
        c.leave();
        assert!(c.stop(Duration::ZERO, &NoPause));
    }
}
