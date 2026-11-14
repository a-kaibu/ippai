//! The JIT's guest functions done in wasm (koji::jit::Hle) against the guest's own code and a
//! reference: the input is the function's arguments' contents; the function's real bytes go at
//! LO (crates/koji/testdata), the call's frame on the stack, and
//! - the JIT (translate_region at the function: one HLE block and its RET) runs in wasmtime,
//! - the interpreter runs the guest's code to the return address,
//! - a plain Rust implementation computes the result.
//! All three must agree: the result in memory, EAX, ESP and the registers the callee keeps, the
//! guest memory but for the callee's own frame below ESP (where only the message schedule the
//! JIT writes too is compared). With an argument outside the guest's range the JIT must leave
//! for the interpreter at the function, nothing changed.
//!
//! Hle::Sha256 (libtomcrypt sha256_compress, KiriKiri's sigcheck.dll): input = 32 bytes of
//! state, 64 bytes of block, 1 byte of placement (where md and buf go, and whether md is out
//! of range).
#![no_main]

use koji::jit::{self, wasm::Module as JitModule};
use koji::{Cpu, Exit, Fault, Memory};
use libfuzzer_sys::fuzz_target;
use std::cell::UnsafeCell;
use std::sync::OnceLock;
use wasmtime::*;

const CPU_AT: u32 = 0x1000;
const HASH_AT: u32 = 0x4000;
const HASH_N: u32 = 64;
const RING_AT: u32 = 0x8000;
const CALLS_AT: u32 = 0x10000;
const PAGES_AT: u32 = 0x20000;
const LO: u32 = 0x10_0000;
const HI: u32 = 0x14_0000;
const ESP0: u32 = 0x13_0000;
/// where the function returns to (a HLT: not translated, the JIT leaves there)
const RET_TO: u32 = LO + 0x3_f000;
const SHA256: &[u8] = include_bytes!("../../crates/koji/testdata/ltc_sha256_compress_od.bin");

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

fn cpu_in(mem: &[UnsafeCell<u8>], at: u32) -> &mut Cpu {
    // SAFETY: Cpu is plain data (repr(C), any bits valid) and CPU_AT is 16-byte aligned; one thread
    unsafe { &mut *(mem[at as usize].get() as *mut Cpu) }
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

/// Runs the JIT's translation of the function at LO from `cpu` over `window`: the state and
/// window after, or None when the translation has no HLE block (then nothing is compared).
fn run_jit(cpu: &Cpu, window: &[u8]) -> (Cpu, Vec<u8>) {
    let mut m = JitModule::default();
    let mut tmem = Window(window.to_vec());
    assert_eq!(jit::hle_at(&mut tmem, LO), Some(jit::Hle::Sha256), "the function's bytes are not recognized");
    let region = jit::translate_region(&mut m, &mut tmem, &[LO], 16, |a| (LO..LO + SHA256.len() as u32).contains(&a)).expect("translated");
    let code: Vec<(u32, u32)> = region.code.clone();
    // (the translation depends on all the function's bytes: saved regions and page checks)
    assert!(code.contains(&(LO, SHA256.len() as u32)), "code ranges {code:x?}");
    let bytes = m.finish();
    let mut features = wasmparser::WasmFeatures::default();
    features.insert(wasmparser::WasmFeatures::THREADS | wasmparser::WasmFeatures::TAIL_CALL);
    wasmparser::Validator::new_with_features(features).validate_all(&bytes).expect("valid wasm");
    let (engine, dispatch) = engine();
    let module = Module::new(engine, &bytes).unwrap();
    static MEM: OnceLock<SharedMemory> = OnceLock::new();
    let mem = MEM.get_or_init(|| SharedMemory::new(engine, MemoryType::shared(jit::wasm::memory_min_pages(), jit::wasm::memory_min_pages())).unwrap()).clone();
    for b in &mem.data()[..PAGES_AT as usize + 0x2_0000] {
        unsafe { *b.get() = 0 };
    }
    let mut store = Store::new(engine, ());
    let table = Table::new(&mut store, TableType::new(RefType::FUNCREF, 0, None), Ref::Func(None)).unwrap();
    let mut linker = Linker::new(engine);
    linker.define(&store, "env", "memory", mem.clone()).unwrap();
    linker.define(&store, "env", "table", table).unwrap();
    // (the HLE block never steps: any helper call is a failure)
    linker.func_wrap("env", "step", |_: u32| -> i32 { panic!("the HLE block stepped") }).unwrap();
    linker.func_wrap("env", "eflags", |_: u32| -> u32 { 0x202 }).unwrap();
    linker
        .func_wrap("env", "status", |op: u32, a: u32, b: u32, r: u32, cc: u32| -> u32 { koji::lazy::Lazy { op, a: a.into(), b: b.into(), r: r.into(), c: cc }.status() })
        .unwrap();
    linker
        .func_wrap("env", "x87", |_: u32, _: u32, _: u32, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64| {})
        .unwrap();
    linker.func_wrap("env", "callmiss", |_: u32, _: u32, _: u32| -> u32 { 0 }).unwrap();
    let blocks = linker.instantiate(&mut store, &module).unwrap();
    let n = module.exports().count() as u64;
    table.grow(&mut store, n, Ref::Func(None)).unwrap();
    for i in 0..n {
        let f = blocks.get_func(&mut store, &format!("f{i}")).unwrap();
        table.set(&mut store, i, Ref::Func(Some(f))).unwrap();
    }
    let run = linker.instantiate(&mut store, dispatch).unwrap().get_typed_func::<u32, i32>(&mut store, "f0").unwrap();
    let d = mem.data();
    for (i, &v) in window.iter().enumerate() {
        unsafe { *d[LO as usize + i].get() = v };
    }
    let mut hash = vec![0u8; 8 * HASH_N as usize];
    for &(a, i) in &region.starts {
        let mut h = jit::dispatch::slot(a, HASH_N - 1);
        while hash[8 * h as usize..][..4] != [0; 4] {
            h = (h + 1) & (HASH_N - 1);
        }
        hash[8 * h as usize..][..4].copy_from_slice(&a.to_le_bytes());
        hash[8 * h as usize + 4..][..4].copy_from_slice(&(i << jit::ENTRY_SHIFT).to_le_bytes());
    }
    for (i, &v) in hash.iter().enumerate() {
        unsafe { *d[HASH_AT as usize + i].get() = v };
    }
    let mut jcpu = cpu.clone();
    jcpu.jit_hash = HASH_AT;
    jcpu.jit_mask = HASH_N - 1;
    jcpu.jit_ring = RING_AT;
    jcpu.jit_calls = CALLS_AT;
    jcpu.jit_code_pages = PAGES_AT;
    jcpu.jit_budget = 64;
    *cpu_in(d, CPU_AT) = jcpu;
    run.call(&mut store, CPU_AT).expect("the JIT's code trapped");
    let got = cpu_in(d, CPU_AT).clone();
    (got, (LO..HI).map(|a| unsafe { *d[a as usize].get() }).collect())
}

/// SHA-256's compression function (FIPS 180-4 6.2.2) on `state`.
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be,
        0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa,
        0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85,
        0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3,
        0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f,
        0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[4 * i..4 * i + 4].try_into().unwrap());
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
    }
    let mut v = *state;
    for i in 0..64 {
        let [a, b, c, d, e, f, g, h] = v;
        let t1 = h.wrapping_add(e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25)).wrapping_add((e & f) ^ (!e & g)).wrapping_add(K[i]).wrapping_add(w[i]);
        let t2 = (a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22)).wrapping_add((a & b) ^ (a & c) ^ (b & c));
        v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
    }
    for (s, x) in state.iter_mut().zip(v) {
        *s = s.wrapping_add(x);
    }
}

fn word(m: &[u8], a: u32) -> u32 {
    u32::from_le_bytes(m[(a - LO) as usize..][..4].try_into().unwrap())
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 32 + 64 + 1 {
        return;
    }
    let (state, rest) = data.split_at(32);
    let (block, place) = rest.split_at(64);
    let place = place[0];
    // md and buf anywhere in the data area, unaligned too; or md past the guest's range
    let md = LO + 0x2_0000 + (place as u32 & 0x3f) * 3;
    let buf = LO + 0x2_1000 + (place as u32 >> 2 & 0x3f) * 5;
    let out = place & 0xc0 == 0xc0;
    let mut window = Window((LO..HI).map(|a| (a.wrapping_mul(0x9e37_79b1) >> 24) as u8).collect());
    window.0[..SHA256.len()].copy_from_slice(SHA256);
    window.0[(RET_TO - LO) as usize] = 0xf4;
    for (i, &b) in state.iter().enumerate() {
        window.write8(md + 8 + i as u32, b).unwrap();
    }
    for (i, &b) in block.iter().enumerate() {
        window.write8(buf + i as u32, b).unwrap();
    }
    let md_arg = if out { 0x8000_0000 + (place as u32 & 0x3f) } else { md };
    let esp = ESP0 - 12;
    for (k, v) in [RET_TO, md_arg, buf].into_iter().enumerate() {
        for (i, b) in v.to_le_bytes().into_iter().enumerate() {
            window.write8(esp + 4 * k as u32 + i as u32, b).unwrap();
        }
    }
    let mut cpu = Cpu::default();
    cpu.regs = [0x1111_1111, 0x2222_2222, 0x3333_3333, 0x4444_4444, esp, 0x6666_6666, 0x7777_7777, 0x8888_8888];
    cpu.eflags = 0x202;
    cpu.eip = LO;
    let (got, got_mem) = run_jit(&cpu, &window.0);

    if out {
        // (the interpreter would fault on md: the JIT leaves for it at the function, as it was)
        assert_eq!(got.eip, LO, "left at {:#x}", got.eip);
        assert_eq!(got.regs, cpu.regs);
        assert!(got_mem == window.0, "the JIT changed memory with md out of range");
        return;
    }

    // the interpreter, on the guest's own code
    let mut icpu = cpu.clone();
    let mut imem = Window(window.0.clone());
    for _ in 0..100_000 {
        if icpu.eip == RET_TO {
            break;
        }
        match koji::step(&mut icpu, &mut imem) {
            Exit::Continue => {}
            e => panic!("the interpreter stopped: {e:?} at {:#x}", icpu.eip),
        }
    }
    assert_eq!(icpu.eip, RET_TO, "the interpreter did not return");

    // the reference
    let mut want = [0u32; 8];
    for (i, w) in want.iter_mut().enumerate() {
        *w = u32::from_le_bytes(state[4 * i..4 * i + 4].try_into().unwrap());
    }
    compress(&mut want, block.try_into().unwrap());

    assert_eq!(got.eip, RET_TO, "the JIT left at {:#x}", got.eip);
    for (i, &w) in want.iter().enumerate() {
        assert_eq!(word(&imem.0, md + 8 + 4 * i as u32), w, "interpreter state[{i}]");
        assert_eq!(word(&got_mem, md + 8 + 4 * i as u32), w, "JIT state[{i}]");
    }
    // EAX = CRYPT_OK, ESP past the return address, the callee-saved registers kept
    for r in [0, 3, 4, 5, 6, 7] {
        assert_eq!(got.regs[r], icpu.regs[r], "register {r}: jit {:#x} interpreter {:#x}", got.regs[r], icpu.regs[r]);
    }
    assert_eq!(got.regs[0], 0);
    // memory: the same but for the callee's frame below the caller's ESP, where the message
    // schedule (W[64] at ESP-0x104) is the same too
    let frame = (esp - 0x200 - LO) as usize..(esp - LO) as usize;
    let sched = (esp - 0x104 - LO) as usize..(esp - 4 - LO) as usize;
    assert!(got_mem[sched.clone()] == imem.0[sched], "message schedule");
    for (i, (a, b)) in got_mem.iter().zip(&imem.0).enumerate() {
        if !frame.contains(&i) {
            assert_eq!(a, b, "memory at {:#x}", LO + i as u32);
        }
    }
});
