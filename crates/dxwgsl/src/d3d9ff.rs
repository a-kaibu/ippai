//! The fixed-function pipeline as WGSL: a vertex shader from the vertex declaration and
//! transforms, a pixel shader from the texture stage states.
//!
//! Uniforms (same block as programmable shaders, see d3d9::uniforms):
//! vertex: x[0] position fixup, x[2] (vp x, vp y, 2/vp w, 2/vp h) for pretransformed vertices,
//! c[0..4] world*view*projection (D3D's row-major memory), c[4..12] texture matrices of
//! stages 0-1, c[12] material diffuse (lighting without normals).
//! pixel: x[0] alpha bits and alpha reference, c[0] texture factor, c[1..9] stage constants.

use crate::d3d9::{uniforms, USAGE_COLOR, USAGE_NORMAL, USAGE_POSITION, USAGE_POSITIONT, USAGE_TEXCOORD};
use std::fmt::Write;

pub const VS_CONSTS: u32 = 13;
pub const PS_CONSTS: u32 = 9;

/// What the generated vertex shader depends on.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct VsKey {
    /// (usage, index, location, kind) of each input
    pub inputs: Vec<(u32, u32, u32, u32)>,
    pub rhw: bool,
    pub lighting: bool,
    /// per stage: texcoord index (low bits of TEXCOORDINDEX), and the texture transform
    /// (TEXTURETRANSFORMFLAGS count, 0 off) for stages 0-1
    pub tci: [(u8, u8); 8],
    /// SHADEMODE flat: the colors come from each triangle's first vertex
    pub flat: bool,
}

pub fn vertex(k: &VsKey) -> String {
    let mut h = uniforms(VS_CONSTS, 0);
    h.push_str("struct In {\n");
    for &(_, _, loc, kind) in &k.inputs {
        let ty = ["vec4f", "vec4u", "vec4i", "vec4f"][kind as usize];
        let _ = writeln!(h, "  @location({}) i{}: {},", loc, loc, ty);
    }
    if k.inputs.is_empty() {
        h.push_str("  @builtin(vertex_index) vid: u32,\n");
    }
    let f = if k.flat { "@interpolate(flat) " } else { "" };
    let _ = write!(h, "}}\nstruct Out {{\n  @builtin(position) pos: vec4f,\n  @location(0) {f}l0: vec4f,\n  @location(1) {f}l1: vec4f,\n");
    for t in 0..8 {
        let _ = writeln!(h, "  @location({}) l{}: vec4f,", 2 + t, 2 + t);
    }
    h.push_str("}\n@vertex fn main(i: In) -> Out {\n  var o: Out;\n");
    let get = |usage: u32, index: u32| -> Option<String> {
        k.inputs.iter().find(|x| x.0 == usage && x.1 == index).map(|&(_, _, loc, kind)| match kind {
            3 => format!("i.i{}.zyxw", loc),
            0 => format!("i.i{}", loc),
            _ => format!("vec4f(i.i{})", loc),
        })
    };
    let pos = get(USAGE_POSITIONT, 0).or_else(|| get(USAGE_POSITION, 0)).unwrap_or("vec4f(0.0, 0.0, 0.0, 1.0)".into());
    if k.rhw {
        let _ = writeln!(h, "  let p = {};", pos);
        h.push_str(
            "  let vp = vec4f(bitcast<f32>(u.x[2].x), bitcast<f32>(u.x[2].y), bitcast<f32>(u.x[2].z), bitcast<f32>(u.x[2].w));
  var w = 1.0;
  if (p.w != 0.0) { w = 1.0 / p.w; }
  let ndc = vec2f((p.x - vp.x) * vp.z - 1.0, 1.0 - (p.y - vp.y) * vp.w);
  o.pos = vec4f(ndc * w, p.z * w, w);
",
        );
    } else {
        let _ = writeln!(h, "  let p = vec4f(({}).xyz, 1.0);", pos);
        h.push_str("  o.pos = mat4x4f(u.c[0], u.c[1], u.c[2], u.c[3]) * p;\n");
    }
    h.push_str("  o.pos = fixpos(o.pos);\n");
    let diffuse = get(USAGE_COLOR, 0);
    let has_normal = get(USAGE_NORMAL, 0).is_some();
    let d = match (&diffuse, k.lighting && !k.rhw) {
        (Some(c), false) => c.clone(),
        (None, false) => "vec4f(1.0)".into(),
        // (lights aren't done: material/vertex color as lit by a white ambient)
        (Some(c), true) => c.clone(),
        (None, true) => "u.c[12]".into(),
    };
    let _ = has_normal;
    let _ = writeln!(h, "  o.l0 = clamp({}, vec4f(0.0), vec4f(1.0));", d);
    let _ = writeln!(h, "  o.l1 = {};", get(USAGE_COLOR, 1).unwrap_or("vec4f(0.0)".into()));
    for t in 0..8u32 {
        let (src, count) = k.tci[t as usize];
        let tc = get(USAGE_TEXCOORD, src as u32).unwrap_or("vec4f(0.0, 0.0, 0.0, 1.0)".into());
        if count > 0 && t < 2 {
            // texture transform: (u, v, 1, 0) style input through the stage's matrix
            let _ = writeln!(
                h,
                "  {{ let c = {tc}; let v = vec4f(c.xy, select(c.z, 1.0, {last}), select(c.w, 1.0, {last3})); \
                 o.l{l} = mat4x4f(u.c[{m}], u.c[{m1}], u.c[{m2}], u.c[{m3}]) * v; }}",
                tc = tc,
                last = if count <= 2 { "true" } else { "false" },
                last3 = if count <= 3 { "true" } else { "false" },
                l = 2 + t,
                m = 4 + t * 4,
                m1 = 5 + t * 4,
                m2 = 6 + t * 4,
                m3 = 7 + t * 4
            );
        } else {
            let _ = writeln!(h, "  o.l{} = {};", 2 + t, tc);
        }
    }
    h.push_str("  return o;\n}\n");
    h.push_str(FIXPOS);
    h
}

const FIXPOS: &str = "
fn fixpos(p: vec4f) -> vec4f {
  return vec4f(p.x + bitcast<f32>(u.x[0].x) * p.w, p.y - bitcast<f32>(u.x[0].y) * p.w, p.z, p.w);
}
";

/// What the generated pixel shader depends on.
#[derive(Clone, PartialEq, Eq, Hash, Debug, Default)]
pub struct PsKey {
    /// per stage until the first disabled one: colorop, arg1, arg2, arg0, alphaop, a1, a2, a0,
    /// resultarg, projected (TTFF_PROJECTED)
    pub stages: Vec<[u32; 10]>,
    pub specular: bool,
    pub alpha_test: Option<u32>,
    /// texture types per stage (2: 2D, 0: none bound)
    pub tex: [u8; 8],
    /// SHADEMODE flat (must match the vertex shader's)
    pub flat: bool,
    /// stages with BORDER / MIRRORONCE addressing (crate::addr)
    pub addr: Vec<crate::addr::Addr>,
}

const TOP_DISABLE: u32 = 1;

fn arg(a: u32, stage: usize, alpha: bool) -> String {
    let base = match a & 0xf {
        0 => "diffuse".to_string(),
        1 => "current".to_string(),
        2 => format!("t{}", stage),
        3 => "u.c[0]".to_string(),
        4 => "specular".to_string(),
        5 => "temp".to_string(),
        6 => format!("u.c[{}]", 1 + stage),
        _ => "current".to_string(),
    };
    let mut v = base;
    if a & 0x20 != 0 && !alpha {
        v = format!("vec4f({}.w)", v); // ALPHAREPLICATE
    }
    if a & 0x10 != 0 {
        v = format!("(1.0 - {})", v); // COMPLEMENT
    }
    v
}

fn op(o: u32, a1: &str, a2: &str, a0: &str, stage: usize) -> String {
    match o {
        2 => a1.into(),
        3 => a2.into(),
        4 => format!("({} * {})", a1, a2),
        5 => format!("({} * {} * 2.0)", a1, a2),
        6 => format!("({} * {} * 4.0)", a1, a2),
        7 => format!("({} + {})", a1, a2),
        8 => format!("({} + {} - 0.5)", a1, a2),
        9 => format!("(({} + {} - 0.5) * 2.0)", a1, a2),
        10 => format!("({} - {})", a1, a2),
        11 => format!("({0} + {1} - {0} * {1})", a1, a2),
        12 => format!("mix({}, {}, diffuse.w)", a2, a1),
        13 => format!("mix({}, {}, t{}.w)", a2, a1, stage),
        14 => format!("mix({}, {}, u.c[0].w)", a2, a1),
        15 => format!("({} + {} * (1.0 - t{}.w))", a1, a2, stage),
        16 => format!("mix({}, {}, current.w)", a2, a1),
        18 => format!("vec4f({0}.xyz + {0}.w * {1}.xyz, {0}.w)", a1, a2),
        19 => format!("vec4f({0}.xyz * {1}.xyz + {0}.w, {0}.w)", a1, a2),
        20 => format!("vec4f((1.0 - {0}.w) * {1}.xyz + {0}.xyz, {0}.w)", a1, a2),
        21 => format!("vec4f((1.0 - {0}.xyz) * {1}.xyz + {0}.w, {0}.w)", a1, a2),
        24 => format!("vec4f(clamp(dot({0}.xyz - 0.5, {1}.xyz - 0.5) * 4.0, 0.0, 1.0))", a1, a2),
        25 => format!("({} + {} * {})", a0, a1, a2),
        26 => format!("mix({}, {}, {})", a2, a1, a0),
        _ => a1.into(),
    }
}

pub fn pixel(k: &PsKey) -> String {
    let mut h = uniforms(PS_CONSTS, 1);
    for t in 0..k.stages.len() {
        if k.tex[t] == 2 {
            let _ = writeln!(h, "@group(1) @binding({}) var tex{}: texture_2d<f32>;", t * 2, t);
            let _ = writeln!(h, "@group(1) @binding({}) var smp{}: sampler;", t * 2 + 1, t);
        }
    }
    let addr: Vec<_> = k.addr.iter().filter(|a| k.tex.get(a.sampler as usize) == Some(&2) && (a.sampler as usize) < k.stages.len()).copied().collect();
    h.push_str(&crate::addr::helpers(&addr));
    let f = if k.flat { "@interpolate(flat) " } else { "" };
    let _ = write!(h, "struct In {{\n  @builtin(position) fragpos: vec4f,\n  @location(0) {f}l0: vec4f,\n  @location(1) {f}l1: vec4f,\n");
    for t in 0..8 {
        let _ = writeln!(h, "  @location({}) l{}: vec4f,", 2 + t, 2 + t);
    }
    h.push_str("}\n@fragment fn main(i: In) -> @location(0) vec4f {\n");
    h.push_str("  let diffuse = i.l0;\n  let specular = i.l1;\n  var current = diffuse;\n  var temp = vec4f(0.0);\n");
    // all samples first (uniform control flow)
    for (t, s) in k.stages.iter().enumerate() {
        // (BLENDTEXTUREALPHA / BLENDTEXTUREALPHAPM read the stage's texture as an alpha op too)
        let uses = [s[1], s[2], s[3], s[5], s[6], s[7]].iter().any(|a| a & 0xf == 2) || [s[0], s[4]].iter().any(|&o| o == 13 || o == 15);
        if !uses {
            continue;
        }
        if k.tex[t] == 2 {
            let coord = if s[9] != 0 {
                format!("(i.l{0}.xy / i.l{0}.w)", 2 + t)
            } else {
                format!("i.l{}.xy", 2 + t)
            };
            let smp = format!("textureSample(tex{0}, smp{0}, {1})", t, crate::addr::coord(&addr, t as u32, &coord));
            let _ = writeln!(h, "  var t{} = {};", t, smp);
            let _ = writeln!(h, "  if ((u.x[0].z & {}u) != 0u) {{ t{}.w = 1.0; }}", 1u32 << t, t);
            // (bits 16-31: red is the luminance, rgb = r)
            let _ = writeln!(h, "  if ((u.x[0].z & {}u) != 0u) {{ t{t} = vec4f(t{t}.xxx, t{t}.w); }}", 1u32 << (t + 16));
            if addr.iter().any(|a| a.sampler == t as u32) {
                let _ = writeln!(h, "  t{t} = {};", crate::addr::wrap(&addr, t as u32, &format!("t{t}"), &coord));
            }
        } else {
            // no texture: white (as stage 0's default MODULATE then gives the diffuse color)
            let _ = writeln!(h, "  let t{} = vec4f(1.0);", t);
        }
    }
    for (t, s) in k.stages.iter().enumerate() {
        if s[0] == TOP_DISABLE {
            break;
        }
        let c = op(s[0], &arg(s[1], t, false), &arg(s[2], t, false), &arg(s[3], t, false), t);
        let a = if s[4] == TOP_DISABLE {
            "current.w".to_string()
        } else {
            format!("({}).w", op(s[4], &arg(s[5], t, true), &arg(s[6], t, true), &arg(s[7], t, true), t))
        };
        let dst = if s[8] & 0xf == 5 { "temp" } else { "current" };
        let _ = writeln!(h, "  {{ let c = clamp({}, vec4f(-1.0), vec4f(1.0)); let a = clamp({}, -1.0, 1.0);", c, a);
        let _ = writeln!(h, "    {} = vec4f(c.xyz, a); }}", dst);
    }
    if k.specular {
        h.push_str("  current = vec4f(current.xyz + specular.xyz, current.w);\n");
    }
    h.push_str("  let out = clamp(current, vec4f(0.0), vec4f(1.0));\n");
    if let Some(func) = k.alpha_test {
        let _ = writeln!(h, "  if (!alpha_ok(out.w, {}u)) {{ discard; }}", func);
    }
    // (a render target of fewer bits: what it keeps, x[3])
    h.push_str("  return steps(out);\n}\n");
    h.push_str(
        "
// (a render target of fewer bits, x[3]: R, G, B, A each rounded to its 2^bits - 1 steps, 2^(bits-1) - 1 signed; 0: as it is)
fn steps(c: vec4f) -> vec4f {
  let m = vec4f(u.x[3] & vec4u(0x7fffffffu));
  // (bit 31: SNORM, -1 to 1)
  let lo = select(vec4f(0.0), vec4f(-1.0), (u.x[3] & vec4u(0x80000000u)) != vec4u(0u));
  return select(c, round(clamp(c, lo, vec4f(1.0)) * m) / max(m, vec4f(1.0)), m > vec4f(0.0));
}
fn alpha_ok(a: f32, func: u32) -> bool {
  let r = bitcast<f32>(u.x[0].w);
  // (the alpha as it is, against ref / 255: Windows lets 127.5 / 255 pass LESS 128)
  let q = a;
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
",
    );
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn valid(w: &str) {
        let m = naga::front::wgsl::parse_str(w).unwrap_or_else(|e| panic!("{}\n{w}", e.emit_to_string(w)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&m)
            .unwrap_or_else(|e| panic!("{e:?}\n{w}"));
    }

    /// The stage lines of the shader: what each stage assigns ("current" / "temp").
    fn stage_dsts(w: &str) -> Vec<&str> {
        w.lines().filter_map(|l| l.trim().strip_suffix(" = vec4f(c.xyz, a); }")).collect()
    }

    // D3DTA_TEMP (5) and D3DTSS_RESULTARG = D3DTA_TEMP: what kura reports with
    // D3DPMISCCAPS_TSSARGTEMP. A stage that targets temp leaves current as it was for the next
    // stage, and a later stage reads temp (ダンジョンズ＆シネステジア quits without the cap).
    #[test]
    fn resultarg_temp_goes_to_temp_and_back() {
        // stage 0: temp = tex0 (SELECTARG1 TEXTURE, result TEMP); stage 1: current = diffuse * temp
        let k = PsKey {
            stages: vec![[2, 2, 0, 0, 2, 2, 0, 0, 5, 0], [4, 0, 5, 0, 4, 0, 5, 0, 1, 0]],
            tex: [2, 0, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        };
        let w = pixel(&k);
        valid(&w);
        assert_eq!(stage_dsts(&w), ["temp", "current"], "{w}");
        assert!(w.contains("(diffuse * temp)"), "{w}");
        assert!(w.contains("var temp = vec4f(0.0);"), "temp starts at 0 (as D3D's)\n{w}");
    }

    fn stage() -> impl Strategy<Value = [u32; 10]> {
        // ops 2-26 (not 1, DISABLE, which ends the stages), args 0-6 with COMPLEMENT /
        // ALPHAREPLICATE, RESULTARG CURRENT or TEMP
        let op = 2u32..=26;
        let a = (0u32..=6, any::<bool>(), any::<bool>()).prop_map(|(r, c, ar)| r | if c { 0x10 } else { 0 } | if ar { 0x20 } else { 0 });
        (op.clone(), a.clone(), a.clone(), a.clone(), op, a.clone(), a.clone(), a, prop::bool::ANY, prop::bool::ANY)
            .prop_map(|(co, c1, c2, c0, ao, a1, a2, a0, temp, proj)| [co, c1, c2, c0, ao, a1, a2, a0, if temp { 5 } else { 1 }, proj as u32])
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(256))]
        // Every stage setup a game can make (any op, any arg incl. TEMP, either result register)
        // gives a valid shader, and each stage writes the register its RESULTARG names.
        #[test]
        fn any_stages_valid_and_write_their_resultarg(stages in prop::collection::vec(stage(), 1..=8), tex in prop::array::uniform8(prop_oneof![Just(0u8), Just(2u8)]), spec: bool) {
            let k = PsKey { stages: stages.clone(), tex, specular: spec, ..Default::default() };
            let w = pixel(&k);
            valid(&w);
            let want: Vec<&str> = stages.iter().map(|s| if s[8] & 0xf == 5 { "temp" } else { "current" }).collect();
            prop_assert_eq!(stage_dsts(&w), want);
        }
    }
}
