//! MXCSR's DAZ (denormal operands read as zero) and FTZ (a denormal result, underflow masked,
//! becomes zero with UE and PE) in koji's interpreter against the host CPU (#70): the SSE
//! arithmetic, square roots, min/max, compares, conversions and horizontal adds, scalar and
//! packed, single and double, with operands from the special values (denormals of both signs,
//! the normal range's edges, zeros, NaNs) and random bits around the denormal range, in every
//! combination of DAZ, FTZ and rounding: the destination's bits, MXCSR's flags and (compares)
//! EFLAGS.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use core::arch::asm;
use koji::{step, Cpu, Exit, Fault, Memory};

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
    0, 0x8000_0000, 1, 0x8000_0001, 0x007f_ffff, 0x807f_ffff, 0x0040_0000, 0x0080_0000, 0x8080_0000, 0x0080_0001,
    0x0100_0000, 0x3f80_0000, 0xbf80_0000, 0x3400_0000, 0x0c80_0000, 0x7f80_0000, 0x7fc0_0000, 0x7fa0_0000, 0x4040_0000,
];
const S64: &[u64] = &[
    0, 1 << 63, 1, (1 << 63) | 1, (1 << 52) - 1, 0x800f_ffff_ffff_ffff, 1 << 51, 1 << 52, (1 << 63) | (1 << 52),
    (1 << 52) + 1, 2 << 52, 0x3ff0 << 48, 0xbff0 << 48, 0x3cb0 << 48, 0x0360 << 48, 0x7ff0 << 48, 0x7ff8 << 48,
    0x7ff4 << 48, 0x36a0_0000_0000_0000, 0x3810_0000_0000_0000,
];

fn lane32(r: &mut Rng) -> u32 {
    let x = r.next();
    match x % 3 {
        0 => S32[(x >> 8) as usize % S32.len()],
        // denormals and the smallest normals, either sign
        1 => ((x >> 32) as u32 & 0x80ff_ffff) & if x & 0x200 != 0 { 0x807f_ffff } else { 0x80ff_ffff },
        _ => (x >> 32) as u32 & 0x8fff_ffff | if x & 0x400 != 0 { 0x3000_0000 } else { 0 },
    }
}
fn lane64(r: &mut Rng) -> u64 {
    let x = r.next();
    match x % 3 {
        0 => S64[(x >> 8) as usize % S64.len()],
        1 => r.next() & if x & 0x200 != 0 { 0x800f_ffff_ffff_ffff } else { 0x801f_ffff_ffff_ffff },
        _ => r.next() & 0x83ff_ffff_ffff_ffff | if x & 0x400 != 0 { 0x3000 << 48 } else { 0 },
    }
}
fn vec(r: &mut Rng, single: bool) -> u128 {
    if single {
        (0..4).fold(0u128, |v, i| v | (lane32(r) as u128) << (32 * i))
    } else {
        lane64(r) as u128 | (lane64(r) as u128) << 64
    }
}

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

/// (name, opcode bytes of `op xmm0, xmm1`, single, whether the result is EFLAGS)
type Case = (&'static str, &'static [u8], bool, bool);

/// koji: movups xmm0, [0x2000]; movups xmm1, [0x2010]; op xmm0, xmm1; movups [0x2020], xmm0;
/// pushfd; pop eax. (xmm0 after, MXCSR, EFLAGS' ZF PF CF)
fn koji_run(op: &[u8], a: u128, b: u128, mx: u32) -> (u128, u32, u32) {
    let mut code = vec![0x0f, 0x10, 0x05, 0, 0x20, 0, 0, 0x0f, 0x10, 0x0d, 0x10, 0x20, 0, 0];
    code.extend_from_slice(op);
    code.push(0xc1);
    code.extend_from_slice(&[0x0f, 0x11, 0x05, 0x20, 0x20, 0, 0, 0x9c, 0x58]);
    let mut m = Flat(vec![0; 0x10000]);
    m.0[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    m.0[0x2000..0x2010].copy_from_slice(&a.to_le_bytes());
    m.0[0x2010..0x2020].copy_from_slice(&b.to_le_bytes());
    let mut cpu = Cpu::default();
    cpu.eip = 0x1000;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    cpu.mxcsr = mx;
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    while cpu.eip < 0x1000 + code.len() as u32 {
        assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    }
    let r = u128::from_le_bytes(m.0[0x2020..0x2030].try_into().unwrap());
    (r, cpu.mxcsr & 0x3f, cpu.regs[0] & 0x45)
}

macro_rules! host_op {
    ($ins:literal, $a:expr, $b:expr, $mx:expr) => {{
        let mut mx: u32 = $mx;
        let mut a: core::arch::x86_64::__m128 = unsafe { core::mem::transmute($a) };
        let b: core::arch::x86_64::__m128 = unsafe { core::mem::transmute($b) };
        let fl: u64;
        // (the test's own MXCSR is put back: koji's arithmetic runs on this thread)
        let mut keep: u32 = 0;
        unsafe {
            asm!("stmxcsr [{k}]", "ldmxcsr [{m}]", concat!($ins, " {a}, {b}"), "pushfq", "pop {f}", "stmxcsr [{m}]", "ldmxcsr [{k}]",
                 m = in(reg) &mut mx, k = in(reg) &mut keep, a = inout(xmm_reg) a, b = in(xmm_reg) b, f = out(reg) fl);
        }
        (unsafe { core::mem::transmute::<_, u128>(a) }, mx & 0x3f, fl as u32 & 0x45)
    }};
}

fn host(name: &str, a: u128, b: u128, mx: u32) -> (u128, u32, u32) {
    match name {
        "addss" => host_op!("addss", a, b, mx),
        "subss" => host_op!("subss", a, b, mx),
        "mulss" => host_op!("mulss", a, b, mx),
        "divss" => host_op!("divss", a, b, mx),
        "sqrtss" => host_op!("sqrtss", a, b, mx),
        "minss" => host_op!("minss", a, b, mx),
        "maxss" => host_op!("maxss", a, b, mx),
        "addps" => host_op!("addps", a, b, mx),
        "mulps" => host_op!("mulps", a, b, mx),
        "divps" => host_op!("divps", a, b, mx),
        "sqrtps" => host_op!("sqrtps", a, b, mx),
        "minps" => host_op!("minps", a, b, mx),
        "haddps" => host_op!("haddps", a, b, mx),
        "addsd" => host_op!("addsd", a, b, mx),
        "subsd" => host_op!("subsd", a, b, mx),
        "mulsd" => host_op!("mulsd", a, b, mx),
        "divsd" => host_op!("divsd", a, b, mx),
        "sqrtsd" => host_op!("sqrtsd", a, b, mx),
        "maxsd" => host_op!("maxsd", a, b, mx),
        "addpd" => host_op!("addpd", a, b, mx),
        "mulpd" => host_op!("mulpd", a, b, mx),
        "subpd" => host_op!("subpd", a, b, mx),
        "haddpd" => host_op!("haddpd", a, b, mx),
        "cvtsd2ss" => host_op!("cvtsd2ss", a, b, mx),
        "cvtpd2ps" => host_op!("cvtpd2ps", a, b, mx),
        "cvtss2sd" => host_op!("cvtss2sd", a, b, mx),
        "cvtps2pd" => host_op!("cvtps2pd", a, b, mx),
        "ucomiss" => host_op!("ucomiss", a, b, mx),
        "comisd" => host_op!("comisd", a, b, mx),
        "ucomisd" => host_op!("ucomisd", a, b, mx),
        "cmpltps" => host_op!("cmpltps", a, b, mx),
        "cmplesd" => host_op!("cmplesd", a, b, mx),
        _ => unreachable!("{name}"),
    }
}

const CASES: &[Case] = &[
    ("addss", &[0xf3, 0x0f, 0x58], true, false),
    ("subss", &[0xf3, 0x0f, 0x5c], true, false),
    ("mulss", &[0xf3, 0x0f, 0x59], true, false),
    ("divss", &[0xf3, 0x0f, 0x5e], true, false),
    ("sqrtss", &[0xf3, 0x0f, 0x51], true, false),
    ("minss", &[0xf3, 0x0f, 0x5d], true, false),
    ("maxss", &[0xf3, 0x0f, 0x5f], true, false),
    ("addps", &[0x0f, 0x58], true, false),
    ("mulps", &[0x0f, 0x59], true, false),
    ("divps", &[0x0f, 0x5e], true, false),
    ("sqrtps", &[0x0f, 0x51], true, false),
    ("minps", &[0x0f, 0x5d], true, false),
    ("haddps", &[0xf2, 0x0f, 0x7c], true, false),
    ("addsd", &[0xf2, 0x0f, 0x58], false, false),
    ("subsd", &[0xf2, 0x0f, 0x5c], false, false),
    ("mulsd", &[0xf2, 0x0f, 0x59], false, false),
    ("divsd", &[0xf2, 0x0f, 0x5e], false, false),
    ("sqrtsd", &[0xf2, 0x0f, 0x51], false, false),
    ("maxsd", &[0xf2, 0x0f, 0x5f], false, false),
    ("addpd", &[0x66, 0x0f, 0x58], false, false),
    ("mulpd", &[0x66, 0x0f, 0x59], false, false),
    ("subpd", &[0x66, 0x0f, 0x5c], false, false),
    ("haddpd", &[0x66, 0x0f, 0x7c], false, false),
    ("cvtsd2ss", &[0xf2, 0x0f, 0x5a], false, false),
    ("cvtpd2ps", &[0x66, 0x0f, 0x5a], false, false),
    ("cvtss2sd", &[0xf3, 0x0f, 0x5a], true, false),
    ("cvtps2pd", &[0x0f, 0x5a], true, false),
    ("ucomiss", &[0x0f, 0x2e], true, true),
    ("comisd", &[0x66, 0x0f, 0x2f], false, true),
    ("ucomisd", &[0x66, 0x0f, 0x2e], false, true),
];

/// (cmpps / cmpsd: the predicate is an immediate after the ModRM byte)
fn cmp_cases() -> [(&'static str, Vec<u8>, bool); 2] {
    [("cmpltps", vec![0x0f, 0xc2], true), ("cmplesd", vec![0xf2, 0x0f, 0xc2], false)]
}

/// The modes: DAZ, FTZ, both, with each rounding control; the reference without either too.
const MODES: [u32; 8] = [0x1fc0, 0x9f80, 0x9fc0, 0xbfc0, 0xdfc0, 0xffc0, 0x1f80, 0xdf80];

#[test]
fn daz_and_ftz_match_the_host() {
    let mut r = Rng(0x7061_7a7a_6c65_2101);
    let mut bad = Vec::new();
    for round in 0..4000 {
        let mx = MODES[round % MODES.len()];
        for &(name, op, single, fl) in CASES {
            let (a, b) = (vec(&mut r, single), vec(&mut r, single));
            let want = host(name, a, b, mx);
            let got = koji_run(op, a, b, mx);
            let got = if fl { (0, got.1, got.2) } else { (got.0, got.1, 0) };
            let want = if fl { (0, want.1, want.2) } else { (want.0, want.1, 0) };
            if got != want && bad.len() < 20 {
                bad.push(format!("{name} mxcsr {mx:04x} {a:032x} {b:032x}: koji {:032x} {:02x} {:02x}, cpu {:032x} {:02x} {:02x}", got.0, got.1, got.2, want.0, want.1, want.2));
            }
        }
        for (name, op, single) in cmp_cases() {
            let (a, b) = (vec(&mut r, single), vec(&mut r, single));
            let want = host(name, a, b, mx);
            // (the predicate: lt 1, le 2)
            let mut code = op.clone();
            code.push(0xc1);
            code.push(if name == "cmpltps" { 1 } else { 2 });
            let got = koji_run(&code[..code.len() - 1], a, b, mx);
            let _ = got;
            let got = koji_cmp(&code, a, b, mx);
            if (got.0, got.1) != (want.0, want.1) && bad.len() < 20 {
                bad.push(format!("{name} mxcsr {mx:04x} {a:032x} {b:032x}: koji {:032x} {:02x}, cpu {:032x} {:02x}", got.0, got.1, want.0, want.1));
            }
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// koji_run for an instruction with its ModRM and immediate given whole
fn koji_cmp(op: &[u8], a: u128, b: u128, mx: u32) -> (u128, u32, u32) {
    let mut code = vec![0x0f, 0x10, 0x05, 0, 0x20, 0, 0, 0x0f, 0x10, 0x0d, 0x10, 0x20, 0, 0];
    code.extend_from_slice(op);
    code.extend_from_slice(&[0x0f, 0x11, 0x05, 0x20, 0x20, 0, 0]);
    let mut m = Flat(vec![0; 0x10000]);
    m.0[0x1000..0x1000 + code.len()].copy_from_slice(&code);
    m.0[0x2000..0x2010].copy_from_slice(&a.to_le_bytes());
    m.0[0x2010..0x2020].copy_from_slice(&b.to_le_bytes());
    let mut cpu = Cpu::default();
    cpu.eip = 0x1000;
    cpu.regs[4] = 0x8000;
    cpu.eflags = 2;
    cpu.mxcsr = mx;
    cpu.trap_lo = 0xffff_0000;
    cpu.trap_hi = 0xffff_0001;
    while cpu.eip < 0x1000 + code.len() as u32 {
        assert_eq!(step(&mut cpu, &mut m), Exit::Continue);
    }
    (u128::from_le_bytes(m.0[0x2020..0x2030].try_into().unwrap()), cpu.mxcsr & 0x3f, 0)
}

/// The issue's cases (the Conformance diff job, run 42387122870), at MXCSR 0x9fc0.
#[test]
fn the_diff_jobs_cases() {
    let f = |op: &[u8], a: u128, b: u128| koji_run(op, a, b, 0x9fc0);
    assert_eq!(f(&[0xf3, 0x0f, 0x59], 1, 0x3f80_0000), (0, 0, 0x44 & 0), "mulss");
    assert_eq!(f(&[0xf3, 0x0f, 0x51], 0, 1).0 as u32, 0, "sqrtss");
    let (r, fl, _) = f(&[0xf2, 0x0f, 0x51], 0, 0x800f_ffff_ffff_ffff);
    assert_eq!((r as u64, fl), (0x8000_0000_0000_0000, 0), "sqrtsd");
    let (r, fl, _) = f(&[0xf2, 0x0f, 0x5c], 0, 0x800f_ffff_ffff_ffff);
    assert_eq!((r as u64, fl), (0, 0), "subsd");
    let (_, fl, ef) = f(&[0x66, 0x0f, 0x2e], 0x000f_d361_64c5_d7f2, 0x000f_ffff_ffff_ffff);
    assert_eq!((fl, ef), (0, 0x40), "ucomisd: equal");
}
