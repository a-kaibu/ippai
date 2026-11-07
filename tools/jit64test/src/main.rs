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
                let skip = ["Maskmov", "Vmaskmov", "Ldmxcsr", "Stmxcsr", "Aes", "Pclmul", "Sha", "Gf2p8", "Vcvtph2ps", "Vcvtps2ph", "Extrq", "Insertq", "Movnt", "Crc32", "Vfm", "Vfnm", "Vgather", "Vpgather", "Vpdpb", "Vpdpw", "Vpmadd52", "Kmov", "Vpbroadcast", "Vbroadcast", "Vperm", "Vtest", "Vpblendd", "Vpsllv", "Vpsrlv", "Vpsrav", "Vblendv", "Vpblendv", "Rdrand", "Rdseed", "Pconfig", "Bndc", "Bndm", "Senduipi"];
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
            let jne = label(Instruction::with_branch(Code::Jne_rel32_64, head).unwrap());
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
        let jcc = label(Instruction::with_branch(code, target).unwrap());
        list.insert(i, jcc);
    }
    let sub: Vec<Instruction> = sub.into_iter().map(&mut label).collect();
    if let Some(first) = sub.first().map(|i| i.ip()) {
        let call = label(Instruction::with_branch(Code::Call_rel32_64, first).unwrap());
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
    let mut jmp = Instruction::with_branch(Code::Jmp_rel32_64, end).unwrap();
    jmp.set_ip(1); // (kept by the shrinker)
    list.push(jmp);
    let has_sub = !sub.is_empty();
    list.extend(sub);
    if has_sub {
        let ret = label(Instruction::with1(Code::Retnq_imm16, 8u32).unwrap());
        list.push(ret);
    }
    list
}

/// the dispatcher's hash table the regions chain through (Cpu::jit_hash: HASH_N (eip, entry)
/// pairs), and the ring of jumps between regions (Cpu::jit_ring)
const HASH: u32 = 0x8000;
const HASH_N: u32 = 2048;
const RING: u32 = 0xc000;

struct Jit {
    engine: Engine,
    store: Store<()>,
    linker: Linker<()>,
    table: Table,
    /// eip -> (function, block, table index)
    entries: HashMap<u32, (Func, u32, u32)>,
    /// blocks a region may take in
    range: (u32, u32),
}

impl Jit {
    fn new(engine: &Engine, memory: &SharedMemory, base: usize) -> Jit {
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
                let r = koji::step(&mut c, &mut Mem(m));
                unsafe { put_cpu(m, &c) };
                match r {
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
        Jit { engine: engine.clone(), store, linker, table, entries: HashMap::new(), range: (0, u32::MAX) }
    }

    /// Points the Cpu at the hash table and the ring (regions jump to each other through them).
    fn chain(&self, base: usize) {
        let m = base as *mut u8;
        let mut c = unsafe { get_cpu(m) };
        c.jit_hash = HASH;
        c.jit_mask = HASH_N - 1;
        c.jit_ring = RING;
        unsafe { put_cpu(m, &c) };
    }

    fn translate(&mut self, base: usize, eip: u32, rip_hi: u32) -> Result<(), String> {
        let mut m = koji::jit::wasm::Module::default();
        let full = eip as u64 | (rip_hi as u64) << 32;
        let (lo, hi) = self.range;
        let region = koji::jit::long::translate_region64(&mut m, &mut Mem(base as *mut u8), &[full], (END, END + 16), |a, _| a != END && a >= lo && a < hi).ok_or("refused")?;
        let bytes = m.finish();
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
        for _ in 0..100_000 {
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
            if !koji::jit::sse_ok(c.mxcsr) {
                // (as kura: the interpreter while MXCSR isn't the JIT's)
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
                2 if !koji::jit::sse_ok(unsafe { get_cpu(m) }.mxcsr) || SWITCHED.load(Relaxed) => {}
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
    for i in 0..16 {
        let v: u128 = match rng.below(6) {
            0 => rng.next() as u128 | (rng.next() as u128) << 64,
            1 => (0..4).fold(0, |v, k| v | ((((rng.next() as i32) >> rng.below(31)) as f32 / 8.0).to_bits() as u128) << (32 * k)),
            2 => (0..2).fold(0, |v, k| v | ((((rng.next() as i64) >> rng.below(63)) as f64 / 8.0).to_bits() as u128) << (64 * k)),
            3 => (0..8).fold(0, |v, k| v | ((rng.below(512) as u16).wrapping_sub(256) as u128) << (16 * k)),
            // the MXCSR flags' edge cases: zeros, infinities, NaNs, denormals, range ends
            4 => (0..4).fold(0, |v, k| v | (if rng.below(2) == 0 { FS32[rng.below(FS32.len() as u64) as usize] } else { (1.5f32 * k as f32).to_bits() } as u128) << (32 * k)),
            _ => (0..2).fold(0, |v, k| v | (if rng.below(2) == 0 { FS64[rng.below(FS64.len() as u64) as usize] } else { (1.5 * k as f64).to_bits() } as u128) << (64 * k)),
        };
        if i < 8 {
            c.xmm[i] = v;
        } else {
            c.xmm8[i - 8] = v;
        }
        // the upper YMM halves: VEX.128 must clear the destination's, legacy SSE keep them
        c.ymmh[i] = rng.next() as u128 | (rng.next() as u128) << 64;
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

fn run_case(engine: &Engine, memory: &SharedMemory, base: usize, list0: &[Instruction], data: &[u8], cpu0: &Cpu, rip_hi: u32) -> (Vec<String>, Vec<u8>) {
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
        let reads = koji::jit::mx_switched();
        let exact = EXACT.load(Relaxed);
        koji::jit::MX_EXACT.store(exact, Relaxed);
        SWITCHED.store(false, Relaxed);
        let (mut want_cpu, want_data, want_stack) = unsafe { (get_cpu(m), std::slice::from_raw_parts(m.add(DATA as usize), DATA_LEN as usize).to_vec(), std::slice::from_raw_parts(m.add(STACK as usize - 0x1000), 0x1000).to_vec()) };
        setup(cpu0);
        let mut jit = Jit::new(engine, memory, base);
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
        if want.as_ref().is_err_and(|e| e == "no end") {
            // (a loop longer than the interpreter's step limit, which the JIT's regions run
            // through: nothing to compare)
            return (diffs, code);
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
fn exe_mode(args: &[String]) {
    let image = std::fs::read(&args[2]).unwrap();
    let lo = u32::from_str_radix(args[3].trim_start_matches("0x"), 16).unwrap();
    let hi = u32::from_str_radix(args[4].trim_start_matches("0x"), 16).unwrap();
    let count: usize = args.get(5).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let seed: u64 = args.get(6).and_then(|s| s.parse().ok()).unwrap_or(1);
    // PE: sections at their RVAs
    let pe = u32::from_le_bytes(image[0x3c..0x40].try_into().unwrap()) as usize;
    let nsec = u16::from_le_bytes(image[pe + 6..pe + 8].try_into().unwrap()) as usize;
    let opt = u16::from_le_bytes(image[pe + 20..pe + 22].try_into().unwrap()) as usize;
    let base64 = u64::from_le_bytes(image[pe + 24 + 24..pe + 24 + 32].try_into().unwrap());
    let size = u32::from_le_bytes(image[pe + 24 + 56..pe + 24 + 60].try_into().unwrap());
    let alias = base64 as u32;
    let rip_hi = (base64 >> 32) as u32;
    let mut mapped = vec![0u8; size as usize];
    let headers = u32::from_le_bytes(image[pe + 24 + 60..pe + 24 + 64].try_into().unwrap()) as usize;
    mapped[..headers].copy_from_slice(&image[..headers]);
    for i in 0..nsec {
        let sh = pe + 24 + opt + i * 40;
        let va = u32::from_le_bytes(image[sh + 12..sh + 16].try_into().unwrap()) as usize;
        let raw = u32::from_le_bytes(image[sh + 16..sh + 20].try_into().unwrap()) as usize;
        let ptr = u32::from_le_bytes(image[sh + 20..sh + 24].try_into().unwrap()) as usize;
        let n = raw.min(mapped.len() - va).min(image.len() - ptr);
        mapped[va..va + n].copy_from_slice(&image[ptr..ptr + n]);
    }
    let top = alias as usize + size as usize;
    LIMIT.store(top as u32, std::sync::atomic::Ordering::Relaxed);
    let mut config = Config::new();
    config.wasm_threads(true);
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
    let engine = Engine::new(&config).unwrap();
    let memory = SharedMemory::new(&engine, MemoryType::shared(koji::jit::wasm::memory_min_pages(), koji::jit::wasm::memory_min_pages())).unwrap();
    let base = memory.data().as_ptr() as *mut u8 as usize;
    let m = base as *mut u8;
    let mut fails = 0;
    let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    for t in 0..count {
        let rip_hi = (t % 3 == 2) as u32;
        EXACT.store(t / 2 % 3 != 2, Relaxed);
        // (every other test in small regions: exits chain into other regions and back into a
        // region's own blocks through the hash table)
        // (JALLREGS: regions load and store all sixteen registers, as before they named theirs)
        koji::jit::long::DEBUG_ALL_REGS.store(std::env::var("JALLREGS").is_ok(), Relaxed);
        koji::jit::long::DEBUG_BLOCKS.store(if t % 2 == 1 { 1 + t / 2 % 3 } else { 0 }, std::sync::atomic::Ordering::Relaxed);
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
        // (JONLY=<test>: that one alone)
        if std::env::var("JONLY").is_ok_and(|o| o != t.to_string()) {
            continue;
        }
        let (diffs, _) = run_case(&engine, &memory, base, &list, &data, &cpu0, rip_hi);
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
                if !run_case(&engine, &memory, base, &l2, &data, &cpu0, rip_hi).0.is_empty() {
                    list = l2;
                } else {
                    i += 1;
                }
            }
        }
        let (diffs, code) = run_case(&engine, &memory, base, &list, &data, &cpu0, rip_hi);
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
    println!("{} tests, {fails} failed", count);
    if fails > 0 {
        std::process::exit(1);
    }
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


