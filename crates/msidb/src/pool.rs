//! The string pool: `_StringPool` (u32 codepage | 0x80000000 for 3-byte references, then
//! (u16 length, u16 refcount) per string id from 1; length 0 with a refcount: the length is
//! the u32 after) and `_StringData` (the strings back to back in the codepage).
//!
//! References are counted apart for persistent and temporary rows: a string only temporary
//! rows use is not saved, and an id whose counts both drop to 0 is reused.

use crate::HashMap;

const LONG_REFS: u32 = 0x8000_0000;

/// The ANSI code page codepage 0 (neutral) stands for (sake is a Japanese Windows).
pub static DEFAULT_CODEPAGE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(932);

pub fn encoding(cp: u32) -> &'static encoding_rs::Encoding {
    use encoding_rs::*;
    let cp = if cp == 0 { DEFAULT_CODEPAGE.load(std::sync::atomic::Ordering::Relaxed) } else { cp };
    match cp {
        932 => SHIFT_JIS,
        936 => GBK,
        949 => EUC_KR,
        950 => BIG5,
        1250 => WINDOWS_1250,
        1251 => WINDOWS_1251,
        1253 => WINDOWS_1253,
        1254 => WINDOWS_1254,
        1255 => WINDOWS_1255,
        1256 => WINDOWS_1256,
        1257 => WINDOWS_1257,
        1258 => WINDOWS_1258,
        874 => WINDOWS_874,
        65001 => UTF_8,
        _ => WINDOWS_1252,
    }
}

pub fn decode_cp(cp: u32, b: &[u8]) -> String {
    encoding(cp).decode_without_bom_handling(b).0.into_owned()
}

pub fn encode_cp(cp: u32, s: &str) -> Vec<u8> {
    if cp == 65001 {
        return s.as_bytes().to_vec();
    }
    encoding(cp).encode(s).0.into_owned()
}

#[derive(Clone, Debug, Default)]
pub struct Entry {
    pub s: String,
    pub persistent: u32,
    pub temporary: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Pool {
    pub codepage: u32,
    /// id - 1 → entry (unused ids: empty string, both counts 0)
    pub v: Vec<Entry>,
    map: HashMap<String, u32>,
    /// the ids no string holds (both counts 0, not in `map`): a new string takes the lowest
    free: std::collections::BTreeSet<u32>,
    /// the file had 3-byte string references
    pub long_refs: bool,
}

impl Pool {
    pub fn load(pool: &[u8], data: &[u8]) -> crate::R<Pool> {
        let mut p = Pool::default();
        if pool.len() < 4 {
            return Ok(p);
        }
        let h = u32::from_le_bytes(pool[0..4].try_into().unwrap());
        p.codepage = h & !LONG_REFS;
        p.long_refs = h & LONG_REFS != 0;
        let mut i = 4;
        let mut at = 0usize;
        while i + 4 <= pool.len() {
            let mut len = u16::from_le_bytes([pool[i], pool[i + 1]]) as usize;
            let refs = u16::from_le_bytes([pool[i + 2], pool[i + 3]]) as u32;
            i += 4;
            if len == 0 && refs != 0 {
                if i + 4 > pool.len() {
                    return Err(crate::ERROR_INVALID_DATA);
                }
                len = u32::from_le_bytes(pool[i..i + 4].try_into().unwrap()) as usize;
                i += 4;
            }
            let end = at.checked_add(len).filter(|&e| e <= data.len()).ok_or(crate::ERROR_INVALID_DATA)?;
            let s = decode_cp(p.codepage, &data[at..end]);
            at = end;
            let id = p.v.len() as u32 + 1;
            // (the counts are rebuilt from the rows that load; a string no row uses is dropped)
            if refs != 0 && !p.map.contains_key(&s) {
                p.map.insert(s.clone(), id);
            } else {
                p.free.insert(id);
            }
            p.v.push(Entry { s, persistent: 0, temporary: 0 });
        }
        Ok(p)
    }

    /// The id of `s` if the pool has it (with a reference).
    pub fn find(&self, s: &str) -> Option<u32> {
        self.map.get(s).copied()
    }

    pub fn get(&self, id: u32) -> Option<&str> {
        if id == 0 {
            return None;
        }
        self.v.get(id as usize - 1).map(|e| e.s.as_str())
    }

    /// Adds a reference to `s` (empty: id 0, NULL), allocating an id for a new string.
    pub fn add(&mut self, s: &str, persistent: bool) -> u32 {
        if s.is_empty() {
            return 0;
        }
        let id = match self.map.get(s) {
            Some(&id) => id,
            None => {
                let id = match self.take_free() {
                    Some(id) => id,
                    None => {
                        self.v.push(Entry::default());
                        self.v.len() as u32
                    }
                };
                self.v[id as usize - 1] = Entry { s: s.to_string(), persistent: 0, temporary: 0 };
                self.map.insert(s.to_string(), id);
                id
            }
        };
        let e = &mut self.v[id as usize - 1];
        if persistent {
            e.persistent += 1;
        } else {
            e.temporary += 1;
        }
        id
    }

    /// The lowest id no string holds (the scan over every entry it replaces made N new
    /// strings O(N x pool)).
    fn take_free(&mut self) -> Option<u32> {
        while let Some(id) = self.free.pop_first() {
            let e = &self.v[id as usize - 1];
            if e.persistent == 0 && e.temporary == 0 && !self.map.contains_key(&e.s) {
                return Some(id);
            }
        }
        None
    }

    /// Adds a reference to an id already in the pool.
    pub fn addref(&mut self, id: u32, persistent: bool) {
        self.free.remove(&id);
        if let Some(e) = id.checked_sub(1).and_then(|i| self.v.get_mut(i as usize)) {
            if persistent {
                e.persistent += 1;
            } else {
                e.temporary += 1;
            }
            if !self.map.contains_key(&e.s) && !e.s.is_empty() {
                self.map.insert(e.s.clone(), id);
            }
        }
    }

    pub fn release(&mut self, id: u32, persistent: bool) {
        let Some(e) = id.checked_sub(1).and_then(|i| self.v.get_mut(i as usize)) else { return };
        if persistent {
            e.persistent = e.persistent.saturating_sub(1);
        } else {
            e.temporary = e.temporary.saturating_sub(1);
        }
        if e.persistent == 0 && e.temporary == 0 {
            let s = std::mem::take(&mut e.s);
            if self.map.get(&s) == Some(&id) {
                self.map.remove(&s);
            }
            self.free.insert(id);
        }
    }

    /// Drops strings no row references (after a load counted the rows' references).
    pub fn prune(&mut self) {
        for (i, e) in self.v.iter_mut().enumerate() {
            if e.persistent == 0 && e.temporary == 0 && !e.s.is_empty() {
                if self.map.get(&e.s) == Some(&(i as u32 + 1)) {
                    self.map.remove(&e.s);
                }
                e.s.clear();
            }
            if e.persistent == 0 && e.temporary == 0 {
                self.free.insert(i as u32 + 1);
            }
        }
    }

    /// Whether references need 3 bytes in the saved file.
    pub fn wide_refs(&self) -> bool {
        self.long_refs || self.v.len() > 0xffff
    }

    /// (_StringPool, _StringData) of the persistent strings.
    pub fn save(&self) -> (Vec<u8>, Vec<u8>) {
        let mut pool = (self.codepage | if self.wide_refs() { LONG_REFS } else { 0 }).to_le_bytes().to_vec();
        let mut data = Vec::new();
        // (trailing unused ids are not written)
        let n = self.v.iter().rposition(|e| e.persistent > 0).map_or(0, |i| i + 1);
        for e in &self.v[..n] {
            if e.persistent == 0 {
                pool.extend([0, 0, 0, 0]);
                continue;
            }
            let b = encode_cp(self.codepage, &e.s);
            let refs = e.persistent.min(0xffff) as u16;
            if b.len() > 0xffff {
                pool.extend(0u16.to_le_bytes());
                pool.extend(refs.max(1).to_le_bytes());
                pool.extend((b.len() as u32).to_le_bytes());
            } else {
                pool.extend((b.len() as u16).to_le_bytes());
                pool.extend(refs.to_le_bytes());
            }
            data.extend(b);
        }
        (pool, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load() {
        let mut p = Pool { codepage: 932, ..Default::default() };
        let a = p.add("あいう", true);
        let b = p.add("abc", true);
        let t = p.add("tmp", false);
        assert_eq!((a, b, t), (1, 2, 3));
        assert_eq!(p.add("abc", true), 2);
        let (sp, sd) = p.save();
        let q = Pool::load(&sp, &sd).unwrap();
        assert_eq!(q.codepage, 932);
        assert_eq!(q.get(1), Some("あいう"));
        assert_eq!(q.get(2), Some("abc"));
        assert_eq!(q.v.len(), 2);
        assert_eq!(&sp[4..12], &[6, 0, 1, 0, 3, 0, 2, 0]);
    }

    #[test]
    fn reuse_ids() {
        let mut p = Pool::default();
        let a = p.add("a", true);
        p.add("b", true);
        p.release(a, true);
        assert_eq!(p.find("a"), None);
        assert_eq!(p.add("c", true), a);
    }

    #[test]
    fn long_string() {
        let mut p = Pool { codepage: 1252, ..Default::default() };
        let s = "x".repeat(70000);
        p.add(&s, true);
        let (sp, sd) = p.save();
        assert_eq!(&sp[4..8], &[0, 0, 1, 0]);
        let q = Pool::load(&sp, &sd).unwrap();
        assert_eq!(q.get(1).unwrap().len(), 70000);
    }

    /// The id search the free set replaced: the lowest id no string holds.
    fn old_add(p: &mut Pool, s: &str, persistent: bool) -> u32 {
        if s.is_empty() {
            return 0;
        }
        let id = match p.map.get(s) {
            Some(&id) => id,
            None => {
                let id = match p.v.iter().position(|e| e.persistent == 0 && e.temporary == 0 && !p.map.contains_key(&e.s)) {
                    Some(i) => i as u32 + 1,
                    None => {
                        p.v.push(Entry::default());
                        p.v.len() as u32
                    }
                };
                p.v[id as usize - 1] = Entry { s: s.to_string(), persistent: 0, temporary: 0 };
                p.map.insert(s.to_string(), id);
                id
            }
        };
        let e = &mut p.v[id as usize - 1];
        if persistent { e.persistent += 1 } else { e.temporary += 1 }
        id
    }

    proptest::proptest! {
        /// Any sequence of adds, releases and addrefs (and a save/load/prune in between):
        /// the ids handed out are the ones the full scan gave.
        #[test]
        fn free_ids_as_the_scan(ops in proptest::collection::vec((0u8..4, 0u8..12, proptest::prelude::any::<bool>()), 0..200), reload_at in 0usize..200) {
            let mut a = Pool { codepage: 65001, ..Default::default() };
            let mut b = a.clone();
            for (i, &(op, x, pers)) in ops.iter().enumerate() {
                if i == reload_at {
                    // (as Database::open: load, the rows' addrefs, prune)
                    let (sp, sd) = a.save();
                    a = Pool::load(&sp, &sd).unwrap();
                    b = a.clone();
                    for id in 1..=a.v.len() as u32 {
                        if id % 3 == 0 {
                            a.addref(id, true);
                            b.addref(id, true);
                        }
                    }
                    a.prune();
                    b.prune();
                }
                match op {
                    0 | 1 => {
                        let s = format!("s{x}");
                        proptest::prop_assert_eq!(a.add(&s, pers), old_add(&mut b, &s, pers));
                    }
                    2 => {
                        a.release(x as u32 + 1, pers);
                        b.release(x as u32 + 1, pers);
                    }
                    _ => {
                        if (x as usize) < a.v.len() && !a.v[x as usize].s.is_empty() {
                            a.addref(x as u32 + 1, pers);
                            b.addref(x as u32 + 1, pers);
                        }
                    }
                }
                proptest::prop_assert_eq!(a.v.iter().map(|e| (&e.s, e.persistent, e.temporary)).collect::<Vec<_>>(), b.v.iter().map(|e| (&e.s, e.persistent, e.temporary)).collect::<Vec<_>>());
            }
        }
    }

    /// 20000 new strings into a pool of 20000 held ones: no scan of the pool per string.
    #[test]
    fn many_new_strings() {
        let mut p = Pool::default();
        for i in 0..20000 {
            p.add(&format!("held{i}"), true);
        }
        let t = std::time::Instant::now();
        for i in 0..20000 {
            p.add(&format!("new{i}"), true);
        }
        assert!(t.elapsed().as_secs() < 5, "{:?}", t.elapsed());
        assert_eq!(p.v.len(), 40000);
    }
}
