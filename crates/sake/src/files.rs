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
    /// its entry in OPEN (sharing checks), removed when the handle goes
    share: u64,
}

impl Drop for File {
    fn drop(&mut self) {
        if self.share != 0 {
            OPEN.lock().unwrap().retain(|o| o.0 != self.share);
        }
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
    let full = full_path(w);
    match full.trim_end_matches('\\').rsplit_once('\\') {
        Some((d, _)) if resolve(&format!("{d}\\")).is_none() => ERROR_PATH_NOT_FOUND,
        _ => ERROR_FILE_NOT_FOUND,
    }
}

/// FILE_ATTRIBUTE_READONLY and the like set by SetFileAttributes (the host files keep none):
/// host path in lower case -> attributes
pub static ATTRS: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());
pub fn set_attrs(host: &str, a: u32) {
    let key = host.to_lowercase();
    let mut t = ATTRS.lock().unwrap();
    t.retain(|x| x.0 != key);
    if a & !0x80 & 0x7 != 0 {
        t.push((key, a & 0x7));
    }
}
pub fn extra_attrs(host: &str) -> u32 {
    let key = host.to_lowercase();
    ATTRS.lock().unwrap().iter().find(|x| x.0 == key).map_or(0, |x| x.1)
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
    C.get_or_init(|| std::env::var("SAKE_C").unwrap_or_else(|_| "/tmp/sake-c".into())).clone()
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
    let p = p.replace('/', "\\");
    let abs = if p.len() >= 2 && p.as_bytes()[1] == b':' && p.is_char_boundary(2) {
        if p.len() == 2 || p.as_bytes()[2] != b'\\' {
            format!("{}\\{}", &p[..2], &p[2..])
        } else {
            p
        }
    } else if let Some(rest) = p.strip_prefix("\\\\?\\").or_else(|| p.strip_prefix("\\??\\")) {
        // (an NT or long path; an empty or driveless one is taken from the current directory)
        if rest.len() >= 2 && rest.as_bytes()[1] == b':' { full_path(rest) } else { format!("{}{}", cwd(), rest) }
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
    if let Some(l) = parts.last_mut() {
        let t = l.trim_end_matches(['.', ' ']);
        if !t.is_empty() {
            *l = t;
        }
    }
    let trailing = abs.ends_with('\\') && !parts.is_empty();
    let mut out = format!("{}\\{}", drive.to_ascii_uppercase(), parts.join("\\"));
    if trailing {
        out.push('\\');
    }
    out
}

/// Host path for a Windows path, without checking that it exists.
pub fn to_host(w: &str) -> String {
    let w = full_path(w);
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
    match full.rsplit_once('\\') {
        Some((d, n)) if !n.is_empty() => match resolve(&format!("{d}\\")) {
            Some(hd) => format!("{}/{n}", hd.trim_end_matches('/')),
            None => to_host(&full),
        },
        _ => to_host(&full),
    }
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
    let write = access & 0x4000_0000 != 0 || access & 0x1000_0000 != 0 || access & 2 != 0;
    let existing = resolve(w);
    let host = existing.clone().unwrap_or_else(|| resolve_new(w));
    if let Some(h) = &existing {
        if kura::fs::metadata(h).is_ok_and(|m| m.is_dir()) {
            let f = kura::fs::File::open(h).map_err(|_| ERROR_ACCESS_DENIED)?;
            return Ok((File { f: Mutex::new(f), path: h.clone(), dir: true, write: false, share: 0 }, true));
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
            Ok((File { f: Mutex::new(f), path: host, dir: false, write, share: id }, existing.is_some()))
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
