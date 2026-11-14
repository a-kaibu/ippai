//! Differential test of the long-mode JIT (koji jit/long.rs): random x86-64 code (and a few
//! fixed programs) run by the interpreter alone and by translated regions (wasmtime, with the
//! interpreter as the `step` import), from the same state; registers, flags and memory must
//! match.
//!
//!   cargo run --release -- [count] [seed]
mod native;
use iced_x86::{BlockEncoder, BlockEncoderOptions, Code, Instruction, InstructionBlock, MemoryOperand, Register};
use std::collections::HashMap;
use std::sync::atomic::Ordering::Relaxed;
use koji::{Cpu, Exit, Fault};
use wasmtime::*;

const CPU: u32 = 0x2000;
const CODE: u32 = 0x10_0000;
const END: u32 = 0x18_0000;
const DATA: u32 = 0x20_0000;
const DATA_LEN: u32 = 0x1000;
const STACK: u32 = 0x30_0000;
const MEM: usize = 0x40_0000;
/// accessible guest memory (bigger for `exe`)
static LIMIT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(MEM as u32);

/// Float edge values for the MXCSR flags (zeros, infinities, NaNs, denormals, the JIT's range ends)
const FS32: &[u32] = &[0, 0x8000_0000, 0x7f80_0000, 0xff80_0000, 0x7fc0_0000, 0x7fa0_0000, 1, 0x8000_0001, 0x007f_ffff, 0x0080_0000, 0x7f7f_ffff, 0x3f80_0000, 0x2000_0000, 0x5f00_0000, 0x4e80_0000, 0x7f00_0000];
const FS64: &[u64] = &[0, 1 << 63, 0x7ff0 << 48, 0xfff0 << 48, 0x7ff8 << 48, 0x7ff4 << 48, 1, (1 << 52) - 1, 1 << 52, 0x7fef_ffff_ffff_ffff, 0x3ff0 << 48, 0x2000 << 48, 0x5fe0 << 48, 0x41d0 << 48, 0x47e0 << 48, 0x3810 << 48];

fn limit() -> u32 {
    LIMIT.load(std::sync::atomic::Ordering::Relaxed)
}

struct Mem(*mut u8);
impl koji::Memory for Mem {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        if !(0x10000..limit()).contains(&a) {
            return Err(Fault { addr: a, write: false });
        }
        Ok(unsafe { *self.0.add(a as usize) })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        if !(0x10000..limit()).contains(&a) {
            return Err(Fault { addr: a, write: true });
        }
        // (as kura's interp_write: the interpreter's store into translated code)
        if in_code(a) {
            CODE_WRITTEN.store(true, Relaxed);
        }
        unsafe { *self.0.add(a as usize) = v };
        Ok(())
    }
}

unsafe fn get_cpu(m: *mut u8) -> Cpu {
    std::ptr::read_unaligned(m.add(CPU as usize) as *const Cpu)
}
unsafe fn put_cpu(m: *mut u8, c: &Cpu) {
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

const R64: [Register; 13] = [Register::RAX, Register::RCX, Register::RDX, Register::RSI, Register::RDI, Register::R8, Register::R9, Register::R10, Register::R11, Register::R12, Register::R13, Register::R14, Register::R15];

fn sized(r: Register, bits: u32) -> Register {
    let i = R64.iter().position(|&x| x == r).unwrap();
    let full = [0usize, 1, 2, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15][i];
    let base = match bits {
        64 => Register::RAX,
        32 => Register::EAX,
        16 => Register::AX,
        _ => {
            return match full {
                0..=3 => [Register::AL, Register::CL, Register::DL, Register::BL][full],
                4..=7 => [Register::SPL, Register::BPL, Register::SIL, Register::DIL][full - 4],
                _ => unsafe { std::mem::transmute::<u8, Register>(Register::R8L as u8 + (full - 8) as u8) },
            }
        }
    };
    unsafe { std::mem::transmute::<u8, Register>(base as u8 + full as u8) }
}

fn mem(rng: &mut Rng, rip_target: bool) -> MemoryOperand {
    if rip_target {
        return MemoryOperand::new(Register::RIP, Register::None, 1, (DATA as u64 + rng.below(DATA_LEN as u64 - 16)) as i64, 8, false, Register::None);
    }
    let off = rng.below(DATA_LEN as u64 - 16 - 0x100) as i64;
    match rng.below(5) {
        // the stack (rsp = STACK - 0x800)
        0 => MemoryOperand::new(Register::RSP, Register::None, 1, rng.below(0x7f0) as i64, 1, false, Register::None),
        // rbp = DATA + 0x100
        1 => MemoryOperand::new(Register::RBP, Register::None, 1, off - 0x100, 1, false, Register::None),
        // index only: rbp * 2
        2 => MemoryOperand::new(Register::None, Register::RBP, 2, off - DATA as i64 - 0x200, 4, false, Register::None),
        _ => MemoryOperand::new(Register::RBX, Register::None, 1, off, 1, false, Register::None),
    }
}

/// One random instruction (or a few: push/pop pairs)
fn random_ins(rng: &mut Rng, out: &mut Vec<Instruction>, rip_hi: u32) {
    let r = |rng: &mut Rng| rng.pick(&R64);
    let bitsel = |rng: &mut Rng| rng.pick(&[8u32, 16, 32, 64, 64, 32]);
    let m = |rng: &mut Rng| {
        let rip = rip_hi == 0 && rng.below(4) == 0;
        mem(rng, rip)
    };
    use Code as C;
    if rip_hi == 0 && (WATCH.load(Relaxed) || FRESH.load(Relaxed)) && rng.below(60) == 0 {
        // a store into the program's own code (koji::jit::long's Gen::smc, Gen::fresh_store):
        // its first bytes,
        // often an instruction that already ran or runs next in a loop
        let at = MemoryOperand::new(Register::RIP, Register::None, 1, (CODE as u64 + rng.below(48)) as i64, 8, false, Register::None);
        out.push(Instruction::with2(C::Add_rm8_imm8, at, 1 + rng.below(3) as u32).unwrap());
        return;
    }
    let k = rng.below(40);
    if k >= 30 {
        if rng.below(40) == 0 {
            // a new MXCSR rounding mode (the region leaves: the interpreter runs on)
            let at = MemoryOperand::with_base_displ(Register::RSP, -8);
            out.push(Instruction::with2(Code::Mov_rm32_imm32, at, 0x1f80 | (rng.below(4) as u32) << 13).unwrap());
            out.push(Instruction::with1(Code::Ldmxcsr_m32, at).unwrap());
            return;
        }
        if rng.below(20) == 0 {
            // MSVC's math functions: the control bits alone (no switch, koji::jit::control_read)
            let at = MemoryOperand::with_base_displ(Register::RSP, -8);
            out.push(Instruction::with1(Code::Stmxcsr_m32, at).unwrap());
            out.push(Instruction::with2(Code::Mov_r32_rm32, Register::EAX, at).unwrap());
            out.push(Instruction::with2(Code::And_EAX_imm32, Register::EAX, rng.pick(&[0x7f80u32, 0x1f80, 0xffc0])).unwrap());
            // (the slot over: what it held of the flags may be what the JIT lost, not compared)
            out.push(Instruction::with2(Code::Mov_rm32_r32, at, Register::EAX).unwrap());
            return;
        }
        if rng.below(20) == 0 {
            // _clearfp: the flags read (the JIT keeps them from then on, koji::jit::MX_EXACT),
            // then cleared
            let at = MemoryOperand::with_base_displ(Register::RSP, -8);
            out.push(Instruction::with1(Code::Stmxcsr_m32, at).unwrap());
            out.push(Instruction::with2(Code::And_rm32_imm8, at, -64i32).unwrap());
            out.push(Instruction::with1(Code::Ldmxcsr_m32, at).unwrap());
            return;
        }
        if let Some(i) = random_sse(rng) {
            out.push(i);
        }
        return;
    }
    let ins = match k {
        0 => {
            let b = bitsel(rng);
            let code = match b { 8 => C::Mov_r8_rm8, 16 => C::Mov_r16_rm16, 32 => C::Mov_r32_rm32, _ => C::Mov_r64_rm64 };
            Instruction::with2(code, sized(r(rng), b), sized(r(rng), b)).unwrap()
        }
        1 => Instruction::with2(C::Mov_r64_imm64, r(rng), rng.next()).unwrap(),
        2 if rng.below(3) == 0 => Instruction::with2(C::Mov_r32_imm32, Register::ECX, rng.pick(&[0u32, 32, 64])).unwrap(),
        2 => Instruction::with2(C::Mov_r32_imm32, sized(r(rng), 32), rng.next() as u32).unwrap(),
        3 => {
            let b = bitsel(rng);
            let code = match b { 8 => C::Mov_r8_rm8, 16 => C::Mov_r16_rm16, 32 => C::Mov_r32_rm32, _ => C::Mov_r64_rm64 };
            Instruction::with2(code, sized(r(rng), b), m(rng)).unwrap()
        }
        4 => {
            let b = bitsel(rng);
            let code = match b { 8 => C::Mov_rm8_r8, 16 => C::Mov_rm16_r16, 32 => C::Mov_rm32_r32, _ => C::Mov_rm64_r64 };
            Instruction::with2(code, m(rng), sized(r(rng), b)).unwrap()
        }
        5 => Instruction::with2(C::Mov_rm8_imm8, m(rng), rng.next() as u8 as u32).unwrap(),
        6 => {
            let (code, d, s) = rng.pick(&[(C::Movzx_r32_rm8, 32, 8), (C::Movzx_r64_rm16, 64, 16), (C::Movsx_r64_rm8, 64, 8), (C::Movsx_r32_rm16, 32, 16), (C::Movsxd_r64_rm32, 64, 32)]);
            if rng.below(2) == 0 {
                Instruction::with2(code, sized(r(rng), d), sized(r(rng), s)).unwrap()
            } else {
                Instruction::with2(code, sized(r(rng), d), m(rng)).unwrap()
            }
        }
        7 => {
            let b = rng.pick(&[32u32, 64]);
            let mo = MemoryOperand::new(sized(r(rng), 64), sized(r(rng), 64), rng.pick(&[1, 2, 4, 8]), rng.next() as i32 as i64 >> 8, 1, false, Register::None);
            let code = if b == 64 { C::Lea_r64_m } else { C::Lea_r32_m };
            Instruction::with2(code, sized(r(rng), b), mo).unwrap()
        }
        8..=13 => {
            // ALU: add or adc sbb and sub xor cmp test
            let op = rng.below(9) as usize;
            let b = bitsel(rng);
            let form = rng.below(4);
            let rr = [
                [C::Add_rm8_r8, C::Add_rm16_r16, C::Add_rm32_r32, C::Add_rm64_r64],
                [C::Or_rm8_r8, C::Or_rm16_r16, C::Or_rm32_r32, C::Or_rm64_r64],
                [C::Adc_rm8_r8, C::Adc_rm16_r16, C::Adc_rm32_r32, C::Adc_rm64_r64],
                [C::Sbb_rm8_r8, C::Sbb_rm16_r16, C::Sbb_rm32_r32, C::Sbb_rm64_r64],
                [C::And_rm8_r8, C::And_rm16_r16, C::And_rm32_r32, C::And_rm64_r64],
                [C::Sub_rm8_r8, C::Sub_rm16_r16, C::Sub_rm32_r32, C::Sub_rm64_r64],
                [C::Xor_rm8_r8, C::Xor_rm16_r16, C::Xor_rm32_r32, C::Xor_rm64_r64],
                [C::Cmp_rm8_r8, C::Cmp_rm16_r16, C::Cmp_rm32_r32, C::Cmp_rm64_r64],
                [C::Test_rm8_r8, C::Test_rm16_r16, C::Test_rm32_r32, C::Test_rm64_r64],
            ];
            let ri = [
                [C::Add_rm8_imm8, C::Add_rm16_imm16, C::Add_rm32_imm32, C::Add_rm64_imm32],
                [C::Or_rm8_imm8, C::Or_rm16_imm16, C::Or_rm32_imm32, C::Or_rm64_imm32],
                [C::Adc_rm8_imm8, C::Adc_rm16_imm16, C::Adc_rm32_imm32, C::Adc_rm64_imm32],
                [C::Sbb_rm8_imm8, C::Sbb_rm16_imm16, C::Sbb_rm32_imm32, C::Sbb_rm64_imm32],
                [C::And_rm8_imm8, C::And_rm16_imm16, C::And_rm32_imm32, C::And_rm64_imm32],
                [C::Sub_rm8_imm8, C::Sub_rm16_imm16, C::Sub_rm32_imm32, C::Sub_rm64_imm32],
                [C::Xor_rm8_imm8, C::Xor_rm16_imm16, C::Xor_rm32_imm32, C::Xor_rm64_imm32],
                [C::Cmp_rm8_imm8, C::Cmp_rm16_imm16, C::Cmp_rm32_imm32, C::Cmp_rm64_imm32],
                [C::Test_rm8_imm8, C::Test_rm16_imm16, C::Test_rm32_imm32, C::Test_rm64_imm32],
            ];
            let bi = match b { 8 => 0, 16 => 1, 32 => 2, _ => 3 };
            let imm = match b { 8 => rng.next() as u8 as i64, 16 => rng.next() as u16 as i64, _ => rng.next() as i32 as i64 };
            if op < 8 && b > 8 && rng.below(3) == 0 {
                // imm8 sign-extended (0x83), or reg <- r/m (0x03 family)
                let i8s = [
                    [C::Add_rm16_imm8, C::Add_rm32_imm8, C::Add_rm64_imm8],
                    [C::Or_rm16_imm8, C::Or_rm32_imm8, C::Or_rm64_imm8],
                    [C::Adc_rm16_imm8, C::Adc_rm32_imm8, C::Adc_rm64_imm8],
                    [C::Sbb_rm16_imm8, C::Sbb_rm32_imm8, C::Sbb_rm64_imm8],
                    [C::And_rm16_imm8, C::And_rm32_imm8, C::And_rm64_imm8],
                    [C::Sub_rm16_imm8, C::Sub_rm32_imm8, C::Sub_rm64_imm8],
                    [C::Xor_rm16_imm8, C::Xor_rm32_imm8, C::Xor_rm64_imm8],
                    [C::Cmp_rm16_imm8, C::Cmp_rm32_imm8, C::Cmp_rm64_imm8],
                ];
                let rm = [
                    [C::Add_r16_rm16, C::Add_r32_rm32, C::Add_r64_rm64],
                    [C::Or_r16_rm16, C::Or_r32_rm32, C::Or_r64_rm64],
                    [C::Adc_r16_rm16, C::Adc_r32_rm32, C::Adc_r64_rm64],
                    [C::Sbb_r16_rm16, C::Sbb_r32_rm32, C::Sbb_r64_rm64],
                    [C::And_r16_rm16, C::And_r32_rm32, C::And_r64_rm64],
                    [C::Sub_r16_rm16, C::Sub_r32_rm32, C::Sub_r64_rm64],
                    [C::Xor_r16_rm16, C::Xor_r32_rm32, C::Xor_r64_rm64],
                    [C::Cmp_r16_rm16, C::Cmp_r32_rm32, C::Cmp_r64_rm64],
                ];
                let imm8 = rng.next() as i8 as i32;
                return out.push(match rng.below(4) {
                    0 => Instruction::with2(i8s[op][bi - 1], sized(r(rng), b), imm8).unwrap(),
                    1 => Instruction::with2(i8s[op][bi - 1], m(rng), imm8).unwrap(),
                    _ => Instruction::with2(rm[op][bi - 1], sized(r(rng), b), m(rng)).unwrap(),
                });
            }
            match form {
                0 => Instruction::with2(rr[op][bi], sized(r(rng), b), sized(r(rng), b)).unwrap(),
                1 => Instruction::with2(rr[op][bi], m(rng), sized(r(rng), b)).unwrap(),
                2 => Instruction::with2(ri[op][bi], sized(r(rng), b), imm as i32).unwrap(),
                _ => Instruction::with2(ri[op][bi], m(rng), imm as i32).unwrap(),
            }
        }
        14 | 15 => {
            let b = bitsel(rng);
            let bi = match b { 8 => 0, 16 => 1, 32 => 2, _ => 3 };
            let codes = [
                [C::Inc_rm8, C::Inc_rm16, C::Inc_rm32, C::Inc_rm64],
                [C::Dec_rm8, C::Dec_rm16, C::Dec_rm32, C::Dec_rm64],
                [C::Neg_rm8, C::Neg_rm16, C::Neg_rm32, C::Neg_rm64],
                [C::Not_rm8, C::Not_rm16, C::Not_rm32, C::Not_rm64],
            ];
            let code = codes[rng.below(4) as usize][bi];
            if rng.below(3) == 0 { Instruction::with1(code, m(rng)).unwrap() } else { Instruction::with1(code, sized(r(rng), b)).unwrap() }
        }
        16 | 17 => {
            let b = bitsel(rng);
            let bi = match b { 8 => 0, 16 => 1, 32 => 2, _ => 3 };
            let codes = [
                [C::Shl_rm8_imm8, C::Shl_rm16_imm8, C::Shl_rm32_imm8, C::Shl_rm64_imm8],
                [C::Shr_rm8_imm8, C::Shr_rm16_imm8, C::Shr_rm32_imm8, C::Shr_rm64_imm8],
                [C::Sar_rm8_imm8, C::Sar_rm16_imm8, C::Sar_rm32_imm8, C::Sar_rm64_imm8],
                [C::Shl_rm8_CL, C::Shl_rm16_CL, C::Shl_rm32_CL, C::Shl_rm64_CL],
                [C::Shr_rm8_CL, C::Shr_rm16_CL, C::Shr_rm32_CL, C::Shr_rm64_CL],
                [C::Sar_rm8_CL, C::Sar_rm16_CL, C::Sar_rm32_CL, C::Sar_rm64_CL],
            ];
            let w = rng.below(6) as usize;
            let code = codes[w][bi];
            let cnt = rng.below(b as u64 + 2) as u32;
            let dst = sized(r(rng), b);
            if w >= 3 {
                if rng.below(3) == 0 {
                    return out.push(Instruction::with2(code, m(rng), Register::CL).unwrap());
                }
                if dst == Register::CL || dst == Register::CX || dst == Register::ECX || dst == Register::RCX {
                    Instruction::with(C::Nopd)
                } else {
                    Instruction::with2(code, dst, Register::CL).unwrap()
                }
            } else {
                Instruction::with2(code, dst, cnt).unwrap()
            }
        }
        18 => {
            let b = rng.pick(&[16u32, 32, 64]);
            match rng.below(3) {
                0 => Instruction::with2(match b { 16 => C::Imul_r16_rm16, 32 => C::Imul_r32_rm32, _ => C::Imul_r64_rm64 }, sized(r(rng), b), sized(r(rng), b)).unwrap(),
                1 => Instruction::with3(match b { 16 => C::Imul_r16_rm16_imm16, 32 => C::Imul_r32_rm32_imm32, _ => C::Imul_r64_rm64_imm32 }, sized(r(rng), b), sized(r(rng), b), if b == 16 { rng.next() as i16 as i32 } else { rng.next() as i32 >> rng.below(30) }).unwrap(),
                _ => Instruction::with2(match b { 16 => C::Imul_r16_rm16, 32 => C::Imul_r32_rm32, _ => C::Imul_r64_rm64 }, sized(r(rng), b), m(rng)).unwrap(),
            }
        }
        19 => {
            let cc = rng.below(16) as u32;
            let b = rng.pick(&[16u32, 32, 64]);
            let base = match b { 64 => C::Cmovo_r64_rm64 as u32, 32 => C::Cmovo_r32_rm32 as u32, _ => C::Cmovo_r16_rm16 as u32 };
            // (Cmovo..Cmovg are 3 apart: r16, r32, r64)
            let code: Code = unsafe { std::mem::transmute::<u16, Code>((base + cc * 3) as u16) };
            if rng.below(3) == 0 {
                Instruction::with2(code, sized(r(rng), b), m(rng)).unwrap()
            } else {
                Instruction::with2(code, sized(r(rng), b), sized(r(rng), b)).unwrap()
            }
        }
        20 => {
            let cc = rng.below(16) as u32;
            let code: Code = unsafe { std::mem::transmute::<u16, Code>((C::Seto_rm8 as u32 + cc) as u16) };
            if rng.below(3) == 0 { Instruction::with1(code, m(rng)).unwrap() } else { Instruction::with1(code, sized(r(rng), 8)).unwrap() }
        }
        21 => {
            let a = r(rng);
            let b2 = r(rng);
            out.push(Instruction::with1(C::Push_r64, a).unwrap());
            if rng.below(2) == 0 {
                out.push(Instruction::with1(C::Pushq_imm32, rng.next() as i32).unwrap());
                out.push(Instruction::with1(C::Pop_r64, r(rng)).unwrap());
            }
            match rng.below(4) {
                0 => {
                    out.push(Instruction::with1(C::Push_rm64, m(rng)).unwrap());
                    Instruction::with1(C::Pop_r64, b2).unwrap()
                }
                1 => Instruction::with1(C::Pop_rm64, m(rng)).unwrap(),
                _ => Instruction::with1(C::Pop_r64, b2).unwrap(),
            }
        }
        22 => {
            let b = rng.pick(&[32u32, 64]);
            Instruction::with2(if b == 64 { C::Xchg_rm64_r64 } else { C::Xchg_rm32_r32 }, sized(r(rng), b), sized(r(rng), b)).unwrap()
        }
        23 => Instruction::with(rng.pick(&[C::Cdqe, C::Cqo, C::Cdq, C::Cwde, C::Cbw, C::Cwd])),
        24 => {
            // fallback paths: bt, bsf, rol, locked ops, xchg with memory
            match rng.below(9) {
                3 => {
                    let mut i = Instruction::with2(C::Cmpxchg_rm64_r64, m(rng), r(rng)).unwrap();
                    i.set_has_lock_prefix(true);
                    i
                }
                4 => {
                    let mut i = Instruction::with2(C::Xadd_rm32_r32, m(rng), sized(r(rng), 32)).unwrap();
                    i.set_has_lock_prefix(true);
                    i
                }
                5 => {
                    let mut i = Instruction::with2(C::Add_rm64_r64, m(rng), r(rng)).unwrap();
                    i.set_has_lock_prefix(true);
                    i
                }
                6 => Instruction::with2(C::Xchg_rm64_r64, m(rng), r(rng)).unwrap(),
                0 => {
                    let b = rng.pick(&[32u32, 64]);
                    let w = rng.below(4) as usize;
                    let rr = if b == 64 { [C::Bt_rm64_r64, C::Bts_rm64_r64, C::Btr_rm64_r64, C::Btc_rm64_r64] } else { [C::Bt_rm32_r32, C::Bts_rm32_r32, C::Btr_rm32_r32, C::Btc_rm32_r32] };
                    let ri = if b == 64 { [C::Bt_rm64_imm8, C::Bts_rm64_imm8, C::Btr_rm64_imm8, C::Btc_rm64_imm8] } else { [C::Bt_rm32_imm8, C::Bts_rm32_imm8, C::Btr_rm32_imm8, C::Btc_rm32_imm8] };
                    match rng.below(3) {
                        0 => Instruction::with2(rr[w], sized(r(rng), b), sized(r(rng), b)).unwrap(),
                        1 => Instruction::with2(ri[w], sized(r(rng), b), rng.next() as u8 as u32).unwrap(),
                        _ => Instruction::with2(ri[w], m(rng), rng.next() as u8 as u32).unwrap(),
                    }
                }
                1 => Instruction::with2(C::Rol_rm32_imm8, sized(r(rng), 32), rng.below(32) as u32).unwrap(),
                _ => {
                    let (code, b) = rng.pick(&[(C::Bsr_r64_rm64, 64), (C::Bsf_r64_rm64, 64), (C::Bsr_r32_rm32, 32), (C::Bsf_r32_rm32, 32)]);
                    let src = match rng.below(4) {
                        0 => return out.push(Instruction::with2(code, sized(r(rng), b), m(rng)).unwrap()),
                        // (zero sometimes)
                        1 => {
                            let z = r(rng);
                            out.push(Instruction::with2(C::Xor_rm32_r32, sized(z, 32), sized(z, 32)).unwrap());
                            z
                        }
                        _ => r(rng),
                    };
                    Instruction::with2(code, sized(r(rng), b), sized(src, b)).unwrap()
                }
            }
        }
        25 => {
            // legacy high bytes (no REX)
            let hi = [Register::AH, Register::CH, Register::DH];
            let lo = [Register::AL, Register::CL, Register::DL, Register::AH, Register::CH, Register::DH];
            let a = rng.pick(&hi);
            let b = rng.pick(&lo);
            match rng.below(5) {
                0 => Instruction::with2(C::Mov_r8_rm8, a, b).unwrap(),
                1 => Instruction::with2(C::Mov_r8_rm8, b, a).unwrap(),
                2 => Instruction::with2(C::Add_rm8_r8, a, b).unwrap(),
                3 => Instruction::with2(C::Movzx_r32_rm8, rng.pick(&[Register::EAX, Register::ECX, Register::EDX, Register::ESI]), a).unwrap(),
                _ => Instruction::with2(C::Xor_rm8_imm8, a, rng.next() as u8 as u32).unwrap(),
            }
        }
        26 => {
            // one-operand MUL/IMUL, DIV/IDIV (a dividend made to fit now and then)
            let b = rng.pick(&[32u32, 64]);
            let w = rng.below(4) as usize;
            let codes = if b == 64 { [C::Mul_rm64, C::Imul_rm64, C::Div_rm64, C::Idiv_rm64] } else { [C::Mul_rm32, C::Imul_rm32, C::Div_rm32, C::Idiv_rm32] };
            let src = rng.pick(&[Register::RCX, Register::RSI, Register::R9, Register::R13]);
            if w >= 2 {
                match rng.below(3) {
                    0 => out.push(Instruction::with2(C::Xor_rm32_r32, Register::EDX, Register::EDX).unwrap()),
                    1 => out.push(Instruction::with(if b == 64 { C::Cqo } else { C::Cdq })),
                    _ => {}
                }
                if rng.below(2) == 0 {
                    out.push(Instruction::with2(C::Or_rm64_imm8, src, rng.below(16) as i32 + 1).unwrap());
                }
            }
            if rng.below(4) == 0 {
                return out.push(Instruction::with1(codes[w], m(rng)).unwrap());
            }
            Instruction::with1(codes[w], sized(src, b)).unwrap()
        }
        27 => {
            // TZCNT LZCNT POPCNT, ROL ROR by an immediate, BSWAP
            let b = rng.pick(&[32u32, 64]);
            match rng.below(4) {
                0 => {
                    let code = rng.pick(&if b == 64 { [C::Tzcnt_r64_rm64, C::Lzcnt_r64_rm64, C::Popcnt_r64_rm64] } else { [C::Tzcnt_r32_rm32, C::Lzcnt_r32_rm32, C::Popcnt_r32_rm32] });
                    if rng.below(4) == 0 {
                        let z = r(rng);
                        out.push(Instruction::with2(C::Xor_rm32_r32, sized(z, 32), sized(z, 32)).unwrap());
                        Instruction::with2(code, sized(r(rng), b), sized(z, b)).unwrap()
                    } else if rng.below(3) == 0 {
                        Instruction::with2(code, sized(r(rng), b), m(rng)).unwrap()
                    } else {
                        Instruction::with2(code, sized(r(rng), b), sized(r(rng), b)).unwrap()
                    }
                }
                1 | 2 => {
                    let code = rng.pick(&if b == 64 { [C::Rol_rm64_imm8, C::Ror_rm64_imm8, C::Rol_rm64_1, C::Ror_rm64_1] } else { [C::Rol_rm32_imm8, C::Ror_rm32_imm8, C::Rol_rm32_1, C::Ror_rm32_1] });
                    let dst = if rng.below(4) == 0 { None } else { Some(sized(r(rng), b)) };
                    let one = matches!(code, C::Rol_rm64_1 | C::Ror_rm64_1 | C::Rol_rm32_1 | C::Ror_rm32_1);
                    let cnt = if one { 1 } else { rng.below(70) as u32 };
                    match dst {
                        Some(d) => Instruction::with2(code, d, cnt).unwrap(),
                        None => Instruction::with2(code, m(rng), cnt).unwrap(),
                    }
                }
                _ => Instruction::with1(if b == 64 { C::Bswap_r64 } else { C::Bswap_r32 }, sized(r(rng), b)).unwrap(),
            }
        }
        29 if rng.below(8) == 0 => Instruction::with(C::Emms),
        28 => {
            // locked read-modify-writes and xchg with memory: aligned (wasm atomics) or not
            let b = rng.pick(&[32u32, 64]);
            let mem = if rng.below(4) == 0 {
                m(rng)
            } else {
                // rbx (= DATA) + an aligned offset
                let off = rng.below(DATA_LEN as u64 / 2) as i64 & !(b as i64 / 8 - 1);
                MemoryOperand::new(Register::RBX, Register::None, 1, off, 4, false, Register::None)
            };
            let x = sized(r(rng), b);
            let w = b == 64;
            let mut i = match rng.below(10) {
                0 => Instruction::with2(if w { C::Xchg_rm64_r64 } else { C::Xchg_rm32_r32 }, mem, x).unwrap(),
                1 => Instruction::with2(if w { C::Cmpxchg_rm64_r64 } else { C::Cmpxchg_rm32_r32 }, mem, x).unwrap(),
                2 => Instruction::with2(if w { C::Xadd_rm64_r64 } else { C::Xadd_rm32_r32 }, mem, x).unwrap(),
                3 => Instruction::with1(rng.pick(&if w { [C::Inc_rm64, C::Dec_rm64] } else { [C::Inc_rm32, C::Dec_rm32] }), mem).unwrap(),
                4 | 5 => {
                    let code = rng.pick(&if w { [C::Add_rm64_r64, C::Sub_rm64_r64, C::And_rm64_r64, C::Or_rm64_r64, C::Xor_rm64_r64] } else { [C::Add_rm32_r32, C::Sub_rm32_r32, C::And_rm32_r32, C::Or_rm32_r32, C::Xor_rm32_r32] });
                    Instruction::with2(code, mem, x).unwrap()
                }
                6 => {
                    let code = rng.pick(&if w { [C::Add_rm64_imm8, C::Sub_rm64_imm32, C::And_rm64_imm32, C::Or_rm64_imm8, C::Xor_rm64_imm32] } else { [C::Add_rm32_imm8, C::Sub_rm32_imm32, C::And_rm32_imm32, C::Or_rm32_imm8, C::Xor_rm32_imm32] });
                    let imm = if matches!(code, C::Add_rm64_imm8 | C::Or_rm64_imm8 | C::Add_rm32_imm8 | C::Or_rm32_imm8) { rng.next() as i8 as i32 } else { rng.next() as i32 };
                    Instruction::with2(code, mem, imm).unwrap()
                }
                7 => {
                    // cmpxchg that succeeds: the accumulator loaded from memory first
                    out.push(Instruction::with2(if w { C::Mov_r64_rm64 } else { C::Mov_r32_rm32 }, sized(Register::RAX, b), mem).unwrap());
                    Instruction::with2(if w { C::Cmpxchg_rm64_r64 } else { C::Cmpxchg_rm32_r32 }, mem, x).unwrap()
                }
                _ => Instruction::with2(if w { C::Xchg_rm64_r64 } else { C::Xchg_rm32_r32 }, mem, x).unwrap(),
            };
            if rng.below(8) != 0 || !matches!(i.mnemonic(), iced_x86::Mnemonic::Xchg) {
                i.set_has_lock_prefix(true);
            }
            i
        }
        _ => {
            let b = bitsel(rng);
            let code = match b { 8 => C::Mov_r8_rm8, 16 => C::Mov_r16_rm16, 32 => C::Mov_r32_rm32, _ => C::Mov_r64_rm64 };
            Instruction::with2(code, sized(r(rng), b), sized(r(rng), b)).unwrap()
        }
    };
    out.push(ins);
}

/// SSE and VEX.128 forms with xmm, GPR, memory and imm8 operands (every such Code of iced's,
/// those the JIT doesn't translate run through `step`)
fn sse_codes() -> &'static [Code] {
    use iced_x86::{EncodingKind, OpCodeOperandKind as K};
    static CODES: std::sync::OnceLock<Vec<Code>> = std::sync::OnceLock::new();
    CODES.get_or_init(|| {
        Code::values()
            .filter(|&c| {
                let o = c.op_code();
                if !(o.encoding() == EncodingKind::Legacy || o.encoding() == EncodingKind::VEX) || !o.mode64() || o.op_count() == 0 {
                    return false;
                }
                let kinds: Vec<K> = (0..o.op_count()).map(|i| o.op_kind(i)).collect();
                let ok = kinds.iter().all(|k| matches!(k, K::xmm_reg | K::xmm_rm | K::xmm_or_mem | K::xmm_vvvv | K::ymm_reg | K::ymm_rm | K::ymm_or_mem | K::ymm_vvvv | K::mm_reg | K::mm_rm | K::mm_or_mem | K::r32_reg | K::r64_reg | K::r32_rm | K::r64_rm | K::r32_or_mem | K::r64_or_mem | K::imm8 | K::mem));
                // (MMX forms too: an MMX register is the low half of a v128 in the JIT)
                let any_x = kinds.iter().any(|k| matches!(k, K::xmm_reg | K::xmm_rm | K::xmm_or_mem | K::xmm_vvvv | K::ymm_reg | K::ymm_rm | K::ymm_or_mem | K::ymm_vvvv | K::mm_reg | K::mm_rm | K::mm_or_mem));
                let m = format!("{:?}", c.mnemonic());
                // (side effects outside the compared state, or not in CPUID)
                let skip = ["Maskmov", "Vmaskmov", "Ldmxcsr", "Stmxcsr", "Aes", "Pclmul", "Sha", "Gf2p8", "Vcvtph2ps", "Vcvtps2ph", "Extrq", "Insertq", "Movntdqa", "Vmovntdqa", "Movntss", "Movntsd", "Crc32", "Vfm", "Vfnm", "Vgather", "Vpgather", "Vpdpb", "Vpdpw", "Vpmadd52", "Kmov", "Vtest", "Vblendv", "Vpblendv", "Rdrand", "Rdseed", "Pconfig", "Bndc", "Bndm", "Senduipi"];
                // (JIT64_SSE=Movlps,Blendv: only these mnemonic prefixes, to focus a run)
                let only = std::env::var("JIT64_SSE").map_or(true, |v| v.split(',').any(|p| m.starts_with(p)));
                ok && any_x && only && !skip.iter().any(|p| m.starts_with(p))
            })
            .collect()
    })
}

fn rand_xmm(rng: &mut Rng) -> Register {
    unsafe { std::mem::transmute::<u8, Register>(Register::XMM0 as u8 + rng.below(16) as u8) }
}

fn random_sse(rng: &mut Rng) -> Option<Instruction> {
    use iced_x86::{OpCodeOperandKind as K, OpKind};
    let codes = sse_codes();
    let c = rng.pick(codes);
    let o = c.op_code();
    let mut ins = Instruction::with(c);
    for i in 0..o.op_count() {
        let k = o.op_kind(i);
        let memory = matches!(k, K::mem) || (matches!(k, K::xmm_or_mem | K::ymm_or_mem | K::mm_or_mem | K::r32_or_mem | K::r64_or_mem) && rng.below(3) == 0);
        if memory {
            // rbx (= DATA) + a 32-aligned offset (aligned forms don't fault)
            ins.set_op_kind(i, OpKind::Memory);
            ins.set_memory_base(Register::RBX);
            ins.set_memory_index(Register::None);
            ins.set_memory_index_scale(1);
            ins.set_memory_displacement64(rng.below((DATA_LEN as u64 - 64) / 32) * 32);
            ins.set_memory_displ_size(1);
            continue;
        }
        match k {
            K::xmm_reg | K::xmm_rm | K::xmm_or_mem | K::xmm_vvvv => {
                ins.set_op_kind(i, OpKind::Register);
                ins.set_op_register(i, rand_xmm(rng));
            }
            K::ymm_reg | K::ymm_rm | K::ymm_or_mem | K::ymm_vvvv => {
                ins.set_op_kind(i, OpKind::Register);
                ins.set_op_register(i, Register::YMM0 + rng.below(16) as u32);
            }
            K::mm_reg | K::mm_rm | K::mm_or_mem => {
                ins.set_op_kind(i, OpKind::Register);
                ins.set_op_register(i, unsafe { std::mem::transmute::<u8, Register>(Register::MM0 as u8 + rng.below(8) as u8) });
            }
            K::r32_reg | K::r32_rm | K::r32_or_mem => {
                ins.set_op_kind(i, OpKind::Register);
                ins.set_op_register(i, sized(rng.pick(&R64), 32));
            }
            K::r64_reg | K::r64_rm | K::r64_or_mem => {
                ins.set_op_kind(i, OpKind::Register);
                ins.set_op_register(i, rng.pick(&R64));
            }
            K::imm8 => {
                ins.set_op_kind(i, OpKind::Immediate8);
                ins.set_immediate8(rng.next() as u8);
            }
            _ => return None,
        }
    }
    Some(ins)
}

/// None when a branch lost its target (the shrinker removed it)
fn encode(list: &[Instruction], rip: u64) -> Option<Vec<u8>> {
    for i in list {
        if i.op0_kind() == iced_x86::OpKind::NearBranch64 {
            let t = i.near_branch64();
            if t >= LBASE && !list.iter().any(|j| j.ip() == t) {
                return None;
            }
        }
    }
    let block = InstructionBlock::new(list, rip);
    BlockEncoder::encode(64, block, BlockEncoderOptions::NONE).ok().map(|r| r.code_buffer)
}

const LBASE: u64 = 0x7000_0000;
const COUNTER: u64 = 0xff8;

/// Labels every instruction (its IP: an id for the encoder), adds branches: forward jcc, a
/// counted loop, a call to a subroutine after the final jmp.
/// Whether `list[k]` is inside one of the MXCSR reads random_ins makes (_clearfp's STMXCSR,
/// AND, LDMXCSR; the math functions' STMXCSR, MOV, AND, MOV back): nothing goes in there (a jump past
/// the LDMXCSR would leave flags the JIT without their checks may have lost, and a read of
/// the control bits is one only as the three instructions, see koji::jit::control_read).
fn inner(list: &[Instruction], k: usize) -> bool {
    k < list.len() && (1..=3).any(|d| k >= d && list[k - d].code() == Code::Stmxcsr_m32)
}

/// Sometimes an F2 (BND, as in MSVC's CRT) or F3 (AMD's `rep ret`) prefix on a branch, call or
/// return: one the CPU ignores, which the JIT translates as if it had none.
fn bnd(rng: &mut Rng, mut i: Instruction) -> Instruction {
    // (JNOBND: none, the same programs otherwise)
    match rng.below(6) {
        _ if std::env::var("JNOBND").is_ok() => {}
        0 => i.set_has_repne_prefix(true),
        1 => i.set_has_rep_prefix(true),
        _ => {}
    }
    i
}

fn structure(rng: &mut Rng, body: Vec<Instruction>, sub: Vec<Instruction>, rip_hi: u32) -> Vec<Instruction> {
    let mut id = 0u64;
    let mut label = |mut i: Instruction| {
        id += 1;
        i.set_ip(LBASE + id * 16);
        i
    };
    let mut list: Vec<Instruction> = body.into_iter().map(&mut label).collect();
    let cnt = MemoryOperand::new(Register::RBX, Register::None, 1, COUNTER as i64, 1, false, Register::None);
    let mut lp = None;
    if list.len() > 3 && rng.below(2) == 0 {
        // loop over [i, j)
        let i = rng.below(list.len() as u64 - 1) as usize;
        let j = i + 1 + rng.below((list.len() - i - 1) as u64) as usize;
        if !inner(&list, i) && !inner(&list, j) {
            let head = list[i].ip();
            let dec = label(Instruction::with1(Code::Dec_rm32, cnt).unwrap());
            let jne = label(bnd(rng, Instruction::with_branch(Code::Jne_rel32_64, head).unwrap()));
            list.insert(j, jne);
            list.insert(j, dec);
            let init = label(Instruction::with2(Code::Mov_rm32_imm32, cnt, 1 + rng.below(3) as u32).unwrap());
            lp = Some((init.ip(), jne.ip()));
            list.insert(i, init);
        }
    }
    for _ in 0..rng.below(4) {
        // a forward jcc
        if list.len() < 3 {
            break;
        }
        let i = rng.below(list.len() as u64 - 1) as usize;
        let j = i + 1 + rng.below((list.len() - i - 1) as u64) as usize;
        let cc = rng.below(16) as u16;
        let code: Code = unsafe { std::mem::transmute::<u16, Code>(Code::Jo_rel32_64 as u16 + cc * 3) };
        let target = list[j].ip();
        // not into the loop from outside it (it would skip the counter's init)
        if let Some((a, b)) = lp {
            let pa = list.iter().position(|x| x.ip() == a).unwrap();
            let pb = list.iter().position(|x| x.ip() == b).unwrap();
            if ((i <= pa || i > pb) && j > pa && j <= pb) || j == pb {
                continue;
            }
        }
        if inner(&list, i) {
            continue;
        }
        let jcc = label(bnd(rng, Instruction::with_branch(code, target).unwrap()));
        list.insert(i, jcc);
    }
    let sub: Vec<Instruction> = sub.into_iter().map(&mut label).collect();
    if let Some(first) = sub.first().map(|i| i.ip()) {
        let call = label(bnd(rng, Instruction::with_branch(Code::Call_rel32_64, first).unwrap()));
        let push = label(Instruction::with1(Code::Push_r64, Register::RAX).unwrap());
        let mut at = rng.below(list.len() as u64 + 1) as usize;
        if let Some((_, b)) = lp {
            // not between the loop's dec and jne (flags)
            if at == list.iter().position(|x| x.ip() == b).unwrap() {
                at += 1;
            }
        }
        while at < list.len() && inner(&list, at) {
            at += 1;
        }
        list.insert(at, call);
        list.insert(at, push);
    }
    let end = END as u64 | (rip_hi as u64) << 32;
    let mut jmp = bnd(rng, Instruction::with_branch(Code::Jmp_rel32_64, end).unwrap());
    jmp.set_ip(1); // (kept by the shrinker)
    list.push(jmp);
    let has_sub = !sub.is_empty();
    list.extend(sub);
    if has_sub {
        let ret = label(bnd(rng, Instruction::with1(Code::Retnq_imm16, 8u32).unwrap()));
        list.push(ret);
    }
    list
}

/// the dispatcher's hash table the regions chain through (Cpu::jit_hash: HASH_N (eip, entry)
/// pairs), and the ring of jumps between regions (Cpu::jit_ring)
const HASH: u32 = 0x8000;
const HASH_N: u32 = 2048;
const RING: u32 = 0xc000;
/// a bit per 4 KB page with translated code (Cpu::jit_code_pages; addresses below 128 MB)
const PAGES: u32 = 0xd000;

/// Whether translations test their stores against the pages with translated code
/// (koji::jit::set_watch, as kura for a main executable's W|X pages).
static WATCH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn watch(_: u32) -> bool {
    WATCH.load(Relaxed)
}

/// Whether translations check their code on entry (koji::jit::set_fresh, as kura for W|X pages
/// outside the exe: Mono's code in a 64-bit process): a region entered where the code is no
/// longer what it was made from leaves with Cpu::jit_stale; its stores into its own block
/// leave with Cpu::jit_smc.
static FRESH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn fresh(_: u32) -> bool {
    FRESH.load(Relaxed)
}
/// Regions entered and found stale (Cpu::jit_stale) in this run.
static STALE_HITS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
thread_local! {
    /// instructions regions handed to the interpreter (the `step` import), on this thread
    static STEPS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The translated code's byte ranges (kura's CODE_RANGES), and whether the interpreter wrote
/// into them since last asked.
static CODE_RANGES: std::sync::Mutex<Vec<(u32, u32)>> = std::sync::Mutex::new(Vec::new());
static CODE_WRITTEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
fn in_code(a: u32) -> bool {
    CODE_RANGES.lock().unwrap().iter().any(|&(s, n)| a >= s && a - s < n.max(1))
}

struct Jit {
    engine: Engine,
    store: Store<()>,
    linker: Linker<()>,
    table: Table,
    /// eip -> (function, block, table index)
    entries: HashMap<u32, (Func, u32, u32)>,
    /// blocks a region may take in
    range: (u32, u32),
    /// translated code was written over (by a region's store, Cpu::jit_smc, or the
    /// interpreter's): as kura, the translations went and the interpreter runs on
    rewritten: bool,
}

impl Jit {
    fn new(engine: &Engine, memory: &SharedMemory, base: usize) -> Jit {
        let mut store = Store::new(engine, ());
        // A region can loop (and chain) without returning to run's host-side step limit.
        // Trap instead of hanging the whole CI when a generated program or the JIT loops.
        store.set_fuel(100_000_000).unwrap();
        let mut linker = Linker::new(engine);
        linker.define(&store, "env", "memory", memory.clone()).unwrap();
        let table = Table::new(&mut store, TableType::new(RefType::FUNCREF, 0, None), Ref::Func(None)).unwrap();
        linker.define(&store, "env", "table", table).unwrap();
        linker
            .func_wrap("env", "step", move |cpu: i32| -> i32 {
                let m = base as *mut u8;
                assert_eq!(cpu as u32, CPU);
                STEPS.set(STEPS.get() + 1);
                let mut c = unsafe { get_cpu(m) };
                let r = koji::step(&mut c, &mut Mem(m));
                unsafe { put_cpu(m, &c) };
                match r {
                    // (as kura: it rewrote translated code, the region leaves)
                    Exit::Continue if CODE_WRITTEN.load(Relaxed) => 2,
                    // (as kura: another MXCSR mode leaves the region, and so does the first
                    // read of its flags)
                    Exit::Continue if !koji::jit::sse_ok(c.mxcsr) => 2,
                    Exit::Continue if mx_check() => 2,
                    Exit::Continue => 0,
                    _ => 2,
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
                koji::lazy::Lazy { op: op as u32, a: a as u32 as u64, b: b as u32 as u64, r: r as u32 as u64, c: c as u32 }.status() as i32
            })
            .unwrap();
        linker
            .func_wrap("env", "x87", |_: i32, _: i32, _: i32, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64, _: f64| {})
            .unwrap();
        unsafe { std::ptr::write_bytes((base + HASH as usize) as *mut u8, 0, (HASH_N * 8) as usize + 0x1000) };
        unsafe { std::ptr::write_bytes((base + PAGES as usize) as *mut u8, 0, 0x1000) };
        CODE_RANGES.lock().unwrap().clear();
        CODE_WRITTEN.store(false, Relaxed);
        koji::jit::set_watch(watch);
        koji::jit::set_fresh(fresh);
        Jit { engine: engine.clone(), store, linker, table, entries: HashMap::new(), range: (0, u32::MAX), rewritten: false }
    }

    /// Points the Cpu at the hash table and the ring (regions jump to each other through them).
    fn chain(&self, base: usize) {
        let m = base as *mut u8;
        let mut c = unsafe { get_cpu(m) };
        c.jit_hash = HASH;
        c.jit_mask = HASH_N - 1;
        c.jit_ring = RING;
        c.jit_code_pages = PAGES;
        unsafe { put_cpu(m, &c) };
    }

    fn translate(&mut self, base: usize, eip: u32, rip_hi: u32) -> Result<(), String> {
        let mut m = koji::jit::wasm::Module::default();
        let full = eip as u64 | (rip_hi as u64) << 32;
        let (lo, hi) = self.range;
        let region = koji::jit::long::translate_region64(&mut m, &mut Mem(base as *mut u8), &[full], (END, END + 16), |a, _| a != END && a >= lo && a < hi).ok_or("refused")?;
        let bytes = m.finish();
        for &(a, n) in &region.code {
            CODE_RANGES.lock().unwrap().push((a, n));
            for p in (a >> 12)..=((a + n.max(1) - 1) >> 12) {
                if p < 0x8000 {
                    let w = (base + PAGES as usize + 4 * (p as usize >> 5)) as *mut u32;
                    unsafe { w.write(w.read() | 1 << (p & 31)) };
                }
            }
        }
        if std::env::var("JDUMP").is_ok() { std::fs::write("/tmp/jit64-last.wasm", &bytes).ok(); eprintln!("module {} bytes", bytes.len()); }
        let module = Module::new(&self.engine, &bytes).map_err(|e| {
            std::fs::write("/tmp/jit64-bad.wasm", &bytes).ok();
            format!("invalid module (in /tmp/jit64-bad.wasm): {e:?}")
        })?;
        let inst = self.linker.instantiate(&mut self.store, &module).map_err(|e| format!("{e}"))?;
        let f = inst.get_func(&mut self.store, "f0").unwrap();
        let idx = self.table.grow(&mut self.store, 1, Ref::Func(Some(f))).unwrap() as u32;
        for (a, k) in region.starts {
            self.entries.insert(a, (f, k, idx));
            // (the dispatcher's linear probing, newer regions taking over a block)
            let mut h = koji::jit::dispatch::slot(a, HASH_N - 1);
            for probe in 0.. {
                // (a full table: the program wrote over it, or more blocks than it holds)
                if probe == HASH_N {
                    return Err(format!("hash table full at {a:#x}"));
                }
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

    /// Runs from cpu.eip until END: regions, the interpreter for what they leave to it.
    fn run(&mut self, base: usize) -> Result<usize, String> {
        let m = base as *mut u8;
        let mut regions = 0;
        // (a program's store can raise a loop count it runs once rewritten, one interpreter
        // step a round: 0x20001 iterations of a three-instruction loop fit)
        for _ in 0..1_000_000 {
            let c = unsafe { get_cpu(m) };
            if c.eip == END {
                if std::env::var("JRING").is_ok() {
                    eprintln!("regions {regions} chained {}", unsafe { ((base + RING as usize) as *const u32).read() });
                }
                return Ok(regions);
            }
            // (as kura: translations made without the MXCSR flag checks go)
            mx_check();
            if SWITCHED.swap(false, Relaxed) {
                unsafe { std::ptr::write_bytes((base + HASH as usize) as *mut u8, 0, (HASH_N * 8) as usize + 0x1000) };
                self.entries.clear();
            }
            if !koji::jit::sse_ok(c.mxcsr) || self.rewritten {
                // (as kura: the interpreter while MXCSR isn't the JIT's, and for code that
                // was rewritten)
                let mut c = c;
                let r = koji::step(&mut c, &mut Mem(m));
                unsafe { put_cpu(m, &c) };
                if r != Exit::Continue {
                    return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
                }
                continue;
            }
            if !self.entries.contains_key(&c.eip) {
                self.translate(base, c.eip, c.rip_hi)?;
            }
            let (f, k, idx) = self.entries[&c.eip];
            self.chain(base);
            let mut params = vec![Val::I32(0); 16];
            params[0] = Val::I32(CPU as i32);
            params[1] = Val::I32((idx | k << koji::jit::ENTRY_SHIFT) as i32);
            let mut results = vec![Val::I32(0); 1];
            f.call(&mut self.store, &params, &mut results).map_err(|e| format!("trap: {e:?}"))?;
            regions += 1;
            let mut c = unsafe { get_cpu(m) };
            if c.jit_stale != 0 {
                // (as kura's stale_hit: the code changed since translated; the translations go,
                // the interpreter runs the block's first instruction, then it is made again)
                let a = std::mem::take(&mut c.jit_stale);
                unsafe { put_cpu(m, &c) };
                if !FRESH.load(Relaxed) || results[0].unwrap_i32() != 1 || c.eip != a {
                    return Err(format!("stale exit at {a:#x}: fresh {}, status {}, eip {:#x}", FRESH.load(Relaxed), results[0].unwrap_i32(), c.eip));
                }
                STALE_HITS.fetch_add(1, Relaxed);
                unsafe { std::ptr::write_bytes((base + HASH as usize) as *mut u8, 0, (HASH_N * 8) as usize + 0x1000) };
                self.entries.clear();
            }
            if c.jit_smc != 0 {
                // (a store into a page with translated code, the region left at the next
                // instruction: into the code itself, or data beside it, which leaves the
                // translations alone)
                let a = std::mem::take(&mut c.jit_smc);
                unsafe { put_cpu(m, &c) };
                if results[0].unwrap_i32() != 1 {
                    return Err(format!("store at {a:#x} into a page with code: status {}", results[0].unwrap_i32()));
                }
                if in_code(a) {
                    CODE_WRITTEN.store(true, Relaxed);
                }
            }
            if CODE_WRITTEN.swap(false, Relaxed) {
                self.rewritten = true;
                unsafe { std::ptr::write_bytes((base + HASH as usize) as *mut u8, 0, (HASH_N * 8) as usize + 0x1000) };
                self.entries.clear();
            }
            match results[0].unwrap_i32() {
                0 => {}
                1 => {
                    let mut c = unsafe { get_cpu(m) };
                    let r = koji::step(&mut c, &mut Mem(m));
                    unsafe { put_cpu(m, &c) };
                    if r != Exit::Continue {
                        return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
                    }
                }
                // (a step that changed MXCSR: on in the interpreter; one that read its flags)
                2 if !koji::jit::sse_ok(unsafe { get_cpu(m) }.mxcsr) || SWITCHED.load(Relaxed) || self.rewritten => {}
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
        let r = koji::step(&mut c, &mut Mem(m));
        if r != Exit::Continue {
            // (the state where it stopped, as jit32test: before, the one it started with)
            unsafe { put_cpu(m, &c) };
            return Err(format!("interpreter exit {r:?} at {:#x}", c.eip));
        }
    }
    Err("no end".into())
}

fn initial(rng: &mut Rng, rip_hi: u32) -> Cpu {
    let mut c = Cpu { mode64: 1, rip_hi, eip: CODE, jit_budget: 1 << 30, ..Cpu::default() };
    for i in 0..16 {
        let v = match rng.below(4) {
            0 => rng.below(256),
            1 => rng.next() as u32 as u64,
            2 => (rng.next() as i32 as i64) as u64,
            _ => rng.next(),
        };
        c.set_gpr(i, v);
    }
    c.set_gpr(3, DATA as u64 | (rng.below(2) << 32)); // rbx (its upper half: modulo 4GB)
    c.set_gpr(4, (STACK - 0x800) as u64); // rsp
    c.set_gpr(5, DATA as u64 + 0x100); // rbp
    c.eflags = 0x202 | (rng.next() as u32 & 0x8d5);
    // xmm: random bits, or floats / doubles / small integers (fewer NaNs)
    let vec = |rng: &mut Rng| -> u128 {
        match rng.below(8) {
            0 => rng.next() as u128 | (rng.next() as u128) << 64,
            1 => (0..4).fold(0, |v, k| v | ((((rng.next() as i32) >> rng.below(31)) as f32 / 8.0).to_bits() as u128) << (32 * k)),
            2 => (0..2).fold(0, |v, k| v | ((((rng.next() as i64) >> rng.below(63)) as f64 / 8.0).to_bits() as u128) << (64 * k)),
            3 => (0..8).fold(0, |v, k| v | ((rng.below(512) as u16).wrapping_sub(256) as u128) << (16 * k)),
            // the MXCSR flags' edge cases: zeros, infinities, NaNs, denormals, range ends
            4 => (0..4).fold(0, |v, k| v | (if rng.below(2) == 0 { FS32[rng.below(FS32.len() as u64) as usize] } else { (1.5f32 * k as f32).to_bits() } as u128) << (32 * k)),
            5 => (0..2).fold(0, |v, k| v | (if rng.below(2) == 0 { FS64[rng.below(FS64.len() as u64) as usize] } else { (1.5 * k as f64).to_bits() } as u128) << (64 * k)),
            // small dwords / qwords: shift counts around the element widths (VPSLLVD...), indices
            6 => (0..4).fold(0, |v, k| v | (rng.below(72) as u128) << (32 * k)),
            _ => (0..2).fold(0, |v, k| v | (rng.below(72) as u128) << (64 * k)),
        }
    };
    for i in 0..16 {
        let v = vec(rng);
        if i < 8 {
            c.xmm[i] = v;
        } else {
            c.xmm8[i - 8] = v;
        }
        // the upper YMM halves: VEX.128 must clear the destination's, legacy SSE keep them
        c.ymmh[i] = if rng.below(2) == 0 { vec(rng) } else { rng.next() as u128 | (rng.next() as u128) << 64 };
    }
    for i in 0..8 {
        c.mm[i] = rng.next();
    }
    c.mxcsr = 0x1f80 | if rng.below(4) == 0 { (rng.below(4) as u32) << 13 } else { 0 } | if rng.below(2) == 0 { rng.below(64) as u32 } else { 0 };
    c
}

/// MXCSR flags kept from the start (koji::jit::MX_EXACT); else from the program's first read
/// of them (STMXCSR, FXSAVE), as a program that never reads them runs.
static EXACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// The JIT's MXCSR flag mode switched (a step read the flags): translations are dropped.
static SWITCHED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// As kura's mx_check: the program read the MXCSR flags first (in the interpreter).
fn mx_check() -> bool {
    let s = koji::jit::mx_switched();
    if s {
        SWITCHED.store(true, Relaxed);
    }
    s
}

/// An interpreter fault at an address in [MEM, GUEST_HI): "interpreter exit Fault(Fault { addr: N, .."
fn wild_fault(e: &str) -> bool {
    let Some(i) = e.find("addr: ") else { return false };
    let n: String = e[i + 6..].chars().take_while(|c| c.is_ascii_digit()).collect();
    n.parse::<u32>().is_ok_and(|a| a >= limit() && a < koji::jit::GUEST_HI)
}

/// `pre`: (FRESH) another program run first at the same address by the same Jit, whose
/// translations the case's program then meets: they must find their code changed.
fn run_case(engine: &Engine, memory: &SharedMemory, base: usize, list0: &[Instruction], data: &[u8], cpu0: &Cpu, rip_hi: u32, pre: Option<(&[Instruction], &Cpu)>) -> (Vec<String>, Vec<u8>) {
    let m = base as *mut u8;
    let rip = CODE as u64 | (rip_hi as u64) << 32;
    let Some(code) = encode(list0, rip) else { return (Vec::new(), Vec::new()) };
    let setup = |cpu: &Cpu| unsafe {
        std::ptr::write_bytes(m.add(0x10000), 0, MEM - 0x10000);
        std::ptr::copy_nonoverlapping(code.as_ptr(), m.add(CODE as usize), code.len());
        std::ptr::copy_nonoverlapping(data.as_ptr(), m.add(DATA as usize), data.len());
        put_cpu(m, cpu);
    };
        setup(cpu0);
        // (whether the program reads the MXCSR flags: then, made without their checks until it
        // does and cleared there, they must be the CPU's at the end)
        koji::jit::MX_EXACT.store(false, Relaxed);
        koji::jit::mx_switched();
        let want = interp(base);
        if want.as_ref().is_err_and(|e| e == "no end") {
            // There is no reference final state. Previously this was ignored only AFTER
            // running the JIT, which could loop forever on the same generated program.
            return (Vec::new(), code);
        }
        // (a store into the code made an instruction access an address beyond this harness's
        // memory but in the JIT's guest window, which regions access directly: not comparable)
        if want.as_ref().is_err_and(|e| wild_fault(e)) {
            return (Vec::new(), code);
        }
        let reads = koji::jit::mx_switched();
        let exact = EXACT.load(Relaxed);
        koji::jit::MX_EXACT.store(exact, Relaxed);
        SWITCHED.store(false, Relaxed);
        let (mut want_cpu, want_data, want_stack) = unsafe { (get_cpu(m), std::slice::from_raw_parts(m.add(DATA as usize), DATA_LEN as usize).to_vec(), std::slice::from_raw_parts(m.add(STACK as usize - 0x1000), 0x1000).to_vec()) };
        let mut jit = Jit::new(engine, memory, base);
        if let Some((pl, pc)) = pre.filter(|_| FRESH.load(Relaxed)) {
            if let Some(pcode) = encode(pl, rip) {
                unsafe {
                    std::ptr::write_bytes(m.add(0x10000), 0, MEM - 0x10000);
                    std::ptr::copy_nonoverlapping(pcode.as_ptr(), m.add(CODE as usize), pcode.len());
                    std::ptr::copy_nonoverlapping(data.as_ptr(), m.add(DATA as usize), data.len());
                    put_cpu(m, pc);
                }
                let _ = jit.run(base);
                // (its translations stay; whatever it found rewritten is forgotten)
                jit.rewritten = false;
                CODE_WRITTEN.store(false, Relaxed);
                koji::jit::MX_EXACT.store(exact, Relaxed);
                SWITCHED.store(false, Relaxed);
            }
        }
        setup(cpu0);
        if koji::jit::long::DEBUG_BLOCKS.load(std::sync::atomic::Ordering::Relaxed) != 0 {
            // (small regions at every instruction first, in reverse: nearly every exit then
            // chains to another region, or back into its own)
            let mut dec = iced_x86::Decoder::with_ip(64, &code, rip, iced_x86::DecoderOptions::NONE);
            let ips: Vec<u64> = (&mut dec).into_iter().map(|i| i.ip()).collect();
            for &ip in ips.iter().rev() {
                let _ = jit.translate(base, ip as u32, (ip >> 32) as u32);
            }
        }
        let got = jit.run(base);
        let (mut got_cpu, got_data, got_stack) = unsafe { (get_cpu(m), std::slice::from_raw_parts(m.add(DATA as usize), DATA_LEN as usize).to_vec(), std::slice::from_raw_parts(m.add(STACK as usize - 0x1000), 0x1000).to_vec()) };
        want_cpu.materialize();
        got_cpu.materialize();
        if !exact && !reads {
            // (made without the flag checks throughout, the flags may be missing: the rest
            // must not)
            want_cpu.mxcsr &= !0x3f;
            got_cpu.mxcsr &= !0x3f;
        }
        let mut diffs = Vec::new();
        if !exact && !reads && koji::jit::mx_exact() {
            diffs.push("mx: the JIT switched to the flag checks, the program read no flags".into());
        }
        if want.is_err() && got.is_err() {
            return (diffs, code); // (both stopped: where differs)
        }
        if want.is_err() || got.is_err() {
            if want.is_ok() != got.is_ok() {
                diffs.push(format!("runs: interp {want:?} jit {got:?}"));
            }
        }
        for i in 0..16 {
            if want_cpu.gpr(i) != got_cpu.gpr(i) {
                diffs.push(format!("r{i}: want {:#x} got {:#x}", want_cpu.gpr(i), got_cpu.gpr(i)));
            }
        }
        if want_cpu.eflags & 0x8d5 != got_cpu.eflags & 0x8d5 {
            diffs.push(format!("flags: want {:#x} got {:#x}", want_cpu.eflags & 0x8d5, got_cpu.eflags & 0x8d5));
        }
        for i in 0..16 {
            let (w, g) = if i < 8 { (want_cpu.xmm[i], got_cpu.xmm[i]) } else { (want_cpu.xmm8[i - 8], got_cpu.xmm8[i - 8]) };
            if w != g {
                diffs.push(format!("xmm{i}: want {w:032x} got {g:032x}"));
            }
            if want_cpu.ymmh[i] != got_cpu.ymmh[i] {
                diffs.push(format!("ymm{i} upper: want {:032x} got {:032x}", want_cpu.ymmh[i], got_cpu.ymmh[i]));
            }
        }
        for i in 0..8 {
            if want_cpu.mm[i] != got_cpu.mm[i] {
                diffs.push(format!("mm{i}: want {:016x} got {:016x}", want_cpu.mm[i], got_cpu.mm[i]));
            }
        }
        if want_cpu.fpu.empty != got_cpu.fpu.empty {
            diffs.push(format!("fpu empty: want {:#x} got {:#x}", want_cpu.fpu.empty, got_cpu.fpu.empty));
        }
        if want_cpu.mxcsr != got_cpu.mxcsr {
            diffs.push(format!("mxcsr: want {:#x} got {:#x}", want_cpu.mxcsr, got_cpu.mxcsr));
        }
        if want_cpu.rip_hi != got_cpu.rip_hi {
            diffs.push(format!("rip_hi: want {} got {}", want_cpu.rip_hi, got_cpu.rip_hi));
        }
        if want_data != got_data {
            let i = want_data.iter().zip(&got_data).position(|(a, b)| a != b).unwrap();
            diffs.push(format!("data differs at +{i:#x}"));
        }
        if want_stack != got_stack {
            diffs.push("stack differs".into());
        }
    (diffs, code)
}

/// `exe <file.exe> <lo> <hi> [count] [seed]`: the exe's code in [lo, hi) (low-32 addresses of the
/// image mapped at its 4 GB alias) from random instruction boundaries and random states
/// (registers small or pointing into DATA), interpreter against JIT until the code leaves the range.
/// A PE image's sections at their RVAs: (the image, its base's low and high halves, its size).
/// Every header field checked against the file: a damaged or cut file is an error, not a panic
/// (`mapped.len() - va` underflowed for a section past the image).
fn map_pe(image: &[u8]) -> Result<(Vec<u8>, u32, u32, u32), String> {
    let u16_at = |o: usize| image.get(o..o + 2).map(|b| u16::from_le_bytes(b.try_into().unwrap()) as usize).ok_or(format!("cut at {o:#x}"));
    let u32_at = |o: usize| image.get(o..o + 4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).ok_or(format!("cut at {o:#x}"));
    let pe = u32_at(0x3c)? as usize;
    if image.get(pe..pe + 4) != Some(b"PE\0\0") {
        return Err("not a PE file".into());
    }
    let nsec = u16_at(pe + 6)?;
    let opt = u16_at(pe + 20)?;
    let base64 = image.get(pe + 48..pe + 56).map(|b| u64::from_le_bytes(b.try_into().unwrap())).ok_or("cut in the optional header")?;
    let size = u32_at(pe + 24 + 56)?;
    // (a 4 GB address space at most: the image must fit in it)
    if size == 0 || base64 as u32 as u64 + size as u64 > 1 << 32 {
        return Err(format!("image size {size:#x} at {base64:#x}"));
    }
    let mut mapped = vec![0u8; size as usize];
    let headers = (u32_at(pe + 24 + 60)? as usize).min(image.len()).min(mapped.len());
    mapped[..headers].copy_from_slice(&image[..headers]);
    for i in 0..nsec {
        let sh = pe + 24 + opt + i * 40;
        let va = u32_at(sh + 12)? as usize;
        let raw = u32_at(sh + 16)? as usize;
        let ptr = u32_at(sh + 20)? as usize;
        if va >= mapped.len() || ptr >= image.len() {
            continue;
        }
        let n = raw.min(mapped.len() - va).min(image.len() - ptr);
        mapped[va..va + n].copy_from_slice(&image[ptr..ptr + n]);
    }
    Ok((mapped, base64 as u32, (base64 >> 32) as u32, size))
}

fn exe_mode(args: &[String]) {
    let image = std::fs::read(&args[2]).unwrap();
    let lo = u32::from_str_radix(args[3].trim_start_matches("0x"), 16).unwrap();
    let hi = u32::from_str_radix(args[4].trim_start_matches("0x"), 16).unwrap();
    let count: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let seed: u64 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(1);
    let (mapped, alias, rip_hi, size) = match map_pe(&image) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("{}: {e}", args[2]);
            std::process::exit(2);
        }
    };
    if lo < alias || hi <= lo || (hi - alias) as usize > mapped.len() {
        eprintln!("{:#x}..{:#x} is not inside the image ({:#x}..{:#x})", lo, hi, alias, alias as u64 + size as u64);
        std::process::exit(2);
    }
    let top = alias as usize + size as usize;
    LIMIT.store(top as u32, std::sync::atomic::Ordering::Relaxed);
    let mut config = Config::new();
    config.wasm_threads(true);
    config.consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let pages = (top + 0xffff) / 65536;
    let memory = SharedMemory::new(&engine, MemoryType::shared(koji::jit::wasm::memory_min_pages().max(pages as u32), koji::jit::wasm::memory_min_pages().max(pages as u32))).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let m = base as *mut u8;
    // instruction boundaries in the range
    let mut starts = Vec::new();
    {
        let off = (lo - alias) as usize;
        let mut dec = iced_x86::Decoder::with_ip(64, &mapped[off..(hi - alias) as usize], lo as u64 | (rip_hi as u64) << 32, iced_x86::DecoderOptions::NONE);
        for ins in &mut dec {
            if !ins.is_invalid() && ins.mnemonic() != iced_x86::Mnemonic::Int3 {
                starts.push(ins.ip() as u32);
            }
        }
    }
    if starts.is_empty() {
        eprintln!("no instructions in {lo:#x}..{hi:#x}");
        std::process::exit(2);
    }
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let (mut fails, mut compared) = (0, 0);
    let outside = |c: &Cpu| c.rip_hi != rip_hi || c.eip < lo || c.eip >= hi;
    for t in 0..count {
        let start = starts[rng.below(starts.len() as u64) as usize];
        let data: Vec<u8> = (0..0x10000).map(|_| rng.next() as u8).collect();
        let mut cpu0 = initial(&mut rng, rip_hi);
        cpu0.eip = start;
        for i in [0usize, 1, 2, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15] {
            match rng.below(3) {
                0 => cpu0.set_gpr(i, DATA as u64 + rng.below(0xf000)),
                1 => cpu0.set_gpr(i, rng.below(64)),
                _ => {}
            }
        }
        cpu0.set_gpr(3, DATA as u64 + rng.below(0xf000));
        cpu0.set_gpr(5, DATA as u64 + rng.below(0xf000));
        let setup = || unsafe {
            std::ptr::write_bytes(m.add(0x10000), 0, MEM - 0x10000);
            std::ptr::copy_nonoverlapping(data.as_ptr(), m.add(DATA as usize), data.len());
            std::ptr::copy_nonoverlapping(mapped.as_ptr(), m.add(alias as usize), mapped.len());
            put_cpu(m, &cpu0);
        };
        setup();
        let mut c = cpu0.clone();
        let mut ok = false;
        for _ in 0..20000 {
            if koji::step(&mut c, &mut Mem(m)) != Exit::Continue {
                break;
            }
            if outside(&c) {
                ok = true;
                break;
            }
        }
        if !ok {
            continue;
        }
        let snap = |m: *mut u8| unsafe { (std::slice::from_raw_parts(m.add(DATA as usize), 0x10000).to_vec(), std::slice::from_raw_parts(m.add(STACK as usize - 0x1000), 0x1000).to_vec(), std::slice::from_raw_parts(m.add(alias as usize), mapped.len()).to_vec()) };
        let want_mem = snap(m);
        let want = c;
        setup();
        let mut jit = Jit::new(&engine, &memory, base);
        jit.range = (lo, hi);
        let mut done = false;
        for _ in 0..20000 {
            let c = unsafe { get_cpu(m) };
            if outside(&c) {
                done = true;
                break;
            }
            if !jit.entries.contains_key(&c.eip) && jit.translate(base, c.eip, c.rip_hi).is_err() {
                break;
            }
            let (f, k, _) = jit.entries[&c.eip];
            let mut params = vec![Val::I32(0); 16];
            params[0] = Val::I32(CPU as i32);
            params[1] = Val::I32((k << koji::jit::ENTRY_SHIFT) as i32);
            let mut results = vec![Val::I32(0); 1];
            if f.call(&mut jit.store, &params, &mut results).is_err() {
                break;
            }
            match results[0].unwrap_i32() {
                0 => {}
                1 => {
                    let mut c = unsafe { get_cpu(m) };
                    let r = koji::step(&mut c, &mut Mem(m));
                    unsafe { put_cpu(m, &c) };
                    if r != Exit::Continue {
                        break;
                    }
                }
                _ => break,
            }
        }
        if !done {
            continue;
        }
        compared += 1;
        let mut got = unsafe { get_cpu(m) };
        let got_mem = snap(m);
        let mut w = want.clone();
        w.materialize();
        got.materialize();
        let mut d = Vec::new();
        for i in 0..16 {
            if w.gpr(i) != got.gpr(i) {
                d.push(format!("r{i}: want {:#x} got {:#x}", w.gpr(i), got.gpr(i)));
            }
        }
        if (w.eip, w.rip_hi) != (got.eip, got.rip_hi) {
            d.push(format!("rip want {:#x} got {:#x}", w.eip, got.eip));
        }
        if w.eflags & 0x8d5 != got.eflags & 0x8d5 {
            d.push(format!("flags want {:#x} got {:#x}", w.eflags & 0x8d5, got.eflags & 0x8d5));
        }
        for (name, a, b) in [("data", &want_mem.0, &got_mem.0), ("stack", &want_mem.1, &got_mem.1), ("image", &want_mem.2, &got_mem.2)] {
            if a != b {
                let i = a.iter().zip(b.iter()).position(|(x, y)| x != y).unwrap();
                d.push(format!("{name} differs at +{i:#x}"));
            }
        }
        if !d.is_empty() {
            fails += 1;
            if fails <= 10 {
                let mut c0 = cpu0.clone();
                c0.materialize();
                println!("test {t} from {start:#x}: {}\n    init {:x?} flags {:#x}", d.join("; "), (0..16).map(|i| c0.gpr(i)).collect::<Vec<_>>(), c0.eflags);
            }
        }
    }
    println!("{count} tests, {compared} compared, {fails} failed");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("exe") {
        return exe_mode(&args);
    }
    if args.get(1).map(|s| s.as_str()) == Some("native") {
        return native::run(&args);
    }
    let count: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(500);
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
    let mut config = Config::new();
    config.wasm_threads(true);
    config.consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, MemoryType::shared(koji::jit::wasm::memory_min_pages(), koji::jit::wasm::memory_min_pages())).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let mut fails = 0;
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    for t in 0..count {
        let rip_hi = (t % 3 == 2) as u32;
        EXACT.store(t / 2 % 3 != 2, Relaxed);
        // (every other test in small regions: exits chain into other regions and back into a
        // region's own blocks through the hash table)
        // (JALLREGS: regions load and store all sixteen registers, as before they named theirs)
        koji::jit::long::DEBUG_ALL_REGS.store(std::env::var("JALLREGS").is_ok(), Relaxed);
        // (two tests in three with the regions' blocks nested (STRUCTURED), the third dispatched
        // on a number in one loop (JFLAT: all of them): both layouts must agree with the
        // interpreter)
        koji::jit::long::STRUCTURED.store(std::env::var("JFLAT").is_err() && t % 3 != 2, Relaxed);
        // (and of the nested ones, every other three without entries inside loops)
        koji::jit::long::DEBUG_NO_SIDE.store(t / 3 % 2 == 1, Relaxed);
        koji::jit::long::DEBUG_BLOCKS.store(if t % 2 == 1 { 1 + t / 2 % 3 } else { 0 }, std::sync::atomic::Ordering::Relaxed);
        // (every other pair of tests: stores checked against the code, some into it)
        WATCH.store(t / 4 % 2 == 0, Relaxed);
        // (the other pairs, half of them: code checked on entry, some programs after another
        // one's translations at the same address)
        FRESH.store(t / 4 % 2 == 1 && t / 8 % 2 == 0, Relaxed);
        let mut list = Vec::new();
        let n = 5 + rng.below(40) as usize;
        while list.len() < n {
            random_ins(&mut rng, &mut list, rip_hi);
        }
        let mut sub = Vec::new();
        if rng.below(3) == 0 {
            let k = 1 + rng.below(6) as usize;
            while sub.len() < k {
                random_ins(&mut rng, &mut sub, rip_hi);
            }
        }
        let mut list = structure(&mut rng, list, sub, rip_hi);
        let data: Vec<u8> = (0..DATA_LEN).map(|_| rng.next() as u8).collect();
        let cpu0 = initial(&mut rng, rip_hi);
        let pre = if FRESH.load(Relaxed) && rng.below(2) == 0 {
            let mut other = Vec::new();
            let n = 5 + rng.below(40) as usize;
            while other.len() < n {
                random_ins(&mut rng, &mut other, rip_hi);
            }
            Some((structure(&mut rng, other, Vec::new(), rip_hi), initial(&mut rng, rip_hi)))
        } else {
            None
        };
        let pre = pre.as_ref().map(|(l, c)| (l.as_slice(), c));
        // (JONLY=<test>: that one alone)
        if std::env::var("JONLY").is_ok_and(|o| o != t.to_string()) {
            continue;
        }
        let (mut diffs, mut code) = run_case(&engine, &memory, base, &list, &data, &cpu0, rip_hi, pre);
        // (JNOSHRINK: the program as generated)
        if !diffs.is_empty() && std::env::var("JNOSHRINK").is_err() {
            // shrink: drop instructions while it still fails
            let mut i = 0;
            while i < list.len() {
                if list[i].ip() == 1 || inner(&list, i) || list[i].code() == Code::Retnq_imm16 || (list[i].op0_kind() == iced_x86::OpKind::Memory && list[i].memory_base() == Register::RBX && list[i].memory_displacement64() == COUNTER) {
                    i += 1;
                    continue;
                }
                let mut l2 = list.clone();
                l2.remove(i);
                if !run_case(&engine, &memory, base, &l2, &data, &cpu0, rip_hi, pre).0.is_empty() {
                    list = l2;
                } else {
                    i += 1;
                }
            }
            (diffs, code) = run_case(&engine, &memory, base, &list, &data, &cpu0, rip_hi, pre);
        }
        let rip = CODE as u64 | (rip_hi as u64) << 32;
        if !diffs.is_empty() {
            fails += 1;
            if fails <= 10 {
                println!("test {t} (rip_hi {rip_hi}): {}", diffs.join("; "));
                println!("    init: {:x?} flags {:#x}", (0..16).map(|i| cpu0.gpr(i)).collect::<Vec<_>>(), cpu0.eflags);
                let mut dec = iced_x86::Decoder::with_ip(64, &code, rip, iced_x86::DecoderOptions::NONE);
                let mut f = iced_x86::FastFormatter::new();
                let mut s = String::new();
                for ins in &mut dec {
                    s.clear();
                    f.format(&ins, &mut s);
                    println!("    {:#x} {s}", ins.ip());
                }
            }
        }
    }
    println!("{} tests, {fails} failed ({} stale regions found)", count, STALE_HITS.load(Relaxed));
    if fails > 0 {
        std::process::exit(1);
    }
}

#[test]
fn looping_region_is_bounded() {
    let mut config = Config::new();
    config.wasm_threads(true).consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let pages = koji::jit::wasm::memory_min_pages();
    let memory = SharedMemory::new(&engine, MemoryType::shared(pages, pages)).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let mut cpu = initial(&mut Rng(1), 0);
    cpu.eip = CODE;
    // (the JIT's MXCSR: with another one (initial's are random) the interpreter runs the loop)
    cpu.mxcsr = 0x1f80;
    unsafe {
        // jmp to itself: no END, no return to the host's outer step counter.
        std::ptr::copy_nonoverlapping([0xeb, 0xfe].as_ptr(), (base + CODE as usize) as *mut u8, 2);
        put_cpu(base as *mut u8, &cpu);
    }
    let mut jit = Jit::new(&engine, &memory, base);
    jit.store.set_fuel(10_000).unwrap();
    let error = jit.run(base).unwrap_err();
    assert!(error.contains("fuel"), "{error}");
}

#[test]
fn sse_encodes() {
    let mut rng = Rng(7);
    println!("{} codes", sse_codes().len());
    let (mut ok, mut bad) = (0, 0);
    for _ in 0..2000 {
        let Some(i) = random_sse(&mut rng) else { continue };
        match encode(&[i], CODE as u64) {
            Some(_) => ok += 1,
            None => {
                bad += 1;
                if bad < 5 {
                    println!("{:?} ds {} d {:#x} k {:?} {:?}", i.code(), i.memory_displ_size(), i.memory_displacement64(), (0..i.op_count()).map(|k| i.op_kind(k)).collect::<Vec<_>>(), BlockEncoder::encode(64, InstructionBlock::new(&[i], CODE as u64), BlockEncoderOptions::NONE).err());
                }
            }
        }
    }
    println!("ok {ok} bad {bad}");
}

/// Code that rewrites itself inside one region (issue #80): `mov eax, imm32` whose immediate
/// the loop's `add byte [rip+...], 1` raises each time around. rdx: the sum of what EAX was.
#[cfg(test)]
fn self_modifying(watch: bool) -> u64 {
    let mut config = Config::new();
    config.wasm_threads(true).consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let pages = koji::jit::wasm::memory_min_pages();
    let memory = SharedMemory::new(&engine, MemoryType::shared(pages, pages)).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    // (the immediate's address: the loop's first instruction + 1)
    let imm = MemoryOperand::new(Register::RIP, Register::None, 1, CODE as i64 + 5 + 1, 8, false, Register::None);
    let mut list = vec![
        Instruction::with2(Code::Mov_r32_imm32, Register::ECX, 3u32).unwrap(),
        Instruction::with2(Code::Mov_r32_imm32, Register::EAX, 1u32).unwrap(),
        Instruction::with2(Code::Add_rm8_imm8, imm, 1u32).unwrap(),
        Instruction::with2(Code::Add_rm64_r64, Register::RDX, Register::RAX).unwrap(),
        Instruction::with1(Code::Dec_rm32, Register::ECX).unwrap(),
        Instruction::with_branch(Code::Jne_rel32_64, LBASE).unwrap(),
        Instruction::with_branch(Code::Jmp_rel32_64, END as u64).unwrap(),
    ];
    // (the loop's label)
    list[1].set_ip(LBASE);
    let code = encode(&list, CODE as u64).unwrap();
    // (the loop starts at +5: mov ecx is 5 bytes, its mov eax's immediate at +6)
    assert_eq!(&code[5..7], &[0xb8, 1]);
    let mut cpu = Cpu { mode64: 1, eip: CODE, jit_budget: 1 << 30, ..Cpu::default() };
    cpu.set_gpr(4, (STACK - 0x800) as u64);
    unsafe {
        std::ptr::write_bytes((base + 0x10000) as *mut u8, 0, MEM - 0x10000);
        std::ptr::copy_nonoverlapping(code.as_ptr(), (base + CODE as usize) as *mut u8, code.len());
        put_cpu(base as *mut u8, &cpu);
    }
    WATCH.store(watch, Relaxed);
    let mut jit = Jit::new(&engine, &memory, base);
    jit.run(base).unwrap();
    WATCH.store(false, Relaxed);
    let c = unsafe { get_cpu(base as *mut u8) };
    assert_eq!(c.eip, END);
    c.gpr(2)
}

#[test]
fn self_modifying_code_in_a_region() {
    // the interpreter: 1 + 2 + 3
    assert_eq!(self_modifying(true), 6);
    // (unwatched, the region runs its translation of the code as it was: the test sees it)
    assert_eq!(self_modifying(false), 3);
}

/// MSVC's CRT has `bnd ret` / `bnd jne` (F2: MPX's BND, ignored with MPX off) in memcpy and
/// __security_check_cookie, and AMD's idiom is `rep ret` (F3): the JIT translates them as
/// branches, calls and returns without the prefix. They went to the interpreter one by one
/// before, and Unity's Vorbis decoding (a cookie check per call) ran some 30 times slower.
#[test]
fn ignored_prefixes_on_branches_stay_translated() {
    let mut config = Config::new();
    config.wasm_threads(true).consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let pages = koji::jit::wasm::memory_min_pages();
    let memory = SharedMemory::new(&engine, MemoryType::shared(pages, pages)).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let jmp = (END - (CODE + 26)).to_le_bytes();
    #[rustfmt::skip]
    let code: Vec<u8> = [
        &[0xb9, 100, 0, 0, 0][..],          // 0: mov ecx, 100
        &[0xe8, 16, 0, 0, 0],               // 5: call f (26)
        &[0xe8, 17, 0, 0, 0],               // 10: call g (32)
        &[0xff, 0xc9],                      // 15: dec ecx
        &[0xf2, 0x75, 0xf1],                // 17: bnd jne 5
        &[0xf2, 0xe9], &jmp,                // 20: bnd jmp END
        &[0x48, 0x83, 0xc0, 0x01, 0xf2, 0xc3], // 26: f: add rax, 1; bnd ret
        &[0x48, 0x83, 0xc2, 0x02, 0xf3, 0xc3], // 32: g: add rdx, 2; rep ret
    ].concat();
    let ins: Vec<_> = iced_x86::Decoder::with_ip(64, &code, CODE as u64, iced_x86::DecoderOptions::NONE).into_iter().collect();
    assert_eq!(ins.iter().filter(|i| koji::jit::rep_ignored(i) && (i.has_repne_prefix() || i.has_rep_prefix())).count(), 4);
    let mut cpu = Cpu { mode64: 1, eip: CODE, jit_budget: 1 << 30, mxcsr: 0x1f80, ..Cpu::default() };
    cpu.set_gpr(4, (STACK - 0x800) as u64);
    // the interpreter alone
    let mut want = cpu.clone();
    let mut m = vec![0u8; MEM];
    m[CODE as usize..CODE as usize + code.len()].copy_from_slice(&code);
    while want.eip != END {
        assert_eq!(koji::step(&mut want, &mut Mem(m.as_mut_ptr())), Exit::Continue);
    }
    assert_eq!((want.gpr(0), want.gpr(2), want.gpr(1)), (100, 200, 0));
    unsafe {
        std::ptr::write_bytes((base + 0x10000) as *mut u8, 0, MEM - 0x10000);
        std::ptr::copy_nonoverlapping(code.as_ptr(), (base + CODE as usize) as *mut u8, code.len());
        put_cpu(base as *mut u8, &cpu);
    }
    let mut jit = Jit::new(&engine, &memory, base);
    STEPS.set(0);
    jit.run(base).unwrap();
    let c = unsafe { get_cpu(base as *mut u8) };
    assert_eq!(c.eip, END);
    assert_eq!((c.gpr(0), c.gpr(2), c.gpr(1), c.gpr(4)), (want.gpr(0), want.gpr(2), want.gpr(1), want.gpr(4)));
    assert_eq!(STEPS.get(), 0, "no instruction left to the interpreter");
}

/// MOVNTPS / MOVNTPD / MOVNTDQ (memcpy's large copies in MSVC's CRT) are stores for the
/// program: translated as MOVUPS, not left to the interpreter one by one.
#[test]
fn non_temporal_stores_stay_translated() {
    let mut config = Config::new();
    config.wasm_threads(true).consume_fuel(true);
    let engine = Engine::new(&config).unwrap();
    let pages = koji::jit::wasm::memory_min_pages();
    let memory = SharedMemory::new(&engine, MemoryType::shared(pages, pages)).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let jmp = (END - (CODE + 27)).to_le_bytes();
    #[rustfmt::skip]
    let code: Vec<u8> = [
        &[0x0f, 0x2b, 0x0b][..],            // 0: movntps [rbx], xmm1
        &[0x66, 0x0f, 0x2b, 0x53, 0x10],    // 3: movntpd [rbx+16], xmm2
        &[0x66, 0x44, 0x0f, 0xe7, 0x43, 0x20], // 8: movntdq [rbx+32], xmm8
        &[0x0f, 0x2b, 0x5b, 0x30],          // 14: movntps [rbx+48], xmm3
        &[0x90, 0x90, 0x90],                // 18
        &[0x90],                            // 21
        &[0xe9], &jmp,                      // 22: jmp END
    ].concat();
    let mut cpu = Cpu { mode64: 1, eip: CODE, jit_budget: 1 << 30, mxcsr: 0x1f80, ..Cpu::default() };
    cpu.set_gpr(3, DATA as u64);
    cpu.set_gpr(4, (STACK - 0x800) as u64);
    cpu.xmm[1] = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;
    cpu.xmm[2] = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;
    cpu.xmm[3] = !cpu.xmm[1];
    cpu.xmm8[0] = 0xdead_beef_0000_0000_1234_5678_9abc_def0;
    unsafe {
        std::ptr::write_bytes((base + 0x10000) as *mut u8, 0, MEM - 0x10000);
        std::ptr::copy_nonoverlapping(code.as_ptr(), (base + CODE as usize) as *mut u8, code.len());
        put_cpu(base as *mut u8, &cpu);
    }
    let mut jit = Jit::new(&engine, &memory, base);
    STEPS.set(0);
    jit.run(base).unwrap();
    let c = unsafe { get_cpu(base as *mut u8) };
    assert_eq!(c.eip, END);
    let got = unsafe { std::slice::from_raw_parts((base + DATA as usize) as *const u8, 64) };
    let want: Vec<u8> = [cpu.xmm[1], cpu.xmm[2], cpu.xmm8[0], cpu.xmm[3]].iter().flat_map(|x| x.to_le_bytes()).collect();
    assert_eq!(got, &want[..]);
    assert_eq!(STEPS.get(), 0, "no instruction left to the interpreter");
}

/// A small PE32+ image: two sections, the second's bytes at its RVA.
#[cfg(test)]
fn test_pe() -> Vec<u8> {
    let mut f = vec![0u8; 0x600];
    f[0..2].copy_from_slice(b"MZ");
    f[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
    let pe = 0x80;
    f[pe..pe + 4].copy_from_slice(b"PE\0\0");
    f[pe + 6..pe + 8].copy_from_slice(&2u16.to_le_bytes());
    f[pe + 20..pe + 22].copy_from_slice(&0xf0u16.to_le_bytes());
    f[pe + 48..pe + 56].copy_from_slice(&0x1_4000_0000u64.to_le_bytes());
    f[pe + 80..pe + 84].copy_from_slice(&0x3000u32.to_le_bytes());
    f[pe + 84..pe + 88].copy_from_slice(&0x200u32.to_le_bytes());
    for (i, (va, raw, ptr)) in [(0x1000u32, 0x200u32, 0x200u32), (0x2000, 0x200, 0x400)].into_iter().enumerate() {
        let sh = pe + 24 + 0xf0 + i * 40;
        f[sh + 12..sh + 16].copy_from_slice(&va.to_le_bytes());
        f[sh + 16..sh + 20].copy_from_slice(&raw.to_le_bytes());
        f[sh + 20..sh + 24].copy_from_slice(&ptr.to_le_bytes());
    }
    for (k, b) in f[0x200..0x600].iter_mut().enumerate() {
        *b = (k * 7 + 1) as u8;
    }
    f
}

#[test]
fn pe_mapped_at_rvas() {
    let f = test_pe();
    let (m, lo, hi, size) = map_pe(&f).unwrap();
    assert_eq!((lo, hi, size, m.len()), (0x4000_0000, 1, 0x3000, 0x3000));
    assert_eq!(&m[0x1000..0x1200], &f[0x200..0x400]);
    assert_eq!(&m[0x2000..0x2200], &f[0x400..0x600]);
    assert_eq!(&m[..0x200], &f[..0x200]);
}

/// Cut and damaged images (random bytes over every header field, random lengths): an error or
/// an image of the size the header says, never a panic (fuzz, seeded).
#[test]
fn pe_damaged_never_panics() {
    let good = test_pe();
    let mut rng = Rng(0x5eed);
    let (mut ok, mut bad) = (0, 0);
    for _ in 0..20_000 {
        let mut f = good.clone();
        for _ in 0..1 + rng.below(6) {
            let at = if rng.below(2) == 0 { 0x80 + rng.below(0x1a0) as usize } else { rng.below(f.len() as u64) as usize };
            f[at] = rng.next() as u8;
        }
        if rng.below(3) == 0 {
            f.truncate(rng.below(f.len() as u64 + 1) as usize);
        }
        match std::panic::catch_unwind(|| map_pe(&f)) {
            Ok(Ok((m, lo, _, size))) => {
                assert_eq!(m.len(), size as usize);
                assert!(lo as u64 + size as u64 <= 1 << 32);
                ok += 1;
            }
            Ok(Err(_)) => bad += 1,
            Err(_) => panic!("map_pe panicked on {:?}", &f[..f.len().min(0x1c0)]),
        }
    }
    assert!(ok > 1000 && bad > 1000, "{ok} mapped, {bad} refused");
}
