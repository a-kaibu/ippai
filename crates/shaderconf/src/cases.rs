//! The cases, and the references they are checked against.

use super::{Case, Tol, Want};

// ---------------------------------------------------------------- inputs

/// Floats that exercise the rules: signed zeros, halves (rounding), magnitudes near the integer
/// limits, infinities, NaN. (No denormals: Direct3D allows flushing them.)
const F: &[f32] = &[
    0.0, -0.0, 1.0, -1.0, 0.5, -0.5, 1.5, -1.5, 2.5, -2.5, 3.7, -3.7, 0.1, 7.0, 100.0, -100.0, 1e-3, 1e20,
    -1e20, 16777216.0, 2147483648.0, -2147483904.0, 4294967296.0, 3.4e38, f32::INFINITY, f32::NEG_INFINITY, f32::NAN,
];
/// A smaller set for pairs.
const F2: &[f32] = &[0.0, -0.0, 1.0, -1.0, 0.5, 2.5, -3.7, 100.0, 1e20, f32::INFINITY, f32::NEG_INFINITY, f32::NAN];
/// Integers: zero, small, the shift edges, the sign bit, all ones.
const U: &[u32] = &[0, 1, 2, 3, 7, 31, 32, 33, 255, 0x8000, 0xffff, 0x10000, 12345678, 0x7fffffff, 0x80000000, 0x80000001, 0xfffffffe, 0xffffffff];

fn fpairs() -> Vec<(f32, f32)> {
    F2.iter().flat_map(|&a| F2.iter().map(move |&b| (a, b))).collect()
}
fn upairs() -> Vec<(u32, u32)> {
    U.iter().flat_map(|&a| U.iter().map(move |&b| (a, b))).collect()
}

fn one(v: f32) -> Want {
    Some(vec![v])
}

// ---------------------------------------------------------------- Direct3D 10/11 (DXBC)

/// What a DXBC case's `r` is and how it is written out.
#[derive(Clone, Copy)]
enum Out {
    /// float r -> (r, 0, 0, 1)
    F,
    /// uint r -> (r >> 16, r & 0xffff, 0, 1): exact in floats
    U,
    /// float4 r
    F4,
}

const DXBC_HEAD: &str = "cbuffer C : register(b0) { uint4 A; uint4 B; uint4 Cc; };
float4 ps() : SV_Target {
  uint4 a = A; uint4 b = B; uint4 c = Cc;
  float4 fa = asfloat(a); float4 fb = asfloat(b); float4 fc = asfloat(c);
  int4 ia = asint(a); int4 ib = asint(b); int4 ic = asint(c);
";

fn dxbc_wgsl(src: &str, target: &str) -> Result<(String, u32, u32, Vec<u8>), String> {
    let mut inc = |_: &str, _: bool| None;
    let code = hlslc::compile(src, &[], &mut inc, "ps", target, 0)?;
    let w = dxwgsl::translate(&code, true, None)?;
    let b = w.bindings.iter().find(|b| matches!(b.1, dxwgsl::Binding::Uniform(_))).map_or(0, |b| b.0);
    Ok((w.code, 1, b, code))
}

/// A DXBC case: `body` sets `r` from a, b, c (uint4), fa.. (float4), ia.. (int4); one pixel
/// per input (12 words: A, B, Cc), `want` its expected channels.
#[allow(clippy::too_many_arguments)]
fn dxbc(
    out: &mut Vec<Case>,
    errors: &mut Vec<String>,
    name: &str,
    rule: &'static str,
    target: &str,
    body: &str,
    kind: Out,
    tol: Tol,
    pixels: Vec<([u32; 12], [Want; 4])>,
) {
    let ret = match kind {
        Out::F => "  return float4(r, 0, 0, 1);\n}\n",
        Out::U => "  return float4(r >> 16, r & 0xffff, 0, 1);\n}\n",
        Out::F4 => "  return r;\n}\n",
    };
    let src = format!("{DXBC_HEAD}  {body}\n{ret}");
    dxbc_raw(out, errors, name, rule, &src, target, tol, pixels.into_iter().map(|(d, w)| (d.to_vec(), w)).collect());
}

/// A DXBC case from whole HLSL source (entry `ps`), the constant buffer's words per pixel.
#[allow(clippy::too_many_arguments)]
pub fn dxbc_raw(out: &mut Vec<Case>, errors: &mut Vec<String>, name: &str, rule: &'static str, src: &str, target: &str, tol: Tol, pixels: Vec<(Vec<u32>, [Want; 4])>) {
    match dxbc_wgsl(src, target) {
        Ok((wgsl, group, binding, code)) => out.push(Case {
            name: format!("dxbc/{name}"),
            api: "dxbc",
            src: src.to_string(),
            code,
            wgsl,
            group,
            binding,
            pixels: pixels.into_iter().map(|(d, w)| (d.iter().flat_map(|x| x.to_le_bytes()).collect(), w)).collect(),
            tol,
            rule,
        }),
        Err(e) => errors.push(format!("dxbc/{name}: {e}\n{src}")),
    }
}

fn wf(v: Vec<f32>) -> [Want; 4] {
    [Some(v), one(0.0), one(0.0), one(1.0)]
}
fn wu(v: &[u32]) -> [Want; 4] {
    [Some(v.iter().map(|x| (x >> 16) as f32).collect()), Some(v.iter().map(|x| (x & 0xffff) as f32).collect()), one(0.0), one(1.0)]
}
fn f3(a: f32, b: f32, c: f32) -> [u32; 12] {
    [a.to_bits(), 0, 0, 0, b.to_bits(), 0, 0, 0, c.to_bits(), 0, 0, 0]
}
fn u3(a: u32, b: u32, c: u32) -> [u32; 12] {
    [a, 0, 0, 0, b, 0, 0, 0, c, 0, 0, 0]
}

/// D3D11 min/max: a NaN operand gives the other one (both NaN: NaN); +0 and -0 either.
fn d3d_min(a: f32, b: f32, max: bool) -> Vec<f32> {
    if a.is_nan() {
        return vec![b];
    }
    if b.is_nan() {
        return vec![a];
    }
    if a == 0.0 && b == 0.0 {
        return vec![a, b];
    }
    vec![if (a < b) != max { a } else { b }]
}

/// float to int (D3D11 ftoi): truncation, NaN 0, clamped to the int range
fn ftoi(f: f32) -> u32 {
    if f.is_nan() {
        0
    } else {
        (f as f64).trunc().clamp(i32::MIN as f64, i32::MAX as f64) as i32 as u32
    }
}
/// ftou: NaN and negatives 0, clamped
fn ftou(f: f32) -> u32 {
    if f.is_nan() {
        0
    } else {
        (f as f64).trunc().clamp(0.0, u32::MAX as f64) as u32
    }
}

fn f16_rtz(v: f32) -> u16 {
    // (round toward zero: the conversion D3D also allows for f32tof16, WARP's, which gives
    // every NaN as 0x7fff)
    if v.is_nan() {
        return (v.to_bits() >> 16) as u16 & 0x8000 | 0x7fff;
    }
    let h = half::f16::from_f32(v);
    if v.is_infinite() || (h.to_f32().abs() <= v.abs()) {
        return h.to_bits();
    }
    let b = h.to_bits();
    if b & 0x7fff == 0 { b } else { b - 1 }
}

fn dxbc_cases(out: &mut Vec<Case>, errors: &mut Vec<String>) {
    let fp = fpairs();
    let bin = |f: fn(f32, f32) -> f32| -> Vec<([u32; 12], [Want; 4])> { fp.iter().map(|&(a, b)| (f3(a, b, 0.0), wf(vec![f(a, b)]))).collect() };
    dxbc(out, errors, "add", "IEEE add, correctly rounded; inf - inf = NaN", "ps_4_0", "float r = fa.x + fb.x;", Out::F, Tol::Exact, bin(|a, b| a + b));
    dxbc(out, errors, "sub", "IEEE subtract", "ps_4_0", "float r = fa.x - fb.x;", Out::F, Tol::Exact, bin(|a, b| a - b));
    dxbc(out, errors, "mul", "IEEE multiply; 0 * inf = NaN", "ps_4_0", "float r = fa.x * fb.x;", Out::F, Tol::Exact, bin(|a, b| a * b));
    dxbc(out, errors, "div", "division within 2.5 ULP; x / 0 = +-inf, 0 / 0 = NaN", "ps_4_0", "float r = fa.x / fb.x;", Out::F, Tol::Ulp(3), bin(|a, b| a / b));
    let mad: Vec<_> = F2
        .iter()
        .flat_map(|&a| F2.iter().map(move |&b| (a, b)))
        .flat_map(|(a, b)| [0.5f32, -1.0, f32::INFINITY].map(|c| (f3(a, b, c), wf(vec![a * b + c, a.mul_add(b, c)]))))
        .collect();
    dxbc(out, errors, "mad", "mad: fused or not (either rounding)", "ps_4_0", "float r = mad(fa.x, fb.x, fc.x);", Out::F, Tol::Exact, mad);
    for (n, max) in [("min", false), ("max", true)] {
        let px = fp.iter().map(|&(a, b)| (f3(a, b, 0.0), wf(d3d_min(a, b, max)))).collect();
        let body = if max { "float r = max(fa.x, fb.x);" } else { "float r = min(fa.x, fb.x);" };
        dxbc(out, errors, n, "min/max: a NaN operand returns the other", "ps_4_0", body, Out::F, Tol::Exact, px);
    }
    let un = |f: fn(f32) -> Vec<f32>| -> Vec<([u32; 12], [Want; 4])> { F.iter().map(|&a| (f3(a, 0.0, 0.0), wf(f(a)))).collect() };
    dxbc(out, errors, "sqrt", "sqrt; sqrt(-x) = NaN, sqrt(-0) = -0", "ps_4_0", "float r = sqrt(fa.x);", Out::F, Tol::Ulp(3), un(|a| vec![a.sqrt()]));
    dxbc(out, errors, "rsqrt", "rsq within 2 ULP; rsq(+-0) = +-inf", "ps_4_0", "float r = rsqrt(fa.x);", Out::F, Tol::Ulp(3), un(|a| vec![1.0 / a.sqrt()]));
    dxbc(out, errors, "rcp", "rcp (shader model 5) within 1 ULP; rcp(+-0) = +-inf", "ps_5_0", "float r = rcp(fa.x);", Out::F, Tol::Ulp(1), un(|a| vec![1.0 / a]));
    // (frc(+-inf or NaN) is NaN; WGSL's fract clamps those to 1 - 2^-24 on some implementations:
    // accepted, no game relies on it)
    dxbc(out, errors, "frac", "frc: x - floor(x)", "ps_4_0", "float r = frac(fa.x);", Out::F, Tol::Exact, un(|a| if a.is_finite() { vec![a - a.floor()] } else { vec![f32::NAN, 1.0 - f32::EPSILON / 2.0] }));
    dxbc(out, errors, "floor", "round_ni", "ps_4_0", "float r = floor(fa.x);", Out::F, Tol::Exact, un(|a| vec![a.floor()]));
    dxbc(out, errors, "ceil", "round_pi", "ps_4_0", "float r = ceil(fa.x);", Out::F, Tol::Exact, un(|a| vec![a.ceil()]));
    dxbc(out, errors, "trunc", "round_z", "ps_4_0", "float r = trunc(fa.x);", Out::F, Tol::Exact, un(|a| vec![a.trunc()]));
    dxbc(out, errors, "round", "round_ne: halves to even", "ps_4_0", "float r = round(fa.x);", Out::F, Tol::Exact, un(|a| vec![a.round_ties_even()]));
    dxbc(out, errors, "saturate", "_sat: clamp to [0, 1], NaN to 0", "ps_4_0", "float r = saturate(fa.x);", Out::F, Tol::Exact, un(|a| vec![if a.is_nan() { 0.0 } else { a.clamp(0.0, 1.0) }]));
    dxbc(out, errors, "abs_neg", "source modifiers: -|x|", "ps_4_0", "float r = -abs(fa.x);", Out::F, Tol::Exact, un(|a| vec![-a.abs()]));
    let small: Vec<f32> = F.iter().copied().filter(|a| a.abs() <= 100.0 || !a.is_finite()).collect();
    // (exp2(NaN): NaN, or what the WebGPU implementation gives: WGSL leaves it open)
    let exp2 = small.iter().map(|&a| (f3(a, 0.0, 0.0), wf(if a.is_nan() { vec![f32::NAN, f32::INFINITY] } else { vec![a.exp2()] }))).collect();
    dxbc(out, errors, "exp2", "exp: 2^x within 3 ULP (2^-21 absolute near 0)", "ps_4_0", "float r = exp2(fa.x);", Out::F, Tol::Rel(4e-7, 5e-7), exp2);
    let log2 = F.iter().map(|&a| (f3(a, 0.0, 0.0), wf(vec![a.log2()]))).collect();
    dxbc(out, errors, "log2", "log: log2(x) within 2^-21 absolute in [0.5, 2], 3 ULP elsewhere; log(0) = -inf, log(-x) = NaN", "ps_4_0", "float r = log2(fa.x);", Out::F, Tol::Rel(4e-7, 5e-7), log2);
    let trig: Vec<f32> = small.iter().copied().filter(|a| a.is_finite()).collect();
    let sin = trig.iter().map(|&a| (f3(a, 0.0, 0.0), wf(vec![a.sin()]))).collect();
    let cos = trig.iter().map(|&a| (f3(a, 0.0, 0.0), wf(vec![a.cos()]))).collect();
    dxbc(out, errors, "sin", "sincos: within 0.0008 for |x| <= 100 pi", "ps_4_0", "float r = sin(fa.x);", Out::F, Tol::Abs(0.0008), sin);
    dxbc(out, errors, "cos", "sincos: within 0.0008 for |x| <= 100 pi", "ps_4_0", "float r = cos(fa.x);", Out::F, Tol::Abs(0.0008), cos);
    // comparisons: ordered, NaN unequal to everything
    for (n, op, f) in [
        ("lt", "<", (|a: f32, b: f32| a < b) as fn(f32, f32) -> bool),
        ("ge", ">=", |a, b| a >= b),
        ("eq", "==", |a, b| a == b),
        ("ne", "!=", |a, b| a != b),
    ] {
        let px = fp.iter().map(|&(a, b)| (f3(a, b, 0.0), wu(&[f(a, b) as u32]))).collect();
        dxbc(out, errors, &format!("cmp_{n}"), "float comparisons are ordered (NaN: false, != true); -0 == +0", "ps_4_0", &format!("uint r = (fa.x {op} fb.x) ? 1 : 0;"), Out::U, Tol::Exact, px);
    }
    dxbc(out, errors, "dot4", "dp4", "ps_4_0", "float4 r = dot(fa, fb).xxxx;", Out::F4, Tol::Ulp(2), {
        let v = [[1.0f32, 2.0, 3.0, 4.0], [0.5, -0.25, 8.0, 1e3], [-1.0, 0.0, 0.0, 2.0]];
        v.iter().flat_map(|a| v.iter().map(move |b| {
            let d = a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
            let mut w = [0u32; 12];
            for i in 0..4 {
                w[i] = a[i].to_bits();
                w[4 + i] = b[i].to_bits();
            }
            (w, [one(d), one(d), one(d), one(d)])
        })).collect()
    });
    // conversions
    let conv = |f: fn(f32) -> u32| -> Vec<([u32; 12], [Want; 4])> { F.iter().map(|&a| (f3(a, 0.0, 0.0), wu(&[f(a)]))).collect() };
    dxbc(out, errors, "ftoi", "ftoi: toward zero, NaN to 0, clamped to the int range", "ps_4_0", "uint r = asuint((int)fa.x);", Out::U, Tol::Exact, conv(ftoi));
    dxbc(out, errors, "ftou", "ftou: toward zero, NaN and negatives to 0, clamped", "ps_4_0", "uint r = (uint)fa.x;", Out::U, Tol::Exact, conv(ftou));
    let itof = U.iter().map(|&a| (u3(a, 0, 0), wf(vec![a as i32 as f32]))).collect();
    let utof = U.iter().map(|&a| (u3(a, 0, 0), wf(vec![a as f32]))).collect();
    dxbc(out, errors, "itof", "itof: round to nearest even", "ps_4_0", "float r = (float)ia.x;", Out::F, Tol::Exact, itof);
    dxbc(out, errors, "utof", "utof: round to nearest even", "ps_4_0", "float r = (float)a.x;", Out::F, Tol::Exact, utof);
    let f2h = F.iter().map(|&a| (f3(a, 0.0, 0.0), wu(&[half::f16::from_f32(a).to_bits() as u32, f16_rtz(a) as u32]))).collect();
    dxbc(out, errors, "f32tof16", "f32tof16: binary16, round to nearest even or toward zero; NaN stays NaN", "ps_5_0", "uint r = f32tof16(fa.x);", Out::U, Tol::Exact, f2h);
    let halves: [u32; 10] = [0, 0x8000, 0x3c00, 0xbc00, 0x3555, 0x7bff, 0x0001, 0x03ff, 0x7c00, 0xfc00];
    let h2f = halves.iter().map(|&h| (u3(h, 0, 0), wf(vec![half::f16::from_bits(h as u16).to_f32()]))).collect();
    dxbc(out, errors, "f16tof32", "f16tof32: exact, binary16 denormals kept", "ps_5_0", "float r = f16tof32(a.x);", Out::F, Tol::Exact, h2f);
    // integers
    let up = upairs();
    let ibin = |f: fn(u32, u32) -> u32| -> Vec<([u32; 12], [Want; 4])> { up.iter().map(|&(a, b)| (u3(a, b, 0), wu(&[f(a, b)]))).collect() };
    dxbc(out, errors, "iadd", "iadd wraps", "ps_4_0", "uint r = a.x + b.x;", Out::U, Tol::Exact, ibin(|a, b| a.wrapping_add(b)));
    dxbc(out, errors, "isub", "ineg + iadd wraps", "ps_4_0", "uint r = a.x - b.x;", Out::U, Tol::Exact, ibin(|a, b| a.wrapping_sub(b)));
    dxbc(out, errors, "imul", "imul: the low 32 bits", "ps_4_0", "uint r = a.x * b.x;", Out::U, Tol::Exact, ibin(|a, b| a.wrapping_mul(b)));
    dxbc(out, errors, "udiv", "udiv: x / 0 = 0xffffffff", "ps_4_0", "uint r = a.x / b.x;", Out::U, Tol::Exact, ibin(|a, b| a.checked_div(b).unwrap_or(u32::MAX)));
    dxbc(out, errors, "urem", "udiv: x % 0 = 0xffffffff", "ps_4_0", "uint r = a.x % b.x;", Out::U, Tol::Exact, ibin(|a, b| a.checked_rem(b).unwrap_or(u32::MAX)));
    let sdiv: Vec<_> = up.iter().filter(|&&(a, b)| b != 0 && !(a == 0x80000000 && b == u32::MAX)).collect();
    let px = sdiv.iter().map(|&&(a, b)| (u3(a, b, 0), wu(&[(a as i32 / b as i32) as u32]))).collect();
    dxbc(out, errors, "idiv", "signed / truncates toward zero (C rules, HLSL)", "ps_4_0", "uint r = asuint(ia.x / ib.x);", Out::U, Tol::Exact, px);
    let px = sdiv.iter().map(|&&(a, b)| (u3(a, b, 0), wu(&[(a as i32 % b as i32) as u32]))).collect();
    dxbc(out, errors, "irem", "signed %: the sign of the dividend", "ps_4_0", "uint r = asuint(ia.x % ib.x);", Out::U, Tol::Exact, px);
    dxbc(out, errors, "ishl", "ishl: only the low 5 bits of the count", "ps_4_0", "uint r = a.x << b.x;", Out::U, Tol::Exact, ibin(|a, b| a << (b & 31)));
    dxbc(out, errors, "ushr", "ushr: only the low 5 bits of the count", "ps_4_0", "uint r = a.x >> b.x;", Out::U, Tol::Exact, ibin(|a, b| a >> (b & 31)));
    dxbc(out, errors, "ishr", "ishr: arithmetic, the low 5 bits of the count", "ps_4_0", "uint r = asuint(ia.x >> ib.x);", Out::U, Tol::Exact, ibin(|a, b| ((a as i32) >> (b & 31)) as u32));
    dxbc(out, errors, "and_or_xor", "and, or, xor, not", "ps_4_0", "uint r = ((a.x & b.x) | ~(a.x ^ b.x)) ^ 0x5a5a5a5a;", Out::U, Tol::Exact, ibin(|a, b| ((a & b) | !(a ^ b)) ^ 0x5a5a5a5a));
    dxbc(out, errors, "umin", "umin", "ps_4_0", "uint r = min(a.x, b.x);", Out::U, Tol::Exact, ibin(|a, b| a.min(b)));
    dxbc(out, errors, "imax", "imax", "ps_4_0", "uint r = asuint(max(ia.x, ib.x));", Out::U, Tol::Exact, ibin(|a, b| (a as i32).max(b as i32) as u32));
    let ib1 = |f: fn(u32) -> u32| -> Vec<([u32; 12], [Want; 4])> { U.iter().map(|&a| (u3(a, 0, 0), wu(&[f(a)]))).collect() };
    dxbc(out, errors, "countbits", "countbits", "ps_5_0", "uint r = countbits(a.x);", Out::U, Tol::Exact, ib1(|a| a.count_ones()));
    dxbc(out, errors, "reversebits", "bfrev", "ps_5_0", "uint r = reversebits(a.x);", Out::U, Tol::Exact, ib1(|a| a.reverse_bits()));
    dxbc(out, errors, "firstbitlow", "firstbit_lo: index of the lowest set bit, 0 -> 0xffffffff", "ps_5_0", "uint r = firstbitlow(a.x);", Out::U, Tol::Exact, ib1(|a| if a == 0 { u32::MAX } else { a.trailing_zeros() }));
    dxbc(out, errors, "firstbithigh_u", "firstbithigh(uint): index from bit 0 of the highest set bit, 0 -> 0xffffffff", "ps_5_0", "uint r = firstbithigh(a.x);", Out::U, Tol::Exact, ib1(|a| if a == 0 { u32::MAX } else { 31 - a.leading_zeros() }));
    dxbc(out, errors, "firstbithigh_i", "firstbithigh(int): for negatives the highest 0 bit; 0 and -1 -> 0xffffffff", "ps_5_0", "uint r = asuint(firstbithigh(ia.x));", Out::U, Tol::Exact, ib1(|a| {
        let v = if (a as i32) < 0 { !a } else { a };
        if v == 0 { u32::MAX } else { 31 - v.leading_zeros() }
    }));
    dxbc(out, errors, "abs_int", "iabs (ineg of INT_MIN wraps)", "ps_4_0", "uint r = asuint(abs(ia.x));", Out::U, Tol::Exact, ib1(|a| (a as i32).wrapping_abs() as u32));
    dxbc(out, errors, "movc", "movc: any nonzero bits select the first", "ps_4_0", "uint r = a.x ? b.x : c.x;", Out::U, Tol::Exact, U.iter().map(|&a| (u3(a, 0x1234, 0xabcd), wu(&[if a != 0 { 0x1234 } else { 0xabcd }]))).collect());
    // dynamic indexing of a constant buffer array, loops
    let src = "cbuffer C : register(b0) { uint4 A; uint4 B; uint4 Cc; };
cbuffer T : register(b1) { float4 tab[4]; };
float4 ps() : SV_Target {
  float s = 0;
  [loop] for (uint i = 0; i < A.x; i++) { s += asfloat(B.x) * (float)(i + 1); if (i == A.y) break; }
  return float4(s, 0, 0, 1);
}
";
    let _ = src; // (two buffers: the runner binds one; loops are below with one buffer)
    let loops = [(0u32, 99u32), (1, 99), (5, 99), (5, 2), (200, 99)]
        .iter()
        .map(|&(n, brk)| {
            let mut s = 0.0f32;
            for i in 0..n {
                s += 0.5 * (i + 1) as f32;
                if i == brk {
                    break;
                }
            }
            ([n, brk, 0, 0, 0.5f32.to_bits(), 0, 0, 0, 0, 0, 0, 0], wf(vec![s]))
        })
        .collect();
    dxbc(out, errors, "loop_break", "loop with a run-time count and break", "ps_4_0",
        "float r = 0; [loop] for (uint i = 0; i < a.x; i++) { r += fb.x * (float)(i + 1); if (i == a.y) break; }",
        Out::F, Tol::Exact, loops);
    let sw = [[1.0f32, 2.0, 3.0, 4.0]]
        .iter()
        .map(|v| {
            let mut w = [0u32; 12];
            for i in 0..4 {
                w[i] = v[i].to_bits();
            }
            (w, [one(4.0), one(3.0), one(1.0), one(1.0)])
        })
        .collect();
    dxbc(out, errors, "swizzle", "source swizzles and write masks", "ps_4_0", "float4 r = 0; r.yx = fa.zw; r.zw = fa.xx;", Out::F4, Tol::Exact, sw);
    packing(out, errors);
    // local arrays written at a run-time index (indexable temps, or a select per element)
    for (n, len) in [("dyn_store4", 4), ("dyn_store8", 8)] {
        let src = format!("cbuffer C : register(b0) {{ float4 A; float4 B; float4 Cc; }};
float4 ps() : SV_Target {{
  float4 arr[{len}];
  for (int k = 0; k < {len}; k++) arr[k] = float4(k, 0, 0, 1);
  arr[(int)A.x] = B;
  arr[(int)A.y].y += 5;
  return float4(arr[2].x, arr[2].y, arr[(int)A.z].x, arr[1].y);
}}
");
        let px = [(0.0f32, 2.0f32, 1.0f32), (2.0, 2.0, 2.0), (2.0, 1.0, 3.0), (3.0, 0.0, 0.0)]
            .iter()
            .map(|&(i, j, k)| {
                let mut arr: Vec<[f32; 4]> = (0..len).map(|q| [q as f32, 0.0, 0.0, 1.0]).collect();
                arr[i as usize] = [9.0, 8.0, 7.0, 6.0];
                arr[j as usize][1] += 5.0;
                let mut w = vec![0u32; 12];
                w[0] = i.to_bits();
                w[1] = j.to_bits();
                w[2] = k.to_bits();
                for (q, v) in [9.0f32, 8.0, 7.0, 6.0].iter().enumerate() {
                    w[4 + q] = v.to_bits();
                }
                (w, [one(arr[2][0]), one(arr[2][1]), one(arr[k as usize][0]), one(arr[1][1])])
            })
            .collect::<Vec<_>>();
        dxbc_raw(out, errors, n, "a local array written at a run-time index", &src, "ps_4_0", Tol::Exact, px.clone());
        let src3 = src.replace("cbuffer C : register(b0) { float4 A; float4 B; float4 Cc; };", "float4 A : register(c0); float4 B : register(c1); float4 Cc : register(c2);").replace("SV_Target", "COLOR");
        let mut inc = |_: &str, _: bool| None;
        match hlslc::compile(&src3, &[], &mut inc, "ps", "ps_3_0", 0) {
            Ok(code) => {
                let words: Vec<u32> = code.chunks(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
                let px3 = px.iter().map(|(w, want)| {
                    let f = |i: usize| [f32::from_bits(w[i]), f32::from_bits(w[i + 1]), f32::from_bits(w[i + 2]), f32::from_bits(w[i + 3])];
                    ([f(0), f(4), f(8)], want.clone())
                }).collect();
                d3d9_words(out, errors, &format!("hlsl_{n}"), "a local array written at a run-time index (shader model 3)", &words, &src3, Tol::Exact, px3);
            }
            Err(e) => errors.push(format!("d3d9/hlsl_{n}: {e}")),
        }
    }
}

/// HLSL constant packing ("Packing rules for constant variables"): a member never straddles a
/// 16-byte register, arrays put every element in a register of its own, matrices are
/// column-major by default. Every word of the buffer holds its index; reading a member gives
/// the index of the word it is at.
fn packing(out: &mut Vec<Case>, errors: &mut Vec<String>) {
    // (declarations, expression read, word it must come from)
    let cases: &[(&str, &str, &str, u32)] = &[
        ("float_float2", "float a; float2 b;", "b.x", 1),
        ("float2_float3", "float2 a; float3 b;", "b.x", 4),
        ("float_float3", "float a; float3 b;", "b.y", 2),
        ("float3_float", "float3 a; float b;", "b", 3),
        ("float3_float2", "float3 a; float2 b;", "b.y", 5),
        ("array_stride", "float a[3]; float b;", "a[1]", 4),
        ("array_tail", "float a[3]; float b;", "b", 9),
        ("array_float2", "float2 a[2]; float b;", "b", 6),
        ("struct", "struct S { float3 v; float f; }; S s; float g;", "s.f", 3),
        ("struct_after", "struct S { float f; }; float g; S s;", "s.f", 4),
        ("matrix_column_major", "float4x4 m;", "m._m01", 4),
        ("matrix_column_major_10", "float4x4 m;", "m._m10", 1),
        ("matrix_row_major", "row_major float4x4 m;", "m._m01", 1),
        ("matrix_2x3_column", "float2x3 m; float f;", "f", 10),
        ("matrix_3x2_row", "row_major float3x2 m; float f;", "f", 10),
        ("matrix_3x3_column", "float3x3 m;", "m._m21", 6),
        ("after_matrix", "float3x3 m; float f;", "f", 11),
    ];
    for &(name, decl, expr, word) in cases {
        let src = format!("cbuffer C : register(b0) {{ {decl} }};\nfloat4 ps() : SV_Target {{ return float4({expr}, 0, 0, 1); }}\n");
        match dxbc_wgsl(&src, "ps_4_0") {
            Ok((wgsl, group, binding, code)) => {
                let data: Vec<u8> = (0..64u32).flat_map(|i| (i as f32).to_bits().to_le_bytes()).collect();
                out.push(Case {
                    name: format!("dxbc/pack_{name}"),
                    api: "dxbc",
                    src: src.clone(),
                    code,
                    wgsl,
                    group,
                    binding,
                    pixels: vec![(data, wf(vec![word as f32]))],
                    tol: Tol::Exact,
                    rule: "HLSL constant buffer packing",
                });
            }
            Err(e) => errors.push(format!("dxbc/pack_{name}: {e}\n{src}")),
        }
    }
}

// ---------------------------------------------------------------- Direct3D 9 (ps_3_0 assembly)

/// A ps_3_0 case: `body` (assembly) computes r0 from c0, c1, c2 (each a float4 per pixel).
fn d3d9(out: &mut Vec<Case>, errors: &mut Vec<String>, name: &str, rule: &'static str, body: &str, tol: Tol, pixels: Vec<([[f32; 4]; 3], [Want; 4])>) {
    let src = format!("ps_3_0\n{body}\nmov oC0, r0\n");
    match hlslc::d3d9asm::assemble(&src) {
        Ok(words) => d3d9_words(out, errors, name, rule, &words, &src, tol, pixels),
        Err(e) => errors.push(format!("d3d9/{name}: {e}\n{src}")),
    }
}

/// A D3D9 pixel shader case from its tokens; c0, c1, c2 per pixel.
#[allow(clippy::too_many_arguments)]
pub fn d3d9_words(out: &mut Vec<Case>, errors: &mut Vec<String>, name: &str, rule: &'static str, words: &[u32], src: &str, tol: Tol, pixels: Vec<([[f32; 4]; 3], [Want; 4])>) {
    let r = (|| -> Result<(String, u32), String> {
        let s = dxwgsl::d3d9::Shader::parse(words).ok_or("dxwgsl cannot parse it")?;
        let n = s.max_const;
        let w = dxwgsl::d3d9::Gen::new(&s).pixel(&dxwgsl::d3d9::Options { link: 0, vs_inputs: Vec::new(), alpha_test: None, fog: false, addr: Vec::new(), targets: 1, flat: false });
        Ok((w, dxwgsl::d3d9::uniform_size(n)))
    })();
    match r {
        Ok((wgsl, size)) => out.push(Case {
            name: format!("d3d9/{name}"),
            api: "d3d9",
            src: src.to_string(),
            code: words.iter().flat_map(|w| w.to_le_bytes()).collect(),
            wgsl,
            group: 0,
            binding: 1,
            pixels: pixels
                .into_iter()
                .map(|(c, w)| {
                    let mut d = vec![0u8; size as usize];
                    // (x[4], i[16], then c[])
                    let at = (dxwgsl::d3d9::CONSTS_EXTRA as usize + 16) * 16;
                    for (k, v) in c.iter().flatten().enumerate() {
                        if at + k * 4 + 4 <= d.len() {
                            d[at + k * 4..at + k * 4 + 4].copy_from_slice(&v.to_le_bytes());
                        }
                    }
                    (d, w)
                })
                .collect(),
            tol,
            rule,
        }),
        Err(e) => errors.push(format!("d3d9/{name}: {e}\n{src}")),
    }
}

fn x(a: f32) -> [[f32; 4]; 3] {
    [[a, 0.0, 0.0, 0.0], [0.0; 4], [0.0; 4]]
}
fn xyz(a: f32, b: f32, c: f32) -> [[f32; 4]; 3] {
    [[a, 0.0, 0.0, 0.0], [b, 0.0, 0.0, 0.0], [c, 0.0, 0.0, 0.0]]
}
/// r0 = (v, 0, 0, 0)
fn w1(v: Vec<f32>) -> [Want; 4] {
    [Some(v), None, None, None]
}

fn d3d9_cases(out: &mut Vec<Case>, errors: &mut Vec<String>) {
    let finite: Vec<f32> = F.iter().copied().filter(|a| a.is_finite() && a.abs() < 1e19).collect();
    let un = |f: fn(f32) -> Vec<f32>| -> Vec<_> { finite.iter().map(|&a| (x(a), w1(f(a)))).collect() };
    // (results the D3D9 reference calls infinity: FLT_MAX allowed too, as drivers and Wine give)
    let inf_or_max = |v: f32| if v.is_infinite() { vec![v, f32::MAX.copysign(v)] } else { vec![v] };
    d3d9(out, errors, "rcp", "rcp: 1/x, rcp(0) = +inf (FLT_MAX accepted), 1/1 exact", "mov r0, c0.y\nrcp r0.x, c0.x", Tol::Ulp(1), finite.iter().map(|&a| (x(a), w1(inf_or_max(1.0 / a).into_iter().chain(if a == 0.0 { vec![f32::INFINITY, f32::MAX] } else { vec![] }).collect()))).collect());
    d3d9(out, errors, "rsq", "rsq: 1/sqrt(|x|), rsq(0) = +inf (FLT_MAX accepted)", "mov r0, c0.y\nrsq r0.x, c0.x", Tol::Ulp(2), finite.iter().map(|&a| (x(a), w1(if a == 0.0 { vec![f32::INFINITY, f32::MAX] } else { vec![1.0 / a.abs().sqrt()] }))).collect());
    d3d9(out, errors, "log", "log: log2(|x|), log(0) = -inf (-FLT_MAX accepted)", "mov r0, c0.y\nlog r0.x, c0.x", Tol::Rel(4e-7, 5e-7), finite.iter().map(|&a| (x(a), w1(if a == 0.0 { vec![f32::NEG_INFINITY, -f32::MAX] } else { vec![a.abs().log2()] }))).collect());
    let small: Vec<f32> = finite.iter().copied().filter(|a| a.abs() <= 100.0).collect();
    d3d9(out, errors, "exp", "exp: 2^x", "mov r0, c0.y\nexp r0.x, c0.x", Tol::Rel(4e-7, 5e-7), small.iter().map(|&a| (x(a), w1(vec![a.exp2()]))).collect());
    d3d9(out, errors, "frc", "frc: x - floor(x)", "frc r0, c0.x", Tol::Exact, un(|a| vec![a - a.floor()]));
    d3d9(out, errors, "abs", "abs and the _abs source modifier", "abs r0, c0.x\nadd r0, r0, c0_abs.x", Tol::Exact, un(|a| vec![2.0 * a.abs()]));
    d3d9(out, errors, "sat", "_sat: clamp to [0, 1]", "mov_sat r0, c0.x", Tol::Exact, un(|a| vec![a.clamp(0.0, 1.0)]));
    let pw: Vec<_> = [(2.0f32, 3.0f32), (-2.0, 3.0), (2.0, 0.5), (-4.0, 0.5), (0.0, 2.0), (9.0, -0.5), (1.0, 100.0)]
        .iter()
        .map(|&(a, b)| (xyz(a, b, 0.0), w1(vec![a.abs().powf(b)])))
        .collect();
    d3d9(out, errors, "pow", "pow: |x|^y", "mov r0, c0.y\npow r0.x, c0.x, c1.x", Tol::Rel(2e-6, 1e-6), pw);
    let cmp: Vec<_> = [0.0f32, -0.0, 1.0, -1.0, 0.25, -1e-3].iter().map(|&a| (xyz(a, 10.0, 20.0), w1(vec![if a >= 0.0 { 10.0 } else { 20.0 }]))).collect();
    d3d9(out, errors, "cmp", "cmp: src0 >= 0 ? src1 : src2 (-0 >= 0)", "cmp r0, c0.x, c1.x, c2.x", Tol::Exact, cmp);
    let lrp: Vec<_> = [(0.0f32, 2.0f32, 6.0f32), (1.0, 2.0, 6.0), (0.25, 2.0, 6.0), (2.0, -1.0, 1.0)].iter().map(|&(a, b, c)| (xyz(a, b, c), w1(vec![a * (b - c) + c, a.mul_add(b - c, c)]))).collect();
    d3d9(out, errors, "lrp", "lrp: src0 * (src1 - src2) + src2", "lrp r0, c0.x, c1.x, c2.x", Tol::Ulp(1), lrp);
    let mm: Vec<_> = [(1.0f32, 2.0f32), (-1.0, -2.0), (0.5, 0.5), (3.0, -3.0)].iter().flat_map(|&(a, b)| {
        [(a, b), (b, a)]
    }).map(|(a, b)| (xyz(a, b, 0.0), [one(a.min(b)), one(a.max(b)), None, None])).collect();
    d3d9(out, errors, "min_max", "min, max", "mov r0, c2\nmin r0.x, c0.x, c1.x\nmax r0.y, c0.x, c1.x", Tol::Exact, mm);
    let dp = vec![([[1.0f32, 2.0, 3.0, 4.0], [5.0, 6.0, 7.0, 8.0], [0.5, 0.0, 0.0, 0.0]], [one(17.0 + 0.5), one(1.0 * 5.0 + 2.0 * 6.0 + 3.0 * 7.0), one(70.0), None])];
    d3d9(out, errors, "dp", "dp2add, dp3, dp4", "mov r0, c2.y\ndp2add r0.x, c0, c1, c2.x\ndp3 r0.y, c0, c1\ndp4 r0.z, c0, c1", Tol::Exact, dp);
    let nrm = vec![([[3.0f32, 0.0, 4.0, 9.0], [0.0; 4], [0.0; 4]], [one(0.6), one(0.0), one(0.8), None])];
    d3d9(out, errors, "nrm", "nrm: xyz / |xyz|", "nrm r0.xyz, c0", Tol::Ulp(2), nrm);
    let crs = vec![([[1.0f32, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0], [0.0; 4]], [one(0.0), one(0.0), one(1.0), None])];
    d3d9(out, errors, "crs", "crs: cross product", "mov r0, c2\ncrs r0.xyz, c0, c1", Tol::Exact, crs);
    let sc: Vec<_> = [0.0f32, 0.5, 1.0, 3.0, -2.0].iter().map(|&a| (x(a), [one(a.cos()), one(a.sin()), None, None])).collect();
    d3d9(out, errors, "sincos", "sincos: .x cos, .y sin", "mov r0, c1\nsincos r0.xy, c0.x", Tol::Abs(0.002), sc);
    // flow control: rep with an integer constant, if/break, the loop counter
    d3d9(out, errors, "rep", "rep i#: the count from defi", "defi i0, 3, 0, 0, 0\nmov r0, c2\nrep i0\nadd r0.x, r0.x, c0.x\nendrep", Tol::Exact, vec![(x(1.5), w1(vec![4.5]))]);
    d3d9(out, errors, "if_gt", "if_gt / else", "mov r0, c2\nif_gt c0.x, c1.x\nmov r0.x, c0.x\nelse\nmov r0.x, c1.x\nendif", Tol::Exact,
        vec![(xyz(2.0, 1.0, 0.0), w1(vec![2.0])), (xyz(1.0, 2.0, 0.0), w1(vec![2.0])), (xyz(-1.0, -1.0, 0.0), w1(vec![-1.0]))]);
    d3d9(out, errors, "break_comp", "break_ge inside rep", "defi i0, 10, 0, 0, 0\nmov r0, c2\nrep i0\nadd r0.x, r0.x, c0.x\nbreak_ge r0.x, c1.x\nendrep", Tol::Exact,
        vec![(xyz(1.0, 3.0, 0.0), w1(vec![3.0])), (xyz(1.0, 100.0, 0.0), w1(vec![10.0]))]);
    d3d9(out, errors, "loop_aL", "loop aL, i#: aL = start, start + step, ...", "defi i0, 3, 2, 5, 0\nmov r0, c2\nloop aL, i0\nadd r0.x, r0.x, c0.x\nendloop", Tol::Exact, vec![(x(1.0), w1(vec![3.0]))]);
    d3d9(out, errors, "setp_if_pred", "setp_gt, then if on the predicate", "mov r0, c2\nsetp_gt p0.x, c0.x, c1.x\nif p0.x\nmov r0.x, c0.x\nendif", Tol::Exact,
        vec![(xyz(5.0, 1.0, 0.0), w1(vec![5.0])), (xyz(0.0, 1.0, 0.0), w1(vec![0.0]))]);
    d3d9(out, errors, "swizzle_mask", "swizzles and write masks", "mov r0, c2\nmov r0.yw, c0.wzyx", Tol::Exact,
        vec![([[1.0, 2.0, 3.0, 4.0], [0.0; 4], [9.0, 9.0, 9.0, 9.0]], [one(9.0), one(3.0), one(9.0), one(1.0)])]);
    d3d9(out, errors, "dsx_in_if", "dsx inside an if on a constant (WGSL's uniformity analysis must not reject it): the derivative of a constant is 0", "mov r0, c2\nif_gt c0.x, c1.x\ndsx r1, c0\nadd r0, r1, c2\nendif", Tol::Exact,
        vec![(xyz(2.0, 1.0, 5.0), w1(vec![5.0])), (xyz(0.0, 1.0, 5.0), w1(vec![5.0]))]);
    d3d9(out, errors, "def", "def: constants in the shader", "def c5, 0.25, 0.5, 2.0, -1.0\nmul r0, c0, c5", Tol::Exact,
        vec![([[4.0, 4.0, 4.0, 4.0], [0.0; 4], [0.0; 4]], [one(1.0), one(2.0), one(8.0), one(-4.0)])]);
}

// ---------------------------------------------------------------- OpenGL (GLSL 1.20)

pub const GL_VS: &str = "#version 120\nattribute vec4 p;\nvoid main() { gl_Position = p; }\n";

/// A GLSL case: `body` sets gl_FragColor from uniforms a, b, c (vec4), ia, ib (ivec4), m2,
/// m3, m4 (mat2-4, column-major as glUniformMatrix with transpose false), f (float[3]).
fn glsl(out: &mut Vec<Case>, errors: &mut Vec<String>, name: &str, rule: &'static str, body: &str, tol: Tol, pixels: Vec<(Vec<(&'static str, Vec<u32>)>, [Want; 4])>) {
    glsl_raw(out, errors, name, rule, &format!("void main() {{\n  {body}\n}}"), tol, pixels);
}

/// A GLSL case from its main function (and functions before it), with the uniforms of `glsl`.
pub fn glsl_raw(out: &mut Vec<Case>, errors: &mut Vec<String>, name: &str, rule: &'static str, main: &str, tol: Tol, pixels: Vec<(Vec<(&'static str, Vec<u32>)>, [Want; 4])>) {
    let ps = format!(
        "#version 120\nuniform vec4 a; uniform vec4 b; uniform vec4 c; uniform ivec4 ia; uniform ivec4 ib;\nuniform mat2 m2; uniform mat3 m3; uniform mat4 m4; uniform float f[3];\n{main}\n"
    );
    match glwgsl::link(GL_VS, &ps, &Default::default()) {
        Ok(p) => {
            let (group, binding) = uniform_binding(&p.ps.wgsl);
            let block = p.ps.block.as_ref();
            let size = block.map_or(16, |b| b.size.max(16)) as usize;
            out.push(Case {
                name: format!("glsl/{name}"),
                api: "glsl",
                src: ps.clone(),
                code: Vec::new(),
                wgsl: p.ps.wgsl.clone(),
                group,
                binding,
                pixels: pixels
                    .into_iter()
                    .map(|(vals, w)| {
                        let mut d = vec![0u8; size.next_multiple_of(16)];
                        for (n, v) in vals {
                            if let Some(m) = block.and_then(|b| b.members.iter().find(|m| m.name == n)) {
                                m.put(&mut d, &v);
                            }
                        }
                        (d, w)
                    })
                    .collect(),
                tol,
                rule,
            });
        }
        Err(e) => errors.push(format!("glsl/{name}: {e}")),
    }
}

/// (group, binding) of the WGSL's uniform buffer
fn uniform_binding(w: &str) -> (u32, u32) {
    let i = w.find("var<uniform>").unwrap_or(0);
    let head = &w[..i];
    let num = |key: &str| head.rfind(key).and_then(|j| head[j + key.len()..].split(')').next()?.trim().parse().ok()).unwrap_or(0);
    (num("@group("), num("@binding("))
}

fn fv(v: [f32; 4]) -> Vec<u32> {
    v.iter().map(|x| x.to_bits()).collect()
}
fn iv(v: [i32; 4]) -> Vec<u32> {
    v.iter().map(|&x| x as u32).collect()
}
fn w4(v: [f32; 4]) -> [Want; 4] {
    v.map(one)
}

fn glsl_cases(out: &mut Vec<Case>, errors: &mut Vec<String>) {
    let ab = |a: [f32; 4], b: [f32; 4]| vec![("a", fv(a)), ("b", fv(b))];
    let mods: Vec<_> = [(5.5f32, 2.0f32), (-5.5, 2.0), (5.5, -2.0), (-5.5, -2.0), (1.0, 0.3)]
        .iter()
        .map(|&(x, y)| (ab([x, 0.0, 0.0, 0.0], [y, 0.0, 0.0, 0.0]), w4([x - y * (x / y).floor(), 0.0, 0.0, 1.0])))
        .collect();
    glsl(out, errors, "mod", "mod(x, y) = x - y * floor(x / y) (the sign of y)", "gl_FragColor = vec4(mod(a.x, b.x), 0.0, 0.0, 1.0);", Tol::Ulp(2), mods);
    let un = [0.0f32, -0.0, 0.5, -0.5, 1.5, -1.5, 2.5, 3.7, -3.7, 100.0];
    let px = |f: fn(f32) -> f32| -> Vec<_> { un.iter().map(|&x| (ab([x, 0.0, 0.0, 0.0], [0.0; 4]), w4([f(x), 0.0, 0.0, 1.0]))).collect() };
    glsl(out, errors, "fract", "fract(x) = x - floor(x)", "gl_FragColor = vec4(fract(a.x), 0.0, 0.0, 1.0);", Tol::Exact, px(|x| x - x.floor()));
    glsl(out, errors, "sign", "sign: -1, 0, 1", "gl_FragColor = vec4(sign(a.x), 0.0, 0.0, 1.0);", Tol::Exact, px(|x| if x > 0.0 { 1.0 } else if x < 0.0 { -1.0 } else { 0.0 }));
    glsl(out, errors, "floor", "floor", "gl_FragColor = vec4(floor(a.x), 0.0, 0.0, 1.0);", Tol::Exact, px(f32::floor));
    glsl(out, errors, "ceil", "ceil", "gl_FragColor = vec4(ceil(a.x), 0.0, 0.0, 1.0);", Tol::Exact, px(f32::ceil));
    glsl(out, errors, "int_cast", "int(x) truncates toward zero", "gl_FragColor = vec4(float(int(a.x)), 0.0, 0.0, 1.0);", Tol::Exact, px(f32::trunc));
    glsl(out, errors, "step", "step(edge, x): x < edge ? 0 : 1", "gl_FragColor = vec4(step(0.5, a.x), 0.0, 0.0, 1.0);", Tol::Exact, px(|x| if x < 0.5 { 0.0 } else { 1.0 }));
    glsl(out, errors, "smoothstep", "smoothstep(0, 2, x): t*t*(3-2t)", "gl_FragColor = vec4(smoothstep(0.0, 2.0, a.x), 0.0, 0.0, 1.0);", Tol::Abs(1e-6), px(|x| {
        let t = (x / 2.0).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }));
    glsl(out, errors, "clamp", "clamp(x, -1, 1)", "gl_FragColor = vec4(clamp(a.x, -1.0, 1.0), 0.0, 0.0, 1.0);", Tol::Exact, px(|x| x.clamp(-1.0, 1.0)));
    glsl(out, errors, "mix", "mix(x, y, a) = x * (1 - a) + y * a", "gl_FragColor = vec4(mix(2.0, 6.0, a.x), 0.0, 0.0, 1.0);", Tol::Ulp(2), px(|x| 2.0 * (1.0 - x) + 6.0 * x));
    let pos = [0.25f32, 0.5, 1.0, 2.0, 9.0, 100.0];
    let ppx = |f: fn(f32) -> f32| -> Vec<_> { pos.iter().map(|&x| (ab([x, 0.0, 0.0, 0.0], [0.0; 4]), w4([f(x), 0.0, 0.0, 1.0]))).collect() };
    glsl(out, errors, "inversesqrt", "inversesqrt", "gl_FragColor = vec4(inversesqrt(a.x), 0.0, 0.0, 1.0);", Tol::Ulp(3), ppx(|x| 1.0 / x.sqrt()));
    glsl(out, errors, "pow", "pow(x, 1.5)", "gl_FragColor = vec4(pow(a.x, 1.5), 0.0, 0.0, 1.0);", Tol::Rel(2e-6, 1e-6), ppx(|x| x.powf(1.5)));
    let atan: Vec<_> = [(1.0f32, 1.0f32), (1.0, -1.0), (-1.0, -1.0), (-1.0, 1.0), (0.0, 1.0), (1.0, 0.0), (0.0, -1.0)]
        .iter()
        .map(|&(y, x)| (ab([y, 0.0, 0.0, 0.0], [x, 0.0, 0.0, 0.0]), w4([y.atan2(x), 0.0, 0.0, 1.0])))
        .collect();
    glsl(out, errors, "atan2", "atan(y, x): the quadrant from both signs", "gl_FragColor = vec4(atan(a.x, b.x), 0.0, 0.0, 1.0);", Tol::Abs(1e-5), atan);
    let idiv: Vec<_> = [(7, 2), (-7, 2), (7, -2), (-7, -2), (0, 5), (100, 7)]
        .iter()
        .map(|&(x, y): &(i32, i32)| (vec![("ia", iv([x, 0, 0, 0])), ("ib", iv([y, 0, 0, 0]))], w4([(x / y) as f32, 0.0, 0.0, 1.0])))
        .collect();
    glsl(out, errors, "int_div", "int division truncates", "gl_FragColor = vec4(float(ia.x / ib.x), 0.0, 0.0, 1.0);", Tol::Exact, idiv);
    let rel: Vec<_> = [(1.0f32, 2.0f32), (2.0, 1.0), (1.0, 1.0)]
        .iter()
        .map(|&(x, y)| (ab([x, 0.0, 0.0, 0.0], [y, 0.0, 0.0, 0.0]), w4([(x < y) as u8 as f32, (x >= y) as u8 as f32, (x == y) as u8 as f32, 1.0])))
        .collect();
    glsl(out, errors, "relational", "lessThan, greaterThanEqual, equal as floats", "gl_FragColor = vec4(vec3(lessThan(a.xxx, b.xxx).x, greaterThanEqual(a, b).x, equal(a, b).x), 1.0);", Tol::Exact, rel);
    // matrices: column-major uniforms (glUniformMatrix, transpose false); m * v sums columns
    let m4: Vec<f32> = (1..=16).map(|i| i as f32).collect();
    let v = [1.0f32, 0.5, -1.0, 2.0];
    let mv: [f32; 4] = std::array::from_fn(|r| (0..4).map(|c| m4[c * 4 + r] * v[c]).sum());
    let vm: [f32; 4] = std::array::from_fn(|c| (0..4).map(|r| m4[c * 4 + r] * v[r]).sum());
    let m4u: Vec<u32> = m4.iter().map(|x| x.to_bits()).collect();
    glsl(out, errors, "mat4_vec", "m * v: columns weighted by v (column-major storage)", "gl_FragColor = m4 * a;", Tol::Exact, vec![(vec![("m4", m4u.clone()), ("a", fv(v))], w4(mv))]);
    glsl(out, errors, "vec_mat4", "v * m: dot products with the columns", "gl_FragColor = a * m4;", Tol::Exact, vec![(vec![("m4", m4u.clone()), ("a", fv(v))], w4(vm))]);
    glsl(out, errors, "mat4_index", "m[i] is column i", "gl_FragColor = m4[1];", Tol::Exact, vec![(vec![("m4", m4u)], w4([5.0, 6.0, 7.0, 8.0]))]);
    let m3: Vec<u32> = (1..=9).map(|i| (i as f32).to_bits()).collect();
    glsl(out, errors, "mat3_columns", "mat3 uniform: three columns of three (std140 pads each to 16 bytes)", "gl_FragColor = vec4(m3[0].z, m3[1].x, m3[2].z, m3[1][1]);", Tol::Exact, vec![(vec![("m3", m3)], w4([3.0, 4.0, 9.0, 5.0]))]);
    let m2: Vec<u32> = [1.0f32, 2.0, 3.0, 4.0].iter().map(|x| x.to_bits()).collect();
    glsl(out, errors, "mat2_vec", "mat2 * vec2", "gl_FragColor = vec4(m2 * a.xy, m2[1]);", Tol::Exact, vec![(vec![("m2", m2), ("a", fv([1.0, 1.0, 0.0, 0.0]))], w4([4.0, 6.0, 3.0, 4.0]))]);
    let fa: Vec<u32> = [10.0f32, 20.0, 30.0].iter().map(|x| x.to_bits()).collect();
    glsl(out, errors, "float_array", "uniform float[3]: elements 16 bytes apart (std140)", "gl_FragColor = vec4(f[0], f[1], f[2], 1.0);", Tol::Exact, vec![(vec![("f", fa)], w4([10.0, 20.0, 30.0, 1.0]))]);
    let bools: Vec<_> = [0, 1, -3].iter().map(|&i| (vec![("ia", iv([i, 0, 0, 0]))], w4([(i != 0) as u8 as f32, if i != 0 { 2.0 } else { 3.0 }, 0.0, 1.0]))).collect();
    glsl(out, errors, "bool", "bool(int) and float(bool)", "bool t = bool(ia.x); gl_FragColor = vec4(float(t), t ? 2.0 : 3.0, 0.0, 1.0);", Tol::Exact, bools);
    glsl(out, errors, "loop", "for loop with a uniform bound and break", "float s = 0.0; for (int i = 0; i < 16; i++) { if (i >= ia.x) break; s += a.x; } gl_FragColor = vec4(s, 0.0, 0.0, 1.0);", Tol::Exact,
        [0, 3, 16, 40].iter().map(|&n| (vec![("ia", iv([n, 0, 0, 0])), ("a", fv([0.5, 0.0, 0.0, 0.0]))], w4([0.5 * n.min(16) as f32, 0.0, 0.0, 1.0]))).collect());
}

pub fn all(out: &mut Vec<Case>, errors: &mut Vec<String>) {
    dxbc_cases(out, errors);
    d3d9_cases(out, errors);
    glsl_cases(out, errors);
    super::fflight::cases(out, errors);
    super::random::cases(out, errors, 300, 0x5eed, 0, 2, "small");
    super::random::cases(out, errors, 500, 0x1bba1, 2, 4, "random");
}
