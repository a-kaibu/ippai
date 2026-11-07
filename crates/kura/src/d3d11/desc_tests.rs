//! Windows' answers for state descriptions (tests/winref/golden/d3d11desc-32) checked at once:
//! the probe's own cases (tests/winref/probes/d3d11desc.c, read from its tables) through
//! normalize_state, for the result, the description GetDesc gives and which cases share an
//! object (the same normalized description).

use super::normalize_state;

fn root(p: &str) -> String {
    let p = format!("{}/../../{p}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn num(s: &str) -> u32 {
    let s = s.trim();
    if let Some(v) = s.strip_prefix("(DWORD)-") {
        return 0u32.wrapping_sub(num(v));
    }
    if let Some(h) = s.strip_prefix("0x") {
        return u32::from_str_radix(h, 16).unwrap();
    }
    s.parse().unwrap_or_else(|_| panic!("{s:?}"))
}

/// The cases of `static const struct test <table>[]`: (name, [(word, value)]).
fn cases(src: &str, table: &str) -> Vec<(String, Vec<(usize, u32)>)> {
    let at = src.find(&format!("struct test {table}[] = {{")).unwrap();
    let body = &src[at..at + src[at..].find("\n};").unwrap()];
    let mut out = Vec::new();
    for l in body.lines().skip(1) {
        let Some(q) = l.find('"') else { continue };
        let name = &l[q + 1..q + 1 + l[q + 1..].find('"').unwrap()];
        let mut ch = Vec::new();
        let mut rest = &l[q + name.len() + 2..];
        // "{ at, v }" pairs; at is a number or RT(i, f)
        while let Some(o) = rest.find("{ ") {
            rest = &rest[o + 2..];
            if rest.starts_with('{') || rest.starts_with("END") {
                continue;
            }
            let (word, after) = if let Some(r) = rest.strip_prefix("RT(") {
                let c = r.find(')').unwrap();
                let (i, f) = r[..c].split_once(',').unwrap();
                (2 + 8 * num(i) as usize + num(f) as usize, &r[c + 1..])
            } else {
                let c = rest.find(',').unwrap();
                (num(&rest[..c]) as usize, &rest[c..])
            };
            let after = after.trim_start_matches([',', ' ']);
            let e = after.find(" }").unwrap();
            ch.push((word, num(&after[..e])));
            rest = &after[e..];
        }
        out.push((name.to_string(), ch));
    }
    out
}

#[test]
fn state_descriptions_are_windows() {
    let src = root("tests/winref/probes/d3d11desc.c");
    let golden = root("tests/winref/golden/d3d11desc-32/stdout.txt");
    let mut lines = golden.lines();
    // (the probe's base descriptions: main)
    let mut blend = vec![0u32; 66];
    blend[2..10].copy_from_slice(&[1, 5, 6, 1, 2, 1, 1, 0xf]);
    let depth = vec![1, 1, 2, 0, 0xffff, 1, 1, 1, 8, 1, 1, 1, 8];
    let raster = vec![3, 3, 0, 0, 0, 0, 1, 0, 0, 0];
    let sampler = vec![0x15, 1, 1, 1, 0, 1, 1, 0, 0, 0, 0, 0, 0x7f7fffff];
    let mut bad = Vec::new();
    let mut n = 0;
    for (what, m, base, table) in [("blend", 20, blend, "blends"), ("depth", 21, depth, "depths"), ("raster", 22, raster, "rasters"), ("sampler", 23, sampler, "samplers")] {
        let mut made: Vec<(String, Vec<u32>)> = Vec::new();
        for (name, ch) in cases(&src, table) {
            let mut d = base.clone();
            for (w, v) in ch {
                d[w] = v;
            }
            let ok = normalize_state(m, &mut d).is_some();
            let got = if ok {
                let words: Vec<String> = d.iter().map(|w| format!("{w:x}")).collect();
                let same = made.iter().find(|(_, x)| *x == d).map_or("-".to_string(), |(n, _)| n.clone());
                made.push((name.clone(), d.clone()));
                format!("{what} {name} 0 {} same {same}", words.join(" "))
            } else {
                format!("{what} {name} 0x80070057 same -")
            };
            let want = lines.next().unwrap();
            n += 1;
            if got != want {
                bad.push(format!("want {want}\n  got  {got}"));
            }
        }
    }
    assert!(n > 100, "{n} cases");
    assert!(bad.is_empty(), "{} of {n} cases differ:\n  {}", bad.len(), bad.join("\n  "));
}
