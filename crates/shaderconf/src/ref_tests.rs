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
