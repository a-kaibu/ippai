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
//!
//! Only once the program has read the flags (jit::MX_EXACT: its first STMXCSR or FXSAVE runs
//! in the interpreter, which turns it on; the host then drops every translation): before
//! that, translated code has no checks (SSE float loops run 2-3x faster) and what it raised is
//! not in MXCSR.

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
    if !super::mx_exact() {
        return None; // (nobody has read the flags yet: see jit::MX_EXACT)
    }
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
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct R {
    lo: i64,
    hi: i64,
    zero: bool,
    abs: bool,
}

/// The ranges of the operand-0 and source lanes. A product or quotient of magnitudes in
/// [2^-63, 2^63) (f64: 2^-511, 2^511) is a normal number: no overflow, no underflow. A sum or
/// difference of magnitudes from 2^-102 (f64: 2^-969) up is 0 or normal (a multiple of their
/// ulp, 2^-125 at least): no denormal result for MXCSR.FTZ to flush.
fn ranges(g: G, b64: bool) -> (R, R) {
    let (normal, inf, add_lo, add_hi, mul_lo, mul_hi, int_hi) = if b64 {
        (1i64 << 52, 0x7ff0i64 << 48, 54i64 << 52, 0x7fe0i64 << 48, 0x2000i64 << 48, 0x5fe0i64 << 48, 1053i64 << 52)
    } else {
        (0x0080_0000, 0x7f80_0000, 25 << 23, 0x7f00_0000, 0x2000_0000, 0x5f00_0000, 0x4e80_0000)
    };
    let r = |lo, hi, zero| R { lo, hi, zero, abs: true };
    match g {
        G::Add => (r(add_lo, add_hi, true), r(add_lo, add_hi, true)),
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

/// Pushes an i32, nonzero when an operand may raise a flag (the instruction must go to the
/// interpreter): `a` (when the check reads operand 0) and `b` are v128 locals with the
/// operands (left as they are). False (nothing pushed) for G::Inexact, which checks none. A
/// scalar is checked the same way, on its lane only: float compares on |x| (a NaN fails them
/// all).
pub(super) fn operands(c: &mut Code, (g, bits, lanes, _): (G, u32, u32, bool), a: Option<u32>, b: Option<u32>, scratch: Option<u32>) -> bool {
    let b64 = bits == 64;
    let (ra, rb) = ranges(g, b64);
    if g == G::Inexact || a.is_none() && b.is_none() {
        return false;
    }
    if let (Some(a), Some(b), Some(t)) = (a, b, scratch) {
        if lanes * bits <= 64 && ra.abs && rb.abs && ra.lo != 0 && rb.lo != 0 {
            paired(c, b64, lanes, (a, ra), (b, rb), t);
            return true;
        }
    }
    {
        let (abs, eq, lt, ge) = if b64 { (0xec, 0x47, 0x49, 0x4c) } else { (0xe0, 0x41, 0x43, 0x46) };
        let mut first = true;
        for (x, r) in [(b, rb), (a, ra)] {
            let Some(x) = x else { continue };
            // (the locals stay as they are: the instruction reads them)
            let get = |c: &mut Code| {
                c.get(x);
                if r.abs {
                    c.simd(abs);
                }
            };
            // lo <= |x| < hi, or x = 0
            get(c);
            vconst(c, b64, r.hi);
            c.simd(lt);
            if r.lo != 0 {
                get(c);
                vconst(c, b64, r.lo);
                c.simd(ge);
                if r.zero {
                    // (|x| == 0; not abs: +0 only, as bits)
                    c.get(x).v128zero().simd(if r.abs { eq } else if b64 { 0xd6 } else { 0x37 }).simd(0x50); // or
                }
                c.simd(0x4e); // and
            }
            if !first {
                c.simd(0x4e);
            }
            first = false;
        }
        // the lanes the instruction doesn't read: fine
        if lanes * bits < 128 {
            c.simd(12);
            for i in 0..16 {
                c.b.push(if i < (lanes * bits / 8) as usize { 0 } else { 0xff });
            }
            c.simd(0x50);
        }
        c.simd(0xa3).eqz(); // i32x4.all_true
    }
    true
}

/// Both operands of a scalar (or two-lane f32) in one v128 (`t`), checked at once: the
/// same as [`operands`]' two checks, lane by lane.
fn paired(c: &mut Code, b64: bool, lanes: u32, (a, ra): (u32, R), (b, rb): (u32, R), t: u32) {
    let (abs, eq, lt, ge) = if b64 { (0xec, 0x47, 0x49, 0x4c) } else { (0xe0, 0x41, 0x43, 0x46) };
    let w = if b64 { 8 } else { 4 };
    let n = 16 / w;
    // lane j: b's lane j, a's lane j - lanes, then b's lane 0 again
    let (mut idx, mut hi, mut lo, mut z) = ([0u8; 16], [0u8; 16], [0u8; 16], [0u8; 16]);
    for j in 0..n {
        let (from, lane, r) = if j < lanes as usize { (0, j, rb) } else if j < 2 * lanes as usize { (16, j - lanes as usize, ra) } else { (0, 0, rb) };
        for k in 0..w {
            idx[j * w + k] = (from + lane * w + k) as u8;
            hi[j * w + k] = r.hi.to_le_bytes()[k];
            lo[j * w + k] = r.lo.to_le_bytes()[k];
            z[j * w + k] = if r.zero { 0xff } else { 0 };
        }
    }
    let v = |c: &mut Code, x: [u8; 16]| {
        c.simd(12);
        c.b.extend_from_slice(&x);
    };
    c.get(b).get(a).shuffle(idx).simd(abs).set(t);
    c.get(t);
    v(c, hi);
    c.simd(lt);
    c.get(t);
    v(c, lo);
    c.simd(ge);
    c.get(t).v128zero().simd(eq);
    v(c, z);
    c.simd(0x4e).simd(0x50).simd(0x4e); // and, or, and
    c.simd(0xa3).eqz(); // i32x4.all_true
}

/// What the check of operand `i` (0 or 1) proves of its register, for [`operands`] to skip
/// it while the register keeps its value: (range, lane bits, lanes)
pub(super) type Key = (R, u32, u32);
pub(super) fn key((g, bits, lanes, _): (G, u32, u32, bool), i: u32) -> Option<Key> {
    let (ra, rb) = ranges(g, bits == 64);
    (g != G::Inexact).then_some((if i == 0 { ra } else { rb }, bits, lanes))
}

/// Whether the instruction needs [`mode`] (it may round: not G::Special).
pub(super) fn rounds(g: G) -> bool {
    g != G::Special
}

/// Pushes an i32, nonzero when PE is not set yet: `cpu` the Cpu local, `mxcsr` the offset of
/// Cpu::mxcsr. (Only the interpreter changes MXCSR: once a block has checked, the rest of it
/// is fine. RC is to nearest whenever translated code runs, see jit::sse_ok.)
pub(super) fn mode(c: &mut Code, cpu: u32, mxcsr: u32) {
    c.get(cpu).load32(mxcsr).i32c(0x20).and().eqz();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every sum, difference, product and quotient of operands the ranges let translated code
    /// handle is 0 or normal in every rounding mode: nothing MXCSR.FTZ would flush, no UE
    /// (the range edges and random bits inside them, both signs).
    #[test]
    fn guarded_results_are_never_tiny() {
        let mut s = 0x9e37_79b9_7f4a_7c15u64;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        for g in [G::Add, G::Mul, G::Div] {
            for b64 in [false, true] {
                let (ra, rb) = ranges(g, b64);
                let pick = |r: R, x: u64| -> i64 {
                    let span = (r.hi - r.lo) as u64;
                    match x % 5 {
                        0 => r.lo,
                        1 => r.hi - 1,
                        2 if r.zero => 0,
                        _ => r.lo + ((x >> 8) % span) as i64,
                    }
                };
                for _ in 0..200_000 {
                    let (x, y) = (rnd(), rnd());
                    let (a, b) = (pick(ra, x), pick(rb, y));
                    let (sa, sb) = (x & 1 != 0, y & 1 != 0);
                    let tiny = if b64 {
                        let (a, b) = (f64::from_bits(a as u64 | (sa as u64) << 63), f64::from_bits(b as u64 | (sb as u64) << 63));
                        let r = match g {
                            G::Add => a + b,
                            G::Mul => a * b,
                            _ => a / b,
                        };
                        // (rounded to nearest: the bounds' own extreme results, 2^-1022 and
                        // the ulp multiples, are representable, so no rounding mode goes below)
                        r != 0.0 && r.abs() < f64::MIN_POSITIVE
                    } else {
                        let (a, b) = (f32::from_bits(a as u32 | (sa as u32) << 31), f32::from_bits(b as u32 | (sb as u32) << 31));
                        let r = match g {
                            G::Add => a + b,
                            G::Mul => a * b,
                            _ => a / b,
                        };
                        r != 0.0 && r.abs() < f32::MIN_POSITIVE
                    };
                    assert!(!tiny, "{g:?} b64={b64} {a:#x} {b:#x}");
                }
            }
        }
    }
}
