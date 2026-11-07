//! Differential test of koji's 16-bit translation (jit::translate_region16, Win16 code): random
//! 16-bit code run by the interpreter alone and by translated regions (wasmtime, with the
//! interpreter as the `step` import), from the same state; registers, flags and the data, extra
//! and stack segments must match (none overlaps the code: no self-modifying code here). Segments
//! load the real-mode way (base = selector * 16), the
//! code segment's base not 64K-aligned (linear branch targets).
//!
//!   cargo run --release -- [count] [seed]
use iced_x86::{BlockEncoder, BlockEncoderOptions, Code, Instruction, InstructionBlock, MemoryOperand, Register};
use koji::{Cpu, Exit, Fault};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};

mod sst;
use wasmtime::*;

const CPU: u32 = 0x2000;
const CS: u16 = 0x1003;
const DS: u16 = 0x3000;
const ES: u16 = 0x4000;
const SS: u16 = 0x5000;
/// where the code starts and ends (offsets in CS)
const START: u32 = 0x100;
const END: u32 = 0x8000;
const DATA_LO: u32 = 0x30000;
const DATA_HI: u32 = 0x60000;
const MEM: u32 = 0x70000;

fn lin(sel: u16, off: u32) -> u32 {
    ((sel as u32) << 4) + off
}

/// the guest memory the interpreter may touch (the random tests': 0x10000..MEM)
static LO: AtomicU32 = AtomicU32::new(0x10000);
static HI: AtomicU32 = AtomicU32::new(MEM);
/// the exit of the last instruction the `step` import ran that left the region
static LAST_EXIT: std::sync::Mutex<Option<Exit>> = std::sync::Mutex::new(None);

struct Mem(*mut u8);
impl koji::Memory for Mem {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if !(LO.load(Relaxed)..HI.load(Relaxed)).contains(&a) {
            return Err(Fault { addr: a, write: false });
        }
        Ok(unsafe { *self.0.add(a as usize) })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if !(LO.load(Relaxed)..HI.load(Relaxed)).contains(&a) {
            return Err(Fault { addr: a, write: true });
        }
        unsafe { *self.0.add(a as usize) = v };
        Ok(())
    }
}

pub(crate) unsafe fn get_cpu(m: *mut u8) -> Cpu {
    std::ptr::read_unaligned(m.add(CPU as usize) as *const Cpu)
}
pub(crate) unsafe fn put_cpu(m: *mut u8, c: &Cpu) {
    std::ptr::write_unaligned(m.add(CPU as usize) as *mut Cpu, c.clone());
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<T: Copy>(&mut self, v: &[T]) -> T {
        v[self.below(v.len() as u64) as usize]
    }
}

const R16: [Register; 8] = [Register::AX, Register::CX, Register::DX, Register::BX, Register::SP, Register::BP, Register::SI, Register::DI];
const R8: [Register; 8] = [Register::AL, Register::CL, Register::DL, Register::BL, Register::AH, Register::CH, Register::DH, Register::BH];
const R32: [Register; 8] = [Register::EAX, Register::ECX, Register::EDX, Register::EBX, Register::ESP, Register::EBP, Register::ESI, Register::EDI];

/// A 16-bit memory operand: one of the eight forms, a displacement, maybe a segment override.
fn mem(rng: &mut Rng) -> MemoryOperand {
    let (b, x) = rng.pick(&[
        (Register::BX, Register::SI),
        (Register::BX, Register::DI),
        (Register::BP, Register::SI),
        (Register::BP, Register::DI),
        (Register::SI, Register::None),
        (Register::DI, Register::None),
        (Register::BP, Register::None),
        (Register::BX, Register::None),
        (Register::None, Register::None),
    ]);
    let (disp, size) = match rng.below(3) {
        0 if b != Register::None && b != Register::BP || x != Register::None => (0i64, 0u32),
        1 => ((rng.next() as i8) as i64, 1),
        _ => (rng.next() as u16 as i64, 2),
    };
    let (disp, size) = if b == Register::None && x == Register::None {
        (rng.next() as u16 as i64, 2)
    } else {
        (disp, size)
    };
    let seg = rng.pick(&[Register::None, Register::None, Register::None, Register::ES, Register::SS, Register::DS]);
    MemoryOperand::new(b, x, 1, disp, size, false, seg)
}

fn code(name: &str) -> Code {
    // (by name: iced's Code has thousands of variants)
    static MAP: std::sync::OnceLock<HashMap<String, Code>> = std::sync::OnceLock::new();
    *MAP.get_or_init(|| {
        let mut m = HashMap::new();
        for c in Code::values() {
            m.insert(format!("{c:?}"), c);
        }
        m
    })
    .get(name)
    .unwrap_or_else(|| panic!("no code {name}"))
}

const ALU: [&str; 8] = ["Add", "Or", "Adc", "Sbb", "And", "Sub", "Xor", "Cmp"];
const SHIFTS: [&str; 5] = ["Shl", "Shr", "Sar", "Rol", "Ror"];

/// One random instruction (or a few: push/pop pairs).
fn random_ins(rng: &mut Rng, out: &mut Vec<Instruction>) {
    let r16 = |rng: &mut Rng| rng.pick(&R16);
    // (SP kept as it is mostly: pushes and pops then stay in their part of the stack segment)
    let r16w = |rng: &mut Rng| loop {
        let r = rng.pick(&R16);
        if r != Register::SP || rng.below(8) == 0 {
            break r;
        }
    };
    let r8 = |rng: &mut Rng| rng.pick(&R8);
    let i = match rng.below(24) {
        0..=4 => {
            let op = rng.pick(&ALU);
            match rng.below(7) {
                0 => Instruction::with2(code(&format!("{op}_rm16_r16")), mem(rng), r16(rng)),
                1 => Instruction::with2(code(&format!("{op}_r16_rm16")), r16w(rng), mem(rng)),
                2 => Instruction::with2(code(&format!("{op}_rm8_r8")), mem(rng), r8(rng)),
                3 => Instruction::with2(code(&format!("{op}_r16_rm16")), r16w(rng), r16(rng)),
                4 => Instruction::with2(code(&format!("{op}_rm16_imm16")), mem(rng), rng.next() as u16 as u32),
                5 => Instruction::with2(code(&format!("{op}_rm16_imm8")), r16w(rng), rng.next() as i8 as i32),
                _ => Instruction::with2(code(&format!("{op}_rm8_imm8")), r8(rng), rng.next() as u8 as u32),
            }
        }
        5..=8 => match rng.below(7) {
            0 => Instruction::with2(Code::Mov_rm16_r16, mem(rng), r16(rng)),
            1 => Instruction::with2(Code::Mov_r16_rm16, r16w(rng), mem(rng)),
            2 => Instruction::with2(Code::Mov_rm8_r8, mem(rng), r8(rng)),
            3 => Instruction::with2(Code::Mov_r8_rm8, r8(rng), mem(rng)),
            4 => Instruction::with2(Code::Mov_r16_imm16, r16w(rng), rng.next() as u16 as u32),
            5 => Instruction::with2(Code::Mov_rm16_imm16, mem(rng), rng.next() as u16 as u32),
            _ => Instruction::with2(Code::Mov_r16_rm16, r16w(rng), r16(rng)),
        },
        9 => {
            let op = rng.pick(&["Inc", "Dec", "Neg", "Not"]);
            match rng.below(3) {
                0 => Instruction::with1(code(&format!("{op}_rm16")), mem(rng)),
                1 => Instruction::with1(code(&format!("{op}_rm8")), mem(rng)),
                _ => Instruction::with1(code(&format!("{op}_rm16")), r16w(rng)),
            }
        }
        10 => {
            let op = rng.pick(&SHIFTS);
            let m = rng.below(2) == 0;
            let dst = |rng: &mut Rng| if m { Err(mem(rng)) } else { Ok(r16w(rng)) };
            match (rng.below(3), dst(rng)) {
                (0, Ok(r)) => Instruction::with2(code(&format!("{op}_rm16_imm8")), r, 1 + rng.below(15) as u32),
                (0, Err(mo)) => Instruction::with2(code(&format!("{op}_rm16_imm8")), mo, 1 + rng.below(15) as u32),
                (1, Ok(r)) => Instruction::with2(code(&format!("{op}_rm16_CL")), r, Register::CL),
                (1, Err(mo)) => Instruction::with2(code(&format!("{op}_rm8_CL")), mo, Register::CL),
                (_, Ok(r)) => Instruction::with1(code(&format!("{op}_rm16_1")), r),
                (_, Err(mo)) => Instruction::with1(code(&format!("{op}_rm8_1")), mo),
            }
        }
        11 => match rng.below(4) {
            0 => Instruction::with2(Code::Movzx_r16_rm8, r16w(rng), mem(rng)),
            1 => Instruction::with2(Code::Movsx_r16_rm8, r16w(rng), r8(rng)),
            2 => Instruction::with2(Code::Movzx_r32_rm16, rng.pick(&R32), mem(rng)),
            _ => Instruction::with2(Code::Movsx_r32_rm8, rng.pick(&R32), mem(rng)),
        },
        // 32-bit operations (operand-size prefix)
        12 => {
            let op = rng.pick(&ALU);
            let r = rng.pick(&[Register::EAX, Register::ECX, Register::EDX, Register::EBX, Register::ESI, Register::EDI]);
            match rng.below(3) {
                0 => Instruction::with2(code(&format!("{op}_rm32_r32")), mem(rng), r),
                1 => Instruction::with2(code(&format!("{op}_r32_rm32")), r, mem(rng)),
                _ => Instruction::with2(code(&format!("{op}_rm32_imm32")), r, rng.next() as u32),
            }
        }
        13 => match rng.below(2) {
            0 => Instruction::with2(Code::Test_rm16_r16, mem(rng), r16(rng)),
            _ => Instruction::with2(Code::Test_rm8_imm8, r8(rng), rng.next() as u8 as u32),
        },
        14 => Instruction::with2(Code::Xchg_rm16_r16, r16w(rng), r16w(rng)),
        // what the interpreter steps for the translated code
        15 => {
            let r = rng.pick(&[Register::AX, Register::BX, Register::SI, Register::DI, Register::BP, Register::DX]);
            out.push(Instruction::with1(Code::Push_r16, r).unwrap());
            if rng.below(2) == 0 {
                out.push(Instruction::with2(Code::Add_rm16_r16, r, Register::CX).unwrap());
            }
            Instruction::with1(Code::Pop_r16, r16w(rng))
        }
        16 => Instruction::with2(Code::Lea_r16_m, r16w(rng), mem(rng)),
        17 => match rng.below(5) {
            0 => Instruction::with(Code::Cbw),
            1 => Instruction::with(Code::Cwd),
            2 => Instruction::with1(Code::Mul_rm16, mem(rng)).unwrap(),
            3 => Instruction::with2(Code::Imul_r16_rm16, r16w(rng), mem(rng)).unwrap(),
            _ => Instruction::with(Code::Pushfw),
        }
        .pipe(Ok),
        18 => {
            // a segment register reloaded (to the same base or another data segment)
            let s = rng.pick(&[DS, ES]);
            out.push(Instruction::with2(Code::Mov_r16_imm16, Register::AX, s as u32).unwrap());
            let to = rng.pick(&[Register::DS, Register::ES]);
            Instruction::with2(Code::Mov_Sreg_rm16, to, Register::AX)
        }
        19 => {
            // a string op (ES:DI, DS:SI), a few bytes
            out.push(Instruction::with2(Code::Mov_r16_imm16, Register::CX, 1 + rng.below(9) as u32).unwrap());
            let c = rng.pick(&[Code::Movsb_m8_m8, Code::Stosb_m8_AL, Code::Movsw_m16_m16]);
            let mut i = match c {
                Code::Stosb_m8_AL => Instruction::with_stosb(16, iced_x86::RepPrefixKind::Repe),
                Code::Movsw_m16_m16 => Instruction::with_movsw(16, Register::None, iced_x86::RepPrefixKind::Repe),
                _ => Instruction::with_movsb(16, Register::None, iced_x86::RepPrefixKind::Repe),
            };
            if let Ok(x) = &mut i {
                x.set_code(c);
            }
            i
        }
        20 => Instruction::with2(code(&format!("Mov_rm16_r16")), mem(rng), r16(rng)),
        21 => {
            let cc = rng.below(16) as u16;
            let c: Code = unsafe { std::mem::transmute::<u16, Code>(Code::Seto_rm8 as u16 + cc) };
            Instruction::with1(c, r8(rng))
        }
        _ => {
            let op = rng.pick(&ALU);
            Instruction::with2(code(&format!("{op}_rm8_r8")), r8(rng), r8(rng))
        }
    };
    if let Ok(i) = i {
        if i.code() == Code::Pushfw {
            out.push(i);
            out.push(Instruction::with1(Code::Pop_r16, Register::DX).unwrap());
            return;
        }
        out.push(i);
    }
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

const LBASE: u64 = 0x9000;

/// Labels every instruction (its IP: an id for the encoder), adds branches: forward jcc, a
/// counted loop (its counter in SS:[0xfff0]), a call to a subroutine after the final jmp.
fn structure(rng: &mut Rng, body: Vec<Instruction>, sub: Vec<Instruction>) -> Vec<Instruction> {
    let mut id = 0u64;
    let mut label = |mut i: Instruction| {
        id += 1;
        i.set_ip(LBASE + id * 8);
        i
    };
    let mut list: Vec<Instruction> = body.into_iter().map(&mut label).collect();
    let cnt = MemoryOperand::new(Register::None, Register::None, 1, 0xfff0, 2, false, Register::SS);
    let mut lp = None;
    if list.len() > 3 && rng.below(2) == 0 {
        let i = rng.below(list.len() as u64 - 1) as usize;
        let j = i + 1 + rng.below((list.len() - i - 1) as u64) as usize;
        let head = list[i].ip();
        let dec = label(Instruction::with1(Code::Dec_rm16, cnt).unwrap());
        let jne = label(Instruction::with_branch(Code::Jne_rel16, head).unwrap());
        list.insert(j, jne);
        list.insert(j, dec);
        let init = label(Instruction::with2(Code::Mov_rm16_imm16, cnt, 1 + rng.below(3) as u32).unwrap());
        lp = Some((init.ip(), jne.ip()));
        list.insert(i, init);
    }
    for _ in 0..rng.below(4) {
        if list.len() < 3 {
            break;
        }
        let i = rng.below(list.len() as u64 - 1) as usize;
        let j = i + 1 + rng.below((list.len() - i - 1) as u64) as usize;
        let cc = rng.below(16) as u16;
        let code: Code = unsafe { std::mem::transmute::<u16, Code>(Code::Jo_rel16 as u16 + cc * 3) };
        let target = list[j].ip();
        if let Some((a, b)) = lp {
            let pa = list.iter().position(|x| x.ip() == a).unwrap();
            let pb = list.iter().position(|x| x.ip() == b).unwrap();
            if ((i <= pa || i > pb) && j > pa && j <= pb) || j == pb {
                continue;
            }
        }
        let jcc = label(Instruction::with_branch(code, target).unwrap());
        list.insert(i, jcc);
    }
    let sub: Vec<Instruction> = sub.into_iter().map(&mut label).collect();
    if let Some(first) = sub.first().map(|i| i.ip()) {
        let call = label(Instruction::with_branch(Code::Call_rel16, first).unwrap());
        let mut at = rng.below(list.len() as u64 + 1) as usize;
        if let Some((_, b)) = lp {
            if at == list.iter().position(|x| x.ip() == b).unwrap() {
                at += 1;
            }
        }
        list.insert(at, call);
    }
    let mut jmp = Instruction::with_branch(Code::Jmp_rel16, END as u64).unwrap();
    jmp.set_ip(1); // (kept by the shrinker)
    list.push(jmp);
    let has_sub = !sub.is_empty();
    list.extend(sub);
    if has_sub {
        list.push(label(Instruction::with(Code::Retnw)));
    }
    list
}

fn encode(list: &[Instruction]) -> Option<Vec<u8>> {
    for i in list {
        if i.op0_kind() == iced_x86::OpKind::NearBranch16 {
            let t = i.near_branch16() as u64;
            if t >= LBASE && !list.iter().any(|j| j.ip() == t) {
                return None;
            }
        }
    }
    let block = InstructionBlock::new(list, START as u64);
    let code = match BlockEncoder::encode(16, block, BlockEncoderOptions::NONE) {
        Ok(r) => r.code_buffer,
        Err(e) => {
            if std::env::var("JLAYOUT").is_ok() {
                eprintln!("encode: {e}");
            }
            return None;
        }
    };
    (START as usize + code.len() < END as usize).then_some(code)
}

const HASH: u32 = 0x8000;
const HASH_N: u32 = 2048;
const RING: u32 = 0xc000;

struct Jit {
    engine: Engine,
    store: Store<()>,
    linker: Linker<()>,
    table: Table,
    /// linear eip -> (function, block, table index)
    entries: HashMap<u32, (Func, u32, u32)>,
    /// small regions: each block on its own
    single: bool,
}

fn to_linear(c: &mut Cpu) {
    c.eip = c.pc16();
}
fn from_linear(c: &mut Cpu) {
    c.eip = c.eip.wrapping_sub(c.sbase[1]) & 0xffff;
}

impl Jit {
    fn new(engine: &Engine, memory: &SharedMemory, base: usize, single: bool) -> Jit {
        let mut store = Store::new(engine, ());
        let mut linker = Linker::new(engine);
        linker.define(&store, "env", "memory", memory.clone()).unwrap();
        let table = Table::new(&mut store, TableType::new(RefType::FUNCREF, 0, None), Ref::Func(None)).unwrap();
        linker.define(&store, "env", "table", table).unwrap();
        linker
            .func_wrap("env", "step", move |cpu: i32| -> i32 {
                let m = base as *mut u8;
                assert_eq!(cpu as u32, CPU);
                let mut c = unsafe { get_cpu(m) };
                from_linear(&mut c);
                let r = koji::step(&mut c, &mut Mem(m));
                to_linear(&mut c);
                unsafe { put_cpu(m, &c) };
                match r {
                    Exit::Continue => 0,
                    e => {
                        *LAST_EXIT.lock().unwrap() = Some(e);
                        if std::env::var("JLAYOUT").is_ok() {
                            eprintln!("step exit {e:?} at {:#x}", c.eip);
                        }
                        2
                    }
                }
            })
            .unwrap();
        linker
            .func_wrap("env", "eflags", move |_cpu: i32| -> i32 {
                let m = base as *mut u8;
                let mut c = unsafe { get_cpu(m) };
                c.materialize();
                unsafe { put_cpu(m, &c) };
                c.eflags as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "status", |op: i32, a: i32, b: i32, r: i32, c: i32| -> i32 {
                koji::lazy::Lazy {
                    op: op as u32,
                    a: a as u32 as u64,
                    b: b as u32 as u64,
                    r: r as u32 as u64,
                    c: c as u32,
                }
                .status() as i32
            })
            .unwrap();
        linker.func_wrap("env", "callmiss", |_: i32, _: i32, _: i32| -> i32 { 0 }).unwrap();
        linker
            .func_wrap("env", "x87", |_: i32, _: i32, _: i32, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64| {})
            .unwrap();
        unsafe { std::ptr::write_bytes((base + HASH as usize) as *mut u8, 0, (HASH_N * 8) as usize + 0x1000) };
        Jit {
            engine: engine.clone(),
            store,
            linker,
            table,
            entries: HashMap::new(),
            single,
        }
    }

    fn chain(&self, base: usize) {
        let m = base as *mut u8;
        let mut c = unsafe { get_cpu(m) };
        c.jit_hash = HASH;
        c.jit_mask = HASH_N - 1;
        c.jit_ring = RING;
        c.jit_budget = 1 << 16;
        unsafe { put_cpu(m, &c) };
    }

    fn translate(&mut self, base: usize, eip: u32, cs: u32) -> Result<(), String> {
        let mut m = koji::jit::wasm::Module::default();
        let single = self.single;
        let end = cs + END;
        let region = koji::jit::translate_region16(&mut m, &mut Mem(base as *mut u8), eip, cs, if single { 1 } else { 64 }, |a| a != end && (!single || a == eip)).ok_or("refused")?;
        if std::env::var("JLAYOUT").is_ok() {
            eprintln!("region {eip:#x}:\n{}", region.layout);
        }
        let bytes = m.finish();
        let module = Module::new(&self.engine, &bytes).map_err(|e| {
            std::fs::write("/tmp/jit16-bad.wasm", &bytes).ok();
            format!("invalid module (in /tmp/jit16-bad.wasm): {e:?}")
        })?;
        let inst = self.linker.instantiate(&mut self.store, &module).map_err(|e| format!("{e}"))?;
        let f = inst.get_func(&mut self.store, "f0").unwrap();
        let idx = self.table.grow(&mut self.store, 1, Ref::Func(Some(f))).unwrap() as u32;
        for (a, k) in region.starts {
            self.entries.insert(a, (f, k, idx));
            let mut h = koji::jit::dispatch::slot(a, HASH_N - 1);
            loop {
                let e = (base + (HASH + 8 * h) as usize) as *mut u32;
                let key = unsafe { e.read() };
                if key == 0 || key == a {
                    unsafe {
                        e.write(a);
                        e.add(1).write(idx | k << koji::jit::ENTRY_SHIFT);
                    }
                    break;
                }
                h = (h + 1) & (HASH_N - 1);
            }
        }
        Ok(())
    }

    /// One region from CS:IP (translated first): its status (0 left, 1 to step, 2 a stepped
    /// instruction's exit, in LAST_EXIT), the Cpu back at CS:IP; Err("refused") when there is
    /// nothing to translate.
    fn once(&mut self, base: usize) -> Result<i32, String> {
        let m = base as *mut u8;
        let mut c = unsafe { get_cpu(m) };
        to_linear(&mut c);
        let eip = c.eip;
        unsafe { put_cpu(m, &c) };
        if !self.entries.contains_key(&eip) {
            self.translate(base, eip, c.sbase[1])?;
        }
        let (f, k, idx) = self.entries[&eip];
        self.chain(base);
        let mut params = vec![Val::I32(0); koji::jit::N_PARAMS as usize];
        params[0] = Val::I32(CPU as i32);
        params[1] = Val::I32((idx | k << koji::jit::ENTRY_SHIFT) as i32);
        for i in 0..8 {
            params[2 + i] = Val::I32(c.regs[i] as i32);
        }
        params[10] = Val::I32(-1);
        let last = koji::jit::N_PARAMS as usize - 1;
        params[last] = Val::I32(1 << 20);
        let mut results = vec![Val::I32(0); 1];
        f.call(&mut self.store, &params, &mut results).map_err(|e| format!("trap: {e:?}"))?;
        let mut c = unsafe { get_cpu(m) };
        from_linear(&mut c);
        unsafe { put_cpu(m, &c) };
        Ok(results[0].unwrap_i32())
    }

    /// Runs from CS:IP until END: regions, the interpreter for what they leave to it.
    fn run(&mut self, base: usize) -> Result<usize, String> {
        let m = base as *mut u8;
        let mut regions = 0;
        for _ in 0..20_000 {
            let mut c = unsafe { get_cpu(m) };
            if c.eip == END {
                return Ok(regions);
            }
            if std::env::var("JTRACE").is_ok() {
                let mut d = c.clone();
                d.materialize();
                eprintln!("J {:x} {:x?} {:x}", d.eip, d.regs, d.eflags & 0x8d5);
            }
            to_linear(&mut c);
            let eip = c.eip;
            unsafe { put_cpu(m, &c) };
            if !self.entries.contains_key(&eip) {
                match self.translate(base, eip, c.sbase[1]) {
                    // (nothing there to translate, as kura's cpu_run: the interpreter steps it)
                    Err(e) if e == "refused" => {
                        let mut c = unsafe { get_cpu(m) };
                        from_linear(&mut c);
                        let r = koji::step(&mut c, &mut Mem(m));
                        unsafe { put_cpu(m, &c) };
                        if r != Exit::Continue {
                            return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
                        }
                        continue;
                    }
                    r => r?,
                }
            }
            let (f, k, idx) = self.entries[&eip];
            self.chain(base);
            let mut params = vec![Val::I32(0); koji::jit::N_PARAMS as usize];
            params[0] = Val::I32(CPU as i32);
            params[1] = Val::I32((idx | k << koji::jit::ENTRY_SHIFT) as i32);
            let c = unsafe { get_cpu(m) };
            for i in 0..8 {
                params[2 + i] = Val::I32(c.regs[i] as i32);
            }
            params[10] = Val::I32(-1);
            let last = koji::jit::N_PARAMS as usize - 1;
            params[last] = Val::I32(1 << 20);
            let mut results = vec![Val::I32(0); 1];
            f.call(&mut self.store, &params, &mut results).map_err(|e| format!("trap: {e:?}"))?;
            regions += 1;
            if std::env::var("JTRACE").is_ok() {
                let mut d = unsafe { get_cpu(m) };
                let raw = (d.eip, d.eflags);
                d.materialize();
                eprintln!("R {:x} status {} raw {:x?} flags {:x}", d.eip, results[0].unwrap_i32(), raw, d.eflags & 0x8d5);
            }
            let mut c = unsafe { get_cpu(m) };
            from_linear(&mut c);
            unsafe { put_cpu(m, &c) };
            match results[0].unwrap_i32() {
                0 => {}
                1 => {
                    let mut c = unsafe { get_cpu(m) };
                    if c.eip == END {
                        return Ok(regions);
                    }
                    let r = koji::step(&mut c, &mut Mem(m));
                    unsafe { put_cpu(m, &c) };
                    if r != Exit::Continue {
                        return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
                    }
                }
                // (the hlt at END)
                2 if unsafe { get_cpu(m) }.eip == END => return Ok(regions),
                s => return Err(format!("status {s} at {:#x}", unsafe { get_cpu(m) }.eip)),
            }
        }
        Err("no end".into())
    }
}

fn interp(base: usize) -> Result<(), String> {
    let m = base as *mut u8;
    let mut c = unsafe { get_cpu(m) };
    for _ in 0..1_000_000 {
        if c.eip == END {
            unsafe { put_cpu(m, &c) };
            return Ok(());
        }
        if std::env::var("JTRACE").is_ok() {
            eprintln!("I {:x} {:x?} {:x}", c.eip, c.regs, c.eflags & 0x8d5);
        }
        let r = koji::step(&mut c, &mut Mem(m));
        if r != Exit::Continue {
            return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
        }
    }
    Err("no end".into())
}

fn initial(rng: &mut Rng) -> Cpu {
    let mut c = Cpu {
        mode16: 1,
        ldt: koji::LDT_REAL,
        eip: START,
        ..Cpu::default()
    };
    for i in 0..8 {
        c.regs[i] = match rng.below(3) {
            0 => rng.below(256) as u32,
            1 => rng.next() as u16 as u32,
            _ => rng.next() as u32,
        };
    }
    c.regs[4] = 0x8000 | (rng.next() as u32 & 0xffff_0000); // sp (garbage above)
    for (k, sel) in [ES, CS, SS, DS, DS, DS].into_iter().enumerate() {
        c.sreg[k] = sel;
        c.sbase[k] = (sel as u32) << 4;
    }
    c.eflags = 0x202 | (rng.next() as u32 & 0x8d5);
    c
}

fn run_case(engine: &Engine, memory: &SharedMemory, base: usize, list: &[Instruction], data: &[u8], cpu0: &Cpu, single: bool) -> (Vec<String>, Vec<u8>) {
    let m = base as *mut u8;
    let Some(code) = encode(list) else { return (Vec::new(), Vec::new()) };
    let setup = |cpu: &Cpu| unsafe {
        std::ptr::write_bytes(m.add(0x10000), 0, (MEM - 0x10000) as usize);
        std::ptr::copy_nonoverlapping(data.as_ptr(), m.add(DATA_LO as usize), data.len());
        std::ptr::copy_nonoverlapping(code.as_ptr(), m.add(lin(CS, START) as usize), code.len());
        // (hlt at END: a run of straight-line code a ret wandered into stops there in a region too)
        m.add(lin(CS, END) as usize).write(0xf4);
        put_cpu(m, cpu);
    };
    let segs = || unsafe { std::slice::from_raw_parts(m.add(DATA_LO as usize), (DATA_HI - DATA_LO) as usize).to_vec() };
    setup(cpu0);
    let want = interp(base);
    // (no end, or an access outside the harness's memory, which the JIT's wider bounds allow)
    if want.as_ref().is_err_and(|e| e == "no end" || e.contains("Fault")) {
        return (Vec::new(), code);
    }
    let (mut want_cpu, want_data) = (unsafe { get_cpu(m) }, segs());
    setup(cpu0);
    let mut jit = Jit::new(engine, memory, base, single);
    let got = jit.run(base);
    let (mut got_cpu, got_data) = (unsafe { get_cpu(m) }, segs());
    want_cpu.materialize();
    got_cpu.materialize();
    let mut diffs = Vec::new();
    if want.is_err() && got.is_err() {
        return (diffs, code);
    }
    if want.is_ok() != got.is_ok() {
        diffs.push(format!("runs: interp {want:?} jit {got:?}"));
    }
    for i in 0..8 {
        if want_cpu.regs[i] != got_cpu.regs[i] {
            diffs.push(format!("{:?}: want {:#x} got {:#x}", R32[i], want_cpu.regs[i], got_cpu.regs[i]));
        }
    }
    if want_cpu.eflags & 0x8d5 != got_cpu.eflags & 0x8d5 {
        diffs.push(format!("flags: want {:#x} got {:#x}", want_cpu.eflags & 0x8d5, got_cpu.eflags & 0x8d5));
    }
    if want_cpu.sbase != got_cpu.sbase || want_cpu.sreg != got_cpu.sreg {
        diffs.push(format!("segments: want {:x?} got {:x?}", want_cpu.sreg, got_cpu.sreg));
    }
    if want_data != got_data {
        let i = want_data.iter().zip(&got_data).position(|(a, b)| a != b).unwrap();
        diffs.push(format!("memory differs at {:#x}", DATA_LO as usize + i));
    }
    (diffs, code)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == "sst") {
        return sst::main(&args[2..]);
    }
    if std::env::var("JLAYOUT").is_ok() {
        koji::jit::LAYOUT.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let count: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500);
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut config = Config::new();
    config.wasm_threads(true);
    let engine = Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, MemoryType::shared(koji::jit::wasm::memory_min_pages(), koji::jit::wasm::memory_min_pages())).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let mut fails = 0;
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    for t in 0..count {
        let single = t % 2 == 1;
        let mut list = Vec::new();
        let n = 5 + rng.below(40) as usize;
        while list.len() < n {
            random_ins(&mut rng, &mut list);
        }
        let mut sub = Vec::new();
        if rng.below(3) == 0 {
            let k = 1 + rng.below(6) as usize;
            while sub.len() < k {
                random_ins(&mut rng, &mut sub);
            }
        }
        let mut list = structure(&mut rng, list, sub);
        let data: Vec<u8> = (0..DATA_HI - DATA_LO).map(|_| rng.next() as u8).collect();
        let cpu0 = initial(&mut rng);
        if std::env::var("JONLY").is_ok_and(|v| v != t.to_string()) {
            continue;
        }
        if std::env::var("JPROG").is_ok() {
            eprintln!("test {t}");
        }
        let (diffs, _) = run_case(&engine, &memory, base, &list, &data, &cpu0, single);
        if !diffs.is_empty() {
            let mut i = 0;
            while i < list.len() {
                if list[i].ip() == 1 || list[i].code() == Code::Retnw {
                    i += 1;
                    continue;
                }
                let mut l2 = list.clone();
                l2.remove(i);
                if !run_case(&engine, &memory, base, &l2, &data, &cpu0, single).0.is_empty() {
                    list = l2;
                } else {
                    i += 1;
                }
            }
        }
        let (diffs, code) = run_case(&engine, &memory, base, &list, &data, &cpu0, single);
        if !diffs.is_empty() {
            fails += 1;
            if fails <= 10 {
                println!("test {t} (single {single}): {}", diffs.join("; "));
                println!("    init: {:x?} flags {:#x}", cpu0.regs, cpu0.eflags);
                let dec = iced_x86::Decoder::with_ip(16, &code, START as u64, iced_x86::DecoderOptions::NONE);
                let mut f = iced_x86::FastFormatter::new();
                let mut s = String::new();
                for ins in dec {
                    s.clear();
                    f.format(&ins, &mut s);
                    println!("    {:#06x} {s}", ins.ip());
                }
            }
        }
    }
    println!("{count} tests, {fails} failed");
    if fails > 0 {
        std::process::exit(1);
    }
}
