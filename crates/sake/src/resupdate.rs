#![allow(non_snake_case)]
//! kernel32's resource updates (BeginUpdateResource / UpdateResource / EndUpdateResource), as
//! installer builders use them on their stubs (Inno Setup's ISCC puts the icons and version
//! into SetupLdr): the file's resource tree read (unless the existing resources are dropped),
//! the updates applied (data replaces the type/name/language's, NULL data with size 0 removes
//! it), and the tree written back as the resource section.
//!
//! The section: the one holding the resource directory (or named ".rsrc") is rebuilt in place,
//! sections after it moved down when its size changes (their file offsets and addresses, and the
//! data directories pointing into them); without one a ".rsrc" section is added after the last.
//! The layout is the usual one: the three directory levels, the data entries, the names, then
//! the data (8-byte aligned). Names are kept upper-cased (as FindResource looks for them), sorted
//! before the integer ids, and "#123" is the id 123.

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem;
use crate::HashMap;
use std::collections::BTreeMap;
use std::sync::Mutex;

/// A resource type or name: names sort before integer ids (as the directory keeps them)
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Id {
    Name(Vec<u16>),
    Int(u16),
}

/// The data of one language: (code page, bytes)
pub type Langs = BTreeMap<u16, (u32, Vec<u8>)>;
/// type -> name -> language
pub type Tree = BTreeMap<Id, BTreeMap<Id, Langs>>;

fn r16(b: &[u8], o: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?))
}
fn r32(b: &[u8], o: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?))
}
fn w16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn w32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn align(v: u32, a: u32) -> u32 {
    if a <= 1 { v } else { ((v as u64).div_ceil(a as u64) * a as u64).min(u32::MAX as u64) as u32 }
}

/// Where a PE's headers are
#[derive(Clone, Copy, Debug)]
pub struct Pe {
    /// the optional header
    opt: usize,
    /// the data directories
    dirs: usize,
    ndirs: u32,
    /// the section table
    secs: usize,
    nsecs: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Section {
    pub vsize: u32,
    pub va: u32,
    pub raw_size: u32,
    pub raw: u32,
    pub flags: u32,
}

impl Pe {
    pub fn parse(b: &[u8]) -> Option<Pe> {
        if r16(b, 0)? != 0x5a4d {
            return None;
        }
        let nt = r32(b, 0x3c)? as usize;
        if r32(b, nt)? != 0x4550 {
            return None;
        }
        let nsecs = r16(b, nt + 6)? as usize;
        let opt_size = r16(b, nt + 20)? as usize;
        let opt = nt + 24;
        let (dirs, ndirs) = match r16(b, opt)? {
            0x10b => (opt + 96, r32(b, opt + 92)?),
            0x20b => (opt + 112, r32(b, opt + 108)?),
            _ => return None,
        };
        let secs = opt + opt_size;
        b.get(secs..secs + 40 * nsecs)?;
        Some(Pe { opt, dirs, ndirs, secs, nsecs })
    }
    pub fn section(&self, b: &[u8], i: usize) -> Section {
        let s = self.secs + 40 * i;
        let g = |o| r32(b, s + o).unwrap_or(0);
        Section { vsize: g(8), va: g(12), raw_size: g(16), raw: g(20), flags: g(36) }
    }
    fn put_section(&self, b: &mut [u8], i: usize, x: &Section) {
        let s = self.secs + 40 * i;
        w32(b, s + 8, x.vsize);
        w32(b, s + 12, x.va);
        w32(b, s + 16, x.raw_size);
        w32(b, s + 20, x.raw);
        w32(b, s + 36, x.flags);
    }
    pub fn sections(&self, b: &[u8]) -> Vec<Section> {
        (0..self.nsecs).map(|i| self.section(b, i)).collect()
    }
    /// Data directory i: (rva, size)
    pub fn dir(&self, b: &[u8], i: u32) -> (u32, u32) {
        if i >= self.ndirs {
            return (0, 0);
        }
        (r32(b, self.dirs + 8 * i as usize).unwrap_or(0), r32(b, self.dirs + 8 * i as usize + 4).unwrap_or(0))
    }
    /// The file offset of `len` bytes at rva
    fn offset(&self, b: &[u8], rva: u32, len: u32) -> Option<usize> {
        for s in self.sections(b) {
            let size = s.vsize.max(s.raw_size) as u64;
            let (rva, len, va) = (rva as u64, len as u64, s.va as u64);
            if rva >= va && rva + len <= va + size && rva - va + len <= s.raw_size as u64 {
                let o = (s.raw as u64 + rva - va) as usize;
                b.get(o..o + len as usize)?;
                return Some(o);
            }
        }
        None
    }
}

/// The resource tree of a PE file (empty without resources); None when it is not a PE or its
/// directory points outside the file
pub fn read(b: &[u8]) -> Option<Tree> {
    let pe = Pe::parse(b)?;
    let (rva, size) = pe.dir(b, 2);
    let mut tree = Tree::new();
    if rva == 0 || size == 0 {
        return Some(tree);
    }
    let base = pe.offset(b, rva, 16)?;
    // (offsets in the directory are from its start; a directory reached twice is a loop)
    let entries = |dir: usize| -> Option<Vec<(Id, u32)>> {
        let n = r16(b, dir + 12)? as usize + r16(b, dir + 14)? as usize;
        let mut v = Vec::with_capacity(n.min(4096));
        for i in 0..n {
            let e = dir + 16 + 8 * i;
            let name = r32(b, e)?;
            let id = if name & 0x8000_0000 != 0 {
                let p = base.checked_add((name & 0x7fff_ffff) as usize)?;
                let len = r16(b, p)? as usize;
                Id::Name((0..len).map(|k| r16(b, p + 2 + 2 * k)).collect::<Option<_>>()?)
            } else {
                Id::Int(name as u16)
            };
            v.push((id, r32(b, e + 4)?));
        }
        Some(v)
    };
    for (ty, t) in entries(base)? {
        if t & 0x8000_0000 == 0 {
            continue;
        }
        let names = tree.entry(ty).or_default();
        for (name, n) in entries(base.checked_add((t & 0x7fff_ffff) as usize)?)? {
            if n & 0x8000_0000 == 0 {
                continue;
            }
            let langs = names.entry(name).or_default();
            for (lang, l) in entries(base.checked_add((n & 0x7fff_ffff) as usize)?)? {
                if l & 0x8000_0000 != 0 {
                    continue;
                }
                let Id::Int(lang) = lang else { continue };
                let d = base.checked_add(l as usize)?;
                let (drva, dsize, cp) = (r32(b, d)?, r32(b, d + 4)?, r32(b, d + 8)?);
                let o = if dsize == 0 { 0 } else { pe.offset(b, drva, dsize)? };
                langs.insert(lang, (cp, b.get(o..o + dsize as usize)?.to_vec()));
            }
        }
    }
    Some(tree)
}

/// The resource section's bytes for `tree` at `rva`
pub fn build(tree: &Tree, rva: u32) -> Vec<u8> {
    let ntypes = tree.len();
    let nnames: usize = tree.values().map(|n| n.len()).sum();
    let nlangs: usize = tree.values().flat_map(|n| n.values()).map(|l| l.len()).sum();
    let names_at = 16 + 8 * ntypes;
    let langs_at = names_at + 16 * ntypes + 8 * nnames;
    let entries_at = langs_at + 16 * nnames + 8 * nlangs;
    let strings_at = entries_at + 16 * nlangs;
    let mut strings_len = 0;
    for (ty, names) in tree {
        for id in std::iter::once(ty).chain(names.keys()) {
            if let Id::Name(s) = id {
                strings_len += 2 + 2 * s.len();
            }
        }
    }
    let mut data_at = (strings_at + strings_len).next_multiple_of(8);
    let mut total = data_at;
    for l in tree.values().flat_map(|n| n.values()).flat_map(|l| l.values()) {
        total = (total + l.1.len()).next_multiple_of(8);
    }
    let mut b = vec![0u8; total];
    let mut strings = strings_at;
    let mut id_word = |b: &mut Vec<u8>, id: &Id| -> u32 {
        match id {
            Id::Int(n) => *n as u32,
            Id::Name(s) => {
                let at = strings;
                w16(b, at, s.len() as u16);
                for (k, &ch) in s.iter().enumerate() {
                    w16(b, at + 2 + 2 * k, ch);
                }
                strings += 2 + 2 * s.len();
                0x8000_0000 | at as u32
            }
        }
    };
    let dir = |b: &mut Vec<u8>, at: usize, named: usize, ids: usize| {
        w16(b, at + 12, named as u16);
        w16(b, at + 14, ids as u16);
    };
    let named = |ids: &mut dyn Iterator<Item = &Id>| ids.filter(|i| matches!(i, Id::Name(_))).count();
    dir(&mut b, 0, named(&mut tree.keys()), ntypes - named(&mut tree.keys()));
    let (mut name_dir, mut lang_dir, mut entry) = (names_at, langs_at, entries_at);
    for (ti, (ty, names)) in tree.iter().enumerate() {
        let w = id_word(&mut b, ty);
        w32(&mut b, 16 + 8 * ti, w);
        w32(&mut b, 16 + 8 * ti + 4, 0x8000_0000 | name_dir as u32);
        let k = named(&mut names.keys());
        dir(&mut b, name_dir, k, names.len() - k);
        for (ni, (name, langs)) in names.iter().enumerate() {
            let w = id_word(&mut b, name);
            w32(&mut b, name_dir + 16 + 8 * ni, w);
            w32(&mut b, name_dir + 16 + 8 * ni + 4, 0x8000_0000 | lang_dir as u32);
            dir(&mut b, lang_dir, 0, langs.len());
            for (li, (&lang, (cp, data))) in langs.iter().enumerate() {
                w32(&mut b, lang_dir + 16 + 8 * li, lang as u32);
                w32(&mut b, lang_dir + 16 + 8 * li + 4, entry as u32);
                w32(&mut b, entry, rva + data_at as u32);
                w32(&mut b, entry + 4, data.len() as u32);
                w32(&mut b, entry + 8, *cp);
                b[data_at..data_at + data.len()].copy_from_slice(data);
                data_at = (data_at + data.len()).next_multiple_of(8);
                entry += 16;
            }
            lang_dir += 16 + 8 * langs.len();
        }
        name_dir += 16 + 8 * names.len();
    }
    b
}

/// The PE file with its resources replaced by `tree`; None when it is not a PE or there is no
/// room for a new section header
pub fn write(file: &[u8], tree: &Tree) -> Option<Vec<u8>> {
    let mut b = file.to_vec();
    let pe = Pe::parse(&b)?;
    let (fa, sa) = (r32(&b, pe.opt + 36)?, r32(&b, pe.opt + 32)?);
    // FileAlignment controls output padding. A damaged header must not turn a tiny
    // resource update into a multi-gigabyte allocation (mutation-test baseline), yet Windows
    // still updates such a file (host_resupdate-32): pad as the usual 0x200 / 0x1000 then.
    // PE permits sub-page alignment only when FileAlignment == SectionAlignment.
    let (fa, sa) = if !fa.is_power_of_two() || !sa.is_power_of_two() || fa > 0x10000 || sa < fa
        || (sa < 0x1000 && fa != sa) || (sa >= 0x1000 && fa < 0x200) {
        (0x200, 0x1000)
    } else {
        (fa, sa)
    };
    let secs = pe.sections(&b);
    let (rva, _) = pe.dir(&b, 2);
    let name = |i: usize| &b[pe.secs + 40 * i..pe.secs + 40 * i + 8];
    let found = (0..secs.len())
        .find(|&i| rva != 0 && rva >= secs[i].va && rva < secs[i].va.saturating_add(secs[i].vsize.max(secs[i].raw_size)))
        .or_else(|| (0..secs.len()).find(|&i| name(i).starts_with(b".rsrc\0")));
    let pe = match found {
        Some(_) => pe,
        None => {
            // a new section after the last (the header must have room for its entry)
            let end = pe.secs + 40 * (pe.nsecs + 1);
            let headers = r32(&b, pe.opt + 60)? as usize;
            let first_raw = secs.iter().filter(|s| s.raw != 0).map(|s| s.raw as usize).min().unwrap_or(headers);
            if end > headers.min(first_raw).min(b.len()) {
                return None;
            }
            let va = secs.iter().map(|s| s.va.saturating_add(align(s.vsize.max(s.raw_size), sa))).max().unwrap_or(align(headers as u32, sa));
            let raw = align(b.len() as u32, fa);
            let at = pe.secs + 40 * pe.nsecs;
            b[at..at + 40].fill(0);
            b[at..at + 5].copy_from_slice(b".rsrc");
            let nt = r32(&b, 0x3c)? as usize;
            w16(&mut b, nt + 6, pe.nsecs as u16 + 1);
            let pe = Pe::parse(&b)?;
            pe.put_section(&mut b, pe.nsecs - 1, &Section { vsize: 0, va, raw_size: 0, raw, flags: 0x4000_0040 });
            pe
        }
    };
    let i = found.unwrap_or(pe.nsecs - 1);
    let mut s = pe.section(&b, i);
    // (an empty section gets its data at the end of the file; one past the end is damage)
    if s.raw_size == 0 {
        s.raw = align(b.len() as u32, fa);
    } else if (s.raw as u64 + s.raw_size as u64) > b.len() as u64 {
        return None;
    }
    let data = build(tree, s.va);
    let new_raw = align(data.len() as u32, fa);
    let new_vsize = data.len() as u32;
    let delta = new_raw as i64 - s.raw_size as i64;
    let rva_delta = align(new_vsize.max(1), sa) as i64 - align(s.vsize.max(s.raw_size).max(1), sa) as i64;
    let old_end = (s.raw + s.raw_size) as usize;
    // the bytes after the section move with it (later sections and anything appended)
    let tail = if s.raw_size == 0 { Vec::new() } else { b[old_end..].to_vec() };
    let empty = s.raw_size == 0;
    b.resize(b.len().max(s.raw as usize), 0);
    b.truncate(s.raw as usize);
    b.extend_from_slice(&data);
    b.resize(s.raw as usize + new_raw as usize, 0);
    b.extend_from_slice(&tail);
    for k in 0..pe.nsecs {
        if k == i {
            continue;
        }
        let mut o = pe.section(&b, k);
        if o.raw > s.raw && !empty {
            o.raw = (o.raw as i64 + delta) as u32;
        }
        if o.va > s.va {
            o.va = (o.va as i64 + rva_delta) as u32;
        }
        pe.put_section(&mut b, k, &o);
    }
    for d in 0..pe.ndirs {
        let at = pe.dirs + 8 * d as usize;
        let v = r32(&b, at)?;
        // (the security directory is a file offset)
        if d == 4 {
            if v as usize >= old_end && v != 0 && !empty {
                w32(&mut b, at, (v as i64 + delta) as u32);
            }
        } else if v > s.va {
            w32(&mut b, at, (v as i64 + rva_delta) as u32);
        }
    }
    s.raw_size = new_raw;
    s.vsize = new_vsize;
    pe.put_section(&mut b, i, &s);
    let dir = pe.dirs + 16;
    if pe.ndirs > 2 {
        w32(&mut b, dir, if tree.is_empty() { 0 } else { s.va });
        w32(&mut b, dir + 4, if tree.is_empty() { 0 } else { data.len() as u32 });
    }
    let secs = pe.sections(&b);
    let image = secs.iter().map(|x| x.va.saturating_add(align(x.vsize.max(x.raw_size), sa))).max().unwrap_or(0);
    w32(&mut b, pe.opt + 56, image);
    let init = secs.iter().filter(|x| x.flags & 0x40 != 0).fold(0u32, |a, x| a.wrapping_add(x.raw_size));
    w32(&mut b, pe.opt + 8, init);
    Some(b)
}

/// One UpdateResource: Some(data) replaces, None removes
type Op = (Id, Id, u16, Option<Vec<u8>>);

struct Update {
    path: String,
    keep: bool,
    original: Tree,
    ops: Vec<Op>,
    failed: bool,
}

impl Update {
    fn push(&mut self, op: Op) -> bool {
        let (ty, name, lang, data) = &op;
        if data.is_none() {
            let exists = self.ops.iter().rev().find(|(t, n, l, _)| (t, n, l) == (ty, name, lang))
                .map(|(_, _, _, d)| d.is_some())
                .unwrap_or_else(|| self.original.get(ty).and_then(|n| n.get(name)).is_some_and(|l| l.contains_key(lang)));
            if !exists {
                // Windows rejects the missing deletion and cannot commit this update set
                // afterwards (recorded by host_resupdate-32).
                self.failed = true;
                return false;
            }
        }
        self.ops.push(op);
        true
    }
}

static UPDATES: Mutex<Option<HashMap<u32, Update>>> = Mutex::new(None);

fn updates<R>(f: impl FnOnce(&mut HashMap<u32, Update>) -> R) -> R {
    f(UPDATES.lock().unwrap().get_or_insert_with(HashMap::default))
}

/// The tree after the updates
pub fn apply(mut tree: Tree, ops: &[Op]) -> Tree {
    for (ty, name, lang, data) in ops {
        match data {
            Some(d) => {
                tree.entry(ty.clone()).or_default().entry(name.clone()).or_default().insert(*lang, (0, d.clone()));
            }
            None => {
                if let Some(names) = tree.get_mut(ty) {
                    if let Some(langs) = names.get_mut(name) {
                        langs.remove(lang);
                        if langs.is_empty() {
                            names.remove(name);
                        }
                    }
                    if names.is_empty() {
                        tree.remove(ty);
                    }
                }
            }
        }
    }
    tree
}

fn id_arg(a: u32, wide: bool) -> Id {
    match crate::pe::ResId::from_arg(a, wide) {
        crate::pe::ResId::Int(n) => Id::Int(n as u16),
        crate::pe::ResId::Name(s) => Id::Name(s),
    }
}

/// BeginUpdateResource(file, delete existing): a handle; NULL with the error when the file is
/// not there or (keeping its resources) is not a PE
fn begin(c: &mut Ctx, wide: bool) -> u32 {
    let (name, delete) = (c.a(0), c.a(1) != 0);
    if name == 0 {
        last_error(87);
        return 0;
    }
    let path = crate::files::full_path(&crate::k32::arg_str(name, wide));
    let Some(host) = crate::files::resolve(&path) else {
        last_error(crate::files::missing(&path));
        return 0;
    };
    let Ok(bytes) = kura::fs::read(&host) else {
        last_error(5);
        return 0;
    };
    let original = if delete { Tree::new() } else {
        let Some(tree) = read(&bytes) else {
            last_error(193);
            return 0;
        };
        tree
    };
    let h = mem::heap_alloc(4, true);
    updates(|u| u.insert(h, Update { path, keep: !delete, original, ops: Vec::new(), failed: false }));
    h
}
fn BeginUpdateResourceA(c: &mut Ctx) -> u32 {
    begin(c, false)
}
fn BeginUpdateResourceW(c: &mut Ctx) -> u32 {
    begin(c, true)
}

/// UpdateResource(handle, type, name, language, data, size): NULL data and size 0 remove
fn update(c: &mut Ctx, wide: bool) -> u32 {
    let (h, ty, name, lang, data, size) = (c.a(0), c.a(1), c.a(2), c.a(3) as u16, c.a(4), c.a(5));
    let op = if data == 0 && size == 0 {
        None
    } else if data == 0 {
        last_error(87);
        return 0;
    } else {
        Some(mem::slice(data, size).to_vec())
    };
    let (ty, name) = (id_arg(ty, wide), id_arg(name, wide));
    match updates(|u| u.get_mut(&h).map(|x| x.push((ty, name, lang, op)))) {
        Some(true) => 1,
        result => {
            last_error(if result.is_none() { 6 } else { 1814 });
            0
        }
    }
}
fn UpdateResourceA(c: &mut Ctx) -> u32 {
    update(c, false)
}
fn UpdateResourceW(c: &mut Ctx) -> u32 {
    update(c, true)
}

/// EndUpdateResource(handle, discard): the file rewritten (unless discarded); the handle freed
fn end(c: &mut Ctx) -> u32 {
    let (h, discard) = (c.a(0), c.a(1) != 0);
    let Some(x) = updates(|u| u.remove(&h)) else {
        last_error(6);
        return 0;
    };
    mem::heap_free(h);
    if discard {
        return 1;
    }
    if x.failed {
        last_error(1814);
        return 0;
    }
    let Some(host) = crate::files::resolve(&x.path) else {
        last_error(2);
        return 0;
    };
    let Ok(bytes) = kura::fs::read(&host) else {
        last_error(5);
        return 0;
    };
    let base = if x.keep { read(&bytes) } else { Pe::parse(&bytes).map(|_| Tree::new()) };
    let Some(base) = base else {
        last_error(193);
        return 0;
    };
    let Some(out) = write(&bytes, &apply(base, &x.ops)) else {
        last_error(193);
        return 0;
    };
    if kura::fs::write(&host, &out).is_err() {
        last_error(5);
        return 0;
    }
    1
}
fn EndUpdateResourceA(c: &mut Ctx) -> u32 {
    end(c)
}
fn EndUpdateResourceW(c: &mut Ctx) -> u32 {
    end(c)
}

pub fn init() {
    implement("kernel32", "BeginUpdateResourceA", BeginUpdateResourceA);
    implement("kernel32", "BeginUpdateResourceW", BeginUpdateResourceW);
    implement("kernel32", "UpdateResourceA", UpdateResourceA);
    implement("kernel32", "UpdateResourceW", UpdateResourceW);
    implement("kernel32", "EndUpdateResourceA", EndUpdateResourceA);
    implement("kernel32", "EndUpdateResourceW", EndUpdateResourceW);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A small PE32: headers (room for more sections), .text, then the given sections
    /// (name, raw size, flags) and the resource directory pointing at the one named .rsrc
    fn image(extra: &[(&[u8], u32, u32)], rsrc_tree: Option<&Tree>) -> Vec<u8> {
        let mut b = vec![0u8; 0x400];
        w16(&mut b, 0, 0x5a4d);
        w32(&mut b, 0x3c, 0x80);
        w32(&mut b, 0x80, 0x4550);
        w16(&mut b, 0x84, 0x14c);
        w16(&mut b, 0x80 + 20, 224);
        let opt = 0x98;
        w16(&mut b, opt, 0x10b);
        w32(&mut b, opt + 32, 0x1000);
        w32(&mut b, opt + 36, 0x200);
        w32(&mut b, opt + 60, 0x400);
        w32(&mut b, opt + 92, 16);
        let mut secs: Vec<(&[u8], u32, u32)> = vec![(b".text", 0x200, 0x6000_0020)];
        secs.extend_from_slice(extra);
        w16(&mut b, 0x86, secs.len() as u16);
        let mut va = 0x1000;
        for (i, (name, size, flags)) in secs.iter().enumerate() {
            let at = opt + 224 + 40 * i;
            b[at..at + name.len()].copy_from_slice(name);
            let raw = b.len() as u32;
            let mut content = vec![i as u8 + 1; *size as usize];
            if *name == b".rsrc" {
                if let Some(t) = rsrc_tree {
                    content = build(t, va);
                    w32(&mut b, opt + 96 + 16, va);
                    w32(&mut b, opt + 96 + 20, content.len() as u32);
                }
            }
            let rs = align(content.len() as u32, 0x200);
            w32(&mut b, at + 8, content.len() as u32);
            w32(&mut b, at + 12, va);
            w32(&mut b, at + 16, rs);
            w32(&mut b, at + 20, raw);
            w32(&mut b, at + 36, *flags);
            b.extend_from_slice(&content);
            b.resize((raw + rs) as usize, 0);
            if *name == b".reloc" {
                w32(&mut b, opt + 96 + 5 * 8, va);
                w32(&mut b, opt + 96 + 5 * 8 + 4, *size);
            }
            va += align(content.len().max(1) as u32, 0x1000);
        }
        w32(&mut b, opt + 56, va);
        b
    }

    fn sample() -> Tree {
        let mut t = Tree::new();
        t.entry(Id::Int(3)).or_default().entry(Id::Int(1)).or_default().insert(0x409, (0, vec![1, 2, 3]));
        t.entry(Id::Int(3)).or_default().entry(Id::Int(1)).or_default().insert(0x411, (0, vec![4; 300]));
        t.entry(Id::Name("MYTYPE".encode_utf16().collect())).or_default().entry(Id::Name("HELLO".encode_utf16().collect())).or_default().insert(0, (1252, b"hello".to_vec()));
        t.entry(Id::Int(10)).or_default().entry(Id::Int(7)).or_default().insert(0, (0, Vec::new()));
        t
    }

    /// The section table's own rules: sections in order, not overlapping in the file or in
    /// memory, raw data aligned and inside the file, SizeOfImage past the last
    fn check_layout(b: &[u8]) {
        let pe = Pe::parse(b).unwrap();
        let secs = pe.sections(b);
        let mut va_end = 0;
        let mut raw_end = 0;
        for s in &secs {
            assert!(s.va >= va_end, "{secs:x?}");
            assert_eq!(s.va % 0x1000, 0);
            va_end = s.va + align(s.vsize.max(s.raw_size).max(1), 0x1000);
            if s.raw_size != 0 {
                assert!(s.raw >= raw_end && s.raw % 0x200 == 0 && s.raw_size % 0x200 == 0, "{secs:x?}");
                raw_end = s.raw + s.raw_size;
                assert!(raw_end as usize <= b.len());
            }
        }
        assert_eq!(r32(b, pe.opt + 56).unwrap(), va_end);
    }

    #[test]
    fn round_trip_in_place() {
        let b = image(&[(b".data", 0x10, 0xc000_0040), (b".rsrc", 0, 0x4000_0040)], Some(&Tree::new()));
        assert_eq!(read(&b).unwrap(), Tree::new());
        let out = write(&b, &sample()).unwrap();
        check_layout(&out);
        assert_eq!(read(&out).unwrap(), sample());
        // and back to nothing
        let none = write(&out, &Tree::new()).unwrap();
        check_layout(&none);
        assert_eq!(read(&none).unwrap(), Tree::new());
        assert_eq!(Pe::parse(&none).unwrap().dir(&none, 2), (0, 0));
    }

    #[test]
    fn later_sections_move() {
        let mut small = Tree::new();
        small.entry(Id::Int(1)).or_default().entry(Id::Int(1)).or_default().insert(0, (0, vec![9]));
        let b = image(&[(b".rsrc", 0, 0x4000_0040), (b".reloc", 0x30, 0x4200_0040)], Some(&small));
        let before = Pe::parse(&b).unwrap().sections(&b);
        let reloc = b[before[2].raw as usize..before[2].raw as usize + 0x30].to_vec();
        let mut big = sample();
        big.entry(Id::Int(2)).or_default().entry(Id::Int(5)).or_default().insert(0, (0, vec![7; 0x2345]));
        let out = write(&b, &big).unwrap();
        check_layout(&out);
        assert_eq!(read(&out).unwrap(), big);
        let pe = Pe::parse(&out).unwrap();
        let after = pe.sections(&out);
        assert!(after[2].va > before[2].va && after[2].raw > before[2].raw);
        // the relocations are where the directory says, unchanged
        assert_eq!(pe.dir(&out, 5).0, after[2].va);
        assert_eq!(&out[after[2].raw as usize..after[2].raw as usize + 0x30], &reloc[..]);
        // and shrinking moves them back up
        let again = write(&out, &small).unwrap();
        check_layout(&again);
        assert_eq!(Pe::parse(&again).unwrap().sections(&again)[2], before[2]);
        assert_eq!(read(&again).unwrap(), small);
    }

    #[test]
    fn section_added() {
        let b = image(&[(b".data", 0x10, 0xc000_0040)], None);
        assert_eq!(read(&b).unwrap(), Tree::new());
        let out = write(&b, &sample()).unwrap();
        check_layout(&out);
        let pe = Pe::parse(&out).unwrap();
        assert_eq!(pe.nsecs, 3);
        assert_eq!(&out[pe.secs + 80..pe.secs + 86], b".rsrc\0");
        assert_eq!(read(&out).unwrap(), sample());
    }

    #[test]
    fn not_a_pe() {
        assert!(read(b"MZ").is_none());
        assert!(write(b"hello world", &sample()).is_none());
        let mut b = image(&[(b".rsrc", 0, 0x4000_0040)], Some(&sample()));
        // a directory pointing past the file
        let pe = Pe::parse(&b).unwrap();
        w32(&mut b, pe.dirs + 16, 0x7000_0000);
        assert!(read(&b).is_none());
    }

    #[test]
    fn updates_replace_and_remove() {
        let name = Id::Name("HELLO".encode_utf16().collect());
        let ty = Id::Name("MYTYPE".encode_utf16().collect());
        let ops = vec![
            (Id::Int(3), Id::Int(1), 0x409, Some(vec![5])),
            (ty.clone(), name.clone(), 0, None),
            (Id::Int(9), Id::Int(9), 0, None),
            (Id::Int(4), Id::Int(2), 7, Some(vec![1])),
            (Id::Int(4), Id::Int(2), 7, None),
        ];
        let t = apply(sample(), &ops);
        assert_eq!(t[&Id::Int(3)][&Id::Int(1)][&0x409], (0, vec![5]));
        assert_eq!(t[&Id::Int(3)][&Id::Int(1)][&0x411].1.len(), 300);
        assert!(!t.contains_key(&ty) && !t.contains_key(&Id::Int(4)) && !t.contains_key(&Id::Int(9)));
    }

    #[test]
    fn missing_delete_invalidates_transaction() {
        let mut u = Update { path: String::new(), keep: true, original: Tree::new(), ops: Vec::new(), failed: false };
        let (ty, name) = (Id::Int(10), Id::Int(1));
        assert!(u.push((ty.clone(), name.clone(), 0, Some(b"data".to_vec()))));
        assert!(u.push((ty.clone(), name.clone(), 0, None)));
        assert!(!u.failed);
        assert!(!u.push((ty, name, 0, None)));
        assert!(u.failed);
    }

    fn id() -> impl Strategy<Value = Id> {
        prop_oneof![any::<u16>().prop_map(Id::Int), proptest::collection::vec(0x41u16..0x5b, 1..6).prop_map(Id::Name)]
    }
    fn tree() -> impl Strategy<Value = Tree> {
        proptest::collection::btree_map(
            id(),
            proptest::collection::btree_map(id(), proptest::collection::btree_map(any::<u16>(), (any::<u32>(), proptest::collection::vec(any::<u8>(), 0..600)), 1..3), 1..4),
            0..5,
        )
    }

    #[test]
    fn valid_file_and_subpage_alignment_still_round_trip() {
        let tree = sample();
        for (fa, sa) in [(0x200, 0x1000), (0x10000, 0x10000), (0x80, 0x80)] {
            let mut b = image(&[(b".rsrc", 0, 0x4000_0040)], Some(&tree));
            w32(&mut b, 0x98 + 36, fa);
            w32(&mut b, 0x98 + 32, sa);
            let out = write(&b, &tree).unwrap();
            assert_eq!(read(&out).unwrap(), tree);
        }
    }

    #[test]
    fn damaged_alignment_cannot_allocate_gigabytes() {
        let original = image(&[(b".rsrc", 0, 0x4000_0040)], Some(&sample()));
        // Every single-byte corruption of FileAlignment, as the damaged-files
        // property test generates. Any accepted output must remain small.
        for byte in 0..4 {
            for value in 0..=255u8 {
                let mut b = original.clone();
                b[0x98 + 36 + byte] = value;
                if let Some(out) = write(&b, &sample()) {
                    assert!(out.len() < 0x20000);
                }
            }
        }
        // (Windows updates such a file: so does sake, with the usual alignments)
        for bad in [0, 3, 0x86000000, 0x80000000] {
            let mut b = original.clone();
            w32(&mut b, 0x98 + 36, bad);
            let out = write(&b, &sample()).expect("a damaged alignment is still updated");
            assert!(out.len() < 0x20000);
            assert_eq!(read(&out).unwrap(), sample());
        }
    }

    proptest! {
        /// Any tree written into any of the layouts reads back the same, the section table stays
        /// valid, and the other sections' bytes are untouched
        #[test]
        fn any_tree_round_trips(first in tree(), second in tree(), layout in 0..3usize) {
            let b = match layout {
                0 => image(&[(b".data", 0x10, 0xc000_0040), (b".rsrc", 0, 0x4000_0040)], Some(&first)),
                1 => image(&[(b".rsrc", 0, 0x4000_0040), (b".reloc", 0x30, 0x4200_0040)], Some(&first)),
                _ => image(&[(b".data", 0x10, 0xc000_0040)], None),
            };
            let out = write(&b, &second).unwrap();
            check_layout(&out);
            prop_assert_eq!(read(&out).unwrap(), second.clone());
            let (pa, pb) = (Pe::parse(&b).unwrap(), Pe::parse(&out).unwrap());
            for (sa, sb) in pa.sections(&b).iter().zip(pb.sections(&out)).filter(|(s, _)| s.va != pa.dir(&b, 2).0) {
                prop_assert_eq!(&b[sa.raw as usize..(sa.raw + sa.raw_size) as usize], &out[sb.raw as usize..(sb.raw + sb.raw_size) as usize]);
            }
        }

        /// Reading never panics on damaged files
        #[test]
        fn damaged_files_do_not_panic(t in tree(), at in any::<prop::sample::Index>(), v in any::<u8>()) {
            let mut b = image(&[(b".rsrc", 0, 0x4000_0040)], Some(&t));
            let i = at.index(b.len());
            b[i] = v;
            let _ = read(&b);
            let _ = write(&b, &t);
        }
    }
}
