//! AVX/AVX2 forms that are no legacy form run on each 128-bit lane (long/sse.rs does those):
//! moves between lanes (VINSERTF128, VEXTRACTF128, VPERM2F128, VPERMQ/PD, VPERMD/PS), broadcasts,
//! VPERMILPS/PD, VPBLENDD, the variable shifts (VPSLLV/VPSRLV/VPSRAV), VPMOVZX/SX into a YMM
//! register, the YMM forms of VPMOVMSKB/VMOVMSKPS/PD and VZEROALL. None of them touches MXCSR
//! (moves of floats too: no exception, no rounding). A YMM register is its XMM register (the
//! lower lanes, `Cpu::xmm`/`xmm8`) and `Cpu::ymmh` (the upper ones); a VEX.128 form clears the
//! upper half of its destination. Results are built in L_V0 (lower) and L_V1 (upper) before
//! either is stored, so a destination that is also a source reads the old value.

use super::{Gen, I64Ops, I32_WRAP, I64_EXT_U, L_A, L_CPU, L_V0, L_V1, L_V2};
use crate::Cpu;
use core::mem::offset_of;
use iced_x86::{Instruction, Mnemonic, OpKind, Register};

/// The Cpu offsets of vector register `r`'s halves (xmm or ymm, by number).
fn halves(r: Register) -> (u32, u32) {
    let i = r.number();
    let lo = if i < 8 { offset_of!(Cpu, xmm) + 16 * i } else { offset_of!(Cpu, xmm8) + 16 * (i - 8) };
    (lo as u32, (offset_of!(Cpu, ymmh) + 16 * i) as u32)
}

fn reg(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Register && (ins.op_register(i).is_xmm() || ins.op_register(i).is_ymm())
}
fn ymm(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_ymm()
}
fn mem(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Memory
}
fn gpr32(ins: &Instruction, i: u32) -> bool {
    ins.op_kind(i) == OpKind::Register && (ins.op_register(i).is_gpr32() || ins.op_register(i).is_gpr64())
}
fn imm(ins: &Instruction, i: u32) -> bool {
    ins.op_count() == i + 1 && ins.op_kind(i) == OpKind::Immediate8
}

// SIMD opcodes
const SWIZZLE: u32 = 0x0e;
const AND: u32 = 0x4e;
const OR: u32 = 0x50;
const I8X16_ADD: u32 = 0x6e;
const I8X16_SUB: u32 = 0x71;
const I32X4_MUL: u32 = 0xb5;

/// Whether [`Gen::avx`] translates `ins`.
pub(super) fn supported(ins: &Instruction) -> bool {
    use Mnemonic::*;
    if ins.encoding() != iced_x86::EncodingKind::VEX {
        return false;
    }
    let n = ins.op_count();
    let m = ins.mnemonic();
    let rm = |i: u32| reg(ins, i) || mem(ins, i);
    let wide = ymm(ins, 0);
    match m {
        Vzeroall => true,
        Vinsertf128 | Vinserti128 => n == 4 && ymm(ins, 0) && ymm(ins, 1) && rm(2) && imm(ins, 3),
        Vextractf128 | Vextracti128 => n == 3 && (reg(ins, 0) || mem(ins, 0)) && ymm(ins, 1) && imm(ins, 2),
        Vperm2f128 | Vperm2i128 => n == 4 && wide && ymm(ins, 1) && rm(2) && imm(ins, 3),
        Vbroadcastss | Vbroadcastsd | Vbroadcastf128 | Vbroadcasti128 | Vpbroadcastb | Vpbroadcastw | Vpbroadcastd | Vpbroadcastq => n == 2 && reg(ins, 0) && rm(1),
        Vpermq | Vpermpd => n == 3 && wide && rm(1) && imm(ins, 2),
        Vpermd | Vpermps => n == 3 && wide && ymm(ins, 1) && rm(2),
        Vpermilps | Vpermilpd => n == 3 && reg(ins, 0) && (imm(ins, 2) && rm(1) || reg(ins, 1) && rm(2)),
        Vpblendd => n == 4 && reg(ins, 0) && reg(ins, 1) && rm(2) && imm(ins, 3),
        Vpsllvd | Vpsllvq | Vpsrlvd | Vpsrlvq | Vpsravd => n == 3 && reg(ins, 0) && reg(ins, 1) && rm(2),
        Vpmovmskb | Vmovmskps | Vmovmskpd => n == 2 && gpr32(ins, 0) && ymm(ins, 1),
        Vpmovzxbw | Vpmovzxbd | Vpmovzxbq | Vpmovzxwd | Vpmovzxwq | Vpmovzxdq | Vpmovsxbw | Vpmovsxbd | Vpmovsxbq | Vpmovsxwd | Vpmovsxwq
        | Vpmovsxdq => n == 2 && wide && rm(1),
        _ => false,
    }
}

impl Gen<'_> {
    /// Pushes half `h` (0 lower, 1 upper) of vector operand `i` (a register, or memory at L_A:
    /// its bytes 16h..16h+16).
    fn half(&mut self, i: u32, h: u32) {
        if self.ins.op_kind(i) == OpKind::Memory {
            self.c.get(L_A).v128load(16 * h);
        } else {
            let (lo, hi) = halves(self.ins.op_register(i));
            self.c.get(L_CPU).v128load(if h == 0 { lo } else { hi });
        }
    }

    /// Stores L_V0 (and L_V1 for a YMM register, else zeros: VEX.128) to vector register `d`.
    fn put(&mut self, d: Register) {
        let (lo, hi) = halves(d);
        self.c.get(L_CPU).get(L_V0).v128store(lo);
        self.c.get(L_CPU);
        if d.is_ymm() {
            self.c.get(L_V1);
        } else {
            self.c.v128zero();
        }
        self.c.v128store(hi);
    }

    /// Translates an instruction [`supported`] accepts; false (nothing emitted) if it can't.
    pub(super) fn avx(&mut self) -> bool {
        use Mnemonic::*;
        let ins = self.ins;
        if !supported(&ins) {
            return false;
        }
        if (0..ins.op_count()).any(|i| mem(&ins, i)) {
            if !self.addr(ins.memory_size().size().max(1) as u32) {
                return false;
            }
        }
        let m = ins.mnemonic();
        let wide = ymm(&ins, 0);
        let lanes = if wide { 2 } else { 1 };
        match m {
            Vzeroall => {
                for r in 0..16 {
                    let (lo, hi) = halves(Register::try_from(Register::XMM0 as usize + r).unwrap());
                    self.c.get(L_CPU).v128zero().v128store(lo);
                    self.c.get(L_CPU).v128zero().v128store(hi);
                }
            }
            Vinsertf128 | Vinserti128 => {
                let up = ins.immediate8() & 1 != 0;
                if up {
                    self.half(1, 0);
                    self.c.set(L_V0);
                    self.half(2, 0);
                    self.c.set(L_V1);
                } else {
                    self.half(2, 0);
                    self.c.set(L_V0);
                    self.half(1, 1);
                    self.c.set(L_V1);
                }
                self.put(ins.op_register(0));
            }
            Vextractf128 | Vextracti128 => {
                let h = (ins.immediate8() & 1) as u32;
                if mem(&ins, 0) {
                    self.c.get(L_A);
                    self.half(1, h);
                    self.c.v128store(0);
                } else {
                    self.half(1, h);
                    self.c.set(L_V0);
                    self.put(ins.op_register(0));
                }
            }
            Vperm2f128 | Vperm2i128 => {
                let imm = ins.immediate8();
                for (k, l) in [(0, L_V0), (4, L_V1)] {
                    let s = imm >> k;
                    if s & 8 != 0 {
                        self.c.v128zero();
                    } else {
                        self.half(if s & 2 == 0 { 1 } else { 2 }, (s & 1) as u32);
                    }
                    self.c.set(l);
                }
                self.put(ins.op_register(0));
            }
            Vbroadcastss | Vbroadcastsd | Vbroadcastf128 | Vbroadcasti128 | Vpbroadcastb | Vpbroadcastw | Vpbroadcastd | Vpbroadcastq => {
                let (load, ext, splat) = match m {
                    Vpbroadcastb => (0x07, 0x16, 0x0f), // v128.load8_splat, i8x16.extract_lane_u, i8x16.splat
                    Vpbroadcastw => (0x08, 0x19, 0x10),
                    Vbroadcastss | Vpbroadcastd => (0x09, 0x1b, 0x11),
                    Vbroadcastsd | Vpbroadcastq => (0x0a, 0x1d, 0x12),
                    _ => (0, 0, 0), // (the 128 bits from memory)
                };
                if load == 0 {
                    self.half(1, 0);
                } else if mem(&ins, 1) {
                    self.c.get(L_A).simd_mem(load, 0);
                } else {
                    self.half(1, 0);
                    self.c.lane(ext, 0).simd(splat);
                }
                self.c.tee(L_V0).set(L_V1);
                self.put(ins.op_register(0));
            }
            Vpermq | Vpermpd => {
                // (every qword of the result picks one of the source's four: a shuffle of its halves)
                let imm = ins.immediate8();
                for (k, l) in [(0, L_V0), (4, L_V1)] {
                    self.half(1, 0);
                    self.half(1, 1);
                    let q = [(imm >> k) & 3, (imm >> (k + 2)) & 3];
                    self.c.shuffle(core::array::from_fn(|b| q[b / 8] * 8 + (b % 8) as u8));
                    self.c.set(l);
                }
                self.put(ins.op_register(0));
            }
            Vpermd | Vpermps => {
                // byte indices into the 32 bytes of operand 2: 4 * (index & 7) + 0..3
                for (h, l) in [(0, L_V0), (1, L_V1)] {
                    self.half(1, h);
                    self.c.v128c([7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0, 7, 0, 0, 0]).simd(AND);
                    self.c.v128c([4; 16]).simd(I32X4_MUL);
                    self.c.v128c([0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]).simd(I8X16_ADD).set(L_V2);
                    self.half(2, 0);
                    self.c.get(L_V2).simd(SWIZZLE);
                    self.half(2, 1);
                    self.c.get(L_V2).v128c([16; 16]).simd(I8X16_SUB).simd(SWIZZLE).simd(OR).set(l);
                }
                self.put(ins.op_register(0));
            }
            Vpermilps | Vpermilpd => {
                let ps = m == Vpermilps;
                if ins.op_kind(2) == OpKind::Immediate8 {
                    let imm = ins.immediate8();
                    for (h, l) in [(0, L_V0), (1, L_V1)].into_iter().take(lanes) {
                        self.half(1, h);
                        self.c.get(L_V0); // (unused second input)
                        let lanes: [u8; 16] = if ps {
                            core::array::from_fn(|b| ((imm >> (2 * (b / 4))) & 3) * 4 + (b % 4) as u8)
                        } else {
                            core::array::from_fn(|b| ((imm >> (2 * h + (b / 8) as u32)) & 1) * 8 + (b % 8) as u8)
                        };
                        self.c.shuffle(lanes).set(l);
                    }
                } else {
                    // control in operand 2: PS bits 1:0 of each dword, PD bit 1 of each qword
                    for (h, l) in [(0, L_V0), (1, L_V1)].into_iter().take(lanes) {
                        self.half(1, h);
                        self.half(2, h);
                        if ps {
                            self.c.v128c([3, 0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0, 3, 0, 0, 0]).simd(AND);
                            self.c.v128c([4; 16]).simd(I32X4_MUL);
                            self.c.v128c([0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3, 0, 1, 2, 3]).simd(I8X16_ADD);
                        } else {
                            // (bit 1 of the qword as 8 in each of its bytes, + 0..7)
                            self.c.v128c([2, 0, 0, 0, 0, 0, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0]).simd(AND);
                            self.c.i32c(2).simd(0xcb).tee(L_V2).get(L_V2); // i64x2.shl
                            self.c.shuffle([0, 0, 0, 0, 0, 0, 0, 0, 8, 8, 8, 8, 8, 8, 8, 8]);
                            self.c.v128c([0, 1, 2, 3, 4, 5, 6, 7, 0, 1, 2, 3, 4, 5, 6, 7]).simd(I8X16_ADD);
                        }
                        self.c.simd(SWIZZLE).set(l);
                    }
                }
                self.put(ins.op_register(0));
            }
            Vpblendd => {
                let imm = ins.immediate8();
                for (h, l) in [(0, L_V0), (1, L_V1)].into_iter().take(lanes) {
                    self.half(1, h);
                    self.half(2, h);
                    let sel = (imm >> (4 * h)) & 0xf;
                    self.c.shuffle(core::array::from_fn(|b| b as u8 + if sel >> (b / 4) & 1 != 0 { 16 } else { 0 })).set(l);
                }
                self.put(ins.op_register(0));
            }
            Vpsllvd | Vpsllvq | Vpsrlvd | Vpsrlvq | Vpsravd => {
                let q = matches!(m, Vpsllvq | Vpsrlvq);
                let (n, ext, rep) = if q { (2u8, 0x1d, 0x1e) } else { (4u8, 0x1b, 0x1c) };
                let (v, c) = (self.temp(), self.temp());
                for (h, l) in [(0, L_V0), (1, L_V1)].into_iter().take(lanes) {
                    self.half(1, h);
                    self.c.set(l);
                    for k in 0..n {
                        self.half(1, h);
                        self.c.lane(ext, k);
                        if !q {
                            self.c.x(I64_EXT_U);
                        }
                        self.c.set(v);
                        self.half(2, h);
                        self.c.lane(ext, k);
                        if !q {
                            self.c.x(I64_EXT_U);
                        }
                        self.c.set(c);
                        self.c.get(l);
                        // the shifted element; a count above the width: 0 (VPSRAVD: the sign)
                        match m {
                            Vpsravd => {
                                self.c.get(v).x(I32_WRAP).get(c).x(I32_WRAP).i32c(31).get(c).i64c(31).x(super::I64_LT_U).select().shr_s();
                            }
                            _ => {
                                let op = if matches!(m, Vpsllvd | Vpsllvq) { super::I64_SHL } else { super::I64_SHR_U };
                                if q {
                                    self.c.get(v).get(c).x(op).i64c(0).get(c).i64c(64).x(super::I64_LT_U).select();
                                } else {
                                    self.c.get(v).get(c).x(op).x(I32_WRAP).i32c(0).get(c).i64c(32).x(super::I64_LT_U).select();
                                }
                            }
                        }
                        self.c.lane(rep, k).set(l);
                    }
                }
                self.put(ins.op_register(0));
            }
            Vpmovmskb | Vmovmskps | Vmovmskpd => {
                let (op, shift) = match m { Vpmovmskb => (0x64, 16), Vmovmskps => (0xa4, 4), _ => (0xc4, 2) };
                self.half(1, 1);
                self.c.simd(op).i32c(shift).shl();
                self.half(1, 0);
                self.c.simd(op).or().x(I64_EXT_U);
                let t = self.temp();
                self.c.set(t);
                self.set_reg(ins.op_register(0), t);
            }
            _ => {
                // VPMOVZX / VPMOVSX into a YMM register: the low part of the source widened, in
                // steps (byte -> word -> dword -> qword); the upper lanes from the next elements
                let s = matches!(m, Vpmovsxbw | Vpmovsxbd | Vpmovsxbq | Vpmovsxwd | Vpmovsxwq | Vpmovsxdq);
                let (from, to) = match m {
                    Vpmovzxbw | Vpmovsxbw => (0, 1),
                    Vpmovzxbd | Vpmovsxbd => (0, 2),
                    Vpmovzxbq | Vpmovsxbq => (0, 3),
                    Vpmovzxwd | Vpmovsxwd => (1, 2),
                    Vpmovzxwq | Vpmovsxwq => (1, 3),
                    _ => (2, 3),
                };
                // extend_low / extend_high of i8x16, i16x8, i32x4 (signed; +2 unsigned)
                let low = |k: u32| [0x87, 0xa7, 0xc7][k as usize] + if s { 0 } else { 2 };
                if mem(&ins, 1) {
                    // (only the bytes the form reads: a 16-byte load could cross into an unmapped page)
                    let size = ins.memory_size().size();
                    self.c.get(L_A);
                    match size {
                        16 => self.c.v128load(0),
                        8 => self.c.v128load64(0),
                        _ => self.c.v128load32(0),
                    };
                } else {
                    self.half(1, 0);
                }
                for k in from..to - 1 {
                    self.c.simd(low(k));
                }
                self.c.set(L_V2);
                self.c.get(L_V2).simd(low(to - 1)).set(L_V0);
                self.c.get(L_V2).simd(low(to - 1) + 1).set(L_V1);
                self.put(ins.op_register(0));
            }
        }
        true
    }
}
