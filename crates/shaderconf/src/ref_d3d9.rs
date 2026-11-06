//! A reference interpreter of Direct3D 9 pixel shaders (ps_2_0 .. ps_3_0 tokens), written from
//! the D3D9 instruction reference and the shader token format (d3d9types.h), independent of
//! dxwgsl. ALU, flow control (if/ifc/else, rep, loop with aL, break/breakc/breakp,
//! call/callnz/ret/label), predication and def/defi/defb; textures, derivatives and texkill are
//! outside it (`Err`). Like ref_dxbc: checked against the cases' expected pixels, then an
//! oracle for bytecode nobody wrote expectations for.

#[derive(Clone, Debug)]
struct Src {
    ty: u32,
    reg: u32,
    swz: [u8; 4],
    md: u32,
    /// relative addressing: (register type, register, component)
    rel: Option<(u32, u32, u8)>,
}

#[derive(Clone, Debug)]
struct Dst {
    ty: u32,
    reg: u32,
    mask: u8,
    sat: bool,
}

#[derive(Clone, Debug)]
struct Inst {
    op: u32,
    /// comparison (ifc, breakc, setp): 1 gt, 2 eq, 3 ge, 4 lt, 5 ne, 6 le
    cmp: u32,
    dst: Option<Dst>,
    pred: Option<Src>,
    src: Vec<Src>,
    /// def/defi/defb values
    raw: [u32; 4],
}

pub struct Program {
    insts: Vec<Inst>,
    /// matching flow control instruction (if -> else/endif, else -> endif, rep/loop -> end)
    jump: Vec<usize>,
    labels: Vec<(u32, usize)>,
}

impl Program {
    pub fn show(&self) -> String {
        self.insts.iter().enumerate().map(|(n, i)| format!("{n}: {i:?}\n")).collect()
    }
}

fn rtype(w: u32) -> u32 {
    ((w >> 28) & 7) | ((w >> 8) & 0x18)
}

fn src_token(w: u32) -> Src {
    Src { ty: rtype(w), reg: w & 0x7ff, swz: std::array::from_fn(|i| ((w >> (16 + 2 * i)) & 3) as u8), md: (w >> 24) & 0xf, rel: None }
}

const NO_DST: &[u32] = &[0, 25, 26, 27, 28, 29, 30, 38, 39, 40, 41, 42, 43, 44, 45, 96];

pub fn parse(words: &[u32]) -> Result<Program, String> {
    let v = *words.first().ok_or("empty")?;
    if v >> 16 != 0xffff {
        return Err("not a pixel shader".into());
    }
    if (v >> 8) & 0xff < 2 {
        return Err("ps_1_x".into());
    }
    let mut p = Program { insts: Vec::new(), jump: Vec::new(), labels: Vec::new() };
    let mut at = 1;
    loop {
        let w = *words.get(at).ok_or("no end token")?;
        if w == 0xffff {
            break;
        }
        let op = w & 0xffff;
        if op == 0xfffe {
            at += 1 + ((w >> 16) & 0x7fff) as usize;
            continue;
        }
        let len = ((w >> 24) & 0xf) as usize;
        let body = words.get(at + 1..at + 1 + len).ok_or("instruction past the end")?;
        at += 1 + len;
        // (what exec runs: anything else is outside the interpreter, run or not)
        const OPS: &[u32] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 32, 33, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 47, 48, 78, 79, 80, 81, 88, 90, 94, 96];
        if op != 31 && !OPS.contains(&op) {
            return Err(format!("opcode {op}"));
        }
        if op != 31 && !matches!(op, 47 | 48 | 81) && body.iter().any(|t| t >> 31 == 0) {
            return Err("a parameter token without bit 31".into());
        }
        let mut i = Inst { op, cmp: (w >> 16) & 7, dst: None, pred: None, src: Vec::new(), raw: [0; 4] };
        match op {
            31 => continue,
            47 | 48 | 81 => {
                let d = *body.first().ok_or("short def")?;
                // (a def writes a constant register of its kind, through a parameter token)
                let want = match op {
                    81 => 2,
                    48 => 7,
                    _ => 14,
                };
                if d >> 31 == 0 || rtype(d) != want {
                    return Err("a def of something that is not a constant register".into());
                }
                i.dst = Some(Dst { ty: rtype(d), reg: d & 0x7ff, mask: 0xf, sat: false });
                for (k, x) in body[1..].iter().take(4).enumerate() {
                    i.raw[k] = *x;
                }
                p.insts.push(i);
                continue;
            }
            _ => {}
        }
        let mut k = 0;
        if !NO_DST.contains(&op) {
            let d = *body.first().ok_or("no destination")?;
            if d & (1 << 13) != 0 {
                return Err("relative destination".into());
            }
            i.dst = Some(Dst { ty: rtype(d), reg: d & 0x7ff, mask: ((d >> 16) & 0xf) as u8, sat: (d >> 20) & 1 != 0 });
            if (d >> 24) & 0xf != 0 {
                return Err("shift scale".into());
            }
            k = 1;
        }
        if w & (1 << 28) != 0 {
            i.pred = Some(src_token(*body.get(k).ok_or("no predicate")?));
            k += 1;
        }
        while k < body.len() {
            let mut s = src_token(body[k]);
            k += 1;
            if body[k - 1] & (1 << 13) != 0 {
                let r = *body.get(k).ok_or("no relative register")?;
                k += 1;
                s.rel = Some((rtype(r), r & 0x7ff, ((r >> 16) & 3) as u8));
            }
            i.src.push(s);
        }
        p.insts.push(i);
    }
    p.jump = vec![usize::MAX; p.insts.len()];
    let mut stack: Vec<usize> = Vec::new();
    for n in 0..p.insts.len() {
        match p.insts[n].op {
            40 | 41 | 38 | 27 => stack.push(n),
            42 => {
                let s = stack.pop().ok_or("else outside an if")?;
                if !matches!(p.insts[s].op, 40 | 41) {
                    return Err("else outside an if".into());
                }
                p.jump[s] = n;
                stack.push(n);
            }
            43 | 39 | 29 => {
                let s = stack.pop().ok_or("unbalanced flow control")?;
                let ok = match p.insts[n].op {
                    43 => matches!(p.insts[s].op, 40 | 41 | 42),
                    39 => p.insts[s].op == 38,
                    _ => p.insts[s].op == 27,
                };
                if !ok {
                    return Err("unbalanced flow control".into());
                }
                p.jump[s] = n;
                p.jump[n] = s;
            }
            30 => p.labels.push((p.insts[n].src.first().ok_or("label without a register")?.reg, n)),
            _ => {}
        }
    }
    if !stack.is_empty() {
        return Err("unclosed flow control".into());
    }
    Ok(p)
}

type V = [f32; 4];

struct Frame {
    /// the rep/loop instruction
    start: usize,
    left: u32,
    /// loop: aL before it, and the step
    al: Option<(i32, i32)>,
}

struct Run<'a> {
    p: &'a Program,
    r: Vec<V>,
    c: Vec<V>,
    i: [[i32; 4]; 16],
    b: [bool; 16],
    pr: [bool; 4],
    al: i32,
    o: [V; 4],
    /// (as ref_dxbc's: out of range, or an infinity, NaN or denormal, whose result D3D9 leaves open)
    taint: std::cell::RefCell<Option<String>>,
}

fn cmp(c: u32, a: f32, b: f32) -> bool {
    match c {
        1 => a > b,
        2 => a == b,
        3 => a >= b,
        4 => a < b,
        5 => a != b,
        6 => a <= b,
        _ => false,
    }
}

impl Run<'_> {
    fn read(&self, s: &Src) -> Result<V, String> {
        let off = match s.rel {
            None => 0,
            Some((15, _, _)) => self.al,
            Some((t, _, _)) => return Err(format!("relative addressing through register type {t}")),
        };
        let v: V = match s.ty {
            0 => *self.r.get(s.reg as usize).ok_or("temp out of range")?,
            1 | 17 => [0.0; 4],
            2 => match self.c.get((s.reg as i32 + off) as usize) {
                Some(c) => *c,
                None => {
                    return Err("constant out of range".into());
                }
            },
            7 => self.i[s.reg as usize & 15].map(|x| x as f32),
            14 => [if self.b[s.reg as usize & 15] { 1.0 } else { 0.0 }; 4],
            15 => [self.al as f32; 4],
            19 => self.pr.map(|x| if x { 1.0 } else { 0.0 }),
            t => return Err(format!("source register type {t}")),
        };
        let v: V = std::array::from_fn(|k| v[s.swz[k] as usize]);
        if v.iter().any(|x| x.is_subnormal()) {
            self.taint.borrow_mut().get_or_insert_with(|| "a denormal (implementations may flush it)".into());
        }
        Ok(match s.md {
            0 => v,
            1 => v.map(|x| -x),
            2 => v.map(|x| x - 0.5),
            3 => v.map(|x| -(x - 0.5)),
            4 => v.map(|x| 2.0 * x - 1.0),
            5 => v.map(|x| -(2.0 * x - 1.0)),
            6 => v.map(|x| 1.0 - x),
            7 => v.map(|x| 2.0 * x),
            8 => v.map(|x| -2.0 * x),
            11 => v.map(f32::abs),
            12 => v.map(|x| -x.abs()),
            m => return Err(format!("source modifier {m}")),
        })
    }
    fn boolean(&self, s: &Src) -> Result<bool, String> {
        let v = match s.ty {
            14 => self.b[s.reg as usize & 15],
            19 => self.pr[s.swz[0] as usize],
            t => return Err(format!("condition register type {t}")),
        };
        Ok(if s.md == 13 { !v } else { v })
    }
    fn write(&mut self, i: &Inst, v: V) -> Result<(), String> {
        let d = i.dst.as_ref().ok_or("no destination")?;
        if (0..4).any(|k| d.mask & (1 << k) != 0 && (!v[k].is_finite() || v[k].is_subnormal())) {
            self.taint.borrow_mut().get_or_insert_with(|| "an infinity, NaN or denormal".into());
        }
        let v = if d.sat { v.map(|x| if x.is_nan() { 0.0 } else { x.clamp(0.0, 1.0) }) } else { v };
        let pred: [bool; 4] = match &i.pred {
            None => [true; 4],
            Some(p) => std::array::from_fn(|k| self.pr[p.swz[k] as usize] != (p.md == 13)),
        };
        if d.ty == 19 {
            for k in 0..4 {
                if d.mask & (1 << k) != 0 && pred[k] {
                    self.pr[k] = v[k] != 0.0;
                }
            }
            return Ok(());
        }
        let slot = match d.ty {
            0 => self.r.get_mut(d.reg as usize).ok_or("temp out of range")?,
            8 => self.o.get_mut(d.reg as usize).ok_or("oC out of range")?,
            t => return Err(format!("destination register type {t}")),
        };
        for k in 0..4 {
            if d.mask & (1 << k) != 0 && pred[k] {
                slot[k] = v[k];
            }
        }
        Ok(())
    }
    fn s(&self, i: &Inst, n: usize) -> Result<V, String> {
        self.read(i.src.get(n).ok_or("missing source")?)
    }

    fn exec(&mut self) -> Result<(), String> {
        let p = self.p;
        let mut pc = 0;
        let mut loops: Vec<Frame> = Vec::new();
        let mut calls: Vec<usize> = Vec::new();
        let mut steps = 0u32;
        while pc < p.insts.len() {
            steps += 1;
            if steps > 1_000_000 {
                return Err("runs too long".into());
            }
            let i = &p.insts[pc];
            pc += 1;
            let dp = |a: V, b: V, n: usize| -> f32 {
                let mut d = a[0] * b[0];
                for k in 1..n {
                    d += a[k] * b[k];
                }
                d
            };
            match i.op {
                0 => {}
                1 => {
                    let a = self.s(i, 0)?;
                    self.write(i, a)?
                }
                2 | 3 | 5 | 10 | 11 | 12 | 13 => {
                    let (a, b) = (self.s(i, 0)?, self.s(i, 1)?);
                    let v = std::array::from_fn(|k| match i.op {
                        2 => a[k] + b[k],
                        3 => a[k] - b[k],
                        5 => a[k] * b[k],
                        10 => a[k].min(b[k]),
                        11 => a[k].max(b[k]),
                        12 => (a[k] < b[k]) as u32 as f32,
                        _ => (a[k] >= b[k]) as u32 as f32,
                    });
                    self.write(i, v)?
                }
                4 => {
                    let (a, b, c) = (self.s(i, 0)?, self.s(i, 1)?, self.s(i, 2)?);
                    self.write(i, std::array::from_fn(|k| a[k] * b[k] + c[k]))?
                }
                // (scalar: the replicated component of the source)
                6 | 7 | 14 | 15 | 78 | 79 => {
                    let x = self.s(i, 0)?[0];
                    let v = match i.op {
                        6 => 1.0 / x,
                        7 => 1.0 / x.abs().sqrt(),
                        14 | 78 => x.exp2(),
                        _ => x.abs().log2(),
                    };
                    self.write(i, [v; 4])?
                }
                8 | 9 => {
                    let d = dp(self.s(i, 0)?, self.s(i, 1)?, if i.op == 8 { 3 } else { 4 });
                    self.write(i, [d; 4])?
                }
                18 => {
                    let (a, b, c) = (self.s(i, 0)?, self.s(i, 1)?, self.s(i, 2)?);
                    self.write(i, std::array::from_fn(|k| a[k] * (b[k] - c[k]) + c[k]))?
                }
                19 => {
                    let a = self.s(i, 0)?;
                    self.write(i, a.map(|x| x - x.floor()))?
                }
                20..=24 => {
                    // m4x4, m4x3, m3x4, m3x3, m3x2: rows from consecutive registers
                    let (n, rows) = [(4, 4), (4, 3), (3, 4), (3, 3), (3, 2)][(i.op - 20) as usize];
                    let a = self.s(i, 0)?;
                    let mut v = [0.0; 4];
                    for (row, out) in v.iter_mut().enumerate().take(rows) {
                        let mut s = i.src[1].clone();
                        s.reg += row as u32;
                        *out = dp(a, self.read(&s)?, n);
                    }
                    self.write(i, v)?
                }
                32 => {
                    let (a, b) = (self.s(i, 0)?[0], self.s(i, 1)?[0]);
                    self.write(i, [a.abs().powf(b); 4])?
                }
                33 => {
                    let (a, b) = (self.s(i, 0)?, self.s(i, 1)?);
                    self.write(i, [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0], 0.0])?
                }
                35 => {
                    let a = self.s(i, 0)?;
                    self.write(i, a.map(f32::abs))?
                }
                36 => {
                    let a = self.s(i, 0)?;
                    let f = 1.0 / dp(a, a, 3).sqrt();
                    self.write(i, a.map(|x| x * f))?
                }
                37 => {
                    let x = self.s(i, 0)?[0];
                    if x.abs() > 1e4 {
                        self.taint.borrow_mut().get_or_insert_with(|| "sincos far from 0".into());
                    }
                    self.write(i, [x.cos(), x.sin(), 0.0, 0.0])?
                }
                80 => {
                    let (a, b, c) = (self.s(i, 0)?, self.s(i, 1)?, self.s(i, 2)?);
                    self.write(i, std::array::from_fn(|k| if a[k] > 0.5 { b[k] } else { c[k] }))?
                }
                88 => {
                    let (a, b, c) = (self.s(i, 0)?, self.s(i, 1)?, self.s(i, 2)?);
                    self.write(i, std::array::from_fn(|k| if a[k] >= 0.0 { b[k] } else { c[k] }))?
                }
                90 => {
                    let (a, b, c) = (self.s(i, 0)?, self.s(i, 1)?, self.s(i, 2)?);
                    self.write(i, [a[0] * b[0] + a[1] * b[1] + c[0]; 4])?
                }
                94 => {
                    let (a, b) = (self.s(i, 0)?, self.s(i, 1)?);
                    let v = std::array::from_fn(|k| cmp(i.cmp, a[k], b[k]) as u32 as f32);
                    self.write(i, v)?
                }
                // (applied before the program runs)
                47 | 48 | 81 => {}
                40 | 41 => {
                    let t = if i.op == 40 { self.boolean(&i.src[0])? } else { cmp(i.cmp, self.s(i, 0)?[0], self.s(i, 1)?[0]) };
                    if !t {
                        pc = p.jump[pc - 1] + 1;
                    }
                }
                42 => pc = p.jump[pc - 1] + 1,
                43 => {}
                38 | 27 => {
                    let n = i.src.last().ok_or("rep/loop without a count")?;
                    if n.ty != 7 {
                        return Err("rep/loop count not an i#".into());
                    }
                    let iv = self.i[n.reg as usize & 15];
                    let count = iv[0].clamp(0, 255) as u32;
                    if count == 0 {
                        pc = p.jump[pc - 1] + 1;
                    } else {
                        let al = (i.op == 27).then(|| {
                            let old = self.al;
                            self.al = iv[1];
                            (old, iv[2])
                        });
                        loops.push(Frame { start: pc - 1, left: count, al });
                    }
                }
                39 | 29 => {
                    let f = loops.last_mut().ok_or("endrep outside a loop")?;
                    f.left -= 1;
                    if f.left > 0 {
                        if let Some((_, step)) = f.al {
                            self.al += step;
                        }
                        pc = f.start + 1;
                    } else {
                        let f = loops.pop().unwrap();
                        if let Some((old, _)) = f.al {
                            self.al = old;
                        }
                    }
                }
                44 | 45 | 96 => {
                    let t = match i.op {
                        44 => true,
                        45 => cmp(i.cmp, self.s(i, 0)?[0], self.s(i, 1)?[0]),
                        _ => self.boolean(&i.src[0])?,
                    };
                    if t {
                        let f = loops.pop().ok_or("break outside a loop")?;
                        if let Some((old, _)) = f.al {
                            self.al = old;
                        }
                        pc = p.jump[f.start] + 1;
                    }
                }
                25 | 26 => {
                    let l = i.src.first().ok_or("call without a label")?.reg;
                    if i.op == 26 && !self.boolean(&i.src[1])? {
                        continue;
                    }
                    let at = p.labels.iter().find(|x| x.0 == l).ok_or("call to a missing label")?.1;
                    if calls.len() >= 16 {
                        return Err("calls nested too deep".into());
                    }
                    calls.push(pc);
                    pc = at + 1;
                }
                28 => match calls.pop() {
                    Some(r) => pc = r,
                    None => return Ok(()),
                },
                // (subroutines follow the main program's ret: reached only through call)
                30 => return Ok(()),
                op => return Err(format!("opcode {op}")),
            }
        }
        Ok(())
    }
}

/// Runs a pixel shader with float constants `c` (c0, c1, ...); returns oC0..oC3, and why they
/// may not be what every implementation gives.
pub fn run(p: &Program, c: &[[f32; 4]]) -> Result<([[f32; 4]; 4], Option<String>), String> {
    let mut cs = vec![[0.0; 4]; 256];
    cs[..c.len()].copy_from_slice(c);
    let mut r = Run { p, r: vec![[0.0; 4]; 32], c: cs, i: [[0; 4]; 16], b: [false; 16], pr: [false; 4], al: 0, o: [[0.0; 4]; 4], taint: Default::default() };
    // def, defi, defb: whatever the application set
    let mut seen = Vec::new();
    for i in &p.insts {
        let Some(d) = &i.dst else { continue };
        if matches!(i.op, 47 | 48 | 81) {
            if seen.contains(&(i.op, d.reg)) {
                return Err("a constant defined twice".into());
            }
            seen.push((i.op, d.reg));
        }
        match i.op {
            81 => {
                if let Some(c) = r.c.get_mut(d.reg as usize) {
                    *c = i.raw.map(f32::from_bits);
                }
            }
            48 => r.i[d.reg as usize & 15] = i.raw.map(|x| x as i32),
            47 => r.b[d.reg as usize & 15] = i.raw[0] != 0,
            _ => {}
        }
    }
    r.exec()?;
    Ok((r.o, r.taint.into_inner()))
}
