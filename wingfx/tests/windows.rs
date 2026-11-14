//! The models against Windows' recorded answers, and the case files against their generators.
//!
//! Windows' answers: WINGFX_GOLDEN=<dir> holding wingfx_<suite>-<arch>/stdout.txt (default: the
//! masu repository's tests/winref/golden, where its Windows runner records them). A suite with
//! no answer yet is reported and skipped.
use std::path::PathBuf;
use wingfx::{case, compare, gen, model};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn golden(suite: &str, arch: u32) -> Option<String> {
    let dir = std::env::var_os("WINGFX_GOLDEN").map(PathBuf::from).unwrap_or_else(|| root().join("../tests/winref/golden"));
    std::fs::read_to_string(dir.join(format!("wingfx_{suite}-{arch}/stdout.txt"))).ok()
}

#[test]
fn case_files_are_what_the_generators_make() {
    for s in gen::SUITES {
        let f = std::fs::read_to_string(root().join(format!("cases/{s}.txt"))).unwrap();
        assert!(f == gen::suite(s).unwrap(), "cases/{s}.txt is stale: cargo run -- gen {s} > cases/{s}.txt");
    }
}

#[test]
fn models_match_windows() {
    let mut bad = Vec::new();
    for s in gen::SUITES {
        let cases = case::parse_cases(&gen::suite(s).unwrap());
        let want = model::run(s, &cases);
        for arch in [32, 64] {
            let Some(g) = golden(s, arch) else {
                eprintln!("wingfx_{s}-{arch}: no answer from Windows yet");
                continue;
            };
            let r = compare::compare_text(&want, &g);
            eprintln!("wingfx_{s}-{arch}: {} cases, {} as Windows, {} unmodeled, {} differ", r.cases, r.same, r.unmodeled, r.differ.len());
            bad.extend(r.differ.iter().map(|d| format!("{s}-{arch} {d}")));
        }
    }
    assert!(bad.is_empty(), "the model differs from Windows:\n{}", bad.join("\n"));
}

/// the D3D9 defaults table (from the documentation) against an independent Windows run, masu's
/// d3d9state probe, when it is there
#[test]
fn d3d9_defaults_match_the_d3d9state_probe() {
    let p = root().join("../tests/winref/golden/d3d9state-32/stdout.txt");
    let Ok(g) = std::fs::read_to_string(p) else { return };
    use wingfx::model::d3d9::{all_states, Key};
    let mut n = 0;
    for (k, d, _) in all_states() {
        let want = match k {
            Key::Rs(i) => format!("rs {i} = {d:#x}"),
            Key::Tss(s, t) if [0, 1, 7].contains(&s) => format!("tss {s} {t}: 0 = {d:#x}"),
            Key::Ss(s, t) if [0, 15, 256, 257, 260].contains(&s) => format!("ss {s} {t}: 0 = {d:#x}"),
            _ => continue,
        };
        // (printf's %#x prints 0 as "0")
        let want = want.replace("= 0x0", "= 0");
        assert!(g.lines().any(|l| l == want), "Windows does not say {want:?}");
        n += 1;
    }
    assert!(n > 200);
}
