//! The import boundary: every Win32 function the game imports is a trap address. A call
//! there leaves the translated code; `dispatch` runs the Rust implementation with the
//! arguments still on the guest stack and returns to the caller as stdcall/cdecl would.

use crate::{HashMap, HashSet};
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};
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
    /// x86: a thiscall function whose `this` (ECX) is argument 0, the stack arguments 1...
    /// (as on x64, where `this` is the first argument): msvcp140 / msvcp120 and their vtables
    pub this: Option<u32>,
}

impl Ctx<'_> {
    #[inline]
    pub fn a(&self, i: u32) -> u32 {
        if crate::mem::x64() {
            return self.a64(i) as u32;
        }
        match self.this {
            Some(t) if i == 0 => t,
            Some(_) => r32(self.esp + 4 * i),
            None => r32(self.esp + 4 + 4 * i),
        }
    }
    /// A pointer-sized argument (size_t, ptrdiff_t): x64 all 64 bits, x86 the slot.
    pub fn z(&self, i: u32) -> u64 {
        if crate::mem::x64() { self.a64(i) } else { self.a(i) as u64 }
    }
    /// The 64-bit integer argument `i` (x64: one argument; x86: the slots `i`, `i + 1`).
    pub fn q(&self, i: u32) -> u64 {
        if crate::mem::x64() {
            return self.a64(i);
        }
        self.a(i) as u64 | (self.a(i + 1) as u64) << 32
    }
    /// The double argument `i` (x64: XMM `i` or its stack slot; x86: the slots `i`, `i + 1`).
    pub fn d(&self, i: u32) -> f64 {
        if crate::mem::x64() {
            return f64::from_bits(self.xmm(i));
        }
        f64::from_bits(self.q(i))
    }
    /// The float argument `i` (x64: XMM `i`; x86: slot `i`).
    pub fn fl(&self, i: u32) -> f32 {
        if crate::mem::x64() {
            return f32::from_bits(self.xmm(i) as u32);
        }
        f32::from_bits(self.a(i))
    }
    /// Where an object passed by value as argument `i` is: x64 passes one bigger than 8 bytes
    /// by address (the argument), x86 copies it onto the stack (from slot `i` on).
    pub fn byval(&self, i: u32) -> u32 {
        if crate::mem::x64() {
            return self.a(i);
        }
        match self.this {
            Some(_) => self.esp + 4 * i,
            None => self.esp + 4 + 4 * i,
        }
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
    /// x86 thiscall with `this` as argument 0 (Ctx::this)
    this_arg: bool,
    /// a bare yield, not work (waits::handoff::Handoff::worked): 1 always (SwitchToThread),
    /// 2 when its argument 0 is 0 (Sleep(0), SleepEx(0, ..))
    yields: u8,
}

fn yields(name: &str) -> u8 {
    match name {
        "SwitchToThread" | "NtYieldExecution" => 1,
        "Sleep" | "SleepEx" => 2,
        _ => 0,
    }
}

static ENTRIES: RwLock<Vec<Entry>> = RwLock::new(Vec::new());

/// Entries at most: their traps stay below RET_TRAP (the traps past it are the run loop's own).
const MAX_ENTRIES: usize = ((RET_TRAP - TRAP_LO) / 4) as usize;

/// What `dispatch` reads of an entry, without a lock (review C-1: the RwLock's reader count,
/// one cache line written by every call of every guest thread). Slots in chunks of
/// HOT_CHUNK made on first use and never freed (a trap's index never changes), written
/// only under ENTRIES' write lock (`publish`), read with Acquire. `tag` and `data` are the
/// handler's own (`set_tag`: a COM method's id, a GL function), 0 when none.
struct Hot {
    f: AtomicUsize,
    meta: AtomicU32,
    tag: AtomicU32,
    data: AtomicUsize,
}

const HOT_CHUNK: usize = 4096;
static HOT: [AtomicPtr<Hot>; MAX_ENTRIES.div_ceil(HOT_CHUNK)] = [const { AtomicPtr::new(std::ptr::null_mut()) }; MAX_ENTRIES.div_ceil(HOT_CHUNK)];

/// An entry's flags as one word (Hot::meta)
#[derive(Clone, Copy, Debug, PartialEq)]
struct Meta {
    bytes: u32,
    cdecl: bool,
    known: bool,
    this_arg: bool,
    yields: u8,
}

const META_SET: u32 = 1 << 31;
const META_BYTES: u32 = (1 << 20) - 1;

impl Meta {
    fn of(e: &Entry) -> Meta {
        Meta { bytes: e.bytes, cdecl: e.cdecl, known: e.known, this_arg: e.this_arg, yields: e.yields }
    }
    /// (bytes: 20 bits, a stdcall function's arguments are far fewer; more is cut to META_BYTES)
    fn pack(self) -> u32 {
        META_SET | self.bytes.min(META_BYTES) | (self.cdecl as u32) << 20 | (self.known as u32) << 21 | (self.this_arg as u32) << 22 | (self.yields as u32 & 3) << 23
    }
    fn unpack(w: u32) -> Option<Meta> {
        (w & META_SET != 0).then_some(Meta { bytes: w & META_BYTES, cdecl: w >> 20 & 1 != 0, known: w >> 21 & 1 != 0, this_arg: w >> 22 & 1 != 0, yields: (w >> 23 & 3) as u8 })
    }
}

/// Slot `i` of the hot table (made when `make`: callers hold ENTRIES' write lock).
fn hot_slot(i: usize, make: bool) -> Option<&'static Hot> {
    let c = HOT.get(i / HOT_CHUNK)?;
    let mut p = c.load(Ordering::Acquire);
    if p.is_null() {
        if !make {
            return None;
        }
        let chunk: Box<[Hot]> = (0..HOT_CHUNK).map(|_| Hot { f: AtomicUsize::new(0), meta: AtomicU32::new(0), tag: AtomicU32::new(0), data: AtomicUsize::new(0) }).collect();
        p = Box::leak(chunk).as_mut_ptr();
        c.store(p, Ordering::Release);
    }
    Some(unsafe { &*p.add(i % HOT_CHUNK) })
}

/// Entry `i` (of `e`, ENTRIES write-locked) into the hot table.
fn publish(i: usize, e: &Entry) {
    let s = hot_slot(i, true).expect("trap index in range");
    s.f.store(e.f.map_or(0, |f| f as usize), Ordering::Relaxed);
    s.meta.store(Meta::of(e).pack(), Ordering::Release);
}

/// Entry `i`'s handler and flags, None when there is no entry `i`.
fn hot(i: usize) -> Option<(Option<Handler>, Meta)> {
    let s = hot_slot(i, false)?;
    let m = Meta::unpack(s.meta.load(Ordering::Acquire))?;
    let f = s.f.load(Ordering::Relaxed);
    // (only ever stored from a Handler)
    let f = (f != 0).then(|| unsafe { std::mem::transmute::<usize, Handler>(f) });
    Some((f, m))
}

/// Gives the trap at `addr` the handler's own `tag` (not 0) and `data`, read back without a
/// lock by [`tag_of`]: what a handler shared by many traps tells them apart by (it was a
/// locked HashMap by trap address: review C-2).
pub fn set_tag(addr: u32, tag: u32, data: usize) {
    debug_assert!(tag != 0);
    let Some(s) = addr.checked_sub(TRAP_LO).and_then(|o| hot_slot(o as usize / 4, false)) else { return };
    s.data.store(data, Ordering::Relaxed);
    s.tag.store(tag, Ordering::Release);
}

/// The trap's [`set_tag`] tag and data; tag 0 when it has none.
pub fn tag_of(addr: u32) -> (u32, usize) {
    let Some(s) = addr.checked_sub(TRAP_LO).and_then(|o| hot_slot(o as usize / 4, false)) else { return (0, 0) };
    let t = s.tag.load(Ordering::Acquire);
    (t, if t != 0 { s.data.load(Ordering::Relaxed) } else { 0 })
}
static INDEX: Mutex<Option<HashMap<(String, String), u32>>> = Mutex::new(None);
static IMPLS: RwLock<Option<HashMap<String, Handler>>> = RwLock::new(None);

struct Spec {
    /// (dll, name) -> (cdecl, bytes, thiscall)
    by_name: HashMap<(String, String), (bool, u32, bool)>,
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
            s.by_name.entry((v[0].to_string(), v[1].to_string())).or_insert((cdecl, bytes, v[2] == "thiscall"));
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
    assert!((i as usize) < MAX_ENTRIES, "sake: more than {MAX_ENTRIES} distinct imports ({dll}!{name})");
    let this_arg = spec.is_some_and(|s| s.2) && THIS_ARG0.contains(&d.as_str());
    e.push(Entry {
        dll: d,
        name: name.to_string(),
        f,
        bytes: spec.map_or(0, |s| s.1),
        cdecl: spec.is_some_and(|s| s.0),
        known: spec.is_some() || f.is_some(),
        this_arg,
        yields: yields(name),
    });
    publish(i as usize, &e[i as usize]);
    idx.insert(key, i);
    TRAP_LO + 4 * i
}

/// The DLLs whose thiscall functions take `this` as argument 0 on x86 (Ctx::this): their
/// handlers are x64's too. (msvcp90's and the others' read ECX themselves.)
const THIS_ARG0: [&str; 2] = ["msvcp140", "msvcp120"];

/// "dll!name" of a trap address (for logs).
pub fn name_of(addr: u32) -> Option<String> {
    let i = addr.checked_sub(TRAP_LO)? / 4;
    ENTRIES.read().unwrap().get(i as usize).map(|e| format!("{}!{}", e.dll, e.name))
}

/// Whether the function at a trap address is cdecl, and its stdcall argument bytes.
pub fn args_of(addr: u32) -> (bool, u32) {
    let i = (addr.wrapping_sub(TRAP_LO) / 4) as usize;
    hot(i).map_or((true, 0), |(_, m)| (m.cdecl, m.bytes))
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
    hot(i).is_some_and(|(_, m)| m.known)
}
/// The handler of a trap address.
pub fn handler_of(addr: u32) -> Option<Handler> {
    let i = (addr.wrapping_sub(TRAP_LO) / 4) as usize;
    hot(i).and_then(|(f, _)| f)
}

pub fn implemented(addr: u32) -> bool {
    let i = (addr.wrapping_sub(TRAP_LO) / 4) as usize;
    hot(i).is_some_and(|(f, _)| f.is_some())
}

pub fn trace() -> u32 {
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| std::env::var("SAKE_TRACE").ok().and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// Counting API calls by name (SAKE_API_STATS=<seconds>), and the time spent in them (µs: a
/// call that waits, WaitMessage or WaitForSingleObject, counts its wait too)
static COUNTS: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);
static TIMES: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);

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
            let m = TIMES.lock().unwrap().take().unwrap_or_default();
            let mut v: Vec<(u32, u64)> = m.into_iter().collect();
            v.sort_by(|a, b| b.1.cmp(&a.1));
            let total: u64 = v.iter().map(|x| x.1).sum();
            let top: Vec<String> = v.iter().take(40).map(|(i, us)| format!("{}={}ms", name_of(TRAP_LO + 4 * i).unwrap_or_default(), us / 1000)).collect();
            eprintln!("sake api time {}ms/{s}s: {}", total / 1000, top.join(" "));
        });
        true
    })
}

/// SAKE_THREADS=<seconds>: every so often, each guest thread's last API calls (name, first
/// arguments, caller, ms), to see what a stalled game's threads wait for without the cost of
/// formatting a trace line per call. SAKE_THREADS_STACKS=1 adds each thread's registers and stack.
// (lock-free: relaxed stores into a slot per thread, so watching barely changes the timing)
const RECENT_N: usize = 8;
const RECENT_THREADS: usize = 128;
static RECENT: [[[std::sync::atomic::AtomicU32; 6]; RECENT_N]; RECENT_THREADS] = unsafe { std::mem::zeroed() };
static RECENT_POS: [std::sync::atomic::AtomicU32; RECENT_THREADS] = unsafe { std::mem::zeroed() };

fn recent_slot(tid: u32) -> Option<usize> {
    let k = (tid.wrapping_sub(0x20) / 4) as usize;
    (k < RECENT_THREADS).then_some(k)
}

fn threads_on() -> bool {
    static T: OnceLock<bool> = OnceLock::new();
    *T.get_or_init(|| {
        let Some(s) = std::env::var("SAKE_THREADS").ok().and_then(|s| s.parse::<f64>().ok()) else { return false };
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs_f64(s));
            use std::sync::atomic::Ordering::Relaxed;
            let mut out = format!("sake threads at {} ms:", crate::k32::ticks_ms() - 60_000);
            for k in 0..RECENT_THREADS {
                let pos = RECENT_POS[k].load(Relaxed) as usize;
                if pos == 0 {
                    continue;
                }
                out += &format!("\n  {:04x}:", 0x20 + 4 * k);
                for n in pos.saturating_sub(RECENT_N)..pos {
                    let r: Vec<u32> = RECENT[k][n % RECENT_N].iter().map(|x| x.load(Relaxed)).collect();
                    out += &format!(" {}({:x},{:x},{:x})@{:x}/{}", name_of(TRAP_LO + 4 * r[0]).unwrap_or_default(), r[1], r[2], r[3], r[4], r[5]);
                }
                // (waiting for a critical section: its LockCount, RecursionCount, OwningThread, LockSemaphore)
                let last: Vec<u32> = RECENT[k][(pos - 1) % RECENT_N].iter().map(|x| x.load(Relaxed)).collect();
                if name_of(TRAP_LO + 4 * last[0]).is_some_and(|n| n.contains("EnterCriticalSection")) && last[1] >= 0x10000 {
                    out += &format!(" [cs {:x}: {:x} {:x} owner {:x} {:x}]", last[1], r32(last[1] + 4), r32(last[1] + 8), r32(last[1] + 12), r32(last[1] + 16));
                }
            }
            eprintln!("{out}");
            // SAKE_THREADS_STACKS=1: also each thread's registers and the code on its stack
            if std::env::var_os("SAKE_THREADS_STACKS").is_some() {
                eprintln!("{}", kura::crashlog::snapshot_text());
            }
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
    crate::comrpc::pump(cpu, false);
    let i = (addr - TRAP_LO) / 4;
    let Some((f, Meta { bytes, cdecl, known, this_arg, yields })) = hot(i as usize) else {
        crate::fatal(cpu, &format!("jump to unknown trap address {addr:#x}"));
    };
    let esp = cpu.regs[ESP];
    let sw = switches();
    if sw & SW_DEBUG != 0 {
        debug_enter(cpu, addr, i, esp, bytes, sw);
    }
    crate::k32::check_terminate();
    let top = crate::snap::api_enter(addr, esp);
    let me = crate::cur();
    crate::obj::HANDOFF.entered(me.tid);
    let outer = me.api;
    let tid = me.tid;
    me.api = (esp, if crate::mem::x64() { 8 } else { 4 + if cdecl { 0 } else { bytes } });
    let this = (this_arg && !crate::mem::x64()).then(|| cpu.regs[koji::ECX]);
    let mut c = Ctx { cpu, esp, edx: None, jumped: false, st0: None, trap: addr, this };
    if yields == 0 || (yields == 2 && c.a(0) != 0) {
        crate::obj::HANDOFF.worked(tid);
    }
    let t0 = (sw & SW_STATS != 0).then(std::time::Instant::now);
    // (SAKE_SHIRUSU=1: the call for a shirusu Trace, crates/sake/src/shirusu.rs)
    let shirusu = if sw & SW_SHIRUSU != 0 {
        let x64 = crate::mem::x64();
        let n = if x64 { 12 } else { (bytes / 4).min(12) };
        let args: Vec<u64> = (0..n).map(|k| if x64 { c.a64(k) } else { c.a(k) as u64 }).collect();
        crate::shirusu::enter(i, &args)
    } else {
        None
    };
    let r = match f {
        Some(f) => f(&mut c),
        None => unimplemented_call(&mut c, i, known),
    };
    if let Some(t0) = t0 {
        *TIMES.lock().unwrap().get_or_insert_with(HashMap::default).entry(i).or_default() += t0.elapsed().as_micros() as u64;
    }
    if top {
        crate::snap::api_leave();
    }
    crate::cur().api = outer;
    if let Some(o) = shirusu.filter(|_| !c.jumped) {
        let ret = match c.edx {
            Some(d) => (d as u64) << 32 | r as u64,
            None if crate::mem::x64() && r >= 0xffff_f000 => r as i32 as i64 as u64,
            None if crate::mem::x64() => crate::mem::widen(r),
            None => r as u64,
        };
        crate::shirusu::leave(o, ret);
    }
    if c.jumped {
        crate::tctx::apply_pending(c.cpu);
        return;
    }
    let (edx, st0) = (c.edx, c.st0);
    if sw & SW_TRACE2 != 0 {
        eprintln!("sake {:04x}:   -> {r:x}", crate::cur().tid);
    }
    finish(cpu, esp, r, edx, st0, cdecl, bytes);
}

/// The debug switches, read once (they are environment variables): one word for `dispatch`
/// instead of a OnceLock each (review C-1).
const SW_SET: u32 = 1 << 31;
const SW_USAGE: u32 = 1;
const SW_STATS: u32 = 2;
const SW_THREADS: u32 = 4;
const SW_WATCH: u32 = 8;
const SW_TRACE: u32 = 16;
const SW_TRACE2: u32 = 32;
const SW_SHIRUSU: u32 = 64;
/// (what `debug_enter` looks at)
const SW_DEBUG: u32 = SW_USAGE | SW_STATS | SW_THREADS | SW_WATCH | SW_TRACE;

fn switches() -> u32 {
    static SW: AtomicU32 = AtomicU32::new(0);
    let s = SW.load(Ordering::Relaxed);
    if s != 0 {
        return s;
    }
    let tr = trace();
    let s = SW_SET
        | if kura::usage::on() { SW_USAGE } else { 0 }
        | if stats_on() { SW_STATS } else { 0 }
        | if threads_on() { SW_THREADS } else { 0 }
        | if watch_addr() != 0 { SW_WATCH } else { 0 }
        | if tr > 0 { SW_TRACE } else { 0 }
        | if tr == 2 { SW_TRACE2 } else { 0 }
        | if crate::shirusu::on() { SW_SHIRUSU } else { 0 };
    SW.store(s, Ordering::Relaxed);
    s
}

/// SAKE_WATCH=<hex address>: its value printed whenever it changed before an API call
fn watch_addr() -> u32 {
    static W: OnceLock<u32> = OnceLock::new();
    *W.get_or_init(|| std::env::var("SAKE_WATCH").ok().and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0))
}

/// What the debug switches `sw` do before a call (out of `dispatch`'s way when none is on)
#[cold]
#[inline(never)]
fn debug_enter(cpu: &mut Cpu, addr: u32, i: u32, esp: u32, bytes: u32, sw: u32) {
    if sw & SW_USAGE != 0 {
        usage(i);
    }
    if sw & SW_STATS != 0 {
        *COUNTS.lock().unwrap().get_or_insert_with(HashMap::default).entry(i).or_default() += 1;
    }
    if sw & SW_THREADS != 0 {
        if let Some(k) = recent_slot(crate::cur().tid) {
            use std::sync::atomic::Ordering::Relaxed;
            let n = RECENT_POS[k].load(Relaxed) as usize;
            let r = [i, r32(esp + 4), r32(esp + 8), r32(esp + 12), r32(esp), crate::k32::ticks_ms() - 60_000];
            for (x, v) in RECENT[k][n % RECENT_N].iter().zip(r) {
                x.store(v, Relaxed);
            }
            RECENT_POS[k].store(n as u32 + 1, Relaxed);
        }
    }
    if sw & SW_WATCH != 0 {
        static LAST: AtomicU32 = AtomicU32::new(0);
        let w = watch_addr();
        let v = r32(w);
        if LAST.swap(v, Ordering::Relaxed) != v {
            eprintln!("sake watch {w:#x} = {v:#x} before {}, from {:x}", name_of(addr).unwrap_or_default(), r32(esp));
        }
    }
    let tr = trace();
    if sw & SW_TRACE != 0 && trace_match(i) {
        let n = name_of(addr).unwrap_or_default();
        let x64 = crate::mem::x64();
        let nargs = if x64 { 4.max(bytes / 4).min(8) } else { (bytes / 4).min(8) };
        let args: Vec<String> = (0..nargs)
            .map(|k| if x64 { format!("{:x}", Ctx { cpu: &mut *cpu, esp, edx: None, jumped: false, st0: None, trap: addr, this: None }.a64(k)) } else { format!("{:x}", r32(esp + 4 + 4 * k)) })
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
}

/// A call to a function sake has no implementation of: its spec's result, said once.
#[cold]
#[inline(never)]
fn unimplemented_call(c: &mut Ctx, i: u32, known: bool) -> u32 {
    let n = name_of(c.trap).unwrap_or_default();
    if !known {
        crate::fatal(c.cpu, &format!("call to {n}, which we neither implement nor know the arguments of"));
    }
    static SEEN: Mutex<Option<HashSet<u32>>> = Mutex::new(None);
    let r = unimplemented_result(&n);
    if SEEN.lock().unwrap().get_or_insert_with(HashSet::default).insert(i) {
        eprintln!("sake: unimplemented {n} (returns {r:#x}), from {:#x}", r32(c.esp));
    }
    r
}

/// Returns from an API call to its caller with result `r` (+ EDX, ST0), popping what stdcall pops.
fn finish(cpu: &mut Cpu, esp: u32, r: u32, edx: Option<u32>, st0: Option<f64>, cdecl: bool, bytes: u32) {
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
        crate::tctx::apply_pending_after_call(cpu);
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
    crate::tctx::apply_pending_after_call(cpu);
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
    publish(i as usize, &e[i as usize]);
    let _ = w32;
    a
}

/// A host function called as a thiscall virtual function (x86: `this` in ECX is argument 0,
/// the callee pops `bytes` of stack arguments; x64: the plain convention).
pub fn host_fn_this(name: &str, f: Handler, bytes: u32) -> u32 {
    let a = host_fn(name, f, bytes);
    let i = ((a - TRAP_LO) / 4) as usize;
    let mut e = ENTRIES.write().unwrap();
    e[i].this_arg = true;
    publish(i, &e[i]);
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

#[cfg(test)]
mod spec_tests {
    use super::{forwards, specs};

    /// Every function sake implements under a DLL whose exports are all stdcall has its spec
    /// line (the bytes the callee pops): without one it binds as cdecl popping nothing, and a
    /// 32-bit caller's stack is off by the arguments after the call (Inno Setup's ISCC crashed
    /// so in BCryptVerifySignature). The LongPtr functions are x64's only (macros on x86); ippai_*
    /// are sake's own, called without arguments.
    #[test]
    fn stdcall_implementations_have_specs() {
        let sp = specs();
        let mut all_stdcall: std::collections::HashMap<&str, bool> = std::collections::HashMap::new();
        for l in include_str!("specs.txt").lines() {
            let v: Vec<&str> = l.split(' ').collect();
            if v.len() >= 5 {
                *all_stdcall.entry(v[0]).or_insert(true) &= v[2] == "stdcall";
            }
        }
        let known = |d: &str, n: &str| {
            sp.by_name.contains_key(&(d.to_string(), n.to_string()))
                || forwards(d).iter().chain(&["kernel32", "kernelbase", "ntdll"]).any(|k| sp.by_name.contains_key(&(k.to_string(), n.to_string())))
        };
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
        let mut missing = Vec::new();
        for f in std::fs::read_dir(dir).unwrap() {
            let p = f.unwrap().path();
            if p.extension().is_none_or(|e| e != "rs") {
                continue;
            }
            let s = std::fs::read_to_string(&p).unwrap();
            let mut found: Vec<(String, String)> = Vec::new();
            // implement("dll", "Name", f)
            for part in s.split("implement(\"").skip(1) {
                let mut q = part.split('"');
                let (Some(d), Some(sep), Some(n)) = (q.next(), q.next(), q.next()) else { continue };
                if sep.trim() == "," {
                    found.push((d.to_string(), n.to_string()));
                }
            }
            // reg!("dll"; A, B, ...)
            for part in s.split("reg!(\"").skip(1) {
                let Some((d, rest)) = part.split_once('"') else { continue };
                let Some(body) = rest.trim_start().strip_prefix(';') else { continue };
                let body = body.split(')').next().unwrap_or("");
                for n in body.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).filter(|n| !n.is_empty()) {
                    found.push((d.to_string(), n.to_string()));
                }
            }
            for (d, n) in found {
                if all_stdcall.get(d.as_str()) == Some(&true) && !n.contains("LongPtr") && !n.starts_with("ippai_") && !known(&d, &n) {
                    missing.push(format!("{d}!{n} ({})", p.file_name().unwrap().to_string_lossy()));
                }
            }
        }
        assert!(missing.is_empty(), "implemented without a spec line: {missing:?}");
    }
}

/// What an export sake knows but doesn't implement returns: 0, except D3DX's functions that
/// make an object, for which 0 is S_OK with the object pointer never written (ダイナマリサ3D
/// called through the NULL D3DXCreateCubeTextureFromFileInMemory left, #126): E_NOTIMPL, a
/// failure the game checks for.
pub fn unimplemented_result(name: &str) -> u32 {
    let (dll, f) = name.split_once('!').unwrap_or(("", name));
    let d3dx = dll.to_ascii_lowercase().starts_with("d3dx");
    if d3dx && ["D3DXCreate", "D3DXLoad", "D3DXCompile", "D3DXAssemble", "D3DXPreprocess", "D3DXGetShader", "D3DXSave", "D3DXFilter", "D3DXComputeNormalMap"].iter().any(|p| f.starts_with(p)) {
        0x8000_4001
    } else {
        0
    }
}

#[cfg(test)]
mod unimplemented_tests {
    use super::unimplemented_result;
    #[test]
    fn d3dx_object_makers_fail() {
        assert_eq!(unimplemented_result("d3dx9_33!D3DXCreateCubeTextureFromFileInMemory"), 0x8000_4001);
        assert_eq!(unimplemented_result("D3DX9_43!D3DXLoadMeshFromXInMemory"), 0x8000_4001);
        assert_eq!(unimplemented_result("d3dx8!D3DXCreateSprite"), 0x8000_4001);
        // (matrices, vectors, others: 0 as before)
        assert_eq!(unimplemented_result("d3dx9_33!D3DXMatrixReflect"), 0);
        assert_eq!(unimplemented_result("kernel32!CreateFileTransactedA"), 0);
        assert_eq!(unimplemented_result("comdlg32!GetFileTitleA"), 0);
    }
}

#[cfg(test)]
mod ordinal_tests {
    use super::{dll_key, ordinal_name, specs};
    use proptest::prelude::*;

    /// d3d12.dll's two numbered exports as Windows numbers them: UnityPlayer.dll 6000.3/6000.4
    /// delay-loads both by ordinal only, and a missing one is the helper's c06d007f, the
    /// player's crash at "Discovering subsystems" (dl_RJ01610703, dl_RJ01684055, dl_RJ01690707)
    #[test]
    fn d3d12_ordinals() {
        assert_eq!(ordinal_name("d3d12.dll", 101).as_deref(), Some("D3D12CreateDevice"));
        assert_eq!(ordinal_name("D3D12", 102).as_deref(), Some("D3D12GetDebugInterface"));
        assert_eq!(ordinal_name("d3d12", 103), None);
    }

    /// One ordinal names one export per DLL (a duplicate in specs.txt would make
    /// GetProcAddress by number depend on line order). One name may have several (MFC's do).
    #[test]
    fn ordinals_are_unique() {
        let mut by_ord = std::collections::HashMap::new();
        for l in include_str!("specs.txt").lines() {
            let v: Vec<&str> = l.split(' ').collect();
            let Some(o) = v.get(4).and_then(|o| o.parse::<u32>().ok()) else { continue };
            if let Some(prev) = by_ord.insert((v[0], o), v[1]) {
                assert_eq!(prev, v[1], "{}: ordinal {o} twice", v[0]);
            }
        }
    }

    fn numbered() -> Vec<(String, u32, String)> {
        let mut v: Vec<_> = specs().by_ord.iter().map(|((d, o), n)| (d.clone(), *o, n.clone())).collect();
        v.sort();
        v
    }

    proptest! {
        /// Any numbered export, asked for by number under any spelling of its DLL's name (case,
        /// .dll, a path), is the export of that name that the spec has
        #[test]
        fn ordinal_round_trip(i in any::<prop::sample::Index>(), upper in any::<bool>(), suffix in any::<bool>(), path in any::<bool>()) {
            let all = numbered();
            let (d, o, n) = &all[i.index(all.len())];
            let mut name = if upper { d.to_uppercase() } else { d.clone() };
            if suffix { name.push_str(".dll"); }
            if path { name = format!("C:\\Windows\\System32\\{name}"); }
            prop_assume!(dll_key(&name) == *d);
            let got = ordinal_name(&name, *o);
            prop_assert_eq!(got.as_deref(), Some(n.as_str()));
            prop_assert!(specs().by_name.contains_key(&(d.clone(), n.clone())));
        }
    }

    /// The imports by ordinal of real game DLLs (the import and the delay-load directories),
    /// from system DLLs sake has a spec for, all have a name: SAKE_IMPORT_DLLS=a.dll:b.dll
    /// (UnityPlayer.dll of 6000.0..6000.5 builds)
    #[test]
    #[ignore]
    fn real_imports_by_ordinal() {
        let Ok(list) = std::env::var("SAKE_IMPORT_DLLS") else { return };
        let mut missing = Vec::new();
        for path in list.split(':') {
            let f = std::fs::read(path).unwrap();
            let u32at = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap()) as usize;
            let u64at = |o: usize| u64::from_le_bytes(f[o..o + 8].try_into().unwrap());
            let pe = u32at(0x3c);
            let wide = u16::from_le_bytes([f[pe + 24], f[pe + 25]]) == 0x20b;
            let (n, opt) = (u16::from_le_bytes([f[pe + 6], f[pe + 7]]) as usize, u16::from_le_bytes([f[pe + 20], f[pe + 21]]) as usize);
            let dirs = pe + 24 + if wide { 112 } else { 96 };
            let off = |rva: usize| {
                (0..n).map(|k| pe + 24 + opt + 40 * k).find_map(|s| {
                    let (va, size, ptr) = (u32at(s + 12), u32at(s + 16).max(u32at(s + 8)), u32at(s + 20));
                    (rva >= va && rva < va + size).then(|| rva - va + ptr)
                })
            };
            let cstr = |o: usize| String::from_utf8_lossy(&f[o..o + f[o..].iter().position(|&b| b == 0).unwrap()]).into_owned();
            // (import descriptor: 20 bytes, name at 12, thunks at 0 or 16; delay: 32 bytes,
            // name at 4, names table at 16)
            for (dir, size, name_at, thunks_at) in [(1usize, 20usize, 12usize, 0usize), (13, 32, 4, 16)] {
                let rva = u32at(dirs + 8 * dir);
                let Some(mut d) = (rva != 0).then(|| off(rva)).flatten() else { continue };
                loop {
                    let (name, mut t) = (u32at(d + name_at), u32at(d + thunks_at));
                    if name == 0 {
                        break;
                    }
                    if t == 0 {
                        t = u32at(d + 16);
                    }
                    let dll = cstr(off(name).unwrap());
                    let mut p = off(t).unwrap();
                    loop {
                        let (v, flag) = if wide { (u64at(p), 1u64 << 63) } else { (u32at(p) as u64, 1 << 31) };
                        if v == 0 {
                            break;
                        }
                        if v & flag != 0 && specs().dlls.contains(&dll_key(&dll)) && ordinal_name(&dll, (v & 0xffff) as u32).is_none() {
                            missing.push(format!("{path}: {dll} #{}", v & 0xffff));
                        }
                        p += if wide { 8 } else { 4 };
                    }
                    d += size;
                }
            }
        }
        assert!(missing.is_empty(), "imports by ordinal without a name: {missing:?}");
    }
}

/// The lock-free table `dispatch` reads (review C-1) says what ENTRIES says, whatever is
/// exported meanwhile on other threads.
#[cfg(test)]
mod hot_tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn meta_round_trips(bytes in 0u32..=META_BYTES, cdecl in any::<bool>(), known in any::<bool>(), this_arg in any::<bool>(), yields in 0u8..3) {
            let m = Meta { bytes, cdecl, known, this_arg, yields };
            prop_assert_eq!(Meta::unpack(m.pack()), Some(m));
        }
    }

    #[test]
    fn unset_slot_is_none() {
        assert_eq!(Meta::unpack(0), None);
        assert!(hot(MAX_ENTRIES).is_none());
        assert!(hot(MAX_ENTRIES - 1).is_none());
        assert_eq!(tag_of(TRAP_LO - 4), (0, 0));
        assert_eq!(tag_of(TRAP_LO + 4 * MAX_ENTRIES as u32), (0, 0));
    }

    fn h1(_: &mut Ctx) -> u32 {
        1
    }
    fn h2(_: &mut Ctx) -> u32 {
        2
    }

    /// Every entry's hot slot against the entry itself
    fn check_all() {
        let e = ENTRIES.read().unwrap();
        for (i, x) in e.iter().enumerate() {
            let (f, m) = hot(i).expect("published");
            assert_eq!(m, Meta::of(x), "{}!{}", x.dll, x.name);
            assert_eq!(f.map(|f| f as usize), x.f.map(|f| f as usize), "{}!{}", x.dll, x.name);
        }
        // (nothing past them: an entry is published under the write lock)
        assert!(hot(e.len()).is_none());
    }

    #[test]
    fn host_fns_and_tags() {
        let a = host_fn("hot_tests_a", h1, 12);
        let b = host_fn_this("hot_tests_b", h2, 8);
        assert_eq!(args_of(a), (false, 12));
        assert!(is_known(a) && implemented(a));
        assert_eq!(handler_of(b).map(|f| f as usize), Some(h2 as usize));
        let (_, m) = hot(((b - TRAP_LO) / 4) as usize).unwrap();
        assert!(m.this_arg);
        // (made again: the same trap, its bytes updated)
        assert_eq!(host_fn("hot_tests_a", h1, 16), a);
        assert_eq!(args_of(a), (false, 16));
        assert_eq!(tag_of(a), (0, 0));
        set_tag(a, 0x1234, 77);
        assert_eq!(tag_of(a), (0x1234, 77));
        assert_eq!(tag_of(b), (0, 0));
        // a function of a DLL's spec sake does not implement: known, no handler
        let s = export("kernel32", "hot_tests_no_such_function");
        assert!(!implemented(s) && !is_known(s));
        check_all();
    }

    /// Threads export new names while others read every published slot: a slot is either not
    /// there yet or whole (its handler and flags as the entry has them).
    #[test]
    fn concurrent_exports() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let readers: Vec<_> = (0..3)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    let mut seen = 0usize;
                    while !stop.load(Ordering::Relaxed) {
                        // (a slot is published whole: its flags say it is set, its handler one
                        // of the program's or none)
                        for i in 0..MAX_ENTRIES {
                            let Some((_, m)) = hot(i) else { break };
                            assert!(m.bytes <= META_BYTES && m.yields < 3);
                            seen = seen.max(i);
                        }
                    }
                    seen
                })
            })
            .collect();
        let writers: Vec<_> = (0..2)
            .map(|w| {
                std::thread::spawn(move || {
                    for k in 0..3000 {
                        let a = host_fn(&format!("hot_tests_c{w}_{k}"), if k % 2 == 0 { h1 } else { h2 }, 4 * (k % 9));
                        let (f, m) = hot(((a - TRAP_LO) / 4) as usize).unwrap();
                        assert_eq!(m.bytes, 4 * (k % 9));
                        assert_eq!(f.map(|f| f as usize), Some(if k % 2 == 0 { h1 as usize } else { h2 as usize }));
                        set_tag(a, k + 1, k as usize);
                        assert_eq!(tag_of(a), (k + 1, k as usize));
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        stop.store(true, Ordering::Relaxed);
        for r in readers {
            r.join().unwrap();
        }
        check_all();
    }
}
