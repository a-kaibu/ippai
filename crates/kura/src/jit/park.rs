//! Translations of code that its program keeps rewriting between a few versions, kept for when
//! the code comes back.
//!
//! RPG Maker's RGSS (1 to 3) draws every sprite with a blend loop it writes as machine code into
//! one buffer just before running it: a plain loop and a zooming one take turns there, many
//! times a frame (#233: ヴァンパイア・ノーツ's title, a zoomed background under unzoomed
//! sprites). Code on such W|X pages is translated with its bytes checked on entry (koji's
//! jit::set_fresh); a block finding them changed used to drop every translation of every thread
//! and, after [`super::STALE_LIMIT`] changes, leave the page to the interpreter for good: the
//! title ran at under 1 frame a second, and keys pressed for less than a frame were never seen.
//!
//! Now the thread that finds code changed takes only the translations made from it out of its
//! hash table ([`Parking::park`]) and keeps them (at most [`PARKED_MAX`]); when the bytes are
//! again those one of them was made from, it goes back in ([`Parking::reinstate`]) instead of
//! being translated (and loaded) again. Only code never seen before counts toward the limit.
//! A kept translation still checks its bytes on every entry, so taking one back too eagerly
//! costs an exit, never wrong code.

use std::collections::{HashMap, HashSet};

/// Translations kept per thread after their code changed: two versions of a few loops take
/// turns in RGSS, and each one parked keeps its module loaded.
pub const PARKED_MAX: usize = 32;

/// A translation of code on a W|X page (checked on entry): what its thread's tables point at it.
#[derive(Clone, Debug, PartialEq)]
pub struct Wx {
    /// table index (a function's adapter)
    pub base: u32,
    /// the hash table's entries it made: (block address, value)
    pub entries: Vec<(u32, u32)>,
    pub roots: Vec<u32>,
    /// a function's entry (jit::FN): in the thread's `fns`
    pub func: Option<u32>,
    /// the code it was made from (address, length) and its bytes then
    pub code: Vec<(u32, u32)>,
    pub text: Vec<u8>,
}

impl Wx {
    fn on_page(&self, page: u32) -> bool {
        self.code.iter().any(|&(a, n)| a >> 12 <= page && page <= a.saturating_add(n.max(1) - 1) >> 12)
    }
    fn has(&self, a: u32) -> bool {
        self.entries.iter().any(|e| e.0 == a)
    }
}

/// One thread's translations of W|X code: in use, and parked.
#[derive(Default, Debug)]
pub struct Parking {
    pub live: HashMap<u32, Wx>,
    /// oldest first
    pub parked: Vec<Wx>,
    /// versions of code seen: (its ranges, bytes) hashed ([`key`])
    seen: HashSet<u64>,
}

/// What [`Parking::park`] did.
#[derive(Debug, Default)]
pub struct Parked {
    /// translations to take out of the tables
    pub out: Vec<Wx>,
    /// whether the code is now in a version not seen before: what counts toward the limit (a
    /// version seen before but not translated yet, a loop the interpreter ran fewer times than
    /// the JIT's threshold, is not new: RGSS's zooming loop was counted every time it came
    /// back and the page still went to the interpreter)
    pub new: bool,
}

/// A version of some code: its ranges and bytes.
fn key(code: &[(u32, u32)], text: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    code.hash(&mut h);
    text.hash(&mut h);
    h.finish()
}

impl Parking {
    /// A new translation (a function built again replaces its earlier builds).
    pub fn track(&mut self, w: Wx) {
        self.seen.insert(key(&w.code, &w.text));
        self.live.retain(|_, o| o.roots != w.roots);
        self.live.insert(w.base, w);
    }

    /// Code on the page of `a` changed: the live translations from that page whose bytes are no
    /// longer as they were (`now`: the bytes of some code as they are, None if unreadable) leave
    /// the live set and are parked (one made from the same code as a parked one takes its
    /// place).
    pub fn park(&mut self, a: u32, now: &mut impl FnMut(&[(u32, u32)]) -> Option<Vec<u8>>) -> Parked {
        let page = a >> 12;
        let mut gone: Vec<(u32, Option<Vec<u8>>)> = self
            .live
            .values()
            .filter(|w| w.on_page(page))
            .map(|w| (w.base, now(&w.code)))
            .filter(|(b, t)| t.as_ref() != Some(&self.live[b].text))
            .collect();
        gone.sort_unstable_by_key(|g| g.0);
        let mut new = false;
        for (b, t) in &gone {
            // (unreadable code: new, as the interpreter will find out)
            new |= t.as_ref().is_none_or(|t| self.seen.insert(key(&self.live[b].code, t)));
        }
        let out: Vec<Wx> = gone.iter().map(|(b, _)| self.live.remove(b).unwrap()).collect();
        for w in &out {
            self.parked.retain(|p| p.roots != w.roots || p.code != w.code || p.text != w.text);
            self.parked.push(w.clone());
        }
        // (the oldest go: their modules are freed once nothing refers to them, reclaim)
        let over = self.parked.len().saturating_sub(PARKED_MAX);
        self.parked.drain(..over);
        Parked { out, new }
    }

    /// The newest parked translation with a block at `a` whose code is again as it was made
    /// from: back to the live set, returned for the caller to put in its tables.
    pub fn reinstate(&mut self, a: u32, now: &mut impl FnMut(&[(u32, u32)]) -> Option<Vec<u8>>) -> Option<Wx> {
        let i = self.parked.iter().rposition(|w| w.has(a) && now(&w.code).as_ref() == Some(&w.text))?;
        let w = self.parked.remove(i);
        self.live.insert(w.base, w.clone());
        Some(w)
    }

    /// Whether some parked translation has a block at `a` (a cheap test before [`reinstate`]).
    pub fn has_parked(&self, a: u32) -> bool {
        self.parked.iter().any(|w| w.has(a))
    }

    /// Table indexes still referred to (reclaim must not free them).
    pub fn bases(&self) -> impl Iterator<Item = u32> + '_ {
        self.parked.iter().flat_map(|w| w.entries.iter().map(|e| e.1))
    }
}

/// Removes `eip`'s entry from an open-addressing (linear probing) table as kura's `insert` fills
/// it, moving later entries of its probe run back so lookups that stop at an empty slot still
/// find them. Only if it is `v` (another translation may own the address by now); whether it was.
pub fn remove(hash: &mut [[u32; 2]], eip: u32, v: u32, slot: impl Fn(u32, u32) -> u32) -> bool {
    let mask = hash.len() as u32 - 1;
    let mut h = slot(eip, mask);
    loop {
        let e = hash[h as usize];
        if e[0] == 0 {
            return false;
        }
        if e[0] == eip {
            if e[1] != v {
                return false;
            }
            break;
        }
        h = (h + 1) & mask;
    }
    // backward shift: an entry after the hole may move into it unless its home slot lies
    // cyclically in (hole, its slot]
    let mut hole = h;
    let mut j = h;
    loop {
        j = (j + 1) & mask;
        let e = hash[j as usize];
        if e[0] == 0 {
            break;
        }
        let home = slot(e[0], mask);
        let stays = if hole <= j { hole < home && home <= j } else { hole < home || home <= j };
        if !stays {
            hash[hole as usize] = e;
            hole = j;
        }
    }
    hash[hole as usize] = [0, 0];
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn slot(eip: u32, mask: u32) -> u32 {
        koji::jit::dispatch::slot(eip, mask)
    }

    fn insert(hash: &mut [[u32; 2]], eip: u32, idx: u32) {
        let mask = hash.len() as u32 - 1;
        let mut h = slot(eip, mask);
        while hash[h as usize][0] != 0 && hash[h as usize][0] != eip {
            h = (h + 1) & mask;
        }
        hash[h as usize] = [eip, idx];
    }

    fn get(hash: &[[u32; 2]], eip: u32) -> Option<u32> {
        let mask = hash.len() as u32 - 1;
        let mut h = slot(eip, mask);
        while hash[h as usize][0] != 0 {
            if hash[h as usize][0] == eip {
                return Some(hash[h as usize][1]);
            }
            h = (h + 1) & mask;
        }
        None
    }

    proptest! {
        /// Removing entries keeps every other one findable (a table of a few slots: long probe
        /// runs that wrap around), and a removal names its value.
        #[test]
        fn remove_keeps_the_rest(ops in prop::collection::vec((any::<bool>(), 1u32..40, 0u32..4), 0..200)) {
            let mut hash = vec![[0u32; 2]; 64];
            let mut model: HashMap<u32, u32> = HashMap::new();
            for (add, k, v) in ops {
                // (eips spread like code addresses: the same low bits often)
                let eip = 0x35e_0000 + k * 0x10;
                if add {
                    if model.len() < 48 || model.contains_key(&eip) {
                        insert(&mut hash, eip, v);
                        model.insert(eip, v);
                    }
                } else {
                    let did = remove(&mut hash, eip, v, slot);
                    prop_assert_eq!(did, model.get(&eip) == Some(&v));
                    if did {
                        model.remove(&eip);
                    }
                }
                for k in 1..40 {
                    let e = 0x35e_0000 + k * 0x10;
                    prop_assert_eq!(get(&hash, e), model.get(&e).copied());
                }
            }
        }

        /// Code taking turns between versions (each write is one of `versions`): what goes back
        /// in is always made from the bytes there now, the parked list stays bounded, and once
        /// every version was translated, no write needs a translation again.
        #[test]
        fn versions_come_back(versions in 1usize..6, writes in prop::collection::vec(0usize..6, 1..300)) {
            let code = vec![(0x35e_0000u32, 0x92u32)];
            let mut p = Parking::default();
            let mut built = std::collections::HashSet::new();
            let mut base = 0u32;
            let track = |p: &mut Parking, v: usize, base: &mut u32| {
                *base += 2;
                p.track(Wx { base: *base, entries: vec![(0x35e_0000, *base), (0x35e_0017, *base | 1 << 20)], roots: vec![0x35e_0000], func: None, code: code.clone(), text: vec![v as u8; 0x92] });
            };
            track(&mut p, 0, &mut base);
            built.insert(0);
            for w in writes {
                let w = w % versions;
                let mem = w;
                let mut now = |c: &[(u32, u32)]| Some(vec![mem as u8; c[0].1 as usize]);
                let live_ok = p.live.values().all(|x| now(&x.code) == Some(x.text.clone()));
                if live_ok {
                    continue; // (no entry finds its code changed)
                }
                let r = p.park(0x35e_0000, &mut now);
                prop_assert!(!r.out.is_empty());
                prop_assert!(p.parked.len() <= PARKED_MAX);
                prop_assert_eq!(r.new, !built.contains(&w));
                match p.reinstate(0x35e_0000, &mut now) {
                    Some(x) => prop_assert_eq!(now(&x.code), Some(x.text.clone())),
                    None => {
                        prop_assert!(!built.contains(&w) || versions > PARKED_MAX);
                        built.insert(w);
                        track(&mut p, w, &mut base);
                    }
                }
                prop_assert!(p.live.values().all(|x| now(&x.code) == Some(x.text.clone())));
            }
        }
    }

    /// Translations from other pages, or from this page but unchanged, stay live.
    #[test]
    fn park_takes_only_changed_code_of_the_page() {
        let mut p = Parking::default();
        let w = |base, a: u32, t: u8| Wx { base, entries: vec![(a, base)], roots: vec![a], func: None, code: vec![(a, 4)], text: vec![t; 4] };
        p.track(w(1, 0x35e_0000, 1));
        p.track(w(2, 0x35e_0100, 2));
        p.track(w(3, 0x35f_0000, 9));
        let mut now = |c: &[(u32, u32)]| Some(vec![if c[0].0 == 0x35e_0000 { 7 } else if c[0].0 == 0x35e_0100 { 2 } else { 8 }; 4]);
        let r = p.park(0x35e_0000, &mut now);
        assert_eq!(r.out.iter().map(|w| w.base).collect::<Vec<_>>(), [1]);
        assert!(r.new);
        let mut live: Vec<u32> = p.live.keys().copied().collect();
        live.sort();
        assert_eq!(live, [2, 3]);
        assert!(p.has_parked(0x35e_0000));
        assert_eq!(p.bases().collect::<Vec<_>>(), [1]);
        // (unreadable code is not "as it was")
        assert!(p.reinstate(0x35e_0000, &mut |_| None).is_none());
        assert!(p.reinstate(0x35e_0000, &mut |_| Some(vec![1; 4])).is_some());
        assert!(!p.has_parked(0x35e_0000));
    }

    /// A version that came before without being translated (the interpreter ran it fewer times
    /// than the threshold) is not new when it comes again; the same translation parked twice
    /// is kept once.
    #[test]
    fn seen_versions_are_not_new() {
        let mut p = Parking::default();
        let code = vec![(0x35e_0000u32, 4u32)];
        let w = |base, t: u8| Wx { base, entries: vec![(0x35e_0000, base)], roots: vec![0x35e_0000], func: None, code: code.clone(), text: vec![t; 4] };
        p.track(w(1, 1));
        // version 2 shows up: new; nothing to put back
        let r = p.park(0x35e_0000, &mut |_| Some(vec![2; 4]));
        assert!(r.new);
        assert!(p.reinstate(0x35e_0000, &mut |_| Some(vec![2; 4])).is_none());
        // back to 1: its translation comes back
        assert!(p.reinstate(0x35e_0000, &mut |_| Some(vec![1; 4])).is_some());
        // 2 again, still untranslated: seen, not new
        let r = p.park(0x35e_0000, &mut |_| Some(vec![2; 4]));
        assert!(!r.new);
        assert_eq!(p.parked.len(), 1);
        // a rebuild of version 1 (another table index) parked over the first
        assert!(p.reinstate(0x35e_0000, &mut |_| Some(vec![1; 4])).is_some());
        p.track(w(5, 1));
        let r = p.park(0x35e_0000, &mut |_| Some(vec![3; 4]));
        assert!(r.new);
        assert_eq!(p.parked.iter().map(|w| w.base).collect::<Vec<_>>(), [5]);
    }
}
