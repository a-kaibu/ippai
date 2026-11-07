//! Shader model 4/5 code: instructions on virtual temporaries, their register allocation, the
//! token encoding and the DXBC container (RDEF, ISGN, OSGN, SHDR/SHEX).

/// A register file and index. Temps are virtual until `finish`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum RegF {
    Temp(u32),
    Input(u32),
    Output(u32),
    /// constant buffer (symbol, register)
    Cb(u32, u32),
    Icb(u32),
    /// indexable temporary (array, register)
    X(u32, u32),
    Depth,
    Null,
    /// compute shader inputs without an index: vThreadID (32), vThreadGroupID (33),
    /// vThreadIDInGroup (34), vThreadIDInGroupFlattened (35)
    Sys(u32),
    /// a typed unordered access view (symbol): store_uav_typed's destination
    Uav(u32),
    /// thread group shared memory (symbol): store_raw's destination
    G(u32),
}

/// A register operand with an optional relative index (a temp's component) added to the last
/// index.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Reg {
    pub f: RegF,
    pub rel: Option<(u32, u8)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Src {
    R { r: Reg, swz: [u8; 4], neg: bool, abs: bool, one: bool },
    Imm([u32; 4], bool),
    /// texture t (symbol), with a swizzle of its result
    Tex(u32, [u8; 4]),
    Samp(u32),
    /// a sampler with a component select (gather4: the channel gathered)
    SampC(u32, u8),
    /// a typed UAV (symbol) with a swizzle of its result (ld_uav_typed)
    Uav(u32, [u8; 4]),
    /// thread group shared memory (symbol) with a swizzle: the dwords ld_raw reads
    G(u32, [u8; 4]),
}

#[derive(Clone, Debug)]
pub struct Dst {
    pub r: Reg,
    pub mask: u8,
}

#[derive(Clone, Debug)]
pub struct Ins {
    pub op: u32,
    pub sat: bool,
    pub nz: bool,
    /// opcode-specific control bits (11..23)
    pub ctl: u32,
    pub dst: Vec<Dst>,
    pub src: Vec<Src>,
    pub offset: [i32; 3],
}

// opcodes
pub const ADD: u32 = 0x00;
pub const AND: u32 = 0x01;
pub const BREAK: u32 = 0x02;
pub const BREAKC: u32 = 0x03;
pub const CASE: u32 = 0x06;
pub const CONTINUE: u32 = 0x07;
pub const DEFAULT: u32 = 0x0a;
pub const DERIV_RTX: u32 = 0x0b;
pub const DERIV_RTY: u32 = 0x0c;
pub const DISCARD: u32 = 0x0d;
pub const DIV: u32 = 0x0e;
pub const DP2: u32 = 0x0f;
pub const DP3: u32 = 0x10;
pub const DP4: u32 = 0x11;
pub const ELSE: u32 = 0x12;
pub const ENDIF: u32 = 0x15;
pub const ENDLOOP: u32 = 0x16;
pub const ENDSWITCH: u32 = 0x17;
pub const EQ: u32 = 0x18;
pub const EXP: u32 = 0x19;
pub const FRC: u32 = 0x1a;
pub const FTOI: u32 = 0x1b;
pub const FTOU: u32 = 0x1c;
pub const GE: u32 = 0x1d;
pub const IADD: u32 = 0x1e;
pub const IF: u32 = 0x1f;
pub const IEQ: u32 = 0x20;
pub const IGE: u32 = 0x21;
pub const ILT: u32 = 0x22;
pub const IMAD: u32 = 0x23;
pub const IMAX: u32 = 0x24;
pub const IMIN: u32 = 0x25;
pub const IMUL: u32 = 0x26;
pub const INE: u32 = 0x27;
pub const INEG: u32 = 0x28;
pub const ISHL: u32 = 0x29;
pub const ISHR: u32 = 0x2a;
pub const ITOF: u32 = 0x2b;
pub const LD: u32 = 0x2d;
pub const LD_MS: u32 = 0x2e;
pub const LOG: u32 = 0x2f;
pub const LOOP: u32 = 0x30;
pub const LT: u32 = 0x31;
pub const MAD: u32 = 0x32;
pub const MIN: u32 = 0x33;
pub const MAX: u32 = 0x34;
pub const MOV: u32 = 0x36;
pub const MOVC: u32 = 0x37;
pub const MUL: u32 = 0x38;
pub const NE: u32 = 0x39;
pub const NOT: u32 = 0x3b;
pub const OR: u32 = 0x3c;
pub const RESINFO: u32 = 0x3d;
pub const SAMPLE_POS: u32 = 0x6e;
pub const SAMPLE_INFO: u32 = 0x6f;
pub const RET: u32 = 0x3e;
pub const ROUND_NE: u32 = 0x40;
pub const ROUND_NI: u32 = 0x41;
pub const ROUND_PI: u32 = 0x42;
pub const ROUND_Z: u32 = 0x43;
pub const RSQ: u32 = 0x44;
pub const SAMPLE: u32 = 0x45;
pub const SAMPLE_C: u32 = 0x46;
pub const SAMPLE_C_LZ: u32 = 0x47;
pub const SAMPLE_L: u32 = 0x48;
pub const SAMPLE_D: u32 = 0x49;
pub const SAMPLE_B: u32 = 0x4a;
pub const SQRT: u32 = 0x4b;
pub const SWITCH: u32 = 0x4c;
pub const SINCOS: u32 = 0x4d;
pub const UDIV: u32 = 0x4e;
pub const ULT: u32 = 0x4f;
pub const UGE: u32 = 0x50;
pub const UMUL: u32 = 0x51;
pub const UMAX: u32 = 0x53;
pub const UMIN: u32 = 0x54;
pub const USHR: u32 = 0x55;
pub const UTOF: u32 = 0x56;
pub const XOR: u32 = 0x57;
pub const GATHER4: u32 = 0x6d;
pub const RCP: u32 = 0x81;
pub const F32TOF16: u32 = 0x82;
pub const F16TOF32: u32 = 0x83;
pub const COUNTBITS: u32 = 0x86;
pub const FIRSTBIT_HI: u32 = 0x87;
pub const FIRSTBIT_LO: u32 = 0x88;
pub const FIRSTBIT_SHI: u32 = 0x89;
pub const BFREV: u32 = 0x8d;
pub const LD_UAV_TYPED: u32 = 0xa3;
pub const STORE_UAV_TYPED: u32 = 0xa4;
pub const LD_RAW: u32 = 0xa5;
pub const STORE_RAW: u32 = 0xa6;
/// sync: ctl 1 threads in the group, 2 shared memory, 4 UAVs (group), 8 UAVs (global)
pub const SYNC: u32 = 0xbe;

/// A signature element of the shader's interface.
#[derive(Clone, Debug)]
pub struct SigEl {
    pub name: String,
    pub index: u32,
    /// D3D_NAME (0 for SV_Target and generic ones)
    pub sysval: u32,
    /// 1 uint, 2 sint, 3 float
    pub ctype: u32,
    pub reg: u32,
    pub mask: u8,
    /// pixel inputs: interpolation mode (1 constant, 2 linear, 3 centroid, 4 noperspective..)
    pub interp: u32,
    /// the register is oDepth
    pub depth: bool,
}

pub struct Tex {
    pub name: String,
    /// resource dimension (1 buffer, 2 1d, 3 2d, 4 2dms, 5 3d, 6 cube, 7 1darray, 8 2darray, 9
    /// 2dmsarray, 10 cubearray)
    pub dim: u32,
    /// 3 sint, 4 uint, 5 float
    pub ret: u32,
    pub comps: u32,
    pub slot: u32,
}

/// A typed unordered access view (RWTexture*)
pub struct Uav {
    pub name: String,
    /// resource dimension as `Tex::dim`
    pub dim: u32,
    /// 3 sint, 4 uint, 5 float (1 unorm, 2 snorm)
    pub ret: u32,
    pub comps: u32,
    pub slot: u32,
}

pub struct Samp {
    pub name: String,
    pub cmp: bool,
    pub slot: u32,
}

/// A constant buffer variable (RDEF)
#[derive(Clone)]
pub struct CbVar {
    pub name: String,
    pub offset: u32,
    pub size: u32,
    /// D3D_SHADER_VARIABLE_CLASS: 0 scalar, 1 vector, 2 matrix rows, 3 matrix columns, 5 struct
    pub class: u16,
    /// D3D_SHADER_VARIABLE_TYPE: 1 bool, 2 int, 3 float, 19 uint
    pub ty: u16,
    pub rows: u16,
    pub cols: u16,
    pub elems: u16,
    pub used: bool,
    /// register(cN) given (shader model 2/3)
    pub explicit: bool,
    /// members of a struct (offsets from the struct's start)
    pub members: Vec<CbVar>,
}

pub struct Cb {
    pub name: String,
    pub slot: u32,
    /// in registers
    pub size: u32,
    pub dynamic: bool,
    pub vars: Vec<CbVar>,
}

pub struct Shader {
    pub pixel: bool,
    pub major: u32,
    pub minor: u32,
    pub code: Vec<Ins>,
    pub inputs: Vec<SigEl>,
    pub outputs: Vec<SigEl>,
    pub cbs: Vec<Cb>,
    pub texs: Vec<Tex>,
    pub samps: Vec<Samp>,
    /// indexable temps: registers of each
    pub xs: Vec<u32>,
    pub icb: Vec<[u32; 4]>,
    /// (virtual temp, instruction index): a variable's declaration point (live range start)
    pub marks: Vec<(u32, usize)>,
    /// cb symbol -> index into `cbs` (only the used ones are in cbs)
    pub cb_map: Vec<Option<usize>>,
    pub tex_map: Vec<Option<usize>>,
    pub samp_map: Vec<Option<usize>>,
    pub early_depth: bool,
    /// compute shaders: [numthreads(x, y, z)]
    pub threads: Option<[u32; 3]>,
    /// compute inputs read: (register type, mask)
    pub sys_inputs: Vec<(u32, u8)>,
    pub uavs: Vec<Uav>,
    pub uav_map: Vec<Option<usize>>,
    /// thread group shared memory: bytes of each g register
    pub tgsm: Vec<u32>,
}

// ---- register allocation ----

fn temps_of(i: &Ins, f: &mut dyn FnMut(u32)) {
    let reg = |r: &Reg, f: &mut dyn FnMut(u32)| {
        if let RegF::Temp(t) = r.f {
            f(t);
        }
        if let Some((t, _)) = r.rel {
            f(t);
        }
    };
    for d in &i.dst {
        reg(&d.r, f);
    }
    for s in &i.src {
        if let Src::R { r, .. } = s {
            reg(r, f);
        }
    }
}

/// Virtual temps to registers (live ranges over the linear code, extended over loops they cross);
/// the number of registers.
pub(super) fn alloc(code: &mut [Ins], marks: &[(u32, usize)]) -> u32 {
    use std::collections::HashMap;
    let mut iv: HashMap<u32, (usize, usize)> = HashMap::new();
    let note = |t: u32, at: usize, iv: &mut HashMap<u32, (usize, usize)>| {
        let e = iv.entry(t).or_insert((at, at));
        e.0 = e.0.min(at);
        e.1 = e.1.max(at);
    };
    for (n, i) in code.iter().enumerate() {
        temps_of(i, &mut |t| note(t, n, &mut iv));
    }
    for &(t, at) in marks {
        if iv.contains_key(&t) {
            note(t, at, &mut iv);
        }
    }
    // loops
    let mut loops = Vec::new();
    let mut stack = Vec::new();
    for (n, i) in code.iter().enumerate() {
        if i.op == LOOP {
            stack.push(n);
        } else if i.op == ENDLOOP {
            if let Some(s) = stack.pop() {
                loops.push((s, n));
            }
        }
    }
    loop {
        let mut changed = false;
        for v in iv.values_mut() {
            for &(s, e) in &loops {
                let overlaps = v.0 <= e && v.1 >= s;
                let inside = v.0 >= s && v.1 <= e;
                if overlaps && !inside && (v.0 > s || v.1 < e) {
                    v.0 = v.0.min(s);
                    v.1 = v.1.max(e);
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    let mut order: Vec<(u32, usize, usize)> = iv.iter().map(|(&t, &(a, b))| (t, a, b)).collect();
    order.sort_by_key(|x| (x.1, x.0));
    let mut map: HashMap<u32, u32> = HashMap::new();
    let mut active: Vec<(usize, u32)> = Vec::new();
    let mut free: Vec<u32> = Vec::new();
    let mut n = 0;
    for (t, a, b) in order {
        active.retain(|&(end, r)| {
            if end < a {
                free.push(r);
                false
            } else {
                true
            }
        });
        free.sort_unstable_by(|x, y| y.cmp(x));
        let r = free.pop().unwrap_or_else(|| {
            n += 1;
            n - 1
        });
        active.push((b, r));
        map.insert(t, r);
    }
    let fix = |r: &mut Reg| {
        if let RegF::Temp(t) = r.f {
            r.f = RegF::Temp(map[&t]);
        }
        if let Some((t, c)) = r.rel {
            r.rel = Some((map[&t], c));
        }
    };
    for i in code.iter_mut() {
        for d in &mut i.dst {
            fix(&mut d.r);
        }
        for s in &mut i.src {
            if let Src::R { r, .. } = s {
                fix(r);
            }
        }
    }
    n
}

// ---- encoding ----

/// operand types
const T_TEMP: u32 = 0;
const T_INPUT: u32 = 1;
const T_OUTPUT: u32 = 2;
const T_X: u32 = 3;
const T_IMM32: u32 = 4;
const T_SAMPLER: u32 = 6;
const T_RESOURCE: u32 = 7;
const T_CB: u32 = 8;
const T_ICB: u32 = 9;
const T_DEPTH: u32 = 12;
const T_NULL: u32 = 13;
const T_UAV: u32 = 30;
const T_TGSM: u32 = 31;

struct Enc<'a> {
    s: &'a Shader,
}

impl Enc<'_> {
    /// operand token pieces: (type, indices)
    fn reg_parts(&self, r: &Reg) -> (u32, Vec<u32>) {
        match r.f {
            RegF::Temp(i) => (T_TEMP, vec![i]),
            RegF::Input(i) => (T_INPUT, vec![i]),
            RegF::Output(i) => (T_OUTPUT, vec![i]),
            RegF::Cb(s, i) => (T_CB, vec![self.s.cbs[self.s.cb_map[s as usize].unwrap()].slot, i]),
            RegF::Icb(i) => (T_ICB, vec![i]),
            RegF::X(a, i) => (T_X, vec![a, i]),
            RegF::Depth => (T_DEPTH, vec![]),
            RegF::Null => (T_NULL, vec![]),
            RegF::Sys(t) => (t, vec![]),
            RegF::Uav(u) => (T_UAV, vec![self.s.uavs[self.s.uav_map[u as usize].unwrap()].slot]),
            RegF::G(g) => (T_TGSM, vec![g]),
        }
    }

    /// `sel`: 0 mask, 1 swizzle, 2 select-1; `num` component count code (0 = none, 1 = one, 2 = four)
    fn reg(&self, out: &mut Vec<u32>, r: &Reg, num: u32, sel: u32, bits: u32, modifier: u32) {
        let (ty, idx) = self.reg_parts(r);
        let mut t = num | (ty << 12) | ((idx.len() as u32) << 20);
        if num == 2 {
            t |= (sel << 2) | (bits << 4);
        }
        let rel_at = idx.len().wrapping_sub(1);
        if r.rel.is_some() {
            // last index: immediate + relative
            t |= 3 << (22 + 3 * rel_at as u32);
        }
        if modifier != 0 {
            t |= 1 << 31;
        }
        out.push(t);
        if modifier != 0 {
            out.push(1 | (modifier << 6));
        }
        for (k, &v) in idx.iter().enumerate() {
            out.push(v);
            if k == rel_at {
                if let Some((tr, c)) = r.rel {
                    // the relative part: a temp's component (select-1)
                    out.push(2 | (2 << 2) | ((c as u32) << 4) | (T_TEMP << 12) | (1 << 20));
                    out.push(tr);
                }
            }
        }
    }

    fn src(&self, out: &mut Vec<u32>, s: &Src) {
        match s {
            Src::R { r, swz, neg, abs, one } => {
                let m = (*neg as u32) | ((*abs as u32) << 1);
                if matches!(r.f, RegF::Depth) {
                    self.reg(out, r, 1, 0, 0, m);
                } else if *one {
                    self.reg(out, r, 2, 2, swz[0] as u32, m);
                } else {
                    let b = swz[0] as u32 | (swz[1] as u32) << 2 | (swz[2] as u32) << 4 | (swz[3] as u32) << 6;
                    self.reg(out, r, 2, 1, b, m);
                }
            }
            Src::Imm(v, one) => {
                if *one {
                    out.push(1 | (T_IMM32 << 12));
                    out.push(v[0]);
                } else {
                    out.push(2 | (T_IMM32 << 12));
                    out.extend_from_slice(v);
                }
            }
            Src::Tex(t, swz) => {
                let slot = self.s.texs[self.s.tex_map[*t as usize].unwrap()].slot;
                let b = swz[0] as u32 | (swz[1] as u32) << 2 | (swz[2] as u32) << 4 | (swz[3] as u32) << 6;
                out.push(2 | (1 << 2) | (b << 4) | (T_RESOURCE << 12) | (1 << 20));
                out.push(slot);
            }
            Src::Samp(s) => {
                let slot = self.s.samps[self.s.samp_map[*s as usize].unwrap()].slot;
                out.push((T_SAMPLER << 12) | (1 << 20));
                out.push(slot);
            }
            Src::SampC(s, c) => {
                let slot = self.s.samps[self.s.samp_map[*s as usize].unwrap()].slot;
                out.push(2 | (2 << 2) | ((*c as u32) << 4) | (T_SAMPLER << 12) | (1 << 20));
                out.push(slot);
            }
            Src::Uav(u, swz) | Src::G(u, swz) => {
                let (ty, slot) = match s {
                    Src::Uav(..) => (T_UAV, self.s.uavs[self.s.uav_map[*u as usize].unwrap()].slot),
                    _ => (T_TGSM, *u),
                };
                let b = swz[0] as u32 | (swz[1] as u32) << 2 | (swz[2] as u32) << 4 | (swz[3] as u32) << 6;
                out.push(2 | (1 << 2) | (b << 4) | (ty << 12) | (1 << 20));
                out.push(slot);
            }
        }
    }

    fn dst(&self, out: &mut Vec<u32>, d: &Dst) {
        match d.r.f {
            RegF::Null => out.push(T_NULL << 12),
            RegF::Depth => self.reg(out, &d.r, 1, 0, 0, 0),
            _ => self.reg(out, &d.r, 2, 0, d.mask as u32, 0),
        }
    }

    fn ins(&self, out: &mut Vec<u32>, i: &Ins) {
        let at = out.len();
        let ext = i.offset != [0; 3];
        out.push(0);
        if ext {
            let o = |v: i32| (v as u32) & 0xf;
            out.push(1 | (o(i.offset[0]) << 9) | (o(i.offset[1]) << 13) | (o(i.offset[2]) << 17));
        }
        for d in &i.dst {
            self.dst(out, d);
        }
        for s in &i.src {
            self.src(out, s);
        }
        let len = (out.len() - at) as u32;
        out[at] = i.op | (i.ctl << 11) | ((i.sat as u32) << 13) | ((i.nz as u32) << 18) | (len << 24) | ((ext as u32) << 31);
    }
}

fn decl(out: &mut Vec<u32>, op: u32, ctl: u32, body: &[u32]) {
    out.push(op | (ctl << 11) | (((body.len() + 1) as u32) << 24));
    out.extend_from_slice(body);
}

/// The SHDR/SHEX chunk.
fn program(s: &mut Shader) -> Vec<u32> {
    let temps = alloc(&mut s.code, &s.marks.clone());
    let s = &*s;
    let e = Enc { s };
    let kind = if s.threads.is_some() { 5 } else { (!s.pixel) as u32 };
    let mut w = vec![kind << 16 | (s.major << 4) | s.minor, 0];
    // global flags: refactoring allowed (+ early depth stencil)
    decl(&mut w, 0x6a, 1 | ((s.early_depth as u32) << 2), &[]);
    if !s.icb.is_empty() {
        w.push(0x35 | (3 << 11));
        w.push(2 + 4 * s.icb.len() as u32);
        for v in &s.icb {
            w.extend_from_slice(v);
        }
    }
    for cb in &s.cbs {
        let t = 2 | (1 << 2) | (0xe4 << 4) | (T_CB << 12) | (2 << 20);
        decl(&mut w, 0x59, cb.dynamic as u32, &[t, cb.slot, cb.size.max(1)]);
    }
    for sm in &s.samps {
        decl(&mut w, 0x5a, sm.cmp as u32, &[(T_SAMPLER << 12) | (1 << 20), sm.slot]);
    }
    for t in &s.texs {
        let r = t.ret;
        let ret = r | (r << 4) | (r << 8) | (r << 12);
        decl(&mut w, 0x58, t.dim, &[(T_RESOURCE << 12) | (1 << 20), t.slot, ret]);
    }
    for u in &s.uavs {
        let r = u.ret;
        let ret = r | (r << 4) | (r << 8) | (r << 12);
        decl(&mut w, 0x9c, u.dim, &[(T_UAV << 12) | (1 << 20), u.slot, ret]);
    }
    for (g, &bytes) in s.tgsm.iter().enumerate() {
        decl(&mut w, 0x9f, 0, &[(T_TGSM << 12) | (1 << 20), g as u32, bytes]);
    }
    for &(ty, mask) in &s.sys_inputs {
        // (vThreadIDInGroupFlattened is one component)
        let t = if ty == 35 { 1 | (ty << 12) } else { 2 | ((mask as u32) << 4) | (ty << 12) };
        decl(&mut w, 0x5f, 0, &[t]);
    }
    let reg_tok = |ty: u32, mask: u8| 2 | ((mask as u32) << 4) | (ty << 12) | (1 << 20);
    for i in &s.inputs {
        let t = reg_tok(T_INPUT, i.mask);
        if s.pixel {
            match i.sysval {
                0 => decl(&mut w, 0x62, i.interp, &[t, i.reg]),
                1 => decl(&mut w, 0x64, i.interp, &[t, i.reg, 1]),
                v => decl(&mut w, 0x63, 1, &[t, i.reg, v]),
            }
        } else if i.sysval != 0 {
            decl(&mut w, 0x60, 0, &[t, i.reg, i.sysval]);
        } else {
            decl(&mut w, 0x5f, 0, &[t, i.reg]);
        }
    }
    for o in &s.outputs {
        if o.depth {
            decl(&mut w, 0x65, 0, &[1 | (T_DEPTH << 12)]);
        } else if o.sysval != 0 && o.sysval < 64 {
            decl(&mut w, 0x67, 0, &[reg_tok(T_OUTPUT, o.mask), o.reg, o.sysval]);
        } else {
            decl(&mut w, 0x65, 0, &[reg_tok(T_OUTPUT, o.mask), o.reg]);
        }
    }
    if temps > 0 {
        decl(&mut w, 0x68, 0, &[temps]);
    }
    for (n, &size) in s.xs.iter().enumerate() {
        decl(&mut w, 0x69, 0, &[n as u32, size, 4]);
    }
    if let Some([x, y, z]) = s.threads {
        decl(&mut w, 0x9b, 0, &[x, y, z]);
    }
    for i in &s.code {
        e.ins(&mut w, i);
    }
    w[1] = w.len() as u32;
    w
}

fn put_str(b: &mut Vec<u8>, s: &str) -> u32 {
    let at = b.len() as u32;
    b.extend_from_slice(s.as_bytes());
    b.push(0);
    at
}

fn w32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn set32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

fn signature(els: &[SigEl], used_out: bool) -> Vec<u8> {
    let mut b = Vec::new();
    w32(&mut b, els.len() as u32);
    w32(&mut b, 8);
    let names_at = 8 + 24 * els.len();
    let mut names = Vec::new();
    for e in els {
        let off = names_at as u32 + names.len() as u32;
        put_str(&mut names, &e.name);
        w32(&mut b, off);
        w32(&mut b, e.index);
        w32(&mut b, e.sysval);
        w32(&mut b, e.ctype);
        w32(&mut b, if e.depth { u32::MAX } else { e.reg });
        // mask, then the read/write mask (outputs: the complement of what is written)
        let rw = if used_out { (!e.mask) & 0xf } else { e.mask };
        w32(&mut b, e.mask as u32 | ((rw as u32) << 8));
    }
    b.extend_from_slice(&names);
    while b.len() % 4 != 0 {
        b.push(0xab);
    }
    b
}

/// The resource definitions (what D3DReflect reads).
fn rdef(s: &Shader) -> Vec<u8> {
    let sm5 = s.major >= 5;
    let header = if sm5 { 60 } else { 28 };
    let mut b = vec![0u8; header];
    let ncb = s.cbs.len() as u32;
    // bound resources: samplers, textures, cbuffers
    let mut binds: Vec<(String, u32, u32, u32, u32, u32)> = Vec::new(); // name, type, ret, dim, slot, flags
    for sm in &s.samps {
        binds.push((sm.name.clone(), 3, 0, 0, sm.slot, sm.cmp as u32 * 2));
    }
    for t in &s.texs {
        let dim = match t.dim {
            1 => 1,
            2 => 2,
            3 => 4,
            4 => 6,
            5 => 8,
            6 => 9,
            7 => 3,
            8 => 5,
            9 => 7,
            _ => 10,
        };
        binds.push((t.name.clone(), 2, t.ret, dim, t.slot, (t.comps - 1) << 2));
    }
    for u in &s.uavs {
        let dim = match u.dim {
            1 => 1,
            2 => 2,
            3 => 4,
            5 => 8,
            7 => 3,
            _ => 5,
        };
        binds.push((u.name.clone(), 4, u.ret, dim, u.slot, (u.comps - 1) << 2));
    }
    for cb in &s.cbs {
        binds.push((cb.name.clone(), 0, 0, 0, cb.slot, 0));
    }
    let cb_at = b.len();
    // constant buffer descriptions: name, vars, var offset, size, flags, type
    b.resize(cb_at + 24 * s.cbs.len(), 0);
    let bind_at = b.len();
    b.resize(bind_at + 32 * binds.len(), 0);
    let mut fix_names: Vec<(usize, String)> = Vec::new();
    for (n, bd) in binds.iter().enumerate() {
        let at = bind_at + 32 * n;
        fix_names.push((at, bd.0.clone()));
        set32(&mut b, at + 4, bd.1);
        set32(&mut b, at + 8, bd.2);
        set32(&mut b, at + 12, bd.3);
        set32(&mut b, at + 16, if bd.1 == 2 || bd.1 == 4 { u32::MAX } else { 0 });
        set32(&mut b, at + 20, bd.4);
        set32(&mut b, at + 24, 1);
        set32(&mut b, at + 28, bd.5);
    }
    let var_size = if sm5 { 40 } else { 24 };
    for (n, cb) in s.cbs.iter().enumerate() {
        let at = cb_at + 24 * n;
        fix_names.push((at, cb.name.clone()));
        set32(&mut b, at + 4, cb.vars.len() as u32);
        let vars_at = b.len();
        set32(&mut b, at + 8, vars_at as u32);
        set32(&mut b, at + 12, cb.size * 16);
        b.resize(vars_at + var_size * cb.vars.len(), 0);
        for (k, v) in cb.vars.iter().enumerate() {
            let va = vars_at + var_size * k;
            fix_names.push((va, v.name.clone()));
            set32(&mut b, va + 4, v.offset);
            set32(&mut b, va + 8, v.size);
            set32(&mut b, va + 12, if v.used { 2 } else { 0 });
            // the type
            let ta = b.len();
            set32(&mut b, va + 16, ta as u32);
            let mut t = Vec::new();
            t.extend_from_slice(&v.class.to_le_bytes());
            t.extend_from_slice(&v.ty.to_le_bytes());
            t.extend_from_slice(&v.rows.to_le_bytes());
            t.extend_from_slice(&v.cols.to_le_bytes());
            t.extend_from_slice(&v.elems.to_le_bytes());
            t.extend_from_slice(&0u16.to_le_bytes());
            t.extend_from_slice(&0u32.to_le_bytes());
            if sm5 {
                t.extend_from_slice(&[0; 16]);
            }
            b.extend_from_slice(&t);
            if sm5 {
                // texture start, size, sampler start, size
                set32(&mut b, va + 24, u32::MAX);
                set32(&mut b, va + 32, u32::MAX);
            }
        }
    }
    for (at, name) in fix_names {
        let off = put_str(&mut b, &name);
        set32(&mut b, at, off);
    }
    let creator = put_str(&mut b, "sake HLSL compiler");
    set32(&mut b, 0, ncb);
    set32(&mut b, 4, cb_at as u32);
    set32(&mut b, 8, binds.len() as u32);
    set32(&mut b, 12, bind_at as u32);
    let kind = if s.threads.is_some() { 0x4353 } else if s.pixel { 0xffff } else { 0xfffe };
    set32(&mut b, 16, (kind << 16) | (s.major << 8) | s.minor);
    set32(&mut b, 20, 0x100);
    set32(&mut b, 24, creator);
    if sm5 {
        b[28..32].copy_from_slice(b"RD11");
        for (k, v) in [60u32, 24, 32, 40, 36, 12, 0].iter().enumerate() {
            set32(&mut b, 32 + 4 * k, *v);
        }
    }
    while b.len() % 4 != 0 {
        b.push(0);
    }
    b
}

/// The whole DXBC blob.
pub fn finish(mut s: Shader) -> Vec<u8> {
    let prog = program(&mut s);
    let shdr: Vec<u8> = prog.iter().flat_map(|w| w.to_le_bytes()).collect();
    let chunks: Vec<([u8; 4], Vec<u8>)> = vec![
        (*b"RDEF", rdef(&s)),
        (*b"ISGN", signature(&s.inputs, false)),
        (*b"OSGN", signature(&s.outputs, true)),
        (if s.major >= 5 { *b"SHEX" } else { *b"SHDR" }, shdr),
    ];
    container(&chunks)
}

pub fn container(chunks: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(b"DXBC");
    b.extend_from_slice(&[0; 16]);
    w32(&mut b, 1);
    w32(&mut b, 0);
    w32(&mut b, chunks.len() as u32);
    let mut off = 32 + 4 * chunks.len();
    for (_, c) in chunks {
        w32(&mut b, off as u32);
        off += 8 + c.len();
    }
    for (t, c) in chunks {
        b.extend_from_slice(t);
        w32(&mut b, c.len() as u32);
        b.extend_from_slice(c);
    }
    let n = b.len() as u32;
    set32(&mut b, 24, n);
    let h = checksum(&b[20..]);
    b[4..20].copy_from_slice(&h);
    b
}

// ---- the DXBC checksum: MD5 with its own padding ----

fn md5_block(st: &mut [u32; 4], blk: &[u8]) {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 4, 11, 16,
        23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64).map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32).collect();
    let m: Vec<u32> = blk.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    let [mut a, mut b, mut c, mut d] = *st;
    for i in 0..64 {
        let (f, g) = match i / 16 {
            0 => ((b & c) | (!b & d), i),
            1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
            2 => (b ^ c ^ d, (3 * i + 5) % 16),
            _ => (c ^ (b | !d), (7 * i) % 16),
        };
        let t = d;
        d = c;
        c = b;
        b = b.wrapping_add(a.wrapping_add(f).wrapping_add(k[i]).wrapping_add(m[g]).rotate_left(S[i]));
        a = t;
    }
    st[0] = st[0].wrapping_add(a);
    st[1] = st[1].wrapping_add(b);
    st[2] = st[2].wrapping_add(c);
    st[3] = st[3].wrapping_add(d);
}

/// The checksum of a DXBC blob (from offset 20 on).
pub fn checksum(data: &[u8]) -> [u8; 16] {
    let mut st = [0x67452301u32, 0xefcdab89, 0x98badcfe, 0x10325476];
    let len = data.len();
    let full = len / 64 * 64;
    for blk in data[..full].chunks_exact(64) {
        md5_block(&mut st, blk);
    }
    let rest = &data[full..];
    let bits = (len as u32).wrapping_mul(8);
    let mut last = [0u8; 64];
    if rest.len() >= 56 {
        last[..rest.len()].copy_from_slice(rest);
        last[rest.len()] = 0x80;
        md5_block(&mut st, &last);
        last = [0; 64];
        last[0..4].copy_from_slice(&bits.to_le_bytes());
        last[60..64].copy_from_slice(&((bits >> 2) | 1).to_le_bytes());
    } else {
        last[0..4].copy_from_slice(&bits.to_le_bytes());
        last[4..4 + rest.len()].copy_from_slice(rest);
        last[4 + rest.len()] = 0x80;
        last[60..64].copy_from_slice(&((bits >> 2) | 1).to_le_bytes());
    }
    md5_block(&mut st, &last);
    let mut out = [0u8; 16];
    for (k, v) in st.iter().enumerate() {
        out[4 * k..4 * k + 4].copy_from_slice(&v.to_le_bytes());
    }
    out
}
