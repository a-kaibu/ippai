//! Write-behind: a value kept in memory whose changes are written to its file later and
//! together (sake's registry: an installer's 200 values are one file write, not 200), without
//! ever losing one that a later look at the files should see.
//!
//! Changes are made in transactions ([`Later::with`]). One that changed the value marks it
//! dirty; it is written at the end of a transaction when the caller asks for it at once
//! (`eager`), when it has waited `after`, or when someone asked for it while the transaction
//! went on ([`Later::flush_soon`]). [`Later::flush`] writes it now, waiting for a transaction
//! going on.
//!
//! [`Later::flush_soon`] is for a thread that must not wait (the page's main thread, before it
//! exports the saves, and the process's exit): it writes at once when nobody is in a
//! transaction, else it leaves a request the transaction takes up as it ends. Who may touch the
//! value is one atomic word (`busy`; transactions queue for it on a mutex, flush_soon only tries
//! it), and the request and the transaction's last look at it are a Dekker pair: flush_soon
//! stores the request and then tries the word; a transaction lets the word go and then looks for
//! a request (a SeqCst fence between each pair), so either flush_soon gets the word or the transaction sees the request,
//! never neither. `tests/loom.rs` (mod later) checks every interleaving: no change made before
//! a flush_soon began stays unwritten once everyone is done, and nothing is written twice for
//! one change (with the look before letting the word go, loom finds the lost one). (The word is
//! an atomic, not a try_lock of the mutex: loom does not order a failed try_lock against the
//! holder's unlock, so it would not see the race.)

#[cfg(loom)]
use loom::sync::atomic::{fence, AtomicBool};
#[cfg(loom)]
use loom::sync::{Mutex, MutexGuard};
#[cfg(not(loom))]
use std::sync::atomic::{fence, AtomicBool};
#[cfg(not(loom))]
use std::sync::{Mutex, MutexGuard};

use std::sync::atomic::Ordering::SeqCst;
use std::time::{Duration, Instant};

/// What a [`Later`] keeps: written by [`Store::write`].
pub trait Store {
    /// Writes the value out; false when that failed (it stays dirty and is tried again).
    fn write(&mut self) -> bool;
}

/// The value and how far it is from its file
pub struct Held<T> {
    pub value: T,
    /// changed since written: since when
    dirty: Option<Instant>,
}

impl<T: Store> Held<T> {
    /// Changed since written
    pub fn dirty(&self) -> bool {
        self.dirty.is_some()
    }
    fn flush(&mut self) {
        if self.dirty.is_some() && self.value.write() {
            self.dirty = None;
        }
    }
}

pub struct Later<T> {
    /// the value; touched only by the one holding `busy`
    held: Mutex<Option<Held<T>>>,
    /// transactions and flush, one after the other (flush_soon does not queue)
    queue: Mutex<()>,
    /// someone has the value
    busy: AtomicBool,
    /// a flush_soon that found someone with the value
    asap: AtomicBool,
}

impl<T: Store> Default for Later<T> {
    fn default() -> Self {
        Self::new()
    }
}


impl<T: Store> Later<T> {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        Later { held: Mutex::new(None), queue: Mutex::new(()), busy: AtomicBool::new(false), asap: AtomicBool::new(false) }
    }
    #[cfg(loom)]
    pub fn new() -> Self {
        Later { held: Mutex::new(None), queue: Mutex::new(()), busy: AtomicBool::new(false), asap: AtomicBool::new(false) }
    }

    fn held(&self) -> MutexGuard<'_, Option<Held<T>>> {
        self.held.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `busy` for a thread that may wait (it is held only by one in the queue, or by a
    /// flush_soon for one write)
    fn take(&self) -> MutexGuard<'_, ()> {
        let q = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        while self.busy.compare_exchange(false, true, SeqCst, SeqCst).is_err() {
            std::hint::spin_loop();
            #[cfg(loom)]
            loom::thread::yield_now();
            #[cfg(not(loom))]
            std::thread::yield_now();
        }
        q
    }

    /// Lets `busy` go, then looks for a request left meanwhile (by a flush_soon that found it
    /// busy): every holder does, whatever it did with the value
    fn release(&self) {
        self.busy.store(false, SeqCst);
        // (the Dekker pair with flush_soon: loom models SeqCst accesses as acquire/release
        // only, the fence is what orders this store before that load)
        fence(SeqCst);
        if self.asap.load(SeqCst) {
            self.flush_soon();
        }
    }

    /// A transaction: `f` gets the value (made by `init` the first time) and says whether it
    /// changed it. Written at its end when `eager`, when the first change waiting has waited
    /// `after`, or when a flush_soon asked meanwhile.
    pub fn with<R>(&self, init: impl FnOnce() -> T, eager: bool, after: Duration, f: impl FnOnce(&mut Held<T>) -> (R, bool)) -> R {
        let q = self.take();
        let r = {
            let mut g = self.held();
            let h = g.get_or_insert_with(|| Held { value: init(), dirty: None });
            let (r, changed) = f(h);
            if changed && h.dirty.is_none() {
                h.dirty = Some(Instant::now());
            }
            if h.dirty.is_some_and(|t| eager || t.elapsed() >= after) {
                h.flush();
            }
            r
        };
        self.release();
        drop(q);
        r
    }

    /// The value as it is (None before the first transaction), for a look that changes nothing
    /// the file needs (the caller's own bookkeeping may change)
    pub fn peek<R>(&self, f: impl FnOnce(Option<&mut Held<T>>) -> R) -> R {
        let _q = self.take();
        let r = f(self.held().as_mut());
        self.release();
        r
    }

    /// Writes what waits now (waiting for a transaction going on).
    pub fn flush(&self) {
        let _q = self.take();
        self.asap.store(false, SeqCst);
        if let Some(h) = self.held().as_mut() {
            h.flush();
        }
        self.release();
    }

    /// Writes what waits without waiting: at once when nobody has the value (true), else the one
    /// who has it writes it as it lets go (false).
    pub fn flush_soon(&self) -> bool {
        self.asap.store(true, SeqCst);
        fence(SeqCst);
        if self.busy.compare_exchange(false, true, SeqCst, SeqCst).is_err() {
            return false;
        }
        self.asap.store(false, SeqCst);
        if let Some(h) = self.held().as_mut() {
            h.flush();
        }
        self.release();
        true
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;

    #[derive(Default)]
    struct File {
        value: u32,
        written: Vec<u32>,
        fail: bool,
    }
    impl Store for File {
        fn write(&mut self) -> bool {
            if self.fail {
                return false;
            }
            self.written.push(self.value);
            true
        }
    }

    fn bump(l: &Later<File>, eager: bool, after: Duration) {
        l.with(File::default, eager, after, |h| {
            h.value.value += 1;
            ((), true)
        })
    }
    fn written(l: &Later<File>) -> Vec<u32> {
        l.peek(|h| h.map(|h| h.value.written.clone()).unwrap_or_default())
    }

    /// Changes wait for flush, flush_soon, eager or `after`; a look without a change writes
    /// nothing; a failed write is tried again
    #[test]
    fn written_later_together() {
        let l = Later::new();
        for _ in 0..200 {
            bump(&l, false, Duration::MAX);
        }
        assert!(written(&l).is_empty());
        assert!(l.flush_soon());
        assert_eq!(written(&l), vec![200]);
        l.with(File::default, false, Duration::ZERO, |_| ((), false));
        l.flush();
        assert_eq!(written(&l), vec![200], "nothing changed");
        bump(&l, true, Duration::MAX);
        assert_eq!(written(&l), vec![200, 201]);
        bump(&l, false, Duration::ZERO);
        assert_eq!(written(&l), vec![200, 201, 202]);
        l.peek(|h| h.unwrap().value.fail = true);
        bump(&l, true, Duration::MAX);
        assert!(l.peek(|h| h.unwrap().dirty()));
        l.peek(|h| h.unwrap().value.fail = false);
        l.flush();
        assert_eq!(written(&l), vec![200, 201, 202, 203]);
        assert!(!l.peek(|h| h.unwrap().dirty()));
    }

    /// A flush_soon during a transaction is written by that transaction as it ends
    #[test]
    fn flush_soon_during_a_transaction() {
        let l = std::sync::Arc::new(Later::new());
        let (tx, rx) = std::sync::mpsc::channel();
        let (go, wait) = std::sync::mpsc::channel::<()>();
        let l2 = l.clone();
        let t = std::thread::spawn(move || {
            l2.with(File::default, false, Duration::MAX, |h| {
                h.value.value = 7;
                tx.send(()).unwrap();
                wait.recv().unwrap();
                ((), true)
            })
        });
        rx.recv().unwrap();
        assert!(!l.flush_soon(), "a transaction goes on");
        go.send(()).unwrap();
        t.join().unwrap();
        assert_eq!(written(&l), vec![7]);
    }
}
