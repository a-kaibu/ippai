//! The C runtime's low-level I/O (file descriptors: _open, _read, _write, _lseeki64, _fstat64...)
//! and the rest of what Python and SDL ask of the UCRT besides stdio: the wide environment,
//! the current directory, time zone variables, multibyte conversion, conio and process stubs.
//!
//! A descriptor is a slot in FDS: 0-2 are the standard streams (the log), the others share a
//! k32 file object (`Arc<File>`) with the handle _get_osfhandle gives out. All descriptors are
//! binary: text mode is remembered for _setmode but nothing is translated (Python and SDL open
//! everything with _O_BINARY).

use super::*;
use crate::obj::{self, Obj};
use std::sync::atomic::{AtomicU32, Ordering};

const EBADF: u32 = 9;
const EACCES: u32 = 13;
const EEXIST: u32 = 17;
const EINVAL: u32 = 22;
const ENOENT: u32 = 2;
const EMFILE: u32 = 24;

const O_WRONLY: u32 = 1;
const O_RDWR: u32 = 2;
const O_APPEND: u32 = 8;
const O_CREAT: u32 = 0x100;
const O_TRUNC: u32 = 0x200;
const O_EXCL: u32 = 0x400;
const O_TEXT: u32 = 0x4000;
const O_BINARY: u32 = 0x8000;

#[derive(Clone)]
enum Kind {
    /// standard stream 0, 1 or 2
    Std(usize),
    File(Arc<File>),
}

#[derive(Clone)]
struct Fd {
    kind: Kind,
    text: bool,
    append: bool,
    /// the handle _get_osfhandle gives (0: none made yet); closed with the descriptor
    osf: u32,
}

static FDS: Mutex<Vec<Option<Fd>>> = Mutex::new(Vec::new());

fn with_fds<R>(f: impl FnOnce(&mut Vec<Option<Fd>>) -> R) -> R {
    let mut g = FDS.lock().unwrap();
    if g.is_empty() {
        for i in 0..3 {
            g.push(Some(Fd {
                kind: Kind::Std(i),
                text: true,
                append: false,
                osf: 0,
            }));
        }
    }
    f(&mut g)
}

fn get(fd: u32) -> Option<Fd> {
    with_fds(|t| t.get(fd as usize).cloned().flatten())
}

/// The lowest free descriptor for `d` (-1 with EMFILE past 2048, the UCRT's limit).
fn add(d: Fd) -> u32 {
    with_fds(|t| {
        let i = t.iter().position(Option::is_none).unwrap_or(t.len());
        if i >= 2048 {
            set_errno(EMFILE);
            return u32::MAX;
        }
        if i == t.len() {
            t.push(None);
        }
        t[i] = Some(d);
        i as u32
    })
}

/// A descriptor sharing an open file (fopen's streams have one, for _fileno).
pub(super) fn fd_of_file(f: Arc<File>) -> u32 {
    add(Fd {
        kind: Kind::File(f),
        text: false,
        append: false,
        osf: 0,
    })
}

/// The file behind descriptor `fd` (None: a standard stream, or no such descriptor).
pub(super) fn file_of(fd: u32) -> Option<Arc<File>> {
    match get(fd)?.kind {
        Kind::File(f) => Some(f),
        Kind::Std(_) => None,
    }
}

pub(super) fn close_fd(fd: u32) -> bool {
    let Some(d) = with_fds(|t| t.get_mut(fd as usize).and_then(Option::take)) else { return false };
    if d.osf != 0 && !matches!(d.kind, Kind::Std(_)) {
        obj::close(d.osf);
    }
    true
}

/// errno for a Windows error.
fn errno_for(e: u32) -> u32 {
    match e {
        2 | 3 | 15 | 123 => ENOENT,
        80 | 183 => EEXIST,
        87 => EINVAL,
        _ => EACCES,
    }
}

fn path_arg(p: u32, wide: bool) -> String {
    if wide { String::from_utf16_lossy(&wstr(p)) } else { a2s(&cstr(p)) }
}

/// _open's work: the descriptor, or -1 with errno.
fn open(path: &str, oflag: u32) -> u32 {
    let access = match oflag & 3 {
        O_WRONLY => 0x4000_0000,
        O_RDWR => 0xc000_0000,
        _ => 0x8000_0000,
    };
    let disp = match (oflag & O_CREAT != 0, oflag & O_EXCL != 0, oflag & O_TRUNC != 0) {
        (true, true, _) => 1,
        (true, false, true) => 2,
        (true, false, false) => 4,
        (false, _, true) => 5,
        (false, _, false) => 3,
    };
    match crate::files::open(path, access, disp) {
        // (a directory does not open as a descriptor)
        Ok(f) if f.dir => {
            set_errno(EACCES);
            u32::MAX
        }
        Ok(f) => add(Fd {
            kind: Kind::File(Arc::new(f)),
            text: oflag & O_BINARY == 0 && oflag & O_TEXT != 0,
            append: oflag & O_APPEND != 0,
            osf: 0,
        }),
        Err(e) => {
            set_errno(errno_for(e));
            u32::MAX
        }
    }
}
fn _open(c: &mut Ctx) -> u32 {
    open(&path_arg(c.a(0), false), c.a(1))
}
fn _wopen(c: &mut Ctx) -> u32 {
    open(&path_arg(c.a(0), true), c.a(1))
}
/// _sopen / _wsopen (path, oflag, share, pmode): the share mode is not checked
fn _sopen(c: &mut Ctx) -> u32 {
    open(&path_arg(c.a(0), false), c.a(1))
}
fn _wsopen(c: &mut Ctx) -> u32 {
    open(&path_arg(c.a(0), true), c.a(1))
}
/// _sopen_s / _wsopen_s (&fd, path, oflag, share, pmode): errno
fn sopen_s(c: &mut Ctx, wide: bool) -> u32 {
    let (pfd, path) = (c.a(0), c.a(1));
    if pfd == 0 || path == 0 {
        set_errno(EINVAL);
        return EINVAL;
    }
    let fd = open(&path_arg(path, wide), c.a(2));
    w32(pfd, fd);
    if fd == u32::MAX { r32(super::_errno(c)) } else { 0 }
}
fn _sopen_s(c: &mut Ctx) -> u32 {
    sopen_s(c, false)
}
fn _wsopen_s(c: &mut Ctx) -> u32 {
    sopen_s(c, true)
}
fn bad() -> u32 {
    set_errno(EBADF);
    u32::MAX
}
fn _close(c: &mut Ctx) -> u32 {
    if close_fd(c.a(0)) { 0 } else { bad() }
}
/// _read(fd, buf, count): bytes read, 0 at the end (standard input: always at its end)
fn _read(c: &mut Ctx) -> u32 {
    let (fd, buf, n) = (c.a(0), c.a(1), c.a(2));
    match get(fd).map(|d| d.kind) {
        Some(Kind::File(f)) => f.read_into(buf, n.min(i32::MAX as u32)),
        Some(Kind::Std(0)) => 0,
        _ => bad(),
    }
}
/// _write(fd, buf, count): standard output and error go to the log
fn _write(c: &mut Ctx) -> u32 {
    let (fd, buf, n) = (c.a(0), c.a(1), c.a(2));
    let Some(d) = get(fd) else { return bad() };
    match d.kind {
        Kind::Std(0) => bad(),
        Kind::Std(_) => {
            crate::k32::console_write(&a2s(mem::slice(buf, n)));
            n
        }
        Kind::File(f) => {
            if !f.write {
                return bad();
            }
            if d.append {
                f.seek(SeekFrom::End(0));
            }
            crate::k32::echo_write(&f.path, buf, n);
            f.write_from(buf, n)
        }
    }
}
fn lseek(fd: u32, off: i64, origin: u32) -> Option<u64> {
    let f = match get(fd).map(|d| d.kind) {
        Some(Kind::File(f)) => f,
        _ => {
            set_errno(EBADF);
            return None;
        }
    };
    let pos = match origin {
        0 if off >= 0 => SeekFrom::Start(off as u64),
        1 => SeekFrom::Current(off),
        2 => SeekFrom::End(off),
        _ => {
            set_errno(EINVAL);
            return None;
        }
    };
    let r = f.seek(pos);
    if r.is_none() {
        set_errno(EINVAL);
    }
    r
}
/// _lseek(fd, long offset, origin): the new position (past 2 GB: -1, EINVAL)
fn _lseek(c: &mut Ctx) -> u32 {
    match lseek(c.a(0), c.i(1) as i64, c.a(2)) {
        Some(p) if p <= i32::MAX as u64 => p as u32,
        Some(_) => {
            set_errno(EINVAL);
            u32::MAX
        }
        None => u32::MAX,
    }
}
/// _lseeki64(fd, __int64 offset, origin)
fn _lseeki64(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (fd, off, origin) = (a.p(c), a.u64(c) as i64, a.p(c));
    let r = lseek(fd, off, origin).map_or(u64::MAX, |p| p);
    c.ret64(r)
}
fn _tell(c: &mut Ctx) -> u32 {
    lseek(c.a(0), 0, 1).map_or(u32::MAX, |p| p as u32)
}
fn _telli64(c: &mut Ctx) -> u32 {
    let r = lseek(c.a(0), 0, 1).unwrap_or(u64::MAX);
    c.ret64(r)
}
fn _filelength(c: &mut Ctx) -> u32 {
    file_of(c.a(0)).map_or_else(bad, |f| f.size() as u32)
}
fn _filelengthi64(c: &mut Ctx) -> u32 {
    let r = match file_of(c.a(0)) {
        Some(f) => f.size(),
        None => bad() as u64 | u64::MAX,
    };
    c.ret64(r)
}
fn _eof(c: &mut Ctx) -> u32 {
    let Some(f) = file_of(c.a(0)) else { return bad() };
    let pos = f.seek(SeekFrom::Current(0)).unwrap_or(0);
    (pos >= f.size()) as u32
}
/// _chsize / _chsize_s (fd, size): the file cut or zero-extended
fn chsize(fd: u32, size: u64) -> u32 {
    match file_of(fd) {
        Some(f) if f.write => {
            if f.f.lock().unwrap().set_len(size).is_ok() {
                0
            } else {
                set_errno(EACCES);
                EACCES
            }
        }
        Some(_) => {
            set_errno(EBADF);
            EBADF
        }
        None => {
            set_errno(EBADF);
            EBADF
        }
    }
}
fn _chsize(c: &mut Ctx) -> u32 {
    if chsize(c.a(0), c.a(1) as u64) == 0 { 0 } else { u32::MAX }
}
fn _chsize_s(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (fd, size) = (a.p(c), a.u64(c));
    chsize(fd, size)
}
fn _commit(c: &mut Ctx) -> u32 {
    if get(c.a(0)).is_some() { 0 } else { bad() }
}
fn _locking(c: &mut Ctx) -> u32 {
    if get(c.a(0)).is_some() { 0 } else { bad() }
}
/// _isatty: no descriptor is a console here (the log is not one), so programs buffer as for a file
fn _isatty(c: &mut Ctx) -> u32 {
    if get(c.a(0)).is_none() {
        set_errno(EBADF);
    }
    0
}
/// _setmode(fd, mode): the previous mode
fn _setmode(c: &mut Ctx) -> u32 {
    let (fd, mode) = (c.a(0), c.a(1));
    if mode & !(O_TEXT | O_BINARY | 0x10000 | 0x20000 | 0x40000) != 0 || mode == 0 {
        set_errno(EINVAL);
        return u32::MAX;
    }
    with_fds(|t| match t.get_mut(fd as usize).and_then(Option::as_mut) {
        Some(d) => {
            let old = if d.text { O_TEXT } else { O_BINARY };
            d.text = mode & O_BINARY == 0;
            old
        }
        None => bad(),
    })
}
/// _get_osfhandle(fd): the Windows handle (-1, EBADF, when there is none)
fn _get_osfhandle(c: &mut Ctx) -> u32 {
    let fd = c.a(0);
    with_fds(|t| match t.get_mut(fd as usize).and_then(Option::as_mut) {
        Some(d) => {
            if d.osf == 0 {
                d.osf = match &d.kind {
                    Kind::Std(i) => crate::k32::std_handle(*i),
                    Kind::File(f) => obj::add(Obj::File(f.clone())),
                };
            }
            d.osf
        }
        None => bad(),
    })
}
/// _open_osfhandle(handle, flags): a descriptor that owns the handle
fn _open_osfhandle(c: &mut Ctx) -> u32 {
    let (h, flags) = (c.a(0), c.a(1));
    let kind = obj::with(|t| match t.objs.get(&h) {
        Some((Obj::File(f), _)) => Some(Kind::File(f.clone())),
        Some((Obj::Other(k), _)) => Some(Kind::Std(if *k == "stdin" {
            0
        } else if *k == "stderr" {
            2
        } else {
            1
        })),
        _ => None,
    });
    match kind {
        Some(kind) => add(Fd {
            kind,
            text: flags & O_TEXT != 0,
            append: flags & O_APPEND != 0,
            osf: h,
        }),
        None => bad(),
    }
}
fn _dup(c: &mut Ctx) -> u32 {
    match get(c.a(0)) {
        Some(d) => add(Fd { osf: 0, ..d }),
        None => bad(),
    }
}
fn _dup2(c: &mut Ctx) -> u32 {
    let (from, to) = (c.a(0), c.a(1));
    let Some(d) = get(from) else { return bad() };
    if to >= 2048 {
        return bad();
    }
    if from == to {
        return 0;
    }
    close_fd(to);
    with_fds(|t| {
        if t.len() <= to as usize {
            t.resize(to as usize + 1, None);
        }
        t[to as usize] = Some(Fd { osf: 0, ..d });
    });
    0
}
fn _umask(c: &mut Ctx) -> u32 {
    static MASK: Mutex<u32> = Mutex::new(0);
    std::mem::replace(&mut *MASK.lock().unwrap(), c.a(0) & 0o600)
}

// ---------------------------------------------------------------------------------------------
// stat

/// The struct _stat* forms: the size's and the times' widths.
#[derive(Clone, Copy)]
enum StatKind {
    /// _stat32: 32-bit size and times
    S32,
    /// _stat32i64: 64-bit size
    S32i64,
    /// _stat64i32: 64-bit times
    S64i32,
    /// _stat64
    S64,
}

struct StatInfo {
    mode: u16,
    dev: u32,
    size: u64,
    atime: i64,
    mtime: i64,
    ctime: i64,
}

fn unix_time(t: std::io::Result<std::time::SystemTime>) -> i64 {
    t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64)
}

/// st_mode as the UCRT makes it: the type, read always, write unless read-only, execute for
/// directories and .exe/.com/.bat/.cmd, copied to the group and other bits
fn mode_of(dir: bool, readonly: bool, name: &str) -> u16 {
    let lower = name.to_ascii_lowercase();
    let exec = dir || [".exe", ".com", ".bat", ".cmd"].iter().any(|e| lower.ends_with(e));
    let mut m: u16 = 0o400;
    if !readonly {
        m |= 0o200;
    }
    if exec {
        m |= 0o100;
    }
    m |= m >> 3 | m >> 6;
    m | if dir { 0x4000 } else { 0x8000 }
}

/// read-only by the host's permissions or by _chmod / SetFileAttributes
fn readonly(m: &kura::fs::Metadata, host: &str) -> bool {
    m.permissions().readonly() || crate::files::extra_attrs(host) & 1 != 0
}

fn info_of(m: &kura::fs::Metadata, host: &str) -> StatInfo {
    let mtime = unix_time(m.modified());
    StatInfo {
        mode: mode_of(m.is_dir(), readonly(m, host), host),
        dev: 2,
        size: if m.is_dir() { 0 } else { m.len() },
        atime: mtime,
        mtime,
        ctime: mtime,
    }
}

fn put_stat(p: u32, k: StatKind, s: &StatInfo) {
    let total = match k {
        StatKind::S32 => 36,
        StatKind::S32i64 | StatKind::S64i32 => 48,
        StatKind::S64 => 56,
    };
    mem::fill(p, total, 0);
    w32(p, s.dev);
    w16(p + 6, s.mode);
    w16(p + 8, 1);
    w32(p + 16, s.dev);
    let t32 = |v: i64| v.clamp(0, i32::MAX as i64) as u32;
    let put64 = |a: u32, v: u64| mem::write(a, &v.to_le_bytes());
    match k {
        StatKind::S32 => {
            w32(p + 20, s.size.min(i32::MAX as u64) as u32);
            w32(p + 24, t32(s.atime));
            w32(p + 28, t32(s.mtime));
            w32(p + 32, t32(s.ctime));
        }
        StatKind::S64i32 => {
            w32(p + 20, s.size.min(i32::MAX as u64) as u32);
            put64(p + 24, s.atime as u64);
            put64(p + 32, s.mtime as u64);
            put64(p + 40, s.ctime as u64);
        }
        StatKind::S32i64 => {
            put64(p + 24, s.size);
            w32(p + 32, t32(s.atime));
            w32(p + 36, t32(s.mtime));
            w32(p + 40, t32(s.ctime));
        }
        StatKind::S64 => {
            put64(p + 24, s.size);
            put64(p + 32, s.atime as u64);
            put64(p + 40, s.mtime as u64);
            put64(p + 48, s.ctime as u64);
        }
    }
}

fn fstat(c: &mut Ctx, k: StatKind) -> u32 {
    let (fd, p) = (c.a(0), c.a(1));
    if p == 0 {
        set_errno(EINVAL);
        return u32::MAX;
    }
    let info = match get(fd).map(|d| d.kind) {
        Some(Kind::Std(i)) => StatInfo {
            mode: 0x2000 | 0o666,
            dev: i as u32,
            size: 0,
            atime: 0,
            mtime: 0,
            ctime: 0,
        },
        Some(Kind::File(f)) => {
            let Ok(m) = f.f.lock().unwrap().metadata() else { return bad() };
            info_of(&m, &f.path)
        }
        None => return bad(),
    };
    put_stat(p, k, &info);
    0
}

/// Whether _stat may look `path` up at all: never an empty name or wildcards, and a trailing
/// separator only on a root ("X:\\", "\\"): "dir\\" and ".\\" are ENOENT on Windows, and so
/// is a bare drive ("C:", winref crt_io)
fn stat_name_ok(path: &str) -> bool {
    let trimmed = path.trim_end_matches(['\\', '/']);
    let bare_drive = path.len() == 2 && path.ends_with(':');
    !path.is_empty() && !bare_drive && !path.contains(['*', '?']) && (trimmed.len() == path.len() || trimmed.is_empty() || trimmed.ends_with(':'))
}
fn stat(c: &mut Ctx, k: StatKind, wide: bool) -> u32 {
    let (path, p) = (path_arg(c.a(0), wide), c.a(1));
    if p == 0 {
        set_errno(EINVAL);
        return u32::MAX;
    }
    if !stat_name_ok(&path) {
        set_errno(ENOENT);
        return u32::MAX;
    }
    let trimmed = path.trim_end_matches(['\\', '/']);
    let Some((host, m)) = crate::files::resolve(if trimmed.is_empty() || trimmed.ends_with(':') { &path } else { trimmed }).and_then(|h| Some((h.clone(), kura::fs::metadata(h).ok()?))) else {
        set_errno(ENOENT);
        return u32::MAX;
    };
    put_stat(p, k, &info_of(&m, &host));
    0
}

macro_rules! stats {
    ($($f:ident, $s:ident, $w:ident = $k:expr;)*) => { $(
        fn $f(c: &mut Ctx) -> u32 { fstat(c, $k) }
        fn $s(c: &mut Ctx) -> u32 { stat(c, $k, false) }
        fn $w(c: &mut Ctx) -> u32 { stat(c, $k, true) }
    )* };
}
stats! {
    _fstat32, _stat32, _wstat32 = StatKind::S32;
    _fstat32i64, _stat32i64, _wstat32i64 = StatKind::S32i64;
    _fstat64i32, _stat64i32, _wstat64i32 = StatKind::S64i32;
    _fstat64, _stat64, _wstat64 = StatKind::S64;
}
/// msvcrt's _stat / _fstat / _wstat: _stat32 on x86, _stat64i32 on x64; _stati64: _stat32i64 / _stat64
fn legacy_stat() -> StatKind {
    if mem::x64() { StatKind::S64i32 } else { StatKind::S32 }
}
fn legacy_stati64() -> StatKind {
    if mem::x64() { StatKind::S64 } else { StatKind::S32i64 }
}
fn _fstat(c: &mut Ctx) -> u32 {
    fstat(c, legacy_stat())
}
fn _stat(c: &mut Ctx) -> u32 {
    stat(c, legacy_stat(), false)
}
fn _wstat(c: &mut Ctx) -> u32 {
    stat(c, legacy_stat(), true)
}
fn _fstati64(c: &mut Ctx) -> u32 {
    fstat(c, legacy_stati64())
}
fn _stati64(c: &mut Ctx) -> u32 {
    stat(c, legacy_stati64(), false)
}
fn _wstati64(c: &mut Ctx) -> u32 {
    stat(c, legacy_stati64(), true)
}

// ---------------------------------------------------------------------------------------------
// streams <-> descriptors

/// _fileno(stream): the standard streams are 0-2, fopen's have their own descriptor
fn _fileno(c: &mut Ctx) -> u32 {
    match with_file(c.a(0), |s| s.fd) {
        Some(fd) => fd,
        None => {
            set_errno(EINVAL);
            u32::MAX
        }
    }
}
/// _fdopen / _wfdopen (fd, mode): a stream over the descriptor's file
fn fdopen(fd: u32, mode: &[u8]) -> u32 {
    match get(fd).map(|d| d.kind) {
        Some(Kind::File(f)) => new_stream_of(f, fd, mode),
        Some(Kind::Std(i)) => data("_iob").unwrap() + file_size() * i as u32,
        None => {
            set_errno(EBADF);
            0
        }
    }
}
fn _fdopen(c: &mut Ctx) -> u32 {
    fdopen(c.a(0), &cstr(c.a(1)))
}
fn _wfdopen(c: &mut Ctx) -> u32 {
    let mode: Vec<u8> = wstr(c.a(1)).iter().map(|&u| u as u8).collect();
    fdopen(c.a(0), &mode)
}

// ---------------------------------------------------------------------------------------------
// files by name

fn access(c: &mut Ctx, wide: bool) -> u32 {
    // (msvcrt ignores mode bits other than 2: _access(f, 1) answers 0)
    let (path, mode) = (path_arg(c.a(0), wide), c.a(1));
    let Some((host, m)) = crate::files::resolve(&path).and_then(|h| Some((h.clone(), kura::fs::metadata(h).ok()?))) else {
        set_errno(ENOENT);
        return u32::MAX;
    };
    if mode & 2 != 0 && readonly(&m, &host) && !m.is_dir() {
        set_errno(EACCES);
        return u32::MAX;
    }
    0
}
fn _access(c: &mut Ctx) -> u32 {
    access(c, false)
}
fn _waccess(c: &mut Ctx) -> u32 {
    access(c, true)
}
fn unlink(c: &mut Ctx, wide: bool) -> u32 {
    let path = path_arg(c.a(0), wide);
    let Some(h) = crate::files::resolve(&path) else {
        set_errno(ENOENT);
        return u32::MAX;
    };
    if kura::fs::metadata(&h).is_ok_and(|m| m.is_dir() || readonly(&m, &h)) || crate::files::delete_blocked(&h) || kura::fs::remove_file(&h).is_err() {
        set_errno(EACCES);
        return u32::MAX;
    }
    0
}

/// _chmod: only _S_IWRITE (0x80) means anything, as FILE_ATTRIBUTE_READONLY
fn chmod(c: &mut Ctx, wide: bool) -> u32 {
    let path = path_arg(c.a(0), wide);
    let Some(h) = crate::files::resolve(&path).filter(|h| kura::fs::metadata(h).is_ok()) else {
        set_errno(ENOENT);
        return u32::MAX;
    };
    let other = crate::files::extra_attrs(&h) & !1;
    crate::files::set_attrs(&h, other | (c.a(1) & 0x80 == 0) as u32);
    0
}
fn _chmod(c: &mut Ctx) -> u32 {
    chmod(c, false)
}
fn _wchmod(c: &mut Ctx) -> u32 {
    chmod(c, true)
}
/// _rmdir: RemoveDirectory, its error as errno (ENOTEMPTY for a directory with files in it)
fn rmdir(c: &mut Ctx, wide: bool) -> u32 {
    if crate::k32::rmdir(&path_arg(c.a(0), wide)) != 0 {
        return 0;
    }
    set_errno(match r32(crate::cur().teb + crate::teb_last_error()) {
        145 => 41,
        e => errno_for(e),
    });
    u32::MAX
}
fn _rmdir(c: &mut Ctx) -> u32 {
    rmdir(c, false)
}
fn _wrmdir(c: &mut Ctx) -> u32 {
    rmdir(c, true)
}
fn _unlink(c: &mut Ctx) -> u32 {
    unlink(c, false)
}
fn _wunlink(c: &mut Ctx) -> u32 {
    unlink(c, true)
}
fn _wremove(c: &mut Ctx) -> u32 {
    unlink(c, true)
}
fn chdir(c: &mut Ctx, wide: bool) -> u32 {
    let path = crate::files::full_path(&path_arg(c.a(0), wide));
    if crate::files::resolve(&path).is_some_and(|h| crate::files::is_dir(h)) {
        crate::files::set_cwd(&path);
        0
    } else {
        set_errno(ENOENT);
        u32::MAX
    }
}
fn _chdir(c: &mut Ctx) -> u32 {
    chdir(c, false)
}
fn _wchdir(c: &mut Ctx) -> u32 {
    chdir(c, true)
}
/// Copies `s` and a NUL into `buf` of `n` units, or into a new malloc'd block when `buf` is NULL
/// (at least `n` units then); NULL with ERANGE when it does not fit.
fn put_or_alloc<T: Unit>(buf: u32, n: u32, s: &[T]) -> u32 {
    let len = s.len() as u32 + 1;
    let dst = if buf == 0 {
        mem::heap_alloc(unit::<T>() * len.max(n), false)
    } else if n < len {
        set_errno(34);
        return 0;
    } else {
        buf
    };
    put_units(dst, s);
    put_nul::<T>(dst + unit::<T>() * s.len() as u32);
    dst
}
/// The current directory without its trailing separator (except a root's).
fn cwd_string() -> String {
    let d = crate::files::cwd();
    if d.len() > 3 { d.trim_end_matches('\\').to_string() } else { d }
}
fn _getcwd(c: &mut Ctx) -> u32 {
    put_or_alloc::<u8>(c.a(0), c.a(1), &crate::text::s2a(&cwd_string()))
}
fn _wgetcwd(c: &mut Ctx) -> u32 {
    let w: Vec<u16> = cwd_string().encode_utf16().collect();
    put_or_alloc::<u16>(c.a(0), c.a(1), &w)
}
/// _tempnam / _wtempnam (dir, prefix): a malloc'd name of a file not there yet, in TMP when that
/// is a directory, else in `dir`, else in the root, else the current directory, as the CRTs
/// choose; NULL (ENOENT) without one.
fn tempnam(c: &mut Ctx, wide: bool) -> u32 {
    static N: AtomicU32 = AtomicU32::new(1);
    let (dir, pre) = (c.a(0), c.a(1));
    let is_dir = |d: &str| !d.is_empty() && crate::files::resolve(d).and_then(|h| kura::fs::metadata(h).ok()).is_some_and(|m| m.is_dir());
    let arg = if dir != 0 { path_arg(dir, wide) } else { String::new() };
    let tmp = crate::k32::env_get("TMP").unwrap_or_default();
    let Some(d) = [tmp, arg, "\\".to_string(), cwd_string()].into_iter().find(|d| is_dir(d)) else {
        set_errno(ENOENT);
        return 0;
    };
    let d = crate::files::full_path(&d);
    let pre = if pre != 0 { path_arg(pre, wide) } else { String::new() };
    let name = loop {
        let n = format!("{}\\{pre}{}", d.trim_end_matches('\\'), radix32(N.fetch_add(1, Ordering::Relaxed)));
        if crate::files::resolve(&n).is_none() {
            break n;
        }
    };
    if wide {
        let w: Vec<u16> = name.encode_utf16().collect();
        put_or_alloc::<u16>(0, 0, &w)
    } else {
        put_or_alloc::<u8>(0, 0, &crate::text::s2a(&name))
    }
}
/// The CRTs' temporary-name numbering: base 32, digits then lower-case letters
fn radix32(mut n: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b"0123456789abcdefghijklmnopqrstuv"[(n % 32) as usize]);
        n /= 32;
        if n == 0 {
            break;
        }
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}
fn _tempnam(c: &mut Ctx) -> u32 {
    tempnam(c, false)
}
fn _wtempnam(c: &mut Ctx) -> u32 {
    tempnam(c, true)
}
/// _fullpath / _wfullpath (buf, path, n): the absolute path (n is 260 when buf is NULL)
fn _fullpath(c: &mut Ctx) -> u32 {
    let full = crate::files::full_path(&path_arg(c.a(1), false));
    let n = if c.a(0) == 0 { 0 } else { c.a(2) };
    put_or_alloc::<u8>(c.a(0), n, &crate::text::s2a(&full))
}
fn _wfullpath(c: &mut Ctx) -> u32 {
    let full: Vec<u16> = crate::files::full_path(&path_arg(c.a(1), true)).encode_utf16().collect();
    let n = if c.a(0) == 0 { 0 } else { c.a(2) };
    put_or_alloc::<u16>(c.a(0), n, &full)
}
/// _utime / _utime64 / _wutime64 (path, times): file times are the host's; 0 when it exists
fn utime(c: &mut Ctx, wide: bool) -> u32 {
    if crate::files::resolve(&path_arg(c.a(0), wide)).is_some() {
        0
    } else {
        set_errno(ENOENT);
        u32::MAX
    }
}
fn _utime64(c: &mut Ctx) -> u32 {
    utime(c, false)
}
fn _wutime64(c: &mut Ctx) -> u32 {
    utime(c, true)
}

// ---------------------------------------------------------------------------------------------
// environment

/// The CRT's own copy of the environment, as Windows' CRT keeps one: taken from the process's on
/// first use, changed by _putenv (which sets the process's too) but not by SetEnvironmentVariable,
/// so getenv does not see what a program (Python's os.environ, say) set through kernel32 only.
static CRT_ENV: Mutex<Option<Vec<(String, String)>>> = Mutex::new(None);
fn with_crt_env<R>(f: impl FnOnce(&mut Vec<(String, String)>) -> R) -> R {
    let mut g = CRT_ENV.lock().unwrap();
    f(g.get_or_insert_with(crate::k32::env_vars))
}
pub(super) fn env_get(name: &str) -> Option<String> {
    let key = name.to_ascii_uppercase();
    with_crt_env(|e| e.iter().find(|(k, _)| k.to_ascii_uppercase() == key).map(|(_, v)| v.clone()))
}
fn env_put(name: &str, value: Option<&str>) {
    let key = name.to_ascii_uppercase();
    with_crt_env(|e| {
        let at = e.iter().position(|(k, _)| k.to_ascii_uppercase() == key);
        match (at, value) {
            (Some(i), Some(v)) => e[i].1 = v.to_string(),
            (None, Some(v)) => e.push((name.to_string(), v.to_string())),
            (Some(i), None) => {
                e.remove(i);
            }
            (None, None) => {}
        }
    });
    crate::k32::env_set(name, value);
}

/// The strings of the environment, NAME=value, in a NULL-terminated guest array.
fn env_array(wide: bool) -> u32 {
    let words: Vec<Vec<u8>> = with_crt_env(|e| e.clone())
        .into_iter()
        .map(|(k, v)| {
            let s = format!("{k}={v}");
            if wide { s.encode_utf16().flat_map(u16::to_le_bytes).collect() } else { crate::text::s2a(&s) }
        })
        .collect();
    string_array(words, if wide { 2 } else { 1 })
}
/// _environ / _wenviron: made again after each change (the CRT's are kept up to date as well)
pub(super) fn refresh_environ() {
    for (name, wide) in [("_environ", false), ("_wenviron", true)] {
        let var = data(name).unwrap();
        wp(var, env_array(wide));
    }
}
pub(super) fn environ(wide: bool) -> u32 {
    env_array(wide)
}

/// _wgetenv(name): the value, kept in a guest buffer per name (as getenv's)
fn _wgetenv(c: &mut Ctx) -> u32 {
    static VALS: Mutex<Option<HashMap<String, u32>>> = Mutex::new(None);
    let name = path_arg(c.a(0), true);
    let Some(v) = env_get(&name) else { return 0 };
    let w: Vec<u16> = v.encode_utf16().collect();
    let mut g = VALS.lock().unwrap();
    let m = g.get_or_insert_with(HashMap::default);
    let key = name.to_ascii_uppercase();
    if let Some(&p) = m.get(&key) {
        mem::heap_free(p);
    }
    let p = put_or_alloc::<u16>(0, 0, &w);
    m.insert(key, p);
    p
}
/// _wgetenv_s / getenv_s (&len, buf, n, name)
fn getenv_s(c: &mut Ctx, wide: bool) -> u32 {
    let (plen, buf, n, name) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let v = env_get(&path_arg(name, wide));
    let units: Vec<u16> = v.as_deref().map_or_else(Vec::new, |v| {
        if wide {
            v.encode_utf16().collect()
        } else {
            crate::text::s2a(v).into_iter().map(u16::from).collect()
        }
    });
    let need = if v.is_some() { units.len() as u32 + 1 } else { 0 };
    putp(plen, need);
    if v.is_none() {
        if buf != 0 && n > 0 {
            if wide { w16(buf, 0) } else { w8(buf, 0) }
        }
        return 0;
    }
    if buf == 0 || n < need {
        return if buf == 0 && n == 0 { 0 } else { 34 };
    }
    if wide {
        put_or_alloc::<u16>(buf, n, &units);
    } else {
        let b: Vec<u8> = units.iter().map(|&u| u as u8).collect();
        put_or_alloc::<u8>(buf, n, &b);
    }
    0
}
fn getenv_s_a(c: &mut Ctx) -> u32 {
    getenv_s(c, false)
}
fn _wgetenv_s(c: &mut Ctx) -> u32 {
    getenv_s(c, true)
}
/// Sets NAME (empty value: removes it), as _putenv does; 0 or -1 (EINVAL)
fn put_env(name: &str, value: &str) -> u32 {
    if name.is_empty() || name.contains('=') {
        set_errno(EINVAL);
        return u32::MAX;
    }
    env_put(name, if value.is_empty() { None } else { Some(value) });
    refresh_environ();
    0
}
/// _putenv / _wputenv ("NAME=value")
fn putenv(c: &mut Ctx, wide: bool) -> u32 {
    let s = path_arg(c.a(0), wide);
    // (a name may start with '=': the per-drive directories)
    let Some(i) = s.char_indices().skip(1).find(|&(_, ch)| ch == '=').map(|(i, _)| i) else {
        set_errno(EINVAL);
        return u32::MAX;
    };
    put_env(&s[..i], &s[i + 1..])
}
pub(super) fn _putenv(c: &mut Ctx) -> u32 {
    putenv(c, false)
}
fn _wputenv(c: &mut Ctx) -> u32 {
    putenv(c, true)
}
/// _putenv_s / _wputenv_s (name, value): errno
fn putenv_s(c: &mut Ctx, wide: bool) -> u32 {
    if c.a(0) == 0 || c.a(1) == 0 {
        set_errno(EINVAL);
        return EINVAL;
    }
    if put_env(&path_arg(c.a(0), wide), &path_arg(c.a(1), wide)) == 0 { 0 } else { EINVAL }
}
fn _putenv_s(c: &mut Ctx) -> u32 {
    putenv_s(c, false)
}
fn _wputenv_s(c: &mut Ctx) -> u32 {
    putenv_s(c, true)
}

// ---------------------------------------------------------------------------------------------
// time zone: kernel32's (GetTimeZoneInformation, localtime: k32::tz_bias, Tokyo when the code
// page is 932, else UTC), as msvcrt's _tzset takes it from there: _timezone is UTC minus local
// time in seconds (-32400 in Tokyo) and _tzname the zone's names in the ANSI code page. Games
// check them: Triangle's 妄想ぷろとこる！ quits "Japan only" unless _timezone is -32400.

/// _timezone: seconds to add to local time for UTC
pub(super) fn timezone() -> i32 {
    -(crate::k32::tz_bias() as i32)
}
/// the zone's standard and daylight names, as GetTimeZoneInformation's in the ANSI code page
fn tz_names() -> [Vec<u8>; 2] {
    if crate::k32::tz_bias() == 9 * 3600 {
        ["東京 (標準時)", "東京 (夏時間)"].map(|n| crate::text::s2a(n))
    } else {
        [b"UTC".to_vec(), b"UTC".to_vec()]
    }
}

fn _tzset(_: &mut Ctx) -> u32 {
    0
}
fn __timezone(_: &mut Ctx) -> u32 {
    data("_timezone").unwrap()
}
fn __daylight(_: &mut Ctx) -> u32 {
    data("_daylight").unwrap()
}
fn __dstbias(_: &mut Ctx) -> u32 {
    data("_dstbias").unwrap()
}
fn __tzname(_: &mut Ctx) -> u32 {
    data("_tzname").unwrap()
}
pub(super) fn tzname_array() -> u32 {
    // (the CRT's two buffers are 64 bytes each: programs may write their own names there)
    let arr = mem::heap_alloc(2 * ps(), true);
    for (i, w) in tz_names().iter().enumerate() {
        let s = mem::heap_alloc(64, true);
        mem::write(s, w);
        wp(arr + ps() * i as u32, s);
    }
    arr
}
/// _get_tzname(&len, buf, n, index): len counts the NUL
fn _get_tzname(c: &mut Ctx) -> u32 {
    let (plen, buf, n, i) = (c.a(0), c.a(1), c.a(2), c.a(3));
    if plen == 0 || i > 1 || (buf == 0 && n != 0) {
        set_errno(EINVAL);
        return EINVAL;
    }
    let mut name = tz_names()[i as usize].clone();
    name.push(0);
    putp(plen, name.len() as u32);
    if buf == 0 {
        return 0;
    }
    if (n as usize) < name.len() {
        return 34;
    }
    mem::write(buf, &name);
    0
}
fn _get_timezone(c: &mut Ctx) -> u32 {
    w32(c.a(0), timezone() as u32);
    0
}
fn _get_daylight(c: &mut Ctx) -> u32 {
    w32(c.a(0), 0);
    0
}
fn _get_dstbias(c: &mut Ctx) -> u32 {
    w32(c.a(0), 0);
    0
}

// ---------------------------------------------------------------------------------------------
// the locale and multibyte conversion

/// setlocale's LC_CTYPE: 0 for the "C" locale (each byte is the character of its value, as
/// Windows' CRTs start), else the code page of the locale set (setlocale(LC_ALL, ""): the ANSI one)
static LC_CP: AtomicU32 = AtomicU32::new(0);
/// each category's locale name (LC_COLLATE..LC_TIME), None for "C"
static LC_NAMES: Mutex<[Option<String>; 5]> = Mutex::new([None, None, None, None, None]);

/// LC_CTYPE's code page, 0 for "C" (___lc_codepage_func)
pub(super) fn lc_cp() -> u32 {
    LC_CP.load(Ordering::Relaxed)
}
/// The locale a setlocale string names: (name, code page), "C" as code page 0; None if unknown
fn locale_of(s: &str) -> Option<(String, u32)> {
    let acp = crate::text::acp();
    let named = |cp: u32| match cp {
        932 => Some(("Japanese_Japan.932".to_string(), 932)),
        1252 => Some(("English_United States.1252".to_string(), 1252)),
        _ => None,
    };
    let l = s.to_ascii_lowercase();
    match l.as_str() {
        "c" => Some(("C".to_string(), 0)),
        "" | ".acp" => named(acp),
        ".932" | "japanese" | "japanese_japan" | "japanese_japan.932" | "jpn" | "ja-jp" | "ja" | "japan" => named(932),
        ".1252" | "english" | "english_united states" | "english_united states.1252" | "american" | "en-us" | "us" => named(1252),
        _ => None,
    }
}

/// setlocale's answer for `cat`: a name, LC_ALL's a composite one when the categories differ
fn locale_name(cat: u32) -> String {
    let n = LC_NAMES.lock().unwrap();
    let one = |i: usize| n[i].clone().unwrap_or_else(|| "C".to_string());
    if cat != 0 {
        return one(cat as usize - 1);
    }
    if n.iter().all(|x| *x == n[0]) {
        return one(0);
    }
    ["LC_COLLATE", "LC_CTYPE", "LC_MONETARY", "LC_NUMERIC", "LC_TIME"]
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{k}={}", one(i)))
        .collect::<Vec<_>>()
        .join(";")
}

/// setlocale(category, locale) / _wsetlocale: the locale's name in a static buffer per category,
/// NULL (unchanged) for a locale it does not know
fn set_locale(c: &mut Ctx, wide: bool) -> u32 {
    static BUF: Mutex<[u32; 6]> = Mutex::new([0; 6]);
    let (cat, p) = (c.a(0), c.a(1));
    if cat > 5 {
        set_errno(EINVAL);
        return 0;
    }
    if p != 0 {
        let Some((name, cp)) = locale_of(&path_arg(p, wide)) else { return 0 };
        let mut n = LC_NAMES.lock().unwrap();
        for i in 0..5 {
            if cat == 0 || cat as usize == i + 1 {
                n[i] = (cp != 0).then(|| name.clone());
            }
        }
        if cat == 0 || cat == 2 {
            LC_CP.store(cp, Ordering::Relaxed);
            let dbcs = cp != 0 && matches!(cp, 932 | 936 | 949 | 950);
            w32(super::data("__mb_cur_max").unwrap(), if dbcs { 2 } else { 1 });
        }
    }
    let name = locale_name(cat);
    let units: Vec<u8> = if wide {
        name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
    } else {
        name.bytes().chain([0]).collect()
    };
    let mut b = BUF.lock().unwrap();
    let slot = &mut b[cat as usize];
    if *slot != 0 {
        mem::heap_free(*slot);
    }
    *slot = mem::heap_alloc(units.len() as u32, true);
    mem::write(*slot, &units);
    *slot
}
fn setlocale(c: &mut Ctx) -> u32 {
    set_locale(c, false)
}
fn _wsetlocale(c: &mut Ctx) -> u32 {
    set_locale(c, true)
}

/// a lead byte of the locale's code page (none in "C")
fn lead(b: u8) -> bool {
    lead_in(lc_cp(), b)
}
/// bytes of the locale as UTF-16 ("C": each byte its value)
fn to_wide(b: &[u8]) -> Vec<u16> {
    to_wide_in(lc_cp(), b)
}
/// a character's bytes in the locale, None when it has none
fn wc_bytes(u: u16) -> Option<Vec<u8>> {
    wc_bytes_in(lc_cp(), u)
}
/// lead() in code page `cp` (0: "C")
fn lead_in(cp: u32, b: u8) -> bool {
    cp != 0 && crate::text::is_lead_cp(cp, b)
}
/// to_wide() in code page `cp` (0: "C", each byte its value)
fn to_wide_in(cp: u32, b: &[u8]) -> Vec<u16> {
    if cp == 0 { b.iter().map(|&x| x as u16).collect() } else { crate::text::mb2w(cp, b) }
}
/// wc_bytes() in code page `cp`: "C" has the characters up to 0xff; a code page lacks what
/// conversion has to make its default character
fn wc_bytes_in(cp: u32, u: u16) -> Option<Vec<u8>> {
    if cp == 0 {
        return (u <= 0xff).then(|| vec![u as u8]);
    }
    let (b, used) = crate::text::w2mb_def(cp, &[u], b"?");
    (!used && !b.is_empty()).then_some(b)
}

/// One character from at most `n` bytes at `s`: (UTF-16 unit, bytes used); 0 bytes for NUL, None
/// for an incomplete or invalid sequence
fn mb_char(s: u32, n: u32) -> Option<(u16, u32)> {
    if n == 0 {
        return None;
    }
    let b = r8(s);
    if b == 0 {
        return Some((0, 0));
    }
    if lead(b) {
        if n < 2 || r8(s + 1) == 0 {
            return None;
        }
        return to_wide(&[b, r8(s + 1)]).first().map(|&u| (u, 2));
    }
    to_wide(&[b]).first().map(|&u| (u, 1))
}
/// mbtowc / _mbtowc_l (pwc, s, n): bytes used, 0 for NUL or a NULL s, -1 (EILSEQ)
fn _mbtowc_l(c: &mut Ctx) -> u32 {
    let (pwc, s, n) = (c.a(0), c.a(1), c.a(2));
    if s == 0 {
        return 0;
    }
    match mb_char(s, n) {
        Some((u, used)) => {
            if pwc != 0 {
                w16(pwc, u);
            }
            used
        }
        None => {
            set_errno(42);
            u32::MAX
        }
    }
}
/// mbrtowc(pwc, s, n, state): as mbtowc, -2 for an incomplete character
fn mbrtowc(c: &mut Ctx) -> u32 {
    let (pwc, s, n) = (c.a(0), c.a(1), c.a(2));
    if s == 0 {
        return 0;
    }
    match mb_char(s, n) {
        Some((u, used)) => {
            if pwc != 0 {
                w16(pwc, u);
            }
            used
        }
        None if n == 1 && lead(r8(s)) => u32::MAX - 1,
        None => {
            set_errno(42);
            u32::MAX
        }
    }
}
fn mbrlen(c: &mut Ctx) -> u32 {
    let (s, n) = (c.a(0), c.a(1));
    if s == 0 {
        return 0;
    }
    match mb_char(s, n) {
        Some((_, used)) => used,
        None if n == 1 && lead(r8(s)) => u32::MAX - 1,
        None => {
            set_errno(42);
            u32::MAX
        }
    }
}
/// btowc(c): the wide character of a single byte, WEOF for a lead byte or EOF
fn btowc(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if v > 255 || lead(v as u8) {
        return 0xffff;
    }
    // (a byte that is no character alone, as 0x82 or 0xA0 in 932, is WEOF: winref crt_io)
    let cp = lc_cp();
    if cp == 0 {
        return v;
    }
    match crate::text::mb2w_checked(cp, &[v as u8]) {
        (w, false) if w.len() == 1 => w[0] as u32,
        _ => 0xffff,
    }
}
/// wctob(wc): the single byte of a wide character, EOF when it takes two or none
fn wctob(c: &mut Ctx) -> u32 {
    match wc_bytes(c.a(0) as u16) {
        Some(b) if b.len() == 1 => b[0] as u32,
        _ => u32::MAX,
    }
}
/// wcrtomb(s, wc, state): bytes written, -1 (EILSEQ) for a character the locale lacks
fn wcrtomb(c: &mut Ctx) -> u32 {
    let (s, u) = (c.a(0), c.a(1) as u16);
    if s == 0 {
        return 1;
    }
    let Some(b) = (if u == 0 { Some(vec![0]) } else { wc_bytes(u) }) else {
        set_errno(42);
        return u32::MAX;
    };
    mem::write(s, &b);
    b.len() as u32
}
/// wcrtomb_s(&ret, s, size, wc, state)
fn wcrtomb_s(c: &mut Ctx) -> u32 {
    let (pret, s, size, u) = (c.a(0), c.a(1), c.a(2), c.a(3) as u16);
    let Some(b) = (if u == 0 { Some(vec![0]) } else { wc_bytes(u) }) else {
        putp(pret, u32::MAX);
        set_errno(42);
        return 42;
    };
    if s != 0 {
        if (size as usize) < b.len() {
            return 34;
        }
        mem::write(s, &b);
    }
    putp(pret, b.len() as u32);
    0
}
/// mbstowcs(dst, src, n): units converted (dst NULL: the length), -1 for an invalid sequence
fn mbstowcs(c: &mut Ctx) -> u32 {
    let (dst, src, n) = (c.a(0), c.a(1), c.a(2));
    let w = to_wide(&cstr(src));
    if dst == 0 {
        return w.len() as u32;
    }
    let k = w.len().min(n as usize);
    for (i, &u) in w[..k].iter().enumerate() {
        w16(dst + 2 * i as u32, u);
    }
    if k < n as usize {
        w16(dst + 2 * k as u32, 0);
    }
    k as u32
}
/// mbsrtowcs(dst, &src, n, state): as mbstowcs, *src moved past what was converted (NULL at the end)
fn mbsrtowcs(c: &mut Ctx) -> u32 {
    let (dst, psrc, n) = (c.a(0), c.a(1), c.a(2));
    let src = rp(psrc);
    let bytes = cstr(src);
    let w = to_wide(&bytes);
    if dst == 0 {
        return w.len() as u32;
    }
    let k = w.len().min(n as usize);
    for (i, &u) in w[..k].iter().enumerate() {
        w16(dst + 2 * i as u32, u);
    }
    if k < n as usize {
        w16(dst + 2 * k as u32, 0);
        putp(psrc, 0);
    } else {
        let used: usize = w[..k].iter().map(|&u| wc_bytes(u).map_or(1, |b| b.len())).sum();
        putp(psrc, src + used as u32);
    }
    k as u32
}
/// wcstombs(dst, src, n): bytes converted (dst NULL: the length), -1 (EILSEQ) for a character
/// the locale lacks
fn wcstombs(c: &mut Ctx) -> u32 {
    let (dst, src, n) = (c.a(0), c.a(1), c.a(2));
    let mut k = 0usize;
    for ch in wstr(src) {
        let Some(cb) = wc_bytes(ch) else {
            // (counting does not fail: Windows' msvcrt counts such a character as one byte,
            // winref trialapis)
            if dst == 0 {
                k += 1;
                continue;
            }
            set_errno(42);
            return u32::MAX;
        };
        // (never half a double-byte character)
        if dst != 0 && k + cb.len() > n as usize {
            break;
        }
        if dst != 0 {
            mem::write(dst + k as u32, &cb);
        }
        k += cb.len();
    }
    if dst != 0 && k < n as usize {
        w8(dst + k as u32, 0);
    }
    k as u32
}

// ---------------------------------------------------------------------------------------------
// conio (no console: input is at its end, output goes to the log)

fn conio_eof(_: &mut Ctx) -> u32 {
    u32::MAX
}
fn conio_weof(_: &mut Ctx) -> u32 {
    0xffff
}
fn _kbhit(_: &mut Ctx) -> u32 {
    0
}
fn _putch(c: &mut Ctx) -> u32 {
    crate::k32::console_write(&a2s(&[c.a(0) as u8]));
    c.a(0) & 0xff
}
fn _putwch(c: &mut Ctx) -> u32 {
    crate::k32::console_write(&String::from_utf16_lossy(&[c.a(0) as u16]));
    c.a(0) & 0xffff
}

// ---------------------------------------------------------------------------------------------
// processes: none can be started

fn _getpid(_: &mut Ctx) -> u32 {
    crate::pid()
}
fn no_process(_: &mut Ctx) -> u32 {
    set_errno(ENOENT);
    u32::MAX
}
fn _popen(_: &mut Ctx) -> u32 {
    set_errno(ENOENT);
    0
}

// ---------------------------------------------------------------------------------------------
// misc

fn _set_invalid_parameter_handler(c: &mut Ctx) -> u32 {
    static H: Mutex<u32> = Mutex::new(0);
    std::mem::replace(&mut *H.lock().unwrap(), c.a(0))
}
fn _get_invalid_parameter_handler(_: &mut Ctx) -> u32 {
    0
}
/// _assert / _wassert (message, file, line)
fn assert_fail(c: &mut Ctx, wide: bool) -> u32 {
    eprintln!("sake: Assertion failed: {}, file {}, line {}", path_arg(c.a(0), wide), path_arg(c.a(1), wide), c.a(2));
    unsafe { libc::exit(3) }
}
fn _assert(c: &mut Ctx) -> u32 {
    assert_fail(c, false)
}
fn _wassert(c: &mut Ctx) -> u32 {
    assert_fail(c, true)
}
/// raise(sig): SIGABRT and SIGTERM end the program (no handlers are kept), others do nothing
fn raise(c: &mut Ctx) -> u32 {
    match c.a(0) {
        22 | 15 | 2 => {
            eprintln!("sake: raise({})", c.a(0));
            unsafe { libc::exit(3) }
        }
        2..=23 => 0,
        _ => {
            set_errno(EINVAL);
            u32::MAX
        }
    }
}
/// The UCRT's messages for errno values 0..=42 (and "Unknown error" past them)
const MESSAGES: [&str; 43] = [
    "No error",
    "Operation not permitted",
    "No such file or directory",
    "No such process",
    "Interrupted function call",
    "Input/output error",
    "No such device or address",
    "Arg list too long",
    "Exec format error",
    "Bad file descriptor",
    "No child processes",
    "Resource temporarily unavailable",
    "Not enough space",
    "Permission denied",
    "Bad address",
    "Unknown error",
    "Resource device",
    "File exists",
    "Improper link",
    "No such device",
    "Not a directory",
    "Is a directory",
    "Invalid argument",
    "Too many open files in system",
    "Too many open files",
    "Inappropriate I/O control operation",
    "Unknown error",
    "File too large",
    "No space left on device",
    "Invalid seek",
    "Read-only file system",
    "Too many links",
    "Broken pipe",
    "Domain error",
    "Result too large",
    "Unknown error",
    "Resource deadlock avoided",
    "Unknown error",
    "Filename too long",
    "No locks available",
    "Function not implemented",
    "Directory not empty",
    "Illegal byte sequence",
];
pub(super) fn message(e: u32) -> &'static str {
    MESSAGES.get(e as usize).copied().unwrap_or("Unknown error")
}
/// strerror(errnum) / _wcserror(errnum): the message, in one buffer per thread (and per kind)
/// that the next call overwrites, as Windows: strerror(2) and strerror(200) evaluated for one
/// printf both show the later message
fn errmsg(e: u32, wide: bool) -> u32 {
    static BUFS: Mutex<Vec<(u32, bool, u32)>> = Mutex::new(Vec::new());
    let tid = crate::cur().tid;
    let p = {
        let mut b = BUFS.lock().unwrap();
        match b.iter().find(|x| x.0 == tid && x.1 == wide) {
            Some(&(_, _, p)) => p,
            None => {
                let p = mem::heap_alloc(256, true);
                b.push((tid, wide, p));
                p
            }
        }
    };
    let m = message(e);
    let bytes: Vec<u8> = if wide { m.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect() } else { m.bytes().chain([0]).collect() };
    mem::write(p, &bytes);
    p
}
fn strerror(c: &mut Ctx) -> u32 {
    errmsg(c.a(0), false)
}
fn _wcserror(c: &mut Ctx) -> u32 {
    errmsg(c.a(0), true)
}
/// strerror_s(buf, n, errnum)
fn strerror_s(c: &mut Ctx) -> u32 {
    let (buf, n, e) = (c.a(0), c.a(1), c.a(2));
    if buf == 0 || n == 0 {
        return EINVAL;
    }
    let m = message(e).as_bytes();
    let k = m.len().min(n as usize - 1);
    mem::write(buf, &m[..k]);
    w8(buf + k as u32, 0);
    0
}
/// _wcserror_s(buf, n, errnum)
fn _wcserror_s(c: &mut Ctx) -> u32 {
    let (buf, n, e) = (c.a(0), c.a(1), c.a(2));
    if buf == 0 || n == 0 {
        return EINVAL;
    }
    let m: Vec<u16> = message(e).encode_utf16().take(n as usize - 1).collect();
    put_units(buf, &m);
    w16(buf + 2 * m.len() as u32, 0);
    0
}
fn __sys_errlist(_: &mut Ctx) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| string_array(MESSAGES.iter().map(|m| m.as_bytes().to_vec()).collect(), 1))
}
fn __sys_nerr(_: &mut Ctx) -> u32 {
    static P: OnceLock<u32> = OnceLock::new();
    *P.get_or_init(|| {
        let p = mem::heap_alloc(4, true);
        w32(p, MESSAGES.len() as u32);
        p
    })
}
/// perror(s): "s: message" to the log
fn perror(c: &mut Ctx) -> u32 {
    let e = r32(super::_errno(c));
    let s = if c.a(0) != 0 { a2s(&cstr(c.a(0))) } else { String::new() };
    let line = if s.is_empty() { format!("{}\n", message(e)) } else { format!("{s}: {}\n", message(e)) };
    crate::k32::console_write(&line);
    0
}
/// rand_s(&v): random bits from the host
fn rand_s(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if p == 0 {
        return EINVAL;
    }
    let mut b = [0u8; 4];
    let _ = getrandom_bytes(&mut b);
    w32(p, u32::from_le_bytes(b));
    0
}
fn getrandom_bytes(b: &mut [u8]) -> bool {
    use std::io::Read;
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(b)).is_ok() || {
        // (no /dev/urandom: the clock, mixed)
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        let mut x = t ^ 0x9e37_79b9_7f4a_7c15;
        for v in b.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *v = x as u8;
        }
        true
    }
}
fn zero(_: &mut Ctx) -> u32 {
    0
}
fn _amsg_exit(c: &mut Ctx) -> u32 {
    eprintln!("sake: _amsg_exit({})", c.a(0));
    unsafe { libc::exit(255) }
}
fn isblank(c: &mut Ctx) -> u32 {
    matches!(c.a(0), 9 | 32) as u32
}
fn iswblank(c: &mut Ctx) -> u32 {
    matches!(c.a(0) & 0xffff, 9 | 32 | 0x3000) as u32
}
fn __isascii(c: &mut Ctx) -> u32 {
    (c.a(0) < 0x80) as u32
}
/// iswctype(wc, mask) / _isctype_l: the ctype classes (_UPPER 1, _LOWER 2, _DIGIT 4, _SPACE 8,
/// _PUNCT 0x10, _CONTROL 0x20, _BLANK 0x40, _HEX 0x80, _ALPHA 0x103)
pub(super) fn wctype_bits(u: u32) -> u32 {
    let Some(ch) = char::from_u32(u) else { return 0 };
    let mut b = 0;
    if ch.is_uppercase() {
        b |= 1;
    }
    if ch.is_lowercase() {
        b |= 2;
    }
    if ch.is_ascii_digit() {
        b |= 4;
    }
    if ch.is_whitespace() {
        b |= 8;
    }
    if ch.is_ascii_punctuation() || (!ch.is_alphanumeric() && !ch.is_whitespace() && !ch.is_control() && u >= 0x80) {
        b |= 0x10;
    }
    if ch.is_control() {
        b |= 0x20;
    }
    if u == 9 || u == 32 || u == 0x3000 {
        b |= 0x40;
    }
    if ch.is_ascii_hexdigit() {
        b |= 0x80;
    }
    if ch.is_alphabetic() {
        b |= 0x100;
    }
    b
}
fn iswctype(c: &mut Ctx) -> u32 {
    wctype_bits(c.a(0) & 0xffff) & (c.a(1) & 0xffff)
}
fn _isctype_l(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if v > 255 {
        return 0;
    }
    (r16(ctype_table() + 2 + 2 * v) as u32) & c.a(1)
}
fn _isctype(c: &mut Ctx) -> u32 {
    _isctype_l(c)
}
macro_rules! wclass_l {
    ($($name:ident = $mask:expr),* $(,)?) => { $( fn $name(c: &mut Ctx) -> u32 { wctype_bits(c.a(0) & 0xffff) & $mask } )* };
}
wclass_l!(
    _iswalpha_l = 0x103,
    _iswcntrl_l = 0x20,
    _iswdigit_l = 4,
    _iswlower_l = 2,
    _iswprint_l = 0x157,
    _iswpunct_l = 0x10,
    _iswspace_l = 8,
    _iswupper_l = 1,
    _iswxdigit_l = 0x80,
    _iswalnum_l = 0x107,
    _iswgraph_l = 0x117
);
/// wcscoll / _wcscoll_l / strcoll's _l form: by code (the "C" locale's order)
fn wcscoll(c: &mut Ctx) -> u32 {
    wstr(c.a(0)).cmp(&wstr(c.a(1))) as i32 as u32
}
fn _strcoll_l(c: &mut Ctx) -> u32 {
    cstr(c.a(0)).cmp(&cstr(c.a(1))) as i32 as u32
}
/// strxfrm / wcsxfrm (dst, src, n) and the _l forms: a copy (the "C" locale); the length
fn _strxfrm_l(c: &mut Ctx) -> u32 {
    let (dst, src, n) = (c.a(0), c.a(1), c.a(2));
    let s = cstr(src);
    if (s.len() as u32) < n {
        mem::write(dst, &s);
        w8(dst + s.len() as u32, 0);
    }
    s.len() as u32
}
fn wcsxfrm(c: &mut Ctx) -> u32 {
    let (dst, src, n) = (c.a(0), c.a(1), c.a(2));
    let s = wstr(src);
    if (s.len() as u32) < n {
        put_units(dst, &s);
        w16(dst + 2 * s.len() as u32, 0);
    }
    s.len() as u32
}
fn _tolower_l(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if (b'A' as u32..=b'Z' as u32).contains(&v) { v + 32 } else { v }
}
fn _toupper_l(c: &mut Ctx) -> u32 {
    let v = c.a(0);
    if (b'a' as u32..=b'z' as u32).contains(&v) { v - 32 } else { v }
}
fn setbuf(_: &mut Ctx) -> u32 {
    0
}
fn clearerr(c: &mut Ctx) -> u32 {
    with_file(c.a(0), |s| s.set_eof(false));
    0
}
/// fputwc(wc, stream): the character's bytes in the locale, WEOF (EILSEQ) when it has none
fn fputwc(c: &mut Ctx) -> u32 {
    let u = c.a(0) as u16;
    let Some(b) = wc_bytes(u) else {
        set_errno(42);
        return 0xffff;
    };
    write_stream(c.a(1), &b);
    u as u32
}
/// fputws(s, stream): as fputwc for each character, EOF at one the locale lacks
fn fputws(c: &mut Ctx) -> u32 {
    for u in wstr(c.a(0)) {
        let Some(b) = wc_bytes(u) else {
            set_errno(42);
            return u32::MAX;
        };
        write_stream(c.a(1), &b);
    }
    0
}
/// fgetwc(stream): one character from the locale's bytes (as mbtowc); WEOF at the end
fn fgetwc(c: &mut Ctx) -> u32 {
    let fp = c.a(0);
    let b = getc_of(fp);
    if b == u32::MAX {
        return 0xffff;
    }
    let mut bytes = vec![b as u8];
    if lead(b as u8) {
        let t = getc_of(fp);
        if t != u32::MAX {
            bytes.push(t as u8);
        }
    }
    to_wide(&bytes).first().map_or(0xffff, |&u| u as u32)
}
/// ungetwc(wc, stream): a character below 0x80 goes back (others are not kept)
fn ungetwc(c: &mut Ctx) -> u32 {
    let (u, fp) = (c.a(0) & 0xffff, c.a(1));
    if u == 0xffff || u >= 0x80 {
        return 0xffff;
    }
    with_file(fp, |s| {
        s.unget = Some(u as u8);
        s.set_eof(false);
        u
    })
    .unwrap_or(0xffff)
}
fn getchar(_: &mut Ctx) -> u32 {
    u32::MAX
}
fn putc(c: &mut Ctx) -> u32 {
    let ch = c.a(0) & 0xff;
    write_stream(c.a(1), &[ch as u8]);
    ch
}
fn wprintf(c: &mut Ctx) -> u32 {
    let Some(s) = legacy::<u16>(c.a(0), &mut VaArgs::arg(c, 1)) else { return u32::MAX };
    crate::k32::console_write(&String::from_utf16_lossy(&s));
    s.len() as u32
}
// math the UCRT has and Rust's std does not: libm's
fn erf(c: &mut Ctx) -> u32 {
    let x = c.f64(0);
    ret(c, libm::erf(x))
}
fn erfc(c: &mut Ctx) -> u32 {
    let x = c.f64(0);
    ret(c, libm::erfc(x))
}
fn erff(c: &mut Ctx) -> u32 {
    let x = Args(0).f32(c);
    ret_f32(c, libm::erff(x))
}
fn erfcf(c: &mut Ctx) -> u32 {
    let x = Args(0).f32(c);
    ret_f32(c, libm::erfcf(x))
}
fn tgamma(c: &mut Ctx) -> u32 {
    let x = c.f64(0);
    ret(c, libm::tgamma(x))
}
fn lgamma(c: &mut Ctx) -> u32 {
    let x = c.f64(0);
    ret(c, libm::lgamma(x))
}
fn fma(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, y, z) = (a.f64(c), a.f64(c), a.f64(c));
    ret(c, x.mul_add(y, z))
}
fn fmaf(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, y, z) = (a.f32(c), a.f32(c), a.f32(c));
    ret_f32(c, x.mul_add(y, z))
}
fn nextafter(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, y) = (a.f64(c), a.f64(c));
    ret(c, libm::nextafter(x, y))
}
fn nextafterf(c: &mut Ctx) -> u32 {
    let mut a = Args(0);
    let (x, y) = (a.f32(c), a.f32(c));
    ret_f32(c, libm::nextafterf(x, y))
}
/// rint / nearbyint: to nearest, ties to even (the default rounding mode)
fn rint(c: &mut Ctx) -> u32 {
    let x = c.f64(0);
    ret(c, x.round_ties_even())
}
fn rintf(c: &mut Ctx) -> u32 {
    let x = Args(0).f32(c);
    ret_f32(c, x.round_ties_even())
}
fn lrint(c: &mut Ctx) -> u32 {
    let x = c.f64(0).round_ties_even();
    if x >= -2147483648.0 && x < 2147483648.0 { x as i32 as u32 } else { 0x8000_0000 }
}
fn lrintf(c: &mut Ctx) -> u32 {
    let x = Args(0).f32(c).round_ties_even();
    if x >= -2147483648.0 && x < 2147483648.0 { x as i32 as u32 } else { 0x8000_0000 }
}
fn llrint(c: &mut Ctx) -> u32 {
    let x = c.f64(0).round_ties_even();
    let v = if x >= -9.223372036854776e18 && x < 9.223372036854776e18 { x as i64 } else { i64::MIN };
    c.ret64(v as u64)
}
fn lroundf(c: &mut Ctx) -> u32 {
    let x = Args(0).f32(c).round();
    if x >= -2147483648.0 && x < 2147483648.0 { x as i32 as u32 } else { 0x8000_0000 }
}

pub(super) fn init() {
    reg!(
        _open,
        _wopen,
        _sopen,
        _wsopen,
        _sopen_s,
        _wsopen_s,
        _close,
        _read,
        _write,
        _lseek,
        _lseeki64,
        _tell,
        _telli64,
        _filelength,
        _filelengthi64,
        _eof,
        _chsize,
        _chsize_s,
        _commit,
        _locking,
        _isatty,
        _setmode,
        _get_osfhandle,
        _open_osfhandle,
        _dup,
        _dup2,
        _umask
    );
    reg!(
        _fstat32,
        _stat32,
        _wstat32,
        _fstat32i64,
        _stat32i64,
        _wstat32i64,
        _fstat64i32,
        _stat64i32,
        _wstat64i32,
        _fstat64,
        _stat64,
        _wstat64,
        _fstat,
        _stat,
        _wstat,
        _fstati64,
        _stati64,
        _wstati64
    );
    reg!(
        _fileno, _fdopen, _wfdopen, _access, _waccess, _chmod, _wchmod, _rmdir, _wrmdir, _unlink, _wunlink, _wremove, _chdir, _wchdir, _getcwd, _wgetcwd, _fullpath, _wfullpath, _utime64, _wutime64, _tempnam, _wtempnam
    );
    reg!(_wgetenv, _wgetenv_s, _wputenv, _putenv, _putenv_s, _wputenv_s);
    implement("msvcrt", "getenv_s", getenv_s_a);
    reg!(_tzset, __timezone, __daylight, __dstbias, __tzname, _get_tzname, _get_timezone, _get_daylight, _get_dstbias);
    reg!(strerror, _wcserror, setlocale, _wsetlocale, _mbtowc_l, mbrtowc, mbrlen, btowc, wctob, wcrtomb, wcrtomb_s, mbstowcs, mbsrtowcs, wcstombs);
    implement("msvcrt", "mbtowc", _mbtowc_l);
    implement("msvcrt", "_mbstowcs_l", mbstowcs);
    implement("msvcrt", "_wcstombs_l", wcstombs);
    for n in ["_getch", "_getche", "_ungetch", "_getch_nolock", "_getche_nolock"] {
        implement("msvcrt", n, conio_eof);
    }
    for n in ["_getwch", "_getwche", "_ungetwch", "_getwch_nolock", "_getwche_nolock"] {
        implement("msvcrt", n, conio_weof);
    }
    reg!(_kbhit, _putch, _putwch, _getpid, _popen);
    for n in [
        "system",
        "_wsystem",
        "_pclose",
        "_cwait",
        "_execv",
        "_execve",
        "_execvp",
        "_wexecv",
        "_wexecve",
        "_wexecvp",
        "_spawnv",
        "_spawnve",
        "_spawnvp",
        "_wspawnv",
        "_wspawnve",
        "_wspawnvp",
    ] {
        implement("msvcrt", n, no_process);
    }
    reg!(
        _set_invalid_parameter_handler,
        _get_invalid_parameter_handler,
        _assert,
        _wassert,
        raise,
        strerror_s,
        _wcserror_s,
        __sys_errlist,
        __sys_nerr,
        perror,
        rand_s,
        _amsg_exit,
        isblank,
        iswblank,
        __isascii,
        iswctype,
        _isctype_l,
        _isctype
    );
    implement("msvcrt", "_set_thread_local_invalid_parameter_handler", _set_invalid_parameter_handler);
    implement("msvcrt", "_iswctype_l", iswctype);
    reg!(
        _iswalpha_l,
        _iswcntrl_l,
        _iswdigit_l,
        _iswlower_l,
        _iswprint_l,
        _iswpunct_l,
        _iswspace_l,
        _iswupper_l,
        _iswxdigit_l,
        _iswalnum_l,
        _iswgraph_l
    );
    reg!(wcscoll, _strcoll_l, _strxfrm_l, wcsxfrm, _tolower_l, _toupper_l);
    implement("msvcrt", "_wcscoll_l", wcscoll);
    implement("msvcrt", "_wcsxfrm_l", wcsxfrm);
    implement("msvcrt", "strxfrm", _strxfrm_l);
    implement("msvcrt", "_towlower_l", towlower);
    implement("msvcrt", "_towupper_l", towupper);
    implement("msvcrt", "_strftime_l", strftime);
    for n in ["_heapmin", "__lconv_init", "_heapchk_ok"] {
        implement("msvcrt", n, zero);
    }
    reg!(setbuf, clearerr, fputwc, fputws, fgetwc, ungetwc, getchar, putc, wprintf);
    implement("msvcrt", "_fputwc_nolock", fputwc);
    implement("msvcrt", "_putc_nolock", putc);
    implement("msvcrt", "getwc", fgetwc);
    implement("msvcrt", "putwc", fputwc);
    implement("msvcrt", "__intrinsic_setjmpex", _setjmp3);
    reg!(erf, erfc, erff, erfcf, tgamma, lgamma, fma, fmaf, nextafter, nextafterf, rint, rintf, lrint, lrintf, llrint, lroundf);
    implement("msvcrt", "nearbyint", rint);
    implement("msvcrt", "nearbyintf", rintf);
    implement("msvcrt", "_nextafter", nextafter);
}

/// _mbstrlen(s): characters in the locale (a double-byte one counts once)
fn _mbstrlen(c: &mut Ctx) -> u32 {
    let s = cstr(c.a(0));
    let (mut i, mut n) = (0, 0);
    while i < s.len() {
        i += if lead(s[i]) && i + 1 < s.len() { 2 } else { 1 };
        n += 1;
    }
    n
}
/// _memicmp(a, b, n): ASCII letters without case
fn _memicmp(c: &mut Ctx) -> u32 {
    let n = c.a(2);
    let a = mem::slice(c.a(0), n).to_ascii_lowercase();
    let b = mem::slice(c.a(1), n).to_ascii_lowercase();
    a.cmp(&b) as i32 as u32
}
/// _wmakepath_s(path, size, drive, dir, fname, ext): errno
fn makepath_value(c: &Ctx, wide: bool, start: u32) -> Vec<u16> {
    let part = |p: u32| if p == 0 { Vec::new() } else if wide { wstr(p) } else { crate::text::a2w(&cstr(p)) };
    let (drive, dir, fname, ext) = (part(c.a(start)), part(c.a(start + 1)), part(c.a(start + 2)), part(c.a(start + 3)));
    let mut out = Vec::new();
    if let Some(&d) = drive.first() {
        out.extend([d, b':' as u16]);
    }
    if !dir.is_empty() {
        out.extend(&dir);
        if !matches!(dir.last(), Some(&0x5c) | Some(&0x2f)) {
            out.push(0x5c);
        }
    }
    out.extend(&fname);
    if !ext.is_empty() {
        if ext[0] != b'.' as u16 {
            out.push(b'.' as u16);
        }
        out.extend(&ext);
    }
    out
}
fn makepath_s(c: &mut Ctx, wide: bool) -> u32 {
    let (dst, size) = (c.a(0), c.a(1));
    let out = makepath_value(c, wide, 2);
    if dst == 0 || size == 0 {
        set_errno(EINVAL);
        return EINVAL;
    }
    let bytes = if wide { Vec::new() } else { crate::text::w2a(&out) };
    if (if wide { out.len() } else { bytes.len() }) as u32 >= size {
        if wide { w16(dst, 0); } else { mem::w8(dst, 0); }
        set_errno(34);
        return 34;
    }
    if wide {
        put_units(dst, &out);
        w16(dst + 2 * out.len() as u32, 0);
    } else {
        mem::write(dst, &bytes);
        mem::w8(dst + bytes.len() as u32, 0);
    }
    0
}
fn _wmakepath_s(c: &mut Ctx) -> u32 { makepath_s(c, true) }
fn _makepath_s(c: &mut Ctx) -> u32 { makepath_s(c, false) }
fn makepath(c: &mut Ctx, wide: bool) -> u32 {
    let dst = c.a(0);
    if dst == 0 { set_errno(EINVAL); return 0; }
    let out = makepath_value(c, wide, 1);
    if wide {
        put_units(dst, &out);
        w16(dst + 2 * out.len() as u32, 0);
    } else {
        let bytes = crate::text::w2a(&out);
        mem::write(dst, &bytes);
        mem::w8(dst + bytes.len() as u32, 0);
    }
    0
}
fn _makepath(c: &mut Ctx) -> u32 { makepath(c, false) }
fn _wmakepath(c: &mut Ctx) -> u32 { makepath(c, true) }

pub(super) fn init2() {
    reg!(_mbstrlen, _memicmp, _wmakepath_s, _makepath_s, _makepath, _wmakepath);
}

#[cfg(test)]
#[path = "crt_io_tests.rs"]
mod tests;
