//! ntdll's native file API (Nt*/Zw*File), for programs that call it directly (Enigma
//! Protector's loader does). Each one goes through the kernel32 function that does the same.

use crate::api::{export, implement, Ctx};
use crate::mem::{self, put32, r16, r32, w32};

fn k32(c: &mut Ctx, name: &str, args: &[u32]) -> u32 {
    let f = export("kernel32", name);
    c.call(f, args)
}

fn last_error() -> u32 {
    r32(crate::cur().teb + crate::teb_last_error())
}

/// The NTSTATUS for the thread's Win32 error.
fn status() -> u32 {
    match last_error() {
        2 => 0xc000_0034,  // STATUS_OBJECT_NAME_NOT_FOUND
        3 => 0xc000_003a,  // STATUS_OBJECT_PATH_NOT_FOUND
        5 => 0xc000_0022,  // STATUS_ACCESS_DENIED
        32 => 0xc000_0043, // STATUS_SHARING_VIOLATION
        80 | 183 => 0xc000_0035,
        6 => 0xc000_0008,
        _ => 0xc000_0001,
    }
}

/// IO_STATUS_BLOCK: the status (a pointer-sized union), then Information (ULONG_PTR). An
/// error status leaves it as it was (Windows writes it for success and warnings only: Wine's
/// mailslot / directory tests)
fn iosb(p: u32, st: u32, info: u32) {
    if p != 0 && st < 0xc000_0000 {
        mem::wp(p, st);
        mem::wp(p + mem::ps(), info);
    }
}

/// OBJECT_ATTRIBUTES' name as a Win32 path (in guest memory, to free).
fn path_of(oa: u32) -> Option<u32> {
    // (OBJECT_ATTRIBUTES: Length, RootDirectory, ObjectName; UNICODE_STRING's Buffer after its
    // two lengths, both pointer-aligned)
    let us = mem::rp(oa + 2 * mem::ps());
    if us == 0 {
        return None;
    }
    let (n, b) = (r16(us) as u32 / 2, mem::rp(us + mem::ps()));
    let mut w: Vec<u16> = (0..n).map(|i| r16(b + 2 * i)).collect();
    let s = String::from_utf16_lossy(&w);
    for pre in ["\\??\\UNC\\", "\\??\\", "\\DosDevices\\", "\\GLOBAL??\\"] {
        if s.starts_with(pre) {
            w.drain(..pre.len());
            if pre.ends_with("UNC\\") {
                w.splice(0..0, [b'\\' as u16, b'\\' as u16]);
            }
            break;
        }
    }
    // a device path (QueryDosDevice's): its drive
    for d in ['C', 'Z'] {
        let dev = crate::k32::drive_device(d).unwrap();
        // (s may hold Japanese: compare bytes, the device name is ASCII)
        if s.as_bytes().get(..dev.len()).is_some_and(|p| p.eq_ignore_ascii_case(dev.as_bytes())) {
            w.splice(0..dev.len(), [d as u16, b':' as u16]);
        }
    }
    // relative to RootDirectory (a directory handle): under that directory
    let root = mem::rp(oa + mem::ps());
    if root != 0 {
        let dir = crate::obj::with(|t| match t.objs.get(&root) {
            Some((crate::obj::Obj::File(f), _)) if f.dir => Some(crate::files::host_to_win(&f.path)),
            _ => None,
        });
        if let Some(d) = dir {
            let pre: Vec<u16> = format!("{}\\", d.trim_end_matches('\\')).encode_utf16().collect();
            w.splice(0..0, pre);
        }
    }
    let p = mem::heap_alloc(2 * w.len() as u32 + 2, true);
    for (i, &x) in w.iter().enumerate() {
        mem::w16(p + 2 * i as u32, x);
    }
    Some(p)
}

fn create(c: &mut Ctx, out: u32, access: u32, oa: u32, io: u32, attrs: u32, share: u32, disp: u32, opts: u32) -> u32 {
    let Some(path) = path_of(oa) else { return 0xc000_0033 };
    // NT disposition -> CreateFile's
    let cd = match disp {
        0 | 5 => 2, // SUPERSEDE, OVERWRITE_IF: CREATE_ALWAYS
        2 => 1,     // CREATE: CREATE_NEW
        3 => 4,     // OPEN_IF: OPEN_ALWAYS
        4 => 5,     // OVERWRITE: TRUNCATE_EXISTING
        _ => 3,     // OPEN: OPEN_EXISTING
    };
    let mut flags = attrs & 0xffff;
    if opts & 1 != 0 {
        flags |= 0x0200_0000; // FILE_DIRECTORY_FILE: FILE_FLAG_BACKUP_SEMANTICS
    }
    if opts & 0x1000 != 0 {
        flags |= 0x0400_0000; // FILE_DELETE_ON_CLOSE
    }
    let attrs_now = k32(c, "GetFileAttributesW", &[path]);
    let existed = attrs_now != u32::MAX;
    let is_dir = existed && attrs_now & 0x10 != 0;
    // FILE_DIRECTORY_FILE: a directory only (made by CREATE / OPEN_IF); FILE_NON_DIRECTORY_FILE:
    // never one
    let fail = |st: u32| {
        mem::heap_free(path);
        st
    };
    if opts & 1 != 0 {
        if existed && !is_dir {
            return fail(0xc000_0103); // STATUS_NOT_A_DIRECTORY
        }
        if existed && disp == 2 {
            return fail(0xc000_0035); // STATUS_OBJECT_NAME_COLLISION
        }
        if !existed && (disp == 2 || disp == 3) {
            if k32(c, "CreateDirectoryW", &[path, 0]) == 0 {
                let st = status();
                return fail(st);
            }
        }
    } else if opts & 0x40 != 0 && is_dir {
        return fail(0xc000_00ba); // STATUS_FILE_IS_A_DIRECTORY
    }
    let cd = if opts & 1 != 0 { 3 } else { cd };
    let h = k32(c, "CreateFileW", &[path, access, share, 0, cd, flags, 0]);
    mem::heap_free(path);
    if h == u32::MAX {
        let st = status();
        iosb(io, st, 0);
        return st;
    }
    mem::wp(out, h);
    // FILE_SUPERSEDED 0, FILE_OPENED 1, FILE_CREATED 2, FILE_OVERWRITTEN 3
    let info = match (disp, existed) {
        (0, true) => 0,
        (4 | 5, true) => 3,
        (_, true) => 1,
        _ => 2,
    };
    iosb(io, 0, info);
    0
}

/// NtCreateFile(&h, access, oa, iosb, alloc, attrs, share, disposition, options, ea, ealen)
fn nt_create_file(c: &mut Ctx) -> u32 {
    let a: Vec<u32> = (0..9).map(|i| c.a(i)).collect();
    create(c, a[0], a[1], a[2], a[3], a[5], a[6], a[7], a[8])
}
/// NtOpenFile(&h, access, oa, iosb, share, options)
fn nt_open_file(c: &mut Ctx) -> u32 {
    let a: Vec<u32> = (0..6).map(|i| c.a(i)).collect();
    create(c, a[0], a[1], a[2], a[3], 0, a[4], 1, a[5])
}
fn nt_close(c: &mut Ctx) -> u32 {
    let h = c.a(0);
    if k32(c, "CloseHandle", &[h]) != 0 { 0 } else { 0xc000_0008 }
}

/// Moves to the offset a read or write was given (none, or FILE_USE_FILE_POINTER_POSITION).
fn seek_to(c: &mut Ctx, h: u32, off: u32) {
    if off == 0 {
        return;
    }
    let (lo, hi) = (r32(off), r32(off + 4));
    if hi == 0xffff_ffff && (lo == 0xffff_fffe || lo == 0xffff_ffff) {
        return;
    }
    k32(c, "SetFilePointerEx", &[h, lo, hi, 0, 0]);
}

/// NtReadFile(h, event, apc, apcctx, iosb, buf, len, &offset, key)
fn nt_read_file(c: &mut Ctx) -> u32 {
    let (h, io, buf, len, off) = (c.a(0), c.a(4), c.a(5), c.a(6), c.a(7));
    if let Some(p) = crate::pipe::handle(h) {
        return crate::pipe::nt_read(h, p, crate::pipe::NtIo { iosb: io, event: c.a(1), apc: c.a(2), ctx: c.a(3) }, buf, len);
    }
    seek_to(c, h, off);
    let n = mem::heap_alloc(4, true);
    let ok = k32(c, "ReadFile", &[h, buf, len, n, 0]);
    let got = r32(n);
    mem::heap_free(n);
    if ok == 0 {
        let st = status();
        iosb(io, st, 0);
        return st;
    }
    if got == 0 && len != 0 {
        iosb(io, 0xc000_0011, 0);
        return 0xc000_0011; // STATUS_END_OF_FILE
    }
    iosb(io, 0, got);
    0
}
fn nt_write_file(c: &mut Ctx) -> u32 {
    let (h, io, buf, len, off) = (c.a(0), c.a(4), c.a(5), c.a(6), c.a(7));
    if let Some(p) = crate::pipe::handle(h) {
        return crate::pipe::nt_write(h, p, crate::pipe::NtIo { iosb: io, event: c.a(1), apc: c.a(2), ctx: c.a(3) }, buf, len);
    }
    seek_to(c, h, off);
    let n = mem::heap_alloc(4, true);
    let ok = k32(c, "WriteFile", &[h, buf, len, n, 0]);
    let got = r32(n);
    mem::heap_free(n);
    if ok == 0 {
        let st = status();
        iosb(io, st, 0);
        return st;
    }
    iosb(io, 0, got);
    0
}
fn nt_flush_buffers_file(c: &mut Ctx) -> u32 {
    iosb(c.a(1), 0, 0);
    0
}

/// BY_HANDLE_FILE_INFORMATION of `h` (52 bytes, in guest memory to free) or None.
fn by_handle(c: &mut Ctx, h: u32) -> Option<u32> {
    let p = mem::heap_alloc(52, true);
    if k32(c, "GetFileInformationByHandle", &[h, p]) == 0 {
        mem::heap_free(p);
        return None;
    }
    Some(p)
}
fn copy(dst: u32, src: u32, n: u32) {
    mem::copy(dst, src, n);
}
/// FILE_BASIC_INFORMATION from Win32 times at `t` (creation, access, write) and attributes.
fn basic(out: u32, t: u32, attrs: u32) {
    copy(out, t, 8);
    copy(out + 8, t + 8, 8);
    copy(out + 16, t + 16, 8);
    copy(out + 24, t + 16, 8); // ChangeTime
    w32(out + 32, attrs);
    w32(out + 36, 0);
}

/// NtQueryInformationFile(h, iosb, buf, len, class)
fn nt_query_information_file(c: &mut Ctx) -> u32 {
    let (h, io, out, len, class) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let need = match class {
        4 => 40,
        5 => 24,
        14 => 8,
        34 => 56,
        _ => {
            iosb(io, 0xc000_0003, 0);
            return 0xc000_0003; // STATUS_INVALID_INFO_CLASS
        }
    };
    if len < need {
        return 0xc000_0004; // STATUS_INFO_LENGTH_MISMATCH
    }
    if class == 14 {
        let p = mem::heap_alloc(8, true);
        k32(c, "SetFilePointerEx", &[h, 0, 0, p, 1]);
        copy(out, p, 8);
        mem::heap_free(p);
        iosb(io, 0, 8);
        return 0;
    }
    let Some(i) = by_handle(c, h) else { return 0xc000_0008 };
    let (attrs, size_hi, size_lo) = (r32(i), r32(i + 32), r32(i + 36));
    match class {
        4 => basic(out, i + 4, attrs),
        5 => {
            for o in [0, 8] {
                w32(out + o, size_lo);
                w32(out + o + 4, size_hi);
            }
            w32(out + 16, r32(i + 40));
            w32(out + 20, (attrs & 0x10 != 0) as u32 * 0x100);
        }
        _ => {
            // FILE_NETWORK_OPEN_INFORMATION
            basic(out, i + 4, 0);
            for o in [32, 40] {
                w32(out + o, size_lo);
                w32(out + o + 4, size_hi);
            }
            w32(out + 48, attrs);
            w32(out + 52, 0);
        }
    }
    mem::heap_free(i);
    iosb(io, 0, need);
    0
}

/// NtSetInformationFile(h, iosb, buf, len, class): the position and the end of file.
fn nt_set_information_file(c: &mut Ctx) -> u32 {
    let (h, io, buf, class) = (c.a(0), c.a(1), c.a(2), c.a(4));
    match class {
        14 => {
            k32(c, "SetFilePointerEx", &[h, r32(buf), r32(buf + 4), 0, 0]);
        }
        20 => {
            let p = mem::heap_alloc(8, true);
            k32(c, "SetFilePointerEx", &[h, 0, 0, p, 1]);
            k32(c, "SetFilePointerEx", &[h, r32(buf), r32(buf + 4), 0, 0]);
            k32(c, "SetEndOfFile", &[h]);
            k32(c, "SetFilePointerEx", &[h, r32(p), r32(p + 4), 0, 0]);
            mem::heap_free(p);
        }
        4 => {}
        _ => {
            iosb(io, 0xc000_0003, 0);
            return 0xc000_0003;
        }
    }
    iosb(io, 0, 0);
    0
}

/// WIN32_FILE_ATTRIBUTE_DATA (36 bytes) of OBJECT_ATTRIBUTES' file, or the status.
fn attr_data(c: &mut Ctx, oa: u32) -> Result<u32, u32> {
    let Some(path) = path_of(oa) else { return Err(0xc000_0033) };
    let d = mem::heap_alloc(36, true);
    let ok = k32(c, "GetFileAttributesExW", &[path, 0, d]);
    mem::heap_free(path);
    if ok == 0 {
        mem::heap_free(d);
        return Err(status());
    }
    Ok(d)
}
/// NtQueryAttributesFile(oa, FILE_BASIC_INFORMATION*)
fn nt_query_attributes_file(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    match attr_data(c, c.a(0)) {
        Ok(d) => {
            basic(out, d + 4, r32(d));
            mem::heap_free(d);
            0
        }
        Err(st) => st,
    }
}
/// NtQueryFullAttributesFile(oa, FILE_NETWORK_OPEN_INFORMATION*)
fn nt_query_full_attributes_file(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    match attr_data(c, c.a(0)) {
        Ok(d) => {
            basic(out, d + 4, 0);
            let (hi, lo) = (r32(d + 28), r32(d + 32));
            for o in [32, 40] {
                w32(out + o, lo);
                w32(out + o + 4, hi);
            }
            w32(out + 48, r32(d));
            w32(out + 52, 0);
            mem::heap_free(d);
            0
        }
        Err(st) => st,
    }
}
fn nt_delete_file(c: &mut Ctx) -> u32 {
    let Some(path) = path_of(c.a(0)) else { return 0xc000_0033 };
    let ok = k32(c, "DeleteFileW", &[path]);
    mem::heap_free(path);
    if ok != 0 { 0 } else { status() }
}

/// NtSetInformationThread(h, class, buf, len): accepted (ThreadHideFromDebugger and the like).
fn nt_set_information_thread(_: &mut Ctx) -> u32 {
    0
}
/// NtQueryInformationProcess(h, class, buf, len, &retlen)
fn nt_query_information_process(c: &mut Ctx) -> u32 {
    let (class, out, len, ret) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let (need, st) = match class {
        0 => (24, 0),            // ProcessBasicInformation
        7 => (4, 0),             // ProcessDebugPort: none
        0x1e => (4, 0xc000_0353), // ProcessDebugObjectHandle: STATUS_PORT_NOT_SET
        0x1f => (4, 0),          // ProcessDebugFlags: not debugged
        _ => return 0xc000_0003,
    };
    if len < need {
        return 0xc000_0004;
    }
    mem::fill(out, need, 0);
    match class {
        0 => {
            w32(out + 4, crate::PEB.load(std::sync::atomic::Ordering::Relaxed));
            w32(out + 8, 1);
            w32(out + 12, 8);
            w32(out + 16, 0x20);
        }
        0x1f => w32(out, 1),
        _ => {}
    }
    put32(ret, need);
    st
}

/// A name in upper case as UTF-16, the way NTFS orders a directory (RtlUpcaseUnicodeChar on
/// each unit)
fn upcase(n: &str) -> Vec<u16> {
    n.encode_utf16().map(up).collect()
}
pub fn up(u: u16) -> u16 {
    match char::from_u32(u as u32) {
        Some(c) => {
            let mut it = c.to_uppercase();
            match (it.next(), it.next()) {
                (Some(x), None) if (x as u32) < 0x10000 => x as u32 as u16,
                _ => u,
            }
        }
        None => u,
    }
}

/// The attributes of a listed entry (what SetFileAttributes gave a file included)
fn entry_attrs(dir: &str, name: &str, m: &kura::fs::Metadata) -> u32 {
    if name == "." || name == ".." {
        return crate::files::attrs(m);
    }
    crate::files::attrs_of(&format!("{}/{name}", dir.trim_end_matches('/')), m)
}

/// Whether `name` matches the NtQueryDirectoryFile mask `expr`, without regard to case:
/// '*' any characters, '?' one, and the DOS wildcards FindFirstFile's patterns become:
/// '<' (DOS_STAR) any characters up to and including the name's last period (all of a name
/// whose periods all lead, from its start), '>' (DOS_QM) one character, or none at a period or the end (a run of
/// them then ends there, taking the period only when nothing follows), '"' (DOS_DOT) a period
/// or nothing at the end. ".." is matched as "." (Windows' answers in Wine's ntdll directory
/// test).
pub fn nt_match(expr: &[u16], name: &[u16]) -> bool {
    let name = if name == [b'.' as u16, b'.' as u16] { &name[..1] } else { name };
    let (e, n) = (expr.len(), name.len());
    let lead = name.iter().take_while(|&&c| c == b'.' as u16).count();
    // memo[i][j]: expr[i..] against name[j..]
    let mut memo = vec![None::<bool>; (e + 1) * (n + 1)];
    fn go(i: usize, j: usize, x: &[u16], s: &[u16], lead: usize, memo: &mut Vec<Option<bool>>) -> bool {
        let (e, n) = (x.len(), s.len());
        if let Some(r) = memo[i * (n + 1) + j] {
            return r;
        }
        let dot = b'.' as u16;
        let r = if i == e {
            j == n
        } else {
            match x[i] {
                0x2a => (j..=n).any(|k| go(i + 1, k, x, s, lead, memo)),
                0x3c => {
                    // up to just past the last period, or anywhere with none; at the start of a
                    // name whose periods all lead (".a"), also the whole name
                    let to = (j..n).rev().find(|&k| s[k] == dot).map_or(n, |k| k + 1);
                    let whole = j == 0 && !s[lead..].contains(&dot);
                    (j..=to).chain(whole.then_some(n)).any(|k| go(i + 1, k, x, s, lead, memo))
                }
                0x3f => j < n && go(i + 1, j + 1, x, s, lead, memo),
                0x3e => {
                    if j == n {
                        go(i + 1, j, x, s, lead, memo)
                    } else if s[j] == dot {
                        let mut k = i;
                        while k < e && x[k] == 0x3e {
                            k += 1;
                        }
                        if k == e { go(k, j + 1, x, s, lead, memo) } else { go(k, j, x, s, lead, memo) }
                    } else {
                        go(i + 1, j + 1, x, s, lead, memo)
                    }
                }
                0x22 => {
                    if j == n {
                        go(i + 1, j, x, s, lead, memo)
                    } else {
                        s[j] == dot && go(i + 1, j + 1, x, s, lead, memo)
                    }
                }
                c => j < n && up(c) == up(s[j]) && go(i + 1, j + 1, x, s, lead, memo),
            }
        };
        memo[i * (n + 1) + j] = Some(r);
        r
    }
    go(0, 0, expr, name, lead, &mut memo)
}

/// The fixed part of each FILE_INFORMATION_CLASS NtQueryDirectoryFile writes: (its size, where
/// FileNameLength is, where the name goes)
fn dir_class(class: u32) -> Option<(u32, u32, u32)> {
    Some(match class {
        1 => (64, 60, 64),   // FileDirectoryInformation
        2 => (68, 60, 68),   // FileFullDirectoryInformation
        3 => (94, 60, 94),   // FileBothDirectoryInformation
        12 => (12, 8, 12),   // FileNamesInformation
        37 => (104, 60, 104), // FileIdBothDirectoryInformation
        38 => (80, 60, 80),  // FileIdFullDirectoryInformation
        // (ReFS's 128-bit ids and Windows 11's 64-bit + 128-bit ones, after EaSize and
        // ReparsePointTag; the Both ones with the short name between)
        60 => (88, 60, 88),   // FileIdExtdDirectoryInformation
        63 => (114, 60, 114), // FileIdExtdBothDirectoryInformation
        78 => (80, 60, 80),   // FileId64ExtdDirectoryInformation
        79 => (106, 60, 106), // FileId64ExtdBothDirectoryInformation
        80 => (96, 60, 96),   // FileIdAllExtdDirectoryInformation
        81 => (122, 60, 122), // FileIdAllExtdBothDirectoryInformation
        _ => return None,
    })
}

/// One entry at `p` (zeroed first): the times, sizes and attributes the class has, and as much
/// of the name as fits in `room` bytes; the name's bytes written
fn dir_entry(p: u32, class: u32, name: &str, m: &kura::fs::Metadata, attrs: u32, room: u32) -> u32 {
    let (fixed, len_at, name_at) = dir_class(class).unwrap();
    mem::fill(p, fixed, 0);
    if class != 12 {
        let t = m.modified().map_or(0, crate::files::filetime);
        for o in [8, 16, 24, 32] {
            mem::put64(p + o, t);
        }
        let size = if m.is_dir() { 0 } else { m.len() };
        mem::put64(p + 40, size);
        mem::put64(p + 48, size); // (as NtQueryInformationFile gives it)
        w32(p + 56, attrs);
    }
    let w: Vec<u16> = name.encode_utf16().collect();
    w32(p + len_at, 2 * w.len() as u32);
    let n = (2 * w.len() as u32).min(room.saturating_sub(name_at)) / 2;
    for (i, &u) in w[..n as usize].iter().enumerate() {
        mem::w16(p + name_at + 2 * i as u32, u);
    }
    2 * n
}

/// NtQueryDirectoryFile(dir, event, apc, apc ctx, iosb, buf, len, class, single, mask, restart):
/// the next entries of a directory opened with FILE_DIRECTORY_FILE / FILE_FLAG_BACKUP_SEMANTICS
/// (.NET's Directory.GetFiles under mono reads directories this way). The mask counts on the
/// first call (or a restart) only; "." and ".." come first except at a drive's root.
fn nt_query_directory_file(c: &mut Ctx) -> u32 {
    let (h, io, buf, len, class) = (c.a(0), c.a(4), c.a(5), c.a(6), c.a(7));
    let (single, mask, restart) = (c.a(8) & 0xff != 0, c.a(9), c.a(10) & 0xff != 0);
    let Some((fixed, _, _)) = dir_class(class) else {
        // FileObjectIdInformation, FileQuotaInformation, FileReparsePointInformation: not for
        // a directory of files, once the buffer holds one
        return match class {
            29 | 32 | 33 if len < [72, 56, 16][[29, 32, 33].iter().position(|&x| x == class).unwrap()] => 0xc000_0004,
            29 | 32 | 33 => 0xc000_0003,
            // STATUS_INVALID_INFO_CLASS; WoW64's thunk has no conversion for the classes from
            // 76 on that are not a directory's (76, 84..100 and 99 in winref ntdir):
            // STATUS_NOT_IMPLEMENTED in a 32-bit process (Wine's directory test: class 50 is
            // INVALID_INFO_CLASS there)
            c if c >= 76 && !mem::x64() => 0xc000_0002,
            _ => 0xc000_0003,
        };
    };
    let f = crate::obj::with(|t| match t.objs.get(&h) {
        Some((crate::obj::Obj::File(f), _)) => Some(f.clone()),
        _ => None,
    });
    let Some(f) = f else { return 0xc000_0008 };
    if !f.dir {
        return 0xc000_000d; // STATUS_INVALID_PARAMETER
    }
    // (room for the fixed part and one character, 8-aligned)
    if len < (fixed + 2 + 7) & !7 {
        return 0xc000_0004; // STATUS_INFO_LENGTH_MISMATCH
    }
    let mut scan = f.scan.lock().unwrap();
    let first = scan.is_none();
    if first || restart {
        // a new mask on the first call or a restart (Windows 8 on); none (NULL or empty) keeps
        // the last one, "*" at first
        let mask: Vec<u16> = if mask != 0 && r16(mask) != 0 {
            let b = mem::rp(mask + mem::ps());
            (0..r16(mask) as u32 / 2).map(|i| r16(b + 2 * i)).collect()
        } else {
            scan.as_ref().map_or(vec![b'*' as u16], |s| s.0.clone())
        };
        let win = crate::files::host_to_win(&f.path);
        let all = crate::files::find(&format!("{}\\*", win.trim_end_matches('\\'))).map_or(Vec::new(), |f| f.entries);
        let mut entries: Vec<_> = all.into_iter().filter(|(n, _)| nt_match(&mask, &n.encode_utf16().collect::<Vec<_>>())).collect();
        // NTFS's order: "." and "..", then the names compared in upper case
        entries.sort_by_cached_key(|(n, _)| (n != ".", n != "..", upcase(n)));
        let none = entries.is_empty();
        *scan = Some((mask, crate::files::Find { entries, at: 0 }));
        if none {
            // (nothing at all on a handle's first call; a restart that finds nothing has
            // reached the end)
            if first {
                // (an error, yet the IO_STATUS_BLOCK gets it, single or not, restarting or
                // not: winref ntdir)
                if io != 0 {
                    mem::wp(io, 0xc000_000f);
                    mem::wp(io + mem::ps(), 0);
                }
                return 0xc000_000f; // STATUS_NO_SUCH_FILE
            }
            iosb(io, 0x8000_0006, 0);
            return 0x8000_0006;
        }
    }
    let s = &mut scan.as_mut().unwrap().1;
    if s.at >= s.entries.len() {
        iosb(io, 0x8000_0006, 0);
        return 0x8000_0006; // STATUS_NO_MORE_FILES
    }
    let (mut at, mut prev) = (0u32, None::<u32>);
    while s.at < s.entries.len() {
        let (name, m) = &s.entries[s.at];
        let need = fixed + 2 * name.encode_utf16().count() as u32;
        if at + need > len {
            if prev.is_none() {
                // (not even the first: its fixed part and what fits of the name, kept for next time)
                dir_entry(buf, class, name, m, entry_attrs(&f.path, name, m), len);
                iosb(io, 0x8000_0005, len);
                return 0x8000_0005; // STATUS_BUFFER_OVERFLOW
            }
            break;
        }
        if let Some(p) = prev {
            w32(buf + p, at - p);
        }
        dir_entry(buf + at, class, name, m, entry_attrs(&f.path, name, m), need);
        prev = Some(at);
        s.at += 1;
        at = (at + need + 7) & !7;
        if single {
            break;
        }
    }
    let end = prev.map_or(0, |p| p + fixed + r32(buf + p + dir_class(class).unwrap().1));
    iosb(io, 0, end);
    0
}

pub fn init() {
    for (n, f) in [
        ("SetInformationThread", nt_set_information_thread as crate::api::Handler),
        ("QueryInformationProcess", nt_query_information_process),
    ] {
        implement("ntdll", &format!("Nt{n}"), f);
        implement("ntdll", &format!("Zw{n}"), f);
    }
    for d in ["ntdll"] {
        for (n, f) in [
            ("CreateFile", nt_create_file as crate::api::Handler),
            ("OpenFile", nt_open_file),
            ("Close", nt_close),
            ("ReadFile", nt_read_file),
            ("WriteFile", nt_write_file),
            ("FlushBuffersFile", nt_flush_buffers_file),
            ("QueryInformationFile", nt_query_information_file),
            ("SetInformationFile", nt_set_information_file),
            ("QueryAttributesFile", nt_query_attributes_file),
            ("QueryFullAttributesFile", nt_query_full_attributes_file),
            ("DeleteFile", nt_delete_file),
            ("QueryDirectoryFile", nt_query_directory_file),
        ] {
            implement(d, &format!("Nt{n}"), f);
            implement(d, &format!("Zw{n}"), f);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::nt_match;
    fn m(e: &str, n: &str) -> bool {
        nt_match(&e.encode_utf16().collect::<Vec<_>>(), &n.encode_utf16().collect::<Vec<_>>())
    }
    #[test]
    fn nt_masks() {
        // (mask, name, whether Windows lists it)
        for (e, n, want) in [
            ("*", "a.b", true),
            ("*.", "a.", true),
            ("*.", "ab", false),
            ("?.*", ".", false),
            ("?.*", "..", false),
            ("?.*", "a.b", true),
            ("A.TXT", "a.txt", true),
            ("\u{c9}b.tmp", "\u{e9}b.tmp", true),
            ("<a", ".a", true),
            ("<a", "a.", false),
            ("<a", "ea", true),
            ("<a", ".aa", false),
            ("<.<", "ea", false),
            ("<.<", ".", true),
            ("<.<.<", "ea.tmp.tmp", true),
            ("<.<.<", "n.tmp", false),
            ("<.<.<", "..a", true),
            ("<\"\"", ".a", true),
            ("<\"\"", ".a.a", false),
            ("<\"\"", "n.tmp", false),
            ("<\"<.", "a..", true),
            ("<\"<.", "a.", false),
            (">", ".", true),
            (">", "..", true),
            (">", "ea", false),
            (">>", "a.", true),
            (">>", ".a", false),
            (">.>", ".a", true),
            (">>.>>>", ".a.a", false),
            (">>.>>>", "a..", true),
            (">>.tmp", "ea.tmp", true),
            (">>.tmp", "longfilename.tmp", false),
            ("n\"tmp", "n.tmp", true),
            ("ea\"", "ea", true),
            ("ea\"", "ea.tmp", false),
            ("e\"a", "ea", false),
            ("<", ".a", true),
            ("<", ".a.a", false),
            ("<", "n.tmp", false),
            (".<", "..a", false),
            (".<", ".aa", true),
            (".<", "a.", false),
        ] {
            assert_eq!(m(e, n), want, "{e:?} {n:?}");
        }
    }
}

#[cfg(test)]
mod dir_class_tests {
    use super::dir_class;
    use std::mem::offset_of;

    // the directory information structures as ntifs.h lays them out (FILE_ID_128 is 16 bytes,
    // LARGE_INTEGER 8-aligned), up to FileName
    #[repr(C)]
    struct Head {
        next: u32,
        index: u32,
        times: [i64; 4],
        eof: i64,
        alloc: i64,
        attrs: u32,
        name_len: u32,
    }
    #[repr(C)]
    struct Dir { h: Head, name: u16 }
    #[repr(C)]
    struct Full { h: Head, ea: u32, name: u16 }
    #[repr(C)]
    struct Both { h: Head, ea: u32, short_len: i8, short: [u16; 12], name: u16 }
    #[repr(C)]
    struct IdBoth { h: Head, ea: u32, short_len: i8, short: [u16; 12], id: i64, name: u16 }
    #[repr(C)]
    struct IdFull { h: Head, ea: u32, id: i64, name: u16 }
    #[repr(C)]
    struct IdExtd { h: Head, ea: u32, tag: u32, id: [u8; 16], name: u16 }
    #[repr(C)]
    struct IdExtdBoth { h: Head, ea: u32, tag: u32, id: [u8; 16], short_len: i8, short: [u16; 12], name: u16 }
    #[repr(C)]
    struct Id64Extd { h: Head, ea: u32, tag: u32, id: i64, name: u16 }
    #[repr(C)]
    struct Id64ExtdBoth { h: Head, ea: u32, tag: u32, id: i64, short_len: i8, short: [u16; 12], name: u16 }
    #[repr(C)]
    struct IdAllExtd { h: Head, ea: u32, tag: u32, id: i64, id128: [u8; 16], name: u16 }
    #[repr(C)]
    struct IdAllExtdBoth { h: Head, ea: u32, tag: u32, id: i64, id128: [u8; 16], short_len: i8, short: [u16; 12], name: u16 }

    #[test]
    fn layouts_as_the_headers() {
        let len_at = offset_of!(Head, name_len) as u32;
        assert_eq!(len_at, 60);
        for (class, name_at) in [
            (1, offset_of!(Dir, name) as u32),
            (2, offset_of!(Full, name) as u32),
            (3, offset_of!(Both, name) as u32),
            (37, offset_of!(IdBoth, name) as u32),
            (38, offset_of!(IdFull, name) as u32),
            (60, offset_of!(IdExtd, name) as u32),
            (63, offset_of!(IdExtdBoth, name) as u32),
            (78, offset_of!(Id64Extd, name) as u32),
            (79, offset_of!(Id64ExtdBoth, name) as u32),
            (80, offset_of!(IdAllExtd, name) as u32),
            (81, offset_of!(IdAllExtdBoth, name) as u32),
        ] {
            assert_eq!(dir_class(class), Some((name_at, len_at, name_at)), "class {class}");
        }
        assert_eq!(dir_class(12), Some((12, 8, 12)));
        // not a directory's, or past the classes
        for c in [0, 4, 29, 50, 76, 77, 82, 84, 99] {
            assert_eq!(dir_class(c), None, "class {c}");
        }
    }
}
