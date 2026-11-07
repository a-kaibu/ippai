//! Rounding modes in translated code. wasm rounds to nearest (and x87 arithmetic is f64 here):
//! an operation runs as wasm has it while the x87 control word / MXCSR asks for that; else
//! the nearest result is rounded again from its exact error (TwoSum, Dekker's product), all
//! in wasm, and what that can't do exactly (results outside the normal range, operands near
//! the exponent's limits, packed operations, square roots) leaves for the interpreter
//! (crate::round). No calls: a call in a loop makes V8 keep its values on the stack.
//!
//! The emitters leave their result in a local and set [`Tmp::bad`] when the instruction must
//! go to the interpreter (nothing of it done yet); the translator bails then.

use super::wasm::Code;

/// The locals the emitters use: 6 f64, 3 i64, 4 i32
#[derive(Clone, Copy)]
pub struct Tmp {
    pub f: u32,
    pub i: u32,
    pub n: u32,
}

impl Tmp {
    fn f(self, k: u32) -> u32 {
        self.f + k
    }
    /// i64: bits, truncated bits, granule
    fn rb(self) -> u32 {
        self.i
    }
    fn tt(self) -> u32 {
        self.i + 1
    }
    fn g(self) -> u32 {
        self.i + 2
    }
    /// i32: the rounding control, then the result's error toward and away from 0
    fn rc(self) -> u32 {
        self.n
    }
    /// i32: set when the interpreter must do the instruction
    pub fn bad(self) -> u32 {
        self.n + 1
    }
    fn pos(self) -> u32 {
        self.n + 2
    }
    fn neg(self) -> u32 {
        self.n + 3
    }
}

/// Where the x87 control word is: the Cpu (a local) and the field's offset. (SSE has no mode
/// here: translated code runs only while MXCSR.RC is to nearest, see jit::sse_ok.)
#[derive(Clone, Copy)]
pub struct Mode {
    pub cpu: u32,
    pub off: u32,
}

/// The arithmetic the emitters round (wasm f64 add, sub, mul, div)
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ar {
    Add,
    Sub,
    Mul,
    Div,
}

impl Ar {
    pub fn op(self) -> u8 {
        match self {
            Ar::Add => 0xa0,
            Ar::Sub => 0xa1,
            Ar::Mul => 0xa2,
            Ar::Div => 0xa3,
        }
    }
}

const F_ABS: u8 = 0x99;
const F_NEG: u8 = 0x9a;
const F_EQ: u8 = 0x61;
const F_NE: u8 = 0x62;
const F_LT: u8 = 0x63;
const F_GT: u8 = 0x64;
const F_ADD: u8 = 0xa0;
const F_SUB: u8 = 0xa1;
const F_MUL: u8 = 0xa2;
const F_COPYSIGN: u8 = 0xa6;
const I64_BITS: u8 = 0xbd; // i64.reinterpret_f64
const F64_BITS: u8 = 0xbf; // f64.reinterpret_i64
const I64_EQZ: u8 = 0x50;
const I64_EQ: u8 = 0x51;
const I64_LT_S: u8 = 0x53;
const I64_LT_U: u8 = 0x54;
const I64_GT_U: u8 = 0x56;
const I64_ADD: u8 = 0x7c;
const I64_SUB: u8 = 0x7d;
const I64_AND: u8 = 0x83;
const I64_XOR: u8 = 0x85;
const I64_SHR_U: u8 = 0x88;
const SELECT: u8 = 0x1b;

/// 2^29: the f64 bits below a 24-bit significand
const G24: i64 = 1 << 29;

impl Mode {
    fn load(self, c: &mut Code) {
        c.get(self.cpu).load16u(self.off);
    }

    /// [] -> [i32]: whether arithmetic must be rounded again (RC not nearest, or PC single:
    /// 24 bits; PC 64 is koji's 53)
    pub fn arith_slow(self, c: &mut Code) {
        self.load(c);
        c.i32c(0xf00).and().i32c(0x200).sub().u32c(0x200).ge_u();
    }

    /// [] -> [i32]: whether a rounding to f32 isn't to nearest
    pub fn rc_slow(self, c: &mut Code) {
        self.load(c);
        c.i32c(0xc00).and();
    }

    /// In the slow path: the rounding control into t.rc, the granule of the precision into
    /// t.g (`single`: f32 results; else the precision control's), t.bad 0
    pub fn setup(self, c: &mut Code, t: Tmp, single: bool) {
        self.load(c);
        c.i32c(10).shr_u().i32c(3).and().set(t.rc());
        if single {
            c.i64c(G24);
        } else {
            // PC 00: 24 bits, else 53
            c.i64c(G24).i64c(1);
            self.load(c);
            c.i32c(0x300).and().eqz().op(SELECT);
        }
        c.set(t.g());
        c.i32c(0).set(t.bad());
    }
}

/// After [`Mode::setup`]: `out` (holding x op y to nearest, f64) rounded as t says; t.bad when
/// the interpreter must do it
pub fn arith(c: &mut Code, t: Tmp, op: Ar, x: u32, y: u32, out: u32) {
    let yy = t.f(1);
    // (sub: x + -y)
    c.get(y);
    if op == Ar::Sub {
        c.op(F_NEG);
    }
    c.set(yy);
    special(c, t, out);
    c.if_();
    {
        // r 0, subnormal, infinite or NaN
        match op {
            Ar::Add | Ar::Sub => {
                // an exact zero sum: -0 rounding down (but -0 + -0 stays): -((-x) + (-y))
                c.get(out).f64c(0.0).op(F_EQ).get(t.rc()).i32c(1).eq().and().if_();
                c.get(x).op(F_NEG).get(yy).op(F_NEG).op(F_ADD).op(F_NEG).set(out);
                c.end();
                // subnormal sums are exact (but not 24 bits, nor f32); overflow
                c.get(out).f64c(0.0).op(F_NE);
                finite(c, out);
                c.and().get(t.g()).i64c(1).op(0x52).and(); // i64.ne
                finite(c, out);
                c.eqz();
                finite(c, x);
                c.and();
                finite(c, y);
                c.and().or().get(t.bad()).or().set(t.bad());
            }
            Ar::Mul | Ar::Div => {
                // exact when an operand is 0 or not finite; else underflow or overflow
                c.get(x).f64c(0.0).op(F_EQ).get(y).f64c(0.0).op(F_EQ).or();
                finite(c, x);
                c.eqz().or();
                finite(c, y);
                c.eqz().or().eqz().get(t.bad()).or().set(t.bad());
            }
        }
    }
    c.else_();
    {
        error(c, t, op, x, yy, out);
        regrid(c, t, out);
    }
    c.end();
}

/// After [`Mode::setup`] (single): the f64 local `x` rounded to an f32 value into `out`;
/// t.bad for values that become subnormal or overflow in f32
pub fn to_f32(c: &mut Code, t: Tmp, x: u32, out: u32) {
    c.get(x).set(out);
    c.f64c(0.0).set(t.f(0)); // (e = 0: x is exact)
    // zeros, infinities and NaNs as they are; subnormals to the interpreter
    special(c, t, out);
    c.if_();
    c.get(out).op(F_ABS).f64c(0.0).op(F_GT).get(out).op(F_ABS).f64c(f64::INFINITY).op(F_LT).and().get(t.bad()).or().set(t.bad());
    c.else_();
    regrid(c, t, out);
    f32_range(c, t, out);
    c.end();
}

/// [] -> [i32]: whether the f64 local `r` is 0, subnormal, infinite or NaN (leaves its bits
/// in t.rb)
fn special(c: &mut Code, t: Tmp, r: u32) {
    c.get(r).op(I64_BITS).set(t.rb());
    c.get(t.rb()).i64c(52).op(I64_SHR_U).i64c(0x7ff).op(I64_AND).set(t.tt());
    c.get(t.tt()).op(I64_EQZ).get(t.tt()).i64c(0x7ff).op(I64_EQ).or();
}

/// [] -> [i32]: whether the f64 local `v` is finite
fn finite(c: &mut Code, v: u32) {
    c.get(v).op(F_ABS).f64c(f64::INFINITY).op(F_LT);
}


/// The exact error (exact - r) of r = x op y into t.f(0) (its sign is what counts); t.bad set
/// where Dekker's product can't be exact (`y`: -y for Sub)
fn error(c: &mut Code, t: Tmp, op: Ar, x: u32, y: u32, r: u32) {
    let e = t.f(0);
    match op {
        Ar::Add | Ar::Sub => {
            // TwoSum: bb = r - x; e = (x - (r - bb)) + (y - bb)
            let bb = t.f(2);
            c.get(r).get(x).op(F_SUB).set(bb);
            c.get(x).get(r).get(bb).op(F_SUB).op(F_SUB).get(y).get(bb).op(F_SUB).op(F_ADD).set(e);
        }
        Ar::Mul => {
            let big = |c: &mut Code, v: u32| {
                c.get(v).op(F_ABS).f64c(libm::ldexp(1.0, 995)).op(F_GT);
            };
            big(c, x);
            big(c, y);
            c.or().get(r).op(F_ABS).f64c(libm::ldexp(1.0, -968)).op(F_LT).or().get(t.bad()).or().set(t.bad());
            two_product_error(c, t, x, y, r, e);
        }
        Ar::Div => {
            // q = r: the residual x - q*y = (x - ph) - pl, its sign times y's
            let big = |c: &mut Code, v: u32| {
                c.get(v).op(F_ABS).f64c(libm::ldexp(1.0, 995)).op(F_GT);
            };
            big(c, r);
            big(c, y);
            c.or().get(x).op(F_ABS).f64c(libm::ldexp(1.0, -968)).op(F_LT).or().get(t.bad()).or().set(t.bad());
            let (ph, pl) = (t.f(2), e);
            c.get(r).get(y).op(F_MUL).set(ph);
            two_product_error(c, t, r, y, ph, pl);
            c.get(x).get(ph).op(F_SUB).get(pl).op(F_SUB).set(e);
            c.get(e).get(e).op(F_NEG).get(y).f64c(0.0).op(F_GT).op(SELECT).set(e);
        }
    }
}

/// Dekker: the error of p = fl(a * b) into `out` (exact without overflow and underflow)
fn two_product_error(c: &mut Code, t: Tmp, a: u32, b: u32, p: u32, out: u32) {
    let (ah, al, bh, bl) = (t.f(3), t.f(4), t.f(5), out);
    let split = |c: &mut Code, v: u32, h: u32| {
        // c = v * (2^27 + 1); h = c - (c - v)
        c.get(v).f64c(134217729.0).op(F_MUL).tee(h).get(h).get(v).op(F_SUB).op(F_SUB).set(h);
    };
    split(c, a, ah);
    c.get(a).get(ah).op(F_SUB).set(al);
    split(c, b, bh);
    c.get(b).get(bh).op(F_SUB).set(bl);
    // ((ah*bh - p) + ah*bl + al*bh) + al*bl
    c.get(ah).get(bh).op(F_MUL).get(p).op(F_SUB);
    c.get(ah).get(bl).op(F_MUL).op(F_ADD);
    c.get(al).get(bh).op(F_MUL).op(F_ADD);
    c.get(al).get(bl).op(F_MUL).op(F_ADD).set(out);
}

/// The f64 local `r` (normal, finite) plus the error t.f(0) rounded to the granule t.g of its
/// bits (1: 53 bits, 2^29: 24 bits) as t.rc says, into `r`
fn regrid(c: &mut Code, t: Tmp, r: u32) {
    let e = t.f(0);
    // pos / neg: the exact value is past r away from 0 / toward 0
    c.get(e).f64c(0.0).op(F_NE).get(e).get(r).op(F_COPYSIGN).get(e).op(F_EQ).and().set(t.pos());
    c.get(e).f64c(0.0).op(F_NE).get(t.pos()).eqz().and().set(t.neg());
    c.get(r).op(I64_BITS).set(t.rb());
    // tt: r's bits cut to the granule
    c.get(t.rb()).get(t.g()).i64c(1).op(I64_SUB).i64c(-1).op(I64_XOR).op(I64_AND).set(t.tt());
    let low = |c: &mut Code| {
        c.get(t.rb()).get(t.tt()).op(I64_SUB);
    };
    let half = |c: &mut Code| {
        c.get(t.g()).i64c(1).op(0x87); // i64.shr_s
    };
    let up = |c: &mut Code| {
        c.get(t.tt()).get(t.g()).op(I64_ADD);
    };
    // to nearest: past half up; below it (or 53 bits: r is the nearest) down; at it by the
    // error, else to even. [up, tt, up, tt, tt, up, even] select neg select pos select ...
    up(c);
    c.get(t.tt());
    up(c);
    c.get(t.tt());
    c.get(t.tt());
    up(c);
    c.get(t.tt()).get(t.g()).op(I64_AND).op(I64_EQZ).op(SELECT); // even ? tt : up
    c.get(t.neg()).op(SELECT);
    c.get(t.pos()).op(SELECT); // the tie's
    low(c);
    half(c);
    c.op(I64_LT_U).get(t.g()).i64c(1).op(I64_EQ).or().op(SELECT);
    low(c);
    half(c);
    c.op(I64_GT_U).op(SELECT); // nearest
    // away from 0: r itself when on the grid and the exact value not past it, else up
    c.get(t.tt());
    up(c);
    low(c);
    c.op(I64_EQZ).get(t.pos()).eqz().and().op(SELECT);
    // toward 0: one granule down when r is on the grid and the exact value below it
    c.get(t.tt()).get(t.g()).op(I64_SUB);
    c.get(t.tt());
    low(c);
    c.op(I64_EQZ).get(t.neg()).and().op(SELECT);
    // [nearest, away, toward]: away rounding down a negative or up a positive
    c.get(t.rc()).i32c(1).eq().get(t.rb()).i64c(0).op(I64_LT_S).and();
    c.get(t.rc()).i32c(2).eq().get(t.rb()).i64c(0).op(0x59).and(); // i64.ge_s
    c.or().op(SELECT);
    c.get(t.rc()).eqz().op(SELECT);
    c.op(F64_BITS).set(r);
}

/// t.bad when the f64 local `r` (24 bits) is not a normal f32
fn f32_range(c: &mut Code, t: Tmp, r: u32) {
    c.get(r).op(F_ABS).f64c(libm::ldexp(1.0, 128)).op(0x66); // f64.ge
    c.get(r).op(F_ABS).f64c(libm::ldexp(1.0, -126)).op(F_LT).get(r).f64c(0.0).op(F_NE).and();
    c.or().get(t.bad()).or().set(t.bad());
}
