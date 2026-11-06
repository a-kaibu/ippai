//! Intrinsics written in HLSL (one overload per float vector size), approximations as fxc's.

pub fn source() -> String {
    let mut s = String::new();
    for t in ["float", "float2", "float3", "float4"] {
        s.push_str(&BODY.replace("T ", &format!("{t} ")).replace("(T)", &format!("({t})")));
    }
    s.push_str(MATS);
    s
}

const BODY: &str = r#"
T atan(T x) {
    T a = abs(x); T t = min(a, 1.0) / max(a, 1.0); T t2 = t * t;
    T p = t * (0.99986601 + t2 * (-0.33029950 + t2 * (0.18014100 + t2 * (-0.08513300 + t2 * 0.02083510))));
    p = a > 1.0 ? 1.57079637 - p : p;
    return x < 0.0 ? -p : p;
}
T atan2(T y, T x) {
    T ax = abs(x); T ay = abs(y);
    T t = min(ax, ay) / max(max(ax, ay), 1e-30); T t2 = t * t;
    T p = t * (0.99986601 + t2 * (-0.33029950 + t2 * (0.18014100 + t2 * (-0.08513300 + t2 * 0.02083510))));
    p = ay > ax ? 1.57079637 - p : p;
    p = x < 0.0 ? 3.14159274 - p : p;
    return y < 0.0 ? -p : p;
}
T acos(T x) {
    T a = abs(x);
    T r = sqrt(1.0 - a) * (1.57072878 + a * (-0.21211439 + a * (0.07426100 + a * -0.01872930)));
    return x < 0.0 ? 3.14159274 - r : r;
}
T asin(T x) { return 1.57079637 - acos(x); }
T tan(T x) { return sin(x) / cos(x); }
T exp(T x) { return exp2(x * 1.44269502); }
T log(T x) { return log2(x) * 0.693147182; }
T log10(T x) { return log2(x) * 0.301029980; }
T sinh(T x) { return (exp(x) - exp(-x)) * 0.5; }
T cosh(T x) { return (exp(x) + exp(-x)) * 0.5; }
T tanh(T x) { T e = exp(2.0 * clamp(x, -40.0, 40.0)); return (e - 1.0) / (e + 1.0); }
T smoothstep(T a, T b, T x) { T t = saturate((x - a) / (b - a)); return t * t * (3.0 - 2.0 * t); }
T fmod(T a, T b) { T c = frac(abs(a / b)) * abs(b); return a < 0.0 ? -c : c; }
T degrees(T x) { return x * 57.2957802; }
T radians(T x) { return x * 0.0174532924; }
T ldexp(T x, T e) { return x * exp2(e); }
T reflect(T i, T n) { return i - 2.0 * dot(n, i) * n; }
T refract(T i, T n, float eta) {
    float c = dot(i, n); float k = 1.0 - eta * eta * (1.0 - c * c);
    return k < 0.0 ? (T)0 : eta * i - (eta * c + sqrt(k)) * n;
}
T faceforward(T n, T i, T ng) { return dot(ng, i) < 0.0 ? n : -n; }
"#;

const MATS: &str = r#"
float determinant(float2x2 m) { return m._11 * m._22 - m._12 * m._21; }
float determinant(float3x3 m) { return dot(m[0], cross(m[1], m[2])); }
float determinant(float4x4 m) {
    float3x3 a = float3x3(m._22_23_24, m._32_33_34, m._42_43_44);
    float3x3 b = float3x3(m._21_23_24, m._31_33_34, m._41_43_44);
    float3x3 c = float3x3(m._21_22_24, m._31_32_34, m._41_42_44);
    float3x3 d = float3x3(m._21_22_23, m._31_32_33, m._41_42_43);
    return m._11 * determinant(a) - m._12 * determinant(b) + m._13 * determinant(c) - m._14 * determinant(d);
}
"#;
