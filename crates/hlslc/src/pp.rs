//! HLSL tokens and the preprocessor (#define with arguments, #if/#ifdef/#elif, #include through
//! the caller, #pragma pack_matrix).

use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    /// value, unsigned suffix
    Int(u64, bool),
    Float(f64),
    Str(String),
    Punct(&'static str),
    /// #pragma pack_matrix: true row_major
    PackMatrix(bool),
}

#[derive(Clone, Debug)]
pub struct Token {
    pub t: Tok,
    pub line: u32,
}

const PUNCTS: &[&str] = &[
    "<<=", ">>=", "...", "&&", "||", "==", "!=", "<=", ">=", "<<", ">>", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=",
    "|=", "^=", "::", "##", "+", "-", "*", "/", "%", "&", "|", "^", "~", "!", "<", ">", "=", "?", ":", ";", ",", ".", "(",
    ")", "[", "]", "{", "}", "#", "@", "\\", "$", "`", "'",
];

/// Tokens of one (logical) line.
fn lex_line(s: &str, line: u32, out: &mut Vec<Token>) -> Result<(), String> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let st = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            out.push(Token { t: Tok::Ident(s[st..i].to_string()), line });
            continue;
        }
        if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            let st = i;
            if c == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
                i += 2;
                while i < b.len() && b[i].is_ascii_hexdigit() {
                    i += 1;
                }
                let v = u64::from_str_radix(&s[st + 2..i], 16).map_err(|_| format!("line {line}: bad number"))?;
                let mut u = false;
                while i < b.len() && matches!(b[i], b'u' | b'U' | b'l' | b'L') {
                    u |= matches!(b[i], b'u' | b'U');
                    i += 1;
                }
                out.push(Token { t: Tok::Int(v, u), line });
                continue;
            }
            let mut float = false;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && b[i] == b'.' {
                float = true;
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            if i < b.len() && matches!(b[i], b'e' | b'E') {
                let mut j = i + 1;
                if j < b.len() && matches!(b[j], b'+' | b'-') {
                    j += 1;
                }
                if j < b.len() && b[j].is_ascii_digit() {
                    float = true;
                    i = j;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                }
            }
            let text = &s[st..i];
            let mut u = false;
            while i < b.len() && matches!(b[i], b'f' | b'F' | b'h' | b'H' | b'l' | b'L' | b'u' | b'U') {
                if matches!(b[i], b'f' | b'F' | b'h' | b'H') {
                    float = true;
                }
                u |= matches!(b[i], b'u' | b'U');
                i += 1;
            }
            if float {
                let v: f64 = text.parse().map_err(|_| format!("line {line}: bad number {text}"))?;
                out.push(Token { t: Tok::Float(v), line });
            } else {
                // (a leading 0: octal)
                let v = if text.len() > 1 && text.starts_with('0') {
                    u64::from_str_radix(&text[1..], 8).unwrap_or(0)
                } else {
                    text.parse().map_err(|_| format!("line {line}: bad number {text}"))?
                };
                out.push(Token { t: Tok::Int(v, u), line });
            }
            continue;
        }
        if c == b'"' {
            let st = i + 1;
            i += 1;
            while i < b.len() && b[i] != b'"' {
                if b[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            out.push(Token { t: Tok::Str(s[st..i.min(b.len())].to_string()), line });
            i += 1;
            continue;
        }
        let rest = &s[i..];
        match PUNCTS.iter().find(|p| rest.starts_with(**p)) {
            Some(p) => {
                out.push(Token { t: Tok::Punct(p), line });
                i += p.len();
            }
            None => {
                // (any other character: skipped, as a stray byte of a non-ASCII comment)
                i += rest.chars().next().map_or(1, |c| c.len_utf8());
            }
        }
    }
    Ok(())
}

/// Source without comments, continuation lines joined (line numbers kept: a joined or
/// commented-out line leaves an empty line).
fn clean(src: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut in_block = false;
    let mut pending_empty = 0;
    for raw in src.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        let b = raw.as_bytes();
        let mut i = 0;
        let mut in_str = false;
        while i < b.len() {
            if in_block {
                if raw[i..].starts_with("*/") {
                    in_block = false;
                    cur.push(' ');
                    i += 2;
                } else {
                    i += 1;
                }
                continue;
            }
            if in_str {
                if b[i] == b'\\' && i + 1 < b.len() {
                    cur.push_str(&raw[i..i + 2]);
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    in_str = false;
                }
            } else if raw[i..].starts_with("//") {
                break;
            } else if raw[i..].starts_with("/*") {
                in_block = true;
                i += 2;
                continue;
            } else if b[i] == b'"' {
                in_str = true;
            }
            let ch = raw[i..].chars().next().unwrap();
            cur.push(ch);
            i += ch.len_utf8();
        }
        if cur.ends_with('\\') && !in_block {
            cur.pop();
            pending_empty += 1;
            continue;
        }
        if in_block {
            // (a comment going on: the line so far stays open)
            pending_empty += 1;
            continue;
        }
        lines.push(std::mem::take(&mut cur));
        for _ in 0..pending_empty {
            lines.push(String::new());
        }
        pending_empty = 0;
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[derive(Clone)]
struct Macro {
    params: Option<Vec<String>>,
    body: Vec<Token>,
}

pub struct Pp<'a> {
    macros: HashMap<String, Macro>,
    include: &'a mut dyn FnMut(&str, bool) -> Option<String>,
    out: Vec<Token>,
    depth: u32,
    row_major: bool,
}

/// Preprocess `src` with `defines` (name, value); `include(name, system)` gives an included
/// file's text.
pub fn preprocess(src: &str, defines: &[(String, String)], include: &mut dyn FnMut(&str, bool) -> Option<String>) -> Result<Vec<Token>, String> {
    let mut pp = Pp { macros: HashMap::new(), include, out: Vec::new(), depth: 0, row_major: false };
    for (n, v) in defines {
        let mut body = Vec::new();
        lex_line(v, 0, &mut body)?;
        pp.macros.insert(n.clone(), Macro { params: None, body });
    }
    pp.macros.insert("__HLSL_VERSION".into(), Macro { params: None, body: vec![Token { t: Tok::Int(2016, false), line: 0 }] });
    pp.file(src)?;
    let _ = pp.row_major;
    Ok(pp.out)
}

impl Pp<'_> {
    fn file(&mut self, src: &str) -> Result<(), String> {
        self.depth += 1;
        if self.depth > 32 {
            return Err("#include nested too deep".into());
        }
        let lines = clean(src);
        // conditional stack: (this branch is on, some branch was taken, the enclosing is on)
        let mut conds: Vec<(bool, bool, bool)> = Vec::new();
        let on = |c: &Vec<(bool, bool, bool)>| c.last().is_none_or(|x| x.0 && x.2);
        let mut pending: Vec<Token> = Vec::new();
        for (n, l) in lines.iter().enumerate() {
            let line = n as u32 + 1;
            let t = l.trim_start();
            if let Some(d) = t.strip_prefix('#') {
                let d = d.trim_start();
                let (name, rest) = d.split_at(d.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(d.len()));
                let rest = rest.trim();
                match name {
                    "if" | "ifdef" | "ifndef" => {
                        let outer = on(&conds);
                        let v = if !outer {
                            false
                        } else if name == "if" {
                            self.eval_if(rest, line)? != 0
                        } else {
                            let id = rest.split_whitespace().next().unwrap_or("");
                            self.macros.contains_key(id) == (name == "ifdef")
                        };
                        conds.push((v, v, outer));
                    }
                    "elif" => {
                        let Some(&(_, taken, outer)) = conds.last() else { return Err(format!("line {line}: #elif without #if")) };
                        let v = !taken && outer && self.eval_if(rest, line)? != 0;
                        *conds.last_mut().unwrap() = (v, taken || v, outer);
                    }
                    "else" => {
                        let Some(c) = conds.last_mut() else { return Err(format!("line {line}: #else without #if")) };
                        *c = (!c.1, true, c.2);
                    }
                    "endif" => {
                        conds.pop().ok_or(format!("line {line}: #endif without #if"))?;
                    }
                    _ if !on(&conds) => {}
                    "define" => {
                        self.flush(&mut pending)?;
                        self.define(rest, line)?;
                    }
                    "undef" => {
                        self.flush(&mut pending)?;
                        self.macros.remove(rest.split_whitespace().next().unwrap_or(""));
                    }
                    "include" => {
                        self.flush(&mut pending)?;
                        let (fname, sys) = if let Some(r) = rest.strip_prefix('"') {
                            (r.split('"').next().unwrap_or(""), false)
                        } else if let Some(r) = rest.strip_prefix('<') {
                            (r.split('>').next().unwrap_or(""), true)
                        } else {
                            return Err(format!("line {line}: bad #include"));
                        };
                        let text = (self.include)(fname, sys).ok_or(format!("line {line}: cannot open include file {fname}"))?;
                        self.file(&text)?;
                    }
                    "pragma" => {
                        let r = rest.replace(' ', "");
                        if let Some(p) = r.strip_prefix("pack_matrix(") {
                            self.flush(&mut pending)?;
                            let row = p.starts_with("row_major");
                            self.row_major = row;
                            self.out.push(Token { t: Tok::PackMatrix(row), line });
                        }
                    }
                    "error" => return Err(format!("line {line}: #error {rest}")),
                    "line" | "" | "warning" => {}
                    _ => return Err(format!("line {line}: unknown directive #{name}")),
                }
                continue;
            }
            if on(&conds) {
                lex_line(l, line, &mut pending)?;
            }
        }
        if !conds.is_empty() {
            return Err("missing #endif".into());
        }
        self.flush(&mut pending)?;
        self.depth -= 1;
        Ok(())
    }

    fn flush(&mut self, pending: &mut Vec<Token>) -> Result<(), String> {
        let toks = std::mem::take(pending);
        let mut v = self.expand(&toks, &mut Vec::new(), 0)?;
        self.out.append(&mut v);
        Ok(())
    }

    fn define(&mut self, rest: &str, line: u32) -> Result<(), String> {
        let end = rest.find(|c: char| !c.is_ascii_alphanumeric() && c != '_').unwrap_or(rest.len());
        let name = &rest[..end];
        if name.is_empty() {
            return Err(format!("line {line}: bad #define"));
        }
        let after = &rest[end..];
        let (params, body) = if let Some(p) = after.strip_prefix('(') {
            let close = p.find(')').ok_or(format!("line {line}: bad macro parameters"))?;
            let ps: Vec<String> = p[..close].split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            (Some(ps), &p[close + 1..])
        } else {
            (None, after)
        };
        let mut toks = Vec::new();
        lex_line(body, line, &mut toks)?;
        self.macros.insert(name.to_string(), Macro { params, body: toks });
        Ok(())
    }

    fn expand(&self, toks: &[Token], disabled: &mut Vec<String>, depth: u32) -> Result<Vec<Token>, String> {
        if depth > 64 {
            return Err("macro expansion too deep".into());
        }
        let mut out = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            let tk = &toks[i];
            let Tok::Ident(name) = &tk.t else {
                out.push(tk.clone());
                i += 1;
                continue;
            };
            let Some(m) = self.macros.get(name).filter(|_| !disabled.contains(name)) else {
                out.push(tk.clone());
                i += 1;
                continue;
            };
            match &m.params {
                None => {
                    disabled.push(name.clone());
                    let body: Vec<Token> = m.body.iter().map(|t| Token { t: t.t.clone(), line: tk.line }).collect();
                    let mut e = self.expand(&paste(body), disabled, depth + 1)?;
                    disabled.pop();
                    out.append(&mut e);
                    i += 1;
                }
                Some(params) => {
                    if toks.get(i + 1).map(|t| &t.t) != Some(&Tok::Punct("(")) {
                        out.push(tk.clone());
                        i += 1;
                        continue;
                    }
                    // the arguments
                    let mut args: Vec<Vec<Token>> = vec![Vec::new()];
                    let mut level = 0;
                    let mut j = i + 2;
                    loop {
                        let Some(t) = toks.get(j) else { return Err(format!("line {}: unterminated call of macro {name}", tk.line)) };
                        match t.t {
                            Tok::Punct("(") => level += 1,
                            Tok::Punct(")") if level == 0 => break,
                            Tok::Punct(")") => level -= 1,
                            Tok::Punct(",") if level == 0 => {
                                args.push(Vec::new());
                                j += 1;
                                continue;
                            }
                            _ => {}
                        }
                        args.last_mut().unwrap().push(t.clone());
                        j += 1;
                    }
                    if params.is_empty() && args.len() == 1 && args[0].is_empty() {
                        args.clear();
                    }
                    let mut expanded = Vec::new();
                    for a in &args {
                        expanded.push(self.expand(a, disabled, depth + 1)?);
                    }
                    let mut body = Vec::new();
                    let mb = &m.body;
                    let mut k = 0;
                    while k < mb.len() {
                        let t = &mb[k];
                        // #param
                        if t.t == Tok::Punct("#") {
                            if let Some(Tok::Ident(p)) = mb.get(k + 1).map(|t| &t.t) {
                                if let Some(pi) = params.iter().position(|x| x == p) {
                                    let s = args.get(pi).map_or(String::new(), |a| a.iter().map(text).collect::<Vec<_>>().join(" "));
                                    body.push(Token { t: Tok::Str(s), line: tk.line });
                                    k += 2;
                                    continue;
                                }
                            }
                        }
                        if let Tok::Ident(p) = &t.t {
                            if let Some(pi) = params.iter().position(|x| x == p) {
                                // (next to ##: the argument as written)
                                let raw = mb.get(k + 1).is_some_and(|t| t.t == Tok::Punct("##"))
                                    || (k > 0 && mb[k - 1].t == Tok::Punct("##"));
                                let a = if raw { args.get(pi) } else { expanded.get(pi) };
                                if let Some(a) = a {
                                    body.extend(a.iter().map(|t| Token { t: t.t.clone(), line: tk.line }));
                                }
                                k += 1;
                                continue;
                            }
                        }
                        body.push(Token { t: t.t.clone(), line: tk.line });
                        k += 1;
                    }
                    disabled.push(name.clone());
                    let mut e = self.expand(&paste(body), disabled, depth + 1)?;
                    disabled.pop();
                    out.append(&mut e);
                    i = j + 1;
                }
            }
        }
        Ok(out)
    }

    fn eval_if(&self, expr: &str, line: u32) -> Result<i64, String> {
        let mut toks = Vec::new();
        lex_line(expr, line, &mut toks)?;
        // defined X / defined(X) before expansion
        let mut t2 = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if toks[i].t == Tok::Ident("defined".into()) {
                let (id, n) = match (toks.get(i + 1).map(|t| &t.t), toks.get(i + 2).map(|t| &t.t)) {
                    (Some(Tok::Punct("(")), Some(Tok::Ident(id))) => (id.clone(), 4),
                    (Some(Tok::Ident(id)), _) => (id.clone(), 2),
                    _ => return Err(format!("line {line}: bad defined()")),
                };
                t2.push(Token { t: Tok::Int(self.macros.contains_key(&id) as u64, false), line });
                i += n;
                continue;
            }
            t2.push(toks[i].clone());
            i += 1;
        }
        let e = self.expand(&t2, &mut Vec::new(), 0)?;
        let mut p = 0;
        let v = cexpr(&e, &mut p, 0)?;
        Ok(v)
    }
}

pub fn text(t: &Token) -> String {
    match &t.t {
        Tok::Ident(s) => s.clone(),
        Tok::Int(v, u) => format!("{v}{}", if *u { "u" } else { "" }),
        Tok::Float(f) => format!("{f:?}"),
        Tok::Str(s) => format!("\"{s}\""),
        Tok::Punct(p) => p.to_string(),
        Tok::PackMatrix(_) => String::new(),
    }
}

/// a ## b
fn paste(toks: Vec<Token>) -> Vec<Token> {
    if !toks.iter().any(|t| t.t == Tok::Punct("##")) {
        return toks;
    }
    let mut out: Vec<Token> = Vec::new();
    let mut i = 0;
    while i < toks.len() {
        if toks[i].t == Tok::Punct("##") && !out.is_empty() && i + 1 < toks.len() {
            let a = out.pop().unwrap();
            let s = text(&a) + &text(&toks[i + 1]);
            let mut v = Vec::new();
            let _ = lex_line(&s, a.line, &mut v);
            out.extend(v);
            i += 2;
            continue;
        }
        out.push(toks[i].clone());
        i += 1;
    }
    out
}

/// Integer expressions of #if (precedence climbing).
fn cexpr(t: &[Token], p: &mut usize, min: u32) -> Result<i64, String> {
    let mut l = cunary(t, p)?;
    loop {
        let Some(Tok::Punct(op)) = t.get(*p).map(|x| &x.t) else { break };
        let prec = match *op {
            "?" => 1,
            "||" => 2,
            "&&" => 3,
            "|" => 4,
            "^" => 5,
            "&" => 6,
            "==" | "!=" => 7,
            "<" | ">" | "<=" | ">=" => 8,
            "<<" | ">>" => 9,
            "+" | "-" => 10,
            "*" | "/" | "%" => 11,
            _ => break,
        };
        if prec < min {
            break;
        }
        *p += 1;
        if *op == "?" {
            let a = cexpr(t, p, 0)?;
            if t.get(*p).map(|x| &x.t) != Some(&Tok::Punct(":")) {
                return Err("bad ?: in #if".into());
            }
            *p += 1;
            let b = cexpr(t, p, 1)?;
            l = if l != 0 { a } else { b };
            continue;
        }
        let r = cexpr(t, p, prec + 1)?;
        l = match *op {
            "||" => (l != 0 || r != 0) as i64,
            "&&" => (l != 0 && r != 0) as i64,
            "|" => l | r,
            "^" => l ^ r,
            "&" => l & r,
            "==" => (l == r) as i64,
            "!=" => (l != r) as i64,
            "<" => (l < r) as i64,
            ">" => (l > r) as i64,
            "<=" => (l <= r) as i64,
            ">=" => (l >= r) as i64,
            "<<" => l.wrapping_shl(r as u32),
            ">>" => l.wrapping_shr(r as u32),
            "+" => l.wrapping_add(r),
            "-" => l.wrapping_sub(r),
            "*" => l.wrapping_mul(r),
            "/" => {
                if r == 0 {
                    0
                } else {
                    l / r
                }
            }
            _ => {
                if r == 0 {
                    0
                } else {
                    l % r
                }
            }
        };
    }
    Ok(l)
}

fn cunary(t: &[Token], p: &mut usize) -> Result<i64, String> {
    let Some(tk) = t.get(*p) else { return Err("bad #if expression".into()) };
    *p += 1;
    Ok(match &tk.t {
        Tok::Int(v, _) => *v as i64,
        Tok::Float(f) => *f as i64,
        Tok::Ident(_) => 0,
        Tok::Punct("(") => {
            let v = cexpr(t, p, 0)?;
            *p += 1;
            v
        }
        Tok::Punct("!") => (cunary(t, p)? == 0) as i64,
        Tok::Punct("-") => -cunary(t, p)?,
        Tok::Punct("+") => cunary(t, p)?,
        Tok::Punct("~") => !cunary(t, p)?,
        _ => return Err("bad #if expression".into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pp(s: &str) -> String {
        let mut inc = |_: &str, _: bool| None;
        preprocess(s, &[("EXT".into(), "3".into())], &mut inc).unwrap().iter().map(text).collect::<Vec<_>>().join(" ")
    }
    #[test]
    fn macros() {
        assert_eq!(pp("#define A(x, y) ((x) * y)\nA(1 + 2, B)"), "( ( 1 + 2 ) * B )");
        assert_eq!(pp("#if defined(EXT) && EXT > 2\nyes\n#else\nno\n#endif"), "yes");
        assert_eq!(pp("#ifdef NOPE\nno\n#elif 1\nmaybe\n#endif"), "maybe");
        assert_eq!(pp("#define C(a) a##1\nC(x) /* c */ y // d"), "x1 y");
        assert_eq!(pp("#define L \\\n  5\nL"), "5");
        assert_eq!(pp("float f = 1.5f; int i = 0x10u;"), "float f = 1.5 ; int i = 16u ;");
    }
}
