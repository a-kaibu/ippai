//! ID3DXEffect over compiled effects (fx_2_0 binaries, as D3DXCreateEffect gets them from games):
//! parameters, techniques and passes parsed from the binary; a pass sets its render, texture
//! stage and sampler states, textures and shaders on the device, and the shaders' constants
//! from the parameters of the same name (their constant tables). Effect source text is compiled
//! to this binary first (hlslc::compile_effect, in d3dx9.rs). Shader preshaders and the FXLC
//! expressions of states are evaluated on the CPU. Not done: array selectors (logged), parameter
//! blocks.

#![allow(non_snake_case)]

use crate::api::{vtable, Ctx, Handler};
use crate::d3dx9::{add_ref, com_call, qi_no};
use crate::mem::{self, r32, w32};
use crate::HashMap;
use std::sync::Mutex;

// D3DXPARAMETER_CLASS / _TYPE
const PC_SCALAR: u32 = 0;
const PC_VECTOR: u32 = 1;
const PC_MATRIX_ROWS: u32 = 2;
const PC_MATRIX_COLUMNS: u32 = 3;
const PC_OBJECT: u32 = 4;
const PC_STRUCT: u32 = 5;
const PT_BOOL: u32 = 1;
const PT_FLOAT: u32 = 3;
const PT_STRING: u32 = 4;
const PT_PIXELSHADER: u32 = 15;
const PT_VERTEXSHADER: u32 = 16;

fn is_sampler(ty: u32) -> bool {
    (10..=14).contains(&ty)
}
fn is_texture(ty: u32) -> bool {
    (5..=9).contains(&ty)
}

#[derive(Default, Clone)]
struct Param {
    name: String,
    semantic: String,
    class: u32,
    ty: u32,
    rows: u32,
    cols: u32,
    elements: u32,
    /// member or element parameters (indices into Effect::params)
    children: Vec<usize>,
    annotations: Vec<usize>,
    flags: u32,
    bytes: u32,
    /// the top-level parameter whose value holds this one, and where
    top: usize,
    off: u32,
    /// objects: the object id; samplers: their states
    object: Option<u32>,
    sampler: Vec<State>,
}

#[derive(Clone)]
enum Kind {
    /// the state's own value (its parameter)
    Const,
    /// another parameter's value, by name
    Ref(usize),
    /// an FXLC expression of parameters (evaluated as preshaders are)
    Expr(std::sync::Arc<Pres>),
    /// array selectors: not evaluated
    Unsupported,
}

#[derive(Clone)]
struct State {
    op: u32,
    index: u32,
    /// the parameter holding its value
    param: usize,
    kind: Kind,
}

struct Pass {
    name: String,
    annotations: Vec<usize>,
    states: Vec<State>,
}
struct Technique {
    name: String,
    annotations: Vec<usize>,
    passes: Vec<Pass>,
}

#[derive(Default)]
struct Object {
    data: Vec<u8>,
    ty: u32,
    /// shaders: the device's shader object; textures: the texture set (referenced)
    created: u32,
}

struct Effect {
    device: u32,
    params: Vec<Param>,
    /// top-level parameters
    top: Vec<usize>,
    /// each top-level parameter's value bytes
    values: HashMap<usize, Vec<u8>>,
    techniques: Vec<Technique>,
    objects: Vec<Object>,
    /// handles: params, then techniques, then passes (in technique order)
    hbase: u32,
    tech: usize,
    /// Begin's state block, the pass being applied
    block: u32,
    pass: Option<usize>,
    /// shader constant tables by shader object
    ctabs: HashMap<u32, Vec<Const>>,
    /// shader preshaders by shader object
    pres: HashMap<u32, Pres>,
    /// guest copies of the names and strings the Get*Desc / GetString calls handed out (one
    /// per string, valid while the effect lives, as Windows' point into the effect)
    strings: HashMap<Vec<u8>, u32>,
}

/// A guest copy of `s` (NUL-terminated) owned by effect `this`.
fn fx_str(this: u32, s: &[u8]) -> u32 {
    with(this, |e| {
        *e.strings.entry(s.to_vec()).or_insert_with(|| {
            let a = mem::heap_alloc(s.len() as u32 + 1, true);
            mem::write(a, s);
            a
        })
    })
    .unwrap_or(0)
}

struct Const {
    name: String,
    set: u16,
    reg: u32,
    count: u32,
    class: u16,
    rows: u32,
    cols: u32,
}

static FX: Mutex<Option<HashMap<u32, Effect>>> = Mutex::new(None);

fn with<R>(this: u32, f: impl FnOnce(&mut Effect) -> R) -> Option<R> {
    FX.lock().unwrap().get_or_insert_with(HashMap::default).get_mut(&this).map(f)
}

// ---- parsing ----

struct Rd<'a> {
    d: &'a [u8],
    at: usize,
}
impl Rd<'_> {
    fn u32(&mut self) -> Result<u32, String> {
        let v = self.d.get(self.at..self.at + 4).ok_or("effect data cut short")?;
        self.at += 4;
        Ok(u32::from_le_bytes(v.try_into().unwrap()))
    }
}

struct Parser<'a> {
    base: &'a [u8],
    params: Vec<Param>,
    obj_ty: HashMap<u32, u32>,
}

impl<'a> Parser<'a> {
    fn at(&self, off: u32) -> Rd<'a> {
        Rd { d: self.base, at: off as usize }
    }
    fn name(&self, off: u32) -> Result<String, String> {
        let mut r = self.at(off);
        let n = r.u32()? as usize;
        let b = self.base.get(r.at..r.at + n).ok_or("bad name")?;
        Ok(String::from_utf8_lossy(b.split(|&x| x == 0).next().unwrap_or(&[])).into_owned())
    }
    /// A typedef at `r` (the parameter `parent` for array elements); returns the new parameter.
    fn typedef(&mut self, r: &mut Rd, parent: Option<usize>, flags: u32) -> Result<usize, String> {
        let mut p = Param { flags, ..Default::default() };
        match parent {
            None => {
                p.ty = r.u32()?;
                p.class = r.u32()?;
                p.name = self.name(r.u32()?)?;
                p.semantic = self.name(r.u32()?)?;
                p.elements = r.u32()?;
                match p.class {
                    PC_VECTOR => {
                        p.cols = r.u32()?;
                        p.rows = r.u32()?;
                        p.bytes = 4 * p.rows * p.cols;
                    }
                    PC_SCALAR | PC_MATRIX_ROWS | PC_MATRIX_COLUMNS => {
                        p.rows = r.u32()?;
                        p.cols = r.u32()?;
                        p.bytes = 4 * p.rows * p.cols;
                    }
                    PC_STRUCT => {
                        // (the member count, kept as children.len() once read)
                        p.rows = r.u32()?;
                    }
                    PC_OBJECT if !is_sampler(p.ty) => p.bytes = 4,
                    _ => {}
                }
            }
            Some(q) => {
                let q = &self.params[q];
                p = Param { name: q.name.clone(), semantic: q.semantic.clone(), class: q.class, ty: q.ty, rows: q.rows, cols: q.cols, flags, ..Default::default() };
                p.bytes = if q.class == PC_OBJECT && !is_sampler(q.ty) { 4 } else { 4 * q.rows * q.cols };
            }
        }
        let idx = self.params.len();
        self.params.push(p);
        let (elements, class, members) = (self.params[idx].elements, self.params[idx].class, self.params[idx].rows);
        if elements != 0 {
            let save = r.at;
            let mut bytes = 0;
            for _ in 0..elements {
                r.at = save;
                let e = self.typedef(r, Some(idx), flags)?;
                bytes += self.params[e].bytes;
                self.params[idx].children.push(e);
            }
            self.params[idx].bytes = bytes;
        } else if class == PC_STRUCT {
            let mut bytes = 0;
            for _ in 0..members {
                let m = self.typedef(r, None, flags)?;
                bytes += self.params[m].bytes;
                self.params[idx].children.push(m);
            }
            self.params[idx].bytes = bytes;
        }
        Ok(idx)
    }
    /// Places parameter `i` at `off` of top-level `top`'s value and reads its object ids /
    /// sampler states from `r`.
    fn value(&mut self, i: usize, top: usize, off: u32, r: &mut Rd) -> Result<(), String> {
        self.params[i].top = top;
        self.params[i].off = off;
        let kids = self.params[i].children.clone();
        if !kids.is_empty() {
            let mut o = off;
            for k in kids {
                self.value(k, top, o, r)?;
                o += self.params[k].bytes;
            }
            return Ok(());
        }
        let (class, ty) = (self.params[i].class, self.params[i].ty);
        if class == PC_OBJECT {
            if is_sampler(ty) {
                let n = r.u32()?;
                let mut states = Vec::new();
                for _ in 0..n {
                    states.push(self.state(r)?);
                }
                self.params[i].sampler = states;
            } else {
                let id = r.u32()?;
                self.params[i].object = Some(id);
                self.obj_ty.insert(id, ty);
            }
        }
        Ok(())
    }
    /// A typedef + initial value pair (parameters, annotations, states); returns the parameter
    /// and its value bytes.
    fn param(&mut self, tdef: u32, val: u32, flags: u32) -> Result<(usize, Vec<u8>), String> {
        let mut r = self.at(tdef);
        let i = self.typedef(&mut r, None, flags)?;
        let bytes = self.params[i].bytes as usize;
        let numeric = self.params[i].class != PC_OBJECT;
        let v = if numeric { self.base.get(val as usize..val as usize + bytes).map(|b| b.to_vec()).unwrap_or(vec![0; bytes]) } else { vec![0; bytes] };
        let mut r = self.at(val);
        self.value(i, i, 0, &mut r)?;
        Ok((i, v))
    }
    fn state(&mut self, r: &mut Rd) -> Result<State, String> {
        let op = r.u32()?;
        let index = r.u32()?;
        let (t, v) = (r.u32()?, r.u32()?);
        let (param, val) = self.param(t, v, 0)?;
        STATE_VALUES.lock().unwrap().push((param, val));
        Ok(State { op, index, param, kind: Kind::Const })
    }
    fn annotations(&mut self, r: &mut Rd, n: u32) -> Result<Vec<usize>, String> {
        let mut v = Vec::new();
        for _ in 0..n {
            let (t, val) = (r.u32()?, r.u32()?);
            let (a, bytes) = self.param(t, val, 0)?;
            STATE_VALUES.lock().unwrap().push((a, bytes));
            v.push(a);
        }
        Ok(v)
    }
}

/// (values of states and annotations, collected while parsing: they are top-level parameters of
/// their own)
static STATE_VALUES: Mutex<Vec<(usize, Vec<u8>)>> = Mutex::new(Vec::new());

fn parse(data: &[u8]) -> Result<(Vec<Param>, Vec<usize>, HashMap<usize, Vec<u8>>, Vec<Technique>, Vec<Object>), String> {
    let mut r = Rd { d: data, at: 0 };
    let tag = r.u32()?;
    if tag != 0xfeff0901 {
        return Err(format!("not a compiled effect ({tag:#x})"));
    }
    let start = r.u32()?;
    let base = &data[8..];
    let mut p = Parser { base, params: Vec::new(), obj_ty: HashMap::default() };
    STATE_VALUES.lock().unwrap().clear();
    let mut r = p.at(start);
    let nparams = r.u32()?;
    let ntech = r.u32()?;
    let _shaders = r.u32()?;
    let nobjects = r.u32()?;
    let mut top = Vec::new();
    let mut values = HashMap::default();
    for _ in 0..nparams {
        let (t, v) = (r.u32()?, r.u32()?);
        let flags = r.u32()?;
        let nanno = r.u32()?;
        let (i, bytes) = p.param(t, v, flags)?;
        let annos = p.annotations(&mut r, nanno)?;
        p.params[i].annotations = annos;
        top.push(i);
        values.insert(i, bytes);
    }
    let mut techniques = Vec::new();
    for _ in 0..ntech {
        let name = p.name(r.u32()?)?;
        let nanno = r.u32()?;
        let npass = r.u32()?;
        let annotations = p.annotations(&mut r, nanno)?;
        let mut passes = Vec::new();
        for _ in 0..npass {
            let name = p.name(r.u32()?)?;
            let nanno = r.u32()?;
            let nstate = r.u32()?;
            let annotations = p.annotations(&mut r, nanno)?;
            let mut states = Vec::new();
            for _ in 0..nstate {
                states.push(p.state(&mut r)?);
            }
            passes.push(Pass { name, annotations, states });
        }
        techniques.push(Technique { name, annotations, passes });
    }
    let mut objects: Vec<Object> = (0..nobjects).map(|_| Object::default()).collect();
    let nstrings = r.u32()?;
    let nres = r.u32()?;
    let blob = |r: &mut Rd| -> Result<Vec<u8>, String> {
        let n = r.u32()? as usize;
        let b = r.d.get(r.at..r.at + n).ok_or("object data cut short")?.to_vec();
        r.at += (n + 3) & !3;
        Ok(b)
    };
    for _ in 0..nstrings {
        let id = r.u32()? as usize;
        let b = blob(&mut r)?;
        if let Some(o) = objects.get_mut(id) {
            o.data = b;
        }
    }
    let mut unsupported = 0;
    for _ in 0..nres {
        let (ti, index, elem, si, usage) = (r.u32()?, r.u32()?, r.u32()?, r.u32()?, r.u32()?);
        let b = blob(&mut r)?;
        // the state this is for
        let sampler_of = if ti == u32::MAX {
            let mut q = *top.get(index as usize).ok_or("bad resource parameter")?;
            if elem != u32::MAX && !p.params[q].children.is_empty() {
                q = p.params[q].children[elem as usize];
            }
            Some(q)
        } else {
            None
        };
        let st: Option<&State> = match sampler_of {
            Some(q) => p.params[q].sampler.get(si as usize),
            None => techniques.get(ti as usize).and_then(|t| t.passes.get(index as usize)).and_then(|ps| ps.states.get(si as usize)),
        };
        let Some(sp) = st.map(|s| s.param) else { return Err("bad resource state".into()) };
        let kind = match usage {
            0 if matches!(p.params[sp].ty, PT_VERTEXSHADER | PT_PIXELSHADER) => {
                if let Some(id) = p.params[sp].object {
                    if let Some(o) = objects.get_mut(id as usize) {
                        o.data = b;
                    }
                }
                Kind::Const
            }
            2 => match pres_body(&b) {
                Some(pr) => Kind::Expr(std::sync::Arc::new(pr)),
                None => {
                    unsupported += 1;
                    Kind::Unsupported
                }
            },
            1 => {
                let name = String::from_utf8_lossy(b.split(|&x| x == 0).next().unwrap_or(&[])).into_owned();
                match top.iter().find(|&&i| p.params[i].name == name) {
                    Some(&i) => Kind::Ref(i),
                    None => return Err(format!("state refers to unknown parameter {name}")),
                }
            }
            _ => {
                unsupported += 1;
                Kind::Unsupported
            }
        };
        let st: Option<&mut State> = match sampler_of {
            Some(q) => p.params[q].sampler.get_mut(si as usize),
            None => techniques.get_mut(ti as usize).and_then(|t| t.passes.get_mut(index as usize)).and_then(|ps| ps.states.get_mut(si as usize)),
        };
        if let Some(st) = st {
            st.kind = kind;
        }
    }
    if unsupported != 0 {
        eprintln!("sake: D3DXEffect: {unsupported} expression / array selector states not evaluated");
    }
    for (id, ty) in &p.obj_ty {
        if let Some(o) = objects.get_mut(*id as usize) {
            o.ty = *ty;
        }
    }
    for (i, v) in STATE_VALUES.lock().unwrap().drain(..) {
        values.insert(i, v);
    }
    Ok((p.params, top, values, techniques, objects))
}

/// The comment blocks of shader-style bytecode (after its version token): (fourcc, body).
pub(crate) fn comments(code: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let w = |i: usize| code.get(4 * i..4 * i + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
    let mut out = Vec::new();
    let mut i = 1;
    while 4 * i < code.len() {
        let t = w(i);
        if t & 0xffff == 0xfffe {
            let n = (t >> 16) as usize;
            if n >= 1 {
                let four = w(i + 1).to_le_bytes();
                out.push((four, code.get(4 * (i + 2)..(4 * (i + 1 + n)).min(code.len())).unwrap_or(&[])));
            }
            i += n + 1;
        } else if t == 0xffff {
            break;
        } else {
            i += 1;
        }
    }
    out
}

/// A constant table (a CTAB comment's body).
fn ctab_of(d: &[u8]) -> Vec<Const> {
    let r16 = |o: usize| d.get(o..o + 2).map_or(0, |b| u16::from_le_bytes([b[0], b[1]]));
    let r32 = |o: usize| d.get(o..o + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()));
    let s = |o: usize| String::from_utf8_lossy(d.get(o..).unwrap_or(&[]).split(|&x| x == 0).next().unwrap_or(&[])).into_owned();
    let (count, info) = (r32(12) as usize, r32(16) as usize);
    (0..count)
        .map(|k| {
            let e = info + 20 * k;
            let ti = r32(e + 12) as usize;
            Const {
                name: s(r32(e) as usize),
                set: r16(e + 4),
                reg: r16(e + 6) as u32,
                count: r16(e + 8) as u32,
                class: r16(ti),
                rows: r16(ti + 4) as u32,
                cols: r16(ti + 6) as u32,
            }
        })
        .collect()
}

/// A shader's constant table (its CTAB comment).
fn ctab(code: &[u8]) -> Vec<Const> {
    comments(code).into_iter().find(|(f, _)| f == b"CTAB").map(|(_, d)| ctab_of(d)).unwrap_or_default()
}

/// A preshader (the PRES comment of an effect's shader): constants the compiler moved out of
/// the shader, computed from parameters on the CPU. Its CTAB names the inputs, CLIT holds the
/// literals and FXLC the instructions; operands index scalars in a table (1 literals,
/// 2 inputs, 4 the shader's float constants, 7 temporaries).
struct Pres {
    inputs: Vec<Const>,
    lits: Vec<f64>,
    ins: Vec<PresIns>,
}
struct PresArg {
    table: u32,
    off: u32,
    /// relative addressing: (table, offset) of the register index
    index: Option<(u32, u32)>,
}
struct PresIns {
    op: u32,
    comps: u32,
    scalar: bool,
    /// inputs, then the output
    args: Vec<PresArg>,
}

fn pres_inputs(op: u32) -> Option<usize> {
    Some(match op {
        0x100..=0x10c | 0x120 | 0x130 => 1,
        0x200..=0x208 | 0x500 | 0x502 => 2,
        0x300 | 0x301 => 3,
        0xa00 | 0x700 => 6,
        0xa01 | 0x701 => 8,
        _ => return None,
    })
}

fn pres(code: &[u8]) -> Option<Pres> {
    let (_, body) = comments(code).into_iter().find(|(f, _)| f == b"PRES")?;
    pres_body(body)
}

/// A preshader's own bytecode (a PRES comment's body, or an effect's FXLC state expression:
/// version token, then CTAB, CLIT, FXLC comments).
fn pres_body(body: &[u8]) -> Option<Pres> {
    let mut inputs = Vec::new();
    let mut lits = Vec::new();
    let mut ins = Vec::new();
    for (f, d) in comments(body) {
        let w: Vec<u32> = d.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        match &f {
            b"CTAB" => inputs = ctab_of(d),
            b"CLIT" => {
                let n = *w.first()? as usize;
                lits = (0..n).filter_map(|k| d.get(4 + 8 * k..12 + 8 * k)).map(|b| f64::from_le_bytes(b.try_into().unwrap())).collect();
            }
            b"FXLC" => {
                let n = *w.first()?;
                let mut i = 1;
                for _ in 0..n {
                    let code = *w.get(i)?;
                    let nin = *w.get(i + 1)? as usize;
                    i += 2;
                    let op = (code >> 20) & 0x7ff;
                    if pres_inputs(op) != Some(nin) {
                        eprintln!("sake: D3DXEffect: preshader op {op:#x} with {nin} inputs is not known");
                        return None;
                    }
                    let mut args = Vec::new();
                    for _ in 0..=nin {
                        let index = if *w.get(i)? != 0 {
                            i += 3;
                            Some((*w.get(i - 2)?, *w.get(i - 1)?))
                        } else {
                            i += 1;
                            None
                        };
                        args.push(PresArg { table: *w.get(i)?, off: *w.get(i + 1)?, index });
                        i += 2;
                    }
                    ins.push(PresIns { op, comps: code & 0xffff, scalar: code & 0x8000_0000 != 0, args });
                }
            }
            _ => {}
        }
    }
    Some(Pres { inputs, lits, ins })
}

// ---- values ----

impl Effect {
    fn value(&self, i: usize) -> &[u8] {
        let p = &self.params[i];
        let v = &self.values[&p.top];
        &v[(p.off as usize).min(v.len())..((p.off + p.bytes) as usize).min(v.len())]
    }
    fn value_mut(&mut self, i: usize) -> &mut [u8] {
        let (top, off, bytes) = (self.params[i].top, self.params[i].off, self.params[i].bytes);
        let v = self.values.get_mut(&top).unwrap();
        let n = v.len();
        &mut v[(off as usize).min(n)..((off + bytes) as usize).min(n)]
    }
    fn u32_at(&self, i: usize, k: usize) -> u32 {
        self.value(i).get(4 * k..4 * k + 4).map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    /// element k of a numeric parameter as a float
    fn f32_at(&self, i: usize, k: usize) -> f32 {
        let v = self.u32_at(i, k);
        match self.params[i].ty {
            PT_FLOAT => f32::from_bits(v),
            PT_BOOL => (v != 0) as u32 as f32,
            _ => v as i32 as f32,
        }
    }
    /// Stores floats into a numeric parameter, converted to its type.
    fn set_floats(&mut self, i: usize, f: &[f32]) {
        let ty = self.params[i].ty;
        let v = self.value_mut(i);
        for (k, x) in f.iter().enumerate() {
            let Some(b) = v.get_mut(4 * k..4 * k + 4) else { break };
            let u = match ty {
                PT_FLOAT => x.to_bits(),
                PT_BOOL => (*x != 0.0) as u32,
                _ => *x as i32 as u32,
            };
            b.copy_from_slice(&u.to_le_bytes());
        }
    }
    fn n_values(&self, i: usize) -> usize {
        self.params[i].bytes as usize / 4
    }
    /// A handle's parameter (handles are our addresses; anything else is a name)
    fn param_of(&self, h: u32) -> Option<usize> {
        if h >= self.hbase && h < self.hbase + 4 * self.params.len() as u32 {
            return Some(((h - self.hbase) / 4) as usize);
        }
        if h == 0 {
            return None;
        }
        let name = crate::text::a2s(&mem::cstr(h));
        self.by_name(None, &name)
    }
    fn by_name(&self, parent: Option<usize>, name: &str) -> Option<usize> {
        // "a.b" and "a[2]"
        if let Some((a, b)) = name.split_once('.') {
            let p = self.by_name(parent, a)?;
            return self.by_name(Some(p), b);
        }
        if let Some((a, rest)) = name.split_once('[') {
            let p = self.by_name(parent, a)?;
            let k: usize = rest.trim_end_matches(']').parse().ok()?;
            return self.params[p].children.get(k).copied();
        }
        let list: &[usize] = match parent {
            None => &self.top,
            Some(p) => &self.params[p].children,
        };
        list.iter().copied().find(|&i| self.params[i].name == name)
    }
    fn tech_of(&self, h: u32) -> Option<usize> {
        let b = self.hbase + 4 * self.params.len() as u32;
        if h >= b && h < b + 4 * self.techniques.len() as u32 {
            return Some(((h - b) / 4) as usize);
        }
        if h == 0 {
            return None;
        }
        let name = crate::text::a2s(&mem::cstr(h));
        self.techniques.iter().position(|t| t.name == name)
    }
    fn h_param(&self, i: usize) -> u32 {
        self.hbase + 4 * i as u32
    }
    fn h_tech(&self, t: usize) -> u32 {
        self.hbase + 4 * (self.params.len() + t) as u32
    }
    fn h_pass(&self, t: usize, k: usize) -> u32 {
        let before: usize = self.techniques[..t].iter().map(|x| x.passes.len()).sum();
        self.hbase + 4 * (self.params.len() + self.techniques.len() + before + k) as u32
    }
    fn pass_of(&self, h: u32) -> Option<(usize, usize)> {
        let mut at = self.hbase + 4 * (self.params.len() + self.techniques.len()) as u32;
        for (t, tech) in self.techniques.iter().enumerate() {
            for k in 0..tech.passes.len() {
                if h == at {
                    return Some((t, k));
                }
                at += 4;
            }
        }
        None
    }
}

// ---- applying a pass ----

/// Device calls a pass makes (gathered under the lock, made after it).
enum Call {
    Dev(u32, Vec<u32>),
    /// Set*ShaderConstantF(start, floats)
    ConstF(bool, u32, Vec<f32>),
    ConstI(bool, u32, Vec<u32>),
    ConstB(bool, u32, Vec<u32>),
}

const RS: [u32; 103] = [7, 8, 9, 14, 15, 16, 19, 20, 22, 23, 24, 25, 26, 27, 28, 29, 34, 35, 36, 37, 38, 48, 52, 53, 54, 55, 56, 57,
    58, 59, 60, 128, 129, 130, 131, 132, 133, 134, 135, 198, 199, 200, 201, 202, 203, 204, 205, 136, 137, 139, 140, 141, 142,
    143, 145, 146, 147, 148, 151, 152, 154, 155, 166, 156, 157, 158, 159, 160, 161, 162, 163, 165, 167, 168, 170, 171, 172,
    173, 174, 175, 176, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 189, 190, 191, 192, 193, 194, 195, 206, 207,
    208, 209];
const TSS: [u32; 18] = [1, 26, 2, 3, 4, 27, 5, 6, 28, 7, 8, 9, 10, 11, 22, 23, 24, 32];

impl Effect {
    /// What state `s` does, for sampler stage `stage` when it is a sampler's state.
    fn state_calls(&self, s: &State, stage: u32, out: &mut Vec<Call>) {
        let (vp, expr) = match &s.kind {
            Kind::Const => (s.param, None),
            Kind::Ref(i) => (*i, None),
            // (the expression's results, as the state parameter's type)
            Kind::Expr(pr) => (s.param, Some(self.run_pres(pr).into_iter().map(|v| v.unwrap_or(0.0)).collect::<Vec<f64>>())),
            Kind::Unsupported => return,
        };
        let ty = self.params[vp].ty;
        let u32_at = |k: usize| match &expr {
            None => self.u32_at(vp, k),
            Some(v) => {
                let x = v.get(k).copied().unwrap_or(0.0);
                match ty {
                    PT_FLOAT => (x as f32).to_bits(),
                    PT_BOOL => (x != 0.0) as u32,
                    _ => x as i32 as u32,
                }
            }
        };
        let f32_at = |k: usize| match &expr {
            None => self.f32_at(vp, k),
            Some(v) => v.get(k).copied().unwrap_or(0.0) as f32,
        };
        let first = u32_at(0);
        let obj = |p: usize| self.params[p].object.and_then(|id| self.objects.get(id as usize)).map_or(0, |o| o.created);
        match s.op {
            0..=102 => out.push(Call::Dev(57, vec![RS[s.op as usize], first])),
            103..=120 => out.push(Call::Dev(67, vec![s.index, TSS[(s.op - 103) as usize], first])),
            122 => out.push(Call::Dev(89, vec![first])),
            146 => out.push(Call::Dev(92, vec![obj(vp)])),
            147 => out.push(Call::Dev(107, vec![obj(vp)])),
            148..=163 => {
                let vs = s.op < 156;
                let n = self.n_values(vp);
                match s.op - if vs { 148 } else { 156 } {
                    1 => out.push(Call::ConstB(vs, s.index, (0..n).map(u32_at).collect())),
                    2 => out.push(Call::ConstI(vs, s.index, (0..n).map(u32_at).collect())),
                    _ => out.push(Call::ConstF(vs, s.index, (0..n).map(f32_at).collect())),
                }
            }
            164 => out.push(Call::Dev(65, vec![if stage != u32::MAX { stage } else { s.index }, obj(vp)])),
            165..=177 => out.push(Call::Dev(69, vec![if stage != u32::MAX { stage } else { s.index }, s.op - 164, first])),
            178 => self.sampler_calls(vp, s.index, out),
            _ => {
                static ONCE: std::sync::Once = std::sync::Once::new();
                ONCE.call_once(|| eprintln!("sake: D3DXEffect: state {} not applied", s.op));
            }
        }
    }
    fn sampler_calls(&self, p: usize, stage: u32, out: &mut Vec<Call>) {
        for s in &self.params[p].sampler {
            self.state_calls(s, stage, out);
        }
    }
    /// Parameter `p` as constant `k`'s float4 registers.
    fn float_regs(&self, p: usize, k: &Const) -> Vec<f32> {
        let mut regs: Vec<f32> = Vec::new();
        let elems: Vec<usize> = if self.params[p].elements != 0 { self.params[p].children.clone() } else { vec![p] };
        for e in elems {
            let q = &self.params[e];
            let (rows, cols) = (q.rows.max(1), q.cols.max(1));
            let at = |r: u32, c: u32| if r < rows && c < cols { self.f32_at(e, (r * cols + c) as usize) } else { 0.0 };
            match k.class as u32 {
                PC_MATRIX_COLUMNS => {
                    for c in 0..k.cols.max(1) {
                        regs.extend((0..4).map(|r| at(r, c)));
                    }
                }
                PC_MATRIX_ROWS => {
                    for r in 0..k.rows.max(1) {
                        regs.extend((0..4).map(|c| at(r, c)));
                    }
                }
                _ => regs.extend((0..4).map(|c| if (c as usize) < self.n_values(e) { self.f32_at(e, c as usize) } else { 0.0 })),
            }
        }
        regs.truncate(4 * k.count as usize);
        regs
    }
    /// Runs shader `sh`'s preshader: the float constants it computes.
    fn pres_calls(&self, sh: u32, vs: bool, out: &mut Vec<Call>) {
        let Some(pr) = self.pres.get(&sh) else { return };
        let outv = self.run_pres(pr);
        // the registers written, in runs
        let regs = outv.len().div_ceil(4);
        let mut r = 0;
        while r < regs {
            if (0..4).all(|j| outv.get(4 * r + j).copied().flatten().is_none()) {
                r += 1;
                continue;
            }
            let start = r;
            let mut f = Vec::new();
            while r < regs && (0..4).any(|j| outv.get(4 * r + j).copied().flatten().is_some()) {
                f.extend((0..4).map(|j| outv.get(4 * r + j).copied().flatten().unwrap_or(0.0) as f32));
                r += 1;
            }
            out.push(Call::ConstF(vs, start as u32, f));
        }
    }
    /// Runs a preshader on the parameters' values: the output scalars it wrote.
    fn run_pres(&self, pr: &Pres) -> Vec<Option<f64>> {
        let mut input: Vec<f64> = Vec::new();
        for k in &pr.inputs {
            let Some(p) = self.by_name(None, &k.name) else { continue };
            let regs = if k.set == 2 {
                self.float_regs(p, k)
            } else {
                (0..k.count as usize).flat_map(|i| [self.f32_at(p, i), 0.0, 0.0, 0.0]).collect()
            };
            let at = 4 * k.reg as usize;
            if input.len() < at + regs.len() {
                input.resize(at + regs.len(), 0.0);
            }
            for (i, x) in regs.iter().enumerate() {
                input[at + i] = *x as f64;
            }
        }
        let mut temp: Vec<f64> = Vec::new();
        let mut outv: Vec<Option<f64>> = Vec::new();
        let get = |t: &[f64], o: &[Option<f64>], table: u32, i: usize| -> f64 {
            match table {
                1 => pr.lits.get(i).copied().unwrap_or(0.0),
                2 => input.get(i).copied().unwrap_or(0.0),
                4 => o.get(i).copied().flatten().unwrap_or(0.0),
                7 => t.get(i).copied().unwrap_or(0.0),
                _ => 0.0,
            }
        };
        for ins in &pr.ins {
            let nin = ins.args.len() - 1;
            let base = |a: &PresArg, t: &[f64], o: &[Option<f64>]| -> usize {
                let rel = a.index.map_or(0, |(tb, off)| get(t, o, tb, off as usize).round() as i64 * 4);
                (a.off as i64 + rel).max(0) as usize
            };
            let arg = |k: usize, j: usize, t: &[f64], o: &[Option<f64>]| -> f64 {
                let a = &ins.args[k];
                let j = if ins.scalar && k == 0 && nin > 1 { 0 } else { j };
                get(t, o, a.table, base(a, t, o) + j)
            };
            let mut res: Vec<f64> = Vec::new();
            match ins.op {
                // dot products: one result
                0x500 => res.push((0..ins.comps as usize).map(|j| arg(0, j, &temp, &outv) * arg(1, j, &temp, &outv)).sum()),
                0xa00 | 0x700 | 0xa01 | 0x701 => {
                    let v: Vec<f64> = (0..nin).map(|k| arg(k, 0, &temp, &outv)).collect();
                    res.push(v.chunks(2).map(|c| c[0] * c.get(1).copied().unwrap_or(0.0)).sum());
                }
                op => {
                    for j in 0..ins.comps as usize {
                        let a: Vec<f64> = (0..nin).map(|k| arg(k, j, &temp, &outv)).collect();
                        res.push(match op {
                            0x100 | 0x130 => a[0],
                            0x101 | 0x120 => -a[0],
                            0x103 => 1.0 / a[0],
                            0x104 => a[0] - a[0].floor(),
                            0x105 => a[0].exp2(),
                            0x106 => a[0].abs().log2(),
                            0x107 => 1.0 / a[0].abs().sqrt(),
                            0x108 => a[0].sin(),
                            0x109 => a[0].cos(),
                            0x10a => a[0].asin(),
                            0x10b => a[0].acos(),
                            0x10c => a[0].atan(),
                            0x200 => a[0].min(a[1]),
                            0x201 => a[0].max(a[1]),
                            0x202 => (a[0] < a[1]) as u32 as f64,
                            0x203 => (a[0] >= a[1]) as u32 as f64,
                            0x204 => a[0] + a[1],
                            0x205 => a[0] * a[1],
                            0x206 => a[0].atan2(a[1]),
                            0x208 => a[0] / a[1],
                            0x300 => if a[0] >= 0.0 { a[1] } else { a[2] },
                            0x301 => if a[0] != 0.0 { a[1] } else { a[2] },
                            _ => 0.0,
                        });
                    }
                }
            }
            let o = &ins.args[nin];
            let at = base(o, &temp, &outv);
            for (j, v) in res.into_iter().enumerate() {
                match o.table {
                    4 => {
                        if outv.len() <= at + j {
                            outv.resize(at + j + 1, None);
                        }
                        outv[at + j] = Some(v);
                    }
                    7 => {
                        if temp.len() <= at + j {
                            temp.resize(at + j + 1, 0.0);
                        }
                        temp[at + j] = v;
                    }
                    _ => {}
                }
            }
        }
        outv
    }
    /// The shader constants of shader object `sh` from the parameters of the same names.
    fn const_calls(&self, sh: u32, vs: bool, out: &mut Vec<Call>) {
        let Some(table) = self.ctabs.get(&sh) else { return };
        for k in table {
            let Some(p) = self.by_name(None, &k.name) else { continue };
            match k.set {
                // samplers: the sampler parameter's states on its stage
                3 => {
                    let stage = if vs { 257 + k.reg } else { k.reg };
                    if is_sampler(self.params[p].ty) {
                        self.sampler_calls(p, stage, out);
                    }
                }
                2 => {
                    let regs = self.float_regs(p, k);
                    if !regs.is_empty() {
                        out.push(Call::ConstF(vs, k.reg, regs));
                    }
                }
                1 => out.push(Call::ConstI(vs, k.reg, (0..4 * k.count as usize).map(|i| self.u32_at(p, i)).collect())),
                0 => out.push(Call::ConstB(vs, k.reg, (0..k.count as usize).map(|i| self.u32_at(p, i)).collect())),
                _ => {}
            }
        }
        self.pres_calls(sh, vs, out);
    }
    fn pass_calls(&self, k: usize) -> Vec<Call> {
        let mut out = Vec::new();
        let Some(pass) = self.techniques.get(self.tech).and_then(|t| t.passes.get(k)) else { return out };
        let (mut vs, mut ps) = (0, 0);
        for s in &pass.states {
            let before = out.len();
            self.state_calls(s, u32::MAX, &mut out);
            if let Some(Call::Dev(m, a)) = out.get(before) {
                match m {
                    92 => vs = a[0],
                    107 => ps = a[0],
                    _ => {}
                }
            }
        }
        self.const_calls(vs, true, &mut out);
        self.const_calls(ps, false, &mut out);
        out
    }
}

fn run(c: &mut Ctx, dev: u32, calls: Vec<Call>) {
    for call in calls {
        match call {
            Call::Dev(m, a) => {
                com_call(c, dev, m, &a);
            }
            Call::ConstF(vs, start, f) => {
                let n = f.len().div_ceil(4) as u32;
                let p = mem::heap_alloc(16 * n, true);
                for (i, x) in f.iter().enumerate() {
                    w32(p + 4 * i as u32, x.to_bits());
                }
                com_call(c, dev, if vs { 94 } else { 109 }, &[start, p, n]);
                mem::heap_free(p);
            }
            Call::ConstI(vs, start, v) | Call::ConstB(vs, start, v) if v.is_empty() => {
                let _ = (vs, start);
            }
            Call::ConstI(vs, start, v) => {
                let n = v.len().div_ceil(4) as u32;
                let p = mem::heap_alloc(16 * n, true);
                for (i, x) in v.iter().enumerate() {
                    w32(p + 4 * i as u32, *x);
                }
                com_call(c, dev, if vs { 96 } else { 111 }, &[start, p, n]);
                mem::heap_free(p);
            }
            Call::ConstB(vs, start, v) => {
                let p = mem::heap_alloc(4 * v.len() as u32, true);
                for (i, x) in v.iter().enumerate() {
                    w32(p + 4 * i as u32, (*x != 0) as u32);
                }
                com_call(c, dev, if vs { 98 } else { 113 }, &[start, p, v.len() as u32]);
                mem::heap_free(p);
            }
        }
    }
}

// ---- ID3DXEffect ----

fn this(c: &Ctx) -> u32 {
    c.a(0)
}
const D3DERR_INVALIDCALL: u32 = 0x8876086c;

/// ID3DXEffect::Release: at the last reference the effect's shaders, textures, saved state
/// block and device reference go, with its handles and strings.
fn fx_release(c: &mut Ctx) -> u32 {
    let o = c.a(0);
    let (n, last) = crate::d3dx9::release_count(o);
    if !last {
        return n;
    }
    let Some(e) = FX.lock().unwrap().get_or_insert_with(HashMap::default).remove(&o) else { return 0 };
    for p in e.strings.values().copied().chain([e.hbase]) {
        mem::heap_free(p);
    }
    mem::heap_free(o);
    for x in e.objects.iter().map(|x| x.created).chain([e.block, e.device]) {
        if x != 0 {
            com_call(c, x, 2, &[]);
        }
    }
    0
}
fn GetDesc(c: &mut Ctx) -> u32 {
    let d = c.a(1);
    with(this(c), |e| {
        w32(d, 0);
        w32(d + 4, e.top.len() as u32);
        w32(d + 8, e.techniques.len() as u32);
        w32(d + 12, 0);
    });
    0
}
/// D3DXPARAMETER_DESC: Name, Semantic, Class, Type, Rows, Columns, Elements, Annotations,
/// StructMembers, Flags, Bytes
fn GetParameterDesc(c: &mut Ctx) -> u32 {
    let (h, d) = (c.a(1), c.a(2));
    let r = with(this(c), |e| {
        let i = e.param_of(h)?;
        let p = e.params[i].clone();
        Some(p)
    })
    .flatten();
    let Some(p) = r else { return D3DERR_INVALIDCALL };
    let fx = this(c);
    let s = |x: &str| fx_str(fx, x.as_bytes());
    w32(d, s(&p.name));
    w32(d + 4, if p.semantic.is_empty() { 0 } else { s(&p.semantic) });
    let members = if p.class == PC_STRUCT && p.elements == 0 { p.children.len() as u32 } else { 0 };
    for (k, v) in [p.class, p.ty, p.rows, p.cols, p.elements, p.annotations.len() as u32, members, p.flags, p.bytes].iter().enumerate() {
        w32(d + 8 + 4 * k as u32, *v);
    }
    0
}
fn GetTechniqueDesc(c: &mut Ctx) -> u32 {
    let (h, d) = (c.a(1), c.a(2));
    let r = with(this(c), |e| e.tech_of(h).map(|t| (e.techniques[t].name.clone(), e.techniques[t].passes.len(), e.techniques[t].annotations.len()))).flatten();
    let Some((name, n, a)) = r else { return D3DERR_INVALIDCALL };
    w32(d, fx_str(this(c), name.as_bytes()));
    w32(d + 4, n as u32);
    w32(d + 8, a as u32);
    0
}
fn GetPassDesc(c: &mut Ctx) -> u32 {
    let (h, d) = (c.a(1), c.a(2));
    let r = with(this(c), |e| e.pass_of(h).map(|(t, k)| (e.techniques[t].passes[k].name.clone(), e.techniques[t].passes[k].annotations.len()))).flatten();
    let Some((name, a)) = r else { return D3DERR_INVALIDCALL };
    w32(d, fx_str(this(c), name.as_bytes()));
    w32(d + 4, a as u32);
    w32(d + 8, 0);
    w32(d + 12, 0);
    0
}
fn GetParameter(c: &mut Ctx) -> u32 {
    let (h, k) = (c.a(1), c.a(2) as usize);
    with(this(c), |e| {
        let list = match e.param_of(h) {
            Some(p) => e.params[p].children.clone(),
            None if h == 0 => e.top.clone(),
            None => return 0,
        };
        list.get(k).map_or(0, |&i| e.h_param(i))
    })
    .unwrap_or(0)
}
fn GetParameterByName(c: &mut Ctx) -> u32 {
    let (h, n) = (c.a(1), c.a(2));
    if n == 0 {
        return 0;
    }
    let name = crate::text::a2s(&mem::cstr(n));
    with(this(c), |e| {
        let parent = if h == 0 { None } else { Some(e.param_of(h)?) };
        e.by_name(parent, &name).map(|i| e.h_param(i))
    })
    .flatten()
    .unwrap_or(0)
}
fn GetParameterBySemantic(c: &mut Ctx) -> u32 {
    let (h, n) = (c.a(1), c.a(2));
    if n == 0 {
        return 0;
    }
    let sem = crate::text::a2s(&mem::cstr(n));
    with(this(c), |e| {
        let list = if h == 0 { e.top.clone() } else { e.params[e.param_of(h)?].children.clone() };
        list.into_iter().find(|&i| e.params[i].semantic.eq_ignore_ascii_case(&sem)).map(|i| e.h_param(i))
    })
    .flatten()
    .unwrap_or(0)
}
fn GetParameterElement(c: &mut Ctx) -> u32 {
    let (h, k) = (c.a(1), c.a(2) as usize);
    with(this(c), |e| {
        let p = e.param_of(h)?;
        (e.params[p].elements != 0).then(|| e.params[p].children.get(k).map(|&i| e.h_param(i))).flatten()
    })
    .flatten()
    .unwrap_or(0)
}
fn GetTechnique(c: &mut Ctx) -> u32 {
    let k = c.a(1) as usize;
    with(this(c), |e| if k < e.techniques.len() { e.h_tech(k) } else { 0 }).unwrap_or(0)
}
fn GetTechniqueByName(c: &mut Ctx) -> u32 {
    let n = c.a(1);
    with(this(c), |e| e.tech_of(n).map(|t| e.h_tech(t))).flatten().unwrap_or(0)
}
fn GetPass(c: &mut Ctx) -> u32 {
    let (h, k) = (c.a(1), c.a(2) as usize);
    with(this(c), |e| e.tech_of(h).filter(|&t| k < e.techniques[t].passes.len()).map(|t| e.h_pass(t, k))).flatten().unwrap_or(0)
}
fn GetPassByName(c: &mut Ctx) -> u32 {
    let (h, n) = (c.a(1), c.a(2));
    let name = crate::text::a2s(&mem::cstr(n));
    with(this(c), |e| {
        let t = e.tech_of(h)?;
        let k = e.techniques[t].passes.iter().position(|p| p.name == name)?;
        Some(e.h_pass(t, k))
    })
    .flatten()
    .unwrap_or(0)
}
fn GetAnnotation(c: &mut Ctx) -> u32 {
    let (h, k) = (c.a(1), c.a(2) as usize);
    with(this(c), |e| annotations_of(e, h).and_then(|a| a.get(k).map(|&i| e.h_param(i)))).flatten().unwrap_or(0)
}
fn GetAnnotationByName(c: &mut Ctx) -> u32 {
    let (h, n) = (c.a(1), c.a(2));
    let name = crate::text::a2s(&mem::cstr(n));
    with(this(c), |e| annotations_of(e, h).and_then(|a| a.into_iter().find(|&i| e.params[i].name == name).map(|i| e.h_param(i)))).flatten().unwrap_or(0)
}
fn annotations_of(e: &Effect, h: u32) -> Option<Vec<usize>> {
    if let Some(p) = e.param_of(h) {
        return Some(e.params[p].annotations.clone());
    }
    if let Some(t) = e.tech_of(h) {
        return Some(e.techniques[t].annotations.clone());
    }
    e.pass_of(h).map(|(t, k)| e.techniques[t].passes[k].annotations.clone())
}
fn SetValue(c: &mut Ctx) -> u32 {
    let (h, data, n) = (c.a(1), c.a(2), c.a(3));
    let tex = with(this(c), |e| {
        let p = e.param_of(h)?;
        if e.params[p].class == PC_OBJECT && is_texture(e.params[p].ty) {
            return Some(Some(p));
        }
        let src = mem::slice(data, n).to_vec();
        let v = e.value_mut(p);
        let k = v.len().min(src.len());
        v[..k].copy_from_slice(&src[..k]);
        Some(None)
    })
    .flatten();
    match tex {
        Some(Some(_)) => set_texture(c, h, r32(data)),
        Some(None) => 0,
        None => D3DERR_INVALIDCALL,
    }
}
fn GetValue(c: &mut Ctx) -> u32 {
    let (h, data, n) = (c.a(1), c.a(2), c.a(3));
    let v = with(this(c), |e| e.param_of(h).map(|p| e.value(p).to_vec())).flatten();
    let Some(v) = v else { return D3DERR_INVALIDCALL };
    let k = (n as usize).min(v.len());
    mem::write(data, &v[..k]);
    0
}
fn set_f(c: &mut Ctx, f: Vec<f32>) -> u32 {
    let h = c.a(1);
    with(this(c), |e| e.param_of(h).map(|p| e.set_floats(p, &f)).is_some()).unwrap_or(false).then_some(0).unwrap_or(D3DERR_INVALIDCALL)
}
fn get_f(c: &mut Ctx, out: u32, n: usize) -> u32 {
    let h = c.a(1);
    let r = with(this(c), |e| e.param_of(h).map(|p| (0..n.min(e.n_values(p))).map(|k| (e.f32_at(p, k), e.u32_at(p, k), e.params[p].ty)).collect::<Vec<_>>())).flatten();
    let Some(v) = r else { return D3DERR_INVALIDCALL };
    for (k, (f, _, _)) in v.iter().enumerate() {
        w32(out + 4 * k as u32, f.to_bits());
    }
    0
}
fn floats(p: u32, n: u32) -> Vec<f32> {
    (0..n).map(|k| f32::from_bits(r32(p + 4 * k))).collect()
}
fn SetBool(c: &mut Ctx) -> u32 {
    let v = (c.a(2) != 0) as u32 as f32;
    set_f(c, vec![v])
}
fn SetInt(c: &mut Ctx) -> u32 {
    let v = c.a(2) as i32 as f32;
    set_f(c, vec![v])
}
fn SetFloat(c: &mut Ctx) -> u32 {
    let v = f32::from_bits(c.a(2));
    set_f(c, vec![v])
}
fn SetBoolArray(c: &mut Ctx) -> u32 {
    let v = (0..c.a(3)).map(|k| (r32(c.a(2) + 4 * k) != 0) as u32 as f32).collect();
    set_f(c, v)
}
fn SetIntArray(c: &mut Ctx) -> u32 {
    let v = (0..c.a(3)).map(|k| r32(c.a(2) + 4 * k) as i32 as f32).collect();
    set_f(c, v)
}
fn SetFloatArray(c: &mut Ctx) -> u32 {
    let v = floats(c.a(2), c.a(3));
    set_f(c, v)
}
fn get_int(c: &mut Ctx, as_bool: bool) -> u32 {
    let (h, out) = (c.a(1), c.a(2));
    let r = with(this(c), |e| e.param_of(h).map(|p| e.f32_at(p, 0))).flatten();
    let Some(v) = r else { return D3DERR_INVALIDCALL };
    w32(out, if as_bool { (v != 0.0) as u32 } else { v as i32 as u32 });
    0
}
fn GetBool(c: &mut Ctx) -> u32 {
    get_int(c, true)
}
fn GetInt(c: &mut Ctx) -> u32 {
    get_int(c, false)
}
fn GetFloat(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    get_f(c, out, 1)
}
fn GetFloatArray(c: &mut Ctx) -> u32 {
    let (out, n) = (c.a(2), c.a(3));
    get_f(c, out, n as usize)
}
/// SetVector(param, D3DXVECTOR4*): as many components as the parameter has
fn SetVector(c: &mut Ctx) -> u32 {
    let v = floats(c.a(2), 4);
    set_f(c, v)
}
fn GetVector(c: &mut Ctx) -> u32 {
    let out = c.a(2);
    mem::fill(out, 16, 0);
    get_f(c, out, 4)
}
fn SetVectorArray(c: &mut Ctx) -> u32 {
    // (each element: its own component count of the 4)
    let (h, p, n) = (c.a(1), c.a(2), c.a(3));
    let ok = with(this(c), |e| {
        let q = e.param_of(h)?;
        let elems = if e.params[q].elements != 0 { e.params[q].children.clone() } else { vec![q] };
        for (k, el) in elems.into_iter().take(n as usize).enumerate() {
            e.set_floats(el, &floats(p + 16 * k as u32, 4));
        }
        Some(())
    })
    .flatten();
    if ok.is_some() { 0 } else { D3DERR_INVALIDCALL }
}
/// Matrices: the parameter's rows x columns of the 4x4 (transposed for the Transpose forms)
fn set_matrices(c: &mut Ctx, h: u32, mats: Vec<[f32; 16]>, transpose: bool) -> u32 {
    let ok = with(this(c), |e| {
        let q = e.param_of(h)?;
        let elems = if e.params[q].elements != 0 { e.params[q].children.clone() } else { vec![q] };
        for (el, m) in elems.into_iter().zip(mats) {
            let (rows, cols) = (e.params[el].rows, e.params[el].cols);
            let mut f = Vec::new();
            for r in 0..rows {
                for col in 0..cols {
                    f.push(if transpose { m[(col * 4 + r) as usize] } else { m[(r * 4 + col) as usize] });
                }
            }
            e.set_floats(el, &f);
        }
        Some(())
    })
    .flatten();
    if ok.is_some() { 0 } else { D3DERR_INVALIDCALL }
}
fn mat(p: u32) -> [f32; 16] {
    std::array::from_fn(|k| f32::from_bits(r32(p + 4 * k as u32)))
}
fn SetMatrix(c: &mut Ctx) -> u32 {
    let (h, m) = (c.a(1), mat(c.a(2)));
    set_matrices(c, h, vec![m], false)
}
fn SetMatrixTranspose(c: &mut Ctx) -> u32 {
    let (h, m) = (c.a(1), mat(c.a(2)));
    set_matrices(c, h, vec![m], true)
}
fn SetMatrixArray(c: &mut Ctx) -> u32 {
    let (h, p, n) = (c.a(1), c.a(2), c.a(3));
    set_matrices(c, h, (0..n).map(|k| mat(p + 64 * k)).collect(), false)
}
fn SetMatrixTransposeArray(c: &mut Ctx) -> u32 {
    let (h, p, n) = (c.a(1), c.a(2), c.a(3));
    set_matrices(c, h, (0..n).map(|k| mat(p + 64 * k)).collect(), true)
}
fn SetMatrixPointerArray(c: &mut Ctx) -> u32 {
    let (h, p, n) = (c.a(1), c.a(2), c.a(3));
    set_matrices(c, h, (0..n).map(|k| mat(r32(p + 4 * k))).collect(), false)
}
fn SetMatrixTransposePointerArray(c: &mut Ctx) -> u32 {
    let (h, p, n) = (c.a(1), c.a(2), c.a(3));
    set_matrices(c, h, (0..n).map(|k| mat(r32(p + 4 * k))).collect(), true)
}
fn get_matrix(c: &mut Ctx, transpose: bool) -> u32 {
    let (h, out) = (c.a(1), c.a(2));
    let r = with(this(c), |e| {
        let p = e.param_of(h)?;
        let (rows, cols) = (e.params[p].rows, e.params[p].cols);
        let mut m = [0f32; 16];
        for r in 0..rows.min(4) {
            for col in 0..cols.min(4) {
                let v = e.f32_at(p, (r * cols + col) as usize);
                m[if transpose { col * 4 + r } else { r * 4 + col } as usize] = v;
            }
        }
        Some(m)
    })
    .flatten();
    let Some(m) = r else { return D3DERR_INVALIDCALL };
    for (k, v) in m.iter().enumerate() {
        w32(out + 4 * k as u32, v.to_bits());
    }
    0
}
fn GetMatrix(c: &mut Ctx) -> u32 {
    get_matrix(c, false)
}
fn GetMatrixTranspose(c: &mut Ctx) -> u32 {
    get_matrix(c, true)
}
fn GetString(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(1), c.a(2));
    let r = with(this(c), |e| {
        let p = e.param_of(h)?;
        let o = e.objects.get(e.params[p].object? as usize)?;
        Some(o.data.clone())
    })
    .flatten();
    let Some(s) = r else { return D3DERR_INVALIDCALL };
    // (a string object's data may end in its NUL)
    let s = s.strip_suffix(&[0]).unwrap_or(&s);
    w32(out, fx_str(this(c), s));
    0
}
fn set_texture(c: &mut Ctx, h: u32, tex: u32) -> u32 {
    let old = with(this(c), |e| {
        let p = e.param_of(h)?;
        let id = e.params[p].object? as usize;
        let o = e.objects.get_mut(id)?;
        Some(std::mem::replace(&mut o.created, tex))
    })
    .flatten();
    let Some(old) = old else { return D3DERR_INVALIDCALL };
    if tex != 0 {
        com_call(c, tex, 1, &[]);
    }
    if old != 0 {
        com_call(c, old, 2, &[]);
    }
    0
}
fn SetTexture(c: &mut Ctx) -> u32 {
    let (h, t) = (c.a(1), c.a(2));
    set_texture(c, h, t)
}
fn get_object(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(1), c.a(2));
    let r = with(this(c), |e| {
        let p = e.param_of(h)?;
        e.objects.get(e.params[p].object? as usize).map(|o| o.created)
    })
    .flatten();
    let Some(o) = r else { return D3DERR_INVALIDCALL };
    if o != 0 {
        com_call(c, o, 1, &[]);
    }
    w32(out, o);
    0
}
fn SetTechnique(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    with(this(c), |e| e.tech_of(h).map(|t| e.tech = t)).flatten().map_or(D3DERR_INVALIDCALL, |_| 0)
}
fn GetCurrentTechnique(c: &mut Ctx) -> u32 {
    with(this(c), |e| if e.techniques.is_empty() { 0 } else { e.h_tech(e.tech) }).unwrap_or(0)
}
fn FindNextValidTechnique(c: &mut Ctx) -> u32 {
    let (h, out) = (c.a(1), c.a(2));
    let r = with(this(c), |e| {
        let next = if h == 0 { 0 } else { e.tech_of(h).map_or(usize::MAX, |t| t + 1) };
        if next < e.techniques.len() { e.h_tech(next) } else { 0 }
    })
    .unwrap_or(0);
    w32(out, r);
    if r == 0 { 1 } else { 0 }
}
fn IsParameterUsed(_: &mut Ctx) -> u32 {
    1
}
/// Begin(&passes, flags): saves the device's state (unless D3DXFX_DONOTSAVESTATE)
fn Begin(c: &mut Ctx) -> u32 {
    let (out, flags) = (c.a(1), c.a(2));
    let r = with(this(c), |e| (e.device, e.techniques.get(e.tech).map_or(0, |t| t.passes.len() as u32))).unwrap_or((0, 0));
    if out != 0 {
        w32(out, r.1);
    }
    if flags & 1 == 0 {
        let p = mem::heap_alloc(8, true);
        // CreateStateBlock(D3DSBT_ALL)
        if com_call(c, r.0, 59, &[1, p]) == 0 {
            let b = r32(p);
            with(this(c), |e| e.block = b);
        }
        mem::heap_free(p);
    }
    0
}
fn BeginPass(c: &mut Ctx) -> u32 {
    let k = c.a(1) as usize;
    let r = with(this(c), |e| {
        e.pass = Some(k);
        (e.device, e.pass_calls(k))
    });
    let Some((dev, calls)) = r else { return D3DERR_INVALIDCALL };
    run(c, dev, calls);
    0
}
fn CommitChanges(c: &mut Ctx) -> u32 {
    let r = with(this(c), |e| e.pass.map(|k| (e.device, e.pass_calls(k)))).flatten();
    if let Some((dev, calls)) = r {
        run(c, dev, calls);
    }
    0
}
fn EndPass(c: &mut Ctx) -> u32 {
    with(this(c), |e| e.pass = None);
    0
}
fn End(c: &mut Ctx) -> u32 {
    let b = with(this(c), |e| std::mem::take(&mut e.block)).unwrap_or(0);
    if b != 0 {
        // Apply, Release
        com_call(c, b, 5, &[]);
        com_call(c, b, 2, &[]);
    }
    0
}
fn GetDevice(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let d = with(this(c), |e| e.device).unwrap_or(0);
    if d != 0 {
        com_call(c, d, 1, &[]);
    }
    w32(out, d);
    0
}
fn out_null(c: &mut Ctx) -> u32 {
    if c.a(1) != 0 {
        w32(c.a(1), 0);
    }
    0
}
fn not_impl(_: &mut Ctx) -> u32 {
    0x8000_4001
}
fn SetRawValue(c: &mut Ctx) -> u32 {
    let (h, data, off, n) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let src = mem::slice(data, n).to_vec();
    with(this(c), |e| {
        let p = e.param_of(h)?;
        let v = e.value_mut(p);
        for (k, b) in src.iter().enumerate() {
            if let Some(x) = v.get_mut(off as usize + k) {
                *x = *b;
            }
        }
        Some(())
    })
    .flatten()
    .map_or(D3DERR_INVALIDCALL, |_| 0)
}

fn effect_vtable() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        let z = crate::d3dx9::zero as Handler;
        let m: Vec<(Handler, u32)> = vec![
            (qi_no, 12), (add_ref, 4), (fx_release, 4),
            (GetDesc, 8), (GetParameterDesc, 12), (GetTechniqueDesc, 12), (GetPassDesc, 12), (not_impl, 12),
            (GetParameter, 12), (GetParameterByName, 12), (GetParameterBySemantic, 12), (GetParameterElement, 12),
            (GetTechnique, 8), (GetTechniqueByName, 8), (GetPass, 12), (GetPassByName, 12), (z, 8), (z, 8),
            (GetAnnotation, 12), (GetAnnotationByName, 12),
            (SetValue, 16), (GetValue, 16), (SetBool, 12), (GetBool, 12), (SetBoolArray, 16), (not_impl, 16),
            (SetInt, 12), (GetInt, 12), (SetIntArray, 16), (not_impl, 16), (SetFloat, 12), (GetFloat, 12),
            (SetFloatArray, 16), (GetFloatArray, 16), (SetVector, 12), (GetVector, 12), (SetVectorArray, 16), (not_impl, 16),
            (SetMatrix, 12), (GetMatrix, 12), (SetMatrixArray, 16), (not_impl, 16), (SetMatrixPointerArray, 16), (not_impl, 16),
            (SetMatrixTranspose, 12), (GetMatrixTranspose, 12), (SetMatrixTransposeArray, 16), (not_impl, 16),
            (SetMatrixTransposePointerArray, 16), (not_impl, 16),
            (not_impl, 12), (GetString, 12), (SetTexture, 12), (get_object, 12), (get_object, 12), (get_object, 12), (z, 16),
            (out_null, 8), (SetTechnique, 8), (GetCurrentTechnique, 4), (z, 8), (FindNextValidTechnique, 12),
            (IsParameterUsed, 12), (Begin, 12), (BeginPass, 8), (CommitChanges, 4), (EndPass, 4), (End, 4),
            (GetDevice, 8), (z, 4), (z, 4), (z, 8), (out_null, 8), (z, 4), (z, 4), (z, 8), (z, 8), (not_impl, 12),
            (SetRawValue, 20),
        ];
        vtable("ID3DXEffect", &m)
    })
}

/// A new effect on `device` from compiled effect `data`, or the error.
pub fn create(c: &mut Ctx, device: u32, data: &[u8]) -> Result<u32, String> {
    let (params, top, values, techniques, mut objects) = parse(data)?;
    // the shaders
    let mut ctabs = HashMap::default();
    let mut pres_map = HashMap::default();
    for o in objects.iter_mut() {
        if o.data.len() < 8 || !matches!(o.ty, PT_VERTEXSHADER | PT_PIXELSHADER) {
            continue;
        }
        let code = mem::heap_alloc(o.data.len() as u32, false);
        mem::write(code, &o.data);
        let out = mem::heap_alloc(8, true);
        let m = if o.ty == PT_VERTEXSHADER { 91 } else { 106 };
        if com_call(c, device, m, &[code, out]) == 0 {
            o.created = r32(out);
            if let Some(p) = pres(&o.data) {
                pres_map.insert(o.created, p);
            }
            ctabs.insert(o.created, ctab(&o.data));
        }
        mem::heap_free(out);
        mem::heap_free(code);
    }
    let _ = PT_STRING;
    let nh = params.len() + techniques.len() + techniques.iter().map(|t| t.passes.len()).sum::<usize>();
    let hbase = mem::heap_alloc(4 * nh.max(1) as u32, true);
    let o = mem::heap_alloc(16, true);
    w32(o, effect_vtable());
    w32(o + 4, 1);
    com_call(c, device, 1, &[]);
    let e = Effect { device, params, top, values, techniques, objects, hbase, tech: 0, block: 0, pass: None, ctabs, pres: pres_map, strings: HashMap::default() };
    FX.lock().unwrap().get_or_insert_with(HashMap::default).insert(o, e);
    Ok(o)
}

#[cfg(test)]
mod tests {
    /// effect source compiled by crate::hlsl reads back with the right parameters, states and
    /// shaders
    #[test]
    fn compiled_source() {
        let src = r#"
float4x4 WorldViewProj : WORLDVIEWPROJECTION;
float Alpha <string UIName = "alpha"; float UIMin = 0.0;> = 0.5;
texture Tex;
sampler2D Samp = sampler_state { Texture = <Tex>; MinFilter = LINEAR; MagFilter = Linear; AddressU = Clamp; };
struct VO { float4 pos : POSITION; float2 uv : TEXCOORD0; };
VO vs(float4 p : POSITION, float2 uv : TEXCOORD0) { VO o; o.pos = mul(p, WorldViewProj); o.uv = uv; return o; }
float4 ps(float2 uv : TEXCOORD0, uniform float k) : COLOR { float4 c = tex2D(Samp, uv); c.a *= Alpha * k; return c; }
technique T <string Desc = "main";> {
    pass P0 {
        AlphaBlendEnable = TRUE; SrcBlend = SrcAlpha; DestBlend = InvSrcAlpha; ZEnable = false;
        VertexShader = compile vs_2_0 vs();
        PixelShader = compile ps_2_0 ps(2.0);
    }
    pass P1 { CullMode = None; VertexShader = compile vs_2_0 vs(); PixelShader = NULL; }
}
"#;
        let mut none = |_: &str, _: bool| None;
        let fx = hlslc::compile_effect(src, &[], &mut none, 0).unwrap();
        let (params, top, values, techs, objects) = super::parse(&fx).unwrap();
        let st = |s: &super::State| {
            let v = &values[&s.param];
            let k = match s.kind { super::Kind::Const => "c".to_string(), super::Kind::Ref(i) => params[i].name.clone(), super::Kind::Expr(_) => "expr".into(), super::Kind::Unsupported => "?".into() };
            (s.op, s.index, if let Some(o) = params[s.param].object { 100 + o } else { u32::from_le_bytes(v[..4].try_into().unwrap()) }, k)
        };
        let sts = |v: &[super::State]| v.iter().map(st).map(|(o, i, v, k)| format!("{o}.{i}={v}{k}")).collect::<Vec<_>>().join(" ");
        // AlphaBlendEnable, SrcBlend SrcAlpha, DestBlend InvSrcAlpha, ZEnable, shaders (object ids + 100)
        assert_eq!(sts(&techs[0].passes[0].states), "13.0=1c 6.0=5c 7.0=6c 0.0=0c 146.0=104c 147.0=105c");
        assert_eq!(sts(&techs[0].passes[1].states), "8.0=1c 146.0=106c 147.0=107c");
        // Texture <Tex>, MinFilter, MagFilter Linear, AddressU Clamp
        assert_eq!(sts(&params[top[3]].sampler), "164.0=102Tex 170.0=2c 169.0=2c 165.0=3c");
        let names: Vec<&str> = top.iter().map(|&i| params[i].name.as_str()).collect();
        assert_eq!(names, ["WorldViewProj", "Alpha", "Tex", "Samp"]);
        let alpha = &params[top[1]];
        assert_eq!(alpha.annotations.len(), 2);
        assert_eq!(params[top[0]].semantic, "WORLDVIEWPROJECTION");
        let samp = &params[top[3]];
        assert_eq!(samp.sampler.len(), 4);
        assert_eq!(techs.len(), 1);
        assert_eq!(techs[0].name, "T");
        assert_eq!(techs[0].annotations.len(), 1);
        assert_eq!(techs[0].passes.len(), 2);
        assert_eq!(techs[0].passes[0].states.len(), 6);
        assert_eq!(techs[0].passes[1].states.len(), 3);
        // two vertex shaders, a pixel shader, NULL: bytecode for the compiled ones
        let shaders: Vec<&super::Object> = objects.iter().filter(|o| o.data.starts_with(&[0, 2, 0xfe, 0xff]) || o.data.starts_with(&[0, 2, 0xff, 0xff])).collect();
        assert_eq!(shaders.len(), 3, "{:?}", objects.iter().map(|o| (o.ty, o.data.len())).collect::<Vec<_>>());
        for o in shaders {
            let words: Vec<u32> = o.data.chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
            assert!(dxwgsl::d3d9::Shader::parse(&words).is_some());
        }
    }
}

/// Effect lifetimes (review B-1): the names Get*Desc hand out are the effect's (one copy per
/// string, the same pointer every call), and its last Release frees them with its handles.
#[cfg(test)]
mod lifetime_tests {
    use super::*;
    use crate::test_guest::{call, enter};

    fn effect() -> u32 {
        let src = "float Alpha = 0.5; technique T { pass P0 { ZEnable = false; } pass P1 { CullMode = None; } }";
        let mut none = |_: &str, _: bool| None;
        let fx = hlslc::compile_effect(src, &[], &mut none, 0).unwrap();
        let (params, top, values, techniques, objects) = parse(&fx).unwrap();
        let hbase = mem::heap_alloc(64, true);
        let o = mem::heap_alloc(16, true);
        w32(o, effect_vtable());
        w32(o + 4, 1);
        let e = Effect { device: 0, params, top, values, techniques, objects, hbase, tech: 0, block: 0, pass: None, ctabs: HashMap::default(), pres: HashMap::default(), strings: HashMap::default() };
        FX.lock().unwrap().get_or_insert_with(HashMap::default).insert(o, e);
        o
    }

    #[test]
    fn desc_names_are_the_effects_and_go_with_it() {
        let _g = enter();
        let o = effect();
        let (ht, hp, hb, hpass) = with(o, |e| (e.h_tech(0), e.h_param(e.top[0]), e.hbase, e.h_pass(0, 1))).unwrap();
        let d = mem::heap_alloc(64, true);
        let mut seen = Vec::new();
        for _ in 0..50 {
            assert_eq!(call(GetTechniqueDesc, &[o, ht, d]), 0);
            let t = r32(d);
            assert_eq!(call(GetParameterDesc, &[o, hp, d]), 0);
            let p = r32(d);
            assert_eq!(call(GetPassDesc, &[o, hpass, d]), 0);
            let q = r32(d);
            seen.push((t, p, q));
        }
        let (t, p, q) = seen[0];
        assert!(seen.iter().all(|&x| x == seen[0]), "one copy per name");
        assert_eq!((mem::cstr(t), mem::cstr(p), mem::cstr(q)), (b"T".to_vec(), b"Alpha".to_vec(), b"P1".to_vec()));
        assert_eq!(call(add_ref, &[o]), 2);
        assert_eq!(call(fx_release, &[o]), 1);
        assert!(mem::heap_owns(t));
        assert_eq!(call(fx_release, &[o]), 0);
        assert!(with(o, |_| ()).is_none());
        for x in [t, p, q, hb, o] {
            assert!(!mem::heap_owns(x));
        }
        // (once too often: nothing)
        mem::heap_free(d);
    }
}
