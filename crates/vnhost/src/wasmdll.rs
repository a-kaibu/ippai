//! Wine PE DLLs compiled to wasm (see docs/wasm-dlls.md, crates/vnwasm).
//!
//! When Wine maps an x86 PE DLL that has a wasm build (`<wine>/lib/wine/i386-wasm/<name>.wasm`),
//! the image stays as Wine's loader expects it, but its export table and entry point are pointed
//! at the module's functions: x86 addresses `BIAS + slot` in the x86 core's trap range. x86 code
//! calling one stops the core and [`vnhost_wasm_enter`] calls the function with the arguments
//! from the x86 stack. The module calls x86 code (its imports, through the image's IAT, and any
//! function pointer outside the wasm DLLs) with [`vnhost_wasm_x86_call`], which runs the x86 core
//! nested (signal_wasm.c's `wasm_call_x86`).
//!
//! Each thread (worker) has its own instance of every module of its process and its own
//! funcref table, where the process's modules take the same slots everywhere: slots are given
//! out per process, threads instantiate what they lack when they enter wasm code.
//!
//! The module's static data lives in guest memory (allocated by signal_wasm.c), and its stack
//! is the x86 stack: on entry the stack pointer global is set below the x86 ESP.

use crate::cpu::Linear;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use vnx86::{Cpu, Memory};

/// x86 address of slot 0 (crates/vnwasm, signal_wasm.c's WASM_DLL_BASE)
pub const BIAS: u32 = 0xfe00_0000;
/// slots per process (the size of [`Process::tags`])
const SLOTS: u32 = 0x1_0000;

#[cfg(target_os = "emscripten")]
extern "C" {
    fn vnvm_wasm_thread_init(teb: u32);
    fn vnvm_wasm_instantiate(id: u32, bytes: *const u8, len: usize, mem: u32, slot: u32, image: u32, size: u32, entries: *const u32, n: u32, tags: u32) -> i32;
    fn vnvm_wasm_got(id: u32, name: *const u8, len: usize, value: u32);
    fn vnvm_wasm_fixup(id: u32);
}

#[cfg(not(target_os = "emscripten"))]
mod nojs {
    pub unsafe fn vnvm_wasm_thread_init(_: u32) {}
    pub unsafe fn vnvm_wasm_instantiate(_: u32, _: *const u8, _: usize, _: u32, _: u32, _: u32, _: u32, _: *const u32, _: u32, _: u32) -> i32 {
        -1
    }
    pub unsafe fn vnvm_wasm_got(_: u32, _: *const u8, _: usize, _: u32) {}
    pub unsafe fn vnvm_wasm_fixup(_: u32) {}
}
#[cfg(not(target_os = "emscripten"))]
use nojs::*;

/// A module file (shared by the processes that load it).
struct Module {
    bytes: Vec<u8>,
    size_of_image: u32,
    timestamp: u32,
    mem_size: u32,
    mem_align: u32,
    table_size: u32,
    entry_point: u32,
    /// per slot: entry kind, bytes popped
    slots: Vec<(u16, u16)>,
    /// per entry kind: returns f64
    float: Vec<bool>,
    /// (export address table index, data?, slot or data offset)
    exports: Vec<(u32, bool, u32)>,
    /// (name, RVA, image-relative address rather than the IAT slot there)
    got: Vec<(String, u32, bool)>,
    /// x86 functions that jump to the module's: (RVA, slot)
    patches: Vec<(u32, u32)>,
    /// per slot: its function type's tag (crates/vnwasm type_tag)
    tags: Vec<u32>,
}

struct Cursor<'a> {
    b: &'a [u8],
    p: usize,
}

impl Cursor<'_> {
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_le_bytes(self.b.get(self.p..self.p + 4)?.try_into().ok()?);
        self.p += 4;
        Some(v)
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u32()? as usize;
        let s = String::from_utf8_lossy(self.b.get(self.p..self.p + n)?).into_owned();
        self.p += n;
        Some(s)
    }
}

fn leb(b: &[u8], at: &mut usize) -> Option<u32> {
    let (mut v, mut s) = (0u32, 0);
    loop {
        let c = *b.get(*at)?;
        *at += 1;
        v |= ((c & 0x7f) as u32) << s;
        if c & 0x80 == 0 {
            return Some(v);
        }
        s += 7;
    }
}

/// The module and its "vnwasm" custom section (written by crates/vnwasm).
fn parse(bytes: Vec<u8>) -> Option<Module> {
    let mut at = 8;
    let mut meta = None;
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let size = leb(&bytes, &mut at)? as usize;
        if id == 0 {
            let mut p = at;
            let n = leb(&bytes, &mut p)? as usize;
            if bytes.get(p..p + n)? == b"vnwasm" {
                meta = Some(p + n);
            }
        }
        at += size;
    }
    let mut c = Cursor { b: &bytes, p: meta? };
    if c.u32()? != u32::from_le_bytes(*b"VNW2") {
        return None;
    }
    let size_of_image = c.u32()?;
    let timestamp = c.u32()?;
    c.str()?;
    let (mem_size, mem_align, table_size, entry_point) = (c.u32()?, c.u32()?, c.u32()?, c.u32()?);
    let mut slots = Vec::new();
    for _ in 0..c.u32()? {
        let v = c.u32()?;
        slots.push((v as u16, (v >> 16) as u16));
    }
    let mut float = Vec::new();
    for _ in 0..c.u32()? {
        float.push(c.u32()? & 1 != 0);
    }
    let mut exports = Vec::new();
    for _ in 0..c.u32()? {
        exports.push((c.u32()?, c.u32()? != 0, c.u32()?));
    }
    let mut got = Vec::new();
    for _ in 0..c.u32()? {
        let kind = c.u32()?;
        let name = c.str()?;
        got.push((name, c.u32()?, kind == 2));
    }
    let mut patches = Vec::new();
    for _ in 0..c.u32()? {
        patches.push((c.u32()?, c.u32()?));
    }
    let mut tags = Vec::new();
    for _ in 0..c.u32()? {
        tags.push(c.u32()?);
    }
    if tags.len() != table_size as usize {
        return None;
    }
    Some(Module { bytes, size_of_image, timestamp, mem_size, mem_align, table_size, entry_point, slots, float, exports, got, patches, tags })
}

/// A module loaded into a process.
struct Loaded {
    /// index in [`Process::dlls`] (and the JS side's instance list)
    module: Arc<Module>,
    image: u32,
    mem: u32,
    slot: u32,
    /// data relocations and constructors done (at the first entry)
    fixed: bool,
}

#[derive(Default)]
struct Process {
    dlls: Vec<Loaded>,
    /// slots given out
    slots: u32,
    /// the type tag of every slot given out, read by the modules' indirect calls (vn.tags)
    tags: Option<Box<[u32]>>,
    /// bumped whenever a thread has something to catch up on (read without the lock)
    generation: Arc<AtomicU32>,
}

static MODULES: Mutex<Vec<(String, Option<Arc<Module>>)>> = Mutex::new(Vec::new());
static PROCS: Mutex<Option<HashMap<i32, Process>>> = Mutex::new(None);
/// probed, not yet attached: id -> module
static PENDING: Mutex<Vec<Arc<Module>>> = Mutex::new(Vec::new());

struct Thread {
    cpu: *mut Cpu,
    teb: u32,
    call_x86: Option<extern "C" fn(u32, u32) -> u64>,
    syscall: Option<extern "C" fn(u32, u32) -> u32>,
    /// JS state set up
    init: bool,
    /// modules instantiated on this thread (a prefix of the process's list)
    have: usize,
    /// whose GOT entries are set
    got_set: Vec<bool>,
    generation: u32,
    /// the process's generation counter
    proc_generation: Option<Arc<AtomicU32>>,
    /// the modules instantiated here: (first slot, slots, module, its entries' indices in this
    /// worker's function table)
    mods: Vec<(u32, u32, Arc<Module>, Vec<u32>)>,
    /// entry table slots used (crate::wasmdll_slots)
    used: usize,
}

thread_local! {
    static THREAD: RefCell<Thread> = const { RefCell::new(Thread {
        cpu: std::ptr::null_mut(), teb: 0, call_x86: None, syscall: None, init: false, have: 0, got_set: Vec::new(), generation: u32::MAX, proc_generation: None, mods: Vec::new(), used: 0,
    }) };
}

fn enabled(name: &str) -> bool {
    match std::env::var("VNVM_WASM_DLLS") {
        Ok(v) if v == "0" => false,
        Ok(v) if !v.is_empty() && v != "1" => v.split(',').any(|d| {
            let d = d.trim().to_ascii_lowercase();
            d == name || format!("{d}.dll") == name
        }),
        _ => true,
    }
}

fn dir() -> String {
    std::env::var("VNVM_WASM_DIR").unwrap_or_else(|_| "/opt/wine-vnbox/lib/wine/i386-wasm".into())
}

fn module(name: &str) -> Option<Arc<Module>> {
    let mut mods = MODULES.lock().unwrap();
    if let Some((_, m)) = mods.iter().find(|(n, _)| n == name) {
        return m.clone();
    }
    let path = format!("{}/{name}.wasm", dir());
    let m = std::fs::read(&path).ok().and_then(parse).map(Arc::new);
    if m.is_some() && std::env::var_os("VNVM_TRACE_WASM").is_some() {
        eprintln!("vnwasm: {path}");
    }
    mods.push((name.to_string(), m.clone()));
    m
}

fn rd32(mem: &mut Linear, a: u32) -> u32 {
    mem.read32(a).unwrap_or(0)
}

/// An image was mapped at `base`: the id of its wasm build, or -1.
#[no_mangle]
pub extern "C" fn vnhost_wasmdll_probe(base: u32, size: u32) -> i32 {
    let mut mem = Linear;
    if mem.read16(base).ok() != Some(0x5a4d) {
        return -1;
    }
    let nt = base + rd32(&mut mem, base + 0x3c);
    if rd32(&mut mem, nt) != 0x4550 {
        return -1;
    }
    let opt = nt + 24;
    let size_of_image = rd32(&mut mem, opt + 56);
    let erva = rd32(&mut mem, opt + 96);
    if erva == 0 || erva >= size || size_of_image > size.max(size_of_image) {
        return -1;
    }
    let mut name = Vec::new();
    let mut a = base + rd32(&mut mem, base + erva + 12);
    while let Ok(c) = mem.read8(a) {
        if c == 0 || name.len() > 64 {
            break;
        }
        name.push(c.to_ascii_lowercase());
        a += 1;
    }
    let name = String::from_utf8_lossy(&name).into_owned();
    if !enabled(&name) {
        return -1;
    }
    let Some(m) = module(&name) else { return -1 };
    if m.size_of_image != size_of_image || m.timestamp != rd32(&mut mem, nt + 8) {
        eprintln!("vnwasm: {name}: the wasm build is for another build of the DLL");
        return -1;
    }
    let mut p = PENDING.lock().unwrap();
    p.push(m);
    p.len() as i32 - 1
}

#[no_mangle]
pub extern "C" fn vnhost_wasmdll_mem_size(id: i32) -> u32 {
    let p = PENDING.lock().unwrap();
    let m = &p[id as usize];
    m.mem_size.max(1) + (1 << m.mem_align)
}

/// Points the image's exports and entry point at the module (instantiated on first entry).
#[no_mangle]
pub extern "C" fn vnhost_wasmdll_attach(id: i32, base: u32, mem_at: u32) {
    let m = PENDING.lock().unwrap()[id as usize].clone();
    let mut mem = Linear;
    let pid = crate::proc::current();
    let mut procs = PROCS.lock().unwrap();
    let proc = procs.get_or_insert_with(HashMap::new).entry(pid).or_default();
    if proc.slots + m.table_size > SLOTS {
        eprintln!("vnwasm: out of slots");
        return;
    }
    let slot = proc.slots;
    proc.slots += m.table_size;
    let tags = proc.tags.get_or_insert_with(|| vec![0; SLOTS as usize].into_boxed_slice());
    tags[slot as usize..(slot + m.table_size) as usize].copy_from_slice(&m.tags);
    let align = (1u32 << m.mem_align) - 1;
    let data = (mem_at + align) & !align;
    // exports and entry point
    let nt = base + rd32(&mut mem, base + 0x3c);
    let opt = nt + 24;
    let erva = rd32(&mut mem, opt + 96);
    let funcs = base + rd32(&mut mem, base + erva + 28);
    for &(index, is_data, v) in &m.exports {
        let addr = if is_data { data + v } else { BIAS + slot + v };
        let _ = mem.write32(funcs + index * 4, addr.wrapping_sub(base));
    }
    let _ = mem.write32(opt + 16, (BIAS + slot + m.entry_point).wrapping_sub(base));
    // x86 functions: jmp to the wasm one
    for &(rva, s) in &m.patches {
        let at = base + rva;
        let _ = mem.write8(at, 0xe9);
        let _ = mem.write32(at + 1, (BIAS + slot + s).wrapping_sub(at + 5));
    }
    proc.dlls.push(Loaded { module: m, image: base, mem: data, slot, fixed: false });
    proc.generation.fetch_add(1, Ordering::Release);
    if std::env::var_os("VNVM_TRACE_WASM").is_some() {
        eprintln!("vnwasm: pid {pid}: image {base:#x} data {data:#x} slots {slot}+{}", proc.dlls.last().unwrap().module.table_size);
    }
}

/// signal_wasm.c, when a thread starts.
#[no_mangle]
pub extern "C" fn vnhost_wasm_thread(cpu: *mut Cpu, teb: u32, call_x86: extern "C" fn(u32, u32) -> u64, syscall: extern "C" fn(u32, u32) -> u32) {
    THREAD.with(|t| {
        let mut t = t.borrow_mut();
        t.cpu = cpu;
        t.teb = teb;
        t.call_x86 = Some(call_x86);
        t.syscall = Some(syscall);
    });
}

static SYSCALLS: AtomicU64 = AtomicU64::new(0);

/// Wasm DLL code makes syscall `id` with its arguments at `argp` (imported as `vn.syscall`).
#[no_mangle]
pub extern "C" fn vnhost_wasm_syscall(id: u32, argp: u32) -> u32 {
    count_at(&SYSCALLS, id);
    let f = THREAD.with(|t| t.borrow().syscall).expect("syscall on a thread without a CPU");
    f(id, argp)
}

/// Instantiates what this thread lacks and sets GOT entries; the module whose first entry this
/// is (it contains `need`) gets its data relocations and constructors run, after the locks
/// are released (they may call x86 code, which may call wasm code).
fn sync(need: Option<u32>) {
    let fixup = THREAD.with(|t| {
        let mut t = t.borrow_mut();
        if t.proc_generation.as_ref().is_some_and(|g| g.load(Ordering::Acquire) == t.generation) {
            return None;
        }
        let pid = crate::proc::current();
        let mut procs = PROCS.lock().unwrap();
        let proc = procs.get_or_insert_with(HashMap::new).get_mut(&pid)?;
        t.proc_generation = Some(proc.generation.clone());
        let generation = proc.generation.load(Ordering::Acquire);
        if t.generation == generation {
            return None;
        }
        if !t.init {
            unsafe { vnvm_wasm_thread_init(t.teb) };
            t.init = true;
        }
        let mut mem = Linear;
        while t.have < proc.dlls.len() {
            let d = &proc.dlls[t.have];
            let m = &d.module;
            let slots = &crate::wasmdll_slots::SLOTS;
            let n = m.float.len();
            if t.used + n > slots.len() {
                eprintln!("vnwasm: out of entry table slots");
                std::process::abort();
            }
            let entries: Vec<u32> = slots[t.used..t.used + n].iter().map(|&f| f as usize as u32).collect();
            t.used += n;
            let tags = proc.tags.as_ref().map_or(0, |t| t.as_ptr() as usize as u32);
            let r = unsafe {
                vnvm_wasm_instantiate(t.have as u32, m.bytes.as_ptr(), m.bytes.len(), d.mem, d.slot, d.image, m.table_size, entries.as_ptr(), n as u32, tags)
            };
            if r < 0 {
                eprintln!("vnwasm: cannot instantiate the wasm DLL at {:#x}", d.image);
                std::process::abort();
            }
            t.mods.push((d.slot, m.table_size, m.clone(), entries));
            t.have += 1;
            t.got_set.push(false);
        }
        let mut fixup = None;
        for (i, d) in proc.dlls.iter_mut().enumerate() {
            let first = need.is_some_and(|s| s >= d.slot && s < d.slot + d.module.table_size) && !d.fixed;
            if !t.got_set[i] && (d.fixed || first) {
                for (name, rva, image) in &d.module.got {
                    let v = if *image { d.image + rva } else { rd32(&mut mem, d.image + rva) };
                    unsafe { vnvm_wasm_got(i as u32, name.as_ptr(), name.len(), v) };
                }
                t.got_set[i] = true;
            }
            if first {
                d.fixed = true;
                proc.generation.fetch_add(1, Ordering::Release);
                fixup = Some(i as u32);
            }
        }
        // (a module some other thread fixes later: its GOT is set at the next sync)
        let generation = proc.generation.load(Ordering::Acquire);
        t.generation = if proc.dlls.iter().all(|d| d.fixed) { generation } else { generation.wrapping_sub(1) };
        fixup
    });
    if let Some(i) = fixup {
        unsafe { vnvm_wasm_fixup(i) };
    }
}

/// (entry function's index in this worker's table, bytes popped, returns f64) of a slot
fn lookup(slot: u32) -> Option<(u32, u32, bool)> {
    THREAD.with(|t| {
        let t = t.borrow();
        let (s0, _, m, entries) = t.mods.iter().find(|(s0, n, _, _)| slot >= *s0 && slot < s0 + n)?;
        let (k, pop) = *m.slots.get((slot - s0) as usize)?;
        Some((entries[k as usize], pop as u32, m.float[k as usize]))
    })
}

/// Calls the wasm function at `slot` with x86 arguments at `argp` (and ECX/EDX), the wasm
/// stack below `sp`. EDX:EAX or ST0 (the f64).
fn call(slot: u32, argp: u32, ecx: u32, edx: u32, sp: u32) -> Option<(u32, Result<u64, f64>)> {
    sync(Some(slot));
    let (f, pop, float) = lookup(slot)?;
    // (the entry `__vn_in<k>` of the module's instance on this thread, crates/vnwasm)
    let r = unsafe {
        if float {
            let f: extern "C" fn(u32, u32, u32, u32, u32) -> f64 = std::mem::transmute(f as usize);
            Err(f(slot, argp, ecx, edx, sp))
        } else {
            let f: extern "C" fn(u32, u32, u32, u32, u32) -> u64 = std::mem::transmute(f as usize);
            Ok(f(slot, argp, ecx, edx, sp))
        }
    };
    Some((pop, r))
}

/// VNVM_WASM_STATS=1: entries (x86 -> wasm) and x86 calls (wasm -> x86), printed at powers of 2
static ENTRIES: AtomicU64 = AtomicU64::new(0);
static X86_CALLS: AtomicU64 = AtomicU64::new(0);

fn stats() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VNVM_WASM_STATS").is_some())
}

static TARGETS: Mutex<Option<HashMap<u32, u64>>> = Mutex::new(None);

fn count(c: &AtomicU64) {
    count_at(c, 0)
}

/// (with the x86 address called or entered, to list the busiest at 2^16 and up)
fn count_at(c: &AtomicU64, at: u32) {
    if !stats() {
        return;
    }
    let n = c.fetch_add(1, Ordering::Relaxed) + 1;
    let mut t = TARGETS.lock().unwrap();
    let t = t.get_or_insert_with(HashMap::new);
    *t.entry(at).or_default() += 1;
    if n.is_power_of_two() {
        eprintln!(
            "vnwasm: {} entries, {} x86 calls, {} syscalls",
            ENTRIES.load(Ordering::Relaxed),
            X86_CALLS.load(Ordering::Relaxed),
            SYSCALLS.load(Ordering::Relaxed)
        );
        if n >= 1 << 16 {
            let mut v: Vec<_> = t.iter().map(|(a, n)| (*n, *a)).collect();
            v.sort_unstable_by(|a, b| b.cmp(a));
            for (n, a) in v.iter().take(40) {
                eprintln!("vnwasm: target {a:08x} {n}");
            }
        }
    }
}

/// The x86 core reached `BIAS + slot`: x86 code called a wasm DLL function.
#[no_mangle]
pub unsafe extern "C" fn vnhost_wasm_enter(cpu: *mut Cpu) {
    count_at(&ENTRIES, (*cpu).eip);
    let c = &mut *cpu;
    let mut mem = Linear;
    let slot = c.eip - BIAS;
    let esp = c.regs[vnx86::ESP];
    let ret = rd32(&mut mem, esp);
    match call(slot, esp + 4, c.regs[vnx86::ECX], c.regs[vnx86::EDX], esp & !15) {
        Some((pop, r)) => {
            let c = &mut *cpu;
            match r {
                Ok(v) => {
                    c.regs[vnx86::EAX] = v as u32;
                    c.regs[vnx86::EDX] = (v >> 32) as u32;
                }
                Err(f) => c.fpu.push(f),
            }
            c.regs[vnx86::ESP] = esp + 4 + pop;
            c.eip = ret;
        }
        None => {
            eprintln!("vnwasm: call to {:#x}, no wasm function there (from {ret:#x})", c.eip);
            std::process::abort();
        }
    }
}

/// Wasm DLL code calls `target` with x86 arguments at `argp` (imported as `vn.x86_call`).
#[no_mangle]
pub extern "C" fn vnhost_wasm_x86_call(target: u32, argp: u32) -> u64 {
    if target.wrapping_sub(BIAS) < SLOTS {
        // a wasm function this thread hasn't instantiated yet
        return match call(target - BIAS, argp, 0, 0, argp & !15) {
            Some((_, Ok(v))) => v,
            Some((_, Err(f))) => f.to_bits(),
            None => {
                eprintln!("vnwasm: call to {target:#x}: no wasm function there");
                std::process::abort();
            }
        };
    }
    count_at(&X86_CALLS, target);
    let f = THREAD.with(|t| t.borrow().call_x86).expect("x86 call on a thread without a CPU");
    f(target, argp)
}

/// Same for functions returning a float (`vn.x86_call_f`): the x86 code returns it in ST0.
#[no_mangle]
pub extern "C" fn vnhost_wasm_x86_call_f(target: u32, argp: u32) -> f64 {
    if target.wrapping_sub(BIAS) < SLOTS {
        return match call(target - BIAS, argp, 0, 0, argp & !15) {
            Some((_, Err(f))) => f,
            Some((_, Ok(v))) => f64::from_bits(v),
            None => std::process::abort(),
        };
    }
    count_at(&X86_CALLS, target);
    let (f, cpu) = THREAD.with(|t| {
        let t = t.borrow();
        (t.call_x86.expect("x86 call on a thread without a CPU"), t.cpu)
    });
    f(target, argp);
    let c = unsafe { &mut *cpu };
    let v = c.fpu.st(0);
    c.fpu.pop();
    v
}
