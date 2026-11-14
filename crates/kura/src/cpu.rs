//! The x86 run loop sake drives (crates/sake/src/lib.rs).
//!
//! Guest and host share the linear memory, so a guest address is used as a pointer as is.
//! The first 64 KiB stay unmapped for the guest so NULL dereferences raise access violations.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use koji::{Cpu, Exit, Fault, Memory};

const NULL_PAGES: u32 = 0x10000;

/// Why [`run`] returned.
#[derive(Clone, Copy, Debug)]
pub enum Stop {
    /// Reached a trap address (in [trap_lo, trap_hi)).
    Trap(u32),
    Fault { addr: u32, write: bool },
    Divide,
    /// An invalid opcode at EIP.
    Invalid,
    /// INT n
    Int(u32),
    Halt,
    /// SYSCALL in long mode (x86-64 guests)
    Syscall,
    /// The single-step trap after an instruction run with TF set; EIP is past it.
    SingleStep,
    /// An unmasked SSE floating-point exception (#XM); EIP at the instruction, the MXCSR
    /// flags set.
    SimdException,
    /// A save state is being taken and the thread is at the top level of its run loop
    /// (crates/kura/src/snap.rs): EIP at the next instruction, nothing done.
    Pause,
}

/// The linear memory as the guest sees it.
pub(crate) struct Linear;

#[inline(always)]
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

/// The guest's half of the linear memory ends here: sake's own statics, heap and stacks are
/// above it (crates/sake/build.sh GLOBAL_BASE). The guest may not write there, except in the
/// trap range ([`create`]'s: API entry points, which programs patch like code). Reads there
/// stay allowed: on Windows they fault too, but programs that read through a pointer an API
/// left unset (a sake shortcoming) went on with garbage before, and the Wine tests show
/// several that then crash; a read cannot corrupt sake.
const GUEST_END: u64 = koji::jit::GUEST_HI as u64;

/// [`create`]'s trap range, for [`check`]'s slow path.
static TRAP_LO: AtomicU32 = AtomicU32::new(0);
static TRAP_HI: AtomicU32 = AtomicU32::new(0);
/// MASU_UPPER_OPEN: the old rule (writes too, up to the end of the memory), as an off-switch.
static UPPER_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether the guest may access `addr..addr + len` (`end`: the memory's size): below
/// [`GUEST_END`] and above the NULL pages, or inside the trap range `[lo, hi)` (or anywhere up
/// to `end` with `open`, the old rule); never past `end`.
#[inline(always)]
pub(crate) fn allowed(addr: u32, len: u32, end: u64, lo: u32, hi: u32, open: bool) -> bool {
    let top = addr as u64 + len as u64;
    if addr >= NULL_PAGES && top <= GUEST_END {
        return top <= end;
    }
    upper_allowed(addr, top, end, lo, hi, open)
}

#[cold]
#[inline(never)]
fn upper_allowed(addr: u32, top: u64, end: u64, lo: u32, hi: u32, open: bool) -> bool {
    addr >= NULL_PAGES && top <= end && (open || (addr >= lo && top <= hi as u64))
}

#[cold]
#[inline(never)]
fn upper_ok(addr: u32, len: u32, write: bool) -> bool {
    allowed(addr, len, mem_end(), TRAP_LO.load(Ordering::Relaxed), TRAP_HI.load(Ordering::Relaxed), !write || UPPER_OPEN.load(Ordering::Relaxed))
}

/// (once: a guest access to sake's own memory is an access violation; the log says where)
#[cold]
#[inline(never)]
fn refused(addr: u32, len: u32) -> Fault {
    if addr as u64 + len as u64 > GUEST_END && addr >= NULL_PAGES {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| eprintln!("kura: guest write to {addr:#x} (+{len}) is in sake's memory, not the guest's: access violation (MASU_UPPER_OPEN=1 allows it)"));
    }
    Fault { addr, write: false }
}

#[inline(always)]
fn check(addr: u32, len: u32, write: bool) -> Result<*mut u8, Fault> {
    // (the fast path is allowed()'s first test; the statics are read on the slow one only)
    let top = addr as u64 + len as u64;
    if !(addr >= NULL_PAGES && top <= GUEST_END && top <= mem_end()) && !upper_ok(addr, len, write) {
        return Err(Fault { write, ..refused(addr, len) });
    }
    if crate::jit::faults(addr, len, write) {
        return Err(Fault { addr, write });
    }
    if write {
        crate::jit::interp_write(addr);
    }
    Ok(addr as usize as *mut u8)
}

impl Memory for Linear {
    #[inline(always)]
    fn read8(&mut self, addr: u32) -> Result<u8, Fault> {
        Ok(unsafe { *check(addr, 1, false)? })
    }
    #[inline(always)]
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), Fault> {
        unsafe { *check(addr, 1, true)? = v };
        Ok(())
    }
    #[inline(always)]
    fn read16(&mut self, addr: u32) -> Result<u16, Fault> {
        Ok(unsafe { (check(addr, 2, false)? as *const u16).read_unaligned() })
    }
    #[inline(always)]
    fn read32(&mut self, addr: u32) -> Result<u32, Fault> {
        Ok(unsafe { (check(addr, 4, false)? as *const u32).read_unaligned() })
    }
    #[inline(always)]
    fn read64(&mut self, addr: u32) -> Result<u64, Fault> {
        Ok(unsafe { (check(addr, 8, false)? as *const u64).read_unaligned() })
    }
    #[inline(always)]
    fn write16(&mut self, addr: u32, v: u16) -> Result<(), Fault> {
        unsafe { (check(addr, 2, true)? as *mut u16).write_unaligned(v) };
        Ok(())
    }
    #[inline(always)]
    fn write32(&mut self, addr: u32, v: u32) -> Result<(), Fault> {
        unsafe { (check(addr, 4, true)? as *mut u32).write_unaligned(v) };
        Ok(())
    }
    #[inline(always)]
    fn write64(&mut self, addr: u32, v: u64) -> Result<(), Fault> {
        unsafe { (check(addr, 8, true)? as *mut u64).write_unaligned(v) };
        Ok(())
    }
    fn cmpxchg(&mut self, addr: u32, bytes: u32, old: u64, new: u64) -> Result<u64, Fault> {
        use std::sync::atomic::{AtomicU16, AtomicU64, AtomicU8};
        let p = check(addr, bytes, true)?;
        // an unaligned operand goes through the aligned 8 bytes holding it, or a lock when
        // it straddles two of those (rare; atomic only against other locked accesses)
        let word = addr & !7;
        if addr % bytes != 0 && (addr + bytes - 1) & !7 != word {
            static SPLIT: Mutex<()> = Mutex::new(());
            let _g = SPLIT.lock().unwrap_or_else(|e| e.into_inner());
            let mut cur = [0u8; 8];
            unsafe { std::ptr::copy_nonoverlapping(p, cur.as_mut_ptr(), bytes as usize) };
            let cur = u64::from_le_bytes(cur);
            if cur == old {
                unsafe { std::ptr::copy_nonoverlapping(new.to_le_bytes().as_ptr(), p, bytes as usize) };
            }
            return Ok(cur);
        }
        let ord = Ordering::SeqCst;
        Ok(unsafe {
            match bytes {
                1 => (*(p as *const AtomicU8)).compare_exchange(old as u8, new as u8, ord, ord).unwrap_or_else(|v| v) as u64,
                2 if addr % 2 == 0 => (*(p as *const AtomicU16)).compare_exchange(old as u16, new as u16, ord, ord).unwrap_or_else(|v| v) as u64,
                4 if addr % 4 == 0 => (*(p as *const AtomicU32)).compare_exchange(old as u32, new as u32, ord, ord).unwrap_or_else(|v| v) as u64,
                8 => (*(p as *const AtomicU64)).compare_exchange(old, new, ord, ord).unwrap_or_else(|v| v),
                _ => {
                    // inside one aligned u64: swap the whole word
                    let w = &*(word as usize as *const AtomicU64);
                    let sh = (addr - word) * 8;
                    let m = (u64::MAX >> (64 - bytes * 8)) << sh;
                    let mut cur = w.load(ord);
                    loop {
                        let v = (cur & m) >> sh;
                        if v != old {
                            break v;
                        }
                        match w.compare_exchange(cur, (cur & !m) | ((new << sh) & m), ord, ord) {
                            Ok(_) => break v,
                            Err(c) => cur = c,
                        }
                    }
                }
            }
        })
    }
    fn cmpxchg16(&mut self, addr: u32, old: u128, new: u128) -> Result<u128, Fault> {
        use std::sync::atomic::AtomicU64;
        // no 128-bit atomics in wasm: all 16-byte swaps take one lock, and each half is stored
        // atomically, so a plain reader sees a torn pair at worst (and its own swap then fails)
        static WIDE: Mutex<()> = Mutex::new(());
        let p = check(addr, 16, true)?;
        let _g = WIDE.lock().unwrap_or_else(|e| e.into_inner());
        let ord = Ordering::SeqCst;
        unsafe {
            if p as usize % 8 == 0 {
                let (lo, hi) = (&*(p as *const AtomicU64), &*(p.add(8) as *const AtomicU64));
                let cur = lo.load(ord) as u128 | (hi.load(ord) as u128) << 64;
                if cur == old {
                    lo.store(new as u64, ord);
                    hi.store((new >> 64) as u64, ord);
                }
                Ok(cur)
            } else {
                let cur = (p as *const u128).read_unaligned();
                if cur == old {
                    (p as *mut u128).write_unaligned(new);
                }
                Ok(cur)
            }
        }
    }
    fn host_range(&mut self, addr: u32, len: u32) -> Option<*mut u8> {
        check(addr, len, true).ok()
    }
    fn fetch(&mut self, addr: u32, buf: &mut [u8]) -> usize {
        // (code runs from the guest's half only: the trap range is caught before a fetch)
        let end = if UPPER_OPEN.load(Ordering::Relaxed) { mem_end() } else { mem_end().min(GUEST_END) };
        if addr < NULL_PAGES || addr as u64 >= end {
            return 0;
        }
        let n = (buf.len() as u64).min(end - addr as u64) as usize;
        unsafe { std::ptr::copy_nonoverlapping(addr as usize as *const u8, buf.as_mut_ptr(), n) };
        n
    }
}

// ---------------------------------------------------------------------------------------------
// thread ids

static NEXT_TID: AtomicU32 = AtomicU32::new(1000);

thread_local! {
    static TID: i32 = NEXT_TID.fetch_add(1, Ordering::Relaxed) as i32;
}

/// The calling thread's id (for logs).
pub fn tid() -> i32 {
    TID.with(|t| *t)
}

// ---------------------------------------------------------------------------------------------
// guest threads' CPUs

/// A new CPU (kept until [`destroy`]: the crash log reads it).
pub fn create(trap_lo: u32, trap_hi: u32) -> *mut Cpu {
    // MASU_CPUID_AVX: CPUID reports AVX/AVX2/FMA3/BMI (koji::CPUID_AVX)
    koji::CPUID_AVX.store(crate::env_on!("MASU_CPUID_AVX"), Ordering::Relaxed);
    // MASU_X87_80: the x87 in its 80-bit format (koji::fpu::X87_80: interpreted, slower)
    koji::fpu::X87_80.store(crate::env_on!("MASU_X87_80"), Ordering::Relaxed);
    UPPER_OPEN.store(crate::env_on!("MASU_UPPER_OPEN"), Ordering::Relaxed);
    TRAP_LO.store(trap_lo, Ordering::Relaxed);
    TRAP_HI.store(trap_hi, Ordering::Relaxed);
    let mut cpu = Box::new(Cpu::default());
    cpu.trap_lo = trap_lo;
    cpu.trap_hi = trap_hi;
    cpu.fpu.cw = 0x27f;
    let cpu = Box::into_raw(cpu);
    crate::crashlog::register(cpu);
    cpu
}

/// # Safety
/// `cpu` came from [`create`] and is not used after this.
pub unsafe fn destroy(cpu: *mut Cpu) {
    crate::crashlog::unregister(cpu);
    drop(Box::from_raw(cpu));
}

/// What an [`Exit`] means for the caller of [`run`]; None to keep running.
fn exit_code(cpu: &Cpu, mem: &mut Linear, exit: Exit, from_jit: bool) -> Option<Stop> {
    use crate::crashlog::exception;
    match exit {
        Exit::Continue => None,
        Exit::Trap(a) => {
            crate::crashlog::trap(cpu, a);
            Some(Stop::Trap(a))
        }
        Exit::Fault(f) => {
            exception(cpu, "access violation", Some((f.addr, f.write)), from_jit);
            // MASU_TRACE_FAULT: where the interpreter (or a region's step) faulted
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *ON.get_or_init(|| std::env::var_os("MASU_TRACE_FAULT").is_some()) {
                let r = &cpu.regs;
                let ok = (koji::jit::GUEST_LO..koji::jit::GUEST_HI - 8).contains(&cpu.eip);
                let code: Vec<String> = (0..if ok { 8 } else { 0 }).map(|i| unsafe { *((cpu.eip as usize + i) as *const u8) }).map(|b| format!("{b:02x}")).collect();
                eprintln!(
                    "fault {} {:08x} at eip {:08x} [{}] | eax {:08x} ecx {:08x} edx {:08x} ebx {:08x} esp {:08x} ebp {:08x} esi {:08x} edi {:08x} eflags {:08x}",
                    if f.write { "write" } else { "read" }, f.addr, cpu.eip, code.join(" "), r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], cpu.eflags
                );
            }
            // MASU_DUMP_FAULT=<eip, hex>: 64-bit registers and the memory they point at when a
            // fault happens there
            // ("null": any fault below 64K)
            static AT: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
            let at = *AT.get_or_init(|| {
                std::env::var("MASU_DUMP_FAULT").ok().and_then(|v| if v == "null" { Some(0) } else { u32::from_str_radix(v.trim_start_matches("0x"), 16).ok() })
            });
            if at == Some(cpu.eip) || (at == Some(0) && (f.addr < 0x10000 || f.addr >= 0x8000_0000)) {
                eprintln!("vnfault at {:x}:{:08x} addr {:x} write {}", cpu.rip_hi, cpu.eip, f.addr, f.write);
                let names = ["rax", "rcx", "rdx", "rbx", "rsp", "rbp", "rsi", "rdi", "r8", "r9", "r10", "r11", "r12", "r13", "r14", "r15"];
                for (i, n) in names.iter().enumerate() {
                    let v = cpu.gpr(i);
                    let a = v as u32;
                    let mut line = format!("vnfault {n} {v:016x}");
                    if (koji::jit::GUEST_LO..koji::jit::GUEST_HI - 0x80).contains(&a) {
                        for k in 0..12u32 {
                            let w = mem.read64(a.wrapping_add(k * 8)).unwrap_or(0xdead);
                            line += &format!(" {w:x}");
                        }
                    }
                    eprintln!("{line}");
                }
                let rsp = cpu.gpr(4) as u32;
                let st: Vec<String> = (0..48u32).map(|k| format!("{:x}", mem.read64(rsp.wrapping_add(k * 8)).unwrap_or(0xdead))).collect();
                eprintln!("vnfault stack {}", st.join(" "));
            }
            Some(Stop::Fault { addr: f.addr, write: f.write })
        }
        Exit::DivideError => {
            exception(cpu, "divide error", None, from_jit);
            Some(Stop::Divide)
        }
        Exit::InvalidOpcode => {
            exception(cpu, "invalid opcode", None, from_jit);
            if crate::env_on!("MASU_TRACE_UD") {
                let mut b = [0u8; 16];
                let n = mem.fetch(cpu.eip, &mut b);
                eprintln!("kura: invalid opcode at {:#x}: {:02x?}", cpu.eip, &b[..n]);
            }
            Some(Stop::Invalid)
        }
        Exit::Interrupt(n) => {
            let what = match n {
                3 => "int3 (breakpoint)",
                0x29 => "int 29 (__fastfail)",
                0x2d => "int 2d (debug service)",
                _ => "software interrupt",
            };
            let mut l = crate::crashlog::Line::<32>::new();
            let _ = std::fmt::Write::write_fmt(&mut l, format_args!("{what} {n:#x}"));
            exception(cpu, std::str::from_utf8(l.bytes()).unwrap_or(what), None, from_jit);
            Some(Stop::Int(n as u32))
        }
        Exit::Halt => {
            exception(cpu, "hlt (privileged instruction)", None, from_jit);
            Some(Stop::Halt)
        }
        Exit::Syscall => Some(Stop::Syscall),
        Exit::SingleStep => Some(Stop::SingleStep),
        Exit::SimdException => Some(Stop::SimdException),
    }
}

/// MASU_TRACE_EIP=<seconds>: every so often, print where each thread's guest code is (the
/// EIP of its latest exit from the JIT or interpreter step) and whether it is in guest code
/// or in sake (a syscall, an API call). For finding what a hung or spinning program does.
fn trace_eip(cpu: *mut Cpu) -> Option<impl Drop> {
    use std::sync::atomic::AtomicBool;
    struct Entry {
        cpu: usize,
        inside: AtomicBool,
    }
    static ON: std::sync::OnceLock<Option<f64>> = std::sync::OnceLock::new();
    static CPUS: Mutex<Vec<(i32, Arc<Entry>)>> = Mutex::new(Vec::new());
    let secs = (*ON.get_or_init(|| {
        let s = std::env::var("MASU_TRACE_EIP").ok()?.parse().ok()?;
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs_f64(s));
            let list = CPUS.lock().unwrap();
            let line: Vec<String> = list
                .iter()
                .map(|(tid, e)| {
                    let c = e.cpu as *const Cpu;
                    let eip = unsafe { std::ptr::read_volatile(std::ptr::addr_of!((*c).eip)) };
                    let at = if e.inside.load(Ordering::Relaxed) { "" } else { "*" };
                    // and the return addresses: [esp], then the EBP chain
                    let rd = |a: u32| -> Option<u32> {
                        (a >= koji::jit::GUEST_LO && a < koji::jit::GUEST_HI - 4).then(|| unsafe { std::ptr::read_volatile(a as usize as *const u32) })
                    };
                    let (esp, mut ebp) = unsafe { ((*c).regs[4], (*c).regs[5]) };
                    let mut stack = vec![rd(esp).unwrap_or(0)];
                    for _ in 0..8 {
                        let (Some(r), Some(next)) = (rd(ebp.wrapping_add(4)), rd(ebp)) else { break };
                        stack.push(r);
                        if next <= ebp {
                            break;
                        }
                        ebp = next;
                    }
                    let stack: Vec<String> = stack.iter().map(|a| format!("{a:x}")).collect();
                    format!("{tid}:{eip:08x}{at}<{}>", stack.join(","))
                })
                .collect();
            eprintln!("vnvm eip {}", line.join(" "));
        });
        Some(s)
    }))?;
    let _ = secs;
    let tid = tid();
    let mut list = CPUS.lock().unwrap();
    let e = match list.iter().find(|(t, _)| *t == tid) {
        Some((_, e)) => e.clone(),
        None => {
            let e = Arc::new(Entry { cpu: cpu as usize, inside: AtomicBool::new(false) });
            list.push((tid, e.clone()));
            e
        }
    };
    e.inside.store(true, Ordering::Relaxed);
    struct TraceGuard(Arc<Entry>);
    impl Drop for TraceGuard {
        fn drop(&mut self) {
            self.0.inside.store(false, Ordering::Relaxed);
        }
    }
    Some(TraceGuard(e))
}

/// Runs until the guest reaches a trap address or stops otherwise (fault, INT...).
pub fn run(cpu: &mut Cpu) -> Stop {
    let _guest = trace_eip(cpu);
    crate::crashlog::in_guest(true);
    let r = cpu_run(cpu);
    crate::crashlog::in_guest(false);
    r
}

/// Once per process: code ran with an SSE exception unmasked, so the interpreter ran it (a
/// program that keeps it so runs slowly: the logs of corpus runs show which do).
fn unmasked_note(mx: u32) {
    static ONCE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !ONCE.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("kura: MXCSR {mx:#x} unmasks SSE exceptions: the interpreter runs the code meanwhile");
    }
}

thread_local! {
    /// About how many guest instructions this thread ran (the interpreter's steps, and 8 per
    /// block the JIT ran): a measure of its work that a loaded host's descheduling does not
    /// change (sake's uni: a quantum)
    static WORK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// This thread's guest work so far (see WORK)
pub fn work() -> u64 {
    WORK.get()
}

/// Asked at the top of every slice of the run loop: whether the thread stood still (it is
/// suspended by another thread: it returns once resumed, or when a save state wants it parked).
static PARK: std::sync::OnceLock<fn() -> bool> = std::sync::OnceLock::new();

/// sake's SuspendThread: see PARK.
pub fn set_park(f: fn() -> bool) {
    let _ = PARK.set(f);
}

/// Called when the thread has run guest code a while without leaving the run loop (the JIT's
/// budget ran out, or [`INTERP_SLICE`] instructions interpreted): sake's handoff takes it as the
/// thread's progress, as its API calls (a SetEvent's waiter need not wait for a signaler that
/// spins on without calls; waits::handoff).
static PROGRESS: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();

/// sake's handoff: see PROGRESS.
pub fn set_progress(f: fn()) {
    let _ = PROGRESS.set(f);
}

/// Interpreted instructions that count as a slice of progress (more than the JIT's budget's
/// worth of a short loop: a few instructions after a call are not progress)
pub const INTERP_SLICE: u32 = 1 << 16;

fn progressed() {
    if let Some(f) = PROGRESS.get() {
        f();
    }
}

fn cpu_run(cpu: &mut Cpu) -> Stop {
    let mut mem = Linear;
    let mut stepped = 0u32;
    // long mode: MASU_JIT64=0 leaves it to the interpreter
    let jit = crate::jit::enabled() && (cpu.mode64 == 0 || crate::jit::enabled64()) && (cpu.mode16 == 0 || crate::jit::enabled16());
    crate::jit::count_entry(cpu.eip);
    loop {
        if crate::snap::pausing() && crate::snap::parkable() {
            return Stop::Pause;
        }
        // (a thread another one suspended: SuspendThread, sake's suspend.rs)
        if let Some(park) = PARK.get() {
            if park() {
                continue;
            }
        }
        trace_call(cpu);
        // (with TF set every instruction traps, and with an SSE exception unmasked any SSE
        // instruction may: the interpreter runs them one by one; so it does SSE rounding
        // other than to nearest)
        if jit && cpu.mxcsr & 0x1f80 != 0x1f80 {
            unmasked_note(cpu.mxcsr);
        }
        if jit && cpu.eflags & koji::flags::TF == 0 && koji::jit::sse_ok(cpu.mxcsr) {
            // (16-bit code: the JIT's eip is the linear CS:IP, see jit::to_linear16)
            let m16 = cpu.mode16 != 0;
            if m16 {
                crate::jit::to_linear16(cpu);
            }
            let ran = crate::jit::run(cpu);
            WORK.set(WORK.get() + 8 * (crate::jit::BUDGET - cpu.jit_budget.max(0)).max(0) as u64);
            let compiled = matches!(ran, crate::jit::Ran::Missed) && {
                crate::jit::count_miss(cpu.eip);
                crate::jit::maybe_compile(cpu, &mut mem)
            };
            if m16 {
                crate::jit::from_linear16(cpu);
            }
            match ran {
                crate::jit::Ran::Stopped => {
                    crate::jit::count_entry(cpu.eip | 0x8000_0000);
                    progressed();
                    continue;
                }
                crate::jit::Ran::Exit(e) => match exit_code(cpu, &mut mem, e, true) {
                    Some(code) => return code,
                    None => continue,
                },
                crate::jit::Ran::Missed => {
                    if compiled {
                        continue;
                    }
                }
            }
        }
        // the interpreter: up to the next branch with the JIT, a batch of steps without
        for _ in 0..4096 {
            WORK.set(WORK.get() + 1);
            let (e, branch) = crate::jit::step(cpu, &mut mem);
            stepped += 1;
            if stepped >= INTERP_SLICE {
                stepped = 0;
                progressed();
            }
            if let Some(code) = exit_code(cpu, &mut mem, e, false) {
                return code;
            }
            if jit && branch {
                break;
            }
            trace_call(cpu);
        }
    }
}

/// MASU_TRACE_CALL=<hex eip>: each time a thread gets to that address, print EAX ECX EDX ESI
/// EDI ESP and a hash of the EDX..EDX+ECX bytes of its previous arrival (a register-call
/// memcpy's destination: compare JIT and interpreter runs to find the first copy that differs).
fn trace_call(cpu: &Cpu) {
    static AT: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    let Some(at) = *AT.get_or_init(|| u32::from_str_radix(&std::env::var("MASU_TRACE_CALL").ok()?, 16).ok()) else { return };
    if cpu.eip != at {
        return;
    }
    thread_local!(static PREV: std::cell::Cell<(u32, u32, u32)> = const { std::cell::Cell::new((0, 0, 0)) });
    let (k, d, n) = PREV.with(|p| p.get());
    let ok = |a: u32| (koji::jit::GUEST_LO..koji::jit::GUEST_HI).contains(&a);
    let mut h = 0x811c9dc5u32;
    if n < 1 << 20 && ok(d) && ok(d.wrapping_add(n)) {
        for i in 0..n {
            h = (h ^ unsafe { *((d + i) as usize as *const u8) } as u32).wrapping_mul(0x0100_0193);
        }
    }
    // and the code there (does something overwrite it?)
    let mut c = 0x811c9dc5u32;
    for i in 0..0x40 {
        c = (c ^ unsafe { *((at + i) as usize as *const u8) } as u32).wrapping_mul(0x0100_0193);
    }
    let r = &cpu.regs;
    let ret = if ok(r[4]) { unsafe { *(r[4] as usize as *const u32) } } else { 0 };
    eprintln!("call {k} prev-dst-hash {h:08x} code {c:08x} | eax {:08x} ecx {:08x} edx {:08x} esi {:08x} edi {:08x} esp {:08x} [esp] {ret:08x} r8 {:x} r9 {:x}", r[0], r[1], r[2], r[6], r[7], r[4], cpu.r8[0], cpu.r8[1]);
    PREV.with(|p| p.set((k + 1, r[2], r[1])));
}

#[cfg(test)]
mod tests {
    use super::{allowed, GUEST_END, NULL_PAGES};
    use proptest::prelude::*;

    /// sake's trap range (crates/sake/src/api.rs TRAP_LO/TRAP_HI) and memory sizes
    const LO: u32 = 0xfe00_0000;
    const HI: u32 = 0xffff_1000;
    const FIXED: u64 = 4_294_901_760;
    const GROW: u64 = 0x8400_0000;

    /// review 2026-11-14 A-1: a guest pointer into sake's own memory above 2 GB was used as is
    #[test]
    fn upper_half_is_not_the_guests() {
        for end in [FIXED, GROW] {
            assert!(allowed(0x0040_1000, 4, end, LO, HI, false));
            assert!(allowed(0x7fff_fffc, 4, end, LO, HI, false));
            // crossing 2 GB, sake's first byte, its heap, a negative index off a small base
            assert!(!allowed(0x7fff_fffd, 4, end, LO, HI, false));
            assert!(!allowed(0x8000_0000, 1, end, LO, HI, false));
            assert!(!allowed(0x9000_0000, 4, end, LO, HI, false));
            assert!(!allowed(0x10u32.wrapping_sub(0x7ff0_0000), 4, end, LO, HI, false));
            // NULL pages
            assert!(!allowed(0, 1, end, LO, HI, false));
            assert!(!allowed(NULL_PAGES - 1, 1, end, LO, HI, false));
            assert!(allowed(NULL_PAGES, 1, end, LO, HI, false));
            // the old rule, for MASU_UPPER_OPEN
            assert!(allowed(0x8100_0000, 4, end, LO, HI, true));
        }
        // API entry points: read and patched like code (where the memory reaches them)
        assert!(allowed(LO, 4, FIXED, LO, HI, false));
        assert!(allowed(0xfe00_1234, 5, FIXED, LO, HI, false));
        assert!(!allowed(LO - 1, 4, FIXED, LO, HI, false));
        assert!(!allowed(LO, 4, GROW, LO, HI, false));
        assert!(!allowed(0xffff_0000 - 2, 4, FIXED, LO, HI, false));
    }

    proptest! {
        /// allowed is exactly: inside the memory, not in the NULL pages, and wholly below 2 GB
        /// or wholly inside the trap range (or the old rule when open)
        #[test]
        fn allowed_is_the_rule(addr in any::<u32>(), len in prop_oneof![1u32..=16, any::<u32>()], lo in any::<u32>(), span in any::<u32>(), open in any::<bool>(), grow in any::<bool>()) {
            let end = if grow { GROW } else { FIXED };
            let hi = lo.saturating_add(span);
            let top = addr as u64 + len as u64;
            let inside = addr >= NULL_PAGES && top <= end;
            let guest = top <= GUEST_END;
            let trap = addr >= lo && top <= hi as u64;
            prop_assert_eq!(allowed(addr, len, end, lo, hi, open), inside && (guest || trap || open));
        }

        /// boundary addresses near 2 GB, the NULL pages and the trap range
        #[test]
        fn allowed_edges(d in -64i64..64, len in 1u32..=16, base in prop_oneof![Just(0x8000_0000i64), Just(NULL_PAGES as i64), Just(LO as i64), Just(0xffff_0000i64)]) {
            let addr = (base + d).clamp(0, u32::MAX as i64) as u32;
            let top = addr as u64 + len as u64;
            let want = addr >= NULL_PAGES && top <= FIXED && (top <= GUEST_END || (addr >= LO && top <= HI as u64));
            prop_assert_eq!(allowed(addr, len, FIXED, LO, HI, false), want);
        }
    }
}

#[cfg(kani)]
mod proofs {
    use super::{allowed, GUEST_END, NULL_PAGES};

    /// For every address, length, memory size and trap range: an allowed access lies inside the
    /// memory, above the NULL pages, and either wholly below 2 GB (the guest's half) or wholly in
    /// the trap range, unless the old rule is on. sake's own memory (2 GB up to the trap range)
    /// is never reached.
    #[kani::proof]
    fn allowed_never_reaches_sake() {
        let (addr, len, end, lo, hi): (u32, u32, u64, u32, u32) = (kani::any(), kani::any(), kani::any(), kani::any(), kani::any());
        kani::assume(end <= 1 << 32);
        if allowed(addr, len, end, lo, hi, false) {
            let top = addr as u64 + len as u64;
            assert!(addr >= NULL_PAGES && top <= end);
            assert!(top <= GUEST_END || (addr >= lo && top <= hi as u64));
        }
        // and nothing below 2 GB that the old rule allowed is lost
        if allowed(addr, len, end, lo, hi, true) && addr as u64 + len as u64 <= GUEST_END {
            assert!(allowed(addr, len, end, lo, hi, false));
        }
    }
}
