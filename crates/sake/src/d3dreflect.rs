//! D3DReflect: ID3D11ShaderReflection (and its constant buffer, variable and type objects) read
//! from a DXBC blob's RDEF, ISGN/OSGN and SHDR/SHEX chunks. D3D10ReflectShader: the same
//! objects behind D3D10's interfaces (the same methods first, shorter descriptions:
//! D3D10_SHADER_DESC ends at GSMaxOutputVertexCount, a variable has no texture / sampler slots,
//! a type no name, a signature parameter no stream).

#![allow(non_snake_case)]

use std::sync::Mutex;

use crate::api::Ctx;
use crate::mem::{self, r32, w32};

#[derive(Clone, Default)]
struct TypeR {
    class: u32,
    ty: u32,
    rows: u32,
    cols: u32,
    elems: u32,
    members: Vec<(u32, u32)>, // (name ptr, type object)
    offset: u32,
    name: u32,
}

#[derive(Clone)]
struct VarR {
    name: String,
    name_p: u32,
    offset: u32,
    size: u32,
    flags: u32,
    ty: u32,
}

#[derive(Clone)]
struct CbR {
    name: String,
    name_p: u32,
    ty: u32,
    size: u32,
    flags: u32,
    vars: Vec<u32>,
}

#[derive(Clone)]
struct BindR {
    name: String,
    name_p: u32,
    vals: [u32; 7],
    space: u32,
}

/// RDEF's resource bindings: (name, D3D11 desc fields after Name, Space, uID). Shader model
/// 5.1's entries are 40 bytes with Space and uID after the 32 of before; before 5.1 a binding's
/// uID is its bind point and Space 0 (reflection.c's expected descs).
fn bind_entries(b: &[u8]) -> Vec<(String, [u32; 7], u32, u32)> {
    let (nb, boff) = (rd(b, 8) as usize, rd(b, 12) as usize);
    let sm51 = (rd(b, 16) & 0xffff) >= 0x501;
    let stride = if sm51 { 40 } else { 32 };
    (0..nb.min(256))
        .map(|i| {
            let at = boff + stride * i;
            let vals = [rd(b, at + 4), rd(b, at + 20), rd(b, at + 24), rd(b, at + 28), rd(b, at + 8), rd(b, at + 12), rd(b, at + 16)];
            let (space, uid) = if sm51 { (rd(b, at + 32), rd(b, at + 36)) } else { (0, vals[4]) };
            (cstr_at(b, rd(b, at) as usize), vals, space, uid)
        })
        .collect()
}

#[derive(Clone)]
struct SigR {
    name_p: u32,
    index: u32,
    reg: u32,
    sysval: u32,
    ctype: u32,
    mask: u8,
    rw: u8,
    stream: u32,
}

struct Refl {
    version: u32,
    creator: u32,
    flags: u32,
    cbs: Vec<u32>,
    binds: Vec<BindR>,
    ins: Vec<SigR>,
    outs: Vec<SigR>,
    insts: u32,
    temps: u32,
    threads: [u32; 3],
}

enum Obj {
    Refl(Refl),
    Cb(CbR),
    Var(VarR, u32),
    Type(TypeR),
}

static OBJS: Mutex<Vec<Obj>> = Mutex::new(Vec::new());

fn rd(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 4).map_or(0, |s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn rd16(b: &[u8], at: usize) -> u32 {
    b.get(at..at + 2).map_or(0, |s| u16::from_le_bytes([s[0], s[1]]) as u32)
}
fn cstr_at(b: &[u8], at: usize) -> String {
    let s = b.get(at..).unwrap_or(&[]);
    let e = s.iter().position(|&c| c == 0).unwrap_or(s.len());
    String::from_utf8_lossy(&s[..e]).into_owned()
}

fn gstr(s: &str) -> u32 {
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, s.as_bytes());
    p
}

fn vt(which: u8) -> u32 {
    use crate::api::Handler;
    static V: Mutex<[u32; 8]> = Mutex::new([0; 8]);
    let mut v = V.lock().unwrap();
    if v[which as usize] == 0 {
        let k = 4 * mem::ps() / 4;
        let _ = k;
        let t: Vec<(Handler, u32)> = match which {
            0 => vec![
                (refl_qi, 12),
                (refl_add_ref, 4),
                (refl_release, 4),
                (refl_desc, 8),
                (refl_cb_index, 8),
                (refl_cb_name, 8),
                (refl_bind, 12),
                (refl_in, 12),
                (refl_out, 12),
                (refl_patch, 12),
                (refl_var_name, 8),
                (refl_bind_name, 12),
                (zero, 4),
                (zero, 4),
                (zero, 4),
                (zero, 4),
                (zero, 4),
                (zero, 4),
                (zero, 4),
                (refl_level, 8),
                (refl_threads, 16),
                (zero, 4),
            ],
            1 | 5 => vec![(cb_desc, 8), (cb_var_index, 8), (cb_var_name, 8)],
            2 => vec![(var_desc, 8), (var_type, 4), (var_buffer, 4), (zero, 8)],
            // D3D10's
            4 => vec![(refl_qi, 12), (refl_add_ref, 4), (refl_release, 4), (refl_desc10, 8), (refl_cb_index, 8), (refl_cb_name, 8), (refl_bind, 12), (refl_in, 12), (refl_out, 12)],
            6 => vec![(var_desc10, 8), (var_type, 4)],
            7 => vec![(type_desc10, 8), (type_member, 8), (type_member_name, 8), (type_member_tname, 8)],
            _ => vec![
                (type_desc, 8),
                (type_member, 8),
                (type_member_name, 8),
                (type_member_tname, 8),
                (type_equal, 8),
                (zero, 4),
                (zero, 4),
                (zero, 4),
                (zero, 8),
                (type_equal, 8),
                (zero, 8),
            ],
        };
        let names = [
            "ID3D11ShaderReflection",
            "ID3D11ShaderReflectionConstantBuffer",
            "ID3D11ShaderReflectionVariable",
            "ID3D11ShaderReflectionType",
            "ID3D10ShaderReflection",
            "ID3D10ShaderReflectionConstantBuffer",
            "ID3D10ShaderReflectionVariable",
            "ID3D10ShaderReflectionType",
        ];
        v[which as usize] = crate::api::vtable(names[which as usize], &t);
    }
    v[which as usize]
}

/// A guest object {vtable, refs, index}.
fn new_obj(which: u8, o: Obj) -> u32 {
    let mut g = OBJS.lock().unwrap();
    g.push(o);
    let id = g.len() as u32 - 1;
    drop(g);
    let p = mem::heap_alloc(24, true);
    mem::wp(p, vt(which));
    w32(p + 8, 1);
    w32(p + 12, id);
    p
}

fn id(c: &Ctx) -> usize {
    r32(c.a(0) + 12) as usize
}

/// The interfaces `this` is behind: 0 D3D11's, 4 D3D10's (added to the object kind)
fn flavor(c: &Ctx) -> u8 {
    let v = mem::rp(c.a(0));
    if (4..8).any(|w| vt(w) == v) { 4 } else { 0 }
}

fn zero(_c: &mut Ctx) -> u32 {
    0
}

/// Struct fields as the guest lays them out: (pointer?, value), natural alignment.
fn put(at: u32, fields: &[(bool, u32)]) {
    let k = mem::ps();
    let mut o = 0u32;
    for &(p, v) in fields {
        if p {
            o = o.div_ceil(k) * k;
            mem::wp(at + o, v);
            o += k;
        } else {
            w32(at + o, v);
            o += 4;
        }
    }
}

fn parse_type(b: &[u8], at: usize, sm5: bool, fl: u8) -> u32 {
    let mut t = TypeR {
        class: rd16(b, at),
        ty: rd16(b, at + 2),
        rows: rd16(b, at + 4),
        cols: rd16(b, at + 6),
        elems: rd16(b, at + 8),
        ..Default::default()
    };
    let nm = rd16(b, at + 10) as usize;
    let moff = rd(b, at + 12) as usize;
    if sm5 {
        let n = rd(b, at + 32) as usize;
        if n != 0 {
            t.name = gstr(&cstr_at(b, n));
        }
    }
    for i in 0..nm.min(256) {
        let m = moff + 12 * i;
        let name = gstr(&cstr_at(b, rd(b, m) as usize));
        let mt = parse_type(b, rd(b, m + 4) as usize, sm5, fl);
        let off = rd(b, m + 8);
        if let Obj::Type(x) = &mut OBJS.lock().unwrap()[r32(mt + 12) as usize] {
            x.offset = off;
        }
        t.members.push((name, mt));
    }
    new_obj(3 + fl, Obj::Type(t))
}

fn signature(b: Option<&[u8]>) -> Vec<SigR> {
    let Some(b) = b else { return Vec::new() };
    let n = rd(b, 0) as usize;
    (0..n.min(128))
        .map(|i| {
            let at = 8 + 24 * i;
            SigR {
                name_p: gstr(&cstr_at(b, rd(b, at) as usize)),
                index: rd(b, at + 4),
                sysval: rd(b, at + 8),
                ctype: rd(b, at + 12),
                reg: rd(b, at + 16),
                mask: b.get(at + 20).copied().unwrap_or(0),
                rw: b.get(at + 21).copied().unwrap_or(0),
                stream: 0,
            }
        })
        .collect()
}

fn parse(code: &[u8], fl: u8) -> Option<Refl> {
    let cont = dxwgsl::dxbc::Container::parse(code).ok()?;
    let shdr = cont.chunk(b"SHEX").or_else(|| cont.chunk(b"SHDR"));
    let mut r = Refl {
        version: shdr.map_or(0, |s| rd(s, 0)),
        creator: gstr("sake"),
        flags: 0,
        cbs: Vec::new(),
        binds: Vec::new(),
        ins: signature(cont.chunk(b"ISGN")),
        outs: signature(cont.chunk(b"OSGN")),
        insts: 0,
        temps: 0,
        threads: [0; 3],
    };
    if let Some(s) = shdr {
        if let Ok(p) = dxwgsl::tpf::decode(s) {
            for i in &p.insts {
                match i.op {
                    0x68 => r.temps = i.extra.first().copied().unwrap_or(0),
                    0x9b => {
                        if i.extra.len() >= 3 {
                            r.threads = [i.extra[0], i.extra[1], i.extra[2]];
                        }
                    }
                    0x35 | 0x58..=0x6a | 0x8f..=0xa2 => {}
                    _ => r.insts += 1,
                }
            }
        }
    }
    if let Some(b) = cont.chunk(b"RDEF") {
        let ncb = rd(b, 0) as usize;
        let cboff = rd(b, 4) as usize;
        r.flags = rd(b, 20);
        r.creator = gstr(&cstr_at(b, rd(b, 24) as usize));
        let sm5 = (rd(b, 16) & 0xff00) >= 0x500;
        let vsize = if sm5 { 40 } else { 24 };
        for (name, vals, space, _uid) in bind_entries(b) {
            r.binds.push(BindR { name_p: gstr(&name), name, vals, space });
        }
        for i in 0..ncb.min(64) {
            let at = cboff + 24 * i;
            let name = cstr_at(b, rd(b, at) as usize);
            let nv = rd(b, at + 4) as usize;
            let voff = rd(b, at + 8) as usize;
            let mut cb = CbR { name_p: gstr(&name), name, ty: rd(b, at + 20), size: rd(b, at + 12), flags: rd(b, at + 16), vars: Vec::new() };
            let cb_obj = new_obj(1 + fl, Obj::Cb(cb.clone()));
            for k in 0..nv.min(1024) {
                let va = voff + vsize * k;
                let vname = cstr_at(b, rd(b, va) as usize);
                let ty = parse_type(b, rd(b, va + 16) as usize, sm5, fl);
                let v = VarR { name_p: gstr(&vname), name: vname, offset: rd(b, va + 4), size: rd(b, va + 8), flags: rd(b, va + 12), ty };
                cb.vars.push(new_obj(2 + fl, Obj::Var(v, cb_obj)));
            }
            OBJS.lock().unwrap()[r32(cb_obj + 12) as usize] = Obj::Cb(cb);
            r.cbs.push(cb_obj);
        }
    }
    Some(r)
}

/// D3DReflect(data, size, iid, reflector)
pub fn D3DReflect(c: &mut Ctx) -> u32 {
    let code = mem::slice(c.a(0), c.a(1)).to_vec();
    reflect(&code, 0, c.a(3))
}

/// D3D10ReflectShader(data, size, ID3D10ShaderReflection **)
pub fn D3D10ReflectShader(c: &mut Ctx) -> u32 {
    let code = mem::slice(c.a(0), c.a(1)).to_vec();
    reflect(&code, 4, c.a(2))
}

fn reflect(code: &[u8], fl: u8, out: u32) -> u32 {
    match parse(code, fl) {
        Some(r) => {
            mem::putp(out, new_obj(fl, Obj::Refl(r)));
            0
        }
        None => {
            mem::putp(out, 0);
            0x8007_0057
        }
    }
}

fn with_refl<T>(c: &Ctx, f: impl FnOnce(&Refl) -> T) -> Option<T> {
    match &OBJS.lock().unwrap()[id(c)] {
        Obj::Refl(r) => Some(f(r)),
        _ => None,
    }
}

fn refl_qi(c: &mut Ctx) -> u32 {
    refl_add_ref(c);
    mem::putp(c.a(2), c.a(0));
    0
}
fn refl_add_ref(c: &mut Ctx) -> u32 {
    let n = r32(c.a(0) + 8) + 1;
    w32(c.a(0) + 8, n);
    n
}
fn refl_release(c: &mut Ctx) -> u32 {
    let n = r32(c.a(0) + 8).saturating_sub(1);
    w32(c.a(0) + 8, n);
    n
}

fn refl_desc(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let Some(f) = with_refl(c, |r| {
        let mut v = vec![(false, r.version), (true, r.creator), (false, r.flags), (false, r.cbs.len() as u32), (false, r.binds.len() as u32)];
        v.push((false, r.ins.len() as u32));
        v.push((false, r.outs.len() as u32));
        v.push((false, r.insts));
        v.push((false, r.temps));
        // (the remaining counts and geometry/tessellation fields)
        v.extend(std::iter::repeat_n((false, 0), 29));
        v
    }) else {
        return 0x8000_4005;
    };
    put(out, &f);
    0
}
/// D3D10_SHADER_DESC: D3D11's up to GSMaxOutputVertexCount
fn refl_desc10(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    let Some(f) = with_refl(c, |r| {
        let mut v = vec![(false, r.version), (true, r.creator), (false, r.flags), (false, r.cbs.len() as u32), (false, r.binds.len() as u32)];
        v.extend([(false, r.ins.len() as u32), (false, r.outs.len() as u32), (false, r.insts), (false, r.temps)]);
        v.extend(std::iter::repeat_n((false, 0), 19));
        v
    }) else {
        return 0x8000_4005;
    };
    put(out, &f);
    0
}

/// The dummy objects Windows hands for what is not there (one of each kind and flavor)
static NULLS: Mutex<[u32; 8]> = Mutex::new([0; 8]);

/// Whether `p` is a dummy: its GetDesc fails (E_FAIL), and a dummy type's member type name is
/// "$Invalid" (reflection.c)
fn is_dummy(p: u32) -> bool {
    p != 0 && NULLS.lock().unwrap().contains(&p)
}

fn null_obj(which: u8) -> u32 {
    let mut n = NULLS.lock().unwrap();
    if n[which as usize] == 0 {
        let o = match which & 3 {
            1 => Obj::Cb(CbR { name: String::new(), name_p: 0, ty: 0, size: 0, flags: 0, vars: Vec::new() }),
            2 => Obj::Var(VarR { name: String::new(), name_p: 0, offset: 0, size: 0, flags: 0, ty: 0 }, 0),
            _ => Obj::Type(TypeR::default()),
        };
        n[which as usize] = new_obj(which, o);
    }
    n[which as usize]
}

fn refl_cb_index(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    with_refl(c, |r| r.cbs.get(i).copied()).flatten().unwrap_or_else(|| null_obj(1 + flavor(c)))
}
fn refl_cb_name(c: &mut Ctx) -> u32 {
    let n = crate::k32::arg_str(c.a(1), false);
    let cbs = with_refl(c, |r| r.cbs.clone()).unwrap_or_default();
    let g = OBJS.lock().unwrap();
    for o in cbs {
        if let Obj::Cb(cb) = &g[r32(o + 12) as usize] {
            if cb.name == n {
                return o;
            }
        }
    }
    drop(g);
    null_obj(1 + flavor(c))
}

fn put_bind(out: u32, b: &BindR) {
    put(out, &[(true, b.name_p), (false, b.vals[0]), (false, b.vals[1]), (false, b.vals[2]), (false, b.vals[3]), (false, b.vals[4]), (false, b.vals[5]), (false, b.vals[6])]);
    // x64's D3D11 desc has 4 bytes of padding where D3D12's Space is, and Windows writes it
    // (reflection.c: "Space field still gets the correct value on x64")
    if mem::x64() {
        w32(out + 36, b.space);
    }
}

fn refl_bind(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1) as usize, c.a(2));
    match with_refl(c, |r| r.binds.get(i).cloned()).flatten() {
        Some(b) => {
            put_bind(out, &b);
            0
        }
        None => 0x8007_0057,
    }
}
fn refl_bind_name(c: &mut Ctx) -> u32 {
    let (n, out) = (crate::k32::arg_str(c.a(1), false), c.a(2));
    match with_refl(c, |r| r.binds.iter().find(|b| b.name == n).cloned()).flatten() {
        Some(b) => {
            put_bind(out, &b);
            0
        }
        None => 0x8007_0057,
    }
}

/// D3D11_SIGNATURE_PARAMETER_DESC, or D3D10's (`d3d10`: no Stream, no MinPrecision)
fn put_sig(out: u32, s: &SigR, d3d10: bool) {
    put(out, &[(true, s.name_p), (false, s.index), (false, s.reg), (false, s.sysval), (false, s.ctype)]);
    let o = out + if mem::x64() { 24 } else { 20 };
    mem::w8(o, s.mask);
    mem::w8(o + 1, s.rw);
    if !d3d10 {
        mem::w16(o + 2, 0);
        w32(o + 4, s.stream);
        w32(o + 8, 0);
    }
}

fn refl_in(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1) as usize, c.a(2));
    match with_refl(c, |r| r.ins.get(i).cloned()).flatten() {
        Some(s) => {
            put_sig(out, &s, flavor(c) == 4);
            0
        }
        None => 0x8007_0057,
    }
}
fn refl_out(c: &mut Ctx) -> u32 {
    let (i, out) = (c.a(1) as usize, c.a(2));
    match with_refl(c, |r| r.outs.get(i).cloned()).flatten() {
        Some(s) => {
            put_sig(out, &s, flavor(c) == 4);
            0
        }
        None => 0x8007_0057,
    }
}
fn refl_patch(_c: &mut Ctx) -> u32 {
    0x8007_0057
}

fn refl_var_name(c: &mut Ctx) -> u32 {
    let n = crate::k32::arg_str(c.a(1), false);
    let cbs = with_refl(c, |r| r.cbs.clone()).unwrap_or_default();
    let g = OBJS.lock().unwrap();
    for o in cbs {
        if let Obj::Cb(cb) = &g[r32(o + 12) as usize] {
            for &v in &cb.vars {
                if let Obj::Var(x, _) = &g[r32(v + 12) as usize] {
                    if x.name == n {
                        return v;
                    }
                }
            }
        }
    }
    drop(g);
    null_obj(2 + flavor(c))
}

fn refl_level(c: &mut Ctx) -> u32 {
    let v = with_refl(c, |r| r.version).unwrap_or(0);
    let major = (v >> 4) & 0xf;
    w32(c.a(1), if major >= 5 { 0xb000 } else { 0xa000 });
    0
}
fn refl_threads(c: &mut Ctx) -> u32 {
    let t = with_refl(c, |r| r.threads).unwrap_or([0; 3]);
    for k in 0..3 {
        if c.a(1 + k) != 0 {
            w32(c.a(1 + k), t[k as usize]);
        }
    }
    t[0] * t[1] * t[2]
}

// ---- constant buffers, variables, types (no IUnknown: `this` is argument 0) ----

fn cb_desc(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return 0x8000_4005;
    }
    let g = OBJS.lock().unwrap();
    let Obj::Cb(cb) = &g[id(c)] else { return 0x8000_4005 };
    if cb.name_p == 0 {
        return 0x8000_4005;
    }
    let f = [(true, cb.name_p), (false, cb.ty), (false, cb.vars.len() as u32), (false, cb.size), (false, cb.flags)];
    drop(g);
    put(out, &f);
    0
}
fn cb_var_index(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    let g = OBJS.lock().unwrap();
    let r = match &g[id(c)] {
        Obj::Cb(cb) => cb.vars.get(i).copied(),
        _ => None,
    };
    drop(g);
    r.unwrap_or_else(|| null_obj(2 + flavor(c)))
}
fn cb_var_name(c: &mut Ctx) -> u32 {
    let n = crate::k32::arg_str(c.a(1), false);
    let g = OBJS.lock().unwrap();
    let mut r = None;
    if let Obj::Cb(cb) = &g[id(c)] {
        for &v in &cb.vars {
            if let Obj::Var(x, _) = &g[r32(v + 12) as usize] {
                if x.name == n {
                    r = Some(v);
                }
            }
        }
    }
    drop(g);
    r.unwrap_or_else(|| null_obj(2 + flavor(c)))
}

fn var_desc(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return 0x8000_4005;
    }
    let g = OBJS.lock().unwrap();
    let Obj::Var(v, _) = &g[id(c)] else { return 0x8000_4005 };
    if v.name_p == 0 {
        return 0x8000_4005;
    }
    let f = [(true, v.name_p), (false, v.offset), (false, v.size), (false, v.flags), (true, 0), (false, u32::MAX), (false, 0), (false, u32::MAX), (false, 0)];
    drop(g);
    put(out, &f);
    0
}
/// D3D10_SHADER_VARIABLE_DESC: Name, StartOffset, Size, uFlags, DefaultValue
fn var_desc10(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 {
        return 0x8000_4005;
    }
    let g = OBJS.lock().unwrap();
    let Obj::Var(v, _) = &g[id(c)] else { return 0x8000_4005 };
    if v.name_p == 0 {
        return 0x8000_4005;
    }
    let f = [(true, v.name_p), (false, v.offset), (false, v.size), (false, v.flags), (true, 0)];
    drop(g);
    put(out, &f);
    0
}
fn var_type(c: &mut Ctx) -> u32 {
    let g = OBJS.lock().unwrap();
    let t = match &g[id(c)] {
        Obj::Var(v, _) => v.ty,
        _ => 0,
    };
    drop(g);
    if t == 0 { null_obj(3 + flavor(c)) } else { t }
}
fn var_buffer(c: &mut Ctx) -> u32 {
    let g = OBJS.lock().unwrap();
    let b = match &g[id(c)] {
        Obj::Var(_, b) => *b,
        _ => 0,
    };
    drop(g);
    if b == 0 { null_obj(1 + flavor(c)) } else { b }
}

fn type_desc(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 || is_dummy(c.a(0)) {
        return 0x8000_4005;
    }
    let g = OBJS.lock().unwrap();
    let Obj::Type(t) = &g[id(c)] else { return 0x8000_4005 };
    let f = [
        (false, t.class),
        (false, t.ty),
        (false, t.rows),
        (false, t.cols),
        (false, t.elems),
        (false, t.members.len() as u32),
        (false, t.offset),
        (true, t.name),
    ];
    drop(g);
    put(out, &f);
    0
}
/// D3D10_SHADER_TYPE_DESC: D3D11's without the Name
fn type_desc10(c: &mut Ctx) -> u32 {
    let out = c.a(1);
    if out == 0 || is_dummy(c.a(0)) {
        return 0x8000_4005;
    }
    let g = OBJS.lock().unwrap();
    let Obj::Type(t) = &g[id(c)] else { return 0x8000_4005 };
    let f = [(false, t.class), (false, t.ty), (false, t.rows), (false, t.cols), (false, t.elems), (false, t.members.len() as u32), (false, t.offset)];
    drop(g);
    put(out, &f);
    0
}
fn type_member(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    let g = OBJS.lock().unwrap();
    let r = match &g[id(c)] {
        Obj::Type(t) => t.members.get(i).map(|m| m.1),
        _ => None,
    };
    drop(g);
    r.unwrap_or_else(|| null_obj(3 + flavor(c)))
}
fn type_member_name(c: &mut Ctx) -> u32 {
    let n = crate::k32::arg_str(c.a(1), false);
    let g = OBJS.lock().unwrap();
    let r = match &g[id(c)] {
        Obj::Type(t) => t.members.iter().find(|m| mem::cstr(m.0) == n.as_bytes()).map(|m| m.1),
        _ => None,
    };
    drop(g);
    r.unwrap_or_else(|| null_obj(3 + flavor(c)))
}
fn type_member_tname(c: &mut Ctx) -> u32 {
    let i = c.a(1) as usize;
    if is_dummy(c.a(0)) {
        static INVALID: Mutex<u32> = Mutex::new(0);
        let mut p = INVALID.lock().unwrap();
        if *p == 0 {
            *p = gstr("$Invalid");
        }
        return *p;
    }
    let g = OBJS.lock().unwrap();
    match &g[id(c)] {
        Obj::Type(t) => t.members.get(i).map_or(0, |m| m.0),
        _ => 0,
    }
}
fn type_equal(c: &mut Ctx) -> u32 {
    // S_OK when the same object, S_FALSE otherwise; E_FAIL asked of a dummy
    if is_dummy(c.a(0)) {
        return 0x8000_4005;
    }
    (c.a(0) != c.a(1)) as u32
}

#[cfg(test)]
mod tests {
    #[test]
    fn rdef_layout() {
        let src = "cbuffer A : register(b2) { float4x4 m; float3 v; float s; int k[2]; };\n\
                   Texture2D t; SamplerState ss;\n\
                   float4 main(float2 uv : TEXCOORD) : SV_Target { return t.Sample(ss, uv) * mul(float4(v, s), m) + k[1]; }";
        let mut inc = |_: &str, _: bool| None;
        let b = hlslc::compile(src, &[], &mut inc, "main", "ps_5_0", 0).unwrap();
        let c = dxwgsl::dxbc::Container::parse(&b).unwrap();
        let r = c.chunk(b"RDEF").unwrap();
        assert_eq!(super::rd(r, 0), 1); // one cbuffer
        assert_eq!(super::rd(r, 8), 3); // sampler, texture, cbuffer
        let cb = super::rd(r, 4) as usize;
        assert_eq!(super::cstr_at(r, super::rd(r, cb) as usize), "A");
        assert_eq!(super::rd(r, cb + 4), 4);
        assert_eq!(super::rd(r, cb + 12), 112);
        let vars = super::rd(r, cb + 8) as usize;
        // m at 0 (64), v at 64 (12), s at 76, k at 80 (k[0] 80, k[1] 96: size 20)
        let off: Vec<(u32, u32)> = (0..4).map(|i| (super::rd(r, vars + 40 * i + 4), super::rd(r, vars + 40 * i + 8))).collect();
        assert_eq!(off, [(0, 64), (64, 12), (76, 4), (80, 20)]);
    }

    /// A struct's type record lists its members (name, type, offset), nested structs too, in
    /// shader model 4 (16-byte types) and 5 (36-byte)
    #[test]
    fn rdef_struct_members() {
        let src = "struct In { float a; float2 b; }; struct L { float3 dir; float power; float4 color[2]; In inner; };\n\
                   cbuffer S { L light; };\n\
                   float4 main() : SV_Target { return light.color[1] * light.power + light.inner.b.x; }";
        for profile in ["ps_4_0", "ps_5_0"] {
            let mut inc = |_: &str, _: bool| None;
            let b = hlslc::compile(src, &[], &mut inc, "main", profile, 0).unwrap();
            let c = dxwgsl::dxbc::Container::parse(&b).unwrap();
            let r = c.chunk(b"RDEF").unwrap();
            let cb = super::rd(r, 4) as usize;
            let var = super::rd(r, cb + 8) as usize;
            let t = super::rd(r, var + 16) as usize;
            // class 5 (struct), 4 members
            assert_eq!((super::rd16(r, t), super::rd16(r, t + 10)), (5, 4), "{profile}");
            let m = super::rd(r, t + 12) as usize;
            let members: Vec<(String, u32)> = (0..4).map(|i| (super::cstr_at(r, super::rd(r, m + 12 * i) as usize), super::rd(r, m + 12 * i + 8))).collect();
            assert_eq!(members, [("dir".into(), 0), ("power".into(), 12), ("color".into(), 16), ("inner".into(), 48)], "{profile}");
            // color: a float4 vector, 2 elements; inner: a struct of 2
            let color = super::rd(r, m + 12 * 2 + 4) as usize;
            assert_eq!((super::rd16(r, color), super::rd16(r, color + 6), super::rd16(r, color + 8)), (1, 4, 2));
            let inner = super::rd(r, m + 12 * 3 + 4) as usize;
            assert_eq!((super::rd16(r, inner), super::rd16(r, inner + 10)), (5, 2));
            // a struct is one row of all its components: 3 + 1 + 8 + 3
            assert_eq!((super::rd16(r, t + 4), super::rd16(r, t + 6)), (1, 15), "{profile}");
        }
    }

    /// Windows' answers (d3d10refl.c): a register given is D3D_SIF_USERPACKED; the blob of
    /// D3D10CompileShader has elements 1 for what is not an array, no texture component flags,
    /// sample count 0, and SV_Target without a system value; D3DCompile's keeps them
    #[test]
    fn rdef_bindings_and_the_d3d10_blob() {
        let src = "struct L { float3 dir; float power; };
                   cbuffer S : register(b1) { L light; int mode[2]; };
                   Texture2D tex : register(t2); SamplerState smp; 
                   float4 main(float2 uv : TEXCOORD) : SV_Target { return tex.Sample(smp, uv) * light.power + mode[1]; }";
        for d3d10 in [false, true] {
            let mut inc = |_: &str, _: bool| None;
            let b = hlslc::compile(src, &[], &mut inc, "main", "ps_4_0", if d3d10 { hlslc::D3D10_BLOB } else { 0 }).unwrap();
            let c = dxwgsl::dxbc::Container::parse(&b).unwrap();
            let r = c.chunk(b"RDEF").unwrap();
            let (n, at) = (super::rd(r, 8) as usize, super::rd(r, 12) as usize);
            // name, type, flags, samples
            let binds: Vec<(String, u32, u32, u32)> =
                (0..n).map(|i| at + 32 * i).map(|e| (super::cstr_at(r, super::rd(r, e) as usize), super::rd(r, e + 4), super::rd(r, e + 28), super::rd(r, e + 16))).collect();
            let (tf, ts) = if d3d10 { (1, 0) } else { (0xd, u32::MAX) };
            assert_eq!(binds, [("smp".into(), 3, 0, 0), ("tex".into(), 2, tf, ts), ("S".into(), 0, 1, 0)], "d3d10 {d3d10}");
            let cb = super::rd(r, 4) as usize;
            let vars = super::rd(r, cb + 8) as usize;
            let elems: Vec<u32> = (0..2).map(|k| super::rd16(r, super::rd(r, vars + 24 * k + 16) as usize + 8)).collect();
            assert_eq!(elems, if d3d10 { [1, 2] } else { [0, 2] });
            let o = c.chunk(b"OSGN").unwrap();
            assert_eq!(super::rd(o, 8 + 8), if d3d10 { 0 } else { 64 });
        }
    }

    /// Shader model 5.1's bindings are 40 bytes (Space and uID follow), 5.0's and before 32
    #[test]
    fn sm51_bindings_are_40_bytes() {
        for (ver, stride) in [(0xffff_0501u32, 40usize), (0xffff_0500, 32), (0xffff_0400, 32)] {
            let at = 28usize;
            let names = at + 2 * stride;
            let mut b = vec![0u8; names + 8];
            let w = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
            w(&mut b, 8, 2);
            w(&mut b, 12, at as u32);
            w(&mut b, 16, ver);
            b[names..names + 7].copy_from_slice(b"sam\0tx\0");
            for (i, (n, ty, slot)) in [(names, 3u32, 0u32), (names + 4, 2, 5)].into_iter().enumerate() {
                let e = at + stride * i;
                w(&mut b, e, n as u32);
                w(&mut b, e + 4, ty);
                w(&mut b, e + 8, slot);
                if stride == 40 {
                    w(&mut b, e + 32, 7 + i as u32);
                    w(&mut b, e + 36, 0x10 + i as u32);
                }
            }
            let got: Vec<(String, u32, u32, u32, u32)> = super::bind_entries(&b).into_iter().map(|(n, v, s, u)| (n, v[0], v[4], s, u)).collect();
            let want = if stride == 40 { [(7, 0x10), (8, 0x11)] } else { [(0, 0), (0, 5)] };
            assert_eq!(got, [("sam".into(), 3, 0, want[0].0, want[0].1), ("tx".into(), 2, 5, want[1].0, want[1].1)], "{ver:#x}");
        }
    }

}
