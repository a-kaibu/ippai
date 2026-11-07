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

use crate::dxbc::{Container, SigElement};
use crate::{Binding, Wgsl};
use crate::tpf::{self, Index, Inst, Operand, Program};

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

/// A typed UAV (dcl_uav_typed)
struct UavDecl {
    dim: u32,
    ret: K,
}

struct InDecl {
    mask: u32,
    sysval: u32,
    interp: u32,
}

struct W {
    pixel: bool,
    /// a compute shader: [numthreads]
    threads: Option<[u32; 3]>,
    uavs: BTreeMap<u32, UavDecl>,
    /// groupshared memory: dwords of each g register, and the structured ones' strides
    tgsm: BTreeMap<u32, (u32, u32)>,
    /// compute inputs (register types 32..35) read
    sys_in: std::collections::BTreeSet<u32>,
    used_u: std::collections::BTreeSet<u32>,
    /// UAVs read (from a copy taken before the dispatch: binding 300 + n)
    used_ur: std::collections::BTreeSet<u32>,
    /// each UAV's WebGPU storage format
    uav_formats: BTreeMap<u32, String>,
    /// temporary registers as a variable per component (r1_x...), shader_main's own: WGSL's
    /// uniformity analysis follows a function's variables through assignments, a whole
    /// variable at a time (a loop counted in r0.x with r0.y written by each thread: a barrier
    /// in that loop is in uniform control flow only when r0.x is a variable of its own)
    split: bool,
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
    /// helper functions the body calls (see HELPERS)
    helpers: std::collections::BTreeSet<&'static str>,
    body: String,
    depth: usize,
    tmp: u32,
    /// open switch statements: (labels waiting for a body, a case is open, a default was given)
    switches: Vec<(Vec<String>, bool, bool)>,
    /// what each open block is: b'i' if, b'e' its else, b'l' loop, b's' switch
    blocks: Vec<u8>,
    last_op: u32,
    /// samplers addressing as WebGPU's can't (crate::addr), and the (texture, sampler) pairs
    /// sampled with them
    addr: Vec<crate::addr::Addr>,
    addr_used: std::collections::BTreeSet<(u32, u32)>,
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
            tpf::OP_DCL_INPUT if (32..=35).contains(&i.ops[0].ty) && p.kind == tpf::COMPUTE => {
                w.sys_in.insert(i.ops[0].ty);
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
            0x9b if p.kind == tpf::COMPUTE => {
                let [x, y, z] = i.extra[..] else { return Err("dcl_thread_group without 3 sizes".into()) };
                if x == 0 || y == 0 || z == 0 || x > 1024 || y > 1024 || z > 64 || x * y * z > 1024 {
                    return Err(format!("thread group {x}x{y}x{z} past Direct3D's limits"));
                }
                w.threads = Some([x, y, z]);
            }
            0x9c => {
                // (dimension in the control bits, the globally coherent flag above it)
                let ret = match i.extra.first().map(|r| r & 0xf) {
                    Some(3) => K::I,
                    Some(4) => K::U,
                    _ => K::F,
                };
                let o = &i.ops[0];
                if o.ty != tpf::UAV || o.reg() >= 64 {
                    return Err("dcl_uav_typed of a bad register".into());
                }
                w.uavs.insert(o.reg(), UavDecl { dim: i.ctl & 0x1f, ret });
            }
            0x9f | 0xa0 => {
                let o = &i.ops[0];
                let (bytes, stride) = match (i.op, &i.extra[..]) {
                    (0x9f, [b]) => (*b, 4),
                    (0xa0, [s, n]) => (s.saturating_mul(*n), *s),
                    _ => return Err("bad groupshared declaration".into()),
                };
                if o.ty != tpf::TGSM || bytes % 4 != 0 || stride % 4 != 0 || stride == 0 || bytes == 0 || bytes > 32768 {
                    return Err("groupshared memory past Direct3D's limits".into());
                }
                w.tgsm.insert(o.reg(), (bytes / 4, stride));
            }
            tpf::OP_DCL_RESOURCE_RAW | tpf::OP_DCL_RESOURCE_STRUCTURED | 0x9d | 0x9e => {
                return Err("raw/structured buffers and UAVs".into())
            }
            _ => {}
        }
    }
    if p.kind == tpf::COMPUTE {
        if w.threads.is_none() {
            return Err("a compute shader without dcl_thread_group".into());
        }
        if w.tgsm.values().map(|t| t.0).sum::<u32>() > 8192 {
            return Err("more than 32 KB of groupshared memory".into());
        }
    }
    check_registers(w, p)?;
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

/// Every register an instruction names is declared, and the declarations are within the
/// Direct3D 11 limits (4096 temps, 32 inputs/outputs, 15 constant buffers of 4096 vectors,
/// 128 resources, 16 samplers): bytecode that is not is rejected, not translated to WGSL
/// that names what does not exist.
fn check_registers(w: &W, p: &Program) -> R<()> {
    if w.temps > 4096 || w.xs.len() > 4096 || w.xs.iter().any(|(&r, &n)| r >= 4096 || n == 0 || n > 4096) {
        return Err("more temps than Direct3D allows".into());
    }
    if w.inputs.keys().chain(w.outputs.keys()).any(|&r| r >= 32) || w.cbs.keys().any(|&r| r >= 15) || w.res.keys().any(|&r| r >= 128) || w.samplers.keys().any(|&r| r >= 16) {
        return Err("a register past Direct3D's limits".into());
    }
    if (w.depth_out || w.mask_out) && p.kind != tpf::PIXEL {
        return Err("oDepth or oMask outside a pixel shader".into());
    }
    fn check(w: &W, o: &Operand) -> R<()> {
        if o.sel == tpf::Sel::Mask(0) && !matches!(o.ty, tpf::NULL | tpf::IMM32) {
            return Err("an empty write mask".into());
        }
        let dims = match o.ty {
            tpf::TEMP => {
                if o.reg() >= w.temps {
                    return Err(format!("r{} is not declared", o.reg()));
                }
                1
            }
            tpf::INDEXABLE_TEMP => {
                let n = w.xs.get(&o.reg()).ok_or(format!("x{} is not declared", o.reg()))?;
                if let Some(Index::Imm(k)) = o.idx.get(1) {
                    if k >= n {
                        return Err(format!("x{}[{k}] past its size", o.reg()));
                    }
                }
                2
            }
            tpf::INPUT => {
                if !w.inputs.contains_key(&o.reg()) {
                    return Err(format!("v{} is not declared", o.reg()));
                }
                1
            }
            tpf::OUTPUT => {
                if !w.outputs.contains_key(&o.reg()) {
                    return Err(format!("o{} is not declared", o.reg()));
                }
                1
            }
            tpf::CONSTANT_BUFFER => {
                let n = w.cbs.get(&o.reg()).ok_or(format!("cb{} is not declared", o.reg()))?;
                if let Some(Index::Imm(k)) = o.idx.get(1) {
                    if k >= n {
                        return Err(format!("cb{}[{k}] past its size", o.reg()));
                    }
                }
                2
            }
            tpf::RESOURCE => {
                if !w.res.contains_key(&o.reg()) {
                    return Err(format!("t{} is not declared", o.reg()));
                }
                1
            }
            tpf::SAMPLER => {
                if !w.samplers.contains_key(&o.reg()) {
                    return Err(format!("s{} is not declared", o.reg()));
                }
                1
            }
            tpf::IMM32 | tpf::NULL => 0,
            tpf::UAV => {
                if !w.uavs.contains_key(&o.reg()) {
                    return Err(format!("u{} is not declared", o.reg()));
                }
                1
            }
            tpf::TGSM => {
                if !w.tgsm.contains_key(&o.reg()) {
                    return Err(format!("g{} is not declared", o.reg()));
                }
                1
            }
            tpf::IMM_CONSTANT_BUFFER => {
                // icb[] reads the shader's dcl_immediateConstantBuffer (customdata class 3):
                // without one there is no icb array to read (fuzz: Formal shaders, seed 354)
                let n = w.icb.len() as u32 / 4;
                if n == 0 {
                    return Err("icb is not declared".into());
                }
                if let Some(Index::Imm(k)) = o.idx.first() {
                    if *k >= n {
                        return Err(format!("icb[{k}] past its size"));
                    }
                }
                1
            }
            32..=35 => {
                if !w.sys_in.contains(&o.ty) {
                    return Err(format!("compute input {} is not declared", o.ty));
                }
                0
            }
            _ => return Ok(()),
        };
        if o.idx.len() != dims || (dims > 0 && !matches!(o.idx[0], Index::Imm(_)) && o.ty != tpf::IMM_CONSTANT_BUFFER) {
            return Err(format!("operand of type {} with {} indices", o.ty, o.idx.len()));
        }
        for i in &o.idx {
            if let Index::Rel(r, _) = i {
                if !matches!(r.ty, tpf::TEMP | tpf::INDEXABLE_TEMP | tpf::INPUT | tpf::IMM32) {
                    return Err(format!("register of type {} as an index", r.ty));
                }
                check(w, r)?;
            }
        }
        Ok(())
    }
    for i in &p.insts {
        if tpf::arity(i.op).is_some() {
            for o in &i.ops {
                check(w, o)?;
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
            threads: None,
            uavs: BTreeMap::new(),
            tgsm: BTreeMap::new(),
            sys_in: Default::default(),
            used_u: Default::default(),
            used_ur: Default::default(),
            uav_formats: BTreeMap::new(),
            split: false,
            addr: Vec::new(),
            addr_used: Default::default(),
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
            helpers: Default::default(),
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
            32 => "vtid".into(),
            33 => "vgid".into(),
            34 => "vltid".into(),
            35 => "vlidx".into(),
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
        if n == 0 {
            return Err("a source no component of the destination reads".into());
        }
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
            let sw = if self.split && o.ty == tpf::TEMP {
                let parts: Vec<String> = pick.iter().map(|&c| format!("{base}_{}", XYZW[c as usize])).collect();
                if n == 1 { parts[0].clone() } else { format!("{}({})", vty(K::U, n), parts.join(", ")) }
            } else if pick == [0, 1, 2, 3] {
                base
            } else {
                format!("{base}.{}", letters(&pick))
            };
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
        if comps.is_empty() || (o.comps == 0 && matches!(o.ty, tpf::TEMP | tpf::INDEXABLE_TEMP | tpf::OUTPUT)) {
            // (writes nothing: no compiler makes it, and it is not valid bytecode)
            return Err("a destination with an empty write mask".into());
        }
        if sat && k != K::F {
            // (_sat is a float modifier: not valid on integer results)
            return Err("_sat on an integer result".into());
        }
        let n = comps.len();
        let val = if sat && k == K::F { format!("saturate({val})") } else { val };
        let b = if k == K::U { val } else { format!("bitcast<{}>({val})", vty(K::U, n)) };
        match o.ty {
            tpf::OUTPUT_DEPTH | tpf::OUTPUT_DEPTH_GE | tpf::OUTPUT_DEPTH_LE => {
                if !self.depth_out {
                    return Err("oDepth written without dcl_output oDepth".into());
                }
                self.line(&format!("oDepth = {b};"));
                return Ok(());
            }
            tpf::OUTPUT_COVERAGE_MASK => {
                if !self.mask_out {
                    return Err("oMask written without dcl_output oMask".into());
                }
                self.line(&format!("oMask = {b};"));
                return Ok(());
            }
            _ => {}
        }
        if !matches!(o.ty, tpf::TEMP | tpf::INDEXABLE_TEMP | tpf::OUTPUT) {
            return Err(format!("register type {} as a destination", o.ty));
        }
        let place = self.reg(o)?;
        if self.split && o.ty == tpf::TEMP {
            if n == 1 {
                self.line(&format!("{place}_{} = {b};", XYZW[comps[0] as usize]));
            } else {
                let t = self.fresh();
                let mut s = format!("{{ let {t} = {b};");
                for (i, c) in comps.iter().enumerate() {
                    let _ = write!(s, " {place}_{} = {t}.{};", XYZW[*c as usize], XYZW[i]);
                }
                s.push_str(" }");
                self.line(&s);
            }
            return Ok(());
        }
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
        // (immediates only: a constant expression, which WGSL rejects when it overflows or
        // divides by zero; one operand through nf_zero makes it a run-time value)
        if kin == K::F && !a.is_empty() && i.ops[1..].iter().all(|o| o.ty == tpf::IMM32) {
            self.nonfinite = true;
            let n = comps.len();
            a[0] = format!("bitcast<{}>(bitcast<{}>({}) + nf_zero)", vty(K::F, n), vty(K::U, n), a[0]);
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
        if op == "!==" {
            return self.cw(i, k, K::U, |a| format!("select({z}, {t}, !({} == {}))", a[0], a[1]));
        }
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
            2 => (format!("vec2<f32>({}, 0.5f)", self.src(o, K::F, &[0])?), None),
            7 => (format!("vec2<f32>({}, 0.5f)", self.src(o, K::F, &[0])?), Some(format!("i32(round({}))", self.src(o, K::F, &[1])?))),
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
            2 | 7 => Some(format!("vec2<i32>({}, 0)", o[0])),
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
        let off = self.offset(i, dim);
        let compare = matches!(i.op, 0x46 | 0x47);
        let addr = (matches!(dim, 3 | 8) && off.is_none() && !compare && !depth && k == K::F && self.addr.iter().any(|a| a.sampler == s)).then(|| coord.clone());
        let mut args = vec![format!("t{t}"), format!("s{s}"), if addr.is_some() { format!("am{s}({coord})") } else { coord }];
        args.extend(layer);
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
                    2 | 7 => &[0],
                    3 | 8 => &[0, 1],
                    _ => &[0, 1, 2],
                };
                let (dx, dy) = (self.src(&i.ops[4], K::F, n)?, self.src(&i.ops[5], K::F, n)?);
                let (dx, dy) = if n.len() == 1 { (format!("vec2<f32>({dx}, 0.0f)"), format!("vec2<f32>({dy}, 0.0f)")) } else { (dx, dy) };
                ("textureSampleGrad", vec![dx, dy], false)
            }
            0x46 if self.pixel => ("textureSampleCompare", vec![self.src(&i.ops[4], K::F, &[0])?], true),
            0x46 | 0x47 => ("textureSampleCompareLevel", vec![self.src(&i.ops[4], K::F, &[0])?], true),
            op => return Err(format!("sample opcode {op:#x}")),
        };
        args.extend(extra);
        args.extend(off);
        let call = format!("{f}({})", args.join(", "));
        let e = if compare {
            format!("vec4<f32>({call})")
        } else if depth {
            format!("vec4<f32>({call}, 0.0f, 0.0f, 1.0f)")
        } else if let Some(uv) = addr {
            self.addr_used.insert((t, s));
            format!("ab{t}_{s}({call}, {uv})")
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
        // ld2dms: the sample index instead of a level (WebGPU has no multisampled arrays: the
        // page binds those single-sampled, see texture_type)
        if i.op == 0x2e {
            if !matches!(dim, 4 | 9) {
                return Err(format!("ld2dms from resource dimension {dim}"));
            }
            let si = self.src(&i.ops[3], K::I, &[0])?;
            let call = format!("textureLoad(t{t}, {}, {si})", add(self.src(o, K::I, &[0, 1])?, 2));
            let e = if depth { format!("vec4<f32>({call}, 0.0f, 0.0f, 1.0f)") } else { call };
            return self.vec4_result(i, e, if depth { K::F } else { k }, swz);
        }
        let mip = self.src(o, K::I, &[3])?;
        let args = match dim {
            1 => format!("vec2<i32>({}, 0), 0", self.src(o, K::I, &[0])?),
            // (the level is in w for every dimension)
            2 => format!("vec2<i32>({}, 0), {mip}", add(self.src(o, K::I, &[0])?, 1)),
            7 => format!("vec2<i32>({}, 0), {}, {mip}", add(self.src(o, K::I, &[0])?, 1), self.src(o, K::I, &[1])?),
            3 => format!("{}, {mip}", add(self.src(o, K::I, &[0, 1])?, 2)),
            8 => format!("{}, {}, {mip}", add(self.src(o, K::I, &[0, 1])?, 2), self.src(o, K::I, &[2])?),
            5 => format!("{}, {mip}", add(self.src(o, K::I, &[0, 1, 2])?, 3)),
            d => return Err(format!("ld from resource dimension {d}")),
        };
        let call = format!("textureLoad(t{t}, {args})");
        let e = if depth { format!("vec4<f32>({call}, 0.0f, 0.0f, 1.0f)") } else { call };
        self.vec4_result(i, e, if depth { K::F } else { k }, swz)
    }

    /// sampleinfo: the resource's sample count in x (a rasterizer operand: 1, what the shader
    /// cannot know here)
    fn sample_info(&mut self, i: &Inst) -> R<()> {
        let n = if i.ops[1].ty == tpf::RESOURCE {
            let (t, dim, _, _) = self.texture(&i.ops[1])?;
            if !matches!(dim, 4 | 9) {
                return Err(format!("sampleinfo of resource dimension {dim}"));
            }
            format!("textureNumSamples(t{t})")
        } else {
            "1u".into()
        };
        let swz = i.ops[1].swizzle();
        let u = format!("vec4<u32>({n}, 0u, 0u, 0u)");
        if i.ctl & 1 != 0 {
            self.vec4_result(i, u, K::U, swz)
        } else {
            self.vec4_result(i, format!("vec4<f32>({u})"), K::F, swz)
        }
    }

    /// samplepos: where sample n of the resource is from the pixel's center (the standard
    /// patterns WebGPU uses: 1 and 4 samples; others and out of range 0)
    fn sample_pos(&mut self, i: &Inst) -> R<()> {
        let n = if i.ops[1].ty == tpf::RESOURCE {
            let (t, dim, _, _) = self.texture(&i.ops[1])?;
            if !matches!(dim, 4 | 9) {
                return Err(format!("samplepos of resource dimension {dim}"));
            }
            format!("textureNumSamples(t{t})")
        } else {
            "1u".into()
        };
        let si = self.src(&i.ops[2], K::U, &[0])?;
        self.helpers.insert("d3d_samplepos");
        let swz = i.ops[1].swizzle();
        self.vec4_result(i, format!("d3d_samplepos({n}, {si})"), K::F, swz)
    }

    fn resinfo(&mut self, i: &Inst) -> R<()> {
        let (t, dim, _, _) = self.texture(&i.ops[2])?;
        let swz = i.ops[2].swizzle();
        if matches!(dim, 4 | 9) {
            // (a multisampled one: no levels but 1; arrays of it are single layers on the page)
            let dims = format!("textureDimensions(t{t})");
            let u = format!("vec4<u32>({dims}, {}u, 1u)", (dim == 9) as u32);
            let (e, k) = match i.ctl & 3 {
                2 => (u, K::U),
                1 => {
                    let f = format!("vec4<f32>({u})");
                    (format!("(vec4<f32>(1.0f, 1.0f, 1.0f, 0.0f) / {f} + vec4<f32>(0.0f, 0.0f, 0.0f, 1.0f) * {f})"), K::F)
                }
                _ => (format!("vec4<f32>({u})"), K::F),
            };
            return self.vec4_result(i, e, k, swz);
        }
        let mip = self.src(&i.ops[1], K::U, &[0])?;
        let dims = format!("textureDimensions(t{t}, {mip})");
        let levels = format!("textureNumLevels(t{t})");
        let u = match dim {
            2 => format!("vec4<u32>({dims}.x, 0u, 0u, {levels})"),
            7 => format!("vec4<u32>({dims}.x, textureNumLayers(t{t}), 0u, {levels})"),
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
        // (_sat is for float results: not valid on integer, bit or comparison instructions)
        const INT_RESULT: &[u32] = &[
            0x01, 0x18, 0x1b, 0x1c, 0x1d, 0x1e, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x31, 0x39, 0x3b,
            0x3c, 0x4e, 0x4f, 0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x57, 0x82, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e,
        ];
        if i.sat && INT_RESULT.contains(&op) {
            return Err(format!("_sat on integer instruction {op:#x}"));
        }
        // (break and continue: inside a loop; break also in a switch, handled above)
        if matches!(op, 0x02 | 0x03 | 0x07 | 0x08) && !self.blocks.contains(&b'l') && !(matches!(op, 0x02 | 0x03) && self.blocks.contains(&b's')) {
            return Err("break/continue outside a loop".into());
        }
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
                    return Err("else outside an if (or a second one)".into());
                }
                *self.blocks.last_mut().unwrap() = b'e';
                self.depth -= 1;
                self.line("} else {");
                self.depth += 1;
            }
            0x15 | 0x16 => {
                let open = self.blocks.last().copied();
                if !(if op == 0x15 { matches!(open, Some(b'i' | b'e')) } else { open == Some(b'l') }) {
                    return Err(format!("{} without its block", if op == 0x15 { "endif" } else { "endloop" }));
                }
                self.close()
            }
            0x18 => self.cmp(i, F, "==")?,
            0x1d => self.cmp(i, F, ">=")?,
            0x31 => self.cmp(i, F, "<")?,
            // (unordered: true when either is NaN; `!=` on floats is not that on every WebGPU)
            0x39 => self.cmp(i, F, "!==")?,
            0x20 => self.cmp(i, I, "==")?,
            0x21 => self.cmp(i, I, ">=")?,
            0x22 => self.cmp(i, I, "<")?,
            0x27 => self.cmp(i, I, "!=")?,
            0x4f => self.cmp(i, U, "<")?,
            0x50 => self.cmp(i, U, ">=")?,
            0x19 => {
                self.helpers.insert("d3d_exp2");
                self.per_comp(i, &[F], F, |a| format!("d3d_exp2({})", a[0]))?
            }
            0x1a => {
                self.helpers.insert("d3d_frc");
                self.per_comp(i, &[F], F, |a| format!("d3d_frc({})", a[0]))?
            }
            0x1b => {
                self.helpers.insert("d3d_ftoi");
                self.per_comp(i, &[F], I, |a| format!("d3d_ftoi({})", a[0]))?
            }
            0x1c => {
                self.helpers.insert("d3d_ftou");
                self.per_comp(i, &[F], U, |a| format!("d3d_ftou({})", a[0]))?
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
                    // (the divisor never 0 where it is used: naga and Tint reject a constant
                    // division by zero)
                    let one = self.splat(U, n, 1);
                    let v = format!("select({a} {opr} select({t}, {one}, {t} == {z}), {ones}, {t} == {z})");
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
            0x2d | 0x2e => self.load(i)?,
            0x6e => self.sample_pos(i)?,
            0x6f => self.sample_info(i)?,
            0x2f => {
                self.helpers.insert("d3d_log2");
                self.per_comp(i, &[F], F, |a| format!("d3d_log2({})", a[0]))?
            }
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
            0xa3 => self.ld_uav(i)?,
            0xa4 => self.store_uav(i)?,
            0xa5 | 0xa7 => self.ld_tgsm(i)?,
            0xa6 | 0xa8 => self.store_tgsm(i)?,
            0xbe => {
                if self.threads.is_none() {
                    return Err("sync outside a compute shader".into());
                }
                // (a barrier with the group's threads: WGSL's; a memory fence alone has no WGSL
                // statement and orders nothing more than WGSL's own rules here)
                if i.ctl & 1 != 0 {
                    self.line("workgroupBarrier();");
                }
            }
            0x79 => {
                let (t, _, _, _) = self.texture(&i.ops[1])?;
                let swz = i.ops[1].swizzle();
                self.vec4_result(i, format!("vec4<u32>(textureDimensions(t{t}).x)"), U, swz)?;
            }
            0x81 => self.cw(i, F, F, |a| format!("(1.0f / {})", a[0]))?,
            0x82 => {
                self.helpers.insert("d3d_f32tof16");
                self.per_comp(i, &[F], U, |a| format!("d3d_f32tof16({})", a[0]))?
            }
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

    fn uav(&mut self, o: &Operand) -> R<(u32, u32, K)> {
        if o.ty != tpf::UAV {
            return Err(format!("UAV operand type {}", o.ty));
        }
        let n = o.reg();
        let u = self.uavs.get(&n).ok_or(format!("u{n} not declared"))?;
        Ok((n, u.dim, u.ret))
    }

    /// A typed UAV's coordinates (vecN<u32>, with the layer) and how many there are.
    fn uav_coords(&mut self, o: &Operand, dim: u32) -> R<String> {
        Ok(match dim {
            // (1D views are 2D ones of height 1 on the page)
            2 => format!("vec2<u32>({}, 0u)", self.src(o, K::U, &[0])?),
            7 => format!("vec3<u32>({}, 0u, {})", self.src(o, K::U, &[0])?, self.src(o, K::U, &[1])?),
            3 => self.src(o, K::U, &[0, 1])?,
            8 => self.src(o, K::U, &[0, 1, 2])?,
            5 => self.src(o, K::U, &[0, 1, 2])?,
            d => return Err(format!("UAV dimension {d}")),
        })
    }

    /// textureLoad/textureStore arguments: (coordinates, layer) of `c` and the in-bounds test
    /// (Direct3D: out-of-bounds reads give 0, writes do nothing; WGSL may clamp them)
    fn uav_access(&mut self, tex: &str, c: &str, dim: u32) -> (String, String) {
        let t = self.fresh();
        self.line(&format!("let {t} = {c};"));
        match dim {
            7 | 8 => (format!("{t}.xy, {t}.z"), format!("all({t}.xy < textureDimensions({tex})) && {t}.z < textureNumLayers({tex})")),
            5 => (t.clone(), format!("all({t} < textureDimensions({tex}))")),
            _ => (t.clone(), format!("all({t} < textureDimensions({tex}))")),
        }
    }

    fn ld_uav(&mut self, i: &Inst) -> R<()> {
        let (n, dim, k) = self.uav(&i.ops[2])?;
        self.used_ur.insert(n);
        let swz = i.ops[2].swizzle();
        let c = self.uav_coords(&i.ops[1], dim)?;
        let tex = format!("ur{n}");
        let (args, inb) = self.uav_access(&tex, &c, dim);
        let e = format!("select(vec4<{}>(), textureLoad({tex}, {args}, 0), {inb})", k.s());
        self.vec4_result(i, e, k, swz)
    }

    fn store_uav(&mut self, i: &Inst) -> R<()> {
        let (n, dim, k) = self.uav(&i.ops[0])?;
        self.used_u.insert(n);
        let c = self.uav_coords(&i.ops[1], dim)?;
        let v = self.src(&i.ops[2], k, &[0, 1, 2, 3])?;
        let tex = format!("u{n}");
        let (args, inb) = self.uav_access(&tex, &c, dim);
        self.line(&format!("if ({inb}) {{ textureStore({tex}, {args}, {v}); }}"));
        Ok(())
    }

    /// The dword index in groupshared memory of ld/store_raw (byte address) or
    /// ld/store_structured (index, byte offset), and the memory's name and size.
    fn tgsm_addr(&mut self, i: &Inst, structured: bool, at: usize) -> R<(String, String, u32)> {
        let g = &i.ops[if structured { at + 2 } else { at + 1 }];
        if g.ty != tpf::TGSM {
            return Err(format!("raw/structured access to register type {} (only groupshared memory is read so)", g.ty));
        }
        let n = g.reg();
        let (dwords, stride) = *self.tgsm.get(&n).ok_or(format!("g{n} not declared"))?;
        let a = if structured {
            let idx = self.src(&i.ops[at], K::U, &[0])?;
            let off = self.src(&i.ops[at + 1], K::U, &[0])?;
            format!("(({idx}) * {}u + ({off} >> 2u))", stride / 4)
        } else {
            format!("({} >> 2u)", self.src(&i.ops[at], K::U, &[0])?)
        };
        let t = self.fresh();
        self.line(&format!("let {t} = {a};"));
        Ok((format!("g{n}"), t, dwords))
    }

    fn ld_tgsm(&mut self, i: &Inst) -> R<()> {
        let structured = i.op == 0xa7;
        let (g, a, dwords) = self.tgsm_addr(i, structured, 1)?;
        let swz = i.ops[if structured { 3 } else { 2 }].swizzle();
        // (out of range: 0, as Direct3D reads past the memory)
        let word = |c: u8| format!("select(0u, {g}[min({a} + {c}u, {}u)], {a} + {c}u < {dwords}u)", dwords - 1);
        let e = format!("vec4<u32>({}, {}, {}, {})", word(0), word(1), word(2), word(3));
        self.vec4_result(i, e, K::U, swz)
    }

    fn store_tgsm(&mut self, i: &Inst) -> R<()> {
        let structured = i.op == 0xa8;
        let d = &i.ops[0];
        let comps = bits(d.mask() as u32);
        let (g, a, dwords) = {
            let g = d.reg();
            if d.ty != tpf::TGSM {
                return Err(format!("raw/structured store to register type {} (only groupshared memory is written so)", d.ty));
            }
            let (dwords, stride) = *self.tgsm.get(&g).ok_or(format!("g{g} not declared"))?;
            let a = if structured {
                let idx = self.src(&i.ops[1], K::U, &[0])?;
                let off = self.src(&i.ops[2], K::U, &[0])?;
                format!("(({idx}) * {}u + ({off} >> 2u))", stride / 4)
            } else {
                format!("({} >> 2u)", self.src(&i.ops[1], K::U, &[0])?)
            };
            let t = self.fresh();
            self.line(&format!("let {t} = {a};"));
            (format!("g{g}"), t, dwords)
        };
        let vo = &i.ops[if structured { 3 } else { 2 }];
        let v = self.src(vo, K::U, &comps)?;
        let t = self.fresh();
        self.line(&format!("let {t} = {v};"));
        // (component c is the dword at address + 4c; out of range: not written)
        for (k, &c) in comps.iter().enumerate() {
            let val = if comps.len() == 1 { t.clone() } else { format!("{t}.{}", XYZW[k]) };
            self.line(&format!("if ({a} + {c}u < {dwords}u) {{ {g}[{a} + {c}u] = {val}; }}"));
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
    // (1D textures and arrays are 2D ones of height 1 on the page: WebGPU's 1D textures have no
    // levels or layers)
    let vd = match dim {
        // (Texture2DMS and Texture2DMSArray: multisampled 2D, view dimension 6)
        4 | 9 => 6,
        1..=3 => 1,
        7 => 2,
        5 => 5,
        6 => 3,
        8 => 2,
        10 => 4,
        d => return Err(format!("resource dimension {d}")),
    };
    if vd == 6 {
        return Ok((if depth { "texture_depth_multisampled_2d".into() } else { format!("texture_multisampled_2d<{}>", k.s()) }, vd));
    }
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
    translate_with(code, pixel, ps, None, &[])
}

/// [`translate`] for samplers with BORDER / MIRROR_ONCE addressing (`addr`, by sampler
/// register): what they sample at and the border (crate::addr). For 2D textures and arrays
/// sampled without offsets or comparison; the rest samples as the sampler does (clamped).
pub fn translate_addr(code: &[u8], pixel: bool, ps: Option<&[u8]>, addr: &[crate::addr::Addr]) -> R<Wgsl> {
    translate_with(code, pixel, ps, None, addr)
}

/// Point sprites (what ANGLE's point sprite geometry shader does, for a page without geometry
/// shaders): the vertex shader runs once per point as an instance and six times per instance,
/// once per corner of two triangles, moving its position by the corner times the point size
/// output (PSIZE, clamped to 1..`max_size`) in pixels. The viewport's half size comes in a
/// uniform at binding 15 of group 0 (xy).
pub fn translate_point_sprite(code: &[u8], ps: Option<&[u8]>, max_size: f32) -> R<Wgsl> {
    translate_with(code, false, ps, Some(max_size), &[])
}

/// The typed UAVs a compute shader declares: (slot, dimension as dcl_uav_typed has it (2 1D,
/// 3 2D, 5 3D, 7 1D array, 8 2D array), return type (1 unorm, 2 snorm, 3 sint, 4 uint, 5
/// float)).
pub fn uav_slots(code: &[u8]) -> R<Vec<(u32, u32, u32)>> {
    let (_, p) = parse(code)?;
    Ok(p.insts.iter().filter(|i| i.op == 0x9c && !i.ops.is_empty()).map(|i| (i.ops[0].reg(), i.ctl & 0x1f, i.extra.first().map_or(5, |r| r & 0xf))).collect())
}

/// A compute shader (cs_4_x/cs_5_0 DXBC) as a WGSL compute entry point `main`. Group 0:
/// constant buffer b<n> at binding n, t<n> 16 + n, s<n> 160 + n, typed UAV u<n> 200 + n as a
/// write-only storage texture of `uav_formats` (slot, WebGPU format; a UAV left out: by its
/// return type, rgba32float/uint/sint), and, for UAVs the shader reads, 300 + n: a copy of
/// u<n> taken before the dispatch (texture_2d.., read with textureLoad). Groupshared memory is
/// var<workgroup>.
pub fn translate_compute(code: &[u8], uav_formats: &[(u32, &str)]) -> R<Wgsl> {
    let (c, p) = parse(code)?;
    if p.kind != tpf::COMPUTE {
        return Err(format!("program type {} for a compute shader", p.kind));
    }
    let mut w = W::new(false, c.signature(false), c.signature(true));
    w.split = true;
    for &(n, f) in uav_formats {
        w.uav_formats.insert(n, f.to_string());
    }
    declarations(&mut w, &p)?;
    if !w.inputs.is_empty() || !w.outputs.is_empty() {
        return Err("a compute shader with input/output registers".into());
    }
    for i in &p.insts {
        if matches!(i.op, 0x0b | 0x0c | 0x7a..=0x7d | 0x0d | 0x45 | 0x4a | 0x46 | 0x7e..=0x80) && i.op != 0x7e {
            // (no derivatives in a compute shader: implicit-level sampling and discard neither)
            if !matches!(i.op, 0x7e | 0x7f) {
                return Err(format!("instruction {:#x} in a compute shader", i.op));
            }
        }
        w.inst(i)?;
    }
    if !w.blocks.is_empty() {
        return Err("unclosed block".into());
    }
    let mut m = String::new();
    let mut bindings = Vec::new();
    let mut temps = String::new();
    module_decls(&mut w, &mut m, &mut bindings, 0, Some(&mut temps))?;
    for &n in &w.used_u {
        let u = &w.uavs[&n];
        let (ty, vd) = storage_type(u.dim)?;
        let fmt = w.uav_formats.get(&n).cloned().unwrap_or_else(|| match u.ret {
            K::F => "rgba32float".into(),
            K::U => "rgba32uint".into(),
            K::I => "rgba32sint".into(),
        });
        if storage_kind(&fmt) != Some(u.ret) {
            return Err(format!("u{n}: {fmt} views are not {} UAVs", u.ret.s()));
        }
        let _ = writeln!(m, "@group(0) @binding({}) var u{n}: {ty}<{fmt}, write>;", 200 + n);
        bindings.push((200 + n, Binding::Storage(vd, fmt)));
    }
    for &n in &w.used_ur {
        let u = &w.uavs[&n];
        let (ty, vd) = texture_type(u.dim, u.ret, false)?;
        let _ = writeln!(m, "@group(0) @binding({}) var ur{n}: {ty};", 300 + n);
        let st = match u.ret {
            K::U => 1,
            K::I => 2,
            K::F => 0,
        };
        bindings.push((300 + n, Binding::Texture(vd, st)));
    }
    for (g, &(dwords, _)) in &w.tgsm {
        let _ = writeln!(m, "var<workgroup> g{g}: array<u32, {dwords}>;");
    }
    for (ty, name) in [(32, "vtid"), (33, "vgid"), (34, "vltid"), (35, "vlidx")] {
        if w.sys_in.contains(&ty) {
            let _ = writeln!(m, "var<private> {name}: vec4<u32>;");
        }
    }
    let _ = write!(m, "\nfn shader_main() {{\n{temps}{}}}\n\n", w.body);
    let [x, y, z] = w.threads.unwrap();
    let _ = writeln!(
        m,
        "@compute @workgroup_size({x}, {y}, {z})\nfn main(@builtin(global_invocation_id) in_tid: vec3<u32>, @builtin(workgroup_id) in_gid: vec3<u32>, @builtin(local_invocation_id) in_ltid: vec3<u32>, @builtin(local_invocation_index) in_lidx: u32) {{"
    );
    for (ty, s) in [(32, "vtid = vec4<u32>(in_tid, 0u);"), (33, "vgid = vec4<u32>(in_gid, 0u);"), (34, "vltid = vec4<u32>(in_ltid, 0u);"), (35, "vlidx = vec4<u32>(in_lidx);")] {
        if w.sys_in.contains(&ty) {
            let _ = writeln!(m, "    {s}");
        }
    }
    m.push_str("    shader_main();\n}\n");
    Ok(Wgsl { code: m, bindings })
}

/// A typed UAV's storage texture type (WGSL, without the format) and view dimension.
fn storage_type(dim: u32) -> R<(&'static str, u32)> {
    Ok(match dim {
        2 | 3 => ("texture_storage_2d", 1),
        7 | 8 => ("texture_storage_2d_array", 2),
        5 => ("texture_storage_3d", 5),
        d => return Err(format!("UAV dimension {d}")),
    })
}

/// The channel type of a WebGPU storage texture format.
fn storage_kind(f: &str) -> Option<K> {
    Some(match f {
        "rgba8unorm" | "rgba8snorm" | "bgra8unorm" | "rgba16float" | "r32float" | "rg32float" | "rgba32float" | "r8unorm" | "rg8unorm" | "r16float" | "rg16float"
        | "rgb10a2unorm" | "rg11b10ufloat" | "r16unorm" | "rg16unorm" | "rgba16unorm" | "r8snorm" | "rg8snorm" | "r16snorm" | "rg16snorm" | "rgba16snorm" => K::F,
        "rgba8uint" | "rgba16uint" | "r32uint" | "rg32uint" | "rgba32uint" | "r8uint" | "rg8uint" | "r16uint" | "rg16uint" | "rgb10a2uint" => K::U,
        "rgba8sint" | "rgba16sint" | "r32sint" | "rg32sint" | "rgba32sint" | "r8sint" | "rg8sint" | "r16sint" | "rg16sint" => K::I,
        _ => return None,
    })
}

/// The module's declarations of what the body uses: bindings, registers, helpers, the
/// immediate constant buffer. `local`: the temporary registers are shader_main's own variables
/// (`temps`), not the module's: WGSL's uniformity analysis follows a function's variables
/// through assignments, not module ones (a barrier in a loop counted in a register).
fn module_decls(w: &mut W, m: &mut String, bindings: &mut Vec<(u32, Binding)>, group: u32, temps: Option<&mut String>) -> R<()> {
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
    let (local, out) = match temps {
        Some(t) => ("    var", t),
        None => ("var<private>", &mut *m),
    };
    for r in 0..w.temps {
        if w.split {
            let _ = writeln!(out, "{local} r{r}_x: u32; {local} r{r}_y: u32; {local} r{r}_z: u32; {local} r{r}_w: u32;");
        } else {
            let _ = writeln!(out, "{local} r{r}: vec4<u32>;");
        }
    }
    for (x, n) in &w.xs {
        let _ = writeln!(out, "{local} x{x}: array<vec4<u32>, {n}>;");
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
    for &(name, code) in HELPERS {
        if w.helpers.contains(name) {
            m.push_str(code);
        }
    }
    for x in &w.addr {
        if w.addr_used.iter().any(|&(_, s)| s == x.sampler) {
            m.push_str(&crate::addr::coord_fn(x));
        }
        for &(t, _) in w.addr_used.iter().filter(|&&(_, s)| s == x.sampler) {
            m.push_str(&crate::addr::border_fn(x, &format!("t{t}"), &format!("ab{t}_{}", x.sampler)));
        }
    }
    if !w.icb.is_empty() {
        let n = w.icb.len() / 4;
        let items: Vec<String> = w.icb.chunks_exact(4).map(|c| format!("vec4<u32>({}u, {}u, {}u, {}u)", c[0], c[1], c[2], c[3])).collect();
        let _ = writeln!(m, "var<private> icb: array<vec4<u32>, {n}> = array<vec4<u32>, {n}>({});", items.join(", "));
    }
    Ok(())
}

fn translate_with(code: &[u8], pixel: bool, ps: Option<&[u8]>, sprite: Option<f32>, addr: &[crate::addr::Addr]) -> R<Wgsl> {
    let (c, p) = parse(code)?;
    if p.kind != if pixel { tpf::PIXEL } else { tpf::VERTEX } {
        return Err(format!("program type {} for a {} shader", p.kind, if pixel { "pixel" } else { "vertex" }));
    }
    let mut w = W::new(pixel, c.signature(false), c.signature(true));
    w.addr = addr.to_vec();
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
    module_decls(&mut w, &mut m, &mut bindings, group, None)?;
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
                // (point sprites: the point is the instance, and draws are not instanced)
                6 if sprite.is_some() => {
                    params.push("@builtin(instance_index) in_vid: u32".into());
                    pre.push(assign(format!("v{r}"), d.mask, "vec4<u32>(in_vid)".into()));
                }
                8 if sprite.is_some() => pre.push(assign(format!("v{r}"), d.mask, "vec4<u32>(0u)".into())),
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
        if let Some(max) = sprite {
            params.push("@builtin(vertex_index) sp_vid: u32".into());
            let _ = writeln!(m, "@group(0) @binding(15) var<uniform> sp_view: vec4<f32>;");
            bindings.push((15, Binding::Uniform(16)));
            let size = w
                .osgn
                .iter()
                .find(|e| e.sysval == 0 && e.name.eq_ignore_ascii_case("PSIZE") && w.outputs.contains_key(&e.reg))
                .map_or("1.0f".to_string(), |e| format!("bitcast<f32>(o{}.{})", e.reg, XYZW[bits(e.mask)[0] as usize]));
            // (the geometry shader's strip 0 1 2 3 as triangles 0 1 2, 2 1 3)
            post.push("var sp_c = array<vec2<f32>, 6>(vec2<f32>(0.5f, -0.5f), vec2<f32>(0.5f, 0.5f), vec2<f32>(-0.5f, -0.5f), vec2<f32>(-0.5f, -0.5f), vec2<f32>(0.5f, 0.5f), vec2<f32>(-0.5f, 0.5f));".into());
            post.push(format!("let sp_s = clamp({size}, 1.0f, {max:?}f);"));
            post.push("out.pos = out.pos + vec4<f32>(sp_c[sp_vid % 6u] * sp_s * out.pos.w / sp_view.xy, 0.0f, 0.0f);".into());
        }
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
mod icb_tests {
    /// ps_4_0: dcl_output o0.xyzw; [dcl_immediateConstantBuffer of `icb` vec4s]; mov o0, icb[k]; ret
    fn ps(icb: Option<&[[u32; 4]]>, k: u32) -> Vec<u8> {
        let mut t = vec![0x0000_0040, 0, 0x0300_0065, 0x0010_20f2, 0];
        if let Some(v) = icb {
            t.extend([0x35 | 3 << 11, 2 + 4 * v.len() as u32]);
            t.extend(v.iter().flatten());
        }
        t.extend([0x0500_0036, 0x0010_20f2, 0, 0x0010_9e46, k, 0x0100_003e]);
        t[1] = t.len() as u32;
        let shdr: Vec<u8> = t.iter().flat_map(|x| x.to_le_bytes()).collect();
        // OSGN: SV_Target0, float4, o0, mask xyzw
        let mut osgn = Vec::new();
        for x in [1u32, 8, 32, 0, 64, 3, 0, 0xf] {
            osgn.extend(x.to_le_bytes());
        }
        osgn.extend(b"SV_Target\0\0\0");
        let chunks: [(&[u8; 4], &[u8]); 2] = [(b"OSGN", &osgn), (b"SHDR", &shdr)];
        let mut d = b"DXBC".to_vec();
        d.extend([0; 16]);
        d.extend(1u32.to_le_bytes());
        d.extend(0u32.to_le_bytes()); // total size, set below
        d.extend((chunks.len() as u32).to_le_bytes());
        let mut at = d.len() + 4 * chunks.len();
        for (_, c) in chunks {
            d.extend((at as u32).to_le_bytes());
            at += 8 + c.len();
        }
        for (tag, c) in chunks {
            d.extend(*tag);
            d.extend((c.len() as u32).to_le_bytes());
            d.extend(c);
        }
        let n = d.len() as u32;
        d[24..28].copy_from_slice(&n.to_le_bytes());
        d
    }

    fn valid(w: &str) {
        let m = naga::front::wgsl::parse_str(w).unwrap_or_else(|e| panic!("{}\n{w}", e.emit_to_string(w)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&m)
            .unwrap_or_else(|e| panic!("{e:?}\n{w}"));
    }

    #[test]
    fn icb_reads_its_declaration() {
        let v = [[1, 2, 3, 4], [5, 6, 7, 8]];
        for k in 0..2 {
            let w = crate::translate(&ps(Some(&v), k), true, None).expect("declared icb");
            assert!(w.code.contains(&format!("icb[{k}")), "{}", w.code);
            valid(&w.code);
        }
    }

    #[test]
    fn icb_without_a_declaration_is_rejected() {
        // the fuzz's input read icb[3] with no dcl_immediateConstantBuffer: WGSL naming an
        // undefined icb went to WebGPU
        let e = crate::translate(&ps(None, 3), true, None).err().expect("rejected");
        assert_eq!(e, "icb is not declared");
        let e = crate::translate(&ps(Some(&[]), 0), true, None).err().expect("rejected");
        assert_eq!(e, "icb is not declared");
        let e = crate::translate(&ps(Some(&[[1, 2, 3, 4]]), 1), true, None).err().expect("rejected");
        assert!(e.contains("past its size"), "{e}");
    }
}

#[cfg(test)]
mod tests {
    /// Every DXBC file in $IPPAI_DXBC_DIR (shaders taken from games) translates to WGSL that naga
    /// accepts. Pixel or vertex comes from the program type; each vertex shader is linked to
    /// every pixel shader in the directory.
    #[test]
    fn corpus() {
        let Ok(dir) = std::env::var("IPPAI_DXBC_DIR") else { return };
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
                        if std::env::var_os("IPPAI_DXBC_PRINT").is_some() {
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

/// Direct3D's results where WGSL's built-ins leave them to the implementation (D3D11.3
/// functional spec; checked on WebGPU by tests/shaders.mjs). Infinities and NaN come from bit
/// patterns of the argument (WGSL has no such constants).
const HELPERS: &[(&str, &str)] = &[
    // log: log(+-0) = -inf (denormals are 0, as WARP flushes them), log(x < 0) = NaN, NaN stays
    ("d3d_log2", "fn d3d_log2(x: f32) -> f32 {
  let b = bitcast<u32>(x);
  if ((b & 0x7fffffffu) > 0x7f800000u) { return x; }
  if ((b & 0x7f800000u) == 0u) { return bitcast<f32>((b & 0x80000000u) | 0xff800000u); }
  if (b > 0x80000000u) { return bitcast<f32>(b | 0x7fc00000u); }
  return log2(x);
}
"),
    // exp: NaN stays NaN
    ("d3d_exp2", "fn d3d_exp2(x: f32) -> f32 {
  if ((bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u) { return x; }
  return exp2(x);
}
"),
    // frc: below 1 (-tiny gives 0x3f7fffff), infinities and NaN give NaN
    // samplepos: the standard 4x pattern (D3D11 and WebGPU's), 1x the center
    ("d3d_samplepos", "fn d3d_samplepos(n: u32, i: u32) -> vec4<f32> {
  if (n == 4u && i < 4u) {
    var p = array<vec2<f32>, 4>(vec2<f32>(-0.125f, -0.375f), vec2<f32>(0.375f, -0.125f), vec2<f32>(-0.375f, 0.125f), vec2<f32>(0.125f, 0.375f));
    return vec4<f32>(p[i], 0.0f, 0.0f);
  }
  return vec4<f32>(0.0f);
}"),
    ("d3d_frc", "fn d3d_frc(x: f32) -> f32 {
  let b = bitcast<u32>(x);
  if ((b & 0x7fffffffu) >= 0x7f800000u) { return bitcast<f32>((b & 0x7f800000u) | 0x00400000u); }
  return min(x - floor(x), bitcast<f32>(0x3f7fffffu));
}
"),
    // ftoi: toward zero, NaN 0, clamped to the int range (WGSL clamps to 2^31 - 128)
    ("d3d_ftoi", "fn d3d_ftoi(x: f32) -> i32 {
  if ((bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u) { return 0; }
  if (x >= 2147483648.0f) { return 2147483647; }
  return i32(x);
}
"),
    // ftou: NaN and negatives 0, clamped (WGSL clamps to 2^32 - 256)
    ("d3d_ftou", "fn d3d_ftou(x: f32) -> u32 {
  if ((bitcast<u32>(x) & 0x7fffffffu) > 0x7f800000u) { return 0u; }
  if (x >= 4294967296.0f) { return 0xffffffffu; }
  return u32(x);
}
"),
    // f32tof16 as Direct3D's reference rasterizer (WARP) converts: toward zero, magnitudes past
    // 65504 to 65504 (infinities stay), NaNs all 0x7fff (with the sign), float denormals 0
    ("d3d_f32tof16", "fn d3d_f32tof16(x: f32) -> u32 {
  let b = bitcast<u32>(x);
  let s = (b >> 16u) & 0x8000u;
  let a = b & 0x7fffffffu;
  if (a > 0x7f800000u) { return s | 0x7fffu; }
  if (a == 0x7f800000u) { return s | 0x7c00u; }
  if (a >= 0x477fe000u) { return s | 0x7bffu; }
  if (a >= 0x38800000u) { return s | ((a - 0x38000000u) >> 13u); }
  if (a < 0x00800000u) { return s; }
  return s | u32(bitcast<f32>(a) * 16777216.0f);
}
"),
];
