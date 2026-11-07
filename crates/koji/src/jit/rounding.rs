//! Rounding modes in translated code. wasm rounds to nearest (and x87 arithmetic is f64 here):
//! an operation runs as wasm has it while the x87 control word / MXCSR asks for that, else
//! through the module's `round` import ([`crate::round::jit`]), so both ways leave the same
//! translation state behind (the check costs a load and a branch).

use super::wasm::{Code, F64, V128};
pub use crate::round::kind;

/// Where the mode is: the Cpu (a local) and the field's offset
#[derive(Clone, Copy)]
pub struct Mode {
    pub f: u32,
    pub cpu: u32,
    pub off: u32,
    /// the x87 control word (16 bits), else MXCSR
    pub cw: bool,
}

impl Mode {
    fn load(self, c: &mut Code) {
        c.get(self.cpu);
        if self.cw {
            c.load16u(self.off);
        } else {
            c.load32(self.off);
        }
    }

    /// [] -> [i32]: whether arithmetic needs the import (x87: RC not nearest or PC single
    /// (24 bits); SSE: RC not nearest)
    pub fn arith_slow(self, c: &mut Code) {
        self.load(c);
        if self.cw {
            c.i32c(0xf00).and().i32c(0x200).sub().u32c(0x200).ge_u();
        } else {
            c.i32c(0x6000).and();
        }
    }

    /// [] -> [i32]: whether a conversion to f32 (or from i64) needs the import: RC not nearest
    pub fn rc_slow(self, c: &mut Code) {
        self.load(c);
        c.i32c(if self.cw { 0xc00 } else { 0x6000 }).and();
    }

    /// [kind, a, b] -> [f64]
    pub fn call(self, c: &mut Code) {
        self.load(c);
        c.call(self.f);
    }

    /// x87 arithmetic `x op y` of f64 locals into local `out`: [] -> []. `op`: the wasm opcode
    /// (add, sub, mul, div), `k` the import's kind. (Products and quotients below f64's normal
    /// range go to the import too: the x87 rounds them twice, see round::x87.)
    pub fn x87_op(self, c: &mut Code, k: u32, op: u8, x: u32, y: u32, out: u32) {
        c.get(x).get(y).op(op).set(out);
        self.arith_slow(c);
        if matches!(op, 0xa2 | 0xa3) {
            c.get(out).op(0x99).f64c(f64::MIN_POSITIVE).op(0x63).or(); // f64.abs, f64.lt
        }
        c.if_();
        c.u32c(k).get(x).get(y);
        self.call(c);
        c.set(out);
        c.end();
    }

    /// Lanes 0..n of v128 locals `x` op `y` (f32 lanes when `single`), the other lanes of `x`:
    /// [] -> [v128]. `fast` emits the usual code ([] -> [v128]).
    pub fn lanes(self, c: &mut Code, k: u32, unary_op: bool, x: u32, y: u32, n: u8, single: bool, fast: impl FnOnce(&mut Code)) {
        self.arith_slow(c);
        c.if_ty(V128);
        c.get(x);
        for i in 0..n {
            c.u32c(k);
            // (unary: the operand first)
            for v in if unary_op { [Some(y), None] } else { [Some(x), Some(y)] } {
                match v {
                    None => c.f64c(0.0),
                    Some(v) if single => c.get(v).lane(0x1f, i).op(0xbb), // f32x4.extract_lane, f64.promote_f32
                    Some(v) => c.get(v).lane(0x21, i),                     // f64x2.extract_lane
                };
            }
            self.call(c);
            if single {
                c.op(0xb6).lane(0x20, i); // f32.demote_f64, f32x4.replace_lane
            } else {
                c.lane(0x22, i);
            }
        }
        c.else_();
        fast(c);
        c.end();
    }
}

impl Mode {
    /// The f64 local `x` as f32 (rounded as RC says): [] -> [f32]
    pub fn to_f32(self, c: &mut Code, k: u32, x: u32) {
        self.rc_slow(c);
        c.if_ty(F64).u32c(k).get(x).f64c(0.0);
        self.call(c);
        c.else_().get(x).end();
        c.op(0xb6); // f32.demote_f64 (exact after the import)
    }

    /// Lanes 0..n of v128 local `x` (i32 lanes, else f64) converted to f32 lanes 0..n (as RC
    /// says), the other lanes 0: [] -> [v128]. `fast` emits the usual code.
    pub fn f32_lanes(self, c: &mut Code, x: u32, n: u8, ints: bool, fast: impl FnOnce(&mut Code)) {
        self.rc_slow(c);
        c.if_ty(V128);
        c.v128zero();
        for i in 0..n {
            c.u32c(kind::F32);
            if ints {
                c.get(x).lane(0x1b, i).op(0xb7); // i32x4.extract_lane, f64.convert_i32_s (exact)
            } else {
                c.get(x).lane(0x21, i);
            }
            c.f64c(0.0);
            self.call(c);
            c.op(0xb6).lane(0x20, i);
        }
        c.else_();
        fast(c);
        c.end();
    }
}

/// The import's kind for a wasm scalar opcode (f32 or f64 add/sub/mul/div/sqrt)
pub fn scalar_kind(op: u8) -> u32 {
    match op {
        0x92 => kind::SS,
        0x93 => kind::SS + 1,
        0x94 => kind::SS + 2,
        0x95 => kind::SS + 3,
        0x91 => kind::SS + 4,
        0xa0 => kind::SD,
        0xa1 => kind::SD + 1,
        0xa2 => kind::SD + 2,
        0xa3 => kind::SD + 3,
        _ => kind::SD + 4,
    }
}

/// The same for a wasm vector opcode (f32x4/f64x2 add/sub/mul/div/sqrt)
pub fn vector_kind(op: u32) -> u32 {
    match op {
        0xe4 => kind::SS,
        0xe5 => kind::SS + 1,
        0xe6 => kind::SS + 2,
        0xe7 => kind::SS + 3,
        0xe3 => kind::SS + 4,
        0xf0 => kind::SD,
        0xf1 => kind::SD + 1,
        0xf2 => kind::SD + 2,
        0xf3 => kind::SD + 3,
        _ => kind::SD + 4,
    }
}
