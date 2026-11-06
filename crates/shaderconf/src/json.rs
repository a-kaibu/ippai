//! (no serde: the few shapes the runner reads)

use super::{Case, Tol};
use std::fmt::Write;

fn string(s: &mut String, v: &str) {
    s.push('"');
    for c in v.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(s, "\\u{:04x}", c as u32);
            }
            c => s.push(c),
        }
    }
    s.push('"');
}

/// floats as their bits (NaN, infinities and -0 survive JSON)
pub fn case(s: &mut String, c: &Case) {
    s.push_str("{\"name\":");
    string(s, &c.name);
    s.push_str(",\"api\":");
    string(s, c.api);
    s.push_str(",\"rule\":");
    string(s, c.rule);
    let _ = write!(s, ",\"group\":{},\"binding\":{},\"tol\":", c.group, c.binding);
    match c.tol {
        Tol::Exact => s.push_str("[\"exact\"]"),
        Tol::Ulp(n) => {
            let _ = write!(s, "[\"ulp\",{n}]");
        }
        Tol::Abs(e) => {
            let _ = write!(s, "[\"abs\",{e:e}]");
        }
        Tol::Rel(r, a) => {
            let _ = write!(s, "[\"rel\",{r:e},{a:e}]");
        }
    }
    s.push_str(",\"src\":");
    string(s, &c.src);
    s.push_str(",\"wgsl\":");
    string(s, &c.wgsl);
    s.push_str(",\"pixels\":[");
    for (i, (data, want)) in c.pixels.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str("{\"data\":[");
        let words: Vec<String> = data.chunks(4).map(|w| {
            let mut b = [0u8; 4];
            b[..w.len()].copy_from_slice(w);
            u32::from_le_bytes(b).to_string()
        }).collect();
        s.push_str(&words.join(","));
        s.push_str("],\"want\":[");
        for (k, w) in want.iter().enumerate() {
            if k > 0 {
                s.push(',');
            }
            match w {
                None => s.push_str("null"),
                Some(v) => {
                    s.push('[');
                    s.push_str(&v.iter().map(|x| x.to_bits().to_string()).collect::<Vec<_>>().join(","));
                    s.push(']');
                }
            }
        }
        s.push_str("]}");
    }
    s.push_str("]}");
}
