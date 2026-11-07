//! Rounding as the x87 control word (RC, PC) and MXCSR.RC say. An operation's result to
//! nearest `r` and its exact error `e` (r + e is the exact result: TwoSum, FMA) are enough to
//! round the exact result again in any direction and to fewer bits, without wider arithmetic.
//! Only the sign of `e` and whether it is 0 matter.

/// Rounding control (x87 CW bits 10-11, MXCSR bits 13-14)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Rc {
    Near,
    Down,
    Up,
    Zero,
}

impl Rc {
    pub fn from_bits(b: u32) -> Rc {
        match b & 3 {
            0 => Rc::Near,
            1 => Rc::Down,
            2 => Rc::Up,
            _ => Rc::Zero,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Sqrt,
}

fn next_up(x: f64) -> f64 {
    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x == 0.0 {
        return f64::from_bits(1);
    }
    let b = x.to_bits();
    f64::from_bits(if x > 0.0 { b + 1 } else { b - 1 })
}
fn next_down(x: f64) -> f64 {
    -next_up(-x)
}
fn next_up32(x: f32) -> f32 {
    if x.is_nan() || x == f32::INFINITY {
        return x;
    }
    if x == 0.0 {
        return f32::from_bits(1);
    }
    let b = x.to_bits();
    f32::from_bits(if x > 0.0 { b + 1 } else { b - 1 })
}
fn next_down32(x: f32) -> f32 {
    -next_up32(-x)
}

/// -1, 0, 1
fn sign(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// `r`, the nearest result of an operation that overflowed (finite operands, infinite result),
/// in the direction `rc`
fn overflowed(r: f64, max: f64, rc: Rc) -> f64 {
    match (rc, r > 0.0) {
        (Rc::Near, _) => r,
        (Rc::Zero, true) | (Rc::Down, true) => max,
        (Rc::Zero, false) | (Rc::Up, false) => -max,
        _ => r,
    }
}

/// Rounds the exact value r + e (r an f64, e of sign `es`, below half of r's ulp) onto a grid
/// coarser than f64's: `near` is the grid's nearest to an f64, `up`/`down` the next grid points.
fn regrid(r: f64, es: i32, rc: Rc, near: impl Fn(f64) -> f64, up: impl Fn(f64) -> f64, down: impl Fn(f64) -> f64) -> f64 {
    let n = near(r);
    if !r.is_finite() || n.is_nan() {
        return n;
    }
    // x - n: its sign is d's when d isn't 0 (d is a multiple of r's ulp, e below half of it)
    let d = r - n;
    let s = if d != 0.0 { sign(d) } else { es };
    match rc {
        Rc::Near => {
            // (n is r's nearest, ties to even; r exactly between two grid points and e pushing
            // away from n: the other one)
            if d != 0.0 && es == sign(d) && n.is_finite() {
                let nb = if d > 0.0 { up(n) } else { down(n) };
                if (n + nb) * 0.5 == r {
                    return nb;
                }
            }
            n
        }
        Rc::Down if s < 0 => down(n),
        Rc::Up if s > 0 => up(n),
        Rc::Zero if s < 0 && n > 0.0 => down(n),
        Rc::Zero if s > 0 && n < 0.0 => up(n),
        _ => n,
    }
}

/// The f32 grid
fn f32_grid(r: f64, es: i32, rc: Rc) -> f64 {
    regrid(r, es, rc, |x| x as f32 as f64, |x| next_up32(x as f32) as f64, |x| next_down32(x as f32) as f64)
}

/// Where a result is rounded to
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Prec {
    /// f64, its subnormals included: once (SSE)
    Sse,
    /// 53 bits (x87 PC double) at any exponent, then f64 (as the store of the x87 register:
    /// koji's registers are f64): twice
    X87,
    /// 24 bits (x87 PC single) at any exponent, then f64
    X87Single,
}

/// The exact value (p + e) * 2^k (e of sign `es`) rounded as `prec` says
fn place(p: f64, es: i32, k: i32, rc: Rc, prec: Prec) -> f64 {
    if prec == Prec::X87Single {
        // (24 bits on the significand)
        let (m, e) = libm::frexp(p);
        return place(f32_grid(m, es, rc), 0, e + k, rc, Prec::Sse);
    }
    if prec == Prec::X87 {
        return place(regrid(p, es, rc, |x| x, next_up, next_down), 0, k, rc, Prec::Sse);
    }
    // n: the nearest f64 (subnormals included) of p * 2^k, then moved as rc says; worked
    // out at the result's scale (scaling n's neighbours back can overflow: 2^-1074 * 2^2000)
    let n = libm::ldexp(p, k);
    if n.is_infinite() {
        // (p rounded up to 2^1024 or past it: the exact value is past the threshold)
        return overflowed(n, f64::MAX, rc);
    }
    // d = p - n's value at p's scale (exact: the bits of p below n's grid)
    let d = p - libm::ldexp(n, -k);
    let s = if d != 0.0 { sign(d) } else { es };
    match rc {
        Rc::Near => {
            // (p exactly between two grid points and e pushing away from n: the other one)
            if d != 0.0 && es == sign(d) {
                let o = if d > 0.0 { next_up(n) } else { next_down(n) };
                if (libm::ldexp(n, -k) + libm::ldexp(o, -k)) * 0.5 == p {
                    return o;
                }
            }
            n
        }
        Rc::Down if s < 0 => next_down(n),
        Rc::Up if s > 0 => next_up(n),
        Rc::Zero if s < 0 && n > 0.0 => next_down(n),
        Rc::Zero if s > 0 && n < 0.0 => next_up(n),
        _ => n,
    }
}

/// The exact sum's zero: -0 rounding down, else +0 (an exact zero of nonzero operands, or
/// zeros of opposite signs)
fn zero_sum(a: f64, b: f64, rc: Rc) -> f64 {
    if a == 0.0 && b == 0.0 && a.is_sign_negative() == b.is_sign_negative() {
        return a;
    }
    if rc == Rc::Down { -0.0 } else { 0.0 }
}

/// `a op b` (b unused for Sqrt) rounded in direction `rc` as `prec` says
fn rounded64(op: Op, a: f64, b: f64, rc: Rc, prec: Prec) -> f64 {
    let plain = match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        Op::Div => a / b,
        Op::Sqrt => libm::sqrt(a),
    };
    // (exact: NaNs, infinities, zeros, x / 0, sqrt of a negative)
    let special = !a.is_finite() || (op != Op::Sqrt && !b.is_finite()) || plain.is_nan();
    match op {
        _ if special => plain,
        Op::Add | Op::Sub => {
            let b = if op == Op::Sub { -b } else { b };
            let s = a + b;
            if s.is_infinite() {
                return overflowed(s, f64::MAX, rc);
            }
            // TwoSum
            let bb = s - a;
            let e = (a - (s - bb)) + (b - bb);
            if s == 0.0 && e == 0.0 {
                return zero_sum(a, b, rc);
            }
            place(s, sign(e), 0, rc, prec)
        }
        Op::Mul | Op::Div => {
            if a == 0.0 || b == 0.0 {
                return plain;
            }
            // on the significands (no underflow or overflow there), the exponents apart
            let ((ma, ea), (mb, eb)) = (libm::frexp(a), libm::frexp(b));
            let (p, es, k) = if op == Op::Mul {
                let p = ma * mb;
                (p, sign(libm::fma(ma, mb, -p)), ea + eb)
            } else {
                let q = ma / mb;
                (q, sign(-libm::fma(q, mb, -ma)) * sign(mb), ea - eb)
            };
            place(p, es, k, rc, prec)
        }
        Op::Sqrt => {
            if a == 0.0 {
                return plain;
            }
            let (mut m, mut e) = libm::frexp(a);
            if e % 2 != 0 {
                m *= 2.0;
                e -= 1;
            }
            let q = libm::sqrt(m);
            place(q, sign(-libm::fma(q, q, -m)), e / 2, rc, prec)
        }
    }
}

/// The same for f32 operands and result (SSE): worked out in f64, rounded to the f32 grid
fn rounded32(op: Op, a: f32, b: f32, rc: Rc) -> f32 {
    let plain = match op {
        Op::Add => a + b,
        Op::Sub => a - b,
        Op::Mul => a * b,
        Op::Div => a / b,
        Op::Sqrt => libm::sqrtf(a),
    };
    if !a.is_finite() || (op != Op::Sqrt && !b.is_finite()) || plain.is_nan() {
        return plain;
    }
    let (x, y) = (a as f64, if op == Op::Sub { -(b as f64) } else { b as f64 });
    let (r, es) = match op {
        Op::Add | Op::Sub => {
            let s = x + y;
            let bb = s - x;
            let e = (x - (s - bb)) + (y - bb);
            if s == 0.0 && e == 0.0 {
                return zero_sum(x, y, rc) as f32;
            }
            (s, sign(e))
        }
        // (24-bit significands: the product is exact)
        Op::Mul => (x * y, 0),
        Op::Div => {
            if y == 0.0 {
                return plain;
            }
            let q = x / y;
            (q, sign(-libm::fma(q, y, -x)) * sign(y))
        }
        Op::Sqrt => {
            let q = libm::sqrt(x);
            (q, sign(-libm::fma(q, q, -x)))
        }
    };
    f32_grid(r, es, rc) as f32
}

/// The exact value r + e (e of sign `es`) rounded to f32 in direction `rc` (stores, CVTSD2SS)
pub fn to_f32(r: f64, es: i32, rc: Rc) -> f32 {
    f32_grid(r, es, rc) as f32
}

/// An x87 arithmetic result: `a op b` rounded as CW says (RC, and PC: 24 bits when single;
/// double and extended both 53 here, koji's registers being f64), then to f64 as a store
/// would (twice: below f64's normal range that is not the same as once)
pub fn x87(op: Op, a: f64, b: f64, cw: u16) -> f64 {
    let rc = Rc::from_bits((cw >> 10) as u32);
    let single = (cw >> 8) & 3 == 0;
    if rc == Rc::Near && !single {
        let r = match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div => a / b,
            Op::Sqrt => libm::sqrt(a),
        };
        // (sums and square roots below the normal range are exact)
        if !(matches!(op, Op::Mul | Op::Div) && r.abs() < f64::MIN_POSITIVE) {
            return r;
        }
    }
    rounded64(op, a, b, rc, if single { Prec::X87Single } else { Prec::X87 })
}

/// An SSE single-precision result rounded as MXCSR.RC says
pub fn sse32(op: Op, a: f32, b: f32, mxcsr: u32) -> f32 {
    let rc = Rc::from_bits(mxcsr >> 13);
    if rc == Rc::Near {
        return match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div => a / b,
            Op::Sqrt => libm::sqrtf(a),
        };
    }
    rounded32(op, a, b, rc)
}

/// An SSE double-precision result rounded as MXCSR.RC says
pub fn sse64(op: Op, a: f64, b: f64, mxcsr: u32) -> f64 {
    let rc = Rc::from_bits(mxcsr >> 13);
    if rc == Rc::Near {
        return match op {
            Op::Add => a + b,
            Op::Sub => a - b,
            Op::Mul => a * b,
            Op::Div => a / b,
            Op::Sqrt => libm::sqrt(a),
        };
    }
    rounded64(op, a, b, rc, Prec::Sse)
}

/// f64 -> f32 as RC says (x87 stores, CVTSD2SS, CVTPD2PS)
pub fn f64_to_f32(v: f64, rc_bits: u32) -> f32 {
    to_f32(v, 0, Rc::from_bits(rc_bits))
}

/// i64 -> f64 as RC says (FILD of a 64-bit integer, CVTSI2SD of one)
pub fn i64_to_f64(v: i64, rc_bits: u32) -> f64 {
    let r = v as f64;
    let rc = Rc::from_bits(rc_bits);
    if rc == Rc::Near {
        return r;
    }
    let e = v as i128 - r as i128;
    place(r, e.signum() as i32, 0, rc, Prec::Sse)
}

/// i64 -> f32 as RC says (CVTSI2SS)
pub fn i64_to_f32(v: i64, rc_bits: u32) -> f32 {
    let r = v as f64;
    let e = v as i128 - r as i128;
    to_f32(r, e.signum() as i32, Rc::from_bits(rc_bits))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        /// mostly ordinary numbers, some extremes
        fn f64(&mut self) -> f64 {
            match self.next() % 10 {
                // (the smallest subnormals, and the normal range's edge)
                8 => f64::from_bits(self.next() % 64 | (self.next() & 1) << 63),
                9 => f64::MIN_POSITIVE * [1.0, -1.0, 0.5, 1.5][(self.next() % 4) as usize],
                0 => f64::from_bits(self.next()),
                1 => (self.next() % 1000) as f64 / 8.0,
                2 => f64::from_bits(self.next() & 0x000f_ffff_ffff_ffff),
                3 => f64::MAX / (1 + self.next() % 4) as f64,
                _ => ((self.next() as i64 >> (self.next() % 60)) as f64) / (1u64 << (self.next() % 40)) as f64,
            }
        }
    }

    /// The exact value as a rational: (numerator, power of 2) for finite values
    fn bigval(v: f64) -> Option<(i128, i32)> {
        if !v.is_finite() {
            return None;
        }
        let (m, e) = if v == 0.0 { (0i128, 0) } else {
            let b = v.to_bits();
            let ex = ((b >> 52) & 0x7ff) as i32;
            let mant = (b & ((1 << 52) - 1)) as i128;
            if ex == 0 { (mant, -1074) } else { (mant | 1 << 52, ex - 1075) }
        };
        Some((if v < 0.0 { -m } else { m }, e))
    }

    /// The result of an add or mul of a and b rounded in direction rc, checked against bracketing:
    /// the result's neighbours lie on the right sides of the exact value (computed with i128 when
    /// the exponents allow)
    fn check(op: Op, a: f64, b: f64, rc: Rc) {
        let mx = match rc { Rc::Near => 0, Rc::Down => 1, Rc::Up => 2, Rc::Zero => 3 } << 13;
        let got = sse64(op, a, b, mx);
        let (Some((ma, ea)), Some((mb, eb))) = (bigval(a), bigval(b)) else { return };
        // exact = n * 2^ex as i128 if it fits
        let exact: Option<(i128, i32)> = match op {
            Op::Add | Op::Sub => {
                let mb = if op == Op::Sub { -mb } else { mb };
                let e = ea.min(eb);
                let (sa, sb) = ((ea - e) as u32, (eb - e) as u32);
                if sa > 60 || sb > 60 { None } else { Some(((ma << sa) + (mb << sb), e)) }
            }
            Op::Mul => Some((ma * mb, ea + eb)),
            _ => None,
        };
        let Some((n, ex)) = exact else { return };
        let Some((mg, eg)) = bigval(got) else {
            // (infinite: only past f64::MAX)
            let top = 128 - n.unsigned_abs().leading_zeros() as i32 + ex;
            assert!(!got.is_infinite() || top >= 1024, "{op:?} {a:e} {b:e} {rc:?} -> {got:e}");
            return;
        };
        // compare got with exact: got - exact sign
        let cmp = |m: i128, e: i32| -> Option<i32> {
            // (far apart in magnitude: the larger one's sign; tiny results of tiny operands)
            let top = |v: i128, e: i32| 128 - v.unsigned_abs().leading_zeros() as i32 + e;
            if m != 0 && n != 0 && (top(m, e) - top(n, ex)).abs() > 2 {
                return Some(if top(m, e) > top(n, ex) { m.signum() as i32 } else { -n.signum() as i32 });
            }
            let k = e.min(ex);
            let (s1, s2) = ((e - k) as u32, (ex - k) as u32);
            if m == 0 || n == 0 {
                return Some((m.signum() - n.signum()).signum() as i32);
            }
            if s1 > 70 || s2 > 70 { return None; }
            let shl = |v: i128, s: u32| if v != 0 && v.unsigned_abs().leading_zeros() <= s + 1 { None } else { Some(v << s) };
            let d = shl(m, s1)?.checked_sub(shl(n, s2)?)?;
            Some(d.signum() as i32)
        };
        let Some(c) = cmp(mg, eg) else { return };
        let up = bigval(next_up(got)).and_then(|(m, e)| cmp(m, e));
        let down = bigval(next_down(got)).and_then(|(m, e)| cmp(m, e));
        match rc {
            Rc::Down => assert!(c <= 0 && up.is_none_or(|u| u > 0), "{op:?} {a:e} {b:e} down -> {got:e}"),
            Rc::Up => assert!(c >= 0 && down.is_none_or(|d| d < 0), "{op:?} {a:e} {b:e} up -> {got:e}"),
            Rc::Zero => {
                if n >= 0 {
                    assert!(c <= 0 && up.is_none_or(|u| u > 0), "{op:?} {a:e} {b:e} zero -> {got:e}");
                } else {
                    assert!(c >= 0 && down.is_none_or(|d| d < 0), "{op:?} {a:e} {b:e} zero -> {got:e}");
                }
            }
            Rc::Near => assert_eq!(got.to_bits(), match op { Op::Add => a + b, Op::Sub => a - b, _ => a * b }.to_bits()),
        }
    }

    #[test]
    fn directed_add_mul_bracket_the_exact_value() {
        let mut r = Rng(0x1234_5678);
        for _ in 0..300_000 {
            let (a, b) = (r.f64(), r.f64());
            for op in [Op::Add, Op::Sub, Op::Mul] {
                for rc in [Rc::Near, Rc::Down, Rc::Up, Rc::Zero] {
                    check(op, a, b, rc);
                }
            }
        }
    }

    #[test]
    fn directed_results_are_neighbours_of_nearest() {
        // down <= nearest <= up, at most one ulp apart, zero between down and up
        let mut r = Rng(99);
        for _ in 0..300_000 {
            let (a, b) = (r.f64(), r.f64());
            for op in [Op::Add, Op::Sub, Op::Mul, Op::Div, Op::Sqrt] {
                let a = if op == Op::Sqrt { a.abs() } else { a };
                let n = x87(op, a, b, 0x027f);
                let d = x87(op, a, b, 0x067f);
                let u = x87(op, a, b, 0x0a7f);
                let z = x87(op, a, b, 0x0e7f);
                if !n.is_finite() || n.is_nan() || !d.is_finite() || !u.is_finite() {
                    continue;
                }
                assert!(d <= n && n <= u, "{op:?} {a:e} {b:e}: {d:e} {n:e} {u:e}");
                assert!(d == u || next_up(d) == u, "{op:?} {a:e} {b:e}: {d:e} {u:e}");
                assert!(z == if n.is_sign_positive() { d } else { u } || d == u, "{op:?} {a:e} {b:e} zero {z:e}");
                // single precision: the result has 24 bits
                let s = x87(op, a, b, 0x007f);
                if s.is_finite() && s != 0.0 {
                    let (m, _) = libm::frexp(s);
                    assert_eq!(libm::ldexp(m, 24).fract(), 0.0, "{op:?} {a:e} {b:e}: {s:e}");
                }
            }
        }
    }

    #[test]
    fn f32_rounding_matches_hardware_conversions() {
        // to_f32 to nearest with e = 0 is `as f32`; directed ones bracket
        let mut r = Rng(5);
        for _ in 0..300_000 {
            let v = r.f64();
            assert_eq!(to_f32(v, 0, Rc::Near).to_bits(), (v as f32).to_bits());
            let (d, u) = (to_f32(v, 0, Rc::Down), to_f32(v, 0, Rc::Up));
            if v.is_finite() && (d.is_finite() || d == f32::NEG_INFINITY) {
                assert!((d as f64) <= v && v <= u as f64, "{v:e}: {d:e} {u:e}");
                assert!(d == u || next_up32(d) == u || (d == f32::MAX && u.is_infinite()), "{v:e}: {d:e} {u:e}");
            }
        }
    }

    #[test]
    fn sse32_against_f64_reference() {
        // f32 ops: the f64 result of the same operands is exact for add (when close) and mul,
        // then rounded by to_f32 with that direction
        let mut r = Rng(17);
        for _ in 0..300_000 {
            let (a, b) = (r.f64() as f32, r.f64() as f32);
            for (op, exact) in [(Op::Mul, a as f64 * b as f64), (Op::Add, a as f64 + b as f64)] {
                if op == Op::Add {
                    // (only when exact in f64: TwoSum's error 0)
                    let (x, y) = (a as f64, b as f64);
                    let s = x + y;
                    let bb = s - x;
                    // (an exact zero sum's sign depends on rc: zero_sum)
                    if (x - (s - bb)) + (y - bb) != 0.0 || s == 0.0 {
                        continue;
                    }
                }
                for (rc, bits) in [(Rc::Down, 1u32), (Rc::Up, 2), (Rc::Zero, 3), (Rc::Near, 0)] {
                    let want = to_f32(exact, 0, rc);
                    let got = sse32(op, a, b, bits << 13);
                    assert!(got.to_bits() == want.to_bits() || (got.is_nan() && want.is_nan()), "{op:?} {a:e} {b:e} {rc:?}: {got:e} want {want:e}");
                }
            }
        }
    }

    #[test]
    fn integer_conversions() {
        let mut r = Rng(3);
        for _ in 0..300_000 {
            let v = r.next() as i64 >> (r.next() % 64);
            assert_eq!(i64_to_f64(v, 0).to_bits(), (v as f64).to_bits());
            assert_eq!(i64_to_f32(v, 0).to_bits(), (v as f32).to_bits(), "{v}");
            for rc in 1..4 {
                let (d, f) = (i64_to_f64(v, rc), i64_to_f32(v, rc));
                let (dv, fv) = (d as i128, f as i128);
                let want = |x: i128| match rc { 1 => x <= v as i128, 2 => x >= v as i128, _ => x.abs() <= (v as i128).abs() };
                assert!(want(dv) && want(fv), "{v} {rc}: {d:e} {f:e}");
            }
            // exact zero sums
            assert!(x87(Op::Add, 1.0, -1.0, 0x067f).is_sign_negative());
            assert!(x87(Op::Add, 1.0, -1.0, 0x0a7f).is_sign_positive());
        }
    }

    #[test]
    fn found_by_the_cpu() {
        // (jit32test native: x87 single precision keeps the f64 exponent; overflow toward 0)
        let r = x87(Op::Add, f64::from_bits(0xf83c140000000040), f64::from_bits(0x7d00000000000000), 0x0c7f);
        assert_eq!(r.to_bits(), 0x7cffffffe0000000, "{r:e}");
        let r = x87(Op::Add, 0.0, f64::from_bits(0x00038f42840000c4), 0x007f);
        assert_eq!(r.to_bits() & 0xff, 0, "{r:e}");
        assert!(r != 0.0);
        let r = x87(Op::Mul, f64::from_bits(0x0b98bb09ae0a4110), f64::from_bits(0xa00000466d6200d7), 0x047f);
        assert_eq!(r.to_bits(), 0x8000_0000_0000_0001, "{r:e}");
        let r = sse64(Op::Div, f64::MAX, f64::from_bits(0x0000017b000001f3), 0x7f80);
        assert_eq!(r, f64::MAX);
        // (jit32test native: a product of the smallest subnormals rounding up)
        let t = f64::from_bits(1);
        assert_eq!(sse64(Op::Mul, t, t, 0x5f80), t);
        assert_eq!(sse64(Op::Mul, -t, t, 0x3f80), -t);
        assert_eq!(sse64(Op::Div, t, f64::MAX, 0x5f80), t);
        assert_eq!(x87(Op::Mul, t, t, 0x0a7f), t);
    }

    #[test]
    fn x87_rounds_tiny_results_twice() {
        // 53 bits at any exponent (done by sse64 on a scaled operand: exact), then to f64
        // (sse64's multiplication by a power of 2); normal results as SSE's
        let mut r = Rng(77);
        let mut tiny = 0;
        for _ in 0..300_000 {
            // (products and quotients near and below f64's normal range)
            let mut v = |lo: i32, hi: i32| {
                let m = 1.0 + (r.next() >> 12) as f64 / (1u64 << 52) as f64;
                let e = lo + (r.next() % (hi - lo) as u64) as i32;
                if r.next() & 1 == 0 { libm::ldexp(m, e) } else { -libm::ldexp(m, e) }
            };
            let (a, b) = (v(-580, -480), v(-580, -480));
            for (op, b) in [(Op::Mul, b), (Op::Div, 1.0 / b)] {
                for (rc, bits) in [(Rc::Near, 0u32), (Rc::Down, 1), (Rc::Up, 2), (Rc::Zero, 3)] {
                    let got = x87(op, a, b, 0x027f | (bits as u16) << 10);
                    let once = sse64(op, a, b, bits << 13);
                    if !once.is_finite() || once.abs() >= f64::MIN_POSITIVE || a == 0.0 || b == 0.0 {
                        assert!(got.to_bits() == once.to_bits() || (got.is_nan() && once.is_nan()), "{op:?} {a:e} {b:e} {rc:?}: {got:e} {once:e}");
                        continue;
                    }
                    let up = libm::ldexp(1.0, 600);
                    let r53 = sse64(op, a * up, b, bits << 13);
                    if r53.abs() < f64::MIN_POSITIVE {
                        // (still below the range scaled: rare, skipped)
                        continue;
                    }
                    let want = sse64(Op::Mul, r53, libm::ldexp(1.0, -600), bits << 13);
                    assert_eq!(got.to_bits(), want.to_bits(), "{op:?} {a:e} {b:e} {rc:?}");
                    tiny += 1;
                }
            }
        }
        assert!(tiny > 1000, "{tiny}");
    }
}
