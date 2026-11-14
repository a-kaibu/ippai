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
        // (11 bits in the token)
        p.parse::<u32>().ok().filter(|&n| n < 0x800).ok_or(format!("bad register {s}"))
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
    if lower.is_empty() || !lower.is_char_boundary(1) {
        return Err(format!("unknown register {s}"));
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
    // (a register letter before the '[' and a ']' after it; anything else is not an index)
    let index = s.find('[').filter(|&o| o > 0 && s.is_char_boundary(1)).and_then(|o| s.rfind(']').filter(|&c| c > o).map(|c| (o, c)));
    if let Some((open, close)) = index {
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
        // (the length field has 4 bits: more tokens would spill into the co-issue and
        // predicate bits)
        if body.len() > 15 {
            return Err(format!("{mnem}: {} operand tokens in one instruction", body.len()));
        }
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

    /// D3DSHADER_INSTRUCTION_OPCODE_TYPE (d3d9types.h).
    #[test]
    fn opcodes() {
        use super::op_code;
        let names = [
            "nop", "mov", "add", "sub", "mad", "mul", "rcp", "rsq", "dp3", "dp4", "min", "max", "slt", "sge", "exp", "log", "lit", "dst", "lrp", "frc", "m4x4", "m4x3", "m3x4", "m3x3", "m3x2", "call",
            "callnz", "loop", "ret", "endloop", "label", "dcl", "pow", "crs", "sgn", "abs", "nrm", "sincos", "rep", "endrep", "if", "ifc", "else", "endif", "break", "breakc", "mova", "defb", "defi",
        ];
        for (i, n) in names.iter().enumerate() {
            assert_eq!(op_code(n), Some(i as u32), "{n}");
        }
        let tex = [
            ("texcoord", 64), ("texcrd", 64), ("texkill", 65), ("tex", 66), ("texld", 66), ("texldp", 66), ("texldb", 66), ("texbem", 67), ("texbeml", 68), ("texreg2ar", 69), ("texreg2gb", 70),
            ("texm3x2pad", 71), ("texm3x2tex", 72), ("texm3x3pad", 73), ("texm3x3tex", 74), ("texm3x3spec", 76), ("texm3x3vspec", 77), ("expp", 78), ("logp", 79), ("cnd", 80), ("def", 81),
            ("texreg2rgb", 82), ("texdp3tex", 83), ("texm3x2depth", 84), ("texdp3", 85), ("texm3x3", 86), ("texdepth", 87), ("cmp", 88), ("bem", 89), ("dp2add", 90), ("dsx", 91), ("dsy", 92),
            ("texldd", 93), ("setp", 94), ("texldl", 95), ("breakp", 96), ("phase", 0xfffd),
        ];
        for (n, c) in tex {
            assert_eq!(op_code(n), Some(c), "{n}");
        }
        assert_eq!(op_code("movx"), None);
    }

    #[test]
    fn registers_and_selectors() {
        use super::{register, swizzle, Ver};
        let ps = Ver { pixel: true, major: 2, minor: 0 };
        let vs = Ver { pixel: false, major: 2, minor: 0 };
        assert_eq!(register("t3", &ps), Ok((3, 3)), "ps: texture coordinate input");
        assert_eq!(register("t3", &vs), Ok((6, 3)), "vs: texture coordinate output");
        assert_eq!(register("b2", &vs), Ok((14, 2)));
        assert_eq!(register("i1", &vs), Ok((7, 1)));
        assert_eq!(register("p0", &ps), Ok((19, 0)));
        assert_eq!(register("r2047", &vs), Ok((0, 2047)));
        assert!(register("r2048", &vs).is_err());
        assert!(register("", &vs).is_err() && register("é3", &vs).is_err() && register("q1", &vs).is_err());
        assert_eq!(swizzle("xyzw"), Ok(0xe4));
        assert_eq!(swizzle("x"), Ok(0));
        assert_eq!(swizzle("wz"), Ok(0b10_10_10_11));
        assert!(swizzle("xyzwx").is_err() && swizzle("").is_err() && swizzle("q").is_err());
    }

    /// Token words by the D3D9 bytecode format (instruction, destination, source and relative
    /// address tokens), written out by hand.
    #[test]
    fn tokens() {
        let body = |src: &str| assemble(src).unwrap_or_else(|e| panic!("{e}: {src}"))[1..].to_vec();
        let end = 0x0000_ffff;
        // relative addressing: the index folded into the register number, an address token
        assert_eq!(body("vs_3_0\nmov r1, c[a0.x + 3]"), [0x0300_0001, 0x800f_0001, 0xa0e4_2003, 0xb000_0000, end]);
        assert_eq!(body("vs_3_0\nmov r0.y, c4[a0.w + 2]"), [0x0300_0001, 0x8002_0000, 0xa0e4_2006, 0xb0ff_0000, end]);
        assert_eq!(body("vs_3_0\nmov r0, c[a0.x + 1].y"), [0x0300_0001, 0x800f_0000, 0xa055_2001, 0xb000_0000, end]);
        assert_eq!(body("vs_3_0\nmov o2[aL].xy, r0"), [0x0300_0001, 0xe003_2002, 0xf000_0800, 0x80e4_0000, end]);
        assert_eq!(body("vs_3_0\nmov o2[a0.y], r0"), [0x0300_0001, 0xe00f_2002, 0xb055_0000, 0x80e4_0000, end]);
        // vs_1_1: no address token, no length
        assert_eq!(body("vs_1_1\nmov r0, c[a0.x + 2]"), [0x0000_0001, 0x800f_0000, 0xa0e4_2002, end]);
        // source modifiers
        assert_eq!(body("ps_2_0\nadd r0, -r1, r2_abs"), [0x0300_0002, 0x800f_0000, 0x81e4_0001, 0x8be4_0002, end]);
        assert_eq!(body("ps_2_0\nmad r0, -r3_abs, 1-r1, -r2_bx2"), [0x0400_0004, 0x800f_0000, 0x8ce4_0003, 0x86e4_0001, 0x85e4_0002, end]);
        assert_eq!(body("ps_1_4\nmad r0, r1_bias, r2_x2, -r3_x2"), [0x0000_0004, 0x800f_0000, 0x82e4_0001, 0x87e4_0002, 0x88e4_0003, end]);
        assert_eq!(body("ps_1_4\nmad r0, -r1_bias, r2_dz, r3_dw"), [0x0000_0004, 0x800f_0000, 0x83e4_0001, 0x89e4_0002, 0x8ae4_0003, end]);
        assert_eq!(body("ps_3_0\nif !b0\nendif"), [0x0100_0028, 0x8de4_0800 | 0x6000_0000, 0x0000_002b, end]);
        // result modifiers and shifts
        assert_eq!(body("ps_2_0\nmul_sat_x2 r0, r1, r2")[1], 0x811f_0000);
        assert_eq!(body("ps_2_0\nmul_sat_pp r0, r1, r2")[1], 0x803f_0000);
        assert_eq!(body("ps_2_0\nmul_centroid_d2 r0, r1, r2")[1], 0x8f4f_0000);
        assert_eq!(body("ps_2_0\nmul_x4 r0, r1, r2")[1], 0x820f_0000);
        assert_eq!(body("ps_2_0\nmul_x8 r0, r1, r2")[1], 0x830f_0000);
        assert_eq!(body("ps_2_0\nmul_d4 r0, r1, r2")[1], 0x8e0f_0000);
        assert_eq!(body("ps_2_0\nmul_d8 r0, r1, r2")[1], 0x8d0f_0000);
        // declarations
        assert_eq!(body("vs_3_0\ndcl_texcoord3 v2"), [0x0200_001f, 0x8003_0005, 0x900f_0002, end]);
        assert_eq!(body("ps_3_0\ndcl_volume s1"), [0x0200_001f, 0xa000_0000, 0xa00f_0801, end]);
        assert_eq!(body("ps_3_0\ndcl_cube s2"), [0x0200_001f, 0x9800_0000, 0xa00f_0802, end]);
        assert_eq!(body("ps_3_0\ndcl_texcoord0_centroid v0.xy"), [0x0200_001f, 0x8000_0005, 0x9043_0000, end]);
        assert_eq!(body("ps_3_0\ndcl_color1_pp v1"), [0x0200_001f, 0x8001_000a, 0x902f_0001, end]);
        assert!(assemble("ps_3_0\ndcl_bogus v0").is_err());
        // constants
        assert_eq!(body("vs_3_0\ndefb b0, true\ndefb b1, false"), [0x0200_002f, 0xe00f_0800, 1, 0x0200_002f, 0xe00f_0801, 0, end]);
        assert_eq!(body("vs_3_0\ndefi i1, 1, -2, 3, 4"), [0x0500_0030, 0xf00f_0001, 1, (-2i32) as u32, 3, 4, end]);
        assert_eq!(body("ps_2_0\ndef c0, 1.0, 0.5f, -2, 0"), [0x0500_0051, 0xa00f_0000, 0x3f80_0000, 0x3f00_0000, 0xc000_0000, 0, end]);
        // comparisons, predicates, co-issue, projected and biased texld
        assert_eq!(body("ps_3_0\nif_gt r0.x, r1.x\nendif"), [0x0201_0029, 0x8000_0000, 0x8000_0001, 0x0000_002b, end]);
        assert_eq!(body("ps_3_0\nbreak_le r0.x, r1.y"), [0x0206_002d, 0x8000_0000, 0x8055_0001, end]);
        assert_eq!(body("ps_3_0\nsetp_ne p0, r0, r1")[..2], [0x0305_005e, 0xb00f_1000]);
        assert_eq!(body("ps_1_1\n+add r0.a, r1, r2")[0], 0x4000_0002);
        assert_eq!(body("ps_2_0\ntexldp r0, t0, s0")[0], 0x0301_0042);
        assert_eq!(body("ps_2_0\ntexldb r0, t0, s0")[0], 0x0302_0042);
        assert_eq!(body("ps_2_0\nmov r0, r1 ; comment\n// line\n\nmov r1, r0").len(), 7);
    }

    #[test]
    fn versions_and_operands() {
        assert_eq!(assemble("vs_2_x\n").unwrap()[0], 0xfffe_0200);
        assert_eq!(assemble("vs_2_sw\n").unwrap()[0], 0xfffe_0200);
        assert_eq!(assemble("ps.1.4\n").unwrap()[0], 0xffff_0104);
        for bad in ["xs_1_1\n", "vs_1\n", "vs_a_1\n", "vs_1_q\n", "", "mov r0, r1\n", "ps_2_0\nfoo r0\n", "ps_2_0\nmov_qq r0, r1\n", "ps_2_0\nmov r0, r1_zz\n"] {
            assert!(assemble(bad).is_err(), "{bad:?}");
        }
        assert_eq!(super::split_args("c[a0.x + 1, 2], r1 , t2"), ["c[a0.x + 1, 2]", "r1", "t2"]);
        assert_eq!(super::split_args("[[a,b],c], d"), ["[[a,b],c]", "d"]);
        assert_eq!(super::split_operand("c3[a0.x + 2].y"), ("c5".to_string(), Some("a0.x".to_string()), "y".to_string()));
        assert_eq!(super::split_operand("c[7]"), ("c7".to_string(), None, String::new()));
        assert_eq!(super::split_operand("[3]"), ("[3]".to_string(), None, String::new()));
    }
}
