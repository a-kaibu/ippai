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
/// Serializes LoadLibrary (DllMain calls may load more).
pub static LOADER: Mutex<()> = Mutex::new(());

pub fn by_base(base: u32) -> Option<usize> {
    MODULES.read().unwrap().iter().position(|m| m.base == base)
}

pub fn by_name(name: &str) -> Option<u32> {
    let n = base_name(name);
    MODULES.read().unwrap().iter().find(|m| m.name == n || m.name.trim_end_matches(".dll") == n).map(|m| m.base)
}

pub fn containing(a: u32) -> Option<u32> {
    MODULES.read().unwrap().iter().find(|m| a >= m.base && a < m.base + m.size).map(|m| m.base)
}

pub fn with<R>(base: u32, f: impl FnOnce(&Module) -> R) -> Option<R> {
    MODULES.read().unwrap().iter().find(|m| m.base == base).map(f)
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
    });
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
    // the first language (Japanese builds have one)
    let first = r32(langs + 16 + 4);
    (first & 0x8000_0000 == 0).then_some(root + first)
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
    let first = r32(langs + 16 + 4);
    (first & 0x8000_0000 == 0).then_some(root + first)
}

/// (address, size) of a found resource's data.
pub fn resource_data(module: u32, entry: u32) -> (u32, u32) {
    (module + r32(entry), r32(entry + 4))
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
        // each function: Windows' hot-patch prologue undone, then `mov ecx, trap; jmp ecx`:
        // ASProtect copies an API's first instructions into its own code and jumps past them, so
        // what follows the prologue must work anywhere (no relative jump)
        for (i, f) in names.iter().enumerate() {
            let i = i as u32;
            let t = base + thunks + 16 * (i + 1);
            let trap = crate::api::export(&key, f);
            mem::write(t, &[0x8b, 0xff, 0x55, 0x8b, 0xec, 0x5d, 0xb9]);
            w32(t + 7, trap);
            mem::write(t + 11, &[0xff, 0xe1, 0xcc, 0xcc, 0xcc]);
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
    });
    base
}

use crate::mem::w16;

/// The folders SetDllDirectory / AddDllDirectory added to the search (after the exe's).
pub static DLL_DIRS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// The guest file of a DLL the program ships (exe directory, then the current directory).
fn find_dll_file(dll: &str) -> Option<(String, String)> {
    let name = if dll.contains('.') { dll.to_string() } else { format!("{dll}.dll") };
    if name.contains('\\') || name.contains('/') || name.contains(':') {
        let w = crate::files::full_path(&name);
        // (x64: system32's 64-bit DLLs are C:\windows\system64 in the 32-bit prefix)
        if mem::x64() && w.len() > 20 && w[..20].eq_ignore_ascii_case("C:\\windows\\system32\\") {
            let w64 = format!("{}{}", SYSDIR64, &w[19..]);
            if let Some(h) = crate::files::resolve(&w64) {
                return Some((h, w));
            }
        }
        return crate::files::resolve(&w).map(|h| (h, w));
    }
    let exe_dir = MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.rsplit_once('\\').map_or(String::new(), |x| x.0.to_string()));
    let mut dirs: Vec<String> = exe_dir.into_iter().collect();
    dirs.extend(DLL_DIRS.lock().unwrap().iter().cloned());
    dirs.push(crate::files::cwd().trim_end_matches('\\').to_string());
    // the folders of DLLs loaded by path (LOAD_WITH_ALTERED_SEARCH_PATH finds their dependencies there)
    for m in MODULES.read().unwrap().iter().filter(|m| m.dll && !m.hle) {
        if let Some((d, _)) = m.path.rsplit_once('\\') {
            // (not system32, where Wine's own builds of the DLLs we implement also are)
            if !dirs.iter().any(|x| x.eq_ignore_ascii_case(d)) && !d.eq_ignore_ascii_case("C:\\windows\\system32") && !d.eq_ignore_ascii_case(SYSDIR64) {
                dirs.push(d.to_string());
            }
        }
    }
    // then the Wine prefix's system32 for DLLs we do not implement at all (msvcr90 and other
    // side-by-side runtimes, d3dx9_43...: Wine's PE builds)
    if !api::is_system(dll) {
        dirs.push(if mem::x64() { SYSDIR64 } else { "C:\\windows\\system32" }.into());
    }
    for d in dirs {
        let w = format!("{d}\\{name}");
        if let Some(h) = crate::files::resolve(&w) {
            // (as Windows' loader, one of the other architecture is passed over: a 64-bit game
            // shipping a 32-bit d3dcompiler_47.dll gets the system's)
            if is_64bit_file(&h) == mem::x64() {
                // (the DLLs still taken from the prefix: what sake has yet to implement)
                if d.eq_ignore_ascii_case("C:\\windows\\system32") || d.eq_ignore_ascii_case(SYSDIR64) {
                    eprintln!("sake: prefix DLL {w}");
                }
                return Some((h, w));
            }
        }
    }
    None
}

/// Where a 64-bit program's system DLLs (Wine's x86_64 PE builds) are.
const SYSDIR64: &str = "C:\\windows\\system64";

/// The C runtime's API sets are ucrtbase, which a 64-bit program loads from the prefix.
fn canon(dll: &str) -> &str {
    if mem::x64() && api::dll_key(dll) == "ucrtbase" { "ucrtbase.dll" } else { dll }
}

fn resolve_import(dll: &str, imp: Import) -> u32 {
    let dll = canon(dll);
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
    (api::is_crt(&k) && !k.starts_with("msvcp") && !k.starts_with("vcruntime")).then(|| crate::crt::data(name)).flatten()
}

fn load_dll_file(dll: &str) -> Result<u32, String> {
    let (host, win) = find_dll_file(dll).ok_or_else(|| format!("{dll} not found"))?;
    let data = kura::fs::read(&host).map_err(|e| format!("{host}: {e}"))?;
    if crate::env_on!("SAKE_TRACE_LOAD") {
        eprintln!("sake: loading {win} ({} bytes)", data.len());
    }
    let base = load(&data, &win, &mut resolve_import)?;
    crate::vorbis_hook::patch(base, with(base, |m| m.size).unwrap_or(0));
    tls_module(base);
    PENDING.lock().unwrap().push(base);
    Ok(base)
}

/// LoadLibrary: an existing module, one of ours, or a DLL file (initialized here).
pub fn load_library(cpu: &mut Cpu, name: &str) -> u32 {
    let name = canon(name);
    let _g = LOADER.lock().unwrap();
    let key = api::dll_key(name);
    if let Some(b) = by_name(name) {
        if let Some(i) = by_base(b) {
            MODULES.write().unwrap()[i].refs += 1;
        }
        return b;
    }
    let file = find_dll_file(name);
    if api::is_core(name) || (api::is_system(name) && file.is_none()) {
        return hle_module(&key);
    }
    match load_dll_file(name) {
        Ok(b) => {
            drop(_g);
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
