//! Shader model 2/3 output: lower.rs's instructions (in its sm3 mode, where ints and bools are
//! floats and true is 1.0) as Direct3D 9 shader assembly, assembled by d3d9asm.rs, with a CTAB
//! comment (the constant table D3DXGetShaderConstantTable and ID3DXConstantTable read).
//! Instruction count limits are not checked: the bytecode only feeds a WGSL translation (dxwgsl).

use std::collections::HashMap;

use super::sm4::*;

/// the operations this output has (anything else is an error here)
pub fn float_op(op: u32) -> bool {
    matches!(
        op,
        ADD | MUL
            | MAD
            | DIV
            | MIN
            | MAX
            | MOV
            | MOVC
            | EQ
            | NE
            | LT
            | GE
            | FRC
            | ROUND_NI
            | ROUND_PI
            | ROUND_Z
            | ROUND_NE
            | SQRT
            | RSQ
            | EXP
            | LOG
            | RCP
            | DP2
            | DP3
            | DP4
    )
}

fn op_name(op: u32) -> String {
    match op {
        CONTINUE => "continue".into(),
        SWITCH | CASE | DEFAULT | ENDSWITCH => "switch".into(),
        AND | OR | XOR | NOT | ISHL | ISHR | USHR => "bit operations".into(),
        COUNTBITS | FIRSTBIT_HI | FIRSTBIT_LO | FIRSTBIT_SHI | BFREV => "bit counting".into(),
        LD | RESINFO | GATHER4 | SAMPLE_C | SAMPLE_C_LZ => "this texture operation".into(),
        F32TOF16 | F16TOF32 => "half conversion".into(),
        x => format!("operation {x:#x}"),
    }
}

const SW: [char; 4] = ['x', 'y', 'z', 'w'];

fn mask_str(m: u8) -> String {
    if m == 0xf || m == 0 {
        return String::new();
    }
    let mut s = String::from(".");
    for (k, c) in SW.iter().enumerate() {
        if m & 1 << k != 0 {
            s.push(*c);
        }
    }
    s
}

fn swz_str(s: [u8; 4]) -> String {
    if s == [0, 1, 2, 3] {
        return String::new();
    }
    let mut o = String::from(".");
    for k in s {
        o.push(SW[k as usize]);
    }
    o
}

fn num(v: u32) -> String {
    let f = f32::from_bits(v);
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 {
            "1e39".into()
        } else {
            "-1e39".into()
        }
    } else {
        format!("{f:?}")
    }
}

/// usage of a semantic (dcl_*)
fn usage(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_uppercase().as_str() {
        "SV_POSITION" | "POSITION" => "position",
        "POSITIONT" => "positiont",
        "NORMAL" => "normal",
        "TEXCOORD" => "texcoord",
        "COLOR" => "color",
        "TANGENT" => "tangent",
        "BINORMAL" => "binormal",
        "BLENDWEIGHT" => "blendweight",
        "BLENDINDICES" => "blendindices",
        "PSIZE" => "psize",
        "FOG" => "fog",
        "DEPTH" => "depth",
        "SV_TARGET" => "color",
        _ => return None,
    })
}

/// A component of a source operand, replicated.
fn comp(s: &Src, k: usize) -> Src {
    match s {
        Src::R { r, swz, neg, abs, one } => {
            let c = if *one { swz[0] } else { swz[k] };
            Src::R { r: *r, swz: [c; 4], neg: *neg, abs: *abs, one: true }
        }
        Src::Imm(v, scalar) => Src::Imm([if *scalar { v[0] } else { v[k] }; 4], true),
        s => s.clone(),
    }
}

fn neg(s: &Src) -> Src {
    match s {
        Src::R { r, swz, neg, abs, one } => Src::R { r: *r, swz: *swz, neg: !neg, abs: *abs, one: *one },
        Src::Imm(v, sc) => Src::Imm(v.map(|x| if x == 0 { 0 } else { x ^ 0x8000_0000 }), *sc),
        s => s.clone(),
    }
}

fn abs(s: &Src) -> Src {
    match s {
        Src::R { r, swz, one, .. } => Src::R { r: *r, swz: *swz, neg: false, abs: true, one: *one },
        Src::Imm(v, sc) => Src::Imm(v.map(|x| x & 0x7fff_ffff), *sc),
        s => s.clone(),
    }
}

/// a scratch temp (after the allocated ones), all components
fn scr(k: u32) -> Src {
    Src::R { r: Reg { f: RegF::Temp(1000 + k), rel: None }, swz: [0, 1, 2, 3], neg: false, abs: false, one: false }
}

/// A constant table type (D3DXSHADER_TYPEINFO)
fn ctype(v: &CbVar) -> (u16, u16, u16, u16, u16) {
    let ty = match v.ty {
        1 => 1,
        2 | 19 => 2,
        x => x,
    };
    (v.class, ty, v.rows, v.cols, v.elems.max(1))
}

struct Gen<'a> {
    s: &'a Shader,
    temps: u32,
    lines: Vec<String>,
    /// def registers: their components
    defs: Vec<[Option<u32>; 4]>,
    def_base: u32,
    /// (cbuffer index, old register) -> c register
    cmap: HashMap<(usize, u32), u32>,
    ins: HashMap<u32, String>,
    outs: HashMap<u32, String>,
    /// sampler slot -> texture dimension (sm4 numbering)
    samplers: Vec<(u32, u32)>,
    loops: bool,
}

impl Gen<'_> {
    fn line(&mut self, l: String) {
        self.lines.push(l);
    }

    /// a def constant holding `v`: register and swizzle
    fn konst(&mut self, v: [u32; 4]) -> String {
        let mut distinct: Vec<u32> = Vec::new();
        for x in v {
            if !distinct.contains(&x) {
                distinct.push(x);
            }
        }
        let fits = |d: &[Option<u32>; 4]| {
            let missing = distinct.iter().filter(|x| !d.contains(&Some(**x))).count();
            missing <= d.iter().filter(|c| c.is_none()).count()
        };
        let i = match self.defs.iter().position(fits) {
            Some(i) => i,
            None => {
                self.defs.push([None; 4]);
                self.defs.len() - 1
            }
        };
        let d = &mut self.defs[i];
        for x in &distinct {
            if !d.contains(&Some(*x)) {
                let f = d.iter().position(|c| c.is_none()).unwrap();
                d[f] = Some(*x);
            }
        }
        let swz: [u8; 4] = v.map(|x| d.iter().position(|c| *c == Some(x)).unwrap() as u8);
        format!("c{}{}", self.def_base + i as u32, swz_str(swz))
    }

    fn reg(&self, r: &Reg) -> Result<String, String> {
        Ok(match r.f {
            RegF::Temp(t) if t >= 1000 => format!("r{}", self.temps + t - 1000),
            RegF::Temp(t) => format!("r{t}"),
            RegF::Input(i) => self.ins.get(&i).cloned().ok_or("an input without a register")?,
            RegF::Output(i) => self.outs.get(&i).cloned().ok_or("an output without a register")?,
            RegF::Depth => "oDepth".into(),
            RegF::Cb(sym, k) => {
                let cb = self.s.cb_map[sym as usize].ok_or("an unused constant")?;
                let n = self.cmap.get(&(cb, k)).ok_or("constant without a register")?;
                if r.rel.is_some() {
                    return Ok(format!("c[a0.x + {n}]"));
                }
                format!("c{n}")
            }
            _ => return Err("indexable temps and immediate constant buffers are not available in shader model 2/3".into()),
        })
    }

    fn src(&mut self, s: &Src) -> Result<String, String> {
        Ok(match s {
            Src::R { r, swz, neg, abs, one } => {
                let name = self.reg(r)?;
                let sw = if *one { [swz[0]; 4] } else { *swz };
                let sw = swz_str(sw);
                let sw = if sw == ".xxxx" {
                    ".x".into()
                } else if sw == ".yyyy" {
                    ".y".into()
                } else if sw == ".zzzz" {
                    ".z".into()
                } else if sw == ".wwww" {
                    ".w".into()
                } else {
                    sw
                };
                format!("{}{}{}{}", if *neg { "-" } else { "" }, name, if *abs { "_abs" } else { "" }, sw)
            }
            Src::Imm(v, scalar) => {
                let v = if *scalar { [v[0]; 4] } else { *v };
                self.konst(v)
            }
            Src::Samp(_) | Src::Tex(..) => return Err("texture operand".into()),
        })
    }

    fn dst(&self, d: &Dst) -> Result<String, String> {
        let n = self.reg(&d.r)?;
        if d.r.f == RegF::Depth {
            return Ok(n);
        }
        Ok(format!("{n}{}", mask_str(d.mask)))
    }

    /// `op dst, srcs` (dst a scratch register with `mask` when `to` is None)
    fn op(&mut self, op: &str, sat: bool, d: &str, srcs: &[&Src]) -> Result<(), String> {
        let mut l = format!("{op}{} {d}", if sat { "_sat" } else { "" });
        for s in srcs {
            let t = self.src(s)?;
            l.push_str(", ");
            l.push_str(&t);
        }
        self.line(l);
        Ok(())
    }

    fn scr_d(&self, k: u32, mask: u8) -> String {
        format!("r{}{}", self.temps + k, mask_str(mask))
    }

    fn sampler(&self, s: &Src, t: &Src) -> Result<(String, u32), String> {
        let Src::Samp(sym) = s else {
            return Err("a sampler is needed".into());
        };
        let n = self.s.samp_map[*sym as usize].ok_or("an unused sampler")?;
        let slot = self.s.samps[n].slot;
        let dim = match t {
            Src::Tex(t, _) => self.s.texs[self.s.tex_map[*t as usize].ok_or("an unused texture")?].dim,
            _ => 3,
        };
        Ok((format!("s{slot}"), dim))
    }

    fn ins(&mut self, i: &Ins) -> Result<(), String> {
        let pixel = self.s.pixel;
        // (relative constants: the address register first)
        let mut rel = None;
        for s in &i.src {
            if let Src::R { r: Reg { rel: Some(x), .. }, .. } = s {
                if rel.is_some_and(|y| y != *x) {
                    return Err("two indexed constant arrays in one operation".into());
                }
                rel = Some(*x);
            }
        }
        if let Some((t, c)) = rel {
            if pixel {
                return Err("indexed constants in a pixel shader".into());
            }
            let r = self.reg(&Reg { f: RegF::Temp(t), rel: None })?;
            self.line(format!("mova a0.x, {r}.{}", SW[c as usize]));
        }
        let a = |k: usize| i.src.get(k).cloned().ok_or_else(|| "missing operand".to_string());
        let sat = i.sat;
        let one = Src::Imm([0x3f80_0000; 4], true);
        let zero = Src::Imm([0; 4], true);
        match i.op {
            RET => {}
            IF | BREAKC => {
                let c = self.src(&comp(&a(0)?, 0))?;
                let z = self.src(&zero)?;
                let name = if i.op == IF { "ifc" } else { "breakc" };
                self.line(format!("{name}_{} {c}, {z}", if i.nz { "ne" } else { "eq" }));
            }
            ELSE => self.line("else".into()),
            ENDIF => self.line("endif".into()),
            LOOP => {
                self.loops = true;
                self.line("rep i0".into());
            }
            ENDLOOP => self.line("endrep".into()),
            BREAK => self.line("break".into()),
            DISCARD => {
                if !pixel {
                    return Err("discard in a vertex shader".into());
                }
                let s = a(0)?;
                let n = i.ctl as usize;
                if n > 0 {
                    // clip(): any component below 0
                    let d = self.scr_d(0, 0xf);
                    self.op("mov", false, &d, &[&s])?;
                    let m = self.scr_d(0, ((1u16 << n) - 1) as u8);
                    self.line(format!("texkill {m}"));
                    return Ok(());
                }
                let c = comp(&s, 0);
                if let Src::Imm(v, _) = c {
                    if (v[0] != 0) != i.nz {
                        return Ok(());
                    }
                    let d = self.scr_d(0, 1);
                    self.op("mov", false, &d, &[&Src::Imm([0xbf80_0000; 4], true)])?;
                } else if i.nz {
                    let d = self.scr_d(0, 1);
                    self.op("mov", false, &d, &[&neg(&abs(&c))])?;
                } else {
                    let d = self.scr_d(0, 1);
                    self.op("cmp", false, &d, &[&neg(&abs(&c)), &Src::Imm([0xbf80_0000; 4], true), &zero])?;
                }
                let m = self.scr_d(0, 1);
                self.line(format!("texkill {m}"));
            }
            _ => {
                let Some(d0) = i.dst.first() else {
                    return Err(op_name(i.op));
                };
                let m = d0.mask;
                let d = if d0.r.f == RegF::Null { String::new() } else { self.dst(d0)? };
                let simple = match i.op {
                    MOV => Some("mov"),
                    ADD => Some("add"),
                    MUL => Some("mul"),
                    MAD => Some("mad"),
                    MIN => Some("min"),
                    MAX => Some("max"),
                    FRC => Some("frc"),
                    DP3 => Some("dp3"),
                    DP4 => Some("dp4"),
                    DERIV_RTX => Some("dsx"),
                    DERIV_RTY => Some("dsy"),
                    _ => None,
                };
                if let Some(name) = simple {
                    if matches!(i.op, DERIV_RTX | DERIV_RTY) && !pixel {
                        return Err("derivatives in a vertex shader".into());
                    }
                    let srcs: Vec<Src> = i.src.clone();
                    let refs: Vec<&Src> = srcs.iter().collect();
                    return self.op(name, sat, &d, &refs);
                }
                let s0 = self.scr_d(0, m);
                let s1 = self.scr_d(1, m);
                match i.op {
                    DIV | SQRT | RSQ | RCP | EXP | LOG => {
                        let (x, op) = if i.op == DIV { (a(1)?, "rcp") } else { (a(0)?, "") };
                        for k in 0..4 {
                            if m & 1 << k == 0 {
                                continue;
                            }
                            let dk = self.scr_d(0, 1 << k);
                            let ck = comp(&x, k);
                            match i.op {
                                DIV => self.op(op, false, &dk, &[&ck])?,
                                SQRT => {
                                    self.op("rsq", false, &dk, &[&ck])?;
                                    let sk = comp(&scr(0), k);
                                    self.op("rcp", false, &dk, &[&sk])?;
                                }
                                RSQ => self.op("rsq", false, &dk, &[&ck])?,
                                RCP => self.op("rcp", false, &dk, &[&ck])?,
                                EXP => self.op("exp", false, &dk, &[&ck])?,
                                _ => self.op("log", false, &dk, &[&ck])?,
                            }
                        }
                        if i.op == DIV {
                            self.op("mul", sat, &d, &[&a(0)?, &scr(0)])?;
                        } else {
                            self.op("mov", sat, &d, &[&scr(0)])?;
                        }
                    }
                    ROUND_NI => {
                        self.op("frc", false, &s0, &[&a(0)?])?;
                        self.op("add", sat, &d, &[&a(0)?, &neg(&scr(0))])?;
                    }
                    ROUND_PI => {
                        self.op("frc", false, &s0, &[&neg(&a(0)?)])?;
                        self.op("add", sat, &d, &[&a(0)?, &scr(0)])?;
                    }
                    ROUND_NE => {
                        self.op("add", false, &s0, &[&a(0)?, &Src::Imm([0x3f00_0000; 4], true)])?;
                        self.op("frc", false, &s1, &[&scr(0)])?;
                        self.op("add", sat, &d, &[&scr(0), &neg(&scr(1))])?;
                    }
                    ROUND_Z => {
                        let x = a(0)?;
                        self.op("frc", false, &s0, &[&abs(&x)])?;
                        self.op("add", false, &s0, &[&abs(&x), &neg(&scr(0))])?;
                        if pixel {
                            self.op("cmp", sat, &d, &[&x, &scr(0), &neg(&scr(0))])?;
                        } else {
                            self.op("sge", false, &s1, &[&x, &zero])?;
                            self.op("mad", false, &s1, &[&scr(1), &Src::Imm([0x4000_0000; 4], true), &Src::Imm([0xbf80_0000; 4], true)])?;
                            self.op("mul", sat, &d, &[&scr(0), &scr(1)])?;
                        }
                    }
                    EQ | NE | LT | GE => {
                        let (x, y) = (a(0)?, a(1)?);
                        if pixel {
                            self.op("add", false, &s0, &[&x, &neg(&y)])?;
                            let na = neg(&abs(&scr(0)));
                            match i.op {
                                EQ => self.op("cmp", sat, &d, &[&na, &one, &zero])?,
                                NE => self.op("cmp", sat, &d, &[&na, &zero, &one])?,
                                LT => self.op("cmp", sat, &d, &[&scr(0), &zero, &one])?,
                                _ => self.op("cmp", sat, &d, &[&scr(0), &one, &zero])?,
                            }
                        } else {
                            match i.op {
                                LT => self.op("slt", sat, &d, &[&x, &y])?,
                                GE => self.op("sge", sat, &d, &[&x, &y])?,
                                _ => {
                                    self.op("sge", false, &s0, &[&x, &y])?;
                                    self.op("sge", false, &s1, &[&y, &x])?;
                                    if i.op == EQ {
                                        self.op("mul", sat, &d, &[&scr(0), &scr(1)])?;
                                    } else {
                                        self.op("mul", false, &s0, &[&scr(0), &scr(1)])?;
                                        self.op("add", sat, &d, &[&neg(&scr(0)), &one])?;
                                    }
                                }
                            }
                        }
                    }
                    MOVC => {
                        let (c, x, y) = (a(0)?, a(1)?, a(2)?);
                        if pixel {
                            self.op("cmp", sat, &d, &[&neg(&abs(&c)), &y, &x])?;
                        } else {
                            self.op("add", false, &s0, &[&x, &neg(&y)])?;
                            self.op("mad", sat, &d, &[&c, &scr(0), &y])?;
                        }
                    }
                    DP2 => {
                        if pixel {
                            self.op("dp2add", sat, &d, &[&a(0)?, &a(1)?, &zero])?;
                        } else {
                            let s = self.scr_d(0, 3);
                            self.op("mul", false, &s, &[&a(0)?, &a(1)?])?;
                            self.op("add", sat, &d, &[&comp(&scr(0), 0), &comp(&scr(0), 1)])?;
                        }
                    }
                    SINCOS => {
                        let x = a(0)?;
                        let dsin = i.dst.first().filter(|d| d.r.f != RegF::Null).cloned();
                        let dcos = i.dst.get(1).filter(|d| d.r.f != RegF::Null).cloned();
                        let mm = dsin.as_ref().map_or(0, |d| d.mask) | dcos.as_ref().map_or(0, |d| d.mask);
                        for k in 0..4 {
                            if mm & 1 << k == 0 {
                                continue;
                            }
                            let sc = self.scr_d(0, 3);
                            let ck = comp(&x, k);
                            if self.s.major < 3 {
                                // (D3DSINCOSCONST1/2)
                                let c1 = Src::Imm(
                                    [(-1.5500992e-6f32).to_bits(), (-2.1701389e-5f32).to_bits(), 0.0026041667f32.to_bits(), 0.00026041668f32.to_bits()],
                                    false,
                                );
                                let c2 = Src::Imm([(-0.020833334f32).to_bits(), (-0.125f32).to_bits(), 1f32.to_bits(), 0.5f32.to_bits()], false);
                                self.op("sincos", false, &sc, &[&ck, &c1, &c2])?;
                            } else {
                                self.op("sincos", false, &sc, &[&ck])?;
                            }
                            let s1 = self.scr_d(1, 1 << k);
                            let s2 = self.scr_d(2, 1 << k);
                            self.op("mov", false, &s1, &[&comp(&scr(0), 1)])?;
                            self.op("mov", false, &s2, &[&comp(&scr(0), 0)])?;
                        }
                        if let Some(ds) = dsin {
                            let dd = self.dst(&ds)?;
                            self.op("mov", sat, &dd, &[&scr(1)])?;
                        }
                        if let Some(dc) = dcos {
                            let dd = self.dst(&dc)?;
                            self.op("mov", sat, &dd, &[&scr(2)])?;
                        }
                    }
                    SAMPLE | SAMPLE_L | SAMPLE_B | SAMPLE_D => {
                        if i.offset != [0; 3] {
                            return Err("texel offsets need shader model 4".into());
                        }
                        let (coord, tex, smp) = (a(0)?, a(1)?, a(2)?);
                        let (sname, dim) = self.sampler(&smp, &tex)?;
                        if !self.samplers.iter().any(|x| x.0 == sname[1..].parse::<u32>().unwrap()) {
                            self.samplers.push((sname[1..].parse().unwrap(), dim));
                        }
                        let tswz = match tex {
                            Src::Tex(_, s) => s,
                            _ => [0, 1, 2, 3],
                        };
                        let direct = tswz == [0, 1, 2, 3];
                        let td = if direct { d.clone() } else { self.scr_d(1, 0xf) };
                        let op = if !pixel && i.op == SAMPLE { SAMPLE_L } else { i.op };
                        match op {
                            SAMPLE => {
                                let c = self.src(&coord)?;
                                self.line(format!("texld {td}, {c}, {sname}"));
                            }
                            SAMPLE_D => {
                                let c = self.src(&coord)?;
                                let dx = self.src(&a(3)?)?;
                                let dy = self.src(&a(4)?)?;
                                self.line(format!("texldd {td}, {c}, {sname}, {dx}, {dy}"));
                            }
                            _ => {
                                let s = self.scr_d(0, 0xf);
                                self.op("mov", false, &s, &[&coord])?;
                                let w = self.scr_d(0, 8);
                                let lod = if i.op == SAMPLE { zero.clone() } else { comp(&a(3)?, 0) };
                                self.op("mov", false, &w, &[&lod])?;
                                let name = if op == SAMPLE_L { "texldl" } else { "texldb" };
                                self.line(format!("{name} {td}, r{}, {sname}", self.temps));
                            }
                        }
                        if sat && direct {
                            self.op("mov_sat", false, &d, &[&Src::R { r: d0.r, swz: [0, 1, 2, 3], neg: false, abs: false, one: false }])?;
                        }
                        if !direct {
                            let mut sw = [0u8; 4];
                            for (k, x) in sw.iter_mut().enumerate() {
                                *x = tswz[k];
                            }
                            self.op("mov", sat, &d, &[&Src::R { r: Reg { f: RegF::Temp(1001), rel: None }, swz: sw, neg: false, abs: false, one: false }])?;
                        }
                    }
                    op => return Err(format!("{} is not available in shader model 2/3", op_name(op))),
                }
            }
        }
        Ok(())
    }
}

/// A uniform's place in the constant table.
pub struct Uni {
    pub name: String,
    pub reg: u32,
    pub count: u32,
}

fn put_str(b: &mut Vec<u8>, s: &str) -> u32 {
    let at = b.len() as u32;
    b.extend_from_slice(s.as_bytes());
    b.push(0);
    while b.len() % 4 != 0 {
        b.push(0);
    }
    at
}

fn set16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn set32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// a D3DXSHADER_TYPEINFO (and its members) at the end of `b`
fn put_type(b: &mut Vec<u8>, v: &CbVar) -> u32 {
    let at = b.len();
    b.resize(at + 16, 0);
    let (class, ty, rows, cols, elems) = ctype(v);
    for (k, x) in [class, ty, rows, cols, elems, v.members.len() as u16].iter().enumerate() {
        set16(b, at + 2 * k, *x);
    }
    if !v.members.is_empty() {
        let mi = b.len();
        b.resize(mi + 8 * v.members.len(), 0);
        set32(b, at + 12, mi as u32);
        for (k, m) in v.members.iter().enumerate() {
            let n = put_str(b, &m.name);
            let t = put_type(b, m);
            set32(b, mi + 8 * k, n);
            set32(b, mi + 8 * k + 4, t);
        }
    }
    at as u32
}

/// The CTAB body: (name, register set, register, count, variable).
fn ctab(target: &str, entries: &[(String, u16, u32, u32, CbVar)]) -> Vec<u8> {
    let mut b = vec![0u8; 28];
    set32(&mut b, 0, 28);
    let info = b.len();
    b.resize(info + 20 * entries.len(), 0);
    let creator = put_str(&mut b, "sake HLSL compiler");
    let tgt = put_str(&mut b, target);
    set32(&mut b, 4, creator);
    let v: u32 = if target.starts_with("ps") { 0xffff_0000 } else { 0xfffe_0000 };
    let digits: Vec<u32> = target.split('_').skip(1).map(|x| x.parse().unwrap_or(0)).collect();
    set32(&mut b, 8, v | digits.first().copied().unwrap_or(0) << 8 | digits.get(1).copied().unwrap_or(0));
    set32(&mut b, 12, entries.len() as u32);
    set32(&mut b, 16, info as u32);
    set32(&mut b, 24, tgt);
    for (k, (name, set, reg, count, var)) in entries.iter().enumerate() {
        let e = info + 20 * k;
        let n = put_str(&mut b, name);
        let t = put_type(&mut b, var);
        set32(&mut b, e, n);
        set16(&mut b, e + 4, *set);
        set16(&mut b, e + 6, *reg as u16);
        set16(&mut b, e + 8, *count as u16);
        set32(&mut b, e + 12, t);
    }
    b
}

/// The shader's assembly text and constant table.
pub fn assembly(mut s: Shader) -> Result<(String, Vec<u8>), String> {
    let temps = alloc(&mut s.code, &s.marks);
    let (pixel, major, minor) = (s.pixel, s.major.max(2), if s.major < 2 { 0 } else { s.minor });
    let target = format!("{}_{major}_{minor}", if pixel { "ps" } else { "vs" });
    // uniforms: explicit registers first, then the lowest free
    let mut taken: Vec<(u32, u32)> = Vec::new();
    let mut cmap = HashMap::new();
    let mut entries: Vec<(String, u16, u32, u32, CbVar)> = Vec::new();
    let mut vars: Vec<(usize, &CbVar)> = Vec::new();
    for (ci, cb) in s.cbs.iter().enumerate() {
        for v in cb.vars.iter().filter(|v| v.used) {
            vars.push((ci, v));
        }
    }
    vars.sort_by_key(|(_, v)| !v.explicit);
    for (ci, v) in vars {
        let old = v.offset / 16;
        let count = v.size.div_ceil(16).max(1);
        let reg = if v.explicit {
            old
        } else {
            let mut r = 0;
            while taken.iter().any(|&(a, n)| r < a + n && a < r + count) {
                r += 1;
            }
            r
        };
        taken.push((reg, count));
        for k in 0..count {
            cmap.insert((ci, old + k), reg + k);
        }
        entries.push((v.name.clone(), 2, reg, count, v.clone()));
    }
    let def_base = taken.iter().map(|&(a, n)| a + n).max().unwrap_or(0);
    // inputs and outputs
    let mut ins = HashMap::new();
    let mut outs = HashMap::new();
    let mut head = vec![target.clone()];
    for e in &s.inputs {
        let up = e.name.to_ascii_uppercase();
        let mask = mask_str(e.mask);
        let name = if pixel && up == "SV_POSITION" {
            if major < 3 {
                return Err("VPOS needs ps_3_0".into());
            }
            head.push("dcl vPos.xy".into());
            "vPos".to_string()
        } else if pixel && up == "SV_ISFRONTFACE" {
            if major < 3 {
                return Err("VFACE needs ps_3_0".into());
            }
            head.push("dcl vFace".into());
            "vFace".to_string()
        } else if pixel && major < 3 {
            let r = match up.as_str() {
                "TEXCOORD" => format!("t{}", e.index),
                "COLOR" => format!("v{}", e.index),
                _ => return Err(format!("ps_2_0 input semantic {}{}", e.name, e.index)),
            };
            head.push(format!("dcl {r}{mask}"));
            r
        } else {
            let u = usage(&e.name).ok_or(format!("input semantic {}{} in shader model 2/3", e.name, e.index))?;
            let r = format!("v{}", e.reg);
            head.push(format!("dcl_{u}{} {r}{mask}", e.index));
            r
        };
        ins.insert(e.reg, name);
    }
    for e in &s.outputs {
        let up = e.name.to_ascii_uppercase();
        let name = if pixel {
            match up.as_str() {
                "SV_TARGET" => format!("oC{}", e.index),
                "SV_DEPTH" => "oDepth".into(),
                _ => return Err(format!("pixel shader output semantic {}{}", e.name, e.index)),
            }
        } else if major < 3 {
            match up.as_str() {
                "SV_POSITION" => "oPos".into(),
                "TEXCOORD" => format!("oT{}", e.index),
                "COLOR" => format!("oD{}", e.index),
                "FOG" => "oFog".into(),
                "PSIZE" => "oPts".into(),
                _ => return Err(format!("vs_2_0 output semantic {}{}", e.name, e.index)),
            }
        } else {
            let u = usage(&e.name).ok_or(format!("output semantic {}{} in shader model 2/3", e.name, e.index))?;
            let r = format!("o{}", e.reg);
            head.push(format!("dcl_{u}{} {r}{}", e.index, mask_str(e.mask)));
            r
        };
        if !e.depth {
            outs.insert(e.reg, name);
        }
    }
    let mut g = Gen { s: &s, temps, lines: Vec::new(), defs: Vec::new(), def_base, cmap, ins, outs, samplers: Vec::new(), loops: false };
    let code = s.code.clone();
    for i in &code {
        g.ins(i)?;
    }
    // samplers
    let mut samplers = g.samplers.clone();
    samplers.sort();
    for (slot, dim) in &samplers {
        let t = match dim {
            6 => "cube",
            5 => "volume",
            _ => "2d",
        };
        head.push(format!("dcl_{t} s{slot}"));
        if let Some(sm) = s.samps.iter().find(|x| x.slot == *slot) {
            let ty = match dim {
                6 => 14,
                5 => 13,
                2 => 11,
                _ => 12,
            };
            let var =
                CbVar { name: sm.name.clone(), offset: 0, size: 0, class: 4, ty, rows: 1, cols: 1, elems: 1, used: true, explicit: false, members: Vec::new() };
            entries.push((sm.name.clone(), 3, *slot, 1, var));
        }
    }
    for (k, d) in g.defs.iter().enumerate() {
        let v: Vec<String> = d.iter().map(|c| num(c.unwrap_or(0))).collect();
        head.push(format!("def c{}, {}", def_base + k as u32, v.join(", ")));
    }
    if g.loops {
        head.push("defi i0, 255, 0, 1, 0".into());
    }
    let mut text = head.join("\n");
    text.push('\n');
    text.push_str(&g.lines.join("\n"));
    text.push('\n');
    // (CTAB types: D3D10's 19 uint is an int)
    for e in &mut entries {
        if e.4.ty == 19 {
            e.4.ty = 2;
        }
    }
    Ok((text, ctab(&target, &entries)))
}

/// The shader's bytecode (with its CTAB comment after the version token).
pub fn finish(s: Shader) -> Result<Vec<u8>, String> {
    let (text, tab) = assembly(s)?;
    let words = crate::d3d9asm::assemble(&text).map_err(|e| format!("internal error: {e}\n{text}"))?;
    let mut out = Vec::new();
    out.extend_from_slice(&words[0].to_le_bytes());
    let mut body = b"CTAB".to_vec();
    body.extend_from_slice(&tab);
    while body.len() % 4 != 0 {
        body.push(0);
    }
    out.extend_from_slice(&(0xfffe | ((body.len() as u32 / 4) << 16)).to_le_bytes());
    out.extend_from_slice(&body);
    for w in &words[1..] {
        out.extend_from_slice(&w.to_le_bytes());
    }
    Ok(out)
}
