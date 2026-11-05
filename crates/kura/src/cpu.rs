//! The x86 run loop sake drives (crates/sake/src/lib.rs).
//!
//! Guest and host share the linear memory, so a guest address is used as a pointer as is.
//! The first 64 KiB stay unmapped for the guest so NULL dereferences raise access violations.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use koji::{Cpu, Exit, Fault, Memory};

const NULL_PAGES: u32 = 0x10000;

/// Exit codes shared with signal_wasm.c (`enum vnhost_exit`).
const EXIT_TRAP: i32 = 1;
const EXIT_FAULT: i32 = 2;
const EXIT_DIVIDE: i32 = 3;
const EXIT_INVALID: i32 = 4;
const EXIT_INT: i32 = 5;
const EXIT_HALT: i32 = 6;
const EXIT_PENDING: i32 = 7;
/// SYSCALL in long mode (x86-64 guests)
const EXIT_SYSCALL: i32 = 8;

/// The linear memory as the guest sees it.
pub(crate) struct Linear;

#[inline(always)]
fn mem_end() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::memory_size(0) as u64 * 65536
    }
    #[cfg(target_arch = "wasm64")]
    {
        core::arch::wasm64::memory_size(0) as u64 * 65536
    }
    #[cfg(not(target_family = "wasm"))]
    {
        0
    }
}

#[inline(always)]
fn check(addr: u32, len: u32, write: bool) -> Result<*mut u8, Fault> {
    if addr < NULL_PAGES || addr as u64 + len as u64 > mem_end() {
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
        let end = mem_end();
        if addr < NULL_PAGES || addr as u64 >= end {
            return 0;
        }
        let n = (buf.len() as u64).min(end - addr as u64) as usize;
        unsafe { std::ptr::copy_nonoverlapping(addr as usize as *const u8, buf.as_mut_ptr(), n) };
        n
    }
}

// ---------------------------------------------------------------------------------------------
// signals: pending bits per thread the run loop and the JIT check between blocks (nothing
// raises them since Wine's processes went; the JIT still polls the word)

struct ThreadSignals {
    pending: AtomicU32,
}

static NEXT_TID: AtomicU32 = AtomicU32::new(1000);

thread_local! {
    static SELF: (i32, Arc<ThreadSignals>) = {
        let tid = NEXT_TID.fetch_add(1, Ordering::Relaxed) as i32;
        (tid, Arc::new(ThreadSignals { pending: AtomicU32::new(0) }))
    };
}

#[no_mangle]
pub extern "C" fn vnhost_gettid() -> i32 {
    SELF.with(|s| s.0)
}

/// Takes `sig` off the calling thread's pending set; returns whether it was pending.
#[no_mangle]
pub extern "C" fn vnhost_signal_pending(sig: i32) -> i32 {
    SELF.with(|s| (s.1.pending.fetch_and(!(1 << sig), Ordering::SeqCst) & (1 << sig) != 0) as i32)
}

// ---------------------------------------------------------------------------------------------
// the C interface of signal_wasm.c

#[no_mangle]
pub extern "C" fn vnhost_cpu_create(trap_lo: u32, trap_hi: u32) -> *mut Cpu {
    let mut cpu = Box::new(Cpu::default());
    cpu.trap_lo = trap_lo;
    cpu.trap_hi = trap_hi;
    cpu.fpu.cw = 0x27f;
    let cpu = Box::into_raw(cpu);
    crate::crashlog::register(cpu);
    cpu
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_destroy(cpu: *mut Cpu) {
    crate::crashlog::unregister(cpu);
    drop(Box::from_raw(cpu));
}

/// regs: eax ecx edx ebx esp ebp esi edi eip eflags
#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_get(cpu: *const Cpu, regs: *mut u32) {
    let c = &*cpu;
    let r = std::slice::from_raw_parts_mut(regs, 10);
    r[..8].copy_from_slice(&c.regs);
    r[8] = c.eip;
    r[9] = c.flags();
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_set(cpu: *mut Cpu, regs: *const u32) {
    let c = &mut *cpu;
    let r = std::slice::from_raw_parts(regs, 10);
    c.regs.copy_from_slice(&r[..8]);
    c.eip = r[8];
    c.eflags = r[9] | 0x2;
    c.lazy.op = 0;
}

/// x86-64 guests (signal_wasm64.c): long mode, GS = the TEB.
#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_set_long_mode(cpu: *mut Cpu, gs_base: u32) {
    (*cpu).mode64 = 1;
    (*cpu).gs_base = gs_base;
}

/// regs: rax rcx rdx rbx rsp rbp rsi rdi r8-r15 rip rflags
#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_get64(cpu: *const Cpu, regs: *mut u64) {
    let c = &*cpu;
    let r = std::slice::from_raw_parts_mut(regs, 18);
    for (i, v) in r[..16].iter_mut().enumerate() {
        *v = c.gpr(i);
    }
    r[16] = c.eip as u64 | (c.rip_hi as u64) << 32;
    r[17] = c.flags() as u64;
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_set64(cpu: *mut Cpu, regs: *const u64) {
    let c = &mut *cpu;
    let r = std::slice::from_raw_parts(regs, 18);
    for (i, v) in r[..16].iter().enumerate() {
        c.set_gpr(i, *v);
    }
    c.eip = r[16] as u32;
    c.rip_hi = (r[16] >> 32) as u32;
    c.eflags = r[17] as u32 | 0x2;
    c.lazy.op = 0;
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_set_fs(cpu: *mut Cpu, base: u32) {
    (*cpu).fs_base = base;
}

/// FXSAVE image of the x87/SSE state into a host buffer.
#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_fxsave(cpu: *const Cpu, dst: *mut u8) {
    let mut buf = Buf(std::slice::from_raw_parts_mut(dst, 512));
    let _ = koji::fxsave(&*cpu, &mut buf, 0);
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_fxrstor(cpu: *mut Cpu, src: *const u8) {
    let mut copy = [0u8; 512];
    copy.copy_from_slice(std::slice::from_raw_parts(src, 512));
    let mut buf = Buf(&mut copy);
    let _ = koji::fxrstor(&mut *cpu, &mut buf, 0);
}

#[no_mangle]
pub unsafe extern "C" fn vnhost_cpuid(leaf: u32, sub: u32, regs: *mut u32) {
    let (a, b, c, d) = koji::cpuid(leaf, sub);
    let r = std::slice::from_raw_parts_mut(regs, 4);
    r.copy_from_slice(&[a, b, c, d]);
}

/// What an [`Exit`] means for signal_wasm.c; None to keep running.
fn exit_code(cpu: &Cpu, mem: &mut Linear, exit: Exit, info: &mut [u32], from_jit: bool) -> Option<i32> {
    use crate::crashlog::exception;
    match exit {
        Exit::Continue => None,
        Exit::Trap(a) => {
            crate::crashlog::trap(cpu, a);
            info[0] = a;
            Some(EXIT_TRAP)
        }
        Exit::Fault(f) => {
            exception(cpu, "access violation", Some((f.addr, f.write)), from_jit);
            // IPPAI_TRACE_FAULT: where the interpreter (or a region's step) faulted
            static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            if *ON.get_or_init(|| std::env::var_os("IPPAI_TRACE_FAULT").is_some()) {
                let r = &cpu.regs;
                let ok = (koji::jit::GUEST_LO..koji::jit::GUEST_HI - 8).contains(&cpu.eip);
                let code: Vec<String> = (0..if ok { 8 } else { 0 }).map(|i| unsafe { *((cpu.eip as usize + i) as *const u8) }).map(|b| format!("{b:02x}")).collect();
                eprintln!(
                    "fault {} {:08x} at eip {:08x} [{}] | eax {:08x} ecx {:08x} edx {:08x} ebx {:08x} esp {:08x} ebp {:08x} esi {:08x} edi {:08x} eflags {:08x}",
                    if f.write { "write" } else { "read" }, f.addr, cpu.eip, code.join(" "), r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], cpu.eflags
                );
            }
            // IPPAI_DUMP_FAULT=<eip, hex>: 64-bit registers and the memory they point at when a
            // fault happens there
            // ("null": any fault below 64K)
            static AT: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
            let at = *AT.get_or_init(|| {
                std::env::var("IPPAI_DUMP_FAULT").ok().and_then(|v| if v == "null" { Some(0) } else { u32::from_str_radix(v.trim_start_matches("0x"), 16).ok() })
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
            info[0] = f.addr;
            info[1] = f.write as u32;
            Some(EXIT_FAULT)
        }
        Exit::DivideError => {
            exception(cpu, "divide error", None, from_jit);
            Some(EXIT_DIVIDE)
        }
        Exit::InvalidOpcode => {
            exception(cpu, "invalid opcode", None, from_jit);
            info[0] = cpu.eip;
            if crate::env_on!("IPPAI_TRACE_UD") {
                let mut b = [0u8; 16];
                let n = mem.fetch(cpu.eip, &mut b);
                eprintln!("kura: invalid opcode at {:#x}: {:02x?}", cpu.eip, &b[..n]);
            }
            Some(EXIT_INVALID)
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
            info[0] = n as u32;
            Some(EXIT_INT)
        }
        Exit::Halt => {
            exception(cpu, "hlt (privileged instruction)", None, from_jit);
            Some(EXIT_HALT)
        }
        Exit::Syscall => Some(EXIT_SYSCALL),
    }
}

/// IPPAI_TRACE_EIP=<seconds>: every so often, print where each thread's guest code is (the
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
        let s = std::env::var("IPPAI_TRACE_EIP").ok()?.parse().ok()?;
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
    let tid = SELF.with(|s| s.0);
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

/// Runs until the guest reaches a trap address, faults, or a signal is pending.
/// `info` gets the trap/fault address (and for faults, whether it was a write).
#[no_mangle]
pub unsafe extern "C" fn vnhost_cpu_run(cpu: *mut Cpu, info: *mut u32) -> i32 {
    let _guest = trace_eip(cpu);
    crate::crashlog::in_guest(true);
    let r = cpu_run(cpu, info);
    crate::crashlog::in_guest(false);
    r
}

unsafe fn cpu_run(cpu: *mut Cpu, info: *mut u32) -> i32 {
    let cpu = &mut *cpu;
    let info = std::slice::from_raw_parts_mut(info, 2);
    let signals = SELF.with(|s| s.1.clone());
    let mut mem = Linear;
    // long mode: IPPAI_JIT64=0 leaves it to the interpreter
    let jit = crate::jit::enabled() && (cpu.mode64 == 0 || crate::jit::enabled64()) && cpu.mode16 == 0;
    crate::jit::count_entry(cpu.eip);
    loop {
        let pending = signals.pending.load(Ordering::Relaxed);
        if pending != 0 {
            return EXIT_PENDING;
        }
        trace_call(cpu);
        if jit {
            match crate::jit::run(cpu, signals.pending.as_ptr() as u32) {
                crate::jit::Ran::Stopped => {
                    crate::jit::count_entry(cpu.eip | 0x8000_0000);
                    continue;
                }
                crate::jit::Ran::Exit(e) => match exit_code(cpu, &mut mem, e, info, true) {
                    Some(code) => return code,
                    None => continue,
                },
                crate::jit::Ran::Missed => {
                    crate::jit::count_miss(cpu.eip);
                    if crate::jit::maybe_compile(cpu, &mut mem) {
                        continue;
                    }
                }
            }
        }
        // the interpreter: up to the next branch with the JIT, a batch of steps without
        for _ in 0..4096 {
            let (e, branch) = crate::jit::step(cpu, &mut mem);
            if let Some(code) = exit_code(cpu, &mut mem, e, info, false) {
                return code;
            }
            if jit && branch {
                break;
            }
            trace_call(cpu);
        }
    }
}

/// IPPAI_TRACE_CALL=<hex eip>: each time a thread gets to that address, print EAX ECX EDX ESI
/// EDI ESP and a hash of the EDX..EDX+ECX bytes of its previous arrival (a register-call
/// memcpy's destination: compare JIT and interpreter runs to find the first copy that differs).
fn trace_call(cpu: &Cpu) {
    static AT: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    let Some(at) = *AT.get_or_init(|| u32::from_str_radix(&std::env::var("IPPAI_TRACE_CALL").ok()?, 16).ok()) else { return };
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

/// A host buffer seen as guest memory at address 0 (for FXSAVE images).
struct Buf<'a>(&'a mut [u8]);

impl Memory for Buf<'_> {
    fn read8(&mut self, addr: u32) -> Result<u8, Fault> {
        self.0.get(addr as usize).copied().ok_or(Fault { addr, write: false })
    }
    fn write8(&mut self, addr: u32, v: u8) -> Result<(), Fault> {
        match self.0.get_mut(addr as usize) {
            Some(b) => {
                *b = v;
                Ok(())
            }
            None => Err(Fault { addr, write: true }),
        }
    }
}
