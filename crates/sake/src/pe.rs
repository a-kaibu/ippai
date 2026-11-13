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
}

/// The module mapped at the low alias of a base above 4G: (low base, upper half, size).
static ALIAS: RwLock<(u32, u32, u32)> = RwLock::new((0, 0, 0));

pub fn alias() -> (u32, u32, u32) {
    *ALIAS.read().unwrap()
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

pub fn by_name(name: &str) -> Option<u32> {
    let n = base_name(name);
    MODULES.read().unwrap().iter().find(|m| !m.redirected && (m.name == n || m.name.trim_end_matches(".dll") == n)).map(|m| m.base)
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

/// LoadLibraryEx(LOAD_LIBRARY_AS_DATAFILE / AS_IMAGE_RESOURCE): the file's headers and
/// sections laid out as an image (any bitness; no imports, relocations or entry point) for its
/// resources, read now (a later change to the file is seen by the next load); listed without
/// a name, so GetModuleHandle never finds it. The base.
pub fn load_datafile(data: &[u8], path: &str) -> Option<u32> {
    let pe = crate::resupdate::Pe::parse(data)?;
    let secs = pe.sections(data);
    let headers = secs.iter().filter(|s| s.raw_size != 0).map(|s| s.raw as usize).min().unwrap_or(0x1000).min(0x1000).min(data.len());
    let size = secs.iter().map(|s| s.va.saturating_add(s.vsize.max(s.raw_size))).max().unwrap_or(0).max(headers as u32).next_multiple_of(0x1000);
    let base = mem::reserve(0, size, 0x02, 0x100_0000, false)?;
    mem::commit(base, 0x04);
    mem::write(base, &data[..headers]);
    for s in &secs {
        let n = s.raw_size.min(if s.vsize != 0 { s.vsize } else { s.raw_size }) as usize;
        let n = n.min(data.len().saturating_sub(s.raw as usize));
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

/// Whether the file at host path `path` is a 64-bit (x86-64) PE.
pub fn is_64bit_file(path: &str) -> bool {
    let Ok(d) = kura::fs::read(path) else { return false };
    let pe = d.get(0x3c..0x40).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) as usize;
    d.get(pe + 4..pe + 6) == Some(&[0x64, 0x86])
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

/// Maps the image in `data`; `resolve(dll, import)` gives import addresses.
pub fn load(data: &[u8], path: &str, resolve: &mut dyn FnMut(&str, Import) -> u32) -> Result<u32, String> {
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
        None if can_move || (dll && entry_rva == 0) => mem::reserve(0, size, 0x80, 0x100_0000, false).ok_or("no room for the image")?,
        // above 4G and fixed: memory is addressed modulo 4G, so the image lives at its base's
        // low half and RIP keeps the upper one (koji Cpu::rip_hi); addresses handed to the
        // program in that range get it back (mem::widen)
        None if pref >> 32 != 0 && alias().2 == 0 => {
            let b = mem::reserve(low, size, 0x80, 0x100_0000, false).ok_or_else(|| format!("cannot map at {pref:#x} (its low alias {low:#x} is taken)"))?;
            *ALIAS.write().unwrap() = (b, (pref >> 32) as u32, size);
            b
        }
        None => return Err(format!("cannot map at {pref:#x} (no relocations)")),
    };
    mem::commit(base, 0x40);
    mem::write(base, &data[..(hdr_size as usize).min(data.len())]);
    let secs = opt + optsize;
    for i in 0..nsec {
        let s = secs + 40 * i;
        let vsize = rd32(s + 8);
        let va = rd32(s + 12);
        let raw = rd32(s + 16) as usize;
        let ptr = rd32(s + 20) as usize;
        let n = raw.min(if vsize != 0 { vsize as usize } else { raw }).min(data.len().saturating_sub(ptr));
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
        let (mut at, end) = (base + relocs.0, base + relocs.0 + relocs.1);
        while at + 8 <= end {
            let page = r32(at);
            let len = r32(at + 4);
            if len < 8 {
                break;
            }
            for k in 0..(len - 8) / 2 {
                let e = r16(at + 8 + 2 * k);
                let a = base + page + (e & 0xfff) as u32;
                match e >> 12 {
                    3 => w32(a, r32(a).wrapping_add(delta as u32)),
                    10 => mem::w64(a, mem::r64(a).wrapping_add(delta)),
                    _ => {}
                }
            }
            at += len;
        }
    }
    let name = base_name(path);
    // exports
    let mut exports = HashMap::default();
    let mut ordinals = HashMap::default();
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
                if let Some(o) = n.strip_prefix('#') {
                    resolve(&format!("{d}.dll"), Import::Ordinal(o.parse::<u32>().unwrap_or(0)))
                } else {
                    resolve(&format!("{d}.dll"), Import::Name(n))
                }
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
            d += 20;
        }
    }
    // each section's protection from its characteristics (the image was mapped read-write-execute
    // to be written): the JIT translates only pages that are not both writable and executable
    mem::protect(base, hdr_size.max(1), 0x02);
    for i in 0..nsec {
        let s = secs + 40 * i;
        let (vsize, va, ch) = (rd32(s + 8), rd32(s + 12), rd32(s + 36));
        let n = if vsize != 0 { vsize } else { rd32(s + 16) };
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

/// Where the exe's entry and threads return: a stub in kernel32's image that goes on to
/// EXIT_TRAP (0 until kernel32 is made).
static EXIT_THUNK: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

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

/// The module handle of one of our DLLs (made on first use: a header page so code reading
/// the headers finds an image).
pub fn hle_module(dll: &str) -> u32 {
    let name = format!("{}.dll", crate::api::dll_key(dll));
    if let Some(b) = MODULES.read().unwrap().iter().find(|m| m.name == name).map(|m| m.base) {
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
    let size = if n == 0 { 0x1000 } else { (thunks + 16 * (n + 1) + 0xfff) & !0xfff };
    let base = mem::reserve(0, size, 2, 0x100_0000, true).expect("module");
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
        resources: 0,
        refs: 1,
        hle: true,
        pdata: (0, 0),
        redirected: false,
    });
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
}

/// A module's flags or path changed after it was listed.
pub fn ldr_update(base: u32) {
    let Some(e) = LDR.lock().unwrap().1.iter().find(|x| x.0 == base).map(|x| x.1) else { return };
    let Some((path, redirected)) = with(base, |m| (m.path.clone(), m.redirected)) else { return };
    let ps = mem::ps();
    let (full, basen, flags) = if ps == 8 { (72, 88, 104) } else { (36, 44, 52) };
    ustr_at(e + full, &path);
    ustr_at(e + basen, path.rsplit('\\').next().unwrap_or(&path));
    let f = r32(e + flags) & !LDR_REDIRECTED;
    w32(e + flags, f | if redirected { LDR_REDIRECTED } else { 0 });
}

use crate::mem::w16;

/// The folders SetDllDirectory / AddDllDirectory added to the search (after the exe's).
pub static DLL_DIRS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// A guest DLL, including third-party runtimes installed in system32 / SysWOW64.
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
        // A 32-bit runtime on a 64-bit prefix normally lives in SysWOW64;
        // architecture checks below also allow a 32-bit-only system32 prefix.
        if !mem::x64() {
            dirs.push("C:\\windows\\SysWOW64".into());
        }
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

/// C:\windows\system32 or C:\windows\SysWOW64 (a game run from there, or one that adds it with
/// SetDllDirectory, still gets sake's DLLs only)
fn is_system_dir(d: &str) -> bool {
    let d = crate::files::full_path(d).to_ascii_lowercase();
    let d = d.trim_end_matches('\\');
    d == "c:\\windows\\system32" || d == "c:\\windows\\syswow64" || d == "c:\\windows\\system"
}


fn resolve_import(dll: &str, imp: Import) -> u32 {
    let hle = api::is_core(dll) || (api::is_system(dll) && find_dll_file(dll).is_none());
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
            Ok(b) => b,
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
    let base = load(&data, &win, &mut resolve_import)?;
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
        if let Some(i) = by_base(b).filter(|_| b != 0) {
            MODULES.write().unwrap()[i].refs += 1;
            return b;
        }
        if let Ok(b) = load_dll_file(&path) {
            if let Some(i) = by_base(b) {
                MODULES.write().unwrap()[i].redirected = true;
                ldr_update(b);
            }
            init_dlls(cpu);
            return b;
        }
    }
    if let Some(b) = by_name(name) {
        if let Some(i) = by_base(b) {
            MODULES.write().unwrap()[i].refs += 1;
        }
        return b;
    }
    let file = find_dll_file(name);
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
            if let Some(i) = by_base(b) {
                MODULES.write().unwrap()[i].path = p;
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
    let base = load(&data, win, &mut resolve_import)?;
    // the main exe: the JIT translates its writable code too, checking stores into it
    let size = with(base, |m| m.size).unwrap_or(0);
    kura::jit::exe_image(base, size);
    crate::movie::patch(base, size);
    crate::vorbis_hook::patch(base, size);
    tls_module(base);
    Ok(base)
}

/// Implicit TLS of a module just loaded: its index, and the calling thread's block.
fn tls_module(base: u32) {
    let Some(i) = by_base(base) else { return };
    let mut mods = MODULES.write().unwrap();
    let Some(t) = mods[i].tls.as_mut() else { return };
    t.index = TLS_NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if t.index_addr != 0 {
        w32(t.index_addr, t.index);
    }
    let t = *t;
    drop(mods);
    tls_block(crate::cur().teb, &t);
}

fn tls_block(teb: u32, t: &Tls) {
    let n = t.end.saturating_sub(t.start);
    let b = mem::heap_alloc(n + t.zero_fill + 16, true);
    mem::copy(b, t.start, n);
    mem::wp(r32(teb + crate::teb_tls_pointer()) + mem::ps() * t.index, b);
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
