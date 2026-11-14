//! A process pinned to one processor (SetProcessAffinityMask with a single bit) runs its
//! threads' code one at a time (waits::uni, #199): a thread holds the processor while its
//! guest code runs ([`crate::run`]), lets it go in each API call ([`Api`], in api::dispatch),
//! while it stands still (a save state's park, SuspendThread) and when it ends, and gives it
//! up after a quantum while another waits (each slice of the run loop: [`slice`]).

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::time::Instant;
use waits::uni::{quantum_over, OneCpu};

static ON: AtomicBool = AtomicBool::new(false);
static CPU: std::sync::LazyLock<OneCpu> = std::sync::LazyLock::new(OneCpu::new);

thread_local! {
    /// Whether this thread holds the processor, and since when
    static HELD: Cell<Option<Instant>> = const { Cell::new(None) };
    /// kura::cpu::work then (waits::uni::QUANTUM_WORK)
    static WORK: Cell<u64> = const { Cell::new(0) };
}

/// From now on the threads' code runs one at a time (`one`: the affinity has a single bit;
/// a wider one later does not undo it: threads already rely on it).
pub fn pinned(one: bool) {
    if one && !ON.swap(true, Relaxed) {
        eprintln!("sake: the process asked for one processor: its threads run one at a time");
    }
}

/// Takes the processor unless this thread holds it already. Whether it took it.
pub fn enter() -> bool {
    if !ON.load(Relaxed) || HELD.get().is_some() {
        return false;
    }
    CPU.acquire();
    HELD.set(Some(Instant::now()));
    WORK.set(kura::cpu::work());
    true
}

/// Lets the processor go if this thread holds it. Whether it did.
pub fn leave() -> bool {
    if HELD.get().is_none() {
        return false;
    }
    HELD.set(None);
    CPU.release();
    true
}

/// Whether this thread holds the processor
pub fn held() -> bool {
    HELD.get().is_some()
}

/// An API call: the processor is let go for it and taken (back) for the program's code after
/// it, also when the call was the one that pinned the process
pub struct Api;
impl Api {
    pub fn new() -> Self {
        leave();
        Api
    }
}
impl Drop for Api {
    fn drop(&mut self) {
        enter();
    }
}

/// A slice of the run loop: after a quantum, the processor goes to a waiting thread first.
pub fn slice() {
    let Some(t) = HELD.get() else { return };
    let work = kura::cpu::work() - WORK.get();
    if quantum_over(t.elapsed(), work) {
        if CPU.contended() {
            leave();
            enter();
        } else {
            HELD.set(Some(Instant::now()));
            WORK.set(kura::cpu::work());
        }
    }
}
