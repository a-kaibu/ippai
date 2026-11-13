//! Win16: NE programs (Windows 3.x) in koji's 16-bit protected mode.
//!
//! Selectors are indexes into a table of segment bases in guest memory (`Cpu::ldt`); every
//! segment is a block of guest memory, so a far pointer's linear address is base + offset and
//! the 32-bit handlers see plain pointers. Imports are far pointers into a thunk segment whose
//! base is in the trap range: a far call there stops the CPU and `dispatch` runs the Rust
//! function with the pascal arguments on the 16-bit stack (specs16.txt, from Wine's .spec
//! files). Most functions convert their arguments and call sake's own 32-bit handler
//! (w16api.rs); window procedures go the other way through `call16`.

use crate::api::{self, Ctx, Handler};
use crate::mem::{r16, r32, w16, w32, w8};
use crate::HashMap;
use std::cell::Cell;
use std::sync::{Mutex, OnceLock};
use koji::{Cpu, EAX, ECX, EDX, ESP};

/// Base of the thunk segment (in the CPU's trap range): import i is at offset 4*i.
pub const TRAP: u32 = 0xffc0_0000;
/// Offset in the thunk segment that `call16`'s callee returns to.
const RET_OFF: u32 = 0xfff0;

const NSEL: u32 = 8192;

pub struct Seg {
    pub base: u32,
    pub size: u32,
}

struct St {
    ldt: u32,
    segs: Vec<Option<Seg>>,
    /// selector of the thunk segment
    thunk: u16,
    /// imports: (module, ordinal) -> thunk index
    by_ord: HashMap<(String, u16), u32>,
    entries: Vec<Api>,
    /// local heaps by data segment selector: (offset -> size) of blocks in use
    heaps: HashMap<u16, std::collections::BTreeMap<u16, u16>>,
}

pub type H16 = fn(&mut C16) -> u32;

struct Api {
    module: String,
    name: String,
    conv: Conv,
    args: Vec<u8>,
    f: Option<H16>,
}

#[derive(Clone, Copy, PartialEq)]
enum Conv {
    Pascal,
    Varargs,
    Register,
    Stub,
}

static ST: Mutex<Option<St>> = Mutex::new(None);

fn with<R>(f: impl FnOnce(&mut St) -> R) -> R {
    f(ST.lock().unwrap().as_mut().expect("w16 state"))
}

/// The running program (one task).
pub struct Task {
    pub path: String,
    pub hinst: u16,
    pub psp: u16,
    pub env: u16,
    pub segs: Vec<u16>,
    pub file: Vec<u8>,
    pub ne: u32,
    pub cmd_show: u16,
}

pub static TASK: OnceLock<Task> = OnceLock::new();

pub fn task() -> &'static Task {
    TASK.get().expect("w16 task")
}

static ON: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// A Win16 program is running.
pub fn on() -> bool {
    ON.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn is_ne(path: &str) -> bool {
    let Ok(d) = kura::fs::read(path) else { return false };
    if d.len() < 0x40 || &d[..2] != b"MZ" {
        return false;
    }
    let o = u32::from_le_bytes(d[0x3c..0x40].try_into().unwrap()) as usize;
    d.get(o..o + 2) == Some(b"NE")
}

// ---------------------------------------------------------------------------------------------
// selectors

pub fn sel_of(i: u32) -> u16 {
    (i << 3 | 7) as u16
}

/// A new selector for [base, base+size).
pub fn sel_alloc(base: u32, size: u32) -> u16 {
    sel_alloc_n(base, size, 1)
}

/// `n` consecutive selectors (a huge block: each 64 KB further, __AHINCR apart).
pub fn sel_alloc_n(base: u32, size: u32, n: u32) -> u16 {
    with(|s| {
        let mut i = 1u32;
        'find: loop {
            if i + n > NSEL {
                panic!("w16: out of selectors");
            }
            for k in 0..n {
                if s.segs[(i + k) as usize].is_some() {
                    i += k + 1;
                    continue 'find;
                }
            }
            break;
        }
        for k in 0..n {
            let b = base + k * 0x10000;
            s.segs[(i + k) as usize] = Some(Seg { base: b, size: size.saturating_sub(k * 0x10000) });
            w32(s.ldt + 4 * (i + k), b);
        }
        sel_of(i)
    })
}

pub fn sel_free(sel: u16) {
    with(|s| {
        let i = (sel >> 3) as usize;
        if i > 0 && i < s.segs.len() {
            s.segs[i] = None;
            w32(s.ldt + 4 * i as u32, 0);
        }
    })
}

pub fn sel_set_base(sel: u16, base: u32, size: u32) {
    with(|s| {
        let i = (sel >> 3) as usize;
        s.segs[i] = Some(Seg { base, size });
        w32(s.ldt + 4 * i as u32, base);
    })
}

pub fn sel_base(sel: u16) -> Option<(u32, u32)> {
    with(|s| s.segs.get((sel >> 3) as usize).and_then(|x| x.as_ref()).map(|x| (x.base, x.size)))
}

/// Linear address of a far pointer (selector 0: the value itself, an integer resource id
/// or NULL).
pub fn lin(p: u32) -> u32 {
    let sel = (p >> 16) as u16;
    if sel == 0 {
        return p;
    }
    match sel_base(sel) {
        Some((b, _)) => b.wrapping_add(p & 0xffff),
        None => 0,
    }
}

// ---------------------------------------------------------------------------------------------
// handles: sake's are 32-bit; 16-bit code gets small numbers for those above 0xffff

struct HMap {
    to16: HashMap<u32, u16>,
    to32: HashMap<u16, u32>,
    next: u16,
}

static HM: Mutex<Option<HMap>> = Mutex::new(None);

pub fn h16(h: u32) -> u16 {
    if h < 0x10000 {
        return h as u16;
    }
    let mut g = HM.lock().unwrap();
    let m = g.get_or_insert_with(|| HMap { to16: HashMap::default(), to32: HashMap::default(), next: 0x8000 });
    if let Some(&v) = m.to16.get(&h) {
        return v;
    }
    loop {
        let v = m.next;
        m.next = if m.next >= 0xfff8 { 0x8000 } else { m.next + 4 };
        if !m.to32.contains_key(&v) {
            m.to16.insert(h, v);
            m.to32.insert(v, h);
            return v;
        }
    }
}

pub fn h32(h: u16) -> u32 {
    let g = HM.lock().unwrap();
    g.as_ref().and_then(|m| m.to32.get(&h).copied()).unwrap_or(h as u32)
}

/// The 32-bit handle is gone (DeleteObject, DestroyWindow...).
pub fn h_release(h: u16) {
    let mut g = HM.lock().unwrap();
    if let Some(m) = g.as_mut() {
        if let Some(v) = m.to32.remove(&h) {
            m.to16.remove(&v);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// memory

/// GlobalAlloc: a block and its selector(s); the handle is the (first) selector.
pub fn global_alloc(size: u32, zero: bool) -> u16 {
    let size = size.max(16);
    let n = size.div_ceil(0x10000);
    let b = crate::mem::heap_alloc(size + 16, zero);
    if b == 0 {
        return 0;
    }
    sel_alloc_n(b, size, n)
}

pub fn global_size(sel: u16) -> u32 {
    sel_base(sel).map_or(0, |(_, s)| s)
}

pub fn global_free(sel: u16) {
    let Some((b, size)) = sel_base(sel) else { return };
    for k in 0..size.div_ceil(0x10000).max(1) {
        sel_free(sel + 8 * k as u16);
    }
    crate::mem::heap_free(b);
}

pub fn global_realloc(sel: u16, size: u32, zero: bool) -> u16 {
    let Some((b, old)) = sel_base(sel) else { return 0 };
    let size = size.max(16);
    let (n_old, n_new) = (old.div_ceil(0x10000).max(1), size.div_ceil(0x10000).max(1));
    if n_new != n_old {
        // (the selectors must stay consecutive: a new handle)
        let s = global_alloc(size, zero);
        if s != 0 {
            crate::mem::copy(sel_base(s).unwrap().0, b, old.min(size));
            global_free(sel);
        }
        return s;
    }
    let nb = crate::mem::heap_realloc(b, size + 16, zero);
    if nb == 0 {
        return 0;
    }
    for k in 0..n_new {
        sel_set_base(sel + 8 * k as u16, nb + k * 0x10000, size.saturating_sub(k * 0x10000));
    }
    sel
}

/// LocalAlloc in the data segment `ds`: first fit between the stack and the segment's end.
pub fn local_alloc(ds: u16, size: u16, zero: bool) -> u16 {
    let size = (size.max(2) + 3) & !3;
    let r = with(|s| {
        let heap = s.heaps.get_mut(&ds)?;
        let lo = *heap.get(&0).unwrap_or(&0x100); // (key 0: the heap's start)
        let mut at = lo;
        for (&o, &n) in heap.range(1..) {
            if o >= at && o - at >= size + 4 {
                break;
            }
            at = at.max(o + n + 4);
        }
        if (at as u32) + size as u32 + 4 > 0xfff0 {
            return None;
        }
        heap.insert(at + 4, size);
        Some(at + 4)
    });
    let Some(off) = r else { return 0 };
    if zero {
        if let Some((b, _)) = sel_base(ds) {
            crate::mem::fill(b + off as u32, size as u32, 0);
        }
    }
    off
}

pub fn local_size(ds: u16, off: u16) -> u16 {
    with(|s| s.heaps.get(&ds).and_then(|h| h.get(&off).copied()).unwrap_or(0))
}

pub fn local_free(ds: u16, off: u16) {
    with(|s| {
        if let Some(h) = s.heaps.get_mut(&ds) {
            if off != 0 {
                h.remove(&off);
            }
        }
    })
}

pub fn local_realloc(ds: u16, off: u16, size: u16, zero: bool) -> u16 {
    let old = local_size(ds, off);
    if off == 0 || old == 0 {
        return local_alloc(ds, size, zero);
    }
    let n = local_alloc(ds, size, zero);
    if n == 0 {
        return 0;
    }
    let b = sel_base(ds).unwrap().0;
    crate::mem::copy(b + n as u32, b + off as u32, old.min(size) as u32);
    local_free(ds, off);
    n
}

// ---------------------------------------------------------------------------------------------
// scratch: 32-bit frames for calling sake's handlers, 16-bit memory for converted structs

thread_local! {
    static SCR32: Cell<u32> = const { Cell::new(0) };
    static SCR16: Cell<(u16, u32, u16)> = const { Cell::new((0, 0, 0)) };
}

/// Calls a 32-bit handler with `args` as its stack arguments; returns (EAX, EDX).
pub fn call32_with(cpu: &mut Cpu, f: Handler, args: &[u32], trap: u32) -> (u32, u32) {
    let top = SCR32.with(|s| s.get());
    let sp = (top - 4 * (args.len() as u32 + 1) - 64) & !15;
    w32(sp, 0);
    for (i, &a) in args.iter().enumerate() {
        w32(sp + 4 + 4 * i as u32, a);
    }
    SCR32.with(|s| s.set(sp));
    let mut c = Ctx { cpu, esp: sp, edx: None, jumped: false, st0: None, trap, this: None };
    let r = f(&mut c);
    let edx = c.edx.unwrap_or(0);
    SCR32.with(|s| s.set(top));
    (r, edx)
}

/// Calls sake's 32-bit `dll!name` with `args`.
pub fn call32(cpu: &mut Cpu, func: &str, args: &[u32]) -> u32 {
    let (dll, name) = func.split_once('!').unwrap();
    let Some(f) = api::find_impl(dll, name) else {
        crate::fatal(cpu, &format!("w16: no 32-bit {func}"));
    };
    call32_with(cpu, f, args, 0).0
}

/// What `crate::call` does in a Win16 program: the callee can only be a trap (a 16-bit
/// window procedure's thunk, a system class's procedure), so its handler runs directly.
pub fn call_trap(cpu: &mut Cpu, f: u32, args: &[u32]) -> u32 {
    match api::handler_of(f) {
        Some(h) => call32_with(cpu, h, args, f).0,
        None => crate::fatal(cpu, &format!("w16: call to {f:#x}, not a host function")),
    }
}

/// 16-bit scratch memory: (far pointer, linear address) of `n` bytes; free with `scratch_reset`.
pub fn scratch(n: u32) -> (u32, u32) {
    SCR16.with(|s| {
        let (sel, base, top) = s.get();
        let n = ((n + 3) & !3) as u16;
        let at = top.checked_sub(n).expect("w16 scratch");
        s.set((sel, base, at));
        crate::mem::fill(base + at as u32, n as u32, 0);
        ((sel as u32) << 16 | at as u32, base + at as u32)
    })
}

pub fn scratch_mark() -> u16 {
    SCR16.with(|s| s.get().2)
}

pub fn scratch_reset(m: u16) {
    SCR16.with(|s| {
        let (sel, base, _) = s.get();
        s.set((sel, base, m));
    })
}

/// A copy of the C string at `p` (linear) in scratch memory: its far pointer.
pub fn scratch_str(p: u32) -> u32 {
    if p < 0x10000 {
        return p;
    }
    let s = crate::mem::cstr(p);
    let (fp, l) = scratch(s.len() as u32 + 1);
    crate::mem::write(l, &s);
    fp
}

// ---------------------------------------------------------------------------------------------
// calls between 16-bit code and Rust

pub struct C16<'a> {
    pub cpu: &'a mut Cpu,
    /// the arguments in declaration order: words, longs, far pointers as they are; `p`
    /// pointers already linear
    pub args: Vec<u32>,
    /// SS:SP's linear address at the call (the far return address)
    pub sp: u32,
    /// for varargs functions: linear address of the first variable argument
    pub va: u32,
    pub module: String,
    pub name: String,
    /// specs16.txt's argument types
    pub types: Vec<u8>,
    /// the arguments as pushed
    pub raw: Vec<u32>,
}

impl C16<'_> {
    pub fn a(&self, i: usize) -> u32 {
        self.args[i]
    }
    pub fn w(&self, i: usize) -> u16 {
        self.args[i] as u16
    }
    pub fn s(&self, i: usize) -> i32 {
        self.args[i] as u16 as i16 as i32
    }
    pub fn h(&self, i: usize) -> u32 {
        h32(self.args[i] as u16)
    }
    pub fn ds(&self) -> u16 {
        self.cpu.sreg[3]
    }
}

/// Implements a 16-bit function (module as in specs16.txt).
pub fn implement16(module: &str, name: &str, f: H16) {
    IMPL16.lock().unwrap().get_or_insert_with(HashMap::default).insert(format!("{module}.{name}"), f);
}

static IMPL16: Mutex<Option<HashMap<String, H16>>> = Mutex::new(None);

fn spec_line(module: &str, ord: u16) -> Option<(String, Conv, Vec<u8>, Option<u32>)> {
    static SPECS: OnceLock<HashMap<(String, u16), (String, Conv, Vec<u8>, Option<u32>)>> = OnceLock::new();
    let m = SPECS.get_or_init(|| {
        let mut m = HashMap::default();
        for l in include_str!("specs16.txt").lines() {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 4 {
                continue;
            }
            let ord: u16 = f[1].parse().unwrap();
            let (conv, args, eq) = match f[3] {
                "=" => (Conv::Stub, vec![], f.get(4).and_then(|v| parse_num(v))),
                "stub" => (Conv::Stub, vec![], None),
                c => {
                    let conv = match c {
                        "varargs" | "cdecl" => Conv::Varargs,
                        "register" => Conv::Register,
                        _ => Conv::Pascal,
                    };
                    let a = f.get(4).map_or(vec![], |a| if *a == "-" { vec![] } else { a.bytes().collect() });
                    (conv, a, None)
                }
            };
            m.insert((f[0].to_string(), ord), (f[2].to_string(), conv, args, eq));
        }
        m
    });
    m.get(&(module.to_string(), ord)).cloned()
}

fn parse_num(s: &str) -> Option<u32> {
    if let Some(h) = s.strip_prefix("0x") { u32::from_str_radix(h, 16).ok() } else { s.parse().ok() }
}

/// An ordinal's name in a system module.
fn spec_ordinal(module: &str, name: &str) -> Option<u16> {
    let text = include_str!("specs16.txt");
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 3 && f[0] == module && f[2].eq_ignore_ascii_case(name) {
            return f[1].parse().ok();
        }
    }
    None
}

/// What an import of module!ord resolves to: a far pointer to its thunk, or an equate's value.
fn import(module: &str, ord: u16) -> Result<u32, String> {
    let module = module.to_ascii_uppercase();
    let Some((name, conv, args, eq)) = spec_line(&module, ord) else {
        return Err(format!("{module}.{ord} is not a known function"));
    };
    if let Some(v) = eq {
        return Ok(v);
    }
    let f = IMPL16.lock().unwrap().as_ref().and_then(|m| m.get(&format!("{module}.{name}")).copied());
    with(|s| {
        let i = match s.by_ord.get(&(module.clone(), ord)) {
            Some(&i) => i,
            None => {
                let i = s.entries.len() as u32;
                s.entries.push(Api { module: module.clone(), name, conv, args, f });
                s.by_ord.insert((module.clone(), ord), i);
                i
            }
        };
        Ok((s.thunk as u32) << 16 | 4 * i)
    })
}

fn trace() -> u32 {
    static T: OnceLock<u32> = OnceLock::new();
    *T.get_or_init(|| std::env::var("SAKE_TRACE").ok().and_then(|s| s.parse().ok()).unwrap_or(0))
}

/// A far call reached the thunk segment at linear `a`. True: it is `call16`'s return.
pub fn dispatch(cpu: &mut Cpu, a: u32) -> bool {
    let off = a - TRAP;
    if off == RET_OFF {
        return true;
    }
    let i = off / 4;
    let (module, name, conv, types, f) = with(|s| {
        let e = &s.entries[i as usize];
        (e.module.clone(), e.name.clone(), e.conv, e.args.clone(), e.f)
    });
    let sp = cpu.sbase[2].wrapping_add(cpu.regs[ESP] & 0xffff);
    let size = |t: u8| if matches!(t, b'w' | b's') { 2 } else if t == b'd' { 8 } else { 4 };
    let total: u32 = types.iter().map(|&t| size(t)).sum();
    let mut args = vec![0u32; types.len()];
    let mut va = 0;
    if conv == Conv::Varargs {
        // (cdecl: the first argument lowest)
        let mut o = sp + 4;
        for (k, &t) in types.iter().enumerate() {
            args[k] = if size(t) == 2 { r16(o) as u32 } else { r32(o) };
            o += size(t);
        }
        va = o;
    } else {
        let mut o = sp + 4;
        for (k, &t) in types.iter().enumerate().rev() {
            args[k] = if size(t) == 2 { r16(o) as u32 } else { r32(o) };
            o += size(t);
        }
    }
    let raw = args.clone();
    for (k, &t) in types.iter().enumerate() {
        if t == b'p' {
            args[k] = lin(args[k]);
        }
    }
    let tr = trace();
    if tr > 0 {
        let s: Vec<String> = args.iter().map(|v| format!("{v:x}")).collect();
        eprintln!("w16 {module}.{name}({}) from {:04x}:{:04x}", s.join(","), r16(sp + 2), r16(sp));
    }
    let mut c = C16 { cpu, args, sp, va, module: module.clone(), name: name.clone(), types, raw };
    let r = match f {
        Some(f) => f(&mut c),
        None => {
            static SEEN: Mutex<Option<crate::HashSet<String>>> = Mutex::new(None);
            let key = format!("{module}.{name}");
            if SEEN.lock().unwrap().get_or_insert_with(Default::default).insert(key.clone()) {
                eprintln!("sake: unimplemented 16-bit {key} (returns 0)");
            }
            if conv == Conv::Stub || conv == Conv::Register {
                // (no argument list known: a stub returning to the caller)
            }
            0
        }
    };
    if tr > 1 {
        eprintln!("w16   -> {r:x}");
    }
    let cpu = c.cpu;
    let ret_ip = r16(sp) as u32;
    let ret_cs = r16(sp + 2);
    if conv != Conv::Register {
        cpu.regs[EAX] = (cpu.regs[EAX] & 0xffff_0000) | (r & 0xffff);
        cpu.regs[EDX] = (cpu.regs[EDX] & 0xffff_0000) | (r >> 16);
    }
    let pop = 4 + if conv == Conv::Pascal { total } else { 0 };
    let nsp = (cpu.regs[ESP] & 0xffff).wrapping_add(pop) & 0xffff;
    cpu.regs[ESP] = (cpu.regs[ESP] & 0xffff_0000) | nsp;
    set_seg(cpu, 1, ret_cs);
    cpu.eip = ret_ip;
    if conv == Conv::Register {
        if let Some(after) = AFTER.with(|a| a.take()) {
            after(cpu);
        }
    }
    false
}

thread_local! {
    /// a register function's last step, after the return (InitTask pushes a word)
    pub static AFTER: Cell<Option<fn(&mut Cpu)>> = const { Cell::new(None) };
}

/// Loads segment register `i` (ES CS SS DS FS GS).
pub fn set_seg(cpu: &mut Cpu, i: usize, sel: u16) {
    cpu.sreg[i] = sel;
    cpu.sbase[i] = if sel >> 3 == 0 { 0 } else { sel_base(sel).map_or(0, |b| b.0) };
}

/// An argument for `call16`.
#[derive(Clone, Copy)]
pub enum A {
    W(u16),
    L(u32),
}

/// Calls the 16-bit far function `f` (pascal: arguments pushed in order) with DS = the
/// program's data segment; returns DX:AX. Everything else comes back as it was.
pub fn call16(cpu: &mut Cpu, f: u32, args: &[A]) -> u32 {
    let saved = (cpu.regs, cpu.eip, cpu.sreg, cpu.sbase, cpu.mode16);
    cpu.materialize();
    let eflags = cpu.eflags;
    if cpu.mode16 == 0 {
        // (not from 16-bit code: the program's stack, below where it was)
        fatal16(cpu, "call16 outside 16-bit code");
    }
    let mut sp = (cpu.regs[ESP] & 0xffff).wrapping_sub(0x10) & !1;
    let ss = cpu.sbase[2];
    for a in args {
        match *a {
            A::W(v) => {
                sp -= 2;
                w16(ss + sp, v);
            }
            A::L(v) => {
                sp -= 4;
                w32(ss + sp, v);
            }
        }
    }
    let thunk = with(|s| s.thunk);
    sp -= 4;
    w16(ss + sp, RET_OFF as u16);
    w16(ss + sp + 2, thunk);
    cpu.regs[ESP] = (cpu.regs[ESP] & 0xffff_0000) | sp;
    let ds = task().hinst;
    set_seg(cpu, 3, ds);
    set_seg(cpu, 0, ds);
    cpu.regs[EAX] = ds as u32;
    set_seg(cpu, 1, (f >> 16) as u16);
    cpu.eip = f & 0xffff;
    crate::run(cpu, 0);
    let r = (cpu.regs[EDX] & 0xffff) << 16 | (cpu.regs[EAX] & 0xffff);
    cpu.regs = saved.0;
    cpu.eip = saved.1;
    cpu.sreg = saved.2;
    cpu.sbase = saved.3;
    cpu.mode16 = saved.4;
    cpu.eflags = eflags;
    cpu.lazy.op = 0;
    r
}

pub fn fatal16(cpu: &mut Cpu, msg: &str) -> ! {
    let r = &cpu.regs;
    eprintln!(
        "sake w16: {msg}\n  cs:ip {:04x}:{:04x} ax {:04x} bx {:04x} cx {:04x} dx {:04x} si {:04x} di {:04x} bp {:04x} ss:sp {:04x}:{:04x} ds {:04x} es {:04x}",
        cpu.sreg[1], cpu.eip & 0xffff, r[0] & 0xffff, r[3] & 0xffff, r[1] & 0xffff, r[2] & 0xffff, r[6] & 0xffff, r[7] & 0xffff, r[5] & 0xffff,
        cpu.sreg[2], r[4] & 0xffff, cpu.sreg[3], cpu.sreg[0]
    );
    let pc = cpu.pc16();
    if pc < 0xfe00_0000 {
        eprintln!("  code: {:02x?}", crate::mem::slice(pc, 12));
    }
    let sp = cpu.sbase[2] + (r[4] & 0xffff);
    eprintln!("  stack: {:04x?}", (0..12).map(|k| r16(sp + 2 * k)).collect::<Vec<_>>());
    unsafe { libc::exit(3) }
}

/// The CPU stopped in 16-bit code with `code` (lib.rs's X_*): faults are fatal, INT 21h is DOS.
pub fn exception(cpu: &mut Cpu, stop: kura::cpu::Stop) {
    use kura::cpu::Stop;
    match stop {
        Stop::Int(0x21) => crate::w16api::dos(cpu),
        Stop::Int(n @ (0x2f | 0x31 | 0x1a | 0x16 | 0x2a | 0x5c | 0x3f)) => crate::w16api::int_other(cpu, n),
        Stop::Int(n) => fatal16(cpu, &format!("int {n:#x}")),
        Stop::Fault { addr, write } => fatal16(cpu, &format!("fault at {addr:#x} (write {write})")),
        Stop::Divide => fatal16(cpu, "divide error"),
        Stop::Invalid => fatal16(cpu, "illegal instruction"),
        _ => fatal16(cpu, &format!("cpu exit {stop:?}")),
    }
}

// ---------------------------------------------------------------------------------------------
// the NE loader

fn u16at(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}

/// Far pointer of an entry (by ordinal) of the program.
fn entry_point(d: &[u8], ne: usize, segs: &[u16], ord: u16) -> Option<u32> {
    let mut p = ne + u16at(d, ne + 4) as usize;
    let mut n = 1u16;
    loop {
        let count = d[p] as u16;
        if count == 0 {
            return None;
        }
        let ty = d[p + 1];
        p += 2;
        if ty == 0 {
            n += count;
            continue;
        }
        let sz = if ty == 0xff { 6 } else { 3 };
        if ord < n + count {
            let e = p + sz * (ord - n) as usize;
            let (seg, off) = if ty == 0xff { (d[e + 3], u16at(d, e + 4)) } else { (ty, u16at(d, e + 1)) };
            return Some((segs[seg as usize - 1] as u32) << 16 | off as u32);
        }
        n += count;
        p += sz * count as usize;
    }
}

/// Loads the program and starts it.
pub fn start(cpu: &mut Cpu, exe: &str, wpath: &str, args: &[String]) -> ! {
    ON.store(true, std::sync::atomic::Ordering::Relaxed);
    let ldt = crate::mem::alloc(4 * NSEL).expect("w16 ldt");
    let thunk_base = TRAP;
    {
        let mut segs = Vec::with_capacity(NSEL as usize);
        segs.resize_with(NSEL as usize, || None);
        *ST.lock().unwrap() = Some(St { ldt, segs, thunk: 0, by_ord: HashMap::default(), entries: vec![], heaps: HashMap::default() });
    }
    let thunk = sel_alloc(thunk_base, 0x10000);
    with(|s| s.thunk = thunk);
    cpu.ldt = ldt;
    // scratch areas
    let s32 = crate::mem::alloc(0x40000).expect("w16 scratch");
    SCR32.with(|s| s.set(s32 + 0x40000));
    let s16 = crate::mem::alloc(0x10000).expect("w16 scratch16");
    let s16sel = sel_alloc(s16, 0x10000);
    SCR16.with(|s| s.set((s16sel, s16, 0xfff0)));
    crate::w16api::init();

    let d = kura::fs::read(exe).expect("read exe");
    let ne = u32::from_le_bytes(d[0x3c..0x40].try_into().unwrap()) as usize;
    let h = |o: usize| u16at(&d, ne + o);
    let (autodata, heap, mut stack) = (h(0x0e), h(0x10), h(0x12));
    let (ip, cs, mut sp, ss) = (h(0x14), h(0x16), h(0x18), h(0x1a));
    let (nseg, nmod, segtab, modref, impnam, align) = (h(0x1c), h(0x1e), h(0x22), h(0x28), h(0x2a), h(0x32));
    if stack < 0x1400 {
        stack = 0x1400;
    }
    let modname = |i: u16| -> String {
        let r = u16at(&d, ne + modref as usize + 2 * (i as usize - 1)) as usize;
        let p = ne + impnam as usize + r;
        String::from_utf8_lossy(&d[p + 1..p + 1 + d[p] as usize]).into_owned()
    };
    // segments
    let mut segs = vec![];
    let mut info = vec![];
    for k in 0..nseg as usize {
        let e = ne + segtab as usize + 8 * k;
        let (off, len, flags, min) = (u16at(&d, e), u16at(&d, e + 2), u16at(&d, e + 4), u16at(&d, e + 6));
        let fofs = (off as usize) << align;
        let flen = if len == 0 && off != 0 { 0x10000 } else { len as usize };
        let b = crate::mem::alloc(0x10000).expect("segment");
        if off != 0 {
            crate::mem::write(b, &d[fofs..(fofs + flen).min(d.len())]);
        }
        segs.push(sel_alloc(b, 0x10000));
        info.push((fofs, flen, flags, if min == 0 { 0x10000u32 } else { min as u32 }));
    }
    // relocations
    for k in 0..nseg as usize {
        let (fofs, flen, flags, _) = info[k];
        if flags & 0x100 == 0 || fofs == 0 {
            continue;
        }
        let base = sel_base(segs[k]).unwrap().0;
        let mut p = fofs + flen;
        let n = u16at(&d, p);
        p += 2;
        for _ in 0..n {
            let (st, fl, so, a, b) = (d[p], d[p + 1], u16at(&d, p + 2), u16at(&d, p + 4), u16at(&d, p + 6));
            p += 8;
            let target: u32 = match fl & 3 {
                0 => {
                    if a & 0xff == 0xff {
                        entry_point(&d, ne, &segs, b).unwrap_or(0)
                    } else {
                        (segs[(a & 0xff) as usize - 1] as u32) << 16 | b as u32
                    }
                }
                1 => match import(&modname(a), b) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("sake w16: {e}");
                        0
                    }
                },
                2 => {
                    let q = ne + impnam as usize + b as usize;
                    let name = String::from_utf8_lossy(&d[q + 1..q + 1 + d[q] as usize]).into_owned();
                    let m = modname(a);
                    match spec_ordinal(&m.to_ascii_uppercase(), &name).map(|o| import(&m, o)) {
                        Some(Ok(v)) => v,
                        _ => {
                            eprintln!("sake w16: unknown import {m}.{name}");
                            0
                        }
                    }
                }
                _ => continue, // OSFIXUP (floating point emulation): leave the x87 code
            };
            let additive = fl & 4 != 0;
            let mut at = so as u32;
            loop {
                let a = base + at;
                let next = r16(a);
                match st {
                    0 => w8(a, if additive { crate::mem::r8(a).wrapping_add(target as u8) } else { target as u8 }),
                    2 => w16(a, (target >> 16) as u16),
                    3 => {
                        if additive {
                            w16(a, r16(a).wrapping_add(target as u16));
                        } else {
                            w16(a, target as u16);
                        }
                        w16(a + 2, (target >> 16) as u16);
                    }
                    5 => w16(a, if additive { r16(a).wrapping_add(target as u16) } else { target as u16 }),
                    _ => eprintln!("sake w16: relocation type {st}"),
                }
                if additive || next == 0xffff || next as u32 == at {
                    break;
                }
                at = next as u32;
            }
        }
    }
    let dgroup = if autodata == 0 { 0 } else { segs[autodata as usize - 1] };
    let dmin = if autodata == 0 { 0 } else { info[autodata as usize - 1].3 };
    if sp == 0 {
        sp = (dmin + stack as u32).min(0xfffe) as u16;
    }
    // the local heap: above the stack
    let heap_lo = ((sp as u32 + 15) & !15).min(0xff00) as u16;
    with(|s| {
        let mut m = std::collections::BTreeMap::new();
        m.insert(0u16, heap_lo);
        s.heaps.insert(dgroup, m);
    });
    // PSP and environment
    let env = global_alloc(0x400, true);
    let eb = sel_base(env).unwrap().0;
    let mut ev = b"PATH=C:\\WINDOWS\0WINDIR=C:\\WINDOWS\0\0\x01\0".to_vec();
    ev.extend_from_slice(wpath.as_bytes());
    ev.push(0);
    crate::mem::write(eb, &ev);
    let psp = global_alloc(0x100, true);
    let pb = sel_base(psp).unwrap().0;
    w16(pb, 0x20cd);
    w16(pb + 0x2c, env);
    let tail = args.join(" ");
    let tail = crate::text::s2a(&tail);
    let n = tail.len().min(126);
    w8(pb + 0x80, n as u8);
    crate::mem::write(pb + 0x81, &tail[..n]);
    w8(pb + 0x81 + n as u32, 0); // (as Wine: NUL, not CR; WinMain's lpCmdLine points here)
    let _ = TASK.set(Task { path: wpath.to_string(), hinst: dgroup, psp, env, segs: segs.clone(), file: d.clone(), ne: ne as u32, cmd_show: 1 });

    // registers at the entry (Wine's NE_StartTask)
    cpu.mode16 = 1;
    cpu.regs = [0; 8];
    cpu.regs[3] = stack as u32;
    cpu.regs[ECX] = heap as u32;
    cpu.regs[7] = dgroup as u32;
    set_seg(cpu, 0, psp);
    set_seg(cpu, 3, dgroup);
    set_seg(cpu, 2, if ss == 0 { dgroup } else { segs[ss as usize - 1] });
    set_seg(cpu, 1, segs[cs as usize - 1]);
    cpu.regs[ESP] = sp as u32;
    cpu.eip = ip as u32;
    eprintln!("sake w16: {wpath}: {nseg} segments, {nmod} modules, cs:ip {:04x}:{ip:04x} ss:sp {:04x}:{sp:04x}", cpu.sreg[1], cpu.sreg[2]);
    crate::run(cpu, 0);
    unsafe { libc::exit(0) }
}
