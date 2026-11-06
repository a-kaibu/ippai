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
    let Ok(which) = std::env::var("HLSL_PRINT") else { return };
    let (src, vs, ps) = match which.as_str() {
        "game" => (GAME, "vs_main", "ps_main"),
        "misc" => (MISC, "vs", "ps"),
        "legacy" => (LEGACY, "vs", "ps"),
        _ => (D3D11TEST, "vs_tri", "ps_quad"),
    };
    let (v, p) = pair(src, vs, ps, "5_0");
    eprintln!("{v}\n-----\n{p}");
}
