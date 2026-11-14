//! x87 instructions (see crate::fpu for the f64 model).

use super::X;
use crate::flags::{self, *};
use crate::f80::F80;
use crate::fpu::{self, C0, C1, C2, C3};
use crate::round::{self, Op};
use crate::{Exit, Memory, EAX};
use iced_x86::{MemorySize, Mnemonic, OpKind, Register};

macro_rules! tryf {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(f) => return Exit::Fault(f),
        }
    };
}

fn st_index(r: Register) -> u32 {
    r as u32 - Register::ST0 as u32
}

impl<'a, M: Memory> X<'a, M> {
    fn fload(&mut self) -> Result<f64, crate::Fault> {
        let a = self.addr();
        // (an SNaN loads as its QNaN with IE, as the x87 converts it; a denormal with DE)
        let ex = |snan: bool, den: bool| (if snan { fpu::IE } else { 0 }) | if den { fpu::DE } else { 0 };
        Ok(match self.ins.memory_size() {
            MemorySize::Float32 => {
                let v = f32::from_bits(self.mem.read32(a)?);
                self.cpu.fpu.sw |= ex(v.is_nan() && v.to_bits() & 0x40_0000 == 0, v.is_subnormal());
                fpu::quiet(v as f64)
            }
            MemorySize::Float64 => {
                let v = f64::from_bits(self.mem.read64(a)?);
                self.cpu.fpu.sw |= ex(v.is_nan() && v.to_bits() & (1 << 51) == 0, v.is_subnormal());
                fpu::quiet(v)
            }
            MemorySize::Float80 => {
                let mut b = [0u8; 10];
                for (i, x) in b.iter_mut().enumerate() {
                    *x = self.mem.read8(a.wrapping_add(i as u32))?;
                }
                fpu::from_f80(&b)
            }
            MemorySize::Int16 => self.mem.read16(a)? as i16 as f64,
            MemorySize::Int32 => self.mem.read32(a)? as i32 as f64,
            MemorySize::Int64 => self.mem.read64(a)? as i64 as f64,
            _ => 0.0,
        })
    }

    fn fstore(&mut self, v: f64) -> Result<(), crate::Fault> {
        let a = self.addr();
        // an integer FILD loaded (64 bits: more than the f64 register holds) rounds once, as
        // the control word says, from its exact value
        if let Some(i) = self.cpu.fpu.exact_int() {
            let rc = (self.cpu.fpu.cw >> 10) as u32;
            return match self.ins.memory_size() {
                MemorySize::Float32 => self.mem.write32(a, crate::round::i64_to_f32(i, rc).to_bits()),
                MemorySize::Float64 => self.mem.write64(a, crate::round::i64_to_f64(i, rc).to_bits()),
                _ => {
                    for (k, x) in fpu::int_to_f80(i).iter().enumerate() {
                        self.mem.write8(a.wrapping_add(k as u32), *x)?;
                    }
                    Ok(())
                }
            };
        }
        match self.ins.memory_size() {
            MemorySize::Float32 => self.mem.write32(a, crate::round::f64_to_f32(v, (self.cpu.fpu.cw >> 10) as u32).to_bits()),
            MemorySize::Float64 => self.mem.write64(a, v.to_bits()),
            _ => {
                let b = fpu::to_f80(v);
                for (i, x) in b.iter().enumerate() {
                    self.mem.write8(a.wrapping_add(i as u32), *x)?;
                }
                Ok(())
            }
        }
    }

    /// (destination ST index, source value) for two-operand arithmetic.
    fn farith_operands(&mut self) -> Result<(u32, f64, bool), crate::Fault> {
        let ins = self.ins;
        if ins.op_count() == 1 && ins.op_kind(0) == OpKind::Memory {
            return Ok((0, self.fload()?, false));
        }
        if ins.op_count() == 2 {
            let d = st_index(ins.op_register(0));
            let s = st_index(ins.op_register(1));
            return Ok((d, self.cpu.fpu.st(s), true));
        }
        Ok((1, self.cpu.fpu.st(0), true))
    }

    /// A memory operand as an 80-bit value (precise mode).
    fn fload80(&mut self) -> Result<F80, crate::Fault> {
        let a = self.addr();
        Ok(match self.ins.memory_size() {
            MemorySize::Float32 => {
                let v = f32::from_bits(self.mem.read32(a)?);
                F80::from_f64(fpu::quiet(v as f64))
            }
            MemorySize::Float64 => F80::from_f64(fpu::quiet(f64::from_bits(self.mem.read64(a)?))),
            MemorySize::Float80 => {
                let mut b = [0u8; 10];
                for (i, x) in b.iter_mut().enumerate() {
                    *x = self.mem.read8(a.wrapping_add(i as u32))?;
                }
                F80::from_bytes(&b)
            }
            MemorySize::Int16 => F80::from_i64(self.mem.read16(a)? as i16 as i64),
            MemorySize::Int32 => F80::from_i64(self.mem.read32(a)? as i32 as i64),
            _ => F80::from_i64(self.mem.read64(a)? as i64),
        })
    }

    /// The precise mode's x87 (crate::fpu::X87_80): the instructions whose results depend on
    /// the 80-bit format, in it; None for the rest (the f64 code runs them, and a register it
    /// writes drops its 80-bit value).
    fn x87_precise(&mut self) -> Option<Exit> {
        use Mnemonic::*;
        let ins = self.ins;
        let mn = ins.mnemonic();
        let cw = self.cpu.fpu.cw as u32;
        let (pc, rc) = ((cw >> 8) & 3, (cw >> 10) & 3);
        macro_rules! t {
            ($e:expr) => {
                match $e {
                    Ok(v) => v,
                    Err(f) => return Some(Exit::Fault(f)),
                }
            };
        }
        let mem0 = ins.op_count() >= 1 && ins.op_kind(0) == OpKind::Memory;
        match mn {
            Fld => {
                let v = if mem0 { t!(self.fload80()) } else { self.cpu.fpu.st80(st_index(ins.op_register(0))) };
                // (FLD of an SNaN from memory: quiet, as the x87 converts it; m80 and registers keep it)
                self.cpu.fpu.push80(v);
            }
            Fild => {
                let v = t!(self.fload80());
                self.cpu.fpu.push80(v);
            }
            Fst | Fstp => {
                let v = self.cpu.fpu.st80(0);
                if mem0 {
                    let a = self.addr();
                    match ins.memory_size() {
                        MemorySize::Float32 => t!(self.mem.write32(a, v.to_f32(rc).to_bits())),
                        MemorySize::Float64 => t!(self.mem.write64(a, v.to_f64(rc).to_bits())),
                        _ => {
                            for (i, x) in v.to_bytes().iter().enumerate() {
                                t!(self.mem.write8(a.wrapping_add(i as u32), *x));
                            }
                        }
                    }
                } else {
                    self.cpu.fpu.set_st80(st_index(ins.op_register(0)), v);
                }
                if mn == Fstp {
                    self.cpu.fpu.pop();
                }
            }
            Fist | Fistp | Fisttp => {
                let v = self.cpu.fpu.st80(0);
                let r = v.to_int(if mn == Fisttp { 3 } else { rc });
                let a = self.addr();
                match ins.memory_size() {
                    MemorySize::Int16 => {
                        let x = r.filter(|r| (-32768..=32767).contains(r)).map_or(i16::MIN, |r| r as i16);
                        t!(self.mem.write16(a, x as u16));
                    }
                    MemorySize::Int32 => {
                        let x = r.filter(|r| (i32::MIN as i128..=i32::MAX as i128).contains(r)).map_or(i32::MIN, |r| r as i32);
                        t!(self.mem.write32(a, x as u32));
                    }
                    _ => {
                        let x = r.filter(|r| (i64::MIN as i128..=i64::MAX as i128).contains(r)).map_or(i64::MIN, |r| r as i64);
                        t!(self.mem.write64(a, x as u64));
                    }
                }
                if mn != Fist {
                    self.cpu.fpu.pop();
                }
            }
            Fldpi | Fldl2e | Fldl2t | Fldlg2 | Fldln2 => {
                // the x87's constants: 64 bits of the exact value, rounded by RC (the nearest
                // and the one below it: down and toward zero take it when the nearest is above)
                let (se, m, near_up) = match mn {
                    Fldpi => (0x4000, 0xc90f_daa2_2168_c235u64, true),
                    Fldl2e => (0x3fff, 0xb8aa_3b29_5c17_f0bc, true),
                    Fldl2t => (0x4000, 0xd49a_784b_cd1b_8afe, false),
                    Fldlg2 => (0x3ffd, 0x9a20_9a84_fbcf_f799, true),
                    _ => (0x3ffe, 0xb172_17f7_d1cf_79ac, true),
                };
                let m = match (rc, near_up) {
                    (1 | 3, true) => m - 1,
                    (2, false) => m + 1,
                    _ => m,
                };
                self.cpu.fpu.push80(F80 { m, se });
            }
            Fadd | Faddp | Fsub | Fsubp | Fsubr | Fsubrp | Fmul | Fmulp | Fdiv | Fdivp | Fdivr | Fdivrp | Fiadd | Fisub | Fisubr | Fimul | Fidiv | Fidivr => {
                let (d, s) = if mem0 {
                    (0, t!(self.fload80()))
                } else if ins.op_count() == 2 {
                    (st_index(ins.op_register(0)), self.cpu.fpu.st80(st_index(ins.op_register(1))))
                } else {
                    (1, self.cpu.fpu.st80(0))
                };
                let x = self.cpu.fpu.st80(d);
                let r = match mn {
                    Fadd | Faddp | Fiadd => x.add(s, pc, rc),
                    Fsub | Fsubp | Fisub => x.sub(s, pc, rc),
                    Fsubr | Fsubrp | Fisubr => s.sub(x, pc, rc),
                    Fmul | Fmulp | Fimul => x.mul(s, pc, rc),
                    Fdiv | Fdivp | Fidiv => x.div(s, pc, rc),
                    _ => s.div(x, pc, rc),
                };
                self.cpu.fpu.set_st80(d, r);
                if matches!(mn, Faddp | Fsubp | Fsubrp | Fmulp | Fdivp | Fdivrp) {
                    self.cpu.fpu.pop();
                }
            }
            Fchs | Fabs => {
                let v = self.cpu.fpu.st80(0);
                self.cpu.fpu.set_st80(0, if mn == Fchs { v.neg() } else { v.abs() });
            }
            Fsqrt => {
                let v = self.cpu.fpu.st80(0);
                self.cpu.fpu.set_st80(0, v.sqrt(pc, rc));
            }
            Frndint => {
                let v = self.cpu.fpu.st80(0);
                let r = v.round_int(rc);
                let f = &mut self.cpu.fpu;
                f.sw &= !C1;
                if r != v && !v.is_nan() {
                    f.sw |= fpu::PE | if r.abs().compare(v.abs()) == Some(core::cmp::Ordering::Greater) { C1 } else { 0 };
                }
                f.set_st80(0, r);
            }
            Fcom | Fcomp | Fcompp | Fucom | Fucomp | Fucompp | Ficom | Ficomp | Ftst => {
                let s = if mn == Ftst {
                    F80::default()
                } else if ins.op_count() >= 1 && ins.op_kind(ins.op_count() - 1) == OpKind::Memory {
                    t!(self.fload80())
                } else if ins.op_count() == 2 {
                    self.cpu.fpu.st80(st_index(ins.op_register(1)))
                } else if ins.op_count() == 1 {
                    self.cpu.fpu.st80(st_index(ins.op_register(0)))
                } else {
                    self.cpu.fpu.st80(1)
                };
                let a = self.cpu.fpu.st80(0);
                use core::cmp::Ordering::*;
                let cc = match a.compare(s) {
                    None => C3 | C2 | C0,
                    Some(Greater) => 0,
                    Some(Less) => C0,
                    Some(Equal) => C3,
                };
                self.cpu.fpu.sw = (self.cpu.fpu.sw & !(C3 | C2 | C1 | C0)) | cc;
                match mn {
                    Fcomp | Fucomp | Ficomp => self.cpu.fpu.pop(),
                    Fcompp | Fucompp => {
                        self.cpu.fpu.pop();
                        self.cpu.fpu.pop();
                    }
                    _ => {}
                }
            }
            Fcomi | Fcomip | Fucomi | Fucomip => {
                let s = self.cpu.fpu.st80(st_index(ins.op_register(1)));
                let a = self.cpu.fpu.st80(0);
                use core::cmp::Ordering::*;
                let f = match a.compare(s) {
                    None => ZF | PF | CF,
                    Some(Greater) => 0,
                    Some(Less) => CF,
                    Some(Equal) => ZF,
                };
                self.cpu.eflags = (self.cpu.eflags & !STATUS) | f;
                if matches!(mn, Fcomip | Fucomip) {
                    self.cpu.fpu.pop();
                }
            }
            Fcmovb | Fcmove | Fcmovbe | Fcmovu | Fcmovnb | Fcmovne | Fcmovnbe | Fcmovnu => {
                let e = self.cpu.eflags;
                let c = match mn {
                    Fcmovb => e & CF != 0,
                    Fcmove => e & ZF != 0,
                    Fcmovbe => e & (CF | ZF) != 0,
                    Fcmovu => e & PF != 0,
                    Fcmovnb => e & CF == 0,
                    Fcmovne => e & ZF == 0,
                    Fcmovnbe => e & (CF | ZF) == 0,
                    _ => e & PF == 0,
                };
                if c {
                    let v = self.cpu.fpu.st80(st_index(ins.op_register(1)));
                    self.cpu.fpu.set_st80(0, v);
                }
            }
            Fnsave | Fsave => {
                let a = self.addr();
                let f = &self.cpu.fpu;
                let env = [f.cw as u32 | 0xffff_0000, f.status() as u32 | 0xffff_0000, f.tag_word() as u32 | 0xffff_0000, 0, 0, 0, 0];
                for (i, v) in env.iter().enumerate() {
                    t!(self.mem.write32(a + 4 * i as u32, *v));
                }
                for i in 0..8 {
                    for (k, x) in self.cpu.fpu.st80(i).to_bytes().iter().enumerate() {
                        t!(self.mem.write8(a + 28 + 10 * i + k as u32, *x));
                    }
                }
                self.cpu.fpu = crate::Fpu::default();
            }
            Frstor => {
                let a = self.addr();
                let cw = t!(self.mem.read32(a)) as u16;
                let sw = t!(self.mem.read32(a + 4)) as u16;
                let tw = t!(self.mem.read32(a + 8)) as u16;
                let mut regs = [F80::default(); 8];
                for (i, r) in regs.iter_mut().enumerate() {
                    let mut b = [0u8; 10];
                    for (k, x) in b.iter_mut().enumerate() {
                        *x = t!(self.mem.read8(a + 28 + 10 * i as u32 + k as u32));
                    }
                    *r = F80::from_bytes(&b);
                }
                let f = &mut self.cpu.fpu;
                f.cw = cw;
                f.set_status(sw);
                f.set_tag_word(tw);
                for (i, r) in regs.iter().enumerate() {
                    f.load80(i as u32, *r);
                }
            }
            _ => return None,
        }
        Some(self.done())
    }

    pub(super) fn x87(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let mn = ins.mnemonic();
        if fpu::x87_80() {
            if let Some(e) = self.x87_precise() {
                return e;
            }
        }
        match mn {
            Fld => {
                if ins.op_kind(0) == OpKind::Memory {
                    let v = tryf!(self.fload());
                    self.cpu.fpu.push(v);
                } else {
                    // FLD ST(i) copies the exact integer too
                    let i = st_index(ins.op_register(0));
                    let f = &self.cpu.fpu;
                    let p = ((f.top + i) & 7) as usize;
                    match (f.exact & (1 << p) != 0).then(|| f.ints[p]) {
                        Some(x) => self.cpu.fpu.push_int(x),
                        None => {
                            let v = self.cpu.fpu.st(i);
                            self.cpu.fpu.push(v);
                        }
                    }
                }
            }
            Fild => {
                let a = self.addr();
                let v = match ins.memory_size() {
                    MemorySize::Int16 => tryf!(self.mem.read16(a)) as i16 as i64,
                    MemorySize::Int32 => tryf!(self.mem.read32(a)) as i32 as i64,
                    _ => tryf!(self.mem.read64(a)) as i64,
                };
                self.cpu.fpu.push_int(v);
            }
            Fst | Fstp => {
                let v = self.cpu.fpu.st(0);
                if ins.op_kind(0) == OpKind::Memory {
                    tryf!(self.fstore(v));
                } else {
                    let i = st_index(ins.op_register(0));
                    self.cpu.fpu.set_st(i, v);
                }
                if mn == Fstp {
                    self.cpu.fpu.pop();
                }
            }
            Fist | Fistp | Fisttp => {
                let f = &self.cpu.fpu;
                let v = f.st(0);
                let a = self.addr();
                let exact = f.exact_int();
                let r = if mn == Fisttp { fpu::trunc(v) } else { f.round(v) };
                match ins.memory_size() {
                    MemorySize::Int16 => {
                        let x = if r >= -32768.0 && r <= 32767.0 { r as i16 } else { i16::MIN };
                        tryf!(self.mem.write16(a, x as u16));
                    }
                    MemorySize::Int32 => {
                        let x = if r >= -2147483648.0 && r <= 2147483647.0 { r as i32 } else { i32::MIN };
                        tryf!(self.mem.write32(a, x as u32));
                    }
                    _ => {
                        let x = match exact {
                            Some(e) => e,
                            None if r >= -9.223372036854775808e18 && r < 9.223372036854775808e18 => r as i64,
                            None => i64::MIN,
                        };
                        tryf!(self.mem.write64(a, x as u64));
                    }
                }
                if mn != Fist {
                    self.cpu.fpu.pop();
                }
            }
            // packed BCD (Delphi's FloatToText stores its digits with FBSTP): 9 bytes of two
            // digits each, low digits first, then the sign byte
            Fbstp => {
                let f = &self.cpu.fpu;
                let r = f.exact_int().map_or_else(|| f.round(f.st(0)), |e| e as f64);
                let a = self.addr();
                let mut b = [0u8; 10];
                if r.is_nan() || r.abs() >= 1e18 {
                    b[7] = 0xc0; // the BCD indefinite
                    b[8] = 0xff;
                    b[9] = 0xff;
                } else {
                    let mut n = r.abs() as u64;
                    for x in b.iter_mut().take(9) {
                        *x = (n % 10) as u8 | (((n / 10) % 10) as u8) << 4;
                        n /= 100;
                    }
                    if r.is_sign_negative() {
                        b[9] = 0x80;
                    }
                }
                for (i, x) in b.iter().enumerate() {
                    tryf!(self.mem.write8(a.wrapping_add(i as u32), *x));
                }
                self.cpu.fpu.pop();
            }
            Fbld => {
                let a = self.addr();
                let mut v: i64 = 0;
                for i in (0..9).rev() {
                    let x = tryf!(self.mem.read8(a.wrapping_add(i)));
                    v = v * 100 + ((x >> 4) as i64) * 10 + (x & 15) as i64;
                }
                if tryf!(self.mem.read8(a.wrapping_add(9))) & 0x80 != 0 {
                    v = -v;
                }
                self.cpu.fpu.push_int(v);
            }
            Fxch => {
                let i = if ins.op_count() == 0 {
                    1
                } else if ins.op_count() == 2 {
                    st_index(ins.op_register(1)).max(st_index(ins.op_register(0)))
                } else {
                    st_index(ins.op_register(0))
                };
                self.cpu.fpu.swap(i);
            }
            Fld1 => self.cpu.fpu.push(1.0),
            Fldz => self.cpu.fpu.push(0.0),
            Fldpi => self.cpu.fpu.push(core::f64::consts::PI),
            Fldl2e => self.cpu.fpu.push(core::f64::consts::LOG2_E),
            Fldl2t => self.cpu.fpu.push(core::f64::consts::LOG2_10),
            Fldlg2 => self.cpu.fpu.push(core::f64::consts::LOG10_2),
            Fldln2 => self.cpu.fpu.push(core::f64::consts::LN_2),

            Fadd | Faddp | Fsub | Fsubp | Fsubr | Fsubrp | Fmul | Fmulp | Fdiv | Fdivp | Fdivr | Fdivrp => {
                let (d, s, _) = tryf!(self.farith_operands());
                // two exact integers (FILD'ed): the exact result rounded once (an int64 is
                // more than 53 bits: rounding it first and the sum again would round twice)
                if ins.op_kind(0) != OpKind::Memory && !matches!(mn, Fdiv | Fdivp | Fdivr | Fdivrp) {
                    let si = if ins.op_count() == 2 { st_index(ins.op_register(1)) } else { 0 };
                    let f = &self.cpu.fpu;
                    if let (Some(a), Some(b)) = (f.exact_st(d), f.exact_st(si)) {
                        let (a, b) = (a as i128, b as i128);
                        let v = match mn {
                            Fadd | Faddp => a + b,
                            Fsub | Fsubp => a - b,
                            Fsubr | Fsubrp => b - a,
                            _ => a * b,
                        };
                        if self.cpu.fpu.set_exact_result(d, v).is_some() {
                            if matches!(mn, Faddp | Fsubp | Fsubrp | Fmulp) {
                                self.cpu.fpu.pop();
                            }
                            return self.done();
                        }
                    }
                }
                let x = self.cpu.fpu.st(d);
                let cw = self.cpu.fpu.cw;
                let r = match mn {
                    Fadd | Faddp => round::x87(Op::Add, x, s, cw),
                    Fsub | Fsubp => round::x87(Op::Sub, x, s, cw),
                    Fsubr | Fsubrp => round::x87(Op::Sub, s, x, cw),
                    Fmul | Fmulp => round::x87(Op::Mul, x, s, cw),
                    Fdiv | Fdivp => round::x87(Op::Div, x, s, cw),
                    _ => round::x87(Op::Div, s, x, cw),
                };
                self.cpu.fpu.set_st(d, r);
                if matches!(mn, Faddp | Fsubp | Fsubrp | Fmulp | Fdivp | Fdivrp) {
                    self.cpu.fpu.pop();
                }
            }
            Fiadd | Fisub | Fisubr | Fimul | Fidiv | Fidivr => {
                let s = tryf!(self.fload());
                let x = self.cpu.fpu.st(0);
                let cw = self.cpu.fpu.cw;
                let r = match mn {
                    Fiadd => round::x87(Op::Add, x, s, cw),
                    Fisub => round::x87(Op::Sub, x, s, cw),
                    Fisubr => round::x87(Op::Sub, s, x, cw),
                    Fimul => round::x87(Op::Mul, x, s, cw),
                    Fidiv => round::x87(Op::Div, x, s, cw),
                    _ => round::x87(Op::Div, s, x, cw),
                };
                self.cpu.fpu.set_st(0, r);
            }
            Fchs => {
                let e = self.cpu.fpu.exact_int();
                let v = self.cpu.fpu.st(0);
                self.cpu.fpu.set_st(0, -v);
                if let Some(i) = e {
                    self.cpu.fpu.push_int(i.wrapping_neg());
                    self.cpu.fpu.swap(1);
                    self.cpu.fpu.pop();
                }
            }
            Fabs => {
                let e = self.cpu.fpu.exact_int();
                let v = self.cpu.fpu.st(0);
                self.cpu.fpu.set_st(0, v.abs());
                if let Some(i) = e {
                    self.cpu.fpu.push_int(i.wrapping_abs());
                    self.cpu.fpu.swap(1);
                    self.cpu.fpu.pop();
                }
            }
            Fsqrt => { let v = self.cpu.fpu.st(0); let r = round::x87(Op::Sqrt, v, 0.0, self.cpu.fpu.cw); self.cpu.fpu.set_st(0, r); }
            Frndint => {
                if self.cpu.fpu.exact_int().is_none() {
                    let v = self.cpu.fpu.st(0);
                    // (an SNaN comes out quiet); a fraction rounded off is PE, C1 when it
                    // rounded away from zero
                    let r = fpu::quiet(self.cpu.fpu.round(v));
                    let f = &mut self.cpu.fpu;
                    f.sw &= !C1;
                    if v.is_finite() && r != v {
                        f.sw |= fpu::PE | if r.abs() > v.abs() { C1 } else { 0 };
                    }
                    f.set_st(0, r);
                }
            }
            Fsin => { let v = self.cpu.fpu.st(0); self.cpu.fpu.set_st(0, libm::sin(v)); self.cpu.fpu.sw &= !C2; }
            Fcos => { let v = self.cpu.fpu.st(0); self.cpu.fpu.set_st(0, libm::cos(v)); self.cpu.fpu.sw &= !C2; }
            Fsincos => {
                let v = self.cpu.fpu.st(0);
                self.cpu.fpu.set_st(0, libm::sin(v));
                self.cpu.fpu.push(libm::cos(v));
                self.cpu.fpu.sw &= !C2;
            }
            Fptan => {
                let v = self.cpu.fpu.st(0);
                self.cpu.fpu.set_st(0, libm::tan(v));
                self.cpu.fpu.push(1.0);
                self.cpu.fpu.sw &= !C2;
            }
            Fpatan => {
                let (x, y) = (self.cpu.fpu.st(0), self.cpu.fpu.st(1));
                self.cpu.fpu.set_st(1, libm::atan2(y, x));
                self.cpu.fpu.pop();
            }
            F2xm1 => { let v = self.cpu.fpu.st(0); self.cpu.fpu.set_st(0, libm::exp2(v) - 1.0); }
            Fyl2x => {
                let (x, y) = (self.cpu.fpu.st(0), self.cpu.fpu.st(1));
                self.cpu.fpu.set_st(1, y * libm::log2(x));
                self.cpu.fpu.pop();
            }
            Fyl2xp1 => {
                let (x, y) = (self.cpu.fpu.st(0), self.cpu.fpu.st(1));
                self.cpu.fpu.set_st(1, y * libm::log2(x + 1.0));
                self.cpu.fpu.pop();
            }
            Fscale => {
                let (x, s) = (self.cpu.fpu.st(0), self.cpu.fpu.st(1));
                self.cpu.fpu.set_st(0, libm::ldexp(x, fpu::trunc(s).clamp(-4096.0, 4096.0) as i32));
            }
            Fprem | Fprem1 => {
                let (x, y) = (self.cpu.fpu.st(0), self.cpu.fpu.st(1));
                // exact, as the FPU: fmod / IEEE remainder; the quotient's low 3 bits from the
                // remainder modulo 8y (always complete: C2 clear)
                let rt = libm::fmod(x, y);
                let r = if mn == Fprem { rt } else { libm::remainder(x, y) };
                self.cpu.fpu.set_st(0, if r == 0.0 { 0f64.copysign(x) } else { r });
                let y8 = y * 8.0;
                let mut qi = if y8.is_finite() && y.is_finite() && y != 0.0 {
                    libm::round(libm::fabs((libm::fmod(x, y8) - rt) / y)) as i64
                } else {
                    0
                };
                if r != rt {
                    qi += 1; // (rounded to the nearest instead of truncated)
                }
                let mut sw = self.cpu.fpu.sw & !(C0 | C1 | C2 | C3);
                if qi & 1 != 0 { sw |= C1; }
                if qi & 2 != 0 { sw |= C3; }
                if qi & 4 != 0 { sw |= C0; }
                self.cpu.fpu.sw = sw;
            }
            Fxtract => {
                let v = self.cpu.fpu.st(0);
                let (m, e) = libm::frexp(v);
                self.cpu.fpu.set_st(0, (e - 1) as f64);
                self.cpu.fpu.push(m * 2.0);
            }
            Fcom | Fcomp | Fcompp | Fucom | Fucomp | Fucompp | Ficom | Ficomp => {
                let s = if ins.op_count() >= 1 && ins.op_kind(ins.op_count() - 1) == OpKind::Memory {
                    tryf!(self.fload())
                } else if ins.op_count() == 2 {
                    self.cpu.fpu.st(st_index(ins.op_register(1)))
                } else if ins.op_count() == 1 {
                    self.cpu.fpu.st(st_index(ins.op_register(0)))
                } else {
                    self.cpu.fpu.st(1)
                };
                let a = self.cpu.fpu.st(0);
                self.cpu.fpu.compare_cc(a, s);
                match mn {
                    Fcomp | Fucomp | Ficomp => self.cpu.fpu.pop(),
                    Fcompp | Fucompp => { self.cpu.fpu.pop(); self.cpu.fpu.pop(); }
                    _ => {}
                }
            }
            Fcomi | Fcomip | Fucomi | Fucomip => {
                let s = self.cpu.fpu.st(st_index(ins.op_register(1)));
                let a = self.cpu.fpu.st(0);
                let f = fpu::compare_eflags(a, s);
                self.cpu.eflags = (self.cpu.eflags & !STATUS) | f;
                if matches!(mn, Fcomip | Fucomip) {
                    self.cpu.fpu.pop();
                }
            }
            Ftst => { let a = self.cpu.fpu.st(0); self.cpu.fpu.compare_cc(a, 0.0); }
            Fxam => {
                let f = &self.cpu.fpu;
                let p = ((f.top) & 7) as usize;
                let v = f.st(0);
                let mut cc = if f.empty & (1 << p) != 0 {
                    C3 | C0
                } else if v.is_nan() {
                    C0
                } else if v.is_infinite() {
                    C2 | C0
                } else if v == 0.0 {
                    C3
                } else if v.is_subnormal() {
                    C3 | C2
                } else {
                    C2
                };
                if v.is_sign_negative() { cc |= C1; }
                self.cpu.fpu.sw = (self.cpu.fpu.sw & !(C0 | C1 | C2 | C3)) | cc;
            }
            Fcmovb | Fcmove | Fcmovbe | Fcmovu | Fcmovnb | Fcmovne | Fcmovnbe | Fcmovnu => {
                let e = self.cpu.eflags;
                let c = match mn {
                    Fcmovb => e & CF != 0,
                    Fcmove => e & ZF != 0,
                    Fcmovbe => e & (CF | ZF) != 0,
                    Fcmovu => e & PF != 0,
                    Fcmovnb => e & CF == 0,
                    Fcmovne => e & ZF == 0,
                    Fcmovnbe => e & (CF | ZF) == 0,
                    _ => e & PF == 0,
                };
                if c {
                    let v = self.cpu.fpu.st(st_index(ins.op_register(1)));
                    self.cpu.fpu.set_st(0, v);
                }
            }
            Fldcw => {
                let a = self.addr();
                self.cpu.fpu.cw = tryf!(self.mem.read16(a));
            }
            Fnstcw | Fstcw => {
                let a = self.addr();
                tryf!(self.mem.write16(a, self.cpu.fpu.cw));
            }
            Fnstsw | Fstsw => {
                let v = self.cpu.fpu.status();
                if ins.op_kind(0) == OpKind::Memory {
                    let a = self.addr();
                    tryf!(self.mem.write16(a, v));
                } else {
                    self.cpu.regs[EAX] = (self.cpu.regs[EAX] & 0xffff_0000) | v as u32;
                }
            }
            Fnclex | Fclex => self.cpu.fpu.sw &= !0x80ff,
            Fninit | Finit => {
                let cw = 0x037f;
                self.cpu.fpu = crate::Fpu::default();
                self.cpu.fpu.cw = cw;
            }
            Wait | Fnop | Fndisi | Fneni | Fnsetpm => {}
            Ffree | Ffreep => {
                let i = st_index(ins.op_register(0));
                let p = ((self.cpu.fpu.top + i) & 7) as u8;
                self.cpu.fpu.empty |= 1 << p;
                if mn == Ffreep {
                    self.cpu.fpu.pop();
                }
            }
            Fincstp => self.cpu.fpu.top = (self.cpu.fpu.top + 1) & 7,
            Fdecstp => self.cpu.fpu.top = (self.cpu.fpu.top + 7) & 7,
            Fnstenv | Fstenv | Fnsave | Fsave => {
                let a = self.addr();
                let f = &self.cpu.fpu;
                let env = [f.cw as u32 | 0xffff_0000, f.status() as u32 | 0xffff_0000, f.tag_word() as u32 | 0xffff_0000, 0, 0, 0, 0];
                for (i, v) in env.iter().enumerate() {
                    tryf!(self.mem.write32(a + 4 * i as u32, *v));
                }
                if matches!(mn, Fnsave | Fsave) {
                    for i in 0..8 {
                        let b = fpu::to_f80(self.cpu.fpu.st(i));
                        for (k, x) in b.iter().enumerate() {
                            tryf!(self.mem.write8(a + 28 + 10 * i + k as u32, *x));
                        }
                    }
                    self.cpu.fpu = crate::Fpu::default();
                }
            }
            Fldenv | Frstor => {
                let a = self.addr();
                let cw = tryf!(self.mem.read32(a)) as u16;
                let sw = tryf!(self.mem.read32(a + 4)) as u16;
                let tw = tryf!(self.mem.read32(a + 8)) as u16;
                let mut regs = [0.0; 8];
                if mn == Frstor {
                    for (i, r) in regs.iter_mut().enumerate() {
                        let mut b = [0u8; 10];
                        for (k, x) in b.iter_mut().enumerate() {
                            *x = tryf!(self.mem.read8(a + 28 + 10 * i as u32 + k as u32));
                        }
                        *r = fpu::from_f80(&b);
                    }
                }
                let f = &mut self.cpu.fpu;
                f.cw = cw;
                f.set_status(sw);
                f.set_tag_word(tw);
                if mn == Frstor {
                    for (i, r) in regs.iter().enumerate() {
                        let p = ((f.top + i as u32) & 7) as usize;
                        f.regs[p] = *r;
                    }
                }
            }
            Fxsave | Fxrstor => {
                let a = self.addr();
                if mn == Fxsave {
                    crate::jit::mx_read(self.cpu.eip);
                    tryf!(crate::fxsave(self.cpu, self.mem, a));
                } else {
                    tryf!(crate::fxrstor(self.cpu, self.mem, a));
                }
            }
            _ => return Exit::InvalidOpcode,
        }
        let _ = flags::mask;
        self.done()
    }
}
