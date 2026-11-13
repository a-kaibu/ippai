//! msvcp140.dll's <experimental/filesystem> support (x64), as the STL's stl/src/filesys.cpp:
//! the C-style functions std::experimental::filesystem calls (_Stat, _Open_dir, _File_size and
//! the rest), each a kernel32 call or two. They answer with Win32 error codes (or bools, or
//! -1) and leave errno alone, as the Windows ones (Wine's msvcp140 tests).
//!
//! Also here: the few Concurrency and std:: odds the same tests call (_Thrd_create /
//! _Thrd_start, GetNextAsyncId, _Winerror_message, _ContextCallback::_Assign and
//! _IsCurrentOriginSTA).

#![allow(non_snake_case)]

use std::sync::atomic::{AtomicU32, Ordering};

use crate::api::{self, implement, Ctx, Handler};
use crate::mem::{self, r32, w16, w32, w64, wp};

const MAX_PATH: u32 = 260;
const INVALID: u32 = u32::MAX;
const ERROR_BAD_PATHNAME: u32 = 161;
/// 100 ns ticks from 1601 to 1970 (what _Last_write_time subtracts)
const TICKS_1601_TO_1970: u64 = 116_444_736_000_000_000;

// file_type
const FILE_NOT_FOUND: u32 = -1i32 as u32;
const REGULAR: u32 = 1;
const DIRECTORY: u32 = 2;
const STATUS_UNKNOWN: u32 = 8;

const ATTR_READONLY: u32 = 0x1;
const ATTR_DIRECTORY: u32 = 0x10;

fn k32(c: &mut Ctx, name: &str, args: &[u32]) -> u32 {
    let f = api::export("kernel32", name);
    c.call(f, args)
}
fn last_error(c: &mut Ctx) -> u32 {
    k32(c, "GetLastError", &[])
}

/// Scratch guest memory for a call (freed by the caller).
struct Scratch(u32);
impl Scratch {
    fn new(n: u32) -> Scratch {
        Scratch(mem::heap_alloc(n, true))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        mem::heap_free(self.0);
    }
}

/// GetFileAttributesExW(path, GetFileExInfoStandard): WIN32_FILE_ATTRIBUTE_DATA { attributes,
/// creation, access, write FILETIMEs, size high, size low } or None.
fn attributes(c: &mut Ctx, path: u32) -> Option<(u32, u64, u64)> {
    let d = Scratch::new(36);
    if k32(c, "GetFileAttributesExW", &[path, 0, d.0]) == 0 {
        return None;
    }
    let lwt = r32(d.0 + 20) as u64 | (r32(d.0 + 24) as u64) << 32;
    let size = r32(d.0 + 32) as u64 | (r32(d.0 + 28) as u64) << 32;
    Some((r32(d.0), lwt, size))
}

/// _To_byte(const wchar_t *src, char *dst): the path in the file APIs' code page, at most
/// MAX_PATH bytes; 0 for a NULL one
fn _To_byte(c: &mut Ctx) -> u32 {
    let (src, dst) = (c.a(0), c.a(1));
    if src == 0 || dst == 0 {
        return 0;
    }
    k32(c, "WideCharToMultiByte", &[0, 0, src, INVALID, dst, MAX_PATH, 0, 0])
}
/// _To_wide(const char *src, wchar_t *dst)
fn _To_wide(c: &mut Ctx) -> u32 {
    let (src, dst) = (c.a(0), c.a(1));
    if src == 0 || dst == 0 {
        return 0;
    }
    k32(c, "MultiByteToWideChar", &[0, 0, src, INVALID, dst, MAX_PATH])
}

/// _File_size(path): the size (0 for a directory), or ~0
fn _File_size(c: &mut Ctx) -> u32 {
    let v = attributes(c, c.a(0)).map_or(u64::MAX, |(_, _, s)| s);
    c.ret64(v)
}
/// _Resize(path, size): 0 or the error
fn _Resize(c: &mut Ctx) -> u32 {
    let (path, size) = (c.a(0), c.a64(1));
    // FILE_GENERIC_WRITE, OPEN_EXISTING
    let h = k32(c, "CreateFileW", &[path, 0x120116, 0, 0, 3, 0, 0]);
    if h == INVALID {
        return last_error(c);
    }
    let hi = Scratch::new(4);
    w32(hi.0, (size >> 32) as u32);
    let r = if k32(c, "SetFilePointer", &[h, size as u32, hi.0, 0]) == INVALID && last_error(c) != 0 {
        last_error(c)
    } else if k32(c, "SetEndOfFile", &[h]) == 0 {
        last_error(c)
    } else {
        0
    };
    k32(c, "CloseHandle", &[h]);
    r
}

/// _Current_get(wchar_t (&dst)[MAX_PATH]): the current directory
fn _Current_get(c: &mut Ctx) -> u32 {
    let dst = c.a(0);
    let n = k32(c, "GetCurrentDirectoryW", &[MAX_PATH, dst]);
    (n != 0 && n < MAX_PATH) as u32
}
fn _Current_set(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    (k32(c, "SetCurrentDirectoryW", &[p]) != 0) as u32
}
/// _Temp_get(wchar_t (&dst)[MAX_PATH]): GetTempPathW's answer, `dst`
fn _Temp_get(c: &mut Ctx) -> u32 {
    let dst = c.a(0);
    k32(c, "GetTempPathW", &[MAX_PATH, dst]);
    dst
}

/// _Stat / _Lstat(path, perms *): the file_type, and 0555 / 0777 (read-only or not) when found.
/// Not found (or no such name): file_not_found; any other failure (a busy pipe):
/// status_unknown, the perms left as they are.
fn stat(c: &mut Ctx) -> u32 {
    let (path, perms) = (c.a(0), c.a(1));
    match attributes(c, path) {
        Some((attr, _, _)) => {
            if perms != 0 {
                w32(perms, if attr & ATTR_READONLY != 0 { 0o555 } else { 0o777 });
            }
            if attr & ATTR_DIRECTORY != 0 { DIRECTORY } else { REGULAR }
        }
        None => match last_error(c) {
            // FILE_NOT_FOUND, PATH_NOT_FOUND, INVALID_DRIVE, BAD_NETPATH, INVALID_NAME,
            // BAD_PATHNAME, INVALID_PARAMETER (NULL)
            2 | 3 | 15 | 53 | 87 | 123 | 161 => FILE_NOT_FOUND,
            _ => STATUS_UNKNOWN,
        },
    }
}

/// The type of a WIN32_FIND_DATAW entry, and whether it is "." or "..".
fn entry(fd: u32) -> (u32, bool) {
    let name = fd + 44;
    let dots = mem::wstrn(name, 3);
    let dot = dots == [b'.' as u16] || dots == [b'.' as u16, b'.' as u16];
    (if r32(fd) & ATTR_DIRECTORY != 0 { DIRECTORY } else { REGULAR }, dot)
}
/// The next entry of a search but "." and "..": its name into `dst` and its type, or "" and
/// status_unknown at the end.
fn next_entry(c: &mut Ctx, h: u32, fd: u32, mut have: bool, dst: u32, ftype: u32) -> bool {
    loop {
        if !have {
            if k32(c, "FindNextFileW", &[h, fd]) == 0 {
                w16(dst, 0);
                w32(ftype, STATUS_UNKNOWN);
                return false;
            }
        }
        have = false;
        let (t, dot) = entry(fd);
        if !dot {
            let name = mem::wstrn(fd + 44, MAX_PATH);
            mem::put_wstr(dst, MAX_PATH, &name);
            w32(ftype, t);
            return true;
        }
    }
}
/// _Open_dir(wchar_t (&dst)[MAX_PATH], const wchar_t *dir, int &err, file_type &ftype): a search
/// of `dir`\* and its first entry. A directory that can't be searched: NULL, err
/// ERROR_BAD_PATHNAME, ftype untouched; an empty one: NULL, err 0, status_unknown.
fn _Open_dir(c: &mut Ctx) -> u32 {
    let (dst, dir, err, ftype) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let mut pat = if dir == 0 { Vec::new() } else { mem::wstr(dir) };
    if let Some(&l) = pat.last() {
        if l != b'\\' as u16 && l != b'/' as u16 && l != b':' as u16 {
            pat.push(b'\\' as u16);
        }
    }
    pat.push(b'*' as u16);
    pat.push(0);
    let p = Scratch::new(2 * pat.len() as u32);
    for (i, &v) in pat.iter().enumerate() {
        w16(p.0 + 2 * i as u32, v);
    }
    let fd = Scratch::new(592);
    // FindFirstFileExW(pattern, FindExInfoBasic, data, FindExSearchNameMatch, NULL, 0)
    let h = k32(c, "FindFirstFileExW", &[p.0, 1, fd.0, 0, 0, 0]);
    if h == INVALID {
        w32(err, ERROR_BAD_PATHNAME);
        w16(dst, 0);
        return 0;
    }
    w32(err, 0);
    if !next_entry(c, h, fd.0, true, dst, ftype) {
        k32(c, "FindClose", &[h]);
        return 0;
    }
    h
}
/// _Read_dir(wchar_t (&dst)[MAX_PATH], void *handle, file_type &ftype): `dst`
fn _Read_dir(c: &mut Ctx) -> u32 {
    let (dst, h, ftype) = (c.a(0), c.a(1), c.a(2));
    let fd = Scratch::new(592);
    next_entry(c, h, fd.0, false, dst, ftype);
    dst
}
fn _Close_dir(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if h != 0 {
        k32(c, "FindClose", &[h]);
    }
    0
}

/// _Make_dir(path): 1 made, 0 already there, -1 failed
fn _Make_dir(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    if k32(c, "CreateDirectoryW", &[p, 0]) != 0 {
        return 1;
    }
    if last_error(c) == 183 { 0 } else { u32::MAX }
}
fn _Remove_dir(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    (k32(c, "RemoveDirectoryW", &[p]) != 0) as u32
}
/// 0 when `ok`, else GetLastError()
fn result(c: &mut Ctx, ok: u32) -> u32 {
    if ok != 0 { 0 } else { last_error(c) }
}
fn _Unlink(c: &mut Ctx) -> u32 {
    let p = c.a(0);
    let r = k32(c, "DeleteFileW", &[p]);
    result(c, r)
}
fn _Rename(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    let r = k32(c, "MoveFileExW", &[a, b, 0]);
    result(c, r)
}
/// _Copy_file(from, to): CopyFileW, overwriting
fn _Copy_file(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    let r = k32(c, "CopyFileW", &[a, b, 0]);
    result(c, r)
}
/// _Link(existing, new) / _Symlink(existing, new)
fn _Link(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    let r = k32(c, "CreateHardLinkW", &[b, a, 0]);
    result(c, r)
}
fn _Symlink(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    let r = k32(c, "CreateSymbolicLinkW", &[b, a, 0]);
    if r != 0 { 0 } else { last_error(c) }
}

/// _Last_write_time(path): the last write time in 100 ns ticks since 1970, or -1
fn _Last_write_time(c: &mut Ctx) -> u32 {
    let v = attributes(c, c.a(0)).map_or(u64::MAX, |(_, t, _)| t.wrapping_sub(TICKS_1601_TO_1970));
    c.ret64(v)
}
/// _Set_last_write_time(path, ticks since 1970)
fn _Set_last_write_time(c: &mut Ctx) -> u32 {
    let (path, t) = (c.a(0), c.a64(1));
    // FILE_WRITE_ATTRIBUTES, share all, OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS
    let h = k32(c, "CreateFileW", &[path, 0x100, 7, 0, 3, 0x0200_0000, 0]);
    if h == INVALID {
        return 0;
    }
    let ft = Scratch::new(8);
    w64(ft.0, t.wrapping_add(TICKS_1601_TO_1970));
    k32(c, "SetFileTime", &[h, 0, 0, ft.0]);
    k32(c, "CloseHandle", &[h]);
    0
}

/// The (volume serial, file index) of `path`, None when it can't be opened.
fn identity(c: &mut Ctx, path: u32) -> Option<[u32; 3]> {
    let h = k32(c, "CreateFileW", &[path, 0, 7, 0, 3, 0x0200_0000, 0]);
    if h == INVALID {
        return None;
    }
    let info = Scratch::new(52);
    let ok = k32(c, "GetFileInformationByHandle", &[h, info.0]);
    k32(c, "CloseHandle", &[h]);
    (ok != 0).then(|| [r32(info.0 + 28), r32(info.0 + 44), r32(info.0 + 48)])
}
/// _Equivalent(a, b): 1 the same file, 0 not, -1 neither exists
fn _Equivalent(c: &mut Ctx) -> u32 {
    let (a, b) = (c.a(0), c.a(1));
    match (identity(c, a), identity(c, b)) {
        (None, None) => u32::MAX,
        (Some(x), Some(y)) => (x == y) as u32,
        _ => 0,
    }
}

/// _Winerror_message(error, buf, size): FormatMessageA's text without its line end, its length
/// (0 and `buf` untouched when it doesn't fit)
fn _Winerror_message(c: &mut Ctx) -> u32 {
    let (err, buf, size) = (c.a(0), c.a(1), c.a(2));
    let tmp = Scratch::new(size.max(1));
    // FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS
    let mut n = k32(c, "FormatMessageA", &[0x1200, 0, err, 0, tmp.0, size, 0]);
    if n == 0 {
        return 0;
    }
    let s = mem::slice(tmp.0, n).to_vec();
    while n > 0 && matches!(s[n as usize - 1], b'\r' | b'\n') {
        n -= 1;
    }
    mem::write(buf, &s[..n as usize]);
    if n < size {
        mem::write(buf + n, &[0]);
    }
    n
}

/// _Thrd_create(_Thrd_t *, int (*)(void *), void *) / _Thrd_start(_Thrd_t *, unsigned
/// (__stdcall *)(void *), void *): a thread; the _Thrd_t { handle, id } filled in
fn _Thrd_create(c: &mut Ctx) -> u32 {
    let (t, f, arg) = (c.a(0), c.a(1), c.a(2));
    let id = Scratch::new(4);
    let h = k32(c, "CreateThread", &[0, 0, f, arg, 0, id.0]);
    if h == 0 {
        // _Thrd_error
        return 4;
    }
    wp(t, h);
    w32(t + mem::ps(), r32(id.0));
    0
}

/// Concurrency::details::platform::GetNextAsyncId: 1, 2, ...
fn GetNextAsyncId(_: &mut Ctx) -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn nothing(_: &mut Ctx) -> u32 {
    0
}

pub fn init() {
    let fs: &[(&str, Handler)] = &[
        ("_To_byte", _To_byte),
        ("_To_wide", _To_wide),
        ("_File_size", _File_size),
        ("_Resize", _Resize),
        ("_Current_get", _Current_get),
        ("_Current_set", _Current_set),
        ("_Temp_get", _Temp_get),
        ("_Stat", stat),
        ("_Lstat", stat),
        ("_Open_dir", _Open_dir),
        ("_Read_dir", _Read_dir),
        ("_Close_dir", _Close_dir),
        ("_Make_dir", _Make_dir),
        ("_Remove_dir", _Remove_dir),
        ("_Unlink", _Unlink),
        ("_Rename", _Rename),
        ("_Copy_file", _Copy_file),
        ("_Link", _Link),
        ("_Symlink", _Symlink),
        ("_Last_write_time", _Last_write_time),
        ("_Set_last_write_time", _Set_last_write_time),
        ("_Equivalent", _Equivalent),
        ("?_Winerror_message@std@@YAKKPEADK@Z", _Winerror_message),
        ("_Thrd_create", _Thrd_create),
        ("_Thrd_start", _Thrd_create),
        ("?GetNextAsyncId@platform@details@Concurrency@@YAIXZ", GetNextAsyncId),
        // (the context of an STA apartment: none here)
        ("?_IsCurrentOriginSTA@_ContextCallback@details@Concurrency@@CA_NXZ", nothing),
        ("?_Assign@_ContextCallback@details@Concurrency@@AEAAXPEAX@Z", nothing),
    ];
    for (n, f) in fs {
        implement("msvcp140", n, *f);
    }
}
