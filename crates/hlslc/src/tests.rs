//! Shaders compiled here go through dxwgsl's DXBC to WGSL translation and naga's validation.

fn compile(src: &str, entry: &str, target: &str) -> Vec<u8> {
    let mut inc = |_: &str, _: bool| None;
    match super::compile(src, &[], &mut inc, entry, target, 0) {
        Ok(b) => b,
        Err(e) => panic!("{entry}: {e}"),
    }
}

fn wgsl(code: &[u8], pixel: bool, ps: Option<&[u8]>) -> String {
    let w = dxwgsl::translate(code, pixel, ps).unwrap_or_else(|e| panic!("translate: {e}"));
    let m = match naga::front::wgsl::parse_str(&w.code) {
        Ok(m) => m,
        Err(e) => panic!("{}\n{}", e.emit_to_string(&w.code), w.code),
    };
    if let Err(e) = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m) {
        panic!("invalid: {e:?}\n{}", w.code);
    }
    w.code
}

/// vertex + pixel shader pair through the translator
fn pair(src: &str, vs: &str, ps: &str, target: &str) -> (String, String) {
    let v = compile(src, vs, &format!("vs_{target}"));
    let p = compile(src, ps, &format!("ps_{target}"));
    let pw = wgsl(&p, true, None);
    let vw = wgsl(&v, false, Some(&p));
    (vw, pw)
}

const D3D11TEST: &str = "cbuffer C : register(b0) { float4 rot; float4 tint; };\n\
struct V { float4 pos : SV_Position; float4 col : COLOR; float2 uv : TEXCOORD; };\n\
Texture2D t : register(t0); SamplerState s : register(s0);\n\
V vs_tri(float2 p : POSITION, float4 c : COLOR) {\n\
  V o; o.pos = float4(p.x * rot.x - p.y * rot.y, p.x * rot.y + p.y * rot.x, 0.5, 1); o.col = c; o.uv = 0; return o; }\n\
float4 ps_tri(V i) : SV_Target { return i.col * tint; }\n\
V vs_quad(float2 p : POSITION, float2 uv : TEXCOORD) {\n\
  V o; o.pos = float4(p, 0.5, 1); o.col = 1; o.uv = uv; return o; }\n\
float4 ps_quad(V i) : SV_Target { return t.Sample(s, i.uv); }\n";

#[test]
fn d3d11test() {
    for t in ["4_0", "5_0", "4_0_level_9_1"] {
        let (v, p) = pair(D3D11TEST, "vs_tri", "ps_tri", t);
        assert!(v.contains("fn main") && p.contains("fn main"));
        pair(D3D11TEST, "vs_quad", "ps_quad", t);
    }
    let v = compile(D3D11TEST, "vs_tri", "vs_4_0");
    let sig = dxwgsl::signature(&v, false);
    assert_eq!(sig.iter().map(|e| (e.name.as_str(), e.reg)).collect::<Vec<_>>(), [("POSITION", 0), ("COLOR", 1)]);
    let osig = dxwgsl::signature(&v, true);
    assert_eq!(osig[0].name, "SV_Position");
    assert_eq!(osig[0].sysval, 1);
}

const GAME: &str = r#"
#define BLUR_TAPS 5
#pragma pack_matrix(row_major)
cbuffer Scene : register(b0) {
    float4x4 world_view_proj;
    float4 color_mul;
    float4 color_add;
    float2 texel;
    float time;
    int mode;
    bool use_mask;
};
cbuffer Extra { float4 offsets[BLUR_TAPS]; float weights[BLUR_TAPS]; };
Texture2D<float4> tex : register(t0);
Texture2D mask_tex : register(t1);
SamplerState samp : register(s0);
static const float3 LUMA = float3(0.299, 0.587, 0.114);
static const float gauss[3] = { 0.25, 0.5, 0.25 };

struct VS_IN { float3 pos : POSITION; float2 uv : TEXCOORD0; float4 col : COLOR0; };
struct PS_IN { float4 pos : SV_POSITION; float2 uv : TEXCOORD0; float4 col : COLOR0; };

PS_IN vs_main(VS_IN i) {
    PS_IN o;
    o.pos = mul(float4(i.pos, 1.0), world_view_proj);
    o.uv = i.uv;
    o.col = i.col;
    return o;
}

float3 grey(float3 c) { return dot(c, LUMA).xxx; }

float4 blur(float2 uv) {
    float4 acc = 0;
    [unroll] for (int k = 0; k < BLUR_TAPS; k++) {
        acc += tex.Sample(samp, uv + offsets[k].xy * texel) * weights[k];
    }
    for (int j = 0; j < 3; ++j) acc += tex.SampleLevel(samp, uv, 0) * gauss[j] * 0.01;
    return acc;
}

float4 ps_main(PS_IN i) : SV_TARGET {
    float4 c = tex.Sample(samp, i.uv) * i.col;
    if (mode == 1) {
        c.rgb = grey(c.rgb);
    } else if (mode == 2) {
        c = blur(i.uv);
    } else {
        c.rgb = lerp(c.rgb, 1 - c.rgb, saturate(sin(time) * 0.5 + 0.5));
    }
    if (use_mask) {
        float m = mask_tex.Sample(samp, i.uv).a;
        clip(m - 0.5);
        c.a *= m;
    }
    switch (mode) {
    case 3: c.rg = c.gr; break;
    case 4:
    case 5: c = pow(abs(c), 2.2); break;
    default: break;
    }
    float r = length(i.uv - 0.5);
    c.rgb *= smoothstep(0.8, 0.2, r) + atan2(i.uv.y, i.uv.x) * 0.0;
    return c * color_mul + color_add;
}
"#;

#[test]
fn game_like() {
    pair(GAME, "vs_main", "ps_main", "4_0");
    pair(GAME, "vs_main", "ps_main", "5_0");
}

/// D3D9-style code compiled for shader model 4 (backward compatibility).
const LEGACY: &str = r#"
float4x4 mvp;
sampler2D s0 : register(s0);
sampler2D s1;
float alpha;
struct VO { float4 pos : POSITION; float2 uv : TEXCOORD0; };
VO vs(float4 pos : POSITION, float2 uv : TEXCOORD0) { VO o; o.pos = mul(mvp, pos); o.uv = uv; return o; }
float4 ps(float2 uv : TEXCOORD0) : COLOR {
    float4 a = tex2D(s0, uv);
    float4 b = tex2Dlod(s1, float4(uv, 0, 0));
    return lerp(a, b, alpha);
}
"#;

#[test]
fn legacy_syntax() {
    let v = compile(LEGACY, "vs", "vs_4_0");
    let p = compile(LEGACY, "ps", "ps_4_0");
    wgsl(&p, true, None);
    // (the vertex shader's TEXCOORD0 meets the pixel shader's)
    wgsl(&v, false, Some(&p));
}

const MISC: &str = r#"
struct Light { float3 dir; float4 color; };
cbuffer L : register(b1) { Light lights[4]; int count; float4x4 bones[8]; };
Texture2DArray arr; TextureCube cube; SamplerState ss;
Texture2D<float> depth; SamplerComparisonState cmp;
Buffer<float4> buf;
void vs(in float4 p : POSITION, in uint4 idx : BLENDINDICES, in float4 w : BLENDWEIGHT, uint vid : SV_VertexID,
        out float4 o : SV_Position, out float3 n : NORMAL, out nointerpolation uint id : ID) {
    float4x4 m = bones[idx.x] * w.x + bones[idx.y] * w.y;
    o = mul(p, m);
    n = normalize(mul((float3x3)m, p.xyz));
    id = vid % 3u + (uint)(vid / 7);
    int s = (int)vid; s = s / -3 + s % 5;
    id += (uint)s;
}
float4 calc(float3 n, out float spec) {
    float4 c = 0;
    spec = 0;
    for (int i = 0; i < count; i++) {
        if (i >= 4) break;
        float d = saturate(dot(n, -lights[i].dir));
        if (d <= 0) continue;
        c += lights[i].color * d;
        spec = max(spec, pow(d, 16.0));
    }
    return c;
}
float early(float x) {
    if (x < 0) return -1;
    while (x > 1) { x *= 0.5; if (x < 0.75) return x; }
    return x * 2;
}
float4 ps(float4 pos : SV_Position, float3 n : NORMAL, nointerpolation uint id : ID, bool front : SV_IsFrontFace) : SV_Target {
    float spec;
    float4 c = calc(n, spec);
    c += arr.Sample(ss, float3(pos.xy, id)) + cube.SampleLevel(ss, n, 2);
    c.x += depth.SampleCmpLevelZero(cmp, pos.xy, 0.5);
    c += buf.Load(id) + arr.Load(int4(pos.xy, 0, 0));
    uint w, h, l; arr.GetDimensions(0, w, h, l, l);
    c.y += early(c.y) + w * h;
    c.z += asin(c.x) + acos(c.y) + atan(c.z) + fmod(c.w, 2) + exp(spec) + log10(spec + 1);
    float3x3 rot = { 1, 0, 0, 0, 1, 0, 0, 0, 1 };
    c.xyz = mul(rot, c.xyz) + determinant(rot) + (front ? 1 : 0);
    float arr2[3] = { c.x, c.y, c.z };
    arr2[id % 3] = 5;
    c.w = arr2[(id + 1) % 3] + any(c > 2) + all(c < 0);
    return c;
}
"#;

#[test]
fn misc() {
    pair(MISC, "vs", "ps", "5_0");
}

#[test]
fn errors() {
    let mut inc = |_: &str, _: bool| None;
    let e = super::compile("float4 main() : SV_Target { return undefined_x; }", &[], &mut inc, "main", "ps_4_0", 0).unwrap_err();
    assert!(e.contains("undefined_x"), "{e}");
    let e = super::compile("float4 main() : SV_Target { return 1; }", &[], &mut inc, "nope", "ps_4_0", 0).unwrap_err();
    assert!(e.contains("nope"), "{e}");
}

#[test]
fn checksum() {
    // (a blob's checksum covers everything after it: changing a byte changes it)
    let a = compile(D3D11TEST, "ps_quad", "ps_4_0");
    let mut b = a.clone();
    let n = b.len();
    b[n - 5] ^= 1;
    assert_ne!(super::sm4::checksum(&a[20..]), super::sm4::checksum(&b[20..]));
    assert_eq!(&a[4..20], &super::sm4::checksum(&a[20..]));
}

#[test]
fn print() {
    let Ok(which) = std::env::var("HLSL_PRINT") else {
        return;
    };
    let (src, vs, ps) = match which.as_str() {
        "game" => (GAME, "vs_main", "ps_main"),
        "misc" => (MISC, "vs", "ps"),
        "legacy" => (LEGACY, "vs", "ps"),
        "sm3" | "sm3ps" | "skin" | "ps3" => {
            let (e, t) = match which.as_str() {
                "sm3" => ("vs", "vs_2_0"),
                "sm3ps" => ("ps", "ps_2_0"),
                "skin" => ("vs_skin", "vs_3_0"),
                _ => ("ps3", "ps_3_0"),
            };
            eprintln!("{}", sm3_text(SM3, e, t));
            let link = if t.starts_with("ps") { 0xffff } else { 0 };
            eprintln!("{}", d3d9(SM3, e, t, link).0);
            return;
        }
        _ => (D3D11TEST, "vs_tri", "ps_quad"),
    };
    let (v, p) = pair(src, vs, ps, "5_0");
    eprintln!("{v}\n-----\n{p}");
}

// ---- shader model 2/3 ----

/// D3D9 bytecode through dxwgsl's D3D9 translation; (WGSL, assembly)
fn d3d9(src: &str, entry: &str, target: &str, link: u32) -> (String, dxwgsl::d3d9::Shader) {
    let code = compile(src, entry, target);
    let words: Vec<u32> = code.chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
    let s = dxwgsl::d3d9::Shader::parse(&words).unwrap_or_else(|| panic!("{entry}: dxwgsl cannot parse"));
    let g = dxwgsl::d3d9::Gen::new(&s);
    let code = if s.pixel {
        g.pixel(&dxwgsl::d3d9::Options { link, vs_inputs: Vec::new(), alpha_test: None, fog: false, addr: Vec::new(), targets: 1, flat: false })
    } else {
        let ins = s.inputs.iter().enumerate().map(|(k, i)| (i.0, k as u32, 0)).collect();
        g.vertex(&dxwgsl::d3d9::Options { link: 0, vs_inputs: ins, alpha_test: None, fog: false, addr: Vec::new(), targets: 1, flat: false })
    };
    let m = match naga::front::wgsl::parse_str(&code) {
        Ok(m) => m,
        Err(e) => panic!("{entry}: {}\n{}", e.emit_to_string(&code), code),
    };
    if let Err(e) = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m) {
        panic!("{entry}: invalid: {e:?}\n{code}");
    }
    (code, s)
}

fn sm3_text(src: &str, entry: &str, target: &str) -> String {
    let mut inc = |_: &str, _: bool| None;
    super::assembly(src, &[], &mut inc, entry, target, 0).unwrap_or_else(|e| panic!("{entry}: {e}"))
}

const SM3: &str = r#"
float4x4 WorldViewProj;
float4 Diffuse : register(c8);
float4x3 Bones[20];
sampler2D Tex : register(s0);
sampler2D Mask;
samplerCUBE Env;
float Alpha;
bool UseMask;
float4 Weights[4];
int Count;
struct VO { float4 pos : POSITION; float2 uv : TEXCOORD0; float4 col : COLOR0; float3 n : TEXCOORD1; };
VO vs(float4 p : POSITION, float2 uv : TEXCOORD0, float4 c : COLOR0, float3 n : NORMAL) {
    VO o;
    o.pos = mul(p, WorldViewProj);
    o.uv = uv;
    o.col = saturate(c * Diffuse);
    o.n = normalize(n);
    return o;
}
VO vs_skin(float4 p : POSITION, float2 uv : TEXCOORD0, float4 idx : BLENDINDICES, float4 w : BLENDWEIGHT) {
    VO o;
    float3 pos = 0;
    for (int k = 0; k < 2; k++) pos += mul(p, Bones[(int)idx[k]]) * w[k];
    o.pos = mul(float4(pos, 1), WorldViewProj);
    o.uv = uv;
    o.col = Count > 1 ? Diffuse : 1;
    o.n = 0;
    return o;
}
float4 ps(VO i) : COLOR0 {
    float4 c = tex2D(Tex, i.uv) * i.col;
    if (UseMask) c.a *= tex2D(Mask, i.uv).r;
    float4 acc = 0;
    for (int k = 0; k < Count; k++) acc += Weights[k] * tex2D(Tex, i.uv + k * 0.01);
    clip(c.a - 0.01);
    c.rgb = lerp(c.rgb, dot(c.rgb, float3(0.299, 0.587, 0.114)), Alpha);
    c += texCUBE(Env, i.n) * 0.1;
    return c + acc * (Count > 2 ? 1 : 0) + frac(i.uv.x) + fmod(i.uv.y, 0.5) + step(0.5, i.uv.x);
}
float early(float x) {
    if (x < 0) return -1;
    while (x > 1) { x *= 0.5; if (x < 0.75) return x; }
    return x * 2;
}
float4 ps3(VO i, float2 vpos : VPOS, float face : VFACE) : COLOR0 {
    if (i.uv.x > 2) return 0;
    int a = (int)(i.uv.x * 7);
    int b = a / 3 + a % 3;
    float s, co;
    sincos(i.uv.y, s, co);
    float4 c = float4(floor(i.uv.x), ceil(i.uv.y), round(vpos.x), sqrt(abs(s)));
    c.x += b + (face > 0 ? 1 : 0) + early(co) + pow(abs(c.y), 2.2) + exp(c.z) + atan2(s, co);
    c.y += tex2Dbias(Tex, float4(i.uv, 0, 1)).x + tex2Dproj(Tex, float4(i.uv, 0, 2)).y + tex2Dgrad(Tex, i.uv, ddx(i.uv), ddy(i.uv)).z;
    c.z += any(c.xy > 1) ? 1 : (all(c.xy == 0) ? 2 : -1);
    c.w = sign(c.x) + min(a, 2) + max(c.x, c.y) + (float)(a != 2 && b >= 1) + (float)!(a < 1 || b == 4);
    return c;
}
"#;

#[test]
fn sm3() {
    let (_, v) = d3d9(SM3, "vs", "vs_2_0", 0);
    let (_, p) = d3d9(SM3, "ps", "ps_2_0", v.writes);
    assert!(p.samplers.len() == 3);
    let (_, v3) = d3d9(SM3, "vs", "vs_3_0", 0);
    d3d9(SM3, "ps", "ps_3_0", v3.writes);
    let (_, sk) = d3d9(SM3, "vs_skin", "vs_3_0", 0);
    assert!(sk.uses_rel);
    d3d9(SM3, "ps3", "ps_3_0", v3.writes);
    // constants: Diffuse stays in c8; the rest packed around it
    let t = sm3_text(SM3, "vs", "vs_2_0");
    assert!(t.contains("c8"), "{t}");
}

#[test]
fn sm3_ctab() {
    let code = compile(SM3, "ps", "ps_2_0");
    let w = |i: usize| u32::from_le_bytes(code[4 * i..4 * i + 4].try_into().unwrap());
    assert_eq!(w(0), 0xffff_0200);
    assert_eq!(w(1) & 0xffff, 0xfffe);
    assert_eq!(&code[8..12], b"CTAB");
    let t = &code[12..];
    let r32 = |o: usize| u32::from_le_bytes(t[o..o + 4].try_into().unwrap());
    let r16 = |o: usize| u16::from_le_bytes(t[o..o + 2].try_into().unwrap());
    let name = |o: usize| String::from_utf8_lossy(t[o..].split(|&b| b == 0).next().unwrap()).into_owned();
    let n = r32(12) as usize;
    let info = r32(16) as usize;
    let mut got = Vec::new();
    for k in 0..n {
        let e = info + 20 * k;
        got.push((name(r32(e) as usize), r16(e + 4), r16(e + 8)));
    }
    got.sort();
    let want: Vec<(String, u16, u16)> = vec![
        ("Alpha".into(), 2, 1),
        ("Count".into(), 2, 1),
        ("Env".into(), 3, 1),
        ("Mask".into(), 3, 1),
        ("Tex".into(), 3, 1),
        ("UseMask".into(), 2, 1),
        ("Weights".into(), 2, 4),
    ];
    assert_eq!(got, want);
    assert_eq!(name(r32(24) as usize), "ps_2_0");
}

/// Inputs the shaderconf fuzzer found: an error, not a hang or a panic.
#[test]
fn malformed_sources_are_errors() {
    for src in ["float4 A : register(c0); float4 B : register(c/*); float4 C : register(c2);\nfloat4 ps() : COLOR { return A; }\n"] {
        for t in ["ps_3_0", "ps_4_0"] {
            let mut inc = |_: &str, _: bool| None;
            let _ = super::compile(src, &[], &mut inc, "ps", t, 0);
        }
    }
    // (shaderconf --fuzz: a register list that is not one, a cbuffer at a c register, and
    // COLOR4 in shader model 3, which has four render targets: errors, not bad bytecode)
    let mut inc = |_: &str, _: bool| None;
    for (src, t) in [
        ("cbuffer C : register(buniform mat2 m2; uniform mat3 m30) { float4 A; };\nfloat4 ps() : SV_Target { return A; }\n", "ps_4_0"),
        ("cbuffer C : register(c3) { float4 A; };\nfloat4 ps() : SV_Target { return A; }\n", "ps_4_0"),
        ("cbuffer C : register(b14) { float4 A; };\nfloat4 ps() : SV_Target { return A; }\n", "ps_4_0"),
        ("float4 A : register(c0);\nfloat4 ps() : COLOR4 { return A; }\n", "ps_3_0"),
        // (a constant buffer member written through a run-time index)
        ("cbuffer C : register(b0) { float4 A; float4 B; };\nfloat4 ps() : SV_Target { B[(int)A.x] = 1; return A; }\n", "ps_4_0"),
        ("cbuffer C : register(b0) { float4 A; float4 B; };\nfloat4 ps() : SV_Target { float4 t = 0; t.yz = A.rr[(int)A.x] = 2; return t; }\n", "ps_4_0"),
        ("cbuffer C : register(b0) { float4 A; float4 B[2]; };\nfloat4 ps() : SV_Target { B[(int)A.x].y = 1; return A; }\n", "ps_4_0"),
        ("float4 A : register(c0); float4 B : register(c1);\nfloat4 ps() : COLOR { B[(int)A.x] = 1; return A; }\n", "ps_3_0"),
        // (an array without a size or an initializer, indexed: it has no elements)
        ("float4 A : register(c0);\nfloat4 ps() : COLOR { float4 a[]; a[(int)A.x] = A; return a[1]; }\n", "ps_3_0"),
        ("float4 A : register(c0);\nfloat4 ps() : SV_Target { float4 a[]; a[(int)A.x] = A; return a[1]; }\n", "ps_4_0"),
        ("float4 A : register(c0);\nfloat4 ps() : COLOR { float a[2][]; return A; }\n", "ps_3_0"),
        // (globals too: a static or a uniform array without a size)
        ("float4 A : register(c0); static float4 a[];\nfloat4 ps() : COLOR { return A + a[(int)A.x]; }\n", "ps_3_0"),
        ("float4 A : register(c0); static float4 a[];\nfloat4 ps() : SV_Target { return A + a[(int)A.x]; }\n", "ps_4_0"),
        ("float4 A : register(c0); float4 a[];\nfloat4 ps() : COLOR { return A + a[(int)A.x]; }\n", "ps_3_0"),
        ("float4 A : register(c0); float4 a[];\nfloat4 ps() : SV_Target { return A + a[(int)A.x]; }\n", "ps_4_0"),
    ] {
        assert!(super::compile(src, &[], &mut inc, "ps", t, 0).is_err(), "{src}");
    }
    for (src, t) in [
        ("cbuffer C : register(b13) { float4 A; };\nfloat4 ps() : SV_Target { return A; }\n", "ps_4_0"),
        ("cbuffer C : register(ps_5_0, b1) { float4 A; };\nfloat4 ps() : SV_Target { return A; }\n", "ps_5_0"),
        ("cbuffer C : register(b0, space0) { float4 A; };\nfloat4 ps() : SV_Target { return A; }\n", "ps_5_0"),
        ("float4 A : register(c0);\nfloat4 ps() : COLOR3 { return A; }\n", "ps_3_0"),
        // (the size from the initializer list)
        ("float4 A : register(c0);\nfloat4 ps() : COLOR { float a[] = { 1, 2, 3 }; return A * a[(int)A.x]; }\n", "ps_3_0"),
        ("float4 A : register(c0);\nfloat4 ps() : SV_Target { float a[] = { 1, 2, 3 }; return A * a[(int)A.x]; }\n", "ps_4_0"),
    ] {
        if let Err(e) = super::compile(src, &[], &mut inc, "ps", t, 0) { panic!("{src}: {e}") }
    }
    // (an array sized by its initializer list is the array of that size)
    for t in ["ps_3_0", "ps_4_0"] {
        for (a, b, pick) in [("float a[]", "float a[6]", ""), ("float2 a[]", "float2 a[3]", ".y")] {
            let out = if t == "ps_3_0" { "COLOR" } else { "SV_Target" };
            let src = |d: &str| format!("float4 A : register(c0);\nfloat4 ps() : {out} {{ {d} = {{ 1, 2, 3, 4, 5, 6 }}; return A * a[(int)A.x]{pick}; }}\n");
            let x = super::compile(&src(a), &[], &mut inc, "ps", t, 0).unwrap();
            let y = super::compile(&src(b), &[], &mut inc, "ps", t, 0).unwrap();
            assert_eq!(x, y, "{t} {a}");
        }
    }
    for asm in ["ps_3_0\nexp , c0.x\nmov oCswitch\n", "ps_3_0\nlog r0.x,.y\nmov odcl_te0.5rd0 r0\n", "ps_3_0\nmov r0, ]c[\n", "ps_3_0\nmov r0, [1]\n"] {
        let _ = super::d3d9asm::assemble(asm);
    }
}

/// ANGLE's D3D11 back end (Godot's GL compatibility renderer on Windows): arrays of textures and
/// samplers indexed by a constant that arrives through a function argument
const ANGLE_TEX_ARRAY: &str = "struct PS_INPUT { float4 dx_Position : SV_Position; float2 v0 : TEXCOORD0; };\n\
static const uint _source = 0; static const uint _second = 1;\n\
uniform Texture2D<float4> textures2D[2] : register(t0);\n\
uniform SamplerState samplers2D[2] : register(s0);\n\
float4 gl_texture2D(uint samplerIndex, float2 t) { return textures2D[samplerIndex].Sample(samplers2D[samplerIndex], float2(t.x, t.y)); }\n\
float4 main(PS_INPUT input) : SV_Target { return gl_texture2D(_source, input.v0) + gl_texture2D(_second, input.v0.yx); }\n";

#[test]
fn texture_arrays() {
    let p = compile(ANGLE_TEX_ARRAY, "main", "ps_5_0");
    let w = wgsl(&p, true, None);
    // both elements are bound: t0/s0 and t1/s1
    assert!(w.matches("textureSample").count() >= 2, "{w}");
    assert!(w.contains("t1") && w.contains("s1"), "{w}");
    let mut inc = |_: &str, _: bool| None;
    let bad = ANGLE_TEX_ARRAY.replace("gl_texture2D(_second", "gl_texture2D(5");
    let e = super::compile(&bad, &[], &mut inc, "main", "ps_5_0", 0).unwrap_err();
    assert!(e.contains("out of range"), "{e}");
}

/// ANGLE's vertex shader for GL_POINTS (its geometry shader expands the points): translated in
/// point sprite mode, it is valid WGSL that offsets by the corner and the PSIZE output
#[test]
fn point_sprite_vertex_shader() {
    let src = "struct VS_OUTPUT { float4 dx_Position : SV_Position; float gl_PointSize : PSIZE; float2 v0 : TEXCOORD0; };\n\
VS_OUTPUT main(float2 p : TEXCOORD0, float s : TEXCOORD1) { VS_OUTPUT o; o.dx_Position = float4(p, 0, 1); o.gl_PointSize = s; o.v0 = p; return o; }\n\
float4 ps(VS_OUTPUT i) : SV_Target { return float4(i.v0, 0, 1); }\n";
    let v = compile(src, "main", "vs_5_0");
    let p = compile(src, "ps", "ps_5_0");
    let w = dxwgsl::translate_point_sprite(&v, Some(&p), 1024.0).unwrap();
    let m = naga::front::wgsl::parse_str(&w.code).unwrap_or_else(|e| panic!("{}\n{}", e.emit_to_string(&w.code), w.code));
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m).unwrap();
    assert!(w.code.contains("sp_view") && w.code.contains("clamp(bitcast<f32>(o"), "{}", w.code);
    assert!(w.bindings.iter().any(|b| b.0 == 15), "{:?}", w.bindings);
}

/// Samplers with BORDER / MIRROR_ONCE (dxwgsl::addr): 2D textures and arrays sample at am{s}
/// and mix in the border; offsets, comparisons and other samplers sample as they are.
#[test]
fn border_addressing_in_d3d11_shaders() {
    const SRC: &str = "Texture2D a : register(t0); Texture2DArray b : register(t1); Texture2D<float> z : register(t2);\n\
SamplerState s0 : register(s0); SamplerState s1 : register(s1); SamplerComparisonState c : register(s2);\n\
float4 ps(float4 p : SV_Position, float2 uv : TEXCOORD) : SV_Target {\n\
  return a.Sample(s0, uv) + b.Sample(s0, float3(uv, 1)) + a.Sample(s1, uv) + a.Sample(s0, uv, int2(1, 0))\n\
    + z.SampleCmp(c, uv, 0.5) + a.SampleLevel(s0, uv * 2, 1); }\n";
    let code = compile(SRC, "ps", "ps_5_0");
    for flags in [1, 2, 3, 4, 8, 17, 19, 21, 31] {
        let addr = [dxwgsl::addr::Addr { sampler: 0, flags, border: [0.25f32.to_bits(), 0, 1.0f32.to_bits(), 0.5f32.to_bits()] },
            dxwgsl::addr::Addr { sampler: 2, flags, border: [0; 4] }];
        let w = dxwgsl::translate_addr(&code, true, None, &addr).unwrap();
        let m = naga::front::wgsl::parse_str(&w.code).unwrap_or_else(|e| panic!("{}\n{}", e.emit_to_string(&w.code), w.code));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
            .validate(&m)
            .unwrap_or_else(|e| panic!("{e:?}\n{}", w.code));
        let c = &w.code;
        // t0 and t1 with s0 (three samples without offset), not s1, not the offset one, not the comparison
        assert_eq!(c.matches("am0(").count() - 1, 3, "{c}");
        assert_eq!(c.matches("ab0_0(").count() - 1, 2, "{c}");
        assert_eq!(c.matches("ab1_0(").count() - 1, 1, "{c}");
        assert!(!c.contains("am1(") && !c.contains("am2(") && !c.contains("_2("), "{c}");
        assert!(c.contains("vec4f(0.25, 0.0, 1.0, 0.5)") == (flags & 3 != 0), "{c}");
    }
    // no samplers with them: as translate
    assert_eq!(dxwgsl::translate_addr(&code, true, None, &[]).unwrap().code, dxwgsl::translate(&code, true, None).unwrap().code);
}

// ---- compute shaders ----

fn cs(src: &str, defs: &[(&str, &str)], target: &str, fmts: &[(u32, &str)]) -> String {
    let defs: Vec<(String, String)> = defs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    let mut inc = |name: &str, _: bool| match name {
        "NIS_Scaler.h" => Some(NIS_SCALER.to_string()),
        _ => None,
    };
    let code = super::compile(src, &defs, &mut inc, "main", target, 0).unwrap_or_else(|e| panic!("{target} {defs:?}: {e}\n{src}"));
    let w = dxwgsl::translate_compute(&code, fmts).unwrap_or_else(|e| panic!("translate {defs:?}: {e}\n{src}"));
    let m = match naga::front::wgsl::parse_str(&w.code) {
        Ok(m) => m,
        Err(e) => panic!("{}\n{}", e.emit_to_string(&w.code), w.code),
    };
    if let Err(e) = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty()).validate(&m) {
        panic!("invalid: {e:?}\n{}", w.code);
    }
    w.code
}

fn cs_err(src: &str, target: &str) -> String {
    let mut inc = |_: &str, _: bool| None;
    match super::compile(src, &[], &mut inc, "main", target, 0) {
        Err(e) => e,
        Ok(code) => match dxwgsl::translate_compute(&code, &[]) {
            Err(e) => e,
            Ok(w) => panic!("{target}: compiled and translated\n{src}\n{}", w.code),
        },
    }
}

const NIS_MAIN: &str = include_str!("../../../tests/compute/nis/NIS_Main.hlsl");
const NIS_SCALER: &str = include_str!("../../../tests/compute/nis/NIS_Scaler.h");

/// What compute shaders use: the four thread ID semantics, typed UAVs of every dimension and
/// channel type (stores, loads, compound assignments, ++), groupshared arrays, structs and
/// matrices with every barrier, numthreads at the limits, Gather of each channel, SampleLevel
/// and Load, cs_4_0's limits.
#[test]
fn compute_shaders() {
    let ids = "RWTexture2D<uint4> u : register(u0);\n\
        [numthreads(4, 2, 1)] void main(uint3 t : SV_DispatchThreadID, uint3 g : SV_GroupID, uint3 l : SV_GroupThreadID, uint i : SV_GroupIndex) {\n\
        u[t.xy] = uint4(g.x * 100 + l.x, g.y * 100 + l.y, i, t.z); }";
    let w = cs(ids, &[], "cs_5_0", &[]);
    assert!(w.contains("@compute @workgroup_size(4, 2, 1)"), "{w}");
    assert!(w.contains("texture_storage_2d<rgba32uint, write>"), "{w}");
    for (src, fmts, want) in [
        // (1D textures are 2D ones of height 1 on the page, see render::d3d create_texture2)
        ("RWTexture1D<float4> u : register(u2); [numthreads(64,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.x] = t.x / 64.0; }", vec![], "texture_storage_2d<rgba32float"),
        ("RWTexture2DArray<int4> u : register(u1); [numthreads(8,8,1)] void main(uint3 t : SV_DispatchThreadID) { u[t] = int4(t, 1) - 4; }", vec![], "texture_storage_2d_array<rgba32sint"),
        ("RWTexture3D<float> u : register(u0); [numthreads(4,4,4)] void main(uint3 t : SV_DispatchThreadID) { u[t] = dot(t, 1); }", vec![(0, "r32float")], "texture_storage_3d<r32float"),
        ("RWTexture2D<unorm float4> u : register(u0); [numthreads(1,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = u[t.xy].yzwx * 0.5; }", vec![(0, "rgba8unorm")], "ur0"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(8,8,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] += float4(1, 2, 3, 4); u[t.xy] *= 2; }", vec![(0, "rgba16float")], "rgba16float"),
        ("RWTexture2D<uint> u : register(u0); [numthreads(8,8,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy]++; ++u[t.yx]; }", vec![(0, "r32uint")], "r32uint"),
        ("RWTexture2D<snorm float4> u : register(u3); [numthreads(32,32,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = -1; }", vec![(3, "rgba8snorm")], "workgroup_size(32, 32, 1)"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(1024,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 0; }", vec![], "workgroup_size(1024, 1, 1)"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(1,1,64)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = t.z; }", vec![], "workgroup_size(1, 1, 64)"),
    ] {
        let w = cs(src, &[], "cs_5_0", &fmts);
        assert!(w.contains(want), "{src}: no {want}\n{w}");
    }
    // groupshared
    let shared = "struct P { float3 c; uint n; };\n\
        groupshared float4 a[64]; groupshared P ps[16]; groupshared float2x2 m; groupshared uint total;\n\
        Texture2D<float4> t : register(t0); SamplerState s : register(s0); cbuffer C : register(b0) { float4 k; uint pick; };\n\
        RWTexture2D<float4> u : register(u0);\n\
        [numthreads(8, 8, 1)] void main(uint3 id : SV_DispatchThreadID, uint i : SV_GroupIndex) {\n\
          if (i == 0) { total = 0; m = float2x2(1, 2, 3, 4); }\n\
          a[i] = t.SampleLevel(s, id.xy / 64.0, 0) + t.Load(int3(id.xy, 0)) * k;\n\
          ps[i & 15].c = a[i].xyz; ps[i & 15].n = i;\n\
          GroupMemoryBarrierWithGroupSync();\n\
          float4 sum = 0; [unroll] for (int j = 0; j < 4; j++) sum += a[(i + j * 16) & 63];\n\
          sum.xy = mul(m, sum.xy); sum.z += ps[pick & 15].c.x;\n\
          GroupMemoryBarrier(); DeviceMemoryBarrier(); DeviceMemoryBarrierWithGroupSync(); AllMemoryBarrier(); AllMemoryBarrierWithGroupSync();\n\
          u[id.xy] = sum + float4(t.GatherGreen(s, id.xy / 64.0).x, t.GatherBlue(s, id.xy / 64.0).y, t.GatherAlpha(s, id.xy / 64.0).z, t.GatherRed(s, id.xy / 64.0).w); }";
    let w = cs(shared, &[], "cs_5_0", &[(0, "rgba8unorm")]);
    assert!(w.contains("var<workgroup>") && w.contains("workgroupBarrier()"), "{w}");
    assert!(w.contains("textureGather(1,") && w.contains("textureGather(2,") && w.contains("textureGather(3,"), "{w}");
    // shader model 4 compute: 768 threads, z of 1
    cs("RWTexture2D<float4> u : register(u0); [numthreads(768,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", &[], "cs_4_0", &[]);
}

#[test]
fn compute_errors() {
    for (src, target, want) in [
        ("RWTexture2D<float4> u : register(u0); void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", "cs_5_0", "numthreads"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(0,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", "cs_5_0", "numthreads"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(33,32,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", "cs_5_0", "numthreads"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(1,1,65)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", "cs_5_0", "numthreads"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(1024,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", "cs_4_0", "numthreads"),
        ("RWTexture2D<float4> u : register(u0); [numthreads(1,1,2)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", "cs_4_0", "numthreads"),
        ("RWTexture2D<float4> u : register(u0); groupshared float4 g[2049]; [numthreads(1,1,1)] void main(uint3 t : SV_DispatchThreadID) { g[t.x] = 1; u[t.xy] = g[0]; }", "cs_5_0", "groupshared"),
        ("RWTexture2D<float4> u : register(u0); groupshared float g = 1; [numthreads(1,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = g; }", "cs_5_0", "groupshared"),
        ("RWTexture2D<float4> u : register(t0); [numthreads(1,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", "cs_5_0", "register"),
        ("Texture2D t : register(t0); SamplerState s : register(s0); RWTexture2D<float4> u : register(u0); [numthreads(1,1,1)] void main(uint3 i : SV_DispatchThreadID) { u[i.xy] = t.Sample(s, i.xy); }", "cs_5_0", ""),
        ("RWTexture2D<float4> u : register(u0); [numthreads(1,1,1)] void main(uint3 i : SV_DispatchThreadID) { float x = i.x; u[i.xy] = ddx(x); }", "cs_5_0", ""),
        // (shaderconf --fuzz: a Load of too few coordinates, an array as an operand)
        ("Texture2D t : register(t0); RWTexture2D<float4> u : register(u0); [numthreads(1,1,1)] void main(uint3 i : SV_DispatchThreadID) { u[i.xy] = t.Load((i.x, i.y)); }", "cs_5_0", "coordinates"),
        ("RWTexture2D<uint> u : register(u0); groupshared uint a[4]; [numthreads(4,1,1)] void main(uint i : SV_GroupIndex) { a[i] = i; u[uint2(i, 0)] = a[i] + a; }", "cs_5_0", "types"),
        ("RWTexture2D<uint> u : register(u0); [numthreads(4,1,1)] void main(uint i : SV_GroupIndex) { uint a[2] = { 1, 2 }; u[uint2(i, 0)] = 1 + a; }", "cs_5_0", "types"),
        // (shaderconf --fuzz: a swizzle of a groupshared array recursed until the stack ran out)
        ("RWTexture2D<uint> u : register(u0); groupshared uint a[4]; [numthreads(4,1,1)] void main(uint i : SV_GroupIndex) { a[i] = i; u[uint2(i, 0)] = a.wzyx[i]; }", "cs_5_0", "groupshared array"),
        ("RWTexture2D<uint> u : register(u0); groupshared uint a[4]; [numthreads(4,1,1)] void main(uint i : SV_GroupIndex) { a.x = i; u[uint2(i, 0)] = a[i]; }", "cs_5_0", "groupshared array"),
        // (shaderconf --fuzz seed 447 #265534: NIS with `groupshared float shPixelsY[][38]`; fxc: X3074
        // implicit array missing initial value. hlslc gave dxwgsl a 0-byte declaration)
        ("RWTexture2D<uint> u : register(u0); groupshared uint a[][4]; [numthreads(4,1,1)] void main(uint i : SV_GroupIndex) { a[0][i] = i; u[uint2(i, 0)] = a[0][i]; }", "cs_5_0", "without a size"),
        ("RWTexture2D<uint> u : register(u0); groupshared uint a[]; [numthreads(4,1,1)] void main(uint i : SV_GroupIndex) { u[uint2(i, 0)] = i; }", "cs_5_0", "without a size"),
        ("RWTexture2D<uint> u : register(u0); groupshared uint a[4][]; [numthreads(4,1,1)] void main(uint i : SV_GroupIndex) { u[uint2(i, 0)] = i; }", "cs_5_0", "without a size"),
    ] {
        let e = cs_err(src, target);
        assert!(e.to_lowercase().contains(want), "{src}: {e}");
    }
    // a UAV's storage format of another channel type than the shader's: an error, not WGSL
    // that WebGPU refuses; a pixel shader is no compute shader
    let mut inc = |_: &str, _: bool| None;
    let code = super::compile("RWTexture2D<uint4> u : register(u0); [numthreads(1,1,1)] void main(uint3 t : SV_DispatchThreadID) { u[t.xy] = 1; }", &[], &mut inc, "main", "cs_5_0", 0).unwrap();
    assert!(dxwgsl::translate_compute(&code, &[(0, "rgba8unorm")]).is_err());
    assert!(dxwgsl::translate_compute(&code, &[(0, "rgba8uint")]).is_ok());
    assert_eq!(dxwgsl::uav_slots(&code).unwrap().len(), 1);
    let ps = super::compile("float4 main() : SV_Target { return 1; }", &[], &mut inc, "main", "ps_5_0", 0).unwrap();
    assert!(dxwgsl::translate_compute(&ps, &[]).is_err());
    // compute-only things in a pixel shader
    for src in [
        "groupshared float g[4]; float4 main() : SV_Target { return g[0]; }",
        "float4 main() : SV_Target { GroupMemoryBarrierWithGroupSync(); return 1; }",
    ] {
        assert!(super::compile(src, &[], &mut inc, "main", "ps_5_0", 0).is_err(), "{src}");
    }
}

/// Every variant of the NVIDIA Image Scaling SDK's shader a game can compile (tests/compute/nis):
/// scaler and sharpen-only, SDR / HDR linear / HDR PQ, half precision, viewport support, NV12
/// input, thread groups of 128 and 256, the block sizes of the SDK's own config.
#[test]
fn nis_variants() {
    let mut n = 0;
    for scaler in [1, 0] {
        for hdr in ["0", "1", "2"] {
            for half in ["0", "1"] {
                for vp in ["0", "1"] {
                    for nv12 in ["0", "1"] {
                        for tg in ["128", "256"] {
                            let (bw, bh) = if scaler == 1 { ("32", "24") } else { ("32", "32") };
                            let s = scaler.to_string();
                            let defs = [("NIS_SCALER", s.as_str()), ("NIS_HDR_MODE", hdr), ("NIS_BLOCK_WIDTH", bw), ("NIS_BLOCK_HEIGHT", bh), ("NIS_THREAD_GROUP_SIZE", tg), ("NIS_USE_HALF_PRECISION", half), ("NIS_VIEWPORT_SUPPORT", vp), ("NIS_NV12_SUPPORT", nv12)];
                            let w = cs(NIS_MAIN, &defs, "cs_5_0", &[(0, "rgba8unorm")]);
                            assert!(w.contains(&format!("@workgroup_size({tg}, 1, 1)")), "{defs:?}");
                            n += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(n, 96);
}

/// Multisampled textures (Texture2DMS / Texture2DMSArray of floats and integers): Load with a
/// sample index (ld2dms), GetDimensions (resinfo and sampleinfo), GetSamplePosition
/// (samplepos), to texture_multisampled_2d in WGSL that naga accepts.
#[test]
fn multisampled_textures() {
    const SRC: &str = "Texture2DMS<float4> ms : register(t0); Texture2DMS<uint4, 4> msu : register(t1); Texture2DMSArray<float4> msa : register(t2);
float4 ps(float4 pos : SV_Position, uint si : SV_SampleIndex) : SV_Target {
    uint w, h, n, aw, ah, al, an;
    ms.GetDimensions(w, h, n);
    msa.GetDimensions(aw, ah, al, an);
    float4 c = 0;
    for (uint k = 0; k < n; k++) c += ms.Load(int2(pos.xy), k);
    c += ms.Load(int2(pos.xy), si, int2(1, -1)) + float4(msu.Load(int2(0, 0), 3)) + msa.Load(int3(pos.xy, 0), 1);
    float2 p = ms.GetSamplePosition(si);
    return c / n + float4(p, w + h, aw + ah + al + an);
}";
    for t in ["ps_4_1", "ps_5_0"] {
        let code = compile(SRC, "ps", t);
        let w = wgsl(&code, true, None);
        assert!(w.contains("texture_multisampled_2d<f32>") && w.contains("texture_multisampled_2d<u32>"), "{w}");
        assert!(w.contains("textureNumSamples(t0)") && w.contains("d3d_samplepos("), "{w}");
        assert!(w.matches("textureLoad(t").count() >= 4, "{w}");
        let b = dxwgsl::translate(&code, true, None).unwrap().bindings;
        assert!(b.iter().filter(|x| matches!(x.1, dxwgsl::Binding::Texture(6, _))).count() == 3, "{b:?}");
        assert!(b.contains(&(17, dxwgsl::Binding::Texture(6, 1))), "{b:?}");
    }
}

/// `code` with the instruction `inst` put before its last one (ret), its SHDR/SHEX chunk and the
/// container's chunk offsets after it moved on (the checksum is not read).
fn splice(code: &[u8], inst: &[u32]) -> Vec<u8> {
    let rd = |b: &[u8], at: usize| u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
    let n = rd(code, 28) as usize;
    let offs: Vec<usize> = (0..n).map(|i| rd(code, 32 + 4 * i) as usize).collect();
    let k = offs.iter().position(|&o| &code[o..o + 4] == b"SHEX" || &code[o..o + 4] == b"SHDR").unwrap();
    let (o, size) = (offs[k], rd(code, offs[k] + 4) as usize);
    let at = o + 8 + size - 4;
    assert_eq!(rd(code, at), 0x0100_003e, "ends with ret");
    let mut out = code[..at].to_vec();
    out.extend(inst.iter().flat_map(|w| w.to_le_bytes()));
    out.extend_from_slice(&code[at..]);
    let grow = 4 * inst.len() as u32;
    let put = |b: &mut Vec<u8>, at: usize, v: u32| b[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let total = rd(&out, 24) + grow;
    put(&mut out, 24, total);
    put(&mut out, o + 4, size as u32 + grow);
    let len = rd(&out, o + 12) + inst.len() as u32;
    put(&mut out, o + 12, len);
    for (i, &off) in offs.iter().enumerate() {
        if off > o {
            put(&mut out, 32 + 4 * i, off as u32 + grow);
        }
    }
    out
}

/// Every opcode with 0 to 6 operands of each register type a shader declares (temps, immediates,
/// constant buffers, textures, samplers, UAVs, groupshared memory), in a pixel and a compute
/// shader: the translators give an error or valid WGSL, never a panic (fuzzing found an
/// ld_structured of 3 operands read past them; this is every such count, not a sample).
#[test]
fn every_opcode_with_any_operand_count() {
    let ps = compile(
        "cbuffer C : register(b0) { float4 k; }; Texture2D t : register(t0); SamplerState s : register(s0);\n\
         float4 main(float4 p : SV_Position) : SV_Target { return t.Sample(s, k.xy) + k; }",
        "main",
        "ps_5_0",
    );
    let mut inc = |_: &str, _: bool| None;
    let cs = super::compile(
        "cbuffer C : register(b0) { float4 k; }; Texture2D t : register(t0); SamplerState s : register(s0);\n\
         RWTexture2D<float4> u : register(u1); groupshared uint g[64];\n\
         [numthreads(64, 1, 1)] void main(uint i : SV_GroupIndex) { g[i] = i; GroupMemoryBarrierWithGroupSync();\n\
           u[uint2(i, 0)] = t.SampleLevel(s, k.xy, 0) + g[63 - i] + k; }",
        &[],
        &mut inc,
        "main",
        "cs_5_0",
        0,
    )
    .unwrap();
    // (operand tokens: r0.xyzw / r0 masked, an immediate vector, cb0[0], t0, s0, u1, g0)
    let kinds: [&[u32]; 8] = [
        &[0x0010_0e46, 0],
        &[0x0010_00f2, 0],
        &[0x0000_4002, 0x3f80_0000, 1, 2, 0xffff_ffff],
        &[0x0020_8e46, 0, 0],
        &[0x0010_7e46, 0],
        &[0x0010_6000, 0],
        &[0x0011_ee46, 1],
        &[0x0011_f006, 0],
    ];
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut bad = Vec::new();
    let (mut tried, mut took) = (0, 0);
    for op in (0..0x100u32).filter(|&op| op != 0x35) {
        for n in 0..=6usize {
            for p in 0..kinds.len() * 2 {
                // (all operands of one kind, or a masked r0 destination and the rest of one kind)
                let mut body = Vec::new();
                for j in 0..n {
                    body.extend_from_slice(if p >= kinds.len() && j == 0 { kinds[1] } else { kinds[p % kinds.len()] });
                }
                let mut inst = vec![op | ((body.len() as u32 + 1) << 24)];
                inst.extend(body);
                for (code, compute) in [(&ps, false), (&cs, true)] {
                    let b = splice(code, &inst);
                    tried += 1;
                    let r = std::panic::catch_unwind(|| if compute { dxwgsl::translate_compute(&b, &[]) } else { dxwgsl::translate(&b, true, None) });
                    match r {
                        Err(_) => bad.push(format!("op {op:#x} {n} operands kind {p} compute {compute}: panic")),
                        Ok(Err(_)) => {}
                        Ok(Ok(w)) => {
                            took += 1;
                            if let Err(e) = naga::front::wgsl::parse_str(&w.code).map_err(|e| e.emit_to_string(&w.code)).and_then(|m| {
                                naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all()).validate(&m).map(|_| ()).map_err(|e| format!("{e:?}"))
                            }) {
                                bad.push(format!("op {op:#x} {n} operands kind {p} compute {compute}: invalid WGSL {}", e.lines().next().unwrap_or("")));
                            }
                        }
                    }
                }
            }
        }
    }
    std::panic::set_hook(hook);
    assert!(bad.is_empty(), "{} of {tried} ({took} translated):\n{}", bad.len(), bad.join("\n"));
    assert!(took > 1000, "{took} of {tried} translated: the splicing is wrong");
}

/// Register pressure in the SM3 lowering must fail at compilation, rather
/// than returning bytecode that the D3D9 translator cannot consume.
#[test]
fn sm3_temporary_budget() {
    let mut inc = |_: &str, _: bool| None;
    for target in ["ps_2_0", "ps_3_0"] {
        let within = "float4 ps(float4 v:TEXCOORD0):COLOR {float4 a[25]; a[(int)v.x]=v; return a[(int)v.x];}";
        let code = super::compile(within, &[], &mut inc, "ps", target, 0).unwrap();
        let words: Vec<u32> = code.chunks_exact(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        assert!(dxwgsl::d3d9::Shader::parse(&words).is_some());
        for source in [
            // Ordinary array register beyond r31.
            "float4 ps(float4 v:TEXCOORD0):COLOR {float4 a[40]; a[39]=v; a[(int)v.x]=v; return v;}",
            // Conversion/comparison scratch registers also count.
            "float4 ps(float4 v:TEXCOORD0):COLOR {float4 a[28]; a[(int)v.x]=v; return v;}",
        ] {
            let err = super::compile(source, &[], &mut inc, "ps", target, 0).unwrap_err();
            assert!(err.contains("exceeds r31"), "{target}: {err}");
        }
    }
}

/// Review A-6: shaders fxc rejects (X3013 no overload, X3052 dimensions, a void or mismatched
/// condition) panicked in the lowering, and a panic is the whole VM under panic=abort: each
/// must be an error. Also run on every target, with each source cut at every byte (a
/// truncated shader file), none of which may panic.
#[test]
fn shaders_fxc_rejects_are_errors_not_panics() {
    let srcs = [
        "sampler2D s;\nfloat4 ps(float2 uv : TEXCOORD0) : COLOR { return tex2D(s); }\n",
        "sampler2D s;\nfloat4 ps(float2 uv : TEXCOORD0) : COLOR { return tex2D(s, uv, uv); }\n",
        "sampler2D s;\nfloat4 ps(float2 uv : TEXCOORD0) : COLOR { return tex2Dgrad(s, uv); }\n",
        "sampler2D s;\nfloat4 ps(float2 uv : TEXCOORD0) : COLOR { return tex2Dlod(s, uv); }\n",
        "sampler2D s;\nfloat4 ps(float3 uv : TEXCOORD0) : COLOR { return tex2Dlod(s, uv); }\n",
        "sampler2D s;\nfloat4 ps(float3 uv : TEXCOORD0) : COLOR { return tex2Dbias(s, uv); }\n",
        "sampler2D s;\nfloat4 ps(float4 uv : TEXCOORD0) : COLOR { return tex2Dproj(s, uv.xyw); }\n",
        "sampler3D s;\nfloat4 ps(float2 uv : TEXCOORD0) : COLOR { return tex3D(s, uv); }\n",
        "sampler2D s;\nfloat4 ps(float2 uv : TEXCOORD0) : COLOR { return tex2D(s, uv.x); }\n",
        "sampler2D s;\nfloat4 ps() : COLOR { return tex2D(); }\n",
        "float4 ps() : COLOR { vector<float, 9> v = 0; return v.x; }\n",
        "float4 ps() : COLOR { vector<float, 0> v = 0; return 1; }\n",
        "float4 ps() : COLOR { matrix<float, 1, 9> m = 0; return m._m00; }\n",
        "float4 ps() : COLOR { matrix<float, 200, 200> m = 0; return 1; }\n",
        "float4 ps() : COLOR { vector<float, 4294967297> v = 0; return 1; }\n",
    ];
    for src in srcs {
        for t in ["ps_2_0", "ps_3_0", "ps_4_0", "ps_5_0"] {
            let mut inc = |_: &str, _: bool| None;
            assert!(super::compile(src, &[], &mut inc, "ps", t, 0).is_err(), "{t}: {src}");
        }
    }
    // sizes 1 to 4 still compile
    for src in [
        "float4 ps() : COLOR { vector<float, 1> a = 1; vector<float, 4> b = a.xxxx; return b; }\n",
        "float4 ps() : COLOR { matrix<float, 4, 4> m = 1; matrix<float, 1, 4> n = 2; return m[0] + n[0]; }\n",
        "sampler2D s;\nfloat4 ps(float4 uv : TEXCOORD0) : COLOR { return tex2Dlod(s, uv) + tex2Dbias(s, uv) + tex2Dproj(s, uv) + tex2D(s, uv.xy, uv.xy, uv.zw) + tex2Dgrad(s, uv.xy, uv.xy, uv.zw); }\n",
    ] {
        let mut inc = |_: &str, _: bool| None;
        assert!(super::compile(src, &[], &mut inc, "ps", "ps_3_0", 0).is_ok(), "{src}");
    }
    for src in srcs {
        for cut in 0..src.len() {
            let mut inc = |_: &str, _: bool| None;
            let _ = super::compile(&src[..cut], &[], &mut inc, "ps", "ps_3_0", 0);
        }
    }
}

/// The lowering cases of review A-6 (lower.rs): a matrix chosen by a vector condition and a
/// condition of type void. fxc refuses both; they must not panic.
#[test]
fn mismatched_conditions_are_errors_not_panics() {
    for src in [
"void f() {}\nfloat4 ps() : COLOR { if (f() ? 1 : 2) return 1; return 0; }\n",
        "void f() {}\nfloat4 ps() : COLOR { return f() ? 1 : 2; }\n",
        "void f() {}\nfloat4 ps() : COLOR { if (f()) return 1; return 0; }\n",
    ] {
        for t in ["ps_3_0", "ps_4_0"] {
            let mut inc = |_: &str, _: bool| None;
            let r = super::compile(src, &[], &mut inc, "ps", t, 0);
            assert!(r.is_err(), "{t}: {src}");
        }
    }
    // (the lowering compiles this one, its components as the condition's; what matters is that
    // the matrix has its four components after it)
    let src = "float2x2 a; float2x2 b; bool2 c;\nfloat4 ps() : COLOR { float2x2 m = c ? a : b; return m._m11 + m[1].y + mul(m, float2(1, 2)).x; }\n";
    for t in ["ps_3_0", "ps_4_0"] {
        let mut inc = |_: &str, _: bool| None;
        let _ = super::compile(src, &[], &mut inc, "ps", t, 0);
    }
}
