//! Regions kept across runs: a module of region functions (see [`super::wasm::merge`]) and an
//! index saying, for function i, the roots it was built from, its block starts and the guest
//! code it was translated from (used only if that code is still there).
//!
//! A function's (jit::FN) entry is two module functions, its adapter and itself.
//!
//! Index: "VNJ2", count, then per entry: roots, starts (address, block), code (address,
//! length) as u32-counted lists of u32s, the code bytes (u32 length first), then the build
//! (0: a region, else a function's) and the function's indirect call sites (counted list).
//! "VNJ1" is the same without the last two (regions only).

use alloc::vec::Vec;

#[derive(Clone, PartialEq, Eq)]
pub struct Entry {
    pub roots: Vec<u32>,
    pub starts: Vec<(u32, u32)>,
    pub code: Vec<(u32, u32)>,
    /// the guest code bytes of `code`, concatenated
    pub text: Vec<u8>,
    /// 0 for a region, else a function's build (1 up)
    pub build: u32,
    /// a function's indirect call sites
    pub sites: Vec<u32>,
}

impl Entry {
    /// "x86_<first root>_<roots>", the name profilers show
    pub fn name(&self) -> alloc::string::String {
        if self.build > 0 {
            return alloc::format!("x86_{:08x}_fn", self.roots[0]);
        }
        alloc::format!("x86_{:08x}_{}", self.roots[0], self.roots.len())
    }

    /// Module functions (table entries) it takes.
    pub fn width(&self) -> u32 {
        if self.build > 0 { 2 } else { 1 }
    }
}

pub fn encode(entries: &[Entry]) -> Vec<u8> {
    let mut out = Vec::from(&b"VNJ2"[..]);
    let u = |out: &mut Vec<u8>, v: u32| out.extend_from_slice(&v.to_le_bytes());
    u(&mut out, entries.len() as u32);
    for e in entries {
        u(&mut out, e.roots.len() as u32);
        for &r in &e.roots {
            u(&mut out, r);
        }
        for list in [&e.starts, &e.code] {
            u(&mut out, list.len() as u32);
            for &(a, b) in list {
                u(&mut out, a);
                u(&mut out, b);
            }
        }
        u(&mut out, e.text.len() as u32);
        out.extend_from_slice(&e.text);
        u(&mut out, e.build);
        u(&mut out, e.sites.len() as u32);
        for &a in &e.sites {
            u(&mut out, a);
        }
    }
    out
}

struct Reader<'a>(&'a [u8], usize);

impl Reader<'_> {
    fn u(&mut self) -> Option<u32> {
        let v = u32::from_le_bytes(self.0.get(self.1..self.1 + 4)?.try_into().ok()?);
        self.1 += 4;
        Some(v)
    }
    fn pairs(&mut self) -> Option<Vec<(u32, u32)>> {
        (0..self.u()?).map(|_| Some((self.u()?, self.u()?))).collect()
    }
}

pub fn decode(b: &[u8]) -> Option<Vec<Entry>> {
    let (e, n) = decode_one(b)?;
    (n == b.len()).then_some(e)
}

/// Indexes one after another (the parts of a bundle), as one list.
pub fn decode_all(mut b: &[u8]) -> Option<Vec<Entry>> {
    let mut all = Vec::new();
    while !b.is_empty() {
        let (e, n) = decode_one(b)?;
        all.extend(e);
        b = &b[n..];
    }
    Some(all)
}

/// An index at the start of `b`, and its length.
fn decode_one(b: &[u8]) -> Option<(Vec<Entry>, usize)> {
    let v2 = match b.get(..4)? {
        b"VNJ1" => false,
        b"VNJ2" => true,
        _ => return None,
    };
    let mut r = Reader(b, 4);
    let e = (0..r.u()?)
        .map(|_| {
            let roots = (0..r.u()?).map(|_| r.u()).collect::<Option<Vec<_>>>()?;
            // (what reads an index takes roots[0]: an entry without one, from a damaged file or
            // a server that merged a bad upload, aborted every thread at its start)
            if roots.is_empty() {
                return None;
            }
            let (starts, code) = (r.pairs()?, r.pairs()?);
            let n = r.u()? as usize;
            let text = b.get(r.1..r.1 + n)?.to_vec();
            r.1 += n;
            let (build, sites) = if v2 { (r.u()?, (0..r.u()?).map(|_| r.u()).collect::<Option<Vec<_>>>()?) } else { (0, Vec::new()) };
            Some(Entry { roots, starts, code, text, build, sites })
        })
        .collect::<Option<Vec<_>>>()?;
    Some((e, r.1))
}

/// `keep` (one per entry) as one per module function, for [`super::wasm::select`].
pub fn keep_funcs(entries: &[Entry], keep: &[bool]) -> Vec<bool> {
    entries.iter().zip(keep).flat_map(|(e, &k)| core::iter::repeat_n(k, e.width() as usize)).collect()
}

/// The module functions' names, for [`super::wasm::merge`].
pub fn func_names(entries: &[Entry]) -> Vec<alloc::string::String> {
    entries
        .iter()
        .flat_map(|e| {
            let n = e.name();
            if e.width() == 2 { alloc::vec![alloc::format!("{n}_adapter"), n] } else { alloc::vec![n] }
        })
        .collect()
}

/// Each entry's first table index past the module's first.
pub fn offsets(entries: &[Entry]) -> Vec<u32> {
    let mut at = 0;
    entries
        .iter()
        .map(|e| {
            let o = at;
            at += e.width();
            o
        })
        .collect()
}

#[cfg(test)]
mod decode_tests {
    use super::*;
    use proptest::prelude::*;

    type Parts = (Vec<u32>, Vec<(u32, u32)>, Vec<u8>, u32, Vec<u32>);
    fn entry() -> impl Strategy<Value = Parts> {
        (prop::collection::vec(any::<u32>(), 1..4), prop::collection::vec(any::<(u32, u32)>(), 0..3), prop::collection::vec(any::<u8>(), 0..12), 0u32..3, prop::collection::vec(any::<u32>(), 0..3))
    }

    #[test]
    fn an_entry_without_roots_is_no_index() {
        // one entry, 0 roots, no starts, no code, no text, build 0, no sites
        let mut b = Vec::from(&b"VNJ2"[..]);
        for v in [1u32, 0, 0, 0, 0, 0, 0] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        assert!(decode(&b).is_none() && decode_all(&b).is_none());
    }

    proptest! {
        /// What is written is read back; and whatever bytes are changed, cut or added, what is
        /// read (if anything) has a root in every entry and can be named.
        #[test]
        fn damaged_indexes_have_roots_or_are_refused(es in prop::collection::vec(entry(), 0..5), flips in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..6), cut in proptest::option::of(any::<prop::sample::Index>())) {
            let es: Vec<Entry> = es.into_iter().map(|(roots, starts, text, build, sites)| Entry { roots, code: starts.clone(), starts, text, build, sites }).collect();
            let mut b = encode(&es);
            let back = decode(&b).unwrap();
            prop_assert_eq!(back.len(), es.len());
            for (a, e) in back.iter().zip(&es) {
                prop_assert!(a.roots == e.roots && a.starts == e.starts && a.text == e.text && a.build == e.build && a.sites == e.sites);
            }
            for (at, v) in flips {
                let i = at.index(b.len());
                b[i] = v;
            }
            if let Some(c) = cut {
                b.truncate(c.index(b.len() + 1));
            }
            for e in decode_all(&b).into_iter().flatten() {
                prop_assert!(!e.roots.is_empty());
                let _ = (e.name(), e.width());
            }
        }
    }
}
