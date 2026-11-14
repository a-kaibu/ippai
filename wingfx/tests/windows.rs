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

/// the state block groups against masu's d3d9sb probe (which block types bring each state back)
#[test]
fn d3d9_groups_match_the_d3d9sb_probe() {
    let p = root().join("../tests/winref/golden/d3d9sb-32/stdout.txt");
    let Ok(g) = std::fs::read_to_string(p) else { return };
    use wingfx::model::d3d9::{all_states, Group, Key};
    let mut n = 0;
    for (k, _, grp) in all_states() {
        // (the probe's names: rs i, tss t (stage 0) / 700+t (stage 7), ss t (sampler 0) / 1500+t ...)
        let name = match k {
            Key::Rs(i) => format!("rs {i}"),
            Key::Tss(0, t) => format!("tss {t}"),
            Key::Tss(7, t) => format!("tss {}", 700 + t),
            Key::Ss(0, t) => format!("ss {t}"),
            Key::Ss(15, t) => format!("ss {}", 1500 + t),
            _ => continue,
        };
        let (a, p, v) = match grp {
            Group::Pixel => (1, 1, 0),
            Group::Vertex => (1, 0, 1),
            Group::Both => (1, 1, 1),
            Group::All => (1, 0, 0),
            Group::Nowhere => (0, 0, 0),
        };
        let want = format!("{name} all {a} pixel {p} vertex {v}");
        assert!(g.lines().any(|l| l == want), "Windows does not say {want:?}");
        n += 1;
    }
    assert!(n > 150);
}

/// the Direct3D 11 model against masu's d3d11desc probe: its tables of descriptions (read from
/// the probe's source) through the model, each line as Windows printed it
#[test]
fn d3d11_matches_the_d3d11desc_probe() {
    let Ok(src) = std::fs::read_to_string(root().join("../tests/winref/probes/d3d11desc.c")) else { return };
    let Ok(g) = std::fs::read_to_string(root().join("../tests/winref/golden/d3d11desc-32/stdout.txt")) else { return };
    use wingfx::model::d3d11::{normalize, Kind};
    let num = |t: &str| -> u32 {
        let t = t.trim();
        if let Some(v) = t.strip_prefix("(DWORD)-") {
            return (v.parse::<i64>().unwrap() as u32).wrapping_neg();
        }
        if let Some(h) = t.strip_prefix("0x") {
            return u32::from_str_radix(h, 16).unwrap();
        }
        t.parse().unwrap()
    };
    let at = |t: &str| -> usize {
        let t = t.trim();
        if let Some(a) = t.strip_prefix("RT(") {
            let (i, f) = a.trim_end_matches(')').split_once(',').unwrap();
            return 2 + 8 * i.trim().parse::<usize>().unwrap() + f.trim().parse::<usize>().unwrap();
        }
        t.parse().unwrap()
    };
    let mut n = 0;
    for (k, table) in [(Kind::Blend, "blends[]"), (Kind::Depth, "depths[]"), (Kind::Raster, "rasters[]"), (Kind::Sampler, "samplers[]")] {
        let body = &src[src.find(table).unwrap()..];
        let body = &body[..body.find("};").unwrap()];
        let mut made: Vec<(String, Vec<u32>)> = Vec::new();
        for l in body.lines().filter(|l| l.trim_start().starts_with("{ \"")) {
            let name = l.split('"').nth(1).unwrap().to_string();
            let mut d = wingfx::gen::d3d11_base(k);
            // { "name", { { at, v }, ... END } }, at maybe RT(i, f)
            for c in l.split("{ ").skip(3) {
                let c = c.split(" }").next().unwrap();
                if let Some((a, v)) = c.rsplit_once(", ") {
                    d[at(a)] = num(v);
                }
            }
            let want = match normalize(k, &d) {
                None => format!("{} {name} 0x80070057 same -", k.name()),
                Some(nd) => {
                    let same = made.iter().find(|(_, o)| *o == nd).map_or("-".to_string(), |(n, _)| n.clone());
                    let words: Vec<String> = nd.iter().map(|w| format!("{w:x}")).collect();
                    made.push((name.clone(), nd));
                    format!("{} {name} 0 {} same {same}", k.name(), words.join(" "))
                }
            };
            assert!(g.lines().any(|l| l == want), "Windows does not say {want:?}");
            n += 1;
        }
    }
    assert!(n > 100, "{n}");
}

/// the Direct3D 8 defaults against masu's d3d8state probe (device 0 software, 1 hardware
/// vertex processing)
#[test]
fn d3d8_defaults_match_the_d3d8state_probe() {
    let Ok(g) = std::fs::read_to_string(root().join("../tests/winref/golden/d3d8state-32/stdout.txt")) else { return };
    let text = format!("case a\ndevice8 swvp\n{0}end\ncase b\ndevice8 hwvp\n{0}end\n", {
        let mut s = String::new();
        for i in 0..256 {
            s += &format!("rs get {i}\n");
        }
        for st in [0, 7] {
            for t in 0..30 {
                s += &format!("tss get {st} {t}\n");
            }
        }
        s
    });
    let mut n = 0;
    for (dev, c) in wingfx::case::parse_cases(&text).iter().enumerate() {
        let out = wingfx::model::run_case(c).unwrap();
        for l in &out[1..] {
            // the probe: "0 rs 7 = 0x1", "0 rs 1: Get 0x8876086c", "0 tss 0 1: 0 = 0x4"
            let t: Vec<&str> = l.split(' ').collect();
            let v = |h: &str| match u32::from_str_radix(h, 16).unwrap() {
                0 => "0".to_string(),
                v => format!("{v:#x}"),
            };
            let want = match (t[0], t.len()) {
                ("rs", 4) if t[2] == "=" => format!("{dev} rs {} = {}", t[1], v(t[3])),
                ("rs", 4) => format!("{dev} rs {}: Get {}", t[1], v(t[3])),
                ("tss", 5) if t[3] == "=" => format!("{dev} tss {} {}: 0 = {}", t[1], t[2], v(t[4])),
                _ => panic!("{l}"),
            };
            assert!(g.lines().any(|l| l == want), "Windows does not say {want:?}");
            n += 1;
        }
    }
    assert_eq!(n, 2 * (256 + 60));
}

/// the WIC model against masu's wicconv probe: its conversions of 8x4 bitmaps (bytes from its
/// own formula), each one as Windows printed it; the pairs the model leaves open are skipped
#[test]
fn wic_matches_the_wicconv_probe() {
    let Ok(g) = std::fs::read_to_string(root().join("../tests/winref/golden/wicconv-32/stdout.txt")) else { return };
    use wingfx::model::wic::{convert, format, stride, FORMATS};
    const LONG: [&str; 23] = ["BlackWhite", "2bppGray", "4bppGray", "8bppGray", "16bppGray", "16bppBGR555", "16bppBGR565", "24bppBGR", "24bppRGB", "32bppBGR", "32bppBGRA", "32bppPBGRA", "32bppRGB", "32bppRGBA", "32bppPRGBA", "48bppRGB", "64bppRGBA", "32bppCMYK", "8bppAlpha", "1bppIndexed", "2bppIndexed", "4bppIndexed", "8bppIndexed"];
    assert_eq!(LONG.len(), FORMATS.len());
    let bytes = |f: usize, seed: u32| -> Vec<u8> { (0..stride(f, 8) * 4).map(|i| ((i as u32 * 73 + seed * 29 + 41) ^ (i as u32 >> 3) * 151) as u8).collect() };
    let mut jobs: Vec<(usize, usize, u32)> = (0..23).map(|f| (f, 10, f as u32)).collect();
    for seed in 0..2 {
        jobs.extend((0..23).map(|f| (10, f, 100 + seed)));
    }
    for (i, (a, b)) in [(7, 11), (3, 11), (11, 10), (14, 11), (16, 11), (15, 7), (4, 3), (6, 7), (13, 7), (22, 11)].into_iter().enumerate() {
        jobs.push((a, b, 200 + i as u32));
    }
    let (mut n, mut open, mut bad) = (0, 0, Vec::new());
    let mut seen = std::collections::HashMap::new();
    for (from, to, seed) in jobs {
        let label = format!("{}>{}: can 1 ", LONG[from], LONG[to]);
        // (the probe prints the same label for both 32bppBGRA seeds: take them in order)
        let k = seen.entry(label.clone()).or_insert(0);
        let Some(line) = g.lines().filter(|l| l.starts_with(&label)).nth(*k) else { panic!("no {label}") };
        *k += 1;
        let want = &line[label.len()..];
        match convert(from, to, 8, 4, &bytes(from, seed)) {
            Ok(o) => {
                let got: String = o.iter().map(|b| format!("{b:02x}")).collect();
                if got != want {
                    bad.push(format!("{label}\n  want {want}\n  got  {got}"));
                }
                n += 1;
            }
            Err(_) => open += 1,
        }
    }
    let _ = format;
    assert!(bad.is_empty(), "{} of {n} differ:\n{}", bad.len(), bad.join("\n"));
    assert!(n > 50, "{n} {open}");
}
