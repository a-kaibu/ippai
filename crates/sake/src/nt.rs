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

fn iosb(p: u32, st: u32, info: u32) {
    if p != 0 {
        w32(p, st);
        w32(p + 4, info);
    }
}

/// OBJECT_ATTRIBUTES' name as a Win32 path (in guest memory, to free).
fn path_of(oa: u32) -> Option<u32> {
    let us = r32(oa + 8);
    if us == 0 {
        return None;
    }
    let (n, b) = (r16(us) as u32 / 2, r32(us + 4));
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
        if s.len() >= dev.len() && s[..dev.len()].eq_ignore_ascii_case(dev) {
            w.splice(0..dev.len(), [d as u16, b':' as u16]);
        }
    }
    // relative to RootDirectory: not tracked; the name alone
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
    let existed = k32(c, "GetFileAttributesW", &[path]) != u32::MAX;
    let h = k32(c, "CreateFileW", &[path, access, share, 0, cd, flags, 0]);
    mem::heap_free(path);
    if h == u32::MAX {
        let st = status();
        iosb(io, st, 0);
        return st;
    }
    put32(out, h);
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
        ] {
            implement(d, &format!("Nt{n}"), f);
            implement(d, &format!("Zw{n}"), f);
        }
    }
}
