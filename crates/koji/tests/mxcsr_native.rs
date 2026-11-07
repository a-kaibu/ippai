//! The MXCSR exception flags koji computes for SSE arithmetic, against the host CPU's (all
//! exceptions masked, DAZ and FTZ off, round to nearest): operands drawn from the special
//! values (zeros, infinities, quiet and signaling NaNs, denormals, the edges of the normal
//! range) and from random bits. And the unmasked traps as the interpreter takes them (the
//! winref seh probe's cases: Windows reports the MXCSR after them).
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use core::arch::asm;
use core::arch::x86_64::*;
use koji::{flags32, flags64, narrow_flags, step, Cpu, Exit, FOp, Fault, Memory};

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

const S32: &[u32] = &[
    0, 0x8000_0000, 0x7f80_0000, 0xff80_0000, 0x7fc0_0000, 0xffc0_0001, 0x7f80_0001, 0xffa0_0000, 1, 0x8000_0001,
    0x007f_ffff, 0x0080_0000, 0x8080_0000, 0x7f7f_ffff, 0xff7f_ffff, 0x3f80_0000, 0xbf80_0000, 0x4040_0000,
    0x0040_0000, 0x3400_0000, 0x5f00_0000, 0x2000_0000, 0x0100_0000,
];
const S64: &[u64] = &[
    0, 1 << 63, 0x7ff0 << 48, 0xfff0 << 48, 0x7ff8 << 48, (0xfff8 << 48) | 1, (0x7ff0 << 48) | 1, 0xfff4 << 48, 1,
    (1 << 63) | 1, (1 << 52) - 1, 1 << 52, 0x7fef_ffff_ffff_ffff, 0xffef_ffff_ffff_ffff, 0x3ff0 << 48, 0xbff0 << 48,
    0x4008 << 48, 0x3810_0000_0000_0000, 0x47ef_ffff_e000_0000, 0x36a0_0000_0000_0000, 0x3cb0 << 48, 0x2000 << 48,
];

fn v32(r: &mut Rng) -> f32 {
    let x = r.next();
    f32::from_bits(match x % 4 {
        0 | 1 => S32[(x >> 8) as usize % S32.len()],
        // random bits around the denormal / overflow edges
        2 => ((x >> 32) as u32 & 0x80ff_ffff) | ((((x >> 24) as u32 % 6).wrapping_sub(2) & 0xff) << 23) | if x & 0x100 != 0 { 0x7c00_0000 } else { 0 },
        _ => (x >> 32) as u32,
    })
}

fn v64(r: &mut Rng) -> f64 {
    let x = r.next();
    f64::from_bits(match x % 4 {
        0 | 1 => S64[(x >> 8) as usize % S64.len()],
        2 => {
            let e = ((x >> 20) % 6).wrapping_sub(2) & 0x7ff;
            let e = if x & 0x100 != 0 { e ^ 0x7fc } else { e };
            (r.next() & 0x800f_ffff_ffff_ffff) | e << 52
        }
        _ => r.next(),
    })
}

macro_rules! host {
    ($ins:literal, $a:expr, $b:expr) => {{
        let mut mx: u32 = 0x1f80;
        let (mut a, b) = ($a, $b);
        unsafe {
            asm!("ldmxcsr [{m}]", concat!($ins, " {a}, {b}"), "stmxcsr [{m}]",
                 m = in(reg) &mut mx, a = inout(xmm_reg) a, b = in(xmm_reg) b, options(nostack));
        }
        let _ = a;
        mx & 0x3f
    }};
}

fn host32(op: FOp, x: f32, y: f32) -> u32 {
    {
        let (a, b) = unsafe { (_mm_set_ss(x), _mm_set_ss(y)) };
        match op {
            FOp::Add => host!("addss", a, b),
            FOp::Sub => host!("subss", a, b),
            FOp::Mul => host!("mulss", a, b),
            FOp::Div => host!("divss", a, b),
            FOp::Sqrt => host!("sqrtss", a, b),
            FOp::MinMax => host!("minss", a, b),
        }
    }
}

fn host64(op: FOp, x: f64, y: f64) -> u32 {
    {
        let (a, b) = unsafe { (_mm_set_sd(x), _mm_set_sd(y)) };
        match op {
            FOp::Add => host!("addsd", a, b),
            FOp::Sub => host!("subsd", a, b),
            FOp::Mul => host!("mulsd", a, b),
            FOp::Div => host!("divsd", a, b),
            FOp::Sqrt => host!("sqrtsd", a, b),
            FOp::MinMax => host!("maxsd", a, b),
        }
    }
}

const OPS: [FOp; 6] = [FOp::Add, FOp::Sub, FOp::Mul, FOp::Div, FOp::Sqrt, FOp::MinMax];

#[test]
fn single_precision_flags_match_the_host() {
    let mut r = Rng(0x1234_5678_9abc_def1);
    for _ in 0..300_000 {
        let (x, y) = (v32(&mut r), v32(&mut r));
        for op in OPS {
            let (pre, post) = flags32(op, x, y, false);
            assert_eq!(pre | post, host32(op, x, y), "{op:?} {:#x} {:#x}", x.to_bits(), y.to_bits());
        }
    }
}

#[test]
fn double_precision_flags_match_the_host() {
    let mut r = Rng(0x0fed_cba9_8765_4321);
    for _ in 0..300_000 {
        let (x, y) = (v64(&mut r), v64(&mut r));
        for op in OPS {
            let (pre, post) = flags64(op, x, y, false);
            assert_eq!(pre | post, host64(op, x, y), "{op:?} {:#x} {:#x}", x.to_bits(), y.to_bits());
        }
    }
}

#[test]
fn narrowing_flags_match_the_host() {
    let mut r = Rng(0x5555_aaaa_3333_cccc);
    for _ in 0..300_000 {
        let x = v64(&mut r);
        let (pre, post) = narrow_flags(x, false);
        let (a, b) = unsafe { (_mm_setzero_ps(), _mm_set_sd(x)) };
        let h = host!("cvtsd2ss", a, b);
        assert_eq!(pre | post, h, "{:#x}", x.to_bits());
    }
}

// ---- traps in the interpreter

struct Flat(Vec<u8>);
impl Memory for Flat {
    fn read8(&mut self, a: u32) -> Result<u8, Fault> {
        self.0.get(a as usize).copied().ok_or(Fault { addr: a, write: false })
    }
    fn write8(&mut self, a: u32, v: u8) -> Result<(), Fault> {
        match self.0.get_mut(a as usize) {
            Some(b) => {
                *b = v;
                Ok(())
            }
            None => Err(Fault { addr: a, write: true }),
        }
    }
}

/// Runs `code` from 0x1000 with MXCSR = `mx` until an exit other than Continue or the end.
fn run(code: &[u8], mx: u32) -> (Cpu, Exit) {
    let mut m = Flat(vec![0; 0x10000]);
    m.0[0x1000..0x1000 + code.len()].copy_from_slice(code);
    let mut cpu = Cpu::default();
    cpu.eip = 0x1000;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    cpu.mxcsr = mx;
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    loop {
        if cpu.eip >= 0x1000 + code.len() as u32 {
            return (cpu, Exit::Continue);
        }
        let e = step(&mut cpu, &mut m);
        if e != Exit::Continue {
            return (cpu, e);
        }
    }
}

// mov eax, 0x3f800000; movd xmm0, eax; xorps xmm1, xmm1; divss xmm0, xmm1
const DIV0: &[u8] = &[0xb8, 0, 0, 0x80, 0x3f, 0x66, 0x0f, 0x6e, 0xc0, 0x0f, 0x57, 0xc9, 0xf3, 0x0f, 0x5e, 0xc1];
// mov eax, 0x4f800000 (2^32); movd xmm0, eax; cvttss2si eax, xmm0
const CVT: &[u8] = &[0xb8, 0, 0, 0x80, 0x4f, 0x66, 0x0f, 0x6e, 0xc0, 0xf3, 0x0f, 0x2c, 0xc0];

#[test]
fn unmasked_divide_by_zero_traps_before_the_result() {
    let (cpu, e) = run(DIV0, 0x1f80 & !0x200);
    assert_eq!(e, Exit::SimdException);
    assert_eq!(cpu.eip, 0x100c, "at the DIVSS");
    assert_eq!(cpu.mxcsr, 0x1d84, "as Windows reports it");
    assert_eq!(cpu.xmm(0) as u32, 0x3f80_0000, "destination left alone");
}

#[test]
fn unmasked_invalid_conversion_traps() {
    let (cpu, e) = run(CVT, 0x1f80 & !0x80);
    assert_eq!(e, Exit::SimdException);
    assert_eq!(cpu.eip, 0x1009);
    assert_eq!(cpu.mxcsr, 0x1f01);
    assert_eq!(cpu.regs[0], 0x4f80_0000, "EAX left alone");
}

#[test]
fn masked_runs_on() {
    let (cpu, e) = run(CVT, 0x1f80);
    assert_eq!(e, Exit::Continue);
    assert_eq!(cpu.regs[0], 0x8000_0000);
    assert_eq!(cpu.mxcsr, 0x1f80, "fully masked: flags not tracked");
    let (cpu, e) = run(DIV0, 0x1f80 & !0x400); // (overflow unmasked: divide stays masked)
    assert_eq!(e, Exit::Continue);
    assert_eq!(cpu.xmm(0) as u32, 0x7f80_0000);
    assert_eq!(cpu.mxcsr, 0x1b80 | 4, "the masked flag is set");
}

#[test]
fn an_unmasked_pre_flag_hides_the_post_flags() {
    // denormal unmasked: 1 + denormal -> DE only (not PE)
    // mov eax, 1; movd xmm1, eax; mov eax, 0x3f800000; movd xmm0, eax; addss xmm0, xmm1
    let code = [0xb8, 1, 0, 0, 0, 0x66, 0x0f, 0x6e, 0xc8, 0xb8, 0, 0, 0x80, 0x3f, 0x66, 0x0f, 0x6e, 0xc0, 0xf3, 0x0f, 0x58, 0xc1];
    let (cpu, e) = run(&code, 0x1f80 & !0x100);
    assert_eq!(e, Exit::SimdException);
    assert_eq!(cpu.mxcsr & 0x3f, 2);
    // inexact unmasked, denormal masked: DE and PE, a trap
    let (cpu, e) = run(&code, 0x1f80 & !0x1000);
    assert_eq!(e, Exit::SimdException);
    assert_eq!(cpu.mxcsr & 0x3f, 0x22);
}
