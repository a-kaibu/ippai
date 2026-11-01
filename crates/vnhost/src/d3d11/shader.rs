//! Direct3D 10/11 shaders (DXBC) to WGSL: vkd3d-shader makes SPIR-V (hangover/vkd3d/vnshader.c,
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
    // (D3D samples anywhere: WGSL's uniformity rules would reject some of these shaders)
    let code = format!("diagnostic(off, derivative_uniformity);\n{text}");
    Ok(Wgsl { code, bindings })
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
