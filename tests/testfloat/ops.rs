//! The instructions checked against Berkeley TestFloat (tests/testfloat/build.sh): which
//! TestFloat function each one computes, its operand and result types, and the 32-bit code that
//! runs it. Shared by crates/koji/tests/testfloat.rs (the interpreter, every case) and
//! tools/jit32test (`testfloat`: the JIT, a sample).
//!
//! Code conventions: operands at [esi], [esi+8], [esi+16] (8 bytes each, f32/i32 in the low
//! half), the result written to [edi], ebx = data base. The harness adds the mode setup and the
//! flag read ([edi+8]) around it.
#![allow(dead_code)]
use iced_x86::code_asm::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ty {
    F32,
    F64,
    I32,
    I64,
    /// a comparison: 0 or 1 in a dword
    Bool,
}

impl Ty {
    pub fn bytes(self) -> usize {
        match self {
            Ty::F32 | Ty::I32 | Ty::Bool => 4,
            Ty::F64 | Ty::I64 => 8,
        }
    }
    pub fn is_zero(self, v: u64) -> bool {
        match self {
            Ty::F32 => v as u32 & 0x7fff_ffff == 0,
            Ty::F64 => v & !(1 << 63) == 0,
            _ => false,
        }
    }
    pub fn is_inf(self, v: u64) -> bool {
        match self {
            Ty::F32 => v as u32 & 0x7fff_ffff == 0x7f80_0000,
            Ty::F64 => v & !(1 << 63) == 0x7ff0 << 48,
            _ => false,
        }
    }
    pub fn is_nan(self, v: u64) -> bool {
        match self {
            Ty::F32 => v as u32 & 0x7fff_ffff > 0x7f80_0000,
            Ty::F64 => v & !(1 << 63) > 0x7ff0 << 48,
            _ => false,
        }
    }
}

pub type Asm = fn(&mut CodeAssembler) -> Result<(), IcedError>;

pub struct Op {
    /// shown in reports: instruction (and variant)
    pub name: &'static str,
    /// the TestFloat function and its options (besides the rounding mode)
    pub tf: &'static str,
    pub opts: &'static [&'static str],
    pub args: &'static [Ty],
    pub out: Ty,
    /// x87 code: the precision control (0 = 24 bits, 2 = 53, 3 = 64) it runs at; else SSE
    pub x87: Option<u16>,
    /// whether the result depends on the rounding mode (else run at round-to-nearest only)
    pub rounds: bool,
    /// test cases: TestFloat level 1 by default; Some(n) limits (mulAdd's level 1 is 6 M)
    pub n: Option<u32>,
    pub asm: Asm,
}

/// x86 rounding control (MXCSR.RC, x87 CW.RC) -> TestFloat's option
pub const RC: [&str; 4] = ["-rnear_even", "-rmin", "-rmax", "-rminMag"];

/// TestFloat flags (bit 0 inexact, 1 underflow, 2 overflow, 3 infinite, 4 invalid) as MXCSR's
/// exception bits (PE UE OE ZE IE; DE, the denormal operand flag, has no TestFloat equivalent)
pub fn mxcsr_flags(tf: u32) -> u32 {
    [(1, 0x20), (2, 0x10), (4, 0x08), (8, 0x04), (16, 0x01)].iter().filter(|(t, _)| tf & t != 0).map(|(_, m)| m).sum()
}

/// The MXCSR exception bits compared (not DE)
pub const MX_CMP: u32 = 0x3d;

const F32: Ty = Ty::F32;
const F64: Ty = Ty::F64;
const I32: Ty = Ty::I32;
const I64: Ty = Ty::I64;
const B: Ty = Ty::Bool;

macro_rules! op {
    ($name:expr, $tf:expr, $opts:expr, [$($a:expr),*], $out:expr, $x87:expr, $rounds:expr, $n:expr, $asm:expr) => {
        Op { name: $name, tf: $tf, opts: $opts, args: &[$($a),*], out: $out, x87: $x87, rounds: $rounds, n: $n, asm: $asm }
    };
}

/// eax = 1 if (cc1 and cc2) after a COMIS/FCOMI, as a dword at [edi]
fn both(a: &mut CodeAssembler, f: fn(&mut CodeAssembler) -> Result<(), IcedError>) -> Result<(), IcedError> {
    f(a)?;
    a.setnp(cl)?;
    a.and(al, cl)?;
    a.movzx(eax, al)?;
    a.mov(dword_ptr(edi), eax)
}

/// CMPSS/CMPSD's mask as 0/1 at [edi]
fn mask(a: &mut CodeAssembler) -> Result<(), IcedError> {
    a.movd(eax, xmm0)?;
    a.and(eax, 1)?;
    a.mov(dword_ptr(edi), eax)
}

pub fn ops() -> Vec<Op> {
    let ex: &'static [&'static str] = &["-exact"];
    let nx: &'static [&'static str] = &["-notexact"];
    let no: &'static [&'static str] = &[];
    vec![
        // SSE scalar arithmetic
        op!("addss", "f32_add", no, [F32, F32], F32, None, true, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.addss(xmm0, dword_ptr(esi + 8))?; a.movss(dword_ptr(edi), xmm0) }),
        op!("subss", "f32_sub", no, [F32, F32], F32, None, true, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.subss(xmm0, dword_ptr(esi + 8))?; a.movss(dword_ptr(edi), xmm0) }),
        op!("mulss", "f32_mul", no, [F32, F32], F32, None, true, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.mulss(xmm0, dword_ptr(esi + 8))?; a.movss(dword_ptr(edi), xmm0) }),
        op!("divss", "f32_div", no, [F32, F32], F32, None, true, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.divss(xmm0, dword_ptr(esi + 8))?; a.movss(dword_ptr(edi), xmm0) }),
        op!("sqrtss", "f32_sqrt", no, [F32], F32, None, true, None, |a| { a.sqrtss(xmm0, dword_ptr(esi))?; a.movss(dword_ptr(edi), xmm0) }),
        op!("addsd", "f64_add", no, [F64, F64], F64, None, true, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.addsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        op!("subsd", "f64_sub", no, [F64, F64], F64, None, true, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.subsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        op!("mulsd", "f64_mul", no, [F64, F64], F64, None, true, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.mulsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        op!("divsd", "f64_div", no, [F64, F64], F64, None, true, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.divsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        op!("sqrtsd", "f64_sqrt", no, [F64], F64, None, true, None, |a| { a.sqrtsd(xmm0, qword_ptr(esi))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        // registers instead of memory operands (the JIT's other path)
        op!("addss reg", "f32_add", no, [F32, F32], F32, None, true, None, |a| { a.movss(xmm1, dword_ptr(esi))?; a.movss(xmm2, dword_ptr(esi + 8))?; a.addss(xmm1, xmm2)?; a.movss(dword_ptr(edi), xmm1) }),
        op!("mulsd reg", "f64_mul", no, [F64, F64], F64, None, true, None, |a| { a.movsd_2(xmm1, qword_ptr(esi))?; a.movsd_2(xmm2, qword_ptr(esi + 8))?; a.mulsd(xmm1, xmm2)?; a.movsd_2(qword_ptr(edi), xmm1) }),
        // packed: lane 0 (the other lanes hold the same operands)
        op!("addps", "f32_add", no, [F32, F32], F32, None, true, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.shufps(xmm0, xmm0, 0)?; a.movss(xmm1, dword_ptr(esi + 8))?; a.shufps(xmm1, xmm1, 0)?; a.addps(xmm0, xmm1)?; a.shufps(xmm0, xmm0, 0xff)?; a.movss(dword_ptr(edi), xmm0) }),
        op!("divpd", "f64_div", no, [F64, F64], F64, None, true, None, |a| { a.movddup(xmm0, qword_ptr(esi))?; a.movddup(xmm1, qword_ptr(esi + 8))?; a.divpd(xmm0, xmm1)?; a.movhpd(qword_ptr(edi), xmm0) }),
        // VEX
        op!("vaddss", "f32_add", no, [F32, F32], F32, None, true, None, |a| { a.vmovss(xmm1, dword_ptr(esi))?; a.vaddss(xmm0, xmm1, dword_ptr(esi + 8))?; a.vmovss(dword_ptr(edi), xmm0) }),
        op!("vsqrtsd", "f64_sqrt", no, [F64], F64, None, true, None, |a| { a.vsqrtsd(xmm0, xmm7, qword_ptr(esi))?; a.vmovsd(qword_ptr(edi), xmm0) }),
        op!("vfmadd132ss", "f32_mulAdd", no, [F32, F32, F32], F32, None, true, Some(200_000), |a| { a.movss(xmm0, dword_ptr(esi))?; a.movss(xmm1, dword_ptr(esi + 8))?; a.movss(xmm2, dword_ptr(esi + 16))?; a.vfmadd132ss(xmm0, xmm2, xmm1)?; a.movss(dword_ptr(edi), xmm0) }),
        op!("vfmadd132sd", "f64_mulAdd", no, [F64, F64, F64], F64, None, true, Some(200_000), |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.movsd_2(xmm1, qword_ptr(esi + 8))?; a.movsd_2(xmm2, qword_ptr(esi + 16))?; a.vfmadd132sd(xmm0, xmm2, xmm1)?; a.movsd_2(qword_ptr(edi), xmm0) }),
        // SSE conversions
        op!("cvtss2sd", "f32_to_f64", no, [F32], F64, None, false, None, |a| { a.cvtss2sd(xmm0, dword_ptr(esi))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        op!("cvtsd2ss", "f64_to_f32", no, [F64], F32, None, true, None, |a| { a.cvtsd2ss(xmm0, qword_ptr(esi))?; a.movss(dword_ptr(edi), xmm0) }),
        op!("cvtsi2ss", "i32_to_f32", no, [I32], F32, None, true, None, |a| { a.cvtsi2ss(xmm0, dword_ptr(esi))?; a.movss(dword_ptr(edi), xmm0) }),
        op!("cvtsi2sd", "i32_to_f64", no, [I32], F64, None, false, None, |a| { a.cvtsi2sd(xmm0, dword_ptr(esi))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        op!("cvtss2si", "f32_to_i32", ex, [F32], I32, None, true, None, |a| { a.cvtss2si(eax, dword_ptr(esi))?; a.mov(dword_ptr(edi), eax) }),
        op!("cvttss2si", "f32_to_i32_r_minMag", ex, [F32], I32, None, false, None, |a| { a.cvttss2si(eax, dword_ptr(esi))?; a.mov(dword_ptr(edi), eax) }),
        op!("cvtsd2si", "f64_to_i32", ex, [F64], I32, None, true, None, |a| { a.cvtsd2si(eax, qword_ptr(esi))?; a.mov(dword_ptr(edi), eax) }),
        op!("cvttsd2si", "f64_to_i32_r_minMag", ex, [F64], I32, None, false, None, |a| { a.cvttsd2si(eax, qword_ptr(esi))?; a.mov(dword_ptr(edi), eax) }),
        op!("cvtps2dq", "f32_to_i32", ex, [F32], I32, None, true, None, |a| { a.movss(xmm1, dword_ptr(esi))?; a.cvtps2dq(xmm0, xmm1)?; a.movd(dword_ptr(edi), xmm0) }),
        op!("cvttpd2dq", "f64_to_i32_r_minMag", ex, [F64], I32, None, false, None, |a| { a.movsd_2(xmm1, qword_ptr(esi))?; a.cvttpd2dq(xmm0, xmm1)?; a.movd(dword_ptr(edi), xmm0) }),
        // SSE4.1 rounding to an integer: imm RC (inexact signalled, or not with bit 3), MXCSR.RC
        op!("roundss imm", "f32_roundToInt", ex, [F32], F32, None, true, None, |a| a.nop()),
        op!("roundss imm|8", "f32_roundToInt", nx, [F32], F32, None, true, None, |a| a.nop()),
        op!("roundss mxcsr", "f32_roundToInt", ex, [F32], F32, None, true, None, |a| { a.roundss(xmm0, dword_ptr(esi), 4)?; a.movss(dword_ptr(edi), xmm0) }),
        op!("roundsd imm", "f64_roundToInt", ex, [F64], F64, None, true, None, |a| a.nop()),
        op!("roundsd imm|8", "f64_roundToInt", nx, [F64], F64, None, true, None, |a| a.nop()),
        op!("roundsd mxcsr", "f64_roundToInt", ex, [F64], F64, None, true, None, |a| { a.roundsd(xmm0, qword_ptr(esi), 4)?; a.movsd_2(qword_ptr(edi), xmm0) }),
        // SSE comparisons: CMPSS predicates and COMIS (signalling) / UCOMIS (quiet)
        op!("cmpeqss", "f32_eq", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.cmpss(xmm0, dword_ptr(esi + 8), 0)?; mask(a) }),
        op!("cmpltss", "f32_lt", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.cmpss(xmm0, dword_ptr(esi + 8), 1)?; mask(a) }),
        op!("cmpless", "f32_le", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm0, dword_ptr(esi))?; a.cmpss(xmm0, dword_ptr(esi + 8), 2)?; mask(a) }),
        op!("vcmpss eq_os", "f32_eq_signaling", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm1, dword_ptr(esi))?; a.vcmpss(xmm0, xmm1, dword_ptr(esi + 8), 0x10)?; mask(a) }),
        op!("vcmpss lt_oq", "f32_lt_quiet", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm1, dword_ptr(esi))?; a.vcmpss(xmm0, xmm1, dword_ptr(esi + 8), 0x11)?; mask(a) }),
        op!("vcmpss le_oq", "f32_le_quiet", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm1, dword_ptr(esi))?; a.vcmpss(xmm0, xmm1, dword_ptr(esi + 8), 0x12)?; mask(a) }),
        op!("cmpeqsd", "f64_eq", no, [F64, F64], B, None, false, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.cmpsd_3(xmm0, qword_ptr(esi + 8), 0)?; mask(a) }),
        op!("cmpltsd", "f64_lt", no, [F64, F64], B, None, false, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.cmpsd_3(xmm0, qword_ptr(esi + 8), 1)?; mask(a) }),
        op!("cmplesd", "f64_le", no, [F64, F64], B, None, false, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.cmpsd_3(xmm0, qword_ptr(esi + 8), 2)?; mask(a) }),
        op!("ucomiss eq", "f32_eq", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm0, dword_ptr(esi))?; both(a, |a| { a.ucomiss(xmm0, dword_ptr(esi + 8))?; a.setz(al) }) }),
        op!("ucomiss lt", "f32_lt_quiet", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm0, dword_ptr(esi))?; both(a, |a| { a.ucomiss(xmm0, dword_ptr(esi + 8))?; a.setb(al) }) }),
        op!("comiss le", "f32_le", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm0, dword_ptr(esi))?; both(a, |a| { a.comiss(xmm0, dword_ptr(esi + 8))?; a.setbe(al) }) }),
        op!("comiss eq", "f32_eq_signaling", no, [F32, F32], B, None, false, None, |a| { a.movss(xmm0, dword_ptr(esi))?; both(a, |a| { a.comiss(xmm0, dword_ptr(esi + 8))?; a.setz(al) }) }),
        op!("ucomisd le", "f64_le_quiet", no, [F64, F64], B, None, false, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; both(a, |a| { a.ucomisd(xmm0, qword_ptr(esi + 8))?; a.setbe(al) }) }),
        op!("comisd lt", "f64_lt", no, [F64, F64], B, None, false, None, |a| { a.movsd_2(xmm0, qword_ptr(esi))?; both(a, |a| { a.comisd(xmm0, qword_ptr(esi + 8))?; a.setb(al) }) }),
        // x87 at double precision: f64 results (double rounding below the normal range and
        // the two-NaN rule are the x87's own: not compared, see testfloat.rs)
        op!("fadd m64", "f64_add", no, [F64, F64], F64, Some(2), true, None, |a| { a.fld(qword_ptr(esi))?; a.fadd(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        op!("fsub m64", "f64_sub", no, [F64, F64], F64, Some(2), true, None, |a| { a.fld(qword_ptr(esi))?; a.fsub(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        op!("fsubr m64", "f64_sub", no, [F64, F64], F64, Some(2), true, None, |a| { a.fld(qword_ptr(esi + 8))?; a.fsubr(qword_ptr(esi))?; a.fstp(qword_ptr(edi)) }),
        op!("fmul m64", "f64_mul", no, [F64, F64], F64, Some(2), true, None, |a| { a.fld(qword_ptr(esi))?; a.fmul(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        op!("fdiv m64", "f64_div", no, [F64, F64], F64, Some(2), true, None, |a| { a.fld(qword_ptr(esi))?; a.fdiv(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        op!("fdivrp", "f64_div", no, [F64, F64], F64, Some(2), true, None, |a| { a.fld(qword_ptr(esi + 8))?; a.fld(qword_ptr(esi))?; a.fdivrp(st1, st0)?; a.fstp(qword_ptr(edi)) }),
        op!("fsqrt", "f64_sqrt", no, [F64], F64, Some(2), true, None, |a| { a.fld(qword_ptr(esi))?; a.fsqrt()?; a.fstp(qword_ptr(edi)) }),
        // ...at single precision: f32 results
        op!("fadd m32 pc24", "f32_add", no, [F32, F32], F32, Some(0), true, None, |a| { a.fld(dword_ptr(esi))?; a.fadd(dword_ptr(esi + 8))?; a.fstp(dword_ptr(edi)) }),
        op!("fmul m32 pc24", "f32_mul", no, [F32, F32], F32, Some(0), true, None, |a| { a.fld(dword_ptr(esi))?; a.fmul(dword_ptr(esi + 8))?; a.fstp(dword_ptr(edi)) }),
        op!("fdiv m32 pc24", "f32_div", no, [F32, F32], F32, Some(0), true, None, |a| { a.fld(dword_ptr(esi))?; a.fdiv(dword_ptr(esi + 8))?; a.fstp(dword_ptr(edi)) }),
        op!("fsqrt pc24", "f32_sqrt", no, [F32], F32, Some(0), true, None, |a| { a.fld(dword_ptr(esi))?; a.fsqrt()?; a.fstp(dword_ptr(edi)) }),
        // x87 loads, stores, integer conversions, FRNDINT, FPREM1, FUCOMI/FCOMI
        op!("fld m32, fstp m64", "f32_to_f64", no, [F32], F64, Some(3), false, None, |a| { a.fld(dword_ptr(esi))?; a.fstp(qword_ptr(edi)) }),
        op!("fld m64, fstp m32", "f64_to_f32", no, [F64], F32, Some(3), true, None, |a| { a.fld(qword_ptr(esi))?; a.fstp(dword_ptr(edi)) }),
        op!("fild m32", "i32_to_f64", no, [I32], F64, Some(3), false, None, |a| { a.fild(dword_ptr(esi))?; a.fstp(qword_ptr(edi)) }),
        op!("fild m64, fstp m64", "i64_to_f64", no, [I64], F64, Some(3), true, None, |a| { a.fild(qword_ptr(esi))?; a.fstp(qword_ptr(edi)) }),
        op!("fild m64, fstp m32", "i64_to_f32", no, [I64], F32, Some(3), true, None, |a| { a.fild(qword_ptr(esi))?; a.fstp(dword_ptr(edi)) }),
        op!("fistp m32", "f64_to_i32", ex, [F64], I32, Some(3), true, None, |a| { a.fld(qword_ptr(esi))?; a.fistp(dword_ptr(edi)) }),
        op!("fistp m64", "f64_to_i64", ex, [F64], I64, Some(3), true, None, |a| { a.fld(qword_ptr(esi))?; a.fistp(qword_ptr(edi)) }),
        op!("fisttp m32", "f64_to_i32_r_minMag", ex, [F64], I32, Some(3), false, None, |a| { a.fld(qword_ptr(esi))?; a.fisttp(dword_ptr(edi)) }),
        op!("fisttp m64", "f64_to_i64_r_minMag", ex, [F64], I64, Some(3), false, None, |a| { a.fld(qword_ptr(esi))?; a.fisttp(qword_ptr(edi)) }),
        op!("fist m32 (f32)", "f32_to_i32", ex, [F32], I32, Some(3), true, None, |a| { a.fld(dword_ptr(esi))?; a.fistp(dword_ptr(edi)) }),
        op!("frndint", "f64_roundToInt", ex, [F64], F64, Some(3), true, None, |a| { a.fld(qword_ptr(esi))?; a.frndint()?; a.fstp(qword_ptr(edi)) }),
        op!("fprem1", "f64_rem", no, [F64, F64], F64, Some(3), false, None, |a| {
            let mut again = a.create_label();
            a.fld(qword_ptr(esi + 8))?;
            a.fld(qword_ptr(esi))?;
            a.set_label(&mut again)?;
            a.fprem1()?;
            a.fnstsw(ax)?;
            a.test(ah, 4)?;
            a.jne(again)?;
            a.fstp(qword_ptr(edi))?;
            a.fstp(st0)
        }),
        op!("fucomip eq", "f64_eq", no, [F64, F64], B, Some(3), false, None, |a| { a.fld(qword_ptr(esi + 8))?; a.fld(qword_ptr(esi))?; both(a, |a| { a.fucomip(st0, st1)?; a.fstp(st0)?; a.setz(al) }) }),
        op!("fucomip lt", "f64_lt_quiet", no, [F64, F64], B, Some(3), false, None, |a| { a.fld(qword_ptr(esi + 8))?; a.fld(qword_ptr(esi))?; both(a, |a| { a.fucomip(st0, st1)?; a.fstp(st0)?; a.setb(al) }) }),
        op!("fcomip le", "f64_le", no, [F64, F64], B, Some(3), false, None, |a| { a.fld(qword_ptr(esi + 8))?; a.fld(qword_ptr(esi))?; both(a, |a| { a.fcomip(st0, st1)?; a.fstp(st0)?; a.setbe(al) }) }),
    ]
}

/// The code of `op` at rounding mode `rc`: ROUNDSS/SD with the immediate take it from here
pub fn body(op: &Op, rc: u32, a: &mut CodeAssembler) -> Result<(), IcedError> {
    match op.name {
        "roundss imm" | "roundss imm|8" => {
            a.roundss(xmm0, dword_ptr(esi), rc | if op.name.ends_with("|8") { 8 } else { 0 })?;
            a.movss(dword_ptr(edi), xmm0)
        }
        "roundsd imm" | "roundsd imm|8" => {
            a.roundsd(xmm0, qword_ptr(esi), rc | if op.name.ends_with("|8") { 8 } else { 0 })?;
            a.movsd_2(qword_ptr(edi), xmm0)
        }
        _ => (op.asm)(a),
    }
}

/// The x87 control word for `op` at rounding mode `rc` (all exceptions masked)
pub fn x87_cw(pc: u16, rc: u32) -> u16 {
    0x7f | pc << 8 | (rc as u16) << 10
}

/// One TestFloat case: operands, expected result, expected flags (TestFloat's bits)
pub struct Case {
    pub args: [u64; 3],
    pub want: u64,
    pub flags: u32,
}

pub fn parse_line(op: &Op, l: &str) -> Option<Case> {
    let f: Vec<&str> = l.split_whitespace().collect();
    if f.len() != op.args.len() + 2 {
        return None;
    }
    let mut args = [0; 3];
    for (i, s) in f[..op.args.len()].iter().enumerate() {
        args[i] = u64::from_str_radix(s, 16).ok()?;
    }
    Some(Case { args, want: u64::from_str_radix(f[op.args.len()], 16).ok()?, flags: u32::from_str_radix(f[op.args.len() + 1], 16).ok()? })
}

/// Cases koji does not have to match: the x87's double rounding below the normal range (it
/// rounds to the precision control's bits at the 80-bit format's exponent range, then again
/// when storing: TestFloat's single rounding differs; flagged underflow by TestFloat) and the
/// x87's two-NaN rule (larger significand, not SSE's first operand); and FMA's 0 * inf + NaN,
/// which x86 CPUs answer with that NaN (quieted; a QNaN without the invalid flag) (crates/koji/tests/diff_isa.rs
/// against the host CPU) where SoftFloat gives the default NaN and invalid
pub fn skipped(op: &Op, c: &Case) -> bool {
    let arith = ["_add", "_sub", "_mul", "_div"].iter().any(|s| op.tf.ends_with(s));
    let nans = op.args.iter().zip(c.args).filter(|(t, v)| t.is_nan(*v)).count();
    let t = op.args[0];
    let zero_inf = |a: u64, b: u64| t.is_zero(a) && t.is_inf(b) || t.is_inf(a) && t.is_zero(b);
    if op.tf.ends_with("_mulAdd") && t.is_nan(c.args[2]) && zero_inf(c.args[0], c.args[1]) {
        return true;
    }
    op.x87.is_some() && (arith && c.flags & 2 != 0 || nans > 1)
}

/// Runs testfloat_gen for `op` at `rc`, `f` on each output line (the first `op.n` lines when
/// limited: testfloat_gen's own -n can't go below the level's count)
pub fn generate(gen: &std::path::Path, op: &Op, rc: u32, level: u32, mut f: impl FnMut(&str)) {
    use std::io::BufRead;
    // (truncating conversions: testfloat_gen's <float>_to_<int> toward zero)
    let (tf, rc) = match op.tf.strip_suffix("_r_minMag") {
        Some(t) => (t, 3),
        None => (op.tf, rc),
    };
    let mut child = std::process::Command::new(gen)
        .arg(RC[rc as usize])
        .args(op.opts)
        .arg("-level")
        .arg(level.to_string())
        .arg(tf)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("testfloat_gen");
    let out = std::io::BufReader::new(child.stdout.take().unwrap());
    let max = op.n.map_or(usize::MAX, |n| n as usize);
    let mut n = 0;
    for l in out.lines() {
        f(&l.unwrap());
        n += 1;
        if n >= max {
            let _ = child.kill();
            break;
        }
    }
    let st = child.wait().unwrap();
    assert!(n >= max || st.success(), "testfloat_gen {} failed", op.tf);
}
