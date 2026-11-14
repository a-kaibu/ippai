//! Compute shader cases for tests/compute.mjs (`shaderconf --compute`): the NVIDIA Image Scaling
//! SDK's shader (tests/compute/nis) through hlslc and dxwgsl, next to Slang's translation of the
//! same source (tests/compute/slang: an independent HLSL compiler), run on the same inputs; what
//! the two store must agree within a byte. And shaders whose answer is known exactly.

use std::fmt::Write;
use std::path::Path;

/// NIS_Config.h NVScalerUpdateConfig / NVSharpenUpdateConfig (SDK 1.0.3), whole viewports: the
/// constant buffer's words.
fn config(sharp: f32, iw: u32, ih: u32, ow: u32, oh: u32, hdr: u32) -> Vec<u32> {
    let s = sharp.clamp(0.0, 1.0) - 0.5;
    let (max_scale, min_scale, limit_scale) = if s >= 0.0 { (1.25, 1.25, 1.25) } else { (1.75, 1.0, 1.0) };
    let (mut thres, mut min_c, mut max_c, mut sy, mut ey) = (64.0 / 1024.0, 2.0f32, 10.0f32, 0.45f32, 0.9f32);
    let mut smin = (0.4 + s * min_scale * 1.2f32).max(0.0);
    let mut smax = 1.6 + s * max_scale * 1.8;
    let mut lmin = (0.14 + s * limit_scale * 0.32f32).max(0.1);
    let mut lmax = 0.5 + s * limit_scale * 0.6;
    if hdr != 0 {
        (thres, min_c, max_c) = (32.0 / 1024.0, 1.5, 5.0);
        smin = (0.4 + s * min_scale * 1.1f32).max(0.0);
        smax = 2.2 + s * max_scale * 1.8;
        lmin = (0.10 + s * limit_scale * 0.28f32).max(0.06);
        lmax = 0.6 + s * limit_scale * 0.6;
        (sy, ey) = if hdr == 2 { (0.35, 0.55) } else { (0.3, 0.5) };
    }
    let f = [
        2.0 * 1127.0 / 1024.0,
        thres,
        min_c,
        1.0 / (max_c - min_c),
        1.0,
        1.0 / 255.0,
        sy,
        1.0 / (ey - sy),
        smin,
        smax - smin,
        lmin,
        lmax - lmin,
        iw as f32 / ow as f32,
        ih as f32 / oh as f32,
        1.0 / ow as f32,
        1.0 / oh as f32,
        1.0 / iw as f32,
        1.0 / ih as f32,
    ];
    let mut w: Vec<u32> = f.iter().map(|x| x.to_bits()).collect();
    w.extend_from_slice(&[0, 0, iw, ih, 0, 0, ow, oh, 0, 0]);
    w
}

/// noise RGBA bytes, alpha 255 (few exact ties between the scaler's gradients)
fn noise(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut x = seed.wrapping_mul(2_654_435_761);
    (0..w * h * 4)
        .map(|i| {
            x = x.wrapping_mul(1_103_515_245).wrapping_add(12345) & 0x7fff_ffff;
            if i % 4 == 3 { 255 } else { (x >> 16) as u8 }
        })
        .collect()
}

fn list<T: std::fmt::Display>(v: &[T]) -> String {
    v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(",")
}

fn string(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// The prefix sum of the groupshared case (a barrier in a loop counted in a register).
const SCAN: &str = "RWTexture2D<uint> u : register(u1); groupshared uint a[64], b[64];\n\
        [numthreads(64, 1, 1)] void main(uint3 t : SV_DispatchThreadID, uint i : SV_GroupIndex, uint3 g : SV_GroupID) {\n\
          a[i] = (i * 7 + g.y * 3) % 13; GroupMemoryBarrierWithGroupSync();\n\
          [loop] for (uint s = 1; s < 64; s <<= 1) {\n\
            b[i] = i >= s ? a[i] + a[i - s] : a[i]; GroupMemoryBarrierWithGroupSync();\n\
            a[i] = b[i]; GroupMemoryBarrierWithGroupSync(); }\n\
          u[t.xy] = a[63 - i] * 1000 + a[i]; }\n";

/// Compute shader sources for the fuzzer (entry main, cs_5_0, nothing to include): the scan, a
/// read-modify-write of a UAV, and the NIS scaler and sharpener with their defines and header
/// written in.
pub fn fuzz_sources() -> Vec<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/compute/nis");
    let main = std::fs::read_to_string(dir.join("NIS_Main.hlsl")).unwrap_or_default();
    let header = std::fs::read_to_string(dir.join("NIS_Scaler.h")).unwrap_or_default();
    let mut out = vec![
        SCAN.to_string(),
        "Texture2D<float4> t : register(t0); RWTexture2D<unorm float4> u : register(u0);\n\
         [numthreads(8, 8, 1)] void main(uint3 i : SV_DispatchThreadID, uint3 l : SV_GroupThreadID) {\n\
           u[i.xy] = u[i.xy] * 0.5 + t.Load(int3(i.xy, 0)) * float(l.x) / 8; }\n"
            .to_string(),
    ];
    for (sc, hdr, half, vp) in [(1, 0, 0, 0), (0, 0, 0, 1), (1, 1, 1, 0)] {
        let (bw, bh) = if sc == 1 { (32, 24) } else { (32, 32) };
        let defs = format!(
            "#define NIS_SCALER {sc}\n#define NIS_HDR_MODE {hdr}\n#define NIS_BLOCK_WIDTH {bw}\n#define NIS_BLOCK_HEIGHT {bh}\n\
             #define NIS_THREAD_GROUP_SIZE 128\n#define NIS_VIEWPORT_SUPPORT {vp}\n#define NIS_USE_HALF_PRECISION {half}\n"
        );
        out.push(defs + &main.replace("#include \"NIS_Scaler.h\"", &header));
    }
    out
}

/// The cases as compute.mjs reads them.
pub fn cases() -> Result<String, String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/compute");
    let read = |p: &str| std::fs::read(dir.join(p)).map_err(|e| format!("{p}: {e}"));
    let text = |p: &str| std::fs::read_to_string(dir.join(p)).map_err(|e| format!("{p}: {e}"));
    let main = text("nis/NIS_Main.hlsl")?;
    let scaler_h = text("nis/NIS_Scaler.h")?;
    let floats = |b: Vec<u8>| list(&b.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).map(|f| format!("{f:e}")).collect::<Vec<_>>());
    let (coef_scale, coef_usm) = (floats(read("nis/coef_scale.f32")?), floats(read("nis/coef_usm.f32")?));
    let mut out = Vec::new();
    // (name, scaler, in w h, out w h, sharpness, hdr, thread group, half precision, viewport)
    let all: [(&str, u32, u32, u32, u32, u32, f32, u32, u32, u32, u32); 6] = [
        ("nis scaler 64x48->96x72 s0.5 tg128", 1, 64, 48, 96, 72, 0.5, 0, 128, 0, 0),
        ("nis scaler 50x30->133x77 s1.0 tg256", 1, 50, 30, 133, 77, 1.0, 0, 256, 0, 0),
        ("nis scaler 64x48->96x72 s0.0 hdr1 tg128", 1, 64, 48, 96, 72, 0.0, 1, 128, 0, 0),
        ("nis scaler 40x40->70x55 s0.3 hdr2 half tg128", 1, 40, 40, 70, 55, 0.3, 2, 128, 1, 0),
        ("nis sharpen 70x50 s0.5 tg256", 0, 70, 50, 70, 50, 0.5, 0, 256, 0, 0),
        ("nis sharpen 33x65 s1.0 tg128 vp", 0, 33, 65, 33, 65, 1.0, 0, 128, 0, 1),
    ];
    for (k, &(name, sc, iw, ih, ow, oh, sharp, hdr, tg, half, vp)) in all.iter().enumerate() {
        let (bw, bh) = if sc == 1 { (32, 24) } else { (32, 32) };
        let defs: Vec<(String, String)> = [
            ("NIS_SCALER", sc),
            ("NIS_HDR_MODE", hdr),
            ("NIS_BLOCK_WIDTH", bw),
            ("NIS_BLOCK_HEIGHT", bh),
            ("NIS_THREAD_GROUP_SIZE", tg),
            ("NIS_VIEWPORT_SUPPORT", vp),
            ("NIS_USE_HALF_PRECISION", half),
        ]
        .iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let mut inc = |n: &str, _: bool| (n == "NIS_Scaler.h").then(|| scaler_h.clone());
        let code = hlslc::compile(&main, &defs, &mut inc, "main", "cs_5_0", 0).map_err(|e| format!("{name}: hlslc: {e}"))?;
        let ours = dxwgsl::translate_compute(&code, &[(0, "rgba8unorm")]).map_err(|e| format!("{name}: dxwgsl: {e}"))?.code;
        let slang = text(&format!("slang/nis_s{sc}_h{hdr}_t{tg}_p{half}_v{vp}.wgsl"))?;
        let img = noise(iw, ih, 2 * k as u32 + 1);
        let mut b = vec![format!(r#"{{"binding":0,"uniform":[{}]}}"#, list(&config(sharp, iw, ih, ow, oh, hdr)))];
        b.push(format!(r#"{{"binding":16,"texture":{{"format":"rgba8unorm","w":{iw},"h":{ih},"bytes":[{}]}}}}"#, list(&img)));
        if sc == 1 {
            b.push(format!(r#"{{"binding":17,"texture":{{"format":"rgba32float","w":2,"h":64,"f32":[{coef_scale}]}}}}"#));
            b.push(format!(r#"{{"binding":18,"texture":{{"format":"rgba32float","w":2,"h":64,"f32":[{coef_usm}]}}}}"#));
        }
        b.push(r#"{"binding":160,"sampler":{"filter":"linear","address":"clamp-to-edge"}}"#.into());
        b.push(format!(r#"{{"binding":200,"storage":{{"format":"rgba8unorm","w":{ow},"h":{oh}}}}}"#));
        out.push(format!(
            r#"{{"name":{},"dispatch":[{},{},1],"out":200,"tol":1,"variants":[{{"name":"masu","wgsl":{}}},{{"name":"slang","wgsl":{}}}],"bindings":[{}]}}"#,
            string(name),
            ow.div_ceil(bw),
            oh.div_ceil(bh),
            string(&ours),
            string(&slang),
            b.join(",")
        ));
    }
    // a prefix sum in groupshared memory with a barrier in a loop counted in a register (WGSL's
    // uniformity rules: a barrier only in uniform control flow), against its answer
    let code = hlslc::compile(SCAN, &[], &mut |_, _| None, "main", "cs_5_0", 0).map_err(|e| format!("scan: {e}"))?;
    let ours = dxwgsl::translate_compute(&code, &[(1, "r32uint")]).map_err(|e| format!("scan: {e}"))?.code;
    let mut want = Vec::new();
    for gy in 0..4u32 {
        let a: Vec<u32> = (0..64).map(|i| (i * 7 + gy * 3) % 13).collect();
        let s: Vec<u32> = (0..64).map(|i| a[..=i].iter().sum()).collect();
        for i in 0..64 {
            want.extend_from_slice(&(s[63 - i] * 1000 + s[i]).to_le_bytes());
        }
    }
    out.push(format!(
        r#"{{"name":"groupshared scan, barriers in a loop","dispatch":[1,4,1],"out":201,"want":[{}],"variants":[{{"name":"masu","wgsl":{}}}],"bindings":[{{"binding":201,"storage":{{"format":"r32uint","w":64,"h":4}}}}]}}"#,
        list(&want),
        string(&ours)
    ));
    Ok(format!("[\n{}\n]\n", out.join(",\n")))
}

#[cfg(test)]
mod tests {
    /// (the cases build: every variant compiles and translates, the fixtures are there)
    #[test]
    fn compute_cases_build() {
        let s = super::cases().unwrap();
        assert_eq!(s.matches("\"dispatch\"").count(), 7);
    }
}
