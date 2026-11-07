#![allow(non_snake_case)]
//! setupapi's disk space lists (installers ask how much a file queue needs on each drive):
//! SetupCreate/Duplicate/DestroyDiskSpaceList, SetupAddToDiskSpaceList,
//! SetupQuerySpaceRequiredOnDrive, SetupQueryDrivesInDiskSpaceList.
//!
//! Behaviour follows Wine's setupapi diskspace tests (Windows' answers): sizes count in whole
//! 4096-byte clusters; a copy needs its size less the existing file's (unless SPDSL_IGNORE_DISK),
//! a delete frees the existing file (its given size is ignored), both measured when added; a
//! later operation on the same file replaces the earlier one, except a delete after a copy, which
//! is ignored. A drive is queried as exactly "x:" and only once the list has a file on it.

use crate::api::{implement, Ctx};
use crate::k32::last_error;
use crate::mem;
use crate::HashMap;
use std::collections::BTreeMap;
use std::sync::Mutex;

const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_INVALID_DRIVE: u32 = 15;
const ERROR_INVALID_PARAMETER: u32 = 87;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
const ERROR_INVALID_NAME: u32 = 123;

pub const SPDSL_IGNORE_DISK: u32 = 1;
pub const SPDSL_DISALLOW_NEGATIVE_ADJUST: u32 = 2;
pub const FILEOP_COPY: u32 = 0;
pub const FILEOP_DELETE: u32 = 2;
const CLUSTER: i64 = 4096;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct List {
    pub flags: u32,
    /// full path (lower case) -> (op, space it needs)
    pub files: BTreeMap<String, (u32, i64)>,
}

fn clusters(n: i64) -> i64 {
    (n.max(0) + CLUSTER - 1) / CLUSTER * CLUSTER
}

/// The key of a path (its full path, lower case) or the error for a name it refuses
pub fn file_key(path: &str, full: impl Fn(&str) -> String) -> Result<String, u32> {
    if path.is_empty() {
        return Err(ERROR_INVALID_NAME);
    }
    if path.starts_with("\\\\")
        || path.starts_with("//")
        || path.ends_with('/')
        || path.ends_with('\\')
    {
        return Err(ERROR_INVALID_PARAMETER);
    }
    let f = full(path).to_ascii_lowercase();
    if f.len() < 3 || f.as_bytes()[1] != b':' {
        return Err(ERROR_INVALID_PARAMETER);
    }
    Ok(f)
}

impl List {
    /// Adds an operation; `existing` is the file's size now (None: no such file)
    pub fn add(
        &mut self,
        key: String,
        size: i64,
        op: u32,
        existing: Option<i64>,
    ) -> Result<(), u32> {
        if op != FILEOP_COPY && op != FILEOP_DELETE {
            return Err(ERROR_INVALID_PARAMETER);
        }
        let have = if self.flags & SPDSL_IGNORE_DISK != 0 {
            0
        } else {
            clusters(existing.unwrap_or(0))
        };
        if op == FILEOP_DELETE && self.files.get(&key).is_some_and(|e| e.0 == FILEOP_COPY) {
            return Ok(());
        }
        let space = if op == FILEOP_COPY {
            clusters(size) - have
        } else {
            -have
        };
        self.files.insert(key, (op, space));
        Ok(())
    }

    /// The drives with files, sorted ("c:", "d:", ...)
    pub fn drives(&self) -> Vec<String> {
        let mut d: Vec<String> = self.files.keys().map(|k| k[..2].to_string()).collect();
        d.dedup();
        d
    }

    /// The space needed on `drive` ("x:"), Err(ERROR_INVALID_DRIVE)
    pub fn required(&self, drive: &str) -> Result<i64, u32> {
        let d = drive.to_ascii_lowercase();
        if d.len() != 2
            || !d.as_bytes()[0].is_ascii_alphabetic()
            || d.as_bytes()[1] != b':'
            || !self.drives().contains(&d)
        {
            return Err(ERROR_INVALID_DRIVE);
        }
        Ok(self
            .files
            .iter()
            .filter(|(k, _)| k.starts_with(&d))
            .map(|(_, v)| v.1)
            .sum())
    }
}

/// SetupQueryDrivesInDiskSpaceList's multi-string: the whole drive names that fit in `cap`
/// characters (and the final NUL when everything does), the length it reports (when they do
/// not fit: up to the first one that does not, and a NUL) and whether all fit.
pub fn drives_multi_sz(drives: &[String], cap: usize) -> (Vec<u8>, usize, bool) {
    let mut out = Vec::new();
    let mut need = drives.len() * 3 + 1;
    for d in drives {
        if out.len() + 3 > cap {
            need = out.len() + 3 + 1;
            break;
        }
        out.extend_from_slice(d.as_bytes());
        out.push(0);
    }
    let fits = cap >= drives.len() * 3 + 1;
    if fits {
        out.push(0);
    }
    (out, need, fits)
}

static LISTS: Mutex<Option<HashMap<u32, List>>> = Mutex::new(None);
static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x6d50_0000);

fn with<R>(f: impl FnOnce(&mut HashMap<u32, List>) -> R) -> R {
    f(LISTS.lock().unwrap().get_or_insert_with(HashMap::default))
}

fn fail(e: u32) -> u32 {
    last_error(e);
    0
}

fn new_list(l: List) -> u32 {
    let h = NEXT.fetch_add(16, std::sync::atomic::Ordering::Relaxed);
    with(|m| m.insert(h, l));
    h
}

/// SetupCreateDiskSpaceList(reserved1, reserved2, flags)
fn create(c: &mut Ctx) -> u32 {
    let (r1, r2, flags) = (c.a(0), c.a(1), c.a(2));
    if r1 != 0 || r2 != 0 || flags & !(SPDSL_IGNORE_DISK | SPDSL_DISALLOW_NEGATIVE_ADJUST) != 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    new_list(List {
        flags,
        ..Default::default()
    })
}

/// SetupDuplicateDiskSpaceList(list, reserved1, reserved2, flags)
fn duplicate(c: &mut Ctx) -> u32 {
    let (h, r1, r2, flags) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if r1 != 0 || r2 != 0 || flags != 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    match with(|m| m.get(&h).cloned()) {
        Some(l) => new_list(l),
        None => fail(ERROR_INVALID_HANDLE),
    }
}

/// SetupAddToDiskSpaceList(list, file, LONGLONG size, op, reserved1, reserved2)
fn add(c: &mut Ctx, wide: bool) -> u32 {
    let x64 = mem::x64();
    let (h, file) = (c.a(0), c.a(1));
    let (size, op) = if x64 {
        (c.a64(2) as i64, c.a(3))
    } else {
        ((c.a(2) as u64 | (c.a(3) as u64) << 32) as i64, c.a(4))
    };
    if !with(|m| m.contains_key(&h)) {
        return fail(ERROR_INVALID_HANDLE);
    }
    if file == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let path = crate::k32::arg_str(file, wide);
    let key = match file_key(&path, crate::files::full_path) {
        Ok(k) => k,
        Err(e) => return fail(e),
    };
    let existing = crate::files::resolve(&path)
        .and_then(|p| kura::fs::metadata(p).ok())
        .filter(|m| !m.is_dir())
        .map(|m| m.len() as i64);
    match with(|m| m.get_mut(&h).map(|l| l.add(key, size, op, existing))) {
        Some(Ok(())) => {
            last_error(0);
            1
        }
        Some(Err(e)) => fail(e),
        None => fail(ERROR_INVALID_HANDLE),
    }
}

/// SetupQuerySpaceRequiredOnDrive(list, drive, &space, reserved1, reserved2): the A function
/// checks the drive before the list, the W one the list first
fn query(c: &mut Ctx, wide: bool) -> u32 {
    let (h, drive, out) = (c.a(0), c.a(1), c.a(2));
    let known = with(|m| m.contains_key(&h));
    if !wide && drive == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    if !known {
        return fail(ERROR_INVALID_HANDLE);
    }
    if drive == 0 {
        return fail(ERROR_INVALID_PARAMETER);
    }
    let d = crate::k32::arg_str(drive, wide);
    match with(|m| m[&h].required(&d)) {
        Ok(s) => {
            if out != 0 {
                mem::w64(out, s as u64);
            }
            last_error(0);
            1
        }
        Err(e) => fail(e),
    }
}

/// SetupQueryDrivesInDiskSpaceList(list, buffer, size, &needed)
fn drives(c: &mut Ctx, wide: bool) -> u32 {
    let (h, buf, size, pneed) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(d) = with(|m| m.get(&h).map(|l| l.drives())) else {
        return fail(ERROR_INVALID_HANDLE);
    };
    let (s, need, fits) = drives_multi_sz(&d, if buf == 0 { usize::MAX } else { size as usize });
    mem::put32(pneed, need as u32);
    if buf != 0 {
        for (i, &b) in s.iter().enumerate() {
            if wide {
                mem::w16(buf + 2 * i as u32, b as u16);
            } else {
                mem::w8(buf + i as u32, b);
            }
        }
    }
    if buf != 0 && !fits {
        return fail(ERROR_INSUFFICIENT_BUFFER);
    }
    last_error(0);
    1
}

pub fn init() {
    let d = "setupapi";
    implement(d, "SetupCreateDiskSpaceListA", create);
    implement(d, "SetupCreateDiskSpaceListW", create);
    implement(d, "SetupDuplicateDiskSpaceListA", duplicate);
    implement(d, "SetupDuplicateDiskSpaceListW", duplicate);
    implement(d, "SetupDestroyDiskSpaceList", |c| {
        if with(|m| m.remove(&c.a(0))).is_some() {
            1
        } else {
            fail(ERROR_INVALID_HANDLE)
        }
    });
    implement(d, "SetupAddToDiskSpaceListA", |c| add(c, false));
    implement(d, "SetupAddToDiskSpaceListW", |c| add(c, true));
    implement(d, "SetupQuerySpaceRequiredOnDriveA", |c| query(c, false));
    implement(d, "SetupQuerySpaceRequiredOnDriveW", |c| query(c, true));
    implement(d, "SetupQueryDrivesInDiskSpaceListA", |c| drives(c, false));
    implement(d, "SetupQueryDrivesInDiskSpaceListW", |c| drives(c, true));
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn full(p: &str) -> String {
        let p = p.replace('/', "\\");
        if p.as_bytes().get(1) == Some(&b':') {
            p
        } else {
            format!("C:\\d\\{p}")
        }
    }

    #[test]
    fn windows_sequence() {
        // Wine's test: copies replace each other per file, a delete after a copy is ignored
        let mut l = List::default();
        let k = |p: &str| file_key(p, full).unwrap();
        assert_eq!(l.required("c:"), Err(ERROR_INVALID_DRIVE));
        l.add(k("C:/file1"), 10000, FILEOP_COPY, None).unwrap();
        l.add(k("C:/file2"), 13, FILEOP_COPY, None).unwrap();
        l.add(k("C:\\file1"), 5000, FILEOP_COPY, None).unwrap();
        l.add(k("C:/file1"), 4000, FILEOP_DELETE, None).unwrap();
        l.add(k("D:/file1"), 4000, FILEOP_COPY, None).unwrap();
        for bad in ["c", "c:\\", ""] {
            assert_eq!(l.required(bad), Err(ERROR_INVALID_DRIVE));
        }
        assert_eq!(l.required("C:"), Ok(4096 * 3));
        // an existing file of 1025 bytes
        let mut l = List::default();
        l.add(k("C:/t"), 10000, FILEOP_DELETE, None).unwrap();
        assert_eq!(l.required("c:"), Ok(0));
        l.add(k("C:/t"), 10000, FILEOP_DELETE, Some(1025)).unwrap();
        assert_eq!(l.required("c:"), Ok(-4096));
        l.add(k("C:/t"), 4097, FILEOP_COPY, Some(1025)).unwrap();
        assert_eq!(l.required("c:"), Ok(4096));
        l.add(k("C:/t"), 10000, FILEOP_DELETE, Some(1025)).unwrap();
        assert_eq!(l.required("c:"), Ok(4096));
        let mut l = List {
            flags: SPDSL_IGNORE_DISK,
            ..Default::default()
        };
        l.add(k("C:/t"), 4097, FILEOP_COPY, Some(1025)).unwrap();
        assert_eq!(l.required("c:"), Ok(8192));
        assert_eq!(l.add(k("C:/t"), 0, 1, None), Err(ERROR_INVALID_PARAMETER));
    }

    #[test]
    fn names() {
        assert_eq!(file_key("", full), Err(ERROR_INVALID_NAME));
        for bad in ["W:/dir/", "\\\\unc\\file1", "\\\\.\\file1", "\\\\?\\file1"] {
            assert_eq!(file_key(bad, full), Err(ERROR_INVALID_PARAMETER), "{bad}");
        }
        assert_eq!(
            file_key("../file1", full).map(|k| k[..2].to_string()),
            Ok("c:".into())
        );
    }

    #[test]
    fn multi_sz() {
        let d: Vec<String> = ["h:", "p:", "r:"].map(String::from).to_vec();
        assert_eq!(drives_multi_sz(&d, 4), (b"h:\0".to_vec(), 7, false));
        assert_eq!(drives_multi_sz(&d, 8), (b"h:\0p:\0".to_vec(), 10, false));
        assert_eq!(
            drives_multi_sz(&d, 20),
            (b"h:\0p:\0r:\0\0".to_vec(), 10, true)
        );
        assert_eq!(drives_multi_sz(&[], 1), (b"\0".to_vec(), 1, true));
        assert_eq!(drives_multi_sz(&[], 0), (vec![], 1, false));
    }

    proptest! {
        /// the drive's total is the sum of the latest operation per file, in whole clusters
        #[test]
        fn totals_are_whole_clusters(ops in proptest::collection::vec((0u8..4, 0i64..100_000, any::<bool>(), proptest::option::of(0i64..50_000)), 1..30)) {
            let mut l = List::default();
            for (f, size, copy, existing) in &ops {
                let _ = l.add(format!("c:\\f{f}"), *size, if *copy { FILEOP_COPY } else { FILEOP_DELETE }, *existing);
            }
            let t = l.required("c:").unwrap();
            prop_assert_eq!(t % CLUSTER, 0);
            prop_assert!(l.files.len() <= 4);
            prop_assert_eq!(l.drives(), vec!["c:".to_string()]);
        }
    }
}
