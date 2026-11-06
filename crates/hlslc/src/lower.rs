//! The syntax tree to shader model 4/5 instructions (sm4.rs). Every function call is inlined;
//! values are lists of components (a register's component or an immediate), so swizzles,
//! struct members, matrix layouts and constant buffer offsets cost no code.

use std::collections::HashMap;

use super::ast::*;
use super::sm4::{self, Dst, Ins, Reg, RegF, SigEl, Src};

type R<T> = Result<T, String>;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum C {
    R(Reg, u8),
    I(u32),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Obj {
    None,
    Tex(u32),
    Samp(u32),
    /// sampler2D: texture and sampler
    Legacy(u32, u32),
}

#[derive(Clone, Debug)]
pub struct Val {
    pub ty: Ty,
    pub c: Vec<C>,
    pub obj: Obj,
}

impl Val {
    pub fn num(ty: Ty, c: Vec<C>) -> Val {
        Val { ty, c, obj: Obj::None }
    }
}

pub struct Opts {
    pub pixel: bool,
    pub major: u32,
    pub minor: u32,
    pub row_major: bool,
    /// D3DCOMPILE_IEEE_STRICTNESS: without it isnan() is false, as fxc folds it
    pub ieee: bool,
}

struct TexD {
    name: String,
    dim: Dim,
    el: Ty,
    reg: Option<u32>,
    used: bool,
}

struct SampD {
    name: String,
    cmp: bool,
    reg: Option<u32>,
    used: bool,
}

struct CbD {
    name: String,
    reg: Option<u32>,
    size: u32,
    used: bool,
    dynamic: bool,
    vars: Vec<sm4::CbVar>,
}

struct FnCtx {
    ret: Vec<C>,
    ret_ty: Ty,
    flag: Option<u32>,
    loops: u32,
    /// entry point: (where each output's value is, its output registers) and the return
    /// value's output registers
    entry: Option<(Vec<(Vec<C>, Vec<C>)>, Vec<C>)>,
}

pub struct L<'a> {
    pub(crate) o: Opts,
    prog: &'a Program,
    prelude: &'a Program,
    code: Vec<Ins>,
    next: u32,
    xs: Vec<u32>,
    icb: Vec<[u32; 4]>,
    marks: Vec<(u32, usize)>,
    scopes: Vec<HashMap<String, Val>>,
    globals: HashMap<String, Val>,
    /// global name -> (cbuffer, variable)
    cbvar: HashMap<String, (usize, usize)>,
    texs: Vec<TexD>,
    samps: Vec<SampD>,
    cbs: Vec<CbD>,
    inputs: Vec<SigEl>,
    outputs: Vec<SigEl>,
    fns: Vec<FnCtx>,
    /// places picked by a run-time index from a select chain: (the elements, the index, the
    /// copy written to), stored back once the write is done
    wbs: Vec<(Vec<Vec<C>>, C, Vec<C>, B)>,
}

const TRUE: u32 = 0xffff_ffff;
const ONE_F: u32 = 0x3f80_0000;

fn fbits(f: f32) -> u32 {
    f.to_bits()
}

fn flt(v: u32) -> f32 {
    f32::from_bits(v)
}

pub fn ncomp(prog: &Program, t: &Ty) -> usize {
    match t {
        Ty::Array(e, n) => ncomp(prog, e) * *n as usize,
        Ty::Struct(i) => prog.structs[*i].fields.iter().map(|f| ncomp(prog, &f.ty)).sum(),
        t => t.comps(),
    }
}

/// The base type of each component.
fn leaf_bases(prog: &Program, t: &Ty, out: &mut Vec<B>) {
    match t {
        Ty::Array(e, n) => {
            for _ in 0..*n {
                leaf_bases(prog, e, out);
            }
        }
        Ty::Struct(i) => {
            for f in &prog.structs[*i].fields {
                leaf_bases(prog, &f.ty, out);
            }
        }
        t => {
            for _ in 0..t.comps() {
                out.push(t.base());
            }
        }
    }
}

/// Register-sized units (each a vector or a matrix row) of a type, in component order.
fn units(prog: &Program, t: &Ty, out: &mut Vec<usize>) {
    match t {
        Ty::Array(e, n) => {
            for _ in 0..*n {
                units(prog, e, out);
            }
        }
        Ty::Struct(i) => {
            for f in &prog.structs[*i].fields {
                units(prog, &f.ty, out);
            }
        }
        Ty::Mat(_, r, c) => {
            for _ in 0..*r {
                out.push(*c as usize);
            }
        }
        t if t.comps() > 0 => out.push(t.comps()),
        _ => {}
    }
}

fn split_sem(s: &str) -> (String, u32) {
    let d = s.trim_end_matches(|c: char| c.is_ascii_digit());
    let idx = s[d.len()..].parse().unwrap_or(0);
    (d.to_string(), idx)
}

fn pixel_out(pixel: bool, output: bool) -> bool {
    pixel && output
}

fn temp(t: u32) -> Reg {
    Reg { f: RegF::Temp(t), rel: None }
}

fn ctype(b: B) -> u32 {
    match b {
        B::Float => 3,
        B::Int => 2,
        _ => 1,
    }
}

fn swizzle_of(name: &str, n: usize) -> Option<Vec<usize>> {
    if name.is_empty() || name.len() > 4 {
        return None;
    }
    let set = |s: &str| name.chars().map(|c| s.find(c)).collect::<Option<Vec<usize>>>();
    let v = set("xyzw").or_else(|| set("rgba"))?;
    if v.iter().any(|&i| i >= n.max(1)) {
        return None;
    }
    Some(v)
}

/// _11_22 / _m00_m11: (row, column) pairs
fn mat_swizzle(name: &str, r: usize, c: usize) -> Option<Vec<(usize, usize)>> {
    let mut out = Vec::new();
    let mut s = name;
    while !s.is_empty() {
        let rest = s.strip_prefix('_')?;
        let (a, b, n) = if let Some(m) = rest.strip_prefix('m') {
            let b = m.as_bytes();
            if b.len() < 2 {
                return None;
            }
            ((b[0] as char).to_digit(10)? as usize, (b[1] as char).to_digit(10)? as usize, 3)
        } else {
            let b = rest.as_bytes();
            if b.len() < 2 {
                return None;
            }
            ((b[0] as char).to_digit(10)? as usize - 1, (b[1] as char).to_digit(10)? as usize - 1, 2)
        };
        if a >= r || b >= c {
            return None;
        }
        out.push((a, b));
        s = &rest[n..];
    }
    Some(out)
}

fn has_return(s: &Stmt) -> bool {
    match s {
        Stmt::Return(..) => true,
        Stmt::Block(v) => v.iter().any(has_return),
        Stmt::If(_, a, b) => has_return(a) || b.as_deref().is_some_and(has_return),
        Stmt::For(_, _, _, b) | Stmt::While(_, b) | Stmt::Do(b, _) => has_return(b),
        Stmt::Switch(_, cs) => cs.iter().any(|c| c.1.iter().any(has_return)),
        _ => false,
    }
}

fn has_continue(s: &Stmt) -> bool {
    match s {
        Stmt::Continue => true,
        Stmt::Block(v) => v.iter().any(has_continue),
        Stmt::If(_, a, b) => has_continue(a) || b.as_deref().is_some_and(has_continue),
        Stmt::Switch(_, cs) => cs.iter().any(|c| c.1.iter().any(has_continue)),
        _ => false,
    }
}

fn expr_writes(e: &Expr, name: &str, outs: &dyn Fn(&str) -> bool) -> bool {
    let root = |mut t: &Expr| loop {
        match t {
            Expr::Var(n, _) => return n == name,
            Expr::Member(a, _) | Expr::Index(a, _) => t = a,
            _ => return false,
        }
    };
    match e {
        Expr::Assign(_, a, b) => root(a) || expr_writes(a, name, outs) || expr_writes(b, name, outs),
        Expr::Un(Un::PreInc | Un::PreDec | Un::PostInc | Un::PostDec, a) => root(a),
        Expr::Bin(_, a, b) | Expr::Comma(a, b) | Expr::Index(a, b) => expr_writes(a, name, outs) || expr_writes(b, name, outs),
        Expr::Un(_, a) | Expr::Cast(_, a) | Expr::Member(a, _) => expr_writes(a, name, outs),
        Expr::Cond(a, b, c) => expr_writes(a, name, outs) || expr_writes(b, name, outs) || expr_writes(c, name, outs),
        // (out arguments: user functions and the intrinsics with out parameters)
        Expr::Call(f, args, _) => {
            let o = !super::intrinsics::NO_OUT.contains(&f.as_str()) && outs(f);
            args.iter().any(|a| expr_writes(a, name, outs) || (o && root(a)))
        }
        Expr::Construct(_, args) | Expr::List(args) => args.iter().any(|a| expr_writes(a, name, outs)),
        Expr::Method(o, m, args) => expr_writes(o, name, outs) || args.iter().any(|a| expr_writes(a, name, outs) || (m == "GetDimensions" && root(a))),
        _ => false,
    }
}

fn writes(s: &Stmt, name: &str, outs: &dyn Fn(&str) -> bool) -> bool {
    match s {
        Stmt::Expr(e) => expr_writes(e, name, outs),
        Stmt::Decl(vs) => vs.iter().any(|v| v.init.as_ref().is_some_and(|e| expr_writes(e, name, outs))),
        Stmt::Block(v) => v.iter().any(|s| writes(s, name, outs)),
        Stmt::If(c, a, b) => expr_writes(c, name, outs) || writes(a, name, outs) || b.as_deref().is_some_and(|b| writes(b, name, outs)),
        Stmt::For(i, c, st, b) => {
            i.as_deref().is_some_and(|i| writes(i, name, outs))
                || c.as_ref().is_some_and(|c| expr_writes(c, name, outs))
                || st.as_ref().is_some_and(|c| expr_writes(c, name, outs))
                || writes(b, name, outs)
        }
        Stmt::While(c, b) | Stmt::Do(b, c) => expr_writes(c, name, outs) || writes(b, name, outs),
        Stmt::Switch(c, cs) => expr_writes(c, name, outs) || cs.iter().any(|c| c.1.iter().any(|s| writes(s, name, outs))),
        Stmt::Return(Some(e), _) => expr_writes(e, name, outs),
        _ => false,
    }
}

/// Constant folding of an elementwise operation on immediates.
fn fold(op: u32, a: &[u32], tru: u32) -> Option<u32> {
    use sm4::*;
    let f = |i: usize| flt(a[i]);
    let i = |k: usize| a[k] as i32;
    let m = |b: bool| if b { tru } else { 0 };
    Some(match op {
        MOV => a[0],
        ADD => fbits(f(0) + f(1)),
        MUL => fbits(f(0) * f(1)),
        DIV => fbits(f(0) / f(1)),
        MAD => fbits(f(0) * f(1) + f(2)),
        MIN => fbits(f(0).min(f(1))),
        MAX => fbits(f(0).max(f(1))),
        IADD => a[0].wrapping_add(a[1]),
        IMAX => i(0).max(i(1)) as u32,
        IMIN => i(0).min(i(1)) as u32,
        UMAX => a[0].max(a[1]),
        UMIN => a[0].min(a[1]),
        AND => a[0] & a[1],
        OR => a[0] | a[1],
        XOR => a[0] ^ a[1],
        NOT => !a[0],
        INEG => a[0].wrapping_neg(),
        ISHL => a[0].wrapping_shl(a[1] & 31),
        ISHR => (i(0) >> (a[1] & 31)) as u32,
        USHR => a[0] >> (a[1] & 31),
        ITOF => fbits(i(0) as f32),
        UTOF => fbits(a[0] as f32),
        FTOI => f(0) as i32 as u32,
        FTOU => f(0) as u32,
        EQ => m(f(0) == f(1)),
        NE => m(f(0) != f(1)),
        LT => m(f(0) < f(1)),
        GE => m(f(0) >= f(1)),
        IEQ => m(a[0] == a[1]),
        INE => m(a[0] != a[1]),
        ILT => m(i(0) < i(1)),
        IGE => m(i(0) >= i(1)),
        ULT => m(a[0] < a[1]),
        UGE => m(a[0] >= a[1]),
        MOVC => {
            if a[0] != 0 {
                a[1]
            } else {
                a[2]
            }
        }
        FRC => fbits(f(0) - f(0).floor()),
        ROUND_NI => fbits(f(0).floor()),
        ROUND_PI => fbits(f(0).ceil()),
        ROUND_Z => fbits(f(0).trunc()),
        ROUND_NE => fbits(f(0).round_ties_even()),
        SQRT => fbits(f(0).sqrt()),
        RSQ => fbits(1.0 / f(0).sqrt()),
        EXP => fbits(f(0).exp2()),
        LOG => fbits(f(0).log2()),
        _ => return None,
    })
}

fn neg_imm(v: u32, b: B) -> u32 {
    if b == B::Float { v ^ 0x8000_0000 } else { v.wrapping_neg() }
}

impl<'a> L<'a> {
    pub fn new(o: Opts, prog: &'a Program, prelude: &'a Program) -> Self {
        L {
            o,
            prog,
            prelude,
            code: Vec::new(),
            next: 0,
            xs: Vec::new(),
            icb: Vec::new(),
            marks: Vec::new(),
            wbs: Vec::new(),
            scopes: Vec::new(),
            globals: HashMap::new(),
            cbvar: HashMap::new(),
            texs: Vec::new(),
            samps: Vec::new(),
            cbs: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            fns: Vec::new(),
        }
    }

    /// shader model 2/3 output: ints and bools are floats (true 1.0), no bit operations
    fn sm3(&self) -> bool {
        self.o.major < 4
    }
    fn tru(&self) -> u32 {
        if self.sm3() {
            ONE_F
        } else {
            TRUE
        }
    }
    /// an int constant
    fn ival(&self, v: i64) -> u32 {
        if self.sm3() {
            fbits(v as f32)
        } else {
            v as u32
        }
    }

    /// a user function of this name takes an out parameter
    fn has_out(&self, f: &str) -> bool {
        self.prog.funcs.iter().any(|x| x.name == f && x.params.iter().any(|p| p.is_out)) || matches!(f, "sincos" | "modf")
    }

    fn st(&self, i: usize) -> &Struct {
        &self.prog.structs[i]
    }

    // ---- emission ----

    fn tmp(&mut self) -> u32 {
        self.next += 1;
        self.next - 1
    }

    fn emit(&mut self, op: u32, dst: Vec<Dst>, src: Vec<Src>) -> &mut Ins {
        self.code.push(Ins { op, sat: false, nz: false, ctl: 0, dst, src, offset: [0; 3] });
        self.code.last_mut().unwrap()
    }

    fn flow(&mut self, op: u32) {
        self.emit(op, vec![], vec![]);
    }

    /// if_nz / if_z (and breakc, discard...) on one component
    fn cond_op(&mut self, op: u32, c: C, nz: bool) {
        let s = self.scalar_src(c);
        self.emit(op, vec![], vec![s]).nz = nz;
    }

    fn scalar_src(&mut self, c: C) -> Src {
        match c {
            C::I(v) => Src::Imm([v; 4], true),
            C::R(r, k) => Src::R { r, swz: [k; 4], neg: false, abs: false, one: true },
        }
    }

    /// A source operand of up to 4 components (one register or immediates; others are gathered
    /// into a temp first).
    fn opnd(&mut self, cs: &[C]) -> Src {
        let n = cs.len();
        if cs.iter().all(|c| matches!(c, C::I(_))) {
            let mut v = [0u32; 4];
            for k in 0..4 {
                if let C::I(x) = cs[k.min(n - 1)] {
                    v[k] = x;
                }
            }
            return Src::Imm(v, false);
        }
        if let C::R(r0, _) = cs[0] {
            if cs.iter().all(|c| matches!(c, C::R(r, _) if *r == r0)) {
                let mut swz = [0u8; 4];
                for (k, s) in swz.iter_mut().enumerate() {
                    if let C::R(_, c) = cs[k.min(n - 1)] {
                        *s = c;
                    }
                }
                return Src::R { r: r0, swz, neg: false, abs: false, one: false };
            }
        }
        let t = self.tmp();
        let dst: Vec<C> = (0..n).map(|k| C::R(temp(t), k as u8)).collect();
        self.assign_raw(&dst, cs);
        Src::R { r: temp(t), swz: [0, 1.min(n as u8 - 1), 2.min(n as u8 - 1), 3.min(n as u8 - 1)], neg: false, abs: false, one: false }
    }

    /// dst comps <- src comps, movs grouped by register (no overlap check)
    fn assign_raw(&mut self, dst: &[C], src: &[C]) {
        self.assign_op(sm4::MOV, dst, src, false);
    }

    fn assign_op(&mut self, op: u32, dst: &[C], src: &[C], sat: bool) {
        // group destination comps by register
        let mut groups: Vec<(Reg, Vec<(u8, C)>)> = Vec::new();
        for (d, s) in dst.iter().zip(src) {
            let C::R(r, c) = *d else { continue };
            match groups.iter_mut().find(|g| g.0 == r) {
                Some(g) => g.1.push((c, *s)),
                None => groups.push((r, vec![(c, *s)])),
            }
        }
        for (r, comps) in groups {
            // then by source register
            let mut sub: Vec<(Option<Reg>, Vec<(u8, C)>)> = Vec::new();
            for (dc, s) in comps {
                let key = match s {
                    C::R(r, _) => Some(r),
                    C::I(_) => None,
                };
                match sub.iter_mut().find(|g| g.0 == key) {
                    Some(g) => g.1.push((dc, s)),
                    None => sub.push((key, vec![(dc, s)])),
                }
            }
            for (key, items) in sub {
                let mask = items.iter().fold(0u8, |m, x| m | 1 << x.0);
                let s = match key {
                    None => {
                        let mut v = [0u32; 4];
                        for (dc, s) in &items {
                            if let C::I(x) = s {
                                v[*dc as usize] = *x;
                            }
                        }
                        Src::Imm(v, false)
                    }
                    Some(sr) => {
                        let mut swz = [0u8; 4];
                        for (dc, s) in &items {
                            if let C::R(_, c) = s {
                                swz[*dc as usize] = *c;
                            }
                        }
                        Src::R { r: sr, swz, neg: false, abs: false, one: false }
                    }
                };
                self.emit(op, vec![Dst { r, mask }], vec![s]).sat = sat;
            }
        }
    }

    /// dst <- src, safe when they share registers
    fn assign(&mut self, dst: &[C], src: &[C]) {
        let dregs: Vec<Reg> = dst.iter().filter_map(|c| if let C::R(r, _) = c { Some(*r) } else { None }).collect();
        let clash = src.iter().any(|c| matches!(c, C::R(r, _) if dregs.contains(r)));
        if clash {
            let copy = self.copy(src);
            self.assign_raw(dst, &copy);
        } else {
            self.assign_raw(dst, src);
        }
    }

    /// A fresh copy of comps in temps.
    fn copy(&mut self, src: &[C]) -> Vec<C> {
        let mut out = Vec::new();
        for ch in src.chunks(4) {
            let t = self.tmp();
            let d: Vec<C> = (0..ch.len()).map(|k| C::R(temp(t), k as u8)).collect();
            self.assign_raw(&d, ch);
            out.extend(d);
        }
        out
    }

    /// An elementwise operation over comps (args the same length); modifiers per argument
    /// (1 negate, 2 absolute value).
    fn alu_m(&mut self, op: u32, b: B, args: &[(&[C], u8)]) -> Vec<C> {
        let mut owned: Vec<(&[C], u8)> = args.to_vec();
        let (mut op, mut b) = (op, b);
        if self.sm3() {
            // (integer operations on float values)
            use sm4::*;
            op = match op {
                IADD => ADD,
                IMAD => MAD,
                IMUL | UMUL => MUL,
                IEQ => EQ,
                INE => NE,
                ILT | ULT => LT,
                IGE | UGE => GE,
                IMAX | UMAX => MAX,
                IMIN | UMIN => MIN,
                ITOF | UTOF => MOV,
                FTOI | FTOU => ROUND_Z,
                // (of bools)
                AND => MUL,
                OR => MAX,
                INEG => {
                    owned[0].1 ^= 1;
                    MOV
                }
                x => x,
            };
            b = B::Float;
        }
        let args = &owned[..];
        let n = args[0].0.len();
        let foldable = !self.sm3() || super::sm3::float_op(op);
        if foldable && args.iter().all(|a| a.0.iter().all(|c| matches!(c, C::I(_)))) {
            let mut out = Vec::new();
            let mut ok = true;
            for k in 0..n {
                let vals: Vec<u32> = args
                    .iter()
                    .map(|a| {
                        let C::I(v) = a.0[k] else { unreachable!() };
                        let v = if a.1 & 2 != 0 { if b == B::Float { v & 0x7fff_ffff } else { (v as i32).unsigned_abs() } } else { v };
                        if a.1 & 1 != 0 { neg_imm(v, b) } else { v }
                    })
                    .collect();
                match fold(op, &vals, self.tru()) {
                    Some(v) => out.push(C::I(v)),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                return out;
            }
        }
        let mut out = Vec::new();
        for st in (0..n).step_by(4) {
            let m = (n - st).min(4);
            let t = self.tmp();
            let mut srcs = Vec::new();
            for (a, md) in args {
                let mut s = self.opnd(&a[st..st + m]);
                if *md != 0 {
                    match &mut s {
                        Src::R { neg, abs, .. } => {
                            *neg = md & 1 != 0;
                            *abs = md & 2 != 0;
                        }
                        Src::Imm(v, _) => {
                            for x in v.iter_mut() {
                                if md & 2 != 0 {
                                    *x = if b == B::Float { *x & 0x7fff_ffff } else { (*x as i32).unsigned_abs() };
                                }
                                if md & 1 != 0 {
                                    *x = neg_imm(*x, b);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                srcs.push(s);
            }
            self.emit(op, vec![Dst { r: temp(t), mask: (1u8 << m) - 1 }], srcs);
            out.extend((0..m).map(|k| C::R(temp(t), k as u8)));
        }
        out
    }

    fn alu(&mut self, op: u32, b: B, args: &[&[C]]) -> Vec<C> {
        let a: Vec<(&[C], u8)> = args.iter().map(|x| (*x, 0u8)).collect();
        self.alu_m(op, b, &a)
    }

    /// An operation with two results (imul/udiv: hi/quotient first): which one to keep.
    fn alu2(&mut self, op: u32, args: &[&[C]], second: bool) -> Vec<C> {
        use sm4::*;
        if self.sm3() {
            match op {
                IMUL | UMUL => return self.alu(MUL, B::Float, args),
                UDIV => {
                    let q = self.alu(DIV, B::Float, args);
                    let tq = self.alu(ROUND_Z, B::Float, &[&q]);
                    if !second {
                        return tq;
                    }
                    return self.alu_m(MAD, B::Float, &[(&tq, 1), (args[1], 0), (args[0], 0)]);
                }
                _ => {}
            }
        }
        let n = args[0].len();
        let mut out = Vec::new();
        for st in (0..n).step_by(4) {
            let m = (n - st).min(4);
            let t = self.tmp();
            let srcs: Vec<Src> = args.iter().map(|a| self.opnd(&a[st..st + m])).collect();
            let d = Dst { r: temp(t), mask: (1u8 << m) - 1 };
            let null = Dst { r: Reg { f: RegF::Null, rel: None }, mask: 0 };
            let dst = if second { vec![null, d] } else { vec![d, null] };
            self.emit(op, dst, srcs);
            out.extend((0..m).map(|k| C::R(temp(t), k as u8)));
        }
        out
    }

    fn imm(v: u32, n: usize) -> Vec<C> {
        vec![C::I(v); n]
    }

    // ---- types and conversions ----

    fn conv_base(&mut self, cs: &[C], from: B, to: B) -> Vec<C> {
        use sm4::*;
        if from == to || (matches!(from, B::Int | B::Uint) && matches!(to, B::Int | B::Uint)) {
            return cs.to_vec();
        }
        let n = cs.len();
        if self.sm3() {
            return match (from, to) {
                (B::Float, B::Int | B::Uint) => self.alu(ROUND_Z, to, &[cs]),
                (_, B::Bool) => self.alu(NE, to, &[cs, &Self::imm(0, n)]),
                _ => cs.to_vec(),
            };
        }
        match (from, to) {
            (B::Float, B::Int) => self.alu(FTOI, to, &[cs]),
            (B::Float, B::Uint) => self.alu(FTOU, to, &[cs]),
            (B::Int, B::Float) => self.alu(ITOF, to, &[cs]),
            (B::Uint, B::Float) => self.alu(UTOF, to, &[cs]),
            (B::Bool, B::Float) => self.alu(AND, to, &[cs, &Self::imm(ONE_F, n)]),
            (B::Bool, _) => self.alu(AND, to, &[cs, &Self::imm(1, n)]),
            (B::Float, B::Bool) => self.alu(NE, to, &[cs, &Self::imm(0, n)]),
            (_, B::Bool) => self.alu(INE, to, &[cs, &Self::imm(0, n)]),
            _ => cs.to_vec(),
        }
    }

    fn cast(&mut self, v: Val, to: &Ty) -> R<Val> {
        if &v.ty == to {
            return Ok(v);
        }
        if v.ty.is_object() || to.is_object() {
            return Err(format!("cannot convert {:?} to {:?}", v.ty, to));
        }
        let n = v.c.len();
        let m = ncomp(self.prog, to);
        let mut tb = Vec::new();
        leaf_bases(self.prog, to, &mut tb);
        // shape
        let comps: Vec<C> = if n == m {
            v.c.clone()
        } else if n == 1 {
            vec![v.c[0]; m]
        } else if let (Ty::Mat(_, r1, c1), Ty::Mat(_, r2, c2)) = (&v.ty, to) {
            if r2 > r1 || c2 > c1 {
                return Err(format!("cannot convert {:?} to {:?}", v.ty, to));
            }
            let mut o = Vec::new();
            for r in 0..*r2 as usize {
                for c in 0..*c2 as usize {
                    o.push(v.c[r * *c1 as usize + c]);
                }
            }
            o
        } else if m < n && v.ty.numeric() && to.numeric() {
            v.c[..m].to_vec()
        } else {
            return Err(format!("cannot convert {:?} to {:?}", v.ty, to));
        };
        let src = Val::num(if n == 1 && m != 1 { v.ty.clone() } else { v.ty.clone() }, comps);
        // per-comp bases of the source
        let mut fb = Vec::new();
        leaf_bases(self.prog, &v.ty, &mut fb);
        let mut out = Vec::new();
        for (k, c) in src.c.iter().enumerate() {
            let f = if n == 1 { fb[0] } else if n == m { fb[k] } else { fb.get(k).copied().unwrap_or(B::Float) };
            out.extend(self.conv_base(&[*c], f, tb[k]));
        }
        Ok(Val::num(to.clone(), out))
    }

    /// The type two operands of an arithmetic operator meet in.
    fn common(&self, a: &Ty, b: &Ty) -> R<Ty> {
        let base = if a.base() == B::Float || b.base() == B::Float {
            B::Float
        } else if a.base() == B::Uint || b.base() == B::Uint {
            B::Uint
        } else {
            B::Int
        };
        Ok(match (a, b) {
            (Ty::Scalar(_), t) | (t, Ty::Scalar(_)) => t.with_base(base),
            (Ty::Vec(_, n), Ty::Vec(_, m)) => Ty::vec(base, (*n).min(*m) as usize),
            (Ty::Mat(_, r, c), Ty::Mat(_, r2, c2)) => Ty::Mat(base, (*r).min(*r2), (*c).min(*c2)),
            (Ty::Vec(_, n), Ty::Mat(_, r, c)) | (Ty::Mat(_, r, c), Ty::Vec(_, n)) if *n as usize == (*r * *c) as usize => {
                Ty::vec(base, *n as usize)
            }
            _ => return Err(format!("operands of types {a:?} and {b:?}")),
        })
    }

    // ---- storage ----

    /// New storage for a variable of type `t`: temps, arrays in an indexable temp.
    fn storage(&mut self, t: &Ty) -> Vec<C> {
        match t {
            Ty::Array(e, n) if self.sm3() => {
                // (no indexable temps: elements in temps)
                let mut out = Vec::new();
                for _ in 0..*n {
                    out.extend(self.storage(e));
                }
                out
            }
            Ty::Array(..) => {
                let mut u = Vec::new();
                units(self.prog, t, &mut u);
                let id = self.xs.len() as u32;
                self.xs.push(u.len().max(1) as u32);
                let mut out = Vec::new();
                for (k, n) in u.iter().enumerate() {
                    for c in 0..*n {
                        out.push(C::R(Reg { f: RegF::X(id, k as u32), rel: None }, c as u8));
                    }
                }
                out
            }
            Ty::Struct(i) => {
                let fields: Vec<Ty> = self.st(*i).fields.iter().map(|f| f.ty.clone()).collect();
                let mut out = Vec::new();
                for f in fields {
                    out.extend(self.storage(&f));
                }
                out
            }
            Ty::Mat(_, r, c) => {
                let mut out = Vec::new();
                for _ in 0..*r {
                    let t = self.tmp();
                    self.marks.push((t, self.code.len()));
                    out.extend((0..*c).map(|k| C::R(temp(t), k)));
                }
                out
            }
            t => {
                let n = t.comps();
                if n == 0 {
                    return Vec::new();
                }
                let t = self.tmp();
                self.marks.push((t, self.code.len()));
                (0..n).map(|k| C::R(temp(t), k as u8)).collect()
            }
        }
    }

    fn lookup(&mut self, name: &str, line: u32) -> R<Val> {
        for s in self.scopes.iter().rev() {
            if let Some(v) = s.get(name) {
                return Ok(v.clone());
            }
        }
        if let Some(&(cb, var)) = self.cbvar.get(name) {
            self.cbs[cb].used = true;
            self.cbs[cb].vars[var].used = true;
        }
        match self.globals.get(name).cloned() {
            Some(v) => {
                match v.obj {
                    Obj::Tex(t) => self.texs[t as usize].used = true,
                    Obj::Samp(s) => self.samps[s as usize].used = true,
                    Obj::Legacy(t, s) => {
                        self.texs[t as usize].used = true;
                        self.samps[s as usize].used = true;
                    }
                    Obj::None => {}
                }
                // (bools of constant buffers are 0/1: canonical masks)
                if self.cbvar.contains_key(name) && v.ty.base() == B::Bool && v.ty.numeric() {
                    let c = self.alu(sm4::INE, B::Bool, &[&v.c, &Self::imm(0, v.c.len())]);
                    return Ok(Val::num(v.ty, c));
                }
                Ok(v)
            }
            None => Err(format!("line {line}: undeclared identifier '{name}'")),
        }
    }

    // ---- globals ----

    /// Constant buffer layout of a variable of `t` at byte offset `at`: (comps, end offset).
    fn cb_layout(&self, t: &Ty, row_major: bool, mut at: u32, sym: u32, out: &mut Vec<C>) -> u32 {
        let reg = |r: u32| Reg { f: RegF::Cb(sym, r), rel: None };
        match t {
            Ty::Scalar(_) | Ty::Vec(..) => {
                let n = t.comps() as u32;
                // (shader model 2/3: each in registers of its own)
                if at % 16 + 4 * n > 16 || self.sm3() {
                    at = at.div_ceil(16) * 16;
                }
                for k in 0..n {
                    let o = at + 4 * k;
                    out.push(C::R(reg(o / 16), ((o % 16) / 4) as u8));
                }
                if self.sm3() {
                    at + 16
                } else {
                    at + 4 * n
                }
            }
            Ty::Mat(_, r, c) => {
                at = at.div_ceil(16) * 16;
                let (r, c) = (*r as u32, *c as u32);
                for i in 0..r {
                    for j in 0..c {
                        let (rr, cc) = if row_major { (i, j) } else { (j, i) };
                        out.push(C::R(reg(at / 16 + rr), cc as u8));
                    }
                }
                let end = if row_major { at + (r - 1) * 16 + c * 4 } else { at + (c - 1) * 16 + r * 4 };
                if self.sm3() { end.div_ceil(16) * 16 } else { end }
            }
            Ty::Array(e, n) => {
                at = at.div_ceil(16) * 16;
                let mut end = at;
                for k in 0..*n {
                    let st = at + k * 16 * self.cb_regs(e, row_major);
                    end = self.cb_layout(e, row_major, st, sym, out);
                }
                end
            }
            Ty::Struct(i) => {
                at = at.div_ceil(16) * 16;
                for f in &self.prog.structs[*i].fields {
                    at = self.cb_layout(&f.ty, f.row_major.unwrap_or(row_major), at, sym, out);
                }
                at
            }
            _ => at,
        }
    }

    /// registers one element of `t` takes in a constant buffer array
    fn cb_regs(&self, t: &Ty, row_major: bool) -> u32 {
        let mut v = Vec::new();
        let end = self.cb_layout(t, row_major, 0, 0, &mut v);
        end.div_ceil(16)
    }

    fn rdef_var(&self, name: &str, t: &Ty, row_major: bool, off: u32, end: u32) -> sm4::CbVar {
        let el = if let Ty::Array(e, _) = t { &**e } else { t };
        let mut members = Vec::new();
        if let Ty::Struct(i) = el {
            let mut at = 0u32;
            for f in &self.prog.structs[*i].fields {
                let rm = f.row_major.unwrap_or(row_major);
                let mut v = Vec::new();
                let st = if self.sm3() { at.div_ceil(16) * 16 } else { at };
                let e = self.cb_layout(&f.ty, rm, st, 0, &mut v);
                let first = match v.first() {
                    Some(C::R(Reg { f: RegF::Cb(_, r), .. }, c)) => r * 16 + *c as u32 * 4,
                    _ => st,
                };
                members.push(self.rdef_var(&f.name, &f.ty, rm, first, e));
                at = e;
            }
        }
        let mut v = self.rdef_var0(name, t, row_major, off, end);
        v.members = members;
        v
    }

    fn rdef_var0(&self, name: &str, t: &Ty, row_major: bool, off: u32, end: u32) -> sm4::CbVar {
        let (el, elems) = match t {
            Ty::Array(e, n) => (&**e, *n as u16),
            t => (t, 0),
        };
        let ty = match el.base() {
            B::Bool => 1,
            B::Int => 2,
            B::Uint => 19,
            B::Float => 3,
        };
        let (class, rows, cols) = match el {
            Ty::Scalar(_) => (0, 1, 1),
            Ty::Vec(_, n) => (1, 1, *n as u16),
            Ty::Mat(_, r, c) => (if row_major { 2 } else { 3 }, *r as u16, *c as u16),
            _ => (5, 1, 1),
        };
        sm4::CbVar {
            name: name.into(),
            offset: off,
            size: end - off,
            class,
            ty: if class == 5 { 0 } else { ty },
            rows,
            cols,
            elems,
            used: false,
            explicit: false,
            members: Vec::new(),
        }
    }

    fn add_cbuffer(&mut self, name: &str, reg: Option<u32>, vars: &[Var]) -> R<()> {
        let sym = self.cbs.len() as u32;
        let mut at = 0;
        let mut rvars = Vec::new();
        for v in vars {
            if v.ty().is_object() {
                self.add_object(v)?;
                continue;
            }
            let rm = v.row_major.unwrap_or(self.o.row_major);
            let start = match (v.packoffset, v.reg) {
                (Some(p), _) => p,
                (None, Some(('c', r))) => r * 16,
                _ => at,
            };
            let mut comps = Vec::new();
            let end = self.cb_layout(v.ty(), rm, start, sym, &mut comps);
            let first = match comps.first() {
                Some(C::R(Reg { f: RegF::Cb(_, r), .. }, c)) => r * 16 + *c as u32 * 4,
                _ => start,
            };
            let mut rv = self.rdef_var(&v.name, v.ty(), rm, first, end);
            rv.explicit = matches!(v.reg, Some(('c', _)));
            rvars.push(rv);
            self.cbvar.insert(v.name.clone(), (sym as usize, rvars.len() - 1));
            self.globals.insert(v.name.clone(), Val::num(v.ty().clone(), comps));
            at = at.max(end);
        }
        self.cbs.push(CbD { name: name.into(), reg, size: at.div_ceil(16), used: false, dynamic: false, vars: rvars });
        Ok(())
    }

    fn add_object(&mut self, v: &Var) -> R<()> {
        let reg = v.reg.map(|r| r.1);
        let obj = match v.ty() {
            Ty::Tex(d, el) => {
                self.texs.push(TexD { name: v.name.clone(), dim: *d, el: (**el).clone(), reg, used: false });
                Obj::Tex(self.texs.len() as u32 - 1)
            }
            Ty::Sampler(cmp) => {
                self.samps.push(SampD { name: v.name.clone(), cmp: *cmp, reg, used: false });
                Obj::Samp(self.samps.len() as u32 - 1)
            }
            Ty::Legacy(d) => {
                self.texs.push(TexD { name: v.name.clone(), dim: *d, el: Ty::Vec(B::Float, 4), reg, used: false });
                self.samps.push(SampD { name: v.name.clone(), cmp: false, reg, used: false });
                Obj::Legacy(self.texs.len() as u32 - 1, self.samps.len() as u32 - 1)
            }
            Ty::Array(..) => return Err(format!("line {}: arrays of textures or samplers are not supported", v.line)),
            _ => return Ok(()),
        };
        self.globals.insert(v.name.clone(), Val { ty: v.ty().clone(), c: Vec::new(), obj });
        Ok(())
    }

    /// Initial value of a declared variable (lists flattened).
    fn init_value(&mut self, t: &Ty, e: &Expr) -> R<Val> {
        if let Expr::List(items) = e {
            let mut flat = Vec::new();
            self.flatten_list(items, &mut flat)?;
            let mut tb = Vec::new();
            leaf_bases(self.prog, t, &mut tb);
            let mut comps = Vec::new();
            for v in &flat {
                let mut fb = Vec::new();
                leaf_bases(self.prog, &v.ty, &mut fb);
                for (k, c) in v.c.iter().enumerate() {
                    let at = comps.len();
                    let to = tb.get(at).copied().unwrap_or(fb[k]);
                    comps.extend(self.conv_base(&[*c], fb[k], to));
                }
            }
            if comps.len() < tb.len() {
                return Err(format!("initializer has {} components, {} needed", comps.len(), tb.len()));
            }
            comps.truncate(tb.len());
            return Ok(Val::num(t.clone(), comps));
        }
        let v = self.expr(e)?;
        self.cast(v, t)
    }

    fn flatten_list(&mut self, items: &[Expr], out: &mut Vec<Val>) -> R<()> {
        for i in items {
            if let Expr::List(v) = i {
                self.flatten_list(v, out)?;
            } else {
                out.push(self.expr(i)?);
            }
        }
        Ok(())
    }

    /// Globals: constant buffers, objects, statics (initialized here, at the start of the entry).
    fn setup_globals(&mut self, extra_uniforms: &[Var]) -> R<()> {
        let prog = self.prog;
        let mut uniforms: Vec<Var> = Vec::new();
        let mut statics = Vec::new();
        for v in &prog.globals {
            if v.ty().is_object() {
                self.add_object(v)?;
            } else if matches!(v.ty(), Ty::Str) {
            } else if v.is_static {
                statics.push(v.clone());
            } else {
                uniforms.push(v.clone());
            }
        }
        uniforms.extend(extra_uniforms.iter().cloned());
        self.add_cbuffer("$Globals", None, &uniforms)?;
        for cb in &prog.cbufs {
            self.add_cbuffer(&cb.name, cb.reg, &cb.vars)?;
        }
        for v in statics {
            let t = v.ty().clone();
            let val = match &v.init {
                Some(e) => Some(self.init_value(&t, e)?),
                None => None,
            };
            let all_imm = val.as_ref().is_some_and(|x| x.c.iter().all(|c| matches!(c, C::I(_))));
            if v.is_const && all_imm {
                let val = val.unwrap();
                if let Ty::Array(..) = t {
                    // an immediate constant buffer
                    let mut u = Vec::new();
                    units(self.prog, &t, &mut u);
                    let base = self.icb.len() as u32;
                    let mut comps = Vec::new();
                    let mut k = 0;
                    for (n, w) in u.iter().enumerate() {
                        let mut row = [0u32; 4];
                        for (c, r) in row.iter_mut().enumerate().take(*w) {
                            if let C::I(x) = val.c[k] {
                                *r = x;
                            }
                            comps.push(C::R(Reg { f: RegF::Icb(base + n as u32), rel: None }, c as u8));
                            k += 1;
                        }
                        self.icb.push(row);
                    }
                    self.globals.insert(v.name.clone(), Val::num(t, comps));
                } else {
                    self.globals.insert(v.name.clone(), val);
                }
                continue;
            }
            let s = self.storage(&t);
            let init = match val {
                Some(x) => x.c,
                None => vec![C::I(0); s.len()],
            };
            self.assign(&s, &init);
            self.globals.insert(v.name.clone(), Val::num(t, s));
        }
        Ok(())
    }

    // ---- the entry point's signature ----

    /// Signature elements of `t` with semantic `sem` (struct members their own): register comps
    /// in component order.
    fn sig(&mut self, t: &Ty, sem: Option<&str>, interp: Interp, output: bool, out: &mut Vec<C>) -> R<()> {
        if let Ty::Struct(i) = t {
            let fields = self.st(*i).fields.clone();
            for f in &fields {
                self.sig(&f.ty, f.sem.as_deref(), f.interp, output, out)?;
            }
            return Ok(());
        }
        let Some(sem) = sem else {
            return Err(format!("missing semantic on a shader {}", if output { "output" } else { "input" }));
        };
        let (name0, mut index) = split_sem(sem);
        let up = name0.to_ascii_uppercase();
        // (8 render targets, 4 in shader model 2/3; 32 input/output registers)
        let max = if matches!(up.as_str(), "SV_TARGET" | "COLOR") && pixel_out(self.o.pixel, output) {
            if self.sm3() { 4 } else { 8 }
        } else {
            32
        };
        if index >= max {
            return Err(format!("semantic {sem}: index out of range"));
        }
        let pixel = self.o.pixel;
        // shader model 2/3 names (backward compatibility)
        let (name, sysval) = match (up.as_str(), pixel, output) {
            // (shader model 2/3 pixel shaders: POSITION is not VPOS, not readable)
            ("POSITION", true, false) if self.sm3() => ("POSITION".to_string(), 0),
            ("SV_POSITION", _, _) => ("SV_Position".to_string(), 1),
            ("POSITION", false, true) | ("POSITION", true, false) | ("VPOS", true, false) | ("POSITIONT", false, true) => ("SV_Position".to_string(), 1),
            // (in the signature as D3D_NAME_TARGET / DEPTH; declared as plain outputs)
            ("SV_TARGET", true, true) | ("COLOR", true, true) => ("SV_Target".to_string(), 64),
            ("SV_DEPTH", true, true) | ("DEPTH", true, true) => ("SV_Depth".to_string(), 65),
            ("SV_VERTEXID", false, false) => ("SV_VertexID".to_string(), 6),
            ("SV_INSTANCEID", false, false) => ("SV_InstanceID".to_string(), 8),
            ("SV_ISFRONTFACE", true, false) | ("VFACE", true, false) => ("SV_IsFrontFace".to_string(), 9),
            ("SV_PRIMITIVEID", true, false) => ("SV_PrimitiveID".to_string(), 7),
            ("SV_SAMPLEINDEX", true, false) => ("SV_SampleIndex".to_string(), 10),
            ("SV_CLIPDISTANCE", _, _) => ("SV_ClipDistance".to_string(), 2),
            _ => (name0.clone(), 0),
        };
        let mut u = Vec::new();
        units(self.prog, t, &mut u);
        if name == "POSITION" && pixel && !output && self.sm3() {
            // (unreadable: zeros)
            out.extend(std::iter::repeat_n(C::I(0), u.iter().sum()));
            return Ok(());
        }
        let mut bases = Vec::new();
        leaf_bases(self.prog, t, &mut bases);
        let b = bases.first().copied().unwrap_or(B::Float);
        let mut k = 0;
        for n in u {
            let list = if output { &mut self.outputs } else { &mut self.inputs };
            let depth = name == "SV_Depth";
            let reg = if name == "SV_Target" {
                index
            } else {
                list.iter().filter(|e| !e.depth && e.name != "SV_Target").map(|e| e.reg + 1).max().unwrap_or(0).max(
                    list.iter().filter(|e| e.name == "SV_Target").map(|e| e.reg + 1).max().unwrap_or(0),
                )
            };
            let interp = if !pixel || output {
                0
            } else if sysval == 1 {
                4
            } else if b != B::Float || sysval != 0 {
                1
            } else {
                match interp {
                    Interp::Linear => 2,
                    Interp::Centroid => 3,
                    Interp::NoInterp => 1,
                    Interp::NoPersp => 4,
                    Interp::NoPerspCentroid => 5,
                    Interp::Sample => 6,
                }
            };
            let ct = if sysval == 9 { 1 } else { ctype(b) };
            list.push(SigEl { name: name.clone(), index, sysval, ctype: ct, reg, mask: ((1u16 << n) - 1) as u8, interp, depth });
            let f = if depth {
                RegF::Depth
            } else if output {
                RegF::Output(reg)
            } else {
                RegF::Input(reg)
            };
            for c in 0..n {
                out.push(C::R(Reg { f, rel: None }, c as u8));
            }
            k += n;
            index += 1;
        }
        let _ = k;
        Ok(())
    }

    // ---- entry ----

    /// The entry point `name`; `args` (an effect's compile statement) give its first uniform
    /// parameters (or ones without a semantic) their values.
    pub fn entry(mut self, name: &str, args: &[Expr]) -> R<sm4::Shader> {
        let prog = self.prog;
        let f = prog.funcs.iter().find(|f| f.name == name && f.body.is_some()).ok_or(format!("entry point '{name}' not found"))?;
        let bindable = |p: &&Var| p.uniform || (p.sem.is_none() && !p.is_out && !matches!(p.ty(), Ty::Struct(_)));
        let bound: Vec<&Var> = f.params.iter().filter(bindable).take(args.len()).collect();
        if bound.len() < args.len() {
            return Err(format!("{name}: {} arguments for {} uniform parameters", args.len(), bound.len()));
        }
        let is_bound = |p: &Var| bound.iter().any(|b| b.name == p.name);
        let uniforms: Vec<Var> = f.params.iter().filter(|p| p.uniform && !is_bound(p)).cloned().collect();
        self.scopes.push(HashMap::new());
        self.setup_globals(&uniforms)?;
        for (p, a) in bound.iter().zip(args) {
            let v = self.expr(a)?;
            let v = self.cast(v, p.ty())?;
            self.scopes.last_mut().unwrap().insert(p.name.clone(), v);
        }
        let body = f.body.as_ref().unwrap();
        let mut outs = Vec::new();
        for p in f.params.iter().filter(|p| !p.uniform && !is_bound(p)) {
            let t = p.ty().clone();
            if p.is_out {
                let mut regs = Vec::new();
                self.sig(&t, p.sem.as_deref(), p.interp, true, &mut regs)?;
                let s = self.storage(&t);
                if p.is_in {
                    let mut iregs = Vec::new();
                    self.sig(&t, p.sem.as_deref(), p.interp, false, &mut iregs)?;
                    self.assign(&s, &iregs);
                }
                outs.push((s.clone(), regs));
                self.scopes.last_mut().unwrap().insert(p.name.clone(), Val::num(t, s));
            } else {
                let mut regs = Vec::new();
                self.sig(&t, p.sem.as_deref(), p.interp, false, &mut regs)?;
                let v = if body.iter().any(|s| writes(s, &p.name, &|f| self.has_out(f))) || matches!(t, Ty::Array(..)) {
                    let s = self.storage(&t);
                    self.assign(&s, &regs);
                    s
                } else {
                    regs
                };
                self.scopes.last_mut().unwrap().insert(p.name.clone(), Val::num(t, v));
            }
        }
        let mut ret_regs = Vec::new();
        if f.ret != Ty::Void {
            self.sig(&f.ret.clone(), f.ret_sem.as_deref(), Interp::Linear, true, &mut ret_regs)?;
        }
        if self.sm3() {
            // (no ret before the end: returns as in an inlined function, outputs at the end)
            let ret = if f.ret == Ty::Void { Vec::new() } else { self.storage(&f.ret) };
            let early = body.iter().enumerate().any(|(i, s)| has_return(s) && (i + 1 < body.len() || !matches!(s, Stmt::Return(..))));
            let flag = if early {
                let t = self.tmp();
                self.marks.push((t, self.code.len()));
                self.assign(&[C::R(temp(t), 0)], &[C::I(0)]);
                Some(t)
            } else {
                None
            };
            self.fns.push(FnCtx { ret: ret.clone(), ret_ty: f.ret.clone(), flag, loops: 0, entry: Some((outs, ret_regs.clone())) });
            self.stmts(body)?;
            self.assign(&ret_regs, &ret);
            self.write_outputs()?;
            return self.finish();
        }
        self.fns.push(FnCtx { ret: Vec::new(), ret_ty: f.ret.clone(), flag: None, loops: 0, entry: Some((outs, ret_regs)) });
        self.stmts(body)?;
        if f.ret == Ty::Void {
            self.write_outputs()?;
        }
        if self.code.last().map(|i| i.op) != Some(sm4::RET) {
            self.flow(sm4::RET);
        }
        self.finish()
    }

    fn write_outputs(&mut self) -> R<()> {
        let (outs, _) = self.fns.last().unwrap().entry.clone().unwrap();
        for (s, regs) in outs {
            self.assign(&regs, &s);
        }
        Ok(())
    }

    fn finish(self) -> R<sm4::Shader> {
        // resource slots: explicit registers first, then the lowest free in declaration order
        let mut taken: Vec<u32> = self.texs.iter().filter(|t| t.used).filter_map(|t| t.reg).collect();
        let mut tex_map = vec![None; self.texs.len()];
        let mut texs = Vec::new();
        let mut legacy_slot: HashMap<usize, u32> = HashMap::new();
        for (n, t) in self.texs.iter().enumerate() {
            if !t.used {
                continue;
            }
            let slot = t.reg.unwrap_or_else(|| {
                let mut s = 0;
                while taken.contains(&s) {
                    s += 1;
                }
                taken.push(s);
                s
            });
            legacy_slot.insert(n, slot);
            let dim = match t.dim {
                Dim::Buffer => 1,
                Dim::T1D => 2,
                Dim::T2D => 3,
                Dim::T2DMS => 4,
                Dim::T3D => 5,
                Dim::Cube => 6,
                Dim::T1DArray => 7,
                Dim::T2DArray => 8,
                Dim::T2DMSArray => 9,
                Dim::CubeArray => 10,
            };
            let ret = match t.el.base() {
                B::Int => 3,
                B::Uint | B::Bool => 4,
                B::Float => 5,
            };
            tex_map[n] = Some(texs.len());
            texs.push(sm4::Tex { name: t.name.clone(), dim, ret, comps: t.el.comps().max(1) as u32, slot });
        }
        let mut staken: Vec<u32> = self.samps.iter().filter(|t| t.used).filter_map(|t| t.reg).collect();
        let mut samp_map = vec![None; self.samps.len()];
        let mut samps = Vec::new();
        for (n, s) in self.samps.iter().enumerate() {
            if !s.used {
                continue;
            }
            // (a sampler2D: the slot of its texture)
            let legacy = self.globals.values().find_map(|v| match v.obj {
                Obj::Legacy(t, sm) if sm as usize == n => legacy_slot.get(&(t as usize)).copied(),
                _ => None,
            });
            let slot = s.reg.or(legacy).unwrap_or_else(|| {
                let mut k = 0;
                while staken.contains(&k) {
                    k += 1;
                }
                k
            });
            staken.push(slot);
            samp_map[n] = Some(samps.len());
            samps.push(sm4::Samp { name: s.name.clone(), cmp: s.cmp, slot });
        }
        let mut ctaken: Vec<u32> = self.cbs.iter().filter(|c| c.used).filter_map(|c| c.reg).collect();
        let mut cb_map = vec![None; self.cbs.len()];
        let mut cbs = Vec::new();
        for (n, c) in self.cbs.into_iter().enumerate() {
            if !c.used {
                continue;
            }
            let slot = c.reg.unwrap_or_else(|| {
                let mut k = 0;
                while ctaken.contains(&k) {
                    k += 1;
                }
                ctaken.push(k);
                k
            });
            cb_map[n] = Some(cbs.len());
            cbs.push(sm4::Cb { name: c.name, slot, size: c.size, dynamic: c.dynamic, vars: c.vars });
        }
        Ok(sm4::Shader {
            pixel: self.o.pixel,
            major: self.o.major,
            minor: self.o.minor,
            code: self.code,
            inputs: self.inputs,
            outputs: self.outputs,
            cbs,
            texs,
            samps,
            xs: self.xs,
            icb: self.icb,
            marks: self.marks,
            cb_map,
            tex_map,
            samp_map,
            early_depth: false,
        })
    }

    // ---- statements ----

    fn fctx(&mut self) -> &mut FnCtx {
        self.fns.last_mut().unwrap()
    }

    fn stmts(&mut self, ss: &[Stmt]) -> R<()> {
        for (i, s) in ss.iter().enumerate() {
            self.stmt(s)?;
            if let Some(f) = self.fctx().flag {
                if has_return(s) && i + 1 < ss.len() {
                    self.cond_op(sm4::IF, C::R(temp(f), 0), false);
                    self.stmts(&ss[i + 1..])?;
                    self.flow(sm4::ENDIF);
                    return Ok(());
                }
            }
        }
        Ok(())
    }

    fn scoped(&mut self, s: &Stmt) -> R<()> {
        self.scopes.push(HashMap::new());
        let r = self.stmt(s);
        self.scopes.pop();
        r
    }

    fn truth(&mut self, v: Val) -> R<C> {
        if !v.ty.numeric() {
            return Err("a condition must be a scalar".into());
        }
        let c = v.c[0];
        Ok(match v.ty.base() {
            B::Bool => c,
            B::Float => self.alu(sm4::NE, B::Bool, &[&[c], &[C::I(0)]])[0],
            _ => c,
        })
    }

    /// after a loop or switch whose body returned (inlined function with a flag): leave the
    /// enclosing loop too
    fn after_breakable(&mut self, body_returns: bool) {
        let f = self.fctx();
        if let (Some(flag), true) = (f.flag, body_returns) {
            if f.loops > 0 {
                self.cond_op(sm4::BREAKC, C::R(temp(flag), 0), true);
            }
        }
    }

    fn stmt(&mut self, s: &Stmt) -> R<()> {
        use sm4::*;
        match s {
            Stmt::Expr(e) => {
                self.expr(e)?;
            }
            Stmt::Decl(vs) => {
                for v in vs {
                    let t = v.ty().clone();
                    if t.is_object() {
                        let val = match &v.init {
                            Some(e) => self.expr(e)?,
                            None => return Err(format!("line {}: local texture or sampler without a value", v.line)),
                        };
                        self.scopes.last_mut().unwrap().insert(v.name.clone(), val);
                        continue;
                    }
                    if v.is_static {
                        return Err(format!("line {}: static locals are not supported", v.line));
                    }
                    let init = match &v.init {
                        Some(e) => Some(self.init_value(&t, e)?),
                        None => None,
                    };
                    // (a constant: no storage)
                    if v.is_const && !matches!(t, Ty::Array(..)) {
                        if let Some(i) = &init {
                            if i.c.iter().all(|c| matches!(c, C::I(_))) {
                                self.scopes.last_mut().unwrap().insert(v.name.clone(), i.clone());
                                continue;
                            }
                        }
                    }
                    let st = self.storage(&t);
                    if let Some(i) = init {
                        self.assign(&st, &i.c);
                    }
                    self.scopes.last_mut().unwrap().insert(v.name.clone(), Val::num(t, st));
                }
            }
            Stmt::Block(v) => {
                self.scopes.push(HashMap::new());
                let r = self.stmts(v);
                self.scopes.pop();
                r?;
            }
            Stmt::If(c, a, b) => {
                let cv = self.expr(c)?;
                let t = self.truth(cv)?;
                if let C::I(v) = t {
                    // a constant condition
                    if v != 0 {
                        self.scoped(a)?;
                    } else if let Some(b) = b {
                        self.scoped(b)?;
                    }
                    return Ok(());
                }
                self.cond_op(IF, t, true);
                self.scoped(a)?;
                if let Some(b) = b {
                    self.flow(ELSE);
                    self.scoped(b)?;
                }
                self.flow(ENDIF);
            }
            Stmt::For(init, cond, step, body) => {
                self.scopes.push(HashMap::new());
                if let Some(i) = init {
                    self.stmt(i)?;
                }
                let cont = has_continue(body);
                let first = if cont && step.is_some() {
                    let f = self.tmp();
                    self.marks.push((f, self.code.len()));
                    self.assign(&[C::R(temp(f), 0)], &[C::I(self.tru())]);
                    Some(f)
                } else {
                    None
                };
                self.flow(LOOP);
                self.fctx().loops += 1;
                if let (Some(f), Some(st)) = (first, step) {
                    self.cond_op(IF, C::R(temp(f), 0), false);
                    self.expr(st)?;
                    self.flow(ENDIF);
                    self.assign(&[C::R(temp(f), 0)], &[C::I(0)]);
                }
                if let Some(c) = cond {
                    let cv = self.expr(c)?;
                    let t = self.truth(cv)?;
                    self.cond_op(BREAKC, t, false);
                }
                self.scoped(body)?;
                if let (None, Some(st)) = (first, step) {
                    self.expr(st)?;
                }
                self.fctx().loops -= 1;
                self.flow(ENDLOOP);
                self.scopes.pop();
                self.after_breakable(has_return(body));
            }
            Stmt::While(c, body) => {
                self.flow(LOOP);
                self.fctx().loops += 1;
                let cv = self.expr(c)?;
                let t = self.truth(cv)?;
                self.cond_op(BREAKC, t, false);
                self.scoped(body)?;
                self.fctx().loops -= 1;
                self.flow(ENDLOOP);
                self.after_breakable(has_return(body));
            }
            Stmt::Do(body, c) => {
                let cont = has_continue(body);
                let first = if cont {
                    let f = self.tmp();
                    self.marks.push((f, self.code.len()));
                    self.assign(&[C::R(temp(f), 0)], &[C::I(self.tru())]);
                    Some(f)
                } else {
                    None
                };
                self.flow(LOOP);
                self.fctx().loops += 1;
                if let Some(f) = first {
                    self.cond_op(IF, C::R(temp(f), 0), false);
                    let cv = self.expr(c)?;
                    let t = self.truth(cv)?;
                    self.cond_op(BREAKC, t, false);
                    self.flow(ENDIF);
                    self.assign(&[C::R(temp(f), 0)], &[C::I(0)]);
                }
                self.scoped(body)?;
                if first.is_none() {
                    let cv = self.expr(c)?;
                    let t = self.truth(cv)?;
                    self.cond_op(BREAKC, t, false);
                }
                self.fctx().loops -= 1;
                self.flow(ENDLOOP);
                self.after_breakable(has_return(body));
            }
            Stmt::Switch(v, cases) => {
                let val = self.expr(v)?;
                let val = self.cast(val, &Ty::Scalar(B::Int))?;
                let s = self.scalar_src(val.c[0]);
                let s = match s {
                    Src::Imm(..) => {
                        let c = self.copy(&val.c);
                        self.scalar_src(c[0])
                    }
                    s => s,
                };
                self.emit(SWITCH, vec![], vec![s]);
                self.fctx().loops += 1;
                let mut returns = false;
                for (labels, body) in cases {
                    for l in labels {
                        match l {
                            Some(e) => {
                                let k = super::parse::const_int(e).ok_or("case label is not a constant")?;
                                self.emit(CASE, vec![], vec![Src::Imm([k as u32; 4], true)]);
                            }
                            None => self.flow(DEFAULT),
                        }
                    }
                    self.scopes.push(HashMap::new());
                    self.stmts(body)?;
                    self.scopes.pop();
                    returns |= body.iter().any(has_return);
                    // (a case falling through to the end of the switch)
                    if !matches!(self.code.last().map(|i| i.op), Some(BREAK | RET | CONTINUE)) {
                        self.flow(BREAK);
                    }
                }
                self.fctx().loops -= 1;
                self.flow(ENDSWITCH);
                self.after_breakable(returns);
            }
            Stmt::Break => self.flow(BREAK),
            Stmt::Continue => self.flow(CONTINUE),
            Stmt::Discard => {
                self.emit(DISCARD, vec![], vec![Src::Imm([TRUE; 4], true)]).nz = true;
            }
            Stmt::Return(e, line) => {
                let rt = self.fctx().ret_ty.clone();
                let val = match e {
                    Some(e) if rt != Ty::Void => {
                        let v = self.expr(e)?;
                        Some(self.cast(v, &rt).map_err(|m| format!("line {line}: return value: {m}"))?)
                    }
                    Some(e) => {
                        self.expr(e)?;
                        None
                    }
                    None => None,
                };
                if self.fctx().entry.is_some() && !self.sm3() {
                    let (_, ret_regs) = self.fctx().entry.clone().unwrap();
                    if let Some(v) = val {
                        self.assign(&ret_regs, &v.c);
                    }
                    self.write_outputs()?;
                    self.flow(RET);
                    return Ok(());
                }
                if let Some(v) = val {
                    let r = self.fctx().ret.clone();
                    self.assign(&r, &v.c);
                }
                if let Some(f) = self.fctx().flag {
                    self.assign(&[C::R(temp(f), 0)], &[C::I(self.tru())]);
                    if self.fctx().loops > 0 {
                        self.flow(BREAK);
                    }
                }
            }
        }
        Ok(())
    }

    // ---- expressions ----

    fn lit_int(&self, v: i64, unsigned: bool) -> Val {
        Val::num(Ty::Scalar(if unsigned { B::Uint } else { B::Int }), vec![C::I(self.ival(v))])
    }

    pub fn expr(&mut self, e: &Expr) -> R<Val> {
        use sm4::*;
        Ok(match e {
            Expr::Int(v, u) => self.lit_int(*v, *u),
            Expr::Float(f) => Val::num(Ty::Scalar(B::Float), vec![C::I(fbits(*f as f32))]),
            Expr::Bool(b) => Val::num(Ty::Scalar(B::Bool), vec![C::I(if *b { self.tru() } else { 0 })]),
            Expr::Var(n, line) => self.lookup(n, *line)?,
            Expr::Comma(a, b) => {
                self.expr(a)?;
                self.expr(b)?
            }
            Expr::List(_) => return Err("initializer list in an expression".into()),
            Expr::Str(_) => return Err("a string in an expression".into()),
            Expr::Construct(t, args) => {
                let mut flat = Vec::new();
                for a in args {
                    flat.push(self.expr(a)?);
                }
                let mut tb = Vec::new();
                leaf_bases(self.prog, t, &mut tb);
                let n: usize = flat.iter().map(|v| v.c.len()).sum();
                if n == 1 && tb.len() > 1 {
                    // float4(x): every component x
                    return self.cast(flat.pop().unwrap(), t);
                }
                if n != tb.len() {
                    return Err(format!("constructor of {t:?} with {n} components"));
                }
                let mut comps = Vec::new();
                for v in &flat {
                    let mut fb = Vec::new();
                    leaf_bases(self.prog, &v.ty, &mut fb);
                    for (k, c) in v.c.iter().enumerate() {
                        let to = tb[comps.len()];
                        comps.extend(self.conv_base(&[*c], fb[k], to));
                    }
                }
                Val::num(t.clone(), comps)
            }
            Expr::Cast(t, a) => {
                let v = self.expr(a)?;
                self.cast(v, t)?
            }
            Expr::Un(op, a) => match op {
                Un::Neg => {
                    let v = self.expr(a)?;
                    let b = if v.ty.base() == B::Bool { B::Int } else { v.ty.base() };
                    let v = self.cast(v.clone(), &v.ty.with_base(b))?;
                    let c = if b == B::Float {
                        self.alu_m(MOV, b, &[(&v.c, 1)])
                    } else {
                        self.alu(INEG, b, &[&v.c])
                    };
                    Val::num(v.ty, c)
                }
                Un::Not => {
                    let v = self.expr(a)?;
                    let v = self.cast(v.clone(), &v.ty.with_base(B::Bool))?;
                    let c = self.alu(IEQ, B::Bool, &[&v.c, &Self::imm(0, v.c.len())]);
                    Val::num(v.ty, c)
                }
                Un::BitNot => {
                    if self.sm3() {
                        return Err("bitwise operators need shader model 4".into());
                    }
                    let v = self.expr(a)?;
                    let c = self.alu(NOT, v.ty.base(), &[&v.c]);
                    Val::num(v.ty, c)
                }
                Un::PreInc | Un::PreDec | Un::PostInc | Un::PostDec => {
                    let mark = self.wbs.len();
                    let p = self.place(a)?;
                    let old = self.copy(&p.c);
                    let fl = p.ty.base() == B::Float || self.sm3();
                    let one = if fl { fbits(1.0) } else { 1 };
                    let dec = matches!(op, Un::PreDec | Un::PostDec);
                    let c = if fl {
                        self.alu_m(ADD, B::Float, &[(&old, 0), (&Self::imm(one, old.len()), dec as u8)])
                    } else {
                        let v = if dec { one.wrapping_neg() } else { one };
                        self.alu(IADD, p.ty.base(), &[&old, &Self::imm(v, old.len())])
                    };
                    self.assign(&p.c, &c);
                    self.write_back(mark);
                    if matches!(op, Un::PostInc | Un::PostDec) {
                        Val::num(p.ty, old)
                    } else {
                        Val::num(p.ty, c)
                    }
                }
            },
            Expr::Bin(op, a, b) => {
                let x = self.expr(a)?;
                let y = self.expr(b)?;
                self.binary(*op, x, y)?
            }
            Expr::Assign(op, a, b) => {
                let mark = self.wbs.len();
                let p = self.place(a)?;
                let mut v = self.expr(b)?;
                if let Some(op) = op {
                    let cur = Val::num(p.ty.clone(), p.c.clone());
                    v = self.binary(*op, cur, v)?;
                }
                let v = self.cast(v, &p.ty)?;
                self.assign(&p.c, &v.c);
                self.write_back(mark);
                Val::num(p.ty, p.c)
            }
            Expr::Cond(c, a, b) => {
                let cv = self.expr(c)?;
                let x = self.expr(a)?;
                let y = self.expr(b)?;
                let t = if x.ty.numeric() && y.ty.numeric() { self.common(&x.ty, &y.ty)? } else { x.ty.clone() };
                // (a vector condition picks per component)
                let t = if cv.ty.comps() > 1 && t.comps() == 1 { Ty::vec(t.base(), cv.ty.comps()) } else { t };
                // (a narrower vector condition: the values truncated to it, as fxc does)
                let t = match t {
                    Ty::Vec(b, n) if cv.ty.comps() > 1 && (cv.ty.comps() as u32) < n as u32 => Ty::vec(b, cv.ty.comps()),
                    t => t,
                };
                let x = self.cast(x, &t)?;
                let y = self.cast(y, &t)?;
                let cb = self.cast(cv.clone(), &cv.ty.with_base(B::Bool))?;
                let n = x.c.len();
                let cc: Vec<C> = if cb.c.len() == 1 { vec![cb.c[0]; n] } else { cb.c[..n.min(cb.c.len())].to_vec() };
                if let Some(C::I(v)) = cc.first().filter(|_| cc.iter().all(|c| *c == cc[0])) {
                    return Ok(if *v != 0 { x } else { y });
                }
                let r = self.alu(MOVC, t.base(), &[&cc, &x.c, &y.c]);
                Val::num(t, r)
            }
            Expr::Member(a, name) => {
                let v = self.expr(a)?;
                self.member(v, name)?
            }
            Expr::Index(a, i) => {
                let v = self.expr(a)?;
                let iv = self.expr(i)?;
                self.index(v, iv)?
            }
            Expr::Call(name, args, line) => self.call(name, args, *line)?,
            Expr::Method(o, name, args) => self.method(o, name, args)?,
        })
    }

    /// An lvalue: storage comps.
    fn place(&mut self, e: &Expr) -> R<Val> {
        let v = match e {
            Expr::Var(n, line) => self.lookup(n, *line)?,
            Expr::Member(a, n) => {
                let v = self.place(a)?;
                self.member(v, n)?
            }
            Expr::Index(a, i) => {
                let v = self.place(a)?;
                let mut iv = self.expr(i)?;
                if matches!(iv.c.first(), Some(C::I(_))) || !matches!(v.ty, Ty::Array(..) | Ty::Vec(..) | Ty::Mat(..)) {
                    self.index(v, iv)?
                } else {
                    iv = self.cast(iv, &Ty::Scalar(B::Int))?;
                    let r = self.index(v.clone(), iv.clone())?;
                    // (a register indexed relative to the variable's own storage is the place)
                    let own = |c: &C| match c {
                        C::R(Reg { f, rel: Some(_) }, _) => v.c.iter().any(|d| matches!(d, C::R(Reg { f: g, .. }, _) if g == f)),
                        _ => false,
                    };
                    if !r.c.iter().all(own) {
                        let k = match &v.ty {
                            Ty::Array(e, _) => ncomp(self.prog, e),
                            Ty::Mat(_, _, c) => *c as usize,
                            _ => 1,
                        };
                        let elems: Vec<Vec<C>> = v.c.chunks(k.max(1)).map(|e| e.to_vec()).collect();
                        // (the select may hand back an element itself: write to a copy)
                        let cp = if r.c.iter().any(|c| v.c.contains(c)) { self.copy(&r.c) } else { r.c.clone() };
                        self.wbs.push((elems, iv.c[0], cp.clone(), r.ty.base()));
                        Val::num(r.ty, cp)
                    } else {
                        r
                    }
                }
            }
            _ => return Err("assignment to something that is not a variable".into()),
        };
        if v.c.iter().any(|c| !matches!(c, C::R(Reg { f: RegF::Temp(_) | RegF::Output(_) | RegF::X(..) | RegF::Depth, .. }, _))) {
            return Err("assignment to a constant, a uniform or an input".into());
        }
        Ok(v)
    }

    /// Stores the places picked by a run-time index since `mark` back (innermost first):
    /// element k = index == k ? the copy : element k.
    fn write_back(&mut self, mark: usize) {
        while self.wbs.len() > mark {
            let (elems, iv, cp, b) = self.wbs.pop().unwrap();
            for (k, e) in elems.iter().enumerate() {
                let m = self.alu(sm4::IEQ, B::Bool, &[&[iv], &[C::I(self.ival(k as i64))]])[0];
                let n = e.len();
                let v = self.alu(sm4::MOVC, b, &[&vec![m; n], &cp, e]);
                self.assign(e, &v);
            }
        }
    }

    fn member(&mut self, v: Val, name: &str) -> R<Val> {
        match &v.ty {
            Ty::Scalar(b) | Ty::Vec(b, _) => {
                let n = v.c.len();
                let s = swizzle_of(name, n).ok_or(format!("bad swizzle .{name}"))?;
                let c: Vec<C> = s.iter().map(|&i| v.c[i]).collect();
                Ok(Val::num(Ty::vec(*b, c.len()), c))
            }
            Ty::Mat(b, r, c) => {
                let s = mat_swizzle(name, *r as usize, *c as usize).ok_or(format!("bad matrix member .{name}"))?;
                let comps: Vec<C> = s.iter().map(|&(i, j)| v.c[i * *c as usize + j]).collect();
                Ok(Val::num(Ty::vec(*b, comps.len()), comps))
            }
            Ty::Struct(i) => {
                let mut at = 0;
                for f in &self.prog.structs[*i].fields {
                    let n = ncomp(self.prog, &f.ty);
                    if f.name == name {
                        return Ok(Val::num(f.ty.clone(), v.c[at..at + n].to_vec()));
                    }
                    at += n;
                }
                Err(format!("no member '{name}'"))
            }
            t => Err(format!("member .{name} of {t:?}")),
        }
    }

    /// An index value as an int comp in a temp register.
    fn index_reg(&mut self, iv: Val, stride: u32) -> R<(u32, u8)> {
        let iv = self.cast(iv, &Ty::Scalar(B::Int))?;
        let mut c = iv.c[0];
        if stride > 1 && self.sm3() {
            c = self.alu(sm4::MUL, B::Float, &[&[c], &[C::I(fbits(stride as f32))]])[0];
        } else if stride > 1 {
            c = if stride.is_power_of_two() {
                self.alu(sm4::ISHL, B::Int, &[&[c], &[C::I(stride.trailing_zeros())]])[0]
            } else {
                self.alu2(sm4::IMUL, &[&[c], &[C::I(stride)]], true)[0]
            };
        }
        if let C::R(Reg { f: RegF::Temp(t), rel: None }, k) = c {
            return Ok((t, k));
        }
        let cp = self.copy(&[c]);
        let C::R(Reg { f: RegF::Temp(t), .. }, k) = cp[0] else { unreachable!() };
        Ok((t, k))
    }

    /// element `i` of elements (each a comp list): constant index, a register with a relative
    /// index, or a select chain
    fn select(&mut self, elems: Vec<Vec<C>>, iv: Val, ty: Ty) -> R<Val> {
        if let Some(C::I(k)) = iv.c.first() {
            let k = if iv.ty.base() == B::Float || self.sm3() { flt(*k) as i64 } else { *k as i32 as i64 };
            let e = elems.get(k as usize).ok_or(format!("index {k} out of range"))?;
            return Ok(Val::num(ty, e.clone()));
        }
        // a regular layout: element k = element 0 with register + k * stride
        let addressable = |c: &C| matches!(c, C::R(Reg { f: RegF::X(..) | RegF::Cb(..) | RegF::Icb(_), rel: None }, _));
        let regno = |c: &C| match c {
            C::R(Reg { f: RegF::X(_, r) | RegF::Cb(_, r) | RegF::Icb(r), .. }, _) => *r,
            _ => 0,
        };
        let e0 = &elems[0];
        // (shader model 3 pixel shaders: no address register)
        if !e0.is_empty() && e0.iter().all(addressable) && !(self.sm3() && self.o.pixel) {
            let stride = if elems.len() > 1 { regno(&elems[1][0]).wrapping_sub(regno(&e0[0])) } else { 1 };
            let regular = elems.iter().enumerate().all(|(k, e)| {
                e.len() == e0.len()
                    && e.iter().zip(e0).all(|(a, b)| match (a, b) {
                        (C::R(ra, ca), C::R(rb, cb)) => {
                            ca == cb
                                && regno(a) == regno(b).wrapping_add(stride.wrapping_mul(k as u32))
                                && std::mem::discriminant(&ra.f) == std::mem::discriminant(&rb.f)
                        }
                        _ => false,
                    })
            });
            if regular && stride > 0 && stride < 4096 {
                let rel = self.index_reg(iv, stride)?;
                let mut out = Vec::new();
                for c in e0 {
                    if let C::R(r, k) = c {
                        if let RegF::Cb(s, _) = r.f {
                            let cb = &mut self.cbs[s as usize];
                            cb.dynamic = true;
                        }
                        out.push(C::R(Reg { f: r.f, rel: Some(rel) }, *k));
                    }
                }
                return Ok(Val::num(ty, out));
            }
        }
        // a select chain
        let iv = self.cast(iv, &Ty::Scalar(B::Int))?;
        let mut cur = elems[0].clone();
        for (k, e) in elems.iter().enumerate().skip(1) {
            let m = self.alu(sm4::IEQ, B::Bool, &[&iv.c, &[C::I(self.ival(k as i64))]])[0];
            let n = cur.len();
            cur = self.alu(sm4::MOVC, ty.base(), &[&vec![m; n], e, &cur]);
        }
        Ok(Val::num(ty, cur))
    }

    fn index(&mut self, v: Val, iv: Val) -> R<Val> {
        match &v.ty {
            Ty::Array(e, n) => {
                let k = ncomp(self.prog, e);
                let mut elems: Vec<Vec<C>> = (0..*n as usize).map(|i| v.c[i * k..(i + 1) * k].to_vec()).collect();
                let not_reg = !elems.is_empty()
                    && !elems[0].iter().all(|c| matches!(c, C::R(Reg { f: RegF::X(..) | RegF::Cb(..) | RegF::Icb(_), .. }, _)));
                if not_reg && !matches!(iv.c.first(), Some(C::I(_))) && *n > 4 && !self.sm3() {
                    // (into an indexable temp first)
                    let st = self.storage(&v.ty);
                    self.assign(&st, &v.c);
                    elems = (0..*n as usize).map(|i| st[i * k..(i + 1) * k].to_vec()).collect();
                }
                self.select(elems, iv, (**e).clone())
            }
            Ty::Vec(b, n) => {
                let elems: Vec<Vec<C>> = v.c.iter().map(|c| vec![*c]).collect();
                let _ = n;
                self.select(elems, iv, Ty::Scalar(*b))
            }
            Ty::Mat(b, r, c) => {
                let c = *c as usize;
                let elems: Vec<Vec<C>> = (0..*r as usize).map(|i| v.c[i * c..(i + 1) * c].to_vec()).collect();
                self.select(elems, iv, Ty::vec(*b, c))
            }
            Ty::Tex(..) => {
                let t = match v.obj {
                    Obj::Tex(t) => t,
                    _ => return Err("texture[] of a non-texture".into()),
                };
                self.load(t, iv, [0; 3], false)
            }
            t => Err(format!("index of {t:?}")),
        }
    }

    fn binary(&mut self, op: Bin, x: Val, y: Val) -> R<Val> {
        use sm4::*;
        if matches!(op, Bin::LAnd | Bin::LOr) {
            let t = self.common(&x.ty, &y.ty)?.with_base(B::Bool);
            let a = self.cast(x, &t)?;
            let b = self.cast(y, &t)?;
            let c = self.alu(if op == Bin::LAnd { AND } else { OR }, B::Bool, &[&a.c, &b.c]);
            return Ok(Val::num(t, c));
        }
        let mut t = self.common(&x.ty, &y.ty)?;
        if t.base() == B::Bool {
            t = t.with_base(B::Int);
        }
        if matches!(op, Bin::Shl | Bin::Shr | Bin::And | Bin::Or | Bin::Xor | Bin::Mod) && t.base() == B::Float && op != Bin::Mod {
            return Err("bitwise operator on floats".into());
        }
        if matches!(op, Bin::Shl | Bin::Shr | Bin::And | Bin::Or | Bin::Xor) && self.sm3() {
            return Err("bitwise operators need shader model 4".into());
        }
        let sm3 = self.sm3();
        let a = self.cast(x, &t)?;
        let b = self.cast(y, &t)?;
        let base = t.base();
        let f = base == B::Float;
        let u = base == B::Uint;
        let cmp = |s: &mut Self, o: u32, swap: bool| {
            let c = if swap { s.alu(o, B::Bool, &[&b.c, &a.c]) } else { s.alu(o, B::Bool, &[&a.c, &b.c]) };
            Val::num(t.with_base(B::Bool), c)
        };
        Ok(match op {
            Bin::Add => Val::num(t.clone(), self.alu(if f { ADD } else { IADD }, base, &[&a.c, &b.c])),
            Bin::Sub => Val::num(t.clone(), self.alu_m(if f { ADD } else { IADD }, base, &[(&a.c, 0), (&b.c, 1)])),
            Bin::Mul => {
                if f || sm3 {
                    Val::num(t.clone(), self.alu(MUL, base, &[&a.c, &b.c]))
                } else if a.c.iter().chain(&b.c).all(|c| matches!(c, C::I(_))) {
                    let c = a.c.iter().zip(&b.c).map(|(x, y)| if let (C::I(x), C::I(y)) = (x, y) { C::I(x.wrapping_mul(*y)) } else { *x }).collect();
                    Val::num(t.clone(), c)
                } else {
                    Val::num(t.clone(), self.alu2(if u { UMUL } else { IMUL }, &[&a.c, &b.c], true))
                }
            }
            Bin::Div => {
                if f {
                    Val::num(t.clone(), self.alu(DIV, base, &[&a.c, &b.c]))
                } else if sm3 {
                    let q = self.alu(DIV, base, &[&a.c, &b.c]);
                    Val::num(t.clone(), self.alu(ROUND_Z, base, &[&q]))
                } else if u {
                    Val::num(t.clone(), self.alu2(UDIV, &[&a.c, &b.c], false))
                } else {
                    let c = self.idiv(&a.c, &b.c, false);
                    Val::num(t.clone(), c)
                }
            }
            Bin::Mod => {
                if f || sm3 {
                    // a - b * trunc(a / b)
                    let q = self.alu(DIV, base, &[&a.c, &b.c]);
                    let tq = self.alu(ROUND_Z, base, &[&q]);
                    let c = self.alu_m(MAD, base, &[(&tq, 1), (&b.c, 0), (&a.c, 0)]);
                    Val::num(t.clone(), c)
                } else if u {
                    Val::num(t.clone(), self.alu2(UDIV, &[&a.c, &b.c], true))
                } else {
                    let c = self.idiv(&a.c, &b.c, true);
                    Val::num(t.clone(), c)
                }
            }
            Bin::Shl => Val::num(t.clone(), self.alu(ISHL, base, &[&a.c, &b.c])),
            Bin::Shr => Val::num(t.clone(), self.alu(if u { USHR } else { ISHR }, base, &[&a.c, &b.c])),
            Bin::And => Val::num(t.clone(), self.alu(AND, base, &[&a.c, &b.c])),
            Bin::Or => Val::num(t.clone(), self.alu(OR, base, &[&a.c, &b.c])),
            Bin::Xor => Val::num(t.clone(), self.alu(XOR, base, &[&a.c, &b.c])),
            Bin::Lt => cmp(self, if f { LT } else if u { ULT } else { ILT }, false),
            Bin::Gt => cmp(self, if f { LT } else if u { ULT } else { ILT }, true),
            Bin::Le => cmp(self, if f { GE } else if u { UGE } else { IGE }, true),
            Bin::Ge => cmp(self, if f { GE } else if u { UGE } else { IGE }, false),
            Bin::Eq => cmp(self, if f { EQ } else { IEQ }, false),
            Bin::Ne => cmp(self, if f { NE } else { INE }, false),
            Bin::LAnd | Bin::LOr => unreachable!(),
        })
    }

    /// signed division (or remainder) through udiv
    fn idiv(&mut self, a: &[C], b: &[C], rem: bool) -> Vec<C> {
        use sm4::*;
        let na = self.alu(INEG, B::Int, &[a]);
        let aa = self.alu(IMAX, B::Int, &[a, &na]);
        let nb = self.alu(INEG, B::Int, &[b]);
        let ab = self.alu(IMAX, B::Int, &[b, &nb]);
        let q = self.alu2(UDIV, &[&aa, &ab], rem);
        // the sign: of a (remainder) or of a ^ b
        let s = if rem { a.to_vec() } else { self.alu(XOR, B::Int, &[a, b]) };
        let neg = self.alu(ILT, B::Bool, &[&s, &Self::imm(0, s.len())]);
        let nq = self.alu(INEG, B::Int, &[&q]);
        self.alu(MOVC, B::Int, &[&neg, &nq, &q])
    }

    // ---- calls ----

    fn find_func(&self, name: &str, args: &[Val]) -> Option<&'a Func> {
        let prog = self.prog;
        let cands: Vec<&Func> = prog.funcs.iter().filter(|f| f.name == name && f.params.len() == args.len() && f.body.is_some()).collect();
        if !cands.is_empty() {
            // the closest parameter types
            return cands.into_iter().min_by_key(|f| {
                f.params
                    .iter()
                    .zip(args)
                    .map(|(p, a)| {
                        let pt = p.ty();
                        (pt != &a.ty) as u32 * 2 + (ncomp(prog, pt) != ncomp(prog, &a.ty)) as u32 * 4 + (pt.base() != a.ty.base()) as u32
                    })
                    .sum::<u32>()
            });
        }
        let pre = self.prelude;
        let n = args.iter().map(|a| a.c.len()).max().unwrap_or(1).max(1);
        pre.funcs.iter().find(|f| f.name == name && f.params.len() == args.len() && f.params.first().is_some_and(|p| p.ty().comps() == n))
    }

    fn call(&mut self, name: &str, args: &[Expr], line: u32) -> R<Val> {
        // (out arguments picked by a run-time index are stored back after the call)
        let mark = self.wbs.len();
        let r = self.call_inner(name, args, line);
        self.write_back(mark);
        r
    }

    fn call_inner(&mut self, name: &str, args: &[Expr], line: u32) -> R<Val> {
        // user functions (and the prelude's) take their out arguments as places
        let user = self.prog.funcs.iter().any(|f| f.name == name);
        let pre = !user && self.prelude.funcs.iter().any(|f| f.name == name);
        if user || pre {
            let mut vals = Vec::new();
            let fs: Vec<&Func> = if user { self.prog.funcs.iter().collect() } else { self.prelude.funcs.iter().collect() };
            let outs: Vec<bool> = (0..args.len())
                .map(|i| fs.iter().filter(|f| f.name == name).any(|f| f.params.get(i).is_some_and(|p| p.is_out)))
                .collect();
            for (i, a) in args.iter().enumerate() {
                vals.push(if outs[i] { self.place(a)? } else { self.expr(a)? });
            }
            let f = self.find_func(name, &vals).ok_or(format!("line {line}: no overload of {name} for these arguments"))?;
            return self.inline(f, vals, line);
        }
        super::intrinsics::call(self, name, args, line)
    }

    fn inline(&mut self, f: &'a Func, args: Vec<Val>, line: u32) -> R<Val> {
        if self.fns.len() > 64 {
            return Err(format!("line {line}: calls nested too deep (recursion?)"));
        }
        let body = f.body.as_ref().unwrap();
        let mut scope = HashMap::new();
        let mut copy_back = Vec::new();
        for (p, a) in f.params.iter().zip(args) {
            let t = p.ty().clone();
            if t.is_object() {
                scope.insert(p.name.clone(), a);
                continue;
            }
            if p.is_out {
                let st = self.storage(&t);
                if p.is_in {
                    let v = self.cast(a.clone(), &t)?;
                    self.assign(&st, &v.c);
                }
                copy_back.push((st.clone(), a, t.clone()));
                scope.insert(p.name.clone(), Val::num(t, st));
                continue;
            }
            let v = self.cast(a, &t).map_err(|m| format!("line {line}: argument {} of {}: {m}", p.name, f.name))?;
            let v = if body.iter().any(|s| writes(s, &p.name, &|f| self.has_out(f))) {
                let st = self.storage(&t);
                self.assign(&st, &v.c);
                Val::num(t, st)
            } else {
                v
            };
            scope.insert(p.name.clone(), v);
        }
        let ret = if f.ret == Ty::Void { Vec::new() } else { self.storage(&f.ret) };
        // returns before the end: a flag that skips the rest
        let early = body.iter().enumerate().any(|(i, s)| has_return(s) && (i + 1 < body.len() || !matches!(s, Stmt::Return(..))));
        let flag = if early {
            let t = self.tmp();
            self.marks.push((t, self.code.len()));
            self.assign(&[C::R(temp(t), 0)], &[C::I(0)]);
            Some(t)
        } else {
            None
        };
        // (the callee sees globals and its parameters only)
        let saved = std::mem::take(&mut self.scopes);
        self.scopes.push(scope);
        self.fns.push(FnCtx { ret: ret.clone(), ret_ty: f.ret.clone(), flag, loops: 0, entry: None });
        let r = self.stmts(body);
        self.fns.pop();
        self.scopes = saved;
        r?;
        for (st, a, t) in copy_back {
            let v = self.cast(Val::num(t, st), &a.ty)?;
            self.assign(&a.c, &v.c);
        }
        // (the result in fresh registers: the callee's storage may be reused by a later call)
        Ok(Val::num(f.ret.clone(), ret))
    }

    fn method(&mut self, o: &Expr, name: &str, args: &[Expr]) -> R<Val> {
        let ov = self.expr(o)?;
        super::intrinsics::method(self, ov, name, args)
    }

    // ---- for intrinsics.rs ----

    pub(super) fn x_alu(&mut self, op: u32, b: B, args: &[&[C]]) -> Vec<C> {
        self.alu(op, b, args)
    }
    pub(super) fn x_alu_m(&mut self, op: u32, b: B, args: &[(&[C], u8)]) -> Vec<C> {
        self.alu_m(op, b, args)
    }
    pub(super) fn x_alu2(&mut self, op: u32, args: &[&[C]], second: bool) -> Vec<C> {
        self.alu2(op, args, second)
    }
    pub(super) fn x_cast(&mut self, v: Val, t: &Ty) -> R<Val> {
        self.cast(v, t)
    }
    pub(super) fn x_common(&self, a: &Ty, b: &Ty) -> R<Ty> {
        self.common(a, b)
    }
    pub(super) fn x_place(&mut self, e: &Expr) -> R<Val> {
        self.place(e)
    }
    pub(super) fn x_assign(&mut self, d: &[C], s: &[C]) {
        self.assign(d, s)
    }
    pub(super) fn x_assign_sat(&mut self, d: &[C], s: &[C]) {
        self.assign_op(sm4::MOV, d, s, true)
    }
    pub(super) fn x_tmp(&mut self) -> u32 {
        self.tmp()
    }
    pub(super) fn x_opnd(&mut self, cs: &[C]) -> Src {
        self.opnd(cs)
    }
    pub(super) fn x_emit(&mut self, op: u32, dst: Vec<Dst>, src: Vec<Src>) -> &mut Ins {
        self.emit(op, dst, src)
    }
    pub(super) fn x_cond(&mut self, op: u32, c: C, nz: bool) {
        self.cond_op(op, c, nz)
    }
    pub(super) fn x_tex_el(&self, t: u32) -> (Dim, Ty) {
        let d = &self.texs[t as usize];
        (d.dim, d.el.clone())
    }
    pub(super) fn x_samp_cmp(&mut self, s: u32) {
        self.samps[s as usize].cmp = true;
    }
    pub(super) fn x_sm3(&self) -> bool {
        self.sm3()
    }
    pub(super) fn x_major(&self) -> u32 {
        self.o.major
    }
}
