//! Byte-range file locks (LockFile / LockFileEx / UnlockFile / UnlockFileEx) as Windows keeps
//! them for one file system: each lock belongs to the open file (handle) that took it.
//!
//! - An exclusive lock may not overlap any lock already held on the range, the taker's own
//!   included; a shared lock may overlap shared locks, and exclusive ones only of its own
//!   handle ("Exclusive locks cannot overlap an existing locked region of a file. Shared locks
//!   can overlap a locked region provided locks held on that region are shared locks", plus a
//!   handle's shared lock inside its own exclusive one, which UnlockFileEx's "two unlock
//!   operations are necessary" describes).
//! - A zero-length lock never conflicts.
//! - Unlocking names exactly a range that was locked (same offset and length, same handle);
//!   with an exclusive and a shared lock on the same range, the exclusive one goes first.
//! - Closing the handle releases its locks.
//!
//! Ranges reach past the end of the file and up to 2^64 - 1 (offset + length as u128).

/// One file system's locks: (file, owner, offset, length, exclusive).
#[derive(Default, Clone, Debug)]
pub struct Locks {
    held: Vec<Lock>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Lock {
    file: String,
    owner: u64,
    off: u64,
    len: u64,
    excl: bool,
}

/// Whether [a, a + al) and [b, b + bl) share a byte (empty ranges share none).
pub fn overlap(a: u64, al: u64, b: u64, bl: u64) -> bool {
    al != 0 && bl != 0 && (a as u128) < b as u128 + bl as u128 && (b as u128) < a as u128 + al as u128
}

/// Whether a lock request (`owner`, exclusive `excl`) over a range that overlaps a held lock
/// (`held_owner`, `held_excl`) is refused.
pub fn conflicts(owner: u64, excl: bool, held_owner: u64, held_excl: bool) -> bool {
    excl || (held_excl && held_owner != owner)
}

impl Locks {
    pub const fn new() -> Self {
        Locks { held: Vec::new() }
    }

    /// LockFileEx: false (ERROR_LOCK_VIOLATION) when it conflicts with a held lock.
    pub fn lock(&mut self, file: &str, owner: u64, off: u64, len: u64, excl: bool) -> bool {
        if self.blocked(file, owner, off, len, excl) {
            return false;
        }
        self.held.push(Lock { file: file.to_string(), owner, off, len, excl });
        true
    }

    /// Whether the lock would be refused now (a waiting LockFileEx waits while this holds).
    pub fn blocked(&self, file: &str, owner: u64, off: u64, len: u64, excl: bool) -> bool {
        len != 0
            && self
                .held
                .iter()
                .any(|l| l.file == file && overlap(off, len, l.off, l.len) && conflicts(owner, excl, l.owner, l.excl))
    }

    /// UnlockFileEx: false (ERROR_NOT_LOCKED) when `owner` holds no lock of exactly that range.
    pub fn unlock(&mut self, file: &str, owner: u64, off: u64, len: u64) -> bool {
        let same = |l: &Lock| l.file == file && l.owner == owner && l.off == off && l.len == len;
        let at = self.held.iter().position(|l| same(l) && l.excl).or_else(|| self.held.iter().position(same));
        match at {
            Some(i) => {
                self.held.remove(i);
                true
            }
            None => false,
        }
    }

    /// The handle closed: its locks go.
    pub fn release(&mut self, owner: u64) {
        self.held.retain(|l| l.owner != owner);
    }

    /// How many locks are held (tests).
    pub fn len(&self) -> usize {
        self.held.len()
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// The reference: a byte-by-byte picture of a small file (offsets and lengths below 16),
    /// each byte the list of (owner, exclusive) locks covering it, and each lock remembered as
    /// taken so unlock can match it exactly.
    #[derive(Default)]
    struct Model {
        taken: Vec<(u8, u64, u64, u64, bool)>, // (file, owner, off, len, excl)
    }

    impl Model {
        fn covering(&self, file: u8, byte: u64) -> impl Iterator<Item = &(u8, u64, u64, u64, bool)> {
            self.taken.iter().filter(move |t| t.0 == file && t.2 <= byte && byte < t.2 + t.3)
        }
        fn lock(&mut self, file: u8, owner: u64, off: u64, len: u64, excl: bool) -> bool {
            for byte in off..off + len {
                for t in self.covering(file, byte) {
                    if excl || (t.4 && t.1 != owner) {
                        return false;
                    }
                }
            }
            self.taken.push((file, owner, off, len, excl));
            true
        }
        fn unlock(&mut self, file: u8, owner: u64, off: u64, len: u64) -> bool {
            let m = |t: &&(u8, u64, u64, u64, bool)| t.0 == file && t.1 == owner && t.2 == off && t.3 == len;
            let pick = self.taken.iter().filter(m).find(|t| t.4).or_else(|| self.taken.iter().find(m)).copied();
            match pick {
                Some(p) => {
                    let i = self.taken.iter().position(|t| *t == p).unwrap();
                    self.taken.remove(i);
                    true
                }
                None => false,
            }
        }
    }

    #[derive(Debug, Clone)]
    enum Op {
        Lock(u8, u64, u64, u64, bool),
        Unlock(u8, u64, u64, u64),
        Close(u64),
    }

    fn op() -> impl Strategy<Value = Op> {
        let (f, o, x, n) = (0u8..2, 1u64..4, 0u64..12, 0u64..6);
        prop_oneof![
            4 => (f.clone(), o.clone(), x.clone(), n.clone(), any::<bool>()).prop_map(|(f, o, x, n, e)| Op::Lock(f, o, x, n, e)),
            3 => (f, o.clone(), x, n).prop_map(|(f, o, x, n)| Op::Unlock(f, o, x, n)),
            1 => o.prop_map(Op::Close),
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig { cases: 2000, ..ProptestConfig::default() })]
        /// Any sequence of locks, unlocks and closes on two files by three handles answers as the
        /// byte-by-byte reference does, and leaves the same locks.
        #[test]
        fn matches_the_byte_model(ops in prop::collection::vec(op(), 1..60)) {
            let (mut l, mut m) = (Locks::default(), Model::default());
            for op in ops {
                match op {
                    Op::Lock(f, o, x, n, e) => prop_assert_eq!(l.lock(&f.to_string(), o, x, n, e), m.lock(f, o, x, n, e), "{:?}", op),
                    Op::Unlock(f, o, x, n) => prop_assert_eq!(l.unlock(&f.to_string(), o, x, n), m.unlock(f, o, x, n), "{:?}", op),
                    Op::Close(o) => {
                        l.release(o);
                        m.taken.retain(|t| t.1 != o);
                    }
                }
                prop_assert_eq!(l.len(), m.taken.len());
            }
        }

        /// Ranges near 2^64 overlap as the integers do (no wrap-around).
        #[test]
        fn overlap_has_no_wraparound(a in any::<u64>(), al in any::<u64>(), b in any::<u64>(), bl in any::<u64>()) {
            let (ae, be) = (a as u128 + al as u128, b as u128 + bl as u128);
            let want = al > 0 && bl > 0 && (a as u128).max(b as u128) < ae.min(be);
            prop_assert_eq!(overlap(a, al, b, bl), want);
            prop_assert_eq!(overlap(a, al, b, bl), overlap(b, bl, a, al));
        }
    }

    #[test]
    fn windows_cases() {
        let mut l = Locks::default();
        // (LockFile twice on one range by one handle: the second fails, ERROR_LOCK_VIOLATION)
        assert!(l.lock("f", 1, 0, 10, true));
        assert!(!l.lock("f", 1, 5, 1, true));
        assert!(!l.lock("f", 2, 9, 100, false));
        assert!(l.lock("f", 1, 0, 10, false), "a shared lock inside the handle's own exclusive one");
        assert!(l.lock("f", 2, 10, 5, true), "the next byte");
        assert!(l.lock("g", 2, 0, 10, true), "another file");
        assert!(l.lock("f", 3, 3, 0, true), "zero bytes");
        assert!(!l.unlock("f", 1, 0, 9), "not the locked range");
        assert!(l.unlock("f", 1, 0, 10));
        assert!(!l.lock("f", 2, 0, 1, true), "the shared lock is left");
        assert!(l.lock("f", 2, 0, 1, false), "shared beside shared");
        l.release(1);
        assert!(!l.lock("f", 4, 0, 10, true), "handle 2's shared lock");
        assert!(l.unlock("f", 2, 0, 1));
        assert!(l.lock("f", 4, 0, 10, true), "handle 1 closed: its locks went");
        assert!(l.lock("f", 5, u64::MAX - 1, u64::MAX, true), "past the end, up to 2^64");
        assert!(!l.lock("f", 6, u64::MAX - 1, 1, false));
    }
}

/// Kani proofs (formal/kani.sh): the overlap and conflict rules, for every input.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// overlap is symmetric, empty ranges overlap nothing, and two ranges overlap exactly when
    /// some byte is in both (the byte is chosen by Kani: it exists when they overlap).
    #[kani::proof]
    fn overlap_is_byte_intersection() {
        let (a, al, b, bl): (u64, u64, u64, u64) = (kani::any(), kani::any(), kani::any(), kani::any());
        let o = overlap(a, al, b, bl);
        assert_eq!(o, overlap(b, bl, a, al));
        if al == 0 || bl == 0 {
            assert!(!o);
        }
        let x: u128 = kani::any();
        let inside = |s: u64, n: u64| (s as u128) <= x && x < s as u128 + n as u128;
        if inside(a, al) && inside(b, bl) {
            assert!(o);
        }
        if o {
            let m = (a as u128).max(b as u128);
            assert!(m >= a as u128 && m < a as u128 + al as u128 && m < b as u128 + bl as u128);
        }
    }

    /// Two locks by one handle may coexist unless the new one is exclusive; two handles' only
    /// when both are shared.
    #[kani::proof]
    fn conflict_rules() {
        let (o, h): (u64, u64) = (kani::any(), kani::any());
        let (e, he): (bool, bool) = (kani::any(), kani::any());
        let c = conflicts(o, e, h, he);
        if o == h {
            assert_eq!(c, e);
        } else {
            assert_eq!(c, e || he);
        }
    }
}
