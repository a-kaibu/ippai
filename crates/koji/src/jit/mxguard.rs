//! The MXCSR exception flags in translated code. The hardware sets the flags of every SSE
//! floating-point instruction (sticky, masked or not); translated code computes the result
//! as wasm does and leaves the flags to the interpreter (interp::sse's mx_flags): before an
//! instruction that may raise one, a check of its operands sends it to the interpreter when
//! it might. What it checks, lane by lane:
//!
//! - an operand that is a NaN, an infinity or a denormal (invalid, denormal flags);
//! - a result that may overflow or underflow: the operands' magnitudes are checked to be in
//!   ranges whose results can't (a zero operand of a multiplication or a zero dividend
//!   gives an exact zero), a zero divisor, the square root of a negative number, a
//!   conversion out of the integer's range;
//! - the inexact flag (PE) not yet set, for an operation that may round: once a program has
//!   had one inexact result PE stays set, so the check costs a load and a test.
//!
//! Neither JIT sets a flag itself: every set flag comes from the interpreter.

use super::wasm::Code;
use iced_x86::{Instruction, Mnemonic, OpKind};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum G {
    Add,
    Mul,
    Div,
    Sqrt,
    /// MIN/MAX, compares, f32 -> f64: the operands' special values only
    Special,
    /// f64 -> f32
    Narrow,
    /// truncating or rounding to an int32
    ToInt,
    /// ... to an int64
    ToInt64,
    /// int -> float: may round (PE) only
    Inexact,
}

/// The check for `ins` (the legacy form): (what, lane bits, lanes checked, operand 0 read
/// too); None for an instruction that raises no flag.
pub(super) fn kind(ins: &Instruction) -> Option<(G, u32, u32, bool)> {
    use Mnemonic::*;
    let m = ins.mnemonic();
    let gpr64 = |i: u32| ins.op_kind(i) == OpKind::Register && ins.op_register(i).is_gpr64();
    Some(match m {
        Addps | Subps => (G::Add, 32, 4, true),
        Addpd | Subpd => (G::Add, 64, 2, true),
        Addss | Subss => (G::Add, 32, 1, true),
        Addsd | Subsd => (G::Add, 64, 1, true),
        Mulps => (G::Mul, 32, 4, true),
        Mulpd => (G::Mul, 64, 2, true),
        Mulss => (G::Mul, 32, 1, true),
        Mulsd => (G::Mul, 64, 1, true),
        Divps => (G::Div, 32, 4, true),
        Divpd => (G::Div, 64, 2, true),
        Divss => (G::Div, 32, 1, true),
        Divsd => (G::Div, 64, 1, true),
        Sqrtps => (G::Sqrt, 32, 4, false),
        Sqrtpd => (G::Sqrt, 64, 2, false),
        Sqrtss => (G::Sqrt, 32, 1, false),
        Sqrtsd => (G::Sqrt, 64, 1, false),
        Minps | Maxps | Cmpps => (G::Special, 32, 4, true),
        Minpd | Maxpd | Cmppd => (G::Special, 64, 2, true),
        Minss | Maxss | Cmpss | Comiss | Ucomiss => (G::Special, 32, 1, true),
        Minsd | Maxsd | Cmpsd | Comisd | Ucomisd => (G::Special, 64, 1, true),
        Cvtps2pd => (G::Special, 32, 2, false),
        Cvtss2sd => (G::Special, 32, 1, false),
        Cvtpd2ps => (G::Narrow, 64, 2, false),
        Cvtsd2ss => (G::Narrow, 64, 1, false),
        Cvttss2si | Cvtss2si => (if gpr64(0) { G::ToInt64 } else { G::ToInt }, 32, 1, false),
        Cvttsd2si | Cvtsd2si => (if gpr64(0) { G::ToInt64 } else { G::ToInt }, 64, 1, false),
        Cvtps2pi | Cvttps2pi => (G::ToInt, 32, 2, false),
        Cvtps2dq | Cvttps2dq => (G::ToInt, 32, 4, false),
        Cvtpd2dq | Cvttpd2dq | Cvtpd2pi | Cvttpd2pi => (G::ToInt, 64, 2, false),
        Cvtdq2ps | Cvtsi2ss => (G::Inexact, 32, 0, false),
        Cvtsi2sd if gpr64(1) || (ins.op_kind(1) == OpKind::Memory && ins.memory_size().size() == 8) => (G::Inexact, 64, 0, false),
        _ => return None,
    })
}

/// The magnitudes (the bits of |x|, or of x itself for a square root) translated code
/// handles in an operand: [lo, hi), and 0 when `zero`.
#[derive(Clone, Copy)]
struct R {
    lo: i64,
    hi: i64,
    zero: bool,
    abs: bool,
}

/// The ranges of the operand-0 and source lanes. A product or quotient of magnitudes in
/// [2^-63, 2^63) (f64: 2^-511, 2^511) is a normal number: no overflow, no underflow.
fn ranges(g: G, b64: bool) -> (R, R) {
    let (normal, inf, add_hi, mul_lo, mul_hi, int_hi) = if b64 {
        (1i64 << 52, 0x7ff0i64 << 48, 0x7fe0i64 << 48, 0x2000i64 << 48, 0x5fe0i64 << 48, 1053i64 << 52)
    } else {
        (0x0080_0000, 0x7f80_0000, 0x7f00_0000, 0x2000_0000, 0x5f00_0000, 0x4e80_0000)
    };
    let r = |lo, hi, zero| R { lo, hi, zero, abs: true };
    match g {
        G::Add => (r(normal, add_hi, true), r(normal, add_hi, true)),
        G::Mul => (r(mul_lo, mul_hi, true), r(mul_lo, mul_hi, true)),
        G::Div => (r(mul_lo, mul_hi, true), r(mul_lo, mul_hi, false)),
        // (the sign bit makes a negative x below lo; -0 goes to the interpreter too)
        G::Sqrt => (r(0, 0, true), R { lo: normal, hi: inf, zero: true, abs: false }),
        G::Special | G::Inexact => (r(normal, inf, true), r(normal, inf, true)),
        // f64 -> f32: the f32 normals but their top binade (which may round up to inf)
        G::Narrow => (r(0, 0, true), r(897 << 52, 1150 << 52, true)),
        // |x| < 2^30 (rounding just below 2^31 may overflow); int64: 2^62
        G::ToInt => (r(0, 0, true), r(0, int_hi, true)),
        G::ToInt64 => (r(0, 0, true), r(0, if b64 { 1085 << 52 } else { 0x5e80_0000 }, true)),
    }
}

fn vconst(c: &mut Code, b64: bool, v: i64) {
    c.simd(12);
    for _ in 0..(if b64 { 2 } else { 4 }) {
        if b64 {
            c.b.extend_from_slice(&v.to_le_bytes());
        } else {
            c.b.extend_from_slice(&(v as i32).to_le_bytes());
        }
    }
}

/// Pushes an i32, nonzero when the instruction must go to the interpreter: `a` (when the
/// check reads operand 0) and `b` are v128 locals with the operands (changed here), `t32`
/// and `t64` an i32 and an i64 temporary, `cpu` the Cpu local, `mxcsr` the offset of
/// Cpu::mxcsr.
pub(super) fn emit(c: &mut Code, (g, bits, lanes, _): (G, u32, u32, bool), a: Option<u32>, b: u32, t: (u32, u32), cpu: u32, mxcsr: u32) {
    let b64 = bits == 64;
    let (ra, rb) = ranges(g, b64);
    if g == G::Inexact {
        c.i32c(0);
    } else {
        exact(c, b64, ra, rb, lanes, bits, a, b, t);
    }
    if g != G::Special {
        // PE not set yet
        c.get(cpu).load32(mxcsr).i32c(0x20).and().eqz().or();
    }
}

/// The check: pushes an i32, nonzero when a lane is outside its range.
#[allow(clippy::too_many_arguments)]
fn exact(c: &mut Code, b64: bool, ra: R, rb: R, lanes: u32, bits: u32, a: Option<u32>, b: u32, t: (u32, u32)) {
    if lanes == 1 {
        // a scalar: on the low lane as an integer, x - lo >=u hi - lo
        let mut first = true;
        for (x, r) in [(Some(b), rb), (a, ra)] {
            let Some(x) = x else { continue };
            let l = if b64 { t.1 } else { t.0 };
            c.get(x);
            if b64 {
                c.lane(0x1d, 0); // i64x2.extract_lane
                if r.abs {
                    c.i64c(i64::MAX).op(0x83); // i64.and
                }
                if r.zero {
                    c.tee(l).get(l).op(0x50).i64_extend_u().i64c(62).op(0x86).op(0x84); // i64.eqz, shl, or
                }
                c.i64c(r.lo).op(0x7d).i64c(r.hi - r.lo).op(0x5a); // i64.sub, i64.ge_u
            } else {
                c.lane(0x1b, 0); // i32x4.extract_lane
                if r.abs {
                    c.i32c(i32::MAX).and();
                }
                if r.zero {
                    c.tee(l).get(l).eqz().i32c(30).shl().or();
                }
                c.i32c(r.lo as i32).sub().i32c((r.hi - r.lo) as i32).ge_u();
            }
            if !first {
                c.or();
            }
            first = false;
        }
    } else {
        // (float compares: lo <= |x| < hi; the bits of non-negative floats order as their
        // values, NaNs compare false. V8 has no native i64x2 ordered compare on x64: it
        // emulated them, 4-7x on SSE loops.)
        let (abs, ge, lt, feq, ieq) = if b64 { (0xec, 0x4c, 0x49, 0x47, 0xd6) } else { (0xe0, 0x46, 0x43, 0x41, 0x37) };
        let mut first = true;
        for (x, r) in [(Some(b), rb), (a, ra)] {
            let Some(x) = x else { continue };
            c.get(x);
            if r.abs {
                c.simd(abs);
            }
            c.set(x).get(x);
            vconst(c, b64, r.lo);
            c.simd(ge).get(x);
            vconst(c, b64, r.hi);
            c.simd(lt).simd(0x4e); // and
            if r.zero {
                // (|x| == 0; not abs: +0 only, as bits)
                c.get(x).v128zero().simd(if r.abs { feq } else { ieq }).simd(0x50);
            }
            c.simd(0x4d); // not: outside
            if !first {
                c.simd(0x50);
            }
            first = false;
        }
        // the lanes the instruction reads
        if lanes * bits < 128 {
            c.simd(12);
            for i in 0..16 {
                c.b.push(if i < (lanes * bits / 8) as usize { 0xff } else { 0 });
            }
            c.simd(0x4e);
        }
        c.simd(0x53); // v128.any_true
    }
}
