//! The reference interpreters against the cases: every DXBC and D3D9 case's bytecode, run on
//! each pixel's constants, must give what the case's rule says (the same comparison as
//! tests/shaders.mjs). The cases' expectations come from the specs and the interpreters from
//! the specs too, written apart: where both hold, hlslc's bytecode does what its source says.

use super::cases;
use super::oracle::{close, interpret};

#[test]
fn interpreters_give_what_the_cases_expect() {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    cases::all(&mut out, &mut errors);
    let (mut ran, mut skipped) = (0, Vec::new());
    let mut bad = Vec::new();
    for c in out.iter().filter(|c| c.api != "glsl") {
        for (n, (data, want)) in c.pixels.iter().enumerate() {
            let got = match interpret(c, data) {
                Ok((g, _)) => g,
                Err(e) => {
                    skipped.push(format!("{}: {e}", c.name));
                    break;
                }
            };
            ran += 1;
            for k in 0..4 {
                if let Some(w) = &want[k] {
                    if !w.iter().any(|&w| close(got[k], w, c.tol)) {
                        bad.push(format!("{} pixel {n} channel {k}: got {} want {:?}", c.name, got[k], w));
                    }
                }
            }
        }
    }
    eprintln!("{ran} pixels interpreted; {} cases outside the interpreters", skipped.len());
    for s in &skipped {
        eprintln!("  skipped {s}");
    }
    assert!(bad.is_empty(), "{} differ:\n{}", bad.len(), bad[..bad.len().min(60)].join("\n"));
}

/// WGSL may drop the sign of a zero: where a -0 decides the output (a movc on its bits), the
/// oracle has no single answer and says so; where it washes out, the result stands (oracle seed
/// 447 #587: max(-18, -0.0) into a movc, +0 on SwiftShader).
#[test]
fn the_sign_of_a_zero_is_no_answer() {
    let run = |src: &str, a: f32| {
        let mut inc = |_: &str, _: bool| None;
        let code = hlslc::compile(src, &[], &mut inc, "ps", "ps_4_0", 0).unwrap();
        let p = super::ref_dxbc::parse(&code).unwrap();
        let (o, t) = super::ref_dxbc::run(&p, &[vec![[a.to_bits(), 0, 0, 0]]]).unwrap();
        (f32::from_bits(o[0][0]), t)
    };
    let bits = "cbuffer C : register(b0) { float4 A; };\nfloat4 ps() : SV_Target { float z = max(A.x, -0.0); return (asuint(z) != 0) ? 1 : 2; }\n";
    let (_, t) = run(bits, -18.0);
    assert!(t.is_some_and(|t| t.contains("sign of a zero")));
    let (_, t) = run("cbuffer C : register(b0) { float4 A; };\nfloat4 ps() : SV_Target { float z = min(A.x, 0.0); float w = max(z, -0.0); return (asuint(w) != 0) ? 1 : 2; }\n", -0.0);
    assert!(t.is_some_and(|t| t.contains("sign of a zero")));
    assert_eq!(run(bits, 5.0), (1.0, None));
    assert_eq!(run("cbuffer C : register(b0) { float4 A; };\nfloat4 ps() : SV_Target { return max(A.x, -0.0) + 1; }\n", -18.0), (1.0, None));
}
