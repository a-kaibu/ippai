//! A queue of output records (stdout / stderr writes) that the page's threads fill and the page's
//! own thread empties when it gets to it, so that a write never waits for the page.
//!
//! (On the web, emscripten proxies every write of a pthread to the page's thread and waits for
//! it there. A game thread that logged a line now and then stopped for good, rarely, inside one
//! such write, with Rust's stderr lock held: every other thread that logged then stopped too, #221.)
//!
//! Records are `fd` (1 byte), length (2 bytes, little-endian), the bytes. A write longer than a
//! record holds is split. When the queue is full a record is dropped whole and counted; the next
//! take that has room reports how many bytes went before what follows.

#[cfg(loom)]
use loom::sync::Mutex;
#[cfg(not(loom))]
use std::sync::Mutex;

/// The longest record's bytes
pub const RECORD_MAX: usize = 0xffff;
const HEAD: usize = 3;

/// The ring itself (no lock): `N` bytes.
pub struct Ring<const N: usize> {
    buf: [u8; N],
    /// where the oldest record starts
    start: usize,
    /// bytes in use
    len: usize,
    /// bytes of output dropped since the last take that reported them
    dropped: u64,
}

impl<const N: usize> Default for Ring<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Ring<N> {
    pub const fn new() -> Self {
        Ring { buf: [0; N], start: 0, len: 0, dropped: 0 }
    }

    /// (at most two copies: up to the ring's end, then from its start)
    fn put(&mut self, b: &[u8]) {
        let at = (self.start + self.len) % N;
        let first = b.len().min(N - at);
        self.buf[at..at + first].copy_from_slice(&b[..first]);
        self.buf[..b.len() - first].copy_from_slice(&b[first..]);
        self.len += b.len();
    }

    /// `out.len()` bytes from `at` bytes past the oldest record's start (two copies at most)
    fn read(&self, at: usize, out: &mut [u8]) {
        let from = (self.start + at) % N;
        let first = out.len().min(N - from);
        out[..first].copy_from_slice(&self.buf[from..from + first]);
        let rest = out.len() - first;
        out[first..].copy_from_slice(&self.buf[..rest]);
    }

    fn peek(&self, at: usize) -> u8 {
        self.buf[(self.start + at) % N]
    }

    /// Adds `b` written to `fd`, as records; what does not fit is dropped (and counted).
    pub fn push(&mut self, fd: u8, b: &[u8]) {
        for c in b.chunks(RECORD_MAX) {
            self.push_record(fd, c);
        }
    }

    /// One record of at most RECORD_MAX bytes (dropped and counted if it does not fit).
    pub fn push_record(&mut self, fd: u8, c: &[u8]) {
        if N - self.len < HEAD + c.len() {
            self.dropped += c.len() as u64;
            return;
        }
        let n = c.len() as u16;
        self.put(&[fd]);
        self.put(&n.to_le_bytes());
        self.put(c);
    }

    /// Moves whole records into `out` while they fit; returns the bytes written. Drops first
    /// come as a record of their own on fd 2.
    pub fn take(&mut self, out: &mut [u8]) -> usize {
        let mut w = 0;
        if self.dropped > 0 {
            let note = format!("({} bytes of output dropped: the page did not keep up)\n", self.dropped);
            if out.len() >= HEAD + note.len() {
                out[0] = 2;
                out[1..3].copy_from_slice(&(note.len() as u16).to_le_bytes());
                out[3..3 + note.len()].copy_from_slice(note.as_bytes());
                w = HEAD + note.len();
                self.dropped = 0;
            }
        }
        w + self.take_records(&mut out[w..])
    }

    /// The records part of [`Ring::take`]: whole records into `out` while they fit.
    pub fn take_records(&mut self, out: &mut [u8]) -> usize {
        let mut w = 0;
        while self.len >= HEAD {
            let n = u16::from_le_bytes([self.peek(1), self.peek(2)]) as usize;
            if out.len() - w < HEAD + n {
                break;
            }
            self.read(0, &mut out[w..w + HEAD + n]);
            w += HEAD + n;
            self.start = (self.start + HEAD + n) % N;
            self.len -= HEAD + n;
        }
        w
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0 && self.dropped == 0
    }
}

/// Splits what [`Ring::take`] wrote into (fd, bytes).
pub fn records(b: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 0;
    std::iter::from_fn(move || {
        if at + HEAD > b.len() {
            return None;
        }
        let n = u16::from_le_bytes([b[at + 1], b[at + 2]]) as usize;
        let r = (b[at], b.get(at + HEAD..at + HEAD + n)?);
        at += HEAD + n;
        Some(r)
    })
}

/// The ring behind a lock: writers wait only for each other and for a take's copy, never for the
/// page's thread, which only tries the lock.
pub struct OutQ<const N: usize> {
    ring: Mutex<Ring<N>>,
}

impl<const N: usize> OutQ<N> {
    #[cfg(not(loom))]
    pub const fn new() -> Self {
        OutQ { ring: Mutex::new(Ring::new()) }
    }
    #[cfg(loom)]
    pub fn new() -> Self {
        OutQ { ring: Mutex::new(Ring::new()) }
    }

    pub fn push(&self, fd: u8, b: &[u8]) {
        crate::relock(&self.ring).push(fd, b);
    }

    /// What is queued, into `out` (0 also when a writer holds the lock: try again later).
    pub fn take(&self, out: &mut [u8]) -> usize {
        #[cfg(not(loom))]
        let g = self.ring.try_lock().ok();
        #[cfg(loom)]
        let g = self.ring.try_lock().ok();
        g.map_or(0, |mut r| r.take(out))
    }
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn drain<const N: usize>(r: &mut Ring<N>, cap: usize) -> Vec<(u8, Vec<u8>)> {
        let mut got = vec![];
        let mut out = vec![0; cap];
        loop {
            let n = r.take(&mut out);
            if n == 0 {
                return got;
            }
            got.extend(records(&out[..n]).map(|(f, b)| (f, b.to_vec())));
        }
    }

    #[test]
    fn records_come_out_whole_and_in_order() {
        let mut r = Ring::<64>::new();
        r.push(2, b"sake: GetProcAddress(");
        r.push(2, b"0x");
        r.push(1, b"out\n");
        assert_eq!(drain(&mut r, 32), vec![(2, b"sake: GetProcAddress(".to_vec()), (2, b"0x".to_vec()), (1, b"out\n".to_vec())]);
        assert!(r.is_empty());
    }

    #[test]
    fn a_full_ring_drops_and_says_so() {
        let mut r = Ring::<16>::new();
        r.push(2, b"0123456789");
        r.push(2, b"abcdef");
        let mut out = [0u8; 256];
        let n = r.take(&mut out);
        let v: Vec<_> = records(&out[..n]).collect();
        assert_eq!(v[0].0, 2);
        assert!(std::str::from_utf8(v[0].1).unwrap().starts_with("(6 bytes of output dropped"));
        assert_eq!(v[1], (2, &b"0123456789"[..]));
        assert!(r.is_empty());
    }

    #[test]
    fn long_writes_are_split() {
        let mut r = Ring::<{ 3 * (RECORD_MAX + 3) }>::new();
        let b: Vec<u8> = (0..RECORD_MAX + 10).map(|i| i as u8).collect();
        r.push(1, &b);
        let got = drain(&mut r, RECORD_MAX + 3);
        assert_eq!(got.len(), 2);
        assert_eq!([got[0].1.clone(), got[1].1.clone()].concat(), b);
    }

    proptest! {
        /// Against a model (a list of writes): whatever the ring size and the take sizes, what
        /// comes out is the writes in order, each whole, minus the dropped ones, and the
        /// dropped bytes are reported exactly.
        #[test]
        fn matches_a_list_of_writes(ops in proptest::collection::vec(prop_oneof![
            (0u8..3, proptest::collection::vec(any::<u8>(), 0..40)).prop_map(|(f, b)| Ok((f, b))),
            (4usize..120).prop_map(Err),
        ], 0..80)) {
            let mut r = Ring::<97>::new();
            let mut sent: Vec<(u8, Vec<u8>)> = vec![];
            let mut got: Vec<(u8, Vec<u8>)> = vec![];
            let mut dropped = 0u64;
            for o in ops {
                match o {
                    Ok((f, b)) => {
                        let before = r.len;
                        r.push(f, &b);
                        if r.len > before {
                            sent.push((f, b));
                        } else {
                            // (an empty write makes no record)
                            dropped += b.len() as u64;
                        }
                    }
                    Err(cap) => {
                        let mut out = vec![0; cap];
                        let n = r.take(&mut out);
                        prop_assert!(n <= cap);
                        for (f, b) in records(&out[..n]) {
                            let s = String::from_utf8_lossy(b);
                            if f == 2 && s.ends_with("the page did not keep up)\n") && s.starts_with('(') {
                                let k: u64 = s[1..].split(' ').next().unwrap().parse().unwrap();
                                prop_assert!(k <= dropped);
                                dropped -= k;
                            } else {
                                got.push((f, b.to_vec()));
                            }
                        }
                    }
                }
                prop_assert!(r.len <= 97);
            }
            got.extend(drain(&mut r, 200).into_iter().filter(|(f, b)| !(*f == 2 && b.ends_with(b"the page did not keep up)\n"))));
            prop_assert_eq!(got, sent);
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Two writes of 1 or 2 bytes into an 10-byte ring that already went around (a symbolic
    /// start), taken out with any first buffer size and then whatever is left: the two records
    /// come out whole and in order, and the ring is empty after.
    #[kani::proof]
    #[kani::unwind(12)]
    fn writes_come_out_whole_across_the_wrap() {
        let mut r = Ring::<10>::new();
        let s: usize = kani::any();
        kani::assume(s < 10);
        r.start = s;
        let a: [u8; 2] = kani::any();
        let b: [u8; 2] = kani::any();
        let (la, lb): (usize, usize) = (kani::any(), kani::any());
        kani::assume(la >= 1 && la <= 2 && lb >= 1 && lb <= 2);
        r.push_record(1, &a[..la]);
        r.push_record(2, &b[..lb]);
        assert!(r.len == la + lb + 6);
        let c1: usize = kani::any();
        kani::assume(c1 <= 10);
        let mut out = [0u8; 10];
        let n1 = r.take_records(&mut out[..c1]);
        let n2 = r.take_records(&mut out[n1..]);
        assert!(n1 + n2 == la + lb + 6);
        assert!(r.len == 0);
        assert!(out[0] == 1 && out[1] as usize == la && out[2] == 0);
        assert!(out[3] == a[0] && (la == 1 || out[4] == a[1]));
        let o = 3 + la;
        assert!(out[o] == 2 && out[o + 1] as usize == lb && out[o + 2] == 0);
        assert!(out[o + 3] == b[0] && (lb == 1 || out[o + 4] == b[1]));
    }
}
