//! The GLSL limits glGetIntegerv answers: present (EntisGLS, zombie2_trial.zip, builds
//! `#define MAX_BONE_PALETTE n` from GL_MAX_VERTEX_UNIFORM_COMPONENTS and crashed on the 0 an
//! unknown query gave), at least the GL 2.1 / GLES 2 minimums, consistent between their
//! component and vector forms, and true: shaders using random amounts up to them translate,
//! with a uniform block inside WebGPU's 64 KiB binding and inter-stage locations inside its 16.

use super::*;
use crate::d3d9::trace_tests::{Rng, SERIAL};
use std::collections::HashMap;

fn get(p: u32) -> i32 {
    let mut v = [-7i32; 4];
    unsafe { glGetIntegerv(p, v.as_mut_ptr()) };
    v[0]
}

fn fresh() {
    *GL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    let ctx = vngl_context_create(0);
    vngl_make_current(ctx, 0, 0, 0);
}

#[test]
fn glsl_limits_are_answered_and_consistent() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fresh();
    // (pname, GL 2.1 / GLES 2 minimum)
    for (p, min) in [(0x8B4A, 512), (0x8B49, 64), (0x8B4B, 32), (0x8DFB, 128), (0x8DFD, 16), (0x8DFC, 8), (0x8869, 16)] {
        assert!(get(p) >= min, "{p:#x}: {} < {min}", get(p));
    }
    assert_eq!(get(0x8B4A), 4 * get(0x8DFB));
    assert_eq!(get(0x8B49), 4 * get(0x8DFD));
    assert_eq!(get(0x8B4B), 4 * get(0x8DFC));
    let mut f = [0f32; 4];
    unsafe { glGetFloatv(0x8B4A, f.as_mut_ptr()) };
    assert_eq!(f[0] as i32, get(0x8B4A));
}

/// Random uniform arrays (vec4 / mat4 / mat3, the bone palettes games declare) and varying
/// counts within the advertised limits link, and what they need fits WebGPU's defaults.
#[test]
fn shaders_within_the_limits_translate() {
    let _g = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    fresh();
    let (uv, fv, vary) = (get(0x8DFB) as usize, get(0x8DFD) as usize, get(0x8DFC) as usize);
    let mut r = Rng(0x2026_1113);
    for case in 0..48 {
        // a bone palette: mat3 + vec4 per bone (4 vectors), as EntisGLS declares it
        let bones = 1 + r.below(uv / 4 - 2);
        let extra = r.below(uv - bones * 4);
        let pix = 1 + r.below(fv - 1);
        let nv = 1 + r.below(vary);
        let vs = format!(
            "#define MAX_BONE_PALETTE {bones}\nattribute vec4 p;\nuniform mat3 rot[MAX_BONE_PALETTE];\nuniform vec4 tr[MAX_BONE_PALETTE];\nuniform vec4 more[{n}];\n{vs_vary}void main() {{ int k = int(p.w); gl_Position = vec4(rot[k] * p.xyz + tr[k].xyz, 1.0) + more[k]; {vs_set} }}",
            n = extra.max(1),
            vs_vary = (0..nv).map(|i| format!("varying vec4 v{i};\n")).collect::<String>(),
            vs_set = (0..nv).map(|i| format!("v{i} = p;")).collect::<String>(),
        );
        let ps = format!(
            "uniform vec4 c[{pix}];\n{ps_vary}void main() {{ gl_FragColor = c[0]{ps_sum}; }}",
            ps_vary = (0..nv).map(|i| format!("varying vec4 v{i};\n")).collect::<String>(),
            ps_sum = (0..nv).map(|i| format!(" + v{i}")).collect::<String>(),
        );
        let prog = glwgsl::link(&vs, &ps, &HashMap::new()).unwrap_or_else(|e| panic!("case {case}: bones {bones} extra {extra} pix {pix} varyings {nv}: {e}"));
        for b in [&prog.vs.block, &prog.ps.block].into_iter().flatten() {
            assert!(b.size <= 65536, "case {case}: block of {} bytes", b.size);
        }
        let locs = prog.vs.wgsl.matches("@location(").count().saturating_sub(prog.attribs.len());
        assert!(locs <= 16, "case {case}: {locs} inter-stage locations\n{}", prog.vs.wgsl);
    }
}
