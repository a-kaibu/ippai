//! Direct3D 10/11 shaders (DXBC) to WGSL: vkd3d-shader makes SPIR-V (host/vkd3d/vnshader.c,
//! descriptor set = stage, bindings by register: b<n> n, t<n> 16 + n, s<n> 160 + n), naga makes
//! WGSL of it. What the page needs to bind comes from naga's module.

use std::ffi::{c_char, c_void, CStr};

extern "C" {
    fn vn_shader_to_spirv(
        code: *const c_void,
        size: usize,
        source: i32,
        stage: i32,
        ps: *const c_void,
        ps_size: usize,
        out: *mut *mut c_void,
        out_size: *mut usize,
        log: *mut *mut c_char,
    ) -> i32;
    fn vn_shader_signature(code: *const c_void, size: usize, which: i32, out: *mut SigElement, max: i32) -> i32;
    fn vn_shader_free(p: *mut c_void);
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SigElement {
    name: [u8; 32],
    pub index: u32,
    pub reg: u32,
    pub sysval: u32,
    pub component_type: u32,
    pub mask: u32,
}

impl SigElement {
    pub fn name(&self) -> &str {
        let n = self.name.iter().position(|&c| c == 0).unwrap_or(32);
        std::str::from_utf8(&self.name[..n]).unwrap_or("")
    }
}

/// A shader's input (`output` false) or output signature.
pub fn signature(code: &[u8], output: bool) -> Vec<SigElement> {
    let mut v = vec![SigElement { name: [0; 32], index: 0, reg: 0, sysval: 0, component_type: 0, mask: 0 }; 64];
    let n = unsafe { vn_shader_signature(code.as_ptr() as _, code.len(), output as i32, v.as_mut_ptr(), 64) };
    v.truncate(n.max(0) as usize);
    v
}

/// What a shader binds (group = stage)
#[derive(Clone, Debug, PartialEq)]
pub enum Binding {
    /// constant buffer: size the shader reads
    Uniform(u32),
    /// texture: view dimension (0 1d, 1 2d, 2 2d array, 3 cube, 4 cube array, 5 3d), sample
    /// type (0 float, 1 uint, 2 sint, 3 depth)
    Texture(u32, u32),
    Sampler(bool),
}

pub struct Wgsl {
    pub code: String,
    /// (binding, what)
    pub bindings: Vec<(u32, Binding)>,
}

/// DXBC to WGSL; `ps`: the pixel shader a vertex shader feeds (its outputs go to that shader's
/// input registers).
pub fn translate(code: &[u8], pixel: bool, ps: Option<&[u8]>) -> Result<Wgsl, String> {
    let spv = unsafe {
        let (mut out, mut n, mut log) = (std::ptr::null_mut(), 0usize, std::ptr::null_mut());
        let (pp, pn) = ps.map_or((std::ptr::null(), 0), |p| (p.as_ptr() as *const c_void, p.len()));
        let r = vn_shader_to_spirv(code.as_ptr() as _, code.len(), 0, pixel as i32, pp, pn, &mut out, &mut n, &mut log);
        let msg = if log.is_null() { String::new() } else { CStr::from_ptr(log).to_string_lossy().into_owned() };
        if !log.is_null() {
            vn_shader_free(log as _);
        }
        if r != 0 {
            return Err(format!("vkd3d-shader {r}: {msg}"));
        }
        let v = std::slice::from_raw_parts(out as *const u8, n).to_vec();
        vn_shader_free(out);
        v
    };
    let spv = buffers_to_2d(spv);
    if let Ok(dir) = std::env::var("VNVM_D3D_SPV") {
        // (debugging: the SPIR-V naga gets, by content hash)
        let h = spv.iter().fold(0u32, |h, &b| h.wrapping_mul(31).wrapping_add(b as u32));
        let _ = std::fs::write(format!("{dir}/{h:08x}.spv"), &spv);
    }
    let opts = naga::front::spv::Options { adjust_coordinate_space: false, strict_capabilities: false, block_ctx_dump_prefix: None };
    let mut m = naga::front::spv::parse_u8_slice(&spv, &opts).map_err(|e| format!("naga spv-in: {e}"))?;
    drop_point_size(&mut m);
    let info = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&m)
        .map_err(|e| format!("naga validation: {e:?}"))?;
    let text = naga::back::wgsl::write_string(&m, &info, naga::back::wgsl::WriterFlags::empty())
        .map_err(|e| format!("naga wgsl-out: {e}"))?;
    let mut bindings = Vec::new();
    for (_, g) in m.global_variables.iter() {
        let Some(rb) = &g.binding else { continue };
        let b = match (&m.types[g.ty].inner, g.space) {
            (_, naga::AddressSpace::Uniform) => {
                let size = m.types[g.ty].inner.size(m.to_ctx());
                Binding::Uniform(size)
            }
            (naga::TypeInner::Image { dim, arrayed, class }, _) => {
                use naga::ImageDimension as D;
                let vd = match (dim, arrayed) {
                    (D::D1, _) => 0,
                    (D::D2, false) => 1,
                    (D::D2, true) => 2,
                    (D::Cube, false) => 3,
                    (D::Cube, true) => 4,
                    (D::D3, _) => 5,
                };
                let st = match class {
                    naga::ImageClass::Sampled { kind: naga::ScalarKind::Uint, .. } => 1,
                    naga::ImageClass::Sampled { kind: naga::ScalarKind::Sint, .. } => 2,
                    naga::ImageClass::Depth { .. } => 3,
                    _ => 0,
                };
                Binding::Texture(vd, st)
            }
            (naga::TypeInner::Sampler { comparison }, _) => Binding::Sampler(*comparison),
            _ => continue,
        };
        bindings.push((rb.binding, b));
    }
    // a vertex shader's outputs interpolate as the pixel shader's inputs do (D3D matches them
    // by register; WebGPU wants the same @interpolate on both sides)
    let text = match ps {
        Some(p) if !pixel => match translate(p, true, None) {
            Ok(pw) => match_interpolation(&text, &pw.code),
            Err(_) => text,
        },
        _ => text,
    };
    // (D3D samples anywhere: WGSL's uniformity rules would reject some of these shaders)
    let code = format!("diagnostic(off, derivative_uniformity);\n{text}");
    Ok(Wgsl { code, bindings })
}

/// `vs` (WGSL) with the @interpolate attributes the pixel shader `ps` gives its inputs, on the
/// vertex shader's output locations (the members of the struct its entry point returns).
fn match_interpolation(vs: &str, ps: &str) -> String {
    let mut want = std::collections::HashMap::new();
    for (i, _) in ps.match_indices("@location(") {
        let rest = &ps[i + 10..];
        let Some(end) = rest.find(')') else { continue };
        let after = rest[end + 1..].trim_start();
        if after.starts_with("@interpolate(") {
            if let Some(e) = after.find(')') {
                want.insert(rest[..end].to_string(), after[..e + 1].to_string());
            }
        }
    }
    if want.is_empty() {
        return vs.to_string();
    }
    // the entry point's result type
    let Some(ret) = vs.find("@vertex").and_then(|i| vs[i..].find("->").map(|j| i + j + 2)) else { return vs.to_string() };
    let ty: String = vs[ret..].trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
    let Some(start) = vs.find(&format!("struct {ty} {{")) else { return vs.to_string() };
    let Some(len) = vs[start..].find("\n}") else { return vs.to_string() };
    let mut body = vs[start..start + len].to_string();
    for (loc, interp) in &want {
        let at = format!("@location({loc}) ");
        if let Some(i) = body.find(&at) {
            if !body[i + at.len()..].starts_with("@interpolate") {
                body.insert_str(i + at.len(), &format!("{interp} "));
            }
        }
    }
    format!("{}{}{}", &vs[..start], body, &vs[start + len..])
}

/// What WGSL cannot say, rewritten in the SPIR-V.
/// - No texel buffers (naga rejects SPIR-V's Buffer images): each Buffer image type becomes a
///   2D one and each fetch from it reads texel (x, 0) at level 0; buffer views are one-row
///   textures (mod.rs create_buffer_srv).
/// - No NaN or infinity literals, while vkd3d keeps its registers as floats and so writes
///   integer constants such as the all-ones "true" (0xffffffff) as float constants. Such a
///   constant (and a composite holding one) becomes a value the function makes at its start:
///   the bits from a private u32 variable, bitcast (a load, so nothing folds it back into a
///   literal). Used by more than one function, it falls back to a huge finite float.
fn buffers_to_2d(spv: Vec<u8>) -> Vec<u8> {
    const OP_NAME: u32 = 5;
    const OP_MEMBER_NAME: u32 = 6;
    const OP_LINE: u32 = 8;
    const OP_CAPABILITY: u32 = 17;
    const OP_TYPE_INT: u32 = 21;
    const OP_TYPE_FLOAT: u32 = 22;
    const OP_TYPE_VECTOR: u32 = 23;
    const OP_TYPE_IMAGE: u32 = 25;
    const OP_TYPE_POINTER: u32 = 32;
    const OP_CONSTANT: u32 = 43;
    const OP_CONSTANT_COMPOSITE: u32 = 44;
    const OP_FUNCTION: u32 = 54;
    const OP_FUNCTION_END: u32 = 56;
    const OP_VARIABLE: u32 = 59;
    const OP_LOAD: u32 = 61;
    const OP_DECORATE: u32 = 71;
    const OP_MEMBER_DECORATE: u32 = 72;
    const OP_COMPOSITE_CONSTRUCT: u32 = 80;
    const OP_IMAGE_FETCH: u32 = 95;
    const OP_BITCAST: u32 = 124;
    const OP_LABEL: u32 = 248;
    const PRIVATE: u32 = 6;
    if spv.len() < 20 || spv.len() % 4 != 0 {
        return spv;
    }
    let w: Vec<u32> = spv.chunks_exact(4).map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
    // instructions: (start, word count, opcode)
    let mut ins = Vec::new();
    let mut i = 5;
    while i < w.len() {
        let (n, op) = ((w[i] >> 16) as usize, w[i] & 0xffff);
        if n == 0 || i + n > w.len() {
            return spv;
        }
        ins.push((i, n, op));
        i += n;
    }
    let buffers: Vec<u32> = ins.iter().filter(|&&(i, n, op)| op == OP_TYPE_IMAGE && n >= 9 && w[i + 3] == 5).map(|&(i, ..)| w[i + 1]).collect();
    let floats: Vec<u32> = ins.iter().filter(|&&(i, n, op)| op == OP_TYPE_FLOAT && n == 3 && w[i + 2] == 32).map(|&(i, ..)| w[i + 1]).collect();
    // the non-finite float constants: id -> bits
    let nonfinite: std::collections::HashMap<u32, u32> = ins
        .iter()
        .filter(|&&(i, n, op)| op == OP_CONSTANT && n == 4 && floats.contains(&w[i + 1]) && w[i + 3] & 0x7f80_0000 == 0x7f80_0000)
        .map(|&(i, ..)| (w[i + 2], w[i + 3]))
        .collect();
    if buffers.is_empty() && nonfinite.is_empty() {
        return spv;
    }
    let first_fn = ins.iter().position(|&(_, _, op)| op == OP_FUNCTION).unwrap_or(ins.len());
    // composites holding them (in order, so the parts come first)
    let mut composites: Vec<usize> = Vec::new();
    let mut local: std::collections::HashSet<u32> = nonfinite.keys().copied().collect();
    for (k, &(i, n, op)) in ins[..first_fn].iter().enumerate() {
        if op == OP_CONSTANT_COMPOSITE && w[i + 3..i + n].iter().any(|c| local.contains(c)) {
            composites.push(k);
            local.insert(w[i + 2]);
        }
    }
    // the functions (instruction ranges) and which of them use each of those
    let fns: Vec<(usize, usize)> = ins
        .iter()
        .enumerate()
        .filter(|(_, x)| x.2 == OP_FUNCTION)
        .map(|(k, _)| (k, ins[k..].iter().position(|x| x.2 == OP_FUNCTION_END).map_or(ins.len(), |e| k + e + 1)))
        .collect();
    let mut users: std::collections::HashMap<u32, Vec<usize>> = std::collections::HashMap::new();
    for (f, &(a, b)) in fns.iter().enumerate() {
        for &(i, n, op) in &ins[a..b] {
            if op == OP_LINE {
                continue;
            }
            for v in &w[i + 1..i + n] {
                if local.contains(v) && !users.get(v).is_some_and(|u| u.contains(&f)) {
                    users.entry(*v).or_default().push(f);
                }
            }
        }
    }
    // a composite's parts are needed where it is
    for &k in composites.iter().rev() {
        let (i, n, _) = ins[k];
        let fs = users.get(&w[i + 2]).cloned().unwrap_or_default();
        for part in w[i + 3..i + n].iter().filter(|c| local.contains(c)) {
            let u = users.entry(*part).or_default();
            for f in &fs {
                if !u.contains(f) {
                    u.push(*f);
                }
            }
        }
    }
    // used elsewhere in the global section (other than names, decorations and the composites)
    let global_use = |id: u32| {
        ins[..first_fn].iter().enumerate().any(|(k, &(i, n, op))| {
            !matches!(op, OP_NAME | OP_MEMBER_NAME | OP_DECORATE | OP_MEMBER_DECORATE)
                && !composites.contains(&k)
                && !(op == OP_CONSTANT && w[i + 2] == id)
                && w[i + 1..i + n].contains(&id)
        })
    };
    let movable = local.iter().all(|&id| users.get(&id).is_none_or(|u| u.len() == 1) && !global_use(id));
    if !movable {
        // (fallback: 2^127 of the same sign; FLT_MAX itself comes out of naga's decimal printing
        // rounded up past what the page's WGSL compiler accepts)
        local.clear();
        composites.clear();
    }
    // result -> type, for the value instructions (type, result, ...)
    // (not the types, nor debug info, decorations, stores, image writes, barriers, branches)
    const NO_RESULT: [u32; 26] = [3, 4, 5, 6, 7, 8, 11, 14, 15, 16, 17, 62, 63, 71, 72, 99, 224, 225, 228, 246, 247, 249, 250, 251, 252, 253];
    let mut ty = std::collections::HashMap::new();
    for &(i, n, op) in &ins {
        if n >= 3 && !(19..=39).contains(&op) && !NO_RESULT.contains(&op) {
            ty.insert(w[i + 2], w[i + 1]);
        }
    }
    let mut bound = w[3];
    let mut fresh = || {
        bound += 1;
        bound - 1
    };
    let find = |op: u32, f: &dyn Fn(usize) -> bool| ins[..first_fn].iter().find(|&&(i, n, o)| o == op && f(i) && n > 1).map(|&(i, ..)| w[i + 1]);
    let mut globals: Vec<u32> = Vec::new();
    // per scalar type: (ivec2 type, zero)
    let mut made: std::collections::HashMap<u32, (u32, u32)> = std::collections::HashMap::new();
    let mut vec2_zero = |scalar: u32, globals: &mut Vec<u32>, fresh: &mut dyn FnMut() -> u32| -> (u32, u32) {
        *made.entry(scalar).or_insert_with(|| {
            let vec2 = find(OP_TYPE_VECTOR, &|i| w[i + 2] == scalar && w[i + 3] == 2).unwrap_or_else(|| {
                let id = fresh();
                globals.extend([4 << 16 | OP_TYPE_VECTOR, id, scalar, 2]);
                id
            });
            let zero = ins[..first_fn]
                .iter()
                .find(|&&(i, n, op)| op == OP_CONSTANT && n == 4 && w[i + 1] == scalar && w[i + 3] == 0)
                .map(|&(i, ..)| w[i + 2])
                .unwrap_or_else(|| {
                    let id = fresh();
                    globals.extend([4 << 16 | OP_CONSTANT, scalar, id, 0]);
                    id
                });
            (vec2, zero)
        })
    };
    // the private variables with the bits: constant id -> variable
    let mut bits_var: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut uint = 0;
    if !local.is_empty() {
        uint = find(OP_TYPE_INT, &|i| w[i + 2] == 32 && w[i + 3] == 0).unwrap_or_else(|| {
            let id = fresh();
            globals.extend([4 << 16 | OP_TYPE_INT, id, 32, 0]);
            id
        });
        let ptr = find(OP_TYPE_POINTER, &|i| w[i + 2] == PRIVATE && w[i + 3] == uint).unwrap_or_else(|| {
            let id = fresh();
            globals.extend([4 << 16 | OP_TYPE_POINTER, id, PRIVATE, uint]);
            id
        });
        let mut ids: Vec<_> = nonfinite.iter().map(|(&c, &b)| (c, b)).collect();
        ids.sort();
        for (c, bits) in ids {
            let (k, v) = (fresh(), fresh());
            globals.extend([4 << 16 | OP_CONSTANT, uint, k, bits, 5 << 16 | OP_VARIABLE, ptr, v, PRIVATE, k]);
            bits_var.insert(c, v);
        }
    }
    // what each function makes at its start: function -> instructions
    let mut prologue: std::collections::HashMap<usize, Vec<u32>> = std::collections::HashMap::new();
    let mut scalars: Vec<_> = bits_var.iter().map(|(&c, &v)| (c, v)).collect();
    scalars.sort();
    for (c, v) in scalars {
        let Some(&f) = users.get(&c).and_then(|u| u.first()) else { continue };
        let t = fresh();
        let float = ty.get(&c).copied().unwrap_or(0);
        prologue.entry(f).or_default().extend([4 << 16 | OP_LOAD, uint, t, v, 4 << 16 | OP_BITCAST, float, c, t]);
    }
    for &k in &composites {
        let (i, n, _) = ins[k];
        let Some(&f) = users.get(&w[i + 2]).and_then(|u| u.first()) else { continue };
        let mut inst = w[i..i + n].to_vec();
        inst[0] = (n as u32) << 16 | OP_COMPOSITE_CONSTRUCT;
        prologue.entry(f).or_default().extend(inst);
    }
    let mut out: Vec<u32> = w[..5].to_vec();
    let mut body: Vec<u32> = Vec::new();
    let f_of = |k: usize| fns.iter().position(|&(a, b)| k >= a && k < b);
    let mut pending: Option<usize> = None;
    for (k, &(i, n, op)) in ins.iter().enumerate() {
        let mut inst = w[i..i + n].to_vec();
        // a function's prologue goes after its first label and the variables that follow it
        if let Some(f) = pending {
            if op != OP_VARIABLE {
                body.extend(prologue.remove(&f).unwrap_or_default());
                pending = None;
            }
        }
        match op {
            OP_CONSTANT if bits_var.contains_key(&inst[2]) => continue,
            OP_CONSTANT_COMPOSITE if composites.contains(&k) => continue,
            OP_CONSTANT if floats.contains(&inst[1]) && inst.len() == 4 && inst[3] & 0x7f80_0000 == 0x7f80_0000 => {
                inst[3] = inst[3] & 0x8000_0000 | 0x7f00_0000
            }
            // SampledBuffer / ImageBuffer: not needed any more
            OP_CAPABILITY if matches!(inst[1], 46 | 47) => inst[1] = 1,
            OP_TYPE_IMAGE if buffers.contains(&inst[1]) => inst[3] = 1,
            OP_IMAGE_FETCH if n >= 5 && ty.get(&inst[3]).is_some_and(|t| buffers.contains(t)) => {
                let coord = inst[4];
                let scalar = ty.get(&coord).copied().unwrap_or(0);
                let is_int = ins.iter().any(|&(j, m, o)| o == OP_TYPE_INT && m == 4 && w[j + 1] == scalar);
                if is_int {
                    let (vec2, zero) = vec2_zero(scalar, &mut globals, &mut fresh);
                    let c2 = fresh();
                    body.extend([5 << 16 | OP_COMPOSITE_CONSTRUCT, vec2, c2, coord, zero]);
                    inst[4] = c2;
                    if n == 5 {
                        // image operands: Lod 0
                        inst.extend([2, zero]);
                        inst[0] = (inst.len() as u32) << 16 | op;
                    }
                }
            }
            _ => {}
        }
        if k < first_fn {
            out.extend(inst);
        } else {
            body.extend(inst);
        }
        if op == OP_LABEL {
            if let Some(f) = f_of(k).filter(|f| prologue.contains_key(f)) {
                pending = Some(f);
            }
        }
    }
    out.extend(globals);
    out.extend(body);
    out[3] = bound;
    out.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// WGSL has no point size output (vkd3d writes one in every vertex shader): drop it from the
/// entry points' results.
fn drop_point_size(m: &mut naga::Module) {
    use naga::*;
    for ep in &mut m.entry_points {
        let Some(res) = &ep.function.result else { continue };
        let old = res.ty;
        let t = m.types[old].clone();
        let TypeInner::Struct { members, span } = t.inner else { continue };
        let Some(i) = members.iter().position(|x| x.binding == Some(Binding::BuiltIn(BuiltIn::PointSize))) else {
            continue;
        };
        let mut ms = members.clone();
        ms.remove(i);
        m.types.replace(old, Type { name: t.name.clone(), inner: TypeInner::Struct { members: ms, span } });
        for (_, e) in ep.function.expressions.iter_mut() {
            if let Expression::Compose { ty, components } = e {
                if *ty == old {
                    components.remove(i);
                }
            }
        }
    }
}
