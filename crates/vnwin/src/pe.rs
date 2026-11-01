//! PE images: mapping, relocations, imports, exports, resources, TLS.

use std::collections::HashMap;
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

/// Maps the image in `data`; `resolve(dll, import)` gives import addresses.
pub fn load(data: &[u8], path: &str, resolve: &mut dyn FnMut(&str, Import) -> u32) -> Result<u32, String> {
    let rd16 = |o: usize| -> u32 { data.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]) as u32) };
    let rd32 = |o: usize| -> u32 { data.get(o..o + 4).map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])) };
    if rd16(0) != 0x5a4d {
        return Err("not an MZ file".into());
    }
    let pe = rd32(0x3c) as usize;
    if rd32(pe) != 0x4550 || rd16(pe + 4) != 0x14c {
        return Err("not a 32-bit x86 PE".into());
    }
    let nsec = rd16(pe + 6) as usize;
    let chars = rd16(pe + 22);
    let opt = pe + 24;
    let optsize = rd16(pe + 20) as usize;
    if rd16(opt) != 0x10b {
        return Err("not PE32".into());
    }
    let entry_rva = rd32(opt + 16);
    let pref = rd32(opt + 28);
    let size = rd32(opt + 56);
    let hdr_size = rd32(opt + 60);
    let dir = |i: usize| (rd32(opt + 96 + 8 * i), rd32(opt + 100 + 8 * i));
    let dll = chars & 0x2000 != 0;
    let relocs = dir(5);
    let base = match mem::reserve(pref, size, 0x80, 0x100_0000, false) {
        Some(b) => b,
        None if relocs.0 != 0 && chars & 1 == 0 => mem::reserve(0, size, 0x80, 0x100_0000, false).ok_or("no room for the image")?,
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
    }
    // relocations
    let delta = base.wrapping_sub(pref);
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
                if e >> 12 == 3 {
                    let a = base + page + (e & 0xfff) as u32;
                    w32(a, r32(a).wrapping_add(delta));
                }
            }
            at += len;
        }
    }
    let name = base_name(path);
    // exports
    let mut exports = HashMap::new();
    let mut ordinals = HashMap::new();
    let (erva, esize) = dir(0);
    if erva != 0 {
        let e = base + erva;
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
        Tls { start: r32(t), end: r32(t + 4), index_addr: r32(t + 8), callbacks: r32(t + 12), zero_fill: r32(t + 16), index: 0 }
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
    });
    // imports (after the module is listed: a DLL importing from itself, or a cycle)
    let (irva, _) = dir(1);
    if irva != 0 {
        let mut d = base + irva;
        while r32(d + 12) != 0 {
            let dll_name = String::from_utf8_lossy(&mem::cstr(base + r32(d + 12))).into_owned();
            let lookup = if r32(d) != 0 { r32(d) } else { r32(d + 16) };
            let iat = base + r32(d + 16);
            let mut k = 0;
            loop {
                let v = r32(base + lookup + 4 * k);
                if v == 0 {
                    break;
                }
                let a = if v & 0x8000_0000 != 0 {
                    resolve(&dll_name, Import::Ordinal(v & 0xffff))
                } else {
                    let n = String::from_utf8_lossy(&mem::cstr(base + v + 2)).into_owned();
                    resolve(&dll_name, Import::Name(&n))
                };
                w32(iat + 4 * k, a);
                k += 1;
            }
            d += 20;
        }
    }
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

use vnx86::Cpu;

/// DLLs loaded but not initialized yet (DllMain), dependencies first.
static PENDING: Mutex<Vec<u32>> = Mutex::new(Vec::new());
/// TLS index of the next module with a TLS directory
static TLS_NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The module handle of one of our DLLs (made on first use: a header page so code reading
/// the headers finds an image).
pub fn hle_module(dll: &str) -> u32 {
    let name = format!("{}.dll", crate::api::dll_key(dll));
    if let Some(b) = MODULES.read().unwrap().iter().find(|m| m.name == name).map(|m| m.base) {
        return b;
    }
    let base = mem::reserve(0, 0x1000, 2, 0x100_0000, true).expect("module");
    mem::commit(base, 2);
    // MZ + PE headers with no sections, directories or exports
    w16(base, 0x5a4d);
    w32(base + 0x3c, 0x40);
    w32(base + 0x40, 0x4550);
    w16(base + 0x44, 0x14c);
    w16(base + 0x54, 0xe0);
    w16(base + 0x56, 0x2102);
    w16(base + 0x58, 0x10b);
    w32(base + 0x58 + 28, base);
    w32(base + 0x58 + 56, 0x1000);
    MODULES.write().unwrap().push(Module {
        base,
        size: 0x1000,
        entry: 0,
        name: name.clone(),
        path: format!("C:\\windows\\system32\\{name}"),
        exports: HashMap::new(),
        ordinals: HashMap::new(),
        dll: true,
        tls: None,
        resources: 0,
        refs: 1,
        hle: true,
    });
    base
}

use crate::mem::w16;

/// The guest file of a DLL the program ships (exe directory, then the current directory).
fn find_dll_file(dll: &str) -> Option<(String, String)> {
    let name = if dll.contains('.') { dll.to_string() } else { format!("{dll}.dll") };
    if name.contains('\\') || name.contains('/') || name.contains(':') {
        let w = crate::files::full_path(&name);
        return crate::files::resolve(&w).map(|h| (h, w));
    }
    let exe_dir = MODULES.read().unwrap().iter().find(|m| !m.dll).map(|m| m.path.rsplit_once('\\').map_or(String::new(), |x| x.0.to_string()));
    let mut dirs: Vec<String> = exe_dir.into_iter().collect();
    dirs.push(crate::files::cwd().trim_end_matches('\\').to_string());
    for d in dirs {
        let w = format!("{d}\\{name}");
        if let Some(h) = crate::files::resolve(&w) {
            return Some((h, w));
        }
    }
    None
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
                eprintln!("vnwin: import from {dll}: {e}");
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
        eprintln!("vnwin: {dll} has no export {name}");
        api::export(dll, &name)
    })
}

use crate::api;

/// GetProcAddress
pub fn proc_address(base: u32, imp: Import) -> Option<u32> {
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
    let a = api::export(&name, &n);
    (api::implemented(a) || std::env::var_os("VNWIN_ALL_EXPORTS").is_some()).then(|| crate::fast::bind(&name, &n, a))
}

/// A data export of a DLL we implement (msvcrt's variables).
fn hle_data(dll: &str, name: &str) -> Option<u32> {
    (api::dll_key(dll) == "msvcrt").then(|| crate::crt::data(name)).flatten()
}

fn load_dll_file(dll: &str) -> Result<u32, String> {
    let (host, win) = find_dll_file(dll).ok_or_else(|| format!("{dll} not found"))?;
    let data = std::fs::read(&host).map_err(|e| format!("{host}: {e}"))?;
    let base = load(&data, &win, &mut resolve_import)?;
    tls_module(base);
    PENDING.lock().unwrap().push(base);
    Ok(base)
}

/// LoadLibrary: an existing module, one of ours, or a DLL file (initialized here).
pub fn load_library(cpu: &mut Cpu, name: &str) -> u32 {
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
            if std::env::var_os("VNWIN_TRACE_LOAD").is_some() {
                eprintln!("vnwin: LoadLibrary({name}): {e}");
            }
            0
        }
    }
}

pub fn load_exe(_cpu: &mut Cpu, host: &str, win: &str) -> Result<u32, String> {
    let data = std::fs::read(host).map_err(|e| e.to_string())?;
    let base = load(&data, win, &mut resolve_import)?;
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
    w32(r32(teb + 0x2c) + 4 * t.index, b);
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
        p += 4;
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
            let ok = crate::call(cpu, entry, &[base, 1, 0]);
            if std::env::var_os("VNWIN_TRACE_LOAD").is_some() {
                eprintln!("vnwin: DllMain of {:#x}: {ok}", base);
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
