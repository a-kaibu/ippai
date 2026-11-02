//! sake: a Win32 layer written from scratch for running 32-bit Windows games on the vnx86
//! core in the browser, without Wine (docs/sake.md).
//!
//! One process, one address space: the guest's 2 GiB are the low half of the wasm memory
//! (guest address = host address), the game's x86 code runs on the JIT (crates/vnhost), and
//! every imported API is a trap into Rust (api.rs). Windows threads are host threads.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};

use vnx86::{Cpu, EAX, ESP};

pub mod acm;
pub mod api;
pub mod d3d9;
pub mod d3d11;
pub mod unixlib;
pub mod guest;
pub mod crt;
pub mod ddraw;
pub mod ini;
pub mod dinput;
pub mod fast;
pub mod files;
pub mod gdi;
pub mod k32;
pub mod mem;
pub mod misc;
pub mod obj;
pub mod dshow;
pub mod usp;
pub mod ole;
pub mod pe;
pub mod seh;
pub mod cxx;
pub mod sound;
pub mod mmio;
pub mod gl;
pub mod gl_sigs;
pub mod text;
pub mod user;

use mem::{r32, w32};

pub struct Thread {
    pub cpu: *mut Cpu,
    pub tid: u32,
    pub teb: u32,
    /// the thread's own handle object (signaled when it ends)
    pub obj: u32,
    /// the guest stack's reservation (released when the thread ends)
    pub stack: u32,
}

thread_local! {
    static CUR: Cell<*mut Thread> = const { Cell::new(std::ptr::null_mut()) };
}

pub fn cur() -> &'static mut Thread {
    let p = CUR.with(|c| c.get());
    assert!(!p.is_null(), "sake: not a guest thread");
    unsafe { &mut *p }
}

pub fn is_guest_thread() -> bool {
    !CUR.with(|c| c.get()).is_null()
}

static NEXT_TID: AtomicU32 = AtomicU32::new(0x20);
pub const PID: u32 = 0x10;
pub static PEB: AtomicU32 = AtomicU32::new(0);

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
    w32(teb, 0xffff_ffff); // ExceptionList
    w32(teb + 4, top);
    w32(teb + 8, st);
    w32(teb + 0x18, teb);
    w32(teb + 0x20, PID);
    w32(teb + 0x24, tid);
    w32(teb + 0x2c, teb + 0x2000); // ThreadLocalStoragePointer (static TLS blocks)
    w32(teb + 0x30, PEB.load(Ordering::Relaxed));
    (teb, top, st)
}

/// Makes the calling host thread a guest thread.
fn enter(stack: u32) -> &'static mut Thread {
    let tid = NEXT_TID.fetch_add(4, Ordering::Relaxed);
    let (teb, top, st) = new_teb(tid, stack);
    let cpu = vnhost::cpu::vnhost_cpu_create(api::TRAP_LO, api::TRAP_HI);
    unsafe {
        (*cpu).fs_base = teb;
        (*cpu).regs[ESP] = top - 16;
    }
    let t = Box::leak(Box::new(Thread { cpu, tid, teb, obj: 0, stack: st }));
    CUR.with(|c| c.set(t));
    k32::tls_thread_init(teb);
    t
}

/// Exit codes of vnhost_cpu_run (signal_wasm.c's `enum vnhost_exit`).
const X_TRAP: i32 = 1;
const X_FAULT: i32 = 2;
const X_DIVIDE: i32 = 3;
const X_INVALID: i32 = 4;
const X_INT: i32 = 5;
const X_HALT: i32 = 6;
const X_PENDING: i32 = 7;

/// Runs guest code until it returns to RET_TRAP with ESP at or above `until` (0: forever).
pub fn run(cpu: &mut Cpu, until: u32) {
    let mut info = [0u32; 2];
    loop {
        let code = unsafe { vnhost::cpu::vnhost_cpu_run(cpu, info.as_mut_ptr()) };
        match code {
            X_TRAP => {
                let a = info[0];
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
                } else {
                    api::dispatch(cpu, a);
                }
            }
            X_FAULT => {
                if std::env::var_os("SAKE_TRACE_FAULT").is_some() {
                    eprintln!("sake: fault at {:#x} reading {:#x} (write {})", cpu.eip, info[0], info[1]);
                }
                if info[0] >= api::TRAP_LO && cpu.eip == info[0] {
                    fatal(cpu, "fault at a trap address");
                }
                seh::raise_fault(cpu, info[0], info[1] != 0)
            }
            X_DIVIDE => {
                let e = cpu.eip;
                seh::raise(cpu, 0xc000_0094, 0, e, &[])
            }
            X_INVALID => {
                let e = cpu.eip;
                let mut b = [0u8; 8];
                b.copy_from_slice(mem::slice(e, 8));
                eprintln!("sake: illegal instruction at {e:#x}: {b:02x?}");
                seh::raise(cpu, 0xc000_001d, 0, e, &[])
            }
            X_INT => {
                let e = cpu.eip;
                if info[0] == 3 {
                    seh::raise(cpu, 0x8000_0003, 0, e, &[0])
                } else {
                    // int 2d etc.: skip it
                    cpu.eip += 2;
                }
            }
            X_HALT => {
                let e = cpu.eip;
                seh::raise(cpu, 0xc000_0096, 0, e, &[])
            }
            X_PENDING => {
                for s in 1..32 {
                    vnhost::cpu::vnhost_signal_pending(s);
                }
            }
            _ => fatal(cpu, &format!("cpu run exit {code}")),
        }
    }
}

/// Calls guest code at `f` with `args` pushed right to left; returns EAX. The registers
/// (but EAX/EDX) come back as they were.
pub fn call(cpu: &mut Cpu, f: u32, args: &[u32]) -> u32 {
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
    let edx = cpu.regs[vnx86::EDX];
    cpu.regs = regs;
    cpu.regs[EAX] = r;
    cpu.regs[vnx86::EDX] = edx;
    cpu.eip = eip;
    cpu.eflags = eflags;
    cpu.lazy.op = 0;
    r
}

pub fn fatal(cpu: &mut Cpu, msg: &str) -> ! {
    api::dump_ring("the fatal error");
    // the page saves a crash report (with this thread's registers, stack and last API calls)
    vnhost::crashlog::exception(cpu, &format!("sake fatal: {msg}"), None, false);
    vnhost::crashlog::crash(&format!("sake: {msg}"));
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
            t.obj = obj;
            tx.send(t.tid).unwrap();
            if gated {
                k32::wait_gate(obj);
            }
            let cpu = unsafe { &mut *t.cpu };
            pe::thread_attach(cpu);
            let sp = cpu.regs[ESP] - 8;
            w32(sp, api::EXIT_TRAP);
            w32(sp + 4, param);
            cpu.regs[ESP] = sp;
            cpu.eip = start;
            run(cpu, 0);
        })
        .expect("thread");
    rx.recv().unwrap()
}

/// The program: `exe` is its host path.
pub fn main(exe: &str, args: &[String]) -> ! {
    guest::init();
    k32::init();
    user::init();
    gdi::init();
    misc::init();
    crt::init();
    ini::init();
    ddraw::init();
    ole::init();
    usp::init();
    acm::init();
    dinput::init();
    sound::init();
    mmio::init();
    gl::init();
    d3d9::register();
    seh::register();
    cxx::init();
    d3d11::register();
    guest::bind_host();
    vnhost::crashlog::set_trap_namer(api::name_of);
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
    let base = match pe::load_exe(cpu, exe, &wpath) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sake: {exe}: {e}");
            unsafe { libc::exit(2) }
        }
    };
    w32(peb + 8, base);
    w32(peb + 0x18, k32::PROCESS_HEAP);
    w32(peb + 0xa4, 6);
    w32(peb + 0xa8, 1);
    w32(peb + 0xac, 7601);
    w32(peb + 0xb0, 2);
    // RTL_USER_PROCESS_PARAMETERS (the CRT reads its flags; image path and command line)
    let pp = peb + 0x1000;
    w32(peb + 0x10, pp);
    w32(pp, 0x2a0);
    w32(pp + 4, 0x2a0);
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
        w32(at + 4, buf);
    };
    ustr(pp + 0x38, 2 * wpath16.len() as u32, wp);
    ustr(pp + 0x40, cmdlen, wcmd);
    let entry = pe::with(base, |m| m.entry).unwrap();
    pe::init_dlls(cpu);
    let sp = cpu.regs[ESP] - 8;
    w32(sp, api::EXIT_TRAP);
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
