//! Shaders compiled here go through kura's DXBC to WGSL translation and naga's validation.

fn compile(src: &str, entry: &str, target: &str) -> Vec<u8> {
    let mut inc = |_: &str, _: bool| None;
    match super::compile(src, &[], &mut inc, entry, target, 0) {
        Ok(b) => b,
        Err(e) => panic!("{entry}: {e}"),
    }
}

fn wgsl(code: &[u8], pixel: bool, ps: Option<&[u8]>) -> String {
    let w = kura::d3d11::shader::translate(code, pixel, ps).unwrap_or_else(|e| panic!("translate: {e}"));
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
    let sig = kura::d3d11::shader::signature(&v, false);
    assert_eq!(sig.iter().map(|e| (e.name.as_str(), e.reg)).collect::<Vec<_>>(), [("POSITION", 0), ("COLOR", 1)]);
    let osig = kura::d3d11::shader::signature(&v, true);
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

/// D3D9 bytecode through kura's D3D9 translation; (WGSL, assembly)
fn d3d9(src: &str, entry: &str, target: &str, link: u32) -> (String, kura::d3d9::shader::Shader) {
    let code = compile(src, entry, target);
    let words: Vec<u32> = code.chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
    let s = kura::d3d9::shader::Shader::parse(&words).unwrap_or_else(|| panic!("{entry}: kura cannot parse"));
    let g = kura::d3d9::shader::Gen::new(&s);
    let code = if s.pixel {
        g.pixel(&kura::d3d9::shader::Options { link, vs_inputs: Vec::new(), alpha_test: None, fog: false })
    } else {
        let ins = s.inputs.iter().enumerate().map(|(k, i)| (i.0, k as u32, 0)).collect();
        g.vertex(&kura::d3d9::shader::Options { link: 0, vs_inputs: ins, alpha_test: None, fog: false })
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
