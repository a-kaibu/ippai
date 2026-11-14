//! koji's 80-bit arithmetic (crate::f80, the precise x87 mode) against Berkeley TestFloat 3e's
//! extF80 cases (tests/testfloat/build.sh: SoftFloat 3e, 8086-SSE): add, sub, mul, div, sqrt at
//! the three rounding precisions (TestFloat's -precision32/64/80 = the x87's PC 24/53/64 bits)
//! and four rounding modes, the conversions to and from f32/f64/i64, roundToInt and the
//! compares; then the same operations as x87 instructions in the interpreter with the precise
//! mode on (FLD m80, the arithmetic, FSTP m80 / m64 / m32, FISTP m64, FRNDINT, FCOMI), the
//! precision and rounding control in the control word. Values only: koji's x87 keeps no
//! exception flags. Cases with an operand the 387 rejects (an unnormal, pseudo-NaN or
//! pseudo-infinity: invalid on the x87, computed by SoftFloat) are left out.
//!
//! Without target/testfloat/testfloat_gen (or TF_GEN) this test does nothing. TF_LEVEL=2:
//! TestFloat's level 2 (much longer).
use iced_x86::code_asm::*;
use koji::f80::F80;
use koji::{Cpu, Exit, Fault, Memory};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn gen() -> Option<PathBuf> {
    let p = std::env::var("TF_GEN").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/testfloat/testfloat_gen")));
    p.exists().then_some(p)
}

const RC: [&str; 4] = ["-rnear_even", "-rmin", "-rmax", "-rminMag"];
/// precision control -> TestFloat's option
const PC: [(u32, &str); 3] = [(0, "-precision32"), (2, "-precision64"), (3, "-precision80")];

fn hex(s: &str) -> u128 {
    u128::from_str_radix(s, 16).unwrap()
}
fn x(v: u128) -> F80 {
    F80 { m: v as u64, se: (v >> 64) as u16 }
}
fn rejected(v: F80) -> bool {
    v.se & 0x7fff != 0 && v.m >> 63 == 0
}

/// The cases of a TestFloat function: operand/result tokens (hex), as many as `limit`.
fn cases(g: &PathBuf, f: &str, opts: &[&str], limit: usize, each: &mut dyn FnMut(&[u128])) -> usize {
    let level = std::env::var("TF_LEVEL").unwrap_or_else(|_| "1".into());
    let mut child = Command::new(g).args(["-level", &level]).args(opts).arg(f).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let out = BufReader::new(child.stdout.take().unwrap());
    let mut n = 0;
    for line in out.lines() {
        let line = line.unwrap();
        let t: Vec<u128> = line.split_whitespace().map(hex).collect();
        // (the last token: the exception flags)
        each(&t[..t.len() - 1]);
        n += 1;
        if n >= limit {
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    n
}

fn same(a: F80, b: F80) -> bool {
    a == b
}

/// The F80 functions themselves.
#[test]
fn extf80_against_testfloat() {
    let Some(g) = gen() else {
        eprintln!("no testfloat_gen (tests/testfloat/build.sh): skipped");
        return;
    };
    let mut total = 0;
    let mut bad = Vec::new();
    for rc in 0..4u32 {
        for (pc, popt) in PC {
            for f in ["extF80_add", "extF80_sub", "extF80_mul", "extF80_div", "extF80_sqrt"] {
                let mut fails = 0;
                let n = cases(&g, f, &[RC[rc as usize], popt], usize::MAX, &mut |t| {
                    let a = x(t[0]);
                    let (b, want) = if f == "extF80_sqrt" { (F80::default(), x(t[1])) } else { (x(t[1]), x(t[2])) };
                    if rejected(a) || rejected(b) {
                        return;
                    }
                    let got = match f {
                        "extF80_add" => a.add(b, pc, rc),
                        "extF80_sub" => a.sub(b, pc, rc),
                        "extF80_mul" => a.mul(b, pc, rc),
                        "extF80_div" => a.div(b, pc, rc),
                        _ => a.sqrt(pc, rc),
                    };
                    if !same(got, want) {
                        fails += 1;
                        if fails <= 3 {
                            bad.push(format!("{f} {} pc {pc}: {a:x?} {b:x?} want {want:x?} got {got:x?}", RC[rc as usize]));
                        }
                    }
                });
                total += n;
                if fails > 0 {
                    bad.push(format!("{f} {} pc {pc}: {fails} of {n} differ", RC[rc as usize]));
                }
            }
        }
        // conversions and rounding (no precision)
        for f in ["extF80_to_f64", "extF80_to_f32", "extF80_to_i64", "extF80_roundToInt", "f64_to_extF80", "f32_to_extF80", "i64_to_extF80"] {
            let mut fails = 0;
            let n = cases(&g, f, &[RC[rc as usize]], usize::MAX, &mut |t| {
                let ok = match f {
                    "extF80_to_f64" => rejected(x(t[0])) || {
                        let (g, w) = (x(t[0]).to_f64(rc).to_bits(), t[1] as u64);
                        g == w || (f64::from_bits(g).is_nan() && f64::from_bits(w).is_nan() && g == w)
                    },
                    "extF80_to_f32" => rejected(x(t[0])) || x(t[0]).to_f32(rc).to_bits() == t[1] as u32,
                    "extF80_to_i64" => rejected(x(t[0])) || {
                        let r = x(t[0]).to_int(rc).filter(|r| (i64::MIN as i128..=i64::MAX as i128).contains(r)).map_or(i64::MIN, |r| r as i64);
                        r as u64 == t[1] as u64
                    },
                    "extF80_roundToInt" => rejected(x(t[0])) || x(t[0]).round_int(rc) == x(t[1]),
                    "f64_to_extF80" => F80::from_f64(koji::fpu::quiet(f64::from_bits(t[0] as u64))) == x(t[1]),
                    "f32_to_extF80" => F80::from_f32(f32::from_bits(t[0] as u32)) == x(t[1]) || f32::from_bits(t[0] as u32).is_nan(),
                    _ => F80::from_i64(t[0] as u64 as i64) == x(t[1]),
                };
                if !ok {
                    fails += 1;
                    if fails <= 3 {
                        bad.push(format!("{f} {}: {:x?}", RC[rc as usize], t));
                    }
                }
            });
            total += n;
            if fails > 0 {
                bad.push(format!("{f} {}: {fails} of {n} differ", RC[rc as usize]));
            }
        }
    }
    for f in ["extF80_eq", "extF80_lt", "extF80_le"] {
        let mut fails = 0;
        let n = cases(&g, f, &[], usize::MAX, &mut |t| {
            let (a, b) = (x(t[0]), x(t[1]));
            if rejected(a) || rejected(b) {
                return;
            }
            use std::cmp::Ordering::*;
            let o = a.compare(b);
            let got = match f {
                "extF80_eq" => o == Some(Equal),
                "extF80_lt" => o == Some(Less),
                _ => matches!(o, Some(Less | Equal)),
            };
            if got != (t[2] != 0) {
                fails += 1;
                if fails <= 3 {
                    bad.push(format!("{f}: {a:x?} {b:x?} want {}", t[2]));
                }
            }
        });
        total += n;
        if fails > 0 {
            bad.push(format!("{f}: {fails} of {n} differ"));
        }
    }
    eprintln!("{total} TestFloat extF80 cases");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

struct Ram(Vec<u8>);
impl Memory for Ram {
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

const CODE: u32 = 0x1000;
const DATA: u32 = 0x10000;
const OUT: u32 = 0x10100;

/// Runs 32-bit code on the interpreter with the control word `cw`; the memory after.
fn run(code: &[u8], args: &[[u8; 10]], cw: u16) -> Ram {
    let mut mem = Ram(vec![0; 0x20000]);
    mem.0[CODE as usize..][..code.len()].copy_from_slice(code);
    for (i, a) in args.iter().enumerate() {
        mem.0[DATA as usize + 16 * i..][..10].copy_from_slice(a);
    }
    let mut cpu = Cpu { eip: CODE, ..Cpu::default() };
    cpu.regs[koji::ESI] = DATA;
    cpu.regs[koji::EDI] = OUT;
    cpu.regs[koji::ESP] = 0x8000;
    cpu.fpu.cw = cw;
    let end = CODE + code.len() as u32;
    let mut n = 0;
    while cpu.eip != end {
        assert_eq!(koji::step(&mut cpu, &mut mem), Exit::Continue);
        n += 1;
        assert!(n < 100);
    }
    mem
}

fn asm(f: impl Fn(&mut CodeAssembler) -> Result<(), IcedError>) -> Vec<u8> {
    let mut a = CodeAssembler::new(32).unwrap();
    f(&mut a).unwrap();
    a.assemble(CODE as u64).unwrap()
}

/// The x87 instructions in the precise mode: the cases of each operation through the
/// interpreter (a sample per mode: the F80 functions above take all of them).
#[test]
fn x87_instructions_in_the_precise_mode() {
    let Some(g) = gen() else { return };
    koji::fpu::X87_80.store(true, std::sync::atomic::Ordering::Relaxed);
    let bin = |f: fn(&mut CodeAssembler) -> Result<(), IcedError>| asm(move |a| {
        a.fld(tword_ptr(esi))?;
        a.fld(tword_ptr(esi + 16))?;
        f(a)?;
        a.fstp(tword_ptr(edi))
    });
    let progs: [(&str, Vec<u8>); 5] = [
        ("extF80_add", bin(|a| a.faddp(st1, st0))),
        ("extF80_sub", bin(|a| a.fsubp(st1, st0))),
        ("extF80_mul", bin(|a| a.fmulp(st1, st0))),
        ("extF80_div", bin(|a| a.fdivp(st1, st0))),
        ("extF80_sqrt", asm(|a| {
            a.fld(tword_ptr(esi))?;
            a.fsqrt()?;
            a.fstp(tword_ptr(edi))
        })),
    ];
    let mut bad = Vec::new();
    let mut total = 0;
    for rc in 0..4u32 {
        for (pc, popt) in PC {
            let cw = 0x037f & !0xf00 | (pc as u16) << 8 | (rc as u16) << 10;
            for (f, code) in &progs {
                let mut fails = 0;
                total += cases(&g, f, &[RC[rc as usize], popt], 4000, &mut |t| {
                    let a = x(t[0]);
                    let (b, want) = if *f == "extF80_sqrt" { (F80::default(), x(t[1])) } else { (x(t[1]), x(t[2])) };
                    if rejected(a) || rejected(b) {
                        return;
                    }
                    let m = run(code, &[a.to_bytes(), b.to_bytes()], cw);
                    let got = F80::from_bytes(m.0[OUT as usize..][..10].try_into().unwrap());
                    if got != want {
                        fails += 1;
                        if fails <= 3 {
                            bad.push(format!("{f} cw {cw:#x}: {a:x?} {b:x?} want {want:x?} got {got:x?}"));
                        }
                    }
                });
                if fails > 0 {
                    bad.push(format!("{f} cw {cw:#x}: {fails} differ"));
                }
            }
            // stores rounding by RC, FISTP, FRNDINT, FCOMI's flags (one program, several outputs)
            let code = asm(|a| {
                a.fld(tword_ptr(esi))?;
                a.fst(qword_ptr(edi + 16))?;
                a.fst(dword_ptr(edi + 24))?;
                a.fld(st0)?;
                a.frndint()?;
                a.fstp(tword_ptr(edi + 32))?;
                a.fld(tword_ptr(esi + 16))?;
                a.fcomip(st0, st1)?;
                a.pushfd()?;
                a.pop(eax)?;
                a.mov(dword_ptr(edi + 48), eax)?;
                a.fistp(qword_ptr(edi + 56))
            });
            let mut fails = 0;
            total += cases(&g, "extF80_lt", &[], 2000, &mut |t| {
                let (a, b) = (x(t[0]), x(t[1]));
                if rejected(a) || rejected(b) {
                    return;
                }
                let m = run(&code, &[a.to_bytes(), b.to_bytes()], cw);
                let o = |k: usize, n: usize| -> u64 { let mut v = [0u8; 8]; v[..n].copy_from_slice(&m.0[OUT as usize + k..][..n]); u64::from_le_bytes(v) };
                let f64w = a.to_f64(rc).to_bits();
                let f32w = a.to_f32(rc).to_bits() as u64;
                let rnd = a.round_int(rc).to_bytes();
                let i = a.to_int(rc).filter(|r| (i64::MIN as i128..=i64::MAX as i128).contains(r)).map_or(i64::MIN, |r| r as i64) as u64;
                // FCOMIP ST0(b), ST1(a): CF when b < a, ZF when equal, all three unordered
                use std::cmp::Ordering::*;
                let fl = match b.compare(a) { None => 0x45, Some(Less) => 1, Some(Equal) => 0x40, Some(Greater) => 0 };
                let got = (o(16, 8), o(24, 4), m.0[OUT as usize + 32..][..10].to_vec(), o(48, 4) & 0x45, o(56, 8));
                let want = (f64w, f32w, rnd.to_vec(), fl, i);
                // (NaN stores: the payload as the x87 converts it)
                if got != want && !a.is_nan() {
                    fails += 1;
                    if fails <= 3 {
                        bad.push(format!("stores cw {cw:#x}: {a:x?} {b:x?} want {want:x?} got {got:x?}"));
                    }
                }
            });
            if fails > 0 {
                bad.push(format!("stores cw {cw:#x}: {fails} differ"));
            }
        }
    }
    koji::fpu::X87_80.store(false, std::sync::atomic::Ordering::Relaxed);
    eprintln!("{total} cases through the interpreter");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}
