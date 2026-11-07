//! The JIT against the interpreter: the input is an initial register state and up to 1 KB of
//! x86 code; the code is translated (koji::jit::translate_region), the wasm validated and run in
//! wasmtime with the dispatcher (koji::jit::dispatch) and the helpers kura gives the blocks,
//! and the interpreter runs the same code from the same state. Somewhere along the
//! interpreter's run (at most a few thousand steps) the state must be the JIT's: registers,
//! status flags, x87/MMX/SSE and the guest memory window.
//!
//! Guest memory is [LO, HI): code at LO, data above it. A run where the interpreter touches
//! memory outside it (a fault) is not compared (the JIT has no faults: the wasm memory is all
//! 4 GB).
#![no_main]

use koji::flags::STATUS;
use koji::jit::{self, wasm::Module as JitModule};
use koji::{Cpu, Exit, Fault, Memory};
use libfuzzer_sys::fuzz_target;
use std::cell::UnsafeCell;
use std::sync::OnceLock;
use wasmtime::*;

const CPU_AT: u32 = 0x1000;
const HASH_AT: u32 = 0x4000; // 64 entries
const HASH_N: u32 = 64;
const RING_AT: u32 = 0x8000;
const CALLS_AT: u32 = 0x10000;
const PAGES_AT: u32 = 0x20000; // a bit per 4 KB page: 128 KB
const LO: u32 = 0x10_0000;
const HI: u32 = 0x14_0000;
const CODE_MAX: usize = 1024;
const ESP0: u32 = 0x13_0000;
const BUDGET: i32 = 64;
/// interpreter steps before giving up on reaching the JIT's state
const STEPS: usize = 20_000;

/// The data window's starting bytes (code goes over the first part).
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

/// The wasm memory as guest memory for the `step` helper (the JIT's own view: all of it).
struct Wasm<'a>(&'a [UnsafeCell<u8>]);
impl Memory for Wasm<'_> {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        // (only the window: a step outside it is an interpreter fault too, never compared)
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
    // SAFETY: Cpu is plain data (repr(C), any bits valid) and CPU_AT is 16-byte aligned in a
    // mapping that is; one thread
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

/// `status`: compare the status flags too (not at an exception: the JIT keeps them only where
/// later code reads them, which an exception handler is not).
fn same(a: &Cpu, b: &Cpu, status: bool) -> bool {
    let keep = if status { !0 } else { !STATUS };
    // x87: TOP, control and status words, and the registers both call valid (the JIT's
    // register cache does not keep the tags of a stack that underflows exactly: code that
    // reads an empty register is broken anyway)
    let both = !(a.fpu.empty | b.fpu.empty);
    let fp = |c: &Cpu| {
        let f = &c.fpu;
        let regs: Vec<u64> = (0..8).map(|i| if both & 1 << i == 0 { 0 } else { f.regs[i].to_bits() }).collect();
        (regs, f.top & 7, f.cw, f.sw)
    };
    a.regs == b.regs
        && a.eip == b.eip
        && a.flags() & keep == b.flags() & keep
        && a.mm == b.mm
        && a.xmm == b.xmm
        && a.mxcsr == b.mxcsr
        && fp(a) == fp(b)
}

fn show(c: &Cpu) -> String {
    format!(
        "eip {:#x} regs {:x?} flags {:#x} lazy {:x?} xmm {:x?} mm {:x?} mxcsr {:#x} fpu top {} empty {:#x} sw {:#x} regs {:?}",
        c.eip, c.regs, c.flags(), c.lazy, c.xmm, c.mm, c.mxcsr, c.fpu.top, c.fpu.empty, c.fpu.sw, c.fpu.regs
    )
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 32 + 64 + 1 {
        return;
    }
    let (state, code) = data.split_at(32 + 64);
    let code = &code[..code.len().min(CODE_MAX)];
    let word = |i: usize| u32::from_le_bytes(state[4 * i..4 * i + 4].try_into().unwrap());
    let mut cpu = Cpu::default();
    for i in 0..8 {
        // (a flag bit per register: an address in the data window, or the value as it is)
        let v = word(i);
        cpu.regs[i] = if state[95] >> i & 1 != 0 { LO + 0x1_0000 + (v & 0x2_fffc) } else { v };
    }
    cpu.regs[4] = ESP0;
    cpu.eflags = 0x202 | (word(7) & STATUS);
    for i in 0..4 {
        cpu.xmm[i] = u128::from_le_bytes(state[32 + 16 * i..48 + 16 * i].try_into().unwrap());
    }
    for i in 0..4 {
        cpu.mm[i] = u64::from_le_bytes(state[32 + 8 * i..40 + 8 * i].try_into().unwrap()) ^ 0x5555_5555_5555_5555;
    }
    cpu.eip = LO;
    let mut window = Window((LO..HI).map(pattern).collect());
    window.0[..code.len()].copy_from_slice(code);

    // translate
    let mut m = JitModule::default();
    let mut tmem = Window(window.0.clone());
    let end = LO + code.len() as u32;
    let Some(region) = jit::translate_region(&mut m, &mut tmem, &[LO], 16, |a| (LO..end).contains(&a)) else { return };
    let bytes = m.finish();
    let mut features = wasmparser::WasmFeatures::default();
    features.insert(wasmparser::WasmFeatures::THREADS | wasmparser::WasmFeatures::TAIL_CALL);
    if let Err(e) = wasmparser::Validator::new_with_features(features).validate_all(&bytes) {
        panic!("the JIT made invalid wasm for {:02x?}: {e}", code);
    }

    // run it
    let (engine, dispatch) = engine();
    let module = Module::new(engine, &bytes).unwrap();
    // (one memory for all runs: mapping 4 GB each time is most of a run. What a run leaves
    // outside the window is never read: a run touching it is not compared)
    static MEM: OnceLock<SharedMemory> = OnceLock::new();
    let mem = MEM.get_or_init(|| SharedMemory::new(engine, MemoryType::shared(jit::wasm::MEMORY_MIN_PAGES, jit::wasm::MEMORY_MIN_PAGES)).unwrap()).clone();
    for b in &mem.data()[..PAGES_AT as usize + 0x2_0000] {
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
            match koji::step(cpu_in(d, at), &mut Wasm(d)) {
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
            let d = c.data().mem.data();
            let cpu = cpu_in(d, at);
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
                let m = c.data().mem.data();
                jit::x87_out(&mut cpu_in(m, at).fpu, top, info, [a, b, cc, d, e, f, g, h]);
            },
        )
        .unwrap();
    linker.func_wrap("env", "callmiss", |_: u32, _: u32, _: u32| -> u32 { 0 }).unwrap();
    linker.func_wrap("env", "round", |k: u32, a: f64, b: f64, m: u32| -> f64 { koji::round::jit(k, a, b, m) }).unwrap();
    let blocks = linker.instantiate(&mut store, &module).unwrap();
    let n = module.exports().count() as u64;
    table.grow(&mut store, n, Ref::Func(None)).unwrap();
    for i in 0..n {
        let f = blocks.get_func(&mut store, &format!("f{i}")).unwrap();
        table.set(&mut store, i, Ref::Func(Some(f))).unwrap();
    }
    let run = linker.instantiate(&mut store, dispatch).unwrap().get_typed_func::<u32, i32>(&mut store, "f0").unwrap();

    let d = mem.data();
    let put = |a: u32, b: &[u8]| {
        for (i, &v) in b.iter().enumerate() {
            unsafe { *d[a as usize + i].get() = v };
        }
    };
    put(LO, &window.0);
    let mut hash = vec![0u8; 8 * HASH_N as usize];
    for &(a, i) in &region.starts {
        let mut h = jit::dispatch::slot(a, HASH_N - 1);
        while hash[8 * h as usize..][..4] != [0; 4] {
            h = (h + 1) & (HASH_N - 1);
        }
        hash[8 * h as usize..][..4].copy_from_slice(&a.to_le_bytes());
        hash[8 * h as usize + 4..][..4].copy_from_slice(&(i << jit::ENTRY_SHIFT).to_le_bytes());
    }
    put(HASH_AT, &hash);
    let mut jcpu = cpu.clone();
    jcpu.jit_hash = HASH_AT;
    jcpu.jit_mask = HASH_N - 1;
    jcpu.jit_ring = RING_AT;
    jcpu.jit_calls = CALLS_AT;
    jcpu.jit_code_pages = PAGES_AT;
    jcpu.jit_budget = BUDGET;
    *cpu_in(d, CPU_AT) = jcpu;
    let r = match run.call(&mut store, CPU_AT) {
        Ok(r) => r,
        Err(e) => panic!("the JIT's code trapped for {:02x?}: {e:?}", code),
    };
    let got = cpu_in(d, CPU_AT).clone();
    let got_mem: Vec<u8> = (LO..HI).map(|a| unsafe { *d[a as usize].get() }).collect();
    let jit_exit = store.data().exit;

    // the interpreter: somewhere along its run, at the JIT's EIP, the same state but for
    // status flags the JIT left stale because the code overwrites them before reading them
    // (a block left early for the interpreter); from there both states run on in the
    // interpreter and must agree, flags included, once those are written
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
            Exit::Fault(_) => return,
            e => {
                if Some(e) == jit_exit && same(&cpu, &got, false) && window.0 == got_mem {
                    return;
                }
                // (an exit the JIT did not reach: it stopped before, compare where it did)
                break;
            }
        }
    }
    let (at, at_mem) = first.unwrap_or((cpu, window.0));
    let memdiff: Vec<(u32, u8, u8)> =
        at_mem.iter().zip(&got_mem).enumerate().filter(|(_, (a, b))| a != b).take(8).map(|(i, (a, b))| (LO + i as u32, *a, *b)).collect();
    panic!(
        "JIT and interpreter differ for {:02x?}\n  start  {}\n  interp {}\n  jit    {}\n  jit exit {:?}, status {r}\n  memory (addr, interp, jit) {:x?}",
        code,
        show(&start),
        show(&at),
        show(&got),
        jit_exit,
        memdiff
    );
});

/// Runs both states on in the interpreter (up to 64 instructions, or to where either stops):
/// whether they end the same. Status flags that differ and were read make the runs part (a
/// branch, a SETcc, an ADC); ones still different at the end were never read.
fn converge(a: &Cpu, am: &[u8], b: &Cpu, bm: &[u8]) -> bool {
    let (mut a, mut b) = (a.clone(), b.clone());
    let (mut am, mut bm) = (Window(am.to_vec()), Window(bm.to_vec()));
    for _ in 0..64 {
        let (x, y) = (koji::step(&mut a, &mut am), koji::step(&mut b, &mut bm));
        if x != y {
            return false;
        }
        if x != Exit::Continue {
            // (an exception: the handler sees the flags as they are, which the JIT does not
            // keep where nothing after reads them)
            return same(&a, &b, false) && am.0 == bm.0;
        }
    }
    a.eip == b.eip && same(&a, &b, false) && am.0 == bm.0
}
