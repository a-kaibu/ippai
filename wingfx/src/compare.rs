//! Comparing two outputs case by case. A case whose wanted lines are "unmodeled ..." (a model's
//! output) is skipped. Differences are reported by case with the first differing line; a
//! differing picture row also names the first differing pixel.

use crate::case::{parse_output, Output};

#[derive(Debug, Default)]
pub struct Report {
    pub cases: usize,
    pub same: usize,
    pub unmodeled: usize,
    pub differ: Vec<String>,
}

fn first_pixel(want: &str, got: &str, bpp: u32) -> Option<usize> {
    let w = want.as_bytes();
    let g = got.as_bytes();
    let i = (0..w.len().min(g.len())).find(|&i| w[i] != g[i]).unwrap_or(w.len().min(g.len()));
    // (hex digits per pixel; below 8 bpp a byte holds several pixels)
    Some(if bpp >= 8 { i / (bpp as usize / 4) } else { i / 2 * (8 / bpp as usize) })
}

pub fn compare(want: &Output, got: &Output) -> Report {
    let mut r = Report::default();
    let mut gi = 0;
    for (name, wl) in &want.cases {
        r.cases += 1;
        if wl.first().is_some_and(|l| l.starts_with("unmodeled")) {
            r.unmodeled += 1;
            if got.cases.iter().any(|(n, _)| n == name) {
                continue;
            }
        }
        // (cases in the same order; a missing one does not shift the rest)
        let found = got.cases[gi.min(got.cases.len())..].iter().position(|(n, _)| n == name);
        let Some(k) = found else {
            r.differ.push(format!("{name}: missing (the program stopped or skipped it)"));
            continue;
        };
        let gl = &got.cases[gi + k].1;
        gi += k + 1;
        if wl.first().is_some_and(|l| l.starts_with("unmodeled")) {
            continue;
        }
        let mut bpp = 0;
        let mut diff = None;
        for i in 0..wl.len().max(gl.len()) {
            let (a, b) = (wl.get(i).map(String::as_str), gl.get(i).map(String::as_str));
            if let Some(px) = a.and_then(|a| a.strip_prefix("px ")) {
                bpp = px.split(' ').nth(3).and_then(|v| v.parse().ok()).unwrap_or(0);
            }
            if a != b {
                let mut d = format!("{name}: line {}: want {:?} got {:?}", i + 1, a.unwrap_or("(none)"), b.unwrap_or("(none)"));
                if let (Some(a), Some(b), true) = (a, b, bpp > 0) {
                    if a.len() == b.len() && a.bytes().all(|c| c.is_ascii_hexdigit()) {
                        let row = (0..i).rev().take_while(|&j| !wl[j].starts_with("px ")).count();
                        if let Some(x) = first_pixel(a, b, bpp) {
                            d += &format!(" (row {row}, first pixel {x})");
                        }
                    }
                }
                diff = Some(d);
                break;
            }
        }
        match diff {
            Some(d) => r.differ.push(d),
            None => r.same += 1,
        }
    }
    if !want.done && !got.done {
        // (both died: nothing more to say)
    } else if want.done != got.done {
        r.differ.push(format!("program end: want {} got {}", if want.done { "done" } else { "a crash" }, if got.done { "done" } else { "a crash" }));
    }
    r
}

pub fn compare_text(want: &str, got: &str) -> Report {
    compare(&parse_output(want), &parse_output(got))
}
