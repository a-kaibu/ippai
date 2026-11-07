//! BMI1 and BMI2 (VEX-encoded general register instructions; TZCNT/LZCNT are in mod.rs).

use super::X;
use crate::flags::{self, CF, SF, ZF};
use crate::{Exit, Memory};
use iced_x86::{Mnemonic, Register};

macro_rules! tryf {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(f) => return Exit::Fault(f),
        }
    };
}

pub(super) fn is_bmi(m: Mnemonic) -> bool {
    use Mnemonic::*;
    matches!(m, Andn | Bextr | Blsi | Blsmsk | Blsr | Bzhi | Mulx | Pdep | Pext | Rorx | Sarx | Shlx | Shrx)
}

/// PDEP: the low bits of `v` to the positions of `m`'s set bits.
pub(crate) fn pdep(v: u64, mut m: u64) -> u64 {
    let (mut r, mut k) = (0, 0);
    while m != 0 {
        let b = m & m.wrapping_neg();
        if v >> k & 1 != 0 {
            r |= b;
        }
        k += 1;
        m &= m - 1;
    }
    r
}

/// PEXT: the bits of `v` at `m`'s set bits, packed to the bottom.
pub(crate) fn pext(v: u64, mut m: u64) -> u64 {
    let (mut r, mut k) = (0, 0);
    while m != 0 {
        let b = m & m.wrapping_neg();
        if v & b != 0 {
            r |= 1 << k;
        }
        k += 1;
        m &= m - 1;
    }
    r
}

impl<'a, M: Memory> X<'a, M> {
    pub(super) fn bmi(&mut self) -> Exit {
        use Mnemonic::*;
        let ins = self.ins;
        let bits = self.op_bits(0);
        let m = flags::mask64(bits);
        // SF and ZF of a result (PF and AF are undefined after these)
        let sz = |r: u64| (if r == 0 { ZF } else { 0 }) | (if r & flags::sign64(bits) != 0 { SF } else { 0 });
        let mn = ins.mnemonic();
        if mn == Mulx {
            let src = tryf!(self.read_op(2));
            let d = self.get_reg(if bits == 64 { Register::RDX } else { Register::EDX });
            let p = d as u128 * src as u128;
            // the low half to the second destination, then the high half to the first (which
            // wins when both are the same register)
            tryf!(self.write_op(1, p as u64 & m));
            tryf!(self.write_op(0, (p >> bits) as u64 & m));
            return self.done();
        }
        let a = tryf!(self.read_op(1)) & m;
        let b = if ins.op_count() > 2 { tryf!(self.read_op(2)) & m } else { 0 };
        let (r, f) = match mn {
            Andn => {
                let r = !a & b & m;
                (r, Some(sz(r)))
            }
            Bextr => {
                let (start, len) = ((b & 0xff) as u32, ((b >> 8) & 0xff) as u32);
                let v = if start >= bits { 0 } else { a >> start };
                let r = if len >= 64 { v } else { v & ((1u64 << len) - 1) };
                (r & m, Some(if r & m == 0 { ZF } else { 0 }))
            }
            Blsi => {
                let r = a.wrapping_neg() & a & m;
                (r, Some(sz(r) | if a != 0 { CF } else { 0 }))
            }
            Blsmsk => {
                let r = (a.wrapping_sub(1) ^ a) & m;
                (r, Some(sz(r) | if a == 0 { CF } else { 0 }))
            }
            Blsr => {
                let r = a.wrapping_sub(1) & a & m;
                (r, Some(sz(r) | if a == 0 { CF } else { 0 }))
            }
            Bzhi => {
                let n = (b & 0xff) as u32;
                let (r, c) = if n < bits { (a & ((1u64 << n) - 1), 0) } else { (a, CF) };
                (r, Some(sz(r) | c))
            }
            Pdep => (pdep(a, b) & m, None),
            Pext => (pext(a, b) & m, None),
            Rorx => {
                let c = ins.immediate8() as u32 & (bits - 1);
                let r = if c == 0 { a } else { (a >> c | a << (bits - c)) & m };
                (r, None)
            }
            Sarx => {
                let c = b as u32 & (bits - 1);
                let sx = ((a << (64 - bits)) as i64) >> (64 - bits);
                ((sx >> c) as u64 & m, None)
            }
            Shlx => ((a << (b as u32 & (bits - 1))) & m, None),
            _ => (a >> (b as u32 & (bits - 1)), None),
        };
        tryf!(self.write_op(0, r));
        if let Some(f) = f {
            self.set_status(f);
        }
        self.done()
    }
}
