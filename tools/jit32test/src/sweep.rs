//! The rounding sweep: every FP instruction the JIT rounds itself, over operand pairs made to hit
//! its corner cases (ties at 24 and 53 bits, exact zero sums, products of odd significands,
//! range edges, specials), under every x87 precision/rounding control and every SSE rounding mode.
//! JIT against the interpreter (koji::round), which the native runs check against the CPU.
use super::*;

const PAIRS: u32 = 128;
const OUT: i32 = 0x800;

type Template = fn(&mut CodeAssembler) -> Result<(), IcedError>;

fn templates() -> Vec<(&'static str, bool, Template)> {
    let x = |s| (s, true);
    let v: Vec<((&str, bool), Template)> = vec![
        (x("fadd m64"), |a| { a.fld(qword_ptr(esi))?; a.fadd(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        (x("fsub m64"), |a| { a.fld(qword_ptr(esi))?; a.fsub(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        (x("fsubr m64"), |a| { a.fld(qword_ptr(esi))?; a.fsubr(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        (x("fmul m64"), |a| { a.fld(qword_ptr(esi))?; a.fmul(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        (x("fdiv m64"), |a| { a.fld(qword_ptr(esi))?; a.fdiv(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        (x("fdivr m64"), |a| { a.fld(qword_ptr(esi))?; a.fdivr(qword_ptr(esi + 8))?; a.fstp(qword_ptr(edi)) }),
        (x("faddp"), |a| { a.fld(qword_ptr(esi))?; a.fld(qword_ptr(esi + 8))?; a.faddp(st1, st0)?; a.fstp(qword_ptr(edi)) }),
        (x("fsubp"), |a| { a.fld(qword_ptr(esi))?; a.fld(qword_ptr(esi + 8))?; a.fsubp(st1, st0)?; a.fstp(qword_ptr(edi)) }),
        (x("fmulp"), |a| { a.fld(qword_ptr(esi))?; a.fld(qword_ptr(esi + 8))?; a.fmulp(st1, st0)?; a.fstp(qword_ptr(edi)) }),
        (x("fdivrp"), |a| { a.fld(qword_ptr(esi))?; a.fld(qword_ptr(esi + 8))?; a.fdivrp(st1, st0)?; a.fstp(qword_ptr(edi)) }),
        (x("fadd st0,st1"), |a| { a.fld(qword_ptr(esi + 8))?; a.fld(qword_ptr(esi))?; a.fadd_2(st0, st1)?; a.fstp(qword_ptr(edi))?; a.fstp(st0) }),
        (x("fmul st1,st0"), |a| { a.fld(qword_ptr(esi + 8))?; a.fld(qword_ptr(esi))?; a.fmul_2(st1, st0)?; a.fstp(st0)?; a.fstp(qword_ptr(edi)) }),
        (x("fadd m32"), |a| { a.fld(dword_ptr(esi))?; a.fadd(dword_ptr(esi + 4))?; a.fstp(qword_ptr(edi)) }),
        (x("fmul m32"), |a| { a.fld(dword_ptr(esi))?; a.fmul(dword_ptr(esi + 4))?; a.fstp(qword_ptr(edi)) }),
        // a load passed straight to a store: an SNaN comes out quiet (#65)
        (x("fld m64, fstp m64"), |a| { a.fld(qword_ptr(esi))?; a.fstp(qword_ptr(edi)) }),
        (x("fld m32, fstp m64"), |a| { a.fld(dword_ptr(esi))?; a.fstp(qword_ptr(edi)) }),
        (x("fld m32, fstp m32"), |a| { a.fld(dword_ptr(esi))?; a.fstp(dword_ptr(edi)) }),
        (x("fst m32"), |a| { a.fld(qword_ptr(esi))?; a.fst(dword_ptr(edi))?; a.fstp(dword_ptr(edi + 4)) }),
        (x("fadd, fst m32"), |a| { a.fld(qword_ptr(esi))?; a.fadd(qword_ptr(esi + 8))?; a.fstp(dword_ptr(edi)) }),
        // the mode changing inside the loop: FLDCW (translated) and LDMXCSR (the interpreter)
        (x("fldcw, fmul, fldcw, fadd"), |a| {
            a.fldcw(word_ptr(ebx + 0xfe0))?;
            a.fld(qword_ptr(esi))?;
            a.fmul(qword_ptr(esi + 8))?;
            a.fstp(dword_ptr(edi))?;
            a.fldcw(word_ptr(ebx + 0xfe2))?;
            a.fld(qword_ptr(esi))?;
            a.fadd(qword_ptr(esi + 8))?;
            a.fstp(dword_ptr(edi + 4))
        }),
        (("ldmxcsr, addss, ldmxcsr, mulss", false), |a| {
            a.ldmxcsr(dword_ptr(ebx + 0xfe4))?;
            a.movss(xmm0, dword_ptr(esi))?;
            a.addss(xmm0, dword_ptr(esi + 4))?;
            a.movss(dword_ptr(edi), xmm0)?;
            a.ldmxcsr(dword_ptr(ebx + 0xfe8))?;
            a.movss(xmm0, dword_ptr(esi))?;
            a.mulss(xmm0, dword_ptr(esi + 4))?;
            a.movss(dword_ptr(edi + 4), xmm0)
        }),
        (("addsd", false), |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.addsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        (("subsd", false), |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.subsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        (("mulsd", false), |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.mulsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        (("divsd", false), |a| { a.movsd_2(xmm0, qword_ptr(esi))?; a.divsd(xmm0, qword_ptr(esi + 8))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        (("sqrtsd", false), |a| { a.sqrtsd(xmm0, qword_ptr(esi))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        (("addsd reg", false), |a| { a.movsd_2(xmm1, qword_ptr(esi))?; a.movsd_2(xmm2, qword_ptr(esi + 8))?; a.addsd(xmm1, xmm2)?; a.movsd_2(qword_ptr(edi), xmm1) }),
        (("addss", false), |a| { a.movss(xmm0, dword_ptr(esi))?; a.addss(xmm0, dword_ptr(esi + 4))?; a.movss(dword_ptr(edi), xmm0) }),
        (("subss", false), |a| { a.movss(xmm0, dword_ptr(esi))?; a.subss(xmm0, dword_ptr(esi + 4))?; a.movss(dword_ptr(edi), xmm0) }),
        (("mulss", false), |a| { a.movss(xmm0, dword_ptr(esi))?; a.mulss(xmm0, dword_ptr(esi + 4))?; a.movss(dword_ptr(edi), xmm0) }),
        (("divss", false), |a| { a.movss(xmm0, dword_ptr(esi))?; a.divss(xmm0, dword_ptr(esi + 4))?; a.movss(dword_ptr(edi), xmm0) }),
        (("sqrtss", false), |a| { a.sqrtss(xmm0, dword_ptr(esi))?; a.movss(dword_ptr(edi), xmm0) }),
        (("cvtsd2ss", false), |a| { a.cvtsd2ss(xmm0, qword_ptr(esi))?; a.movss(dword_ptr(edi), xmm0) }),
        (("cvtsi2ss", false), |a| { a.cvtsi2ss(xmm0, dword_ptr(esi))?; a.movss(dword_ptr(edi), xmm0) }),
        (("cvtsi2sd", false), |a| { a.cvtsi2sd(xmm0, dword_ptr(esi))?; a.movsd_2(qword_ptr(edi), xmm0) }),
        (("cvtsd2si", false), |a| { a.cvtsd2si(eax, qword_ptr(esi))?; a.mov(dword_ptr(edi), eax) }),
        (("cvtss2si", false), |a| { a.cvtss2si(eax, dword_ptr(esi))?; a.mov(dword_ptr(edi), eax) }),
        (("addpd", false), |a| { a.movupd(xmm0, xmmword_ptr(esi))?; a.addpd(xmm0, xmmword_ptr(esi))?; a.movlpd(qword_ptr(edi), xmm0) }),
        (("mulps", false), |a| { a.movups(xmm0, xmmword_ptr(esi))?; a.mulps(xmm0, xmmword_ptr(esi))?; a.movlps(qword_ptr(edi), xmm0) }),
        (("cvtpd2ps", false), |a| { a.cvtpd2ps(xmm0, xmmword_ptr(esi))?; a.movlps(qword_ptr(edi), xmm0) }),
        (("cvtdq2ps", false), |a| { a.movdqu(xmm0, xmmword_ptr(esi))?; a.cvtdq2ps(xmm0, xmm0)?; a.movlps(qword_ptr(edi), xmm0) }),
    ];
    v.into_iter().map(|((n, x87), t)| (n, x87, t)).collect()
}

fn program(t: Template) -> Result<Vec<Instruction>, IcedError> {
    let mut a = CodeAssembler::new(32)?;
    let mut top = a.create_label();
    a.mov(esi, ebx)?;
    a.lea(edi, ptr(ebx + OUT))?;
    a.mov(dword_ptr(ebx + COUNTER as i32), PAIRS)?;
    a.set_label(&mut top)?;
    t(&mut a)?;
    a.add(esi, 16)?;
    a.add(edi, 8)?;
    a.dec(dword_ptr(ebx + COUNTER as i32))?;
    a.jne(top)?;
    a.jmp(END as u64)?;
    Ok(a.take_instructions())
}

fn sign(rng: &mut Rng) -> f64 {
    if rng.below(2) == 0 { 1.0 } else { -1.0 }
}

fn sign32(rng: &mut Rng) -> f32 {
    if rng.below(2) == 0 { 1.0 } else { -1.0 }
}

fn p2(e: i32) -> f64 {
    f64::powi(2.0, e)
}

/// A random significand of `bits` bits (top bit set), as an integer-valued f64
fn sig(rng: &mut Rng, bits: u32) -> f64 {
    (1u64 << (bits - 1) | rng.below(1 << (bits - 1))) as f64
}

/// One (x, y) pair as two little-endian qwords; f32 operands live in x's two halves
fn pair(rng: &mut Rng) -> (u64, u64) {
    let e = rng.below(80) as i32 - 40;
    let f = |v: f64| v.to_bits();
    let h = |a: f32, b: f32| a.to_bits() as u64 | (b.to_bits() as u64) << 32;
    match rng.below(16) {
        // ties and near-ties at 53 bits: a 53-bit x and a y around its last half ulp
        0 => {
            let x = sign(rng) * sig(rng, 53) * p2(e - 52);
            let k = rng.pick(&[1.0, 1.0, 3.0, 5.0, 0.75, 1.25]);
            (f(x), f(sign(rng) * k * p2(e - 53 - rng.below(2) as i32)))
        }
        // ...at 24 bits (x87 single precision): 24-bit x, y around its half ulp
        1 => {
            let x = sign(rng) * sig(rng, 24) * p2(e - 23);
            let k = rng.pick(&[1.0, 1.0, 3.0, 0.5, 1.5, 1.0 + p2(-20)]);
            (f(x), f(sign(rng) * k * p2(e - 24 - rng.below(2) as i32)))
        }
        // products of odd significands: 27x27 bits (54-bit products, ties at 53), 13x13 (ties at 24)
        2 => {
            let b = rng.pick(&[27, 26, 13, 12]);
            let (mut x, mut y) = (sig(rng, b), sig(rng, b));
            if x as u64 & 1 == 0 {
                x += 1.0;
            }
            if y as u64 & 1 == 0 {
                y += 1.0;
            }
            (f(sign(rng) * x * p2(e)), f(sign(rng) * y * p2(-e / 2)))
        }
        // exact zero sums: x + -x, x - x, ±0 with ±0
        3 => {
            let x = sign(rng) * sig(rng, 53) * p2(e - 52);
            match rng.below(4) {
                0 => (f(x), f(-x)),
                1 => (f(x), f(x)),
                2 => (f(sign(rng) * 0.0), f(sign(rng) * 0.0)),
                _ => (f(x), f(sign(rng) * 0.0)),
            }
        }
        // range edges: overflow, underflow, subnormals, the f32 range
        4 => {
            let big = [f64::MAX, p2(1023), p2(1000), p2(996), p2(995), p2(128), p2(127) * 1.9999999, f32::MAX as f64];
            let small = [f64::MIN_POSITIVE, p2(-1074), p2(-1060), p2(-1000), p2(-968), p2(-969), p2(-126), p2(-149), p2(-150), 1.5 * p2(-149)];
            let any = |r: &mut Rng| if r.below(2) == 0 { r.pick(&big) } else { r.pick(&small) } * (1.0 + r.below(1 << 20) as f64 * p2(-52 + r.below(33) as i32));
            let x = sign(rng) * any(rng);
            let y = match rng.below(3) {
                0 => sign(rng) * any(rng),
                1 => sign(rng) * rng.pick(&[0.5, 2.0, 1.0, 3.0, 0.75, 1.0 + p2(-52)]),
                _ => -x * (1.0 + sign(rng) * p2(-52)),
            };
            (f(x), f(y))
        }
        // specials against anything
        5 => {
            // (signaling NaNs too: quiet bit 0, a payload)
            let s = [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -f64::NAN, 0.0, -0.0, 1.0, p2(-1074), f64::from_bits(0x7ff4_f3d1_14af_58e4), f64::from_bits(0xfff0_0000_0000_0001)];
            let a = rng.pick(&s);
            let b = if rng.below(2) == 0 { rng.pick(&s) } else { sign(rng) * sig(rng, 53) * p2(e) };
            if rng.below(2) == 0 { (f(a), f(b)) } else { (f(b), f(a)) }
        }
        // f32 pairs: ties at 24 bits for the ss ops (x's halves), plus 53-bit qwords
        6 | 7 => {
            let e = e / 2;
            let a = sign32(rng) * (1u32 << 23 | rng.below(1 << 23) as u32) as f32 * f32::powi(2.0, e - 23);
            let b = match rng.below(4) {
                0 => sign32(rng) * f32::powi(2.0, e - 24 - rng.below(2) as i32),
                1 => sign32(rng) * 3.0 * f32::powi(2.0, e - 25),
                2 => -a,
                _ => sign32(rng) * (1 + rng.below(1 << 13)) as f32 * f32::powi(2.0, -(rng.below(20) as i32)),
            };
            (h(a, b), f(sign(rng) * sig(rng, 53) * p2(e - 52)))
        }
        // f32 range edges
        8 => {
            let edge = [f32::MAX, f32::MIN_POSITIVE, f32::from_bits(1), f32::from_bits(0x7f_ffff), f32::powi(2.0, 127), 1.0, f32::INFINITY, f32::NAN, f32::from_bits(0x7fa0_0001)];
            let a = sign32(rng) * rng.pick(&edge);
            let b = sign32(rng) * rng.pick(&edge) * rng.pick(&[1.0, 0.5, 2.0, 1.5]);
            (h(a, b), f(sign(rng) * rng.pick(&[f32::MAX as f64 * (1.0 + p2(-25)), f32::MAX as f64 * (1.0 + p2(-24)), p2(-150), p2(-150) * 1.0000001, p2(-126) * (1.0 - p2(-25)), p2(128)])))
        }
        // doubles that round to f32 ties (cvtsd2ss, fst m32): 25-bit odd significands, and just off them
        9 => {
            let x = sign(rng) * (sig(rng, 25) as u64 | 1) as f64 * p2(e - 24);
            let d = rng.pick(&[0.0, 0.0, p2(-40), -p2(-40)]);
            (f(x * (1.0 + d)), f(x))
        }
        // integers for cvtsi2ss/sd: above 2^24 with ties, and extremes
        10 => {
            let i: i32 = match rng.below(4) {
                0 => (1 << 24 | rng.below(1 << 24) as i32) * if rng.below(2) == 0 { 1 } else { -1 },
                1 => rng.pick(&[i32::MIN, i32::MAX, 0x0100_0001, 0x0100_0003, -0x0100_0001, 0x7fff_ffc0, 0x7fff_ff80]),
                _ => rng.next() as i32,
            };
            (i as u32 as u64 | (rng.next() as u32 as u64) << 32, f(sign(rng) * (rng.below(1 << 31) as f64 + rng.pick(&[0.5, 0.25, 0.75, 0.0]))))
        }
        // random full-precision doubles, and random bits
        11 | 12 | 13 => (f(sign(rng) * sig(rng, 53) * p2(e - 52)), f(sign(rng) * sig(rng, 53) * p2(rng.below(80) as i32 - 92))),
        _ => (rng.next(), rng.next()),
    }
}

fn data(rng: &mut Rng) -> Vec<u8> {
    let mut d = random_data(rng);
    for k in 0..PAIRS as usize {
        let (x, y) = pair(rng);
        d[k * 16..k * 16 + 8].copy_from_slice(&x.to_le_bytes());
        d[k * 16 + 8..k * 16 + 16].copy_from_slice(&y.to_le_bytes());
    }
    d
}

/// Runs the sweep; returns the failures
pub fn run(engine: &Engine, dispatch: &Module, memory: &SharedMemory, base: usize, seeds: u64) -> usize {
    let mut fails = 0;
    let mut runs = 0;
    for (name, x87, t) in templates() {
        let list = program(t).unwrap();
        let modes: Vec<(u16, u32)> = if x87 {
            (0..12).map(|k| ([0x007f, 0x027f, 0x037f][k / 4] | (k as u16 % 4) << 10, 0x1f80)).collect()
        } else {
            (0..4).map(|k| (0x037f, 0x1f80 | k << 13)).collect()
        };
        for (cw, mxcsr) in modes {
            for s in 0..seeds {
                let mut rng = Rng((s + 1).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ (cw as u64) << 20 ^ (mxcsr as u64) << 40 | 1);
                let mut d = data(&mut rng);
                // (the other mode the mode-changing templates switch to, then back)
                let other = rng.below(12) as u16;
                d[0xfe0..0xfe2].copy_from_slice(&([0x007f, 0x027f, 0x037f][other as usize / 4] | (other % 4) << 10).to_le_bytes());
                d[0xfe2..0xfe4].copy_from_slice(&cw.to_le_bytes());
                d[0xfe4..0xfe8].copy_from_slice(&(0x1f80u32 | (other as u32 % 4) << 13).to_le_bytes());
                d[0xfe8..0xfec].copy_from_slice(&mxcsr.to_le_bytes());
                let mut cpu0 = initial(&mut rng);
                cpu0.fpu.cw = cw;
                cpu0.mxcsr = mxcsr;
                let mode = Mode { fns: s % 2 == 1, blocks: 1 + s as usize % 3, hoist: true, slots: 0, pre: false, exact: s % 4 != 3, fresh: false, stale: 0 };
                runs += 1;
                let (diffs, _) = run_case(engine, dispatch, memory, base, &list, &d, &cpu0, mode);
                if !diffs.is_empty() {
                    fails += 1;
                    if fails <= 20 {
                        println!("sweep {name} cw {cw:#x} mxcsr {mxcsr:#x} seed {s} {mode:?}: {}", diffs.join("; "));
                        if let Some(i) = diffs.iter().find_map(|d| d.strip_prefix("data differs at +0x")).and_then(|h| usize::from_str_radix(h, 16).ok()) {
                            if i >= OUT as usize {
                                let k = (i - OUT as usize) / 8;
                                let q = |o: usize| u64::from_le_bytes(d[o..o + 8].try_into().unwrap());
                                println!("    pair {k}: x {:016x} ({:e}) y {:016x} ({:e})", q(k * 16), f64::from_bits(q(k * 16)), q(k * 16 + 8), f64::from_bits(q(k * 16 + 8)));
                            }
                        }
                    }
                }
            }
        }
    }
    println!("sweep: {runs} runs x {PAIRS} pairs, {fails} failed");
    fails
}
