//! Who waits on which kernel object, so that a change to one object wakes only its waiters
//! (sake's obj.rs: WaitForSingleObject / WaitForMultipleObjects on events, mutexes, semaphores).
//!
//! The list holds no lock of its own: its owner keeps it under the lock that guards the objects,
//! gives each waiter a condition variable of its own (by the waiter's id) and notifies the ids
//! these calls return. Every waiter takes broadcasts ([`WaitList::wake_all`], for changes nobody
//! names an object for); [`WaitList::wake`] wakes the waiters of one object: every wait-all
//! waiter of it and up to `n` wait-any ones not yet woken (an auto-reset event or a mutex lets
//! one of them through, a semaphore released by `n` lets `n`).
//!
//! What keeps that from losing a wakeup: a wait-any waiter woken while its object is signaled
//! takes something (that object, or an earlier one of its list) and returns, and a waiter that
//! goes away ([`WaitList::remove`]) hands its wakeup on: its owner wakes one more waiter of each
//! of its objects that is still signaled. tests/loom.rs checks this on every interleaving.

/// One waiter's entry.
struct Entry<K> {
    id: u64,
    keys: Vec<K>,
    /// WaitForMultipleObjects with bWaitAll: woken by any change of any of its objects
    all: bool,
    /// woken since it last went to sleep
    poked: bool,
}

pub struct WaitList<K> {
    next: u64,
    /// oldest first: the order the wait-any waiters of an object are woken in
    list: Vec<Entry<K>>,
}

impl<K> Default for WaitList<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K> WaitList<K> {
    pub const fn new() -> Self {
        WaitList { next: 0, list: Vec::new() }
    }
    pub fn len(&self) -> usize {
        self.list.len()
    }
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }
}

impl<K: PartialEq + Clone> WaitList<K> {
    /// A new waiter on `keys`; its id.
    pub fn add(&mut self, keys: &[K], all: bool) -> u64 {
        self.next += 1;
        self.list.push(Entry { id: self.next, keys: keys.to_vec(), all, poked: false });
        self.next
    }

    /// Waiter `id` goes away (it returned, or ended): whether it had been woken and not yet
    /// gone back to sleep, and its objects (for its owner to hand the wakeup on, see the top).
    pub fn remove(&mut self, id: u64) -> Option<(bool, Vec<K>)> {
        let i = self.list.iter().position(|e| e.id == id)?;
        let e = self.list.remove(i);
        Some((e.poked, e.keys))
    }

    /// Waiter `id` checked its objects and goes back to sleep.
    pub fn rearm(&mut self, id: u64) {
        if let Some(e) = self.list.iter_mut().find(|e| e.id == id) {
            e.poked = false;
        }
    }

    /// Whether waiter `id` has been woken since it went to sleep (true for an unknown id: it
    /// has nothing to sleep for).
    pub fn poked(&self, id: u64) -> bool {
        self.list.iter().find(|e| e.id == id).is_none_or(|e| e.poked)
    }

    /// Whether waiter `id` waits on `key`.
    pub fn waits_on(&self, id: u64, key: &K) -> bool {
        self.list.iter().any(|e| e.id == id && e.keys.contains(key))
    }

    /// Wakes every waiter: their ids go to `out`.
    pub fn wake_all(&mut self, out: &mut Vec<u64>) {
        for e in &mut self.list {
            if !e.poked {
                e.poked = true;
                out.push(e.id);
            }
        }
    }

    /// Object `key` changed: its wait-all waiters and up to `n` of its wait-any waiters not
    /// woken yet, oldest first, are woken; their ids go to `out`.
    pub fn wake(&mut self, key: &K, mut n: usize, out: &mut Vec<u64>) {
        for e in &mut self.list {
            if e.poked || !e.keys.contains(key) {
                continue;
            }
            if e.all {
                e.poked = true;
                out.push(e.id);
            } else if n > 0 {
                n -= 1;
                e.poked = true;
                out.push(e.id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn wakes_the_oldest_of_the_object_and_every_wait_all() {
        let mut w = WaitList::new();
        let a = w.add(&[1, 2], false);
        let b = w.add(&[2], false);
        let c = w.add(&[2, 3], true);
        let d = w.add(&[3], false);
        let mut out = Vec::new();
        w.wake(&2, 1, &mut out);
        assert_eq!(out, [a, c]);
        assert!(w.poked(a) && !w.poked(b) && w.poked(c) && !w.poked(d));
        out.clear();
        // (a woken waiter that has not run yet is not woken again: the next one is)
        w.wake(&2, 1, &mut out);
        assert_eq!(out, [b]);
        out.clear();
        w.rearm(a);
        w.wake_all(&mut out);
        assert_eq!(out, [a, d]);
        assert_eq!(w.remove(b), Some((true, vec![2])));
        assert_eq!(w.remove(b), None);
        assert!(w.poked(b));
    }

    proptest! {
        /// Against the plain rule: a wake of `key` by `n` wakes exactly the wait-all waiters of
        /// it and the first `n` wait-any ones among those asleep; a broadcast wakes all asleep.
        #[test]
        fn matches_the_plain_rule(ops in proptest::collection::vec((0u8..5, 0u8..4, 0usize..3, proptest::bool::ANY), 1..60)) {
            let mut w = WaitList::new();
            // the model: (id, keys, all, poked)
            let mut m: Vec<(u64, Vec<u8>, bool, bool)> = Vec::new();
            for (op, key, n, all) in ops {
                let mut out = Vec::new();
                match op {
                    0 | 1 => {
                        let keys = vec![key, (key + 1) % 4];
                        let id = w.add(&keys, all);
                        m.push((id, keys, all, false));
                    }
                    2 => {
                        w.wake(&key, n, &mut out);
                        let mut want = Vec::new();
                        let mut left = n;
                        for e in m.iter_mut().filter(|e| !e.3 && e.1.contains(&key)) {
                            if e.2 || left > 0 {
                                if !e.2 { left -= 1; }
                                e.3 = true;
                                want.push(e.0);
                            }
                        }
                        prop_assert_eq!(out, want);
                    }
                    3 => {
                        w.wake_all(&mut out);
                        let want: Vec<u64> = m.iter_mut().filter(|e| !e.3).map(|e| { e.3 = true; e.0 }).collect();
                        prop_assert_eq!(out, want);
                    }
                    _ => {
                        if let Some(e) = m.get(n % m.len().max(1)).map(|e| e.0) {
                            if all {
                                let i = m.iter().position(|x| x.0 == e).unwrap();
                                let x = m.remove(i);
                                prop_assert_eq!(w.remove(e), Some((x.3, x.1)));
                            } else {
                                w.rearm(e);
                                m.iter_mut().find(|x| x.0 == e).unwrap().3 = false;
                            }
                        }
                    }
                }
                for e in &m {
                    prop_assert_eq!(w.poked(e.0), e.3);
                }
                prop_assert_eq!(w.len(), m.len());
            }
        }
    }
}
