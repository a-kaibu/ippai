//! sake: a Win32 layer written from scratch for running 32-bit Windows games on the koji
//! core in the browser, without Wine (docs/sake.md).
//!
//! One process, one address space: the guest's 2 GiB are the low half of the wasm memory
//! (guest address = host address), the game's x86 code runs on the JIT (crates/kura), and
//! every imported API is a trap into Rust (api.rs). Windows threads are host threads.

/// Whether environment variable `$name` is set, read once (std::env::var takes a lock and walks
/// the environment on every call).
#[macro_export]
macro_rules! env_on {
    ($name:literal) => {{
        static V: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *V.get_or_init(|| std::env::var_os($name).is_some())
    }};
}


use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use koji::{Cpu, EAX, ESP};

pub mod acm;
pub mod apc;
pub mod api;
pub mod d3d9;
pub mod d3d11;
pub mod guest;
pub mod crt;
pub mod com;
pub mod d2d;
pub mod d3dx9;
pub mod d3dcompiler;
pub mod d3dreflect;
pub mod d3dx9fx;
pub mod d3dx9sprite;
pub mod gdiplus;
pub mod gdiplus_text;
pub mod d3dx9shader;
pub mod vcomp;
pub mod nt;
pub mod dwrite;
pub mod ddraw;
pub mod typelib;
pub mod jscript;
pub mod ini;
pub mod dinput;
pub mod fast;
pub mod files;
pub mod fmtmsg;
pub mod gdi;
pub mod gdi_geom;
pub mod k32;
pub mod kern;
pub mod pipe;
pub mod process;
pub mod mem;
pub mod misc;
pub mod movie;
pub mod unity6;
pub mod vorbis_hook;
pub mod obj;
pub mod dshow;
mod dshow_play;
pub mod mf;
pub mod usp;
pub mod ole;
pub mod picture;
pub mod wic;
pub mod wmi;
pub mod winrt;
pub mod pe;
pub mod rawinput;
pub mod seh;
pub mod seh64;
pub mod cxx;
pub mod cxx64;
pub mod rtti;
pub mod msvcp;
mod msvcp_io;
mod msvcp_loc;
mod msvcp_thr;
pub mod sound;
pub mod xapo;
pub mod xaudio2;
mod mmdevapi;
mod wasapi;
pub mod x3daudio;
pub mod mmio;
pub mod gl;
pub mod glu;
pub mod cryptstr;
pub mod dnsrec;
pub mod smalldlls;
pub mod security;
pub mod sysinfo;
pub mod cng;
pub mod iphlp;
pub mod netaddr;
pub mod fxdsp;
pub mod ws2;
pub mod ws2name;
pub mod iocp;
pub mod gl_sigs;
pub mod text;
pub mod collate;
mod casemap_table;
pub mod user;
pub mod w16;
pub mod w16api;
pub mod snap;

use mem::{r32, w32};

pub struct Thread {
    pub cpu: *mut Cpu,
    pub tid: u32,
    pub teb: u32,
    /// the thread's own handle object (signaled when it ends)
    pub obj: u32,
    /// the guest stack's reservation (released when the thread ends)
    pub stack: u32,
    /// save states: the host's thread-local state of this thread when it stood still (snap.rs)
    pub tls: [u32; 8],
    /// a thread pool thread's arguments (k32 tp_take), kept for its start over
    pub tp: Option<(u32, (u32, u32, u32))>,
}

thread_local! {
    static CUR: Cell<*mut Thread> = const { Cell::new(std::ptr::null_mut()) };
}

pub fn cur() -> &'static mut Thread {
    let p = CUR.with(|c| c.get());
    assert!(!p.is_null(), "sake: not a guest thread");
    unsafe { &mut *p }
}

static NEXT_TID: AtomicU32 = AtomicU32::new(0x20);
static PID: AtomicU32 = AtomicU32::new(kern::ROOT_PID);
/// This process's id (SAKE_PID: one another process started; crate::kern numbers them).
pub fn pid() -> u32 {
    PID.load(Ordering::Relaxed)
}
pub static PEB: AtomicU32 = AtomicU32::new(0);

/// TEB offsets that differ between 32 and 64-bit programs.
#[inline]
pub fn teb_last_error() -> u32 {
    if mem::x64() { 0x68 } else { 0x34 }
}
#[inline]
pub fn teb_tls_pointer() -> u32 {
    if mem::x64() { 0x58 } else { 0x2c }
}

/// A TEB and a stack for a new thread; returns (teb, stack top, stack base).
fn new_teb(tid: u32, stack: u32) -> (u32, u32, u32) {
    let teb = mem::reserve(0, 0x3000, 4, 0x20000, true).expect("TEB");
    mem::commit(teb, 4);
    // (at least Windows' usual 1 MB, and from the top: a stack near the bottom of the address
    // space, within the JIT's safety margin of GUEST_LO, makes every block that touches ESP
    // bail to the interpreter; KiriKiri's audio thread asked for 64 KB and got 0x10000)
    let size = stack.max(0x10_0000);
    let st = mem::reserve(0, size, 4, 0x20000, true).expect("stack");
    mem::commit(st, 4);
    let top = st + size;
    if mem::x64() {
        mem::w64(teb + 8, top as u64);
        mem::w64(teb + 0x10, st as u64);
        mem::w64(teb + 0x30, teb as u64);
        mem::w64(teb + 0x40, pid() as u64);
        mem::w64(teb + 0x48, tid as u64);
        mem::w64(teb + 0x58, teb as u64 + 0x2000); // ThreadLocalStoragePointer (static TLS blocks)
        mem::w64(teb + 0x60, PEB.load(Ordering::Relaxed) as u64);
        return (teb, top, st);
    }
    w32(teb, 0xffff_ffff); // ExceptionList
    w32(teb + 4, top);
    w32(teb + 8, st);
    w32(teb + 0x18, teb);
    w32(teb + 0x20, pid());
    w32(teb + 0x24, tid);
    w32(teb + 0x2c, teb + 0x2000); // ThreadLocalStoragePointer (static TLS blocks)
    w32(teb + 0x30, PEB.load(Ordering::Relaxed));
    (teb, top, st)
}

/// Makes the calling host thread a guest thread.
fn enter(stack: u32) -> &'static mut Thread {
    let tid = NEXT_TID.fetch_add(4, Ordering::Relaxed);
    let (teb, top, st) = new_teb(tid, stack);
    let cpu = kura::cpu::create(api::TRAP_LO, api::TRAP_HI);
    unsafe {
        (*cpu).fs_base = teb;
        (*cpu).regs[ESP] = top - 16;
        if mem::x64() {
            // long mode, GS = the TEB
            (*cpu).mode64 = 1;
            (*cpu).gs_base = teb;
        } else {
            // (a 32-bit thread starts at 53-bit precision on Windows)
            (*cpu).fpu.cw = 0x27f;
        }
    }
    let t = Box::leak(Box::new(Thread { cpu, tid, teb, obj: 0, stack: st, tls: [0; 8], tp: None }));
    adopt(t as *mut Thread);
    k32::tls_thread_init(teb);
    t
}

/// Makes the calling host thread run guest thread `t` (a new one, or one a restored save
/// state goes on with): sake's and the save states' records of it.
pub(crate) fn adopt(t: *mut Thread) {
    CUR.with(|c| c.set(t));
    kura::snap::register("guest thread", true, t as usize);
}

/// Runs guest code until it returns to RET_TRAP with ESP at or above `until` (0: forever).
pub fn run(cpu: &mut Cpu, until: u32) {
    let depth = snap::enter_run(until == 0);
    run_loop(cpu, until, depth);
    snap::leave_run();
}

fn run_loop(cpu: &mut Cpu, until: u32, depth: u32) {
    use kura::cpu::Stop;
    loop {
        snap::before_guest(cpu, depth);
        let stop = kura::cpu::run(cpu);
        match stop {
            Stop::Pause => snap::park_here(),
            Stop::Trap(a) if (w16::TRAP..w16::TRAP + 0x10000).contains(&a) => {
                if w16::dispatch(cpu, a) {
                    return;
                }
            }
            Stop::Fault { .. } | Stop::Divide | Stop::Invalid | Stop::Int(_) | Stop::Halt if cpu.mode16 != 0 => w16::exception(cpu, stop),
            Stop::Trap(a) => {
                if a == api::RET_TRAP {
                    if cpu.regs[ESP] >= until && until != 0 {
                        return;
                    }
                    fatal(cpu, "return to the host with an unexpected stack");
                } else if a == api::SEH_TRAP {
                    seh::handler_done(cpu);
                } else if a == api::EXIT_TRAP {
                    let code = cpu.regs[EAX];
                    k32::exit_thread(code);
                } else if a == api::SEH64_TRAP {
                    seh64::handler_done(cpu);
                } else if a == seh64::CONSOLIDATE_TRAP {
                    seh64::consolidated(cpu);
                } else if a == seh64::UNWIND_TRAP {
                    seh64::unwind_handler_done(cpu);
                } else {
                    api::dispatch(cpu, a);
                }
            }
            Stop::Fault { addr, write } => {
                if crate::env_on!("SAKE_TRACE_FAULT") {
                    eprintln!("sake: fault at {:#x} reading {addr:#x} (write {write})", cpu.eip);
                }
                if addr >= api::TRAP_LO && cpu.eip == addr {
                    fatal(cpu, "fault at a trap address");
                }
                // (STATUS_GUARD_PAGE_VIOLATION once on a guard page, else STATUS_ACCESS_VIOLATION)
                let guard = mem::guard_hit(addr);
                // a guard page of the thread's stack (between TEB DeallocationStack and
                // StackBase): Windows commits it, moves the guard page down and StackLimit
                // with it, and the program sees nothing (kernel32 virtual test_stack_commit)
                if guard && !mem::x64() && mem::stack_grow(crate::cur().teb, addr) {
                    continue;
                }
                let code = if guard { 0x8000_0001 } else { 0xc000_0005 };
                // (the kind: 0 read, 1 write, 8 executing at the address: DEP)
                let kind = if addr == cpu.eip && !guard { 8 } else { write as u32 };
                if mem::x64() {
                    let e = cpu.eip;
                    // a non-canonical address is a general protection fault: read of -1
                    if kind != 8 && memory_operand(cpu).is_some_and(|a| (a as i64) >> 47 != 0 && (a as i64) >> 47 != -1) {
                        seh64::raise(cpu, code, 0, e, &[0, u64::MAX]);
                        continue;
                    }
                    seh64::raise(cpu, code, 0, e, &[kind as u64, addr as u64]);
                    continue;
                }
                let e = cpu.eip;
                seh::raise(cpu, code, 0, e, &[kind, addr])
            }
            Stop::Divide => {
                // (a nonzero divisor whose quotient does not fit: STATUS_INTEGER_OVERFLOW)
                let e = cpu.eip;
                let code = if divisor_zero(cpu) == Some(false) { 0xc000_0095 } else { 0xc000_0094 };
                raise(cpu, code, e, &[])
            }
            Stop::Invalid => {
                let e = cpu.eip;
                let mut b = [0u8; 8];
                b.copy_from_slice(mem::slice(e, 8));
                if privileged(&b) {
                    raise(cpu, 0xc000_0096, e, &[]);
                    continue;
                }
                eprintln!("sake: illegal instruction at {e:#x}: {b:02x?}");
                raise(cpu, 0xc000_001d, e, &[])
            }
            // EIP is past the instruction here; Windows reports these as it does
            Stop::Int(n) => {
                let e = cpu.eip;
                match n {
                    // int3 / int 3: at the byte before EIP, and the context there too
                    3 => {
                        cpu.eip = e - 1;
                        raise(cpu, 0x8000_0003, e - 1, &[0])
                    }
                    // INTO with OF: STATUS_INTEGER_OVERFLOW at the INTO, the context after it
                    4 => raise(cpu, 0xc000_0095, e - 1, &[]),
                    // BOUND out of range (a fault: EIP at the instruction)
                    5 => raise(cpu, 0xc000_008c, e, &[]),
                    // int 2c: STATUS_ASSERTION_FAILURE at the instruction
                    0x2c => {
                        cpu.eip = e - 2;
                        raise(cpu, 0xc000_0420, e - 2, &[])
                    }
                    // int 2d (debug service), int 29 (__fastfail): skipped as before
                    0x2d | 0x29 => cpu.eip += 2,
                    // a vector user mode may not use: an access violation at the instruction
                    // reading address -1, as the general protection fault reaches the program
                    _ => {
                        cpu.eip = e - 2;
                        let m = if mem::x64() { u64::MAX } else { u32::MAX as u64 };
                        if mem::x64() {
                            seh64::raise(cpu, 0xc000_0005, 0, e - 2, &[0, m]);
                        } else {
                            seh::raise(cpu, 0xc000_0005, 0, e - 2, &[0, m as u32]);
                        }
                    }
                }
            }
            Stop::Halt => {
                let e = cpu.eip;
                raise(cpu, 0xc000_0096, e, &[])
            }
            Stop::Syscall => fatal(cpu, "syscall"),
            // STATUS_SINGLE_STEP after the instruction; the trap clears TF
            Stop::SingleStep => {
                cpu.eflags &= !koji::flags::TF;
                let e = cpu.eip;
                raise(cpu, 0x8000_0004, e, &[])
            }
            // the MXCSR as the trap left it in the second parameter; x86 Windows reports every
            // SSE trap as STATUS_FLOAT_MULTIPLE_TRAPS, x64 by the first unmasked flag
            Stop::SimdException => {
                let e = cpu.eip;
                let mx = cpu.mxcsr;
                if mem::x64() {
                    let fired = mx & 0x3f & !(mx >> 7);
                    let code = [(1, 0xc000_0090), (4, 0xc000_008e), (2, 0xc000_008d), (8, 0xc000_0091), (0x10, 0xc000_0093)]
                        .iter()
                        .find(|(b, _)| fired & b != 0)
                        .map_or(0xc000_008f, |&(_, c)| c);
                    seh64::raise(cpu, code, 0, e, &[0, mx as u64]);
                } else {
                    seh::raise(cpu, 0xc000_02b5, 0, e, &[0, mx]);
                }
            }
        }
    }
}

/// Instructions user mode may not run that raise STATUS_PRIVILEGED_INSTRUCTION (koji reports
/// them as invalid): CLI/STI, IN/OUT/INS/OUTS, MOV to or from CR/DR, CLTS, INVD/WBINVD,
/// WRMSR/RDMSR, LGDT/LIDT/LMSW/INVLPG (0F 01 /2 /3 /6 /7).
fn privileged(b: &[u8; 8]) -> bool {
    let mut i = 0;
    while i < 4 && matches!(b[i], 0x66 | 0x67 | 0xf2 | 0xf3 | 0x26 | 0x2e | 0x36 | 0x3e | 0x64 | 0x65 | 0x40..=0x4f) {
        if (0x40..=0x4f).contains(&b[i]) && !mem::x64() {
            break;
        }
        i += 1;
    }
    match b[i] {
        0xfa | 0xfb | 0xe4..=0xe7 | 0xec..=0xef | 0x6c..=0x6f | 0xf4 => true,
        0x0f => match b[i + 1] {
            0x20..=0x23 | 0x06 | 0x08 | 0x09 | 0x30 | 0x32 => true,
            0x01 => matches!((b[i + 2] >> 3) & 7, 2 | 3 | 6 | 7) && b[i + 2] >> 6 != 3,
            _ => false,
        },
        _ => false,
    }
}

/// The instruction at EIP.
fn instruction_at(cpu: &Cpu) -> koji::iced_x86::Instruction {
    use koji::iced_x86::{Decoder, DecoderOptions};
    let ip = cpu.eip as u64 | (cpu.rip_hi as u64) << 32;
    let bytes = mem::slice(cpu.eip, 15).to_vec();
    Decoder::with_ip(if mem::x64() { 64 } else { 32 }, &bytes, ip, DecoderOptions::NONE).decode()
}

/// The full address of the memory operand of the instruction at EIP.
fn memory_operand(cpu: &Cpu) -> Option<u64> {
    use koji::iced_x86::Register;
    let ins = instruction_at(cpu);
    let op = (0..ins.op_count()).find(|&i| ins.op_kind(i) == koji::iced_x86::OpKind::Memory)?;
    ins.virtual_address(op, 0, |r, _, _| match r {
        Register::FS => Some(cpu.fs_base as u64),
        Register::GS => Some(cpu.gs_base as u64),
        Register::ES | Register::CS | Register::SS | Register::DS => Some(0),
        Register::AH | Register::CH | Register::DH | Register::BH => Some(cpu.gpr(r as usize - Register::AH as usize) >> 8 & 0xff),
        r => Some(cpu.gpr(r.full_register().number())),
    })
}

/// For a divide error at EIP: whether the divisor of the DIV/IDIV there is zero (None: not
/// decoded).
fn divisor_zero(cpu: &Cpu) -> Option<bool> {
    use koji::iced_x86::{OpKind, Register};
    let ins = instruction_at(cpu);
    let (v, size) = match ins.op0_kind() {
        OpKind::Register => {
            let r = ins.op0_register();
            let v = if matches!(r, Register::AH | Register::CH | Register::DH | Register::BH) {
                cpu.gpr(r as usize - Register::AH as usize) >> 8
            } else {
                cpu.gpr(r.full_register().number())
            };
            (v, r.size())
        }
        OpKind::Memory => {
            // (the divide read it already)
            let a = memory_operand(cpu)? as u32;
            let size = ins.memory_size().size();
            let v = match size {
                1 => mem::r8(a) as u64,
                2 => mem::r16(a) as u64,
                4 => mem::r32(a) as u64,
                _ => mem::r64(a),
            };
            (v, size)
        }
        _ => return None,
    };
    let m = if size >= 8 { u64::MAX } else { (1u64 << (8 * size)) - 1 };
    Some(v & m == 0)
}

/// An exception at `addr` (32 or 64-bit dispatch).
fn raise(cpu: &mut Cpu, code: u32, addr: u32, params: &[u32]) {
    if mem::x64() {
        let p: Vec<u64> = params.iter().map(|&x| x as u64).collect();
        seh64::raise(cpu, code, 0, addr, &p)
    } else {
        seh::raise(cpu, code, 0, addr, params)
    }
}

/// Calls guest code at `f` with `args` pushed right to left; returns EAX. The registers
/// (but EAX/EDX) come back as they were.
pub fn call(cpu: &mut Cpu, f: u32, args: &[u32]) -> u32 {
    if w16::on() {
        return w16::call_trap(cpu, f, args);
    }
    if mem::x64() {
        // (addresses in an image above 4G get their upper half: callbacks' module handles,
        // objects in its data)
        let a: Vec<u64> = args.iter().map(|&x| mem::widen(x)).collect();
        return call64(cpu, mem::widen(f), &a) as u32;
    }
    let regs = cpu.regs;
    let eip = cpu.eip;
    cpu.materialize();
    let eflags = cpu.eflags;
    let mut sp = regs[ESP];
    for &a in args.iter().rev() {
        sp -= 4;
        w32(sp, a);
    }
    sp -= 4;
    w32(sp, api::RET_TRAP);
    cpu.regs[ESP] = sp;
    cpu.eip = f;
    run(cpu, sp + 4);
    let r = cpu.regs[EAX];
    let edx = cpu.regs[koji::EDX];
    cpu.regs = regs;
    cpu.regs[EAX] = r;
    cpu.regs[koji::EDX] = edx;
    cpu.eip = eip;
    cpu.eflags = eflags;
    cpu.lazy.op = 0;
    r
}

/// x64: calls guest code at `f` with the Windows x64 convention (RCX, RDX, R8, R9, then the
/// stack above 32 bytes of home space); returns RAX. The registers but RAX come back.
pub fn call64(cpu: &mut Cpu, f: u64, args: &[u64]) -> u64 {
    let (regs, hi, r8) = (cpu.regs, cpu.hi, cpu.r8);
    let (eip, rip_hi) = (cpu.eip, cpu.rip_hi);
    cpu.materialize();
    let eflags = cpu.eflags;
    let nstack = args.len().saturating_sub(4) as u32;
    // (at the callee's entry RSP+8 is 16-byte aligned)
    let sp = ((regs[ESP] - 0x30 - 8 * nstack) & !15) - 8;
    mem::w64(sp, api::RET_TRAP as u64);
    for (i, &a) in args.iter().enumerate() {
        if i < 4 {
            cpu.set_gpr([1, 2, 8, 9][i], a);
        }
        mem::w64(sp + 8 + 8 * i as u32, a);
    }
    cpu.set_gpr(ESP, sp as u64);
    cpu.eip = f as u32;
    cpu.rip_hi = (f >> 32) as u32;
    run(cpu, sp + 8);
    let rax = cpu.gpr(EAX);
    // (RDX too comes back: the host function's own arguments are RCX/RDX/R8/R9, which it may
    // read again after the call)
    cpu.regs = regs;
    cpu.hi = hi;
    cpu.r8 = r8;
    cpu.set_gpr(EAX, rax);
    cpu.eip = eip;
    cpu.rip_hi = rip_hi;
    cpu.eflags = eflags;
    cpu.lazy.op = 0;
    rax
}

pub fn fatal(cpu: &mut Cpu, msg: &str) -> ! {
    api::dump_ring("the fatal error");
    // the page saves a crash report (with this thread's registers, stack and last API calls)
    kura::crashlog::exception(cpu, &format!("sake fatal: {msg}"), None, false);
    kura::crashlog::crash(&format!("sake: {msg}"));
    let r = &cpu.regs;
    eprintln!(
        "sake: {msg}\n  eip {:08x} eax {:08x} ecx {:08x} edx {:08x} ebx {:08x} esp {:08x} ebp {:08x} esi {:08x} edi {:08x}",
        cpu.eip, r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7]
    );
    let mut s = String::new();
    for k in 0..16 {
        let a = r[ESP] + 4 * k;
        if a < mem::HI {
            s += &format!(" {:x}", r32(a));
        }
    }
    eprintln!("  stack:{s}");
    let mut ebp = r[5];
    let mut chain = String::new();
    for _ in 0..12 {
        if !(mem::LO..mem::HI - 8).contains(&ebp) {
            break;
        }
        chain += &format!(" {:x}", r32(ebp + 4));
        let n = r32(ebp);
        if n <= ebp {
            break;
        }
        ebp = n;
    }
    eprintln!("  frames:{chain}");
    unsafe { libc::exit(3) }
}

/// Starts a guest thread at `start(param)`; returns its tid. `gated`: it waits for
/// ResumeThread first.
pub fn spawn_gated(start: u32, param: u32, stack: u32, obj: u32, gated: bool) -> u32 {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .stack_size(1 << 20)
        .spawn(move || {
            let t = enter(stack);
            // (the thread's own reference: the creator closing its handle doesn't end the
            // object while the thread runs; GetCurrentThread duplicates it, mono's GC does)
            t.obj = obj::dup(obj);
            tx.send(t.tid).unwrap();
            if gated {
                k32::wait_gate_start(obj, start, param);
            }
            thread_start(t, start, param);
            run(unsafe { &mut *t.cpu }, 0);
        })
        .expect("thread");
    rx.recv().unwrap()
}

/// A new thread's first steps: DllMain(THREAD_ATTACH), then `start(param)` set up to run.
pub(crate) fn thread_start(t: &mut Thread, start: u32, param: u32) {
    let cpu = unsafe { &mut *t.cpu };
    pe::thread_attach(cpu);
    if mem::x64() {
        return setup64(cpu, start, param);
    }
    let sp = cpu.regs[ESP] - 8;
    w32(sp, pe::exit_return());
    w32(sp + 4, param);
    cpu.regs[ESP] = sp;
    cpu.eip = start;
}

/// x64: runs `start(param)` as the thread's first function (returning to EXIT_TRAP).
fn start64(cpu: &mut Cpu, start: u32, param: u32) -> ! {
    setup64(cpu, start, param);
    run(cpu, 0);
    unsafe { libc::exit(0) }
}

/// x64: the registers and stack for `start(param)` returning to EXIT_TRAP.
fn setup64(cpu: &mut Cpu, start: u32, param: u32) {
    let sp = ((cpu.regs[ESP] - 0x40) & !15) - 8;
    mem::w64(sp, api::EXIT_TRAP as u64);
    cpu.set_gpr(ESP, sp as u64);
    cpu.set_gpr(1, mem::widen(param));
    let f = mem::widen(start);
    cpu.eip = f as u32;
    cpu.rip_hi = (f >> 32) as u32;
}

/// The program: `exe` is its host path.
pub fn main(exe: &str, args: &[String]) -> ! {
    if let Some(p) = std::env::var("SAKE_PID").ok().and_then(|s| s.parse::<u32>().ok()) {
        PID.store(p, Ordering::Relaxed);
        NEXT_TID.store(p + 0x10, Ordering::Relaxed);
    }
    if kura::snap::restoring() {
        snap::restore();
    }
    if pe::is_64bit_file(exe) {
        mem::set_x64();
    }
    guest::init();
    k32::init();
    user::init();
    rawinput::init();
    gdi::init();
    misc::init();
    gdiplus::init();
    gdiplus_text::init();
    crt::init();
    msvcp::init();
    vcomp::init();
    nt::init();
    ini::init();
    ddraw::init();
    typelib::init();
    ole::init();
    winrt::init();
    usp::init();
    acm::init();
    dinput::init();
    d2d::init();
    d3dx9::init();
    d3dx9shader::init();
    d3dcompiler::init();
    dwrite::init();
    mf::init();
    sound::init();
    mmio::init();
    gl::init();
    glu::init();
    smalldlls::init();
    cryptstr::init();
    security::init();
    sysinfo::init();
    pipe::register();
    cng::init();
    iphlp::init();
    netaddr::init();
    ws2name::init();
    ws2::init();
    iocp::init();
    xaudio2::init();
    xapo::init();
    d3d9::register();
    seh::register();
    seh64::register();
    cxx::init();
    rtti::init();
    d3d11::register();
    guest::bind_host();
    snap::init();
    // SAKE_MEMINFO=<seconds>: sake's own heap (above the guest's 2 GB) now and then, on stderr
    #[cfg(target_os = "emscripten")]
    if let Some(secs) = std::env::var("SAKE_MEMINFO").ok().and_then(|s| s.parse::<f64>().ok()) {
        std::thread::spawn(move || {
            let mut max = 0.0f64;
            loop {
                std::thread::sleep(std::time::Duration::from_secs_f64(secs.max(0.1)));
                let mut m = [0f64; 2];
                unsafe { kura::wrap::vnhost_mem_info(m.as_mut_ptr()) };
                max = max.max(m[0]);
                eprintln!("sake: heap top {:.0} MB (max {:.0}), in use {:.0} MB", m[0] / 1048576.0, max / 1048576.0, m[1] / 1048576.0);
            }
        });
    }
    kura::crashlog::set_trap_namer(api::name_of);
    let peb = mem::reserve(0, 0x2000, 4, 0x20000, true).expect("PEB");
    mem::commit(peb, 4);
    PEB.store(peb, Ordering::Relaxed);
    let t = enter(0x10_0000);
    t.obj = obj::new_thread_obj(t.tid);
    let cpu = unsafe { &mut *t.cpu };
    let wpath = files::host_to_win(exe);
    k32::set_command_line(&wpath, args);
    let dir = wpath.rsplit_once('\\').map_or(String::from("C:\\"), |(d, _)| format!("{d}\\"));
    files::set_cwd(&dir);
    // (a process another one started: what CreateProcess gave it, and maybe a wait for
    // ResumeThread)
    if let Some(r) = process::record() {
        k32::set_command_line_raw(&r.cmdline);
        files::set_cwd(&r.cwd);
        k32::set_std_handles(r.std);
        process::start_child(&r);
        if r.suspend > 0 {
            process::wait_resumed();
        }
    }
    if w16::is_ne(exe) {
        w16::start(cpu, exe, &wpath, args);
    }
    let base = match pe::load_exe(cpu, exe, &wpath) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sake: {exe}: {e}");
            unsafe { libc::exit(2) }
        }
    };
    let x64 = mem::x64();
    let pp = peb + 0x1000;
    if x64 {
        mem::w64(peb + 0x10, mem::widen(base));
        mem::w64(peb + 0x20, pp as u64);
        mem::w64(peb + 0x30, k32::PROCESS_HEAP as u64);
        w32(peb + 0x118, 6);
        w32(peb + 0x11c, 1);
        w32(peb + 0x120, 7601);
        w32(peb + 0x124, 2);
        w32(pp, 0x400);
        w32(pp + 4, 0x400);
    } else {
        w32(peb + 8, base);
        w32(peb + 0x18, k32::PROCESS_HEAP);
        w32(peb + 0xa4, 6);
        w32(peb + 0xa8, 1);
        w32(peb + 0xac, 7601);
        w32(peb + 0xb0, 2);
        // RTL_USER_PROCESS_PARAMETERS (the CRT reads its flags; image path and command line)
        w32(peb + 0x10, pp);
        w32(pp, 0x2a0);
        w32(pp + 4, 0x2a0);
    }
    w32(pp + 8, 1);
    let (_, wcmd) = k32::cmdline();
    let wpath16: Vec<u16> = wpath.encode_utf16().collect();
    let wp = mem::heap_alloc(2 * wpath16.len() as u32 + 2, true);
    for (i, &u) in wpath16.iter().enumerate() {
        mem::w16(wp + 2 * i as u32, u);
    }
    let cmdlen = mem::wstr(wcmd).len() as u32 * 2;
    let ustr = |at: u32, len: u32, buf: u32| {
        mem::w16(at, len as u16);
        mem::w16(at + 2, len as u16 + 2);
        mem::wp(at + mem::ps(), buf);
    };
    let (img, cmd) = if x64 { (0x60, 0x70) } else { (0x38, 0x40) };
    ustr(pp + img, 2 * wpath16.len() as u32, wp);
    ustr(pp + cmd, cmdlen, wcmd);
    // the window title (STARTUPINFO lpTitle) is the image path when the parent gave none
    ustr(pp + if x64 { 0xb0 } else { 0x70 }, 2 * wpath16.len() as u32, wp);
    let entry = pe::with(base, |m| m.entry).unwrap();
    pe::init_dlls(cpu);
    if x64 {
        start64(cpu, entry, peb);
    }
    let sp = cpu.regs[ESP] - 8;
    w32(sp, pe::exit_return());
    w32(sp + 4, peb);
    cpu.regs[ESP] = sp;
    cpu.eip = entry;
    run(cpu, 0);
    unsafe { libc::exit(0) }
}

/// C entry: `sake_main(argc, argv)`; argv[1] is the exe's host path.
#[no_mangle]
pub unsafe extern "C" fn sake_main(argc: i32, argv: *const *const libc::c_char) -> i32 {
    let args: Vec<String> =
        (0..argc as usize).map(|i| std::ffi::CStr::from_ptr(*argv.add(i)).to_string_lossy().into_owned()).collect();
    if args.len() < 2 {
        eprintln!("usage: sake <program.exe> [args]");
        return 1;
    }
    // SAKE_TRACE=3: a panic of sake itself (a guest's bad argument) also shows the last API calls
    if api::trace() == 3 {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |i| {
            api::dump_ring("the panic");
            prev(i)
        }));
    }
    main(&args[1], &args[2..])
}

/// The hash for sake's maps: their keys are handles and addresses (u32), looked up on every
/// API call, where std's SipHash costs more than the lookup (FxHash, as in rustc-hash 2).
#[derive(Default, Clone, Copy)]
pub struct Fx(u64);
impl Fx {
    #[inline]
    fn add(&mut self, x: u64) {
        self.0 = (self.0 ^ x).wrapping_mul(0xf135_7aea_2e62_a9c5);
    }
}
impl std::hash::Hasher for Fx {
    #[inline]
    fn finish(&self) -> u64 {
        // the product's high bits are the mixed ones; hashbrown indexes with the low ones
        self.0.rotate_left(26)
    }
    fn write(&mut self, b: &[u8]) {
        for c in b.chunks(8) {
            let mut a = [0u8; 8];
            a[..c.len()].copy_from_slice(c);
            self.add(u64::from_le_bytes(a));
        }
        self.add(b.len() as u64);
    }
    #[inline]
    fn write_u8(&mut self, n: u8) {
        self.add(n as u64)
    }
    #[inline]
    fn write_u16(&mut self, n: u16) {
        self.add(n as u64)
    }
    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.add(n as u64)
    }
    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.add(n)
    }
    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.add(n as u64)
    }
}
pub type HashMap<K, V> = std::collections::HashMap<K, V, std::hash::BuildHasherDefault<Fx>>;
pub type HashSet<K> = std::collections::HashSet<K, std::hash::BuildHasherDefault<Fx>>;
