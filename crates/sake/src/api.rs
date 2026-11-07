//! The import boundary: every Win32 function the game imports is a trap address. A call
//! there leaves the translated code; `dispatch` runs the Rust implementation with the
//! arguments still on the guest stack and returns to the caller as stdcall/cdecl would.

use crate::{HashMap, HashSet};
use std::sync::{Mutex, OnceLock, RwLock};

use koji::{Cpu, EAX, EDX, ESP};

use crate::mem::{r32, w32};

/// Trap range of the x86 core: [TRAP_LO, TRAP_HI).
pub const TRAP_LO: u32 = 0xfe00_0000;
pub const TRAP_HI: u32 = 0xffff_1000;
/// Return address of calls from Rust into guest code.
pub const RET_TRAP: u32 = 0xffff_0000;
/// Where the guest's exception dispatch loop reports a handler's result (see seh.rs).
pub const SEH_TRAP: u32 = 0xffff_0010;
/// Threads end by returning here.
pub const EXIT_TRAP: u32 = 0xffff_0020;
/// x64: where a language-specific exception handler returns to the dispatcher (seh64.rs).
pub const SEH64_TRAP: u32 = 0xffff_0030;

pub type Handler = fn(&mut Ctx) -> u32;

pub struct Ctx<'a> {
    pub cpu: &'a mut Cpu,
    /// ESP at the call (the return address)
    pub esp: u32,
    pub edx: Option<u32>,
    /// the handler has set EIP/ESP itself (no return to the caller)
    pub jumped: bool,
    /// a double for ST0 (functions returning floating point)
    pub st0: Option<f64>,
    /// the trap address called (handlers shared by many entries tell them apart by it)
    pub trap: u32,
}

impl Ctx<'_> {
    #[inline]
    pub fn a(&self, i: u32) -> u32 {
        if crate::mem::x64() {
            return self.a64(i) as u32;
        }
        r32(self.esp + 4 + 4 * i)
    }
    /// x64: the whole argument `i` (RCX, RDX, R8, R9, then the stack past the home space).
    pub fn a64(&self, i: u32) -> u64 {
        if i < 4 { self.cpu.gpr([1, 2, 8, 9][i as usize]) } else { crate::mem::r64(self.esp + 8 + 8 * i) }
    }
    /// x64: floating-point argument `i` (XMM0-3, then the stack), as a double's bits.
    pub fn xmm(&self, i: u32) -> u64 {
        if i < 4 { self.cpu.xmm(i as usize) as u64 } else { crate::mem::r64(self.esp + 8 + 8 * i) }
    }
    /// A POINT passed by value at slots `i`, `i + 1` (x64: the one 8-byte argument `i`; the
    /// index is the same when only 4-byte arguments come before it).
    pub fn pt(&self, i: u32) -> (i32, i32) {
        if crate::mem::x64() {
            let v = self.a64(i);
            return (v as u32 as i32, (v >> 32) as u32 as i32);
        }
        (self.i(i), self.i(i + 1))
    }
    /// The float argument `i` (x64: XMM0-3, then the stack slot's low half).
    pub fn f(&self, i: u32) -> f32 {
        if crate::mem::x64() {
            return f32::from_bits(self.xmm(i) as u32);
        }
        f32::from_bits(self.a(i))
    }
    /// A D2D1_POINT_2F passed by value at slots `i`, `i + 1` (x64: the one 8-byte argument
    /// `i`); and the slot after it.
    pub fn pt_f(&self, i: u32) -> ((f32, f32), u32) {
        if crate::mem::x64() {
            let v = self.a64(i);
            return ((f32::from_bits(v as u32), f32::from_bits((v >> 32) as u32)), i + 1);
        }
        ((f32::from_bits(self.a(i)), f32::from_bits(self.a(i + 1))), i + 2)
    }
    /// Calls guest code at `f` with `args`; the float arguments `floats` (their indexes in
    /// `args`, which hold their bits) go in XMM registers as well on x64.
    pub fn call_f(&mut self, f: u32, args: &[u32], floats: &[usize]) -> u32 {
        if crate::mem::x64() {
            for &i in floats {
                if i < 4 {
                    self.cpu.set_xmm(i, args[i] as u128);
                }
            }
        }
        crate::call(self.cpu, f, args)
    }
    pub fn i(&self, i: u32) -> i32 {
        self.a(i) as i32
    }
    /// The double at 32-bit stack slots i, i+1 (x64: argument i/2, the double's register)
    pub fn f64(&self, i: u32) -> f64 {
        if crate::mem::x64() {
            return f64::from_bits(self.xmm(i / 2));
        }
        f64::from_bits(self.a(i) as u64 | (self.a(i + 1) as u64) << 32)
    }
    /// Calls guest code at `f` with `args` (stdcall or cdecl alike); returns EAX.
    pub fn call(&mut self, f: u32, args: &[u32]) -> u32 {
        crate::call(self.cpu, f, args)
    }
    pub fn ret64(&mut self, v: u64) -> u32 {
        self.edx = Some((v >> 32) as u32);
        v as u32
    }
}

struct Entry {
    dll: String,
    name: String,
    f: Option<Handler>,
    bytes: u32,
    cdecl: bool,
    known: bool,
}

static ENTRIES: RwLock<Vec<Entry>> = RwLock::new(Vec::new());
static INDEX: Mutex<Option<HashMap<(String, String), u32>>> = Mutex::new(None);
static IMPLS: RwLock<Option<HashMap<String, Handler>>> = RwLock::new(None);

struct Spec {
    /// (dll, name) -> (cdecl, bytes)
    by_name: HashMap<(String, String), (bool, u32)>,
    by_ord: HashMap<(String, u32), String>,
    dlls: HashSet<String>,
}

fn specs() -> &'static Spec {
    static S: OnceLock<Spec> = OnceLock::new();
    S.get_or_init(|| {
        let mut s = Spec { by_name: HashMap::default(), by_ord: HashMap::default(), dlls: HashSet::default() };
        for l in include_str!("specs.txt").lines() {
            let v: Vec<&str> = l.split(' ').collect();
            if v.len() < 5 {
                continue;
            }
            // (thiscall: `this` in ECX, the callee pops the rest)
            let cdecl = !matches!(v[2], "stdcall" | "thiscall");
            let bytes: u32 = v[3].parse::<u32>().unwrap_or(0).saturating_sub(if v[2] == "thiscall" { 4 } else { 0 });
            s.dlls.insert(v[0].to_string());
            s.by_name.entry((v[0].to_string(), v[1].to_string())).or_insert((cdecl, bytes));
            if let Ok(o) = v[4].parse::<u32>() {
                s.by_ord.insert((v[0].to_string(), o), v[1].to_string());
            }
        }
        for d in ["sake"] {
            s.dlls.insert(d.into());
        }
        s
    })
}

/// The functions the spec lists for `dll` (a key: "kernel32"), sorted.
pub fn spec_names(dll: &str) -> Vec<String> {
    let mut v: Vec<String> = specs().by_name.keys().filter(|(d, _)| d == dll).map(|(_, n)| n.clone()).collect();
    v.sort();
    v
}

/// Whether a system DLL has this export on Windows: its spec lists it, or we implement it under
/// that DLL's own name (DLLs without a spec: assumed). find_impl's forwarder fallback (kernel32
/// <-> ntdll, msvcrt) is for imports; GetProcAddress on ntdll or msvcrt for a kernel32 function
/// gives NULL on Windows, and mono takes the first module that answers
pub fn exported_by(dll: &str, name: &str) -> bool {
    let d = dll_key(dll);
    let sp = specs();
    !sp.dlls.contains(&d)
        || sp.by_name.contains_key(&(d.clone(), name.to_string()))
        || IMPLS.read().unwrap().as_ref().is_some_and(|m| m.contains_key(&format!("{d}!{name}")))
}

/// "KERNEL32.dll" -> "kernel32"
pub fn dll_key(dll: &str) -> String {
    let d = crate::pe::base_name(dll);
    let k = [".dll", ".drv", ".ax", ".ocx"].iter().find_map(|x| d.strip_suffix(x)).unwrap_or(&d);
    // API sets (Wine's PE DLLs and VC++ 2015+ programs import these)
    if k.starts_with("api-ms-win-crt-") {
        return "ucrtbase".into();
    }
    // (Windows' security.dll forwards everything to secur32)
    if k == "security" {
        return "secur32".into();
    }
    for (pre, dll) in [("api-ms-win-core-winrt-", "combase"), ("api-ms-win-core-com-", "ole32"), ("api-ms-win-core-registry-", "advapi32"), ("api-ms-win-core-shlwapi-", "shlwapi")] {
        if k.starts_with(pre) {
            return dll.into();
        }
    }
    if k.starts_with("api-ms-win-core-") || k.starts_with("api-ms-win-appmodel-") || k.starts_with("ext-ms-win-kernel32-") {
        return "kernelbase".into();
    }
    // (the others by the DLL that implements them)
    for (pre, dll) in [("ntuser-", "user32"), ("rtcore-ntuser-", "user32"), ("gdi-", "gdi32"), ("security-", "advapi32"), ("shell-", "shell32"), ("shcore-", "shcore")] {
        if k.strip_prefix("api-ms-win-").or_else(|| k.strip_prefix("ext-ms-win-")).is_some_and(|r| r.starts_with(pre)) {
            return dll.into();
        }
    }
    k.to_string()
}

/// Whether `dll` is one we implement (or at least know the exports of).
pub fn is_system(dll: &str) -> bool {
    let k = dll_key(dll);
    specs().dlls.contains(&k) || (crate::mem::x64() && matches!(k.as_str(), "msvcp140" | "vcruntime140_1"))
}

/// The C/C++ runtimes.
pub fn is_crt(key: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || b == b'_');
    matches!(key, "msvcrt" | "ucrtbase" | "crtdll")
        || key.strip_prefix("msvcr").is_some_and(digits)
        || key.strip_prefix("msvcp").is_some_and(digits)
        || key.starts_with("vcruntime")
}

/// DLLs we implement even when the game ships its own copy.
pub fn is_core(dll: &str) -> bool {
    // (x64: the C runtime and vcruntime too, which the C++ exception support is part of)
    if crate::mem::x64() && matches!(dll_key(dll).as_str(), "ucrtbase" | "vcruntime140" | "vcruntime140_1") {
        return true;
    }
    matches!(
        dll_key(dll).as_str(),
        "kernel32" | "kernelbase" | "ntdll" | "user32" | "gdi32" | "advapi32" | "ole32" | "oleaut32" | "comctl32"
            | "shell32" | "version" | "winmm" | "imm32" | "dsound" | "ddraw" | "comdlg32" | "shlwapi" | "msvcrt" | "msvfw32" | "msimg32"
    )
}

pub fn implement(dll: &str, name: &str, f: Handler) {
    IMPLS.write().unwrap().get_or_insert_with(HashMap::default).insert(format!("{dll}!{name}"), f);
}

pub fn find_impl(dll: &str, name: &str) -> Option<Handler> {
    let g = IMPLS.read().unwrap();
    let m = g.as_ref()?;
    if let Some(f) = m.get(&format!("{dll}!{name}")) {
        return Some(*f);
    }
    // kernel32 <-> kernelbase <-> ntdll forwarders, and the DLLs that export another's functions
    for d in forwards(dll).iter().chain(&["kernel32", "kernelbase", "ntdll", "msvcrt", "user32"]) {
        if let Some(f) = m.get(&format!("{d}!{name}")) {
            return Some(*f);
        }
    }
    // the UCRT's _o_ exports (api-ms-win-crt-private, for Windows' own DLLs) are its plain ones
    if dll == "ucrtbase" {
        if let Some(plain) = name.strip_prefix("_o_") {
            drop(g);
            return find_impl(dll, plain);
        }
    }
    None
}

/// The DLLs whose implementations `dll` (a key) also exports: shcore has shlwapi's string,
/// registry and stream helpers (shlwapi forwards them there since Windows 8), olepro32 is
/// oleaut32's OLE Automation picture/font functions, xaudio2_9 is xaudio2_8 (and both carry
/// X3DAudio, x3daudio1_7's).
fn forwards(dll: &str) -> &'static [&'static str] {
    match dll {
        "shcore" => &["shlwapi"],
        "olepro32" => &["oleaut32"],
        "xaudio2_9" => &["xaudio2_8", "x3daudio1_7"],
        "xaudio2_8" => &["x3daudio1_7"],
        _ => &[],
    }
}

/// Whether a system DLL exports `name` (specs.txt).
pub fn exports(dll: &str, name: &str) -> bool {
    specs().by_name.contains_key(&(dll_key(dll), name.to_string()))
}

/// Name of ordinal `ord` of a system DLL.
pub fn ordinal_name(dll: &str, ord: u32) -> Option<String> {
    specs().by_ord.get(&(dll_key(dll), ord)).cloned()
}

/// The trap address of a system DLL's export (made on first use).
pub fn export(dll: &str, name: &str) -> u32 {
    let d = dll_key(dll);
    let key = (d.clone(), name.to_string());
    let mut idx = INDEX.lock().unwrap();
    let idx = idx.get_or_insert_with(HashMap::default);
    if let Some(&i) = idx.get(&key) {
        return TRAP_LO + 4 * i;
    }
    let sp = specs();
    let spec = sp.by_name.get(&key).copied().or_else(|| {
        forwards(&d).iter().chain(&["kernel32", "kernelbase", "ntdll"]).find_map(|k| sp.by_name.get(&(k.to_string(), name.to_string())).copied())
    });
    let f = find_impl(&d, name);
    let mut e = ENTRIES.write().unwrap();
    let i = e.len() as u32;
    e.push(Entry {
        dll: d,
        name: name.to_string(),
        f,
        bytes: spec.map_or(0, |s| s.1),
        cdecl: spec.is_some_and(|s| s.0),
        known: spec.is_some() || f.is_some(),
    });
    idx.insert(key, i);
    TRAP_LO + 4 * i
}

/// "dll!name" of a trap address (for logs).
pub fn name_of(addr: u32) -> Option<String> {
    let i = addr.checked_sub(TRAP_LO)? / 4;
    ENTRIES.read().unwrap().get(i as usize).map(|e| format!("{}!{}", e.dll, e.name))
}

/// Whether the function at a trap address is cdecl, and its stdcall argument bytes.
pub fn args_of(addr: u32) -> (bool, u32) {
    let i = (addr.wrapping_sub(TRAP_LO) / 4) as usize;
    ENTRIES.read().unwrap().get(i).map_or((true, 0), |e| (e.cdecl, e.bytes))
}

/// The DLL key ("ucrtbase", "msvcrt"...) of a trap address.
pub fn dll_of(addr: u32) -> Option<String> {
    let i = addr.checked_sub(TRAP_LO)? / 4;
    ENTRIES.read().unwrap().get(i as usize).map(|e| e.dll.clone())
}

/// Whether the API at trap address `addr` has an implementation.
/// The trap is a function of the DLL's spec (implemented or not).
pub fn is_known(addr: u32) -> bool {
    let i = (addr.wrapping_sub(TRAP_LO) / 4) as usize;
    ENTRIES.read().unwrap().get(i).is_some_and(|e| e.known)
}
/// The handler of a trap address.
pub fn handler_of(addr: u32) -> Option<Handler> {
    let i = (addr.wrapping_sub(TRAP_LO) / 4) as usize;
    ENTRIES.read().unwrap().get(i).and_then(|e| e.f)
}

pub fn implemented(addr: u32) -> bool {
    let i = (addr.wrapping_sub(TRAP_LO) / 4) as usize;
    ENTRIES.read().unwrap().get(i).is_some_and(|e| e.f.is_some())
}

pub fn trace() -> u32 {
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| std::env::var("SAKE_TRACE").ok().and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// Counting API calls by name (SAKE_API_STATS=<seconds>).
static COUNTS: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);

fn stats_on() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| {
        let Some(s) = std::env::var("SAKE_API_STATS").ok().and_then(|s| s.parse::<f64>().ok()) else { return false };
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs_f64(s));
            let m = COUNTS.lock().unwrap().take().unwrap_or_default();
            let mut v: Vec<(u32, u64)> = m.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            let total: u64 = v.iter().map(|x| x.1).sum();
            let top: Vec<String> = v.iter().take(40).map(|(i, n)| format!("{}={n}", name_of(TRAP_LO + 4 * i).unwrap_or_default())).collect();
            eprintln!("sake api {total} calls/{s}s: {}", top.join(" "));
        });
        true
    })
}

/// SAKE_TRACE_FILTER=a,b: trace only functions whose name contains one of these.
fn trace_match(i: u32) -> bool {
    // SAKE_TRACE_AFTER=<ms>: only from then on
    static AFTER: OnceLock<u32> = OnceLock::new();
    let after = *AFTER.get_or_init(|| std::env::var("SAKE_TRACE_AFTER").ok().and_then(|s| s.parse().ok()).unwrap_or(0));
    if after > 0 && crate::k32::ticks_ms() - 60_000 < after {
        return false;
    }
    static F: OnceLock<Vec<String>> = OnceLock::new();
    let f = F.get_or_init(|| std::env::var("SAKE_TRACE_FILTER").map(|s| s.split(',').map(String::from).collect()).unwrap_or_default());
    if f.is_empty() {
        return true;
    }
    static HIT: Mutex<Option<HashMap<u32, bool>>> = Mutex::new(None);
    let mut g = HIT.lock().unwrap();
    *g.get_or_insert_with(HashMap::default).entry(i).or_insert_with(|| {
        let n = name_of(TRAP_LO + 4 * i).unwrap_or_default();
        f.iter().any(|x| n.contains(x.as_str()))
    })
}

thread_local! {
    static RING: std::cell::RefCell<std::collections::VecDeque<String>> = const { std::cell::RefCell::new(std::collections::VecDeque::new()) };
}

/// Prints (and clears) this thread's last API calls (SAKE_TRACE=3).
pub fn dump_ring(why: &str) {
    RING.with(|r| {
        let Ok(mut r) = r.try_borrow_mut() else { return };
        if r.is_empty() {
            return;
        }
        eprintln!("sake: last API calls before {why}:");
        for l in r.drain(..) {
            eprintln!("  {l}");
        }
    });
}

/// (IPPAI_API_COUNTS: calls by trap index)
static USAGE: Mutex<Vec<u64>> = Mutex::new(Vec::new());

fn usage(i: u32) {
    {
        let mut u = USAGE.lock().unwrap();
        if u.is_empty() {
            kura::usage::more(|| {
                let u = USAGE.lock().unwrap().clone();
                let e = ENTRIES.read().unwrap();
                u.iter()
                    .enumerate()
                    .filter(|x| *x.1 > 0)
                    .filter_map(|(i, &n)| {
                        let x = e.get(i)?;
                        let stub = x.f.is_none() || crate::com::is_stub(TRAP_LO + 4 * i as u32);
                        Some((format!("{}:{}!{}", if stub { "stub" } else { "api" }, x.dll, x.name), n))
                    })
                    .collect()
            });
        }
        if u.len() <= i as usize {
            u.resize(i as usize + 1, 0);
        }
        u[i as usize] += 1;
    }
    kura::usage::tick();
}

pub fn dispatch(cpu: &mut Cpu, addr: u32) {
    let i = (addr - TRAP_LO) / 4;
    let (f, bytes, cdecl, known) = {
        let e = ENTRIES.read().unwrap();
        let Some(e) = e.get(i as usize) else {
            crate::fatal(cpu, &format!("jump to unknown trap address {addr:#x}"));
        };
        (e.f, e.bytes, e.cdecl, e.known)
    };
    if kura::usage::on() {
        usage(i);
    }
    if stats_on() {
        *COUNTS.lock().unwrap().get_or_insert_with(HashMap::default).entry(i).or_default() += 1;
    }
    let esp = cpu.regs[ESP];
    {
        static W: OnceLock<u32> = OnceLock::new();
        static LAST: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let w = *W.get_or_init(|| std::env::var("SAKE_WATCH").ok().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0));
        if w != 0 {
            let v = r32(w);
            if LAST.swap(v, std::sync::atomic::Ordering::Relaxed) != v {
                eprintln!("sake watch {w:#x} = {v:#x} before {}, from {:x}", name_of(addr).unwrap_or_default(), r32(esp));
            }
        }
    }
    let tr = trace();
    if tr > 0 && trace_match(i) {
        let n = name_of(addr).unwrap_or_default();
        let x64 = crate::mem::x64();
        let nargs = if x64 { 4.max(bytes / 4).min(8) } else { (bytes / 4).min(8) };
        let args: Vec<String> = (0..nargs)
            .map(|k| if x64 { format!("{:x}", Ctx { cpu: &mut *cpu, esp, edx: None, jumped: false, st0: None, trap: addr }.a64(k)) } else { format!("{:x}", r32(esp + 4 + 4 * k)) })
            .collect();
        let from = if x64 { crate::mem::r64(esp) } else { r32(esp) as u64 };
        let line = format!("sake {:04x} {}: {n}({}) from {:x}", crate::cur().tid, crate::k32::ticks_ms() - 60_000, args.join(","), from);
        if tr == 3 {
            RING.with(|r| {
                let mut r = r.borrow_mut();
                if r.len() >= 300 {
                    r.pop_front();
                }
                r.push_back(line);
            });
        } else {
            eprintln!("{line}");
        }
    }
    let top = crate::snap::api_enter(addr, esp);
    let mut c = Ctx { cpu, esp, edx: None, jumped: false, st0: None, trap: addr };
    let r = match f {
        Some(f) => f(&mut c),
        None => {
            let n = name_of(addr).unwrap_or_default();
            if !known {
                crate::fatal(c.cpu, &format!("call to {n}, which we neither implement nor know the arguments of"));
            }
            static SEEN: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
            if SEEN.lock().unwrap().get_or_insert_with(HashSet::default).insert(i) {
                eprintln!("sake: unimplemented {n} (returns 0), from {:#x}", r32(esp));
            }
            0
        }
    };
    if top {
        crate::snap::api_leave();
    }
    if c.jumped {
        return;
    }
    let (edx, st0) = (c.edx, c.st0);
    if tr == 2 {
        eprintln!("sake {:04x}:   -> {r:x}", crate::cur().tid);
    }
    if crate::mem::x64() {
        // RAX: a 64-bit result (ret64), a small negative one (INVALID_HANDLE_VALUE,
        // pseudo-handles) sign-extended, an address in an image above 4G widened
        let rax = match edx {
            Some(d) => (d as u64) << 32 | r as u64,
            None if r >= 0xffff_f000 => r as i32 as i64 as u64,
            None => crate::mem::widen(r),
        };
        cpu.set_gpr(EAX, rax);
        if let Some(v) = st0 {
            cpu.set_xmm(0, v.to_bits() as u128);
        }
        let ret = crate::mem::r64(esp);
        cpu.eip = ret as u32;
        cpu.rip_hi = (ret >> 32) as u32;
        cpu.set_gpr(ESP, esp as u64 + 8);
        return;
    }
    cpu.regs[EAX] = r;
    if let Some(d) = edx {
        cpu.regs[EDX] = d;
    }
    if let Some(v) = st0 {
        cpu.fpu.push(v);
    }
    cpu.eip = r32(esp);
    cpu.regs[ESP] = esp + 4 + if cdecl { 0 } else { bytes };
}

/// Writes a small x86 stub `code` into guest memory; returns its address.
pub fn stub(code: &[u8]) -> u32 {
    static AT: Mutex<(u32, u32)> = Mutex::new((0, 0));
    let mut g = AT.lock().unwrap();
    if g.1 < code.len() as u32 {
        let b = crate::mem::alloc(0x10000).expect("stub page");
        crate::mem::commit(b, 0x40);
        *g = (b, 0x10000);
    }
    let a = g.0;
    crate::mem::write(a, code);
    let n = (code.len() as u32 + 15) & !15;
    g.0 += n;
    g.1 = g.1.saturating_sub(n);
    a
}

/// A guest "function" that is a trap: an address code can call (callbacks we hand out).
pub fn host_fn(name: &str, f: Handler, bytes: u32) -> u32 {
    implement("sake", name, f);
    let a = export("sake", name);
    let i = (a - TRAP_LO) / 4;
    let mut e = ENTRIES.write().unwrap();
    e[i as usize].bytes = bytes;
    e[i as usize].known = true;
    let _ = w32;
    a
}

/// A COM vtable of traps (x64: 8-byte entries).
pub fn vtable(name: &str, methods: &[(Handler, u32)]) -> u32 {
    let k = crate::mem::ps();
    let v = crate::mem::heap_alloc(k * methods.len() as u32, true);
    for (i, (f, bytes)) in methods.iter().enumerate() {
        crate::mem::wp(v + k * i as u32, host_fn(&format!("{name}::{i}"), *f, *bytes));
    }
    v
}

#[cfg(test)]
mod dll_key_tests {
    use super::dll_key;

    /// Module names as programs import or load them, and the DLL whose exports answer (on
    /// Windows the API sets resolve to these hosts; security.dll forwards to secur32)
    #[test]
    fn names_to_keys() {
        for (name, key) in [
            ("KERNEL32.dll", "kernel32"),
            ("C:\\Windows\\System32\\USER32.DLL", "user32"),
            ("winmm", "winmm"),
            ("wineoss.drv", "wineoss"),
            ("quartz.ax", "quartz"),
            ("comctl32.ocx", "comctl32"),
            ("api-ms-win-crt-runtime-l1-1-0.dll", "ucrtbase"),
            ("Security.dll", "secur32"),
            ("security", "secur32"),
            ("secur32.dll", "secur32"),
            ("api-ms-win-core-winrt-l1-1-0.dll", "combase"),
            ("api-ms-win-core-com-l1-1-0.dll", "ole32"),
            ("api-ms-win-core-registry-l1-1-0.dll", "advapi32"),
            ("api-ms-win-core-shlwapi-legacy-l1-1-0.dll", "shlwapi"),
            ("api-ms-win-core-synch-l1-2-0.dll", "kernelbase"),
            ("api-ms-win-appmodel-runtime-l1-1-2.dll", "kernelbase"),
            ("ext-ms-win-kernel32-package-current-l1-1-0.dll", "kernelbase"),
            ("api-ms-win-ntuser-sysparams-l1-1-0.dll", "user32"),
            ("ext-ms-win-rtcore-ntuser-window-l1-1-0.dll", "user32"),
            ("ext-ms-win-gdi-dc-l1-2-0.dll", "gdi32"),
            ("api-ms-win-security-base-l1-1-0.dll", "advapi32"),
            ("api-ms-win-shell-shellcom-l1-1-0.dll", "shell32"),
            ("api-ms-win-shcore-scaling-l1-1-1.dll", "shcore"),
        ] {
            assert_eq!(dll_key(name), key, "{name}");
        }
    }
}
