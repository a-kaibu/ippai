//! A Direct3D 9 shader assembler (vs_1_1 .. vs_3_0, ps_1_1 .. ps_3_0 assembly to the bytecode
//! CreateVertexShader/CreatePixelShader take), for D3DXAssembleShader.

fn op_code(name: &str) -> Option<u32> {
    Some(match name {
        "nop" => 0,
        "mov" => 1,
        "add" => 2,
        "sub" => 3,
        "mad" => 4,
        "mul" => 5,
        "rcp" => 6,
        "rsq" => 7,
        "dp3" => 8,
        "dp4" => 9,
        "min" => 10,
        "max" => 11,
        "slt" => 12,
        "sge" => 13,
        "exp" => 14,
        "log" => 15,
        "lit" => 16,
        "dst" => 17,
        "lrp" => 18,
        "frc" => 19,
        "m4x4" => 20,
        "m4x3" => 21,
        "m3x4" => 22,
        "m3x3" => 23,
        "m3x2" => 24,
        "call" => 25,
        "callnz" => 26,
        "loop" => 27,
        "ret" => 28,
        "endloop" => 29,
        "label" => 30,
        "dcl" => 31,
        "pow" => 32,
        "crs" => 33,
        "sgn" => 34,
        "abs" => 35,
        "nrm" => 36,
        "sincos" => 37,
        "rep" => 38,
        "endrep" => 39,
        "if" => 40,
        "ifc" => 41,
        "else" => 42,
        "endif" => 43,
        "break" => 44,
        "breakc" => 45,
        "mova" => 46,
        "defb" => 47,
        "defi" => 48,
        "texcoord" | "texcrd" => 64,
        "texkill" => 65,
        "tex" | "texld" | "texldp" | "texldb" => 66,
        "texbem" => 67,
        "texbeml" => 68,
        "texreg2ar" => 69,
        "texreg2gb" => 70,
        "texm3x2pad" => 71,
        "texm3x2tex" => 72,
        "texm3x3pad" => 73,
        "texm3x3tex" => 74,
        "texm3x3spec" => 76,
        "texm3x3vspec" => 77,
        "expp" => 78,
        "logp" => 79,
        "cnd" => 80,
        "def" => 81,
        "texreg2rgb" => 82,
        "texdp3tex" => 83,
        "texm3x2depth" => 84,
        "texdp3" => 85,
        "texm3x3" => 86,
        "texdepth" => 87,
        "cmp" => 88,
        "bem" => 89,
        "dp2add" => 90,
        "dsx" => 91,
        "dsy" => 92,
        "texldd" => 93,
        "setp" => 94,
        "texldl" => 95,
        "breakp" => 96,
        "phase" => 0xfffd,
        _ => return None,
    })
}

/// Instructions with no destination (all operands are sources).
fn no_dest(op: u32) -> bool {
    // (texkill's operand is a destination token)
    matches!(op, 0 | 25 | 26 | 27 | 28 | 29 | 30 | 38 | 39 | 40 | 41 | 42 | 43 | 44 | 45 | 96 | 0xfffd)
}

const USAGES: &[&str] =
    &["position", "blendweight", "blendindices", "normal", "psize", "texcoord", "tangent", "binormal", "tessfactor", "positiont", "color", "fog", "depth", "sample"];

struct Ver {
    pixel: bool,
    major: u32,
    minor: u32,
}

fn reg_bits(ty: u32, n: u32) -> u32 {
    (n & 0x7ff) | (ty & 7) << 28 | ((ty >> 3) & 3) << 11
}

/// A register name: (type, number).
fn register(s: &str, v: &Ver) -> Result<(u32, u32), String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    let num = |p: &str| -> Result<u32, String> {
        if p.is_empty() {
            return Ok(0);
        }
        p.parse::<u32>().map_err(|_| format!("bad register {s}"))
    };
    let named: &[(&str, u32, u32)] = &[("opos", 4, 0), ("ofog", 4, 1), ("opts", 4, 2), ("odepth", 9, 0), ("al", 15, 0), ("vpos", 17, 0), ("vface", 17, 1)];
    for &(n, t, r) in named {
        if lower == n {
            return Ok((t, r));
        }
    }
    let pre = |p: &str| lower.strip_prefix(p);
    if let Some(r) = pre("od") {
        return Ok((5, num(r)?));
    }
    if let Some(r) = pre("ot") {
        return Ok((6, num(r)?));
    }
    if let Some(r) = pre("oc") {
        return Ok((8, num(r)?));
    }
    if let Some(r) = pre("o") {
        return Ok((6, num(r)?));
    }
    let (c, rest) = lower.split_at(1);
    Ok(match c {
        "r" => (0, num(rest)?),
        "v" => (1, num(rest)?),
        "c" => (2, num(rest)?),
        "a" => (3, num(rest)?),
        "t" if v.pixel => (3, num(rest)?),
        "t" => (6, num(rest)?),
        "s" => (10, num(rest)?),
        "i" => (7, num(rest)?),
        "b" => (14, num(rest)?),
        "p" => (19, num(rest)?),
        _ => return Err(format!("unknown register {s}")),
    })
}

fn comp(ch: char) -> Option<u32> {
    match ch {
        'x' | 'r' => Some(0),
        'y' | 'g' => Some(1),
        'z' | 'b' => Some(2),
        'w' | 'a' => Some(3),
        _ => None,
    }
}

fn swizzle(s: &str) -> Result<u32, String> {
    let c: Vec<u32> = s.chars().map(|ch| comp(ch).ok_or(format!("bad swizzle .{s}"))).collect::<Result<_, _>>()?;
    if c.is_empty() || c.len() > 4 {
        return Err(format!("bad swizzle .{s}"));
    }
    let mut v = 0;
    for i in 0..4 {
        v |= c[i.min(c.len() - 1)] << (2 * i);
    }
    Ok(v)
}

fn write_mask(s: &str) -> Result<u32, String> {
    let mut m = 0;
    for ch in s.chars() {
        m |= 1 << comp(ch).ok_or(format!("bad write mask .{s}"))?;
    }
    Ok(m)
}

/// Splits "reg.sel" (and an index "c[a0.x + 3]" / "c3[a0.x]") into (register text, relative
/// address register text and offset, selector).
fn split_operand(s: &str) -> (String, Option<String>, String) {
    let s = s.trim();
    if let Some(open) = s.find('[') {
        let close = s.rfind(']').unwrap_or(s.len());
        let base = &s[..open];
        let inside = &s[open + 1..close];
        let after = s[close + 1..].trim_start_matches('.').to_string();
        // (inside: "a0.x + 5", "a0.x", "5", "aL + 2")
        let mut off: i64 = base[1..].parse().unwrap_or(0);
        let mut rel = None;
        for part in inside.split('+') {
            let p = part.trim();
            if let Ok(n) = p.parse::<i64>() {
                off += n;
            } else if !p.is_empty() {
                rel = Some(p.to_string());
            }
        }
        return (format!("{}{}", &base[..1], off), rel, after);
    }
    match s.split_once('.') {
        Some((r, sel)) => (r.to_string(), None, sel.to_string()),
        None => (s.to_string(), None, String::new()),
    }
}

fn source(s: &str, v: &Ver, out: &mut Vec<u32>) -> Result<(), String> {
    let mut t = s.trim().to_ascii_lowercase();
    let mut m = 0;
    if let Some(r) = t.strip_prefix("1-") {
        t = r.trim().to_string();
        m = 6;
    }
    let mut neg = false;
    if let Some(r) = t.strip_prefix('-') {
        t = r.trim().to_string();
        neg = true;
    }
    if let Some(r) = t.strip_prefix('!') {
        t = r.to_string();
        m = 13;
    }
    // suffix modifiers (before the selector: r0_bx2.x, r0_abs)
    let (head, sel) = match t.find('.') {
        Some(i) if !t[..i].contains('[') => (t[..i].to_string(), t[i + 1..].to_string()),
        _ => (t.clone(), String::new()),
    };
    let (head, suffix) = match head.find('_') {
        Some(i) => (head[..i].to_string(), head[i + 1..].to_string()),
        None => (head, String::new()),
    };
    match suffix.as_str() {
        "" => {}
        "bias" => m = 2,
        "bx2" => m = 4,
        "x2" => m = 7,
        "dz" | "db" => m = 9,
        "dw" | "da" => m = 10,
        "abs" => m = 11,
        x => return Err(format!("source modifier _{x}")),
    }
    if neg {
        m = match m {
            0 => 1,
            2 => 3,
            4 => 5,
            7 => 8,
            11 => 12,
            x => x,
        };
    }
    let full = if sel.is_empty() { head } else { format!("{head}.{sel}") };
    let (reg, rel, sel) = split_operand(&full);
    let (ty, n) = register(&reg, v)?;
    let sw = if sel.is_empty() { 0xe4 } else { swizzle(&sel)? };
    let mut tok = 0x8000_0000 | reg_bits(ty, n) | sw << 16 | m << 24;
    if rel.is_some() {
        tok |= 1 << 13;
    }
    out.push(tok);
    if let Some(r) = rel {
        if v.major >= 2 {
            let (rr, _, rsel) = split_operand(&r);
            let (rt, rn) = register(&rr, v)?;
            let c = rsel.chars().next().and_then(comp).unwrap_or(0);
            out.push(0x8000_0000 | reg_bits(rt, rn) | (c * 0x55) << 16);
        }
    }
    Ok(())
}

fn dest(s: &str, v: &Ver, resmod: u32, shift: u32, out: &mut Vec<u32>) -> Result<(), String> {
    let (reg, rel, sel) = split_operand(&s.trim().to_ascii_lowercase());
    let (ty, n) = register(&reg, v)?;
    let mask = if sel.is_empty() { 0xf } else { write_mask(&sel)? };
    let mut tok = 0x8000_0000 | reg_bits(ty, n) | mask << 16 | resmod << 20 | shift << 24;
    if rel.is_some() {
        tok |= 1 << 13;
    }
    out.push(tok);
    if let Some(r) = rel {
        let (rr, _, rsel) = split_operand(&r);
        let (rt, rn) = register(&rr, v)?;
        let c = rsel.chars().next().and_then(comp).unwrap_or(0);
        out.push(0x8000_0000 | reg_bits(rt, rn) | (c * 0x55) << 16);
    }
    Ok(())
}

fn number(s: &str) -> Result<u32, String> {
    let t = s.trim();
    match t {
        "true" => return Ok(1),
        "false" => return Ok(0),
        _ => {}
    }
    t.trim_end_matches('f').parse::<f32>().map(f32::to_bits).map_err(|_| format!("bad number {t}"))
}

/// Assembles `src`; the token words.
pub fn assemble(src: &str) -> Result<Vec<u32>, String> {
    let mut words = Vec::new();
    let mut ver: Option<Ver> = None;
    // (statements: lines, also split at ';' only when ';' starts a comment)
    for raw in src.lines() {
        let line = raw.split("//").next().unwrap_or("");
        let line = line.split(';').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (mnem, rest) = match line.find(|c: char| c.is_whitespace()) {
            Some(i) => (&line[..i], line[i..].trim()),
            None => (line, ""),
        };
        let mnem_l = mnem.to_ascii_lowercase();
        if ver.is_none() {
            let m = mnem_l.replace('.', "_");
            let parts: Vec<&str> = m.split('_').collect();
            if parts.len() == 3 && (parts[0] == "vs" || parts[0] == "ps") {
                let pixel = parts[0] == "ps";
                let major: u32 = parts[1].parse().map_err(|_| format!("bad version {mnem}"))?;
                let minor: u32 = if parts[2] == "x" || parts[2] == "sw" { 0 } else { parts[2].parse().map_err(|_| format!("bad version {mnem}"))? };
                words.push(if pixel { 0xffff_0000 } else { 0xfffe_0000 } | major << 8 | minor);
                ver = Some(Ver { pixel, major, minor });
                continue;
            }
            return Err(format!("no shader version before {line}"));
        }
        let v = ver.as_ref().unwrap();
        let _ = v.minor;
        let (coissue, mnem_l) = match mnem_l.strip_prefix('+') {
            Some(m) => (true, m.to_string()),
            None => (false, mnem_l),
        };
        // mnemonic modifiers: op_sat_pp, op_x2, dcl_usageN, dcl_2d, ifc_gt
        let mut parts = mnem_l.split('_');
        let base = parts.next().unwrap_or("");
        let mods: Vec<&str> = parts.collect();
        let Some(op) = op_code(base) else {
            return Err(format!("unknown instruction {mnem}"));
        };
        let operands: Vec<String> = if rest.is_empty() { Vec::new() } else { split_args(rest) };
        let mut body: Vec<u32> = Vec::new();
        let mut ctl = 0u32;
        match op {
            31 => {
                // dcl
                let mut tok = 0x8000_0000u32;
                if let Some(m) = mods.first() {
                    let tex = match *m {
                        "2d" => Some(2),
                        "cube" => Some(3),
                        "volume" => Some(4),
                        _ => None,
                    };
                    match tex {
                        Some(t) => tok |= t << 27,
                        None => {
                            let digits = m.trim_start_matches(|c: char| c.is_ascii_alphabetic());
                            let name = &m[..m.len() - digits.len()];
                            let u = USAGES.iter().position(|u| *u == name).ok_or(format!("dcl usage {m}"))? as u32;
                            tok |= u | digits.parse::<u32>().unwrap_or(0) << 16;
                        }
                    }
                }
                body.push(tok);
                let resmod = if mods.contains(&"centroid") { 4 } else if mods.contains(&"pp") { 2 } else { 0 };
                dest(operands.first().ok_or("dcl without a register")?, v, resmod, 0, &mut body)?;
            }
            81 | 47 | 48 => {
                // def c, f, f, f, f / defb b, bool / defi i, n, n, n, n
                dest(operands.first().ok_or("def without a register")?, v, 0, 0, &mut body)?;
                for o in &operands[1..] {
                    body.push(if op == 48 { o.trim().parse::<i32>().map_err(|_| format!("bad integer {o}"))? as u32 } else { number(o)? });
                }
            }
            _ => {
                let mut resmod = 0;
                let mut shift = 0;
                for m in &mods {
                    match *m {
                        "sat" => resmod |= 1,
                        "pp" => resmod |= 2,
                        "centroid" => resmod |= 4,
                        "x2" => shift = 1,
                        "x4" => shift = 2,
                        "x8" => shift = 3,
                        "d2" => shift = 0xf,
                        "d4" => shift = 0xe,
                        "d8" => shift = 0xd,
                        "gt" => ctl = 1,
                        "eq" => ctl = 2,
                        "ge" => ctl = 3,
                        "lt" => ctl = 4,
                        "ne" => ctl = 5,
                        "le" => ctl = 6,
                        x => return Err(format!("instruction modifier _{x}")),
                    }
                }
                if base == "texldp" {
                    ctl = 1;
                }
                if base == "texldb" {
                    ctl = 2;
                }
                let mut ops = operands.iter();
                if !no_dest(op) {
                    if let Some(d) = ops.next() {
                        dest(d, v, resmod, shift, &mut body)?;
                    }
                }
                for s in ops {
                    source(s, v, &mut body)?;
                }
            }
        }
        // (Microsoft's spelling if_gt / break_gt: the comparing forms, as ifc_gt / breakc_gt)
        let op = match op {
            40 if ctl != 0 => 41,
            44 if ctl != 0 => 45,
            op => op,
        };
        let len = if v.major >= 2 { (body.len() as u32) << 24 } else { 0 };
        words.push(op | ctl << 16 | len | if coissue { 1 << 30 } else { 0 });
        words.extend(body);
    }
    if ver.is_none() {
        return Err("empty shader".into());
    }
    words.push(0x0000_ffff);
    Ok(words)
}

/// Operands separated by commas outside brackets.
fn split_args(s: &str) -> Vec<String> {
    let mut v = Vec::new();
    let (mut cur, mut depth) = (String::new(), 0);
    for ch in s.chars() {
        match ch {
            '[' => depth += 1,
            ']' => depth -= 1,
            _ => {}
        }
        if ch == ',' && depth == 0 {
            v.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(ch);
        }
    }
    if !cur.trim().is_empty() {
        v.push(cur.trim().to_string());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::assemble;

    #[test]
    fn vs11() {
        let w = assemble("vs.1.1\ndcl_position v0\ndcl_color v1\nm4x4 oPos, v0, c0\nmov oD0, v1\n").unwrap();
        assert_eq!(w[0], 0xfffe0101);
        // dcl_position v0: dcl, usage token, register token
        assert_eq!(&w[1..4], &[31, 0x8000_0000, 0x900f_0000]);
        assert_eq!(&w[7..11], &[20, 0xc00f_0000, 0x90e4_0000, 0xa0e4_0000]);
        assert_eq!(*w.last().unwrap(), 0xffff);
    }

    #[test]
    fn ps20() {
        let w = assemble("ps_2_0\ndcl t0.xy\ndcl_2d s0\ntexld r0, t0, s0\nmul_sat r0, r0, -c0.x\nmov oC0, r0\n").unwrap();
        assert_eq!(w[0], 0xffff0200);
        assert_eq!(&w[1..4], &[0x0200_001f, 0x8000_0000, 0xb003_0000]);
        assert_eq!(&w[4..7], &[0x0200_001f, 0x9000_0000, 0xa00f_0800]);
        assert_eq!(&w[7..11], &[0x0300_0042, 0x800f_0000, 0xb0e4_0000, 0xa0e4_0800]);
        assert_eq!(&w[11..15], &[0x0300_0005, 0x801f_0000, 0x80e4_0000, 0xa100_0000]);
    }
}
