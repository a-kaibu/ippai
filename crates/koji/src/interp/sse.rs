//! MMX, SSE, SSE2, SSE3, SSSE3 and SSE4.1 (all but DPPS/DPPD, MPSADBW, PHMINPOSUW).

use super::X;
use crate::flags::STATUS;
use crate::round::{self, Op};
use crate::{fpu, Exit, Fault, Memory};
use iced_x86::{Mnemonic, OpKind, Register};

macro_rules! tryf {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(f) => return Exit::Fault(f),
        }
    };
}

/// Apply `f` to each `lane`-bit element of a and b.
#[inline]
fn lanes(a: u128, b: u128, lane: u32, f: impl Fn(u64, u64) -> u64) -> u128 {
    let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
    let mut r = 0u128;
    let mut s = 0;
    while s < 128 {
        let x = ((a >> s) & m) as u64;
        let y = ((b >> s) & m) as u64;
        r |= ((f(x, y) as u128) & m) << s;
        s += lane;
    }
    r
}

#[inline]
fn sx(v: u64, bits: u32) -> i64 {
    ((v << (64 - bits)) as i64) >> (64 - bits)
}

fn sat_s(v: i64, bits: u32) -> u64 {
    let lo = -(1i64 << (bits - 1));
    let hi = (1i64 << (bits - 1)) - 1;
    v.clamp(lo, hi) as u64
}

fn sat_u(v: i64, bits: u32) -> u64 {
    v.clamp(0, (1i64 << bits) - 1) as u64
}

#[inline]
fn f32x(v: u128, i: u32) -> f32 {
    f32::from_bits((v >> (32 * i)) as u32)
}
#[inline]
fn f64x(v: u128, i: u32) -> f64 {
    f64::from_bits((v >> (64 * i)) as u64)
}
#[inline]
fn set32(v: u128, i: u32, x: u32) -> u128 {
    (v & !((u32::MAX as u128) << (32 * i))) | (x as u128) << (32 * i)
}
#[inline]
fn set64(v: u128, i: u32, x: u64) -> u128 {
    (v & !((u64::MAX as u128) << (64 * i))) | (x as u128) << (64 * i)
}

/// Interleave the low (or high) halves of a and b in `lane`-bit units,
/// over a vector of `width` bits.
fn unpack(a: u128, b: u128, lane: u32, width: u32, high: bool) -> u128 {
    let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
    let n = width / lane / 2;
    let base = if high { n * lane } else { 0 };
    let mut r = 0u128;
    for i in 0..n {
        let x = (a >> (base + i * lane)) & m;
        let y = (b >> (base + i * lane)) & m;
        r |= x << (2 * i * lane) | y << ((2 * i + 1) * lane);
    }
    r
}

fn cmp_pred(a: f64, b: f64, p: u8) -> bool {
    let un = a.is_nan() || b.is_nan();
    match p & 7 {
        0 => a == b,
        1 => a < b,
        2 => a <= b,
        3 => un,
        4 => !(a == b),
        5 => !(a < b),
        6 => !(a <= b),
        _ => !un,
    }
}

fn sse_min(a: f64, b: f64) -> bool {
    a < b // true: take a, else b (NaN or equal -> second operand)
}

impl<'a, M: Memory> X<'a, M> {
    fn vbits(&self, i: u32) -> u32 {
        match self.ins.op_kind(i) {
            OpKind::Register => {
                let r = self.ins.op_register(i);
                if r.is_xmm() { 128 } else if r.is_mm() || r.is_gpr64() { 64 } else { 32 }
            }
            OpKind::Memory => self.ins.memory_size().size() as u32 * 8,
            _ => 8,
        }
    }

    pub(super) fn read_v(&mut self, i: u32) -> Result<u128, Fault> {
        match self.ins.op_kind(i) {
            OpKind::Register => {
                let r = self.ins.op_register(i);
                Ok(if r.is_xmm() {
                    self.cpu.xmm(r as usize - Register::XMM0 as usize)
                } else if r.is_mm() {
                    self.cpu.mm[r as usize - Register::MM0 as usize] as u128
                } else {
                    self.get_reg(r) as u128
                })
            }
            OpKind::Memory => {
                let a = self.addr();
                Ok(match self.ins.memory_size().size() {
                    1 => self.mem.read8(a)? as u128,
                    2 => self.mem.read16(a)? as u128,
                    4 => self.mem.read32(a)? as u128,
                    8 => self.mem.read64(a)? as u128,
                    _ => self.mem.read64(a)? as u128 | (self.mem.read64(a.wrapping_add(8))? as u128) << 64,
                })
            }
            _ => Ok(self.ins.immediate(i) as u128),
        }
    }

    pub(super) fn write_v(&mut self, i: u32, v: u128) -> Result<(), Fault> {
        match self.ins.op_kind(i) {
            OpKind::Register => {
                let r = self.ins.op_register(i);
                if r.is_xmm() {
                    self.cpu.set_xmm(r as usize - Register::XMM0 as usize, v);
                } else if r.is_mm() {
                    self.cpu.mm[r as usize - Register::MM0 as usize] = v as u64;
                } else {
                    self.set_reg(r, v as u64);
                }
                Ok(())
            }
            _ => {
                let a = self.addr();
                match self.ins.memory_size().size() {
                    1 => self.mem.write8(a, v as u8),
                    2 => self.mem.write16(a, v as u16),
                    4 => self.mem.write32(a, v as u32),
                    8 => self.mem.write64(a, v as u64),
                    _ => {
                        self.mem.write64(a, v as u64)?;
                        self.mem.write64(a.wrapping_add(8), (v >> 64) as u64)
                    }
                }
            }
        }
    }

    /// Merge the low `bits` of v into operand 0 (MOVSS/MOVSD/MOVLPS register forms).
    fn write_low(&mut self, v: u128, bits: u32) -> Result<(), Fault> {
        if self.ins.op_kind(0) == OpKind::Memory {
            return self.write_v(0, v);
        }
        let old = self.read_v(0)?;
        let m = (1u128 << bits) - 1;
        self.write_v(0, (old & !m) | (v & m))
    }

    fn round_mx(&self, v: f64) -> f64 {
        match (self.cpu.mxcsr >> 13) & 3 {
            0 => fpu::libm_round_even(v),
            1 => fpu::floor(v),
            2 => fpu::ceil(v),
            _ => fpu::trunc(v),
        }
    }

    fn to_i32(&self, v: f64, trunc: bool) -> u32 {
        let r = if trunc { fpu::trunc(v) } else { self.round_mx(v) };
        if r >= -2147483648.0 && r <= 2147483647.0 { r as i32 as u32 } else { 0x8000_0000 }
    }

    fn to_i64(&self, v: f64, trunc: bool) -> u64 {
        let r = if trunc { fpu::trunc(v) } else { self.round_mx(v) };
        if r >= -9223372036854775808.0 && r < 9223372036854775808.0 { r as i64 as u64 } else { 1 << 63 }
    }

    pub(super) fn sse(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let mn = ins.mnemonic();
        // The instruction's MXCSR flags are found first (sticky, as the hardware sets them:
        // the JIT leaves an instruction that may set one to here, see jit::mxguard); one
        // unmasked traps, leaving the destination alone.
        {
            if let Some((pre, post)) = self.mx_flags() {
                let mask = (self.cpu.mxcsr >> 7) & 0x3f;
                // an unmasked invalid, denormal or divide exception comes before the result
                let f = if pre & !mask != 0 { pre } else { pre | post };
                self.cpu.mxcsr |= f;
                if f & !mask != 0 {
                    return Exit::SimdException;
                }
            }
        }
        let w = if ins.op_count() > 0 { self.vbits(0) } else { 128 };
        let w = if w == 32 { 128 } else { w }; // gpr destination: lane math on xmm source
        let wmask: u128 = if w == 64 { u64::MAX as u128 } else { u128::MAX };
        // two-operand integer/logic ops: dst = f(dst, src)
        let binop = |x: &mut Self, f: &dyn Fn(u128, u128) -> u128| -> Exit {
            let a = tryf!(x.read_v(0));
            let b = tryf!(x.read_v(1));
            tryf!(x.write_v(0, f(a, b) & wmask));
            x.done()
        };
        match mn {
            Emms | Femms => {
                self.cpu.fpu.empty = 0xff;
                self.done()
            }
            Fxsave | Fxrstor => self.x87(),
            Ldmxcsr => {
                let a = self.addr();
                self.cpu.mxcsr = tryf!(self.mem.read32(a));
                self.done()
            }
            Stmxcsr => {
                let a = self.addr();
                tryf!(self.mem.write32(a, self.cpu.mxcsr));
                self.done()
            }
            Clflush | Clflushopt => self.done(),

            // ---- moves
            Movd => {
                let v = tryf!(self.read_v(1)) as u32 as u128;
                tryf!(self.write_v(0, v));
                self.done()
            }
            Movq | Movq2dq | Movdq2q => {
                let v = tryf!(self.read_v(1)) as u64 as u128;
                tryf!(self.write_v(0, v));
                self.done()
            }
            Movdqa | Movdqu | Movaps | Movups | Movapd | Movupd | Movntdq | Movntdqa | Movntps | Movntpd | Movntq | Lddqu
            | Movnti => {
                let v = tryf!(self.read_v(1));
                tryf!(self.write_v(0, v));
                self.done()
            }
            Movss | Movsd => {
                let bits = if mn == Movss { 32 } else { 64 };
                let v = tryf!(self.read_v(1));
                if ins.op_kind(1) == OpKind::Memory {
                    // load: upper part zeroed
                    tryf!(self.write_v(0, v & ((1u128 << bits) - 1)));
                } else {
                    tryf!(self.write_low(v, bits));
                }
                self.done()
            }
            Movlps | Movlpd => {
                let v = tryf!(self.read_v(1));
                tryf!(self.write_low(v, 64));
                self.done()
            }
            Movhps | Movhpd => {
                if ins.op_kind(0) == OpKind::Memory {
                    let v = tryf!(self.read_v(1)) >> 64;
                    tryf!(self.write_v(0, v));
                } else {
                    let a = tryf!(self.read_v(0));
                    let b = tryf!(self.read_v(1)) as u64;
                    tryf!(self.write_v(0, set64(a, 1, b)));
                }
                self.done()
            }
            Movlhps => binop(self, &|a, b| set64(a, 1, b as u64)),
            Movhlps => binop(self, &|a, b| set64(a, 0, (b >> 64) as u64)),

            // ---- logic
            Pand | Andps | Andpd => binop(self, &|a, b| a & b),
            Pandn | Andnps | Andnpd => binop(self, &|a, b| !a & b),
            Por | Orps | Orpd => binop(self, &|a, b| a | b),
            Pxor | Xorps | Xorpd => binop(self, &|a, b| a ^ b),

            // ---- integer arithmetic
            Paddb => binop(self, &|a, b| lanes(a, b, 8, |x, y| x.wrapping_add(y))),
            Paddw => binop(self, &|a, b| lanes(a, b, 16, |x, y| x.wrapping_add(y))),
            Paddd => binop(self, &|a, b| lanes(a, b, 32, |x, y| x.wrapping_add(y))),
            Paddq => binop(self, &|a, b| lanes(a, b, 64, |x, y| x.wrapping_add(y))),
            Psubb => binop(self, &|a, b| lanes(a, b, 8, |x, y| x.wrapping_sub(y))),
            Psubw => binop(self, &|a, b| lanes(a, b, 16, |x, y| x.wrapping_sub(y))),
            Psubd => binop(self, &|a, b| lanes(a, b, 32, |x, y| x.wrapping_sub(y))),
            Psubq => binop(self, &|a, b| lanes(a, b, 64, |x, y| x.wrapping_sub(y))),
            Paddsb => binop(self, &|a, b| lanes(a, b, 8, |x, y| sat_s(sx(x, 8) + sx(y, 8), 8))),
            Paddsw => binop(self, &|a, b| lanes(a, b, 16, |x, y| sat_s(sx(x, 16) + sx(y, 16), 16))),
            Psubsb => binop(self, &|a, b| lanes(a, b, 8, |x, y| sat_s(sx(x, 8) - sx(y, 8), 8))),
            Psubsw => binop(self, &|a, b| lanes(a, b, 16, |x, y| sat_s(sx(x, 16) - sx(y, 16), 16))),
            Paddusb => binop(self, &|a, b| lanes(a, b, 8, |x, y| sat_u(x as i64 + y as i64, 8))),
            Paddusw => binop(self, &|a, b| lanes(a, b, 16, |x, y| sat_u(x as i64 + y as i64, 16))),
            Psubusb => binop(self, &|a, b| lanes(a, b, 8, |x, y| sat_u(x as i64 - y as i64, 8))),
            Psubusw => binop(self, &|a, b| lanes(a, b, 16, |x, y| sat_u(x as i64 - y as i64, 16))),
            Pmullw => binop(self, &|a, b| lanes(a, b, 16, |x, y| x.wrapping_mul(y))),
            Pmulhw => binop(self, &|a, b| lanes(a, b, 16, |x, y| ((sx(x, 16) * sx(y, 16)) >> 16) as u64)),
            Pmulhuw => binop(self, &|a, b| lanes(a, b, 16, |x, y| (x * y) >> 16)),
            Pmuludq => binop(self, &|a, b| lanes(a, b, 64, |x, y| (x as u32 as u64) * (y as u32 as u64))),
            Pmaddwd => binop(self, &|a, b| {
                lanes(a, b, 32, |x, y| {
                    let p0 = sx(x & 0xffff, 16) * sx(y & 0xffff, 16);
                    let p1 = sx(x >> 16, 16) * sx(y >> 16, 16);
                    (p0 + p1) as u64
                })
            }),
            Pavgb => binop(self, &|a, b| lanes(a, b, 8, |x, y| (x + y + 1) >> 1)),
            Pavgw => binop(self, &|a, b| lanes(a, b, 16, |x, y| (x + y + 1) >> 1)),
            Pminub => binop(self, &|a, b| lanes(a, b, 8, |x, y| x.min(y))),
            Pmaxub => binop(self, &|a, b| lanes(a, b, 8, |x, y| x.max(y))),
            Pminsw => binop(self, &|a, b| lanes(a, b, 16, |x, y| if sx(x, 16) < sx(y, 16) { x } else { y })),
            Pmaxsw => binop(self, &|a, b| lanes(a, b, 16, |x, y| if sx(x, 16) > sx(y, 16) { x } else { y })),
            Maskmovq | Maskmovdqu => {
                // store the bytes of the first register whose mask byte (second register) has
                // its top bit set, at DS:EDI
                let n = ins.op_count();
                let v = tryf!(self.read_v(n - 2));
                let m = tryf!(self.read_v(n - 1));
                let a = self.get_reg(Register::EDI).wrapping_add(self.seg_base()) as u32;
                for k in 0..(if mn == Maskmovq { 8 } else { 16 }) {
                    if (m >> (8 * k + 7)) & 1 != 0 {
                        tryf!(self.mem.write8(a.wrapping_add(k), (v >> (8 * k)) as u8));
                    }
                }
                self.done()
            }
            Psadbw => binop(self, &|a, b| {
                lanes(a, b, 64, |x, y| {
                    let mut s = 0u64;
                    for k in 0..8 {
                        let p = (x >> (8 * k)) & 0xff;
                        let q = (y >> (8 * k)) & 0xff;
                        s += p.abs_diff(q);
                    }
                    s
                })
            }),
            Pcmpeqb => binop(self, &|a, b| lanes(a, b, 8, |x, y| if x == y { 0xff } else { 0 })),
            Pcmpeqw => binop(self, &|a, b| lanes(a, b, 16, |x, y| if x == y { 0xffff } else { 0 })),
            Pcmpeqd => binop(self, &|a, b| lanes(a, b, 32, |x, y| if x == y { 0xffff_ffff } else { 0 })),
            Pcmpgtb => binop(self, &|a, b| lanes(a, b, 8, |x, y| if sx(x, 8) > sx(y, 8) { 0xff } else { 0 })),
            Pcmpgtw => binop(self, &|a, b| lanes(a, b, 16, |x, y| if sx(x, 16) > sx(y, 16) { 0xffff } else { 0 })),
            Pcmpgtd => binop(self, &|a, b| lanes(a, b, 32, |x, y| if sx(x, 32) > sx(y, 32) { 0xffff_ffff } else { 0 })),

            // ---- shifts
            Psllw | Pslld | Psllq | Psrlw | Psrld | Psrlq | Psraw | Psrad => {
                let lane = match mn {
                    Psllw | Psrlw | Psraw => 16,
                    Pslld | Psrld | Psrad => 32,
                    _ => 64,
                };
                let a = tryf!(self.read_v(0));
                let c = match ins.op_kind(1) {
                    OpKind::Immediate8 => ins.immediate8() as u64,
                    _ => tryf!(self.read_v(1)) as u64,
                };
                let r = lanes(a, 0, lane, |x, _| match mn {
                    Psllw | Pslld | Psllq => if c >= lane as u64 { 0 } else { x << c },
                    Psrlw | Psrld | Psrlq => if c >= lane as u64 { 0 } else { x >> c },
                    _ => (sx(x, lane) >> c.min(lane as u64 - 1)) as u64,
                });
                tryf!(self.write_v(0, r & wmask));
                self.done()
            }
            Pslldq | Psrldq => {
                let a = tryf!(self.read_v(0));
                let n = (ins.immediate8() as u32).min(16) * 8;
                let r = if n >= 128 { 0 } else if mn == Pslldq { a << n } else { a >> n };
                tryf!(self.write_v(0, r));
                self.done()
            }

            // ---- pack / unpack / shuffle
            Punpcklbw => binop(self, &|a, b| unpack(a, b, 8, w, false)),
            Punpcklwd => binop(self, &|a, b| unpack(a, b, 16, w, false)),
            Punpckldq | Unpcklps => binop(self, &|a, b| unpack(a, b, 32, w, false)),
            Punpcklqdq | Unpcklpd => binop(self, &|a, b| unpack(a, b, 64, w, false)),
            Punpckhbw => binop(self, &|a, b| unpack(a, b, 8, w, true)),
            Punpckhwd => binop(self, &|a, b| unpack(a, b, 16, w, true)),
            Punpckhdq | Unpckhps => binop(self, &|a, b| unpack(a, b, 32, w, true)),
            Punpckhqdq | Unpckhpd => binop(self, &|a, b| unpack(a, b, 64, w, true)),
            Packsswb | Packssdw | Packuswb => {
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let (src, dst) = if mn == Packssdw { (32, 16) } else { (16, 8) };
                let n = w / src; // elements per operand
                let mut r = 0u128;
                for (k, v) in [a, b].iter().enumerate() {
                    for i in 0..n {
                        let e = sx(((v >> (i * src)) as u64) & ((1u64 << src) - 1), src);
                        let s = if mn == Packuswb { sat_u(e, dst) } else { sat_s(e, dst) };
                        r |= ((s as u128) & ((1u128 << dst) - 1)) << ((k as u32 * n + i) * dst);
                    }
                }
                tryf!(self.write_v(0, r & wmask));
                self.done()
            }
            Pshufd | Pshufw | Pshuflw | Pshufhw => {
                let v = tryf!(self.read_v(1));
                let imm = ins.immediate8() as u32;
                let r = match mn {
                    Pshufd => {
                        let mut r = 0u128;
                        for i in 0..4 {
                            r = set32(r, i, (v >> (32 * ((imm >> (2 * i)) & 3))) as u32);
                        }
                        r
                    }
                    _ => {
                        let base = if mn == Pshufhw { 64 } else { 0 };
                        let mut r = v;
                        for i in 0..4u32 {
                            let src = (v >> (base + 16 * ((imm >> (2 * i)) & 3))) as u16 as u128;
                            r = (r & !(0xffffu128 << (base + 16 * i))) | src << (base + 16 * i);
                        }
                        r
                    }
                };
                tryf!(self.write_v(0, r & wmask));
                self.done()
            }
            Shufps => {
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let imm = ins.immediate8() as u32;
                let pick = |v: u128, s: u32| (v >> (32 * (s & 3))) as u32;
                let mut r = 0u128;
                r = set32(r, 0, pick(a, imm));
                r = set32(r, 1, pick(a, imm >> 2));
                r = set32(r, 2, pick(b, imm >> 4));
                r = set32(r, 3, pick(b, imm >> 6));
                tryf!(self.write_v(0, r));
                self.done()
            }
            Shufpd => {
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let imm = ins.immediate8() as u32;
                let r = set64(set64(0, 0, (a >> (64 * (imm & 1))) as u64), 1, (b >> (64 * ((imm >> 1) & 1))) as u64);
                tryf!(self.write_v(0, r));
                self.done()
            }
            Pextrw => {
                let v = tryf!(self.read_v(1));
                let n = (ins.immediate8() as u32) & (if self.vbits(1) == 64 { 3 } else { 7 });
                let r = (v >> (16 * n)) as u16 as u128;
                tryf!(self.write_v(0, r));
                self.done()
            }
            Pinsrw => {
                let a = tryf!(self.read_v(0));
                let v = tryf!(self.read_v(1)) as u16 as u128;
                let n = (ins.immediate8() as u32) & (if w == 64 { 3 } else { 7 });
                let r = (a & !(0xffffu128 << (16 * n))) | v << (16 * n);
                tryf!(self.write_v(0, r));
                self.done()
            }
            Pmovmskb => {
                let v = tryf!(self.read_v(1));
                let n = self.vbits(1) / 8;
                let mut m = 0u32;
                for i in 0..n {
                    m |= (((v >> (8 * i + 7)) & 1) as u32) << i;
                }
                tryf!(self.write_v(0, m as u128));
                self.done()
            }
            Movmskps | Movmskpd => {
                let v = tryf!(self.read_v(1));
                let lane = if mn == Movmskps { 32 } else { 64 };
                let mut m = 0u32;
                for i in 0..128 / lane {
                    m |= (((v >> (lane * i + lane - 1)) & 1) as u32) << i;
                }
                tryf!(self.write_v(0, m as u128));
                self.done()
            }

            // ---- floating point
            Addss | Subss | Mulss | Divss | Minss | Maxss | Sqrtss | Addps | Subps | Mulps | Divps | Minps | Maxps
            | Sqrtps | Rcpps | Rcpss | Rsqrtps | Rsqrtss => {
                let scalar = matches!(mn, Addss | Subss | Mulss | Divss | Minss | Maxss | Sqrtss | Rcpss | Rsqrtss);
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let mut r = a;
                let mx = self.cpu.mxcsr;
                for i in 0..(if scalar { 1 } else { 4 }) {
                    let (x, y) = (f32x(a, i), f32x(b, i));
                    let v = match mn {
                        Addss | Addps => round::sse32(Op::Add, x, y, mx),
                        Subss | Subps => round::sse32(Op::Sub, x, y, mx),
                        Mulss | Mulps => round::sse32(Op::Mul, x, y, mx),
                        Divss | Divps => round::sse32(Op::Div, x, y, mx),
                        Minss | Minps => if sse_min(x as f64, y as f64) { x } else { y },
                        Maxss | Maxps => if (x as f64) > (y as f64) { x } else { y },
                        Sqrtss | Sqrtps => if y.is_nan() { f32::from_bits(y.to_bits() | 0x40_0000) } else if y < 0.0 { f32::from_bits(0xffc0_0000) } else { round::sse32(Op::Sqrt, y, 0.0, mx) },
                        // (the approximations: denormal operands as 0, tiny results 0, as x86)
                        Rcpss | Rcpps => {
                            let y = if y.is_subnormal() { 0.0f32.copysign(y) } else { y };
                            let r = 1.0 / y;
                            if r.is_subnormal() { 0.0f32.copysign(r) } else { r }
                        }
                        _ => 1.0 / libm::sqrtf(if y.is_subnormal() { 0.0f32.copysign(y) } else { y }),
                    };
                    r = set32(r, i, v.to_bits());
                }
                tryf!(self.write_v(0, r));
                self.done()
            }
            Addsd | Subsd | Mulsd | Divsd | Minsd | Maxsd | Sqrtsd | Addpd | Subpd | Mulpd | Divpd | Minpd | Maxpd
            | Sqrtpd => {
                let scalar = matches!(mn, Addsd | Subsd | Mulsd | Divsd | Minsd | Maxsd | Sqrtsd);
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let mut r = a;
                let mx = self.cpu.mxcsr;
                for i in 0..(if scalar { 1 } else { 2 }) {
                    let (x, y) = (f64x(a, i), f64x(b, i));
                    let v = match mn {
                        Addsd | Addpd => round::sse64(Op::Add, x, y, mx),
                        Subsd | Subpd => round::sse64(Op::Sub, x, y, mx),
                        Mulsd | Mulpd => round::sse64(Op::Mul, x, y, mx),
                        Divsd | Divpd => round::sse64(Op::Div, x, y, mx),
                        Minsd | Minpd => if sse_min(x, y) { x } else { y },
                        Maxsd | Maxpd => if x > y { x } else { y },
                        _ => if y.is_nan() { f64::from_bits(y.to_bits() | 0x8_0000_0000_0000) } else if y < 0.0 { f64::from_bits(0xfff8_0000_0000_0000) } else { round::sse64(Op::Sqrt, y, 0.0, mx) },
                    };
                    r = set64(r, i, v.to_bits());
                }
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cmpss | Cmpps | Cmpsd | Cmppd => {
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let p = ins.immediate8();
                let mut r = a;
                match mn {
                    Cmpss | Cmpps => {
                        for i in 0..(if mn == Cmpss { 1 } else { 4 }) {
                            let t = cmp_pred(f32x(a, i) as f64, f32x(b, i) as f64, p);
                            r = set32(r, i, if t { u32::MAX } else { 0 });
                        }
                    }
                    _ => {
                        for i in 0..(if mn == Cmpsd { 1 } else { 2 }) {
                            let t = cmp_pred(f64x(a, i), f64x(b, i), p);
                            r = set64(r, i, if t { u64::MAX } else { 0 });
                        }
                    }
                }
                tryf!(self.write_v(0, r));
                self.done()
            }
            Comiss | Ucomiss | Comisd | Ucomisd => {
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let (x, y) = if matches!(mn, Comiss | Ucomiss) {
                    (f32x(a, 0) as f64, f32x(b, 0) as f64)
                } else {
                    (f64x(a, 0), f64x(b, 0))
                };
                self.cpu.eflags = (self.cpu.eflags & !STATUS) | fpu::compare_eflags(x, y);
                self.done()
            }

            // ---- conversions
            Cvtsi2ss | Cvtsi2sd => {
                let raw = tryf!(self.read_v(1));
                let a = tryf!(self.read_v(0));
                // a 64-bit source (REX.W) converts with one rounding, like the hardware
                let v = if self.vbits(1) == 64 { raw as u64 as i64 } else { raw as u32 as i32 as i64 };
                let rc = self.cpu.mxcsr >> 13;
                let (f, d) = (round::i64_to_f32(v, rc), round::i64_to_f64(v, rc));
                let r = if mn == Cvtsi2ss { set32(a, 0, f.to_bits()) } else { set64(a, 0, d.to_bits()) };
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvttss2si | Cvtss2si | Cvttsd2si | Cvtsd2si => {
                let v = tryf!(self.read_v(1));
                let x = if matches!(mn, Cvttss2si | Cvtss2si) { f32x(v, 0) as f64 } else { f64x(v, 0) };
                let t = matches!(mn, Cvttss2si | Cvttsd2si);
                let r = if self.vbits(0) == 64 { self.to_i64(x, t) as u128 } else { self.to_i32(x, t) as u128 };
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtss2sd => {
                let a = tryf!(self.read_v(0));
                let v = tryf!(self.read_v(1));
                tryf!(self.write_v(0, set64(a, 0, (f32x(v, 0) as f64).to_bits())));
                self.done()
            }
            Cvtsd2ss => {
                let a = tryf!(self.read_v(0));
                let v = tryf!(self.read_v(1));
                tryf!(self.write_v(0, set32(a, 0, round::f64_to_f32(f64x(v, 0), self.cpu.mxcsr >> 13).to_bits())));
                self.done()
            }
            Cvtdq2ps => {
                let v = tryf!(self.read_v(1));
                let rc = self.cpu.mxcsr >> 13;
                let r = lanes(v, 0, 32, |x, _| round::i64_to_f32(x as u32 as i32 as i64, rc).to_bits() as u64);
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtps2dq | Cvttps2dq => {
                let v = tryf!(self.read_v(1));
                let mut r = 0u128;
                for i in 0..4 {
                    r = set32(r, i, self.to_i32(f32x(v, i) as f64, mn == Cvttps2dq));
                }
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtdq2pd | Cvtpi2pd => {
                let v = tryf!(self.read_v(1));
                let r = set64(set64(0, 0, ((v as u32 as i32) as f64).to_bits()), 1, (((v >> 32) as u32 as i32) as f64).to_bits());
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtpd2dq | Cvttpd2dq | Cvtpd2pi | Cvttpd2pi => {
                let v = tryf!(self.read_v(1));
                let t = matches!(mn, Cvttpd2dq | Cvttpd2pi);
                let r = set32(set32(0, 0, self.to_i32(f64x(v, 0), t)), 1, self.to_i32(f64x(v, 1), t));
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtps2pd => {
                let v = tryf!(self.read_v(1));
                let r = set64(set64(0, 0, (f32x(v, 0) as f64).to_bits()), 1, (f32x(v, 1) as f64).to_bits());
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtpd2ps => {
                let v = tryf!(self.read_v(1));
                let rc = self.cpu.mxcsr >> 13;
                let r = set32(set32(0, 0, round::f64_to_f32(f64x(v, 0), rc).to_bits()), 1, round::f64_to_f32(f64x(v, 1), rc).to_bits());
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtpi2ps => {
                let a = tryf!(self.read_v(0));
                let v = tryf!(self.read_v(1));
                let rc = self.cpu.mxcsr >> 13;
                let r = set32(set32(a, 0, round::i64_to_f32(v as u32 as i32 as i64, rc).to_bits()), 1, round::i64_to_f32((v >> 32) as u32 as i32 as i64, rc).to_bits());
                tryf!(self.write_v(0, r));
                self.done()
            }
            Cvtps2pi | Cvttps2pi => {
                let v = tryf!(self.read_v(1));
                let t = mn == Cvttps2pi;
                let r = set32(set32(0, 0, self.to_i32(f32x(v, 0) as f64, t)), 1, self.to_i32(f32x(v, 1) as f64, t));
                tryf!(self.write_v(0, r));
                self.done()
            }
            // ---- SSE3
            Movshdup | Movsldup => {
                let v = tryf!(self.read_v(1));
                let o = if mn == Movshdup { 1 } else { 0 };
                let r = (0..4).fold(0, |r, i| set32(r, i, (v >> (32 * ((i & !1) + o))) as u32));
                tryf!(self.write_v(0, r));
                self.done()
            }
            Movddup => {
                let v = tryf!(self.read_v(1)) as u64;
                tryf!(self.write_v(0, set64(v as u128, 1, v)));
                self.done()
            }
            Haddps | Hsubps => {
                let sub = mn == Hsubps;
                binop(self, &|a, b| {
                    let h = |v: u128, i: u32| {
                        let (x, y) = (f32x(v, 2 * i), f32x(v, 2 * i + 1));
                        (if sub { x - y } else { x + y }).to_bits()
                    };
                    set32(set32(set32(set32(0, 0, h(a, 0)), 1, h(a, 1)), 2, h(b, 0)), 3, h(b, 1))
                })
            }
            Haddpd | Hsubpd => {
                let sub = mn == Hsubpd;
                binop(self, &|a, b| {
                    let h = |v: u128| {
                        let (x, y) = (f64x(v, 0), f64x(v, 1));
                        (if sub { x - y } else { x + y }).to_bits()
                    };
                    set64(set64(0, 0, h(a)), 1, h(b))
                })
            }
            Addsubps => binop(self, &|a, b| {
                (0..4).fold(0, |r, i| {
                    let (x, y) = (f32x(a, i), f32x(b, i));
                    set32(r, i, (if i % 2 == 0 { x - y } else { x + y }).to_bits())
                })
            }),
            Addsubpd => binop(self, &|a, b| set64(set64(0, 0, (f64x(a, 0) - f64x(b, 0)).to_bits()), 1, (f64x(a, 1) + f64x(b, 1)).to_bits())),
            // (SSE4.2)
            Pcmpgtq => binop(self, &|a, b| lanes(a, b, 64, |x, y| if (x as i64) > (y as i64) { u64::MAX } else { 0 })),

            // ---- SSSE3
            Pshufb => binop(self, &|a, b| {
                let n = w / 8;
                let mut r = 0u128;
                for i in 0..n {
                    let c = (b >> (8 * i)) as u8;
                    if c & 0x80 == 0 {
                        r |= ((a >> (8 * (c as u32 % n))) & 0xff) << (8 * i);
                    }
                }
                r
            }),
            Pabsb => binop(self, &|_, b| lanes(b, 0, 8, |x, _| sx(x, 8).unsigned_abs())),
            Pabsw => binop(self, &|_, b| lanes(b, 0, 16, |x, _| sx(x, 16).unsigned_abs())),
            Pabsd => binop(self, &|_, b| lanes(b, 0, 32, |x, _| sx(x, 32).unsigned_abs())),
            Psignb | Psignw | Psignd => {
                let lane = match mn { Psignb => 8, Psignw => 16, _ => 32 };
                binop(self, &|a, b| {
                    lanes(a, b, lane, |x, y| match sx(y, lane) {
                        0 => 0,
                        v if v < 0 => (sx(x, lane).wrapping_neg()) as u64,
                        _ => x,
                    })
                })
            }
            Pmulhrsw => binop(self, &|a, b| lanes(a, b, 16, |x, y| (((sx(x, 16) * sx(y, 16) >> 14) + 1) >> 1) as u64)),
            Pmaddubsw => binop(self, &|a, b| {
                lanes(a, b, 16, |x, y| {
                    let p0 = (x & 0xff) as i64 * sx(y & 0xff, 8);
                    let p1 = (x >> 8 & 0xff) as i64 * sx(y >> 8 & 0xff, 8);
                    sat_s(p0 + p1, 16)
                })
            }),
            Phaddw | Phaddd | Phsubw | Phsubd | Phaddsw | Phsubsw => {
                let lane = if matches!(mn, Phaddd | Phsubd) { 32 } else { 16 };
                let sub = matches!(mn, Phsubw | Phsubd | Phsubsw);
                let sat = matches!(mn, Phaddsw | Phsubsw);
                binop(self, &|a, b| {
                    let n = w / lane / 2; // pairs per source
                    let m = (1u128 << lane) - 1;
                    let mut r = 0u128;
                    for (k, v) in [a, b].into_iter().enumerate() {
                        for i in 0..n {
                            let x = (v >> (2 * i * lane)) & m;
                            let y = (v >> ((2 * i + 1) * lane)) & m;
                            let s = if sat {
                                let (p, q) = (sx(x as u64, 16), sx(y as u64, 16));
                                sat_s(if sub { p - q } else { p + q }, 16) as u128
                            } else if sub {
                                x.wrapping_sub(y)
                            } else {
                                x.wrapping_add(y)
                            } & m;
                            r |= s << ((k as u32 * n + i) * lane);
                        }
                    }
                    r
                })
            }
            Palignr => {
                let n = ins.immediate8() as u32;
                binop(self, &|a, b| {
                    // (a:b) >> 8n, over 2w bits
                    let mut r = 0u128;
                    for i in 0..w / 8 {
                        let k = i + n;
                        let byte = if k < w / 8 { (b >> (8 * k)) & 0xff } else if k < 2 * w / 8 { (a >> (8 * (k - w / 8))) & 0xff } else { 0 };
                        r |= byte << (8 * i);
                    }
                    r
                })
            }

            // ---- SSE4.1
            Pmulld => binop(self, &|a, b| lanes(a, b, 32, |x, y| x.wrapping_mul(y))),
            Pmuldq => binop(self, &|a, b| lanes(a, b, 64, |x, y| (sx(x & 0xffff_ffff, 32) * sx(y & 0xffff_ffff, 32)) as u64)),
            Pcmpeqq => binop(self, &|a, b| lanes(a, b, 64, |x, y| if x == y { u64::MAX } else { 0 })),
            Pminsb => binop(self, &|a, b| lanes(a, b, 8, |x, y| if sx(x, 8) < sx(y, 8) { x } else { y })),
            Pmaxsb => binop(self, &|a, b| lanes(a, b, 8, |x, y| if sx(x, 8) > sx(y, 8) { x } else { y })),
            Pminsd => binop(self, &|a, b| lanes(a, b, 32, |x, y| if sx(x, 32) < sx(y, 32) { x } else { y })),
            Pmaxsd => binop(self, &|a, b| lanes(a, b, 32, |x, y| if sx(x, 32) > sx(y, 32) { x } else { y })),
            Pminuw => binop(self, &|a, b| lanes(a, b, 16, |x, y| x.min(y))),
            Pmaxuw => binop(self, &|a, b| lanes(a, b, 16, |x, y| x.max(y))),
            Pminud => binop(self, &|a, b| lanes(a, b, 32, |x, y| x.min(y))),
            Pmaxud => binop(self, &|a, b| lanes(a, b, 32, |x, y| x.max(y))),
            Packusdw => binop(self, &|a, b| {
                let mut r = 0u128;
                for (k, v) in [a, b].into_iter().enumerate() {
                    for i in 0..4u32 {
                        let x = sat_u(sx((v >> (32 * i)) as u64 & 0xffff_ffff, 32), 16) as u128;
                        r |= x << (16 * (4 * k as u32 + i));
                    }
                }
                r
            }),
            Pmovzxbw | Pmovzxbd | Pmovzxbq | Pmovzxwd | Pmovzxwq | Pmovzxdq | Pmovsxbw | Pmovsxbd | Pmovsxbq
            | Pmovsxwd | Pmovsxwq | Pmovsxdq => {
                let (from, to) = match mn {
                    Pmovzxbw | Pmovsxbw => (8, 16),
                    Pmovzxbd | Pmovsxbd => (8, 32),
                    Pmovzxbq | Pmovsxbq => (8, 64),
                    Pmovzxwd | Pmovsxwd => (16, 32),
                    Pmovzxwq | Pmovsxwq => (16, 64),
                    _ => (32, 64),
                };
                let signed = matches!(mn, Pmovsxbw | Pmovsxbd | Pmovsxbq | Pmovsxwd | Pmovsxwq | Pmovsxdq);
                let v = tryf!(self.read_v(1));
                let mut r = 0u128;
                for i in 0..128 / to {
                    let x = (v >> (from * i)) as u64 & ((1u64 << from) - 1);
                    let x = if signed { sx(x, from) as u64 } else { x };
                    let m = if to == 64 { u64::MAX } else { (1u64 << to) - 1 };
                    r |= ((x & m) as u128) << (to * i);
                }
                tryf!(self.write_v(0, r));
                self.done()
            }
            Pinsrb | Pinsrd | Pinsrq => {
                let lane = match mn { Pinsrb => 8, Pinsrd => 32, _ => 64 };
                let a = tryf!(self.read_v(0));
                let v = tryf!(self.read_v(1));
                let n = (ins.immediate8() as u32) & (128 / lane - 1);
                let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
                tryf!(self.write_v(0, (a & !(m << (lane * n))) | (v & m) << (lane * n)));
                self.done()
            }
            Pextrb | Pextrd | Pextrq | Extractps => {
                let lane = match mn { Pextrb => 8, Pextrq => 64, _ => 32 };
                let v = tryf!(self.read_v(1));
                let n = (ins.immediate8() as u32) & (128 / lane - 1);
                let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
                tryf!(self.write_v(0, (v >> (lane * n)) & m));
                self.done()
            }
            Insertps => {
                let a = tryf!(self.read_v(0));
                let imm = ins.immediate8() as u32;
                let src = if ins.op_kind(1) == OpKind::Memory { tryf!(self.read_v(1)) as u32 } else { (tryf!(self.read_v(1)) >> (32 * (imm >> 6))) as u32 };
                let mut r = set32(a, (imm >> 4) & 3, src);
                for i in 0..4 {
                    if imm & (1 << i) != 0 {
                        r = set32(r, i, 0);
                    }
                }
                tryf!(self.write_v(0, r));
                self.done()
            }
            Pblendw | Blendps | Blendpd => {
                let lane = match mn { Pblendw => 16, Blendps => 32, _ => 64 };
                let imm = ins.immediate8() as u32;
                binop(self, &|a, b| {
                    let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
                    let mut r = a;
                    for i in 0..128 / lane {
                        if imm & (1 << i) != 0 {
                            r = (r & !(m << (lane * i))) | (b & (m << (lane * i)));
                        }
                    }
                    r
                })
            }
            // (legacy forms: the mask is XMM0)
            Pblendvb | Blendvps | Blendvpd => {
                let lane = match mn { Pblendvb => 8, Blendvps => 32, _ => 64 };
                let mask = self.cpu.xmm(0);
                binop(self, &|a, b| {
                    let m: u128 = if lane == 64 { u64::MAX as u128 } else { (1u128 << lane) - 1 };
                    let mut r = a;
                    for i in 0..128 / lane {
                        if (mask >> (lane * i + lane - 1)) & 1 != 0 {
                            r = (r & !(m << (lane * i))) | (b & (m << (lane * i)));
                        }
                    }
                    r
                })
            }
            // SSE4.1: dot products (each product rounded, then the sums in pairs)
            Dpps => {
                let imm = ins.immediate8() as u32;
                binop(self, &|a, b| {
                    let p = |i: u32| if imm & (16 << i) != 0 { f32x(a, i) * f32x(b, i) } else { 0.0 };
                    let sum = (p(0) + p(1)) + (p(2) + p(3));
                    (0..4).fold(0, |r, i| if imm & (1 << i) != 0 { set32(r, i, sum.to_bits()) } else { r })
                })
            }
            Dppd => {
                let imm = ins.immediate8() as u32;
                binop(self, &|a, b| {
                    let p = |i: u32| if imm & (16 << i) != 0 { f64x(a, i) * f64x(b, i) } else { 0.0 };
                    let sum = p(0) + p(1);
                    (0..2).fold(0, |r, i| if imm & (1 << i) != 0 { set64(r, i, sum.to_bits()) } else { r })
                })
            }
            // SSE4.1: sums of absolute differences of 4 bytes, 8 windows of the destination
            Mpsadbw => {
                let imm = ins.immediate8() as u32;
                binop(self, &|a, b| {
                    let byte = |v: u128, i: u32| (v >> (8 * i)) as u8 as i32;
                    let (so, d) = (4 * (imm & 3), 4 * ((imm >> 2) & 1));
                    (0..8u32).fold(0u128, |r, i| {
                        let sad: i32 = (0..4).map(|j| (byte(a, d + i + j) - byte(b, so + j)).abs()).sum();
                        r | (sad as u128) << (16 * i)
                    })
                })
            }
            // SSE4.1: the smallest word and its index
            Phminposuw => {
                let v = tryf!(self.read_v(1));
                let (mut m, mut at) = (u16::MAX as u128 + 1, 0);
                for i in 0..8 {
                    let w = (v >> (16 * i)) & 0xffff;
                    if w < m {
                        (m, at) = (w, i);
                    }
                }
                tryf!(self.write_v(0, m | (at as u128) << 16));
                self.done()
            }
            // SSE4.2 string compares
            Pcmpestri | Pcmpestrm | Pcmpistri | Pcmpistrm | Pcmpestri64 | Pcmpestrm64 => {
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let imm = ins.immediate8() as u32;
                let explicit = matches!(mn, Pcmpestri | Pcmpestrm | Pcmpestri64 | Pcmpestrm64);
                let lens = explicit.then(|| (self.cpu.regs[0], self.cpu.regs[2]));
                let (res, la, lb, n) = pcmpstr(a, b, imm, lens);
                let mut f = self.cpu.eflags & !STATUS;
                if res != 0 {
                    f |= crate::flags::CF;
                }
                if lb < n {
                    f |= crate::flags::ZF;
                }
                if la < n {
                    f |= crate::flags::SF;
                }
                if res & 1 != 0 {
                    f |= crate::flags::OF;
                }
                self.cpu.eflags = f;
                if matches!(mn, Pcmpestri | Pcmpistri | Pcmpestri64) {
                    let i = if res == 0 { n } else if imm & 0x40 != 0 { 31 - res.leading_zeros() } else { res.trailing_zeros() };
                    self.set_reg(Register::ECX, i as u64);
                } else {
                    let m = if imm & 0x40 == 0 {
                        res as u128
                    } else {
                        let w = if imm & 1 != 0 { 16 } else { 8 };
                        (0..n).filter(|i| res >> i & 1 != 0).fold(0u128, |r, i| r | (((1u128 << w) - 1) << (w * i)))
                    };
                    self.cpu.set_xmm(0, m);
                }
                self.done()
            }
            Ptest => {
                let a = tryf!(self.read_v(0));
                let b = tryf!(self.read_v(1));
                let mut f = self.cpu.eflags & !STATUS;
                if a & b == 0 {
                    f |= crate::flags::ZF;
                }
                if !a & b == 0 {
                    f |= crate::flags::CF;
                }
                self.cpu.eflags = f;
                self.done()
            }
            Roundss | Roundsd | Roundps | Roundpd => {
                let imm = ins.immediate8() as u32;
                let mode = if imm & 4 != 0 { (self.cpu.mxcsr >> 13) & 3 } else { imm & 3 };
                let rnd = |v: f64| match mode {
                    0 => fpu::libm_round_even(v),
                    1 => fpu::floor(v),
                    2 => fpu::ceil(v),
                    _ => fpu::trunc(v),
                };
                let a = tryf!(self.read_v(0));
                let v = tryf!(self.read_v(1));
                let r = match mn {
                    Roundss => set32(a, 0, (rnd(f32x(v, 0) as f64) as f32).to_bits()),
                    Roundsd => set64(a, 0, rnd(f64x(v, 0)).to_bits()),
                    Roundps => (0..4).fold(0, |r, i| set32(r, i, (rnd(f32x(v, i) as f64) as f32).to_bits())),
                    _ => (0..2).fold(0, |r, i| set64(r, i, rnd(f64x(v, i)).to_bits())),
                };
                tryf!(self.write_v(0, r));
                self.done()
            }
            _ => Exit::InvalidOpcode,
        }
    }
}

// ---- MXCSR exception flags
const IE: u32 = 1;
const DE: u32 = 2;
const ZE: u32 = 4;
const OE: u32 = 8;
const UE: u32 = 0x10;
const PE: u32 = 0x20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FOp {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
    MinMax,
}

fn snan32(x: f32) -> bool {
    x.is_nan() && x.to_bits() & 0x40_0000 == 0
}
fn snan64(x: f64) -> bool {
    x.is_nan() && x.to_bits() & (1 << 51) == 0
}

/// s + e == a + b exactly (Knuth's TwoSum).
fn two_sum(a: f64, b: f64) -> (f64, f64) {
    let s = a + b;
    let bb = s - a;
    (s, (a - (s - bb)) + (b - bb))
}

/// a * b == c exactly, for finite values (on the integer significands, so that it holds
/// where an FMA residual would underflow).
fn product_is(a: f64, b: f64, c: f64) -> bool {
    if a == 0.0 || b == 0.0 || c == 0.0 {
        return (a == 0.0 || b == 0.0) && c == 0.0;
    }
    if (a.is_sign_negative() != b.is_sign_negative()) != c.is_sign_negative() {
        return false;
    }
    let parts = |v: f64| {
        let bits = v.to_bits();
        let ex = ((bits >> 52) & 0x7ff) as i32;
        let fr = (bits & ((1 << 52) - 1)) as u128;
        if ex == 0 { (fr, -1074) } else { (fr | 1 << 52, ex - 1075) }
    };
    let norm = |(m, e): (u128, i32)| (m >> m.trailing_zeros(), e + m.trailing_zeros() as i32);
    let ((ma, ea), (mb, eb)) = (parts(a), parts(b));
    norm((ma * mb, ea + eb)) == norm(parts(c))
}

/// The flags before the result (invalid, denormal, divide by zero: the operands decide them)
/// and after it (overflow, underflow, inexact), in the order of precedence the SDM gives:
/// an SNaN operand, a QNaN operand (no flag), another invalid operation or a division by
/// zero, a denormal operand, then the result. `x` and `y` are widened exactly from f32 when
/// `single`; `sn`/`den` say whether an operand was a signaling NaN / denormal in its own
/// format. For Sqrt the operand is `y`.
fn arith_flags(op: FOp, x: f64, y: f64, single: bool, sn: bool, den: bool, ue_unmasked: bool) -> (u32, u32) {
    let x = if op == FOp::Sqrt { y } else { x };
    if x.is_nan() || y.is_nan() {
        return (if sn || op == FOp::MinMax { IE } else { 0 }, 0);
    }
    let invalid = match op {
        FOp::Add => x.is_infinite() && y.is_infinite() && x.is_sign_negative() != y.is_sign_negative(),
        FOp::Sub => x.is_infinite() && y.is_infinite() && x.is_sign_negative() == y.is_sign_negative(),
        FOp::Mul => (x == 0.0 && y.is_infinite()) || (y == 0.0 && x.is_infinite()),
        FOp::Div => (x == 0.0 && y == 0.0) || (x.is_infinite() && y.is_infinite()),
        FOp::Sqrt => y < 0.0,
        FOp::MinMax => false,
    };
    if invalid {
        return (IE, 0);
    }
    if op == FOp::Div && y == 0.0 && x.is_finite() {
        return (ZE, 0);
    }
    let pre = if den { DE } else { 0 };
    if op == FOp::MinMax || x.is_infinite() || y.is_infinite() {
        return (pre, 0);
    }
    // the rounded result (to nearest, as the arithmetic itself) and whether it is exact
    let (inf, sub, zero, inexact) = if single {
        let (a, b) = (x as f32, y as f32);
        let r = match op {
            FOp::Add => a + b,
            FOp::Sub => a - b,
            FOp::Mul => a * b,
            FOp::Div => a / b,
            _ => libm::sqrtf(b),
        };
        let rd = r as f64;
        let inexact = r.is_infinite()
            || match op {
                FOp::Add | FOp::Sub => {
                    let (s, e) = two_sum(x, if op == FOp::Sub { -y } else { y });
                    rd != s || e != 0.0
                }
                FOp::Mul => rd != x * y, // (24 x 24 bits: exact in f64)
                FOp::Div => rd * y != x,
                _ => rd * rd != y,
            };
        (r.is_infinite(), r.is_subnormal(), r == 0.0, inexact)
    } else {
        let r = match op {
            FOp::Add => x + y,
            FOp::Sub => x - y,
            FOp::Mul => x * y,
            FOp::Div => x / y,
            _ => libm::sqrt(y),
        };
        let inexact = r.is_infinite()
            || match op {
                FOp::Add | FOp::Sub => two_sum(x, if op == FOp::Sub { -y } else { y }).1 != 0.0,
                FOp::Mul => !product_is(x, y, r),
                FOp::Div => !product_is(r, y, x),
                _ => !product_is(r, r, y),
            };
        (r.is_infinite(), r.is_subnormal(), r == 0.0, inexact)
    };
    let mut post = 0;
    if inf {
        post = OE | PE;
    } else {
        let tiny = sub || (zero && inexact);
        if tiny && (inexact || ue_unmasked) {
            post |= UE;
        }
        if inexact {
            post |= PE;
        }
    }
    (pre, post)
}

/// arith_flags for f32 operands.
pub fn flags32(op: FOp, x: f32, y: f32, ue_unmasked: bool) -> (u32, u32) {
    let ops: &[f32] = if op == FOp::Sqrt { &[y] } else { &[x, y] };
    let sn = ops.iter().any(|&v| snan32(v));
    let den = ops.iter().any(|v| v.is_subnormal());
    arith_flags(op, x as f64, y as f64, true, sn, den, ue_unmasked)
}

/// arith_flags for f64 operands.
pub fn flags64(op: FOp, x: f64, y: f64, ue_unmasked: bool) -> (u32, u32) {
    let ops: &[f64] = if op == FOp::Sqrt { &[y] } else { &[x, y] };
    let sn = ops.iter().any(|&v| snan64(v));
    let den = ops.iter().any(|v| v.is_subnormal());
    arith_flags(op, x, y, false, sn, den, ue_unmasked)
}

/// f64 -> f32 (CVTSD2SS, CVTPD2PS).
pub fn narrow_flags(x: f64, ue_unmasked: bool) -> (u32, u32) {
    if x.is_nan() {
        return (if snan64(x) { IE } else { 0 }, 0);
    }
    let pre = if x.is_subnormal() { DE } else { 0 };
    if x.is_infinite() {
        return (pre, 0);
    }
    let r = x as f32;
    let inexact = r as f64 != x;
    let post = if r.is_infinite() {
        OE | PE
    } else {
        let tiny = r.is_subnormal() || (r == 0.0 && x != 0.0);
        (if tiny && (inexact || ue_unmasked) { UE } else { 0 }) | if inexact { PE } else { 0 }
    };
    (pre, post)
}

/// A comparison: an SNaN operand is invalid, a QNaN too when `signaling` (COMISS, the
/// ordered less/less-equal predicates of CMPPS); else a denormal operand.
fn cmp_flags(nan: bool, sn: bool, den: bool, signaling: bool) -> u32 {
    if nan {
        if sn || signaling { IE } else { 0 }
    } else if den {
        DE
    } else {
        0
    }
}

fn or2(a: (u32, u32), b: (u32, u32)) -> (u32, u32) {
    (a.0 | b.0, a.1 | b.1)
}

impl<'a, M: Memory> X<'a, M> {
    /// The MXCSR flags the current SSE instruction raises (before, after the result); None
    /// for an instruction that raises none, or an operand that faults (the instruction then
    /// faults when it runs).
    fn mx_flags(&mut self) -> Option<(u32, u32)> {
        use Mnemonic::*;
        let mn = self.ins.mnemonic();
        let ue = self.cpu.mxcsr & 0x800 == 0;
        let mut f = (0u32, 0u32);
        let op = |mn: Mnemonic| match mn {
            Addss | Addps | Addsd | Addpd => FOp::Add,
            Subss | Subps | Subsd | Subpd => FOp::Sub,
            Mulss | Mulps | Mulsd | Mulpd => FOp::Mul,
            Divss | Divps | Divsd | Divpd => FOp::Div,
            Sqrtss | Sqrtps | Sqrtsd | Sqrtpd => FOp::Sqrt,
            _ => FOp::MinMax,
        };
        // a conversion to an integer: NaN or out of range is invalid, else rounding inexact
        let to_int = |x: &Self, v: f64, trunc: bool, bits: u32| -> (u32, u32) {
            if v.is_nan() {
                return (IE, 0);
            }
            let r = if trunc { fpu::trunc(v) } else { x.round_mx(v) };
            let lim = if bits == 64 { 9223372036854775808.0 } else { 2147483648.0 };
            if !(r >= -lim && r < lim) {
                (IE, 0)
            } else if r != v {
                (0, PE)
            } else {
                (0, 0)
            }
        };
        match mn {
            Addss | Subss | Mulss | Divss | Minss | Maxss | Sqrtss | Addps | Subps | Mulps | Divps | Minps | Maxps
            | Sqrtps => {
                let n = if matches!(mn, Addss | Subss | Mulss | Divss | Minss | Maxss | Sqrtss) { 1 } else { 4 };
                let a = self.read_v(0).ok()?;
                let b = self.read_v(1).ok()?;
                for i in 0..n {
                    f = or2(f, flags32(op(mn), f32x(a, i), f32x(b, i), ue));
                }
            }
            Addsd | Subsd | Mulsd | Divsd | Minsd | Maxsd | Sqrtsd | Addpd | Subpd | Mulpd | Divpd | Minpd | Maxpd
            | Sqrtpd => {
                let n = if matches!(mn, Addsd | Subsd | Mulsd | Divsd | Minsd | Maxsd | Sqrtsd) { 1 } else { 2 };
                let a = self.read_v(0).ok()?;
                let b = self.read_v(1).ok()?;
                for i in 0..n {
                    f = or2(f, flags64(op(mn), f64x(a, i), f64x(b, i), ue));
                }
            }
            Haddps | Hsubps | Addsubps => {
                let a = self.read_v(0).ok()?;
                let b = self.read_v(1).ok()?;
                for i in 0..4 {
                    let (x, y, o) = match mn {
                        Addsubps => (f32x(a, i), f32x(b, i), if i % 2 == 0 { FOp::Sub } else { FOp::Add }),
                        _ => {
                            let v = if i < 2 { a } else { b };
                            let j = 2 * (i % 2);
                            (f32x(v, j), f32x(v, j + 1), if mn == Hsubps { FOp::Sub } else { FOp::Add })
                        }
                    };
                    f = or2(f, flags32(o, x, y, ue));
                }
            }
            Haddpd | Hsubpd | Addsubpd => {
                let a = self.read_v(0).ok()?;
                let b = self.read_v(1).ok()?;
                for i in 0..2 {
                    let (x, y, o) = match mn {
                        Addsubpd => (f64x(a, i), f64x(b, i), if i == 0 { FOp::Sub } else { FOp::Add }),
                        _ => {
                            let v = if i == 0 { a } else { b };
                            (f64x(v, 0), f64x(v, 1), if mn == Hsubpd { FOp::Sub } else { FOp::Add })
                        }
                    };
                    f = or2(f, flags64(o, x, y, ue));
                }
            }
            Cmpss | Cmpps | Cmpsd | Cmppd | Comiss | Ucomiss | Comisd | Ucomisd => {
                let a = self.read_v(0).ok()?;
                let b = self.read_v(1).ok()?;
                let signaling = match mn {
                    Comiss | Comisd => true,
                    Ucomiss | Ucomisd => false,
                    _ => matches!(self.ins.immediate8() & 7, 1 | 2 | 5 | 6),
                };
                let (n, single) = match mn {
                    Cmpss | Comiss | Ucomiss => (1, true),
                    Cmpps => (4, true),
                    Cmppd => (2, false),
                    _ => (1, false),
                };
                for i in 0..n {
                    f.0 |= if single {
                        let (x, y) = (f32x(a, i), f32x(b, i));
                        cmp_flags(x.is_nan() || y.is_nan(), snan32(x) || snan32(y), x.is_subnormal() || y.is_subnormal(), signaling)
                    } else {
                        let (x, y) = (f64x(a, i), f64x(b, i));
                        cmp_flags(x.is_nan() || y.is_nan(), snan64(x) || snan64(y), x.is_subnormal() || y.is_subnormal(), signaling)
                    };
                }
            }
            Cvttss2si | Cvtss2si | Cvttsd2si | Cvtsd2si => {
                let v = self.read_v(1).ok()?;
                let x = if matches!(mn, Cvttss2si | Cvtss2si) { f32x(v, 0) as f64 } else { f64x(v, 0) };
                let bits = if self.vbits(0) == 64 { 64 } else { 32 };
                f = to_int(self, x, matches!(mn, Cvttss2si | Cvttsd2si), bits);
            }
            Cvtps2dq | Cvttps2dq | Cvtps2pi | Cvttps2pi => {
                let v = self.read_v(1).ok()?;
                let n = if matches!(mn, Cvtps2dq | Cvttps2dq) { 4 } else { 2 };
                for i in 0..n {
                    f = or2(f, to_int(self, f32x(v, i) as f64, matches!(mn, Cvttps2dq | Cvttps2pi), 32));
                }
            }
            Cvtpd2dq | Cvttpd2dq | Cvtpd2pi | Cvttpd2pi => {
                let v = self.read_v(1).ok()?;
                for i in 0..2 {
                    f = or2(f, to_int(self, f64x(v, i), matches!(mn, Cvttpd2dq | Cvttpd2pi), 32));
                }
            }
            Cvtsi2ss | Cvtsi2sd | Cvtdq2ps | Cvtpi2ps => {
                let raw = self.read_v(1).ok()?;
                let vals: [i64; 4] = match mn {
                    Cvtdq2ps => core::array::from_fn(|i| (raw >> (32 * i)) as u32 as i32 as i64),
                    Cvtpi2ps => core::array::from_fn(|i| if i < 2 { (raw >> (32 * i)) as u32 as i32 as i64 } else { 0 }),
                    _ if self.vbits(1) == 64 => [raw as u64 as i64, 0, 0, 0],
                    _ => [raw as u32 as i32 as i64, 0, 0, 0],
                };
                for v in vals {
                    let exact = if mn == Cvtsi2sd { (v as f64) as i128 == v as i128 } else { (v as f32) as i128 == v as i128 };
                    if !exact {
                        f.1 |= PE;
                    }
                }
            }
            Cvtss2sd | Cvtps2pd => {
                let v = self.read_v(1).ok()?;
                for i in 0..(if mn == Cvtss2sd { 1 } else { 2 }) {
                    let x = f32x(v, i);
                    f.0 |= if snan32(x) { IE } else if x.is_subnormal() { DE } else { 0 };
                }
            }
            Cvtsd2ss | Cvtpd2ps => {
                let v = self.read_v(1).ok()?;
                for i in 0..(if mn == Cvtsd2ss { 1 } else { 2 }) {
                    f = or2(f, narrow_flags(f64x(v, i), ue));
                }
            }
            Roundss | Roundsd | Roundps | Roundpd => {
                let imm = self.ins.immediate8() as u32;
                let mode = if imm & 4 != 0 { (self.cpu.mxcsr >> 13) & 3 } else { imm & 3 };
                let v = self.read_v(1).ok()?;
                let single = matches!(mn, Roundss | Roundps);
                let n = match mn {
                    Roundss | Roundsd => 1,
                    Roundps => 4,
                    _ => 2,
                };
                for i in 0..n {
                    let (x, sn) = if single { (f32x(v, i) as f64, snan32(f32x(v, i))) } else { (f64x(v, i), snan64(f64x(v, i))) };
                    if x.is_nan() {
                        f.0 |= if sn { IE } else { 0 };
                        continue;
                    }
                    let r = match mode {
                        0 => fpu::libm_round_even(x),
                        1 => fpu::floor(x),
                        2 => fpu::ceil(x),
                        _ => fpu::trunc(x),
                    };
                    if imm & 8 == 0 && r != x {
                        f.1 |= PE;
                    }
                }
            }
            _ => return None,
        }
        Some(f)
    }
}

/// PCMPxSTRx: the result bits (IntRes2), the lengths of `a` and `b` and the element count.
/// `lens`: the explicit lengths (EAX, EDX), else up to the first zero element.
fn pcmpstr(a: u128, b: u128, imm: u32, lens: Option<(u32, u32)>) -> (u32, u32, u32, u32) {
    let words = imm & 1 != 0;
    let signed = imm & 2 != 0;
    let n: u32 = if words { 8 } else { 16 };
    let el = |v: u128, i: u32| -> i32 {
        let x = if words { (v >> (16 * i)) as u16 as u32 } else { (v >> (8 * i)) as u8 as u32 };
        match (signed, words) {
            (true, true) => x as u16 as i16 as i32,
            (true, false) => x as u8 as i8 as i32,
            _ => x as i32,
        }
    };
    let len = |v: u128, r: Option<u32>| match r {
        Some(r) => (r as i32).unsigned_abs().min(n),
        None => (0..n).find(|&i| el(v, i) == 0).unwrap_or(n),
    };
    let (la, lb) = (len(a, lens.map(|l| l.0)), len(b, lens.map(|l| l.1)));
    let agg = (imm >> 2) & 3;
    // a[j] against b[i], with the forced results for elements past the lengths
    let cmp = |j: u32, i: u32| -> bool {
        match (j < la, i < lb) {
            (true, true) => match agg {
                1 if j % 2 == 0 => el(a, j) <= el(b, i),
                1 => el(a, j) >= el(b, i),
                _ => el(a, j) == el(b, i),
            },
            (false, false) => agg >= 2,
            (false, true) => agg == 3,
            (true, false) => false,
        }
    };
    let mut r1 = 0u32;
    for i in 0..n {
        let bit = match agg {
            0 => (0..n).any(|j| cmp(j, i)),
            1 => (0..n / 2).any(|k| cmp(2 * k, i) && cmp(2 * k + 1, i)),
            2 => cmp(i, i),
            _ => (0..n - i).all(|k| cmp(k, i + k)),
        };
        r1 |= (bit as u32) << i;
    }
    let all = (1u32 << n) - 1;
    let r2 = match (imm >> 4) & 3 {
        1 => !r1 & all,
        3 => r1 ^ ((1u32 << lb) - 1),
        _ => r1,
    };
    (r2, la, lb, n)
}
