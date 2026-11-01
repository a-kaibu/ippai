//! Wine .spec files: how each export is called from x86 code.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conv {
    Stdcall,
    Cdecl,
    Varargs,
    Thiscall,
    Fastcall,
}

#[derive(Debug)]
pub struct Entry {
    /// exported name ("" for an ordinal-only export)
    pub name: String,
    pub ordinal: Option<u32>,
    pub conv: Conv,
    /// stack bytes of each argument (4, 8 or 16)
    pub args: Vec<u32>,
    /// the C symbol implementing it
    pub symbol: String,
}

#[derive(Debug)]
pub struct Data {
    pub name: String,
    pub ordinal: Option<u32>,
    pub symbol: String,
}

#[derive(Default)]
pub struct Spec {
    pub funcs: Vec<Entry>,
    pub data: Vec<Data>,
    /// exported name -> "dll.symbol" it forwards to
    pub forwards: Vec<(String, String)>,
}

/// Parses the entries that matter on i386 (functions implemented in this DLL and data exports).
pub fn parse(text: &str) -> Spec {
    let mut spec = Spec::default();
    // join continuation lines, drop comments
    let mut joined = String::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        if let Some(l) = line.strip_suffix('\\') {
            joined.push_str(l);
            joined.push(' ');
        } else {
            joined.push_str(line);
            joined.push('\n');
        }
    }
    for line in joined.lines() {
        let mut toks = tokenize(line);
        if toks.len() < 2 {
            continue;
        }
        let ordinal = toks[0].parse::<u32>().ok();
        if ordinal.is_none() && toks[0] != "@" {
            continue;
        }
        let kind = toks[1].clone();
        toks.drain(..2);
        let mut skip = false;
        let mut noname = false;
        while let Some(f) = toks.first().filter(|t| t.starts_with('-')).cloned() {
            toks.remove(0);
            if let Some(archs) = f.strip_prefix("-arch=") {
                // "win32", "i386", "!i386", "win64", ...
                let list: Vec<&str> = archs.split(',').collect();
                let neg = list.iter().all(|a| a.starts_with('!'));
                let has = list.iter().any(|a| matches!(a.trim_start_matches('!'), "i386" | "win32" | "x86"));
                skip |= if neg { has } else { !has };
            }
            if f == "-noname" {
                noname = true;
            }
            if f == "-import" {
                skip = true;
            }
        }
        if skip || toks.is_empty() {
            continue;
        }
        let conv = match kind.as_str() {
            "stdcall" | "pascal" => Conv::Stdcall,
            "cdecl" => Conv::Cdecl,
            "varargs" => Conv::Varargs,
            "thiscall" => Conv::Thiscall,
            "fastcall" => Conv::Fastcall,
            "extern" => {
                let name = toks[0].clone();
                let symbol = toks.get(1).cloned().unwrap_or_else(|| name.clone());
                if !symbol.contains('.') {
                    spec.data.push(Data { name: if noname { String::new() } else { name }, ordinal, symbol });
                }
                continue;
            }
            _ => continue,
        };
        // name(args) [symbol]
        let head = &toks[0];
        let Some(open) = head.find('(') else { continue };
        let name = head[..open].to_string();
        let args_text = head[open + 1..].trim_end_matches(')');
        let args = args_text
            .split_whitespace()
            .map(|a| match a {
                "int64" | "double" => 8,
                "int128" => 16,
                _ => 4,
            })
            .collect();
        let symbol = toks.get(1).cloned().unwrap_or_else(|| name.clone());
        if symbol.contains('.') {
            spec.forwards.push((name, symbol));
            continue;
        }
        spec.funcs.push(Entry { name: if noname { String::new() } else { name }, ordinal, conv, args, symbol });
    }
    spec
}

/// Splits on whitespace, keeping "name(arg arg)" together.
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0;
    for c in line.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            c if c.is_whitespace() && depth == 0 => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}
