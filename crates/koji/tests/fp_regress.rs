//! The differences Berkeley TestFloat found (crates/koji/tests/testfloat.rs), one case each, so
//! they stay fixed without the TestFloat build: MXCSR's overflow and underflow decided after
//! rounding in MXCSR's direction, Fast2Sum near f64::MAX, FMA in every rounding mode, the x87
//! storing a 64-bit FILD integer from its exact value, SNaNs quieted by x87 loads and FRNDINT.
use iced_x86::code_asm::*;
use koji::round::{fma32, fma64, sse64, Op};
use koji::{flags32, flags64, narrow_flags, Cpu, Exit, FOp, Fault, Memory};

const PE: u32 = 0x20;
const UE: u32 = 0x10;
const OE: u32 = 0x08;
const NEAR: u32 = 0;
const DOWN: u32 = 1;
const UP: u32 = 2;
const ZERO: u32 = 3;

fn f32b(b: u32) -> f32 {
    f32::from_bits(b)
}
fn f64b(b: u64) -> f64 {
    f64::from_bits(b)
}

#[test]
fn overflow_is_decided_after_rounding_in_rc() {
    // rounding up past f32::MAX overflows (TestFloat: 05), rounding toward zero just past it doesn't
    assert_eq!(flags32(FOp::Add, f32b(0x0000_0001), f32b(0x7f7f_ffff), false, UP).1, OE | PE);
    assert_eq!(flags32(FOp::Add, f32b(0xbf7f_ffff), f32b(0xff7f_ffff), false, DOWN).1, OE | PE);
    assert_eq!(flags32(FOp::Sub, f32b(0x7f00_0000), f32b(0xfeff_ffff), false, ZERO).1, PE);
    assert_eq!(flags64(FOp::Add, f64b(1), f64b(0x7fef_ffff_ffff_ffff), false, UP).1, OE | PE);
    assert_eq!(narrow_flags(f64b(0x47ef_ffff_ffff_ffff), false, DOWN).1, PE);
    assert_eq!(narrow_flags(f64b(0x47ef_ffff_ffff_ffff), false, UP).1, OE | PE);
}

#[test]
fn tininess_is_decided_after_rounding_at_unbounded_exponent() {
    // 2^-126 * (1 - 2^-24): rounds to the smallest normal, but at 24 bits and any exponent it
    // stays below it: tiny (TestFloat: 03)
    assert_eq!(flags32(FOp::Mul, f32b(0x0080_0000), f32b(0x3f7f_ffff), false, NEAR).1, UE | PE);
    assert_eq!(flags64(FOp::Div, f64b(0x001f_ffff_ffff_ffff), 2.0, false, NEAR).1, UE | PE);
    assert_eq!(narrow_flags(f64b(0x380f_ffff_ffff_ffff), false, DOWN).1, UE | PE);
    // just above the threshold rounding to nearest: not tiny
    assert_eq!(flags32(FOp::Mul, f32b(0x0080_0001), f32b(0x3f7f_ffff), false, NEAR).1, PE);
}

#[test]
fn sums_near_the_largest_double_round_in_rc() {
    // TwoSum's s - a overflowed here: rounded up instead of down
    let (a, b) = (f64b(0xffdf_ffff_fffd_ffff), f64b(0xffef_ffff_ffff_ffff));
    assert_eq!(sse64(Op::Sub, a, b, DOWN << 13).to_bits(), 0x7fe0_0000_0000_ffff);
    assert_eq!(sse64(Op::Sub, a, b, ZERO << 13).to_bits(), 0x7fe0_0000_0000_ffff);
    assert_eq!(sse64(Op::Sub, f64b(0x7fdf_ffbe_ffff_ffff), f64b(0x7fef_ffff_ffff_ffff), UP << 13).to_bits(), 0xffe0_0020_7fff_ffff);
}

#[test]
fn fma_rounds_in_rc() {
    assert_eq!(fma32(f32b(0x8683_f7ff), f32b(0xc07f_3fff), 0.0, UP).to_bits(), 0x0783_9505);
    assert_eq!(fma32(f32b(0x9ede_38f7), f32b(0x3e7f_7f7f), f32b(0xdf7e_ffff), DOWN).to_bits(), 0xdf7f_0000);
    assert_eq!(fma64(f64b(0x41e0_0003_fffb_ffff), f64b(0xbfdf_ffff_ffef_ffff), f64b(0x8025_1295_1031_85ae), ZERO).to_bits(), 0xc1d0_0003_fff3_fffc);
    // an exact zero sum rounding down is -0
    assert_eq!(fma64(f64b(0xc040_0000_0000_1000), 0.0, 0.0, DOWN).to_bits(), 0x8000_0000_0000_0000);
    assert_eq!(fma64(f64b(0xc040_0000_0000_1000), 0.0, 0.0, NEAR).to_bits(), 0);
}

struct Ram(Vec<u8>);
impl Memory for Ram {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        *self.0.get_mut(a as usize).ok_or(Fault { addr: a, write: true })? = v;
        Ok(())
    }
}

/// Runs `f`'s code with the qword `x` at [esi] and the x87 control word `cw`; the qword at [edi]
fn x87(cw: u16, x: u64, f: impl Fn(&mut CodeAssembler) -> Result<(), IcedError>) -> u64 {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a).unwrap();
    a.nop().unwrap();
    let code = a.assemble(0x1000).unwrap();
    let mut mem = Ram(vec![0; 0x4000]);
    mem.0[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    mem.0[0x2000..0x2008].copy_from_slice(&x.to_le_bytes());
    let mut cpu = Cpu { eip: 0x1000, ..Cpu::default() };
    cpu.regs[koji::ESI] = 0x2000;
    cpu.regs[koji::EDI] = 0x3000;
    cpu.fpu.cw = cw;
    while cpu.eip != 0x1000 + code.len() as u32 - 1 {
        assert_eq!(koji::step(&mut cpu, &mut mem), Exit::Continue);
    }
    u64::from_le_bytes(mem.0[0x3000..0x3008].try_into().unwrap())
}

#[test]
fn fild_m64_is_stored_from_its_exact_value() {
    let st64 = |a: &mut CodeAssembler| { a.fild(qword_ptr(esi))?; a.fstp(qword_ptr(edi)) };
    let st32 = |a: &mut CodeAssembler| { a.fild(qword_ptr(esi))?; a.fstp(dword_ptr(edi)) };
    // (TestFloat i64_to_f64 / i64_to_f32 cases: round down, up; to nearest without double rounding)
    assert_eq!(x87(0x077f, 0x07ff_fdff_ffff_ff7f, st64), 0x439f_fff7_ffff_fffd);
    assert_eq!(x87(0x0b7f, 0x1000_0000_0001_003e, st64), 0x43b0_0000_0000_0101);
    assert_eq!(x87(0x037f, 0xffbf_dfff_bfff_ffff, st32) as u32, 0xda80_4001);
    assert_eq!(x87(0x037f, 0x0200_0000_0000_0002, st32) as u32, 0x5c00_0000);
    assert_eq!(x87(0x0b7f, 0x0200_0000_0000_0002, st32) as u32, 0x5c00_0001);
    // FSTP m80: all 64 bits
    let st80 = |a: &mut CodeAssembler| { a.fild(qword_ptr(esi))?; a.fstp(tword_ptr(edi)) };
    assert_eq!(x87(0x037f, 0x07ff_fdff_ffff_ff7f, st80), 0x07ff_fdff_ffff_ff7f << 5);
}

#[test]
fn x87_loads_and_frndint_quiet_snans() {
    let mv = |a: &mut CodeAssembler| { a.fld(qword_ptr(esi))?; a.fstp(qword_ptr(edi)) };
    assert_eq!(x87(0x037f, 0x7ff4_f3d1_14af_58e4, mv), 0x7ffc_f3d1_14af_58e4);
    let rnd = |a: &mut CodeAssembler| { a.fld(qword_ptr(esi))?; a.frndint()?; a.fstp(qword_ptr(edi)) };
    assert_eq!(x87(0x037f, 0xfff0_7fff_ffff_bfff, rnd), 0xfff8_7fff_ffff_bfff);
}
