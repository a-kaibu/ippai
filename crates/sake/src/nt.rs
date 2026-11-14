//! ntdll's native file API (Nt*/Zw*File), for programs that call it directly (Enigma
//! Protector's loader does). Each one goes through the kernel32 function that does the same.

use crate::api::{export, implement, Ctx};
use crate::mem::{self, put32, r16, r32, w32};

fn k32(c: &mut Ctx, name: &str, args: &[u32]) -> u32 {
    let f = export("kernel32", name);
    // (kernel32 under ntdll here: it does the work, not the program's hook again)
    IN_NT.with(|d| d.set(d.get() + 1));
    let r = c.call(f, args);
    IN_NT.with(|d| d.set(d.get() - 1));
    r
}

thread_local! {
    /// Inside one of ntdll's file functions (their kernel32 calls).
    static IN_NT: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The code of ntdll's `name` ("NtCreateFile"; ZwCreateFile is the same function) when the
/// program has patched it, as Enigma Virtual Box does to serve the files packed into the exe.
/// kernel32's file functions then go through it the way Windows' go through ntdll; while it is
/// sake's own thunk (or from inside ntdll's work) they need not.
pub fn hooked(name: &str) -> Option<u32> {
    if mem::x64() || IN_NT.with(|d| d.get()) != 0 {
        return None;
    }
    let base = crate::pe::by_name("ntdll.dll")?;
    let t = crate::pe::with(base, |m| m.exports.get(name).copied())??;
    // (the thunk: hot-patch prologue `mov edi, edi; push ebp; mov ebp, esp; pop ebp`, then
    // `push trap; ret`)
    let same = mem::slice(t, 7) == [0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d, 0x68] && r32(t + 7) == export("ntdll", name) && mem::r8(t + 11) == 0xc3;
    (!same).then_some(t)
}

/// Guest scratch for a call into ntdll: freed when dropped.
struct Scratch(Vec<u32>);
impl Scratch {
    fn new() -> Self {
        Scratch(Vec::new())
    }
    fn alloc(&mut self, n: u32) -> u32 {
        let p = mem::heap_alloc(n.max(4), true);
        self.0.push(p);
        p
    }
    /// OBJECT_ATTRIBUTES (OBJ_CASE_INSENSITIVE, OBJ_INHERIT if asked) naming the NT path of a
    /// Win32 one (RtlDosPathNameToNtPathName_U: \??\ and the full path); `relative`: a
    /// relative path under the current directory as its name under the directory's handle
    /// (RtlDosPathNameToRelativeNtPathName_U: CreateFile's and GetFileAttributesEx's, not
    /// GetFileAttributes': Windows' answers in winref nthook-32)
    fn attributes(&mut self, path: &str, inherit: bool, relative: bool) -> u32 {
        let (nt, root) = match relative.then(|| relative_name(path)).flatten() {
            Some((name, h)) => (name, h),
            None => (nt_path(path), 0),
        };
        let w: Vec<u16> = nt.encode_utf16().collect();
        let buf = self.alloc(2 * w.len() as u32 + 2);
        for (i, &x) in w.iter().enumerate() {
            mem::w16(buf + 2 * i as u32, x);
        }
        let us = self.alloc(8);
        mem::w16(us, 2 * w.len() as u16);
        mem::w16(us + 2, 2 * w.len() as u16 + 2);
        w32(us + 4, buf);
        let oa = self.alloc(24);
        w32(oa, 24);
        w32(oa + 4, root);
        w32(oa + 8, us);
        w32(oa + 12, 0x40 | if inherit { 2 } else { 0 });
        oa
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        for &p in &self.0 {
            mem::heap_free(p);
        }
    }
}

/// A relative Win32 path (no drive, not rooted) whose full path is under the current
/// directory: the rest of it and the current directory's handle.
fn relative_name(path: &str) -> Option<(String, u32)> {
    let full = crate::files::full_path(path);
    let (dir, h) = cwd_handle()?;
    relative_under(path, &full, &dir).map(|r| (r, h))
}

/// The name of `path` (its full path `full`) under directory `dir`, when the path is relative
/// and the full one is under `dir` (case-insensitive; ".." out of it: None).
fn relative_under(path: &str, full: &str, dir: &str) -> Option<String> {
    let b = path.as_bytes();
    if b.is_empty() || b[0] == b'\\' || b[0] == b'/' || b.get(1) == Some(&b':') {
        return None;
    }
    let pre = format!("{}\\", dir.trim_end_matches('\\'));
    let rest = full.get(pre.len()..).filter(|_| full.get(..pre.len()).is_some_and(|p| p.eq_ignore_ascii_case(&pre)))?;
    (!rest.is_empty()).then(|| rest.to_string())
}

/// The current directory's handle, as Windows keeps one in the process parameters: opened
/// for the directory it is now, again (the old one closed) once it changed.
fn cwd_handle() -> Option<(String, u32)> {
    static H: std::sync::Mutex<Option<(String, u32)>> = std::sync::Mutex::new(None);
    let dir = crate::files::cwd();
    let mut g = H.lock().unwrap();
    if let Some((d, h)) = &*g {
        if *d == dir {
            return Some((dir, *h));
        }
    }
    if let Some((_, h)) = g.take() {
        crate::obj::close(h);
    }
    let f = crate::files::open(&dir, 0x8000_0000, 3).ok().filter(|f| f.dir)?;
    let h = crate::obj::add(crate::obj::Obj::File(std::sync::Arc::new(f)));
    *g = Some((dir.clone(), h));
    Some((dir, h))
}

/// The NT name of a Win32 path: "\??\" and its full path (\\?\ kept as it is, \\server
/// as \??\UNC\server).
pub fn nt_path(path: &str) -> String {
    if let Some(r) = path.strip_prefix("\\\\?\\") {
        return format!("\\??\\{r}");
    }
    let full = crate::files::full_path(path);
    match full.strip_prefix("\\\\") {
        Some(r) => format!("\\??\\UNC\\{r}"),
        None => format!("\\??\\{full}"),
    }
}

/// The Win32 error kernel32 gives for a file function's NTSTATUS (RtlNtStatusToDosError).
pub fn file_error(st: u32) -> u32 {
    dos_error(st)
}

/// RtlNtStatusToDosError(NoTeb): the Win32 error for any NTSTATUS. 0 and customer codes (bit 29)
/// come back as they are; 0xd... is 0xc...; facility 1 (debugger), 7 (Win32) carry the error in
/// their low word; the rest by table (the codes programs meet: file, memory, loader, sync),
/// ERROR_MR_MID_NOT_FOUND (317) for a code with no Win32 error. (MSVC's CRT and std::filesystem
/// ask ntdll for it by GetProcAddress and give up when it's missing: the Edicius trial quit at
/// start with "GetProcAddress: (127)".)
pub fn dos_error(st: u32) -> u32 {
    if st == 0 || st & 0x2000_0000 != 0 {
        return st;
    }
    let st = if st & 0xf000_0000 == 0xd000_0000 { st & !0x1000_0000 } else { st };
    if matches!(st >> 16, 0xc001 | 0x8007 | 0xc007) {
        return st & 0xffff;
    }
    match st {
        0x0000_0102 => 1460, // TIMEOUT: ERROR_TIMEOUT
        0x0000_0103 => 997,  // PENDING: ERROR_IO_PENDING
        0x4000_0000 => 698,  // OBJECT_NAME_EXISTS
        0x8000_0005 => 234,  // BUFFER_OVERFLOW: ERROR_MORE_DATA
        0x8000_0006 => 18,   // NO_MORE_FILES
        0x8000_001a => 259,  // NO_MORE_ENTRIES: ERROR_NO_MORE_ITEMS
        0xc000_0001 => 31,   // UNSUCCESSFUL: ERROR_GEN_FAILURE
        0xc000_0002 | 0xc000_0010 => 1, // NOT_IMPLEMENTED, INVALID_DEVICE_REQUEST: ERROR_INVALID_FUNCTION
        0xc000_0003 | 0xc000_000d => 87, // INVALID_INFO_CLASS, INVALID_PARAMETER
        0xc000_0004 => 24,   // INFO_LENGTH_MISMATCH: ERROR_BAD_LENGTH
        0xc000_0005 => 998,  // ACCESS_VIOLATION: ERROR_NOACCESS
        0xc000_0008 | 0xc000_0024 => 6, // INVALID_HANDLE, OBJECT_TYPE_MISMATCH
        0xc000_000f | 0xc000_0034 => 2, // NO_SUCH_FILE, OBJECT_NAME_NOT_FOUND: ERROR_FILE_NOT_FOUND
        0xc000_0011 => 38,   // END_OF_FILE: ERROR_HANDLE_EOF
        0xc000_0017 => 8,    // NO_MEMORY: ERROR_NOT_ENOUGH_MEMORY
        0xc000_0022 | 0xc000_0056 | 0xc000_00ba => 5, // ACCESS_DENIED, DELETE_PENDING, FILE_IS_A_DIRECTORY
        0xc000_0023 => 122,  // BUFFER_TOO_SMALL: ERROR_INSUFFICIENT_BUFFER
        0xc000_0033 => 123,  // OBJECT_NAME_INVALID
        0xc000_0035 => 183,  // OBJECT_NAME_COLLISION: ERROR_ALREADY_EXISTS
        0xc000_003a => 3,    // OBJECT_PATH_NOT_FOUND
        0xc000_003b => 161,  // OBJECT_PATH_SYNTAX_BAD: ERROR_BAD_PATHNAME
        0xc000_0043 => 32,   // SHARING_VIOLATION
        0xc000_0046 => 288,  // MUTANT_NOT_OWNED: ERROR_NOT_OWNER
        0xc000_0047 => 298,  // SEMAPHORE_LIMIT_EXCEEDED: ERROR_TOO_MANY_POSTS
        0xc000_0061 => 1314, // PRIVILEGE_NOT_HELD
        0xc000_007a | 0xc000_0139 => 127, // PROCEDURE_NOT_FOUND, ENTRYPOINT_NOT_FOUND: ERROR_PROC_NOT_FOUND
        0xc000_007f => 112,  // DISK_FULL
        0xc000_009a => 1450, // INSUFFICIENT_RESOURCES: ERROR_NO_SYSTEM_RESOURCES
        0xc000_00bb => 50,   // NOT_SUPPORTED
        0xc000_0101 => 145,  // DIRECTORY_NOT_EMPTY: ERROR_DIR_NOT_EMPTY
        0xc000_0103 => 267,  // NOT_A_DIRECTORY: ERROR_DIRECTORY
        0xc000_0135 => 126,  // DLL_NOT_FOUND: ERROR_MOD_NOT_FOUND
        0xc000_0225 => 1168, // NOT_FOUND: ERROR_NOT_FOUND
        // (the pipe codes kern.rs names)
        st => crate::kern::win32(st),
    }
}

fn rtl_nt_status_to_dos_error(c: &mut Ctx) -> u32 {
    dos_error(c.a(0))
}

/// kernel32's file functions as Windows' kernelbase makes them out of ntdll's, for a program that
/// hooked ntdll (None: it did not, and kernel32 does the work itself).
pub mod via {
    use super::{file_error, hooked, Scratch};
    use crate::api::Ctx;
    use crate::k32::last_error;
    use crate::mem::{self, put32, r32, w32};

    /// CreateFileA/W(name, access, share, sa, disposition, flags and attributes, template)
    pub fn create_file(c: &mut Ctx, name: &str) -> Option<u32> {
        // (consoles, devices and pipes are not files kernel32 opens by NtCreateFile alone)
        let l = name.to_ascii_lowercase();
        if l.starts_with("\\\\.\\") || l == "conout$" || l == "conin$" || l == "con" || name.is_empty() {
            return None;
        }
        let f = hooked("NtCreateFile")?;
        let (access, share, sa, disp, fl) = (c.a(1), c.a(2), c.a(3), c.a(4), c.a(5));
        // CREATE_NEW, CREATE_ALWAYS, OPEN_EXISTING, OPEN_ALWAYS, TRUNCATE_EXISTING
        let nd = match disp {
            1 => 2,
            2 => 5,
            3 => 1,
            4 => 3,
            5 => 4,
            _ => {
                last_error(87);
                return Some(u32::MAX);
            }
        };
        // FILE_FLAG_*: OVERLAPPED (else FILE_SYNCHRONOUS_IO_NONALERT), BACKUP_SEMANTICS (else
        // FILE_NON_DIRECTORY_FILE), NO_BUFFERING, WRITE_THROUGH, RANDOM_ACCESS, SEQUENTIAL_SCAN,
        // DELETE_ON_CLOSE
        let mut opts = if fl & 0x4000_0000 == 0 { 0x20 } else { 0 };
        opts |= if fl & 0x0200_0000 != 0 { 0x4000 } else { 0x40 };
        for (bit, o) in [(0x2000_0000, 8), (0x8000_0000, 2), (0x1000_0000, 0x800), (0x0800_0000, 4), (0x0400_0000, 0x1000)] {
            if fl & bit != 0 {
                opts |= o;
            }
        }
        let mut s = Scratch::new();
        let inherit = sa != 0 && r32(sa + 8) != 0;
        let oa = s.attributes(name, inherit, true);
        let (out, io) = (s.alloc(4), s.alloc(8));
        let delete = if fl & 0x0400_0000 != 0 { 0x0001_0000 } else { 0 };
        let st = c.call(f, &[out, access | delete | 0x0010_0080, oa, io, 0, fl & 0x7fa7, share, nd, opts, 0, 0]);
        if st >= 0xc000_0000 {
            // (CREATE_NEW of a file there: ERROR_FILE_EXISTS)
            last_error(if st == 0xc000_0035 && disp == 1 { 80 } else { file_error(st) });
            return Some(u32::MAX);
        }
        // ERROR_ALREADY_EXISTS: CREATE_ALWAYS that overwrote (FILE_OVERWRITTEN), OPEN_ALWAYS that
        // opened (FILE_OPENED); a CREATE_ALWAYS that only opened says 0 (winref nthook-32)
        let info = r32(io + 4);
        last_error(if disp == 2 && info == 3 || disp == 4 && info == 1 { 183 } else { 0 });
        Some(r32(out))
    }

    /// ReadFile / WriteFile(h, buf, n, &done, overlapped)
    pub fn io(c: &mut Ctx, write: bool) -> Option<u32> {
        let f = hooked(if write { "NtWriteFile" } else { "NtReadFile" })?;
        let (h, buf, n, done, ov) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
        put32(done, 0);
        let mut s = Scratch::new();
        // (an OVERLAPPED is the IO_STATUS_BLOCK, its offset and event)
        let (io, off, ev) = if ov != 0 { (ov, ov + 8, r32(ov + 16)) } else { (s.alloc(8), 0, 0) };
        if ov != 0 {
            w32(ov, 0x103);
        }
        let st = c.call(f, &[h, ev, 0, if ov != 0 { ov } else { 0 }, io, buf, n, off, 0]);
        if st == 0x103 {
            last_error(997);
            return Some(0);
        }
        if st == 0xc000_0011 && ov == 0 && !write {
            return Some(1);
        }
        if st >= 0xc000_0000 {
            last_error(file_error(st));
            return Some(0);
        }
        put32(done, r32(io + 4));
        Some(1)
    }

    fn query(c: &mut Ctx, h: u32, class: u32, len: u32, s: &mut Scratch) -> Result<u32, u32> {
        let f = hooked("NtQueryInformationFile").unwrap_or(crate::api::export("ntdll", "NtQueryInformationFile"));
        let (io, out) = (s.alloc(8), s.alloc(len));
        let st = c.call(f, &[h, io, out, len, class]);
        if st >= 0xc000_0000 { Err(st) } else { Ok(out) }
    }

    /// The new position of a SetFilePointer(Ex): FILE_BEGIN, FILE_CURRENT, FILE_END; Err: the
    /// Win32 error
    fn seek(c: &mut Ctx, h: u32, d: i64, how: u32) -> Result<u64, u32> {
        let set = hooked("NtSetInformationFile").unwrap_or(crate::api::export("ntdll", "NtSetInformationFile"));
        let mut s = Scratch::new();
        let base = match how {
            0 => 0,
            // FilePositionInformation, FileStandardInformation's EndOfFile
            1 => mem::r64(query(c, h, 14, 8, &mut s).map_err(file_error)?) as i64,
            2 => mem::r64(query(c, h, 5, 24, &mut s).map_err(file_error)? + 8) as i64,
            _ => return Err(87),
        };
        let p = base.wrapping_add(d);
        if p < 0 {
            return Err(131); // ERROR_NEGATIVE_SEEK
        }
        let (io, b) = (s.alloc(8), s.alloc(8));
        mem::put64(b, p as u64);
        let st = c.call(set, &[h, io, b, 8, 14]);
        if st >= 0xc000_0000 {
            return Err(file_error(st));
        }
        Ok(p as u64)
    }

    /// SetFilePointer(h, distance, &high, method)
    pub fn set_file_pointer(c: &mut Ctx) -> Option<u32> {
        hooked("NtSetInformationFile").or_else(|| hooked("NtQueryInformationFile"))?;
        let (h, lo, phi, how) = (c.a(0), c.a(1), c.a(2), c.a(3));
        let d = if phi != 0 { (lo as u64 | (r32(phi) as u64) << 32) as i64 } else { lo as i32 as i64 };
        Some(match seek(c, h, d, how) {
            Ok(p) => {
                put32(phi, (p >> 32) as u32);
                p as u32
            }
            Err(e) => {
                last_error(e);
                u32::MAX
            }
        })
    }
    /// SetFilePointerEx(h, distance (2 dwords), &new, method)
    pub fn set_file_pointer_ex(c: &mut Ctx) -> Option<u32> {
        hooked("NtSetInformationFile").or_else(|| hooked("NtQueryInformationFile"))?;
        let (h, d, out, how) = (c.a(0), (c.a(1) as u64 | (c.a(2) as u64) << 32) as i64, c.a(3), c.a(4));
        Some(match seek(c, h, d, how) {
            Ok(p) => {
                if out != 0 {
                    mem::put64(out, p);
                }
                1
            }
            Err(e) => {
                last_error(e);
                0
            }
        })
    }
    /// GetFileSize(h, &high) / GetFileSizeEx(h, &size): FileStandardInformation's EndOfFile
    pub fn file_size(c: &mut Ctx, ex: bool) -> Option<u32> {
        hooked("NtQueryInformationFile")?;
        let (h, p) = (c.a(0), c.a(1));
        let mut s = Scratch::new();
        Some(match query(c, h, 5, 24, &mut s) {
            Ok(i) => {
                let n = mem::r64(i + 8);
                if ex {
                    mem::put64(p, n);
                    1
                } else {
                    put32(p, (n >> 32) as u32);
                    n as u32
                }
            }
            Err(st) => {
                last_error(file_error(st));
                if ex { 0 } else { u32::MAX }
            }
        })
    }
    /// CloseHandle(h): NtClose
    pub fn close(c: &mut Ctx) -> Option<u32> {
        let f = hooked("NtClose")?;
        let st = c.call(f, &[c.a(0)]);
        if st >= 0xc000_0000 {
            last_error(file_error(st));
            return Some(0);
        }
        Some(1)
    }
    /// GetFileAttributesA/W(name): NtQueryAttributesFile's FileAttributes
    pub fn attributes(c: &mut Ctx, name: &str) -> Option<u32> {
        let f = hooked("NtQueryAttributesFile")?;
        let mut s = Scratch::new();
        let (oa, out) = (s.attributes(name, false, false), s.alloc(40));
        let st = c.call(f, &[oa, out]);
        if st >= 0xc000_0000 {
            last_error(file_error(st));
            return Some(u32::MAX);
        }
        Some(r32(out + 32))
    }
    /// GetFileAttributesExA/W(name, GetFileExInfoStandard, WIN32_FILE_ATTRIBUTE_DATA*):
    /// NtQueryFullAttributesFile's FILE_NETWORK_OPEN_INFORMATION
    pub fn attributes_ex(c: &mut Ctx, name: &str) -> Option<u32> {
        if c.a(1) != 0 {
            return None;
        }
        let f = hooked("NtQueryFullAttributesFile")?;
        let p = c.a(2);
        let mut s = Scratch::new();
        let (oa, out) = (s.attributes(name, false, true), s.alloc(56));
        let st = c.call(f, &[oa, out]);
        if st >= 0xc000_0000 {
            last_error(file_error(st));
            return Some(0);
        }
        w32(p, r32(out + 48));
        mem::copy(p + 4, out, 24);
        w32(p + 28, r32(out + 44));
        w32(p + 32, r32(out + 40));
        Some(1)
    }
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
    for d in ['C', 'D', 'Z'] {
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
    // (a directory opens unless FILE_NON_DIRECTORY_FILE: CreateFile's FILE_FLAG_BACKUP_SEMANTICS,
    // which kernel32 passes as FILE_OPEN_FOR_BACKUP_INTENT)
    if opts & 0x40 == 0 {
        flags |= 0x0200_0000;
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
    if let Some(h) = afd_name(a[2]).and_then(|n| crate::afd::open(&n)) {
        mem::wp(a[0], h);
        iosb(a[3], 0, 1); // FILE_OPENED
        return 0;
    }
    create(c, a[0], a[1], a[2], a[3], a[5], a[6], a[7], a[8])
}
/// OBJECT_ATTRIBUTES' name when it is a device's ("\Device\..."), for afd.rs
fn afd_name(oa: u32) -> Option<String> {
    if oa == 0 {
        return None;
    }
    let us = mem::rp(oa + 2 * mem::ps());
    if us == 0 {
        return None;
    }
    let (n, b) = (r16(us) as u32 / 2, mem::rp(us + mem::ps()));
    let s = String::from_utf16_lossy(&(0..n).map(|i| r16(b + 2 * i)).collect::<Vec<_>>());
    s.to_ascii_lowercase().starts_with("\\device\\").then_some(s)
}

/// NtDeviceIoControlFile(h, event, apc, apcctx, iosb, code, in, inlen, out, outlen): AFD's poll
/// (afd.rs), else kernel32's DeviceIoControl, synchronously
fn nt_device_io_control_file(c: &mut Ctx) -> u32 {
    if let Some(st) = crate::afd::ioctl(c) {
        return st;
    }
    let (h, event, io, code, inp, inlen, out, outlen) = (c.a(0), c.a(1), c.a(4), c.a(5), c.a(6), c.a(7), c.a(8), c.a(9));
    let n = mem::heap_alloc(4, true);
    let ok = k32(c, "DeviceIoControl", &[h, code, inp, inlen, out, outlen, n, 0]);
    let got = r32(n);
    mem::heap_free(n);
    let st = if ok != 0 { 0 } else { status() };
    iosb(io, st, got);
    if event != 0 && st < 0xc000_0000 {
        crate::k32::set_event(event);
    }
    st
}

/// NtQueryVolumeInformationFile's answer for FsInformationClass `class` on a handle whose
/// GetFileType is `ty`, the volume kernel32's GetVolumeInformation / GetDiskFreeSpace describe
/// (label SAKE, serial 12345678, NTFS, 8 sectors of 512 bytes a cluster, 0x200_0000 clusters of
/// which 0x100_0000 free). Err: the NTSTATUS for a class answered nowhere here.
pub fn fs_info(class: u32, ty: u32) -> Result<Vec<u8>, u32> {
    let w = |s: &str| s.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<u8>>();
    let (sec, bps, total, free) = (8u32, 512u32, 0x200_0000u64, 0x100_0000u64);
    let mut v = Vec::new();
    match class {
        // FileFsVolumeInformation: creation time, serial, label length, supports objects, label
        1 => {
            let label = w("SAKE");
            v.extend(0u64.to_le_bytes());
            v.extend(0x1234_5678u32.to_le_bytes());
            v.extend((label.len() as u32).to_le_bytes());
            v.extend([0, 0]);
            v.extend(label);
        }
        // FileFsSizeInformation: total and available clusters, sectors a cluster, bytes a sector
        3 => {
            v.extend(total.to_le_bytes());
            v.extend(free.to_le_bytes());
            v.extend(sec.to_le_bytes());
            v.extend(bps.to_le_bytes());
        }
        // FileFsDeviceInformation: DeviceType (FILE_DEVICE_DISK; a pipe's, a console's), Characteristics
        4 => {
            let dev: u32 = match ty {
                3 => 0x11, // FILE_DEVICE_NAMED_PIPE
                2 => 0x50, // FILE_DEVICE_CONSOLE
                _ => 7,
            };
            v.extend(dev.to_le_bytes());
            v.extend(0u32.to_le_bytes());
        }
        // FileFsAttributeInformation: attributes (as GetVolumeInformation's), longest name, name
        5 => {
            let name = w("NTFS");
            v.extend(3u32.to_le_bytes());
            v.extend(255u32.to_le_bytes());
            v.extend((name.len() as u32).to_le_bytes());
            v.extend(name);
        }
        // FileFsFullSizeInformation: total, caller's and actual available clusters, geometry
        7 => {
            v.extend(total.to_le_bytes());
            v.extend(free.to_le_bytes());
            v.extend(free.to_le_bytes());
            v.extend(sec.to_le_bytes());
            v.extend(bps.to_le_bytes());
        }
        _ => return Err(0xc000_0003), // STATUS_INVALID_INFO_CLASS
    }
    Ok(v)
}

/// The fixed part of class `class`'s structure (less than this: STATUS_INFO_LENGTH_MISMATCH;
/// the variable name past it is cut with STATUS_BUFFER_OVERFLOW)
pub fn fs_info_fixed(class: u32) -> u32 {
    match class {
        1 => 18,
        5 => 12,
        3 => 24,
        4 => 8,
        7 => 32,
        _ => 0,
    }
}

/// NtQueryVolumeInformationFile(h, iosb, out, len, class)
fn nt_query_volume_information_file(c: &mut Ctx) -> u32 {
    let (h, io, out, len, class) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let ty = k32(c, "GetFileType", &[h]);
    if ty == 0 && crate::k32::get_last_error() != 0 {
        return 0xc000_0008; // STATUS_INVALID_HANDLE
    }
    let v = match fs_info(class, ty) {
        Ok(v) => v,
        Err(st) => return st,
    };
    if len < fs_info_fixed(class) {
        return 0xc000_0004; // STATUS_INFO_LENGTH_MISMATCH
    }
    let n = (v.len() as u32).min(len);
    for (i, b) in v[..n as usize].iter().enumerate() {
        mem::w8(out + i as u32, *b);
    }
    let st = if n < v.len() as u32 { 0x8000_0005 } else { 0 }; // STATUS_BUFFER_OVERFLOW
    iosb(io, st, n);
    st
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

/// NtSetInformationThread(h, class, buf, len): ThreadAffinityMask (all bits: the process's)
/// and ThreadGroupInformation through crate::affinity, the rest accepted
/// (ThreadHideFromDebugger and the like).
fn nt_set_information_thread(c: &mut Ctx) -> u32 {
    let (h, class, buf, len) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let Some(tid) = crate::k32::any_thread_tid(h) else { return 0xC000_0008 };
    let ps = mem::ps();
    match class {
        4 => {
            if len != ps {
                return 0xC000_0004;
            }
            let m = mem::rp(buf);
            crate::affinity::with(|a| a.set_thread(tid, m)).map_or(0xC000_000D, |_| 0)
        }
        30 => {
            if len != ps + 8 {
                return 0xC000_0004;
            }
            if buf == 0 || mem::accessible(buf, len, false, false) < len {
                return 0xC000_0005;
            }
            if mem::r16(buf + ps) != 0 {
                return 0xC000_000D;
            }
            crate::affinity::with(|a| a.set_group(tid, mem::rp(buf))).map_or(0xC000_000D, |_| 0)
        }
        38 => set_name_info(tid, buf, len),
        _ => 0,
    }
}
/// Thread descriptions (SetThreadDescription, ThreadNameInformation), by thread id
static NAMES: std::sync::Mutex<Option<crate::HashMap<u32, Vec<u16>>>> = std::sync::Mutex::new(None);
pub fn thread_name(tid: u32) -> Vec<u16> {
    NAMES.lock().unwrap().get_or_insert_with(Default::default).get(&tid).cloned().unwrap_or_default()
}
pub fn set_thread_name(tid: u32, name: Vec<u16>) {
    let mut g = NAMES.lock().unwrap();
    let m = g.get_or_insert_with(Default::default);
    if name.is_empty() {
        m.remove(&tid);
    } else {
        m.insert(tid, name);
    }
}
/// ThreadNameInformation's set: a UNICODE_STRING (exactly its size), its characters readable
fn set_name_info(tid: u32, buf: u32, len: u32) -> u32 {
    let ps = mem::ps();
    if len != 2 * ps {
        return 0xC000_0004;
    }
    if buf == 0 || mem::accessible(buf, len, false, false) < len {
        return 0xC000_0005;
    }
    let (n, p) = (r16(buf) as u32, mem::rp(buf + ps));
    if n != 0 && (p == 0 || mem::accessible(p, n, false, false) < n) {
        return 0xC000_0005;
    }
    set_thread_name(tid, (0..n / 2).map(|i| r16(p + 2 * i)).collect());
    0
}
/// GetThreadDescription(thread, &string): a LocalAlloc'd copy; SetThreadDescription(thread,
/// string): NULL clears it. Both HRESULT_FROM_NT (0x10000000 | status).
fn get_thread_description(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(0), c.a(1));
    let Some(tid) = crate::k32::any_thread_tid(h) else { return 0x1000_0000 | 0xC000_0008 };
    if !crate::obj::allows_any(h, 0x40 | 0x800) {
        return 0x1000_0000 | 0xC000_0022;
    }
    let name = thread_name(tid);
    let p = crate::k32::global_alloc(0x40, 2 * name.len() as u32 + 2);
    for (i, &u) in name.iter().enumerate() {
        mem::w16(p + 2 * i as u32, u);
    }
    mem::putp(out, p);
    0x1000_0000
}
fn set_thread_description(c: &mut Ctx) -> u32 {
    let (h, s) = (c.a(0), c.a(1));
    let Some(tid) = crate::k32::any_thread_tid(h) else { return 0x1000_0000 | 0xC000_0008 };
    if !crate::obj::allows_any(h, 0x20 | 0x400) {
        return 0x1000_0000 | 0xC000_0022;
    }
    set_thread_name(tid, if s == 0 { vec![] } else { mem::wstr(s) });
    0x1000_0000
}

/// NtQueryInformationThread(h, class, buf, len, &retlen): basic information, times, whether it
/// is the last thread, the priority boost, I/O pending, hidden from the debugger, the group
/// affinity. A handle opened with THREAD_QUERY_LIMITED_INFORMATION only gets the first four
/// (kernel32:thread test_thread_info); others need THREAD_QUERY_INFORMATION.
fn nt_query_information_thread(c: &mut Ctx) -> u32 {
    let (h, class, out, len, ret) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    let Some(tid) = crate::k32::any_thread_tid(h) else { return 0xC000_0008 };
    let limited = matches!(class, 0 | 1 | 12 | 14 | 38);
    if !crate::obj::allows(h, 0x40) && !(limited && crate::obj::allows(h, 0x800)) {
        return 0xC000_0022; // STATUS_ACCESS_DENIED
    }
    let (x64, ps) = (mem::x64(), mem::ps());
    if class == 38 {
        // THREAD_NAME_INFORMATION: a UNICODE_STRING, its characters after it
        let name = thread_name(tid);
        let need = 2 * ps + 2 * name.len() as u32;
        if ret != 0 {
            w32(ret, need);
        }
        if len < need {
            return 0xC000_0023; // STATUS_BUFFER_TOO_SMALL
        }
        let n = 2 * name.len() as u16;
        mem::w16(out, n);
        mem::w16(out + 2, n);
        mem::wp(out + ps, out + 2 * ps);
        for (i, &u) in name.iter().enumerate() {
            mem::w16(out + 2 * ps + 2 * i as u32, u);
        }
        return 0;
    }
    let need = match class {
        0 => if x64 { 48 } else { 28 },
        1 => 32,
        12 | 14 | 16 => 4,
        17 => 1,
        30 => ps + 8,
        _ => return 0xC000_0003, // STATUS_INVALID_INFO_CLASS
    };
    if len < need {
        return 0xC000_0004;
    }
    mem::fill(out, need, 0);
    match class {
        0 => {
            // THREAD_BASIC_INFORMATION: ExitStatus, TebBaseAddress, ClientId (process, thread),
            // AffinityMask, Priority, BasePriority
            let code = crate::obj::with(|t| match t.objs.get(&crate::obj::norm(h)) {
                Some((crate::obj::Obj::Thread { code, .. }, _)) => *code,
                _ => 0x103,
            });
            let teb = if h == crate::obj::CURRENT_THREAD { crate::cur().teb } else { crate::k32::teb_of(tid).unwrap_or(0) };
            w32(out, code);
            mem::wp(out + ps, teb);
            mem::wp(out + 2 * ps, crate::pid());
            mem::wp(out + 3 * ps, tid);
            mem::wp(out + 4 * ps, crate::affinity::with(|a| a.thread(tid)));
            w32(out + 5 * ps, 8);
            w32(out + 5 * ps + 4, 8);
        }
        12 => w32(out, (crate::k32::thread_count() <= 1) as u32),
        14 => w32(out, crate::k32::boost_off(tid) as u32),
        30 => mem::wp(out, crate::affinity::with(|a| a.thread(tid))),
        _ => {}
    }
    if ret != 0 {
        w32(ret, need);
    }
    0
}
/// NtQueryInformationProcess(h, class, buf, len, &retlen)
fn nt_query_information_process(c: &mut Ctx) -> u32 {
    let (class, out, len, ret) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let ps = mem::ps();
    // (whether a debugger debugs the process: crate::dbg)
    let debugged = || crate::process::of_handle(c.a(0)).is_some_and(|(pid, _)| crate::dbg::debugged_pid(pid));
    let (need, st) = match class {
        0 => (24, 0),            // ProcessBasicInformation
        7 => (ps, 0),            // ProcessDebugPort
        0x1e => (ps, if debugged() { 0 } else { 0xc000_0353 }), // ProcessDebugObjectHandle: STATUS_PORT_NOT_SET
        0x1f => (4, 0),          // ProcessDebugFlags (NoDebugInherit)
        0x1a => (mem::ps(), 0),  // ProcessWow64Information: ULONG_PTR
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
        7 if debugged() => mem::wp(out, u32::MAX),
        0x1e if debugged() => mem::wp(out, crate::obj::add(crate::obj::Obj::Event { manual: true, set: false })),
        0x1f => w32(out, !debugged() as u32),
        // (a 32-bit process is a WoW64 one, k32::is_wow64: its 32-bit PEB's address; a 64-bit
        // one 0)
        0x1a if crate::k32::is_wow64(c.a(0)) == Some(true) => w32(out, crate::PEB.load(std::sync::atomic::Ordering::Relaxed)),
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

/// The NT name of a file object (NtQueryObject's ObjectNameInformation): the device of its
/// drive (QueryDosDevice's) and the rest of its Win32 path; a drive's root ends in "\".
/// None: not on one of our drives.
pub fn object_name(win: &str) -> Option<String> {
    let mut ch = win.chars();
    let d = ch.next()?;
    if ch.next() != Some(':') {
        return None;
    }
    let dev = crate::k32::drive_device(d)?;
    let rest = &win[2..];
    let rest = if rest.is_empty() { "\\" } else { rest };
    // (no trailing "\" but the root's: Windows names a directory without one)
    let rest = if rest.len() > 1 { rest.trim_end_matches('\\') } else { rest };
    let rest = if rest.is_empty() { "\\" } else { rest };
    Some(format!("{dev}{rest}"))
}

/// The type name of an object (NtQueryObject's ObjectTypeInformation).
fn type_name(o: &crate::obj::Obj) -> &'static str {
    use crate::obj::Obj;
    match o {
        Obj::Event { .. } | Obj::Shared(_) => "Event",
        Obj::Timer { .. } => "Timer",
        Obj::Mutex { .. } => "Mutant",
        Obj::Semaphore { .. } => "Semaphore",
        Obj::Thread { .. } | Obj::RemoteThread { .. } => "Thread",
        Obj::Process { .. } => "Process",
        Obj::File(_) | Obj::Pipe(_) | Obj::Find(_) => "File",
        Obj::Mapping { .. } => "Section",
        Obj::Key(_) => "Key",
        Obj::DebugObject(_) => "DebugObject",
        Obj::Other(n) => n,
    }
}

/// NtQueryObject(h, class, buf, len, &retlen): ObjectBasicInformation (0), ObjectNameInformation
/// (1: a file's NT name, "" for an unnamed object) and ObjectTypeInformation (2). Enigma Virtual
/// Box's NtCreateFile hook asks it for the name of the root directory handle that kernel32
/// passes with a relative path (ane_ane_trial.exe called a null pointer while sake's ntdll had
/// no NtQueryObject).
fn nt_query_object(c: &mut Ctx) -> u32 {
    let (h, class, buf, len, ret) = (c.a(0), c.a(1), c.a(2), c.a(3), c.a(4));
    // (the current process and thread pseudo-handles)
    let info = match h {
        0xffff_ffff => Some(("Process", String::new())),
        0xffff_fffe => Some(("Thread", String::new())),
        _ => crate::obj::with(|t| {
            t.objs.get(&h).map(|(o, _)| {
                let name = match o {
                    crate::obj::Obj::File(f) => object_name(&crate::files::host_to_win(&f.path)).unwrap_or_default(),
                    _ => String::new(),
                };
                (type_name(o), name)
            })
        }),
    };
    let Some((ty, name)) = info else { return 0xc000_0008 }; // STATUS_INVALID_HANDLE
    let ps = mem::ps();
    // a UNICODE_STRING (2 lengths, a pointer) and its characters after the structure
    let ustr = |at: u32, s: &str, base: u32| {
        let w: Vec<u16> = s.encode_utf16().collect();
        mem::w16(at, 2 * w.len() as u16);
        mem::w16(at + 2, if w.is_empty() { 0 } else { 2 * w.len() as u16 + 2 });
        mem::wp(at + ps, if w.is_empty() { 0 } else { base });
        for (i, &x) in w.iter().enumerate() {
            mem::w16(base + 2 * i as u32, x);
        }
        if !w.is_empty() {
            mem::w16(base + 2 * w.len() as u32, 0);
        }
    };
    let chars = |s: &str| 2 * s.encode_utf16().count() as u32;
    let us = 2 * ps; // sizeof(UNICODE_STRING)
    let need = match class {
        0 => 56,
        1 => us + if name.is_empty() { 0 } else { chars(&name) + 2 },
        // OBJECT_TYPE_INFORMATION (0x60, x64 0x68) and the name
        2 => (if mem::x64() { 0x68 } else { 0x60 }) + chars(ty) + 2,
        _ => return 0xc000_0003, // STATUS_INVALID_INFO_CLASS
    };
    put32(ret, need);
    if len < need {
        return 0xc000_0004; // STATUS_INFO_LENGTH_MISMATCH
    }
    mem::fill(buf, need, 0);
    match class {
        0 => {
            // Attributes, GrantedAccess, HandleCount, PointerCount
            w32(buf + 8, 1);
            w32(buf + 12, 1);
        }
        1 => ustr(buf, &name, buf + us),
        _ => ustr(buf, ty, buf + need - chars(ty) - 2),
    }
    0
}

pub fn init() {
    for d in ["kernel32", "kernelbase"] {
        implement(d, "GetThreadDescription", get_thread_description);
        implement(d, "SetThreadDescription", set_thread_description);
    }
    implement("ntdll", "RtlNtStatusToDosError", rtl_nt_status_to_dos_error);
    implement("ntdll", "RtlNtStatusToDosErrorNoTeb", rtl_nt_status_to_dos_error);
    for (n, f) in [
        ("SetInformationThread", nt_set_information_thread as crate::api::Handler),
        ("QueryInformationThread", nt_query_information_thread),
        ("QueryInformationProcess", nt_query_information_process),
        ("QueryObject", nt_query_object),
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
            ("DeviceIoControlFile", nt_device_io_control_file),
            ("QueryVolumeInformationFile", nt_query_volume_information_file),
        ] {
            implement(d, &format!("Nt{n}"), f);
            implement(d, &format!("Zw{n}"), f);
        }
    }
}

#[cfg(test)]
mod route_tests {
    use super::{file_error, nt_path, object_name, relative_under};
    use proptest::prelude::*;

    #[test]
    fn object_names_of_files() {
        // (NtQueryObject ObjectNameInformation: the drive's device and the rest; the root ends in
        // "\", a directory does not; winref ntobject)
        assert_eq!(object_name("C:\\game\\a.dat").as_deref(), Some("\\Device\\HarddiskVolume1\\game\\a.dat"));
        assert_eq!(object_name("Z:\\home\\x\\").as_deref(), Some("\\Device\\HarddiskVolume2\\home\\x"));
        assert_eq!(object_name("C:\\").as_deref(), Some("\\Device\\HarddiskVolume1\\"));
        assert_eq!(object_name("C:").as_deref(), Some("\\Device\\HarddiskVolume1\\"));
        assert_eq!(object_name("z:\\a").as_deref(), Some("\\Device\\HarddiskVolume2\\a"));
        // (the empty CD-ROM drive, k32::CD_DRIVE: its device as Windows names it)
        assert_eq!(object_name("D:\\x").as_deref(), Some("\\Device\\CdRom0\\x"));
        for p in ["E:\\x", "\\\\server\\share", "", "C", "\u{3042}:\\x"] {
            assert_eq!(object_name(p), None, "{p}");
        }
    }

    proptest! {
        // any path on C: or Z: (Japanese too): its NT name is the device and the path after the
        // drive, without a trailing "\" but the root's, and the device read back the way
        // path_of reads a device path gives the same file
        #[test]
        fn object_names_map_back(drive in prop::sample::select(vec!['C', 'Z', 'c', 'z']),
                                 segs in prop::collection::vec("[^\\\\/:\u{0}]{1,12}", 0..5), slash in any::<bool>()) {
            let rest = segs.join("\\");
            let win = format!("{drive}:\\{rest}{}", if slash && !rest.is_empty() { "\\" } else { "" });
            let n = object_name(&win).unwrap();
            let dev = crate::k32::drive_device(drive).unwrap();
            prop_assert!(n.starts_with(dev));
            let tail = &n[dev.len()..];
            prop_assert_eq!(tail, format!("\\{rest}"));
            prop_assert!(!tail.ends_with('\\') || tail == "\\");
            let back = format!("{}:{tail}", drive.to_ascii_uppercase());
            prop_assert_eq!(back.to_lowercase(), win.trim_end_matches('\\').to_lowercase() + if rest.is_empty() { "\\" } else { "" });
        }
    }

    #[test]
    fn relative_names_under_the_current_directory() {
        // (RtlDosPathNameToRelativeNtPathName_U, as CreateFile passed them in winref nthook-32)
        let d = "C:\\Game\\";
        assert_eq!(relative_under("a.dat", "C:\\Game\\a.dat", d).as_deref(), Some("a.dat"));
        assert_eq!(relative_under("sub\\a.dat", "C:\\Game\\sub\\a.dat", d).as_deref(), Some("sub\\a.dat"));
        assert_eq!(relative_under(".\\a.dat", "C:\\game\\a.dat", d).as_deref(), Some("a.dat"));
        assert_eq!(relative_under("a.dat", "C:\\Game\\a.dat", "C:\\Game").as_deref(), Some("a.dat"));
        // a full path, a rooted one, another drive's, or one out of the directory: the full name
        for (p, f) in [("C:\\Game\\a.dat", "C:\\Game\\a.dat"), ("\\Game\\a.dat", "C:\\Game\\a.dat"), ("D:a.dat", "D:\\a.dat"), ("..\\a.dat", "C:\\a.dat"), ("/x", "C:\\x"), ("", "C:\\Game\\")] {
            assert_eq!(relative_under(p, f, d), None, "{p}");
        }
        // (the directory itself has no name under it)
        assert_eq!(relative_under(".", "C:\\Game\\", d), None);
    }

    #[test]
    fn nt_names_of_win32_paths() {
        // (RtlDosPathNameToNtPathName_U: \\?\ passes as it is, a share is under UNC)
        assert_eq!(nt_path("C:\\game\\a.dat"), "\\??\\C:\\game\\a.dat");
        assert_eq!(nt_path("\\\\?\\C:\\x"), "\\??\\C:\\x");
        assert_eq!(nt_path("\\\\server\\share\\f"), "\\??\\UNC\\server\\share\\f");
    }

    #[test]
    fn file_errors_as_rtl_nt_status_to_dos_error() {
        for (st, e) in [
            (0xc000_0034, 2),
            (0xc000_003a, 3),
            (0xc000_0033, 123),
            (0xc000_0022, 5),
            (0xc000_00ba, 5),
            (0xc000_0043, 32),
            (0xc000_0035, 183),
            (0xc000_0008, 6),
            (0xc000_0011, 38),
            (0xc000_0103, 267),
            (0xc000_000d, 87),
            (0xc000_0001, 31),
            (0x0000_0103, 997),
        ] {
            assert_eq!(file_error(st), e, "{st:#x}");
        }
    }
}

#[cfg(test)]
mod fs_info_tests {
    use super::{fs_info, fs_info_fixed};
    use proptest::prelude::*;
    #[test]
    fn layouts() {
        // (sizes as Windows' structures with these names: SAKE, NTFS)
        assert_eq!(fs_info(1, 1).unwrap().len(), 26);
        assert_eq!(fs_info(3, 1).unwrap().len(), 24);
        assert_eq!(fs_info(4, 1).unwrap(), [7, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(fs_info(4, 3).unwrap()[0], 0x11);
        assert_eq!(fs_info(5, 1).unwrap().len(), 20);
        assert_eq!(&fs_info(5, 1).unwrap()[12..], &[b'N', 0, b'T', 0, b'F', 0, b'S', 0]);
        assert_eq!(fs_info(7, 1).unwrap().len(), 32);
        assert_eq!(fs_info(2, 1), Err(0xc000_0003));
    }
    proptest! {
        // every answered class is at least its fixed part, a name's length field matches the
        // bytes after it, and the sizes agree with GetDiskFreeSpace's (8 x 512, 0x200_0000 / 0x100_0000)
        #[test]
        fn consistent(class in 0u32..12, ty in 0u32..4) {
            match fs_info(class, ty) {
                Ok(v) => {
                    prop_assert!(v.len() as u32 >= fs_info_fixed(class));
                    let u32at = |o: usize| u32::from_le_bytes(v[o..o + 4].try_into().unwrap());
                    let u64at = |o: usize| u64::from_le_bytes(v[o..o + 8].try_into().unwrap());
                    match class {
                        1 => prop_assert_eq!(u32at(12) as usize, v.len() - 18),
                        5 => prop_assert_eq!(u32at(8) as usize, v.len() - 12),
                        3 => prop_assert_eq!((u64at(0), u64at(8), u32at(16), u32at(20)), (0x200_0000, 0x100_0000, 8, 512)),
                        7 => prop_assert_eq!((u64at(0), u64at(8), u64at(16), u32at(24), u32at(28)), (0x200_0000, 0x100_0000, 0x100_0000, 8, 512)),
                        _ => {}
                    }
                }
                Err(st) => {
                    prop_assert_eq!(st, 0xc000_0003);
                    prop_assert_eq!(fs_info_fixed(class), 0);
                }
            }
        }
    }
}

#[cfg(test)]
mod dos_error_tests {
    use super::dos_error;
    use proptest::prelude::*;
    #[test]
    fn known_codes() {
        // (status, Windows' error)
        for (st, e) in [
            (0, 0), (0x103, 997), (0x102, 1460), (0x8000_0005, 234), (0x8000_001a, 259), (0xc000_0001, 31),
            (0xc000_0005, 998), (0xc000_0017, 8), (0xc000_0022, 5), (0xc000_0034, 2), (0xc000_003a, 3),
            (0xc000_0135, 126), (0xc000_0139, 127), (0xc000_014b, 109), (0xc000_00bb, 50),
            (0x8007_0005, 5), (0xc007_0002, 2), (0xc001_0003, 3), (0xd000_0034, 2),
            (0xe000_1234, 0xe000_1234), (0xc000_ffff, 317), (0x4000_0001, 317),
        ] {
            assert_eq!(dos_error(st), e, "{st:#x}");
        }
    }
    proptest! {
        // facility Win32 (7) and the debugger's (c001): the low word, whatever it is
        #[test]
        fn facility_codes_carry_the_error(lo in 0u32..0x10000, hi in prop::sample::select(vec![0x8007u32, 0xc007, 0xc001])) {
            prop_assert_eq!(dos_error(hi << 16 | lo), lo);
        }
        // customer codes come back unchanged; 0xd... answers as 0xc...
        #[test]
        fn customer_and_d_codes(st in any::<u32>()) {
            if st & 0x2000_0000 != 0 { prop_assert_eq!(dos_error(st), st); }
            if st & 0xf000_0000 == 0xd000_0000 { prop_assert_eq!(dos_error(st), dos_error(st & !0x1000_0000)); }
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
