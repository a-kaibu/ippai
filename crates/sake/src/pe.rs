//! PE images: mapping, relocations, imports, exports, resources, TLS.

use crate::HashMap;
use std::sync::{Mutex, RwLock};

use crate::mem::{self, r16, r32, w32};

pub struct Module {
    pub base: u32,
    pub size: u32,
    pub entry: u32,
    /// lower-case file name ("reunion.exe")
    pub name: String,
    /// Windows path
    pub path: String,
    pub exports: HashMap<String, u32>,
    pub ordinals: HashMap<u32, u32>,
    pub dll: bool,
    pub tls: Option<Tls>,
    pub resources: u32,
    pub refs: u32,
    /// one of ours (no code: GetProcAddress gives trap addresses)
    pub hle: bool,
    /// x64: the exception directory (RUNTIME_FUNCTION array, count)
    pub pdata: (u32, u32),
    /// loaded from a side-by-side assembly (LDR_REDIRECTED): its name finds it only while
    /// that activation context is active
    pub redirected: bool,
    /// the DLL files its imports hold a reference of (released when it is unloaded)
    pub deps: Vec<u32>,
    /// GetModuleHandleEx(PIN): never unloaded
    pub pinned: bool,
}

/// The 64-bit address an image mapped at `base` (preferring `pref`) has.
fn widen_base(base: u32, pref: u64) -> u64 {
    if base as u64 != pref && base == pref as u32 { pref } else { base as u64 }
}

#[derive(Clone, Copy)]
pub struct Tls {
    pub start: u32,
    pub end: u32,
    pub index_addr: u32,
    pub callbacks: u32,
    pub zero_fill: u32,
    pub index: u32,
}

pub static MODULES: RwLock<Vec<Module>> = RwLock::new(Vec::new());
// Windows holds the loader lock across TLS callbacks and DllMain, including
// DLL_THREAD_ATTACH. Delphi's DLL entry uses a shared initialization context:
// concurrent callbacks can corrupt it and call ExitProcess(0) from a worker.
// The holder's thread and how many times it took the lock (DllMain may itself call LoadLibrary:
// the same thread takes it again, other threads wait until that whole initialization ends).
static LOADER: Mutex<(Option<std::thread::ThreadId>, u32)> = Mutex::new((None, 0));
static LOADER_FREE: std::sync::Condvar = std::sync::Condvar::new();
struct LoaderGuard;
impl Drop for LoaderGuard {
    fn drop(&mut self) {
        let me = std::thread::current().id();
        let mut l = LOADER.lock().unwrap();
        if l.0 == Some(me) && l.1 > 0 {
            l.1 -= 1;
            if l.1 == 0 {
                l.0 = None;
                LOADER_FREE.notify_all();
            }
        }
    }
}
fn loader_lock() -> LoaderGuard {
    let me = std::thread::current().id();
    let mut l = LOADER.lock().unwrap();
    if l.0 == Some(me) {
        l.1 += 1;
        return LoaderGuard;
    }
    while l.1 != 0 {
        l = LOADER_FREE.wait(l).unwrap();
    }
    *l = (Some(me), 1);
    LoaderGuard
}
/// The calling thread ends (ExitThread, also from inside a DllMain or TLS callback): the loader
/// lock it holds goes, as its guards never drop (pthread_exit), or every later LoadLibrary and
/// thread start would wait forever.
pub fn loader_thread_exit() {
    let me = std::thread::current().id();
    let mut l = LOADER.lock().unwrap();
    if l.0 == Some(me) {
        *l = (None, 0);
        LOADER_FREE.notify_all();
    }
}

/// (a data-file module's handle is its base | 1 or | 2)
pub fn by_base(base: u32) -> Option<usize> {
    MODULES.read().unwrap().iter().position(|m| m.base == base & !3)
}

/// The module loaded from the file at Windows path `path` (a path-qualified LoadLibrary is
/// matched by the whole path: same-named DLLs of different folders are different modules, as
/// Ren'Py 6.99/7's renpy\\gl\\gldraw.pyd and renpy\\angle\\gldraw.pyd).
pub fn by_path(path: &str) -> Option<u32> {
    MODULES.read().unwrap().iter().find(|m| !m.redirected && m.path.eq_ignore_ascii_case(path)).map(|m| m.base)
}

pub fn by_name(name: &str) -> Option<u32> {
    let n = name.rsplit(['\\', '/']).next().unwrap_or(name);
    MODULES.read().unwrap().iter().find(|m| !m.redirected && name_matches(&m.name, n)).map(|m| m.base)
}
/// Whether module name `m` (lower case) is base name `n` in any case, or `n` is it without ".dll"
/// (by_name without a lower-cased copy per call: nt.rs and every thread start ask it).
fn name_matches(m: &str, n: &str) -> bool {
    m.eq_ignore_ascii_case(n) || m.strip_suffix(".dll").is_some_and(|s| s.eq_ignore_ascii_case(n))
}

/// `f` on module `base`, looked up and changed under one write lock (an index taken under a
/// read lock may name another module by the time the write lock is had: a data file freed
/// on another thread shifts the list).
fn with_mut<R>(base: u32, f: impl FnOnce(&mut Module) -> R) -> Option<R> {
    MODULES.write().unwrap().iter_mut().find(|m| m.base == base & !3).map(f)
}

/// The module a bare DLL name stands for in the active activation context: Some(the
/// redirected module, 0 when not loaded, and its path) when an assembly of the context lists
/// the DLL and its file is there.
pub fn sxs_module(name: &str) -> Option<(u32, String)> {
    let path = crate::actctx::redirect_dll(name)?;
    crate::files::resolve(&path)?;
    let b = MODULES.read().unwrap().iter().find(|m| m.path.eq_ignore_ascii_case(&path)).map(|m| m.base).unwrap_or(0);
    Some((b, path))
}

pub fn containing(a: u32) -> Option<u32> {
    MODULES.read().unwrap().iter().find(|m| a >= m.base && a < m.base + m.size).map(|m| m.base)
}

pub fn with<R>(base: u32, f: impl FnOnce(&Module) -> R) -> Option<R> {
    MODULES.read().unwrap().iter().find(|m| m.base == base & !3).map(f)
}

/// How many of a section's bytes come from the file (at PointerToRawData; the rest of it is
/// zeros): SizeOfRawData, at most VirtualSize (0: SizeOfRawData is the size), at most what the
/// file has there, and none at all when PointerToRawData is 0, whatever SizeOfRawData says:
/// Watcom's .bss has a raw size and no file pointer (#207).
pub fn section_file_bytes(vsize: u32, raw: u32, ptr: u32, file_len: usize) -> usize {
    if ptr == 0 {
        return 0;
    }
    let n = raw.min(if vsize != 0 { vsize } else { raw }) as usize;
    n.min(file_len.saturating_sub(ptr as usize))
}

/// LoadLibraryEx(LOAD_LIBRARY_AS_DATAFILE / AS_IMAGE_RESOURCE): the file's headers and
/// sections laid out as an image (any bitness; no imports, relocations or entry point) for its
/// resources, read now (a later change to the file is seen by the next load); listed without
/// a name, so GetModuleHandle never finds it. The base.
pub fn load_datafile(data: &[u8], path: &str) -> Option<u32> {
    let pe = crate::resupdate::Pe::parse(data)?;
    let secs = pe.sections(data);
    let headers = secs.iter().filter(|s| s.raw_size != 0 && s.raw != 0).map(|s| s.raw as usize).min().unwrap_or(0x1000).min(0x1000).min(data.len());
    let size = secs.iter().map(|s| s.va.saturating_add(s.vsize.max(s.raw_size))).max().unwrap_or(0).max(headers as u32).next_multiple_of(0x1000);
    let base = mem::reserve(0, size, 0x02, 0x100_0000, false)?;
    mem::commit(base, 0x04);
    mem::write(base, &data[..headers]);
    for s in &secs {
        let n = section_file_bytes(s.vsize, s.raw_size, s.raw, data.len());
        if n > 0 && s.va as u64 + n as u64 <= size as u64 {
            mem::write(base + s.va, &data[s.raw as usize..s.raw as usize + n]);
        }
    }
    mem::protect(base, size, 0x02);
    let (rva, _) = pe.dir(data, 2);
    MODULES.write().unwrap().push(Module {
        base,
        size,
        entry: 0,
        name: String::new(),
        path: path.to_string(),
        exports: HashMap::default(),
        ordinals: HashMap::default(),
        dll: true,
        tls: None,
        resources: if rva != 0 && rva < size { base + rva } else { 0 },
        refs: 1,
        hle: false,
        pdata: (0, 0),
        redirected: false,
        deps: Vec::new(),
        pinned: false,
    });
    Some(base)
}

/// FreeLibrary of a data-file module: unmapped once the last reference goes
pub fn free_datafile(base: u32) {
    let mut v = MODULES.write().unwrap();
    let Some(i) = v.iter().position(|m| m.base == base && m.name.is_empty() && !m.hle) else { return };
    v[i].refs = v[i].refs.saturating_sub(1);
    if v[i].refs == 0 {
        v.remove(i);
        drop(v);
        mem::release(base);
    }
}

/// FreeLibrary of a DLL file loaded as code: false if `base` is no module. Its last reference
/// going unloads it: DllMain(PROCESS_DETACH, NULL) and its TLS callbacks, out of the loader's
/// lists, unmapped, then the references of the DLLs it imported go the same way (an exe
/// LoadLibrary mapped too). Ours, the process's exe and a pinned DLL stay.
pub fn free_library(cpu: &mut Cpu, base: u32) -> bool {
    let _g = loader_lock();
    let gone = {
        let mut v = MODULES.write().unwrap();
        let Some(i) = v.iter().position(|m| m.base == base & !3) else { return false };
        // (the process's exe is the first image that is no DLL; another exe LoadLibrary
        // mapped goes like a DLL)
        let main = v.iter().position(|m| !m.dll && !m.hle);
        let m = &mut v[i];
        if main == Some(i) || m.hle || m.pinned || m.name.is_empty() {
            return true;
        }
        m.refs = m.refs.saturating_sub(1);
        m.refs == 0
    };
    if gone {
        unload(cpu, base);
    }
    true
}

fn unload(cpu: &mut Cpu, base: u32) {
    // (not yet initialized: no DllMain to tell)
    let fresh = {
        let mut p = PENDING.lock().unwrap();
        let had = p.contains(&base);
        p.retain(|&b| b != base);
        had
    };
    // (an exe's entry point is no DllMain)
    let entry = with(base, |m| if m.dll { m.entry } else { 0 }).unwrap_or(0);
    if !fresh {
        if entry != 0 {
            if crate::env_on!("SAKE_TRACE_LOAD") {
                eprintln!("sake: DllMain(PROCESS_DETACH) of {base:#x}");
            }
            crate::call(cpu, entry, &[base, 0, 0]);
        }
        tls_callbacks(cpu, base, 0);
    }
    let (deps, tls) = {
        let mut v = MODULES.write().unwrap();
        let Some(i) = v.iter().position(|m| m.base == base) else { return };
        let m = v.remove(i);
        (m.deps, m.tls)
    };
    if let Some(t) = tls {
        tls_unload(&t);
    }
    ldr_remove(base);
    crate::dbg::dll_unloaded(base);
    mem::release(base);
    if crate::env_on!("SAKE_TRACE_LOAD") {
        eprintln!("sake: unloaded {base:#x}");
    }
    for d in deps {
        let last = {
            let mut v = MODULES.write().unwrap();
            match v.iter_mut().find(|m| m.base == d && !m.pinned) {
                Some(m) => {
                    m.refs = m.refs.saturating_sub(1);
                    m.refs == 0
                }
                None => false,
            }
        };
        if last {
            unload(cpu, d);
        }
    }
}

/// Whether `base` is a DLL file loaded as code (not ours, not the exe, not a data file).
pub fn is_file_dll(base: u32) -> bool {
    with(base, |m| m.dll && !m.hle && !m.name.is_empty()).unwrap_or(false)
}

/// GetModuleHandleEx without GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: a reference more
/// (or, PIN, never unloaded).
pub fn add_reference(base: u32, pin: bool) {
    let mut v = MODULES.write().unwrap();
    if let Some(m) = v.iter_mut().find(|m| m.base == base & !3) {
        if pin {
            m.pinned = true;
        } else {
            m.refs += 1;
        }
    }
}

/// FreeLibrary of one of our DLLs: its last reference gone, it counts as unloaded (a debugger
/// gets UNLOAD_DLL; LoadLibrary loads it again: LOAD_DLL). Its image stays mapped, as
/// pointers into it (its exports' thunks) stay good.
pub fn free_hle(base: u32) {
    let mut v = MODULES.write().unwrap();
    let Some(m) = v.iter_mut().find(|m| m.base == base && m.hle && m.dll) else { return };
    if m.refs == 0 {
        return;
    }
    m.refs -= 1;
    let gone = m.refs == 0;
    drop(v);
    if gone {
        crate::dbg::dll_unloaded(base);
    }
}
/// A module counted as unloaded (free_hle) is loaded again
fn reload_if_freed(base: u32) {
    let Some((again, base)) = with_mut(base, |m| {
        m.refs += 1;
        (m.refs == 1 && m.hle, m.base)
    }) else { return };
    if again {
        crate::dbg::dll_loaded(base);
    }
}

/// "C:\foo\Bar.DLL" -> "bar.dll"
pub fn base_name(path: &str) -> String {
    let n = path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase();
    n
}

#[derive(Clone, Copy)]
pub enum Import<'a> {
    Name(&'a str),
    Ordinal(u32),
}

/// Whether the file at host path `path` is a 64-bit (x86-64) PE: from its headers alone (the
/// loader asks for each DLL file it looks at, a game's bundled runtime is megabytes).
pub fn is_64bit_file(path: &str) -> bool {
    let Ok(mut f) = kura::fs::File::open(path) else { return false };
    machine_is_x64(&mut f)
}
/// IMAGE_FILE_HEADER.Machine == AMD64, reading e_lfanew and the 2 bytes it points past.
fn machine_is_x64(f: &mut (impl std::io::Read + std::io::Seek)) -> bool {
    use std::io::SeekFrom;
    let mut b = [0u8; 4];
    if f.seek(SeekFrom::Start(0x3c)).is_err() || f.read_exact(&mut b).is_err() {
        return false;
    }
    let pe = u32::from_le_bytes(b) as u64;
    let mut m = [0u8; 2];
    f.seek(SeekFrom::Start(pe + 4)).is_ok() && f.read_exact(&mut m).is_ok() && m == [0x64, 0x86]
}

/// The image's SizeOfStackReserve (the size of each thread's stack unless CreateThread asks for
/// more), from the PE32 or PE32+ optional header; None if `d` is not a PE.
pub fn stack_reserve(d: &[u8]) -> Option<u64> {
    let at = |o: usize, n: usize| d.get(o..o.checked_add(n)?);
    let pe = u32::from_le_bytes(at(0x3c, 4)?.try_into().ok()?) as usize;
    if at(pe, 4)? != b"PE\0\0" {
        return None;
    }
    let opt = pe.checked_add(24)?;
    match u16::from_le_bytes(at(opt, 2)?.try_into().ok()?) {
        0x10b => Some(u32::from_le_bytes(at(opt + 72, 4)?.try_into().ok()?) as u64),
        0x20b => Some(u64::from_le_bytes(at(opt + 72, 8)?.try_into().ok()?)),
        _ => None,
    }
}

/// The stack a new thread reserves, as Windows' RtlCreateUserStack works it out: CreateThread's
/// size is the commit, and a commit at or past the image's reserve makes the reserve that
/// commit rounded up to 1 MB; with STACK_SIZE_PARAM_IS_A_RESERVATION (0x10000) it is the
/// reserve itself. Rounded to 64 KB, at least 1 MB (sake's own floor, see new_teb) and at most
/// 256 MB of the 4 GB space.
pub fn thread_stack(image_reserve: u64, size: u32, flags: u32) -> u32 {
    const MB: u64 = 0x10_0000;
    let mut reserve = image_reserve.max(1);
    if flags & 0x10000 != 0 && size != 0 {
        reserve = size as u64;
    } else if size as u64 >= reserve {
        reserve = (size as u64).div_ceil(MB) * MB;
    }
    (reserve.min(256 * MB).div_ceil(0x10000) * 0x10000).clamp(MB, 256 * MB) as u32
}

/// Read an integer-typed resource from an on-disk PE without mapping the image,
/// resolving imports or running DllMain. Version queries also inspect executables
/// that the caller has never loaded (SDRT's installed service, for example).
pub fn file_resource(data: &[u8], ty: u32) -> Option<&[u8]> {
    let u16_at = |o: usize| Some(u16::from_le_bytes(data.get(o..o.checked_add(2)?)?.try_into().ok()?));
    let u32_at = |o: usize| Some(u32::from_le_bytes(data.get(o..o.checked_add(4)?)?.try_into().ok()?));
    if data.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(0x3c)? as usize;
    if data.get(pe..pe.checked_add(4)?)? != b"PE\0\0" {
        return None;
    }
    let opt = pe.checked_add(24)?;
    let optlen = u16_at(pe.checked_add(20)?)? as usize;
    let dirs = match u16_at(opt)? {
        0x10b => 96,
        0x20b => 112,
        _ => return None,
    };
    if optlen < dirs + 24 || u32_at(opt.checked_add(dirs - 4)?)? < 3 {
        return None;
    }
    let sections = opt.checked_add(optlen)?;
    let count = u16_at(pe.checked_add(6)?)? as usize;
    data.get(sections..sections.checked_add(count.checked_mul(40)?)?)?;
    let rva = |addr: u32, size: u32| -> Option<&[u8]> {
        for i in 0..count {
            let s = sections + i * 40;
            let start = u32_at(s + 12)?;
            let rawlen = u32_at(s + 16)?;
            if let Some(off) = addr.checked_sub(start).filter(|off| off.checked_add(size).is_some_and(|end| end <= rawlen)) {
                let at = u32_at(s + 20)?.checked_add(off)? as usize;
                return data.get(at..at.checked_add(size as usize)?);
            }
        }
        None
    };
    let root = rva(u32_at(opt + dirs + 16)?, u32_at(opt + dirs + 20)?)?;
    let word = |o: usize| Some(u16::from_le_bytes(root.get(o..o.checked_add(2)?)?.try_into().ok()?));
    let dword = |o: usize| Some(u32::from_le_bytes(root.get(o..o.checked_add(4)?)?.try_into().ok()?));
    let entry = |dir: u32, id: Option<u32>| -> Option<u32> {
        let at = (dir & 0x7fff_ffff) as usize;
        let n = word(at.checked_add(12)?)? as usize + word(at.checked_add(14)?)? as usize;
        let first = at.checked_add(16)?;
        root.get(first..first.checked_add(n.checked_mul(8)?)?)?;
        for i in 0..n {
            let e = first + i * 8;
            if id.is_none_or(|id| dword(e) == Some(id)) {
                return dword(e + 4);
            }
        }
        None
    };
    let t = entry(0, Some(ty))?;
    if t & 0x8000_0000 == 0 {
        return None;
    }
    let name = entry(t, Some(1)).or_else(|| entry(t, None))?;
    if name & 0x8000_0000 == 0 {
        return None;
    }
    let leaf = entry(name, None)?;
    if leaf & 0x8000_0000 != 0 {
        return None;
    }
    rva(dword(leaf as usize)?, dword((leaf as usize).checked_add(4)?)?)
}

/// [`file_resource`] of a file read in pieces (`read(offset, n)`: up to n bytes there): its
/// headers and the section the resource directory is in, not the whole file (a 300 MB
/// single-file exe was read whole, twice, for GetFileVersionInfo). Those pieces make a PE of one
/// section that `file_resource` then reads; None where that one section does not hold it all.
pub fn file_resource_read(read: &mut dyn FnMut(u64, usize) -> Option<Vec<u8>>, ty: u32) -> Option<Vec<u8>> {
    const MAX_HEAD: usize = 1 << 20;
    const MAX_SECTION: u32 = 64 << 20;
    let mut head = read(0, 0x1000)?;
    let u16_at = |d: &[u8], o: usize| Some(u16::from_le_bytes(d.get(o..o.checked_add(2)?)?.try_into().ok()?));
    let u32_at = |d: &[u8], o: usize| Some(u32::from_le_bytes(d.get(o..o.checked_add(4)?)?.try_into().ok()?));
    if head.get(..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(&head, 0x3c)? as usize;
    // (the PE header, then the section table: past the first 4 KB in some files)
    if pe.checked_add(24)? > head.len() {
        head = read(0, pe.checked_add(24)?.min(MAX_HEAD))?;
    }
    let opt = pe.checked_add(24)?;
    let optlen = u16_at(&head, pe.checked_add(20)?)? as usize;
    let count = u16_at(&head, pe.checked_add(6)?)? as usize;
    let sections = opt.checked_add(optlen)?;
    let end = sections.checked_add(count.checked_mul(40)?)?;
    if end > MAX_HEAD {
        return None;
    }
    if end > head.len() {
        head = read(0, end)?;
    }
    let dirs = match u16_at(&head, opt)? {
        0x10b => 96,
        0x20b => 112,
        _ => return None,
    };
    let (dir, _) = (u32_at(&head, opt.checked_add(dirs + 16)?)?, u32_at(&head, opt.checked_add(dirs + 20)?)?);
    let s = (0..count).map(|i| sections + i * 40).find(|&s| {
        let (start, rawlen) = (u32_at(&head, s + 12).unwrap_or(0), u32_at(&head, s + 16).unwrap_or(0));
        dir.checked_sub(start).is_some_and(|off| off < rawlen)
    })?;
    let (rawlen, rawptr) = (u32_at(&head, s + 16)?, u32_at(&head, s + 20)?);
    if rawlen > MAX_SECTION {
        return None;
    }
    let raw = read(rawptr as u64, rawlen as usize)?;
    if raw.len() != rawlen as usize {
        return None;
    }
    // the headers with that one section, its bytes right after the table
    let mut one = head.get(..end)?.to_vec();
    one.copy_within(s..s + 40, sections);
    one.truncate(sections + 40);
    one[pe + 6..pe + 8].copy_from_slice(&1u16.to_le_bytes());
    let at = one.len() as u32;
    one[sections + 20..sections + 24].copy_from_slice(&at.to_le_bytes());
    one.extend_from_slice(&raw);
    file_resource(&one, ty).map(<[u8]>::to_vec)
}

/// Maps the image in `data`; `resolve(dll, import)` gives import addresses.
/// The bytes of an image of SizeOfImage `size` from RVA `va` on.
fn image_room(size: u32, va: u32) -> u32 {
    size.saturating_sub(va)
}

/// The fixups of the base relocation table at RVA `start` (`n` bytes) of an image of `size`
/// bytes: `out(RVA, 64-bit)` for each HIGHLOW / DIR64 entry whose bytes lie inside the image. A
/// block whose length runs past the table ends it (as a 0x7fff_fff8 length looped for ever).
fn reloc_targets(start: u32, n: u32, size: u32, rd32: impl Fn(u32) -> u32, rd16: impl Fn(u32) -> u32, mut out: impl FnMut(u32, bool)) {
    let (mut at, end) = (start as u64, start as u64 + n as u64);
    while at + 8 <= end {
        let page = rd32(at as u32) as u64;
        let len = rd32(at as u32 + 4) as u64;
        if len < 8 || at + len > end {
            break;
        }
        for k in 0..(len - 8) / 2 {
            let e = rd16((at + 8 + 2 * k) as u32);
            let off = page + (e & 0xfff) as u64;
            let w = match e >> 12 {
                3 => 4,
                10 => 8,
                _ => continue,
            };
            if off + w <= size as u64 {
                out(off as u32, w == 8);
            }
        }
        at += len;
    }
}

/// Maps the PE file `data` (taken: it goes once its sections are copied, before the imports
/// load their DLLs, so an import chain never holds every file at once besides the images).
pub fn load(data: Vec<u8>, path: &str, resolve: &mut dyn FnMut(&str, Import) -> u32) -> Result<u32, String> {
    let rd16 = |o: usize| -> u32 { data.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as u32) };
    let rd32 = |o: usize| -> u32 { data.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
    if rd16(0) != 0x5a4d {
        return Err("not an MZ file".into());
    }
    let pe = rd32(0x3c) as usize;
    let machine = rd16(pe + 4);
    if rd32(pe) != 0x4550 || (machine != 0x14c && machine != 0x8664) {
        return Err("not an x86 or x86-64 PE".into());
    }
    let wide = machine == 0x8664;
    if wide != mem::x64() {
        return Err(if wide { "a 64-bit image in a 32-bit process".into() } else { "a 32-bit image in a 64-bit process".into() });
    }
    let nsec = rd16(pe + 6) as usize;
    let chars = rd16(pe + 22);
    let opt = pe + 24;
    let optsize = rd16(pe + 20) as usize;
    if rd16(opt) != if wide { 0x20b } else { 0x10b } {
        return Err("bad optional header".into());
    }
    let entry_rva = rd32(opt + 16);
    let pref: u64 = if wide { rd32(opt + 24) as u64 | (rd32(opt + 28) as u64) << 32 } else { rd32(opt + 28) as u64 };
    let size = rd32(opt + 56);
    let hdr_size = rd32(opt + 60);
    let dirs = opt + if wide { 112 } else { 96 };
    let dir = |i: usize| (rd32(dirs + 8 * i), rd32(dirs + 4 + 8 * i));
    let dll = chars & 0x2000 != 0;
    let relocs = dir(5);
    let can_move = relocs.0 != 0 && chars & 1 == 0;
    let low = pref as u32;
    let base = match (pref >> 32 == 0).then(|| mem::reserve(low, size, 0x80, 0x100_0000, false)).flatten() {
        Some(b) => b,
        // (elsewhere when it can move: relocations, or a resource-only DLL with no code to fix)
        // (one that lands on its base's low half by chance is taken as mapped at the base itself,
        // widen_base: unrelocated, so it stands for the base)
        None if can_move || (dll && entry_rva == 0) => {
            let b = mem::reserve(0, size, 0x80, 0x100_0000, false).ok_or("no room for the image")?;
            if pref >> 32 != 0 && b == low {
                mem::set_high(b, (pref >> 32) as u32);
            }
            b
        }
        // above 4G and fixed: memory is addressed modulo 4G, so the image lives at its base's
        // low half and RIP keeps the upper one (koji Cpu::rip_hi); addresses handed to the
        // program in that range get it back (mem::widen). Any number of them, as long as their
        // low halves don't meet (each other's, or a low allocation's)
        None if pref >> 32 != 0 => {
            let b = mem::reserve(low, size, 0x80, 0x100_0000, false).ok_or_else(|| format!("cannot map at {pref:#x} (its low alias {low:#x} is taken)"))?;
            mem::set_high(b, (pref >> 32) as u32);
            b
        }
        None => return Err(format!("cannot map at {pref:#x} (no relocations)")),
    };
    mem::commit(base, 0x40);
    mem::write(base, &data[..(hdr_size as usize).min(data.len()).min(size as usize)]);
    let secs = opt + optsize;
    // (vsize, va, raw size, characteristics) of each section, for the protections below
    let mut sections = Vec::with_capacity(nsec.min(96));
    for i in 0..nsec {
        let s = secs + 40 * i;
        let vsize = rd32(s + 8);
        let va = rd32(s + 12);
        let raw = rd32(s + 16) as usize;
        let ptr = rd32(s + 20) as usize;
        sections.push((vsize, va, raw as u32, rd32(s + 36)));
        // (what lies past SizeOfImage is not the image's: a packer stub's section claiming more
        // is cut there rather than written over the allocation after the image)
        let n = section_file_bytes(vsize, raw as u32, ptr as u32, data.len()).min(image_room(size, va) as usize);
        if n > 0 {
            mem::write(base + va, &data[ptr..ptr + n]);
        }
        // ASProtect (its .adata section): its decryptors run from small W|X allocations that
        // the JIT then translates too (with its stores watched) rather than interpreting
        if !dll && data.get(s..s + 8) == Some(b".adata\0\0") {
            kura::jit::translate_wx_pages();
        }
    }
    // relocations
    let delta = (widen_base(base, pref)).wrapping_sub(pref);
    if delta != 0 && relocs.0 != 0 {
        let (start, n) = (relocs.0.min(size), relocs.1.min(image_room(size, relocs.0)));
        reloc_targets(start, n, size, |o| r32(base + o), |o| r16(base + o) as u32, |off, wide| {
            let a = base + off;
            if wide {
                mem::w64(a, mem::r64(a).wrapping_add(delta));
            } else {
                w32(a, r32(a).wrapping_add(delta as u32));
            }
        });
    }
    // (the data directories, from the file: it goes now)
    let dirs: [(u32, u32); 16] = std::array::from_fn(dir);
    drop(data);
    let dir = |i: usize| dirs[i];
    let name = base_name(path);
    // exports
    let mut exports = HashMap::default();
    let mut ordinals = HashMap::default();
    // (the DLLs its forwarders and imports name: references of its own)
    let mut deps: Vec<u32> = Vec::new();
    let (erva, esize) = dir(0);
    // (a table outside the image is a packer's leftover: ASProtect's AGE.EXE (Eushully) has
    // 0x48000000 functions, which Windows never reads because nothing looks them up)
    let fits = |rva: u32, n: u32, k: u32| (rva as u64) + (n as u64) * (k as u64) <= size as u64;
    let e = base + erva;
    if erva != 0 && fits(erva, 40, 1) && fits(r32(e + 28), r32(e + 20), 4) && fits(r32(e + 32), r32(e + 24), 4) && fits(r32(e + 36), r32(e + 24), 2) {
        let ord_base = r32(e + 16);
        let nfun = r32(e + 20);
        let nnames = r32(e + 24);
        let funs = base + r32(e + 28);
        let names = base + r32(e + 32);
        let ords = base + r32(e + 36);
        let target = |rva: u32| -> u32 {
            if rva >= erva && rva < erva + esize {
                // forwarder "dll.name": resolved like an import
                let s = String::from_utf8_lossy(&mem::cstr(base + rva)).into_owned();
                let (d, n) = s.split_once('.').unwrap_or((&s, ""));
                let dll = format!("{d}.dll");
                let a = if let Some(o) = n.strip_prefix('#') {
                    resolve(&dll, Import::Ordinal(o.parse::<u32>().unwrap_or(0)))
                } else {
                    resolve(&dll, Import::Name(n))
                };
                if let Some(b) = by_name(&dll).filter(|&b| b != base && !deps.contains(&b)) {
                    deps.push(b);
                }
                a
            } else {
                base + rva
            }
        };
        let mut resolve_fwd = target;
        for i in 0..nfun {
            let rva = r32(funs + 4 * i);
            if rva != 0 {
                ordinals.insert(ord_base + i, resolve_fwd(rva));
            }
        }
        for i in 0..nnames {
            let n = String::from_utf8_lossy(&mem::cstr(base + r32(names + 4 * i))).into_owned();
            let o = r16(ords + 2 * i) as u32;
            if let Some(&a) = ordinals.get(&(ord_base + o)) {
                exports.insert(n, a);
            }
        }
    }
    // TLS directory
    let (trva, _) = dir(9);
    let tls = (trva != 0).then(|| {
        let t = base + trva;
        let k = mem::ps();
        Tls { start: r32(t), end: r32(t + k), index_addr: r32(t + 2 * k), callbacks: r32(t + 3 * k), zero_fill: r32(t + 4 * k), index: 0 }
    });
    MODULES.write().unwrap().push(Module {
        base,
        size,
        entry: if entry_rva != 0 { base + entry_rva } else { 0 },
        name,
        path: path.to_string(),
        exports,
        ordinals,
        dll,
        tls,
        resources: if dir(2).0 != 0 { base + dir(2).0 } else { 0 },
        refs: 1,
        hle: false,
        pdata: if dir(3).0 != 0 { (base + dir(3).0, dir(3).1 / 12) } else { (0, 0) },
        redirected: false,
        deps: Vec::new(),
        pinned: false,
    });
    ldr_add(base);
    // imports (after the module is listed: a DLL importing from itself, or a cycle)
    let (irva, _) = dir(1);
    if crate::env_on!("SAKE_TRACE_LOAD") {
        eprintln!("sake: mapped {path} at {base:#x} ({size:#x} bytes), imports at {irva:#x}");
    }
    if irva != 0 {
        let mut d = base + irva;
        while r32(d + 12) != 0 {
            let dll_name = String::from_utf8_lossy(&mem::cstr(base + r32(d + 12))).into_owned();
            let lookup = if r32(d) != 0 { r32(d) } else { r32(d + 16) };
            let iat = base + r32(d + 16);
            let mut k = 0;
            let ps = mem::ps();
            loop {
                let v = r32(base + lookup + ps * k);
                let ordinal = if wide { r32(base + lookup + ps * k + 4) & 0x8000_0000 != 0 } else { v & 0x8000_0000 != 0 };
                if v == 0 && !ordinal {
                    break;
                }
                let a = if ordinal {
                    resolve(&dll_name, Import::Ordinal(v & 0xffff))
                } else {
                    let n = String::from_utf8_lossy(&mem::cstr(base + v + 2)).into_owned();
                    resolve(&dll_name, Import::Name(&n))
                };
                if wide {
                    mem::w64(iat + 8 * k, mem::widen(a));
                } else {
                    w32(iat + 4 * k, a);
                }
                k += 1;
            }
            if let Some(b) = by_name(&dll_name).filter(|&b| b != base && !deps.contains(&b)) {
                deps.push(b);
            }
            d += 20;
        }
    }
    // (each DLL file imported from holds a reference of this module's)
    {
        let mut v = MODULES.write().unwrap();
        deps.retain(|&b| v.iter().any(|m| m.base == b && m.dll && !m.hle));
        for m in v.iter_mut().filter(|m| deps.contains(&m.base)) {
            m.refs += 1;
        }
        if let Some(m) = v.iter_mut().find(|m| m.base == base) {
            m.deps = deps;
        }
    }
    // each section's protection from its characteristics (the image was mapped read-write-execute
    // to be written): the JIT translates only pages that are not both writable and executable
    mem::protect(base, hdr_size.max(1).min(size), 0x02);
    for (vsize, va, raw, ch) in sections {
        let n = (if vsize != 0 { vsize } else { raw }).min(image_room(size, va));
        let (x, w) = (ch & 0x2000_0000 != 0, ch & 0x8000_0000 != 0);
        let prot = match (x, w) {
            (true, true) => 0x40,
            (true, false) => 0x20,
            (false, true) => 0x04,
            (false, false) => 0x02,
        };
        if n != 0 {
            mem::protect(base + va, n, prot);
        }
    }
    kura::crashlog::add_module(base, path);
    Ok(base)
}

// ---------------------------------------------------------------------------------------------
// resources

/// A resource id: an integer or a name.
#[derive(Clone, Debug)]
pub enum ResId {
    Int(u32),
    Name(Vec<u16>),
}

impl ResId {
    /// From a guest LPCSTR/LPCWSTR argument (MAKEINTRESOURCE below 0x10000).
    pub fn from_arg(a: u32, wide: bool) -> ResId {
        if a < 0x10000 {
            return ResId::Int(a);
        }
        let s: Vec<u16> = if wide { mem::wstr(a) } else { crate::text::a2w(&mem::cstr(a)) };
        if s.first() == Some(&('#' as u16)) {
            if let Ok(n) = String::from_utf16_lossy(&s[1..]).parse() {
                return ResId::Int(n);
            }
        }
        ResId::Name(s.iter().map(|&c| crate::text::upper16(c)).collect())
    }
}

/// The entry of `id` in the resource directory `dir` (base = the resource section).
fn find_entry(root: u32, dir: u32, id: &ResId) -> Option<u32> {
    let named = r16(dir + 12) as u32;
    let ids = r16(dir + 14) as u32;
    for i in 0..named + ids {
        let e = dir + 16 + 8 * i;
        let n = r32(e);
        let hit = match id {
            ResId::Int(v) => n & 0x8000_0000 == 0 && n == *v,
            ResId::Name(s) => {
                n & 0x8000_0000 != 0 && {
                    let p = root + (n & 0x7fff_ffff);
                    let len = r16(p) as u32;
                    len as usize == s.len() && (0..len).all(|k| crate::text::upper16(r16(p + 2 + 2 * k)) == s[k as usize])
                }
            }
        };
        if hit {
            return Some(r32(e + 4));
        }
    }
    None
}

/// Which of a resource's languages (`langs`, in the directory's order) a lookup without a
/// language takes, as Windows' (and Wine's) resource loader: LANG_NEUTRAL itself, the UI
/// language `ui`, its primary language with SUBLANG_NEUTRAL, English (US), else the first one.
/// A launcher with Chinese (0x404, 0x804), English and Japanese string tables showed its
/// buttons in Chinese when the first language was taken.
pub(crate) fn pick_lang(langs: &[u32], ui: u32) -> Option<usize> {
    [0, ui, ui & 0x3ff, 0x409]
        .iter()
        .find_map(|want| langs.iter().position(|l| l == want))
        .or(if langs.is_empty() { None } else { Some(0) })
}

/// The entry (data entry offset, or a subdirectory with the high bit) of the language
/// pick_lang takes in the language directory `langs`.
fn neutral_entry(root: u32, langs: u32) -> Option<u32> {
    let ids = dir_ids(root, langs);
    let l: Vec<u32> = ids.iter().map(|(id, _)| if let ResId::Int(v) = id { *v } else { u32::MAX }).collect();
    pick_lang(&l, crate::k32::lcid()).map(|i| ids[i].1)
}

/// The data entry of a resource in language `lang` (None or LANG_NEUTRAL 0: pick_lang's), or
/// the error Windows gives: ERROR_RESOURCE_TYPE/NAME/LANG_NOT_FOUND (1813/1814/1815).
pub fn find_resource_lang(module: u32, ty: &ResId, name: &ResId, lang: Option<u32>) -> Result<u32, u32> {
    let root = with(module, |m| m.resources).unwrap_or(0);
    if root == 0 {
        return Err(1812);
    }
    let t = find_entry(root, root, ty).filter(|t| t & 0x8000_0000 != 0).ok_or(1813u32)?;
    let n = find_entry(root, root + (t & 0x7fff_ffff), name).filter(|n| n & 0x8000_0000 != 0).ok_or(1814u32)?;
    let langs = root + (n & 0x7fff_ffff);
    let e = match lang {
        None | Some(0) => neutral_entry(root, langs).ok_or(1815u32)?,
        Some(l) => find_entry(root, langs, &ResId::Int(l)).ok_or(1815u32)?,
    };
    if e & 0x8000_0000 != 0 {
        return Err(1815);
    }
    Ok(root + e)
}

/// The data entry (IMAGE_RESOURCE_DATA_ENTRY address) of a resource; any language.
pub fn find_resource(module: u32, ty: &ResId, name: &ResId) -> Option<u32> {
    let root = with(module, |m| m.resources)?;
    if root == 0 {
        return None;
    }
    let t = find_entry(root, root, ty)?;
    if t & 0x8000_0000 == 0 {
        return None;
    }
    let n = find_entry(root, root + (t & 0x7fff_ffff), name)?;
    if n & 0x8000_0000 == 0 {
        return None;
    }
    let langs = root + (n & 0x7fff_ffff);
    let e = neutral_entry(root, langs)?;
    (e & 0x8000_0000 == 0).then_some(root + e)
}

/// The first resource of type `ty` (any name).
pub fn first_resource(module: u32, ty: u32) -> Option<u32> {
    let root = with(module, |m| m.resources)?;
    if root == 0 {
        return None;
    }
    let t = find_entry(root, root, &ResId::Int(ty))?;
    if t & 0x8000_0000 == 0 {
        return None;
    }
    let dir = root + (t & 0x7fff_ffff);
    let n = r32(dir + 16 + 4);
    if n & 0x8000_0000 == 0 {
        return None;
    }
    let langs = root + (n & 0x7fff_ffff);
    let e = neutral_entry(root, langs)?;
    (e & 0x8000_0000 == 0).then_some(root + e)
}

/// The ids of directory `dir`'s entries, in their order (names first, as linkers sort them).
fn dir_ids(root: u32, dir: u32) -> Vec<(ResId, u32)> {
    let n = r16(dir + 12) as u32 + r16(dir + 14) as u32;
    (0..n)
        .map(|i| {
            let e = dir + 16 + 8 * i;
            let id = r32(e);
            let key = if id & 0x8000_0000 != 0 {
                let p = root + (id & 0x7fff_ffff);
                ResId::Name((0..r16(p) as u32).map(|k| r16(p + 2 + 2 * k)).collect())
            } else {
                ResId::Int(id)
            };
            (key, r32(e + 4))
        })
        .collect()
}
/// What EnumResourceTypes / Names / Languages walk: the ids one level down `path` (types for
/// an empty path, names of a type, languages of a type and name). None: no resources there.
pub fn resource_ids(module: u32, path: &[ResId]) -> Option<Vec<ResId>> {
    let root = with(module, |m| m.resources)?;
    if root == 0 {
        return None;
    }
    let mut dir = root;
    for id in path {
        let e = find_entry(root, dir, id)?;
        if e & 0x8000_0000 == 0 {
            return None;
        }
        dir = root + (e & 0x7fff_ffff);
    }
    Some(dir_ids(root, dir).into_iter().map(|x| x.0).collect())
}

/// (address, size) of a found resource's data.
pub fn resource_data(module: u32, entry: u32) -> (u32, u32) {
    ((module & !3) + r32(entry), r32(entry + 4))
}

// ---------------------------------------------------------------------------------------------
// the loader

use koji::Cpu;

/// DLLs loaded but not initialized yet (DllMain), dependencies first.
static PENDING: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// TLS index of the next module with a TLS directory
static TLS_NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// TLS indexes of unloaded modules, given out again first (lowest first, as Windows)
static TLS_FREE: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// How many implicit TLS blocks a thread's ThreadLocalStoragePointer array holds (the last
/// 0x1000 bytes of its TEB's reservation: crate::new_teb).
fn tls_capacity() -> u32 {
    0x1000 / mem::ps()
}

/// An index for a module's implicit TLS: a freed one, else the next (None: the array is full).
fn tls_index_take(free: &mut Vec<u32>, next: &std::sync::atomic::AtomicU32, cap: u32) -> Option<u32> {
    if let Some(k) = free.iter().enumerate().min_by_key(|&(_, &v)| v).map(|(k, _)| k) {
        return Some(free.swap_remove(k));
    }
    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if i < cap {
        return Some(i);
    }
    next.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    None
}

/// Where the exe's entry and threads return: a stub in kernel32's image that goes on to
/// EXIT_TRAP (0 until kernel32 is made).
static EXIT_THUNK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// (32-bit) the handler of a thread's last SEH frame, in ntdll as Windows' (see seh::top_frame)
static TOP_HANDLER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The return address for a 32-bit entry point or thread start: inside kernel32's image, as on
/// Windows (BaseThreadInitThunk).
pub fn exit_return() -> u32 {
    if mem::x64() {
        return crate::api::EXIT_TRAP;
    }
    hle_module("kernel32.dll");
    match EXIT_THUNK.load(std::sync::atomic::Ordering::Relaxed) {
        0 => crate::api::EXIT_TRAP,
        t => t,
    }
}

/// (32-bit) The handler of the SEH frame every thread starts with (seh::top_frame): code in
/// ntdll's image, `mov eax, 1 (ExceptionContinueSearch); ret`, so the dispatch goes on to the
/// end of the chain and the unhandled-exception path (SetUnhandledExceptionFilter's filter,
/// then the end of the process), as Windows' _except_handler4 in RtlUserThreadStart ends it.
pub fn top_seh_handler() -> u32 {
    hle_module("ntdll.dll");
    match TOP_HANDLER.load(std::sync::atomic::Ordering::Relaxed) {
        0 => crate::seh::fallback_top_handler(),
        t => t,
    }
}

/// The module handle of one of our DLLs (made on first use: a header page so code reading
/// the headers finds an image).
pub fn hle_module(dll: &str) -> u32 {
    let name = format!("{}.dll", crate::api::dll_key(dll));
    let find = || MODULES.read().unwrap().iter().find(|m| m.name == name).map(|m| m.base);
    if let Some(b) = find() {
        return b;
    }
    // (made by one thread at a time, looked for again under it: two threads starting at once
    // made two images of one DLL; called without the loader lock from every thread's start)
    static MAKING: Mutex<()> = Mutex::new(());
    let _making = MAKING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(b) = find() {
        return b;
    }
    // (32-bit) an export directory of every function the DLL has, each a jump to its trap:
    // packers (ASProtect, Enigma) find kernel32 from the return address of the exe's entry and
    // walk its exports for GetProcAddress, and ASProtect runs the first instructions of the
    // APIs it imports (user32's too) from copies in its own code
    let key = crate::api::dll_key(dll);
    let names = if !mem::x64() { crate::api::spec_names(&key) } else { Vec::new() };
    let strings: usize = names.iter().map(|n| n.len() + 1).sum::<usize>() + name.len() + 1;
    let n = names.len() as u32;
    let exp_size = 40 + 10 * n + strings as u32;
    // header page, then the export directory, then 16-byte thunks (the first: the exe's return)
    let thunks = 0x1000 + (exp_size + 15) / 16 * 16;
    let code_end = if n == 0 { 0x1000 } else { (thunks + 16 * (n + 1) + 0xfff) & !0xfff };
    // (then the DLL's resources, if it has any: crate::sysres)
    let res = crate::sysres::tree(&key).map(|t| crate::resupdate::build(&t, code_end));
    let size = code_end + res.as_ref().map_or(0, |r| (r.len() as u32 + 0xfff) & !0xfff);
    // (no room left in the address space: the DLL is not there, as a failed LoadLibrary)
    let Some(base) = mem::reserve(0, size, 2, 0x100_0000, true) else {
        eprintln!("sake: no room for {name}");
        return 0;
    };
    mem::commit_range(base, size, 0x20);
    // MZ + PE headers with no sections
    w16(base, 0x5a4d);
    w32(base + 0x3c, 0x40);
    w32(base + 0x40, 0x4550);
    if mem::x64() {
        w16(base + 0x44, 0x8664);
        w16(base + 0x54, 0xf0);
        w16(base + 0x56, 0x2022);
        w16(base + 0x58, 0x20b);
        w32(base + 0x58 + 24, base);
    } else {
        w16(base + 0x44, 0x14c);
        w16(base + 0x54, 0xe0);
        w16(base + 0x56, 0x2102);
        w16(base + 0x58, 0x10b);
        w32(base + 0x58 + 28, base);
    }
    w32(base + 0x58 + 56, size);
    let mut exports = HashMap::default();
    if n != 0 {
        // IMAGE_EXPORT_DIRECTORY at 0x1000: functions, names (sorted), ordinals, strings
        let e = base + 0x1000;
        let (funs, nms, ords) = (e + 40, e + 40 + 4 * n, e + 40 + 8 * n);
        let mut str_at = ords + 2 * n;
        let dll_name = str_at;
        mem::write(str_at, name.as_bytes());
        str_at += name.len() as u32 + 1;
        w32(e + 12, dll_name - base);
        w32(e + 16, 1);
        w32(e + 20, n);
        w32(e + 24, n);
        w32(e + 28, funs - base);
        w32(e + 32, nms - base);
        w32(e + 36, ords - base);
        // (stub 0: the exe's and threads' return address, on to EXIT_TRAP with EAX kept)
        mem::write(base + thunks, &[0xb9, 0x20, 0x00, 0xff, 0xff, 0xff, 0xe1, 0xcc]);
        // each function: Windows' hot-patch prologue undone, then `push trap; ret`:
        // ASProtect copies an API's first instructions into its own code and jumps past them, so
        // what follows the prologue must work anywhere (no relative jump); and it keeps every
        // register, as fastcall and thiscall functions found by GetProcAddress take ECX/EDX
        for (i, f) in names.iter().enumerate() {
            let i = i as u32;
            // (ntdll's Zw* are its Nt* functions, at the same address: a hook on one is on both)
            if let Some(&t) = f.strip_prefix("Zw").filter(|_| key == "ntdll").and_then(|n| exports.get(&format!("Nt{n}"))) {
                exports.insert(f.clone(), t);
                w32(funs + 4 * i, t - base);
                w32(nms + 4 * i, str_at - base);
                w16(ords + 2 * i, i as u16);
                mem::write(str_at, f.as_bytes());
                str_at += f.len() as u32 + 1;
                continue;
            }
            let t = base + thunks + 16 * (i + 1);
            let trap = crate::api::export(&key, f);
            mem::write(t, &[0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d, 0x68]);
            w32(t + 7, trap);
            mem::write(t + 11, &[0xc3, 0xcc, 0xcc, 0xcc, 0xcc]);
            exports.insert(f.clone(), t);
            w32(funs + 4 * i, t - base);
            w32(nms + 4 * i, str_at - base);
            w16(ords + 2 * i, i as u16);
            mem::write(str_at, f.as_bytes());
            str_at += f.len() as u32 + 1;
        }
        // the directory entry (export table, its size)
        w32(base + 0x58 + 96, 0x1000);
        w32(base + 0x58 + 100, exp_size);
        w32(base + 0x58 + 92, 16);
        if key == "kernel32" {
            EXIT_THUNK.store(base + thunks, std::sync::atomic::Ordering::Relaxed);
        }
        if key == "ntdll" {
            // (after stub 0's 8 bytes: the last SEH frame's handler, top_seh_handler)
            mem::write(base + thunks + 8, &[0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3, 0xcc, 0xcc]);
            TOP_HANDLER.store(base + thunks + 8, std::sync::atomic::Ordering::Relaxed);
        }
    }
    if let Some(r) = &res {
        mem::write(base + code_end, r);
        // the resource directory entry (IMAGE_DIRECTORY_ENTRY_RESOURCE, 2)
        let dirs = base + 0x58 + if mem::x64() { 112 } else { 96 };
        w32(dirs + 16, code_end);
        w32(dirs + 20, r.len() as u32);
        if mem::r32(base + 0x58 + if mem::x64() { 108 } else { 92 }) < 3 {
            w32(base + 0x58 + if mem::x64() { 108 } else { 92 }, 16);
        }
    }
    mem::protect(base, size, 0x20);
    MODULES.write().unwrap().push(Module {
        base,
        size,
        entry: 0,
        name: name.clone(),
        path: format!("C:\\windows\\system32\\{name}"),
        exports,
        ordinals: HashMap::default(),
        dll: true,
        tls: None,
        resources: if res.is_some() { base + code_end } else { 0 },
        refs: 1,
        hle: true,
        pdata: (0, 0),
        redirected: false,
        deps: Vec::new(),
        pinned: false,
    });
    drop(_making);
    ldr_add(base);
    base
}

// ---------------------------------------------------------------------------------------------
// the PEB's loader data: PEB_LDR_DATA and a LDR_DATA_TABLE_ENTRY per module, in its three
// lists (load, memory and initialization order), as programs walk them

/// (PEB_LDR_DATA, base -> entry)
static LDR: Mutex<(u32, Vec<(u32, u32)>)> = Mutex::new((0, Vec::new()));
/// LDR_DATA_TABLE_ENTRY.Flags: loaded from a side-by-side assembly
pub const LDR_REDIRECTED: u32 = 0x1000_0000;

fn ldr_data() -> u32 {
    let mut g = LDR.lock().unwrap();
    if g.0 == 0 {
        let peb = crate::PEB.load(std::sync::atomic::Ordering::Relaxed);
        if peb == 0 {
            return 0;
        }
        let ps = mem::ps();
        let d = mem::heap_alloc(0x58, true);
        w32(d, if ps == 8 { 0x58 } else { 0x30 });
        w32(d + 4, 1);
        for k in 0..3 {
            let head = d + if ps == 8 { 16 } else { 12 } + 2 * ps * k;
            mem::wp(head, head);
            mem::wp(head + ps, head);
        }
        mem::wp(peb + if ps == 8 { 0x18 } else { 0x0c }, d);
        g.0 = d;
    }
    g.0
}

/// Frees the buffer of the UNICODE_STRING at `at` that ustr_at made (none yet: 0).
fn ustr_free(at: u32) {
    let b = mem::rp(at + mem::ps());
    if b != 0 {
        mem::heap_free(b);
        mem::wp(at + mem::ps(), 0);
    }
}

fn ustr_at(at: u32, s: &str) {
    let w: Vec<u16> = s.encode_utf16().collect();
    let b = mem::heap_alloc(2 * w.len() as u32 + 2, true);
    for (i, &c) in w.iter().enumerate() {
        w16(b + 2 * i as u32, c);
    }
    w16(at, 2 * w.len() as u16);
    w16(at + 2, 2 * w.len() as u16 + 2);
    mem::wp(at + mem::ps(), b);
}

/// The program's own image
pub fn exe_base() -> u32 {
    MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.base).unwrap_or(0)
}
/// The modules on the loader's lists, in load order
pub fn ldr_bases() -> Vec<u32> {
    LDR.lock().unwrap().1.iter().map(|x| x.0).collect()
}

/// Lists module `base` (just added to MODULES) at the end of the loader's lists.
fn ldr_add(base: u32) {
    let d = ldr_data();
    let Some((size, entry, path, redirected)) = with(base, |m| (m.size, m.entry, m.path.clone(), m.redirected)) else { return };
    if d == 0 {
        return;
    }
    let ps = mem::ps();
    let e = mem::heap_alloc(if ps == 8 { 0x120 } else { 0xa8 }, true);
    for k in 0..3 {
        let head = d + if ps == 8 { 16 } else { 12 } + 2 * ps * k;
        let link = e + 2 * ps * k;
        let last = mem::rp(head + ps);
        mem::wp(link, head);
        mem::wp(link + ps, last);
        mem::wp(last, link);
        mem::wp(head + ps, link);
    }
    let (dllbase, ep, sz, full, basen, flags, count) = if ps == 8 { (48, 56, 64, 72, 88, 104, 108) } else { (24, 28, 32, 36, 44, 52, 56) };
    mem::wp(e + dllbase, base);
    mem::wp(e + ep, entry);
    w32(e + sz, size);
    ustr_at(e + full, &path);
    ustr_at(e + basen, path.rsplit('\\').next().unwrap_or(&path));
    w32(e + flags, 0x4 | if redirected { LDR_REDIRECTED } else { 0 });
    w16(e + count, 1);
    LDR.lock().unwrap().1.push((base, e));
    crate::dbg::dll_loaded(base);
}

/// Takes module `base` out of the loader's three lists.
fn ldr_remove(base: u32) {
    let e = {
        let mut l = LDR.lock().unwrap();
        let Some(i) = l.1.iter().position(|x| x.0 == base) else { return };
        l.1.remove(i).1
    };
    let ps = mem::ps();
    for k in 0..3 {
        let link = e + 2 * ps * k;
        let (next, prev) = (mem::rp(link), mem::rp(link + ps));
        mem::wp(prev, next);
        mem::wp(next + ps, prev);
    }
    // (the entry's FullDllName and BaseDllName buffers go with it)
    let (full, basen) = if ps == 8 { (72, 88) } else { (36, 44) };
    ustr_free(e + full);
    ustr_free(e + basen);
    mem::heap_free(e);
}

/// A module's flags or path changed after it was listed.
pub fn ldr_update(base: u32) {
    let Some(e) = LDR.lock().unwrap().1.iter().find(|x| x.0 == base).map(|x| x.1) else { return };
    let Some((path, redirected)) = with(base, |m| (m.path.clone(), m.redirected)) else { return };
    let ps = mem::ps();
    let (full, basen, flags) = if ps == 8 { (72, 88, 104) } else { (36, 44, 52) };
    ustr_free(e + full);
    ustr_free(e + basen);
    ustr_at(e + full, &path);
    ustr_at(e + basen, path.rsplit('\\').next().unwrap_or(&path));
    let f = r32(e + flags) & !LDR_REDIRECTED;
    w32(e + flags, f | if redirected { LDR_REDIRECTED } else { 0 });
}

use crate::mem::w16;

/// The folders SetDllDirectory / AddDllDirectory added to the search (after the exe's).
pub static DLL_DIRS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A guest DLL, including third-party runtimes installed in system32 (SysWOW64 is the same folder).
/// Known Windows DLLs there remain sake's own: an old Wine prefix must not replace
/// its system implementations. This restriction does not apply to vendor DLLs
/// such as SDRT's UCUtil50.dll, which the official MSI installs in SystemFolder.
fn find_dll_file(dll: &str) -> Option<(String, String)> {
    let name = if dll.contains('.') { dll.to_string() } else { format!("{dll}.dll") };
    let system_dll = api::is_system(name.rsplit(['\\', '/']).next().unwrap_or(&name));
    if name.contains('\\') || name.contains('/') || name.contains(':') {
        let w = crate::files::full_path(&name);
        let system_dir = is_system_dir(w.rsplit_once('\\').map_or("", |x| x.0));
        if system_dll && system_dir {
            return None;
        }
        return crate::files::resolve(&w).filter(|h| !system_dir || !wine_builtin(h)).map(|h| (h, w));
    }
    let exe_dir = MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.rsplit_once('\\').map_or(String::new(), |x| x.0.to_string()));
    let mut dirs: Vec<String> = exe_dir.into_iter().collect();
    dirs.extend(DLL_DIRS.lock().unwrap().iter().cloned());
    dirs.push(crate::files::cwd().trim_end_matches('\\').to_string());
    // the folders of DLLs loaded by path (LOAD_WITH_ALTERED_SEARCH_PATH finds their dependencies there)
    for m in MODULES.read().unwrap().iter().filter(|m| m.dll && !m.hle) {
        if let Some((d, _)) = m.path.rsplit_once('\\') {
            if !dirs.iter().any(|x| x.eq_ignore_ascii_case(d)) {
                dirs.push(d.to_string());
            }
        }
    }
    if !system_dll {
        // (SysWOW64 is the same folder: files::wow64_system; the machine check below takes a
        // runtime of this process's kind)
        dirs.push("C:\\windows\\system32".into());
        dirs.push("C:\\windows\\system".into());
    }
    for d in dirs.iter().filter(|d| !system_dll || !is_system_dir(d)) {
        let w = format!("{d}\\{name}");
        if let Some(h) = crate::files::resolve(&w) {
            // (an old Wine prefix's own builds there are not a vendor's: sake has none of
            // Wine's DLLs, and one it does not implement stays missing)
            if is_system_dir(d) && wine_builtin(&h) {
                continue;
            }
            // (as Windows' loader, one of the other architecture is passed over: a 64-bit game
            // shipping a 32-bit d3dcompiler_47.dll gets the system's)
            if is_64bit_file(&h) == mem::x64() {
                return Some((h, w));
            }
        }
    }
    None
}

/// A PE build of Wine's own DLLs (winebuild writes "Wine builtin DLL" after the DOS header;
/// "Wine placeholder DLL" for the fake ones a prefix keeps).
pub(crate) fn wine_builtin(host: &str) -> bool {
    use std::io::Read;
    let mut head = [0u8; 0x60];
    let Ok(mut f) = kura::fs::File::open(host) else { return false };
    let n = f.read(&mut head).unwrap_or(0);
    wine_stub(&head[..n])
}
fn wine_stub(head: &[u8]) -> bool {
    head.get(0x40..0x60).is_some_and(|s| s.starts_with(b"Wine builtin DLL") || s.starts_with(b"Wine placeholder DLL"))
}

/// C:\windows\system32 (or SysWOW64, Sysnative: files::wow64_system) or C:\windows\system (a game run from there, or one that adds it with
/// SetDllDirectory, still gets sake's DLLs only)
fn is_system_dir(d: &str) -> bool {
    let d = crate::files::wow64_system(&crate::files::full_path(d)).to_ascii_lowercase();
    let d = d.trim_end_matches('\\');
    d == "c:\\windows\\system32" || d == "c:\\windows\\system"
}


/// Whether imports from `dll` bind to sake's own DLL, the search for a vendor's copy of a
/// system-named DLL done once per DLL name and image being loaded (it is asked per symbol: an
/// exe importing 300 functions from its own msvcr100.dll searched the folders 300 times).
fn imports_hle(seen: &mut HashMap<String, bool>, dll: &str, find: impl FnOnce() -> bool) -> bool {
    if api::is_core(dll) {
        return true;
    }
    if !api::is_system(dll) {
        return false;
    }
    if let Some(&h) = seen.get(dll) {
        return h;
    }
    let h = !find();
    seen.insert(dll.to_string(), h);
    h
}

/// The import resolver for one image: resolve_import with that image's searches remembered.
fn image_resolver() -> impl FnMut(&str, Import) -> u32 {
    let mut seen = HashMap::default();
    move |dll, imp| resolve_import_with(&mut seen, dll, imp)
}

fn resolve_import_with(seen: &mut HashMap<String, bool>, dll: &str, imp: Import) -> u32 {
    let hle = imports_hle(seen, dll, || find_dll_file(dll).is_some());
    if hle {
        let name = match imp {
            Import::Name(n) => n.to_string(),
            Import::Ordinal(o) => api::ordinal_name(dll, o).unwrap_or_else(|| format!("#{o}")),
        };
        hle_module(dll);
        if let Some(d) = hle_data(dll, &name) {
            return d;
        }
        return crate::fast::bind(dll, &name, api::export(dll, &name));
    }
    let base = match by_name(dll) {
        Some(b) => b,
        None => match load_dll_file(dll) {
            Ok(b) => {
                // (its one reference is the importer's, counted once the importer is loaded)
                with_mut(b, |m| m.refs = 0);
                b
            }
            Err(e) => {
                eprintln!("sake: import from {dll}: {e}");
                let name = match imp {
                    Import::Name(n) => n.to_string(),
                    Import::Ordinal(o) => format!("#{o}"),
                };
                return api::export(dll, &name);
            }
        },
    };
    proc_address(base, imp).unwrap_or_else(|| {
        let name = match imp {
            Import::Name(n) => n.to_string(),
            Import::Ordinal(o) => format!("#{o}"),
        };
        eprintln!("sake: {dll} has no export {name}");
        api::export(dll, &name)
    })
}

use crate::api;

/// GetProcAddress
pub fn proc_address(base: u32, imp: Import) -> Option<u32> {
    lookup_proc(base, imp, false)
}
/// proc_address as a static import binds it: kernel32/ntdll functions we know but don't
/// implement resolve too (to a trap returning 0). Import resolvers of packers want this.
pub fn proc_address_static(base: u32, imp: Import) -> Option<u32> {
    lookup_proc(base, imp, true)
}
fn lookup_proc(base: u32, imp: Import, bind_known: bool) -> Option<u32> {
    let (hle, name, a) = with(base, |m| {
        let a = match &imp {
            Import::Name(n) => m.exports.get(*n).copied(),
            Import::Ordinal(o) => m.ordinals.get(o).copied(),
        };
        (m.hle, m.name.clone(), a)
    })?;
    if !hle {
        return a;
    }
    let n = match imp {
        Import::Name(n) => n.to_string(),
        Import::Ordinal(o) => api::ordinal_name(&name, o)?,
    };
    if let Some(d) = hle_data(&name, &n) {
        return Some(d);
    }
    if let Some(d) = crate::dbg::export(&name, &n) {
        return Some(d);
    }
    if let Some(g) = crate::guest::lookup(&api::dll_key(&name), &n) {
        return Some(g);
    }
    if !bind_known && !api::exported_by(&name, &n) {
        return None;
    }
    let a = api::export(&name, &n);
    // an unimplemented export of a system DLL resolves to its trap (logged, returns 0) as a static
    // import does: programs that look up a whole API by name call some unchecked (騎士見習いと村娘).
    // kernel32/ntdll keep answering NULL: the CRT looks up newer functions there and falls back
    // to older ones when they are missing
    let kernel = !bind_known && matches!(api::dll_key(&name).as_str(), "kernel32" | "kernelbase" | "ntdll");
    let known = api::is_known(a);
    if !(api::implemented(a) || (known && !kernel) || crate::env_on!("SAKE_ALL_EXPORTS")) {
        return None;
    }
    // a trap: the function's thunk in the module's image if it has one (kernel32 code at a
    // kernel32 address, which packers copy instructions from)
    match crate::fast::bind(&name, &n, a) {
        b if b == a => Some(with(base, |m| m.exports.get(&n).copied()).flatten().unwrap_or(a)),
        b => Some(b),
    }
}

/// A data export of a DLL we implement (msvcrt's variables).
fn hle_data(dll: &str, name: &str) -> Option<u32> {
    // (msvcr90's too: AdvHD imports _acmdln from it)
    let k = api::dll_key(dll);
    if crate::mfc42::DLLS.contains(&k.as_str()) {
        crate::mfc42::use_dll(&k);
        return crate::mfc42::data(name);
    }
    if k == "msvcp90" || k == "msvcp80" {
        if let Some(a) = crate::msvcp90::data(name) {
            return Some(a);
        }
    }
    if k.starts_with("msvcp") {
        return crate::msvcp::data(name);
    }
    if k == "crtdll" {
        return crate::crt::crtdll_data(name);
    }
    (api::is_crt(&k) && !k.starts_with("vcruntime")).then(|| crate::crt::data(name)).flatten()
}

fn load_dll_file(dll: &str) -> Result<u32, String> {
    let (host, win) = find_dll_file(dll).ok_or_else(|| format!("{dll} not found"))?;
    let data = kura::fs::read(&host).map_err(|e| format!("{host}: {e}"))?;
    if crate::env_on!("SAKE_TRACE_LOAD") {
        eprintln!("sake: loading {win} ({} bytes)", data.len());
    }
    let base = load(data, &win, &mut image_resolver())?;
    crate::vorbis_hook::patch(base, with(base, |m| m.size).unwrap_or(0));
    crate::unity6::patch(&win, base, with(base, |m| m.size).unwrap_or(0));
    tls_module(base);
    PENDING.lock().unwrap().push(base);
    Ok(base)
}

/// LoadLibrary: an existing module, one of ours, or a DLL file (initialized here).
pub fn load_library(cpu: &mut Cpu, name: &str) -> u32 {
    let _g = loader_lock();
    let key = api::dll_key(name);
    if let Some((b, path)) = sxs_module(name) {
        if b != 0 && with_mut(b, |m| m.refs += 1).is_some() {
            return b;
        }
        if let Ok(b) = load_dll_file(&path) {
            if with_mut(b, |m| m.redirected = true).is_some() {
                ldr_update(b);
            }
            init_dlls(cpu);
            return b;
        }
    }
    let qualified = name.contains(['\\', '/', ':']);
    let at = if qualified { find_dll_file(name) } else { None };
    let loaded = match &at {
        Some((_, w)) => by_path(w),
        None => by_name(name),
    };
    if let Some(b) = loaded {
        reload_if_freed(b);
        return b;
    }
    let file = if qualified { at } else { find_dll_file(name) };
    if api::is_core(name) || (api::is_system(name) && file.is_none()) {
        // the Visual C++ runtimes bring the UCRT in with them (GetModuleHandle("ucrtbase.dll")
        // then finds it, as Wine's msvcp140 tests take setlocale from it)
        if key.starts_with("msvcp14") || key.starts_with("vcruntime14") || key.starts_with("concrt14") {
            hle_module("ucrtbase");
            if !key.starts_with("vcruntime14") {
                hle_module(if mem::x64() { "vcruntime140_1" } else { "vcruntime140" });
                hle_module("vcruntime140");
            }
        }
        let b = hle_module(&key);
        // (a system assembly's DLL, the Visual C++ runtime's: from its WinSxS folder)
        if let Some(p) = crate::actctx::builtin_dll_path(name) {
            if with_mut(b, |m| m.path = p).is_some() {
                ldr_update(b);
            }
        }
        return b;
    }
    match load_dll_file(name) {
        Ok(b) => {
            init_dlls(cpu);
            b
        }
        Err(e) => {
            if crate::env_on!("SAKE_TRACE_LOAD") {
                eprintln!("sake: LoadLibrary({name}): {e}");
            }
            0
        }
    }
}

pub fn load_exe(_cpu: &mut Cpu, host: &str, win: &str) -> Result<u32, String> {
    let data = kura::fs::read(host).map_err(|e| e.to_string())?;
    let base = load(data, win, &mut image_resolver())?;
    // the main exe: the JIT translates its writable code too, checking stores into it
    let size = with(base, |m| m.size).unwrap_or(0);
    kura::jit::exe_image(base, size);
    crate::movie::patch(base, size);
    crate::vorbis_hook::patch(base, size);
    tls_module(base);
    Ok(base)
}

/// Implicit TLS of a module just loaded: its index (one an unloaded module gave back, else the
/// next), and a block in every running thread (Windows since Vista: a thread that already ran
/// gets one too, not only the loading thread and those started later).
fn tls_module(base: u32) {
    let mut mods = MODULES.write().unwrap();
    let Some(t) = mods.iter_mut().find(|m| m.base == base).and_then(|m| m.tls.as_mut()) else { return };
    let Some(index) = tls_index_take(&mut TLS_FREE.lock().unwrap(), &TLS_NEXT, tls_capacity()) else {
        // (more modules with TLS loaded at once than the array holds: none rather than a write
        // past the TEB; the DLL's thread-local variables fault)
        eprintln!("sake: no TLS index left for the module at {base:#x} ({} in use)", tls_capacity());
        mods.iter_mut().find(|m| m.base == base).unwrap().tls = None;
        return;
    };
    t.index = index;
    if t.index_addr != 0 {
        w32(t.index_addr, t.index);
    }
    let t = *t;
    drop(mods);
    for (_, teb, _) in crate::k32::thread_list() {
        tls_block(teb, &t);
    }
}

/// Where thread `teb`'s block of TLS index `index` is pointed to.
fn tls_slot(teb: u32, index: u32) -> u32 {
    r32(teb + crate::teb_tls_pointer()) + mem::ps() * index
}

/// Thread `teb`'s block of module TLS `t`, unless it has one (a thread started while the module
/// loaded gets it from both tls_module and thread_attach).
fn tls_block(teb: u32, t: &Tls) {
    let slot = tls_slot(teb, t.index);
    if mem::rp(slot) != 0 {
        return;
    }
    let n = t.end.saturating_sub(t.start);
    let b = mem::heap_alloc(n + t.zero_fill + 16, true);
    mem::copy(b, t.start, n);
    mem::wp(slot, b);
}

/// Frees thread `teb`'s block of module TLS `t`.
fn tls_block_free(teb: u32, t: &Tls) {
    let slot = tls_slot(teb, t.index);
    let b = mem::rp(slot);
    if b != 0 {
        mem::wp(slot, 0);
        mem::heap_free(b);
    }
}

/// A module with TLS unloads: its blocks go in every thread, its index to the next module.
fn tls_unload(t: &Tls) {
    for (_, teb, _) in crate::k32::thread_list() {
        tls_block_free(teb, t);
    }
    TLS_FREE.lock().unwrap().push(t.index);
}

/// A thread ends (k32::exit_thread, before its TEB goes): out of the running threads, and its
/// TLS blocks freed.
pub fn thread_tls_exit(teb: u32) {
    let _g = loader_lock();
    crate::k32::tls_thread_exit(teb);
    let tls: Vec<Tls> = MODULES.read().unwrap().iter().filter_map(|m| m.tls).collect();
    for t in &tls {
        tls_block_free(teb, t);
    }
}

fn tls_callbacks(cpu: &mut Cpu, base: u32, reason: u32) {
    let Some(t) = with(base, |m| m.tls).flatten() else { return };
    if t.callbacks == 0 {
        return;
    }
    let mut p = t.callbacks;
    while r32(p) != 0 {
        let f = r32(p);
        crate::call(cpu, f, &[base, reason, 0]);
        p += mem::ps();
    }
}

/// DllMain(PROCESS_ATTACH) of the DLLs loaded since, and the exe's TLS callbacks the first time.
pub fn init_dlls(cpu: &mut Cpu) {
    let _g = loader_lock();
    static EXE_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    loop {
        let next = {
            let mut p = PENDING.lock().unwrap();
            if p.is_empty() {
                None
            } else {
                Some(p.remove(0))
            }
        };
        let Some(base) = next else { break };
        tls_callbacks(cpu, base, 1);
        let entry = with(base, |m| m.entry).unwrap_or(0);
        if entry != 0 {
            if crate::env_on!("SAKE_TRACE_LOAD") {
                eprintln!("sake: DllMain of {:#x} {} ...", base, with(base, |m| m.path.clone()).unwrap_or_default());
            }
            let ok = crate::call(cpu, entry, &[base, 1, 0]);
            if crate::env_on!("SAKE_TRACE_LOAD") {
                eprintln!("sake: DllMain of {:#x} {}: {ok}", base, with(base, |m| m.name.clone()).unwrap_or_default());
            }
        }
    }
    if !EXE_DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        let exe = MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.base);
        if let Some(b) = exe {
            tls_callbacks(cpu, b, 1);
        }
    }
}

/// A new thread: its TLS blocks, TLS callbacks and DllMain(THREAD_ATTACH).
pub fn thread_attach(cpu: &mut Cpu) {
    let _g = loader_lock();
    let mods: Vec<(u32, Option<Tls>, u32, bool)> =
        MODULES.read().unwrap().iter().map(|m| (m.base, m.tls, m.entry, m.dll && !m.hle)).collect();
    let teb = crate::cur().teb;
    for (_, t, _, _) in &mods {
        if let Some(t) = t {
            tls_block(teb, t);
        }
    }
    for (base, _, entry, dll) in mods {
        tls_callbacks(cpu, base, 2);
        if dll && entry != 0 {
            crate::call(cpu, entry, &[base, 2, 0]);
        }
    }
}

#[cfg(test)]
mod unload_tests {
    use super::*;
    use proptest::prelude::*;

    fn fake(base: u32, refs: u32, deps: Vec<u32>) {
        MODULES.write().unwrap().push(Module {
            base,
            size: 0x1000,
            entry: 0,
            name: format!("unload{base:x}.dll"),
            path: String::new(),
            exports: HashMap::default(),
            ordinals: HashMap::default(),
            dll: true,
            tls: None,
            resources: 0,
            refs,
            hle: false,
            pdata: (0, 0),
            redirected: false,
            deps,
            pinned: false,
        });
    }
    fn refs(b: u32) -> Option<u32> {
        with(b, |m| m.refs)
    }

    /// A DLL (refs 1) importing from B (1 from it, 1 from a LoadLibrary): FreeLibrary of A
    /// unloads A and leaves B at 1; of B then unloads it; a pinned DLL stays; a gone one is
    /// no module.
    #[test]
    fn last_reference_unloads_and_releases_imports() {
        let mut cpu = koji::Cpu::default();
        let (a, b, p) = (0x7a01_0000, 0x7a02_0000, 0x7a03_0000);
        fake(b, 2, vec![]);
        fake(a, 1, vec![b]);
        fake(p, 1, vec![]);
        assert!(free_library(&mut cpu, a));
        assert_eq!((refs(a), refs(b)), (None, Some(1)));
        assert!(!free_library(&mut cpu, a));
        assert!(free_library(&mut cpu, b));
        assert_eq!(refs(b), None);
        add_reference(p, true);
        assert!(free_library(&mut cpu, p) && free_library(&mut cpu, p));
        assert_eq!(refs(p), Some(1));
        MODULES.write().unwrap().retain(|m| m.base != p);
    }

    proptest! {
        /// Against a model of references (a DAG of imports over 5 DLLs, LoadLibrary and
        /// FreeLibrary in any order): each DLL's count and whether it is loaded are the
        /// model's, its last reference releasing those of the DLLs it imports.
        #[test]
        fn loaded_while_referenced(edges in proptest::collection::vec((0usize..5, 0usize..5), 0..8), ops in proptest::collection::vec((any::<bool>(), 0usize..5), 0..40), salt in 0u32..0x100) {
            let mut cpu = koji::Cpu::default();
            let base = |i: usize| 0x7b00_0000 + (salt << 16) + (i as u32) * 0x1000;
            // (i imports j only for j > i: no cycles)
            let mut deps = vec![vec![]; 5];
            for (i, j) in edges {
                if j > i && !deps[i].contains(&j) {
                    deps[i].push(j);
                }
            }
            // (a failed case before may have left its modules)
            MODULES.write().unwrap().retain(|m| m.base >> 24 != 0x7b || (m.base >> 16) & 0xff != salt);
            // all loaded once by the program, their imports counted: one count per DLL, as
            // Windows keeps (a FreeLibrary too many takes an importer's reference)
            let mut n = [0u32; 5];
            for i in 0..5 {
                n[i] = 1 + (0..5).filter(|&k| deps[k].contains(&i)).count() as u32;
                fake(base(i), n[i], deps[i].iter().map(|&j| base(j)).collect());
            }
            let mut live = [true; 5];
            fn drop_ref(i: usize, n: &mut [u32; 5], live: &mut [bool; 5], deps: &[Vec<usize>]) {
                n[i] -= 1;
                if n[i] == 0 {
                    live[i] = false;
                    for &j in &deps[i] {
                        if live[j] {
                            drop_ref(j, n, live, deps);
                        }
                    }
                }
            }
            for (load, i) in ops {
                if load {
                    if live[i] {
                        n[i] += 1;
                        add_reference(base(i), false);
                    }
                } else if live[i] {
                    prop_assert!(free_library(&mut cpu, base(i)));
                    drop_ref(i, &mut n, &mut live, &deps);
                } else {
                    prop_assert!(!free_library(&mut cpu, base(i)));
                }
                for k in 0..5 {
                    prop_assert_eq!(refs(base(k)), live[k].then_some(n[k]), "dll {} after {:?}", k, (load, i));
                }
            }
            MODULES.write().unwrap().retain(|m| m.base >> 24 != 0x7b || (m.base >> 16) & 0xff != salt);
        }
    }
}

#[cfg(test)]
mod loader_tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::time::Duration;

    // (one test: they share the process-wide lock)
    #[test]
    fn loader_lock_is_reentrant_exclusive_and_released_by_thread_exit() {
        // the same thread again: no wait; the outer guard still holds it after the inner goes
        let outer = loader_lock();
        drop(loader_lock());
        let (tx, rx) = channel();
        let waiter = std::thread::spawn(move || {
            let _g = loader_lock();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err(), "another thread got the lock while it was held");
        drop(outer);
        rx.recv_timeout(Duration::from_secs(5)).expect("the lock was not released by its last guard");
        waiter.join().unwrap();
        // a thread that ends inside its DllMain (ExitThread: its guards never drop)
        std::thread::spawn(|| {
            let g = loader_lock();
            let _inner = std::mem::ManuallyDrop::new(loader_lock());
            std::mem::forget(g);
            loader_thread_exit();
        })
        .join()
        .unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let _g = loader_lock();
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(5)).expect("a thread that exited holding the loader lock kept it");
        // (exiting without holding it changes nothing)
        let held = loader_lock();
        std::thread::spawn(loader_thread_exit).join().unwrap();
        assert_eq!(LOADER.lock().unwrap().1, 1);
        drop(held);
        assert_eq!(*LOADER.lock().unwrap(), (None, 0));
    }
}

#[cfg(test)]
mod wine_tests {
    #[test]
    fn wine_builds_are_recognized_by_their_dos_stub() {
        let mut head = vec![0u8; 0x60];
        head[..2].copy_from_slice(b"MZ");
        assert!(!super::wine_stub(&head));
        head[0x40..0x50].copy_from_slice(b"Wine builtin DLL");
        assert!(super::wine_stub(&head));
        head[0x40..0x54].copy_from_slice(b"Wine placeholder DLL");
        assert!(super::wine_stub(&head));
        // (a vendor DLL's ordinary stub, and a short file)
        head[0x40..0x60].copy_from_slice(b"This program cannot be run in DO");
        assert!(!super::wine_stub(&head));
        assert!(!super::wine_stub(b"MZ"));
    }
}

#[cfg(test)]
mod lang_tests {
    use super::pick_lang;
    use proptest::prelude::*;

    #[test]
    fn neutral_lookups_take_the_ui_language_before_the_first() {
        // 虜ノ焔 体験版's launcher: string tables in 0x404, 0x409, 0x411, 0x804 (sorted)
        assert_eq!(pick_lang(&[0x404, 0x409, 0x411, 0x804], 0x411), Some(2));
        // no Japanese: English (US), then the first
        assert_eq!(pick_lang(&[0x404, 0x409, 0x804], 0x411), Some(1));
        assert_eq!(pick_lang(&[0x404, 0x804], 0x411), Some(0));
        // LANG_NEUTRAL itself first; the primary language with SUBLANG_NEUTRAL before English
        assert_eq!(pick_lang(&[0x409, 0x411, 0], 0x411), Some(2));
        assert_eq!(pick_lang(&[0x409, 0x11], 0x411), Some(1));
        // one language (most Japanese builds), none
        assert_eq!(pick_lang(&[0x411], 0x409), Some(0));
        assert_eq!(pick_lang(&[], 0x411), None);
    }

    proptest! {
        /// the reference order, written out: the first of [neutral, ui, ui's primary, en-US]
        /// that is there, else index 0; always an index of `langs`
        #[test]
        fn pick_matches_the_lookup_order(langs in proptest::collection::vec(prop_oneof![Just(0u32), Just(0x11), Just(0x404), Just(0x409), Just(0x411), Just(0x804), 0u32..0x10000], 0..8), ui in prop_oneof![Just(0x411u32), Just(0x409), Just(0x804), 0u32..0x10000]) {
            let got = pick_lang(&langs, ui);
            let mut want = None;
            'o: for w in [0, ui, ui & 0x3ff, 0x409] {
                for (i, l) in langs.iter().enumerate() {
                    if *l == w { want = Some(i); break 'o; }
                }
            }
            if want.is_none() && !langs.is_empty() { want = Some(0); }
            prop_assert_eq!(got, want);
            if let Some(i) = got { prop_assert!(i < langs.len()); }
            // the UI language is taken whenever it is there and no LANG_NEUTRAL entry is
            if langs.contains(&ui) && !langs.contains(&0) { prop_assert_eq!(langs[got.unwrap()], ui); }
        }
    }
}

#[cfg(test)]
mod stack_tests {
    use super::*;
    use proptest::prelude::*;

    const MB: u32 = 0x10_0000;

    #[test]
    fn as_windows_sizes_them() {
        assert_eq!(thread_stack(4 * MB as u64, 0, 0), 4 * MB);
        assert_eq!(thread_stack(4 * MB as u64, 0x10000, 0), 4 * MB);
        assert_eq!(thread_stack(4 * MB as u64, 5 * MB + 1, 0), 6 * MB);
        assert_eq!(thread_stack(4 * MB as u64, 8 * MB, 0x10000), 8 * MB);
        assert_eq!(thread_stack(4 * MB as u64, 2 * MB, 0x10000), 2 * MB);
        // a reservation of 0 is the image's
        assert_eq!(thread_stack(4 * MB as u64, 0, 0x10000), 4 * MB);
        // sake's floor and ceiling
        assert_eq!(thread_stack(0x10000, 0, 0), MB);
        assert_eq!(thread_stack(u64::MAX, 0, 0), 256 * MB);
    }

    /// A PE32 and a PE32+ header with the field set, and what is not a PE.
    #[test]
    fn reads_the_header() {
        for (magic, wide) in [(0x10bu16, false), (0x20b, true)] {
            let mut d = vec![0u8; 0x200];
            d[0x3c] = 0x80;
            d[0x80..0x84].copy_from_slice(b"PE\0\0");
            d[0x98..0x9a].copy_from_slice(&magic.to_le_bytes());
            if wide {
                d[0x98 + 72..0x98 + 80].copy_from_slice(&0x1_0040_0000u64.to_le_bytes());
            } else {
                d[0x98 + 72..0x98 + 76].copy_from_slice(&0x40_0000u32.to_le_bytes());
            }
            assert_eq!(stack_reserve(&d), Some(if wide { 0x1_0040_0000 } else { 0x40_0000 }));
        }
        assert_eq!(stack_reserve(b"MZ"), None);
        assert_eq!(stack_reserve(&[0u8; 0x100]), None);
    }

    proptest! {
        /// Any bytes: no panic.
        #[test]
        fn header_any_bytes(d in proptest::collection::vec(any::<u8>(), 0..0x200), pe in 0u32..0x180) {
            let mut d = d;
            if d.len() >= 0x40 {
                d[0x3c..0x40].copy_from_slice(&pe.to_le_bytes());
            }
            let _ = stack_reserve(&d);
        }

        /// The stack holds what was asked for (within the ceiling), is 64 KB aligned and in range,
        /// and never shrinks below the image's reserve without the reservation flag.
        #[test]
        fn stack_holds_the_request(image in 0u64..0x2000_0000, size in any::<u32>(), resv in any::<bool>()) {
            let flags = if resv { 0x10000 } else { 0 };
            let s = thread_stack(image, size, flags);
            prop_assert_eq!(s % 0x10000, 0);
            prop_assert!((MB..=256 * MB).contains(&s));
            prop_assert!(s as u64 >= (size as u64).min(256 * MB as u64));
            if !resv || size == 0 {
                prop_assert!(s as u64 >= image.clamp(MB as u64, 256 * MB as u64));
            }
        }
    }
}

/// LdrFindEntryForAddress(address, LDR_DATA_TABLE_ENTRY *out)
#[allow(non_snake_case)]
fn LdrFindEntryForAddress(c: &mut crate::api::Ctx) -> u32 {
    let Some(b) = containing(c.a(0)) else { return 0xc000_0135 };
    let Some(e) = LDR.lock().unwrap().1.iter().find(|x| x.0 == b).map(|x| x.1) else { return 0xc000_0135 };
    mem::putp(c.a(1), e);
    0
}
/// RtlImageNtHeader(base): its IMAGE_NT_HEADERS, or NULL when it is not a PE image
#[allow(non_snake_case)]
fn RtlImageNtHeader(c: &mut crate::api::Ctx) -> u32 {
    let b = c.a(0);
    if b == 0 || mem::accessible(b, 64, false, true) < 64 || r16(b) != 0x5a4d {
        return 0;
    }
    let nt = b.wrapping_add(r32(b + 0x3c));
    if mem::accessible(nt, 4, false, true) < 4 || r32(nt) != 0x4550 {
        return 0;
    }
    nt
}
pub fn init() {
    crate::api::implement("ntdll", "LdrFindEntryForAddress", LdrFindEntryForAddress);
    crate::api::implement("ntdll", "RtlImageNtHeader", RtlImageNtHeader);
}

#[cfg(test)]
mod path_tests {
    use super::*;
    use proptest::prelude::*;

    fn fake(base: u32, path: &str) -> Module {
        Module {
            base,
            size: 0x1000,
            entry: 0,
            name: base_name(path),
            path: path.into(),
            exports: Default::default(),
            ordinals: Default::default(),
            dll: true,
            tls: None,
            resources: 0,
            refs: 1,
            hle: false,
            pdata: (0, 0),
            redirected: false,
            deps: Vec::new(),
            pinned: false,
        }
    }

    const A: &str = "C:\\game\\renpy\\angle\\pathtest_gldraw.pyd";
    const B: &str = "C:\\game\\renpy\\gl\\pathtest_gldraw.pyd";

    /// Ren'Py 6.99/7: renpy\angle\gldraw.pyd loaded, then renpy\gl\gldraw.pyd by its path (#146):
    /// the path finds only the module of that file; the bare name, a module of that name.
    #[test]
    fn same_named_modules_are_told_apart_by_path() {
        MODULES.write().unwrap().push(fake(0x7f10_0000, A));
        assert_eq!(by_path(B), None, "the gl gldraw.pyd is not the angle one");
        assert_eq!(by_name(B), Some(0x7f10_0000), "(by_name matches base names only)");
        MODULES.write().unwrap().push(fake(0x7f20_0000, B));
        assert_eq!(by_path(A), Some(0x7f10_0000));
        assert_eq!(by_path(B), Some(0x7f20_0000));
        assert_eq!(by_path(&B.to_ascii_uppercase()), Some(0x7f20_0000));
        assert!(matches!(by_name("pathtest_gldraw.pyd"), Some(0x7f10_0000 | 0x7f20_0000)));
        // (a redirected module is found through its activation context only)
        MODULES.write().unwrap().iter_mut().find(|m| m.base == 0x7f20_0000).unwrap().redirected = true;
        assert_eq!(by_path(B), None);
        MODULES.write().unwrap().retain(|m| m.base != 0x7f10_0000 && m.base != 0x7f20_0000);
    }

    proptest! {
        /// any spelling of a loaded module's path (case) finds it, and no other folder's file of
        /// the same name does
        #[test]
        fn by_path_is_the_whole_path_without_case(dir in "[a-zA-Z0-9_]{1,8}", other in "[a-zA-Z0-9_]{1,8}", flips in proptest::collection::vec(any::<bool>(), 64)) {
            let p = format!("C:\\pt_{dir}\\pathprop.dll");
            let base = 0x7f30_0000;
            MODULES.write().unwrap().push(fake(base, &p));
            let spelled: String = p.chars().zip(flips.iter().cycle()).map(|(c, f)| if *f { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() }).collect();
            let found = by_path(&spelled);
            let elsewhere = by_path(&format!("C:\\pt_{other}\\pathprop.dll"));
            MODULES.write().unwrap().retain(|m| m.base != base);
            prop_assert_eq!(found, Some(base));
            prop_assert_eq!(elsewhere, if dir.eq_ignore_ascii_case(&other) { Some(base) } else { None });
        }
    }
}

#[cfg(test)]
mod section_tests {
    use super::section_file_bytes;
    use proptest::prelude::*;

    /// IPF33.DLL's .bss (Watcom): raw 0x1800 at file pointer 0 is all zeros, not the headers
    #[test]
    fn watcom_bss_has_no_file_bytes() {
        assert_eq!(section_file_bytes(0, 0x1800, 0, 36352), 0);
        assert_eq!(section_file_bytes(0, 0x6600, 0x400, 36352), 0x6600);
        assert_eq!(section_file_bytes(0x10, 0x200, 0x400, 0x1000), 0x10);
        assert_eq!(section_file_bytes(0x1000, 0x200, 0xf80, 0x1000), 0x80);
    }

    proptest! {
        /// the bytes read always lie inside the file, inside SizeOfRawData and VirtualSize (when
        /// set), and are none without a file pointer
        #[test]
        fn bytes_stay_in_bounds(vsize in 0u32..0x4000, raw in 0u32..0x4000, ptr in 0u32..0x5000, len in 0usize..0x6000) {
            let n = section_file_bytes(vsize, raw, ptr, len);
            prop_assert!(n <= raw as usize);
            prop_assert!(vsize == 0 || n <= vsize as usize);
            prop_assert!(ptr as usize + n <= len.max(ptr as usize));
            if ptr == 0 { prop_assert_eq!(n, 0); }
            // (everything the limits allow is read)
            if ptr != 0 && (ptr as usize) <= len {
                prop_assert_eq!(n, (raw.min(if vsize != 0 { vsize } else { raw }) as usize).min(len - ptr as usize));
            }
        }
    }
}

#[cfg(test)]
mod tls_index_tests {
    use super::tls_index_take;
    use proptest::prelude::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A plugin with TLS linked and unlinked over and over (KiriKiri's Plugins.link/unlink, a
    /// codec DLL per movie) keeps getting the same index, never one past the array.
    #[test]
    fn unloaded_indexes_are_given_again() {
        let (mut free, next) = (Vec::new(), AtomicU32::new(0));
        let exe = tls_index_take(&mut free, &next, 1024).unwrap();
        assert_eq!(exe, 0);
        for _ in 0..100_000 {
            let i = tls_index_take(&mut free, &next, 1024).unwrap();
            assert_eq!(i, 1);
            free.push(i);
        }
        assert_eq!(next.load(Ordering::Relaxed), 2);
    }

    /// A full array answers None (and stays full), never an index past it.
    #[test]
    fn a_full_array_gives_none() {
        let (mut free, next) = (Vec::new(), AtomicU32::new(0));
        for k in 0..4 {
            assert_eq!(tls_index_take(&mut free, &next, 4), Some(k));
        }
        assert_eq!(tls_index_take(&mut free, &next, 4), None);
        assert_eq!(tls_index_take(&mut free, &next, 4), None);
        free.push(2);
        assert_eq!(tls_index_take(&mut free, &next, 4), Some(2));
        assert_eq!(tls_index_take(&mut free, &next, 4), None);
    }

    proptest! {
        /// Any sequence of loads and unloads: the indexes in use are distinct and inside the
        /// array, a load gets the lowest index not in use (as Windows), and it fails only when
        /// every index is in use.
        #[test]
        fn indexes_stay_distinct_and_bounded(ops in prop::collection::vec((any::<bool>(), 0usize..64), 1..400), cap in 1u32..40) {
            let (mut free, next) = (Vec::new(), AtomicU32::new(0));
            let mut used: Vec<u32> = Vec::new();
            for (load, pick) in ops {
                if load || used.is_empty() {
                    let lowest = (0..cap).find(|i| !used.contains(i));
                    let got = tls_index_take(&mut free, &next, cap);
                    prop_assert_eq!(got, lowest);
                    if let Some(i) = got {
                        used.push(i);
                    }
                } else {
                    let i = used.swap_remove(pick % used.len());
                    free.push(i);
                }
                prop_assert!(used.iter().all(|&i| i < cap));
                let mut u = used.clone();
                u.sort_unstable();
                u.dedup();
                prop_assert_eq!(u.len(), used.len());
            }
        }
    }
}

#[cfg(test)]
mod review_tests {
    use super::*;
    use proptest::prelude::*;
    use std::io::{Cursor, Read, Seek, SeekFrom};

    /// A reader that counts the bytes it hands out.
    struct Counting(Cursor<Vec<u8>>, usize);
    impl Read for Counting {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            let n = self.0.read(b)?;
            self.1 += n;
            Ok(n)
        }
    }
    impl Seek for Counting {
        fn seek(&mut self, p: SeekFrom) -> std::io::Result<u64> {
            self.0.seek(p)
        }
    }
    /// The whole-file check is_64bit_file made before (review b09: it read every DLL whole).
    fn whole_file(d: &[u8]) -> bool {
        let pe = d.get(0x3c..0x40).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) as usize;
        d.get(pe + 4..pe + 6) == Some(&[0x64, 0x86])
    }
    fn image(machine: u16, lfanew: u32, len: usize) -> Vec<u8> {
        let mut d = vec![0u8; len];
        d[..2].copy_from_slice(b"MZ");
        d[0x3c..0x40].copy_from_slice(&lfanew.to_le_bytes());
        let p = lfanew as usize;
        if p + 6 <= len {
            d[p..p + 4].copy_from_slice(b"PE\0\0");
            d[p + 4..p + 6].copy_from_slice(&machine.to_le_bytes());
        }
        d
    }

    /// An 8 MiB 64-bit DLL and a 32-bit one: the answer from 6 bytes of the file.
    #[test]
    fn machine_from_the_headers_only() {
        for (m, x64) in [(0x8664u16, true), (0x14c, false)] {
            let mut f = Counting(Cursor::new(image(m, 0xf8, 8 << 20)), 0);
            assert_eq!(machine_is_x64(&mut f), x64);
            assert_eq!(f.1, 6, "bytes read");
        }
        // (e_lfanew past the end, a file shorter than the DOS header)
        assert!(!machine_is_x64(&mut Cursor::new(image(0x8664, 0x7fff_fff0, 0x100))));
        assert!(!machine_is_x64(&mut Cursor::new(b"MZ".to_vec())));
        // through kura's files, as the loader asks
        let dir = std::env::temp_dir().join(format!("sake-pe-machine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.dll");
        std::fs::write(&p, image(0x8664, 0x80, 0x400)).unwrap();
        assert!(is_64bit_file(p.to_str().unwrap()));
        std::fs::write(&p, image(0x14c, 0x80, 0x400)).unwrap();
        assert!(!is_64bit_file(p.to_str().unwrap()));
        assert!(!is_64bit_file(dir.join("none.dll").to_str().unwrap()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One search per DLL name however many symbols the image imports from it; DLLs sake has
    /// no file for and non-system DLLs never searched.
    #[test]
    fn one_search_per_imported_dll() {
        let mut seen = HashMap::default();
        let mut searches = 0;
        for _ in 0..300 {
            assert!(!imports_hle(&mut seen, "msvcr100.dll", || {
                searches += 1;
                true
            }));
        }
        assert_eq!(searches, 1);
        assert!(imports_hle(&mut seen, "kernel32.dll", || unreachable!("a core DLL is sake's")));
        assert!(!imports_hle(&mut seen, "gamelib_review.dll", || unreachable!("not a system name")));
        assert!(imports_hle(&mut seen, "d3dx9_43.dll", || false));
        assert!(imports_hle(&mut seen, "d3dx9_43.dll", || unreachable!()));
    }

    /// Base relocations as the loader applied them before, unbounded (the reference).
    fn relocs_model(t: &[u8], size: u32) -> Vec<(u32, bool)> {
        let r32 = |o: usize| t.get(o..o + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
        let r16 = |o: usize| t.get(o..o + 2).map_or(0, |b| u16::from_le_bytes(b.try_into().unwrap()));
        let mut v = Vec::new();
        let mut at = 0usize;
        while at + 8 <= t.len() {
            let (page, len) = (r32(at), r32(at + 4) as usize);
            if len < 8 || at + len > t.len() {
                break;
            }
            for k in 0..(len - 8) / 2 {
                let e = r16(at + 8 + 2 * k);
                let off = page as u64 + (e & 0xfff) as u64;
                let w = match e >> 12 {
                    3 => 4,
                    10 => 8,
                    _ => continue,
                };
                if off + w <= size as u64 {
                    v.push((off as u32, w == 8));
                }
            }
            at += len;
        }
        v
    }
    fn relocs(t: &[u8], size: u32) -> Vec<(u32, bool)> {
        let r32 = |o: u32| t.get(o as usize..o as usize + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
        let r16 = |o: u32| t.get(o as usize..o as usize + 2).map_or(0, |b| u16::from_le_bytes(b.try_into().unwrap()) as u32);
        let mut v = Vec::new();
        reloc_targets(0, t.len() as u32, size, r32, r16, |o, w| v.push((o, w)));
        v
    }

    /// A block claiming 0x7fff_fff8 bytes ends the table (it was ~2^30 entries over whatever
    /// followed), and a fixup past SizeOfImage is not written.
    #[test]
    fn relocations_stay_in_the_table_and_image() {
        let mut t = Vec::new();
        t.extend_from_slice(&0x1000u32.to_le_bytes());
        t.extend_from_slice(&12u32.to_le_bytes());
        t.extend_from_slice(&0x3010u16.to_le_bytes());
        t.extend_from_slice(&0x3ffeu16.to_le_bytes());
        t.extend_from_slice(&0x2000u32.to_le_bytes());
        t.extend_from_slice(&0x7fff_fff8u32.to_le_bytes());
        t.extend_from_slice(&[0x30; 64]);
        assert_eq!(relocs(&t, 0x2000), vec![(0x1010, false)], "0x1ffe + 4 bytes is past 0x2000");
    }

    proptest! {
        /// Any table: the targets are the model's, each inside the image.
        #[test]
        fn relocations_match_the_model(t in proptest::collection::vec(any::<u8>(), 0..256), blocks in proptest::collection::vec((0u32..0x4000, 0u32..12, proptest::collection::vec(any::<u16>(), 0..8)), 0..4), size in 0u32..0x6000) {
            // (well-formed blocks first, then noise)
            let mut d = Vec::new();
            for (page, extra, es) in blocks {
                d.extend_from_slice(&page.to_le_bytes());
                d.extend_from_slice(&(8 + 2 * es.len() as u32 + (extra & !1)).to_le_bytes());
                for e in es { d.extend_from_slice(&e.to_le_bytes()); }
                d.extend(std::iter::repeat(0).take((extra & !1) as usize));
            }
            d.extend_from_slice(&t);
            let got = relocs(&d, size);
            prop_assert_eq!(&got, &relocs_model(&d, size));
            for (o, w) in got { let n: u64 = if w { 8 } else { 4 }; prop_assert!(o as u64 + n <= size as u64); }
        }

        /// The header-only machine check answers as the whole-file one did, for any bytes.
        #[test]
        fn machine_check_matches_whole_file(d in proptest::collection::vec(any::<u8>(), 0..0x200), lfanew in prop_oneof![0u32..0x200, any::<u32>()], m in prop_oneof![Just(0x8664u16), Just(0x14c), any::<u16>()]) {
            let mut d = d;
            if d.len() >= 0x40 {
                d[0x3c..0x40].copy_from_slice(&lfanew.to_le_bytes());
                let p = lfanew as usize;
                if p + 6 <= d.len() { d[p + 4..p + 6].copy_from_slice(&m.to_le_bytes()); }
            }
            prop_assert_eq!(machine_is_x64(&mut Cursor::new(d.clone())), whole_file(&d));
        }

        /// by_name's case-blind match is the lower-cased copy's (base_name), with or without ".dll".
        #[test]
        fn name_match_is_base_names(m in "[a-z0-9_]{1,8}(\\.dll|\\.drv|)", n in "[a-zA-Z0-9_]{1,8}(\\.dll|\\.DLL|\\.drv|)") {
            let lower = n.to_ascii_lowercase();
            prop_assert_eq!(name_matches(&m, &n), m == lower || m.trim_end_matches(".dll") == lower);
            prop_assert!(name_matches(&m, &m.to_ascii_uppercase()));
        }

        /// Sections and SizeOfProtect: what is written or protected never passes the image.
        #[test]
        fn room_is_inside_the_image(size in any::<u32>(), va in any::<u32>()) {
            let n = image_room(size, va);
            prop_assert!(va as u64 + n as u64 <= size as u64 || n == 0);
        }
    }

    /// The LDR entry's two path strings go with it, and with each rename (ldr_update).
    #[test]
    fn loader_entry_strings_are_freed() {
        let _g = crate::test_guest::enter();
        let at = mem::heap_alloc(16, true);
        ustr_at(at, "C:\\game\\plugin_review.dll");
        let b = mem::rp(at + 4);
        assert!(mem::heap_owns(b) && mem::heap_size(b) >= 2 * 26);
        ustr_free(at);
        assert_eq!(mem::rp(at + 4), 0);
        assert!(!mem::heap_free(b), "the buffer was freed already");
        // (none yet: nothing freed)
        ustr_free(at);
        mem::heap_free(at);
    }

    /// Two threads asking for one of sake's DLLs at once (every thread start does) get one image.
    #[test]
    fn hle_module_made_once_under_a_race() {
        let _g = crate::test_guest::enter();
        let bases: Vec<u32> = (0..8).map(|_| std::thread::spawn(|| hle_module("review_race_dll"))).collect::<Vec<_>>().into_iter().map(|t| t.join().unwrap()).collect();
        assert!(bases.iter().all(|&b| b == bases[0] && b != 0), "{bases:x?}");
        assert_eq!(MODULES.read().unwrap().iter().filter(|m| m.name == "review_race_dll.dll").count(), 1);
    }
}

/// Kani (formal/kani.sh): for every table of up to 3 blocks over any image size, every fixup
/// reloc_targets hands out lies inside the image and inside the table's blocks.
#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    #[kani::unwind(10)]
    fn relocation_fixups_stay_in_the_image() {
        let t: [u8; 24] = kani::any();
        let size: u32 = kani::any();
        let n: u32 = kani::any();
        kani::assume(n <= 24);
        let r32 = |o: u32| if o as usize + 4 <= n as usize { u32::from_le_bytes(t[o as usize..o as usize + 4].try_into().unwrap()) } else { 0 };
        let r16 = |o: u32| if o as usize + 2 <= n as usize { u16::from_le_bytes(t[o as usize..o as usize + 2].try_into().unwrap()) as u32 } else { 0 };
        reloc_targets(0, n, size, r32, r16, |o, w| {
            assert!(o as u64 + if w { 8 } else { 4 } <= size as u64);
        });
    }

    #[kani::proof]
    fn section_bytes_stay_in_the_image() {
        let (size, va): (u32, u32) = (kani::any(), kani::any());
        let n = image_room(size, va);
        assert!(n == 0 || va as u64 + n as u64 <= size as u64);
    }
}

#[cfg(test)]
mod resource_read_tests {
    use super::*;
    use proptest::prelude::*;

    /// A PE32 file: e_lfanew `lfanew`, `before` sections of filler, the resource section (a
    /// type 16 / name 1 / one language tree holding `value`), `after` sections more.
    fn pe_with_version(lfanew: usize, before: usize, after: usize, value: &[u8]) -> Vec<u8> {
        let count = before + 1 + after;
        let sections = lfanew + 24 + 224;
        let mut v = vec![0u8; sections + count * 40];
        v[..2].copy_from_slice(b"MZ");
        v[0x3c..0x40].copy_from_slice(&(lfanew as u32).to_le_bytes());
        v[lfanew..lfanew + 4].copy_from_slice(b"PE\0\0");
        v[lfanew + 4..lfanew + 6].copy_from_slice(&0x14cu16.to_le_bytes());
        v[lfanew + 6..lfanew + 8].copy_from_slice(&(count as u16).to_le_bytes());
        v[lfanew + 20..lfanew + 22].copy_from_slice(&224u16.to_le_bytes());
        let opt = lfanew + 24;
        v[opt..opt + 2].copy_from_slice(&0x10bu16.to_le_bytes());
        v[opt + 92..opt + 96].copy_from_slice(&16u32.to_le_bytes());
        let mut raw_at = v.len().next_multiple_of(0x200) as u32;
        let rsrc_rva = 0x1000 * (before as u32 + 1);
        for i in 0..count {
            let rva = 0x1000 * (i as u32 + 1);
            let body: Vec<u8> = if i == before {
                // root -> 16 -> 1 -> language -> data entry -> value
                let mut d = vec![0u8; 0x60];
                let dir = |d: &mut Vec<u8>, at: usize, id: u32, to: u32| {
                    d[at + 14..at + 16].copy_from_slice(&1u16.to_le_bytes());
                    d[at + 16..at + 20].copy_from_slice(&id.to_le_bytes());
                    d[at + 20..at + 24].copy_from_slice(&to.to_le_bytes());
                };
                dir(&mut d, 0, 16, 0x8000_0018);
                dir(&mut d, 0x18, 1, 0x8000_0030);
                dir(&mut d, 0x30, 0x411, 0x48);
                d[0x48..0x4c].copy_from_slice(&(rsrc_rva + 0x60).to_le_bytes());
                d[0x4c..0x50].copy_from_slice(&(value.len() as u32).to_le_bytes());
                d.extend_from_slice(value);
                d
            } else {
                vec![i as u8; 0x80 + 7 * i]
            };
            let s = sections + i * 40;
            v[s + 12..s + 16].copy_from_slice(&rva.to_le_bytes());
            v[s + 16..s + 20].copy_from_slice(&(body.len() as u32).to_le_bytes());
            v[s + 20..s + 24].copy_from_slice(&raw_at.to_le_bytes());
            if i == before {
                v[opt + 96 + 16..opt + 96 + 20].copy_from_slice(&rva.to_le_bytes());
                v[opt + 96 + 20..opt + 96 + 24].copy_from_slice(&0x60u32.to_le_bytes());
            }
            v.resize(raw_at as usize, 0);
            v.extend_from_slice(&body);
            raw_at = v.len().next_multiple_of(0x200) as u32;
        }
        v
    }
    fn read_of(data: &[u8]) -> impl FnMut(u64, usize) -> Option<Vec<u8>> + '_ {
        |at, n| {
            let at = (at as usize).min(data.len());
            Some(data[at..at.saturating_add(n).min(data.len())].to_vec())
        }
    }

    #[test]
    fn version_read_by_offset_as_from_the_whole_file() {
        for (lfanew, before, after) in [(0x80, 0, 0), (0x80, 3, 2), (0x1ff0, 1, 1), (0x40, 0, 5)] {
            let v = pe_with_version(lfanew, before, after, b"version bytes");
            assert_eq!(file_resource(&v, 16), Some(&b"version bytes"[..]));
            assert_eq!(file_resource_read(&mut read_of(&v), 16).as_deref(), Some(&b"version bytes"[..]));
            assert_eq!(file_resource_read(&mut read_of(&v), 3), None);
        }
    }

    proptest! {
        /// Any such file, then damaged at any bytes: what the pieces give is what the whole file
        /// gives whenever they give something (else the caller reads the whole of a small file),
        /// and an undamaged one always gives it; no panic either way.
        #[test]
        fn pieces_agree_with_the_whole(lfanew in 0x40usize..0x1200, before in 0usize..4, after in 0usize..4, value in prop::collection::vec(any::<u8>(), 0..64), damage in prop::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 0..6)) {
            let mut v = pe_with_version(lfanew & !3, before, after, &value);
            let first = file_resource_read(&mut read_of(&v), 16);
            prop_assert_eq!(first.as_deref(), Some(&value[..]));
            for (i, b) in damage {
                let k = i.index(v.len());
                v[k] = b;
            }
            let whole = file_resource(&v, 16).map(<[u8]>::to_vec);
            let pieces = file_resource_read(&mut read_of(&v), 16);
            if pieces.is_some() {
                prop_assert_eq!(pieces, whole);
            }
        }
    }
}
