//! Lazily computed status flags. The JIT records what the last flag-setting operation was and
//! its operands; EFLAGS are worked out from them only when something reads them.

use crate::flags::{self, *};

/// `op` = kind * 4 + size (0: 8 bits, 1: 16, 2: 32, 3: 64); 0: EFLAGS are exact.
#[derive(Clone, Copy, Debug, Default)]
#[repr(C)]
pub struct Lazy {
    pub op: u32,
    /// carry in (ADC/SBB), CF kept through INC/DEC, or the overflow bit of MUL/IMUL
    pub c: u32,
    pub a: u64,
    pub b: u64,
    pub r: u64,
}

pub const ADD: u32 = 1;
pub const SUB: u32 = 2;
pub const LOGIC: u32 = 3;
pub const INC: u32 = 4;
pub const DEC: u32 = 5;
/// shifts: `a` the value, `b` the count (1..31)
pub const SHL: u32 = 6;
pub const SHR: u32 = 7;
pub const SAR: u32 = 8;
pub const ADC: u32 = 9;
pub const SBB: u32 = 10;
/// MUL/IMUL: CF = OF = `c`, SZP from `r`
pub const MUL: u32 = 11;

pub fn size_code(bits: u32) -> u32 {
    match bits {
        8 => 0,
        16 => 1,
        _ => 2,
    }
}

pub fn op(kind: u32, bits: u32) -> u32 {
    kind * 4 + size_code(bits)
}

impl Lazy {
    /// The status flags (CF PF AF ZF SF OF) this operation produced.
    pub fn status(&self) -> u32 {
        let bits = 8 << (self.op & 3);
        let (a64, b64, r64, c) = (self.a, self.b, self.r, self.c);
        // (shifts: up to 32 bits only, the long-mode JIT works out 64-bit ones itself)
        let (a, b) = (a64 as u32, b64 as u32);
        match self.op >> 2 {
            ADD => flags::add64(a64, b64, 0, bits).1,
            SUB => flags::sub64(a64, b64, 0, bits).1,
            ADC => flags::add64(a64, b64, c, bits).1,
            SBB => flags::sub64(a64, b64, c, bits).1,
            LOGIC => flags::szp64(r64, bits),
            INC => (flags::add64(a64, 1, 0, bits).1 & !CF) | (c & CF),
            DEC => (flags::sub64(a64, 1, 0, bits).1 & !CF) | (c & CF),
            SHL => {
                let m = flags::mask(bits);
                let wide = ((a & m) as u64) << b;
                let r = wide as u32 & m;
                let cf = if b <= bits { ((wide >> bits) & 1) as u32 } else { 0 };
                let of = ((r & flags::sign(bits) != 0) as u32) ^ cf;
                flags::szp(r, bits) | cf * CF | of * OF
            }
            SHR => {
                let a = a & flags::mask(bits);
                let r = if b >= 32 { 0 } else { a >> b };
                let cf = if b <= bits { (a >> (b - 1)) & 1 } else { 0 };
                let of = (a & flags::sign(bits) != 0) as u32;
                flags::szp(r, bits) | cf * CF | of * OF
            }
            SAR => {
                let sx = ((a << (32 - bits)) as i32) >> (32 - bits);
                let k = b.min(31);
                let r = (sx >> k) as u32 & flags::mask(bits);
                let cf = if b > 31 { (sx < 0) as u32 } else { ((sx >> (k - 1).min(31)) & 1) as u32 };
                flags::szp(r, bits) | cf * CF
            }
            MUL => flags::szp64(r64, bits) | if c != 0 { CF | OF } else { 0 },
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 64-bit helpers give the 32-bit ones' flags at 8, 16 and 32 bits.
    #[test]
    fn widths_agree() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..200_000 {
            let (a, b, c) = (next(), next(), (next() & 1) as u32);
            for bits in [8, 16, 32] {
                let (a32, b32) = (a as u32, b as u32);
                assert_eq!(flags::add(a32, b32, c, bits).1, flags::add64(a, b, c, bits).1, "add {a:x} {b:x} {c} {bits}");
                assert_eq!(flags::sub(a32, b32, c, bits).1, flags::sub64(a, b, c, bits).1, "sub {a:x} {b:x} {c} {bits}");
                assert_eq!(flags::logic(a32, bits), flags::szp64(a, bits), "logic {a:x} {bits}");
            }
        }
    }
}
