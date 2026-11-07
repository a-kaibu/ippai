//! The long-mode JIT (koji::jit::long) against the interpreter, across regions: the input is
//! a register state, a region size and up to 1 KB of x86-64 code. The code runs as kura runs
//! it: the dispatcher (koji::jit::dispatch) enters regions through the hash table, regions of
//! one to a few blocks each in a module of its own (so that nearly every jump, call and return
//! is a hop between regions: Gen::chain's tail call, with only the registers each region names
//! moving between its locals and the Cpu), a region is translated where the dispatcher found
//! none, and the interpreter steps what a region left to it. The interpreter runs the same code
//! from the same state; somewhere along its run the state must be the JIT's: the sixteen
//! registers, RIP, the status flags, SSE and the guest memory window.
//!
//! Guest memory is [LO, HI): code at LO, data above it. A run where the interpreter touches
//! memory outside it (a fault) is not compared.
#![no_main]

use koji::flags::STATUS;
use koji::jit::{self, wasm::Module as JitModule};
use koji::{Cpu, Exit, Fault, Memory};
use libfuzzer_sys::fuzz_target;
use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::OnceLock;
use wasmtime::*;

const CPU_AT: u32 = 0x1000;
const HASH_AT: u32 = 0x4000; // 512 entries
const HASH_N: u32 = 512;
const RING_AT: u32 = 0x8000;
const LO: u32 = 0x10_0000;
const HI: u32 = 0x14_0000;
const CODE_MAX: usize = 1024;
const RSP0: u32 = 0x13_0000;
/// hops (and backward jumps) per dispatcher call
const BUDGET: i32 = 64;
/// dispatcher calls (each one ends at a miss, an interpreter step or the budget)
const ROUNDS: usize = 64;
/// interpreter steps before giving up on reaching the JIT's state
const STEPS: usize = 20_000;

fn pattern(a: u32) -> u8 {
    (a.wrapping_mul(0x9e37_79b1) >> 24) as u8
}

struct Window(Vec<u8>);
impl Memory for Window {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if (LO..HI).contains(&a) { Ok(self.0[(a - LO) as usize]) } else { Err(Fault { addr: a, write: false }) }
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if (LO..HI).contains(&a) {
            self.0[(a - LO) as usize] = v;
            Ok(())
        } else {
            Err(Fault { addr: a, write: true })
        }
    }
}

/// The wasm memory's window as guest memory (the `step` helper's and the translator's view).
struct Wasm<'a>(&'a [UnsafeCell<u8>]);
impl Memory for Wasm<'_> {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if (LO..HI).contains(&a) { Ok(unsafe { *self.0[a as usize].get() }) } else { Err(Fault { addr: a, write: false }) }
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if (LO..HI).contains(&a) {
            unsafe { *self.0[a as usize].get() = v };
            Ok(())
        } else {
            Err(Fault { addr: a, write: true })
        }
    }
}

fn cpu_in(mem: &[UnsafeCell<u8>], at: u32) -> &mut Cpu {
    // SAFETY: Cpu is plain data (repr(C), any bits valid) and CPU_AT is aligned; one thread
    unsafe { &mut *(mem[at as usize].get() as *mut Cpu) }
}

struct Host {
    mem: SharedMemory,
    exit: Option<Exit>,
}

fn engine() -> &'static (Engine, Module) {
    static E: OnceLock<(Engine, Module)> = OnceLock::new();
    E.get_or_init(|| {
        let mut c = Config::new();
        c.wasm_threads(true).shared_memory(true).wasm_simd(true).wasm_tail_call(true).cranelift_opt_level(OptLevel::None);
        let e = Engine::new(&c).unwrap();
        let d = Module::new(&e, jit::dispatch::module()).unwrap();
        (e, d)
    })
}

fn same(a: &Cpu, b: &Cpu, status: bool) -> bool {
    let keep = if status { !0 } else { !STATUS };
    (0..16).all(|i| a.gpr(i) == b.gpr(i))
        && a.eip == b.eip
        && a.rip_hi == b.rip_hi
        && a.flags() & keep == b.flags() & keep
        && a.xmm == b.xmm
        && a.mxcsr == b.mxcsr
}

fn show(c: &Cpu) -> String {
    let r: Vec<u64> = (0..16).map(|i| c.gpr(i)).collect();
    format!("rip {:#x}:{:#x} regs {:x?} flags {:#x} xmm {:x?} mxcsr {:#x}", c.rip_hi, c.eip, r, c.flags(), c.xmm, c.mxcsr)
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 128 + 2 + 1 {
        return;
    }
    let (state, code) = data.split_at(128 + 2);
    let code = &code[..code.len().min(CODE_MAX)];
    let quad = |i: usize| u64::from_le_bytes(state[8 * i..8 * i + 8].try_into().unwrap());
    let mut cpu = Cpu { mode64: 1, rip_hi: 0, eip: LO, ..Cpu::default() };
    for i in 0..16 {
        // (a flag bit per register: an address in the data window, or the value as it is)
        let v = quad(i);
        let addr = state[128] as u16 | (state[129] as u16) << 8;
        cpu.set_gpr(i, if addr >> i & 1 != 0 { (LO + 0x1_0000 + (v as u32 & 0x2_fff8)) as u64 } else { v });
    }
    cpu.set_gpr(4, RSP0 as u64);
    cpu.eflags = 0x202 | (state[0] as u32 & STATUS);
    for i in 0..8 {
        cpu.xmm[i] = (quad(i) as u128) << 64 | quad(15 - i) as u128;
    }
    // regions of 1..4 blocks (koji::jit::long::DEBUG_BLOCKS), all sixteen registers moved at
    // every entry and exit or only the named ones (DEBUG_ALL_REGS: both must agree with the
    // interpreter)
    let blocks = 1 + (state[1] & 3) as usize;
    jit::long::DEBUG_BLOCKS.store(blocks, Relaxed);
    jit::long::DEBUG_ALL_REGS.store(state[2] & 0x80 != 0, Relaxed);
    let mut window = Window((LO..HI).map(pattern).collect());
    window.0[..code.len()].copy_from_slice(code);
    let end = LO + code.len() as u32;

    let (engine, dispatch) = engine();
    static MEM: OnceLock<SharedMemory> = OnceLock::new();
    let mem = MEM.get_or_init(|| SharedMemory::new(engine, MemoryType::shared(jit::wasm::memory_min_pages(), jit::wasm::memory_min_pages())).unwrap()).clone();
    for b in &mem.data()[..0x1_0000] {
        unsafe { *b.get() = 0 };
    }
    let mut store = Store::new(engine, Host { mem: mem.clone(), exit: None });
    let table = Table::new(&mut store, TableType::new(RefType::FUNCREF, 0, None), Ref::Func(None)).unwrap();
    let mut linker = Linker::new(engine);
    linker.define(&store, "env", "memory", mem.clone()).unwrap();
    linker.define(&store, "env", "table", table).unwrap();
    linker
        .func_wrap("env", "step", |mut c: Caller<'_, Host>, at: u32| -> i32 {
            let mem = c.data().mem.clone();
            let d = mem.data();
            let cpu = cpu_in(d, at);
            match koji::step(cpu, &mut Wasm(d)) {
                Exit::Continue if !koji::jit::sse_ok(cpu.mxcsr) => 2,
                Exit::Continue => 0,
                e => {
                    c.data_mut().exit = Some(e);
                    2
                }
            }
        })
        .unwrap();
    linker
        .func_wrap("env", "eflags", |c: Caller<'_, Host>, at: u32| -> u32 {
            let cpu = cpu_in(c.data().mem.data(), at);
            cpu.materialize();
            cpu.eflags
        })
        .unwrap();
    linker
        .func_wrap("env", "status", |op: u32, a: u32, b: u32, r: u32, cc: u32| -> u32 { koji::lazy::Lazy { op, a: a.into(), b: b.into(), r: r.into(), c: cc }.status() })
        .unwrap();
    linker
        .func_wrap(
            "env",
            "x87",
            |c: Caller<'_, Host>, at: u32, top: u32, info: u32, a: f64, b: f64, cc: f64, d: f64, e: f64, f: f64, g: f64, h: f64| {
                jit::x87_out(&mut cpu_in(c.data().mem.data(), at).fpu, top, info, [a, b, cc, d, e, f, g, h]);
            },
        )
        .unwrap();
    let run = linker.instantiate(&mut store, dispatch).unwrap().get_typed_func::<u32, i32>(&mut store, "f0").unwrap();

    let d = mem.data();
    let put = |a: u32, b: &[u8]| {
        for (i, &v) in b.iter().enumerate() {
            unsafe { *d[a as usize + i].get() = v };
        }
    };
    put(LO, &window.0);
    let mut jcpu = cpu.clone();
    jcpu.jit_hash = HASH_AT;
    jcpu.jit_mask = HASH_N - 1;
    jcpu.jit_ring = RING_AT;
    *cpu_in(d, CPU_AT) = jcpu;
    // eip -> table index | block << ENTRY_SHIFT, as the hash table holds it
    let mut entries: HashMap<u32, u32> = HashMap::new();
    let mut tried: std::collections::HashSet<u32> = Default::default();
    let mut features = wasmparser::WasmFeatures::default();
    features.insert(wasmparser::WasmFeatures::THREADS | wasmparser::WasmFeatures::TAIL_CALL | wasmparser::WasmFeatures::SIMD);
    for _ in 0..ROUNDS {
        let c = cpu_in(d, CPU_AT);
        if store.data().exit.is_some() || c.rip_hi != 0 || !(LO..end).contains(&c.eip) {
            break;
        }
        let eip = c.eip;
        if !entries.contains_key(&eip) {
            if !tried.insert(eip) {
                // (refused before: the interpreter's)
                match koji::step(c, &mut Wasm(d)) {
                    Exit::Continue => continue,
                    e => {
                        store.data_mut().exit = Some(e);
                        break;
                    }
                }
            }
            let mut m = JitModule::default();
            let Some(region) = jit::long::translate_region64(&mut m, &mut Wasm(d), &[eip as u64], (HI, HI + 16), |a, _| (LO..end).contains(&a)) else { continue };
            let bytes = m.finish();
            if let Err(e) = wasmparser::Validator::new_with_features(features).validate_all(&bytes) {
                panic!("the JIT made invalid wasm for {:02x?} at {eip:#x}: {e}", code);
            }
            let module = Module::new(engine, &bytes).unwrap();
            let inst = linker.instantiate(&mut store, &module).unwrap();
            let f = inst.get_func(&mut store, "f0").unwrap();
            let idx = table.grow(&mut store, 1, Ref::Func(Some(f))).unwrap() as u32;
            for (a, k) in region.starts {
                let v = idx | k << jit::ENTRY_SHIFT;
                entries.insert(a, v);
                // (linear probing, a newer region taking over a block)
                let mut h = jit::dispatch::slot(a, HASH_N - 1);
                loop {
                    let e = HASH_AT + 8 * h;
                    let key = u32::from_le_bytes(std::array::from_fn(|i| unsafe { *d[(e + i as u32) as usize].get() }));
                    if key == 0 || key == a {
                        put(e, &a.to_le_bytes());
                        put(e + 4, &v.to_le_bytes());
                        break;
                    }
                    h = (h + 1) & (HASH_N - 1);
                }
            }
        }
        cpu_in(d, CPU_AT).jit_budget = BUDGET;
        let r = match run.call(&mut store, CPU_AT) {
            Ok(r) => r,
            Err(e) => panic!("the JIT's code trapped for {:02x?}: {e:?}", code),
        };
        match r {
            0 => break, // budget
            1 => {
                // a miss (translated at the top) or an instruction left to the interpreter
                let c = cpu_in(d, CPU_AT);
                if entries.contains_key(&c.eip) {
                    match koji::step(c, &mut Wasm(d)) {
                        Exit::Continue => {}
                        e => {
                            store.data_mut().exit = Some(e);
                            break;
                        }
                    }
                }
            }
            _ => break,
        }
    }
    let mut got = cpu_in(d, CPU_AT).clone();
    got.materialize();
    let got_mem: Vec<u8> = (LO..HI).map(|a| unsafe { *d[a as usize].get() }).collect();
    let jit_exit = store.data().exit;

    // the interpreter: somewhere along its run, at the JIT's RIP, the same state but for
    // status flags the JIT left stale because the code overwrites them before reading them;
    // from there both run on in the interpreter and must agree
    let start = cpu.clone();
    let mut first: Option<(Cpu, Vec<u8>)> = None;
    for _ in 0..STEPS {
        if cpu.eip == got.eip && same(&cpu, &got, false) && window.0 == got_mem {
            if jit_exit.is_some() || converge(&cpu, &window.0, &got, &got_mem) {
                return;
            }
        }
        if cpu.eip == got.eip && first.is_none() {
            first = Some((cpu.clone(), window.0.clone()));
        }
        match koji::step(&mut cpu, &mut window) {
            Exit::Continue => {}
            // (a fault outside the window before the JIT's RIP, where the JIT's code reads and
            // writes the wasm memory as it is: not compared; one after the interpreter passed
            // the JIT's RIP without its state: they differ. A fault the JIT's interpreter steps
            // met too, at the same state, is the check at the top.)
            Exit::Fault(_) if first.is_none() => return,
            Exit::Fault(_) => break,
            e => {
                if Some(e) == jit_exit && same(&cpu, &got, false) && window.0 == got_mem {
                    return;
                }
                break;
            }
        }
    }
    // (code that wrote over its own bytes: the long-mode JIT has no check for it, unlike
    // koji::jit::packed's, and runs what it translated)
    if window.0[..code.len()] != *code {
        return;
    }
    let (at, at_mem) = first.unwrap_or((cpu, window.0));
    let memdiff: Vec<(u32, u8, u8)> =
        at_mem.iter().zip(&got_mem).enumerate().filter(|(_, (a, b))| a != b).take(8).map(|(i, (a, b))| (LO + i as u32, *a, *b)).collect();
    panic!(
        "JIT and interpreter differ for {:02x?} (regions of {blocks} blocks)\n  start  {}\n  interp {}\n  jit    {}\n  jit exit {:?}\n  memory (addr, interp, jit) {:x?}",
        code,
        show(&start),
        show(&at),
        show(&got),
        jit_exit,
        memdiff
    );
});

/// Runs both states on in the interpreter (up to 64 instructions, or to where either stops):
/// whether they end the same. Status flags that differ and were read make the runs part.
fn converge(a: &Cpu, am: &[u8], b: &Cpu, bm: &[u8]) -> bool {
    let (mut a, mut b) = (a.clone(), b.clone());
    let (mut am, mut bm) = (Window(am.to_vec()), Window(bm.to_vec()));
    for _ in 0..64 {
        let (x, y) = (koji::step(&mut a, &mut am), koji::step(&mut b, &mut bm));
        if x != y {
            return false;
        }
        if x != Exit::Continue {
            return same(&a, &b, false) && am.0 == bm.0;
        }
    }
    a.eip == b.eip && same(&a, &b, false) && am.0 == bm.0
}
