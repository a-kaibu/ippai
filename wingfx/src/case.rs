//! The two line formats (SPEC.md): case files (input) and executor output.

/// One case of a case file: its name and its lines, each split into tokens (comments dropped).
#[derive(Clone, Debug, PartialEq)]
pub struct Case {
    pub name: String,
    pub ops: Vec<Op>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Op {
    pub line: usize,
    pub tok: Vec<String>,
}

/// Tokens of a line without its comment ("# " to the end, or a line starting with '#'; "#RRGGBB"
/// is a color), as the executor splits it.
pub fn tokens(line: &str) -> Vec<String> {
    let mut l = line;
    if let Some(i) = l.find('#') {
        if i == 0 || l[i + 1..].starts_with(' ') {
            l = &l[..i];
        }
    }
    l.split([' ', '\t', '\r']).filter(|t| !t.is_empty()).map(str::to_string).collect()
}

/// The cases of a case file. Lines outside a case are ignored, as the executor ignores them.
pub fn parse_cases(text: &str) -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    let mut open = false;
    for (i, l) in text.lines().enumerate() {
        let t = tokens(l);
        if t.is_empty() {
            continue;
        }
        match t[0].as_str() {
            "case" => {
                cases.push(Case { name: t.get(1).cloned().unwrap_or_else(|| "?".into()), ops: Vec::new() });
                open = true;
            }
            "end" => open = false,
            _ if open => cases.last_mut().unwrap().ops.push(Op { line: i + 1, tok: t }),
            _ => {}
        }
    }
    cases
}

/// Executor output split by case: header lines (before the first case), then each case's name
/// and lines, the final "done" (absent when the program died) as `done`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Output {
    pub head: Vec<String>,
    pub cases: Vec<(String, Vec<String>)>,
    pub done: bool,
}

pub fn parse_output(text: &str) -> Output {
    let mut o = Output::default();
    for l in text.lines() {
        let l = l.trim_end_matches('\r');
        if let Some(n) = l.strip_prefix("case ") {
            o.cases.push((n.to_string(), Vec::new()));
        } else if l == "done" {
            o.done = true;
        } else if let Some(c) = o.cases.last_mut() {
            c.1.push(l.to_string());
        } else {
            o.head.push(l.to_string());
        }
    }
    o
}

/// "#RRGGBB" -> 0x00RRGGBB
pub fn color(t: &str) -> Option<u32> {
    let h = t.strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    u32::from_str_radix(h, 16).ok()
}

/// a number as the executor reads it: decimal, 0x hex or -decimal
pub fn num(t: &str) -> Option<i64> {
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u32::from_str_radix(h, 16).ok().map(|v| v as i64);
    }
    t.parse::<i64>().ok()
}
