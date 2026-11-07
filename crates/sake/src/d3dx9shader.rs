//! D3DX 9's shader functions on sake's HLSL compiler (crate::hlsl, shader model 2/3 output):
//! D3DXCompileShader (and from files), D3DXGetShaderConstantTable(Ex) and ID3DXConstantTable,
//! which sets a shader's constants by name through the device, and the small helpers
//! (D3DXGet{Pixel,Vertex}ShaderProfile, D3DXGetShaderSize/Version, D3DXFindShaderComment,
//! D3DXGetShaderSamplers, D3DXGetShaderInputSemantics/OutputSemantics).

#![allow(non_snake_case)]

use std::sync::Mutex;

use crate::api::{implement, vtable, Ctx, Handler};
use crate::d3dx9::{add_ref, buffer, com_call, qi_no, release};
use crate::mem::{self, r32, w32};

const INVALIDDATA: u32 = 0x8876_0b59;
const INVALIDCALL: u32 = 0x8876_086c;

/// The bytes of shader bytecode at `p` (through its end token).
fn shader_at(p: u32) -> Vec<u8> {
    if p == 0 {
        return Vec::new();
    }
    let ver = r32(p);
    let major = (ver >> 8) & 0xff;
    let mut i = 1u32;
    while i < 1 << 20 {
        let t = r32(p + 4 * i);
        if t == 0xffff {
            i += 1;
            break;
        }
        if t & 0xffff == 0xfffe {
            i += 1 + (t >> 16);
        } else if major >= 2 {
            i += 1 + ((t >> 24) & 0xf);
        } else {
            i += 1;
        }
    }
    mem::slice(p, 4 * i).to_vec()
}

// ---- the constant table ----

#[derive(Clone)]
struct K {
    name: String,
    /// D3DXREGISTER_SET: 0 bool, 1 int4, 2 float4, 3 sampler
    set: u32,
    reg: u32,
    count: u32,
    class: u32,
    ty: u32,
    rows: u32,
    cols: u32,
    elems: u32,
    members: Vec<usize>,
    elements: Vec<usize>,
    /// a guest copy of the name (descriptions)
    name_ptr: u32,
}

impl K {
    fn bytes(&self, t: &Table) -> u32 {
        if self.class == 5 {
            return self.members.iter().map(|&m| t.ks[m].bytes(t)).sum::<u32>() * self.elems.max(1);
        }
        4 * self.rows * self.cols * self.elems.max(1)
    }
}

struct Table {
    vs: bool,
    version: u32,
    ks: Vec<K>,
    top: Vec<usize>,
    /// handles: base + 4 * index
    base: u32,
    creator: u32,
}

static TABLES: Mutex<Vec<Table>> = Mutex::new(Vec::new());

/// registers one element of a type takes
fn regs_of(t: &Table, k: &K) -> u32 {
    match k.class {
        5 => k
            .members
            .iter()
            .map(|&m| regs_of(t, &t.ks[m]) * t.ks[m].elems.max(1))
            .sum(),
        2 => k.rows,
        3 => k.cols,
        _ => 1,
    }
}

/// Parses a CTAB body into a table.
fn parse(d: &[u8], vs: bool) -> Table {
    let r16 = |o: usize| {
        d.get(o..o + 2)
            .map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) as u32
    };
    let r32 = |o: usize| {
        d.get(o..o + 4)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let s = |o: usize| {
        String::from_utf8_lossy(
            d.get(o..)
                .unwrap_or(&[])
                .split(|&x| x == 0)
                .next()
                .unwrap_or(&[]),
        )
        .into_owned()
    };
    let mut t = Table {
        vs,
        version: r32(8),
        ks: Vec::new(),
        top: Vec::new(),
        base: 0,
        creator: 0,
    };
    let creator = s(r32(4) as usize);
    t.creator = guest_str(&creator);
    // (type info at `ti` for a constant named `name` at register `reg`)
    fn add(
        t: &mut Table,
        d: &[u8],
        name: String,
        set: u32,
        reg: u32,
        count: u32,
        ti: usize,
        depth: u32,
    ) -> usize {
        let r16 = |o: usize| {
            d.get(o..o + 2)
                .map_or(0, |b| u16::from_le_bytes([b[0], b[1]])) as u32
        };
        let r32 = |o: usize| {
            d.get(o..o + 4)
                .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
        };
        let s = |o: usize| {
            String::from_utf8_lossy(
                d.get(o..)
                    .unwrap_or(&[])
                    .split(|&x| x == 0)
                    .next()
                    .unwrap_or(&[]),
            )
            .into_owned()
        };
        let (class, ty, rows, cols, elems, nm, mi) = (
            r16(ti),
            r16(ti + 2),
            r16(ti + 4),
            r16(ti + 6),
            r16(ti + 8),
            r16(ti + 10),
            r32(ti + 12) as usize,
        );
        let idx = t.ks.len();
        t.ks.push(K {
            name: name.clone(),
            set,
            reg,
            count,
            class,
            ty,
            rows,
            cols,
            elems,
            members: Vec::new(),
            elements: Vec::new(),
            name_ptr: guest_str(&name),
        });
        if depth > 8 {
            return idx;
        }
        // members (of one element)
        let mut members = Vec::new();
        let mut at = reg;
        for m in 0..nm as usize {
            let mname = s(r32(mi + 8 * m) as usize);
            let mti = r32(mi + 8 * m + 4) as usize;
            let k = add(t, d, mname, set, at, 0, mti, depth + 1);
            let n = regs_of(t, &t.ks[k]) * t.ks[k].elems.max(1);
            t.ks[k].count = n;
            members.push(k);
            at += n;
        }
        t.ks[idx].members = members.clone();
        // elements of an array (each with its own members)
        if elems > 1 && depth < 4 {
            let per = regs_of(t, &t.ks[idx]);
            let mut els = Vec::new();
            for e in 0..elems.min(256) {
                let k = add(
                    t,
                    d,
                    format!("{name}[{e}]"),
                    set,
                    reg + e * per,
                    per,
                    ti,
                    depth + 1,
                );
                t.ks[k].elems = 1;
                els.push(k);
            }
            t.ks[idx].elements = els;
        }
        idx
    }
    let (n, info) = (r32(12) as usize, r32(16) as usize);
    for k in 0..n {
        let e = info + 20 * k;
        let i = add(
            &mut t,
            d,
            s(r32(e) as usize),
            r16(e + 4),
            r16(e + 6),
            r16(e + 8),
            r32(e + 12) as usize,
            0,
        );
        t.top.push(i);
    }
    t.base = mem::heap_alloc(4 * t.ks.len().max(1) as u32, true);
    t
}

fn guest_str(s: &str) -> u32 {
    let p = mem::heap_alloc(s.len() as u32 + 1, true);
    mem::write(p, s.as_bytes());
    p
}

fn table_vt() -> u32 {
    static V: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *V.get_or_init(|| {
        vtable(
            "ID3DXConstantTable",
            &[
                (qi_no, 12),
                (add_ref, 4),
                (release, 4),
                (ct_pointer, 4),
                (ct_size, 4),
                (ct_desc, 8),
                (ct_const_desc, 16),
                (ct_sampler_index, 8),
                (ct_constant, 12),
                (ct_by_name, 12),
                (ct_element, 12),
                (ct_defaults, 8),
                (ct_set_value, 20),
                (ct_set_bool, 16),
                (ct_set_bool_array, 20),
                (ct_set_int, 16),
                (ct_set_int_array, 20),
                (ct_set_float, 16),
                (ct_set_float_array, 20),
                (ct_set_vector, 16),
                (ct_set_vector_array, 20),
                (ct_set_matrix, 16),
                (ct_set_matrix_array, 20),
                (ct_set_matrix_pointer_array, 20),
                (ct_set_matrix_t, 16),
                (ct_set_matrix_t_array, 20),
                (ct_set_matrix_t_pointer_array, 20),
            ],
        )
    })
}

/// A new ID3DXConstantTable for shader bytecode; None without a CTAB.
fn new_table(code: &[u8]) -> Option<u32> {
    let (_, d) = crate::d3dx9fx::comments(code)
        .into_iter()
        .find(|(f, _)| f == b"CTAB")?;
    let vs = code.get(2..4) == Some(&[0xfe, 0xff]);
    let t = parse(d, vs);
    let data = mem::heap_alloc(d.len() as u32 + 4, true);
    mem::write(data, d);
    let o = mem::heap_alloc(20, true);
    w32(o, table_vt());
    w32(o + 4, 1);
    w32(o + 8, data);
    w32(o + 12, d.len() as u32);
    let mut tabs = TABLES.lock().unwrap();
    w32(o + 16, tabs.len() as u32);
    tabs.push(t);
    Some(o)
}

fn with<R>(this: u32, f: impl FnOnce(&Table) -> R) -> Option<R> {
    let tabs = TABLES.lock().unwrap();
    tabs.get(r32(this + 16) as usize).map(f)
}

/// The constant a handle names: one of ours, or a name ("a", "s.b", "a[2]") under `parent`.
fn lookup(t: &Table, parent: Option<usize>, h: u32) -> Option<usize> {
    if h == 0 {
        return None;
    }
    if h >= t.base && h < t.base + 4 * t.ks.len() as u32 && (h - t.base) % 4 == 0 {
        return Some(((h - t.base) / 4) as usize);
    }
    let name = String::from_utf8_lossy(&mem::cstr(h)).into_owned();
    by_name(t, parent, &name)
}

fn by_name(t: &Table, parent: Option<usize>, name: &str) -> Option<usize> {
    let (head, rest) = match name.find(['.', '[']) {
        Some(0) if name.starts_with('[') => {
            let close = name.find(']')?;
            let i: usize = name[1..close].trim().parse().ok()?;
            let k = *t.ks[parent?].elements.get(i)?;
            let r = name[close + 1..].trim_start_matches('.');
            return if r.is_empty() {
                Some(k)
            } else {
                by_name(t, Some(k), r)
            };
        }
        Some(i) => (&name[..i], &name[i..]),
        None => (name, ""),
    };
    let list = match parent {
        None => &t.top,
        Some(p) => &t.ks[p].members,
    };
    let k = *list.iter().find(|&&k| t.ks[k].name == head)?;
    let rest = rest.strip_prefix('.').unwrap_or(rest);
    if rest.is_empty() {
        Some(k)
    } else {
        by_name(t, Some(k), rest)
    }
}

fn handle(t: &Table, k: usize) -> u32 {
    t.base + 4 * k as u32
}

fn ct_pointer(c: &mut Ctx) -> u32 {
    r32(c.a(0) + 8)
}
fn ct_size(c: &mut Ctx) -> u32 {
    r32(c.a(0) + 12)
}
/// GetDesc(desc): {creator, version, constants}
fn ct_desc(c: &mut Ctx) -> u32 {
    let d = c.a(1);
    if d == 0 {
        return INVALIDCALL;
    }
    with(c.a(0), |t| {
        w32(d, t.creator);
        w32(d + 4, t.version);
        w32(d + 8, t.top.len() as u32);
    });
    0
}
/// GetConstantDesc(handle, desc, count)
fn ct_const_desc(c: &mut Ctx) -> u32 {
    let (h, d, n) = (c.a(1), c.a(2), c.a(3));
    with(c.a(0), |t| {
        let Some(k) = lookup(t, None, h) else {
            return INVALIDCALL;
        };
        let x = &t.ks[k];
        if n != 0 {
            w32(n, 1);
        }
        if d != 0 {
            let v = [
                x.name_ptr,
                x.set,
                x.reg,
                x.count,
                x.class,
                x.ty,
                x.rows,
                x.cols,
                x.elems.max(1),
                x.members.len() as u32,
                x.bytes(t),
                0,
            ];
            for (i, w) in v.iter().enumerate() {
                w32(d + 4 * i as u32, *w);
            }
        }
        0
    })
    .unwrap_or(INVALIDCALL)
}
fn ct_sampler_index(c: &mut Ctx) -> u32 {
    let h = c.a(1);
    with(c.a(0), |t| match lookup(t, None, h) {
        Some(k) if t.ks[k].set == 3 => t.ks[k].reg,
        _ => u32::MAX,
    })
    .unwrap_or(u32::MAX)
}
/// GetConstant(parent, index)
fn ct_constant(c: &mut Ctx) -> u32 {
    let (p, i) = (c.a(1), c.a(2) as usize);
    with(c.a(0), |t| {
        let list = if p == 0 {
            Some(&t.top)
        } else {
            lookup(t, None, p).map(|k| &t.ks[k].members)
        };
        list.and_then(|l| l.get(i)).map_or(0, |&k| handle(t, k))
    })
    .unwrap_or(0)
}
fn ct_by_name(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(1), c.a(2));
    if n == 0 {
        return 0;
    }
    let name = String::from_utf8_lossy(&mem::cstr(n)).into_owned();
    with(c.a(0), |t| {
        let parent = if p == 0 { None } else { lookup(t, None, p) };
        if p != 0 && parent.is_none() {
            return 0;
        }
        by_name(t, parent, &name).map_or(0, |k| handle(t, k))
    })
    .unwrap_or(0)
}
fn ct_element(c: &mut Ctx) -> u32 {
    let (h, i) = (c.a(1), c.a(2) as usize);
    with(c.a(0), |t| {
        let Some(k) = lookup(t, None, h) else {
            return 0;
        };
        match t.ks[k].elements.get(i) {
            Some(&e) => handle(t, e),
            // (element 0 of a non-array: itself)
            None if i == 0 && t.ks[k].elems <= 1 => handle(t, k),
            None => 0,
        }
    })
    .unwrap_or(0)
}
fn ct_defaults(_c: &mut Ctx) -> u32 {
    0
}

/// Register writes for constant `k` from values in its packed order (each element's rows by
/// columns, members in order); (set, register, values of 4 per register).
fn layout(
    t: &Table,
    k: usize,
    vals: &[f64],
    out: &mut Vec<(u32, u32, [f64; 4])>,
    used: &mut usize,
) {
    let x = &t.ks[k];
    let elems = x.elems.max(1);
    let per = regs_of(t, x);
    for e in 0..elems {
        let base = x.reg + e * per;
        if x.class == 5 {
            let ms = if x.elements.is_empty() {
                x.members.clone()
            } else {
                t.ks[x.elements[e as usize]].members.clone()
            };
            for m in ms {
                layout(t, m, vals, out, used);
            }
            continue;
        }
        let (r, cc) = (x.rows.max(1), x.cols.max(1));
        let mut regs = vec![[0f64; 4]; per as usize];
        let mut touched = vec![false; per as usize];
        for i in 0..r {
            for j in 0..cc {
                let Some(&v) = vals.get(*used) else { break };
                *used += 1;
                let (reg, comp) = match x.class {
                    3 => (j, i),
                    2 => (i, j),
                    _ => (0, j),
                };
                if (reg as usize) < regs.len() && comp < 4 {
                    regs[reg as usize][comp as usize] = v;
                    touched[reg as usize] = true;
                }
            }
        }
        for (n, v) in regs.iter().enumerate() {
            if touched[n] && (n as u32) < per && base + (n as u32) < x.reg + x.count.max(per) {
                out.push((x.set, base + n as u32, *v));
            }
        }
    }
}

/// Sets constant `h` (of `this`) on `dev` from values in packed order.
fn set(c: &mut Ctx, this: u32, dev: u32, h: u32, vals: Vec<f64>) -> u32 {
    let r = with(this, |t| {
        let k = lookup(t, None, h)?;
        let mut out = Vec::new();
        let mut used = 0;
        layout(t, k, &vals, &mut out, &mut used);
        Some((t.vs, out))
    })
    .flatten();
    let Some((vs, writes)) = r else {
        return INVALIDCALL;
    };
    if dev == 0 {
        return INVALIDCALL;
    }
    let p = mem::heap_alloc(16, true);
    for (s, reg, v) in writes {
        match s {
            0 => {
                w32(p, (v[0] != 0.0) as u32);
                com_call(c, dev, if vs { 98 } else { 113 }, &[reg, p, 1]);
            }
            1 => {
                for (i, x) in v.iter().enumerate() {
                    w32(p + 4 * i as u32, *x as i32 as u32);
                }
                com_call(c, dev, if vs { 96 } else { 111 }, &[reg, p, 1]);
            }
            2 => {
                for (i, x) in v.iter().enumerate() {
                    w32(p + 4 * i as u32, (*x as f32).to_bits());
                }
                com_call(c, dev, if vs { 94 } else { 109 }, &[reg, p, 1]);
            }
            _ => {}
        }
    }
    mem::heap_free(p);
    0
}

fn ints(p: u32, n: u32) -> Vec<f64> {
    (0..n.min(1 << 16))
        .map(|i| r32(p + 4 * i) as i32 as f64)
        .collect()
}
fn floats(p: u32, n: u32) -> Vec<f64> {
    (0..n.min(1 << 16))
        .map(|i| f32::from_bits(r32(p + 4 * i)) as f64)
        .collect()
}
fn bools(p: u32, n: u32) -> Vec<f64> {
    (0..n.min(1 << 16))
        .map(|i| (r32(p + 4 * i) != 0) as u32 as f64)
        .collect()
}

/// the packed component count of one element (rows x columns; vectors their columns)
fn comps_of(c: &Ctx, h: u32) -> (u32, u32, u32) {
    with(c.a(0), |t| {
        lookup(t, None, h).map(|k| (t.ks[k].rows.max(1), t.ks[k].cols.max(1), t.ks[k].ty))
    })
    .flatten()
    .unwrap_or((1, 4, 3))
}

/// SetValue(device, handle, data, bytes): data in the constant's own type
fn ct_set_value(c: &mut Ctx) -> u32 {
    let (dev, h, p, n) = (c.a(1), c.a(2), c.a(3), c.a(4));
    let ty = comps_of(c, h).2;
    let v = match ty {
        1 => bools(p, n / 4),
        2 => ints(p, n / 4),
        _ => floats(p, n / 4),
    };
    set(c, c.a(0), dev, h, v)
}
fn ct_set_bool(c: &mut Ctx) -> u32 {
    let v = vec![(c.a(3) != 0) as u32 as f64];
    set(c, c.a(0), c.a(1), c.a(2), v)
}
fn ct_set_bool_array(c: &mut Ctx) -> u32 {
    let v = bools(c.a(3), c.a(4));
    set(c, c.a(0), c.a(1), c.a(2), v)
}
fn ct_set_int(c: &mut Ctx) -> u32 {
    let v = vec![c.a(3) as i32 as f64];
    set(c, c.a(0), c.a(1), c.a(2), v)
}
fn ct_set_int_array(c: &mut Ctx) -> u32 {
    let v = ints(c.a(3), c.a(4));
    set(c, c.a(0), c.a(1), c.a(2), v)
}
fn ct_set_float(c: &mut Ctx) -> u32 {
    let v = vec![f32::from_bits(c.a(3)) as f64];
    set(c, c.a(0), c.a(1), c.a(2), v)
}
fn ct_set_float_array(c: &mut Ctx) -> u32 {
    let v = floats(c.a(3), c.a(4));
    set(c, c.a(0), c.a(1), c.a(2), v)
}
/// vectors of 4: the constant's components of each
fn vectors(c: &Ctx, h: u32, p: u32, n: u32) -> Vec<f64> {
    let (r, cc, _) = comps_of(c, h);
    let per = (r * cc).min(4);
    let mut v = Vec::new();
    for i in 0..n.min(1 << 14) {
        v.extend(floats(p + 16 * i, 4).into_iter().take(per as usize));
    }
    v
}
fn ct_set_vector(c: &mut Ctx) -> u32 {
    let v = vectors(c, c.a(2), c.a(3), 1);
    set(c, c.a(0), c.a(1), c.a(2), v)
}
fn ct_set_vector_array(c: &mut Ctx) -> u32 {
    let v = vectors(c, c.a(2), c.a(3), c.a(4));
    set(c, c.a(0), c.a(1), c.a(2), v)
}
/// 4x4 matrices (at pointers): the constant's rows x columns of each
fn matrices(c: &Ctx, h: u32, ptrs: &[u32], transpose: bool) -> Vec<f64> {
    let (r, cc, _) = comps_of(c, h);
    let mut v = Vec::new();
    for &p in ptrs {
        let m = floats(p, 16);
        for i in 0..r.min(4) {
            for j in 0..cc.min(4) {
                v.push(if transpose {
                    m[(j * 4 + i) as usize]
                } else {
                    m[(i * 4 + j) as usize]
                });
            }
        }
    }
    v
}
fn matrix_set(c: &mut Ctx, ptrs: Vec<u32>, transpose: bool) -> u32 {
    let v = matrices(c, c.a(2), &ptrs, transpose);
    set(c, c.a(0), c.a(1), c.a(2), v)
}
fn ct_set_matrix(c: &mut Ctx) -> u32 {
    matrix_set(c, vec![c.a(3)], false)
}
fn ct_set_matrix_array(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(3), c.a(4).min(4096));
    matrix_set(c, (0..n).map(|i| p + 64 * i).collect(), false)
}
fn ct_set_matrix_pointer_array(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(3), c.a(4).min(4096));
    matrix_set(c, (0..n).map(|i| r32(p + 4 * i)).collect(), false)
}
fn ct_set_matrix_t(c: &mut Ctx) -> u32 {
    matrix_set(c, vec![c.a(3)], true)
}
fn ct_set_matrix_t_array(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(3), c.a(4).min(4096));
    matrix_set(c, (0..n).map(|i| p + 64 * i).collect(), true)
}
fn ct_set_matrix_t_pointer_array(c: &mut Ctx) -> u32 {
    let (p, n) = (c.a(3), c.a(4).min(4096));
    matrix_set(c, (0..n).map(|i| r32(p + 4 * i)).collect(), true)
}

// ---- compiling ----

/// D3DXSHADER_PACKMATRIX_ROWMAJOR is D3DCOMPILE_PACK_MATRIX_ROW_MAJOR
#[allow(clippy::too_many_arguments)]
fn compile(
    c: &mut Ctx,
    src: &[u8],
    name: &str,
    defs: u32,
    inc: u32,
    entry: u32,
    target: u32,
    flags: u32,
    out: u32,
    errs: u32,
    table: u32,
) -> u32 {
    let src = String::from_utf8_lossy(src).into_owned();
    let src = src.trim_end_matches('\0');
    let entry = if entry == 0 {
        "main".to_string()
    } else {
        crate::k32::arg_str(entry, false)
    };
    let target = crate::k32::arg_str(target, false);
    let defines = crate::d3dcompiler::defines(defs);
    let base = crate::d3dcompiler::dir_of(name);
    let mut include = |n: &str, sys: bool| crate::d3dcompiler::open_include(c, inc, &base, n, sys);
    let r = hlslc::compile(
        src,
        &defines,
        &mut include,
        &entry,
        &target,
        flags & hlslc::PACK_MATRIX_ROW_MAJOR,
    );
    for p in [out, errs, table] {
        if p != 0 {
            w32(p, 0);
        }
    }
    match r {
        Ok(code) => {
            if out != 0 {
                w32(out, buffer(&code));
            }
            if table != 0 {
                w32(table, new_table(&code).unwrap_or(0));
            }
            0
        }
        Err(e) => {
            let msg = format!(
                "{}: error X3000: {e}",
                if name.is_empty() { "memory" } else { name }
            );
            eprintln!("sake: D3DXCompileShader({entry}, {target}): {msg}");
            if std::env::var_os("SAKE_HLSL_DUMP").is_some() {
                eprintln!("{src}");
            }
            if errs != 0 {
                w32(errs, buffer(format!("{msg}\0").as_bytes()));
            }
            0x8000_4005
        }
    }
}

/// D3DXCompileShader(data, length, defines, include, function, profile, flags, shader, errors,
/// constant table)
fn D3DXCompileShader(c: &mut Ctx) -> u32 {
    let src = mem::slice(c.a(0), c.a(1)).to_vec();
    let a: Vec<u32> = (2..10).map(|i| c.a(i)).collect();
    compile(c, &src, "", a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7])
}
/// D3DXCompileShaderFromFile{A,W}(file, defines, include, function, profile, flags, shader,
/// errors, constant table)
fn from_file(c: &mut Ctx, wide: bool) -> u32 {
    let name = crate::k32::arg_str(c.a(0), wide);
    let a: Vec<u32> = (1..9).map(|i| c.a(i)).collect();
    let Some(src) = crate::d3dcompiler::read_file(&name) else {
        for &p in &a[5..8] {
            if p != 0 {
                w32(p, 0);
            }
        }
        return 0x8007_0002;
    };
    compile(
        c, &src, &name, a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7],
    )
}
fn D3DXCompileShaderFromFileA(c: &mut Ctx) -> u32 {
    from_file(c, false)
}
fn D3DXCompileShaderFromFileW(c: &mut Ctx) -> u32 {
    from_file(c, true)
}

fn D3DXGetShaderConstantTable(c: &mut Ctx) -> u32 {
    let (code, out) = (shader_at(c.a(0)), c.a(1));
    get_table(&code, out)
}
fn D3DXGetShaderConstantTableEx(c: &mut Ctx) -> u32 {
    let (code, out) = (shader_at(c.a(0)), c.a(2));
    get_table(&code, out)
}
fn get_table(code: &[u8], out: u32) -> u32 {
    if out == 0 {
        return INVALIDCALL;
    }
    match new_table(code) {
        Some(t) => {
            w32(out, t);
            0
        }
        None => {
            w32(out, 0);
            INVALIDDATA
        }
    }
}

fn profile(s: &str) -> u32 {
    static P: Mutex<Vec<(String, u32)>> = Mutex::new(Vec::new());
    let mut p = P.lock().unwrap();
    if let Some((_, a)) = p.iter().find(|x| x.0 == s) {
        return *a;
    }
    let a = guest_str(s);
    p.push((s.to_string(), a));
    a
}
fn D3DXGetPixelShaderProfile(_c: &mut Ctx) -> u32 {
    profile("ps_3_0")
}
fn D3DXGetVertexShaderProfile(_c: &mut Ctx) -> u32 {
    profile("vs_3_0")
}
fn D3DXGetShaderSize(c: &mut Ctx) -> u32 {
    shader_at(c.a(0)).len() as u32
}
fn D3DXGetShaderVersion(c: &mut Ctx) -> u32 {
    if c.a(0) == 0 {
        0
    } else {
        r32(c.a(0))
    }
}
/// D3DXFindShaderComment(code, fourcc, data, size)
fn D3DXFindShaderComment(c: &mut Ctx) -> u32 {
    let (p, four, data, size) = (c.a(0), c.a(1), c.a(2), c.a(3));
    let code = shader_at(p);
    for (f, d) in crate::d3dx9fx::comments(&code) {
        if u32::from_le_bytes(f) == four {
            // (the body follows the fourcc in the guest's copy)
            let off = d.as_ptr() as usize - code.as_ptr() as usize;
            if data != 0 {
                w32(data, p + off as u32);
            }
            if size != 0 {
                w32(size, d.len() as u32);
            }
            return 0;
        }
    }
    1 // S_FALSE
}
/// D3DXGetShaderSamplers(code, names, count)
fn D3DXGetShaderSamplers(c: &mut Ctx) -> u32 {
    let (names, count) = (c.a(1), c.a(2));
    let code = shader_at(c.a(0));
    let Some((_, d)) = crate::d3dx9fx::comments(&code)
        .into_iter()
        .find(|(f, _)| f == b"CTAB")
    else {
        if count != 0 {
            w32(count, 0);
        }
        return 0;
    };
    let t = parse(d, false);
    let mut n = 0;
    for &k in &t.top {
        if t.ks[k].set == 3 {
            if names != 0 {
                w32(names + 4 * t.ks[k].reg, t.ks[k].name_ptr);
            }
            n = n.max(t.ks[k].reg + 1);
        }
    }
    if count != 0 {
        w32(count, n);
    }
    0
}
/// D3DXGetShaderInputSemantics / OutputSemantics(code, semantics {usage, index}, count): the
/// declared registers
fn semantics(c: &mut Ctx, output: bool) -> u32 {
    let (out, count) = (c.a(1), c.a(2));
    let code = shader_at(c.a(0));
    let w = |i: usize| {
        code.get(4 * i..4 * i + 4)
            .map_or(0, |b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let ver = w(0);
    let pixel = ver >> 16 == 0xffff;
    let major = (ver >> 8) & 0xff;
    let mut n = 0u32;
    let mut i = 1;
    while 4 * i < code.len() {
        let t = w(i);
        if t == 0xffff {
            break;
        }
        let len = if t & 0xffff == 0xfffe {
            (t >> 16) as usize
        } else if major >= 2 {
            ((t >> 24) & 0xf) as usize
        } else {
            0
        };
        if t & 0xffff == 31 && major >= 2 {
            let (u, r) = (w(i + 1), w(i + 2));
            let ty = ((r >> 28) & 7) | ((r >> 8) & 0x18);
            let num = r & 0x7ff;
            let sem = match (ty, output, pixel) {
                (1, false, false) => Some((u & 0x1f, (u >> 16) & 0xf)),
                (1, false, true) if major >= 3 => Some((u & 0x1f, (u >> 16) & 0xf)),
                (1, false, true) => Some((10, num)),
                (3, false, true) => Some((5, num)),
                (6, true, false) if major >= 3 => Some((u & 0x1f, (u >> 16) & 0xf)),
                _ => None,
            };
            if let Some((usage, index)) = sem {
                if out != 0 {
                    w32(out + 8 * n, usage);
                    w32(out + 8 * n + 4, index);
                }
                n += 1;
            }
        }
        i += 1 + len;
    }
    if count != 0 {
        w32(count, n);
    }
    0
}
fn D3DXGetShaderInputSemantics(c: &mut Ctx) -> u32 {
    semantics(c, false)
}
fn D3DXGetShaderOutputSemantics(c: &mut Ctx) -> u32 {
    semantics(c, true)
}

pub fn init() {
    let fns: &[(&str, Handler)] = &[
        ("D3DXCompileShader", D3DXCompileShader),
        ("D3DXCompileShaderFromFileA", D3DXCompileShaderFromFileA),
        ("D3DXCompileShaderFromFileW", D3DXCompileShaderFromFileW),
        ("D3DXGetShaderConstantTable", D3DXGetShaderConstantTable),
        ("D3DXGetShaderConstantTableEx", D3DXGetShaderConstantTableEx),
        ("D3DXGetPixelShaderProfile", D3DXGetPixelShaderProfile),
        ("D3DXGetVertexShaderProfile", D3DXGetVertexShaderProfile),
        ("D3DXGetShaderSize", D3DXGetShaderSize),
        ("D3DXGetShaderVersion", D3DXGetShaderVersion),
        ("D3DXFindShaderComment", D3DXFindShaderComment),
        ("D3DXGetShaderSamplers", D3DXGetShaderSamplers),
        ("D3DXGetShaderInputSemantics", D3DXGetShaderInputSemantics),
        ("D3DXGetShaderOutputSemantics", D3DXGetShaderOutputSemantics),
    ];
    for v in 24..=43 {
        let dll = format!("d3dx9_{v}");
        for (n, f) in fns {
            implement(&dll, n, *f);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn layout() {
        // a ctab of our compiler: names, sets, registers; elements and members by name
        let src = "struct L { float3 dir; float4 col; }; L lights[2]; float4x4 wvp; float4 tint : register(c20);\n\
                   sampler2D tex;\n\
                   float4 main(float2 uv : TEXCOORD0) : COLOR { return tex2D(tex, uv) * tint + mul(float4(uv, 0, 1), wvp) + lights[1].col + lights[0].dir.x; }";
        let mut inc = |_: &str, _: bool| None;
        let code = hlslc::compile(src, &[], &mut inc, "main", "ps_3_0", 0).unwrap();
        let (_, d) = crate::d3dx9fx::comments(&code)
            .into_iter()
            .find(|(f, _)| f == b"CTAB")
            .unwrap();
        // (parse without guest memory: names only)
        let names = super::ctab_names(d);
        assert!(names.contains(&("tint".to_string(), 2, 20, 1)), "{names:?}");
        assert!(names.contains(&("tex".to_string(), 3, 0, 1)), "{names:?}");
        assert!(
            names.iter().any(|n| n.0 == "lights" && n.3 == 4),
            "{names:?}"
        );
        assert!(names.iter().any(|n| n.0 == "wvp" && n.3 == 4), "{names:?}");
    }
}

/// (tests) the top-level entries of a CTAB body: name, set, register, count
#[cfg(test)]
fn ctab_names(d: &[u8]) -> Vec<(String, u32, u32, u32)> {
    let r16 = |o: usize| u16::from_le_bytes([d[o], d[o + 1]]) as u32;
    let r32 = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let s =
        |o: usize| String::from_utf8_lossy(d[o..].split(|&x| x == 0).next().unwrap()).into_owned();
    (0..r32(12) as usize)
        .map(|k| {
            let e = r32(16) as usize + 20 * k;
            (s(r32(e) as usize), r16(e + 4), r16(e + 6), r16(e + 8))
        })
        .collect()
}
