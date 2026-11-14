//! One processor for a process that asked for one (SetProcessAffinityMask with a single bit):
//! its threads' code runs one thread at a time, as Windows runs it on that processor. Programs
//! pin themselves there and then share state between threads without locks. 二代目は☆魔法少女
//! (AIMS, #199) does: its loader thread and its main thread both unpack LZSS into one global
//! ring buffer, and when sake ran them at the same time each got the other's bytes ("file not
//! found" for a PNG that D3DX could not read, at a different file every run).
//!
//! A thread holds the processor while its guest code runs and lets it go in every API call (a
//! call may block: a wait for another thread of the program) and for good when it ends. The
//! ones waiting get it in the order they asked (a ticket lock: none waits behind a thread that
//! keeps taking it back), and a thread that used up its quantum ([`quantum_over`]) while
//! another waits gives it up at the next slice of its run loop, as the scheduler's quantum
//! does, so a thread that spins on a flag another one sets still lets that one run.
//!
//! Under `--cfg loom` the condition variable has no timeout: `tests/loom.rs` checks that two
//! threads' code between their calls never overlaps and that a waiter always gets its turn.

#[cfg(loom)]
use loom::sync::{Condvar, Mutex};
#[cfg(not(loom))]
use std::sync::{Condvar, Mutex};
use std::time::Duration;

/// A thread's quantum: it lets a waiting thread have the processor once its code ran this
/// long ([`quantum_over`]). Windows' quantum is about 2 clock ticks (31 ms) of native code.
/// Time alone is a bad measure here: a loaded host deschedules the thread for longer than
/// that in the middle of what it does between two calls (an LZSS unpack of a 150 KB PNG,
/// which Windows never cuts), so the quantum also needs [`QUANTUM_WORK`] guest instructions
/// (about: kura::cpu::work), some 10 ms of native code. [`QUANTUM_MAX`] caps it.
pub const QUANTUM: Duration = Duration::from_millis(30);
pub const QUANTUM_WORK: u64 = 30_000_000;
pub const QUANTUM_MAX: Duration = Duration::from_secs(5);

/// Whether a quantum of `ran` host time and `work` guest instructions is used up
pub fn quantum_over(ran: Duration, work: u64) -> bool {
    (ran >= QUANTUM && work >= QUANTUM_WORK) || ran >= QUANTUM_MAX
}

#[derive(Default)]
struct Tickets {
    /// the next ticket to hand out
    next: u64,
    /// the ticket that holds the processor (none when it equals `next`)
    serving: u64,
}

/// The processor: whoever holds the ticket being served runs.
pub struct OneCpu {
    t: Mutex<Tickets>,
    cv: Condvar,
}

impl Default for OneCpu {
    fn default() -> Self {
        Self::new()
    }
}

impl OneCpu {
    pub fn new() -> Self {
        OneCpu { t: Mutex::new(Tickets::default()), cv: Condvar::new() }
    }
    /// Waits for the processor (behind every thread that asked before).
    pub fn acquire(&self) {
        let mut g = self.t.lock().unwrap_or_else(|e| e.into_inner());
        let me = g.next;
        g.next += 1;
        while g.serving != me {
            #[cfg(loom)]
            {
                g = self.cv.wait(g).unwrap();
            }
            #[cfg(not(loom))]
            {
                // (the timeout only a safety net)
                g = self.cv.wait_timeout(g, Duration::from_millis(50)).unwrap_or_else(|e| e.into_inner()).0;
            }
        }
    }
    /// Lets the processor go to the next one waiting (the caller holds it).
    pub fn release(&self) {
        let mut g = self.t.lock().unwrap_or_else(|e| e.into_inner());
        debug_assert!(g.serving < g.next, "released without holding it");
        g.serving += 1;
        let waiting = g.serving < g.next;
        drop(g);
        if waiting {
            self.cv.notify_all();
        }
    }
    /// Whether a thread waits for it (the holder asks, to give up its quantum).
    pub fn contended(&self) -> bool {
        let g = self.t.lock().unwrap_or_else(|e| e.into_inner());
        g.next > g.serving + 1
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU32, Ordering::SeqCst};
    use std::sync::Arc;

    #[test]
    fn uncontended_takes_and_gives() {
        let c = OneCpu::new();
        assert!(!c.contended());
        c.acquire();
        assert!(!c.contended());
        c.release();
        c.acquire();
        c.release();
    }

    /// The AIMS shape with real threads: each thread unpacks into one shared buffer between
    /// its calls (no lock of its own); holding the processor there, no thread ever sees
    /// another one's bytes, however the host schedules them.
    #[test]
    fn code_between_calls_never_overlaps() {
        let c = Arc::new(OneCpu::new());
        let inside = Arc::new(AtomicU32::new(0));
        let buf = Arc::new(std::sync::Mutex::new([0u32; 64]));
        let t: Vec<_> = (1..=4u32)
            .map(|id| {
                let (c, inside, buf) = (c.clone(), inside.clone(), buf.clone());
                std::thread::spawn(move || {
                    for round in 0..200 {
                        c.acquire();
                        assert_eq!(inside.fetch_add(1, SeqCst), 0, "two threads run at once");
                        for i in 0..64 {
                            buf.lock().unwrap()[i] = id;
                            if (round + i) % 17 == 0 {
                                std::thread::yield_now();
                            }
                        }
                        assert!(buf.lock().unwrap().iter().all(|&x| x == id), "another thread's bytes");
                        inside.fetch_sub(1, SeqCst);
                        c.release(); // an API call
                    }
                })
            })
            .collect();
        for t in t {
            t.join().unwrap();
        }
    }

    /// A spinner that gives the processor up each quantum lets the thread it waits for run.
    #[test]
    fn a_spinner_lets_the_setter_run() {
        let c = Arc::new(OneCpu::new());
        let flag = Arc::new(AtomicU32::new(0));
        c.acquire();
        let (c2, f2) = (c.clone(), flag.clone());
        let setter = std::thread::spawn(move || {
            c2.acquire();
            f2.store(1, SeqCst);
            c2.release();
        });
        let mut slices = 0;
        while flag.load(SeqCst) == 0 {
            // (a slice of the run loop)
            if c.contended() {
                c.release();
                c.acquire();
            }
            slices += 1;
            assert!(slices < 10_000_000);
            std::hint::spin_loop();
        }
        c.release();
        setter.join().unwrap();
    }

    #[test]
    fn a_quantum_needs_time_and_work() {
        let ms = Duration::from_millis;
        assert!(!quantum_over(ms(500), 3000), "a descheduled thread keeps it");
        assert!(!quantum_over(ms(5), 10_000), "too soon");
        assert!(quantum_over(QUANTUM, QUANTUM_WORK));
        assert!(quantum_over(QUANTUM_MAX, 0), "the interpreter's cap");
    }

    proptest! {
        /// quantum_over is monotonic in both time and work
        #[test]
        fn quantum_is_monotonic(t in 0u64..8000, w in 0u64..100_000_000, dt in 0u64..8000, dw in 0u64..100_000_000) {
            let ms = Duration::from_millis;
            if quantum_over(ms(t), w) {
                prop_assert!(quantum_over(ms(t + dt), w + dw));
            }
        }

        /// However the turns are asked for, the processor goes in ticket order, and contended()
        /// says whether one waits behind the holder.
        #[test]
        fn tickets_go_in_order(n in 1usize..6) {
            let c = Arc::new(OneCpu::new());
            c.acquire();
            let order = Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut hs = Vec::new();
            for k in 0..n {
                let (c2, o2) = (c.clone(), order.clone());
                hs.push(std::thread::spawn(move || {
                    c2.acquire();
                    o2.lock().unwrap().push(k);
                    c2.release();
                }));
                // (each asks only after the one before has its ticket)
                while { let g = c.t.lock().unwrap(); g.next as usize != k + 2 } {
                    std::thread::yield_now();
                }
                prop_assert!(c.contended());
            }
            c.release();
            for h in hs {
                h.join().unwrap();
            }
            prop_assert_eq!(order.lock().unwrap().clone(), (0..n).collect::<Vec<_>>());
            prop_assert!(!c.contended());
        }
    }
}
