//! Direct3D 9 shader bytecode (vs/ps 1.x-3.0) to WGSL.
//!
//! The registers become vec4f variables; constants come from a uniform block (`Consts`) the
//! device fills per draw. Vertex outputs and pixel inputs meet at fixed locations by semantic
//! (`location`), so any vertex shader goes with any pixel shader: the pixel shader is generated
//! knowing which locations the vertex stage writes (`Link`).

use std::fmt::Write;

// register types
const TEMP: u32 = 0;
const INPUT: u32 = 1;
const CONST: u32 = 2;
const ADDR: u32 = 3; // vs: a0, ps: t# (texture)
const RASTOUT: u32 = 4;
const ATTROUT: u32 = 5;
const OUTPUT: u32 = 6; // vs < 3: oT#, vs 3: o#
const CONSTINT: u32 = 7;
const COLOROUT: u32 = 8;
const DEPTHOUT: u32 = 9;
const SAMPLER: u32 = 10;
const CONST2: u32 = 11;
const CONST3: u32 = 12;
const CONST4: u32 = 13;
const CONSTBOOL: u32 = 14;
const LOOP: u32 = 15;
const MISCTYPE: u32 = 17;
const PREDICATE: u32 = 19;

// usages
pub const USAGE_POSITION: u32 = 0;
pub const USAGE_BLENDWEIGHT: u32 = 1;
pub const USAGE_BLENDINDICES: u32 = 2;
pub const USAGE_NORMAL: u32 = 3;
pub const USAGE_PSIZE: u32 = 4;
pub const USAGE_TEXCOORD: u32 = 5;
pub const USAGE_TANGENT: u32 = 6;
pub const USAGE_BINORMAL: u32 = 7;
pub const USAGE_TESSFACTOR: u32 = 8;
pub const USAGE_POSITIONT: u32 = 9;
pub const USAGE_COLOR: u32 = 10;
pub const USAGE_FOG: u32 = 11;
pub const USAGE_DEPTH: u32 = 12;
pub const USAGE_SAMPLE: u32 = 13;

/// Where a vertex output / pixel input of a semantic goes (None: not passed).
pub fn location(usage: u32, index: u32) -> Option<u32> {
    match (usage, index) {
        (USAGE_COLOR, 0..=1) => Some(index),
        (USAGE_TEXCOORD, 0..=7) => Some(2 + index),
        (USAGE_FOG, 0) => Some(10),
        (USAGE_NORMAL, 0) => Some(11),
        (USAGE_TANGENT, 0) => Some(12),
        (USAGE_BINORMAL, 0) => Some(13),
        (USAGE_TEXCOORD, 8..=9) => Some(14 + index - 8),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
struct Param {
    ty: u32,
    num: u32,
    /// dest: write mask; src: swizzle
    mask: u32,
    /// dest: result modifier; src: source modifier
    modi: u32,
    /// dest (ps 1.x): shift
    shift: u32,
    rel: Option<(u32, u32, u32)>, // (type, num, component) of the address register
}

#[derive(Debug)]
struct Ins {
    op: u32,
    ctl: u32,
    dst: Option<Param>,
    src: Vec<Param>,
    /// dcl's usage token, def's values
    extra: [u32; 4],
    predicated: Option<Param>,
}

/// A parsed shader.
#[derive(Debug)]
pub struct Shader {
    pub pixel: bool,
    pub major: u32,
    pub minor: u32,
    ins: Vec<Ins>,
    /// vs inputs: (register, usage, index)
    pub inputs: Vec<(u32, u32, u32)>,
    /// vs 3 outputs / ps 3 inputs: (register, usage, index, mask)
    pub decls: Vec<(u32, u32, u32)>,
    /// samplers: (register, texture type: 2 2D, 3 cube, 4 volume)
    pub samplers: Vec<(u32, u32)>,
    /// highest float constant used + 1
    pub max_const: u32,
    pub uses_rel: bool,
    /// locations a vertex shader writes
    pub writes: u32,
    /// locations a pixel shader reads
    pub reads: u32,
}

fn parse_param(t: u32, dst: bool) -> Param {
    Param {
        ty: ((t >> 28) & 7) | ((t >> 8) & 0x18),
        num: t & 0x7ff,
        mask: if dst { (t >> 16) & 0xf } else { (t >> 16) & 0xff },
        modi: if dst { (t >> 20) & 0xf } else { (t >> 24) & 0xf },
        shift: (t >> 24) & 0xf,
        rel: None,
    }
}

// opcodes
const NOP: u32 = 0;
const MOV: u32 = 1;
const ADD: u32 = 2;
const SUB: u32 = 3;
const MAD: u32 = 4;
const MUL: u32 = 5;
const RCP: u32 = 6;
const RSQ: u32 = 7;
const DP3: u32 = 8;
const DP4: u32 = 9;
const MIN: u32 = 10;
const MAX: u32 = 11;
const SLT: u32 = 12;
const SGE: u32 = 13;
const EXP: u32 = 14;
const LOG: u32 = 15;
const LIT: u32 = 16;
const DST: u32 = 17;
const LRP: u32 = 18;
const FRC: u32 = 19;
const M4X4: u32 = 20;
const M4X3: u32 = 21;
const M3X4: u32 = 22;
const M3X3: u32 = 23;
const M3X2: u32 = 24;
const LOOPOP: u32 = 27;
const RET: u32 = 28;
const ENDLOOP: u32 = 29;
const DCL: u32 = 31;
const POW: u32 = 32;
const CRS: u32 = 33;
const SGN: u32 = 34;
const ABS: u32 = 35;
const NRM: u32 = 36;
const SINCOS: u32 = 37;
const REP: u32 = 38;
const ENDREP: u32 = 39;
const IF: u32 = 40;
const IFC: u32 = 41;
const ELSE: u32 = 42;
const ENDIF: u32 = 43;
const BREAK: u32 = 44;
const BREAKC: u32 = 45;
const MOVA: u32 = 46;
const DEFB: u32 = 47;
const DEFI: u32 = 48;
const TEXCOORD: u32 = 64;
const TEXKILL: u32 = 65;
const TEX: u32 = 66;
const EXPP: u32 = 78;
const LOGP: u32 = 79;
const CND: u32 = 80;
const DEF: u32 = 81;
const CMP: u32 = 88;
const DP2ADD: u32 = 90;
const DSX: u32 = 91;
const DSY: u32 = 92;
const TEXLDD: u32 = 93;
const SETP: u32 = 94;
const TEXLDL: u32 = 95;
const BREAKP: u32 = 96;
const PHASE: u32 = 0xfffd;
const COMMENT: u32 = 0xfffe;
const END: u32 = 0xffff;

/// Ops with no destination.
fn no_dst(op: u32) -> bool {
    matches!(op, NOP | LOOPOP | RET | ENDLOOP | REP | ENDREP | IF | IFC | ELSE | ENDIF | BREAK | BREAKC | BREAKP | PHASE | 25 | 26 | 30)
}

impl Shader {
    pub fn parse(code: &[u32]) -> Option<Shader> {
        let ver = *code.first()?;
        let pixel = match ver >> 16 {
            0xffff => true,
            0xfffe => false,
            _ => return None,
        };
        let major = (ver >> 8) & 0xff;
        let minor = ver & 0xff;
        let mut s = Shader {
            pixel,
            major,
            minor,
            ins: Vec::new(),
            inputs: Vec::new(),
            decls: Vec::new(),
            samplers: Vec::new(),
            max_const: 0,
            uses_rel: false,
            writes: 0,
            reads: 0,
        };
        let mut i = 1;
        while i < code.len() {
            let t = code[i];
            let op = t & 0xffff;
            if op == END {
                break;
            }
            if op == COMMENT {
                i += 1 + ((t >> 16) & 0x7fff) as usize;
                continue;
            }
            // the instruction's tokens
            let len = if major >= 2 {
                ((t >> 24) & 0xf) as usize
            } else if op == DEF {
                5
            } else {
                let mut n = 0;
                while i + 1 + n < code.len() && code[i + 1 + n] & 0x8000_0000 != 0 {
                    n += 1;
                }
                n
            };
            let toks = code.get(i + 1..i + 1 + len)?;
            i += 1 + len;
            let ctl = (t >> 16) & 0xff;
            let mut ins = Ins { op, ctl, dst: None, src: Vec::new(), extra: [0; 4], predicated: None };
            let mut k = 0;
            let mut next_param = |k: &mut usize, dst: bool| -> Option<Param> {
                let mut p = parse_param(*toks.get(*k)?, dst);
                let raw = toks[*k];
                *k += 1;
                if !dst && raw & 0x2000 != 0 {
                    if major >= 2 {
                        let a = parse_param(*toks.get(*k)?, false);
                        *k += 1;
                        p.rel = Some((a.ty, a.num, a.mask & 3));
                    } else {
                        p.rel = Some((ADDR, 0, 0));
                    }
                }
                Some(p)
            };
            match op {
                DCL => {
                    ins.extra[0] = *toks.first()?;
                    k = 1;
                    ins.dst = next_param(&mut k, true);
                }
                DEF | DEFI | DEFB => {
                    ins.dst = next_param(&mut k, true);
                    for j in 0..4 {
                        ins.extra[j] = toks.get(k + j).copied().unwrap_or(0);
                    }
                    k = toks.len();
                }
                _ => {
                    if !no_dst(op) {
                        ins.dst = next_param(&mut k, true);
                    }
                    if t & 0x1000_0000 != 0 && major >= 2 {
                        // predicated: the predicate comes before the sources
                        ins.predicated = next_param(&mut k, false);
                    }
                    while k < toks.len() {
                        let p = next_param(&mut k, false)?;
                        ins.src.push(p);
                    }
                }
            }
            s.ins.push(ins);
        }
        s.scan();
        Some(s)
    }

    fn scan(&mut self) {
        let mut max_const = 0;
        for ins in &self.ins {
            if ins.op == DCL {
                let d = ins.dst.unwrap();
                let usage = ins.extra[0] & 0x1f;
                let index = (ins.extra[0] >> 16) & 0xf;
                match d.ty {
                    INPUT if !self.pixel => self.inputs.push((d.num, usage, index)),
                    SAMPLER => self.samplers.push((d.num, (ins.extra[0] >> 27) & 0xf)),
                    INPUT | OUTPUT if self.major >= 3 => self.decls.push((d.num | (d.ty << 16), usage, index)),
                    _ => {}
                }
            }
            for p in ins.src.iter() {
                if matches!(p.ty, CONST | CONST2 | CONST3 | CONST4) {
                    let n = const_index(p) + if p.rel.is_some() { 0 } else { 1 };
                    max_const = max_const.max(n);
                    if p.rel.is_some() {
                        self.uses_rel = true;
                    }
                }
                if self.pixel && self.major < 2 && p.ty == SAMPLER {
                    // (not in 1.x)
                }
            }
        }
        // matrix ops read 2-4 consecutive constants
        for ins in &self.ins {
            let rows = match ins.op {
                M4X4 | M3X4 => 4,
                M4X3 | M3X3 => 3,
                M3X2 => 2,
                _ => 0,
            };
            if rows > 0 {
                if let Some(p) = ins.src.get(1) {
                    if matches!(p.ty, CONST | CONST2 | CONST3 | CONST4) {
                        max_const = max_const.max(const_index(p) + rows);
                    }
                }
            }
        }
        if self.pixel && self.major < 2 {
            // ps 1.x: texture stages are implicit samplers
            for ins in &self.ins {
                if ins.op == TEX || (ins.op >= 67 && ins.op <= 87 && ins.op != 78 && ins.op != 79 && ins.op != 80 && ins.op != 81) {
                    if let Some(d) = ins.dst {
                        if self.major == 1 && self.minor >= 4 {
                            if let Some(src) = ins.src.first() {
                                let _ = src;
                            }
                            self.samplers.push((d.num, 2));
                        } else {
                            self.samplers.push((d.num, 2));
                        }
                    }
                }
            }
            self.samplers.sort();
            self.samplers.dedup();
        }
        self.max_const = if self.uses_rel { if self.pixel { 224 } else { 256 } } else { max_const };
        // linkage
        if self.pixel {
            if self.major >= 3 {
                for &(reg, usage, index) in &self.decls {
                    if reg >> 16 == INPUT {
                        if let Some(l) = location(usage, index) {
                            self.reads |= 1 << l;
                        }
                    }
                }
            } else {
                self.reads = 0b11 | (0xff << 2);
            }
        } else if self.major >= 3 {
            for &(reg, usage, index) in &self.decls {
                if reg >> 16 == OUTPUT {
                    if let Some(l) = location(usage, index) {
                        self.writes |= 1 << l;
                    }
                }
            }
        } else {
            for ins in &self.ins {
                if let Some(d) = ins.dst {
                    match d.ty {
                        ATTROUT if d.num < 2 => self.writes |= 1 << d.num,
                        OUTPUT if d.num < 8 => self.writes |= 1 << (2 + d.num),
                        RASTOUT if d.num == 1 => self.writes |= 1 << 10,
                        _ => {}
                    }
                }
            }
        }
    }
}

fn const_index(p: &Param) -> u32 {
    match p.ty {
        CONST2 => p.num + 2048,
        CONST3 => p.num + 4096,
        CONST4 => p.num + 6144,
        _ => p.num,
    }
}

fn f(bits: u32) -> String {
    let v = f32::from_bits(bits);
    if v.is_finite() {
        format!("{:?}", v)
    } else if v.is_nan() {
        "0.0".into()
    } else if v > 0.0 {
        "3.4e38".into()
    } else {
        "-3.4e38".into()
    }
}

const SW: [char; 4] = ['x', 'y', 'z', 'w'];

/// The WGSL of one shader. `link`: for a pixel shader, the locations the vertex stage writes;
/// for a vertex shader, unused. `vs_in`: the vertex shader's input register -> (location, kind)
/// where kind 0 is vec4f, 1 vec4u, 2 vec4i (from the vertex declaration).
pub struct Gen<'a> {
    s: &'a Shader,
    out: String,
    /// the defs (constants defined in the shader)
    defs: Vec<(u32, String)>,
    defis: Vec<(u32, String)>,
    defbs: Vec<(u32, bool)>,
    indent: usize,
    tex_level: bool,
    loops: usize,
}

pub struct Options {
    /// pixel shader: the locations the vertex stage passes
    pub link: u32,
    /// vertex shader inputs: (register, location, kind)
    pub vs_inputs: Vec<(u32, u32, u32)>,
    /// pixel shader: samplers that read alpha as 1 are known at run time (u.x[0].x bits)
    pub alpha_test: Option<u32>,
    /// sampler texture types (by register) for samplers the shader didn't declare (ps 1.x)
    pub fog: bool,
}

pub const CONSTS_EXTRA: u32 = 4;

impl<'a> Gen<'a> {
    pub fn new(s: &'a Shader) -> Self {
        let mut defs = Vec::new();
        let mut defis = Vec::new();
        let mut defbs = Vec::new();
        for ins in &s.ins {
            let Some(d) = ins.dst else { continue };
            match ins.op {
                DEF => defs.push((
                    const_index(&d),
                    format!("vec4f({}, {}, {}, {})", f(ins.extra[0]), f(ins.extra[1]), f(ins.extra[2]), f(ins.extra[3])),
                )),
                DEFI => defis.push((
                    d.num,
                    format!(
                        "vec4i({}, {}, {}, {})",
                        ins.extra[0] as i32, ins.extra[1] as i32, ins.extra[2] as i32, ins.extra[3] as i32
                    ),
                )),
                DEFB => defbs.push((d.num, ins.extra[0] != 0)),
                _ => {}
            }
        }
        Gen { s, out: String::new(), defs, defis, defbs, indent: 1, tex_level: false, loops: 0 }
    }

    fn line(&mut self, l: &str) {
        for _ in 0..self.indent {
            self.out.push_str("  ");
        }
        self.out.push_str(l);
        self.out.push('\n');
    }

    fn reg(&self, p: &Param) -> String {
        let s = self.s;
        match p.ty {
            TEMP => format!("r{}", p.num),
            INPUT => format!("v{}", p.num),
            CONST | CONST2 | CONST3 | CONST4 => {
                let n = const_index(p);
                if let Some((aty, anum, comp)) = p.rel {
                    let a = if aty == LOOP { "aL".to_string() } else { format!("a{}.{}", anum, SW[comp as usize]) };
                    format!("u.c[clamp({} + {}, 0, {})]", n, a, s.max_const.max(1) - 1)
                } else if let Some((_, d)) = self.defs.iter().find(|(i, _)| *i == n) {
                    d.clone()
                } else {
                    format!("u.c[{}]", n)
                }
            }
            ADDR => {
                if s.pixel {
                    format!("t{}", p.num)
                } else {
                    format!("vec4f(a{})", p.num)
                }
            }
            RASTOUT => ["opos", "ofog", "opts"][p.num.min(2) as usize].to_string(),
            ATTROUT => format!("od{}", p.num),
            OUTPUT => format!("o{}", p.num),
            CONSTINT => {
                if let Some((_, d)) = self.defis.iter().find(|(i, _)| *i == p.num) {
                    format!("vec4f({})", d)
                } else {
                    format!("vec4f(u.i[{}])", p.num)
                }
            }
            COLOROUT => format!("oc{}", p.num),
            DEPTHOUT => "odepth".into(),
            CONSTBOOL => self.boolean(p.num),
            LOOP => "vec4f(f32(aL))".into(),
            MISCTYPE => if p.num == 0 { "vpos".into() } else { "vface".into() },
            PREDICATE => "vec4f(p0)".into(),
            _ => "vec4f(0.0)".into(),
        }
    }

    fn boolean(&self, n: u32) -> String {
        if let Some((_, b)) = self.defbs.iter().find(|(i, _)| *i == n) {
            if *b { "true".into() } else { "false".into() }
        } else {
            format!("((u.x[1].x & {}u) != 0u)", 1u32 << n)
        }
    }

    /// A source with its swizzle and modifier, as vec4f.
    fn src(&self, p: &Param) -> String {
        let r = self.reg(p);
        let sw = p.mask;
        let r = if sw == 0xe4 {
            r
        } else {
            let c: String = (0..4).map(|i| SW[((sw >> (i * 2)) & 3) as usize]).collect();
            format!("{}.{}", paren(&r), c)
        };
        match p.modi {
            0 => r,
            1 => format!("(-{})", paren(&r)),
            2 => format!("({} - 0.5)", r),
            3 => format!("(0.5 - {})", r),
            4 => format!("(2.0 * {} - 1.0)", r),
            5 => format!("(1.0 - 2.0 * {})", r),
            6 => format!("(1.0 - {})", r),
            7 => format!("(2.0 * {})", r),
            8 => format!("(-2.0 * {})", r),
            9 => format!("({0} / {0}.z)", paren(&r)),
            10 => format!("({0} / {0}.w)", paren(&r)),
            11 => format!("abs({})", r),
            12 => format!("(-abs({}))", r),
            13 => format!("(1.0 - {})", r),
            _ => r,
        }
    }

    /// The first component of a source (scalar ops: the swizzle's x picks it).
    fn scalar(&self, p: &Param) -> String {
        format!("{}.x", paren(&self.src(p)))
    }

    /// dst = value (vec4f), with the write mask and modifiers.
    fn assign(&mut self, d: &Param, value: String, pred: Option<&Param>) {
        let mut v = value;
        if self.s.major < 2 && self.s.pixel {
            v = match d.shift {
                1 => format!("({} * 2.0)", v),
                2 => format!("({} * 4.0)", v),
                3 => format!("({} * 8.0)", v),
                0xf => format!("({} * 0.5)", v),
                0xe => format!("({} * 0.25)", v),
                0xd => format!("({} * 0.125)", v),
                _ => v,
            };
        }
        if d.modi & 1 != 0 {
            v = format!("clamp({}, vec4f(0.0), vec4f(1.0))", v);
        }
        let name = match d.ty {
            ADDR if !self.s.pixel => {
                // mova / mov to a0: rounded (vs 2+) or floored (vs 1.1)
                let v2 = if self.s.major >= 2 { format!("vec4i(round({}))", v) } else { format!("vec4i(floor({}))", v) };
                self.masked(&format!("a{}", d.num), d.mask, v2, pred);
                return;
            }
            PREDICATE => {
                // (setp writes bools itself)
                return;
            }
            _ => self.reg(d),
        };
        self.masked(&name, d.mask, v, pred);
    }

    fn masked(&mut self, name: &str, mask: u32, v: String, pred: Option<&Param>) {
        let mask = if mask == 0 { 0xf } else { mask };
        let cond = pred.map(|p| {
            let sw = p.mask;
            let neg = p.modi == 13;
            (sw, neg)
        });
        if mask == 0xf && cond.is_none() {
            self.line(&format!("{} = {};", name, v));
            return;
        }
        self.line(&format!("{{ let t = {};", v));
        for i in 0..4 {
            if mask & (1 << i) != 0 {
                let set = format!("{}.{} = t.{};", name, SW[i], SW[i]);
                match cond {
                    Some((sw, neg)) => {
                        let c = SW[((sw >> (i * 2)) & 3) as usize];
                        self.line(&format!("  if ({}p0.{}) {{ {} }}", if neg { "!" } else { "" }, c, set));
                    }
                    None => self.line(&format!("  {}", set)),
                }
            }
        }
        self.line("}");
    }

    fn sample(&self, sampler: u32, coord: &str, kind: u32, extra: &str) -> String {
        let ty = self.s.samplers.iter().find(|s| s.0 == sampler).map_or(2, |s| s.1);
        let c = match ty {
            3 | 4 => format!("{}.xyz", paren(coord)),
            _ => format!("{}.xy", paren(coord)),
        };
        if ty == 3 || ty == 4 {
            // (cube/volume textures aren't bound: black)
            return "vec4f(0.0, 0.0, 0.0, 1.0)".into();
        }
        let call = match (kind, self.tex_level) {
            (1, _) => format!("textureSampleLevel(tex{0}, smp{0}, {1}, {2})", sampler, c, extra),
            (2, false) => format!("textureSampleBias(tex{0}, smp{0}, {1}, {2})", sampler, c, extra),
            (3, _) => format!("textureSampleGrad(tex{0}, smp{0}, {1}, {2})", sampler, c, extra),
            (_, true) => format!("textureSampleLevel(tex{0}, smp{0}, {1}, 0.0)", sampler, c),
            _ => format!("textureSample(tex{0}, smp{0}, {1})", sampler, c),
        };
        format!("fixa({}, {}u)", call, sampler)
    }

    fn body(&mut self) {
        let s = self.s;
        let ins_list: Vec<&Ins> = s.ins.iter().collect();
        for ins in ins_list {
            let pred = ins.predicated;
            let pred = pred.as_ref();
            let a = |g: &Self, i: usize| ins.src.get(i).map(|p| g.src(p)).unwrap_or_else(|| "vec4f(0.0)".into());
            let sc = |g: &Self, i: usize| ins.src.get(i).map(|p| g.scalar(p)).unwrap_or_else(|| "0.0".into());
            let Some(d) = ins.dst else {
                match ins.op {
                    IF => {
                        let c = ins.src.first().map(|p| self.cond_bool(p)).unwrap_or("false".into());
                        self.line(&format!("if ({}) {{", c));
                        self.indent += 1;
                        self.tex_level_push();
                    }
                    IFC => {
                        let c = format!("{} {} {}", sc(self, 0), cmp_op(ins.ctl & 7), sc(self, 1));
                        self.line(&format!("if ({}) {{", c));
                        self.indent += 1;
                        self.tex_level_push();
                    }
                    ELSE => {
                        self.indent -= 1;
                        self.line("} else {");
                        self.indent += 1;
                    }
                    ENDIF | ENDLOOP | ENDREP => {
                        self.indent -= 1;
                        self.line("}");
                        if ins.op != ENDIF {
                            self.loops -= 1;
                        }
                        self.tex_level_pop();
                    }
                    REP => {
                        let n = ins.src.first().map(|p| self.reg(p)).unwrap_or("vec4f(0.0)".into());
                        let id = self.loops;
                        self.line(&format!("for (var rep{0} = 0; rep{0} < min(i32({1}.x), 255); rep{0}++) {{", id, n));
                        self.loops += 1;
                        self.indent += 1;
                        self.tex_level_push();
                    }
                    LOOPOP => {
                        let n = ins.src.get(1).map(|p| self.reg(p)).unwrap_or("vec4f(0.0)".into());
                        let id = self.loops;
                        self.line(&format!("aL = i32({}.y);", n));
                        self.line(&format!(
                            "for (var rep{0} = 0; rep{0} < min(i32({1}.x), 255); rep{0}++) {{",
                            id, n
                        ));
                        self.loops += 1;
                        self.indent += 1;
                        self.line(&format!("defer_aL{} = i32({}.z);", id, n));
                        self.tex_level_push();
                    }
                    BREAK => self.line("break;"),
                    BREAKC => {
                        let c = format!("{} {} {}", sc(self, 0), cmp_op(ins.ctl & 7), sc(self, 1));
                        self.line(&format!("if ({}) {{ break; }}", c));
                    }
                    BREAKP => {
                        let c = ins.src.first().map(|p| self.cond_bool(p)).unwrap_or("false".into());
                        self.line(&format!("if ({}) {{ break; }}", c));
                    }
                    RET | NOP | PHASE | 30 => {}
                    _ => self.line(&format!("// unsupported op {}", ins.op)),
                }
                continue;
            };
            let v: Option<String> = match ins.op {
                DCL | DEF | DEFI | DEFB => None,
                MOV => Some(a(self, 0)),
                MOVA => Some(a(self, 0)),
                ADD => Some(format!("({} + {})", a(self, 0), a(self, 1))),
                SUB => Some(format!("({} - {})", a(self, 0), a(self, 1))),
                MAD => Some(format!("({} * {} + {})", a(self, 0), a(self, 1), a(self, 2))),
                MUL => Some(format!("({} * {})", a(self, 0), a(self, 1))),
                RCP => Some(format!("vec4f(rcp({}))", sc(self, 0))),
                RSQ => Some(format!("vec4f(rsq({}))", sc(self, 0))),
                DP3 => Some(format!("vec4f(dot({}.xyz, {}.xyz))", paren(&a(self, 0)), paren(&a(self, 1)))),
                DP4 => Some(format!("vec4f(dot({}, {}))", a(self, 0), a(self, 1))),
                DP2ADD => Some(format!(
                    "vec4f(dot({}.xy, {}.xy) + {})",
                    paren(&a(self, 0)),
                    paren(&a(self, 1)),
                    sc(self, 2)
                )),
                MIN => Some(format!("min({}, {})", a(self, 0), a(self, 1))),
                MAX => Some(format!("max({}, {})", a(self, 0), a(self, 1))),
                SLT => Some(format!("select(vec4f(0.0), vec4f(1.0), {} < {})", a(self, 0), a(self, 1))),
                SGE => Some(format!("select(vec4f(0.0), vec4f(1.0), {} >= {})", a(self, 0), a(self, 1))),
                EXP | EXPP => Some(format!("vec4f(exp2({}))", sc(self, 0))),
                LOG | LOGP => Some(format!("vec4f(lg2({}))", sc(self, 0))),
                POW => Some(format!("vec4f(pow(abs({}), {}))", sc(self, 0), sc(self, 1))),
                LIT => Some(format!("lit({})", a(self, 0))),
                DST => Some(format!(
                    "vec4f(1.0, {0}.y * {1}.y, {0}.z, {1}.w)",
                    paren(&a(self, 0)),
                    paren(&a(self, 1))
                )),
                LRP => Some(format!("mix({2}, {1}, {0})", a(self, 0), a(self, 1), a(self, 2))),
                FRC => Some(format!("fract({})", a(self, 0))),
                ABS => Some(format!("abs({})", a(self, 0))),
                SGN => Some(format!("sign({})", a(self, 0))),
                NRM => Some(format!("nrm({})", a(self, 0))),
                CRS => Some(format!(
                    "vec4f(cross({}.xyz, {}.xyz), 0.0)",
                    paren(&a(self, 0)),
                    paren(&a(self, 1))
                )),
                SINCOS => Some(format!("vec4f(cos({0}), sin({0}), 0.0, 0.0)", sc(self, 0))),
                CMP => Some(format!("select({2}, {1}, {0} >= vec4f(0.0))", a(self, 0), a(self, 1), a(self, 2))),
                CND => {
                    if s.major == 1 && s.minor < 4 {
                        Some(format!("select({2}, {1}, vec4f({0}.w) > vec4f(0.5))", paren(&a(self, 0)), a(self, 1), a(self, 2)))
                    } else {
                        Some(format!("select({2}, {1}, {0} > vec4f(0.5))", a(self, 0), a(self, 1), a(self, 2)))
                    }
                }
                DSX => Some(format!("dpdx({})", a(self, 0))),
                DSY => Some(format!("dpdy({})", a(self, 0))),
                M4X4 | M4X3 | M3X4 | M3X3 | M3X2 => {
                    let (cols, rows) = match ins.op {
                        M4X4 => (4, 4),
                        M4X3 => (4, 3),
                        M3X4 => (3, 4),
                        M3X3 => (3, 3),
                        _ => (3, 2),
                    };
                    let v0 = a(self, 0);
                    let m = ins.src.get(1).copied();
                    let mut parts = Vec::new();
                    for r in 0..4 {
                        if r < rows {
                            let row = m
                                .map(|mut p| {
                                    p.num += r as u32;
                                    self.src(&p)
                                })
                                .unwrap_or("vec4f(0.0)".into());
                            if cols == 4 {
                                parts.push(format!("dot({}, {})", v0, row));
                            } else {
                                parts.push(format!("dot({}.xyz, {}.xyz)", paren(&v0), paren(&row)));
                            }
                        } else {
                            parts.push("0.0".into());
                        }
                    }
                    Some(format!("vec4f({})", parts.join(", ")))
                }
                SETP => {
                    let op = cmp_op(ins.ctl & 7);
                    let (x, y) = (a(self, 0), a(self, 1));
                    let mask = if d.mask == 0 { 0xf } else { d.mask };
                    self.line(&format!("{{ let pa = {}; let pb = {};", x, y));
                    for i in 0..4 {
                        if mask & (1 << i) != 0 {
                            self.line(&format!("  p0.{0} = pa.{0} {1} pb.{0};", SW[i], op));
                        }
                    }
                    self.line("}");
                    None
                }
                TEXKILL => {
                    let r = self.reg(&d);
                    let mask = if s.major < 2 { 0x7 } else if d.mask == 0 { 0xf } else { d.mask };
                    let c: Vec<String> =
                        (0..4).filter(|i| mask & (1 << i) != 0).map(|i| format!("{}.{} < 0.0", r, SW[i])).collect();
                    self.line(&format!("if ({}) {{ discard; }}", c.join(" || ")));
                    None
                }
                TEX => {
                    if s.major >= 2 {
                        let coord = a(self, 0);
                        let smp = ins.src.get(1).map_or(0, |p| p.num);
                        let project = ins.ctl & 1 != 0;
                        let bias = ins.ctl & 2 != 0;
                        if project {
                            Some(self.sample(smp, &format!("({0} / {0}.w)", paren(&coord)), 0, ""))
                        } else if bias {
                            Some(self.sample(smp, &coord, 2, &format!("{}.w", paren(&coord))))
                        } else {
                            Some(self.sample(smp, &coord, 0, ""))
                        }
                    } else if s.minor >= 4 {
                        // texld rN, tM / rM
                        let coord = ins.src.first().map(|p| self.reg(p)).unwrap_or("vec4f(0.0)".into());
                        Some(self.sample(d.num, &coord, 0, ""))
                    } else {
                        // tex tN: samples stage N at texcoord N
                        Some(self.sample(d.num, &format!("tc{}", d.num), 0, ""))
                    }
                }
                TEXCOORD => {
                    if s.minor >= 4 {
                        let src = ins.src.first().map(|p| self.reg(p)).unwrap_or("vec4f(0.0)".into());
                        Some(src)
                    } else {
                        Some(format!("clamp(tc{}, vec4f(0.0), vec4f(1.0))", d.num))
                    }
                }
                TEXLDL => {
                    let coord = a(self, 0);
                    let smp = ins.src.get(1).map_or(0, |p| p.num);
                    Some(self.sample(smp, &coord, 1, &format!("{}.w", paren(&coord))))
                }
                TEXLDD => {
                    let coord = a(self, 0);
                    let smp = ins.src.get(1).map_or(0, |p| p.num);
                    let ty = s.samplers.iter().find(|x| x.0 == smp).map_or(2, |x| x.1);
                    let sw = if ty == 2 { "xy" } else { "xyz" };
                    Some(self.sample(
                        smp,
                        &coord,
                        3,
                        &format!("{}.{}, {}.{}", paren(&a(self, 2)), sw, paren(&a(self, 3)), sw),
                    ))
                }
                _ => {
                    self.line(&format!("// unsupported op {}", ins.op));
                    None
                }
            };
            if let Some(v) = v {
                self.assign(&d, v, pred);
            }
        }
    }

    fn tex_level_push(&mut self) {
        // samples inside control flow can't take derivatives (WGSL uniformity): level 0
        self.tex_level = true;
    }

    fn tex_level_pop(&mut self) {
        if self.indent <= 1 {
            self.tex_level = false;
        }
    }

    fn cond_bool(&self, p: &Param) -> String {
        match p.ty {
            CONSTBOOL => {
                let b = self.boolean(p.num);
                if p.modi == 13 { format!("!{}", b) } else { b }
            }
            PREDICATE => {
                let c = SW[(p.mask & 3) as usize];
                if p.modi == 13 { format!("!p0.{}", c) } else { format!("p0.{}", c) }
            }
            _ => "false".into(),
        }
    }

    fn temps(&self) -> (u32, u32, u32, u32) {
        let mut r = 0;
        let mut a = 0;
        let mut o = 0;
        let mut t = 0;
        for ins in &self.s.ins {
            for p in ins.dst.iter().chain(ins.src.iter()) {
                match p.ty {
                    TEMP => r = r.max(p.num + 1),
                    ADDR => {
                        if self.s.pixel {
                            t = t.max(p.num + 1)
                        } else {
                            a = a.max(p.num + 1)
                        }
                    }
                    OUTPUT => o = o.max(p.num + 1),
                    _ => {}
                }
                if let Some((ty, num, _)) = p.rel {
                    if ty == ADDR {
                        a = a.max(num + 1);
                    }
                }
            }
        }
        (r.max(1), a.max(1), o, t)
    }

    /// Vertex shader WGSL. Outputs go to `location(...)`; `opos` is fixed up for D3D's pixel
    /// centers with u.x[0] (= (1/vp_w, -1/vp_h) * 2 / 2).
    pub fn vertex(mut self, opts: &Options) -> String {
        let s = self.s;
        let (nr, na, no, _) = self.temps();
        let mut h = String::new();
        h.push_str(&uniforms(s.max_const, 0));
        h.push_str(HELPERS);
        // inputs
        h.push_str("struct In {\n");
        for &(reg, loc, kind) in &opts.vs_inputs {
            let ty = ["vec4f", "vec4u", "vec4i", "vec4f"][kind as usize];
            let _ = writeln!(h, "  @location({}) v{}: {},", loc, reg, ty);
        }
        if opts.vs_inputs.is_empty() {
            h.push_str("  @builtin(vertex_index) vid: u32,\n");
        }
        h.push_str("}\n");
        h.push_str("struct Out {\n  @builtin(position) pos: vec4f,\n");
        let writes = s.writes;
        for l in 0..16 {
            if writes & (1 << l) != 0 {
                let _ = writeln!(h, "  @location({}) l{}: vec4f,", l, l);
            }
        }
        h.push_str("}\n");
        h.push_str("@vertex fn main(i: In) -> Out {\n");
        let mut b = String::new();
        for &(reg, _, kind) in &opts.vs_inputs {
            match kind {
                0 => {
                    let _ = writeln!(b, "  var v{0} = i.v{0};", reg);
                }
                3 => {
                    // D3DCOLOR: B, G, R, A bytes
                    let _ = writeln!(b, "  var v{0} = i.v{0}.zyxw;", reg);
                }
                _ => {
                    let _ = writeln!(b, "  var v{0} = vec4f(i.v{0});", reg);
                }
            }
        }
        // inputs the declaration doesn't feed
        for &(reg, _, _) in &s.inputs {
            if !opts.vs_inputs.iter().any(|x| x.0 == reg) {
                let _ = writeln!(b, "  var v{} = vec4f(0.0, 0.0, 0.0, 1.0);", reg);
            }
        }
        for r in 0..nr {
            let _ = writeln!(b, "  var r{}: vec4f;", r);
        }
        for r in 0..na {
            let _ = writeln!(b, "  var a{}: vec4i;", r);
        }
        b.push_str("  var aL: i32;\n  var p0: vec4<bool>;\n");
        b.push_str("  var opos = vec4f(0.0, 0.0, 0.0, 1.0);\n  var ofog = vec4f(0.0);\n  var opts = vec4f(0.0);\n");
        b.push_str("  var od0 = vec4f(1.0);\n  var od1 = vec4f(0.0);\n");
        for o in 0..no.max(8) {
            let _ = writeln!(b, "  var o{}: vec4f;", o);
        }
        self.out = b;
        self.body();
        let mut tail = String::from("  var out: Out;\n");
        // position
        let pos = if s.major >= 3 {
            s.decls
                .iter()
                .find(|d| d.0 >> 16 == OUTPUT && (d.1 == USAGE_POSITION || d.1 == USAGE_POSITIONT))
                .map(|d| format!("o{}", d.0 & 0xffff))
                .unwrap_or("opos".into())
        } else {
            "opos".into()
        };
        let _ = writeln!(tail, "  out.pos = fixpos({});", pos);
        for l in 0..16u32 {
            if writes & (1 << l) == 0 {
                continue;
            }
            let src = if s.major >= 3 {
                s.decls
                    .iter()
                    .find(|d| d.0 >> 16 == OUTPUT && location(d.1, d.2) == Some(l))
                    .map(|d| format!("o{}", d.0 & 0xffff))
                    .unwrap_or("vec4f(0.0)".into())
            } else {
                match l {
                    0 | 1 => format!("clamp(od{}, vec4f(0.0), vec4f(1.0))", l),
                    2..=9 => format!("o{}", l - 2),
                    10 => "ofog".into(),
                    _ => "vec4f(0.0)".into(),
                }
            };
            let _ = writeln!(tail, "  out.l{} = {};", l, src);
        }
        tail.push_str("  return out;\n}\n");
        // loop counters for aL (set at loop start: start + step * iteration); kept simple
        let body = self.out.replace("defer_aL", "//");
        format!("{}{}{}{}", h, "", body, tail)
    }

    /// Pixel shader WGSL; `opts.link`: the locations the vertex stage writes.
    pub fn pixel(mut self, opts: &Options) -> String {
        let s = self.s;
        let (nr, _, _, nt) = self.temps();
        let mut h = String::new();
        h.push_str(&uniforms(s.max_const, 1));
        h.push_str(HELPERS);
        h.push_str(PS_HELPERS);
        for &(reg, ty) in &s.samplers {
            if ty == 2 || ty == 0 {
                let _ = writeln!(h, "@group(1) @binding({}) var tex{}: texture_2d<f32>;", reg * 2, reg);
                let _ = writeln!(h, "@group(1) @binding({}) var smp{}: sampler;", reg * 2 + 1, reg);
            }
        }
        h.push_str("struct In {\n  @builtin(position) fragpos: vec4f,\n  @builtin(front_facing) ff: bool,\n");
        let used = s.reads & opts.link;
        for l in 0..16 {
            if used & (1 << l) != 0 {
                let _ = writeln!(h, "  @location({}) l{}: vec4f,", l, l);
            }
        }
        h.push_str("}\n");
        h.push_str("struct Out {\n  @location(0) c0: vec4f,\n");
        let writes_depth = s.ins.iter().any(|i| i.dst.is_some_and(|d| d.ty == DEPTHOUT));
        if writes_depth {
            h.push_str("  @builtin(frag_depth) depth: f32,\n");
        }
        h.push_str("}\n@fragment fn main(i: In) -> Out {\n");
        let mut b = String::new();
        let inp = |l: u32, def: &str| {
            if used & (1 << l) != 0 { format!("i.l{}", l) } else { def.to_string() }
        };
        if s.major >= 3 {
            for &(reg, usage, index) in &s.decls {
                if reg >> 16 != INPUT {
                    continue;
                }
                let def = if usage == USAGE_COLOR { "vec4f(1.0)" } else { "vec4f(0.0)" };
                let v = location(usage, index).map_or(def.to_string(), |l| inp(l, def));
                let _ = writeln!(b, "  var v{} = {};", reg & 0xffff, v);
            }
            b.push_str("  let vpos = vec4f(i.fragpos.xy - 0.5, 0.0, 0.0);\n");
            b.push_str("  let vface = vec4f(select(-1.0, 1.0, i.ff));\n");
        } else {
            let _ = writeln!(b, "  var v0 = {};", inp(0, "vec4f(1.0)"));
            let _ = writeln!(b, "  var v1 = {};", inp(1, "vec4f(0.0)"));
            for t in 0..8 {
                let _ = writeln!(b, "  var tc{} = {};", t, inp(2 + t, "vec4f(0.0)"));
            }
            for t in 0..nt.max(if s.major >= 2 { 8 } else { 4 }) {
                if s.major >= 2 {
                    let _ = writeln!(b, "  var t{0} = tc{0};", t);
                } else {
                    let _ = writeln!(b, "  var t{}: vec4f;", t);
                }
            }
        }
        for r in 0..nr {
            let _ = writeln!(b, "  var r{}: vec4f;", r);
        }
        b.push_str("  var aL: i32;\n  var p0: vec4<bool>;\n");
        b.push_str("  var oc0 = vec4f(0.0);\n  var oc1 = vec4f(0.0);\n  var oc2 = vec4f(0.0);\n  var oc3 = vec4f(0.0);\n");
        b.push_str("  var odepth = vec4f(i.fragpos.z);\n");
        self.out = b;
        self.body();
        let mut tail = String::from("  var out: Out;\n");
        let result = if s.major >= 2 { "oc0" } else { "r0" };
        let _ = writeln!(tail, "  out.c0 = {};", result);
        if opts.fog {
            tail.push_str("  out.c0 = vec4f(mix(u.x[2].xyz, out.c0.xyz, clamp(fogf, 0.0, 1.0)), out.c0.w);\n");
        }
        if let Some(func) = opts.alpha_test {
            let _ = writeln!(tail, "  if (!alpha_ok(out.c0.w, {}u)) {{ discard; }}", func);
        }
        if writes_depth {
            tail.push_str("  out.depth = odepth.x;\n");
        }
        tail.push_str("  return out;\n}\n");
        let fog_decl = if opts.fog {
            format!("  let fogf = {}.x;\n", inp(10, "vec4f(1.0)"))
        } else {
            String::new()
        };
        let body = self.out.replace("defer_aL", "//");
        format!("{}{}{}{}", h, fog_decl, body, tail)
    }
}

fn paren(s: &str) -> String {
    let simple = s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '[' || c == ']');
    if simple { s.to_string() } else { format!("({})", s) }
}

fn cmp_op(c: u32) -> &'static str {
    match c {
        1 => ">",
        2 => "==",
        3 => ">=",
        4 => "<",
        5 => "!=",
        6 => "<=",
        _ => "==",
    }
}

/// The uniform block every stage has: x[0..4] are the device's (see CONSTS_EXTRA), then the
/// shader's float constants.
pub fn uniforms(n: u32, binding: u32) -> String {
    format!(
        "struct U {{ x: array<vec4u, {}>, i: array<vec4i, 16>, c: array<vec4f, {}> }}\n@group(0) @binding({}) var<uniform> u: U;\n",
        CONSTS_EXTRA,
        n.max(1),
        binding
    )
}

/// Bytes of the uniform block of a stage with `n` float constants.
pub fn uniform_size(n: u32) -> u32 {
    (CONSTS_EXTRA + 16 + n.max(1)) * 16
}

const HELPERS: &str = "
fn rcp(x: f32) -> f32 { if (x == 0.0) { return 3.4e38; } return 1.0 / x; }
fn rsq(x: f32) -> f32 { let a = abs(x); if (a == 0.0) { return 3.4e38; } return inverseSqrt(a); }
fn lg2(x: f32) -> f32 { let a = abs(x); if (a == 0.0) { return -3.4e38; } return log2(a); }
fn nrm(v: vec4f) -> vec4f { let l = dot(v.xyz, v.xyz); if (l == 0.0) { return vec4f(0.0); } return v * inverseSqrt(l); }
fn lit(v: vec4f) -> vec4f {
  var r = vec4f(1.0, 0.0, 0.0, 1.0);
  if (v.x > 0.0) { r.y = v.x; if (v.y > 0.0) { r.z = pow(v.y, clamp(v.w, -127.9961, 127.9961)); } }
  return r;
}
// D3D9 vertex positions to WebGPU's: pixel centers are half a pixel apart (x[0].xy: 1/vp size)
fn fixpos(p: vec4f) -> vec4f {
  return vec4f(p.x + bitcast<f32>(u.x[0].x) * p.w, p.y - bitcast<f32>(u.x[0].y) * p.w, p.z, p.w);
}
";

const PS_HELPERS: &str = "
// samplers of formats without alpha read 1 (x[0].z: a bit per sampler)
fn fixa(c: vec4f, s: u32) -> vec4f { if ((u.x[0].z & (1u << s)) != 0u) { return vec4f(c.xyz, 1.0); } return c; }
// alpha test against x[0].w (the reference, 0..1 as f32 bits)
fn alpha_ok(a: f32, func: u32) -> bool {
  let r = bitcast<f32>(u.x[0].w);
  let q = round(a * 255.0) / 255.0;
  switch (func) {
    case 1u: { return false; }
    case 2u: { return q < r; }
    case 3u: { return q == r; }
    case 4u: { return q <= r; }
    case 5u: { return q > r; }
    case 6u: { return q != r; }
    case 7u: { return q >= r; }
    default: { return true; }
  }
}
";
