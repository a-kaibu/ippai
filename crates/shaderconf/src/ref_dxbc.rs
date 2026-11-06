//! A reference interpreter of shader model 4/5 pixel shaders (DXBC), written from the Direct3D
//! 11.3 functional spec and the tokenized program format (d3d11TokenizedProgramFormat.hpp),
//! independent of dxwgsl: its own decoder, and every instruction as the spec states it. It runs
//! the ALU, flow control, indexable temps and constant buffers; resources, derivatives and
//! discard are outside it (`Err`).
//!
//! What it is for: the cases' expected pixels check it (tests below); then it is the oracle
//! for bytecode nobody wrote expectations for (fuzzed programs, real games' shaders).

/// A register value: four 32-bit components.
pub type V = [u32; 4];

#[derive(Clone, Debug)]
enum Idx {
    Imm(u32),
    /// a register's component plus an offset
    Rel(Box<Op>, u32),
}

#[derive(Clone, Debug)]
pub struct Op {
    ty: u32,
    /// for sources: which component each of x, y, z, w reads; for destinations: unused
    swz: [u8; 4],
    /// destinations: write mask
    mask: u8,
    idx: Vec<Idx>,
    imm: V,
    /// 1 negate, 2 absolute, 3 both
    md: u8,
}

#[derive(Clone, Debug)]
struct Inst {
    op: u32,
    sat: bool,
    /// (if, breakc, ...: true when testing for nonzero)
    nz: bool,
    ops: Vec<Op>,
}

pub struct Program {
    insts: Vec<Inst>,
    temps: usize,
    /// indexable temps: register -> size
    xs: Vec<(u32, u32)>,
    icb: Vec<V>,
    /// flow control: the matching instruction of each if/else/loop/switch/case
    jump: Vec<usize>,
}

impl Program {
    pub fn show(&self) -> String {
        self.insts.iter().enumerate().map(|(n, i)| format!("{n}: {i:?}\n")).collect()
    }
}

fn rd(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| "short".to_string())
}

/// The SHDR/SHEX chunk's tokens.
fn tokens(code: &[u8]) -> Result<Vec<u32>, String> {
    if code.get(..4) != Some(b"DXBC") {
        return Err("not DXBC".into());
    }
    let n = rd(code, 28)? as usize;
    for i in 0..n {
        let at = rd(code, 32 + 4 * i)? as usize;
        let tag = code.get(at..at + 4).ok_or("short")?;
        if tag == b"SHDR" || tag == b"SHEX" {
            let size = rd(code, at + 4)? as usize;
            let body = code.get(at + 8..at + 8 + size).ok_or("short chunk")?;
            return Ok(body.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect());
        }
    }
    Err("no SHDR/SHEX".into())
}

fn operand(t: &[u32], at: &mut usize) -> Result<Op, String> {
    let w = *t.get(*at).ok_or("operand past the end")?;
    *at += 1;
    let comps = w & 3;
    let ty = (w >> 12) & 0xff;
    let dims = (w >> 20) & 3;
    let mut op = Op { ty, swz: [0, 1, 2, 3], mask: 0xf, idx: Vec::new(), imm: [0; 4], md: 0 };
    match comps {
        0 => op.mask = 0,
        1 => {
            op.swz = [0; 4];
            op.mask = 1;
        }
        2 => match (w >> 2) & 3 {
            0 => op.mask = ((w >> 4) & 0xf) as u8,
            1 => {
                op.swz = std::array::from_fn(|i| ((w >> (4 + 2 * i)) & 3) as u8);
            }
            2 => {
                let c = ((w >> 4) & 3) as u8;
                op.swz = [c; 4];
                op.mask = 1 << c;
            }
            _ => return Err("selection mode 3".into()),
        },
        _ => return Err("N-component operand".into()),
    }
    let mut ext = w >> 31 != 0;
    while ext {
        let e = *t.get(*at).ok_or("short")?;
        *at += 1;
        if e & 0x3f == 1 {
            op.md = ((e >> 6) & 0xff) as u8;
        }
        ext = e >> 31 != 0;
    }
    if ty == 4 {
        let n = if comps == 1 { 1 } else { 4 };
        for i in 0..n {
            op.imm[i] = *t.get(*at).ok_or("short")?;
            *at += 1;
        }
        if n == 1 {
            op.imm = [op.imm[0]; 4];
        }
        return Ok(op);
    }
    if ty == 5 {
        return Err("64-bit immediates".into());
    }
    for d in 0..dims {
        let rep = (w >> (22 + 3 * d)) & 7;
        let imm = match rep {
            0 | 3 => {
                let v = *t.get(*at).ok_or("short")?;
                *at += 1;
                v
            }
            2 => 0,
            _ => return Err("64-bit index".into()),
        };
        op.idx.push(if rep == 0 { Idx::Imm(imm) } else { Idx::Rel(Box::new(operand(t, at)?), imm) });
    }
    Ok(op)
}

pub fn parse(code: &[u8]) -> Result<Program, String> {
    let t = tokens(code)?;
    if t.len() < 2 {
        return Err("short program".into());
    }
    if (t[0] >> 16) & 0xffff != 0 {
        return Err("not a pixel shader".into());
    }
    let end = (t[1] as usize).min(t.len());
    let mut p = Program { insts: Vec::new(), temps: 0, xs: Vec::new(), icb: Vec::new(), jump: Vec::new() };
    let mut at = 2;
    while at < end {
        let w = t[at];
        let op = w & 0x7ff;
        let len = if op == 0x35 { *t.get(at + 1).ok_or("short")? as usize } else { ((w >> 24) & 0x7f) as usize };
        if len == 0 || at + len > end {
            return Err(format!("instruction {op:#x}: length {len}"));
        }
        let body = &t[at..at + len];
        match op {
            // customdata: an immediate constant buffer
            0x35 => {
                if w >> 11 == 3 {
                    p.icb = body[2..].chunks_exact(4).map(|c| [c[0], c[1], c[2], c[3]]).collect();
                }
            }
            // dcl_temps, dcl_indexableTemp
            0x68 => p.temps = *body.get(1).ok_or("short")? as usize,
            0x69 => p.xs.push((*body.get(1).ok_or("short")?, *body.get(2).ok_or("short")?)),
            0x58..=0x6a | 0x8f..=0xa2 => {}
            _ => {
                let mut k = 1;
                let mut ext = w >> 31 != 0;
                while ext {
                    let e = *body.get(k).ok_or("short")?;
                    k += 1;
                    ext = e >> 31 != 0;
                }
                let mut ops = Vec::new();
                while k < len {
                    ops.push(operand(body, &mut k)?);
                }
                const OPS: &[u32] = &[
                    0x00, 0x01, 0x02, 0x03, 0x06, 0x07, 0x08, 0x0a, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x15, 0x16, 0x17, 0x18, 0x19, 0x1a,
                    0x1b, 0x1c, 0x1d, 0x1e, 0x1f, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x2b, 0x2f, 0x30,
                    0x31, 0x32, 0x33, 0x34, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x3b, 0x3c, 0x3e, 0x3f, 0x40, 0x41, 0x42, 0x43, 0x44, 0x4b,
                    0x4c, 0x4d, 0x4e, 0x4f, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57, 0x81, 0x82, 0x83, 0x86, 0x87, 0x88, 0x89,
                    0x8a, 0x8b, 0x8c, 0x8d, 0x8e,
                ];
                if !OPS.contains(&op) {
                    return Err(format!("opcode {op:#x}"));
                }
                // (the operand count of each opcode, from the D3D10/11 instruction reference)
                let want = match op {
                    0x02 | 0x07 | 0x0a | 0x12 | 0x15 | 0x16 | 0x17 | 0x30 | 0x3a | 0x3e => 0,
                    0x03 | 0x06 | 0x08 | 0x1f | 0x3f | 0x4c => 1,
                    0x19 | 0x1a | 0x1b | 0x1c | 0x28 | 0x2b | 0x2f | 0x36 | 0x3b | 0x40..=0x44 | 0x4b | 0x56 | 0x81..=0x83
                    | 0x86..=0x89 | 0x8d => 2,
                    0x23 | 0x26 | 0x32 | 0x37 | 0x4e | 0x51 | 0x52 | 0x8a | 0x8b => 4,
                    0x8c | 0x8e => 5,
                    _ => 3,
                };
                if ops.len() != want {
                    return Err(format!("opcode {op:#x} with {} operands, {want} wanted", ops.len()));
                }
                p.insts.push(Inst { op, sat: (w >> 13) & 1 != 0, nz: (w >> 18) & 1 != 0, ops });
            }
        }
        at += len;
    }
    // matching flow control
    p.jump = vec![usize::MAX; p.insts.len()];
    let mut stack: Vec<usize> = Vec::new();
    for i in 0..p.insts.len() {
        match p.insts[i].op {
            0x1f | 0x30 | 0x4c => stack.push(i),
            0x12 => {
                let s = *stack.last().ok_or("else outside an if")?;
                if p.insts[s].op != 0x1f {
                    return Err("else outside an if".into());
                }
                p.jump[s] = i;
                *stack.last_mut().unwrap() = i;
            }
            0x15 | 0x16 | 0x17 => {
                let s = stack.pop().ok_or("unbalanced flow control")?;
                let want: &[u32] = match p.insts[i].op {
                    0x15 => &[0x1f, 0x12],
                    0x16 => &[0x30],
                    _ => &[0x4c],
                };
                if !want.contains(&p.insts[s].op) {
                    return Err("unbalanced flow control".into());
                }
                p.jump[s] = i;
                p.jump[i] = s;
            }
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("unclosed flow control".into());
    }
    Ok(p)
}

fn f(x: u32) -> f32 {
    f32::from_bits(x)
}
fn b(x: f32) -> u32 {
    x.to_bits()
}

/// The functional spec's float min/max: a NaN operand gives the other one.
fn fmin(a: f32, c: f32) -> f32 {
    if a.is_nan() { c } else if c.is_nan() { a } else if a < c || (a == c && a.is_sign_negative()) { a } else { c }
}
fn fmax(a: f32, c: f32) -> f32 {
    if a.is_nan() { c } else if c.is_nan() { a } else if a > c || (a == c && c.is_sign_negative()) { a } else { c }
}

/// ftoi: NaN 0, out of range saturates.
fn ftoi(x: f32) -> u32 {
    if x.is_nan() { 0 } else { (x as i32) as u32 }
}
fn ftou(x: f32) -> u32 {
    if x.is_nan() { 0 } else { x as u32 }
}

/// f32 -> f16 bits as WARP converts (dxwgsl's d3d_f32tof16): toward zero, finite overflow to
/// 65504, infinities stay, NaNs 0x7fff (with the sign), float denormals 0.
fn f32tof16(x: f32) -> u32 {
    let b = x.to_bits();
    let (s, a) = ((b >> 16) & 0x8000, b & 0x7fff_ffff);
    if a > 0x7f80_0000 {
        s | 0x7fff
    } else if a == 0x7f80_0000 {
        s | 0x7c00
    } else if a >= 0x477f_e000 {
        s | 0x7bff
    } else if a >= 0x3880_0000 {
        s | (a - 0x3800_0000) >> 13
    } else if a < 0x0080_0000 {
        s
    } else {
        s | (f32::from_bits(a) * 16777216.0) as u32
    }
}

struct Run<'a> {
    p: &'a Program,
    r: Vec<V>,
    x: Vec<(u32, Vec<V>)>,
    o: [V; 8],
    cb: &'a [Vec<V>],
    /// why the result is not one every implementation gives: a read or write out of range
    /// (robust access may return anything in range), or a float op that gave an infinity or
    /// NaN (WGSL leaves those indeterminate)
    taint: std::cell::RefCell<Option<String>>,
}

/// Float ops whose results the oracle wants finite.
const FLOAT_OUT: &[u32] = &[0x00, 0x0e, 0x19, 0x1a, 0x2f, 0x32, 0x33, 0x34, 0x38, 0x40, 0x41, 0x42, 0x43, 0x44, 0x4b, 0x81, 0x83];

#[derive(Clone, Copy, PartialEq)]
enum K {
    F,
    I,
    U,
}

impl Run<'_> {
    fn index(&self, i: &Idx) -> Result<u32, String> {
        Ok(match i {
            Idx::Imm(n) => *n,
            Idx::Rel(o, n) => self.src(o, K::U)?[0].wrapping_add(*n),
        })
    }
    /// The register an operand names (all four components).
    fn reg(&self, o: &Op) -> Result<V, String> {
        let i0 = o.idx.first().map(|i| self.index(i)).transpose()?.unwrap_or(0);
        Ok(match o.ty {
            0 => *self.r.get(i0 as usize).ok_or("temp out of range")?,
            2 => self.o[i0 as usize & 7],
            3 => {
                let i1 = self.index(o.idx.get(1).ok_or("x# without index")?)?;
                let x = &self.x.iter().find(|x| x.0 == i0).ok_or("undeclared x#")?.1;
                // (out of range: 0, as reads of a register past the end)
                x.get(i1 as usize).copied().unwrap_or_else(|| self.taint("x# read out of range"))
            }
            4 => o.imm,
            8 => {
                let i1 = self.index(o.idx.get(1).ok_or("cb without index")?)?;
                self.cb.get(i0 as usize).and_then(|c| c.get(i1 as usize)).copied().unwrap_or_else(|| self.taint("cb read out of range"))
            }
            9 => self.p.icb.get(i0 as usize).copied().unwrap_or_else(|| self.taint("icb read out of range")),
            t => return Err(format!("source register type {t}")),
        })
    }
    /// A source read as kind `k` (modifiers: float negate/abs; integer negate).
    fn src(&self, o: &Op, k: K) -> Result<V, String> {
        let v = self.reg(o)?;
        let v: V = std::array::from_fn(|i| v[o.swz[i] as usize]);
        Ok(v.map(|x| match (k, o.md) {
            (_, 0) => x,
            (K::F, 1) => x ^ 0x8000_0000,
            (K::F, 2) => x & 0x7fff_ffff,
            (K::F, 3) => x | 0x8000_0000,
            (_, 1) => x.wrapping_neg(),
            _ => x,
        }))
    }
    fn dst(&mut self, o: &Op, v: V, sat: bool) -> Result<(), String> {
        let v = if sat { v.map(|x| b(if f(x).is_nan() { 0.0 } else { f(x).clamp(0.0, 1.0) })) } else { v };
        if o.ty == 13 {
            return Ok(());
        }
        let i0 = o.idx.first().map(|i| self.index(i)).transpose()?.unwrap_or(0) as usize;
        let i1 = o.idx.get(1).map(|i| self.index(i)).transpose()?;
        let slot: &mut V = match o.ty {
            0 => self.r.get_mut(i0).ok_or("temp out of range")?,
            2 => self.o.get_mut(i0).ok_or("output out of range")?,
            3 => {
                let x = &mut self.x.iter_mut().find(|x| x.0 == i0 as u32).ok_or("undeclared x#")?.1;
                match x.get_mut(i1.unwrap_or(0) as usize) {
                    Some(s) => s,
                    // (writes past the end are dropped)
                    None => {
                        self.taint("x# write out of range");
                        return Ok(());
                    }
                }
            }
            t => return Err(format!("destination register type {t}")),
        };
        for c in 0..4 {
            if o.mask & (1 << c) != 0 {
                slot[c] = v[c];
            }
        }
        Ok(())
    }
    /// Component-wise: d = g(a, b, c) on sources read as `k`.
    fn cw(&mut self, i: &Inst, k: K, g: impl Fn(&[u32]) -> u32) -> Result<(), String> {
        let s: Vec<V> = i.ops[1..].iter().map(|o| self.src(o, k)).collect::<Result<_, _>>()?;
        let v: V = std::array::from_fn(|c| g(&s.iter().map(|x| x[c]).collect::<Vec<_>>()));
        // (float math or comparisons on a denormal: implementations may flush it)
        if (FLOAT_OUT.contains(&i.op) || matches!(i.op, 0x18 | 0x1d | 0x31 | 0x39)) && s.iter().flatten().any(|&x| f(x).is_subnormal()) {
            self.taint("a denormal operand");
        }
        if FLOAT_OUT.contains(&i.op) {
            self.finite(&i.ops[0], &v);
        }
        self.dst(&i.ops[0], v, i.sat)
    }
    fn taint(&self, why: &str) -> V {
        self.taint.borrow_mut().get_or_insert_with(|| why.to_string());
        [0; 4]
    }
    /// (the written components)
    fn finite(&self, d: &Op, v: &V) {
        if (0..4).any(|c| d.mask & (1 << c) != 0 && (!f(v[c]).is_finite() || f(v[c]).is_subnormal())) {
            self.taint("a float op gave an infinity, NaN or denormal");
        }
    }
    fn cond(&self, i: &Inst) -> Result<bool, String> {
        let v = self.src(&i.ops[0], K::U)?[0];
        Ok((v != 0) == i.nz)
    }

    fn exec(&mut self) -> Result<(), String> {
        let p = self.p;
        let mut pc = 0;
        let mut steps = 0u32;
        // (enclosing loops and switches: their opening instruction)
        let mut blocks: Vec<usize> = Vec::new();
        while pc < p.insts.len() {
            steps += 1;
            if steps > 1_000_000 {
                return Err("runs too long".into());
            }
            let i = &p.insts[pc];
            pc += 1;
            use K::*;
            match i.op {
                0x00 => self.cw(i, F, |a| b(f(a[0]) + f(a[1])))?,
                0x01 => self.cw(i, U, |a| a[0] & a[1])?,
                0x02 | 0x03 => {
                    if i.op == 0x02 || self.cond(i)? {
                        let s = blocks.pop().ok_or("break outside a loop")?;
                        pc = p.jump[s] + 1;
                    }
                }
                0x07 | 0x08 => {
                    if i.op == 0x07 || self.cond(i)? {
                        let s = *blocks.iter().rev().find(|&&s| p.insts[s].op == 0x30).ok_or("continue outside a loop")?;
                        while *blocks.last().unwrap() != s {
                            blocks.pop();
                        }
                        pc = s + 1;
                    }
                }
                0x0e => self.cw(i, F, |a| b(f(a[0]) / f(a[1])))?,
                0x0f | 0x10 | 0x11 => {
                    let n = (i.op - 0x0d) as usize;
                    let a = self.src(&i.ops[1], F)?;
                    let c = self.src(&i.ops[2], F)?;
                    if a[..n].iter().chain(&c[..n]).any(|&x| f(x).is_subnormal()) {
                        self.taint("a denormal operand");
                    }
                    let mut d = f(a[0]) * f(c[0]);
                    for k in 1..n {
                        d += f(a[k]) * f(c[k]);
                    }
                    self.finite(&i.ops[0], &[b(d); 4]);
                    self.dst(&i.ops[0], [b(d); 4], i.sat)?;
                }
                0x12 => pc = p.jump[pc - 1] + 1,
                0x15 => {}
                0x16 => pc = p.jump[pc - 1] + 1,
                0x17 => {
                    blocks.pop();
                }
                0x06 | 0x0a => {}
                0x18 => self.cw(i, F, |a| if f(a[0]) == f(a[1]) { !0 } else { 0 })?,
                0x1d => self.cw(i, F, |a| if f(a[0]) >= f(a[1]) { !0 } else { 0 })?,
                0x31 => self.cw(i, F, |a| if f(a[0]) < f(a[1]) { !0 } else { 0 })?,
                0x39 => self.cw(i, F, |a| if f(a[0]) != f(a[1]) { !0 } else { 0 })?,
                0x20 => self.cw(i, I, |a| if a[0] == a[1] { !0 } else { 0 })?,
                0x21 => self.cw(i, I, |a| if a[0] as i32 >= a[1] as i32 { !0 } else { 0 })?,
                0x22 => self.cw(i, I, |a| if (a[0] as i32) < a[1] as i32 { !0 } else { 0 })?,
                0x27 => self.cw(i, I, |a| if a[0] != a[1] { !0 } else { 0 })?,
                0x4f => self.cw(i, U, |a| if a[0] < a[1] { !0 } else { 0 })?,
                0x50 => self.cw(i, U, |a| if a[0] >= a[1] { !0 } else { 0 })?,
                0x19 => self.cw(i, F, |a| b(f(a[0]).exp2()))?,
                // (below 1: -tiny gives 0x3f7fffff; infinities NaN)
                0x1a => self.cw(i, F, |a| b(if f(a[0]).is_finite() { (f(a[0]) - f(a[0]).floor()).min(f32::from_bits(0x3f7f_ffff)) } else { f32::NAN }))?,
                0x1b => self.cw(i, F, |a| ftoi(f(a[0])))?,
                0x1c => self.cw(i, F, |a| ftou(f(a[0])))?,
                0x2b => self.cw(i, I, |a| b(a[0] as i32 as f32))?,
                0x56 => self.cw(i, U, |a| b(a[0] as f32))?,
                0x1e => self.cw(i, I, |a| a[0].wrapping_add(a[1]))?,
                0x1f => {
                    if !self.cond(i)? {
                        let j = p.jump[pc - 1];
                        // (to the else's body, or past the endif)
                        pc = j + 1;
                    }
                }
                0x23 => self.cw(i, I, |a| a[0].wrapping_mul(a[1]).wrapping_add(a[2]))?,
                0x52 => self.cw(i, U, |a| a[0].wrapping_mul(a[1]).wrapping_add(a[2]))?,
                0x24 => self.cw(i, I, |a| (a[0] as i32).max(a[1] as i32) as u32)?,
                0x25 => self.cw(i, I, |a| (a[0] as i32).min(a[1] as i32) as u32)?,
                0x53 => self.cw(i, U, |a| a[0].max(a[1]))?,
                0x54 => self.cw(i, U, |a| a[0].min(a[1]))?,
                0x26 | 0x51 => {
                    // (hi, lo) = a * b
                    let k = if i.op == 0x26 { I } else { U };
                    let a = self.src(&i.ops[2], k)?;
                    let c = self.src(&i.ops[3], k)?;
                    let wide: [u64; 4] = std::array::from_fn(|n| {
                        if k == I { (a[n] as i32 as i64 * c[n] as i32 as i64) as u64 } else { a[n] as u64 * c[n] as u64 }
                    });
                    self.dst(&i.ops[0], wide.map(|x| (x >> 32) as u32), false)?;
                    self.dst(&i.ops[1], wide.map(|x| x as u32), false)?;
                }
                0x4e => {
                    let a = self.src(&i.ops[2], U)?;
                    let c = self.src(&i.ops[3], U)?;
                    let q = std::array::from_fn(|n| a[n].checked_div(c[n]).unwrap_or(!0));
                    let r = std::array::from_fn(|n| a[n].checked_rem(c[n]).unwrap_or(!0));
                    self.dst(&i.ops[0], q, false)?;
                    self.dst(&i.ops[1], r, false)?;
                }
                0x28 => self.cw(i, I, |a| a[0].wrapping_neg())?,
                0x29 => self.cw(i, I, |a| a[0] << (a[1] & 31))?,
                0x2a => self.cw(i, I, |a| ((a[0] as i32) >> (a[1] & 31)) as u32)?,
                0x55 => self.cw(i, U, |a| a[0] >> (a[1] & 31))?,
                // (denormals are 0: WARP flushes them)
                0x2f => self.cw(i, F, |a| b(if f(a[0]).is_subnormal() { f32::NEG_INFINITY } else { f(a[0]).log2() }))?,
                0x30 => blocks.push(pc - 1),
                0x32 => self.cw(i, F, |a| b(f(a[0]) * f(a[1]) + f(a[2])))?,
                0x33 => self.cw(i, F, |a| b(fmin(f(a[0]), f(a[1]))))?,
                0x34 => self.cw(i, F, |a| b(fmax(f(a[0]), f(a[1]))))?,
                0x36 => self.cw(i, F, |a| a[0])?,
                0x37 => {
                    let c = self.src(&i.ops[1], U)?;
                    let a = self.src(&i.ops[2], F)?;
                    let e = self.src(&i.ops[3], F)?;
                    self.dst(&i.ops[0], std::array::from_fn(|n| if c[n] != 0 { a[n] } else { e[n] }), i.sat)?;
                }
                0x38 => self.cw(i, F, |a| b(f(a[0]) * f(a[1])))?,
                0x3a => {}
                0x3b => self.cw(i, U, |a| !a[0])?,
                0x3c => self.cw(i, U, |a| a[0] | a[1])?,
                0x57 => self.cw(i, U, |a| a[0] ^ a[1])?,
                0x3e => return Ok(()),
                0x3f => {
                    if self.cond(i)? {
                        return Ok(());
                    }
                }
                0x40 => self.cw(i, F, |a| b(f(a[0]).round_ties_even()))?,
                0x41 => self.cw(i, F, |a| b(f(a[0]).floor()))?,
                0x42 => self.cw(i, F, |a| b(f(a[0]).ceil()))?,
                0x43 => self.cw(i, F, |a| b(f(a[0]).trunc()))?,
                0x44 => self.cw(i, F, |a| b(1.0 / f(a[0]).sqrt()))?,
                0x4b => self.cw(i, F, |a| b(f(a[0]).sqrt()))?,
                0x4c => {
                    let s = self.src(&i.ops[0], I)?[0];
                    let start = pc - 1;
                    let end = p.jump[start];
                    blocks.push(start);
                    // the case with that label at this level, else default, else past the end
                    let mut depth = 0;
                    let mut target = None;
                    let mut default = None;
                    for j in start + 1..end {
                        match p.insts[j].op {
                            0x1f | 0x30 | 0x4c => depth += 1,
                            0x15 | 0x16 | 0x17 => depth -= 1,
                            0x06 if depth == 0 && p.insts[j].ops[0].imm[0] == s && target.is_none() => target = Some(j),
                            0x0a if depth == 0 => default = Some(j),
                            _ => {}
                        }
                    }
                    match target.or(default) {
                        Some(j) => pc = j + 1,
                        None => {
                            blocks.pop();
                            pc = end + 1;
                        }
                    }
                }
                0x4d => {
                    let a = self.src(&i.ops[2], F)?;
                    if a.iter().any(|&x| f(x).abs() > 1e4) {
                        self.taint("sincos far from 0");
                    }
                    self.dst(&i.ops[0], a.map(|x| b(f(x).sin())), i.sat)?;
                    self.dst(&i.ops[1], a.map(|x| b(f(x).cos())), i.sat)?;
                }
                0x81 => self.cw(i, F, |a| b(1.0 / f(a[0])))?,
                0x82 => self.cw(i, F, |a| f32tof16(f(a[0])))?,
                0x83 => self.cw(i, U, |a| b(half::f16::from_bits(a[0] as u16).to_f32()))?,
                0x86 => self.cw(i, U, |a| a[0].count_ones())?,
                0x87 => self.cw(i, U, |a| if a[0] == 0 { !0 } else { a[0].leading_zeros() })?,
                0x88 => self.cw(i, U, |a| if a[0] == 0 { !0 } else { a[0].trailing_zeros() })?,
                0x89 => self.cw(i, I, |a| {
                    let x = a[0] as i32;
                    if x == 0 || x == -1 { !0 } else if x < 0 { (!a[0]).leading_zeros() } else { a[0].leading_zeros() }
                })?,
                0x8a | 0x8b => self.cw(i, U, |a| {
                    let (w, o, x) = (a[0] & 31, a[1] & 31, a[2]);
                    if w == 0 {
                        0
                    } else if w + o < 32 {
                        let sh = x << (32 - w - o);
                        if i.op == 0x8a { sh >> (32 - w) } else { ((sh as i32) >> (32 - w)) as u32 }
                    } else if i.op == 0x8a {
                        x >> o
                    } else {
                        ((x as i32) >> o) as u32
                    }
                })?,
                0x8c => self.cw(i, U, |a| {
                    let (w, o, x, y) = (a[0] & 31, a[1] & 31, a[2], a[3]);
                    let m = ((1u64 << w) - 1) as u32;
                    let m = m.wrapping_shl(o);
                    (x.wrapping_shl(o) & m) | (y & !m)
                })?,
                0x8d => self.cw(i, U, |a| a[0].reverse_bits())?,
                0x8e => {
                    let c = self.src(&i.ops[2], U)?;
                    let a = self.src(&i.ops[3], U)?;
                    let e = self.src(&i.ops[4], U)?;
                    let d0 = std::array::from_fn(|n| if c[n] != 0 { e[n] } else { a[n] });
                    let d1 = std::array::from_fn(|n| if c[n] != 0 { a[n] } else { e[n] });
                    self.dst(&i.ops[0], d0, false)?;
                    self.dst(&i.ops[1], d1, false)?;
                }
                op => return Err(format!("opcode {op:#x}")),
            }
        }
        Ok(())
    }
}

/// Runs a pixel shader on constant buffers `cb`; returns o0..o7, and why they may not be what
/// every implementation gives (see `Run::taint`).
pub fn run(p: &Program, cb: &[Vec<V>]) -> Result<([V; 8], Option<String>), String> {
    let mut r = Run {
        p,
        r: vec![[0; 4]; p.temps],
        x: p.xs.iter().map(|&(n, s)| (n, vec![[0; 4]; s as usize])).collect(),
        o: [[0; 4]; 8],
        cb,
        taint: Default::default(),
    };
    r.exec()?;
    Ok((r.o, r.taint.into_inner()))
}
