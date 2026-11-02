//! Shader model 4/5 (tpf.rs) to WGSL, directly.
//!
//! D3D registers are typeless, so every register is a `vec4<u32>` of bits and each instruction
//! bitcasts its operands to the type it works in (f32, i32, u32) and its result back. The
//! shader's code goes in `shader_main`, on module-scope private registers; the entry point
//! `main` copies the stage inputs into the input registers and builds its result from the
//! output registers.
//!
//! Bindings (group = stage, 0 vertex, 1 pixel): constant buffer b<n> is binding n, shader
//! resource t<n> 16 + n, sampler s<n> 160 + n. Buffer resources are one-row 2D textures
//! (mod.rs create_buffer_srv). A vertex shader linked to a pixel shader writes the pixel
//! shader's input registers (matched by semantic), interpolated as the pixel shader declares.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use super::dxbc::{Container, SigElement};
use super::shader::{Binding, Wgsl};
use super::tpf::{self, Index, Inst, Operand, Program};

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    F,
    I,
    U,
}

impl K {
    fn s(self) -> &'static str {
        match self {
            K::F => "f32",
            K::I => "i32",
            K::U => "u32",
        }
    }
    /// a signature's component type
    fn of_sig(t: u32) -> K {
        match t {
            1 => K::U,
            2 => K::I,
            _ => K::F,
        }
    }
}

fn vty(k: K, n: usize) -> String {
    if n == 1 { k.s().into() } else { format!("vec{n}<{}>", k.s()) }
}

const XYZW: [char; 4] = ['x', 'y', 'z', 'w'];

fn bits(mask: u32) -> Vec<u8> {
    (0..4).filter(|c| mask & (1 << c) != 0).collect()
}

fn letters(c: &[u8]) -> String {
    c.iter().map(|&c| XYZW[c as usize]).collect()
}

/// A pixel shader input register passed between the stages.
#[derive(Clone, Debug)]
pub struct Varying {
    pub reg: u32,
    k: K,
    /// the @interpolate attribute ("" for the default)
    interp: &'static str,
    elems: Vec<SigElement>,
}

struct Res {
    dim: u32,
    ret: K,
    depth: bool,
}

struct InDecl {
    mask: u32,
    sysval: u32,
    interp: u32,
}

struct W {
    pixel: bool,
    isgn: Vec<SigElement>,
    osgn: Vec<SigElement>,
    temps: u32,
    xs: BTreeMap<u32, u32>,
    cbs: BTreeMap<u32, u32>,
    icb: Vec<u32>,
    res: BTreeMap<u32, Res>,
    samplers: BTreeMap<u32, bool>,
    inputs: BTreeMap<u32, InDecl>,
    outputs: BTreeMap<u32, (u32, u32)>,
    depth_out: bool,
    mask_out: bool,
    used_cb: std::collections::BTreeSet<u32>,
    used_t: std::collections::BTreeSet<u32>,
    used_s: std::collections::BTreeSet<u32>,
    nonfinite: bool,
    body: String,
    depth: usize,
    tmp: u32,
    /// open switch statements: (labels waiting for a body, a case is open, a default was given)
    switches: Vec<(Vec<String>, bool, bool)>,
    /// what each open block is: b'i' if, b'l' loop, b's' switch
    blocks: Vec<u8>,
    last_op: u32,
}

type R<T> = Result<T, String>;

/// The declarations of a program, read into `w`.
fn declarations(w: &mut W, p: &Program) -> R<()> {
    for i in &p.insts {
        match i.op {
            tpf::OP_DCL_TEMPS => w.temps = i.extra.first().copied().unwrap_or(0),
            tpf::OP_DCL_INDEXABLE_TEMP => {
                if i.extra.len() >= 2 {
                    w.xs.insert(i.extra[0], i.extra[1]);
                }
            }
            tpf::OP_DCL_CONSTANT_BUFFER => {
                let o = &i.ops[0];
                let size = match o.idx.get(1) {
                    Some(Index::Imm(n)) => *n,
                    _ => 4096,
                };
                w.cbs.insert(o.reg(), size.clamp(1, 4096));
            }
            tpf::OP_CUSTOMDATA => {
                if i.ctl == 3 {
                    w.icb = i.data.clone();
                }
            }
            tpf::OP_DCL_RESOURCE => {
                let ret = match i.extra.first().map(|r| r & 0xf) {
                    Some(3) => K::I,
                    Some(4) => K::U,
                    _ => K::F,
                };
                w.res.insert(i.ops[0].reg(), Res { dim: i.ctl & 0xf, ret, depth: false });
            }
            tpf::OP_DCL_SAMPLER => {
                w.samplers.insert(i.ops[0].reg(), i.ctl & 0xf == 1);
            }
            tpf::OP_DCL_INPUT..=tpf::OP_DCL_INPUT_PS_SIV => {
                let o = &i.ops[0];
                if o.ty != tpf::INPUT {
                    return Err(format!("input register type {}", o.ty));
                }
                let sysval = if matches!(i.op, tpf::OP_DCL_INPUT_SGV | tpf::OP_DCL_INPUT_SIV | tpf::OP_DCL_INPUT_PS_SGV | tpf::OP_DCL_INPUT_PS_SIV) {
                    i.extra.first().copied().unwrap_or(0)
                } else {
                    0
                };
                let interp = if (tpf::OP_DCL_INPUT_PS..=tpf::OP_DCL_INPUT_PS_SIV).contains(&i.op) { i.ctl & 0xf } else { 0 };
                let e = w.inputs.entry(o.reg()).or_insert(InDecl { mask: 0, sysval, interp });
                e.mask |= o.mask() as u32;
                if sysval != 0 {
                    e.sysval = sysval;
                }
            }
            tpf::OP_DCL_OUTPUT..=tpf::OP_DCL_OUTPUT_SIV => {
                let o = &i.ops[0];
                match o.ty {
                    tpf::OUTPUT => {
                        let sysval = if i.op != tpf::OP_DCL_OUTPUT { i.extra.first().copied().unwrap_or(0) } else { 0 };
                        let e = w.outputs.entry(o.reg()).or_insert((0, sysval));
                        e.0 |= o.mask() as u32;
                        if sysval != 0 {
                            e.1 = sysval;
                        }
                    }
                    tpf::OUTPUT_DEPTH | tpf::OUTPUT_DEPTH_GE | tpf::OUTPUT_DEPTH_LE => w.depth_out = true,
                    tpf::OUTPUT_COVERAGE_MASK => w.mask_out = true,
                    t => return Err(format!("output register type {t}")),
                }
            }
            tpf::OP_DCL_INDEX_RANGE => return Err("indexed input/output ranges".into()),
            tpf::OP_DCL_RESOURCE_RAW | tpf::OP_DCL_RESOURCE_STRUCTURED | 0x9c..=0xa0 => {
                return Err("raw/structured buffers and UAVs".into())
            }
            _ => {}
        }
    }
    // compare sampling needs depth textures
    for i in &p.insts {
        if matches!(i.op, 0x46 | 0x47 | 0x7e) {
            if let Some(r) = i.ops.get(2).and_then(|o| w.res.get_mut(&o.reg())) {
                r.depth = true;
            }
        }
    }
    Ok(())
}

fn interp_attr(mode: u32, k: K) -> &'static str {
    if k != K::F || mode == 1 {
        return "@interpolate(flat)";
    }
    match mode {
        3 => "@interpolate(perspective, centroid)",
        4 => "@interpolate(linear)",
        5 => "@interpolate(linear, centroid)",
        6 => "@interpolate(perspective, sample)",
        7 => "@interpolate(linear, sample)",
        _ => "",
    }
}

fn parse(code: &[u8]) -> R<(Container<'_>, Program)> {
    let c = Container::parse(code)?;
    let shdr = c.chunk(b"SHEX").or_else(|| c.chunk(b"SHDR")).ok_or("no SHDR/SHEX chunk (DXIL is not read yet)")?;
    let p = tpf::decode(shdr)?;
    Ok((c, p))
}

/// The pixel shader's input registers that come from the vertex shader.
pub fn varyings(ps: &[u8]) -> R<Vec<Varying>> {
    let (c, p) = parse(ps)?;
    let isgn = c.signature(false);
    let mut w = W::new(true, isgn.clone(), Vec::new());
    declarations(&mut w, &p)?;
    Ok(w.ps_varyings())
}

impl W {
    fn new(pixel: bool, isgn: Vec<SigElement>, osgn: Vec<SigElement>) -> W {
        W {
            pixel,
            isgn,
            osgn,
            temps: 0,
            xs: BTreeMap::new(),
            cbs: BTreeMap::new(),
            icb: Vec::new(),
            res: BTreeMap::new(),
            samplers: BTreeMap::new(),
            inputs: BTreeMap::new(),
            outputs: BTreeMap::new(),
            depth_out: false,
            mask_out: false,
            used_cb: Default::default(),
            used_t: Default::default(),
            used_s: Default::default(),
            nonfinite: false,
            body: String::new(),
            depth: 1,
            tmp: 0,
            switches: Vec::new(),
            blocks: Vec::new(),
            last_op: u32::MAX,
        }
    }

    fn ps_varyings(&self) -> Vec<Varying> {
        let mut v = Vec::new();
        for (&reg, d) in &self.inputs {
            if d.sysval != 0 {
                continue;
            }
            let elems: Vec<SigElement> = self.isgn.iter().filter(|e| e.reg == reg && e.sysval == 0).cloned().collect();
            let k = elems.first().map_or(K::F, |e| K::of_sig(e.component_type));
            v.push(Varying { reg, k, interp: interp_attr(d.interp, k), elems });
        }
        v
    }

    fn line(&mut self, s: &str) {
        for _ in 0..self.depth {
            self.body.push_str("    ");
        }
        self.body.push_str(s);
        self.body.push('\n');
    }

    fn fresh(&mut self) -> String {
        self.tmp += 1;
        format!("tmp{}", self.tmp)
    }

    fn lit(&mut self, k: K, v: u32) -> String {
        match k {
            K::U => format!("{v}u"),
            K::I => {
                let i = v as i32;
                if i == i32::MIN {
                    "bitcast<i32>(0x80000000u)".into()
                } else if i < 0 {
                    format!("({i}i)")
                } else {
                    format!("{i}i")
                }
            }
            K::F => {
                let f = f32::from_bits(v);
                if f.is_finite() {
                    let s = format!("{f:?}");
                    if f < 0.0 || (f == 0.0 && f.is_sign_negative()) { format!("({s}f)") } else { format!("{s}f") }
                } else {
                    // (no infinity or NaN literals in WGSL: from a value it cannot fold)
                    self.nonfinite = true;
                    format!("bitcast<f32>({v}u + nf_zero)")
                }
            }
        }
    }

    fn index(&mut self, i: &Index) -> R<String> {
        Ok(match i {
            Index::Imm(v) => format!("{v}u"),
            Index::Rel(o, 0) => self.src(o, K::U, &[0])?,
            Index::Rel(o, off) => format!("({} + {off}u)", self.src(o, K::U, &[0])?),
        })
    }

    /// A register as a vec4<u32> expression (sources) or place (destinations).
    fn reg(&mut self, o: &Operand) -> R<String> {
        let imm = |n: usize| match o.idx.get(n) {
            Some(Index::Imm(v)) => Ok(*v),
            _ => Err(format!("relative register index (type {})", o.ty)),
        };
        Ok(match o.ty {
            tpf::TEMP => format!("r{}", imm(0)?),
            tpf::INPUT => format!("v{}", imm(0)?),
            tpf::OUTPUT => format!("o{}", imm(0)?),
            tpf::INDEXABLE_TEMP => {
                let i = self.index(o.idx.get(1).ok_or("x register without index")?)?;
                format!("x{}[{i}]", imm(0)?)
            }
            tpf::CONSTANT_BUFFER => {
                let n = imm(0)?;
                if !self.cbs.contains_key(&n) {
                    return Err(format!("cb{n} not declared"));
                }
                self.used_cb.insert(n);
                let i = self.index(o.idx.get(1).ok_or("cb without index")?)?;
                format!("cb{n}.v[{i}]")
            }
            tpf::IMM_CONSTANT_BUFFER => {
                let i = self.index(o.idx.first().ok_or("icb without index")?)?;
                format!("icb[{i}]")
            }
            t => return Err(format!("operand type {t}")),
        })
    }

    /// A source as vecN<k> (N = comps.len(), a scalar for one): the components the destination's
    /// components `comps` read.
    fn src(&mut self, o: &Operand, k: K, comps: &[u8]) -> R<String> {
        if k == K::U && o.modifier != 0 {
            let e = self.src(o, K::I, comps)?;
            return Ok(format!("bitcast<{}>({e})", vty(K::U, comps.len())));
        }
        let swz = o.swizzle();
        let pick: Vec<u8> = comps.iter().map(|&c| swz[c as usize]).collect();
        let n = pick.len();
        let mut e = if o.ty == tpf::IMM32 {
            let vals: Vec<String> = pick.iter().map(|&c| self.lit(k, o.imm[c as usize])).collect();
            if n == 1 {
                vals[0].clone()
            } else if vals.iter().all(|v| *v == vals[0]) {
                format!("{}({})", vty(k, n), vals[0])
            } else {
                format!("{}({})", vty(k, n), vals.join(", "))
            }
        } else {
            let base = self.reg(o)?;
            let sw = if pick == [0, 1, 2, 3] { base } else { format!("{base}.{}", letters(&pick)) };
            if k == K::U { sw } else { format!("bitcast<{}>({sw})", vty(k, n)) }
        };
        match o.modifier {
            1 => e = format!("(-{e})"),
            2 => e = format!("abs({e})"),
            3 => e = format!("(-abs({e}))"),
            _ => {}
        }
        Ok(e)
    }

    fn splat(&mut self, k: K, n: usize, v: u32) -> String {
        let l = self.lit(k, v);
        if n == 1 { l } else { format!("{}({l})", vty(k, n)) }
    }

    /// Write `val` (vecN<k>, N = the mask's component count) to a destination.
    fn dst(&mut self, o: &Operand, val: String, k: K, sat: bool) -> R<()> {
        if o.ty == tpf::NULL {
            return Ok(());
        }
        let comps = bits(o.mask() as u32);
        let n = comps.len().max(1);
        let val = if sat && k == K::F { format!("saturate({val})") } else { val };
        let b = if k == K::U { val } else { format!("bitcast<{}>({val})", vty(K::U, n)) };
        match o.ty {
            tpf::OUTPUT_DEPTH | tpf::OUTPUT_DEPTH_GE | tpf::OUTPUT_DEPTH_LE => {
                self.line(&format!("oDepth = {b};"));
                return Ok(());
            }
            tpf::OUTPUT_COVERAGE_MASK => {
                self.line(&format!("oMask = {b};"));
                return Ok(());
            }
            _ => {}
        }
        let place = self.reg(o)?;
        if comps.len() == 4 {
            self.line(&format!("{place} = {b};"));
        } else if comps.len() == 1 {
            self.line(&format!("{place}.{} = {b};", XYZW[comps[0] as usize]));
        } else {
            let t = self.fresh();
            let mut s = format!("{{ let {t} = {b};");
            for (i, c) in comps.iter().enumerate() {
                let _ = write!(s, " {place}.{} = {t}.{};", XYZW[*c as usize], XYZW[i]);
            }
            s.push_str(" }");
            self.line(&s);
        }
        Ok(())
    }

    /// A component-wise instruction: dst = f(sources read as `kin`) of type `kout`.
    fn cw(&mut self, i: &Inst, kin: K, kout: K, f: impl Fn(&[String]) -> String) -> R<()> {
        let comps = bits(i.ops[0].mask() as u32);
        let mut a = Vec::new();
        for o in &i.ops[1..] {
            a.push(self.src(o, kin, &comps)?);
        }
        let v = f(&a);
        self.dst(&i.ops[0], v, kout, i.sat)
    }

    /// The same, with a per-component scalar function (for builtins that take scalars).
    fn per_comp(&mut self, i: &Inst, kin: &[K], kout: K, f: impl Fn(&[String]) -> String) -> R<()> {
        let comps = bits(i.ops[0].mask() as u32);
        let n = comps.len();
        let mut names = Vec::new();
        for (j, o) in i.ops[1..].iter().enumerate() {
            let e = self.src(o, kin[j.min(kin.len() - 1)], &comps)?;
            let t = self.fresh();
            self.line(&format!("let {t} = {e};"));
            names.push(t);
        }
        let parts: Vec<String> = (0..n)
            .map(|c| {
                let args: Vec<String> = names.iter().map(|t| if n == 1 { t.clone() } else { format!("{t}.{}", XYZW[c]) }).collect();
                f(&args)
            })
            .collect();
        let v = if n == 1 { parts[0].clone() } else { format!("{}({})", vty(kout, n), parts.join(", ")) };
        self.dst(&i.ops[0], v, kout, i.sat)
    }

    fn cmp(&mut self, i: &Inst, k: K, op: &str) -> R<()> {
        let n = bits(i.ops[0].mask() as u32).len();
        let (z, t) = (self.splat(K::U, n, 0), self.splat(K::U, n, 0xffff_ffff));
        self.cw(i, k, K::U, |a| format!("select({z}, {t}, {} {op} {})", a[0], a[1]))
    }

    fn cond(&mut self, i: &Inst) -> R<String> {
        let c = self.src(&i.ops[0], K::U, &[0])?;
        Ok(if i.nz { format!("{c} != 0u") } else { format!("{c} == 0u") })
    }

    fn dot(&mut self, i: &Inst, n: usize) -> R<()> {
        let comps: Vec<u8> = (0..n as u8).collect();
        let a = self.src(&i.ops[1], K::F, &comps)?;
        let b = self.src(&i.ops[2], K::F, &comps)?;
        let d = if n == 1 { format!("({a} * {b})") } else { format!("dot({a}, {b})") };
        let m = bits(i.ops[0].mask() as u32).len();
        let v = if m == 1 { d } else { format!("vec{m}<f32>({d})") };
        self.dst(&i.ops[0], v, K::F, i.sat)
    }

    /// A vec4 result (of `k`) that the destination takes through `swz` (the resource operand's
    /// swizzle).
    fn vec4_result(&mut self, i: &Inst, e: String, k: K, swz: [u8; 4]) -> R<()> {
        let comps = bits(i.ops[0].mask() as u32);
        let pick: Vec<u8> = comps.iter().map(|&c| swz[c as usize]).collect();
        let t = self.fresh();
        self.line(&format!("let {t} = {e};"));
        let v = if pick == [0, 1, 2, 3] { t } else { format!("{t}.{}", letters(&pick)) };
        self.dst(&i.ops[0], v, k, i.sat)
    }

    fn texture(&mut self, o: &Operand) -> R<(u32, u32, K, bool)> {
        if o.ty != tpf::RESOURCE {
            return Err(format!("resource operand type {}", o.ty));
        }
        let n = o.reg();
        let r = self.res.get(&n).ok_or(format!("t{n} not declared"))?;
        self.used_t.insert(n);
        Ok((n, r.dim, r.ret, r.depth))
    }

    fn sampler(&mut self, o: &Operand) -> R<u32> {
        if o.ty != tpf::SAMPLER {
            return Err(format!("sampler operand type {}", o.ty));
        }
        let n = o.reg();
        if !self.samplers.contains_key(&n) {
            return Err(format!("s{n} not declared"));
        }
        self.used_s.insert(n);
        Ok(n)
    }

    /// Sampling coordinates for a resource dimension: (coordinates, array index).
    fn sample_coords(&mut self, o: &Operand, dim: u32) -> R<(String, Option<String>)> {
        Ok(match dim {
            2 => (self.src(o, K::F, &[0])?, None),
            3 => (self.src(o, K::F, &[0, 1])?, None),
            5 | 6 => (self.src(o, K::F, &[0, 1, 2])?, None),
            8 => (self.src(o, K::F, &[0, 1])?, Some(format!("i32(round({}))", self.src(o, K::F, &[2])?))),
            10 => (self.src(o, K::F, &[0, 1, 2])?, Some(format!("i32(round({}))", self.src(o, K::F, &[3])?))),
            d => return Err(format!("sampling resource dimension {d}")),
        })
    }

    fn offset(&self, i: &Inst, dim: u32) -> Option<String> {
        if i.offset == [0; 3] {
            return None;
        }
        let o = i.offset;
        match dim {
            3 | 8 => Some(format!("vec2<i32>({}, {})", o[0], o[1])),
            5 => Some(format!("vec3<i32>({}, {}, {})", o[0], o[1], o[2])),
            _ => None,
        }
    }

    fn sample(&mut self, i: &Inst) -> R<()> {
        let (t, dim, k, depth) = self.texture(&i.ops[2])?;
        let s = self.sampler(&i.ops[3])?;
        let swz = i.ops[2].swizzle();
        let (coord, layer) = self.sample_coords(&i.ops[1], dim)?;
        let mut args = vec![format!("t{t}"), format!("s{s}"), coord];
        args.extend(layer);
        let off = self.offset(i, dim);
        let level = |w: &mut W, o: &Operand| -> R<String> {
            let l = w.src(o, K::F, &[0])?;
            Ok(if depth { format!("i32({l})") } else { l })
        };
        let (f, extra, compare) = match i.op {
            0x45 if self.pixel => ("textureSample", vec![], false),
            0x45 => ("textureSampleLevel", vec![if depth { "0i".into() } else { "0.0f".into() }], false),
            0x48 => ("textureSampleLevel", vec![level(self, &i.ops[4])?], false),
            0x4a if self.pixel => ("textureSampleBias", vec![self.src(&i.ops[4], K::F, &[0])?], false),
            0x4a => ("textureSampleLevel", vec![if depth { "0i".into() } else { "0.0f".into() }], false),
            0x49 => {
                let n: &[u8] = match dim {
                    2 => &[0],
                    3 | 8 => &[0, 1],
                    _ => &[0, 1, 2],
                };
                ("textureSampleGrad", vec![self.src(&i.ops[4], K::F, n)?, self.src(&i.ops[5], K::F, n)?], false)
            }
            0x46 if self.pixel => ("textureSampleCompare", vec![self.src(&i.ops[4], K::F, &[0])?], true),
            0x46 | 0x47 => ("textureSampleCompareLevel", vec![self.src(&i.ops[4], K::F, &[0])?], true),
            op => return Err(format!("sample opcode {op:#x}")),
        };
        if dim == 2 && f != "textureSample" {
            return Err("1D texture sampled with a level".into());
        }
        args.extend(extra);
        args.extend(off);
        let call = format!("{f}({})", args.join(", "));
        let e = if compare {
            format!("vec4<f32>({call})")
        } else if depth {
            format!("vec4<f32>({call}, 0.0f, 0.0f, 1.0f)")
        } else {
            call
        };
        self.vec4_result(i, e, if depth || compare { K::F } else { k }, swz)
    }

    fn gather(&mut self, i: &Inst) -> R<()> {
        let (t, dim, k, depth) = self.texture(&i.ops[2])?;
        let s = self.sampler(&i.ops[3])?;
        let swz = i.ops[2].swizzle();
        let channel = i.ops[3].swizzle()[0];
        let (coord, layer) = self.sample_coords(&i.ops[1], dim)?;
        let mut args = Vec::new();
        let compare = i.op == 0x7e;
        if !depth {
            args.push(format!("{channel}"));
        }
        args.extend([format!("t{t}"), format!("s{s}"), coord]);
        args.extend(layer);
        if compare {
            args.push(self.src(&i.ops[4], K::F, &[0])?);
        }
        args.extend(self.offset(i, dim));
        let f = if compare { "textureGatherCompare" } else { "textureGather" };
        self.vec4_result(i, format!("{f}({})", args.join(", ")), if depth { K::F } else { k }, swz)
    }

    fn load(&mut self, i: &Inst) -> R<()> {
        let (t, dim, k, depth) = self.texture(&i.ops[2])?;
        let swz = i.ops[2].swizzle();
        let o = &i.ops[1];
        let off = i.offset;
        let add = |e: String, n: usize| -> String {
            if off == [0; 3] {
                e
            } else if n == 1 {
                format!("({e} + {})", off[0])
            } else {
                format!("({e} + vec{n}<i32>({}))", off[..n].iter().map(|v| v.to_string()).collect::<Vec<_>>().join(", "))
            }
        };
        let mip = self.src(o, K::I, &[3])?;
        let args = match dim {
            1 => format!("vec2<i32>({}, 0), 0", self.src(o, K::I, &[0])?),
            2 => format!("{}, {}", add(self.src(o, K::I, &[0])?, 1), self.src(o, K::I, &[1])?),
            3 => format!("{}, {mip}", add(self.src(o, K::I, &[0, 1])?, 2)),
            8 => format!("{}, {}, {mip}", add(self.src(o, K::I, &[0, 1])?, 2), self.src(o, K::I, &[2])?),
            5 => format!("{}, {mip}", add(self.src(o, K::I, &[0, 1, 2])?, 3)),
            d => return Err(format!("ld from resource dimension {d}")),
        };
        let call = format!("textureLoad(t{t}, {args})");
        let e = if depth { format!("vec4<f32>({call}, 0.0f, 0.0f, 1.0f)") } else { call };
        self.vec4_result(i, e, if depth { K::F } else { k }, swz)
    }

    fn resinfo(&mut self, i: &Inst) -> R<()> {
        let (t, dim, _, _) = self.texture(&i.ops[2])?;
        let swz = i.ops[2].swizzle();
        let mip = self.src(&i.ops[1], K::U, &[0])?;
        let dims = format!("textureDimensions(t{t}, {mip})");
        let levels = format!("textureNumLevels(t{t})");
        let u = match dim {
            2 => format!("vec4<u32>({dims}, 0u, 0u, {levels})"),
            3 | 6 => format!("vec4<u32>({dims}, 0u, {levels})"),
            8 | 10 => format!("vec4<u32>({dims}, textureNumLayers(t{t}), {levels})"),
            5 => format!("vec4<u32>({dims}, {levels})"),
            d => return Err(format!("resinfo of resource dimension {d}")),
        };
        let (e, k) = match i.ctl & 3 {
            2 => (u, K::U),
            1 => {
                let f = format!("vec4<f32>({u})");
                (format!("(vec4<f32>(1.0f, 1.0f, 1.0f, 0.0f) / {f} + vec4<f32>(0.0f, 0.0f, 0.0f, 1.0f) * {f})"), K::F)
            }
            _ => (format!("vec4<f32>({u})"), K::F),
        };
        self.vec4_result(i, e, k, swz)
    }

    fn open(&mut self, s: &str, kind: u8) {
        self.line(s);
        self.depth += 1;
        self.blocks.push(kind);
    }

    fn close(&mut self) {
        self.depth -= 1;
        self.blocks.pop();
        self.line("}");
    }

    /// Before a statement in a switch: open the case its labels name.
    fn case_body(&mut self) -> bool {
        if self.blocks.last() != Some(&b's') {
            return true;
        }
        let sw = self.switches.last_mut().unwrap();
        if sw.1 {
            return true;
        }
        if sw.0.is_empty() {
            // (dead code after a case's break)
            return false;
        }
        let labels = std::mem::take(&mut sw.0).join(", ");
        sw.1 = true;
        self.line(&format!("case {labels}: {{"));
        self.depth += 1;
        true
    }

    fn close_case(&mut self) {
        let sw = self.switches.last_mut().unwrap();
        if sw.1 {
            sw.1 = false;
            self.depth -= 1;
            self.line("}");
        }
    }

    fn inst(&mut self, i: &Inst) -> R<()> {
        use K::*;
        let op = i.op;
        // declarations
        if (0x58..=0x6a).contains(&op) || op == tpf::OP_CUSTOMDATA || (0x8f..=0xa2).contains(&op) {
            return Ok(());
        }
        // switch structure
        match op {
            0x06 | 0x0a => {
                if self.blocks.last() != Some(&b's') {
                    return Err("case outside a switch".into());
                }
                if self.switches.last().unwrap().1 {
                    if matches!(self.last_op, 0x3e | 0x07) {
                        self.close_case();
                    } else {
                        return Err("switch case falls through".into());
                    }
                }
                let label = if op == 0x0a {
                    self.switches.last_mut().unwrap().2 = true;
                    "default".to_string()
                } else {
                    format!("{}i", i.ops[0].imm[0] as i32)
                };
                self.switches.last_mut().unwrap().0.push(label);
                self.last_op = op;
                return Ok(());
            }
            0x17 => {
                if self.blocks.last() != Some(&b's') {
                    return Err("endswitch outside a switch".into());
                }
                self.close_case();
                let (pending, _, has_default) = self.switches.pop().unwrap();
                if !pending.is_empty() {
                    self.line(&format!("case {}: {{}}", pending.join(", ")));
                }
                if !has_default {
                    self.line("default: {}");
                }
                self.close();
                self.last_op = op;
                return Ok(());
            }
            0x02 if self.blocks.last() == Some(&b's') => {
                self.close_case();
                self.last_op = op;
                return Ok(());
            }
            0x12 | 0x15 | 0x16 => {}
            _ => {
                if !self.case_body() {
                    return Ok(());
                }
            }
        }
        self.last_op = op;
        match op {
            0x00 => self.cw(i, F, F, |a| format!("({} + {})", a[0], a[1]))?,
            0x01 => self.cw(i, U, U, |a| format!("({} & {})", a[0], a[1]))?,
            0x02 => self.line("break;"),
            0x03 => {
                let c = self.cond(i)?;
                self.line(&format!("if ({c}) {{ break; }}"));
            }
            0x07 => self.line("continue;"),
            0x08 => {
                let c = self.cond(i)?;
                self.line(&format!("if ({c}) {{ continue; }}"));
            }
            0x0b => self.cw(i, F, F, |a| format!("dpdx({})", a[0]))?,
            0x0c => self.cw(i, F, F, |a| format!("dpdy({})", a[0]))?,
            0x7a => self.cw(i, F, F, |a| format!("dpdxCoarse({})", a[0]))?,
            0x7b => self.cw(i, F, F, |a| format!("dpdxFine({})", a[0]))?,
            0x7c => self.cw(i, F, F, |a| format!("dpdyCoarse({})", a[0]))?,
            0x7d => self.cw(i, F, F, |a| format!("dpdyFine({})", a[0]))?,
            0x0d => {
                let c = self.cond(i)?;
                self.line(&format!("if ({c}) {{ discard; }}"));
            }
            0x0e => self.cw(i, F, F, |a| format!("({} / {})", a[0], a[1]))?,
            0x0f => self.dot(i, 2)?,
            0x10 => self.dot(i, 3)?,
            0x11 => self.dot(i, 4)?,
            0x12 => {
                if self.blocks.last() != Some(&b'i') {
                    return Err("else outside an if".into());
                }
                self.depth -= 1;
                self.line("} else {");
                self.depth += 1;
            }
            0x15 | 0x16 => self.close(),
            0x18 => self.cmp(i, F, "==")?,
            0x1d => self.cmp(i, F, ">=")?,
            0x31 => self.cmp(i, F, "<")?,
            0x39 => self.cmp(i, F, "!=")?,
            0x20 => self.cmp(i, I, "==")?,
            0x21 => self.cmp(i, I, ">=")?,
            0x22 => self.cmp(i, I, "<")?,
            0x27 => self.cmp(i, I, "!=")?,
            0x4f => self.cmp(i, U, "<")?,
            0x50 => self.cmp(i, U, ">=")?,
            0x19 => self.cw(i, F, F, |a| format!("exp2({})", a[0]))?,
            0x1a => self.cw(i, F, F, |a| format!("fract({})", a[0]))?,
            0x1b => {
                let n = vty(I, bits(i.ops[0].mask() as u32).len());
                self.cw(i, F, I, |a| format!("{n}({})", a[0]))?
            }
            0x1c => {
                let n = vty(U, bits(i.ops[0].mask() as u32).len());
                self.cw(i, F, U, |a| format!("{n}({})", a[0]))?
            }
            0x2b => {
                let n = vty(F, bits(i.ops[0].mask() as u32).len());
                self.cw(i, I, F, |a| format!("{n}({})", a[0]))?
            }
            0x56 => {
                let n = vty(F, bits(i.ops[0].mask() as u32).len());
                self.cw(i, U, F, |a| format!("{n}({})", a[0]))?
            }
            0x1e => self.cw(i, I, I, |a| format!("({} + {})", a[0], a[1]))?,
            0x1f => {
                let c = self.cond(i)?;
                self.open(&format!("if ({c}) {{"), b'i');
            }
            0x23 => self.cw(i, I, I, |a| format!("({} * {} + {})", a[0], a[1], a[2]))?,
            0x52 => self.cw(i, U, U, |a| format!("({} * {} + {})", a[0], a[1], a[2]))?,
            0x24 => self.cw(i, I, I, |a| format!("max({}, {})", a[0], a[1]))?,
            0x25 => self.cw(i, I, I, |a| format!("min({}, {})", a[0], a[1]))?,
            0x53 => self.cw(i, U, U, |a| format!("max({}, {})", a[0], a[1]))?,
            0x54 => self.cw(i, U, U, |a| format!("min({}, {})", a[0], a[1]))?,
            0x26 | 0x51 => {
                // (high, low) = a * b: the low half only
                if i.ops[0].ty != tpf::NULL {
                    return Err("the high half of imul/umul".into());
                }
                let k = if op == 0x26 { I } else { U };
                let comps = bits(i.ops[1].mask() as u32);
                let a = self.src(&i.ops[2], k, &comps)?;
                let b = self.src(&i.ops[3], k, &comps)?;
                self.dst(&i.ops[1], format!("({a} * {b})"), k, false)?;
            }
            0x4e => {
                // udiv quotient, remainder, a, b
                for (d, opr) in [(0usize, "/"), (1, "%")] {
                    if i.ops[d].ty == tpf::NULL {
                        continue;
                    }
                    let comps = bits(i.ops[d].mask() as u32);
                    let a = self.src(&i.ops[2], U, &comps)?;
                    let b = self.src(&i.ops[3], U, &comps)?;
                    let t = self.fresh();
                    self.line(&format!("let {t} = {b};"));
                    let n = comps.len();
                    let ones = self.splat(U, n, 0xffff_ffff);
                    let z = self.splat(U, n, 0);
                    // (D3D: all ones for a division by zero)
                    let v = format!("select({a} {opr} {t}, {ones}, {t} == {z})");
                    if d == 0 && i.ops[1].ty != tpf::NULL && same_place(&i.ops[0], &i.ops[2..4]) {
                        return Err("udiv quotient overwrites its operands".into());
                    }
                    self.dst(&i.ops[d], v, U, false)?;
                }
            }
            0x28 => self.cw(i, I, I, |a| format!("(-{})", a[0]))?,
            0x29 => self.shift(i, I, "<<")?,
            0x2a => self.shift(i, I, ">>")?,
            0x55 => self.shift(i, U, ">>")?,
            0x2d => self.load(i)?,
            0x2f => self.cw(i, F, F, |a| format!("log2({})", a[0]))?,
            0x30 => self.open("loop {", b'l'),
            0x32 => self.cw(i, F, F, |a| format!("({} * {} + {})", a[0], a[1], a[2]))?,
            0x33 => self.cw(i, F, F, |a| format!("min({}, {})", a[0], a[1]))?,
            0x34 => self.cw(i, F, F, |a| format!("max({}, {})", a[0], a[1]))?,
            0x36 => {
                let k = if i.sat || i.ops[1].modifier != 0 { F } else { U };
                self.cw(i, k, k, |a| a[0].clone())?
            }
            0x37 => {
                let k = if i.sat || i.ops[2..].iter().any(|o| o.modifier != 0) { F } else { U };
                let comps = bits(i.ops[0].mask() as u32);
                let c = self.src(&i.ops[1], U, &comps)?;
                let a = self.src(&i.ops[2], k, &comps)?;
                let b = self.src(&i.ops[3], k, &comps)?;
                let z = self.splat(U, comps.len(), 0);
                self.dst(&i.ops[0], format!("select({b}, {a}, {c} != {z})"), k, i.sat)?;
            }
            0x38 => self.cw(i, F, F, |a| format!("({} * {})", a[0], a[1]))?,
            0x3a => {}
            0x3b => self.cw(i, U, U, |a| format!("(~{})", a[0]))?,
            0x3c => self.cw(i, U, U, |a| format!("({} | {})", a[0], a[1]))?,
            0x57 => self.cw(i, U, U, |a| format!("({} ^ {})", a[0], a[1]))?,
            0x3d => self.resinfo(i)?,
            0x3e => self.line("return;"),
            0x3f => {
                let c = self.cond(i)?;
                self.line(&format!("if ({c}) {{ return; }}"));
            }
            0x40 => self.cw(i, F, F, |a| format!("round({})", a[0]))?,
            0x41 => self.cw(i, F, F, |a| format!("floor({})", a[0]))?,
            0x42 => self.cw(i, F, F, |a| format!("ceil({})", a[0]))?,
            0x43 => self.cw(i, F, F, |a| format!("trunc({})", a[0]))?,
            0x44 => self.cw(i, F, F, |a| format!("inverseSqrt({})", a[0]))?,
            0x45..=0x4a => self.sample(i)?,
            0x4b => self.cw(i, F, F, |a| format!("sqrt({})", a[0]))?,
            0x4c => {
                let c = self.src(&i.ops[0], I, &[0])?;
                self.open(&format!("switch ({c}) {{"), b's');
                self.switches.push((Vec::new(), false, false));
            }
            0x4d => {
                // sincos sin, cos, a
                let t = self.fresh();
                let all = self.src(&i.ops[2], F, &[0, 1, 2, 3])?;
                self.line(&format!("let {t} = {all};"));
                for (d, f) in [(0usize, "sin"), (1, "cos")] {
                    if i.ops[d].ty == tpf::NULL {
                        continue;
                    }
                    let comps = bits(i.ops[d].mask() as u32);
                    let a = if comps.len() == 4 { t.clone() } else { format!("{t}.{}", letters(&comps)) };
                    self.dst(&i.ops[d], format!("{f}({a})"), F, i.sat)?;
                }
            }
            0x6d | 0x7e => self.gather(i)?,
            0x79 => {
                let (t, _, _, _) = self.texture(&i.ops[1])?;
                let swz = i.ops[1].swizzle();
                self.vec4_result(i, format!("vec4<u32>(textureDimensions(t{t}).x)"), U, swz)?;
            }
            0x81 => self.cw(i, F, F, |a| format!("(1.0f / {})", a[0]))?,
            0x82 => self.per_comp(i, &[F], U, |a| format!("(pack2x16float(vec2<f32>({}, 0.0f)) & 0xffffu)", a[0]))?,
            0x83 => self.per_comp(i, &[U], F, |a| format!("unpack2x16float({} & 0xffffu).x", a[0]))?,
            0x86 => self.cw(i, U, U, |a| format!("countOneBits({})", a[0]))?,
            0x87 => self.per_comp(i, &[U], U, |a| format!("select(31u - firstLeadingBit({0}), 0xffffffffu, {0} == 0u)", a[0]))?,
            0x88 => self.cw(i, U, U, |a| format!("firstTrailingBit({})", a[0]))?,
            0x89 => self.per_comp(i, &[I], U, |a| {
                format!("select(31u - bitcast<u32>(firstLeadingBit({0})), 0xffffffffu, {0} == 0i || {0} == -1i)", a[0])
            })?,
            0x8a => self.per_comp(i, &[U, U, U], U, |a| format!("extractBits({}, {} & 31u, {} & 31u)", a[2], a[1], a[0]))?,
            0x8b => self.per_comp(i, &[U, U, I], I, |a| format!("extractBits({}, {} & 31u, {} & 31u)", a[2], a[1], a[0]))?,
            0x8c => self.per_comp(i, &[U], U, |a| format!("insertBits({}, {}, {} & 31u, {} & 31u)", a[3], a[2], a[1], a[0]))?,
            0x8d => self.cw(i, U, U, |a| format!("reverseBits({})", a[0]))?,
            0x8e => {
                // swapc d0, d1, cond, a, b: d0 = cond ? b : a, d1 = cond ? a : b
                let mut vals = Vec::new();
                for d in 0..2 {
                    let comps = bits(i.ops[d].mask() as u32);
                    let c = self.src(&i.ops[2], U, &comps)?;
                    let a = self.src(&i.ops[3], U, &comps)?;
                    let b = self.src(&i.ops[4], U, &comps)?;
                    let z = self.splat(U, comps.len(), 0);
                    let t = self.fresh();
                    let v = if d == 0 { format!("select({a}, {b}, {c} != {z})") } else { format!("select({b}, {a}, {c} != {z})") };
                    self.line(&format!("let {t} = {v};"));
                    vals.push(t);
                }
                for (d, v) in vals.into_iter().enumerate() {
                    self.dst(&i.ops[d], v, U, false)?;
                }
            }
            op => return Err(format!("opcode {op:#x}")),
        }
        Ok(())
    }

    fn shift(&mut self, i: &Inst, k: K, opr: &str) -> R<()> {
        let comps = bits(i.ops[0].mask() as u32);
        let a = self.src(&i.ops[1], k, &comps)?;
        let b = self.src(&i.ops[2], K::U, &comps)?;
        let m = self.splat(K::U, comps.len(), 31);
        self.dst(&i.ops[0], format!("({a} {opr} ({b} & {m}))"), k, false)
    }
}

/// Whether `d` is one of `srcs`' registers (a write that the next read would see).
fn same_place(d: &Operand, srcs: &[Operand]) -> bool {
    srcs.iter().any(|s| s.ty == d.ty && s.ty != tpf::IMM32 && s.reg() == d.reg() && s.idx.len() == d.idx.len())
}

fn texture_type(dim: u32, k: K, depth: bool) -> R<(String, u32)> {
    let vd = match dim {
        1 | 3 => 1,
        2 => 0,
        5 => 5,
        6 => 3,
        8 => 2,
        10 => 4,
        d => return Err(format!("resource dimension {d}")),
    };
    if depth {
        let t = match vd {
            1 => "texture_depth_2d",
            2 => "texture_depth_2d_array",
            3 => "texture_depth_cube",
            4 => "texture_depth_cube_array",
            _ => return Err("depth comparison on a 1D/3D texture".into()),
        };
        return Ok((t.into(), vd));
    }
    let t = ["texture_1d", "texture_2d", "texture_2d_array", "texture_cube", "texture_cube_array", "texture_3d"][vd as usize];
    Ok((format!("{t}<{}>", k.s()), vd))
}

/// DXBC (shader model 4/5) to WGSL; `ps`: the pixel shader a vertex shader feeds.
pub fn translate(code: &[u8], pixel: bool, ps: Option<&[u8]>) -> R<Wgsl> {
    let (c, p) = parse(code)?;
    if p.kind != if pixel { tpf::PIXEL } else { tpf::VERTEX } {
        return Err(format!("program type {} for a {} shader", p.kind, if pixel { "pixel" } else { "vertex" }));
    }
    let mut w = W::new(pixel, c.signature(false), c.signature(true));
    declarations(&mut w, &p)?;
    for i in &p.insts {
        w.inst(i)?;
    }
    if !w.blocks.is_empty() {
        return Err("unclosed block".into());
    }
    let group = pixel as u32;
    let mut m = String::from("diagnostic(off, derivative_uniformity);\n\n");
    let mut bindings = Vec::new();
    for &n in &w.used_cb {
        let size = w.cbs[&n];
        let _ = writeln!(m, "struct CB{n} {{ v: array<vec4<u32>, {size}> }}");
        let _ = writeln!(m, "@group({group}) @binding({n}) var<uniform> cb{n}: CB{n};");
        bindings.push((n, Binding::Uniform(size * 16)));
    }
    for &n in &w.used_t {
        let r = &w.res[&n];
        let (ty, vd) = texture_type(r.dim, r.ret, r.depth)?;
        let _ = writeln!(m, "@group({group}) @binding({}) var t{n}: {ty};", 16 + n);
        let st = if r.depth {
            3
        } else {
            match r.ret {
                K::U => 1,
                K::I => 2,
                K::F => 0,
            }
        };
        bindings.push((16 + n, Binding::Texture(vd, st)));
    }
    for &n in &w.used_s {
        let cmp = w.samplers[&n];
        let _ = writeln!(m, "@group({group}) @binding({}) var s{n}: {};", 160 + n, if cmp { "sampler_comparison" } else { "sampler" });
        bindings.push((160 + n, Binding::Sampler(cmp)));
    }
    for r in 0..w.temps {
        let _ = writeln!(m, "var<private> r{r}: vec4<u32>;");
    }
    for (x, n) in &w.xs {
        let _ = writeln!(m, "var<private> x{x}: array<vec4<u32>, {n}>;");
    }
    for r in w.inputs.keys() {
        let _ = writeln!(m, "var<private> v{r}: vec4<u32>;");
    }
    for r in w.outputs.keys() {
        let _ = writeln!(m, "var<private> o{r}: vec4<u32>;");
    }
    if w.depth_out {
        m.push_str("var<private> oDepth: u32;\n");
    }
    if w.mask_out {
        m.push_str("var<private> oMask: u32;\n");
    }
    if w.nonfinite {
        m.push_str("var<private> nf_zero: u32;\n");
    }
    if !w.icb.is_empty() {
        let n = w.icb.len() / 4;
        let items: Vec<String> = w.icb.chunks_exact(4).map(|c| format!("vec4<u32>({}u, {}u, {}u, {}u)", c[0], c[1], c[2], c[3])).collect();
        let _ = writeln!(m, "var<private> icb: array<vec4<u32>, {n}> = array<vec4<u32>, {n}>({});", items.join(", "));
    }
    let _ = write!(m, "\nfn shader_main() {{\n{}}}\n\n", w.body);
    // the entry point
    let mut params = Vec::new();
    let mut pre = Vec::new();
    let mut fields = Vec::new();
    let mut post = Vec::new();
    let sig_k = |sig: &[SigElement], reg: u32| sig.iter().find(|e| e.reg == reg).map_or(K::F, |e| K::of_sig(e.component_type));
    let assign = |place: String, mask: u32, val: String| -> String {
        let c = bits(mask);
        if c.len() == 4 {
            format!("{place} = {val};")
        } else {
            c.iter().map(|&c| format!("{place}.{0} = {val}.{0};", XYZW[c as usize])).collect::<Vec<_>>().join(" ")
        }
    };
    if pixel {
        let vary = w.ps_varyings();
        for (&r, d) in &w.inputs {
            match d.sysval {
                0 => {
                    let v = vary.iter().find(|v| v.reg == r).unwrap();
                    params.push(format!("@location({r}) {} in{r}: {}", v.interp, vty(v.k, 4)));
                    let val = if v.k == K::U { format!("in{r}") } else { format!("bitcast<vec4<u32>>(in{r})") };
                    pre.push(assign(format!("v{r}"), d.mask, val));
                }
                1 => {
                    params.push("@builtin(position) in_pos: vec4<f32>".into());
                    pre.push(assign(format!("v{r}"), d.mask, "bitcast<vec4<u32>>(in_pos)".into()));
                }
                9 => {
                    params.push("@builtin(front_facing) in_ff: bool".into());
                    pre.push(assign(format!("v{r}"), d.mask, "vec4<u32>(select(0u, 0xffffffffu, in_ff))".into()));
                }
                10 => {
                    params.push("@builtin(sample_index) in_si: u32".into());
                    pre.push(assign(format!("v{r}"), d.mask, "vec4<u32>(in_si)".into()));
                }
                s => return Err(format!("pixel shader input system value {s}")),
            }
        }
        for &r in w.outputs.keys() {
            let k = sig_k(&w.osgn, r);
            fields.push(format!("@location({r}) c{r}: {}", vty(k, 4)));
            post.push(if k == K::U { format!("out.c{r} = o{r};") } else { format!("out.c{r} = bitcast<vec4<{}>>(o{r});", k.s()) });
        }
        if w.depth_out {
            fields.push("@builtin(frag_depth) depth: f32".into());
            post.push("out.depth = bitcast<f32>(oDepth);".into());
        }
        if w.mask_out {
            fields.push("@builtin(sample_mask) mask: u32".into());
            post.push("out.mask = oMask;".into());
        }
    } else {
        for (&r, d) in &w.inputs {
            match d.sysval {
                0 => {
                    let k = sig_k(&w.isgn, r);
                    params.push(format!("@location({r}) in{r}: {}", vty(k, 4)));
                    let val = if k == K::U { format!("in{r}") } else { format!("bitcast<vec4<u32>>(in{r})") };
                    pre.push(assign(format!("v{r}"), d.mask, val));
                }
                6 => {
                    params.push("@builtin(vertex_index) in_vid: u32".into());
                    pre.push(assign(format!("v{r}"), d.mask, "vec4<u32>(in_vid)".into()));
                }
                8 => {
                    params.push("@builtin(instance_index) in_iid: u32".into());
                    pre.push(assign(format!("v{r}"), d.mask, "vec4<u32>(in_iid)".into()));
                }
                s => return Err(format!("vertex shader input system value {s}")),
            }
        }
        // position
        let pos = w.outputs.iter().find(|o| o.1 .1 == 1).map(|o| *o.0).or_else(|| w.osgn.iter().find(|e| e.sysval == 1).map(|e| e.reg));
        fields.push("@builtin(position) pos: vec4<f32>".into());
        post.push(match pos {
            Some(r) if w.outputs.contains_key(&r) => format!("out.pos = bitcast<vec4<f32>>(o{r});"),
            _ => "out.pos = vec4<f32>(0.0f);".into(),
        });
        match ps.map(varyings) {
            Some(Ok(vary)) => {
                for v in vary {
                    let mut comp = ["0u".to_string(), "0u".to_string(), "0u".to_string(), "0u".to_string()];
                    for e in &v.elems {
                        let Some(f) = w.osgn.iter().find(|f| f.sysval == 0 && f.name.eq_ignore_ascii_case(&e.name) && f.index == e.index) else {
                            continue;
                        };
                        if !w.outputs.contains_key(&f.reg) {
                            continue;
                        }
                        for (pc, vc) in bits(e.mask).into_iter().zip(bits(f.mask)) {
                            comp[pc as usize] = format!("o{}.{}", f.reg, XYZW[vc as usize]);
                        }
                    }
                    let r = v.reg;
                    fields.push(format!("@location({r}) {} l{r}: {}", v.interp, vty(v.k, 4)));
                    let u = format!("vec4<u32>({})", comp.join(", "));
                    post.push(if v.k == K::U { format!("out.l{r} = {u};") } else { format!("out.l{r} = bitcast<vec4<{}>>({u});", v.k.s()) });
                }
            }
            Some(Err(e)) => return Err(format!("its pixel shader: {e}")),
            None => {
                for (&r, &(_, sysval)) in &w.outputs {
                    if sysval != 0 || Some(r) == pos {
                        continue;
                    }
                    let k = sig_k(&w.osgn, r);
                    let interp = interp_attr(2, k);
                    fields.push(format!("@location({r}) {interp} l{r}: {}", vty(k, 4)));
                    post.push(if k == K::U { format!("out.l{r} = o{r};") } else { format!("out.l{r} = bitcast<vec4<{}>>(o{r});", k.s()) });
                }
            }
        }
    }
    let stage = if pixel { "@fragment" } else { "@vertex" };
    if fields.is_empty() {
        let _ = writeln!(m, "{stage}\nfn main({}) {{", params.join(", "));
    } else {
        let _ = writeln!(m, "struct Out {{\n    {},\n}}\n", fields.join(",\n    "));
        let _ = writeln!(m, "{stage}\nfn main({}) -> Out {{", params.join(", "));
    }
    for s in &pre {
        let _ = writeln!(m, "    {s}");
    }
    m.push_str("    shader_main();\n");
    if !fields.is_empty() {
        m.push_str("    var out: Out;\n");
        for s in &post {
            let _ = writeln!(m, "    {s}");
        }
        m.push_str("    return out;\n");
    }
    m.push_str("}\n");
    Ok(Wgsl { code: m, bindings })
}

#[cfg(test)]
mod tests {
    /// Every DXBC file in $VNVM_DXBC_DIR (shaders taken from games) translates to WGSL that naga
    /// accepts. Pixel or vertex comes from the program type; each vertex shader is linked to
    /// every pixel shader in the directory.
    #[test]
    fn corpus() {
        let Ok(dir) = std::env::var("VNVM_DXBC_DIR") else { return };
        let mut shaders = Vec::new();
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let code = std::fs::read(&p).unwrap();
            let Ok((_, prog)) = super::parse(&code) else { continue };
            shaders.push((p, code, prog.kind));
        }
        let mut fails = 0;
        let check = |name: &str, code: &[u8], pixel: bool, ps: Option<&[u8]>| -> bool {
            match super::translate(code, pixel, ps) {
                Ok(w) => match naga::front::wgsl::parse_str(&w.code) {
                    Ok(m) => {
                        let v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m);
                        if let Err(e) = v {
                            eprintln!("{name}: invalid: {e:?}\n{}", w.code);
                            return false;
                        }
                        if std::env::var_os("VNVM_DXBC_PRINT").is_some() {
                            eprintln!("{name}:\n{}", w.code);
                        }
                        true
                    }
                    Err(e) => {
                        eprintln!("{name}: {}\n{}", e.emit_to_string(&w.code), w.code);
                        false
                    }
                },
                Err(e) => {
                    eprintln!("{name}: {e}");
                    false
                }
            }
        };
        for (p, code, kind) in &shaders {
            let name = p.display().to_string();
            match *kind {
                0 => fails += !check(&name, code, true, None) as u32,
                1 => {
                    fails += !check(&name, code, false, None) as u32;
                    for (q, ps, k) in &shaders {
                        if *k == 0 {
                            fails += !check(&format!("{name} + {}", q.display()), code, false, Some(ps)) as u32;
                        }
                    }
                }
                k => eprintln!("{name}: program type {k} skipped"),
            }
        }
        assert_eq!(fails, 0);
    }
}
