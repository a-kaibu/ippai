//! The crash log: what a run did lately, kept where the page reads it straight from the linear
//! memory (web/src/crashlog.ts), so a crash can be diagnosed afterwards even when the wasm
//! instance is gone (a trap, an abort) or the tab gets killed (the page saves it every few
//! seconds).
//!
//! - `EV`: events. Every guest exception (access violation, illegal instruction, int3, divide
//!   error, ...) as it leaves the x86 core, with the registers, the code and stack there, the
//!   frame chain and return-address candidates resolved to `module!export+off`, the thread's
//!   last syscalls / API calls and the JIT's recent region jumps; process starts and ends;
//!   panics.
//! - `OUT`: everything written to stdout/stderr (sake's messages, its "Unhandled
//!   exception" report).
//! - `SNAP`: every guest thread's state now ([`vnhost_crashlog_snapshot`]), rewritten each time.
//!
//! The rings are plain text; a writer reserves its bytes with one `fetch_add`, so lines from
//! different threads never interleave. Nothing on these paths allocates (the page may ask for a
//! snapshot while another thread died holding the allocator's lock), except the sake API-name
//! lookup in a guest thread's own exception record.
//!
//! The cost while nothing goes wrong: four stores per syscall (the thread's trap ring), two per
//! entry into guest code, and a copy of each stderr write.

use core::fmt::{self, Write};
use std::cell::{Cell, UnsafeCell};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::*};
use std::sync::Mutex;

use koji::Cpu;

const EV_SIZE: usize = 1 << 19;
const OUT_SIZE: usize = 1 << 19;
const SNAP_SIZE: usize = 1 << 16;
/// Syscalls / API calls kept per thread.
const TRAPS: usize = 128;

struct Ring<const N: usize> {
    buf: UnsafeCell<[u8; N]>,
    head: AtomicU32,
}

unsafe impl<const N: usize> Sync for Ring<N> {}

impl<const N: usize> Ring<N> {
    const fn new() -> Self {
        Ring {
            buf: UnsafeCell::new([0; N]),
            head: AtomicU32::new(0),
        }
    }

    fn put(&self, b: &[u8]) {
        let b = &b[..b.len().min(N / 4)];
        let at = self.head.fetch_add(b.len() as u32, Relaxed) as usize & (N - 1);
        let p = self.buf.get() as *mut u8;
        let first = b.len().min(N - at);
        unsafe {
            std::ptr::copy_nonoverlapping(b.as_ptr(), p.add(at), first);
            std::ptr::copy_nonoverlapping(b.as_ptr().add(first), p, b.len() - first);
        }
    }

    fn addr(&self) -> u32 {
        self.buf.get() as usize as u32
    }
}

static EV: Ring<EV_SIZE> = Ring::new();
static OUT: Ring<OUT_SIZE> = Ring::new();

struct Snap(UnsafeCell<[u8; SNAP_SIZE]>);
unsafe impl Sync for Snap {}
static SNAP: Snap = Snap(UnsafeCell::new([0; SNAP_SIZE]));
static SNAP_LEN: AtomicU32 = AtomicU32::new(0);
static SNAP_BUSY: AtomicBool = AtomicBool::new(false);

/// Crashes seen (the page polls it) and the latest one's reason.
static CRASHES: AtomicU32 = AtomicU32::new(0);
struct Reason(UnsafeCell<[u8; 256]>);
unsafe impl Sync for Reason {}
static REASON: Reason = Reason(UnsafeCell::new([0; 256]));

struct Info(UnsafeCell<[u32; 16]>);
unsafe impl Sync for Info {}
static INFO: Info = Info(UnsafeCell::new([0; 16]));

/// Where the page finds everything: magic "VNCL", version, then (address, size, address of
/// the head) for EV and OUT, (address, size, address of the length) for SNAP, the address of
/// the crash counter, of the reason (NUL-terminated) and its size.
#[no_mangle]
pub extern "C" fn vnhost_crashlog_info() -> u32 {
    let i = unsafe { &mut *INFO.0.get() };
    *i = [
        0x4c43_4e56,
        1,
        EV.addr(),
        EV_SIZE as u32,
        &EV.head as *const _ as usize as u32,
        OUT.addr(),
        OUT_SIZE as u32,
        &OUT.head as *const _ as usize as u32,
        SNAP.0.get() as usize as u32,
        SNAP_SIZE as u32,
        &SNAP_LEN as *const _ as usize as u32,
        &CRASHES as *const _ as usize as u32,
        REASON.0.get() as usize as u32,
        256,
        0,
        0,
    ];
    i.as_ptr() as usize as u32
}

// ---------------------------------------------------------------------------------------------
// text without allocating

/// A line built on the stack (what does not fit is cut).
pub struct Line<const N: usize> {
    buf: [u8; N],
    n: usize,
}

impl<const N: usize> Line<N> {
    pub fn new() -> Self {
        Line { buf: [0; N], n: 0 }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.n]
    }
    pub fn push(&mut self, b: &[u8]) {
        let k = b.len().min(N - self.n);
        self.buf[self.n..self.n + k].copy_from_slice(&b[..k]);
        self.n += k;
    }
}

impl<const N: usize> Write for Line<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let k = s.len().min(N - self.n);
        self.buf[self.n..self.n + k].copy_from_slice(&s.as_bytes()[..k]);
        self.n += k;
        Ok(())
    }
}

/// "hh:mm:ss.mmm" (UTC) and the thread: the prefix of every record.
fn stamp(w: &mut impl Write) {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    let s = ms / 1000;
    let _ = write!(
        w,
        "{:02}:{:02}:{:02}.{:03} t{} ",
        (s / 3600) % 24,
        (s / 60) % 60,
        s % 60,
        ms % 1000,
        crate::cpu::tid()
    );
}

/// An event line (or lines) in EV.
pub fn event(args: fmt::Arguments) {
    let mut l = Line::<1024>::new();
    stamp(&mut l);
    let _ = l.write_fmt(args);
    let _ = l.write_str("\n");
    EV.put(l.bytes());
}

/// A crash the page should save a report for now.
pub fn crash(reason: &str) {
    let r = unsafe { &mut *REASON.0.get() };
    let n = reason.len().min(r.len() - 1);
    r[..n].copy_from_slice(&reason.as_bytes()[..n]);
    r[n] = 0;
    event(format_args!("CRASH: {reason}"));
    CRASHES.fetch_add(1, SeqCst);
}

/// A write to stdout/stderr. A report of an unhandled exception is a crash.
pub fn output(fd: i32, b: &[u8]) {
    if b.is_empty() {
        return;
    }
    let mut l = Line::<40>::new();
    stamp(&mut l);
    if fd == 1 {
        let _ = l.write_str("[out] ");
    }
    OUT.put(l.bytes());
    OUT.put(b);
    if b.last() != Some(&b'\n') {
        OUT.put(b"\n");
    }
    // "Unhandled exception: ..."
    let find = |pat: &[u8]| b.windows(pat.len()).any(|w| w == pat);
    if find(b"Unhandled exception: ") {
        // the thread writing this is the one whose exception nobody handled: its stack still
        // has the code that raised it
        thread_state("the thread reporting the unhandled exception");
        let s = String::from_utf8_lossy(&b[..b.len().min(250)]);
        crash(s.trim_end());
    }
}

/// The calling guest thread's state (at its current syscall), deep, in EV.
fn thread_state(why: &str) {
    let Some(t) = CUR.with(|c| c.get()) else {
        return;
    };
    let c = t.cpu.load(Relaxed);
    if c == 0 {
        return;
    }
    let cpu = unsafe { &*(c as *const Cpu) };
    let mut l = Line::<16384>::new();
    stamp(&mut l);
    let mods = &mut [Module {
        base: 0,
        size: 0,
        name: [0; 40],
        len: 0,
    }; MAX_MODULES];
    let n = modules(cpu, mods);
    let mods = &mods[..n];
    let _ = write!(
        l,
        "{why}: at its syscall from {:08x}",
        rd32(cpu.regs[4]).unwrap_or(0)
    );
    sym(&mut l, mods, rd32(cpu.regs[4]).unwrap_or(0));
    regs(&mut l, cpu);
    detail(&mut l, cpu, mods, true, DEEP);
    module_list(&mut l, mods, false);
    let _ = l.write_str("\n");
    EV.put(l.bytes());
}

// ---------------------------------------------------------------------------------------------
// guest threads

struct ThreadLog {
    tid: i32,
    cpu: AtomicUsize,
    /// 1 while it runs guest code, 0 in sake (a syscall, an API call)
    guest: AtomicU32,
    /// (trap address, EAX, [ESP], [ESP+4]) of its last syscalls / API calls
    traps: UnsafeCell<[[u32; 4]; TRAPS]>,
    n: AtomicU32,
    exceptions: AtomicU32,
}

unsafe impl Sync for ThreadLog {}

static THREADS: Mutex<Vec<&'static ThreadLog>> = Mutex::new(Vec::new());

thread_local! {
    static CUR: Cell<Option<&'static ThreadLog>> = const { Cell::new(None) };
}

/// The calling thread got `cpu` (cpu::create).
pub fn register(cpu: *mut Cpu) {
    static HOOK: std::sync::Once = std::sync::Once::new();
    HOOK.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let mut l = Line::<512>::new();
            let _ = write!(l, "panic: {info}");
            crash(std::str::from_utf8(l.bytes()).unwrap_or("panic"));
            prev(info);
        }));
    });
    let t: &'static ThreadLog = Box::leak(Box::new(ThreadLog {
        tid: crate::cpu::tid(),
        cpu: AtomicUsize::new(cpu as usize),
        guest: AtomicU32::new(0),
        traps: UnsafeCell::new([[0; 4]; TRAPS]),
        n: AtomicU32::new(0),
        exceptions: AtomicU32::new(0),
    }));
    CUR.with(|c| c.set(Some(t)));
    let mut g = THREADS.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|x| x.cpu.load(Relaxed) != cpu as usize);
    g.push(t);
}

/// `cpu` is going away (cpu::destroy).
pub fn unregister(cpu: *mut Cpu) {
    let mut g = THREADS.lock().unwrap_or_else(|e| e.into_inner());
    g.retain(|x| {
        let keep = x.cpu.load(Relaxed) != cpu as usize;
        if !keep {
            x.cpu.store(0, Relaxed);
        }
        keep
    });
    CUR.with(|c| {
        if c.get().is_some_and(|t| t.cpu.load(Relaxed) == 0) {
            c.set(None)
        }
    });
}

/// The calling thread enters (true) or leaves guest code.
#[inline]
pub fn in_guest(on: bool) {
    CUR.with(|c| {
        if let Some(t) = c.get() {
            t.guest.store(on as u32, Relaxed);
        }
    });
}

/// The guest got to a trap address (a syscall, a unix call, a sake API).
#[inline]
pub fn trap(cpu: &Cpu, addr: u32) {
    CUR.with(|c| {
        let Some(t) = c.get() else { return };
        let esp = cpu.regs[4];
        let n = t.n.load(Relaxed);
        unsafe {
            (*t.traps.get())[n as usize % TRAPS] = [
                addr,
                cpu.regs[0],
                rd32(esp).unwrap_or(0),
                rd32(esp.wrapping_add(4)).unwrap_or(0),
            ]
        };
        t.n.store(n.wrapping_add(1), Relaxed);
    });
}

// ---------------------------------------------------------------------------------------------
// guest memory, modules, symbols

fn mem_end() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::memory_size(0) as u64 * 65536
    }
    #[cfg(not(target_family = "wasm"))]
    {
        0
    }
}

fn readable(a: u32, n: u32) -> bool {
    a >= 0x10000 && a as u64 + n as u64 <= mem_end().min(koji::jit::GUEST_HI as u64)
}

fn rd32(a: u32) -> Option<u32> {
    readable(a, 4)
        .then(|| unsafe { std::ptr::read_volatile(a as usize as *const [u8; 4]) })
        .map(u32::from_le_bytes)
}

fn rd16(a: u32) -> Option<u16> {
    readable(a, 2)
        .then(|| unsafe { std::ptr::read_volatile(a as usize as *const [u8; 2]) })
        .map(u16::from_le_bytes)
}

fn rd8(a: u32) -> Option<u8> {
    readable(a, 1).then(|| unsafe { std::ptr::read_volatile(a as usize as *const u8) })
}

#[derive(Clone, Copy)]
struct Module {
    base: u32,
    size: u32,
    name: [u8; 40],
    len: u8,
}

impl Module {
    fn name(&self) -> &str {
        std::str::from_utf8(&self.name[..self.len as usize]).unwrap_or("?")
    }
}

const MAX_MODULES: usize = 128;

/// Modules sake loaded (it keeps no loader list in the PEB).
static EXTRA: Mutex<Vec<Module>> = Mutex::new(Vec::new());

fn make_module(base: u32, size: u32, name: impl Iterator<Item = u8>) -> Module {
    let mut m = Module {
        base,
        size,
        name: [0; 40],
        len: 0,
    };
    for c in name.take(40) {
        m.name[m.len as usize] = c;
        m.len += 1;
    }
    m
}

/// sake: an image it mapped at `base` from `path`.
pub fn add_module(base: u32, path: &str) {
    let size = rd32(
        base.wrapping_add(rd32(base + 0x3c).unwrap_or(0))
            .wrapping_add(0x50),
    )
    .unwrap_or(0x1000);
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let m = make_module(base, size, name.bytes().filter(|c| c.is_ascii_graphic()));
    let mut g = EXTRA.lock().unwrap_or_else(|e| e.into_inner());
    if g.len() < MAX_MODULES {
        g.push(m);
    }
}

/// The loaded modules of the process `cpu` runs in: its PEB's loader list.
fn modules(cpu: &Cpu, out: &mut [Module; MAX_MODULES]) -> usize {
    let long = cpu.mode64 != 0;
    let (teb, peb_at, ldr_at, head_at, base_at, size_at, name_at, buf_at) = if long {
        (cpu.gs_base, 0x60, 0x18, 0x10, 0x30, 0x40, 0x58, 0x60)
    } else {
        (cpu.fs_base, 0x30, 0x0c, 0x0c, 0x18, 0x20, 0x2c, 0x30)
    };
    let mut n = 0;
    (|| -> Option<()> {
        let peb = rd32(teb.wrapping_add(peb_at))?;
        let ldr = rd32(peb.wrapping_add(ldr_at))?;
        let head = ldr.wrapping_add(head_at);
        let mut e = rd32(head)?;
        while e != head && n < MAX_MODULES {
            let len = rd16(e.wrapping_add(name_at))? as u32 / 2;
            let buf = rd32(e.wrapping_add(buf_at))?;
            let name = (0..len.min(40))
                .map(|i| rd16(buf.wrapping_add(2 * i)).unwrap_or(b'?' as u16))
                .map(|c| if c < 0x80 && c >= 0x20 { c as u8 } else { b'?' });
            out[n] = make_module(
                rd32(e.wrapping_add(base_at))?,
                rd32(e.wrapping_add(size_at))?,
                name,
            );
            n += 1;
            e = rd32(e)?;
        }
        Some(())
    })();
    if n == 0 {
        if let Ok(g) = EXTRA.try_lock() {
            for m in g.iter().take(MAX_MODULES) {
                out[n] = *m;
                n += 1;
            }
        }
    }
    n
}

/// The export of `m` nearest below `addr`: (its name's address, offset).
fn export_near(m: &Module, addr: u32) -> Option<(u32, u32)> {
    let base = m.base;
    let rva = addr - base;
    let pe = base.wrapping_add(rd32(base + 0x3c)?);
    if rd32(pe)? != 0x4550 {
        return None;
    }
    let oh = pe + 24;
    let dir = if rd16(oh)? == 0x20b {
        oh + 112
    } else {
        oh + 96
    };
    let exp = rd32(dir)?;
    if exp == 0 || exp >= m.size {
        return None;
    }
    let ed = base + exp;
    let names = rd32(ed + 0x18)?.min(20000);
    let (funcs, name_tab, ords) = (
        base + rd32(ed + 0x1c)?,
        base + rd32(ed + 0x20)?,
        base + rd32(ed + 0x24)?,
    );
    let mut best: Option<(u32, u32)> = None;
    for i in 0..names {
        let ord = rd16(ords + 2 * i)? as u32;
        let f = rd32(funcs + 4 * ord)?;
        if f <= rva && best.is_none_or(|(b, _)| f > b) {
            best = Some((f, rd32(name_tab + 4 * i)?));
        }
    }
    best.map(|(f, name)| (base + name, rva - f))
}

/// `addr` as "module+rva (export+off)", or nothing outside the modules.
fn sym(w: &mut impl Write, mods: &[Module], addr: u32) {
    let Some(m) = mods
        .iter()
        .find(|m| addr >= m.base && addr - m.base < m.size)
    else {
        return;
    };
    let _ = write!(w, " {}+{:x}", m.name(), addr - m.base);
    if let Some((name, off)) = export_near(m, addr) {
        let _ = w.write_str(" (");
        for i in 0..64 {
            match rd8(name + i) {
                Some(c) if c >= 0x20 && c < 0x80 => {
                    let _ = w.write_char(c as char);
                }
                _ => break,
            }
        }
        let _ = write!(w, "+{off:x})");
    }
}

/// sake: the API at a trap address (crates/sake api::name_of).
static NAMER: std::sync::OnceLock<fn(u32) -> Option<String>> = std::sync::OnceLock::new();

pub fn set_trap_namer(f: fn(u32) -> Option<String>) {
    let _ = NAMER.set(f);
}

// ---------------------------------------------------------------------------------------------
// records

fn regs(w: &mut impl Write, cpu: &Cpu) {
    let r = &cpu.regs;
    if cpu.mode64 != 0 {
        const N: [&str; 16] = [
            "rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11",
            "r12", "r13", "r14", "r15",
        ];
        for (i, n) in N.iter().enumerate() {
            let _ = write!(
                w,
                "{}{n} {:016x}",
                if i % 4 == 0 { "\n  " } else { " " },
                cpu.gpr(i)
            );
        }
        let _ = write!(
            w,
            "\n  rip {:08x}{:08x} rflags {:08x} gs {:08x} fs {:08x}",
            cpu.rip_hi,
            cpu.eip,
            cpu.flags(),
            cpu.gs_base,
            cpu.fs_base
        );
    } else {
        let _ = write!(
            w,
            "\n  eax {:08x} ecx {:08x} edx {:08x} ebx {:08x} esp {:08x} ebp {:08x} esi {:08x} edi {:08x}\n  eip {:08x} eflags {:08x} fs {:08x}",
            r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], cpu.eip, cpu.flags(), cpu.fs_base
        );
    }
}

/// How far `detail` looks: stack bytes scanned for code addresses, how many it lists, frames.
#[derive(Clone, Copy)]
struct Depth {
    scan: u32,
    found: u32,
    frames: u32,
}

const NORMAL: Depth = Depth {
    scan: 0x800,
    found: 24,
    frames: 16,
};
/// The thread that reports an unhandled exception: its stack holds the dispatcher's frames
/// (records, contexts) above the code that raised it.
const DEEP: Depth = Depth {
    scan: 0x6000,
    found: 64,
    frames: 40,
};

fn detail(w: &mut impl Write, cpu: &Cpu, mods: &[Module], own_thread: bool, depth: Depth) {
    let long = cpu.mode64 != 0;
    let f = &cpu.fpu;
    let _ = write!(
        w,
        "\n  fpu cw {:04x} sw {:04x} top {} empty {:02x} mxcsr {:08x}",
        f.cw, f.sw, f.top, f.empty, cpu.mxcsr
    );
    for (i, x) in cpu.xmm.iter().enumerate() {
        if *x != 0 {
            let _ = write!(w, " xmm{i} {x:032x}");
        }
    }
    if long {
        for (i, x) in cpu.xmm8.iter().enumerate() {
            if *x != 0 {
                let _ = write!(w, " xmm{} {x:032x}", i + 8);
            }
        }
    }
    let _ = write!(w, "\n  code @{:08x}:", cpu.eip);
    for i in 0..16 {
        match rd8(cpu.eip.wrapping_add(i)) {
            Some(b) => {
                let _ = write!(w, " {b:02x}");
            }
            None => {
                let _ = w.write_str(" ??");
            }
        }
    }
    let esp = cpu.regs[4];
    let _ = write!(w, "\n  stack @{esp:08x}:");
    for i in 0..32u32 {
        if i % 8 == 0 {
            let _ = w.write_str("\n   ");
        }
        match rd32(esp.wrapping_add(4 * i)) {
            Some(v) => {
                let _ = write!(w, " {v:08x}");
            }
            None => {
                let _ = w.write_str(" ????????");
            }
        }
    }
    // the frame chain (32-bit code mostly keeps EBP frames)
    if !long {
        let _ = w.write_str("\n  frames (ebp chain):");
        let mut ebp = cpu.regs[5];
        for _ in 0..depth.frames {
            let (Some(ret), Some(next)) = (rd32(ebp.wrapping_add(4)), rd32(ebp)) else {
                break;
            };
            let _ = write!(w, "\n    {ret:08x}");
            sym(w, mods, ret);
            if next <= ebp {
                break;
            }
            ebp = next;
        }
    }
    // stack words that point into a module: the return addresses (and some function pointers)
    let _ = w.write_str("\n  code addresses on the stack:");
    let mut found = 0;
    let step = if long { 8 } else { 4 };
    let mut a = esp;
    while a < esp.wrapping_add(depth.scan) && found < depth.found {
        let Some(v) = rd32(a) else { break };
        if long && rd32(a + 4).unwrap_or(1) > 0xffff {
            a += step;
            continue;
        }
        if mods
            .iter()
            .any(|m| v >= m.base && v - m.base < m.size && v - m.base >= 0x1000)
        {
            let _ = write!(w, "\n    [{:x}] {v:08x}", a - esp);
            sym(w, mods, v);
            found += 1;
        }
        a += step;
    }
    // the last syscalls / API calls
    let t = CUR.with(|c| c.get()).filter(|_| own_thread);
    if let Some(t) = t.or_else(|| find_thread(cpu)) {
        traps(w, t, mods, own_thread, 24);
    }
    // the JIT: its recent jumps between regions (from, to), newest first
    if cpu.jit_ring != 0 {
        let ring = cpu.jit_ring;
        if let Some(n) = rd32(ring) {
            let _ = w.write_str("\n  jit region jumps (newest first):");
            let size = koji::jit::RING_SIZE;
            for k in 0..n.min(size).min(16) {
                let i = (n.wrapping_sub(1 + k)) & (size - 1);
                let (a, b) = (
                    rd32(ring + 4 + 8 * i).unwrap_or(0),
                    rd32(ring + 8 + 8 * i).unwrap_or(0),
                );
                let _ = write!(w, " {a:x}>{b:x}");
            }
        }
    }
    let c = crate::jit::compiled_totals();
    let _ = write!(
        w,
        "\n  jit: on {} (x64 {}), regions built {}, functions {}, rebuilt {}, call depth {}",
        crate::jit::enabled(),
        crate::jit::enabled64(),
        c[0],
        c[1],
        c[2],
        cpu.jit_depth
    );
}

fn find_thread(cpu: &Cpu) -> Option<&'static ThreadLog> {
    let g = THREADS.try_lock().ok()?;
    g.iter()
        .find(|t| t.cpu.load(Relaxed) == cpu as *const Cpu as usize)
        .copied()
}

fn traps(w: &mut impl Write, t: &ThreadLog, mods: &[Module], name: bool, max: u32) {
    let n = t.n.load(Relaxed);
    let _ = write!(
        w,
        "\n  last syscalls/API calls (newest first, {n} in all): trap eax [esp] [esp+4]"
    );
    // (a call repeated, or two alternating — a loop's — is shown once with a count)
    let mut shown: [[u32; 4]; 2] = [[0; 4]; 2];
    let mut repeats = 0;
    let mut lines = 0;
    for k in 0..n.min(TRAPS as u32) {
        let e = unsafe { (*t.traps.get())[(n.wrapping_sub(1 + k)) as usize % TRAPS] };
        if lines > 0 && (e == shown[0] || e == shown[1]) {
            repeats += 1;
            continue;
        }
        if repeats > 0 {
            let _ = write!(w, "\n    (repeated {repeats} more times)");
            repeats = 0;
        }
        if lines == max {
            break;
        }
        shown = [e, shown[0]];
        lines += 1;
        let [addr, eax, r0, r1] = e;
        let _ = write!(w, "\n    {addr:08x} {eax:08x}");
        if name {
            if let Some(s) = NAMER.get().and_then(|f| f(addr)) {
                let _ = write!(w, " {s}");
            }
        }
        let _ = write!(w, " | {r0:08x}");
        sym(w, mods, r0);
        let _ = write!(w, " | {r1:08x}");
        sym(w, mods, r1);
    }
    if repeats > 0 {
        let _ = write!(w, "\n    (repeated {repeats} more times)");
    }
}

/// A hash of the modules last listed: a record lists them again only when they changed.
static LISTED: Mutex<u32> = Mutex::new(0);

fn module_list(w: &mut impl Write, mods: &[Module], always: bool) {
    let h = mods.iter().fold(0x811c9dc5u32, |h, m| {
        (h ^ m.base ^ m.size).wrapping_mul(0x0100_0193)
    });
    if !always {
        let mut g = LISTED.lock().unwrap_or_else(|e| e.into_inner());
        if *g == h {
            let _ = write!(w, "\n  modules: as listed before ({})", mods.len());
            return;
        }
        *g = h;
    }
    let _ = write!(w, "\n  modules ({}):", mods.len());
    for (i, m) in mods.iter().enumerate() {
        let _ = write!(
            w,
            "{}{:08x}-{:08x} {}",
            if i % 3 == 0 { "\n   " } else { "  " },
            m.base,
            m.base.wrapping_add(m.size),
            m.name()
        );
    }
}

/// A guest exception as the x86 core stops on it (before sake's SEH sees it).
/// `what` names it, `access` is (address, write) for access violations.
pub fn exception(cpu: &Cpu, what: &str, access: Option<(u32, bool)>, from_jit: bool) {
    let t = CUR.with(|c| c.get());
    let k = t.map_or(0, |t| t.exceptions.fetch_add(1, Relaxed));
    let mut l = Line::<12288>::new();
    stamp(&mut l);
    let mods = &mut [Module {
        base: 0,
        size: 0,
        name: [0; 40],
        len: 0,
    }; MAX_MODULES];
    let n = modules(cpu, mods);
    let mods = &mods[..n];
    let _ = write!(l, "exception #{} {what} at {:08x}", k + 1, cpu.eip);
    sym(&mut l, mods, cpu.eip);
    let _ = write!(l, " [{}]", if from_jit { "jit" } else { "interpreter" });
    if let Some((a, wr)) = access {
        let _ = write!(l, " {} {a:08x}", if wr { "writing" } else { "reading" });
    }
    // all of the first ones of a thread, then one in 100 (programs that use exceptions for
    // control flow)
    if k < 16 || k % 100 == 0 {
        regs(&mut l, cpu);
        detail(&mut l, cpu, mods, true, NORMAL);
        module_list(&mut l, mods, false);
    }
    let _ = l.write_str("\n");
    EV.put(l.bytes());
}

/// Writes every guest thread's state into SNAP; returns its length (0 if busy).
#[no_mangle]
pub extern "C" fn vnhost_crashlog_snapshot() -> u32 {
    if SNAP_BUSY.swap(true, Acquire) {
        return 0;
    }
    let mut w = SnapWriter { n: 0 };
    stamp(&mut w);
    // (a fixed list: another thread may have died holding the allocator's lock)
    let mut list: [Option<&'static ThreadLog>; 256] = [None; 256];
    let mut count = 0;
    match THREADS.try_lock() {
        Ok(g) => {
            for (s, t) in list.iter_mut().zip(g.iter()) {
                *s = Some(*t);
                count += 1;
            }
        }
        Err(_) => {
            let _ = w.write_str("(thread list busy) ");
        }
    }
    let _ = write!(w, "threads now: {count}");
    let mut last_teb = u32::MAX;
    for t in list.iter().flatten() {
        let c = t.cpu.load(Relaxed);
        if c == 0 {
            continue;
        }
        let cpu = unsafe { &*(c as *const Cpu) };
        let mods = &mut [Module {
            base: 0,
            size: 0,
            name: [0; 40],
            len: 0,
        }; MAX_MODULES];
        let n = modules(cpu, mods);
        let mods = &mods[..n];
        let _ = write!(
            w,
            "\n\nthread t{} {} at {:08x}",
            t.tid,
            if t.guest.load(Relaxed) != 0 {
                "in guest code"
            } else {
                "in a syscall/API (host side)"
            },
            cpu.eip
        );
        sym(&mut w, mods, cpu.eip);
        let _ = write!(w, ", exceptions {}", t.exceptions.load(Relaxed));
        regs(&mut w, cpu);
        detail(&mut w, cpu, mods, false, NORMAL);
        let peb = if cpu.mode64 != 0 {
            rd32(cpu.gs_base + 0x60)
        } else {
            rd32(cpu.fs_base + 0x30)
        }
        .unwrap_or(0);
        if peb != last_teb {
            last_teb = peb;
            module_list(&mut w, mods, true);
        }
    }
    let _ = w.write_str("\n");
    SNAP_LEN.store(w.n as u32, Release);
    SNAP_BUSY.store(false, Release);
    w.n as u32
}

struct SnapWriter {
    n: usize,
}

impl Write for SnapWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let b = unsafe { &mut *SNAP.0.get() };
        let k = s.len().min(SNAP_SIZE - self.n);
        b[self.n..self.n + k].copy_from_slice(&s.as_bytes()[..k]);
        self.n += k;
        Ok(())
    }
}
