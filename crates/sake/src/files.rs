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
    let abs = if p.len() >= 2 && p.as_bytes()[1] == b':' {
        if p.len() == 2 || p.as_bytes()[2] != b'\\' {
            format!("{}\\{}", &p[..2], &p[2..])
        } else {
            p
        }
    } else if let Some(rest) = p.strip_prefix("\\\\?\\") {
        rest.to_string()
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

/// CreateFile: (handle object, error)
pub fn open(w: &str, access: u32, disposition: u32) -> Result<File, u32> {
    let write = access & 0x4000_0000 != 0 || access & 0x1000_0000 != 0 || access & 2 != 0;
    let existing = resolve(w);
    let host = existing.clone().unwrap_or_else(|| resolve_new(w));
    if let Some(h) = &existing {
        if kura::fs::metadata(h).is_ok_and(|m| m.is_dir()) {
            let f = kura::fs::File::open(h).map_err(|_| ERROR_ACCESS_DENIED)?;
            return Ok(File { f: Mutex::new(f), path: h.clone(), dir: true });
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
                return Err(ERROR_FILE_NOT_FOUND);
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
                return Err(ERROR_FILE_NOT_FOUND);
            }
            o.write(true).truncate(true);
        }
        _ => return Err(87),
    }
    match o.open(&host) {
        Ok(f) => Ok(File { f: Mutex::new(f), path: host, dir: false }),
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
            Some('?') => !n.is_empty() && m(&p[1..], &n[1..]) || n.is_empty() && m(&p[1..], n),
            Some(c) => n.first() == Some(c) && m(&p[1..], &n[1..]),
        }
    }
    // "*.*" matches names without a dot too
    if pat == "*.*" {
        return true;
    }
    m(&p, &n)
}

pub fn find(w: &str) -> Option<Find> {
    let full = full_path(w);
    let (dir, pat) = full.rsplit_once('\\')?;
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
