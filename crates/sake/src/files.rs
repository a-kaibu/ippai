//! Files: Windows paths over the host's (node's) or the page's (kura vfs) file system.
//!
//! Z:\ is the host root (as in Wine), C:\ is SAKE_C (default /tmp/sake-c). Names are
//! matched without case when the exact one is missing.

use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Mutex;

pub struct File {
    pub f: Mutex<kura::fs::File>,
    pub path: String,
    pub dir: bool,
    /// opened for writing (WriteFile fails with ERROR_ACCESS_DENIED otherwise)
    pub write: bool,
    /// a directory's listing being read by NtQueryDirectoryFile, with the mask it took
    pub scan: Mutex<Option<(Vec<u16>, Find)>>,
    /// its entry in OPEN (sharing checks), removed when the handle goes
    share: u64,
    /// FILE_FLAG_DELETE_ON_CLOSE: the file goes with the last handle (or the process)
    delete: std::sync::atomic::AtomicBool,
}

impl Drop for File {
    fn drop(&mut self) {
        // (closing the handle releases its byte-range locks)
        let me = self.lock_owner();
        let mut locks = LOCKS.lock().unwrap();
        if !locks.is_empty() {
            locks.release(me);
        }
        drop(locks);
        if self.share != 0 {
            OPEN.lock().unwrap().retain(|o| o.0 != self.share);
        }
        if self.delete.load(std::sync::atomic::Ordering::Relaxed) {
            DELETE_ON_CLOSE.lock().unwrap().retain(|p| *p != self.path);
            let _ = kura::fs::remove_file(&self.path);
        }
    }
}

/// The byte-range locks of all files (LockFileEx; kura::filelock).
pub static LOCKS: Mutex<kura::filelock::Locks> = Mutex::new(kura::filelock::Locks::new());

impl File {
    /// Who holds a lock: the open file (one per CreateFile; DuplicateHandle shares it).
    pub fn lock_owner(&self) -> u64 {
        self as *const File as usize as u64
    }
    /// The file a lock is on (paths match without case).
    pub fn lock_key(&self) -> String {
        self.path.to_lowercase()
    }
    /// FILE_FLAG_DELETE_ON_CLOSE: deleted when its last handle closes, or when the process
    /// ends with it open (as Windows closes a process's handles)
    pub fn delete_on_close(&self) {
        if self.dir || self.delete.swap(true, std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let mut l = DELETE_ON_CLOSE.lock().unwrap();
        static AT_EXIT: std::sync::Once = std::sync::Once::new();
        AT_EXIT.call_once(|| unsafe {
            libc::atexit(delete_at_exit);
        });
        l.push(self.path.clone());
    }
}

/// The files open with FILE_FLAG_DELETE_ON_CLOSE.
static DELETE_ON_CLOSE: Mutex<Vec<String>> = Mutex::new(Vec::new());

extern "C" fn delete_at_exit() {
    let l = std::mem::take(&mut *DELETE_ON_CLOSE.lock().unwrap_or_else(|e| e.into_inner()));
    for p in l {
        let _ = kura::fs::remove_file(&p);
    }
}

/// CreateFile's sharing: (entry id, host path in lower case, access bits, share mode) of each
/// open handle that asked for a share mode (the C runtime's and sake's own opens share all)
static OPEN: Mutex<Vec<(u64, String, u32, u32)>> = Mutex::new(Vec::new());
const R: u32 = 1;
const W: u32 = 2;
const D: u32 = 4;

/// The read/write/delete bits of a CreateFile access mask.
fn access_bits(access: u32) -> u32 {
    let mut b = 0;
    if access & (0x8000_0000 | 0x1000_0000 | 1) != 0 {
        b |= R;
    }
    if access & (0x4000_0000 | 0x1000_0000 | 2 | 4) != 0 {
        b |= W;
    }
    if access & (0x0001_0000 | 0x1000_0000) != 0 {
        b |= D;
    }
    b
}

/// ERROR_SHARING_VIOLATION when an open handle of `host` does not share what this one wants or
/// this one does not share what that one has.
fn share_conflict(host: &str, want: u32, share: u32) -> bool {
    let key = host.to_lowercase();
    OPEN.lock().unwrap().iter().any(|o| o.1 == key && (want & !o.3 & 7 != 0 || o.2 & !share & 7 != 0))
}

/// Whether some handle keeps `host` from being deleted or renamed (opened without FILE_SHARE_DELETE).
pub fn delete_blocked(host: &str) -> bool {
    share_conflict(host, D, 7)
}

/// Not found: ERROR_PATH_NOT_FOUND when the directory is missing too, else ERROR_FILE_NOT_FOUND.
pub fn missing(w: &str) -> u32 {
    // (no name at all, as NULL: ERROR_PATH_NOT_FOUND)
    if w.is_empty() {
        return ERROR_PATH_NOT_FOUND;
    }
    let full = full_path(w);
    match full.trim_end_matches('\\').rsplit_once('\\') {
        Some((d, _)) if resolve(&format!("{d}\\")).is_none() => ERROR_PATH_NOT_FOUND,
        _ => ERROR_FILE_NOT_FOUND,
    }
}

/// FILE_ATTRIBUTE_READONLY and the like set by SetFileAttributes or asked of CreateFile (the
/// host files keep none): host path in lower case -> attributes. FILE_ATTRIBUTE_ARCHIVE is kept
/// beside read-only / hidden / system: a file made hidden is 0x22, one set hidden 0x2.
pub static ATTRS: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());
pub fn set_attrs(host: &str, a: u32) {
    let key = host.to_lowercase();
    let mut t = ATTRS.lock().unwrap();
    t.retain(|x| x.0 != key);
    if a & 0x7 != 0 {
        t.push((key, a & 0x27));
    }
}
pub fn extra_attrs(host: &str) -> u32 {
    let key = host.to_lowercase();
    ATTRS.lock().unwrap().iter().find(|x| x.0 == key).map_or(0, |x| x.1)
}

/// SetFileTime's last write times (the host files keep the host's own): host path in lower case
/// -> (the FILETIME set, the host's modification time then). A write since (the host's time
/// moved) shows the host's time again.
static TIMES: Mutex<Vec<(String, u64, u64)>> = Mutex::new(Vec::new());
pub fn set_write_time(host: &str, ft: u64) {
    let key = host.to_lowercase();
    let now = kura::fs::metadata(host).ok().and_then(|m| m.modified().ok()).map_or(0, filetime);
    let mut t = TIMES.lock().unwrap();
    t.retain(|x| x.0 != key);
    t.push((key, ft, now));
}
/// The last write time of host file `host` (metadata `m`) as a FILETIME.
pub fn write_time(host: &str, m: &kura::fs::Metadata) -> u64 {
    let real = m.modified().map_or(0, filetime);
    let key = host.to_lowercase();
    TIMES.lock().unwrap().iter().find(|x| x.0 == key && x.2 == real).map_or(real, |x| x.1)
}
/// A file's index (BY_HANDLE_FILE_INFORMATION): the host's inode, kept by renames, else a hash
/// of its host path.
pub fn file_index(host: &str, m: &kura::fs::Metadata) -> u64 {
    if m.ino() != 0 {
        return m.ino();
    }
    host.to_lowercase().bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

pub struct Find {
    pub entries: Vec<(String, kura::fs::Metadata)>,
    pub at: usize,
}

static CWD: Mutex<String> = Mutex::new(String::new());

pub fn cwd() -> String {
    let c = CWD.lock().unwrap();
    if c.is_empty() {
        "C:\\".into()
    } else {
        c.clone()
    }
}

pub fn set_cwd(d: &str) {
    let mut s = d.to_string();
    if !s.ends_with('\\') {
        s.push('\\');
    }
    *CWD.lock().unwrap() = s;
}

fn c_root() -> String {
    static C: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    C.get_or_init(|| drive_root(std::env::var("SAKE_C").unwrap_or_else(|_| "/tmp/sake-c".into()))).clone()
}

/// The host directory of a drive, made when missing: C:\ is always there on Windows (stat,
/// GetFileAttributes and FindFirstFile of "C:\" find a directory; winref crt_io, #72)
fn drive_root(h: String) -> String {
    let _ = kura::fs::create_dir_all(&h);
    h
}

/// Host path of a host file as a Windows path.
pub fn host_to_win(h: &str) -> String {
    let c = c_root();
    if let Some(rest) = h.strip_prefix(&c) {
        return format!("C:{}", rest.replace('/', "\\"));
    }
    format!("Z:{}", h.replace('/', "\\"))
}

/// Whether host path `h` is a directory (kura's file system, not std's).
pub fn is_dir<P: AsRef<std::path::Path>>(h: P) -> bool {
    kura::fs::metadata(h).is_ok_and(|m| m.is_dir())
}

/// Absolute Windows path of `p` (GetFullPathName): drive, no "." or "..", backslashes.
pub fn full_path(p: &str) -> String {
    full_path_trim(p, true)
}
/// full_path; `trim` false for a \\?\ path, whose last component keeps trailing dots and
/// spaces (CreateFile of \\?\C:\d\a. makes "a."): such a path stays \\?\-prefixed so that
/// taking its full path again keeps them
fn full_path_trim(p: &str, trim: bool) -> String {
    let p = p.replace('/', "\\");
    let abs = if p.len() >= 2 && p.as_bytes()[1] == b':' && p.is_char_boundary(2) {
        if p.len() == 2 || p.as_bytes()[2] != b'\\' {
            format!("{}\\{}", &p[..2], &p[2..])
        } else {
            p
        }
    } else if let Some(rest) = p.strip_prefix("\\\\?\\").or_else(|| p.strip_prefix("\\??\\")) {
        // (an NT or long path; an empty or driveless one is taken from the current directory)
        let rest = if rest.len() >= 2 && rest.as_bytes()[1] == b':' { rest.to_string() } else { format!("{}{}", cwd(), rest) };
        return full_path_trim(&rest, false);
    } else if let Some(rest) = p.strip_prefix("\\\\") {
        // UNC: \\server\share stays the root (".." does not go above it)
        let mut it = rest.splitn(3, '\\');
        let (srv, share, tail) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
        let mut parts: Vec<&str> = Vec::new();
        for c in tail.split('\\') {
            match c {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                c => parts.push(c),
            }
        }
        let mut out = format!("\\\\{srv}\\{share}");
        for c in parts {
            out.push('\\');
            out.push_str(c);
        }
        return out;
    } else if p.starts_with('\\') {
        format!("{}{}", &cwd()[..2], p)
    } else {
        format!("{}{}", cwd(), p)
    };
    let mut parts: Vec<&str> = Vec::new();
    let (drive, rest) = abs.split_at(2);
    for c in rest.split('\\') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    // (the last component loses its trailing dots and spaces: "trail. ." is "trail")
    let mut kept = false;
    if let Some(l) = parts.last_mut() {
        let t = l.trim_end_matches(['.', ' ']);
        if !t.is_empty() && t.len() != l.len() {
            if trim {
                *l = t;
            } else {
                kept = true;
            }
        }
    }
    let trailing = abs.ends_with('\\') && !parts.is_empty();
    let mut out = format!("{}{}\\{}", if kept { "\\\\?\\" } else { "" }, drive.to_ascii_uppercase(), parts.join("\\"));
    if trailing {
        out.push('\\');
    }
    out
}

/// Host path for a Windows path, without checking that it exists.
pub fn to_host(w: &str) -> String {
    let w = full_path(w);
    let w = w.strip_prefix("\\\\?\\").unwrap_or(&w);
    let (drive, rest) = w.split_at(2);
    let rest = rest.replace('\\', "/");
    match drive {
        "Z:" => if rest.is_empty() { "/".into() } else { rest },
        "C:" => format!("{}{}", c_root(), rest),
        d => format!("{}/{}{}", c_root(), &d[..1].to_ascii_lowercase(), rest),
    }
}

fn exists(h: &str) -> bool {
    kura::fs::symlink_metadata(h).is_ok()
}

/// The host path of an existing file or directory `w` (case-insensitive match).
pub fn resolve(w: &str) -> Option<String> {
    if w.is_empty() {
        return None;
    }
    let h = to_host(w);
    if exists(&h) {
        return Some(h);
    }
    let mut at = String::from("/");
    for comp in h.split('/').filter(|c| !c.is_empty()) {
        let cand = if at == "/" { format!("/{comp}") } else { format!("{at}/{comp}") };
        if exists(&cand) {
            at = cand;
            continue;
        }
        let found = kura::fs::name_nocase(&at, comp)?;
        at = if at == "/" { format!("/{found}") } else { format!("{at}/{found}") };
    }
    Some(at)
}

/// The host path for creating `w`: its directory resolved, the name as given.
pub fn resolve_new(w: &str) -> String {
    let full = full_path(w);
    // (a trailing backslash, "save\\" as CreateDirectory takes it: the name is the last
    // component before it, else the parent's spelling was not looked up and a parent spelled in
    // another case than the host's was not found; YU-RIS makes its save folder this way after
    // lowering the case of the game's path)
    let t = full.trim_end_matches('\\');
    let full = if t.len() > 2 { t.to_string() } else { full };
    match full.rsplit_once('\\') {
        Some((d, n)) if !n.is_empty() => match resolve(&format!("{d}\\")) {
            Some(hd) => format!("{}/{n}", hd.trim_end_matches('/')),
            None => to_host(&full),
        },
        _ => to_host(&full),
    }
}

/// (C:\windows\temp goes into a C:\Windows made before, not beside it on a case-sensitive
/// (C:\\windows\\temp goes into a C:\\Windows made before, not beside it on a case-sensitive
/// host). The host path.
pub fn make_dirs(w: &str) -> String {
    let full = full_path(w);
    let full = full.trim_end_matches('\\');
    let mut at = String::new();
    let mut host = to_host(full);
    for (i, part) in full.split('\\').enumerate() {
        if i > 0 {
            at.push('\\');
        }
        at.push_str(part);
        if i == 0 {
            continue;
        }
        host = match resolve(&at) {
            Some(h) => h,
            None => {
                let h = resolve_new(&at);
                let _ = kura::fs::create_dir(&h);
                h
            }
        };
    }
    host
}

pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_PATH_NOT_FOUND: u32 = 3;
pub const ERROR_ACCESS_DENIED: u32 = 5;
pub const ERROR_FILE_EXISTS: u32 = 80;
pub const ERROR_ALREADY_EXISTS: u32 = 183;

/// CreateFile: (handle object, error), sharing everything
pub fn open(w: &str, access: u32, disposition: u32) -> Result<File, u32> {
    open_shared(w, access, disposition, None).map(|f| f.0)
}

/// CreateFile with its share mode (None: no sharing checks): (handle object, whether it existed)
pub fn open_shared(w: &str, access: u32, disposition: u32, share: Option<u32>) -> Result<(File, bool), u32> {
    if w.is_empty() {
        return Err(ERROR_PATH_NOT_FOUND);
    }
    let write = access & 0x4000_0000 != 0 || access & 0x1000_0000 != 0 || access & 2 != 0;
    let existing = resolve(w);
    let host = existing.clone().unwrap_or_else(|| resolve_new(w));
    if let Some(h) = &existing {
        if kura::fs::metadata(h).is_ok_and(|m| m.is_dir()) {
            let f = kura::fs::File::open(h).map_err(|_| ERROR_ACCESS_DENIED)?;
            return Ok((File { f: Mutex::new(f), path: h.clone(), dir: true, write: false, scan: Mutex::new(None), share: 0, delete: Default::default() }, true));
        }
        if share.is_some_and(|s| share_conflict(h, access_bits(access), s)) {
            return Err(32);
        }
        if write && extra_attrs(h) & 1 != 0 {
            return Err(ERROR_ACCESS_DENIED);
        }
    }
    let mut o = kura::fs::OpenOptions::new();
    o.read(true);
    match disposition {
        1 => {
            // CREATE_NEW
            if existing.is_some() {
                return Err(ERROR_FILE_EXISTS);
            }
            o.write(true).create_new(true);
        }
        2 => {
            o.write(true).create(true).truncate(true);
        }
        3 => {
            if existing.is_none() {
                return Err(missing(w));
            }
            if write {
                o.write(true);
            }
        }
        4 => {
            o.write(true).create(true);
        }
        5 => {
            if existing.is_none() {
                return Err(missing(w));
            }
            o.write(true).truncate(true);
        }
        _ => return Err(87),
    }
    match o.open(&host) {
        Ok(f) => {
            let id = match share {
                Some(s) => {
                    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    OPEN.lock().unwrap().push((id, host.to_lowercase(), access_bits(access), s));
                    id
                }
                None => 0,
            };
            Ok((File { f: Mutex::new(f), path: host, dir: false, write, scan: Mutex::new(None), share: id, delete: Default::default() }, existing.is_some()))
        }
        Err(e) => Err(match e.kind() {
            std::io::ErrorKind::NotFound => {
                if existing.is_none() && disposition != 3 {
                    ERROR_PATH_NOT_FOUND
                } else {
                    ERROR_FILE_NOT_FOUND
                }
            }
            std::io::ErrorKind::AlreadyExists => ERROR_FILE_EXISTS,
            _ => ERROR_ACCESS_DENIED,
        }),
    }
}

impl File {
    pub fn read_into(&self, dst: u32, n: u32) -> u32 {
        let buf = crate::mem::slice_mut(dst, n);
        let mut f = self.f.lock().unwrap();
        let mut got = 0;
        while got < buf.len() {
            match f.read(&mut buf[got..]) {
                Ok(0) => break,
                Ok(k) => got += k,
                Err(_) => break,
            }
        }
        got as u32
    }
    /// Up to `n` bytes into host memory (fewer at the end of the file)
    pub fn read_vec(&self, n: usize) -> Vec<u8> {
        let mut buf = vec![0; n];
        let mut f = self.f.lock().unwrap();
        let mut got = 0;
        while got < n {
            match f.read(&mut buf[got..]) {
                Ok(0) | Err(_) => break,
                Ok(k) => got += k,
            }
        }
        buf.truncate(got);
        buf
    }
    pub fn write_bytes(&self, b: &[u8]) -> u32 {
        let mut f = self.f.lock().unwrap();
        f.write_all(b).map_or(0, |_| b.len() as u32)
    }
    pub fn write_from(&self, src: u32, n: u32) -> u32 {
        let mut f = self.f.lock().unwrap();
        f.write_all(crate::mem::slice(src, n)).map_or(0, |_| n)
    }
    pub fn seek(&self, pos: SeekFrom) -> Option<u64> {
        self.f.lock().unwrap().seek(pos).ok()
    }
    pub fn size(&self) -> u64 {
        self.f.lock().unwrap().metadata().map_or(0, |m| m.len())
    }
}

/// FILETIME (100 ns since 1601) of a system time.
pub fn filetime(t: std::time::SystemTime) -> u64 {
    let d = t.duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    116_444_736_000_000_000 + d.as_nanos() as u64 / 100
}

/// Attributes of host file `h` (with those SetFileAttributes gave it).
pub fn attrs_of(h: &str, m: &kura::fs::Metadata) -> u32 {
    // (attributes set replace the archive bit the host file shows)
    match extra_attrs(h) {
        0 => attrs(m),
        x => attrs(m) & 0x10 | x,
    }
}

pub fn attrs(m: &kura::fs::Metadata) -> u32 {
    if m.is_dir() {
        0x10
    } else if m.permissions().readonly() {
        0x21
    } else {
        0x20
    }
}

/// Wildcard match without case (* and ?), the way FindFirstFile matches.
pub fn wild(pat: &str, name: &str) -> bool {
    let p: Vec<char> = pat.to_lowercase().chars().collect();
    let n: Vec<char> = name.to_lowercase().chars().collect();
    fn m(p: &[char], n: &[char]) -> bool {
        match p.first() {
            None => n.is_empty(),
            Some('*') => (0..=n.len()).any(|i| m(&p[1..], &n[i..])),
            // (DOS: '?' also matches nothing at the end of the name or before its dot)
            Some('?') => !n.is_empty() && m(&p[1..], &n[1..]) || (n.is_empty() || n[0] == '.') && m(&p[1..], n),
            Some(c) => n.first() == Some(c) && m(&p[1..], &n[1..]),
        }
    }
    // "*.*" matches names without a dot too
    if pat == "*.*" {
        return true;
    }
    // "name." and "*.": names without an extension
    if let Some(base) = pat.strip_suffix('.').filter(|b| !b.is_empty() && !b.ends_with('.')) {
        return (name == "." || name == ".." || !name.contains('.')) && wild(base, name);
    }
    m(&p, &n)
}

pub fn find(w: &str) -> Option<Find> {
    let full = full_path(w);
    let (dir, mut pat) = full.rsplit_once('\\')?;
    // (the pattern as given: GetFullPathName's normalization drops the dot of "*.")
    let given = w.rsplit(['\\', '/']).next().unwrap_or("");
    if !given.is_empty() && given.trim_end_matches(['.', ' ']) == pat {
        pat = given;
    }
    // ("" and "dir\" find nothing, as on Windows)
    if w.is_empty() || pat.is_empty() {
        return None;
    }
    let hd = resolve(&format!("{dir}\\"))?;
    let mut entries = Vec::new();
    if !pat.contains(['*', '?']) {
        let h = resolve(&full)?;
        let m = kura::fs::metadata(&h).ok()?;
        let name = h.rsplit('/').next().unwrap_or("").to_string();
        return Some(Find { entries: vec![(name, m)], at: 0 });
    }
    if let Ok(m) = kura::fs::metadata(&hd) {
        // ("." and ".." as listed entries: only where the pattern takes them, as "*", not "*.ttf")
        if dir.len() > 3 {
            for d in [".", ".."] {
                if wild(pat, d) {
                    entries.push((d.into(), m.clone()));
                }
            }
        }
    }
    for e in kura::fs::read_dir(&hd).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if wild(pat, &name) {
            if let Ok(m) = e.metadata() {
                entries.push((name, m));
            }
        }
    }
    if entries.is_empty() {
        return None;
    }
    Some(Find { entries, at: 0 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drive_root_is_made_when_missing() {
        let h = std::env::temp_dir().join(format!("sake-drive-{}", std::process::id())).join("c");
        let h = h.to_string_lossy().into_owned();
        let _ = std::fs::remove_dir_all(&h);
        assert_eq!(drive_root(h.clone()), h);
        assert!(kura::fs::metadata(&h).unwrap().is_dir());
        // (and again, when it is there)
        assert_eq!(drive_root(h.clone()), h);
        // C:\ itself, as stat and GetFileAttributes look it up
        let c = resolve("C:\\").expect("C:\\ is there");
        assert!(kura::fs::metadata(&c).unwrap().is_dir());
        assert!(resolve("C:/").is_some() && resolve("c:\\").is_some());
        let _ = std::fs::remove_dir_all(std::path::Path::new(&h).parent().unwrap());
    }

    #[test]
    fn attributes_made_and_set_as_windows() {
        let f = std::env::temp_dir().join(format!("sake-attrs-{}", std::process::id()));
        std::fs::write(&f, b"abc").unwrap();
        let (h, m) = (f.to_string_lossy().into_owned(), kura::fs::metadata(&f).unwrap());
        assert_eq!(attrs_of(&h, &m), 0x20);
        // made hidden by CreateFile / CreateFile2: hidden and archive (winref files: 34)
        set_attrs(&h, 2 | 0x20);
        assert_eq!(attrs_of(&h, &m), 0x22);
        // SetFileAttributes replaces: hidden alone, then read-only alone (winref files c.dat: 1)
        set_attrs(&h, 2);
        assert_eq!(attrs_of(&h, &m), 2);
        set_attrs(&h, 1);
        assert_eq!(attrs_of(&h, &m), 1);
        set_attrs(&h, 0x23);
        assert_eq!(attrs_of(&h, &m), 0x23);
        // normal, or archive alone: what the host file shows
        set_attrs(&h, 0x80);
        assert_eq!(attrs_of(&h, &m), 0x20);
        set_attrs(&h, 0x20);
        assert_eq!(attrs_of(&h, &m), 0x20);
        // the case of the host path does not matter
        set_attrs(&h.to_uppercase(), 4);
        assert_eq!(extra_attrs(&h), 4);
        set_attrs(&h, 0x80);
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn set_write_times_hold_until_the_file_changes() {
        let f = std::env::temp_dir().join(format!("sake-times-{}", std::process::id()));
        std::fs::write(&f, b"abc").unwrap();
        let h = f.to_string_lossy().into_owned();
        let m = kura::fs::metadata(&h).unwrap();
        let real = write_time(&h, &m);
        assert_ne!(real, 0);
        // _Set_last_write_time to 1970 + 1 s: GetFileTime gives it back
        set_write_time(&h, 116_444_736_010_000_000);
        assert_eq!(write_time(&h, &kura::fs::metadata(&h).unwrap()), 116_444_736_010_000_000);
        // a write after it moves the host's time: the host's time shows again
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&f, b"abcd").unwrap();
        let m2 = kura::fs::metadata(&h).unwrap();
        if m2.modified().ok() != m.modified().ok() {
            assert_eq!(write_time(&h, &m2), m2.modified().map_or(0, filetime));
        }
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn file_indexes_tell_files_apart_and_follow_hard_links() {
        let d = std::env::temp_dir().join(format!("sake-index-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let (a, b, l) = (d.join("a"), d.join("b"), d.join("l"));
        std::fs::write(&a, b"1").unwrap();
        std::fs::write(&b, b"2").unwrap();
        let idx = |p: &std::path::Path| {
            let h = p.to_string_lossy().into_owned();
            file_index(&h, &kura::fs::metadata(&h).unwrap())
        };
        assert_ne!(idx(&a), idx(&b));
        assert_eq!(idx(&a), idx(&a));
        kura::fs::hard_link(&a, &l).unwrap();
        assert_eq!(idx(&a), idx(&l));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn made_directories_follow_the_existing_spelling() {
        let root = std::env::temp_dir().join(format!("sake-mkdirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Windows").join("System32")).unwrap();
        let r = root.to_string_lossy().into_owned();
        // as the Z: drive maps the host's /, a path under the temporary root
        let w = |p: &str| format!("Z:{}\\{p}", r.replace('/', "\\"));
        let h = make_dirs(&w("windows\\temp"));
        assert!(h.ends_with("Windows/temp"), "{h}");
        assert!(!root.join("windows").exists());
        // a second time: the same directory
        assert_eq!(make_dirs(&w("WINDOWS\\TEMP\\")), h);
        // new parents too
        let h2 = make_dirs(&w("windows\\a\\b"));
        assert!(h2.ends_with("Windows/a/b") && std::path::Path::new(&h2).is_dir(), "{h2}");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A new name under a parent spelled in another case, with trailing backslashes or not
    /// (YU-RIS: CreateDirectory("...\\(実況ok)\\save\\") with the folder on the host as
    /// "(実況OK)"): the parent's host spelling, the new name as given.
    #[test]
    fn new_names_follow_the_parent_spelling_with_trailing_backslashes() {
        let root = std::env::temp_dir().join(format!("sake-newname-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Game（実況OK）")).unwrap();
        let r = root.to_string_lossy().into_owned();
        let w = |p: &str| format!("Z:{}\\{p}", r.replace('/', "\\"));
        let want = format!("{r}/Game（実況OK）/save");
        assert_eq!(resolve_new(&w("game（実況ok）\\save")), want);
        assert_eq!(resolve_new(&w("game（実況ok）\\save\\")), want);
        assert_eq!(resolve_new(&w("GAME（実況OK）\\save\\\\")), want);
        kura::fs::create_dir(resolve_new(&w("game（実況ok）\\save\\"))).unwrap();
        assert!(root.join("Game（実況OK）").join("save").is_dir());
        // a drive root stays a root
        assert_eq!(resolve_new("Z:\\"), "/");
        let _ = std::fs::remove_dir_all(&root);
    }

    proptest::proptest! {
        /// Any case of the parent's ASCII letters and any number of trailing backslashes: the
        /// same host path, under the parent as the host spells it.
        #[test]
        fn new_name_case_and_trailing_separators(flips in proptest::collection::vec(proptest::bool::ANY, 12), tail in 0usize..4, name in "[a-z0-9]{1,8}") {
            let root = std::env::temp_dir().join(format!("sake-newprop-{}", std::process::id()));
            std::fs::create_dir_all(root.join("SaveData").join("Sub")).unwrap();
            let r = root.to_string_lossy().into_owned();
            let parent: String = "SaveData\\Sub".chars().zip(flips.iter().cycle()).map(|(c, &f)| if f { c.to_ascii_lowercase() } else { c.to_ascii_uppercase() }).collect();
            let w = format!("Z:{}\\{parent}\\{name}{}", r.replace('/', "\\"), "\\".repeat(tail));
            proptest::prop_assert_eq!(resolve_new(&w), format!("{r}/SaveData/Sub/{name}"));
        }
    }
}
